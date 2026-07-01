//! vmnet lifecycle boundary types for future macOS host networking.

use std::fmt;

pub const VMNET_HOST_MODE_VALUE: u32 = 1000;
pub const VMNET_SHARED_MODE_VALUE: u32 = 1001;
pub const VMNET_BRIDGED_MODE_VALUE: u32 = 1002;

pub const VMNET_SUCCESS_VALUE: u32 = 1000;
pub const VMNET_FAILURE_VALUE: u32 = 1001;
pub const VMNET_MEM_FAILURE_VALUE: u32 = 1002;
pub const VMNET_INVALID_ARGUMENT_VALUE: u32 = 1003;
pub const VMNET_SETUP_INCOMPLETE_VALUE: u32 = 1004;
pub const VMNET_INVALID_ACCESS_VALUE: u32 = 1005;
pub const VMNET_PACKET_TOO_BIG_VALUE: u32 = 1006;
pub const VMNET_BUFFER_EXHAUSTED_VALUE: u32 = 1007;
pub const VMNET_TOO_MANY_PACKETS_VALUE: u32 = 1008;
pub const VMNET_SHARING_SERVICE_BUSY_VALUE: u32 = 1009;
pub const VMNET_NOT_AUTHORIZED_VALUE: u32 = 1010;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetMode {
    Host,
    Shared,
    Bridged,
}

impl VmnetMode {
    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Host => VMNET_HOST_MODE_VALUE,
            Self::Shared => VMNET_SHARED_MODE_VALUE,
            Self::Bridged => VMNET_BRIDGED_MODE_VALUE,
        }
    }
}

impl fmt::Display for VmnetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Host => "host",
            Self::Shared => "shared",
            Self::Bridged => "bridged",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetStatus {
    Success,
    Failure,
    MemoryFailure,
    InvalidArgument,
    SetupIncomplete,
    InvalidAccess,
    PacketTooBig,
    BufferExhausted,
    TooManyPackets,
    SharingServiceBusy,
    NotAuthorized,
    Unknown(u32),
}

impl VmnetStatus {
    pub const fn from_raw(value: u32) -> Self {
        match value {
            VMNET_SUCCESS_VALUE => Self::Success,
            VMNET_FAILURE_VALUE => Self::Failure,
            VMNET_MEM_FAILURE_VALUE => Self::MemoryFailure,
            VMNET_INVALID_ARGUMENT_VALUE => Self::InvalidArgument,
            VMNET_SETUP_INCOMPLETE_VALUE => Self::SetupIncomplete,
            VMNET_INVALID_ACCESS_VALUE => Self::InvalidAccess,
            VMNET_PACKET_TOO_BIG_VALUE => Self::PacketTooBig,
            VMNET_BUFFER_EXHAUSTED_VALUE => Self::BufferExhausted,
            VMNET_TOO_MANY_PACKETS_VALUE => Self::TooManyPackets,
            VMNET_SHARING_SERVICE_BUSY_VALUE => Self::SharingServiceBusy,
            VMNET_NOT_AUTHORIZED_VALUE => Self::NotAuthorized,
            value => Self::Unknown(value),
        }
    }

    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Success => VMNET_SUCCESS_VALUE,
            Self::Failure => VMNET_FAILURE_VALUE,
            Self::MemoryFailure => VMNET_MEM_FAILURE_VALUE,
            Self::InvalidArgument => VMNET_INVALID_ARGUMENT_VALUE,
            Self::SetupIncomplete => VMNET_SETUP_INCOMPLETE_VALUE,
            Self::InvalidAccess => VMNET_INVALID_ACCESS_VALUE,
            Self::PacketTooBig => VMNET_PACKET_TOO_BIG_VALUE,
            Self::BufferExhausted => VMNET_BUFFER_EXHAUSTED_VALUE,
            Self::TooManyPackets => VMNET_TOO_MANY_PACKETS_VALUE,
            Self::SharingServiceBusy => VMNET_SHARING_SERVICE_BUSY_VALUE,
            Self::NotAuthorized => VMNET_NOT_AUTHORIZED_VALUE,
            Self::Unknown(value) => value,
        }
    }

    const fn name(self) -> Option<&'static str> {
        match self {
            Self::Success => Some("VMNET_SUCCESS"),
            Self::Failure => Some("VMNET_FAILURE"),
            Self::MemoryFailure => Some("VMNET_MEM_FAILURE"),
            Self::InvalidArgument => Some("VMNET_INVALID_ARGUMENT"),
            Self::SetupIncomplete => Some("VMNET_SETUP_INCOMPLETE"),
            Self::InvalidAccess => Some("VMNET_INVALID_ACCESS"),
            Self::PacketTooBig => Some("VMNET_PACKET_TOO_BIG"),
            Self::BufferExhausted => Some("VMNET_BUFFER_EXHAUSTED"),
            Self::TooManyPackets => Some("VMNET_TOO_MANY_PACKETS"),
            Self::SharingServiceBusy => Some("VMNET_SHARING_SERVICE_BUSY"),
            Self::NotAuthorized => Some("VMNET_NOT_AUTHORIZED"),
            Self::Unknown(_) => None,
        }
    }
}

impl fmt::Display for VmnetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "{name} (vmnet_return_t={})", self.raw_value()),
            None => write!(f, "vmnet_return_t={}", self.raw_value()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetOperation {
    StartInterface,
    StopInterface,
    ReadPackets,
    WritePackets,
}

impl fmt::Display for VmnetOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StartInterface => "vmnet_start_interface",
            Self::StopInterface => "vmnet_stop_interface",
            Self::ReadPackets => "vmnet_read",
            Self::WritePackets => "vmnet_write",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmnetError {
    operation: VmnetOperation,
    status: VmnetStatus,
}

impl VmnetError {
    pub const fn new(operation: VmnetOperation, status: VmnetStatus) -> Self {
        Self { operation, status }
    }

    pub const fn operation(&self) -> VmnetOperation {
        self.operation
    }

    pub const fn status(&self) -> VmnetStatus {
        self.status
    }
}

impl fmt::Display for VmnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed with {}", self.operation, self.status)
    }
}

impl std::error::Error for VmnetError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmnetInterfaceConfig {
    mode: VmnetMode,
    bridged_interface_name: Option<String>,
}

impl VmnetInterfaceConfig {
    pub const fn host() -> Self {
        Self {
            mode: VmnetMode::Host,
            bridged_interface_name: None,
        }
    }

    pub const fn shared() -> Self {
        Self {
            mode: VmnetMode::Shared,
            bridged_interface_name: None,
        }
    }

    pub fn bridged(interface_name: impl Into<String>) -> Result<Self, VmnetInterfaceConfigError> {
        let interface_name = interface_name.into();
        if interface_name.is_empty() {
            return Err(VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        }

        Ok(Self {
            mode: VmnetMode::Bridged,
            bridged_interface_name: Some(interface_name),
        })
    }

    pub const fn mode(&self) -> VmnetMode {
        self.mode
    }

    pub fn bridged_interface_name(&self) -> Option<&str> {
        self.bridged_interface_name.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceConfigError {
    EmptyBridgedInterfaceName,
}

impl fmt::Display for VmnetInterfaceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not be empty")
            }
        }
    }
}

impl std::error::Error for VmnetInterfaceConfigError {}

pub trait VmnetInterfaceLifecycle: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn start_interface(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<Self::Interface, VmnetError>;

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError>;
}

#[derive(Debug)]
pub struct OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    lifecycle: L,
    interface: Option<L::Interface>,
}

impl<L> OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    pub fn start(mut lifecycle: L, config: &VmnetInterfaceConfig) -> Result<Self, VmnetError> {
        let interface = lifecycle.start_interface(config)?;

        Ok(Self {
            lifecycle,
            interface: Some(interface),
        })
    }

    pub const fn is_started(&self) -> bool {
        self.interface.is_some()
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        if let Some(interface) = self.interface.as_mut() {
            self.lifecycle.stop_interface(interface)?;
            self.interface = None;
        }

        Ok(())
    }
}

impl<L> Drop for OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    fn drop(&mut self) {
        match self.stop() {
            Ok(()) | Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        OwnedVmnetInterface, VMNET_BRIDGED_MODE_VALUE, VMNET_HOST_MODE_VALUE,
        VMNET_SHARED_MODE_VALUE, VmnetError, VmnetInterfaceConfig, VmnetInterfaceConfigError,
        VmnetInterfaceLifecycle, VmnetMode, VmnetOperation, VmnetStatus,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedVmnetInterface {
        id: u64,
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetLifecycle {
        events: Arc<Mutex<Vec<String>>>,
        start_status: Option<VmnetStatus>,
        stop_status: Option<VmnetStatus>,
    }

    impl RecordingVmnetLifecycle {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                start_status: None,
                stop_status: None,
            }
        }

        fn with_start_status(mut self, status: VmnetStatus) -> Self {
            self.start_status = Some(status);
            self
        }

        fn with_stop_status(mut self, status: VmnetStatus) -> Self {
            self.stop_status = Some(status);
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetInterfaceLifecycle for RecordingVmnetLifecycle {
        type Interface = RecordedVmnetInterface;

        fn start_interface(
            &mut self,
            config: &VmnetInterfaceConfig,
        ) -> Result<Self::Interface, VmnetError> {
            push_event(&self.events, format!("start:{}", config.mode()));
            if let Some(status) = self.start_status {
                return Err(VmnetError::new(VmnetOperation::StartInterface, status));
            }

            Ok(RecordedVmnetInterface { id: 7 })
        }

        fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
            push_event(&self.events, format!("stop:{}", interface.id));
            if let Some(status) = self.stop_status {
                return Err(VmnetError::new(VmnetOperation::StopInterface, status));
            }

            Ok(())
        }
    }

    fn push_event(events: &Arc<Mutex<Vec<String>>>, event: String) {
        match events.lock() {
            Ok(mut guard) => guard.push(event),
            Err(poisoned) => poisoned.into_inner().push(event),
        }
    }

    fn recorded_events(events: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        match events.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    #[test]
    fn vmnet_modes_match_sdk_values() {
        assert_eq!(VmnetMode::Host.raw_value(), VMNET_HOST_MODE_VALUE);
        assert_eq!(VmnetMode::Shared.raw_value(), VMNET_SHARED_MODE_VALUE);
        assert_eq!(VmnetMode::Bridged.raw_value(), VMNET_BRIDGED_MODE_VALUE);
        assert_eq!(VmnetMode::Host.to_string(), "host");
        assert_eq!(VmnetMode::Shared.to_string(), "shared");
        assert_eq!(VmnetMode::Bridged.to_string(), "bridged");
    }

    #[test]
    fn vmnet_status_maps_known_and_unknown_values() {
        assert_eq!(VmnetStatus::from_raw(1000), VmnetStatus::Success);
        assert_eq!(VmnetStatus::from_raw(1010), VmnetStatus::NotAuthorized);
        assert_eq!(VmnetStatus::from_raw(9000), VmnetStatus::Unknown(9000));
        assert_eq!(
            VmnetStatus::NotAuthorized.to_string(),
            "VMNET_NOT_AUTHORIZED (vmnet_return_t=1010)"
        );
        assert_eq!(
            VmnetStatus::Unknown(9000).to_string(),
            "vmnet_return_t=9000"
        );
    }

    #[test]
    fn vmnet_error_includes_operation_and_status() {
        let error = VmnetError::new(VmnetOperation::ReadPackets, VmnetStatus::BufferExhausted);

        assert_eq!(error.operation(), VmnetOperation::ReadPackets);
        assert_eq!(error.status(), VmnetStatus::BufferExhausted);
        assert_eq!(
            error.to_string(),
            "vmnet_read failed with VMNET_BUFFER_EXHAUSTED (vmnet_return_t=1007)"
        );
    }

    #[test]
    fn vmnet_interface_config_models_modes() {
        let host = VmnetInterfaceConfig::host();
        let shared = VmnetInterfaceConfig::shared();
        let bridged = VmnetInterfaceConfig::bridged("en0").expect("bridged config should validate");

        assert_eq!(host.mode(), VmnetMode::Host);
        assert_eq!(host.bridged_interface_name(), None);
        assert_eq!(shared.mode(), VmnetMode::Shared);
        assert_eq!(shared.bridged_interface_name(), None);
        assert_eq!(bridged.mode(), VmnetMode::Bridged);
        assert_eq!(bridged.bridged_interface_name(), Some("en0"));
    }

    #[test]
    fn bridged_config_rejects_empty_interface_name() {
        let error = VmnetInterfaceConfig::bridged("")
            .expect_err("empty bridged interface should be rejected");

        assert_eq!(error, VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not be empty"
        );
    }

    #[test]
    fn owned_interface_starts_and_stops_once() {
        let lifecycle = RecordingVmnetLifecycle::new();
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
            .expect("owned vmnet interface should start");

        assert!(interface.is_started());
        interface.stop().expect("owned vmnet interface should stop");
        assert!(!interface.is_started());
        interface.stop().expect("second stop should be a no-op");

        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn owned_interface_drop_stops_started_interface() {
        let lifecycle = RecordingVmnetLifecycle::new();
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let _interface = OwnedVmnetInterface::start(lifecycle, &config)
                .expect("owned vmnet interface should start");
        }

        assert_eq!(recorded_events(&event_log), ["start:shared", "stop:7"]);
    }

    #[test]
    fn owned_interface_start_failure_does_not_create_owner() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_start_status(VmnetStatus::InvalidAccess);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let error = OwnedVmnetInterface::start(lifecycle, &config)
            .expect_err("start failure should return an error");

        assert_eq!(error.operation(), VmnetOperation::StartInterface);
        assert_eq!(error.status(), VmnetStatus::InvalidAccess);
        assert_eq!(recorded_events(&event_log), ["start:host"]);
    }

    #[test]
    fn failed_stop_keeps_interface_started_for_retry() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
            .expect("owned vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed stop should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(interface.is_started());
        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn vmnet_write_operation_displays_name() {
        let error = VmnetError::new(VmnetOperation::WritePackets, VmnetStatus::PacketTooBig);

        assert_eq!(
            error.to_string(),
            "vmnet_write failed with VMNET_PACKET_TOO_BIG (vmnet_return_t=1006)"
        );
    }
}
