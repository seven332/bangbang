use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::balloon::VirtioBalloonDeviceNotificationDispatch;
use crate::block::{VirtioBlockDeviceNotificationDispatch, VirtioBlockQueueDispatch};
use crate::serial::SerialOutputMetrics;

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
    deprecated_api: DeprecatedApiMetrics,
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

    pub(crate) fn record_deprecated_api_call(&mut self) {
        self.deprecated_api.record_deprecated_http_api_call();
    }

    pub(crate) fn record_put_actions_request(&mut self) {
        self.put_api_requests.record_actions_request();
    }

    pub(crate) fn record_put_actions_failure(&mut self) {
        self.put_api_requests.record_actions_failure();
    }

    pub(crate) fn record_put_balloon_request(&mut self) {
        self.put_api_requests.record_balloon_request();
    }

    pub(crate) fn record_put_balloon_failure(&mut self) {
        self.put_api_requests.record_balloon_failure();
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

    pub(crate) fn record_put_hotplug_memory_request(&mut self) {
        self.put_api_requests.record_hotplug_memory_request();
    }

    pub(crate) fn record_put_hotplug_memory_failure(&mut self) {
        self.put_api_requests.record_hotplug_memory_failure();
    }

    pub(crate) fn record_put_pmem_request(&mut self) {
        self.put_api_requests.record_pmem_request();
    }

    pub(crate) fn record_put_pmem_failure(&mut self) {
        self.put_api_requests.record_pmem_failure();
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

    pub(crate) fn record_patch_balloon_request(&mut self) {
        self.patch_api_requests.record_balloon_request();
    }

    pub(crate) fn record_patch_balloon_failure(&mut self) {
        self.patch_api_requests.record_balloon_failure();
    }

    pub(crate) fn record_patch_network_request(&mut self) {
        self.patch_api_requests.record_network_request();
    }

    pub(crate) fn record_patch_network_failure(&mut self) {
        self.patch_api_requests.record_network_failure();
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

    pub(crate) fn record_patch_hotplug_memory_request(&mut self) {
        self.patch_api_requests.record_hotplug_memory_request();
    }

    pub(crate) fn record_patch_hotplug_memory_failure(&mut self) {
        self.patch_api_requests.record_hotplug_memory_failure();
    }

    pub(crate) fn record_patch_pmem_request(&mut self) {
        self.patch_api_requests.record_pmem_request();
    }

    pub(crate) fn record_patch_pmem_failure(&mut self) {
        self.patch_api_requests.record_pmem_failure();
    }

    pub(crate) fn record_get_balloon_request(&mut self) {
        self.get_api_requests.record_balloon_request();
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

    pub(crate) fn record_get_hotplug_memory_request(&mut self) {
        self.get_api_requests.record_hotplug_memory_request();
    }

    pub(crate) fn record_missed_log(&mut self) {
        self.logger_metrics.record_missed_log();
    }

    pub fn flush_with_diagnostics(
        &mut self,
        diagnostics: &MetricsDiagnostics,
    ) -> Result<bool, MetricsFlushError> {
        let Some(sink) = &mut self.sink else {
            return Ok(false);
        };
        let next_flush_count = self.flush_count.saturating_add(1);
        let snapshot = MinimalMetricsSnapshot {
            flush_count: next_flush_count,
            diagnostics,
            deprecated_api: self.deprecated_api,
            get_api_requests: self.get_api_requests,
            logger_metrics: self.logger_metrics,
            patch_api_requests: self.patch_api_requests,
            put_api_requests: self.put_api_requests,
        };
        if let Err(err) = sink.write_minimal_metrics(snapshot) {
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
struct DeprecatedApiMetrics {
    deprecated_http_api_calls: u64,
}

impl DeprecatedApiMetrics {
    const fn is_empty(self) -> bool {
        self.deprecated_http_api_calls == 0
    }

    fn record_deprecated_http_api_call(&mut self) {
        self.deprecated_http_api_calls = self.deprecated_http_api_calls.saturating_add(1);
    }

    const fn deprecated_http_api_calls(self) -> u64 {
        self.deprecated_http_api_calls
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GetApiRequestMetrics {
    balloon_count: u64,
    hotplug_memory_count: u64,
    instance_info_count: u64,
    vmm_version_count: u64,
    machine_cfg_count: u64,
    mmds_count: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LoggerMetrics {
    missed_log_count: u64,
    missed_metrics_count: u64,
}

impl LoggerMetrics {
    const fn is_empty(self) -> bool {
        self.missed_log_count == 0 && self.missed_metrics_count == 0
    }

    fn record_missed_log(&mut self) {
        self.missed_log_count = self.missed_log_count.saturating_add(1);
    }

    fn record_missed_metrics(&mut self) {
        self.missed_metrics_count = self.missed_metrics_count.saturating_add(1);
    }

    const fn missed_log_count(self) -> u64 {
        self.missed_log_count
    }

    const fn missed_metrics_count(self) -> u64 {
        self.missed_metrics_count
    }
}

impl GetApiRequestMetrics {
    const fn is_empty(self) -> bool {
        self.balloon_count == 0
            && self.hotplug_memory_count == 0
            && self.instance_info_count == 0
            && self.vmm_version_count == 0
            && self.machine_cfg_count == 0
            && self.mmds_count == 0
    }

    fn record_balloon_request(&mut self) {
        self.balloon_count = self.balloon_count.saturating_add(1);
    }

    fn record_hotplug_memory_request(&mut self) {
        self.hotplug_memory_count = self.hotplug_memory_count.saturating_add(1);
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

    const fn balloon_count(self) -> u64 {
        self.balloon_count
    }

    const fn hotplug_memory_count(self) -> u64 {
        self.hotplug_memory_count
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
    balloon_count: u64,
    balloon_fails: u64,
    drive_count: u64,
    drive_fails: u64,
    network_count: u64,
    network_fails: u64,
    machine_cfg_count: u64,
    machine_cfg_fails: u64,
    mmds_count: u64,
    mmds_fails: u64,
    hotplug_memory_count: u64,
    hotplug_memory_fails: u64,
    pmem_count: u64,
    pmem_fails: u64,
}

impl PatchApiRequestMetrics {
    const fn is_empty(self) -> bool {
        self.balloon_count == 0
            && self.balloon_fails == 0
            && self.drive_count == 0
            && self.drive_fails == 0
            && self.network_count == 0
            && self.network_fails == 0
            && self.machine_cfg_count == 0
            && self.machine_cfg_fails == 0
            && self.mmds_count == 0
            && self.mmds_fails == 0
            && self.hotplug_memory_count == 0
            && self.hotplug_memory_fails == 0
            && self.pmem_count == 0
            && self.pmem_fails == 0
    }

    fn record_drive_request(&mut self) {
        self.drive_count = self.drive_count.saturating_add(1);
    }

    fn record_drive_failure(&mut self) {
        self.drive_fails = self.drive_fails.saturating_add(1);
    }

    fn record_balloon_request(&mut self) {
        self.balloon_count = self.balloon_count.saturating_add(1);
    }

    fn record_balloon_failure(&mut self) {
        self.balloon_fails = self.balloon_fails.saturating_add(1);
    }

    fn record_network_request(&mut self) {
        self.network_count = self.network_count.saturating_add(1);
    }

    fn record_network_failure(&mut self) {
        self.network_fails = self.network_fails.saturating_add(1);
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

    fn record_hotplug_memory_request(&mut self) {
        self.hotplug_memory_count = self.hotplug_memory_count.saturating_add(1);
    }

    fn record_hotplug_memory_failure(&mut self) {
        self.hotplug_memory_fails = self.hotplug_memory_fails.saturating_add(1);
    }

    fn record_pmem_request(&mut self) {
        self.pmem_count = self.pmem_count.saturating_add(1);
    }

    fn record_pmem_failure(&mut self) {
        self.pmem_fails = self.pmem_fails.saturating_add(1);
    }

    const fn drive_count(self) -> u64 {
        self.drive_count
    }

    const fn drive_fails(self) -> u64 {
        self.drive_fails
    }

    const fn balloon_count(self) -> u64 {
        self.balloon_count
    }

    const fn balloon_fails(self) -> u64 {
        self.balloon_fails
    }

    const fn network_count(self) -> u64 {
        self.network_count
    }

    const fn network_fails(self) -> u64 {
        self.network_fails
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

    const fn hotplug_memory_count(self) -> u64 {
        self.hotplug_memory_count
    }

    const fn hotplug_memory_fails(self) -> u64 {
        self.hotplug_memory_fails
    }

    const fn pmem_count(self) -> u64 {
        self.pmem_count
    }

    const fn pmem_fails(self) -> u64 {
        self.pmem_fails
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PutApiRequestMetrics {
    actions_count: u64,
    actions_fails: u64,
    balloon_count: u64,
    balloon_fails: u64,
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
    hotplug_memory_count: u64,
    hotplug_memory_fails: u64,
    mmds_count: u64,
    mmds_fails: u64,
    network_count: u64,
    network_fails: u64,
    pmem_count: u64,
    pmem_fails: u64,
    serial_count: u64,
    serial_fails: u64,
    vsock_count: u64,
    vsock_fails: u64,
}

impl PutApiRequestMetrics {
    const fn is_empty(self) -> bool {
        self.actions_count == 0
            && self.actions_fails == 0
            && self.balloon_count == 0
            && self.balloon_fails == 0
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
            && self.hotplug_memory_count == 0
            && self.hotplug_memory_fails == 0
            && self.mmds_count == 0
            && self.mmds_fails == 0
            && self.network_count == 0
            && self.network_fails == 0
            && self.pmem_count == 0
            && self.pmem_fails == 0
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

    fn record_balloon_request(&mut self) {
        self.balloon_count = self.balloon_count.saturating_add(1);
    }

    fn record_balloon_failure(&mut self) {
        self.balloon_fails = self.balloon_fails.saturating_add(1);
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

    fn record_hotplug_memory_request(&mut self) {
        self.hotplug_memory_count = self.hotplug_memory_count.saturating_add(1);
    }

    fn record_hotplug_memory_failure(&mut self) {
        self.hotplug_memory_fails = self.hotplug_memory_fails.saturating_add(1);
    }

    fn record_network_request(&mut self) {
        self.network_count = self.network_count.saturating_add(1);
    }

    fn record_network_failure(&mut self) {
        self.network_fails = self.network_fails.saturating_add(1);
    }

    fn record_pmem_request(&mut self) {
        self.pmem_count = self.pmem_count.saturating_add(1);
    }

    fn record_pmem_failure(&mut self) {
        self.pmem_fails = self.pmem_fails.saturating_add(1);
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

    const fn balloon_count(self) -> u64 {
        self.balloon_count
    }

    const fn balloon_fails(self) -> u64 {
        self.balloon_fails
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

    const fn hotplug_memory_count(self) -> u64 {
        self.hotplug_memory_count
    }

    const fn hotplug_memory_fails(self) -> u64 {
        self.hotplug_memory_fails
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

    const fn pmem_count(self) -> u64 {
        self.pmem_count
    }

    const fn pmem_fails(self) -> u64 {
        self.pmem_fails
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
pub struct BlockDeviceMetrics {
    event_fails: u64,
    execute_fails: u64,
    invalid_reqs_count: u64,
    flush_count: u64,
    queue_event_count: u64,
    read_bytes: u64,
    write_bytes: u64,
    read_count: u64,
    write_count: u64,
}

impl BlockDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.event_fails == 0
            && self.execute_fails == 0
            && self.invalid_reqs_count == 0
            && self.flush_count == 0
            && self.queue_event_count == 0
            && self.read_bytes == 0
            && self.write_bytes == 0
            && self.read_count == 0
            && self.write_count == 0
    }

    pub const fn event_fails(self) -> u64 {
        self.event_fails
    }

    pub const fn execute_fails(self) -> u64 {
        self.execute_fails
    }

    pub const fn invalid_reqs_count(self) -> u64 {
        self.invalid_reqs_count
    }

    pub const fn flush_count(self) -> u64 {
        self.flush_count
    }

    pub const fn queue_event_count(self) -> u64 {
        self.queue_event_count
    }

    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    pub const fn write_bytes(self) -> u64 {
        self.write_bytes
    }

    pub const fn read_count(self) -> u64 {
        self.read_count
    }

    pub const fn write_count(self) -> u64 {
        self.write_count
    }

    pub const fn with_event_fails(mut self, event_fails: u64) -> Self {
        self.event_fails = event_fails;
        self
    }

    pub const fn with_execute_fails(mut self, execute_fails: u64) -> Self {
        self.execute_fails = execute_fails;
        self
    }

    pub const fn with_invalid_reqs_count(mut self, invalid_reqs_count: u64) -> Self {
        self.invalid_reqs_count = invalid_reqs_count;
        self
    }

    pub const fn with_flush_count(mut self, flush_count: u64) -> Self {
        self.flush_count = flush_count;
        self
    }

    pub const fn with_queue_event_count(mut self, queue_event_count: u64) -> Self {
        self.queue_event_count = queue_event_count;
        self
    }

    pub const fn with_read_bytes(mut self, read_bytes: u64) -> Self {
        self.read_bytes = read_bytes;
        self
    }

    pub const fn with_write_bytes(mut self, write_bytes: u64) -> Self {
        self.write_bytes = write_bytes;
        self
    }

    pub const fn with_read_count(mut self, read_count: u64) -> Self {
        self.read_count = read_count;
        self
    }

    pub const fn with_write_count(mut self, write_count: u64) -> Self {
        self.write_count = write_count;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            event_fails: self.event_fails.saturating_add(other.event_fails),
            execute_fails: self.execute_fails.saturating_add(other.execute_fails),
            invalid_reqs_count: self
                .invalid_reqs_count
                .saturating_add(other.invalid_reqs_count),
            flush_count: self.flush_count.saturating_add(other.flush_count),
            queue_event_count: self
                .queue_event_count
                .saturating_add(other.queue_event_count),
            read_bytes: self.read_bytes.saturating_add(other.read_bytes),
            write_bytes: self.write_bytes.saturating_add(other.write_bytes),
            read_count: self.read_count.saturating_add(other.read_count),
            write_count: self.write_count.saturating_add(other.write_count),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockDeviceMetricsByDrive {
    metrics: BTreeMap<String, BlockDeviceMetrics>,
}

impl BlockDeviceMetricsByDrive {
    pub fn new() -> Self {
        Self {
            metrics: BTreeMap::new(),
        }
    }

    pub fn with_drive_metrics(
        mut self,
        drive_id: impl Into<String>,
        metrics: BlockDeviceMetrics,
    ) -> Self {
        self.insert_drive_metrics(drive_id, metrics);
        self
    }

    pub fn insert_drive_metrics(
        &mut self,
        drive_id: impl Into<String>,
        metrics: BlockDeviceMetrics,
    ) {
        self.metrics
            .entry(drive_id.into())
            .and_modify(|existing| *existing = existing.merged_with(metrics))
            .or_insert(metrics);
    }

    pub fn is_empty(&self) -> bool {
        self.metrics
            .values()
            .all(|metrics| BlockDeviceMetrics::is_empty(*metrics))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, BlockDeviceMetrics)> {
        self.metrics
            .iter()
            .map(|(drive_id, metrics)| (drive_id.as_str(), *metrics))
    }

    fn merged_with(mut self, other: Self) -> Self {
        for (drive_id, metrics) in other.metrics {
            self.insert_drive_metrics(drive_id, metrics);
        }
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedBlockDeviceMetrics {
    inner: Arc<SharedBlockDeviceMetricsInner>,
}

impl SharedBlockDeviceMetrics {
    pub fn record_notification_dispatch(&self, dispatch: &VirtioBlockDeviceNotificationDispatch) {
        self.record_queue_events(usize_to_u64_saturating(
            dispatch.drained_notifications().len(),
        ));
        if let Some(queue_dispatch) = dispatch.queue_dispatch() {
            self.record_queue_dispatch(queue_dispatch);
        }
    }

    pub fn record_queue_dispatch(&self, dispatch: &VirtioBlockQueueDispatch) {
        self.record_reads(
            usize_to_u64_saturating(dispatch.read_count()),
            dispatch.read_bytes(),
        );
        self.record_writes(
            usize_to_u64_saturating(dispatch.write_count()),
            dispatch.write_bytes(),
        );
        self.record_flushes(usize_to_u64_saturating(dispatch.flush_count()));
        self.record_execute_failures(usize_to_u64_saturating(
            dispatch
                .parse_failures()
                .saturating_add(dispatch.status_write_failures()),
        ));
        self.record_invalid_requests(usize_to_u64_saturating(
            dispatch
                .io_errors()
                .saturating_add(dispatch.unsupported_requests()),
        ));
    }

    pub fn record_queue_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.queue_event_count, count);
        }
    }

    pub fn record_event_failure(&self) {
        record_atomic_metric(&self.inner.event_fails, 1);
    }

    pub fn snapshot(&self) -> BlockDeviceMetrics {
        BlockDeviceMetrics {
            event_fails: self.inner.event_fails.load(Ordering::Relaxed),
            execute_fails: self.inner.execute_fails.load(Ordering::Relaxed),
            invalid_reqs_count: self.inner.invalid_reqs_count.load(Ordering::Relaxed),
            flush_count: self.inner.flush_count.load(Ordering::Relaxed),
            queue_event_count: self.inner.queue_event_count.load(Ordering::Relaxed),
            read_bytes: self.inner.read_bytes.load(Ordering::Relaxed),
            write_bytes: self.inner.write_bytes.load(Ordering::Relaxed),
            read_count: self.inner.read_count.load(Ordering::Relaxed),
            write_count: self.inner.write_count.load(Ordering::Relaxed),
        }
    }

    fn record_reads(&self, count: u64, bytes: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.read_count, count);
        }
        if bytes != 0 {
            record_atomic_metric(&self.inner.read_bytes, bytes);
        }
    }

    fn record_writes(&self, count: u64, bytes: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.write_count, count);
        }
        if bytes != 0 {
            record_atomic_metric(&self.inner.write_bytes, bytes);
        }
    }

    fn record_flushes(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.flush_count, count);
        }
    }

    fn record_execute_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.execute_fails, count);
        }
    }

    fn record_invalid_requests(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.invalid_reqs_count, count);
        }
    }
}

#[derive(Debug, Default)]
struct SharedBlockDeviceMetricsInner {
    event_fails: AtomicU64,
    execute_fails: AtomicU64,
    invalid_reqs_count: AtomicU64,
    flush_count: AtomicU64,
    queue_event_count: AtomicU64,
    read_bytes: AtomicU64,
    write_bytes: AtomicU64,
    read_count: AtomicU64,
    write_count: AtomicU64,
}

#[derive(Debug, Clone, Default)]
pub struct SharedBlockDeviceMetricsRegistry {
    aggregate: SharedBlockDeviceMetrics,
    per_drive: Arc<BTreeMap<String, SharedBlockDeviceMetrics>>,
}

impl SharedBlockDeviceMetricsRegistry {
    pub fn from_drive_ids<'a>(drive_ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut per_drive = BTreeMap::new();
        for drive_id in drive_ids {
            per_drive
                .entry(drive_id.to_string())
                .or_insert_with(SharedBlockDeviceMetrics::default);
        }

        Self {
            aggregate: SharedBlockDeviceMetrics::default(),
            per_drive: Arc::new(per_drive),
        }
    }

    pub fn aggregate(&self) -> SharedBlockDeviceMetrics {
        self.aggregate.clone()
    }

    pub fn per_drive(&self, drive_id: &str) -> Option<SharedBlockDeviceMetrics> {
        self.per_drive.get(drive_id).cloned()
    }

    pub fn record_notification_dispatch_for_drive(
        &self,
        drive_id: &str,
        dispatch: &VirtioBlockDeviceNotificationDispatch,
    ) {
        self.aggregate.record_notification_dispatch(dispatch);
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_notification_dispatch(dispatch);
        }
    }

    pub fn record_queue_dispatch_for_drive(
        &self,
        drive_id: &str,
        dispatch: &VirtioBlockQueueDispatch,
    ) {
        self.aggregate.record_queue_dispatch(dispatch);
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_queue_dispatch(dispatch);
        }
    }

    pub fn record_queue_events_for_drive(&self, drive_id: &str, count: u64) {
        self.aggregate.record_queue_events(count);
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_queue_events(count);
        }
    }

    pub fn record_event_failure(&self) {
        self.aggregate.record_event_failure();
    }

    pub fn record_event_failure_for_drive(&self, drive_id: &str) {
        self.aggregate.record_event_failure();
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_event_failure();
        }
    }

    pub fn aggregate_snapshot(&self) -> BlockDeviceMetrics {
        self.aggregate.snapshot()
    }

    pub fn per_drive_snapshot(&self) -> BlockDeviceMetricsByDrive {
        let mut snapshot = BlockDeviceMetricsByDrive::new();
        for (drive_id, metrics) in self.per_drive.iter() {
            let metrics = metrics.snapshot();
            if !metrics.is_empty() {
                snapshot.insert_drive_metrics(drive_id.clone(), metrics);
            }
        }
        snapshot
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BalloonDeviceMetrics {
    activate_fails: u64,
    inflate_count: u64,
    stats_updates_count: u64,
    stats_update_fails: u64,
    deflate_count: u64,
    event_fails: u64,
}

impl BalloonDeviceMetrics {
    pub const fn new(
        activate_fails: u64,
        inflate_count: u64,
        stats_updates_count: u64,
        stats_update_fails: u64,
        deflate_count: u64,
        event_fails: u64,
    ) -> Self {
        Self {
            activate_fails,
            inflate_count,
            stats_updates_count,
            stats_update_fails,
            deflate_count,
            event_fails,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.inflate_count == 0
            && self.stats_updates_count == 0
            && self.stats_update_fails == 0
            && self.deflate_count == 0
            && self.event_fails == 0
    }

    pub const fn activate_fails(self) -> u64 {
        self.activate_fails
    }

    pub const fn inflate_count(self) -> u64 {
        self.inflate_count
    }

    pub const fn stats_updates_count(self) -> u64 {
        self.stats_updates_count
    }

    pub const fn stats_update_fails(self) -> u64 {
        self.stats_update_fails
    }

    pub const fn deflate_count(self) -> u64 {
        self.deflate_count
    }

    pub const fn event_fails(self) -> u64 {
        self.event_fails
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            activate_fails: self.activate_fails.saturating_add(other.activate_fails),
            inflate_count: self.inflate_count.saturating_add(other.inflate_count),
            stats_updates_count: self
                .stats_updates_count
                .saturating_add(other.stats_updates_count),
            stats_update_fails: self
                .stats_update_fails
                .saturating_add(other.stats_update_fails),
            deflate_count: self.deflate_count.saturating_add(other.deflate_count),
            event_fails: self.event_fails.saturating_add(other.event_fails),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedBalloonDeviceMetrics {
    inner: Arc<SharedBalloonDeviceMetricsInner>,
}

impl SharedBalloonDeviceMetrics {
    pub fn record_activation_failure(&self) {
        record_atomic_metric(&self.inner.activate_fails, 1);
    }

    pub fn record_notification_dispatch(&self, dispatch: &VirtioBalloonDeviceNotificationDispatch) {
        self.record_inflations(usize_to_u64_saturating(dispatch.inflate_notifications()));
        self.record_deflations(usize_to_u64_saturating(dispatch.deflate_notifications()));

        let stats_updates = if dispatch.statistics_notifications() != 0 {
            dispatch.statistics_notifications()
        } else {
            dispatch
                .statistics_queue_dispatch()
                .map(|queue| queue.completed_descriptors())
                .unwrap_or_default()
        };
        self.record_statistics_updates(usize_to_u64_saturating(stats_updates));
    }

    pub fn record_statistics_update_failure(&self) {
        record_atomic_metric(&self.inner.stats_update_fails, 1);
    }

    pub fn record_event_failure(&self) {
        record_atomic_metric(&self.inner.event_fails, 1);
    }

    pub fn snapshot(&self) -> BalloonDeviceMetrics {
        BalloonDeviceMetrics::new(
            self.inner.activate_fails.load(Ordering::Relaxed),
            self.inner.inflate_count.load(Ordering::Relaxed),
            self.inner.stats_updates_count.load(Ordering::Relaxed),
            self.inner.stats_update_fails.load(Ordering::Relaxed),
            self.inner.deflate_count.load(Ordering::Relaxed),
            self.inner.event_fails.load(Ordering::Relaxed),
        )
    }

    fn record_inflations(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.inflate_count, count);
        }
    }

    fn record_deflations(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.deflate_count, count);
        }
    }

    fn record_statistics_updates(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.stats_updates_count, count);
        }
    }
}

#[derive(Debug, Default)]
struct SharedBalloonDeviceMetricsInner {
    activate_fails: AtomicU64,
    inflate_count: AtomicU64,
    stats_updates_count: AtomicU64,
    stats_update_fails: AtomicU64,
    deflate_count: AtomicU64,
    event_fails: AtomicU64,
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn record_atomic_metric(metric: &AtomicU64, increment: u64) {
    let mut current = metric.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(increment);
        match metric.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsDiagnostics {
    block_device_metrics: Option<BlockDeviceMetrics>,
    block_device_metrics_by_drive: Option<BlockDeviceMetricsByDrive>,
    balloon_device_metrics: Option<BalloonDeviceMetrics>,
    boot_run_loop_status: Option<BootRunLoopMetricStatus>,
    start_time_us: Option<u64>,
    start_time_cpu_us: Option<u64>,
    parent_cpu_time_us: Option<u64>,
    serial_output_metrics: Option<SerialOutputMetrics>,
}

impl MetricsDiagnostics {
    pub fn new() -> Self {
        Self {
            block_device_metrics: None,
            block_device_metrics_by_drive: None,
            balloon_device_metrics: None,
            boot_run_loop_status: None,
            start_time_us: None,
            start_time_cpu_us: None,
            parent_cpu_time_us: None,
            serial_output_metrics: None,
        }
    }

    pub fn with_block_device_metrics(mut self, block_device_metrics: BlockDeviceMetrics) -> Self {
        self.block_device_metrics = Some(block_device_metrics);
        self
    }

    pub fn with_block_device_metrics_by_drive(
        mut self,
        block_device_metrics_by_drive: BlockDeviceMetricsByDrive,
    ) -> Self {
        self.block_device_metrics_by_drive = Some(block_device_metrics_by_drive);
        self
    }

    pub fn with_balloon_device_metrics(
        mut self,
        balloon_device_metrics: BalloonDeviceMetrics,
    ) -> Self {
        self.balloon_device_metrics = Some(balloon_device_metrics);
        self
    }

    pub fn with_boot_run_loop_status(mut self, status: BootRunLoopMetricStatus) -> Self {
        self.boot_run_loop_status = Some(status);
        self
    }

    pub fn with_start_time_us(mut self, start_time_us: u64) -> Self {
        self.start_time_us = Some(start_time_us);
        self
    }

    pub fn with_start_time_cpu_us(mut self, start_time_cpu_us: u64) -> Self {
        self.start_time_cpu_us = Some(start_time_cpu_us);
        self
    }

    pub fn with_parent_cpu_time_us(mut self, parent_cpu_time_us: u64) -> Self {
        self.parent_cpu_time_us = Some(parent_cpu_time_us);
        self
    }

    pub fn with_serial_output_metrics(
        mut self,
        serial_output_metrics: SerialOutputMetrics,
    ) -> Self {
        self.serial_output_metrics = Some(serial_output_metrics);
        self
    }

    pub fn merged_with(mut self, other: Self) -> Self {
        if let Some(metrics) = other.block_device_metrics {
            self.block_device_metrics = Some(match self.block_device_metrics {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }
        if let Some(metrics) = other.block_device_metrics_by_drive {
            self.block_device_metrics_by_drive = Some(match self.block_device_metrics_by_drive {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }
        if let Some(metrics) = other.balloon_device_metrics {
            self.balloon_device_metrics = Some(match self.balloon_device_metrics {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }
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
        if other.serial_output_metrics.is_some() {
            self.serial_output_metrics = other.serial_output_metrics;
        }

        self
    }

    pub fn block_device_metrics(&self) -> Option<BlockDeviceMetrics> {
        self.block_device_metrics
    }

    pub fn block_device_metrics_by_drive(&self) -> Option<&BlockDeviceMetricsByDrive> {
        self.block_device_metrics_by_drive.as_ref()
    }

    pub fn balloon_device_metrics(&self) -> Option<BalloonDeviceMetrics> {
        self.balloon_device_metrics
    }

    pub fn boot_run_loop_status(&self) -> Option<BootRunLoopMetricStatus> {
        self.boot_run_loop_status
    }

    pub fn start_time_us(&self) -> Option<u64> {
        self.start_time_us
    }

    pub fn start_time_cpu_us(&self) -> Option<u64> {
        self.start_time_cpu_us
    }

    pub fn parent_cpu_time_us(&self) -> Option<u64> {
        self.parent_cpu_time_us
    }

    pub fn serial_output_metrics(&self) -> Option<SerialOutputMetrics> {
        self.serial_output_metrics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootRunLoopMetricStatus {
    Running,
    Paused,
    Exited,
    Failed,
}

impl BootRunLoopMetricStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
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

#[derive(Debug, Clone, Copy)]
struct MinimalMetricsSnapshot<'a> {
    flush_count: u64,
    diagnostics: &'a MetricsDiagnostics,
    deprecated_api: DeprecatedApiMetrics,
    get_api_requests: GetApiRequestMetrics,
    logger_metrics: LoggerMetrics,
    patch_api_requests: PatchApiRequestMetrics,
    put_api_requests: PutApiRequestMetrics,
}

impl MetricsOutput for FileMetricsOutput {
    fn write_json_line(&mut self, line: &serde_json::Value) -> Result<(), MetricsFlushError> {
        writeln!(self.writer, "{line}").map_err(|err| MetricsFlushError::Write(err.kind()))?;
        self.writer
            .flush()
            .map_err(|err| MetricsFlushError::Write(err.kind()))
    }
}

fn block_device_metrics_json_object(
    metrics: BlockDeviceMetrics,
) -> serde_json::Map<String, serde_json::Value> {
    let mut block = serde_json::Map::new();
    block.insert(
        "event_fails".to_string(),
        serde_json::Value::Number(metrics.event_fails().into()),
    );
    block.insert(
        "execute_fails".to_string(),
        serde_json::Value::Number(metrics.execute_fails().into()),
    );
    block.insert(
        "flush_count".to_string(),
        serde_json::Value::Number(metrics.flush_count().into()),
    );
    block.insert(
        "invalid_reqs_count".to_string(),
        serde_json::Value::Number(metrics.invalid_reqs_count().into()),
    );
    block.insert(
        "queue_event_count".to_string(),
        serde_json::Value::Number(metrics.queue_event_count().into()),
    );
    block.insert(
        "read_bytes".to_string(),
        serde_json::Value::Number(metrics.read_bytes().into()),
    );
    block.insert(
        "read_count".to_string(),
        serde_json::Value::Number(metrics.read_count().into()),
    );
    block.insert(
        "write_bytes".to_string(),
        serde_json::Value::Number(metrics.write_bytes().into()),
    );
    block.insert(
        "write_count".to_string(),
        serde_json::Value::Number(metrics.write_count().into()),
    );
    block
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
        snapshot: MinimalMetricsSnapshot<'_>,
    ) -> Result<(), MetricsFlushError> {
        let MinimalMetricsSnapshot {
            flush_count,
            diagnostics,
            deprecated_api,
            get_api_requests,
            logger_metrics,
            patch_api_requests,
            put_api_requests,
        } = snapshot;
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

        let mut root = serde_json::Map::new();
        let mut api_server = serde_json::Map::new();
        if let Some(value) = diagnostics.start_time_us() {
            api_server.insert(
                "process_startup_time_us".to_string(),
                serde_json::Value::Number(value.into()),
            );
        }
        if let Some(value) = diagnostics.start_time_cpu_us() {
            let process_startup_time_cpu_us =
                value.saturating_add(diagnostics.parent_cpu_time_us().unwrap_or_default());
            api_server.insert(
                "process_startup_time_cpu_us".to_string(),
                serde_json::Value::Number(process_startup_time_cpu_us.into()),
            );
        }
        if !api_server.is_empty() {
            root.insert(
                "api_server".to_string(),
                serde_json::Value::Object(api_server),
            );
        }
        if !deprecated_api.is_empty() {
            let mut deprecated = serde_json::Map::new();
            deprecated.insert(
                "deprecated_http_api_calls".to_string(),
                serde_json::Value::Number(deprecated_api.deprecated_http_api_calls().into()),
            );
            root.insert(
                "deprecated_api".to_string(),
                serde_json::Value::Object(deprecated),
            );
        }
        if let Some(block_device_metrics_by_drive) = diagnostics.block_device_metrics_by_drive() {
            for (drive_id, metrics) in block_device_metrics_by_drive.iter() {
                if !metrics.is_empty() {
                    root.insert(
                        format!("block_{drive_id}"),
                        serde_json::Value::Object(block_device_metrics_json_object(metrics)),
                    );
                }
            }
        }
        if let Some(block_device_metrics) = diagnostics.block_device_metrics()
            && !block_device_metrics.is_empty()
        {
            root.insert(
                "block".to_string(),
                serde_json::Value::Object(block_device_metrics_json_object(block_device_metrics)),
            );
        }
        if let Some(balloon_device_metrics) = diagnostics.balloon_device_metrics()
            && !balloon_device_metrics.is_empty()
        {
            let mut balloon = serde_json::Map::new();
            balloon.insert(
                "activate_fails".to_string(),
                serde_json::Value::Number(balloon_device_metrics.activate_fails().into()),
            );
            balloon.insert(
                "deflate_count".to_string(),
                serde_json::Value::Number(balloon_device_metrics.deflate_count().into()),
            );
            balloon.insert(
                "event_fails".to_string(),
                serde_json::Value::Number(balloon_device_metrics.event_fails().into()),
            );
            balloon.insert(
                "inflate_count".to_string(),
                serde_json::Value::Number(balloon_device_metrics.inflate_count().into()),
            );
            balloon.insert(
                "stats_update_fails".to_string(),
                serde_json::Value::Number(balloon_device_metrics.stats_update_fails().into()),
            );
            balloon.insert(
                "stats_updates_count".to_string(),
                serde_json::Value::Number(balloon_device_metrics.stats_updates_count().into()),
            );
            root.insert("balloon".to_string(), serde_json::Value::Object(balloon));
        }
        if !get_api_requests.is_empty() {
            let mut get_requests = serde_json::Map::new();
            get_requests.insert(
                "balloon_count".to_string(),
                serde_json::Value::Number(get_api_requests.balloon_count().into()),
            );
            get_requests.insert(
                "hotplug_memory_count".to_string(),
                serde_json::Value::Number(get_api_requests.hotplug_memory_count().into()),
            );
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
            if logger_metrics.missed_log_count() != 0 {
                logger.insert(
                    "missed_log_count".to_string(),
                    serde_json::Value::Number(logger_metrics.missed_log_count().into()),
                );
            }
            if logger_metrics.missed_metrics_count() != 0 {
                logger.insert(
                    "missed_metrics_count".to_string(),
                    serde_json::Value::Number(logger_metrics.missed_metrics_count().into()),
                );
            }
            root.insert("logger".to_string(), serde_json::Value::Object(logger));
        }
        if let Some(serial_output_metrics) = diagnostics.serial_output_metrics()
            && !serial_output_metrics.is_empty()
        {
            let mut uart = serde_json::Map::new();
            if serial_output_metrics.rate_limiter_dropped_bytes() != 0 {
                uart.insert(
                    "rate_limiter_dropped_bytes".to_string(),
                    serde_json::Value::Number(
                        serial_output_metrics.rate_limiter_dropped_bytes().into(),
                    ),
                );
            }
            root.insert("uart".to_string(), serde_json::Value::Object(uart));
        }
        if !patch_api_requests.is_empty() {
            let mut patch_requests = serde_json::Map::new();
            patch_requests.insert(
                "balloon_count".to_string(),
                serde_json::Value::Number(patch_api_requests.balloon_count().into()),
            );
            patch_requests.insert(
                "balloon_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.balloon_fails().into()),
            );
            patch_requests.insert(
                "drive_count".to_string(),
                serde_json::Value::Number(patch_api_requests.drive_count().into()),
            );
            patch_requests.insert(
                "drive_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.drive_fails().into()),
            );
            patch_requests.insert(
                "network_count".to_string(),
                serde_json::Value::Number(patch_api_requests.network_count().into()),
            );
            patch_requests.insert(
                "network_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.network_fails().into()),
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
            patch_requests.insert(
                "hotplug_memory_count".to_string(),
                serde_json::Value::Number(patch_api_requests.hotplug_memory_count().into()),
            );
            patch_requests.insert(
                "hotplug_memory_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.hotplug_memory_fails().into()),
            );
            patch_requests.insert(
                "pmem_count".to_string(),
                serde_json::Value::Number(patch_api_requests.pmem_count().into()),
            );
            patch_requests.insert(
                "pmem_fails".to_string(),
                serde_json::Value::Number(patch_api_requests.pmem_fails().into()),
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
                "balloon_count".to_string(),
                serde_json::Value::Number(put_api_requests.balloon_count().into()),
            );
            put_requests.insert(
                "balloon_fails".to_string(),
                serde_json::Value::Number(put_api_requests.balloon_fails().into()),
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
                "hotplug_memory_count".to_string(),
                serde_json::Value::Number(put_api_requests.hotplug_memory_count().into()),
            );
            put_requests.insert(
                "hotplug_memory_fails".to_string(),
                serde_json::Value::Number(put_api_requests.hotplug_memory_fails().into()),
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
                "pmem_count".to_string(),
                serde_json::Value::Number(put_api_requests.pmem_count().into()),
            );
            put_requests.insert(
                "pmem_fails".to_string(),
                serde_json::Value::Number(put_api_requests.pmem_fails().into()),
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
        BalloonDeviceMetrics, BlockDeviceMetrics, BlockDeviceMetricsByDrive,
        BootRunLoopMetricStatus, MetricsConfigError, MetricsConfigInput, MetricsDiagnostics,
        MetricsFlushError, MetricsOutput, MetricsState, SharedBalloonDeviceMetrics,
        SharedBlockDeviceMetrics, SharedBlockDeviceMetricsRegistry,
    };
    use crate::serial::SerialOutputMetrics;

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

    fn block_metrics_with_all_fields() -> BlockDeviceMetrics {
        BlockDeviceMetrics::default()
            .with_event_fails(1)
            .with_execute_fails(2)
            .with_invalid_reqs_count(3)
            .with_flush_count(4)
            .with_queue_event_count(5)
            .with_read_bytes(6)
            .with_write_bytes(7)
            .with_read_count(8)
            .with_write_count(9)
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
    fn logger_metrics_include_log_and_metrics_miss_counts() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());

        state.record_missed_log();
        output.fail_next_write();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"logger":{"missed_log_count":1,"missed_metrics_count":1},"vmm":{"metrics_flush_count":1}}"#
            ]
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
    fn writes_paused_boot_run_loop_diagnostics_when_provided() {
        let path = unique_metrics_path("paused-diagnostics");
        let mut state = MetricsState::default();
        let diagnostics =
            MetricsDiagnostics::new().with_boot_run_loop_status(BootRunLoopMetricStatus::Paused);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"vmm\":{\"boot_run_loop_status\":\"paused\",\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_serial_output_diagnostics_when_dropped_bytes_are_nonzero() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_serial_output_metrics(SerialOutputMetrics::new(3));

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [r#"{"uart":{"rate_limiter_dropped_bytes":3},"vmm":{"metrics_flush_count":1}}"#]
        );
    }

    #[test]
    fn omits_serial_output_diagnostics_when_dropped_bytes_are_zero() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_serial_output_metrics(SerialOutputMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(output.lines(), [r#"{"vmm":{"metrics_flush_count":1}}"#]);
    }

    #[test]
    fn writes_block_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_block_device_metrics(block_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"block":{"event_fails":1,"execute_fails":2,"flush_count":4,"invalid_reqs_count":3,"queue_event_count":5,"read_bytes":6,"read_count":8,"write_bytes":7,"write_count":9},"vmm":{"metrics_flush_count":1}}"#
            ]
        );
    }

    #[test]
    fn omits_empty_block_device_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_block_device_metrics(BlockDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(output.lines(), [r#"{"vmm":{"metrics_flush_count":1}}"#]);
    }

    #[test]
    fn writes_block_device_metrics_by_drive_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let rootfs_metrics = BlockDeviceMetrics::default()
            .with_queue_event_count(1)
            .with_read_bytes(512)
            .with_read_count(1);
        let data_metrics = BlockDeviceMetrics::default()
            .with_queue_event_count(1)
            .with_write_bytes(256)
            .with_write_count(1);
        let diagnostics = MetricsDiagnostics::new()
            .with_block_device_metrics(rootfs_metrics.merged_with(data_metrics))
            .with_block_device_metrics_by_drive(
                BlockDeviceMetricsByDrive::new()
                    .with_drive_metrics("rootfs", rootfs_metrics)
                    .with_drive_metrics("noop", BlockDeviceMetrics::default())
                    .with_drive_metrics("data", data_metrics),
            );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"block":{"event_fails":0,"execute_fails":0,"flush_count":0,"invalid_reqs_count":0,"queue_event_count":2,"read_bytes":512,"read_count":1,"write_bytes":256,"write_count":1},"block_data":{"event_fails":0,"execute_fails":0,"flush_count":0,"invalid_reqs_count":0,"queue_event_count":1,"read_bytes":0,"read_count":0,"write_bytes":256,"write_count":1},"block_rootfs":{"event_fails":0,"execute_fails":0,"flush_count":0,"invalid_reqs_count":0,"queue_event_count":1,"read_bytes":512,"read_count":1,"write_bytes":0,"write_count":0},"vmm":{"metrics_flush_count":1}}"#
            ]
        );
    }

    #[test]
    fn shared_block_device_metrics_snapshot_is_per_instance() {
        let first = SharedBlockDeviceMetrics::default();
        let second = SharedBlockDeviceMetrics::default();

        first.record_queue_events(2);
        first.record_event_failure();

        assert_eq!(
            first.snapshot(),
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(2)
        );
        assert_eq!(second.snapshot(), BlockDeviceMetrics::default());
    }

    #[test]
    fn shared_block_device_metrics_registry_snapshot_is_per_instance() {
        let first = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs", "data"]);
        let second = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        first.record_queue_events_for_drive("rootfs", 2);
        first.record_event_failure_for_drive("rootfs");

        assert_eq!(
            first.aggregate_snapshot(),
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(2)
        );
        assert_eq!(
            first.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(2),
            )
        );
        assert_eq!(second.aggregate_snapshot(), BlockDeviceMetrics::default());
        assert!(second.per_drive_snapshot().is_empty());
    }

    #[test]
    fn block_metric_increment_saturates() {
        let metrics = SharedBlockDeviceMetrics::default();
        metrics
            .inner
            .queue_event_count
            .store(u64::MAX - 1, Ordering::Relaxed);

        metrics.record_queue_events(3);

        assert_eq!(metrics.snapshot().queue_event_count(), u64::MAX);
    }

    #[test]
    fn block_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_block_device_metrics(
            BlockDeviceMetrics::default()
                .with_event_fails(u64::MAX - 1)
                .with_execute_fails(u64::MAX - 2)
                .with_invalid_reqs_count(u64::MAX - 3)
                .with_flush_count(u64::MAX - 4)
                .with_queue_event_count(u64::MAX - 5)
                .with_read_bytes(u64::MAX - 6)
                .with_write_bytes(u64::MAX - 7)
                .with_read_count(u64::MAX - 8)
                .with_write_count(u64::MAX - 9),
        );
        let additional =
            MetricsDiagnostics::new().with_block_device_metrics(block_metrics_with_all_fields());

        assert_eq!(
            base.merged_with(additional).block_device_metrics(),
            Some(
                BlockDeviceMetrics::default()
                    .with_event_fails(u64::MAX)
                    .with_execute_fails(u64::MAX)
                    .with_invalid_reqs_count(u64::MAX)
                    .with_flush_count(u64::MAX)
                    .with_queue_event_count(u64::MAX)
                    .with_read_bytes(u64::MAX)
                    .with_write_bytes(u64::MAX)
                    .with_read_count(u64::MAX)
                    .with_write_count(u64::MAX)
            )
        );
    }

    #[test]
    fn block_diagnostics_merge_per_drive_metrics_saturates() {
        let base = MetricsDiagnostics::new().with_block_device_metrics_by_drive(
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(u64::MAX - 1)
                    .with_read_count(u64::MAX - 2),
            ),
        );
        let additional = MetricsDiagnostics::new().with_block_device_metrics_by_drive(
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics("rootfs", block_metrics_with_all_fields())
                .with_drive_metrics("data", BlockDeviceMetrics::default().with_write_count(3)),
        );
        let expected = BlockDeviceMetricsByDrive::new()
            .with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(u64::MAX)
                    .with_execute_fails(2)
                    .with_invalid_reqs_count(3)
                    .with_flush_count(4)
                    .with_queue_event_count(5)
                    .with_read_bytes(6)
                    .with_write_bytes(7)
                    .with_read_count(u64::MAX)
                    .with_write_count(9),
            )
            .with_drive_metrics("data", BlockDeviceMetrics::default().with_write_count(3));
        let merged = base.merged_with(additional);

        assert_eq!(merged.block_device_metrics_by_drive(), Some(&expected));
    }

    #[test]
    fn writes_balloon_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new()
            .with_balloon_device_metrics(BalloonDeviceMetrics::new(1, 2, 3, 4, 5, 6));

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"balloon":{"activate_fails":1,"deflate_count":5,"event_fails":6,"inflate_count":2,"stats_update_fails":4,"stats_updates_count":3},"vmm":{"metrics_flush_count":1}}"#
            ]
        );
    }

    #[test]
    fn omits_empty_balloon_device_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_balloon_device_metrics(BalloonDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(output.lines(), [r#"{"vmm":{"metrics_flush_count":1}}"#]);
    }

    #[test]
    fn shared_balloon_device_metrics_snapshot_is_per_instance() {
        let first = SharedBalloonDeviceMetrics::default();
        let second = SharedBalloonDeviceMetrics::default();

        first.record_activation_failure();
        first.record_statistics_update_failure();
        first.record_event_failure();

        assert_eq!(
            first.snapshot(),
            BalloonDeviceMetrics::new(1, 0, 0, 1, 0, 1)
        );
        assert_eq!(second.snapshot(), BalloonDeviceMetrics::default());
    }

    #[test]
    fn balloon_metric_increment_saturates() {
        let metric = AtomicU64::new(u64::MAX - 1);

        super::record_atomic_metric(&metric, 5);

        assert_eq!(metric.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn balloon_diagnostics_merge_saturates() {
        let base =
            MetricsDiagnostics::new().with_balloon_device_metrics(BalloonDeviceMetrics::new(
                u64::MAX,
                u64::MAX - 1,
                u64::MAX - 2,
                u64::MAX - 3,
                u64::MAX - 4,
                u64::MAX - 5,
            ));
        let additional = MetricsDiagnostics::new()
            .with_balloon_device_metrics(BalloonDeviceMetrics::new(1, 2, 3, 4, 5, 6));

        assert_eq!(
            base.merged_with(additional).balloon_device_metrics(),
            Some(BalloonDeviceMetrics::new(
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
            ))
        );
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
            "{\"api_server\":{\"process_startup_time_cpu_us\":5000,\"process_startup_time_us\":1000},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn omits_api_server_cpu_time_when_only_parent_cpu_time_is_provided() {
        let path = unique_metrics_path("startup-parent-only");
        let mut state = MetricsState::default();
        let diagnostics = MetricsDiagnostics::new().with_parent_cpu_time_us(3000);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(output, "{\"vmm\":{\"metrics_flush_count\":1}}\n");

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_api_server_cpu_time_without_parent_cpu_time() {
        let path = unique_metrics_path("startup-cpu-only");
        let mut state = MetricsState::default();
        let diagnostics = MetricsDiagnostics::new().with_start_time_cpu_us(2000);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"api_server\":{\"process_startup_time_cpu_us\":2000},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_zero_startup_time_diagnostics_when_provided() {
        let path = unique_metrics_path("startup-zero");
        let mut state = MetricsState::default();
        let diagnostics = MetricsDiagnostics::new()
            .with_start_time_us(0)
            .with_start_time_cpu_us(0)
            .with_parent_cpu_time_us(0);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"api_server\":{\"process_startup_time_cpu_us\":0,\"process_startup_time_us\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn startup_cpu_time_diagnostics_saturate_when_parent_time_overflows() {
        let path = unique_metrics_path("startup-time-saturates");
        let mut state = MetricsState::default();
        let diagnostics = MetricsDiagnostics::new()
            .with_start_time_cpu_us(u64::MAX)
            .with_parent_cpu_time_us(1);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"api_server\":{\"process_startup_time_cpu_us\":18446744073709551615},\"vmm\":{\"metrics_flush_count\":1}}\n"
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
            "{\"put_api_requests\":{\"actions_count\":2,\"actions_fails\":1,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_patch_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-patch");
        let mut state = MetricsState::default();

        state.record_patch_drive_request();
        state.record_patch_drive_failure();
        state.record_patch_network_request();
        state.record_patch_network_failure();
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
            "{\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":1,\"drive_fails\":1,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"mmds_count\":1,\"mmds_fails\":1,\"network_count\":1,\"network_fails\":1,\"pmem_count\":0,\"pmem_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
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
            "{\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":2,\"boot_source_fails\":1,\"cpu_cfg_count\":1,\"cpu_cfg_fails\":1,\"drive_count\":1,\"drive_fails\":1,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":2,\"machine_cfg_fails\":1,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":1,\"network_fails\":1,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":1,\"vsock_fails\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
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
            "{\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":2,\"mmds_fails\":1,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_pmem_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-pmem");
        let mut state = MetricsState::default();

        state.record_put_pmem_request();
        state.record_put_pmem_request();
        state.record_put_pmem_failure();
        state.record_patch_pmem_request();
        state.record_patch_pmem_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":1,\"pmem_fails\":1},\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":2,\"pmem_fails\":1,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_memory_hotplug_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-memory-hotplug");
        let mut state = MetricsState::default();

        state.record_get_hotplug_memory_request();
        state.record_put_hotplug_memory_request();
        state.record_put_hotplug_memory_request();
        state.record_put_hotplug_memory_failure();
        state.record_patch_hotplug_memory_request();
        state.record_patch_hotplug_memory_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"get_api_requests\":{\"balloon_count\":0,\"hotplug_memory_count\":1,\"instance_info_count\":0,\"machine_cfg_count\":0,\"mmds_count\":0,\"vmm_version_count\":0},\"patch_api_requests\":{\"balloon_count\":0,\"balloon_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":1,\"hotplug_memory_fails\":1,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":2,\"hotplug_memory_fails\":1,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_balloon_api_request_metrics_when_recorded() {
        let path = unique_metrics_path("api-request-balloon");
        let mut state = MetricsState::default();

        state.record_get_balloon_request();
        state.record_get_balloon_request();
        state.record_put_balloon_request();
        state.record_put_balloon_request();
        state.record_put_balloon_failure();
        state.record_patch_balloon_request();
        state.record_patch_balloon_request();
        state.record_patch_balloon_failure();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"get_api_requests\":{\"balloon_count\":2,\"hotplug_memory_count\":0,\"instance_info_count\":0,\"machine_cfg_count\":0,\"mmds_count\":0,\"vmm_version_count\":0},\"patch_api_requests\":{\"balloon_count\":2,\"balloon_fails\":1,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0},\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"balloon_count\":2,\"balloon_fails\":1,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":0,\"logger_fails\":0,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":0,\"metrics_fails\":0,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":0,\"serial_fails\":0,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_deprecated_api_metrics_when_recorded() {
        let path = unique_metrics_path("deprecated-api");
        let mut state = MetricsState::default();

        state.record_deprecated_api_call();
        state.record_deprecated_api_call();
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let output = fs::read_to_string(&path).expect("metrics output should be readable");
        assert_eq!(
            output,
            "{\"deprecated_api\":{\"deprecated_http_api_calls\":2},\"vmm\":{\"metrics_flush_count\":1}}\n"
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
            "{\"put_api_requests\":{\"actions_count\":0,\"actions_fails\":0,\"balloon_count\":0,\"balloon_fails\":0,\"boot_source_count\":0,\"boot_source_fails\":0,\"cpu_cfg_count\":0,\"cpu_cfg_fails\":0,\"drive_count\":0,\"drive_fails\":0,\"hotplug_memory_count\":0,\"hotplug_memory_fails\":0,\"logger_count\":1,\"logger_fails\":1,\"machine_cfg_count\":0,\"machine_cfg_fails\":0,\"metrics_count\":2,\"metrics_fails\":1,\"mmds_count\":0,\"mmds_fails\":0,\"network_count\":0,\"network_fails\":0,\"pmem_count\":0,\"pmem_fails\":0,\"serial_count\":1,\"serial_fails\":1,\"vsock_count\":0,\"vsock_fails\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
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
            "{\"get_api_requests\":{\"balloon_count\":0,\"hotplug_memory_count\":0,\"instance_info_count\":1,\"machine_cfg_count\":1,\"mmds_count\":2,\"vmm_version_count\":1},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn merges_independent_diagnostics() {
        let base = MetricsDiagnostics::new()
            .with_balloon_device_metrics(BalloonDeviceMetrics::new(1, 2, 3, 4, 5, 6))
            .with_start_time_us(1000)
            .with_start_time_cpu_us(2000);
        let session = MetricsDiagnostics::new()
            .with_balloon_device_metrics(BalloonDeviceMetrics::new(10, 20, 30, 40, 50, 60))
            .with_boot_run_loop_status(BootRunLoopMetricStatus::Running)
            .with_parent_cpu_time_us(3000);

        let diagnostics = base.merged_with(session);

        assert_eq!(
            diagnostics.balloon_device_metrics(),
            Some(BalloonDeviceMetrics::new(11, 22, 33, 44, 55, 66))
        );
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
