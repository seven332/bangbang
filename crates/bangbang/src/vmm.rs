use std::fmt;
use std::num::NonZeroUsize;
#[cfg(test)]
use std::sync::Condvar;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};

use bangbang_hvf::{
    HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError, HvfArm64BootRunLoopOutcome,
    HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
    HvfVcpuRunnerError, OwnedHvfArm64BootSession,
};
use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::memory::{GuestAddress, GuestMemory};
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::network::{
    NetworkInterfaceConfig, NetworkMmioLayout, VirtioNetworkRxPacket, VirtioNetworkRxPacketSource,
    VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame, VirtioNetworkTxPacketSink,
    VirtioNetworkTxPacketSinkError,
};
use bangbang_runtime::serial::SharedSerialOutputBuffer;
use bangbang_runtime::startup::{
    Arm64BootNetworkDevice, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
};
use bangbang_runtime::{BackendError, VmmAction, VmmActionError, VmmController, VmmData};

use crate::host_network::virtio_vmnet::{
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

const DEFAULT_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x5000_0000);
const DEFAULT_BLOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1);
const DEFAULT_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x6000_0000);
const DEFAULT_NETWORK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1000);
const DEFAULT_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
const DEFAULT_SERIAL_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(0);
const DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT: usize = 1024;
const HVF_BOOT_RUN_LOOP_THREAD_NAME: &str = "bangbang-hvf-boot-loop";

pub(crate) trait InstanceStartExecutor {
    type Session;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError>;
}

pub(crate) trait VmmRequestHandler {
    fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError>;
}

#[derive(Debug)]
pub(crate) struct ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    controller: VmmController,
    starter: S,
    started_session: Option<S::Session>,
}

impl ProcessVmm<HvfInstanceStartExecutor> {
    pub(crate) fn new(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
    ) -> Self {
        Self::with_starter(
            instance_id,
            vmm_version,
            app_name,
            HvfInstanceStartExecutor::default(),
        )
    }
}

impl<S> ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    pub(crate) fn with_starter(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
        starter: S,
    ) -> Self {
        Self {
            controller: VmmController::new(instance_id, vmm_version, app_name),
            starter,
            started_session: None,
        }
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

    pub(crate) fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        match action {
            VmmAction::InstanceStart => self.start_instance(),
            action => self.controller.handle_action(action),
        }
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
}

impl<S> VmmRequestHandler for ProcessVmm<S>
where
    S: InstanceStartExecutor,
{
    fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        ProcessVmm::handle_action(self, action)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HvfInstanceStartExecutor {
    serial_output: SharedSerialOutputBuffer,
}

impl HvfInstanceStartExecutor {
    fn boot_session_config(&self) -> HvfArm64BootSessionConfig {
        default_hvf_boot_session_config(self.serial_output.clone())
    }
}

impl InstanceStartExecutor for HvfInstanceStartExecutor {
    type Session = HvfBootRunLoopSupervisor;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError> {
        let packet_io = ProcessNetworkPacketIoProvider::from_network_configs(
            controller.network_interface_configs(),
        )
        .map_err(|err| {
            BackendError::Hypervisor(format!(
                "failed to build network packet I/O provider: {err}"
            ))
        })?;
        let session = OwnedHvfArm64BootSession::new(controller, self.boot_session_config())
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
    Vmnet(SystemProcessVmnetPacketIoProvider),
}

impl ProcessNetworkPacketIoProvider {
    fn from_network_configs(
        configs: &[NetworkInterfaceConfig],
    ) -> Result<Self, ProcessNetworkPacketIoProviderBuildError> {
        if configs.is_empty() {
            return Ok(Self::Noop(NoopProcessNetworkPacketIoProvider::default()));
        }

        let mut factory = SystemProcessVmnetPacketIoBackendFactory;
        process_vmnet_packet_io_provider_from_configs(configs, &mut factory).map(Self::Vmnet)
    }
}

impl Arm64BootNetworkPacketIoProvider for ProcessNetworkPacketIoProvider {
    fn packet_io(
        &mut self,
        device: &Arm64BootNetworkDevice,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
        match self {
            Self::Noop(provider) => provider.packet_io(device),
            Self::Vmnet(provider) => provider.packet_io(device),
        }
    }
}

#[derive(Debug)]
enum ProcessNetworkPacketIoProviderBuildError {
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
    ProviderBuild {
        source: VmnetVirtioNetworkPacketIoProviderBuildError,
    },
}

impl fmt::Display for ProcessNetworkPacketIoProviderBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::ProviderBuild { source } => {
                write!(f, "failed to build vmnet packet I/O provider: {source}")
            }
        }
    }
}

impl std::error::Error for ProcessNetworkPacketIoProviderBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HostDeviceName { source, .. } => Some(source),
            Self::Start { source, .. } => Some(source),
            Self::PacketIoBuild { source, .. } => Some(source),
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
        let packet_io = VmnetVirtioNetworkPacketIo::new(backend, interface).map_err(|source| {
            ProcessNetworkPacketIoProviderBuildError::PacketIoBuild {
                iface_id: iface_id.to_string(),
                source,
            }
        })?;

        entries.push(VmnetVirtioNetworkPacketIoProviderEntry::new(
            iface_id, packet_io,
        ));
    }

    VmnetVirtioNetworkPacketIoProvider::new(entries)
        .map_err(|source| ProcessNetworkPacketIoProviderBuildError::ProviderBuild { source })
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
        let (session_release_sender, session_release_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(HVF_BOOT_RUN_LOOP_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = loop {
                    match session.run_loop(&stop_token, max_steps) {
                        Ok(outcome) if S::should_continue_after_outcome(&outcome) => continue,
                        Ok(outcome) => {
                            worker_status.record(BootRunLoopWorkerStatus::Exited(outcome.clone()));
                            break Ok(outcome);
                        }
                        Err(err) => {
                            worker_status.record(BootRunLoopWorkerStatus::Failed(err.to_string()));
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

fn default_hvf_boot_session_config(
    serial_output: SharedSerialOutputBuffer,
) -> HvfArm64BootSessionConfig {
    HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(DEFAULT_BLOCK_MMIO_BASE, DEFAULT_BLOCK_MMIO_REGION_ID),
        NetworkMmioLayout::new(DEFAULT_NETWORK_MMIO_BASE, DEFAULT_NETWORK_MMIO_REGION_ID),
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
    use std::fs::{File, remove_file};
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};

    use bangbang_runtime::block::{DriveConfigInput, DriveConfigs, PreparedBlockDevices};
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::mmio::MmioRegion;
    use bangbang_runtime::network::{
        NetworkInterfaceConfig, NetworkInterfaceConfigInput, NetworkInterfaceConfigs,
        NetworkMmioLayout, PreparedNetworkDevices,
    };
    use bangbang_runtime::serial::{
        SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::startup::{
        Arm64BootNetworkDevice, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
        Arm64BootNetworkPacketIoProvider,
    };
    use bangbang_runtime::{BackendError, InstanceState, VmmAction, VmmActionError};

    use crate::host_network::vmnet::{
        VmnetError, VmnetInterfaceBackend, VmnetInterfaceConfig, VmnetInterfaceDescriptor,
        VmnetInterfaceDescriptorError, VmnetOperation, VmnetPacketIoBackend, VmnetPacketIoError,
        VmnetReadPacket, VmnetStatus, VmnetWritePacket,
    };

    use super::{
        BootRunLoopControl, BootRunLoopSession, BootRunLoopSupervisor, BootRunLoopWorkerStatus,
        DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT, DEFAULT_NETWORK_MMIO_BASE,
        DEFAULT_NETWORK_MMIO_REGION_ID, DEFAULT_SERIAL_MMIO_BASE, DEFAULT_SERIAL_MMIO_REGION_ID,
        EmptyProcessNetworkRxPacketSource, HvfInstanceStartExecutor, InstanceStartExecutor,
        NetworkPacketIoRunLoopSession, NoopProcessNetworkTxPacketSink, ProcessHvfBootSession,
        ProcessNetworkPacketIoProvider, ProcessNetworkPacketIoProviderBuildError, ProcessVmm,
        ProcessVmnetPacketIoBackendFactory, default_hvf_boot_run_loop_step_limit,
        default_hvf_boot_session_config, process_vmnet_packet_io_provider_from_configs,
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

    impl Drop for TempFilePath {
        fn drop(&mut self) {
            let _ = remove_file(&self.path);
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeSession {
        id: u64,
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

        let config = default_hvf_boot_session_config(SharedSerialOutputBuffer::default());
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
        let block_devices = PreparedBlockDevices::from_configs(&drives)
            .expect("block devices should prepare")
            .register_mmio(config.block_mmio_layout)
            .expect("block MMIO should register");
        let network_devices = PreparedNetworkDevices::from_configs(&networks)
            .expect("network devices should prepare")
            .register_mmio(config.network_mmio_layout)
            .expect("network MMIO should register");
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
                .all(|registration| registration.region_id() != serial_region_id)
        );
        assert!(
            network_devices
                .registrations()
                .iter()
                .all(|registration| registration.region_id() != serial_region_id)
        );
        assert!(block_devices.registrations().iter().all(|block| {
            network_devices
                .registrations()
                .iter()
                .all(|network| !block.region().range().overlaps(network.region().range()))
                && !block.region().range().overlaps(serial_region.range())
        }));
        assert!(
            network_devices
                .registrations()
                .iter()
                .all(|network| !network.region().range().overlaps(serial_region.range()))
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
            ProcessNetworkPacketIoProvider::Vmnet(_) => {
                panic!("empty network configs should not build a vmnet provider");
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
            ProcessNetworkPacketIoProviderBuildError::Start { .. }
            | ProcessNetworkPacketIoProviderBuildError::PacketIoBuild { .. }
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
            ProcessNetworkPacketIoProviderBuildError::Start { .. }
            | ProcessNetworkPacketIoProviderBuildError::PacketIoBuild { .. }
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
            | VmmActionError::MissingBootSource
            | VmmActionError::BootSourceConfig(_)
            | VmmActionError::DriveConfig(_)
            | VmmActionError::LoggerConfig(_)
            | VmmActionError::MachineConfig(_)
            | VmmActionError::MetricsConfig(_)
            | VmmActionError::MetricsFlush(_)
            | VmmActionError::NetworkInterfaceConfig(_)
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

        let supervisor =
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

        let supervisor =
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
