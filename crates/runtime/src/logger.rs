use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

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
}

#[derive(Debug)]
struct LoggerSink {
    _file: File,
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

        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{LoggerConfigError, LoggerConfigInput, LoggerLevel, LoggerState};

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
