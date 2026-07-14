use std::fmt;
use std::fs::OpenOptions;
use std::io::{LineWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Instant;

const BOOT_TIMER_LOG_MODULE: &str = "bangbang_runtime::boot_timer";
const API_REQUEST_LOG_MODULE: &str = "bangbang_runtime::api_server";
const MINIMAL_ACTION_LOG_MODULE: &str = "bangbang_runtime::vmm_action";
const DEFAULT_LOG_RATE_LIMIT_BURST: u64 = 10;
const DEFAULT_LOG_RATE_LIMIT_REFILL_MS: u64 = 5_000;
const DEFAULT_LOG_RATE_LIMIT_PERIOD_MS: u64 =
    DEFAULT_LOG_RATE_LIMIT_REFILL_MS / DEFAULT_LOG_RATE_LIMIT_BURST;

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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("logger level is invalid")
    }
}

impl std::error::Error for LoggerLevelParseError {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoggerConfigInput {
    log_path: Option<PathBuf>,
    level: Option<LoggerLevel>,
    show_level: Option<bool>,
    show_log_origin: Option<bool>,
    module: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggerConfig {
    log_path: Option<PathBuf>,
    level: Option<LoggerLevel>,
    show_level: Option<bool>,
    show_log_origin: Option<bool>,
    module: Option<String>,
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
}

impl fmt::Display for LoggerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("logger path must not be empty"),
            Self::OpenFile(kind) => write!(f, "logger output could not be initialized: {kind:?}"),
        }
    }
}

impl std::error::Error for LoggerConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoggerDeliveryError {
    LockContended,
    LockPoisoned,
    Write,
}

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
    fn record_saturating(counter: &AtomicU64) {
        let mut current = counter.load(Ordering::Relaxed);
        while current != u64::MAX {
            let next = current.saturating_add(1);
            match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn record_missed_log(&self) {
        Self::record_saturating(&self.inner.missed_log_count);
    }

    pub(crate) fn record_rate_limited_log(&self) {
        Self::record_saturating(&self.inner.rate_limited_log_count);
    }

    pub(crate) fn missed_log_count(&self) -> u64 {
        self.inner.missed_log_count.load(Ordering::Relaxed)
    }

    pub(crate) fn rate_limited_log_count(&self) -> u64 {
        self.inner.rate_limited_log_count.load(Ordering::Relaxed)
    }
}

trait LogRateLimiterClock: fmt::Debug + Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug)]
struct SystemLogRateLimiterClock {
    epoch: Instant,
}

impl Default for SystemLogRateLimiterClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }
}

impl LogRateLimiterClock for SystemLogRateLimiterClock {
    fn now_ms(&self) -> u64 {
        let elapsed = self.epoch.elapsed();
        elapsed
            .as_secs()
            .saturating_mul(1_000)
            .saturating_add(u64::from(elapsed.subsec_millis()))
    }
}

#[derive(Debug, Default)]
struct LogRateLimiterState {
    theoretical_arrival_time_ms: u64,
    suppressed: u64,
}

#[derive(Debug)]
struct LogRateLimiterInner {
    clock: Arc<dyn LogRateLimiterClock>,
    state: Mutex<LogRateLimiterState>,
}

#[derive(Debug, Clone)]
struct BootTimerLogRateLimiter {
    inner: Arc<LogRateLimiterInner>,
}

impl Default for BootTimerLogRateLimiter {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemLogRateLimiterClock::default()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogRateLimitDecision {
    Admitted { suppressed: u64 },
    Denied,
}

impl BootTimerLogRateLimiter {
    fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: Arc::new(LogRateLimiterInner {
                clock,
                state: Mutex::new(LogRateLimiterState::default()),
            }),
        }
    }

    fn check(&self) -> LogRateLimitDecision {
        let now_ms = self.inner.clock.now_ms();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_theoretical_arrival_time_ms = state
            .theoretical_arrival_time_ms
            .max(now_ms)
            .saturating_add(DEFAULT_LOG_RATE_LIMIT_PERIOD_MS);

        if next_theoretical_arrival_time_ms.saturating_sub(now_ms)
            > DEFAULT_LOG_RATE_LIMIT_REFILL_MS
        {
            state.suppressed = state.suppressed.saturating_add(1);
            return LogRateLimitDecision::Denied;
        }

        state.theoretical_arrival_time_ms = next_theoretical_arrival_time_ms;
        let suppressed = state.suppressed;
        state.suppressed = 0;
        LogRateLimitDecision::Admitted { suppressed }
    }
}

#[derive(Debug, Clone)]
pub struct BootTimerLogger {
    sink: Option<LoggerSink>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    metrics: SharedLoggerMetrics,
    rate_limiter: BootTimerLogRateLimiter,
}

impl BootTimerLogger {
    fn record_delivery(&self, result: Result<(), LoggerDeliveryError>) -> bool {
        if result.is_err() {
            self.metrics.record_missed_log();
            return false;
        }
        true
    }

    #[track_caller]
    pub fn log_boot_time(&self, wall_time_us: u64, cpu_time_us: u64) -> bool {
        const BOOT_TIMER_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(BOOT_TIMER_LEVEL) {
            return false;
        }

        if !module_filter_allows(self.module.as_deref(), BOOT_TIMER_LOG_MODULE) {
            return false;
        }

        let Some(sink) = &self.sink else {
            return false;
        };

        let suppressed = match self.rate_limiter.check() {
            LogRateLimitDecision::Admitted { suppressed } => suppressed,
            LogRateLimitDecision::Denied => {
                self.metrics.record_rate_limited_log();
                return false;
            }
        };
        let origin = Location::caller();

        if suppressed != 0 {
            self.record_delivery(sink.write_rate_limit_recovery(
                self.show_level,
                self.show_log_origin,
                origin,
                LoggerLevel::Warn,
                suppressed,
            ));
        }

        self.record_delivery(sink.write_boot_timer(
            self.show_level,
            self.show_log_origin,
            origin,
            BOOT_TIMER_LEVEL,
            wall_time_us,
            cpu_time_us,
        ))
    }
}

#[derive(Debug)]
pub struct LoggerState {
    sink: Option<LoggerSink>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    metrics: SharedLoggerMetrics,
    boot_timer_rate_limiter: BootTimerLogRateLimiter,
}

impl Default for LoggerState {
    fn default() -> Self {
        Self {
            sink: None,
            level: LoggerLevel::Info,
            show_level: false,
            show_log_origin: false,
            module: None,
            metrics: SharedLoggerMetrics::default(),
            boot_timer_rate_limiter: BootTimerLogRateLimiter::default(),
        }
    }
}

impl LoggerState {
    pub(crate) fn with_shared_metrics(metrics: SharedLoggerMetrics) -> Self {
        Self {
            metrics,
            ..Self::default()
        }
    }

    fn record_delivery(&self, result: Result<(), LoggerDeliveryError>) -> bool {
        if result.is_err() {
            self.metrics.record_missed_log();
            return false;
        }
        true
    }

    pub fn configure(&mut self, input: LoggerConfigInput) -> Result<(), LoggerConfigError> {
        let config = input.validate()?;
        let sink = config.log_path().map(LoggerSink::open).transpose()?;

        if let Some(sink) = sink {
            self.sink = Some(sink);
        }
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

        Ok(())
    }

    #[track_caller]
    pub(crate) fn log_action(&self, action: &str) -> bool {
        const ACTION_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(ACTION_LEVEL) {
            return false;
        }

        if !module_filter_allows(self.module.as_deref(), MINIMAL_ACTION_LOG_MODULE) {
            return false;
        }

        let Some(sink) = &self.sink else {
            return false;
        };

        self.record_delivery(sink.write_action(
            self.show_level,
            self.show_log_origin,
            Location::caller(),
            ACTION_LEVEL,
            action,
        ))
    }

    #[track_caller]
    pub fn log_api_request(&self, method: &str, path: impl fmt::Display) -> bool {
        const API_REQUEST_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(API_REQUEST_LEVEL) {
            return false;
        }

        if !module_filter_allows(self.module.as_deref(), API_REQUEST_LOG_MODULE) {
            return false;
        }

        let Some(sink) = &self.sink else {
            return false;
        };

        self.record_delivery(sink.write_api_request(
            self.show_level,
            self.show_log_origin,
            Location::caller(),
            API_REQUEST_LEVEL,
            method,
            path,
        ))
    }

    pub fn boot_timer_logger(&self) -> BootTimerLogger {
        BootTimerLogger {
            sink: self.sink.clone(),
            level: self.level,
            show_level: self.show_level,
            show_log_origin: self.show_log_origin,
            module: self.module.clone(),
            metrics: self.metrics.clone(),
            rate_limiter: self.boot_timer_rate_limiter.clone(),
        }
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
        self.sink.is_some()
    }

    #[cfg(test)]
    pub(crate) fn configure_test_writer(&mut self, writer: impl Write + Send + 'static) {
        self.sink = Some(LoggerSink::from_writer(writer));
    }
}

#[derive(Clone)]
struct LoggerSink {
    writer: Arc<Mutex<LineWriter<Box<dyn Write + Send>>>>,
}

impl fmt::Debug for LoggerSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoggerSink").finish_non_exhaustive()
    }
}

impl LoggerSink {
    fn open(path: &Path) -> Result<Self, LoggerConfigError> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|err| LoggerConfigError::OpenFile(err.kind()))?;

        Ok(Self::from_writer(file))
    }

    fn from_writer(writer: impl Write + Send + 'static) -> Self {
        Self {
            writer: Arc::new(Mutex::new(LineWriter::new(Box::new(writer)))),
        }
    }

    fn write_action(
        &self,
        show_level: bool,
        show_log_origin: bool,
        origin: &Location<'_>,
        level: LoggerLevel,
        action: &str,
    ) -> Result<(), LoggerDeliveryError> {
        self.write_message(
            show_level,
            show_log_origin,
            origin,
            level,
            format_args!("action={action}"),
        )
    }

    fn write_api_request(
        &self,
        show_level: bool,
        show_log_origin: bool,
        origin: &Location<'_>,
        level: LoggerLevel,
        method: &str,
        path: impl fmt::Display,
    ) -> Result<(), LoggerDeliveryError> {
        self.write_message(
            show_level,
            show_log_origin,
            origin,
            level,
            format_args!("The API server received a {method} request on \"{path}\"."),
        )
    }

    fn write_boot_timer(
        &self,
        show_level: bool,
        show_log_origin: bool,
        origin: &Location<'_>,
        level: LoggerLevel,
        wall_time_us: u64,
        cpu_time_us: u64,
    ) -> Result<(), LoggerDeliveryError> {
        let wall_time_ms = wall_time_us / 1_000;
        let cpu_time_ms = cpu_time_us / 1_000;
        self.write_message(
            show_level,
            show_log_origin,
            origin,
            level,
            format_args!(
            "Guest-boot-time = {wall_time_us:>6} us {wall_time_ms} ms, {cpu_time_us:>6} CPU us {cpu_time_ms} CPU ms"
            ),
        )
    }

    fn write_rate_limit_recovery(
        &self,
        show_level: bool,
        show_log_origin: bool,
        origin: &Location<'_>,
        level: LoggerLevel,
        suppressed: u64,
    ) -> Result<(), LoggerDeliveryError> {
        self.write_message(
            show_level,
            show_log_origin,
            origin,
            level,
            format_args!("{suppressed} messages were suppressed due to rate limiting"),
        )
    }

    fn write_message(
        &self,
        show_level: bool,
        show_log_origin: bool,
        origin: &Location<'_>,
        level: LoggerLevel,
        message: fmt::Arguments<'_>,
    ) -> Result<(), LoggerDeliveryError> {
        let mut writer = match self.writer.try_lock() {
            Ok(writer) => writer,
            Err(TryLockError::WouldBlock) => return Err(LoggerDeliveryError::LockContended),
            Err(TryLockError::Poisoned(_)) => return Err(LoggerDeliveryError::LockPoisoned),
        };

        match (show_level, show_log_origin) {
            (true, true) => writeln!(
                writer,
                "level={} origin={}:{} {message}",
                level.as_str(),
                origin.file(),
                origin.line()
            ),
            (true, false) => writeln!(writer, "level={} {message}", level.as_str()),
            (false, true) => writeln!(
                writer,
                "origin={}:{} {message}",
                origin.file(),
                origin.line()
            ),
            (false, false) => writeln!(writer, "{message}"),
        }
        .map_err(|_| LoggerDeliveryError::Write)?;
        writer.flush().map_err(|_| LoggerDeliveryError::Write)
    }
}

fn module_filter_allows(filter: Option<&str>, module_path: &str) -> bool {
    filter.is_none_or(|filter| module_path.starts_with(filter))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Error, ErrorKind, Write};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        API_REQUEST_LOG_MODULE, BOOT_TIMER_LOG_MODULE, BootTimerLogRateLimiter,
        LogRateLimitDecision, LogRateLimiterClock, LoggerConfigError, LoggerConfigInput,
        LoggerLevel, LoggerState, MINIMAL_ACTION_LOG_MODULE, SharedLoggerMetrics,
    };

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

    #[derive(Debug)]
    struct FlushFailingWriter;

    impl Write for FlushFailingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(Error::from(ErrorKind::BrokenPipe))
        }
    }

    #[derive(Debug, Clone, Default)]
    struct TestLogRateLimiterClock {
        now_ms: Arc<AtomicU64>,
    }

    impl TestLogRateLimiterClock {
        fn advance_ms(&self, elapsed_ms: u64) {
            self.now_ms.fetch_add(elapsed_ms, Ordering::Relaxed);
        }

        fn set_ms(&self, now_ms: u64) {
            self.now_ms.store(now_ms, Ordering::Relaxed);
        }
    }

    impl LogRateLimiterClock for TestLogRateLimiterClock {
        fn now_ms(&self) -> u64 {
            self.now_ms.load(Ordering::Relaxed)
        }
    }

    #[derive(Debug)]
    struct PanickingDisplay;

    impl std::fmt::Display for PanickingDisplay {
        fn fmt(&self, _formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("suppressed logger output should not format the API request path");
        }
    }

    #[test]
    fn shared_logger_metrics_saturate_at_u64_max() {
        let metrics = SharedLoggerMetrics::default();
        metrics
            .inner
            .missed_log_count
            .store(u64::MAX - 1, Ordering::Relaxed);
        metrics
            .inner
            .rate_limited_log_count
            .store(u64::MAX - 1, Ordering::Relaxed);

        metrics.record_missed_log();
        metrics.record_missed_log();
        metrics.record_rate_limited_log();
        metrics.record_rate_limited_log();

        assert_eq!(metrics.missed_log_count(), u64::MAX);
        assert_eq!(metrics.rate_limited_log_count(), u64::MAX);
    }

    fn assert_action_output_with_origin(output: &str, level: Option<LoggerLevel>, action: &str) {
        let mut lines = output.lines();
        let line = lines
            .next()
            .expect("logger output should include action line");
        assert_eq!(lines.next(), None);

        let prefix = level.map_or_else(
            || "origin=".to_string(),
            |level| format!("level={} origin=", level.as_str()),
        );
        assert!(line.starts_with(&prefix));

        let suffix = format!(" action={action}");
        assert!(line.ends_with(&suffix));

        let origin = line
            .strip_prefix(&prefix)
            .expect("logger output should include origin prefix")
            .strip_suffix(&suffix)
            .expect("logger output should include action suffix");
        let (file, line_number) = origin
            .rsplit_once(':')
            .expect("logger origin should include file and line");

        assert!(file.ends_with(file!()), "unexpected origin file: {file}");
        assert!(
            line_number.parse::<u32>().is_ok(),
            "unexpected origin line: {line_number}"
        );
    }

    #[test]
    fn parses_logger_levels() {
        for (input, expected) in [
            ("off", LoggerLevel::Off),
            ("TRACE", LoggerLevel::Trace),
            ("Debug", LoggerLevel::Debug),
            ("info", LoggerLevel::Info),
            ("warning", LoggerLevel::Warn),
            ("Warn", LoggerLevel::Warn),
            ("ERROR", LoggerLevel::Error),
        ] {
            assert_eq!(input.parse::<LoggerLevel>(), Ok(expected));
        }
        assert!("verbose".parse::<LoggerLevel>().is_err());
    }

    #[test]
    fn validates_firecracker_shaped_logger_config() {
        let config = LoggerConfigInput::new()
            .with_log_path("/tmp/logger")
            .with_level(LoggerLevel::Warn)
            .with_show_level(true)
            .with_show_log_origin(true)
            .with_module("api_server")
            .validate()
            .expect("logger config should validate");

        assert_eq!(
            config.log_path(),
            Some(PathBuf::from("/tmp/logger").as_path())
        );
        assert_eq!(config.level(), Some(LoggerLevel::Warn));
        assert_eq!(config.show_level(), Some(true));
        assert_eq!(config.show_log_origin(), Some(true));
        assert_eq!(config.module(), Some("api_server"));
    }

    #[test]
    fn rejects_empty_log_path() {
        assert_eq!(
            LoggerConfigInput::new()
                .with_log_path(PathBuf::new())
                .validate(),
            Err(LoggerConfigError::EmptyPath)
        );
    }

    #[test]
    fn default_state_matches_firecracker_defaults() {
        let state = LoggerState::default();

        assert!(!state.is_configured());
        assert_eq!(state.level(), LoggerLevel::Info);
        assert!(!state.show_level());
        assert!(!state.show_log_origin());
        assert_eq!(state.module(), None);
    }

    #[test]
    fn configures_output_and_updates_fields() {
        let path = unique_logger_path("configured");
        let mut state = LoggerState::default();

        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn)
                    .with_show_level(true)
                    .with_show_log_origin(true)
                    .with_module("bangbang"),
            )
            .expect("logger should configure");

        assert!(state.is_configured());
        assert!(path.exists());
        assert_eq!(state.level(), LoggerLevel::Warn);
        assert!(state.show_level());
        assert!(state.show_log_origin());
        assert_eq!(state.module(), Some("bangbang"));

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_without_configuration_is_noop() {
        let state = LoggerState::default();

        assert!(!state.log_action("InstanceStart"));
        assert!(!state.is_configured());
    }

    #[test]
    fn log_action_writes_minimal_action_lines() {
        let path = unique_logger_path("actions");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");

        assert!(state.log_action("InstanceStart"));
        assert!(state.log_action("FlushMetrics"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "action=InstanceStart\naction=FlushMetrics\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_api_request_writes_firecracker_shaped_line() {
        let path = unique_logger_path("api-request");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");

        assert!(state.log_api_request("Put", "/mmds"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "The API server received a Put request on \"/mmds\".\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_api_request_respects_level_filter() {
        let path = unique_logger_path("filtered-api-request");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn),
            )
            .expect("logger should configure");

        assert!(!state.log_api_request("Get", "/version"));
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            ""
        );

        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Info))
            .expect("logger should update level");
        assert!(state.log_api_request("Get", "/version"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "The API server received a Get request on \"/version\".\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_api_request_respects_module_filter() {
        let path = unique_logger_path("api-request-module-filter");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_module(MINIMAL_ACTION_LOG_MODULE),
            )
            .expect("logger should configure");

        assert!(!state.log_api_request("Get", "/version"));

        state
            .configure(LoggerConfigInput::new().with_module(API_REQUEST_LOG_MODULE))
            .expect("logger should update module filter");
        assert!(state.log_api_request("Get", "/version"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "The API server received a Get request on \"/version\".\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_api_request_does_not_format_path_when_output_is_suppressed() {
        let path = unique_logger_path("api-request-suppressed");
        let mut state = LoggerState::default();

        assert!(!state.log_api_request("Get", PanickingDisplay));

        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn),
            )
            .expect("logger should configure");
        assert!(!state.log_api_request("Get", PanickingDisplay));

        state
            .configure(
                LoggerConfigInput::new()
                    .with_level(LoggerLevel::Info)
                    .with_module(MINIMAL_ACTION_LOG_MODULE),
            )
            .expect("logger should update filters");
        assert!(!state.log_api_request("Get", PanickingDisplay));

        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            ""
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_api_request_records_write_failure_without_failing_caller() {
        let mut state = LoggerState::default();
        state.configure_test_writer(FailingWriter);

        assert!(!state.log_api_request("Patch", "/mmds"));
        assert_eq!(state.metrics.missed_log_count(), 1);
    }

    #[test]
    fn log_boot_timer_writes_firecracker_shaped_line() {
        let path = unique_logger_path("boot-timer");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");

        assert!(state.log_boot_timer(7_123, 1_456));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "Guest-boot-time =   7123 us 7 ms,   1456 CPU us 1 CPU ms\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn boot_timer_logger_snapshot_shares_sink_with_action_logs() {
        let path = unique_logger_path("boot-timer-shared");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");
        let boot_timer_logger = state.boot_timer_logger();

        assert!(state.log_action("InstanceStart"));
        assert!(boot_timer_logger.log_boot_time(1_000, 200));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "action=InstanceStart\nGuest-boot-time =   1000 us 1 ms,    200 CPU us 0 CPU ms\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn boot_timer_logger_records_missed_log_on_write_failure() {
        let mut state = LoggerState::default();
        state.configure_test_writer(FailingWriter);
        let boot_timer_logger = state.boot_timer_logger();

        assert!(!boot_timer_logger.log_boot_time(1_000, 200));
        assert_eq!(state.metrics.missed_log_count(), 1);
    }

    #[test]
    fn boot_timer_logger_does_not_record_missed_log_for_success_or_no_output() {
        let path = unique_logger_path("boot-timer-no-miss");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");

        assert!(state.boot_timer_logger().log_boot_time(1_000, 200));
        assert_eq!(state.metrics.missed_log_count(), 0);

        let unconfigured_state = LoggerState::default();
        assert!(
            !unconfigured_state
                .boot_timer_logger()
                .log_boot_time(1_000, 200)
        );
        assert_eq!(unconfigured_state.metrics.missed_log_count(), 0);

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn boot_timer_logger_does_not_record_missed_log_for_filtered_output() {
        let mut state = LoggerState::default();
        state.configure_test_writer(FailingWriter);
        state
            .configure(LoggerConfigInput::new().with_module(MINIMAL_ACTION_LOG_MODULE))
            .expect("logger should update module filter");
        let boot_timer_logger = state.boot_timer_logger();

        assert!(!boot_timer_logger.log_boot_time(1_000, 200));
        assert_eq!(state.metrics.missed_log_count(), 0);
        assert_eq!(state.metrics.rate_limited_log_count(), 0);
    }

    #[test]
    fn log_action_includes_level_when_configured() {
        let path = unique_logger_path("actions-with-level");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_show_level(true),
            )
            .expect("logger should configure");

        assert!(state.log_action("InstanceStart"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "level=Info action=InstanceStart\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_includes_origin_when_configured() {
        let path = unique_logger_path("actions-with-origin");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_show_log_origin(true),
            )
            .expect("logger should configure");

        assert!(state.log_action("InstanceStart"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_action_output_with_origin(&output, None, "InstanceStart");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_includes_level_and_origin_when_configured() {
        let path = unique_logger_path("actions-with-level-origin");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_show_level(true)
                    .with_show_log_origin(true),
            )
            .expect("logger should configure");

        assert!(state.log_action("FlushMetrics"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_action_output_with_origin(&output, Some(LoggerLevel::Info), "FlushMetrics");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_respects_level_filter_and_reconfiguration() {
        let path = unique_logger_path("filtered-actions");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn),
            )
            .expect("logger should configure");

        assert!(!state.log_action("InstanceStart"));
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            ""
        );

        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Debug))
            .expect("logger should update level without replacing sink");
        assert!(state.log_action("FlushMetrics"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "action=FlushMetrics\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_respects_module_filter_and_reconfiguration() {
        let path = unique_logger_path("module-filtered-actions");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_module("bangbang_runtime"),
            )
            .expect("logger should configure");

        assert!(state.log_action("InstanceStart"));

        state
            .configure(LoggerConfigInput::new().with_module(MINIMAL_ACTION_LOG_MODULE))
            .expect("logger should update module filter");
        assert!(state.log_action("FlushMetrics"));

        state
            .configure(LoggerConfigInput::new().with_module("api_server"))
            .expect("logger should update module filter");
        assert!(!state.log_action("Suppressed"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "action=InstanceStart\naction=FlushMetrics\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_boot_timer_respects_module_filter() {
        let path = unique_logger_path("boot-timer-module-filter");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_module(MINIMAL_ACTION_LOG_MODULE),
            )
            .expect("logger should configure");

        assert!(!state.log_boot_timer(1, 1));

        state
            .configure(LoggerConfigInput::new().with_module(BOOT_TIMER_LOG_MODULE))
            .expect("logger should update module filter");
        assert!(state.log_boot_timer(1, 1));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "Guest-boot-time =      1 us 0 ms,      1 CPU us 0 CPU ms\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_treats_empty_module_filter_as_match_all() {
        let path = unique_logger_path("empty-module-filter");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_module(""),
            )
            .expect("logger should configure");

        assert!(state.log_action("InstanceStart"));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "action=InstanceStart\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_records_write_failure_without_failing_caller() {
        let mut state = LoggerState::default();
        state.configure_test_writer(FailingWriter);

        assert!(!state.log_action("InstanceStart"));
        assert_eq!(state.metrics.missed_log_count(), 1);
    }

    #[test]
    fn log_action_records_flush_failure_without_failing_caller() {
        let mut state = LoggerState::default();
        state.configure_test_writer(FlushFailingWriter);

        assert!(!state.log_action("InstanceStart"));
        assert_eq!(state.metrics.missed_log_count(), 1);
    }

    #[test]
    fn log_action_drops_contended_sink_without_blocking() {
        let mut state = LoggerState::default();
        state.configure_test_writer(std::io::sink());
        let sink = state.sink.as_ref().expect("sink should exist").clone();
        let guard = sink.writer.lock().expect("sink lock should be available");

        assert!(!state.log_action("InstanceStart"));
        assert_eq!(state.metrics.missed_log_count(), 1);

        drop(guard);
    }

    #[test]
    fn log_action_drops_poisoned_sink_without_panicking() {
        let mut state = LoggerState::default();
        state.configure_test_writer(std::io::sink());
        let writer = state
            .sink
            .as_ref()
            .expect("sink should exist")
            .writer
            .clone();
        let poison_result = thread::spawn(move || {
            let _guard = writer.lock().expect("sink lock should be available");
            panic!("poison logger sink for test");
        })
        .join();
        assert!(poison_result.is_err());

        assert!(!state.log_action("InstanceStart"));
        assert_eq!(state.metrics.missed_log_count(), 1);
    }

    #[test]
    fn boot_timer_rate_limit_allows_burst_then_reports_recovery() {
        let path = unique_logger_path("boot-timer-rate-limit");
        let clock = TestLogRateLimiterClock::default();
        let mut state = LoggerState {
            boot_timer_rate_limiter: BootTimerLogRateLimiter::with_clock(Arc::new(clock.clone())),
            ..LoggerState::default()
        };
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_show_level(true),
            )
            .expect("logger should configure");
        let first_session_logger = state.boot_timer_logger();

        for _ in 0..10 {
            assert!(first_session_logger.log_boot_time(1_000, 200));
        }

        let reconstructed_session_logger = state.boot_timer_logger();
        assert!(!reconstructed_session_logger.log_boot_time(1_000, 200));
        assert_eq!(state.metrics.rate_limited_log_count(), 1);
        assert_eq!(state.metrics.missed_log_count(), 0);

        clock.advance_ms(500);
        assert!(reconstructed_session_logger.log_boot_time(2_000, 300));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 12);
        assert_eq!(
            lines[10],
            "level=Warn 1 messages were suppressed due to rate limiting"
        );
        assert_eq!(
            lines[11],
            "level=Info Guest-boot-time =   2000 us 2 ms,    300 CPU us 0 CPU ms"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn boot_timer_local_policy_does_not_consume_rate_limit_or_record_misses() {
        let path = unique_logger_path("boot-timer-local-policy");
        let clock = TestLogRateLimiterClock::default();
        let mut state = LoggerState {
            boot_timer_rate_limiter: BootTimerLogRateLimiter::with_clock(Arc::new(clock)),
            ..LoggerState::default()
        };

        for _ in 0..20 {
            assert!(!state.log_boot_timer(1_000, 200));
        }

        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn),
            )
            .expect("logger should configure");
        for _ in 0..20 {
            assert!(!state.log_boot_timer(1_000, 200));
        }

        state
            .configure(
                LoggerConfigInput::new()
                    .with_level(LoggerLevel::Info)
                    .with_module(MINIMAL_ACTION_LOG_MODULE),
            )
            .expect("logger should update filters");
        for _ in 0..20 {
            assert!(!state.log_boot_timer(1_000, 200));
        }

        state
            .configure(LoggerConfigInput::new().with_module(BOOT_TIMER_LOG_MODULE))
            .expect("logger should update module filter");
        for _ in 0..10 {
            assert!(state.log_boot_timer(1_000, 200));
        }
        assert!(!state.log_boot_timer(1_000, 200));

        assert_eq!(state.metrics.missed_log_count(), 0);
        assert_eq!(state.metrics.rate_limited_log_count(), 1);
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn boot_timer_rate_limit_suppressed_count_saturates() {
        let clock = TestLogRateLimiterClock::default();
        let limiter = BootTimerLogRateLimiter::with_clock(Arc::new(clock.clone()));
        {
            let mut state = limiter
                .inner
                .state
                .lock()
                .expect("rate limiter lock should be available");
            state.theoretical_arrival_time_ms = 5_000;
            state.suppressed = u64::MAX - 1;
        }

        assert_eq!(limiter.check(), LogRateLimitDecision::Denied);
        assert_eq!(limiter.check(), LogRateLimitDecision::Denied);
        clock.advance_ms(500);
        assert_eq!(
            limiter.check(),
            LogRateLimitDecision::Admitted {
                suppressed: u64::MAX
            }
        );
    }

    #[test]
    fn boot_timer_rate_limit_matches_refill_recovery_and_backwards_time_contract() {
        let clock = TestLogRateLimiterClock::default();
        clock.set_ms(1_000);
        let limiter = BootTimerLogRateLimiter::with_clock(Arc::new(clock.clone()));

        for _ in 0..10 {
            assert_eq!(
                limiter.check(),
                LogRateLimitDecision::Admitted { suppressed: 0 }
            );
        }
        for _ in 0..3 {
            assert_eq!(limiter.check(), LogRateLimitDecision::Denied);
        }

        clock.advance_ms(499);
        assert_eq!(limiter.check(), LogRateLimitDecision::Denied);
        clock.advance_ms(1);
        assert_eq!(
            limiter.check(),
            LogRateLimitDecision::Admitted { suppressed: 4 }
        );

        clock.advance_ms(5_000);
        for _ in 0..10 {
            assert_eq!(
                limiter.check(),
                LogRateLimitDecision::Admitted { suppressed: 0 }
            );
        }
        assert_eq!(limiter.check(), LogRateLimitDecision::Denied);

        clock.set_ms(6_000);
        assert_eq!(limiter.check(), LogRateLimitDecision::Denied);
        clock.set_ms(7_000);
        assert_eq!(
            limiter.check(),
            LogRateLimitDecision::Admitted { suppressed: 2 }
        );
    }

    #[test]
    fn boot_timer_recovery_and_original_delivery_failures_are_counted_independently() {
        let clock = TestLogRateLimiterClock::default();
        let mut state = LoggerState {
            boot_timer_rate_limiter: BootTimerLogRateLimiter::with_clock(Arc::new(clock.clone())),
            ..LoggerState::default()
        };
        state.configure_test_writer(std::io::sink());

        for _ in 0..10 {
            assert!(state.log_boot_timer(1_000, 200));
        }
        assert!(!state.log_boot_timer(1_000, 200));
        state.configure_test_writer(FailingWriter);
        clock.advance_ms(500);

        assert!(!state.log_boot_timer(2_000, 300));
        assert_eq!(state.metrics.missed_log_count(), 2);
        assert_eq!(state.metrics.rate_limited_log_count(), 1);
    }

    #[test]
    fn boot_timer_rate_limiters_are_independent_between_logger_states() {
        let mut first = LoggerState::default();
        first.configure_test_writer(std::io::sink());
        let mut second = LoggerState::default();
        second.configure_test_writer(std::io::sink());

        for _ in 0..10 {
            assert!(first.log_boot_timer(1_000, 200));
        }
        assert!(!first.log_boot_timer(1_000, 200));
        assert!(second.log_boot_timer(1_000, 200));
    }

    #[test]
    fn repeated_configuration_updates_without_requiring_log_path() {
        let path = unique_logger_path("repeat");
        let mut state = LoggerState::default();
        state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Warn),
            )
            .expect("initial logger should configure");

        state
            .configure(
                LoggerConfigInput::new()
                    .with_level(LoggerLevel::Debug)
                    .with_show_level(true)
                    .with_module("runtime"),
            )
            .expect("logger should update without a new path");

        assert!(state.is_configured());
        assert!(path.exists());
        assert_eq!(state.level(), LoggerLevel::Debug);
        assert!(state.show_level());
        assert!(!state.show_log_origin());
        assert_eq!(state.module(), Some("runtime"));

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn open_errors_do_not_mutate_existing_state_or_echo_path() {
        let missing_parent = unique_logger_path("parent").join("logger");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Warn))
            .expect("level-only logger update should succeed");

        let err = state
            .configure(
                LoggerConfigInput::new()
                    .with_log_path(&missing_parent)
                    .with_level(LoggerLevel::Debug),
            )
            .expect_err("missing parent should fail");
        let missing_parent_text = missing_parent.to_string_lossy();

        assert!(matches!(err, LoggerConfigError::OpenFile(_)));
        assert!(!err.to_string().contains(missing_parent_text.as_ref()));
        assert_eq!(state.level(), LoggerLevel::Warn);
        assert!(!state.is_configured());
    }
}
