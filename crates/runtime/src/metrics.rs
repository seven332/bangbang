use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsConfigInput {
    metrics_path: PathBuf,
}

impl MetricsConfigInput {
    pub fn new(metrics_path: impl Into<PathBuf>) -> Self {
        Self {
            metrics_path: metrics_path.into(),
        }
    }

    pub fn validate(self) -> Result<MetricsConfig, MetricsConfigError> {
        if self.metrics_path.as_os_str().is_empty() {
            return Err(MetricsConfigError::EmptyPath);
        }

        Ok(MetricsConfig {
            metrics_path: self.metrics_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsConfig {
    metrics_path: PathBuf,
}

impl MetricsConfig {
    pub fn metrics_path(&self) -> &Path {
        &self.metrics_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsConfigError {
    AlreadyInitialized,
    EmptyPath,
    OpenFile(std::io::ErrorKind),
}

impl fmt::Display for MetricsConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => f.write_str("metrics system is already initialized"),
            Self::EmptyPath => f.write_str("metrics path must not be empty"),
            Self::OpenFile(kind) => write!(f, "metrics output could not be initialized: {kind:?}"),
        }
    }
}

impl std::error::Error for MetricsConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsFlushError {
    Write(std::io::ErrorKind),
}

impl fmt::Display for MetricsFlushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(kind) => write!(f, "failed to flush metrics: {kind:?}"),
        }
    }
}

impl std::error::Error for MetricsFlushError {}

#[derive(Debug, Default)]
pub struct MetricsState {
    sink: Option<MetricsSink>,
    flush_count: u64,
    get_api_requests: GetApiRequestMetrics,
    logger_metrics: LoggerMetrics,
    patch_api_requests: PatchApiRequestMetrics,
    put_api_requests: PutApiRequestMetrics,
}

impl MetricsState {
    pub fn configure(&mut self, input: MetricsConfigInput) -> Result<(), MetricsConfigError> {
        if self.sink.is_some() {
            return Err(MetricsConfigError::AlreadyInitialized);
        }

        let config = input.validate()?;
        self.sink = Some(MetricsSink::open(&config)?);

        Ok(())
    }

    pub fn flush(&mut self) -> Result<bool, MetricsFlushError> {
        self.flush_with_diagnostics(&MetricsDiagnostics::default())
    }

    pub(crate) fn record_put_actions_request(&mut self) {
        self.put_api_requests.record_actions_request();
    }

    pub(crate) fn record_put_actions_failure(&mut self) {
        self.put_api_requests.record_actions_failure();
    }

    pub(crate) fn record_put_boot_source_request(&mut self) {
        self.put_api_requests.record_boot_source_request();
    }

    pub(crate) fn record_put_boot_source_failure(&mut self) {
        self.put_api_requests.record_boot_source_failure();
    }

    pub(crate) fn record_put_cpu_config_request(&mut self) {
        self.put_api_requests.record_cpu_config_request();
    }

    pub(crate) fn record_put_cpu_config_failure(&mut self) {
        self.put_api_requests.record_cpu_config_failure();
    }

    pub(crate) fn record_put_drive_request(&mut self) {
        self.put_api_requests.record_drive_request();
    }

    pub(crate) fn record_put_drive_failure(&mut self) {
        self.put_api_requests.record_drive_failure();
    }

    pub(crate) fn record_put_metrics_request(&mut self) {
        self.put_api_requests.record_metrics_request();
    }

    pub(crate) fn record_put_metrics_failure(&mut self) {
        self.put_api_requests.record_metrics_failure();
    }

    pub(crate) fn record_put_logger_request(&mut self) {
        self.put_api_requests.record_logger_request();
    }

    pub(crate) fn record_put_logger_failure(&mut self) {
        self.put_api_requests.record_logger_failure();
    }

    pub(crate) fn record_put_machine_config_request(&mut self) {
        self.put_api_requests.record_machine_config_request();
    }

    pub(crate) fn record_put_machine_config_failure(&mut self) {
        self.put_api_requests.record_machine_config_failure();
    }

    pub(crate) fn record_put_mmds_request(&mut self) {
        self.put_api_requests.record_mmds_request();
    }

    pub(crate) fn record_put_mmds_failure(&mut self) {
        self.put_api_requests.record_mmds_failure();
    }

    pub(crate) fn record_put_network_request(&mut self) {
        self.put_api_requests.record_network_request();
    }

    pub(crate) fn record_put_network_failure(&mut self) {
        self.put_api_requests.record_network_failure();
    }

    pub(crate) fn record_put_serial_request(&mut self) {
        self.put_api_requests.record_serial_request();
    }

    pub(crate) fn record_put_serial_failure(&mut self) {
        self.put_api_requests.record_serial_failure();
    }

    pub(crate) fn record_put_vsock_request(&mut self) {
        self.put_api_requests.record_vsock_request();
    }

    pub(crate) fn record_put_vsock_failure(&mut self) {
        self.put_api_requests.record_vsock_failure();
    }

    pub(crate) fn record_patch_drive_request(&mut self) {
        self.patch_api_requests.record_drive_request();
    }

    pub(crate) fn record_patch_drive_failure(&mut self) {
        self.patch_api_requests.record_drive_failure();
    }

    pub(crate) fn record_patch_machine_config_request(&mut self) {
        self.patch_api_requests.record_machine_config_request();
    }

    pub(crate) fn record_patch_machine_config_failure(&mut self) {
        self.patch_api_requests.record_machine_config_failure();
    }

    pub(crate) fn record_patch_mmds_request(&mut self) {
        self.patch_api_requests.record_mmds_request();
    }

    pub(crate) fn record_patch_mmds_failure(&mut self) {
        self.patch_api_requests.record_mmds_failure();
    }

    pub(crate) fn record_get_instance_info_request(&mut self) {
        self.get_api_requests.record_instance_info_request();
    }

    pub(crate) fn record_get_vmm_version_request(&mut self) {
        self.get_api_requests.record_vmm_version_request();
    }

    pub(crate) fn record_get_machine_config_request(&mut self) {
        self.get_api_requests.record_machine_config_request();
    }

    pub(crate) fn record_get_mmds_request(&mut self) {
        self.get_api_requests.record_mmds_request();
    }

    pub fn flush_with_diagnostics(
        &mut self,
        diagnostics: &MetricsDiagnostics,
    ) -> Result<bool, MetricsFlushError> {
        let Some(sink) = &mut self.sink else {
            return Ok(false);
        };
        let next_flush_count = self.flush_count.saturating_add(1);
        if let Err(err) = sink.write_minimal_metrics(
            next_flush_count,
            diagnostics,
            self.get_api_requests,
            self.logger_metrics,
            self.patch_api_requests,
            self.put_api_requests,
        ) {
            self.logger_metrics.record_missed_metrics();
            return Err(err);
        }
        self.flush_count = next_flush_count;

        Ok(true)
    }

    #[cfg(test)]
    fn with_test_output(output: impl MetricsOutput + 'static) -> Self {
        Self {
            sink: Some(MetricsSink::new(Box::new(output))),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub const fn is_configured(&self) -> bool {
        self.sink.is_some()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GetApiRequestMetrics {
    instance_info_count: u64,
    vmm_version_count: u64,
    machine_cfg_count: u64,
    mmds_count: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LoggerMetrics {
    missed_metrics_count: u64,
}

impl LoggerMetrics {
    const fn is_empty(self) -> bool {
        self.missed_metrics_count == 0
    }

    fn record_missed_metrics(&mut self) {
        self.missed_metrics_count = self.missed_metrics_count.saturating_add(1);
    }

    const fn missed_metrics_count(self) -> u64 {
        self.missed_metrics_count
    }
}

impl GetApiRequestMetrics {
    const fn is_empty(self) -> bool {
        self.instance_info_count == 0
            && self.vmm_version_count == 0
            && self.machine_cfg_count == 0
            && self.mmds_count == 0
    }

    fn record_instance_info_request(&mut self) {
        self.instance_info_count = self.instance_info_count.saturating_add(1);
    }

    fn record_vmm_version_request(&mut self) {
        self.vmm_version_count = self.vmm_version_count.saturating_add(1);
    }

    fn record_machine_config_request(&mut self) {
        self.machine_cfg_count = self.machine_cfg_count.saturating_add(1);
    }

    fn record_mmds_request(&mut self) {
        self.mmds_count = self.mmds_count.saturating_add(1);
    }

    const fn instance_info_count(self) -> u64 {
        self.instance_info_count
    }

    const fn vmm_version_count(self) -> u64 {
        self.vmm_version_count
    }

    const fn machine_cfg_count(self) -> u64 {
        self.machine_cfg_count
    }

    const fn mmds_count(self) -> u64 {
        self.mmds_count
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PatchApiRequestMetrics {
    drive_count: u64,
    drive_fails: u64,
    machine_cfg_count: u64,
    machine_cfg_fails: u64,
    mmds_count: u64,
    mmds_fails: u64,
}

impl PatchApiRequestMetrics {
    const fn is_empty(self) -> bool {
        self.drive_count == 0
            && self.drive_fails == 0
            && self.machine_cfg_count == 0
            && self.machine_cfg_fails == 0
            && self.mmds_count == 0
            && self.mmds_fails == 0
    }

    fn record_drive_request(&mut self) {
        self.drive_count = self.drive_count.saturating_add(1);
    }

    fn record_drive_failure(&mut self) {
        self.drive_fails = self.drive_fails.saturating_add(1);
    }

    fn record_machine_config_request(&mut self) {
        self.machine_cfg_count = self.machine_cfg_count.saturating_add(1);
    }

    fn record_machine_config_failure(&mut self) {
        self.machine_cfg_fails = self.machine_cfg_fails.saturating_add(1);
    }

    fn record_mmds_request(&mut self) {
        self.mmds_count = self.mmds_count.saturating_add(1);
    }

    fn record_mmds_failure(&mut self) {
        self.mmds_fails = self.mmds_fails.saturating_add(1);
    }

    const fn drive_count(self) -> u64 {
        self.drive_count
    }

    const fn drive_fails(self) -> u64 {
        self.drive_fails
    }

    const fn machine_cfg_count(self) -> u64 {
        self.machine_cfg_count
    }

    const fn machine_cfg_fails(self) -> u64 {
        self.machine_cfg_fails
    }

    const fn mmds_count(self) -> u64 {
        self.mmds_count
    }

    const fn mmds_fails(self) -> u64 {
        self.mmds_fails
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PutApiRequestMetrics {
    actions_count: u64,
    actions_fails: u64,
    boot_source_count: u64,
    boot_source_fails: u64,
    cpu_cfg_count: u64,
    cpu_cfg_fails: u64,
    drive_count: u64,
    drive_fails: u64,
    logger_count: u64,
    logger_fails: u64,
    machine_cfg_count: u64,
    machine_cfg_fails: u64,
    metrics_count: u64,
    metrics_fails: u64,
    mmds_count: u64,
    mmds_fails: u64,
    network_count: u64,
    network_fails: u64,
    serial_count: u64,
    serial_fails: u64,
    vsock_count: u64,
    vsock_fails: u64,
}

impl PutApiRequestMetrics {
    const fn is_empty(self) -> bool {
        self.actions_count == 0
            && self.actions_fails == 0
            && self.boot_source_count == 0
            && self.boot_source_fails == 0
            && self.cpu_cfg_count == 0
            && self.cpu_cfg_fails == 0
            && self.drive_count == 0
            && self.drive_fails == 0
            && self.logger_count == 0
            && self.logger_fails == 0
            && self.machine_cfg_count == 0
            && self.machine_cfg_fails == 0
            && self.metrics_count == 0
            && self.metrics_fails == 0
            && self.mmds_count == 0
            && self.mmds_fails == 0
            && self.network_count == 0
            && self.network_fails == 0
            && self.serial_count == 0
            && self.serial_fails == 0
            && self.vsock_count == 0
            && self.vsock_fails == 0
    }

    fn record_actions_request(&mut self) {
        self.actions_count = self.actions_count.saturating_add(1);
    }

    fn record_actions_failure(&mut self) {
        self.actions_fails = self.actions_fails.saturating_add(1);
    }

    fn record_boot_source_request(&mut self) {
        self.boot_source_count = self.boot_source_count.saturating_add(1);
    }

    fn record_boot_source_failure(&mut self) {
        self.boot_source_fails = self.boot_source_fails.saturating_add(1);
    }

    fn record_cpu_config_request(&mut self) {
        self.cpu_cfg_count = self.cpu_cfg_count.saturating_add(1);
    }

    fn record_cpu_config_failure(&mut self) {
        self.cpu_cfg_fails = self.cpu_cfg_fails.saturating_add(1);
    }

    fn record_drive_request(&mut self) {
        self.drive_count = self.drive_count.saturating_add(1);
    }

    fn record_drive_failure(&mut self) {
        self.drive_fails = self.drive_fails.saturating_add(1);
    }

    fn record_metrics_request(&mut self) {
        self.metrics_count = self.metrics_count.saturating_add(1);
    }

    fn record_metrics_failure(&mut self) {
        self.metrics_fails = self.metrics_fails.saturating_add(1);
    }

    fn record_logger_request(&mut self) {
        self.logger_count = self.logger_count.saturating_add(1);
    }

    fn record_logger_failure(&mut self) {
        self.logger_fails = self.logger_fails.saturating_add(1);
    }

    fn record_machine_config_request(&mut self) {
        self.machine_cfg_count = self.machine_cfg_count.saturating_add(1);
    }

    fn record_machine_config_failure(&mut self) {
        self.machine_cfg_fails = self.machine_cfg_fails.saturating_add(1);
    }

    fn record_mmds_request(&mut self) {
        self.mmds_count = self.mmds_count.saturating_add(1);
    }

    fn record_mmds_failure(&mut self) {
        self.mmds_fails = self.mmds_fails.saturating_add(1);
    }

    fn record_network_request(&mut self) {
        self.network_count = self.network_count.saturating_add(1);
    }

    fn record_network_failure(&mut self) {
        self.network_fails = self.network_fails.saturating_add(1);
    }

    fn record_serial_request(&mut self) {
        self.serial_count = self.serial_count.saturating_add(1);
    }

    fn record_serial_failure(&mut self) {
        self.serial_fails = self.serial_fails.saturating_add(1);
    }

    fn record_vsock_request(&mut self) {
        self.vsock_count = self.vsock_count.saturating_add(1);
    }

    fn record_vsock_failure(&mut self) {
        self.vsock_fails = self.vsock_fails.saturating_add(1);
    }

    const fn actions_count(self) -> u64 {
        self.actions_count
    }

    const fn actions_fails(self) -> u64 {
        self.actions_fails
    }

    const fn boot_source_count(self) -> u64 {
        self.boot_source_count
    }

    const fn boot_source_fails(self) -> u64 {
        self.boot_source_fails
    }

    const fn cpu_cfg_count(self) -> u64 {
        self.cpu_cfg_count
    }

    const fn cpu_cfg_fails(self) -> u64 {
        self.cpu_cfg_fails
    }

    const fn drive_count(self) -> u64 {
        self.drive_count
    }

    const fn drive_fails(self) -> u64 {
        self.drive_fails
    }

    const fn logger_count(self) -> u64 {
        self.logger_count
    }

    const fn logger_fails(self) -> u64 {
        self.logger_fails
    }

    const fn machine_cfg_count(self) -> u64 {
        self.machine_cfg_count
    }

    const fn machine_cfg_fails(self) -> u64 {
        self.machine_cfg_fails
    }

    const fn metrics_count(self) -> u64 {
        self.metrics_count
    }

    const fn metrics_fails(self) -> u64 {
        self.metrics_fails
    }

    const fn mmds_count(self) -> u64 {
        self.mmds_count
    }

    const fn mmds_fails(self) -> u64 {
        self.mmds_fails
    }

    const fn network_count(self) -> u64 {
        self.network_count
    }

    const fn network_fails(self) -> u64 {
        self.network_fails
    }

    const fn serial_count(self) -> u64 {
        self.serial_count
    }

    const fn serial_fails(self) -> u64 {
        self.serial_fails
    }

    const fn vsock_count(self) -> u64 {
        self.vsock_count
    }

    const fn vsock_fails(self) -> u64 {
        self.vsock_fails
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsDiagnostics {
    boot_run_loop_status: Option<BootRunLoopMetricStatus>,
    start_time_us: Option<u64>,
    start_time_cpu_us: Option<u64>,
    parent_cpu_time_us: Option<u64>,
}

impl MetricsDiagnostics {
    pub const fn new() -> Self {
        Self {
            boot_run_loop_status: None,
            start_time_us: None,
            start_time_cpu_us: None,
            parent_cpu_time_us: None,
        }
    }

    pub const fn with_boot_run_loop_status(mut self, status: BootRunLoopMetricStatus) -> Self {
        self.boot_run_loop_status = Some(status);
        self
    }

    pub const fn with_start_time_us(mut self, start_time_us: u64) -> Self {
        self.start_time_us = Some(start_time_us);
        self
    }

    pub const fn with_start_time_cpu_us(mut self, start_time_cpu_us: u64) -> Self {
        self.start_time_cpu_us = Some(start_time_cpu_us);
        self
    }

    pub const fn with_parent_cpu_time_us(mut self, parent_cpu_time_us: u64) -> Self {
        self.parent_cpu_time_us = Some(parent_cpu_time_us);
        self
    }

    pub const fn merged_with(mut self, other: Self) -> Self {
        if other.boot_run_loop_status.is_some() {
            self.boot_run_loop_status = other.boot_run_loop_status;
        }
        if other.start_time_us.is_some() {
            self.start_time_us = other.start_time_us;
        }
        if other.start_time_cpu_us.is_some() {
            self.start_time_cpu_us = other.start_time_cpu_us;
        }
        if other.parent_cpu_time_us.is_some() {
            self.parent_cpu_time_us = other.parent_cpu_time_us;
        }

        self
    }

    pub const fn boot_run_loop_status(&self) -> Option<BootRunLoopMetricStatus> {
        self.boot_run_loop_status
    }

    pub const fn start_time_us(&self) -> Option<u64> {
        self.start_time_us
    }

    pub const fn start_time_cpu_us(&self) -> Option<u64> {
        self.start_time_cpu_us
    }

    pub const fn parent_cpu_time_us(&self) -> Option<u64> {
        self.parent_cpu_time_us
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootRunLoopMetricStatus {
    Running,
    Exited,
    Failed,
}

impl BootRunLoopMetricStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
struct MetricsSink {
    output: Box<dyn MetricsOutput>,
}

trait MetricsOutput: fmt::Debug + Send {
    fn write_json_line(&mut self, line: &serde_json::Value) -> Result<(), MetricsFlushError>;
}

#[derive(Debug)]
struct FileMetricsOutput {
    writer: LineWriter<File>,
}

impl MetricsOutput for FileMetricsOutput {
    fn write_json_line(&mut self, line: &serde_json::Value) -> Result<(), MetricsFlushError> {
        writeln!(self.writer, "{line}").map_err(|err| MetricsFlushError::Write(err.kind()))?;
        self.writer
            .flush()
            .map_err(|err| MetricsFlushError::Write(err.kind()))
    }
}

impl MetricsSink {
    fn open(config: &MetricsConfig) -> Result<Self, MetricsConfigError> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(config.metrics_path())
            .map_err(|err| MetricsConfigError::OpenFile(err.kind()))?;

        Ok(Self::new(Box::new(FileMetricsOutput {
            writer: LineWriter::new(file),
        })))
    }

    fn new(output: Box<dyn MetricsOutput>) -> Self {
        Self { output }
    }

    fn write_minimal_metrics(
        &mut self,
        flush_count: u64,
        diagnostics: &MetricsDiagnostics,
        get_api_requests: GetApiRequestMetrics,
        logger_metrics: LoggerMetrics,
        patch_api_requests: PatchApiRequestMetrics,
        put_api_requests: PutApiRequestMetrics,
    ) -> Result<(), MetricsFlushError> {
        let mut vmm = serde_json::Map::new();
        if let Some(status) = diagnostics.boot_run_loop_status() {
            vmm.insert(
                "boot_run_loop_status".to_string(),
                serde_json::Value::String(status.as_str().to_string()),
            );
        }
        vmm.insert(
            "metrics_flush_count".to_string(),
            serde_json::Value::Number(flush_count.into()),
        );
        if let Some(value) = diagnostics.parent_cpu_time_us() {
            vmm.insert(
                "parent_cpu_time_us".to_string(),
                serde_json::Value::Number(value.into()),
            );
        }
        if let Some(value) = diagnostics.start_time_cpu_us() {
            vmm.insert(
                "start_time_cpu_us".to_string(),
                serde_json::Value::Number(value.into()),
            );
        }
        if let Some(value) = diagnostics.start_time_us() {
            vmm.insert(
                "start_time_us".to_string(),
                serde_json::Value::Number(value.into()),
            );
        }

        let mut root = serde_json::Map::new();
        if !get_api_requests.is_empty() {
            let mut get_requests = serde_json::Map::new();
            get_requests.insert(
                "instance_info_count".to_string(),
                serde_json::Value::Number(get_api_requests.instance_info_count().into()),
            );
            get_requests.insert(
                "machine_cfg_count".to_string(),
                serde_json::Value::Number(get_api_requests.machine_cfg_count().into()),
            );
            get_requests.insert(
                "mmds_count".to_string(),
                serde_json::Value::Number(get_api_requests.mmds_count().into()),
            );
            get_requests.insert(
                "vmm_version_count".to_string(),
                serde_json::Value::Number(get_api_requests.vmm_version_count().into()),
            );
            root.insert(
                "get_api_requests".to_string(),
                serde_json::Value::Object(get_requests),
            );
        }
        if !logger_metrics.is_empty() {
            let mut logger = serde_json::Map::new();
            logger.insert(
                "missed_metrics_count".to_string(),
                serde_json::Value::Number(logger_metrics.missed_metrics_count().into()),
            );
            root.insert("logger".to_string(), serde_json::Value::Object(logger));
        }
        if !patch_api_requests.is_empty() {
            let mut patch_requests = serde_json::Map::new();
            patch_requests.insert(
                "drive_count".to_string(),
                serde_json::Value::Number(patch_api_requests.drive_count().into()),
            );
            patch_requests.insert(
                "drive_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.drive_fails().into()),
            );
            patch_requests.insert(
                "machine_cfg_count".to_string(),
                serde_json::Value::Number(patch_api_requests.machine_cfg_count().into()),
            );
            patch_requests.insert(
                "machine_cfg_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.machine_cfg_fails().into()),
            );
            patch_requests.insert(
                "mmds_count".to_string(),
                serde_json::Value::Number(patch_api_requests.mmds_count().into()),
            );
            patch_requests.insert(
                "mmds_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.mmds_fails().into()),
            );
            root.insert(
                "patch_api_requests".to_string(),
                serde_json::Value::Object(patch_requests),
            );
        }
        if !put_api_requests.is_empty() {
            let mut put_requests = serde_json::Map::new();
            put_requests.insert(
                "actions_count".to_string(),
                serde_json::Value::Number(put_api_requests.actions_count().into()),
            );
            put_requests.insert(
                "actions_fails".to_string(),
                serde_json::Value::Number(put_api_requests.actions_fails().into()),
            );
            put_requests.insert(
                "boot_source_count".to_string(),
                serde_json::Value::Number(put_api_requests.boot_source_count().into()),
            );
            put_requests.insert(
                "boot_source_fails".to_string(),
                serde_json::Value::Number(put_api_requests.boot_source_fails().into()),
            );
            put_requests.insert(
                "cpu_cfg_count".to_string(),
                serde_json::Value::Number(put_api_requests.cpu_cfg_count().into()),
            );
            put_requests.insert(
                "cpu_cfg_fails".to_string(),
                serde_json::Value::Number(put_api_requests.cpu_cfg_fails().into()),
            );
            put_requests.insert(
                "drive_count".to_string(),
                serde_json::Value::Number(put_api_requests.drive_count().into()),
            );
            put_requests.insert(
                "drive_fails".to_string(),
                serde_json::Value::Number(put_api_requests.drive_fails().into()),
            );
            put_requests.insert(
                "logger_count".to_string(),
                serde_json::Value::Number(put_api_requests.logger_count().into()),
            );
            put_requests.insert(
                "logger_fails".to_string(),
                serde_json::Value::Number(put_api_requests.logger_fails().into()),
            );
            put_requests.insert(
                "machine_cfg_count".to_string(),
                serde_json::Value::Number(put_api_requests.machine_cfg_count().into()),
            );
            put_requests.insert(
                "machine_cfg_fails".to_string(),
                serde_json::Value::Number(put_api_requests.machine_cfg_fails().into()),
            );
            put_requests.insert(
                "metrics_count".to_string(),
                serde_json::Value::Number(put_api_requests.metrics_count().into()),
            );
            put_requests.insert(
                "metrics_fails".to_string(),
                serde_json::Value::Number(put_api_requests.metrics_fails().into()),
            );
            put_requests.insert(
                "mmds_count".to_string(),
                serde_json::Value::Number(put_api_requests.mmds_count().into()),
            );
            put_requests.insert(
                "mmds_fails".to_string(),
                serde_json::Value::Number(put_api_requests.mmds_fails().into()),
            );
            put_requests.insert(
                "network_count".to_string(),
                serde_json::Value::Number(put_api_requests.network_count().into()),
            );
            put_requests.insert(
                "network_fails".to_string(),
                serde_json::Value::Number(put_api_requests.network_fails().into()),
            );
            put_requests.insert(
                "serial_count".to_string(),
                serde_json::Value::Number(put_api_requests.serial_count().into()),
            );
            put_requests.insert(
                "serial_fails".to_string(),
                serde_json::Value::Number(put_api_requests.serial_fails().into()),
            );
            put_requests.insert(
                "vsock_count".to_string(),
                serde_json::Value::Number(put_api_requests.vsock_count().into()),
            );
            put_requests.insert(
                "vsock_fails".to_string(),
                serde_json::Value::Number(put_api_requests.vsock_fails().into()),
            );
            root.insert(
                "put_api_requests".to_string(),
                serde_json::Value::Object(put_requests),
            );
        }
        root.insert("vmm".to_string(), serde_json::Value::Object(vmm));

        self.output
            .write_json_line(&serde_json::Value::Object(root))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        BootRunLoopMetricStatus, MetricsConfigError, MetricsConfigInput, MetricsDiagnostics,
        MetricsFlushError, MetricsOutput, MetricsState,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn unique_metrics_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-metrics-test-{}-{nanos}-{id}-{name}",
            std::process::id()
        ))
    }

    #[derive(Debug, Clone, Default)]
    struct TestMetricsOutput {
        state: Arc<Mutex<TestMetricsOutputState>>,
    }

    impl TestMetricsOutput {
        fn fail_next_write(&self) {
            self.state
                .lock()
                .expect("test metrics output lock should not be poisoned")
                .fail_next_write = true;
        }

        fn lines(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("test metrics output lock should not be poisoned")
                .lines
                .clone()
        }
    }

    #[derive(Debug, Default)]
    struct TestMetricsOutputState {
        fail_next_write: bool,
        lines: Vec<String>,
    }

    impl MetricsOutput for TestMetricsOutput {
        fn write_json_line(&mut self, line: &serde_json::Value) -> Result<(), MetricsFlushError> {
            let mut state = self
                .state
                .lock()
                .expect("test metrics output lock should not be poisoned");
            if state.fail_next_write {
                state.fail_next_write = false;
                return Err(MetricsFlushError::Write(ErrorKind::BrokenPipe));
            }

            state.lines.push(line.to_string());
            Ok(())
        }
    }

    #[test]
    fn validates_metrics_path() {
        let config = MetricsConfigInput::new("/tmp/metrics")
            .validate()
            .expect("path should validate");

        assert_eq!(config.metrics_path(), PathBuf::from("/tmp/metrics"));
    }

    #[test]
    fn rejects_empty_metrics_path() {
        assert_eq!(
            MetricsConfigInput::new(PathBuf::new()).validate(),
            Err(MetricsConfigError::EmptyPath)
        );
    }

    #[test]
    fn flush_without_configuration_is_noop() {
        let mut state = MetricsState::default();

        assert_eq!(state.flush(), Ok(false));
        assert!(!state.is_configured());
    }

    #[test]
    fn configures_once_and_writes_metrics_lines() {
        let path = unique_metrics_path("configured");
        let mut state = MetricsState::default();

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert!(state.is_configured());
        assert_eq!(state.flush(), Ok(true));
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"vmm\":{\"metrics_flush_count\":1}}\n{\"vmm\":{\"metrics_flush_count\":2}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn failed_flush_records_missed_metrics_without_incrementing_flush_count() {
        let output = TestMetricsOutput::default();
        output.fail_next_write();
        let mut state = MetricsState::with_test_output(output.clone());

        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        assert_eq!(
            output.lines(),
            [r#"{"logger":{"missed_metrics_count":1},"vmm":{"metrics_flush_count":1}}"#]
        );
    }

    #[test]
    fn repeated_failed_flushes_accumulate_missed_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());

        output.fail_next_write();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        output.fail_next_write();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        assert_eq!(
            output.lines(),
            [r#"{"logger":{"missed_metrics_count":2},"vmm":{"metrics_flush_count":1}}"#]
        );
    }

    #[test]
    fn writes_boot_run_loop_diagnostics_when_provided() {
        let path = unique_metrics_path("diagnostics");
        let mut state = MetricsState::default();
        let diagnostics =
            MetricsDiagnostics::new().with_boot_run_loop_status(BootRunLoopMetricStatus::Failed);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"vmm\":{\"boot_run_loop_status\":\"failed\",\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_startup_time_diagnostics_when_provided() {
        let path = unique_metrics_path("startup-time");
        let mut state = MetricsState::default();
        let diagnostics = MetricsDiagnostics::new()
            .with_start_time_us(1000)
            .with_start_time_cpu_us(2000)
            .with_parent_cpu_time_us(3000);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"vmm\":{\"metrics_flush_count\":1,\"parent_cpu_time_us\":3000,\"start_time_cpu_us\":2000,\"start_time_us\":1000}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_put_actions_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-actions");
        let mut state = MetricsState::default();

        state.record_put_actions_request();
        state.record_put_actions_request();
        state.record_put_actions_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":1,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_patch_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-patch");
        let mut state = MetricsState::default();

        state.record_patch_drive_request();
        state.record_patch_drive_failure();
        state.record_patch_machine_config_request();
        state.record_patch_machine_config_request();
        state.record_patch_machine_config_failure();
        state.record_patch_mmds_request();
        state.record_patch_mmds_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"patch_api_requests\":{\"drive_count\":1,\"drive_fails\":1,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"mmds_count\":1,\"mmds_fails\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_put_core_config_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-core-config");
        let mut state = MetricsState::default();

        state.record_put_boot_source_request();
        state.record_put_boot_source_request();
        state.record_put_boot_source_failure();
        state.record_put_cpu_config_request();
        state.record_put_cpu_config_failure();
        state.record_put_drive_request();
        state.record_put_drive_failure();
        state.record_put_machine_config_request();
        state.record_put_machine_config_request();
        state.record_put_machine_config_failure();
        state.record_put_network_request();
        state.record_put_network_failure();
        state.record_put_vsock_request();
        state.record_put_vsock_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"boot_source_count\":2,\"boot_source_fails\":1,\"cpu_cfg_count\":1,\"cpu_cfg_fails\":1,\"drive_count\":1,\"drive_fails\":1,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":1,\"network_fails\":1,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":1,\"vsock_fails\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_put_mmds_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-mmds");
        let mut state = MetricsState::default();

        state.record_put_mmds_request();
        state.record_put_mmds_request();
        state.record_put_mmds_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":2,\"mmds_fails\":1,\"network_count\":0,\"network_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_put_observability_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-observability");
        let mut state = MetricsState::default();

        state.record_put_metrics_request();
        state.record_put_metrics_request();
        state.record_put_metrics_failure();
        state.record_put_logger_request();
        state.record_put_logger_failure();
        state.record_put_serial_request();
        state.record_put_serial_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"logger_count\":1,\"logger_fails\":1,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":2,\"metrics_fails\":1,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"serial_count\":1,\"serial_fails\":1,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_get_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-get");
        let mut state = MetricsState::default();

        state.record_get_instance_info_request();
        state.record_get_vmm_version_request();
        state.record_get_machine_config_request();
        state.record_get_mmds_request();
        state.record_get_mmds_request();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"get_api_requests\":{\"instance_info_count\":1,\"machine_cfg_count\":1,\"mmds_count\":2,\"vmm_version_count\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn merges_independent_diagnostics() {
        let base = MetricsDiagnostics::new()
            .with_start_time_us(1000)
            .with_start_time_cpu_us(2000);
        let session = MetricsDiagnostics::new()
            .with_boot_run_loop_status(BootRunLoopMetricStatus::Running)
            .with_parent_cpu_time_us(3000);

        let diagnostics = base.merged_with(session);

        assert_eq!(
            diagnostics.boot_run_loop_status(),
            Some(BootRunLoopMetricStatus::Running)
        );
        assert_eq!(diagnostics.start_time_us(), Some(1000));
        assert_eq!(diagnostics.start_time_cpu_us(), Some(2000));
        assert_eq!(diagnostics.parent_cpu_time_us(), Some(3000));
    }

    #[test]
    fn rejects_duplicate_configuration_without_replacing_existing_sink() {
        let first_path = unique_metrics_path("first");
        let second_path = unique_metrics_path("second");
        let mut state = MetricsState::default();

        state
            .configure(MetricsConfigInput::new(&first_path))
            .expect("initial metrics should configure");

        assert_eq!(
            state.configure(MetricsConfigInput::new(&second_path)),
            Err(MetricsConfigError::AlreadyInitialized)
        );
        assert_eq!(state.flush(), Ok(true));

        let first_output =
            fs::read_to_string(&first_path).expect("first metrics output should be readable");
        assert_eq!(first_output, "{\"vmm\":{\"metrics_flush_count\":1}}\n");
        assert!(!second_path.exists());

        fs::remove_file(first_path).expect("fixture should clean up");
    }

    #[test]
    fn open_errors_do_not_echo_path() {
        let missing_parent = unique_metrics_path("parent").join("metrics");
        let err = MetricsState::default()
            .configure(MetricsConfigInput::new(&missing_parent))
            .expect_err("missing parent should fail");
        let missing_parent_text = missing_parent.to_string_lossy();

        assert!(matches!(err, MetricsConfigError::OpenFile(_)));
        assert!(!err.to_string().contains(missing_parent_text.as_ref()));
    }

    #[test]
    fn flush_error_display_omits_path_details() {
        let err = MetricsFlushError::Write(std::io::ErrorKind::BrokenPipe);

        assert_eq!(err.to_string(), "failed to flush metrics: BrokenPipe");
    }
}
