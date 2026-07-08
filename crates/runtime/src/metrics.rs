use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::balloon::VirtioBalloonDeviceNotificationDispatch;
use crate::block::{
    VirtioBlockDeviceNotificationDispatch, VirtioBlockLatencyAggregate, VirtioBlockQueueDispatch,
};
use crate::entropy::{
    VirtioRngDeviceNotificationDispatch, VirtioRngDeviceNotificationError, VirtioRngQueueDispatch,
};
use crate::network::{
    VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX, VirtioNetworkDeviceNotificationDispatch,
    VirtioNetworkDeviceNotificationError, VirtioNetworkRxQueueDispatch,
    VirtioNetworkTxQueueDispatch,
};
use crate::serial::SerialOutputMetrics;
use crate::vsock::{
    VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX,
    VirtioVsockDeviceNotificationDispatch, VirtioVsockDeviceNotificationError,
    VirtioVsockRxQueueDispatch, VirtioVsockTxQueueDispatch,
};

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
    rate_limiter_event_count: u64,
    rate_limiter_throttled_events: u64,
    update_count: u64,
    update_fails: u64,
    read_bytes: u64,
    write_bytes: u64,
    read_count: u64,
    write_count: u64,
    read_agg: VirtioBlockLatencyAggregate,
    write_agg: VirtioBlockLatencyAggregate,
}

impl BlockDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.event_fails == 0
            && self.execute_fails == 0
            && self.invalid_reqs_count == 0
            && self.flush_count == 0
            && self.queue_event_count == 0
            && self.rate_limiter_event_count == 0
            && self.rate_limiter_throttled_events == 0
            && self.update_count == 0
            && self.update_fails == 0
            && self.read_bytes == 0
            && self.write_bytes == 0
            && self.read_count == 0
            && self.write_count == 0
            && self.read_agg.is_empty()
            && self.write_agg.is_empty()
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

    pub const fn rate_limiter_event_count(self) -> u64 {
        self.rate_limiter_event_count
    }

    pub const fn rate_limiter_throttled_events(self) -> u64 {
        self.rate_limiter_throttled_events
    }

    pub const fn update_count(self) -> u64 {
        self.update_count
    }

    pub const fn update_fails(self) -> u64 {
        self.update_fails
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

    pub const fn read_agg(self) -> VirtioBlockLatencyAggregate {
        self.read_agg
    }

    pub const fn write_agg(self) -> VirtioBlockLatencyAggregate {
        self.write_agg
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

    pub const fn with_rate_limiter_event_count(mut self, rate_limiter_event_count: u64) -> Self {
        self.rate_limiter_event_count = rate_limiter_event_count;
        self
    }

    pub const fn with_rate_limiter_throttled_events(
        mut self,
        rate_limiter_throttled_events: u64,
    ) -> Self {
        self.rate_limiter_throttled_events = rate_limiter_throttled_events;
        self
    }

    pub const fn with_update_count(mut self, update_count: u64) -> Self {
        self.update_count = update_count;
        self
    }

    pub const fn with_update_fails(mut self, update_fails: u64) -> Self {
        self.update_fails = update_fails;
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

    pub const fn with_read_agg(mut self, read_agg: VirtioBlockLatencyAggregate) -> Self {
        self.read_agg = read_agg;
        self
    }

    pub const fn with_write_agg(mut self, write_agg: VirtioBlockLatencyAggregate) -> Self {
        self.write_agg = write_agg;
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
            rate_limiter_event_count: self
                .rate_limiter_event_count
                .saturating_add(other.rate_limiter_event_count),
            rate_limiter_throttled_events: self
                .rate_limiter_throttled_events
                .saturating_add(other.rate_limiter_throttled_events),
            update_count: self.update_count.saturating_add(other.update_count),
            update_fails: self.update_fails.saturating_add(other.update_fails),
            read_bytes: self.read_bytes.saturating_add(other.read_bytes),
            write_bytes: self.write_bytes.saturating_add(other.write_bytes),
            read_count: self.read_count.saturating_add(other.read_count),
            write_count: self.write_count.saturating_add(other.write_count),
            read_agg: self.read_agg.merged_with(other.read_agg),
            write_agg: self.write_agg.merged_with(other.write_agg),
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
        if let Some(read_agg) = dispatch.read_latency_aggregate() {
            self.record_read_latency_aggregate(read_agg);
        }
        if let Some(write_agg) = dispatch.write_latency_aggregate() {
            self.record_write_latency_aggregate(write_agg);
        }
        self.record_flushes(usize_to_u64_saturating(dispatch.flush_count()));
        self.record_rate_limiter_throttled_events(usize_to_u64_saturating(
            dispatch.rate_limiter_throttled_requests(),
        ));
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

    pub fn record_update(&self) {
        record_atomic_metric(&self.inner.update_count, 1);
    }

    pub fn record_update_failure(&self) {
        record_atomic_metric(&self.inner.update_fails, 1);
    }

    pub fn snapshot(&self) -> BlockDeviceMetrics {
        BlockDeviceMetrics {
            event_fails: self.inner.event_fails.load(Ordering::Relaxed),
            execute_fails: self.inner.execute_fails.load(Ordering::Relaxed),
            invalid_reqs_count: self.inner.invalid_reqs_count.load(Ordering::Relaxed),
            flush_count: self.inner.flush_count.load(Ordering::Relaxed),
            queue_event_count: self.inner.queue_event_count.load(Ordering::Relaxed),
            rate_limiter_event_count: self.inner.rate_limiter_event_count.load(Ordering::Relaxed),
            rate_limiter_throttled_events: self
                .inner
                .rate_limiter_throttled_events
                .load(Ordering::Relaxed),
            update_count: self.inner.update_count.load(Ordering::Relaxed),
            update_fails: self.inner.update_fails.load(Ordering::Relaxed),
            read_bytes: self.inner.read_bytes.load(Ordering::Relaxed),
            write_bytes: self.inner.write_bytes.load(Ordering::Relaxed),
            read_count: self.inner.read_count.load(Ordering::Relaxed),
            write_count: self.inner.write_count.load(Ordering::Relaxed),
            read_agg: self.read_latency_aggregate_snapshot(),
            write_agg: self.write_latency_aggregate_snapshot(),
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

    fn record_read_latency_aggregate(&self, latency_aggregate: VirtioBlockLatencyAggregate) {
        record_latency_aggregate(
            latency_aggregate,
            &self.inner.read_agg_min_us,
            &self.inner.read_agg_max_us,
            &self.inner.read_agg_sum_us,
            &self.inner.read_agg_sample_count,
        );
    }

    fn record_write_latency_aggregate(&self, latency_aggregate: VirtioBlockLatencyAggregate) {
        record_latency_aggregate(
            latency_aggregate,
            &self.inner.write_agg_min_us,
            &self.inner.write_agg_max_us,
            &self.inner.write_agg_sum_us,
            &self.inner.write_agg_sample_count,
        );
    }

    fn read_latency_aggregate_snapshot(&self) -> VirtioBlockLatencyAggregate {
        latency_aggregate_snapshot(
            &self.inner.read_agg_min_us,
            &self.inner.read_agg_max_us,
            &self.inner.read_agg_sum_us,
            &self.inner.read_agg_sample_count,
        )
    }

    fn write_latency_aggregate_snapshot(&self) -> VirtioBlockLatencyAggregate {
        latency_aggregate_snapshot(
            &self.inner.write_agg_min_us,
            &self.inner.write_agg_max_us,
            &self.inner.write_agg_sum_us,
            &self.inner.write_agg_sample_count,
        )
    }

    fn record_flushes(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.flush_count, count);
        }
    }

    fn record_rate_limiter_throttled_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rate_limiter_throttled_events, count);
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

#[derive(Debug)]
struct SharedBlockDeviceMetricsInner {
    event_fails: AtomicU64,
    execute_fails: AtomicU64,
    invalid_reqs_count: AtomicU64,
    flush_count: AtomicU64,
    queue_event_count: AtomicU64,
    rate_limiter_event_count: AtomicU64,
    rate_limiter_throttled_events: AtomicU64,
    update_count: AtomicU64,
    update_fails: AtomicU64,
    read_bytes: AtomicU64,
    write_bytes: AtomicU64,
    read_count: AtomicU64,
    write_count: AtomicU64,
    read_agg_min_us: AtomicU64,
    read_agg_max_us: AtomicU64,
    read_agg_sum_us: AtomicU64,
    read_agg_sample_count: AtomicU64,
    write_agg_min_us: AtomicU64,
    write_agg_max_us: AtomicU64,
    write_agg_sum_us: AtomicU64,
    write_agg_sample_count: AtomicU64,
}

impl Default for SharedBlockDeviceMetricsInner {
    fn default() -> Self {
        Self {
            event_fails: AtomicU64::new(0),
            execute_fails: AtomicU64::new(0),
            invalid_reqs_count: AtomicU64::new(0),
            flush_count: AtomicU64::new(0),
            queue_event_count: AtomicU64::new(0),
            rate_limiter_event_count: AtomicU64::new(0),
            rate_limiter_throttled_events: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            update_fails: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            write_bytes: AtomicU64::new(0),
            read_count: AtomicU64::new(0),
            write_count: AtomicU64::new(0),
            read_agg_min_us: AtomicU64::new(u64::MAX),
            read_agg_max_us: AtomicU64::new(0),
            read_agg_sum_us: AtomicU64::new(0),
            read_agg_sample_count: AtomicU64::new(0),
            write_agg_min_us: AtomicU64::new(u64::MAX),
            write_agg_max_us: AtomicU64::new(0),
            write_agg_sum_us: AtomicU64::new(0),
            write_agg_sample_count: AtomicU64::new(0),
        }
    }
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

    pub fn record_update_for_drive(&self, drive_id: &str) {
        self.aggregate.record_update();
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_update();
        }
    }

    pub fn record_update_failure_for_drive(&self, drive_id: &str) {
        self.aggregate.record_update_failure();
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_update_failure();
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
pub struct NetworkInterfaceMetrics {
    event_fails: u64,
    rx_queue_event_count: u64,
    rx_bytes_count: u64,
    rx_packets_count: u64,
    rx_fails: u64,
    rx_count: u64,
    tx_bytes_count: u64,
    tx_malformed_frames: u64,
    tx_fails: u64,
    tx_count: u64,
    tx_packets_count: u64,
    tx_queue_event_count: u64,
}

impl NetworkInterfaceMetrics {
    pub const fn is_empty(self) -> bool {
        self.event_fails == 0
            && self.rx_queue_event_count == 0
            && self.rx_bytes_count == 0
            && self.rx_packets_count == 0
            && self.rx_fails == 0
            && self.rx_count == 0
            && self.tx_bytes_count == 0
            && self.tx_malformed_frames == 0
            && self.tx_fails == 0
            && self.tx_count == 0
            && self.tx_packets_count == 0
            && self.tx_queue_event_count == 0
    }

    pub const fn event_fails(self) -> u64 {
        self.event_fails
    }

    pub const fn rx_queue_event_count(self) -> u64 {
        self.rx_queue_event_count
    }

    pub const fn rx_bytes_count(self) -> u64 {
        self.rx_bytes_count
    }

    pub const fn rx_packets_count(self) -> u64 {
        self.rx_packets_count
    }

    pub const fn rx_fails(self) -> u64 {
        self.rx_fails
    }

    pub const fn rx_count(self) -> u64 {
        self.rx_count
    }

    pub const fn tx_bytes_count(self) -> u64 {
        self.tx_bytes_count
    }

    pub const fn tx_malformed_frames(self) -> u64 {
        self.tx_malformed_frames
    }

    pub const fn tx_fails(self) -> u64 {
        self.tx_fails
    }

    pub const fn tx_count(self) -> u64 {
        self.tx_count
    }

    pub const fn tx_packets_count(self) -> u64 {
        self.tx_packets_count
    }

    pub const fn tx_queue_event_count(self) -> u64 {
        self.tx_queue_event_count
    }

    pub const fn with_event_fails(mut self, event_fails: u64) -> Self {
        self.event_fails = event_fails;
        self
    }

    pub const fn with_rx_queue_event_count(mut self, rx_queue_event_count: u64) -> Self {
        self.rx_queue_event_count = rx_queue_event_count;
        self
    }

    pub const fn with_rx_bytes_count(mut self, rx_bytes_count: u64) -> Self {
        self.rx_bytes_count = rx_bytes_count;
        self
    }

    pub const fn with_rx_packets_count(mut self, rx_packets_count: u64) -> Self {
        self.rx_packets_count = rx_packets_count;
        self
    }

    pub const fn with_rx_fails(mut self, rx_fails: u64) -> Self {
        self.rx_fails = rx_fails;
        self
    }

    pub const fn with_rx_count(mut self, rx_count: u64) -> Self {
        self.rx_count = rx_count;
        self
    }

    pub const fn with_tx_bytes_count(mut self, tx_bytes_count: u64) -> Self {
        self.tx_bytes_count = tx_bytes_count;
        self
    }

    pub const fn with_tx_malformed_frames(mut self, tx_malformed_frames: u64) -> Self {
        self.tx_malformed_frames = tx_malformed_frames;
        self
    }

    pub const fn with_tx_fails(mut self, tx_fails: u64) -> Self {
        self.tx_fails = tx_fails;
        self
    }

    pub const fn with_tx_count(mut self, tx_count: u64) -> Self {
        self.tx_count = tx_count;
        self
    }

    pub const fn with_tx_packets_count(mut self, tx_packets_count: u64) -> Self {
        self.tx_packets_count = tx_packets_count;
        self
    }

    pub const fn with_tx_queue_event_count(mut self, tx_queue_event_count: u64) -> Self {
        self.tx_queue_event_count = tx_queue_event_count;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            event_fails: self.event_fails.saturating_add(other.event_fails),
            rx_queue_event_count: self
                .rx_queue_event_count
                .saturating_add(other.rx_queue_event_count),
            rx_bytes_count: self.rx_bytes_count.saturating_add(other.rx_bytes_count),
            rx_packets_count: self.rx_packets_count.saturating_add(other.rx_packets_count),
            rx_fails: self.rx_fails.saturating_add(other.rx_fails),
            rx_count: self.rx_count.saturating_add(other.rx_count),
            tx_bytes_count: self.tx_bytes_count.saturating_add(other.tx_bytes_count),
            tx_malformed_frames: self
                .tx_malformed_frames
                .saturating_add(other.tx_malformed_frames),
            tx_fails: self.tx_fails.saturating_add(other.tx_fails),
            tx_count: self.tx_count.saturating_add(other.tx_count),
            tx_packets_count: self.tx_packets_count.saturating_add(other.tx_packets_count),
            tx_queue_event_count: self
                .tx_queue_event_count
                .saturating_add(other.tx_queue_event_count),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkInterfaceMetricsByInterface {
    metrics: BTreeMap<String, NetworkInterfaceMetrics>,
}

impl NetworkInterfaceMetricsByInterface {
    pub fn new() -> Self {
        Self {
            metrics: BTreeMap::new(),
        }
    }

    pub fn with_interface_metrics(
        mut self,
        iface_id: impl Into<String>,
        metrics: NetworkInterfaceMetrics,
    ) -> Self {
        self.insert_interface_metrics(iface_id, metrics);
        self
    }

    pub fn insert_interface_metrics(
        &mut self,
        iface_id: impl Into<String>,
        metrics: NetworkInterfaceMetrics,
    ) {
        self.metrics
            .entry(iface_id.into())
            .and_modify(|existing| *existing = existing.merged_with(metrics))
            .or_insert(metrics);
    }

    pub fn is_empty(&self) -> bool {
        self.metrics
            .values()
            .all(|metrics| NetworkInterfaceMetrics::is_empty(*metrics))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, NetworkInterfaceMetrics)> {
        self.metrics
            .iter()
            .map(|(iface_id, metrics)| (iface_id.as_str(), *metrics))
    }

    fn merged_with(mut self, other: Self) -> Self {
        for (iface_id, metrics) in other.metrics {
            self.insert_interface_metrics(iface_id, metrics);
        }
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedNetworkInterfaceMetrics {
    inner: Arc<SharedNetworkInterfaceMetricsInner>,
}

impl SharedNetworkInterfaceMetrics {
    pub fn record_notification_dispatch(&self, dispatch: &VirtioNetworkDeviceNotificationDispatch) {
        let rx_queue_events = dispatch
            .drained_notifications()
            .iter()
            .copied()
            .filter(|queue_index| *queue_index == VIRTIO_NET_RX_QUEUE_INDEX)
            .count();
        let tx_queue_events = dispatch
            .drained_notifications()
            .iter()
            .copied()
            .filter(|queue_index| *queue_index == VIRTIO_NET_TX_QUEUE_INDEX)
            .count();
        self.record_rx_queue_events(usize_to_u64_saturating(rx_queue_events));
        self.record_tx_queue_events(usize_to_u64_saturating(tx_queue_events));
        if let Some(dispatch) = dispatch.rx_queue_dispatch() {
            self.record_rx_queue_dispatch(dispatch);
        }
        if let Some(dispatch) = dispatch.tx_queue_dispatch() {
            self.record_tx_queue_dispatch(dispatch);
        }
        if let Some(dispatch) = dispatch.post_tx_rx_queue_dispatch() {
            self.record_rx_queue_dispatch(dispatch);
        }
    }

    pub fn record_rx_queue_dispatch(&self, dispatch: &VirtioNetworkRxQueueDispatch) {
        let delivered_packets = usize_to_u64_saturating(dispatch.delivered_packets());
        self.record_rx_packets(
            delivered_packets,
            dispatch.deliveries().iter().fold(0, |sum, delivery| {
                sum.saturating_add(u64::from(delivery.bytes_written_to_guest()))
            }),
        );
        self.record_rx_failures(usize_to_u64_saturating(
            dispatch
                .buffer_parse_failures()
                .saturating_add(dispatch.buffer_too_small_failures())
                .saturating_add(dispatch.source_failures()),
        ));
    }

    pub fn record_tx_queue_dispatch(&self, dispatch: &VirtioNetworkTxQueueDispatch) {
        let successful_frames = usize_to_u64_saturating(dispatch.sink_successful_frames());
        self.record_tx_packets(successful_frames, dispatch.sink_successful_bytes());
        self.record_tx_malformed_frames(usize_to_u64_saturating(dispatch.parse_failures()));
        self.record_tx_failures(usize_to_u64_saturating(dispatch.sink_failures()));
    }

    pub fn record_event_failure(&self) {
        record_atomic_metric(&self.inner.event_fails, 1);
    }

    pub fn record_rx_queue_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rx_queue_event_count, count);
        }
    }

    pub fn record_tx_queue_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_queue_event_count, count);
        }
    }

    pub fn snapshot(&self) -> NetworkInterfaceMetrics {
        NetworkInterfaceMetrics {
            event_fails: self.inner.event_fails.load(Ordering::Relaxed),
            rx_queue_event_count: self.inner.rx_queue_event_count.load(Ordering::Relaxed),
            rx_bytes_count: self.inner.rx_bytes_count.load(Ordering::Relaxed),
            rx_packets_count: self.inner.rx_packets_count.load(Ordering::Relaxed),
            rx_fails: self.inner.rx_fails.load(Ordering::Relaxed),
            rx_count: self.inner.rx_count.load(Ordering::Relaxed),
            tx_bytes_count: self.inner.tx_bytes_count.load(Ordering::Relaxed),
            tx_malformed_frames: self.inner.tx_malformed_frames.load(Ordering::Relaxed),
            tx_fails: self.inner.tx_fails.load(Ordering::Relaxed),
            tx_count: self.inner.tx_count.load(Ordering::Relaxed),
            tx_packets_count: self.inner.tx_packets_count.load(Ordering::Relaxed),
            tx_queue_event_count: self.inner.tx_queue_event_count.load(Ordering::Relaxed),
        }
    }

    fn record_rx_packets(&self, count: u64, bytes: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rx_count, count);
            record_atomic_metric(&self.inner.rx_packets_count, count);
        }
        if bytes != 0 {
            record_atomic_metric(&self.inner.rx_bytes_count, bytes);
        }
    }

    fn record_rx_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rx_fails, count);
        }
    }

    fn record_tx_packets(&self, count: u64, bytes: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_count, count);
            record_atomic_metric(&self.inner.tx_packets_count, count);
        }
        if bytes != 0 {
            record_atomic_metric(&self.inner.tx_bytes_count, bytes);
        }
    }

    fn record_tx_malformed_frames(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_malformed_frames, count);
        }
    }

    fn record_tx_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_fails, count);
        }
    }
}

#[derive(Debug, Default)]
struct SharedNetworkInterfaceMetricsInner {
    event_fails: AtomicU64,
    rx_queue_event_count: AtomicU64,
    rx_bytes_count: AtomicU64,
    rx_packets_count: AtomicU64,
    rx_fails: AtomicU64,
    rx_count: AtomicU64,
    tx_bytes_count: AtomicU64,
    tx_malformed_frames: AtomicU64,
    tx_fails: AtomicU64,
    tx_count: AtomicU64,
    tx_packets_count: AtomicU64,
    tx_queue_event_count: AtomicU64,
}

#[derive(Debug, Clone, Default)]
pub struct SharedNetworkInterfaceMetricsRegistry {
    aggregate: SharedNetworkInterfaceMetrics,
    per_interface: Arc<BTreeMap<String, SharedNetworkInterfaceMetrics>>,
}

impl SharedNetworkInterfaceMetricsRegistry {
    pub fn from_interface_ids<'a>(iface_ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut per_interface = BTreeMap::new();
        for iface_id in iface_ids {
            per_interface
                .entry(iface_id.to_string())
                .or_insert_with(SharedNetworkInterfaceMetrics::default);
        }

        Self {
            aggregate: SharedNetworkInterfaceMetrics::default(),
            per_interface: Arc::new(per_interface),
        }
    }

    pub fn aggregate(&self) -> SharedNetworkInterfaceMetrics {
        self.aggregate.clone()
    }

    pub fn per_interface(&self, iface_id: &str) -> Option<SharedNetworkInterfaceMetrics> {
        self.per_interface.get(iface_id).cloned()
    }

    pub fn record_notification_dispatch_for_interface(
        &self,
        iface_id: &str,
        dispatch: &VirtioNetworkDeviceNotificationDispatch,
    ) {
        self.aggregate.record_notification_dispatch(dispatch);
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_notification_dispatch(dispatch);
        }
    }

    pub fn record_notification_error_for_interface(
        &self,
        iface_id: &str,
        source: &VirtioNetworkDeviceNotificationError,
    ) {
        let rx_queue_events = source
            .drained_notifications()
            .iter()
            .copied()
            .filter(|queue_index| *queue_index == VIRTIO_NET_RX_QUEUE_INDEX)
            .count();
        let tx_queue_events = source
            .drained_notifications()
            .iter()
            .copied()
            .filter(|queue_index| *queue_index == VIRTIO_NET_TX_QUEUE_INDEX)
            .count();
        self.record_queue_events_for_interface(
            iface_id,
            usize_to_u64_saturating(rx_queue_events),
            usize_to_u64_saturating(tx_queue_events),
        );
        self.record_event_failure_for_interface(iface_id);
        if let Some(dispatch) = source.completed_initial_rx_dispatch() {
            self.record_rx_queue_dispatch_for_interface(iface_id, dispatch);
        }
        if let Some(dispatch) = source.completed_tx_dispatch() {
            self.record_tx_queue_dispatch_for_interface(iface_id, dispatch);
        }
        if let Some(dispatch) = source.completed_rx_dispatch() {
            self.record_rx_queue_dispatch_for_interface(iface_id, dispatch);
        }
    }

    pub fn record_event_failure(&self) {
        self.aggregate.record_event_failure();
    }

    pub fn record_event_failure_for_interface(&self, iface_id: &str) {
        self.aggregate.record_event_failure();
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_event_failure();
        }
    }

    pub fn record_rx_queue_dispatch_for_interface(
        &self,
        iface_id: &str,
        dispatch: &VirtioNetworkRxQueueDispatch,
    ) {
        self.aggregate.record_rx_queue_dispatch(dispatch);
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_rx_queue_dispatch(dispatch);
        }
    }

    pub fn record_tx_queue_dispatch_for_interface(
        &self,
        iface_id: &str,
        dispatch: &VirtioNetworkTxQueueDispatch,
    ) {
        self.aggregate.record_tx_queue_dispatch(dispatch);
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_tx_queue_dispatch(dispatch);
        }
    }

    pub fn record_queue_events_for_interface(&self, iface_id: &str, rx_count: u64, tx_count: u64) {
        self.aggregate.record_rx_queue_events(rx_count);
        self.aggregate.record_tx_queue_events(tx_count);
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_rx_queue_events(rx_count);
            metrics.record_tx_queue_events(tx_count);
        }
    }

    pub fn aggregate_snapshot(&self) -> NetworkInterfaceMetrics {
        self.aggregate.snapshot()
    }

    pub fn per_interface_snapshot(&self) -> NetworkInterfaceMetricsByInterface {
        let mut snapshot = NetworkInterfaceMetricsByInterface::new();
        for (iface_id, metrics) in self.per_interface.iter() {
            let metrics = metrics.snapshot();
            if !metrics.is_empty() {
                snapshot.insert_interface_metrics(iface_id.clone(), metrics);
            }
        }
        snapshot
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VsockDeviceMetrics {
    activate_fails: u64,
    cfg_fails: u64,
    rx_queue_event_fails: u64,
    tx_queue_event_fails: u64,
    ev_queue_event_fails: u64,
    muxer_event_fails: u64,
    conn_event_fails: u64,
    rx_queue_event_count: u64,
    tx_queue_event_count: u64,
    rx_bytes_count: u64,
    tx_bytes_count: u64,
    rx_packets_count: u64,
    tx_packets_count: u64,
    conns_added: u64,
    conns_killed: u64,
    conns_removed: u64,
    killq_resync: u64,
    tx_flush_fails: u64,
    tx_write_fails: u64,
    rx_read_fails: u64,
}

impl VsockDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.cfg_fails == 0
            && self.rx_queue_event_fails == 0
            && self.tx_queue_event_fails == 0
            && self.ev_queue_event_fails == 0
            && self.muxer_event_fails == 0
            && self.conn_event_fails == 0
            && self.rx_queue_event_count == 0
            && self.tx_queue_event_count == 0
            && self.rx_bytes_count == 0
            && self.tx_bytes_count == 0
            && self.rx_packets_count == 0
            && self.tx_packets_count == 0
            && self.conns_added == 0
            && self.conns_killed == 0
            && self.conns_removed == 0
            && self.killq_resync == 0
            && self.tx_flush_fails == 0
            && self.tx_write_fails == 0
            && self.rx_read_fails == 0
    }

    pub const fn activate_fails(self) -> u64 {
        self.activate_fails
    }

    pub const fn cfg_fails(self) -> u64 {
        self.cfg_fails
    }

    pub const fn rx_queue_event_fails(self) -> u64 {
        self.rx_queue_event_fails
    }

    pub const fn tx_queue_event_fails(self) -> u64 {
        self.tx_queue_event_fails
    }

    pub const fn ev_queue_event_fails(self) -> u64 {
        self.ev_queue_event_fails
    }

    pub const fn muxer_event_fails(self) -> u64 {
        self.muxer_event_fails
    }

    pub const fn conn_event_fails(self) -> u64 {
        self.conn_event_fails
    }

    pub const fn rx_queue_event_count(self) -> u64 {
        self.rx_queue_event_count
    }

    pub const fn tx_queue_event_count(self) -> u64 {
        self.tx_queue_event_count
    }

    pub const fn rx_bytes_count(self) -> u64 {
        self.rx_bytes_count
    }

    pub const fn tx_bytes_count(self) -> u64 {
        self.tx_bytes_count
    }

    pub const fn rx_packets_count(self) -> u64 {
        self.rx_packets_count
    }

    pub const fn tx_packets_count(self) -> u64 {
        self.tx_packets_count
    }

    pub const fn conns_added(self) -> u64 {
        self.conns_added
    }

    pub const fn conns_killed(self) -> u64 {
        self.conns_killed
    }

    pub const fn conns_removed(self) -> u64 {
        self.conns_removed
    }

    pub const fn killq_resync(self) -> u64 {
        self.killq_resync
    }

    pub const fn tx_flush_fails(self) -> u64 {
        self.tx_flush_fails
    }

    pub const fn tx_write_fails(self) -> u64 {
        self.tx_write_fails
    }

    pub const fn rx_read_fails(self) -> u64 {
        self.rx_read_fails
    }

    pub const fn with_activate_fails(mut self, activate_fails: u64) -> Self {
        self.activate_fails = activate_fails;
        self
    }

    pub const fn with_cfg_fails(mut self, cfg_fails: u64) -> Self {
        self.cfg_fails = cfg_fails;
        self
    }

    pub const fn with_rx_queue_event_fails(mut self, rx_queue_event_fails: u64) -> Self {
        self.rx_queue_event_fails = rx_queue_event_fails;
        self
    }

    pub const fn with_tx_queue_event_fails(mut self, tx_queue_event_fails: u64) -> Self {
        self.tx_queue_event_fails = tx_queue_event_fails;
        self
    }

    pub const fn with_ev_queue_event_fails(mut self, ev_queue_event_fails: u64) -> Self {
        self.ev_queue_event_fails = ev_queue_event_fails;
        self
    }

    pub const fn with_muxer_event_fails(mut self, muxer_event_fails: u64) -> Self {
        self.muxer_event_fails = muxer_event_fails;
        self
    }

    pub const fn with_conn_event_fails(mut self, conn_event_fails: u64) -> Self {
        self.conn_event_fails = conn_event_fails;
        self
    }

    pub const fn with_rx_queue_event_count(mut self, rx_queue_event_count: u64) -> Self {
        self.rx_queue_event_count = rx_queue_event_count;
        self
    }

    pub const fn with_tx_queue_event_count(mut self, tx_queue_event_count: u64) -> Self {
        self.tx_queue_event_count = tx_queue_event_count;
        self
    }

    pub const fn with_rx_bytes_count(mut self, rx_bytes_count: u64) -> Self {
        self.rx_bytes_count = rx_bytes_count;
        self
    }

    pub const fn with_tx_bytes_count(mut self, tx_bytes_count: u64) -> Self {
        self.tx_bytes_count = tx_bytes_count;
        self
    }

    pub const fn with_rx_packets_count(mut self, rx_packets_count: u64) -> Self {
        self.rx_packets_count = rx_packets_count;
        self
    }

    pub const fn with_tx_packets_count(mut self, tx_packets_count: u64) -> Self {
        self.tx_packets_count = tx_packets_count;
        self
    }

    pub const fn with_conns_added(mut self, conns_added: u64) -> Self {
        self.conns_added = conns_added;
        self
    }

    pub const fn with_conns_killed(mut self, conns_killed: u64) -> Self {
        self.conns_killed = conns_killed;
        self
    }

    pub const fn with_conns_removed(mut self, conns_removed: u64) -> Self {
        self.conns_removed = conns_removed;
        self
    }

    pub const fn with_killq_resync(mut self, killq_resync: u64) -> Self {
        self.killq_resync = killq_resync;
        self
    }

    pub const fn with_tx_flush_fails(mut self, tx_flush_fails: u64) -> Self {
        self.tx_flush_fails = tx_flush_fails;
        self
    }

    pub const fn with_tx_write_fails(mut self, tx_write_fails: u64) -> Self {
        self.tx_write_fails = tx_write_fails;
        self
    }

    pub const fn with_rx_read_fails(mut self, rx_read_fails: u64) -> Self {
        self.rx_read_fails = rx_read_fails;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            activate_fails: self.activate_fails.saturating_add(other.activate_fails),
            cfg_fails: self.cfg_fails.saturating_add(other.cfg_fails),
            rx_queue_event_fails: self
                .rx_queue_event_fails
                .saturating_add(other.rx_queue_event_fails),
            tx_queue_event_fails: self
                .tx_queue_event_fails
                .saturating_add(other.tx_queue_event_fails),
            ev_queue_event_fails: self
                .ev_queue_event_fails
                .saturating_add(other.ev_queue_event_fails),
            muxer_event_fails: self
                .muxer_event_fails
                .saturating_add(other.muxer_event_fails),
            conn_event_fails: self.conn_event_fails.saturating_add(other.conn_event_fails),
            rx_queue_event_count: self
                .rx_queue_event_count
                .saturating_add(other.rx_queue_event_count),
            tx_queue_event_count: self
                .tx_queue_event_count
                .saturating_add(other.tx_queue_event_count),
            rx_bytes_count: self.rx_bytes_count.saturating_add(other.rx_bytes_count),
            tx_bytes_count: self.tx_bytes_count.saturating_add(other.tx_bytes_count),
            rx_packets_count: self.rx_packets_count.saturating_add(other.rx_packets_count),
            tx_packets_count: self.tx_packets_count.saturating_add(other.tx_packets_count),
            conns_added: self.conns_added.saturating_add(other.conns_added),
            conns_killed: self.conns_killed.saturating_add(other.conns_killed),
            conns_removed: self.conns_removed.saturating_add(other.conns_removed),
            killq_resync: self.killq_resync.saturating_add(other.killq_resync),
            tx_flush_fails: self.tx_flush_fails.saturating_add(other.tx_flush_fails),
            tx_write_fails: self.tx_write_fails.saturating_add(other.tx_write_fails),
            rx_read_fails: self.rx_read_fails.saturating_add(other.rx_read_fails),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedVsockDeviceMetrics {
    inner: Arc<SharedVsockDeviceMetricsInner>,
}

impl SharedVsockDeviceMetrics {
    pub fn record_activation_failure(&self) {
        record_atomic_metric(&self.inner.activate_fails, 1);
    }

    pub fn record_config_failure(&self) {
        record_atomic_metric(&self.inner.cfg_fails, 1);
    }

    pub fn record_notification_dispatch(&self, dispatch: &VirtioVsockDeviceNotificationDispatch) {
        let rx_queue_events = vsock_queue_event_count(
            dispatch.drained_notifications(),
            VIRTIO_VSOCK_RX_QUEUE_INDEX,
        );
        let tx_queue_events = vsock_queue_event_count(
            dispatch.drained_notifications(),
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        self.record_rx_queue_events(rx_queue_events);
        self.record_tx_queue_events(tx_queue_events);

        if let Some(dispatch) = dispatch.rx_queue_dispatch() {
            self.record_rx_queue_dispatch(dispatch);
        }
        if let Some(dispatch) = dispatch.tx_queue_dispatch() {
            self.record_tx_queue_dispatch(dispatch);
        }

        self.record_connections_added(usize_to_u64_saturating(
            dispatch
                .host_request_dispatch()
                .completed_requests()
                .saturating_add(dispatch.guest_request_dispatch().retained_requests()),
        ));
        self.record_connections_removed(usize_to_u64_saturating(
            dispatch
                .guest_response_dispatch()
                .dropped_connections()
                .saturating_add(dispatch.guest_rw_dispatch().dropped_connections())
                .saturating_add(dispatch.guest_rst_dispatch().closed_host_connections())
                .saturating_add(dispatch.guest_rst_dispatch().closed_guest_connections())
                .saturating_add(dispatch.guest_shutdown_dispatch().closed_host_connections())
                .saturating_add(
                    dispatch
                        .guest_shutdown_dispatch()
                        .closed_guest_connections(),
                ),
        ));
        self.record_tx_packets(
            0,
            usize_to_u64_saturating(dispatch.guest_rw_dispatch().forwarded_bytes()),
        );
        self.record_connection_event_failures(usize_to_u64_saturating(
            dispatch
                .host_request_dispatch()
                .dropped_connections()
                .saturating_add(dispatch.guest_response_dispatch().dropped_connections())
                .saturating_add(dispatch.guest_request_dispatch().dropped_requests())
                .saturating_add(dispatch.guest_rw_dispatch().dropped_connections()),
        ));
    }

    pub fn record_notification_error(&self, source: &VirtioVsockDeviceNotificationError) {
        let rx_queue_events =
            vsock_queue_event_count(source.drained_notifications(), VIRTIO_VSOCK_RX_QUEUE_INDEX);
        let tx_queue_events =
            vsock_queue_event_count(source.drained_notifications(), VIRTIO_VSOCK_TX_QUEUE_INDEX);
        self.record_rx_queue_events(rx_queue_events);
        self.record_tx_queue_events(tx_queue_events);

        match source {
            VirtioVsockDeviceNotificationError::TxQueueDispatch { .. } => {
                self.record_tx_queue_event_failure();
            }
            VirtioVsockDeviceNotificationError::RxQueueDispatch { .. } => {
                self.record_rx_queue_event_failure();
            }
            VirtioVsockDeviceNotificationError::UnsupportedQueue { queue_index, .. } => {
                self.record_unsupported_queue_failure(*queue_index);
            }
            VirtioVsockDeviceNotificationError::Inactive { .. } => {
                self.record_muxer_event_failure();
            }
        }

        if let Some(dispatch) = source.completed_tx_dispatch() {
            self.record_tx_queue_dispatch(dispatch);
        }
        if let Some(dispatch) = source.completed_rx_dispatch() {
            self.record_rx_queue_dispatch(dispatch);
        }
    }

    pub fn record_muxer_event_failure(&self) {
        record_atomic_metric(&self.inner.muxer_event_fails, 1);
    }

    pub fn snapshot(&self) -> VsockDeviceMetrics {
        VsockDeviceMetrics {
            activate_fails: self.inner.activate_fails.load(Ordering::Relaxed),
            cfg_fails: self.inner.cfg_fails.load(Ordering::Relaxed),
            rx_queue_event_fails: self.inner.rx_queue_event_fails.load(Ordering::Relaxed),
            tx_queue_event_fails: self.inner.tx_queue_event_fails.load(Ordering::Relaxed),
            ev_queue_event_fails: self.inner.ev_queue_event_fails.load(Ordering::Relaxed),
            muxer_event_fails: self.inner.muxer_event_fails.load(Ordering::Relaxed),
            conn_event_fails: self.inner.conn_event_fails.load(Ordering::Relaxed),
            rx_queue_event_count: self.inner.rx_queue_event_count.load(Ordering::Relaxed),
            tx_queue_event_count: self.inner.tx_queue_event_count.load(Ordering::Relaxed),
            rx_bytes_count: self.inner.rx_bytes_count.load(Ordering::Relaxed),
            tx_bytes_count: self.inner.tx_bytes_count.load(Ordering::Relaxed),
            rx_packets_count: self.inner.rx_packets_count.load(Ordering::Relaxed),
            tx_packets_count: self.inner.tx_packets_count.load(Ordering::Relaxed),
            conns_added: self.inner.conns_added.load(Ordering::Relaxed),
            conns_killed: self.inner.conns_killed.load(Ordering::Relaxed),
            conns_removed: self.inner.conns_removed.load(Ordering::Relaxed),
            killq_resync: self.inner.killq_resync.load(Ordering::Relaxed),
            tx_flush_fails: self.inner.tx_flush_fails.load(Ordering::Relaxed),
            tx_write_fails: self.inner.tx_write_fails.load(Ordering::Relaxed),
            rx_read_fails: self.inner.rx_read_fails.load(Ordering::Relaxed),
        }
    }

    fn record_rx_queue_dispatch(&self, dispatch: &VirtioVsockRxQueueDispatch) {
        let delivered_packets = dispatch
            .delivered_requests()
            .saturating_add(dispatch.delivered_responses())
            .saturating_add(dispatch.delivered_reset_packets())
            .saturating_add(dispatch.delivered_credit_updates())
            .saturating_add(dispatch.delivered_host_rw_packets());
        self.record_rx_packets(
            usize_to_u64_saturating(delivered_packets),
            usize_to_u64_saturating(dispatch.delivered_host_rw_bytes()),
        );
        self.record_rx_queue_failures(usize_to_u64_saturating(
            dispatch
                .buffer_parse_failures()
                .saturating_add(dispatch.buffer_too_small_failures()),
        ));
    }

    fn record_tx_queue_dispatch(&self, dispatch: &VirtioVsockTxQueueDispatch) {
        self.record_tx_packets(usize_to_u64_saturating(dispatch.successful_packets()), 0);
        self.record_tx_queue_failures(usize_to_u64_saturating(dispatch.parse_failures()));
    }

    fn record_rx_queue_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rx_queue_event_count, count);
        }
    }

    fn record_tx_queue_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_queue_event_count, count);
        }
    }

    fn record_rx_packets(&self, count: u64, bytes: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rx_packets_count, count);
        }
        if bytes != 0 {
            record_atomic_metric(&self.inner.rx_bytes_count, bytes);
        }
    }

    fn record_tx_packets(&self, count: u64, bytes: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_packets_count, count);
        }
        if bytes != 0 {
            record_atomic_metric(&self.inner.tx_bytes_count, bytes);
        }
    }

    fn record_rx_queue_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rx_queue_event_fails, count);
        }
    }

    fn record_tx_queue_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.tx_queue_event_fails, count);
        }
    }

    fn record_tx_queue_event_failure(&self) {
        record_atomic_metric(&self.inner.tx_queue_event_fails, 1);
    }

    fn record_rx_queue_event_failure(&self) {
        record_atomic_metric(&self.inner.rx_queue_event_fails, 1);
    }

    fn record_event_queue_failure(&self) {
        record_atomic_metric(&self.inner.ev_queue_event_fails, 1);
    }

    fn record_unsupported_queue_failure(&self, queue_index: usize) {
        match queue_index {
            VIRTIO_VSOCK_RX_QUEUE_INDEX => self.record_rx_queue_event_failure(),
            VIRTIO_VSOCK_TX_QUEUE_INDEX => self.record_tx_queue_event_failure(),
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX => self.record_event_queue_failure(),
            _ => self.record_muxer_event_failure(),
        }
    }

    fn record_connections_added(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.conns_added, count);
        }
    }

    fn record_connections_removed(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.conns_removed, count);
        }
    }

    fn record_connection_event_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.conn_event_fails, count);
        }
    }
}

#[derive(Debug, Default)]
struct SharedVsockDeviceMetricsInner {
    activate_fails: AtomicU64,
    cfg_fails: AtomicU64,
    rx_queue_event_fails: AtomicU64,
    tx_queue_event_fails: AtomicU64,
    ev_queue_event_fails: AtomicU64,
    muxer_event_fails: AtomicU64,
    conn_event_fails: AtomicU64,
    rx_queue_event_count: AtomicU64,
    tx_queue_event_count: AtomicU64,
    rx_bytes_count: AtomicU64,
    tx_bytes_count: AtomicU64,
    rx_packets_count: AtomicU64,
    tx_packets_count: AtomicU64,
    conns_added: AtomicU64,
    conns_killed: AtomicU64,
    conns_removed: AtomicU64,
    killq_resync: AtomicU64,
    tx_flush_fails: AtomicU64,
    tx_write_fails: AtomicU64,
    rx_read_fails: AtomicU64,
}

fn vsock_queue_event_count(drained_notifications: &[usize], queue_index: usize) -> u64 {
    usize_to_u64_saturating(
        drained_notifications
            .iter()
            .copied()
            .filter(|drained_queue_index| *drained_queue_index == queue_index)
            .count(),
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntropyDeviceMetrics {
    activate_fails: u64,
    entropy_event_fails: u64,
    entropy_event_count: u64,
    entropy_bytes: u64,
    host_rng_fails: u64,
    entropy_rate_limiter_throttled: u64,
    rate_limiter_event_count: u64,
}

impl EntropyDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.entropy_event_fails == 0
            && self.entropy_event_count == 0
            && self.entropy_bytes == 0
            && self.host_rng_fails == 0
            && self.entropy_rate_limiter_throttled == 0
            && self.rate_limiter_event_count == 0
    }

    pub const fn activate_fails(self) -> u64 {
        self.activate_fails
    }

    pub const fn entropy_event_fails(self) -> u64 {
        self.entropy_event_fails
    }

    pub const fn entropy_event_count(self) -> u64 {
        self.entropy_event_count
    }

    pub const fn entropy_bytes(self) -> u64 {
        self.entropy_bytes
    }

    pub const fn host_rng_fails(self) -> u64 {
        self.host_rng_fails
    }

    pub const fn entropy_rate_limiter_throttled(self) -> u64 {
        self.entropy_rate_limiter_throttled
    }

    pub const fn rate_limiter_event_count(self) -> u64 {
        self.rate_limiter_event_count
    }

    pub const fn with_activate_fails(mut self, activate_fails: u64) -> Self {
        self.activate_fails = activate_fails;
        self
    }

    pub const fn with_entropy_event_fails(mut self, entropy_event_fails: u64) -> Self {
        self.entropy_event_fails = entropy_event_fails;
        self
    }

    pub const fn with_entropy_event_count(mut self, entropy_event_count: u64) -> Self {
        self.entropy_event_count = entropy_event_count;
        self
    }

    pub const fn with_entropy_bytes(mut self, entropy_bytes: u64) -> Self {
        self.entropy_bytes = entropy_bytes;
        self
    }

    pub const fn with_host_rng_fails(mut self, host_rng_fails: u64) -> Self {
        self.host_rng_fails = host_rng_fails;
        self
    }

    pub const fn with_entropy_rate_limiter_throttled(
        mut self,
        entropy_rate_limiter_throttled: u64,
    ) -> Self {
        self.entropy_rate_limiter_throttled = entropy_rate_limiter_throttled;
        self
    }

    pub const fn with_rate_limiter_event_count(mut self, rate_limiter_event_count: u64) -> Self {
        self.rate_limiter_event_count = rate_limiter_event_count;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            activate_fails: self.activate_fails.saturating_add(other.activate_fails),
            entropy_event_fails: self
                .entropy_event_fails
                .saturating_add(other.entropy_event_fails),
            entropy_event_count: self
                .entropy_event_count
                .saturating_add(other.entropy_event_count),
            entropy_bytes: self.entropy_bytes.saturating_add(other.entropy_bytes),
            host_rng_fails: self.host_rng_fails.saturating_add(other.host_rng_fails),
            entropy_rate_limiter_throttled: self
                .entropy_rate_limiter_throttled
                .saturating_add(other.entropy_rate_limiter_throttled),
            rate_limiter_event_count: self
                .rate_limiter_event_count
                .saturating_add(other.rate_limiter_event_count),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedEntropyDeviceMetrics {
    inner: Arc<SharedEntropyDeviceMetricsInner>,
}

impl SharedEntropyDeviceMetrics {
    pub fn record_activation_failure(&self) {
        record_atomic_metric(&self.inner.activate_fails, 1);
    }

    pub fn record_notification_dispatch(&self, dispatch: &VirtioRngDeviceNotificationDispatch) {
        if let Some(queue_dispatch) = dispatch.queue_dispatch() {
            self.record_queue_dispatch(queue_dispatch);
        }
    }

    pub fn record_notification_error(&self, source: &VirtioRngDeviceNotificationError) {
        if let Some(completed) = source.completed_dispatch() {
            self.record_queue_dispatch(completed);
        }
        self.record_event_failure();
    }

    pub fn record_entropy_source_provider_failure(&self) {
        self.record_host_rng_failure();
        self.record_event_failure();
    }

    pub fn record_event_failure(&self) {
        record_atomic_metric(&self.inner.entropy_event_fails, 1);
    }

    pub fn record_host_rng_failure(&self) {
        record_atomic_metric(&self.inner.host_rng_fails, 1);
    }

    pub fn snapshot(&self) -> EntropyDeviceMetrics {
        EntropyDeviceMetrics {
            activate_fails: self.inner.activate_fails.load(Ordering::Relaxed),
            entropy_event_fails: self.inner.entropy_event_fails.load(Ordering::Relaxed),
            entropy_event_count: self.inner.entropy_event_count.load(Ordering::Relaxed),
            entropy_bytes: self.inner.entropy_bytes.load(Ordering::Relaxed),
            host_rng_fails: self.inner.host_rng_fails.load(Ordering::Relaxed),
            entropy_rate_limiter_throttled: self
                .inner
                .entropy_rate_limiter_throttled
                .load(Ordering::Relaxed),
            rate_limiter_event_count: self.inner.rate_limiter_event_count.load(Ordering::Relaxed),
        }
    }

    pub fn record_queue_dispatch(&self, dispatch: &VirtioRngQueueDispatch) {
        self.record_entropy_events(usize_to_u64_saturating(dispatch.processed_requests()));
        self.record_entropy_bytes(dispatch.bytes_written_to_guest());
        self.record_event_failures(usize_to_u64_saturating(
            dispatch
                .buffer_parse_failures()
                .saturating_add(dispatch.source_failures()),
        ));
        self.record_host_rng_failures(usize_to_u64_saturating(dispatch.source_failures()));
        self.record_rate_limiter_throttled(usize_to_u64_saturating(
            dispatch.rate_limiter_throttled_requests(),
        ));
        self.record_rate_limiter_events(usize_to_u64_saturating(dispatch.rate_limiter_events()));
    }

    fn record_entropy_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.entropy_event_count, count);
        }
    }

    fn record_entropy_bytes(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.entropy_bytes, count);
        }
    }

    fn record_event_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.entropy_event_fails, count);
        }
    }

    fn record_host_rng_failures(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.host_rng_fails, count);
        }
    }

    fn record_rate_limiter_throttled(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.entropy_rate_limiter_throttled, count);
        }
    }

    fn record_rate_limiter_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.rate_limiter_event_count, count);
        }
    }
}

#[derive(Debug, Default)]
struct SharedEntropyDeviceMetricsInner {
    activate_fails: AtomicU64,
    entropy_event_fails: AtomicU64,
    entropy_event_count: AtomicU64,
    entropy_bytes: AtomicU64,
    host_rng_fails: AtomicU64,
    entropy_rate_limiter_throttled: AtomicU64,
    rate_limiter_event_count: AtomicU64,
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
    record_atomic_metric_with_ordering(metric, increment, Ordering::Relaxed);
}

fn record_atomic_metric_release(metric: &AtomicU64, increment: u64) {
    record_atomic_metric_with_ordering(metric, increment, Ordering::Release);
}

fn record_atomic_metric_with_ordering(metric: &AtomicU64, increment: u64, success: Ordering) {
    let mut current = metric.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(increment);
        match metric.compare_exchange_weak(current, next, success, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn record_atomic_min_metric(metric: &AtomicU64, value: u64) {
    let mut current = metric.load(Ordering::Relaxed);
    while value < current {
        match metric.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn record_atomic_max_metric(metric: &AtomicU64, value: u64) {
    let mut current = metric.load(Ordering::Relaxed);
    while value > current {
        match metric.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn record_latency_aggregate(
    latency_aggregate: VirtioBlockLatencyAggregate,
    min_us: &AtomicU64,
    max_us: &AtomicU64,
    sum_us: &AtomicU64,
    sample_count: &AtomicU64,
) {
    if latency_aggregate.is_empty() {
        return;
    }

    record_atomic_min_metric(min_us, latency_aggregate.min_us());
    record_atomic_max_metric(max_us, latency_aggregate.max_us());
    record_atomic_metric(sum_us, latency_aggregate.sum_us());
    record_atomic_metric_release(sample_count, latency_aggregate.sample_count());
}

fn latency_aggregate_snapshot(
    min_us: &AtomicU64,
    max_us: &AtomicU64,
    sum_us: &AtomicU64,
    sample_count: &AtomicU64,
) -> VirtioBlockLatencyAggregate {
    let sample_count = sample_count.load(Ordering::Acquire);
    if sample_count == 0 {
        return VirtioBlockLatencyAggregate::default();
    }

    VirtioBlockLatencyAggregate::new(
        min_us.load(Ordering::Relaxed),
        max_us.load(Ordering::Relaxed),
        sum_us.load(Ordering::Relaxed),
        sample_count,
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsDiagnostics {
    block_device_metrics: Option<BlockDeviceMetrics>,
    block_device_metrics_by_drive: Option<BlockDeviceMetricsByDrive>,
    network_interface_metrics: Option<NetworkInterfaceMetrics>,
    network_interface_metrics_by_interface: Option<NetworkInterfaceMetricsByInterface>,
    vsock_device_metrics: Option<VsockDeviceMetrics>,
    entropy_device_metrics: Option<EntropyDeviceMetrics>,
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
            network_interface_metrics: None,
            network_interface_metrics_by_interface: None,
            vsock_device_metrics: None,
            entropy_device_metrics: None,
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

    pub fn with_network_interface_metrics(
        mut self,
        network_interface_metrics: NetworkInterfaceMetrics,
    ) -> Self {
        self.network_interface_metrics = Some(network_interface_metrics);
        self
    }

    pub fn with_network_interface_metrics_by_interface(
        mut self,
        network_interface_metrics_by_interface: NetworkInterfaceMetricsByInterface,
    ) -> Self {
        self.network_interface_metrics_by_interface = Some(network_interface_metrics_by_interface);
        self
    }

    pub fn with_vsock_device_metrics(mut self, vsock_device_metrics: VsockDeviceMetrics) -> Self {
        self.vsock_device_metrics = Some(vsock_device_metrics);
        self
    }

    pub fn with_entropy_device_metrics(
        mut self,
        entropy_device_metrics: EntropyDeviceMetrics,
    ) -> Self {
        self.entropy_device_metrics = Some(entropy_device_metrics);
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
        if let Some(metrics) = other.network_interface_metrics {
            self.network_interface_metrics = Some(match self.network_interface_metrics {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }
        if let Some(metrics) = other.network_interface_metrics_by_interface {
            self.network_interface_metrics_by_interface =
                Some(match self.network_interface_metrics_by_interface {
                    Some(existing) => existing.merged_with(metrics),
                    None => metrics,
                });
        }
        if let Some(metrics) = other.vsock_device_metrics {
            self.vsock_device_metrics = Some(match self.vsock_device_metrics {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }
        if let Some(metrics) = other.entropy_device_metrics {
            self.entropy_device_metrics = Some(match self.entropy_device_metrics {
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

    pub fn network_interface_metrics(&self) -> Option<NetworkInterfaceMetrics> {
        self.network_interface_metrics
    }

    pub fn network_interface_metrics_by_interface(
        &self,
    ) -> Option<&NetworkInterfaceMetricsByInterface> {
        self.network_interface_metrics_by_interface.as_ref()
    }

    pub fn vsock_device_metrics(&self) -> Option<VsockDeviceMetrics> {
        self.vsock_device_metrics
    }

    pub fn entropy_device_metrics(&self) -> Option<EntropyDeviceMetrics> {
        self.entropy_device_metrics
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
        "rate_limiter_event_count".to_string(),
        serde_json::Value::Number(metrics.rate_limiter_event_count().into()),
    );
    block.insert(
        "rate_limiter_throttled_events".to_string(),
        serde_json::Value::Number(metrics.rate_limiter_throttled_events().into()),
    );
    block.insert(
        "update_count".to_string(),
        serde_json::Value::Number(metrics.update_count().into()),
    );
    block.insert(
        "update_fails".to_string(),
        serde_json::Value::Number(metrics.update_fails().into()),
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
    block.insert(
        "read_agg".to_string(),
        serde_json::Value::Object(latency_aggregate_metrics_json_object(metrics.read_agg())),
    );
    block.insert(
        "write_agg".to_string(),
        serde_json::Value::Object(latency_aggregate_metrics_json_object(metrics.write_agg())),
    );
    block
}

fn network_interface_metrics_json_object(
    metrics: NetworkInterfaceMetrics,
) -> serde_json::Map<String, serde_json::Value> {
    let mut net = serde_json::Map::new();
    net.insert(
        "event_fails".to_string(),
        serde_json::Value::Number(metrics.event_fails().into()),
    );
    net.insert(
        "rx_bytes_count".to_string(),
        serde_json::Value::Number(metrics.rx_bytes_count().into()),
    );
    net.insert(
        "rx_count".to_string(),
        serde_json::Value::Number(metrics.rx_count().into()),
    );
    net.insert(
        "rx_fails".to_string(),
        serde_json::Value::Number(metrics.rx_fails().into()),
    );
    net.insert(
        "rx_packets_count".to_string(),
        serde_json::Value::Number(metrics.rx_packets_count().into()),
    );
    net.insert(
        "rx_queue_event_count".to_string(),
        serde_json::Value::Number(metrics.rx_queue_event_count().into()),
    );
    net.insert(
        "tx_bytes_count".to_string(),
        serde_json::Value::Number(metrics.tx_bytes_count().into()),
    );
    net.insert(
        "tx_count".to_string(),
        serde_json::Value::Number(metrics.tx_count().into()),
    );
    net.insert(
        "tx_fails".to_string(),
        serde_json::Value::Number(metrics.tx_fails().into()),
    );
    net.insert(
        "tx_malformed_frames".to_string(),
        serde_json::Value::Number(metrics.tx_malformed_frames().into()),
    );
    net.insert(
        "tx_packets_count".to_string(),
        serde_json::Value::Number(metrics.tx_packets_count().into()),
    );
    net.insert(
        "tx_queue_event_count".to_string(),
        serde_json::Value::Number(metrics.tx_queue_event_count().into()),
    );
    net
}

fn vsock_device_metrics_json_object(
    metrics: VsockDeviceMetrics,
) -> serde_json::Map<String, serde_json::Value> {
    let mut vsock = serde_json::Map::new();
    vsock.insert(
        "activate_fails".to_string(),
        serde_json::Value::Number(metrics.activate_fails().into()),
    );
    vsock.insert(
        "cfg_fails".to_string(),
        serde_json::Value::Number(metrics.cfg_fails().into()),
    );
    vsock.insert(
        "rx_queue_event_fails".to_string(),
        serde_json::Value::Number(metrics.rx_queue_event_fails().into()),
    );
    vsock.insert(
        "tx_queue_event_fails".to_string(),
        serde_json::Value::Number(metrics.tx_queue_event_fails().into()),
    );
    vsock.insert(
        "ev_queue_event_fails".to_string(),
        serde_json::Value::Number(metrics.ev_queue_event_fails().into()),
    );
    vsock.insert(
        "muxer_event_fails".to_string(),
        serde_json::Value::Number(metrics.muxer_event_fails().into()),
    );
    vsock.insert(
        "conn_event_fails".to_string(),
        serde_json::Value::Number(metrics.conn_event_fails().into()),
    );
    vsock.insert(
        "rx_queue_event_count".to_string(),
        serde_json::Value::Number(metrics.rx_queue_event_count().into()),
    );
    vsock.insert(
        "tx_queue_event_count".to_string(),
        serde_json::Value::Number(metrics.tx_queue_event_count().into()),
    );
    vsock.insert(
        "rx_bytes_count".to_string(),
        serde_json::Value::Number(metrics.rx_bytes_count().into()),
    );
    vsock.insert(
        "tx_bytes_count".to_string(),
        serde_json::Value::Number(metrics.tx_bytes_count().into()),
    );
    vsock.insert(
        "rx_packets_count".to_string(),
        serde_json::Value::Number(metrics.rx_packets_count().into()),
    );
    vsock.insert(
        "tx_packets_count".to_string(),
        serde_json::Value::Number(metrics.tx_packets_count().into()),
    );
    vsock.insert(
        "conns_added".to_string(),
        serde_json::Value::Number(metrics.conns_added().into()),
    );
    vsock.insert(
        "conns_killed".to_string(),
        serde_json::Value::Number(metrics.conns_killed().into()),
    );
    vsock.insert(
        "conns_removed".to_string(),
        serde_json::Value::Number(metrics.conns_removed().into()),
    );
    vsock.insert(
        "killq_resync".to_string(),
        serde_json::Value::Number(metrics.killq_resync().into()),
    );
    vsock.insert(
        "tx_flush_fails".to_string(),
        serde_json::Value::Number(metrics.tx_flush_fails().into()),
    );
    vsock.insert(
        "tx_write_fails".to_string(),
        serde_json::Value::Number(metrics.tx_write_fails().into()),
    );
    vsock.insert(
        "rx_read_fails".to_string(),
        serde_json::Value::Number(metrics.rx_read_fails().into()),
    );
    vsock
}

fn entropy_device_metrics_json_object(
    metrics: EntropyDeviceMetrics,
) -> serde_json::Map<String, serde_json::Value> {
    let mut entropy = serde_json::Map::new();
    entropy.insert(
        "activate_fails".to_string(),
        serde_json::Value::Number(metrics.activate_fails().into()),
    );
    entropy.insert(
        "entropy_bytes".to_string(),
        serde_json::Value::Number(metrics.entropy_bytes().into()),
    );
    entropy.insert(
        "entropy_event_count".to_string(),
        serde_json::Value::Number(metrics.entropy_event_count().into()),
    );
    entropy.insert(
        "entropy_event_fails".to_string(),
        serde_json::Value::Number(metrics.entropy_event_fails().into()),
    );
    entropy.insert(
        "entropy_rate_limiter_throttled".to_string(),
        serde_json::Value::Number(metrics.entropy_rate_limiter_throttled().into()),
    );
    entropy.insert(
        "host_rng_fails".to_string(),
        serde_json::Value::Number(metrics.host_rng_fails().into()),
    );
    entropy.insert(
        "rate_limiter_event_count".to_string(),
        serde_json::Value::Number(metrics.rate_limiter_event_count().into()),
    );
    entropy
}

fn serial_output_metrics_json_object(
    metrics: SerialOutputMetrics,
) -> serde_json::Map<String, serde_json::Value> {
    let mut uart = serde_json::Map::new();
    uart.insert(
        "error_count".to_string(),
        serde_json::Value::Number(metrics.error_count().into()),
    );
    uart.insert(
        "flush_count".to_string(),
        serde_json::Value::Number(metrics.flush_count().into()),
    );
    uart.insert(
        "missed_read_count".to_string(),
        serde_json::Value::Number(metrics.missed_read_count().into()),
    );
    uart.insert(
        "missed_write_count".to_string(),
        serde_json::Value::Number(metrics.missed_write_count().into()),
    );
    uart.insert(
        "read_count".to_string(),
        serde_json::Value::Number(metrics.read_count().into()),
    );
    uart.insert(
        "write_count".to_string(),
        serde_json::Value::Number(metrics.write_count().into()),
    );
    uart.insert(
        "rate_limiter_dropped_bytes".to_string(),
        serde_json::Value::Number(metrics.rate_limiter_dropped_bytes().into()),
    );
    uart
}

fn latency_aggregate_metrics_json_object(
    metrics: VirtioBlockLatencyAggregate,
) -> serde_json::Map<String, serde_json::Value> {
    let mut aggregate = serde_json::Map::new();
    aggregate.insert(
        "min_us".to_string(),
        serde_json::Value::Number(metrics.min_us().into()),
    );
    aggregate.insert(
        "max_us".to_string(),
        serde_json::Value::Number(metrics.max_us().into()),
    );
    aggregate.insert(
        "sum_us".to_string(),
        serde_json::Value::Number(metrics.sum_us().into()),
    );
    aggregate
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
        if let Some(network_interface_metrics_by_interface) =
            diagnostics.network_interface_metrics_by_interface()
        {
            for (iface_id, metrics) in network_interface_metrics_by_interface.iter() {
                if !metrics.is_empty() {
                    root.insert(
                        format!("net_{iface_id}"),
                        serde_json::Value::Object(network_interface_metrics_json_object(metrics)),
                    );
                }
            }
        }
        if let Some(network_interface_metrics) = diagnostics.network_interface_metrics()
            && !network_interface_metrics.is_empty()
        {
            root.insert(
                "net".to_string(),
                serde_json::Value::Object(network_interface_metrics_json_object(
                    network_interface_metrics,
                )),
            );
        }
        if let Some(vsock_device_metrics) = diagnostics.vsock_device_metrics()
            && !vsock_device_metrics.is_empty()
        {
            root.insert(
                "vsock".to_string(),
                serde_json::Value::Object(vsock_device_metrics_json_object(vsock_device_metrics)),
            );
        }
        if let Some(entropy_device_metrics) = diagnostics.entropy_device_metrics()
            && !entropy_device_metrics.is_empty()
        {
            root.insert(
                "entropy".to_string(),
                serde_json::Value::Object(entropy_device_metrics_json_object(
                    entropy_device_metrics,
                )),
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
            root.insert(
                "uart".to_string(),
                serde_json::Value::Object(serial_output_metrics_json_object(serial_output_metrics)),
            );
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
        BootRunLoopMetricStatus, EntropyDeviceMetrics, MetricsConfigError, MetricsConfigInput,
        MetricsDiagnostics, MetricsFlushError, MetricsOutput, MetricsState,
        NetworkInterfaceMetrics, NetworkInterfaceMetricsByInterface, SharedBalloonDeviceMetrics,
        SharedBlockDeviceMetrics, SharedBlockDeviceMetricsRegistry, SharedEntropyDeviceMetrics,
        SharedNetworkInterfaceMetrics, SharedNetworkInterfaceMetricsRegistry,
        SharedVsockDeviceMetrics, VsockDeviceMetrics,
    };
    use crate::block::VirtioBlockLatencyAggregate;
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
            .with_rate_limiter_event_count(12)
            .with_rate_limiter_throttled_events(13)
            .with_update_count(10)
            .with_update_fails(11)
            .with_read_bytes(6)
            .with_write_bytes(7)
            .with_read_count(8)
            .with_write_count(9)
            .with_read_agg(VirtioBlockLatencyAggregate::new(12, 30, 42, 2))
            .with_write_agg(VirtioBlockLatencyAggregate::new(13, 31, 44, 3))
    }

    fn network_metrics_with_all_fields() -> NetworkInterfaceMetrics {
        NetworkInterfaceMetrics::default()
            .with_event_fails(1)
            .with_rx_queue_event_count(2)
            .with_rx_bytes_count(3)
            .with_rx_packets_count(4)
            .with_rx_fails(5)
            .with_rx_count(6)
            .with_tx_bytes_count(7)
            .with_tx_malformed_frames(8)
            .with_tx_fails(9)
            .with_tx_count(10)
            .with_tx_packets_count(11)
            .with_tx_queue_event_count(12)
    }

    fn vsock_metrics_with_all_fields() -> VsockDeviceMetrics {
        VsockDeviceMetrics::default()
            .with_activate_fails(1)
            .with_cfg_fails(2)
            .with_rx_queue_event_fails(3)
            .with_tx_queue_event_fails(4)
            .with_ev_queue_event_fails(5)
            .with_muxer_event_fails(6)
            .with_conn_event_fails(7)
            .with_rx_queue_event_count(8)
            .with_tx_queue_event_count(9)
            .with_rx_bytes_count(10)
            .with_tx_bytes_count(11)
            .with_rx_packets_count(12)
            .with_tx_packets_count(13)
            .with_conns_added(14)
            .with_conns_killed(15)
            .with_conns_removed(16)
            .with_killq_resync(17)
            .with_tx_flush_fails(18)
            .with_tx_write_fails(19)
            .with_rx_read_fails(20)
    }

    fn entropy_metrics_with_all_fields() -> EntropyDeviceMetrics {
        EntropyDeviceMetrics::default()
            .with_activate_fails(1)
            .with_entropy_event_fails(2)
            .with_entropy_event_count(3)
            .with_entropy_bytes(4)
            .with_host_rng_fails(5)
            .with_entropy_rate_limiter_throttled(6)
            .with_rate_limiter_event_count(7)
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
    fn writes_serial_output_diagnostics_when_uart_metrics_are_nonzero() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new().with_serial_output_metrics(
            SerialOutputMetrics::default()
                .with_error_count(1)
                .with_missed_write_count(2)
                .with_write_count(3)
                .with_rate_limiter_dropped_bytes(4),
        );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"uart":{"error_count":1,"flush_count":0,"missed_read_count":0,"missed_write_count":2,"rate_limiter_dropped_bytes":4,"read_count":0,"write_count":3},"vmm":{"metrics_flush_count":1}}"#
            ]
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
                r#"{"block":{"event_fails":1,"execute_fails":2,"flush_count":4,"invalid_reqs_count":3,"queue_event_count":5,"rate_limiter_event_count":12,"rate_limiter_throttled_events":13,"read_agg":{"max_us":30,"min_us":12,"sum_us":42},"read_bytes":6,"read_count":8,"update_count":10,"update_fails":11,"write_agg":{"max_us":31,"min_us":13,"sum_us":44},"write_bytes":7,"write_count":9},"vmm":{"metrics_flush_count":1}}"#
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
            .with_read_count(1)
            .with_read_agg(VirtioBlockLatencyAggregate::new(2, 4, 6, 2));
        let data_metrics = BlockDeviceMetrics::default()
            .with_queue_event_count(1)
            .with_write_bytes(256)
            .with_write_count(1)
            .with_write_agg(VirtioBlockLatencyAggregate::new(3, 5, 8, 2));
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
                r#"{"block":{"event_fails":0,"execute_fails":0,"flush_count":0,"invalid_reqs_count":0,"queue_event_count":2,"rate_limiter_event_count":0,"rate_limiter_throttled_events":0,"read_agg":{"max_us":4,"min_us":2,"sum_us":6},"read_bytes":512,"read_count":1,"update_count":0,"update_fails":0,"write_agg":{"max_us":5,"min_us":3,"sum_us":8},"write_bytes":256,"write_count":1},"block_data":{"event_fails":0,"execute_fails":0,"flush_count":0,"invalid_reqs_count":0,"queue_event_count":1,"rate_limiter_event_count":0,"rate_limiter_throttled_events":0,"read_agg":{"max_us":0,"min_us":0,"sum_us":0},"read_bytes":0,"read_count":0,"update_count":0,"update_fails":0,"write_agg":{"max_us":5,"min_us":3,"sum_us":8},"write_bytes":256,"write_count":1},"block_rootfs":{"event_fails":0,"execute_fails":0,"flush_count":0,"invalid_reqs_count":0,"queue_event_count":1,"rate_limiter_event_count":0,"rate_limiter_throttled_events":0,"read_agg":{"max_us":4,"min_us":2,"sum_us":6},"read_bytes":512,"read_count":1,"update_count":0,"update_fails":0,"write_agg":{"max_us":0,"min_us":0,"sum_us":0},"write_bytes":0,"write_count":0},"vmm":{"metrics_flush_count":1}}"#
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
        first.record_update_for_drive("rootfs");
        first.record_update_failure_for_drive("data");
        first
            .aggregate()
            .record_read_latency_aggregate(VirtioBlockLatencyAggregate::new(0, 10, 10, 2));
        first
            .per_drive("rootfs")
            .expect("rootfs metrics should exist")
            .record_read_latency_aggregate(VirtioBlockLatencyAggregate::new(0, 10, 10, 2));

        assert_eq!(
            first.aggregate_snapshot(),
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(2)
                .with_update_count(1)
                .with_update_fails(1)
                .with_read_agg(VirtioBlockLatencyAggregate::new(0, 10, 10, 2))
        );
        assert_eq!(
            first.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics(
                    "rootfs",
                    BlockDeviceMetrics::default()
                        .with_event_fails(1)
                        .with_queue_event_count(2)
                        .with_update_count(1)
                        .with_read_agg(VirtioBlockLatencyAggregate::new(0, 10, 10, 2)),
                )
                .with_drive_metrics("data", BlockDeviceMetrics::default().with_update_fails(1),)
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
    fn block_latency_metric_preserves_saturated_minimum() {
        let metrics = SharedBlockDeviceMetrics::default();

        metrics.record_read_latency_aggregate(VirtioBlockLatencyAggregate::new(
            u64::MAX,
            u64::MAX,
            u64::MAX,
            1,
        ));

        assert_eq!(
            metrics.snapshot().read_agg(),
            VirtioBlockLatencyAggregate::new(u64::MAX, u64::MAX, u64::MAX, 1)
        );
    }

    #[test]
    fn empty_block_latency_aggregate_normalizes_metric_values() {
        assert_eq!(
            VirtioBlockLatencyAggregate::new(7, 9, 11, 0),
            VirtioBlockLatencyAggregate::default()
        );
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
                .with_rate_limiter_event_count(u64::MAX - 12)
                .with_rate_limiter_throttled_events(u64::MAX - 13)
                .with_update_count(u64::MAX - 10)
                .with_update_fails(u64::MAX - 11)
                .with_read_bytes(u64::MAX - 6)
                .with_write_bytes(u64::MAX - 7)
                .with_read_count(u64::MAX - 8)
                .with_write_count(u64::MAX - 9)
                .with_read_agg(VirtioBlockLatencyAggregate::new(20, 24, u64::MAX - 1, 2))
                .with_write_agg(VirtioBlockLatencyAggregate::new(14, 20, u64::MAX - 2, 1)),
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
                    .with_rate_limiter_event_count(u64::MAX)
                    .with_rate_limiter_throttled_events(u64::MAX)
                    .with_update_count(u64::MAX)
                    .with_update_fails(u64::MAX)
                    .with_read_bytes(u64::MAX)
                    .with_write_bytes(u64::MAX)
                    .with_read_count(u64::MAX)
                    .with_write_count(u64::MAX)
                    .with_read_agg(VirtioBlockLatencyAggregate::new(12, 30, u64::MAX, 4))
                    .with_write_agg(VirtioBlockLatencyAggregate::new(13, 31, u64::MAX, 4))
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
                    .with_read_count(u64::MAX - 2)
                    .with_read_agg(VirtioBlockLatencyAggregate::new(20, 20, u64::MAX - 1, 1)),
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
                    .with_rate_limiter_event_count(12)
                    .with_rate_limiter_throttled_events(13)
                    .with_update_count(10)
                    .with_update_fails(11)
                    .with_read_bytes(6)
                    .with_write_bytes(7)
                    .with_read_count(u64::MAX)
                    .with_write_count(9)
                    .with_read_agg(VirtioBlockLatencyAggregate::new(12, 30, u64::MAX, 3))
                    .with_write_agg(VirtioBlockLatencyAggregate::new(13, 31, 44, 3)),
            )
            .with_drive_metrics("data", BlockDeviceMetrics::default().with_write_count(3));
        let merged = base.merged_with(additional);

        assert_eq!(merged.block_device_metrics_by_drive(), Some(&expected));
    }

    #[test]
    fn writes_network_interface_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new()
            .with_network_interface_metrics(network_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"net":{"event_fails":1,"rx_bytes_count":3,"rx_count":6,"rx_fails":5,"rx_packets_count":4,"rx_queue_event_count":2,"tx_bytes_count":7,"tx_count":10,"tx_fails":9,"tx_malformed_frames":8,"tx_packets_count":11,"tx_queue_event_count":12},"vmm":{"metrics_flush_count":1}}"#
            ]
        );
    }

    #[test]
    fn omits_empty_network_interface_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new()
            .with_network_interface_metrics(NetworkInterfaceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(output.lines(), [r#"{"vmm":{"metrics_flush_count":1}}"#]);
    }

    #[test]
    fn writes_network_interface_metrics_by_interface_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let eth0_metrics = NetworkInterfaceMetrics::default()
            .with_rx_queue_event_count(1)
            .with_rx_bytes_count(128)
            .with_rx_packets_count(1)
            .with_rx_count(1);
        let eth1_metrics = NetworkInterfaceMetrics::default()
            .with_tx_queue_event_count(1)
            .with_tx_bytes_count(64)
            .with_tx_packets_count(1)
            .with_tx_count(1);
        let diagnostics = MetricsDiagnostics::new()
            .with_network_interface_metrics(eth0_metrics.merged_with(eth1_metrics))
            .with_network_interface_metrics_by_interface(
                NetworkInterfaceMetricsByInterface::new()
                    .with_interface_metrics("eth0", eth0_metrics)
                    .with_interface_metrics("noop", NetworkInterfaceMetrics::default())
                    .with_interface_metrics("eth1", eth1_metrics),
            );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"net":{"event_fails":0,"rx_bytes_count":128,"rx_count":1,"rx_fails":0,"rx_packets_count":1,"rx_queue_event_count":1,"tx_bytes_count":64,"tx_count":1,"tx_fails":0,"tx_malformed_frames":0,"tx_packets_count":1,"tx_queue_event_count":1},"net_eth0":{"event_fails":0,"rx_bytes_count":128,"rx_count":1,"rx_fails":0,"rx_packets_count":1,"rx_queue_event_count":1,"tx_bytes_count":0,"tx_count":0,"tx_fails":0,"tx_malformed_frames":0,"tx_packets_count":0,"tx_queue_event_count":0},"net_eth1":{"event_fails":0,"rx_bytes_count":0,"rx_count":0,"rx_fails":0,"rx_packets_count":0,"rx_queue_event_count":0,"tx_bytes_count":64,"tx_count":1,"tx_fails":0,"tx_malformed_frames":0,"tx_packets_count":1,"tx_queue_event_count":1},"vmm":{"metrics_flush_count":1}}"#
            ]
        );
    }

    #[test]
    fn shared_network_interface_metrics_snapshot_is_per_instance() {
        let first = SharedNetworkInterfaceMetrics::default();
        let second = SharedNetworkInterfaceMetrics::default();

        first.record_rx_queue_events(2);
        first.record_tx_queue_events(3);
        first.record_event_failure();

        assert_eq!(
            first.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_event_fails(1)
                .with_rx_queue_event_count(2)
                .with_tx_queue_event_count(3)
        );
        assert_eq!(second.snapshot(), NetworkInterfaceMetrics::default());
    }

    #[test]
    fn shared_network_interface_metrics_registry_snapshot_is_per_instance() {
        let first = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0", "eth1"]);
        let second = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);

        first.record_queue_events_for_interface("eth0", 2, 3);
        first.record_event_failure_for_interface("eth0");
        first.record_event_failure_for_interface("eth1");

        assert_eq!(
            first.aggregate_snapshot(),
            NetworkInterfaceMetrics::default()
                .with_event_fails(2)
                .with_rx_queue_event_count(2)
                .with_tx_queue_event_count(3)
        );
        assert_eq!(
            first.per_interface_snapshot(),
            NetworkInterfaceMetricsByInterface::new()
                .with_interface_metrics(
                    "eth0",
                    NetworkInterfaceMetrics::default()
                        .with_event_fails(1)
                        .with_rx_queue_event_count(2)
                        .with_tx_queue_event_count(3),
                )
                .with_interface_metrics(
                    "eth1",
                    NetworkInterfaceMetrics::default().with_event_fails(1),
                )
        );
        assert_eq!(
            second.aggregate_snapshot(),
            NetworkInterfaceMetrics::default()
        );
        assert!(second.per_interface_snapshot().is_empty());
    }

    #[test]
    fn network_metric_increment_saturates() {
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics
            .inner
            .rx_queue_event_count
            .store(u64::MAX - 1, Ordering::Relaxed);

        metrics.record_rx_queue_events(3);

        assert_eq!(metrics.snapshot().rx_queue_event_count(), u64::MAX);
    }

    #[test]
    fn network_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_network_interface_metrics(
            NetworkInterfaceMetrics::default()
                .with_event_fails(u64::MAX - 1)
                .with_rx_queue_event_count(u64::MAX - 2)
                .with_rx_bytes_count(u64::MAX - 3)
                .with_rx_packets_count(u64::MAX - 4)
                .with_rx_fails(u64::MAX - 5)
                .with_rx_count(u64::MAX - 6)
                .with_tx_bytes_count(u64::MAX - 7)
                .with_tx_malformed_frames(u64::MAX - 8)
                .with_tx_fails(u64::MAX - 9)
                .with_tx_count(u64::MAX - 10)
                .with_tx_packets_count(u64::MAX - 11)
                .with_tx_queue_event_count(u64::MAX - 12),
        );
        let additional = MetricsDiagnostics::new()
            .with_network_interface_metrics(network_metrics_with_all_fields());

        assert_eq!(
            base.merged_with(additional).network_interface_metrics(),
            Some(
                NetworkInterfaceMetrics::default()
                    .with_event_fails(u64::MAX)
                    .with_rx_queue_event_count(u64::MAX)
                    .with_rx_bytes_count(u64::MAX)
                    .with_rx_packets_count(u64::MAX)
                    .with_rx_fails(u64::MAX)
                    .with_rx_count(u64::MAX)
                    .with_tx_bytes_count(u64::MAX)
                    .with_tx_malformed_frames(u64::MAX)
                    .with_tx_fails(u64::MAX)
                    .with_tx_count(u64::MAX)
                    .with_tx_packets_count(u64::MAX)
                    .with_tx_queue_event_count(u64::MAX)
            )
        );
    }

    #[test]
    fn network_diagnostics_merge_per_interface_metrics_saturates() {
        let base = MetricsDiagnostics::new().with_network_interface_metrics_by_interface(
            NetworkInterfaceMetricsByInterface::new().with_interface_metrics(
                "eth0",
                NetworkInterfaceMetrics::default()
                    .with_event_fails(u64::MAX - 1)
                    .with_rx_count(u64::MAX - 2),
            ),
        );
        let additional = MetricsDiagnostics::new().with_network_interface_metrics_by_interface(
            NetworkInterfaceMetricsByInterface::new()
                .with_interface_metrics("eth0", network_metrics_with_all_fields())
                .with_interface_metrics(
                    "eth1",
                    NetworkInterfaceMetrics::default().with_tx_count(3),
                ),
        );
        let expected = NetworkInterfaceMetricsByInterface::new()
            .with_interface_metrics(
                "eth0",
                NetworkInterfaceMetrics::default()
                    .with_event_fails(u64::MAX)
                    .with_rx_queue_event_count(2)
                    .with_rx_bytes_count(3)
                    .with_rx_packets_count(4)
                    .with_rx_fails(5)
                    .with_rx_count(u64::MAX)
                    .with_tx_bytes_count(7)
                    .with_tx_malformed_frames(8)
                    .with_tx_fails(9)
                    .with_tx_count(10)
                    .with_tx_packets_count(11)
                    .with_tx_queue_event_count(12),
            )
            .with_interface_metrics("eth1", NetworkInterfaceMetrics::default().with_tx_count(3));
        let merged = base.merged_with(additional);

        assert_eq!(
            merged.network_interface_metrics_by_interface(),
            Some(&expected)
        );
    }

    #[test]
    fn writes_vsock_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_vsock_device_metrics(vsock_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"vmm":{"metrics_flush_count":1},"vsock":{"activate_fails":1,"cfg_fails":2,"conn_event_fails":7,"conns_added":14,"conns_killed":15,"conns_removed":16,"ev_queue_event_fails":5,"killq_resync":17,"muxer_event_fails":6,"rx_bytes_count":10,"rx_packets_count":12,"rx_queue_event_count":8,"rx_queue_event_fails":3,"rx_read_fails":20,"tx_bytes_count":11,"tx_flush_fails":18,"tx_packets_count":13,"tx_queue_event_count":9,"tx_queue_event_fails":4,"tx_write_fails":19}}"#
            ]
        );
    }

    #[test]
    fn omits_empty_vsock_device_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_vsock_device_metrics(VsockDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(output.lines(), [r#"{"vmm":{"metrics_flush_count":1}}"#]);
    }

    #[test]
    fn shared_vsock_device_metrics_snapshot_is_per_instance() {
        let first = SharedVsockDeviceMetrics::default();
        let second = SharedVsockDeviceMetrics::default();

        first.record_activation_failure();
        first.record_config_failure();
        first.record_muxer_event_failure();

        assert_eq!(
            first.snapshot(),
            VsockDeviceMetrics::default()
                .with_activate_fails(1)
                .with_cfg_fails(1)
                .with_muxer_event_fails(1)
        );
        assert_eq!(second.snapshot(), VsockDeviceMetrics::default());
    }

    #[test]
    fn vsock_metric_increment_saturates() {
        let metrics = SharedVsockDeviceMetrics::default();
        metrics
            .inner
            .rx_queue_event_count
            .store(u64::MAX - 1, Ordering::Relaxed);

        metrics.record_rx_queue_events(3);

        assert_eq!(metrics.snapshot().rx_queue_event_count(), u64::MAX);
    }

    #[test]
    fn vsock_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_vsock_device_metrics(
            VsockDeviceMetrics::default()
                .with_activate_fails(u64::MAX - 1)
                .with_cfg_fails(u64::MAX - 2)
                .with_rx_queue_event_fails(u64::MAX - 3)
                .with_tx_queue_event_fails(u64::MAX - 4)
                .with_ev_queue_event_fails(u64::MAX - 5)
                .with_muxer_event_fails(u64::MAX - 6)
                .with_conn_event_fails(u64::MAX - 7)
                .with_rx_queue_event_count(u64::MAX - 8)
                .with_tx_queue_event_count(u64::MAX - 9)
                .with_rx_bytes_count(u64::MAX - 10)
                .with_tx_bytes_count(u64::MAX - 11)
                .with_rx_packets_count(u64::MAX - 12)
                .with_tx_packets_count(u64::MAX - 13)
                .with_conns_added(u64::MAX - 14)
                .with_conns_killed(u64::MAX - 15)
                .with_conns_removed(u64::MAX - 16)
                .with_killq_resync(u64::MAX - 17)
                .with_tx_flush_fails(u64::MAX - 18)
                .with_tx_write_fails(u64::MAX - 19)
                .with_rx_read_fails(u64::MAX - 20),
        );
        let additional =
            MetricsDiagnostics::new().with_vsock_device_metrics(vsock_metrics_with_all_fields());

        assert_eq!(
            base.merged_with(additional).vsock_device_metrics(),
            Some(
                VsockDeviceMetrics::default()
                    .with_activate_fails(u64::MAX)
                    .with_cfg_fails(u64::MAX)
                    .with_rx_queue_event_fails(u64::MAX)
                    .with_tx_queue_event_fails(u64::MAX)
                    .with_ev_queue_event_fails(u64::MAX)
                    .with_muxer_event_fails(u64::MAX)
                    .with_conn_event_fails(u64::MAX)
                    .with_rx_queue_event_count(u64::MAX)
                    .with_tx_queue_event_count(u64::MAX)
                    .with_rx_bytes_count(u64::MAX)
                    .with_tx_bytes_count(u64::MAX)
                    .with_rx_packets_count(u64::MAX)
                    .with_tx_packets_count(u64::MAX)
                    .with_conns_added(u64::MAX)
                    .with_conns_killed(u64::MAX)
                    .with_conns_removed(u64::MAX)
                    .with_killq_resync(u64::MAX)
                    .with_tx_flush_fails(u64::MAX)
                    .with_tx_write_fails(u64::MAX)
                    .with_rx_read_fails(u64::MAX)
            )
        );
    }

    #[test]
    fn writes_entropy_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new()
            .with_entropy_device_metrics(entropy_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            output.lines(),
            [
                r#"{"entropy":{"activate_fails":1,"entropy_bytes":4,"entropy_event_count":3,"entropy_event_fails":2,"entropy_rate_limiter_throttled":6,"host_rng_fails":5,"rate_limiter_event_count":7},"vmm":{"metrics_flush_count":1}}"#
            ]
        );
    }

    #[test]
    fn omits_empty_entropy_device_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_entropy_device_metrics(EntropyDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(output.lines(), [r#"{"vmm":{"metrics_flush_count":1}}"#]);
    }

    #[test]
    fn shared_entropy_device_metrics_snapshot_is_per_instance() {
        let first = SharedEntropyDeviceMetrics::default();
        let second = SharedEntropyDeviceMetrics::default();

        first.record_activation_failure();
        first.record_event_failure();
        first.record_entropy_source_provider_failure();

        assert_eq!(
            first.snapshot(),
            EntropyDeviceMetrics::default()
                .with_activate_fails(1)
                .with_entropy_event_fails(2)
                .with_host_rng_fails(1)
        );
        assert_eq!(second.snapshot(), EntropyDeviceMetrics::default());
    }

    #[test]
    fn entropy_metric_increment_saturates() {
        let metrics = SharedEntropyDeviceMetrics::default();
        metrics
            .inner
            .entropy_event_count
            .store(u64::MAX - 1, Ordering::Relaxed);

        metrics.record_entropy_events(3);

        assert_eq!(metrics.snapshot().entropy_event_count(), u64::MAX);
    }

    #[test]
    fn entropy_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_entropy_device_metrics(
            EntropyDeviceMetrics::default()
                .with_activate_fails(u64::MAX - 1)
                .with_entropy_event_fails(u64::MAX - 2)
                .with_entropy_event_count(u64::MAX - 3)
                .with_entropy_bytes(u64::MAX - 4)
                .with_host_rng_fails(u64::MAX - 5)
                .with_entropy_rate_limiter_throttled(u64::MAX - 6)
                .with_rate_limiter_event_count(u64::MAX - 7),
        );
        let additional = MetricsDiagnostics::new()
            .with_entropy_device_metrics(entropy_metrics_with_all_fields());

        assert_eq!(
            base.merged_with(additional).entropy_device_metrics(),
            Some(
                EntropyDeviceMetrics::default()
                    .with_activate_fails(u64::MAX)
                    .with_entropy_event_fails(u64::MAX)
                    .with_entropy_event_count(u64::MAX)
                    .with_entropy_bytes(u64::MAX)
                    .with_host_rng_fails(u64::MAX)
                    .with_entropy_rate_limiter_throttled(u64::MAX)
                    .with_rate_limiter_event_count(u64::MAX)
            )
        );
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
