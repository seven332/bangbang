use std::fmt;
use std::io::Read;
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::num::NonZeroUsize;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bangbang_hvf::{
    HvfArm64BootBalloonDeviceConfig, HvfArm64BootEntropyDeviceConfig,
    HvfArm64BootMemoryHotplugDeviceConfig, HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError,
    HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig,
    HvfArm64BootSessionConfig, HvfArm64BootTimerDeviceConfig, HvfVcpuRunnerError,
    OwnedHvfArm64BootSession,
};
use bangbang_runtime::balloon::BalloonMmioLayout;
use bangbang_runtime::balloon::{
    BalloonConfig, BalloonConfigInput, BalloonHintingCommandError, BalloonHintingStartInput,
    BalloonHintingStatus, BalloonHintingStatusError, BalloonStats, BalloonStatsError,
    BalloonStatsUpdateInput, BalloonUpdateError, BalloonUpdateInput,
};
use bangbang_runtime::block::{
    BlockFileBacking, BlockMmioLayout, DriveConfig, DriveConfigInput, DriveRateLimiterConfig,
    DriveUpdateError, DriveUpdateInput,
};
use bangbang_runtime::boot::BootSourceConfigInput;
use bangbang_runtime::boot_timer::BootTimerMmioLayout;
use bangbang_runtime::cpu::CpuConfigInput;
use bangbang_runtime::entropy::EntropyMmioLayout;
use bangbang_runtime::logger::LoggerConfigInput;
use bangbang_runtime::machine::{MachineConfigInput, MachineConfigPatchInput};
use bangbang_runtime::memory::{GuestAddress, GuestMemory};
use bangbang_runtime::memory_hotplug::{
    MemoryHotplugConfig, MemoryHotplugConfigInput, MemoryHotplugSizeUpdate,
    MemoryHotplugSizeUpdateInput, MemoryHotplugStatus, MemoryHotplugStatusError,
    MemoryHotplugUpdateError, VirtioMemMmioLayout,
};
use bangbang_runtime::metrics::{
    BootRunLoopMetricStatus, MetricsConfigInput, MetricsDiagnostics, SharedBalloonDeviceMetrics,
    SharedBlockDeviceMetricsRegistry, SharedEntropyDeviceMetrics,
    SharedNetworkInterfaceMetricsRegistry, SharedPmemDeviceMetricsRegistry, SharedRtcDeviceMetrics,
    SharedSignalMetrics, SharedVsockDeviceMetrics,
};
use bangbang_runtime::mmds::{
    MmdsConfig, MmdsConfigInput, MmdsContentInput, MmdsStateHandle, MmdsStateLockError,
};
use bangbang_runtime::mmio::{MmioDispatcher, MmioRegionId};
use bangbang_runtime::network::{
    NetworkInterfaceConfig, NetworkInterfaceConfigError, NetworkInterfaceConfigInput,
    NetworkInterfaceUpdateInput, NetworkMmioLayout, VirtioNetworkRxPacket,
    VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame,
    VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError, validate_network_interface_count,
};
use bangbang_runtime::pmem::{PmemMmioLayout, PmemUpdateInput};
use bangbang_runtime::rtc::RtcMmioLayout;
use bangbang_runtime::serial::{
    SerialConfigError, SerialConfigInput, SerialOutputFile, SharedSerialOutput,
    SharedSerialOutputBuffer,
};
use bangbang_runtime::startup::{
    Arm64BootBalloonDevice, Arm64BootBlockDevice, Arm64BootNetworkDevice, Arm64BootNetworkPacketIo,
    Arm64BootNetworkPacketIoError, Arm64BootNetworkPacketIoProvider,
    balloon_hinting_status_for_device, balloon_stats_for_device, start_balloon_hinting_for_device,
    stop_balloon_hinting_for_device, update_balloon_config_for_device,
    update_balloon_statistics_for_device, update_block_device_for_devices_with_opened,
};
use bangbang_runtime::vsock::{VsockConfigInput, VsockMmioLayout};
use bangbang_runtime::{BackendError, VmmAction, VmmActionError, VmmController, VmmData};

use crate::host_network::virtio_vmnet::{
    MmdsOnlyVirtioNetworkPacketIo, MmdsOnlyVirtioNetworkPacketIoBuildError,
    MmdsOnlyVirtioNetworkPacketIoProvider, MmdsOnlyVirtioNetworkPacketIoProviderBuildError,
    MmdsOnlyVirtioNetworkPacketIoProviderEntry, MmdsPacketDetour, MmdsResponseQueue,
    VmnetVirtioNetworkPacketIo, VmnetVirtioNetworkPacketIoBuildError,
    VmnetVirtioNetworkPacketIoProvider, VmnetVirtioNetworkPacketIoProviderBuildError,
    VmnetVirtioNetworkPacketIoProviderEntry,
};
use crate::host_network::vmnet::{
    StartedVmnetPacketIoBackend, SystemVmnetInterfaceBackend, VmnetHostDeviceNameConfigError,
    VmnetInterfaceBackend, VmnetInterfaceConfig, VmnetInterfaceStartError, VmnetPacketIoBackend,
};

#[cfg(test)]
use bangbang_runtime::InstanceInfo;
#[cfg(test)]
use bangbang_runtime::boot::BootSourceConfig;
#[cfg(test)]
use bangbang_runtime::machine::MachineConfig;
#[cfg(test)]
use bangbang_runtime::serial::SerialConfig;

const DEFAULT_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x5000_0000);
const DEFAULT_BLOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1);
const DEFAULT_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x5800_0000);
const DEFAULT_PMEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(500);
const DEFAULT_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x6000_0000);
const DEFAULT_NETWORK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1000);
const DEFAULT_VSOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x7000_0000);
const DEFAULT_VSOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(2000);
const DEFAULT_BOOT_TIMER_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
const DEFAULT_BOOT_TIMER_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(0);
const DEFAULT_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_2000);
const DEFAULT_SERIAL_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(20);
const DEFAULT_RTC_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_1000);
const DEFAULT_RTC_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(10);
const DEFAULT_ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_7000);
const DEFAULT_ENTROPY_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(3000);
const DEFAULT_BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
const DEFAULT_BALLOON_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(4000);
const DEFAULT_MEMORY_HOTPLUG_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_9000);
const DEFAULT_MEMORY_HOTPLUG_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(5000);
const DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT: usize = 1024;
const BOOT_RUN_LOOP_COMMAND_QUEUE_CAPACITY: usize = 32;
const HVF_BOOT_RUN_LOOP_THREAD_NAME: &str = "bangbang-hvf-boot-loop";

pub(crate) trait InstanceStartExecutor {
    type Session: ProcessSessionDiagnostics;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError>;

    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        MetricsDiagnostics::default()
    }
}

pub(crate) trait ProcessSessionDiagnostics {
    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        MetricsDiagnostics::default()
    }

    fn pause(&mut self) -> Result<(), BackendError> {
        Err(BackendError::InvalidState("active session unavailable"))
    }

    fn resume(&mut self) -> Result<(), BackendError> {
        Err(BackendError::InvalidState("active session unavailable"))
    }

    fn update_block_device(
        &mut self,
        _config: &DriveConfig,
        _refresh_backing: bool,
        _rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        Err(DriveUpdateError::ActiveSessionUnavailable)
    }

    fn update_balloon(&mut self, _config: BalloonConfig) -> Result<(), BalloonUpdateError> {
        Err(BalloonUpdateError::ActiveSessionUnavailable)
    }

    fn update_balloon_statistics(
        &mut self,
        _input: BalloonStatsUpdateInput,
    ) -> Result<(), BalloonUpdateError> {
        Err(BalloonUpdateError::ActiveSessionUnavailable)
    }

    fn update_memory_hotplug(
        &mut self,
        _update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        Err(MemoryHotplugUpdateError::ActiveSessionUnavailable)
    }

    fn memory_hotplug_status(
        &mut self,
        _config: MemoryHotplugConfig,
        _requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        Err(MemoryHotplugStatusError::ActiveSessionUnavailable)
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        Err(BalloonUpdateError::ActiveSessionUnavailable)
    }

    fn balloon_stats(&mut self, _config: BalloonConfig) -> Result<BalloonStats, BalloonStatsError> {
        Err(BalloonStatsError::ActiveSessionUnavailable)
    }

    fn balloon_hinting_status(
        &mut self,
    ) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
        Err(BalloonHintingStatusError::ActiveSessionUnavailable)
    }

    fn start_balloon_hinting(
        &mut self,
        _input: BalloonHintingStartInput,
    ) -> Result<(), BalloonHintingCommandError> {
        Err(BalloonHintingCommandError::ActiveSessionUnavailable)
    }

    fn stop_balloon_hinting(&mut self) -> Result<(), BalloonHintingCommandError> {
        Err(BalloonHintingCommandError::ActiveSessionUnavailable)
    }

    fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
        None
    }

    fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
        Ok(())
    }

    fn process_exit_status(&self) -> ProcessSessionExitStatus {
        ProcessSessionExitStatus::Running
    }
}

impl ProcessSessionDiagnostics for () {}

#[derive(Debug, Clone)]
pub(crate) struct BootRunLoopBlockDeviceUpdater {
    block_devices: Vec<Arm64BootBlockDevice>,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
}

#[derive(Debug, Clone)]
pub(crate) struct BootRunLoopBalloonDeviceUpdater {
    balloon_device: Arm64BootBalloonDevice,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
}

impl BootRunLoopBalloonDeviceUpdater {
    fn new(
        balloon_device: Arm64BootBalloonDevice,
        mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Self {
        Self {
            balloon_device,
            mmio_dispatcher,
        }
    }

    fn update_balloon_config(&self, config: BalloonConfig) -> Result<(), BalloonUpdateError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| BalloonUpdateError::MmioDispatcherUnavailable)?;

        update_balloon_config_for_device(&self.balloon_device, &mut dispatcher, config)
    }

    fn update_balloon_statistics(
        &self,
        input: BalloonStatsUpdateInput,
    ) -> Result<(), BalloonUpdateError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| BalloonUpdateError::MmioDispatcherUnavailable)?;

        update_balloon_statistics_for_device(&self.balloon_device, &mut dispatcher, input)
    }

    fn balloon_stats(&self, config: BalloonConfig) -> Result<BalloonStats, BalloonStatsError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| BalloonStatsError::MmioDispatcherUnavailable)?;

        balloon_stats_for_device(&self.balloon_device, &mut dispatcher, config)
    }

    fn balloon_hinting_status(&self) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| BalloonHintingStatusError::MmioDispatcherUnavailable)?;

        balloon_hinting_status_for_device(&self.balloon_device, &mut dispatcher)
    }

    fn start_balloon_hinting(
        &self,
        input: BalloonHintingStartInput,
    ) -> Result<(), BalloonHintingCommandError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| BalloonHintingCommandError::MmioDispatcherUnavailable)?;

        start_balloon_hinting_for_device(&self.balloon_device, &mut dispatcher, input)
    }

    fn stop_balloon_hinting(&self) -> Result<(), BalloonHintingCommandError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| BalloonHintingCommandError::MmioDispatcherUnavailable)?;

        stop_balloon_hinting_for_device(&self.balloon_device, &mut dispatcher)
    }
}

impl BootRunLoopBlockDeviceUpdater {
    fn new(
        block_devices: Vec<Arm64BootBlockDevice>,
        mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Self {
        Self {
            block_devices,
            mmio_dispatcher,
        }
    }

    fn open_block_device_backing(
        config: &DriveConfig,
    ) -> Result<BlockFileBacking, DriveUpdateError> {
        BlockFileBacking::open(config).map_err(|source| DriveUpdateError::OpenBacking {
            drive_id: config.drive_id().to_string(),
            message: source.to_string(),
        })
    }

    fn update_block_device_with_opened(
        &self,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        let mut dispatcher = self
            .mmio_dispatcher
            .lock()
            .map_err(|_| DriveUpdateError::MmioDispatcherUnavailable)?;

        update_block_device_for_devices_with_opened(
            &self.block_devices,
            &mut dispatcher,
            config,
            backing,
            rate_limiter_update,
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ProcessSessionExitStatus {
    #[default]
    Running,
    GuestRequestedStop,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessSessionExitDecision {
    Continue,
    ExitSuccessfully,
    ExitWithFailure,
}

impl ProcessSessionExitStatus {
    pub(crate) const fn decision(self) -> ProcessSessionExitDecision {
        match self {
            Self::Running => ProcessSessionExitDecision::Continue,
            Self::GuestRequestedStop => ProcessSessionExitDecision::ExitSuccessfully,
            Self::Terminal => ProcessSessionExitDecision::ExitWithFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GetApiRequest {
    Balloon,
    BalloonHintingStatus,
    BalloonStats,
    HotplugMemory,
    InstanceInfo,
    VmmVersion,
    MachineConfig,
    Mmds,
}

impl GetApiRequest {
    const fn action(self) -> VmmAction {
        match self {
            Self::Balloon => VmmAction::GetBalloon,
            Self::BalloonHintingStatus => VmmAction::GetBalloonHintingStatus,
            Self::BalloonStats => VmmAction::GetBalloonStats,
            Self::HotplugMemory => VmmAction::GetMemoryHotplug,
            Self::InstanceInfo => VmmAction::GetVmInstanceInfo,
            Self::VmmVersion => VmmAction::GetVmmVersion,
            Self::MachineConfig => VmmAction::GetMachineConfig,
            Self::Mmds => VmmAction::GetMmds,
        }
    }

    fn record(self, controller: &mut VmmController) {
        match self {
            Self::Balloon | Self::BalloonHintingStatus | Self::BalloonStats => {
                controller.record_get_balloon_request();
            }
            Self::HotplugMemory => controller.record_get_hotplug_memory_request(),
            Self::InstanceInfo => controller.record_get_instance_info_request(),
            Self::VmmVersion => controller.record_get_vmm_version_request(),
            Self::MachineConfig => controller.record_get_machine_config_request(),
            Self::Mmds => controller.record_get_mmds_request(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PutApiRequest {
    kind: PutApiRequestKind,
    action: VmmAction,
}

impl PutApiRequest {
    pub(crate) const fn balloon(input: BalloonConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Balloon,
            action: VmmAction::PutBalloon(input),
        }
    }

    pub(crate) fn boot_source(input: BootSourceConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::BootSource,
            action: VmmAction::PutBootSource(input),
        }
    }

    pub(crate) fn cpu_config(input: CpuConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::CpuConfig,
            action: VmmAction::PutCpuConfig(input),
        }
    }

    pub(crate) fn drive(input: DriveConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Drive,
            action: VmmAction::PutDrive(input),
        }
    }

    pub(crate) fn metrics(input: MetricsConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Metrics,
            action: VmmAction::PutMetrics(input),
        }
    }

    pub(crate) const fn memory_hotplug(input: MemoryHotplugConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::HotplugMemory,
            action: VmmAction::PutMemoryHotplug(input),
        }
    }

    pub(crate) fn logger(input: LoggerConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Logger,
            action: VmmAction::PutLogger(input),
        }
    }

    pub(crate) fn machine_config(input: MachineConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::MachineConfig,
            action: VmmAction::PutMachineConfig(input),
        }
    }

    pub(crate) fn mmds(input: MmdsContentInput) -> Self {
        Self {
            kind: PutApiRequestKind::Mmds,
            action: VmmAction::PutMmds(input),
        }
    }

    pub(crate) fn mmds_config(input: MmdsConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Mmds,
            action: VmmAction::PutMmdsConfig(input),
        }
    }

    pub(crate) fn network(input: NetworkInterfaceConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Network,
            action: VmmAction::PutNetworkInterface(input),
        }
    }

    pub(crate) fn pmem(input: bangbang_runtime::pmem::PmemConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Pmem,
            action: VmmAction::PutPmem(input),
        }
    }

    pub(crate) fn serial(input: SerialConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Serial,
            action: VmmAction::PutSerial(input),
        }
    }

    pub(crate) fn vsock(input: VsockConfigInput) -> Self {
        Self {
            kind: PutApiRequestKind::Vsock,
            action: VmmAction::PutVsock(input),
        }
    }

    fn record_request(&self, controller: &mut VmmController) {
        self.kind.record_request(controller);
    }

    fn into_action(self) -> VmmAction {
        self.action
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PutApiRequestKind {
    Balloon,
    BootSource,
    CpuConfig,
    Drive,
    HotplugMemory,
    Metrics,
    Logger,
    MachineConfig,
    Mmds,
    Network,
    Pmem,
    Serial,
    Vsock,
}

impl PutApiRequestKind {
    fn record_request(self, controller: &mut VmmController) {
        match self {
            Self::Balloon => controller.record_put_balloon_request(),
            Self::BootSource => controller.record_put_boot_source_request(),
            Self::CpuConfig => controller.record_put_cpu_config_request(),
            Self::Drive => controller.record_put_drive_request(),
            Self::HotplugMemory => controller.record_put_hotplug_memory_request(),
            Self::Metrics => controller.record_put_metrics_request(),
            Self::Logger => controller.record_put_logger_request(),
            Self::MachineConfig => controller.record_put_machine_config_request(),
            Self::Mmds => controller.record_put_mmds_request(),
            Self::Network => controller.record_put_network_request(),
            Self::Pmem => controller.record_put_pmem_request(),
            Self::Serial => controller.record_put_serial_request(),
            Self::Vsock => controller.record_put_vsock_request(),
        }
    }

    fn record_failure(self, controller: &mut VmmController) {
        match self {
            Self::Balloon => controller.record_put_balloon_failure(),
            Self::BootSource => controller.record_put_boot_source_failure(),
            Self::CpuConfig => controller.record_put_cpu_config_failure(),
            Self::Drive => controller.record_put_drive_failure(),
            Self::HotplugMemory => controller.record_put_hotplug_memory_failure(),
            Self::Metrics => controller.record_put_metrics_failure(),
            Self::Logger => controller.record_put_logger_failure(),
            Self::MachineConfig => controller.record_put_machine_config_failure(),
            Self::Mmds => controller.record_put_mmds_failure(),
            Self::Network => controller.record_put_network_failure(),
            Self::Pmem => controller.record_put_pmem_failure(),
            Self::Serial => controller.record_put_serial_failure(),
            Self::Vsock => controller.record_put_vsock_failure(),
        }
    }

    fn record_parse_failure(self, controller: &mut VmmController) {
        self.record_request(controller);
        self.record_failure(controller);
    }
}

#[derive(Debug)]
pub(crate) struct PatchApiRequest {
    kind: PatchApiRequestKind,
    action: VmmAction,
}

impl PatchApiRequest {
    pub(crate) const fn balloon(input: BalloonUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Balloon,
            action: VmmAction::PatchBalloon(input),
        }
    }

    pub(crate) const fn balloon_hinting_start(input: BalloonHintingStartInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Balloon,
            action: VmmAction::PatchBalloonHintingStart(input),
        }
    }

    pub(crate) const fn balloon_hinting_stop() -> Self {
        Self {
            kind: PatchApiRequestKind::Balloon,
            action: VmmAction::PatchBalloonHintingStop,
        }
    }

    pub(crate) const fn balloon_stats(input: BalloonStatsUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Balloon,
            action: VmmAction::PatchBalloonStats(input),
        }
    }

    pub(crate) fn drive(input: DriveUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Drive,
            action: VmmAction::UpdateBlockDevice(input),
        }
    }

    pub(crate) fn machine_config(input: MachineConfigPatchInput) -> Self {
        Self {
            kind: PatchApiRequestKind::MachineConfig,
            action: VmmAction::PatchMachineConfig(input),
        }
    }

    pub(crate) fn mmds(input: MmdsContentInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Mmds,
            action: VmmAction::PatchMmds(input),
        }
    }

    pub(crate) const fn memory_hotplug(input: MemoryHotplugSizeUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::HotplugMemory,
            action: VmmAction::PatchMemoryHotplug(input),
        }
    }

    pub(crate) fn network(input: NetworkInterfaceUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Network,
            action: VmmAction::UpdateNetworkInterface(input),
        }
    }

    pub(crate) const fn pmem(input: PmemUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Pmem,
            action: VmmAction::PatchPmem(input),
        }
    }

    fn record_request(&self, controller: &mut VmmController) {
        self.kind.record_request(controller);
    }

    fn into_action(self) -> VmmAction {
        self.action
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiRequestMetricParseFailure {
    Patch(ApiRequestMetricPatchParseFailure),
    Put(ApiRequestMetricPutParseFailure),
}

impl ApiRequestMetricParseFailure {
    fn record(self, controller: &mut VmmController) {
        match self {
            Self::Patch(request) => request.record(controller),
            Self::Put(request) => request.record(controller),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiRequestMetricPutParseFailure {
    Actions,
    Balloon,
    BootSource,
    CpuConfig,
    Drive,
    HotplugMemory,
    Logger,
    MachineConfig,
    Metrics,
    Mmds,
    Network,
    Pmem,
    Serial,
    Vsock,
}

impl ApiRequestMetricPutParseFailure {
    fn record(self, controller: &mut VmmController) {
        match self {
            Self::Actions => {
                controller.record_put_actions_request();
                controller.record_put_actions_failure();
            }
            Self::Balloon => PutApiRequestKind::Balloon.record_parse_failure(controller),
            Self::BootSource => PutApiRequestKind::BootSource.record_parse_failure(controller),
            Self::CpuConfig => PutApiRequestKind::CpuConfig.record_parse_failure(controller),
            Self::Drive => PutApiRequestKind::Drive.record_parse_failure(controller),
            Self::HotplugMemory => {
                PutApiRequestKind::HotplugMemory.record_parse_failure(controller)
            }
            Self::Logger => PutApiRequestKind::Logger.record_parse_failure(controller),
            Self::MachineConfig => {
                PutApiRequestKind::MachineConfig.record_parse_failure(controller)
            }
            Self::Metrics => PutApiRequestKind::Metrics.record_parse_failure(controller),
            Self::Mmds => PutApiRequestKind::Mmds.record_parse_failure(controller),
            Self::Network => PutApiRequestKind::Network.record_parse_failure(controller),
            Self::Pmem => PutApiRequestKind::Pmem.record_parse_failure(controller),
            Self::Serial => PutApiRequestKind::Serial.record_parse_failure(controller),
            Self::Vsock => PutApiRequestKind::Vsock.record_parse_failure(controller),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiRequestMetricPatchParseFailure {
    Balloon,
    Drive,
    HotplugMemory,
    MachineConfig,
    Mmds,
    Network,
    Pmem,
}

impl ApiRequestMetricPatchParseFailure {
    fn record(self, controller: &mut VmmController) {
        match self {
            Self::Balloon => PatchApiRequestKind::Balloon.record_parse_failure(controller),
            Self::Drive => PatchApiRequestKind::Drive.record_parse_failure(controller),
            Self::HotplugMemory => {
                PatchApiRequestKind::HotplugMemory.record_parse_failure(controller)
            }
            Self::MachineConfig => {
                PatchApiRequestKind::MachineConfig.record_parse_failure(controller)
            }
            Self::Mmds => PatchApiRequestKind::Mmds.record_parse_failure(controller),
            Self::Network => PatchApiRequestKind::Network.record_parse_failure(controller),
            Self::Pmem => PatchApiRequestKind::Pmem.record_parse_failure(controller),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchApiRequestKind {
    Balloon,
    Drive,
    HotplugMemory,
    MachineConfig,
    Mmds,
    Network,
    Pmem,
}

impl PatchApiRequestKind {
    fn record_request(self, controller: &mut VmmController) {
        match self {
            Self::Balloon => controller.record_patch_balloon_request(),
            Self::Drive => controller.record_patch_drive_request(),
            Self::HotplugMemory => controller.record_patch_hotplug_memory_request(),
            Self::MachineConfig => controller.record_patch_machine_config_request(),
            Self::Mmds => controller.record_patch_mmds_request(),
            Self::Network => controller.record_patch_network_request(),
            Self::Pmem => controller.record_patch_pmem_request(),
        }
    }

    fn record_failure(self, controller: &mut VmmController) {
        match self {
            Self::Balloon => controller.record_patch_balloon_failure(),
            Self::Drive => controller.record_patch_drive_failure(),
            Self::HotplugMemory => controller.record_patch_hotplug_memory_failure(),
            Self::MachineConfig => controller.record_patch_machine_config_failure(),
            Self::Mmds => controller.record_patch_mmds_failure(),
            Self::Network => controller.record_patch_network_failure(),
            Self::Pmem => controller.record_patch_pmem_failure(),
        }
    }

    fn record_parse_failure(self, controller: &mut VmmController) {
        self.record_request(controller);
        self.record_failure(controller);
    }
}

pub(crate) trait VmmRequestHandler {
    fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError>;

    fn handle_get_request(&mut self, request: GetApiRequest) -> Result<VmmData, VmmActionError>;

    fn handle_patch_request(&mut self, request: PatchApiRequest)
    -> Result<VmmData, VmmActionError>;

    fn handle_put_request(&mut self, request: PutApiRequest) -> Result<VmmData, VmmActionError>;

    fn record_api_request_parse_failure(&mut self, request: ApiRequestMetricParseFailure);

    fn record_put_actions_request(&mut self);

    fn handle_put_action_request(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError>;

    fn record_deprecated_api_call(&mut self);

    fn record_pause_vm_latency_us(&mut self, duration_us: u64);

    fn record_resume_vm_latency_us(&mut self, duration_us: u64);

    fn handle_periodic_metrics_flush(&mut self) -> Result<bool, VmmActionError> {
        Ok(false)
    }

    fn balloon_statistics_update_interval(&self) -> Option<Duration> {
        None
    }

    fn handle_periodic_balloon_statistics_update(&mut self) -> Result<bool, VmmActionError> {
        Ok(false)
    }

    fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
        None
    }

    fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
        Ok(())
    }

    fn process_exit_status(&self) -> ProcessSessionExitStatus {
        ProcessSessionExitStatus::Running
    }
}

#[derive(Debug)]
pub(crate) struct ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    controller: VmmController,
    starter: S,
    started_session: Option<S::Session>,
    process_metrics_diagnostics: MetricsDiagnostics,
    process_signal_metrics: Option<SharedSignalMetrics>,
}

impl ProcessVmm<HvfInstanceStartExecutor> {
    pub(crate) fn new(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
        mmds_data_store_limit_bytes: usize,
    ) -> Self {
        Self::with_starter_and_mmds_data_store_limit(
            instance_id,
            vmm_version,
            app_name,
            HvfInstanceStartExecutor::default(),
            mmds_data_store_limit_bytes,
        )
    }

    pub(crate) fn with_boot_timer_enabled(mut self, enabled: bool) -> Self {
        self.starter.boot_timer_enabled = enabled;
        self
    }
}

impl<S> ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    #[cfg(test)]
    pub(crate) fn with_starter(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
        starter: S,
    ) -> Self {
        Self::with_starter_and_mmds_data_store_limit(
            instance_id,
            vmm_version,
            app_name,
            starter,
            bangbang_runtime::mmds::MMDS_DATA_STORE_LIMIT_BYTES,
        )
    }

    pub(crate) fn with_starter_and_mmds_data_store_limit(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
        starter: S,
        mmds_data_store_limit_bytes: usize,
    ) -> Self {
        Self {
            controller: VmmController::with_mmds_data_store_limit(
                instance_id,
                vmm_version,
                app_name,
                mmds_data_store_limit_bytes,
            ),
            starter,
            started_session: None,
            process_metrics_diagnostics: MetricsDiagnostics::default(),
            process_signal_metrics: None,
        }
    }

    pub(crate) fn with_process_metrics_diagnostics(
        mut self,
        diagnostics: MetricsDiagnostics,
    ) -> Self {
        self.process_metrics_diagnostics = diagnostics;
        self
    }

    pub(crate) fn with_process_signal_metrics(mut self, metrics: SharedSignalMetrics) -> Self {
        self.process_signal_metrics = Some(metrics);
        self
    }

    #[cfg(test)]
    pub(crate) const fn has_started_session(&self) -> bool {
        self.started_session.is_some()
    }
}

impl<S> ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    #[cfg(test)]
    pub(crate) fn instance_info(&self) -> &InstanceInfo {
        self.controller.instance_info()
    }

    #[cfg(test)]
    pub(crate) fn drive_configs(&self) -> &[DriveConfig] {
        self.controller.drive_configs()
    }

    #[cfg(test)]
    pub(crate) const fn machine_config(&self) -> MachineConfig {
        self.controller.machine_config()
    }

    #[cfg(test)]
    pub(crate) fn boot_source_config(&self) -> Option<&BootSourceConfig> {
        self.controller.boot_source_config()
    }

    #[cfg(test)]
    pub(crate) const fn serial_config(&self) -> &SerialConfig {
        self.controller.serial_config()
    }

    pub(crate) fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        match action {
            VmmAction::InstanceStart => self.start_instance(),
            VmmAction::Pause => self.pause_instance(),
            VmmAction::Resume => self.resume_instance(),
            VmmAction::UpdateBlockDevice(input) => self.update_block_device(input),
            VmmAction::PatchBalloon(input) => self.update_balloon(input),
            VmmAction::PatchBalloonStats(input) => self.update_balloon_statistics(input),
            VmmAction::PatchMemoryHotplug(input) => self.update_memory_hotplug(input),
            VmmAction::GetMemoryHotplug => self.memory_hotplug_status(),
            VmmAction::GetBalloonStats => self.balloon_stats(),
            VmmAction::GetBalloonHintingStatus => self.balloon_hinting_status(),
            VmmAction::PatchBalloonHintingStart(input) => self.start_balloon_hinting(input),
            VmmAction::PatchBalloonHintingStop => self.stop_balloon_hinting(),
            VmmAction::FlushMetrics => self.flush_metrics(),
            action => self.controller.handle_action(action),
        }
    }

    fn handle_put_action_request(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        self.controller.record_put_actions_request();
        let result = self.handle_action(action);
        if result.is_err() {
            self.controller.record_put_actions_failure();
        }
        result
    }

    fn handle_get_request(&mut self, request: GetApiRequest) -> Result<VmmData, VmmActionError> {
        request.record(&mut self.controller);
        self.handle_action(request.action())
    }

    fn handle_patch_request(
        &mut self,
        request: PatchApiRequest,
    ) -> Result<VmmData, VmmActionError> {
        let kind = request.kind;
        request.record_request(&mut self.controller);
        let action = request.into_action();
        let result = self.handle_action(action);
        if result.is_err() {
            kind.record_failure(&mut self.controller);
        }
        result
    }

    fn handle_put_request(&mut self, request: PutApiRequest) -> Result<VmmData, VmmActionError> {
        let kind = request.kind;
        request.record_request(&mut self.controller);
        let action = request.into_action();
        let result = self.handle_action(action);
        if result.is_err() {
            kind.record_failure(&mut self.controller);
        }
        result
    }

    fn record_api_request_parse_failure(&mut self, request: ApiRequestMetricParseFailure) {
        request.record(&mut self.controller);
    }

    fn record_put_actions_request(&mut self) {
        self.controller.record_put_actions_request();
    }

    fn record_deprecated_api_call(&mut self) {
        self.controller.record_deprecated_api_call();
    }

    fn record_pause_vm_latency_us(&mut self, duration_us: u64) {
        self.controller.record_pause_vm_latency_us(duration_us);
    }

    fn record_resume_vm_latency_us(&mut self, duration_us: u64) {
        self.controller.record_resume_vm_latency_us(duration_us);
    }

    fn start_instance(&mut self) -> Result<VmmData, VmmActionError> {
        let controller = &mut self.controller;
        let starter = &mut self.starter;
        let mut started_session = None;

        let result = controller.start_instance_with(|controller| {
            started_session = Some(starter.start(controller)?);
            Ok(())
        });

        match result {
            Ok(data) => match started_session {
                Some(session) => {
                    self.started_session = Some(session);
                    Ok(data)
                }
                None => Err(VmmActionError::InstanceStart(BackendError::InvalidState(
                    "startup executor completed without a session",
                ))),
            },
            Err(err) => Err(err),
        }
    }

    fn flush_metrics(&mut self) -> Result<VmmData, VmmActionError> {
        let diagnostics = self.metrics_diagnostics();

        self.controller.flush_metrics_with_diagnostics(&diagnostics)
    }

    fn pause_instance(&mut self) -> Result<VmmData, VmmActionError> {
        self.controller.preflight_pause_instance()?;
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::Lifecycle(BackendError::InvalidState(
                "active session unavailable",
            )));
        };

        session.pause().map_err(VmmActionError::Lifecycle)?;
        self.controller.pause_instance()
    }

    fn resume_instance(&mut self) -> Result<VmmData, VmmActionError> {
        self.controller.preflight_resume_instance()?;
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::Lifecycle(BackendError::InvalidState(
                "active session unavailable",
            )));
        };

        session.resume().map_err(VmmActionError::Lifecycle)?;
        self.controller.resume_instance()
    }

    fn update_block_device(&mut self, input: DriveUpdateInput) -> Result<VmmData, VmmActionError> {
        if self.controller.instance_info().state == bangbang_runtime::InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::UpdateBlockDevice(input).name(),
                state: self.controller.instance_info().state,
            });
        }

        let refresh_backing = input.path_on_host().is_some();
        let rate_limiter_update = input.rate_limiter();
        let updated_config = self.controller.updated_drive_config(input)?;
        if refresh_backing || rate_limiter_update.is_some() {
            let Some(session) = self.started_session.as_mut() else {
                return Err(VmmActionError::DriveUpdate(
                    DriveUpdateError::ActiveSessionUnavailable,
                ));
            };

            session
                .update_block_device(&updated_config, refresh_backing, rate_limiter_update)
                .map_err(VmmActionError::DriveUpdate)?;
        }
        self.controller.commit_drive_update(updated_config)?;

        Ok(VmmData::Empty)
    }

    fn update_balloon(&mut self, input: BalloonUpdateInput) -> Result<VmmData, VmmActionError> {
        let updated_config = self.controller.updated_balloon_config(input)?;
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::BalloonUpdate(
                BalloonUpdateError::ActiveSessionUnavailable,
            ));
        };

        session
            .update_balloon(updated_config)
            .map_err(VmmActionError::BalloonUpdate)?;
        self.controller.commit_balloon_update(updated_config);

        Ok(VmmData::Empty)
    }

    fn update_balloon_statistics(
        &mut self,
        input: BalloonStatsUpdateInput,
    ) -> Result<VmmData, VmmActionError> {
        let updated_config = self.controller.updated_balloon_stats_config(input)?;
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::BalloonUpdate(
                BalloonUpdateError::ActiveSessionUnavailable,
            ));
        };

        session
            .update_balloon_statistics(input)
            .map_err(VmmActionError::BalloonUpdate)?;
        self.controller.commit_balloon_update(updated_config);

        Ok(VmmData::Empty)
    }

    fn update_memory_hotplug(
        &mut self,
        input: MemoryHotplugSizeUpdateInput,
    ) -> Result<VmmData, VmmActionError> {
        let update = self.controller.memory_hotplug_size_update(input)?;
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::MemoryHotplugUpdate(
                MemoryHotplugUpdateError::ActiveSessionUnavailable,
            ));
        };

        session
            .update_memory_hotplug(update)
            .map_err(VmmActionError::MemoryHotplugUpdate)?;
        self.controller.commit_memory_hotplug_size_update(update);

        Ok(VmmData::Empty)
    }

    fn memory_hotplug_status(&mut self) -> Result<VmmData, VmmActionError> {
        if self.controller.instance_info().state == bangbang_runtime::InstanceState::NotStarted {
            return self.controller.handle_action(VmmAction::GetMemoryHotplug);
        }

        let config = self
            .controller
            .memory_hotplug_config()
            .ok_or(VmmActionError::MemoryHotplugUnsupported)?;
        let requested_size_mib = self
            .controller
            .memory_hotplug_status()
            .ok_or(VmmActionError::MemoryHotplugUnsupported)?
            .requested_size_mib();
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::MemoryHotplugStatus(
                MemoryHotplugStatusError::ActiveSessionUnavailable,
            ));
        };
        let status = session
            .memory_hotplug_status(config, requested_size_mib)
            .map_err(VmmActionError::MemoryHotplugStatus)?;

        Ok(VmmData::MemoryHotplugStatus(status))
    }

    fn balloon_stats(&mut self) -> Result<VmmData, VmmActionError> {
        if self.controller.instance_info().state == bangbang_runtime::InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::GetBalloonStats.name(),
                state: self.controller.instance_info().state,
            });
        }

        let config = self
            .controller
            .balloon_config()
            .ok_or(VmmActionError::BalloonUnsupported)?;
        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::BalloonStats(
                BalloonStatsError::ActiveSessionUnavailable,
            ));
        };
        let stats = session
            .balloon_stats(config)
            .map_err(VmmActionError::BalloonStats)?;

        Ok(VmmData::BalloonStatistics(stats))
    }

    fn balloon_hinting_status(&mut self) -> Result<VmmData, VmmActionError> {
        if self.controller.instance_info().state == bangbang_runtime::InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::GetBalloonHintingStatus.name(),
                state: self.controller.instance_info().state,
            });
        }

        let config = self
            .controller
            .balloon_config()
            .ok_or(VmmActionError::BalloonUnsupported)?;
        if !config.free_page_hinting() {
            return Err(VmmActionError::BalloonUnsupported);
        }

        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::BalloonHintingStatus(
                BalloonHintingStatusError::ActiveSessionUnavailable,
            ));
        };
        let status = session.balloon_hinting_status().map_err(|err| match err {
            BalloonHintingStatusError::HintingNotEnabled => VmmActionError::BalloonUnsupported,
            err => VmmActionError::BalloonHintingStatus(err),
        })?;

        Ok(VmmData::BalloonHintingStatus(status))
    }

    fn start_balloon_hinting(
        &mut self,
        input: BalloonHintingStartInput,
    ) -> Result<VmmData, VmmActionError> {
        self.with_active_hinting_session(VmmAction::PatchBalloonHintingStart(input), |session| {
            session.start_balloon_hinting(input)
        })?;

        Ok(VmmData::Empty)
    }

    fn stop_balloon_hinting(&mut self) -> Result<VmmData, VmmActionError> {
        self.with_active_hinting_session(VmmAction::PatchBalloonHintingStop, |session| {
            session.stop_balloon_hinting()
        })?;

        Ok(VmmData::Empty)
    }

    fn with_active_hinting_session(
        &mut self,
        action: VmmAction,
        command: impl FnOnce(&mut S::Session) -> Result<(), BalloonHintingCommandError>,
    ) -> Result<(), VmmActionError> {
        if self.controller.instance_info().state == bangbang_runtime::InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: action.name(),
                state: self.controller.instance_info().state,
            });
        }

        let config = self
            .controller
            .balloon_config()
            .ok_or(VmmActionError::BalloonUnsupported)?;
        if !config.free_page_hinting() {
            return Err(VmmActionError::BalloonUnsupported);
        }

        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::BalloonHintingCommand(
                BalloonHintingCommandError::ActiveSessionUnavailable,
            ));
        };

        command(session).map_err(|err| match err {
            BalloonHintingCommandError::HintingNotEnabled => VmmActionError::BalloonUnsupported,
            err => VmmActionError::BalloonHintingCommand(err),
        })
    }

    pub(crate) fn flush_startup_metrics(&mut self) -> Result<bool, VmmActionError> {
        let diagnostics = self.metrics_diagnostics();

        self.controller
            .flush_startup_metrics_with_diagnostics(&diagnostics)
    }

    fn flush_periodic_metrics(&mut self) -> Result<bool, VmmActionError> {
        let diagnostics = self.metrics_diagnostics();

        self.controller
            .flush_periodic_metrics_with_diagnostics(&diagnostics)
    }

    fn balloon_statistics_update_interval(&self) -> Option<Duration> {
        if self.controller.instance_info().state != bangbang_runtime::InstanceState::Running {
            return None;
        }

        let interval_s = self.controller.balloon_config()?.stats_polling_interval_s();
        if interval_s == 0 {
            return None;
        }

        Some(Duration::from_secs(u64::from(interval_s)))
    }

    fn trigger_periodic_balloon_statistics_update(&mut self) -> Result<bool, VmmActionError> {
        if self.balloon_statistics_update_interval().is_none() {
            return Ok(false);
        }

        let Some(session) = self.started_session.as_mut() else {
            return Err(VmmActionError::BalloonUpdate(
                BalloonUpdateError::ActiveSessionUnavailable,
            ));
        };

        session
            .trigger_balloon_statistics_update()
            .map_err(VmmActionError::BalloonUpdate)?;

        Ok(true)
    }

    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        let session_diagnostics = self
            .started_session
            .as_ref()
            .map(ProcessSessionDiagnostics::metrics_diagnostics)
            .unwrap_or_default();
        let signal_diagnostics = self
            .process_signal_metrics
            .as_ref()
            .map(|metrics| MetricsDiagnostics::new().with_signal_metrics(metrics.snapshot()))
            .unwrap_or_default();
        self.process_metrics_diagnostics
            .clone()
            .merged_with(signal_diagnostics)
            .merged_with(self.starter.metrics_diagnostics())
            .merged_with(session_diagnostics)
    }

    pub(crate) fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
        self.started_session
            .as_ref()
            .and_then(ProcessSessionDiagnostics::process_exit_wakeup_fd)
    }

    pub(crate) fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
        if let Some(session) = self.started_session.as_mut() {
            session.drain_process_exit_wakeup()?;
        }

        Ok(())
    }

    pub(crate) fn process_exit_status(&self) -> ProcessSessionExitStatus {
        self.started_session
            .as_ref()
            .map(ProcessSessionDiagnostics::process_exit_status)
            .unwrap_or_default()
    }
}

impl<S> VmmRequestHandler for ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        ProcessVmm::handle_action(self, action)
    }

    fn handle_put_action_request(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        ProcessVmm::handle_put_action_request(self, action)
    }

    fn handle_periodic_metrics_flush(&mut self) -> Result<bool, VmmActionError> {
        ProcessVmm::flush_periodic_metrics(self)
    }

    fn balloon_statistics_update_interval(&self) -> Option<Duration> {
        ProcessVmm::balloon_statistics_update_interval(self)
    }

    fn handle_periodic_balloon_statistics_update(&mut self) -> Result<bool, VmmActionError> {
        ProcessVmm::trigger_periodic_balloon_statistics_update(self)
    }

    fn handle_get_request(&mut self, request: GetApiRequest) -> Result<VmmData, VmmActionError> {
        ProcessVmm::handle_get_request(self, request)
    }

    fn handle_patch_request(
        &mut self,
        request: PatchApiRequest,
    ) -> Result<VmmData, VmmActionError> {
        ProcessVmm::handle_patch_request(self, request)
    }

    fn handle_put_request(&mut self, request: PutApiRequest) -> Result<VmmData, VmmActionError> {
        ProcessVmm::handle_put_request(self, request)
    }

    fn record_api_request_parse_failure(&mut self, request: ApiRequestMetricParseFailure) {
        ProcessVmm::record_api_request_parse_failure(self, request);
    }

    fn record_put_actions_request(&mut self) {
        ProcessVmm::record_put_actions_request(self);
    }

    fn record_deprecated_api_call(&mut self) {
        ProcessVmm::record_deprecated_api_call(self);
    }

    fn record_pause_vm_latency_us(&mut self, duration_us: u64) {
        ProcessVmm::record_pause_vm_latency_us(self, duration_us);
    }

    fn record_resume_vm_latency_us(&mut self, duration_us: u64) {
        ProcessVmm::record_resume_vm_latency_us(self, duration_us);
    }

    fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
        ProcessVmm::process_exit_wakeup_fd(self)
    }

    fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
        ProcessVmm::drain_process_exit_wakeup(self)
    }

    fn process_exit_status(&self) -> ProcessSessionExitStatus {
        ProcessVmm::process_exit_status(self)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HvfInstanceStartExecutor {
    boot_timer_enabled: bool,
    serial_output: SharedSerialOutputBuffer,
    active_serial_output: Option<SharedSerialOutput>,
}

impl HvfInstanceStartExecutor {
    #[cfg(test)]
    fn boot_session_config(&self) -> HvfArm64BootSessionConfig {
        default_hvf_boot_session_config(SharedSerialOutput::from(self.serial_output.clone()))
    }

    fn serial_output_for_controller(
        &self,
        controller: &VmmController,
    ) -> Result<SharedSerialOutput, SerialConfigError> {
        match controller.serial_config().serial_out_path() {
            Some(path) => Ok(SharedSerialOutput::with_rate_limiter(
                SerialOutputFile::open(path)?,
                controller.serial_config().rate_limiter(),
            )),
            None => Ok(SharedSerialOutput::with_rate_limiter(
                self.serial_output.clone(),
                controller.serial_config().rate_limiter(),
            )),
        }
    }

    #[cfg(test)]
    fn boot_session_config_for_controller(
        &self,
        controller: &VmmController,
    ) -> Result<HvfArm64BootSessionConfig, SerialConfigError> {
        let serial_output = self.serial_output_for_controller(controller)?;
        Ok(self.boot_session_config_for_controller_with_serial_output(controller, serial_output))
    }

    fn boot_session_config_for_controller_with_serial_output(
        &self,
        controller: &VmmController,
        serial_output: SharedSerialOutput,
    ) -> HvfArm64BootSessionConfig {
        let mut config = default_hvf_boot_session_config(serial_output);
        if controller.entropy_config().is_some() {
            config = config.with_entropy_device(HvfArm64BootEntropyDeviceConfig::new(
                EntropyMmioLayout::new(DEFAULT_ENTROPY_MMIO_BASE, DEFAULT_ENTROPY_MMIO_REGION_ID),
            ));
        }
        if controller.balloon_config().is_some() {
            config = config.with_balloon_device(HvfArm64BootBalloonDeviceConfig::new(
                BalloonMmioLayout::new(DEFAULT_BALLOON_MMIO_BASE, DEFAULT_BALLOON_MMIO_REGION_ID),
            ));
        }
        if controller.memory_hotplug_config().is_some() {
            config = config.with_memory_hotplug_device(HvfArm64BootMemoryHotplugDeviceConfig::new(
                VirtioMemMmioLayout::new(
                    DEFAULT_MEMORY_HOTPLUG_MMIO_BASE,
                    DEFAULT_MEMORY_HOTPLUG_MMIO_REGION_ID,
                ),
            ));
        }
        if self.boot_timer_enabled {
            config = config.with_boot_timer_device(HvfArm64BootTimerDeviceConfig::new(
                BootTimerMmioLayout::new(
                    DEFAULT_BOOT_TIMER_MMIO_BASE,
                    DEFAULT_BOOT_TIMER_MMIO_REGION_ID,
                ),
            ));
        }

        config
    }
}

impl InstanceStartExecutor for HvfInstanceStartExecutor {
    type Session = HvfBootRunLoopSupervisor;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError> {
        let serial_output = self
            .serial_output_for_controller(controller)
            .map_err(|err| {
                BackendError::Hypervisor(format!("failed to initialize serial output: {err}"))
            })?;
        let boot_session_config = self.boot_session_config_for_controller_with_serial_output(
            controller,
            serial_output.clone(),
        );
        let packet_io =
            ProcessNetworkPacketIoProvider::from_controller(controller).map_err(|err| {
                BackendError::Hypervisor(format!(
                    "failed to build network packet I/O provider: {err}"
                ))
            })?;
        let session = OwnedHvfArm64BootSession::new(controller, boot_session_config)
            .map_err(|err| BackendError::Hypervisor(err.to_string()))?;
        let session = ProcessHvfBootSession::new(session, packet_io);
        let supervisor =
            HvfBootRunLoopSupervisor::start(session, default_hvf_boot_run_loop_step_limit())?;
        self.active_serial_output = Some(serial_output);

        Ok(supervisor)
    }

    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        self.active_serial_output
            .as_ref()
            .map(|output| MetricsDiagnostics::new().with_serial_output_metrics(output.metrics()))
            .unwrap_or_default()
    }
}

pub(crate) type HvfBootRunLoopSupervisor = BootRunLoopSupervisor<
    ProcessHvfBootSession<OwnedHvfArm64BootSession, ProcessNetworkPacketIoProvider>,
>;

pub(crate) struct ProcessHvfBootSession<S, P> {
    session: S,
    packet_io: P,
}

impl<S, P> ProcessHvfBootSession<S, P> {
    const fn new(session: S, packet_io: P) -> Self {
        Self { session, packet_io }
    }
}

impl<S, P> fmt::Debug for ProcessHvfBootSession<S, P>
where
    S: fmt::Debug,
    P: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessHvfBootSession")
            .field("session", &self.session)
            .field("packet_io", &self.packet_io)
            .finish()
    }
}

#[derive(Debug, Default)]
pub(crate) struct NoopProcessNetworkPacketIoProvider {
    tx_sink: NoopProcessNetworkTxPacketSink,
    rx_source: EmptyProcessNetworkRxPacketSource,
}

impl Arm64BootNetworkPacketIoProvider for NoopProcessNetworkPacketIoProvider {
    fn packet_io(
        &mut self,
        _device: &Arm64BootNetworkDevice,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
        Ok(Arm64BootNetworkPacketIo::new(
            &mut self.tx_sink,
            &mut self.rx_source,
        ))
    }
}

type SystemStartedVmnetPacketIoBackend = StartedVmnetPacketIoBackend<SystemVmnetInterfaceBackend>;
type SystemProcessVmnetPacketIoProvider =
    VmnetVirtioNetworkPacketIoProvider<SystemStartedVmnetPacketIoBackend>;

#[derive(Debug)]
pub(crate) enum ProcessNetworkPacketIoProvider {
    Noop(NoopProcessNetworkPacketIoProvider),
    MmdsOnly(MmdsOnlyVirtioNetworkPacketIoProvider),
    Vmnet(SystemProcessVmnetPacketIoProvider),
}

impl ProcessNetworkPacketIoProvider {
    fn from_controller(
        controller: &VmmController,
    ) -> Result<Self, ProcessNetworkPacketIoProviderBuildError> {
        let mmds_config = controller
            .mmds_config()
            .map_err(|source| ProcessNetworkPacketIoProviderBuildError::MmdsState { source })?;
        let mmds_detour = mmds_config.as_ref().map(|config| {
            ProcessMmdsPacketDetourConfig::from_mmds_config(controller.mmds_state_handle(), config)
        });

        Self::from_network_configs_and_mmds_detour(
            controller.network_interface_configs(),
            mmds_detour.as_ref(),
        )
    }

    #[cfg(test)]
    fn from_network_configs(
        configs: &[NetworkInterfaceConfig],
    ) -> Result<Self, ProcessNetworkPacketIoProviderBuildError> {
        Self::from_network_configs_and_mmds_detour(configs, None)
    }

    fn from_network_configs_and_mmds_detour(
        configs: &[NetworkInterfaceConfig],
        mmds_detour: Option<&ProcessMmdsPacketDetourConfig>,
    ) -> Result<Self, ProcessNetworkPacketIoProviderBuildError> {
        validate_network_interface_count(configs.len()).map_err(|source| {
            ProcessNetworkPacketIoProviderBuildError::NetworkInterfaceCount { source }
        })?;

        if configs.is_empty() {
            return Ok(Self::Noop(NoopProcessNetworkPacketIoProvider::default()));
        }

        if let Some(mmds_detour) = mmds_detour
            && mmds_detour.covers_all_interfaces(configs)
        {
            return process_mmds_only_packet_io_provider_from_configs(configs, mmds_detour)
                .map(Self::MmdsOnly);
        }

        let mut factory = SystemProcessVmnetPacketIoBackendFactory;
        process_vmnet_packet_io_provider_from_configs_with_mmds_detour(
            configs,
            &mut factory,
            mmds_detour,
        )
        .map(Self::Vmnet)
    }
}

#[derive(Debug, Clone)]
struct ProcessMmdsPacketDetourConfig {
    mmds_state: MmdsStateHandle,
    mmds_ipv4_address: Ipv4Addr,
    network_interfaces: Vec<String>,
}

impl ProcessMmdsPacketDetourConfig {
    fn from_mmds_config(mmds_state: MmdsStateHandle, config: &MmdsConfig) -> Self {
        Self {
            mmds_state,
            mmds_ipv4_address: config.effective_ipv4_address(),
            network_interfaces: config.network_interfaces().to_vec(),
        }
    }

    fn detour_for_interface(&self, iface_id: &str) -> Option<MmdsPacketDetour> {
        if !self
            .network_interfaces
            .iter()
            .any(|configured_iface_id| configured_iface_id == iface_id)
        {
            return None;
        }

        Some(MmdsPacketDetour::new(
            self.mmds_state.clone(),
            self.mmds_ipv4_address,
            MmdsResponseQueue::default(),
        ))
    }

    fn covers_all_interfaces(&self, configs: &[NetworkInterfaceConfig]) -> bool {
        configs
            .iter()
            .all(|config| self.interface_is_configured(config.iface_id()))
    }

    fn interface_is_configured(&self, iface_id: &str) -> bool {
        self.network_interfaces
            .iter()
            .any(|configured_iface_id| configured_iface_id == iface_id)
    }
}

impl Arm64BootNetworkPacketIoProvider for ProcessNetworkPacketIoProvider {
    fn packet_io(
        &mut self,
        device: &Arm64BootNetworkDevice,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
        match self {
            Self::Noop(provider) => provider.packet_io(device),
            Self::MmdsOnly(provider) => provider.packet_io(device),
            Self::Vmnet(provider) => provider.packet_io(device),
        }
    }
}

#[derive(Debug)]
enum ProcessNetworkPacketIoProviderBuildError {
    NetworkInterfaceCount {
        source: NetworkInterfaceConfigError,
    },
    MmdsState {
        source: MmdsStateLockError,
    },
    HostDeviceName {
        iface_id: String,
        source: VmnetHostDeviceNameConfigError,
    },
    Start {
        iface_id: String,
        source: VmnetInterfaceStartError,
    },
    PacketIoBuild {
        iface_id: String,
        source: VmnetVirtioNetworkPacketIoBuildError,
    },
    MmdsOnlyPacketIoBuild {
        iface_id: String,
        source: MmdsOnlyVirtioNetworkPacketIoBuildError,
    },
    MissingMmdsDetour {
        iface_id: String,
    },
    MmdsOnlyProviderBuild {
        source: MmdsOnlyVirtioNetworkPacketIoProviderBuildError,
    },
    ProviderBuild {
        source: VmnetVirtioNetworkPacketIoProviderBuildError,
    },
}

impl fmt::Display for ProcessNetworkPacketIoProviderBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkInterfaceCount { source } => {
                write!(f, "unsupported network interface count: {source}")
            }
            Self::MmdsState { source } => {
                write!(f, "failed to access MMDS state: {source}")
            }
            Self::HostDeviceName { iface_id, source } => {
                write!(
                    f,
                    "network interface {iface_id} has unsupported vmnet host device config: {source}"
                )
            }
            Self::Start { iface_id, source } => {
                write!(
                    f,
                    "failed to start vmnet packet I/O for interface {iface_id}: {source}"
                )
            }
            Self::PacketIoBuild { iface_id, source } => {
                write!(
                    f,
                    "failed to build vmnet packet I/O for interface {iface_id}: {source}"
                )
            }
            Self::MmdsOnlyPacketIoBuild { iface_id, source } => {
                write!(
                    f,
                    "failed to build MMDS-only packet I/O for interface {iface_id}: {source}"
                )
            }
            Self::MissingMmdsDetour { iface_id } => {
                write!(f, "missing MMDS packet detour for interface {iface_id}")
            }
            Self::MmdsOnlyProviderBuild { source } => {
                write!(f, "failed to build MMDS-only packet I/O provider: {source}")
            }
            Self::ProviderBuild { source } => {
                write!(f, "failed to build vmnet packet I/O provider: {source}")
            }
        }
    }
}

impl std::error::Error for ProcessNetworkPacketIoProviderBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NetworkInterfaceCount { source } => Some(source),
            Self::MmdsState { source } => Some(source),
            Self::HostDeviceName { source, .. } => Some(source),
            Self::Start { source, .. } => Some(source),
            Self::PacketIoBuild { source, .. } => Some(source),
            Self::MmdsOnlyPacketIoBuild { source, .. } => Some(source),
            Self::MissingMmdsDetour { .. } => None,
            Self::MmdsOnlyProviderBuild { source } => Some(source),
            Self::ProviderBuild { source } => Some(source),
        }
    }
}

trait ProcessVmnetPacketIoBackendFactory {
    type Backend: VmnetInterfaceBackend;

    fn new_backend(&mut self, iface_id: &str) -> Self::Backend;
}

#[derive(Debug, Default)]
struct SystemProcessVmnetPacketIoBackendFactory;

impl ProcessVmnetPacketIoBackendFactory for SystemProcessVmnetPacketIoBackendFactory {
    type Backend = SystemVmnetInterfaceBackend;

    fn new_backend(&mut self, _iface_id: &str) -> Self::Backend {
        SystemVmnetInterfaceBackend::new()
    }
}

#[cfg(test)]
fn process_vmnet_packet_io_provider_from_configs<F>(
    configs: &[NetworkInterfaceConfig],
    factory: &mut F,
) -> Result<
    VmnetVirtioNetworkPacketIoProvider<StartedVmnetPacketIoBackend<F::Backend>>,
    ProcessNetworkPacketIoProviderBuildError,
>
where
    F: ProcessVmnetPacketIoBackendFactory,
    F::Backend: VmnetPacketIoBackend<Interface = <F::Backend as VmnetInterfaceBackend>::Interface>,
{
    process_vmnet_packet_io_provider_from_configs_with_mmds_detour(configs, factory, None)
}

fn process_vmnet_packet_io_provider_from_configs_with_mmds_detour<F>(
    configs: &[NetworkInterfaceConfig],
    factory: &mut F,
    mmds_detour: Option<&ProcessMmdsPacketDetourConfig>,
) -> Result<
    VmnetVirtioNetworkPacketIoProvider<StartedVmnetPacketIoBackend<F::Backend>>,
    ProcessNetworkPacketIoProviderBuildError,
>
where
    F: ProcessVmnetPacketIoBackendFactory,
    F::Backend: VmnetPacketIoBackend<Interface = <F::Backend as VmnetInterfaceBackend>::Interface>,
{
    validate_network_interface_count(configs.len()).map_err(|source| {
        ProcessNetworkPacketIoProviderBuildError::NetworkInterfaceCount { source }
    })?;

    let mut entries = Vec::new();

    for config in configs {
        let iface_id = config.iface_id();
        let vmnet_config = VmnetInterfaceConfig::from_host_dev_name(config.host_dev_name())
            .map_err(
                |source| ProcessNetworkPacketIoProviderBuildError::HostDeviceName {
                    iface_id: iface_id.to_string(),
                    source,
                },
            )?;
        let backend = factory.new_backend(iface_id);
        let (backend, interface) = StartedVmnetPacketIoBackend::start(backend, &vmnet_config)
            .map_err(|source| ProcessNetworkPacketIoProviderBuildError::Start {
                iface_id: iface_id.to_string(),
                source,
            })?;
        let detour = mmds_detour.and_then(|detour| detour.detour_for_interface(iface_id));
        let packet_io = match detour {
            Some(detour) => {
                VmnetVirtioNetworkPacketIo::with_mmds_detour(backend, interface, detour)
            }
            None => VmnetVirtioNetworkPacketIo::new(backend, interface),
        }
        .map_err(
            |source| ProcessNetworkPacketIoProviderBuildError::PacketIoBuild {
                iface_id: iface_id.to_string(),
                source,
            },
        )?;

        entries.push(VmnetVirtioNetworkPacketIoProviderEntry::new(
            iface_id, packet_io,
        ));
    }

    VmnetVirtioNetworkPacketIoProvider::new(entries)
        .map_err(|source| ProcessNetworkPacketIoProviderBuildError::ProviderBuild { source })
}

fn process_mmds_only_packet_io_provider_from_configs(
    configs: &[NetworkInterfaceConfig],
    mmds_detour: &ProcessMmdsPacketDetourConfig,
) -> Result<MmdsOnlyVirtioNetworkPacketIoProvider, ProcessNetworkPacketIoProviderBuildError> {
    let mut entries = Vec::new();

    for config in configs {
        let iface_id = config.iface_id();
        VmnetInterfaceConfig::from_host_dev_name(config.host_dev_name()).map_err(|source| {
            ProcessNetworkPacketIoProviderBuildError::HostDeviceName {
                iface_id: iface_id.to_string(),
                source,
            }
        })?;
        let Some(detour) = mmds_detour.detour_for_interface(iface_id) else {
            return Err(
                ProcessNetworkPacketIoProviderBuildError::MissingMmdsDetour {
                    iface_id: iface_id.to_string(),
                },
            );
        };
        let packet_io = MmdsOnlyVirtioNetworkPacketIo::new(detour).map_err(|source| {
            ProcessNetworkPacketIoProviderBuildError::MmdsOnlyPacketIoBuild {
                iface_id: iface_id.to_string(),
                source,
            }
        })?;
        entries.push(MmdsOnlyVirtioNetworkPacketIoProviderEntry::new(
            iface_id, packet_io,
        ));
    }

    MmdsOnlyVirtioNetworkPacketIoProvider::new(entries).map_err(|source| {
        ProcessNetworkPacketIoProviderBuildError::MmdsOnlyProviderBuild { source }
    })
}

#[derive(Debug, Default)]
struct NoopProcessNetworkTxPacketSink;

impl VirtioNetworkTxPacketSink for NoopProcessNetworkTxPacketSink {
    fn transmit_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
    ) -> Result<(), VirtioNetworkTxPacketSinkError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct EmptyProcessNetworkRxPacketSource;

impl VirtioNetworkRxPacketSource for EmptyProcessNetworkRxPacketSource {
    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        Ok(None)
    }

    fn consume_packet(&mut self) {}
}

pub(crate) trait NetworkPacketIoRunLoopSession: Send + 'static {
    type Control: BootRunLoopControl;
    type Error: fmt::Display + Send + 'static;
    type Outcome: Clone + fmt::Debug + Send + 'static;

    fn run_loop_control(&self) -> Self::Control;

    fn block_device_updater(&self) -> Option<BootRunLoopBlockDeviceUpdater> {
        None
    }

    fn balloon_device_updater(&self) -> Option<BootRunLoopBalloonDeviceUpdater> {
        None
    }

    fn shared_balloon_device_metrics(&self) -> Option<SharedBalloonDeviceMetrics> {
        None
    }

    fn shared_block_device_metrics(&self) -> Option<SharedBlockDeviceMetricsRegistry> {
        None
    }

    fn shared_pmem_device_metrics(&self) -> Option<SharedPmemDeviceMetricsRegistry> {
        None
    }

    fn shared_network_interface_metrics(&self) -> Option<SharedNetworkInterfaceMetricsRegistry> {
        None
    }

    fn shared_vsock_device_metrics(&self) -> Option<SharedVsockDeviceMetrics> {
        None
    }

    fn shared_entropy_device_metrics(&self) -> Option<SharedEntropyDeviceMetrics> {
        None
    }

    fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        None
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        Err(BalloonUpdateError::ActiveSessionUnavailable)
    }

    fn update_memory_hotplug(
        &mut self,
        _update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        Err(MemoryHotplugUpdateError::ActiveSessionUnavailable)
    }

    fn memory_hotplug_status(
        &mut self,
        _config: MemoryHotplugConfig,
        _requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        Err(MemoryHotplugStatusError::ActiveSessionUnavailable)
    }

    fn run_loop_with_network_packet_io<P>(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
        packet_io: &mut P,
    ) -> Result<Self::Outcome, Self::Error>
    where
        P: Arm64BootNetworkPacketIoProvider;

    fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool;
}

impl NetworkPacketIoRunLoopSession for OwnedHvfArm64BootSession {
    type Control = HvfArm64BootRunLoopControl;
    type Error = HvfArm64BootRunLoopError;
    type Outcome = HvfArm64BootRunLoopOutcome;

    fn run_loop_control(&self) -> Self::Control {
        OwnedHvfArm64BootSession::run_loop_control(self)
    }

    fn block_device_updater(&self) -> Option<BootRunLoopBlockDeviceUpdater> {
        Some(BootRunLoopBlockDeviceUpdater::new(
            self.runtime_resources().block_devices.clone(),
            self.mmio_dispatcher(),
        ))
    }

    fn balloon_device_updater(&self) -> Option<BootRunLoopBalloonDeviceUpdater> {
        self.runtime_resources()
            .balloon_device
            .clone()
            .map(|device| BootRunLoopBalloonDeviceUpdater::new(device, self.mmio_dispatcher()))
    }

    fn shared_balloon_device_metrics(&self) -> Option<SharedBalloonDeviceMetrics> {
        Some(OwnedHvfArm64BootSession::shared_balloon_device_metrics(
            self,
        ))
    }

    fn shared_block_device_metrics(&self) -> Option<SharedBlockDeviceMetricsRegistry> {
        Some(OwnedHvfArm64BootSession::shared_block_device_metrics(self))
    }

    fn shared_pmem_device_metrics(&self) -> Option<SharedPmemDeviceMetricsRegistry> {
        Some(OwnedHvfArm64BootSession::shared_pmem_device_metrics(self))
    }

    fn shared_network_interface_metrics(&self) -> Option<SharedNetworkInterfaceMetricsRegistry> {
        Some(OwnedHvfArm64BootSession::shared_network_interface_metrics(
            self,
        ))
    }

    fn shared_vsock_device_metrics(&self) -> Option<SharedVsockDeviceMetrics> {
        Some(OwnedHvfArm64BootSession::shared_vsock_device_metrics(self))
    }

    fn shared_entropy_device_metrics(&self) -> Option<SharedEntropyDeviceMetrics> {
        Some(OwnedHvfArm64BootSession::shared_entropy_device_metrics(
            self,
        ))
    }

    fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        OwnedHvfArm64BootSession::shared_rtc_device_metrics(self)
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        OwnedHvfArm64BootSession::trigger_balloon_statistics_update_and_signal_interrupts(self)
    }

    fn update_memory_hotplug(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        OwnedHvfArm64BootSession::update_memory_hotplug_requested_size_and_signal_interrupt(
            self, update,
        )
    }

    fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        OwnedHvfArm64BootSession::memory_hotplug_status(self, config, requested_size_mib)
    }

    fn run_loop_with_network_packet_io<P>(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
        packet_io: &mut P,
    ) -> Result<Self::Outcome, Self::Error>
    where
        P: Arm64BootNetworkPacketIoProvider,
    {
        OwnedHvfArm64BootSession::run_loop_with_network_packet_io(
            self, stop_token, max_steps, packet_io,
        )
    }

    fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
        matches!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { .. }
                | HvfArm64BootRunLoopOutcome::Wakeup { .. }
        )
    }
}

pub(crate) trait BootRunLoopControl: Clone + fmt::Debug + Send + Sync + 'static {
    type Error: fmt::Display + Send + 'static;
    type StopToken: Clone + Send + Sync + 'static;

    fn stop_token(&self) -> Self::StopToken;

    fn request_stop(&self) -> Result<(), Self::Error>;

    fn request_wakeup(&self) -> Result<(), Self::Error>;
}

impl BootRunLoopControl for HvfArm64BootRunLoopControl {
    type Error = HvfVcpuRunnerError;
    type StopToken = HvfArm64BootRunLoopStopToken;

    fn stop_token(&self) -> Self::StopToken {
        HvfArm64BootRunLoopControl::stop_token(self)
    }

    fn request_stop(&self) -> Result<(), Self::Error> {
        HvfArm64BootRunLoopControl::request_stop(self)
    }

    fn request_wakeup(&self) -> Result<(), Self::Error> {
        HvfArm64BootRunLoopControl::request_wakeup(self)
    }
}

pub(crate) trait BootRunLoopSession: Send + 'static {
    type Control: BootRunLoopControl;
    type Error: fmt::Display + Send + 'static;
    type Outcome: Clone + fmt::Debug + Send + 'static;

    fn run_loop_control(&self) -> Self::Control;

    fn block_device_updater(&self) -> Option<BootRunLoopBlockDeviceUpdater> {
        None
    }

    fn balloon_device_updater(&self) -> Option<BootRunLoopBalloonDeviceUpdater> {
        None
    }

    fn shared_balloon_device_metrics(&self) -> Option<SharedBalloonDeviceMetrics> {
        None
    }

    fn shared_block_device_metrics(&self) -> Option<SharedBlockDeviceMetricsRegistry> {
        None
    }

    fn shared_pmem_device_metrics(&self) -> Option<SharedPmemDeviceMetricsRegistry> {
        None
    }

    fn shared_network_interface_metrics(&self) -> Option<SharedNetworkInterfaceMetricsRegistry> {
        None
    }

    fn shared_vsock_device_metrics(&self) -> Option<SharedVsockDeviceMetrics> {
        None
    }

    fn shared_entropy_device_metrics(&self) -> Option<SharedEntropyDeviceMetrics> {
        None
    }

    fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        None
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        Err(BalloonUpdateError::ActiveSessionUnavailable)
    }

    fn update_memory_hotplug(
        &mut self,
        _update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        Err(MemoryHotplugUpdateError::ActiveSessionUnavailable)
    }

    fn memory_hotplug_status(
        &mut self,
        _config: MemoryHotplugConfig,
        _requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        Err(MemoryHotplugStatusError::ActiveSessionUnavailable)
    }

    fn run_loop(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
    ) -> Result<Self::Outcome, Self::Error>;

    fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool;
}

impl BootRunLoopSession for OwnedHvfArm64BootSession {
    type Control = HvfArm64BootRunLoopControl;
    type Error = HvfArm64BootRunLoopError;
    type Outcome = HvfArm64BootRunLoopOutcome;

    fn run_loop_control(&self) -> Self::Control {
        OwnedHvfArm64BootSession::run_loop_control(self)
    }

    fn block_device_updater(&self) -> Option<BootRunLoopBlockDeviceUpdater> {
        Some(BootRunLoopBlockDeviceUpdater::new(
            self.runtime_resources().block_devices.clone(),
            self.mmio_dispatcher(),
        ))
    }

    fn balloon_device_updater(&self) -> Option<BootRunLoopBalloonDeviceUpdater> {
        self.runtime_resources()
            .balloon_device
            .clone()
            .map(|device| BootRunLoopBalloonDeviceUpdater::new(device, self.mmio_dispatcher()))
    }

    fn shared_balloon_device_metrics(&self) -> Option<SharedBalloonDeviceMetrics> {
        Some(OwnedHvfArm64BootSession::shared_balloon_device_metrics(
            self,
        ))
    }

    fn shared_block_device_metrics(&self) -> Option<SharedBlockDeviceMetricsRegistry> {
        Some(OwnedHvfArm64BootSession::shared_block_device_metrics(self))
    }

    fn shared_pmem_device_metrics(&self) -> Option<SharedPmemDeviceMetricsRegistry> {
        Some(OwnedHvfArm64BootSession::shared_pmem_device_metrics(self))
    }

    fn shared_network_interface_metrics(&self) -> Option<SharedNetworkInterfaceMetricsRegistry> {
        Some(OwnedHvfArm64BootSession::shared_network_interface_metrics(
            self,
        ))
    }

    fn shared_vsock_device_metrics(&self) -> Option<SharedVsockDeviceMetrics> {
        Some(OwnedHvfArm64BootSession::shared_vsock_device_metrics(self))
    }

    fn shared_entropy_device_metrics(&self) -> Option<SharedEntropyDeviceMetrics> {
        Some(OwnedHvfArm64BootSession::shared_entropy_device_metrics(
            self,
        ))
    }

    fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        OwnedHvfArm64BootSession::shared_rtc_device_metrics(self)
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        OwnedHvfArm64BootSession::trigger_balloon_statistics_update_and_signal_interrupts(self)
    }

    fn update_memory_hotplug(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        OwnedHvfArm64BootSession::update_memory_hotplug_requested_size_and_signal_interrupt(
            self, update,
        )
    }

    fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        OwnedHvfArm64BootSession::memory_hotplug_status(self, config, requested_size_mib)
    }

    fn run_loop(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
    ) -> Result<Self::Outcome, Self::Error> {
        OwnedHvfArm64BootSession::run_loop(self, stop_token, max_steps)
    }

    fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
        matches!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { .. }
                | HvfArm64BootRunLoopOutcome::Wakeup { .. }
        )
    }
}

impl<S, P> BootRunLoopSession for ProcessHvfBootSession<S, P>
where
    S: NetworkPacketIoRunLoopSession,
    P: Arm64BootNetworkPacketIoProvider + Send + 'static,
{
    type Control = S::Control;
    type Error = S::Error;
    type Outcome = S::Outcome;

    fn run_loop_control(&self) -> Self::Control {
        self.session.run_loop_control()
    }

    fn block_device_updater(&self) -> Option<BootRunLoopBlockDeviceUpdater> {
        self.session.block_device_updater()
    }

    fn balloon_device_updater(&self) -> Option<BootRunLoopBalloonDeviceUpdater> {
        self.session.balloon_device_updater()
    }

    fn shared_balloon_device_metrics(&self) -> Option<SharedBalloonDeviceMetrics> {
        self.session.shared_balloon_device_metrics()
    }

    fn shared_block_device_metrics(&self) -> Option<SharedBlockDeviceMetricsRegistry> {
        self.session.shared_block_device_metrics()
    }

    fn shared_pmem_device_metrics(&self) -> Option<SharedPmemDeviceMetricsRegistry> {
        self.session.shared_pmem_device_metrics()
    }

    fn shared_network_interface_metrics(&self) -> Option<SharedNetworkInterfaceMetricsRegistry> {
        self.session.shared_network_interface_metrics()
    }

    fn shared_vsock_device_metrics(&self) -> Option<SharedVsockDeviceMetrics> {
        self.session.shared_vsock_device_metrics()
    }

    fn shared_entropy_device_metrics(&self) -> Option<SharedEntropyDeviceMetrics> {
        self.session.shared_entropy_device_metrics()
    }

    fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        self.session.shared_rtc_device_metrics()
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        self.session.trigger_balloon_statistics_update()
    }

    fn update_memory_hotplug(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        self.session.update_memory_hotplug(update)
    }

    fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        self.session
            .memory_hotplug_status(config, requested_size_mib)
    }

    fn run_loop(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
    ) -> Result<Self::Outcome, Self::Error> {
        self.session
            .run_loop_with_network_packet_io(stop_token, max_steps, &mut self.packet_io)
    }

    fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
        S::should_continue_after_outcome(outcome)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BootRunLoopWorkerStatus<O> {
    Running,
    Paused,
    Exited(O),
    Failed(String),
}

trait BootRunLoopProcessExit {
    fn process_exit_status(&self) -> ProcessSessionExitStatus;
}

impl BootRunLoopProcessExit for HvfArm64BootRunLoopOutcome {
    fn process_exit_status(&self) -> ProcessSessionExitStatus {
        match self {
            Self::StepLimitReached { .. } | Self::Wakeup { .. } => {
                ProcessSessionExitStatus::Running
            }
            Self::GuestShutdown { .. } | Self::GuestReset { .. } => {
                ProcessSessionExitStatus::GuestRequestedStop
            }
            _ => ProcessSessionExitStatus::Terminal,
        }
    }
}

#[derive(Debug)]
struct BootRunLoopWorkerStatusCell<O> {
    status: Mutex<BootRunLoopWorkerStatus<O>>,
    #[cfg(test)]
    changed: Condvar,
}

impl<O> BootRunLoopWorkerStatusCell<O> {
    fn new() -> Self {
        Self {
            status: Mutex::new(BootRunLoopWorkerStatus::Running),
            #[cfg(test)]
            changed: Condvar::new(),
        }
    }

    fn snapshot(&self) -> BootRunLoopWorkerStatus<O>
    where
        O: Clone,
    {
        self.lock_status().clone()
    }

    fn record(&self, status: BootRunLoopWorkerStatus<O>) {
        {
            let mut current = self.lock_status();
            *current = status;
        }
        #[cfg(test)]
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn wait_for_terminal_status(&self) -> BootRunLoopWorkerStatus<O>
    where
        O: Clone,
    {
        let mut current = self.lock_status();
        while matches!(
            &*current,
            BootRunLoopWorkerStatus::Running | BootRunLoopWorkerStatus::Paused
        ) {
            current = match self.changed.wait(current) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
        current.clone()
    }

    fn lock_status(&self) -> MutexGuard<'_, BootRunLoopWorkerStatus<O>> {
        match self.status.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl<O> Default for BootRunLoopWorkerStatusCell<O> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct BootRunLoopPauseGate {
    state: Mutex<BootRunLoopPauseState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct BootRunLoopPauseState {
    paused: bool,
    shutdown: bool,
    command_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootRunLoopPauseWait {
    Running,
    Paused,
    Shutdown,
}

impl BootRunLoopPauseGate {
    fn pause(&self) {
        {
            let mut state = self.lock_state();
            if !state.shutdown {
                state.paused = true;
            }
        }
        self.changed.notify_all();
    }

    fn resume(&self) {
        {
            let mut state = self.lock_state();
            state.paused = false;
        }
        self.changed.notify_all();
    }

    fn shutdown(&self) {
        {
            let mut state = self.lock_state();
            state.shutdown = true;
            state.paused = false;
        }
        self.changed.notify_all();
    }

    fn notify_command_available(&self) {
        {
            let mut state = self.lock_state();
            state.command_generation = state.command_generation.wrapping_add(1);
        }
        self.changed.notify_all();
    }

    fn command_generation(&self) -> u64 {
        self.lock_state().command_generation
    }

    fn wait_once_if_paused(&self, observed_command_generation: &mut u64) -> BootRunLoopPauseWait {
        let mut state = self.lock_state();
        loop {
            if state.shutdown {
                return BootRunLoopPauseWait::Shutdown;
            }
            if !state.paused {
                return BootRunLoopPauseWait::Running;
            }
            if state.command_generation != *observed_command_generation {
                *observed_command_generation = state.command_generation;
                return BootRunLoopPauseWait::Paused;
            }

            state = match self.changed.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, BootRunLoopPauseState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

type BootRunLoopCommand<S> = Box<dyn FnOnce(&mut S) + Send + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BootRunLoopCommandError<C, E> {
    WorkerNotRunning,
    QueueFull,
    QueueClosed,
    Wakeup { source: C },
    ResponseClosed,
    Command { source: E },
}

impl<C, E> fmt::Display for BootRunLoopCommandError<C, E>
where
    C: fmt::Display,
    E: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerNotRunning => f.write_str("boot run loop worker is not running"),
            Self::QueueFull => f.write_str("boot run loop command queue is full"),
            Self::QueueClosed => f.write_str("boot run loop command queue is closed"),
            Self::Wakeup { source } => {
                write!(f, "failed to wake boot run loop for command: {source}")
            }
            Self::ResponseClosed => f.write_str("boot run loop command response closed"),
            Self::Command { source } => write!(f, "boot run loop command failed: {source}"),
        }
    }
}

fn drive_update_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, DriveUpdateError>,
) -> DriveUpdateError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => DriveUpdateError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn balloon_update_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, BalloonUpdateError>,
) -> BalloonUpdateError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => BalloonUpdateError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn memory_hotplug_update_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, MemoryHotplugUpdateError>,
) -> MemoryHotplugUpdateError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => MemoryHotplugUpdateError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn memory_hotplug_status_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, MemoryHotplugStatusError>,
) -> MemoryHotplugStatusError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => MemoryHotplugStatusError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn balloon_stats_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, BalloonStatsError>,
) -> BalloonStatsError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => BalloonStatsError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn balloon_hinting_status_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, BalloonHintingStatusError>,
) -> BalloonHintingStatusError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => BalloonHintingStatusError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn balloon_hinting_command_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, BalloonHintingCommandError>,
) -> BalloonHintingCommandError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        other => BalloonHintingCommandError::ActiveSessionCommand {
            message: other.to_string(),
        },
    }
}

fn lifecycle_error_from_boot_run_loop_command<C>(
    err: BootRunLoopCommandError<C, BackendError>,
) -> BackendError
where
    C: fmt::Display,
{
    match err {
        BootRunLoopCommandError::Command { source } => source,
        BootRunLoopCommandError::WorkerNotRunning
        | BootRunLoopCommandError::QueueClosed
        | BootRunLoopCommandError::ResponseClosed => {
            BackendError::InvalidState("boot run loop worker is not running")
        }
        BootRunLoopCommandError::QueueFull => {
            BackendError::Hypervisor("boot run loop command queue is full".to_string())
        }
        BootRunLoopCommandError::Wakeup { source } => BackendError::Hypervisor(format!(
            "failed to wake boot run loop for lifecycle command: {source}"
        )),
    }
}

pub(crate) struct BootRunLoopCommandHandle<S>
where
    S: BootRunLoopSession,
{
    sender: mpsc::SyncSender<BootRunLoopCommand<S>>,
    control: S::Control,
    status: Arc<BootRunLoopWorkerStatusCell<S::Outcome>>,
    pause_gate: Arc<BootRunLoopPauseGate>,
}

impl<S> Clone for BootRunLoopCommandHandle<S>
where
    S: BootRunLoopSession,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            control: self.control.clone(),
            status: Arc::clone(&self.status),
            pause_gate: Arc::clone(&self.pause_gate),
        }
    }
}

impl<S> fmt::Debug for BootRunLoopCommandHandle<S>
where
    S: BootRunLoopSession,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootRunLoopCommandHandle")
            .field("control", &self.control)
            .field("status", &self.status.snapshot())
            .finish_non_exhaustive()
    }
}

impl<S> BootRunLoopCommandHandle<S>
where
    S: BootRunLoopSession,
{
    fn new(
        sender: mpsc::SyncSender<BootRunLoopCommand<S>>,
        control: S::Control,
        status: Arc<BootRunLoopWorkerStatusCell<S::Outcome>>,
        pause_gate: Arc<BootRunLoopPauseGate>,
    ) -> Self {
        Self {
            sender,
            control,
            status,
            pause_gate,
        }
    }

    fn run<R, E>(
        &self,
        command: impl FnOnce(&mut S) -> Result<R, E> + Send + 'static,
    ) -> Result<R, BootRunLoopCommandError<<S::Control as BootRunLoopControl>::Error, E>>
    where
        R: Send + 'static,
        E: Send + 'static,
    {
        if !matches!(
            self.status.snapshot(),
            BootRunLoopWorkerStatus::Running | BootRunLoopWorkerStatus::Paused
        ) {
            return Err(BootRunLoopCommandError::WorkerNotRunning);
        }

        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        let queued_command: BootRunLoopCommand<S> = Box::new(move |session| {
            let _ = response_sender.send(command(session));
        });

        match self.sender.try_send(queued_command) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => return Err(BootRunLoopCommandError::QueueFull),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(BootRunLoopCommandError::QueueClosed);
            }
        }
        self.pause_gate.notify_command_available();

        match response_receiver.try_recv() {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(source)) => return Err(BootRunLoopCommandError::Command { source }),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(BootRunLoopCommandError::ResponseClosed);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        let wakeup_result = self.control.request_wakeup();
        match response_receiver.recv() {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(source)) => Err(BootRunLoopCommandError::Command { source }),
            Err(_) => match wakeup_result {
                Ok(()) => Err(BootRunLoopCommandError::ResponseClosed),
                Err(source) => Err(BootRunLoopCommandError::Wakeup { source }),
            },
        }
    }
}

fn drain_boot_run_loop_commands<S>(
    session: &mut S,
    command_receiver: &mpsc::Receiver<BootRunLoopCommand<S>>,
    command_limit: usize,
) where
    S: BootRunLoopSession,
{
    for _ in 0..command_limit {
        let Ok(command) = command_receiver.try_recv() else {
            break;
        };
        command(session);
    }
}

pub(crate) struct BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    control: S::Control,
    block_device_updater: Option<BootRunLoopBlockDeviceUpdater>,
    balloon_device_updater: Option<BootRunLoopBalloonDeviceUpdater>,
    block_device_metrics: Option<SharedBlockDeviceMetricsRegistry>,
    pmem_device_metrics: Option<SharedPmemDeviceMetricsRegistry>,
    balloon_device_metrics: Option<SharedBalloonDeviceMetrics>,
    network_interface_metrics: Option<SharedNetworkInterfaceMetricsRegistry>,
    vsock_device_metrics: Option<SharedVsockDeviceMetrics>,
    entropy_device_metrics: Option<SharedEntropyDeviceMetrics>,
    rtc_device_metrics: Option<SharedRtcDeviceMetrics>,
    command_handle: BootRunLoopCommandHandle<S>,
    status: Arc<BootRunLoopWorkerStatusCell<S::Outcome>>,
    pause_gate: Arc<BootRunLoopPauseGate>,
    terminal_wakeup_reader: UnixStream,
    session_release_sender: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl<S> BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    fn start(session: S, max_steps: NonZeroUsize) -> Result<Self, BackendError> {
        Self::start_with_command_queue_capacity(
            session,
            max_steps,
            BOOT_RUN_LOOP_COMMAND_QUEUE_CAPACITY,
        )
    }

    fn start_with_command_queue_capacity(
        mut session: S,
        max_steps: NonZeroUsize,
        command_queue_capacity: usize,
    ) -> Result<Self, BackendError> {
        let control = session.run_loop_control();
        let block_device_updater = session.block_device_updater();
        let balloon_device_updater = session.balloon_device_updater();
        let block_device_metrics = session.shared_block_device_metrics();
        let pmem_device_metrics = session.shared_pmem_device_metrics();
        let balloon_device_metrics = session.shared_balloon_device_metrics();
        let network_interface_metrics = session.shared_network_interface_metrics();
        let vsock_device_metrics = session.shared_vsock_device_metrics();
        let entropy_device_metrics = session.shared_entropy_device_metrics();
        let rtc_device_metrics = session.shared_rtc_device_metrics();
        let stop_token = control.stop_token();
        let status = Arc::new(BootRunLoopWorkerStatusCell::new());
        let worker_status = Arc::clone(&status);
        let pause_gate = Arc::new(BootRunLoopPauseGate::default());
        let worker_pause_gate = Arc::clone(&pause_gate);
        let (command_sender, command_receiver) = mpsc::sync_channel(command_queue_capacity);
        let command_handle = BootRunLoopCommandHandle::new(
            command_sender,
            control.clone(),
            Arc::clone(&status),
            Arc::clone(&pause_gate),
        );
        let (terminal_wakeup_reader, mut terminal_wakeup_writer) =
            UnixStream::pair().map_err(|err| {
                BackendError::Hypervisor(format!(
                    "failed to create HVF boot run loop wakeup stream: {err}"
                ))
            })?;
        terminal_wakeup_reader
            .set_nonblocking(true)
            .map_err(|err| {
                BackendError::Hypervisor(format!(
                    "failed to configure HVF boot run loop wakeup stream: {err}"
                ))
            })?;
        let (session_release_sender, session_release_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(HVF_BOOT_RUN_LOOP_THREAD_NAME.to_owned())
            .spawn(move || {
                let mut observed_command_generation = worker_pause_gate.command_generation();
                loop {
                    drain_boot_run_loop_commands(
                        &mut session,
                        &command_receiver,
                        command_queue_capacity,
                    );
                    loop {
                        match worker_pause_gate
                            .wait_once_if_paused(&mut observed_command_generation)
                        {
                            BootRunLoopPauseWait::Running => break,
                            BootRunLoopPauseWait::Paused => {
                                drain_boot_run_loop_commands(
                                    &mut session,
                                    &command_receiver,
                                    command_queue_capacity,
                                );
                            }
                            BootRunLoopPauseWait::Shutdown => {
                                drop(command_receiver);
                                let _ = session_release_receiver.recv();
                                return;
                            }
                        }
                    }
                    match session.run_loop(&stop_token, max_steps) {
                        Ok(outcome) if S::should_continue_after_outcome(&outcome) => continue,
                        Ok(outcome) => {
                            worker_status.record(BootRunLoopWorkerStatus::Exited(outcome.clone()));
                            let _ = terminal_wakeup_writer.write_all(&[1]);
                            break;
                        }
                        Err(err) => {
                            worker_status.record(BootRunLoopWorkerStatus::Failed(err.to_string()));
                            let _ = terminal_wakeup_writer.write_all(&[1]);
                            break;
                        }
                    }
                }
                drop(command_receiver);
                let _ = session_release_receiver.recv();
            })
            .map_err(|err| {
                BackendError::Hypervisor(format!("failed to spawn HVF boot run loop: {err}"))
            })?;

        Ok(Self {
            control,
            block_device_updater,
            balloon_device_updater,
            block_device_metrics,
            pmem_device_metrics,
            balloon_device_metrics,
            network_interface_metrics,
            vsock_device_metrics,
            entropy_device_metrics,
            rtc_device_metrics,
            command_handle,
            status,
            pause_gate,
            terminal_wakeup_reader,
            session_release_sender: Some(session_release_sender),
            worker: Some(worker),
        })
    }

    #[cfg(test)]
    fn command_handle(&self) -> BootRunLoopCommandHandle<S> {
        self.command_handle.clone()
    }

    pub(crate) fn run_command<R, E>(
        &self,
        command: impl FnOnce(&mut S) -> Result<R, E> + Send + 'static,
    ) -> Result<R, BootRunLoopCommandError<<S::Control as BootRunLoopControl>::Error, E>>
    where
        R: Send + 'static,
        E: Send + 'static,
    {
        self.command_handle.run(command)
    }

    fn status(&self) -> BootRunLoopWorkerStatus<S::Outcome> {
        self.status.snapshot()
    }

    #[cfg(test)]
    fn wait_for_terminal_status(&self) -> BootRunLoopWorkerStatus<S::Outcome> {
        self.status.wait_for_terminal_status()
    }

    fn metric_status(&self) -> BootRunLoopMetricStatus {
        let current = self.status.lock_status();
        match &*current {
            BootRunLoopWorkerStatus::Running => BootRunLoopMetricStatus::Running,
            BootRunLoopWorkerStatus::Paused => BootRunLoopMetricStatus::Paused,
            BootRunLoopWorkerStatus::Exited(_) => BootRunLoopMetricStatus::Exited,
            BootRunLoopWorkerStatus::Failed(_) => BootRunLoopMetricStatus::Failed,
        }
    }

    fn pause(&self) -> Result<(), BackendError> {
        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BackendError::InvalidState(
                "boot run loop worker is not running",
            ));
        }

        let status = Arc::clone(&self.status);
        let pause_gate = Arc::clone(&self.pause_gate);
        self.run_command(move |_| {
            status.record(BootRunLoopWorkerStatus::Paused);
            pause_gate.pause();
            Ok(())
        })
        .map_err(lifecycle_error_from_boot_run_loop_command)
    }

    fn resume(&self) -> Result<(), BackendError> {
        if !matches!(self.status(), BootRunLoopWorkerStatus::Paused) {
            return Err(BackendError::InvalidState(
                "boot run loop worker is not paused",
            ));
        }

        let status = Arc::clone(&self.status);
        let pause_gate = Arc::clone(&self.pause_gate);
        self.run_command(move |_| {
            status.record(BootRunLoopWorkerStatus::Running);
            pause_gate.resume();
            Ok(())
        })
        .map_err(lifecycle_error_from_boot_run_loop_command)
    }

    fn record_block_device_update(&self, drive_id: &str) {
        if let Some(metrics) = &self.block_device_metrics {
            metrics.record_update_for_drive(drive_id);
        }
    }

    fn record_block_device_update_failure(&self, drive_id: &str) {
        if let Some(metrics) = &self.block_device_metrics {
            metrics.record_update_failure_for_drive(drive_id);
        }
    }

    fn stop_and_join(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };

        let was_paused = matches!(self.status(), BootRunLoopWorkerStatus::Paused);
        let stop_requested = self.control.request_stop().is_ok();
        self.pause_gate.shutdown();
        drop(self.session_release_sender.take());

        // A stop error can mean an in-flight vCPU run was not canceled; avoid
        // turning cleanup into an unbounded join in that error path.
        if stop_requested || was_paused || worker.is_finished() {
            let _ = worker.join();
        }
    }
}

impl<S> ProcessSessionDiagnostics for BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
    S::Outcome: BootRunLoopProcessExit,
{
    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        let mut diagnostics =
            MetricsDiagnostics::new().with_boot_run_loop_status(self.metric_status());
        if let Some(metrics) = &self.block_device_metrics {
            diagnostics = diagnostics
                .with_block_device_metrics(metrics.aggregate_snapshot())
                .with_block_device_metrics_by_drive(metrics.per_drive_snapshot());
        }
        if let Some(metrics) = &self.pmem_device_metrics {
            diagnostics = diagnostics
                .with_pmem_device_metrics(metrics.aggregate_snapshot())
                .with_pmem_device_metrics_by_device(metrics.per_device_snapshot());
        }
        if let Some(metrics) = &self.network_interface_metrics {
            diagnostics = diagnostics
                .with_network_interface_metrics(metrics.aggregate_snapshot())
                .with_network_interface_metrics_by_interface(metrics.per_interface_snapshot());
        }
        if let Some(metrics) = &self.vsock_device_metrics {
            diagnostics = diagnostics.with_vsock_device_metrics(metrics.snapshot());
        }
        if let Some(metrics) = &self.entropy_device_metrics {
            diagnostics = diagnostics.with_entropy_device_metrics(metrics.snapshot());
        }
        if let Some(metrics) = &self.rtc_device_metrics {
            diagnostics = diagnostics.with_rtc_device_metrics(metrics.snapshot());
        }
        if let Some(metrics) = &self.balloon_device_metrics {
            diagnostics = diagnostics.with_balloon_device_metrics(metrics.snapshot());
        }
        diagnostics
    }

    fn pause(&mut self) -> Result<(), BackendError> {
        BootRunLoopSupervisor::pause(self)
    }

    fn resume(&mut self) -> Result<(), BackendError> {
        BootRunLoopSupervisor::resume(self)
    }

    fn update_block_device(
        &mut self,
        config: &DriveConfig,
        refresh_backing: bool,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), DriveUpdateError> {
        let drive_id = config.drive_id();
        let Some(updater) = self.block_device_updater.as_ref() else {
            self.record_block_device_update_failure(drive_id);
            return Err(DriveUpdateError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            self.record_block_device_update_failure(drive_id);
            return Err(DriveUpdateError::ActiveSessionUnavailable);
        }

        // Keep host file open/stat work on the caller side; only the active
        // handler mutation runs on the boot run-loop worker.
        let backing = if refresh_backing {
            match BootRunLoopBlockDeviceUpdater::open_block_device_backing(config) {
                Ok(backing) => Some(backing),
                Err(err) => {
                    self.record_block_device_update_failure(drive_id);
                    return Err(err);
                }
            }
        } else {
            None
        };
        let updater = updater.clone();
        let config = config.clone();

        let result = self
            .run_command(move |_| {
                updater.update_block_device_with_opened(&config, backing, rate_limiter_update)
            })
            .map_err(drive_update_error_from_boot_run_loop_command);
        if result.is_ok() {
            self.record_block_device_update(drive_id);
        } else {
            self.record_block_device_update_failure(drive_id);
        }
        result
    }

    fn update_balloon(&mut self, config: BalloonConfig) -> Result<(), BalloonUpdateError> {
        let Some(updater) = self.balloon_device_updater.as_ref() else {
            return Err(BalloonUpdateError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonUpdateError::ActiveSessionUnavailable);
        }

        let updater = updater.clone();

        self.run_command(move |_| updater.update_balloon_config(config))
            .map_err(balloon_update_error_from_boot_run_loop_command)
    }

    fn update_balloon_statistics(
        &mut self,
        input: BalloonStatsUpdateInput,
    ) -> Result<(), BalloonUpdateError> {
        let Some(updater) = self.balloon_device_updater.as_ref() else {
            return Err(BalloonUpdateError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonUpdateError::ActiveSessionUnavailable);
        }

        let updater = updater.clone();

        self.run_command(move |_| updater.update_balloon_statistics(input))
            .map_err(balloon_update_error_from_boot_run_loop_command)
    }

    fn update_memory_hotplug(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        if !matches!(
            self.status(),
            BootRunLoopWorkerStatus::Running | BootRunLoopWorkerStatus::Paused
        ) {
            return Err(MemoryHotplugUpdateError::ActiveSessionUnavailable);
        }

        self.run_command(move |session| session.update_memory_hotplug(update))
            .map_err(memory_hotplug_update_error_from_boot_run_loop_command)
    }

    fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        if !matches!(
            self.status(),
            BootRunLoopWorkerStatus::Running | BootRunLoopWorkerStatus::Paused
        ) {
            return Err(MemoryHotplugStatusError::ActiveSessionUnavailable);
        }

        self.run_command(move |session| session.memory_hotplug_status(config, requested_size_mib))
            .map_err(memory_hotplug_status_error_from_boot_run_loop_command)
    }

    fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
        if self.balloon_device_updater.is_none() {
            return Err(BalloonUpdateError::ActiveSessionUnavailable);
        }

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonUpdateError::ActiveSessionUnavailable);
        }

        self.run_command(BootRunLoopSession::trigger_balloon_statistics_update)
            .map_err(balloon_update_error_from_boot_run_loop_command)
    }

    fn balloon_stats(&mut self, config: BalloonConfig) -> Result<BalloonStats, BalloonStatsError> {
        let Some(updater) = self.balloon_device_updater.as_ref() else {
            return Err(BalloonStatsError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonStatsError::ActiveSessionUnavailable);
        }

        let updater = updater.clone();

        self.run_command(move |_| updater.balloon_stats(config))
            .map_err(balloon_stats_error_from_boot_run_loop_command)
    }

    fn balloon_hinting_status(
        &mut self,
    ) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
        let Some(updater) = self.balloon_device_updater.as_ref() else {
            return Err(BalloonHintingStatusError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonHintingStatusError::ActiveSessionUnavailable);
        }

        let updater = updater.clone();

        self.run_command(move |_| updater.balloon_hinting_status())
            .map_err(balloon_hinting_status_error_from_boot_run_loop_command)
    }

    fn start_balloon_hinting(
        &mut self,
        input: BalloonHintingStartInput,
    ) -> Result<(), BalloonHintingCommandError> {
        let Some(updater) = self.balloon_device_updater.as_ref() else {
            return Err(BalloonHintingCommandError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonHintingCommandError::ActiveSessionUnavailable);
        }

        let updater = updater.clone();

        self.run_command(move |_| updater.start_balloon_hinting(input))
            .map_err(balloon_hinting_command_error_from_boot_run_loop_command)
    }

    fn stop_balloon_hinting(&mut self) -> Result<(), BalloonHintingCommandError> {
        let Some(updater) = self.balloon_device_updater.as_ref() else {
            return Err(BalloonHintingCommandError::ActiveSessionUnavailable);
        };

        if !matches!(self.status(), BootRunLoopWorkerStatus::Running) {
            return Err(BalloonHintingCommandError::ActiveSessionUnavailable);
        }

        let updater = updater.clone();

        self.run_command(move |_| updater.stop_balloon_hinting())
            .map_err(balloon_hinting_command_error_from_boot_run_loop_command)
    }

    fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
        Some(self.terminal_wakeup_reader.as_raw_fd())
    }

    fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
        let mut buffer = [0; 64];

        loop {
            match self.terminal_wakeup_reader.read(&mut buffer) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof),
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.kind()),
            }
        }
    }

    fn process_exit_status(&self) -> ProcessSessionExitStatus {
        let current = self.status.lock_status();
        match &*current {
            BootRunLoopWorkerStatus::Running => ProcessSessionExitStatus::Running,
            BootRunLoopWorkerStatus::Paused => ProcessSessionExitStatus::Running,
            BootRunLoopWorkerStatus::Exited(outcome) => outcome.process_exit_status(),
            BootRunLoopWorkerStatus::Failed(_) => ProcessSessionExitStatus::Terminal,
        }
    }
}

impl<S> Drop for BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

impl<S> fmt::Debug for BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootRunLoopSupervisor")
            .field("control", &self.control)
            .field("command_handle", &self.command_handle)
            .field("status", &self.status())
            .field(
                "has_balloon_device_updater",
                &self.balloon_device_updater.is_some(),
            )
            .field("worker_active", &self.worker.is_some())
            .finish()
    }
}

fn default_hvf_boot_run_loop_step_limit() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT).unwrap_or(NonZeroUsize::MIN)
}

fn default_hvf_boot_session_config(serial_output: SharedSerialOutput) -> HvfArm64BootSessionConfig {
    HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(DEFAULT_BLOCK_MMIO_BASE, DEFAULT_BLOCK_MMIO_REGION_ID),
        PmemMmioLayout::new(DEFAULT_PMEM_MMIO_BASE, DEFAULT_PMEM_MMIO_REGION_ID),
        NetworkMmioLayout::new(DEFAULT_NETWORK_MMIO_BASE, DEFAULT_NETWORK_MMIO_REGION_ID),
        VsockMmioLayout::new(DEFAULT_VSOCK_MMIO_BASE, DEFAULT_VSOCK_MMIO_REGION_ID),
        RtcMmioLayout::new(DEFAULT_RTC_MMIO_BASE, DEFAULT_RTC_MMIO_REGION_ID),
    )
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        DEFAULT_SERIAL_MMIO_REGION_ID,
        DEFAULT_SERIAL_MMIO_BASE,
        serial_output,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fmt;
    use std::fs::{self, remove_file};
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Duration;

    use bangbang_runtime::balloon::{
        BalloonConfig, BalloonConfigInput, BalloonHintingCommandError, BalloonHintingStartInput,
        BalloonHintingStatus, BalloonHintingStatusError, BalloonStats, BalloonStatsError,
        BalloonStatsUpdateInput, BalloonUpdateError, BalloonUpdateInput,
    };
    use bangbang_runtime::block::{
        BlockMmioLayout, DriveConfig, DriveConfigInput, DriveConfigs, DriveRateLimiterConfig,
        DriveTokenBucketConfig, DriveUpdateError, DriveUpdateInput, PreparedBlockDevices,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::cpu::CpuConfigInput;
    use bangbang_runtime::entropy::EntropyConfigInput;
    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::logger::LoggerConfigInput;
    use bangbang_runtime::machine::{MachineConfigInput, MachineConfigPatchInput};
    use bangbang_runtime::memory_hotplug::{
        MemoryHotplugConfig, MemoryHotplugConfigInput, MemoryHotplugSizeUpdate,
        MemoryHotplugSizeUpdateInput, MemoryHotplugStatus, MemoryHotplugStatusError,
        MemoryHotplugUpdateError,
    };
    use bangbang_runtime::metrics::{
        BalloonDeviceMetrics, BlockDeviceMetrics, BlockDeviceMetricsByDrive,
        BootRunLoopMetricStatus, EntropyDeviceMetrics, MetricsConfigInput, MetricsDiagnostics,
        NetworkInterfaceMetrics, NetworkInterfaceMetricsByInterface, PmemDeviceMetrics,
        PmemDeviceMetricsByDevice, RtcDeviceMetrics, SharedBalloonDeviceMetrics,
        SharedBlockDeviceMetricsRegistry, SharedEntropyDeviceMetrics,
        SharedNetworkInterfaceMetricsRegistry, SharedPmemDeviceMetricsRegistry,
        SharedRtcDeviceMetrics, SharedSignalMetrics, SharedVsockDeviceMetrics, VsockDeviceMetrics,
    };
    use bangbang_runtime::mmds::{MmdsConfigInput, MmdsContentInput, MmdsStateHandle};
    use bangbang_runtime::mmio::MmioRegion;
    use bangbang_runtime::network::{
        MAX_NETWORK_INTERFACE_COUNT, NetworkInterfaceConfig, NetworkInterfaceConfigError,
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioLayout,
        PreparedNetworkDevices,
    };
    use bangbang_runtime::serial::{
        SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialConfigInput, SerialOutput, SerialOutputMetrics,
        SerialRateLimiterConfig, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::startup::{
        Arm64BootBlockDevice, Arm64BootNetworkDevice, Arm64BootNetworkPacketIo,
        Arm64BootNetworkPacketIoError, Arm64BootNetworkPacketIoProvider,
    };
    use bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
    use bangbang_runtime::vsock::VsockConfigInput;
    use bangbang_runtime::{BackendError, InstanceState, VmmAction, VmmActionError, VmmController};

    use crate::host_network::vmnet::{
        VmnetError, VmnetInterfaceBackend, VmnetInterfaceConfig, VmnetInterfaceDescriptor,
        VmnetInterfaceDescriptorError, VmnetOperation, VmnetPacketIoBackend, VmnetPacketIoError,
        VmnetReadPacket, VmnetStatus, VmnetWritePacket,
    };

    use super::{
        BootRunLoopBlockDeviceUpdater, BootRunLoopControl, BootRunLoopSession,
        BootRunLoopSupervisor, BootRunLoopWorkerStatus, DEFAULT_BALLOON_MMIO_BASE,
        DEFAULT_BALLOON_MMIO_REGION_ID, DEFAULT_BLOCK_MMIO_BASE, DEFAULT_BLOCK_MMIO_REGION_ID,
        DEFAULT_BOOT_TIMER_MMIO_BASE, DEFAULT_BOOT_TIMER_MMIO_REGION_ID, DEFAULT_ENTROPY_MMIO_BASE,
        DEFAULT_ENTROPY_MMIO_REGION_ID, DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT,
        DEFAULT_MEMORY_HOTPLUG_MMIO_BASE, DEFAULT_MEMORY_HOTPLUG_MMIO_REGION_ID,
        DEFAULT_NETWORK_MMIO_BASE, DEFAULT_NETWORK_MMIO_REGION_ID, DEFAULT_PMEM_MMIO_BASE,
        DEFAULT_PMEM_MMIO_REGION_ID, DEFAULT_SERIAL_MMIO_BASE, DEFAULT_SERIAL_MMIO_REGION_ID,
        DEFAULT_VSOCK_MMIO_BASE, DEFAULT_VSOCK_MMIO_REGION_ID, EmptyProcessNetworkRxPacketSource,
        HvfInstanceStartExecutor, InstanceStartExecutor, NetworkPacketIoRunLoopSession,
        NoopProcessNetworkTxPacketSink, ProcessHvfBootSession, ProcessMmdsPacketDetourConfig,
        ProcessNetworkPacketIoProvider, ProcessNetworkPacketIoProviderBuildError,
        ProcessSessionDiagnostics, ProcessVmm, ProcessVmnetPacketIoBackendFactory,
        default_hvf_boot_run_loop_step_limit, default_hvf_boot_session_config,
        process_vmnet_packet_io_provider_from_configs,
    };

    static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TempFilePath {
        path: PathBuf,
    }

    impl TempFilePath {
        fn create(name: &str) -> Self {
            Self::create_with_bytes(name, b"")
        }

        fn create_with_bytes(name: &str, bytes: &[u8]) -> Self {
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("bb-vmm-{}-{id}-{name}", std::process::id()));
            fs::write(&path, bytes).expect("test backing file should be written");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn missing_temp_child_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("bb-vmm-missing-{}-{id}", std::process::id()))
            .join(name)
    }

    fn drive_config(drive_id: &str, path: &Path) -> DriveConfig {
        DriveConfigInput::new(drive_id, drive_id, path, false)
            .validate()
            .expect("drive config should validate")
    }

    const fn balloon_update_input(amount_mib: u32) -> BalloonUpdateInput {
        BalloonUpdateInput::new(amount_mib)
    }

    const fn balloon_stats_update_input(stats_polling_interval_s: u16) -> BalloonStatsUpdateInput {
        BalloonStatsUpdateInput::new(stats_polling_interval_s)
    }

    const fn memory_hotplug_config_input() -> MemoryHotplugConfigInput {
        MemoryHotplugConfigInput::new(1024, 2, 128)
    }

    fn memory_hotplug_config() -> MemoryHotplugConfig {
        MemoryHotplugConfig::try_from(memory_hotplug_config_input())
            .expect("test memory hotplug config should validate")
    }

    const fn memory_hotplug_size_update_input(
        requested_size_mib: u64,
    ) -> MemoryHotplugSizeUpdateInput {
        MemoryHotplugSizeUpdateInput::new(requested_size_mib)
    }

    fn memory_hotplug_status(requested_size_mib: u64) -> MemoryHotplugStatus {
        memory_hotplug_status_with_plugged(0, requested_size_mib)
    }

    fn memory_hotplug_status_with_plugged(
        plugged_size_mib: u64,
        requested_size_mib: u64,
    ) -> MemoryHotplugStatus {
        MemoryHotplugStatus::new(
            memory_hotplug_config(),
            plugged_size_mib,
            requested_size_mib,
        )
    }

    fn block_device_updater_fixture(
        drive_id: &str,
        backing_path: &Path,
    ) -> (BootRunLoopBlockDeviceUpdater, DriveConfig) {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                drive_id,
                drive_id,
                backing_path,
                false,
            ))
            .expect("drive config should insert");
        let config = configs
            .as_slice()
            .first()
            .expect("fixture drive should exist")
            .clone();
        let devices = PreparedBlockDevices::from_configs(&configs)
            .expect("prepared block devices should build")
            .register_mmio(BlockMmioLayout::new(
                DEFAULT_BLOCK_MMIO_BASE,
                DEFAULT_BLOCK_MMIO_REGION_ID,
            ))
            .expect("block MMIO devices should register");
        let (dispatcher, registrations) = devices.into_parts();
        let block_devices = registrations
            .into_iter()
            .map(|registration| {
                let range = registration.region().range();
                Arm64BootBlockDevice {
                    registration,
                    fdt_device: Arm64FdtVirtioMmioDevice {
                        region: Arm64FdtRegion {
                            base: range.start().raw_value(),
                            size: range.size(),
                        },
                        interrupt_line: GuestInterruptLine::new(32)
                            .expect("test interrupt line should validate"),
                    },
                }
            })
            .collect();
        let dispatcher = Arc::new(Mutex::new(dispatcher));
        let updater = BootRunLoopBlockDeviceUpdater::new(block_devices, Arc::clone(&dispatcher));

        (updater, config)
    }

    impl Drop for TempFilePath {
        fn drop(&mut self) {
            let _ = remove_file(&self.path);
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeSession {
        id: u64,
        pause_count: usize,
        pause_result: Option<BackendError>,
        resume_count: usize,
        resume_result: Option<BackendError>,
        block_update_count: usize,
        last_block_update: Option<String>,
        last_block_update_refresh_backing: Option<bool>,
        last_block_update_rate_limiter: Option<Option<DriveRateLimiterConfig>>,
        block_update_result: Option<DriveUpdateError>,
        balloon_update_count: usize,
        last_balloon_update_mib: Option<u32>,
        balloon_update_result: Option<BalloonUpdateError>,
        balloon_stats_update_count: usize,
        last_balloon_stats_update_interval_s: Option<u16>,
        balloon_stats_update_result: Option<BalloonUpdateError>,
        balloon_stats_trigger_count: usize,
        balloon_stats_trigger_result: Option<BalloonUpdateError>,
        balloon_stats_count: usize,
        last_balloon_stats_mib: Option<u32>,
        balloon_stats_result: Option<Result<BalloonStats, BalloonStatsError>>,
        balloon_hinting_status_count: usize,
        balloon_hinting_status_result:
            Option<Result<BalloonHintingStatus, BalloonHintingStatusError>>,
        balloon_hinting_start_count: usize,
        last_balloon_hinting_start_ack: Option<bool>,
        balloon_hinting_start_result: Option<Result<(), BalloonHintingCommandError>>,
        balloon_hinting_stop_count: usize,
        balloon_hinting_stop_result: Option<Result<(), BalloonHintingCommandError>>,
        memory_hotplug_update_count: usize,
        last_memory_hotplug_requested_size_mib: Option<u64>,
        memory_hotplug_update_result: Option<MemoryHotplugUpdateError>,
        memory_hotplug_status_count: usize,
        last_memory_hotplug_status_requested_size_mib: Option<u64>,
        memory_hotplug_status_result: Option<Result<MemoryHotplugStatus, MemoryHotplugStatusError>>,
    }

    impl FakeSession {
        const fn new(id: u64) -> Self {
            Self {
                id,
                pause_count: 0,
                pause_result: None,
                resume_count: 0,
                resume_result: None,
                block_update_count: 0,
                last_block_update: None,
                last_block_update_refresh_backing: None,
                last_block_update_rate_limiter: None,
                block_update_result: None,
                balloon_update_count: 0,
                last_balloon_update_mib: None,
                balloon_update_result: None,
                balloon_stats_update_count: 0,
                last_balloon_stats_update_interval_s: None,
                balloon_stats_update_result: None,
                balloon_stats_trigger_count: 0,
                balloon_stats_trigger_result: None,
                balloon_stats_count: 0,
                last_balloon_stats_mib: None,
                balloon_stats_result: None,
                balloon_hinting_status_count: 0,
                balloon_hinting_status_result: None,
                balloon_hinting_start_count: 0,
                last_balloon_hinting_start_ack: None,
                balloon_hinting_start_result: None,
                balloon_hinting_stop_count: 0,
                balloon_hinting_stop_result: None,
                memory_hotplug_update_count: 0,
                last_memory_hotplug_requested_size_mib: None,
                memory_hotplug_update_result: None,
                memory_hotplug_status_count: 0,
                last_memory_hotplug_status_requested_size_mib: None,
                memory_hotplug_status_result: None,
            }
        }

        fn with_block_update_result(id: u64, result: DriveUpdateError) -> Self {
            let mut session = Self::new(id);
            session.block_update_result = Some(result);
            session
        }

        fn with_pause_result(id: u64, result: BackendError) -> Self {
            let mut session = Self::new(id);
            session.pause_result = Some(result);
            session
        }

        fn with_resume_result(id: u64, result: BackendError) -> Self {
            let mut session = Self::new(id);
            session.resume_result = Some(result);
            session
        }

        fn with_balloon_update_result(id: u64, result: BalloonUpdateError) -> Self {
            let mut session = Self::new(id);
            session.balloon_update_result = Some(result);
            session
        }

        fn with_balloon_stats_update_result(id: u64, result: BalloonUpdateError) -> Self {
            let mut session = Self::new(id);
            session.balloon_stats_update_result = Some(result);
            session
        }

        fn with_balloon_stats_trigger_result(id: u64, result: BalloonUpdateError) -> Self {
            let mut session = Self::new(id);
            session.balloon_stats_trigger_result = Some(result);
            session
        }

        fn with_balloon_stats_result(
            id: u64,
            result: Result<BalloonStats, BalloonStatsError>,
        ) -> Self {
            let mut session = Self::new(id);
            session.balloon_stats_result = Some(result);
            session
        }

        fn with_balloon_hinting_status_result(
            id: u64,
            result: Result<BalloonHintingStatus, BalloonHintingStatusError>,
        ) -> Self {
            let mut session = Self::new(id);
            session.balloon_hinting_status_result = Some(result);
            session
        }

        fn with_balloon_hinting_start_result(
            id: u64,
            result: Result<(), BalloonHintingCommandError>,
        ) -> Self {
            let mut session = Self::new(id);
            session.balloon_hinting_start_result = Some(result);
            session
        }

        fn with_balloon_hinting_stop_result(
            id: u64,
            result: Result<(), BalloonHintingCommandError>,
        ) -> Self {
            let mut session = Self::new(id);
            session.balloon_hinting_stop_result = Some(result);
            session
        }

        fn with_memory_hotplug_update_result(id: u64, result: MemoryHotplugUpdateError) -> Self {
            let mut session = Self::new(id);
            session.memory_hotplug_update_result = Some(result);
            session
        }

        fn with_memory_hotplug_status_result(
            id: u64,
            result: Result<MemoryHotplugStatus, MemoryHotplugStatusError>,
        ) -> Self {
            let mut session = Self::new(id);
            session.memory_hotplug_status_result = Some(result);
            session
        }
    }

    impl ProcessSessionDiagnostics for FakeSession {
        fn pause(&mut self) -> Result<(), BackendError> {
            self.pause_count += 1;
            match self.pause_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn resume(&mut self) -> Result<(), BackendError> {
            self.resume_count += 1;
            match self.resume_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn update_block_device(
            &mut self,
            config: &DriveConfig,
            refresh_backing: bool,
            rate_limiter_update: Option<DriveRateLimiterConfig>,
        ) -> Result<(), DriveUpdateError> {
            self.block_update_count += 1;
            self.last_block_update = Some(config.drive_id().to_string());
            self.last_block_update_refresh_backing = Some(refresh_backing);
            self.last_block_update_rate_limiter = Some(rate_limiter_update);
            match self.block_update_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn update_balloon(
            &mut self,
            config: bangbang_runtime::balloon::BalloonConfig,
        ) -> Result<(), BalloonUpdateError> {
            self.balloon_update_count += 1;
            self.last_balloon_update_mib = Some(config.amount_mib());
            match self.balloon_update_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn update_balloon_statistics(
            &mut self,
            input: BalloonStatsUpdateInput,
        ) -> Result<(), BalloonUpdateError> {
            self.balloon_stats_update_count += 1;
            self.last_balloon_stats_update_interval_s = Some(input.stats_polling_interval_s());
            match self.balloon_stats_update_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn trigger_balloon_statistics_update(&mut self) -> Result<(), BalloonUpdateError> {
            self.balloon_stats_trigger_count += 1;
            match self.balloon_stats_trigger_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn balloon_stats(
            &mut self,
            config: bangbang_runtime::balloon::BalloonConfig,
        ) -> Result<BalloonStats, BalloonStatsError> {
            self.balloon_stats_count += 1;
            self.last_balloon_stats_mib = Some(config.amount_mib());
            match self.balloon_stats_result.clone() {
                Some(result) => result,
                None => BalloonStats::from_config_and_actual_pages(config, 0),
            }
        }

        fn balloon_hinting_status(
            &mut self,
        ) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
            self.balloon_hinting_status_count += 1;
            match self.balloon_hinting_status_result.clone() {
                Some(result) => result,
                None => Ok(BalloonHintingStatus::new(0, None)),
            }
        }

        fn start_balloon_hinting(
            &mut self,
            input: BalloonHintingStartInput,
        ) -> Result<(), BalloonHintingCommandError> {
            self.balloon_hinting_start_count += 1;
            self.last_balloon_hinting_start_ack = Some(input.acknowledge_on_stop());
            match self.balloon_hinting_start_result.clone() {
                Some(result) => result,
                None => Ok(()),
            }
        }

        fn stop_balloon_hinting(&mut self) -> Result<(), BalloonHintingCommandError> {
            self.balloon_hinting_stop_count += 1;
            match self.balloon_hinting_stop_result.clone() {
                Some(result) => result,
                None => Ok(()),
            }
        }

        fn update_memory_hotplug(
            &mut self,
            update: MemoryHotplugSizeUpdate,
        ) -> Result<(), MemoryHotplugUpdateError> {
            self.memory_hotplug_update_count += 1;
            self.last_memory_hotplug_requested_size_mib = Some(update.requested_size_mib());
            match self.memory_hotplug_update_result.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn memory_hotplug_status(
            &mut self,
            config: MemoryHotplugConfig,
            requested_size_mib: u64,
        ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
            self.memory_hotplug_status_count += 1;
            self.last_memory_hotplug_status_requested_size_mib = Some(requested_size_mib);
            match self.memory_hotplug_status_result.clone() {
                Some(result) => result,
                None => Ok(MemoryHotplugStatus::new(config, 0, requested_size_mib)),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DiagnosticSession {
        status: BootRunLoopMetricStatus,
    }

    impl ProcessSessionDiagnostics for DiagnosticSession {
        fn metrics_diagnostics(&self) -> bangbang_runtime::metrics::MetricsDiagnostics {
            bangbang_runtime::metrics::MetricsDiagnostics::new()
                .with_boot_run_loop_status(self.status)
        }
    }

    #[derive(Debug, Clone)]
    struct DiagnosticStarter {
        status: BootRunLoopMetricStatus,
        diagnostics: MetricsDiagnostics,
        calls: usize,
    }

    impl DiagnosticStarter {
        fn new(status: BootRunLoopMetricStatus) -> Self {
            Self {
                status,
                diagnostics: MetricsDiagnostics::new(),
                calls: 0,
            }
        }

        fn with_metrics_diagnostics(mut self, diagnostics: MetricsDiagnostics) -> Self {
            self.diagnostics = diagnostics;
            self
        }
    }

    impl InstanceStartExecutor for DiagnosticStarter {
        type Session = DiagnosticSession;

        fn start(
            &mut self,
            _controller: &bangbang_runtime::VmmController,
        ) -> Result<Self::Session, BackendError> {
            self.calls += 1;
            Ok(DiagnosticSession {
                status: self.status,
            })
        }

        fn metrics_diagnostics(&self) -> MetricsDiagnostics {
            self.diagnostics.clone()
        }
    }

    #[derive(Debug, Clone)]
    enum FakeStartResult {
        Success(Box<FakeSession>),
        Failure(BackendError),
    }

    #[derive(Debug, Clone)]
    struct FakeStarter {
        result: FakeStartResult,
        calls: usize,
    }

    impl FakeStarter {
        fn success(id: u64) -> Self {
            Self::success_with_session(FakeSession::new(id))
        }

        fn success_with_session(session: FakeSession) -> Self {
            Self {
                result: FakeStartResult::Success(Box::new(session)),
                calls: 0,
            }
        }

        const fn failure(source: BackendError) -> Self {
            Self {
                result: FakeStartResult::Failure(source),
                calls: 0,
            }
        }
    }

    impl InstanceStartExecutor for FakeStarter {
        type Session = FakeSession;

        fn start(
            &mut self,
            _controller: &bangbang_runtime::VmmController,
        ) -> Result<Self::Session, BackendError> {
            self.calls += 1;
            match &self.result {
                FakeStartResult::Success(session) => Ok((**session).clone()),
                FakeStartResult::Failure(source) => Err(source.clone()),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeRunLoopOutcome {
        StepLimitReached,
        Wakeup,
        Terminal,
    }

    impl super::BootRunLoopProcessExit for FakeRunLoopOutcome {
        fn process_exit_status(&self) -> super::ProcessSessionExitStatus {
            match self {
                Self::StepLimitReached | Self::Wakeup => super::ProcessSessionExitStatus::Running,
                Self::Terminal => super::ProcessSessionExitStatus::GuestRequestedStop,
            }
        }
    }

    #[test]
    fn process_exit_status_maps_to_process_decision() {
        assert_eq!(
            super::ProcessSessionExitStatus::Running.decision(),
            super::ProcessSessionExitDecision::Continue
        );
        assert_eq!(
            super::ProcessSessionExitStatus::GuestRequestedStop.decision(),
            super::ProcessSessionExitDecision::ExitSuccessfully
        );
        assert_eq!(
            super::ProcessSessionExitStatus::Terminal.decision(),
            super::ProcessSessionExitDecision::ExitWithFailure
        );
    }

    #[test]
    fn hvf_guest_power_outcomes_request_process_stop() {
        assert_eq!(
            super::BootRunLoopProcessExit::process_exit_status(
                &super::HvfArm64BootRunLoopOutcome::GuestShutdown { steps: 1 },
            ),
            super::ProcessSessionExitStatus::GuestRequestedStop
        );
        assert_eq!(
            super::BootRunLoopProcessExit::process_exit_status(
                &super::HvfArm64BootRunLoopOutcome::GuestReset { steps: 1 },
            ),
            super::ProcessSessionExitStatus::GuestRequestedStop
        );
        assert_eq!(
            super::BootRunLoopProcessExit::process_exit_status(
                &super::HvfArm64BootRunLoopOutcome::Unknown {
                    steps: 1,
                    reason: 1,
                },
            ),
            super::ProcessSessionExitStatus::Terminal
        );
    }

    #[test]
    fn hvf_resumable_run_loop_outcomes_keep_process_running() {
        assert_eq!(
            super::BootRunLoopProcessExit::process_exit_status(
                &super::HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 },
            ),
            super::ProcessSessionExitStatus::Running
        );
        assert_eq!(
            super::BootRunLoopProcessExit::process_exit_status(
                &super::HvfArm64BootRunLoopOutcome::Wakeup { steps: 1 },
            ),
            super::ProcessSessionExitStatus::Running
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeRunLoopError;

    impl fmt::Display for FakeRunLoopError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fake run loop failed")
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeRunLoopStopError;

    impl fmt::Display for FakeRunLoopStopError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fake run loop stop failed")
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeRunLoopCommandError;

    impl fmt::Display for FakeRunLoopCommandError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fake run loop command failed")
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeRunLoopStopToken {
        stopped: Arc<(Mutex<bool>, Condvar)>,
    }

    impl FakeRunLoopStopToken {
        fn request_stop(&self) {
            let (lock, condition) = &*self.stopped;
            let mut stopped = lock.lock().expect("stop flag should lock");
            *stopped = true;
            condition.notify_all();
        }

        fn wait_for_stop(&self) {
            let (lock, condition) = &*self.stopped;
            let mut stopped = lock.lock().expect("stop flag should lock");
            while !*stopped {
                stopped = condition
                    .wait(stopped)
                    .expect("stop flag should wait without poisoning");
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeRunLoopControl {
        stop_token: FakeRunLoopStopToken,
        request_stop_count: Arc<AtomicU64>,
        wakeup: Arc<(Mutex<u64>, Condvar)>,
        request_wakeup_count: Arc<AtomicU64>,
    }

    impl FakeRunLoopControl {
        fn request_stop_count(&self) -> u64 {
            self.request_stop_count.load(Ordering::SeqCst)
        }

        fn request_wakeup_count(&self) -> u64 {
            self.request_wakeup_count.load(Ordering::SeqCst)
        }

        fn wait_for_wakeup(&self) {
            let (lock, condition) = &*self.wakeup;
            let mut wakeup_count = lock.lock().expect("wakeup count should lock");
            while *wakeup_count == 0 {
                wakeup_count = condition
                    .wait(wakeup_count)
                    .expect("wakeup count should wait without poisoning");
            }
            *wakeup_count -= 1;
        }

        fn wait_for_request_wakeup_count(&self, expected_count: u64) {
            let (lock, condition) = &*self.wakeup;
            let mut wakeup_count = lock.lock().expect("wakeup count should lock");
            while self.request_wakeup_count() < expected_count {
                wakeup_count = condition
                    .wait(wakeup_count)
                    .expect("wakeup count should wait without poisoning");
            }
        }
    }

    impl BootRunLoopControl for FakeRunLoopControl {
        type Error = FakeRunLoopStopError;
        type StopToken = FakeRunLoopStopToken;

        fn stop_token(&self) -> Self::StopToken {
            self.stop_token.clone()
        }

        fn request_stop(&self) -> Result<(), Self::Error> {
            self.request_stop_count.fetch_add(1, Ordering::SeqCst);
            self.stop_token.request_stop();
            let (lock, condition) = &*self.wakeup;
            let mut wakeup_count = lock.lock().expect("wakeup count should lock");
            *wakeup_count += 1;
            condition.notify_all();
            Ok(())
        }

        fn request_wakeup(&self) -> Result<(), Self::Error> {
            self.request_wakeup_count.fetch_add(1, Ordering::SeqCst);
            let (lock, condition) = &*self.wakeup;
            let mut wakeup_count = lock.lock().expect("wakeup count should lock");
            *wakeup_count += 1;
            condition.notify_all();
            Ok(())
        }
    }

    struct FakeRunLoopSession {
        control: FakeRunLoopControl,
        drop_count: Arc<AtomicU64>,
        run_count: Arc<AtomicU64>,
        max_steps_sender: mpsc::Sender<usize>,
        outcomes: Arc<Mutex<VecDeque<Result<FakeRunLoopOutcome, FakeRunLoopError>>>>,
        block_device_updater: Option<BootRunLoopBlockDeviceUpdater>,
        block_device_metrics: Option<SharedBlockDeviceMetricsRegistry>,
        pmem_device_metrics: Option<SharedPmemDeviceMetricsRegistry>,
        balloon_device_metrics: Option<SharedBalloonDeviceMetrics>,
        network_interface_metrics: Option<SharedNetworkInterfaceMetricsRegistry>,
        vsock_device_metrics: Option<SharedVsockDeviceMetrics>,
        entropy_device_metrics: Option<SharedEntropyDeviceMetrics>,
        rtc_device_metrics: Option<SharedRtcDeviceMetrics>,
        memory_hotplug_updates: Arc<Mutex<Vec<u64>>>,
        memory_hotplug_status_requests: Arc<Mutex<Vec<u64>>>,
        memory_hotplug_status_plugged_size_mib: u64,
        wait_for_stop: bool,
        wait_for_wakeup: bool,
        wait_for_stop_sequence: Arc<Mutex<VecDeque<bool>>>,
    }

    impl FakeRunLoopSession {
        fn new(
            control: FakeRunLoopControl,
            drop_count: Arc<AtomicU64>,
            max_steps_sender: mpsc::Sender<usize>,
        ) -> Self {
            Self {
                control,
                drop_count,
                run_count: Arc::default(),
                max_steps_sender,
                outcomes: Arc::new(Mutex::new(VecDeque::from([Ok(
                    FakeRunLoopOutcome::Terminal,
                )]))),
                block_device_updater: None,
                block_device_metrics: None,
                pmem_device_metrics: None,
                balloon_device_metrics: None,
                network_interface_metrics: None,
                vsock_device_metrics: None,
                entropy_device_metrics: None,
                rtc_device_metrics: None,
                memory_hotplug_updates: Arc::default(),
                memory_hotplug_status_requests: Arc::default(),
                memory_hotplug_status_plugged_size_mib: 0,
                wait_for_stop: true,
                wait_for_wakeup: false,
                wait_for_stop_sequence: Arc::default(),
            }
        }

        fn run_count(&self) -> Arc<AtomicU64> {
            Arc::clone(&self.run_count)
        }

        fn memory_hotplug_updates(&self) -> Arc<Mutex<Vec<u64>>> {
            Arc::clone(&self.memory_hotplug_updates)
        }

        fn memory_hotplug_status_requests(&self) -> Arc<Mutex<Vec<u64>>> {
            Arc::clone(&self.memory_hotplug_status_requests)
        }

        const fn with_memory_hotplug_status_plugged_size_mib(
            mut self,
            plugged_size_mib: u64,
        ) -> Self {
            self.memory_hotplug_status_plugged_size_mib = plugged_size_mib;
            self
        }

        fn with_outcomes(
            mut self,
            outcomes: impl IntoIterator<Item = Result<FakeRunLoopOutcome, FakeRunLoopError>>,
        ) -> Self {
            self.outcomes = Arc::new(Mutex::new(outcomes.into_iter().collect()));
            self
        }

        fn with_block_device_updater(mut self, updater: BootRunLoopBlockDeviceUpdater) -> Self {
            self.block_device_updater = Some(updater);
            self
        }

        fn with_block_device_metrics(mut self, metrics: SharedBlockDeviceMetricsRegistry) -> Self {
            self.block_device_metrics = Some(metrics);
            self
        }

        fn with_pmem_device_metrics(mut self, metrics: SharedPmemDeviceMetricsRegistry) -> Self {
            self.pmem_device_metrics = Some(metrics);
            self
        }

        fn with_balloon_device_metrics(mut self, metrics: SharedBalloonDeviceMetrics) -> Self {
            self.balloon_device_metrics = Some(metrics);
            self
        }

        fn with_network_interface_metrics(
            mut self,
            metrics: SharedNetworkInterfaceMetricsRegistry,
        ) -> Self {
            self.network_interface_metrics = Some(metrics);
            self
        }

        fn with_vsock_device_metrics(mut self, metrics: SharedVsockDeviceMetrics) -> Self {
            self.vsock_device_metrics = Some(metrics);
            self
        }

        fn with_entropy_device_metrics(mut self, metrics: SharedEntropyDeviceMetrics) -> Self {
            self.entropy_device_metrics = Some(metrics);
            self
        }

        fn with_rtc_device_metrics(mut self, metrics: SharedRtcDeviceMetrics) -> Self {
            self.rtc_device_metrics = Some(metrics);
            self
        }

        const fn with_wait_for_stop(mut self, wait_for_stop: bool) -> Self {
            self.wait_for_stop = wait_for_stop;
            self
        }

        const fn with_wait_for_wakeup(mut self, wait_for_wakeup: bool) -> Self {
            self.wait_for_wakeup = wait_for_wakeup;
            self
        }

        fn with_wait_for_stop_sequence(
            mut self,
            wait_for_stop: impl IntoIterator<Item = bool>,
        ) -> Self {
            self.wait_for_stop_sequence = Arc::new(Mutex::new(wait_for_stop.into_iter().collect()));
            self
        }
    }

    impl Drop for FakeRunLoopSession {
        fn drop(&mut self) {
            self.drop_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl BootRunLoopSession for FakeRunLoopSession {
        type Control = FakeRunLoopControl;
        type Error = FakeRunLoopError;
        type Outcome = FakeRunLoopOutcome;

        fn run_loop_control(&self) -> Self::Control {
            self.control.clone()
        }

        fn block_device_updater(&self) -> Option<BootRunLoopBlockDeviceUpdater> {
            self.block_device_updater.clone()
        }

        fn shared_block_device_metrics(&self) -> Option<SharedBlockDeviceMetricsRegistry> {
            self.block_device_metrics.clone()
        }

        fn shared_pmem_device_metrics(&self) -> Option<SharedPmemDeviceMetricsRegistry> {
            self.pmem_device_metrics.clone()
        }

        fn shared_balloon_device_metrics(&self) -> Option<SharedBalloonDeviceMetrics> {
            self.balloon_device_metrics.clone()
        }

        fn shared_network_interface_metrics(
            &self,
        ) -> Option<SharedNetworkInterfaceMetricsRegistry> {
            self.network_interface_metrics.clone()
        }

        fn shared_vsock_device_metrics(&self) -> Option<SharedVsockDeviceMetrics> {
            self.vsock_device_metrics.clone()
        }

        fn shared_entropy_device_metrics(&self) -> Option<SharedEntropyDeviceMetrics> {
            self.entropy_device_metrics.clone()
        }

        fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
            self.rtc_device_metrics.clone()
        }

        fn update_memory_hotplug(
            &mut self,
            update: MemoryHotplugSizeUpdate,
        ) -> Result<(), MemoryHotplugUpdateError> {
            self.memory_hotplug_updates
                .lock()
                .expect("fake memory hotplug updates should lock")
                .push(update.requested_size_mib());
            Ok(())
        }

        fn memory_hotplug_status(
            &mut self,
            config: MemoryHotplugConfig,
            requested_size_mib: u64,
        ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
            self.memory_hotplug_status_requests
                .lock()
                .expect("fake memory hotplug status requests should lock")
                .push(requested_size_mib);
            Ok(MemoryHotplugStatus::new(
                config,
                self.memory_hotplug_status_plugged_size_mib,
                requested_size_mib,
            ))
        }

        fn run_loop(
            &mut self,
            stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
            max_steps: NonZeroUsize,
        ) -> Result<Self::Outcome, Self::Error> {
            self.run_count.fetch_add(1, Ordering::SeqCst);
            let _ = self.max_steps_sender.send(max_steps.get());
            let wait_for_stop = self
                .wait_for_stop_sequence
                .lock()
                .expect("fake wait sequence should lock")
                .pop_front()
                .unwrap_or(self.wait_for_stop);
            if wait_for_stop {
                stop_token.wait_for_stop();
            }
            if self.wait_for_wakeup {
                self.control.wait_for_wakeup();
            }
            self.outcomes
                .lock()
                .expect("fake outcomes should lock")
                .pop_front()
                .unwrap_or(Ok(FakeRunLoopOutcome::Terminal))
        }

        fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
            matches!(
                outcome,
                FakeRunLoopOutcome::StepLimitReached | FakeRunLoopOutcome::Wakeup
            )
        }
    }

    struct FakeNetworkPacketIoRunLoopSession {
        control: FakeRunLoopControl,
        max_steps_sender: mpsc::Sender<usize>,
    }

    impl FakeNetworkPacketIoRunLoopSession {
        const fn new(control: FakeRunLoopControl, max_steps_sender: mpsc::Sender<usize>) -> Self {
            Self {
                control,
                max_steps_sender,
            }
        }
    }

    impl NetworkPacketIoRunLoopSession for FakeNetworkPacketIoRunLoopSession {
        type Control = FakeRunLoopControl;
        type Error = FakeRunLoopError;
        type Outcome = FakeRunLoopOutcome;

        fn run_loop_control(&self) -> Self::Control {
            self.control.clone()
        }

        fn run_loop_with_network_packet_io<P>(
            &mut self,
            _stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
            max_steps: NonZeroUsize,
            packet_io: &mut P,
        ) -> Result<Self::Outcome, Self::Error>
        where
            P: Arm64BootNetworkPacketIoProvider,
        {
            let _ = self.max_steps_sender.send(max_steps.get());
            packet_io
                .packet_io(&test_boot_network_device())
                .map_err(|_| FakeRunLoopError)?;
            Ok(FakeRunLoopOutcome::Terminal)
        }

        fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
            matches!(
                outcome,
                FakeRunLoopOutcome::StepLimitReached | FakeRunLoopOutcome::Wakeup
            )
        }
    }

    #[derive(Debug, Default)]
    struct RecordingProcessNetworkPacketIoProvider {
        requested_ifaces: Arc<Mutex<Vec<String>>>,
        tx_sink: NoopProcessNetworkTxPacketSink,
        rx_source: EmptyProcessNetworkRxPacketSource,
    }

    impl RecordingProcessNetworkPacketIoProvider {
        fn requested_ifaces(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.requested_ifaces)
        }
    }

    impl Arm64BootNetworkPacketIoProvider for RecordingProcessNetworkPacketIoProvider {
        fn packet_io(
            &mut self,
            device: &Arm64BootNetworkDevice,
        ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
            self.requested_ifaces
                .lock()
                .expect("requested ifaces should lock")
                .push(device.registration.iface_id().to_string());
            Ok(Arm64BootNetworkPacketIo::new(
                &mut self.tx_sink,
                &mut self.rx_source,
            ))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordingVmnetInterface {
        iface_id: String,
    }

    #[derive(Debug)]
    struct RecordingVmnetPacketIoBackend {
        iface_id: String,
        events: Arc<Mutex<Vec<String>>>,
        start_status: Option<VmnetStatus>,
    }

    impl VmnetInterfaceBackend for RecordingVmnetPacketIoBackend {
        type Interface = RecordingVmnetInterface;

        fn build_interface_descriptor(
            &mut self,
            config: &VmnetInterfaceConfig,
        ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
            push_recorded_event(
                &self.events,
                format!("descriptor:{}:{}", self.iface_id, config.mode()),
            );
            VmnetInterfaceDescriptor::new(config)
        }

        fn start_interface(
            &mut self,
            _descriptor: &VmnetInterfaceDescriptor,
        ) -> Result<Self::Interface, VmnetError> {
            push_recorded_event(&self.events, format!("start:{}", self.iface_id));
            if let Some(status) = self.start_status {
                return Err(VmnetError::new(VmnetOperation::StartInterface, status));
            }

            Ok(RecordingVmnetInterface {
                iface_id: self.iface_id.clone(),
            })
        }

        fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
            push_recorded_event(&self.events, format!("stop:{}", interface.iface_id));
            Ok(())
        }
    }

    impl VmnetPacketIoBackend for RecordingVmnetPacketIoBackend {
        type Interface = RecordingVmnetInterface;

        fn read_packet(
            &mut self,
            interface: &mut Self::Interface,
            _packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            push_recorded_event(&self.events, format!("read:{}", interface.iface_id));
            Ok(None)
        }

        fn write_packet(
            &mut self,
            interface: &mut Self::Interface,
            _packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            push_recorded_event(&self.events, format!("write:{}", interface.iface_id));
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingVmnetPacketIoBackendFactory {
        events: Arc<Mutex<Vec<String>>>,
        start_statuses: VecDeque<Option<VmnetStatus>>,
    }

    impl RecordingVmnetPacketIoBackendFactory {
        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }

        fn with_next_start_status(mut self, status: Option<VmnetStatus>) -> Self {
            self.start_statuses.push_back(status);
            self
        }
    }

    impl ProcessVmnetPacketIoBackendFactory for RecordingVmnetPacketIoBackendFactory {
        type Backend = RecordingVmnetPacketIoBackend;

        fn new_backend(&mut self, iface_id: &str) -> Self::Backend {
            push_recorded_event(&self.events, format!("backend:{iface_id}"));
            RecordingVmnetPacketIoBackend {
                iface_id: iface_id.to_string(),
                events: Arc::clone(&self.events),
                start_status: self.start_statuses.pop_front().unwrap_or(None),
            }
        }
    }

    fn push_recorded_event(events: &Arc<Mutex<Vec<String>>>, event: String) {
        events
            .lock()
            .expect("recorded event log should lock")
            .push(event);
    }

    fn recorded_events(events: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        events
            .lock()
            .expect("recorded event log should lock")
            .clone()
    }

    fn network_configs(
        configs: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> Vec<NetworkInterfaceConfig> {
        let mut network_configs = NetworkInterfaceConfigs::new();
        for (iface_id, host_dev_name) in configs {
            network_configs
                .insert(NetworkInterfaceConfigInput::new(
                    iface_id,
                    iface_id,
                    host_dev_name,
                ))
                .expect("network config should insert");
        }

        network_configs.as_slice().to_vec()
    }

    fn validated_network_configs(count: usize) -> Vec<NetworkInterfaceConfig> {
        (0..count)
            .map(|index| {
                let iface_id = format!("eth{index}");
                NetworkInterfaceConfigInput::new(iface_id.clone(), iface_id, "vmnet:shared")
                    .validate()
                    .expect("individual network config should validate")
            })
            .collect()
    }

    fn test_boot_network_device() -> Arm64BootNetworkDevice {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("test network config should insert");
        let prepared = PreparedNetworkDevices::from_configs(&configs)
            .expect("test network device should prepare");
        let devices = prepared
            .register_mmio(NetworkMmioLayout::new(
                DEFAULT_NETWORK_MMIO_BASE,
                DEFAULT_NETWORK_MMIO_REGION_ID,
            ))
            .expect("test network MMIO should register");
        let (_dispatcher, mut registrations) = devices.into_parts();
        let registration = registrations.remove(0);
        let region = registration.region();

        Arm64BootNetworkDevice {
            registration,
            fdt_device: Arm64FdtVirtioMmioDevice {
                region: Arm64FdtRegion {
                    base: region.range().start().raw_value(),
                    size: region.range().size(),
                },
                interrupt_line: GuestInterruptLine::new(32)
                    .expect("test interrupt line should be valid"),
            },
        }
    }

    fn configured_vmm(starter: FakeStarter) -> ProcessVmm<FakeStarter> {
        let mut vmm = ProcessVmm::with_starter("demo-1", "0.1.0", "bangbang", starter);
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm
    }

    #[test]
    fn default_hvf_boot_session_config_includes_process_owned_serial_output() {
        let executor = HvfInstanceStartExecutor::default();
        let retained_output = executor.serial_output.clone();

        let config = executor.boot_session_config();

        assert_eq!(config.balloon_device, None);
        assert_eq!(config.boot_timer_device, None);
        assert_eq!(config.entropy_device, None);
        let serial = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO");
        assert_eq!(serial.region_id, DEFAULT_SERIAL_MMIO_REGION_ID);
        assert_eq!(serial.address, DEFAULT_SERIAL_MMIO_BASE);
        assert_ne!(serial.region_id, DEFAULT_BOOT_TIMER_MMIO_REGION_ID);
        assert_ne!(serial.address, DEFAULT_BOOT_TIMER_MMIO_BASE);

        let mut configured_output = serial.output.clone();
        configured_output
            .write_byte(b'B')
            .expect("serial output should accept byte");
        assert_eq!(
            retained_output.bytes().expect("serial output should read"),
            b"B"
        );
    }

    #[test]
    fn configured_hvf_boot_session_config_rate_limits_default_serial_output() {
        let executor = HvfInstanceStartExecutor::default();
        let retained_output = executor.serial_output.clone();
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(
                SerialConfigInput::new()
                    .with_rate_limiter(SerialRateLimiterConfig::new(1, None, 60_000)),
            ))
            .expect("serial config should store");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured serial output should build");

        let mut output = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO")
            .output
            .clone();
        output
            .write_byte(b'A')
            .expect("first serial byte should write");
        output
            .write_byte(b'B')
            .expect("exhausted serial byte should be dropped");
        assert_eq!(
            retained_output.bytes().expect("serial output should read"),
            b"A"
        );
        assert_eq!(output.metrics().rate_limiter_dropped_bytes(), 1);
    }

    #[test]
    fn configured_hvf_boot_session_config_includes_balloon_device() {
        let executor = HvfInstanceStartExecutor::default();
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("balloon config should store");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured balloon should build boot config");

        let balloon = config
            .balloon_device
            .expect("configured balloon should add HVF boot balloon device");
        assert_eq!(balloon.mmio_layout.address(), DEFAULT_BALLOON_MMIO_BASE);
        assert_eq!(
            balloon.mmio_layout.region_id(),
            DEFAULT_BALLOON_MMIO_REGION_ID
        );
    }

    #[test]
    fn configured_hvf_boot_session_config_includes_entropy_device() {
        let executor = HvfInstanceStartExecutor::default();
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
            .expect("entropy config should store");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured entropy should build boot config");

        let entropy = config
            .entropy_device
            .expect("configured entropy should add HVF boot entropy device");
        assert_eq!(entropy.mmio_layout.address(), DEFAULT_ENTROPY_MMIO_BASE);
        assert_eq!(
            entropy.mmio_layout.region_id(),
            DEFAULT_ENTROPY_MMIO_REGION_ID
        );
    }

    #[test]
    fn configured_hvf_boot_session_config_includes_memory_hotplug_device() {
        let executor = HvfInstanceStartExecutor::default();
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
                1024, 2, 128,
            )))
            .expect("memory hotplug config should store");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured memory hotplug should build boot config");

        let memory_hotplug = config
            .memory_hotplug_device
            .expect("configured memory hotplug should add HVF boot device");
        assert_eq!(
            memory_hotplug.mmio_layout.address(),
            DEFAULT_MEMORY_HOTPLUG_MMIO_BASE
        );
        assert_eq!(
            memory_hotplug.mmio_layout.region_id(),
            DEFAULT_MEMORY_HOTPLUG_MMIO_REGION_ID
        );
    }

    #[test]
    fn configured_hvf_boot_session_config_includes_boot_timer_device() {
        let executor = HvfInstanceStartExecutor {
            boot_timer_enabled: true,
            ..Default::default()
        };
        let controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured boot timer should build boot config");

        let boot_timer = config
            .boot_timer_device
            .expect("configured boot timer should add HVF boot timer device");
        assert_eq!(
            boot_timer.mmio_layout.address(),
            DEFAULT_BOOT_TIMER_MMIO_BASE
        );
        assert_eq!(
            boot_timer.mmio_layout.region_id(),
            DEFAULT_BOOT_TIMER_MMIO_REGION_ID
        );
    }

    #[test]
    fn configured_hvf_boot_session_config_uses_serial_output_file() {
        let executor = HvfInstanceStartExecutor::default();
        let serial_file = TempFilePath::create("serial-output");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(
                SerialConfigInput::new()
                    .with_serial_out_path(serial_file.path().to_string_lossy().into_owned()),
            ))
            .expect("serial config should store");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured serial output should open");

        let mut output = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO")
            .output
            .clone();
        output
            .write_byte(b'S')
            .expect("serial file output should accept byte");
        assert_eq!(
            fs::read(serial_file.path()).expect("serial output should read"),
            b"S"
        );
    }

    #[test]
    fn configured_hvf_boot_session_config_rate_limits_serial_output_file() {
        let executor = HvfInstanceStartExecutor::default();
        let serial_file = TempFilePath::create("serial-output-rate-limited");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(
                SerialConfigInput::new()
                    .with_serial_out_path(serial_file.path().to_string_lossy().into_owned())
                    .with_rate_limiter(SerialRateLimiterConfig::new(1, None, 60_000)),
            ))
            .expect("serial config should store");

        let config = executor
            .boot_session_config_for_controller(&controller)
            .expect("configured serial output should open");

        let mut output = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO")
            .output
            .clone();
        output
            .write_byte(b'F')
            .expect("first serial file byte should write");
        output
            .write_byte(b'G')
            .expect("exhausted serial file byte should be dropped");
        assert_eq!(
            fs::read(serial_file.path()).expect("serial output should read"),
            b"F"
        );
        assert_eq!(output.metrics().rate_limiter_dropped_bytes(), 1);
    }

    #[test]
    fn configured_hvf_boot_session_config_redacts_serial_output_open_errors() {
        let executor = HvfInstanceStartExecutor::default();
        let missing_path = missing_temp_child_path("serial.out");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(
                SerialConfigInput::new()
                    .with_serial_out_path(missing_path.to_string_lossy().into_owned()),
            ))
            .expect("serial config should store");

        let err = executor
            .boot_session_config_for_controller(&controller)
            .expect_err("missing serial output parent should fail");

        assert_eq!(
            err.to_string(),
            "serial output could not be initialized: NotFound"
        );
        assert!(
            !err.to_string()
                .contains(&missing_path.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn instance_start_rejects_serial_output_open_failure_without_running() {
        let missing_path = missing_temp_child_path("serial-start.out");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            "0.1.0",
            "bangbang",
            HvfInstanceStartExecutor::default(),
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::PutSerial(
            SerialConfigInput::new()
                .with_serial_out_path(missing_path.to_string_lossy().into_owned()),
        ))
        .expect("serial config should store");

        let err = vmm
            .handle_action(VmmAction::InstanceStart)
            .expect_err("missing serial output parent should fail startup");

        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());
        assert_eq!(
            err.to_string(),
            "failed to start microVM: hypervisor error: failed to initialize serial output: serial output could not be initialized: NotFound"
        );
        assert!(
            !err.to_string()
                .contains(&missing_path.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn default_hvf_boot_session_config_uses_non_overlapping_device_layouts() {
        let root = TempFilePath::create("root");
        let data = TempFilePath::create("data");
        let mut drives = DriveConfigs::new();
        drives
            .insert(DriveConfigInput::new("rootfs", "rootfs", root.path(), true))
            .expect("root drive should configure");
        drives
            .insert(DriveConfigInput::new("data", "data", data.path(), false))
            .expect("data drive should configure");
        let mut networks = NetworkInterfaceConfigs::new();
        networks
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network should configure");
        networks
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network should configure");

        let config = default_hvf_boot_session_config(SharedSerialOutput::from(
            SharedSerialOutputBuffer::default(),
        ));
        let serial_region_id = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO")
            .region_id;
        assert_eq!(
            config.network_mmio_layout.base_address(),
            DEFAULT_NETWORK_MMIO_BASE
        );
        assert_eq!(
            config.network_mmio_layout.base_region_id(),
            DEFAULT_NETWORK_MMIO_REGION_ID
        );
        assert_eq!(
            config.pmem_mmio_layout.base_address(),
            DEFAULT_PMEM_MMIO_BASE
        );
        assert_eq!(
            config.pmem_mmio_layout.base_region_id(),
            DEFAULT_PMEM_MMIO_REGION_ID
        );
        assert_eq!(config.vsock_mmio_layout.address(), DEFAULT_VSOCK_MMIO_BASE);
        assert_eq!(
            config.vsock_mmio_layout.region_id(),
            DEFAULT_VSOCK_MMIO_REGION_ID
        );
        let block_devices = PreparedBlockDevices::from_configs(&drives)
            .expect("block devices should prepare")
            .register_mmio(config.block_mmio_layout)
            .expect("block MMIO should register");
        let network_devices = PreparedNetworkDevices::from_configs(&networks)
            .expect("network devices should prepare")
            .register_mmio(config.network_mmio_layout)
            .expect("network MMIO should register");
        let pmem_first_region = MmioRegion::new(
            config.pmem_mmio_layout.base_region_id(),
            config.pmem_mmio_layout.base_address(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("pmem MMIO region should be valid");
        let vsock_region = MmioRegion::new(
            config.vsock_mmio_layout.region_id(),
            config.vsock_mmio_layout.address(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("vsock MMIO region should be valid");
        let entropy_region = MmioRegion::new(
            DEFAULT_ENTROPY_MMIO_REGION_ID,
            DEFAULT_ENTROPY_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("entropy MMIO region should be valid");
        let balloon_region = MmioRegion::new(
            DEFAULT_BALLOON_MMIO_REGION_ID,
            DEFAULT_BALLOON_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("balloon MMIO region should be valid");
        let memory_hotplug_region = MmioRegion::new(
            DEFAULT_MEMORY_HOTPLUG_MMIO_REGION_ID,
            DEFAULT_MEMORY_HOTPLUG_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("memory hotplug MMIO region should be valid");
        let serial_region = MmioRegion::new(
            serial_region_id,
            DEFAULT_SERIAL_MMIO_BASE,
            SERIAL_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("serial MMIO region should be valid");

        assert!(
            block_devices
                .registrations()
                .iter()
                .all(|registration| registration.region_id() != serial_region_id
                    && registration.region_id() != pmem_first_region.id()
                    && registration.region_id() != vsock_region.id()
                    && registration.region_id() != entropy_region.id()
                    && registration.region_id() != balloon_region.id()
                    && registration.region_id() != memory_hotplug_region.id())
        );
        assert!(network_devices.registrations().iter().all(
            |registration| registration.region_id() != serial_region_id
                && registration.region_id() != pmem_first_region.id()
                && registration.region_id() != vsock_region.id()
                && registration.region_id() != entropy_region.id()
                && registration.region_id() != balloon_region.id()
                && registration.region_id() != memory_hotplug_region.id()
        ));
        assert_ne!(pmem_first_region.id(), serial_region_id);
        assert_ne!(vsock_region.id(), serial_region_id);
        assert_ne!(vsock_region.id(), pmem_first_region.id());
        assert_ne!(entropy_region.id(), serial_region_id);
        assert_ne!(entropy_region.id(), pmem_first_region.id());
        assert_ne!(entropy_region.id(), vsock_region.id());
        assert_ne!(balloon_region.id(), serial_region_id);
        assert_ne!(balloon_region.id(), pmem_first_region.id());
        assert_ne!(balloon_region.id(), vsock_region.id());
        assert_ne!(balloon_region.id(), entropy_region.id());
        assert_ne!(memory_hotplug_region.id(), serial_region_id);
        assert_ne!(memory_hotplug_region.id(), pmem_first_region.id());
        assert_ne!(memory_hotplug_region.id(), vsock_region.id());
        assert_ne!(memory_hotplug_region.id(), entropy_region.id());
        assert_ne!(memory_hotplug_region.id(), balloon_region.id());
        assert!(block_devices.registrations().iter().all(|block| {
            network_devices
                .registrations()
                .iter()
                .all(|network| !block.region().range().overlaps(network.region().range()))
                && !block.region().range().overlaps(serial_region.range())
                && !block.region().range().overlaps(pmem_first_region.range())
                && !block.region().range().overlaps(vsock_region.range())
                && !block.region().range().overlaps(entropy_region.range())
                && !block.region().range().overlaps(balloon_region.range())
                && !block
                    .region()
                    .range()
                    .overlaps(memory_hotplug_region.range())
        }));
        assert!(network_devices.registrations().iter().all(|network| {
            !network.region().range().overlaps(serial_region.range())
                && !network.region().range().overlaps(pmem_first_region.range())
                && !network.region().range().overlaps(vsock_region.range())
                && !network.region().range().overlaps(entropy_region.range())
                && !network.region().range().overlaps(balloon_region.range())
                && !network
                    .region()
                    .range()
                    .overlaps(memory_hotplug_region.range())
        }));
        assert!(!pmem_first_region.range().overlaps(serial_region.range()));
        assert!(!vsock_region.range().overlaps(serial_region.range()));
        assert!(!vsock_region.range().overlaps(pmem_first_region.range()));
        assert!(!entropy_region.range().overlaps(serial_region.range()));
        assert!(!entropy_region.range().overlaps(pmem_first_region.range()));
        assert!(!entropy_region.range().overlaps(vsock_region.range()));
        assert!(!balloon_region.range().overlaps(serial_region.range()));
        assert!(!balloon_region.range().overlaps(pmem_first_region.range()));
        assert!(!balloon_region.range().overlaps(vsock_region.range()));
        assert!(!balloon_region.range().overlaps(entropy_region.range()));
        assert!(
            !memory_hotplug_region
                .range()
                .overlaps(serial_region.range())
        );
        assert!(
            !memory_hotplug_region
                .range()
                .overlaps(pmem_first_region.range())
        );
        assert!(!memory_hotplug_region.range().overlaps(vsock_region.range()));
        assert!(
            !memory_hotplug_region
                .range()
                .overlaps(entropy_region.range())
        );
        assert!(
            !memory_hotplug_region
                .range()
                .overlaps(balloon_region.range())
        );
    }

    #[test]
    fn process_hvf_boot_session_routes_run_loop_through_packet_io() {
        let control = FakeRunLoopControl::default();
        let stop_token = control.stop_token();
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let fake_session = FakeNetworkPacketIoRunLoopSession::new(control, max_steps_sender);
        let provider = RecordingProcessNetworkPacketIoProvider::default();
        let requested_ifaces = provider.requested_ifaces();
        let mut session = ProcessHvfBootSession::new(fake_session, provider);

        let result = session
            .run_loop(
                &stop_token,
                NonZeroUsize::new(17).expect("test step limit should be nonzero"),
            )
            .expect("process HVF boot session should run");

        assert_eq!(result, FakeRunLoopOutcome::Terminal);
        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("fake session should receive step limit"),
            17
        );
        assert_eq!(
            *requested_ifaces
                .lock()
                .expect("requested ifaces should lock"),
            ["eth0".to_string()]
        );
    }

    #[test]
    fn process_network_packet_io_provider_empty_configs_use_noop() {
        let mut provider = ProcessNetworkPacketIoProvider::from_network_configs(&[])
            .expect("empty network configs should build a no-op provider");

        match &provider {
            ProcessNetworkPacketIoProvider::Noop(_) => {}
            ProcessNetworkPacketIoProvider::MmdsOnly(_)
            | ProcessNetworkPacketIoProvider::Vmnet(_) => {
                panic!("empty network configs should not build a network provider");
            }
        }
        provider
            .packet_io(&test_boot_network_device())
            .expect("no-op provider should return packet I/O for any device");
    }

    #[test]
    fn process_vmnet_packet_io_provider_starts_supported_configs() {
        let configs = network_configs([("eth0", "vmnet:shared")]);
        let mut factory = RecordingVmnetPacketIoBackendFactory::default();
        let event_log = factory.events();

        {
            let mut provider =
                process_vmnet_packet_io_provider_from_configs(&configs, &mut factory)
                    .expect("supported vmnet configs should build provider");
            provider
                .packet_io(&test_boot_network_device())
                .expect("provider should select packet I/O by iface id");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "backend:eth0".to_string(),
                "descriptor:eth0:shared".to_string(),
                "start:eth0".to_string(),
                "stop:eth0".to_string(),
            ]
        );
    }

    #[test]
    fn process_vmnet_packet_io_provider_maps_all_supported_host_dev_name_forms() {
        let configs = network_configs([
            ("eth0", "vmnet:host"),
            ("eth1", "vmnet:shared"),
            ("eth2", "vmnet:bridged:en0"),
        ]);
        let mut factory = RecordingVmnetPacketIoBackendFactory::default();
        let event_log = factory.events();
        let provider = process_vmnet_packet_io_provider_from_configs(&configs, &mut factory)
            .expect("all supported vmnet host device names should build provider");

        let events = recorded_events(&event_log);
        assert!(events.iter().any(|event| event == "descriptor:eth0:host"));
        assert!(events.iter().any(|event| event == "descriptor:eth1:shared"));
        assert!(
            events
                .iter()
                .any(|event| event == "descriptor:eth2:bridged")
        );
        drop(provider);

        let events = recorded_events(&event_log);
        for iface_id in ["eth0", "eth1", "eth2"] {
            let expected = format!("stop:{iface_id}");
            assert!(events.iter().any(|event| event == &expected));
        }
    }

    #[test]
    fn process_mmds_packet_detour_config_matches_only_configured_interfaces() {
        let configs = network_configs([
            ("eth0", "vmnet:shared"),
            ("eth1", "vmnet:shared"),
            ("eth2", "vmnet:shared"),
        ]);
        let mmds_config = MmdsConfigInput::new(vec!["eth0".to_string(), "eth2".to_string()])
            .validate(&configs)
            .expect("MMDS config should validate");
        let detour_config = ProcessMmdsPacketDetourConfig::from_mmds_config(
            MmdsStateHandle::default(),
            &mmds_config,
        );

        let eth0 = detour_config
            .detour_for_interface("eth0")
            .expect("eth0 should have an MMDS detour");
        assert!(detour_config.detour_for_interface("eth1").is_none());
        let eth2 = detour_config
            .detour_for_interface("eth2")
            .expect("eth2 should have an MMDS detour");
        assert!(
            !eth0
                .response_queue()
                .shares_state_with(&eth2.response_queue())
        );
    }

    #[test]
    fn process_network_packet_io_provider_uses_mmds_only_for_all_mmds_interfaces() {
        let configs = network_configs([("eth0", "vmnet:shared"), ("eth1", "vmnet:shared")]);
        let mmds_config = MmdsConfigInput::new(vec!["eth0".to_string(), "eth1".to_string()])
            .validate(&configs)
            .expect("MMDS config should validate");
        let detour_config = ProcessMmdsPacketDetourConfig::from_mmds_config(
            MmdsStateHandle::default(),
            &mmds_config,
        );

        let mut provider = ProcessNetworkPacketIoProvider::from_network_configs_and_mmds_detour(
            &configs,
            Some(&detour_config),
        )
        .expect("all-MMDS network configs should build an MMDS-only provider");

        match &provider {
            ProcessNetworkPacketIoProvider::MmdsOnly(_) => {}
            ProcessNetworkPacketIoProvider::Noop(_) | ProcessNetworkPacketIoProvider::Vmnet(_) => {
                panic!("all-MMDS network configs should not open vmnet resources");
            }
        }
        provider
            .packet_io(&test_boot_network_device())
            .expect("MMDS-only provider should return packet I/O for configured device");
    }

    #[test]
    fn process_network_packet_io_provider_validates_host_dev_name_for_mmds_only() {
        let configs = network_configs([("eth0", "tap0")]);
        let mmds_config = MmdsConfigInput::new(vec!["eth0".to_string()])
            .validate(&configs)
            .expect("MMDS config should validate");
        let detour_config = ProcessMmdsPacketDetourConfig::from_mmds_config(
            MmdsStateHandle::default(),
            &mmds_config,
        );

        let error = ProcessNetworkPacketIoProvider::from_network_configs_and_mmds_detour(
            &configs,
            Some(&detour_config),
        )
        .expect_err("MMDS-only provider should still validate host_dev_name syntax");

        match error {
            ProcessNetworkPacketIoProviderBuildError::HostDeviceName { iface_id, .. } => {
                assert_eq!(iface_id, "eth0");
            }
            ProcessNetworkPacketIoProviderBuildError::NetworkInterfaceCount { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsState { .. }
            | ProcessNetworkPacketIoProviderBuildError::Start { .. }
            | ProcessNetworkPacketIoProviderBuildError::PacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyPacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MissingMmdsDetour { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyProviderBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::ProviderBuild { .. } => {
                panic!("unsupported host_dev_name should be reported as config failure");
            }
        }
    }

    #[test]
    fn process_vmnet_packet_io_provider_rejects_over_limit_before_backend() {
        let configs = validated_network_configs(MAX_NETWORK_INTERFACE_COUNT + 1);
        let mut factory = RecordingVmnetPacketIoBackendFactory::default();
        let event_log = factory.events();
        let error = process_vmnet_packet_io_provider_from_configs(&configs, &mut factory)
            .expect_err("over-limit network configs should fail provider construction");

        match error {
            ProcessNetworkPacketIoProviderBuildError::NetworkInterfaceCount { source } => {
                assert_eq!(
                    source,
                    NetworkInterfaceConfigError::TooManyNetworkInterfaces {
                        count: MAX_NETWORK_INTERFACE_COUNT + 1,
                        max: MAX_NETWORK_INTERFACE_COUNT,
                    }
                );
            }
            ProcessNetworkPacketIoProviderBuildError::HostDeviceName { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsState { .. }
            | ProcessNetworkPacketIoProviderBuildError::Start { .. }
            | ProcessNetworkPacketIoProviderBuildError::PacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyPacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MissingMmdsDetour { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyProviderBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::ProviderBuild { .. } => {
                panic!("over-limit configs should fail before vmnet backend start");
            }
        }
        assert!(recorded_events(&event_log).is_empty());
    }

    #[test]
    fn process_vmnet_packet_io_provider_rejects_unsupported_host_dev_name_before_backend() {
        let configs = network_configs([("eth0", "tap0")]);
        let mut factory = RecordingVmnetPacketIoBackendFactory::default();
        let event_log = factory.events();
        let error = process_vmnet_packet_io_provider_from_configs(&configs, &mut factory)
            .expect_err("unsupported host_dev_name should fail provider construction");

        match error {
            ProcessNetworkPacketIoProviderBuildError::HostDeviceName { iface_id, .. } => {
                assert_eq!(iface_id, "eth0");
            }
            ProcessNetworkPacketIoProviderBuildError::NetworkInterfaceCount { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsState { .. }
            | ProcessNetworkPacketIoProviderBuildError::Start { .. }
            | ProcessNetworkPacketIoProviderBuildError::PacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyPacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MissingMmdsDetour { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyProviderBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::ProviderBuild { .. } => {
                panic!("unsupported host_dev_name should fail before vmnet backend start");
            }
        }
        assert!(recorded_events(&event_log).is_empty());
    }

    #[test]
    fn process_vmnet_packet_io_provider_cleans_started_entries_after_later_failure() {
        let configs = network_configs([("eth0", "vmnet:shared"), ("eth1", "tap1")]);
        let mut factory = RecordingVmnetPacketIoBackendFactory::default();
        let event_log = factory.events();
        let error = process_vmnet_packet_io_provider_from_configs(&configs, &mut factory)
            .expect_err("later unsupported config should fail provider construction");

        match error {
            ProcessNetworkPacketIoProviderBuildError::HostDeviceName { iface_id, .. } => {
                assert_eq!(iface_id, "eth1");
            }
            ProcessNetworkPacketIoProviderBuildError::NetworkInterfaceCount { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsState { .. }
            | ProcessNetworkPacketIoProviderBuildError::Start { .. }
            | ProcessNetworkPacketIoProviderBuildError::PacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyPacketIoBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::MissingMmdsDetour { .. }
            | ProcessNetworkPacketIoProviderBuildError::MmdsOnlyProviderBuild { .. }
            | ProcessNetworkPacketIoProviderBuildError::ProviderBuild { .. } => {
                panic!("unsupported host_dev_name should be reported as config failure");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                "backend:eth0".to_string(),
                "descriptor:eth0:shared".to_string(),
                "start:eth0".to_string(),
                "stop:eth0".to_string(),
            ]
        );
    }

    #[test]
    fn process_vmnet_packet_io_provider_start_failure_preserves_not_started_state() {
        let mut controller = bangbang_runtime::VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                "/tmp/vmlinux",
            )))
            .expect("boot source should configure");
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new("eth0", "eth0", "vmnet:shared"),
            ))
            .expect("network config should insert");
        let mut factory = RecordingVmnetPacketIoBackendFactory::default()
            .with_next_start_status(Some(VmnetStatus::NotAuthorized));
        let event_log = factory.events();
        let error = controller
            .start_instance_with(|controller| {
                process_vmnet_packet_io_provider_from_configs(
                    controller.network_interface_configs(),
                    &mut factory,
                )
                .map(|_provider| ())
                .map_err(|err| {
                    BackendError::Hypervisor(format!(
                        "failed to build network packet I/O provider: {err}"
                    ))
                })
            })
            .expect_err("vmnet start failure should fail InstanceStart");

        match error {
            VmmActionError::InstanceStart(BackendError::Hypervisor(message)) => {
                assert!(message.contains("failed to start vmnet packet I/O for interface eth0"));
            }
            VmmActionError::InstanceStart(
                BackendError::Unsupported(_) | BackendError::InvalidState(_),
            )
            | VmmActionError::UnsupportedAction(_)
            | VmmActionError::UnsupportedState { .. }
            | VmmActionError::BalloonConfig(_)
            | VmmActionError::BalloonHintingCommand(_)
            | VmmActionError::BalloonHintingStatus(_)
            | VmmActionError::BalloonStats(_)
            | VmmActionError::BalloonUnsupported
            | VmmActionError::BalloonUpdate(_)
            | VmmActionError::EntropyConfig(_)
            | VmmActionError::EntropyUnsupported
            | VmmActionError::Lifecycle(_)
            | VmmActionError::MissingBootSource
            | VmmActionError::BootSourceConfig(_)
            | VmmActionError::DriveConfig(_)
            | VmmActionError::DriveUpdate(_)
            | VmmActionError::DriveUpdateUnsupported
            | VmmActionError::LoggerConfig(_)
            | VmmActionError::LoggerWrite(_)
            | VmmActionError::MachineConfig(_)
            | VmmActionError::MetricsConfig(_)
            | VmmActionError::MetricsFlush(_)
            | VmmActionError::MmdsConfig(_)
            | VmmActionError::MmdsDataStore(_)
            | VmmActionError::MmdsState(_)
            | VmmActionError::NetworkInterfaceConfig(_)
            | VmmActionError::NetworkInterfaceUpdate(_)
            | VmmActionError::NetworkInterfaceUpdateUnsupported
            | VmmActionError::MemoryHotplugConfig(_)
            | VmmActionError::MemoryHotplugStatus(_)
            | VmmActionError::MemoryHotplugUpdate(_)
            | VmmActionError::MemoryHotplugUnsupported
            | VmmActionError::PmemConfig(_)
            | VmmActionError::PmemUpdate(_)
            | VmmActionError::PmemUnsupported
            | VmmActionError::SerialConfig(_)
            | VmmActionError::SnapshotUnsupported
            | VmmActionError::VsockConfig(_) => {
                panic!("vmnet start failure should propagate as hypervisor startup error");
            }
        }
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(
            recorded_events(&event_log),
            [
                "backend:eth0".to_string(),
                "descriptor:eth0:shared".to_string(),
                "start:eth0".to_string(),
            ]
        );
    }

    #[test]
    fn default_hvf_boot_run_loop_step_limit_is_nonzero() {
        assert_eq!(
            default_hvf_boot_run_loop_step_limit().get(),
            DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT
        );
    }

    #[test]
    fn boot_run_loop_supervisor_returns_without_waiting_for_stop() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(7).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            7
        );
        assert_eq!(control.request_stop_count(), 0);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_running_status_before_stop() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Running);
        assert_eq!(
            supervisor.metrics_diagnostics().boot_run_loop_status(),
            Some(BootRunLoopMetricStatus::Running)
        );
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_block_device_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs", "data"]);
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_block_device_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_queue_events_for_drive("rootfs", 1);
        metrics.record_event_failure_for_drive("rootfs");
        let diagnostics = supervisor.metrics_diagnostics();

        assert_eq!(
            diagnostics.block_device_metrics(),
            Some(
                BlockDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1)
            )
        );
        assert_eq!(
            diagnostics.block_device_metrics_by_drive(),
            Some(
                &BlockDeviceMetricsByDrive::new().with_drive_metrics(
                    "rootfs",
                    BlockDeviceMetrics::default()
                        .with_event_fails(1)
                        .with_queue_event_count(1),
                )
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_pmem_device_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0", "pmem1"]);
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_pmem_device_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_queue_events_for_device("pmem0", 1);
        metrics.record_event_failure_for_device("pmem0");
        let diagnostics = supervisor.metrics_diagnostics();

        assert_eq!(
            diagnostics.pmem_device_metrics(),
            Some(
                PmemDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1)
            )
        );
        assert_eq!(
            diagnostics.pmem_device_metrics_by_device(),
            Some(
                &PmemDeviceMetricsByDevice::new().with_device_metrics(
                    "pmem0",
                    PmemDeviceMetrics::default()
                        .with_event_fails(1)
                        .with_queue_event_count(1),
                )
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_network_interface_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0", "eth1"]);
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_network_interface_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_queue_events_for_interface("eth0", 1, 2);
        metrics.record_event_failure_for_interface("eth0");
        let diagnostics = supervisor.metrics_diagnostics();

        assert_eq!(
            diagnostics.network_interface_metrics(),
            Some(
                NetworkInterfaceMetrics::default()
                    .with_event_fails(1)
                    .with_rx_queue_event_count(1)
                    .with_tx_queue_event_count(2)
            )
        );
        assert_eq!(
            diagnostics.network_interface_metrics_by_interface(),
            Some(
                &NetworkInterfaceMetricsByInterface::new().with_interface_metrics(
                    "eth0",
                    NetworkInterfaceMetrics::default()
                        .with_event_fails(1)
                        .with_rx_queue_event_count(1)
                        .with_tx_queue_event_count(2),
                )
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_vsock_device_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedVsockDeviceMetrics::default();
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_vsock_device_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_activation_failure();
        metrics.record_muxer_event_failure();

        assert_eq!(
            supervisor.metrics_diagnostics().vsock_device_metrics(),
            Some(
                VsockDeviceMetrics::default()
                    .with_activate_fails(1)
                    .with_muxer_event_fails(1)
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_entropy_device_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedEntropyDeviceMetrics::default();
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_entropy_device_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_event_failure();
        metrics.record_entropy_source_provider_failure();

        assert_eq!(
            supervisor.metrics_diagnostics().entropy_device_metrics(),
            Some(
                EntropyDeviceMetrics::default()
                    .with_entropy_event_fails(2)
                    .with_host_rng_fails(1)
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_rtc_device_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedRtcDeviceMetrics::default();
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_rtc_device_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_read_error();
        metrics.record_write_error();

        assert_eq!(
            supervisor.metrics_diagnostics().rtc_device_metrics(),
            Some(
                RtcDeviceMetrics::default()
                    .with_error_count(2)
                    .with_missed_read_count(1)
                    .with_missed_write_count(1)
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_reports_balloon_device_metrics() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedBalloonDeviceMetrics::default();
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_balloon_device_metrics(metrics.clone());

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(5).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            5
        );
        metrics.record_event_failure();
        metrics.record_statistics_update_failure();

        assert_eq!(
            supervisor.metrics_diagnostics().balloon_device_metrics(),
            Some(BalloonDeviceMetrics::new(0, 0, 0, 1, 0, 1))
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_pauses_without_entering_next_run_loop() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Wakeup),
                ])
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true);
        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(17).expect("non-zero limit"))
                .expect("supervisor should start");
        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter first run loop"),
            17
        );

        supervisor.pause().expect("supervisor should pause");

        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Paused);
        assert_eq!(
            supervisor.metrics_diagnostics().boot_run_loop_status(),
            Some(BootRunLoopMetricStatus::Paused)
        );
        assert_eq!(
            supervisor.process_exit_status(),
            super::ProcessSessionExitStatus::Running
        );
        assert_eq!(
            max_steps_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        supervisor.resume().expect("supervisor should resume");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("run loop should restart after resume"),
            17
        );
        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Running);
        drop(supervisor);
        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_runs_command_while_paused() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_outcomes([Ok(FakeRunLoopOutcome::Wakeup)])
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true);
        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(19).expect("non-zero limit"))
                .expect("supervisor should start");
        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter first run loop"),
            19
        );
        supervisor.pause().expect("supervisor should pause");

        let run_count = supervisor
            .run_command(|session| {
                Ok::<u64, FakeRunLoopCommandError>(session.run_count.load(Ordering::SeqCst))
            })
            .expect("paused worker should still execute queued commands");

        assert_eq!(run_count, 1);
        assert_eq!(
            max_steps_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        drop(supervisor);
        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_updates_memory_hotplug_on_worker_after_wakeup() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let update = memory_hotplug_config()
            .validate_size_update(memory_hotplug_size_update_input(256))
            .expect("memory hotplug update should validate");
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);
        let updates = session.memory_hotplug_updates();
        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(21).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            21
        );
        supervisor
            .update_memory_hotplug(update)
            .expect("memory hotplug update should run on worker");

        assert_eq!(control.request_wakeup_count(), 1);
        assert_eq!(
            *updates
                .lock()
                .expect("fake memory hotplug updates should lock"),
            [256]
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_updates_memory_hotplug_while_paused() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let update = memory_hotplug_config()
            .validate_size_update(memory_hotplug_size_update_input(128))
            .expect("memory hotplug update should validate");
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_outcomes([Ok(FakeRunLoopOutcome::Wakeup)])
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true);
        let updates = session.memory_hotplug_updates();
        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(22).expect("non-zero limit"))
                .expect("supervisor should start");
        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter first run loop"),
            22
        );
        supervisor.pause().expect("supervisor should pause");

        supervisor
            .update_memory_hotplug(update)
            .expect("paused worker should still update memory hotplug");

        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Paused);
        assert_eq!(
            max_steps_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        assert_eq!(
            *updates
                .lock()
                .expect("fake memory hotplug updates should lock"),
            [128]
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_queries_memory_hotplug_status_on_worker_after_wakeup() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_memory_hotplug_status_plugged_size_mib(384)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);
        let status_requests = session.memory_hotplug_status_requests();
        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(24).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            24
        );
        let status = supervisor
            .memory_hotplug_status(memory_hotplug_config(), 512)
            .expect("memory hotplug status should run on worker");

        assert_eq!(status, memory_hotplug_status_with_plugged(384, 512));
        assert_eq!(control.request_wakeup_count(), 1);
        assert_eq!(
            *status_requests
                .lock()
                .expect("fake memory hotplug status requests should lock"),
            [512]
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_queries_memory_hotplug_status_while_paused() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_memory_hotplug_status_plugged_size_mib(128)
                .with_outcomes([Ok(FakeRunLoopOutcome::Wakeup)])
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true);
        let status_requests = session.memory_hotplug_status_requests();
        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(25).expect("non-zero limit"))
                .expect("supervisor should start");
        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter first run loop"),
            25
        );
        supervisor.pause().expect("supervisor should pause");

        let status = supervisor
            .memory_hotplug_status(memory_hotplug_config(), 256)
            .expect("paused worker should still query memory hotplug status");

        assert_eq!(status, memory_hotplug_status_with_plugged(128, 256));
        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Paused);
        assert_eq!(
            max_steps_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        assert_eq!(
            *status_requests
                .lock()
                .expect("fake memory hotplug status requests should lock"),
            [256]
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_rejects_pause_and_resume_after_terminal_outcome() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_outcomes([Ok(FakeRunLoopOutcome::Terminal)])
                .with_wait_for_stop(false);
        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(23).expect("non-zero limit"))
                .expect("supervisor should start");
        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            23
        );
        assert_eq!(
            supervisor.wait_for_terminal_status(),
            BootRunLoopWorkerStatus::Exited(FakeRunLoopOutcome::Terminal)
        );

        let pause_err = supervisor
            .pause()
            .expect_err("terminal worker should reject pause");
        let resume_err = supervisor
            .resume()
            .expect_err("terminal worker should reject resume");

        assert_eq!(
            pause_err,
            BackendError::InvalidState("boot run loop worker is not running")
        );
        assert_eq!(
            resume_err,
            BackendError::InvalidState("boot run loop worker is not paused")
        );
        supervisor
            .drain_process_exit_wakeup()
            .expect("terminal wakeup should drain");
        drop(supervisor);
        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_retains_session_after_bounded_loop_returns() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(3).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            3
        );
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_records_terminal_outcome_before_release() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(3).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            3
        );
        assert_eq!(
            supervisor.wait_for_terminal_status(),
            BootRunLoopWorkerStatus::Exited(FakeRunLoopOutcome::Terminal)
        );
        assert_eq!(
            supervisor.metrics_diagnostics().boot_run_loop_status(),
            Some(BootRunLoopMetricStatus::Exited)
        );
        assert_eq!(
            supervisor.process_exit_status(),
            super::ProcessSessionExitStatus::GuestRequestedStop
        );
        assert!(supervisor.process_exit_wakeup_fd().is_some());
        supervisor
            .drain_process_exit_wakeup()
            .expect("terminal wakeup should drain");
        supervisor
            .drain_process_exit_wakeup()
            .expect("terminal wakeup drain should be idempotent");
        assert_eq!(control.request_stop_count(), 0);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_repeats_after_step_limit() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::StepLimitReached),
                    Ok(FakeRunLoopOutcome::StepLimitReached),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);
        let run_count = session.run_count();

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(11).expect("non-zero limit"))
                .expect("supervisor should start");

        for _ in 0..3 {
            assert_eq!(
                max_steps_receiver
                    .recv()
                    .expect("worker should enter run loop"),
                11
            );
        }
        assert_eq!(run_count.load(Ordering::SeqCst), 3);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_repeats_after_wakeup_outcome() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);
        let run_count = session.run_count();

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(13).expect("non-zero limit"))
                .expect("supervisor should start");

        for _ in 0..2 {
            assert_eq!(
                max_steps_receiver
                    .recv()
                    .expect("worker should enter run loop"),
                13
            );
        }
        assert_eq!(run_count.load(Ordering::SeqCst), 2);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_runs_command_on_worker_after_wakeup() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(23).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            23
        );

        let result = supervisor
            .run_command(|session| {
                Ok::<_, FakeRunLoopCommandError>((
                    session.run_count.load(Ordering::SeqCst),
                    std::thread::current()
                        .name()
                        .unwrap_or_default()
                        .to_string(),
                ))
            })
            .expect("command should run");

        assert_eq!(result.0, 1);
        assert_eq!(result.1, super::HVF_BOOT_RUN_LOOP_THREAD_NAME);
        assert_eq!(control.request_wakeup_count(), 1);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_returns_command_failure_without_terminal_status() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(29).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            29
        );

        let error = supervisor
            .run_command(|_| Err::<(), _>(FakeRunLoopCommandError))
            .expect_err("command failure should be returned");

        assert_eq!(
            error,
            super::BootRunLoopCommandError::Command {
                source: FakeRunLoopCommandError
            }
        );
        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Running);
        assert_eq!(control.request_wakeup_count(), 1);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_rejects_command_after_terminal_outcome() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(31).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            31
        );
        assert_eq!(
            supervisor.wait_for_terminal_status(),
            BootRunLoopWorkerStatus::Exited(FakeRunLoopOutcome::Terminal)
        );

        let error = supervisor
            .run_command(|_| Ok::<_, FakeRunLoopCommandError>(()))
            .expect_err("terminal worker should reject commands");

        assert_eq!(error, super::BootRunLoopCommandError::WorkerNotRunning);
        assert_eq!(control.request_wakeup_count(), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_closes_pending_command_after_run_loop_error() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_outcomes([Err(FakeRunLoopError)]);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(33).expect("non-zero limit"))
                .expect("supervisor should start");
        let command_handle = supervisor.command_handle();

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            33
        );

        let command_caller = std::thread::spawn(move || {
            command_handle.run(|_| Ok::<_, FakeRunLoopCommandError>(()))
        });
        control.wait_for_request_wakeup_count(1);
        control
            .request_stop()
            .expect("fake stop should release fake run loop");

        let error = command_caller
            .join()
            .expect("command caller should not panic")
            .expect_err("pending command response should close");

        assert_eq!(error, super::BootRunLoopCommandError::ResponseClosed);
        assert_eq!(
            supervisor.wait_for_terminal_status(),
            BootRunLoopWorkerStatus::Failed("fake run loop failed".to_string())
        );
        supervisor
            .drain_process_exit_wakeup()
            .expect("error wakeup should drain");

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 2);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_command_queue_full_does_not_block() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender);

        let supervisor = BootRunLoopSupervisor::start_with_command_queue_capacity(
            session,
            NonZeroUsize::new(37).expect("non-zero limit"),
            0,
        )
        .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            37
        );

        let error = supervisor
            .run_command(|_| Ok::<_, FakeRunLoopCommandError>(()))
            .expect_err("zero-capacity queue should reject command");

        assert_eq!(error, super::BootRunLoopCommandError::QueueFull);
        assert_eq!(control.request_wakeup_count(), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_handles_concurrent_command_callers() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::StepLimitReached),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(41).expect("non-zero limit"))
                .expect("supervisor should start");
        let command_handle = supervisor.command_handle();

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            41
        );

        let mut results = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for index in 0..4 {
                let handle = command_handle.clone();
                handles.push(scope.spawn(move || {
                    handle.run(move |session| {
                        Ok::<_, FakeRunLoopCommandError>((
                            index,
                            session.run_count.load(Ordering::SeqCst),
                        ))
                    })
                }));
            }

            control.wait_for_request_wakeup_count(4);
            control
                .request_stop()
                .expect("fake stop should release fake run loop");

            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .expect("command caller should not panic")
                        .expect("command should succeed")
                })
                .collect::<Vec<_>>()
        });
        results.sort_unstable();

        assert_eq!(
            results.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert!(results.iter().all(|(_, run_count)| *run_count > 0));
        assert_eq!(control.request_wakeup_count(), 4);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 2);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_updates_drive_backing_on_worker_after_wakeup() {
        let original = TempFilePath::create_with_bytes("run-loop-drive-original", &[0x11; 512]);
        let replacement =
            TempFilePath::create_with_bytes("run-loop-drive-replacement", &[0x22; 1024]);
        let (updater, _original_config) = block_device_updater_fixture("data", original.path());
        let replacement_config = drive_config("data", replacement.path());
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["data"]);
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_block_device_updater(updater)
                .with_block_device_metrics(metrics)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(43).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            43
        );
        supervisor
            .update_block_device(&replacement_config, true, None)
            .expect("drive update should run on worker");
        let diagnostics = supervisor.metrics_diagnostics();

        assert_eq!(control.request_wakeup_count(), 1);
        assert_eq!(
            diagnostics.block_device_metrics(),
            Some(BlockDeviceMetrics::default().with_update_count(1))
        );
        assert_eq!(
            diagnostics.block_device_metrics_by_drive(),
            Some(
                &BlockDeviceMetricsByDrive::new()
                    .with_drive_metrics("data", BlockDeviceMetrics::default().with_update_count(1))
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_serializes_drive_update_after_existing_command() {
        let original =
            TempFilePath::create_with_bytes("run-loop-drive-serialized-original", &[0x11; 512]);
        let replacement =
            TempFilePath::create_with_bytes("run-loop-drive-serialized-replacement", &[0x22; 1024]);
        let (updater, _original_config) = block_device_updater_fixture("data", original.path());
        let replacement_config = drive_config("data", replacement.path());
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_block_device_updater(updater)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(44).expect("non-zero limit"))
                .expect("supervisor should start");
        let command_handle = supervisor.command_handle();
        let (command_started_sender, command_started_receiver) = mpsc::channel();
        let (release_command_sender, release_command_receiver) = mpsc::channel();

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            44
        );

        std::thread::scope(|scope| {
            let blocking_command = scope.spawn(move || {
                command_handle.run(move |_| {
                    command_started_sender
                        .send(())
                        .expect("command start should be observed");
                    release_command_receiver
                        .recv()
                        .expect("command should be released");
                    Ok::<_, FakeRunLoopCommandError>(())
                })
            });
            control.wait_for_request_wakeup_count(1);
            command_started_receiver
                .recv()
                .expect("blocking command should start on worker");

            let drive_update =
                scope.spawn(|| supervisor.update_block_device(&replacement_config, true, None));
            control.wait_for_request_wakeup_count(2);
            release_command_sender
                .send(())
                .expect("blocking command release should send");

            blocking_command
                .join()
                .expect("blocking command caller should not panic")
                .expect("blocking command should complete");
            drive_update
                .join()
                .expect("drive update caller should not panic")
                .expect("drive update should run after blocking command");
        });

        assert_eq!(control.request_wakeup_count(), 2);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_propagates_drive_update_command_error() {
        let original =
            TempFilePath::create_with_bytes("run-loop-drive-error-original", &[0x11; 512]);
        let replacement =
            TempFilePath::create_with_bytes("run-loop-drive-error-replacement", &[0x22; 1024]);
        let (updater, _original_config) = block_device_updater_fixture("data", original.path());
        let replacement_config = drive_config("missing", replacement.path());
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_block_device_updater(updater)
                .with_wait_for_stop(false)
                .with_wait_for_wakeup(true)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::Wakeup),
                    Ok(FakeRunLoopOutcome::Terminal),
                ]);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(47).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            47
        );

        let error = supervisor
            .update_block_device(&replacement_config, true, None)
            .expect_err("unknown active drive should fail");

        assert_eq!(
            error,
            DriveUpdateError::UnknownDrive {
                drive_id: "missing".to_string()
            }
        );
        assert_eq!(control.request_wakeup_count(), 1);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_maps_full_drive_update_command_queue() {
        let original =
            TempFilePath::create_with_bytes("run-loop-drive-queue-original", &[0x11; 512]);
        let replacement =
            TempFilePath::create_with_bytes("run-loop-drive-queue-replacement", &[0x22; 1024]);
        let (updater, _original_config) = block_device_updater_fixture("data", original.path());
        let replacement_config = drive_config("data", replacement.path());
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["data"]);
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_block_device_updater(updater)
                .with_block_device_metrics(metrics);

        let mut supervisor = BootRunLoopSupervisor::start_with_command_queue_capacity(
            session,
            NonZeroUsize::new(53).expect("non-zero limit"),
            0,
        )
        .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            53
        );

        let error = supervisor
            .update_block_device(&replacement_config, true, None)
            .expect_err("full queue should fail");

        assert_eq!(
            error,
            DriveUpdateError::ActiveSessionCommand {
                message: "boot run loop command queue is full".to_string()
            }
        );
        let diagnostics = supervisor.metrics_diagnostics();
        assert_eq!(control.request_wakeup_count(), 0);
        assert_eq!(
            diagnostics.block_device_metrics(),
            Some(BlockDeviceMetrics::default().with_update_fails(1))
        );
        assert_eq!(
            diagnostics.block_device_metrics_by_drive(),
            Some(
                &BlockDeviceMetricsByDrive::new().with_drive_metrics(
                    "data",
                    BlockDeviceMetrics::default().with_update_fails(1),
                )
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_open_backing_failure_does_not_queue_drive_update() {
        let original =
            TempFilePath::create_with_bytes("run-loop-drive-open-original", &[0x11; 512]);
        let missing = missing_temp_child_path("run-loop-drive-open-missing");
        let (updater, _original_config) = block_device_updater_fixture("data", original.path());
        let missing_config = drive_config("data", &missing);
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["data"]);
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_block_device_updater(updater)
                .with_block_device_metrics(metrics);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(59).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            59
        );

        let error = supervisor
            .update_block_device(&missing_config, true, None)
            .expect_err("missing backing should fail before queueing command");

        assert!(matches!(
            error,
            DriveUpdateError::OpenBacking { drive_id, .. } if drive_id == "data"
        ));
        let diagnostics = supervisor.metrics_diagnostics();
        assert_eq!(control.request_wakeup_count(), 0);
        assert_eq!(
            diagnostics.block_device_metrics(),
            Some(BlockDeviceMetrics::default().with_update_fails(1))
        );
        assert_eq!(
            diagnostics.block_device_metrics_by_drive(),
            Some(
                &BlockDeviceMetricsByDrive::new().with_drive_metrics(
                    "data",
                    BlockDeviceMetrics::default().with_update_fails(1),
                )
            )
        );

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_keeps_status_running_across_step_limit() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_outcomes([
                    Ok(FakeRunLoopOutcome::StepLimitReached),
                    Ok(FakeRunLoopOutcome::Terminal),
                ])
                .with_wait_for_stop_sequence([false, true]);
        let run_count = session.run_count();

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(17).expect("non-zero limit"))
                .expect("supervisor should start");

        for _ in 0..2 {
            assert_eq!(
                max_steps_receiver
                    .recv()
                    .expect("worker should enter run loop"),
                17
            );
        }
        assert_eq!(supervisor.status(), BootRunLoopWorkerStatus::Running);
        assert_eq!(run_count.load(Ordering::SeqCst), 2);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_stops_after_run_loop_error() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_outcomes([
                    Err(FakeRunLoopError),
                    Ok(FakeRunLoopOutcome::StepLimitReached),
                ]);
        let run_count = session.run_count();

        let supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(13).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            13
        );

        drop(supervisor);

        assert_eq!(run_count.load(Ordering::SeqCst), 1);
        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn boot_run_loop_supervisor_records_error_status_before_release() {
        let control = FakeRunLoopControl::default();
        let drop_count = Arc::new(AtomicU64::new(0));
        let (max_steps_sender, max_steps_receiver) = mpsc::channel();
        let session =
            FakeRunLoopSession::new(control.clone(), Arc::clone(&drop_count), max_steps_sender)
                .with_wait_for_stop(false)
                .with_outcomes([Err(FakeRunLoopError)]);

        let mut supervisor =
            BootRunLoopSupervisor::start(session, NonZeroUsize::new(19).expect("non-zero limit"))
                .expect("supervisor should start");

        assert_eq!(
            max_steps_receiver
                .recv()
                .expect("worker should enter run loop"),
            19
        );
        assert_eq!(
            supervisor.wait_for_terminal_status(),
            BootRunLoopWorkerStatus::Failed("fake run loop failed".to_string())
        );
        assert_eq!(
            supervisor.metrics_diagnostics().boot_run_loop_status(),
            Some(BootRunLoopMetricStatus::Failed)
        );
        assert_eq!(
            supervisor.process_exit_status(),
            super::ProcessSessionExitStatus::Terminal
        );
        supervisor
            .drain_process_exit_wakeup()
            .expect("error wakeup should drain");
        assert_eq!(control.request_stop_count(), 0);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        drop(supervisor);

        assert_eq!(control.request_stop_count(), 1);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn instance_start_missing_boot_source_does_not_call_starter() {
        let mut vmm =
            ProcessVmm::with_starter("demo-1", "0.1.0", "bangbang", FakeStarter::success(1));

        let err = vmm
            .handle_action(VmmAction::InstanceStart)
            .expect_err("missing boot source should fail before execution");

        assert_eq!(err, VmmActionError::MissingBootSource);
        assert_eq!(vmm.starter.calls, 0);
        assert!(!vmm.has_started_session());
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn instance_start_success_commits_running_and_stores_session() {
        let mut vmm = configured_vmm(FakeStarter::success(7));

        let data = vmm
            .handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        assert_eq!(vmm.instance_info().state, InstanceState::Running);
        assert_eq!(vmm.starter.calls, 1);
        assert_eq!(vmm.started_session, Some(FakeSession::new(7)));
    }

    #[test]
    fn runtime_pause_updates_session_before_state_commit() {
        let mut vmm = configured_vmm(FakeStarter::success(8));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::Pause)
            .expect("running instance should pause");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        assert_eq!(vmm.instance_info().state, InstanceState::Paused);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.pause_count, 1);
        assert_eq!(session.resume_count, 0);
    }

    #[test]
    fn runtime_resume_updates_session_before_state_commit() {
        let mut vmm = configured_vmm(FakeStarter::success(9));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");
        vmm.handle_action(VmmAction::Pause)
            .expect("running instance should pause");

        let data = vmm
            .handle_action(VmmAction::Resume)
            .expect("paused instance should resume");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        assert_eq!(vmm.instance_info().state, InstanceState::Running);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.pause_count, 1);
        assert_eq!(session.resume_count, 1);
    }

    #[test]
    fn runtime_pause_failure_does_not_commit_state() {
        let source = BackendError::Hypervisor("pause failed".to_string());
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_pause_result(10, source.clone()),
        ));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::Pause)
            .expect_err("pause should fail before state commit");

        assert_eq!(err, VmmActionError::Lifecycle(source));
        assert_eq!(vmm.instance_info().state, InstanceState::Running);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.pause_count, 1);
        assert_eq!(session.resume_count, 0);
    }

    #[test]
    fn runtime_resume_failure_does_not_commit_state() {
        let source = BackendError::Hypervisor("resume failed".to_string());
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_resume_result(11, source.clone()),
        ));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");
        vmm.handle_action(VmmAction::Pause)
            .expect("running instance should pause");

        let err = vmm
            .handle_action(VmmAction::Resume)
            .expect_err("resume should fail before state commit");

        assert_eq!(err, VmmActionError::Lifecycle(source));
        assert_eq!(vmm.instance_info().state, InstanceState::Paused);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.pause_count, 1);
        assert_eq!(session.resume_count, 1);
    }

    #[test]
    fn runtime_pause_and_resume_reject_invalid_transitions_without_session_call() {
        let mut vmm = configured_vmm(FakeStarter::success(12));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::Resume)
            .expect_err("running instance should not resume again");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::Resume.name(),
                state: InstanceState::Running,
            }
        );
        assert_eq!(vmm.instance_info().state, InstanceState::Running);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.pause_count, 0);
        assert_eq!(session.resume_count, 0);

        vmm.handle_action(VmmAction::Pause)
            .expect("running instance should pause");
        let err = vmm
            .handle_action(VmmAction::Pause)
            .expect_err("paused instance should not pause again");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::Pause.name(),
                state: InstanceState::Paused,
            }
        );
        assert_eq!(vmm.instance_info().state, InstanceState::Paused);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.pause_count, 1);
        assert_eq!(session.resume_count, 0);
    }

    #[test]
    fn runtime_balloon_update_refreshes_active_session_before_config_commit() {
        let mut vmm = configured_vmm(FakeStarter::success(15));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::PatchBalloon(balloon_update_input(96)))
            .expect("runtime balloon target update should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_update_count, 1);
        assert_eq!(session.last_balloon_update_mib, Some(96));
        assert_eq!(
            vmm.handle_action(VmmAction::GetBalloon)
                .expect("balloon config should be returned"),
            bangbang_runtime::VmmData::BalloonConfiguration(BalloonConfig::from(
                BalloonConfigInput::new(96, false)
            ))
        );
    }

    #[test]
    fn runtime_balloon_update_failure_does_not_commit_config() {
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_update_result(
                16,
                BalloonUpdateError::ActiveSessionUnavailable,
            ),
        ));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchBalloon(balloon_update_input(96)))
            .expect_err("runtime balloon target update should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonUpdate(BalloonUpdateError::ActiveSessionUnavailable)
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_update_count, 1);
        assert_eq!(session.last_balloon_update_mib, Some(96));
        assert_eq!(
            vmm.handle_action(VmmAction::GetBalloon)
                .expect("balloon config should be returned"),
            bangbang_runtime::VmmData::BalloonConfiguration(BalloonConfig::from(
                BalloonConfigInput::new(64, false)
            ))
        );
    }

    #[test]
    fn runtime_memory_hotplug_update_refreshes_active_session_before_config_commit() {
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_memory_hotplug_status_result(
                28,
                Ok(memory_hotplug_status_with_plugged(128, 256)),
            ),
        ));
        vmm.handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("initial memory hotplug config should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::PatchMemoryHotplug(
                memory_hotplug_size_update_input(256),
            ))
            .expect("runtime memory hotplug update should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.memory_hotplug_update_count, 1);
        assert_eq!(session.last_memory_hotplug_requested_size_mib, Some(256));
        assert_eq!(
            vmm.handle_action(VmmAction::GetMemoryHotplug)
                .expect("memory hotplug status should be returned"),
            bangbang_runtime::VmmData::MemoryHotplugStatus(memory_hotplug_status_with_plugged(
                128, 256
            ))
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.memory_hotplug_status_count, 1);
        assert_eq!(
            session.last_memory_hotplug_status_requested_size_mib,
            Some(256)
        );
    }

    #[test]
    fn runtime_memory_hotplug_update_failure_does_not_commit_status() {
        let expected_error = MemoryHotplugUpdateError::ActiveSessionUnavailable;
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_memory_hotplug_update_result(29, expected_error.clone()),
        ));
        vmm.handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("initial memory hotplug config should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchMemoryHotplug(
                memory_hotplug_size_update_input(256),
            ))
            .expect_err("runtime memory hotplug update should fail");

        assert_eq!(err, VmmActionError::MemoryHotplugUpdate(expected_error));
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.memory_hotplug_update_count, 1);
        assert_eq!(session.last_memory_hotplug_requested_size_mib, Some(256));
        assert_eq!(
            vmm.handle_action(VmmAction::GetMemoryHotplug)
                .expect("memory hotplug status should be returned"),
            bangbang_runtime::VmmData::MemoryHotplugStatus(memory_hotplug_status(0))
        );
    }

    #[test]
    fn runtime_memory_hotplug_status_failure_returns_status_fault() {
        let expected_error = MemoryHotplugStatusError::ActiveSessionUnavailable;
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_memory_hotplug_status_result(30, Err(expected_error.clone())),
        ));
        vmm.handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("initial memory hotplug config should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::GetMemoryHotplug)
            .expect_err("active memory hotplug status query should fail");

        assert_eq!(err, VmmActionError::MemoryHotplugStatus(expected_error));
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.memory_hotplug_status_count, 1);
        assert_eq!(
            session.last_memory_hotplug_status_requested_size_mib,
            Some(0)
        );
    }

    #[test]
    fn runtime_balloon_stats_update_refreshes_active_session_before_config_commit() {
        let mut vmm = configured_vmm(FakeStarter::success(18));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_stats_polling_interval_s(60),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(30)))
            .expect("runtime balloon stats interval update should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_stats_update_count, 1);
        assert_eq!(session.last_balloon_stats_update_interval_s, Some(30));
        assert_eq!(
            vmm.handle_action(VmmAction::GetBalloon)
                .expect("balloon config should be returned"),
            bangbang_runtime::VmmData::BalloonConfiguration(BalloonConfig::from(
                BalloonConfigInput::new(64, false).with_stats_polling_interval_s(30)
            ))
        );
    }

    #[test]
    fn runtime_balloon_stats_update_failure_does_not_commit_config() {
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_stats_update_result(
                19,
                BalloonUpdateError::ActiveSessionUnavailable,
            ),
        ));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_stats_polling_interval_s(60),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(30)))
            .expect_err("runtime balloon stats interval update should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonUpdate(BalloonUpdateError::ActiveSessionUnavailable)
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_stats_update_count, 1);
        assert_eq!(session.last_balloon_stats_update_interval_s, Some(30));
        assert_eq!(
            vmm.handle_action(VmmAction::GetBalloon)
                .expect("balloon config should be returned"),
            bangbang_runtime::VmmData::BalloonConfiguration(BalloonConfig::from(
                BalloonConfigInput::new(64, false).with_stats_polling_interval_s(60)
            ))
        );
    }

    #[test]
    fn periodic_balloon_statistics_interval_requires_running_enabled_balloon() {
        let mut vmm = configured_vmm(FakeStarter::success(20));

        assert_eq!(vmm.balloon_statistics_update_interval(), None);
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("disabled statistics balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");
        assert_eq!(vmm.balloon_statistics_update_interval(), None);

        vmm.handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(1)))
            .expect_err("statistics cannot be enabled after activation");

        let mut enabled = configured_vmm(FakeStarter::success(21));
        enabled
            .handle_action(VmmAction::PutBalloon(
                BalloonConfigInput::new(64, false).with_stats_polling_interval_s(60),
            ))
            .expect("enabled statistics balloon should configure");
        assert_eq!(enabled.balloon_statistics_update_interval(), None);
        enabled
            .handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");
        assert_eq!(
            enabled.balloon_statistics_update_interval(),
            Some(Duration::from_secs(60))
        );
        enabled
            .handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(30)))
            .expect("runtime statistics interval update should succeed");
        assert_eq!(
            enabled.balloon_statistics_update_interval(),
            Some(Duration::from_secs(30))
        );
        enabled
            .handle_action(VmmAction::Pause)
            .expect("pause should succeed");
        assert_eq!(enabled.balloon_statistics_update_interval(), None);
        enabled
            .handle_action(VmmAction::Resume)
            .expect("resume should succeed");
        assert_eq!(
            enabled.balloon_statistics_update_interval(),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn periodic_balloon_statistics_update_triggers_active_session() {
        let mut vmm = configured_vmm(FakeStarter::success(22));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_stats_polling_interval_s(60),
        ))
        .expect("enabled statistics balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        assert_eq!(vmm.trigger_periodic_balloon_statistics_update(), Ok(true));
        assert_eq!(
            vmm.started_session
                .as_ref()
                .expect("started session should remain available")
                .balloon_stats_trigger_count,
            1
        );
    }

    #[test]
    fn periodic_balloon_statistics_update_is_noop_when_disabled() {
        let mut vmm = configured_vmm(FakeStarter::success(23));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("disabled statistics balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        assert_eq!(vmm.trigger_periodic_balloon_statistics_update(), Ok(false));
        assert_eq!(
            vmm.started_session
                .as_ref()
                .expect("started session should remain available")
                .balloon_stats_trigger_count,
            0
        );
    }

    #[test]
    fn periodic_balloon_statistics_update_propagates_session_failure() {
        let expected_error = BalloonUpdateError::ActiveSessionCommand {
            message: "stats trigger failed".to_string(),
        };
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_stats_trigger_result(24, expected_error.clone()),
        ));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_stats_polling_interval_s(60),
        ))
        .expect("enabled statistics balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        assert_eq!(
            vmm.trigger_periodic_balloon_statistics_update(),
            Err(VmmActionError::BalloonUpdate(expected_error))
        );
        assert_eq!(
            vmm.started_session
                .as_ref()
                .expect("started session should remain available")
                .balloon_stats_trigger_count,
            1
        );
    }

    #[test]
    fn runtime_balloon_stats_reads_active_session_with_current_config() {
        let mut vmm = configured_vmm(FakeStarter::success(17));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");
        vmm.handle_action(VmmAction::PatchBalloon(balloon_update_input(96)))
            .expect("runtime balloon target update should succeed");

        let data = vmm
            .handle_action(VmmAction::GetBalloonStats)
            .expect("balloon stats should be returned");

        assert_eq!(
            data,
            bangbang_runtime::VmmData::BalloonStatistics(
                BalloonStats::from_config_and_actual_pages(
                    BalloonConfigInput::new(96, false).into(),
                    0,
                )
                .expect("expected stats should convert")
            )
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_stats_count, 1);
        assert_eq!(session.last_balloon_stats_mib, Some(96));
    }

    #[test]
    fn runtime_balloon_stats_failure_does_not_mutate_config() {
        let expected_error = BalloonStatsError::ActualPageCountTooLarge {
            actual_pages: u64::from(u32::MAX) + 1,
        };
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_stats_result(18, Err(expected_error.clone())),
        ));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::GetBalloonStats)
            .expect_err("balloon stats should fail");

        assert_eq!(err, VmmActionError::BalloonStats(expected_error));
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_stats_count, 1);
        assert_eq!(session.last_balloon_stats_mib, Some(64));
        assert_eq!(
            vmm.handle_action(VmmAction::GetBalloon)
                .expect("balloon config should be returned"),
            bangbang_runtime::VmmData::BalloonConfiguration(BalloonConfig::from(
                BalloonConfigInput::new(64, false)
            ))
        );
    }

    #[test]
    fn runtime_balloon_hinting_status_reads_active_session_when_enabled() {
        let expected = BalloonHintingStatus::new(0, None);
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_hinting_status_result(19, Ok(expected)),
        ));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_free_page_hinting(true),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::GetBalloonHintingStatus)
            .expect("balloon hinting status should be returned");

        assert_eq!(
            data,
            bangbang_runtime::VmmData::BalloonHintingStatus(expected)
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_status_count, 1);
    }

    #[test]
    fn runtime_balloon_hinting_status_rejects_without_hinting_enabled() {
        let mut vmm = configured_vmm(FakeStarter::success(20));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::GetBalloonHintingStatus)
            .expect_err("balloon hinting status should require hinting support");

        assert_eq!(err, VmmActionError::BalloonUnsupported);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_status_count, 0);
    }

    #[test]
    fn runtime_balloon_hinting_status_maps_handler_hinting_disabled_to_unsupported() {
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_hinting_status_result(
                21,
                Err(BalloonHintingStatusError::HintingNotEnabled),
            ),
        ));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_free_page_hinting(true),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::GetBalloonHintingStatus)
            .expect_err("handler mismatch should map to existing balloon fault");

        assert_eq!(err, VmmActionError::BalloonUnsupported);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_status_count, 1);
    }

    #[test]
    fn runtime_balloon_hinting_start_reads_active_session_when_enabled() {
        let mut vmm = configured_vmm(FakeStarter::success(22));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_free_page_hinting(true),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::PatchBalloonHintingStart(
                BalloonHintingStartInput::new(false),
            ))
            .expect("balloon hinting start should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_start_count, 1);
        assert_eq!(session.last_balloon_hinting_start_ack, Some(false));
    }

    #[test]
    fn runtime_balloon_hinting_start_rejects_without_hinting_enabled() {
        let mut vmm = configured_vmm(FakeStarter::success(23));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchBalloonHintingStart(
                BalloonHintingStartInput::new(true),
            ))
            .expect_err("balloon hinting start should require hinting support");

        assert_eq!(err, VmmActionError::BalloonUnsupported);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_start_count, 0);
        assert_eq!(session.last_balloon_hinting_start_ack, None);
    }

    #[test]
    fn runtime_balloon_hinting_start_maps_handler_hinting_disabled_to_unsupported() {
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_hinting_start_result(
                24,
                Err(BalloonHintingCommandError::HintingNotEnabled),
            ),
        ));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_free_page_hinting(true),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchBalloonHintingStart(
                BalloonHintingStartInput::new(true),
            ))
            .expect_err("handler mismatch should map to existing balloon fault");

        assert_eq!(err, VmmActionError::BalloonUnsupported);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_start_count, 1);
        assert_eq!(session.last_balloon_hinting_start_ack, Some(true));
    }

    #[test]
    fn runtime_balloon_hinting_stop_reads_active_session_when_enabled() {
        let mut vmm = configured_vmm(FakeStarter::success(25));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_free_page_hinting(true),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::PatchBalloonHintingStop)
            .expect("balloon hinting stop should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_stop_count, 1);
    }

    #[test]
    fn runtime_balloon_hinting_stop_rejects_without_hinting_enabled() {
        let mut vmm = configured_vmm(FakeStarter::success(26));
        vmm.handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(64, false)))
            .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchBalloonHintingStop)
            .expect_err("balloon hinting stop should require hinting support");

        assert_eq!(err, VmmActionError::BalloonUnsupported);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_stop_count, 0);
    }

    #[test]
    fn runtime_balloon_hinting_stop_maps_handler_hinting_disabled_to_unsupported() {
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_balloon_hinting_stop_result(
                27,
                Err(BalloonHintingCommandError::HintingNotEnabled),
            ),
        ));
        vmm.handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(64, false).with_free_page_hinting(true),
        ))
        .expect("initial balloon should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::PatchBalloonHintingStop)
            .expect_err("handler mismatch should map to existing balloon fault");

        assert_eq!(err, VmmActionError::BalloonUnsupported);
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.balloon_hinting_stop_count, 1);
    }

    #[test]
    fn runtime_drive_update_refreshes_active_session_before_config_commit() {
        let original = TempFilePath::create("runtime-drive-original");
        let replacement = TempFilePath::create("runtime-drive-replacement");
        let mut vmm = configured_vmm(FakeStarter::success(11));
        vmm.handle_action(VmmAction::PutDrive(DriveConfigInput::new(
            "data",
            "data",
            original.path(),
            false,
        )))
        .expect("initial drive should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "data",
                "data",
                Some(replacement.path().to_path_buf()),
            )))
            .expect("runtime drive update should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        assert_eq!(vmm.drive_configs().len(), 1);
        assert_eq!(vmm.drive_configs()[0].path_on_host(), replacement.path());
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.block_update_count, 1);
        assert_eq!(session.last_block_update.as_deref(), Some("data"));
        assert_eq!(session.last_block_update_refresh_backing, Some(true));
        assert_eq!(session.last_block_update_rate_limiter, Some(None));
    }

    #[test]
    fn runtime_drive_update_without_path_is_noop_without_session_refresh() {
        let original = TempFilePath::create("runtime-drive-noop-original");
        let mut vmm = configured_vmm(FakeStarter::success(14));
        vmm.handle_action(VmmAction::PutDrive(DriveConfigInput::new(
            "data",
            "data",
            original.path(),
            false,
        )))
        .expect("initial drive should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let data = vmm
            .handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "data", "data", None,
            )))
            .expect("pathless runtime drive update should be a no-op");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        assert_eq!(vmm.drive_configs().len(), 1);
        assert_eq!(vmm.drive_configs()[0].path_on_host(), original.path());
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.block_update_count, 0);
        assert_eq!(session.last_block_update, None);
        assert_eq!(session.last_block_update_refresh_backing, None);
        assert_eq!(session.last_block_update_rate_limiter, None);
    }

    #[test]
    fn runtime_drive_rate_limiter_update_refreshes_active_session_without_backing() {
        let original = TempFilePath::create("runtime-drive-rate-limiter-original");
        let mut vmm = configured_vmm(FakeStarter::success(15));
        vmm.handle_action(VmmAction::PutDrive(DriveConfigInput::new(
            "data",
            "data",
            original.path(),
            false,
        )))
        .expect("initial drive should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let bandwidth = DriveTokenBucketConfig::new(1024, Some(2048), 100);
        let data = vmm
            .handle_action(VmmAction::UpdateBlockDevice(
                DriveUpdateInput::new("data", "data", None)
                    .with_rate_limiter(DriveRateLimiterConfig::new(Some(bandwidth), None)),
            ))
            .expect("runtime drive rate limiter update should succeed");

        assert_eq!(data, bangbang_runtime::VmmData::Empty);
        assert_eq!(vmm.drive_configs().len(), 1);
        assert_eq!(vmm.drive_configs()[0].path_on_host(), original.path());
        assert_eq!(
            vmm.drive_configs()[0].rate_limiter(),
            Some(DriveRateLimiterConfig::new(Some(bandwidth), None))
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.block_update_count, 1);
        assert_eq!(session.last_block_update.as_deref(), Some("data"));
        assert_eq!(session.last_block_update_refresh_backing, Some(false));
        assert_eq!(
            session.last_block_update_rate_limiter,
            Some(Some(DriveRateLimiterConfig::new(Some(bandwidth), None)))
        );
    }

    #[test]
    fn runtime_drive_update_failure_preserves_stored_config() {
        let original = TempFilePath::create("runtime-drive-failure-original");
        let replacement = TempFilePath::create("runtime-drive-failure-replacement");
        let mut vmm = configured_vmm(FakeStarter::success_with_session(
            FakeSession::with_block_update_result(12, DriveUpdateError::ActiveSessionUnavailable),
        ));
        vmm.handle_action(VmmAction::PutDrive(DriveConfigInput::new(
            "data",
            "data",
            original.path(),
            false,
        )))
        .expect("initial drive should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "data",
                "data",
                Some(replacement.path().to_path_buf()),
            )))
            .expect_err("failed session update should fail action");

        assert_eq!(
            err,
            VmmActionError::DriveUpdate(DriveUpdateError::ActiveSessionUnavailable)
        );
        assert_eq!(vmm.drive_configs().len(), 1);
        assert_eq!(vmm.drive_configs()[0].path_on_host(), original.path());
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.block_update_count, 1);
        assert_eq!(session.last_block_update.as_deref(), Some("data"));
        assert_eq!(session.last_block_update_refresh_backing, Some(true));
        assert_eq!(session.last_block_update_rate_limiter, Some(None));
    }

    #[test]
    fn runtime_drive_update_unknown_drive_does_not_touch_session() {
        let replacement = TempFilePath::create("runtime-drive-unknown-replacement");
        let mut vmm = configured_vmm(FakeStarter::success(13));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        let err = vmm
            .handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "missing",
                "missing",
                Some(replacement.path().to_path_buf()),
            )))
            .expect_err("unknown drive should fail");

        assert_eq!(
            err,
            VmmActionError::DriveUpdate(DriveUpdateError::UnknownDrive {
                drive_id: "missing".to_string()
            })
        );
        let session = vmm
            .started_session
            .as_ref()
            .expect("started session should remain available");
        assert_eq!(session.block_update_count, 0);
        assert_eq!(session.last_block_update, None);
        assert_eq!(session.last_block_update_refresh_backing, None);
        assert_eq!(session.last_block_update_rate_limiter, None);
    }

    #[test]
    fn flush_metrics_includes_started_session_diagnostics() {
        let metrics = TempFilePath::create("metrics");
        let process_diagnostics = MetricsDiagnostics::new()
            .with_start_time_us(1000)
            .with_parent_cpu_time_us(3000);
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            "0.1.0",
            "bangbang",
            DiagnosticStarter::new(BootRunLoopMetricStatus::Failed),
        )
        .with_process_metrics_diagnostics(process_diagnostics);
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            metrics.path(),
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("metrics should flush");

        assert_eq!(vmm.starter.calls, 1);
        assert_eq!(
            fs::read_to_string(metrics.path()).expect("metrics output should read"),
            "{\"api_server\":{\"process_startup_time_us\":1000},\"vmm\":{\"boot_run_loop_status\":\"failed\",\"metrics_flush_count\":1}}\n"
        );
    }

    #[test]
    fn flush_metrics_includes_process_signal_metrics() {
        let metrics = TempFilePath::create("metrics");
        let signal_metrics = SharedSignalMetrics::default();
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            "0.1.0",
            "bangbang",
            DiagnosticStarter::new(BootRunLoopMetricStatus::Running),
        )
        .with_process_signal_metrics(signal_metrics.clone());
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            metrics.path(),
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        signal_metrics.record_sigpipe();
        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("metrics should flush");

        assert_eq!(
            fs::read_to_string(metrics.path()).expect("metrics output should read"),
            "{\"signals\":{\"sigpipe\":1},\"vmm\":{\"boot_run_loop_status\":\"running\",\"metrics_flush_count\":1}}\n"
        );
    }

    #[test]
    fn flush_metrics_includes_starter_serial_output_diagnostics() {
        let metrics = TempFilePath::create("metrics");
        let starter_diagnostics = MetricsDiagnostics::new().with_serial_output_metrics(
            SerialOutputMetrics::default().with_rate_limiter_dropped_bytes(2),
        );
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            "0.1.0",
            "bangbang",
            DiagnosticStarter::new(BootRunLoopMetricStatus::Running)
                .with_metrics_diagnostics(starter_diagnostics),
        );
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            metrics.path(),
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("metrics should flush");

        assert_eq!(
            fs::read_to_string(metrics.path()).expect("metrics output should read"),
            "{\"uart\":{\"error_count\":0,\"flush_count\":0,\"missed_read_count\":0,\"missed_write_count\":0,\"rate_limiter_dropped_bytes\":2,\"read_count\":0,\"write_count\":0},\"vmm\":{\"boot_run_loop_status\":\"running\",\"metrics_flush_count\":1}}\n"
        );
    }

    #[test]
    fn periodic_metrics_flush_includes_diagnostics_without_logger_action() {
        let metrics = TempFilePath::create("periodic-metrics");
        let logger = TempFilePath::create("periodic-logger");
        let process_diagnostics = MetricsDiagnostics::new()
            .with_start_time_us(1000)
            .with_parent_cpu_time_us(3000);
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            "0.1.0",
            "bangbang",
            DiagnosticStarter::new(BootRunLoopMetricStatus::Failed),
        )
        .with_process_metrics_diagnostics(process_diagnostics);
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            metrics.path(),
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutLogger(
            LoggerConfigInput::new().with_log_path(logger.path()),
        ))
        .expect("logger should configure");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        assert_eq!(vmm.flush_periodic_metrics(), Ok(true));

        assert_eq!(vmm.starter.calls, 1);
        assert_eq!(
            fs::read_to_string(metrics.path()).expect("metrics output should read"),
            "{\"api_server\":{\"process_startup_time_us\":1000},\"vmm\":{\"boot_run_loop_status\":\"failed\",\"metrics_flush_count\":1}}\n"
        );
        assert_eq!(
            fs::read_to_string(logger.path()).expect("logger output should read"),
            "action=InstanceStart\n"
        );
    }

    #[test]
    fn direct_actions_do_not_record_api_request_metrics() {
        let metrics = TempFilePath::create("direct-metrics");
        let logger = TempFilePath::create("direct-logger");
        let serial = TempFilePath::create("direct-serial");
        let drive = TempFilePath::create("direct-drive");
        let vsock = TempFilePath::create("direct-vsock");
        let mut vmm =
            ProcessVmm::with_starter("demo-1", "0.1.0", "bangbang", FakeStarter::success(3));
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            metrics.path(),
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("machine config should configure");
        vmm.handle_action(VmmAction::PatchMachineConfig(
            MachineConfigPatchInput::new().with_mem_size_mib(512),
        ))
        .expect("machine config patch should configure");
        vmm.handle_action(VmmAction::PutLogger(
            LoggerConfigInput::new().with_log_path(logger.path()),
        ))
        .expect("logger should configure");
        vmm.handle_action(VmmAction::PutSerial(
            SerialConfigInput::new().with_serial_out_path(serial.path().to_string_lossy()),
        ))
        .expect("serial should configure");
        vmm.handle_action(VmmAction::PutDrive(DriveConfigInput::new(
            "data",
            "data",
            drive.path(),
            false,
        )))
        .expect("drive should configure");
        vmm.handle_action(VmmAction::PutNetworkInterface(
            NetworkInterfaceConfigInput::new("eth0", "eth0", "vmnet:shared"),
        ))
        .expect("network interface should configure");
        vmm.handle_action(VmmAction::PutMmdsConfig(MmdsConfigInput::new(vec![
            "eth0".to_string(),
        ])))
        .expect("MMDS config should configure");
        vmm.handle_action(VmmAction::PutMmds(MmdsContentInput::new(
            serde_json::json!({"latest": {"meta-data": {}}}),
        )))
        .expect("MMDS data should configure");
        vmm.handle_action(VmmAction::PatchMmds(MmdsContentInput::new(
            serde_json::json!({"latest": {"meta-data": {}}}),
        )))
        .expect("MMDS data patch should configure");
        vmm.handle_action(VmmAction::PutVsock(VsockConfigInput::new(
            3,
            vsock.path().to_string_lossy(),
        )))
        .expect("vsock should configure");
        assert_eq!(
            vmm.handle_action(VmmAction::PutCpuConfig(
                CpuConfigInput::with_custom_template()
            )),
            Err(VmmActionError::UnsupportedAction("PutCpuConfig"))
        );
        assert_eq!(
            vmm.handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "data", "data", None,
            ))),
            Err(VmmActionError::UnsupportedState {
                action: "UpdateBlockDevice",
                state: InstanceState::NotStarted,
            })
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("startup should succeed");

        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("metrics should flush");

        assert_eq!(
            fs::read_to_string(metrics.path()).expect("metrics output should read"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
    }

    #[test]
    fn instance_start_failure_keeps_not_started_without_session() {
        let source = BackendError::InvalidState("test startup failed");
        let mut vmm = configured_vmm(FakeStarter::failure(source.clone()));

        let err = vmm
            .handle_action(VmmAction::InstanceStart)
            .expect_err("startup failure should propagate");

        assert_eq!(err, VmmActionError::InstanceStart(source));
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert_eq!(vmm.starter.calls, 1);
        assert!(!vmm.has_started_session());
    }

    #[test]
    fn instance_start_after_success_fails_before_calling_starter_again() {
        let mut vmm = configured_vmm(FakeStarter::success(9));
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("first startup should succeed");

        let err = vmm
            .handle_action(VmmAction::InstanceStart)
            .expect_err("second startup should be rejected by state");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::InstanceStart.name(),
                state: InstanceState::Running,
            }
        );
        assert_eq!(vmm.starter.calls, 1);
        assert_eq!(vmm.started_session, Some(FakeSession::new(9)));
    }
}
