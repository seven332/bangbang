use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Read;
use std::mem::MaybeUninit;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Instant;

mod api_server;
#[doc(hidden)]
#[cfg(target_os = "macos")]
pub mod host_network;
mod periodic_metrics;
mod vmm;

use api_server::{ApiServer, ApiServerError, config_vmm_action_from_api_request};
use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;
use bangbang_api::http::{RequestError, parse_request_with_limit};
use bangbang_hvf::HvfBackend;
use periodic_metrics::PeriodicMetricsScheduler;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::{SigId, low_level};
use vmm::{ProcessSessionExitDecision, ProcessVmm, VmmRequestHandler};

use bangbang_runtime::logger::{LoggerConfigInput, LoggerLevel};
use bangbang_runtime::metrics::{MetricsConfigInput, MetricsDiagnostics};
use bangbang_runtime::mmds::MmdsContentInput;
use bangbang_runtime::{VmmAction, VmmActionError};

const DEFAULT_API_SOCK_PATH: &str = "/tmp/bangbang.socket";
const DEFAULT_INSTANCE_ID: &str = "anonymous-instance";
const APP_NAME: &str = "bangbang";
const CONFIG_FILE_MAX_BYTES: usize = 1024 * 1024;
const METADATA_FILE_MAX_BYTES: usize = CONFIG_FILE_MAX_BYTES;
const MIN_INSTANCE_ID_LEN: usize = 1;
const MAX_INSTANCE_ID_LEN: usize = 64;
const UNSUPPORTED_FIRECRACKER_ARGS: &[&str] = &[
    "boot-timer",
    "describe-snapshot",
    "enable-pci",
    "no-seccomp",
    "seccomp-filter",
    "snapshot-version",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let exit_code = err.exit_code().into_exit_code();
            eprintln!("bangbang: {err}");
            exit_code
        }
    }
}

fn run() -> Result<(), ProcessError> {
    let args = parse_process_args(env::args_os().skip(1))?;

    match args.command {
        Command::Help => {
            print_help();
            return Ok(());
        }
        Command::Version => {
            println!("bangbang {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Command::Run(config) => {
            let config = *config;
            let effective_mmds_size_limit = config.effective_mmds_size_limit();
            let StartupConfig {
                api_sock,
                config_file,
                http_api_max_payload_size,
                id,
                logger_config,
                mmds_size_limit: _,
                metadata,
                metrics_config,
                no_api,
                startup_time,
            } = config;
            let process_metrics_diagnostics = startup_time
                .metrics_diagnostics()
                .map_err(ProcessError::StartupTime)?;

            println!("bangbang {}", env!("CARGO_PKG_VERSION"));
            println!(
                "hvf target supported: {}",
                HvfBackend::is_supported_target()
            );

            let mut vmm = ProcessVmm::new(
                id,
                env!("CARGO_PKG_VERSION"),
                APP_NAME,
                effective_mmds_size_limit,
            )
            .with_process_metrics_diagnostics(process_metrics_diagnostics);
            apply_startup_metrics_config(&mut vmm, metrics_config)?;
            apply_startup_logger_config(&mut vmm, logger_config)?;
            apply_startup_metadata(&mut vmm, metadata.as_deref())?;
            apply_startup_config_file(&mut vmm, config_file.as_deref())?;
            let mut shutdown_signal = ShutdownSignal::install()?;
            if no_api {
                println!("status: VM running without API");
                wait_for_no_api_shutdown(&mut shutdown_signal, &mut vmm)?;
                return Ok(());
            }

            let server =
                ApiServer::bind_with_max_payload_size(&api_sock, http_api_max_payload_size)
                    .map_err(ProcessError::ApiServer)?;
            println!("status: API server listening");
            let shutdown_wakeup = shutdown_signal.wakeup_reader();
            server
                .run_until(&mut vmm, shutdown_wakeup)
                .map_err(ProcessError::ApiServer)?;
        }
    }

    Ok(())
}

fn wait_for_no_api_shutdown(
    shutdown_signal: &mut ShutdownSignal,
    vmm: &mut impl VmmRequestHandler,
) -> Result<(), ProcessError> {
    wait_for_no_api_shutdown_with_periodic_metrics_scheduler(
        shutdown_signal,
        vmm,
        PeriodicMetricsScheduler::new(Instant::now()),
    )
}

fn wait_for_no_api_shutdown_with_periodic_metrics_scheduler(
    shutdown_signal: &mut ShutdownSignal,
    vmm: &mut impl VmmRequestHandler,
    mut metrics_scheduler: PeriodicMetricsScheduler,
) -> Result<(), ProcessError> {
    shutdown_signal.set_nonblocking()?;

    loop {
        let now = Instant::now();
        match wait_for_shutdown_or_process_exit(
            shutdown_signal.wakeup_fd(),
            vmm.process_exit_wakeup_fd(),
            Some(metrics_scheduler.poll_timeout_ms(now)),
        )? {
            ProcessWaitResult::Ready => {}
            ProcessWaitResult::TimedOut => {
                vmm.handle_periodic_metrics_flush()
                    .map_err(ProcessError::PeriodicMetricsFlush)?;
                metrics_scheduler.schedule_next(Instant::now());
                continue;
            }
        }
        if shutdown_signal.drain_wakeup()? {
            return Ok(());
        }
        vmm.drain_process_exit_wakeup()
            .map_err(ProcessError::ProcessExitNotification)?;
        match vmm.process_exit_status().decision() {
            ProcessSessionExitDecision::Continue => {}
            ProcessSessionExitDecision::ExitSuccessfully => return Ok(()),
            ProcessSessionExitDecision::ExitWithFailure => {
                return Err(ProcessError::ProcessSessionTerminal);
            }
        }
    }
}

fn apply_startup_config_file<S>(
    vmm: &mut ProcessVmm<S>,
    config_file: Option<&str>,
) -> Result<(), ProcessError>
where
    S: vmm::InstanceStartExecutor,
{
    let Some(config_file) = config_file else {
        return Ok(());
    };
    let actions = config_file_actions(config_file).map_err(ProcessError::ConfigFile)?;

    for action in actions {
        let flush_startup_metrics = matches!(action, VmmAction::PutMetrics(_));
        vmm.handle_action(action)
            .map_err(ConfigFileError::Apply)
            .map_err(ProcessError::ConfigFile)?;
        if flush_startup_metrics {
            vmm.flush_startup_metrics()
                .map(|_| ())
                .map_err(ConfigFileError::Apply)
                .map_err(ProcessError::ConfigFile)?;
        }
    }

    vmm.handle_action(VmmAction::InstanceStart)
        .map(|_| ())
        .map_err(ConfigFileError::Apply)
        .map_err(ProcessError::ConfigFile)
}

fn config_file_actions(config_file: &str) -> Result<Vec<VmmAction>, ConfigFileError> {
    let contents = read_limited_regular_utf8_file(config_file, CONFIG_FILE_MAX_BYTES).map_err(
        |err| match err {
            StartupFileReadError::Read(kind) => ConfigFileError::Read(kind),
            StartupFileReadError::NotRegular => ConfigFileError::NotRegular,
            StartupFileReadError::TooLarge => ConfigFileError::TooLarge,
        },
    )?;
    config_file_actions_from_str(&contents)
}

fn config_file_actions_from_str(contents: &str) -> Result<Vec<VmmAction>, ConfigFileError> {
    let value = parse_config_file_json_value(contents)?;
    let object = value.as_object().ok_or(ConfigFileError::Malformed)?;

    validate_config_file_sections(object)?;

    let mut requests = Vec::new();
    if let Some(machine_config) = object.get("machine-config") {
        requests.push(config_section_request(
            "machine-config",
            "PUT",
            "/machine-config".to_string(),
            machine_config,
        )?);
    }

    let boot_source = object
        .get("boot-source")
        .ok_or(ConfigFileError::MissingSection("boot-source"))?;
    requests.push(config_section_request(
        "boot-source",
        "PUT",
        "/boot-source".to_string(),
        boot_source,
    )?);

    if let Some(drives) = object.get("drives") {
        for drive in config_section_array("drives", drives)? {
            let drive_id = config_section_string_field("drives", drive, "drive_id")?;
            requests.push(config_section_request(
                "drives",
                "PUT",
                format!("/drives/{drive_id}"),
                drive,
            )?);
        }
    }

    if let Some(network_interfaces) = object.get("network-interfaces") {
        for network_interface in config_section_array("network-interfaces", network_interfaces)? {
            let iface_id =
                config_section_string_field("network-interfaces", network_interface, "iface_id")?;
            requests.push(config_section_request(
                "network-interfaces",
                "PUT",
                format!("/network-interfaces/{iface_id}"),
                network_interface,
            )?);
        }
    }

    if let Some(mmds_config) = object.get("mmds-config") {
        requests.push(config_section_request(
            "mmds-config",
            "PUT",
            "/mmds/config".to_string(),
            mmds_config,
        )?);
    }

    if let Some(vsock) = object.get("vsock") {
        requests.push(config_section_request(
            "vsock",
            "PUT",
            "/vsock".to_string(),
            vsock,
        )?);
    }

    if let Some(entropy) = object.get("entropy") {
        requests.push(config_section_request(
            "entropy",
            "PUT",
            "/entropy".to_string(),
            entropy,
        )?);
    }

    if let Some(cpu_config) = object.get("cpu-config") {
        requests.push(config_section_request(
            "cpu-config",
            "PUT",
            "/cpu-config".to_string(),
            cpu_config,
        )?);
    }

    if let Some(metrics) = object.get("metrics") {
        requests.push(config_section_request(
            "metrics",
            "PUT",
            "/metrics".to_string(),
            metrics,
        )?);
    }

    if let Some(logger) = object.get("logger") {
        requests.push(config_section_request(
            "logger",
            "PUT",
            "/logger".to_string(),
            logger,
        )?);
    }

    if let Some(serial) = object.get("serial") {
        requests.push(config_section_request(
            "serial",
            "PUT",
            "/serial".to_string(),
            serial,
        )?);
    }

    requests
        .into_iter()
        .map(|(section, request)| {
            config_vmm_action_from_api_request(request)
                .ok_or(ConfigFileError::UnsupportedRequest { section })
        })
        .collect()
}

fn parse_config_file_json_value(contents: &str) -> Result<serde_json::Value, ConfigFileError> {
    let ConfigFileJsonValue(value) = serde_json::from_str::<ConfigFileJsonValue>(contents)
        .map_err(|_| ConfigFileError::Malformed)?;
    Ok(value)
}

#[derive(Debug)]
struct ConfigFileJsonValue(serde_json::Value);

impl<'de> serde::Deserialize<'de> for ConfigFileJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer
            .deserialize_any(ConfigFileJsonValueVisitor)
            .map(Self)
    }
}

#[derive(Debug)]
struct ConfigFileJsonValueVisitor;

impl<'de> Visitor<'de> for ConfigFileJsonValueVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(serde_json::Value::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::Deserialize::deserialize(deserializer).map(|ConfigFileJsonValue(value)| value)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));

        while let Some(ConfigFileJsonValue(value)) = sequence.next_element()? {
            values.push(value);
        }

        Ok(serde_json::Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();

        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("duplicate object key"));
            }

            let ConfigFileJsonValue(value) = map.next_value()?;
            object.insert(key, value);
        }

        Ok(serde_json::Value::Object(object))
    }
}

fn validate_config_file_sections(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigFileError> {
    for section in object.keys() {
        match section.as_str() {
            "boot-source" | "cpu-config" | "drives" | "logger" | "machine-config" | "metrics"
            | "mmds-config" | "network-interfaces" | "serial" | "vsock" | "entropy" => {}
            "balloon" | "memory-hotplug" | "pmem" => {
                return Err(ConfigFileError::UnsupportedSection(section.clone()));
            }
            _ => return Err(ConfigFileError::UnknownSection(section.clone())),
        }
    }

    Ok(())
}

fn config_section_array<'value>(
    section: &'static str,
    value: &'value serde_json::Value,
) -> Result<&'value [serde_json::Value], ConfigFileError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or(ConfigFileError::MalformedSection { section })
}

fn config_section_string_field<'value>(
    section: &'static str,
    value: &'value serde_json::Value,
    field: &'static str,
) -> Result<&'value str, ConfigFileError> {
    value
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(serde_json::Value::as_str)
        .ok_or(ConfigFileError::MalformedSection { section })
}

fn config_section_request(
    section: &'static str,
    method: &str,
    path: String,
    body: &serde_json::Value,
) -> Result<(&'static str, bangbang_api::http::ApiRequest), ConfigFileError> {
    let body =
        serde_json::to_vec(body).map_err(|_| ConfigFileError::MalformedSection { section })?;
    let header = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut request = header.into_bytes();
    request.extend_from_slice(&body);

    parse_request_with_limit(&request, usize::MAX)
        .map(|request| (section, request))
        .map_err(|source| ConfigFileError::Request { section, source })
}

fn apply_startup_metrics_config<S>(
    vmm: &mut ProcessVmm<S>,
    metrics_config: Option<MetricsConfigInput>,
) -> Result<(), ProcessError>
where
    S: vmm::InstanceStartExecutor,
{
    if let Some(metrics_config) = metrics_config {
        vmm.handle_action(VmmAction::PutMetrics(metrics_config))
            .map(|_| ())
            .map_err(ProcessError::StartupConfiguration)?;
        vmm.flush_startup_metrics()
            .map(|_| ())
            .map_err(ProcessError::StartupConfiguration)?;
    }

    Ok(())
}

fn apply_startup_logger_config<S>(
    vmm: &mut ProcessVmm<S>,
    logger_config: Option<LoggerConfigInput>,
) -> Result<(), ProcessError>
where
    S: vmm::InstanceStartExecutor,
{
    if let Some(logger_config) = logger_config {
        vmm.handle_action(VmmAction::PutLogger(logger_config))
            .map(|_| ())
            .map_err(ProcessError::StartupConfiguration)?;
    }

    Ok(())
}

fn apply_startup_metadata<S>(
    vmm: &mut ProcessVmm<S>,
    metadata: Option<&str>,
) -> Result<(), ProcessError>
where
    S: vmm::InstanceStartExecutor,
{
    let Some(metadata) = metadata else {
        return Ok(());
    };
    let input = metadata_content_input(metadata).map_err(ProcessError::Metadata)?;

    vmm.handle_action(VmmAction::PutMmds(input))
        .map(|_| ())
        .map_err(MetadataFileError::Apply)
        .map_err(ProcessError::Metadata)
}

fn metadata_content_input(metadata_file: &str) -> Result<MmdsContentInput, MetadataFileError> {
    let contents =
        read_limited_regular_utf8_file(metadata_file, METADATA_FILE_MAX_BYTES).map_err(|err| {
            match err {
                StartupFileReadError::Read(kind) => MetadataFileError::Read(kind),
                StartupFileReadError::NotRegular => MetadataFileError::NotRegular,
                StartupFileReadError::TooLarge => MetadataFileError::TooLarge,
            }
        })?;
    let value = serde_json::from_str::<serde_json::Value>(&contents)
        .map_err(|_| MetadataFileError::Malformed)?;

    Ok(MmdsContentInput::new(value))
}

fn read_limited_regular_utf8_file(
    path: &str,
    max_bytes: usize,
) -> Result<String, StartupFileReadError> {
    let max_bytes_u64 = max_bytes as u64;

    // Keep special files such as FIFOs from hanging startup before file-type validation.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| StartupFileReadError::Read(err.kind()))?;
    let metadata = file
        .metadata()
        .map_err(|err| StartupFileReadError::Read(err.kind()))?;
    if !metadata.file_type().is_file() {
        return Err(StartupFileReadError::NotRegular);
    }
    if metadata.len() > max_bytes_u64 {
        return Err(StartupFileReadError::TooLarge);
    }

    // Re-check through a capped reader in case the file grows after metadata validation.
    let mut contents = Vec::new();
    file.take(max_bytes_u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|err| StartupFileReadError::Read(err.kind()))?;
    if contents.len() > max_bytes {
        return Err(StartupFileReadError::TooLarge);
    }

    String::from_utf8(contents)
        .map_err(|_| StartupFileReadError::Read(std::io::ErrorKind::InvalidData))
}

fn parse_process_args<I>(args: I) -> Result<Args, ProcessError>
where
    I: IntoIterator<Item = OsString>,
{
    Args::parse_os(args).map_err(ProcessError::ArgumentParsing)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessExitCode {
    ProcessFailure = 1,
    ArgumentParsing = 153,
}

impl ProcessExitCode {
    const fn value(self) -> u8 {
        self as u8
    }

    fn into_exit_code(self) -> ExitCode {
        ExitCode::from(self.value())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessError {
    ApiServer(ApiServerError),
    ArgumentParsing(String),
    ConfigFile(ConfigFileError),
    Metadata(MetadataFileError),
    PeriodicMetricsFlush(VmmActionError),
    ProcessExitNotification(std::io::ErrorKind),
    ProcessSessionTerminal,
    SignalHandler(std::io::ErrorKind),
    StartupConfiguration(VmmActionError),
    StartupTime(StartupTimeClockError),
}

impl ProcessError {
    fn exit_code(&self) -> ProcessExitCode {
        match self {
            Self::ApiServer(_) => ProcessExitCode::ProcessFailure,
            Self::ArgumentParsing(_) => ProcessExitCode::ArgumentParsing,
            Self::ConfigFile(_) => ProcessExitCode::ProcessFailure,
            Self::Metadata(_) => ProcessExitCode::ProcessFailure,
            Self::PeriodicMetricsFlush(_) => ProcessExitCode::ProcessFailure,
            Self::ProcessExitNotification(_) => ProcessExitCode::ProcessFailure,
            Self::ProcessSessionTerminal => ProcessExitCode::ProcessFailure,
            Self::SignalHandler(_) => ProcessExitCode::ProcessFailure,
            Self::StartupConfiguration(_) => ProcessExitCode::ProcessFailure,
            Self::StartupTime(_) => ProcessExitCode::ProcessFailure,
        }
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiServer(err) => write!(f, "API server error: {err}"),
            Self::ArgumentParsing(message) => f.write_str(message),
            Self::ConfigFile(err) => write!(f, "config-file error: {err}"),
            Self::Metadata(err) => write!(f, "metadata error: {err}"),
            Self::PeriodicMetricsFlush(err) => {
                write!(f, "failed to flush periodic metrics: {err}")
            }
            Self::ProcessExitNotification(kind) => {
                write!(f, "process exit notification failed: {kind:?}")
            }
            Self::ProcessSessionTerminal => {
                f.write_str("process-owned boot run loop exited with failure")
            }
            Self::SignalHandler(kind) => {
                write!(f, "shutdown signal handling failed: {kind:?}")
            }
            Self::StartupConfiguration(err) => {
                write!(f, "startup configuration error: {err}")
            }
            Self::StartupTime(err) => write!(f, "startup time error: {err}"),
        }
    }
}

impl std::error::Error for ProcessError {}

#[derive(Debug, PartialEq, Eq)]
enum StartupFileReadError {
    Read(std::io::ErrorKind),
    NotRegular,
    TooLarge,
}

#[derive(Debug, PartialEq, Eq)]
enum ConfigFileError {
    Read(std::io::ErrorKind),
    NotRegular,
    TooLarge,
    Malformed,
    MissingSection(&'static str),
    UnknownSection(String),
    UnsupportedSection(String),
    MalformedSection {
        section: &'static str,
    },
    Request {
        section: &'static str,
        source: RequestError,
    },
    UnsupportedRequest {
        section: &'static str,
    },
    Apply(VmmActionError),
}

impl fmt::Display for ConfigFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(kind) => write!(f, "failed to read config file: {kind:?}"),
            Self::NotRegular => f.write_str("config file must be a regular file"),
            Self::TooLarge => write!(
                f,
                "config file exceeds {CONFIG_FILE_MAX_BYTES} byte size limit"
            ),
            Self::Malformed => f.write_str("malformed config file"),
            Self::MissingSection(section) => {
                write!(f, "config file is missing required section: {section}")
            }
            Self::UnknownSection(section) => write!(f, "unknown config-file section: {section}"),
            Self::UnsupportedSection(section) => {
                write!(f, "unsupported config-file section: {section}")
            }
            Self::MalformedSection { section } => {
                write!(f, "malformed config-file section: {section}")
            }
            Self::Request { section, source } => write!(
                f,
                "invalid config-file section {section}: {}",
                source.fault_message()
            ),
            Self::UnsupportedRequest { section } => {
                write!(f, "unsupported config-file request in section: {section}")
            }
            Self::Apply(err) => write!(f, "failed to apply config-file action: {err}"),
        }
    }
}

impl std::error::Error for ConfigFileError {}

#[derive(Debug, PartialEq, Eq)]
enum MetadataFileError {
    Read(std::io::ErrorKind),
    NotRegular,
    TooLarge,
    Malformed,
    Apply(VmmActionError),
}

impl fmt::Display for MetadataFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(kind) => write!(f, "failed to read metadata file: {kind:?}"),
            Self::NotRegular => f.write_str("metadata file must be a regular file"),
            Self::TooLarge => write!(
                f,
                "metadata file exceeds {METADATA_FILE_MAX_BYTES} byte size limit"
            ),
            Self::Malformed => f.write_str("malformed metadata file"),
            Self::Apply(err) => write!(f, "failed to apply metadata: {err}"),
        }
    }
}

impl std::error::Error for MetadataFileError {}

#[derive(Debug)]
struct ShutdownSignal {
    wakeup_reader: UnixStream,
    signal_ids: [SigId; 2],
}

impl ShutdownSignal {
    fn install() -> Result<Self, ProcessError> {
        let (wakeup_reader, wakeup_writer) =
            UnixStream::pair().map_err(|err| ProcessError::SignalHandler(err.kind()))?;
        let sigint = register_signal_wakeup(SIGINT, &wakeup_writer)?;
        let sigterm = match register_signal_wakeup(SIGTERM, &wakeup_writer) {
            Ok(sigterm) => sigterm,
            Err(err) => {
                low_level::unregister(sigint);
                return Err(err);
            }
        };

        Ok(Self {
            wakeup_reader,
            signal_ids: [sigint, sigterm],
        })
    }

    fn wakeup_reader(&mut self) -> &mut UnixStream {
        &mut self.wakeup_reader
    }

    fn wakeup_fd(&self) -> RawFd {
        self.wakeup_reader.as_raw_fd()
    }

    fn set_nonblocking(&self) -> Result<(), ProcessError> {
        self.wakeup_reader
            .set_nonblocking(true)
            .map_err(|err| ProcessError::SignalHandler(err.kind()))
    }

    fn drain_wakeup(&mut self) -> Result<bool, ProcessError> {
        let mut drained = false;
        let mut buffer = [0; 64];

        loop {
            match self.wakeup_reader.read(&mut buffer) {
                Ok(0) => {
                    return Err(ProcessError::SignalHandler(
                        std::io::ErrorKind::UnexpectedEof,
                    ));
                }
                Ok(_) => drained = true,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(drained),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(ProcessError::SignalHandler(err.kind())),
            }
        }
    }
}

fn wait_for_shutdown_or_process_exit(
    shutdown_wakeup_fd: RawFd,
    process_exit_wakeup_fd: Option<RawFd>,
    timeout_ms: Option<i32>,
) -> Result<ProcessWaitResult, ProcessError> {
    let mut poll_fds = [
        libc::pollfd {
            fd: shutdown_wakeup_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: process_exit_wakeup_fd.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let poll_fd_count = if process_exit_wakeup_fd.is_some() {
        poll_fds.len()
    } else {
        poll_fds.len() - 1
    };
    let poll_fds = poll_fds
        .get_mut(..poll_fd_count)
        .ok_or(ProcessError::SignalHandler(
            std::io::ErrorKind::InvalidInput,
        ))?;

    loop {
        for poll_fd in poll_fds.iter_mut() {
            poll_fd.revents = 0;
        }

        // SAFETY: `poll_fds` points to initialized `pollfd` values and remains
        // valid for the duration of the call.
        let result = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as _,
                timeout_ms.unwrap_or(-1),
            )
        };
        if result > 0 {
            return Ok(ProcessWaitResult::Ready);
        }
        if result == 0 {
            return Ok(ProcessWaitResult::TimedOut);
        }

        let kind = std::io::Error::last_os_error().kind();
        if kind != std::io::ErrorKind::Interrupted {
            return Err(ProcessError::SignalHandler(kind));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessWaitResult {
    Ready,
    TimedOut,
}

fn register_signal_wakeup(signal: i32, wakeup_writer: &UnixStream) -> Result<SigId, ProcessError> {
    let wakeup_fd = wakeup_writer
        .try_clone()
        .map_err(|err| ProcessError::SignalHandler(err.kind()))?
        .into_raw_fd();

    match low_level::pipe::register_raw(signal, wakeup_fd) {
        Ok(signal_id) => Ok(signal_id),
        Err(err) => {
            // SAFETY: `wakeup_fd` came from `UnixStream::into_raw_fd` and has
            // not been handed to a registered signal action on this error path.
            let _ = unsafe { libc::close(wakeup_fd) };
            Err(ProcessError::SignalHandler(err.kind()))
        }
    }
}

impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        for signal_id in self.signal_ids {
            low_level::unregister(signal_id);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    command: Command,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Run(Box<StartupConfig>),
}

#[derive(Debug, PartialEq, Eq)]
struct StartupConfig {
    api_sock: String,
    config_file: Option<String>,
    http_api_max_payload_size: usize,
    id: String,
    logger_config: Option<LoggerConfigInput>,
    mmds_size_limit: Option<usize>,
    metadata: Option<String>,
    metrics_config: Option<MetricsConfigInput>,
    no_api: bool,
    startup_time: StartupTimeConfig,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StartupTimeConfig {
    start_time_us: Option<u64>,
    start_time_cpu_us: Option<u64>,
    parent_cpu_time_us: Option<u64>,
}

impl StartupTimeConfig {
    fn metrics_diagnostics(self) -> Result<MetricsDiagnostics, StartupTimeClockError> {
        if !self.needs_clock_sample() {
            return Ok(MetricsDiagnostics::new());
        }

        let clock = StartupTimeClock::sample_for(&self)?;
        Ok(self.metrics_diagnostics_at(clock))
    }

    fn metrics_diagnostics_at(self, clock: StartupTimeClock) -> MetricsDiagnostics {
        let mut diagnostics = MetricsDiagnostics::new();
        if let Some(start_time_us) = self.start_time_us {
            diagnostics = diagnostics
                .with_start_time_us(clock.monotonic_time_us.saturating_sub(start_time_us));
        }
        if let Some(start_time_cpu_us) = self.start_time_cpu_us {
            let process_startup_time_cpu_us = clock
                .process_cpu_time_us
                .saturating_sub(start_time_cpu_us)
                .saturating_add(self.parent_cpu_time_us.unwrap_or_default());
            diagnostics = diagnostics.with_start_time_cpu_us(process_startup_time_cpu_us);
        }

        diagnostics
    }

    fn needs_clock_sample(&self) -> bool {
        self.start_time_us.is_some() || self.start_time_cpu_us.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StartupTimeClock {
    monotonic_time_us: u64,
    process_cpu_time_us: u64,
}

impl StartupTimeClock {
    #[cfg(test)]
    const fn new(monotonic_time_us: u64, process_cpu_time_us: u64) -> Self {
        Self {
            monotonic_time_us,
            process_cpu_time_us,
        }
    }

    fn sample_for(config: &StartupTimeConfig) -> Result<Self, StartupTimeClockError> {
        let monotonic_time_us = if config.start_time_us.is_some() {
            clock_time_us(libc::CLOCK_MONOTONIC).map_err(StartupTimeClockError::Monotonic)?
        } else {
            0
        };
        let process_cpu_time_us = if config.start_time_cpu_us.is_some() {
            clock_time_us(libc::CLOCK_PROCESS_CPUTIME_ID)
                .map_err(StartupTimeClockError::ProcessCpu)?
        } else {
            0
        };

        Ok(Self {
            monotonic_time_us,
            process_cpu_time_us,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupTimeClockError {
    Monotonic(std::io::ErrorKind),
    ProcessCpu(std::io::ErrorKind),
}

impl fmt::Display for StartupTimeClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Monotonic(kind) => write!(f, "failed to read monotonic clock: {kind:?}"),
            Self::ProcessCpu(kind) => write!(f, "failed to read process CPU clock: {kind:?}"),
        }
    }
}

impl std::error::Error for StartupTimeClockError {}

fn clock_time_us(clock_id: libc::clockid_t) -> Result<u64, std::io::ErrorKind> {
    let mut time = MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: `clock_gettime` writes a valid `timespec` to the provided pointer
    // when it returns 0. The pointer is valid for writes and properly aligned.
    let result = unsafe { libc::clock_gettime(clock_id, time.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().kind());
    }
    // SAFETY: `clock_gettime` returned success, so the `timespec` was initialized.
    let time = unsafe { time.assume_init() };

    timespec_time_us(time)
}

fn timespec_time_us(time: libc::timespec) -> Result<u64, std::io::ErrorKind> {
    let seconds = u64::try_from(time.tv_sec).map_err(|_| std::io::ErrorKind::InvalidData)?;
    let nanoseconds = u64::try_from(time.tv_nsec).map_err(|_| std::io::ErrorKind::InvalidData)?;

    Ok(seconds
        .saturating_mul(1_000_000)
        .saturating_add(nanoseconds / 1_000))
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            api_sock: DEFAULT_API_SOCK_PATH.to_string(),
            config_file: None,
            http_api_max_payload_size: HTTP_MAX_PAYLOAD_SIZE,
            id: DEFAULT_INSTANCE_ID.to_string(),
            logger_config: None,
            mmds_size_limit: None,
            metadata: None,
            metrics_config: None,
            no_api: false,
            startup_time: StartupTimeConfig::default(),
        }
    }
}

impl StartupConfig {
    fn effective_mmds_size_limit(&self) -> usize {
        self.mmds_size_limit
            .unwrap_or(self.http_api_max_payload_size)
    }
}

impl Args {
    fn parse_os<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = OsString>,
    {
        let args = args.into_iter().collect::<Vec<_>>();

        if args.iter().any(|arg| arg == "--help" || arg == "-h") {
            return Ok(Self {
                command: Command::Help,
            });
        }

        if args.iter().any(|arg| arg == "--version" || arg == "-V") {
            return Ok(Self {
                command: Command::Version,
            });
        }

        let args = args
            .into_iter()
            .map(os_arg_into_string)
            .collect::<Result<Vec<_>, _>>()?;

        Self::parse(args)
    }

    fn parse<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
    {
        let args = args.into_iter().collect::<Vec<_>>();

        if args.iter().any(|arg| arg == "--help" || arg == "-h") {
            return Ok(Self {
                command: Command::Help,
            });
        }

        if args.iter().any(|arg| arg == "--version" || arg == "-V") {
            return Ok(Self {
                command: Command::Version,
            });
        }

        let mut config = StartupConfig::default();
        let mut api_sock_seen = false;
        let mut config_file_seen = false;
        let mut http_api_max_payload_size_seen = false;
        let mut id_seen = false;
        let mut logger_config = LoggerConfigInput::new();
        let mut logger_config_seen = false;
        let mut log_path_seen = false;
        let mut level_seen = false;
        let mut mmds_size_limit_seen = false;
        let mut metadata_seen = false;
        let mut metrics_path_seen = false;
        let mut module_seen = false;
        let mut no_api_seen = false;
        let mut parent_cpu_time_us_seen = false;
        let mut show_level_seen = false;
        let mut show_log_origin_seen = false;
        let mut start_time_cpu_us_seen = false;
        let mut start_time_us_seen = false;
        let mut index = 0;

        while let Some(arg) = args.get(index) {
            match arg.as_str() {
                value_arg if is_value_arg(value_arg, "--api-sock") => {
                    if api_sock_seen {
                        return Err("duplicate argument: --api-sock".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--api-sock")?;
                    validate_api_sock(&value)?;
                    config.api_sock = value;
                    api_sock_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--config-file") => {
                    if config_file_seen {
                        return Err("duplicate argument: --config-file".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--config-file")?;
                    validate_config_file_path(&value)?;
                    config.config_file = Some(value);
                    config_file_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--http-api-max-payload-size") => {
                    if http_api_max_payload_size_seen {
                        return Err("duplicate argument: --http-api-max-payload-size".to_string());
                    }
                    let (value, consumed) =
                        take_value_arg(&args, index, "--http-api-max-payload-size")?;
                    config.http_api_max_payload_size = parse_http_api_max_payload_size(&value)?;
                    http_api_max_payload_size_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--id") => {
                    if id_seen {
                        return Err("duplicate argument: --id".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--id")?;
                    validate_instance_id(&value)?;
                    config.id = value;
                    id_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--log-path") => {
                    if log_path_seen {
                        return Err("duplicate argument: --log-path".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--log-path")?;
                    logger_config = logger_config.with_log_path(value);
                    logger_config_seen = true;
                    log_path_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--level") => {
                    if level_seen {
                        return Err("duplicate argument: --level".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--level")?;
                    let level = value
                        .parse::<LoggerLevel>()
                        .map_err(|err| format!("invalid --level: {err}"))?;
                    logger_config = logger_config.with_level(level);
                    logger_config_seen = true;
                    level_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--mmds-size-limit") => {
                    if mmds_size_limit_seen {
                        return Err("duplicate argument: --mmds-size-limit".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--mmds-size-limit")?;
                    config.mmds_size_limit = Some(parse_mmds_size_limit(&value)?);
                    mmds_size_limit_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--metadata") => {
                    if metadata_seen {
                        return Err("duplicate argument: --metadata".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--metadata")?;
                    validate_metadata_path(&value)?;
                    config.metadata = Some(value);
                    metadata_seen = true;
                    index += consumed;
                }
                "--no-api" => {
                    if no_api_seen {
                        return Err("duplicate argument: --no-api".to_string());
                    }
                    config.no_api = true;
                    no_api_seen = true;
                    index += 1;
                }
                value_arg if is_value_arg(value_arg, "--parent-cpu-time-us") => {
                    if parent_cpu_time_us_seen {
                        return Err("duplicate argument: --parent-cpu-time-us".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--parent-cpu-time-us")?;
                    config.startup_time.parent_cpu_time_us =
                        Some(parse_startup_time_us(&value, "parent-cpu-time-us")?);
                    parent_cpu_time_us_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--metrics-path") => {
                    if metrics_path_seen {
                        return Err("duplicate argument: --metrics-path".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--metrics-path")?;
                    config.metrics_config = Some(MetricsConfigInput::new(value));
                    metrics_path_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--module") => {
                    if module_seen {
                        return Err("duplicate argument: --module".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--module")?;
                    logger_config = logger_config.with_module(value);
                    logger_config_seen = true;
                    module_seen = true;
                    index += consumed;
                }
                "--show-level" => {
                    if show_level_seen {
                        return Err("duplicate argument: --show-level".to_string());
                    }
                    logger_config = logger_config.with_show_level(true);
                    logger_config_seen = true;
                    show_level_seen = true;
                    index += 1;
                }
                "--show-log-origin" => {
                    if show_log_origin_seen {
                        return Err("duplicate argument: --show-log-origin".to_string());
                    }
                    logger_config = logger_config.with_show_log_origin(true);
                    logger_config_seen = true;
                    show_log_origin_seen = true;
                    index += 1;
                }
                value_arg if is_value_arg(value_arg, "--start-time-cpu-us") => {
                    if start_time_cpu_us_seen {
                        return Err("duplicate argument: --start-time-cpu-us".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--start-time-cpu-us")?;
                    config.startup_time.start_time_cpu_us =
                        Some(parse_startup_time_us(&value, "start-time-cpu-us")?);
                    start_time_cpu_us_seen = true;
                    index += consumed;
                }
                value_arg if is_value_arg(value_arg, "--start-time-us") => {
                    if start_time_us_seen {
                        return Err("duplicate argument: --start-time-us".to_string());
                    }
                    let (value, consumed) = take_value_arg(&args, index, "--start-time-us")?;
                    config.startup_time.start_time_us =
                        Some(parse_startup_time_us(&value, "start-time-us")?);
                    start_time_us_seen = true;
                    index += consumed;
                }
                other => {
                    if let Some(name) = unsupported_flag_equals_syntax(other) {
                        return Err(format!(
                            "unsupported argument syntax for --{name}; use --{name}"
                        ));
                    }

                    if let Some(name) = unsupported_firecracker_arg(other) {
                        return Err(format!("unsupported Firecracker argument: --{name}"));
                    }

                    if other.starts_with('-') {
                        return Err(format!("unknown argument: {}", display_arg_name(other)));
                    }

                    return Err("unexpected positional argument".to_string());
                }
            }
        }

        if logger_config_seen {
            config.logger_config = Some(logger_config);
        }

        if config.no_api && config.config_file.is_none() {
            return Err("--no-api requires --config-file".to_string());
        }

        Ok(Self {
            command: Command::Run(Box::new(config)),
        })
    }
}

fn os_arg_into_string(arg: OsString) -> Result<String, String> {
    arg.into_string()
        .map_err(|_| "invalid argument: arguments must be valid UTF-8".to_string())
}

fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> String {
    format!(
        concat!(
            "bangbang {}\n\n",
            "Usage:\n",
            "  bangbang [OPTIONS]\n\n",
            "Value-taking long options accept either --name value or --name=value.\n",
            "Value-less flags reject attached values.\n\n",
            "Options:\n",
            "      --api-sock <PATH>  Unix domain socket path for the API server [default: {}]\n",
            "      --config-file <PATH>\n",
            "                         Firecracker-shaped config file for API-enabled startup\n",
            "      --http-api-max-payload-size <BYTES>\n",
            "                         Maximum HTTP API request size [default: {}]\n",
            "      --id <ID>          MicroVM unique identifier [default: {}]\n",
            "                         Accepts 1-64 bytes, ASCII alphanumeric or '-'\n",
            "      --log-path <PATH>  Logger output file or FIFO path\n",
            "      --level <LEVEL>    Logger level: Off, Trace, Debug, Info, Warn, or Error\n",
            "      --metrics-path <PATH>  Metrics output file or FIFO path\n",
            "      --mmds-size-limit <BYTES>\n",
            "                         MMDS data store size; defaults to HTTP API limit\n",
            "      --metadata <PATH>  JSON metadata file used to initialize MMDS at startup\n",
            "      --module <MODULE>  Logger module prefix filter for minimal action logs\n",
            "      --no-api          Start from --config-file without publishing an API socket\n",
            "      --show-level       Include level in minimal logger action lines\n",
            "      --show-log-origin  Include callsite origin in minimal logger action lines\n",
            "      --start-time-us <MICROS>\n",
            "                         Process start wall-clock time for future metrics\n",
            "      --start-time-cpu-us <MICROS>\n",
            "                         Process start CPU time for future metrics\n",
            "      --parent-cpu-time-us <MICROS>\n",
            "                         Parent process CPU time for future metrics\n",
            "  -V, --version          Print version\n",
            "  -h, --help             Print help\n\n",
            "Current scope:\n",
            "  Serves GET /, GET /version, GET /vm/config, GET /machine-config, ",
            "pre-boot PUT /machine-config, pre-boot PUT /boot-source, ",
            "pre-boot PUT /drives/{{drive_id}}, pre-boot ",
            "PUT /network-interfaces/{{iface_id}}, pre-boot PUT /vsock, ",
            "pre-boot PUT /metrics and startup metrics output configuration, ",
            "and pre-boot PUT /logger and startup logger configuration with ",
            "minimal action logs; --config-file can apply the same supported ",
            "pre-boot configuration and start the VM before API serving, ",
            "or with --no-api can start without publishing an API socket; ",
            "PATCH /vm parses Paused and Resumed state requests ",
            "as unsupported lifecycle actions; PUT /cpu-config accepts empty ",
            "CPU config as no-op and rejects custom CPU templates; ",
            "PUT /actions starts a process-owned ",
            "HVF boot run-loop worker across bounded step windows for InstanceStart, ",
            "but public run-loop control is not implemented yet."
        ),
        env!("CARGO_PKG_VERSION"),
        DEFAULT_API_SOCK_PATH,
        HTTP_MAX_PAYLOAD_SIZE,
        DEFAULT_INSTANCE_ID
    )
}

fn take_value(args: &[String], index: usize, name: &str) -> Result<String, String> {
    args.get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .cloned()
        .ok_or_else(|| format!("missing value for {name}"))
}

fn take_value_arg(args: &[String], index: usize, name: &str) -> Result<(String, usize), String> {
    let arg = args
        .get(index)
        .ok_or_else(|| format!("missing value for {name}"))?;
    if let Some(value) = inline_value(arg, name) {
        return Ok((value.to_string(), 1));
    }

    take_value(args, index, name).map(|value| (value, 2))
}

fn is_value_arg(arg: &str, name: &str) -> bool {
    arg == name || inline_value(arg, name).is_some()
}

fn inline_value<'arg>(arg: &'arg str, name: &str) -> Option<&'arg str> {
    arg.strip_prefix(name)?.strip_prefix('=')
}

fn validate_api_sock(api_sock: &str) -> Result<(), String> {
    if api_sock.is_empty() {
        return Err("invalid --api-sock: path must not be empty".to_string());
    }

    if api_sock.chars().any(char::is_control) {
        return Err("invalid --api-sock: path must not contain control characters".to_string());
    }

    Ok(())
}

fn parse_http_api_max_payload_size(value: &str) -> Result<usize, String> {
    parse_positive_usize_arg(value, "http-api-max-payload-size")
}

fn validate_config_file_path(config_file: &str) -> Result<(), String> {
    validate_startup_file_path(config_file, "config-file")
}

fn validate_metadata_path(metadata: &str) -> Result<(), String> {
    validate_startup_file_path(metadata, "metadata")
}

fn validate_startup_file_path(path: &str, name: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(format!("invalid --{name}: path must not be empty"));
    }

    if path.chars().any(char::is_control) {
        return Err(format!(
            "invalid --{name}: path must not contain control characters"
        ));
    }

    Ok(())
}

fn parse_mmds_size_limit(value: &str) -> Result<usize, String> {
    parse_positive_usize_arg(value, "mmds-size-limit")
}

fn parse_startup_time_us(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("invalid --{name}: value must be a non-negative integer"))
}

fn parse_positive_usize_arg(value: &str, name: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid --{name}: value must be a positive integer"))?;

    if parsed == 0 {
        return Err(format!("invalid --{name}: value must be greater than 0"));
    }

    Ok(parsed)
}

fn validate_instance_id(id: &str) -> Result<(), String> {
    if !(MIN_INSTANCE_ID_LEN..=MAX_INSTANCE_ID_LEN).contains(&id.len()) {
        return Err(format!(
            "invalid --id: invalid length {}; length must be between {} and {}",
            id.len(),
            MIN_INSTANCE_ID_LEN,
            MAX_INSTANCE_ID_LEN
        ));
    }

    for (position, ch) in id.chars().enumerate() {
        if !(ch == '-' || ch.is_ascii_alphanumeric()) {
            return Err(format!(
                "invalid --id: invalid character {ch:?} at position {position}"
            ));
        }
    }

    Ok(())
}

fn unsupported_firecracker_arg(arg: &str) -> Option<&str> {
    let name = firecracker_arg_name(arg)?;
    UNSUPPORTED_FIRECRACKER_ARGS.contains(&name).then_some(name)
}

fn firecracker_arg_name(arg: &str) -> Option<&str> {
    let name = arg.strip_prefix("--")?;

    Some(name.split_once('=').map_or(name, |(name, _)| name))
}

fn display_arg_name(arg: &str) -> &str {
    arg.split_once('=').map_or(arg, |(name, _)| name)
}

fn unsupported_flag_equals_syntax(arg: &str) -> Option<&'static str> {
    [
        ("--help=", "help"),
        ("--no-api=", "no-api"),
        ("--show-level=", "show-level"),
        ("--show-log-origin=", "show-log-origin"),
        ("--version=", "version"),
    ]
    .into_iter()
    .find_map(|(prefix, name)| arg.starts_with(prefix).then_some(name))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::logger::{LoggerConfigError, LoggerConfigInput, LoggerLevel};
    use bangbang_runtime::machine::{MAX_MEM_SIZE_MIB, MachineConfigError};
    use bangbang_runtime::metrics::{MetricsConfigError, MetricsConfigInput, MetricsDiagnostics};
    use bangbang_runtime::mmds::MmdsDataStoreError;
    use bangbang_runtime::serial::SerialConfigError;
    use bangbang_runtime::{BackendError, InstanceState, VmmAction, VmmActionError, VmmData};

    use crate::vmm::{
        ApiRequestMetricParseFailure, GetApiRequest, InstanceStartExecutor, PatchApiRequest,
        ProcessSessionDiagnostics, ProcessSessionExitStatus, ProcessVmm, PutApiRequest,
        VmmRequestHandler,
    };

    use super::{
        ApiServerError, Args, Command, DEFAULT_API_SOCK_PATH, DEFAULT_INSTANCE_ID,
        HTTP_MAX_PAYLOAD_SIZE, MAX_INSTANCE_ID_LEN, ProcessError, ProcessExitCode, StartupConfig,
        StartupTimeClock, StartupTimeConfig, parse_process_args,
    };

    #[derive(Debug, Clone)]
    struct TestInstanceStarter;

    impl InstanceStartExecutor for TestInstanceStarter {
        type Session = ();

        fn start(
            &mut self,
            _controller: &bangbang_runtime::VmmController,
        ) -> Result<Self::Session, BackendError> {
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct ProcessExitTestStarter {
        signal: TestProcessExitSignal,
    }

    impl InstanceStartExecutor for ProcessExitTestStarter {
        type Session = TestProcessExitSession;

        fn start(
            &mut self,
            _controller: &bangbang_runtime::VmmController,
        ) -> Result<Self::Session, BackendError> {
            Ok(TestProcessExitSession {
                signal: self.signal.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct TestProcessExitSession {
        signal: TestProcessExitSignal,
    }

    impl ProcessSessionDiagnostics for TestProcessExitSession {
        fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
            Some(self.signal.wakeup_fd())
        }

        fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
            self.signal.drain()
        }

        fn process_exit_status(&self) -> ProcessSessionExitStatus {
            self.signal.status()
        }
    }

    #[derive(Debug, Clone)]
    struct TestProcessExitSignal {
        reader: Arc<Mutex<UnixStream>>,
        writer: Arc<Mutex<UnixStream>>,
        reader_fd: RawFd,
        status: Arc<Mutex<ProcessSessionExitStatus>>,
    }

    impl TestProcessExitSignal {
        fn new() -> Self {
            let (reader, writer) =
                UnixStream::pair().expect("test process-exit signal should be created");
            reader
                .set_nonblocking(true)
                .expect("test process-exit reader should be nonblocking");
            let reader_fd = reader.as_raw_fd();

            Self {
                reader: Arc::new(Mutex::new(reader)),
                writer: Arc::new(Mutex::new(writer)),
                reader_fd,
                status: Arc::new(Mutex::new(ProcessSessionExitStatus::Running)),
            }
        }

        const fn wakeup_fd(&self) -> RawFd {
            self.reader_fd
        }

        fn status(&self) -> ProcessSessionExitStatus {
            *self
                .status
                .lock()
                .expect("test process-exit status should lock")
        }

        fn trigger(&self, status: ProcessSessionExitStatus) {
            *self
                .status
                .lock()
                .expect("test process-exit status should lock") = status;
            self.writer
                .lock()
                .expect("test process-exit writer should lock")
                .write_all(&[1])
                .expect("test process-exit signal should write");
        }

        fn drain(&mut self) -> Result<(), std::io::ErrorKind> {
            let mut reader = self
                .reader
                .lock()
                .expect("test process-exit reader should lock");
            let mut buffer = [0; 64];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof),
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err.kind()),
                }
            }
        }
    }

    struct ProcessExitAfterPeriodicFlush<S>
    where
        S: InstanceStartExecutor,
    {
        inner: ProcessVmm<S>,
        process_exit_trigger: TestProcessExitSignal,
    }

    impl<S> ProcessExitAfterPeriodicFlush<S>
    where
        S: InstanceStartExecutor,
    {
        fn new(inner: ProcessVmm<S>, process_exit_trigger: TestProcessExitSignal) -> Self {
            Self {
                inner,
                process_exit_trigger,
            }
        }
    }

    impl<S> VmmRequestHandler for ProcessExitAfterPeriodicFlush<S>
    where
        S: InstanceStartExecutor,
    {
        fn handle_action(&mut self, action: VmmAction) -> Result<VmmData, VmmActionError> {
            self.inner.handle_action(action)
        }

        fn handle_get_request(
            &mut self,
            request: GetApiRequest,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_get_request(request)
        }

        fn handle_patch_request(
            &mut self,
            request: PatchApiRequest,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_patch_request(request)
        }

        fn handle_put_request(
            &mut self,
            request: PutApiRequest,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_put_request(request)
        }

        fn record_api_request_parse_failure(&mut self, request: ApiRequestMetricParseFailure) {
            self.inner.record_api_request_parse_failure(request);
        }

        fn record_put_actions_request(&mut self) {
            self.inner.record_put_actions_request();
        }

        fn handle_put_action_request(
            &mut self,
            action: VmmAction,
        ) -> Result<VmmData, VmmActionError> {
            self.inner.handle_put_action_request(action)
        }

        fn record_deprecated_api_call(&mut self) {
            self.inner.record_deprecated_api_call();
        }

        fn handle_periodic_metrics_flush(&mut self) -> Result<bool, VmmActionError> {
            let result = self.inner.handle_periodic_metrics_flush();
            self.process_exit_trigger
                .trigger(ProcessSessionExitStatus::GuestRequestedStop);
            result
        }

        fn process_exit_wakeup_fd(&self) -> Option<RawFd> {
            self.inner.process_exit_wakeup_fd()
        }

        fn drain_process_exit_wakeup(&mut self) -> Result<(), std::io::ErrorKind> {
            self.inner.drain_process_exit_wakeup()
        }

        fn process_exit_status(&self) -> ProcessSessionExitStatus {
            self.inner.process_exit_status()
        }
    }

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|arg| arg.to_string()))
    }

    fn parse_run(args: &[&str]) -> Result<StartupConfig, String> {
        match parse(args)?.command {
            Command::Run(config) => Ok(*config),
            command => Err(format!("expected run command, got {command:?}")),
        }
    }

    fn unique_logger_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-main-test-{}-{nanos}-{name}.log",
            std::process::id()
        ))
    }

    fn unique_metrics_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-main-test-{}-{nanos}-{name}.metrics",
            std::process::id()
        ))
    }

    fn test_shutdown_signal() -> super::ShutdownSignal {
        super::ShutdownSignal::install().expect("test shutdown signal should install")
    }

    fn unique_serial_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-main-test-{}-{nanos}-{name}.serial",
            std::process::id()
        ))
    }

    fn unique_config_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-main-test-{}-{nanos}-{name}.json",
            std::process::id()
        ))
    }

    #[test]
    fn process_exit_code_value_matches_argument_parsing_contract() {
        assert_eq!(ProcessExitCode::ProcessFailure.value(), 1);
        assert_eq!(ProcessExitCode::ArgumentParsing.value(), 153);
    }

    #[test]
    fn api_server_error_maps_to_process_failure_exit_code() {
        let err = ProcessError::ApiServer(ApiServerError::SocketPathExists);

        assert_eq!(err.exit_code(), ProcessExitCode::ProcessFailure);
    }

    #[test]
    fn startup_configuration_error_maps_to_process_failure_exit_code() {
        let err = ProcessError::StartupConfiguration(
            bangbang_runtime::VmmActionError::LoggerConfig(LoggerConfigError::EmptyPath),
        );

        assert_eq!(err.exit_code(), ProcessExitCode::ProcessFailure);
        assert_eq!(
            err.to_string(),
            "startup configuration error: logger path must not be empty"
        );

        let err = ProcessError::StartupConfiguration(
            bangbang_runtime::VmmActionError::MetricsConfig(MetricsConfigError::EmptyPath),
        );

        assert_eq!(err.exit_code(), ProcessExitCode::ProcessFailure);
        assert_eq!(
            err.to_string(),
            "startup configuration error: metrics path must not be empty"
        );
    }

    #[test]
    fn argument_parse_error_maps_to_argument_parsing_exit_code() {
        let err = ProcessError::ArgumentParsing("unknown argument: --unknown".to_string());

        assert_eq!(err.exit_code(), ProcessExitCode::ArgumentParsing);
    }

    #[test]
    fn process_error_display_preserves_parser_message() {
        let err = ProcessError::ArgumentParsing("unknown argument: --unknown".to_string());

        assert_eq!(err.to_string(), "unknown argument: --unknown");
    }

    #[test]
    fn parse_process_args_wraps_parser_errors() {
        let err = parse_process_args([OsString::from("--unknown=/tmp/secret")])
            .expect_err("process arg parsing should fail");

        assert_eq!(
            err,
            ProcessError::ArgumentParsing("unknown argument: --unknown".to_string())
        );
        assert_eq!(err.exit_code(), ProcessExitCode::ArgumentParsing);
    }

    #[test]
    fn parse_os_help_arg_ignores_non_utf8_args() {
        let args = Args::parse_os([OsString::from("--help"), OsString::from_vec(vec![0xff])])
            .expect("help should bypass parsing");

        assert_eq!(args.command, Command::Help);
    }

    #[test]
    fn rejects_non_utf8_process_arg() {
        let err =
            Args::parse_os([OsString::from_vec(vec![0xff])]).expect_err("non-utf8 arg should fail");

        assert_eq!(err, "invalid argument: arguments must be valid UTF-8");
    }

    #[test]
    fn parse_empty_args_uses_defaults() {
        let config = parse_run(&[]).expect("empty args should parse");

        assert_eq!(config.api_sock, DEFAULT_API_SOCK_PATH);
        assert_eq!(config.config_file, None);
        assert_eq!(config.http_api_max_payload_size, HTTP_MAX_PAYLOAD_SIZE);
        assert_eq!(config.mmds_size_limit, None);
        assert_eq!(config.effective_mmds_size_limit(), HTTP_MAX_PAYLOAD_SIZE);
        assert_eq!(config.id, DEFAULT_INSTANCE_ID);
        assert_eq!(config.logger_config, None);
        assert_eq!(config.metrics_config, None);
        assert_eq!(config.metadata, None);
        assert!(!config.no_api);
        assert_eq!(config.startup_time, StartupTimeConfig::default());
    }

    #[test]
    fn parse_help_arg() {
        let args = parse(&["--help"]).expect("help arg should parse");

        assert_eq!(args.command, Command::Help);
    }

    #[test]
    fn help_text_lists_current_api_scope() {
        let help = super::help_text();

        assert!(help.contains("Serves GET /, GET /version"));
        assert!(
            help.contains("Value-taking long options accept either --name value or --name=value")
        );
        assert!(help.contains("Value-less flags reject attached values"));
        assert!(help.contains("GET /vm/config"));
        assert!(help.contains("--config-file <PATH>"));
        assert!(help.contains("GET /machine-config"));
        assert!(help.contains("pre-boot PUT /machine-config"));
        assert!(help.contains("pre-boot PUT /boot-source"));
        assert!(help.contains("pre-boot PUT /drives/{drive_id}"));
        assert!(help.contains("pre-boot PUT /metrics"));
        assert!(help.contains("startup metrics output configuration"));
        assert!(help.contains("pre-boot PUT /logger and startup logger configuration"));
        assert!(help.contains("minimal action logs"));
        assert!(help.contains("--config-file can apply the same supported pre-boot configuration"));
        assert!(help.contains("PATCH /vm parses Paused and Resumed state requests"));
        assert!(help.contains("PUT /cpu-config accepts empty CPU config as no-op"));
        assert!(help.contains("--log-path <PATH>"));
        assert!(help.contains("--metrics-path <PATH>"));
        assert!(help.contains("--http-api-max-payload-size <BYTES>"));
        assert!(help.contains("--mmds-size-limit <BYTES>"));
        assert!(help.contains("--metadata <PATH>"));
        assert!(help.contains("--no-api"));
        assert!(help.contains("without publishing an API socket"));
        assert!(help.contains("--show-level"));
        assert!(help.contains("--start-time-us <MICROS>"));
        assert!(help.contains("--start-time-cpu-us <MICROS>"));
        assert!(help.contains("--parent-cpu-time-us <MICROS>"));
        assert!(help.contains("PUT /actions starts a process-owned HVF boot run-loop worker"));
        assert!(help.contains("across bounded step windows for InstanceStart"));
        assert!(help.contains("public run-loop control is not implemented yet"));
    }

    #[test]
    fn parse_short_help_arg() {
        let args = parse(&["-h"]).expect("short help arg should parse");

        assert_eq!(args.command, Command::Help);
    }

    #[test]
    fn parse_help_arg_ignores_other_args() {
        let args = parse(&["--help", "--unknown"]).expect("help should bypass parsing");

        assert_eq!(args.command, Command::Help);
    }

    #[test]
    fn parse_version_arg() {
        let args = parse(&["--version"]).expect("version arg should parse");

        assert_eq!(args.command, Command::Version);
    }

    #[test]
    fn parse_version_arg_ignores_other_args() {
        let args = parse(&["--version", "--unknown"]).expect("version should bypass parsing");

        assert_eq!(args.command, Command::Version);
    }

    #[test]
    fn parse_short_version_arg() {
        let args = parse(&["-V"]).expect("short version arg should parse");

        assert_eq!(args.command, Command::Version);
    }

    #[test]
    fn parse_api_sock_arg() {
        let config =
            parse_run(&["--api-sock", "/tmp/custom.socket"]).expect("api socket arg should parse");

        assert_eq!(config.api_sock, "/tmp/custom.socket");
        assert_eq!(config.config_file, None);
        assert_eq!(config.http_api_max_payload_size, HTTP_MAX_PAYLOAD_SIZE);
        assert_eq!(config.id, DEFAULT_INSTANCE_ID);
        assert_eq!(config.logger_config, None);
        assert_eq!(config.metrics_config, None);
    }

    #[test]
    fn parse_config_file_arg() {
        let config = parse_run(&["--config-file", "/tmp/bangbang-config.json"])
            .expect("config-file arg should parse");

        assert_eq!(
            config.config_file,
            Some("/tmp/bangbang-config.json".to_string())
        );
        assert!(!config.no_api);
    }

    #[test]
    fn parse_no_api_config_file_arg() {
        let config = parse_run(&["--config-file", "/tmp/bangbang-config.json", "--no-api"])
            .expect("no-api config-file startup should parse");

        assert_eq!(
            config.config_file,
            Some("/tmp/bangbang-config.json".to_string())
        );
        assert!(config.no_api);
    }

    #[test]
    fn parse_http_api_max_payload_size_arg() {
        let config = parse_run(&["--http-api-max-payload-size", "65536"])
            .expect("payload size arg should parse");

        assert_eq!(config.http_api_max_payload_size, 65_536);
    }

    #[test]
    fn parse_mmds_size_limit_arg() {
        let config =
            parse_run(&["--mmds-size-limit", "65536"]).expect("MMDS size limit should parse");

        assert_eq!(config.mmds_size_limit, Some(65_536));
        assert_eq!(config.effective_mmds_size_limit(), 65_536);
    }

    #[test]
    fn parse_metadata_arg() {
        let config = parse_run(&["--metadata", "/tmp/mmds.json"])
            .expect("metadata startup arg should parse");

        assert_eq!(config.metadata, Some("/tmp/mmds.json".to_string()));
    }

    #[test]
    fn mmds_size_limit_inherits_http_api_max_payload_size_when_omitted() {
        let config = parse_run(&["--http-api-max-payload-size", "65536"])
            .expect("HTTP payload size should parse");

        assert_eq!(config.mmds_size_limit, None);
        assert_eq!(config.effective_mmds_size_limit(), 65_536);
    }

    #[test]
    fn explicit_mmds_size_limit_overrides_http_api_max_payload_size() {
        let config = parse_run(&[
            "--http-api-max-payload-size",
            "65536",
            "--mmds-size-limit",
            "4096",
        ])
        .expect("MMDS size limit should parse");

        assert_eq!(config.http_api_max_payload_size, 65_536);
        assert_eq!(config.mmds_size_limit, Some(4096));
        assert_eq!(config.effective_mmds_size_limit(), 4096);
    }

    #[test]
    fn parse_id_arg() {
        let config = parse_run(&["--id", "demo-1"]).expect("id arg should parse");

        assert_eq!(config.api_sock, DEFAULT_API_SOCK_PATH);
        assert_eq!(config.http_api_max_payload_size, HTTP_MAX_PAYLOAD_SIZE);
        assert_eq!(config.id, "demo-1");
        assert_eq!(config.logger_config, None);
        assert_eq!(config.metrics_config, None);
    }

    #[test]
    fn parse_logger_startup_args() {
        let config = parse_run(&[
            "--log-path",
            "/tmp/bangbang.log",
            "--level",
            "Warning",
            "--module",
            "api_server",
            "--show-level",
            "--show-log-origin",
        ])
        .expect("logger startup args should parse");

        assert_eq!(
            config.logger_config,
            Some(
                LoggerConfigInput::new()
                    .with_log_path("/tmp/bangbang.log")
                    .with_level(LoggerLevel::Warn)
                    .with_module("api_server")
                    .with_show_level(true)
                    .with_show_log_origin(true)
            )
        );
    }

    #[test]
    fn parse_metrics_startup_args() {
        let config = parse_run(&["--metrics-path", "/tmp/bangbang.metrics"])
            .expect("metrics startup arg should parse");

        assert_eq!(
            config.metrics_config,
            Some(MetricsConfigInput::new("/tmp/bangbang.metrics"))
        );
    }

    #[test]
    fn parse_startup_time_args() {
        let config = parse_run(&[
            "--start-time-us",
            "1000",
            "--start-time-cpu-us",
            "2000",
            "--parent-cpu-time-us",
            "3000",
        ])
        .expect("startup timing args should parse");

        assert_eq!(
            config.startup_time,
            StartupTimeConfig {
                start_time_us: Some(1000),
                start_time_cpu_us: Some(2000),
                parent_cpu_time_us: Some(3000),
            }
        );
    }

    #[test]
    fn parse_zero_startup_time_args() {
        let config = parse_run(&[
            "--start-time-us",
            "0",
            "--start-time-cpu-us",
            "0",
            "--parent-cpu-time-us",
            "0",
        ])
        .expect("zero startup timing args should parse");

        assert_eq!(
            config.startup_time,
            StartupTimeConfig {
                start_time_us: Some(0),
                start_time_cpu_us: Some(0),
                parent_cpu_time_us: Some(0),
            }
        );
    }

    #[test]
    fn parse_max_startup_time_args() {
        let max_value = u64::MAX.to_string();
        let config = parse_run(&[
            "--start-time-us",
            &max_value,
            "--start-time-cpu-us",
            &max_value,
            "--parent-cpu-time-us",
            &max_value,
        ])
        .expect("maximum startup timing args should parse");

        assert_eq!(
            config.startup_time,
            StartupTimeConfig {
                start_time_us: Some(u64::MAX),
                start_time_cpu_us: Some(u64::MAX),
                parent_cpu_time_us: Some(u64::MAX),
            }
        );
    }

    #[test]
    fn startup_time_config_builds_elapsed_metrics_diagnostics() {
        let diagnostics = StartupTimeConfig {
            start_time_us: Some(1000),
            start_time_cpu_us: Some(2000),
            parent_cpu_time_us: Some(3000),
        }
        .metrics_diagnostics_at(StartupTimeClock::new(1500, 2500));

        assert_eq!(diagnostics.start_time_us(), Some(500));
        assert_eq!(diagnostics.start_time_cpu_us(), Some(3500));
        assert_eq!(diagnostics.parent_cpu_time_us(), None);
    }

    #[test]
    fn startup_time_config_omits_parent_only_diagnostics() {
        let diagnostics = StartupTimeConfig {
            start_time_us: None,
            start_time_cpu_us: None,
            parent_cpu_time_us: Some(3000),
        }
        .metrics_diagnostics_at(StartupTimeClock::new(1500, 2500));

        assert_eq!(diagnostics, MetricsDiagnostics::default());
    }

    #[test]
    fn startup_time_config_saturates_future_start_times() {
        let diagnostics = StartupTimeConfig {
            start_time_us: Some(2000),
            start_time_cpu_us: Some(3000),
            parent_cpu_time_us: Some(4000),
        }
        .metrics_diagnostics_at(StartupTimeClock::new(1000, 2500));

        assert_eq!(diagnostics.start_time_us(), Some(0));
        assert_eq!(diagnostics.start_time_cpu_us(), Some(4000));
        assert_eq!(diagnostics.parent_cpu_time_us(), None);
    }

    #[test]
    fn startup_time_config_saturates_parent_cpu_overflow() {
        let diagnostics = StartupTimeConfig {
            start_time_us: None,
            start_time_cpu_us: Some(0),
            parent_cpu_time_us: Some(1),
        }
        .metrics_diagnostics_at(StartupTimeClock::new(0, u64::MAX));

        assert_eq!(diagnostics.start_time_us(), None);
        assert_eq!(diagnostics.start_time_cpu_us(), Some(u64::MAX));
        assert_eq!(diagnostics.parent_cpu_time_us(), None);
    }

    #[test]
    fn parse_startup_args_together() {
        let config = parse_run(&[
            "--api-sock",
            "/tmp/custom.socket",
            "--config-file",
            "/tmp/bangbang-config.json",
            "--id",
            "demo-1",
            "--http-api-max-payload-size",
            "65536",
            "--mmds-size-limit",
            "4096",
            "--metadata",
            "/tmp/mmds.json",
            "--metrics-path",
            "/tmp/bangbang.metrics",
            "--start-time-us",
            "1000",
            "--start-time-cpu-us",
            "2000",
            "--parent-cpu-time-us",
            "3000",
        ])
        .expect("startup args should parse");

        assert_eq!(config.api_sock, "/tmp/custom.socket");
        assert_eq!(
            config.config_file,
            Some("/tmp/bangbang-config.json".to_string())
        );
        assert_eq!(config.http_api_max_payload_size, 65_536);
        assert_eq!(config.mmds_size_limit, Some(4096));
        assert_eq!(config.metadata, Some("/tmp/mmds.json".to_string()));
        assert_eq!(config.id, "demo-1");
        assert_eq!(config.logger_config, None);
        assert_eq!(
            config.metrics_config,
            Some(MetricsConfigInput::new("/tmp/bangbang.metrics"))
        );
        assert_eq!(
            config.startup_time,
            StartupTimeConfig {
                start_time_us: Some(1000),
                start_time_cpu_us: Some(2000),
                parent_cpu_time_us: Some(3000),
            }
        );
    }

    #[test]
    fn parse_startup_args_with_equals_syntax() {
        let config = parse_run(&[
            "--api-sock=/tmp/custom.socket",
            "--config-file=/tmp/bangbang-config.json",
            "--id=demo-1",
            "--http-api-max-payload-size=65536",
            "--mmds-size-limit=4096",
            "--metadata=/tmp/mmds.json",
            "--metrics-path=/tmp/bangbang.metrics",
            "--start-time-us=1000",
            "--start-time-cpu-us=2000",
            "--parent-cpu-time-us=3000",
        ])
        .expect("startup args should parse with equals syntax");

        assert_eq!(config.api_sock, "/tmp/custom.socket");
        assert_eq!(
            config.config_file,
            Some("/tmp/bangbang-config.json".to_string())
        );
        assert_eq!(config.http_api_max_payload_size, 65_536);
        assert_eq!(config.mmds_size_limit, Some(4096));
        assert_eq!(config.metadata, Some("/tmp/mmds.json".to_string()));
        assert_eq!(config.id, "demo-1");
        assert_eq!(
            config.metrics_config,
            Some(MetricsConfigInput::new("/tmp/bangbang.metrics"))
        );
        assert_eq!(
            config.startup_time,
            StartupTimeConfig {
                start_time_us: Some(1000),
                start_time_cpu_us: Some(2000),
                parent_cpu_time_us: Some(3000),
            }
        );
    }

    #[test]
    fn parse_observability_args_with_equals_syntax() {
        let config = parse_run(&[
            "--log-path=/tmp/bangbang.log",
            "--level=Warning",
            "--module=api_server",
            "--show-level",
            "--show-log-origin",
        ])
        .expect("logger startup args should parse with equals syntax");

        assert_eq!(
            config.logger_config,
            Some(
                LoggerConfigInput::new()
                    .with_log_path("/tmp/bangbang.log")
                    .with_level(LoggerLevel::Warn)
                    .with_module("api_server")
                    .with_show_level(true)
                    .with_show_log_origin(true)
            )
        );
    }

    #[test]
    fn rejects_missing_api_sock_value() {
        let err = parse(&["--api-sock"]).expect_err("missing api socket value should fail");

        assert_eq!(err, "missing value for --api-sock");
    }

    #[test]
    fn rejects_missing_config_file_value() {
        let err = parse(&["--config-file"]).expect_err("missing config file value should fail");

        assert_eq!(err, "missing value for --config-file");
    }

    #[test]
    fn rejects_no_api_without_config_file() {
        let err = parse(&["--no-api"]).expect_err("no-api should require config file");

        assert_eq!(err, "--no-api requires --config-file");
    }

    #[test]
    fn rejects_missing_id_value() {
        let err = parse(&["--id", "--api-sock"]).expect_err("missing id value should fail");

        assert_eq!(err, "missing value for --id");
    }

    #[test]
    fn rejects_missing_http_api_max_payload_size_value() {
        let err = parse(&["--http-api-max-payload-size", "--id"])
            .expect_err("missing payload size value should fail");

        assert_eq!(err, "missing value for --http-api-max-payload-size");
    }

    #[test]
    fn rejects_missing_mmds_size_limit_value() {
        let err = parse(&["--mmds-size-limit", "--id"]).expect_err("missing MMDS size should fail");

        assert_eq!(err, "missing value for --mmds-size-limit");
    }

    #[test]
    fn rejects_missing_metadata_value() {
        let err = parse(&["--metadata", "--id"]).expect_err("missing metadata path should fail");

        assert_eq!(err, "missing value for --metadata");
    }

    #[test]
    fn rejects_missing_observability_arg_values() {
        let err = parse(&["--log-path", "--id"]).expect_err("missing log path value should fail");

        assert_eq!(err, "missing value for --log-path");

        let err = parse(&["--level", "--id"]).expect_err("missing level value should fail");

        assert_eq!(err, "missing value for --level");

        let err = parse(&["--module", "--id"]).expect_err("missing module value should fail");

        assert_eq!(err, "missing value for --module");

        let err =
            parse(&["--metrics-path", "--id"]).expect_err("missing metrics path value should fail");

        assert_eq!(err, "missing value for --metrics-path");
    }

    #[test]
    fn rejects_missing_startup_time_values() {
        let err = parse(&["--start-time-us", "--id"]).expect_err("missing start time should fail");

        assert_eq!(err, "missing value for --start-time-us");

        let err = parse(&["--start-time-cpu-us", "--id"])
            .expect_err("missing start CPU time should fail");

        assert_eq!(err, "missing value for --start-time-cpu-us");

        let err = parse(&["--parent-cpu-time-us", "--id"])
            .expect_err("missing parent CPU time should fail");

        assert_eq!(err, "missing value for --parent-cpu-time-us");
    }

    #[test]
    fn rejects_duplicate_api_sock() {
        let err = parse(&[
            "--api-sock",
            "/tmp/one.socket",
            "--api-sock",
            "/tmp/two.socket",
        ])
        .expect_err("duplicate api socket should fail");

        assert_eq!(err, "duplicate argument: --api-sock");
    }

    #[test]
    fn rejects_duplicate_config_file() {
        let err = parse(&[
            "--config-file",
            "/tmp/one.json",
            "--config-file",
            "/tmp/two.json",
        ])
        .expect_err("duplicate config-file should fail");

        assert_eq!(err, "duplicate argument: --config-file");
    }

    #[test]
    fn rejects_duplicate_no_api() {
        let err = parse(&[
            "--config-file",
            "/tmp/bangbang-config.json",
            "--no-api",
            "--no-api",
        ])
        .expect_err("duplicate no-api should fail");

        assert_eq!(err, "duplicate argument: --no-api");
    }

    #[test]
    fn rejects_duplicate_http_api_max_payload_size() {
        let err = parse(&[
            "--http-api-max-payload-size",
            "65536",
            "--http-api-max-payload-size",
            "65537",
        ])
        .expect_err("duplicate payload size should fail");

        assert_eq!(err, "duplicate argument: --http-api-max-payload-size");
    }

    #[test]
    fn rejects_duplicate_mmds_size_limit() {
        let err = parse(&["--mmds-size-limit", "65536", "--mmds-size-limit", "65537"])
            .expect_err("duplicate MMDS size should fail");

        assert_eq!(err, "duplicate argument: --mmds-size-limit");
    }

    #[test]
    fn rejects_duplicate_metadata() {
        let err = parse(&["--metadata", "/tmp/one.json", "--metadata", "/tmp/two.json"])
            .expect_err("duplicate metadata path should fail");

        assert_eq!(err, "duplicate argument: --metadata");
    }

    #[test]
    fn rejects_duplicate_id() {
        let err = parse(&["--id", "one", "--id", "two"]).expect_err("duplicate id should fail");

        assert_eq!(err, "duplicate argument: --id");
    }

    #[test]
    fn rejects_duplicate_mixed_equals_and_separate_args() {
        let err = parse(&["--id", "one", "--id=two"]).expect_err("duplicate id should fail");

        assert_eq!(err, "duplicate argument: --id");

        let err = parse(&[
            "--api-sock=/tmp/one.socket",
            "--api-sock",
            "/tmp/two.socket",
        ])
        .expect_err("duplicate api socket should fail");

        assert_eq!(err, "duplicate argument: --api-sock");
    }

    #[test]
    fn rejects_invalid_http_api_max_payload_size() {
        let err = parse(&["--http-api-max-payload-size", "0"])
            .expect_err("zero payload size should fail");

        assert_eq!(
            err,
            "invalid --http-api-max-payload-size: value must be greater than 0"
        );

        let err = parse(&["--http-api-max-payload-size", "abc"])
            .expect_err("non-numeric payload size should fail");

        assert_eq!(
            err,
            "invalid --http-api-max-payload-size: value must be a positive integer"
        );

        let err = parse(&[
            "--http-api-max-payload-size",
            "999999999999999999999999999999",
        ])
        .expect_err("overflowing payload size should fail");

        assert_eq!(
            err,
            "invalid --http-api-max-payload-size: value must be a positive integer"
        );
    }

    #[test]
    fn rejects_invalid_mmds_size_limit() {
        let err = parse(&["--mmds-size-limit", "0"]).expect_err("zero MMDS size should fail");

        assert_eq!(
            err,
            "invalid --mmds-size-limit: value must be greater than 0"
        );

        let err =
            parse(&["--mmds-size-limit", "abc"]).expect_err("non-numeric MMDS size should fail");

        assert_eq!(
            err,
            "invalid --mmds-size-limit: value must be a positive integer"
        );

        let err = parse(&["--mmds-size-limit", "999999999999999999999999999999"])
            .expect_err("overflowing MMDS size should fail");

        assert_eq!(
            err,
            "invalid --mmds-size-limit: value must be a positive integer"
        );
    }

    #[test]
    fn rejects_duplicate_observability_args() {
        let err = parse(&["--show-level", "--show-level"])
            .expect_err("duplicate show-level flag should fail");

        assert_eq!(err, "duplicate argument: --show-level");

        let err = parse(&["--level", "Info", "--level", "Debug"])
            .expect_err("duplicate level arg should fail");

        assert_eq!(err, "duplicate argument: --level");

        let err = parse(&["--log-path", "/tmp/one.log", "--log-path", "/tmp/two.log"])
            .expect_err("duplicate log-path arg should fail");

        assert_eq!(err, "duplicate argument: --log-path");

        let err = parse(&["--module", "api_server", "--module", "runtime"])
            .expect_err("duplicate module arg should fail");

        assert_eq!(err, "duplicate argument: --module");

        let err = parse(&["--show-log-origin", "--show-log-origin"])
            .expect_err("duplicate show-log-origin flag should fail");

        assert_eq!(err, "duplicate argument: --show-log-origin");

        let err = parse(&[
            "--metrics-path",
            "/tmp/one.metrics",
            "--metrics-path",
            "/tmp/two.metrics",
        ])
        .expect_err("duplicate metrics-path arg should fail");

        assert_eq!(err, "duplicate argument: --metrics-path");
    }

    #[test]
    fn rejects_duplicate_startup_time_args() {
        let err = parse(&["--start-time-us", "1", "--start-time-us", "2"])
            .expect_err("duplicate start time should fail");

        assert_eq!(err, "duplicate argument: --start-time-us");

        let err = parse(&["--start-time-cpu-us", "1", "--start-time-cpu-us", "2"])
            .expect_err("duplicate start CPU time should fail");

        assert_eq!(err, "duplicate argument: --start-time-cpu-us");

        let err = parse(&["--parent-cpu-time-us", "1", "--parent-cpu-time-us", "2"])
            .expect_err("duplicate parent CPU time should fail");

        assert_eq!(err, "duplicate argument: --parent-cpu-time-us");
    }

    #[test]
    fn rejects_invalid_startup_time_args() {
        let err = parse(&["--start-time-us", "-1"]).expect_err("negative start time should fail");

        assert_eq!(
            err,
            "invalid --start-time-us: value must be a non-negative integer"
        );

        let err = parse(&["--start-time-cpu-us", "abc"])
            .expect_err("non-numeric start CPU time should fail");

        assert_eq!(
            err,
            "invalid --start-time-cpu-us: value must be a non-negative integer"
        );

        let err = parse(&["--parent-cpu-time-us", "999999999999999999999999999999"])
            .expect_err("overflowing parent CPU time should fail");

        assert_eq!(
            err,
            "invalid --parent-cpu-time-us: value must be a non-negative integer"
        );
    }

    #[test]
    fn rejects_empty_api_sock() {
        let err = parse(&["--api-sock", ""]).expect_err("empty api socket should fail");

        assert_eq!(err, "invalid --api-sock: path must not be empty");
    }

    #[test]
    fn rejects_api_sock_with_control_character() {
        let err = parse(&["--api-sock", "/tmp/bangbang\n.socket"])
            .expect_err("api socket with control character should fail");

        assert_eq!(
            err,
            "invalid --api-sock: path must not contain control characters"
        );
    }

    #[test]
    fn rejects_empty_config_file_path() {
        let err = parse(&["--config-file", ""]).expect_err("empty config file should fail");

        assert_eq!(err, "invalid --config-file: path must not be empty");
    }

    #[test]
    fn rejects_config_file_path_with_control_character() {
        let err = parse(&["--config-file", "/tmp/bangbang\n.json"])
            .expect_err("config file with control character should fail");

        assert_eq!(
            err,
            "invalid --config-file: path must not contain control characters"
        );
    }

    #[test]
    fn rejects_empty_metadata_path() {
        let err = parse(&["--metadata", ""]).expect_err("empty metadata path should fail");

        assert_eq!(err, "invalid --metadata: path must not be empty");
    }

    #[test]
    fn rejects_metadata_path_with_control_character() {
        let err = parse(&["--metadata", "/tmp/mmds\n.json"])
            .expect_err("metadata path with control character should fail");

        assert_eq!(
            err,
            "invalid --metadata: path must not contain control characters"
        );
    }

    #[test]
    fn rejects_empty_id() {
        let err = parse(&["--id", ""]).expect_err("empty id should fail");

        assert_eq!(
            err,
            "invalid --id: invalid length 0; length must be between 1 and 64"
        );
    }

    #[test]
    fn rejects_id_with_underscore() {
        let err = parse(&["--id", "vm_1"]).expect_err("underscore id should fail");

        assert_eq!(err, "invalid --id: invalid character '_' at position 2");
    }

    #[test]
    fn rejects_id_with_colon() {
        let err = parse(&["--id", "vm:1"]).expect_err("colon id should fail");

        assert_eq!(err, "invalid --id: invalid character ':' at position 2");
    }

    #[test]
    fn rejects_id_with_non_ascii_alphanumeric() {
        const NON_ASCII_ALPHANUMERIC: char = '\u{e9}';

        let id = format!("vm{NON_ASCII_ALPHANUMERIC}1");
        let err = Args::parse(["--id".to_string(), id]).expect_err("non-ascii id should fail");

        assert_eq!(
            err,
            format!("invalid --id: invalid character {NON_ASCII_ALPHANUMERIC:?} at position 2")
        );
    }

    #[test]
    fn rejects_id_over_max_length() {
        let id = "a".repeat(MAX_INSTANCE_ID_LEN + 1);
        let err = Args::parse(["--id".to_string(), id]).expect_err("long id should fail");

        assert_eq!(
            err,
            "invalid --id: invalid length 65; length must be between 1 and 64"
        );
    }

    #[test]
    fn rejects_multibyte_id_over_max_length_by_bytes() {
        let id = "\u{e9}".repeat(MAX_INSTANCE_ID_LEN / 2 + 1);
        let err = Args::parse(["--id".to_string(), id]).expect_err("long id should fail");

        assert_eq!(
            err,
            "invalid --id: invalid length 66; length must be between 1 and 64"
        );
    }

    #[test]
    fn rejects_unsupported_firecracker_linux_arg() {
        let err = parse(&["--no-seccomp"]).expect_err("unsupported Linux flag should fail");

        assert_eq!(err, "unsupported Firecracker argument: --no-seccomp");
    }

    #[test]
    fn rejects_empty_equals_values_through_existing_validation() {
        let err = parse(&["--api-sock="]).expect_err("empty api socket should fail");

        assert_eq!(err, "invalid --api-sock: path must not be empty");

        let err = parse(&["--id="]).expect_err("empty id should fail");

        assert_eq!(
            err,
            "invalid --id: invalid length 0; length must be between 1 and 64"
        );

        let err =
            parse(&["--http-api-max-payload-size="]).expect_err("empty payload size should fail");

        assert_eq!(
            err,
            "invalid --http-api-max-payload-size: value must be a positive integer"
        );
    }

    #[test]
    fn rejects_equals_syntax_for_supported_flags() {
        let err = parse(&["--no-api=true"]).expect_err("flag with value should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --no-api; use --no-api"
        );

        let err = parse(&["--show-level=true"]).expect_err("flag with value should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --show-level; use --show-level"
        );

        let err = parse(&["--show-log-origin=true"]).expect_err("flag with value should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --show-log-origin; use --show-log-origin"
        );

        let err = parse(&["--help=true"]).expect_err("help flag with value should fail");

        assert_eq!(err, "unsupported argument syntax for --help; use --help");

        let err = parse(&["--version=true"]).expect_err("version flag with value should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --version; use --version"
        );
    }

    #[test]
    fn rejects_invalid_logger_level() {
        let err = parse(&["--level", "verbose"]).expect_err("invalid level should fail");

        assert_eq!(err, "invalid --level: logger level is invalid");
    }

    #[test]
    fn applies_config_file_and_starts_instance() {
        let config_path = unique_config_path("startup");
        let metrics_path = unique_metrics_path("config-file");
        let logger_path = unique_logger_path("config-file");
        let serial_path = unique_serial_path("config-file");
        let metrics_path_json =
            serde_json::to_string(metrics_path.to_str().expect("UTF-8 metrics path"))
                .expect("path should encode");
        let logger_path_json =
            serde_json::to_string(logger_path.to_str().expect("UTF-8 logger path"))
                .expect("path should encode");
        let serial_path_json =
            serde_json::to_string(serial_path.to_str().expect("UTF-8 serial path"))
                .expect("path should encode");
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": 1, "mem_size_mib": 128}},
                "boot-source": {{
                    "kernel_image_path": "/tmp/vmlinux",
                    "boot_args": "console=hvc0 reboot=k panic=1"
                }},
                "drives": [{{
                    "drive_id": "rootfs",
                    "path_on_host": "/tmp/rootfs.ext4",
                    "is_root_device": true,
                    "is_read_only": true
                }}],
                "metrics": {{"metrics_path": {metrics_path_json}}},
                "logger": {{"log_path": {logger_path_json}, "show_level": true}},
                "serial": {{"serial_out_path": {serial_path_json}}},
                "entropy": {{}}
            }}"#
        );
        fs::write(&config_path, config).expect("config file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_config_file(&mut vmm, Some(config_path.to_str().expect("UTF-8 path")))
            .expect("config file should apply and start");

        assert_eq!(vmm.instance_info().state, InstanceState::Running);
        assert!(vmm.has_started_session());
        assert_eq!(vmm.machine_config().vcpu_count(), 1);
        assert_eq!(vmm.machine_config().mem_size_mib(), 128);
        assert!(vmm.boot_source_config().is_some());
        assert_eq!(vmm.drive_configs().len(), 1);
        assert_eq!(
            vmm.serial_config().serial_out_path(),
            Some(serial_path.as_path())
        );
        let data = vmm
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");
        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert!(config.entropy_config().is_some());

        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("flush metrics should succeed");
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n{\"vmm\":{\"metrics_flush_count\":2}}\n"
        );
        assert_eq!(
            fs::read_to_string(&logger_path).expect("logger output should be readable"),
            "level=Info action=InstanceStart\nlevel=Info action=FlushMetrics\n"
        );

        fs::remove_file(config_path).expect("fixture config should clean up");
        fs::remove_file(metrics_path).expect("fixture metrics should clean up");
        fs::remove_file(logger_path).expect("fixture logger should clean up");
    }

    #[test]
    fn config_file_requires_boot_source_before_mutating() {
        let err = super::config_file_actions_from_str(
            r#"{"machine-config":{"vcpu_count":1,"mem_size_mib":128}}"#,
        )
        .expect_err("missing boot-source should fail");

        assert_eq!(err, super::ConfigFileError::MissingSection("boot-source"));
    }

    #[test]
    fn no_api_wait_returns_after_guest_requested_stop_notification() {
        let process_exit_signal = TestProcessExitSignal::new();
        let process_exit_trigger = process_exit_signal.clone();
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            ProcessExitTestStarter {
                signal: process_exit_signal,
            },
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let mut shutdown_signal = test_shutdown_signal();

        process_exit_trigger.trigger(ProcessSessionExitStatus::GuestRequestedStop);

        super::wait_for_no_api_shutdown(&mut shutdown_signal, &mut vmm)
            .expect("guest-requested stop should stop no-api wait successfully");
    }

    #[test]
    fn no_api_wait_fails_after_process_terminal_notification() {
        let process_exit_signal = TestProcessExitSignal::new();
        let process_exit_trigger = process_exit_signal.clone();
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            ProcessExitTestStarter {
                signal: process_exit_signal,
            },
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let mut shutdown_signal = test_shutdown_signal();

        process_exit_trigger.trigger(ProcessSessionExitStatus::Terminal);

        assert_eq!(
            super::wait_for_no_api_shutdown(&mut shutdown_signal, &mut vmm),
            Err(super::ProcessError::ProcessSessionTerminal)
        );
    }

    #[test]
    fn no_api_wait_periodic_metrics_timeout_flushes_after_start_without_sleeping() {
        let metrics_path = unique_metrics_path("no-api-periodic");
        let process_exit_signal = TestProcessExitSignal::new();
        let process_exit_trigger = process_exit_signal.clone();
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            ProcessExitTestStarter {
                signal: process_exit_signal,
            },
        );
        vmm.handle_action(VmmAction::PutMetrics(MetricsConfigInput::new(
            &metrics_path,
        )))
        .expect("metrics should configure");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance should start");
        let mut vmm = ProcessExitAfterPeriodicFlush::new(vmm, process_exit_trigger);
        let mut shutdown_signal = test_shutdown_signal();

        assert_eq!(
            super::wait_for_no_api_shutdown_with_periodic_metrics_scheduler(
                &mut shutdown_signal,
                &mut vmm,
                super::PeriodicMetricsScheduler::due_now(Instant::now()),
            ),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        fs::remove_file(metrics_path).expect("fixture metrics should clean up");
    }

    #[test]
    fn no_api_wait_helper_reports_periodic_metrics_timeout_without_sleeping() {
        let shutdown_signal = test_shutdown_signal();

        assert_eq!(
            super::wait_for_shutdown_or_process_exit(shutdown_signal.wakeup_fd(), None, Some(0)),
            Ok(super::ProcessWaitResult::TimedOut)
        );
    }

    #[test]
    fn config_file_rejects_oversized_machine_config_before_starting() {
        let config_path = unique_config_path("oversized-machine-config");
        let oversized_mem_size_mib = MAX_MEM_SIZE_MIB + 1;
        let config = format!(
            r#"{{
                "machine-config":{{"vcpu_count":1,"mem_size_mib":{oversized_mem_size_mib}}},
                "boot-source":{{"kernel_image_path":"/tmp/vmlinux"}}
            }}"#
        );
        fs::write(&config_path, config).expect("config file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        let err = super::apply_startup_config_file(
            &mut vmm,
            Some(config_path.to_str().expect("UTF-8 path")),
        )
        .expect_err("oversized machine config should fail");

        assert!(matches!(
            err,
            ProcessError::ConfigFile(super::ConfigFileError::Apply(
                bangbang_runtime::VmmActionError::MachineConfig(
                    MachineConfigError::InvalidMemorySize
                )
            ))
        ));
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());
        assert_eq!(vmm.machine_config().mem_size_mib(), 128);

        fs::remove_file(config_path).expect("fixture config should clean up");
    }

    #[test]
    fn config_file_rejects_unknown_section() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"unknown":{}}"#,
        )
        .expect_err("unknown config section should fail");

        assert_eq!(
            err,
            super::ConfigFileError::UnknownSection("unknown".to_string())
        );
    }

    #[test]
    fn config_file_rejects_duplicate_top_level_supported_section() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"boot-source":{"kernel_image_path":"/tmp/vmlinux-2"}}"#,
        )
        .expect_err("duplicate top-level supported section should fail");

        assert_eq!(err, super::ConfigFileError::Malformed);
    }

    #[test]
    fn config_file_rejects_duplicate_top_level_recognized_unsupported_section() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"balloon":{},"balloon":{}}"#,
        )
        .expect_err("duplicate top-level recognized unsupported section should fail");

        assert_eq!(err, super::ConfigFileError::Malformed);
    }

    #[test]
    fn config_file_rejects_duplicate_nested_section_field() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux","kernel_image_path":"/tmp/vmlinux-2"}}"#,
        )
        .expect_err("duplicate nested section field should fail");

        assert_eq!(err, super::ConfigFileError::Malformed);
    }

    #[test]
    fn config_file_rejects_escaped_duplicate_object_key() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"\u0062oot-source":{"kernel_image_path":"/tmp/vmlinux-2"}}"#,
        )
        .expect_err("escaped duplicate object key should fail");

        assert_eq!(err, super::ConfigFileError::Malformed);
    }

    #[test]
    fn config_file_rejects_duplicate_array_item_field() {
        let err = super::config_file_actions_from_str(
            r#"{
                "boot-source":{"kernel_image_path":"/tmp/vmlinux"},
                "drives":[{
                    "drive_id":"rootfs",
                    "drive_id":"data",
                    "path_on_host":"/tmp/rootfs.ext4",
                    "is_root_device":true,
                    "is_read_only":true
                }]
            }"#,
        )
        .expect_err("duplicate array item field should fail");

        assert_eq!(err, super::ConfigFileError::Malformed);
    }

    #[test]
    fn config_file_accepts_entropy_section() {
        let actions = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"entropy":{}}"#,
        )
        .expect("entropy config section should parse");

        assert_eq!(
            actions,
            [
                VmmAction::PutBootSource(BootSourceConfigInput::new("/tmp/vmlinux")),
                VmmAction::PutEntropy(bangbang_runtime::entropy::EntropyConfigInput::new()),
            ]
        );
    }

    #[test]
    fn config_file_accepts_entropy_null_rate_limiter() {
        let actions = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"entropy":{"rate_limiter":null}}"#,
        )
        .expect("entropy config section should accept null rate limiter");

        assert_eq!(
            actions,
            [
                VmmAction::PutBootSource(BootSourceConfigInput::new("/tmp/vmlinux")),
                VmmAction::PutEntropy(bangbang_runtime::entropy::EntropyConfigInput::new()),
            ]
        );
    }

    #[test]
    fn config_file_rejects_malformed_entropy_section() {
        for config in [
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"entropy":"bad"}"#,
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"entropy":{"unknown":true}}"#,
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"entropy":{"rate_limiter":"bad"}}"#,
        ] {
            let err = super::config_file_actions_from_str(config)
                .expect_err("malformed entropy section should fail");

            assert_eq!(
                err,
                super::ConfigFileError::Request {
                    section: "entropy",
                    source: super::RequestError::MalformedRequest
                },
                "{config}"
            );
        }
    }

    #[test]
    fn config_file_entropy_rate_limiter_fails_before_starting() {
        let config_path = unique_config_path("entropy-rate-limiter");
        let config = r#"{
            "boot-source":{"kernel_image_path":"/tmp/vmlinux"},
            "entropy":{"rate_limiter":{"bandwidth":{"size":123456789,"one_time_burst":987654321,"refill_time":777}}}
        }"#;
        fs::write(&config_path, config).expect("config file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        let err = super::apply_startup_config_file(
            &mut vmm,
            Some(config_path.to_str().expect("UTF-8 path")),
        )
        .expect_err("configured entropy rate limiter should fail");

        assert_eq!(
            err,
            ProcessError::ConfigFile(super::ConfigFileError::Apply(
                bangbang_runtime::VmmActionError::EntropyConfig(
                    bangbang_runtime::entropy::EntropyConfigError::UnsupportedRateLimiter
                )
            ))
        );
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());
        let data = vmm
            .handle_action(VmmAction::GetVmConfig)
            .expect("VM config should be returned");
        let VmmData::VmConfiguration(config) = data else {
            panic!("expected VM config");
        };
        assert_eq!(config.entropy_config(), None);

        fs::remove_file(config_path).expect("fixture config should clean up");
    }

    #[test]
    fn config_file_rejects_malformed_drive_array() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"drives":{}}"#,
        )
        .expect_err("malformed drives should fail");

        assert_eq!(
            err,
            super::ConfigFileError::MalformedSection { section: "drives" }
        );
    }

    #[test]
    fn config_file_rejects_non_regular_file() {
        let config_path = unique_config_path("directory");
        fs::create_dir(&config_path).expect("fixture directory should be created");

        let err = super::config_file_actions(config_path.to_str().expect("UTF-8 path"))
            .expect_err("config directory should fail before reading");

        assert_eq!(err, super::ConfigFileError::NotRegular);

        fs::remove_dir(config_path).expect("fixture directory should clean up");
    }

    #[test]
    fn config_file_rejects_oversized_file_before_reading() {
        let config_path = unique_config_path("oversized");
        let file = fs::File::create(&config_path).expect("fixture file should be created");
        file.set_len(super::CONFIG_FILE_MAX_BYTES as u64 + 1)
            .expect("fixture file should be sized");

        let err = super::config_file_actions(config_path.to_str().expect("UTF-8 path"))
            .expect_err("oversized config file should fail before reading");

        assert_eq!(err, super::ConfigFileError::TooLarge);

        fs::remove_file(config_path).expect("fixture file should clean up");
    }

    #[test]
    fn config_file_accepts_exact_size_limit() {
        let config_path = unique_config_path("exact-size");
        let mut config = r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"}}"#.to_string();
        config.extend(std::iter::repeat_n(
            ' ',
            super::CONFIG_FILE_MAX_BYTES - config.len(),
        ));
        fs::write(&config_path, config).expect("fixture file should be written");

        let actions = super::config_file_actions(config_path.to_str().expect("UTF-8 path"))
            .expect("exact limit config file should parse");

        assert!(matches!(actions.as_slice(), [VmmAction::PutBootSource(_)]));

        fs::remove_file(config_path).expect("fixture file should clean up");
    }

    #[test]
    fn config_file_runtime_errors_do_not_start_instance() {
        let config_path = unique_config_path("cpu-config");
        fs::write(
            &config_path,
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"cpu-config":{"kvm_capabilities":["1"]}}"#,
        )
        .expect("config file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        let err = super::apply_startup_config_file(
            &mut vmm,
            Some(config_path.to_str().expect("UTF-8 path")),
        )
        .expect_err("unsupported cpu-config should fail");

        assert!(matches!(
            err,
            ProcessError::ConfigFile(super::ConfigFileError::Apply(
                bangbang_runtime::VmmActionError::UnsupportedAction("PutCpuConfig")
            ))
        ));
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());

        fs::remove_file(config_path).expect("fixture config should clean up");
    }

    #[test]
    fn config_file_noop_cpu_config_starts_instance() {
        let config_path = unique_config_path("noop-cpu-config");
        fs::write(
            &config_path,
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"cpu-config":{}}"#,
        )
        .expect("config file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_config_file(&mut vmm, Some(config_path.to_str().expect("UTF-8 path")))
            .expect("empty cpu-config should not block config-file startup");

        assert_eq!(vmm.instance_info().state, InstanceState::Running);
        assert!(vmm.has_started_session());

        fs::remove_file(config_path).expect("fixture config should clean up");
    }

    #[test]
    fn config_file_serial_errors_do_not_start_instance() {
        let config_path = unique_config_path("serial-rate-limiter");
        fs::write(
            &config_path,
            r#"{
                "boot-source":{"kernel_image_path":"/tmp/vmlinux"},
                "serial":{
                    "serial_out_path":"/tmp/private-serial.out",
                    "rate_limiter":{"bandwidth":{"size":1,"refill_time":1}}
                }
            }"#,
        )
        .expect("config file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        let err = super::apply_startup_config_file(
            &mut vmm,
            Some(config_path.to_str().expect("UTF-8 path")),
        )
        .expect_err("unsupported serial rate limiter should fail");

        assert!(matches!(
            err,
            ProcessError::ConfigFile(super::ConfigFileError::Apply(
                bangbang_runtime::VmmActionError::SerialConfig(
                    SerialConfigError::RateLimiterUnsupported
                )
            ))
        ));
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());
        assert_eq!(vmm.serial_config().serial_out_path(), None);

        fs::remove_file(config_path).expect("fixture config should clean up");
    }

    #[test]
    fn applies_startup_logger_config_before_actions() {
        let path = unique_logger_path("actions");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_logger_config(
            &mut vmm,
            Some(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_show_level(true),
            ),
        )
        .expect("startup logger config should apply");
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance start should succeed");
        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("flush metrics should succeed");

        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            "level=Info action=InstanceStart\nlevel=Info action=FlushMetrics\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn applies_startup_logger_module_filter_before_actions() {
        let matching_path = unique_logger_path("module-filter-match");
        let matching_config = parse_run(&[
            "--log-path",
            matching_path
                .to_str()
                .expect("fixture logger path should be UTF-8"),
            "--module",
            "bangbang_runtime",
        ])
        .expect("matching startup logger args should parse");
        let mut matching_vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_logger_config(&mut matching_vmm, matching_config.logger_config)
            .expect("matching startup logger config should apply");
        matching_vmm
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                "/tmp/vmlinux",
            )))
            .expect("boot source should configure");
        matching_vmm
            .handle_action(VmmAction::InstanceStart)
            .expect("instance start should succeed");
        matching_vmm
            .handle_action(VmmAction::FlushMetrics)
            .expect("flush metrics should succeed");
        assert_eq!(
            fs::read_to_string(&matching_path).expect("matching logger output should be readable"),
            "action=InstanceStart\naction=FlushMetrics\n"
        );

        let filtered_path = unique_logger_path("module-filter-miss");
        let filtered_config = parse_run(&[
            "--log-path",
            filtered_path
                .to_str()
                .expect("fixture logger path should be UTF-8"),
            "--module",
            "api_server",
        ])
        .expect("filtered startup logger args should parse");
        let mut filtered_vmm = ProcessVmm::with_starter(
            "demo-2",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_logger_config(&mut filtered_vmm, filtered_config.logger_config)
            .expect("filtered startup logger config should apply");
        filtered_vmm
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                "/tmp/vmlinux",
            )))
            .expect("boot source should configure");
        filtered_vmm
            .handle_action(VmmAction::InstanceStart)
            .expect("instance start should succeed");
        filtered_vmm
            .handle_action(VmmAction::FlushMetrics)
            .expect("flush metrics should succeed");
        assert_eq!(
            fs::read_to_string(&filtered_path).expect("filtered logger output should be readable"),
            ""
        );

        fs::remove_file(matching_path).expect("matching fixture should clean up");
        fs::remove_file(filtered_path).expect("filtered fixture should clean up");
    }

    #[test]
    fn applies_startup_metrics_config_before_actions() {
        let path = unique_metrics_path("flush");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_metrics_config(&mut vmm, Some(MetricsConfigInput::new(&path)))
            .expect("startup metrics config should apply");
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());
        assert_eq!(
            fs::read_to_string(&path).expect("startup metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );
        vmm.handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            "/tmp/vmlinux",
        )))
        .expect("boot source should configure");
        vmm.handle_action(VmmAction::InstanceStart)
            .expect("instance start should succeed");
        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("flush metrics should succeed");

        assert_eq!(
            fs::read_to_string(&path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n{\"vmm\":{\"metrics_flush_count\":2}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn applies_startup_metrics_config_with_startup_time_diagnostics() {
        let path = unique_metrics_path("startup-time");
        let diagnostics = MetricsDiagnostics::new()
            .with_start_time_us(1000)
            .with_start_time_cpu_us(2000)
            .with_parent_cpu_time_us(3000);
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        )
        .with_process_metrics_diagnostics(diagnostics);

        super::apply_startup_metrics_config(&mut vmm, Some(MetricsConfigInput::new(&path)))
            .expect("startup metrics config should apply");

        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());
        assert_eq!(
            fs::read_to_string(&path).expect("startup metrics output should be readable"),
            "{\"api_server\":{\"process_startup_time_cpu_us\":5000,\"process_startup_time_us\":1000},\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn applies_startup_metadata_before_actions() {
        let path = unique_config_path("metadata");
        fs::write(
            &path,
            r#"{"latest":{"meta-data":{"ami-id":"ami-bangbang"},"user-data":"hello"}}"#,
        )
        .expect("metadata file should be written");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        super::apply_startup_metadata(&mut vmm, Some(path.to_str().expect("UTF-8 path")))
            .expect("startup metadata should apply");

        assert_eq!(
            vmm.handle_action(VmmAction::GetMmds),
            Ok(VmmData::MmdsValue(serde_json::json!({
                "latest": {
                    "meta-data": {
                        "ami-id": "ami-bangbang"
                    },
                    "user-data": "hello"
                }
            })))
        );
        assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
        assert!(!vmm.has_started_session());

        fs::remove_file(path).expect("fixture metadata should clean up");
    }

    #[test]
    fn metadata_file_rejects_non_regular_file() {
        let metadata_path = unique_config_path("metadata-directory");
        fs::create_dir(&metadata_path).expect("fixture directory should be created");

        let err = super::metadata_content_input(metadata_path.to_str().expect("UTF-8 path"))
            .expect_err("metadata directory should fail before reading");

        assert_eq!(err, super::MetadataFileError::NotRegular);

        fs::remove_dir(metadata_path).expect("fixture directory should clean up");
    }

    #[test]
    fn metadata_file_rejects_oversized_file_before_parsing() {
        let metadata_path = unique_config_path("metadata-oversized-file");
        let file = fs::File::create(&metadata_path).expect("fixture file should be created");
        file.set_len(super::METADATA_FILE_MAX_BYTES as u64 + 1)
            .expect("fixture file should be sized");

        let err = super::metadata_content_input(metadata_path.to_str().expect("UTF-8 path"))
            .expect_err("oversized metadata file should fail before parsing");

        assert_eq!(err, super::MetadataFileError::TooLarge);

        fs::remove_file(metadata_path).expect("fixture file should clean up");
    }

    #[test]
    fn metadata_file_accepts_exact_size_limit() {
        let metadata_path = unique_config_path("metadata-exact-size");
        let mut metadata = r#"{"latest":{"user-data":"hello"}}"#.to_string();
        metadata.extend(std::iter::repeat_n(
            ' ',
            super::METADATA_FILE_MAX_BYTES - metadata.len(),
        ));
        fs::write(&metadata_path, metadata).expect("fixture file should be written");

        let input = super::metadata_content_input(metadata_path.to_str().expect("UTF-8 path"))
            .expect("exact limit metadata file should parse");

        assert_eq!(
            input.into_value(),
            serde_json::json!({
                "latest": {
                    "user-data": "hello"
                }
            })
        );

        fs::remove_file(metadata_path).expect("fixture file should clean up");
    }

    #[test]
    fn metadata_file_rejects_invalid_utf8() {
        let metadata_path = unique_config_path("metadata-invalid-utf8");
        fs::write(&metadata_path, [0xff]).expect("fixture file should be written");

        let err = super::metadata_content_input(metadata_path.to_str().expect("UTF-8 path"))
            .expect_err("invalid UTF-8 metadata file should fail");

        assert_eq!(
            err,
            super::MetadataFileError::Read(std::io::ErrorKind::InvalidData)
        );

        fs::remove_file(metadata_path).expect("fixture file should clean up");
    }

    #[test]
    fn startup_metadata_errors_do_not_start_instance() {
        let malformed_path = unique_config_path("metadata-malformed");
        fs::write(&malformed_path, "{").expect("malformed metadata file should be written");
        let non_object_path = unique_config_path("metadata-non-object");
        fs::write(&non_object_path, r#"["not","object"]"#)
            .expect("non-object metadata file should be written");
        let oversized_path = unique_config_path("metadata-oversized");
        let oversized_value = "x".repeat(128);
        fs::write(
            &oversized_path,
            format!(r#"{{"latest":{{"user-data":"{oversized_value}"}}}}"#),
        )
        .expect("oversized metadata file should be written");

        let cases = [
            (
                &malformed_path,
                bangbang_runtime::mmds::MMDS_DATA_STORE_LIMIT_BYTES,
                "malformed",
            ),
            (
                &non_object_path,
                bangbang_runtime::mmds::MMDS_DATA_STORE_LIMIT_BYTES,
                "non-object",
            ),
            (&oversized_path, 32, "oversized"),
        ];
        for (path, limit, case_name) in cases {
            let mut vmm = ProcessVmm::with_starter_and_mmds_data_store_limit(
                "demo-1",
                env!("CARGO_PKG_VERSION"),
                "bangbang",
                TestInstanceStarter,
                limit,
            );
            let err =
                super::apply_startup_metadata(&mut vmm, Some(path.to_str().expect("UTF-8 path")))
                    .expect_err("metadata error should fail startup metadata application");

            match case_name {
                "malformed" => assert_eq!(
                    err,
                    ProcessError::Metadata(super::MetadataFileError::Malformed)
                ),
                "non-object" => assert_eq!(
                    err,
                    ProcessError::Metadata(super::MetadataFileError::Apply(
                        bangbang_runtime::VmmActionError::MmdsDataStore(
                            MmdsDataStoreError::InvalidObject,
                        ),
                    ))
                ),
                "oversized" => {
                    let ProcessError::Metadata(super::MetadataFileError::Apply(
                        bangbang_runtime::VmmActionError::MmdsDataStore(
                            MmdsDataStoreError::DataStoreLimitExceeded {
                                limit_bytes,
                                size_bytes,
                            },
                        ),
                    )) = err
                    else {
                        panic!("expected oversized metadata error, got {err:?}");
                    };
                    assert_eq!(limit_bytes, 32);
                    assert!(size_bytes > limit_bytes);
                }
                _ => panic!("unexpected metadata test case: {case_name}"),
            }
            assert_eq!(vmm.instance_info().state, InstanceState::NotStarted);
            assert!(!vmm.has_started_session());
        }

        fs::remove_file(malformed_path).expect("malformed fixture should clean up");
        fs::remove_file(non_object_path).expect("non-object fixture should clean up");
        fs::remove_file(oversized_path).expect("oversized fixture should clean up");
    }

    #[test]
    fn startup_logger_config_errors_do_not_echo_path() {
        let path = unique_logger_path("missing-parent").join("logger");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        let err = super::apply_startup_logger_config(
            &mut vmm,
            Some(LoggerConfigInput::new().with_log_path(&path)),
        )
        .expect_err("missing parent should fail startup logger config");

        assert!(!err.to_string().contains(path.to_string_lossy().as_ref()));
        assert!(matches!(
            err,
            ProcessError::StartupConfiguration(bangbang_runtime::VmmActionError::LoggerConfig(
                LoggerConfigError::OpenFile(_)
            ))
        ));
    }

    #[test]
    fn startup_metrics_config_errors_do_not_echo_path() {
        let path = unique_metrics_path("missing-parent").join("metrics");
        let mut vmm = ProcessVmm::with_starter(
            "demo-1",
            env!("CARGO_PKG_VERSION"),
            "bangbang",
            TestInstanceStarter,
        );

        let err =
            super::apply_startup_metrics_config(&mut vmm, Some(MetricsConfigInput::new(&path)))
                .expect_err("missing parent should fail startup metrics config");

        assert!(!err.to_string().contains(path.to_string_lossy().as_ref()));
        assert!(matches!(
            err,
            ProcessError::StartupConfiguration(bangbang_runtime::VmmActionError::MetricsConfig(
                MetricsConfigError::OpenFile(_)
            ))
        ));
    }

    #[test]
    fn rejects_unknown_arg() {
        let err = parse(&["--unknown"]).expect_err("unknown args should fail");

        assert_eq!(err, "unknown argument: --unknown");
    }

    #[test]
    fn rejects_unknown_equals_arg_without_echoing_value() {
        let err = parse(&["--unknown=/tmp/secret"]).expect_err("unknown args should fail");

        assert_eq!(err, "unknown argument: --unknown");
    }

    #[test]
    fn rejects_positional_arg() {
        let err = parse(&["image.bin"]).expect_err("positional args should fail");

        assert_eq!(err, "unexpected positional argument");
    }

    #[test]
    fn rejects_positional_path_without_echoing_value() {
        let err = parse(&["/tmp/secret.img"]).expect_err("positional args should fail");

        assert_eq!(err, "unexpected positional argument");
    }
}
