use std::fmt;
use std::io::Read;
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::num::NonZeroUsize;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
#[cfg(test)]
use std::sync::Condvar;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};

use bangbang_hvf::{
    HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError, HvfArm64BootRunLoopOutcome,
    HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
    HvfVcpuRunnerError, OwnedHvfArm64BootSession,
};
use bangbang_runtime::block::{BlockMmioLayout, DriveConfigInput, DriveUpdateInput};
use bangbang_runtime::boot::BootSourceConfigInput;
use bangbang_runtime::cpu::CpuConfigInput;
use bangbang_runtime::logger::LoggerConfigInput;
use bangbang_runtime::machine::{MachineConfigInput, MachineConfigPatchInput};
use bangbang_runtime::memory::{GuestAddress, GuestMemory};
use bangbang_runtime::metrics::{BootRunLoopMetricStatus, MetricsConfigInput, MetricsDiagnostics};
use bangbang_runtime::mmds::{
    MmdsConfig, MmdsConfigInput, MmdsContentInput, MmdsStateHandle, MmdsStateLockError,
};
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::network::{
    NetworkInterfaceConfig, NetworkInterfaceConfigError, NetworkInterfaceConfigInput,
    NetworkInterfaceUpdateInput, NetworkMmioLayout, VirtioNetworkRxPacket,
    VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame,
    VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError, validate_network_interface_count,
};
use bangbang_runtime::serial::{
    SerialConfigError, SerialConfigInput, SerialOutputFile, SharedSerialOutput,
    SharedSerialOutputBuffer,
};
use bangbang_runtime::startup::{
    Arm64BootNetworkDevice, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
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
use bangbang_runtime::block::DriveConfig;
#[cfg(test)]
use bangbang_runtime::boot::BootSourceConfig;
#[cfg(test)]
use bangbang_runtime::machine::MachineConfig;
#[cfg(test)]
use bangbang_runtime::serial::SerialConfig;

const DEFAULT_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x5000_0000);
const DEFAULT_BLOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1);
const DEFAULT_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x6000_0000);
const DEFAULT_NETWORK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1000);
const DEFAULT_VSOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x7000_0000);
const DEFAULT_VSOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(2000);
const DEFAULT_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
const DEFAULT_SERIAL_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(0);
const DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT: usize = 1024;
const HVF_BOOT_RUN_LOOP_THREAD_NAME: &str = "bangbang-hvf-boot-loop";

pub(crate) trait InstanceStartExecutor {
    type Session: ProcessSessionDiagnostics;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError>;
}

pub(crate) trait ProcessSessionDiagnostics {
    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        MetricsDiagnostics::default()
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
    HotplugMemory,
    InstanceInfo,
    VmmVersion,
    MachineConfig,
    Mmds,
}

impl GetApiRequest {
    const fn action(self) -> VmmAction {
        match self {
            Self::HotplugMemory => VmmAction::GetMemoryHotplug,
            Self::InstanceInfo => VmmAction::GetVmInstanceInfo,
            Self::VmmVersion => VmmAction::GetVmmVersion,
            Self::MachineConfig => VmmAction::GetMachineConfig,
            Self::Mmds => VmmAction::GetMmds,
        }
    }

    fn record(self, controller: &mut VmmController) {
        match self {
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

    pub(crate) const fn memory_hotplug() -> Self {
        Self {
            kind: PutApiRequestKind::HotplugMemory,
            action: VmmAction::PutMemoryHotplug,
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

    pub(crate) const fn pmem() -> Self {
        Self {
            kind: PutApiRequestKind::Pmem,
            action: VmmAction::PutPmem,
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
}

#[derive(Debug)]
pub(crate) struct PatchApiRequest {
    kind: PatchApiRequestKind,
    action: VmmAction,
}

impl PatchApiRequest {
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

    pub(crate) const fn memory_hotplug() -> Self {
        Self {
            kind: PatchApiRequestKind::HotplugMemory,
            action: VmmAction::PatchMemoryHotplug,
        }
    }

    pub(crate) fn network(input: NetworkInterfaceUpdateInput) -> Self {
        Self {
            kind: PatchApiRequestKind::Network,
            action: VmmAction::UpdateNetworkInterface(input),
        }
    }

    pub(crate) const fn pmem() -> Self {
        Self {
            kind: PatchApiRequestKind::Pmem,
            action: VmmAction::PatchPmem,
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
enum PatchApiRequestKind {
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
            Self::Drive => controller.record_patch_drive_failure(),
            Self::HotplugMemory => controller.record_patch_hotplug_memory_failure(),
            Self::MachineConfig => controller.record_patch_machine_config_failure(),
            Self::Mmds => controller.record_patch_mmds_failure(),
            Self::Network => controller.record_patch_network_failure(),
            Self::Pmem => controller.record_patch_pmem_failure(),
        }
    }
}

pub(crate) trait VmmRequestHandler {
    fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError>;

    fn handle_get_request(&mut self, request: GetApiRequest) -> Result<VmmData, VmmActionError>;

    fn handle_patch_request(&mut self, request: PatchApiRequest)
    -> Result<VmmData, VmmActionError>;

    fn handle_put_request(&mut self, request: PutApiRequest) -> Result<VmmData, VmmActionError>;

    fn handle_put_action_request(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError>;

    fn record_deprecated_api_call(&mut self);

    fn handle_periodic_metrics_flush(&mut self) -> Result<bool, VmmActionError> {
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
        }
    }

    pub(crate) fn with_process_metrics_diagnostics(
        mut self,
        diagnostics: MetricsDiagnostics,
    ) -> Self {
        self.process_metrics_diagnostics = diagnostics;
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

    fn record_deprecated_api_call(&mut self) {
        self.controller.record_deprecated_api_call();
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

    fn metrics_diagnostics(&self) -> MetricsDiagnostics {
        let session_diagnostics = self
            .started_session
            .as_ref()
            .map(ProcessSessionDiagnostics::metrics_diagnostics)
            .unwrap_or_default();
        self.process_metrics_diagnostics
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

    fn record_deprecated_api_call(&mut self) {
        ProcessVmm::record_deprecated_api_call(self);
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
    serial_output: SharedSerialOutputBuffer,
}

impl HvfInstanceStartExecutor {
    #[cfg(test)]
    fn boot_session_config(&self) -> HvfArm64BootSessionConfig {
        default_hvf_boot_session_config(SharedSerialOutput::from(self.serial_output.clone()))
    }

    fn boot_session_config_for_controller(
        &self,
        controller: &VmmController,
    ) -> Result<HvfArm64BootSessionConfig, SerialConfigError> {
        let serial_output = match controller.serial_config().serial_out_path() {
            Some(path) => SharedSerialOutput::new(SerialOutputFile::open(path)?),
            None => SharedSerialOutput::from(self.serial_output.clone()),
        };

        Ok(default_hvf_boot_session_config(serial_output))
    }
}

impl InstanceStartExecutor for HvfInstanceStartExecutor {
    type Session = HvfBootRunLoopSupervisor;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError> {
        let boot_session_config = self
            .boot_session_config_for_controller(controller)
            .map_err(|err| {
                BackendError::Hypervisor(format!("failed to initialize serial output: {err}"))
            })?;
        let packet_io =
            ProcessNetworkPacketIoProvider::from_controller(controller).map_err(|err| {
                BackendError::Hypervisor(format!(
                    "failed to build network packet I/O provider: {err}"
                ))
            })?;
        let session = OwnedHvfArm64BootSession::new(controller, boot_session_config)
            .map_err(|err| BackendError::Hypervisor(err.to_string()))?;
        let session = ProcessHvfBootSession::new(session, packet_io);
        HvfBootRunLoopSupervisor::start(session, default_hvf_boot_run_loop_step_limit())
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
        matches!(outcome, HvfArm64BootRunLoopOutcome::StepLimitReached { .. })
    }
}

pub(crate) trait BootRunLoopControl: Clone + fmt::Debug + Send + Sync + 'static {
    type Error: fmt::Display + Send + 'static;
    type StopToken: Clone + Send + Sync + 'static;

    fn stop_token(&self) -> Self::StopToken;

    fn request_stop(&self) -> Result<(), Self::Error>;
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
}

pub(crate) trait BootRunLoopSession: Send + 'static {
    type Control: BootRunLoopControl;
    type Error: fmt::Display + Send + 'static;
    type Outcome: Clone + fmt::Debug + Send + 'static;

    fn run_loop_control(&self) -> Self::Control;

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

    fn run_loop(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
    ) -> Result<Self::Outcome, Self::Error> {
        OwnedHvfArm64BootSession::run_loop(self, stop_token, max_steps)
    }

    fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
        matches!(outcome, HvfArm64BootRunLoopOutcome::StepLimitReached { .. })
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
    Exited(O),
    Failed(String),
}

trait BootRunLoopProcessExit {
    fn process_exit_status(&self) -> ProcessSessionExitStatus;
}

impl BootRunLoopProcessExit for HvfArm64BootRunLoopOutcome {
    fn process_exit_status(&self) -> ProcessSessionExitStatus {
        match self {
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
        while matches!(&*current, BootRunLoopWorkerStatus::Running) {
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

pub(crate) struct BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    control: S::Control,
    status: Arc<BootRunLoopWorkerStatusCell<S::Outcome>>,
    terminal_wakeup_reader: UnixStream,
    session_release_sender: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<Result<S::Outcome, S::Error>>>,
}

impl<S> BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    fn start(mut session: S, max_steps: NonZeroUsize) -> Result<Self, BackendError> {
        let control = session.run_loop_control();
        let stop_token = control.stop_token();
        let status = Arc::new(BootRunLoopWorkerStatusCell::new());
        let worker_status = Arc::clone(&status);
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
                let result = loop {
                    match session.run_loop(&stop_token, max_steps) {
                        Ok(outcome) if S::should_continue_after_outcome(&outcome) => continue,
                        Ok(outcome) => {
                            worker_status.record(BootRunLoopWorkerStatus::Exited(outcome.clone()));
                            let _ = terminal_wakeup_writer.write_all(&[1]);
                            break Ok(outcome);
                        }
                        Err(err) => {
                            worker_status.record(BootRunLoopWorkerStatus::Failed(err.to_string()));
                            let _ = terminal_wakeup_writer.write_all(&[1]);
                            break Err(err);
                        }
                    }
                };
                let _ = session_release_receiver.recv();
                result
            })
            .map_err(|err| {
                BackendError::Hypervisor(format!("failed to spawn HVF boot run loop: {err}"))
            })?;

        Ok(Self {
            control,
            status,
            terminal_wakeup_reader,
            session_release_sender: Some(session_release_sender),
            worker: Some(worker),
        })
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
            BootRunLoopWorkerStatus::Exited(_) => BootRunLoopMetricStatus::Exited,
            BootRunLoopWorkerStatus::Failed(_) => BootRunLoopMetricStatus::Failed,
        }
    }

    fn stop_and_join(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };

        let stop_requested = self.control.request_stop().is_ok();
        drop(self.session_release_sender.take());

        // A stop error can mean an in-flight vCPU run was not canceled; avoid
        // turning cleanup into an unbounded join in that error path.
        if stop_requested || worker.is_finished() {
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
        MetricsDiagnostics::new().with_boot_run_loop_status(self.metric_status())
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
            .field("status", &self.status())
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
        NetworkMmioLayout::new(DEFAULT_NETWORK_MMIO_BASE, DEFAULT_NETWORK_MMIO_REGION_ID),
        VsockMmioLayout::new(DEFAULT_VSOCK_MMIO_BASE, DEFAULT_VSOCK_MMIO_REGION_ID),
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
    use std::fs::{self, File, remove_file};
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};

    use bangbang_runtime::block::{
        DriveConfigInput, DriveConfigs, DriveUpdateInput, PreparedBlockDevices,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::cpu::CpuConfigInput;
    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::logger::LoggerConfigInput;
    use bangbang_runtime::machine::{MachineConfigInput, MachineConfigPatchInput};
    use bangbang_runtime::metrics::{
        BootRunLoopMetricStatus, MetricsConfigInput, MetricsDiagnostics,
    };
    use bangbang_runtime::mmds::{MmdsConfigInput, MmdsContentInput, MmdsStateHandle};
    use bangbang_runtime::mmio::MmioRegion;
    use bangbang_runtime::network::{
        MAX_NETWORK_INTERFACE_COUNT, NetworkInterfaceConfig, NetworkInterfaceConfigError,
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioLayout,
        PreparedNetworkDevices,
    };
    use bangbang_runtime::serial::{
        SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialConfigInput, SerialOutput, SharedSerialOutput,
        SharedSerialOutputBuffer,
    };
    use bangbang_runtime::startup::{
        Arm64BootNetworkDevice, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
        Arm64BootNetworkPacketIoProvider,
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
        BootRunLoopControl, BootRunLoopSession, BootRunLoopSupervisor, BootRunLoopWorkerStatus,
        DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT, DEFAULT_NETWORK_MMIO_BASE,
        DEFAULT_NETWORK_MMIO_REGION_ID, DEFAULT_SERIAL_MMIO_BASE, DEFAULT_SERIAL_MMIO_REGION_ID,
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
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("bb-vmm-{}-{id}-{name}", std::process::id()));
            File::create(&path).expect("test backing file should be created");
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

    impl Drop for TempFilePath {
        fn drop(&mut self) {
            let _ = remove_file(&self.path);
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeSession {
        id: u64,
    }

    impl ProcessSessionDiagnostics for FakeSession {}

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
        calls: usize,
    }

    impl DiagnosticStarter {
        const fn new(status: BootRunLoopMetricStatus) -> Self {
            Self { status, calls: 0 }
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
    }

    #[derive(Debug, Clone)]
    enum FakeStartResult {
        Success(u64),
        Failure(BackendError),
    }

    #[derive(Debug, Clone)]
    struct FakeStarter {
        result: FakeStartResult,
        calls: usize,
    }

    impl FakeStarter {
        const fn success(id: u64) -> Self {
            Self {
                result: FakeStartResult::Success(id),
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
                FakeStartResult::Success(id) => Ok(FakeSession { id: *id }),
                FakeStartResult::Failure(source) => Err(source.clone()),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeRunLoopOutcome {
        StepLimitReached,
        Terminal,
    }

    impl super::BootRunLoopProcessExit for FakeRunLoopOutcome {
        fn process_exit_status(&self) -> super::ProcessSessionExitStatus {
            match self {
                Self::StepLimitReached => super::ProcessSessionExitStatus::Running,
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
    }

    impl FakeRunLoopControl {
        fn request_stop_count(&self) -> u64 {
            self.request_stop_count.load(Ordering::SeqCst)
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
            Ok(())
        }
    }

    struct FakeRunLoopSession {
        control: FakeRunLoopControl,
        drop_count: Arc<AtomicU64>,
        run_count: Arc<AtomicU64>,
        max_steps_sender: mpsc::Sender<usize>,
        outcomes: Arc<Mutex<VecDeque<Result<FakeRunLoopOutcome, FakeRunLoopError>>>>,
        wait_for_stop: bool,
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
                wait_for_stop: true,
                wait_for_stop_sequence: Arc::default(),
            }
        }

        fn run_count(&self) -> Arc<AtomicU64> {
            Arc::clone(&self.run_count)
        }

        fn with_outcomes(
            mut self,
            outcomes: impl IntoIterator<Item = Result<FakeRunLoopOutcome, FakeRunLoopError>>,
        ) -> Self {
            self.outcomes = Arc::new(Mutex::new(outcomes.into_iter().collect()));
            self
        }

        const fn with_wait_for_stop(mut self, wait_for_stop: bool) -> Self {
            self.wait_for_stop = wait_for_stop;
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
            self.outcomes
                .lock()
                .expect("fake outcomes should lock")
                .pop_front()
                .unwrap_or(Ok(FakeRunLoopOutcome::Terminal))
        }

        fn should_continue_after_outcome(outcome: &Self::Outcome) -> bool {
            matches!(outcome, FakeRunLoopOutcome::StepLimitReached)
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
            matches!(outcome, FakeRunLoopOutcome::StepLimitReached)
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

        let serial = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO");
        assert_eq!(serial.region_id, DEFAULT_SERIAL_MMIO_REGION_ID);
        assert_eq!(serial.address, DEFAULT_SERIAL_MMIO_BASE);

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
        let vsock_region = MmioRegion::new(
            config.vsock_mmio_layout.region_id(),
            config.vsock_mmio_layout.address(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("vsock MMIO region should be valid");
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
                    && registration.region_id() != vsock_region.id())
        );
        assert!(
            network_devices
                .registrations()
                .iter()
                .all(|registration| registration.region_id() != serial_region_id
                    && registration.region_id() != vsock_region.id())
        );
        assert_ne!(vsock_region.id(), serial_region_id);
        assert!(block_devices.registrations().iter().all(|block| {
            network_devices
                .registrations()
                .iter()
                .all(|network| !block.region().range().overlaps(network.region().range()))
                && !block.region().range().overlaps(serial_region.range())
                && !block.region().range().overlaps(vsock_region.range())
        }));
        assert!(network_devices.registrations().iter().all(|network| {
            !network.region().range().overlaps(serial_region.range())
                && !network.region().range().overlaps(vsock_region.range())
        }));
        assert!(!vsock_region.range().overlaps(serial_region.range()));
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
            | VmmActionError::BalloonUnsupported
            | VmmActionError::EntropyUnsupported
            | VmmActionError::MissingBootSource
            | VmmActionError::BootSourceConfig(_)
            | VmmActionError::DriveConfig(_)
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
            | VmmActionError::NetworkInterfaceUpdateUnsupported
            | VmmActionError::MemoryHotplugUnsupported
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
        assert_eq!(vmm.started_session, Some(FakeSession { id: 7 }));
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
            "{\"vmm\":{\"boot_run_loop_status\":\"failed\",\"metrics_flush_count\":1,\"parent_cpu_time_us\":3000,\"start_time_us\":1000}}\n"
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
            "{\"vmm\":{\"boot_run_loop_status\":\"failed\",\"metrics_flush_count\":1,\"parent_cpu_time_us\":3000,\"start_time_us\":1000}}\n"
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
        assert_eq!(vmm.started_session, Some(FakeSession { id: 9 }));
    }
}
