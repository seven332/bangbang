use std::env;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

mod api_server;
pub mod host_network;
mod vmm;

use api_server::{ApiServer, ApiServerError};
use bangbang_hvf::HvfBackend;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::{SigId, low_level};
use vmm::ProcessVmm;

const DEFAULT_API_SOCK_PATH: &str = "/tmp/bangbang.socket";
const DEFAULT_INSTANCE_ID: &str = "anonymous-instance";
const APP_NAME: &str = "bangbang";
const MIN_INSTANCE_ID_LEN: usize = 1;
const MAX_INSTANCE_ID_LEN: usize = 64;
const UNSUPPORTED_FIRECRACKER_ARGS: &[&str] = &[
    "boot-timer",
    "config-file",
    "describe-snapshot",
    "enable-pci",
    "http-api-max-payload-size",
    "level",
    "log-path",
    "metadata",
    "metrics-path",
    "mmds-size-limit",
    "module",
    "no-api",
    "no-seccomp",
    "parent-cpu-time-us",
    "seccomp-filter",
    "show-level",
    "show-log-origin",
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
            println!("bangbang {}", env!("CARGO_PKG_VERSION"));
            println!(
                "hvf target supported: {}",
                HvfBackend::is_supported_target()
            );

            let mut shutdown_signal = ShutdownSignal::install()?;
            let server = ApiServer::bind(&config.api_sock).map_err(ProcessError::ApiServer)?;
            let mut vmm = ProcessVmm::new(config.id, env!("CARGO_PKG_VERSION"), APP_NAME);
            println!("status: API server listening; VM execution loop is not implemented yet");
            let shutdown_wakeup = shutdown_signal.wakeup_reader();
            server
                .run_until(&mut vmm, shutdown_wakeup)
                .map_err(ProcessError::ApiServer)?;
        }
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
    SignalHandler(std::io::ErrorKind),
}

impl ProcessError {
    fn exit_code(&self) -> ProcessExitCode {
        match self {
            Self::ApiServer(_) => ProcessExitCode::ProcessFailure,
            Self::ArgumentParsing(_) => ProcessExitCode::ArgumentParsing,
            Self::SignalHandler(_) => ProcessExitCode::ProcessFailure,
        }
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiServer(err) => write!(f, "API server error: {err}"),
            Self::ArgumentParsing(message) => f.write_str(message),
            Self::SignalHandler(kind) => {
                write!(f, "failed to register shutdown signal handler: {kind:?}")
            }
        }
    }
}

impl std::error::Error for ProcessError {}

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
    id: String,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            api_sock: DEFAULT_API_SOCK_PATH.to_string(),
            id: DEFAULT_INSTANCE_ID.to_string(),
        }
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
        let mut id_seen = false;
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
            "      --id <ID>          MicroVM unique identifier [default: {}]\n",
            "                         Accepts 1-64 bytes, ASCII alphanumeric or '-'\n",
            "  -V, --version          Print version\n",
            "  -h, --help             Print help\n\n",
            "Current scope:\n",
            "  Serves GET /, GET /version, GET /vm/config, GET /machine-config, ",
            "pre-boot PUT /machine-config, pre-boot PUT /boot-source, ",
            "pre-boot PUT /drives/{{drive_id}}, pre-boot PUT /metrics, and ",
            "pre-boot PUT /logger configuration storage over the API ",
            "socket; PUT /actions starts a process-owned HVF boot run-loop ",
            "worker across bounded step windows for InstanceStart, but public ",
            "run-loop control is not implemented yet."
        ),
        env!("CARGO_PKG_VERSION"),
        DEFAULT_API_SOCK_PATH,
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
    [("--api-sock=", "api-sock"), ("--id=", "id")]
        .into_iter()
        .find_map(|(prefix, name)| arg.starts_with(prefix).then_some(name))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::{
        ApiServerError, Args, Command, DEFAULT_API_SOCK_PATH, DEFAULT_INSTANCE_ID,
        MAX_INSTANCE_ID_LEN, ProcessError, ProcessExitCode, StartupConfig, parse_process_args,
    };

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|arg| arg.to_string()))
    }

    fn parse_run(args: &[&str]) -> Result<StartupConfig, String> {
        match parse(args)?.command {
            Command::Run(config) => Ok(config),
            command => Err(format!("expected run command, got {command:?}")),
        }
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
        assert_eq!(config.id, DEFAULT_INSTANCE_ID);
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
        assert!(help.contains("GET /machine-config"));
        assert!(help.contains("pre-boot PUT /machine-config"));
        assert!(help.contains("pre-boot PUT /boot-source"));
        assert!(help.contains("pre-boot PUT /drives/{drive_id}"));
        assert!(help.contains("pre-boot PUT /metrics"));
        assert!(help.contains("pre-boot PUT /logger configuration storage"));
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
        assert_eq!(config.id, DEFAULT_INSTANCE_ID);
    }

    #[test]
    fn parse_id_arg() {
        let config = parse_run(&["--id", "demo-1"]).expect("id arg should parse");

        assert_eq!(config.api_sock, DEFAULT_API_SOCK_PATH);
        assert_eq!(config.id, "demo-1");
    }

    #[test]
    fn parse_startup_args_together() {
        let config = parse_run(&["--api-sock", "/tmp/custom.socket", "--id", "demo-1"])
            .expect("startup args should parse");

        assert_eq!(config.api_sock, "/tmp/custom.socket");
        assert_eq!(config.id, "demo-1");
    }

    #[test]
    fn rejects_missing_api_sock_value() {
        let err = parse(&["--api-sock"]).expect_err("missing api socket value should fail");

        assert_eq!(err, "missing value for --api-sock");
    }

    #[test]
    fn rejects_missing_id_value() {
        let err = parse(&["--id", "--api-sock"]).expect_err("missing id value should fail");

        assert_eq!(err, "missing value for --id");
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
    fn rejects_duplicate_id() {
        let err = parse(&["--id", "one", "--id", "two"]).expect_err("duplicate id should fail");

        assert_eq!(err, "duplicate argument: --id");
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
    fn rejects_unsupported_firecracker_config_file_arg() {
        let err = parse(&["--config-file", "vm.json"]).expect_err("unsupported arg should fail");

        assert_eq!(err, "unsupported Firecracker argument: --config-file");
    }

    #[test]
    fn rejects_unsupported_firecracker_config_file_equals_arg() {
        let err = parse(&["--config-file=vm.json"]).expect_err("unsupported arg should fail");

        assert_eq!(err, "unsupported Firecracker argument: --config-file");
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
