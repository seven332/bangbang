use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, LineWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::balloon::{VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDiscardOutcome};
use crate::block::{
    VirtioBlockDeviceNotificationDispatch, VirtioBlockLatencyAggregate, VirtioBlockQueueDispatch,
};
use crate::entropy::{
    VirtioRngDeviceNotificationDispatch, VirtioRngDeviceNotificationError, VirtioRngQueueDispatch,
};
use crate::logger::SharedLoggerMetrics;
use crate::network::{
    VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX, VirtioNetworkBackendMetrics,
    VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError,
    VirtioNetworkLatencyAggregate, VirtioNetworkRxQueueDispatch, VirtioNetworkTxQueueDispatch,
};
use crate::pmem::{
    VirtioPmemDeviceNotificationDispatch, VirtioPmemDeviceNotificationError,
    VirtioPmemQueueDispatch,
};
use crate::serial::SerialOutputMetrics;
use crate::vsock::{
    VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX,
    VirtioVsockDeviceNotificationDispatch, VirtioVsockDeviceNotificationError,
    VirtioVsockRxQueueDispatch, VirtioVsockTransportResetAttempt, VirtioVsockTransportResetError,
    VirtioVsockTxQueueDispatch,
};

mod firecracker;

/// Maximum number of configured dynamic Firecracker metrics roots in one arm64 VM.
pub const FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS: usize =
    firecracker::FIRECRACKER_METRICS_MAX_DYNAMIC_ROOTS;

/// Maximum combined configured drive and network identity bytes in one metrics attempt.
pub const FIRECRACKER_METRICS_MAX_IDENTITY_BYTES: usize =
    firecracker::FIRECRACKER_METRICS_MAX_IDENTITY_BYTES;

/// Maximum byte length of one complete newline-terminated Firecracker metrics record.
pub const FIRECRACKER_METRICS_MAX_LINE_BYTES: usize =
    firecracker::FIRECRACKER_METRICS_MAX_LINE_BYTES;

/// Bounds collection work without making a producer wait for the collector.
const PROCESS_METRICS_SNAPSHOT_ATTEMPTS: usize = 64;

/// A Firecracker operation with distinct outer API and inner VMM latency stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLatencyOperation {
    /// Create a full snapshot.
    FullCreateSnapshot,
    /// Create a differential snapshot.
    DiffCreateSnapshot,
    /// Load a snapshot.
    LoadSnapshot,
    /// Pause the VM.
    PauseVm,
    /// Resume the VM.
    ResumeVm,
}

/// The semantic owner of one process-operation latency measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLatencyBoundary {
    /// The complete successful API operation before response publication.
    OuterApi,
    /// The corresponding successful VMM action before outcome logging.
    InnerVmm,
}

const fn incremental_delta(current: u64, previous: u64) -> u64 {
    if current >= previous {
        current - previous
    } else {
        current
    }
}

macro_rules! impl_incremental_delta {
    ($metrics:ident { $($field:ident),+ $(,)? }) => {
        impl $metrics {
            const fn delta_since(self, previous: Self) -> Self {
                Self {
                    $(
                        $field: incremental_delta(self.$field, previous.$field),
                    )+
                }
            }
        }
    };
}

const fn block_latency_delta(
    current: VirtioBlockLatencyAggregate,
    previous: VirtioBlockLatencyAggregate,
) -> VirtioBlockLatencyAggregate {
    VirtioBlockLatencyAggregate::new(
        current.min_us(),
        current.max_us(),
        incremental_delta(current.sum_us(), previous.sum_us()),
        current.sample_count(),
    )
}

#[derive(Clone, PartialEq, Eq)]
pub struct MetricsConfigInput {
    metrics_path: PathBuf,
}

impl fmt::Debug for MetricsConfigInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetricsConfigInput")
            .field("metrics_path", &"<redacted>")
            .finish()
    }
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

#[derive(Clone, PartialEq, Eq)]
pub struct MetricsConfig {
    metrics_path: PathBuf,
}

impl fmt::Debug for MetricsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetricsConfig")
            .field("metrics_path", &"<redacted>")
            .finish()
    }
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
    ProcessSnapshotBusy,
    ProcessGenerationExhausted,
    Clock,
    ConfiguredDevices,
    Allocation,
    Serialization,
    LineTooLong,
    Write(std::io::ErrorKind),
    Newline(std::io::ErrorKind),
    Flush(std::io::ErrorKind),
}

impl fmt::Display for MetricsFlushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProcessSnapshotBusy => {
                f.write_str("failed to flush metrics: process snapshot remained busy")
            }
            Self::ProcessGenerationExhausted => {
                f.write_str("failed to flush metrics: process generation exhausted")
            }
            Self::Clock => f.write_str("failed to flush metrics: clock unavailable"),
            Self::ConfiguredDevices => {
                f.write_str("failed to flush metrics: configured device inventory is invalid")
            }
            Self::Allocation => f.write_str("failed to flush metrics: allocation failed"),
            Self::Serialization => f.write_str("failed to flush metrics: serialization failed"),
            Self::LineTooLong => f.write_str("failed to flush metrics: line exceeds size limit"),
            Self::Write(kind) => write!(f, "failed to flush metrics: {kind:?}"),
            Self::Newline(kind) => write!(f, "failed to terminate metrics line: {kind:?}"),
            Self::Flush(kind) => write!(f, "failed to publish metrics line: {kind:?}"),
        }
    }
}

impl std::error::Error for MetricsFlushError {}

impl From<firecracker::MetricsLineBuildError> for MetricsFlushError {
    fn from(error: firecracker::MetricsLineBuildError) -> Self {
        match error {
            firecracker::MetricsLineBuildError::Clock => Self::Clock,
            firecracker::MetricsLineBuildError::TooManyConfiguredDevices
            | firecracker::MetricsLineBuildError::TooManyNetworkInterfaces
            | firecracker::MetricsLineBuildError::ConfiguredIdentityBytes
            | firecracker::MetricsLineBuildError::DuplicateConfiguredIdentity => {
                Self::ConfiguredDevices
            }
            firecracker::MetricsLineBuildError::Allocation => Self::Allocation,
            firecracker::MetricsLineBuildError::Serialization => Self::Serialization,
            firecracker::MetricsLineBuildError::LineTooLong => Self::LineTooLong,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MetricsSnapshot {
    generation: u64,
    panic_count: u64,
    diagnostics: MetricsDiagnostics,
    deprecated_api: DeprecatedApiMetrics,
    get_api_requests: GetApiRequestMetrics,
    latencies_us: LatencyMetrics,
    logger_metrics: LoggerMetrics,
    patch_api_requests: PatchApiRequestMetrics,
    put_api_requests: PutApiRequestMetrics,
}

impl MetricsSnapshot {
    fn delta_since(&self, previous: &Self) -> Self {
        Self {
            generation: self.generation,
            panic_count: self.panic_count,
            diagnostics: self.diagnostics.delta_since(&previous.diagnostics),
            deprecated_api: self.deprecated_api.delta_since(previous.deprecated_api),
            get_api_requests: self.get_api_requests.delta_since(previous.get_api_requests),
            latencies_us: self.latencies_us,
            logger_metrics: self.logger_metrics.delta_since(previous.logger_metrics),
            patch_api_requests: self
                .patch_api_requests
                .delta_since(previous.patch_api_requests),
            put_api_requests: self.put_api_requests.delta_since(previous.put_api_requests),
        }
    }
}

#[derive(Debug)]
pub struct MetricsState {
    clock: Box<dyn firecracker::MetricsClock>,
    deprecated_api: DeprecatedApiMetrics,
    sink: Option<MetricsSink>,
    get_api_requests: GetApiRequestMetrics,
    latencies_us: LatencyMetrics,
    logger_metrics: LoggerMetrics,
    process_generation: u64,
    previous_successful: MetricsSnapshot,
    serializer: Box<dyn firecracker::MetricsLineSerializer>,
    shared_process_metrics: SharedProcessMetrics,
    patch_api_requests: PatchApiRequestMetrics,
    put_api_requests: PutApiRequestMetrics,
}

impl Default for MetricsState {
    fn default() -> Self {
        Self {
            clock: Box::<firecracker::SystemMetricsClock>::default(),
            deprecated_api: DeprecatedApiMetrics::default(),
            sink: None,
            get_api_requests: GetApiRequestMetrics::default(),
            latencies_us: LatencyMetrics::default(),
            logger_metrics: LoggerMetrics::default(),
            process_generation: 0,
            previous_successful: MetricsSnapshot::default(),
            serializer: Box::<firecracker::SystemMetricsLineSerializer>::default(),
            shared_process_metrics: SharedProcessMetrics::default(),
            patch_api_requests: PatchApiRequestMetrics::default(),
            put_api_requests: PutApiRequestMetrics::default(),
        }
    }
}

/// A fully validated metrics configuration with a ready output sink.
pub struct PreparedMetricsConfig {
    sink: MetricsSink,
}

impl fmt::Debug for PreparedMetricsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedMetricsConfig")
            .field("sink", &"<owned>")
            .finish()
    }
}

impl MetricsState {
    pub(crate) fn with_shared_process_metrics(
        shared_process_metrics: SharedProcessMetrics,
    ) -> Self {
        Self {
            shared_process_metrics,
            ..Self::default()
        }
    }

    pub(crate) fn signal_metrics(&self) -> SharedSignalMetrics {
        self.shared_process_metrics.signal_metrics()
    }

    pub(crate) fn record_process_panic(&self) {
        self.shared_process_metrics.record_process_panic();
    }

    pub fn configure(&mut self, input: MetricsConfigInput) -> Result<(), MetricsConfigError> {
        let config = self.validate_config(input)?;
        let prepared = Self::prepare_config(config, None)?;
        self.commit_config(prepared);

        Ok(())
    }

    /// Validates a metrics request without opening or installing its sink.
    pub fn validate_config(
        &self,
        input: MetricsConfigInput,
    ) -> Result<MetricsConfig, MetricsConfigError> {
        if self.sink.is_some() {
            return Err(MetricsConfigError::AlreadyInitialized);
        }

        input.validate()
    }

    /// Prepares a validated metrics sink without mutating process metrics state.
    pub fn prepare_config(
        config: MetricsConfig,
        provided_file: Option<File>,
    ) -> Result<PreparedMetricsConfig, MetricsConfigError> {
        let sink = match provided_file {
            Some(file) => MetricsSink::from_file(file)?,
            None => MetricsSink::open(&config)?,
        };

        Ok(PreparedMetricsConfig { sink })
    }

    /// Installs a prepared sink without further fallible work.
    pub fn commit_config(&mut self, prepared: PreparedMetricsConfig) {
        debug_assert!(self.sink.is_none());
        self.sink = Some(prepared.sink);
    }

    pub fn flush(&mut self) -> Result<bool, MetricsFlushError> {
        self.flush_with_diagnostics(&MetricsDiagnostics::default())
    }

    pub(crate) fn record_deprecated_api_call(&mut self) {
        self.deprecated_api.record_deprecated_http_api_call();
    }

    pub(crate) fn record_process_latency_us(
        &mut self,
        operation: ProcessLatencyOperation,
        boundary: ProcessLatencyBoundary,
        duration_us: u64,
    ) {
        self.latencies_us.record(operation, boundary, duration_us);
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

    pub fn flush_with_diagnostics(
        &mut self,
        diagnostics: &MetricsDiagnostics,
    ) -> Result<bool, MetricsFlushError> {
        self.flush_with_diagnostics_and_devices(
            diagnostics,
            &firecracker::ConfiguredMetricsDevices::default(),
        )
    }

    pub(crate) fn flush_with_diagnostics_and_devices(
        &mut self,
        diagnostics: &MetricsDiagnostics,
        configured: &firecracker::ConfiguredMetricsDevices,
    ) -> Result<bool, MetricsFlushError> {
        if self.sink.is_none() {
            return Ok(false);
        }
        let Some(generation) = self.process_generation.checked_add(1) else {
            self.logger_metrics.record_missed_metrics();
            return Err(MetricsFlushError::ProcessGenerationExhausted);
        };
        let process_metrics = match self.shared_process_metrics.stable_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.logger_metrics.record_missed_metrics();
                return Err(error);
            }
        };
        self.process_generation = generation;
        let current = MetricsSnapshot {
            generation,
            panic_count: process_metrics.panic_count,
            diagnostics: diagnostics.clone().merged_with(
                MetricsDiagnostics::new().with_signal_metrics(process_metrics.signal_metrics),
            ),
            deprecated_api: self.deprecated_api,
            get_api_requests: self.get_api_requests,
            latencies_us: self.latencies_us,
            logger_metrics: self.logger_metrics.with_log_counts(
                process_metrics.missed_log_count,
                process_metrics.rate_limited_log_count,
            ),
            patch_api_requests: self.patch_api_requests,
            put_api_requests: self.put_api_requests,
        };
        let interval = current.delta_since(&self.previous_successful);
        let result = (|| {
            let timestamp = firecracker::unix_timestamp_ms(self.clock.as_ref())?;
            let line = firecracker::build_metrics_line(timestamp, &current, &interval, configured)?;
            let bytes = self.serializer.serialize(&line)?;
            let Some(sink) = self.sink.as_mut() else {
                return Ok(());
            };
            sink.write_metrics_line(&bytes)
        })();
        if let Err(err) = result {
            // The sink can report an error after some bytes became visible. Retaining the prior
            // successful baseline intentionally gives consumers at-least-once replay.
            self.logger_metrics.record_missed_metrics();
            return Err(err);
        }
        self.previous_successful = current;

        Ok(true)
    }

    pub(crate) fn flush_with_diagnostics_and_configs(
        &mut self,
        diagnostics: &MetricsDiagnostics,
        drives: &[crate::block::DriveConfig],
        networks: &[crate::network::NetworkInterfaceConfig],
    ) -> Result<bool, MetricsFlushError> {
        if self.sink.is_none() {
            return Ok(false);
        }
        let configured =
            match firecracker::ConfiguredMetricsDevices::try_from_configs(drives, networks) {
                Ok(configured) => configured,
                Err(error) => {
                    self.logger_metrics.record_missed_metrics();
                    return Err(MetricsFlushError::from(error));
                }
            };
        self.flush_with_diagnostics_and_devices(diagnostics, &configured)
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

impl_incremental_delta!(DeprecatedApiMetrics {
    deprecated_http_api_calls,
});

impl DeprecatedApiMetrics {
    fn record_deprecated_http_api_call(&mut self) {
        self.deprecated_http_api_calls = self.deprecated_http_api_calls.saturating_add(1);
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

impl_incremental_delta!(GetApiRequestMetrics {
    balloon_count,
    hotplug_memory_count,
    instance_info_count,
    vmm_version_count,
    machine_cfg_count,
    mmds_count,
});

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LoggerMetrics {
    missed_log_count: u64,
    missed_metrics_count: u64,
    rate_limited_log_count: u64,
}

impl_incremental_delta!(LoggerMetrics {
    missed_log_count,
    missed_metrics_count,
    rate_limited_log_count,
});

impl LoggerMetrics {
    const fn with_log_counts(mut self, missed_log_count: u64, rate_limited_log_count: u64) -> Self {
        self.missed_log_count = missed_log_count;
        self.rate_limited_log_count = rate_limited_log_count;
        self
    }

    fn record_missed_metrics(&mut self) {
        self.missed_metrics_count = self.missed_metrics_count.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LatencyMetrics {
    full_create_snapshot: Option<u64>,
    diff_create_snapshot: Option<u64>,
    load_snapshot: Option<u64>,
    pause_vm: Option<u64>,
    resume_vm: Option<u64>,
    vmm_full_create_snapshot: Option<u64>,
    vmm_diff_create_snapshot: Option<u64>,
    vmm_load_snapshot: Option<u64>,
    vmm_pause_vm: Option<u64>,
    vmm_resume_vm: Option<u64>,
}

impl LatencyMetrics {
    fn record(
        &mut self,
        operation: ProcessLatencyOperation,
        boundary: ProcessLatencyBoundary,
        duration_us: u64,
    ) {
        let destination = match (operation, boundary) {
            (ProcessLatencyOperation::FullCreateSnapshot, ProcessLatencyBoundary::OuterApi) => {
                &mut self.full_create_snapshot
            }
            (ProcessLatencyOperation::DiffCreateSnapshot, ProcessLatencyBoundary::OuterApi) => {
                &mut self.diff_create_snapshot
            }
            (ProcessLatencyOperation::LoadSnapshot, ProcessLatencyBoundary::OuterApi) => {
                &mut self.load_snapshot
            }
            (ProcessLatencyOperation::PauseVm, ProcessLatencyBoundary::OuterApi) => {
                &mut self.pause_vm
            }
            (ProcessLatencyOperation::ResumeVm, ProcessLatencyBoundary::OuterApi) => {
                &mut self.resume_vm
            }
            (ProcessLatencyOperation::FullCreateSnapshot, ProcessLatencyBoundary::InnerVmm) => {
                &mut self.vmm_full_create_snapshot
            }
            (ProcessLatencyOperation::DiffCreateSnapshot, ProcessLatencyBoundary::InnerVmm) => {
                &mut self.vmm_diff_create_snapshot
            }
            (ProcessLatencyOperation::LoadSnapshot, ProcessLatencyBoundary::InnerVmm) => {
                &mut self.vmm_load_snapshot
            }
            (ProcessLatencyOperation::PauseVm, ProcessLatencyBoundary::InnerVmm) => {
                &mut self.vmm_pause_vm
            }
            (ProcessLatencyOperation::ResumeVm, ProcessLatencyBoundary::InnerVmm) => {
                &mut self.vmm_resume_vm
            }
        };
        *destination = Some(duration_us);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SignalMetrics {
    sigxfsz: u64,
    sigxcpu: u64,
    sigpipe: u64,
    sighup: u64,
}

impl SignalMetrics {
    pub const fn new(sigpipe: u64) -> Self {
        Self {
            sigxfsz: 0,
            sigxcpu: 0,
            sigpipe,
            sighup: 0,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.sigxfsz == 0 && self.sigxcpu == 0 && self.sigpipe == 0 && self.sighup == 0
    }

    pub const fn sigxfsz(self) -> u64 {
        self.sigxfsz
    }

    pub const fn sigxcpu(self) -> u64 {
        self.sigxcpu
    }

    pub const fn sigpipe(self) -> u64 {
        self.sigpipe
    }

    pub const fn sighup(self) -> u64 {
        self.sighup
    }

    fn delta_since(self, previous: Self) -> Self {
        Self {
            sigxfsz: self.sigxfsz,
            sigxcpu: self.sigxcpu,
            sigpipe: self.sigpipe.saturating_sub(previous.sigpipe),
            sighup: self.sighup,
        }
    }

    fn merged_with(self, other: Self) -> Self {
        Self {
            sigxfsz: self.sigxfsz.max(other.sigxfsz),
            sigxcpu: self.sigxcpu.max(other.sigxcpu),
            sigpipe: self.sigpipe.saturating_add(other.sigpipe),
            sighup: self.sighup.max(other.sighup),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedSignalMetrics {
    inner: Arc<SharedSignalMetricsInner>,
}

impl SharedSignalMetrics {
    pub fn record_sigxfsz(&self) {
        self.inner.sigxfsz.store(1, Ordering::SeqCst);
    }

    pub fn record_sigxcpu(&self) {
        self.inner.sigxcpu.store(1, Ordering::SeqCst);
    }

    pub fn record_sigpipe(&self) {
        record_atomic_metric_seq_cst(&self.inner.sigpipe, 1);
    }

    pub fn record_sighup(&self) {
        self.inner.sighup.store(1, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> SignalMetrics {
        SignalMetrics {
            sigxfsz: self.inner.sigxfsz.load(Ordering::SeqCst),
            sigxcpu: self.inner.sigxcpu.load(Ordering::SeqCst),
            sigpipe: self.inner.sigpipe.load(Ordering::SeqCst),
            sighup: self.inner.sighup.load(Ordering::SeqCst),
        }
    }

    #[cfg(test)]
    fn set_sigpipe_for_test(&self, sigpipe: u64) {
        self.inner.sigpipe.store(sigpipe, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
struct SharedSignalMetricsInner {
    sigxfsz: AtomicU64,
    sigxcpu: AtomicU64,
    sigpipe: AtomicU64,
    sighup: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SharedProcessMetricsSnapshot {
    missed_log_count: u64,
    rate_limited_log_count: u64,
    signal_metrics: SignalMetrics,
    panic_count: u64,
}

#[cfg(test)]
type ProcessMetricsScanHook = Arc<dyn Fn(usize) + Send + Sync>;

#[derive(Clone, Default)]
pub(crate) struct SharedProcessMetrics {
    logger_metrics: SharedLoggerMetrics,
    signal_metrics: SharedSignalMetrics,
    panic_count: Arc<AtomicU64>,
    #[cfg(test)]
    scan_hook: Option<ProcessMetricsScanHook>,
}

impl fmt::Debug for SharedProcessMetrics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedProcessMetrics")
            .field("logger_metrics", &"<shared>")
            .field("signal_metrics", &"<shared>")
            .field("panic_count", &"<shared>")
            .finish()
    }
}

impl SharedProcessMetrics {
    pub(crate) fn logger_metrics(&self) -> SharedLoggerMetrics {
        self.logger_metrics.clone()
    }

    pub(crate) fn signal_metrics(&self) -> SharedSignalMetrics {
        self.signal_metrics.clone()
    }

    fn record_process_panic(&self) {
        self.panic_count.store(1, Ordering::SeqCst);
    }

    fn read_snapshot(&self) -> SharedProcessMetricsSnapshot {
        SharedProcessMetricsSnapshot {
            missed_log_count: self.logger_metrics.missed_log_count(),
            rate_limited_log_count: self.logger_metrics.rate_limited_log_count(),
            signal_metrics: self.signal_metrics.snapshot(),
            panic_count: self.panic_count.load(Ordering::SeqCst),
        }
    }

    fn stable_snapshot(&self) -> Result<SharedProcessMetricsSnapshot, MetricsFlushError> {
        for _attempt in 0..PROCESS_METRICS_SNAPSHOT_ATTEMPTS {
            let first = self.read_snapshot();
            #[cfg(test)]
            if let Some(hook) = &self.scan_hook {
                hook(_attempt);
            }
            // Every producer update and both fixed-order scans are SeqCst. Equal monotonic
            // vectors therefore prove that every process value existed together at this fence.
            fence(Ordering::SeqCst);
            let second = self.read_snapshot();
            if first == second {
                return Ok(second);
            }
        }

        Err(MetricsFlushError::ProcessSnapshotBusy)
    }

    #[cfg(test)]
    fn set_scan_hook(&mut self, hook: impl Fn(usize) + Send + Sync + 'static) {
        self.scan_hook = Some(Arc::new(hook));
    }

    #[cfg(test)]
    fn clear_scan_hook(&mut self) {
        self.scan_hook = None;
    }
}

impl GetApiRequestMetrics {
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

impl_incremental_delta!(PatchApiRequestMetrics {
    balloon_count,
    balloon_fails,
    drive_count,
    drive_fails,
    network_count,
    network_fails,
    machine_cfg_count,
    machine_cfg_fails,
    mmds_count,
    mmds_fails,
    hotplug_memory_count,
    hotplug_memory_fails,
    pmem_count,
    pmem_fails,
});

impl PatchApiRequestMetrics {
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

impl_incremental_delta!(PutApiRequestMetrics {
    actions_count,
    actions_fails,
    balloon_count,
    balloon_fails,
    boot_source_count,
    boot_source_fails,
    cpu_cfg_count,
    cpu_cfg_fails,
    drive_count,
    drive_fails,
    logger_count,
    logger_fails,
    machine_cfg_count,
    machine_cfg_fails,
    metrics_count,
    metrics_fails,
    hotplug_memory_count,
    hotplug_memory_fails,
    mmds_count,
    mmds_fails,
    network_count,
    network_fails,
    pmem_count,
    pmem_fails,
    serial_count,
    serial_fails,
    vsock_count,
    vsock_fails,
});

impl PutApiRequestMetrics {
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
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockDeviceMetrics {
    config_change_time_us: Option<u64>,
    event_fails: u64,
    execute_fails: u64,
    invalid_reqs_count: u64,
    flush_count: u64,
    queue_event_count: u64,
    rate_limiter_event_count: u64,
    rate_limiter_throttled_events: u64,
    io_engine_throttled_events: u64,
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
    fn delta_since(self, previous: Self) -> Self {
        Self {
            config_change_time_us: if self.config_change_time_us == previous.config_change_time_us {
                None
            } else {
                self.config_change_time_us
            },
            event_fails: incremental_delta(self.event_fails, previous.event_fails),
            execute_fails: incremental_delta(self.execute_fails, previous.execute_fails),
            invalid_reqs_count: incremental_delta(
                self.invalid_reqs_count,
                previous.invalid_reqs_count,
            ),
            flush_count: incremental_delta(self.flush_count, previous.flush_count),
            queue_event_count: incremental_delta(
                self.queue_event_count,
                previous.queue_event_count,
            ),
            rate_limiter_event_count: incremental_delta(
                self.rate_limiter_event_count,
                previous.rate_limiter_event_count,
            ),
            rate_limiter_throttled_events: incremental_delta(
                self.rate_limiter_throttled_events,
                previous.rate_limiter_throttled_events,
            ),
            io_engine_throttled_events: incremental_delta(
                self.io_engine_throttled_events,
                previous.io_engine_throttled_events,
            ),
            update_count: incremental_delta(self.update_count, previous.update_count),
            update_fails: incremental_delta(self.update_fails, previous.update_fails),
            read_bytes: incremental_delta(self.read_bytes, previous.read_bytes),
            write_bytes: incremental_delta(self.write_bytes, previous.write_bytes),
            read_count: incremental_delta(self.read_count, previous.read_count),
            write_count: incremental_delta(self.write_count, previous.write_count),
            read_agg: block_latency_delta(self.read_agg, previous.read_agg),
            write_agg: block_latency_delta(self.write_agg, previous.write_agg),
        }
    }

    pub const fn is_empty(self) -> bool {
        self.config_change_time_us.is_none()
            && self.event_fails == 0
            && self.execute_fails == 0
            && self.invalid_reqs_count == 0
            && self.flush_count == 0
            && self.queue_event_count == 0
            && self.rate_limiter_event_count == 0
            && self.rate_limiter_throttled_events == 0
            && self.io_engine_throttled_events == 0
            && self.update_count == 0
            && self.update_fails == 0
            && self.read_bytes == 0
            && self.write_bytes == 0
            && self.read_count == 0
            && self.write_count == 0
            && self.read_agg.is_empty()
            && self.write_agg.is_empty()
    }

    pub const fn config_change_time_us(self) -> Option<u64> {
        self.config_change_time_us
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

    pub const fn io_engine_throttled_events(self) -> u64 {
        self.io_engine_throttled_events
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

    pub const fn with_config_change_time_us(mut self, config_change_time_us: u64) -> Self {
        self.config_change_time_us = Some(config_change_time_us);
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

    pub const fn with_io_engine_throttled_events(
        mut self,
        io_engine_throttled_events: u64,
    ) -> Self {
        self.io_engine_throttled_events = io_engine_throttled_events;
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
            config_change_time_us: match other.config_change_time_us {
                Some(value) => Some(value),
                None => self.config_change_time_us,
            },
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
            io_engine_throttled_events: self
                .io_engine_throttled_events
                .saturating_add(other.io_engine_throttled_events),
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

    fn delta_since(&self, previous: Option<&Self>) -> Self {
        let metrics = self
            .metrics
            .iter()
            .map(|(drive_id, current)| {
                let previous = previous
                    .and_then(|metrics| metrics.metrics.get(drive_id))
                    .copied()
                    .unwrap_or_default();
                (drive_id.clone(), current.delta_since(previous))
            })
            .collect();
        Self { metrics }
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
        self.record_io_engine_throttled_events(usize_to_u64_saturating(
            dispatch.io_engine_throttled_events(),
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

    pub fn record_config_change_time_us(&self, duration_us: u64) {
        self.inner
            .config_change_time_us
            .store(duration_us, Ordering::Relaxed);
        self.inner
            .config_change_time_recorded
            .store(true, Ordering::Release);
    }

    pub fn snapshot(&self) -> BlockDeviceMetrics {
        BlockDeviceMetrics {
            config_change_time_us: self
                .inner
                .config_change_time_recorded
                .load(Ordering::Acquire)
                .then(|| self.inner.config_change_time_us.load(Ordering::Relaxed)),
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
            io_engine_throttled_events: self
                .inner
                .io_engine_throttled_events
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

    fn record_io_engine_throttled_events(&self, count: u64) {
        if count != 0 {
            record_atomic_metric(&self.inner.io_engine_throttled_events, count);
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
    config_change_time_recorded: AtomicBool,
    config_change_time_us: AtomicU64,
    event_fails: AtomicU64,
    execute_fails: AtomicU64,
    invalid_reqs_count: AtomicU64,
    flush_count: AtomicU64,
    queue_event_count: AtomicU64,
    rate_limiter_event_count: AtomicU64,
    rate_limiter_throttled_events: AtomicU64,
    io_engine_throttled_events: AtomicU64,
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
            config_change_time_recorded: AtomicBool::new(false),
            config_change_time_us: AtomicU64::new(0),
            event_fails: AtomicU64::new(0),
            execute_fails: AtomicU64::new(0),
            invalid_reqs_count: AtomicU64::new(0),
            flush_count: AtomicU64::new(0),
            queue_event_count: AtomicU64::new(0),
            rate_limiter_event_count: AtomicU64::new(0),
            rate_limiter_throttled_events: AtomicU64::new(0),
            io_engine_throttled_events: AtomicU64::new(0),
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

#[derive(Debug, Clone)]
pub struct SharedBlockDeviceMetricsRegistry {
    aggregate: SharedBlockDeviceMetrics,
    per_drive: Arc<Mutex<BlockDeviceMetricsRegistryState>>,
}

#[derive(Debug, Default)]
struct BlockDeviceMetricsRegistryState {
    entries: Vec<BlockDeviceMetricsRegistryEntry>,
    reservations: Vec<BlockDeviceMetricsReservation>,
    next_generation: u64,
    capacity: usize,
}

#[derive(Debug)]
struct BlockDeviceMetricsRegistryEntry {
    generation: u64,
    drive_id: String,
    metrics: SharedBlockDeviceMetrics,
    lease_claimed: bool,
}

#[derive(Debug)]
struct BlockDeviceMetricsReservation {
    generation: u64,
    drive_id: String,
}

/// Prepared per-drive metrics ownership that is not visible until publication.
pub struct PreparedBlockDeviceMetrics {
    registry: SharedBlockDeviceMetricsRegistry,
    generation: u64,
    drive_id: String,
    metrics: SharedBlockDeviceMetrics,
    reserved: bool,
}

/// Exact live per-drive metrics ownership removed automatically on drop.
pub struct BlockDeviceMetricsLease {
    registry: SharedBlockDeviceMetricsRegistry,
    generation: u64,
    drive_id: String,
    registered: bool,
}

impl fmt::Debug for BlockDeviceMetricsLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockDeviceMetricsLease")
            .field("ownership", &"<redacted>")
            .field("registered", &self.registered)
            .finish()
    }
}

impl fmt::Debug for PreparedBlockDeviceMetrics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedBlockDeviceMetrics")
            .field("ownership", &"<redacted>")
            .field("reserved", &self.reserved)
            .finish()
    }
}

impl PreparedBlockDeviceMetrics {
    pub fn publish(mut self) -> BlockDeviceMetricsLease {
        let mut state = lock_block_metrics_registry(&self.registry.per_drive);
        let reservation_count = state.reservations.len();
        state.reservations.retain(|reservation| {
            reservation.generation != self.generation || reservation.drive_id != self.drive_id
        });
        debug_assert_eq!(
            state.reservations.len().checked_add(1),
            Some(reservation_count)
        );
        self.reserved = false;
        debug_assert!(state.entries.len() < state.capacity);
        debug_assert!(
            !state
                .entries
                .iter()
                .any(|entry| entry.drive_id == self.drive_id)
        );
        state.entries.push(BlockDeviceMetricsRegistryEntry {
            generation: self.generation,
            drive_id: self.drive_id.clone(),
            metrics: self.metrics.clone(),
            lease_claimed: true,
        });
        drop(state);
        BlockDeviceMetricsLease {
            registry: self.registry.clone(),
            generation: self.generation,
            drive_id: self.drive_id.clone(),
            registered: true,
        }
    }
}

impl Drop for PreparedBlockDeviceMetrics {
    fn drop(&mut self) {
        if !self.reserved {
            return;
        }
        let mut state = lock_block_metrics_registry(&self.registry.per_drive);
        if let Some(index) = state.reservations.iter().position(|reservation| {
            reservation.generation == self.generation && reservation.drive_id == self.drive_id
        }) {
            state.reservations.remove(index);
        }
        self.reserved = false;
    }
}

impl Drop for BlockDeviceMetricsLease {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let mut state = lock_block_metrics_registry(&self.registry.per_drive);
        if let Some(index) = state.entries.iter().position(|entry| {
            entry.generation == self.generation && entry.drive_id == self.drive_id
        }) {
            state.entries.remove(index);
        }
        self.registered = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockDeviceMetricsRegistryError {
    DuplicateDrive,
    UnknownDrive,
    LeaseAlreadyClaimed,
    Capacity,
    GenerationExhausted,
}

impl fmt::Display for BlockDeviceMetricsRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDrive => {
                formatter.write_str("block metrics drive is already registered")
            }
            Self::UnknownDrive => formatter.write_str("block metrics drive is not registered"),
            Self::LeaseAlreadyClaimed => {
                formatter.write_str("block metrics drive lease is already claimed")
            }
            Self::Capacity => formatter.write_str("failed to reserve block metrics capacity"),
            Self::GenerationExhausted => {
                formatter.write_str("block metrics ownership generation is exhausted")
            }
        }
    }
}

impl std::error::Error for BlockDeviceMetricsRegistryError {}

impl Default for SharedBlockDeviceMetricsRegistry {
    fn default() -> Self {
        Self {
            aggregate: SharedBlockDeviceMetrics::default(),
            per_drive: Arc::new(Mutex::new(BlockDeviceMetricsRegistryState::default())),
        }
    }
}

impl SharedBlockDeviceMetricsRegistry {
    pub fn from_drive_ids<'a>(drive_ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut entries = Vec::new();
        for drive_id in drive_ids {
            if entries
                .iter()
                .any(|entry: &BlockDeviceMetricsRegistryEntry| entry.drive_id == drive_id)
            {
                continue;
            }
            let generation = u64::try_from(entries.len()).unwrap_or(u64::MAX);
            entries.push(BlockDeviceMetricsRegistryEntry {
                generation,
                drive_id: drive_id.to_string(),
                metrics: SharedBlockDeviceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len()).unwrap_or(u64::MAX);

        Self {
            aggregate: SharedBlockDeviceMetrics::default(),
            per_drive: Arc::new(Mutex::new(BlockDeviceMetricsRegistryState {
                capacity: entries.len(),
                entries,
                reservations: Vec::new(),
                next_generation,
            })),
        }
    }

    pub fn from_drive_ids_with_capacity<'a>(
        drive_ids: impl IntoIterator<Item = &'a str>,
        capacity: usize,
    ) -> Result<Self, BlockDeviceMetricsRegistryError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| BlockDeviceMetricsRegistryError::Capacity)?;
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(capacity)
            .map_err(|_| BlockDeviceMetricsRegistryError::Capacity)?;
        for drive_id in drive_ids {
            if entries
                .iter()
                .any(|entry: &BlockDeviceMetricsRegistryEntry| entry.drive_id == drive_id)
            {
                continue;
            }
            if entries.len() == capacity {
                return Err(BlockDeviceMetricsRegistryError::Capacity);
            }
            let generation = u64::try_from(entries.len())
                .map_err(|_| BlockDeviceMetricsRegistryError::GenerationExhausted)?;
            entries.push(BlockDeviceMetricsRegistryEntry {
                generation,
                drive_id: drive_id.to_string(),
                metrics: SharedBlockDeviceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len())
            .map_err(|_| BlockDeviceMetricsRegistryError::GenerationExhausted)?;
        Ok(Self {
            aggregate: SharedBlockDeviceMetrics::default(),
            per_drive: Arc::new(Mutex::new(BlockDeviceMetricsRegistryState {
                entries,
                reservations,
                next_generation,
                capacity,
            })),
        })
    }

    /// Builds a bounded registry by consuming already-validated owned IDs.
    ///
    /// This avoids allocating a second string vector while an unpublished
    /// restore transaction is acquiring its complete metrics owner.
    #[doc(hidden)]
    pub fn from_owned_drive_ids_with_capacity(
        drive_ids: Vec<String>,
        capacity: usize,
    ) -> Result<Self, BlockDeviceMetricsRegistryError> {
        if drive_ids.len() > capacity {
            return Err(BlockDeviceMetricsRegistryError::Capacity);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| BlockDeviceMetricsRegistryError::Capacity)?;
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(capacity)
            .map_err(|_| BlockDeviceMetricsRegistryError::Capacity)?;
        for drive_id in drive_ids {
            if entries
                .iter()
                .any(|entry: &BlockDeviceMetricsRegistryEntry| entry.drive_id == drive_id)
            {
                return Err(BlockDeviceMetricsRegistryError::DuplicateDrive);
            }
            let generation = u64::try_from(entries.len())
                .map_err(|_| BlockDeviceMetricsRegistryError::GenerationExhausted)?;
            entries.push(BlockDeviceMetricsRegistryEntry {
                generation,
                drive_id,
                metrics: SharedBlockDeviceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len())
            .map_err(|_| BlockDeviceMetricsRegistryError::GenerationExhausted)?;
        Ok(Self {
            aggregate: SharedBlockDeviceMetrics::default(),
            per_drive: Arc::new(Mutex::new(BlockDeviceMetricsRegistryState {
                entries,
                reservations,
                next_generation,
                capacity,
            })),
        })
    }

    pub fn prepare_drive(
        &self,
        drive_id: impl Into<String>,
    ) -> Result<PreparedBlockDeviceMetrics, BlockDeviceMetricsRegistryError> {
        let drive_id = drive_id.into();
        let mut state = lock_block_metrics_registry(&self.per_drive);
        if state.entries.iter().any(|entry| entry.drive_id == drive_id)
            || state
                .reservations
                .iter()
                .any(|reservation| reservation.drive_id == drive_id)
        {
            return Err(BlockDeviceMetricsRegistryError::DuplicateDrive);
        }
        let claimed_capacity = state
            .entries
            .len()
            .checked_add(state.reservations.len())
            .ok_or(BlockDeviceMetricsRegistryError::Capacity)?;
        if claimed_capacity >= state.capacity {
            return Err(BlockDeviceMetricsRegistryError::Capacity);
        }
        state
            .entries
            .try_reserve_exact(1)
            .map_err(|_| BlockDeviceMetricsRegistryError::Capacity)?;
        state
            .reservations
            .try_reserve_exact(1)
            .map_err(|_| BlockDeviceMetricsRegistryError::Capacity)?;
        let next_generation = state
            .next_generation
            .checked_add(1)
            .ok_or(BlockDeviceMetricsRegistryError::GenerationExhausted)?;
        let generation = state.next_generation;
        state.next_generation = next_generation;
        state.reservations.push(BlockDeviceMetricsReservation {
            generation,
            drive_id: drive_id.clone(),
        });
        drop(state);
        Ok(PreparedBlockDeviceMetrics {
            registry: self.clone(),
            generation,
            drive_id,
            metrics: SharedBlockDeviceMetrics::default(),
            reserved: true,
        })
    }

    /// Validates one prospective runtime drive without reserving a metrics
    /// generation or changing the visible registry.
    pub fn preflight_drive(&self, drive_id: &str) -> Result<(), BlockDeviceMetricsRegistryError> {
        let state = lock_block_metrics_registry(&self.per_drive);
        if state.entries.iter().any(|entry| entry.drive_id == drive_id)
            || state
                .reservations
                .iter()
                .any(|reservation| reservation.drive_id == drive_id)
        {
            return Err(BlockDeviceMetricsRegistryError::DuplicateDrive);
        }
        let claimed_capacity = state
            .entries
            .len()
            .checked_add(state.reservations.len())
            .ok_or(BlockDeviceMetricsRegistryError::Capacity)?;
        if claimed_capacity >= state.capacity
            || state.entries.capacity() < state.capacity
            || state.reservations.capacity() < state.capacity
        {
            return Err(BlockDeviceMetricsRegistryError::Capacity);
        }
        state
            .next_generation
            .checked_add(1)
            .ok_or(BlockDeviceMetricsRegistryError::GenerationExhausted)?;
        Ok(())
    }

    /// Claims exact drop ownership for a drive that was registered when the
    /// bounded inventory was constructed.
    pub fn claim_drive_lease(
        &self,
        drive_id: &str,
    ) -> Result<BlockDeviceMetricsLease, BlockDeviceMetricsRegistryError> {
        let mut state = lock_block_metrics_registry(&self.per_drive);
        let entry = state
            .entries
            .iter_mut()
            .find(|entry| entry.drive_id == drive_id)
            .ok_or(BlockDeviceMetricsRegistryError::UnknownDrive)?;
        if entry.lease_claimed {
            return Err(BlockDeviceMetricsRegistryError::LeaseAlreadyClaimed);
        }
        entry.lease_claimed = true;
        let generation = entry.generation;
        drop(state);
        Ok(BlockDeviceMetricsLease {
            registry: self.clone(),
            generation,
            drive_id: drive_id.to_string(),
            registered: true,
        })
    }

    pub fn aggregate(&self) -> SharedBlockDeviceMetrics {
        self.aggregate.clone()
    }

    pub fn per_drive(&self, drive_id: &str) -> Option<SharedBlockDeviceMetrics> {
        lock_block_metrics_registry(&self.per_drive)
            .entries
            .iter()
            .find_map(|entry| (entry.drive_id == drive_id).then(|| entry.metrics.clone()))
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

    pub fn record_config_change_time_for_drive(&self, drive_id: &str, duration_us: u64) {
        self.aggregate.record_config_change_time_us(duration_us);
        if let Some(metrics) = self.per_drive(drive_id) {
            metrics.record_config_change_time_us(duration_us);
        }
    }

    pub fn aggregate_snapshot(&self) -> BlockDeviceMetrics {
        self.aggregate.snapshot()
    }

    pub fn per_drive_snapshot(&self) -> BlockDeviceMetricsByDrive {
        let mut snapshot = BlockDeviceMetricsByDrive::new();
        for entry in &lock_block_metrics_registry(&self.per_drive).entries {
            let metrics = entry.metrics.snapshot();
            if !metrics.is_empty() {
                snapshot.insert_drive_metrics(entry.drive_id.clone(), metrics);
            }
        }
        snapshot
    }
}

fn lock_block_metrics_registry(
    registry: &Mutex<BlockDeviceMetricsRegistryState>,
) -> MutexGuard<'_, BlockDeviceMetricsRegistryState> {
    match registry.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PmemDeviceMetrics {
    activate_fails: u64,
    cfg_fails: u64,
    event_fails: u64,
    queue_event_count: u64,
    rate_limiter_throttled_events: u64,
    rate_limiter_event_count: u64,
}

impl_incremental_delta!(PmemDeviceMetrics {
    activate_fails,
    cfg_fails,
    event_fails,
    queue_event_count,
    rate_limiter_throttled_events,
    rate_limiter_event_count,
});

impl PmemDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.cfg_fails == 0
            && self.event_fails == 0
            && self.queue_event_count == 0
            && self.rate_limiter_throttled_events == 0
            && self.rate_limiter_event_count == 0
    }

    pub const fn activate_fails(self) -> u64 {
        self.activate_fails
    }

    pub const fn cfg_fails(self) -> u64 {
        self.cfg_fails
    }

    pub const fn event_fails(self) -> u64 {
        self.event_fails
    }

    pub const fn queue_event_count(self) -> u64 {
        self.queue_event_count
    }

    pub const fn rate_limiter_throttled_events(self) -> u64 {
        self.rate_limiter_throttled_events
    }

    pub const fn rate_limiter_event_count(self) -> u64 {
        self.rate_limiter_event_count
    }

    pub const fn with_activate_fails(mut self, activate_fails: u64) -> Self {
        self.activate_fails = activate_fails;
        self
    }

    pub const fn with_cfg_fails(mut self, cfg_fails: u64) -> Self {
        self.cfg_fails = cfg_fails;
        self
    }

    pub const fn with_event_fails(mut self, event_fails: u64) -> Self {
        self.event_fails = event_fails;
        self
    }

    pub const fn with_queue_event_count(mut self, queue_event_count: u64) -> Self {
        self.queue_event_count = queue_event_count;
        self
    }

    pub const fn with_rate_limiter_throttled_events(
        mut self,
        rate_limiter_throttled_events: u64,
    ) -> Self {
        self.rate_limiter_throttled_events = rate_limiter_throttled_events;
        self
    }

    pub const fn with_rate_limiter_event_count(mut self, rate_limiter_event_count: u64) -> Self {
        self.rate_limiter_event_count = rate_limiter_event_count;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            activate_fails: self.activate_fails.saturating_add(other.activate_fails),
            cfg_fails: self.cfg_fails.saturating_add(other.cfg_fails),
            event_fails: self.event_fails.saturating_add(other.event_fails),
            queue_event_count: self
                .queue_event_count
                .saturating_add(other.queue_event_count),
            rate_limiter_throttled_events: self
                .rate_limiter_throttled_events
                .saturating_add(other.rate_limiter_throttled_events),
            rate_limiter_event_count: self
                .rate_limiter_event_count
                .saturating_add(other.rate_limiter_event_count),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PmemDeviceMetricsByDevice {
    metrics: BTreeMap<String, PmemDeviceMetrics>,
}

impl PmemDeviceMetricsByDevice {
    pub fn new() -> Self {
        Self {
            metrics: BTreeMap::new(),
        }
    }

    pub fn with_device_metrics(
        mut self,
        device_id: impl Into<String>,
        metrics: PmemDeviceMetrics,
    ) -> Self {
        self.insert_device_metrics(device_id, metrics);
        self
    }

    pub fn insert_device_metrics(
        &mut self,
        device_id: impl Into<String>,
        metrics: PmemDeviceMetrics,
    ) {
        self.metrics
            .entry(device_id.into())
            .and_modify(|existing| *existing = existing.merged_with(metrics))
            .or_insert(metrics);
    }

    pub fn is_empty(&self) -> bool {
        self.metrics
            .values()
            .all(|metrics| PmemDeviceMetrics::is_empty(*metrics))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, PmemDeviceMetrics)> {
        self.metrics
            .iter()
            .map(|(device_id, metrics)| (device_id.as_str(), *metrics))
    }

    fn delta_since(&self, previous: Option<&Self>) -> Self {
        let metrics = self
            .metrics
            .iter()
            .map(|(device_id, current)| {
                let previous = previous
                    .and_then(|metrics| metrics.metrics.get(device_id))
                    .copied()
                    .unwrap_or_default();
                (device_id.clone(), current.delta_since(previous))
            })
            .collect();
        Self { metrics }
    }

    fn merged_with(mut self, other: Self) -> Self {
        for (device_id, metrics) in other.metrics {
            self.insert_device_metrics(device_id, metrics);
        }
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedPmemDeviceMetrics {
    inner: Arc<Mutex<PmemDeviceMetrics>>,
}

impl SharedPmemDeviceMetrics {
    pub fn record_activation_failure(&self) {
        self.record(PmemDeviceMetrics::default().with_activate_fails(1));
    }

    pub fn record_config_failure(&self) {
        self.record(PmemDeviceMetrics::default().with_cfg_fails(1));
    }

    pub fn record_notification_dispatch(&self, dispatch: &VirtioPmemDeviceNotificationDispatch) {
        let mut observation = PmemDeviceMetrics::default().with_queue_event_count(
            usize_to_u64_saturating(dispatch.drained_notifications().len()),
        );
        if let Some(queue_dispatch) = dispatch.queue_dispatch() {
            observation = observation.merged_with(pmem_queue_dispatch_metrics(queue_dispatch));
        }
        self.record(observation);
    }

    pub fn record_notification_error(&self, source: &VirtioPmemDeviceNotificationError) {
        let mut observation = PmemDeviceMetrics::default()
            .with_queue_event_count(usize_to_u64_saturating(
                source.drained_notifications().len(),
            ))
            .with_event_fails(1);
        if let Some(completed) = source.completed_dispatch() {
            observation = observation.merged_with(pmem_queue_dispatch_metrics(completed));
        }
        self.record(observation);
    }

    pub fn record_queue_dispatch(&self, dispatch: &VirtioPmemQueueDispatch) {
        self.record(pmem_queue_dispatch_metrics(dispatch));
    }

    pub fn record_queue_events(&self, count: u64) {
        self.record(PmemDeviceMetrics::default().with_queue_event_count(count));
    }

    pub fn record_event_failure(&self) {
        self.record(PmemDeviceMetrics::default().with_event_fails(1));
    }

    pub fn snapshot(&self) -> PmemDeviceMetrics {
        *lock_pmem_device_metrics(&self.inner)
    }

    fn record(&self, observation: PmemDeviceMetrics) {
        if observation.is_empty() {
            return;
        }
        let mut metrics = lock_pmem_device_metrics(&self.inner);
        *metrics = metrics.merged_with(observation);
    }
}

fn pmem_queue_dispatch_metrics(dispatch: &VirtioPmemQueueDispatch) -> PmemDeviceMetrics {
    PmemDeviceMetrics::default()
        .with_event_fails(usize_to_u64_saturating(
            dispatch
                .parse_failures()
                .saturating_add(dispatch.status_write_failures()),
        ))
        .with_rate_limiter_throttled_events(usize_to_u64_saturating(
            dispatch.rate_limiter_throttled_events(),
        ))
        .with_rate_limiter_event_count(usize_to_u64_saturating(dispatch.rate_limiter_events()))
}

fn lock_pmem_device_metrics(
    metrics: &Mutex<PmemDeviceMetrics>,
) -> MutexGuard<'_, PmemDeviceMetrics> {
    match metrics.lock() {
        Ok(metrics) => metrics,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Clone)]
pub struct SharedPmemDeviceMetricsRegistry {
    aggregate: SharedPmemDeviceMetrics,
    per_device: Arc<Mutex<PmemDeviceMetricsRegistryState>>,
}

#[derive(Debug, Default)]
struct PmemDeviceMetricsRegistryState {
    entries: Vec<PmemDeviceMetricsRegistryEntry>,
    reservations: Vec<PmemDeviceMetricsReservation>,
    next_generation: u64,
    capacity: usize,
}

#[derive(Debug)]
struct PmemDeviceMetricsRegistryEntry {
    generation: u64,
    device_id: String,
    metrics: SharedPmemDeviceMetrics,
    lease_claimed: bool,
}

#[derive(Debug)]
struct PmemDeviceMetricsReservation {
    generation: u64,
    device_id: String,
}

/// Prepared per-device pmem metrics ownership that is invisible until publication.
pub struct PreparedPmemDeviceMetrics {
    registry: SharedPmemDeviceMetricsRegistry,
    generation: u64,
    device_id: String,
    metrics: SharedPmemDeviceMetrics,
    reserved: bool,
}

/// Exact live per-device pmem metrics ownership removed automatically on drop.
pub struct PmemDeviceMetricsLease {
    registry: SharedPmemDeviceMetricsRegistry,
    generation: u64,
    device_id: String,
    registered: bool,
}

impl fmt::Debug for PreparedPmemDeviceMetrics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPmemDeviceMetrics")
            .field("ownership", &"<redacted>")
            .field("reserved", &self.reserved)
            .finish()
    }
}

impl fmt::Debug for PmemDeviceMetricsLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PmemDeviceMetricsLease")
            .field("ownership", &"<redacted>")
            .field("registered", &self.registered)
            .finish()
    }
}

impl PreparedPmemDeviceMetrics {
    pub fn metrics(&self) -> SharedPmemDeviceMetrics {
        self.metrics.clone()
    }

    pub fn publish(mut self) -> PmemDeviceMetricsLease {
        let mut state = lock_pmem_metrics_registry(&self.registry.per_device);
        let reservation_count = state.reservations.len();
        state.reservations.retain(|reservation| {
            reservation.generation != self.generation || reservation.device_id != self.device_id
        });
        debug_assert_eq!(
            state.reservations.len().checked_add(1),
            Some(reservation_count)
        );
        self.reserved = false;
        debug_assert!(state.entries.len() < state.capacity);
        debug_assert!(
            !state
                .entries
                .iter()
                .any(|entry| entry.device_id == self.device_id)
        );
        state.entries.push(PmemDeviceMetricsRegistryEntry {
            generation: self.generation,
            device_id: self.device_id.clone(),
            metrics: self.metrics.clone(),
            lease_claimed: true,
        });
        drop(state);
        PmemDeviceMetricsLease {
            registry: self.registry.clone(),
            generation: self.generation,
            device_id: self.device_id.clone(),
            registered: true,
        }
    }
}

impl Drop for PreparedPmemDeviceMetrics {
    fn drop(&mut self) {
        if !self.reserved {
            return;
        }
        let mut state = lock_pmem_metrics_registry(&self.registry.per_device);
        if let Some(index) = state.reservations.iter().position(|reservation| {
            reservation.generation == self.generation && reservation.device_id == self.device_id
        }) {
            state.reservations.remove(index);
        }
        self.reserved = false;
    }
}

impl Drop for PmemDeviceMetricsLease {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let mut state = lock_pmem_metrics_registry(&self.registry.per_device);
        if let Some(index) = state.entries.iter().position(|entry| {
            entry.generation == self.generation && entry.device_id == self.device_id
        }) {
            state.entries.remove(index);
        }
        self.registered = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmemDeviceMetricsRegistryError {
    DuplicateDevice,
    UnknownDevice,
    LeaseAlreadyClaimed,
    Capacity,
    GenerationExhausted,
}

impl fmt::Display for PmemDeviceMetricsRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDevice => {
                formatter.write_str("pmem metrics device is already registered")
            }
            Self::UnknownDevice => formatter.write_str("pmem metrics device is not registered"),
            Self::LeaseAlreadyClaimed => {
                formatter.write_str("pmem metrics device lease is already claimed")
            }
            Self::Capacity => formatter.write_str("failed to reserve pmem metrics capacity"),
            Self::GenerationExhausted => {
                formatter.write_str("pmem metrics ownership generation is exhausted")
            }
        }
    }
}

impl std::error::Error for PmemDeviceMetricsRegistryError {}

impl Default for SharedPmemDeviceMetricsRegistry {
    fn default() -> Self {
        Self {
            aggregate: SharedPmemDeviceMetrics::default(),
            per_device: Arc::new(Mutex::new(PmemDeviceMetricsRegistryState::default())),
        }
    }
}

impl SharedPmemDeviceMetricsRegistry {
    pub fn from_device_ids<'a>(device_ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut entries = Vec::new();
        for device_id in device_ids {
            if entries
                .iter()
                .any(|entry: &PmemDeviceMetricsRegistryEntry| entry.device_id == device_id)
            {
                continue;
            }
            let generation = u64::try_from(entries.len()).unwrap_or(u64::MAX);
            entries.push(PmemDeviceMetricsRegistryEntry {
                generation,
                device_id: device_id.to_string(),
                metrics: SharedPmemDeviceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        Self {
            aggregate: SharedPmemDeviceMetrics::default(),
            per_device: Arc::new(Mutex::new(PmemDeviceMetricsRegistryState {
                capacity: entries.len(),
                entries,
                reservations: Vec::new(),
                next_generation,
            })),
        }
    }

    pub fn from_device_ids_with_capacity<'a>(
        device_ids: impl IntoIterator<Item = &'a str>,
        capacity: usize,
    ) -> Result<Self, PmemDeviceMetricsRegistryError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| PmemDeviceMetricsRegistryError::Capacity)?;
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(capacity)
            .map_err(|_| PmemDeviceMetricsRegistryError::Capacity)?;
        for device_id in device_ids {
            if entries
                .iter()
                .any(|entry: &PmemDeviceMetricsRegistryEntry| entry.device_id == device_id)
            {
                continue;
            }
            if entries.len() == capacity {
                return Err(PmemDeviceMetricsRegistryError::Capacity);
            }
            let generation = u64::try_from(entries.len())
                .map_err(|_| PmemDeviceMetricsRegistryError::GenerationExhausted)?;
            entries.push(PmemDeviceMetricsRegistryEntry {
                generation,
                device_id: device_id.to_string(),
                metrics: SharedPmemDeviceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len())
            .map_err(|_| PmemDeviceMetricsRegistryError::GenerationExhausted)?;
        Ok(Self {
            aggregate: SharedPmemDeviceMetrics::default(),
            per_device: Arc::new(Mutex::new(PmemDeviceMetricsRegistryState {
                entries,
                reservations,
                next_generation,
                capacity,
            })),
        })
    }

    /// Builds a bounded registry by consuming already-validated owned IDs.
    #[doc(hidden)]
    pub fn from_owned_device_ids_with_capacity(
        device_ids: Vec<String>,
        capacity: usize,
    ) -> Result<Self, PmemDeviceMetricsRegistryError> {
        if device_ids.len() > capacity {
            return Err(PmemDeviceMetricsRegistryError::Capacity);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| PmemDeviceMetricsRegistryError::Capacity)?;
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(capacity)
            .map_err(|_| PmemDeviceMetricsRegistryError::Capacity)?;
        for device_id in device_ids {
            if entries
                .iter()
                .any(|entry: &PmemDeviceMetricsRegistryEntry| entry.device_id == device_id)
            {
                return Err(PmemDeviceMetricsRegistryError::DuplicateDevice);
            }
            let generation = u64::try_from(entries.len())
                .map_err(|_| PmemDeviceMetricsRegistryError::GenerationExhausted)?;
            entries.push(PmemDeviceMetricsRegistryEntry {
                generation,
                device_id,
                metrics: SharedPmemDeviceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len())
            .map_err(|_| PmemDeviceMetricsRegistryError::GenerationExhausted)?;
        Ok(Self {
            aggregate: SharedPmemDeviceMetrics::default(),
            per_device: Arc::new(Mutex::new(PmemDeviceMetricsRegistryState {
                entries,
                reservations,
                next_generation,
                capacity,
            })),
        })
    }

    pub fn prepare_device(
        &self,
        device_id: impl Into<String>,
    ) -> Result<PreparedPmemDeviceMetrics, PmemDeviceMetricsRegistryError> {
        let device_id = device_id.into();
        let mut state = lock_pmem_metrics_registry(&self.per_device);
        if state
            .entries
            .iter()
            .any(|entry| entry.device_id == device_id)
            || state
                .reservations
                .iter()
                .any(|reservation| reservation.device_id == device_id)
        {
            return Err(PmemDeviceMetricsRegistryError::DuplicateDevice);
        }
        let claimed_capacity = state
            .entries
            .len()
            .checked_add(state.reservations.len())
            .ok_or(PmemDeviceMetricsRegistryError::Capacity)?;
        if claimed_capacity >= state.capacity {
            return Err(PmemDeviceMetricsRegistryError::Capacity);
        }
        state
            .entries
            .try_reserve_exact(1)
            .map_err(|_| PmemDeviceMetricsRegistryError::Capacity)?;
        state
            .reservations
            .try_reserve_exact(1)
            .map_err(|_| PmemDeviceMetricsRegistryError::Capacity)?;
        let next_generation = state
            .next_generation
            .checked_add(1)
            .ok_or(PmemDeviceMetricsRegistryError::GenerationExhausted)?;
        let generation = state.next_generation;
        state.next_generation = next_generation;
        state.reservations.push(PmemDeviceMetricsReservation {
            generation,
            device_id: device_id.clone(),
        });
        drop(state);
        Ok(PreparedPmemDeviceMetrics {
            registry: self.clone(),
            generation,
            device_id,
            metrics: SharedPmemDeviceMetrics::default(),
            reserved: true,
        })
    }

    /// Validates one prospective runtime pmem device without reserving a
    /// metrics generation or changing the visible registry.
    pub fn preflight_device(&self, device_id: &str) -> Result<(), PmemDeviceMetricsRegistryError> {
        let state = lock_pmem_metrics_registry(&self.per_device);
        if state
            .entries
            .iter()
            .any(|entry| entry.device_id == device_id)
            || state
                .reservations
                .iter()
                .any(|reservation| reservation.device_id == device_id)
        {
            return Err(PmemDeviceMetricsRegistryError::DuplicateDevice);
        }
        let claimed_capacity = state
            .entries
            .len()
            .checked_add(state.reservations.len())
            .ok_or(PmemDeviceMetricsRegistryError::Capacity)?;
        if claimed_capacity >= state.capacity
            || state.entries.capacity() < state.capacity
            || state.reservations.capacity() < state.capacity
        {
            return Err(PmemDeviceMetricsRegistryError::Capacity);
        }
        state
            .next_generation
            .checked_add(1)
            .ok_or(PmemDeviceMetricsRegistryError::GenerationExhausted)?;
        Ok(())
    }

    /// Claims drop ownership for one device registered during bounded startup.
    pub fn claim_device_lease(
        &self,
        device_id: &str,
    ) -> Result<PmemDeviceMetricsLease, PmemDeviceMetricsRegistryError> {
        let mut state = lock_pmem_metrics_registry(&self.per_device);
        let entry = state
            .entries
            .iter_mut()
            .find(|entry| entry.device_id == device_id)
            .ok_or(PmemDeviceMetricsRegistryError::UnknownDevice)?;
        if entry.lease_claimed {
            return Err(PmemDeviceMetricsRegistryError::LeaseAlreadyClaimed);
        }
        entry.lease_claimed = true;
        let generation = entry.generation;
        drop(state);
        Ok(PmemDeviceMetricsLease {
            registry: self.clone(),
            generation,
            device_id: device_id.to_string(),
            registered: true,
        })
    }

    pub fn aggregate(&self) -> SharedPmemDeviceMetrics {
        self.aggregate.clone()
    }

    pub fn per_device(&self, device_id: &str) -> Option<SharedPmemDeviceMetrics> {
        lock_pmem_metrics_registry(&self.per_device)
            .entries
            .iter()
            .find_map(|entry| (entry.device_id == device_id).then(|| entry.metrics.clone()))
    }

    pub fn record_notification_dispatch_for_device(
        &self,
        device_id: &str,
        dispatch: &VirtioPmemDeviceNotificationDispatch,
    ) {
        self.aggregate.record_notification_dispatch(dispatch);
        if let Some(metrics) = self.per_device(device_id) {
            metrics.record_notification_dispatch(dispatch);
        }
    }

    pub fn record_notification_error_for_device(
        &self,
        device_id: &str,
        source: &VirtioPmemDeviceNotificationError,
    ) {
        self.aggregate.record_notification_error(source);
        if let Some(metrics) = self.per_device(device_id) {
            metrics.record_notification_error(source);
        }
    }

    pub fn record_event_failure(&self) {
        self.aggregate.record_event_failure();
    }

    pub fn record_event_failure_for_device(&self, device_id: &str) {
        self.aggregate.record_event_failure();
        if let Some(metrics) = self.per_device(device_id) {
            metrics.record_event_failure();
        }
    }

    pub fn record_queue_events_for_device(&self, device_id: &str, count: u64) {
        self.aggregate.record_queue_events(count);
        if let Some(metrics) = self.per_device(device_id) {
            metrics.record_queue_events(count);
        }
    }

    pub fn aggregate_snapshot(&self) -> PmemDeviceMetrics {
        self.aggregate.snapshot()
    }

    pub fn per_device_snapshot(&self) -> PmemDeviceMetricsByDevice {
        let mut snapshot = PmemDeviceMetricsByDevice::new();
        for entry in &lock_pmem_metrics_registry(&self.per_device).entries {
            let metrics = entry.metrics.snapshot();
            if !metrics.is_empty() {
                snapshot.insert_device_metrics(entry.device_id.clone(), metrics);
            }
        }
        snapshot
    }
}

fn lock_pmem_metrics_registry(
    registry: &Mutex<PmemDeviceMetricsRegistryState>,
) -> MutexGuard<'_, PmemDeviceMetricsRegistryState> {
    match registry.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkInterfaceMetrics {
    activate_fails: u64,
    cfg_fails: u64,
    event_fails: u64,
    no_rx_avail_buffer: u64,
    no_tx_avail_buffer: u64,
    rx_queue_event_count: u64,
    rx_bytes_count: u64,
    rx_packets_count: u64,
    rx_fails: u64,
    rx_count: u64,
    rx_rate_limiter_event_count: u64,
    rx_rate_limiter_throttled: u64,
    tx_bytes_count: u64,
    tx_malformed_frames: u64,
    tx_fails: u64,
    tx_count: u64,
    tx_packets_count: u64,
    tx_queue_event_count: u64,
    tx_rate_limiter_event_count: u64,
    tx_rate_limiter_throttled: u64,
    tx_remaining_reqs_count: u64,
    tx_spoofed_mac_count: u64,
    vmnet_read_count: u64,
    vmnet_read_fails: u64,
    vmnet_read_packets_count: u64,
    vmnet_read_partial_batches: u64,
    vmnet_write_count: u64,
    vmnet_write_fails: u64,
    vmnet_write_packets_count: u64,
    vmnet_write_partial_batches: u64,
    vmnet_read_latency: VirtioNetworkLatencyAggregate,
    vmnet_write_latency: VirtioNetworkLatencyAggregate,
}

impl NetworkInterfaceMetrics {
    const fn delta_since(self, previous: Self) -> Self {
        Self {
            activate_fails: incremental_delta(self.activate_fails, previous.activate_fails),
            cfg_fails: incremental_delta(self.cfg_fails, previous.cfg_fails),
            event_fails: incremental_delta(self.event_fails, previous.event_fails),
            no_rx_avail_buffer: incremental_delta(
                self.no_rx_avail_buffer,
                previous.no_rx_avail_buffer,
            ),
            no_tx_avail_buffer: incremental_delta(
                self.no_tx_avail_buffer,
                previous.no_tx_avail_buffer,
            ),
            rx_queue_event_count: incremental_delta(
                self.rx_queue_event_count,
                previous.rx_queue_event_count,
            ),
            rx_bytes_count: incremental_delta(self.rx_bytes_count, previous.rx_bytes_count),
            rx_packets_count: incremental_delta(self.rx_packets_count, previous.rx_packets_count),
            rx_fails: incremental_delta(self.rx_fails, previous.rx_fails),
            rx_count: incremental_delta(self.rx_count, previous.rx_count),
            rx_rate_limiter_event_count: incremental_delta(
                self.rx_rate_limiter_event_count,
                previous.rx_rate_limiter_event_count,
            ),
            rx_rate_limiter_throttled: incremental_delta(
                self.rx_rate_limiter_throttled,
                previous.rx_rate_limiter_throttled,
            ),
            tx_bytes_count: incremental_delta(self.tx_bytes_count, previous.tx_bytes_count),
            tx_malformed_frames: incremental_delta(
                self.tx_malformed_frames,
                previous.tx_malformed_frames,
            ),
            tx_fails: incremental_delta(self.tx_fails, previous.tx_fails),
            tx_count: incremental_delta(self.tx_count, previous.tx_count),
            tx_packets_count: incremental_delta(self.tx_packets_count, previous.tx_packets_count),
            tx_queue_event_count: incremental_delta(
                self.tx_queue_event_count,
                previous.tx_queue_event_count,
            ),
            tx_rate_limiter_event_count: incremental_delta(
                self.tx_rate_limiter_event_count,
                previous.tx_rate_limiter_event_count,
            ),
            tx_rate_limiter_throttled: incremental_delta(
                self.tx_rate_limiter_throttled,
                previous.tx_rate_limiter_throttled,
            ),
            tx_remaining_reqs_count: incremental_delta(
                self.tx_remaining_reqs_count,
                previous.tx_remaining_reqs_count,
            ),
            tx_spoofed_mac_count: incremental_delta(
                self.tx_spoofed_mac_count,
                previous.tx_spoofed_mac_count,
            ),
            vmnet_read_count: incremental_delta(self.vmnet_read_count, previous.vmnet_read_count),
            vmnet_read_fails: incremental_delta(self.vmnet_read_fails, previous.vmnet_read_fails),
            vmnet_read_packets_count: incremental_delta(
                self.vmnet_read_packets_count,
                previous.vmnet_read_packets_count,
            ),
            vmnet_read_partial_batches: incremental_delta(
                self.vmnet_read_partial_batches,
                previous.vmnet_read_partial_batches,
            ),
            vmnet_write_count: incremental_delta(
                self.vmnet_write_count,
                previous.vmnet_write_count,
            ),
            vmnet_write_fails: incremental_delta(
                self.vmnet_write_fails,
                previous.vmnet_write_fails,
            ),
            vmnet_write_packets_count: incremental_delta(
                self.vmnet_write_packets_count,
                previous.vmnet_write_packets_count,
            ),
            vmnet_write_partial_batches: incremental_delta(
                self.vmnet_write_partial_batches,
                previous.vmnet_write_partial_batches,
            ),
            vmnet_read_latency: network_latency_delta(
                self.vmnet_read_latency,
                previous.vmnet_read_latency,
            ),
            vmnet_write_latency: network_latency_delta(
                self.vmnet_write_latency,
                previous.vmnet_write_latency,
            ),
        }
    }
}

const fn network_latency_delta(
    current: VirtioNetworkLatencyAggregate,
    previous: VirtioNetworkLatencyAggregate,
) -> VirtioNetworkLatencyAggregate {
    VirtioNetworkLatencyAggregate::new(
        current.min_us(),
        current.max_us(),
        incremental_delta(current.sum_us(), previous.sum_us()),
        incremental_delta(current.samples(), previous.samples()),
    )
}

impl NetworkInterfaceMetrics {
    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.cfg_fails == 0
            && self.event_fails == 0
            && self.no_rx_avail_buffer == 0
            && self.no_tx_avail_buffer == 0
            && self.rx_queue_event_count == 0
            && self.rx_bytes_count == 0
            && self.rx_packets_count == 0
            && self.rx_fails == 0
            && self.rx_count == 0
            && self.rx_rate_limiter_event_count == 0
            && self.rx_rate_limiter_throttled == 0
            && self.tx_bytes_count == 0
            && self.tx_malformed_frames == 0
            && self.tx_fails == 0
            && self.tx_count == 0
            && self.tx_packets_count == 0
            && self.tx_queue_event_count == 0
            && self.tx_rate_limiter_event_count == 0
            && self.tx_rate_limiter_throttled == 0
            && self.tx_remaining_reqs_count == 0
            && self.tx_spoofed_mac_count == 0
            && self.vmnet_read_count == 0
            && self.vmnet_read_fails == 0
            && self.vmnet_read_packets_count == 0
            && self.vmnet_read_partial_batches == 0
            && self.vmnet_write_count == 0
            && self.vmnet_write_fails == 0
            && self.vmnet_write_packets_count == 0
            && self.vmnet_write_partial_batches == 0
            && self.vmnet_read_latency.samples() == 0
            && self.vmnet_write_latency.samples() == 0
    }

    pub const fn activate_fails(self) -> u64 {
        self.activate_fails
    }

    pub const fn cfg_fails(self) -> u64 {
        self.cfg_fails
    }

    pub const fn event_fails(self) -> u64 {
        self.event_fails
    }

    pub const fn no_rx_avail_buffer(self) -> u64 {
        self.no_rx_avail_buffer
    }

    pub const fn no_tx_avail_buffer(self) -> u64 {
        self.no_tx_avail_buffer
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

    pub const fn rx_rate_limiter_event_count(self) -> u64 {
        self.rx_rate_limiter_event_count
    }

    pub const fn rx_event_rate_limiter_count(self) -> u64 {
        self.rx_rate_limiter_event_count
    }

    pub const fn rx_rate_limiter_throttled(self) -> u64 {
        self.rx_rate_limiter_throttled
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

    pub const fn tx_rate_limiter_event_count(self) -> u64 {
        self.tx_rate_limiter_event_count
    }

    pub const fn tx_rate_limiter_throttled(self) -> u64 {
        self.tx_rate_limiter_throttled
    }

    pub const fn tx_remaining_reqs_count(self) -> u64 {
        self.tx_remaining_reqs_count
    }

    pub const fn tx_spoofed_mac_count(self) -> u64 {
        self.tx_spoofed_mac_count
    }

    pub const fn vmnet_read_count(self) -> u64 {
        self.vmnet_read_count
    }

    pub const fn vmnet_read_fails(self) -> u64 {
        self.vmnet_read_fails
    }

    pub const fn vmnet_read_packets_count(self) -> u64 {
        self.vmnet_read_packets_count
    }

    pub const fn vmnet_read_partial_batches(self) -> u64 {
        self.vmnet_read_partial_batches
    }

    pub const fn vmnet_write_count(self) -> u64 {
        self.vmnet_write_count
    }

    pub const fn vmnet_write_fails(self) -> u64 {
        self.vmnet_write_fails
    }

    pub const fn vmnet_write_packets_count(self) -> u64 {
        self.vmnet_write_packets_count
    }

    pub const fn vmnet_write_partial_batches(self) -> u64 {
        self.vmnet_write_partial_batches
    }

    pub const fn vmnet_read_latency(self) -> VirtioNetworkLatencyAggregate {
        self.vmnet_read_latency
    }

    pub const fn vmnet_write_latency(self) -> VirtioNetworkLatencyAggregate {
        self.vmnet_write_latency
    }

    pub const fn with_activate_fails(mut self, activate_fails: u64) -> Self {
        self.activate_fails = activate_fails;
        self
    }

    pub const fn with_cfg_fails(mut self, cfg_fails: u64) -> Self {
        self.cfg_fails = cfg_fails;
        self
    }

    pub const fn with_event_fails(mut self, event_fails: u64) -> Self {
        self.event_fails = event_fails;
        self
    }

    pub const fn with_no_rx_avail_buffer(mut self, no_rx_avail_buffer: u64) -> Self {
        self.no_rx_avail_buffer = no_rx_avail_buffer;
        self
    }

    pub const fn with_no_tx_avail_buffer(mut self, no_tx_avail_buffer: u64) -> Self {
        self.no_tx_avail_buffer = no_tx_avail_buffer;
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

    pub const fn with_rx_rate_limiter_event_count(
        mut self,
        rx_rate_limiter_event_count: u64,
    ) -> Self {
        self.rx_rate_limiter_event_count = rx_rate_limiter_event_count;
        self
    }

    pub const fn with_rx_event_rate_limiter_count(self, rx_event_rate_limiter_count: u64) -> Self {
        self.with_rx_rate_limiter_event_count(rx_event_rate_limiter_count)
    }

    pub const fn with_rx_rate_limiter_throttled(mut self, rx_rate_limiter_throttled: u64) -> Self {
        self.rx_rate_limiter_throttled = rx_rate_limiter_throttled;
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

    pub const fn with_tx_rate_limiter_event_count(
        mut self,
        tx_rate_limiter_event_count: u64,
    ) -> Self {
        self.tx_rate_limiter_event_count = tx_rate_limiter_event_count;
        self
    }

    pub const fn with_tx_rate_limiter_throttled(mut self, tx_rate_limiter_throttled: u64) -> Self {
        self.tx_rate_limiter_throttled = tx_rate_limiter_throttled;
        self
    }

    pub const fn with_tx_remaining_reqs_count(mut self, tx_remaining_reqs_count: u64) -> Self {
        self.tx_remaining_reqs_count = tx_remaining_reqs_count;
        self
    }

    pub const fn with_tx_spoofed_mac_count(mut self, tx_spoofed_mac_count: u64) -> Self {
        self.tx_spoofed_mac_count = tx_spoofed_mac_count;
        self
    }

    pub const fn with_vmnet_read_count(mut self, vmnet_read_count: u64) -> Self {
        self.vmnet_read_count = vmnet_read_count;
        self
    }

    pub const fn with_vmnet_read_fails(mut self, vmnet_read_fails: u64) -> Self {
        self.vmnet_read_fails = vmnet_read_fails;
        self
    }

    pub const fn with_vmnet_read_packets_count(mut self, vmnet_read_packets_count: u64) -> Self {
        self.vmnet_read_packets_count = vmnet_read_packets_count;
        self
    }

    pub const fn with_vmnet_read_partial_batches(
        mut self,
        vmnet_read_partial_batches: u64,
    ) -> Self {
        self.vmnet_read_partial_batches = vmnet_read_partial_batches;
        self
    }

    pub const fn with_vmnet_write_count(mut self, vmnet_write_count: u64) -> Self {
        self.vmnet_write_count = vmnet_write_count;
        self
    }

    pub const fn with_vmnet_write_fails(mut self, vmnet_write_fails: u64) -> Self {
        self.vmnet_write_fails = vmnet_write_fails;
        self
    }

    pub const fn with_vmnet_write_packets_count(mut self, vmnet_write_packets_count: u64) -> Self {
        self.vmnet_write_packets_count = vmnet_write_packets_count;
        self
    }

    pub const fn with_vmnet_write_partial_batches(
        mut self,
        vmnet_write_partial_batches: u64,
    ) -> Self {
        self.vmnet_write_partial_batches = vmnet_write_partial_batches;
        self
    }

    pub const fn with_vmnet_read_latency(
        mut self,
        vmnet_read_latency: VirtioNetworkLatencyAggregate,
    ) -> Self {
        self.vmnet_read_latency = vmnet_read_latency;
        self
    }

    pub const fn with_vmnet_write_latency(
        mut self,
        vmnet_write_latency: VirtioNetworkLatencyAggregate,
    ) -> Self {
        self.vmnet_write_latency = vmnet_write_latency;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            activate_fails: self.activate_fails.saturating_add(other.activate_fails),
            cfg_fails: self.cfg_fails.saturating_add(other.cfg_fails),
            event_fails: self.event_fails.saturating_add(other.event_fails),
            no_rx_avail_buffer: self
                .no_rx_avail_buffer
                .saturating_add(other.no_rx_avail_buffer),
            no_tx_avail_buffer: self
                .no_tx_avail_buffer
                .saturating_add(other.no_tx_avail_buffer),
            rx_queue_event_count: self
                .rx_queue_event_count
                .saturating_add(other.rx_queue_event_count),
            rx_bytes_count: self.rx_bytes_count.saturating_add(other.rx_bytes_count),
            rx_packets_count: self.rx_packets_count.saturating_add(other.rx_packets_count),
            rx_fails: self.rx_fails.saturating_add(other.rx_fails),
            rx_count: self.rx_count.saturating_add(other.rx_count),
            rx_rate_limiter_event_count: self
                .rx_rate_limiter_event_count
                .saturating_add(other.rx_rate_limiter_event_count),
            rx_rate_limiter_throttled: self
                .rx_rate_limiter_throttled
                .saturating_add(other.rx_rate_limiter_throttled),
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
            tx_rate_limiter_event_count: self
                .tx_rate_limiter_event_count
                .saturating_add(other.tx_rate_limiter_event_count),
            tx_rate_limiter_throttled: self
                .tx_rate_limiter_throttled
                .saturating_add(other.tx_rate_limiter_throttled),
            tx_remaining_reqs_count: self
                .tx_remaining_reqs_count
                .saturating_add(other.tx_remaining_reqs_count),
            tx_spoofed_mac_count: self
                .tx_spoofed_mac_count
                .saturating_add(other.tx_spoofed_mac_count),
            vmnet_read_count: self.vmnet_read_count.saturating_add(other.vmnet_read_count),
            vmnet_read_fails: self.vmnet_read_fails.saturating_add(other.vmnet_read_fails),
            vmnet_read_packets_count: self
                .vmnet_read_packets_count
                .saturating_add(other.vmnet_read_packets_count),
            vmnet_read_partial_batches: self
                .vmnet_read_partial_batches
                .saturating_add(other.vmnet_read_partial_batches),
            vmnet_write_count: self
                .vmnet_write_count
                .saturating_add(other.vmnet_write_count),
            vmnet_write_fails: self
                .vmnet_write_fails
                .saturating_add(other.vmnet_write_fails),
            vmnet_write_packets_count: self
                .vmnet_write_packets_count
                .saturating_add(other.vmnet_write_packets_count),
            vmnet_write_partial_batches: self
                .vmnet_write_partial_batches
                .saturating_add(other.vmnet_write_partial_batches),
            vmnet_read_latency: self
                .vmnet_read_latency
                .merged_with(other.vmnet_read_latency),
            vmnet_write_latency: self
                .vmnet_write_latency
                .merged_with(other.vmnet_write_latency),
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

    fn delta_since(&self, previous: Option<&Self>) -> Self {
        let metrics = self
            .metrics
            .iter()
            .map(|(iface_id, current)| {
                let previous = previous
                    .and_then(|metrics| metrics.metrics.get(iface_id))
                    .copied()
                    .unwrap_or_default();
                (iface_id.clone(), current.delta_since(previous))
            })
            .collect();
        Self { metrics }
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
        if dispatch.rx_rate_limiter_event() {
            record_atomic_metric(&self.inner.rx_rate_limiter_event_count, 1);
        }
        if dispatch.tx_rate_limiter_event() {
            record_atomic_metric(&self.inner.tx_rate_limiter_event_count, 1);
        }
        if dispatch
            .tx_queue_dispatch()
            .is_some_and(|dispatch| dispatch.processed_frames() == 0)
        {
            record_atomic_metric(&self.inner.no_tx_avail_buffer, 1);
        }
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
        record_atomic_metric(
            &self.inner.no_rx_avail_buffer,
            usize_to_u64_saturating(dispatch.no_available_buffers()),
        );
        let throttled = usize_to_u64_saturating(dispatch.rate_limiter_throttled_packets());
        record_atomic_metric(&self.inner.rx_rate_limiter_throttled, throttled);
        self.record_backend_metrics(dispatch.backend_metrics());
    }

    pub fn record_tx_queue_dispatch(&self, dispatch: &VirtioNetworkTxQueueDispatch) {
        let successful_frames = usize_to_u64_saturating(dispatch.sink_successful_frames());
        self.record_tx_packets(successful_frames, dispatch.sink_successful_bytes());
        self.record_tx_malformed_frames(usize_to_u64_saturating(dispatch.malformed_frames()));
        self.record_tx_failures(usize_to_u64_saturating(dispatch.sink_failures()));
        let throttled = usize_to_u64_saturating(dispatch.rate_limiter_throttled_frames());
        record_atomic_metric(&self.inner.tx_rate_limiter_throttled, throttled);
        record_atomic_metric(
            &self.inner.tx_remaining_reqs_count,
            dispatch.remaining_requests(),
        );
        self.record_backend_metrics(dispatch.backend_metrics());
    }

    pub fn record_event_failure(&self) {
        record_atomic_metric(&self.inner.event_fails, 1);
    }

    pub fn record_activation_failure(&self) {
        record_atomic_metric(&self.inner.activate_fails, 1);
    }

    pub fn record_config_failure(&self) {
        record_atomic_metric(&self.inner.cfg_fails, 1);
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
            activate_fails: self.inner.activate_fails.load(Ordering::Relaxed),
            cfg_fails: self.inner.cfg_fails.load(Ordering::Relaxed),
            event_fails: self.inner.event_fails.load(Ordering::Relaxed),
            no_rx_avail_buffer: self.inner.no_rx_avail_buffer.load(Ordering::Relaxed),
            no_tx_avail_buffer: self.inner.no_tx_avail_buffer.load(Ordering::Relaxed),
            rx_queue_event_count: self.inner.rx_queue_event_count.load(Ordering::Relaxed),
            rx_bytes_count: self.inner.rx_bytes_count.load(Ordering::Relaxed),
            rx_packets_count: self.inner.rx_packets_count.load(Ordering::Relaxed),
            rx_fails: self.inner.rx_fails.load(Ordering::Relaxed),
            rx_count: self.inner.rx_count.load(Ordering::Relaxed),
            rx_rate_limiter_event_count: self
                .inner
                .rx_rate_limiter_event_count
                .load(Ordering::Relaxed),
            rx_rate_limiter_throttled: self.inner.rx_rate_limiter_throttled.load(Ordering::Relaxed),
            tx_bytes_count: self.inner.tx_bytes_count.load(Ordering::Relaxed),
            tx_malformed_frames: self.inner.tx_malformed_frames.load(Ordering::Relaxed),
            tx_fails: self.inner.tx_fails.load(Ordering::Relaxed),
            tx_count: self.inner.tx_count.load(Ordering::Relaxed),
            tx_packets_count: self.inner.tx_packets_count.load(Ordering::Relaxed),
            tx_queue_event_count: self.inner.tx_queue_event_count.load(Ordering::Relaxed),
            tx_rate_limiter_event_count: self
                .inner
                .tx_rate_limiter_event_count
                .load(Ordering::Relaxed),
            tx_rate_limiter_throttled: self.inner.tx_rate_limiter_throttled.load(Ordering::Relaxed),
            tx_remaining_reqs_count: self.inner.tx_remaining_reqs_count.load(Ordering::Relaxed),
            tx_spoofed_mac_count: self.inner.tx_spoofed_mac_count.load(Ordering::Relaxed),
            vmnet_read_count: self.inner.vmnet_read_count.load(Ordering::Relaxed),
            vmnet_read_fails: self.inner.vmnet_read_fails.load(Ordering::Relaxed),
            vmnet_read_packets_count: self.inner.vmnet_read_packets_count.load(Ordering::Relaxed),
            vmnet_read_partial_batches: self
                .inner
                .vmnet_read_partial_batches
                .load(Ordering::Relaxed),
            vmnet_write_count: self.inner.vmnet_write_count.load(Ordering::Relaxed),
            vmnet_write_fails: self.inner.vmnet_write_fails.load(Ordering::Relaxed),
            vmnet_write_packets_count: self.inner.vmnet_write_packets_count.load(Ordering::Relaxed),
            vmnet_write_partial_batches: self
                .inner
                .vmnet_write_partial_batches
                .load(Ordering::Relaxed),
            vmnet_read_latency: snapshot_network_latency(&self.inner.vmnet_read_latency),
            vmnet_write_latency: snapshot_network_latency(&self.inner.vmnet_write_latency),
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

    fn record_backend_metrics(&self, metrics: VirtioNetworkBackendMetrics) {
        record_atomic_metric(&self.inner.vmnet_read_count, metrics.vmnet_read_count());
        record_atomic_metric(&self.inner.vmnet_read_fails, metrics.vmnet_read_fails());
        record_atomic_metric(
            &self.inner.vmnet_read_packets_count,
            metrics.vmnet_read_packets_count(),
        );
        record_atomic_metric(
            &self.inner.vmnet_read_partial_batches,
            metrics.vmnet_read_partial_batches(),
        );
        record_atomic_metric(&self.inner.vmnet_write_count, metrics.vmnet_write_count());
        record_atomic_metric(&self.inner.vmnet_write_fails, metrics.vmnet_write_fails());
        record_atomic_metric(
            &self.inner.vmnet_write_packets_count,
            metrics.vmnet_write_packets_count(),
        );
        record_atomic_metric(
            &self.inner.vmnet_write_partial_batches,
            metrics.vmnet_write_partial_batches(),
        );
        record_atomic_metric(
            &self.inner.tx_spoofed_mac_count,
            metrics.tx_spoofed_mac_count(),
        );
        record_network_latency(&self.inner.vmnet_read_latency, metrics.vmnet_read_latency());
        record_network_latency(
            &self.inner.vmnet_write_latency,
            metrics.vmnet_write_latency(),
        );
    }
}

#[derive(Debug)]
struct NetworkLatencyAtomicMetrics {
    min_us: AtomicU64,
    max_us: AtomicU64,
    sum_us: AtomicU64,
    samples: AtomicU64,
}

impl Default for NetworkLatencyAtomicMetrics {
    fn default() -> Self {
        Self {
            min_us: AtomicU64::new(u64::MAX),
            max_us: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
            samples: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Default)]
struct SharedNetworkInterfaceMetricsInner {
    activate_fails: AtomicU64,
    cfg_fails: AtomicU64,
    event_fails: AtomicU64,
    no_rx_avail_buffer: AtomicU64,
    no_tx_avail_buffer: AtomicU64,
    rx_queue_event_count: AtomicU64,
    rx_bytes_count: AtomicU64,
    rx_packets_count: AtomicU64,
    rx_fails: AtomicU64,
    rx_count: AtomicU64,
    rx_rate_limiter_event_count: AtomicU64,
    rx_rate_limiter_throttled: AtomicU64,
    tx_bytes_count: AtomicU64,
    tx_malformed_frames: AtomicU64,
    tx_fails: AtomicU64,
    tx_count: AtomicU64,
    tx_packets_count: AtomicU64,
    tx_queue_event_count: AtomicU64,
    tx_rate_limiter_event_count: AtomicU64,
    tx_rate_limiter_throttled: AtomicU64,
    tx_remaining_reqs_count: AtomicU64,
    tx_spoofed_mac_count: AtomicU64,
    vmnet_read_count: AtomicU64,
    vmnet_read_fails: AtomicU64,
    vmnet_read_packets_count: AtomicU64,
    vmnet_read_partial_batches: AtomicU64,
    vmnet_write_count: AtomicU64,
    vmnet_write_fails: AtomicU64,
    vmnet_write_packets_count: AtomicU64,
    vmnet_write_partial_batches: AtomicU64,
    vmnet_read_latency: NetworkLatencyAtomicMetrics,
    vmnet_write_latency: NetworkLatencyAtomicMetrics,
}

#[derive(Debug, Clone)]
pub struct SharedNetworkInterfaceMetricsRegistry {
    aggregate: SharedNetworkInterfaceMetrics,
    per_interface: Arc<Mutex<NetworkInterfaceMetricsRegistryState>>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct NetworkInterfaceMetricsCaptureEntry {
    generation: u64,
    iface_id: String,
    metrics: NetworkInterfaceMetrics,
}

impl NetworkInterfaceMetricsCaptureEntry {
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub const fn metrics(&self) -> NetworkInterfaceMetrics {
        self.metrics
    }
}

impl fmt::Debug for NetworkInterfaceMetricsCaptureEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkInterfaceMetricsCaptureEntry")
            .field("generation", &self.generation)
            .field("iface_id", &"<redacted>")
            .field("metrics", &"<captured>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NetworkInterfaceMetricsCaptureState {
    aggregate: NetworkInterfaceMetrics,
    entries: Vec<NetworkInterfaceMetricsCaptureEntry>,
    next_generation: u64,
}

impl NetworkInterfaceMetricsCaptureState {
    pub const fn aggregate(&self) -> NetworkInterfaceMetrics {
        self.aggregate
    }

    pub fn entries(&self) -> &[NetworkInterfaceMetricsCaptureEntry] {
        &self.entries
    }

    pub const fn next_generation(&self) -> u64 {
        self.next_generation
    }
}

impl fmt::Debug for NetworkInterfaceMetricsCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkInterfaceMetricsCaptureState")
            .field("entry_count", &self.entries.len())
            .field("next_generation", &self.next_generation)
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkInterfaceMetricsCaptureError {
    Allocation,
    ReservationInFlight,
    CapacityMismatch,
    DuplicateInterface,
    DuplicateGeneration,
    InvalidGeneration,
}

impl fmt::Display for NetworkInterfaceMetricsCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Allocation => "network metrics capture allocation failed",
            Self::ReservationInFlight => "network metrics ownership reservation is in flight",
            Self::CapacityMismatch => "network metrics ownership exceeds its bounded capacity",
            Self::DuplicateInterface => "network metrics capture has a duplicate interface",
            Self::DuplicateGeneration => "network metrics capture has a duplicate generation",
            Self::InvalidGeneration => "network metrics capture generation cursor is invalid",
        })
    }
}

impl std::error::Error for NetworkInterfaceMetricsCaptureError {}

#[derive(Debug, Default)]
struct NetworkInterfaceMetricsRegistryState {
    entries: Vec<NetworkInterfaceMetricsRegistryEntry>,
    reservations: Vec<NetworkInterfaceMetricsReservation>,
    next_generation: u64,
    capacity: usize,
}

#[derive(Debug)]
struct NetworkInterfaceMetricsRegistryEntry {
    generation: u64,
    iface_id: String,
    metrics: SharedNetworkInterfaceMetrics,
    lease_claimed: bool,
}

#[derive(Debug)]
struct NetworkInterfaceMetricsReservation {
    generation: u64,
    iface_id: String,
}

/// Prepared per-interface metrics ownership that is invisible until
/// publication.
pub struct PreparedNetworkInterfaceMetrics {
    registry: SharedNetworkInterfaceMetricsRegistry,
    generation: u64,
    iface_id: String,
    metrics: SharedNetworkInterfaceMetrics,
    reserved: bool,
}

/// Exact live per-interface metrics ownership removed automatically on drop.
pub struct NetworkInterfaceMetricsLease {
    registry: SharedNetworkInterfaceMetricsRegistry,
    generation: u64,
    iface_id: String,
    registered: bool,
}

impl NetworkInterfaceMetricsLease {
    /// Returns whether this exact generation lease belongs to `registry`.
    #[doc(hidden)]
    pub fn belongs_to(&self, registry: &SharedNetworkInterfaceMetricsRegistry) -> bool {
        self.registered
            && Arc::ptr_eq(&self.registry.aggregate.inner, &registry.aggregate.inner)
            && Arc::ptr_eq(&self.registry.per_interface, &registry.per_interface)
            && lock_network_metrics_registry(&registry.per_interface)
                .entries
                .iter()
                .any(|entry| {
                    entry.generation == self.generation
                        && entry.iface_id == self.iface_id
                        && entry.lease_claimed
                })
    }

    /// Returns the leased interface identity without transferring ownership.
    #[doc(hidden)]
    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    /// Returns the exact metrics generation held by this lease.
    #[doc(hidden)]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl fmt::Debug for PreparedNetworkInterfaceMetrics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedNetworkInterfaceMetrics")
            .field("ownership", &"<redacted>")
            .field("reserved", &self.reserved)
            .finish()
    }
}

impl fmt::Debug for NetworkInterfaceMetricsLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkInterfaceMetricsLease")
            .field("ownership", &"<redacted>")
            .field("registered", &self.registered)
            .finish()
    }
}

impl PreparedNetworkInterfaceMetrics {
    pub fn metrics(&self) -> SharedNetworkInterfaceMetrics {
        self.metrics.clone()
    }

    pub fn publish(mut self) -> NetworkInterfaceMetricsLease {
        let mut state = lock_network_metrics_registry(&self.registry.per_interface);
        let reservation_count = state.reservations.len();
        state.reservations.retain(|reservation| {
            reservation.generation != self.generation || reservation.iface_id != self.iface_id
        });
        debug_assert_eq!(
            state.reservations.len().checked_add(1),
            Some(reservation_count)
        );
        self.reserved = false;
        debug_assert!(state.entries.len() < state.capacity);
        debug_assert!(
            !state
                .entries
                .iter()
                .any(|entry| entry.iface_id == self.iface_id)
        );
        state.entries.push(NetworkInterfaceMetricsRegistryEntry {
            generation: self.generation,
            iface_id: self.iface_id.clone(),
            metrics: self.metrics.clone(),
            lease_claimed: true,
        });
        drop(state);
        NetworkInterfaceMetricsLease {
            registry: self.registry.clone(),
            generation: self.generation,
            iface_id: self.iface_id.clone(),
            registered: true,
        }
    }
}

impl Drop for PreparedNetworkInterfaceMetrics {
    fn drop(&mut self) {
        if !self.reserved {
            return;
        }
        let mut state = lock_network_metrics_registry(&self.registry.per_interface);
        if let Some(index) = state.reservations.iter().position(|reservation| {
            reservation.generation == self.generation && reservation.iface_id == self.iface_id
        }) {
            state.reservations.remove(index);
        }
        self.reserved = false;
    }
}

impl Drop for NetworkInterfaceMetricsLease {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let mut state = lock_network_metrics_registry(&self.registry.per_interface);
        if let Some(index) = state.entries.iter().position(|entry| {
            entry.generation == self.generation && entry.iface_id == self.iface_id
        }) {
            state.entries.remove(index);
        }
        self.registered = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkInterfaceMetricsRegistryError {
    DuplicateInterface,
    UnknownInterface,
    LeaseAlreadyClaimed,
    Capacity,
    GenerationExhausted,
}

impl fmt::Display for NetworkInterfaceMetricsRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInterface => {
                formatter.write_str("network metrics interface is already registered")
            }
            Self::UnknownInterface => {
                formatter.write_str("network metrics interface is not registered")
            }
            Self::LeaseAlreadyClaimed => {
                formatter.write_str("network metrics interface lease is already claimed")
            }
            Self::Capacity => formatter.write_str("failed to reserve network metrics capacity"),
            Self::GenerationExhausted => {
                formatter.write_str("network metrics ownership generation is exhausted")
            }
        }
    }
}

impl std::error::Error for NetworkInterfaceMetricsRegistryError {}

impl Default for SharedNetworkInterfaceMetricsRegistry {
    fn default() -> Self {
        Self {
            aggregate: SharedNetworkInterfaceMetrics::default(),
            per_interface: Arc::new(Mutex::new(NetworkInterfaceMetricsRegistryState::default())),
        }
    }
}

impl SharedNetworkInterfaceMetricsRegistry {
    /// Returns whether two handles share the complete aggregate and
    /// per-interface registry identity.
    #[doc(hidden)]
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.aggregate.inner, &other.aggregate.inner)
            && Arc::ptr_eq(&self.per_interface, &other.per_interface)
    }

    pub fn from_interface_ids<'a>(iface_ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut entries = Vec::new();
        for iface_id in iface_ids {
            if entries
                .iter()
                .any(|entry: &NetworkInterfaceMetricsRegistryEntry| entry.iface_id == iface_id)
            {
                continue;
            }
            let generation = u64::try_from(entries.len()).unwrap_or(u64::MAX);
            entries.push(NetworkInterfaceMetricsRegistryEntry {
                generation,
                iface_id: iface_id.to_string(),
                metrics: SharedNetworkInterfaceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        Self {
            aggregate: SharedNetworkInterfaceMetrics::default(),
            per_interface: Arc::new(Mutex::new(NetworkInterfaceMetricsRegistryState {
                capacity: entries.len(),
                entries,
                reservations: Vec::new(),
                next_generation,
            })),
        }
    }

    pub fn from_interface_ids_with_capacity<'a>(
        iface_ids: impl IntoIterator<Item = &'a str>,
        capacity: usize,
    ) -> Result<Self, NetworkInterfaceMetricsRegistryError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| NetworkInterfaceMetricsRegistryError::Capacity)?;
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(capacity)
            .map_err(|_| NetworkInterfaceMetricsRegistryError::Capacity)?;
        for iface_id in iface_ids {
            if entries
                .iter()
                .any(|entry: &NetworkInterfaceMetricsRegistryEntry| entry.iface_id == iface_id)
            {
                continue;
            }
            if entries.len() == capacity {
                return Err(NetworkInterfaceMetricsRegistryError::Capacity);
            }
            let generation = u64::try_from(entries.len())
                .map_err(|_| NetworkInterfaceMetricsRegistryError::GenerationExhausted)?;
            entries.push(NetworkInterfaceMetricsRegistryEntry {
                generation,
                iface_id: iface_id.to_string(),
                metrics: SharedNetworkInterfaceMetrics::default(),
                lease_claimed: false,
            });
        }
        let next_generation = u64::try_from(entries.len())
            .map_err(|_| NetworkInterfaceMetricsRegistryError::GenerationExhausted)?;
        Ok(Self {
            aggregate: SharedNetworkInterfaceMetrics::default(),
            per_interface: Arc::new(Mutex::new(NetworkInterfaceMetricsRegistryState {
                entries,
                reservations,
                next_generation,
                capacity,
            })),
        })
    }

    pub fn prepare_interface(
        &self,
        iface_id: impl Into<String>,
    ) -> Result<PreparedNetworkInterfaceMetrics, NetworkInterfaceMetricsRegistryError> {
        let iface_id = iface_id.into();
        let mut state = lock_network_metrics_registry(&self.per_interface);
        if state.entries.iter().any(|entry| entry.iface_id == iface_id)
            || state
                .reservations
                .iter()
                .any(|reservation| reservation.iface_id == iface_id)
        {
            return Err(NetworkInterfaceMetricsRegistryError::DuplicateInterface);
        }
        let claimed_capacity = state
            .entries
            .len()
            .checked_add(state.reservations.len())
            .ok_or(NetworkInterfaceMetricsRegistryError::Capacity)?;
        if claimed_capacity >= state.capacity {
            return Err(NetworkInterfaceMetricsRegistryError::Capacity);
        }
        state
            .entries
            .try_reserve_exact(1)
            .map_err(|_| NetworkInterfaceMetricsRegistryError::Capacity)?;
        state
            .reservations
            .try_reserve_exact(1)
            .map_err(|_| NetworkInterfaceMetricsRegistryError::Capacity)?;
        let next_generation = state
            .next_generation
            .checked_add(1)
            .ok_or(NetworkInterfaceMetricsRegistryError::GenerationExhausted)?;
        let generation = state.next_generation;
        state.next_generation = next_generation;
        state.reservations.push(NetworkInterfaceMetricsReservation {
            generation,
            iface_id: iface_id.clone(),
        });
        drop(state);
        Ok(PreparedNetworkInterfaceMetrics {
            registry: self.clone(),
            generation,
            iface_id,
            metrics: SharedNetworkInterfaceMetrics::default(),
            reserved: true,
        })
    }

    /// Claims drop ownership for one interface registered during bounded
    /// startup.
    pub fn claim_interface_lease(
        &self,
        iface_id: &str,
    ) -> Result<NetworkInterfaceMetricsLease, NetworkInterfaceMetricsRegistryError> {
        let mut state = lock_network_metrics_registry(&self.per_interface);
        let entry = state
            .entries
            .iter_mut()
            .find(|entry| entry.iface_id == iface_id)
            .ok_or(NetworkInterfaceMetricsRegistryError::UnknownInterface)?;
        if entry.lease_claimed {
            return Err(NetworkInterfaceMetricsRegistryError::LeaseAlreadyClaimed);
        }
        entry.lease_claimed = true;
        let generation = entry.generation;
        drop(state);
        Ok(NetworkInterfaceMetricsLease {
            registry: self.clone(),
            generation,
            iface_id: iface_id.to_string(),
            registered: true,
        })
    }

    pub fn aggregate(&self) -> SharedNetworkInterfaceMetrics {
        self.aggregate.clone()
    }

    pub fn per_interface(&self, iface_id: &str) -> Option<SharedNetworkInterfaceMetrics> {
        lock_network_metrics_registry(&self.per_interface)
            .entries
            .iter()
            .find_map(|entry| (entry.iface_id == iface_id).then(|| entry.metrics.clone()))
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

    pub fn record_activation_failure_for_interface(&self, iface_id: &str) {
        self.aggregate.record_activation_failure();
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_activation_failure();
        }
    }

    pub fn record_config_failure_for_interface(&self, iface_id: &str) {
        self.aggregate.record_config_failure();
        if let Some(metrics) = self.per_interface(iface_id) {
            metrics.record_config_failure();
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
        for entry in &lock_network_metrics_registry(&self.per_interface).entries {
            let metrics = entry.metrics.snapshot();
            if !metrics.is_empty() {
                snapshot.insert_interface_metrics(entry.iface_id.clone(), metrics);
            }
        }
        snapshot
    }

    /// Captures every live generation, including entries whose counters are
    /// all zero, for snapshot continuity validation.
    pub fn capture_state(
        &self,
    ) -> Result<NetworkInterfaceMetricsCaptureState, NetworkInterfaceMetricsCaptureError> {
        let state = lock_network_metrics_registry(&self.per_interface);
        if !state.reservations.is_empty() {
            return Err(NetworkInterfaceMetricsCaptureError::ReservationInFlight);
        }
        if state.entries.len() > state.capacity {
            return Err(NetworkInterfaceMetricsCaptureError::CapacityMismatch);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(state.entries.len())
            .map_err(|_| NetworkInterfaceMetricsCaptureError::Allocation)?;
        for entry in &state.entries {
            if entry.generation >= state.next_generation {
                return Err(NetworkInterfaceMetricsCaptureError::InvalidGeneration);
            }
            if entries
                .iter()
                .any(|captured: &NetworkInterfaceMetricsCaptureEntry| {
                    captured.iface_id == entry.iface_id
                })
            {
                return Err(NetworkInterfaceMetricsCaptureError::DuplicateInterface);
            }
            if entries
                .iter()
                .any(|captured| captured.generation == entry.generation)
            {
                return Err(NetworkInterfaceMetricsCaptureError::DuplicateGeneration);
            }
            entries.push(NetworkInterfaceMetricsCaptureEntry {
                generation: entry.generation,
                iface_id: entry.iface_id.clone(),
                metrics: entry.metrics.snapshot(),
            });
        }
        Ok(NetworkInterfaceMetricsCaptureState {
            aggregate: self.aggregate.snapshot(),
            entries,
            next_generation: state.next_generation,
        })
    }
}

fn lock_network_metrics_registry(
    registry: &Mutex<NetworkInterfaceMetricsRegistryState>,
) -> MutexGuard<'_, NetworkInterfaceMetricsRegistryState> {
    match registry.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MmdsMetrics {
    rx_accepted: u64,
    rx_accepted_err: u64,
    rx_accepted_unusual: u64,
    rx_bad_eth: u64,
    rx_invalid_token: u64,
    rx_no_token: u64,
    rx_count: u64,
    tx_bytes: u64,
    tx_count: u64,
    tx_errors: u64,
    tx_frames: u64,
    connections_created: u64,
    connections_destroyed: u64,
}

impl_incremental_delta!(MmdsMetrics {
    rx_accepted,
    rx_accepted_err,
    rx_accepted_unusual,
    rx_bad_eth,
    rx_invalid_token,
    rx_no_token,
    rx_count,
    tx_bytes,
    tx_count,
    tx_errors,
    tx_frames,
    connections_created,
    connections_destroyed,
});

impl MmdsMetrics {
    pub const fn is_empty(self) -> bool {
        self.rx_accepted == 0
            && self.rx_accepted_err == 0
            && self.rx_accepted_unusual == 0
            && self.rx_bad_eth == 0
            && self.rx_invalid_token == 0
            && self.rx_no_token == 0
            && self.rx_count == 0
            && self.tx_bytes == 0
            && self.tx_count == 0
            && self.tx_errors == 0
            && self.tx_frames == 0
            && self.connections_created == 0
            && self.connections_destroyed == 0
    }

    pub const fn rx_accepted(self) -> u64 {
        self.rx_accepted
    }

    pub const fn rx_accepted_err(self) -> u64 {
        self.rx_accepted_err
    }

    pub const fn rx_accepted_unusual(self) -> u64 {
        self.rx_accepted_unusual
    }

    pub const fn rx_bad_eth(self) -> u64 {
        self.rx_bad_eth
    }

    pub const fn rx_invalid_token(self) -> u64 {
        self.rx_invalid_token
    }

    pub const fn rx_no_token(self) -> u64 {
        self.rx_no_token
    }

    pub const fn rx_count(self) -> u64 {
        self.rx_count
    }

    pub const fn tx_bytes(self) -> u64 {
        self.tx_bytes
    }

    pub const fn tx_count(self) -> u64 {
        self.tx_count
    }

    pub const fn tx_errors(self) -> u64 {
        self.tx_errors
    }

    pub const fn tx_frames(self) -> u64 {
        self.tx_frames
    }

    pub const fn connections_created(self) -> u64 {
        self.connections_created
    }

    pub const fn connections_destroyed(self) -> u64 {
        self.connections_destroyed
    }

    pub const fn with_rx_accepted(mut self, rx_accepted: u64) -> Self {
        self.rx_accepted = rx_accepted;
        self
    }

    pub const fn with_rx_accepted_err(mut self, rx_accepted_err: u64) -> Self {
        self.rx_accepted_err = rx_accepted_err;
        self
    }

    pub const fn with_rx_accepted_unusual(mut self, rx_accepted_unusual: u64) -> Self {
        self.rx_accepted_unusual = rx_accepted_unusual;
        self
    }

    pub const fn with_rx_bad_eth(mut self, rx_bad_eth: u64) -> Self {
        self.rx_bad_eth = rx_bad_eth;
        self
    }

    pub const fn with_rx_invalid_token(mut self, rx_invalid_token: u64) -> Self {
        self.rx_invalid_token = rx_invalid_token;
        self
    }

    pub const fn with_rx_no_token(mut self, rx_no_token: u64) -> Self {
        self.rx_no_token = rx_no_token;
        self
    }

    pub const fn with_rx_count(mut self, rx_count: u64) -> Self {
        self.rx_count = rx_count;
        self
    }

    pub const fn with_tx_bytes(mut self, tx_bytes: u64) -> Self {
        self.tx_bytes = tx_bytes;
        self
    }

    pub const fn with_tx_count(mut self, tx_count: u64) -> Self {
        self.tx_count = tx_count;
        self
    }

    pub const fn with_tx_errors(mut self, tx_errors: u64) -> Self {
        self.tx_errors = tx_errors;
        self
    }

    pub const fn with_tx_frames(mut self, tx_frames: u64) -> Self {
        self.tx_frames = tx_frames;
        self
    }

    pub const fn with_connections_created(mut self, connections_created: u64) -> Self {
        self.connections_created = connections_created;
        self
    }

    pub const fn with_connections_destroyed(mut self, connections_destroyed: u64) -> Self {
        self.connections_destroyed = connections_destroyed;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            rx_accepted: self.rx_accepted.saturating_add(other.rx_accepted),
            rx_accepted_err: self.rx_accepted_err.saturating_add(other.rx_accepted_err),
            rx_accepted_unusual: self
                .rx_accepted_unusual
                .saturating_add(other.rx_accepted_unusual),
            rx_bad_eth: self.rx_bad_eth.saturating_add(other.rx_bad_eth),
            rx_invalid_token: self.rx_invalid_token.saturating_add(other.rx_invalid_token),
            rx_no_token: self.rx_no_token.saturating_add(other.rx_no_token),
            rx_count: self.rx_count.saturating_add(other.rx_count),
            tx_bytes: self.tx_bytes.saturating_add(other.tx_bytes),
            tx_count: self.tx_count.saturating_add(other.tx_count),
            tx_errors: self.tx_errors.saturating_add(other.tx_errors),
            tx_frames: self.tx_frames.saturating_add(other.tx_frames),
            connections_created: self
                .connections_created
                .saturating_add(other.connections_created),
            connections_destroyed: self
                .connections_destroyed
                .saturating_add(other.connections_destroyed),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedMmdsMetrics {
    inner: Arc<SharedMmdsMetricsInner>,
}

impl SharedMmdsMetrics {
    #[doc(hidden)]
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn record_rx_accepted(&self) {
        record_atomic_metric(&self.inner.rx_accepted, 1);
    }

    pub fn record_rx_accepted_error(&self) {
        record_atomic_metric(&self.inner.rx_accepted_err, 1);
    }

    pub fn record_rx_accepted_unusual(&self) {
        record_atomic_metric(&self.inner.rx_accepted_unusual, 1);
    }

    pub fn record_rx_bad_eth(&self) {
        record_atomic_metric(&self.inner.rx_bad_eth, 1);
    }

    pub fn record_rx_invalid_token(&self) {
        record_atomic_metric(&self.inner.rx_invalid_token, 1);
    }

    pub fn record_rx_no_token(&self) {
        record_atomic_metric(&self.inner.rx_no_token, 1);
    }

    pub fn record_rx_count(&self) {
        record_atomic_metric(&self.inner.rx_count, 1);
    }

    pub fn record_tx_frame(&self, len: usize) {
        record_atomic_metric(&self.inner.tx_count, 1);
        record_atomic_metric(&self.inner.tx_frames, 1);
        record_atomic_metric(&self.inner.tx_bytes, usize_to_u64_saturating(len));
    }

    pub fn record_tx_error(&self) {
        record_atomic_metric(&self.inner.tx_errors, 1);
    }

    pub fn record_connection_created(&self) {
        record_atomic_metric(&self.inner.connections_created, 1);
    }

    pub fn record_connection_destroyed(&self) {
        record_atomic_metric(&self.inner.connections_destroyed, 1);
    }

    pub fn snapshot(&self) -> MmdsMetrics {
        MmdsMetrics {
            rx_accepted: self.inner.rx_accepted.load(Ordering::Relaxed),
            rx_accepted_err: self.inner.rx_accepted_err.load(Ordering::Relaxed),
            rx_accepted_unusual: self.inner.rx_accepted_unusual.load(Ordering::Relaxed),
            rx_bad_eth: self.inner.rx_bad_eth.load(Ordering::Relaxed),
            rx_invalid_token: self.inner.rx_invalid_token.load(Ordering::Relaxed),
            rx_no_token: self.inner.rx_no_token.load(Ordering::Relaxed),
            rx_count: self.inner.rx_count.load(Ordering::Relaxed),
            tx_bytes: self.inner.tx_bytes.load(Ordering::Relaxed),
            tx_count: self.inner.tx_count.load(Ordering::Relaxed),
            tx_errors: self.inner.tx_errors.load(Ordering::Relaxed),
            tx_frames: self.inner.tx_frames.load(Ordering::Relaxed),
            connections_created: self.inner.connections_created.load(Ordering::Relaxed),
            connections_destroyed: self.inner.connections_destroyed.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
struct SharedMmdsMetricsInner {
    rx_accepted: AtomicU64,
    rx_accepted_err: AtomicU64,
    rx_accepted_unusual: AtomicU64,
    rx_bad_eth: AtomicU64,
    rx_invalid_token: AtomicU64,
    rx_no_token: AtomicU64,
    rx_count: AtomicU64,
    tx_bytes: AtomicU64,
    tx_count: AtomicU64,
    tx_errors: AtomicU64,
    tx_frames: AtomicU64,
    connections_created: AtomicU64,
    connections_destroyed: AtomicU64,
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

impl_incremental_delta!(VsockDeviceMetrics {
    activate_fails,
    cfg_fails,
    rx_queue_event_fails,
    tx_queue_event_fails,
    ev_queue_event_fails,
    muxer_event_fails,
    conn_event_fails,
    rx_queue_event_count,
    tx_queue_event_count,
    rx_bytes_count,
    tx_bytes_count,
    rx_packets_count,
    tx_packets_count,
    conns_added,
    conns_killed,
    conns_removed,
    killq_resync,
    tx_flush_fails,
    tx_write_fails,
    rx_read_fails,
});

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
    #[doc(hidden)]
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn record_activation_failure(&self) {
        record_atomic_metric(&self.inner.activate_fails, 1);
    }

    pub fn record_config_failure(&self) {
        record_atomic_metric(&self.inner.cfg_fails, 1);
    }

    pub fn record_transport_reset_attempt(
        &self,
        attempt: &Result<VirtioVsockTransportResetAttempt, VirtioVsockTransportResetError>,
    ) {
        if matches!(
            attempt,
            Ok(VirtioVsockTransportResetAttempt::QueueEmpty) | Err(_)
        ) {
            self.record_event_queue_failure();
        }
    }

    pub fn record_event_queue_signal_failure(&self) {
        self.record_event_queue_failure();
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
            .saturating_add(dispatch.delivered_shutdown_packets())
            .saturating_add(dispatch.delivered_credit_requests())
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
    source_provider_fails: u64,
}

impl_incremental_delta!(EntropyDeviceMetrics {
    activate_fails,
    entropy_event_fails,
    entropy_event_count,
    entropy_bytes,
    host_rng_fails,
    entropy_rate_limiter_throttled,
    rate_limiter_event_count,
    source_provider_fails,
});

impl EntropyDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.entropy_event_fails == 0
            && self.entropy_event_count == 0
            && self.entropy_bytes == 0
            && self.host_rng_fails == 0
            && self.entropy_rate_limiter_throttled == 0
            && self.rate_limiter_event_count == 0
            && self.source_provider_fails == 0
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

    /// Bangbang-only failures while acquiring a source before a descriptor is popped.
    pub const fn source_provider_fails(self) -> u64 {
        self.source_provider_fails
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

    pub const fn with_source_provider_fails(mut self, source_provider_fails: u64) -> Self {
        self.source_provider_fails = source_provider_fails;
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
            source_provider_fails: self
                .source_provider_fails
                .saturating_add(other.source_provider_fails),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedEntropyDeviceMetrics {
    inner: Arc<Mutex<EntropyDeviceMetrics>>,
}

impl SharedEntropyDeviceMetrics {
    pub fn record_activation_failure(&self) {
        self.record(EntropyDeviceMetrics::default().with_activate_fails(1));
    }

    pub fn record_notification_dispatch(&self, dispatch: &VirtioRngDeviceNotificationDispatch) {
        if let Some(queue_dispatch) = dispatch.queue_dispatch() {
            self.record(entropy_queue_dispatch_metrics(queue_dispatch));
        }
    }

    pub fn record_notification_error(&self, source: &VirtioRngDeviceNotificationError) {
        let mut observation = EntropyDeviceMetrics::default().with_entropy_event_fails(1);
        if let Some(completed) = source.completed_dispatch() {
            observation = observation.merged_with(entropy_queue_dispatch_metrics(completed));
        }
        self.record(observation);
    }

    pub fn record_entropy_source_provider_failure(&self) {
        self.record(EntropyDeviceMetrics::default().with_source_provider_fails(1));
    }

    pub fn record_event_failure(&self) {
        self.record(EntropyDeviceMetrics::default().with_entropy_event_fails(1));
    }

    pub fn record_host_rng_failure(&self) {
        self.record(EntropyDeviceMetrics::default().with_host_rng_fails(1));
    }

    pub fn snapshot(&self) -> EntropyDeviceMetrics {
        *lock_entropy_device_metrics(&self.inner)
    }

    pub fn record_queue_dispatch(&self, dispatch: &VirtioRngQueueDispatch) {
        self.record(entropy_queue_dispatch_metrics(dispatch));
    }

    fn record(&self, observation: EntropyDeviceMetrics) {
        if observation.is_empty() {
            return;
        }
        let mut metrics = lock_entropy_device_metrics(&self.inner);
        *metrics = metrics.merged_with(observation);
    }
}

fn entropy_queue_dispatch_metrics(dispatch: &VirtioRngQueueDispatch) -> EntropyDeviceMetrics {
    EntropyDeviceMetrics::default()
        .with_entropy_event_count(usize_to_u64_saturating(dispatch.attempted_requests()))
        .with_entropy_bytes(dispatch.bytes_written_to_guest())
        .with_entropy_event_fails(usize_to_u64_saturating(
            dispatch
                .buffer_parse_failures()
                .saturating_add(dispatch.source_failures()),
        ))
        .with_host_rng_fails(usize_to_u64_saturating(dispatch.source_failures()))
        .with_entropy_rate_limiter_throttled(usize_to_u64_saturating(
            dispatch.rate_limiter_throttled_requests(),
        ))
        .with_rate_limiter_event_count(usize_to_u64_saturating(dispatch.rate_limiter_events()))
}

fn lock_entropy_device_metrics(
    metrics: &Mutex<EntropyDeviceMetrics>,
) -> MutexGuard<'_, EntropyDeviceMetrics> {
    match metrics.lock() {
        Ok(metrics) => metrics,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RtcDeviceMetrics {
    error_count: u64,
    missed_read_count: u64,
    missed_write_count: u64,
}

impl_incremental_delta!(RtcDeviceMetrics {
    error_count,
    missed_read_count,
    missed_write_count,
});

impl RtcDeviceMetrics {
    pub const fn new(error_count: u64, missed_read_count: u64, missed_write_count: u64) -> Self {
        Self {
            error_count,
            missed_read_count,
            missed_write_count,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.error_count == 0 && self.missed_read_count == 0 && self.missed_write_count == 0
    }

    pub const fn error_count(self) -> u64 {
        self.error_count
    }

    pub const fn missed_read_count(self) -> u64 {
        self.missed_read_count
    }

    pub const fn missed_write_count(self) -> u64 {
        self.missed_write_count
    }

    pub const fn with_error_count(mut self, error_count: u64) -> Self {
        self.error_count = error_count;
        self
    }

    pub const fn with_missed_read_count(mut self, missed_read_count: u64) -> Self {
        self.missed_read_count = missed_read_count;
        self
    }

    pub const fn with_missed_write_count(mut self, missed_write_count: u64) -> Self {
        self.missed_write_count = missed_write_count;
        self
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            error_count: self.error_count.saturating_add(other.error_count),
            missed_read_count: self
                .missed_read_count
                .saturating_add(other.missed_read_count),
            missed_write_count: self
                .missed_write_count
                .saturating_add(other.missed_write_count),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedRtcDeviceMetrics {
    inner: Arc<Mutex<RtcDeviceMetrics>>,
}

impl SharedRtcDeviceMetrics {
    pub fn record_access_error(&self) {
        self.record(RtcDeviceMetrics::default().with_error_count(1));
    }

    pub fn record_read_error(&self) {
        self.record(
            RtcDeviceMetrics::default()
                .with_error_count(1)
                .with_missed_read_count(1),
        );
    }

    pub fn record_write_error(&self) {
        self.record(
            RtcDeviceMetrics::default()
                .with_error_count(1)
                .with_missed_write_count(1),
        );
    }

    pub fn snapshot(&self) -> RtcDeviceMetrics {
        *lock_rtc_device_metrics(&self.inner)
    }

    fn record(&self, observation: RtcDeviceMetrics) {
        let mut metrics = lock_rtc_device_metrics(&self.inner);
        *metrics = metrics.merged_with(observation);
    }
}

fn lock_rtc_device_metrics(metrics: &Mutex<RtcDeviceMetrics>) -> MutexGuard<'_, RtcDeviceMetrics> {
    match metrics.lock() {
        Ok(metrics) => metrics,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Observable counters for one balloon host-discard source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BalloonDiscardMetrics {
    attempts: u64,
    completed_bytes: u64,
    advised_bytes: u64,
    skipped_bytes: u64,
    failures: u64,
}

impl_incremental_delta!(BalloonDiscardMetrics {
    attempts,
    completed_bytes,
    advised_bytes,
    skipped_bytes,
    failures,
});

impl BalloonDiscardMetrics {
    pub const fn new(attempts: u64, advised_bytes: u64, skipped_bytes: u64, failures: u64) -> Self {
        Self {
            attempts,
            completed_bytes: advised_bytes,
            advised_bytes,
            skipped_bytes,
            failures,
        }
    }

    pub const fn attempts(self) -> u64 {
        self.attempts
    }

    pub const fn with_completed_bytes(mut self, completed_bytes: u64) -> Self {
        self.completed_bytes = completed_bytes;
        self
    }

    pub const fn completed_bytes(self) -> u64 {
        self.completed_bytes
    }

    pub const fn advised_bytes(self) -> u64 {
        self.advised_bytes
    }

    pub const fn skipped_bytes(self) -> u64 {
        self.skipped_bytes
    }

    pub const fn failures(self) -> u64 {
        self.failures
    }

    const fn is_empty(self) -> bool {
        self.attempts == 0
            && self.completed_bytes == 0
            && self.advised_bytes == 0
            && self.skipped_bytes == 0
            && self.failures == 0
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            attempts: self.attempts.saturating_add(other.attempts),
            completed_bytes: self.completed_bytes.saturating_add(other.completed_bytes),
            advised_bytes: self.advised_bytes.saturating_add(other.advised_bytes),
            skipped_bytes: self.skipped_bytes.saturating_add(other.skipped_bytes),
            failures: self.failures.saturating_add(other.failures),
        }
    }
}

impl From<VirtioBalloonDiscardOutcome> for BalloonDiscardMetrics {
    fn from(outcome: VirtioBalloonDiscardOutcome) -> Self {
        Self::new(
            outcome.attempts(),
            outcome.advised_bytes(),
            outcome.skipped_bytes(),
            outcome.failures(),
        )
        .with_completed_bytes(outcome.completed_bytes())
    }
}

/// Observable counters for virtio-balloon free-page reporting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BalloonFreePageReportMetrics {
    count: u64,
    requested_bytes: u64,
    completed_bytes: u64,
    advised_bytes: u64,
    skipped_bytes: u64,
    failures: u64,
}

impl_incremental_delta!(BalloonFreePageReportMetrics {
    count,
    requested_bytes,
    completed_bytes,
    advised_bytes,
    skipped_bytes,
    failures,
});

impl BalloonFreePageReportMetrics {
    pub const fn new(
        count: u64,
        requested_bytes: u64,
        advised_bytes: u64,
        skipped_bytes: u64,
        failures: u64,
    ) -> Self {
        Self {
            count,
            requested_bytes,
            completed_bytes: advised_bytes,
            advised_bytes,
            skipped_bytes,
            failures,
        }
    }

    pub const fn count(self) -> u64 {
        self.count
    }

    pub const fn requested_bytes(self) -> u64 {
        self.requested_bytes
    }

    pub const fn with_completed_bytes(mut self, completed_bytes: u64) -> Self {
        self.completed_bytes = completed_bytes;
        self
    }

    pub const fn completed_bytes(self) -> u64 {
        self.completed_bytes
    }

    pub const fn advised_bytes(self) -> u64 {
        self.advised_bytes
    }

    pub const fn skipped_bytes(self) -> u64 {
        self.skipped_bytes
    }

    pub const fn failures(self) -> u64 {
        self.failures
    }

    const fn is_empty(self) -> bool {
        self.count == 0
            && self.requested_bytes == 0
            && self.completed_bytes == 0
            && self.advised_bytes == 0
            && self.skipped_bytes == 0
            && self.failures == 0
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            count: self.count.saturating_add(other.count),
            requested_bytes: self.requested_bytes.saturating_add(other.requested_bytes),
            completed_bytes: self.completed_bytes.saturating_add(other.completed_bytes),
            advised_bytes: self.advised_bytes.saturating_add(other.advised_bytes),
            skipped_bytes: self.skipped_bytes.saturating_add(other.skipped_bytes),
            failures: self.failures.saturating_add(other.failures),
        }
    }
}

impl From<VirtioBalloonDiscardOutcome> for BalloonFreePageReportMetrics {
    fn from(outcome: VirtioBalloonDiscardOutcome) -> Self {
        Self::new(
            outcome.attempts(),
            outcome.requested_bytes(),
            outcome.advised_bytes(),
            outcome.skipped_bytes(),
            outcome.failures(),
        )
        .with_completed_bytes(outcome.completed_bytes())
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
    inflate_discard: BalloonDiscardMetrics,
    hinting_discard: BalloonDiscardMetrics,
    free_page_report: BalloonFreePageReportMetrics,
}

impl BalloonDeviceMetrics {
    const fn delta_since(self, previous: Self) -> Self {
        Self {
            activate_fails: incremental_delta(self.activate_fails, previous.activate_fails),
            inflate_count: incremental_delta(self.inflate_count, previous.inflate_count),
            stats_updates_count: incremental_delta(
                self.stats_updates_count,
                previous.stats_updates_count,
            ),
            stats_update_fails: incremental_delta(
                self.stats_update_fails,
                previous.stats_update_fails,
            ),
            deflate_count: incremental_delta(self.deflate_count, previous.deflate_count),
            event_fails: incremental_delta(self.event_fails, previous.event_fails),
            inflate_discard: self.inflate_discard.delta_since(previous.inflate_discard),
            hinting_discard: self.hinting_discard.delta_since(previous.hinting_discard),
            free_page_report: self.free_page_report.delta_since(previous.free_page_report),
        }
    }

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
            inflate_discard: BalloonDiscardMetrics::new(0, 0, 0, 0),
            hinting_discard: BalloonDiscardMetrics::new(0, 0, 0, 0),
            free_page_report: BalloonFreePageReportMetrics::new(0, 0, 0, 0, 0),
        }
    }

    pub const fn with_discard_metrics(
        mut self,
        inflate_discard: BalloonDiscardMetrics,
        hinting_discard: BalloonDiscardMetrics,
    ) -> Self {
        self.inflate_discard = inflate_discard;
        self.hinting_discard = hinting_discard;
        self
    }

    pub const fn with_free_page_report_metrics(
        mut self,
        free_page_report: BalloonFreePageReportMetrics,
    ) -> Self {
        self.free_page_report = free_page_report;
        self
    }

    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.inflate_count == 0
            && self.stats_updates_count == 0
            && self.stats_update_fails == 0
            && self.deflate_count == 0
            && self.event_fails == 0
            && self.inflate_discard.is_empty()
            && self.hinting_discard.is_empty()
            && self.free_page_report.is_empty()
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

    pub const fn inflate_discard(self) -> BalloonDiscardMetrics {
        self.inflate_discard
    }

    pub const fn hinting_discard(self) -> BalloonDiscardMetrics {
        self.hinting_discard
    }

    pub const fn free_page_report(self) -> BalloonFreePageReportMetrics {
        self.free_page_report
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
            inflate_discard: self.inflate_discard.merged_with(other.inflate_discard),
            hinting_discard: self.hinting_discard.merged_with(other.hinting_discard),
            free_page_report: self.free_page_report.merged_with(other.free_page_report),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedBalloonDeviceMetrics {
    inner: Arc<Mutex<BalloonDeviceMetrics>>,
}

impl SharedBalloonDeviceMetrics {
    pub fn record_activation_failure(&self) {
        self.record(BalloonDeviceMetrics::new(1, 0, 0, 0, 0, 0));
    }

    pub fn record_notification_dispatch(&self, dispatch: &VirtioBalloonDeviceNotificationDispatch) {
        let statistics_dispatch = dispatch.statistics_queue_dispatch();
        let mut observation = BalloonDeviceMetrics::new(
            0,
            u64::from(dispatch.inflate_notifications() != 0),
            u64::from(dispatch.statistics_notifications() != 0),
            statistics_dispatch
                .map(|queue| usize_to_u64_saturating(queue.unrecognized_statistics()))
                .unwrap_or_default(),
            u64::from(dispatch.deflate_notifications() != 0),
            0,
        );
        if let Some(queue_dispatch) = dispatch.inflate_queue_dispatch() {
            observation.inflate_discard = queue_dispatch.inflate_discard().into();
        }
        if let Some(queue_dispatch) = dispatch.hinting_queue_dispatch() {
            observation.hinting_discard = queue_dispatch.hinting_discard().into();
        }
        if let Some(queue_dispatch) = dispatch.reporting_queue_dispatch() {
            observation.free_page_report = queue_dispatch.reporting_discard().into();
        }
        self.record(observation);
    }

    pub fn record_statistics_update_failure(&self) {
        self.record(BalloonDeviceMetrics::new(0, 0, 0, 1, 0, 0));
    }

    pub fn record_event_failure(&self) {
        self.record(BalloonDeviceMetrics::new(0, 0, 0, 0, 0, 1));
    }

    pub fn snapshot(&self) -> BalloonDeviceMetrics {
        *lock_balloon_device_metrics(&self.inner)
    }

    fn record(&self, observation: BalloonDeviceMetrics) {
        let mut metrics = lock_balloon_device_metrics(&self.inner);
        *metrics = metrics.merged_with(observation);
    }
}

fn lock_balloon_device_metrics(
    metrics: &Mutex<BalloonDeviceMetrics>,
) -> MutexGuard<'_, BalloonDeviceMetrics> {
    match metrics.lock() {
        Ok(metrics) => metrics,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Bounded latency aggregate for one virtio-mem operation family.
///
/// `sample_count` is retained for snapshot/delta correctness. The public JSON
/// shape intentionally matches Firecracker's `LatencyAggregateMetrics` and
/// therefore emits only `min_us`, `max_us`, and `sum_us`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryHotplugLatencyMetrics {
    min_us: u64,
    max_us: u64,
    sum_us: u64,
    sample_count: u64,
}

impl MemoryHotplugLatencyMetrics {
    pub const fn min_us(self) -> u64 {
        self.min_us
    }

    pub const fn max_us(self) -> u64 {
        self.max_us
    }

    pub const fn sum_us(self) -> u64 {
        self.sum_us
    }

    pub const fn sample_count(self) -> u64 {
        self.sample_count
    }

    pub const fn is_empty(self) -> bool {
        self.sample_count == 0
    }

    const fn delta_since(self, previous: Self) -> Self {
        let sample_count = incremental_delta(self.sample_count, previous.sample_count);
        Self {
            min_us: self.min_us,
            max_us: self.max_us,
            sum_us: incremental_delta(self.sum_us, previous.sum_us),
            sample_count,
        }
    }

    const fn from_sample(latency_us: u64) -> Self {
        Self {
            min_us: latency_us,
            max_us: latency_us,
            sum_us: latency_us,
            sample_count: 1,
        }
    }

    const fn merged_with(mut self, other: Self) -> Self {
        if other.is_empty() {
            return self;
        }
        if self.is_empty() || self.min_us == 0 || other.min_us < self.min_us {
            self.min_us = other.min_us;
        }
        if other.max_us > self.max_us {
            self.max_us = other.max_us;
        }
        self.sum_us = self.sum_us.saturating_add(other.sum_us);
        self.sample_count = self.sample_count.saturating_add(other.sample_count);
        self
    }
}

/// Firecracker-shaped singleton virtio-mem metrics plus Bangbang transaction
/// and owner-lifecycle extensions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryHotplugDeviceMetrics {
    activate_fails: u64,
    queue_event_fails: u64,
    queue_event_count: u64,
    plug_agg: MemoryHotplugLatencyMetrics,
    plug_count: u64,
    plug_bytes: u64,
    plug_fails: u64,
    unplug_agg: MemoryHotplugLatencyMetrics,
    unplug_count: u64,
    unplug_bytes: u64,
    unplug_fails: u64,
    unplug_discard_fails: u64,
    unplug_all_agg: MemoryHotplugLatencyMetrics,
    unplug_all_count: u64,
    unplug_all_fails: u64,
    state_agg: MemoryHotplugLatencyMetrics,
    state_count: u64,
    state_fails: u64,
    interrupt_fails: u64,
    rollback_count: u64,
    rollback_fails: u64,
    owner_cleanup_count: u64,
    owner_cleanup_fails: u64,
    teardown_count: u64,
    teardown_fails: u64,
}

impl MemoryHotplugDeviceMetrics {
    pub const fn is_empty(self) -> bool {
        self.activate_fails == 0
            && self.queue_event_fails == 0
            && self.queue_event_count == 0
            && self.plug_agg.is_empty()
            && self.plug_count == 0
            && self.plug_bytes == 0
            && self.plug_fails == 0
            && self.unplug_agg.is_empty()
            && self.unplug_count == 0
            && self.unplug_bytes == 0
            && self.unplug_fails == 0
            && self.unplug_discard_fails == 0
            && self.unplug_all_agg.is_empty()
            && self.unplug_all_count == 0
            && self.unplug_all_fails == 0
            && self.state_agg.is_empty()
            && self.state_count == 0
            && self.state_fails == 0
            && self.interrupt_fails == 0
            && self.rollback_count == 0
            && self.rollback_fails == 0
            && self.owner_cleanup_count == 0
            && self.owner_cleanup_fails == 0
            && self.teardown_count == 0
            && self.teardown_fails == 0
    }

    pub const fn activate_fails(self) -> u64 {
        self.activate_fails
    }

    pub const fn queue_event_fails(self) -> u64 {
        self.queue_event_fails
    }

    pub const fn queue_event_count(self) -> u64 {
        self.queue_event_count
    }

    pub const fn plug_agg(self) -> MemoryHotplugLatencyMetrics {
        self.plug_agg
    }

    pub const fn plug_count(self) -> u64 {
        self.plug_count
    }

    pub const fn plug_bytes(self) -> u64 {
        self.plug_bytes
    }

    pub const fn plug_fails(self) -> u64 {
        self.plug_fails
    }

    pub const fn unplug_agg(self) -> MemoryHotplugLatencyMetrics {
        self.unplug_agg
    }

    pub const fn unplug_count(self) -> u64 {
        self.unplug_count
    }

    pub const fn unplug_bytes(self) -> u64 {
        self.unplug_bytes
    }

    pub const fn unplug_fails(self) -> u64 {
        self.unplug_fails
    }

    pub const fn unplug_discard_fails(self) -> u64 {
        self.unplug_discard_fails
    }

    pub const fn unplug_all_agg(self) -> MemoryHotplugLatencyMetrics {
        self.unplug_all_agg
    }

    pub const fn unplug_all_count(self) -> u64 {
        self.unplug_all_count
    }

    pub const fn unplug_all_fails(self) -> u64 {
        self.unplug_all_fails
    }

    pub const fn state_agg(self) -> MemoryHotplugLatencyMetrics {
        self.state_agg
    }

    pub const fn state_count(self) -> u64 {
        self.state_count
    }

    pub const fn state_fails(self) -> u64 {
        self.state_fails
    }

    pub const fn interrupt_fails(self) -> u64 {
        self.interrupt_fails
    }

    pub const fn rollback_count(self) -> u64 {
        self.rollback_count
    }

    pub const fn rollback_fails(self) -> u64 {
        self.rollback_fails
    }

    pub const fn owner_cleanup_count(self) -> u64 {
        self.owner_cleanup_count
    }

    pub const fn owner_cleanup_fails(self) -> u64 {
        self.owner_cleanup_fails
    }

    pub const fn teardown_count(self) -> u64 {
        self.teardown_count
    }

    pub const fn teardown_fails(self) -> u64 {
        self.teardown_fails
    }

    const fn delta_since(self, previous: Self) -> Self {
        Self {
            activate_fails: incremental_delta(self.activate_fails, previous.activate_fails),
            queue_event_fails: incremental_delta(
                self.queue_event_fails,
                previous.queue_event_fails,
            ),
            queue_event_count: incremental_delta(
                self.queue_event_count,
                previous.queue_event_count,
            ),
            plug_agg: self.plug_agg.delta_since(previous.plug_agg),
            plug_count: incremental_delta(self.plug_count, previous.plug_count),
            plug_bytes: incremental_delta(self.plug_bytes, previous.plug_bytes),
            plug_fails: incremental_delta(self.plug_fails, previous.plug_fails),
            unplug_agg: self.unplug_agg.delta_since(previous.unplug_agg),
            unplug_count: incremental_delta(self.unplug_count, previous.unplug_count),
            unplug_bytes: incremental_delta(self.unplug_bytes, previous.unplug_bytes),
            unplug_fails: incremental_delta(self.unplug_fails, previous.unplug_fails),
            unplug_discard_fails: incremental_delta(
                self.unplug_discard_fails,
                previous.unplug_discard_fails,
            ),
            unplug_all_agg: self.unplug_all_agg.delta_since(previous.unplug_all_agg),
            unplug_all_count: incremental_delta(self.unplug_all_count, previous.unplug_all_count),
            unplug_all_fails: incremental_delta(self.unplug_all_fails, previous.unplug_all_fails),
            state_agg: self.state_agg.delta_since(previous.state_agg),
            state_count: incremental_delta(self.state_count, previous.state_count),
            state_fails: incremental_delta(self.state_fails, previous.state_fails),
            interrupt_fails: incremental_delta(self.interrupt_fails, previous.interrupt_fails),
            rollback_count: incremental_delta(self.rollback_count, previous.rollback_count),
            rollback_fails: incremental_delta(self.rollback_fails, previous.rollback_fails),
            owner_cleanup_count: incremental_delta(
                self.owner_cleanup_count,
                previous.owner_cleanup_count,
            ),
            owner_cleanup_fails: incremental_delta(
                self.owner_cleanup_fails,
                previous.owner_cleanup_fails,
            ),
            teardown_count: incremental_delta(self.teardown_count, previous.teardown_count),
            teardown_fails: incremental_delta(self.teardown_fails, previous.teardown_fails),
        }
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            activate_fails: self.activate_fails.saturating_add(other.activate_fails),
            queue_event_fails: self
                .queue_event_fails
                .saturating_add(other.queue_event_fails),
            queue_event_count: self
                .queue_event_count
                .saturating_add(other.queue_event_count),
            plug_agg: self.plug_agg.merged_with(other.plug_agg),
            plug_count: self.plug_count.saturating_add(other.plug_count),
            plug_bytes: self.plug_bytes.saturating_add(other.plug_bytes),
            plug_fails: self.plug_fails.saturating_add(other.plug_fails),
            unplug_agg: self.unplug_agg.merged_with(other.unplug_agg),
            unplug_count: self.unplug_count.saturating_add(other.unplug_count),
            unplug_bytes: self.unplug_bytes.saturating_add(other.unplug_bytes),
            unplug_fails: self.unplug_fails.saturating_add(other.unplug_fails),
            unplug_discard_fails: self
                .unplug_discard_fails
                .saturating_add(other.unplug_discard_fails),
            unplug_all_agg: self.unplug_all_agg.merged_with(other.unplug_all_agg),
            unplug_all_count: self.unplug_all_count.saturating_add(other.unplug_all_count),
            unplug_all_fails: self.unplug_all_fails.saturating_add(other.unplug_all_fails),
            state_agg: self.state_agg.merged_with(other.state_agg),
            state_count: self.state_count.saturating_add(other.state_count),
            state_fails: self.state_fails.saturating_add(other.state_fails),
            interrupt_fails: self.interrupt_fails.saturating_add(other.interrupt_fails),
            rollback_count: self.rollback_count.saturating_add(other.rollback_count),
            rollback_fails: self.rollback_fails.saturating_add(other.rollback_fails),
            owner_cleanup_count: self
                .owner_cleanup_count
                .saturating_add(other.owner_cleanup_count),
            owner_cleanup_fails: self
                .owner_cleanup_fails
                .saturating_add(other.owner_cleanup_fails),
            teardown_count: self.teardown_count.saturating_add(other.teardown_count),
            teardown_fails: self.teardown_fails.saturating_add(other.teardown_fails),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryHotplugMetricOperation {
    Plug,
    Unplug,
    UnplugAll,
    State,
}

/// Shared producer installed with one virtio-mem device before activation.
#[derive(Debug, Clone, Default)]
pub struct SharedMemoryHotplugDeviceMetrics {
    inner: Arc<Mutex<MemoryHotplugDeviceMetrics>>,
}

impl SharedMemoryHotplugDeviceMetrics {
    pub fn record_activation_failure(&self) {
        self.record(MemoryHotplugDeviceMetrics {
            activate_fails: 1,
            ..Default::default()
        });
    }

    pub fn record_queue_events(&self, count: usize) {
        let count = usize_to_u64_saturating(count);
        self.record(MemoryHotplugDeviceMetrics {
            queue_event_count: count,
            ..Default::default()
        });
    }

    pub fn record_queue_event_failure(&self) {
        self.record(MemoryHotplugDeviceMetrics {
            queue_event_fails: 1,
            ..Default::default()
        });
    }

    pub fn record_operation(
        &self,
        operation: MemoryHotplugMetricOperation,
        succeeded: bool,
        committed_bytes: u64,
        latency_us: u64,
    ) {
        let mut observation = MemoryHotplugDeviceMetrics::default();
        let latency = MemoryHotplugLatencyMetrics::from_sample(latency_us);
        match operation {
            MemoryHotplugMetricOperation::Plug => {
                observation.plug_agg = latency;
                observation.plug_count = 1;
                if succeeded {
                    observation.plug_bytes = committed_bytes;
                } else {
                    observation.plug_fails = 1;
                }
            }
            MemoryHotplugMetricOperation::Unplug => {
                observation.unplug_agg = latency;
                observation.unplug_count = 1;
                if succeeded {
                    observation.unplug_bytes = committed_bytes;
                } else {
                    observation.unplug_fails = 1;
                }
            }
            MemoryHotplugMetricOperation::UnplugAll => {
                observation.unplug_all_agg = latency;
                observation.unplug_all_count = 1;
                if !succeeded {
                    observation.unplug_all_fails = 1;
                }
            }
            MemoryHotplugMetricOperation::State => {
                observation.state_agg = latency;
                observation.state_count = 1;
                if !succeeded {
                    observation.state_fails = 1;
                }
            }
        }
        self.record(observation);
    }

    pub fn record_unplug_discard_failures(&self, failures: u64) {
        self.record(MemoryHotplugDeviceMetrics {
            unplug_discard_fails: failures,
            ..Default::default()
        });
    }

    pub fn record_interrupt_failure(&self) {
        self.record(MemoryHotplugDeviceMetrics {
            interrupt_fails: 1,
            ..Default::default()
        });
    }

    pub fn record_rollbacks(&self, attempts: u64, failures: u64) {
        self.record(MemoryHotplugDeviceMetrics {
            rollback_count: attempts,
            rollback_fails: failures,
            ..Default::default()
        });
    }

    pub fn record_owner_cleanup(&self, attempts: u64, failures: u64) {
        self.record(MemoryHotplugDeviceMetrics {
            owner_cleanup_count: attempts,
            owner_cleanup_fails: failures,
            ..Default::default()
        });
    }

    pub fn record_teardown(&self, succeeded: bool) {
        self.record(MemoryHotplugDeviceMetrics {
            teardown_count: 1,
            teardown_fails: u64::from(!succeeded),
            ..Default::default()
        });
    }

    pub fn snapshot(&self) -> MemoryHotplugDeviceMetrics {
        *lock_memory_hotplug_device_metrics(&self.inner)
    }

    fn record(&self, observation: MemoryHotplugDeviceMetrics) {
        let mut metrics = lock_memory_hotplug_device_metrics(&self.inner);
        *metrics = metrics.merged_with(observation);
    }
}

fn lock_memory_hotplug_device_metrics(
    metrics: &Mutex<MemoryHotplugDeviceMetrics>,
) -> MutexGuard<'_, MemoryHotplugDeviceMetrics> {
    match metrics.lock() {
        Ok(metrics) => metrics,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn record_atomic_metric(metric: &AtomicU64, increment: u64) {
    record_atomic_metric_with_ordering(metric, increment, Ordering::Relaxed);
}

fn record_atomic_metric_seq_cst(metric: &AtomicU64, increment: u64) {
    let mut current = metric.load(Ordering::SeqCst);
    while current != u64::MAX {
        match metric.compare_exchange_weak(
            current,
            current.saturating_add(increment),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
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

fn record_network_latency(
    metrics: &NetworkLatencyAtomicMetrics,
    aggregate: VirtioNetworkLatencyAggregate,
) {
    if aggregate.samples() == 0 {
        return;
    }
    record_atomic_min_metric(&metrics.min_us, aggregate.min_us());
    record_atomic_max_metric(&metrics.max_us, aggregate.max_us());
    record_atomic_metric(&metrics.sum_us, aggregate.sum_us());
    record_atomic_metric_release(&metrics.samples, aggregate.samples());
}

fn snapshot_network_latency(
    metrics: &NetworkLatencyAtomicMetrics,
) -> VirtioNetworkLatencyAggregate {
    let samples = metrics.samples.load(Ordering::Acquire);
    if samples == 0 {
        VirtioNetworkLatencyAggregate::default()
    } else {
        VirtioNetworkLatencyAggregate::new(
            metrics.min_us.load(Ordering::Relaxed),
            metrics.max_us.load(Ordering::Relaxed),
            metrics.sum_us.load(Ordering::Relaxed),
            samples,
        )
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
    pmem_device_metrics: Option<PmemDeviceMetrics>,
    pmem_device_metrics_by_device: Option<PmemDeviceMetricsByDevice>,
    network_interface_metrics: Option<NetworkInterfaceMetrics>,
    network_interface_metrics_by_interface: Option<NetworkInterfaceMetricsByInterface>,
    mmds_metrics: Option<MmdsMetrics>,
    vsock_device_metrics: Option<VsockDeviceMetrics>,
    entropy_device_metrics: Option<EntropyDeviceMetrics>,
    rtc_device_metrics: Option<RtcDeviceMetrics>,
    balloon_device_metrics: Option<BalloonDeviceMetrics>,
    memory_hotplug_device_metrics: Option<MemoryHotplugDeviceMetrics>,
    boot_run_loop_status: Option<BootRunLoopMetricStatus>,
    start_time_us: Option<u64>,
    start_time_cpu_us: Option<u64>,
    parent_cpu_time_us: Option<u64>,
    serial_output_metrics: Option<SerialOutputMetrics>,
    signal_metrics: Option<SignalMetrics>,
}

impl MetricsDiagnostics {
    pub fn new() -> Self {
        Self {
            block_device_metrics: None,
            block_device_metrics_by_drive: None,
            pmem_device_metrics: None,
            pmem_device_metrics_by_device: None,
            network_interface_metrics: None,
            network_interface_metrics_by_interface: None,
            mmds_metrics: None,
            vsock_device_metrics: None,
            entropy_device_metrics: None,
            rtc_device_metrics: None,
            balloon_device_metrics: None,
            memory_hotplug_device_metrics: None,
            boot_run_loop_status: None,
            start_time_us: None,
            start_time_cpu_us: None,
            parent_cpu_time_us: None,
            serial_output_metrics: None,
            signal_metrics: None,
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

    pub fn with_pmem_device_metrics(mut self, pmem_device_metrics: PmemDeviceMetrics) -> Self {
        self.pmem_device_metrics = Some(pmem_device_metrics);
        self
    }

    pub fn with_pmem_device_metrics_by_device(
        mut self,
        pmem_device_metrics_by_device: PmemDeviceMetricsByDevice,
    ) -> Self {
        self.pmem_device_metrics_by_device = Some(pmem_device_metrics_by_device);
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

    pub fn with_mmds_metrics(mut self, mmds_metrics: MmdsMetrics) -> Self {
        self.mmds_metrics = Some(mmds_metrics);
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

    pub fn with_rtc_device_metrics(mut self, rtc_device_metrics: RtcDeviceMetrics) -> Self {
        self.rtc_device_metrics = Some(rtc_device_metrics);
        self
    }

    pub fn with_balloon_device_metrics(
        mut self,
        balloon_device_metrics: BalloonDeviceMetrics,
    ) -> Self {
        self.balloon_device_metrics = Some(balloon_device_metrics);
        self
    }

    pub fn with_memory_hotplug_device_metrics(
        mut self,
        memory_hotplug_device_metrics: MemoryHotplugDeviceMetrics,
    ) -> Self {
        self.memory_hotplug_device_metrics = Some(memory_hotplug_device_metrics);
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

    pub fn with_signal_metrics(mut self, signal_metrics: SignalMetrics) -> Self {
        self.signal_metrics = Some(signal_metrics);
        self
    }

    fn delta_since(&self, previous: &Self) -> Self {
        Self {
            block_device_metrics: self.block_device_metrics.map(|current| {
                current.delta_since(previous.block_device_metrics.unwrap_or_default())
            }),
            block_device_metrics_by_drive: self.block_device_metrics_by_drive.as_ref().map(
                |current| current.delta_since(previous.block_device_metrics_by_drive.as_ref()),
            ),
            pmem_device_metrics: self.pmem_device_metrics.map(|current| {
                current.delta_since(previous.pmem_device_metrics.unwrap_or_default())
            }),
            pmem_device_metrics_by_device: self.pmem_device_metrics_by_device.as_ref().map(
                |current| current.delta_since(previous.pmem_device_metrics_by_device.as_ref()),
            ),
            network_interface_metrics: self.network_interface_metrics.map(|current| {
                current.delta_since(previous.network_interface_metrics.unwrap_or_default())
            }),
            network_interface_metrics_by_interface: self
                .network_interface_metrics_by_interface
                .as_ref()
                .map(|current| {
                    current.delta_since(previous.network_interface_metrics_by_interface.as_ref())
                }),
            mmds_metrics: self
                .mmds_metrics
                .map(|current| current.delta_since(previous.mmds_metrics.unwrap_or_default())),
            vsock_device_metrics: self.vsock_device_metrics.map(|current| {
                current.delta_since(previous.vsock_device_metrics.unwrap_or_default())
            }),
            entropy_device_metrics: self.entropy_device_metrics.map(|current| {
                current.delta_since(previous.entropy_device_metrics.unwrap_or_default())
            }),
            rtc_device_metrics: self.rtc_device_metrics.map(|current| {
                current.delta_since(previous.rtc_device_metrics.unwrap_or_default())
            }),
            balloon_device_metrics: self.balloon_device_metrics.map(|current| {
                current.delta_since(previous.balloon_device_metrics.unwrap_or_default())
            }),
            memory_hotplug_device_metrics: self.memory_hotplug_device_metrics.map(|current| {
                current.delta_since(previous.memory_hotplug_device_metrics.unwrap_or_default())
            }),
            boot_run_loop_status: self.boot_run_loop_status,
            start_time_us: self.start_time_us,
            start_time_cpu_us: self.start_time_cpu_us,
            parent_cpu_time_us: self.parent_cpu_time_us,
            serial_output_metrics: self.serial_output_metrics.map(|current| {
                current.delta_since(previous.serial_output_metrics.unwrap_or_default())
            }),
            signal_metrics: self
                .signal_metrics
                .map(|current| current.delta_since(previous.signal_metrics.unwrap_or_default())),
        }
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
        if let Some(metrics) = other.pmem_device_metrics {
            self.pmem_device_metrics = Some(match self.pmem_device_metrics {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }
        if let Some(metrics) = other.pmem_device_metrics_by_device {
            self.pmem_device_metrics_by_device = Some(match self.pmem_device_metrics_by_device {
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
        if let Some(metrics) = other.mmds_metrics {
            self.mmds_metrics = Some(match self.mmds_metrics {
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
        if let Some(metrics) = other.rtc_device_metrics {
            self.rtc_device_metrics = Some(match self.rtc_device_metrics {
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
        if let Some(metrics) = other.memory_hotplug_device_metrics {
            self.memory_hotplug_device_metrics = Some(match self.memory_hotplug_device_metrics {
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
        if let Some(metrics) = other.signal_metrics {
            self.signal_metrics = Some(match self.signal_metrics {
                Some(existing) => existing.merged_with(metrics),
                None => metrics,
            });
        }

        self
    }

    pub fn block_device_metrics(&self) -> Option<BlockDeviceMetrics> {
        self.block_device_metrics
    }

    pub fn block_device_metrics_by_drive(&self) -> Option<&BlockDeviceMetricsByDrive> {
        self.block_device_metrics_by_drive.as_ref()
    }

    pub fn pmem_device_metrics(&self) -> Option<PmemDeviceMetrics> {
        self.pmem_device_metrics
    }

    pub fn pmem_device_metrics_by_device(&self) -> Option<&PmemDeviceMetricsByDevice> {
        self.pmem_device_metrics_by_device.as_ref()
    }

    pub fn network_interface_metrics(&self) -> Option<NetworkInterfaceMetrics> {
        self.network_interface_metrics
    }

    pub fn network_interface_metrics_by_interface(
        &self,
    ) -> Option<&NetworkInterfaceMetricsByInterface> {
        self.network_interface_metrics_by_interface.as_ref()
    }

    pub fn mmds_metrics(&self) -> Option<MmdsMetrics> {
        self.mmds_metrics
    }

    pub fn vsock_device_metrics(&self) -> Option<VsockDeviceMetrics> {
        self.vsock_device_metrics
    }

    pub fn entropy_device_metrics(&self) -> Option<EntropyDeviceMetrics> {
        self.entropy_device_metrics
    }

    pub fn rtc_device_metrics(&self) -> Option<RtcDeviceMetrics> {
        self.rtc_device_metrics
    }

    pub fn balloon_device_metrics(&self) -> Option<BalloonDeviceMetrics> {
        self.balloon_device_metrics
    }

    pub fn memory_hotplug_device_metrics(&self) -> Option<MemoryHotplugDeviceMetrics> {
        self.memory_hotplug_device_metrics
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

    pub fn signal_metrics(&self) -> Option<SignalMetrics> {
        self.signal_metrics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootRunLoopMetricStatus {
    Running,
    Paused,
    Exited,
    Failed,
}

struct MetricsSink {
    output: Box<dyn MetricsOutput>,
}

impl fmt::Debug for MetricsSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetricsSink")
            .finish_non_exhaustive()
    }
}

trait MetricsOutput: fmt::Debug + Send {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize>;

    fn flush(&mut self) -> io::Result<()>;
}

struct FileMetricsOutput {
    writer: LineWriter<File>,
}

impl fmt::Debug for FileMetricsOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileMetricsOutput")
            .finish_non_exhaustive()
    }
}

impl MetricsOutput for FileMetricsOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
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

    fn from_file(file: File) -> Result<Self, MetricsConfigError> {
        let file = crate::output_file::adopt_write_only_file(file)
            .map_err(MetricsConfigError::OpenFile)?;
        Ok(Self::new(Box::new(FileMetricsOutput {
            writer: LineWriter::new(file),
        })))
    }

    fn new(output: Box<dyn MetricsOutput>) -> Self {
        Self { output }
    }

    fn write_metrics_line(&mut self, line: &[u8]) -> Result<(), MetricsFlushError> {
        write_all_metrics(self.output.as_mut(), line, MetricsFlushError::Write)?;
        write_all_metrics(self.output.as_mut(), b"\n", MetricsFlushError::Newline)?;
        self.output
            .flush()
            .map_err(|error| MetricsFlushError::Flush(error.kind()))
    }
}

fn write_all_metrics(
    output: &mut dyn MetricsOutput,
    mut bytes: &[u8],
    error: fn(io::ErrorKind) -> MetricsFlushError,
) -> Result<(), MetricsFlushError> {
    while !bytes.is_empty() {
        match output.write(bytes) {
            Ok(0) => return Err(error(io::ErrorKind::WriteZero)),
            Ok(written) => {
                bytes = bytes
                    .get(written..)
                    .ok_or_else(|| error(io::ErrorKind::InvalidData))?;
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(error(source.kind())),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs::{self, OpenOptions};
    use std::io::{self, ErrorKind};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::firecracker::ConfiguredMetricsDevices;
    use super::{
        BalloonDeviceMetrics, BalloonDiscardMetrics, BalloonFreePageReportMetrics,
        BlockDeviceMetrics, BlockDeviceMetricsByDrive, BlockDeviceMetricsRegistryError,
        BootRunLoopMetricStatus, DeprecatedApiMetrics, EntropyDeviceMetrics, GetApiRequestMetrics,
        MemoryHotplugMetricOperation, MetricsConfigError, MetricsConfigInput, MetricsDiagnostics,
        MetricsFlushError, MetricsOutput, MetricsState, MmdsMetrics, NetworkInterfaceMetrics,
        NetworkInterfaceMetricsByInterface, NetworkInterfaceMetricsCaptureError,
        NetworkInterfaceMetricsRegistryError, PatchApiRequestMetrics, PmemDeviceMetrics,
        PmemDeviceMetricsByDevice, PmemDeviceMetricsRegistryError, ProcessLatencyBoundary,
        ProcessLatencyOperation, PutApiRequestMetrics, RtcDeviceMetrics,
        SharedBalloonDeviceMetrics, SharedBlockDeviceMetrics, SharedBlockDeviceMetricsRegistry,
        SharedEntropyDeviceMetrics, SharedMemoryHotplugDeviceMetrics, SharedMmdsMetrics,
        SharedNetworkInterfaceMetrics, SharedNetworkInterfaceMetricsRegistry,
        SharedPmemDeviceMetrics, SharedPmemDeviceMetricsRegistry, SharedProcessMetrics,
        SharedRtcDeviceMetrics, SharedSignalMetrics, SharedVsockDeviceMetrics, SignalMetrics,
        VirtioNetworkLatencyAggregate, VsockDeviceMetrics,
    };
    use crate::block::VirtioBlockLatencyAggregate;
    use crate::network::NetworkInterfaceConfigInput;
    use crate::network::VirtioNetworkBackendMetrics;
    use crate::serial::SerialOutputMetrics;

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn configured_metrics_devices(
        ordinary_block_ids: &[&str],
        vhost_user_block_ids: &[&str],
        network_ids: &[&str],
    ) -> ConfiguredMetricsDevices {
        ConfiguredMetricsDevices::from_test_ids(
            ordinary_block_ids,
            vhost_user_block_ids,
            network_ids,
        )
    }

    fn metrics_values(output: &TestMetricsOutput) -> Vec<serde_json::Value> {
        output
            .lines()
            .iter()
            .map(|line| serde_json::from_str(line).expect("metrics line should be valid JSON"))
            .collect()
    }

    fn only_metrics_value(output: &TestMetricsOutput) -> serde_json::Value {
        let values = metrics_values(output);
        assert_eq!(
            values.len(),
            1,
            "exactly one metrics line should be written"
        );
        values.into_iter().next().expect("one metrics value")
    }

    fn metrics_values_from_text(output: &str) -> Vec<serde_json::Value> {
        output
            .lines()
            .map(|line| serde_json::from_str(line).expect("metrics line should be valid JSON"))
            .collect()
    }

    fn only_metrics_value_from_file(path: &std::path::Path) -> serde_json::Value {
        let output = fs::read_to_string(path).expect("metrics output should be readable");
        let values = metrics_values_from_text(&output);
        assert_eq!(
            values.len(),
            1,
            "exactly one metrics line should be written"
        );
        values.into_iter().next().expect("one metrics value")
    }

    fn without_timestamp(mut value: serde_json::Value) -> serde_json::Value {
        value
            .as_object_mut()
            .expect("metrics line root should be an object")
            .remove("utc_timestamp_ms");
        value
    }

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

        fn accept_next_write_then_fail(&self) {
            self.state
                .lock()
                .expect("test metrics output lock should not be poisoned")
                .accept_next_write_then_fail = true;
        }

        fn fail_next_newline(&self) {
            self.state
                .lock()
                .expect("test metrics output lock should not be poisoned")
                .fail_next_newline = true;
        }

        fn fail_next_flush(&self) {
            self.state
                .lock()
                .expect("test metrics output lock should not be poisoned")
                .fail_next_flush = true;
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
        accept_next_write_then_fail: bool,
        fail_next_flush: bool,
        fail_next_newline: bool,
        fail_next_write: bool,
        lines: Vec<String>,
    }

    impl MetricsOutput for TestMetricsOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut state = self
                .state
                .lock()
                .expect("test metrics output lock should not be poisoned");
            if state.fail_next_write {
                state.fail_next_write = false;
                return Err(io::Error::from(ErrorKind::BrokenPipe));
            }
            if bytes == b"\n" && state.fail_next_newline {
                state.fail_next_newline = false;
                return Err(io::Error::from(ErrorKind::BrokenPipe));
            }

            if bytes != b"\n" {
                state.lines.push(
                    String::from_utf8(bytes.to_vec())
                        .expect("canonical metrics line should be UTF-8"),
                );
            }
            if state.accept_next_write_then_fail {
                state.accept_next_write_then_fail = false;
                return Err(io::Error::from(ErrorKind::BrokenPipe));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut state = self
                .state
                .lock()
                .expect("test metrics output lock should not be poisoned");
            if state.fail_next_flush {
                state.fail_next_flush = false;
                return Err(io::Error::from(ErrorKind::BrokenPipe));
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum ScriptedWrite {
        Accept(usize),
        Report(usize),
        Error(ErrorKind),
        Zero,
    }

    #[derive(Debug, Clone)]
    struct ScriptedMetricsOutput {
        state: Arc<Mutex<ScriptedMetricsOutputState>>,
    }

    #[derive(Debug)]
    struct ScriptedMetricsOutputState {
        actions: VecDeque<ScriptedWrite>,
        bytes: Vec<u8>,
        flush_error: Option<ErrorKind>,
        flush_count: usize,
    }

    impl ScriptedMetricsOutput {
        fn new(actions: impl IntoIterator<Item = ScriptedWrite>) -> Self {
            Self {
                state: Arc::new(Mutex::new(ScriptedMetricsOutputState {
                    actions: actions.into_iter().collect(),
                    bytes: Vec::new(),
                    flush_error: None,
                    flush_count: 0,
                })),
            }
        }

        fn with_flush_error(self, error: ErrorKind) -> Self {
            self.state
                .lock()
                .expect("scripted output lock should not be poisoned")
                .flush_error = Some(error);
            self
        }

        fn bytes(&self) -> Vec<u8> {
            self.state
                .lock()
                .expect("scripted output lock should not be poisoned")
                .bytes
                .clone()
        }

        fn flush_count(&self) -> usize {
            self.state
                .lock()
                .expect("scripted output lock should not be poisoned")
                .flush_count
        }
    }

    impl MetricsOutput for ScriptedMetricsOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut state = self
                .state
                .lock()
                .expect("scripted output lock should not be poisoned");
            match state.actions.pop_front() {
                Some(ScriptedWrite::Accept(limit)) => {
                    let accepted = limit.min(bytes.len());
                    state.bytes.extend_from_slice(&bytes[..accepted]);
                    Ok(accepted)
                }
                Some(ScriptedWrite::Report(progress)) => Ok(progress),
                Some(ScriptedWrite::Error(error)) => Err(io::Error::from(error)),
                Some(ScriptedWrite::Zero) => Ok(0),
                None => {
                    state.bytes.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut state = self
                .state
                .lock()
                .expect("scripted output lock should not be poisoned");
            state.flush_count += 1;
            match state.flush_error.take() {
                Some(error) => Err(io::Error::from(error)),
                None => Ok(()),
            }
        }
    }

    #[derive(Debug)]
    struct CountingFixedClock {
        now: SystemTime,
        calls: Arc<AtomicU64>,
    }

    impl super::firecracker::MetricsClock for CountingFixedClock {
        fn now(&self) -> SystemTime {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.now
        }
    }

    #[derive(Debug)]
    struct ProcessEventClock {
        now: SystemTime,
        fired: Arc<AtomicBool>,
        logger_metrics: crate::logger::SharedLoggerMetrics,
        signal_metrics: SharedSignalMetrics,
    }

    impl super::firecracker::MetricsClock for ProcessEventClock {
        fn now(&self) -> SystemTime {
            if !self.fired.swap(true, Ordering::SeqCst) {
                self.logger_metrics.record_missed_log();
                self.logger_metrics.record_rate_limited_log();
                self.signal_metrics.record_sigpipe();
            }
            self.now
        }
    }

    #[derive(Debug)]
    struct FailingMetricsLineSerializer(super::firecracker::MetricsLineBuildError);

    impl super::firecracker::MetricsLineSerializer for FailingMetricsLineSerializer {
        fn serialize(
            &self,
            _line: &super::firecracker::FirecrackerMetricsLine,
        ) -> Result<Vec<u8>, super::firecracker::MetricsLineBuildError> {
            Err(self.0)
        }
    }

    fn block_metrics_with_all_fields() -> BlockDeviceMetrics {
        BlockDeviceMetrics::default()
            .with_event_fails(1)
            .with_execute_fails(2)
            .with_invalid_reqs_count(3)
            .with_flush_count(4)
            .with_queue_event_count(5)
            .with_io_engine_throttled_events(14)
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

    fn pmem_metrics_with_all_fields() -> PmemDeviceMetrics {
        PmemDeviceMetrics::default()
            .with_activate_fails(1)
            .with_cfg_fails(2)
            .with_event_fails(3)
            .with_queue_event_count(4)
            .with_rate_limiter_event_count(5)
            .with_rate_limiter_throttled_events(6)
    }

    fn network_metrics_with_all_fields() -> NetworkInterfaceMetrics {
        NetworkInterfaceMetrics::default()
            .with_activate_fails(1)
            .with_cfg_fails(2)
            .with_event_fails(3)
            .with_no_rx_avail_buffer(4)
            .with_no_tx_avail_buffer(5)
            .with_rx_queue_event_count(6)
            .with_rx_bytes_count(7)
            .with_rx_packets_count(8)
            .with_rx_fails(9)
            .with_rx_count(10)
            .with_rx_event_rate_limiter_count(11)
            .with_rx_rate_limiter_throttled(12)
            .with_tx_bytes_count(13)
            .with_tx_malformed_frames(14)
            .with_tx_fails(15)
            .with_tx_count(16)
            .with_tx_packets_count(17)
            .with_tx_queue_event_count(18)
            .with_tx_rate_limiter_event_count(19)
            .with_tx_rate_limiter_throttled(20)
            .with_tx_remaining_reqs_count(21)
            .with_tx_spoofed_mac_count(22)
            .with_vmnet_read_count(23)
            .with_vmnet_read_fails(24)
            .with_vmnet_read_packets_count(25)
            .with_vmnet_read_partial_batches(26)
            .with_vmnet_write_count(27)
            .with_vmnet_write_fails(28)
            .with_vmnet_write_packets_count(29)
            .with_vmnet_write_partial_batches(30)
            .with_vmnet_read_latency(VirtioNetworkLatencyAggregate::new(31, 32, 63, 2))
            .with_vmnet_write_latency(VirtioNetworkLatencyAggregate::new(33, 34, 67, 2))
    }

    fn mmds_metrics_with_all_fields() -> MmdsMetrics {
        MmdsMetrics::default()
            .with_rx_accepted(1)
            .with_rx_accepted_err(2)
            .with_rx_accepted_unusual(3)
            .with_rx_bad_eth(4)
            .with_rx_invalid_token(5)
            .with_rx_no_token(6)
            .with_rx_count(7)
            .with_tx_bytes(8)
            .with_tx_count(9)
            .with_tx_errors(10)
            .with_tx_frames(11)
            .with_connections_created(12)
            .with_connections_destroyed(13)
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
            .with_source_provider_fails(8)
    }

    fn serial_metrics_with_scale(scale: u64) -> SerialOutputMetrics {
        SerialOutputMetrics::default()
            .with_error_count(scale)
            .with_flush_count(2 * scale)
            .with_input_count(3 * scale)
            .with_interrupt_count(4 * scale)
            .with_missed_read_count(5 * scale)
            .with_missed_write_count(6 * scale)
            .with_overrun_count(7 * scale)
            .with_read_count(8 * scale)
            .with_write_count(9 * scale)
            .with_rate_limiter_dropped_bytes(10 * scale)
    }

    const fn signal_metrics_with_stores(
        sigxfsz: u64,
        sigxcpu: u64,
        sigpipe: u64,
        sighup: u64,
    ) -> SignalMetrics {
        SignalMetrics {
            sigxfsz,
            sigxcpu,
            sigpipe,
            sighup,
        }
    }

    fn balloon_metrics_with_all_fields() -> BalloonDeviceMetrics {
        BalloonDeviceMetrics::new(1, 2, 3, 4, 5, 6)
            .with_discard_metrics(
                BalloonDiscardMetrics::new(7, 8, 9, 10),
                BalloonDiscardMetrics::new(11, 12, 13, 14),
            )
            .with_free_page_report_metrics(BalloonFreePageReportMetrics::new(15, 16, 17, 18, 19))
    }

    fn diagnostics_with_all_fields() -> MetricsDiagnostics {
        let block = block_metrics_with_all_fields();
        let pmem = pmem_metrics_with_all_fields();
        let network = network_metrics_with_all_fields();

        MetricsDiagnostics::new()
            .with_block_device_metrics(block)
            .with_block_device_metrics_by_drive(
                BlockDeviceMetricsByDrive::new().with_drive_metrics("rootfs", block),
            )
            .with_pmem_device_metrics(pmem)
            .with_pmem_device_metrics_by_device(
                PmemDeviceMetricsByDevice::new().with_device_metrics("pmem0", pmem),
            )
            .with_network_interface_metrics(network)
            .with_network_interface_metrics_by_interface(
                NetworkInterfaceMetricsByInterface::new().with_interface_metrics("eth0", network),
            )
            .with_mmds_metrics(mmds_metrics_with_all_fields())
            .with_vsock_device_metrics(vsock_metrics_with_all_fields())
            .with_entropy_device_metrics(entropy_metrics_with_all_fields())
            .with_rtc_device_metrics(RtcDeviceMetrics::new(1, 2, 3))
            .with_balloon_device_metrics(balloon_metrics_with_all_fields())
            .with_boot_run_loop_status(BootRunLoopMetricStatus::Running)
            .with_start_time_us(1_000)
            .with_start_time_cpu_us(2_000)
            .with_parent_cpu_time_us(3_000)
            .with_serial_output_metrics(serial_metrics_with_scale(1))
            .with_signal_metrics(SignalMetrics::new(8))
    }

    fn record_all_process_metrics(state: &mut MetricsState) {
        state.record_deprecated_api_call();
        for (operation, boundary, value) in [
            (
                ProcessLatencyOperation::PauseVm,
                ProcessLatencyBoundary::OuterApi,
                101,
            ),
            (
                ProcessLatencyOperation::ResumeVm,
                ProcessLatencyBoundary::OuterApi,
                102,
            ),
            (
                ProcessLatencyOperation::FullCreateSnapshot,
                ProcessLatencyBoundary::OuterApi,
                103,
            ),
            (
                ProcessLatencyOperation::DiffCreateSnapshot,
                ProcessLatencyBoundary::OuterApi,
                104,
            ),
            (
                ProcessLatencyOperation::LoadSnapshot,
                ProcessLatencyBoundary::OuterApi,
                105,
            ),
            (
                ProcessLatencyOperation::PauseVm,
                ProcessLatencyBoundary::InnerVmm,
                106,
            ),
            (
                ProcessLatencyOperation::ResumeVm,
                ProcessLatencyBoundary::InnerVmm,
                107,
            ),
            (
                ProcessLatencyOperation::FullCreateSnapshot,
                ProcessLatencyBoundary::InnerVmm,
                108,
            ),
            (
                ProcessLatencyOperation::DiffCreateSnapshot,
                ProcessLatencyBoundary::InnerVmm,
                109,
            ),
            (
                ProcessLatencyOperation::LoadSnapshot,
                ProcessLatencyBoundary::InnerVmm,
                110,
            ),
        ] {
            state.record_process_latency_us(operation, boundary, value);
        }
        state.record_put_actions_request();
        state.record_put_actions_failure();
        state.record_put_balloon_request();
        state.record_put_balloon_failure();
        state.record_put_boot_source_request();
        state.record_put_boot_source_failure();
        state.record_put_cpu_config_request();
        state.record_put_cpu_config_failure();
        state.record_put_drive_request();
        state.record_put_drive_failure();
        state.record_put_metrics_request();
        state.record_put_metrics_failure();
        state.record_put_logger_request();
        state.record_put_logger_failure();
        state.record_put_machine_config_request();
        state.record_put_machine_config_failure();
        state.record_put_mmds_request();
        state.record_put_mmds_failure();
        state.record_put_hotplug_memory_request();
        state.record_put_hotplug_memory_failure();
        state.record_put_pmem_request();
        state.record_put_pmem_failure();
        state.record_put_network_request();
        state.record_put_network_failure();
        state.record_put_serial_request();
        state.record_put_serial_failure();
        state.record_put_vsock_request();
        state.record_put_vsock_failure();
        state.record_patch_drive_request();
        state.record_patch_drive_failure();
        state.record_patch_balloon_request();
        state.record_patch_balloon_failure();
        state.record_patch_network_request();
        state.record_patch_network_failure();
        state.record_patch_machine_config_request();
        state.record_patch_machine_config_failure();
        state.record_patch_mmds_request();
        state.record_patch_mmds_failure();
        state.record_patch_hotplug_memory_request();
        state.record_patch_hotplug_memory_failure();
        state.record_patch_pmem_request();
        state.record_patch_pmem_failure();
        state.record_get_balloon_request();
        state.record_get_instance_info_request();
        state.record_get_vmm_version_request();
        state.record_get_machine_config_request();
        state.record_get_mmds_request();
        state.record_get_hotplug_memory_request();
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
        state.record_deprecated_api_call();

        assert_eq!(state.flush(), Ok(false));
        assert!(!state.is_configured());
        assert_eq!(state.process_generation, 0);

        let output = TestMetricsOutput::default();
        state.sink = Some(super::MetricsSink::new(Box::new(output.clone())));
        assert_eq!(state.flush(), Ok(true));
        let value = only_metrics_value(&output);
        assert_eq!(value["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(value["vmm"]["panic_count"], 0);
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
        let values = metrics_values_from_text(&output);
        assert_eq!(values.len(), 2);
        assert!(
            values
                .iter()
                .all(|value| value.as_object().unwrap().len() == 24)
        );
        assert!(values.iter().all(|value| value["vmm"]["panic_count"] == 0));

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn repeated_flushes_emit_all_increment_fields_and_preserve_stores() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let shared_logger_metrics = state.shared_process_metrics.logger_metrics();
        let first = diagnostics_with_all_fields();
        let next = first
            .clone()
            .merged_with(first.clone())
            .with_serial_output_metrics(serial_metrics_with_scale(2));

        record_all_process_metrics(&mut state);
        shared_logger_metrics.record_missed_log();
        shared_logger_metrics.record_rate_limited_log();
        let configured = configured_metrics_devices(&["rootfs"], &[], &["eth0"]);
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&first, &configured),
            Ok(true)
        );
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&first, &configured),
            Ok(true)
        );

        record_all_process_metrics(&mut state);
        shared_logger_metrics.record_missed_log();
        shared_logger_metrics.record_rate_limited_log();
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&next, &configured),
            Ok(true)
        );

        let lines = output.lines();
        assert_eq!(lines.len(), 3);
        let first_value: serde_json::Value =
            serde_json::from_str(&lines[0]).expect("metrics line should be valid JSON");
        let third_value: serde_json::Value =
            serde_json::from_str(&lines[2]).expect("metrics line should be valid JSON");
        assert_eq!(
            without_timestamp(first_value),
            without_timestamp(third_value)
        );

        let unchanged: serde_json::Value =
            serde_json::from_str(&lines[1]).expect("metrics line should be valid JSON");
        let root = unchanged
            .as_object()
            .expect("metrics line root should be an object");
        assert_eq!(root.len(), 26);
        assert!(root.contains_key("api_server"));
        assert!(root.contains_key("block"));
        assert!(root.contains_key("block_rootfs"));
        assert!(root.contains_key("latencies_us"));
        assert!(root.contains_key("vmm"));
        assert_eq!(unchanged["api_server"]["process_startup_time_us"], 1_000);
        assert_eq!(
            unchanged["api_server"]["process_startup_time_cpu_us"],
            5_000
        );
        assert_eq!(unchanged["block"]["read_agg"]["min_us"], 0);
        assert_eq!(unchanged["block"]["read_agg"]["max_us"], 0);
        assert_eq!(unchanged["block"]["read_agg"]["sum_us"], 0);
        assert_eq!(unchanged["block"]["write_agg"]["min_us"], 0);
        assert_eq!(unchanged["block"]["write_agg"]["max_us"], 0);
        assert_eq!(unchanged["block"]["write_agg"]["sum_us"], 0);
        assert_eq!(unchanged["block_rootfs"]["read_agg"]["min_us"], 12);
        assert_eq!(unchanged["block_rootfs"]["read_agg"]["max_us"], 30);
        assert_eq!(unchanged["latencies_us"]["pause_vm"], 101);
        assert_eq!(unchanged["latencies_us"]["resume_vm"], 102);
        assert!(unchanged["vmm"].get("boot_run_loop_status").is_none());
        assert_eq!(unchanged["vmm"]["panic_count"], 0);
    }

    #[test]
    fn incremental_counters_handle_saturation_and_lower_generations() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());

        state.deprecated_api.deprecated_http_api_calls = u64::MAX - 1;
        assert_eq!(state.flush(), Ok(true));
        state.deprecated_api.deprecated_http_api_calls = u64::MAX;
        assert_eq!(state.flush(), Ok(true));
        assert_eq!(state.flush(), Ok(true));
        state.deprecated_api.deprecated_http_api_calls = 2;
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        let counts: Vec<_> = values
            .iter()
            .map(|value| value["deprecated_api"]["deprecated_http_api_calls"].as_u64())
            .collect();
        assert_eq!(counts, [Some(u64::MAX - 1), Some(1), Some(0), Some(2)]);
    }

    #[test]
    fn keyed_metrics_track_new_disappeared_reappearing_and_lower_generations() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let first = MetricsDiagnostics::new().with_block_device_metrics_by_drive(
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics("data", BlockDeviceMetrics::default().with_update_count(5))
                .with_drive_metrics("gone", BlockDeviceMetrics::default().with_update_count(7)),
        );
        let second = MetricsDiagnostics::new().with_block_device_metrics_by_drive(
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics("data", BlockDeviceMetrics::default().with_update_count(8))
                .with_drive_metrics("new", BlockDeviceMetrics::default().with_update_count(2)),
        );
        let third = MetricsDiagnostics::new().with_block_device_metrics_by_drive(
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics("data", BlockDeviceMetrics::default().with_update_count(1))
                .with_drive_metrics("gone", BlockDeviceMetrics::default().with_update_count(4)),
        );

        let configured = configured_metrics_devices(&["data", "gone", "new"], &[], &[]);
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&first, &configured),
            Ok(true)
        );
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&second, &configured),
            Ok(true)
        );
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&third, &configured),
            Ok(true)
        );
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&third, &configured),
            Ok(true)
        );

        let lines = output.lines();
        let values: Vec<serde_json::Value> = lines
            .iter()
            .map(|line| serde_json::from_str(line).expect("metrics line should be valid JSON"))
            .collect();
        assert_eq!(values[0]["block_data"]["update_count"], 5);
        assert_eq!(values[0]["block_gone"]["update_count"], 7);
        let data_position = lines[0]
            .find("block_data")
            .expect("data key should be serialized");
        let gone_position = lines[0]
            .find("block_gone")
            .expect("gone key should be serialized");
        assert!(data_position < gone_position);
        assert_eq!(values[1]["block_data"]["update_count"], 3);
        assert_eq!(values[1]["block_new"]["update_count"], 2);
        assert_eq!(values[1]["block_gone"]["update_count"], 0);
        assert_eq!(values[2]["block_data"]["update_count"], 1);
        assert_eq!(values[2]["block_gone"]["update_count"], 4);
        assert_eq!(values[2]["block_new"]["update_count"], 0);
        assert_eq!(values[3]["block_data"]["update_count"], 0);
        assert_eq!(values[3]["block_gone"]["update_count"], 0);
        assert_eq!(values[3]["block_new"]["update_count"], 0);
    }

    #[test]
    fn independent_metrics_states_do_not_consume_each_others_deltas() {
        let first_output = TestMetricsOutput::default();
        let second_output = TestMetricsOutput::default();
        let shared_process_metrics = SharedProcessMetrics::default();
        let shared_logger_metrics = shared_process_metrics.logger_metrics();
        let mut first = MetricsState::with_test_output(first_output.clone());
        let mut second = MetricsState::with_test_output(second_output.clone());
        first.shared_process_metrics = shared_process_metrics.clone();
        second.shared_process_metrics = shared_process_metrics;

        shared_logger_metrics.record_missed_log();
        assert_eq!(first.flush(), Ok(true));
        assert_eq!(second.flush(), Ok(true));
        shared_logger_metrics.record_missed_log();
        assert_eq!(first.flush(), Ok(true));
        assert_eq!(second.flush(), Ok(true));

        for output in [&first_output, &second_output] {
            let values = metrics_values(output);
            assert_eq!(values.len(), 2);
            assert!(
                values
                    .iter()
                    .all(|value| value["logger"]["missed_log_count"] == 1)
            );
        }
    }

    #[test]
    fn failed_first_flush_replays_counters_and_records_missed_metrics() {
        let output = TestMetricsOutput::default();
        output.fail_next_write();
        let mut state = MetricsState::with_test_output(output.clone());
        state.record_deprecated_api_call();

        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(value["logger"]["missed_metrics_count"], 1);
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
            only_metrics_value(&output)["logger"]["missed_metrics_count"],
            2
        );
    }

    #[test]
    fn failed_middle_flush_retains_the_previous_successful_baseline() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());

        state.record_deprecated_api_call();
        assert_eq!(state.flush(), Ok(true));
        state.record_deprecated_api_call();
        output.fail_next_write();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        state.record_deprecated_api_call();
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values[0]["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(values[1]["deprecated_api"]["deprecated_http_api_calls"], 2);
        assert_eq!(values[1]["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn ambiguous_accepted_failure_replays_at_least_once() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());

        state.record_deprecated_api_call();
        output.accept_next_write_then_fail();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        state.record_deprecated_api_call();
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(values[1]["deprecated_api"]["deprecated_http_api_calls"], 2);
        assert_eq!(values[1]["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn captures_the_clock_once_for_both_serialization_passes() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let calls = Arc::new(AtomicU64::new(0));
        state.clock = Box::new(CountingFixedClock {
            now: UNIX_EPOCH + Duration::from_millis(1_234),
            calls: Arc::clone(&calls),
        });

        assert_eq!(state.flush(), Ok(true));

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(only_metrics_value(&output)["utc_timestamp_ms"], 1_234);
    }

    #[test]
    fn clock_failure_retains_the_baseline_and_records_missed_metrics() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        state.record_deprecated_api_call();
        state.clock = Box::new(CountingFixedClock {
            now: UNIX_EPOCH - Duration::from_millis(1),
            calls: Arc::new(AtomicU64::new(0)),
        });

        assert_eq!(state.flush(), Err(MetricsFlushError::Clock));
        assert!(output.lines().is_empty());

        state.clock = Box::new(CountingFixedClock {
            now: UNIX_EPOCH + Duration::from_millis(1),
            calls: Arc::new(AtomicU64::new(0)),
        });
        assert_eq!(state.flush(), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(value["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn construction_failures_do_not_touch_the_sink_or_advance_the_baseline() {
        for (source, expected) in [
            (
                super::firecracker::MetricsLineBuildError::Allocation,
                MetricsFlushError::Allocation,
            ),
            (
                super::firecracker::MetricsLineBuildError::Serialization,
                MetricsFlushError::Serialization,
            ),
            (
                super::firecracker::MetricsLineBuildError::LineTooLong,
                MetricsFlushError::LineTooLong,
            ),
        ] {
            let output = TestMetricsOutput::default();
            let mut state = MetricsState::with_test_output(output.clone());
            state.record_deprecated_api_call();
            state.serializer = Box::new(FailingMetricsLineSerializer(source));

            assert_eq!(state.flush(), Err(expected));
            assert!(output.lines().is_empty());

            state.serializer = Box::<super::firecracker::SystemMetricsLineSerializer>::default();
            assert_eq!(state.flush(), Ok(true));
            let value = only_metrics_value(&output);
            assert_eq!(value["deprecated_api"]["deprecated_http_api_calls"], 1);
            assert_eq!(value["logger"]["missed_metrics_count"], 1);
        }
    }

    #[test]
    fn metrics_sink_retries_partial_and_interrupted_writes() {
        let output = ScriptedMetricsOutput::new([
            ScriptedWrite::Accept(1),
            ScriptedWrite::Error(ErrorKind::Interrupted),
        ]);
        let mut sink = super::MetricsSink::new(Box::new(output.clone()));

        assert_eq!(sink.write_metrics_line(b"{}"), Ok(()));
        assert_eq!(output.bytes(), b"{}\n");
        assert_eq!(output.flush_count(), 1);
    }

    #[test]
    fn metrics_sink_reports_zero_and_invalid_write_progress() {
        let zero = ScriptedMetricsOutput::new([ScriptedWrite::Zero]);
        let mut zero_sink = super::MetricsSink::new(Box::new(zero));
        assert_eq!(
            zero_sink.write_metrics_line(b"{}"),
            Err(MetricsFlushError::Write(ErrorKind::WriteZero))
        );

        let overreported = ScriptedMetricsOutput::new([ScriptedWrite::Report(3)]);
        let mut overreported_sink = super::MetricsSink::new(Box::new(overreported));
        assert_eq!(
            overreported_sink.write_metrics_line(b"{}"),
            Err(MetricsFlushError::Write(ErrorKind::InvalidData))
        );
    }

    #[test]
    fn metrics_sink_reports_blocked_write_newline_and_flush_stages() {
        let blocked = ScriptedMetricsOutput::new([ScriptedWrite::Error(ErrorKind::WouldBlock)]);
        let mut blocked_sink = super::MetricsSink::new(Box::new(blocked));
        assert_eq!(
            blocked_sink.write_metrics_line(b"{}"),
            Err(MetricsFlushError::Write(ErrorKind::WouldBlock))
        );

        let newline = ScriptedMetricsOutput::new([
            ScriptedWrite::Accept(usize::MAX),
            ScriptedWrite::Error(ErrorKind::BrokenPipe),
        ]);
        let mut newline_sink = super::MetricsSink::new(Box::new(newline));
        assert_eq!(
            newline_sink.write_metrics_line(b"{}"),
            Err(MetricsFlushError::Newline(ErrorKind::BrokenPipe))
        );

        let flush = ScriptedMetricsOutput::new([]).with_flush_error(ErrorKind::BrokenPipe);
        let mut flush_sink = super::MetricsSink::new(Box::new(flush));
        assert_eq!(
            flush_sink.write_metrics_line(b"{}"),
            Err(MetricsFlushError::Flush(ErrorKind::BrokenPipe))
        );
    }

    #[test]
    fn newline_failure_retains_the_baseline_for_retry() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        state.record_deprecated_api_call();
        output.fail_next_newline();

        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Newline(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(values[1]["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(values[1]["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn flush_failure_retains_the_baseline_for_retry() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        state.record_deprecated_api_call();
        output.fail_next_flush();

        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Flush(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(values[1]["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(values[1]["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn invalid_configured_inventory_retains_the_baseline_and_redacts_ids() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        state.record_deprecated_api_call();
        let networks = (0..17)
            .map(|index| {
                let id = format!("secret_network_{index}");
                NetworkInterfaceConfigInput::new(&id, &id, format!("tap{index}"))
                    .validate()
                    .expect("individual network configuration should validate")
            })
            .collect::<Vec<_>>();

        let error = state
            .flush_with_diagnostics_and_configs(&MetricsDiagnostics::default(), &[], &networks)
            .expect_err("the metrics inventory should reject more than sixteen networks");
        assert_eq!(error, MetricsFlushError::ConfiguredDevices);
        assert!(!error.to_string().contains("secret_network"));
        assert!(!format!("{error:?}").contains("secret_network"));

        assert_eq!(
            state.flush_with_diagnostics_and_configs(&MetricsDiagnostics::default(), &[], &[],),
            Ok(true)
        );
        let value = only_metrics_value(&output);
        assert_eq!(value["deprecated_api"]["deprecated_http_api_calls"], 1);
        assert_eq!(value["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn stable_process_snapshot_retries_a_racing_event_into_one_generation() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let logger_metrics = state.shared_process_metrics.logger_metrics();
        let signal_metrics = state.shared_process_metrics.signal_metrics();
        let hook_calls = Arc::new(AtomicUsize::new(0));
        state.shared_process_metrics.set_scan_hook({
            let process_metrics = state.shared_process_metrics.clone();
            let logger_metrics = logger_metrics.clone();
            let signal_metrics = signal_metrics.clone();
            let hook_calls = Arc::clone(&hook_calls);
            move |attempt| {
                hook_calls.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    logger_metrics.record_missed_log();
                    logger_metrics.record_rate_limited_log();
                    signal_metrics.record_sigxfsz();
                    signal_metrics.record_sigxcpu();
                    signal_metrics.record_sigpipe();
                    signal_metrics.record_sighup();
                    process_metrics.record_process_panic();
                }
            }
        });

        assert_eq!(state.flush(), Ok(true));

        assert_eq!(hook_calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.process_generation, 1);
        assert_eq!(state.previous_successful.generation, 1);
        let value = only_metrics_value(&output);
        assert_eq!(value["logger"]["missed_log_count"], 1);
        assert_eq!(value["logger"]["rate_limited_log_count"], 1);
        assert_eq!(value["logger"]["metrics_fails"], 0);
        assert_eq!(value["signals"]["sigxfsz"], 1);
        assert_eq!(value["signals"]["sigxcpu"], 1);
        assert_eq!(value["signals"]["sigpipe"], 1);
        assert_eq!(value["signals"]["sighup"], 1);
        assert_eq!(value["vmm"]["panic_count"], 1);
    }

    #[test]
    fn process_snapshot_preserves_exact_totals_under_producer_contention() {
        const PRODUCERS: usize = 4;
        const EVENTS_PER_PRODUCER: usize = 5_000;

        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let logger_metrics = state.shared_process_metrics.logger_metrics();
        let signal_metrics = state.shared_process_metrics.signal_metrics();
        let release = Arc::new(Barrier::new(PRODUCERS + 1));
        let finished = Arc::new(AtomicUsize::new(0));
        let handles = (0..PRODUCERS)
            .map(|_| {
                let logger_metrics = logger_metrics.clone();
                let signal_metrics = signal_metrics.clone();
                let release = Arc::clone(&release);
                let finished = Arc::clone(&finished);
                thread::spawn(move || {
                    release.wait();
                    for _ in 0..EVENTS_PER_PRODUCER {
                        logger_metrics.record_missed_log();
                        logger_metrics.record_rate_limited_log();
                        signal_metrics.record_sigpipe();
                    }
                    finished.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect::<Vec<_>>();

        let hook_calls = Arc::new(AtomicUsize::new(0));
        state.shared_process_metrics.set_scan_hook({
            let release = Arc::clone(&release);
            let finished = Arc::clone(&finished);
            let hook_calls = Arc::clone(&hook_calls);
            move |attempt| {
                hook_calls.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    release.wait();
                    while finished.load(Ordering::SeqCst) != PRODUCERS {
                        std::hint::spin_loop();
                    }
                }
            }
        });

        assert_eq!(state.flush(), Ok(true));
        for handle in handles {
            handle.join().expect("metrics producer should finish");
        }

        assert_eq!(hook_calls.load(Ordering::SeqCst), 2);
        let expected = u64::try_from(PRODUCERS * EVENTS_PER_PRODUCER)
            .expect("test event count should fit u64");
        let value = only_metrics_value(&output);
        assert_eq!(value["logger"]["missed_log_count"], expected);
        assert_eq!(value["logger"]["rate_limited_log_count"], expected);
        assert_eq!(value["signals"]["sigpipe"], expected);
    }

    #[test]
    fn saturated_process_events_are_already_represented_by_a_stable_cut() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let logger_metrics = state.shared_process_metrics.logger_metrics();
        let signal_metrics = state.shared_process_metrics.signal_metrics();
        logger_metrics.set_counts_for_test(u64::MAX, u64::MAX);
        signal_metrics.set_sigpipe_for_test(u64::MAX);
        state.shared_process_metrics.set_scan_hook({
            let logger_metrics = logger_metrics.clone();
            let signal_metrics = signal_metrics.clone();
            move |_| {
                logger_metrics.record_missed_log();
                logger_metrics.record_rate_limited_log();
                signal_metrics.record_sigpipe();
            }
        });

        assert_eq!(state.flush(), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["logger"]["missed_log_count"], u64::MAX);
        assert_eq!(value["logger"]["rate_limited_log_count"], u64::MAX);
        assert_eq!(value["signals"]["sigpipe"], u64::MAX);
        assert_eq!(state.process_generation, 1);
    }

    #[test]
    fn busy_process_snapshot_retains_events_and_successful_baseline_for_retry() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let logger_metrics = state.shared_process_metrics.logger_metrics();
        let signal_metrics = state.shared_process_metrics.signal_metrics();
        let hook_calls = Arc::new(AtomicUsize::new(0));
        state.shared_process_metrics.set_scan_hook({
            let logger_metrics = logger_metrics.clone();
            let signal_metrics = signal_metrics.clone();
            let hook_calls = Arc::clone(&hook_calls);
            move |_| {
                hook_calls.fetch_add(1, Ordering::SeqCst);
                logger_metrics.record_missed_log();
                logger_metrics.record_rate_limited_log();
                signal_metrics.record_sigpipe();
            }
        });

        let error = state
            .flush()
            .expect_err("continuous process changes must exhaust the bounded scan");
        assert_eq!(error, MetricsFlushError::ProcessSnapshotBusy);
        assert_eq!(
            error.to_string(),
            "failed to flush metrics: process snapshot remained busy"
        );
        assert_eq!(hook_calls.load(Ordering::SeqCst), 64);
        assert!(output.lines().is_empty());
        assert_eq!(state.process_generation, 0);
        assert_eq!(state.previous_successful.generation, 0);
        assert_eq!(state.logger_metrics.missed_metrics_count, 1);

        state.shared_process_metrics.clear_scan_hook();
        assert_eq!(state.flush(), Ok(true));

        assert_eq!(state.process_generation, 1);
        assert_eq!(state.previous_successful.generation, 1);
        let value = only_metrics_value(&output);
        assert_eq!(value["logger"]["missed_log_count"], 64);
        assert_eq!(value["logger"]["missed_metrics_count"], 1);
        assert_eq!(value["logger"]["rate_limited_log_count"], 64);
        assert_eq!(value["logger"]["metrics_fails"], 0);
        assert_eq!(value["signals"]["sigpipe"], 64);
    }

    #[test]
    fn exhausted_process_generation_fails_closed_without_aliasing_a_cut() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        state.process_generation = u64::MAX;

        let error = state
            .flush()
            .expect_err("an exhausted generation must fail closed");

        assert_eq!(error, MetricsFlushError::ProcessGenerationExhausted);
        assert_eq!(
            error.to_string(),
            "failed to flush metrics: process generation exhausted"
        );
        assert!(output.lines().is_empty());
        assert_eq!(state.process_generation, u64::MAX);
        assert_eq!(state.previous_successful.generation, 0);
        assert_eq!(state.logger_metrics.missed_metrics_count, 1);
    }

    #[test]
    fn events_after_a_frozen_process_cut_enter_the_next_generation() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let logger_metrics = state.shared_process_metrics.logger_metrics();
        let signal_metrics = state.shared_process_metrics.signal_metrics();
        state.clock = Box::new(ProcessEventClock {
            now: UNIX_EPOCH + Duration::from_millis(1),
            fired: Arc::new(AtomicBool::new(false)),
            logger_metrics,
            signal_metrics,
        });

        assert_eq!(state.flush(), Ok(true));
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["logger"]["missed_log_count"], 0);
        assert_eq!(values[0]["logger"]["rate_limited_log_count"], 0);
        assert_eq!(values[0]["signals"]["sigpipe"], 0);
        assert_eq!(values[1]["logger"]["missed_log_count"], 1);
        assert_eq!(values[1]["logger"]["rate_limited_log_count"], 1);
        assert_eq!(values[1]["signals"]["sigpipe"], 1);
        assert_eq!(state.process_generation, 2);
        assert_eq!(state.previous_successful.generation, 2);
    }

    #[test]
    fn failed_output_retries_post_cut_events_from_the_prior_successful_generation() {
        let output = TestMetricsOutput::default();
        output.fail_next_write();
        let mut state = MetricsState::with_test_output(output.clone());
        let logger_metrics = state.shared_process_metrics.logger_metrics();
        let signal_metrics = state.shared_process_metrics.signal_metrics();
        state.clock = Box::new(ProcessEventClock {
            now: UNIX_EPOCH + Duration::from_millis(1),
            fired: Arc::new(AtomicBool::new(false)),
            logger_metrics,
            signal_metrics,
        });

        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.process_generation, 1);
        assert_eq!(state.previous_successful.generation, 0);
        assert_eq!(state.flush(), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["logger"]["missed_log_count"], 1);
        assert_eq!(value["logger"]["missed_metrics_count"], 1);
        assert_eq!(value["logger"]["rate_limited_log_count"], 1);
        assert_eq!(value["logger"]["metrics_fails"], 0);
        assert_eq!(value["signals"]["sigpipe"], 1);
        assert_eq!(state.process_generation, 2);
        assert_eq!(state.previous_successful.generation, 2);
    }

    #[test]
    fn every_metrics_failure_stage_keeps_logger_metrics_fails_source_neutral() {
        enum FailureStage {
            Serialization,
            Write,
            AcceptedWrite,
            Newline,
            Flush,
        }

        for stage in [
            FailureStage::Serialization,
            FailureStage::Write,
            FailureStage::AcceptedWrite,
            FailureStage::Newline,
            FailureStage::Flush,
        ] {
            let output = TestMetricsOutput::default();
            let mut state = MetricsState::with_test_output(output.clone());
            match stage {
                FailureStage::Serialization => {
                    state.serializer = Box::new(FailingMetricsLineSerializer(
                        super::firecracker::MetricsLineBuildError::Serialization,
                    ));
                }
                FailureStage::Write => output.fail_next_write(),
                FailureStage::AcceptedWrite => output.accept_next_write_then_fail(),
                FailureStage::Newline => output.fail_next_newline(),
                FailureStage::Flush => output.fail_next_flush(),
            }

            assert!(state.flush().is_err());
            state.serializer = Box::<super::firecracker::SystemMetricsLineSerializer>::default();
            assert_eq!(state.flush(), Ok(true));

            let values = metrics_values(&output);
            let value = values
                .last()
                .expect("successful retry should publish a metrics line");
            assert_eq!(value["logger"]["missed_metrics_count"], 1);
            assert_eq!(value["logger"]["metrics_fails"], 0);
        }
    }

    #[test]
    fn logger_metrics_include_delivery_and_rate_limit_counts() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let shared_logger_metrics = state.shared_process_metrics.logger_metrics();

        shared_logger_metrics.record_missed_log();
        shared_logger_metrics.record_rate_limited_log();
        output.fail_next_write();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(state.flush(), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["logger"]["missed_log_count"], 1);
        assert_eq!(value["logger"]["missed_metrics_count"], 1);
        assert_eq!(value["logger"]["rate_limited_log_count"], 1);
    }

    #[test]
    fn writes_signal_metrics_when_recorded() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_signal_metrics(signal_metrics_with_stores(1, 1, 2, 1));

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["signals"]["sigxfsz"], 1);
        assert_eq!(value["signals"]["sigxcpu"], 1);
        assert_eq!(value["signals"]["sigpipe"], 2);
        assert_eq!(value["signals"]["sighup"], 1);
        assert_eq!(value["signals"]["sigbus"], 0);
        assert_eq!(value["signals"]["sigsegv"], 0);
        assert_eq!(value["signals"]["sigill"], 0);
        assert_eq!(value["seccomp"]["num_faults"], 0);
    }

    #[test]
    fn writes_zero_signal_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new().with_signal_metrics(SignalMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["signals"]["sigpipe"], 0);
    }

    #[test]
    fn shared_signal_metrics_snapshot_fixed_stores_and_incremental_sigpipe() {
        let metrics = SharedSignalMetrics::default();

        metrics.record_sigxfsz();
        metrics.record_sigxfsz();
        metrics.record_sigxcpu();
        metrics.record_sigxcpu();
        metrics.record_sigpipe();
        metrics.record_sigpipe();
        metrics.record_sighup();
        metrics.record_sighup();

        assert_eq!(metrics.snapshot(), signal_metrics_with_stores(1, 1, 2, 1));
    }

    #[test]
    fn process_store_metrics_persist_while_sigpipe_uses_successful_intervals() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let signal_metrics = state.signal_metrics();

        signal_metrics.record_sigxfsz();
        signal_metrics.record_sigxcpu();
        signal_metrics.record_sigpipe();
        signal_metrics.record_sigpipe();
        signal_metrics.record_sighup();
        state.record_process_panic();
        assert_eq!(state.flush(), Ok(true));

        signal_metrics.record_sigpipe();
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values.len(), 2);
        for value in &values {
            assert_eq!(value["signals"]["sigxfsz"], 1);
            assert_eq!(value["signals"]["sigxcpu"], 1);
            assert_eq!(value["signals"]["sighup"], 1);
            assert_eq!(value["vmm"]["panic_count"], 1);
            assert_eq!(value["signals"]["sigbus"], 0);
            assert_eq!(value["signals"]["sigsegv"], 0);
            assert_eq!(value["signals"]["sigill"], 0);
            assert_eq!(value["seccomp"]["num_faults"], 0);
        }
        assert_eq!(values[0]["signals"]["sigpipe"], 2);
        assert_eq!(values[1]["signals"]["sigpipe"], 1);
    }

    #[test]
    fn failed_output_retries_fixed_stores_and_sigpipe_from_last_success() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        assert_eq!(state.flush(), Ok(true));
        let signal_metrics = state.signal_metrics();

        signal_metrics.record_sigxfsz();
        signal_metrics.record_sigxcpu();
        signal_metrics.record_sigpipe();
        signal_metrics.record_sighup();
        state.record_process_panic();
        output.fail_next_write();
        assert_eq!(
            state.flush(),
            Err(MetricsFlushError::Write(ErrorKind::BrokenPipe))
        );

        signal_metrics.record_sigpipe();
        assert_eq!(state.flush(), Ok(true));

        let values = metrics_values(&output);
        assert_eq!(values.len(), 2);
        let retry = &values[1];
        assert_eq!(retry["signals"]["sigxfsz"], 1);
        assert_eq!(retry["signals"]["sigxcpu"], 1);
        assert_eq!(retry["signals"]["sigpipe"], 2);
        assert_eq!(retry["signals"]["sighup"], 1);
        assert_eq!(retry["vmm"]["panic_count"], 1);
        assert_eq!(retry["logger"]["missed_metrics_count"], 1);
    }

    #[test]
    fn keeps_failed_boot_run_loop_diagnostics_internal() {
        let path = unique_metrics_path("diagnostics");
        let mut state = MetricsState::default();
        let diagnostics =
            MetricsDiagnostics::new().with_boot_run_loop_status(BootRunLoopMetricStatus::Failed);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["vmm"]["panic_count"], 0);
        assert!(value["vmm"].get("boot_run_loop_status").is_none());

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn keeps_paused_boot_run_loop_diagnostics_internal() {
        let path = unique_metrics_path("paused-diagnostics");
        let mut state = MetricsState::default();
        let diagnostics =
            MetricsDiagnostics::new().with_boot_run_loop_status(BootRunLoopMetricStatus::Paused);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["vmm"]["panic_count"], 0);
        assert!(value["vmm"].get("boot_run_loop_status").is_none());

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_serial_output_diagnostics_when_uart_metrics_are_nonzero() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new().with_serial_output_metrics(
            SerialOutputMetrics::default()
                .with_error_count(1)
                .with_flush_count(99)
                .with_host_input_fails(5)
                .with_missed_write_count(2)
                .with_receive_fifo_flush_count(6)
                .with_state_fails(7)
                .with_write_count(3)
                .with_rate_limiter_dropped_bytes(4),
        );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["uart"]["error_count"], 1);
        assert_eq!(value["uart"]["flush_count"], 0);
        assert_eq!(value["uart"]["missed_write_count"], 2);
        assert_eq!(value["uart"]["rate_limiter_dropped_bytes"], 4);
        assert_eq!(value["uart"]["write_count"], 3);
        assert!(value["uart"].get("input_count").is_none());
        assert!(value["uart"].get("host_input_fails").is_none());
        assert!(value["uart"].get("receive_fifo_flush_count").is_none());
        assert!(value["uart"].get("state_fails").is_none());
    }

    #[test]
    fn writes_zero_serial_output_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_serial_output_metrics(SerialOutputMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["uart"]["write_count"], 0);
    }

    #[test]
    fn writes_block_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let metrics = block_metrics_with_all_fields();
        let diagnostics = MetricsDiagnostics::new()
            .with_block_device_metrics(metrics)
            .with_block_device_metrics_by_drive(
                BlockDeviceMetricsByDrive::new().with_drive_metrics("rootfs", metrics),
            );
        let configured = configured_metrics_devices(&["rootfs"], &[], &[]);

        assert_eq!(
            state.flush_with_diagnostics_and_devices(&diagnostics, &configured),
            Ok(true)
        );

        let value = only_metrics_value(&output);
        assert_eq!(value["block"]["event_fails"], 1);
        assert_eq!(value["block"]["read_agg"]["min_us"], 0);
        assert_eq!(value["block"]["read_agg"]["max_us"], 0);
        assert_eq!(value["block"]["read_agg"]["sum_us"], 42);
        assert_eq!(value["block_rootfs"]["read_agg"]["min_us"], 12);
        assert_eq!(value["block_rootfs"]["read_agg"]["max_us"], 30);
    }

    #[test]
    fn writes_zero_block_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_block_device_metrics(BlockDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["block"]["event_fails"], 0);
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

        let configured = configured_metrics_devices(&["data", "rootfs"], &[], &[]);
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&diagnostics, &configured),
            Ok(true)
        );

        let value = only_metrics_value(&output);
        assert_eq!(value["block"]["queue_event_count"], 2);
        assert_eq!(
            value["block"]["read_agg"],
            serde_json::json!({
                "min_us": 0, "max_us": 0, "sum_us": 6
            })
        );
        assert_eq!(
            value["block"]["write_agg"],
            serde_json::json!({
                "min_us": 0, "max_us": 0, "sum_us": 8
            })
        );
        assert_eq!(value["block_data"]["write_bytes"], 256);
        assert_eq!(value["block_rootfs"]["read_bytes"], 512);
        assert!(value.get("block_noop").is_none());
    }

    #[test]
    fn vhost_config_change_time_is_optional_and_scoped_to_updated_drive() {
        let registry = SharedBlockDeviceMetricsRegistry::from_drive_ids(["file", "vhost"]);
        registry.record_config_change_time_for_drive("vhost", 37);

        assert_eq!(
            registry.aggregate_snapshot().config_change_time_us(),
            Some(37)
        );
        let by_drive = registry.per_drive_snapshot();
        assert_eq!(
            by_drive
                .iter()
                .find_map(|(drive_id, metrics)| (drive_id == "vhost")
                    .then_some(metrics.config_change_time_us())),
            Some(Some(37))
        );
        assert!(by_drive.iter().all(|(drive_id, metrics)| {
            drive_id != "file" || metrics.config_change_time_us().is_none()
        }));

        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new()
            .with_block_device_metrics(registry.aggregate_snapshot())
            .with_block_device_metrics_by_drive(by_drive);
        let configured = configured_metrics_devices(&["file"], &["vhost"], &[]);
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&diagnostics, &configured),
            Ok(true)
        );
        let value = only_metrics_value(&output);
        assert_eq!(value["vhost_user_block_vhost"]["config_change_time_us"], 37);
        assert!(value.get("block_file").is_some());
        assert!(value.get("block_vhost").is_none());
        assert!(value["block"].get("config_change_time_us").is_none());

        let old_generation = registry
            .claim_drive_lease("vhost")
            .expect("existing vhost metrics generation should be claimable");
        drop(old_generation);
        assert!(registry.per_drive("vhost").is_none());
        let replacement_generation = registry
            .prepare_drive("vhost")
            .expect("released vhost metrics identity should prepare")
            .publish();
        assert_eq!(
            registry
                .per_drive("vhost")
                .expect("replacement generation should publish")
                .snapshot()
                .config_change_time_us(),
            None,
            "same-ID reinsertion must not inherit the removed generation's store"
        );
        assert_eq!(
            registry.aggregate_snapshot().config_change_time_us(),
            Some(37),
            "aggregate latest-value store should retain the VM-wide last success"
        );
        drop(replacement_generation);
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
    fn block_metrics_preparation_is_invisible_and_published_lease_removes_exact_entry() {
        let registry =
            SharedBlockDeviceMetricsRegistry::from_drive_ids_with_capacity(["rootfs"], 2)
                .expect("bounded metrics registry should allocate");
        registry
            .preflight_drive("data")
            .expect("preflight should accept free identity and capacity");
        assert!(registry.per_drive("data").is_none());
        let prepared = registry
            .prepare_drive("data")
            .expect("second metrics entry should prepare");
        assert!(registry.per_drive("data").is_none());
        assert_eq!(
            registry.prepare_drive("data").unwrap_err(),
            BlockDeviceMetricsRegistryError::DuplicateDrive
        );
        assert_eq!(
            registry.preflight_drive("data").unwrap_err(),
            BlockDeviceMetricsRegistryError::DuplicateDrive
        );
        assert_eq!(
            registry.preflight_drive("other").unwrap_err(),
            BlockDeviceMetricsRegistryError::Capacity
        );
        assert_eq!(
            registry.prepare_drive("other").unwrap_err(),
            BlockDeviceMetricsRegistryError::Capacity
        );
        drop(prepared);

        let prepared = registry
            .prepare_drive("data")
            .expect("abandoned metrics reservation should release its identity and capacity");

        let lease = prepared.publish();
        assert!(registry.per_drive("data").is_some());
        assert_eq!(
            registry.prepare_drive("data").unwrap_err(),
            BlockDeviceMetricsRegistryError::DuplicateDrive
        );
        drop(lease);
        assert!(registry.per_drive("data").is_none());

        let replacement = registry
            .prepare_drive("data")
            .expect("released metrics capacity should be reusable")
            .publish();
        assert!(registry.per_drive("data").is_some());
        drop(replacement);
    }

    #[test]
    fn block_metrics_registry_enforces_configured_capacity() {
        let registry =
            SharedBlockDeviceMetricsRegistry::from_drive_ids_with_capacity(["rootfs"], 1)
                .expect("single-entry registry should allocate");

        assert_eq!(
            registry.prepare_drive("data").unwrap_err(),
            BlockDeviceMetricsRegistryError::Capacity
        );
        assert!(registry.per_drive("rootfs").is_some());
    }

    #[test]
    fn block_metrics_startup_lease_releases_exact_entry_for_same_id_reuse() {
        let registry =
            SharedBlockDeviceMetricsRegistry::from_drive_ids_with_capacity(["rootfs", "data"], 2)
                .expect("bounded startup metrics registry should allocate");
        let lease = registry
            .claim_drive_lease("data")
            .expect("startup data metrics should have exact ownership");
        assert_eq!(
            registry.claim_drive_lease("data").unwrap_err(),
            BlockDeviceMetricsRegistryError::LeaseAlreadyClaimed
        );
        assert_eq!(
            registry.claim_drive_lease("missing").unwrap_err(),
            BlockDeviceMetricsRegistryError::UnknownDrive
        );

        drop(lease);
        assert!(registry.per_drive("data").is_none());
        let replacement = registry
            .prepare_drive("data")
            .expect("released startup metrics identity should be reusable")
            .publish();
        assert!(registry.per_drive("data").is_some());
        drop(replacement);
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
                .with_io_engine_throttled_events(u64::MAX - 14)
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
                    .with_io_engine_throttled_events(u64::MAX)
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
                    .with_io_engine_throttled_events(14)
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
    fn writes_pmem_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_pmem_device_metrics(pmem_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            only_metrics_value(&output)["pmem"],
            serde_json::json!({
                "activate_fails": 1,
                "cfg_fails": 2,
                "event_fails": 3,
                "queue_event_count": 4,
                "rate_limiter_throttled_events": 6,
                "rate_limiter_event_count": 5,
            })
        );
    }

    #[test]
    fn writes_zero_pmem_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_pmem_device_metrics(PmemDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["pmem"]["event_fails"], 0);
    }

    #[test]
    fn writes_pmem_device_metrics_by_device_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let first_metrics = PmemDeviceMetrics::default()
            .with_queue_event_count(1)
            .with_event_fails(1);
        let second_metrics = PmemDeviceMetrics::default().with_queue_event_count(2);
        let diagnostics = MetricsDiagnostics::new()
            .with_pmem_device_metrics(first_metrics.merged_with(second_metrics))
            .with_pmem_device_metrics_by_device(
                PmemDeviceMetricsByDevice::new()
                    .with_device_metrics("pmem0", first_metrics)
                    .with_device_metrics("empty", PmemDeviceMetrics::default())
                    .with_device_metrics("pmem1", second_metrics),
            );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["pmem"]["event_fails"], 1);
        assert_eq!(value["pmem"]["queue_event_count"], 3);
        assert!(value.get("pmem_pmem0").is_none());
        assert!(value.get("pmem_pmem1").is_none());
    }

    #[test]
    fn shared_pmem_device_metrics_registry_snapshot_is_per_instance() {
        let first = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0", "pmem1"]);
        let second = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        first.record_queue_events_for_device("pmem0", 2);
        first.record_event_failure_for_device("pmem0");
        first.aggregate().record_config_failure();
        first
            .per_device("pmem1")
            .expect("pmem1 metrics should exist")
            .record_activation_failure();

        assert_eq!(
            first.aggregate_snapshot(),
            PmemDeviceMetrics::default()
                .with_cfg_fails(1)
                .with_event_fails(1)
                .with_queue_event_count(2)
        );
        assert_eq!(
            first.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new()
                .with_device_metrics(
                    "pmem0",
                    PmemDeviceMetrics::default()
                        .with_event_fails(1)
                        .with_queue_event_count(2),
                )
                .with_device_metrics("pmem1", PmemDeviceMetrics::default().with_activate_fails(1),)
        );
        assert_eq!(second.aggregate_snapshot(), PmemDeviceMetrics::default());
        assert!(second.per_device_snapshot().is_empty());
    }

    #[test]
    fn pmem_metrics_runtime_reservation_is_invisible_and_drop_reuses_capacity() {
        let registry = SharedPmemDeviceMetricsRegistry::from_device_ids_with_capacity([], 1)
            .expect("bounded pmem metrics should construct");
        registry
            .preflight_device("pmem0")
            .expect("preflight should accept free identity and capacity");
        assert!(registry.per_device("pmem0").is_none());
        let prepared = registry
            .prepare_device("pmem0")
            .expect("pmem metrics should reserve");
        assert!(registry.per_device("pmem0").is_none());
        assert_eq!(
            registry.preflight_device("pmem0").unwrap_err(),
            PmemDeviceMetricsRegistryError::DuplicateDevice
        );
        assert_eq!(
            registry.preflight_device("pmem1").unwrap_err(),
            PmemDeviceMetricsRegistryError::Capacity
        );
        assert_eq!(
            registry.prepare_device("pmem1").unwrap_err(),
            PmemDeviceMetricsRegistryError::Capacity
        );

        let lease = prepared.publish();
        registry.record_queue_events_for_device("pmem0", 2);
        assert_eq!(
            registry.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default().with_queue_event_count(2),
            )
        );
        drop(lease);
        assert!(registry.per_device("pmem0").is_none());

        let replacement = registry
            .prepare_device("pmem1")
            .expect("released capacity should be reusable")
            .publish();
        assert!(registry.per_device("pmem1").is_some());
        drop(replacement);
    }

    #[test]
    fn pmem_metrics_startup_lease_removes_only_its_exact_generation() {
        let registry = SharedPmemDeviceMetricsRegistry::from_device_ids_with_capacity(["pmem0"], 1)
            .expect("startup pmem metrics should construct");
        let startup = registry
            .claim_device_lease("pmem0")
            .expect("startup lease should claim");
        assert_eq!(
            registry.claim_device_lease("pmem0").unwrap_err(),
            PmemDeviceMetricsRegistryError::LeaseAlreadyClaimed
        );
        drop(startup);
        assert!(registry.per_device("pmem0").is_none());

        let replacement = registry
            .prepare_device("pmem0")
            .expect("same id should reserve after exact lease drop")
            .publish();
        registry.record_event_failure_for_device("pmem0");
        assert_eq!(
            registry
                .per_device("pmem0")
                .expect("replacement metrics should remain")
                .snapshot(),
            PmemDeviceMetrics::default().with_event_fails(1)
        );
        drop(replacement);
    }

    #[test]
    fn shared_pmem_metric_increment_saturates() {
        let metrics = SharedPmemDeviceMetrics::default();
        metrics.record(PmemDeviceMetrics::default().with_queue_event_count(u64::MAX - 1));

        metrics.record_queue_events(3);

        assert_eq!(metrics.snapshot().queue_event_count(), u64::MAX);
    }

    #[test]
    fn pmem_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_pmem_device_metrics(
            PmemDeviceMetrics::default()
                .with_activate_fails(u64::MAX - 1)
                .with_cfg_fails(u64::MAX - 2)
                .with_event_fails(u64::MAX - 3)
                .with_queue_event_count(u64::MAX - 4)
                .with_rate_limiter_event_count(u64::MAX - 5)
                .with_rate_limiter_throttled_events(u64::MAX - 6),
        );
        let additional =
            MetricsDiagnostics::new().with_pmem_device_metrics(pmem_metrics_with_all_fields());

        assert_eq!(
            base.merged_with(additional).pmem_device_metrics(),
            Some(
                PmemDeviceMetrics::default()
                    .with_activate_fails(u64::MAX)
                    .with_cfg_fails(u64::MAX)
                    .with_event_fails(u64::MAX)
                    .with_queue_event_count(u64::MAX)
                    .with_rate_limiter_event_count(u64::MAX)
                    .with_rate_limiter_throttled_events(u64::MAX)
            )
        );
    }

    #[test]
    fn pmem_diagnostics_merge_per_device_metrics_saturates() {
        let base = MetricsDiagnostics::new().with_pmem_device_metrics_by_device(
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default()
                    .with_event_fails(u64::MAX - 1)
                    .with_queue_event_count(u64::MAX - 2),
            ),
        );
        let additional = MetricsDiagnostics::new().with_pmem_device_metrics_by_device(
            PmemDeviceMetricsByDevice::new()
                .with_device_metrics("pmem0", pmem_metrics_with_all_fields())
                .with_device_metrics("pmem1", PmemDeviceMetrics::default().with_event_fails(3)),
        );
        let expected = PmemDeviceMetricsByDevice::new()
            .with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default()
                    .with_activate_fails(1)
                    .with_cfg_fails(2)
                    .with_event_fails(u64::MAX)
                    .with_queue_event_count(u64::MAX)
                    .with_rate_limiter_event_count(5)
                    .with_rate_limiter_throttled_events(6),
            )
            .with_device_metrics("pmem1", PmemDeviceMetrics::default().with_event_fails(3));
        let merged = base.merged_with(additional);

        assert_eq!(merged.pmem_device_metrics_by_device(), Some(&expected));
    }

    #[test]
    fn writes_network_interface_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let metrics = network_metrics_with_all_fields();
        let diagnostics = MetricsDiagnostics::new()
            .with_network_interface_metrics(metrics)
            .with_network_interface_metrics_by_interface(
                NetworkInterfaceMetricsByInterface::new().with_interface_metrics("eth0", metrics),
            );
        let configured = configured_metrics_devices(&[], &[], &["eth0"]);

        assert_eq!(
            state.flush_with_diagnostics_and_devices(&diagnostics, &configured),
            Ok(true)
        );

        let value = only_metrics_value(&output);
        assert_eq!(value["net"]["activate_fails"], 1);
        assert_eq!(value["net"]["rx_bytes_count"], 7);
        assert_eq!(value["net"]["tx_remaining_reqs_count"], 21);
        assert_eq!(value["net_eth0"]["tx_spoofed_mac_count"], 22);
        assert!(value["net"].get("vmnet_read_count").is_none());
    }

    #[test]
    fn writes_zero_network_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new()
            .with_network_interface_metrics(NetworkInterfaceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["net"]["event_fails"], 0);
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

        let configured = configured_metrics_devices(&[], &[], &["eth0", "eth1"]);
        assert_eq!(
            state.flush_with_diagnostics_and_devices(&diagnostics, &configured),
            Ok(true)
        );

        let lines = output.lines();
        let line = lines.first().expect("one metrics line should be written");
        let value: serde_json::Value =
            serde_json::from_str(line).expect("metrics line should be valid JSON");
        assert_eq!(lines.len(), 1);
        assert_eq!(value["net"]["rx_bytes_count"], 128);
        assert_eq!(value["net"]["tx_bytes_count"], 64);
        assert_eq!(value["net_eth0"]["rx_bytes_count"], 128);
        assert_eq!(value["net_eth0"]["tx_bytes_count"], 0);
        assert_eq!(value["net_eth1"]["rx_bytes_count"], 0);
        assert_eq!(value["net_eth1"]["tx_bytes_count"], 64);
        assert!(value.get("net_noop").is_none());
        assert_eq!(value["vmm"], serde_json::json!({ "panic_count": 0 }));
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
    fn network_backend_metrics_preserve_exact_attempt_result_and_latency_counts() {
        let metrics = SharedNetworkInterfaceMetrics::default();
        let mut backend = VirtioNetworkBackendMetrics::default();
        backend.record_vmnet_read(4, Ok(2), Duration::from_micros(5));
        backend.record_vmnet_read(4, Ok(0), Duration::from_micros(7));
        backend.record_vmnet_read(4, Err(()), Duration::from_micros(11));
        backend.record_vmnet_write(3, Ok(3), Duration::from_micros(13));
        backend.record_vmnet_write(3, Ok(1), Duration::from_micros(17));
        backend.record_vmnet_write(3, Err(()), Duration::from_micros(19));
        backend.record_spoofed_mac();

        metrics.record_backend_metrics(backend);

        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_tx_spoofed_mac_count(1)
                .with_vmnet_read_count(3)
                .with_vmnet_read_fails(1)
                .with_vmnet_read_packets_count(2)
                .with_vmnet_read_partial_batches(1)
                .with_vmnet_write_count(3)
                .with_vmnet_write_fails(1)
                .with_vmnet_write_packets_count(4)
                .with_vmnet_write_partial_batches(1)
                .with_vmnet_read_latency(VirtioNetworkLatencyAggregate::new(5, 11, 23, 3))
                .with_vmnet_write_latency(VirtioNetworkLatencyAggregate::new(13, 19, 49, 3))
        );
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
    fn network_metrics_capture_is_complete_stable_and_generation_aware() {
        let registry = SharedNetworkInterfaceMetricsRegistry::from_interface_ids_with_capacity(
            ["eth0", "eth1"],
            3,
        )
        .expect("capture metrics registry should allocate");
        registry.record_queue_events_for_interface("eth0", 2, 3);

        let first = registry
            .capture_state()
            .expect("stable metrics ownership should capture");
        let second = registry
            .capture_state()
            .expect("unchanged metrics ownership should recapture");
        assert_eq!(first, second);
        assert_eq!(first.entries().len(), 2);
        assert_eq!(first.entries()[0].iface_id(), "eth0");
        assert_eq!(first.entries()[0].generation(), 0);
        assert_eq!(first.entries()[0].metrics().rx_queue_event_count(), 2);
        assert_eq!(first.entries()[1].iface_id(), "eth1");
        assert_eq!(
            first.entries()[1].metrics(),
            NetworkInterfaceMetrics::default(),
            "zero-counter generations must remain in the capture"
        );
        assert_eq!(first.next_generation(), 2);
        assert_eq!(first.aggregate().tx_queue_event_count(), 3);
        assert!(!format!("{first:?}").contains("eth0"));

        let reservation = registry
            .prepare_interface("eth2")
            .expect("capture reservation should prepare");
        assert_eq!(
            registry.capture_state(),
            Err(NetworkInterfaceMetricsCaptureError::ReservationInFlight)
        );
        drop(reservation);
        registry
            .capture_state()
            .expect("abandoned reservation should roll back capture exclusion");

        let old_generation = first.entries()[1].generation();
        let old_lease = registry
            .claim_interface_lease("eth1")
            .expect("startup metrics generation should be claimable");
        drop(old_lease);
        let replacement = registry
            .prepare_interface("eth1")
            .expect("same-ID replacement metrics should prepare")
            .publish();
        let replaced = registry
            .capture_state()
            .expect("replacement metrics generation should capture");
        let replacement_entry = replaced
            .entries()
            .iter()
            .find(|entry| entry.iface_id() == "eth1")
            .expect("replacement capture should include eth1");
        assert!(replacement_entry.generation() > old_generation);
        assert!(replaced.next_generation() > replacement_entry.generation());
        drop(replacement);
    }

    #[test]
    fn network_metrics_preparation_is_invisible_and_published_lease_is_exact() {
        let registry =
            SharedNetworkInterfaceMetricsRegistry::from_interface_ids_with_capacity(["eth0"], 2)
                .expect("bounded network metrics registry should allocate");
        let prepared = registry
            .prepare_interface("eth1")
            .expect("second metrics entry should prepare");
        assert!(registry.per_interface("eth1").is_none());
        assert_eq!(
            registry.prepare_interface("eth1").unwrap_err(),
            NetworkInterfaceMetricsRegistryError::DuplicateInterface
        );
        assert_eq!(
            registry.prepare_interface("eth2").unwrap_err(),
            NetworkInterfaceMetricsRegistryError::Capacity
        );
        drop(prepared);

        let lease = registry
            .prepare_interface("eth1")
            .expect("abandoned reservation should release identity and capacity")
            .publish();
        assert!(registry.per_interface("eth1").is_some());
        registry.record_event_failure_for_interface("eth1");
        assert_eq!(
            registry
                .per_interface("eth1")
                .expect("published metrics should remain visible")
                .snapshot()
                .event_fails(),
            1
        );
        drop(lease);
        assert!(registry.per_interface("eth1").is_none());

        let replacement = registry
            .prepare_interface("eth1")
            .expect("released identity should be reusable")
            .publish();
        assert_eq!(
            registry
                .per_interface("eth1")
                .expect("replacement metrics should be visible")
                .snapshot(),
            NetworkInterfaceMetrics::default(),
            "same-ID reuse must receive a fresh metrics generation"
        );
        drop(replacement);
    }

    #[test]
    fn network_metrics_registry_enforces_configured_capacity() {
        let registry =
            SharedNetworkInterfaceMetricsRegistry::from_interface_ids_with_capacity(["eth0"], 1)
                .expect("single-entry network metrics registry should allocate");

        assert_eq!(
            registry.prepare_interface("eth1").unwrap_err(),
            NetworkInterfaceMetricsRegistryError::Capacity
        );
        assert!(registry.per_interface("eth0").is_some());
    }

    #[test]
    fn network_metrics_startup_lease_releases_exact_entry_for_same_id_reuse() {
        let registry = SharedNetworkInterfaceMetricsRegistry::from_interface_ids_with_capacity(
            ["eth0", "eth1"],
            2,
        )
        .expect("bounded startup network metrics registry should allocate");
        let lease = registry
            .claim_interface_lease("eth1")
            .expect("startup interface metrics should have exact ownership");
        let foreign_registry =
            SharedNetworkInterfaceMetricsRegistry::from_interface_ids_with_capacity(["eth1"], 1)
                .expect("foreign startup metrics registry should allocate");
        assert!(lease.belongs_to(&registry));
        assert!(!lease.belongs_to(&foreign_registry));
        assert_eq!(lease.iface_id(), "eth1");
        assert_eq!(lease.generation(), 1);
        assert_eq!(
            registry.claim_interface_lease("eth1").unwrap_err(),
            NetworkInterfaceMetricsRegistryError::LeaseAlreadyClaimed
        );
        assert_eq!(
            registry.claim_interface_lease("missing").unwrap_err(),
            NetworkInterfaceMetricsRegistryError::UnknownInterface
        );

        drop(lease);
        assert!(registry.per_interface("eth1").is_none());
        let replacement = registry
            .prepare_interface("eth1")
            .expect("released startup identity should be reusable")
            .publish();
        assert!(registry.per_interface("eth1").is_some());
        drop(replacement);
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
                network_metrics_with_all_fields()
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
                network_metrics_with_all_fields()
                    .with_event_fails(u64::MAX)
                    .with_rx_count(u64::MAX),
            )
            .with_interface_metrics("eth1", NetworkInterfaceMetrics::default().with_tx_count(3));
        let merged = base.merged_with(additional);

        assert_eq!(
            merged.network_interface_metrics_by_interface(),
            Some(&expected)
        );
    }

    #[test]
    fn writes_mmds_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_mmds_metrics(mmds_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["mmds"]["rx_accepted"], 1);
        assert_eq!(value["mmds"]["connections_destroyed"], 13);
    }

    #[test]
    fn writes_zero_mmds_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new().with_mmds_metrics(MmdsMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["mmds"]["rx_count"], 0);
    }

    #[test]
    fn shared_mmds_metrics_snapshot_is_per_instance() {
        let first = SharedMmdsMetrics::default();
        let second = SharedMmdsMetrics::default();

        first.record_rx_accepted();
        first.record_rx_accepted_error();
        first.record_rx_accepted_unusual();
        first.record_rx_bad_eth();
        first.record_rx_invalid_token();
        first.record_rx_no_token();
        first.record_rx_count();
        first.record_tx_frame(7);
        first.record_tx_error();
        first.record_connection_created();
        first.record_connection_destroyed();

        assert_eq!(
            first.snapshot(),
            MmdsMetrics::default()
                .with_rx_accepted(1)
                .with_rx_accepted_err(1)
                .with_rx_accepted_unusual(1)
                .with_rx_bad_eth(1)
                .with_rx_invalid_token(1)
                .with_rx_no_token(1)
                .with_rx_count(1)
                .with_tx_bytes(7)
                .with_tx_count(1)
                .with_tx_errors(1)
                .with_tx_frames(1)
                .with_connections_created(1)
                .with_connections_destroyed(1)
        );
        assert_eq!(second.snapshot(), MmdsMetrics::default());
    }

    #[test]
    fn mmds_metric_increment_saturates() {
        let metrics = SharedMmdsMetrics::default();
        metrics
            .inner
            .tx_bytes
            .store(u64::MAX - 1, Ordering::Relaxed);

        metrics.record_tx_frame(3);

        assert_eq!(metrics.snapshot().tx_bytes(), u64::MAX);
    }

    #[test]
    fn mmds_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_mmds_metrics(
            MmdsMetrics::default()
                .with_rx_accepted(u64::MAX - 1)
                .with_tx_bytes(u64::MAX - 2),
        );
        let additional =
            MetricsDiagnostics::new().with_mmds_metrics(mmds_metrics_with_all_fields());

        assert_eq!(
            base.merged_with(additional).mmds_metrics(),
            Some(
                MmdsMetrics::default()
                    .with_rx_accepted(u64::MAX)
                    .with_rx_accepted_err(2)
                    .with_rx_accepted_unusual(3)
                    .with_rx_bad_eth(4)
                    .with_rx_invalid_token(5)
                    .with_rx_no_token(6)
                    .with_rx_count(7)
                    .with_tx_bytes(u64::MAX)
                    .with_tx_count(9)
                    .with_tx_errors(10)
                    .with_tx_frames(11)
                    .with_connections_created(12)
                    .with_connections_destroyed(13)
            )
        );
    }

    #[test]
    fn writes_vsock_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_vsock_device_metrics(vsock_metrics_with_all_fields());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value(&output);
        assert_eq!(value["vsock"]["activate_fails"], 1);
        assert_eq!(value["vsock"]["rx_read_fails"], 20);
    }

    #[test]
    fn writes_zero_vsock_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_vsock_device_metrics(VsockDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["vsock"]["activate_fails"], 0);
    }

    #[test]
    fn shared_vsock_device_metrics_snapshot_is_per_instance() {
        let first = SharedVsockDeviceMetrics::default();
        let first_clone = first.clone();
        let second = SharedVsockDeviceMetrics::default();

        assert!(first.shares_state_with(&first_clone));
        assert!(!first.shares_state_with(&second));

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

        let value = only_metrics_value(&output);
        assert_eq!(value["entropy"]["activate_fails"], 1);
        assert_eq!(value["entropy"]["rate_limiter_event_count"], 7);
        assert!(value["entropy"].get("source_provider_fails").is_none());
    }

    #[test]
    fn writes_zero_entropy_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_entropy_device_metrics(EntropyDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["entropy"]["entropy_bytes"], 0);
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
                .with_entropy_event_fails(1)
                .with_source_provider_fails(1)
        );
        assert_eq!(second.snapshot(), EntropyDeviceMetrics::default());
    }

    #[test]
    fn entropy_metric_increment_saturates() {
        let metrics = SharedEntropyDeviceMetrics::default();
        metrics.record(EntropyDeviceMetrics::default().with_entropy_event_count(u64::MAX - 1));
        metrics.record(EntropyDeviceMetrics::default().with_entropy_event_count(3));

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
                    .with_source_provider_fails(8)
            )
        );
    }

    #[test]
    fn writes_rtc_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new().with_rtc_device_metrics(
            RtcDeviceMetrics::default()
                .with_error_count(3)
                .with_missed_read_count(1)
                .with_missed_write_count(2),
        );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            only_metrics_value(&output)["rtc"],
            serde_json::json!({
                "error_count": 3,
                "missed_read_count": 1,
                "missed_write_count": 2,
            })
        );
    }

    #[test]
    fn writes_zero_rtc_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_rtc_device_metrics(RtcDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["rtc"]["error_count"], 0);
    }

    #[test]
    fn shared_rtc_device_metrics_snapshot_is_per_instance() {
        let first = SharedRtcDeviceMetrics::default();
        let second = SharedRtcDeviceMetrics::default();

        first.record_read_error();
        first.record_write_error();

        assert_eq!(
            first.snapshot(),
            RtcDeviceMetrics::default()
                .with_error_count(2)
                .with_missed_read_count(1)
                .with_missed_write_count(1)
        );
        assert_eq!(second.snapshot(), RtcDeviceMetrics::default());
    }

    #[test]
    fn rtc_metric_increment_saturates() {
        let metrics = SharedRtcDeviceMetrics::default();
        metrics.record(RtcDeviceMetrics::default().with_error_count(u64::MAX - 1));

        metrics.record_read_error();
        metrics.record_write_error();

        assert_eq!(metrics.snapshot().error_count(), u64::MAX);
    }

    #[test]
    fn owner_local_device_snapshots_never_expose_partial_compound_updates() {
        const WRITERS: usize = 4;
        const UPDATES: usize = 2_000;

        let entropy = SharedEntropyDeviceMetrics::default();
        let pmem = SharedPmemDeviceMetrics::default();
        let rtc = SharedRtcDeviceMetrics::default();
        let remaining = Arc::new(AtomicUsize::new(WRITERS));

        thread::scope(|scope| {
            for _ in 0..WRITERS {
                let entropy = entropy.clone();
                let pmem = pmem.clone();
                let rtc = rtc.clone();
                let remaining = Arc::clone(&remaining);
                scope.spawn(move || {
                    for _ in 0..UPDATES {
                        entropy.record(
                            EntropyDeviceMetrics::default()
                                .with_entropy_event_count(1)
                                .with_entropy_event_fails(1)
                                .with_host_rng_fails(1),
                        );
                        pmem.record(
                            PmemDeviceMetrics::default()
                                .with_event_fails(1)
                                .with_queue_event_count(1)
                                .with_rate_limiter_event_count(1),
                        );
                        rtc.record_read_error();
                    }
                    remaining.fetch_sub(1, Ordering::SeqCst);
                });
            }

            while remaining.load(Ordering::SeqCst) != 0 {
                let entropy = entropy.snapshot();
                assert_eq!(entropy.entropy_event_count(), entropy.entropy_event_fails());
                assert_eq!(entropy.entropy_event_fails(), entropy.host_rng_fails());

                let pmem = pmem.snapshot();
                assert_eq!(pmem.event_fails(), pmem.queue_event_count());
                assert_eq!(pmem.queue_event_count(), pmem.rate_limiter_event_count());

                let rtc = rtc.snapshot();
                assert_eq!(rtc.error_count(), rtc.missed_read_count());
                assert_eq!(rtc.missed_write_count(), 0);
            }
        });

        let expected = u64::try_from(WRITERS * UPDATES).expect("test count should fit u64");
        assert_eq!(entropy.snapshot().entropy_event_count(), expected);
        assert_eq!(pmem.snapshot().queue_event_count(), expected);
        assert_eq!(rtc.snapshot().error_count(), expected);
    }

    #[test]
    fn rtc_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_rtc_device_metrics(
            RtcDeviceMetrics::default()
                .with_error_count(u64::MAX - 1)
                .with_missed_read_count(u64::MAX - 2)
                .with_missed_write_count(u64::MAX - 3),
        );
        let additional = MetricsDiagnostics::new().with_rtc_device_metrics(
            RtcDeviceMetrics::default()
                .with_error_count(2)
                .with_missed_read_count(3)
                .with_missed_write_count(4),
        );

        assert_eq!(
            base.merged_with(additional).rtc_device_metrics(),
            Some(
                RtcDeviceMetrics::default()
                    .with_error_count(u64::MAX)
                    .with_missed_read_count(u64::MAX)
                    .with_missed_write_count(u64::MAX)
            )
        );
    }

    #[test]
    fn writes_balloon_device_metrics_when_provided() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics = MetricsDiagnostics::new().with_balloon_device_metrics(
            BalloonDeviceMetrics::new(1, 2, 3, 4, 5, 6)
                .with_discard_metrics(
                    BalloonDiscardMetrics::new(7, 8, 9, 10),
                    BalloonDiscardMetrics::new(11, 12, 13, 14).with_completed_bytes(25),
                )
                .with_free_page_report_metrics(
                    BalloonFreePageReportMetrics::new(15, 16, 17, 18, 19).with_completed_bytes(35),
                ),
        );

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(
            only_metrics_value(&output)["balloon"],
            serde_json::json!({
                "activate_fails": 1,
                "inflate_count": 2,
                "stats_updates_count": 3,
                "stats_update_fails": 4,
                "deflate_count": 5,
                "event_fails": 6,
                "free_page_report_count": 15,
                "free_page_report_freed": 35,
                "free_page_report_fails": 19,
                "free_page_hint_count": 11,
                "free_page_hint_freed": 25,
                "free_page_hint_fails": 14,
            })
        );
    }

    #[test]
    fn writes_zero_balloon_metrics_when_empty() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let diagnostics =
            MetricsDiagnostics::new().with_balloon_device_metrics(BalloonDeviceMetrics::default());

        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        assert_eq!(only_metrics_value(&output)["balloon"]["inflate_count"], 0);
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
    fn writes_firecracker_shaped_memory_hotplug_metrics_without_private_extensions() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let metrics = SharedMemoryHotplugDeviceMetrics::default();

        metrics.record_activation_failure();
        metrics.record_queue_events(2);
        metrics.record_queue_event_failure();
        metrics.record_operation(MemoryHotplugMetricOperation::Plug, true, 2 * 1024 * 1024, 7);
        metrics.record_operation(MemoryHotplugMetricOperation::Plug, false, 0, 9);
        metrics.record_operation(
            MemoryHotplugMetricOperation::Unplug,
            true,
            2 * 1024 * 1024,
            11,
        );
        metrics.record_operation(MemoryHotplugMetricOperation::UnplugAll, false, 0, 13);
        metrics.record_operation(MemoryHotplugMetricOperation::State, true, 0, 17);
        metrics.record_unplug_discard_failures(2);
        metrics.record_interrupt_failure();
        metrics.record_rollbacks(3, 1);
        metrics.record_owner_cleanup(2, 1);
        metrics.record_teardown(true);
        metrics.record_teardown(false);

        let diagnostics =
            MetricsDiagnostics::new().with_memory_hotplug_device_metrics(metrics.snapshot());
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value: serde_json::Value =
            serde_json::from_str(&output.lines()[0]).expect("metrics line should be valid JSON");
        assert_eq!(
            value["memory_hotplug"],
            serde_json::json!({
                "activate_fails": 1,
                "queue_event_fails": 1,
                "queue_event_count": 2,
                "plug_agg": {"min_us": 7, "max_us": 9, "sum_us": 16},
                "plug_count": 2,
                "plug_bytes": 2 * 1024 * 1024,
                "plug_fails": 1,
                "unplug_agg": {"min_us": 11, "max_us": 11, "sum_us": 11},
                "unplug_count": 1,
                "unplug_bytes": 2 * 1024 * 1024,
                "unplug_fails": 0,
                "unplug_discard_fails": 2,
                "unplug_all_agg": {"min_us": 13, "max_us": 13, "sum_us": 13},
                "unplug_all_count": 1,
                "unplug_all_fails": 1,
                "state_agg": {"min_us": 17, "max_us": 17, "sum_us": 17},
                "state_count": 1,
                "state_fails": 0,
            })
        );
    }

    #[test]
    fn memory_hotplug_metric_deltas_do_not_reemit_latency_sums() {
        let output = TestMetricsOutput::default();
        let mut state = MetricsState::with_test_output(output.clone());
        let metrics = SharedMemoryHotplugDeviceMetrics::default();

        metrics.record_operation(MemoryHotplugMetricOperation::Plug, true, 2 * 1024 * 1024, 7);
        let first =
            MetricsDiagnostics::new().with_memory_hotplug_device_metrics(metrics.snapshot());
        assert_eq!(state.flush_with_diagnostics(&first), Ok(true));

        metrics.record_operation(MemoryHotplugMetricOperation::Plug, false, 0, 11);
        let second =
            MetricsDiagnostics::new().with_memory_hotplug_device_metrics(metrics.snapshot());
        assert_eq!(state.flush_with_diagnostics(&second), Ok(true));
        assert_eq!(state.flush_with_diagnostics(&second), Ok(true));

        let lines = output.lines();
        let first: serde_json::Value =
            serde_json::from_str(&lines[0]).expect("first metrics line should be valid JSON");
        let second: serde_json::Value =
            serde_json::from_str(&lines[1]).expect("second metrics line should be valid JSON");
        let unchanged: serde_json::Value =
            serde_json::from_str(&lines[2]).expect("third metrics line should be valid JSON");
        assert_eq!(first["memory_hotplug"]["plug_count"], 1);
        assert_eq!(first["memory_hotplug"]["plug_agg"]["sum_us"], 7);
        assert_eq!(second["memory_hotplug"]["plug_count"], 1);
        assert_eq!(second["memory_hotplug"]["plug_fails"], 1);
        assert_eq!(second["memory_hotplug"]["plug_agg"]["min_us"], 7);
        assert_eq!(second["memory_hotplug"]["plug_agg"]["max_us"], 11);
        assert_eq!(second["memory_hotplug"]["plug_agg"]["sum_us"], 11);
        assert_eq!(unchanged["memory_hotplug"]["plug_count"], 0);
        assert_eq!(unchanged["memory_hotplug"]["plug_agg"]["min_us"], 7);
        assert_eq!(unchanged["memory_hotplug"]["plug_agg"]["max_us"], 11);
        assert_eq!(unchanged["memory_hotplug"]["plug_agg"]["sum_us"], 0);
    }

    #[test]
    fn memory_hotplug_latency_minimum_uses_firecracker_zero_sentinel() {
        let metrics = SharedMemoryHotplugDeviceMetrics::default();

        metrics.record_operation(MemoryHotplugMetricOperation::State, true, 0, 0);
        assert_eq!(metrics.snapshot().state_agg().min_us(), 0);
        metrics.record_operation(MemoryHotplugMetricOperation::State, true, 0, 7);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.state_agg().min_us(), 7);
        assert_eq!(snapshot.state_agg().max_us(), 7);
        assert_eq!(snapshot.state_agg().sum_us(), 7);
        assert_eq!(snapshot.state_agg().sample_count(), 2);
    }

    #[test]
    fn shared_memory_hotplug_metrics_are_per_instance() {
        let first = SharedMemoryHotplugDeviceMetrics::default();
        let second = SharedMemoryHotplugDeviceMetrics::default();

        first.record_queue_events(1);
        first.record_operation(MemoryHotplugMetricOperation::State, false, 0, 5);

        assert_eq!(first.snapshot().queue_event_count(), 1);
        assert_eq!(first.snapshot().state_count(), 1);
        assert_eq!(first.snapshot().state_fails(), 1);
        assert!(second.snapshot().is_empty());
    }

    #[test]
    fn concurrent_memory_hotplug_operation_snapshots_are_coherent() {
        const LATENCY_US: u64 = 7;
        const COMMITTED_BYTES: u64 = 4096;
        const SAMPLE_COUNT_PER_WORKER: usize = 10_000;
        const WORKER_COUNT: usize = 4;

        let metrics = SharedMemoryHotplugDeviceMetrics::default();
        let workers = (0..WORKER_COUNT)
            .map(|_| {
                let metrics = metrics.clone();
                thread::spawn(move || {
                    for _ in 0..SAMPLE_COUNT_PER_WORKER {
                        metrics.record_operation(
                            MemoryHotplugMetricOperation::Plug,
                            true,
                            COMMITTED_BYTES,
                            LATENCY_US,
                        );
                    }
                })
            })
            .collect::<Vec<_>>();

        for _ in 0..SAMPLE_COUNT_PER_WORKER {
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.plug_agg().sample_count(), snapshot.plug_count());
            assert_eq!(
                snapshot.plug_bytes(),
                snapshot.plug_count().saturating_mul(COMMITTED_BYTES)
            );
            assert_eq!(
                snapshot.plug_agg().sum_us(),
                snapshot.plug_count().saturating_mul(LATENCY_US)
            );
            assert_eq!(snapshot.plug_fails(), 0);
            if snapshot.plug_count() != 0 {
                assert_eq!(snapshot.plug_agg().min_us(), LATENCY_US);
                assert_eq!(snapshot.plug_agg().max_us(), LATENCY_US);
            }
        }
        for worker in workers {
            worker.join().expect("latency writer should not panic");
        }

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.plug_count(),
            u64::try_from(WORKER_COUNT * SAMPLE_COUNT_PER_WORKER)
                .expect("test sample count should fit u64")
        );
        assert_eq!(
            snapshot.plug_agg().sum_us(),
            snapshot.plug_count().saturating_mul(LATENCY_US)
        );
    }

    #[test]
    fn balloon_metric_increment_saturates() {
        let metric = AtomicU64::new(u64::MAX - 1);

        super::record_atomic_metric(&metric, 5);

        assert_eq!(metric.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn balloon_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_balloon_device_metrics(
            BalloonDeviceMetrics::new(
                u64::MAX,
                u64::MAX - 1,
                u64::MAX - 2,
                u64::MAX - 3,
                u64::MAX - 4,
                u64::MAX - 5,
            )
            .with_discard_metrics(
                BalloonDiscardMetrics::new(u64::MAX - 1, u64::MAX - 2, u64::MAX - 3, u64::MAX - 4),
                BalloonDiscardMetrics::new(u64::MAX - 5, u64::MAX - 6, u64::MAX - 7, u64::MAX - 8),
            )
            .with_free_page_report_metrics(BalloonFreePageReportMetrics::new(
                u64::MAX - 1,
                u64::MAX - 2,
                u64::MAX - 3,
                u64::MAX - 4,
                u64::MAX - 5,
            )),
        );
        let additional = MetricsDiagnostics::new().with_balloon_device_metrics(
            BalloonDeviceMetrics::new(1, 2, 3, 4, 5, 6)
                .with_discard_metrics(
                    BalloonDiscardMetrics::new(2, 3, 4, 5),
                    BalloonDiscardMetrics::new(6, 7, 8, 9),
                )
                .with_free_page_report_metrics(BalloonFreePageReportMetrics::new(2, 3, 4, 5, 6)),
        );

        assert_eq!(
            base.merged_with(additional).balloon_device_metrics(),
            Some(
                BalloonDeviceMetrics::new(
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                )
                .with_discard_metrics(
                    BalloonDiscardMetrics::new(u64::MAX, u64::MAX, u64::MAX, u64::MAX,),
                    BalloonDiscardMetrics::new(u64::MAX, u64::MAX, u64::MAX, u64::MAX,),
                )
                .with_free_page_report_metrics(BalloonFreePageReportMetrics::new(
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                    u64::MAX,
                ))
            )
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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["api_server"]["process_startup_time_us"], 1_000);
        assert_eq!(value["api_server"]["process_startup_time_cpu_us"], 5_000);

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn includes_parent_cpu_time_when_only_parent_cpu_time_is_provided() {
        let path = unique_metrics_path("startup-parent-only");
        let mut state = MetricsState::default();
        let diagnostics = MetricsDiagnostics::new().with_parent_cpu_time_us(3000);

        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush_with_diagnostics(&diagnostics), Ok(true));

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["api_server"]["process_startup_time_us"], 0);
        assert_eq!(value["api_server"]["process_startup_time_cpu_us"], 3_000);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["api_server"]["process_startup_time_us"], 0);
        assert_eq!(value["api_server"]["process_startup_time_cpu_us"], 2_000);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["api_server"]["process_startup_time_us"], 0);
        assert_eq!(value["api_server"]["process_startup_time_cpu_us"], 0);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["api_server"]["process_startup_time_cpu_us"], u64::MAX);

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn api_request_metric_recorders_saturate_all_schema_fields() {
        let mut state = MetricsState {
            deprecated_api: DeprecatedApiMetrics {
                deprecated_http_api_calls: u64::MAX,
            },
            get_api_requests: GetApiRequestMetrics {
                balloon_count: 0,
                hotplug_memory_count: u64::MAX,
                instance_info_count: u64::MAX,
                vmm_version_count: u64::MAX,
                machine_cfg_count: u64::MAX,
                mmds_count: u64::MAX,
            },
            patch_api_requests: PatchApiRequestMetrics {
                balloon_count: 0,
                balloon_fails: 0,
                drive_count: u64::MAX,
                drive_fails: u64::MAX,
                network_count: u64::MAX,
                network_fails: u64::MAX,
                machine_cfg_count: u64::MAX,
                machine_cfg_fails: u64::MAX,
                mmds_count: u64::MAX,
                mmds_fails: u64::MAX,
                hotplug_memory_count: u64::MAX,
                hotplug_memory_fails: u64::MAX,
                pmem_count: u64::MAX,
                pmem_fails: u64::MAX,
            },
            put_api_requests: PutApiRequestMetrics {
                actions_count: u64::MAX,
                actions_fails: u64::MAX,
                balloon_count: 0,
                balloon_fails: 0,
                boot_source_count: u64::MAX,
                boot_source_fails: u64::MAX,
                cpu_cfg_count: u64::MAX,
                cpu_cfg_fails: u64::MAX,
                drive_count: u64::MAX,
                drive_fails: u64::MAX,
                logger_count: u64::MAX,
                logger_fails: u64::MAX,
                machine_cfg_count: u64::MAX,
                machine_cfg_fails: u64::MAX,
                metrics_count: u64::MAX,
                metrics_fails: u64::MAX,
                hotplug_memory_count: u64::MAX,
                hotplug_memory_fails: u64::MAX,
                mmds_count: u64::MAX,
                mmds_fails: u64::MAX,
                network_count: u64::MAX,
                network_fails: u64::MAX,
                pmem_count: u64::MAX,
                pmem_fails: u64::MAX,
                serial_count: u64::MAX,
                serial_fails: u64::MAX,
                vsock_count: u64::MAX,
                vsock_fails: u64::MAX,
            },
            ..MetricsState::default()
        };
        let expected_deprecated = state.deprecated_api;
        let expected_get = state.get_api_requests;
        let expected_patch = state.patch_api_requests;
        let expected_put = state.put_api_requests;

        state.record_deprecated_api_call();
        state.record_get_hotplug_memory_request();
        state.record_get_instance_info_request();
        state.record_get_machine_config_request();
        state.record_get_mmds_request();
        state.record_get_vmm_version_request();
        state.record_patch_drive_request();
        state.record_patch_drive_failure();
        state.record_patch_hotplug_memory_request();
        state.record_patch_hotplug_memory_failure();
        state.record_patch_machine_config_request();
        state.record_patch_machine_config_failure();
        state.record_patch_mmds_request();
        state.record_patch_mmds_failure();
        state.record_patch_network_request();
        state.record_patch_network_failure();
        state.record_patch_pmem_request();
        state.record_patch_pmem_failure();
        state.record_put_actions_request();
        state.record_put_actions_failure();
        state.record_put_boot_source_request();
        state.record_put_boot_source_failure();
        state.record_put_cpu_config_request();
        state.record_put_cpu_config_failure();
        state.record_put_drive_request();
        state.record_put_drive_failure();
        state.record_put_hotplug_memory_request();
        state.record_put_hotplug_memory_failure();
        state.record_put_logger_request();
        state.record_put_logger_failure();
        state.record_put_machine_config_request();
        state.record_put_machine_config_failure();
        state.record_put_metrics_request();
        state.record_put_metrics_failure();
        state.record_put_mmds_request();
        state.record_put_mmds_failure();
        state.record_put_network_request();
        state.record_put_network_failure();
        state.record_put_pmem_request();
        state.record_put_pmem_failure();
        state.record_put_serial_request();
        state.record_put_serial_failure();
        state.record_put_vsock_request();
        state.record_put_vsock_failure();

        assert_eq!(state.deprecated_api, expected_deprecated);
        assert_eq!(state.get_api_requests, expected_get);
        assert_eq!(state.patch_api_requests, expected_patch);
        assert_eq!(state.put_api_requests, expected_put);
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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["put_api_requests"]["actions_count"], 2);
        assert_eq!(value["put_api_requests"]["actions_fails"], 1);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["patch_api_requests"]["drive_count"], 1);
        assert_eq!(value["patch_api_requests"]["drive_fails"], 1);
        assert_eq!(value["patch_api_requests"]["machine_cfg_count"], 2);
        assert_eq!(value["patch_api_requests"]["mmds_fails"], 1);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["put_api_requests"]["boot_source_count"], 2);
        assert_eq!(value["put_api_requests"]["cpu_cfg_fails"], 1);
        assert_eq!(value["put_api_requests"]["drive_fails"], 1);
        assert_eq!(value["put_api_requests"]["machine_cfg_count"], 2);
        assert_eq!(value["put_api_requests"]["network_fails"], 1);
        assert_eq!(value["put_api_requests"]["vsock_fails"], 1);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["put_api_requests"]["mmds_count"], 2);
        assert_eq!(value["put_api_requests"]["mmds_fails"], 1);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["patch_api_requests"]["pmem_count"], 1);
        assert_eq!(value["patch_api_requests"]["pmem_fails"], 1);
        assert_eq!(value["put_api_requests"]["pmem_count"], 2);
        assert_eq!(value["put_api_requests"]["pmem_fails"], 1);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["get_api_requests"]["hotplug_memory_count"], 1);
        assert_eq!(value["patch_api_requests"]["hotplug_memory_count"], 1);
        assert_eq!(value["patch_api_requests"]["hotplug_memory_fails"], 1);
        assert_eq!(value["put_api_requests"]["hotplug_memory_count"], 2);
        assert_eq!(value["put_api_requests"]["hotplug_memory_fails"], 1);

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn omits_non_schema_balloon_api_request_metrics_when_recorded() {
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

        let value = only_metrics_value_from_file(&path);
        assert!(value["get_api_requests"].get("balloon_count").is_none());
        assert!(value["patch_api_requests"].get("balloon_count").is_none());
        assert!(value["put_api_requests"].get("balloon_count").is_none());

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

        assert_eq!(
            only_metrics_value_from_file(&path)["deprecated_api"]["deprecated_http_api_calls"],
            2
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn writes_every_outer_and_inner_process_latency_when_recorded() {
        let path = unique_metrics_path("process-latencies-us");
        let mut state = MetricsState::default();

        for (operation, boundary, value) in [
            (
                ProcessLatencyOperation::FullCreateSnapshot,
                ProcessLatencyBoundary::OuterApi,
                1,
            ),
            (
                ProcessLatencyOperation::DiffCreateSnapshot,
                ProcessLatencyBoundary::OuterApi,
                2,
            ),
            (
                ProcessLatencyOperation::LoadSnapshot,
                ProcessLatencyBoundary::OuterApi,
                3,
            ),
            (
                ProcessLatencyOperation::PauseVm,
                ProcessLatencyBoundary::OuterApi,
                4,
            ),
            (
                ProcessLatencyOperation::ResumeVm,
                ProcessLatencyBoundary::OuterApi,
                5,
            ),
            (
                ProcessLatencyOperation::FullCreateSnapshot,
                ProcessLatencyBoundary::InnerVmm,
                6,
            ),
            (
                ProcessLatencyOperation::DiffCreateSnapshot,
                ProcessLatencyBoundary::InnerVmm,
                7,
            ),
            (
                ProcessLatencyOperation::LoadSnapshot,
                ProcessLatencyBoundary::InnerVmm,
                8,
            ),
            (
                ProcessLatencyOperation::PauseVm,
                ProcessLatencyBoundary::InnerVmm,
                9,
            ),
            (
                ProcessLatencyOperation::ResumeVm,
                ProcessLatencyBoundary::InnerVmm,
                10,
            ),
        ] {
            state.record_process_latency_us(operation, boundary, value);
        }
        state
            .configure(MetricsConfigInput::new(&path))
            .expect("metrics should configure");
        assert_eq!(state.flush(), Ok(true));

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["latencies_us"]["full_create_snapshot"], 1);
        assert_eq!(value["latencies_us"]["diff_create_snapshot"], 2);
        assert_eq!(value["latencies_us"]["load_snapshot"], 3);
        assert_eq!(value["latencies_us"]["pause_vm"], 4);
        assert_eq!(value["latencies_us"]["resume_vm"], 5);
        assert_eq!(value["latencies_us"]["vmm_full_create_snapshot"], 6);
        assert_eq!(value["latencies_us"]["vmm_diff_create_snapshot"], 7);
        assert_eq!(value["latencies_us"]["vmm_load_snapshot"], 8);
        assert_eq!(value["latencies_us"]["vmm_pause_vm"], 9);
        assert_eq!(value["latencies_us"]["vmm_resume_vm"], 10);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["put_api_requests"]["metrics_count"], 2);
        assert_eq!(value["put_api_requests"]["metrics_fails"], 1);
        assert_eq!(value["put_api_requests"]["logger_count"], 1);
        assert_eq!(value["put_api_requests"]["logger_fails"], 1);
        assert_eq!(value["put_api_requests"]["serial_count"], 1);
        assert_eq!(value["put_api_requests"]["serial_fails"], 1);

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

        let value = only_metrics_value_from_file(&path);
        assert_eq!(value["get_api_requests"]["instance_info_count"], 1);
        assert_eq!(value["get_api_requests"]["machine_cfg_count"], 1);
        assert_eq!(value["get_api_requests"]["mmds_count"], 2);
        assert_eq!(value["get_api_requests"]["vmm_version_count"], 1);

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
    fn signal_diagnostics_merge_saturates() {
        let base = MetricsDiagnostics::new().with_signal_metrics(signal_metrics_with_stores(
            0,
            1,
            u64::MAX - 1,
            0,
        ));
        let additional =
            MetricsDiagnostics::new().with_signal_metrics(signal_metrics_with_stores(1, 0, 2, 1));

        assert_eq!(
            base.merged_with(additional).signal_metrics(),
            Some(signal_metrics_with_stores(1, 1, u64::MAX, 1))
        );
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

        assert_eq!(
            only_metrics_value_from_file(&first_path)["vmm"]["panic_count"],
            0
        );
        assert!(!second_path.exists());

        fs::remove_file(first_path).expect("fixture should clean up");
    }

    #[test]
    fn prepared_metrics_adopts_write_only_file_and_appends_on_flush() {
        let path = unique_metrics_path("provided");
        fs::write(&path, b"seed\n").expect("fixture should write");
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("write-only fixture should open");
        let mut state = MetricsState::default();
        let config = state
            .validate_config(MetricsConfigInput::new("bangbang-grant:metrics"))
            .expect("metrics config should validate");
        let prepared = MetricsState::prepare_config(config, Some(file))
            .expect("provided metrics should prepare");
        assert_eq!(
            format!("{prepared:?}"),
            "PreparedMetricsConfig { sink: \"<owned>\" }"
        );

        state.commit_config(prepared);
        assert_eq!(state.flush(), Ok(true));
        let output = fs::read_to_string(&path).expect("metrics output should read");
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some("seed"));
        let value: serde_json::Value = serde_json::from_str(
            lines
                .next()
                .expect("canonical metrics line should follow seed"),
        )
        .expect("metrics line should be valid JSON");
        assert_eq!(value["vmm"]["panic_count"], 0);
        assert_eq!(lines.next(), None);

        fs::remove_file(path).expect("fixture should clean up");
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
