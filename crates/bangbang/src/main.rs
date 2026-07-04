use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

mod api_server;
#[doc(hidden)]
#[cfg(target_os = "macos")]
pub mod host_network;
mod vmm;

use api_server::{ApiServer, ApiServerError, config_vmm_action_from_api_request};
use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;
use bangbang_api::http::{RequestError, parse_request_with_limit};
use bangbang_hvf::HvfBackend;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::{SigId, low_level};
use vmm::ProcessVmm;

use bangbang_runtime::logger::{LoggerConfigInput, LoggerLevel};
use bangbang_runtime::metrics::MetricsConfigInput;
use bangbang_runtime::{VmmAction, VmmActionError};

const DEFAULT_API_SOCK_PATH: &str = "/tmp/bangbang.socket";
const DEFAULT_INSTANCE_ID: &str = "anonymous-instance";
const APP_NAME: &str = "bangbang";
const MIN_INSTANCE_ID_LEN: usize = 1;
const MAX_INSTANCE_ID_LEN: usize = 64;
const UNSUPPORTED_FIRECRACKER_ARGS: &[&str] = &[
    "boot-timer",
    "describe-snapshot",
    "enable-pci",
    "metadata",
    "no-api",
    "no-seccomp",
    "parent-cpu-time-us",
    "seccomp-filter",
    "snapshot-version",
    "start-time-cpu-us",
    "start-time-us",
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
            let effective_mmds_size_limit = config.effective_mmds_size_limit();
            let StartupConfig {
                api_sock,
                config_file,
                http_api_max_payload_size,
                id,
                logger_config,
                mmds_size_limit: _,
                metrics_config,
            } = config;

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
            );
            apply_startup_metrics_config(&mut vmm, metrics_config)?;
            apply_startup_logger_config(&mut vmm, logger_config)?;
            apply_startup_config_file(&mut vmm, config_file.as_deref())?;
            let mut shutdown_signal = ShutdownSignal::install()?;
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
        vmm.handle_action(action)
            .map_err(ConfigFileError::Apply)
            .map_err(ProcessError::ConfigFile)?;
    }

    vmm.handle_action(VmmAction::InstanceStart)
        .map(|_| ())
        .map_err(ConfigFileError::Apply)
        .map_err(ProcessError::ConfigFile)
}

fn config_file_actions(config_file: &str) -> Result<Vec<VmmAction>, ConfigFileError> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(config_file)
        .map_err(|err| ConfigFileError::Read(err.kind()))?;
    let metadata = file
        .metadata()
        .map_err(|err| ConfigFileError::Read(err.kind()))?;
    if !metadata.file_type().is_file() {
        return Err(ConfigFileError::NotRegular);
    }

    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|err| ConfigFileError::Read(err.kind()))?;
    config_file_actions_from_str(&contents)
}

fn config_file_actions_from_str(contents: &str) -> Result<Vec<VmmAction>, ConfigFileError> {
    let value = serde_json::from_str::<serde_json::Value>(contents)
        .map_err(|_| ConfigFileError::Malformed)?;
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

    requests
        .into_iter()
        .map(|(section, request)| {
            config_vmm_action_from_api_request(request)
                .ok_or(ConfigFileError::UnsupportedRequest { section })
        })
        .collect()
}

fn validate_config_file_sections(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigFileError> {
    for section in object.keys() {
        match section.as_str() {
            "boot-source" | "cpu-config" | "drives" | "logger" | "machine-config" | "metrics"
            | "mmds-config" | "network-interfaces" | "vsock" => {}
            "balloon" | "entropy" | "memory-hotplug" | "pmem" => {
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
    SignalHandler(std::io::ErrorKind),
    StartupConfiguration(VmmActionError),
}

impl ProcessError {
    fn exit_code(&self) -> ProcessExitCode {
        match self {
            Self::ApiServer(_) => ProcessExitCode::ProcessFailure,
            Self::ArgumentParsing(_) => ProcessExitCode::ArgumentParsing,
            Self::ConfigFile(_) => ProcessExitCode::ProcessFailure,
            Self::SignalHandler(_) => ProcessExitCode::ProcessFailure,
            Self::StartupConfiguration(_) => ProcessExitCode::ProcessFailure,
        }
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiServer(err) => write!(f, "API server error: {err}"),
            Self::ArgumentParsing(message) => f.write_str(message),
            Self::ConfigFile(err) => write!(f, "config-file error: {err}"),
            Self::SignalHandler(kind) => {
                write!(f, "failed to register shutdown signal handler: {kind:?}")
            }
            Self::StartupConfiguration(err) => {
                write!(f, "startup configuration error: {err}")
            }
        }
    }
}

impl std::error::Error for ProcessError {}

#[derive(Debug, PartialEq, Eq)]
enum ConfigFileError {
    Read(std::io::ErrorKind),
    NotRegular,
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
    Run(StartupConfig),
}

#[derive(Debug, PartialEq, Eq)]
struct StartupConfig {
    api_sock: String,
    config_file: Option<String>,
    http_api_max_payload_size: usize,
    id: String,
    logger_config: Option<LoggerConfigInput>,
    mmds_size_limit: Option<usize>,
    metrics_config: Option<MetricsConfigInput>,
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
            metrics_config: None,
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
        let mut metrics_path_seen = false;
        let mut module_seen = false;
        let mut show_level_seen = false;
        let mut show_log_origin_seen = false;
        let mut index = 0;

        while let Some(arg) = args.get(index) {
            match arg.as_str() {
                "--api-sock" => {
                    if api_sock_seen {
                        return Err("duplicate argument: --api-sock".to_string());
                    }
                    let value = take_value(&args, index, "--api-sock")?;
                    validate_api_sock(&value)?;
                    config.api_sock = value;
                    api_sock_seen = true;
                    index += 2;
                }
                "--config-file" => {
                    if config_file_seen {
                        return Err("duplicate argument: --config-file".to_string());
                    }
                    let value = take_value(&args, index, "--config-file")?;
                    validate_config_file_path(&value)?;
                    config.config_file = Some(value);
                    config_file_seen = true;
                    index += 2;
                }
                "--http-api-max-payload-size" => {
                    if http_api_max_payload_size_seen {
                        return Err("duplicate argument: --http-api-max-payload-size".to_string());
                    }
                    let value = take_value(&args, index, "--http-api-max-payload-size")?;
                    config.http_api_max_payload_size = parse_http_api_max_payload_size(&value)?;
                    http_api_max_payload_size_seen = true;
                    index += 2;
                }
                "--id" => {
                    if id_seen {
                        return Err("duplicate argument: --id".to_string());
                    }
                    let value = take_value(&args, index, "--id")?;
                    validate_instance_id(&value)?;
                    config.id = value;
                    id_seen = true;
                    index += 2;
                }
                "--log-path" => {
                    if log_path_seen {
                        return Err("duplicate argument: --log-path".to_string());
                    }
                    let value = take_value(&args, index, "--log-path")?;
                    logger_config = logger_config.with_log_path(value);
                    logger_config_seen = true;
                    log_path_seen = true;
                    index += 2;
                }
                "--level" => {
                    if level_seen {
                        return Err("duplicate argument: --level".to_string());
                    }
                    let value = take_value(&args, index, "--level")?;
                    let level = value
                        .parse::<LoggerLevel>()
                        .map_err(|err| format!("invalid --level: {err}"))?;
                    logger_config = logger_config.with_level(level);
                    logger_config_seen = true;
                    level_seen = true;
                    index += 2;
                }
                "--mmds-size-limit" => {
                    if mmds_size_limit_seen {
                        return Err("duplicate argument: --mmds-size-limit".to_string());
                    }
                    let value = take_value(&args, index, "--mmds-size-limit")?;
                    config.mmds_size_limit = Some(parse_mmds_size_limit(&value)?);
                    mmds_size_limit_seen = true;
                    index += 2;
                }
                "--metrics-path" => {
                    if metrics_path_seen {
                        return Err("duplicate argument: --metrics-path".to_string());
                    }
                    let value = take_value(&args, index, "--metrics-path")?;
                    config.metrics_config = Some(MetricsConfigInput::new(value));
                    metrics_path_seen = true;
                    index += 2;
                }
                "--module" => {
                    if module_seen {
                        return Err("duplicate argument: --module".to_string());
                    }
                    let value = take_value(&args, index, "--module")?;
                    logger_config = logger_config.with_module(value);
                    logger_config_seen = true;
                    module_seen = true;
                    index += 2;
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
                other => {
                    if let Some(name) = unsupported_firecracker_arg(other) {
                        return Err(format!("unsupported Firecracker argument: --{name}"));
                    }

                    if let Some(name) = unsupported_equals_syntax(other) {
                        return Err(format!(
                            "unsupported argument syntax for --{name}; use --{name} <VALUE>"
                        ));
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

        Ok(Self {
            command: Command::Run(config),
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
            "      --module <MODULE>  Logger module filter stored for future log integration\n",
            "      --show-level       Include level in minimal logger action lines\n",
            "      --show-log-origin  Include callsite origin in minimal logger action lines\n",
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
            "pre-boot configuration and start the VM before API serving; ",
            "PATCH /vm parses Paused and Resumed state requests ",
            "as unsupported lifecycle actions; PUT /cpu-config parses custom CPU ",
            "template requests as unsupported CPU configuration actions; ",
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
    if config_file.is_empty() {
        return Err("invalid --config-file: path must not be empty".to_string());
    }

    if config_file.chars().any(char::is_control) {
        return Err("invalid --config-file: path must not contain control characters".to_string());
    }

    Ok(())
}

fn parse_mmds_size_limit(value: &str) -> Result<usize, String> {
    parse_positive_usize_arg(value, "mmds-size-limit")
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

fn unsupported_equals_syntax(arg: &str) -> Option<&'static str> {
    [
        ("--api-sock=", "api-sock"),
        ("--config-file=", "config-file"),
        ("--http-api-max-payload-size=", "http-api-max-payload-size"),
        ("--id=", "id"),
        ("--log-path=", "log-path"),
        ("--level=", "level"),
        ("--metrics-path=", "metrics-path"),
        ("--mmds-size-limit=", "mmds-size-limit"),
        ("--module=", "module"),
    ]
    .into_iter()
    .find_map(|(prefix, name)| arg.starts_with(prefix).then_some(name))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::logger::{LoggerConfigError, LoggerConfigInput, LoggerLevel};
    use bangbang_runtime::metrics::{MetricsConfigError, MetricsConfigInput};
    use bangbang_runtime::{BackendError, InstanceState, VmmAction};

    use crate::vmm::{InstanceStartExecutor, ProcessVmm};

    use super::{
        ApiServerError, Args, Command, DEFAULT_API_SOCK_PATH, DEFAULT_INSTANCE_ID,
        HTTP_MAX_PAYLOAD_SIZE, MAX_INSTANCE_ID_LEN, ProcessError, ProcessExitCode, StartupConfig,
        parse_process_args,
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

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|arg| arg.to_string()))
    }

    fn parse_run(args: &[&str]) -> Result<StartupConfig, String> {
        match parse(args)?.command {
            Command::Run(config) => Ok(config),
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
        assert!(help.contains("PUT /cpu-config parses custom CPU template requests"));
        assert!(help.contains("--log-path <PATH>"));
        assert!(help.contains("--metrics-path <PATH>"));
        assert!(help.contains("--http-api-max-payload-size <BYTES>"));
        assert!(help.contains("--mmds-size-limit <BYTES>"));
        assert!(help.contains("--show-level"));
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
            "--metrics-path",
            "/tmp/bangbang.metrics",
        ])
        .expect("startup args should parse");

        assert_eq!(config.api_sock, "/tmp/custom.socket");
        assert_eq!(
            config.config_file,
            Some("/tmp/bangbang-config.json".to_string())
        );
        assert_eq!(config.http_api_max_payload_size, 65_536);
        assert_eq!(config.mmds_size_limit, Some(4096));
        assert_eq!(config.id, "demo-1");
        assert_eq!(config.logger_config, None);
        assert_eq!(
            config.metrics_config,
            Some(MetricsConfigInput::new("/tmp/bangbang.metrics"))
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
    fn rejects_duplicate_id() {
        let err = parse(&["--id", "one", "--id", "two"]).expect_err("duplicate id should fail");

        assert_eq!(err, "duplicate argument: --id");
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
    fn rejects_unsupported_firecracker_no_api_arg() {
        let err = parse(&["--no-api"]).expect_err("unsupported flag should fail");

        assert_eq!(err, "unsupported Firecracker argument: --no-api");
    }

    #[test]
    fn rejects_unsupported_firecracker_linux_arg() {
        let err = parse(&["--no-seccomp"]).expect_err("unsupported Linux flag should fail");

        assert_eq!(err, "unsupported Firecracker argument: --no-seccomp");
    }

    #[test]
    fn rejects_unsupported_equals_syntax_for_supported_arg() {
        let err =
            parse(&["--api-sock=/tmp/bangbang.socket"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --api-sock; use --api-sock <VALUE>"
        );

        let err =
            parse(&["--config-file=/tmp/secret.json"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --config-file; use --config-file <VALUE>"
        );

        let err =
            parse(&["--http-api-max-payload-size=65536"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --http-api-max-payload-size; use --http-api-max-payload-size <VALUE>"
        );

        let err = parse(&["--log-path=/tmp/secret.log"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --log-path; use --log-path <VALUE>"
        );

        let err = parse(&["--level=Info"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --level; use --level <VALUE>"
        );

        let err = parse(&["--module=api_server"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --module; use --module <VALUE>"
        );

        let err =
            parse(&["--metrics-path=/tmp/secret.metrics"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --metrics-path; use --metrics-path <VALUE>"
        );

        let err = parse(&["--mmds-size-limit=65536"]).expect_err("equals syntax should fail");

        assert_eq!(
            err,
            "unsupported argument syntax for --mmds-size-limit; use --mmds-size-limit <VALUE>"
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
        let metrics_path_json =
            serde_json::to_string(metrics_path.to_str().expect("UTF-8 metrics path"))
                .expect("path should encode");
        let logger_path_json =
            serde_json::to_string(logger_path.to_str().expect("UTF-8 logger path"))
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
                "logger": {{"log_path": {logger_path_json}, "show_level": true}}
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

        vmm.handle_action(VmmAction::FlushMetrics)
            .expect("flush metrics should succeed");
        assert_eq!(
            fs::read_to_string(&metrics_path).expect("metrics output should be readable"),
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
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
    fn config_file_rejects_unsupported_section_before_apply() {
        let err = super::config_file_actions_from_str(
            r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"},"entropy":{}}"#,
        )
        .expect_err("unsupported config section should fail");

        assert_eq!(
            err,
            super::ConfigFileError::UnsupportedSection("entropy".to_string())
        );
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
    fn config_file_runtime_errors_do_not_start_instance() {
        let config_path = unique_config_path("cpu-config");
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
            "{\"vmm\":{\"metrics_flush_count\":1}}\n"
        );

        fs::remove_file(path).expect("fixture should clean up");
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
