//! Backend-neutral VM runtime boundary.

pub mod balloon;
pub mod block;
pub mod boot;
pub mod boot_timer;
pub mod cpu;
pub mod entropy;
pub mod fdt;
pub mod interrupt;
pub mod logger;
pub mod machine;
pub mod memory;
pub mod memory_hotplug;
pub mod metrics;
pub mod mmds;
pub mod mmio;
pub mod network;
pub mod pmem;
pub mod rtc;
pub mod serial;
pub mod startup;
pub(crate) mod token_bucket;
pub mod virtio_mmio;
pub mod virtio_queue;
pub mod vsock;

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
    GetMmds,
    GetVmConfig,
    InstanceStart,
    Pause,
    Resume,
    CreateSnapshot,
    LoadSnapshot,
    FlushMetrics,
    GetBalloon,
    GetBalloonStats,
    GetBalloonHintingStatus,
    PutBalloon(balloon::BalloonConfigInput),
    PatchBalloon(balloon::BalloonUpdateInput),
    PatchBalloonStats(balloon::BalloonStatsUpdateInput),
    PatchBalloonHintingStart(balloon::BalloonHintingStartInput),
    PatchBalloonHintingStop,
    GetMemoryHotplug,
    PutMemoryHotplug(memory_hotplug::MemoryHotplugConfigInput),
    PatchMemoryHotplug(memory_hotplug::MemoryHotplugSizeUpdateInput),
    PutEntropy(entropy::EntropyConfigInput),
    PutPmem(pmem::PmemConfigInput),
    PatchPmem(pmem::PmemUpdateInput),
    PutBootSource(boot::BootSourceConfigInput),
    PutCpuConfig(cpu::CpuConfigInput),
    PutLogger(logger::LoggerConfigInput),
    PutMachineConfig(machine::MachineConfigInput),
    PatchMachineConfig(machine::MachineConfigPatchInput),
    PutMetrics(metrics::MetricsConfigInput),
    PutMmds(mmds::MmdsContentInput),
    PatchMmds(mmds::MmdsContentInput),
    PutMmdsConfig(mmds::MmdsConfigInput),
    PutSerial(serial::SerialConfigInput),
    PutDrive(block::DriveConfigInput),
    UpdateBlockDevice(block::DriveUpdateInput),
    HotUnplugDevice(HotUnplugDeviceInput),
    PutNetworkInterface(network::NetworkInterfaceConfigInput),
    UpdateNetworkInterface(network::NetworkInterfaceUpdateInput),
    PutVsock(vsock::VsockConfigInput),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotUnplugDeviceKind {
    Drive,
    NetworkInterface,
    Pmem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotUnplugDeviceInput {
    kind: HotUnplugDeviceKind,
    id: String,
}

impl HotUnplugDeviceInput {
    pub fn new(kind: HotUnplugDeviceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub const fn kind(&self) -> HotUnplugDeviceKind {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

impl VmmAction {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::GetVmmVersion => "GetVmmVersion",
            Self::GetVmInstanceInfo => "GetVmInstanceInfo",
            Self::GetMachineConfig => "GetMachineConfig",
            Self::GetMmds => "GetMmds",
            Self::GetVmConfig => "GetVmConfig",
            Self::InstanceStart => "InstanceStart",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::CreateSnapshot => "CreateSnapshot",
            Self::LoadSnapshot => "LoadSnapshot",
            Self::FlushMetrics => "FlushMetrics",
            Self::GetBalloon => "GetBalloon",
            Self::GetBalloonStats => "GetBalloonStats",
            Self::GetBalloonHintingStatus => "GetBalloonHintingStatus",
            Self::PutBalloon(_) => "PutBalloon",
            Self::PatchBalloon(_) => "PatchBalloon",
            Self::PatchBalloonStats(_) => "PatchBalloonStats",
            Self::PatchBalloonHintingStart(_) => "PatchBalloonHintingStart",
            Self::PatchBalloonHintingStop => "PatchBalloonHintingStop",
            Self::GetMemoryHotplug => "GetMemoryHotplug",
            Self::PutMemoryHotplug(_) => "PutMemoryHotplug",
            Self::PatchMemoryHotplug(_) => "PatchMemoryHotplug",
            Self::PutEntropy(_) => "PutEntropy",
            Self::PutPmem(_) => "PutPmem",
            Self::PatchPmem(_) => "PatchPmem",
            Self::PutBootSource(_) => "PutBootSource",
            Self::PutCpuConfig(_) => "PutCpuConfig",
            Self::PutLogger(_) => "PutLogger",
            Self::PutMachineConfig(_) => "PutMachineConfig",
            Self::PatchMachineConfig(_) => "PatchMachineConfig",
            Self::PutMetrics(_) => "PutMetrics",
            Self::PutMmds(_) => "PutMmds",
            Self::PatchMmds(_) => "PatchMmds",
            Self::PutMmdsConfig(_) => "PutMmdsConfig",
            Self::PutSerial(_) => "PutSerial",
            Self::PutDrive(_) => "PutDrive",
            Self::UpdateBlockDevice(_) => "UpdateBlockDevice",
            Self::HotUnplugDevice(_) => "HotUnplugDevice",
            Self::PutNetworkInterface(_) => "PutNetworkInterface",
            Self::UpdateNetworkInterface(_) => "UpdateNetworkInterface",
            Self::PutVsock(_) => "PutVsock",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmData {
    Empty,
    VmmVersion(String),
    InstanceInformation(InstanceInfo),
    MachineConfiguration(machine::MachineConfig),
    BalloonConfiguration(balloon::BalloonConfig),
    BalloonStatistics(balloon::BalloonStats),
    BalloonHintingStatus(balloon::BalloonHintingStatus),
    MemoryHotplugStatus(memory_hotplug::MemoryHotplugStatus),
    MmdsValue(serde_json::Value),
    VmConfiguration(VmConfiguration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfiguration {
    machine_config: machine::MachineConfig,
    boot_source_config: Option<boot::BootSourceConfig>,
    drive_configs: Vec<block::DriveConfig>,
    network_interface_configs: Vec<network::NetworkInterfaceConfig>,
    mmds_config: Option<mmds::MmdsConfig>,
    vsock_config: Option<vsock::VsockConfig>,
    entropy_config: Option<entropy::EntropyConfig>,
    memory_hotplug_config: Option<memory_hotplug::MemoryHotplugConfig>,
    balloon_config: Option<balloon::BalloonConfig>,
    pmem_configs: Vec<pmem::PmemConfig>,
}

impl VmConfiguration {
    pub fn new(
        machine_config: machine::MachineConfig,
        boot_source_config: Option<boot::BootSourceConfig>,
        drive_configs: Vec<block::DriveConfig>,
        network_interface_configs: Vec<network::NetworkInterfaceConfig>,
        mmds_config: Option<mmds::MmdsConfig>,
        vsock_config: Option<vsock::VsockConfig>,
        entropy_config: Option<entropy::EntropyConfig>,
    ) -> Self {
        Self {
            machine_config,
            boot_source_config,
            drive_configs,
            network_interface_configs,
            mmds_config,
            vsock_config,
            entropy_config,
            memory_hotplug_config: None,
            balloon_config: None,
            pmem_configs: Vec::new(),
        }
    }

    pub const fn with_memory_hotplug_config(
        mut self,
        memory_hotplug_config: Option<memory_hotplug::MemoryHotplugConfig>,
    ) -> Self {
        self.memory_hotplug_config = memory_hotplug_config;
        self
    }

    pub const fn with_balloon_config(
        mut self,
        balloon_config: Option<balloon::BalloonConfig>,
    ) -> Self {
        self.balloon_config = balloon_config;
        self
    }

    pub fn with_pmem_configs(mut self, pmem_configs: Vec<pmem::PmemConfig>) -> Self {
        self.pmem_configs = pmem_configs;
        self
    }

    pub const fn machine_config(&self) -> machine::MachineConfig {
        self.machine_config
    }

    pub fn boot_source_config(&self) -> Option<&boot::BootSourceConfig> {
        self.boot_source_config.as_ref()
    }

    pub fn drive_configs(&self) -> &[block::DriveConfig] {
        &self.drive_configs
    }

    pub fn network_interface_configs(&self) -> &[network::NetworkInterfaceConfig] {
        &self.network_interface_configs
    }

    pub fn mmds_config(&self) -> Option<&mmds::MmdsConfig> {
        self.mmds_config.as_ref()
    }

    pub fn vsock_config(&self) -> Option<&vsock::VsockConfig> {
        self.vsock_config.as_ref()
    }

    pub const fn entropy_config(&self) -> Option<entropy::EntropyConfig> {
        self.entropy_config
    }

    pub const fn memory_hotplug_config(&self) -> Option<memory_hotplug::MemoryHotplugConfig> {
        self.memory_hotplug_config
    }

    pub const fn balloon_config(&self) -> Option<balloon::BalloonConfig> {
        self.balloon_config
    }

    pub fn pmem_configs(&self) -> &[pmem::PmemConfig] {
        &self.pmem_configs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmmActionError {
    UnsupportedAction(&'static str),
    UnsupportedState {
        action: &'static str,
        state: InstanceState,
    },
    BalloonConfig(balloon::BalloonConfigError),
    BalloonUnsupported,
    BalloonStats(balloon::BalloonStatsError),
    BalloonHintingCommand(balloon::BalloonHintingCommandError),
    BalloonHintingStatus(balloon::BalloonHintingStatusError),
    BalloonUpdate(balloon::BalloonUpdateError),
    EntropyUnsupported,
    MissingBootSource,
    InstanceStart(BackendError),
    Lifecycle(BackendError),
    BootSourceConfig(boot::BootSourceConfigError),
    DriveConfig(block::DriveConfigError),
    DriveUpdate(block::DriveUpdateError),
    DriveUpdateUnsupported,
    EntropyConfig(entropy::EntropyConfigError),
    LoggerConfig(logger::LoggerConfigError),
    LoggerWrite(logger::LoggerWriteError),
    MachineConfig(machine::MachineConfigError),
    MetricsConfig(metrics::MetricsConfigError),
    MetricsFlush(metrics::MetricsFlushError),
    MmdsConfig(mmds::MmdsConfigError),
    MmdsDataStore(mmds::MmdsDataStoreError),
    MmdsState(mmds::MmdsStateLockError),
    NetworkInterfaceConfig(network::NetworkInterfaceConfigError),
    NetworkInterfaceUpdate(network::NetworkInterfaceUpdateError),
    NetworkInterfaceUpdateUnsupported,
    MemoryHotplugConfig(memory_hotplug::MemoryHotplugConfigError),
    MemoryHotplugStatus(memory_hotplug::MemoryHotplugStatusError),
    MemoryHotplugUpdate(memory_hotplug::MemoryHotplugUpdateError),
    MemoryHotplugUnsupported,
    PmemConfig(pmem::PmemConfigError),
    PmemUpdate(pmem::PmemUpdateError),
    PmemUnsupported,
    SerialConfig(serial::SerialConfigError),
    SnapshotUnsupported,
    VsockConfig(vsock::VsockConfigError),
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
            Self::BalloonConfig(err) => write!(f, "{err}"),
            Self::BalloonUnsupported => f.write_str("Balloon device is not supported."),
            Self::BalloonStats(err) => write!(f, "{err}"),
            Self::BalloonHintingCommand(err) => write!(f, "{err}"),
            Self::BalloonHintingStatus(err) => write!(f, "{err}"),
            Self::BalloonUpdate(err) => write!(f, "{err}"),
            Self::EntropyUnsupported => f.write_str("Entropy device is not supported."),
            Self::MissingBootSource => {
                f.write_str("boot source must be configured before InstanceStart")
            }
            Self::InstanceStart(err) => write!(f, "failed to start microVM: {err}"),
            Self::Lifecycle(err) => write!(f, "failed to update microVM lifecycle: {err}"),
            Self::BootSourceConfig(err) => write!(f, "{err}"),
            Self::DriveConfig(err) => write!(f, "{err}"),
            Self::DriveUpdate(err) => write!(f, "{err}"),
            Self::DriveUpdateUnsupported => f.write_str("Drive updates are not supported."),
            Self::EntropyConfig(err) => write!(f, "{err}"),
            Self::LoggerConfig(err) => write!(f, "{err}"),
            Self::LoggerWrite(err) => write!(f, "{err}"),
            Self::MachineConfig(err) => write!(f, "{err}"),
            Self::MetricsConfig(err) => write!(f, "{err}"),
            Self::MetricsFlush(err) => write!(f, "{err}"),
            Self::MmdsConfig(err) => write!(f, "{err}"),
            Self::MmdsDataStore(err) => write!(f, "{err}"),
            Self::MmdsState(err) => write!(f, "{err}"),
            Self::NetworkInterfaceConfig(err) => write!(f, "{err}"),
            Self::NetworkInterfaceUpdate(err) => write!(f, "{err}"),
            Self::NetworkInterfaceUpdateUnsupported => {
                f.write_str("Network interface updates are not supported.")
            }
            Self::MemoryHotplugConfig(err) => write!(f, "{err}"),
            Self::MemoryHotplugStatus(err) => write!(f, "{err}"),
            Self::MemoryHotplugUpdate(err) => write!(f, "{err}"),
            Self::MemoryHotplugUnsupported => f.write_str("Memory hotplug is not supported."),
            Self::PmemConfig(err) => write!(f, "{err}"),
            Self::PmemUpdate(err) => write!(f, "{err}"),
            Self::PmemUnsupported => f.write_str("Pmem device is not supported."),
            Self::SerialConfig(err) => write!(f, "{err}"),
            Self::SnapshotUnsupported => f.write_str("Snapshot and restore are not supported."),
            Self::VsockConfig(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for VmmActionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InstanceStart(err) => Some(err),
            Self::Lifecycle(err) => Some(err),
            Self::BootSourceConfig(err) => Some(err),
            Self::BalloonConfig(err) => Some(err),
            Self::BalloonStats(err) => Some(err),
            Self::BalloonHintingCommand(err) => Some(err),
            Self::BalloonHintingStatus(err) => Some(err),
            Self::BalloonUpdate(err) => Some(err),
            Self::DriveConfig(err) => Some(err),
            Self::DriveUpdate(err) => Some(err),
            Self::EntropyConfig(err) => Some(err),
            Self::LoggerConfig(err) => Some(err),
            Self::LoggerWrite(err) => Some(err),
            Self::MachineConfig(err) => Some(err),
            Self::MetricsConfig(err) => Some(err),
            Self::MetricsFlush(err) => Some(err),
            Self::MmdsConfig(err) => Some(err),
            Self::MmdsDataStore(err) => Some(err),
            Self::MmdsState(err) => Some(err),
            Self::NetworkInterfaceConfig(err) => Some(err),
            Self::NetworkInterfaceUpdate(err) => Some(err),
            Self::MemoryHotplugConfig(err) => Some(err),
            Self::MemoryHotplugStatus(err) => Some(err),
            Self::MemoryHotplugUpdate(err) => Some(err),
            Self::PmemConfig(err) => Some(err),
            Self::PmemUpdate(err) => Some(err),
            Self::SerialConfig(err) => Some(err),
            Self::VsockConfig(err) => Some(err),
            Self::BalloonUnsupported
            | Self::DriveUpdateUnsupported
            | Self::EntropyUnsupported
            | Self::MissingBootSource
            | Self::NetworkInterfaceUpdateUnsupported
            | Self::MemoryHotplugUnsupported
            | Self::PmemUnsupported
            | Self::SnapshotUnsupported
            | Self::UnsupportedAction(_)
            | Self::UnsupportedState { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct VmmController {
    instance_info: InstanceInfo,
    machine_config: machine::MachineConfig,
    boot_source_config: Option<boot::BootSourceConfig>,
    drive_configs: block::DriveConfigs,
    network_interface_configs: network::NetworkInterfaceConfigs,
    vsock_config: Option<vsock::VsockConfig>,
    entropy_config: Option<entropy::EntropyConfig>,
    memory_hotplug_config: Option<memory_hotplug::MemoryHotplugConfig>,
    memory_hotplug_requested_size_mib: u64,
    balloon_config: Option<balloon::BalloonConfig>,
    pmem_configs: pmem::PmemConfigs,
    serial_config: serial::SerialConfig,
    logger_state: logger::LoggerState,
    metrics_state: metrics::MetricsState,
    mmds_state: mmds::MmdsStateHandle,
}

impl VmmController {
    pub fn new(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
    ) -> Self {
        Self::with_mmds_data_store_limit(
            instance_id,
            vmm_version,
            app_name,
            mmds::MMDS_DATA_STORE_LIMIT_BYTES,
        )
    }

    pub fn with_mmds_data_store_limit(
        instance_id: impl Into<String>,
        vmm_version: impl Into<String>,
        app_name: impl Into<String>,
        mmds_data_store_limit_bytes: usize,
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
            network_interface_configs: network::NetworkInterfaceConfigs::new(),
            vsock_config: None,
            entropy_config: None,
            memory_hotplug_config: None,
            memory_hotplug_requested_size_mib: 0,
            balloon_config: None,
            pmem_configs: pmem::PmemConfigs::new(),
            serial_config: serial::SerialConfig::default(),
            logger_state: logger::LoggerState::default(),
            metrics_state: metrics::MetricsState::default(),
            mmds_state: mmds::MmdsStateHandle::new(mmds::MmdsState::new(
                mmds_data_store_limit_bytes,
            )),
        }
    }

    pub fn instance_info(&self) -> &InstanceInfo {
        &self.instance_info
    }

    pub fn drive_configs(&self) -> &[block::DriveConfig] {
        self.drive_configs.as_slice()
    }

    pub fn network_interface_configs(&self) -> &[network::NetworkInterfaceConfig] {
        self.network_interface_configs.as_slice()
    }

    pub fn vsock_config(&self) -> Option<&vsock::VsockConfig> {
        self.vsock_config.as_ref()
    }

    pub const fn entropy_config(&self) -> Option<entropy::EntropyConfig> {
        self.entropy_config
    }

    pub const fn memory_hotplug_config(&self) -> Option<memory_hotplug::MemoryHotplugConfig> {
        self.memory_hotplug_config
    }

    pub fn memory_hotplug_status(&self) -> Option<memory_hotplug::MemoryHotplugStatus> {
        self.memory_hotplug_config.map(|config| {
            memory_hotplug::MemoryHotplugStatus::new(
                config,
                0,
                self.memory_hotplug_requested_size_mib,
            )
        })
    }

    pub const fn balloon_config(&self) -> Option<balloon::BalloonConfig> {
        self.balloon_config
    }

    pub fn pmem_configs(&self) -> &[pmem::PmemConfig] {
        self.pmem_configs.as_slice()
    }

    pub const fn serial_config(&self) -> &serial::SerialConfig {
        &self.serial_config
    }

    pub fn mmds_state_handle(&self) -> mmds::MmdsStateHandle {
        self.mmds_state.clone()
    }

    pub fn mmds_config(&self) -> Result<Option<mmds::MmdsConfig>, mmds::MmdsStateLockError> {
        self.mmds_state.config()
    }

    pub const fn machine_config(&self) -> machine::MachineConfig {
        self.machine_config
    }

    pub fn boot_source_config(&self) -> Option<&boot::BootSourceConfig> {
        self.boot_source_config.as_ref()
    }

    pub fn boot_timer_logger(&self) -> logger::BootTimerLogger {
        self.logger_state
            .boot_timer_logger()
            .with_missed_log_counter(self.metrics_state.missed_log_counter())
    }

    pub fn vm_config(&self) -> Result<VmConfiguration, mmds::MmdsStateLockError> {
        Ok(VmConfiguration::new(
            self.machine_config,
            self.boot_source_config.clone(),
            self.drive_configs.as_slice().to_vec(),
            self.network_interface_configs.as_slice().to_vec(),
            self.mmds_config()?,
            self.vsock_config.clone(),
            self.entropy_config,
        )
        .with_memory_hotplug_config(self.memory_hotplug_config)
        .with_balloon_config(self.balloon_config)
        .with_pmem_configs(self.pmem_configs.as_slice().to_vec()))
    }

    pub fn updated_drive_config(
        &self,
        input: block::DriveUpdateInput,
    ) -> Result<block::DriveConfig, VmmActionError> {
        self.drive_configs
            .updated_config(input)
            .map_err(VmmActionError::DriveUpdate)
    }

    pub fn commit_drive_update(
        &mut self,
        config: block::DriveConfig,
    ) -> Result<(), VmmActionError> {
        self.drive_configs
            .commit_update(config)
            .map_err(VmmActionError::DriveUpdate)
    }

    fn validate_balloon_config_target_against_memory(
        amount_mib: u32,
        mem_size_mib: u64,
    ) -> Result<(), VmmActionError> {
        if u64::from(amount_mib) > mem_size_mib {
            return Err(VmmActionError::BalloonConfig(
                balloon::BalloonConfigError::TargetExceedsGuestMemory {
                    amount_mib,
                    mem_size_mib,
                },
            ));
        }

        Ok(())
    }

    fn validate_balloon_update_target_against_memory(
        amount_mib: u32,
        mem_size_mib: u64,
    ) -> Result<(), VmmActionError> {
        if u64::from(amount_mib) > mem_size_mib {
            return Err(VmmActionError::BalloonUpdate(
                balloon::BalloonUpdateError::TargetExceedsGuestMemory {
                    amount_mib,
                    mem_size_mib,
                },
            ));
        }

        Ok(())
    }

    fn validated_balloon_config(
        &self,
        input: balloon::BalloonConfigInput,
    ) -> Result<balloon::BalloonConfig, VmmActionError> {
        let config = input.validate().map_err(VmmActionError::BalloonConfig)?;
        Self::validate_balloon_config_target_against_memory(
            config.amount_mib(),
            self.machine_config.mem_size_mib(),
        )?;
        Ok(config)
    }

    fn validate_machine_config_compatible_with_balloon(
        &self,
        config: machine::MachineConfig,
    ) -> Result<machine::MachineConfig, VmmActionError> {
        if let Some(balloon_config) = self.balloon_config
            && u64::from(balloon_config.amount_mib()) > config.mem_size_mib()
        {
            return Err(VmmActionError::MachineConfig(
                machine::MachineConfigError::IncompatibleBalloonSize,
            ));
        }

        Ok(config)
    }

    pub fn updated_balloon_config(
        &self,
        input: balloon::BalloonUpdateInput,
    ) -> Result<balloon::BalloonConfig, VmmActionError> {
        if self.instance_info.state == InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::PatchBalloon(input).name(),
                state: self.instance_info.state,
            });
        }

        let current_config = self
            .balloon_config
            .ok_or(VmmActionError::BalloonUnsupported)?;

        Self::validate_balloon_update_target_against_memory(
            input.amount_mib(),
            self.machine_config.mem_size_mib(),
        )?;

        current_config
            .updated(input)
            .map_err(VmmActionError::BalloonUpdate)
    }

    pub fn updated_balloon_stats_config(
        &self,
        input: balloon::BalloonStatsUpdateInput,
    ) -> Result<balloon::BalloonConfig, VmmActionError> {
        if self.instance_info.state == InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::PatchBalloonStats(input).name(),
                state: self.instance_info.state,
            });
        }

        self.balloon_config
            .ok_or(VmmActionError::BalloonUnsupported)?
            .updated_stats(input)
            .map_err(VmmActionError::BalloonUpdate)
    }

    pub fn commit_balloon_update(&mut self, config: balloon::BalloonConfig) {
        self.balloon_config = Some(config);
    }

    pub fn memory_hotplug_size_update(
        &self,
        input: memory_hotplug::MemoryHotplugSizeUpdateInput,
    ) -> Result<memory_hotplug::MemoryHotplugSizeUpdate, VmmActionError> {
        if self.instance_info.state == InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::PatchMemoryHotplug(input).name(),
                state: self.instance_info.state,
            });
        }

        self.memory_hotplug_config
            .ok_or(VmmActionError::MemoryHotplugUnsupported)?
            .validate_size_update(input)
            .map_err(VmmActionError::MemoryHotplugUpdate)
    }

    pub fn commit_memory_hotplug_size_update(
        &mut self,
        update: memory_hotplug::MemoryHotplugSizeUpdate,
    ) {
        self.memory_hotplug_requested_size_mib = update.requested_size_mib();
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
        self.start_instance_with(|_| Ok(())).map(|_| ())
    }

    pub fn start_instance_with<F>(&mut self, executor: F) -> Result<VmmData, VmmActionError>
    where
        F: FnOnce(&VmmController) -> Result<(), BackendError>,
    {
        self.preflight_instance_start()?;
        executor(self).map_err(VmmActionError::InstanceStart)?;
        self.log_action(VmmAction::InstanceStart.name())?;
        self.instance_info.state = InstanceState::Running;
        Ok(VmmData::Empty)
    }

    pub fn preflight_pause_instance(&self) -> Result<(), VmmActionError> {
        if self.instance_info.state != InstanceState::Running {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::Pause.name(),
                state: self.instance_info.state,
            });
        }

        Ok(())
    }

    pub fn pause_instance(&mut self) -> Result<VmmData, VmmActionError> {
        self.preflight_pause_instance()?;
        self.instance_info.state = InstanceState::Paused;
        Ok(VmmData::Empty)
    }

    pub fn preflight_resume_instance(&self) -> Result<(), VmmActionError> {
        if self.instance_info.state != InstanceState::Paused {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::Resume.name(),
                state: self.instance_info.state,
            });
        }

        Ok(())
    }

    pub fn resume_instance(&mut self) -> Result<VmmData, VmmActionError> {
        self.preflight_resume_instance()?;
        self.instance_info.state = InstanceState::Running;
        Ok(VmmData::Empty)
    }

    #[track_caller]
    fn log_action(&mut self, action: &str) -> Result<(), VmmActionError> {
        if let Err(err) = self.logger_state.log_action(action) {
            self.metrics_state.record_missed_log();
            return Err(VmmActionError::LoggerWrite(err));
        }

        Ok(())
    }

    #[track_caller]
    pub fn log_api_request(
        &mut self,
        method: &str,
        path: impl fmt::Display,
    ) -> Result<bool, VmmActionError> {
        self.logger_state
            .log_api_request(method, path)
            .inspect_err(|_| self.metrics_state.record_missed_log())
            .map_err(VmmActionError::LoggerWrite)
    }

    pub fn record_put_actions_request(&mut self) {
        self.metrics_state.record_put_actions_request();
    }

    pub fn record_put_actions_failure(&mut self) {
        self.metrics_state.record_put_actions_failure();
    }

    pub fn record_put_balloon_request(&mut self) {
        self.metrics_state.record_put_balloon_request();
    }

    pub fn record_put_balloon_failure(&mut self) {
        self.metrics_state.record_put_balloon_failure();
    }

    pub fn record_put_boot_source_request(&mut self) {
        self.metrics_state.record_put_boot_source_request();
    }

    pub fn record_put_boot_source_failure(&mut self) {
        self.metrics_state.record_put_boot_source_failure();
    }

    pub fn record_put_cpu_config_request(&mut self) {
        self.metrics_state.record_put_cpu_config_request();
    }

    pub fn record_put_cpu_config_failure(&mut self) {
        self.metrics_state.record_put_cpu_config_failure();
    }

    pub fn record_put_drive_request(&mut self) {
        self.metrics_state.record_put_drive_request();
    }

    pub fn record_put_drive_failure(&mut self) {
        self.metrics_state.record_put_drive_failure();
    }

    pub fn record_put_metrics_request(&mut self) {
        self.metrics_state.record_put_metrics_request();
    }

    pub fn record_put_metrics_failure(&mut self) {
        self.metrics_state.record_put_metrics_failure();
    }

    pub fn record_put_logger_request(&mut self) {
        self.metrics_state.record_put_logger_request();
    }

    pub fn record_put_logger_failure(&mut self) {
        self.metrics_state.record_put_logger_failure();
    }

    pub fn record_put_machine_config_request(&mut self) {
        self.metrics_state.record_put_machine_config_request();
    }

    pub fn record_put_machine_config_failure(&mut self) {
        self.metrics_state.record_put_machine_config_failure();
    }

    pub fn record_put_mmds_request(&mut self) {
        self.metrics_state.record_put_mmds_request();
    }

    pub fn record_put_mmds_failure(&mut self) {
        self.metrics_state.record_put_mmds_failure();
    }

    pub fn record_put_hotplug_memory_request(&mut self) {
        self.metrics_state.record_put_hotplug_memory_request();
    }

    pub fn record_put_hotplug_memory_failure(&mut self) {
        self.metrics_state.record_put_hotplug_memory_failure();
    }

    pub fn record_put_pmem_request(&mut self) {
        self.metrics_state.record_put_pmem_request();
    }

    pub fn record_put_pmem_failure(&mut self) {
        self.metrics_state.record_put_pmem_failure();
    }

    pub fn record_put_network_request(&mut self) {
        self.metrics_state.record_put_network_request();
    }

    pub fn record_put_network_failure(&mut self) {
        self.metrics_state.record_put_network_failure();
    }

    pub fn record_put_serial_request(&mut self) {
        self.metrics_state.record_put_serial_request();
    }

    pub fn record_put_serial_failure(&mut self) {
        self.metrics_state.record_put_serial_failure();
    }

    pub fn record_put_vsock_request(&mut self) {
        self.metrics_state.record_put_vsock_request();
    }

    pub fn record_put_vsock_failure(&mut self) {
        self.metrics_state.record_put_vsock_failure();
    }

    pub fn record_deprecated_api_call(&mut self) {
        self.metrics_state.record_deprecated_api_call();
    }

    pub fn record_pause_vm_latency_us(&mut self, duration_us: u64) {
        self.metrics_state.record_pause_vm_latency_us(duration_us);
    }

    pub fn record_resume_vm_latency_us(&mut self, duration_us: u64) {
        self.metrics_state.record_resume_vm_latency_us(duration_us);
    }

    pub fn record_full_create_snapshot_latency_us(&mut self, duration_us: u64) {
        self.metrics_state
            .record_full_create_snapshot_latency_us(duration_us);
    }

    pub fn record_diff_create_snapshot_latency_us(&mut self, duration_us: u64) {
        self.metrics_state
            .record_diff_create_snapshot_latency_us(duration_us);
    }

    pub fn record_load_snapshot_latency_us(&mut self, duration_us: u64) {
        self.metrics_state
            .record_load_snapshot_latency_us(duration_us);
    }

    pub fn record_patch_drive_request(&mut self) {
        self.metrics_state.record_patch_drive_request();
    }

    pub fn record_patch_drive_failure(&mut self) {
        self.metrics_state.record_patch_drive_failure();
    }

    pub fn record_patch_balloon_request(&mut self) {
        self.metrics_state.record_patch_balloon_request();
    }

    pub fn record_patch_balloon_failure(&mut self) {
        self.metrics_state.record_patch_balloon_failure();
    }

    pub fn record_patch_network_request(&mut self) {
        self.metrics_state.record_patch_network_request();
    }

    pub fn record_patch_network_failure(&mut self) {
        self.metrics_state.record_patch_network_failure();
    }

    pub fn record_patch_machine_config_request(&mut self) {
        self.metrics_state.record_patch_machine_config_request();
    }

    pub fn record_patch_machine_config_failure(&mut self) {
        self.metrics_state.record_patch_machine_config_failure();
    }

    pub fn record_patch_mmds_request(&mut self) {
        self.metrics_state.record_patch_mmds_request();
    }

    pub fn record_patch_mmds_failure(&mut self) {
        self.metrics_state.record_patch_mmds_failure();
    }

    pub fn record_patch_hotplug_memory_request(&mut self) {
        self.metrics_state.record_patch_hotplug_memory_request();
    }

    pub fn record_patch_hotplug_memory_failure(&mut self) {
        self.metrics_state.record_patch_hotplug_memory_failure();
    }

    pub fn record_patch_pmem_request(&mut self) {
        self.metrics_state.record_patch_pmem_request();
    }

    pub fn record_patch_pmem_failure(&mut self) {
        self.metrics_state.record_patch_pmem_failure();
    }

    pub fn record_get_balloon_request(&mut self) {
        self.metrics_state.record_get_balloon_request();
    }

    pub fn record_get_instance_info_request(&mut self) {
        self.metrics_state.record_get_instance_info_request();
    }

    pub fn record_get_vmm_version_request(&mut self) {
        self.metrics_state.record_get_vmm_version_request();
    }

    pub fn record_get_machine_config_request(&mut self) {
        self.metrics_state.record_get_machine_config_request();
    }

    pub fn record_get_mmds_request(&mut self) {
        self.metrics_state.record_get_mmds_request();
    }

    pub fn record_get_hotplug_memory_request(&mut self) {
        self.metrics_state.record_get_hotplug_memory_request();
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
            VmmAction::GetMmds => {
                let value = if self.instance_info.state == InstanceState::NotStarted {
                    self.mmds_state
                        .with_mut(mmds::MmdsState::get_or_create_data_store_value)
                        .map_err(VmmActionError::MmdsState)?
                } else {
                    self.mmds_state
                        .with(mmds::MmdsState::get_existing_data_store_value)
                        .map_err(VmmActionError::MmdsState)?
                        .map_err(VmmActionError::MmdsDataStore)?
                };

                Ok(VmmData::MmdsValue(value))
            }
            VmmAction::GetVmConfig => self
                .vm_config()
                .map(VmmData::VmConfiguration)
                .map_err(VmmActionError::MmdsState),
            VmmAction::InstanceStart => {
                self.preflight_instance_start()?;
                Err(VmmActionError::UnsupportedAction(action_name))
            }
            VmmAction::Pause => self.pause_instance(),
            VmmAction::Resume => self.resume_instance(),
            VmmAction::CreateSnapshot => {
                if self.instance_info.state != InstanceState::Paused {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                Err(VmmActionError::SnapshotUnsupported)
            }
            VmmAction::LoadSnapshot => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                Err(VmmActionError::SnapshotUnsupported)
            }
            VmmAction::FlushMetrics => {
                self.flush_metrics_with_diagnostics(&metrics::MetricsDiagnostics::default())
            }
            VmmAction::GetBalloon => self
                .balloon_config
                .map(VmmData::BalloonConfiguration)
                .ok_or(VmmActionError::BalloonUnsupported),
            VmmAction::PutBalloon(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.balloon_config = Some(self.validated_balloon_config(config)?);
                Ok(VmmData::Empty)
            }
            VmmAction::PatchBalloon(input) => {
                let config = self.updated_balloon_config(input)?;
                self.commit_balloon_update(config);
                Ok(VmmData::Empty)
            }
            VmmAction::PatchBalloonStats(input) => {
                let config = self.updated_balloon_stats_config(input)?;
                self.commit_balloon_update(config);
                Ok(VmmData::Empty)
            }
            VmmAction::GetBalloonStats
            | VmmAction::GetBalloonHintingStatus
            | VmmAction::PatchBalloonHintingStart(_)
            | VmmAction::PatchBalloonHintingStop => {
                if self.instance_info.state == InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                Err(VmmActionError::BalloonUnsupported)
            }
            VmmAction::PutEntropy(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }
                let config = config.validate().map_err(VmmActionError::EntropyConfig)?;

                self.entropy_config = Some(config);
                Ok(VmmData::Empty)
            }
            VmmAction::PutMemoryHotplug(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.memory_hotplug_config = Some(
                    config
                        .try_into()
                        .map_err(VmmActionError::MemoryHotplugConfig)?,
                );
                self.memory_hotplug_requested_size_mib = 0;
                Ok(VmmData::Empty)
            }
            VmmAction::GetMemoryHotplug => self
                .memory_hotplug_status()
                .map(VmmData::MemoryHotplugStatus)
                .ok_or(VmmActionError::MemoryHotplugUnsupported),
            VmmAction::PatchMemoryHotplug(input) => {
                let update = self.memory_hotplug_size_update(input)?;
                self.commit_memory_hotplug_size_update(update);
                Ok(VmmData::Empty)
            }
            VmmAction::PutPmem(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }
                self.pmem_configs
                    .upsert(config.try_into().map_err(VmmActionError::PmemConfig)?);
                Ok(VmmData::Empty)
            }
            VmmAction::PatchPmem(input) => {
                if self.instance_info.state == InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.pmem_configs
                    .validate_update(input)
                    .map_err(VmmActionError::PmemUpdate)?;

                Ok(VmmData::Empty)
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
            VmmAction::PutCpuConfig(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                if config.custom_template_configured() {
                    return Err(VmmActionError::UnsupportedAction(action_name));
                }

                Ok(VmmData::Empty)
            }
            VmmAction::PutLogger(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.logger_state
                    .configure(config)
                    .map_err(VmmActionError::LoggerConfig)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PutMachineConfig(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                let config = config.validate().map_err(VmmActionError::MachineConfig)?;
                self.machine_config =
                    self.validate_machine_config_compatible_with_balloon(config)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PatchMachineConfig(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                let config = config
                    .apply_to(self.machine_config)
                    .map_err(VmmActionError::MachineConfig)?;
                self.machine_config =
                    self.validate_machine_config_compatible_with_balloon(config)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PutMetrics(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.metrics_state
                    .configure(config)
                    .map_err(VmmActionError::MetricsConfig)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PutMmds(input) => {
                if self.instance_info.state == InstanceState::NotStarted {
                    self.mmds_state
                        .with_mut(|state| {
                            state.ensure_data_store_present();
                            state.put_data(input)
                        })
                        .map_err(VmmActionError::MmdsState)?
                        .map_err(VmmActionError::MmdsDataStore)?;
                } else {
                    self.mmds_state
                        .with_mut(|state| state.put_existing_data_store(input))
                        .map_err(VmmActionError::MmdsState)?
                        .map_err(VmmActionError::MmdsDataStore)?;
                }

                Ok(VmmData::Empty)
            }
            VmmAction::PatchMmds(input) => {
                if self.instance_info.state == InstanceState::NotStarted {
                    self.mmds_state
                        .with_mut(|state| {
                            state.ensure_data_store_present();
                            state.patch_data(input)
                        })
                        .map_err(VmmActionError::MmdsState)?
                        .map_err(VmmActionError::MmdsDataStore)?;
                } else {
                    self.mmds_state
                        .with_mut(|state| state.patch_existing_data_store(input))
                        .map_err(VmmActionError::MmdsState)?
                        .map_err(VmmActionError::MmdsDataStore)?;
                }

                Ok(VmmData::Empty)
            }
            VmmAction::PutMmdsConfig(input) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.mmds_state
                    .with_mut(|state| {
                        state.put_config(input, self.network_interface_configs.as_slice())
                    })
                    .map_err(VmmActionError::MmdsState)?
                    .map_err(VmmActionError::MmdsConfig)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PutSerial(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.serial_config = config.validate().map_err(VmmActionError::SerialConfig)?;

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
            VmmAction::UpdateBlockDevice(_) => {
                if self.instance_info.state == InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                Err(VmmActionError::UnsupportedAction(action_name))
            }
            VmmAction::HotUnplugDevice(input) => {
                if self.instance_info.state == InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                match input.kind() {
                    HotUnplugDeviceKind::Drive => Err(VmmActionError::DriveUpdateUnsupported),
                    HotUnplugDeviceKind::NetworkInterface => {
                        Err(VmmActionError::NetworkInterfaceUpdateUnsupported)
                    }
                    HotUnplugDeviceKind::Pmem => Err(VmmActionError::PmemUnsupported),
                }
            }
            VmmAction::PutNetworkInterface(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.network_interface_configs
                    .insert(config)
                    .map_err(VmmActionError::NetworkInterfaceConfig)?;

                Ok(VmmData::Empty)
            }
            VmmAction::UpdateNetworkInterface(input) => {
                if self.instance_info.state == InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                self.network_interface_configs
                    .validate_update(input)
                    .map_err(VmmActionError::NetworkInterfaceUpdate)?;

                Ok(VmmData::Empty)
            }
            VmmAction::PutVsock(config) => {
                if self.instance_info.state != InstanceState::NotStarted {
                    return Err(VmmActionError::UnsupportedState {
                        action: action_name,
                        state: self.instance_info.state,
                    });
                }

                let config = config.validate().map_err(VmmActionError::VsockConfig)?;
                self.vsock_config = Some(config);

                Ok(VmmData::Empty)
            }
        }
    }

    pub fn flush_metrics_with_diagnostics(
        &mut self,
        diagnostics: &metrics::MetricsDiagnostics,
    ) -> Result<VmmData, VmmActionError> {
        if self.instance_info.state == InstanceState::NotStarted {
            return Err(VmmActionError::UnsupportedState {
                action: VmmAction::FlushMetrics.name(),
                state: self.instance_info.state,
            });
        }

        self.metrics_state
            .flush_with_diagnostics(diagnostics)
            .map_err(VmmActionError::MetricsFlush)?;
        self.log_action(VmmAction::FlushMetrics.name())?;
        Ok(VmmData::Empty)
    }

    pub fn flush_startup_metrics_with_diagnostics(
        &mut self,
        diagnostics: &metrics::MetricsDiagnostics,
    ) -> Result<bool, VmmActionError> {
        self.metrics_state
            .flush_with_diagnostics(diagnostics)
            .map_err(VmmActionError::MetricsFlush)
    }

    pub fn flush_periodic_metrics_with_diagnostics(
        &mut self,
        diagnostics: &metrics::MetricsDiagnostics,
    ) -> Result<bool, VmmActionError> {
        if self.instance_info.state != InstanceState::Running {
            return Ok(false);
        }

        self.metrics_state
            .flush_with_diagnostics(diagnostics)
            .map_err(VmmActionError::MetricsFlush)
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
    use std::cell::Cell;
    use std::error::Error as _;
    use std::fs;
    use std::io::{Error, ErrorKind, Write};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        BackendError, HotUnplugDeviceInput, HotUnplugDeviceKind, InstanceState, VmmAction,
        VmmActionError, VmmController, VmmData,
        balloon::{
            BalloonConfig, BalloonConfigError, BalloonConfigInput, BalloonHintingCommandError,
            BalloonHintingStartInput, BalloonHintingStatusError, BalloonStatsError,
            BalloonStatsUpdateInput, BalloonUpdateError, BalloonUpdateInput,
        },
        block::{DriveConfigError, DriveConfigInput, DriveUpdateInput},
        boot::{
            BootCommandLineError, BootPayloadKind, BootSourceConfigError, BootSourceConfigInput,
        },
        cpu::CpuConfigInput,
        entropy::{
            EntropyConfig, EntropyConfigInput, EntropyRateLimiterConfig, EntropyTokenBucketConfig,
        },
        logger::{LoggerConfigError, LoggerConfigInput, LoggerLevel, LoggerWriteError},
        machine::{
            DEFAULT_MEM_SIZE_MIB, DEFAULT_VCPU_COUNT, MAX_MEM_SIZE_MIB, MachineConfigError,
            MachineConfigInput, MachineConfigPatchInput,
        },
        memory_hotplug::{
            MemoryHotplugConfig, MemoryHotplugConfigError, MemoryHotplugConfigInput,
            MemoryHotplugSizeUpdateInput, MemoryHotplugStatus, MemoryHotplugUpdateError,
        },
        metrics::{MetricsConfigError, MetricsConfigInput, MetricsDiagnostics},
        mmds::{
            MMDS_DATA_STORE_LIMIT_BYTES, MmdsConfigError, MmdsConfigInput, MmdsContentInput,
            MmdsDataStoreError, MmdsState, MmdsVersion,
        },
        network::{
            GuestMacAddress, MAX_NETWORK_INTERFACE_COUNT, NetworkInterfaceConfigError,
            NetworkInterfaceConfigInput, NetworkInterfaceUpdateError, NetworkInterfaceUpdateInput,
        },
        pmem::{PmemConfigError, PmemConfigInput, PmemUpdateError, PmemUpdateInput},
        serial::{SerialConfigError, SerialConfigInput, SerialRateLimiterConfig},
        vsock::{MIN_GUEST_CID, VsockConfigError, VsockConfigInput},
    };

    fn drive_input(id: &str, path: &str, is_root_device: bool) -> DriveConfigInput {
        DriveConfigInput::new(id, id, path, is_root_device)
    }

    fn network_input(id: &str, host_dev_name: &str) -> NetworkInterfaceConfigInput {
        NetworkInterfaceConfigInput::new(id, id, host_dev_name)
    }

    fn pmem_input(id: &str, path_on_host: &str) -> PmemConfigInput {
        PmemConfigInput::new(id, path_on_host)
    }

    fn pmem_update_input(id: &str) -> PmemUpdateInput {
        PmemUpdateInput::new(id, id)
    }

    fn vsock_input(guest_cid: u32, uds_path: &str) -> VsockConfigInput {
        VsockConfigInput::new(guest_cid, uds_path)
    }

    fn serial_input(serial_out_path: &str) -> SerialConfigInput {
        SerialConfigInput::new().with_serial_out_path(serial_out_path)
    }

    const fn entropy_rate_limiter_input() -> EntropyConfigInput {
        EntropyConfigInput::new().with_rate_limiter(EntropyRateLimiterConfig::new(
            Some(EntropyTokenBucketConfig::new(1, None, 100)),
            None,
        ))
    }

    fn balloon_input(amount_mib: u32, deflate_on_oom: bool) -> BalloonConfigInput {
        BalloonConfigInput::new(amount_mib, deflate_on_oom)
            .with_stats_polling_interval_s(60)
            .with_free_page_hinting(true)
            .with_free_page_reporting(false)
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
            .expect("test memory hotplug config should be valid")
    }

    const fn memory_hotplug_size_update_input() -> MemoryHotplugSizeUpdateInput {
        MemoryHotplugSizeUpdateInput::new(256)
    }

    fn boot_source_input(kernel_image_path: &str) -> BootSourceConfigInput {
        BootSourceConfigInput::new(kernel_image_path)
    }

    fn hot_unplug_controller() -> VmmController {
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
            .expect("initial drive config should be stored");
        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect("initial network interface config should be stored");

        controller
    }

    fn assert_hot_unplug_config_unchanged(controller: &VmmController, state: InstanceState) {
        assert_eq!(controller.instance_info().state, state);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(controller.drive_configs().len(), 1);
        assert_eq!(controller.drive_configs()[0].drive_id(), "rootfs");
        assert_eq!(
            controller.drive_configs()[0].path_on_host(),
            Path::new("/tmp/rootfs.ext4")
        );
        assert_eq!(controller.network_interface_configs().len(), 1);
        assert_eq!(controller.network_interface_configs()[0].iface_id(), "eth0");
        assert_eq!(
            controller.network_interface_configs()[0].host_dev_name(),
            "tap0"
        );
    }

    fn serialized_len(value: &serde_json::Value) -> usize {
        value.to_string().len()
    }

    fn mmds_content_input() -> MmdsContentInput {
        MmdsContentInput::new(serde_json::json!({"latest": {"meta-data": {}}}))
    }

    fn mmds_config_input() -> MmdsConfigInput {
        MmdsConfigInput::new(vec!["eth0".to_string()]).with_version(MmdsVersion::V2)
    }

    fn unique_metrics_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-runtime-metrics-test-{}-{nanos}-{name}",
            std::process::id()
        ))
    }

    fn unique_logger_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-runtime-logger-test-{}-{nanos}-{name}",
            std::process::id()
        ))
    }

    #[derive(Debug)]
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(Error::from(ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
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
        assert!(controller.network_interface_configs().is_empty());
        assert_eq!(controller.vsock_config(), None);
        assert_eq!(controller.serial_config().serial_out_path(), None);
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
    fn handles_get_vm_config_before_configuration() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let data = controller
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");

        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert_eq!(config.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert_eq!(config.machine_config().mem_size_mib(), DEFAULT_MEM_SIZE_MIB);
        assert_eq!(config.boot_source_config(), None);
        assert!(config.drive_configs().is_empty());
        assert!(config.network_interface_configs().is_empty());
        assert_eq!(config.mmds_config(), None);
        assert_eq!(config.vsock_config(), None);
        assert_eq!(config.entropy_config(), None);
        assert_eq!(config.memory_hotplug_config(), None);
        assert_eq!(config.balloon_config(), None);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_none());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
        assert!(controller.vsock_config().is_none());
        assert_eq!(controller.entropy_config(), None);
        assert_eq!(controller.memory_hotplug_config(), None);
        assert_eq!(controller.balloon_config(), None);
        assert_eq!(controller.serial_config().serial_out_path(), None);
    }

    #[test]
    fn handles_get_vm_config_after_configuration() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("machine config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(
                boot_source_input("/tmp/vmlinux")
                    .with_initrd_path("/tmp/initrd.img")
                    .with_boot_args("console=hvc0 reboot=k panic=1"),
            ))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("drive config should be stored");
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                network_input("eth0", "tap0").with_guest_mac("12:34:56:78:9a:bc"),
            ))
            .expect("network interface config should be stored");
        controller
            .handle_action(VmmAction::PutMmdsConfig(
                mmds_config_input()
                    .with_ipv4_address("169.254.169.254".parse().expect("valid IPv4 address"))
                    .with_imds_compat(true),
            ))
            .expect("MMDS config should be stored");
        controller
            .handle_action(VmmAction::PutVsock(
                vsock_input(3, "./v.sock").with_vsock_id("vsock0"),
            ))
            .expect("vsock config should be stored");
        controller
            .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
            .expect("entropy config should be stored");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("memory hotplug config should be stored");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("balloon config should be stored");
        controller
            .handle_action(VmmAction::PutPmem(
                pmem_input("pmem0", "/tmp/pmem.img").with_read_only(true),
            ))
            .expect("pmem config should be stored");

        let data = controller
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");

        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert_eq!(config.machine_config().vcpu_count(), 2);
        assert_eq!(config.machine_config().mem_size_mib(), 256);
        let boot_source = config
            .boot_source_config()
            .expect("boot source should be present");
        assert_eq!(boot_source.kernel_image_path(), Path::new("/tmp/vmlinux"));
        assert_eq!(
            boot_source.initrd_path(),
            Some(Path::new("/tmp/initrd.img"))
        );
        assert_eq!(
            boot_source.boot_args(),
            Some("console=hvc0 reboot=k panic=1")
        );
        assert_eq!(config.drive_configs().len(), 1);
        assert_eq!(config.drive_configs()[0].drive_id(), "rootfs");
        assert_eq!(
            config.drive_configs()[0].path_on_host(),
            Path::new("/tmp/rootfs.ext4")
        );
        assert_eq!(config.network_interface_configs().len(), 1);
        assert_eq!(config.network_interface_configs()[0].iface_id(), "eth0");
        assert_eq!(
            config.network_interface_configs()[0].host_dev_name(),
            "tap0"
        );
        assert_eq!(
            config.network_interface_configs()[0].guest_mac(),
            Some(GuestMacAddress::from_bytes([
                0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
            ]))
        );
        let mmds_config = config.mmds_config().expect("MMDS config should be present");
        assert_eq!(mmds_config.network_interfaces(), &["eth0".to_string()]);
        assert_eq!(mmds_config.version(), MmdsVersion::V2);
        assert_eq!(
            mmds_config.ipv4_address(),
            Some("169.254.169.254".parse().expect("valid IPv4 address"))
        );
        assert!(mmds_config.imds_compat());
        let vsock = config
            .vsock_config()
            .expect("vsock config should be present");
        assert_eq!(vsock.vsock_id(), Some("vsock0"));
        assert_eq!(vsock.guest_cid(), 3);
        assert_eq!(vsock.uds_path(), Path::new("./v.sock"));
        assert_eq!(config.entropy_config(), Some(EntropyConfig::new()));
        assert_eq!(
            config.memory_hotplug_config(),
            Some(memory_hotplug_config())
        );
        assert_eq!(
            config.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
        assert_eq!(config.pmem_configs().len(), 1);
        assert_eq!(config.pmem_configs()[0].id(), "pmem0");
        assert_eq!(config.pmem_configs()[0].path_on_host(), "/tmp/pmem.img");
        assert!(!config.pmem_configs()[0].root_device());
        assert!(config.pmem_configs()[0].read_only());
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn handles_get_vm_config_after_running_state() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        let data = controller
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned after start");

        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(config.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert!(config.boot_source_config().is_some());
        assert!(config.drive_configs().is_empty());
        assert!(config.network_interface_configs().is_empty());
        assert_eq!(config.mmds_config(), None);
        assert_eq!(config.vsock_config(), None);
        assert_eq!(config.entropy_config(), None);
        assert_eq!(config.memory_hotplug_config(), None);
        assert_eq!(config.balloon_config(), None);
        assert!(config.pmem_configs().is_empty());
    }

    #[test]
    fn action_names_include_start_metrics_and_logger() {
        assert_eq!(VmmAction::GetVmConfig.name(), "GetVmConfig");
        assert_eq!(VmmAction::InstanceStart.name(), "InstanceStart");
        assert_eq!(VmmAction::Pause.name(), "Pause");
        assert_eq!(VmmAction::Resume.name(), "Resume");
        assert_eq!(VmmAction::CreateSnapshot.name(), "CreateSnapshot");
        assert_eq!(VmmAction::LoadSnapshot.name(), "LoadSnapshot");
        assert_eq!(VmmAction::FlushMetrics.name(), "FlushMetrics");
        assert_eq!(VmmAction::GetBalloon.name(), "GetBalloon");
        assert_eq!(VmmAction::GetBalloonStats.name(), "GetBalloonStats");
        assert_eq!(
            VmmAction::GetBalloonHintingStatus.name(),
            "GetBalloonHintingStatus"
        );
        assert_eq!(
            VmmAction::PutBalloon(balloon_input(64, true)).name(),
            "PutBalloon"
        );
        assert_eq!(
            VmmAction::PatchBalloon(balloon_update_input(32)).name(),
            "PatchBalloon"
        );
        assert_eq!(
            VmmAction::PatchBalloonStats(balloon_stats_update_input(1)).name(),
            "PatchBalloonStats"
        );
        assert_eq!(
            VmmAction::PatchBalloonHintingStart(BalloonHintingStartInput::new(true)).name(),
            "PatchBalloonHintingStart"
        );
        assert_eq!(
            VmmAction::PatchBalloonHintingStop.name(),
            "PatchBalloonHintingStop"
        );
        assert_eq!(VmmAction::GetMemoryHotplug.name(), "GetMemoryHotplug");
        assert_eq!(
            VmmAction::PutMemoryHotplug(memory_hotplug_config_input()).name(),
            "PutMemoryHotplug"
        );
        assert_eq!(
            VmmAction::PatchMemoryHotplug(memory_hotplug_size_update_input()).name(),
            "PatchMemoryHotplug"
        );
        assert_eq!(
            VmmAction::PutEntropy(EntropyConfigInput::new()).name(),
            "PutEntropy"
        );
        assert_eq!(
            VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem.img")).name(),
            "PutPmem"
        );
        assert_eq!(
            VmmAction::PatchPmem(pmem_update_input("pmem0")).name(),
            "PatchPmem"
        );
        assert_eq!(
            VmmAction::PutCpuConfig(CpuConfigInput::noop()).name(),
            "PutCpuConfig"
        );
        assert_eq!(VmmAction::GetMmds.name(), "GetMmds");
        assert_eq!(
            VmmAction::PutLogger(LoggerConfigInput::new()).name(),
            "PutLogger"
        );
        assert_eq!(
            VmmAction::PutNetworkInterface(network_input("eth0", "tap0")).name(),
            "PutNetworkInterface"
        );
        assert_eq!(
            VmmAction::UpdateNetworkInterface(NetworkInterfaceUpdateInput::new("eth0", "eth0"))
                .name(),
            "UpdateNetworkInterface"
        );
        assert_eq!(
            VmmAction::HotUnplugDevice(HotUnplugDeviceInput::new(
                HotUnplugDeviceKind::NetworkInterface,
                "eth0"
            ))
            .name(),
            "HotUnplugDevice"
        );
        assert_eq!(
            VmmAction::PutVsock(vsock_input(3, "./v.sock")).name(),
            "PutVsock"
        );
        assert_eq!(
            VmmAction::PutSerial(serial_input("/tmp/serial.out")).name(),
            "PutSerial"
        );
        assert_eq!(VmmAction::PutMmds(mmds_content_input()).name(), "PutMmds");
        assert_eq!(
            VmmAction::PatchMmds(mmds_content_input()).name(),
            "PatchMmds"
        );
        assert_eq!(
            VmmAction::PutMmdsConfig(mmds_config_input()).name(),
            "PutMmdsConfig"
        );
    }

    #[test]
    fn instance_start_after_preflight_is_unsupported_without_mutating() {
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
    fn pause_and_resume_reject_not_started_without_mutating() {
        for action in [VmmAction::Pause, VmmAction::Resume] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");

            let err = controller
                .handle_action(action.clone())
                .expect_err("VM state update should be unsupported before start");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: action.name(),
                    state: InstanceState::NotStarted,
                }
            );
            assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
            assert!(controller.boot_source_config().is_some());
        }
    }

    #[test]
    fn pause_running_instance_transitions_to_paused() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let data = controller
            .handle_action(VmmAction::Pause)
            .expect("running instance should pause");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::Paused);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn resume_paused_instance_transitions_to_running() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Paused;

        let data = controller
            .handle_action(VmmAction::Resume)
            .expect("paused instance should resume");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn pause_and_resume_reject_invalid_runtime_state_without_mutating() {
        for (state, action) in [
            (InstanceState::Paused, VmmAction::Pause),
            (InstanceState::Running, VmmAction::Resume),
        ] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(action.clone())
                .expect_err("invalid VM state update should fail");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: action.name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
        }
    }

    #[test]
    fn create_snapshot_rejects_not_started_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::CreateSnapshot)
            .expect_err("snapshot create should require paused state");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::CreateSnapshot.name(),
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn create_snapshot_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::CreateSnapshot)
            .expect_err("snapshot create should require paused state");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::CreateSnapshot.name(),
                state: InstanceState::Running,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn create_snapshot_reaches_snapshot_fault_in_paused_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Paused;

        let err = controller
            .handle_action(VmmAction::CreateSnapshot)
            .expect_err("snapshot create should remain unsupported");

        assert_eq!(err, VmmActionError::SnapshotUnsupported);
        assert_eq!(controller.instance_info().state, InstanceState::Paused);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn load_snapshot_reaches_snapshot_fault_before_start_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::LoadSnapshot)
            .expect_err("snapshot load should remain unsupported");

        assert_eq!(err, VmmActionError::SnapshotUnsupported);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn load_snapshot_rejects_running_or_paused_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::LoadSnapshot)
                .expect_err("snapshot load should be pre-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: VmmAction::LoadSnapshot.name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
        }
    }

    #[test]
    fn get_balloon_without_config_reaches_balloon_fault_without_mutating() {
        for state in [
            InstanceState::NotStarted,
            InstanceState::Running,
            InstanceState::Paused,
        ] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::GetBalloon)
                .expect_err("balloon should remain unsupported");

            assert_eq!(err, VmmActionError::BalloonUnsupported);
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
            assert_eq!(controller.balloon_config(), None);
        }
    }

    #[test]
    fn put_balloon_stores_and_replaces_config_before_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        let data = controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("balloon should store before start");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );

        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(128, false)))
            .expect("balloon config should be replaceable before start");

        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(128, false)))
        );
    }

    #[test]
    fn put_balloon_accepts_target_equal_to_machine_memory() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let data = controller
            .handle_action(VmmAction::PutBalloon(balloon_input(
                DEFAULT_MEM_SIZE_MIB as u32,
                false,
            )))
            .expect("balloon target equal to guest memory should be accepted");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(
                DEFAULT_MEM_SIZE_MIB as u32,
                false,
            )))
        );
    }

    #[test]
    fn put_balloon_rejects_target_larger_than_default_memory_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("initial balloon should store");

        let err = controller
            .handle_action(VmmAction::PutBalloon(balloon_input(
                (DEFAULT_MEM_SIZE_MIB + 1) as u32,
                false,
            )))
            .expect_err("oversized balloon target should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonConfig(BalloonConfigError::TargetExceedsGuestMemory {
                amount_mib: (DEFAULT_MEM_SIZE_MIB + 1) as u32,
                mem_size_mib: DEFAULT_MEM_SIZE_MIB,
            })
        );
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
    }

    #[test]
    fn put_balloon_rejects_free_page_reporting_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("initial balloon should store");

        let err = controller
            .handle_action(VmmAction::PutBalloon(
                balloon_input(32, false).with_free_page_reporting(true),
            ))
            .expect_err("free-page reporting should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonConfig(BalloonConfigError::UnsupportedFreePageReporting)
        );
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
    }

    #[test]
    fn put_balloon_uses_custom_machine_memory_limit() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("custom machine memory should store");

        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(256, true)))
            .expect("balloon target equal to custom memory should store");
        let err = controller
            .handle_action(VmmAction::PutBalloon(balloon_input(257, false)))
            .expect_err("balloon target larger than custom memory should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonConfig(BalloonConfigError::TargetExceedsGuestMemory {
                amount_mib: 257,
                mem_size_mib: 256,
            })
        );
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(256, true)))
        );
    }

    #[test]
    fn get_balloon_returns_stored_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("balloon config should be stored");

        let data = controller
            .handle_action(VmmAction::GetBalloon)
            .expect("stored balloon should be returned");

        assert_eq!(
            data,
            VmmData::BalloonConfiguration(BalloonConfig::from(balloon_input(64, true)))
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn put_balloon_rejects_running_or_paused_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller
                .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
                .expect("initial balloon config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::PutBalloon(balloon_input(128, false)))
                .expect_err("balloon put should be pre-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: VmmAction::PutBalloon(balloon_input(0, false)).name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
            assert_eq!(
                controller.balloon_config(),
                Some(BalloonConfig::from(balloon_input(64, true)))
            );
        }
    }

    #[test]
    fn patch_balloon_updates_target_after_start_without_changing_other_fields() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller
                .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
                .expect("initial balloon config should be stored");
            controller.instance_info.state = state;

            let data = controller
                .handle_action(VmmAction::PatchBalloon(balloon_update_input(128)))
                .expect("balloon target should update after start");

            assert_eq!(data, VmmData::Empty);
            assert_eq!(controller.instance_info().state, state);
            assert_eq!(
                controller.balloon_config(),
                Some(
                    BalloonConfig::from(balloon_input(64, true))
                        .updated(balloon_update_input(128))
                        .expect("balloon update should be valid")
                )
            );
        }
    }

    #[test]
    fn patch_balloon_rejects_max_target_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("initial balloon config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PatchBalloon(balloon_update_input(u32::MAX)))
            .expect_err("maximum balloon target should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonUpdate(BalloonUpdateError::TargetExceedsGuestMemory {
                amount_mib: u32::MAX,
                mem_size_mib: DEFAULT_MEM_SIZE_MIB,
            })
        );
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
    }

    #[test]
    fn patch_balloon_rejects_target_larger_than_memory_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("initial balloon config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PatchBalloon(balloon_update_input(129)))
            .expect_err("balloon target larger than guest memory should fail");

        assert_eq!(
            err,
            VmmActionError::BalloonUpdate(BalloonUpdateError::TargetExceedsGuestMemory {
                amount_mib: 129,
                mem_size_mib: DEFAULT_MEM_SIZE_MIB,
            })
        );
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
    }

    #[test]
    fn patch_balloon_stats_updates_interval_after_start_without_changing_other_fields() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller
                .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
                .expect("initial balloon config should be stored");
            controller.instance_info.state = state;

            let data = controller
                .handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(30)))
                .expect("balloon stats interval should update after start");

            let updated = controller
                .balloon_config()
                .expect("balloon config should remain configured");
            assert_eq!(data, VmmData::Empty);
            assert_eq!(controller.instance_info().state, state);
            assert_eq!(updated.amount_mib(), 64);
            assert!(updated.deflate_on_oom());
            assert_eq!(updated.stats_polling_interval_s(), 30);
            assert!(updated.free_page_hinting());
            assert!(!updated.free_page_reporting());
        }
    }

    #[test]
    fn patch_balloon_stats_accepts_same_interval_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("initial balloon config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let data = controller
            .handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(60)))
            .expect("same stats interval should be accepted");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
    }

    #[test]
    fn patch_balloon_stats_rejects_enabled_state_change_without_mutating() {
        for (initial, requested) in [(0, 1), (60, 0)] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            let input = BalloonConfigInput::new(64, true)
                .with_stats_polling_interval_s(initial)
                .with_free_page_hinting(true);
            controller
                .handle_action(VmmAction::PutBalloon(input))
                .expect("initial balloon config should be stored");
            controller.instance_info.state = InstanceState::Running;

            let err = controller
                .handle_action(VmmAction::PatchBalloonStats(balloon_stats_update_input(
                    requested,
                )))
                .expect_err("stats enabled-state change should fail");

            assert_eq!(
                err,
                VmmActionError::BalloonUpdate(BalloonUpdateError::StatisticsStateChange)
            );
            assert_eq!(
                controller.balloon_config(),
                Some(BalloonConfig::from(input))
            );
        }
    }

    #[test]
    fn postboot_balloon_actions_reject_not_started_without_mutating() {
        for action in [
            VmmAction::GetBalloonStats,
            VmmAction::GetBalloonHintingStatus,
            VmmAction::PatchBalloon(balloon_update_input(32)),
            VmmAction::PatchBalloonStats(balloon_stats_update_input(1)),
            VmmAction::PatchBalloonHintingStart(BalloonHintingStartInput::new(true)),
            VmmAction::PatchBalloonHintingStop,
        ] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");

            let err = controller
                .handle_action(action.clone())
                .expect_err("balloon action should be post-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: action.name(),
                    state: InstanceState::NotStarted,
                }
            );
            assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
            assert!(controller.boot_source_config().is_some());
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
        }
    }

    #[test]
    fn postboot_balloon_actions_reach_balloon_fault_after_start_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            for action in [
                VmmAction::GetBalloonStats,
                VmmAction::GetBalloonHintingStatus,
                VmmAction::PatchBalloonStats(balloon_stats_update_input(1)),
                VmmAction::PatchBalloonHintingStart(BalloonHintingStartInput::new(true)),
                VmmAction::PatchBalloonHintingStop,
            ] {
                let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
                controller
                    .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                    .expect("boot source config should be stored");
                controller.instance_info.state = state;

                let err = controller
                    .handle_action(action)
                    .expect_err("balloon should remain unsupported");

                assert_eq!(err, VmmActionError::BalloonUnsupported);
                assert_eq!(controller.instance_info().state, state);
                assert!(controller.boot_source_config().is_some());
                assert!(controller.drive_configs().is_empty());
                assert!(controller.network_interface_configs().is_empty());
            }

            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::PatchBalloon(balloon_update_input(32)))
                .expect_err("missing balloon config should reject patch");

            assert_eq!(err, VmmActionError::BalloonUnsupported);
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
            assert_eq!(controller.balloon_config(), None);
        }
    }

    #[test]
    fn put_entropy_stores_config_before_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        let data = controller
            .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
            .expect("entropy config should be stored");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(controller.entropy_config(), Some(EntropyConfig::new()));
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn put_entropy_stores_rate_limiter_before_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
            .expect("initial entropy config should be stored");

        let data = controller
            .handle_action(VmmAction::PutEntropy(entropy_rate_limiter_input()))
            .expect("entropy rate limiter should be stored");

        let expected_limiter =
            EntropyRateLimiterConfig::new(Some(EntropyTokenBucketConfig::new(1, None, 100)), None);
        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(
            controller.entropy_config(),
            Some(EntropyConfig::new().with_rate_limiter(expected_limiter))
        );
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn put_entropy_rejects_running_or_paused_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller
                .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
                .expect("initial entropy config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::PutEntropy(entropy_rate_limiter_input()))
                .expect_err("entropy should be pre-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: VmmAction::PutEntropy(EntropyConfigInput::new()).name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
            assert_eq!(controller.entropy_config(), Some(EntropyConfig::new()));
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
        }
    }

    #[test]
    fn put_memory_hotplug_stores_and_replaces_before_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        assert_eq!(
            controller
                .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
                .expect("memory hotplug config should be stored"),
            VmmData::Empty
        );
        assert_eq!(
            controller.memory_hotplug_config(),
            Some(memory_hotplug_config())
        );

        let replacement = MemoryHotplugConfigInput::new(2048, 4, 256);
        let expected_replacement =
            MemoryHotplugConfig::try_from(replacement).expect("replacement should be valid");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(replacement))
            .expect("replacement memory hotplug config should be stored");

        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(
            controller.memory_hotplug_config(),
            Some(expected_replacement)
        );
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn get_memory_hotplug_returns_initial_status() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("memory hotplug config should be stored");

        let data = controller
            .handle_action(VmmAction::GetMemoryHotplug)
            .expect("configured memory hotplug status should be returned");

        assert_eq!(
            data,
            VmmData::MemoryHotplugStatus(memory_hotplug_config().initial_status())
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn get_memory_hotplug_without_config_returns_memory_hotplug_fault() {
        for state in [
            InstanceState::NotStarted,
            InstanceState::Running,
            InstanceState::Paused,
        ] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::GetMemoryHotplug)
                .expect_err("unconfigured memory hotplug should fail");

            assert_eq!(err, VmmActionError::MemoryHotplugUnsupported);
            assert_eq!(controller.instance_info().state, state);
            assert_eq!(controller.memory_hotplug_config(), None);
        }
    }

    #[test]
    fn put_memory_hotplug_validates_config_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("initial memory hotplug config should be stored");

        let err = controller
            .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
                1024, 3, 128,
            )))
            .expect_err("memory hotplug config should be validated");

        assert_eq!(
            err,
            VmmActionError::MemoryHotplugConfig(MemoryHotplugConfigError::BlockSizeNotPowerOfTwo)
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(
            controller.memory_hotplug_config(),
            Some(memory_hotplug_config())
        );
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn put_memory_hotplug_rejects_running_or_paused_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller
                .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
                .expect("initial memory hotplug config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
                    1024, 3, 128,
                )))
                .expect_err("memory hotplug put should be pre-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: VmmAction::PutMemoryHotplug(memory_hotplug_config_input()).name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
            assert_eq!(
                controller.memory_hotplug_config(),
                Some(memory_hotplug_config())
            );
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
        }
    }

    #[test]
    fn patch_memory_hotplug_rejects_not_started_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("memory hotplug config should be stored");

        let action = VmmAction::PatchMemoryHotplug(memory_hotplug_size_update_input());
        let err = controller
            .handle_action(action.clone())
            .expect_err("memory hotplug update should be post-boot-only");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: action.name(),
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(
            controller.memory_hotplug_config(),
            Some(memory_hotplug_config())
        );
        assert!(controller.boot_source_config().is_some());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn patch_memory_hotplug_updates_requested_size_after_start() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller
                .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
                .expect("memory hotplug config should be stored");
            controller.instance_info.state = state;

            assert_eq!(
                controller
                    .handle_action(VmmAction::PatchMemoryHotplug(
                        memory_hotplug_size_update_input(),
                    ))
                    .expect("memory hotplug update should succeed"),
                VmmData::Empty
            );

            assert_eq!(controller.instance_info().state, state);
            assert_eq!(
                controller.memory_hotplug_config(),
                Some(memory_hotplug_config())
            );
            assert_eq!(
                controller.memory_hotplug_status(),
                Some(MemoryHotplugStatus::new(memory_hotplug_config(), 0, 256))
            );
            assert!(controller.boot_source_config().is_some());
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
        }
    }

    #[test]
    fn patch_memory_hotplug_rejects_invalid_size_after_start_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("memory hotplug config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PatchMemoryHotplug(
                MemoryHotplugSizeUpdateInput::new(3),
            ))
            .expect_err("unaligned memory hotplug update should fail");

        assert_eq!(
            err,
            VmmActionError::MemoryHotplugUpdate(
                MemoryHotplugUpdateError::RequestedSizeNotMultipleOfBlockSize {
                    requested_size_mib: 3,
                    block_size_mib: 2,
                }
            )
        );
        assert_eq!(
            controller.memory_hotplug_status(),
            Some(memory_hotplug_config().initial_status())
        );
    }

    #[test]
    fn instance_start_with_memory_hotplug_invokes_executor_and_commits() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutMemoryHotplug(memory_hotplug_config_input()))
            .expect("memory hotplug config should be stored");
        let mut executor_called = false;

        let data = controller
            .start_instance_with(|startup_controller| {
                executor_called = true;
                assert_eq!(
                    startup_controller.memory_hotplug_config(),
                    Some(memory_hotplug_config())
                );
                Ok(())
            })
            .expect("memory hotplug config should not block startup preflight");

        assert!(executor_called);
        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(
            controller.memory_hotplug_config(),
            Some(memory_hotplug_config())
        );
    }

    #[test]
    fn hot_unplug_rejects_not_started_without_mutating() {
        for (kind, id) in [
            (HotUnplugDeviceKind::Drive, "rootfs"),
            (HotUnplugDeviceKind::NetworkInterface, "eth0"),
            (HotUnplugDeviceKind::Pmem, "pmem0"),
        ] {
            let mut controller = hot_unplug_controller();

            let err = controller
                .handle_action(VmmAction::HotUnplugDevice(HotUnplugDeviceInput::new(
                    kind, id,
                )))
                .expect_err("hot-unplug should be post-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: "HotUnplugDevice",
                    state: InstanceState::NotStarted,
                }
            );
            assert_hot_unplug_config_unchanged(&controller, InstanceState::NotStarted);
        }
    }

    #[test]
    fn hot_unplug_reaches_device_fault_after_start_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            for (kind, id) in [
                (HotUnplugDeviceKind::Drive, "rootfs"),
                (HotUnplugDeviceKind::NetworkInterface, "eth0"),
                (HotUnplugDeviceKind::Pmem, "pmem0"),
            ] {
                let mut controller = hot_unplug_controller();
                controller.instance_info.state = state;

                let err = controller
                    .handle_action(VmmAction::HotUnplugDevice(HotUnplugDeviceInput::new(
                        kind, id,
                    )))
                    .expect_err("hot-unplug should remain unsupported");

                let expected = match kind {
                    HotUnplugDeviceKind::Drive => VmmActionError::DriveUpdateUnsupported,
                    HotUnplugDeviceKind::NetworkInterface => {
                        VmmActionError::NetworkInterfaceUpdateUnsupported
                    }
                    HotUnplugDeviceKind::Pmem => VmmActionError::PmemUnsupported,
                };
                assert_eq!(err, expected);
                assert_hot_unplug_config_unchanged(&controller, state);
            }
        }
    }

    #[test]
    fn put_pmem_stores_and_replaces_before_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        assert_eq!(
            controller.handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem-old.img"))),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::PutPmem(
                pmem_input("pmem1", "/tmp/pmem-other.img").with_read_only(true)
            )),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::PutPmem(
                pmem_input("pmem0", "/tmp/pmem-new.img").with_read_only(true)
            )),
            Ok(VmmData::Empty)
        );

        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
        assert_eq!(controller.pmem_configs().len(), 2);
        assert_eq!(controller.pmem_configs()[0].id(), "pmem0");
        assert_eq!(
            controller.pmem_configs()[0].path_on_host(),
            "/tmp/pmem-new.img"
        );
        assert!(!controller.pmem_configs()[0].root_device());
        assert!(controller.pmem_configs()[0].read_only());
        assert_eq!(controller.pmem_configs()[1].id(), "pmem1");
        assert_eq!(
            controller.pmem_configs()[1].path_on_host(),
            "/tmp/pmem-other.img"
        );
        assert!(!controller.pmem_configs()[1].root_device());
        assert!(controller.pmem_configs()[1].read_only());
    }

    #[test]
    fn put_pmem_rejects_rate_limiter_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem-old.img")))
            .expect("pmem config should be stored");

        let err = controller
            .handle_action(VmmAction::PutPmem(
                pmem_input("pmem0", "/tmp/pmem-new.img").with_rate_limiter_configured(),
            ))
            .expect_err("configured pmem rate limiter should fail");

        assert_eq!(
            err,
            VmmActionError::PmemConfig(PmemConfigError::UnsupportedRateLimiter)
        );
        assert_eq!(controller.pmem_configs().len(), 1);
        assert_eq!(
            controller.pmem_configs()[0].path_on_host(),
            "/tmp/pmem-old.img"
        );
    }

    #[test]
    fn put_pmem_rejects_root_device_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem-old.img")))
            .expect("pmem config should be stored");

        let err = controller
            .handle_action(VmmAction::PutPmem(
                pmem_input("pmem0", "/tmp/pmem-new.img").with_root_device(true),
            ))
            .expect_err("pmem root device should fail");

        assert_eq!(
            err,
            VmmActionError::PmemConfig(PmemConfigError::UnsupportedRootDevice)
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.pmem_configs().len(), 1);
        assert_eq!(
            controller.pmem_configs()[0].path_on_host(),
            "/tmp/pmem-old.img"
        );
        assert!(!controller.pmem_configs()[0].root_device());
    }

    #[test]
    fn put_pmem_rejects_empty_path_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem-old.img")))
            .expect("pmem config should be stored");

        let err = controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "")))
            .expect_err("empty pmem path should fail");

        assert_eq!(
            err,
            VmmActionError::PmemConfig(PmemConfigError::EmptyPathOnHost)
        );
        assert_eq!(controller.pmem_configs().len(), 1);
        assert_eq!(
            controller.pmem_configs()[0].path_on_host(),
            "/tmp/pmem-old.img"
        );
    }

    #[test]
    fn put_pmem_rejects_invalid_id_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem-old.img")))
            .expect("pmem config should be stored");

        let err = controller
            .handle_action(VmmAction::PutPmem(pmem_input(
                "pmem0/secret",
                "/tmp/pmem-new.img",
            )))
            .expect_err("invalid pmem id should fail");

        assert_eq!(
            err,
            VmmActionError::PmemConfig(PmemConfigError::InvalidPmemId)
        );
        assert_eq!(controller.pmem_configs().len(), 1);
        assert_eq!(controller.pmem_configs()[0].id(), "pmem0");
        assert_eq!(
            controller.pmem_configs()[0].path_on_host(),
            "/tmp/pmem-old.img"
        );
    }

    #[test]
    fn patch_pmem_rejects_not_started_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::PatchPmem(pmem_update_input("pmem0")))
            .expect_err("pmem patch should be post-boot-only");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::PatchPmem(pmem_update_input("pmem0")).name(),
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
        assert!(controller.pmem_configs().is_empty());
    }

    #[test]
    fn put_pmem_rejects_running_or_paused_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem.img")))
                .expect_err("pmem put should be pre-boot-only");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem.img")).name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
            assert!(controller.pmem_configs().is_empty());
        }
    }

    #[test]
    fn patch_pmem_noop_succeeds_after_start_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem.img")))
                .expect("pmem config should be stored");
            controller.instance_info.state = state;

            let data = controller
                .handle_action(VmmAction::PatchPmem(pmem_update_input("pmem0")))
                .expect("no-op pmem patch should succeed");

            assert_eq!(data, VmmData::Empty);
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.drive_configs().is_empty());
            assert!(controller.network_interface_configs().is_empty());
            assert_eq!(controller.pmem_configs().len(), 1);
            assert_eq!(controller.pmem_configs()[0].id(), "pmem0");
            assert_eq!(controller.pmem_configs()[0].path_on_host(), "/tmp/pmem.img");
        }
    }

    #[test]
    fn patch_pmem_rejects_unknown_pmem_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem.img")))
            .expect("pmem config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PatchPmem(pmem_update_input("pmem1")))
            .expect_err("missing pmem patch should fail");

        assert_eq!(
            err,
            VmmActionError::PmemUpdate(PmemUpdateError::UnknownPmem)
        );
        assert_eq!(err.to_string(), "pmem device is not configured");
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(controller.pmem_configs().len(), 1);
        assert_eq!(controller.pmem_configs()[0].id(), "pmem0");
        assert_eq!(controller.pmem_configs()[0].path_on_host(), "/tmp/pmem.img");
    }

    #[test]
    fn patch_pmem_rejects_configured_rate_limiter_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutPmem(pmem_input("pmem0", "/tmp/pmem.img")))
            .expect("pmem config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PatchPmem(
                pmem_update_input("pmem0").with_rate_limiter_configured(),
            ))
            .expect_err("configured pmem rate limiter should fail");

        assert_eq!(
            err,
            VmmActionError::PmemUpdate(PmemUpdateError::UnsupportedRateLimiter)
        );
        assert_eq!(err.to_string(), "pmem rate_limiter is not supported");
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(controller.pmem_configs().len(), 1);
        assert_eq!(controller.pmem_configs()[0].id(), "pmem0");
        assert_eq!(controller.pmem_configs()[0].path_on_host(), "/tmp/pmem.img");
    }

    #[test]
    fn put_cpu_config_noop_succeeds_not_started_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        assert_eq!(
            controller.handle_action(VmmAction::PutCpuConfig(CpuConfigInput::noop())),
            Ok(VmmData::Empty)
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn put_cpu_config_custom_template_rejects_not_started_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        let action = VmmAction::PutCpuConfig(CpuConfigInput::with_custom_template());
        let err = controller
            .handle_action(action.clone())
            .expect_err("custom CPU config should remain unsupported");
        assert_eq!(err, VmmActionError::UnsupportedAction(action.name()));
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn put_cpu_config_rejects_running_or_paused_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
                .expect("boot source config should be stored");
            controller.instance_info.state = state;

            let err = controller
                .handle_action(VmmAction::PutCpuConfig(CpuConfigInput::noop()))
                .expect_err("CPU config should reject runtime mutation");

            assert_eq!(
                err,
                VmmActionError::UnsupportedState {
                    action: VmmAction::PutCpuConfig(CpuConfigInput::noop()).name(),
                    state,
                }
            );
            assert_eq!(controller.instance_info().state, state);
            assert!(controller.boot_source_config().is_some());
        }
    }

    #[test]
    fn instance_start_action_requires_boot_source_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::InstanceStart)
            .expect_err("missing boot source should fail action preflight");

        assert_eq!(err, VmmActionError::MissingBootSource);
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert!(controller.boot_source_config().is_none());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn get_mmds_returns_null_before_start_and_creates_data_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::Value::Null))
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert!(controller.boot_source_config().is_none());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
        assert_eq!(controller.vsock_config(), None);
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(true)
        );
    }

    #[test]
    fn get_mmds_requires_data_store_after_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        let err = controller
            .handle_action(VmmAction::GetMmds)
            .expect_err("runtime uninitialized MMDS read should fail");

        assert_eq!(
            err,
            VmmActionError::MmdsDataStore(MmdsDataStoreError::NotInitialized)
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(
            controller.mmds_state.with(MmdsState::get_data),
            Ok(Err(MmdsDataStoreError::NotInitialized))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(false)
        );
    }

    #[test]
    fn get_mmds_after_preboot_get_returns_null_after_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::Value::Null))
        );
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::Value::Null))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::get_data),
            Ok(Err(MmdsDataStoreError::NotInitialized))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(true)
        );
    }

    #[test]
    fn get_mmds_returns_initialized_data_after_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone())))
            .expect("MMDS data should configure");
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(value))
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
    }

    #[test]
    fn put_and_get_mmds_data_store_json() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let value = serde_json::json!({
            "latest": {
                "meta-data": {
                    "ami-id": "ami-123",
                },
            },
        });

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone()))),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(value))
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
    }

    #[test]
    fn put_mmds_requires_data_store_after_start() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value))),
            Err(VmmActionError::MmdsDataStore(
                MmdsDataStoreError::NotInitialized
            ))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(false)
        );
    }

    #[test]
    fn put_mmds_after_preboot_get_initializes_runtime_data_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        controller
            .handle_action(VmmAction::GetMmds)
            .expect("preboot MMDS GET should create the store");
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone()))),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(value))
        );
    }

    #[test]
    fn put_mmds_after_preboot_config_initializes_runtime_data_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect("network interface config should be stored");
        controller
            .handle_action(VmmAction::PutMmdsConfig(mmds_config_input()))
            .expect("MMDS config should create the store");
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone()))),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(value))
        );
    }

    #[test]
    fn put_mmds_after_failed_preboot_patch_initializes_runtime_data_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        assert_eq!(
            controller.handle_action(VmmAction::PatchMmds(mmds_content_input())),
            Err(VmmActionError::MmdsDataStore(
                MmdsDataStoreError::NotInitialized
            ))
        );
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone()))),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(value))
        );
    }

    #[test]
    fn patch_mmds_data_store_applies_json_merge_patch() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let original = serde_json::json!({
            "latest": {
                "meta-data": {
                    "ami-id": "ami-old",
                    "remove-me": true,
                },
                "user-data": "before",
            },
        });
        let patch = serde_json::json!({
            "latest": {
                "meta-data": {
                    "ami-id": "ami-new",
                    "remove-me": null,
                },
                "dynamic": {
                    "instance-identity": "document",
                },
            },
        });

        controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(original)))
            .expect("MMDS put should initialize data store");
        assert_eq!(
            controller.handle_action(VmmAction::PatchMmds(MmdsContentInput::new(patch))),
            Ok(VmmData::Empty)
        );

        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::json!({
                "latest": {
                    "meta-data": {
                        "ami-id": "ami-new",
                    },
                    "user-data": "before",
                    "dynamic": {
                        "instance-identity": "document",
                    },
                },
            })))
        );
    }

    #[test]
    fn patch_mmds_requires_initialized_data_store_without_mutating_data() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::PatchMmds(mmds_content_input()))
            .expect_err("patching uninitialized MMDS should fail");

        assert_eq!(
            err,
            VmmActionError::MmdsDataStore(MmdsDataStoreError::NotInitialized)
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::get_data),
            Ok(Err(MmdsDataStoreError::NotInitialized))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(true)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::Value::Null))
        );
    }

    #[test]
    fn patch_mmds_after_preboot_get_still_requires_initialized_data() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source should configure");
        controller
            .handle_action(VmmAction::GetMmds)
            .expect("preboot MMDS GET should create the store");
        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::PatchMmds(mmds_content_input())),
            Err(VmmActionError::MmdsDataStore(
                MmdsDataStoreError::NotInitialized
            ))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(true)
        );
    }

    #[test]
    fn put_mmds_rejects_non_object_value_without_initializing_data() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(
                serde_json::json!("not-an-object"),
            ))),
            Err(VmmActionError::MmdsDataStore(
                MmdsDataStoreError::InvalidObject
            ))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::data_store_present),
            Ok(true)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::Value::Null))
        );
    }

    #[test]
    fn patch_mmds_rejects_non_object_value_without_mutating_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let original = serde_json::json!({"latest": {"meta-data": {}}});
        controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(original.clone())))
            .expect("initial MMDS put should succeed");

        assert_eq!(
            controller.handle_action(VmmAction::PatchMmds(MmdsContentInput::new(
                serde_json::json!("not-an-object"),
            ))),
            Err(VmmActionError::MmdsDataStore(
                MmdsDataStoreError::InvalidObject
            ))
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(original))
        );
    }

    #[test]
    fn oversized_put_mmds_does_not_mutate_existing_data_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let original = serde_json::json!({"latest": {"meta-data": {}}});
        let oversized = serde_json::json!({"data": "x".repeat(MMDS_DATA_STORE_LIMIT_BYTES)});

        controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(original.clone())))
            .expect("initial MMDS put should succeed");
        let err = controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(oversized)))
            .expect_err("oversized MMDS put should fail");

        let VmmActionError::MmdsDataStore(MmdsDataStoreError::DataStoreLimitExceeded {
            limit_bytes,
            size_bytes,
        }) = err
        else {
            panic!("expected MMDS data store limit error");
        };
        assert_eq!(limit_bytes, MMDS_DATA_STORE_LIMIT_BYTES);
        assert!(size_bytes > limit_bytes);
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(original))
        );
    }

    #[test]
    fn controller_with_mmds_data_store_limit_accepts_exact_limit() {
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        let mut controller = VmmController::with_mmds_data_store_limit(
            "demo-1",
            "0.1.0",
            "bangbang",
            serialized_len(&value),
        );

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone()))),
            Ok(VmmData::Empty)
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(value))
        );
    }

    #[test]
    fn controller_with_mmds_data_store_limit_rejects_one_byte_over_without_initializing() {
        let value = serde_json::json!({"latest": {"meta-data": {"ami-id": "ami-123"}}});
        let limit_bytes = serialized_len(&value) - 1;
        let mut controller =
            VmmController::with_mmds_data_store_limit("demo-1", "0.1.0", "bangbang", limit_bytes);

        assert_eq!(
            controller.handle_action(VmmAction::PutMmds(MmdsContentInput::new(value.clone()))),
            Err(VmmActionError::MmdsDataStore(
                MmdsDataStoreError::DataStoreLimitExceeded {
                    limit_bytes,
                    size_bytes: serialized_len(&value),
                }
            ))
        );
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::Value::Null))
        );
        assert_eq!(
            controller.mmds_state.with(MmdsState::get_data),
            Ok(Err(MmdsDataStoreError::NotInitialized))
        );
    }

    #[test]
    fn controller_with_mmds_data_store_limit_rejects_patch_without_mutating() {
        let original = serde_json::json!({"latest": {"meta-data": {}}});
        let limit_bytes = serialized_len(&original);
        let oversized_patch = serde_json::json!({
            "latest": {
                "user-data": "x".repeat(64),
            },
        });
        let mut controller =
            VmmController::with_mmds_data_store_limit("demo-1", "0.1.0", "bangbang", limit_bytes);

        controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(original.clone())))
            .expect("initial exact-limit MMDS put should succeed");
        let err = controller
            .handle_action(VmmAction::PatchMmds(MmdsContentInput::new(oversized_patch)))
            .expect_err("oversized MMDS patch should fail");

        let VmmActionError::MmdsDataStore(MmdsDataStoreError::DataStoreLimitExceeded {
            limit_bytes: actual_limit_bytes,
            size_bytes,
        }) = err
        else {
            panic!("expected MMDS data store limit error");
        };
        assert_eq!(actual_limit_bytes, limit_bytes);
        assert!(size_bytes > actual_limit_bytes);
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(original))
        );
    }

    #[test]
    fn oversized_patch_mmds_does_not_mutate_existing_data_store() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let original = serde_json::json!({"latest": {"meta-data": {}}});
        let oversized_patch = serde_json::json!({
            "latest": {
                "user-data": "x".repeat(MMDS_DATA_STORE_LIMIT_BYTES),
            },
        });

        controller
            .handle_action(VmmAction::PutMmds(MmdsContentInput::new(original.clone())))
            .expect("initial MMDS put should succeed");
        let err = controller
            .handle_action(VmmAction::PatchMmds(MmdsContentInput::new(oversized_patch)))
            .expect_err("oversized MMDS patch should fail");

        let VmmActionError::MmdsDataStore(MmdsDataStoreError::DataStoreLimitExceeded {
            limit_bytes,
            size_bytes,
        }) = err
        else {
            panic!("expected MMDS data store limit error");
        };
        assert_eq!(limit_bytes, MMDS_DATA_STORE_LIMIT_BYTES);
        assert!(size_bytes > limit_bytes);
        assert_eq!(
            controller.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(original))
        );
    }

    #[test]
    fn mmds_config_validates_existing_network_interface_ids() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::PutMmdsConfig(mmds_config_input()))
            .expect_err("unknown interface id should fail");
        assert_eq!(
            err,
            VmmActionError::MmdsConfig(MmdsConfigError::UnknownNetworkInterfaceId {
                iface_id: "eth0".to_string(),
            })
        );
        assert_eq!(controller.mmds_config(), Ok(None));

        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect("network interface config should be stored");
        assert_eq!(
            controller.handle_action(VmmAction::PutMmdsConfig(
                mmds_config_input()
                    .with_ipv4_address("169.254.169.254".parse().expect("valid IPv4 address"))
                    .with_imds_compat(true),
            )),
            Ok(VmmData::Empty)
        );
        let config = controller
            .mmds_config()
            .expect("MMDS state should lock")
            .expect("MMDS config should be stored");
        assert_eq!(config.network_interfaces(), &["eth0".to_string()]);
        assert_eq!(config.version(), MmdsVersion::V2);
        assert_eq!(
            config.ipv4_address(),
            Some("169.254.169.254".parse().expect("valid IPv4 address"))
        );
        assert!(config.imds_compat());
    }

    #[test]
    fn mmds_config_rejects_empty_interfaces_and_invalid_ipv4() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect("network interface config should be stored");

        assert_eq!(
            controller.handle_action(VmmAction::PutMmdsConfig(MmdsConfigInput::new(Vec::new()))),
            Err(VmmActionError::MmdsConfig(
                MmdsConfigError::EmptyNetworkInterfaceList
            ))
        );
        assert_eq!(
            controller.handle_action(VmmAction::PutMmdsConfig(
                mmds_config_input()
                    .with_ipv4_address("169.254.0.1".parse().expect("valid IPv4 address")),
            )),
            Err(VmmActionError::MmdsConfig(
                MmdsConfigError::InvalidIpv4Address(
                    "169.254.0.1".parse().expect("valid IPv4 address")
                )
            ))
        );
        assert_eq!(controller.mmds_config(), Ok(None));
    }

    #[test]
    fn mmds_config_action_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        let err = controller
            .handle_action(VmmAction::PutMmdsConfig(mmds_config_input()))
            .expect_err("running MMDS config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutMmdsConfig",
                state: InstanceState::Running,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert!(controller.boot_source_config().is_some());
        assert!(controller.drive_configs().is_empty());
        assert!(controller.network_interface_configs().is_empty());
        assert_eq!(controller.vsock_config(), None);
    }

    #[test]
    fn instance_start_action_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::InstanceStart)
            .expect_err("running state should fail action preflight");

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
    fn instance_start_preflight_accepts_configured_balloon_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("balloon config should be stored");

        assert_eq!(controller.preflight_instance_start(), Ok(()));
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
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
    fn instance_start_preflight_rejects_paused_state() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Paused;

        let err = controller
            .preflight_instance_start()
            .expect_err("paused state should fail preflight");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::InstanceStart.name(),
                state: InstanceState::Paused,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::Paused);
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
    fn start_instance_with_commits_running_after_executor_success() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("machine config should be stored");
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

        let data = controller
            .start_instance_with(|startup_controller| {
                assert_eq!(
                    startup_controller.instance_info().state,
                    InstanceState::NotStarted
                );
                assert_eq!(startup_controller.machine_config().vcpu_count(), 2);
                assert!(startup_controller.boot_source_config().is_some());
                assert_eq!(startup_controller.drive_configs().len(), 1);
                Ok(())
            })
            .expect("startup executor success should commit running state");

        assert_eq!(data, VmmData::Empty);
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert!(controller.boot_source_config().is_some());
        assert_eq!(controller.drive_configs().len(), 1);
    }

    #[test]
    fn start_instance_with_executor_failure_preserves_state_and_source() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        let source = BackendError::InvalidState("fake startup failed");

        let err = controller
            .start_instance_with(|_| Err(source.clone()))
            .expect_err("startup executor failure should be surfaced");

        assert_eq!(err, VmmActionError::InstanceStart(source.clone()));
        assert_eq!(
            err.to_string(),
            "failed to start microVM: invalid backend state: fake startup failed"
        );
        let error_source = err.source().expect("startup error should preserve source");
        assert_eq!(
            error_source
                .downcast_ref::<BackendError>()
                .expect("startup source should be a backend error"),
            &source
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_some());
    }

    #[test]
    fn start_instance_with_configured_logger_writes_action_log() {
        let path = unique_logger_path("start-action");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_show_level(true),
            ))
            .expect("logger config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");

        controller
            .start_instance_with(|_| Ok(()))
            .expect("startup executor success should commit running state");

        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            "level=Info action=InstanceStart\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn start_instance_logger_write_failure_reports_missed_log_count_in_metrics() {
        let metrics_path = unique_metrics_path("start-missed-log");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &metrics_path,
            )))
            .expect("metrics config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.logger_state.configure_test_writer(FailingWriter);

        let err = controller
            .commit_instance_start()
            .expect_err("logger write should fail startup commit");

        assert_eq!(
            err,
            VmmActionError::LoggerWrite(LoggerWriteError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(
            controller.flush_startup_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"logger\":{\"missed_log_count\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn boot_timer_logger_write_failure_reports_missed_log_count_in_metrics() {
        let metrics_path = unique_metrics_path("boot-timer-missed-log");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &metrics_path,
            )))
            .expect("metrics config should be stored");
        controller.logger_state.configure_test_writer(FailingWriter);
        let boot_timer_logger = controller.boot_timer_logger();

        let err = boot_timer_logger
            .log_boot_time(1_000, 200)
            .expect_err("boot timer logger write should fail");

        assert_eq!(err, LoggerWriteError::Write(ErrorKind::BrokenPipe));
        assert_eq!(
            controller.flush_startup_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"logger\":{\"missed_log_count\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn api_request_logger_write_failure_reports_missed_log_count_in_metrics() {
        let metrics_path = unique_metrics_path("api-request-missed-log");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &metrics_path,
            )))
            .expect("metrics config should be stored");
        controller.logger_state.configure_test_writer(FailingWriter);

        let err = controller
            .log_api_request("Put", "/mmds")
            .expect_err("API request logger write should fail");

        assert_eq!(
            err,
            VmmActionError::LoggerWrite(LoggerWriteError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(
            controller.flush_startup_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"logger\":{\"missed_log_count\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn boot_timer_logger_missed_log_count_is_per_controller() {
        let first_metrics_path = unique_metrics_path("boot-timer-first-controller");
        let second_metrics_path = unique_metrics_path("boot-timer-second-controller");
        let mut first = VmmController::new("demo-1", "0.1.0", "bangbang");
        let mut second = VmmController::new("demo-2", "0.1.0", "bangbang");
        first
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &first_metrics_path,
            )))
            .expect("first metrics config should be stored");
        second
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &second_metrics_path,
            )))
            .expect("second metrics config should be stored");
        first.logger_state.configure_test_writer(FailingWriter);
        let first_boot_timer_logger = first.boot_timer_logger();

        assert_eq!(
            first_boot_timer_logger.log_boot_time(1_000, 200),
            Err(LoggerWriteError::Write(ErrorKind::BrokenPipe))
        );

        assert_eq!(
            first.flush_startup_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );
        assert_eq!(
            second.flush_startup_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );
        assert_eq!(
            fs::read_to_string(&first_metrics_path)
                .expect("first metrics output should be readable"),
            "{\"logger\":{\"missed_log_count\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert_eq!(
            fs::read_to_string(&second_metrics_path)
                .expect("second metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(first_metrics_path).expect("first metrics fixture should clean up");
        fs::remove_file(second_metrics_path).expect("second metrics fixture should clean up");
    }

    #[test]
    fn start_instance_with_executor_failure_does_not_write_logger_action() {
        let path = unique_logger_path("start-action-failure");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_log_path(&path),
            ))
            .expect("logger config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        let source = BackendError::InvalidState("fake startup failed");

        let err = controller
            .start_instance_with(|_| Err(source.clone()))
            .expect_err("startup executor failure should be surfaced");

        assert_eq!(err, VmmActionError::InstanceStart(source));
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            ""
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn start_instance_with_missing_boot_source_does_not_invoke_executor() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        let called = Cell::new(false);

        let err = controller
            .start_instance_with(|_| {
                called.set(true);
                Ok(())
            })
            .expect_err("missing boot source should fail before executor");

        assert_eq!(err, VmmActionError::MissingBootSource);
        assert!(!called.get());
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert!(controller.boot_source_config().is_none());
    }

    #[test]
    fn start_instance_with_configured_balloon_invokes_executor() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(64, true)))
            .expect("balloon config should be stored");
        let called = Cell::new(false);

        controller
            .start_instance_with(|_| {
                called.set(true);
                Ok(())
            })
            .expect("configured balloon should reach executor");

        assert!(called.get());
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(64, true)))
        );
    }

    #[test]
    fn start_instance_with_running_state_does_not_invoke_executor() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("first commit should succeed");
        let called = Cell::new(false);

        let err = controller
            .start_instance_with(|_| {
                called.set(true);
                Ok(())
            })
            .expect_err("running state should fail before executor");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::InstanceStart.name(),
                state: InstanceState::Running,
            }
        );
        assert!(!called.get());
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
    fn flush_metrics_rejects_not_started_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::FlushMetrics)
            .expect_err("pre-boot flush metrics should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::FlushMetrics.name(),
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.machine_config().vcpu_count(), DEFAULT_VCPU_COUNT);
        assert!(controller.boot_source_config().is_none());
        assert!(controller.drive_configs().is_empty());
    }

    #[test]
    fn flush_metrics_after_start_without_configuration_is_noop() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        assert_eq!(
            controller.handle_action(VmmAction::FlushMetrics),
            Ok(VmmData::Empty)
        );
        assert_eq!(controller.instance_info().state, InstanceState::Running);
    }

    #[test]
    fn flush_metrics_while_paused_without_configuration_is_noop() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller.instance_info.state = InstanceState::Paused;

        assert_eq!(
            controller.handle_action(VmmAction::FlushMetrics),
            Ok(VmmData::Empty)
        );
        assert_eq!(controller.instance_info().state, InstanceState::Paused);
    }

    #[test]
    fn flush_metrics_after_start_writes_configured_logger_action() {
        let path = unique_logger_path("flush-action");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_log_path(&path),
            ))
            .expect("logger config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        assert_eq!(
            controller.handle_action(VmmAction::FlushMetrics),
            Ok(VmmData::Empty)
        );

        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            "action=InstanceStart\naction=FlushMetrics\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn flush_metrics_logger_write_failure_reports_missed_log_count_in_metrics() {
        let metrics_path = unique_metrics_path("flush-missed-log");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &metrics_path,
            )))
            .expect("metrics config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");
        controller.logger_state.configure_test_writer(FailingWriter);

        let err = controller
            .handle_action(VmmAction::FlushMetrics)
            .expect_err("flush metrics action logger write should fail");

        assert_eq!(
            err,
            VmmActionError::LoggerWrite(LoggerWriteError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(
            controller.flush_periodic_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n{\"logger\":{\"missed_log_count\":1},\"vmm\":{\"metrics_flush_count\":2}}\n"
        );

        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
    }

    #[test]
    fn handles_put_metrics_config_before_start_and_flushes_after_start() {
        let path = unique_metrics_path("configured");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&path))),
            Ok(VmmData::Empty)
        );
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");
        assert_eq!(
            controller.handle_action(VmmAction::FlushMetrics),
            Ok(VmmData::Empty)
        );

        assert_eq!(controller.instance_info().state, InstanceState::Running);
        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(output, "{\"vmm\":{\"metrics_flush_count\":1}}\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn flush_metrics_before_start_with_configuration_does_not_write() {
        let path = unique_metrics_path("configured-preboot");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&path)))
            .expect("metrics config should be stored");

        let err = controller
            .handle_action(VmmAction::FlushMetrics)
            .expect_err("pre-boot metrics flush should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::FlushMetrics.name(),
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(
            fs::read_to_string(&path).expect("metrics output should be readable"),
            ""
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn startup_metrics_before_start_writes_without_logger_action() {
        let metrics_path = unique_metrics_path("startup-preboot");
        let logger_path = unique_logger_path("startup-no-action");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &metrics_path,
            )))
            .expect("metrics config should be stored");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_log_path(&logger_path),
            ))
            .expect("logger config should be stored");

        assert_eq!(
            controller.flush_startup_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );

        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert_eq!(
            fs::read_to_string(&logger_path).expect("logger output should be readable"),
            ""
        );
        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
        fs::remove_file(logger_path).expect("logger fixture should clean up");
    }

    #[test]
    fn periodic_metrics_before_start_with_configuration_does_not_write() {
        let path = unique_metrics_path("periodic-preboot");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&path)))
            .expect("metrics config should be stored");

        assert_eq!(
            controller.flush_periodic_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(false)
        );

        assert_eq!(
            fs::read_to_string(&path).expect("metrics output should be readable"),
            ""
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn periodic_metrics_after_start_writes_without_logger_action() {
        let metrics_path = unique_metrics_path("periodic-running");
        let logger_path = unique_logger_path("periodic-no-action");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                &metrics_path,
            )))
            .expect("metrics config should be stored");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_log_path(&logger_path),
            ))
            .expect("logger config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        assert_eq!(
            controller.flush_periodic_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(true)
        );

        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        assert_eq!(
            fs::read_to_string(&logger_path).expect("logger output should be readable"),
            "action=InstanceStart\n"
        );
        fs::remove_file(metrics_path).expect("metrics fixture should clean up");
        fs::remove_file(logger_path).expect("logger fixture should clean up");
    }

    #[test]
    fn periodic_metrics_while_paused_does_not_write() {
        let path = unique_metrics_path("periodic-paused");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&path)))
            .expect("metrics config should be stored");
        controller.instance_info.state = InstanceState::Paused;

        assert_eq!(
            controller.flush_periodic_metrics_with_diagnostics(&MetricsDiagnostics::default()),
            Ok(false)
        );
        assert_eq!(
            fs::read_to_string(&path).expect("metrics output should be readable"),
            ""
        );

        fs::remove_file(path).expect("metrics fixture should clean up");
    }

    #[test]
    fn put_metrics_rejects_duplicate_configuration_without_replacing_sink() {
        let first_path = unique_metrics_path("first");
        let second_path = unique_metrics_path("second");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&first_path)))
            .expect("initial metrics config should be stored");

        let err = controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&second_path)))
            .expect_err("duplicate metrics config should fail");

        assert_eq!(
            err,
            VmmActionError::MetricsConfig(MetricsConfigError::AlreadyInitialized)
        );
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");
        controller
            .handle_action(VmmAction::FlushMetrics)
            .expect("flush should still use original metrics sink");
        let first_output =
            fs::read_to_string(&first_path).expect("first metrics output should be readable");
        assert_eq!(first_output, "{\"vmm\":{\"metrics_flush_count\":1}}\n");
        assert!(!second_path.exists());
        fs::remove_file(first_path).expect("fixture should clean up");
    }

    #[test]
    fn put_metrics_rejects_running_state_without_mutating() {
        let path = unique_metrics_path("running");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        let err = controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(&path)))
            .expect_err("runtime metrics config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::PutMetrics(MetricsConfigInput::new(&path)).name(),
                state: InstanceState::Running,
            }
        );
        assert_eq!(
            controller.handle_action(VmmAction::FlushMetrics),
            Ok(VmmData::Empty)
        );
        assert!(!path.exists());
    }

    #[test]
    fn put_metrics_rejects_empty_path_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
                PathBuf::new(),
            )))
            .expect_err("empty metrics path should fail");

        assert_eq!(
            err,
            VmmActionError::MetricsConfig(MetricsConfigError::EmptyPath)
        );
        assert_eq!(
            controller.handle_action(VmmAction::FlushMetrics),
            Err(VmmActionError::UnsupportedState {
                action: VmmAction::FlushMetrics.name(),
                state: InstanceState::NotStarted,
            })
        );
    }

    #[test]
    fn handles_put_logger_config_before_start() {
        let path = unique_logger_path("configured");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn)
                    .with_show_level(true)
                    .with_show_log_origin(true)
                    .with_module("api_server"),
            )),
            Ok(VmmData::Empty)
        );

        assert!(controller.logger_state.is_configured());
        assert_eq!(controller.logger_state.level(), LoggerLevel::Warn);
        assert!(controller.logger_state.show_level());
        assert!(controller.logger_state.show_log_origin());
        assert_eq!(controller.logger_state.module(), Some("api_server"));
        assert!(path.exists());
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn put_logger_config_updates_without_requiring_log_path() {
        let path = unique_logger_path("repeat");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn),
            ))
            .expect("initial logger config should be stored");

        assert_eq!(
            controller.handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_level(LoggerLevel::Debug)
                    .with_show_level(true)
                    .with_module("runtime"),
            )),
            Ok(VmmData::Empty)
        );

        assert!(controller.logger_state.is_configured());
        assert!(path.exists());
        assert_eq!(controller.logger_state.level(), LoggerLevel::Debug);
        assert!(controller.logger_state.show_level());
        assert!(!controller.logger_state.show_log_origin());
        assert_eq!(controller.logger_state.module(), Some("runtime"));
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn put_logger_state_is_per_controller() {
        let path = unique_logger_path("isolated");
        let mut first = VmmController::new("demo-1", "0.1.0", "bangbang");
        let mut second = VmmController::new("demo-2", "0.1.0", "bangbang");

        first
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Error),
            ))
            .expect("first logger config should be stored");
        second
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_level(LoggerLevel::Debug),
            ))
            .expect("second logger config should be stored");

        assert!(first.logger_state.is_configured());
        assert_eq!(first.logger_state.level(), LoggerLevel::Error);
        assert!(!second.logger_state.is_configured());
        assert_eq!(second.logger_state.level(), LoggerLevel::Debug);

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn put_logger_rejects_running_state_without_mutating() {
        let path = unique_logger_path("running");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(boot_source_input("/tmp/vmlinux")))
            .expect("boot source config should be stored");
        controller
            .commit_instance_start()
            .expect("start commit should set running state");

        let err = controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Debug),
            ))
            .expect_err("runtime logger config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: VmmAction::PutLogger(LoggerConfigInput::new()).name(),
                state: InstanceState::Running,
            }
        );
        assert!(!controller.logger_state.is_configured());
        assert_eq!(controller.logger_state.level(), LoggerLevel::Info);
        assert!(!path.exists());
    }

    #[test]
    fn put_logger_open_error_does_not_mutate_state() {
        let missing_parent = unique_logger_path("parent").join("logger");
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_level(LoggerLevel::Warn),
            ))
            .expect("level-only logger config should be stored");

        let err = controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new()
                    .with_log_path(&missing_parent)
                    .with_level(LoggerLevel::Debug),
            ))
            .expect_err("missing parent should fail");

        assert!(matches!(
            err,
            VmmActionError::LoggerConfig(LoggerConfigError::OpenFile(_))
        ));
        assert!(
            !err.to_string()
                .contains(missing_parent.to_string_lossy().as_ref())
        );
        assert_eq!(controller.logger_state.level(), LoggerLevel::Warn);
        assert!(!controller.logger_state.is_configured());
    }

    #[test]
    fn put_logger_rejects_empty_path_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::PutLogger(
                LoggerConfigInput::new().with_log_path(PathBuf::new()),
            ))
            .expect_err("empty logger path should fail");

        assert_eq!(
            err,
            VmmActionError::LoggerConfig(LoggerConfigError::EmptyPath)
        );
        assert!(!controller.logger_state.is_configured());
        assert_eq!(controller.logger_state.level(), LoggerLevel::Info);
    }

    #[test]
    fn logger_write_error_preserves_redacted_source() {
        let err =
            VmmActionError::LoggerWrite(LoggerWriteError::Write(std::io::ErrorKind::BrokenPipe));

        assert_eq!(err.to_string(), "failed to write logger output: BrokenPipe");
        assert!(err.source().is_some());
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
    fn put_machine_config_rejects_memory_smaller_than_balloon_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should store");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(128, true)))
            .expect("balloon config should store");

        let err = controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(4, 127)))
            .expect_err("machine memory smaller than balloon target should fail");

        assert_eq!(
            err,
            VmmActionError::MachineConfig(MachineConfigError::IncompatibleBalloonSize)
        );
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(128, true)))
        );

        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(4, 128)))
            .expect("machine memory equal to balloon target should store");
        assert_eq!(controller.machine_config().vcpu_count(), 4);
        assert_eq!(controller.machine_config().mem_size_mib(), 128);
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

        let err = controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                4,
                MAX_MEM_SIZE_MIB + 1,
            )))
            .expect_err("oversized machine config should fail");

        assert_eq!(
            err.to_string(),
            format!("machine mem_size_mib must be in 1..={MAX_MEM_SIZE_MIB}")
        );
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
    fn patch_machine_config_merges_with_previous_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");

        assert_eq!(
            controller.handle_action(VmmAction::PatchMachineConfig(
                MachineConfigPatchInput::new().with_mem_size_mib(512),
            )),
            Ok(VmmData::Empty)
        );

        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 512);
    }

    #[test]
    fn patch_machine_config_rejects_memory_smaller_than_balloon_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should store");
        controller
            .handle_action(VmmAction::PutBalloon(balloon_input(128, true)))
            .expect("balloon config should store");

        let err = controller
            .handle_action(VmmAction::PatchMachineConfig(
                MachineConfigPatchInput::new().with_mem_size_mib(127),
            ))
            .expect_err("machine memory patch smaller than balloon target should fail");

        assert_eq!(
            err,
            VmmActionError::MachineConfig(MachineConfigError::IncompatibleBalloonSize)
        );
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
        assert_eq!(
            controller.balloon_config(),
            Some(BalloonConfig::from(balloon_input(128, true)))
        );

        controller
            .handle_action(VmmAction::PatchMachineConfig(
                MachineConfigPatchInput::new().with_mem_size_mib(128),
            ))
            .expect("machine memory equal to balloon target should store");
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 128);
    }

    #[test]
    fn patch_machine_config_rejects_empty_patch_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");

        let err = controller
            .handle_action(VmmAction::PatchMachineConfig(MachineConfigPatchInput::new()))
            .expect_err("empty patch should fail");

        assert_eq!(
            err,
            VmmActionError::MachineConfig(MachineConfigError::EmptyPatch)
        );
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
    }

    #[test]
    fn patch_machine_config_rejects_invalid_input_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");

        let err = controller
            .handle_action(VmmAction::PatchMachineConfig(
                MachineConfigPatchInput::new().with_vcpu_count(0),
            ))
            .expect_err("invalid machine config patch should fail");

        assert_eq!(err.to_string(), "machine vcpu_count must be in 1..=32");
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);

        let err = controller
            .handle_action(VmmAction::PatchMachineConfig(
                MachineConfigPatchInput::new().with_mem_size_mib(MAX_MEM_SIZE_MIB + 1),
            ))
            .expect_err("oversized machine config patch should fail");

        assert_eq!(
            err.to_string(),
            format!("machine mem_size_mib must be in 1..={MAX_MEM_SIZE_MIB}")
        );
        assert_eq!(controller.machine_config().vcpu_count(), 2);
        assert_eq!(controller.machine_config().mem_size_mib(), 256);
    }

    #[test]
    fn patch_machine_config_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 256)))
            .expect("initial machine config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PatchMachineConfig(
                MachineConfigPatchInput::new().with_mem_size_mib(512),
            ))
            .expect_err("running machine config patch should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PatchMachineConfig",
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
    fn update_block_device_rejects_not_started_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("initial drive config should be stored");

        let err = controller
            .handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "rootfs",
                "rootfs",
                Some(PathBuf::from("/tmp/replaced.ext4")),
            )))
            .expect_err("pre-boot drive update should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "UpdateBlockDevice",
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(controller.drive_configs().len(), 1);
        assert_eq!(
            controller.drive_configs()[0].path_on_host(),
            Path::new("/tmp/rootfs.ext4")
        );
    }

    #[test]
    fn update_block_device_running_is_unsupported_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutDrive(drive_input(
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            )))
            .expect("initial drive config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::UpdateBlockDevice(DriveUpdateInput::new(
                "rootfs",
                "rootfs",
                Some(PathBuf::from("/tmp/replaced.ext4")),
            )))
            .expect_err("runtime drive update should remain unsupported");

        assert_eq!(err, VmmActionError::UnsupportedAction("UpdateBlockDevice"));
        assert_eq!(controller.drive_configs().len(), 1);
        assert_eq!(
            controller.drive_configs()[0].path_on_host(),
            Path::new("/tmp/rootfs.ext4")
        );
    }

    #[test]
    fn handles_put_network_interface_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutNetworkInterface(
                network_input("eth0", "tap0").with_guest_mac("12:34:56:78:9a:bc")
            )),
            Ok(VmmData::Empty)
        );

        assert_eq!(controller.network_interface_configs().len(), 1);
        let config = &controller.network_interface_configs()[0];
        assert_eq!(config.iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap0");
        assert_eq!(
            config.guest_mac(),
            Some(GuestMacAddress::from_bytes([
                0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
            ]))
        );
    }

    #[test]
    fn put_network_interface_config_replaces_duplicate_id() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                network_input("eth0", "tap0").with_guest_mac("12:34:56:78:9a:bc"),
            ))
            .expect("initial network interface config should be stored");

        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap1",
            )))
            .expect("duplicate interface id should replace existing config");

        assert_eq!(controller.network_interface_configs().len(), 1);
        let config = &controller.network_interface_configs()[0];
        assert_eq!(config.iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap1");
        assert_eq!(config.guest_mac(), None);
    }

    #[test]
    fn put_network_interface_config_rejects_duplicate_guest_mac_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                network_input("eth0", "tap0").with_guest_mac("12:34:56:78:9a:bc"),
            ))
            .expect("initial network interface config should be stored");

        let err = controller
            .handle_action(VmmAction::PutNetworkInterface(
                network_input("eth1", "tap1").with_guest_mac("12:34:56:78:9a:BC"),
            ))
            .expect_err("duplicate guest MAC should fail");

        assert_eq!(
            err,
            VmmActionError::NetworkInterfaceConfig(
                NetworkInterfaceConfigError::GuestMacAddressInUse {
                    guest_mac: GuestMacAddress::from_bytes([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,]),
                }
            )
        );
        assert_eq!(err.to_string(), "network guest_mac is already in use");
        assert_eq!(controller.network_interface_configs().len(), 1);
        assert_eq!(controller.network_interface_configs()[0].iface_id(), "eth0");
    }

    #[test]
    fn put_network_interface_config_rejects_one_over_limit_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        for index in 0..MAX_NETWORK_INTERFACE_COUNT {
            let iface_id = format!("eth{index}");
            let host_dev_name = format!("tap{index}");
            controller
                .handle_action(VmmAction::PutNetworkInterface(
                    NetworkInterfaceConfigInput::new(iface_id.clone(), iface_id, host_dev_name),
                ))
                .expect("network interface within limit should insert");
        }

        let iface_id = format!("eth{MAX_NETWORK_INTERFACE_COUNT}");
        let host_dev_name = format!("tap{MAX_NETWORK_INTERFACE_COUNT}");
        let err = controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new(iface_id.clone(), iface_id, host_dev_name),
            ))
            .expect_err("one-over network interface should fail");

        assert_eq!(
            err,
            VmmActionError::NetworkInterfaceConfig(
                NetworkInterfaceConfigError::TooManyNetworkInterfaces {
                    count: MAX_NETWORK_INTERFACE_COUNT + 1,
                    max: MAX_NETWORK_INTERFACE_COUNT,
                }
            )
        );
        assert_eq!(
            controller.network_interface_configs().len(),
            MAX_NETWORK_INTERFACE_COUNT
        );
    }

    #[test]
    fn put_network_interface_config_rejects_invalid_config_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        let err = controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new("eth0", "eth0", ""),
            ))
            .expect_err("invalid network interface config should fail");

        assert_eq!(
            err,
            VmmActionError::NetworkInterfaceConfig(
                NetworkInterfaceConfigError::EmptyHostDeviceName
            )
        );
        assert_eq!(err.to_string(), "network host_dev_name must not be empty");
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn put_network_interface_config_rejects_running_state() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect_err("running network interface config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutNetworkInterface",
                state: InstanceState::Running,
            }
        );
        assert!(controller.network_interface_configs().is_empty());
    }

    #[test]
    fn update_network_interface_rejects_not_started_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect("initial network interface config should be stored");

        let err = controller
            .handle_action(VmmAction::UpdateNetworkInterface(
                NetworkInterfaceUpdateInput::new("eth0", "eth0"),
            ))
            .expect_err("preboot network update should fail on state");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "UpdateNetworkInterface",
                state: InstanceState::NotStarted,
            }
        );
        assert_eq!(
            err.to_string(),
            "The requested operation is not supported in Not started state: UpdateNetworkInterface"
        );
        assert_eq!(controller.instance_info().state, InstanceState::NotStarted);
        assert_eq!(controller.network_interface_configs().len(), 1);
        assert_eq!(controller.network_interface_configs()[0].iface_id(), "eth0");
        assert_eq!(
            controller.network_interface_configs()[0].host_dev_name(),
            "tap0"
        );
    }

    #[test]
    fn update_network_interface_noop_succeeds_after_start_without_mutating() {
        for state in [InstanceState::Running, InstanceState::Paused] {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutNetworkInterface(network_input(
                    "eth0", "tap0",
                )))
                .expect("initial network interface config should be stored");
            controller.instance_info.state = state;

            let data = controller
                .handle_action(VmmAction::UpdateNetworkInterface(
                    NetworkInterfaceUpdateInput::new("eth0", "eth0"),
                ))
                .expect("no-op network update should succeed");

            assert_eq!(data, VmmData::Empty);
            assert_eq!(controller.instance_info().state, state);
            assert_eq!(controller.network_interface_configs().len(), 1);
            assert_eq!(controller.network_interface_configs()[0].iface_id(), "eth0");
            assert_eq!(
                controller.network_interface_configs()[0].host_dev_name(),
                "tap0"
            );
        }
    }

    #[test]
    fn update_network_interface_rejects_unknown_interface_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutNetworkInterface(network_input(
                "eth0", "tap0",
            )))
            .expect("initial network interface config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::UpdateNetworkInterface(
                NetworkInterfaceUpdateInput::new("eth9", "eth9"),
            ))
            .expect_err("unknown network interface should fail");

        assert_eq!(
            err,
            VmmActionError::NetworkInterfaceUpdate(NetworkInterfaceUpdateError::UnknownInterface {
                iface_id: "eth9".to_string(),
            },)
        );
        assert_eq!(err.to_string(), "network interface is not configured");
        assert_eq!(controller.instance_info().state, InstanceState::Running);
        assert_eq!(controller.network_interface_configs().len(), 1);
        assert_eq!(controller.network_interface_configs()[0].iface_id(), "eth0");
        assert_eq!(
            controller.network_interface_configs()[0].host_dev_name(),
            "tap0"
        );
    }

    #[test]
    fn update_network_interface_rejects_rate_limiters_after_start_without_mutating() {
        let cases = [
            (
                NetworkInterfaceUpdateInput::new("eth0", "eth0").with_rx_rate_limiter_configured(),
                NetworkInterfaceUpdateError::UnsupportedRxRateLimiter,
                "network rx_rate_limiter is not supported",
            ),
            (
                NetworkInterfaceUpdateInput::new("eth0", "eth0").with_tx_rate_limiter_configured(),
                NetworkInterfaceUpdateError::UnsupportedTxRateLimiter,
                "network tx_rate_limiter is not supported",
            ),
        ];

        for (input, expected_error, expected_message) in cases {
            let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
            controller
                .handle_action(VmmAction::PutNetworkInterface(network_input(
                    "eth0", "tap0",
                )))
                .expect("initial network interface config should be stored");
            controller.instance_info.state = InstanceState::Running;

            let err = controller
                .handle_action(VmmAction::UpdateNetworkInterface(input))
                .expect_err("configured network rate limiter should fail");

            assert_eq!(err, VmmActionError::NetworkInterfaceUpdate(expected_error));
            assert_eq!(err.to_string(), expected_message);
            assert_eq!(controller.instance_info().state, InstanceState::Running);
            assert_eq!(controller.network_interface_configs().len(), 1);
            assert_eq!(controller.network_interface_configs()[0].iface_id(), "eth0");
            assert_eq!(
                controller.network_interface_configs()[0].host_dev_name(),
                "tap0"
            );
        }
    }

    #[test]
    fn handles_put_serial_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutSerial(
                serial_input("/tmp/serial.out").with_rate_limiter(SerialRateLimiterConfig::new(
                    2,
                    Some(1),
                    3
                ))
            )),
            Ok(VmmData::Empty)
        );

        assert_eq!(
            controller.serial_config().serial_out_path(),
            Some(Path::new("/tmp/serial.out"))
        );
        assert_eq!(
            controller.serial_config().rate_limiter(),
            Some(SerialRateLimiterConfig::new(2, Some(1), 3))
        );
    }

    #[test]
    fn put_serial_config_clear_request_replaces_existing_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(serial_input("/tmp/serial.out")))
            .expect("initial serial config should be stored");

        controller
            .handle_action(VmmAction::PutSerial(SerialConfigInput::new()))
            .expect("serial clear request should be stored");

        assert_eq!(controller.serial_config().serial_out_path(), None);
        assert_eq!(controller.serial_config().rate_limiter(), None);
    }

    #[test]
    fn put_serial_config_rejects_empty_path_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(serial_input("/tmp/original.out")))
            .expect("initial serial config should be stored");

        let err = controller
            .handle_action(VmmAction::PutSerial(serial_input("")))
            .expect_err("empty serial output path should fail");

        assert_eq!(
            err,
            VmmActionError::SerialConfig(SerialConfigError::EmptyOutputPath)
        );
        assert_eq!(err.to_string(), "serial output path must not be empty");
        assert_eq!(
            controller.serial_config().serial_out_path(),
            Some(Path::new("/tmp/original.out"))
        );
    }

    #[test]
    fn put_serial_config_rejects_control_character_path_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(serial_input("/tmp/original.out")))
            .expect("initial serial config should be stored");

        let err = controller
            .handle_action(VmmAction::PutSerial(serial_input("/tmp/bad\npath")))
            .expect_err("control-character serial output path should fail");

        assert_eq!(
            err,
            VmmActionError::SerialConfig(SerialConfigError::InvalidOutputPath)
        );
        assert_eq!(
            err.to_string(),
            "serial output path must not contain control characters"
        );
        assert_eq!(
            controller.serial_config().serial_out_path(),
            Some(Path::new("/tmp/original.out"))
        );
    }

    #[test]
    fn put_serial_config_accepts_rate_limiter() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        controller
            .handle_action(VmmAction::PutSerial(
                SerialConfigInput::new()
                    .with_rate_limiter(SerialRateLimiterConfig::new(4, None, 5)),
            ))
            .expect("serial rate limiter should be stored");

        assert_eq!(controller.serial_config().serial_out_path(), None);
        assert_eq!(
            controller.serial_config().rate_limiter(),
            Some(SerialRateLimiterConfig::new(4, None, 5))
        );
    }

    #[test]
    fn put_serial_config_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutSerial(serial_input("/tmp/original.out")))
            .expect("initial serial config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PutSerial(serial_input("/tmp/replacement.out")))
            .expect_err("running serial config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutSerial",
                state: InstanceState::Running,
            }
        );
        assert_eq!(
            controller.serial_config().serial_out_path(),
            Some(Path::new("/tmp/original.out"))
        );
    }

    #[test]
    fn handles_put_vsock_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");

        assert_eq!(
            controller.handle_action(VmmAction::PutVsock(
                vsock_input(MIN_GUEST_CID, "./v.sock").with_vsock_id("vsock0"),
            )),
            Ok(VmmData::Empty)
        );

        let config = controller
            .vsock_config()
            .expect("vsock config should be stored");
        assert_eq!(config.vsock_id(), Some("vsock0"));
        assert_eq!(config.guest_cid(), MIN_GUEST_CID);
        assert_eq!(config.uds_path(), Path::new("./v.sock"));
    }

    #[test]
    fn put_vsock_config_replaces_existing_config() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutVsock(vsock_input(3, "/tmp/first.sock")))
            .expect("initial vsock config should be stored");

        controller
            .handle_action(VmmAction::PutVsock(vsock_input(42, "/tmp/replaced.sock")))
            .expect("replacement vsock config should be stored");

        let config = controller
            .vsock_config()
            .expect("replacement config should be stored");
        assert_eq!(config.vsock_id(), None);
        assert_eq!(config.guest_cid(), 42);
        assert_eq!(config.uds_path(), Path::new("/tmp/replaced.sock"));
    }

    #[test]
    fn put_vsock_config_rejects_invalid_config_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutVsock(vsock_input(3, "/tmp/original.sock")))
            .expect("initial vsock config should be stored");

        let err = controller
            .handle_action(VmmAction::PutVsock(vsock_input(2, "/tmp/replacement.sock")))
            .expect_err("invalid guest cid should fail");

        assert_eq!(
            err,
            VmmActionError::VsockConfig(VsockConfigError::GuestCidTooSmall {
                guest_cid: 2,
                min: MIN_GUEST_CID,
            })
        );
        assert_eq!(err.to_string(), "vsock guest_cid 2 is below minimum 3");
        let config = controller
            .vsock_config()
            .expect("original config should remain stored");
        assert_eq!(config.guest_cid(), 3);
        assert_eq!(config.uds_path(), Path::new("/tmp/original.sock"));
    }

    #[test]
    fn put_vsock_config_rejects_running_state_without_mutating() {
        let mut controller = VmmController::new("demo-1", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutVsock(vsock_input(3, "/tmp/original.sock")))
            .expect("initial vsock config should be stored");
        controller.instance_info.state = InstanceState::Running;

        let err = controller
            .handle_action(VmmAction::PutVsock(vsock_input(
                42,
                "/tmp/replacement.sock",
            )))
            .expect_err("running vsock config should fail");

        assert_eq!(
            err,
            VmmActionError::UnsupportedState {
                action: "PutVsock",
                state: InstanceState::Running,
            }
        );
        let config = controller
            .vsock_config()
            .expect("original config should remain stored");
        assert_eq!(config.guest_cid(), 3);
        assert_eq!(config.uds_path(), Path::new("/tmp/original.sock"));
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
    fn displays_lifecycle_error() {
        let err = VmmActionError::Lifecycle(BackendError::InvalidState("worker unavailable"));

        assert_eq!(
            err.to_string(),
            "failed to update microVM lifecycle: invalid backend state: worker unavailable"
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_snapshot_unsupported_error() {
        let err = VmmActionError::SnapshotUnsupported;

        assert_eq!(err.to_string(), "Snapshot and restore are not supported.");
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_balloon_unsupported_error() {
        let err = VmmActionError::BalloonUnsupported;

        assert_eq!(err.to_string(), "Balloon device is not supported.");
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_balloon_config_error() {
        let err = VmmActionError::BalloonConfig(BalloonConfigError::TargetExceedsGuestMemory {
            amount_mib: 129,
            mem_size_mib: 128,
        });

        assert_eq!(
            err.to_string(),
            "balloon amount_mib 129 exceeds configured guest memory 128 MiB"
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_balloon_stats_error() {
        let err = VmmActionError::BalloonStats(BalloonStatsError::ActualPageCountTooLarge {
            actual_pages: u64::from(u32::MAX) + 1,
        });

        assert_eq!(
            err.to_string(),
            format!(
                "balloon actual_pages {} exceeds maximum {} representable in the API response",
                u64::from(u32::MAX) + 1,
                u32::MAX
            )
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_balloon_hinting_status_error() {
        let err =
            VmmActionError::BalloonHintingStatus(BalloonHintingStatusError::HintingNotEnabled);

        assert_eq!(err.to_string(), "balloon free-page hinting is not enabled");
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_balloon_hinting_command_error() {
        let err =
            VmmActionError::BalloonHintingCommand(BalloonHintingCommandError::HintingNotEnabled);

        assert_eq!(err.to_string(), "balloon free-page hinting is not enabled");
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_entropy_unsupported_error() {
        let err = VmmActionError::EntropyUnsupported;

        assert_eq!(err.to_string(), "Entropy device is not supported.");
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_drive_update_unsupported_error() {
        let err = VmmActionError::DriveUpdateUnsupported;

        assert_eq!(err.to_string(), "Drive updates are not supported.");
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_network_interface_update_error() {
        let err = VmmActionError::NetworkInterfaceUpdate(
            NetworkInterfaceUpdateError::UnsupportedRxRateLimiter,
        );

        assert_eq!(err.to_string(), "network rx_rate_limiter is not supported");
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_memory_hotplug_unsupported_error() {
        let err = VmmActionError::MemoryHotplugUnsupported;

        assert_eq!(err.to_string(), "Memory hotplug is not supported.");
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_memory_hotplug_config_error() {
        let err = VmmActionError::MemoryHotplugConfig(
            MemoryHotplugConfigError::TotalSizeNotMultipleOfSlotSize { slot_size_mib: 128 },
        );

        assert_eq!(
            err.to_string(),
            "Total size must be a multiple of slot size (128 MiB)"
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn displays_pmem_unsupported_error() {
        let err = VmmActionError::PmemUnsupported;

        assert_eq!(err.to_string(), "Pmem device is not supported.");
        assert!(err.source().is_none());
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
    fn displays_vsock_config_error() {
        let err = VmmActionError::VsockConfig(VsockConfigError::EmptySocketPath);

        assert_eq!(err.to_string(), "vsock uds_path must not be empty");
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

        assert_eq!(
            err.to_string(),
            format!(
                "machine mem_size_mib must be in 1..={}",
                super::machine::MAX_MEM_SIZE_MIB
            )
        );
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
