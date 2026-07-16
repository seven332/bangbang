//! Firecracker v1.16-compatible offline seccomp artifact compiler.

mod artifact;
mod tool;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

use crate::tool::RunOptions;

const INVOCATION_ERROR: &str =
    "seccompiler-bin: invalid arguments; use --help for the supported interface";

#[derive(Debug, Parser)]
#[command(
    name = "seccompiler-bin",
    about = "Compile Firecracker v1.16 seccomp policies into offline Linux artifacts",
    long_about = "Compile Firecracker v1.16 seccomp policies into offline Linux artifacts.\n\nThis is bangbang's host-side compatibility tool. It does not install or enforce seccomp on macOS.",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (bangbang; Firecracker v1.16.0-compatible artifact format)"
    )
)]
struct Cli {
    /// Linux architecture on which the BPF program will run.
    #[arg(short = 't', long = "target-arch", value_name = "ARCH")]
    target_arch: String,

    /// Firecracker v1.16 JSON policy file.
    #[arg(short = 'i', long = "input-file", value_name = "PATH")]
    input_file: PathBuf,

    /// Combined output path, or parent selector in split mode.
    #[arg(
        short = 'o',
        long = "output-file",
        value_name = "PATH",
        default_value = "seccomp_binary_filter.out"
    )]
    output_file: PathBuf,

    /// Drop argument checks and rule-level actions (deprecated).
    #[arg(short = 'b', long = "basic")]
    basic: bool,

    /// Write vmm.bpf, api.bpf, and vcpu.bpf instead of one combined file.
    #[arg(long = "split-output")]
    split_output: bool,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return match error.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        Err(_) => {
            eprintln!("{INVOCATION_ERROR}");
            return ExitCode::from(2);
        }
    };

    let options = RunOptions {
        target_arch: cli.target_arch,
        input_file: cli.input_file,
        output_file: cli.output_file,
        basic: cli.basic,
        split_output: cli.split_output,
    };

    match tool::run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("seccompiler-bin: {error}");
            ExitCode::FAILURE
        }
    }
}
