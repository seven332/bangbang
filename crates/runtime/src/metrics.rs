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

    pub fn flush_with_diagnostics(
        &mut self,
        diagnostics: &MetricsDiagnostics,
    ) -> Result<bool, MetricsFlushError> {
        let Some(sink) = &mut self.sink else {
            return Ok(false);
        };
        let next_flush_count = self.flush_count.saturating_add(1);
        sink.write_minimal_metrics(next_flush_count, diagnostics)?;
        self.flush_count = next_flush_count;

        Ok(true)
    }

    #[cfg(test)]
    pub const fn is_configured(&self) -> bool {
        self.sink.is_some()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsDiagnostics {
    boot_run_loop_status: Option<BootRunLoopMetricStatus>,
}

impl MetricsDiagnostics {
    pub const fn new() -> Self {
        Self {
            boot_run_loop_status: None,
        }
    }

    pub const fn with_boot_run_loop_status(mut self, status: BootRunLoopMetricStatus) -> Self {
        self.boot_run_loop_status = Some(status);
        self
    }

    pub const fn boot_run_loop_status(&self) -> Option<BootRunLoopMetricStatus> {
        self.boot_run_loop_status
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
    writer: LineWriter<File>,
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

        Ok(Self {
            writer: LineWriter::new(file),
        })
    }

    fn write_minimal_metrics(
        &mut self,
        flush_count: u64,
        diagnostics: &MetricsDiagnostics,
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

        let mut root = serde_json::Map::new();
        root.insert("vmm".to_string(), serde_json::Value::Object(vmm));

        writeln!(self.writer, "{}", serde_json::Value::Object(root))
            .map_err(|err| MetricsFlushError::Write(err.kind()))?;
        self.writer
            .flush()
            .map_err(|err| MetricsFlushError::Write(err.kind()))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        BootRunLoopMetricStatus, MetricsConfigError, MetricsConfigInput, MetricsDiagnostics,
        MetricsFlushError, MetricsState,
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
