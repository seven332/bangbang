use bangbang_hvf::{
    HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
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
    type Session = OwnedHvfArm64BootSession;

    fn start(&mut self, controller: &VmmController) -> Result<Self::Session, BackendError> {
        OwnedHvfArm64BootSession::new(controller, self.boot_session_config())
            .map_err(|err| BackendError::Hypervisor(err.to_string()))
    }
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
    use std::fs::{File, remove_file};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use bangbang_runtime::block::{DriveConfigInput, DriveConfigs, PreparedBlockDevices};
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::serial::{SerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::{BackendError, InstanceState, VmmAction, VmmActionError};

    use super::{
        DEFAULT_SERIAL_MMIO_BASE, DEFAULT_SERIAL_MMIO_REGION_ID, HvfInstanceStartExecutor,
        InstanceStartExecutor, ProcessVmm, default_hvf_boot_session_config,
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
