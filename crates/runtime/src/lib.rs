//! Backend-neutral VM runtime boundary.

pub mod block;
pub mod boot;
pub mod fdt;
pub mod interrupt;
pub mod machine;
pub mod memory;
pub mod mmio;
pub mod serial;
pub mod startup;
pub mod virtio_mmio;
pub mod virtio_queue;

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InstanceState {
    #[default]
    NotStarted,
    Running,
    Paused,
}

impl fmt::Display for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted => f.write_str("Not started"),
            Self::Running => f.write_str("Running"),
            Self::Paused => f.write_str("Paused"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInfo {
    pub id: String,
    pub state: InstanceState,
    pub vmm_version: String,
    pub app_name: String,
}

impl InstanceInfo {
    pub fn new(
        id: impl Into<String>,
        state: InstanceState,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            state,
            vmm_version: vmm_version.into(),
            app_name: app_name.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmAction {
    GetVmmVersion,
    GetVmInstanceInfo,
    GetMachineConfig,
    InstanceStart,
    FlushMetrics,
    PutBootSource(boot::BootSourceConfigInput),
    PutMachineConfig(machine::MachineConfigInput),
    PutDrive(block::DriveConfigInput),
}

impl VmmAction {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::GetVmmVersion => "GetVmmVersion",
            Self::GetVmInstanceInfo => "GetVmInstanceInfo",
            Self::GetMachineConfig => "GetMachineConfig",
            Self::InstanceStart => "InstanceStart",
            Self::FlushMetrics => "FlushMetrics",
            Self::PutBootSource(_) => "PutBootSource",
            Self::PutMachineConfig(_) => "PutMachineConfig",
            Self::PutDrive(_) => "PutDrive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmData {
    Empty,
    VmmVersion(String),
    InstanceInformation(InstanceInfo),
    MachineConfiguration(machine::MachineConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmActionError {
    UnsupportedAction(&'static str),
    UnsupportedState {
        action: &'static str,
        state: InstanceState,
    },
    MissingBootSource,
    BootSourceConfig(boot::BootSourceConfigError),
    DriveConfig(block::DriveConfigError),
    MachineConfig(machine::MachineConfigError),
}

impl fmt::Display for VmmActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction(action) => {
                write!(f, "The requested operation is not supported: {action}")
            }
            Self::UnsupportedState { action, state } => {
                write!(
                    f,
                    "The requested operation is not supported in {state} state: {action}"
                )
            }
            Self::MissingBootSource => {
                f.write_str("boot source must be configured before InstanceStart")
            }
            Self::BootSourceConfig(err) => write!(f, "{err}"),
            Self::DriveConfig(err) => write!(f, "{err}"),
            Self::MachineConfig(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for VmmActionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BootSourceConfig(err) => Some(err),
            Self::DriveConfig(err) => Some(err),
            Self::MachineConfig(err) => Some(err),
            Self::MissingBootSource
            | Self::UnsupportedAction(_)
            | Self::UnsupportedState { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmmController {
    instance_info: InstanceInfo,
    machine_config: machine::MachineConfig,
    boot_source_config: Option<boot::BootSourceConfig>,
    drive_configs: block::DriveConfigs,
}

impl VmmController {
    pub fn new(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
    ) -> Self {
        Self {
            instance_info: InstanceInfo::new(
                instance_id,
                InstanceState::NotStarted,
                vmm_version,
                app_name,
            ),
            machine_config: machine::MachineConfig::default(),
            boot_source_config: None,
            drive_configs: block::DriveConfigs::new(),
        }
    }

    pub fn instance_info(&self) -> &InstanceInfo {
        &self.instance_info
    }

    pub fn drive_configs(&self) -> &[block::DriveConfig] {
        self.drive_configs.as_slice()
    }

    pub const fn machine_config(&self) -> machine::MachineConfig {
        self.machine_config
    }

    pub fn boot_source_config(&self) -> Option<&boot::BootSourceConfig> {
        self.boot_source_config.as_ref()
    }

    pub fn preflight_instance_start(&self) -> Result<(), VmmActionError> {
        if self.instance_info.state != InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::InstanceStart.name(),
                state: self.instance_info.state,
            });
        }

        if self.boot_source_config.is_none() {
            return Err(VmmActionError::MissingBootSource);
        }

        Ok(())
    }

    pub fn commit_instance_start(&mut self) -> Result<(), VmmActionError> {
        self.preflight_instance_start()?;
        self.instance_info.state = InstanceState::Running;
        Ok(())
    }

    pub fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        let action_name = action.name();
        match action {
            VmmAction::GetVmmVersion => {
                Ok(VmmData::VmmVersion(self.instance_info.vmm_version.clone()))
            }
            VmmAction::GetVmInstanceInfo => {
                Ok(VmmData::InstanceInformation(self.instance_info.clone()))
            }
            VmmAction::GetMachineConfig => Ok(VmmData::MachineConfiguration(self.machine_config)),
            VmmAction::InstanceStart | VmmAction::FlushMetrics => {
                Err(VmmActionError::UnsupportedAction(action_name))
            }
            VmmAction::PutBootSource(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.boot_source_config = Some(
                    config
                        .validate()
                        .map_err(VmmActionError::BootSourceConfig)?,
                );

                Ok(VmmData::Empty)
            }
            VmmAction::PutMachineConfig(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.machine_config = config.validate().map_err(VmmActionError::MachineConfig)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PutDrive(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.drive_configs
                    .insert(config)
                    .map_err(VmmActionError::DriveConfig)?;

                Ok(VmmData::Empty)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Unsupported(&'static str),
    InvalidState(&'static str),
    Hypervisor(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) => write!(f, "unsupported backend: {message}"),
            Self::InvalidState(message) => write!(f, "invalid backend state: {message}"),
            Self::Hypervisor(message) => write!(f, "hypervisor error: {message}"),
        }
    }
}

impl std::error::Error for BackendError {}

pub trait VmBackend: fmt::Debug {
    fn create_vm(&mut self) -> Result<(), BackendError>;
    fn destroy_vm(&mut self) -> Result<(), BackendError>;
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::path::{Path, PathBuf};

    use super::{
        BackendError, InstanceState, VmmAction, VmmActionError, VmmController, VmmData,
        block::{DriveConfigError, DriveConfigInput},
        boot::{
            BootCommandLineError, BootPayloadKind, BootSourceConfigError, BootSourceConfigInput,
        },
        machine::{DEFAULT_MEM_SIZE_MIB, DEFAULT_VCPU_COUNT, MachineConfigInput},
    };

    fn drive_input(id: &str, path: &str, is_root_device: bool) -> DriveConfigInput {
        DriveConfigInput::new(id, id, path, is_root_device)
    }

    fn boot_source_input(kernel_image_path: &str) -> BootSourceConfigInput {
        BootSourceConfigInput::new(kernel_image_path)
    }

    #[test]
    fn displays_not_started_state() {
        assert_eq!(InstanceState::NotStarted.to_string(), "Not started");
    }

    #[test]
    fn displays_running_state() {
        assert_eq!(InstanceState::Running.to_string(), "Running");
    }

    #[test]
    fn displays_paused_state() {
        assert_eq!(InstanceState::Paused.to_string(), "Paused");
    }

    #[test]
    fn controller_initializes_instance_info() {
        let controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let info = controller.instance_info();
        assert_eq!(info.id, "demo-1");
        assert_eq!(info.state, InstanceState::NotStarted);
        assert_eq!(info.vmm_version, "0.1.0");
        assert_eq!(info.app_name, "bangbang");
        assert_eq!(controller.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert_eq!(
            controller.machine_config().mem_size_mib(),
            DEFAULT_MEM_SIZE_MIB
        );
        assert_eq!(controller.boot_source_config(), None);
        assert!(controller.drive_configs().is_empty());
    }

    #[test]
    fn handles_get_vmm_version() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::GetVmmVersion),
            Ok(VmmData::VmmVersion("0.1.0".to_string()))
        );
    }

    #[test]
    fn handles_get_vm_instance_info() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let data = controller
            .handle_action(VmmAction::GetVmInstanceInfo)
            .expect("instance info should be returned");

        let VmmData::InstanceInformation(info) = data else {
            panic!("expected instance info");
        };
        assert_eq!(info.id, "demo-1");
        assert_eq!(info.state, InstanceState::NotStarted);
        assert_eq!(info.vmm_version, "0.1.0");
        assert_eq!(info.app_name, "bangbang");
    }

    #[test]
    fn handles_get_machine_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let data = controller
            .handle_action(VmmAction::GetMachineConfig)
            .expect("machine config should be returned");

        let VmmData::MachineConfiguration(config) = data else {
            panic!("expected machine config");
        };
        assert_eq!(config.vcpu_count(), DEFAULT_VCPU_COUNT);
        assert_eq!(config.mem_size_mib(), DEFAULT_MEM_SIZE_MIB);
        assert!(!config.smt());
        assert_eq!(config.cpu_template(), None);
        assert!(!config.track_dirty_pages());
    }

    #[test]
    fn action_names_include_start_and_metrics() {
        assert_eq!(VmmAction::InstanceStart.name(), "InstanceStart");
        assert_eq!(VmmAction::FlushMetrics.name(), "FlushMetrics");
    }

    #[test]
    fn instance_start_is_explicitly_unsupported_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("machine config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input(
                "/tmp/original-vmlinux",
            )))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("drive config should be stored");

        let err = controller
            .handle_action(VmmAction::InstanceStart)
            .expect_err("instance start should remain unsupported");

        assert_eq!(
            err,
            VmmActionError::UnsupportedAction(VmmAction::InstanceStart.name())
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(controller.drive_configs().len(), 1);
    }

    #[test]
    fn instance_start_preflight_requires_boot_source_without_mutating() {
        let controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .preflight_instance_start()
            .expect_err("missing boot source should fail preflight");

        assert_eq!(err, VmmActionError::MissingBootSource);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_none());
    }

    #[test]
    fn instance_start_preflight_accepts_not_started_with_boot_source() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        assert_eq!(controller.preflight_instance_start(), Ok(()));
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn instance_start_preflight_rejects_running_state() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .preflight_instance_start()
            .expect_err("running state should fail preflight");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::InstanceStart.name(),
                state: InstanceState::Running,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn commit_instance_start_sets_running_after_preflight_requirements() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        assert_eq!(controller.commit_instance_start(), Ok(()));

        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn commit_instance_start_rejects_missing_boot_source_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .commit_instance_start()
            .expect_err("missing boot source should fail commit");

        assert_eq!(err, VmmActionError::MissingBootSource);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn commit_instance_start_rejects_duplicate_start_without_mutating_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("drive config should be stored");
        controller
            .commit_instance_start()
            .expect("first commit should succeed");

        let err = controller
            .commit_instance_start()
            .expect_err("duplicate start should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::InstanceStart.name(),
                state: InstanceState::Running,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(controller.drive_configs().len(), 1);
    }

    #[test]
    fn flush_metrics_is_explicitly_unsupported_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::FlushMetrics)
            .expect_err("flush metrics should remain unsupported");

        assert_eq!(
            err,
            VmmActionError::UnsupportedAction(VmmAction::FlushMetrics.name())
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert!(controller.boot_source_config().is_none());
        assert!(controller.drive_configs().is_empty());
    }

    #[test]
    fn handles_put_machine_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256))),
            Ok(VmmData::Empty)
        );

        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
    }

    #[test]
    fn put_machine_config_replaces_previous_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");

        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(4, 512)))
            .expect("replacement machine config should be stored");

        assert_eq!(controller.machine_config().vcpu_count(), 4);
        assert_eq!(controller.machine_config().mem_size_mib(), 512);
    }

    #[test]
    fn put_machine_config_rejects_invalid_input_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");

        let err = controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(0, 512)))
            .expect_err("invalid machine config should fail");

        assert_eq!(err.to_string(), "machine vcpu_count must be in 1..=32");
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
    }

    #[test]
    fn put_machine_config_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(4, 512)))
            .expect_err("running machine config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutMachineConfig",
                state: InstanceState::Running,
            }
        );
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
    }

    #[test]
    fn handles_put_boot_source_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutBootSource(
                boot_source_input("/tmp/vmlinux")
                    .with_initrd_path("/tmp/initrd.img")
                    .with_boot_args("console=hvc0 reboot=k panic=1"),
            )),
            Ok(VmmData::Empty)
        );

        let config = controller
            .boot_source_config()
            .expect("boot source config should be stored");
        assert_eq!(config.kernel_image_path(), Path::new("/tmp/vmlinux"));
        assert_eq!(config.initrd_path(), Some(Path::new("/tmp/initrd.img")));
        assert_eq!(config.boot_args(), Some("console=hvc0 reboot=k panic=1"));
    }

    #[test]
    fn put_boot_source_config_replaces_previous_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(
                boot_source_input("/tmp/vmlinux")
                    .with_initrd_path("/tmp/initrd.img")
                    .with_boot_args("console=hvc0"),
            ))
            .expect("initial boot source config should be stored");

        controller
            .handle_action(VmmAction::PutBootSource(
                boot_source_input("/tmp/replacement-vmlinux").with_boot_args("console=ttyS0"),
            ))
            .expect("replacement boot source config should be stored");

        let config = controller
            .boot_source_config()
            .expect("replacement boot source config should be stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/replacement-vmlinux")
        );
        assert_eq!(config.initrd_path(), None);
        assert_eq!(config.boot_args(), Some("console=ttyS0"));
    }

    #[test]
    fn put_boot_source_config_rejects_invalid_input_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input(
                "/tmp/original-vmlinux",
            )))
            .expect("initial boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::PutBootSource(
                boot_source_input("/tmp/private-vmlinux").with_boot_args("secret\0debug"),
            ))
            .expect_err("invalid boot source config should fail");

        assert_eq!(
            err,
            VmmActionError::BootSourceConfig(BootSourceConfigError::CommandLine(
                BootCommandLineError::ContainsNul,
            ))
        );
        assert_eq!(
            err.to_string(),
            "kernel command line is invalid: contains a NUL byte"
        );
        assert!(!err.to_string().contains("secret"));
        assert!(!err.to_string().contains("/tmp/private-vmlinux"));

        let config = controller
            .boot_source_config()
            .expect("original boot source config should remain stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/original-vmlinux")
        );
        assert_eq!(config.initrd_path(), None);
        assert_eq!(config.boot_args(), None);
    }

    #[test]
    fn put_boot_source_config_rejects_empty_paths_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input(
                "/tmp/original-vmlinux",
            )))
            .expect("initial boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                PathBuf::new(),
            )))
            .expect_err("empty kernel path should fail");

        assert_eq!(
            err,
            VmmActionError::BootSourceConfig(BootSourceConfigError::EmptyPath {
                payload: BootPayloadKind::Kernel,
            })
        );

        let err = controller
            .handle_action(VmmAction::PutBootSource(
                boot_source_input("/tmp/private-vmlinux").with_initrd_path(PathBuf::new()),
            ))
            .expect_err("empty initrd path should fail");

        assert_eq!(
            err,
            VmmActionError::BootSourceConfig(BootSourceConfigError::EmptyPath {
                payload: BootPayloadKind::Initrd,
            })
        );

        let config = controller
            .boot_source_config()
            .expect("original boot source config should remain stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/original-vmlinux")
        );
    }

    #[test]
    fn put_boot_source_config_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input(
                "/tmp/original-vmlinux",
            )))
            .expect("initial boot source config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PutBootSource(boot_source_input(
                "/tmp/replacement-vmlinux",
            )))
            .expect_err("running boot source config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutBootSource",
                state: InstanceState::Running,
            }
        );
        let config = controller
            .boot_source_config()
            .expect("original boot source config should remain stored");
        assert_eq!(
            config.kernel_image_path(),
            Path::new("/tmp/original-vmlinux")
        );
    }

    #[test]
    fn handles_put_drive_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))),
            Ok(VmmData::Empty)
        );

        assert_eq!(controller.drive_configs().len(), 1);
        let config = &controller.drive_configs()[0];
        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/rootfs.ext4"));
        assert!(config.is_root_device());
    }

    #[test]
    fn put_drive_config_replaces_duplicate_id() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("initial drive config should be stored");

        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/replaced.ext4",
                false,
            )))
            .expect("duplicate drive id should replace existing config");

        assert_eq!(controller.drive_configs().len(), 1);
        let config = &controller.drive_configs()[0];
        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/replaced.ext4"));
        assert!(!config.is_root_device());
    }

    #[test]
    fn put_drive_config_rejects_second_root_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("root drive config should be stored");

        let err = controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "data",
                "/tmp/data.ext4",
                true,
            )))
            .expect_err("second root drive should fail");

        assert_eq!(
            err,
            VmmActionError::DriveConfig(DriveConfigError::RootDeviceAlreadyConfigured)
        );
        assert_eq!(err.to_string(), "a root drive is already configured");
        assert_eq!(controller.drive_configs().len(), 1);
        assert_eq!(controller.drive_configs()[0].drive_id(), "rootfs");
    }

    #[test]
    fn put_drive_config_rejects_invalid_config_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                "secret\nid",
                "secret\nid",
                "/tmp/rootfs.ext4",
                false,
            )))
            .expect_err("invalid drive id should fail");

        assert_eq!(
            err.to_string(),
            "path drive_id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains("secret"));
        assert!(controller.drive_configs().is_empty());
    }

    #[test]
    fn put_drive_config_rejects_running_state() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect_err("running drive config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutDrive",
                state: InstanceState::Running,
            }
        );
        assert!(controller.drive_configs().is_empty());
    }

    #[test]
    fn displays_unsupported_action_error() {
        let err = VmmActionError::UnsupportedAction(VmmAction::GetVmInstanceInfo.name());

        assert_eq!(
            err.to_string(),
            "The requested operation is not supported: GetVmInstanceInfo"
        );
    }

    #[test]
    fn displays_unsupported_state_error() {
        let err = VmmActionError::UnsupportedState {
            action: VmmAction::GetVmmVersion.name(),
            state: InstanceState::Running,
        };

        assert_eq!(
            err.to_string(),
            "The requested operation is not supported in Running state: GetVmmVersion"
        );
    }

    #[test]
    fn displays_missing_boot_source_error() {
        let err = VmmActionError::MissingBootSource;

        assert_eq!(
            err.to_string(),
            "boot source must be configured before InstanceStart"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_drive_config_error() {
        let err = VmmActionError::DriveConfig(DriveConfigError::EmptyPathOnHost);

        assert_eq!(err.to_string(), "drive path_on_host must not be empty");
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_boot_source_config_error() {
        let err = VmmActionError::BootSourceConfig(BootSourceConfigError::EmptyPath {
            payload: BootPayloadKind::Kernel,
        });

        assert_eq!(err.to_string(), "kernel image path must not be empty");
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_machine_config_error() {
        let err =
            VmmActionError::MachineConfig(super::machine::MachineConfigError::InvalidMemorySize);

        assert_eq!(err.to_string(), "machine mem_size_mib must not be zero");
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_unsupported_error() {
        let err = BackendError::Unsupported("macOS on Apple Silicon required");

        assert_eq!(
            err.to_string(),
            "unsupported backend: macOS on Apple Silicon required"
        );
    }

    #[test]
    fn displays_invalid_state_error() {
        let err = BackendError::InvalidState("VM must be created before creating a vCPU");

        assert_eq!(
            err.to_string(),
            "invalid backend state: VM must be created before creating a vCPU"
        );
    }

    #[test]
    fn displays_hypervisor_error() {
        let err = BackendError::Hypervisor("hv_vm_create failed".to_string());

        assert_eq!(err.to_string(), "hypervisor error: hv_vm_create failed");
    }
}
