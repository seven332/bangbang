use std::env;
use std::fmt;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

mod api_server;

use api_server::{ApiServer, ApiServerError};
use bangbang_hvf::HvfBackend;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::iterator::{Handle as SignalHandle, Signals};

const DEFAULT_API_SOCK_PATH: &str = "/tmp/bangbang.socket";
const DEFAULT_INSTANCE_ID: &str = "anonymous-instance";
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
    let args = parse_process_args(env::args().skip(1))?;

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

            let shutdown_signal = ShutdownSignal::install(config.api_sock.clone())?;
            let server = ApiServer::bind(&config.api_sock).map_err(ProcessError::ApiServer)?;
            println!("status: API server listening; VM startup is not implemented yet");
            server
                .run_until(env!("CARGO_PKG_VERSION"), shutdown_signal.requested())
                .map_err(ProcessError::ApiServer)?;
        }
    }

    Ok(())
}

fn parse_process_args<I>(args: I) -> Result<Args, ProcessError>
where
    I: IntoIterator<Item = String>,
{
    Args::parse(args).map_err(ProcessError::ArgumentParsing)
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
    requested: Arc<AtomicBool>,
    signal_handle: SignalHandle,
    signal_thread: Option<thread::JoinHandle<()>>,
}

impl ShutdownSignal {
    fn install(api_sock: String) -> Result<Self, ProcessError> {
        let requested = Arc::new(AtomicBool::new(false));
        let thread_requested = Arc::clone(&requested);
        let mut signals = Signals::new([SIGINT, SIGTERM])
            .map_err(|err| ProcessError::SignalHandler(err.kind()))?;
        let signal_handle = signals.handle();
        let signal_thread = thread::spawn(move || {
            if signals.forever().next().is_some() {
                thread_requested.store(true, Ordering::Relaxed);
                let _ = UnixStream::connect(api_sock);
            }
        });

        Ok(Self {
            requested,
            signal_handle,
            signal_thread: Some(signal_thread),
        })
    }

    fn requested(&self) -> &AtomicBool {
        &self.requested
    }
}

impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        let should_join = !self.requested.load(Ordering::Relaxed);
        self.signal_handle.close();
        if should_join {
            if let Some(signal_thread) = self.signal_thread.take() {
                let _ = signal_thread.join();
            }
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

        while index < args.len() {
            let arg = &args[index];

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
                other if let Some(name) = unsupported_firecracker_arg(other) => {
                    return Err(format!("unsupported Firecracker argument: --{name}"));
                }
                other if let Some(name) = unsupported_equals_syntax(other) => {
                    return Err(format!(
                        "unsupported argument syntax for --{name}; use --{name} <VALUE>"
                    ));
                }
                other if other.starts_with('-') => {
                    return Err(format!("unknown argument: {}", display_arg_name(other)));
                }
                _ => return Err("unexpected positional argument".to_string()),
            }
        }

        Ok(Self {
            command: Command::Run(config),
        })
    }
}

fn print_help() {
    println!(
        "bangbang {}\n\nUsage:\n  bangbang [OPTIONS]\n\nOptions:\n      --api-sock <PATH>  Unix domain socket path for the API server [default: {}]\n      --id <ID>          MicroVM unique identifier [default: {}]\n  -V, --version          Print version\n  -h, --help             Print help\n\nCurrent scope:\n  Serves GET /version over the API socket; VM startup is not implemented yet.",
        env!("CARGO_PKG_VERSION"),
        DEFAULT_API_SOCK_PATH,
        DEFAULT_INSTANCE_ID
    );
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
        if !(ch == '-' || ch.is_alphanumeric()) {
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
    use super::{
        parse_process_args, ApiServerError, Args, Command, ProcessError, ProcessExitCode,
        StartupConfig, DEFAULT_API_SOCK_PATH, DEFAULT_INSTANCE_ID, MAX_INSTANCE_ID_LEN,
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
        let err = parse_process_args(["--unknown=/tmp/secret".to_string()])
            .expect_err("process arg parsing should fail");

        assert_eq!(
            err,
            ProcessError::ArgumentParsing("unknown argument: --unknown".to_string())
        );
        assert_eq!(err.exit_code(), ProcessExitCode::ArgumentParsing);
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
    fn rejects_id_over_max_length() {
        let id = "a".repeat(MAX_INSTANCE_ID_LEN + 1);
        let err = Args::parse(["--id".to_string(), id]).expect_err("long id should fail");

        assert_eq!(
            err,
            "invalid --id: invalid length 65; length must be between 1 and 64"
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
