//! Internal HVF arm64 boot-session preparation.

use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_runtime::balloon::{
    BalloonConfig, BalloonHintingCommandError, BalloonHintingStartInput, BalloonHintingStatus,
    BalloonHintingStatusError, BalloonMmioLayout, BalloonStats, BalloonStatsError,
    BalloonStatsUpdateInput, BalloonUpdateError, VIRTIO_BALLOON_DEVICE_ID,
    VirtioBalloonConfigSpace, VirtioBalloonDevice, VirtioBalloonDeviceNotificationError,
    VirtioBalloonQueueLayout,
};
use bangbang_runtime::block::{
    BlockFileBacking, BlockMmioLayout, DriveConfig, DriveRateLimiterConfig,
    DriveRuntimeMutationError, DriveUpdateError, PreparedBlockDevice, RuntimeBlockDeviceResource,
    VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_QUEUE_SIZES, VhostUserBlockConfigSignalError,
    VirtioBlockConfigSpace, VirtioBlockDevice, VirtioBlockDeviceNotificationError,
};
use bangbang_runtime::boot::BootSourceFiles;
use bangbang_runtime::boot_timer::{
    BootTimerMmioLayout, BootTimerMmioRegistrationError, register_boot_timer_mmio,
};
use bangbang_runtime::entropy::{
    EntropyMmioLayout, VIRTIO_RNG_DEVICE_ID, VIRTIO_RNG_QUEUE_SIZES, VirtioRngDevice,
    VirtioRngDeviceNotificationError, VirtioRngEntropySource, VirtioRngEntropySourceError,
    VirtioRngOsEntropySource,
};
use bangbang_runtime::fdt::{Arm64FdtCacheHierarchy, Arm64FdtError};
use bangbang_runtime::interrupt::{
    DeviceInterruptKind, DeviceInterruptTriggerError, GuestInterruptLine, InterruptSink,
};
use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryBacking, GuestMemoryRange};
use bangbang_runtime::memory_hotplug::{
    MemoryHotplugConfig, MemoryHotplugSizeUpdate, MemoryHotplugStatus, MemoryHotplugStatusError,
    MemoryHotplugUpdateError, VIRTIO_MEM_DEVICE_ID, VIRTIO_MEM_QUEUE_SIZES, VirtioMemConfigSpace,
    VirtioMemDevice, VirtioMemMmioLayout, VirtioMemMutationExecutor,
};
use bangbang_runtime::message_interrupt::GuestMessageInterruptResources;
use bangbang_runtime::metrics::{
    BlockDeviceMetricsLease, NetworkInterfaceMetricsLease, PmemDeviceMetricsLease,
    SharedBalloonDeviceMetrics, SharedBlockDeviceMetricsRegistry, SharedEntropyDeviceMetrics,
    SharedNetworkInterfaceMetricsRegistry, SharedPmemDeviceMetricsRegistry, SharedRtcDeviceMetrics,
    SharedVsockDeviceMetrics,
};
use bangbang_runtime::mmio::{MmioDispatcher, MmioHandlerError, MmioRegionId};
use bangbang_runtime::network::{
    NetworkInterfaceUpdate, NetworkInterfaceUpdateError, NetworkMmioLayout,
    NetworkRuntimeMutationError, PreparedNetworkDevice, VIRTIO_NET_DEVICE_ID,
    VIRTIO_NET_QUEUE_SIZES, VirtioNetworkConfigSpace, VirtioNetworkDevice,
    VirtioNetworkDeviceNotificationError,
};
use bangbang_runtime::pci::{
    PCI_FIRST_ENDPOINT_DEVICE, PCI_LAST_ENDPOINT_DEVICE, PciBarAddressSpace, PciBarAllocator,
    PciClassCode, PciType0Configuration,
};
use bangbang_runtime::pmem::{
    PmemConfig, PmemFileBacking, PmemMmioLayout, PmemRuntimeMutationError, PmemUpdate,
    PmemUpdateError, PreparedPmemDevice, VIRTIO_PMEM_DEVICE_ID, VIRTIO_PMEM_QUEUE_SIZES,
    VirtioPmemConfigSpace, VirtioPmemDevice, VirtioPmemFlushStatus,
};
use bangbang_runtime::rtc::RtcMmioLayout;
use bangbang_runtime::serial::{SerialConfig, SharedSerialOutput, SharedSerialOutputBuffer};
use bangbang_runtime::snapshot_device::{SnapshotV1BlockRetryState, SnapshotV1DeviceState};
use bangbang_runtime::startup::{
    Arm64BootBalloonNotificationDispatch, Arm64BootBalloonNotificationDispatchError,
    Arm64BootBalloonNotificationDispatches, Arm64BootBlockNotificationDispatch,
    Arm64BootBlockNotificationDispatchError, Arm64BootBlockNotificationDispatches,
    Arm64BootBlockWakeupFdsError,
    Arm64BootEntropyDeviceConfig as RuntimeArm64BootEntropyDeviceConfig,
    Arm64BootEntropyNotificationDispatch, Arm64BootEntropyNotificationDispatchError,
    Arm64BootEntropyNotificationDispatches, Arm64BootEntropySourceProvider,
    Arm64BootMemoryHotplugDeviceConfig as RuntimeArm64BootMemoryHotplugDeviceConfig,
    Arm64BootMemoryHotplugNotificationDispatch, Arm64BootMemoryHotplugNotificationDispatchError,
    Arm64BootMemoryHotplugNotificationDispatches, Arm64BootNetworkInterface,
    Arm64BootNetworkNotificationDispatch, Arm64BootNetworkNotificationDispatchError,
    Arm64BootNetworkNotificationDispatches, Arm64BootNetworkPacketIoProvider,
    Arm64BootPciValidationConfig, Arm64BootPciValidationResources, Arm64BootPmemFlushProvider,
    Arm64BootPmemNotificationDispatch, Arm64BootPmemNotificationDispatchError,
    Arm64BootPmemNotificationDispatches, Arm64BootResourceConfig, Arm64BootResourceError,
    Arm64BootResourceParts, Arm64BootResources,
    Arm64BootRtcDeviceConfig as RuntimeArm64BootRtcDeviceConfig, Arm64BootRuntimeResources,
    Arm64BootSerialDeviceConfig as RuntimeArm64BootSerialDeviceConfig, Arm64BootVmGenIdDevice,
    Arm64BootVmGenIdReplacementError, Arm64BootVsockNotificationDispatch,
    Arm64BootVsockNotificationDispatchError, Arm64BootVsockNotificationDispatches,
    Arm64BootVsockWakeupFdsError, InstalledSnapshotV1Runtime, VmStartupResources,
    memory_hotplug_status_for_device, refresh_vhost_user_block_config_for_devices_with_signal,
    replace_arm64_boot_vmgenid, update_memory_hotplug_config_for_device,
};
use bangbang_runtime::virtio::{
    UnsupportedVirtioDeviceConfig, VIRTIO_MMIO_VERSION_1_FEATURE, VirtioDeviceActivation,
    VirtioDeviceActivationError, VirtioDeviceActivationHandler, VirtioDeviceResetError,
    VirtioDeviceResetOutcome, VirtioDeviceType, VirtioDeviceTypeError, VirtioInterruptIntent,
};
use bangbang_runtime::virtio_pci::{
    PublishedVirtioPciEndpoint, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciDeviceOperationError,
    VirtioPciDiagnostics, VirtioPciEndpoint, VirtioPciEndpointError, VirtioPciEndpointPhase,
    VirtioPciIdentity, VirtioPciPublicationError,
};
use bangbang_runtime::vsock::{
    VIRTIO_VSOCK_DEVICE_ID, VIRTIO_VSOCK_QUEUE_SIZES, VirtioVsockConfigSpace, VirtioVsockDevice,
    VsockHostWakeup, VsockMmioLayout,
};
use bangbang_runtime::{BackendError, VmBackend, VmmController};

use crate::backend::HvfBackend;
use crate::coordinator::{HvfVcpuRunControl, HvfVcpuRunCoordinatorError, HvfVcpuRunTerminalReport};
use crate::dirty::{HvfDirtyWriteEpochResetError, HvfDirtyWriteTrackerStartError};
use crate::gic::{
    HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicError, HvfGicInterruptLineAllocator,
    HvfGicMetadata, HvfGicMsiConfiguration, HvfGicMsiDeviceInterruptResourceError,
    HvfGicMsiDeviceInterruptResources, HvfGicMsiSignaler, HvfGicSpiSignalError, HvfGicSpiSignaler,
    HvfInterruptLineAllocationError,
};
use crate::memory::{HvfGuestMemoryMappingError, HvfMemoryPermissions, HvfPmemFlushExecutor};
use crate::psci::PsciCpuPowerCoordinator;
use crate::runner::{
    HvfArm64SnapshotV1Capture, HvfArm64SnapshotV1Restore, HvfVcpuRunCancelHandle,
    HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError,
};
use crate::session_vcpu::{HvfArm64BootVcpuError, HvfArm64BootVcpuSession};
use crate::snapshot::HvfArm64SnapshotTimerState;
use crate::snapshot_bundle::{
    HvfSnapshotV1CompatibilityState, HvfSnapshotV1EncodeError, HvfSnapshotV1State,
    encode_hvf_snapshot_v1_state,
};
use crate::snapshot_restore::{
    HvfSnapshotV1RestoreCleanup, HvfSnapshotV1RestoreError, HvfSnapshotV1RestoreFailure,
    HvfSnapshotV1RestoreStage, PreparedHvfSnapshotV1Load,
};
use crate::topology::{HvfVcpuTopologyError, prepare_ordered_mpidrs};
use crate::vcpu::{
    HvfArm64BootRegisters, HvfArm64VcpuBreakpointRegisterState,
    HvfArm64VcpuCacheSelectionRegisterState, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuDebugControlRegisterState, HvfArm64VcpuDebugTrapState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuIdentificationRegisterState,
    HvfArm64VcpuPendingInterruptState, HvfArm64VcpuPhysicalTimerState,
    HvfArm64VcpuPointerAuthenticationKeyState, HvfArm64VcpuSimdFpState,
    HvfArm64VcpuSmePRegisterState, HvfArm64VcpuSmePstate, HvfArm64VcpuSmeSystemRegisterState,
    HvfArm64VcpuSmeZRegisterState, HvfArm64VcpuSmeZaRegisterState, HvfArm64VcpuSmeZt0RegisterState,
    HvfArm64VcpuSveSmeIdentificationRegisterState, HvfArm64VcpuSystemContextRegisterState,
    HvfArm64VcpuThreadContextRegisterState, HvfArm64VcpuTranslationRegisterState,
    HvfArm64VcpuVirtualTimerState, HvfArm64VcpuWatchpointRegisterState,
};

const SINGLE_VCPU_COUNT: u8 = 1;
const BLOCK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME: &str = "bangbang-hvf-block-retry-wakeup";
const PMEM_RETRY_WAKEUP_SCHEDULER_THREAD_NAME: &str = "bangbang-hvf-pmem-retry-wakeup";
const ENTROPY_RETRY_WAKEUP_SCHEDULER_THREAD_NAME: &str = "bangbang-hvf-entropy-retry-wakeup";
const NETWORK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME: &str = "bangbang-hvf-network-retry-wakeup";
const VSOCK_WAKEUP_MONITOR_THREAD_NAME: &str = "bangbang-hvf-vsock-wakeup";
const VSOCK_WAKEUP_MONITOR_STOP_BYTE: [u8; 1] = [0];
const POLL_FOREVER: libc::c_int = -1;
const PCI_VALIDATION_VIRTIO_RNG_BAR_REGION_ID: MmioRegionId = MmioRegionId::new(4001);
const PCI_VALIDATION_VIRTIO_RNG_VECTOR_COUNT: usize = 2;
const PCI_VALIDATION_VIRTIO_RNG_ENTROPY_BYTE: u8 = 0xa5;
const PCI_DATA_DEVICE_BAR_REGION_ID_BASE: u64 = 4100;
const PCI_ENDPOINT_SLOT_COUNT: usize =
    (PCI_LAST_ENDPOINT_DEVICE - PCI_FIRST_ENDPOINT_DEVICE + 1) as usize;

fn pci_data_endpoint_count(
    block_count: usize,
    network_count: usize,
    pmem_count: usize,
    balloon_configured: bool,
    vsock_configured: bool,
    entropy_configured: bool,
    memory_hotplug_configured: bool,
) -> usize {
    block_count
        .saturating_add(network_count)
        .saturating_add(pmem_count)
        .saturating_add(usize::from(balloon_configured))
        .saturating_add(usize::from(vsock_configured))
        .saturating_add(usize::from(entropy_configured))
        .saturating_add(usize::from(memory_hotplug_configured))
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootSessionConfig {
    pub block_mmio_layout: BlockMmioLayout,
    pub pmem_mmio_layout: PmemMmioLayout,
    pub network_mmio_layout: NetworkMmioLayout,
    pub vsock_mmio_layout: VsockMmioLayout,
    pub rtc_mmio_layout: RtcMmioLayout,
    pub balloon_device: Option<HvfArm64BootBalloonDeviceConfig>,
    pub boot_timer_device: Option<HvfArm64BootTimerDeviceConfig>,
    pub entropy_device: Option<HvfArm64BootEntropyDeviceConfig>,
    pub memory_hotplug_device: Option<HvfArm64BootMemoryHotplugDeviceConfig>,
    pub serial_device: Option<HvfArm64BootSerialDeviceConfig>,
    pub gic_msi: Option<HvfGicMsiConfiguration>,
    pub pci_validation: Option<Arm64BootPciValidationConfig>,
    pub pci_enabled: bool,
}

impl HvfArm64BootSessionConfig {
    pub const fn new(
        block_mmio_layout: BlockMmioLayout,
        pmem_mmio_layout: PmemMmioLayout,
        network_mmio_layout: NetworkMmioLayout,
        vsock_mmio_layout: VsockMmioLayout,
        rtc_mmio_layout: RtcMmioLayout,
    ) -> Self {
        Self {
            block_mmio_layout,
            pmem_mmio_layout,
            network_mmio_layout,
            vsock_mmio_layout,
            rtc_mmio_layout,
            balloon_device: None,
            boot_timer_device: None,
            entropy_device: None,
            memory_hotplug_device: None,
            serial_device: None,
            gic_msi: None,
            pci_validation: None,
            pci_enabled: false,
        }
    }

    pub const fn with_balloon_device(
        mut self,
        balloon_device: HvfArm64BootBalloonDeviceConfig,
    ) -> Self {
        self.balloon_device = Some(balloon_device);
        self
    }

    pub const fn with_entropy_device(
        mut self,
        entropy_device: HvfArm64BootEntropyDeviceConfig,
    ) -> Self {
        self.entropy_device = Some(entropy_device);
        self
    }

    pub const fn with_memory_hotplug_device(
        mut self,
        memory_hotplug_device: HvfArm64BootMemoryHotplugDeviceConfig,
    ) -> Self {
        self.memory_hotplug_device = Some(memory_hotplug_device);
        self
    }

    pub const fn with_boot_timer_device(
        mut self,
        boot_timer_device: HvfArm64BootTimerDeviceConfig,
    ) -> Self {
        self.boot_timer_device = Some(boot_timer_device);
        self
    }

    pub fn with_serial_device(mut self, serial_device: HvfArm64BootSerialDeviceConfig) -> Self {
        self.serial_device = Some(serial_device);
        self
    }

    /// Opt into a demand-sized public-HVF GICv2m range for platform validation.
    ///
    /// Ordinary process construction leaves this unset. PCI admission and
    /// guest-programmed MSI-X remain outside this startup profile.
    pub const fn with_gic_msi(mut self, configuration: HvfGicMsiConfiguration) -> Self {
        self.gic_msi = Some(configuration);
        self
    }

    /// Adds the internal endpoint used by the signed Linux PCI enumeration test.
    ///
    /// This requires the same configuration to opt into GICv2m with
    /// [`Self::with_gic_msi`]. It does not expose PCI through the production
    /// process configuration.
    pub const fn with_pci_validation(mut self, validation: Arm64BootPciValidationConfig) -> Self {
        self.pci_validation = Some(validation);
        self
    }

    /// Selects production PCI transport for every configured virtio device.
    #[must_use]
    pub const fn with_pci_enabled(mut self) -> Self {
        self.pci_enabled = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootBalloonDeviceConfig {
    pub mmio_layout: BalloonMmioLayout,
}

impl HvfArm64BootBalloonDeviceConfig {
    pub const fn new(mmio_layout: BalloonMmioLayout) -> Self {
        Self { mmio_layout }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootEntropyDeviceConfig {
    pub mmio_layout: EntropyMmioLayout,
}

impl HvfArm64BootEntropyDeviceConfig {
    pub const fn new(mmio_layout: EntropyMmioLayout) -> Self {
        Self { mmio_layout }
    }

    const fn into_runtime(
        self,
        interrupt_line: GuestInterruptLine,
    ) -> RuntimeArm64BootEntropyDeviceConfig {
        RuntimeArm64BootEntropyDeviceConfig::new(self.mmio_layout, interrupt_line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootMemoryHotplugDeviceConfig {
    pub mmio_layout: VirtioMemMmioLayout,
}

impl HvfArm64BootMemoryHotplugDeviceConfig {
    pub const fn new(mmio_layout: VirtioMemMmioLayout) -> Self {
        Self { mmio_layout }
    }

    const fn into_runtime(
        self,
        interrupt_line: GuestInterruptLine,
    ) -> RuntimeArm64BootMemoryHotplugDeviceConfig {
        RuntimeArm64BootMemoryHotplugDeviceConfig::new(self.mmio_layout, interrupt_line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootTimerDeviceConfig {
    pub mmio_layout: BootTimerMmioLayout,
}

impl HvfArm64BootTimerDeviceConfig {
    pub const fn new(mmio_layout: BootTimerMmioLayout) -> Self {
        Self { mmio_layout }
    }
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootSerialDeviceConfig {
    pub region_id: MmioRegionId,
    pub address: GuestAddress,
    pub output: SharedSerialOutput,
}

impl HvfArm64BootSerialDeviceConfig {
    pub fn new(region_id: MmioRegionId, address: GuestAddress, output: SharedSerialOutput) -> Self {
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

#[derive(Debug, Clone)]
struct SharedPciValidationVirtioRngDevice {
    inner: Arc<Mutex<VirtioRngDevice>>,
}

impl SharedPciValidationVirtioRngDevice {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VirtioRngDevice::new())),
        }
    }

    fn dispatch(
        &self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<
        bangbang_runtime::entropy::VirtioRngDeviceNotificationDispatch,
        HvfArm64BootPciValidationError,
    > {
        let mut device = self
            .inner
            .lock()
            .map_err(|_| HvfArm64BootPciValidationError::DeviceStatePoisoned)?;
        let mut source = PciValidationVirtioRngEntropySource;
        device
            .dispatch_drained_queue_notifications(memory, drained_notifications, &mut source)
            .map_err(|source| HvfArm64BootPciValidationError::Dispatch { source })
    }
}

impl VirtioDeviceActivationHandler for SharedPciValidationVirtioRngDevice {
    fn activate(
        &mut self,
        activation: VirtioDeviceActivation<'_>,
    ) -> Result<(), VirtioDeviceActivationError> {
        self.inner
            .lock()
            .map_err(|_| MmioHandlerError::new("PCI validation virtio-rng state is unavailable"))?
            .activate_rng(activation)
            .map_err(|source| MmioHandlerError::new(source.to_string()).into())
    }

    fn reset(&mut self) {
        if let Ok(mut device) = self.inner.lock() {
            device.reset();
        }
    }

    fn reset_outcome(&mut self) -> Result<VirtioDeviceResetOutcome, VirtioDeviceResetError> {
        self.inner
            .lock()
            .map_err(|_| MmioHandlerError::new("PCI validation virtio-rng state is unavailable"))?
            .reset();
        Ok(VirtioDeviceResetOutcome::Reset)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PciValidationVirtioRngEntropySource;

impl VirtioRngEntropySource for PciValidationVirtioRngEntropySource {
    fn fill_entropy(&mut self, destination: &mut [u8]) -> Result<(), VirtioRngEntropySourceError> {
        destination.fill(PCI_VALIDATION_VIRTIO_RNG_ENTROPY_BYTE);
        Ok(())
    }
}

type PublishedPciValidationVirtioRng = PublishedVirtioPciEndpoint<
    UnsupportedVirtioDeviceConfig,
    SharedPciValidationVirtioRngDevice,
    HvfGicMsiDeviceInterruptResources,
>;

#[derive(Debug)]
struct HvfArm64BootPciValidationEndpoint {
    published: PublishedPciValidationVirtioRng,
    device: SharedPciValidationVirtioRngDevice,
    queue_deliveries: usize,
    config_deliveries: usize,
    config_interrupt_triggered: bool,
}

impl HvfArm64BootPciValidationEndpoint {
    fn dispatch(&mut self, memory: &mut GuestMemory) -> Result<(), HvfArm64BootPciValidationError> {
        let work = self
            .published
            .endpoint()
            .admit_device_work()
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        let drained = work
            .take_pending_queue_notifications()
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        if drained.is_empty() {
            return Ok(());
        }
        let dispatch = self.device.dispatch(memory, drained)?;
        if !dispatch.needs_queue_interrupt() {
            return Ok(());
        }

        let diagnostics = self
            .published
            .endpoint()
            .diagnostics()
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        let queue_vector = diagnostics
            .queue_vectors
            .first()
            .copied()
            .flatten()
            .ok_or(HvfArm64BootPciValidationError::MissingQueueVector)?;
        let config_vector = diagnostics
            .config_vector
            .ok_or(HvfArm64BootPciValidationError::MissingConfigVector)?;
        if queue_vector == config_vector {
            return Err(HvfArm64BootPciValidationError::SharedQueueAndConfigVector);
        }

        work.trigger(VirtioInterruptIntent::Queue { queue_index: 0 })
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        self.queue_deliveries = self
            .queue_deliveries
            .checked_add(1)
            .ok_or(HvfArm64BootPciValidationError::DeliveryCountOverflow)?;
        Ok(())
    }

    fn trigger_config_once(&mut self) -> Result<(), HvfArm64BootPciValidationError> {
        if self.config_interrupt_triggered {
            return Ok(());
        }
        let work = self
            .published
            .endpoint()
            .admit_device_work()
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        let diagnostics = self
            .published
            .endpoint()
            .diagnostics()
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        let queue_vector = diagnostics
            .queue_vectors
            .first()
            .copied()
            .flatten()
            .ok_or(HvfArm64BootPciValidationError::MissingQueueVector)?;
        let config_vector = diagnostics
            .config_vector
            .ok_or(HvfArm64BootPciValidationError::MissingConfigVector)?;
        if queue_vector == config_vector {
            return Err(HvfArm64BootPciValidationError::SharedQueueAndConfigVector);
        }
        work.increment_config_generation_and_trigger()
            .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?;
        self.config_interrupt_triggered = true;
        self.config_deliveries = self
            .config_deliveries
            .checked_add(1)
            .ok_or(HvfArm64BootPciValidationError::DeliveryCountOverflow)?;
        Ok(())
    }

    fn diagnostics(
        &self,
    ) -> Result<HvfArm64BootPciValidationDiagnostics, HvfArm64BootPciValidationError> {
        Ok(HvfArm64BootPciValidationDiagnostics {
            transport: self
                .published
                .endpoint()
                .diagnostics()
                .map_err(|source| HvfArm64BootPciValidationError::Endpoint { source })?,
            queue_deliveries: self.queue_deliveries,
            config_deliveries: self.config_deliveries,
        })
    }
}

type PublishedPciBlock = PublishedVirtioPciEndpoint<
    VirtioBlockConfigSpace,
    VirtioBlockDevice,
    HvfGicMsiDeviceInterruptResources,
>;
type PublishedPciNetwork = PublishedVirtioPciEndpoint<
    VirtioNetworkConfigSpace,
    VirtioNetworkDevice,
    HvfGicMsiDeviceInterruptResources,
>;
type PublishedPciPmem = PublishedVirtioPciEndpoint<
    VirtioPmemConfigSpace,
    VirtioPmemDevice,
    HvfGicMsiDeviceInterruptResources,
>;
type PublishedPciBalloon = PublishedVirtioPciEndpoint<
    VirtioBalloonConfigSpace,
    VirtioBalloonDevice,
    HvfGicMsiDeviceInterruptResources,
>;
type PublishedPciVsock = PublishedVirtioPciEndpoint<
    VirtioVsockConfigSpace,
    VirtioVsockDevice,
    HvfGicMsiDeviceInterruptResources,
>;
type PublishedPciEntropy = PublishedVirtioPciEndpoint<
    UnsupportedVirtioDeviceConfig,
    VirtioRngDevice,
    HvfGicMsiDeviceInterruptResources,
>;
type PublishedPciMemoryHotplug = PublishedVirtioPciEndpoint<
    VirtioMemConfigSpace,
    VirtioMemDevice,
    HvfGicMsiDeviceInterruptResources,
>;

#[derive(Debug)]
struct HvfArm64BootPciBlockDevice {
    drive_id: String,
    is_root_device: bool,
    published: PublishedPciBlock,
    queue_deliveries: usize,
    retry_deadline: Option<Instant>,
    _metrics_lease: Option<BlockDeviceMetricsLease>,
}

#[derive(Debug)]
struct HvfArm64BootPciNetworkDevice {
    iface_id: String,
    host_dev_name: String,
    published: PublishedPciNetwork,
    queue_deliveries: usize,
    retry_deadline: Option<Instant>,
    _metrics_lease: Option<NetworkInterfaceMetricsLease>,
}

#[derive(Debug)]
struct HvfArm64BootPciPmemDevice {
    pmem_id: String,
    guest_range: GuestMemoryRange,
    published: PublishedPciPmem,
    queue_deliveries: usize,
    retry_deadline: Option<Instant>,
    _metrics_lease: Option<PmemDeviceMetricsLease>,
}

#[derive(Debug)]
struct HvfArm64BootPciBalloonDevice {
    published: PublishedPciBalloon,
    queue_deliveries: usize,
}

#[derive(Debug)]
struct HvfArm64BootPciVsockDevice {
    published: PublishedPciVsock,
    queue_deliveries: usize,
}

#[derive(Debug)]
struct HvfArm64BootPciEntropyDevice {
    published: PublishedPciEntropy,
    queue_deliveries: usize,
}

#[derive(Debug)]
struct HvfArm64BootPciMemoryHotplugDevice {
    published: PublishedPciMemoryHotplug,
    queue_deliveries: usize,
}

/// Stable class label in hidden PCI data-device diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootPciDataDeviceKind {
    Balloon,
    Block,
    Network,
    Pmem,
    Vsock,
    Entropy,
    MemoryHotplug,
}

/// Redacted diagnostics for one hidden PCI data endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64BootPciDataDeviceDiagnostics {
    pub kind: HvfArm64BootPciDataDeviceKind,
    pub id: String,
    pub transport: VirtioPciDiagnostics,
    pub queue_deliveries: usize,
}

/// Value-redacted failure in hidden PCI data-device preparation or teardown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64BootPciDataError {
    message: String,
}

impl HvfArm64BootPciDataError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for HvfArm64BootPciDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HvfArm64BootPciDataError {}

/// Exact reversible teardown state for one runtime PCI network endpoint.
pub struct PreparedHvfArm64BootPciNetworkRemoval {
    iface_id: String,
    index: usize,
}

impl PreparedHvfArm64BootPciNetworkRemoval {
    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }
}

impl fmt::Debug for PreparedHvfArm64BootPciNetworkRemoval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedHvfArm64BootPciNetworkRemoval")
            .field("ownership", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
struct HvfArm64BootPciDataDevices {
    validation: Arm64BootPciValidationResources,
    dispatcher: Arc<Mutex<MmioDispatcher>>,
    msi_interrupts: Option<HvfGicMsiDeviceInterruptResources>,
    balloon: Option<HvfArm64BootPciBalloonDevice>,
    block: Vec<HvfArm64BootPciBlockDevice>,
    network: Vec<HvfArm64BootPciNetworkDevice>,
    pmem: Vec<HvfArm64BootPciPmemDevice>,
    vsock: Option<HvfArm64BootPciVsockDevice>,
    entropy: Option<HvfArm64BootPciEntropyDevice>,
    memory_hotplug: Option<HvfArm64BootPciMemoryHotplugDevice>,
    pmem_static_reserved_ranges: Vec<GuestMemoryRange>,
    runtime_hotplug: bool,
}

impl HvfArm64BootPciDataDevices {
    fn shared_msi_registry(
        &self,
    ) -> Result<HvfGicMsiDeviceInterruptResources, HvfArm64BootPciDataError> {
        self.msi_interrupts
            .as_ref()
            .ok_or_else(|| {
                HvfArm64BootPciDataError::new(
                    "PCI data endpoint requires shared GICv2m interrupt resources",
                )
            })?
            .shared_registry()
            .map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to share PCI data GICv2m routes: {source}"
                ))
            })
    }

    fn endpoint_count(&self) -> usize {
        pci_data_endpoint_count(
            self.block.len(),
            self.network.len(),
            self.pmem.len(),
            self.balloon.is_some(),
            self.vsock.is_some(),
            self.entropy.is_some(),
            self.memory_hotplug.is_some(),
        )
    }

    fn available_region_id(
        dispatcher: &MmioDispatcher,
    ) -> Result<MmioRegionId, HvfArm64BootPciDataError> {
        (0..PCI_ENDPOINT_SLOT_COUNT)
            .map(pci_data_region_id)
            .find_map(|region_id| match region_id {
                Ok(region_id) if !dispatcher.contains_region_or_handler(region_id) => {
                    Some(Ok(region_id))
                }
                Ok(_) => None,
                Err(source) => Some(Err(source)),
            })
            .unwrap_or_else(|| {
                Err(HvfArm64BootPciDataError::new(
                    "PCI data MMIO region capacity is exhausted",
                ))
            })
    }

    fn insert_runtime_block(
        &mut self,
        prepared: PreparedBlockDevice,
        metrics: &SharedBlockDeviceMetricsRegistry,
    ) -> Result<(), DriveRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(DriveRuntimeMutationError::PciNotEnabled);
        }
        if prepared.is_root_device() {
            return Err(DriveRuntimeMutationError::RootInsertUnsupported);
        }
        if self
            .block
            .iter()
            .any(|device| device.drive_id == prepared.drive_id())
        {
            return Err(DriveRuntimeMutationError::DuplicateDrive {
                drive_id: prepared.drive_id().to_string(),
            });
        }
        if self.endpoint_count() >= PCI_ENDPOINT_SLOT_COUNT {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI endpoint capacity is exhausted".to_string(),
            });
        }
        if self.block.len() == self.block.capacity() {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime block inventory capacity is exhausted".to_string(),
            });
        }

        let drive_id = prepared.drive_id().to_string();
        let prepared_metrics = metrics.prepare_drive(drive_id.clone()).map_err(|source| {
            DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        let interrupts = self.shared_msi_registry().map_err(|source| {
            DriveRuntimeMutationError::TerminalInsertion {
                message: source.to_string(),
            }
        })?;
        let segment = self.validation.segment().clone();
        let block_type = VirtioDeviceType::new(VIRTIO_BLOCK_DEVICE_ID).map_err(|source| {
            DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        let (published_drive_id, is_root_device, config_space, device) = prepared.into_parts();
        debug_assert_eq!(published_drive_id, drive_id);
        debug_assert!(!is_root_device);
        let published = {
            let mut dispatcher = self.dispatcher.lock().map_err(|_| {
                DriveRuntimeMutationError::TerminalInsertion {
                    message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                }
            })?;
            let region_id = Self::available_region_id(&dispatcher).map_err(|source| {
                DriveRuntimeMutationError::PrepareDevice {
                    message: source.to_string(),
                }
            })?;
            PublishedVirtioPciEndpoint::publish(
                VirtioPciIdentity::new(block_type, config_space.available_features()),
                &VIRTIO_BLOCK_QUEUE_SIZES,
                config_space,
                device,
                false,
                self.validation.bar_allocator_mut(),
                segment,
                &mut dispatcher,
                region_id,
                interrupts,
            )
            .map_err(runtime_block_publication_error)?
        };
        let metrics_lease = prepared_metrics.publish();
        debug_assert!(self.block.len() < self.block.capacity());
        self.block.push(HvfArm64BootPciBlockDevice {
            drive_id,
            is_root_device,
            published,
            queue_deliveries: 0,
            retry_deadline: None,
            _metrics_lease: Some(metrics_lease),
        });
        Ok(())
    }

    fn preflight_runtime_block(
        &self,
        drive_id: &str,
        metrics: &SharedBlockDeviceMetricsRegistry,
    ) -> Result<(), DriveRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(DriveRuntimeMutationError::PciNotEnabled);
        }
        if self.block.iter().any(|device| device.drive_id == drive_id) {
            return Err(DriveRuntimeMutationError::DuplicateDrive {
                drive_id: drive_id.to_string(),
            });
        }
        if self.endpoint_count() >= PCI_ENDPOINT_SLOT_COUNT {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI endpoint capacity is exhausted".to_string(),
            });
        }
        if self.block.len() == self.block.capacity() {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime block inventory capacity is exhausted".to_string(),
            });
        }
        let available_slots = self
            .validation
            .segment()
            .with_segment(|segment| segment.available_endpoint_slots())
            .map_err(|source| DriveRuntimeMutationError::ActiveSessionCommand {
                message: format!("PCI segment is unavailable: {source}"),
            })?;
        if available_slots == 0 {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI function capacity is exhausted".to_string(),
            });
        }
        let available_bars = pci_data_available_bar_count(self.validation.bar_allocator())
            .map_err(|source| DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            })?;
        if available_bars == 0 {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI capability BAR capacity is exhausted".to_string(),
            });
        }
        let planned_bar = pci_data_bar_plan(self.validation.bar_allocator(), 1)
            .map_err(|source| DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            })?
            .into_iter()
            .next()
            .ok_or_else(|| DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI capability BAR capacity is exhausted".to_string(),
            })?;
        if self
            .msi_interrupts
            .as_ref()
            .is_none_or(|interrupts| interrupts.lease_count() == 0)
        {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI interrupt resources are unavailable".to_string(),
            });
        }
        let dispatcher = self.dispatcher.lock().map_err(|_| {
            DriveRuntimeMutationError::ActiveSessionCommand {
                message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
            }
        })?;
        Self::available_region_id(&dispatcher).map_err(|source| {
            DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        if dispatcher
            .regions()
            .iter()
            .any(|region| region.range().overlaps(planned_bar))
        {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "runtime PCI capability BAR overlaps live MMIO state".to_string(),
            });
        }
        metrics.preflight_drive(drive_id).map_err(|source| {
            DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        Ok(())
    }

    fn update_runtime_block(
        &self,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        let device = self
            .block
            .iter()
            .find(|device| device.drive_id == config.drive_id())
            .ok_or_else(|| DriveUpdateError::UnknownDrive {
                drive_id: config.drive_id().to_string(),
            })?;
        if config.is_vhost_user() {
            if backing.is_some() || rate_limiter_update.is_some() {
                return Err(DriveUpdateError::UnsupportedBackend);
            }
            return device
                .published
                .endpoint()
                .refresh_vhost_user_block_config(config.cache_type());
        }
        device
            .published
            .endpoint()
            .update_block_device_with_opened(config, backing, rate_limiter_update)
            .map_err(|source| DriveUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })
    }

    fn remove_runtime_block(&mut self, drive_id: &str) -> Result<(), DriveRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(DriveRuntimeMutationError::PciNotEnabled);
        }
        let index = self
            .block
            .iter()
            .position(|device| device.drive_id == drive_id)
            .ok_or_else(|| DriveRuntimeMutationError::UnknownDrive {
                drive_id: drive_id.to_string(),
            })?;
        if self
            .block
            .get(index)
            .is_some_and(|device| device.is_root_device)
        {
            return Err(DriveRuntimeMutationError::RootRemovalUnsupported {
                drive_id: drive_id.to_string(),
            });
        }
        {
            let mut dispatcher =
                self.dispatcher
                    .lock()
                    .map_err(|_| DriveRuntimeMutationError::TerminalRemoval {
                        message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                    })?;
            let device = self.block.get_mut(index).ok_or_else(|| {
                DriveRuntimeMutationError::UnknownDrive {
                    drive_id: drive_id.to_string(),
                }
            })?;
            if let Err(source) = device.published.prepare_teardown(&mut dispatcher) {
                let message = source.to_string();
                return Err(
                    if matches!(
                        source,
                        VirtioPciPublicationError::Rollback { .. }
                            | VirtioPciPublicationError::SegmentLock { .. }
                            | VirtioPciPublicationError::EndpointRelease { .. }
                    ) {
                        DriveRuntimeMutationError::TerminalRemoval { message }
                    } else {
                        DriveRuntimeMutationError::RemoveDevice { message }
                    },
                );
            }
            device
                .published
                .commit_prepared_teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| DriveRuntimeMutationError::TerminalRemoval {
                    message: source.to_string(),
                })?;
        }
        self.block.remove(index);
        Ok(())
    }

    fn insert_runtime_network(
        &mut self,
        prepared: PreparedNetworkDevice,
        metrics: &SharedNetworkInterfaceMetricsRegistry,
    ) -> Result<(), NetworkRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(NetworkRuntimeMutationError::PciNotEnabled);
        }
        if self
            .network
            .iter()
            .any(|device| device.iface_id == prepared.iface_id())
        {
            return Err(NetworkRuntimeMutationError::DuplicateInterface {
                iface_id: prepared.iface_id().to_string(),
            });
        }
        if self.endpoint_count() >= PCI_ENDPOINT_SLOT_COUNT {
            return Err(NetworkRuntimeMutationError::PrepareDevice {
                message: "runtime PCI endpoint capacity is exhausted".to_string(),
            });
        }
        if self.network.len() == self.network.capacity() {
            return Err(NetworkRuntimeMutationError::PrepareDevice {
                message: "runtime network inventory capacity is exhausted".to_string(),
            });
        }

        let iface_id = prepared.iface_id().to_string();
        let prepared_metrics = metrics
            .prepare_interface(iface_id.clone())
            .map_err(|source| NetworkRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            })?;
        let interrupts = self.shared_msi_registry().map_err(|source| {
            NetworkRuntimeMutationError::TerminalInsertion {
                message: source.to_string(),
            }
        })?;
        let segment = self.validation.segment().clone();
        let network_type = VirtioDeviceType::new(VIRTIO_NET_DEVICE_ID).map_err(|source| {
            NetworkRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        let (published_iface_id, host_dev_name, config_space, device) = prepared.into_parts();
        debug_assert_eq!(published_iface_id, iface_id);
        let published = {
            let mut dispatcher = self.dispatcher.lock().map_err(|_| {
                NetworkRuntimeMutationError::TerminalInsertion {
                    message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                }
            })?;
            let region_id = Self::available_region_id(&dispatcher).map_err(|source| {
                NetworkRuntimeMutationError::PrepareDevice {
                    message: source.to_string(),
                }
            })?;
            PublishedVirtioPciEndpoint::publish(
                VirtioPciIdentity::new(network_type, config_space.available_features()),
                &VIRTIO_NET_QUEUE_SIZES,
                config_space,
                device,
                false,
                self.validation.bar_allocator_mut(),
                segment,
                &mut dispatcher,
                region_id,
                interrupts,
            )
            .map_err(runtime_network_publication_error)?
        };
        let metrics_lease = prepared_metrics.publish();
        debug_assert!(self.network.len() < self.network.capacity());
        self.network.push(HvfArm64BootPciNetworkDevice {
            iface_id,
            host_dev_name,
            published,
            queue_deliveries: 0,
            retry_deadline: None,
            _metrics_lease: Some(metrics_lease),
        });
        Ok(())
    }

    fn update_runtime_network(
        &self,
        update: &NetworkInterfaceUpdate,
    ) -> Result<(), NetworkInterfaceUpdateError> {
        let device = self
            .network
            .iter()
            .find(|device| device.iface_id == update.iface_id())
            .ok_or_else(|| NetworkInterfaceUpdateError::UnknownInterface {
                iface_id: update.iface_id().to_string(),
            })?;
        device
            .published
            .endpoint()
            .update_network_rate_limiters(update)
            .map_err(|source| NetworkInterfaceUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })
    }

    fn prepare_runtime_network_removal(
        &mut self,
        iface_id: &str,
    ) -> Result<PreparedHvfArm64BootPciNetworkRemoval, NetworkRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(NetworkRuntimeMutationError::PciNotEnabled);
        }
        let index = self
            .network
            .iter()
            .position(|device| device.iface_id == iface_id)
            .ok_or_else(|| NetworkRuntimeMutationError::UnknownInterface {
                iface_id: iface_id.to_string(),
            })?;
        let mut dispatcher =
            self.dispatcher
                .lock()
                .map_err(|_| NetworkRuntimeMutationError::TerminalRemoval {
                    message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                })?;
        let device = self.network.get_mut(index).ok_or_else(|| {
            NetworkRuntimeMutationError::UnknownInterface {
                iface_id: iface_id.to_string(),
            }
        })?;
        if let Err(source) = device.published.prepare_teardown(&mut dispatcher) {
            let message = source.to_string();
            return Err(
                if matches!(
                    source,
                    VirtioPciPublicationError::Rollback { .. }
                        | VirtioPciPublicationError::SegmentLock { .. }
                        | VirtioPciPublicationError::EndpointRelease { .. }
                ) {
                    NetworkRuntimeMutationError::TerminalRemoval { message }
                } else {
                    NetworkRuntimeMutationError::RemoveDevice { message }
                },
            );
        }
        Ok(PreparedHvfArm64BootPciNetworkRemoval {
            iface_id: iface_id.to_string(),
            index,
        })
    }

    fn rollback_runtime_network_removal(
        &mut self,
        prepared: PreparedHvfArm64BootPciNetworkRemoval,
    ) -> Result<(), NetworkRuntimeMutationError> {
        let mut dispatcher =
            self.dispatcher
                .lock()
                .map_err(|_| NetworkRuntimeMutationError::TerminalRemoval {
                    message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                })?;
        let device = self.network.get_mut(prepared.index).ok_or_else(|| {
            NetworkRuntimeMutationError::TerminalRemoval {
                message: "runtime network endpoint disappeared during rollback".to_string(),
            }
        })?;
        if device.iface_id != prepared.iface_id {
            return Err(NetworkRuntimeMutationError::TerminalRemoval {
                message: "runtime network endpoint identity changed during rollback".to_string(),
            });
        }
        device
            .published
            .rollback_prepared_teardown(&mut dispatcher)
            .map_err(|source| NetworkRuntimeMutationError::TerminalRemoval {
                message: source.to_string(),
            })
    }

    fn commit_runtime_network_removal(
        &mut self,
        prepared: PreparedHvfArm64BootPciNetworkRemoval,
    ) -> Result<(), NetworkRuntimeMutationError> {
        {
            let mut dispatcher = self.dispatcher.lock().map_err(|_| {
                NetworkRuntimeMutationError::TerminalRemoval {
                    message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                }
            })?;
            let device = self.network.get_mut(prepared.index).ok_or_else(|| {
                NetworkRuntimeMutationError::TerminalRemoval {
                    message: "runtime network endpoint disappeared during commit".to_string(),
                }
            })?;
            if device.iface_id != prepared.iface_id {
                return Err(NetworkRuntimeMutationError::TerminalRemoval {
                    message: "runtime network endpoint identity changed during commit".to_string(),
                });
            }
            device
                .published
                .commit_prepared_teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| NetworkRuntimeMutationError::TerminalRemoval {
                    message: source.to_string(),
                })?;
        }
        self.network.remove(prepared.index);
        Ok(())
    }

    fn insert_runtime_pmem(
        &mut self,
        backend: &mut HvfBackend,
        runtime_pmem_devices: &mut Vec<PreparedPmemDevice>,
        config: &PmemConfig,
        backing: PmemFileBacking,
        metrics: &SharedPmemDeviceMetricsRegistry,
    ) -> Result<(), PmemRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(PmemRuntimeMutationError::PciNotEnabled);
        }
        if config.root_device() {
            return Err(PmemRuntimeMutationError::RootInsertUnsupported);
        }
        if self.pmem.iter().any(|device| device.pmem_id == config.id()) {
            return Err(PmemRuntimeMutationError::DuplicatePmem);
        }
        if self.endpoint_count() >= PCI_ENDPOINT_SLOT_COUNT {
            return Err(PmemRuntimeMutationError::PrepareDevice {
                message: "runtime PCI endpoint capacity is exhausted".to_string(),
            });
        }
        if self.pmem.len() == self.pmem.capacity()
            || runtime_pmem_devices.len() == runtime_pmem_devices.capacity()
        {
            return Err(PmemRuntimeMutationError::PrepareDevice {
                message: "runtime pmem inventory capacity is exhausted".to_string(),
            });
        }

        let reserved_count = self
            .pmem_static_reserved_ranges
            .len()
            .checked_add(self.pmem.len())
            .ok_or_else(|| PmemRuntimeMutationError::PrepareDevice {
                message: "runtime pmem guest-range inventory is too large".to_string(),
            })?;
        let mut reserved_ranges = Vec::new();
        reserved_ranges
            .try_reserve_exact(reserved_count)
            .map_err(|_| PmemRuntimeMutationError::PrepareDevice {
                message: "failed to reserve runtime pmem guest-range storage".to_string(),
            })?;
        reserved_ranges.extend(self.pmem_static_reserved_ranges.iter().copied());
        reserved_ranges.extend(self.pmem.iter().map(|device| device.guest_range));
        let prepared = PreparedPmemDevice::from_config_with_backing_and_reserved_ranges(
            config,
            backing,
            &reserved_ranges,
        )
        .map_err(|source| PmemRuntimeMutationError::PrepareDevice {
            message: source.to_string(),
        })?;
        let pmem_id = prepared.id().to_string();
        let guest_range = prepared.guest_range();
        let config_space = prepared.config_space();
        let device = VirtioPmemDevice::with_rate_limiter(
            prepared.mapping().file_len(),
            prepared.rate_limiter(),
        );
        let prepared_metrics = metrics.prepare_device(pmem_id.clone()).map_err(|source| {
            PmemRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        let interrupts = self.shared_msi_registry().map_err(|source| {
            PmemRuntimeMutationError::TerminalInsertion {
                message: source.to_string(),
            }
        })?;
        let segment = self.validation.segment().clone();
        let pmem_type = VirtioDeviceType::new(VIRTIO_PMEM_DEVICE_ID).map_err(|source| {
            PmemRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;

        backend
            .map_runtime_pmem_device(&prepared)
            .map_err(|source| PmemRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            })?;
        let publication = (|| {
            let mut dispatcher = self.dispatcher.lock().map_err(|_| {
                PmemRuntimeMutationError::TerminalInsertion {
                    message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                }
            })?;
            let region_id = Self::available_region_id(&dispatcher).map_err(|source| {
                PmemRuntimeMutationError::PrepareDevice {
                    message: source.to_string(),
                }
            })?;
            PublishedVirtioPciEndpoint::publish(
                VirtioPciIdentity::new(pmem_type, config_space.available_features()),
                &VIRTIO_PMEM_QUEUE_SIZES,
                config_space,
                device,
                false,
                self.validation.bar_allocator_mut(),
                segment,
                &mut dispatcher,
                region_id,
                interrupts,
            )
            .map_err(runtime_pmem_publication_error)
        })();
        let published = match publication {
            Ok(published) => published,
            Err(primary) => {
                return match backend.take_runtime_pmem_mapping(guest_range, false) {
                    Ok(mapping) => {
                        drop(mapping);
                        Err(primary)
                    }
                    Err(cleanup) => Err(PmemRuntimeMutationError::TerminalInsertion {
                        message: format!(
                            "{primary}; also failed to discard unpublished pmem mapping: {cleanup}"
                        ),
                    }),
                };
            }
        };

        let metrics_lease = prepared_metrics.publish();
        debug_assert!(self.pmem.len() < self.pmem.capacity());
        debug_assert!(runtime_pmem_devices.len() < runtime_pmem_devices.capacity());
        self.pmem.push(HvfArm64BootPciPmemDevice {
            pmem_id,
            guest_range,
            published,
            queue_deliveries: 0,
            retry_deadline: None,
            _metrics_lease: Some(metrics_lease),
        });
        runtime_pmem_devices.push(prepared);
        Ok(())
    }

    fn update_runtime_pmem(&self, update: &PmemUpdate) -> Result<bool, PmemUpdateError> {
        let device = self
            .pmem
            .iter()
            .find(|device| device.pmem_id == update.id())
            .ok_or(PmemUpdateError::UnknownPmem)?;
        device
            .published
            .endpoint()
            .update_pmem_rate_limiter(update)
            .map_err(|source| PmemUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })
    }

    fn remove_runtime_pmem(
        &mut self,
        backend: &mut HvfBackend,
        runtime_pmem_devices: &mut Vec<PreparedPmemDevice>,
        pmem_id: &str,
    ) -> Result<(), PmemRuntimeMutationError> {
        if !self.runtime_hotplug {
            return Err(PmemRuntimeMutationError::PciNotEnabled);
        }
        let index = self
            .pmem
            .iter()
            .position(|device| device.pmem_id == pmem_id)
            .ok_or(PmemRuntimeMutationError::UnknownPmem)?;
        let resource_index = runtime_pmem_devices
            .iter()
            .position(|device| device.id() == pmem_id)
            .ok_or_else(|| PmemRuntimeMutationError::TerminalRemoval {
                message: "runtime pmem backing owner is missing".to_string(),
            })?;
        let guest_range = self
            .pmem
            .get(index)
            .map(|device| device.guest_range)
            .ok_or(PmemRuntimeMutationError::UnknownPmem)?;

        {
            let mut dispatcher =
                self.dispatcher
                    .lock()
                    .map_err(|_| PmemRuntimeMutationError::TerminalRemoval {
                        message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                    })?;
            let device = self
                .pmem
                .get_mut(index)
                .ok_or(PmemRuntimeMutationError::UnknownPmem)?;
            if let Err(source) = device.published.prepare_teardown(&mut dispatcher) {
                let message = source.to_string();
                return Err(
                    if matches!(
                        source,
                        VirtioPciPublicationError::Rollback { .. }
                            | VirtioPciPublicationError::SegmentLock { .. }
                            | VirtioPciPublicationError::EndpointRelease { .. }
                    ) {
                        PmemRuntimeMutationError::TerminalRemoval { message }
                    } else {
                        PmemRuntimeMutationError::RemoveDevice { message }
                    },
                );
            }
        }

        let removed_mapping = match backend.take_runtime_pmem_mapping(guest_range, true) {
            Ok(mapping) => mapping,
            Err(primary) => {
                let rollback = (|| {
                    let mut dispatcher = self.dispatcher.lock().map_err(|_| {
                        "PCI data-device MMIO dispatcher is unavailable".to_string()
                    })?;
                    self.pmem
                        .get_mut(index)
                        .ok_or_else(|| "runtime pmem endpoint disappeared".to_string())?
                        .published
                        .rollback_prepared_teardown(&mut dispatcher)
                        .map_err(|source| source.to_string())
                })();
                return match rollback {
                    Ok(()) => Err(PmemRuntimeMutationError::RemoveDevice {
                        message: primary.to_string(),
                    }),
                    Err(rollback) => Err(PmemRuntimeMutationError::TerminalRemoval {
                        message: format!(
                            "{primary}; also failed to restore runtime pmem endpoint: {rollback}"
                        ),
                    }),
                };
            }
        };

        let commit = (|| {
            let mut dispatcher =
                self.dispatcher
                    .lock()
                    .map_err(|_| PmemRuntimeMutationError::TerminalRemoval {
                        message: "PCI data-device MMIO dispatcher is unavailable".to_string(),
                    })?;
            self.pmem
                .get_mut(index)
                .ok_or(PmemRuntimeMutationError::UnknownPmem)?
                .published
                .commit_prepared_teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| PmemRuntimeMutationError::TerminalRemoval {
                    message: source.to_string(),
                })
        })();
        if let Err(primary) = commit {
            return match backend.restore_runtime_pmem_mapping(removed_mapping) {
                Ok(()) => Err(primary),
                Err(restore) => Err(PmemRuntimeMutationError::TerminalRemoval {
                    message: format!(
                        "{primary}; also failed to restore runtime pmem mapping: {restore}"
                    ),
                }),
            };
        }

        drop(removed_mapping);
        self.pmem.remove(index);
        runtime_pmem_devices.remove(resource_index);
        Ok(())
    }

    fn diagnostics(
        &self,
    ) -> Result<Vec<HvfArm64BootPciDataDeviceDiagnostics>, HvfArm64BootPciDataError> {
        let mut diagnostics = Vec::new();
        diagnostics
            .try_reserve_exact(
                self.block.len()
                    + self.network.len()
                    + self.pmem.len()
                    + usize::from(self.balloon.is_some())
                    + usize::from(self.vsock.is_some())
                    + usize::from(self.entropy.is_some())
                    + usize::from(self.memory_hotplug.is_some()),
            )
            .map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to allocate PCI data-device diagnostics: {source}"
                ))
            })?;
        if let Some(device) = &self.balloon {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::Balloon,
                id: "balloon0".to_string(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        for device in &self.block {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::Block,
                id: device.drive_id.clone(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        for device in &self.network {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::Network,
                id: device.iface_id.clone(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        for device in &self.pmem {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::Pmem,
                id: device.pmem_id.clone(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        if let Some(device) = &self.vsock {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::Vsock,
                id: "vsock0".to_string(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        if let Some(device) = &self.entropy {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::Entropy,
                id: "entropy0".to_string(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        if let Some(device) = &self.memory_hotplug {
            diagnostics.push(HvfArm64BootPciDataDeviceDiagnostics {
                kind: HvfArm64BootPciDataDeviceKind::MemoryHotplug,
                id: "mem0".to_string(),
                transport: device
                    .published
                    .endpoint()
                    .diagnostics()
                    .map_err(pci_data_endpoint_error)?,
                queue_deliveries: device.queue_deliveries,
            });
        }
        Ok(diagnostics)
    }

    fn dispatch_block(
        &mut self,
        memory: &mut GuestMemory,
        metrics: &SharedBlockDeviceMetricsRegistry,
    ) -> Option<Duration> {
        for device in &mut self.block {
            let mut retry_after = None;
            let result = device
                .published
                .endpoint()
                .dispatch_block_queue_notifications(memory);
            match &result {
                Ok(dispatch) => {
                    metrics.record_notification_dispatch_for_drive(&device.drive_id, dispatch);
                    retain_earliest_retry(
                        &mut retry_after,
                        dispatch
                            .queue_dispatch()
                            .and_then(|dispatch| dispatch.rate_limiter_retry_after()),
                    );
                    if dispatch.needs_queue_interrupt() {
                        device.queue_deliveries = device.queue_deliveries.saturating_add(1);
                    }
                }
                Err(error) => {
                    record_pci_block_operation_error(metrics, &device.drive_id, error);
                    if let Some(completed) = error.completed_device_operation() {
                        retain_earliest_retry(
                            &mut retry_after,
                            completed
                                .queue_dispatch()
                                .and_then(|dispatch| dispatch.rate_limiter_retry_after()),
                        );
                    }
                    if let Some(source) = error.device_error() {
                        retain_earliest_retry(
                            &mut retry_after,
                            source
                                .completed_dispatch()
                                .and_then(|dispatch| dispatch.rate_limiter_retry_after()),
                        );
                        if error.endpoint_error().is_none() && source.needs_queue_interrupt() {
                            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
                        }
                    }
                }
            }
            device.retry_deadline = retry_after.map(limiter_retry_deadline_after);
        }
        self.block_retry_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn block_retry_deadline(&self) -> Option<Instant> {
        self.block
            .iter()
            .filter_map(|device| device.retry_deadline)
            .min()
    }

    fn dispatch_pmem(
        &mut self,
        memory: &mut GuestMemory,
        flush_provider: &mut impl Arm64BootPmemFlushProvider,
        metrics: &SharedPmemDeviceMetricsRegistry,
    ) -> Option<Duration> {
        for device in &mut self.pmem {
            let mut retry_after = None;
            let mut flush = || flush_provider.flush(device.guest_range);
            let result = device
                .published
                .endpoint()
                .dispatch_pmem_queue_notifications(memory, &mut flush);
            match &result {
                Ok(dispatch) => {
                    metrics.record_notification_dispatch_for_device(&device.pmem_id, dispatch);
                    retain_earliest_retry(&mut retry_after, dispatch.rate_limiter_retry_after());
                    if dispatch.needs_queue_interrupt() {
                        device.queue_deliveries = device.queue_deliveries.saturating_add(1);
                    }
                }
                Err(error) => {
                    if let Some(completed) = error.completed_device_operation() {
                        metrics.record_notification_dispatch_for_device(&device.pmem_id, completed);
                        retain_earliest_retry(
                            &mut retry_after,
                            completed.rate_limiter_retry_after(),
                        );
                    }
                    if let Some(source) = error.device_error() {
                        metrics.record_notification_error_for_device(&device.pmem_id, source);
                        retain_earliest_retry(&mut retry_after, source.rate_limiter_retry_after());
                        if error.endpoint_error().is_none()
                            && source
                                .completed_dispatch()
                                .is_some_and(|dispatch| dispatch.needs_queue_interrupt())
                        {
                            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
                        }
                    }
                    if error.endpoint_error().is_some() {
                        metrics.record_event_failure_for_device(&device.pmem_id);
                    }
                }
            }
            device.retry_deadline = retry_after.map(limiter_retry_deadline_after);
        }
        self.pmem_retry_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn pmem_retry_deadline(&self) -> Option<Instant> {
        self.pmem
            .iter()
            .filter_map(|device| device.retry_deadline)
            .min()
    }

    fn dispatch_network(
        &mut self,
        memory: &mut GuestMemory,
        mut packet_io: Option<&mut dyn Arm64BootNetworkPacketIoProvider>,
        metrics: &SharedNetworkInterfaceMetricsRegistry,
    ) -> Option<Duration> {
        for device in &mut self.network {
            let mut retry_after = None;
            let endpoint = device.published.endpoint();
            let has_pending = match endpoint.has_pending_network_queue_work() {
                Ok(has_pending) => has_pending,
                Err(_) => {
                    metrics.record_event_failure_for_interface(&device.iface_id);
                    device.retry_deadline = None;
                    continue;
                }
            };
            let result = if has_pending {
                match packet_io.as_deref_mut() {
                    Some(provider) => match provider.packet_io(Arm64BootNetworkInterface::new(
                        &device.iface_id,
                        &device.host_dev_name,
                    )) {
                        Ok(packet_io) => {
                            let (tx_sink, rx_source) = packet_io.into_parts();
                            endpoint.dispatch_network_queue_notifications_with_packet_io(
                                memory, tx_sink, rx_source,
                            )
                        }
                        Err(_) => {
                            metrics.record_event_failure_for_interface(&device.iface_id);
                            device.retry_deadline = None;
                            continue;
                        }
                    },
                    None => endpoint.dispatch_network_queue_notifications(memory),
                }
            } else {
                endpoint.dispatch_network_queue_notifications(memory)
            };
            match &result {
                Ok(dispatch) => {
                    metrics.record_notification_dispatch_for_interface(&device.iface_id, dispatch);
                    retain_earliest_retry(&mut retry_after, dispatch.rate_limiter_retry_after());
                    if dispatch.needs_queue_interrupt() {
                        device.queue_deliveries = device.queue_deliveries.saturating_add(1);
                    }
                }
                Err(error) => {
                    if let Some(completed) = error.completed_device_operation() {
                        metrics.record_notification_dispatch_for_interface(
                            &device.iface_id,
                            completed,
                        );
                        retain_earliest_retry(
                            &mut retry_after,
                            completed.rate_limiter_retry_after(),
                        );
                    }
                    if let Some(source) = error.device_error() {
                        metrics.record_notification_error_for_interface(&device.iface_id, source);
                        retain_earliest_retry(&mut retry_after, source.rate_limiter_retry_after());
                        if error.endpoint_error().is_none()
                            && network_error_needs_queue_interrupt(source)
                        {
                            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
                        }
                    }
                    if error.endpoint_error().is_some() {
                        metrics.record_event_failure_for_interface(&device.iface_id);
                    }
                }
            }
            device.retry_deadline = retry_after.map(limiter_retry_deadline_after);
        }
        self.network_retry_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn network_retry_deadline(&self) -> Option<Instant> {
        self.network
            .iter()
            .filter_map(|device| device.retry_deadline)
            .min()
    }

    fn dispatch_vsock(&mut self, memory: &mut GuestMemory, metrics: &SharedVsockDeviceMetrics) {
        let Some(device) = self.vsock.as_mut() else {
            return;
        };
        let result = device
            .published
            .endpoint()
            .dispatch_vsock_queue_notifications(memory);
        match &result {
            Ok(dispatch) => metrics.record_notification_dispatch(dispatch),
            Err(error) => {
                if let Some(completed) = error.completed_device_operation() {
                    metrics.record_notification_dispatch(completed);
                }
                if let Some(source) = error.device_error() {
                    metrics.record_notification_error(source);
                }
                if error.endpoint_error().is_some() {
                    metrics.record_muxer_event_failure();
                }
            }
        }
        let delivered = match &result {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) if error.endpoint_error().is_none() => {
                error.device_error().is_some_and(|source| {
                    source
                        .completed_rx_dispatch()
                        .is_some_and(|dispatch| dispatch.needs_queue_interrupt())
                        || source
                            .completed_tx_dispatch()
                            .is_some_and(|dispatch| dispatch.needs_queue_interrupt())
                })
            }
            Err(_) => false,
        };
        if delivered {
            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
        }
    }

    fn dispatch_balloon(
        &mut self,
        memory: &mut GuestMemory,
        metrics: &SharedBalloonDeviceMetrics,
        statistics_trigger: bool,
    ) -> Result<(), BalloonUpdateError> {
        let Some(device) = self.balloon.as_mut() else {
            return Ok(());
        };
        let result = if statistics_trigger {
            device
                .published
                .endpoint()
                .trigger_balloon_statistics_update(memory)
        } else {
            device
                .published
                .endpoint()
                .dispatch_balloon_queue_notifications(memory)
        };
        match &result {
            Ok(dispatch) => metrics.record_notification_dispatch(dispatch),
            Err(error) => {
                if let Some(completed) = error.completed_device_operation() {
                    metrics.record_notification_dispatch(completed);
                }
                if let Some(source) = error.device_error() {
                    if statistics_trigger
                        || !matches!(
                            source,
                            VirtioBalloonDeviceNotificationError::Inactive { .. }
                        )
                    {
                        metrics.record_event_failure();
                    }
                    if let Some(completed) = source.completed_notification_dispatch() {
                        metrics.record_notification_dispatch(completed);
                    }
                }
                if error.endpoint_error().is_some() {
                    metrics.record_event_failure();
                }
            }
        }
        let delivered = match &result {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) if error.endpoint_error().is_none() => error
                .device_error()
                .and_then(|source| source.completed_notification_dispatch())
                .is_some_and(|dispatch| dispatch.needs_queue_interrupt()),
            Err(_) => false,
        };
        if delivered {
            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
        }
        if statistics_trigger && let Err(source) = result {
            if let Some(device_error) = source.device_error()
                && matches!(
                    device_error,
                    VirtioBalloonDeviceNotificationError::Inactive { .. }
                )
            {
                return Ok(());
            }
            return Err(BalloonUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            });
        }
        Ok(())
    }

    fn dispatch_entropy(
        &mut self,
        memory: &mut GuestMemory,
        entropy_source: &mut (impl VirtioRngEntropySource + ?Sized),
        metrics: &SharedEntropyDeviceMetrics,
    ) -> Option<Duration> {
        let device = self.entropy.as_mut()?;
        let result = device
            .published
            .endpoint()
            .dispatch_rng_queue_notifications(memory, entropy_source);
        let mut retry_after = None;
        match &result {
            Ok(dispatch) => {
                metrics.record_notification_dispatch(dispatch);
                retry_after = dispatch.rate_limiter_retry_after();
            }
            Err(error) => {
                if let Some(completed) = error.completed_device_operation() {
                    metrics.record_notification_dispatch(completed);
                    retry_after = completed.rate_limiter_retry_after();
                }
                if let Some(source) = error.device_error() {
                    metrics.record_notification_error(source);
                    retain_earliest_retry(&mut retry_after, source.rate_limiter_retry_after());
                }
                if error.endpoint_error().is_some() {
                    metrics.record_event_failure();
                }
            }
        }
        let delivered = match &result {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) if error.endpoint_error().is_none() => error
                .device_error()
                .and_then(|source| source.completed_dispatch())
                .is_some_and(|dispatch| dispatch.needs_queue_interrupt()),
            Err(_) => false,
        };
        if delivered {
            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
        }
        retry_after
    }

    fn dispatch_memory_hotplug(
        &mut self,
        memory: &mut GuestMemory,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) {
        let Some(device) = self.memory_hotplug.as_mut() else {
            return;
        };
        let result = device
            .published
            .endpoint()
            .dispatch_mem_queue_notifications_with_executor(memory, mutation_executor);
        let delivered = match &result {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) if error.endpoint_error().is_none() => error
                .device_error()
                .and_then(|source| source.completed_dispatch())
                .is_some_and(|dispatch| dispatch.needs_queue_interrupt()),
            Err(_) => false,
        };
        if delivered {
            device.queue_deliveries = device.queue_deliveries.saturating_add(1);
        }
    }

    fn balloon_updater(&self) -> Option<HvfArm64BootPciBalloonDeviceUpdater> {
        self.balloon.as_ref().map(|device| {
            HvfArm64BootPciBalloonDeviceUpdater::new(device.published.endpoint().clone())
        })
    }

    fn block_updater(&self) -> Option<HvfArm64BootPciBlockDeviceUpdater> {
        (!self.block.is_empty()).then(|| {
            HvfArm64BootPciBlockDeviceUpdater::new(
                self.block
                    .iter()
                    .map(|device| (device.drive_id.clone(), device.published.endpoint().clone()))
                    .collect(),
            )
        })
    }

    fn network_updater(&self) -> Option<HvfArm64BootPciNetworkDeviceUpdater> {
        (!self.network.is_empty()).then(|| {
            HvfArm64BootPciNetworkDeviceUpdater::new(
                self.network
                    .iter()
                    .map(|device| (device.iface_id.clone(), device.published.endpoint().clone()))
                    .collect(),
            )
        })
    }

    fn pmem_updater(&self) -> Option<HvfArm64BootPciPmemDeviceUpdater> {
        (!self.pmem.is_empty()).then(|| {
            HvfArm64BootPciPmemDeviceUpdater::new(
                self.pmem
                    .iter()
                    .map(|device| (device.pmem_id.clone(), device.published.endpoint().clone()))
                    .collect(),
            )
        })
    }

    fn update_memory_hotplug(
        &self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        let device = self
            .memory_hotplug
            .as_ref()
            .ok_or(MemoryHotplugUpdateError::ActiveSessionUnavailable)?;
        match device
            .published
            .endpoint()
            .update_mem_requested_size(update)
        {
            Ok(()) => Ok(()),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(MemoryHotplugUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }

    fn memory_hotplug_status(
        &self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        let device = self
            .memory_hotplug
            .as_ref()
            .ok_or(MemoryHotplugStatusError::ActiveSessionUnavailable)?;
        let plugged_size = device
            .published
            .endpoint()
            .plugged_size()
            .map_err(|source| MemoryHotplugStatusError::ActiveSessionCommand {
                message: source.to_string(),
            })?;
        MemoryHotplugStatus::try_from_plugged_size_bytes(config, plugged_size, requested_size_mib)
    }

    fn vsock_wakeup(&self) -> Result<Option<VsockHostWakeup>, HvfArm64BootPciDataError> {
        let Some(device) = self.vsock.as_ref() else {
            return Ok(None);
        };
        let wakeup = device
            .published
            .endpoint()
            .host_wakeup()
            .map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to access PCI vsock endpoint wakeup state: {source}"
                ))
            })?
            .map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to allocate PCI vsock wakeup state: {source}"
                ))
            })?;
        Ok(Some(wakeup))
    }

    fn vhost_user_block_call_fds(&self) -> Result<Vec<RawFd>, HvfArm64BootPciDataError> {
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(self.block.len())
            .map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to allocate PCI block wakeup descriptors: {source}"
                ))
            })?;
        for device in &self.block {
            if let Some(descriptor) =
                device
                    .published
                    .endpoint()
                    .vhost_user_call_fd()
                    .map_err(|source| {
                        HvfArm64BootPciDataError::new(format!(
                            "failed to access PCI block wakeup state: {source}"
                        ))
                    })?
            {
                descriptors.push(descriptor);
            }
        }
        Ok(descriptors)
    }

    fn teardown(&mut self) -> Result<(), HvfArm64BootPciDataError> {
        let mut dispatcher = self.dispatcher.lock().map_err(|_| {
            HvfArm64BootPciDataError::new("PCI data-device MMIO dispatcher is unavailable")
        })?;
        if let Some(device) = self.memory_hotplug.as_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("memory-hotplug", "mem0", source))?;
            self.memory_hotplug = None;
        }
        if let Some(device) = self.entropy.as_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("entropy", "entropy0", source))?;
            self.entropy = None;
        }
        if let Some(device) = self.vsock.as_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("vsock", "vsock0", source))?;
            self.vsock = None;
        }
        while let Some(device) = self.pmem.last_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("pmem", &device.pmem_id, source))?;
            self.pmem.pop();
        }
        while let Some(device) = self.network.last_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("network", &device.iface_id, source))?;
            self.network.pop();
        }
        while let Some(device) = self.block.last_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("block", &device.drive_id, source))?;
            self.block.pop();
        }
        if let Some(device) = self.balloon.as_mut() {
            device
                .published
                .teardown(&mut dispatcher, self.validation.bar_allocator_mut())
                .map_err(|source| pci_data_teardown_error("balloon", "balloon0", source))?;
            self.balloon = None;
        }
        if let Some(interrupts) = self.msi_interrupts.as_mut() {
            interrupts.release().map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to release shared PCI data GICv2m routes: {source}"
                ))
            })?;
            self.msi_interrupts = None;
        }
        Ok(())
    }
}

/// Cloneable live-update handle for production PCI block endpoints.
#[derive(Debug, Clone)]
pub struct HvfArm64BootPciBlockDeviceUpdater {
    endpoints: Vec<(
        String,
        VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
    )>,
}

impl HvfArm64BootPciBlockDeviceUpdater {
    fn new(
        endpoints: Vec<(
            String,
            VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
        )>,
    ) -> Self {
        Self { endpoints }
    }

    pub fn update_block_device_with_opened(
        &self,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        let endpoint = self
            .endpoints
            .iter()
            .find_map(|(drive_id, endpoint)| (drive_id == config.drive_id()).then_some(endpoint))
            .ok_or_else(|| DriveUpdateError::UnknownDrive {
                drive_id: config.drive_id().to_string(),
            })?;
        if config.is_vhost_user() {
            if backing.is_some() || rate_limiter_update.is_some() {
                return Err(DriveUpdateError::UnsupportedBackend);
            }
            return endpoint.refresh_vhost_user_block_config(config.cache_type());
        }
        endpoint
            .update_block_device_with_opened(config, backing, rate_limiter_update)
            .map_err(|source| DriveUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })
    }
}

fn refresh_mmio_vhost_user_block_config(
    runtime_resources: &Arm64BootRuntimeResources,
    mmio_dispatcher: &Arc<Mutex<MmioDispatcher>>,
    gic: &HvfGicMetadata,
    config: &DriveConfig,
) -> Result<(), DriveUpdateError> {
    let device = runtime_resources
        .block_devices
        .iter()
        .find(|device| device.registration.drive_id() == config.drive_id())
        .ok_or_else(|| DriveUpdateError::UnknownDrive {
            drive_id: config.drive_id().to_string(),
        })?;
    let interrupt_line = device.fdt_device.interrupt_line;
    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        DriveUpdateError::ActiveSessionCommand {
            message: source.to_string(),
        }
    })?;
    signaler.validate_line(interrupt_line).map_err(|source| {
        DriveUpdateError::ActiveSessionCommand {
            message: source.to_string(),
        }
    })?;
    let mut dispatcher = mmio_dispatcher
        .lock()
        .map_err(|_| DriveUpdateError::MmioDispatcherUnavailable)?;
    refresh_vhost_user_block_config_for_devices_with_signal(
        &runtime_resources.block_devices,
        &mut dispatcher,
        config,
        || {
            signaler.set_level(interrupt_line, true).map_err(|source| {
                let delivery_ambiguous = matches!(&source, HvfGicSpiSignalError::Signal { .. });
                VhostUserBlockConfigSignalError::new(source.to_string(), delivery_ambiguous)
            })
        },
    )
}

/// Cloneable live-update handle for production PCI network endpoints.
#[derive(Debug, Clone)]
pub struct HvfArm64BootPciNetworkDeviceUpdater {
    endpoints: Vec<(
        String,
        VirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice>,
    )>,
}

impl HvfArm64BootPciNetworkDeviceUpdater {
    fn new(
        endpoints: Vec<(
            String,
            VirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice>,
        )>,
    ) -> Self {
        Self { endpoints }
    }

    pub fn update_network_interface(
        &self,
        update: &NetworkInterfaceUpdate,
    ) -> Result<(), NetworkInterfaceUpdateError> {
        let endpoint = self
            .endpoints
            .iter()
            .find_map(|(iface_id, endpoint)| (iface_id == update.iface_id()).then_some(endpoint))
            .ok_or_else(|| NetworkInterfaceUpdateError::UnknownInterface {
                iface_id: update.iface_id().to_string(),
            })?;
        endpoint
            .update_network_rate_limiters(update)
            .map_err(|source| NetworkInterfaceUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })
    }
}

/// Cloneable live-update handle for production PCI pmem endpoints.
#[derive(Debug, Clone)]
pub struct HvfArm64BootPciPmemDeviceUpdater {
    endpoints: Vec<(
        String,
        VirtioPciEndpoint<VirtioPmemConfigSpace, VirtioPmemDevice>,
    )>,
}

impl HvfArm64BootPciPmemDeviceUpdater {
    fn new(
        endpoints: Vec<(
            String,
            VirtioPciEndpoint<VirtioPmemConfigSpace, VirtioPmemDevice>,
        )>,
    ) -> Self {
        Self { endpoints }
    }

    pub fn update_pmem(&self, update: &PmemUpdate) -> Result<bool, PmemUpdateError> {
        let endpoint = self
            .endpoints
            .iter()
            .find_map(|(pmem_id, endpoint)| (pmem_id == update.id()).then_some(endpoint))
            .ok_or(PmemUpdateError::UnknownPmem)?;
        endpoint.update_pmem_rate_limiter(update).map_err(|source| {
            PmemUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            }
        })
    }
}

/// Cloneable live-update handle for a production PCI balloon endpoint.
#[derive(Debug, Clone)]
pub struct HvfArm64BootPciBalloonDeviceUpdater {
    endpoint: VirtioPciEndpoint<VirtioBalloonConfigSpace, VirtioBalloonDevice>,
}

impl HvfArm64BootPciBalloonDeviceUpdater {
    fn new(endpoint: VirtioPciEndpoint<VirtioBalloonConfigSpace, VirtioBalloonDevice>) -> Self {
        Self { endpoint }
    }

    pub fn update_balloon_config(&self, config: BalloonConfig) -> Result<(), BalloonUpdateError> {
        match self.endpoint.update_balloon_config(config) {
            Ok(()) => Ok(()),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(BalloonUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }

    pub fn update_balloon_statistics(
        &self,
        input: BalloonStatsUpdateInput,
    ) -> Result<(), BalloonUpdateError> {
        match self.endpoint.update_balloon_statistics(input) {
            Ok(()) => Ok(()),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(BalloonUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }

    pub fn balloon_stats(&self, config: BalloonConfig) -> Result<BalloonStats, BalloonStatsError> {
        match self.endpoint.balloon_stats(config) {
            Ok(stats) => Ok(stats),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(BalloonStatsError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }

    pub fn balloon_hinting_status(
        &self,
    ) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
        match self.endpoint.balloon_hinting_status() {
            Ok(status) => Ok(status),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(BalloonHintingStatusError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }

    pub fn start_balloon_hinting(
        &self,
        input: BalloonHintingStartInput,
    ) -> Result<(), BalloonHintingCommandError> {
        match self.endpoint.start_balloon_hinting(input) {
            Ok(()) => Ok(()),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(BalloonHintingCommandError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }

    pub fn stop_balloon_hinting(&self) -> Result<(), BalloonHintingCommandError> {
        match self.endpoint.stop_balloon_hinting() {
            Ok(()) => Ok(()),
            Err(VirtioPciDeviceOperationError::Device(source)) => Err(*source),
            Err(source) => Err(BalloonHintingCommandError::ActiveSessionCommand {
                message: source.to_string(),
            }),
        }
    }
}

fn retain_earliest_retry(retained: &mut Option<Duration>, candidate: Option<Duration>) {
    if let Some(candidate) = candidate {
        *retained = Some(retained.map_or(candidate, |retained| retained.min(candidate)));
    }
}

fn record_pci_block_operation_error(
    metrics: &SharedBlockDeviceMetricsRegistry,
    drive_id: &str,
    error: &VirtioPciDeviceOperationError<
        VirtioBlockDeviceNotificationError,
        bangbang_runtime::block::VirtioBlockDeviceNotificationDispatch,
    >,
) {
    if let Some(completed) = error.completed_device_operation() {
        metrics.record_notification_dispatch_for_drive(drive_id, completed);
    }
    if let Some(source) = error.device_error() {
        metrics.record_queue_events_for_drive(
            drive_id,
            usize_to_u64_saturating(source.drained_notifications().len()),
        );
        metrics.record_event_failure_for_drive(drive_id);
        if let Some(completed) = source.completed_dispatch() {
            metrics.record_queue_dispatch_for_drive(drive_id, completed);
        }
    }
    if error.endpoint_error().is_some() {
        metrics.record_event_failure_for_drive(drive_id);
    }
}

fn network_error_needs_queue_interrupt(source: &VirtioNetworkDeviceNotificationError) -> bool {
    source
        .completed_initial_rx_dispatch()
        .is_some_and(|dispatch| dispatch.needs_queue_interrupt())
        || source
            .completed_tx_dispatch()
            .is_some_and(|dispatch| dispatch.needs_queue_interrupt())
        || source
            .completed_rx_dispatch()
            .is_some_and(|dispatch| dispatch.needs_queue_interrupt())
}

impl Drop for HvfArm64BootPciDataDevices {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

fn pci_data_endpoint_error(source: VirtioPciEndpointError) -> HvfArm64BootPciDataError {
    HvfArm64BootPciDataError::new(format!("PCI data endpoint is unavailable: {source}"))
}

fn pci_data_teardown_error(
    kind: &str,
    id: &str,
    source: VirtioPciPublicationError,
) -> HvfArm64BootPciDataError {
    HvfArm64BootPciDataError::new(format!(
        "failed to tear down PCI {kind} endpoint {id}: {source}"
    ))
}

fn pci_data_region_id(index: usize) -> Result<MmioRegionId, HvfArm64BootPciDataError> {
    let index = u64::try_from(index)
        .map_err(|_| HvfArm64BootPciDataError::new("PCI data endpoint index does not fit u64"))?;
    let id = PCI_DATA_DEVICE_BAR_REGION_ID_BASE
        .checked_add(index)
        .ok_or_else(|| HvfArm64BootPciDataError::new("PCI data BAR region id overflowed"))?;
    Ok(MmioRegionId::new(id))
}

fn teardown_pci_data_devices(
    devices: &mut Option<HvfArm64BootPciDataDevices>,
) -> Result<(), HvfArm64BootPciDataError> {
    let Some(manager) = devices.as_mut() else {
        return Ok(());
    };
    manager.teardown()?;
    devices.take();
    Ok(())
}

fn dispatch_pci_validation_notifications(
    backend: &mut HvfBackend,
    endpoint: Option<&mut HvfArm64BootPciValidationEndpoint>,
) -> Result<(), HvfArm64BootPciValidationError> {
    let Some(endpoint) = endpoint else {
        return Ok(());
    };
    let memory = backend
        .mapped_guest_memory_mut()
        .map_err(|source| HvfArm64BootPciValidationError::GuestMemory { source })?;
    endpoint.dispatch(memory)
}

fn trigger_pci_validation_config_interrupt(
    endpoint: Option<&mut HvfArm64BootPciValidationEndpoint>,
) -> Result<(), HvfArm64BootPciValidationError> {
    match endpoint {
        Some(endpoint) => endpoint.trigger_config_once(),
        None => Ok(()),
    }
}

fn teardown_pci_validation_endpoint_and_verify_reuse(
    endpoint_slot: &mut Option<HvfArm64BootPciValidationEndpoint>,
    runtime: &mut Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    signaler: Option<HvfGicMsiSignaler>,
) -> Result<Option<HvfArm64BootPciValidationTeardownEvidence>, HvfArm64BootPciValidationTeardownError>
{
    let Some(mut endpoint) = endpoint_slot.take() else {
        return Ok(None);
    };

    let result = (|| {
        let validation = runtime.pci_validation.as_mut().ok_or_else(|| {
            HvfArm64BootPciValidationTeardownError::new(
                "PCI validation runtime resources are unavailable",
            )
        })?;
        let segment = validation.segment().clone();
        let old_bar = endpoint.published.bar_range().ok_or_else(|| {
            HvfArm64BootPciValidationTeardownError::new(
                "PCI validation endpoint no longer owns its BAR before teardown",
            )
        })?;
        let mut dispatcher = dispatcher.lock().map_err(|_| {
            HvfArm64BootPciValidationTeardownError::new(
                "PCI validation MMIO dispatcher state is unavailable",
            )
        })?;
        endpoint
            .published
            .teardown(&mut dispatcher, validation.bar_allocator_mut())
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation endpoint teardown failed: {source}"
                ))
            })?;

        let endpoint_released = endpoint.published.endpoint().phase().map_err(|source| {
            HvfArm64BootPciValidationTeardownError::new(format!(
                "PCI validation endpoint phase is unavailable: {source}"
            ))
        })? == VirtioPciEndpointPhase::Released;
        let stale_endpoint_rejected = matches!(
            endpoint
                .published
                .endpoint()
                .trigger(VirtioInterruptIntent::Queue { queue_index: 0 }),
            Err(VirtioPciEndpointError::NotActive {
                phase: VirtioPciEndpointPhase::Released
            })
        );
        let guest_bar_unpublished = dispatcher.lookup(old_bar.start(), 4).is_err();

        let replacement_bar = validation
            .bar_allocator_mut()
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation BAR reuse failed: {source}"
                ))
            })?;
        let bar_range_reused = replacement_bar.range() == old_bar;
        validation
            .bar_allocator_mut()
            .release(&replacement_bar)
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation replacement BAR release failed: {source}"
                ))
            })?;

        let replacement_function =
            PciType0Configuration::new(0x0042, 0, 0, PciClassCode::Unclassified, 0, 0, 0x0042, 0);
        let replacement_lease = segment
            .with_segment(|segment| segment.add_function(replacement_function))
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation segment lock failed during reuse: {source}"
                ))
            })?
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation slot reuse failed: {source}"
                ))
            })?;
        let pci_slot_reused = replacement_lease.sbdf().device() == PCI_FIRST_ENDPOINT_DEVICE;
        segment
            .with_segment(|segment| segment.remove_function(&replacement_lease))
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation segment lock failed during replacement release: {source}"
                ))
            })?
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation replacement slot release failed: {source}"
                ))
            })?;

        let signaler = signaler.ok_or_else(|| {
            HvfArm64BootPciValidationTeardownError::new(
                "PCI validation MSI signal capability is unavailable during teardown",
            )
        })?;
        let allocator = signaler.allocator();
        let replacements = allocator
            .allocate_many(PCI_VALIDATION_VIRTIO_RNG_VECTOR_COUNT)
            .map_err(|source| {
                HvfArm64BootPciValidationTeardownError::new(format!(
                    "PCI validation message-vector reuse failed: {source}"
                ))
            })?;
        let message_vectors_reused = replacements.len() == PCI_VALIDATION_VIRTIO_RNG_VECTOR_COUNT;
        allocator.release_many(&replacements).map_err(|source| {
            HvfArm64BootPciValidationTeardownError::new(format!(
                "PCI validation replacement message-vector release failed: {source}"
            ))
        })?;

        let evidence = HvfArm64BootPciValidationTeardownEvidence {
            endpoint_released,
            guest_bar_unpublished,
            pci_slot_reused,
            bar_range_reused,
            message_vectors_reused,
            stale_endpoint_rejected,
        };
        if !endpoint_released
            || !guest_bar_unpublished
            || !pci_slot_reused
            || !bar_range_reused
            || !message_vectors_reused
            || !stale_endpoint_rejected
        {
            return Err(HvfArm64BootPciValidationTeardownError::new(
                "PCI validation teardown reuse evidence was incomplete",
            ));
        }
        Ok(evidence)
    })();

    match result {
        Ok(evidence) => Ok(Some(evidence)),
        Err(error) => {
            // A partially completed publication teardown retains its remaining
            // leases and is safe to retry. Once teardown itself completed,
            // however, reinserting the released endpoint would make a later
            // shutdown retry fail before it can make progress because its BAR
            // lease is intentionally gone.
            if !endpoint.published.is_released() {
                *endpoint_slot = Some(endpoint);
            }
            Err(error)
        }
    }
}

/// Redacted evidence retained by the internal signed modern virtio-pci proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64BootPciValidationDiagnostics {
    pub transport: VirtioPciDiagnostics,
    pub queue_deliveries: usize,
    pub config_deliveries: usize,
}

/// Completion evidence for the internal PCI endpoint's normative teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootPciValidationTeardownEvidence {
    pub endpoint_released: bool,
    pub guest_bar_unpublished: bool,
    pub pci_slot_reused: bool,
    pub bar_range_reused: bool,
    pub message_vectors_reused: bool,
    pub stale_endpoint_rejected: bool,
}

/// Value-redacted failure while tearing down the internal PCI proof endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64BootPciValidationTeardownError {
    message: String,
}

impl HvfArm64BootPciValidationTeardownError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for HvfArm64BootPciValidationTeardownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HvfArm64BootPciValidationTeardownError {}

#[derive(Debug)]
pub enum HvfArm64BootPciValidationError {
    DeviceStatePoisoned,
    Dispatch {
        source: VirtioRngDeviceNotificationError,
    },
    Endpoint {
        source: VirtioPciEndpointError,
    },
    MissingQueueVector,
    MissingConfigVector,
    SharedQueueAndConfigVector,
    DeliveryCountOverflow,
    GuestMemory {
        source: HvfGuestMemoryMappingError,
    },
}

impl fmt::Display for HvfArm64BootPciValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceStatePoisoned => {
                f.write_str("PCI validation virtio-rng device state is unavailable")
            }
            Self::Dispatch { source } => {
                write!(f, "PCI validation virtio-rng dispatch failed: {source}")
            }
            Self::Endpoint { source } => {
                write!(f, "PCI validation virtio-pci endpoint failed: {source}")
            }
            Self::MissingQueueVector => {
                f.write_str("PCI validation guest did not assign a queue MSI-X vector")
            }
            Self::MissingConfigVector => {
                f.write_str("PCI validation guest did not assign a configuration MSI-X vector")
            }
            Self::SharedQueueAndConfigVector => f.write_str(
                "PCI validation guest assigned the same MSI-X table index to queue and configuration",
            ),
            Self::DeliveryCountOverflow => {
                f.write_str("PCI validation interrupt delivery count overflowed")
            }
            Self::GuestMemory { source } => {
                write!(f, "PCI validation guest memory is unavailable: {source}")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootPciValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Dispatch { source } => Some(source),
            Self::Endpoint { source } => Some(source),
            Self::GuestMemory { source } => Some(source),
            Self::DeviceStatePoisoned
            | Self::MissingQueueVector
            | Self::MissingConfigVector
            | Self::SharedQueueAndConfigVector
            | Self::DeliveryCountOverflow => None,
        }
    }
}

#[derive(Debug)]
pub struct HvfArm64BootSession<'vm> {
    runner: HvfArm64BootVcpuSession<'vm>,
    backend: &'vm mut HvfBackend,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    runtime_resources: Arm64BootRuntimeResources,
    pci_validation_endpoint: Option<HvfArm64BootPciValidationEndpoint>,
    pci_data_devices: Option<HvfArm64BootPciDataDevices>,
    cache_source: crate::vcpu_config::HvfArm64VcpuCacheFdtSource,
    cache_hierarchy: Option<Arm64FdtCacheHierarchy>,
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    run_loop_wakeup: HvfArm64BootRunLoopWakeupToken,
    block_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    pmem_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    pmem_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    network_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    network_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_source: VirtioRngOsEntropySource,
    block_device_metrics: SharedBlockDeviceMetricsRegistry,
    pmem_device_metrics: SharedPmemDeviceMetricsRegistry,
    balloon_device_metrics: SharedBalloonDeviceMetrics,
    network_interface_metrics: SharedNetworkInterfaceMetricsRegistry,
    vsock_device_metrics: SharedVsockDeviceMetrics,
    entropy_device_metrics: SharedEntropyDeviceMetrics,
    gic: HvfGicMetadata,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    pmem_interrupt_lines: Vec<GuestInterruptLine>,
    network_interrupt_lines: Vec<GuestInterruptLine>,
    vsock_interrupt_line: Option<GuestInterruptLine>,
    balloon_interrupt_line: Option<GuestInterruptLine>,
    entropy_interrupt_line: Option<GuestInterruptLine>,
    memory_hotplug_interrupt_line: Option<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    vmgenid_interrupt_line: GuestInterruptLine,
    vmclock_interrupt_line: GuestInterruptLine,
    boot_registers: Option<HvfArm64BootRegisters>,
}

#[derive(Debug)]
pub struct OwnedHvfArm64BootSession {
    runner: HvfArm64BootVcpuSession<'static>,
    backend: HvfBackend,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    runtime_resources: Arm64BootRuntimeResources,
    pci_validation_endpoint: Option<HvfArm64BootPciValidationEndpoint>,
    pci_data_devices: Option<HvfArm64BootPciDataDevices>,
    cache_source: crate::vcpu_config::HvfArm64VcpuCacheFdtSource,
    cache_hierarchy: Option<Arm64FdtCacheHierarchy>,
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    run_loop_wakeup: HvfArm64BootRunLoopWakeupToken,
    block_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    pmem_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    pmem_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    network_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    network_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_source: VirtioRngOsEntropySource,
    block_device_metrics: SharedBlockDeviceMetricsRegistry,
    pmem_device_metrics: SharedPmemDeviceMetricsRegistry,
    balloon_device_metrics: SharedBalloonDeviceMetrics,
    network_interface_metrics: SharedNetworkInterfaceMetricsRegistry,
    vsock_device_metrics: SharedVsockDeviceMetrics,
    entropy_device_metrics: SharedEntropyDeviceMetrics,
    gic: HvfGicMetadata,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    pmem_interrupt_lines: Vec<GuestInterruptLine>,
    network_interrupt_lines: Vec<GuestInterruptLine>,
    vsock_interrupt_line: Option<GuestInterruptLine>,
    balloon_interrupt_line: Option<GuestInterruptLine>,
    entropy_interrupt_line: Option<GuestInterruptLine>,
    memory_hotplug_interrupt_line: Option<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    vmgenid_interrupt_line: GuestInterruptLine,
    vmclock_interrupt_line: GuestInterruptLine,
    boot_registers: Option<HvfArm64BootRegisters>,
}

/// A never-run native-v1 session plus process-owned restored configuration.
pub struct RestoredHvfArm64BootSession {
    session: OwnedHvfArm64BootSession,
    drive_config: DriveConfig,
    serial_output: SharedSerialOutput,
    serial_output_buffer: SharedSerialOutputBuffer,
}

impl RestoredHvfArm64BootSession {
    pub const fn session(&self) -> &OwnedHvfArm64BootSession {
        &self.session
    }

    pub const fn drive_config(&self) -> &DriveConfig {
        &self.drive_config
    }

    pub const fn serial_output(&self) -> &SharedSerialOutput {
        &self.serial_output
    }

    pub const fn serial_output_buffer(&self) -> &SharedSerialOutputBuffer {
        &self.serial_output_buffer
    }

    pub fn into_parts(
        self,
    ) -> (
        OwnedHvfArm64BootSession,
        DriveConfig,
        SharedSerialOutput,
        SharedSerialOutputBuffer,
    ) {
        (
            self.session,
            self.drive_config,
            self.serial_output,
            self.serial_output_buffer,
        )
    }
}

impl fmt::Debug for RestoredHvfArm64BootSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestoredHvfArm64BootSession")
            .field("profile", &"native-v1")
            .field("resources", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct HvfArm64BootRunLoopStopToken {
    stop_requested: Arc<AtomicBool>,
}

impl HvfArm64BootRunLoopStopToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
    }

    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Default)]
struct HvfArm64BootRunLoopWakeupToken {
    pending_wakeups: Arc<AtomicUsize>,
}

impl HvfArm64BootRunLoopWakeupToken {
    fn request_wakeup(&self) {
        let _ =
            self.pending_wakeups
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                    Some(pending.saturating_add(1))
                });
    }

    fn take_wakeup_request(&self) -> bool {
        self.pending_wakeups
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                pending.checked_sub(1)
            })
            .is_ok()
    }
}

#[derive(Debug, Clone, Default)]
struct HvfArm64BootLimiterRetryWakeupToken {
    wakeup_requested: Arc<AtomicBool>,
}

impl HvfArm64BootLimiterRetryWakeupToken {
    fn request_wakeup(&self) {
        self.wakeup_requested.store(true, Ordering::Relaxed);
    }

    fn take_wakeup_request(&self) -> bool {
        self.wakeup_requested.swap(false, Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct HvfArm64BootLimiterRetryWakeupScheduler {
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
    thread: Option<JoinHandle<()>>,
}

impl HvfArm64BootLimiterRetryWakeupScheduler {
    fn inactive() -> Self {
        Self {
            shared: Arc::new(HvfArm64BootLimiterRetryWakeupSchedulerShared::default()),
            thread: None,
        }
    }

    fn start(
        thread_name: &'static str,
        cancel_handle: HvfVcpuRunCancelHandle,
        wakeup_token: HvfArm64BootLimiterRetryWakeupToken,
    ) -> Result<Self, io::Error> {
        Self::start_with_cancellation(thread_name, wakeup_token, move || cancel_handle.cancel())
    }

    fn start_with_cancellation<R>(
        thread_name: &'static str,
        wakeup_token: HvfArm64BootLimiterRetryWakeupToken,
        cancel_vcpu_run: impl Fn() -> R + Send + 'static,
    ) -> Result<Self, io::Error> {
        let shared = Arc::new(HvfArm64BootLimiterRetryWakeupSchedulerShared::default());
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                run_limiter_retry_wakeup_scheduler(thread_shared, wakeup_token, cancel_vcpu_run);
            })?;

        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    fn schedule_after(&self, retry_after: Option<Duration>) {
        self.schedule_deadline(retry_after.map(limiter_retry_deadline_after));
    }

    fn schedule_deadline(&self, deadline: Option<Instant>) {
        if self.thread.is_none() {
            return;
        }

        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        if matches!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped
        ) {
            return;
        }
        state.deadline = deadline;
        if deadline.is_none() {
            state.deferred_publication = false;
        }
        self.shared.condvar.notify_one();
    }

    fn cancel_and_wait(&self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        state.deadline = None;
        state.deferred_publication = false;
        self.shared.condvar.notify_all();

        while state.publication_in_flight {
            state = wait_limiter_retry_wakeup_state(&self.shared, state);
        }
    }

    fn quiesce(
        &self,
    ) -> Result<
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError,
    > {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        match state.status {
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running => {
                state.status = HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced;
            }
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced => {
                return Err(
                    HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced,
                );
            }
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped => {
                return Err(HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped);
            }
        }
        self.shared.condvar.notify_all();

        while state.publication_in_flight {
            state = wait_limiter_retry_wakeup_state(&self.shared, state);
            if matches!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped
            ) {
                return Err(HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped);
            }
        }

        Ok(HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
            shared: Arc::clone(&self.shared),
        })
    }

    fn stop_with_result(&mut self) -> Result<(), ()> {
        {
            let mut state = lock_limiter_retry_wakeup_state(&self.shared);
            state.status = HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped;
            state.deadline = None;
            state.deferred_publication = false;
        }
        self.shared.condvar.notify_all();

        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| ()),
            None => Ok(()),
        }
    }

    fn stop(&mut self) {
        let _ = self.stop_with_result();
    }
}

impl Drop for HvfArm64BootLimiterRetryWakeupScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Default)]
struct HvfArm64BootLimiterRetryWakeupSchedulerShared {
    state: Mutex<HvfArm64BootLimiterRetryWakeupSchedulerState>,
    condvar: Condvar,
}

#[derive(Debug, Default)]
struct HvfArm64BootLimiterRetryWakeupSchedulerState {
    deadline: Option<Instant>,
    status: HvfArm64BootLimiterRetryWakeupSchedulerStatus,
    publication_in_flight: bool,
    deferred_publication: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum HvfArm64BootLimiterRetryWakeupSchedulerStatus {
    #[default]
    Running,
    Quiesced,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError {
    AlreadyQuiesced,
    Stopped,
}

#[derive(Debug)]
struct HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
}

impl HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
    fn defer_publication(&self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        if matches!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced
        ) {
            state.deferred_publication = true;
        }
    }
}

impl Drop for HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
    fn drop(&mut self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        if matches!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced
        ) {
            state.status = HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running;
            self.shared.condvar.notify_all();
        }
    }
}

/// Holds block, pmem, network, and entropy limiter retry wakeup schedulers quiesced.
///
/// Dropping the guard resumes each scheduler that has not stopped. Active
/// scheduler threads then publish any retry wakeup that became due while the
/// guard was held.
#[derive(Debug)]
#[must_use = "dropping the guard resumes limiter retry wakeup publication"]
pub struct HvfArm64BootLimiterRetryWakeupQuiescenceGuard {
    block: HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
    _pmem: HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
    _network: HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
    _entropy: HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
}

impl HvfArm64BootLimiterRetryWakeupQuiescenceGuard {
    pub fn block_retry_state_at(
        &self,
        now: Instant,
    ) -> Result<SnapshotV1BlockRetryState, HvfArm64BootLimiterRetrySnapshotError> {
        snapshot_limiter_retry_state_at(&self.block, now)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootLimiterRetrySnapshotError {
    Poisoned,
    NotQuiesced,
    Stopped,
    PublicationInFlight,
    DurationOverflow,
}

impl fmt::Display for HvfArm64BootLimiterRetrySnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Poisoned => f.write_str("limiter retry snapshot state is poisoned"),
            Self::NotQuiesced => f.write_str("limiter retry snapshot is not quiesced"),
            Self::Stopped => f.write_str("limiter retry snapshot scheduler is stopped"),
            Self::PublicationInFlight => {
                f.write_str("limiter retry snapshot publication is in flight")
            }
            Self::DurationOverflow => {
                f.write_str("limiter retry snapshot duration is out of bounds")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootLimiterRetrySnapshotError {}

fn snapshot_limiter_retry_state_at(
    guard: &HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
    now: Instant,
) -> Result<SnapshotV1BlockRetryState, HvfArm64BootLimiterRetrySnapshotError> {
    let state = guard
        .shared
        .state
        .lock()
        .map_err(|_| HvfArm64BootLimiterRetrySnapshotError::Poisoned)?;
    match state.status {
        HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running => {
            return Err(HvfArm64BootLimiterRetrySnapshotError::NotQuiesced);
        }
        HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped => {
            return Err(HvfArm64BootLimiterRetrySnapshotError::Stopped);
        }
        HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced => {}
    }
    if state.publication_in_flight {
        return Err(HvfArm64BootLimiterRetrySnapshotError::PublicationInFlight);
    }
    if state.deferred_publication {
        return Ok(SnapshotV1BlockRetryState::Immediate);
    }
    let Some(deadline) = state.deadline else {
        return Ok(SnapshotV1BlockRetryState::None);
    };
    let Some(remaining) = deadline.checked_duration_since(now) else {
        return Ok(SnapshotV1BlockRetryState::Immediate);
    };
    let remaining_nanos = u64::try_from(remaining.as_nanos())
        .map_err(|_| HvfArm64BootLimiterRetrySnapshotError::DurationOverflow)?;
    if remaining_nanos == 0 {
        Ok(SnapshotV1BlockRetryState::Immediate)
    } else {
        Ok(SnapshotV1BlockRetryState::After { remaining_nanos })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootSnapshotV1DeviceCaptureError {
    WrongQuiescenceGuard,
    RetryState,
    GuestMemory,
    MmioDispatcherBusy,
    MmioDispatcherPoisoned,
    RuntimeCapture,
}

#[derive(Debug)]
pub enum HvfArm64BootSnapshotV1StateCaptureError {
    UnsupportedVcpuCount {
        vcpu_count: usize,
    },
    Cancelled {
        stage: HvfArm64BootSnapshotV1CaptureStage,
    },
    CacheIdentityMismatch,
    Runner {
        source: HvfVcpuRunnerError,
    },
    Device {
        source: HvfArm64BootSnapshotV1DeviceCaptureError,
    },
    MissingRtc,
    EncodePreflight {
        source: HvfSnapshotV1EncodeError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64BootSnapshotV1CaptureStage {
    CacheManifest,
    Runner,
    Device,
    EncodePreflight,
}

impl fmt::Display for HvfArm64BootSnapshotV1CaptureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::CacheManifest => "cache manifest",
            Self::Runner => "runner state",
            Self::Device => "device state",
            Self::EncodePreflight => "encoding preflight",
        };
        f.write_str(name)
    }
}

impl fmt::Display for HvfArm64BootSnapshotV1StateCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVcpuCount { vcpu_count } => write!(
                f,
                "native-v1 state capture supports exactly one vCPU, got {vcpu_count}"
            ),
            Self::Cancelled { stage } => {
                write!(f, "native-v1 state capture was cancelled before {stage}")
            }
            Self::CacheIdentityMismatch => {
                f.write_str("native-v1 cache compatibility identity changed after startup")
            }
            Self::Runner { source } => {
                write!(f, "native-v1 runner capture failed: {source}")
            }
            Self::Device { source } => {
                write!(f, "native-v1 device capture failed: {source}")
            }
            Self::MissingRtc => f.write_str("native-v1 session is missing the mandatory PL031 RTC"),
            Self::EncodePreflight { source } => {
                write!(f, "native-v1 state encoding preflight failed: {source}")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootSnapshotV1StateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runner { source } => Some(source),
            Self::Device { source } => Some(source),
            Self::EncodePreflight { source } => Some(source),
            Self::UnsupportedVcpuCount { .. }
            | Self::Cancelled { .. }
            | Self::CacheIdentityMismatch
            | Self::MissingRtc => None,
        }
    }
}

impl fmt::Display for HvfArm64BootSnapshotV1DeviceCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongQuiescenceGuard => {
                f.write_str("snapshot device capture quiescence guard belongs to another session")
            }
            Self::RetryState => f.write_str("snapshot device retry-state capture failed"),
            Self::GuestMemory => f.write_str("snapshot device guest memory is unavailable"),
            Self::MmioDispatcherBusy => f.write_str("snapshot device MMIO dispatcher is busy"),
            Self::MmioDispatcherPoisoned => {
                f.write_str("snapshot device MMIO dispatcher is poisoned")
            }
            Self::RuntimeCapture => f.write_str("snapshot device runtime capture failed"),
        }
    }
}

impl std::error::Error for HvfArm64BootSnapshotV1DeviceCaptureError {}

struct HvfArm64BootSnapshotV1DeviceCaptureOwners<'a> {
    backend: &'a HvfBackend,
    mmio_dispatcher: &'a Arc<Mutex<MmioDispatcher>>,
    runtime_resources: &'a Arm64BootRuntimeResources,
    block_retry_wakeup_scheduler: &'a HvfArm64BootLimiterRetryWakeupScheduler,
}

impl HvfArm64BootSnapshotV1DeviceCaptureOwners<'_> {
    fn capture_at(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
    ) -> Result<SnapshotV1DeviceState, HvfArm64BootSnapshotV1DeviceCaptureError> {
        if !Arc::ptr_eq(
            &guard.block.shared,
            &self.block_retry_wakeup_scheduler.shared,
        ) {
            return Err(HvfArm64BootSnapshotV1DeviceCaptureError::WrongQuiescenceGuard);
        }
        let block_retry = guard
            .block_retry_state_at(now)
            .map_err(|_| HvfArm64BootSnapshotV1DeviceCaptureError::RetryState)?;
        let memory = self
            .backend
            .mapped_guest_memory()
            .map_err(|_| HvfArm64BootSnapshotV1DeviceCaptureError::GuestMemory)?;
        let mut mmio_dispatcher =
            lock_boot_mmio_dispatcher(self.mmio_dispatcher).map_err(|error| match error {
                HvfArm64BootMmioDispatcherError::Busy => {
                    HvfArm64BootSnapshotV1DeviceCaptureError::MmioDispatcherBusy
                }
                HvfArm64BootMmioDispatcherError::Poisoned => {
                    HvfArm64BootSnapshotV1DeviceCaptureError::MmioDispatcherPoisoned
                }
            })?;
        self.runtime_resources
            .capture_snapshot_v1_device_state_at(
                memory,
                &mut mmio_dispatcher,
                drive_config,
                serial_config,
                block_retry,
                now,
            )
            .map_err(|_| HvfArm64BootSnapshotV1DeviceCaptureError::RuntimeCapture)
    }
}

fn build_snapshot_v1_state(
    runtime_resources: &Arm64BootRuntimeResources,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    cache_manifest: crate::vcpu_config::HvfArm64VcpuCacheManifest,
    runner_capture: HvfArm64SnapshotV1Capture,
    device: SnapshotV1DeviceState,
) -> Result<HvfSnapshotV1State, HvfArm64BootSnapshotV1StateCaptureError> {
    let rtc = runtime_resources
        .rtc_device
        .as_ref()
        .ok_or(HvfArm64BootSnapshotV1StateCaptureError::MissingRtc)?;
    let rtc_mmio_layout = RtcMmioLayout::new(rtc.region.range().start(), rtc.region.id());
    let (identification, optional_identification, vcpu, interrupts) = runner_capture.into_parts();
    let compatibility = HvfSnapshotV1CompatibilityState::new(
        identification,
        optional_identification,
        cache_manifest,
        primary_mpidr,
        gic,
        rtc_mmio_layout,
    );
    let state = HvfSnapshotV1State::new(
        runtime_resources.machine_config,
        compatibility,
        vcpu,
        interrupts,
        device,
    );
    // Fail before streaming guest memory if any fixed component, nested device
    // value, or compatibility relationship cannot be encoded.
    encode_hvf_snapshot_v1_state(&state)
        .map_err(|source| HvfArm64BootSnapshotV1StateCaptureError::EncodePreflight { source })?;
    Ok(state)
}

fn check_snapshot_v1_capture_cancelled(
    is_cancelled: &mut impl FnMut(HvfArm64BootSnapshotV1CaptureStage) -> bool,
    stage: HvfArm64BootSnapshotV1CaptureStage,
) -> Result<(), HvfArm64BootSnapshotV1StateCaptureError> {
    if is_cancelled(stage) {
        Err(HvfArm64BootSnapshotV1StateCaptureError::Cancelled { stage })
    } else {
        Ok(())
    }
}

fn retained_snapshot_cache_manifest(
    source: crate::vcpu_config::HvfArm64VcpuCacheFdtSource,
    runner_id_aa64mmfr2_el1: u64,
) -> Result<crate::vcpu_config::HvfArm64VcpuCacheManifest, HvfArm64BootSnapshotV1StateCaptureError>
{
    if runner_id_aa64mmfr2_el1 != source.id_aa64mmfr2_el1() {
        return Err(HvfArm64BootSnapshotV1StateCaptureError::CacheIdentityMismatch);
    }
    Ok(source.manifest())
}

/// Describes why a boot session could not quiesce its limiter retry wakeups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootLimiterRetryWakeupQuiescenceError {
    BlockAlreadyQuiesced,
    BlockStopped,
    PmemAlreadyQuiesced,
    PmemStopped,
    NetworkAlreadyQuiesced,
    NetworkStopped,
    EntropyAlreadyQuiesced,
    EntropyStopped,
}

impl fmt::Display for HvfArm64BootLimiterRetryWakeupQuiescenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockAlreadyQuiesced => {
                f.write_str("block limiter retry wakeups are already quiesced")
            }
            Self::BlockStopped => f.write_str("block limiter retry wakeup scheduler is stopped"),
            Self::PmemAlreadyQuiesced => {
                f.write_str("pmem limiter retry wakeups are already quiesced")
            }
            Self::PmemStopped => f.write_str("pmem limiter retry wakeup scheduler is stopped"),
            Self::NetworkAlreadyQuiesced => {
                f.write_str("network limiter retry wakeups are already quiesced")
            }
            Self::NetworkStopped => {
                f.write_str("network limiter retry wakeup scheduler is stopped")
            }
            Self::EntropyAlreadyQuiesced => {
                f.write_str("entropy limiter retry wakeups are already quiesced")
            }
            Self::EntropyStopped => {
                f.write_str("entropy limiter retry wakeup scheduler is stopped")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootLimiterRetryWakeupQuiescenceError {}

#[derive(Clone, Copy)]
struct HvfArm64BootLimiterRetryWakeupOwner<'a> {
    wakeup: &'a HvfArm64BootLimiterRetryWakeupToken,
    scheduler: &'a HvfArm64BootLimiterRetryWakeupScheduler,
}

impl<'a> HvfArm64BootLimiterRetryWakeupOwner<'a> {
    const fn new(
        wakeup: &'a HvfArm64BootLimiterRetryWakeupToken,
        scheduler: &'a HvfArm64BootLimiterRetryWakeupScheduler,
    ) -> Self {
        Self { wakeup, scheduler }
    }
}

fn quiesce_limiter_retry_wakeups(
    block: HvfArm64BootLimiterRetryWakeupOwner<'_>,
    pmem: HvfArm64BootLimiterRetryWakeupOwner<'_>,
    network: HvfArm64BootLimiterRetryWakeupOwner<'_>,
    entropy: HvfArm64BootLimiterRetryWakeupOwner<'_>,
) -> Result<
    HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
    HvfArm64BootLimiterRetryWakeupQuiescenceError,
> {
    let block_guard = block.scheduler.quiesce().map_err(|err| match err {
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::BlockAlreadyQuiesced
        }
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::BlockStopped
        }
    })?;
    let pmem_guard = pmem.scheduler.quiesce().map_err(|err| match err {
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::PmemAlreadyQuiesced
        }
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::PmemStopped
        }
    })?;
    let network_guard = network.scheduler.quiesce().map_err(|err| match err {
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::NetworkAlreadyQuiesced
        }
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::NetworkStopped
        }
    })?;
    let entropy_guard = entropy.scheduler.quiesce().map_err(|err| match err {
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::EntropyAlreadyQuiesced
        }
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped => {
            HvfArm64BootLimiterRetryWakeupQuiescenceError::EntropyStopped
        }
    })?;

    if block.wakeup.take_wakeup_request() {
        block_guard.defer_publication();
    }
    if pmem.wakeup.take_wakeup_request() {
        pmem_guard.defer_publication();
    }
    if network.wakeup.take_wakeup_request() {
        network_guard.defer_publication();
    }
    if entropy.wakeup.take_wakeup_request() {
        entropy_guard.defer_publication();
    }

    Ok(HvfArm64BootLimiterRetryWakeupQuiescenceGuard {
        block: block_guard,
        _pmem: pmem_guard,
        _network: network_guard,
        _entropy: entropy_guard,
    })
}

#[derive(Debug)]
struct HvfArm64BootLimiterRetryWakeupPublicationGuard {
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
}

impl Drop for HvfArm64BootLimiterRetryWakeupPublicationGuard {
    fn drop(&mut self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        state.publication_in_flight = false;
        self.shared.condvar.notify_all();
    }
}

fn limiter_retry_deadline_after(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}

fn lock_limiter_retry_wakeup_state(
    shared: &HvfArm64BootLimiterRetryWakeupSchedulerShared,
) -> MutexGuard<'_, HvfArm64BootLimiterRetryWakeupSchedulerState> {
    match shared.state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_limiter_retry_wakeup_state<'a>(
    shared: &'a HvfArm64BootLimiterRetryWakeupSchedulerShared,
    state: MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState>,
) -> MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState> {
    match shared.condvar.wait(state) {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_limiter_retry_wakeup_state_timeout<'a>(
    shared: &'a HvfArm64BootLimiterRetryWakeupSchedulerShared,
    state: MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState>,
    timeout: Duration,
) -> MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState> {
    match shared.condvar.wait_timeout(state, timeout) {
        Ok((state, _)) => state,
        Err(poisoned) => poisoned.into_inner().0,
    }
}

fn wait_to_publish_limiter_retry_wakeup(
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
) -> bool {
    let mut state = lock_limiter_retry_wakeup_state(&shared);
    loop {
        match state.status {
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running => {}
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced => {
                state = wait_limiter_retry_wakeup_state(&shared, state);
                continue;
            }
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped => return false,
        }

        let now = Instant::now();
        if state.deferred_publication {
            state.deferred_publication = false;
            if state.deadline.is_some_and(|deadline| deadline <= now) {
                state.deadline = None;
            }
            state.publication_in_flight = true;
            return true;
        }

        let Some(deadline) = state.deadline else {
            state = wait_limiter_retry_wakeup_state(&shared, state);
            continue;
        };

        if deadline <= now {
            state.deadline = None;
            state.publication_in_flight = true;
            return true;
        }

        state = wait_limiter_retry_wakeup_state_timeout(
            &shared,
            state,
            deadline.saturating_duration_since(now),
        );
    }
}

fn run_limiter_retry_wakeup_scheduler<R>(
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
    wakeup_token: HvfArm64BootLimiterRetryWakeupToken,
    cancel_vcpu_run: impl Fn() -> R,
) {
    while wait_to_publish_limiter_retry_wakeup(Arc::clone(&shared)) {
        let _publication = HvfArm64BootLimiterRetryWakeupPublicationGuard {
            shared: Arc::clone(&shared),
        };
        wakeup_token.request_wakeup();
        let _cancel_result = cancel_vcpu_run();
    }
}

#[derive(Debug, Clone, Default)]
struct HvfArm64BootRunLoopControlWakeupToken {
    wakeup_requested: Arc<AtomicBool>,
}

impl HvfArm64BootRunLoopControlWakeupToken {
    fn request_wakeup(&self) {
        self.wakeup_requested.store(true, Ordering::Relaxed);
    }

    fn take_wakeup_request(&self) -> bool {
        self.wakeup_requested.swap(false, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootRunLoopControl {
    stop_token: HvfArm64BootRunLoopStopToken,
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    vcpu_control: HvfVcpuRunControl,
}

impl HvfArm64BootRunLoopControl {
    fn new(
        vcpu_control: HvfVcpuRunControl,
        control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    ) -> Self {
        Self {
            stop_token: HvfArm64BootRunLoopStopToken::new(),
            control_wakeup,
            vcpu_control,
        }
    }

    pub fn stop_token(&self) -> HvfArm64BootRunLoopStopToken {
        self.stop_token.clone()
    }

    /// Request a sticky run-loop stop and interrupt the current topology run.
    ///
    /// If the topology can no longer accept a control barrier, the stop flag
    /// remains set so a run loop that regains control still observes the
    /// request. The returned error reports only the failed vCPU interruption.
    pub fn request_stop(&self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.stop_token.request_stop();
        self.vcpu_control.request_stop().map(|_| ())
    }

    /// Wake the boot run loop without requesting guest shutdown.
    ///
    /// This is runner-command plumbing for future runtime device updates. It
    /// lets the process worker regain control while keeping stop semantics
    /// separate from ordinary command dispatch. A failed vCPU interruption
    /// leaves the wakeup pending so a run loop that regains control does not
    /// lose the request.
    pub fn request_wakeup(&self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.control_wakeup.request_wakeup();
        self.vcpu_control.request_wakeup().map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64BootRunLoopOutcome {
    StepLimitReached { steps: usize },
    Wakeup { steps: usize },
    Stopped { steps: usize },
    Canceled { steps: usize },
    GuestShutdown { steps: usize },
    GuestReset { steps: usize },
    Unknown { steps: usize, reason: u32 },
}

#[derive(Debug)]
pub enum HvfArm64BootRunLoopError {
    StartVsockWakeupMonitor {
        steps_completed: usize,
        source: Box<HvfArm64BootRunLoopWakeupMonitorError>,
    },
    RunStep {
        steps_completed: usize,
        source: Box<HvfArm64BootVcpuError>,
    },
    StopVsockWakeupMonitor {
        steps_completed: usize,
        source: Box<HvfArm64BootRunLoopWakeupMonitorError>,
    },
    DispatchBlockNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootBlockNotificationDispatchError>,
    },
    DispatchPmemNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootPmemNotificationDispatchError>,
    },
    DispatchNetworkNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootNetworkNotificationDispatchError>,
    },
    DispatchVsockNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootVsockNotificationDispatchError>,
    },
    DispatchBalloonNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootBalloonNotificationDispatchError>,
    },
    DispatchMemoryHotplugNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootMemoryHotplugNotificationDispatchError>,
    },
    DispatchEntropyNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootEntropyNotificationDispatchError>,
    },
    HandleVirtualTimer {
        steps_completed: usize,
        source: Box<HvfArm64BootVcpuError>,
    },
}

impl fmt::Display for HvfArm64BootRunLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartVsockWakeupMonitor {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to start HVF boot-session vsock wakeup monitor after {steps_completed} completed steps: {source}"
            ),
            Self::RunStep {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to run HVF boot-session vCPU step after {steps_completed} completed steps: {source}"
            ),
            Self::StopVsockWakeupMonitor {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to stop HVF boot-session vsock wakeup monitor after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchBlockNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session block notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchPmemNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session pmem notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchNetworkNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session network notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchVsockNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session vsock notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchBalloonNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session balloon notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchMemoryHotplugNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session memory-hotplug notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchEntropyNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session entropy notifications after {steps_completed} completed steps: {source}"
            ),
            Self::HandleVirtualTimer {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to handle HVF boot-session virtual timer after {steps_completed} completed steps: {source}"
            ),
        }
    }
}

impl std::error::Error for HvfArm64BootRunLoopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StartVsockWakeupMonitor { source, .. } => Some(source.as_ref()),
            Self::RunStep { source, .. } => Some(source.as_ref()),
            Self::StopVsockWakeupMonitor { source, .. } => Some(source.as_ref()),
            Self::DispatchBlockNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchPmemNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchNetworkNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchVsockNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchBalloonNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchMemoryHotplugNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchEntropyNotifications { source, .. } => Some(source.as_ref()),
            Self::HandleVirtualTimer { source, .. } => Some(source.as_ref()),
        }
    }
}

#[derive(Debug)]
pub enum HvfArm64BootRunLoopWakeupMonitorError {
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    CollectVsockWakeupFds {
        source: Arm64BootVsockWakeupFdsError,
    },
    CollectBlockWakeupFds {
        source: Arm64BootBlockWakeupFdsError,
    },
    PciVsockWakeup {
        message: String,
    },
    PciBlockWakeup {
        message: String,
    },
    PollFdAllocation {
        source: TryReserveError,
    },
    TooManyPollFds {
        count: usize,
    },
    CreateStopPipe {
        source: io::ErrorKind,
    },
    ThreadSpawn {
        source: io::Error,
    },
    StopSignal {
        source: io::ErrorKind,
    },
    ThreadPanicked,
}

impl fmt::Display for HvfArm64BootRunLoopWakeupMonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::CollectVsockWakeupFds { source } => {
                write!(f, "failed to collect boot vsock wakeup fds: {source}")
            }
            Self::CollectBlockWakeupFds { source } => {
                write!(f, "failed to collect boot block wakeup fds: {source}")
            }
            Self::PciVsockWakeup { message } => {
                write!(f, "failed to collect PCI vsock wakeup fds: {message}")
            }
            Self::PciBlockWakeup { message } => {
                write!(f, "failed to collect PCI block wakeup fds: {message}")
            }
            Self::PollFdAllocation { source } => {
                write!(
                    f,
                    "failed to allocate boot vsock wakeup poll fd list: {source}"
                )
            }
            Self::TooManyPollFds { count } => {
                write!(f, "too many boot vsock wakeup poll fds: {count}")
            }
            Self::CreateStopPipe { source } => {
                write!(f, "failed to create boot vsock wakeup stop pipe: {source}")
            }
            Self::ThreadSpawn { source } => {
                write!(f, "failed to spawn boot vsock wakeup monitor: {source}")
            }
            Self::StopSignal { source } => {
                write!(
                    f,
                    "failed to signal boot vsock wakeup monitor stop: {source}"
                )
            }
            Self::ThreadPanicked => f.write_str("boot vsock wakeup monitor thread panicked"),
        }
    }
}

impl std::error::Error for HvfArm64BootRunLoopWakeupMonitorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CollectVsockWakeupFds { source } => Some(source),
            Self::CollectBlockWakeupFds { source } => Some(source),
            Self::PollFdAllocation { source } => Some(source),
            Self::ThreadSpawn { source } => Some(source),
            Self::MmioDispatcher { .. }
            | Self::PciVsockWakeup { .. }
            | Self::PciBlockWakeup { .. }
            | Self::TooManyPollFds { .. }
            | Self::CreateStopPipe { .. }
            | Self::StopSignal { .. }
            | Self::ThreadPanicked => None,
        }
    }
}

impl HvfArm64BootSession<'_> {
    /// Quiesce block, pmem, network, and entropy limiter retry wakeup publication.
    pub fn quiesce_limiter_retry_wakeups(
        &self,
    ) -> Result<
        HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        HvfArm64BootLimiterRetryWakeupQuiescenceError,
    > {
        quiesce_limiter_retry_wakeups(
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.block_retry_wakeup,
                &self.block_retry_wakeup_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.pmem_retry_wakeup,
                &self.pmem_retry_wakeup_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.network_retry_wakeup,
                &self.network_retry_wakeup_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.entropy_retry_wakeup,
                &self.entropy_retry_wakeup_scheduler,
            ),
        )
    }

    pub fn capture_snapshot_v1_device_state_at(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
    ) -> Result<SnapshotV1DeviceState, HvfArm64BootSnapshotV1DeviceCaptureError> {
        HvfArm64BootSnapshotV1DeviceCaptureOwners {
            backend: self.backend,
            mmio_dispatcher: &self.mmio_dispatcher,
            runtime_resources: &self.runtime_resources,
            block_retry_wakeup_scheduler: &self.block_retry_wakeup_scheduler,
        }
        .capture_at(drive_config, serial_config, guard, now)
    }

    /// Capture and preflight every non-memory native-v1 component while the
    /// caller retains the supervisor and auxiliary quiescence guards.
    pub fn capture_snapshot_v1_state_at(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
    ) -> Result<HvfSnapshotV1State, HvfArm64BootSnapshotV1StateCaptureError> {
        self.capture_snapshot_v1_state_at_with_cancel(
            drive_config,
            serial_config,
            guard,
            now,
            |_| false,
        )
    }

    pub fn capture_snapshot_v1_state_at_with_cancel(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
        mut is_cancelled: impl FnMut(HvfArm64BootSnapshotV1CaptureStage) -> bool,
    ) -> Result<HvfSnapshotV1State, HvfArm64BootSnapshotV1StateCaptureError> {
        if self.runner.member_count() != 1 {
            return Err(
                HvfArm64BootSnapshotV1StateCaptureError::UnsupportedVcpuCount {
                    vcpu_count: self.runner.member_count(),
                },
            );
        }
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::CacheManifest,
        )?;
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::Runner,
        )?;
        let runner_capture = self
            .runner
            .capture_arm64_snapshot_v1_state()
            .map_err(|source| HvfArm64BootSnapshotV1StateCaptureError::Runner { source })?;
        let cache_manifest = retained_snapshot_cache_manifest(
            self.cache_source,
            runner_capture.identification().id_aa64mmfr2_el1(),
        )?;
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::Device,
        )?;
        let device = self
            .capture_snapshot_v1_device_state_at(drive_config, serial_config, guard, now)
            .map_err(|source| HvfArm64BootSnapshotV1StateCaptureError::Device { source })?;
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::EncodePreflight,
        )?;
        build_snapshot_v1_state(
            &self.runtime_resources,
            self.gic,
            self.primary_mpidr(),
            cache_manifest,
            runner_capture,
            device,
        )
    }

    /// Requests an immediate run-loop retry after a live pmem limiter update
    /// unblocks work that was already pending on a virtqueue.
    pub fn schedule_pmem_retry_wakeup_after_live_update(&mut self, has_pending_work: bool) {
        if has_pending_work {
            self.pmem_retry_wakeup_scheduler
                .schedule_after(Some(Duration::ZERO));
        }
    }

    pub fn shutdown(&mut self) -> Result<(), HvfArm64BootSessionShutdownError> {
        self.block_retry_wakeup_scheduler.stop();
        self.pmem_retry_wakeup_scheduler.stop();
        self.network_retry_wakeup_scheduler.stop();
        self.entropy_retry_wakeup_scheduler.stop();
        let pci_data_result = self.teardown_pci_data_devices();
        let pci_result = self.teardown_pci_validation_endpoint();
        let runner_result = self.runner.shutdown();
        if let Err(source) = runner_result {
            return Err(HvfArm64BootSessionShutdownError::Vcpu { source });
        }
        if let Err(source) = pci_data_result {
            return Err(HvfArm64BootSessionShutdownError::PciData { source });
        }
        if let Err(source) = pci_result {
            return Err(HvfArm64BootSessionShutdownError::PciValidation { source });
        }
        <HvfBackend as VmBackend>::destroy_vm(self.backend)
            .map_err(|source| HvfArm64BootSessionShutdownError::DestroyVm { source })
    }

    pub const fn gic_metadata(&self) -> HvfGicMetadata {
        self.gic
    }

    /// Return the send-only capability retained by an MSI-enabled session.
    pub fn gic_msi_signaler(&self) -> Option<&HvfGicMsiSignaler> {
        self.backend.gic_msi_signaler()
    }

    /// Dispatches the internal modern virtio-pci validation device, if present.
    #[doc(hidden)]
    pub fn dispatch_pci_validation_notifications(
        &mut self,
    ) -> Result<(), HvfArm64BootPciValidationError> {
        dispatch_pci_validation_notifications(self.backend, self.pci_validation_endpoint.as_mut())
    }

    /// Triggers the validation device's configuration vector exactly once.
    #[doc(hidden)]
    pub fn trigger_pci_validation_config_interrupt(
        &mut self,
    ) -> Result<(), HvfArm64BootPciValidationError> {
        trigger_pci_validation_config_interrupt(self.pci_validation_endpoint.as_mut())
    }

    /// Returns value-redacted state from the internal modern PCI proof.
    #[doc(hidden)]
    pub fn pci_validation_diagnostics(
        &self,
    ) -> Option<Result<HvfArm64BootPciValidationDiagnostics, HvfArm64BootPciValidationError>> {
        self.pci_validation_endpoint
            .as_ref()
            .map(HvfArm64BootPciValidationEndpoint::diagnostics)
    }

    /// Returns value-redacted state from the hidden PCI data-device proof.
    #[doc(hidden)]
    pub fn pci_data_device_diagnostics(
        &self,
    ) -> Option<Result<Vec<HvfArm64BootPciDataDeviceDiagnostics>, HvfArm64BootPciDataError>> {
        self.pci_data_devices
            .as_ref()
            .map(HvfArm64BootPciDataDevices::diagnostics)
    }

    /// Tears down the internal endpoint and proves released capacity is reusable.
    #[doc(hidden)]
    pub fn teardown_pci_validation_endpoint(
        &mut self,
    ) -> Result<
        Option<HvfArm64BootPciValidationTeardownEvidence>,
        HvfArm64BootPciValidationTeardownError,
    > {
        let signaler = self.backend.gic_msi_signaler().cloned();
        teardown_pci_validation_endpoint_and_verify_reuse(
            &mut self.pci_validation_endpoint,
            &mut self.runtime_resources,
            &self.mmio_dispatcher,
            signaler,
        )
    }

    /// Explicitly tears down every hidden PCI data endpoint in reverse order.
    #[doc(hidden)]
    pub fn teardown_pci_data_devices(&mut self) -> Result<(), HvfArm64BootPciDataError> {
        teardown_pci_data_devices(&mut self.pci_data_devices)
    }

    pub fn primary_mpidr(&self) -> u64 {
        self.runner.primary_mpidr()
    }

    pub fn vcpu_count(&self) -> usize {
        self.runner.member_count()
    }

    pub fn vcpu_mpidrs(&self) -> &[u64] {
        self.runner.mpidrs()
    }

    pub fn last_vcpu_terminal_report(&self) -> Option<&HvfVcpuRunTerminalReport> {
        self.runner.last_terminal_report()
    }

    pub fn runtime_resources(&self) -> &Arm64BootRuntimeResources {
        &self.runtime_resources
    }

    pub fn pci_balloon_device_updater(&self) -> Option<HvfArm64BootPciBalloonDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::balloon_updater)
    }

    pub fn pci_block_device_updater(&self) -> Option<HvfArm64BootPciBlockDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::block_updater)
    }

    /// Reports whether block operations belong to the retained PCI inventory.
    pub const fn uses_pci_data_devices(&self) -> bool {
        self.pci_data_devices.is_some()
    }

    /// Checks every owner-side prerequisite for a vhost-user runtime insert
    /// without reserving capacity or cloning shared-memory descriptors.
    pub fn preflight_runtime_vhost_user_block_device(
        &self,
        drive_id: &str,
    ) -> Result<(), DriveRuntimeMutationError> {
        let memory = self.guest_memory().map_err(|source| {
            DriveRuntimeMutationError::ActiveSessionCommand {
                message: format!("guest memory is unavailable: {source}"),
            }
        })?;
        if memory.backing() != GuestMemoryBacking::Shared {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "vhost-user block requires shared guest memory".to_string(),
            });
        }
        PreparedBlockDevice::preflight_vhost_user_memory(memory).map_err(|source| {
            DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        self.pci_data_devices
            .as_ref()
            .ok_or(DriveRuntimeMutationError::PciNotEnabled)?
            .preflight_runtime_block(drive_id, &self.block_device_metrics)
    }

    /// Materializes one closed runtime resource on the owner and publishes it
    /// through the generic PCI transaction.
    pub fn insert_runtime_block_resource(
        &mut self,
        resource: RuntimeBlockDeviceResource,
    ) -> Result<(), DriveRuntimeMutationError> {
        let prepared = match resource {
            RuntimeBlockDeviceResource::Prepared(prepared) => prepared,
            RuntimeBlockDeviceResource::VhostUser { config, frontend } => {
                self.preflight_runtime_vhost_user_block_device(config.drive_id())?;
                PreparedBlockDevice::from_config_with_vhost_user(
                    &config,
                    frontend,
                    self.guest_memory().map_err(|source| {
                        DriveRuntimeMutationError::ActiveSessionCommand {
                            message: format!("guest memory is unavailable: {source}"),
                        }
                    })?,
                )
                .map_err(|source| DriveRuntimeMutationError::PrepareDevice {
                    message: source.to_string(),
                })?
            }
        };
        self.insert_runtime_block_device(prepared)
    }

    /// Publishes one fully prepared block endpoint into the owner-thread PCI inventory.
    pub fn insert_runtime_block_device(
        &mut self,
        prepared: PreparedBlockDevice,
    ) -> Result<(), DriveRuntimeMutationError> {
        let metrics = self.block_device_metrics.clone();
        self.pci_data_devices
            .as_mut()
            .ok_or(DriveRuntimeMutationError::PciNotEnabled)?
            .insert_runtime_block(prepared, &metrics)
    }

    /// Updates one entry by resolving it from the current owner-thread inventory.
    pub fn update_runtime_block_device_with_opened(
        &mut self,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        self.pci_data_devices
            .as_ref()
            .ok_or(DriveUpdateError::ActiveSessionUnavailable)?
            .update_runtime_block(config, backing, rate_limiter_update)
    }

    /// Refreshes one active direct vhost-user block configuration through its
    /// selected MMIO or PCI transport and delivers the guest notification.
    pub fn refresh_vhost_user_block_config(
        &mut self,
        config: &DriveConfig,
    ) -> Result<(), DriveUpdateError> {
        if !config.is_vhost_user() {
            return Err(DriveUpdateError::UnsupportedBackend);
        }
        if let Some(devices) = self.pci_data_devices.as_ref() {
            return devices.update_runtime_block(config, None, None);
        }
        refresh_mmio_vhost_user_block_config(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            &self.gic,
            config,
        )
    }

    /// Removes one non-root block endpoint from the owner-thread PCI inventory.
    pub fn remove_runtime_block_device(
        &mut self,
        drive_id: &str,
    ) -> Result<(), DriveRuntimeMutationError> {
        remove_runtime_pci_block_device_and_refresh_retry(
            &mut self.pci_data_devices,
            &self.block_retry_wakeup,
            &self.block_retry_wakeup_scheduler,
            drive_id,
        )
    }

    /// Maps and publishes one pmem endpoint in the owner-thread PCI inventory.
    pub fn insert_runtime_pmem_device(
        &mut self,
        config: &PmemConfig,
        backing: PmemFileBacking,
    ) -> Result<(), PmemRuntimeMutationError> {
        let metrics = self.pmem_device_metrics.clone();
        self.pci_data_devices
            .as_mut()
            .ok_or(PmemRuntimeMutationError::PciNotEnabled)?
            .insert_runtime_pmem(
                self.backend,
                &mut self.runtime_resources.pmem_devices,
                config,
                backing,
                &metrics,
            )
    }

    /// Updates a pmem endpoint by resolving its current live owner entry.
    pub fn update_runtime_pmem_device(
        &mut self,
        update: &PmemUpdate,
    ) -> Result<bool, PmemUpdateError> {
        self.pci_data_devices
            .as_ref()
            .ok_or(PmemUpdateError::ActiveSessionUnavailable)?
            .update_runtime_pmem(update)
    }

    /// Flushes and removes one pmem endpoint from the owner-thread inventory.
    pub fn remove_runtime_pmem_device(
        &mut self,
        pmem_id: &str,
    ) -> Result<(), PmemRuntimeMutationError> {
        remove_runtime_pci_pmem_device_and_refresh_retry(
            &mut self.pci_data_devices,
            self.backend,
            &mut self.runtime_resources.pmem_devices,
            &self.pmem_retry_wakeup,
            &self.pmem_retry_wakeup_scheduler,
            pmem_id,
        )
    }

    /// Publishes one prepared network endpoint into the owner-thread PCI inventory.
    pub fn insert_runtime_network_device(
        &mut self,
        prepared: PreparedNetworkDevice,
    ) -> Result<(), NetworkRuntimeMutationError> {
        let metrics = self.network_interface_metrics.clone();
        self.pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?
            .insert_runtime_network(prepared, &metrics)
    }

    /// Updates one network entry by resolving it from the current live inventory.
    pub fn update_runtime_network_interface(
        &mut self,
        update: &NetworkInterfaceUpdate,
    ) -> Result<(), NetworkInterfaceUpdateError> {
        self.pci_data_devices
            .as_ref()
            .ok_or(NetworkInterfaceUpdateError::ActiveSessionUnavailable)?
            .update_runtime_network(update)
    }

    /// Makes one network endpoint reversibly unreachable before packet-I/O teardown.
    pub fn prepare_runtime_network_device_removal(
        &mut self,
        iface_id: &str,
    ) -> Result<PreparedHvfArm64BootPciNetworkRemoval, NetworkRuntimeMutationError> {
        self.pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?
            .prepare_runtime_network_removal(iface_id)
    }

    /// Restores one reversibly removed network endpoint.
    pub fn rollback_runtime_network_device_removal(
        &mut self,
        prepared: PreparedHvfArm64BootPciNetworkRemoval,
    ) -> Result<(), NetworkRuntimeMutationError> {
        self.pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?
            .rollback_runtime_network_removal(prepared)
    }

    /// Commits endpoint teardown after packet-I/O cleanup succeeds.
    pub fn commit_runtime_network_device_removal(
        &mut self,
        prepared: PreparedHvfArm64BootPciNetworkRemoval,
    ) -> Result<(), NetworkRuntimeMutationError> {
        let devices = self
            .pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?;
        devices.commit_runtime_network_removal(prepared)?;
        refresh_network_retry_wakeup_after_inventory_change(
            &self.network_retry_wakeup,
            &self.network_retry_wakeup_scheduler,
            devices.network_retry_deadline(),
        );
        Ok(())
    }

    pub fn pci_network_device_updater(&self) -> Option<HvfArm64BootPciNetworkDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::network_updater)
    }

    pub fn pci_pmem_device_updater(&self) -> Option<HvfArm64BootPciPmemDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::pmem_updater)
    }

    /// Return the exact validated cache presentation selected before VM creation.
    pub fn arm64_fdt_cache_hierarchy(&self) -> Option<&Arm64FdtCacheHierarchy> {
        self.cache_hierarchy.as_ref()
    }

    pub fn shared_balloon_device_metrics(&self) -> SharedBalloonDeviceMetrics {
        self.balloon_device_metrics.clone()
    }

    pub fn shared_block_device_metrics(&self) -> SharedBlockDeviceMetricsRegistry {
        self.block_device_metrics.clone()
    }

    pub fn shared_pmem_device_metrics(&self) -> SharedPmemDeviceMetricsRegistry {
        self.pmem_device_metrics.clone()
    }

    pub fn shared_network_interface_metrics(&self) -> SharedNetworkInterfaceMetricsRegistry {
        self.network_interface_metrics.clone()
    }

    pub fn shared_vsock_device_metrics(&self) -> SharedVsockDeviceMetrics {
        self.vsock_device_metrics.clone()
    }

    pub fn shared_entropy_device_metrics(&self) -> SharedEntropyDeviceMetrics {
        self.entropy_device_metrics.clone()
    }

    pub fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        self.runtime_resources
            .rtc_device
            .as_ref()
            .map(|device| device.metrics.clone())
    }

    /// Return a cloned handle to the runner-compatible MMIO dispatcher.
    ///
    /// The dispatcher is local to this boot session. It is shared only so
    /// vCPU-runner commands can dispatch MMIO on the runner thread. Keep cloned
    /// handles scoped to runner commands so dispatcher-owned device resources
    /// are released with the session.
    pub fn mmio_dispatcher(&self) -> Arc<Mutex<MmioDispatcher>> {
        Arc::clone(&self.mmio_dispatcher)
    }

    /// Borrow the guest memory mapped for this prepared boot session.
    ///
    /// This is startup preparation plumbing; it does not enter a continuous
    /// guest run loop or prove guest boot.
    pub fn guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory()
    }

    /// Mutably borrow the guest memory mapped for this prepared boot session.
    ///
    /// The HVF backend remains the mapping owner, so shutdown and drop still
    /// unmap the memory through the backend.
    pub fn guest_memory_mut(&mut self) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory_mut()
    }

    /// Advance one complete dirty generation under snapshot-ready quiescence.
    pub fn reset_dirty_epoch_quiesced(
        &mut self,
    ) -> Result<Option<u64>, HvfDirtyWriteEpochResetError> {
        if !self.runtime_resources.machine_config.track_dirty_pages() {
            return Ok(None);
        }
        self.backend
            .reset_dirty_epoch_quiesced()?
            .ok_or(HvfDirtyWriteEpochResetError::InvalidState(
                "tracked boot session has no active dirty tracker",
            ))
            .map(Some)
    }

    pub fn block_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.block_interrupt_lines
    }

    pub fn pmem_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.pmem_interrupt_lines
    }

    pub fn network_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.network_interrupt_lines
    }

    pub const fn vsock_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.vsock_interrupt_line
    }

    pub const fn balloon_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.balloon_interrupt_line
    }

    pub const fn entropy_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.entropy_interrupt_line
    }

    pub const fn memory_hotplug_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.memory_hotplug_interrupt_line
    }

    pub const fn serial_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.serial_interrupt_line
    }

    pub const fn vmgenid_interrupt_line(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt_line
    }

    pub const fn vmclock_interrupt_line(&self) -> GuestInterruptLine {
        self.vmclock_interrupt_line
    }

    pub const fn boot_registers(&self) -> Option<HvfArm64BootRegisters> {
        self.boot_registers
    }

    /// Capture X0-X30, PC, and CPSR on the primary vCPU's owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It does not
    /// establish snapshot readiness or return complete restorable vCPU state.
    pub fn capture_arm64_general_register_state(
        &self,
    ) -> Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_general_register_state()
    }

    /// Restore X0-X30, PC, and CPSR on the primary vCPU's owner thread.
    ///
    /// Writes are ordered but nontransactional. If one setter fails, retry the
    /// complete typed state or discard the session before guest execution.
    /// Snapshot schema, validation, and wider-state ordering remain outside
    /// this runner primitive.
    pub fn restore_arm64_general_register_state(
        &self,
        state: &HvfArm64VcpuGeneralRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_general_register_state(state)
    }

    /// Capture raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It omits
    /// the wider system-register, SIMD/FP, interrupt, and restore inventories.
    pub fn capture_arm64_core_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_core_system_register_state()
    }

    /// Restore SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 on the owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Schema, validation, and wider ordering remain outside
    /// this runner primitive.
    pub fn restore_arm64_core_system_register_state(
        &self,
        state: &HvfArm64VcpuCoreSystemRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_core_system_register_state(state)
    }

    /// Capture raw EL1 exception-register state on the primary owner thread.
    ///
    /// This subset has a paired low-level restore but omits vector-table
    /// memory, semantic validation, persistence, schema, and wider ordering.
    pub fn capture_arm64_exception_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_exception_register_state()
    }

    /// Restore raw EL1 exception-register state on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Vector memory, validation, schema, and wider ordering
    /// remain outside this runner primitive.
    pub fn restore_arm64_exception_register_state(
        &self,
        state: &HvfArm64VcpuExceptionRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_exception_register_state(state)
    }

    /// Capture raw EL1 ACTLR and CPACR controls on the primary owner thread.
    ///
    /// Complete capture requires macOS 15 and has a paired low-level restore,
    /// but omits feature validation, persistence, and transition ordering.
    pub fn capture_arm64_execution_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_execution_control_register_state()
    }

    /// Restore raw EL1 ACTLR and CPACR controls on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Feature validation, guest ISB transitions, schema, and
    /// wider ordering remain outside this runner primitive.
    pub fn restore_arm64_execution_control_register_state(
        &self,
        state: &HvfArm64VcpuExecutionControlRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_execution_control_register_state(state)
    }

    /// Capture raw EL1 CSSELR cache-size selection state on the primary owner
    /// thread.
    ///
    /// This value is not cache topology. Its complete typed value has a paired
    /// low-level restore, but omits an atomic feature/geometry manifest,
    /// selector and destination validation, synchronization, maintenance,
    /// persistence, schema, and portable restore policy.
    pub fn capture_arm64_cache_selection_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCacheSelectionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_cache_selection_register_state()
    }

    /// Restore raw EL1 CSSELR cache-size selection state on the primary owner
    /// thread.
    ///
    /// The one write is nontransactional. If it fails, retry the complete typed
    /// state or discard the session before guest execution. Selector
    /// interpretation/validation, an atomic destination cache manifest, ISB
    /// and dependent CCSIDR ordering, maintenance, persistence, rollback,
    /// schema, and snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_cache_selection_register_state(
        &self,
        state: &HvfArm64VcpuCacheSelectionRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_cache_selection_register_state(state)
    }

    /// Capture every implemented raw EL1 hardware-breakpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest addresses or identities. This
    /// getter-only subset omits watchpoints, HVF trap state, persistence,
    /// breakpoint activation, and a safe restore sequence.
    pub fn capture_arm64_breakpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuBreakpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_breakpoint_register_state()
    }

    /// Capture every implemented raw EL1 hardware-watchpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest data addresses. This getter-only
    /// subset omits HVF trap state, persistence, watchpoint activation, and a
    /// safe restore sequence.
    pub fn capture_arm64_watchpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuWatchpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_watchpoint_register_state()
    }

    /// Capture raw EL1 MDCCINT and MDSCR debug controls on the primary owner
    /// thread.
    ///
    /// Capture reads this incomplete guest debug subset only and excludes the
    /// separately captured comparator and host trap-policy state.
    pub fn capture_arm64_debug_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_control_register_state()
    }

    /// Restore raw EL1 MDCCINT and MDSCR controls on the primary owner thread.
    ///
    /// The two writes are ordered and nontransactional. Retry the complete
    /// typed state or discard the vCPU after failure. This primitive excludes
    /// feature/writable-bit validation, comparator/trap coordination,
    /// persistence, wider debug ordering, and public snapshot-load behavior.
    pub fn restore_arm64_debug_control_register_state(
        &self,
        state: &HvfArm64VcpuDebugControlRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_debug_control_register_state(state)
    }

    /// Capture Hypervisor.framework debug-trap policy on the primary owner thread.
    ///
    /// Capture reads host policy only. It excludes guest debug-register
    /// contents, persistence, activation, and wider restore policy.
    pub fn capture_arm64_debug_trap_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugTrapState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_trap_state()
    }

    /// Restore Hypervisor.framework debug-trap policy on the primary owner thread.
    ///
    /// The two policy writes are ordered and nontransactional. Retry the
    /// complete typed state or discard the vCPU after failure. This primitive
    /// excludes guest debug-register restore, validation, persistence, wider
    /// debug ordering, and public snapshot-load behavior.
    pub fn restore_arm64_debug_trap_state(
        &self,
        state: &HvfArm64VcpuDebugTrapState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_debug_trap_state(state)
    }

    /// Capture guest-visible arm64 identification registers on the primary
    /// owner thread.
    ///
    /// These raw virtual-CPU/HVF compatibility inputs define no destination
    /// policy, persistence format, or mutable restore state.
    pub fn capture_arm64_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_identification_register_state()
    }

    /// Capture optional SVE/SME identification metadata on the primary owner thread.
    ///
    /// These macOS 15.2+ ZFR0/SMFR0 compatibility inputs exclude feature masks,
    /// destination policy, SVE/SME execution state, persistence, and restore.
    pub fn capture_arm64_sve_sme_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSveSmeIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner
            .capture_arm64_sve_sme_identification_register_state()
    }

    /// Capture mutable SME PSTATE on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value contains `PSTATE.SM` and
    /// `PSTATE.ZA`. It excludes SVE/SME register contents, setters,
    /// persistence, snapshot schema, and restore ordering.
    pub fn capture_arm64_sme_pstate(&self) -> Result<HvfArm64VcpuSmePstate, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_pstate()
    }

    /// Capture all streaming SVE P registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.SM`, preserves every
    /// maximum-SVL/8 predicate byte, and redacts `Debug`. It excludes Z/ZA/ZT0,
    /// setters, persistence, snapshot schema, and restore ordering.
    pub fn capture_arm64_sme_p_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmePRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_p_register_state()
    }

    /// Capture all streaming SVE Z registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.SM`, preserves every
    /// maximum-SVL byte, and redacts `Debug`. It excludes P/ZA/ZT0, setters,
    /// persistence, snapshot schema, and restore ordering.
    pub fn capture_arm64_sme_z_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeZRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_z_register_state()
    }

    /// Capture the complete SME ZA matrix on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.ZA` but not
    /// `PSTATE.SM`, preserves the maximum-SVL square, and redacts `Debug`. It
    /// excludes Z/P/ZT0, setters, layout interpretation, persistence, snapshot
    /// schema, and restore ordering.
    pub fn capture_arm64_sme_za_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeZaRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_za_register_state()
    }

    /// Capture the fixed 64-byte SME2 ZT0 register on the primary owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.ZA` but not
    /// `PSTATE.SM`, is independent of maximum SVL, and redacts `Debug`. It
    /// excludes Z/P/ZA, setters, lane interpretation, persistence, snapshot
    /// schema, and restore ordering.
    pub fn capture_arm64_sme_zt0_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeZt0RegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_zt0_register_state()
    }

    /// Capture raw SME system registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value contains `SMCR_EL1`, `SMPRI_EL1`,
    /// and `TPIDR2_EL0`; `Debug` redacts all three. It excludes maximum SVL,
    /// SVE/SME data, setters, persistence, schema, and restore ordering.
    pub fn capture_arm64_sme_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeSystemRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_system_register_state()
    }

    /// Capture raw system-context registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ value contains `SCXTNUM_EL0` and `SCXTNUM_EL1`;
    /// `Debug` redacts both guest software context numbers. The complete typed
    /// value has a paired low-level restore, but interpretation, validation,
    /// persistence, schema, and wider-context ordering remain outside it.
    pub fn capture_arm64_system_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSystemContextRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_system_context_register_state()
    }

    /// Restore raw system-context registers on the primary vCPU owner thread.
    ///
    /// The EL0-then-EL1 writes are nontransactional. If a setter fails, retry
    /// the complete redacted typed state or discard the session before guest
    /// execution. Interpretation, feature/destination validation, protected
    /// persistence, TPIDR/TPIDR2/CONTEXTIDR ordering, rollback, schema, and
    /// snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_system_context_register_state(
        &self,
        state: &HvfArm64VcpuSystemContextRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_system_context_register_state(state)
    }

    /// Capture raw EL1 translation-register state on the primary owner thread.
    ///
    /// `SCXTNUM_EL0`/`SCXTNUM_EL1` context is captured separately. This subset
    /// has a paired low-level capture-order restore, but omits table memory,
    /// feature validation, TLB/cache maintenance, persistence, and a safe MMU
    /// transition sequence.
    pub fn capture_arm64_translation_register_state(
        &self,
    ) -> Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_translation_register_state()
    }

    /// Restore raw EL1 translation-register state on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Page tables, validation, barriers/maintenance, safe
    /// MMU transition ordering, and schema remain outside this primitive.
    pub fn restore_arm64_translation_register_state(
        &self,
        state: &HvfArm64VcpuTranslationRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_translation_register_state(state)
    }

    /// Capture the five raw EL1 pointer-authentication keys on the primary
    /// owner thread.
    ///
    /// Debug formatting is redacted. The complete typed value has a paired
    /// low-level restore, but feature validation, persistence protection, and
    /// safe SCTLR enable ordering remain outside this subset.
    pub fn capture_arm64_pointer_authentication_key_state(
        &self,
    ) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pointer_authentication_key_state()
    }

    /// Restore the five raw EL1 pointer-authentication keys on the primary
    /// owner thread.
    ///
    /// Ten low/high writes follow capture order but are nontransactional. If a
    /// setter fails, retry the complete redacted typed state or discard the
    /// session before guest execution. Feature/destination validation,
    /// zeroization, protected persistence, SCTLR ordering, rollback, schema,
    /// and snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_pointer_authentication_key_state(
        &self,
        state: &HvfArm64VcpuPointerAuthenticationKeyState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_pointer_authentication_key_state(state)
    }

    /// Capture raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// These sensitive software thread-ID values can contain guest pointers.
    /// `TPIDR2_EL0` is captured separately with SME system registers, while
    /// `SCXTNUM_EL0`/`SCXTNUM_EL1` use a separate system-context value. The
    /// complete typed value has a paired low-level restore, but wider context,
    /// validation, and snapshot policy remain outside this subset.
    pub fn capture_arm64_thread_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_thread_context_register_state()
    }

    /// Restore raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Pointer validation, wider context ordering, and schema
    /// remain outside this runner primitive.
    pub fn restore_arm64_thread_context_register_state(
        &self,
        state: &HvfArm64VcpuThreadContextRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_thread_context_register_state(state)
    }

    /// Capture raw Q0-Q31, FPCR, and FPSR values on the primary vCPU owner
    /// thread.
    ///
    /// This baseline SIMD/FP view has a paired low-level capture-order restore,
    /// but omits SVE/SME state, alias ordering, validation, and the wider
    /// snapshot inventory.
    pub fn capture_arm64_simd_fp_state(
        &self,
    ) -> Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_simd_fp_state()
    }

    /// Restore raw Q0-Q31, FPCR, and FPSR values on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. SVE/SME alias ordering, validation, rollback, schema,
    /// and snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_simd_fp_state(
        &self,
        state: &HvfArm64VcpuSimdFpState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_simd_fp_state(state)
    }

    /// Capture raw EL1 physical-timer state on the primary owner thread.
    ///
    /// CNTP capture requires macOS 15 and a GIC created before the vCPU. The
    /// absolute CVAL and time-sensitive relative TVAL observations are read
    /// sequentially and have no portable restore-time adjustment policy.
    pub fn capture_arm64_physical_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_physical_timer_state()
    }

    /// Capture raw virtual-timer mask, offset, control, and compare state on the
    /// primary vCPU owner thread.
    ///
    /// This subset omits pending interrupt and GIC state, and the host-time
    /// adjustment required by a restore policy. The control status bit is
    /// derived and may change as virtual time advances.
    pub fn capture_arm64_virtual_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_virtual_timer_state()
    }

    /// Capture normalized physical and virtual timers for native-HVF restore.
    pub fn capture_arm64_snapshot_timer_state(
        &self,
    ) -> Result<HvfArm64SnapshotTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_snapshot_timer_state()
    }

    /// Restore normalized timers before this session's first vCPU run.
    pub fn restore_arm64_snapshot_timer_state(
        &mut self,
        state: HvfArm64SnapshotTimerState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_snapshot_timer_state(state)
    }

    /// Replace the retained VMGenID and inject its edge-rising SPI.
    ///
    /// Runner and signaler capabilities are preflighted before guest memory is
    /// changed. A signal failure is reported after memory and host metadata
    /// have committed; retry the complete method or discard the unrun session.
    pub fn replace_vmgenid_for_snapshot_restore(
        &mut self,
    ) -> Result<(), HvfArm64BootVmGenIdRestoreError> {
        replace_vmgenid_for_snapshot_restore(
            &self.runner,
            self.backend,
            &mut self.runtime_resources,
            self.gic,
            self.vmgenid_interrupt_line,
        )
    }

    /// Capture CPU-level IRQ/FIQ pending state on the primary vCPU owner thread.
    ///
    /// HVF clears these per-run injection levels after a vCPU run returns.
    /// This subset has a paired low-level restore but excludes GIC/device state,
    /// delivery policy, automatic per-run reassertion, and snapshot orchestration.
    pub fn capture_arm64_pending_interrupt_state(
        &self,
    ) -> Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pending_interrupt_state()
    }

    /// Restore CPU-level IRQ/FIQ pending state on the primary vCPU owner thread.
    ///
    /// The IRQ-then-FIQ writes are nontransactional. Retry the complete value
    /// or discard the session before execution after failure. HVF clears both
    /// levels after a run; GIC/device composition, delivery/EOI, automatic
    /// reassertion, persistence, schema, and snapshot orchestration remain
    /// outside this primitive.
    pub fn restore_arm64_pending_interrupt_state(
        &self,
        state: &HvfArm64VcpuPendingInterruptState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_pending_interrupt_state(state)
    }

    /// Capture opaque, versioned GIC device state while the primary runner is
    /// stopped.
    ///
    /// The bytes exclude vCPU-affine GIC CPU registers and are not yet a
    /// bangbang snapshot schema or validated restore input.
    pub fn capture_gic_device_state(&self) -> Result<HvfGicDeviceState, HvfVcpuRunnerError> {
        self.runner.capture_gic_device_state()
    }

    /// Reapply complete opaque GIC device state before the primary vCPU runs.
    ///
    /// Session preparation has already created the GIC and vCPU. Compatible
    /// ICC state must follow under a future wider restore lease; after any
    /// failure, discard the session rather than execute it.
    pub fn restore_gic_device_state(
        &self,
        state: &HvfGicDeviceState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_gic_device_state(state)
    }

    /// Capture raw EL1 GIC ICC registers on the primary vCPU owner thread.
    ///
    /// This per-vCPU value complements the opaque GIC device blob but is not a
    /// complete or persisted snapshot. EL2 ICC, ICH, and ICV state are omitted.
    pub fn capture_arm64_gic_icc_register_state(
        &self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_gic_icc_register_state()
    }

    /// Restore raw EL1 GIC ICC registers on the primary vCPU owner thread.
    ///
    /// Apply the compatible opaque GIC blob first. Nine mutable-register writes
    /// are ordered around a derived `ICC_RPR_EL1` validation and are
    /// nontransactional; no vCPU run may have been enqueued. Retry the complete
    /// value or discard the session after a partial failure. This is not a wider
    /// restore lease, persisted snapshot, or destination policy.
    pub fn restore_arm64_gic_icc_register_state(
        &self,
        state: &HvfArm64GicIccRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_gic_icc_register_state(state)
    }

    /// Run a size-one boot session's primary vCPU once with runner-thread MMIO handling.
    ///
    /// Multi-vCPU sessions must use the aggregate run loop. This compatibility
    /// primitive does not dispatch boot block or virtio-net TX notifications or
    /// enter a continuous guest run loop.
    pub fn run_once_and_handle_mmio(&self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        run_boot_session_vcpu_step(self.runner.singular_runner()?, &self.mmio_dispatcher)
    }

    /// Return a primary-only compatibility handle for a size-one run step.
    ///
    /// Multi-vCPU aggregate execution must use [`Self::run_loop_control`]. This
    /// handle does not shut down the boot session or enter a continuous loop.
    pub fn run_cancel_handle(&self) -> HvfVcpuRunCancelHandle {
        self.runner.run_cancel_handle()
    }

    /// Return a control handle for the bounded internal boot-session run loop.
    ///
    /// Stop and non-stop wakeup requests use the existing runner cancellation
    /// boundary. This remains internal runner-loop plumbing and does not start
    /// an unbounded guest loop.
    pub fn run_loop_control(&self) -> HvfArm64BootRunLoopControl {
        HvfArm64BootRunLoopControl::new(self.runner.control(), self.control_wakeup.clone())
    }

    /// Run bounded vCPU steps and dispatch boot block and virtio-net TX
    /// notifications plus virtio-vsock TX notifications between steps.
    ///
    /// The step limit keeps this scaffold deterministic until a later scheduler
    /// owns the continuous guest loop and timer/device policy.
    pub fn run_loop(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop(self, stop_token, max_steps)
    }

    /// Run bounded vCPU steps and report each raw step outcome to `observe_step`.
    ///
    /// This keeps diagnostics at the same boundary as the internal boot loop:
    /// observers see the step that was returned by HVF before the loop performs
    /// follow-up timer or block/network-notification handling.
    pub fn run_loop_with_observer(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
        observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop_with_observer(self, stop_token, max_steps, observe_step)
    }

    pub fn dispatch_block_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_block_notifications(
                self.backend,
                devices,
                &self.block_device_metrics,
            );
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBlockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher)
                .map_err(
                    |source| HvfArm64BootBlockNotificationDispatchError::MmioDispatcher { source },
                )?;

            self.runtime_resources
                .dispatch_block_queue_notifications(memory, &mut mmio_dispatcher)
                .map_err(|source| {
                    HvfArm64BootBlockNotificationDispatchError::DispatchNotifications { source }
                })?
        };

        record_block_runtime_dispatch_metrics(&self.block_device_metrics, dispatches.as_slice());
        let result = if !dispatches.needs_queue_interrupt() {
            collect_block_notification_dispatches(dispatches)
        } else {
            match HvfGicSpiSignaler::from_metadata(&self.gic) {
                Ok(signaler) => signal_block_queue_interrupts(dispatches, &signaler),
                Err(source) => {
                    Err(HvfArm64BootBlockNotificationDispatchError::CreateSignalSink { source })
                }
            }
        };
        match &result {
            Ok(dispatches) => record_block_signal_metrics(&self.block_device_metrics, dispatches),
            Err(_) => self.block_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_pmem_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_pmem_notifications(
                self.backend,
                devices,
                &self.pmem_device_metrics,
            );
        }
        dispatch_pmem_queue_notifications_and_signal_interrupts(
            self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.pmem_device_metrics,
        )
    }

    pub fn dispatch_network_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_network_notifications(
                self.backend,
                devices,
                None,
                &self.network_interface_metrics,
            );
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_network_queue_notifications_with_packet_io_and_signal_interrupts(
        &mut self,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_network_notifications(
                self.backend,
                devices,
                Some(packet_io),
                &self.network_interface_metrics,
            );
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications_with_packet_io(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                packet_io,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_vsock_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        if self
            .pci_data_devices
            .as_ref()
            .is_some_and(|devices| devices.vsock.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootVsockNotificationDispatchError::MapGuestMemory { source }
            })?;
            if let Some(devices) = self.pci_data_devices.as_mut() {
                devices.dispatch_vsock(memory, &self.vsock_device_metrics);
            }
            return Ok(HvfArm64BootVsockNotificationDispatches::new(Vec::new()));
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootVsockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher)
                .map_err(
                    |source| HvfArm64BootVsockNotificationDispatchError::MmioDispatcher { source },
                )?;

            dispatch_vsock_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_vsock_runtime_dispatch_metrics(&self.vsock_device_metrics, dispatches.as_slice());
        let result = collect_or_signal_vsock_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => record_vsock_signal_metrics(&self.vsock_device_metrics, dispatches),
            Err(_) => self.vsock_device_metrics.record_muxer_event_failure(),
        }
        result
    }

    pub fn dispatch_balloon_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.balloon.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBalloonNotificationDispatchError::MapGuestMemory { source }
            })?;
            let _ = devices.dispatch_balloon(memory, &self.balloon_device_metrics, false);
            return Ok(HvfArm64BootBalloonNotificationDispatches::new(Vec::new()));
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBalloonNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootBalloonNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_balloon_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_balloon_runtime_dispatch_metrics(
            &self.balloon_device_metrics,
            dispatches.as_slice(),
            true,
        );
        let result = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_balloon_signal_metrics(&self.balloon_device_metrics, dispatches)
            }
            Err(_) => self.balloon_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_memory_hotplug_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        if self
            .pci_data_devices
            .as_ref()
            .is_some_and(|devices| devices.memory_hotplug.is_some())
        {
            let (memory, mut mutation_executor) = self
                .backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory { source }
                })?;
            if let Some(devices) = self.pci_data_devices.as_mut() {
                devices.dispatch_memory_hotplug(memory, &mut mutation_executor);
            }
            return Ok(HvfArm64BootMemoryHotplugNotificationDispatches::new(
                Vec::new(),
            ));
        }
        let dispatches = {
            let (memory, mut mutation_executor) = self
                .backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory { source }
                })?;
            let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_memory_hotplug_runtime_notifications_with_executor(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                &mut mutation_executor,
            )?
        };

        collect_or_signal_memory_hotplug_queue_interrupts(dispatches, &self.gic)
    }

    pub fn update_memory_hotplug_requested_size_and_signal_interrupt(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        if let Some(devices) = self
            .pci_data_devices
            .as_ref()
            .filter(|devices| devices.memory_hotplug.is_some())
        {
            return devices.update_memory_hotplug(update);
        }
        update_memory_hotplug_requested_size_and_signal_interrupt(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            &self.gic,
            update,
        )
    }

    pub fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        if let Some(devices) = self
            .pci_data_devices
            .as_ref()
            .filter(|devices| devices.memory_hotplug.is_some())
        {
            return devices.memory_hotplug_status(config, requested_size_mib);
        }
        memory_hotplug_status(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            config,
            requested_size_mib,
        )
    }

    pub fn trigger_balloon_statistics_update_and_signal_interrupts(
        &mut self,
    ) -> Result<(), BalloonUpdateError> {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.balloon.is_some())
        {
            let memory = self
                .backend
                .mapped_guest_memory_mut()
                .map_err(balloon_update_error_from_display)?;
            let result = devices.dispatch_balloon(memory, &self.balloon_device_metrics, true);
            if result.is_err() {
                self.balloon_device_metrics
                    .record_statistics_update_failure();
            }
            return result;
        }
        let result = (|| {
            let dispatches = {
                let memory = self
                    .backend
                    .mapped_guest_memory_mut()
                    .map_err(balloon_update_error_from_display)?;
                let mut mmio_dispatcher = lock_boot_mmio_dispatcher(&self.mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?;

                self.runtime_resources
                    .trigger_balloon_statistics_update(memory, &mut mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?
            };

            record_balloon_runtime_dispatch_metrics(
                &self.balloon_device_metrics,
                dispatches.as_slice(),
                false,
            );
            let dispatches = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic)
                .map_err(balloon_update_error_from_display)?;
            record_balloon_signal_metrics(&self.balloon_device_metrics, &dispatches);

            balloon_update_result_from_hvf_dispatches(&dispatches)
        })();
        if result.is_err() {
            self.balloon_device_metrics
                .record_statistics_update_failure();
        }

        result
    }

    pub fn dispatch_entropy_queue_notifications_and_signal_interrupts(
        &mut self,
        entropy_source: &mut impl Arm64BootEntropySourceProvider,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.entropy.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
            })?;
            let source = match entropy_source.pci_entropy_source() {
                Ok(source) => source,
                Err(_) => {
                    self.entropy_device_metrics
                        .record_entropy_source_provider_failure();
                    return Ok(HvfArm64BootEntropyNotificationDispatches::new(Vec::new()));
                }
            };
            let retry_after =
                devices.dispatch_entropy(memory, source.into_inner(), &self.entropy_device_metrics);
            let mut dispatches = HvfArm64BootEntropyNotificationDispatches::new(Vec::new());
            dispatches.rate_limiter_retry_after = retry_after;
            return Ok(dispatches);
        }
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            entropy_source,
        )
    }
}

impl Drop for HvfArm64BootSession<'_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn failed_snapshot_v1_restore(
    stage: HvfSnapshotV1RestoreStage,
    failure: HvfSnapshotV1RestoreFailure,
    scheduler: &mut Option<HvfArm64BootLimiterRetryWakeupScheduler>,
    runner: &mut Option<HvfVcpuRunner<'static>>,
    backend: &mut HvfBackend,
) -> HvfSnapshotV1RestoreError {
    let scheduler_failed = scheduler
        .as_mut()
        .is_some_and(|scheduler| scheduler.stop_with_result().is_err());
    drop(scheduler.take());
    let runner_error = runner.as_mut().and_then(|runner| runner.shutdown().err());
    drop(runner.take());
    let backend_error = <HvfBackend as VmBackend>::destroy_vm(backend).err();
    HvfSnapshotV1RestoreError::new(
        stage,
        failure,
        HvfSnapshotV1RestoreCleanup::new(scheduler_failed, runner_error, backend_error),
    )
}

impl OwnedHvfArm64BootSession {
    pub fn new(
        controller: &VmmController,
        config: HvfArm64BootSessionConfig,
    ) -> Result<Self, HvfArm64BootSessionError> {
        Self::new_with_boot_files(controller, config, BootSourceFiles::default())
    }

    /// Constructs a session while consuming already-opened boot payload files.
    pub fn new_with_boot_files(
        controller: &VmmController,
        config: HvfArm64BootSessionConfig,
        boot_files: BootSourceFiles,
    ) -> Result<Self, HvfArm64BootSessionError> {
        Self::new_with_startup_resources(
            controller,
            config,
            VmStartupResources::with_boot_files(boot_files),
        )
    }

    /// Constructs a session while consuming all already-opened startup files.
    pub fn new_with_startup_resources(
        controller: &VmmController,
        config: HvfArm64BootSessionConfig,
        startup_resources: VmStartupResources,
    ) -> Result<Self, HvfArm64BootSessionError> {
        let mut backend = HvfBackend::new();
        let prepared: PreparedHvfArm64BootSession<'static> = match prepare_arm64_boot_session_parts(
            &mut backend,
            controller,
            config,
            startup_resources,
        ) {
            Ok(prepared) => prepared,
            Err(err) => {
                let _ = <HvfBackend as VmBackend>::destroy_vm(&mut backend);
                return Err(err);
            }
        };

        Ok(Self {
            runner: prepared.runner,
            backend,
            mmio_dispatcher: prepared.mmio_dispatcher,
            runtime_resources: prepared.runtime_resources,
            pci_validation_endpoint: prepared.pci_validation_endpoint,
            pci_data_devices: prepared.pci_data_devices,
            cache_source: prepared.cache_source,
            cache_hierarchy: Some(prepared.cache_hierarchy),
            control_wakeup: prepared.control_wakeup,
            run_loop_wakeup: prepared.run_loop_wakeup,
            block_retry_wakeup: prepared.block_retry_wakeup,
            block_retry_wakeup_scheduler: prepared.block_retry_wakeup_scheduler,
            pmem_retry_wakeup: prepared.pmem_retry_wakeup,
            pmem_retry_wakeup_scheduler: prepared.pmem_retry_wakeup_scheduler,
            network_retry_wakeup: prepared.network_retry_wakeup,
            network_retry_wakeup_scheduler: prepared.network_retry_wakeup_scheduler,
            entropy_retry_wakeup: prepared.entropy_retry_wakeup,
            entropy_retry_wakeup_scheduler: prepared.entropy_retry_wakeup_scheduler,
            entropy_source: VirtioRngOsEntropySource::new(),
            block_device_metrics: prepared.block_device_metrics,
            pmem_device_metrics: prepared.pmem_device_metrics,
            balloon_device_metrics: prepared.balloon_device_metrics,
            network_interface_metrics: prepared.network_interface_metrics,
            vsock_device_metrics: prepared.vsock_device_metrics,
            entropy_device_metrics: prepared.entropy_device_metrics,
            gic: prepared.gic,
            block_interrupt_lines: prepared.block_interrupt_lines,
            pmem_interrupt_lines: prepared.pmem_interrupt_lines,
            network_interrupt_lines: prepared.network_interrupt_lines,
            vsock_interrupt_line: prepared.vsock_interrupt_line,
            balloon_interrupt_line: prepared.balloon_interrupt_line,
            entropy_interrupt_line: prepared.entropy_interrupt_line,
            memory_hotplug_interrupt_line: prepared.memory_hotplug_interrupt_line,
            serial_interrupt_line: prepared.serial_interrupt_line,
            vmgenid_interrupt_line: prepared.vmgenid_interrupt_line,
            vmclock_interrupt_line: prepared.vmclock_interrupt_line,
            boot_registers: prepared.boot_registers,
        })
    }

    /// Construct and completely restore one never-run native-v1 destination.
    pub fn restore_snapshot_v1(
        prepared: PreparedHvfSnapshotV1Load,
        track_dirty_pages: bool,
    ) -> Result<RestoredHvfArm64BootSession, HvfSnapshotV1RestoreError> {
        let (state, installed) = prepared.into_parts();
        let (_machine, compatibility, vcpu_state, interrupt_state, _device_state) =
            state.into_parts();
        let expected_gic = compatibility.gic_metadata();
        let primary_mpidr = compatibility.primary_mpidr();
        let cache_source = crate::vcpu_config::HvfArm64VcpuCacheFdtSource::new(
            compatibility.identification().id_aa64mmfr2_el1(),
            compatibility.cache_manifest(),
        );
        let restore_state = HvfArm64SnapshotV1Restore::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            primary_mpidr,
            vcpu_state,
            interrupt_state,
        );

        let InstalledSnapshotV1Runtime {
            mut memory,
            mmio_dispatcher,
            mut runtime_resources,
            drive_config,
            block_retry,
            serial_output,
            serial_output_buffer,
        } = installed;
        runtime_resources.machine_config = runtime_resources
            .machine_config
            .with_track_dirty_pages(track_dirty_pages);
        let block_interrupt_lines = runtime_resources
            .block_devices
            .iter()
            .map(|device| device.fdt_device.interrupt_line)
            .collect::<Vec<_>>();
        let serial_interrupt_line = runtime_resources
            .serial_device
            .as_ref()
            .map(|device| device.fdt_device.interrupt_line);
        let vmgenid_interrupt_line = runtime_resources.vmgenid_device.fdt_device.interrupt_line;
        let vmclock_interrupt_line = runtime_resources.vmclock_device.fdt_device.interrupt_line;
        let block_device_metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(
            runtime_resources
                .block_devices
                .iter()
                .map(|device| device.registration.drive_id()),
        );

        let mut backend = HvfBackend::new();
        let mut runner: Option<HvfVcpuRunner<'static>> = None;
        let mut block_retry_wakeup_scheduler = None;

        if let Err(source) = <HvfBackend as VmBackend>::create_vm(&mut backend) {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::CreateVm,
                HvfSnapshotV1RestoreFailure::Backend(source),
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }
        let gic = match backend.create_gic() {
            Ok(gic) => *gic,
            Err(source) => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::CreateGic,
                    HvfSnapshotV1RestoreFailure::Gic(source),
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        };
        if gic != expected_gic {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::ValidateGic,
                HvfSnapshotV1RestoreFailure::GicMetadataMismatch,
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }
        if track_dirty_pages && memory.enable_dirty_tracking().is_err() {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::EnableDirtyTracking,
                HvfSnapshotV1RestoreFailure::DirtyTracking,
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }
        if backend
            .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
            .is_err()
        {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::MapMemory,
                HvfSnapshotV1RestoreFailure::MemoryMapping,
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }
        if track_dirty_pages && backend.start_dirty_write_tracking().is_err() {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::EnableDirtyTracking,
                HvfSnapshotV1RestoreFailure::DirtyTracking,
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }
        match backend.start_session_vcpu_runner() {
            Ok(created) => runner = Some(created),
            Err(source) => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::StartRunner,
                    HvfSnapshotV1RestoreFailure::Runner(Box::new(source)),
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        }

        let block_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
        let run_cancel_handle = match runner.as_ref() {
            Some(runner) => runner.run_cancel_handle(),
            None => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::InvalidRuntime,
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        };
        let created_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start(
            BLOCK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            run_cancel_handle,
            block_retry_wakeup.clone(),
        );
        match created_scheduler {
            Ok(scheduler) => block_retry_wakeup_scheduler = Some(scheduler),
            Err(source) => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::StartBlockRetryScheduler,
                    HvfSnapshotV1RestoreFailure::Scheduler(source.kind()),
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        }

        let restore_result = match runner.as_ref() {
            Some(runner) => runner.restore_arm64_snapshot_v1_state(restore_state),
            None => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::InvalidRuntime,
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        };
        if let Err(source) = restore_result {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::RestoreRunnerState,
                HvfSnapshotV1RestoreFailure::Runner(Box::new(source)),
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }
        let vmgenid_result = match runner.as_ref() {
            Some(runner) => replace_vmgenid_for_snapshot_restore(
                runner,
                &mut backend,
                &mut runtime_resources,
                gic,
                vmgenid_interrupt_line,
            ),
            None => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::InvalidRuntime,
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        };
        if let Err(source) = vmgenid_result {
            return Err(failed_snapshot_v1_restore(
                HvfSnapshotV1RestoreStage::ReplaceVmGenId,
                HvfSnapshotV1RestoreFailure::VmGenId(Box::new(source)),
                &mut block_retry_wakeup_scheduler,
                &mut runner,
                &mut backend,
            ));
        }

        let retry_after = match block_retry {
            SnapshotV1BlockRetryState::None => None,
            SnapshotV1BlockRetryState::Immediate => Some(Duration::ZERO),
            SnapshotV1BlockRetryState::After { remaining_nanos } => {
                Some(Duration::from_nanos(remaining_nanos))
            }
        };
        match block_retry_wakeup_scheduler.as_ref() {
            Some(scheduler) => scheduler.schedule_after(retry_after),
            None => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::InvalidRuntime,
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        }

        let runner = match runner.take() {
            Some(runner) => runner,
            None => {
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::InvalidRuntime,
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        };
        let mut block_retry_wakeup_scheduler = match block_retry_wakeup_scheduler.take() {
            Some(scheduler) => scheduler,
            None => {
                let mut runner = Some(runner);
                return Err(failed_snapshot_v1_restore(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::InvalidRuntime,
                    &mut block_retry_wakeup_scheduler,
                    &mut runner,
                    &mut backend,
                ));
            }
        };

        let mmio_dispatcher = Arc::new(Mutex::new(mmio_dispatcher));
        let runner = match HvfArm64BootVcpuSession::from_restored_runner(
            runner,
            primary_mpidr,
            Arc::clone(&mmio_dispatcher),
            gic.timer_interrupts.el1_virtual_timer_intid,
        ) {
            Ok(runner) => runner,
            Err(source) => {
                let scheduler_failed = block_retry_wakeup_scheduler.stop_with_result().is_err();
                let backend_error = <HvfBackend as VmBackend>::destroy_vm(&mut backend).err();
                return Err(HvfSnapshotV1RestoreError::new(
                    HvfSnapshotV1RestoreStage::AssembleSession,
                    HvfSnapshotV1RestoreFailure::Coordinator(Box::new(source)),
                    HvfSnapshotV1RestoreCleanup::new(scheduler_failed, None, backend_error),
                ));
            }
        };
        let entropy_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
        let pmem_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
        let network_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
        let session = Self {
            runner,
            backend,
            mmio_dispatcher,
            runtime_resources,
            pci_validation_endpoint: None,
            pci_data_devices: None,
            cache_source,
            cache_hierarchy: None,
            control_wakeup: HvfArm64BootRunLoopControlWakeupToken::default(),
            run_loop_wakeup: HvfArm64BootRunLoopWakeupToken::default(),
            block_retry_wakeup,
            block_retry_wakeup_scheduler,
            pmem_retry_wakeup,
            pmem_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler::inactive(),
            network_retry_wakeup,
            network_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler::inactive(),
            entropy_retry_wakeup,
            entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler::inactive(),
            entropy_source: VirtioRngOsEntropySource::new(),
            block_device_metrics,
            pmem_device_metrics: SharedPmemDeviceMetricsRegistry::default(),
            balloon_device_metrics: SharedBalloonDeviceMetrics::default(),
            network_interface_metrics: SharedNetworkInterfaceMetricsRegistry::default(),
            vsock_device_metrics: SharedVsockDeviceMetrics::default(),
            entropy_device_metrics: SharedEntropyDeviceMetrics::default(),
            gic,
            block_interrupt_lines,
            pmem_interrupt_lines: Vec::new(),
            network_interrupt_lines: Vec::new(),
            vsock_interrupt_line: None,
            balloon_interrupt_line: None,
            entropy_interrupt_line: None,
            memory_hotplug_interrupt_line: None,
            serial_interrupt_line,
            vmgenid_interrupt_line,
            vmclock_interrupt_line,
            boot_registers: None,
        };

        Ok(RestoredHvfArm64BootSession {
            session,
            drive_config,
            serial_output,
            serial_output_buffer,
        })
    }

    /// Quiesce block, pmem, network, and entropy limiter retry wakeup publication.
    pub fn quiesce_limiter_retry_wakeups(
        &self,
    ) -> Result<
        HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        HvfArm64BootLimiterRetryWakeupQuiescenceError,
    > {
        quiesce_limiter_retry_wakeups(
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.block_retry_wakeup,
                &self.block_retry_wakeup_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.pmem_retry_wakeup,
                &self.pmem_retry_wakeup_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.network_retry_wakeup,
                &self.network_retry_wakeup_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &self.entropy_retry_wakeup,
                &self.entropy_retry_wakeup_scheduler,
            ),
        )
    }

    pub fn capture_snapshot_v1_device_state_at(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
    ) -> Result<SnapshotV1DeviceState, HvfArm64BootSnapshotV1DeviceCaptureError> {
        HvfArm64BootSnapshotV1DeviceCaptureOwners {
            backend: &self.backend,
            mmio_dispatcher: &self.mmio_dispatcher,
            runtime_resources: &self.runtime_resources,
            block_retry_wakeup_scheduler: &self.block_retry_wakeup_scheduler,
        }
        .capture_at(drive_config, serial_config, guard, now)
    }

    /// Capture and preflight every non-memory native-v1 component while the
    /// caller retains the supervisor and auxiliary quiescence guards.
    pub fn capture_snapshot_v1_state_at(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
    ) -> Result<HvfSnapshotV1State, HvfArm64BootSnapshotV1StateCaptureError> {
        self.capture_snapshot_v1_state_at_with_cancel(
            drive_config,
            serial_config,
            guard,
            now,
            |_| false,
        )
    }

    pub fn capture_snapshot_v1_state_at_with_cancel(
        &self,
        drive_config: &DriveConfig,
        serial_config: &SerialConfig,
        guard: &HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        now: Instant,
        mut is_cancelled: impl FnMut(HvfArm64BootSnapshotV1CaptureStage) -> bool,
    ) -> Result<HvfSnapshotV1State, HvfArm64BootSnapshotV1StateCaptureError> {
        if self.runner.member_count() != 1 {
            return Err(
                HvfArm64BootSnapshotV1StateCaptureError::UnsupportedVcpuCount {
                    vcpu_count: self.runner.member_count(),
                },
            );
        }
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::CacheManifest,
        )?;
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::Runner,
        )?;
        let runner_capture = self
            .runner
            .capture_arm64_snapshot_v1_state()
            .map_err(|source| HvfArm64BootSnapshotV1StateCaptureError::Runner { source })?;
        let cache_manifest = retained_snapshot_cache_manifest(
            self.cache_source,
            runner_capture.identification().id_aa64mmfr2_el1(),
        )?;
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::Device,
        )?;
        let device = self
            .capture_snapshot_v1_device_state_at(drive_config, serial_config, guard, now)
            .map_err(|source| HvfArm64BootSnapshotV1StateCaptureError::Device { source })?;
        check_snapshot_v1_capture_cancelled(
            &mut is_cancelled,
            HvfArm64BootSnapshotV1CaptureStage::EncodePreflight,
        )?;
        build_snapshot_v1_state(
            &self.runtime_resources,
            self.gic,
            self.primary_mpidr(),
            cache_manifest,
            runner_capture,
            device,
        )
    }

    /// Requests an immediate run-loop retry after a live pmem limiter update
    /// unblocks work that was already pending on a virtqueue.
    pub fn schedule_pmem_retry_wakeup_after_live_update(&mut self, has_pending_work: bool) {
        if has_pending_work {
            self.pmem_retry_wakeup_scheduler
                .schedule_after(Some(Duration::ZERO));
        }
    }

    pub fn shutdown(&mut self) -> Result<(), HvfArm64BootSessionShutdownError> {
        self.block_retry_wakeup_scheduler.stop();
        self.pmem_retry_wakeup_scheduler.stop();
        self.network_retry_wakeup_scheduler.stop();
        self.entropy_retry_wakeup_scheduler.stop();
        let pci_data_result = self.teardown_pci_data_devices();
        let pci_result = self.teardown_pci_validation_endpoint();
        let runner_result = self.runner.shutdown();
        if let Err(source) = runner_result {
            return Err(HvfArm64BootSessionShutdownError::Vcpu { source });
        }
        if let Err(source) = pci_data_result {
            return Err(HvfArm64BootSessionShutdownError::PciData { source });
        }
        if let Err(source) = pci_result {
            return Err(HvfArm64BootSessionShutdownError::PciValidation { source });
        }
        <HvfBackend as VmBackend>::destroy_vm(&mut self.backend)
            .map_err(|source| HvfArm64BootSessionShutdownError::DestroyVm { source })
    }

    /// Explicit cleanup evidence for an uncommitted restored destination.
    pub fn teardown_snapshot_v1(&mut self) -> HvfSnapshotV1RestoreCleanup {
        let scheduler_failed = self
            .block_retry_wakeup_scheduler
            .stop_with_result()
            .is_err()
            | self.pmem_retry_wakeup_scheduler.stop_with_result().is_err()
            | self
                .network_retry_wakeup_scheduler
                .stop_with_result()
                .is_err()
            | self
                .entropy_retry_wakeup_scheduler
                .stop_with_result()
                .is_err();
        let coordinator = self.runner.shutdown().err();
        let backend = <HvfBackend as VmBackend>::destroy_vm(&mut self.backend).err();
        HvfSnapshotV1RestoreCleanup::with_coordinator(scheduler_failed, coordinator, backend)
    }

    pub const fn gic_metadata(&self) -> HvfGicMetadata {
        self.gic
    }

    /// Return the send-only capability retained by an MSI-enabled session.
    pub fn gic_msi_signaler(&self) -> Option<&HvfGicMsiSignaler> {
        self.backend.gic_msi_signaler()
    }

    /// Dispatches the internal modern virtio-pci validation device, if present.
    #[doc(hidden)]
    pub fn dispatch_pci_validation_notifications(
        &mut self,
    ) -> Result<(), HvfArm64BootPciValidationError> {
        dispatch_pci_validation_notifications(
            &mut self.backend,
            self.pci_validation_endpoint.as_mut(),
        )
    }

    /// Triggers the validation device's configuration vector exactly once.
    #[doc(hidden)]
    pub fn trigger_pci_validation_config_interrupt(
        &mut self,
    ) -> Result<(), HvfArm64BootPciValidationError> {
        trigger_pci_validation_config_interrupt(self.pci_validation_endpoint.as_mut())
    }

    /// Returns value-redacted state from the internal modern PCI proof.
    #[doc(hidden)]
    pub fn pci_validation_diagnostics(
        &self,
    ) -> Option<Result<HvfArm64BootPciValidationDiagnostics, HvfArm64BootPciValidationError>> {
        self.pci_validation_endpoint
            .as_ref()
            .map(HvfArm64BootPciValidationEndpoint::diagnostics)
    }

    /// Returns value-redacted state from the hidden PCI data-device proof.
    #[doc(hidden)]
    pub fn pci_data_device_diagnostics(
        &self,
    ) -> Option<Result<Vec<HvfArm64BootPciDataDeviceDiagnostics>, HvfArm64BootPciDataError>> {
        self.pci_data_devices
            .as_ref()
            .map(HvfArm64BootPciDataDevices::diagnostics)
    }

    /// Tears down the internal endpoint and proves released capacity is reusable.
    #[doc(hidden)]
    pub fn teardown_pci_validation_endpoint(
        &mut self,
    ) -> Result<
        Option<HvfArm64BootPciValidationTeardownEvidence>,
        HvfArm64BootPciValidationTeardownError,
    > {
        let signaler = self.backend.gic_msi_signaler().cloned();
        teardown_pci_validation_endpoint_and_verify_reuse(
            &mut self.pci_validation_endpoint,
            &mut self.runtime_resources,
            &self.mmio_dispatcher,
            signaler,
        )
    }

    /// Explicitly tears down every hidden PCI data endpoint in reverse order.
    #[doc(hidden)]
    pub fn teardown_pci_data_devices(&mut self) -> Result<(), HvfArm64BootPciDataError> {
        teardown_pci_data_devices(&mut self.pci_data_devices)
    }

    pub fn primary_mpidr(&self) -> u64 {
        self.runner.primary_mpidr()
    }

    pub fn vcpu_count(&self) -> usize {
        self.runner.member_count()
    }

    pub fn vcpu_mpidrs(&self) -> &[u64] {
        self.runner.mpidrs()
    }

    pub fn last_vcpu_terminal_report(&self) -> Option<&HvfVcpuRunTerminalReport> {
        self.runner.last_terminal_report()
    }

    pub fn runtime_resources(&self) -> &Arm64BootRuntimeResources {
        &self.runtime_resources
    }

    pub fn pci_balloon_device_updater(&self) -> Option<HvfArm64BootPciBalloonDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::balloon_updater)
    }

    pub fn pci_block_device_updater(&self) -> Option<HvfArm64BootPciBlockDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::block_updater)
    }

    /// Reports whether block operations belong to the retained PCI inventory.
    pub const fn uses_pci_data_devices(&self) -> bool {
        self.pci_data_devices.is_some()
    }

    /// Checks every owner-side prerequisite for a vhost-user runtime insert
    /// without reserving capacity or cloning shared-memory descriptors.
    pub fn preflight_runtime_vhost_user_block_device(
        &self,
        drive_id: &str,
    ) -> Result<(), DriveRuntimeMutationError> {
        let memory = self.guest_memory().map_err(|source| {
            DriveRuntimeMutationError::ActiveSessionCommand {
                message: format!("guest memory is unavailable: {source}"),
            }
        })?;
        if memory.backing() != GuestMemoryBacking::Shared {
            return Err(DriveRuntimeMutationError::PrepareDevice {
                message: "vhost-user block requires shared guest memory".to_string(),
            });
        }
        PreparedBlockDevice::preflight_vhost_user_memory(memory).map_err(|source| {
            DriveRuntimeMutationError::PrepareDevice {
                message: source.to_string(),
            }
        })?;
        self.pci_data_devices
            .as_ref()
            .ok_or(DriveRuntimeMutationError::PciNotEnabled)?
            .preflight_runtime_block(drive_id, &self.block_device_metrics)
    }

    /// Materializes one closed runtime resource on the owner and publishes it
    /// through the generic PCI transaction.
    pub fn insert_runtime_block_resource(
        &mut self,
        resource: RuntimeBlockDeviceResource,
    ) -> Result<(), DriveRuntimeMutationError> {
        let prepared = match resource {
            RuntimeBlockDeviceResource::Prepared(prepared) => prepared,
            RuntimeBlockDeviceResource::VhostUser { config, frontend } => {
                self.preflight_runtime_vhost_user_block_device(config.drive_id())?;
                PreparedBlockDevice::from_config_with_vhost_user(
                    &config,
                    frontend,
                    self.guest_memory().map_err(|source| {
                        DriveRuntimeMutationError::ActiveSessionCommand {
                            message: format!("guest memory is unavailable: {source}"),
                        }
                    })?,
                )
                .map_err(|source| DriveRuntimeMutationError::PrepareDevice {
                    message: source.to_string(),
                })?
            }
        };
        self.insert_runtime_block_device(prepared)
    }

    /// Publishes one fully prepared block endpoint into the owner-thread PCI inventory.
    pub fn insert_runtime_block_device(
        &mut self,
        prepared: PreparedBlockDevice,
    ) -> Result<(), DriveRuntimeMutationError> {
        let metrics = self.block_device_metrics.clone();
        self.pci_data_devices
            .as_mut()
            .ok_or(DriveRuntimeMutationError::PciNotEnabled)?
            .insert_runtime_block(prepared, &metrics)
    }

    /// Updates one entry by resolving it from the current owner-thread inventory.
    pub fn update_runtime_block_device_with_opened(
        &mut self,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        self.pci_data_devices
            .as_ref()
            .ok_or(DriveUpdateError::ActiveSessionUnavailable)?
            .update_runtime_block(config, backing, rate_limiter_update)
    }

    /// Refreshes one active direct vhost-user block configuration through its
    /// selected MMIO or PCI transport and delivers the guest notification.
    pub fn refresh_vhost_user_block_config(
        &mut self,
        config: &DriveConfig,
    ) -> Result<(), DriveUpdateError> {
        if !config.is_vhost_user() {
            return Err(DriveUpdateError::UnsupportedBackend);
        }
        if let Some(devices) = self.pci_data_devices.as_ref() {
            return devices.update_runtime_block(config, None, None);
        }
        refresh_mmio_vhost_user_block_config(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            &self.gic,
            config,
        )
    }

    /// Removes one non-root block endpoint from the owner-thread PCI inventory.
    pub fn remove_runtime_block_device(
        &mut self,
        drive_id: &str,
    ) -> Result<(), DriveRuntimeMutationError> {
        remove_runtime_pci_block_device_and_refresh_retry(
            &mut self.pci_data_devices,
            &self.block_retry_wakeup,
            &self.block_retry_wakeup_scheduler,
            drive_id,
        )
    }

    /// Maps and publishes one pmem endpoint in the owner-thread PCI inventory.
    pub fn insert_runtime_pmem_device(
        &mut self,
        config: &PmemConfig,
        backing: PmemFileBacking,
    ) -> Result<(), PmemRuntimeMutationError> {
        let metrics = self.pmem_device_metrics.clone();
        self.pci_data_devices
            .as_mut()
            .ok_or(PmemRuntimeMutationError::PciNotEnabled)?
            .insert_runtime_pmem(
                &mut self.backend,
                &mut self.runtime_resources.pmem_devices,
                config,
                backing,
                &metrics,
            )
    }

    /// Updates a pmem endpoint by resolving its current live owner entry.
    pub fn update_runtime_pmem_device(
        &mut self,
        update: &PmemUpdate,
    ) -> Result<bool, PmemUpdateError> {
        self.pci_data_devices
            .as_ref()
            .ok_or(PmemUpdateError::ActiveSessionUnavailable)?
            .update_runtime_pmem(update)
    }

    /// Flushes and removes one pmem endpoint from the owner-thread inventory.
    pub fn remove_runtime_pmem_device(
        &mut self,
        pmem_id: &str,
    ) -> Result<(), PmemRuntimeMutationError> {
        remove_runtime_pci_pmem_device_and_refresh_retry(
            &mut self.pci_data_devices,
            &mut self.backend,
            &mut self.runtime_resources.pmem_devices,
            &self.pmem_retry_wakeup,
            &self.pmem_retry_wakeup_scheduler,
            pmem_id,
        )
    }

    /// Publishes one prepared network endpoint into the owner-thread PCI inventory.
    pub fn insert_runtime_network_device(
        &mut self,
        prepared: PreparedNetworkDevice,
    ) -> Result<(), NetworkRuntimeMutationError> {
        let metrics = self.network_interface_metrics.clone();
        self.pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?
            .insert_runtime_network(prepared, &metrics)
    }

    /// Updates one network entry by resolving it from the current live inventory.
    pub fn update_runtime_network_interface(
        &mut self,
        update: &NetworkInterfaceUpdate,
    ) -> Result<(), NetworkInterfaceUpdateError> {
        self.pci_data_devices
            .as_ref()
            .ok_or(NetworkInterfaceUpdateError::ActiveSessionUnavailable)?
            .update_runtime_network(update)
    }

    /// Makes one network endpoint reversibly unreachable before packet-I/O teardown.
    pub fn prepare_runtime_network_device_removal(
        &mut self,
        iface_id: &str,
    ) -> Result<PreparedHvfArm64BootPciNetworkRemoval, NetworkRuntimeMutationError> {
        self.pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?
            .prepare_runtime_network_removal(iface_id)
    }

    /// Restores one reversibly removed network endpoint.
    pub fn rollback_runtime_network_device_removal(
        &mut self,
        prepared: PreparedHvfArm64BootPciNetworkRemoval,
    ) -> Result<(), NetworkRuntimeMutationError> {
        self.pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?
            .rollback_runtime_network_removal(prepared)
    }

    /// Commits endpoint teardown after packet-I/O cleanup succeeds.
    pub fn commit_runtime_network_device_removal(
        &mut self,
        prepared: PreparedHvfArm64BootPciNetworkRemoval,
    ) -> Result<(), NetworkRuntimeMutationError> {
        let devices = self
            .pci_data_devices
            .as_mut()
            .ok_or(NetworkRuntimeMutationError::PciNotEnabled)?;
        devices.commit_runtime_network_removal(prepared)?;
        refresh_network_retry_wakeup_after_inventory_change(
            &self.network_retry_wakeup,
            &self.network_retry_wakeup_scheduler,
            devices.network_retry_deadline(),
        );
        Ok(())
    }

    pub fn pci_network_device_updater(&self) -> Option<HvfArm64BootPciNetworkDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::network_updater)
    }

    pub fn pci_pmem_device_updater(&self) -> Option<HvfArm64BootPciPmemDeviceUpdater> {
        self.pci_data_devices
            .as_ref()
            .and_then(HvfArm64BootPciDataDevices::pmem_updater)
    }

    /// Return the exact validated cache presentation selected before VM creation.
    pub fn arm64_fdt_cache_hierarchy(&self) -> Option<&Arm64FdtCacheHierarchy> {
        self.cache_hierarchy.as_ref()
    }

    pub fn shared_balloon_device_metrics(&self) -> SharedBalloonDeviceMetrics {
        self.balloon_device_metrics.clone()
    }

    pub fn shared_block_device_metrics(&self) -> SharedBlockDeviceMetricsRegistry {
        self.block_device_metrics.clone()
    }

    pub fn shared_pmem_device_metrics(&self) -> SharedPmemDeviceMetricsRegistry {
        self.pmem_device_metrics.clone()
    }

    pub fn shared_network_interface_metrics(&self) -> SharedNetworkInterfaceMetricsRegistry {
        self.network_interface_metrics.clone()
    }

    pub fn shared_vsock_device_metrics(&self) -> SharedVsockDeviceMetrics {
        self.vsock_device_metrics.clone()
    }

    pub fn shared_entropy_device_metrics(&self) -> SharedEntropyDeviceMetrics {
        self.entropy_device_metrics.clone()
    }

    pub fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        self.runtime_resources
            .rtc_device
            .as_ref()
            .map(|device| device.metrics.clone())
    }

    /// Return a cloned handle to the runner-compatible MMIO dispatcher.
    ///
    /// The dispatcher is local to this boot session. It is shared only so
    /// vCPU-runner commands can dispatch MMIO on the runner thread. Keep cloned
    /// handles scoped to runner commands so dispatcher-owned device resources
    /// are released with the session.
    pub fn mmio_dispatcher(&self) -> Arc<Mutex<MmioDispatcher>> {
        Arc::clone(&self.mmio_dispatcher)
    }

    /// Borrow the guest memory mapped for this prepared boot session.
    ///
    /// This is startup preparation plumbing; it does not enter a continuous
    /// guest run loop or prove guest boot.
    pub fn guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory()
    }

    /// Mutably borrow the guest memory mapped for this prepared boot session.
    ///
    /// The HVF backend remains the mapping owner, so shutdown and drop still
    /// unmap the memory through the backend.
    pub fn guest_memory_mut(&mut self) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory_mut()
    }

    /// Advance one complete dirty generation under snapshot-ready quiescence.
    pub fn reset_dirty_epoch_quiesced(
        &mut self,
    ) -> Result<Option<u64>, HvfDirtyWriteEpochResetError> {
        if !self.runtime_resources.machine_config.track_dirty_pages() {
            return Ok(None);
        }
        self.backend
            .reset_dirty_epoch_quiesced()?
            .ok_or(HvfDirtyWriteEpochResetError::InvalidState(
                "tracked boot session has no active dirty tracker",
            ))
            .map(Some)
    }

    pub fn block_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.block_interrupt_lines
    }

    pub fn pmem_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.pmem_interrupt_lines
    }

    pub fn network_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.network_interrupt_lines
    }

    pub const fn vsock_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.vsock_interrupt_line
    }

    pub const fn balloon_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.balloon_interrupt_line
    }

    pub const fn entropy_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.entropy_interrupt_line
    }

    pub const fn memory_hotplug_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.memory_hotplug_interrupt_line
    }

    pub const fn serial_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.serial_interrupt_line
    }

    pub const fn vmgenid_interrupt_line(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt_line
    }

    pub const fn vmclock_interrupt_line(&self) -> GuestInterruptLine {
        self.vmclock_interrupt_line
    }

    pub const fn boot_registers(&self) -> Option<HvfArm64BootRegisters> {
        self.boot_registers
    }

    /// Capture X0-X30, PC, and CPSR on the primary vCPU's owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It does not
    /// establish snapshot readiness or return complete restorable vCPU state.
    pub fn capture_arm64_general_register_state(
        &self,
    ) -> Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_general_register_state()
    }

    /// Restore X0-X30, PC, and CPSR on the primary vCPU's owner thread.
    ///
    /// Writes are ordered but nontransactional. If one setter fails, retry the
    /// complete typed state or discard the session before guest execution.
    /// Snapshot schema, validation, and wider-state ordering remain outside
    /// this runner primitive.
    pub fn restore_arm64_general_register_state(
        &self,
        state: &HvfArm64VcpuGeneralRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_general_register_state(state)
    }

    /// Capture raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It omits
    /// the wider system-register, SIMD/FP, interrupt, and restore inventories.
    pub fn capture_arm64_core_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_core_system_register_state()
    }

    /// Restore SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 on the owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Schema, validation, and wider ordering remain outside
    /// this runner primitive.
    pub fn restore_arm64_core_system_register_state(
        &self,
        state: &HvfArm64VcpuCoreSystemRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_core_system_register_state(state)
    }

    /// Capture raw EL1 exception-register state on the primary owner thread.
    ///
    /// This subset has a paired low-level restore but omits vector-table
    /// memory, semantic validation, persistence, schema, and wider ordering.
    pub fn capture_arm64_exception_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_exception_register_state()
    }

    /// Restore raw EL1 exception-register state on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Vector memory, validation, schema, and wider ordering
    /// remain outside this runner primitive.
    pub fn restore_arm64_exception_register_state(
        &self,
        state: &HvfArm64VcpuExceptionRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_exception_register_state(state)
    }

    /// Capture raw EL1 ACTLR and CPACR controls on the primary owner thread.
    ///
    /// Complete capture requires macOS 15 and has a paired low-level restore,
    /// but omits feature validation, persistence, and transition ordering.
    pub fn capture_arm64_execution_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_execution_control_register_state()
    }

    /// Restore raw EL1 ACTLR and CPACR controls on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Feature validation, guest ISB transitions, schema, and
    /// wider ordering remain outside this runner primitive.
    pub fn restore_arm64_execution_control_register_state(
        &self,
        state: &HvfArm64VcpuExecutionControlRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_execution_control_register_state(state)
    }

    /// Capture raw EL1 CSSELR cache-size selection state on the primary owner
    /// thread.
    ///
    /// This value is not cache topology. Its complete typed value has a paired
    /// low-level restore, but omits an atomic feature/geometry manifest,
    /// selector and destination validation, synchronization, maintenance,
    /// persistence, schema, and portable restore policy.
    pub fn capture_arm64_cache_selection_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCacheSelectionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_cache_selection_register_state()
    }

    /// Restore raw EL1 CSSELR cache-size selection state on the primary owner
    /// thread.
    ///
    /// The one write is nontransactional. If it fails, retry the complete typed
    /// state or discard the session before guest execution. Selector
    /// interpretation/validation, an atomic destination cache manifest, ISB
    /// and dependent CCSIDR ordering, maintenance, persistence, rollback,
    /// schema, and snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_cache_selection_register_state(
        &self,
        state: &HvfArm64VcpuCacheSelectionRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_cache_selection_register_state(state)
    }

    /// Capture every implemented raw EL1 hardware-breakpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest addresses or identities. This
    /// getter-only subset omits watchpoints, HVF trap state, persistence,
    /// breakpoint activation, and a safe restore sequence.
    pub fn capture_arm64_breakpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuBreakpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_breakpoint_register_state()
    }

    /// Capture every implemented raw EL1 hardware-watchpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest data addresses. This getter-only
    /// subset omits HVF trap state, persistence, watchpoint activation, and a
    /// safe restore sequence.
    pub fn capture_arm64_watchpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuWatchpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_watchpoint_register_state()
    }

    /// Capture raw EL1 MDCCINT and MDSCR debug controls on the primary owner
    /// thread.
    ///
    /// Capture reads this incomplete guest debug subset only and excludes the
    /// separately captured comparator and host trap-policy state.
    pub fn capture_arm64_debug_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_control_register_state()
    }

    /// Restore raw EL1 MDCCINT and MDSCR controls on the primary owner thread.
    ///
    /// The two writes are ordered and nontransactional. Retry the complete
    /// typed state or discard the vCPU after failure. This primitive excludes
    /// feature/writable-bit validation, comparator/trap coordination,
    /// persistence, wider debug ordering, and public snapshot-load behavior.
    pub fn restore_arm64_debug_control_register_state(
        &self,
        state: &HvfArm64VcpuDebugControlRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_debug_control_register_state(state)
    }

    /// Capture Hypervisor.framework debug-trap policy on the primary owner thread.
    ///
    /// Capture reads host policy only. It excludes guest debug-register
    /// contents, persistence, activation, and wider restore policy.
    pub fn capture_arm64_debug_trap_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugTrapState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_trap_state()
    }

    /// Restore Hypervisor.framework debug-trap policy on the primary owner thread.
    ///
    /// The two policy writes are ordered and nontransactional. Retry the
    /// complete typed state or discard the vCPU after failure. This primitive
    /// excludes guest debug-register restore, validation, persistence, wider
    /// debug ordering, and public snapshot-load behavior.
    pub fn restore_arm64_debug_trap_state(
        &self,
        state: &HvfArm64VcpuDebugTrapState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_debug_trap_state(state)
    }

    /// Capture guest-visible arm64 identification registers on the primary
    /// owner thread.
    ///
    /// These raw virtual-CPU/HVF compatibility inputs define no destination
    /// policy, persistence format, or mutable restore state.
    pub fn capture_arm64_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_identification_register_state()
    }

    /// Capture optional SVE/SME identification metadata on the primary owner thread.
    ///
    /// These macOS 15.2+ ZFR0/SMFR0 compatibility inputs exclude feature masks,
    /// destination policy, SVE/SME execution state, persistence, and restore.
    pub fn capture_arm64_sve_sme_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSveSmeIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner
            .capture_arm64_sve_sme_identification_register_state()
    }

    /// Capture mutable SME PSTATE on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value contains `PSTATE.SM` and
    /// `PSTATE.ZA`. It excludes SVE/SME register contents, setters,
    /// persistence, snapshot schema, and restore ordering.
    pub fn capture_arm64_sme_pstate(&self) -> Result<HvfArm64VcpuSmePstate, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_pstate()
    }

    /// Capture all streaming SVE P registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.SM`, preserves every
    /// maximum-SVL/8 predicate byte, and redacts `Debug`. It excludes Z/ZA/ZT0,
    /// setters, persistence, snapshot schema, and restore ordering.
    pub fn capture_arm64_sme_p_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmePRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_p_register_state()
    }

    /// Capture all streaming SVE Z registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.SM`, preserves every
    /// maximum-SVL byte, and redacts `Debug`. It excludes P/ZA/ZT0, setters,
    /// persistence, snapshot schema, and restore ordering.
    pub fn capture_arm64_sme_z_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeZRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_z_register_state()
    }

    /// Capture the complete SME ZA matrix on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.ZA` but not
    /// `PSTATE.SM`, preserves the maximum-SVL square, and redacts `Debug`. It
    /// excludes Z/P/ZT0, setters, layout interpretation, persistence, snapshot
    /// schema, and restore ordering.
    pub fn capture_arm64_sme_za_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeZaRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_za_register_state()
    }

    /// Capture the fixed 64-byte SME2 ZT0 register on the primary owner thread.
    ///
    /// This macOS 15.2+ getter-only value requires `PSTATE.ZA` but not
    /// `PSTATE.SM`, is independent of maximum SVL, and redacts `Debug`. It
    /// excludes Z/P/ZA, setters, lane interpretation, persistence, snapshot
    /// schema, and restore ordering.
    pub fn capture_arm64_sme_zt0_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeZt0RegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_zt0_register_state()
    }

    /// Capture raw SME system registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ getter-only value contains `SMCR_EL1`, `SMPRI_EL1`,
    /// and `TPIDR2_EL0`; `Debug` redacts all three. It excludes maximum SVL,
    /// SVE/SME data, setters, persistence, schema, and restore ordering.
    pub fn capture_arm64_sme_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSmeSystemRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_sme_system_register_state()
    }

    /// Capture raw system-context registers on the primary vCPU owner thread.
    ///
    /// This macOS 15.2+ value contains `SCXTNUM_EL0` and `SCXTNUM_EL1`;
    /// `Debug` redacts both guest software context numbers. The complete typed
    /// value has a paired low-level restore, but interpretation, validation,
    /// persistence, schema, and wider-context ordering remain outside it.
    pub fn capture_arm64_system_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSystemContextRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_system_context_register_state()
    }

    /// Restore raw system-context registers on the primary vCPU owner thread.
    ///
    /// The EL0-then-EL1 writes are nontransactional. If a setter fails, retry
    /// the complete redacted typed state or discard the session before guest
    /// execution. Interpretation, feature/destination validation, protected
    /// persistence, TPIDR/TPIDR2/CONTEXTIDR ordering, rollback, schema, and
    /// snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_system_context_register_state(
        &self,
        state: &HvfArm64VcpuSystemContextRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_system_context_register_state(state)
    }

    /// Capture raw EL1 translation-register state on the primary owner thread.
    ///
    /// `SCXTNUM_EL0`/`SCXTNUM_EL1` context is captured separately. This subset
    /// has a paired low-level capture-order restore, but omits table memory,
    /// feature validation, TLB/cache maintenance, persistence, and a safe MMU
    /// transition sequence.
    pub fn capture_arm64_translation_register_state(
        &self,
    ) -> Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_translation_register_state()
    }

    /// Restore raw EL1 translation-register state on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Page tables, validation, barriers/maintenance, safe
    /// MMU transition ordering, and schema remain outside this primitive.
    pub fn restore_arm64_translation_register_state(
        &self,
        state: &HvfArm64VcpuTranslationRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_translation_register_state(state)
    }

    /// Capture the five raw EL1 pointer-authentication keys on the primary
    /// owner thread.
    ///
    /// Debug formatting is redacted. The complete typed value has a paired
    /// low-level restore, but feature validation, persistence protection, and
    /// safe SCTLR enable ordering remain outside this subset.
    pub fn capture_arm64_pointer_authentication_key_state(
        &self,
    ) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pointer_authentication_key_state()
    }

    /// Restore the five raw EL1 pointer-authentication keys on the primary
    /// owner thread.
    ///
    /// Ten low/high writes follow capture order but are nontransactional. If a
    /// setter fails, retry the complete redacted typed state or discard the
    /// session before guest execution. Feature/destination validation,
    /// zeroization, protected persistence, SCTLR ordering, rollback, schema,
    /// and snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_pointer_authentication_key_state(
        &self,
        state: &HvfArm64VcpuPointerAuthenticationKeyState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_pointer_authentication_key_state(state)
    }

    /// Capture raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// These sensitive software thread-ID values can contain guest pointers.
    /// `TPIDR2_EL0` is captured separately with SME system registers, while
    /// `SCXTNUM_EL0`/`SCXTNUM_EL1` use a separate system-context value. The
    /// complete typed value has a paired low-level restore, but wider context,
    /// validation, and snapshot policy remain outside this subset.
    pub fn capture_arm64_thread_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_thread_context_register_state()
    }

    /// Restore raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. Pointer validation, wider context ordering, and schema
    /// remain outside this runner primitive.
    pub fn restore_arm64_thread_context_register_state(
        &self,
        state: &HvfArm64VcpuThreadContextRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .restore_arm64_thread_context_register_state(state)
    }

    /// Capture raw Q0-Q31, FPCR, and FPSR values on the primary vCPU owner
    /// thread.
    ///
    /// This baseline SIMD/FP view has a paired low-level capture-order restore,
    /// but omits SVE/SME state, alias ordering, validation, and the wider
    /// snapshot inventory.
    pub fn capture_arm64_simd_fp_state(
        &self,
    ) -> Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_simd_fp_state()
    }

    /// Restore raw Q0-Q31, FPCR, and FPSR values on the primary owner thread.
    ///
    /// Writes follow capture order but are nontransactional. If one setter
    /// fails, retry the complete typed state or discard the session before
    /// guest execution. SVE/SME alias ordering, validation, rollback, schema,
    /// and snapshot orchestration remain outside this primitive.
    pub fn restore_arm64_simd_fp_state(
        &self,
        state: &HvfArm64VcpuSimdFpState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_simd_fp_state(state)
    }

    /// Capture raw EL1 physical-timer state on the primary owner thread.
    ///
    /// CNTP capture requires macOS 15 and a GIC created before the vCPU. The
    /// absolute CVAL and time-sensitive relative TVAL observations are read
    /// sequentially and have no portable restore-time adjustment policy.
    pub fn capture_arm64_physical_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_physical_timer_state()
    }

    /// Capture raw virtual-timer mask, offset, control, and compare state on the
    /// primary vCPU owner thread.
    ///
    /// This subset omits pending interrupt and GIC state, and the host-time
    /// adjustment required by a restore policy. The control status bit is
    /// derived and may change as virtual time advances.
    pub fn capture_arm64_virtual_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_virtual_timer_state()
    }

    /// Capture normalized physical and virtual timers for native-HVF restore.
    pub fn capture_arm64_snapshot_timer_state(
        &self,
    ) -> Result<HvfArm64SnapshotTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_snapshot_timer_state()
    }

    /// Restore normalized timers before this session's first vCPU run.
    pub fn restore_arm64_snapshot_timer_state(
        &mut self,
        state: HvfArm64SnapshotTimerState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_snapshot_timer_state(state)
    }

    /// Replace the retained VMGenID and inject its edge-rising SPI.
    ///
    /// Runner and signaler capabilities are preflighted before guest memory is
    /// changed. A signal failure is reported after memory and host metadata
    /// have committed; retry the complete method or discard the unrun session.
    pub fn replace_vmgenid_for_snapshot_restore(
        &mut self,
    ) -> Result<(), HvfArm64BootVmGenIdRestoreError> {
        replace_vmgenid_for_snapshot_restore(
            &self.runner,
            &mut self.backend,
            &mut self.runtime_resources,
            self.gic,
            self.vmgenid_interrupt_line,
        )
    }

    /// Capture CPU-level IRQ/FIQ pending state on the primary vCPU owner thread.
    ///
    /// HVF clears these per-run injection levels after a vCPU run returns.
    /// This subset has a paired low-level restore but excludes GIC/device state,
    /// delivery policy, automatic per-run reassertion, and snapshot orchestration.
    pub fn capture_arm64_pending_interrupt_state(
        &self,
    ) -> Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pending_interrupt_state()
    }

    /// Restore CPU-level IRQ/FIQ pending state on the primary vCPU owner thread.
    ///
    /// The IRQ-then-FIQ writes are nontransactional. Retry the complete value
    /// or discard the session before execution after failure. HVF clears both
    /// levels after a run; GIC/device composition, delivery/EOI, automatic
    /// reassertion, persistence, schema, and snapshot orchestration remain
    /// outside this primitive.
    pub fn restore_arm64_pending_interrupt_state(
        &self,
        state: &HvfArm64VcpuPendingInterruptState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_pending_interrupt_state(state)
    }

    /// Capture opaque, versioned GIC device state while the primary runner is
    /// stopped.
    ///
    /// The bytes exclude vCPU-affine GIC CPU registers and are not yet a
    /// bangbang snapshot schema or validated restore input.
    pub fn capture_gic_device_state(&self) -> Result<HvfGicDeviceState, HvfVcpuRunnerError> {
        self.runner.capture_gic_device_state()
    }

    /// Reapply complete opaque GIC device state before the primary vCPU runs.
    ///
    /// Session preparation has already created the GIC and vCPU. Compatible
    /// ICC state must follow under a future wider restore lease; after any
    /// failure, discard the session rather than execute it.
    pub fn restore_gic_device_state(
        &self,
        state: &HvfGicDeviceState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_gic_device_state(state)
    }

    /// Capture raw EL1 GIC ICC registers on the primary vCPU owner thread.
    ///
    /// This per-vCPU value complements the opaque GIC device blob but is not a
    /// complete or persisted snapshot. EL2 ICC, ICH, and ICV state are omitted.
    pub fn capture_arm64_gic_icc_register_state(
        &self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_gic_icc_register_state()
    }

    /// Restore raw EL1 GIC ICC registers on the primary vCPU owner thread.
    ///
    /// Apply the compatible opaque GIC blob first. Nine mutable-register writes
    /// are ordered around a derived `ICC_RPR_EL1` validation and are
    /// nontransactional; no vCPU run may have been enqueued. Retry the complete
    /// value or discard the session after a partial failure. This is not a wider
    /// restore lease, persisted snapshot, or destination policy.
    pub fn restore_arm64_gic_icc_register_state(
        &self,
        state: &HvfArm64GicIccRegisterState,
    ) -> Result<(), HvfVcpuRunnerError> {
        self.runner.restore_arm64_gic_icc_register_state(state)
    }

    pub fn run_once_and_handle_mmio(&self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        run_boot_session_vcpu_step(self.runner.singular_runner()?, &self.mmio_dispatcher)
    }

    /// Return a primary-only compatibility handle for a size-one run step.
    ///
    /// Multi-vCPU aggregate execution must use [`Self::run_loop_control`].
    pub fn run_cancel_handle(&self) -> HvfVcpuRunCancelHandle {
        self.runner.run_cancel_handle()
    }

    pub fn run_loop_control(&self) -> HvfArm64BootRunLoopControl {
        HvfArm64BootRunLoopControl::new(self.runner.control(), self.control_wakeup.clone())
    }

    pub fn run_loop(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop(self, stop_token, max_steps)
    }

    pub fn run_loop_with_network_packet_io(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        let mut session = NetworkPacketIoBootSessionRunLoopSession::new(self, packet_io);
        run_boot_session_loop(&mut session, stop_token, max_steps)
    }

    pub fn run_loop_with_observer(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
        observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop_with_observer(self, stop_token, max_steps, observe_step)
    }

    pub fn dispatch_block_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_block_notifications(
                &mut self.backend,
                devices,
                &self.block_device_metrics,
            );
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBlockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher)
                .map_err(
                    |source| HvfArm64BootBlockNotificationDispatchError::MmioDispatcher { source },
                )?;

            self.runtime_resources
                .dispatch_block_queue_notifications(memory, &mut mmio_dispatcher)
                .map_err(|source| {
                    HvfArm64BootBlockNotificationDispatchError::DispatchNotifications { source }
                })?
        };

        record_block_runtime_dispatch_metrics(&self.block_device_metrics, dispatches.as_slice());
        let result = if !dispatches.needs_queue_interrupt() {
            collect_block_notification_dispatches(dispatches)
        } else {
            match HvfGicSpiSignaler::from_metadata(&self.gic) {
                Ok(signaler) => signal_block_queue_interrupts(dispatches, &signaler),
                Err(source) => {
                    Err(HvfArm64BootBlockNotificationDispatchError::CreateSignalSink { source })
                }
            }
        };
        match &result {
            Ok(dispatches) => record_block_signal_metrics(&self.block_device_metrics, dispatches),
            Err(_) => self.block_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_pmem_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_pmem_notifications(
                &mut self.backend,
                devices,
                &self.pmem_device_metrics,
            );
        }
        dispatch_pmem_queue_notifications_and_signal_interrupts(
            &mut self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.pmem_device_metrics,
        )
    }

    pub fn dispatch_network_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_network_notifications(
                &mut self.backend,
                devices,
                None,
                &self.network_interface_metrics,
            );
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_network_queue_notifications_with_packet_io_and_signal_interrupts(
        &mut self,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        if let Some(devices) = self.pci_data_devices.as_mut() {
            return dispatch_pci_network_notifications(
                &mut self.backend,
                devices,
                Some(packet_io),
                &self.network_interface_metrics,
            );
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications_with_packet_io(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                packet_io,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_vsock_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        if self
            .pci_data_devices
            .as_ref()
            .is_some_and(|devices| devices.vsock.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootVsockNotificationDispatchError::MapGuestMemory { source }
            })?;
            if let Some(devices) = self.pci_data_devices.as_mut() {
                devices.dispatch_vsock(memory, &self.vsock_device_metrics);
            }
            return Ok(HvfArm64BootVsockNotificationDispatches::new(Vec::new()));
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootVsockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher)
                .map_err(
                    |source| HvfArm64BootVsockNotificationDispatchError::MmioDispatcher { source },
                )?;

            dispatch_vsock_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_vsock_runtime_dispatch_metrics(&self.vsock_device_metrics, dispatches.as_slice());
        let result = collect_or_signal_vsock_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => record_vsock_signal_metrics(&self.vsock_device_metrics, dispatches),
            Err(_) => self.vsock_device_metrics.record_muxer_event_failure(),
        }
        result
    }

    pub fn dispatch_balloon_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.balloon.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBalloonNotificationDispatchError::MapGuestMemory { source }
            })?;
            let _ = devices.dispatch_balloon(memory, &self.balloon_device_metrics, false);
            return Ok(HvfArm64BootBalloonNotificationDispatches::new(Vec::new()));
        }
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBalloonNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootBalloonNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_balloon_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_balloon_runtime_dispatch_metrics(
            &self.balloon_device_metrics,
            dispatches.as_slice(),
            true,
        );
        let result = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_balloon_signal_metrics(&self.balloon_device_metrics, dispatches)
            }
            Err(_) => self.balloon_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_memory_hotplug_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        if self
            .pci_data_devices
            .as_ref()
            .is_some_and(|devices| devices.memory_hotplug.is_some())
        {
            let (memory, mut mutation_executor) = self
                .backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory { source }
                })?;
            if let Some(devices) = self.pci_data_devices.as_mut() {
                devices.dispatch_memory_hotplug(memory, &mut mutation_executor);
            }
            return Ok(HvfArm64BootMemoryHotplugNotificationDispatches::new(
                Vec::new(),
            ));
        }
        let dispatches = {
            let (memory, mut mutation_executor) = self
                .backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory { source }
                })?;
            let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(&self.mmio_dispatcher)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_memory_hotplug_runtime_notifications_with_executor(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                &mut mutation_executor,
            )?
        };

        collect_or_signal_memory_hotplug_queue_interrupts(dispatches, &self.gic)
    }

    pub fn update_memory_hotplug_requested_size_and_signal_interrupt(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        if let Some(devices) = self
            .pci_data_devices
            .as_ref()
            .filter(|devices| devices.memory_hotplug.is_some())
        {
            return devices.update_memory_hotplug(update);
        }
        update_memory_hotplug_requested_size_and_signal_interrupt(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            &self.gic,
            update,
        )
    }

    pub fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        if let Some(devices) = self
            .pci_data_devices
            .as_ref()
            .filter(|devices| devices.memory_hotplug.is_some())
        {
            return devices.memory_hotplug_status(config, requested_size_mib);
        }
        memory_hotplug_status(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            config,
            requested_size_mib,
        )
    }

    pub fn trigger_balloon_statistics_update_and_signal_interrupts(
        &mut self,
    ) -> Result<(), BalloonUpdateError> {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.balloon.is_some())
        {
            let memory = self
                .backend
                .mapped_guest_memory_mut()
                .map_err(balloon_update_error_from_display)?;
            let result = devices.dispatch_balloon(memory, &self.balloon_device_metrics, true);
            if result.is_err() {
                self.balloon_device_metrics
                    .record_statistics_update_failure();
            }
            return result;
        }
        let result = (|| {
            let dispatches = {
                let memory = self
                    .backend
                    .mapped_guest_memory_mut()
                    .map_err(balloon_update_error_from_display)?;
                let mut mmio_dispatcher = lock_boot_mmio_dispatcher(&self.mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?;

                self.runtime_resources
                    .trigger_balloon_statistics_update(memory, &mut mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?
            };

            record_balloon_runtime_dispatch_metrics(
                &self.balloon_device_metrics,
                dispatches.as_slice(),
                false,
            );
            let dispatches = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic)
                .map_err(balloon_update_error_from_display)?;
            record_balloon_signal_metrics(&self.balloon_device_metrics, &dispatches);

            balloon_update_result_from_hvf_dispatches(&dispatches)
        })();
        if result.is_err() {
            self.balloon_device_metrics
                .record_statistics_update_failure();
        }

        result
    }

    pub fn dispatch_entropy_queue_notifications_and_signal_interrupts(
        &mut self,
        entropy_source: &mut impl Arm64BootEntropySourceProvider,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.entropy.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
            })?;
            let source = match entropy_source.pci_entropy_source() {
                Ok(source) => source,
                Err(_) => {
                    self.entropy_device_metrics
                        .record_entropy_source_provider_failure();
                    return Ok(HvfArm64BootEntropyNotificationDispatches::new(Vec::new()));
                }
            };
            let retry_after =
                devices.dispatch_entropy(memory, source.into_inner(), &self.entropy_device_metrics);
            let mut dispatches = HvfArm64BootEntropyNotificationDispatches::new(Vec::new());
            dispatches.rate_limiter_retry_after = retry_after;
            return Ok(dispatches);
        }
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            &mut self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            entropy_source,
        )
    }
}

impl BootSessionRunLoopSession for OwnedHvfArm64BootSession {
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        start_run_loop_vsock_wakeup_monitor(
            &self.runtime_resources,
            self.pci_data_devices.as_ref(),
            &self.mmio_dispatcher,
            self.runner.control(),
            self.run_loop_wakeup.clone(),
        )
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        self.run_loop_wakeup.take_wakeup_request()
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        self.control_wakeup.take_wakeup_request()
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        self.block_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.block_retry_wakeup.take_wakeup_request();
        }
        self.block_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn take_run_loop_pmem_retry_wakeup_request(&mut self) -> bool {
        self.pmem_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_pmem_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.pmem_retry_wakeup.take_wakeup_request();
        }
        self.pmem_retry_wakeup_scheduler.schedule_after(retry_after);
    }

    fn cancel_run_loop_pmem_retry_wakeup(&mut self) {
        self.pmem_retry_wakeup_scheduler.cancel_and_wait();
        let _ = self.pmem_retry_wakeup.take_wakeup_request();
    }

    fn take_run_loop_network_retry_wakeup_request(&mut self) -> bool {
        self.network_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_network_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.network_retry_wakeup.take_wakeup_request();
        }
        self.network_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn cancel_run_loop_network_retry_wakeup(&mut self) {
        self.network_retry_wakeup_scheduler.cancel_and_wait();
        let _ = self.network_retry_wakeup.take_wakeup_request();
    }

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        self.entropy_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.entropy_retry_wakeup.take_wakeup_request();
        }
        self.entropy_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        run_boot_session_coordinated_vcpu_step(&mut self.runner, &self.backend)
    }

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfArm64BootVcpuError> {
        let intid = self.gic.timer_interrupts.el1_virtual_timer_intid;
        self.runner.set_last_step_ppi_pending(intid)
    }

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        self.dispatch_block_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        self.dispatch_pmem_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        self.dispatch_network_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        self.dispatch_vsock_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        self.dispatch_balloon_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        self.dispatch_memory_hotplug_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.entropy.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
            })?;
            let retry_after = devices.dispatch_entropy(
                memory,
                &mut self.entropy_source,
                &self.entropy_device_metrics,
            );
            let mut dispatches = HvfArm64BootEntropyNotificationDispatches::new(Vec::new());
            dispatches.rate_limiter_retry_after = retry_after;
            return Ok(dispatches);
        }
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            &mut self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            &mut self.entropy_source,
        )
    }
}

struct NetworkPacketIoBootSessionRunLoopSession<'session, 'packet_io, P>
where
    P: Arm64BootNetworkPacketIoProvider,
{
    session: &'session mut OwnedHvfArm64BootSession,
    packet_io: &'packet_io mut P,
}

impl<'session, 'packet_io, P> NetworkPacketIoBootSessionRunLoopSession<'session, 'packet_io, P>
where
    P: Arm64BootNetworkPacketIoProvider,
{
    const fn new(
        session: &'session mut OwnedHvfArm64BootSession,
        packet_io: &'packet_io mut P,
    ) -> Self {
        Self { session, packet_io }
    }
}

impl<P> BootSessionRunLoopSession for NetworkPacketIoBootSessionRunLoopSession<'_, '_, P>
where
    P: Arm64BootNetworkPacketIoProvider,
{
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        self.session.start_run_loop_vsock_wakeup_monitor()
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_wakeup_request()
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_control_wakeup_request()
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_block_retry_wakeup_request()
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        self.session
            .schedule_run_loop_block_retry_wakeup(retry_after);
    }

    fn take_run_loop_pmem_retry_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_pmem_retry_wakeup_request()
    }

    fn schedule_run_loop_pmem_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        self.session
            .schedule_run_loop_pmem_retry_wakeup(retry_after);
    }

    fn cancel_run_loop_pmem_retry_wakeup(&mut self) {
        self.session.cancel_run_loop_pmem_retry_wakeup();
    }

    fn take_run_loop_network_retry_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_network_retry_wakeup_request()
    }

    fn schedule_run_loop_network_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        self.session
            .schedule_run_loop_network_retry_wakeup(retry_after);
    }

    fn cancel_run_loop_network_retry_wakeup(&mut self) {
        self.session.cancel_run_loop_network_retry_wakeup();
    }

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_entropy_retry_wakeup_request()
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        self.session
            .schedule_run_loop_entropy_retry_wakeup(retry_after);
    }

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        run_boot_session_coordinated_vcpu_step(&mut self.session.runner, &self.session.backend)
    }

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfArm64BootVcpuError> {
        let intid = self.session.gic.timer_interrupts.el1_virtual_timer_intid;
        self.session.runner.set_last_step_ppi_pending(intid)
    }

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        self.session
            .dispatch_block_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        self.session
            .dispatch_pmem_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        self.session
            .dispatch_network_queue_notifications_with_packet_io_and_signal_interrupts(
                self.packet_io,
            )
    }

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        self.session
            .dispatch_vsock_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        self.session
            .dispatch_balloon_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        self.session
            .dispatch_memory_hotplug_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        if let Some(devices) = self
            .session
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.entropy.is_some())
        {
            let memory =
                self.session
                    .backend
                    .mapped_guest_memory_mut()
                    .map_err(|source| {
                        HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
                    })?;
            let retry_after = devices.dispatch_entropy(
                memory,
                &mut self.session.entropy_source,
                &self.session.entropy_device_metrics,
            );
            let mut dispatches = HvfArm64BootEntropyNotificationDispatches::new(Vec::new());
            dispatches.rate_limiter_retry_after = retry_after;
            return Ok(dispatches);
        }
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            &mut self.session.backend,
            &self.session.mmio_dispatcher,
            &mut self.session.runtime_resources,
            &self.session.gic,
            &self.session.entropy_device_metrics,
            &mut self.session.entropy_source,
        )
    }
}

impl Drop for OwnedHvfArm64BootSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBlockNotificationDispatches {
    devices: Vec<HvfArm64BootBlockNotificationDispatch>,
    rate_limiter_retry_after: Option<Duration>,
}

impl HvfArm64BootBlockNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootBlockNotificationDispatch>) -> Self {
        let rate_limiter_retry_after = devices
            .iter()
            .filter_map(|device| device.dispatch().rate_limiter_retry_after())
            .min();
        Self {
            devices,
            rate_limiter_retry_after,
        }
    }

    fn from_pci_retry(rate_limiter_retry_after: Option<Duration>) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after,
        }
    }

    #[cfg(test)]
    fn new_for_test_with_rate_limiter_retry_after(retry_after: Duration) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after: Some(retry_after),
        }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootBlockNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBlockNotificationDispatch {
    dispatch: Arm64BootBlockNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootBlockNotificationDispatch {
    fn new(
        dispatch: Arm64BootBlockNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootBlockNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub struct HvfArm64BootPmemNotificationDispatches {
    devices: Vec<HvfArm64BootPmemNotificationDispatch>,
    rate_limiter_retry_after: Option<Duration>,
}

impl HvfArm64BootPmemNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootPmemNotificationDispatch>) -> Self {
        let rate_limiter_retry_after = devices
            .iter()
            .filter_map(|device| device.dispatch().rate_limiter_retry_after())
            .min();
        Self {
            devices,
            rate_limiter_retry_after,
        }
    }

    fn from_pci_retry(rate_limiter_retry_after: Option<Duration>) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after,
        }
    }

    #[cfg(test)]
    fn new_for_test_with_rate_limiter_retry_after(retry_after: Duration) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after: Some(retry_after),
        }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootPmemNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }
}

#[derive(Debug)]
pub struct HvfArm64BootPmemNotificationDispatch {
    dispatch: Arm64BootPmemNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootPmemNotificationDispatch {
    fn new(
        dispatch: Arm64BootPmemNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootPmemNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub struct HvfArm64BootNetworkNotificationDispatches {
    devices: Vec<HvfArm64BootNetworkNotificationDispatch>,
    rate_limiter_retry_after: Option<Duration>,
}

impl HvfArm64BootNetworkNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootNetworkNotificationDispatch>) -> Self {
        let rate_limiter_retry_after = devices
            .iter()
            .filter_map(|device| device.dispatch().rate_limiter_retry_after())
            .min();
        Self {
            devices,
            rate_limiter_retry_after,
        }
    }

    fn from_pci_retry(rate_limiter_retry_after: Option<Duration>) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after,
        }
    }

    #[cfg(test)]
    fn new_for_test_with_rate_limiter_retry_after(retry_after: Duration) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after: Some(retry_after),
        }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootNetworkNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }
}

#[derive(Debug)]
pub struct HvfArm64BootNetworkNotificationDispatch {
    dispatch: Arm64BootNetworkNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootNetworkNotificationDispatch {
    fn new(
        dispatch: Arm64BootNetworkNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootNetworkNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub struct HvfArm64BootVsockNotificationDispatches {
    devices: Vec<HvfArm64BootVsockNotificationDispatch>,
}

impl HvfArm64BootVsockNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootVsockNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootVsockNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }
}

#[derive(Debug)]
pub struct HvfArm64BootVsockNotificationDispatch {
    dispatch: Arm64BootVsockNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootVsockNotificationDispatch {
    fn new(
        dispatch: Arm64BootVsockNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootVsockNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBalloonNotificationDispatches {
    devices: Vec<HvfArm64BootBalloonNotificationDispatch>,
}

impl HvfArm64BootBalloonNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootBalloonNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootBalloonNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBalloonNotificationDispatch {
    dispatch: Arm64BootBalloonNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootBalloonNotificationDispatch {
    fn new(
        dispatch: Arm64BootBalloonNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootBalloonNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub struct HvfArm64BootMemoryHotplugNotificationDispatches {
    devices: Vec<HvfArm64BootMemoryHotplugNotificationDispatch>,
}

impl HvfArm64BootMemoryHotplugNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootMemoryHotplugNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootMemoryHotplugNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }
}

#[derive(Debug)]
pub struct HvfArm64BootMemoryHotplugNotificationDispatch {
    dispatch: Arm64BootMemoryHotplugNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootMemoryHotplugNotificationDispatch {
    fn new(
        dispatch: Arm64BootMemoryHotplugNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootMemoryHotplugNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub struct HvfArm64BootEntropyNotificationDispatches {
    devices: Vec<HvfArm64BootEntropyNotificationDispatch>,
    rate_limiter_retry_after: Option<Duration>,
}

impl HvfArm64BootEntropyNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootEntropyNotificationDispatch>) -> Self {
        let rate_limiter_retry_after = devices
            .iter()
            .filter_map(|device| device.dispatch().rate_limiter_retry_after())
            .min();
        Self {
            devices,
            rate_limiter_retry_after,
        }
    }

    #[cfg(test)]
    fn new_for_test_with_rate_limiter_retry_after(retry_after: Duration) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after: Some(retry_after),
        }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootEntropyNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }
}

#[derive(Debug)]
pub struct HvfArm64BootEntropyNotificationDispatch {
    dispatch: Arm64BootEntropyNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootEntropyNotificationDispatch {
    fn new(
        dispatch: Arm64BootEntropyNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootEntropyNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub enum HvfArm64BootBlockNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootBlockNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
    PciData {
        source: HvfArm64BootPciDataError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootPmemNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootPmemNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
    PciData {
        source: HvfArm64BootPciDataError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootNetworkNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootNetworkNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
    PciData {
        source: HvfArm64BootPciDataError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootVsockNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootVsockNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootBalloonNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootBalloonNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootMemoryHotplugNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootMemoryHotplugNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootEntropyNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootEntropyNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

impl fmt::Display for HvfArm64BootNetworkNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for network notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot network queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot network interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot network notification results: {source}"
                )
            }
            Self::PciData { source } => {
                write!(f, "failed to dispatch PCI network notifications: {source}")
            }
        }
    }
}

impl fmt::Display for HvfArm64BootPmemNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for pmem notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot pmem queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot pmem interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot pmem notification results: {source}"
                )
            }
            Self::PciData { source } => {
                write!(f, "failed to dispatch PCI pmem notifications: {source}")
            }
        }
    }
}

impl fmt::Display for HvfArm64BootVsockNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for vsock notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot vsock queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot vsock interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot vsock notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootBalloonNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for balloon notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot balloon queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot balloon interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot balloon notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootMemoryHotplugNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for memory-hotplug notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot memory-hotplug queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot memory-hotplug interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot memory-hotplug notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootEntropyNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for entropy notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot entropy queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot entropy interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot entropy notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64BootEntropyNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

impl std::error::Error for HvfArm64BootPmemNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
            Self::PciData { source } => Some(source),
        }
    }
}

impl std::error::Error for HvfArm64BootVsockNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

impl std::error::Error for HvfArm64BootBalloonNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

impl std::error::Error for HvfArm64BootMemoryHotplugNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

impl std::error::Error for HvfArm64BootNetworkNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
            Self::PciData { source } => Some(source),
        }
    }
}

impl fmt::Display for HvfArm64BootBlockNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for block notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot block queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot block interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot block notification results: {source}"
                )
            }
            Self::PciData { source } => {
                write!(f, "failed to dispatch PCI block notifications: {source}")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootBlockNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
            Self::PciData { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootMmioDispatcherError {
    Busy,
    Poisoned,
}

impl fmt::Display for HvfArm64BootMmioDispatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("HVF boot-session MMIO dispatcher lock is busy"),
            Self::Poisoned => f.write_str("HVF boot-session MMIO dispatcher lock is poisoned"),
        }
    }
}

impl std::error::Error for HvfArm64BootMmioDispatcherError {}

trait BootSessionRunStepRunner {
    fn run_once_and_handle_mmio(
        &self,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>;
}

impl BootSessionRunStepRunner for HvfVcpuRunner<'_> {
    fn run_once_and_handle_mmio(
        &self,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        HvfVcpuRunner::run_once_and_handle_mmio(self, dispatcher)
    }
}

fn run_boot_session_vcpu_step(
    runner: &impl BootSessionRunStepRunner,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
    runner.run_once_and_handle_mmio(Arc::clone(dispatcher))
}

fn run_boot_session_coordinated_vcpu_step(
    runner: &mut HvfArm64BootVcpuSession<'_>,
    backend: &HvfBackend,
) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
    let memory =
        backend
            .mapped_guest_memory()
            .map_err(|source| HvfArm64BootVcpuError::GuestMemory {
                source: Box::new(source),
            })?;
    runner.run_step(|entry| {
        let address = GuestAddress::new(entry);
        memory
            .regions()
            .iter()
            .any(|region| region.range().contains(address))
    })
}

fn start_run_loop_vsock_wakeup_monitor(
    runtime_resources: &Arm64BootRuntimeResources,
    pci_data_devices: Option<&HvfArm64BootPciDataDevices>,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    vcpu_control: HvfVcpuRunControl,
    wakeup_token: HvfArm64BootRunLoopWakeupToken,
) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
    let pci_vsock = pci_data_devices.filter(|devices| devices.vsock.is_some());
    let has_mmio_block = !runtime_resources.block_devices.is_empty();
    let has_pci_block = pci_data_devices.is_some_and(|devices| !devices.block.is_empty());
    if runtime_resources.vsock_device.is_none()
        && pci_vsock.is_none()
        && !has_mmio_block
        && !has_pci_block
    {
        return Ok(HvfArm64BootRunLoopWakeupMonitor::inactive());
    }

    let mut read_fds = Vec::new();
    let mut write_fds = Vec::new();
    let mut deadline = None;
    let mut has_block_wakeup_fds = false;
    if runtime_resources.vsock_device.is_some() || has_mmio_block {
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher_runtime(dispatcher)
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::MmioDispatcher { source })?;
        if runtime_resources.vsock_device.is_some() {
            (read_fds, write_fds, deadline) =
                runtime_resources
                    .vsock_wakeup(&mut mmio_dispatcher)
                    .map_err(|source| {
                        HvfArm64BootRunLoopWakeupMonitorError::CollectVsockWakeupFds { source }
                    })?
                    .into_parts();
        }
        if has_mmio_block {
            let block_fds =
                runtime_resources
                    .vhost_user_block_call_fds(&mut mmio_dispatcher)
                    .map_err(|source| {
                        HvfArm64BootRunLoopWakeupMonitorError::CollectBlockWakeupFds { source }
                    })?;
            read_fds
                .try_reserve_exact(block_fds.len())
                .map_err(
                    |source| HvfArm64BootRunLoopWakeupMonitorError::PollFdAllocation { source },
                )?;
            has_block_wakeup_fds |= !block_fds.is_empty();
            read_fds.extend(block_fds);
        }
    }
    if let Some(devices) = pci_vsock {
        let (pci_read_fds, pci_write_fds, pci_deadline) = devices
            .vsock_wakeup()
            .map_err(
                |source| HvfArm64BootRunLoopWakeupMonitorError::PciVsockWakeup {
                    message: source.to_string(),
                },
            )?
            .ok_or_else(|| HvfArm64BootRunLoopWakeupMonitorError::PciVsockWakeup {
                message: "PCI vsock endpoint disappeared".to_string(),
            })?;
        read_fds
            .try_reserve_exact(pci_read_fds.len())
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::PollFdAllocation { source })?;
        write_fds
            .try_reserve_exact(pci_write_fds.len())
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::PollFdAllocation { source })?;
        read_fds.extend(pci_read_fds);
        write_fds.extend(pci_write_fds);
        deadline = earliest_deadline(deadline, pci_deadline);
    }
    if let Some(devices) = pci_data_devices.filter(|devices| !devices.block.is_empty()) {
        let block_fds = devices.vhost_user_block_call_fds().map_err(|source| {
            HvfArm64BootRunLoopWakeupMonitorError::PciBlockWakeup {
                message: source.to_string(),
            }
        })?;
        read_fds
            .try_reserve_exact(block_fds.len())
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::PollFdAllocation { source })?;
        has_block_wakeup_fds |= !block_fds.is_empty();
        read_fds.extend(block_fds);
    }

    HvfArm64BootRunLoopWakeupMonitor::start(
        read_fds,
        write_fds,
        deadline,
        has_block_wakeup_fds,
        vcpu_control,
        wakeup_token,
    )
}

fn earliest_deadline(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

trait BootSessionRunLoopSession {
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        Ok(HvfArm64BootRunLoopWakeupMonitor::inactive())
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        false
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        false
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        false
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, _retry_after: Option<Duration>) {}

    fn take_run_loop_pmem_retry_wakeup_request(&mut self) -> bool {
        false
    }

    fn schedule_run_loop_pmem_retry_wakeup(&mut self, _retry_after: Option<Duration>) {}

    fn cancel_run_loop_pmem_retry_wakeup(&mut self) {}

    fn take_run_loop_network_retry_wakeup_request(&mut self) -> bool;

    fn schedule_run_loop_network_retry_wakeup(&mut self, retry_after: Option<Duration>);

    fn cancel_run_loop_network_retry_wakeup(&mut self);

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        false
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, _retry_after: Option<Duration>) {}

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError>;

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfArm64BootVcpuError>;

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>;

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>;

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    >;

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>;

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    >;

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    >;

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    >;
}

impl BootSessionRunLoopSession for HvfArm64BootSession<'_> {
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        start_run_loop_vsock_wakeup_monitor(
            &self.runtime_resources,
            self.pci_data_devices.as_ref(),
            &self.mmio_dispatcher,
            self.runner.control(),
            self.run_loop_wakeup.clone(),
        )
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        self.run_loop_wakeup.take_wakeup_request()
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        self.control_wakeup.take_wakeup_request()
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        self.block_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.block_retry_wakeup.take_wakeup_request();
        }
        self.block_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn take_run_loop_pmem_retry_wakeup_request(&mut self) -> bool {
        self.pmem_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_pmem_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.pmem_retry_wakeup.take_wakeup_request();
        }
        self.pmem_retry_wakeup_scheduler.schedule_after(retry_after);
    }

    fn cancel_run_loop_pmem_retry_wakeup(&mut self) {
        self.pmem_retry_wakeup_scheduler.cancel_and_wait();
        let _ = self.pmem_retry_wakeup.take_wakeup_request();
    }

    fn take_run_loop_network_retry_wakeup_request(&mut self) -> bool {
        self.network_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_network_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.network_retry_wakeup.take_wakeup_request();
        }
        self.network_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn cancel_run_loop_network_retry_wakeup(&mut self) {
        self.network_retry_wakeup_scheduler.cancel_and_wait();
        let _ = self.network_retry_wakeup.take_wakeup_request();
    }

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        self.entropy_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.entropy_retry_wakeup.take_wakeup_request();
        }
        self.entropy_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        run_boot_session_coordinated_vcpu_step(&mut self.runner, self.backend)
    }

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfArm64BootVcpuError> {
        self.runner
            .set_last_step_ppi_pending(self.gic.timer_interrupts.el1_virtual_timer_intid)
    }

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        self.dispatch_block_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        self.dispatch_pmem_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        self.dispatch_network_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        self.dispatch_vsock_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        self.dispatch_balloon_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        self.dispatch_memory_hotplug_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        if let Some(devices) = self
            .pci_data_devices
            .as_mut()
            .filter(|devices| devices.entropy.is_some())
        {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
            })?;
            let retry_after = devices.dispatch_entropy(
                memory,
                &mut self.entropy_source,
                &self.entropy_device_metrics,
            );
            let mut dispatches = HvfArm64BootEntropyNotificationDispatches::new(Vec::new());
            dispatches.rate_limiter_retry_after = retry_after;
            return Ok(dispatches);
        }
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            &mut self.entropy_source,
        )
    }
}

fn run_boot_session_loop(
    session: &mut impl BootSessionRunLoopSession,
    stop_token: &HvfArm64BootRunLoopStopToken,
    max_steps: NonZeroUsize,
) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
    run_boot_session_loop_with_observer(session, stop_token, max_steps, |_| {})
}

fn dispatch_run_loop_block_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    let dispatches = session
        .dispatch_run_loop_block_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchBlockNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;
    session.schedule_run_loop_block_retry_wakeup(dispatches.rate_limiter_retry_after());

    Ok(())
}

fn dispatch_run_loop_block_retry_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<bool, HvfArm64BootRunLoopError> {
    if !session.take_run_loop_block_retry_wakeup_request() {
        return Ok(false);
    }

    dispatch_run_loop_block_notifications_for_step(session, steps_completed)?;

    Ok(true)
}

fn dispatch_run_loop_pmem_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    let dispatches = session
        .dispatch_run_loop_pmem_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchPmemNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;
    session.schedule_run_loop_pmem_retry_wakeup(dispatches.rate_limiter_retry_after());

    Ok(())
}

fn dispatch_run_loop_pmem_retry_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<bool, HvfArm64BootRunLoopError> {
    if !session.take_run_loop_pmem_retry_wakeup_request() {
        return Ok(false);
    }

    dispatch_run_loop_pmem_notifications_for_step(session, steps_completed)?;

    Ok(true)
}

fn dispatch_run_loop_network_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    let dispatches = session
        .dispatch_run_loop_network_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchNetworkNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;
    session.schedule_run_loop_network_retry_wakeup(dispatches.rate_limiter_retry_after());

    Ok(())
}

fn dispatch_run_loop_network_retry_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<bool, HvfArm64BootRunLoopError> {
    if !session.take_run_loop_network_retry_wakeup_request() {
        return Ok(false);
    }

    dispatch_run_loop_network_notifications_for_step(session, steps_completed)?;

    Ok(true)
}

fn dispatch_run_loop_entropy_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    let dispatches = session
        .dispatch_run_loop_entropy_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchEntropyNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;
    session.schedule_run_loop_entropy_retry_wakeup(dispatches.rate_limiter_retry_after());

    Ok(())
}

fn dispatch_run_loop_entropy_retry_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<bool, HvfArm64BootRunLoopError> {
    if !session.take_run_loop_entropy_retry_wakeup_request() {
        return Ok(false);
    }

    dispatch_run_loop_entropy_notifications_for_step(session, steps_completed)?;

    Ok(true)
}

fn dispatch_run_loop_vsock_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    session
        .dispatch_run_loop_vsock_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchVsockNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;

    Ok(())
}

fn run_boot_session_loop_with_observer(
    session: &mut impl BootSessionRunLoopSession,
    stop_token: &HvfArm64BootRunLoopStopToken,
    max_steps: NonZeroUsize,
    observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
    let result =
        run_boot_session_loop_with_observer_inner(session, stop_token, max_steps, observe_step);
    if !matches!(
        &result,
        Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { .. })
            | Ok(HvfArm64BootRunLoopOutcome::Wakeup { .. })
    ) {
        session.cancel_run_loop_pmem_retry_wakeup();
        session.cancel_run_loop_network_retry_wakeup();
    }

    result
}

fn run_boot_session_loop_with_observer_inner(
    session: &mut impl BootSessionRunLoopSession,
    stop_token: &HvfArm64BootRunLoopStopToken,
    max_steps: NonZeroUsize,
    mut observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
    let max_steps = max_steps.get();
    let mut steps = 0usize;

    loop {
        if stop_token.is_stop_requested() {
            return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
        }
        let _ = dispatch_run_loop_pmem_retry_notifications_for_step(session, steps)?;
        if stop_token.is_stop_requested() {
            return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
        }
        let _ = dispatch_run_loop_network_retry_notifications_for_step(session, steps)?;
        if stop_token.is_stop_requested() {
            return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
        }

        let monitor = session
            .start_run_loop_vsock_wakeup_monitor()
            .map_err(|source| HvfArm64BootRunLoopError::StartVsockWakeupMonitor {
                steps_completed: steps,
                source: Box::new(source),
            })?;
        let monitor_has_block_wakeup_fds = monitor.has_block_wakeup_fds();
        let outcome_result = session.run_loop_vcpu_step();
        let finish_result = monitor.finish();
        let monitor_wakeup_requested = match &outcome_result {
            Ok(_) => finish_result.map_err(|source| {
                HvfArm64BootRunLoopError::StopVsockWakeupMonitor {
                    steps_completed: steps.saturating_add(1),
                    source: Box::new(source),
                }
            })?,
            Err(_) => finish_result.map_err(|source| {
                HvfArm64BootRunLoopError::StopVsockWakeupMonitor {
                    steps_completed: steps,
                    source: Box::new(source),
                }
            })?,
        };
        let outcome = outcome_result.map_err(|source| HvfArm64BootRunLoopError::RunStep {
            steps_completed: steps,
            source: Box::new(source),
        })?;
        observe_step(&outcome);
        steps += 1;

        match outcome {
            HvfVcpuRunStepOutcome::Canceled => {
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let control_wakeup_requested = session.take_run_loop_control_wakeup_request();
                let wakeup_requested =
                    session.take_run_loop_wakeup_request() || monitor_wakeup_requested;
                let block_retry_wakeup_requested =
                    session.take_run_loop_block_retry_wakeup_request();
                let pmem_retry_wakeup_requested = session.take_run_loop_pmem_retry_wakeup_request();
                let network_retry_wakeup_requested =
                    session.take_run_loop_network_retry_wakeup_request();
                let entropy_retry_wakeup_requested =
                    session.take_run_loop_entropy_retry_wakeup_request();
                if block_retry_wakeup_requested {
                    dispatch_run_loop_block_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if pmem_retry_wakeup_requested {
                    dispatch_run_loop_pmem_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if network_retry_wakeup_requested {
                    dispatch_run_loop_network_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if entropy_retry_wakeup_requested {
                    dispatch_run_loop_entropy_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if monitor_wakeup_requested
                    && monitor_has_block_wakeup_fds
                    && !block_retry_wakeup_requested
                {
                    dispatch_run_loop_block_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if wakeup_requested {
                    dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if control_wakeup_requested {
                    return Ok(HvfArm64BootRunLoopOutcome::Wakeup { steps });
                }
                if wakeup_requested
                    || block_retry_wakeup_requested
                    || pmem_retry_wakeup_requested
                    || network_retry_wakeup_requested
                    || entropy_retry_wakeup_requested
                {
                    if steps == max_steps {
                        return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                    }
                    continue;
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
                continue;
            }
            HvfVcpuRunStepOutcome::VtimerActivated => {
                session.handle_run_loop_virtual_timer().map_err(|source| {
                    HvfArm64BootRunLoopError::HandleVirtualTimer {
                        steps_completed: steps,
                        source: Box::new(source),
                    }
                })?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_block_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_pmem_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_network_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_entropy_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
            }
            HvfVcpuRunStepOutcome::Unknown { reason } => {
                return Ok(HvfArm64BootRunLoopOutcome::Unknown { steps, reason });
            }
            HvfVcpuRunStepOutcome::GuestShutdown { .. } => {
                return Ok(HvfArm64BootRunLoopOutcome::GuestShutdown { steps });
            }
            HvfVcpuRunStepOutcome::GuestReset { .. } => {
                return Ok(HvfArm64BootRunLoopOutcome::GuestReset { steps });
            }
            HvfVcpuRunStepOutcome::Hvc { .. }
            | HvfVcpuRunStepOutcome::CpuOff { .. }
            | HvfVcpuRunStepOutcome::CpuSuspend { .. }
            | HvfVcpuRunStepOutcome::Sys64 { .. }
            | HvfVcpuRunStepOutcome::DirtyWrite { .. } => {
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_block_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_pmem_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_network_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_entropy_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
            }
            HvfVcpuRunStepOutcome::Mmio { .. } => {
                let _ = session.take_run_loop_block_retry_wakeup_request();
                let _ = session.take_run_loop_pmem_retry_wakeup_request();
                let _ = session.take_run_loop_network_retry_wakeup_request();
                let _ = session.take_run_loop_entropy_retry_wakeup_request();
                dispatch_run_loop_block_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_pmem_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_network_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                session
                    .dispatch_run_loop_balloon_notifications()
                    .map_err(
                        |source| HvfArm64BootRunLoopError::DispatchBalloonNotifications {
                            steps_completed: steps,
                            source: Box::new(source),
                        },
                    )?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                session
                    .dispatch_run_loop_memory_hotplug_notifications()
                    .map_err(|source| {
                        HvfArm64BootRunLoopError::DispatchMemoryHotplugNotifications {
                            steps_completed: steps,
                            source: Box::new(source),
                        }
                    })?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_entropy_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
            }
        }
    }
}

struct HvfArm64BootRunLoopWakeupMonitor {
    stop_writer: Option<UnixStream>,
    thread: Option<JoinHandle<bool>>,
    completed_wakeup: bool,
    has_block_wakeup_fds: bool,
}

impl HvfArm64BootRunLoopWakeupMonitor {
    const fn inactive() -> Self {
        Self {
            stop_writer: None,
            thread: None,
            completed_wakeup: false,
            has_block_wakeup_fds: false,
        }
    }

    #[cfg(test)]
    const fn completed_for_test(completed_wakeup: bool) -> Self {
        Self {
            stop_writer: None,
            thread: None,
            completed_wakeup,
            has_block_wakeup_fds: false,
        }
    }

    #[cfg(test)]
    const fn completed_block_for_test(completed_wakeup: bool) -> Self {
        Self {
            stop_writer: None,
            thread: None,
            completed_wakeup,
            has_block_wakeup_fds: true,
        }
    }

    fn start(
        host_read_fds: Vec<RawFd>,
        host_write_fds: Vec<RawFd>,
        deadline: Option<Instant>,
        has_block_wakeup_fds: bool,
        vcpu_control: HvfVcpuRunControl,
        wakeup_token: HvfArm64BootRunLoopWakeupToken,
    ) -> Result<Self, HvfArm64BootRunLoopWakeupMonitorError> {
        if host_read_fds.is_empty() && host_write_fds.is_empty() && deadline.is_none() {
            return Ok(Self::inactive());
        }

        let (stop_reader, stop_writer) =
            UnixStream::pair().map_err(|source| Self::create_stop_pipe_error(source.kind()))?;
        let mut pollfds =
            vsock_wakeup_pollfds(host_read_fds, host_write_fds, stop_reader.as_raw_fd())?;
        let pollfd_count = libc::nfds_t::try_from(pollfds.len()).map_err(|_| {
            HvfArm64BootRunLoopWakeupMonitorError::TooManyPollFds {
                count: pollfds.len(),
            }
        })?;
        let thread = thread::Builder::new()
            .name(VSOCK_WAKEUP_MONITOR_THREAD_NAME.to_owned())
            .spawn(move || {
                run_vsock_wakeup_monitor(
                    &mut pollfds,
                    pollfd_count,
                    stop_reader,
                    deadline,
                    vcpu_control,
                    wakeup_token,
                )
            })
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::ThreadSpawn { source })?;

        Ok(Self {
            stop_writer: Some(stop_writer),
            thread: Some(thread),
            completed_wakeup: false,
            has_block_wakeup_fds,
        })
    }

    const fn has_block_wakeup_fds(&self) -> bool {
        self.has_block_wakeup_fds
    }

    fn finish(mut self) -> Result<bool, HvfArm64BootRunLoopWakeupMonitorError> {
        let mut stop_signal_error = None;
        if let Some(mut stop_writer) = self.stop_writer.take() {
            match stop_writer.write_all(&VSOCK_WAKEUP_MONITOR_STOP_BYTE) {
                Ok(()) => {}
                Err(source)
                    if matches!(
                        source.kind(),
                        io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected
                    ) => {}
                Err(source) => {
                    stop_signal_error = Some(source.kind());
                }
            }
        }

        let completed_wakeup = if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| HvfArm64BootRunLoopWakeupMonitorError::ThreadPanicked)?
        } else {
            self.completed_wakeup
        };

        if let Some(source) = stop_signal_error {
            return Err(HvfArm64BootRunLoopWakeupMonitorError::StopSignal { source });
        }

        Ok(completed_wakeup)
    }

    const fn create_stop_pipe_error(
        source: io::ErrorKind,
    ) -> HvfArm64BootRunLoopWakeupMonitorError {
        HvfArm64BootRunLoopWakeupMonitorError::CreateStopPipe { source }
    }
}

fn vsock_wakeup_pollfds(
    mut host_read_fds: Vec<RawFd>,
    mut host_write_fds: Vec<RawFd>,
    stop_fd: RawFd,
) -> Result<Vec<libc::pollfd>, HvfArm64BootRunLoopWakeupMonitorError> {
    host_read_fds.sort_unstable();
    host_read_fds.dedup();
    host_write_fds.sort_unstable();
    host_write_fds.dedup();
    let mut pollfds = Vec::new();
    pollfds
        .try_reserve_exact(
            host_read_fds
                .len()
                .saturating_add(host_write_fds.len())
                .saturating_add(1),
        )
        .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::PollFdAllocation { source })?;
    pollfds.push(libc::pollfd {
        fd: stop_fd,
        events: libc::POLLIN,
        revents: 0,
    });
    let mut read_index = 0;
    let mut write_index = 0;
    while read_index < host_read_fds.len() || write_index < host_write_fds.len() {
        let read_fd = host_read_fds.get(read_index).copied();
        let write_fd = host_write_fds.get(write_index).copied();
        let (fd, events) = match (read_fd, write_fd) {
            (Some(read_fd), Some(write_fd)) if read_fd == write_fd => {
                read_index += 1;
                write_index += 1;
                (read_fd, libc::POLLIN | libc::POLLOUT)
            }
            (Some(read_fd), Some(write_fd)) if read_fd < write_fd => {
                read_index += 1;
                (read_fd, libc::POLLIN)
            }
            (Some(_), Some(write_fd)) => {
                write_index += 1;
                (write_fd, libc::POLLOUT)
            }
            (Some(read_fd), None) => {
                read_index += 1;
                (read_fd, libc::POLLIN)
            }
            (None, Some(write_fd)) => {
                write_index += 1;
                (write_fd, libc::POLLOUT)
            }
            (None, None) => break,
        };
        pollfds.push(libc::pollfd {
            fd,
            events,
            revents: 0,
        });
    }

    Ok(pollfds)
}

fn run_vsock_wakeup_monitor(
    pollfds: &mut [libc::pollfd],
    pollfd_count: libc::nfds_t,
    _stop_reader: UnixStream,
    deadline: Option<Instant>,
    vcpu_control: HvfVcpuRunControl,
    wakeup_token: HvfArm64BootRunLoopWakeupToken,
) -> bool {
    run_vsock_wakeup_monitor_with(
        pollfds,
        pollfd_count,
        deadline,
        Instant::now,
        |pollfds, pollfd_count, timeout| {
            // SAFETY: `pollfds` is a valid mutable slice for `pollfd_count`
            // entries and remains alive for the duration of this `poll` call.
            let result = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfd_count, timeout) };
            if result < 0 {
                Err(io::Error::last_os_error().kind())
            } else {
                Ok(result)
            }
        },
        || {
            if vcpu_control.request_wakeup().is_ok() {
                wakeup_token.request_wakeup();
            }
        },
    )
}

fn run_vsock_wakeup_monitor_with(
    pollfds: &mut [libc::pollfd],
    pollfd_count: libc::nfds_t,
    deadline: Option<Instant>,
    mut now: impl FnMut() -> Instant,
    mut poll: impl FnMut(
        &mut [libc::pollfd],
        libc::nfds_t,
        libc::c_int,
    ) -> Result<libc::c_int, io::ErrorKind>,
    mut request_wakeup: impl FnMut(),
) -> bool {
    loop {
        for pollfd in pollfds.iter_mut() {
            pollfd.revents = 0;
        }

        let timeout = vsock_wakeup_poll_timeout(deadline, now());
        let poll_result = match poll(pollfds, pollfd_count, timeout) {
            Ok(result) => result,
            Err(io::ErrorKind::Interrupted) => continue,
            Err(_) => {
                request_wakeup();
                return true;
            }
        };

        let Some(stop_pollfd) = pollfds.first() else {
            return false;
        };
        if pollfd_has_wakeup_event(stop_pollfd.revents) {
            return false;
        }
        let fd_wakeup = pollfds
            .iter()
            .skip(1)
            .any(|pollfd| pollfd_has_wakeup_event(pollfd.revents));
        if fd_wakeup || (poll_result == 0 && deadline.is_some()) {
            request_wakeup();
            return true;
        }
    }
}

fn vsock_wakeup_poll_timeout(deadline: Option<Instant>, now: Instant) -> libc::c_int {
    let Some(deadline) = deadline else {
        return POLL_FOREVER;
    };
    let Some(remaining) = deadline.checked_duration_since(now) else {
        return 0;
    };
    if remaining.is_zero() {
        return 0;
    }

    let whole_millis = remaining.as_millis();
    let rounded_millis =
        whole_millis.saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
    rounded_millis
        .min(libc::c_int::MAX as u128)
        .try_into()
        .unwrap_or(libc::c_int::MAX)
}

const fn pollfd_has_wakeup_event(revents: libc::c_short) -> bool {
    revents & (libc::POLLIN | libc::POLLOUT | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

fn lock_boot_mmio_dispatcher(
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
) -> Result<MutexGuard<'_, MmioDispatcher>, HvfArm64BootMmioDispatcherError> {
    dispatcher.try_lock().map_err(|source| match source {
        TryLockError::WouldBlock => HvfArm64BootMmioDispatcherError::Busy,
        TryLockError::Poisoned(_) => HvfArm64BootMmioDispatcherError::Poisoned,
    })
}

fn lock_boot_mmio_dispatcher_runtime(
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
) -> Result<MutexGuard<'_, MmioDispatcher>, HvfArm64BootMmioDispatcherError> {
    dispatcher
        .lock()
        .map_err(|_| HvfArm64BootMmioDispatcherError::Poisoned)
}

fn collect_block_notification_dispatches(
    dispatches: Arm64BootBlockNotificationDispatches,
) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBlockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootBlockNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootBlockNotificationDispatches::new(devices))
}

fn collect_pmem_notification_dispatches(
    dispatches: Arm64BootPmemNotificationDispatches,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::ResultAllocation { source })?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootPmemNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootPmemNotificationDispatches::new(devices))
}

fn signal_block_queue_interrupts(
    dispatches: Arm64BootBlockNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBlockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootBlockNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootBlockNotificationDispatches::new(devices))
}

fn signal_pmem_queue_interrupts(
    dispatches: Arm64BootPmemNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::ResultAllocation { source })?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootPmemNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootPmemNotificationDispatches::new(devices))
}

fn collect_network_notification_dispatches(
    dispatches: Arm64BootNetworkNotificationDispatches,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootNetworkNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootNetworkNotificationDispatches::new(devices))
}

fn collect_vsock_notification_dispatches(
    dispatches: Arm64BootVsockNotificationDispatches,
) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootVsockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootVsockNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootVsockNotificationDispatches::new(devices))
}

fn collect_balloon_notification_dispatches(
    dispatches: Arm64BootBalloonNotificationDispatches,
) -> Result<HvfArm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBalloonNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootBalloonNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootBalloonNotificationDispatches::new(devices))
}

fn collect_memory_hotplug_notification_dispatches(
    dispatches: Arm64BootMemoryHotplugNotificationDispatches,
) -> Result<
    HvfArm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootMemoryHotplugNotificationDispatchError::ResultAllocation {
                source,
            },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootMemoryHotplugNotificationDispatch::new(
            dispatch, None,
        ));
    }

    Ok(HvfArm64BootMemoryHotplugNotificationDispatches::new(
        devices,
    ))
}

fn collect_entropy_notification_dispatches(
    dispatches: Arm64BootEntropyNotificationDispatches,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootEntropyNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootEntropyNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootEntropyNotificationDispatches::new(devices))
}

fn dispatch_pci_block_notifications(
    backend: &mut HvfBackend,
    devices: &mut HvfArm64BootPciDataDevices,
    metrics: &SharedBlockDeviceMetricsRegistry,
) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError> {
    let memory = backend
        .mapped_guest_memory_mut()
        .map_err(|source| HvfArm64BootBlockNotificationDispatchError::MapGuestMemory { source })?;
    Ok(HvfArm64BootBlockNotificationDispatches::from_pci_retry(
        devices.dispatch_block(memory, metrics),
    ))
}

fn runtime_block_publication_error(source: VirtioPciPublicationError) -> DriveRuntimeMutationError {
    let terminal = matches!(
        source,
        VirtioPciPublicationError::Rollback { .. } | VirtioPciPublicationError::SegmentLock { .. }
    );
    let message = source.to_string();
    if terminal {
        DriveRuntimeMutationError::TerminalInsertion { message }
    } else {
        DriveRuntimeMutationError::PublishDevice { message }
    }
}

fn runtime_pmem_publication_error(source: VirtioPciPublicationError) -> PmemRuntimeMutationError {
    let terminal = matches!(
        source,
        VirtioPciPublicationError::Rollback { .. } | VirtioPciPublicationError::SegmentLock { .. }
    );
    let message = source.to_string();
    if terminal {
        PmemRuntimeMutationError::TerminalInsertion { message }
    } else {
        PmemRuntimeMutationError::PublishDevice { message }
    }
}

fn runtime_network_publication_error(
    source: VirtioPciPublicationError,
) -> NetworkRuntimeMutationError {
    let terminal = matches!(
        source,
        VirtioPciPublicationError::Rollback { .. } | VirtioPciPublicationError::SegmentLock { .. }
    );
    let message = source.to_string();
    if terminal {
        NetworkRuntimeMutationError::TerminalInsertion { message }
    } else {
        NetworkRuntimeMutationError::PublishDevice { message }
    }
}

fn remove_runtime_pci_block_device_and_refresh_retry(
    pci_data_devices: &mut Option<HvfArm64BootPciDataDevices>,
    retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    retry_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    drive_id: &str,
) -> Result<(), DriveRuntimeMutationError> {
    let devices = pci_data_devices
        .as_mut()
        .ok_or(DriveRuntimeMutationError::PciNotEnabled)?;
    devices.remove_runtime_block(drive_id)?;

    refresh_block_retry_wakeup_after_inventory_change(
        retry_wakeup,
        retry_scheduler,
        devices.block_retry_deadline(),
    );
    Ok(())
}

fn refresh_block_retry_wakeup_after_inventory_change(
    retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    retry_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    retry_deadline: Option<Instant>,
) {
    // The shared scheduler holds the earliest retry across the live block
    // inventory. Once an endpoint disappears, its deadline cannot be
    // distinguished from deadlines owned by survivors, so synchronously
    // discard it and derive the replacement from the remaining endpoints.
    retry_scheduler.cancel_and_wait();
    let _ = retry_wakeup.take_wakeup_request();
    retry_scheduler.schedule_deadline(retry_deadline);
}

fn remove_runtime_pci_pmem_device_and_refresh_retry(
    pci_data_devices: &mut Option<HvfArm64BootPciDataDevices>,
    backend: &mut HvfBackend,
    runtime_pmem_devices: &mut Vec<PreparedPmemDevice>,
    retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    retry_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    pmem_id: &str,
) -> Result<(), PmemRuntimeMutationError> {
    let devices = pci_data_devices
        .as_mut()
        .ok_or(PmemRuntimeMutationError::PciNotEnabled)?;
    devices.remove_runtime_pmem(backend, runtime_pmem_devices, pmem_id)?;
    refresh_pmem_retry_wakeup_after_inventory_change(
        retry_wakeup,
        retry_scheduler,
        devices.pmem_retry_deadline(),
    );
    Ok(())
}

fn refresh_pmem_retry_wakeup_after_inventory_change(
    retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    retry_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    retry_deadline: Option<Instant>,
) {
    retry_scheduler.cancel_and_wait();
    let _ = retry_wakeup.take_wakeup_request();
    retry_scheduler.schedule_deadline(retry_deadline);
}

fn refresh_network_retry_wakeup_after_inventory_change(
    retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    retry_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    retry_deadline: Option<Instant>,
) {
    retry_scheduler.cancel_and_wait();
    let _ = retry_wakeup.take_wakeup_request();
    retry_scheduler.schedule_deadline(retry_deadline);
}

fn dispatch_pci_pmem_notifications(
    backend: &mut HvfBackend,
    devices: &mut HvfArm64BootPciDataDevices,
    metrics: &SharedPmemDeviceMetricsRegistry,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let (memory, executor) = backend
        .mapped_guest_memory_and_pmem_flush_executor_mut()
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::MapGuestMemory { source })?;
    let mut flush_provider = HvfArm64BootPmemFlushProvider::new(executor);
    Ok(HvfArm64BootPmemNotificationDispatches::from_pci_retry(
        devices.dispatch_pmem(memory, &mut flush_provider, metrics),
    ))
}

fn dispatch_pci_network_notifications(
    backend: &mut HvfBackend,
    devices: &mut HvfArm64BootPciDataDevices,
    packet_io: Option<&mut dyn Arm64BootNetworkPacketIoProvider>,
    metrics: &SharedNetworkInterfaceMetricsRegistry,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    let memory = backend.mapped_guest_memory_mut().map_err(|source| {
        HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
    })?;
    Ok(HvfArm64BootNetworkNotificationDispatches::from_pci_retry(
        devices.dispatch_network(memory, packet_io, metrics),
    ))
}

fn dispatch_pmem_queue_notifications_and_signal_interrupts(
    backend: &mut HvfBackend,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    runtime_resources: &mut Arm64BootRuntimeResources,
    gic: &HvfGicMetadata,
    metrics: &SharedPmemDeviceMetricsRegistry,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let dispatches = {
        let (memory, executor) = backend
            .mapped_guest_memory_and_pmem_flush_executor_mut()
            .map_err(
                |source| HvfArm64BootPmemNotificationDispatchError::MapGuestMemory { source },
            )?;
        let mut flush_provider = HvfArm64BootPmemFlushProvider::new(executor);
        let mut mmio_dispatcher =
            lock_boot_mmio_dispatcher_runtime(dispatcher).map_err(|source| {
                HvfArm64BootPmemNotificationDispatchError::MmioDispatcher { source }
            })?;

        dispatch_pmem_runtime_notifications(
            memory,
            runtime_resources,
            &mut mmio_dispatcher,
            &mut flush_provider,
        )?
    };

    record_pmem_runtime_dispatch_metrics(metrics, dispatches.as_slice());
    let result = collect_or_signal_pmem_queue_interrupts(dispatches, gic);
    match &result {
        Ok(dispatches) => record_pmem_signal_metrics(metrics, dispatches),
        Err(_) => metrics.record_event_failure(),
    }
    result
}

fn dispatch_pmem_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    flush_provider: &mut impl Arm64BootPmemFlushProvider,
) -> Result<Arm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    runtime_resources
        .dispatch_pmem_queue_notifications(memory, mmio_dispatcher, flush_provider)
        .map_err(
            |source| HvfArm64BootPmemNotificationDispatchError::DispatchNotifications { source },
        )
}

#[derive(Debug)]
struct HvfArm64BootPmemFlushProvider<'a> {
    executor: HvfPmemFlushExecutor<'a>,
}

impl<'a> HvfArm64BootPmemFlushProvider<'a> {
    fn new(executor: HvfPmemFlushExecutor<'a>) -> Self {
        Self { executor }
    }
}

impl Arm64BootPmemFlushProvider for HvfArm64BootPmemFlushProvider<'_> {
    fn flush(&mut self, guest_range: GuestMemoryRange) -> VirtioPmemFlushStatus {
        VirtioPmemFlushStatus::from_result(self.executor.flush(guest_range).is_ok())
    }
}

fn dispatch_network_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<Arm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError> {
    runtime_resources
        .dispatch_network_queue_notifications(memory, mmio_dispatcher)
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_network_runtime_notifications_with_packet_io(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
) -> Result<Arm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError> {
    runtime_resources
        .dispatch_network_queue_notifications_with_packet_io(memory, mmio_dispatcher, packet_io)
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_vsock_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<Arm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    runtime_resources
        .dispatch_vsock_queue_notifications(memory, mmio_dispatcher)
        .map_err(
            |source| HvfArm64BootVsockNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_balloon_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<Arm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError> {
    runtime_resources
        .dispatch_balloon_queue_notifications(memory, mmio_dispatcher)
        .map_err(
            |source| HvfArm64BootBalloonNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_memory_hotplug_runtime_notifications_with_executor(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    mutation_executor: &mut impl VirtioMemMutationExecutor,
) -> Result<
    Arm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    runtime_resources
        .dispatch_memory_hotplug_queue_notifications_with_executor(
            memory,
            mmio_dispatcher,
            mutation_executor,
        )
        .map_err(|source| {
            HvfArm64BootMemoryHotplugNotificationDispatchError::DispatchNotifications { source }
        })
}

fn update_memory_hotplug_requested_size_and_signal_interrupt(
    runtime_resources: &Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    gic: &HvfGicMetadata,
    update: MemoryHotplugSizeUpdate,
) -> Result<(), MemoryHotplugUpdateError> {
    let device = runtime_resources
        .memory_hotplug_device
        .as_ref()
        .ok_or(MemoryHotplugUpdateError::ActiveSessionUnavailable)?;
    {
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
            MemoryHotplugUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            }
        })?;
        update_memory_hotplug_config_for_device(device, &mut mmio_dispatcher, update)?;
    }

    if let Ok(signaler) = HvfGicSpiSignaler::from_metadata(gic) {
        let _ = signal_device_interrupt(
            device.fdt_device.interrupt_line,
            DeviceInterruptKind::Config,
            &signaler,
        );
    }

    Ok(())
}

fn memory_hotplug_status(
    runtime_resources: &Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    config: MemoryHotplugConfig,
    requested_size_mib: u64,
) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
    let device = runtime_resources
        .memory_hotplug_device
        .as_ref()
        .ok_or(MemoryHotplugStatusError::ActiveSessionUnavailable)?;
    let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
        MemoryHotplugStatusError::ActiveSessionCommand {
            message: source.to_string(),
        }
    })?;

    memory_hotplug_status_for_device(device, &mut mmio_dispatcher, config, requested_size_mib)
}

fn balloon_update_error_from_display(source: impl fmt::Display) -> BalloonUpdateError {
    BalloonUpdateError::ActiveSessionCommand {
        message: source.to_string(),
    }
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
fn record_block_dispatch_metrics(
    metrics: &SharedBlockDeviceMetricsRegistry,
    dispatches: &HvfArm64BootBlockNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootBlockNotificationDispatch::dispatch);
    record_block_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_block_signal_metrics(metrics, dispatches);
}

fn record_block_runtime_dispatch_metrics<'a>(
    metrics: &SharedBlockDeviceMetricsRegistry,
    dispatches: impl IntoIterator<Item = &'a Arm64BootBlockNotificationDispatch>,
) {
    for dispatch in dispatches {
        let drive_id = dispatch.device().registration.drive_id();
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch_for_drive(drive_id, dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_queue_events_for_drive(
                drive_id,
                usize_to_u64_saturating(source.drained_notifications().len()),
            );
            metrics.record_event_failure_for_drive(drive_id);
            if let Some(completed) = source.completed_dispatch() {
                metrics.record_queue_dispatch_for_drive(drive_id, completed);
            }
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure_for_drive(drive_id);
        }
    }
}

fn record_block_signal_metrics(
    metrics: &SharedBlockDeviceMetricsRegistry,
    dispatches: &HvfArm64BootBlockNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            let drive_id = dispatch.dispatch().device().registration.drive_id();
            metrics.record_event_failure_for_drive(drive_id);
        }
    }
}

#[cfg(test)]
fn record_pmem_dispatch_metrics(
    metrics: &SharedPmemDeviceMetricsRegistry,
    dispatches: &HvfArm64BootPmemNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootPmemNotificationDispatch::dispatch);
    record_pmem_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_pmem_signal_metrics(metrics, dispatches);
}

fn record_pmem_runtime_dispatch_metrics<'a>(
    metrics: &SharedPmemDeviceMetricsRegistry,
    dispatches: impl IntoIterator<Item = &'a Arm64BootPmemNotificationDispatch>,
) {
    for dispatch in dispatches {
        let device_id = dispatch.device().registration.pmem_id();
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch_for_device(device_id, dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error_for_device(device_id, source);
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure_for_device(device_id);
        }
    }
}

fn record_pmem_signal_metrics(
    metrics: &SharedPmemDeviceMetricsRegistry,
    dispatches: &HvfArm64BootPmemNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            let device_id = dispatch.dispatch().device().registration.pmem_id();
            metrics.record_event_failure_for_device(device_id);
        }
    }
}

#[cfg(test)]
fn record_network_dispatch_metrics(
    metrics: &SharedNetworkInterfaceMetricsRegistry,
    dispatches: &HvfArm64BootNetworkNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootNetworkNotificationDispatch::dispatch);
    record_network_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_network_signal_metrics(metrics, dispatches);
}

fn record_network_runtime_dispatch_metrics<'a>(
    metrics: &SharedNetworkInterfaceMetricsRegistry,
    dispatches: impl IntoIterator<Item = &'a Arm64BootNetworkNotificationDispatch>,
) {
    for dispatch in dispatches {
        let iface_id = dispatch.device().registration.iface_id();
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch_for_interface(iface_id, dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error_for_interface(iface_id, source);
        }
        if dispatch.outcome().handler_lookup_error().is_some()
            || dispatch.outcome().packet_io_error().is_some()
        {
            metrics.record_event_failure_for_interface(iface_id);
        }
    }
}

fn record_network_signal_metrics(
    metrics: &SharedNetworkInterfaceMetricsRegistry,
    dispatches: &HvfArm64BootNetworkNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            let iface_id = dispatch.dispatch().device().registration.iface_id();
            metrics.record_event_failure_for_interface(iface_id);
        }
    }
}

#[cfg(test)]
fn record_vsock_dispatch_metrics(
    metrics: &SharedVsockDeviceMetrics,
    dispatches: &HvfArm64BootVsockNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootVsockNotificationDispatch::dispatch);
    record_vsock_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_vsock_signal_metrics(metrics, dispatches);
}

fn record_vsock_runtime_dispatch_metrics<'a>(
    metrics: &SharedVsockDeviceMetrics,
    dispatches: impl IntoIterator<Item = &'a Arm64BootVsockNotificationDispatch>,
) {
    for dispatch in dispatches {
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch(dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error(source);
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_muxer_event_failure();
        }
    }
}

fn record_vsock_signal_metrics(
    metrics: &SharedVsockDeviceMetrics,
    dispatches: &HvfArm64BootVsockNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            metrics.record_muxer_event_failure();
        }
    }
}

#[cfg(test)]
fn record_balloon_dispatch_metrics(
    metrics: &SharedBalloonDeviceMetrics,
    dispatches: &HvfArm64BootBalloonNotificationDispatches,
    inactive_is_failure: bool,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootBalloonNotificationDispatch::dispatch);
    record_balloon_runtime_dispatch_metrics(metrics, runtime_dispatches, inactive_is_failure);
    record_balloon_signal_metrics(metrics, dispatches);
}

fn record_balloon_runtime_dispatch_metrics<'a>(
    metrics: &SharedBalloonDeviceMetrics,
    dispatches: impl IntoIterator<Item = &'a Arm64BootBalloonNotificationDispatch>,
    inactive_is_failure: bool,
) {
    for dispatch in dispatches {
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch(dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            if inactive_is_failure
                || !matches!(
                    source,
                    &VirtioBalloonDeviceNotificationError::Inactive { .. }
                )
            {
                metrics.record_event_failure();
            }
            if let Some(completed) = source.completed_notification_dispatch() {
                metrics.record_notification_dispatch(completed);
            }
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure();
        }
    }
}

fn record_balloon_signal_metrics(
    metrics: &SharedBalloonDeviceMetrics,
    dispatches: &HvfArm64BootBalloonNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            metrics.record_event_failure();
        }
    }
}

fn balloon_update_result_from_hvf_dispatches(
    dispatches: &HvfArm64BootBalloonNotificationDispatches,
) -> Result<(), BalloonUpdateError> {
    for dispatch in dispatches.as_slice() {
        if let Some(source) = dispatch.dispatch().outcome().handler_lookup_error() {
            return Err(balloon_update_error_from_display(source));
        }
        if let Some(source) = dispatch.dispatch().outcome().dispatch_error() {
            if matches!(
                source,
                &VirtioBalloonDeviceNotificationError::Inactive { .. }
            ) {
                continue;
            }
            return Err(balloon_update_error_from_display(source));
        }
        if let Some(source) = dispatch.signal_error() {
            return Err(balloon_update_error_from_display(source));
        }
    }

    Ok(())
}

#[cfg(test)]
fn record_entropy_dispatch_metrics(
    metrics: &SharedEntropyDeviceMetrics,
    dispatches: &HvfArm64BootEntropyNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootEntropyNotificationDispatch::dispatch);
    record_entropy_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_entropy_signal_metrics(metrics, dispatches);
}

fn record_entropy_runtime_dispatch_metrics<'a>(
    metrics: &SharedEntropyDeviceMetrics,
    dispatches: impl IntoIterator<Item = &'a Arm64BootEntropyNotificationDispatch>,
) {
    for dispatch in dispatches {
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch(dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error(source);
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure();
        }
        if dispatch.outcome().entropy_source_error().is_some() {
            metrics.record_entropy_source_provider_failure();
        }
    }
}

fn record_entropy_signal_metrics(
    metrics: &SharedEntropyDeviceMetrics,
    dispatches: &HvfArm64BootEntropyNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            metrics.record_event_failure();
        }
    }
}

fn dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
    backend: &mut HvfBackend,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    runtime_resources: &mut Arm64BootRuntimeResources,
    gic: &HvfGicMetadata,
    metrics: &SharedEntropyDeviceMetrics,
    entropy_source: &mut impl Arm64BootEntropySourceProvider,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    let dispatches = {
        let memory = backend.mapped_guest_memory_mut().map_err(|source| {
            HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
        })?;
        let mut mmio_dispatcher =
            lock_boot_mmio_dispatcher_runtime(dispatcher).map_err(|source| {
                HvfArm64BootEntropyNotificationDispatchError::MmioDispatcher { source }
            })?;

        dispatch_entropy_runtime_notifications_with_source(
            memory,
            runtime_resources,
            &mut mmio_dispatcher,
            entropy_source,
        )?
    };

    record_entropy_runtime_dispatch_metrics(metrics, dispatches.as_slice());
    let result = collect_or_signal_entropy_queue_interrupts(dispatches, gic);
    match &result {
        Ok(dispatches) => record_entropy_signal_metrics(metrics, dispatches),
        Err(_) => metrics.record_event_failure(),
    }
    result
}

fn dispatch_entropy_runtime_notifications_with_source(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    entropy_source: &mut impl Arm64BootEntropySourceProvider,
) -> Result<Arm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError> {
    runtime_resources
        .dispatch_entropy_queue_notifications_with_source(memory, mmio_dispatcher, entropy_source)
        .map_err(
            |source| HvfArm64BootEntropyNotificationDispatchError::DispatchNotifications { source },
        )
}

fn collect_or_signal_pmem_queue_interrupts(
    dispatches: Arm64BootPmemNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    if !dispatches.needs_queue_interrupt() {
        return collect_pmem_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic)
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::CreateSignalSink { source })?;

    signal_pmem_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_network_queue_interrupts(
    dispatches: Arm64BootNetworkNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    if !dispatches.needs_queue_interrupt() {
        return collect_network_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootNetworkNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_network_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_vsock_queue_interrupts(
    dispatches: Arm64BootVsockNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    if !dispatches.needs_queue_interrupt() {
        return collect_vsock_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootVsockNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_vsock_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_balloon_queue_interrupts(
    dispatches: Arm64BootBalloonNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError>
{
    if !dispatches.needs_queue_interrupt() {
        return collect_balloon_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootBalloonNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_balloon_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_memory_hotplug_queue_interrupts(
    dispatches: Arm64BootMemoryHotplugNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<
    HvfArm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    if !dispatches.needs_queue_interrupt() {
        return collect_memory_hotplug_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootMemoryHotplugNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_memory_hotplug_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_entropy_queue_interrupts(
    dispatches: Arm64BootEntropyNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    if !dispatches.needs_queue_interrupt() {
        return collect_entropy_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootEntropyNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_entropy_queue_interrupts(dispatches, &signaler)
}

fn signal_network_queue_interrupts(
    dispatches: Arm64BootNetworkNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootNetworkNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootNetworkNotificationDispatches::new(devices))
}

fn signal_vsock_queue_interrupts(
    dispatches: Arm64BootVsockNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootVsockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootVsockNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootVsockNotificationDispatches::new(devices))
}

fn signal_balloon_queue_interrupts(
    dispatches: Arm64BootBalloonNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBalloonNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootBalloonNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootBalloonNotificationDispatches::new(devices))
}

fn signal_memory_hotplug_queue_interrupts(
    dispatches: Arm64BootMemoryHotplugNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<
    HvfArm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootMemoryHotplugNotificationDispatchError::ResultAllocation {
                source,
            },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootMemoryHotplugNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootMemoryHotplugNotificationDispatches::new(
        devices,
    ))
}

fn signal_entropy_queue_interrupts(
    dispatches: Arm64BootEntropyNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootEntropyNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootEntropyNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootEntropyNotificationDispatches::new(devices))
}

fn signal_queue_interrupt(
    line: GuestInterruptLine,
    signaler: &dyn InterruptSink,
) -> Result<(), DeviceInterruptTriggerError> {
    signal_device_interrupt(line, DeviceInterruptKind::Queue, signaler)
}

fn replace_vmgenid_for_snapshot_restore(
    runner: &HvfVcpuRunner<'_>,
    backend: &mut HvfBackend,
    runtime_resources: &mut Arm64BootRuntimeResources,
    gic: HvfGicMetadata,
    interrupt_line: GuestInterruptLine,
) -> Result<(), HvfArm64BootVmGenIdRestoreError> {
    runner
        .ensure_snapshot_restore_available()
        .map_err(|source| HvfArm64BootVmGenIdRestoreError::RunnerPreflight { source })?;

    let signaler = HvfGicSpiSignaler::from_metadata(&gic)
        .map_err(|source| HvfArm64BootVmGenIdRestoreError::SignalerPreflight { source })?;
    signaler
        .validate_line(interrupt_line)
        .map_err(|source| HvfArm64BootVmGenIdRestoreError::SignalerPreflight { source })?;

    let memory = backend
        .mapped_guest_memory_mut()
        .map_err(|source| HvfArm64BootVmGenIdRestoreError::GuestMemory { source })?;
    replace_vmgenid_and_signal_with(
        memory,
        &mut runtime_resources.vmgenid_device,
        replace_arm64_boot_vmgenid,
        || signaler.set_level(interrupt_line, true),
    )
}

fn replace_vmgenid_and_signal_with(
    memory: &mut GuestMemory,
    device: &mut Arm64BootVmGenIdDevice,
    replace: impl FnOnce(
        &mut GuestMemory,
        &mut Arm64BootVmGenIdDevice,
    ) -> Result<(), Arm64BootVmGenIdReplacementError>,
    signal: impl FnOnce() -> Result<(), HvfGicSpiSignalError>,
) -> Result<(), HvfArm64BootVmGenIdRestoreError> {
    replace(memory, device)
        .map_err(|source| HvfArm64BootVmGenIdRestoreError::Replacement { source })?;
    signal().map_err(|source| HvfArm64BootVmGenIdRestoreError::Signal { source })
}

fn signal_device_interrupt(
    line: GuestInterruptLine,
    kind: DeviceInterruptKind,
    signaler: &dyn InterruptSink,
) -> Result<(), DeviceInterruptTriggerError> {
    signaler
        .signal(line)
        .map_err(|source| DeviceInterruptTriggerError::Signal { line, kind, source })
}

#[derive(Debug)]
pub enum HvfArm64BootSessionError {
    BackendAlreadyInitialized,
    UnsupportedVcpuCount {
        vcpu_count: u8,
    },
    CacheTopology {
        source: crate::cache::HvfArm64CacheTopologyError,
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
    StartTopology {
        source: HvfVcpuTopologyError,
    },
    StartDirtyTracking {
        source: HvfDirtyWriteTrackerStartError,
    },
    CpuTemplate {
        source: crate::cpu_template::HvfArm64CpuTemplateError,
    },
    PowerTopology,
    RunCoordinator {
        source: HvfVcpuRunCoordinatorError,
    },
    StartBlockRetryWakeupScheduler {
        source: io::Error,
    },
    StartPmemRetryWakeupScheduler {
        source: io::Error,
    },
    StartEntropyRetryWakeupScheduler {
        source: io::Error,
    },
    StartNetworkRetryWakeupScheduler {
        source: io::Error,
    },
    ReadMpidr {
        source: HvfVcpuRunnerError,
    },
    AssembleResources {
        source: Arm64BootResourceError,
    },
    RegisterBootTimerMmio {
        source: BootTimerMmioRegistrationError,
    },
    MissingPciValidationMsiSignaler,
    PciValidationDeviceType {
        source: VirtioDeviceTypeError,
    },
    PreparePciValidationInterrupts {
        source: HvfGicMsiDeviceInterruptResourceError,
    },
    PciValidationMmioDispatcherPoisoned,
    PublishPciValidationEndpoint {
        source: VirtioPciPublicationError,
    },
    PciData {
        source: HvfArm64BootPciDataError,
    },
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    ConfigureBootRegisters {
        source: HvfVcpuRunCoordinatorError,
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
            Self::CacheTopology { source } => {
                write!(f, "failed to admit HVF arm64 cache topology: {source}")
            }
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
            Self::StartTopology { source } => {
                write!(f, "failed to start HVF vCPU topology: {source}")
            }
            Self::StartDirtyTracking { source } => {
                write!(f, "failed to start HVF dirty tracking: {source}")
            }
            Self::CpuTemplate { source } => {
                write!(f, "failed to apply HVF arm64 CPU template: {source}")
            }
            Self::PowerTopology => f.write_str("failed to initialize HVF vCPU power topology"),
            Self::RunCoordinator { source } => {
                write!(f, "failed to initialize HVF vCPU run coordinator: {source}")
            }
            Self::StartBlockRetryWakeupScheduler { source } => {
                write!(
                    f,
                    "failed to start HVF block retry wakeup scheduler: {source}"
                )
            }
            Self::StartPmemRetryWakeupScheduler { source } => {
                write!(
                    f,
                    "failed to start HVF pmem retry wakeup scheduler: {source}"
                )
            }
            Self::StartEntropyRetryWakeupScheduler { source } => {
                write!(
                    f,
                    "failed to start HVF entropy retry wakeup scheduler: {source}"
                )
            }
            Self::StartNetworkRetryWakeupScheduler { source } => {
                write!(
                    f,
                    "failed to start HVF network retry wakeup scheduler: {source}"
                )
            }
            Self::ReadMpidr { source } => {
                write!(f, "failed to read primary vCPU MPIDR_EL1: {source}")
            }
            Self::AssembleResources { source } => {
                write!(f, "failed to assemble arm64 boot resources: {source}")
            }
            Self::RegisterBootTimerMmio { source } => {
                write!(f, "failed to register boot timer MMIO: {source}")
            }
            Self::MissingPciValidationMsiSignaler => f.write_str(
                "modern virtio-pci validation requires this VM's GICv2m signal capability",
            ),
            Self::PciValidationDeviceType { source } => {
                write!(
                    f,
                    "failed to prepare PCI validation virtio device type: {source}"
                )
            }
            Self::PreparePciValidationInterrupts { source } => write!(
                f,
                "failed to prepare PCI validation MSI-X interrupt resources: {source}"
            ),
            Self::PciValidationMmioDispatcherPoisoned => {
                f.write_str("PCI validation MMIO dispatcher state is unavailable")
            }
            Self::PublishPciValidationEndpoint { source } => {
                write!(f, "failed to publish PCI validation endpoint: {source}")
            }
            Self::PciData { source } => {
                write!(f, "failed to prepare PCI data devices: {source}")
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
            Self::CacheTopology { source } => Some(source),
            Self::CreateVm { source } => Some(source),
            Self::CreateGic { source } => Some(source),
            Self::TimerMetadata { source } => Some(source),
            Self::InterruptLineStorage { source } => Some(source),
            Self::AllocateInterruptLine { source, .. } => Some(source),
            Self::StartRunner { source } => Some(source),
            Self::StartTopology { source } => Some(source),
            Self::StartDirtyTracking { source } => Some(source),
            Self::CpuTemplate { source } => Some(source),
            Self::RunCoordinator { source } => Some(source),
            Self::StartBlockRetryWakeupScheduler { source } => Some(source),
            Self::StartPmemRetryWakeupScheduler { source } => Some(source),
            Self::StartEntropyRetryWakeupScheduler { source } => Some(source),
            Self::StartNetworkRetryWakeupScheduler { source } => Some(source),
            Self::ReadMpidr { source } => Some(source),
            Self::AssembleResources { source } => Some(source),
            Self::RegisterBootTimerMmio { source } => Some(source),
            Self::PciValidationDeviceType { source } => Some(source),
            Self::PreparePciValidationInterrupts { source } => Some(source),
            Self::PublishPciValidationEndpoint { source } => Some(source),
            Self::PciData { source } => Some(source),
            Self::MapGuestMemory { source } => Some(source),
            Self::ConfigureBootRegisters { source } => Some(source),
            Self::BackendAlreadyInitialized
            | Self::UnsupportedVcpuCount { .. }
            | Self::PowerTopology
            | Self::MissingPciValidationMsiSignaler
            | Self::PciValidationMmioDispatcherPoisoned => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootInterruptLinePurpose {
    BlockDevice,
    PmemDevice,
    NetworkDevice,
    VsockDevice,
    BalloonDevice,
    EntropyDevice,
    MemoryHotplugDevice,
    SerialDevice,
    VmGenIdDevice,
    VmClockDevice,
}

impl fmt::Display for HvfArm64BootInterruptLinePurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockDevice => f.write_str("block device"),
            Self::PmemDevice => f.write_str("pmem device"),
            Self::NetworkDevice => f.write_str("network device"),
            Self::VsockDevice => f.write_str("vsock device"),
            Self::BalloonDevice => f.write_str("balloon device"),
            Self::EntropyDevice => f.write_str("entropy device"),
            Self::MemoryHotplugDevice => f.write_str("memory hotplug device"),
            Self::SerialDevice => f.write_str("serial device"),
            Self::VmGenIdDevice => f.write_str("VMGenID device"),
            Self::VmClockDevice => f.write_str("VMClock device"),
        }
    }
}

#[derive(Debug)]
pub enum HvfArm64BootVmGenIdRestoreError {
    RunnerPreflight {
        source: HvfVcpuRunnerError,
    },
    SignalerPreflight {
        source: HvfGicSpiSignalError,
    },
    GuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    Replacement {
        source: Arm64BootVmGenIdReplacementError,
    },
    Signal {
        source: HvfGicSpiSignalError,
    },
}

impl fmt::Display for HvfArm64BootVmGenIdRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunnerPreflight { source } => {
                write!(f, "VMGenID restore runner preflight failed: {source}")
            }
            Self::SignalerPreflight { source } => {
                write!(f, "VMGenID restore signaler preflight failed: {source}")
            }
            Self::GuestMemory { source } => {
                write!(f, "VMGenID restore guest-memory access failed: {source}")
            }
            Self::Replacement { source } => {
                write!(f, "VMGenID restore replacement failed: {source}")
            }
            Self::Signal { source } => {
                write!(
                    f,
                    "VMGenID restore notification failed after replacement: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64BootVmGenIdRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RunnerPreflight { source } => Some(source),
            Self::SignalerPreflight { source } | Self::Signal { source } => Some(source),
            Self::GuestMemory { source } => Some(source),
            Self::Replacement { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64BootSessionShutdownError {
    Vcpu {
        source: HvfVcpuRunCoordinatorError,
    },
    PciValidation {
        source: HvfArm64BootPciValidationTeardownError,
    },
    PciData {
        source: HvfArm64BootPciDataError,
    },
    DestroyVm {
        source: BackendError,
    },
}

impl fmt::Display for HvfArm64BootSessionShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vcpu { source } => {
                write!(
                    f,
                    "failed to shut down HVF boot-session vCPU topology: {source}"
                )
            }
            Self::PciValidation { source } => {
                write!(f, "failed to tear down PCI validation endpoint: {source}")
            }
            Self::PciData { source } => {
                write!(f, "failed to tear down PCI data devices: {source}")
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
            Self::Vcpu { source } => Some(source),
            Self::PciValidation { source } => Some(source),
            Self::PciData { source } => Some(source),
            Self::DestroyVm { source } => Some(source),
        }
    }
}

#[derive(Debug)]
struct PreparedHvfArm64BootSession<'vm> {
    runner: HvfArm64BootVcpuSession<'vm>,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    runtime_resources: Arm64BootRuntimeResources,
    pci_validation_endpoint: Option<HvfArm64BootPciValidationEndpoint>,
    pci_data_devices: Option<HvfArm64BootPciDataDevices>,
    cache_source: crate::vcpu_config::HvfArm64VcpuCacheFdtSource,
    cache_hierarchy: Arm64FdtCacheHierarchy,
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    run_loop_wakeup: HvfArm64BootRunLoopWakeupToken,
    block_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    pmem_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    pmem_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    network_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    network_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    block_device_metrics: SharedBlockDeviceMetricsRegistry,
    pmem_device_metrics: SharedPmemDeviceMetricsRegistry,
    balloon_device_metrics: SharedBalloonDeviceMetrics,
    network_interface_metrics: SharedNetworkInterfaceMetricsRegistry,
    vsock_device_metrics: SharedVsockDeviceMetrics,
    entropy_device_metrics: SharedEntropyDeviceMetrics,
    gic: HvfGicMetadata,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    pmem_interrupt_lines: Vec<GuestInterruptLine>,
    network_interrupt_lines: Vec<GuestInterruptLine>,
    vsock_interrupt_line: Option<GuestInterruptLine>,
    balloon_interrupt_line: Option<GuestInterruptLine>,
    entropy_interrupt_line: Option<GuestInterruptLine>,
    memory_hotplug_interrupt_line: Option<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    vmgenid_interrupt_line: GuestInterruptLine,
    vmclock_interrupt_line: GuestInterruptLine,
    boot_registers: Option<HvfArm64BootRegisters>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HvfArm64BootInterruptLines {
    block: Vec<GuestInterruptLine>,
    pmem: Vec<GuestInterruptLine>,
    network: Vec<GuestInterruptLine>,
    vsock: Option<GuestInterruptLine>,
    balloon: Option<GuestInterruptLine>,
    entropy: Option<GuestInterruptLine>,
    memory_hotplug: Option<GuestInterruptLine>,
    serial: Option<GuestInterruptLine>,
    vmgenid: GuestInterruptLine,
    vmclock: GuestInterruptLine,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HvfArm64BootInterruptRequest {
    block_device_count: usize,
    pmem_device_count: usize,
    network_device_count: usize,
    vsock_configured: bool,
    balloon_configured: bool,
    entropy_configured: bool,
    memory_hotplug_configured: bool,
    serial_configured: bool,
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

        let prepared = match prepare_arm64_boot_session_parts(
            self,
            controller,
            config,
            VmStartupResources::default(),
        ) {
            Ok(prepared) => prepared,
            Err(err) => {
                let _ = <Self as VmBackend>::destroy_vm(self);
                return Err(err);
            }
        };

        Ok(HvfArm64BootSession {
            runner: prepared.runner,
            backend: self,
            mmio_dispatcher: prepared.mmio_dispatcher,
            runtime_resources: prepared.runtime_resources,
            pci_validation_endpoint: prepared.pci_validation_endpoint,
            pci_data_devices: prepared.pci_data_devices,
            cache_source: prepared.cache_source,
            cache_hierarchy: Some(prepared.cache_hierarchy),
            control_wakeup: prepared.control_wakeup,
            run_loop_wakeup: prepared.run_loop_wakeup,
            block_retry_wakeup: prepared.block_retry_wakeup,
            block_retry_wakeup_scheduler: prepared.block_retry_wakeup_scheduler,
            pmem_retry_wakeup: prepared.pmem_retry_wakeup,
            pmem_retry_wakeup_scheduler: prepared.pmem_retry_wakeup_scheduler,
            network_retry_wakeup: prepared.network_retry_wakeup,
            network_retry_wakeup_scheduler: prepared.network_retry_wakeup_scheduler,
            entropy_retry_wakeup: prepared.entropy_retry_wakeup,
            entropy_retry_wakeup_scheduler: prepared.entropy_retry_wakeup_scheduler,
            entropy_source: VirtioRngOsEntropySource::new(),
            block_device_metrics: prepared.block_device_metrics,
            pmem_device_metrics: prepared.pmem_device_metrics,
            balloon_device_metrics: prepared.balloon_device_metrics,
            network_interface_metrics: prepared.network_interface_metrics,
            vsock_device_metrics: prepared.vsock_device_metrics,
            entropy_device_metrics: prepared.entropy_device_metrics,
            gic: prepared.gic,
            block_interrupt_lines: prepared.block_interrupt_lines,
            pmem_interrupt_lines: prepared.pmem_interrupt_lines,
            network_interrupt_lines: prepared.network_interrupt_lines,
            vsock_interrupt_line: prepared.vsock_interrupt_line,
            balloon_interrupt_line: prepared.balloon_interrupt_line,
            entropy_interrupt_line: prepared.entropy_interrupt_line,
            memory_hotplug_interrupt_line: prepared.memory_hotplug_interrupt_line,
            serial_interrupt_line: prepared.serial_interrupt_line,
            vmgenid_interrupt_line: prepared.vmgenid_interrupt_line,
            vmclock_interrupt_line: prepared.vmclock_interrupt_line,
            boot_registers: prepared.boot_registers,
        })
    }
}

fn prepare_arm64_boot_session_parts<'vm>(
    backend: &mut HvfBackend,
    controller: &VmmController,
    config: HvfArm64BootSessionConfig,
    startup_resources: VmStartupResources,
) -> Result<PreparedHvfArm64BootSession<'vm>, HvfArm64BootSessionError> {
    prepare_arm64_boot_session_parts_with_cache(
        backend,
        controller,
        config,
        startup_resources,
        crate::cache::prepare_arm64_cache,
    )
}

fn prepare_arm64_boot_session_parts_with_cache<'vm>(
    backend: &mut HvfBackend,
    controller: &VmmController,
    config: HvfArm64BootSessionConfig,
    startup_resources: VmStartupResources,
    prepare_cache: impl FnOnce(
        u8,
    ) -> Result<
        crate::cache::PreparedHvfArm64Cache,
        crate::cache::HvfArm64CacheTopologyError,
    >,
) -> Result<PreparedHvfArm64BootSession<'vm>, HvfArm64BootSessionError> {
    let cpu_template = controller
        .custom_cpu_template()
        .map(crate::cpu_template::PreparedHvfArm64CpuTemplate::from_runtime)
        .transpose()
        .map_err(|source| HvfArm64BootSessionError::CpuTemplate { source })?;
    let prepared_cache = prepare_cache(controller.machine_config().vcpu_count())
        .map_err(|source| HvfArm64BootSessionError::CacheTopology { source })?;
    let (cache_source, cache_hierarchy) = prepared_cache.into_parts();
    let retained_cache_hierarchy = cache_hierarchy.clone();
    if config.pci_enabled && config.pci_validation.is_some() {
        return Err(HvfArm64BootSessionError::PciData {
            source: HvfArm64BootPciDataError::new(
                "production PCI transport cannot be combined with an internal PCI validation selector",
            ),
        });
    }
    let pci_validation = if config.pci_enabled {
        Some(Arm64BootPciValidationConfig::all_virtio_devices())
    } else {
        config.pci_validation
    };
    let pci_all_virtio =
        pci_validation.is_some_and(Arm64BootPciValidationConfig::is_all_virtio_devices);
    let gic_msi = if pci_all_virtio {
        Some(
            pci_all_virtio_gic_msi_configuration(controller, &config)
                .map_err(|source| HvfArm64BootSessionError::PciData { source })?,
        )
    } else {
        config.gic_msi
    };
    <HvfBackend as VmBackend>::create_vm(backend)
        .map_err(|source| HvfArm64BootSessionError::CreateVm { source })?;
    let gic = *match gic_msi {
        Some(configuration) => backend.create_gic_with_msi(configuration),
        None => backend.create_gic(),
    }
    .map_err(|source| HvfArm64BootSessionError::CreateGic { source })?;
    let timer = gic
        .arm64_fdt_timer_interrupts()
        .map_err(|source| HvfArm64BootSessionError::TimerMetadata { source })?;
    let pci_data_mode = pci_validation.is_some_and(|validation| {
        validation.is_data_devices() || validation.is_all_virtio_devices()
    });
    let interrupt_lines = allocate_interrupt_lines(
        &gic,
        HvfArm64BootInterruptRequest {
            block_device_count: if pci_data_mode {
                0
            } else {
                controller.drive_configs().len()
            },
            pmem_device_count: if pci_data_mode {
                0
            } else {
                controller.pmem_configs().len()
            },
            network_device_count: if pci_data_mode {
                0
            } else {
                controller.network_interface_configs().len()
            },
            vsock_configured: controller.vsock_config().is_some() && !pci_all_virtio,
            balloon_configured: controller.balloon_config().is_some()
                && config.balloon_device.is_some()
                && !pci_all_virtio,
            entropy_configured: config.entropy_device.is_some() && !pci_all_virtio,
            memory_hotplug_configured: controller.memory_hotplug_config().is_some()
                && config.memory_hotplug_device.is_some()
                && !pci_all_virtio,
            serial_configured: config.serial_device.is_some(),
        },
    )?;

    let mpidrs = prepare_ordered_mpidrs(controller.machine_config().vcpu_count())
        .map_err(|source| HvfArm64BootSessionError::StartTopology { source })?;
    let power = PsciCpuPowerCoordinator::new(&mpidrs)
        .map_err(|_| HvfArm64BootSessionError::PowerTopology)?;
    let runtime_serial = config
        .serial_device
        .zip(interrupt_lines.serial)
        .map(|(serial, interrupt_line)| serial.into_runtime(interrupt_line));
    let runtime_entropy = if pci_all_virtio {
        config
            .entropy_device
            .map(|entropy| RuntimeArm64BootEntropyDeviceConfig::for_pci(entropy.mmio_layout))
    } else {
        config
            .entropy_device
            .zip(interrupt_lines.entropy)
            .map(|(entropy, interrupt_line)| entropy.into_runtime(interrupt_line))
    };
    let runtime_memory_hotplug = if pci_all_virtio {
        config.memory_hotplug_device.map(|memory_hotplug| {
            RuntimeArm64BootMemoryHotplugDeviceConfig::for_pci(memory_hotplug.mmio_layout)
        })
    } else {
        config
            .memory_hotplug_device
            .zip(interrupt_lines.memory_hotplug)
            .map(|(memory_hotplug, interrupt_line)| memory_hotplug.into_runtime(interrupt_line))
    };
    let resources =
        Arm64BootResources::assemble_from_controller_with_startup_resources_and_pci_validation(
            controller,
            Arm64BootResourceConfig {
                vcpu_mpidrs: &mpidrs,
                cache_hierarchy,
                gic: gic.arm64_fdt_gic(),
                timer,
                rtc_device: Some(RuntimeArm64BootRtcDeviceConfig::new(config.rtc_mmio_layout)),
                serial_device: runtime_serial,
                vmgenid_interrupt_line: interrupt_lines.vmgenid,
                vmclock_interrupt_line: interrupt_lines.vmclock,
                block_mmio_layout: config.block_mmio_layout,
                block_interrupt_lines: &interrupt_lines.block,
                pmem_mmio_layout: config.pmem_mmio_layout,
                pmem_interrupt_lines: &interrupt_lines.pmem,
                network_mmio_layout: config.network_mmio_layout,
                network_interrupt_lines: &interrupt_lines.network,
                vsock_mmio_layout: config.vsock_mmio_layout,
                vsock_interrupt_line: interrupt_lines.vsock,
                balloon_mmio_layout: config
                    .balloon_device
                    .map(|balloon| balloon.mmio_layout)
                    .unwrap_or_else(|| {
                        BalloonMmioLayout::new(GuestAddress::new(0), MmioRegionId::new(0))
                    }),
                balloon_interrupt_line: interrupt_lines.balloon,
                memory_hotplug_device: runtime_memory_hotplug,
                entropy_device: runtime_entropy,
            },
            startup_resources,
            pci_validation,
        )
        .map_err(|source| HvfArm64BootSessionError::AssembleResources { source })?;
    let boot_registers = HvfArm64BootRegisters {
        kernel_entry: resources.loaded_boot_source.kernel.entry_address,
        fdt_address: resources.fdt.address,
    };
    let Arm64BootResourceParts {
        memory,
        mut mmio_dispatcher,
        mut runtime,
    } = resources.into_parts();
    if let Some(boot_timer) = config.boot_timer_device {
        register_boot_timer_mmio(
            &mut mmio_dispatcher,
            boot_timer.mmio_layout,
            controller.boot_timer_logger(),
        )
        .map_err(|source| HvfArm64BootSessionError::RegisterBootTimerMmio { source })?;
    }

    backend
        .map_guest_memory_with_pmem_devices(
            memory,
            runtime.pmem_devices.as_slice(),
            HvfMemoryPermissions::GUEST_RAM,
        )
        .map_err(|source| HvfArm64BootSessionError::MapGuestMemory { source })?;
    if controller.machine_config().track_dirty_pages() {
        backend
            .start_dirty_write_tracking()
            .map_err(|source| HvfArm64BootSessionError::StartDirtyTracking { source })?;
    }
    let topology = backend
        .start_session_vcpu_topology(controller.machine_config().vcpu_count())
        .map_err(|source| HvfArm64BootSessionError::StartTopology { source })?;
    debug_assert_eq!(topology.mpidrs(), mpidrs);
    if let Some(template) = cpu_template.as_ref() {
        topology
            .apply_arm64_cpu_template(template)
            .map_err(|source| HvfArm64BootSessionError::CpuTemplate { source })?;
    }
    let mmio_dispatcher = Arc::new(Mutex::new(mmio_dispatcher));
    let coordinator = topology
        .into_run_coordinator(Arc::clone(&mmio_dispatcher), &[0])
        .map_err(|source| HvfArm64BootSessionError::RunCoordinator { source })?;
    coordinator
        .configure_arm64_boot_registers(0, boot_registers)
        .map_err(|source| HvfArm64BootSessionError::ConfigureBootRegisters { source })?;
    let runner = HvfArm64BootVcpuSession::new(
        coordinator,
        power,
        gic.timer_interrupts.el1_virtual_timer_intid,
    );
    let runtime_pci_hotplug = runtime
        .pci_validation
        .as_ref()
        .is_some_and(|validation| validation.config().is_all_virtio_devices());
    let block_device_metrics = if runtime_pci_hotplug {
        SharedBlockDeviceMetricsRegistry::from_drive_ids_with_capacity(
            runtime
                .block_devices
                .iter()
                .map(|device| device.registration.drive_id())
                .chain(
                    runtime
                        .pci_block_devices
                        .iter()
                        .map(|device| device.drive_id()),
                ),
            PCI_ENDPOINT_SLOT_COUNT,
        )
        .map_err(|source| HvfArm64BootSessionError::PciData {
            source: HvfArm64BootPciDataError::new(format!(
                "failed to reserve live block metrics inventory: {source}"
            )),
        })?
    } else {
        SharedBlockDeviceMetricsRegistry::from_drive_ids(
            runtime
                .block_devices
                .iter()
                .map(|device| device.registration.drive_id())
                .chain(
                    runtime
                        .pci_block_devices
                        .iter()
                        .map(|device| device.drive_id()),
                ),
        )
    };
    let pmem_device_metrics = if runtime_pci_hotplug {
        SharedPmemDeviceMetricsRegistry::from_device_ids_with_capacity(
            runtime.pmem_devices.iter().map(|device| device.id()),
            PCI_ENDPOINT_SLOT_COUNT,
        )
        .map_err(|source| HvfArm64BootSessionError::PciData {
            source: HvfArm64BootPciDataError::new(format!(
                "failed to reserve live pmem metrics inventory: {source}"
            )),
        })?
    } else {
        SharedPmemDeviceMetricsRegistry::from_device_ids(
            runtime.pmem_devices.iter().map(|device| device.id()),
        )
    };
    let network_interface_ids = runtime
        .network_devices
        .iter()
        .map(|device| device.registration.iface_id())
        .chain(
            runtime
                .pci_network_devices
                .iter()
                .map(|device| device.iface_id()),
        );
    let network_interface_metrics = if runtime_pci_hotplug {
        SharedNetworkInterfaceMetricsRegistry::from_interface_ids_with_capacity(
            network_interface_ids,
            PCI_ENDPOINT_SLOT_COUNT,
        )
        .map_err(|source| HvfArm64BootSessionError::PciData {
            source: HvfArm64BootPciDataError::new(format!(
                "failed to reserve live network metrics inventory: {source}"
            )),
        })?
    } else {
        SharedNetworkInterfaceMetricsRegistry::from_interface_ids(network_interface_ids)
    };
    let has_block_devices =
        !runtime.block_devices.is_empty() || !runtime.pci_block_devices.is_empty();
    let has_network_devices =
        !runtime.network_devices.is_empty() || !runtime.pci_network_devices.is_empty();
    let pci_data_devices = prepare_pci_data_devices(
        backend,
        &mut runtime,
        &mmio_dispatcher,
        &block_device_metrics,
        &network_interface_metrics,
        &pmem_device_metrics,
    )
    .map_err(|source| HvfArm64BootSessionError::PciData { source })?;
    let block_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
    let block_retry_wakeup_scheduler = if !has_block_devices && !runtime_pci_hotplug {
        HvfArm64BootLimiterRetryWakeupScheduler::inactive()
    } else {
        let vcpu_control = runner.control();
        HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            BLOCK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            block_retry_wakeup.clone(),
            move || vcpu_control.request_wakeup(),
        )
        .map_err(|source| HvfArm64BootSessionError::StartBlockRetryWakeupScheduler { source })?
    };
    let pmem_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
    let pmem_retry_wakeup_scheduler = if runtime.pmem_devices.is_empty() && !runtime_pci_hotplug {
        HvfArm64BootLimiterRetryWakeupScheduler::inactive()
    } else {
        let vcpu_control = runner.control();
        HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            PMEM_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            pmem_retry_wakeup.clone(),
            move || vcpu_control.request_wakeup(),
        )
        .map_err(|source| HvfArm64BootSessionError::StartPmemRetryWakeupScheduler { source })?
    };
    let entropy_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
    let entropy_retry_wakeup_scheduler = if runtime.entropy_device.is_none()
        && pci_data_devices
            .as_ref()
            .is_none_or(|devices| devices.entropy.is_none())
    {
        HvfArm64BootLimiterRetryWakeupScheduler::inactive()
    } else {
        let vcpu_control = runner.control();
        HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            ENTROPY_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            entropy_retry_wakeup.clone(),
            move || vcpu_control.request_wakeup(),
        )
        .map_err(|source| HvfArm64BootSessionError::StartEntropyRetryWakeupScheduler { source })?
    };
    let network_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
    let network_retry_wakeup_scheduler = if !has_network_devices && !runtime_pci_hotplug {
        HvfArm64BootLimiterRetryWakeupScheduler::inactive()
    } else {
        let vcpu_control = runner.control();
        HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            NETWORK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            network_retry_wakeup.clone(),
            move || vcpu_control.request_wakeup(),
        )
        .map_err(|source| HvfArm64BootSessionError::StartNetworkRetryWakeupScheduler { source })?
    };
    let pci_validation_endpoint =
        prepare_pci_validation_virtio_rng_endpoint(backend, &mut runtime, &mmio_dispatcher)?;

    Ok(PreparedHvfArm64BootSession {
        runner,
        mmio_dispatcher,
        runtime_resources: runtime,
        pci_validation_endpoint,
        pci_data_devices,
        cache_source,
        cache_hierarchy: retained_cache_hierarchy,
        control_wakeup: HvfArm64BootRunLoopControlWakeupToken::default(),
        run_loop_wakeup: HvfArm64BootRunLoopWakeupToken::default(),
        block_retry_wakeup,
        block_retry_wakeup_scheduler,
        pmem_retry_wakeup,
        pmem_retry_wakeup_scheduler,
        network_retry_wakeup,
        network_retry_wakeup_scheduler,
        entropy_retry_wakeup,
        entropy_retry_wakeup_scheduler,
        block_device_metrics,
        pmem_device_metrics,
        balloon_device_metrics: SharedBalloonDeviceMetrics::default(),
        network_interface_metrics,
        vsock_device_metrics: SharedVsockDeviceMetrics::default(),
        entropy_device_metrics: SharedEntropyDeviceMetrics::default(),
        gic,
        block_interrupt_lines: interrupt_lines.block,
        pmem_interrupt_lines: interrupt_lines.pmem,
        network_interrupt_lines: interrupt_lines.network,
        vsock_interrupt_line: interrupt_lines.vsock,
        balloon_interrupt_line: interrupt_lines.balloon,
        entropy_interrupt_line: interrupt_lines.entropy,
        memory_hotplug_interrupt_line: interrupt_lines.memory_hotplug,
        serial_interrupt_line: interrupt_lines.serial,
        vmgenid_interrupt_line: interrupt_lines.vmgenid,
        vmclock_interrupt_line: interrupt_lines.vmclock,
        boot_registers: Some(boot_registers),
    })
}

fn prepare_pci_validation_virtio_rng_endpoint(
    backend: &HvfBackend,
    runtime: &mut Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
) -> Result<Option<HvfArm64BootPciValidationEndpoint>, HvfArm64BootSessionError> {
    let Some(validation) = runtime.pci_validation.as_mut() else {
        return Ok(None);
    };
    if !validation.config().is_modern_virtio_rng() {
        return Ok(None);
    }

    let signaler = backend
        .gic_msi_signaler()
        .ok_or(HvfArm64BootSessionError::MissingPciValidationMsiSignaler)?;
    let device_type = VirtioDeviceType::new(VIRTIO_RNG_DEVICE_ID)
        .map_err(|source| HvfArm64BootSessionError::PciValidationDeviceType { source })?;
    let device = SharedPciValidationVirtioRngDevice::new();
    let segment = validation.segment().clone();
    let mut dispatcher = dispatcher
        .lock()
        .map_err(|_| HvfArm64BootSessionError::PciValidationMmioDispatcherPoisoned)?;
    let interrupts = HvfGicMsiDeviceInterruptResources::allocate(
        signaler,
        PCI_VALIDATION_VIRTIO_RNG_VECTOR_COUNT,
    )
    .map_err(|source| HvfArm64BootSessionError::PreparePciValidationInterrupts { source })?;
    let published = PublishedVirtioPciEndpoint::publish(
        VirtioPciIdentity::new(device_type, VIRTIO_MMIO_VERSION_1_FEATURE),
        &VIRTIO_RNG_QUEUE_SIZES,
        UnsupportedVirtioDeviceConfig,
        device.clone(),
        false,
        validation.bar_allocator_mut(),
        segment,
        &mut dispatcher,
        PCI_VALIDATION_VIRTIO_RNG_BAR_REGION_ID,
        interrupts,
    )
    .map_err(|source| HvfArm64BootSessionError::PublishPciValidationEndpoint { source })?;

    Ok(Some(HvfArm64BootPciValidationEndpoint {
        published,
        device,
        queue_deliveries: 0,
        config_deliveries: 0,
        config_interrupt_triggered: false,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HvfArm64BootPciDataResourceDemand {
    endpoints: usize,
    routes: usize,
}

fn pci_data_resource_demand(
    block_count: usize,
    network_count: usize,
    pmem_count: usize,
) -> Result<HvfArm64BootPciDataResourceDemand, HvfArm64BootPciDataError> {
    let endpoints = block_count
        .checked_add(network_count)
        .and_then(|count| count.checked_add(pmem_count))
        .ok_or_else(|| HvfArm64BootPciDataError::new("PCI data endpoint count overflowed"))?;
    let routes = block_count
        .checked_mul(VIRTIO_BLOCK_QUEUE_SIZES.len() + 1)
        .and_then(|count| {
            network_count
                .checked_mul(VIRTIO_NET_QUEUE_SIZES.len() + 1)
                .and_then(|routes| count.checked_add(routes))
        })
        .and_then(|count| {
            pmem_count
                .checked_mul(VIRTIO_PMEM_QUEUE_SIZES.len() + 1)
                .and_then(|routes| count.checked_add(routes))
        })
        .ok_or_else(|| HvfArm64BootPciDataError::new("PCI data MSI-X route count overflowed"))?;
    Ok(HvfArm64BootPciDataResourceDemand { endpoints, routes })
}

fn pci_all_virtio_resource_demand(
    block_count: usize,
    network_count: usize,
    pmem_count: usize,
    balloon_queue_count: Option<usize>,
    vsock_configured: bool,
    entropy_configured: bool,
    memory_hotplug_configured: bool,
) -> Result<HvfArm64BootPciDataResourceDemand, HvfArm64BootPciDataError> {
    let mut demand = pci_data_resource_demand(block_count, network_count, pmem_count)?;
    let balloon_routes = balloon_queue_count
        .map(|count| {
            count.checked_add(1).ok_or_else(|| {
                HvfArm64BootPciDataError::new("PCI all-virtio balloon MSI-X route count overflowed")
            })
        })
        .transpose()?;
    for routes in [
        balloon_routes,
        vsock_configured.then_some(VIRTIO_VSOCK_QUEUE_SIZES.len() + 1),
        entropy_configured.then_some(VIRTIO_RNG_QUEUE_SIZES.len() + 1),
        memory_hotplug_configured.then_some(VIRTIO_MEM_QUEUE_SIZES.len() + 1),
    ]
    .into_iter()
    .flatten()
    {
        demand.endpoints = demand.endpoints.checked_add(1).ok_or_else(|| {
            HvfArm64BootPciDataError::new("PCI all-virtio endpoint count overflowed")
        })?;
        demand.routes = demand.routes.checked_add(routes).ok_or_else(|| {
            HvfArm64BootPciDataError::new("PCI all-virtio MSI-X route count overflowed")
        })?;
    }
    Ok(demand)
}

fn pci_all_virtio_gic_msi_configuration(
    controller: &VmmController,
    config: &HvfArm64BootSessionConfig,
) -> Result<HvfGicMsiConfiguration, HvfArm64BootPciDataError> {
    let balloon_queue_count = controller
        .balloon_config()
        .map(|balloon| VirtioBalloonQueueLayout::from_config(balloon).queue_count());
    let startup_demand = pci_all_virtio_resource_demand(
        controller.drive_configs().len(),
        controller.network_interface_configs().len(),
        controller.pmem_configs().len(),
        balloon_queue_count,
        controller.vsock_config().is_some(),
        config.entropy_device.is_some(),
        controller.memory_hotplug_config().is_some(),
    )?;
    if startup_demand.endpoints > PCI_ENDPOINT_SLOT_COUNT {
        return Err(HvfArm64BootPciDataError::new(format!(
            "PCI all-virtio endpoint count {} exceeds segment capacity {PCI_ENDPOINT_SLOT_COUNT}",
            startup_demand.endpoints
        )));
    }
    let fixed_demand = pci_all_virtio_resource_demand(
        0,
        0,
        0,
        balloon_queue_count,
        controller.vsock_config().is_some(),
        config.entropy_device.is_some(),
        controller.memory_hotplug_config().is_some(),
    )?;
    let dynamic_slots = PCI_ENDPOINT_SLOT_COUNT
        .checked_sub(fixed_demand.endpoints)
        .ok_or_else(|| {
            HvfArm64BootPciDataError::new("fixed PCI all-virtio endpoints exceed segment capacity")
        })?;
    let dynamic_routes = dynamic_slots
        .checked_mul(VIRTIO_NET_QUEUE_SIZES.len() + 1)
        .ok_or_else(|| {
            HvfArm64BootPciDataError::new("PCI all-virtio hotplug route count overflowed")
        })?;
    let routes = fixed_demand
        .routes
        .checked_add(dynamic_routes)
        .ok_or_else(|| {
            HvfArm64BootPciDataError::new("PCI all-virtio hotplug route count overflowed")
        })?;
    debug_assert!(routes >= startup_demand.routes);
    let routes = u32::try_from(routes.max(1)).map_err(|_| {
        HvfArm64BootPciDataError::new("PCI all-virtio MSI-X route count does not fit u32")
    })?;
    let routes = NonZeroU32::new(routes)
        .ok_or_else(|| HvfArm64BootPciDataError::new("PCI all-virtio MSI-X route count is zero"))?;
    Ok(HvfGicMsiConfiguration::new(routes))
}

fn pci_data_available_bar_count(
    allocator: &PciBarAllocator,
) -> Result<usize, HvfArm64BootPciDataError> {
    if allocator.address_space() != PciBarAddressSpace::Memory64 {
        return Err(HvfArm64BootPciDataError::new(
            "PCI data capability BAR allocator is not 64-bit memory",
        ));
    }

    allocator
        .available_ranges()
        .iter()
        .try_fold(0usize, |total, range| {
            let start = range.start().raw_value();
            let remainder = start % VIRTIO_PCI_CAPABILITY_BAR_SIZE;
            let aligned_start = if remainder == 0 {
                start
            } else {
                start
                    .checked_add(VIRTIO_PCI_CAPABILITY_BAR_SIZE - remainder)
                    .ok_or_else(|| {
                        HvfArm64BootPciDataError::new(
                            "PCI data capability BAR alignment overflowed",
                        )
                    })?
            };
            let count = range
                .end_exclusive()
                .raw_value()
                .saturating_sub(aligned_start)
                / VIRTIO_PCI_CAPABILITY_BAR_SIZE;
            let count = usize::try_from(count).map_err(|_| {
                HvfArm64BootPciDataError::new("PCI data capability BAR count does not fit usize")
            })?;
            total.checked_add(count).ok_or_else(|| {
                HvfArm64BootPciDataError::new("PCI data capability BAR count overflowed")
            })
        })
}

fn pci_data_bar_plan(
    allocator: &PciBarAllocator,
    endpoint_count: usize,
) -> Result<Vec<GuestMemoryRange>, HvfArm64BootPciDataError> {
    let mut plan = Vec::new();
    plan.try_reserve_exact(endpoint_count).map_err(|source| {
        HvfArm64BootPciDataError::new(format!(
            "failed to reserve PCI data capability BAR preflight plan: {source}"
        ))
    })?;
    if endpoint_count == 0 {
        return Ok(plan);
    }
    for range in allocator.available_ranges() {
        let start = range.start().raw_value();
        let remainder = start % VIRTIO_PCI_CAPABILITY_BAR_SIZE;
        let mut next = if remainder == 0 {
            start
        } else {
            start
                .checked_add(VIRTIO_PCI_CAPABILITY_BAR_SIZE - remainder)
                .ok_or_else(|| {
                    HvfArm64BootPciDataError::new("PCI data capability BAR alignment overflowed")
                })?
        };
        while plan.len() < endpoint_count {
            let end = next
                .checked_add(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                .ok_or_else(|| {
                    HvfArm64BootPciDataError::new("PCI data capability BAR plan overflowed")
                })?;
            if end > range.end_exclusive().raw_value() {
                break;
            }
            plan.push(
                GuestMemoryRange::new(GuestAddress::new(next), VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                    .map_err(|source| {
                        HvfArm64BootPciDataError::new(format!(
                            "failed to build PCI data capability BAR plan: {source}"
                        ))
                    })?,
            );
            next = end;
        }
        if plan.len() == endpoint_count {
            return Ok(plan);
        }
    }
    Err(HvfArm64BootPciDataError::new(format!(
        "PCI data endpoint count {endpoint_count} exceeds plannable capability BAR capacity {}",
        plan.len()
    )))
}

fn preflight_pci_data_dispatcher(
    dispatcher: &MmioDispatcher,
    bar_plan: &[GuestMemoryRange],
) -> Result<(), HvfArm64BootPciDataError> {
    for (endpoint_index, bar) in bar_plan.iter().copied().enumerate() {
        let region_id = pci_data_region_id(endpoint_index)?;
        if dispatcher.contains_region_or_handler(region_id) {
            return Err(HvfArm64BootPciDataError::new(format!(
                "PCI data MMIO region id {region_id} is already in use"
            )));
        }
        if dispatcher
            .regions()
            .iter()
            .any(|region| region.range().overlaps(bar))
        {
            return Err(HvfArm64BootPciDataError::new(format!(
                "PCI data capability BAR {endpoint_index} overlaps an existing MMIO region"
            )));
        }
    }
    Ok(())
}

fn prepare_pci_data_devices(
    backend: &HvfBackend,
    runtime: &mut Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    block_device_metrics: &SharedBlockDeviceMetricsRegistry,
    network_interface_metrics: &SharedNetworkInterfaceMetricsRegistry,
    pmem_device_metrics: &SharedPmemDeviceMetricsRegistry,
) -> Result<Option<HvfArm64BootPciDataDevices>, HvfArm64BootPciDataError> {
    let Some(validation) = runtime.pci_validation.as_ref() else {
        return Ok(None);
    };
    let all_virtio = validation.config().is_all_virtio_devices();
    if !validation.config().is_data_devices() && !all_virtio {
        return Ok(None);
    }

    let block_count = runtime.pci_block_devices.len();
    let network_count = runtime.pci_network_devices.len();
    let pmem_count = runtime.pmem_devices.len();
    let demand = if all_virtio {
        pci_all_virtio_resource_demand(
            block_count,
            network_count,
            pmem_count,
            runtime
                .pci_balloon_device
                .as_ref()
                .map(|prepared| prepared.queue_sizes().len()),
            runtime.pci_vsock_device.is_some(),
            runtime.pci_entropy_device.is_some(),
            runtime.pci_memory_hotplug_device.is_some(),
        )?
    } else {
        pci_data_resource_demand(block_count, network_count, pmem_count)?
    };
    let available_slots = validation
        .segment()
        .with_segment(|segment| segment.available_endpoint_slots())
        .map_err(|source| {
            HvfArm64BootPciDataError::new(format!(
                "failed to preflight PCI data endpoint slots: {source}"
            ))
        })?;
    let reserved_endpoint_capacity = if all_virtio {
        PCI_ENDPOINT_SLOT_COUNT
    } else {
        demand.endpoints
    };
    if reserved_endpoint_capacity > available_slots {
        return Err(HvfArm64BootPciDataError::new(format!(
            "PCI data endpoint reservation {reserved_endpoint_capacity} exceeds available segment capacity {available_slots}"
        )));
    }
    let available_bars = pci_data_available_bar_count(validation.bar_allocator())?;
    if reserved_endpoint_capacity > available_bars {
        return Err(HvfArm64BootPciDataError::new(format!(
            "PCI data endpoint reservation {reserved_endpoint_capacity} exceeds available capability BAR capacity {available_bars}"
        )));
    }
    let bar_plan = pci_data_bar_plan(validation.bar_allocator(), reserved_endpoint_capacity)?;
    {
        let dispatcher = dispatcher.lock().map_err(|_| {
            HvfArm64BootPciDataError::new(
                "PCI data-device MMIO dispatcher is unavailable during preflight",
            )
        })?;
        preflight_pci_data_dispatcher(&dispatcher, &bar_plan)?;
    }

    let signaler = backend.gic_msi_signaler().ok_or_else(|| {
        HvfArm64BootPciDataError::new("PCI data devices require a GICv2m MSI signaler")
    })?;
    let remaining_routes = usize::try_from(signaler.allocator().remaining()).unwrap_or(usize::MAX);
    if demand.routes > remaining_routes {
        return Err(HvfArm64BootPciDataError::new(format!(
            "PCI data route demand {} exceeds remaining GICv2m capacity {remaining_routes}",
            demand.routes
        )));
    }
    let reserved_routes = if all_virtio {
        remaining_routes
    } else {
        demand.routes
    };

    if all_virtio && runtime.pmem_devices.len() < PCI_ENDPOINT_SLOT_COUNT {
        runtime
            .pmem_devices
            .try_reserve_exact(PCI_ENDPOINT_SLOT_COUNT - runtime.pmem_devices.len())
            .map_err(|source| {
                HvfArm64BootPciDataError::new(format!(
                    "failed to reserve runtime pmem backing inventory: {source}"
                ))
            })?;
    }
    let static_range_capacity = runtime
        .layout
        .ranges()
        .len()
        .checked_add(usize::from(runtime.pci_memory_hotplug_device.is_some()))
        .ok_or_else(|| {
            HvfArm64BootPciDataError::new("runtime pmem reserved-range capacity overflow")
        })?;
    let mut pmem_static_reserved_ranges = Vec::new();
    pmem_static_reserved_ranges
        .try_reserve_exact(static_range_capacity)
        .map_err(|source| {
            HvfArm64BootPciDataError::new(format!(
                "failed to reserve runtime pmem guest-range inventory: {source}"
            ))
        })?;
    pmem_static_reserved_ranges.extend(runtime.layout.ranges().iter().copied());
    if let Some(memory_hotplug) = runtime.pci_memory_hotplug_device.as_ref() {
        let config = memory_hotplug.config_space();
        let range = GuestMemoryRange::new(GuestAddress::new(config.addr()), config.region_size())
            .map_err(|source| {
            HvfArm64BootPciDataError::new(format!(
                "failed to retain the virtio-mem reservation for runtime pmem: {source}"
            ))
        })?;
        pmem_static_reserved_ranges.push(range);
    }

    let validation = runtime.pci_validation.take().ok_or_else(|| {
        HvfArm64BootPciDataError::new("PCI data validation resources disappeared during prepare")
    })?;
    let mut manager = HvfArm64BootPciDataDevices {
        validation,
        dispatcher: Arc::clone(dispatcher),
        msi_interrupts: None,
        balloon: None,
        block: Vec::new(),
        network: Vec::new(),
        pmem: Vec::new(),
        vsock: None,
        entropy: None,
        memory_hotplug: None,
        pmem_static_reserved_ranges,
        runtime_hotplug: all_virtio,
    };
    if reserved_routes != 0 {
        manager.msi_interrupts = Some(
            HvfGicMsiDeviceInterruptResources::allocate(signaler, reserved_routes).map_err(
                |source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to allocate shared PCI data MSI-X routes: {source}"
                    ))
                },
            )?,
        );
    }
    manager
        .block
        .try_reserve_exact(if all_virtio {
            PCI_ENDPOINT_SLOT_COUNT
        } else {
            block_count
        })
        .map_err(|source| {
            HvfArm64BootPciDataError::new(format!(
                "failed to reserve PCI block endpoint storage: {source}"
            ))
        })?;
    manager
        .network
        .try_reserve_exact(if all_virtio {
            PCI_ENDPOINT_SLOT_COUNT
        } else {
            network_count
        })
        .map_err(|source| {
            HvfArm64BootPciDataError::new(format!(
                "failed to reserve PCI network endpoint storage: {source}"
            ))
        })?;
    manager
        .pmem
        .try_reserve_exact(if all_virtio {
            PCI_ENDPOINT_SLOT_COUNT
        } else {
            pmem_count
        })
        .map_err(|source| {
            HvfArm64BootPciDataError::new(format!(
                "failed to reserve PCI pmem endpoint storage: {source}"
            ))
        })?;

    let block_type = VirtioDeviceType::new(VIRTIO_BLOCK_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let network_type = VirtioDeviceType::new(VIRTIO_NET_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let pmem_type = VirtioDeviceType::new(VIRTIO_PMEM_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let balloon_type = VirtioDeviceType::new(VIRTIO_BALLOON_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let vsock_type = VirtioDeviceType::new(VIRTIO_VSOCK_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let entropy_type = VirtioDeviceType::new(VIRTIO_RNG_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let memory_hotplug_type = VirtioDeviceType::new(VIRTIO_MEM_DEVICE_ID)
        .map_err(|source| HvfArm64BootPciDataError::new(source.to_string()))?;
    let balloon = runtime.pci_balloon_device.take();
    let blocks = std::mem::take(&mut runtime.pci_block_devices);
    let networks = std::mem::take(&mut runtime.pci_network_devices);
    let vsock = runtime.pci_vsock_device.take();
    let entropy = runtime.pci_entropy_device.take();
    let memory_hotplug = runtime.pci_memory_hotplug_device.take();
    let segment = manager.validation.segment().clone();
    let mut endpoint_index = 0usize;

    let publish_result: Result<(), HvfArm64BootPciDataError> = (|| {
        if let Some(prepared) = balloon {
            let (config_space, available_features, queue_sizes, device) = prepared.into_parts();
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(balloon_type, available_features),
                    queue_sizes.as_slice(),
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI balloon endpoint: {source}"
                    ))
                })?
            };
            manager.balloon = Some(HvfArm64BootPciBalloonDevice {
                published,
                queue_deliveries: 0,
            });
            endpoint_index += 1;
        }

        for prepared in blocks {
            let (drive_id, is_root_device, config_space, device) = prepared.into_parts();
            let metrics_lease = if all_virtio {
                Some(
                    block_device_metrics
                        .claim_drive_lease(&drive_id)
                        .map_err(|source| {
                            HvfArm64BootPciDataError::new(format!(
                                "failed to claim PCI block metrics ownership: {source}"
                            ))
                        })?,
                )
            } else {
                None
            };
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(block_type, config_space.available_features()),
                    &VIRTIO_BLOCK_QUEUE_SIZES,
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI block endpoint {drive_id}: {source}"
                    ))
                })?
            };
            manager.block.push(HvfArm64BootPciBlockDevice {
                drive_id,
                is_root_device,
                published,
                queue_deliveries: 0,
                retry_deadline: None,
                _metrics_lease: metrics_lease,
            });
            endpoint_index += 1;
        }

        for prepared in networks {
            let (iface_id, host_dev_name, config_space, device) = prepared.into_parts();
            let metrics_lease = if all_virtio {
                Some(
                    network_interface_metrics
                        .claim_interface_lease(&iface_id)
                        .map_err(|source| {
                            HvfArm64BootPciDataError::new(format!(
                                "failed to claim PCI network metrics ownership: {source}"
                            ))
                        })?,
                )
            } else {
                None
            };
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(network_type, config_space.available_features()),
                    &VIRTIO_NET_QUEUE_SIZES,
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI network endpoint {iface_id}: {source}"
                    ))
                })?
            };
            manager.network.push(HvfArm64BootPciNetworkDevice {
                iface_id,
                host_dev_name,
                published,
                queue_deliveries: 0,
                retry_deadline: None,
                _metrics_lease: metrics_lease,
            });
            endpoint_index += 1;
        }

        for prepared in &runtime.pmem_devices {
            let pmem_id = prepared.id().to_string();
            let metrics_lease = if all_virtio {
                Some(
                    pmem_device_metrics
                        .claim_device_lease(&pmem_id)
                        .map_err(|source| {
                            HvfArm64BootPciDataError::new(format!(
                                "failed to claim PCI pmem metrics ownership: {source}"
                            ))
                        })?,
                )
            } else {
                None
            };
            let guest_range = prepared.guest_range();
            let config_space = prepared.config_space();
            let device = VirtioPmemDevice::with_rate_limiter(
                prepared.mapping().file_len(),
                prepared.rate_limiter(),
            );
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(pmem_type, config_space.available_features()),
                    &VIRTIO_PMEM_QUEUE_SIZES,
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI pmem endpoint {pmem_id}: {source}"
                    ))
                })?
            };
            manager.pmem.push(HvfArm64BootPciPmemDevice {
                pmem_id,
                guest_range,
                published,
                queue_deliveries: 0,
                retry_deadline: None,
                _metrics_lease: metrics_lease,
            });
            endpoint_index += 1;
        }

        if let Some(prepared) = vsock {
            let (_, _, config_space, device) = prepared.into_parts();
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(vsock_type, config_space.available_features()),
                    &VIRTIO_VSOCK_QUEUE_SIZES,
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI vsock endpoint: {source}"
                    ))
                })?
            };
            manager.vsock = Some(HvfArm64BootPciVsockDevice {
                published,
                queue_deliveries: 0,
            });
            endpoint_index += 1;
        }

        if let Some(prepared) = entropy {
            let (config_space, device) = prepared.into_parts();
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(entropy_type, VIRTIO_MMIO_VERSION_1_FEATURE),
                    &VIRTIO_RNG_QUEUE_SIZES,
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI entropy endpoint: {source}"
                    ))
                })?
            };
            manager.entropy = Some(HvfArm64BootPciEntropyDevice {
                published,
                queue_deliveries: 0,
            });
            endpoint_index += 1;
        }

        if let Some(prepared) = memory_hotplug {
            let (config_space, device) = prepared.into_parts();
            let interrupts = manager.shared_msi_registry()?;
            let region_id = pci_data_region_id(endpoint_index)?;
            let published = {
                let mut dispatcher = manager.dispatcher.lock().map_err(|_| {
                    HvfArm64BootPciDataError::new(
                        "PCI data-device MMIO dispatcher is unavailable during publication",
                    )
                })?;
                PublishedVirtioPciEndpoint::publish(
                    VirtioPciIdentity::new(memory_hotplug_type, config_space.available_features()),
                    &VIRTIO_MEM_QUEUE_SIZES,
                    config_space,
                    device,
                    false,
                    manager.validation.bar_allocator_mut(),
                    segment.clone(),
                    &mut dispatcher,
                    region_id,
                    interrupts,
                )
                .map_err(|source| {
                    HvfArm64BootPciDataError::new(format!(
                        "failed to publish PCI memory-hotplug endpoint: {source}"
                    ))
                })?
            };
            manager.memory_hotplug = Some(HvfArm64BootPciMemoryHotplugDevice {
                published,
                queue_deliveries: 0,
            });
            endpoint_index += 1;
        }
        debug_assert_eq!(endpoint_index, demand.endpoints);
        Ok(())
    })();

    if let Err(primary) = publish_result {
        let cleanup = manager.teardown().err();
        let message = match cleanup {
            Some(cleanup) => format!("{primary}; rollback also failed: {cleanup}"),
            None => primary.to_string(),
        };
        return Err(HvfArm64BootPciDataError::new(message));
    }

    Ok(Some(manager))
}

fn allocate_interrupt_lines(
    gic: &HvfGicMetadata,
    request: HvfArm64BootInterruptRequest,
) -> Result<HvfArm64BootInterruptLines, HvfArm64BootSessionError> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(gic).map_err(|source| {
        HvfArm64BootSessionError::AllocateInterruptLine {
            purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
            source,
        }
    })?;
    let mut block = Vec::new();
    block
        .try_reserve_exact(request.block_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..request.block_device_count {
        block.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
                source,
            }
        })?);
    }

    let mut pmem = Vec::new();
    pmem.try_reserve_exact(request.pmem_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..request.pmem_device_count {
        pmem.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::PmemDevice,
                source,
            }
        })?);
    }

    let mut network = Vec::new();
    network
        .try_reserve_exact(request.network_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..request.network_device_count {
        network.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::NetworkDevice,
                source,
            }
        })?);
    }

    let vsock = if request.vsock_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VsockDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let balloon = if request.balloon_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BalloonDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let entropy = if request.entropy_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::EntropyDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let memory_hotplug = if request.memory_hotplug_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::MemoryHotplugDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let serial = if request.serial_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::SerialDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let vmgenid =
        allocator
            .allocate()
            .map_err(|source| HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmGenIdDevice,
                source,
            })?;
    let vmclock =
        allocator
            .allocate()
            .map_err(|source| HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmClockDevice,
                source,
            })?;

    Ok(HvfArm64BootInterruptLines {
        block,
        pmem,
        network,
        vsock,
        balloon,
        entropy,
        memory_hotplug,
        serial,
        vmgenid,
        vmclock,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::error::Error as _;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    use std::num::{NonZeroU32, NonZeroUsize};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use bangbang_runtime::VmmAction;
    use bangbang_runtime::balloon::{
        BalloonConfigInput, BalloonMmioLayout, VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
        VIRTIO_BALLOON_INFLATE_QUEUE_INDEX, VIRTIO_BALLOON_STATS_QUEUE_INDEX,
        VirtioBalloonDeviceNotificationError,
    };
    use bangbang_runtime::block::{
        BlockMmioLayout, DriveConfigInput, VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, VIRTIO_BLOCK_REQUEST_TYPE_IN, VIRTIO_BLOCK_SECTOR_SIZE,
        VIRTIO_BLOCK_STATUS_OK, VIRTIO_BLOCK_STATUS_SIZE,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::boot_timer::BootTimerMmioLayout;
    use bangbang_runtime::entropy::{
        EntropyMmioLayout, VirtioRngEntropySource, VirtioRngEntropySourceError,
    };
    use bangbang_runtime::fdt::{
        ARM64_FDT_VMGENID_SIZE, Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts,
        Arm64FdtVmGenIdDevice,
    };
    use bangbang_runtime::fdt::{Arm64FdtCache, Arm64FdtCacheHierarchy, Arm64FdtCacheType};
    use bangbang_runtime::interrupt::{
        DeviceInterruptKind, GuestInterruptLine, InterruptSignalError, InterruptSink,
    };
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange, aarch64,
    };
    use bangbang_runtime::memory_hotplug::{
        MemoryHotplugConfig, MemoryHotplugConfigInput, MemoryHotplugSizeUpdateInput,
        VIRTIO_MEM_DEFAULT_REGION_ADDRESS, VIRTIO_MEM_REQUEST_SIZE, VIRTIO_MEM_RESPONSE_SIZE,
        VirtioMemAppliedMutation, VirtioMemMmioLayout, VirtioMemMutation, VirtioMemMutationError,
        VirtioMemMutationExecutor, VirtioMemMutationKind, VirtioMemMutationRollbackError,
    };
    use bangbang_runtime::metrics::{
        BalloonDeviceMetrics, BlockDeviceMetrics, BlockDeviceMetricsByDrive, EntropyDeviceMetrics,
        NetworkInterfaceMetrics, NetworkInterfaceMetricsByInterface, PmemDeviceMetrics,
        PmemDeviceMetricsByDevice, SharedBalloonDeviceMetrics, SharedBlockDeviceMetricsRegistry,
        SharedEntropyDeviceMetrics, SharedNetworkInterfaceMetricsRegistry,
        SharedPmemDeviceMetricsRegistry, SharedVsockDeviceMetrics, VsockDeviceMetrics,
    };
    use bangbang_runtime::mmio::{
        MmioAccess, MmioAccessBytes, MmioDispatchOutcome, MmioDispatcher, MmioHandler,
        MmioHandlerError, MmioOperation, MmioRegionId,
    };
    use bangbang_runtime::network::{
        NetworkInterfaceConfigInput, NetworkMmioLayout, NetworkRateLimiterConfig,
        NetworkTokenBucketConfig, VIRTIO_NET_QUEUE_SIZES, VIRTIO_NET_RX_QUEUE_INDEX,
        VIRTIO_NET_TX_HEADER_SIZE, VIRTIO_NET_TX_QUEUE_INDEX, VirtioNetworkRxPacket,
        VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame,
        VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSink,
        VirtioNetworkTxPacketSinkError,
    };
    use bangbang_runtime::pci::{PciBarAddressSpace, PciBarAllocator};
    use bangbang_runtime::pmem::{
        PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT, VIRTIO_PMEM_REQUEST_SIZE,
        VIRTIO_PMEM_REQUEST_TYPE_FLUSH, VIRTIO_PMEM_STATUS_SIZE, VirtioPmemFlushStatus,
    };
    use bangbang_runtime::rtc::RtcMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::snapshot_device::SnapshotV1BlockRetryState;
    use bangbang_runtime::startup::{
        ARM64_BOOT_VMGENID_SIZE, Arm64BootBalloonNotificationDispatches,
        Arm64BootBlockNotificationDispatches, Arm64BootEntropyDeviceConfig,
        Arm64BootEntropyNotificationDispatches, Arm64BootEntropySource,
        Arm64BootEntropySourceError, Arm64BootEntropySourceProvider,
        Arm64BootMemoryHotplugDeviceConfig, Arm64BootMemoryHotplugNotificationDispatches,
        Arm64BootNetworkNotificationDispatches, Arm64BootNetworkNotificationOutcome,
        Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError, Arm64BootNetworkPacketIoProvider,
        Arm64BootPciValidationConfig, Arm64BootPmemFlushProvider,
        Arm64BootPmemNotificationDispatches, Arm64BootResourceConfig, Arm64BootResources,
        Arm64BootRuntimeResources, Arm64BootVmGenIdDevice, Arm64BootVmGenIdReplacementError,
        Arm64BootVsockNotificationDispatches, update_memory_hotplug_config_for_device,
    };
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };
    use bangbang_runtime::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
    };
    use bangbang_runtime::vsock::{
        VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_PACKET_HEADER_SIZE,
        VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX, VirtioVsockPacketHeader,
        VsockConfigInput, VsockMmioLayout,
    };

    use super::{
        HvfArm64BootBalloonDeviceConfig, HvfArm64BootBalloonNotificationDispatchError,
        HvfArm64BootBlockNotificationDispatchError, HvfArm64BootEntropyDeviceConfig,
        HvfArm64BootEntropyNotificationDispatchError, HvfArm64BootInterruptLinePurpose,
        HvfArm64BootInterruptRequest, HvfArm64BootLimiterRetrySnapshotError,
        HvfArm64BootLimiterRetryWakeupOwner, HvfArm64BootLimiterRetryWakeupQuiescenceError,
        HvfArm64BootLimiterRetryWakeupScheduler,
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError,
        HvfArm64BootLimiterRetryWakeupSchedulerStatus, HvfArm64BootLimiterRetryWakeupToken,
        HvfArm64BootMemoryHotplugDeviceConfig, HvfArm64BootMemoryHotplugNotificationDispatchError,
        HvfArm64BootMmioDispatcherError, HvfArm64BootNetworkNotificationDispatchError,
        HvfArm64BootPmemNotificationDispatchError, HvfArm64BootRunLoopControl,
        HvfArm64BootRunLoopControlWakeupToken, HvfArm64BootRunLoopOutcome,
        HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSessionError, HvfArm64BootTimerDeviceConfig, HvfArm64BootVmGenIdRestoreError,
        HvfArm64BootVsockNotificationDispatchError, PCI_ENDPOINT_SLOT_COUNT,
        allocate_interrupt_lines, collect_balloon_notification_dispatches,
        collect_block_notification_dispatches, collect_entropy_notification_dispatches,
        collect_memory_hotplug_notification_dispatches, collect_network_notification_dispatches,
        collect_vsock_notification_dispatches,
        dispatch_memory_hotplug_runtime_notifications_with_executor,
        dispatch_network_runtime_notifications_with_packet_io, lock_boot_mmio_dispatcher,
        lock_boot_mmio_dispatcher_runtime, pci_all_virtio_gic_msi_configuration,
        pci_all_virtio_resource_demand, pci_data_available_bar_count, pci_data_bar_plan,
        pci_data_endpoint_count, pci_data_region_id, pci_data_resource_demand,
        preflight_pci_data_dispatcher, quiesce_limiter_retry_wakeups,
        record_entropy_dispatch_metrics, record_pmem_dispatch_metrics,
        replace_vmgenid_and_signal_with, run_boot_session_loop, run_boot_session_vcpu_step,
        signal_balloon_queue_interrupts, signal_block_queue_interrupts,
        signal_entropy_queue_interrupts, signal_memory_hotplug_queue_interrupts,
        signal_network_queue_interrupts, signal_pmem_queue_interrupts,
        signal_vsock_queue_interrupts, snapshot_limiter_retry_state_at,
    };
    use crate::coordinator::HvfVcpuRunCoordinator;
    use crate::exit::{
        HvfExceptionExit, HvfHvcExit, HvfMmioAccessSize, HvfMmioDirection, HvfMmioRegister,
        HvfSys64Exit,
    };
    use crate::gic::{
        HvfGicInterruptRange, HvfGicMetadata, HvfGicRedistributor, HvfGicRegion,
        HvfGicSpiSignalError,
    };
    use crate::runner::tests::start_secondary_configure_recording_runner;
    use crate::runner::{HvfVcpuRunStepOutcome, HvfVcpuRunnerError};

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;

    #[test]
    fn vsock_run_loop_wakeup_token_retains_each_pending_request() {
        let token = super::HvfArm64BootRunLoopWakeupToken::default();

        assert!(!token.take_wakeup_request());
        token.request_wakeup();
        token.request_wakeup();

        assert!(token.take_wakeup_request());
        assert!(token.take_wakeup_request());
        assert!(!token.take_wakeup_request());
    }

    #[test]
    fn vsock_wakeup_pollfds_preserve_read_and_write_interests() {
        let pollfds = super::vsock_wakeup_pollfds(vec![13, 11, 13], vec![12, 13, 12], 10)
            .expect("vsock poll descriptors should build");
        let descriptors = pollfds
            .iter()
            .map(|pollfd| (pollfd.fd, pollfd.events, pollfd.revents))
            .collect::<Vec<_>>();

        assert_eq!(
            descriptors,
            [
                (10, libc::POLLIN, 0),
                (11, libc::POLLIN, 0),
                (12, libc::POLLOUT, 0),
                (13, libc::POLLIN | libc::POLLOUT, 0),
            ]
        );
        assert!(super::pollfd_has_wakeup_event(libc::POLLOUT));
    }

    #[test]
    fn vsock_wakeup_poll_timeout_rounds_up_and_clamps_absolute_deadline() {
        let now = Instant::now();

        assert_eq!(super::vsock_wakeup_poll_timeout(None, now), -1);
        assert_eq!(super::vsock_wakeup_poll_timeout(Some(now), now), 0);
        assert_eq!(
            super::vsock_wakeup_poll_timeout(Some(now - Duration::from_nanos(1)), now),
            0
        );
        assert_eq!(
            super::vsock_wakeup_poll_timeout(Some(now + Duration::from_nanos(1)), now),
            1
        );
        assert_eq!(
            super::vsock_wakeup_poll_timeout(Some(now + Duration::from_millis(1)), now),
            1
        );
        assert_eq!(
            super::vsock_wakeup_poll_timeout(
                Some(now + Duration::from_millis(1) + Duration::from_nanos(1)),
                now,
            ),
            2
        );
        assert_eq!(
            super::vsock_wakeup_poll_timeout(
                Some(now + Duration::from_millis(libc::c_int::MAX as u64 + 1)),
                now,
            ),
            libc::c_int::MAX
        );
    }

    #[test]
    fn vsock_wakeup_monitor_recomputes_deadline_only_timeout_after_interrupt() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(5);
        let mut pollfds = [libc::pollfd {
            fd: 10,
            events: libc::POLLIN,
            revents: 0,
        }];
        let mut times = VecDeque::from([now, now + Duration::from_millis(2)]);
        let mut outcomes = VecDeque::from([
            Err(io::ErrorKind::Interrupted),
            Ok::<libc::c_int, io::ErrorKind>(0),
        ]);
        let mut timeouts = Vec::new();
        let mut wakeups = 0usize;

        let woke = super::run_vsock_wakeup_monitor_with(
            &mut pollfds,
            1,
            Some(deadline),
            || times.pop_front().expect("each poll should sample time"),
            |pollfds, count, timeout| {
                assert_eq!(pollfds.len(), 1);
                assert_eq!(count, 1);
                timeouts.push(timeout);
                outcomes.pop_front().expect("poll outcome should exist")
            },
            || wakeups += 1,
        );

        assert!(woke);
        assert_eq!(timeouts, [5, 3]);
        assert_eq!(wakeups, 1);
        assert!(times.is_empty());
        assert!(outcomes.is_empty());
    }

    #[test]
    fn vsock_wakeup_monitor_preserves_stop_precedence_over_fd_and_deadline() {
        let now = Instant::now();
        let mut pollfds = [
            libc::pollfd {
                fd: 10,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: 11,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let mut wakeups = 0usize;

        let woke = super::run_vsock_wakeup_monitor_with(
            &mut pollfds,
            2,
            Some(now),
            || now,
            |pollfds, _, timeout| {
                assert_eq!(timeout, 0);
                pollfds[0].revents = libc::POLLIN;
                pollfds[1].revents = libc::POLLERR;
                Ok(2)
            },
            || wakeups += 1,
        );

        assert!(!woke);
        assert_eq!(wakeups, 0);
    }

    #[test]
    fn vsock_wakeup_monitor_coalesces_simultaneous_fd_and_deadline_readiness() {
        let now = Instant::now();
        let mut pollfds = [
            libc::pollfd {
                fd: 10,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: 11,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let mut wakeups = 0usize;

        let woke = super::run_vsock_wakeup_monitor_with(
            &mut pollfds,
            2,
            Some(now),
            || now,
            |pollfds, _, timeout| {
                assert_eq!(timeout, 0);
                pollfds[1].revents = libc::POLLIN;
                Ok(1)
            },
            || wakeups += 1,
        );

        assert!(woke);
        assert_eq!(wakeups, 1);
    }

    #[test]
    fn vsock_wakeup_monitor_conservatively_wakes_owner_on_poll_error() {
        let now = Instant::now();
        let mut pollfds = [libc::pollfd {
            fd: 10,
            events: libc::POLLIN,
            revents: 0,
        }];
        let mut wakeups = 0usize;

        let woke = super::run_vsock_wakeup_monitor_with(
            &mut pollfds,
            1,
            None,
            || now,
            |_, _, timeout| {
                assert_eq!(timeout, -1);
                Err(io::ErrorKind::PermissionDenied)
            },
            || wakeups += 1,
        );

        assert!(woke);
        assert_eq!(wakeups, 1);
    }

    #[test]
    fn vsock_deadline_only_monitor_stops_and_joins_without_waiting_for_deadline() {
        let (runner, _configured) = start_secondary_configure_recording_runner(false);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let mut coordinator =
            HvfVcpuRunCoordinator::from_test_runners(vec![runner], vec![0], dispatcher, &[0])
                .expect("test coordinator should build");
        let wakeup = super::HvfArm64BootRunLoopWakeupToken::default();
        let monitor = super::HvfArm64BootRunLoopWakeupMonitor::start(
            Vec::new(),
            Vec::new(),
            Some(Instant::now() + Duration::from_secs(60 * 60)),
            false,
            coordinator.control(),
            wakeup.clone(),
        )
        .expect("deadline-only monitor should start");

        assert!(!monitor.finish().expect("monitor should stop and join"));
        assert!(!wakeup.take_wakeup_request());
        coordinator
            .shutdown()
            .expect("test coordinator should shut down");
    }

    #[test]
    fn network_retry_scheduler_start_error_preserves_source() {
        let error = HvfArm64BootSessionError::StartNetworkRetryWakeupScheduler {
            source: std::io::Error::other("fake network retry scheduler failure"),
        };

        assert_eq!(
            error.to_string(),
            "failed to start HVF network retry wakeup scheduler: fake network retry scheduler failure"
        );
        assert_eq!(
            error
                .source()
                .expect("network scheduler start error should retain its source")
                .to_string(),
            "fake network retry scheduler failure"
        );
    }

    fn vmgenid_restore_test_memory_and_device() -> (GuestMemory, Arm64BootVmGenIdDevice) {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::SYSTEM_MEM_START),
                aarch64::SYSTEM_MEM_SIZE,
            )
            .expect("VMGenID test range should be valid"),
        ])
        .expect("VMGenID test layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("VMGenID test memory should allocate");
        let range = GuestMemoryRange::new(
            GuestAddress::new(
                aarch64::SYSTEM_MEM_START + aarch64::SYSTEM_MEM_SIZE - ARM64_FDT_VMGENID_SIZE,
            ),
            ARM64_FDT_VMGENID_SIZE,
        )
        .expect("VMGenID device range should be valid");
        let device = Arm64BootVmGenIdDevice {
            range,
            generation_id: [0x11; ARM64_BOOT_VMGENID_SIZE],
            fdt_device: Arm64FdtVmGenIdDevice {
                region: Arm64FdtRegion {
                    base: range.start().raw_value(),
                    size: range.size(),
                },
                interrupt_line: line(127),
            },
        };
        (memory, device)
    }

    #[test]
    fn vmgenid_restore_replaces_before_signaling() {
        let (mut memory, mut device) = vmgenid_restore_test_memory_and_device();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let replacement_calls = Arc::clone(&calls);
        let signal_calls = Arc::clone(&calls);

        replace_vmgenid_and_signal_with(
            &mut memory,
            &mut device,
            move |memory, device| {
                replacement_calls
                    .lock()
                    .expect("call list should be lockable")
                    .push("replace");
                let candidate = [0x22; ARM64_BOOT_VMGENID_SIZE];
                memory
                    .write_slice(&candidate, device.range.start())
                    .map_err(
                        |source| Arm64BootVmGenIdReplacementError::GuestMemoryWrite { source },
                    )?;
                device.generation_id = candidate;
                Ok(())
            },
            move || {
                signal_calls
                    .lock()
                    .expect("call list should be lockable")
                    .push("signal");
                Ok(())
            },
        )
        .expect("VMGenID restore stages should succeed");

        assert_eq!(
            *calls.lock().expect("call list should be lockable"),
            vec!["replace", "signal"]
        );
        assert_eq!(device.generation_id, [0x22; ARM64_BOOT_VMGENID_SIZE]);
        let mut guest_value = [0; ARM64_BOOT_VMGENID_SIZE];
        memory
            .read_slice(&mut guest_value, device.range.start())
            .expect("replacement VMGenID should read");
        assert_eq!(guest_value, device.generation_id);
    }

    #[test]
    fn vmgenid_restore_replacement_failure_sends_no_signal() {
        let (mut memory, mut device) = vmgenid_restore_test_memory_and_device();
        let signal_count = Arc::new(AtomicU64::new(0));
        let observed_signal_count = Arc::clone(&signal_count);

        let error = replace_vmgenid_and_signal_with(
            &mut memory,
            &mut device,
            |_memory, _device| Err(Arm64BootVmGenIdReplacementError::Random),
            move || {
                observed_signal_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect_err("replacement failure should stop before signal");

        assert!(matches!(
            error,
            HvfArm64BootVmGenIdRestoreError::Replacement {
                source: Arm64BootVmGenIdReplacementError::Random
            }
        ));
        assert_eq!(signal_count.load(Ordering::SeqCst), 0);
        assert_eq!(device.generation_id, [0x11; ARM64_BOOT_VMGENID_SIZE]);
    }

    #[test]
    fn vmgenid_restore_signal_failure_reports_committed_replacement() {
        let (mut memory, mut device) = vmgenid_restore_test_memory_and_device();

        let error = replace_vmgenid_and_signal_with(
            &mut memory,
            &mut device,
            |memory, device| {
                let candidate = [0x33; ARM64_BOOT_VMGENID_SIZE];
                memory
                    .write_slice(&candidate, device.range.start())
                    .map_err(
                        |source| Arm64BootVmGenIdReplacementError::GuestMemoryWrite { source },
                    )?;
                device.generation_id = candidate;
                Ok(())
            },
            || {
                Err(HvfGicSpiSignalError::InvalidState(
                    "fake VMGenID signal failure",
                ))
            },
        )
        .expect_err("signal failure should preserve partial-stage context");

        assert!(matches!(
            error,
            HvfArm64BootVmGenIdRestoreError::Signal { .. }
        ));
        assert_eq!(device.generation_id, [0x33; ARM64_BOOT_VMGENID_SIZE]);
        assert!(!format!("{error:?}").contains("51, 51"));

        let mut retried_signal_called = false;
        replace_vmgenid_and_signal_with(
            &mut memory,
            &mut device,
            |memory, device| {
                assert_eq!(device.generation_id, [0x33; ARM64_BOOT_VMGENID_SIZE]);
                let candidate = [0x44; ARM64_BOOT_VMGENID_SIZE];
                memory
                    .write_slice(&candidate, device.range.start())
                    .map_err(
                        |source| Arm64BootVmGenIdReplacementError::GuestMemoryWrite { source },
                    )?;
                device.generation_id = candidate;
                Ok(())
            },
            || {
                retried_signal_called = true;
                Ok(())
            },
        )
        .expect("complete VMGenID retry should replace again and signal");

        assert!(retried_signal_called);
        assert_eq!(device.generation_id, [0x44; ARM64_BOOT_VMGENID_SIZE]);
        let mut guest_value = [0; ARM64_BOOT_VMGENID_SIZE];
        memory
            .read_slice(&mut guest_value, device.range.start())
            .expect("retried VMGenID should read");
        assert_eq!(guest_value, device.generation_id);
    }

    fn wait_for_limiter_retry_scheduler_status(
        scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
        expected: HvfArm64BootLimiterRetryWakeupSchedulerStatus,
    ) {
        let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        while state.status != expected {
            state = super::wait_limiter_retry_wakeup_state(&scheduler.shared, state);
        }
    }

    fn wait_for_limiter_retry_publication_idle(
        scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    ) {
        let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        while state.publication_in_flight {
            state = super::wait_limiter_retry_wakeup_state(&scheduler.shared, state);
        }
    }

    #[test]
    fn limiter_retry_snapshot_uses_injected_deadline_without_mutation() {
        let scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let now = Instant::now();
        {
            let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            state.deadline = now.checked_add(Duration::from_millis(25));
        }
        let guard = scheduler.quiesce().expect("scheduler should quiesce");

        assert_eq!(
            snapshot_limiter_retry_state_at(&guard, now).expect("future deadline should snapshot"),
            SnapshotV1BlockRetryState::After {
                remaining_nanos: 25_000_000,
            }
        );
        assert_eq!(
            snapshot_limiter_retry_state_at(&guard, now + Duration::from_millis(25))
                .expect("due deadline should snapshot"),
            SnapshotV1BlockRetryState::Immediate
        );
        let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        assert_eq!(state.deadline, now.checked_add(Duration::from_millis(25)));
        drop(state);
        drop(guard);
    }

    #[test]
    fn limiter_retry_snapshot_prioritizes_deferred_work_and_rejects_in_flight() {
        let scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let now = Instant::now();
        {
            let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            state.deferred_publication = true;
        }
        let guard = scheduler.quiesce().expect("scheduler should quiesce");
        assert_eq!(
            snapshot_limiter_retry_state_at(&guard, now).expect("deferred retry should snapshot"),
            SnapshotV1BlockRetryState::Immediate
        );
        {
            let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            state.publication_in_flight = true;
        }
        assert_eq!(
            snapshot_limiter_retry_state_at(&guard, now)
                .expect_err("in-flight publication should reject"),
            HvfArm64BootLimiterRetrySnapshotError::PublicationInFlight
        );
        {
            let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            state.publication_in_flight = false;
        }
        drop(guard);
    }

    #[test]
    fn limiter_retry_quiescence_waits_for_in_flight_and_republishes_drained_wakeup() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-in-flight",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
                release_receiver
                    .recv()
                    .expect("test should release retry wakeup publication");
                Err::<(), &'static str>("expected cancellation failure")
            },
        )
        .expect("test retry wakeup scheduler should start");
        scheduler.schedule_after(Some(Duration::ZERO));
        publication_receiver
            .recv()
            .expect("scheduled retry wakeup should begin publication");

        thread::scope(|scope| {
            let (guard_sender, guard_receiver) = mpsc::channel();
            let scheduler_ref = &scheduler;
            scope.spawn(move || {
                guard_sender
                    .send(scheduler_ref.quiesce())
                    .expect("test should receive quiescence result");
            });

            wait_for_limiter_retry_scheduler_status(
                &scheduler,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced,
            );
            assert!(matches!(
                guard_receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));

            release_sender
                .send(())
                .expect("test should release in-flight publication");
            let guard = guard_receiver
                .recv()
                .expect("quiescence acquisition should complete")
                .expect("running scheduler should quiesce");
            assert!(wakeup_token.take_wakeup_request());
            guard.defer_publication();
            assert_eq!(
                publication_receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            );

            drop(guard);
            publication_receiver
                .recv()
                .expect("drained retry wakeup should be republished");
            assert!(wakeup_token.take_wakeup_request());
            release_sender
                .send(())
                .expect("test should release deferred publication");
        });

        wait_for_limiter_retry_publication_idle(&scheduler);
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_quiescence_coalesces_deferred_and_overdue_work() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-coalesce",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");

        let guard = scheduler
            .quiesce()
            .expect("running scheduler should quiesce");
        scheduler.schedule_after(Some(Duration::ZERO));
        guard.defer_publication();
        assert_eq!(
            publication_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        drop(guard);
        publication_receiver
            .recv()
            .expect("overdue retry wakeup should publish after release");
        wait_for_limiter_retry_publication_idle(&scheduler);
        assert!(wakeup_token.take_wakeup_request());
        assert_eq!(
            publication_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(state.deadline, None);
            assert!(!state.deferred_publication);
        }

        scheduler.stop();
    }

    #[test]
    fn limiter_retry_quiescence_preserves_future_deadline_after_deferred_publication() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-future",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");

        let guard = scheduler
            .quiesce()
            .expect("running scheduler should quiesce");
        scheduler.schedule_after(Some(Duration::from_secs(3_600)));
        guard.defer_publication();
        drop(guard);

        publication_receiver
            .recv()
            .expect("deferred retry wakeup should publish immediately");
        wait_for_limiter_retry_publication_idle(&scheduler);
        assert!(wakeup_token.take_wakeup_request());
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert!(state.deadline.is_some());
            assert!(!state.deferred_publication);
        }

        scheduler.schedule_after(None);
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(state.deadline, None);
            assert!(!state.deferred_publication);
        }
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_schedule_cancellation_discards_deferred_publication() {
        let (publication_sender, publication_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-cancel",
            HvfArm64BootLimiterRetryWakeupToken::default(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");

        let guard = scheduler
            .quiesce()
            .expect("running scheduler should quiesce");
        guard.defer_publication();
        scheduler.schedule_after(None);
        drop(guard);
        wait_for_limiter_retry_scheduler_status(
            &scheduler,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running,
        );
        assert_eq!(
            publication_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        scheduler.stop();
    }

    #[test]
    fn limiter_retry_schedule_replaces_deadline_earlier_and_later() {
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-replace",
            HvfArm64BootLimiterRetryWakeupToken::default(),
            || {},
        )
        .expect("test retry wakeup scheduler should start");

        scheduler.schedule_after(Some(Duration::from_secs(3_600)));
        let first = super::lock_limiter_retry_wakeup_state(&scheduler.shared)
            .deadline
            .expect("first future deadline should be stored");
        scheduler.schedule_after(Some(Duration::from_secs(7_200)));
        let later = super::lock_limiter_retry_wakeup_state(&scheduler.shared)
            .deadline
            .expect("later replacement deadline should be stored");
        scheduler.schedule_after(Some(Duration::from_secs(1_800)));
        let earlier = super::lock_limiter_retry_wakeup_state(&scheduler.shared)
            .deadline
            .expect("earlier replacement deadline should be stored");

        assert!(later > first);
        assert!(earlier < first);
        scheduler.stop();
    }

    #[test]
    fn runtime_block_inventory_change_replaces_stale_retry_from_survivors() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-runtime-block-retry-refresh",
            wakeup_token.clone(),
            || {},
        )
        .expect("test retry wakeup scheduler should start");
        scheduler.schedule_after(Some(Duration::from_secs(3_600)));
        let removed_device_deadline = super::lock_limiter_retry_wakeup_state(&scheduler.shared)
            .deadline
            .expect("removed device deadline should be stored");
        let survivor_deadline = removed_device_deadline
            .checked_add(Duration::from_secs(3_600))
            .expect("survivor deadline should fit");
        wakeup_token.request_wakeup();

        super::refresh_block_retry_wakeup_after_inventory_change(
            &wakeup_token,
            &scheduler,
            Some(survivor_deadline),
        );

        assert!(!wakeup_token.take_wakeup_request());
        assert_eq!(
            super::lock_limiter_retry_wakeup_state(&scheduler.shared).deadline,
            Some(survivor_deadline)
        );

        wakeup_token.request_wakeup();
        super::refresh_block_retry_wakeup_after_inventory_change(&wakeup_token, &scheduler, None);

        assert!(!wakeup_token.take_wakeup_request());
        assert_eq!(
            super::lock_limiter_retry_wakeup_state(&scheduler.shared).deadline,
            None
        );
        scheduler.stop();
    }

    #[test]
    fn runtime_block_publication_rollback_is_terminal() {
        let error =
            super::runtime_block_publication_error(super::VirtioPciPublicationError::Rollback {
                primary: "injected publication failure".to_string(),
                cleanup: "injected cleanup failure".to_string(),
            });

        assert!(matches!(
            error,
            bangbang_runtime::block::DriveRuntimeMutationError::TerminalInsertion { message }
                if message.contains("injected cleanup failure")
        ));
    }

    #[test]
    fn limiter_retry_cancel_waits_for_in_flight_publication() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-strong-cancel",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
                release_receiver
                    .recv()
                    .expect("test should release retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");
        scheduler.schedule_after(Some(Duration::ZERO));
        publication_receiver
            .recv()
            .expect("scheduled retry wakeup should begin publication");

        thread::scope(|scope| {
            let (canceled_sender, canceled_receiver) = mpsc::channel();
            let scheduler_ref = &scheduler;
            scope.spawn(move || {
                scheduler_ref.cancel_and_wait();
                canceled_sender
                    .send(())
                    .expect("test should observe completed cancellation");
            });

            assert_eq!(canceled_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
            release_sender
                .send(())
                .expect("test should release in-flight publication");
            canceled_receiver
                .recv()
                .expect("cancellation should finish after publication");
        });

        assert!(wakeup_token.take_wakeup_request());
        assert!(!wakeup_token.take_wakeup_request());
        let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        assert_eq!(state.deadline, None);
        assert!(!state.publication_in_flight);
        assert!(!state.deferred_publication);
        drop(state);
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_schedulers_publish_only_their_own_tokens() {
        let first_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let second_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (first_sender, first_receiver) = mpsc::channel();
        let (second_sender, second_receiver) = mpsc::channel();
        let mut first = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-first",
            first_token.clone(),
            move || first_sender.send(()),
        )
        .expect("first retry wakeup scheduler should start");
        let mut second = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-second",
            second_token.clone(),
            move || second_sender.send(()),
        )
        .expect("second retry wakeup scheduler should start");

        first.schedule_after(Some(Duration::ZERO));
        first_receiver
            .recv()
            .expect("first scheduler should publish");
        wait_for_limiter_retry_publication_idle(&first);
        assert!(first_token.take_wakeup_request());
        assert!(!second_token.take_wakeup_request());
        assert_eq!(second_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));

        second.schedule_after(Some(Duration::ZERO));
        second_receiver
            .recv()
            .expect("second scheduler should publish");
        wait_for_limiter_retry_publication_idle(&second);
        assert!(!first_token.take_wakeup_request());
        assert!(second_token.take_wakeup_request());

        first.stop();
        second.stop();
    }

    #[test]
    fn limiter_retry_session_quiescence_drains_and_republishes_all_tokens() {
        let block_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let pmem_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let network_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let entropy_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (block_sender, block_receiver) = mpsc::channel();
        let (pmem_sender, pmem_receiver) = mpsc::channel();
        let (network_sender, network_receiver) = mpsc::channel();
        let (entropy_sender, entropy_receiver) = mpsc::channel();
        let mut block_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-block-retry-quiescence",
            block_token.clone(),
            move || {
                block_sender
                    .send(())
                    .expect("test should observe block retry wakeup publication");
            },
        )
        .expect("test block retry wakeup scheduler should start");
        let mut pmem_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-pmem-retry-quiescence",
            pmem_token.clone(),
            move || {
                pmem_sender
                    .send(())
                    .expect("test should observe pmem retry wakeup publication");
            },
        )
        .expect("test pmem retry wakeup scheduler should start");
        let mut network_scheduler =
            HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
                "bangbang-hvf-test-network-retry-quiescence",
                network_token.clone(),
                move || {
                    network_sender
                        .send(())
                        .expect("test should observe network retry wakeup publication");
                },
            )
            .expect("test network retry wakeup scheduler should start");
        let mut entropy_scheduler =
            HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
                "bangbang-hvf-test-entropy-retry-quiescence",
                entropy_token.clone(),
                move || {
                    entropy_sender
                        .send(())
                        .expect("test should observe entropy retry wakeup publication");
                },
            )
            .expect("test entropy retry wakeup scheduler should start");
        block_token.request_wakeup();
        pmem_token.request_wakeup();
        network_token.request_wakeup();
        entropy_token.request_wakeup();

        let guard = quiesce_limiter_retry_wakeups(
            HvfArm64BootLimiterRetryWakeupOwner::new(&block_token, &block_scheduler),
            HvfArm64BootLimiterRetryWakeupOwner::new(&pmem_token, &pmem_scheduler),
            HvfArm64BootLimiterRetryWakeupOwner::new(&network_token, &network_scheduler),
            HvfArm64BootLimiterRetryWakeupOwner::new(&entropy_token, &entropy_scheduler),
        )
        .expect("all running retry wakeup schedulers should quiesce");
        assert!(!block_token.take_wakeup_request());
        assert!(!pmem_token.take_wakeup_request());
        assert!(!network_token.take_wakeup_request());
        assert!(!entropy_token.take_wakeup_request());
        assert_eq!(
            quiesce_limiter_retry_wakeups(
                HvfArm64BootLimiterRetryWakeupOwner::new(&block_token, &block_scheduler),
                HvfArm64BootLimiterRetryWakeupOwner::new(&pmem_token, &pmem_scheduler),
                HvfArm64BootLimiterRetryWakeupOwner::new(&network_token, &network_scheduler),
                HvfArm64BootLimiterRetryWakeupOwner::new(&entropy_token, &entropy_scheduler),
            )
            .expect_err("duplicate session quiescence should fail"),
            HvfArm64BootLimiterRetryWakeupQuiescenceError::BlockAlreadyQuiesced
        );
        assert_eq!(block_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(pmem_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(network_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(entropy_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));

        drop(guard);
        block_receiver
            .recv()
            .expect("block retry wakeup should be republished");
        pmem_receiver
            .recv()
            .expect("pmem retry wakeup should be republished");
        network_receiver
            .recv()
            .expect("network retry wakeup should be republished");
        entropy_receiver
            .recv()
            .expect("entropy retry wakeup should be republished");
        wait_for_limiter_retry_publication_idle(&block_scheduler);
        wait_for_limiter_retry_publication_idle(&pmem_scheduler);
        wait_for_limiter_retry_publication_idle(&network_scheduler);
        wait_for_limiter_retry_publication_idle(&entropy_scheduler);
        assert!(block_token.take_wakeup_request());
        assert!(pmem_token.take_wakeup_request());
        assert!(network_token.take_wakeup_request());
        assert!(entropy_token.take_wakeup_request());

        block_scheduler.stop();
        pmem_scheduler.stop();
        network_scheduler.stop();
        entropy_scheduler.stop();
    }

    #[test]
    fn limiter_retry_session_quiescence_rolls_back_when_entropy_is_stopped() {
        let mut block_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut pmem_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut network_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut entropy_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        entropy_scheduler.stop();

        let err = quiesce_limiter_retry_wakeups(
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &block_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &pmem_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &network_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &entropy_scheduler,
            ),
        )
        .expect_err("stopped entropy scheduler should reject quiescence");

        assert_eq!(
            err,
            HvfArm64BootLimiterRetryWakeupQuiescenceError::EntropyStopped
        );
        {
            let state = super::lock_limiter_retry_wakeup_state(&block_scheduler.shared);
            assert_eq!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running
            );
        }
        block_scheduler.stop();
        pmem_scheduler.stop();
        network_scheduler.stop();
    }

    #[test]
    fn limiter_retry_session_quiescence_rolls_back_when_network_is_stopped() {
        let mut block_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut pmem_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut network_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut entropy_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        network_scheduler.stop();

        let err = quiesce_limiter_retry_wakeups(
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &block_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &pmem_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &network_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &entropy_scheduler,
            ),
        )
        .expect_err("stopped network scheduler should reject quiescence");

        assert_eq!(
            err,
            HvfArm64BootLimiterRetryWakeupQuiescenceError::NetworkStopped
        );
        for scheduler in [&block_scheduler, &pmem_scheduler] {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running
            );
        }
        block_scheduler.stop();
        pmem_scheduler.stop();
        entropy_scheduler.stop();
    }

    #[test]
    fn limiter_retry_session_quiescence_rolls_back_when_network_is_already_quiesced() {
        let mut block_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut pmem_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut network_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut entropy_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let network_guard = network_scheduler
            .quiesce()
            .expect("network scheduler should quiesce independently");

        let err = quiesce_limiter_retry_wakeups(
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &block_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &pmem_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &network_scheduler,
            ),
            HvfArm64BootLimiterRetryWakeupOwner::new(
                &HvfArm64BootLimiterRetryWakeupToken::default(),
                &entropy_scheduler,
            ),
        )
        .expect_err("already-quiesced network scheduler should reject aggregate quiescence");

        assert_eq!(
            err,
            HvfArm64BootLimiterRetryWakeupQuiescenceError::NetworkAlreadyQuiesced
        );
        for scheduler in [&block_scheduler, &pmem_scheduler] {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running
            );
        }
        drop(network_guard);
        block_scheduler.stop();
        pmem_scheduler.stop();
        network_scheduler.stop();
        entropy_scheduler.stop();
    }

    #[test]
    fn limiter_retry_quiescence_guard_resumes_scheduler_during_unwind() {
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = scheduler
                .quiesce()
                .expect("inactive running scheduler should quiesce");
            panic!("test should unwind through quiescence guard");
        }));

        assert!(unwind.is_err());
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running
            );
        }
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_stop_wins_over_late_quiescence_guard_drop() {
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let guard = scheduler
            .quiesce()
            .expect("inactive running scheduler should quiesce");

        scheduler.stop();
        drop(guard);

        assert_eq!(
            scheduler
                .quiesce()
                .expect_err("stopped scheduler should reject quiescence"),
            HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped
        );
        let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        assert_eq!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped
        );
    }
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const ESR_EC_HVC: u64 = 0x16;
    const ESR_EC_SYS64: u64 = 0x18;
    const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
    const ESR_EC_SHIFT: u64 = 26;
    const ESR_ISS_ISV: u64 = 1 << 24;
    const ESR_ISS_SYS64_DIRECTION: u64 = 1;
    const ESR_ISS_SAS_SHIFT: u64 = 22;
    const ESR_ISS_SRT_SHIFT: u64 = 16;
    const ESR_ISS_WNR: u64 = 1 << 6;
    const ESR_ISS_SF: u64 = 1 << 15;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_9000);
    const TEST_QUEUE_SIZE: u16 = 4;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8040_0000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8041_0000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x8042_0000);
    const HEADER_ADDR: GuestAddress = GuestAddress::new(0x8043_0000);
    const DATA_ADDR: GuestAddress = GuestAddress::new(0x8044_0000);
    const STATUS_ADDR: GuestAddress = GuestAddress::new(0x8045_0000);
    const TEST_NETWORK_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8050_0000);
    const TEST_NETWORK_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8051_0000);
    const TEST_NETWORK_RX_USED_RING: GuestAddress = GuestAddress::new(0x8052_0000);
    const TEST_NETWORK_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8060_0000);
    const TEST_NETWORK_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8061_0000);
    const TEST_NETWORK_TX_USED_RING: GuestAddress = GuestAddress::new(0x8062_0000);
    const TEST_NETWORK_TX_HEADER: GuestAddress = GuestAddress::new(0x8063_0000);
    const TEST_NETWORK_TX_PAYLOAD: GuestAddress = GuestAddress::new(0x8064_0000);
    const TEST_NETWORK_SECOND_TX_HEADER: GuestAddress = GuestAddress::new(0x8065_0000);
    const TEST_NETWORK_SECOND_TX_PAYLOAD: GuestAddress = GuestAddress::new(0x8066_0000);
    const TEST_VSOCK_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8070_0000);
    const TEST_VSOCK_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8071_0000);
    const TEST_VSOCK_RX_USED_RING: GuestAddress = GuestAddress::new(0x8072_0000);
    const TEST_VSOCK_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8073_0000);
    const TEST_VSOCK_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8074_0000);
    const TEST_VSOCK_TX_USED_RING: GuestAddress = GuestAddress::new(0x8075_0000);
    const TEST_VSOCK_EVENT_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8076_0000);
    const TEST_VSOCK_EVENT_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8077_0000);
    const TEST_VSOCK_EVENT_USED_RING: GuestAddress = GuestAddress::new(0x8078_0000);
    const TEST_VSOCK_HEADER: GuestAddress = GuestAddress::new(0x8079_0000);
    const TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x807a_0000);
    const TEST_BALLOON_INFLATE_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x807a_1000);
    const TEST_BALLOON_INFLATE_USED_RING: GuestAddress = GuestAddress::new(0x807a_2000);
    const TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x807a_3000);
    const TEST_BALLOON_DEFLATE_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x807a_4000);
    const TEST_BALLOON_DEFLATE_USED_RING: GuestAddress = GuestAddress::new(0x807a_5000);
    const TEST_BALLOON_PFN_PAYLOAD: GuestAddress = GuestAddress::new(0x807a_6000);
    const TEST_BALLOON_REPORTING_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x807a_7000);
    const TEST_BALLOON_REPORTING_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x807a_8000);
    const TEST_BALLOON_REPORTING_USED_RING: GuestAddress = GuestAddress::new(0x807a_9000);
    const TEST_BALLOON_REPORTING_RANGE: GuestAddress = GuestAddress::new(0x807a_a000);
    const TEST_BALLOON_MAPPED_PFN: u32 = 0x80000;
    const TEST_VIRTIO_MEM_REQ_PLUG: u16 = 0;
    const TEST_VIRTIO_MEM_REQ_STATE: u16 = 3;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    const PSCI_VERSION: u64 = 0x8400_0000;
    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
    const PSCI_VERSION_1_0: u64 = 0x0001_0000;
    const PSCI_RET_SUCCESS: u64 = 0;
    const TEST_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_4000);
    const TEST_RTC_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_1000);
    const TEST_ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_7000);
    const TEST_BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const TEST_MEMORY_HOTPLUG_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_a000);

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

    #[derive(Debug, Clone)]
    struct RecordingSink {
        lines: Arc<Mutex<Vec<GuestInterruptLine>>>,
        result: Result<(), InterruptSignalError>,
    }

    impl RecordingSink {
        fn successful() -> (Arc<Mutex<Vec<GuestInterruptLine>>>, Arc<dyn InterruptSink>) {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::new(Self {
                lines: Arc::clone(&lines),
                result: Ok(()),
            });

            (lines, sink)
        }

        fn failing(
            message: &'static str,
        ) -> (Arc<Mutex<Vec<GuestInterruptLine>>>, Arc<dyn InterruptSink>) {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::new(Self {
                lines: Arc::clone(&lines),
                result: Err(InterruptSignalError::new(message)),
            });

            (lines, sink)
        }
    }

    impl InterruptSink for RecordingSink {
        fn signal(&self, line: GuestInterruptLine) -> Result<(), InterruptSignalError> {
            self.lines
                .lock()
                .expect("recording sink lock should not be poisoned")
                .push(line);
            self.result.clone()
        }
    }

    #[derive(Debug, Default)]
    struct RecordingTxPacketSink {
        packets: Vec<Vec<u8>>,
    }

    impl VirtioNetworkTxPacketSink for RecordingTxPacketSink {
        fn transmit_frame(
            &mut self,
            memory: &GuestMemory,
            frame: &VirtioNetworkTxFrame,
        ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
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

            Ok(VirtioNetworkTxPacketDisposition::Forwarded)
        }
    }

    #[derive(Debug, Default)]
    struct EmptyRxPacketSource {
        peek_calls: usize,
    }

    impl VirtioNetworkRxPacketSource for EmptyRxPacketSource {
        fn peek_packet(
            &mut self,
        ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
            self.peek_calls += 1;
            Ok(None)
        }

        fn consume_packet(&mut self) {}
    }

    #[derive(Debug)]
    struct RecordingNetworkPacketIoProvider {
        iface_id: String,
        tx_sink: RecordingTxPacketSink,
        rx_source: EmptyRxPacketSource,
        requested_ifaces: Vec<String>,
        fail: bool,
    }

    impl RecordingNetworkPacketIoProvider {
        fn for_iface(iface_id: &str) -> Self {
            Self {
                iface_id: iface_id.to_string(),
                tx_sink: RecordingTxPacketSink::default(),
                rx_source: EmptyRxPacketSource::default(),
                requested_ifaces: Vec::new(),
                fail: false,
            }
        }

        fn failing_for(iface_id: &str) -> Self {
            Self {
                fail: true,
                ..Self::for_iface(iface_id)
            }
        }
    }

    impl Arm64BootNetworkPacketIoProvider for RecordingNetworkPacketIoProvider {
        fn packet_io(
            &mut self,
            interface: bangbang_runtime::startup::Arm64BootNetworkInterface<'_>,
        ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
            let iface_id = interface.iface_id();
            self.requested_ifaces.push(iface_id.to_string());
            if iface_id != self.iface_id {
                return Err(Arm64BootNetworkPacketIoError::new(format!(
                    "missing test packet I/O for interface {iface_id}"
                )));
            }
            if self.fail {
                return Err(Arm64BootNetworkPacketIoError::new(format!(
                    "test packet I/O unavailable for interface {iface_id}"
                )));
            }

            Ok(Arm64BootNetworkPacketIo::new(
                &mut self.tx_sink,
                &mut self.rx_source,
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
    }

    impl Arm64BootEntropySourceProvider for RecordingEntropySourceProvider {
        fn entropy_source(
            &mut self,
            device: &bangbang_runtime::startup::Arm64BootEntropyDevice,
        ) -> Result<Arm64BootEntropySource<'_>, Arm64BootEntropySourceError> {
            self.requested_regions.push(device.registration.region_id());
            Ok(Arm64BootEntropySource::new(&mut self.source))
        }
    }

    #[derive(Debug, Default)]
    struct RecordingVirtioMemMutationExecutor {
        applied: Vec<VirtioMemMutation>,
        rolled_back: Vec<VirtioMemMutation>,
    }

    impl VirtioMemMutationExecutor for RecordingVirtioMemMutationExecutor {
        fn apply(
            &mut self,
            _memory: &mut GuestMemory,
            mutation: VirtioMemMutation,
        ) -> Result<VirtioMemAppliedMutation, VirtioMemMutationError> {
            self.applied.push(mutation.clone());
            Ok(VirtioMemAppliedMutation::new(mutation))
        }

        fn rollback(
            &mut self,
            _memory: &mut GuestMemory,
            applied: VirtioMemAppliedMutation,
        ) -> Result<(), VirtioMemMutationRollbackError> {
            self.rolled_back.push(applied.mutation().clone());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct WrongBlockHandler;

    impl MmioHandler for WrongBlockHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            Err(MmioHandlerError::new("wrong handler read"))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Err(MmioHandlerError::new("wrong handler write"))
        }
    }

    #[derive(Debug)]
    struct WrongNetworkHandler;

    impl MmioHandler for WrongNetworkHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            Err(MmioHandlerError::new("wrong network handler read"))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Err(MmioHandlerError::new("wrong network handler write"))
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

    type RecordedRunStepDispatchers = Arc<Mutex<Vec<Arc<Mutex<MmioDispatcher>>>>>;

    #[derive(Debug)]
    struct RecordingBootSessionRunStepRunner {
        result: Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>,
        dispatchers: RecordedRunStepDispatchers,
    }

    impl RecordingBootSessionRunStepRunner {
        fn new(
            result: Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>,
        ) -> (Self, RecordedRunStepDispatchers) {
            let dispatchers = Arc::new(Mutex::new(Vec::new()));

            (
                Self {
                    result,
                    dispatchers: Arc::clone(&dispatchers),
                },
                dispatchers,
            )
        }
    }

    impl super::BootSessionRunStepRunner for RecordingBootSessionRunStepRunner {
        fn run_once_and_handle_mmio(
            &self,
            dispatcher: Arc<Mutex<MmioDispatcher>>,
        ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
            self.dispatchers
                .lock()
                .expect("recorded dispatcher list should lock")
                .push(dispatcher);

            self.result.clone()
        }
    }

    fn recorded_run_step_dispatchers(
        dispatchers: &RecordedRunStepDispatchers,
    ) -> Vec<Arc<Mutex<MmioDispatcher>>> {
        dispatchers
            .lock()
            .expect("recorded dispatcher list should lock")
            .clone()
    }

    #[derive(Debug)]
    struct RecordingBootSessionRunLoopSession {
        run_results: VecDeque<Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>>,
        dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootBlockNotificationDispatches,
                HvfArm64BootBlockNotificationDispatchError,
            >,
        >,
        pmem_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootPmemNotificationDispatches,
                HvfArm64BootPmemNotificationDispatchError,
            >,
        >,
        monitor_wakeup_results: VecDeque<(bool, bool)>,
        network_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootNetworkNotificationDispatches,
                HvfArm64BootNetworkNotificationDispatchError,
            >,
        >,
        vsock_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootVsockNotificationDispatches,
                HvfArm64BootVsockNotificationDispatchError,
            >,
        >,
        balloon_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootBalloonNotificationDispatches,
                HvfArm64BootBalloonNotificationDispatchError,
            >,
        >,
        memory_hotplug_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootMemoryHotplugNotificationDispatches,
                HvfArm64BootMemoryHotplugNotificationDispatchError,
            >,
        >,
        entropy_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootEntropyNotificationDispatches,
                HvfArm64BootEntropyNotificationDispatchError,
            >,
        >,
        timer_results: VecDeque<Result<(), HvfVcpuRunnerError>>,
        events: Vec<&'static str>,
        request_stop_on_run: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_pmem_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_network_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_vsock_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_balloon_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_memory_hotplug_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_entropy_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_timer: Option<HvfArm64BootRunLoopStopToken>,
        request_network_retry_wakeup_on_run: bool,
        control_wakeup_requested: bool,
        wakeup_request_count: usize,
        block_retry_wakeup_requested: bool,
        pmem_retry_wakeup_requested: bool,
        network_retry_wakeup_requested: bool,
        entropy_retry_wakeup_requested: bool,
        scheduled_block_retry_wakeups: Vec<Option<Duration>>,
        scheduled_pmem_retry_wakeups: Vec<Option<Duration>>,
        scheduled_network_retry_wakeups: Vec<Option<Duration>>,
        scheduled_entropy_retry_wakeups: Vec<Option<Duration>>,
        pmem_retry_cancel_count: usize,
        network_retry_cancel_count: usize,
    }

    impl RecordingBootSessionRunLoopSession {
        fn new(run_results: impl IntoIterator<Item = HvfVcpuRunStepOutcome>) -> Self {
            Self {
                run_results: run_results.into_iter().map(Ok).collect(),
                dispatch_results: VecDeque::new(),
                pmem_dispatch_results: VecDeque::new(),
                monitor_wakeup_results: VecDeque::new(),
                network_dispatch_results: VecDeque::new(),
                vsock_dispatch_results: VecDeque::new(),
                balloon_dispatch_results: VecDeque::new(),
                memory_hotplug_dispatch_results: VecDeque::new(),
                entropy_dispatch_results: VecDeque::new(),
                timer_results: VecDeque::new(),
                events: Vec::new(),
                request_stop_on_run: None,
                request_stop_on_dispatch: None,
                request_stop_on_pmem_dispatch: None,
                request_stop_on_network_dispatch: None,
                request_stop_on_vsock_dispatch: None,
                request_stop_on_balloon_dispatch: None,
                request_stop_on_memory_hotplug_dispatch: None,
                request_stop_on_entropy_dispatch: None,
                request_stop_on_timer: None,
                request_network_retry_wakeup_on_run: false,
                control_wakeup_requested: false,
                wakeup_request_count: 0,
                block_retry_wakeup_requested: false,
                pmem_retry_wakeup_requested: false,
                network_retry_wakeup_requested: false,
                entropy_retry_wakeup_requested: false,
                scheduled_block_retry_wakeups: Vec::new(),
                scheduled_pmem_retry_wakeups: Vec::new(),
                scheduled_network_retry_wakeups: Vec::new(),
                scheduled_entropy_retry_wakeups: Vec::new(),
                pmem_retry_cancel_count: 0,
                network_retry_cancel_count: 0,
            }
        }

        fn with_run_error(source: HvfVcpuRunnerError) -> Self {
            Self {
                run_results: VecDeque::from([Err(source)]),
                dispatch_results: VecDeque::new(),
                pmem_dispatch_results: VecDeque::new(),
                monitor_wakeup_results: VecDeque::new(),
                network_dispatch_results: VecDeque::new(),
                vsock_dispatch_results: VecDeque::new(),
                balloon_dispatch_results: VecDeque::new(),
                memory_hotplug_dispatch_results: VecDeque::new(),
                entropy_dispatch_results: VecDeque::new(),
                timer_results: VecDeque::new(),
                events: Vec::new(),
                request_stop_on_run: None,
                request_stop_on_dispatch: None,
                request_stop_on_pmem_dispatch: None,
                request_stop_on_network_dispatch: None,
                request_stop_on_vsock_dispatch: None,
                request_stop_on_balloon_dispatch: None,
                request_stop_on_memory_hotplug_dispatch: None,
                request_stop_on_entropy_dispatch: None,
                request_stop_on_timer: None,
                request_network_retry_wakeup_on_run: false,
                control_wakeup_requested: false,
                wakeup_request_count: 0,
                block_retry_wakeup_requested: false,
                pmem_retry_wakeup_requested: false,
                network_retry_wakeup_requested: false,
                entropy_retry_wakeup_requested: false,
                scheduled_block_retry_wakeups: Vec::new(),
                scheduled_pmem_retry_wakeups: Vec::new(),
                scheduled_network_retry_wakeups: Vec::new(),
                scheduled_entropy_retry_wakeups: Vec::new(),
                pmem_retry_cancel_count: 0,
                network_retry_cancel_count: 0,
            }
        }

        fn push_dispatch_error(&mut self, source: HvfArm64BootBlockNotificationDispatchError) {
            self.dispatch_results.push_back(Err(source));
        }

        fn push_block_retry_dispatch(&mut self, retry_after: Duration) {
            self.dispatch_results.push_back(Ok(
                super::HvfArm64BootBlockNotificationDispatches::new_for_test_with_rate_limiter_retry_after(
                    retry_after,
                ),
            ));
        }

        fn push_network_retry_dispatch(&mut self, retry_after: Duration) {
            self.network_dispatch_results.push_back(Ok(
                super::HvfArm64BootNetworkNotificationDispatches::new_for_test_with_rate_limiter_retry_after(
                    retry_after,
                ),
            ));
        }

        fn push_pmem_retry_dispatch(&mut self, retry_after: Duration) {
            self.pmem_dispatch_results.push_back(Ok(
                super::HvfArm64BootPmemNotificationDispatches::new_for_test_with_rate_limiter_retry_after(
                    retry_after,
                ),
            ));
        }

        fn push_entropy_retry_dispatch(&mut self, retry_after: Duration) {
            self.entropy_dispatch_results.push_back(Ok(
                super::HvfArm64BootEntropyNotificationDispatches::new_for_test_with_rate_limiter_retry_after(
                    retry_after,
                ),
            ));
        }

        fn push_pmem_dispatch_error(&mut self, source: HvfArm64BootPmemNotificationDispatchError) {
            self.pmem_dispatch_results.push_back(Err(source));
        }

        fn push_monitor_wakeup(&mut self) {
            self.monitor_wakeup_results.push_back((true, false));
        }

        fn push_block_monitor_wakeup(&mut self) {
            self.monitor_wakeup_results.push_back((true, true));
        }

        fn push_network_dispatch_error(
            &mut self,
            source: HvfArm64BootNetworkNotificationDispatchError,
        ) {
            self.network_dispatch_results.push_back(Err(source));
        }

        fn push_vsock_dispatch_error(
            &mut self,
            source: HvfArm64BootVsockNotificationDispatchError,
        ) {
            self.vsock_dispatch_results.push_back(Err(source));
        }

        fn push_balloon_dispatch_error(
            &mut self,
            source: HvfArm64BootBalloonNotificationDispatchError,
        ) {
            self.balloon_dispatch_results.push_back(Err(source));
        }

        fn push_memory_hotplug_dispatch_error(
            &mut self,
            source: HvfArm64BootMemoryHotplugNotificationDispatchError,
        ) {
            self.memory_hotplug_dispatch_results.push_back(Err(source));
        }

        fn push_entropy_dispatch_error(
            &mut self,
            source: HvfArm64BootEntropyNotificationDispatchError,
        ) {
            self.entropy_dispatch_results.push_back(Err(source));
        }

        fn push_timer_error(&mut self, source: HvfVcpuRunnerError) {
            self.timer_results.push_back(Err(source));
        }

        fn request_stop_on_run(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_run = Some(stop_token);
        }

        fn request_stop_on_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_dispatch = Some(stop_token);
        }

        fn request_stop_on_pmem_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_pmem_dispatch = Some(stop_token);
        }

        fn request_stop_on_network_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_network_dispatch = Some(stop_token);
        }

        fn request_stop_on_vsock_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_vsock_dispatch = Some(stop_token);
        }

        fn request_stop_on_balloon_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_balloon_dispatch = Some(stop_token);
        }

        fn request_stop_on_memory_hotplug_dispatch(
            &mut self,
            stop_token: HvfArm64BootRunLoopStopToken,
        ) {
            self.request_stop_on_memory_hotplug_dispatch = Some(stop_token);
        }

        fn request_stop_on_entropy_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_entropy_dispatch = Some(stop_token);
        }

        fn request_stop_on_timer(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_timer = Some(stop_token);
        }

        const fn request_run_loop_wakeup(&mut self) {
            self.wakeup_request_count = self.wakeup_request_count.saturating_add(1);
        }

        const fn request_run_loop_block_retry_wakeup(&mut self) {
            self.block_retry_wakeup_requested = true;
        }

        const fn request_run_loop_pmem_retry_wakeup(&mut self) {
            self.pmem_retry_wakeup_requested = true;
        }

        const fn request_run_loop_network_retry_wakeup(&mut self) {
            self.network_retry_wakeup_requested = true;
        }

        const fn request_network_retry_wakeup_on_run(&mut self) {
            self.request_network_retry_wakeup_on_run = true;
        }

        const fn request_run_loop_entropy_retry_wakeup(&mut self) {
            self.entropy_retry_wakeup_requested = true;
        }

        const fn request_run_loop_control_wakeup(&mut self) {
            self.control_wakeup_requested = true;
        }
    }

    impl super::BootSessionRunLoopSession for RecordingBootSessionRunLoopSession {
        fn start_run_loop_vsock_wakeup_monitor(
            &mut self,
        ) -> Result<
            super::HvfArm64BootRunLoopWakeupMonitor,
            super::HvfArm64BootRunLoopWakeupMonitorError,
        > {
            let (completed_wakeup, has_block_wakeup_fds) =
                self.monitor_wakeup_results.pop_front().unwrap_or_default();
            if completed_wakeup {
                self.wakeup_request_count = self.wakeup_request_count.saturating_add(1);
            }

            Ok(if has_block_wakeup_fds {
                super::HvfArm64BootRunLoopWakeupMonitor::completed_block_for_test(completed_wakeup)
            } else {
                super::HvfArm64BootRunLoopWakeupMonitor::completed_for_test(completed_wakeup)
            })
        }

        fn take_run_loop_wakeup_request(&mut self) -> bool {
            let Some(remaining) = self.wakeup_request_count.checked_sub(1) else {
                return false;
            };
            self.wakeup_request_count = remaining;
            true
        }

        fn take_run_loop_control_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.control_wakeup_requested;
            self.control_wakeup_requested = false;
            wakeup_requested
        }

        fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.block_retry_wakeup_requested;
            self.block_retry_wakeup_requested = false;
            wakeup_requested
        }

        fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
            self.scheduled_block_retry_wakeups.push(retry_after);
        }

        fn take_run_loop_pmem_retry_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.pmem_retry_wakeup_requested;
            self.pmem_retry_wakeup_requested = false;
            wakeup_requested
        }

        fn schedule_run_loop_pmem_retry_wakeup(&mut self, retry_after: Option<Duration>) {
            self.scheduled_pmem_retry_wakeups.push(retry_after);
        }

        fn cancel_run_loop_pmem_retry_wakeup(&mut self) {
            self.pmem_retry_wakeup_requested = false;
            self.pmem_retry_cancel_count += 1;
        }

        fn take_run_loop_network_retry_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.network_retry_wakeup_requested;
            self.network_retry_wakeup_requested = false;
            wakeup_requested
        }

        fn schedule_run_loop_network_retry_wakeup(&mut self, retry_after: Option<Duration>) {
            self.scheduled_network_retry_wakeups.push(retry_after);
        }

        fn cancel_run_loop_network_retry_wakeup(&mut self) {
            self.network_retry_wakeup_requested = false;
            self.network_retry_cancel_count += 1;
        }

        fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.entropy_retry_wakeup_requested;
            self.entropy_retry_wakeup_requested = false;
            wakeup_requested
        }

        fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
            self.scheduled_entropy_retry_wakeups.push(retry_after);
        }

        fn run_loop_vcpu_step(
            &mut self,
        ) -> Result<HvfVcpuRunStepOutcome, super::HvfArm64BootVcpuError> {
            self.events.push("run");
            if self.request_network_retry_wakeup_on_run {
                self.network_retry_wakeup_requested = true;
                self.request_network_retry_wakeup_on_run = false;
            }
            if let Some(stop_token) = self.request_stop_on_run.take() {
                stop_token.request_stop();
            }

            self.run_results
                .pop_front()
                .expect("test run result should be queued")
                .map_err(super::HvfArm64BootVcpuError::from)
        }

        fn handle_run_loop_virtual_timer(&mut self) -> Result<(), super::HvfArm64BootVcpuError> {
            self.events.push("timer");
            if let Some(stop_token) = self.request_stop_on_timer.take() {
                stop_token.request_stop();
            }

            self.timer_results
                .pop_front()
                .unwrap_or(Ok(()))
                .map_err(super::HvfArm64BootVcpuError::from)
        }

        fn dispatch_run_loop_block_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootBlockNotificationDispatches,
            HvfArm64BootBlockNotificationDispatchError,
        > {
            self.events.push("dispatch");
            if let Some(stop_token) = self.request_stop_on_dispatch.take() {
                stop_token.request_stop();
            }

            self.dispatch_results.pop_front().unwrap_or_else(|| {
                Ok(super::HvfArm64BootBlockNotificationDispatches::new(
                    Vec::new(),
                ))
            })
        }

        fn dispatch_run_loop_pmem_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootPmemNotificationDispatches,
            HvfArm64BootPmemNotificationDispatchError,
        > {
            self.events.push("pmem-dispatch");
            if let Some(stop_token) = self.request_stop_on_pmem_dispatch.take() {
                stop_token.request_stop();
            }

            self.pmem_dispatch_results.pop_front().unwrap_or_else(|| {
                Ok(super::HvfArm64BootPmemNotificationDispatches::new(
                    Vec::new(),
                ))
            })
        }

        fn dispatch_run_loop_network_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootNetworkNotificationDispatches,
            HvfArm64BootNetworkNotificationDispatchError,
        > {
            self.events.push("network-dispatch");
            if let Some(stop_token) = self.request_stop_on_network_dispatch.take() {
                stop_token.request_stop();
            }

            self.network_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootNetworkNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }

        fn dispatch_run_loop_vsock_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootVsockNotificationDispatches,
            HvfArm64BootVsockNotificationDispatchError,
        > {
            self.events.push("vsock-dispatch");
            if let Some(stop_token) = self.request_stop_on_vsock_dispatch.take() {
                stop_token.request_stop();
            }

            self.vsock_dispatch_results.pop_front().unwrap_or_else(|| {
                Ok(super::HvfArm64BootVsockNotificationDispatches::new(
                    Vec::new(),
                ))
            })
        }

        fn dispatch_run_loop_balloon_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootBalloonNotificationDispatches,
            HvfArm64BootBalloonNotificationDispatchError,
        > {
            self.events.push("balloon-dispatch");
            if let Some(stop_token) = self.request_stop_on_balloon_dispatch.take() {
                stop_token.request_stop();
            }

            self.balloon_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootBalloonNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }

        fn dispatch_run_loop_memory_hotplug_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootMemoryHotplugNotificationDispatches,
            HvfArm64BootMemoryHotplugNotificationDispatchError,
        > {
            self.events.push("memory-hotplug-dispatch");
            if let Some(stop_token) = self.request_stop_on_memory_hotplug_dispatch.take() {
                stop_token.request_stop();
            }

            self.memory_hotplug_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootMemoryHotplugNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }

        fn dispatch_run_loop_entropy_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootEntropyNotificationDispatches,
            HvfArm64BootEntropyNotificationDispatchError,
        > {
            self.events.push("entropy-dispatch");
            if let Some(stop_token) = self.request_stop_on_entropy_dispatch.take() {
                stop_token.request_stop();
            }

            self.entropy_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootEntropyNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }
    }

    fn max_steps(steps: usize) -> NonZeroUsize {
        NonZeroUsize::new(steps).expect("test step limit should be non-zero")
    }

    fn mmio_run_step_outcome() -> HvfVcpuRunStepOutcome {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(MmioRegionId::new(7), GuestAddress::new(0x1000), 0x100)
            .expect("test MMIO region should insert");
        let exit = HvfExceptionExit {
            syndrome: data_abort_syndrome(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Write,
                HvfMmioRegister::new(0).expect("test MMIO register should exist"),
            ),
            virtual_address: 0x2000,
            physical_address: 0x1040,
        };
        let resolved = exit
            .decode_mmio_access()
            .expect("test MMIO exit should decode")
            .resolve(dispatcher.bus())
            .expect("test MMIO access should resolve");

        HvfVcpuRunStepOutcome::Mmio {
            access: resolved,
            outcome: MmioDispatchOutcome::Write,
        }
    }

    fn hvc_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::Hvc {
            exit: hvc_exit(0),
            function_id: PSCI_VERSION,
            return_value: PSCI_VERSION_1_0,
        }
    }

    fn guest_shutdown_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::GuestShutdown {
            exit: hvc_exit(0),
            function_id: PSCI_SYSTEM_OFF,
            return_value: PSCI_RET_SUCCESS,
        }
    }

    fn guest_reset_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::GuestReset {
            exit: hvc_exit(0),
            function_id: PSCI_SYSTEM_RESET,
            return_value: PSCI_RET_SUCCESS,
        }
    }

    fn sys64_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::Sys64 { exit: sys64_exit() }
    }

    fn sys64_exit() -> HvfSys64Exit {
        let exit = HvfExceptionExit {
            syndrome: sys64_osdlr_syndrome(),
            virtual_address: 0,
            physical_address: 0,
        };

        exit.decode_sys64().expect("test SYS64 exit should decode")
    }

    fn sys64_osdlr_syndrome() -> u64 {
        (ESR_EC_SYS64 << ESR_EC_SHIFT)
            | ESR_ISS_SYS64_DIRECTION
            | (2 << 20)
            | (1 << 10)
            | (3 << 1)
            | (4 << 17)
    }

    fn hvc_exit(immediate: u16) -> HvfHvcExit {
        let exit = HvfExceptionExit {
            syndrome: hvc_syndrome(immediate),
            virtual_address: 0,
            physical_address: 0,
        };

        exit.decode_hvc().expect("test HVC exit should decode")
    }

    fn hvc_syndrome(immediate: u16) -> u64 {
        (ESR_EC_HVC << ESR_EC_SHIFT) | u64::from(immediate)
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

        (ESR_EC_DATA_ABORT_LOWER_EL << ESR_EC_SHIFT)
            | ESR_ISS_ISV
            | (size_bits << ESR_ISS_SAS_SHIFT)
            | (u64::from(register.raw_value()) << ESR_ISS_SRT_SHIFT)
            | write_bit
            | ESR_ISS_SF
    }

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

    fn line_values(lines: &[bangbang_runtime::interrupt::GuestInterruptLine]) -> Vec<u32> {
        lines.iter().map(|line| line.raw_value()).collect()
    }

    fn line(value: u32) -> GuestInterruptLine {
        GuestInterruptLine::new(value).expect("test interrupt line should be valid")
    }

    fn recorded_lines(lines: &Arc<Mutex<Vec<GuestInterruptLine>>>) -> Vec<u32> {
        lines
            .lock()
            .expect("recorded interrupt lines should be readable")
            .iter()
            .map(|line| line.raw_value())
            .collect()
    }

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-hvf-startup-{name}-{}-{id}",
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

    fn temp_sized_file(name: &str, len: u64) -> TempFile {
        let path = temp_path(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test file should be created");
        file.set_len(len).expect("test file length should be set");
        TempFile { path }
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

    fn controller_with_kernel(kernel: &Path) -> bangbang_runtime::VmmController {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                1,
                TEST_MEMORY_MIB,
            )))
            .expect("machine config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                kernel.to_path_buf(),
            )))
            .expect("boot source should be stored");
        controller
    }

    fn add_drive(
        controller: &mut bangbang_runtime::VmmController,
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

    fn add_network(
        controller: &mut bangbang_runtime::VmmController,
        iface_id: &str,
        host_dev_name: &str,
    ) {
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new(iface_id, iface_id, host_dev_name),
            ))
            .expect("network interface config should be stored");
    }

    fn add_network_with_tx_rate_limiter(
        controller: &mut bangbang_runtime::VmmController,
        iface_id: &str,
        host_dev_name: &str,
        tx_rate_limiter: NetworkRateLimiterConfig,
    ) {
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new(iface_id, iface_id, host_dev_name)
                    .with_tx_rate_limiter(tx_rate_limiter),
            ))
            .expect("network interface config with TX rate limiter should be stored");
    }

    fn add_vsock(
        controller: &mut bangbang_runtime::VmmController,
        guest_cid: u32,
        uds_path: &Path,
    ) {
        controller
            .handle_action(VmmAction::PutVsock(VsockConfigInput::new(
                guest_cid,
                uds_path.to_string_lossy().into_owned(),
            )))
            .expect("vsock config should be stored");
    }

    fn add_balloon(controller: &mut bangbang_runtime::VmmController, amount_mib: u32) {
        controller
            .handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(
                amount_mib, false,
            )))
            .expect("balloon config should be stored");
    }

    fn add_memory_hotplug(controller: &mut bangbang_runtime::VmmController) {
        controller
            .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
                1024, 2, 128,
            )))
            .expect("memory-hotplug config should be stored");
    }

    fn add_pmem(controller: &mut bangbang_runtime::VmmController, id: &str, path: &Path) {
        controller
            .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
                id,
                path.to_string_lossy().into_owned(),
            )))
            .expect("pmem config should be stored");
    }

    fn valid_boot_resource_config(lines: &[GuestInterruptLine]) -> Arm64BootResourceConfig<'_> {
        valid_boot_resource_config_with_network_lines(lines, &[])
    }

    fn valid_boot_resource_config_with_network_lines<'a>(
        block_lines: &'a [GuestInterruptLine],
        network_lines: &'a [GuestInterruptLine],
    ) -> Arm64BootResourceConfig<'a> {
        valid_boot_resource_config_with_network_and_vsock_lines(block_lines, network_lines, None)
    }

    fn valid_boot_resource_config_with_network_and_vsock_lines<'a>(
        block_lines: &'a [GuestInterruptLine],
        network_lines: &'a [GuestInterruptLine],
        vsock_line: Option<GuestInterruptLine>,
    ) -> Arm64BootResourceConfig<'a> {
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[0],
            cache_hierarchy: test_cache_hierarchy(),
            gic: valid_fdt_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
            rtc_device: None,
            serial_device: None,
            vmgenid_interrupt_line: line(127),
            vmclock_interrupt_line: line(126),
            block_mmio_layout: BlockMmioLayout::new(TEST_BLOCK_MMIO_BASE, MmioRegionId::new(1)),
            block_interrupt_lines: block_lines,
            pmem_mmio_layout: PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, MmioRegionId::new(25)),
            pmem_interrupt_lines: &[],
            network_mmio_layout: NetworkMmioLayout::new(
                TEST_NETWORK_MMIO_BASE,
                MmioRegionId::new(50),
            ),
            network_interrupt_lines: network_lines,
            vsock_mmio_layout: VsockMmioLayout::new(
                GuestAddress::new(0x4000_6000),
                MmioRegionId::new(90),
            ),
            vsock_interrupt_line: vsock_line,
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            memory_hotplug_device: None,
            entropy_device: None,
        }
    }

    fn test_cache_hierarchy() -> Arm64FdtCacheHierarchy {
        Arm64FdtCacheHierarchy::new(vec![
            Arm64FdtCache::new(1, Arm64FdtCacheType::Unified, 32_768, 64, 64, 8, 1)
                .expect("test L1 cache should be valid"),
        ])
        .expect("test cache hierarchy should be valid")
    }

    fn valid_fdt_gic() -> Arm64FdtGic {
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

    fn boot_runtime_without_drives() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-no-drives", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config(&[]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_drives(
        drives: &[(&str, &[u8], bool)],
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-drives", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(drives.len())
            .expect("test block files should reserve");

        for (id, bytes, is_root_device) in drives {
            let block = temp_file(id, bytes);
            add_drive(&mut controller, id, block.path(), *is_root_device);
            blocks.push(block);
        }

        let interrupt_lines: Vec<_> = (0..drives.len())
            .map(|index| {
                line(32 + u32::try_from(index).expect("test block device index should fit in u32"))
            })
            .collect();
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config(&interrupt_lines),
        )
        .expect("boot resources should assemble");
        drop(blocks);
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_pmem(
        devices: &[&str],
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-pmem", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        let mut pmem_files = Vec::new();
        pmem_files
            .try_reserve_exact(devices.len())
            .expect("test pmem files should reserve");

        for id in devices {
            let pmem = temp_sized_file(id, VIRTIO_PMEM_ALIGNMENT);
            add_pmem(&mut controller, id, pmem.path());
            pmem_files.push(pmem);
        }

        let interrupt_lines: Vec<_> = (0..devices.len())
            .map(|index| {
                line(32 + u32::try_from(index).expect("test pmem device index should fit in u32"))
            })
            .collect();
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                pmem_interrupt_lines: &interrupt_lines,
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        drop(pmem_files);
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_networks(
        interfaces: &[(&str, &str)],
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-networks", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());

        for (iface_id, host_dev_name) in interfaces {
            add_network(&mut controller, iface_id, host_dev_name);
        }

        let interrupt_lines: Vec<_> = (0..interfaces.len())
            .map(|index| {
                line(
                    32 + u32::try_from(index).expect("test network device index should fit in u32"),
                )
            })
            .collect();
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config_with_network_lines(&[], &interrupt_lines),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_network_tx_rate_limiter(
        tx_rate_limiter: NetworkRateLimiterConfig,
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-network-rate-limiter", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network_with_tx_rate_limiter(&mut controller, "eth0", "tap0", tx_rate_limiter);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config_with_network_lines(&[], &[line(32)]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_vsock() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-vsock", &arm64_image());
        let uds_path = temp_path("vsock-test.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 42, &uds_path);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config_with_network_and_vsock_lines(&[], &[], Some(line(32))),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-balloon", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_balloon(&mut controller, TEST_MEMORY_MIB as u32);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(32)),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_memory_hotplug() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher)
    {
        let kernel = temp_file("kernel-with-memory-hotplug", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_memory_hotplug(&mut controller);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                memory_hotplug_device: Some(Arm64BootMemoryHotplugDeviceConfig::new(
                    VirtioMemMmioLayout::new(TEST_MEMORY_HOTPLUG_MMIO_BASE, MmioRegionId::new(120)),
                    line(32),
                )),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon_stats() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher)
    {
        let kernel = temp_file("kernel-with-balloon-stats", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        controller
            .handle_action(VmmAction::PutBalloon(
                BalloonConfigInput::new(TEST_MEMORY_MIB as u32, false)
                    .with_stats_polling_interval_s(1),
            ))
            .expect("balloon config should be stored");
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(32)),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon_reporting()
    -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-balloon-reporting", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        controller
            .handle_action(VmmAction::PutBalloon(
                BalloonConfigInput::new(TEST_MEMORY_MIB as u32, false)
                    .with_free_page_reporting(true),
            ))
            .expect("reporting balloon config should be stored");
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(32)),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble with reporting balloon");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_entropy() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-entropy", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                entropy_device: Some(Arm64BootEntropyDeviceConfig::new(
                    EntropyMmioLayout::new(TEST_ENTROPY_MMIO_BASE, MmioRegionId::new(100)),
                    line(32),
                )),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn dispatch_boot_block_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootBlockNotificationDispatches {
        runtime
            .dispatch_block_queue_notifications(memory, mmio_dispatcher)
            .expect("block notification dispatch result should allocate")
    }

    fn dispatch_boot_pmem_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootPmemNotificationDispatches {
        let mut flush_provider = |_: GuestMemoryRange| VirtioPmemFlushStatus::Success;
        dispatch_boot_pmem_notifications_with_provider(
            memory,
            runtime,
            mmio_dispatcher,
            &mut flush_provider,
        )
    }

    fn dispatch_boot_pmem_notifications_with_provider(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        flush_provider: &mut impl Arm64BootPmemFlushProvider,
    ) -> Arm64BootPmemNotificationDispatches {
        runtime
            .dispatch_pmem_queue_notifications(memory, mmio_dispatcher, flush_provider)
            .expect("pmem notification dispatch result should allocate")
    }

    fn dispatch_boot_network_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootNetworkNotificationDispatches {
        runtime
            .dispatch_network_queue_notifications(memory, mmio_dispatcher)
            .expect("network notification dispatch result should allocate")
    }

    fn dispatch_boot_network_notifications_with_packet_io(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        provider: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Arm64BootNetworkNotificationDispatches {
        dispatch_network_runtime_notifications_with_packet_io(
            memory,
            runtime,
            mmio_dispatcher,
            provider,
        )
        .expect("network packet I/O notification dispatch result should allocate")
    }

    fn dispatch_boot_vsock_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootVsockNotificationDispatches {
        runtime
            .dispatch_vsock_queue_notifications(memory, mmio_dispatcher)
            .expect("vsock notification dispatch result should allocate")
    }

    fn dispatch_boot_balloon_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootBalloonNotificationDispatches {
        runtime
            .dispatch_balloon_queue_notifications(memory, mmio_dispatcher)
            .expect("balloon notification dispatch result should allocate")
    }

    fn dispatch_boot_memory_hotplug_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootMemoryHotplugNotificationDispatches {
        runtime
            .dispatch_memory_hotplug_queue_notifications(memory, mmio_dispatcher)
            .expect("memory-hotplug notification dispatch result should allocate")
    }

    fn dispatch_boot_memory_hotplug_notifications_with_executor(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Arm64BootMemoryHotplugNotificationDispatches {
        dispatch_memory_hotplug_runtime_notifications_with_executor(
            memory,
            runtime,
            mmio_dispatcher,
            mutation_executor,
        )
        .expect("memory-hotplug notification dispatch result should allocate")
    }

    fn dispatch_boot_entropy_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        provider: &mut impl Arm64BootEntropySourceProvider,
    ) -> Arm64BootEntropyNotificationDispatches {
        runtime
            .dispatch_entropy_queue_notifications_with_source(memory, mmio_dispatcher, provider)
            .expect("entropy notification dispatch result should allocate")
    }

    fn write_boot_block_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime.block_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("block MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("block MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_block_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
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
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("block MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_pmem_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime.pmem_mmio_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("pmem MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("pmem MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_pmem_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime.pmem_mmio_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("pmem MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("pmem MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_network_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime.network_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("network MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("network MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_network_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
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
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("network MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_vsock_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("vsock MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("vsock MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_vsock_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("vsock MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("vsock MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_balloon_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("balloon MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("balloon MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_balloon_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("balloon MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("balloon MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_memory_hotplug_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("memory-hotplug device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("memory-hotplug MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("memory-hotplug MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_memory_hotplug_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("memory-hotplug device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("memory-hotplug MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("memory-hotplug MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_entropy_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
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
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("entropy MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("entropy MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_entropy_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
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
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("entropy MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("entropy MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn configure_boot_block_queue(
        runtime: &mut Arm64BootRuntimeResources,
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

    fn notify_boot_block_queue(
        runtime: &mut Arm64BootRuntimeResources,
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

    fn configure_boot_pmem_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        device_ring: GuestAddress,
    ) {
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueReady,
            1,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_pmem_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn configure_boot_entropy_queue(
        runtime: &mut Arm64BootRuntimeResources,
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
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_entropy_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueNotify, 0);
    }

    fn configure_boot_network_queues(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
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
        );
        configure_boot_network_queue(
            runtime,
            mmio_dispatcher,
            device_index,
            VIRTIO_NET_TX_QUEUE_INDEX,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_network_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        queue_index: usize,
    ) {
        let queue_index_u32 = u32::try_from(queue_index).expect("test queue index should fit");
        let (descriptor_table, driver_ring, device_ring) = network_queue_addresses(queue_index_u32);
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueSel,
            queue_index_u32,
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

    fn notify_boot_network_tx_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
        );
    }

    fn configure_boot_vsock_queues(
        runtime: &mut Arm64BootRuntimeResources,
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
        configure_boot_vsock_queue(runtime, mmio_dispatcher, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        configure_boot_vsock_queue(runtime, mmio_dispatcher, VIRTIO_VSOCK_TX_QUEUE_INDEX);
        configure_boot_vsock_queue(runtime, mmio_dispatcher, VIRTIO_VSOCK_EVENT_QUEUE_INDEX);
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_vsock_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        let queue_index_u32 = u32::try_from(queue_index).expect("test queue index should fit");
        let (descriptor_table, driver_ring, device_ring) = vsock_queue_addresses(queue_index_u32);
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueSel,
            queue_index_u32,
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

    fn notify_boot_vsock_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            queue_index.try_into().expect("queue index should fit"),
        );
    }

    fn configure_boot_balloon_queues(
        runtime: &mut Arm64BootRuntimeResources,
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
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_INFLATE_QUEUE_INDEX);
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX);
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_balloon_reporting_queues(
        runtime: &mut Arm64BootRuntimeResources,
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
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_INFLATE_QUEUE_INDEX);
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX);
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_STATS_QUEUE_INDEX);
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_balloon_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        let queue_index_u32 = u32::try_from(queue_index).expect("test queue index should fit");
        let (descriptor_table, driver_ring, device_ring) = balloon_queue_addresses(queue_index_u32);
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueSel,
            queue_index_u32,
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

    fn notify_boot_balloon_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            queue_index.try_into().expect("queue index should fit"),
        );
    }

    fn configure_boot_memory_hotplug_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
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
            guest_address_low(TEST_USED_RING),
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
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn update_boot_memory_hotplug_requested_size(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        requested_size_mib: u64,
    ) {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("test memory-hotplug config should be valid");
        let update = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(requested_size_mib))
            .expect("test memory-hotplug requested size should be valid");
        let device = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("test memory-hotplug device should exist");
        update_memory_hotplug_config_for_device(device, mmio_dispatcher, update)
            .expect("test memory-hotplug requested size update should succeed");
    }

    fn network_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match queue_index {
            0 => (
                TEST_NETWORK_RX_DESCRIPTOR_TABLE,
                TEST_NETWORK_RX_AVAILABLE_RING,
                TEST_NETWORK_RX_USED_RING,
            ),
            1 => (
                TEST_NETWORK_TX_DESCRIPTOR_TABLE,
                TEST_NETWORK_TX_AVAILABLE_RING,
                TEST_NETWORK_TX_USED_RING,
            ),
            other => panic!("unsupported test network queue index {other}"),
        }
    }

    fn vsock_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match usize::try_from(queue_index).expect("queue index should fit") {
            VIRTIO_VSOCK_RX_QUEUE_INDEX => (
                TEST_VSOCK_RX_DESCRIPTOR_TABLE,
                TEST_VSOCK_RX_AVAILABLE_RING,
                TEST_VSOCK_RX_USED_RING,
            ),
            VIRTIO_VSOCK_TX_QUEUE_INDEX => (
                TEST_VSOCK_TX_DESCRIPTOR_TABLE,
                TEST_VSOCK_TX_AVAILABLE_RING,
                TEST_VSOCK_TX_USED_RING,
            ),
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX => (
                TEST_VSOCK_EVENT_DESCRIPTOR_TABLE,
                TEST_VSOCK_EVENT_AVAILABLE_RING,
                TEST_VSOCK_EVENT_USED_RING,
            ),
            other => panic!("unsupported test vsock queue index {other}"),
        }
    }

    fn balloon_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match usize::try_from(queue_index).expect("queue index should fit") {
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX => (
                TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
                TEST_BALLOON_INFLATE_AVAILABLE_RING,
                TEST_BALLOON_INFLATE_USED_RING,
            ),
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX => (
                TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
                TEST_BALLOON_DEFLATE_AVAILABLE_RING,
                TEST_BALLOON_DEFLATE_USED_RING,
            ),
            VIRTIO_BALLOON_STATS_QUEUE_INDEX => (
                TEST_BALLOON_REPORTING_DESCRIPTOR_TABLE,
                TEST_BALLOON_REPORTING_AVAILABLE_RING,
                TEST_BALLOON_REPORTING_USED_RING,
            ),
            other => panic!("unsupported test balloon queue index {other}"),
        }
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

    fn write_queued_read_request(memory: &mut GuestMemory) {
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

    fn write_queued_pmem_flush_request(memory: &mut GuestMemory) {
        memory
            .write_slice(&VIRTIO_PMEM_REQUEST_TYPE_FLUSH.to_le_bytes(), HEADER_ADDR)
            .expect("pmem request type should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE, None),
        );
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_queued_pmem_flush_request(memory: &mut GuestMemory) {
        memory
            .write_slice(&VIRTIO_PMEM_REQUEST_TYPE_FLUSH.to_le_bytes(), HEADER_ADDR)
            .expect("pmem request type should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::writable(HEADER_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE, None),
        );
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_queued_flush_request(memory: &mut GuestMemory) {
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

    fn write_entropy_request(memory: &mut GuestMemory, len: u32) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, len, None));
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_entropy_request(memory: &mut GuestMemory) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, 16, None));
        write_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_queued_balloon_inflate_request(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_balloon_inflate_descriptor(
            memory,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_balloon_inflate_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_balloon_inflate_request(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_balloon_inflate_descriptor(
            memory,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_balloon_inflate_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_queued_balloon_deflate_request(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_balloon_deflate_descriptor(
            memory,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_balloon_deflate_available_heads(memory, &[0]);
    }

    fn write_queued_balloon_reporting_request(memory: &mut GuestMemory) {
        write_balloon_descriptor_at(
            memory,
            TEST_BALLOON_REPORTING_DESCRIPTOR_TABLE,
            0,
            TestDescriptor::writable(TEST_BALLOON_REPORTING_RANGE, 4096, None),
        );
        write_balloon_reporting_available_heads(memory, &[0]);
    }

    fn write_queued_memory_hotplug_request(
        memory: &mut GuestMemory,
        request_type: u16,
        address: GuestAddress,
        block_count: u16,
    ) {
        let mut request = Vec::new();
        request.extend_from_slice(&request_type.to_le_bytes());
        request.extend_from_slice(&[0; 6]);
        request.extend_from_slice(&address.raw_value().to_le_bytes());
        request.extend_from_slice(&block_count.to_le_bytes());
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

    fn write_queued_memory_hotplug_state_request(memory: &mut GuestMemory) {
        write_queued_memory_hotplug_request(
            memory,
            TEST_VIRTIO_MEM_REQ_STATE,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS,
            1,
        );
    }

    fn write_queued_memory_hotplug_plug_request(memory: &mut GuestMemory) {
        write_queued_memory_hotplug_request(
            memory,
            TEST_VIRTIO_MEM_REQ_PLUG,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS,
            1,
        );
    }

    fn write_request_header(
        memory: &mut GuestMemory,
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

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn write_balloon_inflate_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_balloon_descriptor_at(
            memory,
            TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
            index,
            descriptor,
        );
    }

    fn write_balloon_deflate_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_balloon_descriptor_at(
            memory,
            TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
            index,
            descriptor,
        );
    }

    fn write_balloon_descriptor_at(
        memory: &mut GuestMemory,
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
            .expect("balloon descriptor should write");
    }

    fn write_network_tx_header_at(memory: &mut GuestMemory, address: GuestAddress) {
        let mut bytes = [0; VIRTIO_NET_TX_HEADER_SIZE as usize];
        let (flags, tail) = bytes.split_at_mut(1);
        let (gso_type, tail) = tail.split_at_mut(1);
        let (header_len, tail) = tail.split_at_mut(2);
        let (gso_size, tail) = tail.split_at_mut(2);
        let (checksum_start, tail) = tail.split_at_mut(2);
        let (checksum_offset, num_buffers) = tail.split_at_mut(2);

        flags.copy_from_slice(&[0x1]);
        gso_type.copy_from_slice(&[0x2]);
        header_len.copy_from_slice(&0x0304_u16.to_le_bytes());
        gso_size.copy_from_slice(&0x0506_u16.to_le_bytes());
        checksum_start.copy_from_slice(&0x0708_u16.to_le_bytes());
        checksum_offset.copy_from_slice(&0x090a_u16.to_le_bytes());
        num_buffers.copy_from_slice(&0x0b0c_u16.to_le_bytes());

        memory
            .write_slice(&bytes, address)
            .expect("virtio-net TX header should write");
    }

    fn write_network_tx_header(memory: &mut GuestMemory) {
        write_network_tx_header_at(memory, TEST_NETWORK_TX_HEADER);
    }

    fn write_network_tx_descriptor(
        memory: &mut GuestMemory,
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

        let descriptor_address = TEST_NETWORK_TX_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("network TX descriptor should write");
    }

    fn write_network_tx_descriptors(memory: &mut GuestMemory, descriptors: &[TestDescriptor]) {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_network_tx_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit"),
                descriptor,
            );
        }
    }

    fn write_vsock_tx_packet_header(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &VirtioVsockPacketHeader::new().to_bytes(),
                TEST_VSOCK_HEADER,
            )
            .expect("virtio-vsock TX header should write");
    }

    fn write_vsock_tx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_VSOCK_TX_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("vsock TX descriptor should write");
    }

    fn write_vsock_tx_descriptors(memory: &mut GuestMemory, descriptors: &[TestDescriptor]) {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_vsock_tx_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit"),
                descriptor,
            );
        }
    }

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 field should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 field should read");
        u32::from_le_bytes(bytes)
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest bytes should read");
        bytes
    }

    fn read_entropy_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("entropy used idx address should not overflow"),
        )
    }

    fn read_memory_hotplug_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("memory-hotplug used idx address should not overflow"),
        )
    }

    fn read_pmem_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("pmem used idx address should not overflow"),
        )
    }

    fn read_pmem_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
        let element = TEST_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("pmem used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("pmem used element len address should not overflow"),
            ),
        )
    }

    fn read_entropy_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
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

    fn available_ring_idx_address() -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("available idx address should not overflow")
    }

    fn available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available entry address should not overflow")
    }

    fn write_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn network_tx_available_ring_idx_address() -> GuestAddress {
        TEST_NETWORK_TX_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("network TX available idx address should not overflow")
    }

    fn network_tx_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_NETWORK_TX_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("network TX available entry address should not overflow")
    }

    fn write_network_tx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                network_tx_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            network_tx_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn network_tx_used_ring_idx_address() -> GuestAddress {
        TEST_NETWORK_TX_USED_RING
            .checked_add(2)
            .expect("network TX used idx address should not overflow")
    }

    fn network_tx_used_ring_entry_address(index: usize) -> GuestAddress {
        TEST_NETWORK_TX_USED_RING
            .checked_add(4 + u64::try_from(index).expect("test index should fit") * 8)
            .expect("network TX used entry address should not overflow")
    }

    fn read_network_tx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, network_tx_used_ring_idx_address())
    }

    fn read_network_tx_used_element(memory: &GuestMemory, index: usize) -> (u32, u32) {
        let address = network_tx_used_ring_entry_address(index);
        let descriptor_head = read_guest_u32(memory, address);
        let len = read_guest_u32(
            memory,
            address
                .checked_add(4)
                .expect("network TX used-ring len address should not overflow"),
        );
        (descriptor_head, len)
    }

    fn vsock_tx_available_ring_idx_address() -> GuestAddress {
        TEST_VSOCK_TX_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("vsock TX available idx address should not overflow")
    }

    fn vsock_tx_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_VSOCK_TX_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("vsock TX available entry address should not overflow")
    }

    fn write_vsock_tx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                vsock_tx_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            vsock_tx_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn vsock_tx_used_ring_idx_address() -> GuestAddress {
        TEST_VSOCK_TX_USED_RING
            .checked_add(2)
            .expect("vsock TX used idx address should not overflow")
    }

    fn read_vsock_tx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, vsock_tx_used_ring_idx_address())
    }

    fn balloon_inflate_available_ring_idx_address() -> GuestAddress {
        TEST_BALLOON_INFLATE_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("balloon inflate available idx address should not overflow")
    }

    fn balloon_inflate_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_BALLOON_INFLATE_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("balloon inflate available entry address should not overflow")
    }

    fn write_balloon_inflate_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                balloon_inflate_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            balloon_inflate_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn balloon_deflate_available_ring_idx_address() -> GuestAddress {
        TEST_BALLOON_DEFLATE_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("balloon deflate available idx address should not overflow")
    }

    fn balloon_deflate_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_BALLOON_DEFLATE_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("balloon deflate available entry address should not overflow")
    }

    fn write_balloon_deflate_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                balloon_deflate_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            balloon_deflate_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn balloon_reporting_available_ring_idx_address() -> GuestAddress {
        TEST_BALLOON_REPORTING_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("balloon reporting available idx address should not overflow")
    }

    fn balloon_reporting_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_BALLOON_REPORTING_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("balloon reporting available entry address should not overflow")
    }

    fn write_balloon_reporting_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                balloon_reporting_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            balloon_reporting_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn read_balloon_inflate_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_INFLATE_USED_RING
                .checked_add(2)
                .expect("balloon inflate used idx address should not overflow"),
        )
    }

    fn read_balloon_deflate_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_DEFLATE_USED_RING
                .checked_add(2)
                .expect("balloon deflate used idx address should not overflow"),
        )
    }

    fn read_balloon_reporting_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_REPORTING_USED_RING
                .checked_add(2)
                .expect("balloon reporting used idx address should not overflow"),
        )
    }

    #[test]
    fn block_notification_signal_dispatch_accepts_empty_devices() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_block_notification_dispatches(dispatches)
            .expect("empty dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
        assert_eq!(result.rate_limiter_retry_after(), None);
    }

    #[test]
    fn block_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.drive_id(), "rootfs");
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn block_notification_signal_dispatch_signals_queued_request() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", payload.as_slice(), true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
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
    }

    #[test]
    fn block_notification_signal_dispatch_keeps_multiple_devices_independent() {
        let first_payload = [0x11; 512];
        let second_payload = vec![0x22; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_drives(&[
            ("rootfs", first_payload.as_slice(), true),
            ("data", second_payload.as_slice(), false),
        ]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("multi-device dispatch should collect");

        assert_eq!(result.len(), 2);
        let first = &result.as_slice()[0];
        let second = &result.as_slice()[1];
        assert_eq!(first.dispatch().device().registration.drive_id(), "rootfs");
        assert_eq!(second.dispatch().device().registration.drive_id(), "data");
        assert!(!first.queue_interrupt_signaled());
        assert!(second.queue_interrupt_signaled());
        assert_eq!(recorded_lines(&lines), vec![33]);
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
    fn block_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_flush_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.dispatch().outcome().dispatch_error().is_some());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_guest_bytes(&memory, STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE as usize),
            [VIRTIO_BLOCK_STATUS_OK]
        );
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", payload.as_slice(), true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected signal failure");

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn block_metrics_record_successful_signal_dispatch() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_drives(&[
            ("rootfs", payload.as_slice(), true),
            ("data", payload.as_slice(), false),
        ]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::successful();
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued dispatch should collect");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs", "data"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        let aggregate = metrics.aggregate_snapshot();
        let read_agg = aggregate.read_agg();
        assert!(!read_agg.is_empty());
        assert_eq!(
            aggregate,
            BlockDeviceMetrics::default()
                .with_queue_event_count(1)
                .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                .with_read_count(1)
                .with_read_agg(read_agg)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "data",
                BlockDeviceMetrics::default()
                    .with_queue_event_count(1)
                    .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                    .with_read_count(1)
                    .with_read_agg(read_agg),
            )
        );
    }

    #[test]
    fn block_metrics_preserve_partial_dispatch_before_signal() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_flush_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::successful();
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial dispatch result should collect");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        assert_eq!(
            metrics.aggregate_snapshot(),
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_flush_count(1)
                .with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_flush_count(1)
                    .with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn block_metrics_record_signal_failure() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", payload.as_slice(), true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::failing("injected signal failure");
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("signal failure should stay per-device");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        let aggregate = metrics.aggregate_snapshot();
        let read_agg = aggregate.read_agg();
        assert!(!read_agg.is_empty());
        assert_eq!(
            aggregate,
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(1)
                .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                .with_read_count(1)
                .with_read_agg(read_agg)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1)
                    .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                    .with_read_count(1)
                    .with_read_agg(read_agg),
            )
        );
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn block_metrics_record_handler_lookup_failure() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::successful();
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing handler dispatch should collect");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        assert_eq!(
            metrics.aggregate_snapshot(),
            BlockDeviceMetrics::default().with_event_fails(1)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics("rootfs", BlockDeviceMetrics::default().with_event_fails(1),)
        );
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_wrong_handler_without_signal() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        let region = runtime.block_devices[0].registration.region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement region should insert");
        mmio_dispatcher
            .register_handler(region.id(), WrongBlockHandler)
            .expect("wrong handler should register");
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("wrong handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn pmem_notification_dispatch_flushes_only_notified_device_identity() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_pmem(&["pmem0", "pmem1"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let selected = runtime.pmem_mmio_devices[1].registration.clone();
        let mut flush_calls = Vec::new();
        let mut flush_provider = |guest_range: GuestMemoryRange| {
            flush_calls.push(guest_range);
            VirtioPmemFlushStatus::Failure
        };

        let dispatches = dispatch_boot_pmem_notifications_with_provider(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut flush_provider,
        );

        assert_eq!(flush_calls, [selected.guest_range()]);
        assert!(
            dispatches.as_slice()[0]
                .outcome()
                .dispatched()
                .expect("idle peer should dispatch as a no-op")
                .queue_dispatch()
                .is_none()
        );
        let selected_dispatch = dispatches.as_slice()[1]
            .outcome()
            .dispatched()
            .expect("notified pmem device should dispatch")
            .queue_dispatch()
            .expect("notified pmem queue should be present");
        assert_eq!(selected_dispatch.failed_flushes(), 1);
        assert_eq!(selected_dispatch.successful_flushes(), 0);
        assert_eq!(
            read_guest_bytes(&memory, STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE as usize),
            bangbang_runtime::pmem::VIRTIO_PMEM_STATUS_FAILURE.to_le_bytes()
        );
    }

    #[test]
    fn pmem_notification_dispatch_skips_flush_for_malformed_only_event() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_pmem(&["pmem0"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut flush_calls = 0;
        let mut flush_provider = |_: GuestMemoryRange| {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };

        let dispatches = dispatch_boot_pmem_notifications_with_provider(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut flush_provider,
        );

        assert_eq!(flush_calls, 0);
        let dispatch = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("malformed pmem event should still dispatch")
            .queue_dispatch()
            .expect("malformed pmem queue should be present");
        assert_eq!(dispatch.parse_failures(), 1);
        assert_eq!(dispatch.successful_flushes(), 0);
        assert_eq!(dispatch.failed_flushes(), 0);
    }

    #[test]
    fn pmem_metrics_record_successful_signal_dispatch() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_pmem(&["pmem0", "pmem1"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();
        let result = signal_pmem_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued pmem dispatch should collect");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0", "pmem1"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![33]);
        assert_eq!(
            read_boot_pmem_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                1,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_pmem_used_index(&memory), 1);
        assert_eq!(
            read_pmem_used_element(&memory, 0),
            (0, VIRTIO_PMEM_STATUS_SIZE)
        );
        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default().with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem1",
                PmemDeviceMetrics::default().with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn pmem_metrics_record_parse_failure_before_signal() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_pmem(&["pmem0"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();
        let result = signal_pmem_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial pmem dispatch result should collect");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(read_pmem_used_index(&memory), 1);
        assert_eq!(read_pmem_used_element(&memory, 0), (0, 0));
        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn pmem_metrics_record_signal_failure() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_pmem(&["pmem0"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected pmem signal failure");
        let result = signal_pmem_queue_interrupts(dispatches, sink.as_ref())
            .expect("pmem signal failure should stay per-device");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![32]);
        assert!(result.has_signal_failure());
        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn pmem_metrics_record_handler_lookup_failure() {
        let (mut memory, mut runtime, _) = boot_runtime_with_pmem(&["pmem0"]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result =
            signal_pmem_queue_interrupts(dispatches, RecordingSink::successful().1.as_ref())
                .expect("missing pmem handler dispatch should collect");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default().with_event_fails(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new()
                .with_device_metrics("pmem0", PmemDeviceMetrics::default().with_event_fails(1),)
        );
    }

    #[test]
    fn entropy_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let result = collect_entropy_notification_dispatches(dispatches)
            .expect("empty entropy dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
        assert!(provider.requested_regions.is_empty());
    }

    #[test]
    fn entropy_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop entropy dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(provider.requested_regions.is_empty());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn entropy_notification_signal_dispatch_signals_queued_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued entropy dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(provider.requested_regions, [MmioRegionId::new(100)]);
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, 16),
            (0_u8..16).collect::<Vec<_>>()
        );
        assert_eq!(read_entropy_used_index(&memory), 1);
        assert_eq!(read_entropy_used_element(&memory, 0), (0, 16));
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
    fn entropy_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_partially_invalid_entropy_request(&mut memory);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial entropy dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.dispatch().outcome().dispatch_error().is_some());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(read_entropy_used_index(&memory), 1);
        assert_eq!(read_entropy_used_element(&memory, 0), (0, 16));
    }

    #[test]
    fn entropy_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::failing("injected entropy signal failure");

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("entropy signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected entropy signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
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
    fn entropy_metrics_record_signal_failure() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::failing("injected entropy signal failure");
        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("entropy signal failure should stay per-device");
        let metrics = SharedEntropyDeviceMetrics::default();

        record_entropy_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            metrics.snapshot(),
            EntropyDeviceMetrics::default()
                .with_entropy_event_fails(1)
                .with_entropy_event_count(1)
                .with_entropy_bytes(16)
        );
    }

    #[test]
    fn network_notification_signal_dispatch_accepts_empty_devices() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_network_notification_dispatches(dispatches)
            .expect("empty network dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn network_notification_packet_io_dispatch_accepts_empty_devices() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let result = collect_network_notification_dispatches(dispatches)
            .expect("empty network dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
        assert_eq!(result.rate_limiter_retry_after(), None);
        assert!(provider.requested_ifaces.is_empty());
    }

    #[test]
    fn network_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop network dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.iface_id(), "eth0");
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn network_notification_packet_io_dispatch_skips_provider_without_pending() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop network packet I/O dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.iface_id(), "eth0");
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert!(provider.requested_ifaces.is_empty());
    }

    #[test]
    fn network_notification_signal_dispatch_signals_queued_tx_frame() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued network dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("network notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_frames(), 1);
        assert_eq!(tx.successful_frames(), 1);
        assert_eq!(tx.parse_failures(), 0);
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_packet_io_dispatch_routes_tx_frame_and_signals() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued network packet I/O dispatch should collect");

        assert_eq!(provider.requested_ifaces, ["eth0".to_string()]);
        assert_eq!(provider.tx_sink.packets, [vec![0xde, 0xad, 0xbe, 0xef]]);
        assert_eq!(provider.rx_source.peek_calls, 0);
        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("network notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_frames(), 1);
        assert_eq!(tx.sink_successful_frames(), 1);
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        let expected = NetworkInterfaceMetrics::default()
            .with_tx_queue_event_count(1)
            .with_tx_bytes_count(16)
            .with_tx_packets_count(1)
            .with_tx_count(1);
        assert_eq!(metrics.aggregate_snapshot(), expected);
        assert_eq!(
            metrics.per_interface_snapshot(),
            NetworkInterfaceMetricsByInterface::new().with_interface_metrics("eth0", expected)
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_packet_io_keeps_second_interface_signal_and_metrics_associated() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0"), ("eth1", "tap1")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 1);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("second-interface network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth1");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("two-interface network packet I/O dispatch should collect");

        assert_eq!(provider.requested_ifaces, ["eth1".to_string()]);
        assert_eq!(provider.tx_sink.packets, [vec![0xde, 0xad, 0xbe, 0xef]]);
        assert_eq!(provider.rx_source.peek_calls, 0);
        assert_eq!(result.len(), 2);
        let first = &result.as_slice()[0];
        assert_eq!(first.dispatch().device().registration.iface_id(), "eth0");
        assert!(!first.dispatch().needs_queue_interrupt());
        assert!(!first.queue_interrupt_signaled());
        assert!(first.signal_error().is_none());
        let second = &result.as_slice()[1];
        assert_eq!(second.dispatch().device().registration.iface_id(), "eth1");
        assert!(second.dispatch().needs_queue_interrupt());
        assert!(second.queue_interrupt_signaled());
        assert!(second.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![33]);
        let dispatch = second
            .dispatch()
            .outcome()
            .dispatched()
            .expect("second interface notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("second interface TX queue dispatch should be present");
        assert_eq!(tx.processed_frames(), 1);
        assert_eq!(tx.sink_successful_frames(), 1);
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                1,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0", "eth1"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        let expected = NetworkInterfaceMetrics::default()
            .with_tx_queue_event_count(1)
            .with_tx_bytes_count(16)
            .with_tx_packets_count(1)
            .with_tx_count(1);
        assert_eq!(metrics.aggregate_snapshot(), expected);
        assert_eq!(
            metrics.per_interface_snapshot(),
            NetworkInterfaceMetricsByInterface::new().with_interface_metrics("eth1", expected)
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_packet_io_dispatch_preserves_pending_on_provider_failure() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::failing_for("eth0");
        let failed = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );

        assert_eq!(provider.requested_ifaces, ["eth0".to_string()]);
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
        let (lines, sink) = RecordingSink::successful();
        let result = signal_network_queue_interrupts(failed, sink.as_ref())
            .expect("failed network packet I/O dispatch should collect");
        assert!(!result.has_signal_failure());
        assert!(recorded_lines(&lines).is_empty());
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default().with_event_fails(1)
        );
        assert_eq!(
            metrics.per_interface_snapshot(),
            NetworkInterfaceMetricsByInterface::new().with_interface_metrics(
                "eth0",
                NetworkInterfaceMetrics::default().with_event_fails(1),
            )
        );

        let retried =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        assert!(retried.needs_queue_interrupt());
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
                .expect("TX queue dispatch should be present")
                .processed_frames(),
            1
        );
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        write_network_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_NETWORK_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_network_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial network dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        let err = device
            .dispatch()
            .outcome()
            .dispatch_error()
            .expect("partial TX dispatch error should be preserved");
        let completed = err
            .completed_tx_dispatch()
            .expect("completed TX metadata should be preserved");
        assert_eq!(completed.processed_frames(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default()
                .with_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(16)
                .with_tx_packets_count(1)
                .with_tx_count(1)
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        write_network_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_NETWORK_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected network signal failure");

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("network signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("network signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected network signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default()
                .with_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(16)
                .with_tx_packets_count(1)
                .with_tx_count(1)
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
    fn network_notification_signal_failure_preserves_rate_limiter_retry_after() {
        let rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(16, None, 60_000)),
            None,
        );
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_network_tx_rate_limiter(rate_limiter);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header_at(&mut memory, TEST_NETWORK_TX_HEADER);
        write_network_tx_header_at(&mut memory, TEST_NETWORK_SECOND_TX_HEADER);
        memory
            .write_slice(&[0x10, 0x11, 0x12, 0x13], TEST_NETWORK_TX_PAYLOAD)
            .expect("first network TX payload should write");
        memory
            .write_slice(&[0x20, 0x21, 0x22, 0x23], TEST_NETWORK_SECOND_TX_PAYLOAD)
            .expect("second network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
                TestDescriptor::readable(
                    TEST_NETWORK_SECOND_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(3),
                ),
                TestDescriptor::readable(TEST_NETWORK_SECOND_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0, 2]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let retry_after = dispatches
            .rate_limiter_retry_after()
            .expect("runtime dispatch should expose retry timing");
        assert!(retry_after > Duration::ZERO);
        assert!(retry_after <= Duration::from_secs(60));
        let (lines, sink) = RecordingSink::failing("injected network signal failure");

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("network signal failure should stay per-device");

        assert!(result.has_signal_failure());
        assert_eq!(result.rate_limiter_retry_after(), Some(retry_after));
        let device = &result.as_slice()[0];
        assert_eq!(
            device.dispatch().rate_limiter_retry_after(),
            Some(retry_after)
        );
        assert!(device.signal_error().is_some());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("network notification should dispatch before signal failure");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_frames(), 1);
        assert_eq!(tx.rate_limiter_throttled_frames(), 1);
        assert_eq!(tx.rate_limiter_retry_after(), Some(retry_after));
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_networks(&[("eth0", "tap0")]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing network handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default().with_event_fails(1)
        );
    }

    #[test]
    fn network_notification_packet_io_dispatch_skips_provider_when_handler_missing() {
        let (mut memory, mut runtime, _) = boot_runtime_with_networks(&[("eth0", "tap0")]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing network handler packet I/O dispatch should collect");

        assert!(provider.requested_ifaces.is_empty());
        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_wrong_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_networks(&[("eth0", "tap0")]);
        let region = runtime.network_devices[0].registration.region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement network region should insert");
        mmio_dispatcher
            .register_handler(region.id(), WrongNetworkHandler)
            .expect("wrong network handler should register");
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("wrong network handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn vsock_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_vsock_notification_dispatches(dispatches)
            .expect("empty vsock dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn vsock_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop vsock dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.guest_cid(), 42);
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(metrics.snapshot(), VsockDeviceMetrics::default());
    }

    #[test]
    fn vsock_notification_signal_dispatch_signals_queued_tx_packet() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_vsock_tx_packet_header(&mut memory);
        write_vsock_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued vsock dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("vsock notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert_eq!(tx.parse_failures(), 0);
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default()
                .with_tx_queue_event_count(1)
                .with_tx_packets_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_skips_rx_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_RX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("unsupported vsock dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("RX noop should dispatch");
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
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default().with_rx_queue_event_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_event_noop_without_signal() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("event no-op vsock dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("event notification should be accepted as no-op dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_VSOCK_EVENT_QUEUE_INDEX]
        );
        assert_eq!(dispatch.event_notifications(), 1);
        assert!(dispatch.rx_queue_dispatch().is_none());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_vsock_tx_packet_header(&mut memory);
        write_vsock_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );
        write_vsock_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial vsock dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        let err = device
            .dispatch()
            .outcome()
            .dispatch_error()
            .expect("partial TX dispatch error should be preserved");
        let completed = err
            .completed_tx_dispatch()
            .expect("completed TX metadata should be preserved");
        assert_eq!(completed.processed_packets(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default()
                .with_tx_queue_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_packets_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_vsock_tx_packet_header(&mut memory);
        write_vsock_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected vsock signal failure");

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("vsock signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("vsock signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected vsock signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default()
                .with_muxer_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_packets_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_vsock();
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing vsock handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default().with_muxer_event_fails(1)
        );
    }

    #[test]
    fn balloon_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_balloon_notification_dispatches(dispatches)
            .expect("empty balloon dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn memory_hotplug_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches = dispatch_boot_memory_hotplug_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
        );
        let result = collect_memory_hotplug_notification_dispatches(dispatches)
            .expect("empty memory-hotplug dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn balloon_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop balloon dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(
            device.dispatch().device().registration.region_id(),
            MmioRegionId::new(110)
        );
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn balloon_statistics_trigger_treats_inactive_device_as_noop() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_stats();
        let dispatches = runtime
            .trigger_balloon_statistics_update(&mut memory, &mut mmio_dispatcher)
            .expect("statistics trigger dispatch should allocate");
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("inactive statistics trigger should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(matches!(
            device.dispatch().outcome().dispatch_error(),
            Some(VirtioBalloonDeviceNotificationError::Inactive { .. })
        ));
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            super::balloon_update_result_from_hvf_dispatches(&result),
            Ok(())
        );
        let metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_dispatch_metrics(&metrics, &result, false);
        assert_eq!(metrics.snapshot(), BalloonDeviceMetrics::default());
    }

    #[test]
    fn balloon_notification_signal_dispatch_signals_queued_inflate_descriptor() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued balloon dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("balloon notification should dispatch");
        let inflate = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue dispatch should be present");
        assert_eq!(inflate.completed_descriptors(), 1);
        assert!(inflate.needs_queue_interrupt());
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_balloon_inflate_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_signals_reporting_descriptor_and_records_metrics() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_reporting();
        configure_boot_balloon_reporting_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_reporting_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued reporting dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("reporting notification should dispatch");
        assert_eq!(dispatch.reporting_notifications(), 1);
        let reporting = dispatch
            .reporting_queue_dispatch()
            .expect("reporting queue dispatch should be present");
        assert_eq!(reporting.completed_descriptors(), 1);
        assert_eq!(reporting.reporting_discard().attempts(), 1);
        assert_eq!(reporting.reporting_discard().requested_bytes(), 4096);
        assert!(reporting.needs_queue_interrupt());
        assert_eq!(read_balloon_reporting_used_index(&memory), 1);

        let metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_dispatch_metrics(&metrics, &result, false);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.free_page_report().count(), 1);
        assert_eq!(snapshot.free_page_report().requested_bytes(), 4096);
        assert_eq!(
            snapshot.free_page_report().advised_bytes(),
            reporting.reporting_discard().advised_bytes()
        );
        assert_eq!(
            snapshot.free_page_report().skipped_bytes(),
            reporting.reporting_discard().skipped_bytes()
        );
        assert_eq!(
            snapshot.free_page_report().failures(),
            reporting.reporting_discard().failures()
        );
    }

    #[test]
    fn memory_hotplug_notification_signal_dispatch_signals_queued_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_memory_hotplug();
        configure_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        write_queued_memory_hotplug_state_request(&mut memory);
        notify_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        let dispatches = dispatch_boot_memory_hotplug_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_memory_hotplug_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued memory-hotplug dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("memory-hotplug notification should dispatch");
        let queue = dispatch
            .queue_dispatch()
            .expect("memory-hotplug queue dispatch should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.policy_errors(), 1);
        assert!(queue.needs_queue_interrupt());
        assert_eq!(
            read_boot_memory_hotplug_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_memory_hotplug_used_index(&memory), 1);
    }

    #[test]
    fn memory_hotplug_runtime_dispatch_uses_injected_mutation_executor() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_memory_hotplug();
        configure_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        update_boot_memory_hotplug_requested_size(&mut runtime, &mut mmio_dispatcher, 2);
        write_queued_memory_hotplug_plug_request(&mut memory);
        notify_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        let mut mutation_executor = RecordingVirtioMemMutationExecutor::default();

        let dispatches = dispatch_boot_memory_hotplug_notifications_with_executor(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut mutation_executor,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_memory_hotplug_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued memory-hotplug dispatch should collect");

        let expected_range =
            GuestMemoryRange::new(VIRTIO_MEM_DEFAULT_REGION_ADDRESS, 2 * 1024 * 1024)
                .expect("expected virtio-mem range should be valid");
        assert_eq!(
            mutation_executor.applied,
            vec![VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![
                expected_range
            ]))]
        );
        assert!(mutation_executor.rolled_back.is_empty());
        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("memory-hotplug notification should dispatch");
        let queue = dispatch
            .queue_dispatch()
            .expect("memory-hotplug queue dispatch should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.policy_errors(), 0);
        assert_eq!(queue.mutation_failures(), 0);
        assert!(queue.needs_queue_interrupt());
        assert_eq!(read_memory_hotplug_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_signals_queued_deflate_descriptor() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_deflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued balloon deflate dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("balloon deflate notification should dispatch");
        let deflate = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue dispatch should be present");
        assert_eq!(deflate.completed_descriptors(), 1);
        assert!(deflate.needs_queue_interrupt());
        assert_eq!(read_balloon_deflate_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_partially_invalid_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial balloon dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        let err = device
            .dispatch()
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
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(read_balloon_inflate_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let inflate_discard = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("balloon inflate notification should dispatch")
            .inflate_queue_dispatch()
            .expect("inflate queue dispatch should be present")
            .inflate_discard();
        assert_eq!(inflate_discard.attempts(), 1);
        let runtime_metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_runtime_dispatch_metrics(
            &runtime_metrics,
            dispatches.as_slice(),
            true,
        );
        assert_eq!(
            runtime_metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 1, 0, 0, 0, 0)
                .with_discard_metrics(inflate_discard.into(), Default::default())
        );
        let (lines, sink) = RecordingSink::failing("injected balloon signal failure");

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("balloon signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("balloon signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected balloon signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        let metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_dispatch_metrics(&metrics, &result, true);
        assert_eq!(
            metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 1, 0, 0, 0, 1)
                .with_discard_metrics(inflate_discard.into(), Default::default())
        );
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_balloon();
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing balloon handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_wrong_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_balloon();
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
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("wrong balloon handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn boot_session_run_step_delegates_with_session_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let (runner, recorded_dispatchers) =
            RecordingBootSessionRunStepRunner::new(Ok(HvfVcpuRunStepOutcome::Canceled));

        let result = run_boot_session_vcpu_step(&runner, &dispatcher);

        assert_eq!(result, Ok(HvfVcpuRunStepOutcome::Canceled));
        let recorded = recorded_run_step_dispatchers(&recorded_dispatchers);
        assert_eq!(recorded.len(), 1);
        let recorded_dispatcher = recorded.first().expect("one dispatcher should be recorded");
        assert!(Arc::ptr_eq(recorded_dispatcher, &dispatcher));
        assert!(
            dispatcher
                .try_lock()
                .expect("delegated run step should not keep dispatcher locked")
                .regions()
                .is_empty()
        );
    }

    #[test]
    fn boot_session_run_step_preserves_runner_error() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let source = HvfVcpuRunnerError::InvalidState("fake boot-session run step failed");
        let (runner, recorded_dispatchers) =
            RecordingBootSessionRunStepRunner::new(Err(source.clone()));

        let err = run_boot_session_vcpu_step(&runner, &dispatcher)
            .expect_err("runner error should be returned");

        assert_eq!(err, source);
        let recorded = recorded_run_step_dispatchers(&recorded_dispatchers);
        assert_eq!(recorded.len(), 1);
        let recorded_dispatcher = recorded.first().expect("one dispatcher should be recorded");
        assert!(Arc::ptr_eq(recorded_dispatcher, &dispatcher));
    }

    #[test]
    fn boot_session_run_loop_stops_before_first_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        stop_token.request_stop();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stopped loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 0 });
        assert!(session.events.is_empty());
    }

    #[test]
    fn boot_session_run_loop_control_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<super::HvfArm64BootRunLoopControl>();
        assert_send_sync::<HvfArm64BootRunLoopStopToken>();
    }

    #[test]
    fn boot_session_run_loop_control_preserves_requests_when_vcpu_control_fails() {
        let (runner, _configured) = start_secondary_configure_recording_runner(false);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let mut coordinator =
            HvfVcpuRunCoordinator::from_test_runners(vec![runner], vec![0], dispatcher, &[0])
                .expect("test coordinator should build");
        let wakeup = HvfArm64BootRunLoopControlWakeupToken::default();
        let control = HvfArm64BootRunLoopControl::new(coordinator.control(), wakeup.clone());
        coordinator
            .shutdown()
            .expect("test coordinator should shut down");

        control
            .request_stop()
            .expect_err("shut-down coordinator should reject stop cancellation");
        assert!(control.stop_token().is_stop_requested());
        control
            .request_wakeup()
            .expect_err("shut-down coordinator should reject wakeup cancellation");
        assert!(wakeup.take_wakeup_request());
    }

    #[test]
    fn boot_session_run_loop_dispatches_after_mmio_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            mmio_run_step_outcome(),
            mmio_run_step_outcome(),
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("step-limited loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_continues_after_hvc_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            hvc_run_step_outcome(),
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("step-limited HVC loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            ["run", "vsock-dispatch", "run", "vsock-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_returns_guest_shutdown_as_terminal_outcome() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([guest_shutdown_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("guest shutdown loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::GuestShutdown { steps: 1 }
        );
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_returns_guest_reset_as_terminal_outcome() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([guest_reset_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("guest reset loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::GuestReset { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_cancels_network_retry_for_every_terminal_outcome() {
        for step in [
            HvfVcpuRunStepOutcome::Unknown { reason: 99 },
            guest_shutdown_run_step_outcome(),
            guest_reset_run_step_outcome(),
        ] {
            let stop_token = HvfArm64BootRunLoopStopToken::new();
            let mut session = RecordingBootSessionRunLoopSession::new([step]);
            session.request_network_retry_wakeup_on_run();

            run_boot_session_loop(&mut session, &stop_token, max_steps(2))
                .expect("terminal run-loop outcome should be returned");

            assert_eq!(session.network_retry_cancel_count, 1);
            assert!(!session.network_retry_wakeup_requested);
        }

        let stop_token = HvfArm64BootRunLoopStopToken::new();
        stop_token.request_stop();
        let mut session = RecordingBootSessionRunLoopSession::new([]);
        session.request_run_loop_network_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("pre-step stop should be returned");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 0 });
        assert_eq!(session.network_retry_cancel_count, 1);
        assert!(!session.network_retry_wakeup_requested);
    }

    #[test]
    fn boot_session_run_loop_retains_network_retry_for_continuation_outcomes() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut step_limited = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        step_limited.push_network_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut step_limited, &stop_token, max_steps(1))
            .expect("step-limited run loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(step_limited.network_retry_cancel_count, 0);
        assert_eq!(
            step_limited.scheduled_network_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );

        let mut control_wakeup =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        control_wakeup.request_network_retry_wakeup_on_run();
        control_wakeup.request_run_loop_control_wakeup();
        control_wakeup.push_network_retry_dispatch(Duration::from_millis(40));

        let outcome = run_boot_session_loop(&mut control_wakeup, &stop_token, max_steps(1))
            .expect("control wakeup run loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Wakeup { steps: 1 });
        assert_eq!(control_wakeup.network_retry_cancel_count, 0);
        assert_eq!(
            control_wakeup.scheduled_network_retry_wakeups,
            [Some(Duration::from_millis(40))]
        );
    }

    #[test]
    fn boot_session_run_loop_continues_after_sys64_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            sys64_run_step_outcome(),
            sys64_run_step_outcome(),
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("step-limited SYS64 loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            ["run", "vsock-dispatch", "run", "vsock-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_observer_records_step_outcomes() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            sys64_run_step_outcome(),
            HvfVcpuRunStepOutcome::VtimerActivated,
            mmio_run_step_outcome(),
        ]);
        let mut observed = Vec::new();

        let outcome = super::run_boot_session_loop_with_observer(
            &mut session,
            &stop_token,
            max_steps(4),
            |step| observed.push(*step),
        )
        .expect("observed run loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 4 }
        );
        assert_eq!(
            observed,
            [
                hvc_run_step_outcome(),
                sys64_run_step_outcome(),
                HvfVcpuRunStepOutcome::VtimerActivated,
                mmio_run_step_outcome(),
            ]
        );
        assert_eq!(
            session.events,
            [
                "run",
                "vsock-dispatch",
                "run",
                "vsock-dispatch",
                "run",
                "timer",
                "vsock-dispatch",
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_pmem_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_pmem_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after pmem dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "dispatch", "pmem-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_network_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_network_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after network dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            ["run", "dispatch", "pmem-dispatch", "network-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_vsock_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_vsock_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after vsock dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_balloon_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_balloon_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after balloon dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_non_mmio_vsock_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_stop_on_vsock_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after non-MMIO vsock dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_entropy_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_entropy_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after entropy dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_memory_hotplug_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_memory_hotplug_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after memory-hotplug dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_hvc_when_requested() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_stop_on_run(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after HVC should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_canceled_step_when_requested() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_stop_on_run(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after canceled step should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_continues_after_unattributed_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("unattributed canceled step should remain non-terminal");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_reports_control_wakeup_after_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_control_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("control wakeup loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Wakeup { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_stop_takes_priority_over_control_wakeup() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_control_wakeup();
        session.request_stop_on_run(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop should take priority over control wakeup");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_vsock_before_control_wakeup() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_wakeup();
        session.request_run_loop_control_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("combined wakeup loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Wakeup { steps: 1 });
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_vsock_after_wakeup_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("wakeup cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_vsock_after_monitor_deadline_wakeup() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.push_monitor_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("deadline monitor wakeup loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_block_only_for_a_block_capable_monitor_wakeup() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.push_block_monitor_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("block monitor wakeup loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch", "vsock-dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_block_after_block_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_block_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("block retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_reschedules_repeated_block_retry_after_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_block_retry_wakeup();
        session.push_block_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("repeated block retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch"]);
        assert_eq!(
            session.scheduled_block_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );
    }

    #[test]
    fn boot_session_run_loop_consumes_delayed_block_retry_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_block_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("delayed block retry after HVC should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch", "vsock-dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_keeps_wakeup_for_delayed_cancel_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            HvfVcpuRunStepOutcome::Canceled,
        ]);
        session.push_monitor_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("delayed wakeup cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            ["run", "vsock-dispatch", "run", "vsock-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_keeps_wakeup_across_intervening_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            hvc_run_step_outcome(),
            HvfVcpuRunStepOutcome::Canceled,
        ]);
        session.push_monitor_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(3))
            .expect("an intervening member outcome should not consume a pending wakeup");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 3 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "vsock-dispatch",
                "run",
                "vsock-dispatch",
                "run",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_retains_each_wakeup_across_multiple_delayed_cancels() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            HvfVcpuRunStepOutcome::Canceled,
            HvfVcpuRunStepOutcome::Canceled,
        ]);
        session.push_monitor_wakeup();
        session.push_monitor_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(3))
            .expect("each delayed wakeup cancel should remain attributable");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 3 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "vsock-dispatch",
                "run",
                "vsock-dispatch",
                "run",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_schedules_block_retry_after_mmio_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_block_retry_dispatch(Duration::from_millis(10));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch with block retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch"
            ]
        );
        assert_eq!(
            session.scheduled_block_retry_wakeups,
            [Some(Duration::from_millis(10))]
        );
    }

    #[test]
    fn boot_session_run_loop_clears_block_retry_after_mmio_dispatch_without_retry() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch without block retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_published_pmem_retry_before_first_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_pmem_retry_wakeup();
        session.push_pmem_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("pre-published pmem retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["pmem-dispatch", "run", "vsock-dispatch"]);
        assert_eq!(
            session.scheduled_pmem_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );
        assert_eq!(session.pmem_retry_cancel_count, 0);
    }

    #[test]
    fn boot_session_run_loop_dispatches_network_after_network_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_network_retry_wakeup_on_run();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("network retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "network-dispatch"]);
        assert_eq!(session.scheduled_network_retry_wakeups, [None]);
        assert_eq!(session.network_retry_cancel_count, 0);
    }

    #[test]
    fn boot_session_run_loop_reschedules_repeated_network_retry_after_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_network_retry_wakeup_on_run();
        session.push_network_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("repeated network retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "network-dispatch"]);
        assert_eq!(
            session.scheduled_network_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );
        assert_eq!(session.network_retry_cancel_count, 0);
    }

    #[test]
    fn boot_session_run_loop_consumes_delayed_network_retry_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_network_retry_wakeup_on_run();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("delayed network retry after HVC should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            ["run", "network-dispatch", "vsock-dispatch"]
        );
        assert_eq!(session.scheduled_network_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_published_network_retry_before_first_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_network_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("pre-published network retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            ["network-dispatch", "run", "vsock-dispatch"]
        );
        assert_eq!(session.scheduled_network_retry_wakeups, [None]);
        assert_eq!(session.network_retry_cancel_count, 0);
    }

    #[test]
    fn boot_session_run_loop_stop_after_published_network_retry_prevents_first_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_network_retry_wakeup();
        session.request_stop_on_network_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after pre-published network retry should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 0 });
        assert_eq!(session.events, ["network-dispatch"]);
        assert_eq!(session.scheduled_network_retry_wakeups, [None]);
        assert_eq!(session.network_retry_cancel_count, 1);
    }

    #[test]
    fn boot_session_run_loop_schedules_network_retry_after_mmio_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_network_retry_dispatch(Duration::from_millis(10));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch with network retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.scheduled_network_retry_wakeups,
            [Some(Duration::from_millis(10))]
        );
    }

    #[test]
    fn boot_session_run_loop_dispatches_entropy_after_entropy_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_entropy_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("entropy retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "entropy-dispatch"]);
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_reschedules_repeated_entropy_retry_after_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_entropy_retry_wakeup();
        session.push_entropy_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("repeated entropy retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "entropy-dispatch"]);
        assert_eq!(
            session.scheduled_entropy_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );
    }

    #[test]
    fn boot_session_run_loop_consumes_delayed_entropy_retry_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_entropy_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("delayed entropy retry after HVC should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            ["run", "entropy-dispatch", "vsock-dispatch"]
        );
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_schedules_entropy_retry_after_mmio_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_entropy_retry_dispatch(Duration::from_millis(10));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch with entropy retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch"
            ]
        );
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
        assert_eq!(
            session.scheduled_entropy_retry_wakeups,
            [Some(Duration::from_millis(10))]
        );
    }

    #[test]
    fn boot_session_run_loop_clears_entropy_retry_after_mmio_dispatch_without_retry() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch without entropy retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_block_and_entropy_after_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_block_retry_wakeup();
        session.request_run_loop_entropy_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("combined retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch", "entropy-dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_preserves_runner_error() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let source = HvfVcpuRunnerError::InvalidState("fake run loop step failed");
        let mut session = RecordingBootSessionRunLoopSession::with_run_error(source.clone());

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("runner error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::RunStep {
                steps_completed,
                source: actual,
            } => {
                assert_eq!(steps_completed, 0);
                match *actual {
                    super::HvfArm64BootVcpuError::Member {
                        index,
                        mpidr,
                        generation,
                        source: actual,
                    } => {
                        assert_eq!((index, mpidr, generation), (0, 0, 0));
                        assert_eq!(*actual, source);
                    }
                    other => panic!("expected member error, got {other:?}"),
                }
            }
            other => panic!("expected run-step error, got {other:?}"),
        }
        assert_eq!(session.events, ["run"]);
        assert_eq!(session.network_retry_cancel_count, 1);
    }

    #[test]
    fn boot_session_run_loop_preserves_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_dispatch_error(HvfArm64BootBlockNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        });

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchBlockNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected notification error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_pmem_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_pmem_dispatch_error(
            HvfArm64BootPmemNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("pmem notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchPmemNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected pmem notification error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "dispatch", "pmem-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_network_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_network_dispatch_error(
            HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("network notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchNetworkNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected network notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            ["run", "dispatch", "pmem-dispatch", "network-dispatch"]
        );
        assert_eq!(session.network_retry_cancel_count, 1);
    }

    #[test]
    fn boot_session_run_loop_preserves_vsock_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_vsock_dispatch_error(
            HvfArm64BootVsockNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("vsock notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchVsockNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected vsock notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_balloon_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_balloon_dispatch_error(
            HvfArm64BootBalloonNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("balloon notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchBalloonNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected balloon notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_memory_hotplug_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_memory_hotplug_dispatch_error(
            HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("memory-hotplug notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchMemoryHotplugNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected memory-hotplug notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_vsock_notification_error_after_non_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.push_vsock_dispatch_error(
            HvfArm64BootVsockNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("non-MMIO vsock notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchVsockNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected vsock notification error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_entropy_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_entropy_dispatch_error(
            HvfArm64BootEntropyNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("entropy notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchEntropyNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected entropy notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_handles_virtual_timer_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            HvfVcpuRunStepOutcome::VtimerActivated,
            HvfVcpuRunStepOutcome::VtimerActivated,
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("virtual timer loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "timer",
                "vsock-dispatch",
                "run",
                "timer",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_virtual_timer_handler_error() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::VtimerActivated]);
        let source = HvfVcpuRunnerError::InvalidState("fake timer handler failed");
        session.push_timer_error(source.clone());

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("virtual timer handler error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::HandleVirtualTimer {
                steps_completed,
                source: actual,
            } => {
                assert_eq!(steps_completed, 1);
                match *actual {
                    super::HvfArm64BootVcpuError::Member {
                        index,
                        mpidr,
                        generation,
                        source: actual,
                    } => {
                        assert_eq!((index, mpidr, generation), (0, 0, 0));
                        assert_eq!(*actual, source);
                    }
                    other => panic!("expected member error, got {other:?}"),
                }
            }
            other => panic!("expected virtual timer handler error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "timer"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_virtual_timer_handler() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::VtimerActivated]);
        session.request_stop_on_timer(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after virtual timer handler should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "timer"]);
    }

    #[test]
    fn boot_session_run_loop_returns_unknown_as_terminal_outcome() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Unknown {
                reason: 99,
            }]);

        let unknown_outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("unknown-exit loop should succeed");

        assert_eq!(
            unknown_outcome,
            HvfArm64BootRunLoopOutcome::Unknown {
                steps: 1,
                reason: 99
            }
        );
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_mmio_dispatcher_lock_accepts_available_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));

        let guard =
            lock_boot_mmio_dispatcher(&dispatcher).expect("available boot dispatcher should lock");

        assert!(guard.regions().is_empty());
    }

    #[test]
    fn boot_mmio_dispatcher_lock_reports_busy_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let _held = dispatcher
            .lock()
            .expect("test dispatcher lock should be acquired");

        let err = lock_boot_mmio_dispatcher(&dispatcher)
            .expect_err("already-held dispatcher lock should be busy");

        assert_eq!(err, HvfArm64BootMmioDispatcherError::Busy);
    }

    #[test]
    fn boot_mmio_dispatcher_runtime_lock_waits_for_peer_owner() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let held = dispatcher
            .lock()
            .expect("test dispatcher lock should be acquired");
        let waiter_dispatcher = Arc::clone(&dispatcher);
        let (entered_sender, entered_receiver) = std::sync::mpsc::channel();
        let (completed_sender, completed_receiver) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            entered_sender
                .send(())
                .expect("runtime lock waiter should announce entry");
            let result = lock_boot_mmio_dispatcher_runtime(&waiter_dispatcher)
                .map(|guard| guard.regions().len());
            completed_sender
                .send(result)
                .expect("runtime lock waiter should publish result");
        });

        entered_receiver
            .recv()
            .expect("runtime lock waiter should start");
        assert!(matches!(
            completed_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        drop(held);
        assert_eq!(
            completed_receiver
                .recv()
                .expect("runtime lock waiter should complete"),
            Ok(0)
        );
        waiter.join().expect("runtime lock waiter should join");
    }

    #[test]
    fn boot_mmio_dispatcher_lock_reports_poisoned_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let poisoned_dispatcher = Arc::clone(&dispatcher);
        let thread = thread::spawn(move || {
            let _held = poisoned_dispatcher
                .lock()
                .expect("test dispatcher lock should be acquired");
            panic!("poison test dispatcher lock");
        });

        assert!(thread.join().is_err());
        let err = lock_boot_mmio_dispatcher(&dispatcher)
            .expect_err("poisoned dispatcher lock should fail");

        assert_eq!(err, HvfArm64BootMmioDispatcherError::Poisoned);
    }

    #[test]
    fn displays_block_notification_dispatch_errors() {
        let err = HvfArm64BootBlockNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for block notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootBlockNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        };
        assert_eq!(
            err.to_string(),
            "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("HVF boot-session MMIO dispatcher lock is busy".to_string())
        );
    }

    #[test]
    fn displays_network_notification_dispatch_errors() {
        let err = HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for network notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        };
        assert_eq!(
            err.to_string(),
            "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("HVF boot-session MMIO dispatcher lock is busy".to_string())
        );
    }

    #[test]
    fn displays_vsock_notification_dispatch_errors() {
        let err = HvfArm64BootVsockNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for vsock notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootVsockNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        };
        assert_eq!(
            err.to_string(),
            "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("HVF boot-session MMIO dispatcher lock is busy".to_string())
        );
    }

    #[test]
    fn displays_memory_hotplug_notification_dispatch_errors() {
        let err = HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for memory-hotplug notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        };
        assert_eq!(
            err.to_string(),
            "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("HVF boot-session MMIO dispatcher lock is busy".to_string())
        );
    }

    #[test]
    fn displays_entropy_notification_dispatch_errors() {
        let err = HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for entropy notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootEntropyNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        };
        assert_eq!(
            err.to_string(),
            "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("HVF boot-session MMIO dispatcher lock is busy".to_string())
        );
    }

    #[test]
    fn session_config_stores_serial_device() {
        let serial = HvfArm64BootSerialDeviceConfig::new(
            MmioRegionId::new(7),
            GuestAddress::new(0x4000_0000),
            SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
        );

        let network_layout =
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            network_layout,
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_serial_device(serial);

        assert!(config.serial_device.is_some());
        assert_eq!(config.network_mmio_layout, network_layout);
        assert_eq!(config.rtc_mmio_layout.base(), TEST_RTC_MMIO_BASE);
    }

    #[test]
    fn session_config_opts_into_internal_platform_validation_without_changing_the_default() {
        let default = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        );
        let configuration = crate::gic::HvfGicMsiConfiguration::new(
            NonZeroU32::new(8).expect("test MSI count should be nonzero"),
        );

        assert_eq!(default.gic_msi, None);
        assert_eq!(default.pci_validation, None);
        assert!(!default.pci_enabled);
        assert_eq!(
            default.clone().with_gic_msi(configuration).gic_msi,
            Some(configuration)
        );
        assert!(default.clone().with_pci_enabled().pci_enabled);
        let validation = Arm64BootPciValidationConfig::firecracker_test_endpoint();
        assert_eq!(
            default.with_pci_validation(validation).pci_validation,
            Some(validation)
        );
    }

    #[test]
    fn pci_data_resource_demand_uses_exact_endpoint_and_msix_shapes() {
        let demand = pci_data_resource_demand(2, 3, 5)
            .expect("bounded PCI data-device counts should fit resource demand");
        assert_eq!(demand.endpoints, 10);
        assert_eq!(demand.routes, 2 * 2 + 3 * 3 + 5 * 2);

        let endpoint_overflow = pci_data_resource_demand(usize::MAX, 1, 0)
            .expect_err("endpoint count overflow should fail preflight");
        assert_eq!(
            endpoint_overflow.to_string(),
            "PCI data endpoint count overflowed"
        );

        let route_overflow = pci_data_resource_demand(0, usize::MAX, 0)
            .expect_err("MSI-X route count overflow should fail preflight");
        assert_eq!(
            route_overflow.to_string(),
            "PCI data MSI-X route count overflowed"
        );
    }

    #[test]
    fn pci_all_virtio_resource_demand_includes_every_endpoint_shape() {
        let demand = pci_all_virtio_resource_demand(2, 3, 5, Some(4), true, true, true)
            .expect("bounded all-virtio device counts should fit resource demand");
        assert_eq!(demand.endpoints, 14);
        assert_eq!(demand.routes, 36);

        let no_devices = pci_all_virtio_resource_demand(0, 0, 0, None, false, false, false)
            .expect("an empty all-virtio machine should have zero endpoint demand");
        assert_eq!(no_devices.endpoints, 0);
        assert_eq!(no_devices.routes, 0);

        let balloon_overflow =
            pci_all_virtio_resource_demand(0, 0, 0, Some(usize::MAX), false, false, false)
                .expect_err("overflowing balloon route demand should fail preflight");
        assert_eq!(
            balloon_overflow.to_string(),
            "PCI all-virtio balloon MSI-X route count overflowed"
        );
    }

    #[test]
    fn runtime_pci_endpoint_capacity_is_shared_across_mixed_device_types() {
        let fixed = (true, true, true, true);
        let full_count = pci_data_endpoint_count(8, 8, 11, fixed.0, fixed.1, fixed.2, fixed.3);
        assert_eq!(full_count, PCI_ENDPOINT_SLOT_COUNT);

        for (block_count, network_count, pmem_count) in [(7, 8, 11), (8, 7, 11), (8, 8, 10)] {
            let released_count = pci_data_endpoint_count(
                block_count,
                network_count,
                pmem_count,
                fixed.0,
                fixed.1,
                fixed.2,
                fixed.3,
            );
            assert_eq!(released_count, PCI_ENDPOINT_SLOT_COUNT - 1);
            assert!(released_count < PCI_ENDPOINT_SLOT_COUNT);
        }

        assert_eq!(
            pci_data_endpoint_count(usize::MAX, 1, 1, true, true, true, true),
            usize::MAX,
            "overflow must fail closed as exhausted capacity"
        );
    }

    #[test]
    fn mixed_full_pci_inventory_fits_reserved_runtime_vector_headroom() {
        let balloon_queue_count = Some(1);
        let full = pci_all_virtio_resource_demand(8, 8, 11, balloon_queue_count, true, true, true)
            .expect("mixed full PCI inventory should have bounded resource demand");
        assert_eq!(full.endpoints, PCI_ENDPOINT_SLOT_COUNT);

        let fixed = pci_all_virtio_resource_demand(0, 0, 0, balloon_queue_count, true, true, true)
            .expect("fixed PCI inventory should have bounded resource demand");
        let dynamic_routes =
            (PCI_ENDPOINT_SLOT_COUNT - fixed.endpoints) * (VIRTIO_NET_QUEUE_SIZES.len() + 1);
        assert!(full.routes <= fixed.routes + dynamic_routes);
    }

    #[test]
    fn product_pci_msi_configuration_reserves_bounded_dynamic_slot_headroom() {
        let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_pci_enabled();

        let configuration = pci_all_virtio_gic_msi_configuration(&controller, &config)
            .expect("bounded product PCI capacity should produce an MSI configuration");
        assert_eq!(
            configuration.interrupt_count().get(),
            u32::try_from(PCI_ENDPOINT_SLOT_COUNT * (VIRTIO_NET_QUEUE_SIZES.len() + 1))
                .expect("bounded route count should fit u32")
        );
    }

    #[test]
    fn product_pci_preflights_every_endpoint_bar_and_dispatcher_identity() {
        let endpoint_count =
            u64::try_from(PCI_ENDPOINT_SLOT_COUNT).expect("bounded endpoint count should fit u64");
        let bar_capacity = GuestMemoryRange::new(
            GuestAddress::new(0x40_0000_0000),
            endpoint_count * super::VIRTIO_PCI_CAPABILITY_BAR_SIZE,
        )
        .expect("full product PCI BAR capacity should be valid");
        let allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, bar_capacity);
        let plan = pci_data_bar_plan(&allocator, PCI_ENDPOINT_SLOT_COUNT)
            .expect("all product PCI endpoint BARs should preflight");

        assert_eq!(plan.len(), PCI_ENDPOINT_SLOT_COUNT);
        assert_eq!(
            pci_data_available_bar_count(&allocator)
                .expect("full product PCI BAR capacity should be countable"),
            PCI_ENDPOINT_SLOT_COUNT
        );
        preflight_pci_data_dispatcher(&MmioDispatcher::new(), &plan)
            .expect("all product PCI dispatcher identities should preflight");
        assert!(pci_data_bar_plan(&allocator, PCI_ENDPOINT_SLOT_COUNT + 1).is_err());
    }

    #[test]
    fn pci_data_local_preflight_is_complete_and_mutation_free() {
        let bar_capacity = GuestMemoryRange::new(
            GuestAddress::new(0x40_0000_0000),
            2 * super::VIRTIO_PCI_CAPABILITY_BAR_SIZE,
        )
        .expect("test PCI BAR capacity should be valid");
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, bar_capacity);
        assert!(
            pci_data_bar_plan(&allocator, 0)
                .expect("zero PCI data endpoints should produce an empty BAR plan")
                .is_empty()
        );
        assert_eq!(
            pci_data_available_bar_count(&allocator)
                .expect("aligned test BAR capacity should be countable"),
            2
        );
        let retained = allocator
            .allocate(super::VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("test should retain one BAR lease");
        assert_eq!(
            pci_data_available_bar_count(&allocator)
                .expect("remaining test BAR capacity should be countable"),
            1
        );

        let mut dispatcher = MmioDispatcher::new();
        let bar_plan = pci_data_bar_plan(&allocator, 1)
            .expect("remaining test BAR should produce an exact plan");
        preflight_pci_data_dispatcher(&dispatcher, &bar_plan)
            .expect("fresh PCI data region ids should pass preflight");
        let conflicting_id = pci_data_region_id(0).expect("bounded region id should fit");
        dispatcher
            .insert_region(conflicting_id, GuestAddress::new(0x9000_0000), 0x1000)
            .expect("test conflicting region should register");
        let error = preflight_pci_data_dispatcher(&dispatcher, &bar_plan)
            .expect_err("retained PCI data region id should fail preflight");
        assert_eq!(
            error.to_string(),
            format!("PCI data MMIO region id {conflicting_id} is already in use")
        );
        assert_eq!(dispatcher.regions().len(), 1);

        let mut overlap_dispatcher = MmioDispatcher::new();
        let planned_bar = bar_plan
            .first()
            .copied()
            .expect("one planned BAR should be retained");
        overlap_dispatcher
            .insert_region(
                MmioRegionId::new(9_999),
                planned_bar.start(),
                planned_bar.size(),
            )
            .expect("test overlapping region should register");
        let overlap = preflight_pci_data_dispatcher(&overlap_dispatcher, &bar_plan)
            .expect_err("planned BAR overlap should fail preflight");
        assert_eq!(
            overlap.to_string(),
            "PCI data capability BAR 0 overlaps an existing MMIO region"
        );
        assert_eq!(
            pci_data_available_bar_count(&allocator)
                .expect("failed region preflight must not consume BAR capacity"),
            1
        );

        allocator
            .release(&retained)
            .expect("test BAR lease should release after preflight");
        assert_eq!(
            pci_data_available_bar_count(&allocator)
                .expect("released BAR capacity should be reusable"),
            2
        );
    }

    #[test]
    fn cache_admission_failure_precedes_all_vm_owned_startup_state() {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                1,
                TEST_MEMORY_MIB,
            )))
            .expect("machine config should be stored");
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        );
        let mut backend = crate::HvfBackend::new();

        let error = match super::prepare_arm64_boot_session_parts_with_cache(
            &mut backend,
            &controller,
            config,
            super::VmStartupResources::default(),
            |vcpu_count| {
                assert_eq!(vcpu_count, 1);
                Err(crate::cache::HvfArm64CacheTopologyError::InvalidCcidx)
            },
        ) {
            Ok(_) => panic!("cache admission failure must stop startup"),
            Err(error) => error,
        };

        assert!(matches!(
            &error,
            HvfArm64BootSessionError::CacheTopology {
                source: crate::cache::HvfArm64CacheTopologyError::InvalidCcidx,
            }
        ));
        assert_eq!(
            error.to_string(),
            "failed to admit HVF arm64 cache topology: HVF cache index format is unsupported"
        );
        assert!(!backend.has_created_vm());
        assert_eq!(backend.gic_metadata(), None);
        assert!(backend.mapped_guest_memory().is_err());
    }

    #[test]
    fn snapshot_capture_reuses_retained_manifest_and_rejects_mmfr2_drift() {
        let manifest = crate::vcpu_config::HvfArm64VcpuCacheManifest::new(
            crate::vcpu_config::HvfArm64VcpuCacheConfiguration::new([1, 2, 3]),
            crate::vcpu_config::HvfArm64VcpuCacheGeometry::new([[4; 8], [5; 8]]),
        );
        let source = crate::vcpu_config::HvfArm64VcpuCacheFdtSource::new(6, manifest);

        assert_eq!(
            super::retained_snapshot_cache_manifest(source, 6)
                .expect("unchanged runner MMFR2 should reuse startup manifest"),
            manifest
        );
        let error = super::retained_snapshot_cache_manifest(source, 7)
            .expect_err("changed runner MMFR2 should reject capture");
        assert!(matches!(
            &error,
            super::HvfArm64BootSnapshotV1StateCaptureError::CacheIdentityMismatch
        ));
        assert_eq!(
            error.to_string(),
            "native-v1 cache compatibility identity changed after startup"
        );
    }

    #[test]
    fn session_config_stores_entropy_device() {
        let entropy = HvfArm64BootEntropyDeviceConfig::new(EntropyMmioLayout::new(
            GuestAddress::new(0x8000_0000),
            MmioRegionId::new(3000),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_entropy_device(entropy);

        assert_eq!(config.entropy_device, Some(entropy));
        assert!(config.serial_device.is_none());
    }

    #[test]
    fn session_config_stores_memory_hotplug_device() {
        let memory_hotplug = HvfArm64BootMemoryHotplugDeviceConfig::new(VirtioMemMmioLayout::new(
            GuestAddress::new(0x4000_9000),
            MmioRegionId::new(5000),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_memory_hotplug_device(memory_hotplug);

        assert_eq!(config.memory_hotplug_device, Some(memory_hotplug));
        assert_eq!(config.entropy_device, None);
        assert_eq!(config.balloon_device, None);
    }

    #[test]
    fn session_config_stores_boot_timer_device() {
        let boot_timer = HvfArm64BootTimerDeviceConfig::new(BootTimerMmioLayout::new(
            GuestAddress::new(0x4000_0000),
            MmioRegionId::new(0),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_boot_timer_device(boot_timer);

        assert_eq!(config.boot_timer_device, Some(boot_timer));
        assert_eq!(config.entropy_device, None);
        assert_eq!(config.balloon_device, None);
    }

    #[test]
    fn session_config_stores_balloon_device() {
        let balloon = HvfArm64BootBalloonDeviceConfig::new(BalloonMmioLayout::new(
            GuestAddress::new(0x4000_8000),
            MmioRegionId::new(4000),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_balloon_device(balloon);

        assert_eq!(config.balloon_device, Some(balloon));
        assert_eq!(config.entropy_device, None);
    }

    #[test]
    fn interrupt_lines_allocate_vmclock_after_vmgenid() {
        let lines = allocate_interrupt_lines(
            &gic_with_spi_range(32, 13),
            HvfArm64BootInterruptRequest {
                block_device_count: 2,
                pmem_device_count: 2,
                network_device_count: 2,
                vsock_configured: true,
                balloon_configured: true,
                entropy_configured: true,
                memory_hotplug_configured: true,
                serial_configured: true,
            },
        )
        .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![32, 33]);
        assert_eq!(line_values(&lines.pmem), vec![34, 35]);
        assert_eq!(line_values(&lines.network), vec![36, 37]);
        assert_eq!(lines.vsock.map(|line| line.raw_value()), Some(38));
        assert_eq!(lines.balloon.map(|line| line.raw_value()), Some(39));
        assert_eq!(lines.entropy.map(|line| line.raw_value()), Some(40));
        assert_eq!(lines.memory_hotplug.map(|line| line.raw_value()), Some(41));
        assert_eq!(lines.serial.map(|line| line.raw_value()), Some(42));
        assert_eq!(lines.vmgenid.raw_value(), 43);
        assert_eq!(lines.vmclock.raw_value(), 44);
    }

    #[test]
    fn interrupt_lines_allocate_none_for_absent_serial() {
        let lines = allocate_interrupt_lines(
            &gic_with_spi_range(40, 5),
            HvfArm64BootInterruptRequest {
                block_device_count: 2,
                network_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![40, 41]);
        assert!(lines.pmem.is_empty());
        assert_eq!(line_values(&lines.network), vec![42]);
        assert_eq!(lines.vsock, None);
        assert_eq!(lines.balloon, None);
        assert_eq!(lines.entropy, None);
        assert_eq!(lines.memory_hotplug, None);
        assert_eq!(lines.serial, None);
        assert_eq!(lines.vmgenid.raw_value(), 43);
        assert_eq!(lines.vmclock.raw_value(), 44);
    }

    #[test]
    fn interrupt_lines_report_vmgenid_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("VMGenID allocation should exhaust range after block");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmGenIdDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_vmclock_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest::default(),
        )
        .expect_err("VMClock allocation should exhaust range after VMGenID");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmClockDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_pmem_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                pmem_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("pmem allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::PmemDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_vsock_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 2),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("vsock allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VsockDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_balloon_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 3),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                balloon_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("balloon allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BalloonDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_entropy_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 3),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                entropy_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("entropy allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::EntropyDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_serial_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 3),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                serial_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
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
    fn interrupt_lines_report_network_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("network allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::NetworkDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_reject_invalid_gic_range() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(31, 1),
            HvfArm64BootInterruptRequest::default(),
        )
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
