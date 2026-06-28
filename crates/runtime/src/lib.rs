//! Backend-neutral VM runtime boundary.

pub mod boot;
pub mod fdt;
pub mod memory;
pub mod mmio;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmmAction {
    GetVmmVersion,
    GetVmInstanceInfo,
}

impl VmmAction {
    pub const fn name(self) -> &'static str {
        match self {
            Self::GetVmmVersion => "GetVmmVersion",
            Self::GetVmInstanceInfo => "GetVmInstanceInfo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmData {
    VmmVersion(String),
    InstanceInformation(InstanceInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmActionError {
    UnsupportedAction(&'static str),
    UnsupportedState {
        action: &'static str,
        state: InstanceState,
    },
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
        }
    }
}

impl std::error::Error for VmmActionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmmController {
    instance_info: InstanceInfo,
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
        }
    }

    pub fn instance_info(&self) -> &InstanceInfo {
        &self.instance_info
    }

    pub fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
        match action {
            VmmAction::GetVmmVersion => {
                Ok(VmmData::VmmVersion(self.instance_info.vmm_version.clone()))
            }
            VmmAction::GetVmInstanceInfo => {
                Ok(VmmData::InstanceInformation(self.instance_info.clone()))
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
    use super::{BackendError, InstanceState, VmmAction, VmmActionError, VmmController, VmmData};

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
