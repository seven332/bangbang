//! Firecracker-shaped snapshot editor command.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use bangbang_snapshot_tools::{
    RebaseRequest, RebaseTool, SnapshotInfoView, execute_rebase, execute_snapshot_info,
    execute_snapshot_register_removal,
};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

const INVOCATION_ERROR: &str =
    "snapshot-editor: invalid arguments; use --help for the supported interface";

#[derive(Parser)]
#[command(
    name = "snapshot-editor",
    about = "Inspect and edit bangbang-native snapshot artifacts",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (bangbang; Firecracker v1.16.0-compatible command surface)"
    )
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Edit a snapshot memory artifact.
    #[command(subcommand)]
    EditMemory(EditMemoryCommand),

    /// Edit a snapshot VM state artifact.
    #[command(subcommand)]
    EditVmstate(EditVmstateCommand),

    /// Print information from a snapshot VM state artifact.
    #[command(subcommand)]
    InfoVmstate(InfoVmstateCommand),
}

#[derive(Subcommand)]
enum EditMemoryCommand {
    /// Apply a differential snapshot on top of a base memory image.
    Rebase {
        /// Path to the memory file.
        #[arg(short = 'm', long = "memory-path", value_name = "PATH")]
        memory_path: PathBuf,

        /// Path to the differential memory file.
        #[arg(short = 'd', long = "diff-path", value_name = "PATH")]
        diff_path: PathBuf,
    },
}

#[derive(Subcommand)]
enum EditVmstateCommand {
    /// Remove reviewed Firecracker register state from every vCPU.
    RemoveRegs {
        /// Register IDs in decimal or 0x-prefixed hexadecimal notation.
        #[arg(
            value_parser = parse_maybe_hex_u64,
            num_args = 1..,
            value_delimiter = ' ',
            value_name = "REGS"
        )]
        regs: Vec<u64>,

        /// Path to the VM state file.
        #[arg(short = 'v', long = "vmstate-path", value_name = "PATH")]
        vmstate_path: PathBuf,

        /// Path of the new edited VM state file.
        #[arg(short = 'o', long = "output-path", value_name = "PATH")]
        output_path: PathBuf,
    },
}

#[derive(Subcommand)]
enum InfoVmstateCommand {
    /// Print the exact native snapshot version.
    Version {
        /// Path to the VM state file.
        #[arg(short = 'v', long = "vmstate-path", value_name = "PATH")]
        vmstate_path: PathBuf,
    },

    /// Print canonical information about vCPU states.
    VcpuStates {
        /// Path to the VM state file.
        #[arg(short = 'v', long = "vmstate-path", value_name = "PATH")]
        vmstate_path: PathBuf,
    },

    /// Print canonical readable VM state.
    VmState {
        /// Path to the VM state file.
        #[arg(short = 'v', long = "vmstate-path", value_name = "PATH")]
        vmstate_path: PathBuf,
    },
}

fn parse_maybe_hex_u64(value: &str) -> Result<u64, String> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|_| "invalid register ID".to_string())
    } else {
        value
            .parse::<u64>()
            .map_err(|_| "invalid register ID".to_string())
    }
}

fn emit_invocation_error() -> ExitCode {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(INVOCATION_ERROR.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
    ExitCode::from(2)
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
        Err(_) => return emit_invocation_error(),
    };

    match cli.command {
        Command::EditMemory(EditMemoryCommand::Rebase {
            memory_path,
            diff_path,
        }) => execute_rebase(
            RebaseTool::SnapshotEditor,
            RebaseRequest::new(memory_path, diff_path),
        ),
        Command::EditVmstate(EditVmstateCommand::RemoveRegs {
            regs,
            vmstate_path,
            output_path,
        }) => execute_snapshot_register_removal(regs, vmstate_path, output_path),
        Command::InfoVmstate(InfoVmstateCommand::Version { vmstate_path }) => {
            execute_snapshot_info(SnapshotInfoView::Version, vmstate_path)
        }
        Command::InfoVmstate(InfoVmstateCommand::VcpuStates { vmstate_path }) => {
            execute_snapshot_info(SnapshotInfoView::VcpuStates, vmstate_path)
        }
        Command::InfoVmstate(InfoVmstateCommand::VmState { vmstate_path }) => {
            execute_snapshot_info(SnapshotInfoView::VmState, vmstate_path)
        }
    }
}
