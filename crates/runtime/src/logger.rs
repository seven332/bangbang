use std::fmt;
use std::fs::OpenOptions;
use std::io::{LineWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const BOOT_TIMER_LOG_MODULE: &str = "bangbang_runtime::boot_timer";
const MINIMAL_ACTION_LOG_MODULE: &str = "bangbang_runtime::vmm_action";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoggerWriteError {
    LockPoisoned,
    Write(std::io::ErrorKind),
}

impl fmt::Display for LoggerWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => f.write_str("logger output lock was poisoned"),
            Self::Write(kind) => write!(f, "failed to write logger output: {kind:?}"),
        }
    }
}

impl std::error::Error for LoggerWriteError {}

#[derive(Debug, Clone, Default)]
pub(crate) struct MissedLogCounter {
    count: Arc<AtomicU64>,
}

impl MissedLogCounter {
    pub(crate) fn record(&self) {
        let mut current = self.count.load(Ordering::Relaxed);
        while current != u64::MAX {
            let next = current.saturating_add(1);
            match self.count.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct BootTimerLogger {
    sink: Option<LoggerSink>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
    missed_log_counter: Option<MissedLogCounter>,
}

impl BootTimerLogger {
    pub(crate) fn with_missed_log_counter(mut self, counter: MissedLogCounter) -> Self {
        self.missed_log_counter = Some(counter);
        self
    }

    fn record_missed_log(&self) {
        if let Some(counter) = &self.missed_log_counter {
            counter.record();
        }
    }

    #[track_caller]
    pub fn log_boot_time(
        &self,
        wall_time_us: u64,
        cpu_time_us: u64,
    ) -> Result<bool, LoggerWriteError> {
        const BOOT_TIMER_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(BOOT_TIMER_LEVEL) {
            return Ok(false);
        }

        if !module_filter_allows(self.module.as_deref(), BOOT_TIMER_LOG_MODULE) {
            return Ok(false);
        }

        let Some(sink) = &self.sink else {
            return Ok(false);
        };

        if let Err(err) = sink.write_boot_timer(
            self.show_level,
            self.show_log_origin,
            Location::caller(),
            BOOT_TIMER_LEVEL,
            wall_time_us,
            cpu_time_us,
        ) {
            self.record_missed_log();
            return Err(err);
        }
        Ok(true)
    }
}

#[derive(Debug)]
pub struct LoggerState {
    sink: Option<LoggerSink>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module: Option<String>,
}

impl Default for LoggerState {
    fn default() -> Self {
        Self {
            sink: None,
            level: LoggerLevel::Info,
            show_level: false,
            show_log_origin: false,
            module: None,
        }
    }
}

impl LoggerState {
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
    pub(crate) fn log_action(&mut self, action: &str) -> Result<bool, LoggerWriteError> {
        const ACTION_LEVEL: LoggerLevel = LoggerLevel::Info;

        if !self.level.allows(ACTION_LEVEL) {
            return Ok(false);
        }

        if !module_filter_allows(self.module.as_deref(), MINIMAL_ACTION_LOG_MODULE) {
            return Ok(false);
        }

        let Some(sink) = &self.sink else {
            return Ok(false);
        };

        sink.write_action(
            self.show_level,
            self.show_log_origin,
            Location::caller(),
            ACTION_LEVEL,
            action,
        )?;
        Ok(true)
    }

    pub fn boot_timer_logger(&self) -> BootTimerLogger {
        BootTimerLogger {
            sink: self.sink.clone(),
            level: self.level,
            show_level: self.show_level,
            show_log_origin: self.show_log_origin,
            module: self.module.clone(),
            missed_log_counter: None,
        }
    }

    #[track_caller]
    pub fn log_boot_timer(
        &mut self,
        wall_time_us: u64,
        cpu_time_us: u64,
    ) -> Result<bool, LoggerWriteError> {
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
    ) -> Result<(), LoggerWriteError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| LoggerWriteError::LockPoisoned)?;
        match (show_level, show_log_origin) {
            (true, true) => writeln!(
                writer,
                "level={} origin={}:{} action={action}",
                level.as_str(),
                origin.file(),
                origin.line()
            ),
            (true, false) => writeln!(writer, "level={} action={action}", level.as_str()),
            (false, true) => writeln!(
                writer,
                "origin={}:{} action={action}",
                origin.file(),
                origin.line()
            ),
            (false, false) => writeln!(writer, "action={action}"),
        }
        .map_err(|err| LoggerWriteError::Write(err.kind()))?;
        writer
            .flush()
            .map_err(|err| LoggerWriteError::Write(err.kind()))
    }

    fn write_boot_timer(
        &self,
        show_level: bool,
        show_log_origin: bool,
        origin: &Location<'_>,
        level: LoggerLevel,
        wall_time_us: u64,
        cpu_time_us: u64,
    ) -> Result<(), LoggerWriteError> {
        let wall_time_ms = wall_time_us / 1_000;
        let cpu_time_ms = cpu_time_us / 1_000;
        let message = format!(
            "Guest-boot-time = {wall_time_us:>6} us {wall_time_ms} ms, {cpu_time_us:>6} CPU us {cpu_time_ms} CPU ms"
        );
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| LoggerWriteError::LockPoisoned)?;

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
        .map_err(|err| LoggerWriteError::Write(err.kind()))?;
        writer
            .flush()
            .map_err(|err| LoggerWriteError::Write(err.kind()))
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        BOOT_TIMER_LOG_MODULE, LoggerConfigError, LoggerConfigInput, LoggerLevel, LoggerSink,
        LoggerState, LoggerWriteError, MINIMAL_ACTION_LOG_MODULE, MissedLogCounter,
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
        let mut state = LoggerState::default();

        assert_eq!(state.log_action("InstanceStart"), Ok(false));
        assert!(!state.is_configured());
    }

    #[test]
    fn log_action_writes_minimal_action_lines() {
        let path = unique_logger_path("actions");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");

        assert_eq!(state.log_action("InstanceStart"), Ok(true));
        assert_eq!(state.log_action("FlushMetrics"), Ok(true));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "action=InstanceStart\naction=FlushMetrics\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_boot_timer_writes_firecracker_shaped_line() {
        let path = unique_logger_path("boot-timer");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");

        assert_eq!(state.log_boot_timer(7_123, 1_456), Ok(true));

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

        assert_eq!(state.log_action("InstanceStart"), Ok(true));
        assert_eq!(boot_timer_logger.log_boot_time(1_000, 200), Ok(true));

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
        let counter = MissedLogCounter::default();
        let boot_timer_logger = state
            .boot_timer_logger()
            .with_missed_log_counter(counter.clone());

        assert_eq!(
            boot_timer_logger.log_boot_time(1_000, 200),
            Err(LoggerWriteError::Write(ErrorKind::BrokenPipe))
        );
        assert_eq!(counter.count(), 1);
    }

    #[test]
    fn boot_timer_logger_does_not_record_missed_log_for_success_or_no_output() {
        let path = unique_logger_path("boot-timer-no-miss");
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger should configure");
        let counter = MissedLogCounter::default();

        assert_eq!(
            state
                .boot_timer_logger()
                .with_missed_log_counter(counter.clone())
                .log_boot_time(1_000, 200),
            Ok(true)
        );
        assert_eq!(counter.count(), 0);

        assert_eq!(
            LoggerState::default()
                .boot_timer_logger()
                .with_missed_log_counter(counter.clone())
                .log_boot_time(1_000, 200),
            Ok(false)
        );
        assert_eq!(counter.count(), 0);

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn boot_timer_logger_does_not_record_missed_log_for_filtered_output() {
        let mut state = LoggerState::default();
        state.configure_test_writer(FailingWriter);
        state
            .configure(LoggerConfigInput::new().with_module(MINIMAL_ACTION_LOG_MODULE))
            .expect("logger should update module filter");
        let counter = MissedLogCounter::default();
        let boot_timer_logger = state
            .boot_timer_logger()
            .with_missed_log_counter(counter.clone());

        assert_eq!(boot_timer_logger.log_boot_time(1_000, 200), Ok(false));
        assert_eq!(counter.count(), 0);
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

        assert_eq!(state.log_action("InstanceStart"), Ok(true));

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

        assert_eq!(state.log_action("InstanceStart"), Ok(true));

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

        assert_eq!(state.log_action("FlushMetrics"), Ok(true));

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

        assert_eq!(state.log_action("InstanceStart"), Ok(false));
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            ""
        );

        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Debug))
            .expect("logger should update level without replacing sink");
        assert_eq!(state.log_action("FlushMetrics"), Ok(true));

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

        assert_eq!(state.log_action("InstanceStart"), Ok(true));

        state
            .configure(LoggerConfigInput::new().with_module(MINIMAL_ACTION_LOG_MODULE))
            .expect("logger should update module filter");
        assert_eq!(state.log_action("FlushMetrics"), Ok(true));

        state
            .configure(LoggerConfigInput::new().with_module("api_server"))
            .expect("logger should update module filter");
        assert_eq!(state.log_action("Suppressed"), Ok(false));

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

        assert_eq!(state.log_boot_timer(1, 1), Ok(false));

        state
            .configure(LoggerConfigInput::new().with_module(BOOT_TIMER_LOG_MODULE))
            .expect("logger should update module filter");
        assert_eq!(state.log_boot_timer(1, 1), Ok(true));

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

        assert_eq!(state.log_action("InstanceStart"), Ok(true));

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "action=InstanceStart\n");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn log_action_reports_write_errors_without_path_details() {
        let mut state = LoggerState {
            sink: Some(LoggerSink::from_writer(FailingWriter)),
            level: LoggerLevel::Info,
            show_level: false,
            show_log_origin: false,
            module: None,
        };

        let err = state
            .log_action("InstanceStart")
            .expect_err("failing writer should report logger write error");

        assert_eq!(err, LoggerWriteError::Write(ErrorKind::BrokenPipe));
        assert_eq!(err.to_string(), "failed to write logger output: BrokenPipe");
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
