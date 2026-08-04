mod delivery;
mod event;
mod process_stdout;
mod rate_limiter;

use std::fmt;
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

use delivery::{
    LoggerDelivery, LoggerDeliveryConfig, LoggerEmergencyIngress, LoggerProducer,
    PanicRecordPrefix, PreparedLoggerWriter, ReplaceWriterError,
};
use event::{LogBatch, LogOrigin, LogRecord, LoggerEvent};
#[cfg(test)]
use rate_limiter::LogRateLimiterClock;
use rate_limiter::{LogRateLimitDecision, LoggerRateLimitIdentity, LoggerRateLimiters};

pub use event::{
    LoggerAction, LoggerApiControlOutcome, LoggerApiResultOutcome, LoggerApiRoute,
    LoggerApiWorkerOutcome, LoggerBackendOutcome, LoggerBalloonOutcome, LoggerBlockOutcome,
    LoggerDeviceKind, LoggerEntropyOutcome, LoggerHttpMethod, LoggerLifecycleOutcome,
    LoggerMemoryHotplugOutcome, LoggerNetworkOutcome, LoggerObservabilityOutcome,
    LoggerPmemOutcome, LoggerProcessSignalOutcome, LoggerSerialOutcome, LoggerSnapshotOutcome,
    LoggerTimeIdentityOutcome, LoggerTransportOutcome, LoggerVsockOutcome, PanicLogRecords,
    ProcessStartupOutcome, ProcessTerminalCategory,
};
pub use process_stdout::{ProcessStdoutLogger, ProcessStdoutLoggerError};

const BOOT_TIMER_LOG_MODULE: &str = "bangbang_runtime::boot_timer";
const BACKEND_LOG_MODULE: &str = "bangbang_hvf::backend";
const DEVICE_LOG_MODULE: &str = "bangbang_runtime::device";
const API_REQUEST_LOG_MODULE: &str = "bangbang_runtime::api_server";
const MINIMAL_ACTION_LOG_MODULE: &str = "bangbang_runtime::vmm_action";
const PROCESS_LOG_MODULE: &str = "bangbang::process";
const PANIC_LOG_MODULE: &str = "bangbang::panic";
const LIFECYCLE_LOG_MODULE: &str = "bangbang_runtime::lifecycle";
const SNAPSHOT_LOG_MODULE: &str = "bangbang_runtime::snapshot";
const WORKER_LOG_MODULE: &str = "bangbang::worker";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoggerLevel {
    Off,
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LoggerLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Trace => "Trace",
            Self::Debug => "Debug",
            Self::Info => "Info",
            Self::Warn => "Warn",
            Self::Error => "Error",
        }
    }

    const fn allows(self, level: Self) -> bool {
        match self {
            Self::Off => false,
            Self::Error => matches!(level, Self::Error),
            Self::Warn => matches!(level, Self::Warn | Self::Error),
            Self::Info => matches!(level, Self::Info | Self::Warn | Self::Error),
            Self::Debug => matches!(level, Self::Debug | Self::Info | Self::Warn | Self::Error),
            Self::Trace => !matches!(level, Self::Off),
        }
    }
}

impl fmt::Display for LoggerLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LoggerLevel {
    type Err = LoggerLevelParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(LoggerLevelParseError),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoggerLevelParseError;

impl fmt::Display for LoggerLevelParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("logger level is invalid")
    }
}

impl std::error::Error for LoggerLevelParseError {}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct LoggerConfigInput {
    log_path: Option<PathBuf>,
    level: Option<LoggerLevel>,
    show_level: Option<bool>,
    show_log_origin: Option<bool>,
    module: Option<String>,
}

impl fmt::Debug for LoggerConfigInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoggerConfigInput")
            .field("log_path", &self.log_path.as_ref().map(|_| "<redacted>"))
            .field("level", &self.level)
            .field("show_level", &self.show_level)
            .field("show_log_origin", &self.show_log_origin)
            .field("module", &self.module.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl LoggerConfigInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_log_path(mut self, log_path: impl Into<PathBuf>) -> Self {
        self.log_path = Some(log_path.into());
        self
    }

    pub const fn with_level(mut self, level: LoggerLevel) -> Self {
        self.level = Some(level);
        self
    }

    pub const fn with_show_level(mut self, show_level: bool) -> Self {
        self.show_level = Some(show_level);
        self
    }

    pub const fn with_show_log_origin(mut self, show_log_origin: bool) -> Self {
        self.show_log_origin = Some(show_log_origin);
        self
    }

    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    pub fn validate(self) -> Result<LoggerConfig, LoggerConfigError> {
        if self
            .log_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(LoggerConfigError::EmptyPath);
        }

        Ok(LoggerConfig {
            log_path: self.log_path,
            level: self.level,
            show_level: self.show_level,
            show_log_origin: self.show_log_origin,
            module: self.module,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LoggerConfig {
    log_path: Option<PathBuf>,
    level: Option<LoggerLevel>,
    show_level: Option<bool>,
    show_log_origin: Option<bool>,
    module: Option<String>,
}

impl fmt::Debug for LoggerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoggerConfig")
            .field("log_path", &self.log_path.as_ref().map(|_| "<redacted>"))
            .field("level", &self.level)
            .field("show_level", &self.show_level)
            .field("show_log_origin", &self.show_log_origin)
            .field("module", &self.module.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl LoggerConfig {
    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    pub const fn level(&self) -> Option<LoggerLevel> {
        self.level
    }

    pub const fn show_level(&self) -> Option<bool> {
        self.show_level
    }

    pub const fn show_log_origin(&self) -> Option<bool> {
        self.show_log_origin
    }

    pub fn module(&self) -> Option<&str> {
        self.module.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoggerConfigError {
    EmptyPath,
    OpenFile(std::io::ErrorKind),
    SpawnWorker(std::io::ErrorKind),
    OutputAlreadyInitialized,
    DeliveryQueueFull,
    ReplacementTimedOut,
}

impl fmt::Display for LoggerConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => formatter.write_str("logger path must not be empty"),
            Self::OpenFile(kind) => {
                write!(
                    formatter,
                    "logger output could not be initialized: {kind:?}"
                )
            }
            Self::SpawnWorker(kind) => {
                write!(
                    formatter,
                    "logger worker could not be initialized: {kind:?}"
                )
            }
            Self::OutputAlreadyInitialized => {
                formatter.write_str("logger output is already initialized")
            }
            Self::DeliveryQueueFull => {
                formatter.write_str("logger output replacement queue is full")
            }
            Self::ReplacementTimedOut => formatter.write_str("logger output replacement timed out"),
        }
    }
}

impl std::error::Error for LoggerConfigError {}

#[derive(Debug, Default)]
struct SharedLoggerMetricsInner {
    missed_log_count: AtomicU64,
    rate_limited_log_count: AtomicU64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SharedLoggerMetrics {
    inner: Arc<SharedLoggerMetricsInner>,
}

impl SharedLoggerMetrics {
    fn record_saturating_by(counter: &AtomicU64, amount: usize) {
        let amount = u64::try_from(amount).unwrap_or(u64::MAX);
        let mut current = counter.load(Ordering::Relaxed);
        while current != u64::MAX {
            match counter.compare_exchange_weak(
                current,
                current.saturating_add(amount),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn record_missed_log(&self) {
        self.record_missed_logs(1);
    }

    pub(crate) fn record_missed_logs(&self, count: usize) {
        if count != 0 {
            Self::record_saturating_by(&self.inner.missed_log_count, count);
        }
    }

    pub(crate) fn record_rate_limited_log(&self) {
        Self::record_saturating_by(&self.inner.rate_limited_log_count, 1);
    }

    pub(crate) fn missed_log_count(&self) -> u64 {
        self.inner.missed_log_count.load(Ordering::Relaxed)
    }

    pub(crate) fn rate_limited_log_count(&self) -> u64 {
        self.inner.rate_limited_log_count.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct EmergencyLoggerTarget {
    ingress: Option<Arc<LoggerEmergencyIngress>>,
    prefix: PanicRecordPrefix,
    enabled: bool,
}

impl Default for EmergencyLoggerTarget {
    fn default() -> Self {
        Self {
            ingress: None,
            prefix: PanicRecordPrefix::Plain,
            enabled: false,
        }
    }
}

#[derive(Debug)]
struct EmergencyLoggerInner {
    target: Mutex<EmergencyLoggerTarget>,
    enabled: AtomicBool,
    pending_loss: AtomicBool,
    metrics: SharedLoggerMetrics,
}

/// Narrow, cloneable panic-record admission capability for an executable hook.
#[derive(Clone)]
pub struct EmergencyLogger {
    inner: Arc<EmergencyLoggerInner>,
}

impl fmt::Debug for EmergencyLogger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmergencyLogger")
            .finish_non_exhaustive()
    }
}

impl EmergencyLogger {
    fn new(metrics: SharedLoggerMetrics) -> Self {
        let target = Mutex::new(EmergencyLoggerTarget::default());
        // macOS lazily allocates the pthread-backed mutex on its first lock.
        // Initialize it on this ordinary construction path, never in the hook.
        drop(
            target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Self {
            inner: Arc::new(EmergencyLoggerInner {
                target,
                enabled: AtomicBool::new(false),
                pending_loss: AtomicBool::new(false),
                metrics,
            }),
        }
    }

    /// Attempts one preencoded panic-record publication without waiting or retrying.
    pub fn try_log_panic(&self) -> bool {
        match self.inner.target.try_lock() {
            Ok(target) => {
                if !target.enabled {
                    return false;
                }
                let Some(ingress) = target.ingress.as_deref() else {
                    return false;
                };
                if ingress.publish_once(target.prefix) {
                    true
                } else {
                    self.inner.pending_loss.store(true, Ordering::Release);
                    false
                }
            }
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
                if self.inner.enabled.load(Ordering::Acquire) {
                    self.inner.pending_loss.store(true, Ordering::Release);
                }
                false
            }
        }
    }

    fn update(&self, target: EmergencyLoggerTarget) {
        let enabled = target.enabled;
        let mut current = self
            .inner
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = target;
        drop(current);
        // A contended hook may conservatively linearize before this completed
        // publication; a successful lock always observes the full snapshot.
        self.inner.enabled.store(enabled, Ordering::Release);
    }

    fn settle_pending_loss(&self) {
        if self.inner.pending_loss.swap(false, Ordering::AcqRel) {
            self.inner.metrics.record_missed_logs(1);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BootTimerLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl BootTimerLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::BootTimer)
    }
}

#[derive(Debug, Clone, Default)]
struct ObservabilityWorkerLogRateLimiter {
    inner: LoggerRateLimiters,
}

#[derive(Debug, Clone, Default)]
struct BackendOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
    virtual_timer_activation_logged: Arc<AtomicBool>,
}

impl BackendOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
            virtual_timer_activation_logged: Arc::new(AtomicBool::new(false)),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::BackendOutcome)
    }

    /// Coalesces the repeating successful timer exit before it can consume the
    /// bounded backend budget needed by a later terminal vCPU outcome.
    fn should_log(&self, outcome: LoggerBackendOutcome) -> bool {
        !matches!(outcome, LoggerBackendOutcome::VirtualTimerActivated)
            || !self
                .virtual_timer_activation_logged
                .swap(true, Ordering::AcqRel)
    }
}

#[derive(Debug, Clone, Default)]
struct TransportOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl TransportOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::TransportOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct BalloonOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl BalloonOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::BalloonOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct BlockOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl BlockOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::BlockOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct EntropyOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl EntropyOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::EntropyOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct MemoryHotplugOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl MemoryHotplugOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner
            .check(LoggerRateLimitIdentity::MemoryHotplugOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct PmemOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl PmemOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::PmemOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct NetworkOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl NetworkOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::NetworkOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct SerialOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl SerialOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::SerialOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct TimeIdentityOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl TimeIdentityOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner
            .check(LoggerRateLimitIdentity::TimeIdentityOutcome)
    }
}

#[derive(Debug, Clone, Default)]
struct VsockOutcomeLogRateLimiter {
    inner: LoggerRateLimiters,
}

impl VsockOutcomeLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner.check(LoggerRateLimitIdentity::VsockOutcome)
    }
}

impl ObservabilityWorkerLogRateLimiter {
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: LoggerRateLimiters::with_clock(clock),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        self.inner
            .check(LoggerRateLimitIdentity::ObservabilityWorker)
    }
}

#[derive(Debug, Clone)]
pub struct BootTimerLogger {
    producer: Option<LoggerProducer>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    metrics: SharedLoggerMetrics,
    rate_limiter: BootTimerLogRateLimiter,
}

impl BootTimerLogger {
    #[track_caller]
    pub fn log_boot_time(&self, wall_time_us: u64, cpu_time_us: u64) -> bool {
        const BOOT_TIMER_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(BOOT_TIMER_LEVEL)
            || !module_filter_allows(self.module.as_deref(), BOOT_TIMER_LOG_MODULE)
        {
            return false;
        }

        let Some(producer) = &self.producer else {
            return false;
        };
        let suppressed = match self.rate_limiter.check() {
            LogRateLimitDecision::Admitted { suppressed } => suppressed,
            LogRateLimitDecision::Denied => {
                self.metrics.record_rate_limited_log();
                return false;
            }
        };
        let origin = LogOrigin::from(Location::caller());
        let boot = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            origin,
            BOOT_TIMER_LEVEL,
            LoggerEvent::BootTime {
                wall_time_us,
                cpu_time_us,
            },
        );
        let batch = if suppressed == 0 {
            LogBatch::one(boot)
        } else {
            let recovery = LogRecord::encode(
                self.show_level,
                self.show_log_origin,
                origin,
                LoggerLevel::Warn,
                LoggerEvent::RateLimitRecovery { suppressed },
            );
            LogBatch::two(recovery, boot)
        };

        producer.deliver_nonblocking(batch)
    }

    #[cfg(test)]
    pub(crate) fn wait_for_delivery_for_test(&self) -> bool {
        self.producer
            .as_ref()
            .is_none_or(LoggerProducer::wait_for_idle_for_test)
    }
}

/// Narrow, cloneable admission capability for process-owned asynchronous events.
#[derive(Debug, Clone)]
pub struct AsyncLogger {
    producer: Option<LoggerProducer>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    metrics: SharedLoggerMetrics,
    observability_rate_limiter: ObservabilityWorkerLogRateLimiter,
}

impl AsyncLogger {
    #[track_caller]
    pub fn log_api_worker(&self, outcome: LoggerApiWorkerOutcome) -> bool {
        self.log_unrestricted(outcome.level(), LoggerEvent::ApiWorker(outcome))
    }

    #[track_caller]
    pub fn log_process_signal(&self, outcome: LoggerProcessSignalOutcome) -> bool {
        self.log_unrestricted(outcome.level(), LoggerEvent::ProcessSignal(outcome))
    }

    #[track_caller]
    pub fn log_observability(&self, outcome: LoggerObservabilityOutcome) -> bool {
        let level = outcome.level();
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), WORKER_LOG_MODULE)
        {
            return false;
        }
        let Some(producer) = &self.producer else {
            return false;
        };
        let suppressed = match self.observability_rate_limiter.check() {
            LogRateLimitDecision::Admitted { suppressed } => suppressed,
            LogRateLimitDecision::Denied => {
                self.metrics.record_rate_limited_log();
                return false;
            }
        };
        let origin = LogOrigin::from(Location::caller());
        let outcome = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            origin,
            level,
            LoggerEvent::Observability(outcome),
        );
        let batch = if suppressed == 0 {
            LogBatch::one(outcome)
        } else {
            let recovery = LogRecord::encode(
                self.show_level,
                self.show_log_origin,
                origin,
                LoggerLevel::Warn,
                LoggerEvent::RateLimitRecovery { suppressed },
            );
            LogBatch::two(recovery, outcome)
        };
        producer.deliver_nonblocking(batch)
    }

    #[track_caller]
    fn log_unrestricted(&self, level: LoggerLevel, event: LoggerEvent) -> bool {
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), WORKER_LOG_MODULE)
        {
            return false;
        }
        let Some(producer) = &self.producer else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            level,
            event,
        );
        producer.deliver_nonblocking(LogBatch::one(record))
    }

    #[cfg(test)]
    pub(crate) fn wait_for_delivery_for_test(&self) -> bool {
        self.producer
            .as_ref()
            .is_none_or(LoggerProducer::wait_for_idle_for_test)
    }
}

/// Narrow, cloneable admission capability for guest-triggerable backend,
/// device transport, data-plane, and time/identity events.
///
/// This snapshot exposes no writer, mutable configuration, retry, wait, or
/// delivery-receipt surface. Every production method accepts only a closed
/// semantic outcome and makes at most one nonblocking delivery attempt.
#[derive(Clone, Default)]
pub struct GuestLogger {
    inner: Arc<GuestLoggerInner>,
}

#[derive(Default)]
struct GuestLoggerInner {
    producer: Option<LoggerProducer>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    metrics: SharedLoggerMetrics,
    backend_rate_limiter: BackendOutcomeLogRateLimiter,
    transport_rate_limiter: TransportOutcomeLogRateLimiter,
    balloon_rate_limiter: BalloonOutcomeLogRateLimiter,
    block_rate_limiter: BlockOutcomeLogRateLimiter,
    entropy_rate_limiter: EntropyOutcomeLogRateLimiter,
    memory_hotplug_rate_limiter: MemoryHotplugOutcomeLogRateLimiter,
    pmem_rate_limiter: PmemOutcomeLogRateLimiter,
    network_rate_limiter: NetworkOutcomeLogRateLimiter,
    serial_rate_limiter: SerialOutcomeLogRateLimiter,
    time_identity_rate_limiter: TimeIdentityOutcomeLogRateLimiter,
    vsock_rate_limiter: VsockOutcomeLogRateLimiter,
}

impl fmt::Debug for GuestLogger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuestLogger")
            .field("producer", &self.inner.producer.as_ref().map(|_| "<owned>"))
            .field("level", &self.inner.level)
            .field("show_level", &self.inner.show_level)
            .field("show_log_origin", &self.inner.show_log_origin)
            .field("module", &self.inner.module.as_ref().map(|_| "<redacted>"))
            .field("metrics", &self.inner.metrics)
            .field("backend_rate_limiter", &self.inner.backend_rate_limiter)
            .field("transport_rate_limiter", &self.inner.transport_rate_limiter)
            .field("balloon_rate_limiter", &self.inner.balloon_rate_limiter)
            .field("block_rate_limiter", &self.inner.block_rate_limiter)
            .field("entropy_rate_limiter", &self.inner.entropy_rate_limiter)
            .field(
                "memory_hotplug_rate_limiter",
                &self.inner.memory_hotplug_rate_limiter,
            )
            .field("pmem_rate_limiter", &self.inner.pmem_rate_limiter)
            .field("network_rate_limiter", &self.inner.network_rate_limiter)
            .field("serial_rate_limiter", &self.inner.serial_rate_limiter)
            .field(
                "time_identity_rate_limiter",
                &self.inner.time_identity_rate_limiter,
            )
            .field("vsock_rate_limiter", &self.inner.vsock_rate_limiter)
            .finish()
    }
}

impl GuestLogger {
    #[track_caller]
    pub fn log_backend(&self, outcome: LoggerBackendOutcome) {
        let level = outcome.level();
        if !self.inner.level.allows(level)
            || !module_filter_allows(self.inner.module.as_deref(), BACKEND_LOG_MODULE)
            || self.inner.producer.is_none()
            || !self.inner.backend_rate_limiter.should_log(outcome)
        {
            return;
        }
        self.log_limited(
            level,
            BACKEND_LOG_MODULE,
            LoggerEvent::Backend(outcome),
            || self.inner.backend_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_transport(&self, outcome: LoggerTransportOutcome) {
        if !self.transport_outcome_enabled(outcome) {
            return;
        }
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Transport(outcome),
            || self.inner.transport_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_balloon(&self, outcome: LoggerBalloonOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Balloon(outcome),
            || self.inner.balloon_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub(crate) fn log_balloon_summary(
        &self,
        outcomes: impl IntoIterator<Item = LoggerBalloonOutcome>,
    ) {
        self.log_limited_summary(
            outcomes
                .into_iter()
                .map(|outcome| (outcome.level(), LoggerEvent::Balloon(outcome))),
            || self.inner.balloon_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_block(&self, outcome: LoggerBlockOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Block(outcome),
            || self.inner.block_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_entropy(&self, outcome: LoggerEntropyOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Entropy(outcome),
            || self.inner.entropy_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub(crate) fn log_entropy_summary(
        &self,
        outcomes: impl IntoIterator<Item = LoggerEntropyOutcome>,
    ) {
        self.log_limited_summary(
            outcomes
                .into_iter()
                .map(|outcome| (outcome.level(), LoggerEvent::Entropy(outcome))),
            || self.inner.entropy_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_memory_hotplug(&self, outcome: LoggerMemoryHotplugOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::MemoryHotplug(outcome),
            || self.inner.memory_hotplug_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub(crate) fn log_memory_hotplug_summary(
        &self,
        outcomes: impl IntoIterator<Item = LoggerMemoryHotplugOutcome>,
    ) {
        self.log_limited_summary(
            outcomes
                .into_iter()
                .map(|outcome| (outcome.level(), LoggerEvent::MemoryHotplug(outcome))),
            || self.inner.memory_hotplug_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_pmem(&self, outcome: LoggerPmemOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Pmem(outcome),
            || self.inner.pmem_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_network(&self, outcome: LoggerNetworkOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Network(outcome),
            || self.inner.network_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_serial(&self, outcome: LoggerSerialOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Serial(outcome),
            || self.inner.serial_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_time_identity(&self, outcome: LoggerTimeIdentityOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::TimeIdentity(outcome),
            || self.inner.time_identity_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub fn log_vsock(&self, outcome: LoggerVsockOutcome) {
        self.log_limited(
            outcome.level(),
            DEVICE_LOG_MODULE,
            LoggerEvent::Vsock(outcome),
            || self.inner.vsock_rate_limiter.check(),
        );
    }

    #[track_caller]
    pub(crate) fn log_vsock_summary(&self, outcomes: impl IntoIterator<Item = LoggerVsockOutcome>) {
        self.log_limited_summary(
            outcomes
                .into_iter()
                .map(|outcome| (outcome.level(), LoggerEvent::Vsock(outcome))),
            || self.inner.vsock_rate_limiter.check(),
        );
    }

    pub(crate) fn transport_outcome_enabled(&self, outcome: LoggerTransportOutcome) -> bool {
        self.inner.level.allows(outcome.level())
            && module_filter_allows(self.inner.module.as_deref(), DEVICE_LOG_MODULE)
            && self.inner.producer.is_some()
    }

    #[track_caller]
    fn log_limited(
        &self,
        level: LoggerLevel,
        module: &str,
        event: LoggerEvent,
        check_limiter: impl FnOnce() -> LogRateLimitDecision,
    ) {
        if !self.inner.level.allows(level)
            || !module_filter_allows(self.inner.module.as_deref(), module)
        {
            return;
        }
        let Some(producer) = &self.inner.producer else {
            return;
        };
        let suppressed = match check_limiter() {
            LogRateLimitDecision::Admitted { suppressed } => suppressed,
            LogRateLimitDecision::Denied => {
                self.inner.metrics.record_rate_limited_log();
                return;
            }
        };
        let origin = LogOrigin::from(Location::caller());
        let outcome = LogRecord::encode(
            self.inner.show_level,
            self.inner.show_log_origin,
            origin,
            level,
            event,
        );
        let batch = if suppressed == 0 {
            LogBatch::one(outcome)
        } else {
            let recovery = LogRecord::encode(
                self.inner.show_level,
                self.inner.show_log_origin,
                origin,
                LoggerLevel::Warn,
                LoggerEvent::RateLimitRecovery { suppressed },
            );
            LogBatch::two(recovery, outcome)
        };
        let _delivered = producer.deliver_nonblocking(batch);
    }

    #[track_caller]
    fn log_limited_summary(
        &self,
        events: impl IntoIterator<Item = (LoggerLevel, LoggerEvent)>,
        mut check_limiter: impl FnMut() -> LogRateLimitDecision,
    ) {
        if !module_filter_allows(self.inner.module.as_deref(), DEVICE_LOG_MODULE) {
            return;
        }
        let Some(producer) = &self.inner.producer else {
            return;
        };
        let origin = LogOrigin::from(Location::caller());
        let mut batch = LogBatch::empty();
        let mut suppressed = 0_u64;
        for (level, event) in events {
            if !self.inner.level.allows(level) {
                continue;
            }
            match check_limiter() {
                LogRateLimitDecision::Admitted {
                    suppressed: recovered,
                } => {
                    suppressed = suppressed.saturating_add(recovered);
                    let record = LogRecord::encode(
                        self.inner.show_level,
                        self.inner.show_log_origin,
                        origin,
                        level,
                        event,
                    );
                    if !batch.push(record) {
                        self.inner.metrics.record_missed_logs(1);
                    }
                }
                LogRateLimitDecision::Denied => {
                    self.inner.metrics.record_rate_limited_log();
                }
            }
        }
        if batch.is_empty() {
            return;
        }
        if suppressed != 0 {
            let recovery = LogRecord::encode(
                self.inner.show_level,
                self.inner.show_log_origin,
                origin,
                LoggerLevel::Warn,
                LoggerEvent::RateLimitRecovery { suppressed },
            );
            if !batch.prepend(recovery) {
                self.inner.metrics.record_missed_logs(1);
            }
        }
        let _delivered = producer.deliver_nonblocking(batch);
    }

    #[cfg(test)]
    pub(crate) fn wait_for_delivery_for_test(&self) -> bool {
        self.inner
            .producer
            .as_ref()
            .is_none_or(LoggerProducer::wait_for_idle_for_test)
    }
}

pub struct LoggerState {
    delivery: Option<LoggerDelivery>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    metrics: SharedLoggerMetrics,
    panic_records: Arc<PanicLogRecords>,
    emergency_logger: EmergencyLogger,
    boot_timer_rate_limiter: BootTimerLogRateLimiter,
    observability_rate_limiter: ObservabilityWorkerLogRateLimiter,
    backend_rate_limiter: BackendOutcomeLogRateLimiter,
    transport_rate_limiter: TransportOutcomeLogRateLimiter,
    balloon_rate_limiter: BalloonOutcomeLogRateLimiter,
    block_rate_limiter: BlockOutcomeLogRateLimiter,
    entropy_rate_limiter: EntropyOutcomeLogRateLimiter,
    memory_hotplug_rate_limiter: MemoryHotplugOutcomeLogRateLimiter,
    pmem_rate_limiter: PmemOutcomeLogRateLimiter,
    network_rate_limiter: NetworkOutcomeLogRateLimiter,
    serial_rate_limiter: SerialOutcomeLogRateLimiter,
    time_identity_rate_limiter: TimeIdentityOutcomeLogRateLimiter,
    vsock_rate_limiter: VsockOutcomeLogRateLimiter,
    delivery_config: LoggerDeliveryConfig,
}

impl fmt::Debug for LoggerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoggerState")
            .field("delivery", &self.delivery.as_ref().map(|_| "<owned>"))
            .field("level", &self.level)
            .field("show_level", &self.show_level)
            .field("show_log_origin", &self.show_log_origin)
            .field("module", &self.module.as_ref().map(|_| "<redacted>"))
            .field("metrics", &self.metrics)
            .field("boot_timer_rate_limiter", &self.boot_timer_rate_limiter)
            .field(
                "observability_rate_limiter",
                &self.observability_rate_limiter,
            )
            .field("backend_rate_limiter", &self.backend_rate_limiter)
            .field("transport_rate_limiter", &self.transport_rate_limiter)
            .field("balloon_rate_limiter", &self.balloon_rate_limiter)
            .field("block_rate_limiter", &self.block_rate_limiter)
            .field("entropy_rate_limiter", &self.entropy_rate_limiter)
            .field(
                "memory_hotplug_rate_limiter",
                &self.memory_hotplug_rate_limiter,
            )
            .field("pmem_rate_limiter", &self.pmem_rate_limiter)
            .field("network_rate_limiter", &self.network_rate_limiter)
            .field("serial_rate_limiter", &self.serial_rate_limiter)
            .field(
                "time_identity_rate_limiter",
                &self.time_identity_rate_limiter,
            )
            .field("vsock_rate_limiter", &self.vsock_rate_limiter)
            .field("delivery_config", &self.delivery_config)
            .finish()
    }
}

/// A fully validated logger update whose optional replacement writer is ready.
pub struct PreparedLoggerConfig {
    config: LoggerConfig,
    writer: Option<PreparedLoggerWriter>,
}

impl fmt::Debug for PreparedLoggerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedLoggerConfig")
            .field("config", &self.config)
            .field("writer", &self.writer.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

impl Default for LoggerState {
    fn default() -> Self {
        Self::with_shared_metrics(SharedLoggerMetrics::default())
    }
}

impl LoggerState {
    pub(crate) fn with_shared_metrics(metrics: SharedLoggerMetrics) -> Self {
        let panic_records = Arc::new(PanicLogRecords::new());
        let emergency_logger = EmergencyLogger::new(metrics.clone());
        Self {
            delivery: None,
            level: LoggerLevel::Info,
            show_level: false,
            show_log_origin: false,
            module: None,
            metrics,
            panic_records,
            emergency_logger,
            boot_timer_rate_limiter: BootTimerLogRateLimiter::default(),
            observability_rate_limiter: ObservabilityWorkerLogRateLimiter::default(),
            backend_rate_limiter: BackendOutcomeLogRateLimiter::default(),
            transport_rate_limiter: TransportOutcomeLogRateLimiter::default(),
            balloon_rate_limiter: BalloonOutcomeLogRateLimiter::default(),
            block_rate_limiter: BlockOutcomeLogRateLimiter::default(),
            entropy_rate_limiter: EntropyOutcomeLogRateLimiter::default(),
            memory_hotplug_rate_limiter: MemoryHotplugOutcomeLogRateLimiter::default(),
            pmem_rate_limiter: PmemOutcomeLogRateLimiter::default(),
            network_rate_limiter: NetworkOutcomeLogRateLimiter::default(),
            serial_rate_limiter: SerialOutcomeLogRateLimiter::default(),
            time_identity_rate_limiter: TimeIdentityOutcomeLogRateLimiter::default(),
            vsock_rate_limiter: VsockOutcomeLogRateLimiter::default(),
            delivery_config: LoggerDeliveryConfig::default(),
        }
    }

    pub fn configure(&mut self, input: LoggerConfigInput) -> Result<(), LoggerConfigError> {
        let config = input.validate()?;
        let prepared = Self::prepare_config(config, None)?;
        self.commit_config(prepared)
    }

    pub(crate) fn install_process_stdout(
        &mut self,
        output: ProcessStdoutLogger,
    ) -> Result<(), LoggerConfigError> {
        if self.delivery.is_some() {
            return Err(LoggerConfigError::OutputAlreadyInitialized);
        }
        self.commit_writer(PreparedLoggerWriter::new(output))?;
        self.publish_emergency_target();
        Ok(())
    }

    /// Opens or adopts an optional replacement writer without mutating active state.
    pub fn prepare_config(
        config: LoggerConfig,
        provided_file: Option<File>,
    ) -> Result<PreparedLoggerConfig, LoggerConfigError> {
        let writer = match (config.log_path(), provided_file) {
            (Some(_), Some(file)) => {
                let file = crate::output_file::adopt_write_only_file(file)
                    .map_err(LoggerConfigError::OpenFile)?;
                Some(PreparedLoggerWriter::new(file))
            }
            (Some(path), None) => {
                let file = OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(path)
                    .map_err(|error| LoggerConfigError::OpenFile(error.kind()))?;
                Some(PreparedLoggerWriter::new(file))
            }
            (None, None) => None,
            (None, Some(_)) => {
                return Err(LoggerConfigError::OpenFile(
                    std::io::ErrorKind::InvalidInput,
                ));
            }
        };

        Ok(PreparedLoggerConfig { config, writer })
    }

    /// Commits a prepared update after any writer transaction succeeds.
    pub fn commit_config(
        &mut self,
        prepared: PreparedLoggerConfig,
    ) -> Result<(), LoggerConfigError> {
        let PreparedLoggerConfig { config, writer } = prepared;

        if let Some(writer) = writer {
            self.commit_writer(writer)?;
        }
        self.apply_config(config);
        self.publish_emergency_target();
        Ok(())
    }

    fn commit_writer(&mut self, writer: PreparedLoggerWriter) -> Result<(), LoggerConfigError> {
        let Some(delivery) = &self.delivery else {
            let delivery = LoggerDelivery::spawn(
                writer,
                self.metrics.clone(),
                self.panic_records.clone(),
                self.delivery_config.clone(),
            )
            .map_err(LoggerConfigError::SpawnWorker)?;
            self.delivery = Some(delivery);
            return Ok(());
        };

        match delivery.replace_writer(writer) {
            Ok(()) => Ok(()),
            Err(ReplaceWriterError::Full(_writer)) => Err(LoggerConfigError::DeliveryQueueFull),
            Err(ReplaceWriterError::TimedOut) => Err(LoggerConfigError::ReplacementTimedOut),
            Err(ReplaceWriterError::Disconnected(writer)) => {
                let successor = LoggerDelivery::spawn(
                    writer,
                    self.metrics.clone(),
                    self.panic_records.clone(),
                    self.delivery_config.clone(),
                )
                .map_err(LoggerConfigError::SpawnWorker)?;
                self.delivery = Some(successor);
                Ok(())
            }
        }
    }

    fn apply_config(&mut self, config: LoggerConfig) {
        if let Some(level) = config.level() {
            self.level = level;
        }
        if let Some(show_level) = config.show_level() {
            self.show_level = show_level;
        }
        if let Some(show_log_origin) = config.show_log_origin() {
            self.show_log_origin = show_log_origin;
        }
        if let Some(module) = config.module {
            self.module = Some(module);
        }
    }

    fn publish_emergency_target(&self) {
        let enabled = self.delivery.is_some()
            && self.level.allows(LoggerLevel::Error)
            && module_filter_allows(self.module.as_deref(), PANIC_LOG_MODULE);
        self.emergency_logger.update(EmergencyLoggerTarget {
            ingress: self
                .delivery
                .as_ref()
                .map(LoggerDelivery::emergency_ingress),
            prefix: PanicRecordPrefix::from_flags(self.show_level, self.show_log_origin),
            enabled,
        });
    }

    pub(crate) fn emergency_logger(&self) -> EmergencyLogger {
        self.emergency_logger.clone()
    }

    pub(crate) fn settle_emergency_loss(&self) {
        self.emergency_logger.settle_pending_loss();
    }

    #[track_caller]
    pub(crate) fn log_process_terminal(&self, category: ProcessTerminalCategory) -> bool {
        let level = category.level();
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), PROCESS_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            level,
            LoggerEvent::ProcessExit(category),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub(crate) fn log_action(&self, action: LoggerAction) -> bool {
        const ACTION_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(ACTION_LEVEL)
            || !module_filter_allows(self.module.as_deref(), MINIMAL_ACTION_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            ACTION_LEVEL,
            LoggerEvent::Action(action),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub fn log_api_control(&self, outcome: LoggerApiControlOutcome) -> bool {
        let level = outcome.level();
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), API_REQUEST_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            level,
            LoggerEvent::ApiControl(outcome),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub fn log_api_request(&self, method: LoggerHttpMethod, route: LoggerApiRoute) -> bool {
        const API_REQUEST_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(API_REQUEST_LEVEL)
            || !module_filter_allows(self.module.as_deref(), API_REQUEST_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            API_REQUEST_LEVEL,
            LoggerEvent::ApiRequest { method, route },
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub fn log_api_result(&self, outcome: LoggerApiResultOutcome) -> bool {
        let level = outcome.level();
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), API_REQUEST_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            level,
            LoggerEvent::ApiResult(outcome),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub fn log_process_startup(&self, outcome: ProcessStartupOutcome) -> bool {
        const PROCESS_STARTUP_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(PROCESS_STARTUP_LEVEL)
            || !module_filter_allows(self.module.as_deref(), PROCESS_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            PROCESS_STARTUP_LEVEL,
            LoggerEvent::ProcessStartup(outcome),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    pub fn boot_timer_logger(&self) -> BootTimerLogger {
        BootTimerLogger {
            producer: self.delivery.as_ref().map(LoggerDelivery::producer),
            level: self.level,
            show_level: self.show_level,
            show_log_origin: self.show_log_origin,
            module: self.module.clone(),
            metrics: self.metrics.clone(),
            rate_limiter: self.boot_timer_rate_limiter.clone(),
        }
    }

    pub fn async_logger(&self) -> AsyncLogger {
        AsyncLogger {
            producer: self.delivery.as_ref().map(LoggerDelivery::producer),
            level: self.level,
            show_level: self.show_level,
            show_log_origin: self.show_log_origin,
            module: self.module.clone(),
            metrics: self.metrics.clone(),
            observability_rate_limiter: self.observability_rate_limiter.clone(),
        }
    }

    pub fn guest_logger(&self) -> GuestLogger {
        GuestLogger {
            inner: Arc::new(GuestLoggerInner {
                producer: self.delivery.as_ref().map(LoggerDelivery::producer),
                level: self.level,
                show_level: self.show_level,
                show_log_origin: self.show_log_origin,
                module: self.module.clone(),
                metrics: self.metrics.clone(),
                backend_rate_limiter: self.backend_rate_limiter.clone(),
                transport_rate_limiter: self.transport_rate_limiter.clone(),
                balloon_rate_limiter: self.balloon_rate_limiter.clone(),
                block_rate_limiter: self.block_rate_limiter.clone(),
                entropy_rate_limiter: self.entropy_rate_limiter.clone(),
                memory_hotplug_rate_limiter: self.memory_hotplug_rate_limiter.clone(),
                pmem_rate_limiter: self.pmem_rate_limiter.clone(),
                network_rate_limiter: self.network_rate_limiter.clone(),
                serial_rate_limiter: self.serial_rate_limiter.clone(),
                time_identity_rate_limiter: self.time_identity_rate_limiter.clone(),
                vsock_rate_limiter: self.vsock_rate_limiter.clone(),
            }),
        }
    }

    #[track_caller]
    pub fn log_lifecycle(&self, outcome: LoggerLifecycleOutcome) -> bool {
        let level = outcome.level();
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), LIFECYCLE_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            level,
            LoggerEvent::Lifecycle(outcome),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub fn log_snapshot(&self, outcome: LoggerSnapshotOutcome) -> bool {
        let level = outcome.level();
        if !self.level.allows(level)
            || !module_filter_allows(self.module.as_deref(), SNAPSHOT_LOG_MODULE)
        {
            return false;
        }
        let Some(delivery) = &self.delivery else {
            return false;
        };
        let record = LogRecord::encode(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(Location::caller()),
            level,
            LoggerEvent::Snapshot(outcome),
        );
        delivery.producer().deliver_host(LogBatch::one(record))
    }

    #[track_caller]
    pub fn log_boot_timer(&self, wall_time_us: u64, cpu_time_us: u64) -> bool {
        self.boot_timer_logger()
            .log_boot_time(wall_time_us, cpu_time_us)
    }

    pub const fn level(&self) -> LoggerLevel {
        self.level
    }

    pub const fn show_level(&self) -> bool {
        self.show_level
    }

    pub const fn show_log_origin(&self) -> bool {
        self.show_log_origin
    }

    pub fn module(&self) -> Option<&str> {
        self.module.as_deref()
    }

    #[cfg(test)]
    pub const fn is_configured(&self) -> bool {
        self.delivery.is_some()
    }

    #[cfg(test)]
    pub(crate) fn configure_test_writer(&mut self, writer: impl Write + Send + 'static) {
        self.commit_writer(PreparedLoggerWriter::new(writer))
            .expect("test logger writer should configure");
        self.publish_emergency_target();
    }

    #[cfg(test)]
    fn set_delivery_config_for_test(&mut self, config: LoggerDeliveryConfig) {
        self.delivery_config = config;
    }

    #[cfg(test)]
    fn disconnect_delivery_for_test(&self) -> bool {
        self.delivery
            .as_ref()
            .is_some_and(LoggerDelivery::disconnect_for_test)
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct LoggerTestCapture {
    output: Arc<Mutex<Vec<u8>>>,
}

#[cfg(test)]
impl LoggerTestCapture {
    pub(crate) fn configured_guest_logger(&self) -> (LoggerState, GuestLogger) {
        let mut state = LoggerState::default();
        state.configure_test_writer(self.clone());
        let logger = state.guest_logger();
        (state, logger)
    }

    pub(crate) fn output(&self) -> String {
        String::from_utf8(
            self.output
                .lock()
                .expect("test logger output lock should succeed")
                .clone(),
        )
        .expect("test logger output should be UTF-8")
    }
}

#[cfg(test)]
impl Write for LoggerTestCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.output
            .lock()
            .expect("test logger output lock should succeed")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn module_filter_allows(filter: Option<&str>, module_path: &str) -> bool {
    filter.is_none_or(|filter| module_path.starts_with(filter))
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs::{self, OpenOptions};
    use std::io::{Error, ErrorKind, Read, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::delivery::WorkerObserver;
    use super::{
        BackendOutcomeLogRateLimiter, BalloonOutcomeLogRateLimiter, BlockOutcomeLogRateLimiter,
        BootTimerLogRateLimiter, EntropyOutcomeLogRateLimiter, GuestLogger, LogRateLimitDecision,
        LogRateLimiterClock, LoggerAction, LoggerApiControlOutcome, LoggerApiResultOutcome,
        LoggerApiRoute, LoggerApiWorkerOutcome, LoggerBackendOutcome, LoggerBalloonOutcome,
        LoggerBlockOutcome, LoggerConfigError, LoggerConfigInput, LoggerDeliveryConfig,
        LoggerDeviceKind, LoggerEntropyOutcome, LoggerHttpMethod, LoggerLevel,
        LoggerLifecycleOutcome, LoggerMemoryHotplugOutcome, LoggerNetworkOutcome,
        LoggerObservabilityOutcome, LoggerPmemOutcome, LoggerProcessSignalOutcome,
        LoggerSerialOutcome, LoggerSnapshotOutcome, LoggerState, LoggerTimeIdentityOutcome,
        LoggerTransportOutcome, LoggerVsockOutcome, MemoryHotplugOutcomeLogRateLimiter,
        NetworkOutcomeLogRateLimiter, ObservabilityWorkerLogRateLimiter, PmemOutcomeLogRateLimiter,
        ProcessStartupOutcome, ProcessTerminalCategory, SerialOutcomeLogRateLimiter,
        SharedLoggerMetrics, TimeIdentityOutcomeLogRateLimiter, TransportOutcomeLogRateLimiter,
        VsockOutcomeLogRateLimiter,
    };
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn unique_logger_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-logger-test-{}-{nanos}-{id}-{name}",
            std::process::id()
        ))
    }

    #[derive(Debug, Default)]
    struct TestClock(AtomicU64);

    impl TestClock {
        fn set(&self, now_ms: u64) {
            self.0.store(now_ms, Ordering::Release);
        }
    }

    impl LogRateLimiterClock for TestClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Acquire)
        }
    }

    #[derive(Debug)]
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(Error::from(ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("writer lock should succeed")
                .extend(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct WriterGateState {
        entered: bool,
        released: bool,
    }

    #[derive(Debug, Default)]
    struct WriterGate {
        state: Mutex<WriterGateState>,
        changed: Condvar,
    }

    impl WriterGate {
        fn wait_until_entered(&self) {
            let mut state = self.state.lock().expect("gate lock should succeed");
            while !state.entered {
                state = self.changed.wait(state).expect("gate wait should succeed");
            }
        }

        fn release(&self) {
            self.state
                .lock()
                .expect("gate lock should succeed")
                .released = true;
            self.changed.notify_all();
        }
    }

    #[derive(Debug)]
    struct HeldRecordingWriter {
        gate: Arc<WriterGate>,
        output: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for HeldRecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut state = self.gate.state.lock().expect("gate lock should succeed");
            state.entered = true;
            self.gate.changed.notify_all();
            while !state.released {
                state = self
                    .gate
                    .changed
                    .wait(state)
                    .expect("gate wait should succeed");
            }
            drop(state);
            self.output
                .lock()
                .expect("output lock should succeed")
                .extend(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TestFifo {
        path: PathBuf,
        reader: fs::File,
    }

    impl TestFifo {
        fn create(name: &str) -> Self {
            let path = unique_logger_path(name);
            let c_path = CString::new(path.as_os_str().as_bytes())
                .expect("logger FIFO path should not contain NUL");
            // SAFETY: `c_path` is a live NUL-terminated path owned by this fixture.
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
            let reader = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
                .expect("logger FIFO reader should open");
            Self { path, reader }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn fill_to_capacity(&self) {
            let mut writer = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.path)
                .expect("logger FIFO filler should open");
            let chunk = [b'x'; 4096];
            let mut written = 0;
            loop {
                match writer.write(&chunk) {
                    Ok(0) => panic!("logger FIFO filler unexpectedly wrote zero bytes"),
                    Ok(count) => written += count,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => panic!("logger FIFO filler should reach capacity: {error}"),
                }
            }
            assert!(written != 0);
        }

        fn drain(&mut self) {
            let mut chunk = [0; 4096];
            loop {
                match self.reader.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                    Err(error) => panic!("logger FIFO should drain: {error}"),
                }
            }
        }
    }

    impl Drop for TestFifo {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(condition(), "asynchronous logger condition should arrive");
    }

    #[test]
    fn logger_level_parsing_is_case_insensitive() {
        assert_eq!("trace".parse(), Ok(LoggerLevel::Trace));
        assert_eq!("WARNING".parse(), Ok(LoggerLevel::Warn));
        assert!("verbose".parse::<LoggerLevel>().is_err());
    }

    #[test]
    fn empty_path_is_rejected_without_mutation() {
        let mut state = LoggerState::default();
        assert_eq!(
            state.configure(LoggerConfigInput::new().with_log_path("")),
            Err(LoggerConfigError::EmptyPath)
        );
        assert!(!state.is_configured());
    }

    #[test]
    fn api_and_action_records_preserve_short_text_and_template_selectors() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        state.configure_test_writer(SharedWriter(output.clone()));

        assert!(state.log_api_request(LoggerHttpMethod::Put, LoggerApiRoute::Drive));
        assert!(state.log_action(LoggerAction::InstanceStart));
        assert_eq!(
            String::from_utf8(output.lock().expect("output lock should succeed").clone())
                .expect("logger output should be UTF-8"),
            "The API server received a Put request on \"/drives/{drive_id}\".\naction=InstanceStart\n"
        );
    }

    #[test]
    fn closed_api_control_result_and_startup_producers_apply_levels_and_modules() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        state.configure_test_writer(SharedWriter(output.clone()));

        assert!(state.log_api_control(LoggerApiControlOutcome::ServerRunning));
        assert!(!state.log_api_control(LoggerApiControlOutcome::ServerStopped));
        assert!(state.log_api_control(LoggerApiControlOutcome::ConnectionFailed));
        assert!(state.log_api_control(LoggerApiControlOutcome::RequestDeprecated));
        assert!(state.log_api_result(LoggerApiResultOutcome::NoContent));
        assert!(state.log_api_result(LoggerApiResultOutcome::BadRequest));
        assert!(state.log_process_startup(ProcessStartupOutcome::Running));

        assert_eq!(
            String::from_utf8(output.lock().expect("output lock should succeed").clone())
                .expect("logger output should be UTF-8"),
            concat!(
                "operation=server outcome=running\n",
                "operation=connection outcome=failed\n",
                "operation=request outcome=deprecated\n",
                "action=request outcome=no-content\n",
                "action=request outcome=bad-request\n",
                "operation=process-startup outcome=running\n",
            )
        );

        state
            .configure(LoggerConfigInput::new().with_module("bangbang_runtime::api_server"))
            .expect("API module filter should apply");
        assert!(state.log_api_control(LoggerApiControlOutcome::RequestCompleted));
        assert!(state.log_api_result(LoggerApiResultOutcome::Ok));
        assert!(!state.log_process_startup(ProcessStartupOutcome::Running));

        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Off))
            .expect("off level should apply");
        assert!(!state.log_api_control(LoggerApiControlOutcome::ConnectionFailed));
        assert!(!state.log_api_result(LoggerApiResultOutcome::PayloadTooLarge));
    }

    #[test]
    fn closed_host_event_delivery_failures_are_counted_without_changing_callers() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(FailingWriter);

        assert!(!state.log_api_control(LoggerApiControlOutcome::ConnectionFailed));
        assert!(!state.log_api_result(LoggerApiResultOutcome::BadRequest));
        assert!(!state.log_process_startup(ProcessStartupOutcome::Running));
        assert_eq!(metrics.missed_log_count(), 3);
    }

    #[test]
    fn closed_lifecycle_snapshot_and_async_events_use_fixed_shapes() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        state.configure_test_writer(SharedWriter(output.clone()));

        assert!(state.log_lifecycle(LoggerLifecycleOutcome::BackendStartupSucceeded));
        assert!(
            state.log_lifecycle(LoggerLifecycleOutcome::DeviceAttachSucceeded(
                LoggerDeviceKind::Block,
            ))
        );
        assert!(state.log_snapshot(LoggerSnapshotOutcome::LoadCancelled));
        let logger = state.async_logger();
        assert!(logger.log_api_worker(LoggerApiWorkerOutcome::Running));
        assert!(logger.log_process_signal(LoggerProcessSignalOutcome::GuestReset));
        assert!(logger.log_observability(LoggerObservabilityOutcome::MetricsWorkerFailed));
        assert!(logger.wait_for_delivery_for_test());

        assert_eq!(
            String::from_utf8(output.lock().expect("output lock should succeed").clone())
                .expect("logger output should be UTF-8"),
            concat!(
                "operation=backend-startup outcome=succeeded\n",
                "device-kind=block operation=device-attach outcome=succeeded\n",
                "operation=snapshot-load outcome=cancelled\n",
                "operation=boot-worker outcome=running\n",
                "operation=guest-power outcome=reset\n",
                "operation=metrics-worker outcome=failed\n",
            )
        );
    }

    #[test]
    fn guest_logger_clone_reuses_one_immutable_snapshot() {
        let state = LoggerState::default();
        let logger = state.guest_logger();
        let cloned = logger.clone();
        let refreshed = state.guest_logger();

        assert!(Arc::ptr_eq(&logger.inner, &cloned.inner));
        assert!(!Arc::ptr_eq(&logger.inner, &refreshed.inner));
    }

    #[test]
    fn guest_memory_can_release_logger_before_intentional_owner_retention() {
        let observer = Arc::new(WorkerObserver::default());
        let mut state = LoggerState::default();
        state.set_delivery_config_for_test(
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(10))
                .with_worker_observer(observer.clone()),
        );
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        wait_for(|| observer.active() == 1);
        let range = GuestMemoryRange::new(GuestAddress::new(0), 0x4000)
            .expect("guest-memory range should be valid");
        let layout = GuestMemoryLayout::new(vec![range]).expect("layout should be valid");
        let mut memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        memory.attach_guest_logger(state.guest_logger());

        drop(state);
        assert_eq!(observer.active(), 1);

        memory.attach_guest_logger(GuestLogger::default());
        wait_for(|| observer.active() == 0);
    }

    #[test]
    fn guest_logger_uses_all_closed_guest_outcome_shapes() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        state.configure_test_writer(SharedWriter(output.clone()));
        state
            .configure(
                LoggerConfigInput::new()
                    .with_level(LoggerLevel::Debug)
                    .with_show_level(true),
            )
            .expect("guest logger prefix should configure");
        let logger = state.guest_logger();

        logger.log_backend(LoggerBackendOutcome::VcpuExitGuestReset);
        logger.log_backend(LoggerBackendOutcome::VirtualTimerActivated);
        logger.log_transport(LoggerTransportOutcome::DeviceActivationSucceeded(
            LoggerDeviceKind::Block,
        ));
        logger.log_transport(LoggerTransportOutcome::MmioRegistrationSucceeded);
        logger.log_block(LoggerBlockOutcome::RequestSucceeded);
        logger.log_pmem(LoggerPmemOutcome::FlushFailed);
        logger.log_network(LoggerNetworkOutcome::MmdsRequestDetoured);
        logger.log_vsock(LoggerVsockOutcome::HostConnectionPending);
        assert!(logger.wait_for_delivery_for_test());

        assert_eq!(
            String::from_utf8(output.lock().expect("output lock should succeed").clone())
                .expect("guest logger output should be UTF-8"),
            concat!(
                "level=Info operation=vcpu-exit outcome=guest-reset\n",
                "level=Debug operation=virtual-timer outcome=activated\n",
                "level=Info device-kind=block operation=device-activation outcome=succeeded\n",
                "level=Debug operation=mmio-registration outcome=succeeded\n",
                "level=Info device-kind=block operation=request outcome=succeeded\n",
                "level=Error device-kind=pmem operation=flush outcome=failed\n",
                "level=Info device-kind=network operation=mmds-request outcome=detoured\n",
                "level=Debug device-kind=vsock operation=host-connection outcome=pending\n",
            )
        );
    }

    #[test]
    fn guest_logger_coalesces_virtual_timer_activation_before_terminal_backend_outcome() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(SharedWriter(output.clone()));
        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Debug))
            .expect("debug logger should configure");
        let first = state.guest_logger();
        let second = state.guest_logger();

        for index in 0..20 {
            if index % 2 == 0 {
                first.log_backend(LoggerBackendOutcome::VirtualTimerActivated);
            } else {
                second.log_backend(LoggerBackendOutcome::VirtualTimerActivated);
            }
        }
        second.log_backend(LoggerBackendOutcome::VcpuExitGuestShutdown);
        assert!(second.wait_for_delivery_for_test());

        let output = String::from_utf8(output.lock().expect("output lock should succeed").clone())
            .expect("guest logger output should be UTF-8");
        assert_eq!(
            output
                .matches("operation=virtual-timer outcome=activated\n")
                .count(),
            1
        );
        assert_eq!(
            output
                .matches("operation=vcpu-exit outcome=guest-shutdown\n")
                .count(),
            1
        );
        assert_eq!(metrics.rate_limited_log_count(), 0);
    }

    #[test]
    fn guest_logger_filters_before_independent_limiters_and_recovers_exactly() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let clock = Arc::new(TestClock::default());
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.backend_rate_limiter = BackendOutcomeLogRateLimiter::with_clock(clock.clone());
        state.transport_rate_limiter = TransportOutcomeLogRateLimiter::with_clock(clock.clone());
        state.block_rate_limiter = BlockOutcomeLogRateLimiter::with_clock(clock.clone());
        state.pmem_rate_limiter = PmemOutcomeLogRateLimiter::with_clock(clock.clone());
        state.network_rate_limiter = NetworkOutcomeLogRateLimiter::with_clock(clock.clone());
        state.vsock_rate_limiter = VsockOutcomeLogRateLimiter::with_clock(clock.clone());
        state.configure_test_writer(SharedWriter(output.clone()));
        state
            .configure(LoggerConfigInput::new().with_module("bangbang_runtime::device"))
            .expect("transport module filter should configure");

        let filtered = state.guest_logger();
        for _ in 0..20 {
            filtered.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        }
        filtered.log_transport(LoggerTransportOutcome::DeviceActivationSucceeded(
            LoggerDeviceKind::Entropy,
        ));

        state
            .configure(LoggerConfigInput::new().with_module("bangbang_hvf::backend"))
            .expect("backend module filter should configure");
        let backend = state.guest_logger();
        for _ in 0..10 {
            backend.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        }
        backend.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        assert_eq!(metrics.rate_limited_log_count(), 1);

        clock.set(500);
        backend.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        assert!(backend.wait_for_delivery_for_test());
        let output = String::from_utf8(output.lock().expect("output lock should succeed").clone())
            .expect("guest logger output should be UTF-8");
        assert_eq!(
            output
                .matches("operation=vcpu-run outcome=failed\n")
                .count(),
            11
        );
        assert_eq!(
            output
                .matches("device-kind=entropy operation=device-activation outcome=succeeded\n")
                .count(),
            1
        );
        assert!(output.contains("1 messages were suppressed due to rate limiting\n"));
    }

    #[test]
    fn nine_device_limiters_are_independent_and_filter_before_admission() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let clock = Arc::new(TestClock::default());
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.balloon_rate_limiter = BalloonOutcomeLogRateLimiter::with_clock(clock.clone());
        state.block_rate_limiter = BlockOutcomeLogRateLimiter::with_clock(clock.clone());
        state.entropy_rate_limiter = EntropyOutcomeLogRateLimiter::with_clock(clock.clone());
        state.memory_hotplug_rate_limiter =
            MemoryHotplugOutcomeLogRateLimiter::with_clock(clock.clone());
        state.pmem_rate_limiter = PmemOutcomeLogRateLimiter::with_clock(clock.clone());
        state.network_rate_limiter = NetworkOutcomeLogRateLimiter::with_clock(clock.clone());
        state.serial_rate_limiter = SerialOutcomeLogRateLimiter::with_clock(clock.clone());
        state.time_identity_rate_limiter =
            TimeIdentityOutcomeLogRateLimiter::with_clock(clock.clone());
        state.vsock_rate_limiter = VsockOutcomeLogRateLimiter::with_clock(clock.clone());
        state.configure_test_writer(SharedWriter(output.clone()));
        state
            .configure(LoggerConfigInput::new().with_module("bangbang_hvf::backend"))
            .expect("backend-only filter should configure");

        let filtered = state.guest_logger();
        for _ in 0..20 {
            filtered.log_balloon(LoggerBalloonOutcome::InflateSucceeded);
            filtered.log_block(LoggerBlockOutcome::RequestSucceeded);
            filtered.log_entropy(LoggerEntropyOutcome::FillSucceeded);
            filtered.log_memory_hotplug(LoggerMemoryHotplugOutcome::RequestSucceeded);
            filtered.log_pmem(LoggerPmemOutcome::FlushSucceeded);
            filtered.log_network(LoggerNetworkOutcome::RxSucceeded);
            filtered.log_serial(LoggerSerialOutcome::InputReadSucceeded);
            filtered.log_time_identity(LoggerTimeIdentityOutcome::RtcRestoreSucceeded);
            filtered.log_vsock(LoggerVsockOutcome::RxSucceeded);
        }

        state
            .configure(LoggerConfigInput::new().with_module("bangbang_runtime::device"))
            .expect("device filter should configure");
        let active = state.guest_logger();
        for _ in 0..10 {
            active.log_balloon(LoggerBalloonOutcome::InflateSucceeded);
            active.log_block(LoggerBlockOutcome::RequestSucceeded);
            active.log_entropy(LoggerEntropyOutcome::FillSucceeded);
            active.log_memory_hotplug(LoggerMemoryHotplugOutcome::RequestSucceeded);
            active.log_pmem(LoggerPmemOutcome::FlushSucceeded);
            active.log_network(LoggerNetworkOutcome::RxSucceeded);
            active.log_serial(LoggerSerialOutcome::InputReadSucceeded);
            active.log_time_identity(LoggerTimeIdentityOutcome::RtcRestoreSucceeded);
            active.log_vsock(LoggerVsockOutcome::RxSucceeded);
        }
        active.log_balloon(LoggerBalloonOutcome::InflateSucceeded);
        active.log_block(LoggerBlockOutcome::RequestSucceeded);
        active.log_entropy(LoggerEntropyOutcome::FillSucceeded);
        active.log_memory_hotplug(LoggerMemoryHotplugOutcome::RequestSucceeded);
        active.log_pmem(LoggerPmemOutcome::FlushSucceeded);
        active.log_network(LoggerNetworkOutcome::RxSucceeded);
        active.log_serial(LoggerSerialOutcome::InputReadSucceeded);
        active.log_time_identity(LoggerTimeIdentityOutcome::RtcRestoreSucceeded);
        active.log_vsock(LoggerVsockOutcome::RxSucceeded);
        assert_eq!(metrics.rate_limited_log_count(), 9);

        clock.set(500);
        active.log_balloon(LoggerBalloonOutcome::InflateSucceeded);
        active.log_block(LoggerBlockOutcome::RequestSucceeded);
        active.log_entropy(LoggerEntropyOutcome::FillSucceeded);
        active.log_memory_hotplug(LoggerMemoryHotplugOutcome::RequestSucceeded);
        active.log_pmem(LoggerPmemOutcome::FlushSucceeded);
        active.log_network(LoggerNetworkOutcome::RxSucceeded);
        active.log_serial(LoggerSerialOutcome::InputReadSucceeded);
        active.log_time_identity(LoggerTimeIdentityOutcome::RtcRestoreSucceeded);
        active.log_vsock(LoggerVsockOutcome::RxSucceeded);
        assert!(active.wait_for_delivery_for_test());

        let output = String::from_utf8(output.lock().expect("output lock should succeed").clone())
            .expect("device logger output should be UTF-8");
        for record in [
            "device-kind=balloon operation=inflate outcome=succeeded\n",
            "device-kind=block operation=request outcome=succeeded\n",
            "device-kind=entropy operation=fill outcome=succeeded\n",
            "device-kind=memory-hotplug operation=request outcome=succeeded\n",
            "device-kind=pmem operation=flush outcome=succeeded\n",
            "device-kind=network operation=rx outcome=succeeded\n",
            "device-kind=serial operation=input-read outcome=succeeded\n",
            "device-kind=time-identity operation=rtc-restore outcome=succeeded\n",
            "device-kind=vsock operation=rx outcome=succeeded\n",
        ] {
            assert_eq!(output.matches(record).count(), 11);
        }
        assert_eq!(
            output
                .matches("1 messages were suppressed due to rate limiting\n")
                .count(),
            9
        );
    }

    #[test]
    fn vsock_summary_uses_one_bounded_delivery_attempt() {
        let gate = Arc::new(WriterGate::default());
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.set_delivery_config_for_test(LoggerDeliveryConfig::for_test(
            1,
            Duration::from_millis(100),
        ));
        state.configure_test_writer(HeldRecordingWriter {
            gate: gate.clone(),
            output: output.clone(),
        });
        let logger = state.guest_logger();

        logger.log_vsock_summary([
            LoggerVsockOutcome::GuestConnectionDropped,
            LoggerVsockOutcome::TxSucceeded,
            LoggerVsockOutcome::ConnectionResetQueued,
        ]);
        gate.wait_until_entered();
        assert_eq!(
            metrics.missed_log_count(),
            0,
            "one summary must occupy only one producer queue slot"
        );

        gate.release();
        assert!(logger.wait_for_delivery_for_test());
        assert_eq!(
            *output.lock().expect("output lock should succeed"),
            b"device-kind=vsock operation=guest-connection outcome=dropped\n\
              device-kind=vsock operation=tx outcome=succeeded\n\
              device-kind=vsock operation=connection-reset outcome=queued\n"
        );
    }

    #[test]
    fn guest_logger_delivery_failure_is_observational_and_counted() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(FailingWriter);
        let logger = state.guest_logger();

        logger.log_backend(LoggerBackendOutcome::MemoryMappingFailed);
        logger.log_transport(LoggerTransportOutcome::MmioAccessFailed(Some(
            LoggerDeviceKind::Vsock,
        )));
        logger.log_block(LoggerBlockOutcome::RequestIoFailed);
        logger.log_pmem(LoggerPmemOutcome::FlushFailed);
        logger.log_network(LoggerNetworkOutcome::TxProviderFailed);
        logger.log_vsock(LoggerVsockOutcome::TransportResetFailed);
        assert!(logger.wait_for_delivery_for_test());
        assert_eq!(metrics.missed_log_count(), 6);
    }

    #[test]
    fn guest_logger_snapshot_follows_failure_atomic_writer_replacement() {
        let first = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        state.configure_test_writer(SharedWriter(first.clone()));
        let logger = state.guest_logger();

        logger.log_backend(LoggerBackendOutcome::VcpuExitGuestShutdown);
        assert!(logger.wait_for_delivery_for_test());
        state.configure_test_writer(SharedWriter(second.clone()));
        logger.log_transport(LoggerTransportOutcome::DeviceActivationSucceeded(
            LoggerDeviceKind::Block,
        ));
        assert!(logger.wait_for_delivery_for_test());

        assert_eq!(
            *first.lock().expect("first output lock should succeed"),
            b"operation=vcpu-exit outcome=guest-shutdown\n"
        );
        assert_eq!(
            *second.lock().expect("second output lock should succeed"),
            b"device-kind=block operation=device-activation outcome=succeeded\n"
        );
    }

    #[test]
    fn guest_logger_queue_pressure_counts_loss_without_a_receipt() {
        let gate = Arc::new(WriterGate::default());
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.set_delivery_config_for_test(LoggerDeliveryConfig::for_test(
            1,
            Duration::from_millis(100),
        ));
        state.configure_test_writer(HeldRecordingWriter {
            gate: gate.clone(),
            output,
        });
        let logger = state.guest_logger();

        logger.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        gate.wait_until_entered();
        logger.log_transport(LoggerTransportOutcome::DeviceActivationSucceeded(
            LoggerDeviceKind::Network,
        ));
        logger.log_transport(LoggerTransportOutcome::DeviceResetSucceeded(
            LoggerDeviceKind::Network,
        ));
        assert_eq!(metrics.missed_log_count(), 1);

        gate.release();
        assert!(logger.wait_for_delivery_for_test());
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn disconnected_guest_logger_counts_each_closed_record() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        let logger = state.guest_logger();
        assert!(state.disconnect_delivery_for_test());

        logger.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        logger.log_transport(LoggerTransportOutcome::MmioRegistrationFailed);
        logger.log_block(LoggerBlockOutcome::QueueDispatchFailed);
        logger.log_pmem(LoggerPmemOutcome::QueueDispatchFailed);
        logger.log_network(LoggerNetworkOutcome::QueueDispatchFailed);
        logger.log_vsock(LoggerVsockOutcome::QueueDispatchFailed);
        assert_eq!(metrics.missed_log_count(), 6);
    }

    #[test]
    fn inert_guest_logger_has_no_delivery_or_wait_surface_in_production() {
        let logger = GuestLogger::default();
        logger.log_backend(LoggerBackendOutcome::VcpuRunFailed);
        logger.log_transport(LoggerTransportOutcome::MmioRegistrationFailed);
        logger.log_block(LoggerBlockOutcome::QueueDispatchFailed);
        logger.log_pmem(LoggerPmemOutcome::QueueDispatchFailed);
        logger.log_network(LoggerNetworkOutcome::QueueDispatchFailed);
        logger.log_vsock(LoggerVsockOutcome::QueueDispatchFailed);
        assert!(logger.wait_for_delivery_for_test());
    }

    #[test]
    fn observability_worker_has_an_independent_nonblocking_limiter() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let clock = Arc::new(TestClock::default());
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.observability_rate_limiter =
            ObservabilityWorkerLogRateLimiter::with_clock(clock.clone());
        state.configure_test_writer(SharedWriter(output.clone()));
        let logger = state.async_logger();

        for _ in 0..10 {
            assert!(logger.log_observability(LoggerObservabilityOutcome::MetricsWorkerFailed));
        }
        assert!(!logger.log_observability(LoggerObservabilityOutcome::MetricsWorkerFailed));
        assert_eq!(metrics.rate_limited_log_count(), 1);

        clock.set(500);
        assert!(logger.log_observability(LoggerObservabilityOutcome::MetricsWorkerFailed));
        assert!(logger.wait_for_delivery_for_test());
        let output = String::from_utf8(output.lock().expect("output lock should succeed").clone())
            .expect("logger output should be UTF-8");
        assert_eq!(
            output
                .matches("operation=metrics-worker outcome=failed\n")
                .count(),
            11
        );
        assert!(output.contains("1 messages were suppressed due to rate limiting\n"));
    }

    #[test]
    fn disconnected_async_logger_counts_exact_loss_without_waiting() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(FailingWriter);
        let logger = state.async_logger();
        assert!(state.disconnect_delivery_for_test());

        assert!(!logger.log_api_worker(LoggerApiWorkerOutcome::Failed));
        assert!(!logger.log_process_signal(LoggerProcessSignalOutcome::ShutdownAbnormal));
        assert_eq!(metrics.missed_log_count(), 2);
    }

    #[test]
    fn filters_run_before_delivery_and_limiting() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(FailingWriter);
        state
            .configure(
                LoggerConfigInput::new()
                    .with_level(LoggerLevel::Error)
                    .with_module("not-the-runtime"),
            )
            .expect("path-free update should succeed");

        assert!(!state.log_api_request(LoggerHttpMethod::Get, LoggerApiRoute::Version));
        assert!(!state.log_boot_timer(1_000, 200));
        assert_eq!(metrics.missed_log_count(), 0);
        assert_eq!(metrics.rate_limited_log_count(), 0);
    }

    #[test]
    fn host_failure_is_counted_without_changing_caller() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(FailingWriter);

        assert!(!state.log_action(LoggerAction::FlushMetrics));
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn boot_timer_submission_is_async_and_failure_is_eventually_counted() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(FailingWriter);
        let logger = state.boot_timer_logger();

        assert!(logger.log_boot_time(1_000, 200));
        assert!(logger.wait_for_delivery_for_test());
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn replacement_reuses_delivery_and_switches_output_boundary() {
        let first = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(Mutex::new(Vec::new()));
        let observer = Arc::new(WorkerObserver::default());
        let mut state = LoggerState::default();
        state.set_delivery_config_for_test(
            LoggerDeliveryConfig::for_test(4, Duration::from_millis(100))
                .with_worker_observer(observer.clone()),
        );
        state.configure_test_writer(SharedWriter(first.clone()));
        let stale = state.boot_timer_logger();

        assert!(state.log_action(LoggerAction::InstanceStart));
        state.configure_test_writer(SharedWriter(second.clone()));
        assert!(stale.log_boot_time(1_000, 200));
        assert!(stale.wait_for_delivery_for_test());

        assert_eq!(
            *first.lock().expect("first output lock should succeed"),
            b"action=InstanceStart\n"
        );
        assert_eq!(
            *second.lock().expect("second output lock should succeed"),
            b"Guest-boot-time =   1000 us 1 ms,    200 CPU us 0 CPU ms\n"
        );
        assert_eq!(observer.started(), 1);
        assert_eq!(observer.active(), 1);
    }

    #[test]
    fn stalled_writer_cancels_replacement_and_preserves_facade() {
        let gate = Arc::new(WriterGate::default());
        let old_output = Arc::new(Mutex::new(Vec::new()));
        let candidate_path = unique_logger_path("cancelled-replacement");
        let observer = Arc::new(WorkerObserver::default());
        let mut state = LoggerState::default();
        state.set_delivery_config_for_test(
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(10))
                .with_worker_observer(observer.clone()),
        );
        state.configure_test_writer(HeldRecordingWriter {
            gate: gate.clone(),
            output: old_output.clone(),
        });
        let stale = state.boot_timer_logger();
        let emergency = state.emergency_logger();
        assert!(stale.log_boot_time(1_000, 200));
        gate.wait_until_entered();

        let candidate = LoggerConfigInput::new()
            .with_log_path(&candidate_path)
            .with_level(LoggerLevel::Error)
            .with_show_level(true)
            .validate()
            .expect("replacement config should validate");
        let prepared = LoggerState::prepare_config(candidate, None)
            .expect("replacement writer should prepare");
        assert_eq!(
            state.commit_config(prepared),
            Err(LoggerConfigError::ReplacementTimedOut)
        );
        assert_eq!(state.level(), LoggerLevel::Info);
        assert!(!state.show_level());
        assert_eq!(observer.started(), 1);

        gate.release();
        assert!(stale.wait_for_delivery_for_test());
        assert!(emergency.try_log_panic());
        assert!(state.log_action(LoggerAction::InstanceStart));
        assert_eq!(
            String::from_utf8(
                old_output
                    .lock()
                    .expect("old output lock should succeed")
                    .clone()
            )
            .expect("old output should be UTF-8"),
            concat!(
                "Guest-boot-time =   1000 us 1 ms,    200 CPU us 0 CPU ms\n",
                "event=process-panic\n",
                "action=InstanceStart\n",
            )
        );
        assert_eq!(
            fs::metadata(&candidate_path)
                .expect("candidate should have opened")
                .len(),
            0
        );
        fs::remove_file(candidate_path).expect("candidate fixture should clean up");
    }

    #[test]
    fn repeated_stalled_replacements_never_spawn_another_generation() {
        let gate = Arc::new(WriterGate::default());
        let observer = Arc::new(WorkerObserver::default());
        let mut state = LoggerState::default();
        state.set_delivery_config_for_test(
            LoggerDeliveryConfig::for_test(3, Duration::from_millis(5))
                .with_worker_observer(observer.clone()),
        );
        state.configure_test_writer(HeldRecordingWriter {
            gate: gate.clone(),
            output: Arc::new(Mutex::new(Vec::new())),
        });
        let stale = state.boot_timer_logger();
        assert!(stale.log_boot_time(1_000, 200));
        gate.wait_until_entered();

        let mut paths = Vec::new();
        for attempt in 0..8 {
            let path = unique_logger_path(&format!("repeated-{attempt}"));
            let config = LoggerConfigInput::new()
                .with_log_path(&path)
                .validate()
                .expect("replacement config should validate");
            let prepared = LoggerState::prepare_config(config, None)
                .expect("replacement writer should prepare");
            let error = state
                .commit_config(prepared)
                .expect_err("held worker should reject replacement");
            if attempt < 3 {
                assert_eq!(error, LoggerConfigError::ReplacementTimedOut);
            } else {
                assert_eq!(error, LoggerConfigError::DeliveryQueueFull);
            }
            paths.push(path);
        }
        assert_eq!(observer.started(), 1);
        assert_eq!(observer.active(), 1);

        gate.release();
        assert!(stale.wait_for_delivery_for_test());
        assert_eq!(observer.started(), 1);
        for path in paths {
            assert_eq!(
                fs::metadata(&path)
                    .expect("candidate file should exist")
                    .len(),
                0
            );
            fs::remove_file(path).expect("candidate fixture should clean up");
        }
    }

    #[test]
    fn disconnected_worker_gets_one_successor_and_stale_clone_stays_disconnected() {
        let observer = Arc::new(WorkerObserver::default());
        let metrics = SharedLoggerMetrics::default();
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.set_delivery_config_for_test(
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(10))
                .with_worker_observer(observer.clone()),
        );
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        let stale = state.boot_timer_logger();
        let emergency = state.emergency_logger();

        assert!(state.disconnect_delivery_for_test());
        wait_for(|| observer.active() == 0);
        assert_eq!(observer.started(), 1);
        state.configure_test_writer(SharedWriter(output.clone()));
        wait_for(|| observer.active() == 1);
        assert_eq!(observer.started(), 2);

        assert!(!stale.log_boot_time(1_000, 200));
        assert!(emergency.try_log_panic());
        assert!(state.log_action(LoggerAction::FlushMetrics));
        assert_eq!(metrics.missed_log_count(), 1);
        assert_eq!(
            *output.lock().expect("successor output lock should succeed"),
            b"event=process-panic\naction=FlushMetrics\n"
        );
    }

    #[test]
    fn genuinely_full_nonblocking_fifo_misses_without_blocking_guest() {
        let mut fifo = TestFifo::create("full-fifo");
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state
            .configure(LoggerConfigInput::new().with_log_path(fifo.path()))
            .expect("FIFO logger should configure");
        fifo.fill_to_capacity();
        let logger = state.boot_timer_logger();

        assert!(logger.log_boot_time(1_000, 200));
        assert!(logger.wait_for_delivery_for_test());
        assert_eq!(metrics.missed_log_count(), 1);
        fifo.drain();
    }

    #[test]
    fn initial_spawn_failure_preserves_all_facade_fields() {
        let mut state = LoggerState::default();
        let path = unique_logger_path("spawn-failure");
        state.set_delivery_config_for_test(
            LoggerDeliveryConfig::for_test(1, Duration::from_millis(10)).with_failed_spawn(),
        );
        let config = LoggerConfigInput::new()
            .with_log_path(&path)
            .with_level(LoggerLevel::Error)
            .with_show_level(true)
            .validate()
            .expect("config should validate");
        let prepared =
            LoggerState::prepare_config(config, None).expect("candidate writer should prepare");

        assert!(matches!(
            state.commit_config(prepared),
            Err(LoggerConfigError::SpawnWorker(_))
        ));
        assert!(!state.is_configured());
        assert_eq!(state.level(), LoggerLevel::Info);
        assert!(!state.show_level());
        fs::remove_file(path).expect("candidate fixture should clean up");
    }

    #[test]
    fn path_free_update_retains_writer() {
        let path = unique_logger_path("path-free");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");
        state
            .configure(LoggerConfigInput::new().with_show_level(true))
            .expect("path-free update should succeed");
        assert!(state.log_action(LoggerAction::FlushMetrics));

        assert_eq!(
            fs::read_to_string(&path).expect("logger output should read"),
            "level=Info action=FlushMetrics\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn process_terminal_records_use_fixed_categories_and_filters() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        state.configure_test_writer(SharedWriter(output.clone()));

        assert!(state.log_process_terminal(ProcessTerminalCategory::Success));
        assert!(state.log_process_terminal(ProcessTerminalCategory::ProcessFailure));
        state
            .configure(LoggerConfigInput::new().with_module("other"))
            .expect("module update should succeed");
        assert!(!state.log_process_terminal(ProcessTerminalCategory::Panic));

        assert_eq!(
            *output.lock().expect("output lock should succeed"),
            b"event=process-exit category=success\nevent=process-exit category=process-failure\n"
        );
    }

    #[test]
    fn emergency_logger_observes_late_configuration_and_prefix_update() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut state = LoggerState::default();
        let emergency = state.emergency_logger();
        assert!(!emergency.try_log_panic());

        state.configure_test_writer(SharedWriter(output.clone()));
        state
            .configure(LoggerConfigInput::new().with_show_level(true))
            .expect("prefix update should succeed");
        assert!(emergency.try_log_panic());
        assert!(state.boot_timer_logger().wait_for_delivery_for_test());

        assert_eq!(
            *output.lock().expect("output lock should succeed"),
            b"level=Error event=process-panic\n"
        );
    }

    #[test]
    fn filtered_emergency_logger_does_not_record_a_loss() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Off))
            .expect("filter update should succeed");

        assert!(!state.emergency_logger().try_log_panic());
        state.settle_emergency_loss();
        assert_eq!(metrics.missed_log_count(), 0);
    }

    #[test]
    fn occupied_emergency_ingress_coalesces_one_deferred_loss() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        let emergency = state.emergency_logger();

        assert!(emergency.try_log_panic());
        assert!(!emergency.try_log_panic());
        assert!(!emergency.try_log_panic());
        state.settle_emergency_loss();
        state.settle_emergency_loss();
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn contended_emergency_snapshot_returns_without_locking_and_defers_loss() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        let emergency = state.emergency_logger();
        let held = emergency.clone();
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let holder = std::thread::spawn(move || {
            let _guard = held
                .inner
                .target
                .lock()
                .expect("target lock should succeed");
            entered_sender.send(()).expect("holder should signal entry");
            release_receiver
                .recv()
                .expect("holder should receive release");
        });
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("holder should enter");

        assert!(!emergency.try_log_panic());
        release_sender
            .send(())
            .expect("holder should receive release signal");
        holder.join().expect("holder should exit");
        state.settle_emergency_loss();
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn poisoned_emergency_snapshot_returns_without_locking_and_defers_loss() {
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.configure_test_writer(SharedWriter(Arc::new(Mutex::new(Vec::new()))));
        let emergency = state.emergency_logger();
        let poisoned = emergency.clone();
        let poisoner = std::thread::spawn(move || {
            let _guard = poisoned
                .inner
                .target
                .lock()
                .expect("target lock should initially succeed");
            panic!("poison emergency target fixture");
        });
        assert!(poisoner.join().is_err());

        assert!(!emergency.try_log_panic());
        state.settle_emergency_loss();
        state.settle_emergency_loss();
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn wrapper_limiter_preserves_recovery_count() {
        let clock = Arc::new(TestClock::default());
        let limiter = BootTimerLogRateLimiter::with_clock(clock.clone());
        for _ in 0..10 {
            assert_eq!(
                limiter.check(),
                LogRateLimitDecision::Admitted { suppressed: 0 }
            );
        }
        assert_eq!(limiter.check(), LogRateLimitDecision::Denied);
        clock.set(500);
        assert_eq!(
            limiter.check(),
            LogRateLimitDecision::Admitted { suppressed: 1 }
        );
    }

    #[test]
    fn recovery_and_admitted_boot_record_share_ordered_batch() {
        let clock = Arc::new(TestClock::default());
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let mut state = LoggerState::with_shared_metrics(metrics.clone());
        state.boot_timer_rate_limiter = BootTimerLogRateLimiter::with_clock(clock.clone());
        state.configure_test_writer(SharedWriter(output.clone()));
        let logger = state.boot_timer_logger();

        for _ in 0..10 {
            assert!(logger.log_boot_time(1_000, 200));
        }
        assert!(!logger.log_boot_time(1_000, 200));
        clock.set(500);
        assert!(logger.log_boot_time(2_000, 400));
        assert!(logger.wait_for_delivery_for_test());

        let output = String::from_utf8(output.lock().expect("output lock should succeed").clone())
            .expect("logger output should be UTF-8");
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 12);
        assert_eq!(lines[10], "1 messages were suppressed due to rate limiting");
        assert_eq!(
            lines[11],
            "Guest-boot-time =   2000 us 2 ms,    400 CPU us 0 CPU ms"
        );
        assert_eq!(metrics.rate_limited_log_count(), 1);
        assert_eq!(metrics.missed_log_count(), 0);
    }

    #[test]
    fn debug_output_redacts_path_and_module() {
        let input = LoggerConfigInput::new()
            .with_log_path("/private/secret.log")
            .with_module("private.module");
        let debug = format!("{input:?}");
        assert!(!debug.contains("secret.log"));
        assert!(!debug.contains("private.module"));

        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_module("private.guest.module"))
            .expect("guest logger module should configure");
        let debug = format!("{:?}", state.guest_logger());
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("private.guest.module"));
    }
}
