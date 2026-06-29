use std::fmt;
use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use bangbang_hvf::{
    HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError, HvfArm64BootRunLoopOutcome,
    HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
    HvfVcpuRunnerError, OwnedHvfArm64BootSession,
};
use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::serial::SharedSerialOutputBuffer;
use bangbang_runtime::{BackendError, VmmAction, VmmActionError, VmmController, VmmData};

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
        let session = OwnedHvfArm64BootSession::new(controller, self.boot_session_config())
            .map_err(|err| BackendError::Hypervisor(err.to_string()))?;
        HvfBootRunLoopSupervisor::start(session, default_hvf_boot_run_loop_step_limit())
    }
}

pub(crate) type HvfBootRunLoopSupervisor = BootRunLoopSupervisor<OwnedHvfArm64BootSession>;

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
    type Outcome: fmt::Debug + Send + 'static;

    fn run_loop_control(&self) -> Self::Control;

    fn run_loop(
        &mut self,
        stop_token: &<Self::Control as BootRunLoopControl>::StopToken,
        max_steps: NonZeroUsize,
    ) -> Result<Self::Outcome, Self::Error>;
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
}

pub(crate) struct BootRunLoopSupervisor<S>
where
    S: BootRunLoopSession,
{
    control: S::Control,
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
        let (session_release_sender, session_release_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(HVF_BOOT_RUN_LOOP_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = session.run_loop(&stop_token, max_steps);
                let _ = session_release_receiver.recv();
                result
            })
            .map_err(|err| {
                BackendError::Hypervisor(format!("failed to spawn HVF boot run loop: {err}"))
            })?;

        Ok(Self {
            control,
            session_release_sender: Some(session_release_sender),
            worker: Some(worker),
        })
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
    HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
        DEFAULT_BLOCK_MMIO_BASE,
        DEFAULT_BLOCK_MMIO_REGION_ID,
    ))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        DEFAULT_SERIAL_MMIO_REGION_ID,
        DEFAULT_SERIAL_MMIO_BASE,
        serial_output,
    ))
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::fs::{File, remove_file};
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};

    use bangbang_runtime::block::{DriveConfigInput, DriveConfigs, PreparedBlockDevices};
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::serial::{SerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::{BackendError, InstanceState, VmmAction, VmmActionError};

    use super::{
        BootRunLoopControl, BootRunLoopSession, BootRunLoopSupervisor,
        DEFAULT_HVF_BOOT_RUN_LOOP_STEP_LIMIT, DEFAULT_SERIAL_MMIO_BASE,
        DEFAULT_SERIAL_MMIO_REGION_ID, HvfInstanceStartExecutor, InstanceStartExecutor, ProcessVmm,
        default_hvf_boot_run_loop_step_limit, default_hvf_boot_session_config,
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
    struct FakeRunLoopOutcome;

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
        max_steps_sender: mpsc::Sender<usize>,
        wait_for_stop: bool,
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
                max_steps_sender,
                wait_for_stop: true,
            }
        }

        const fn with_wait_for_stop(mut self, wait_for_stop: bool) -> Self {
            self.wait_for_stop = wait_for_stop;
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
            let _ = self.max_steps_sender.send(max_steps.get());
            if self.wait_for_stop {
                stop_token.wait_for_stop();
            }
            Ok(FakeRunLoopOutcome)
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
    fn default_hvf_boot_session_config_uses_serial_region_id_outside_block_regions() {
        let root = TempFilePath::create("root");
        let data = TempFilePath::create("data");
        let mut drives = DriveConfigs::new();
        drives
            .insert(DriveConfigInput::new("rootfs", "rootfs", root.path(), true))
            .expect("root drive should configure");
        drives
            .insert(DriveConfigInput::new("data", "data", data.path(), false))
            .expect("data drive should configure");

        let config = default_hvf_boot_session_config(SharedSerialOutputBuffer::default());
        let serial_region_id = config
            .serial_device
            .expect("default HVF boot config should include serial MMIO")
            .region_id;
        let block_devices = PreparedBlockDevices::from_configs(&drives)
            .expect("block devices should prepare")
            .register_mmio(config.block_mmio_layout)
            .expect("block MMIO should register");

        assert!(
            block_devices
                .registrations()
                .iter()
                .all(|registration| registration.region_id() != serial_region_id)
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
