//! Firecracker-shaped snapshot editor memory rebase command.

use std::path::PathBuf;
use std::process::ExitCode;

use bangbang_snapshot_tools::{RebaseRequest, RebaseTool, execute_rebase};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

const INVOCATION_ERROR: &str =
    "snapshot-editor: invalid arguments; use --help for the supported interface";

#[derive(Debug, Parser)]
#[command(
    name = "snapshot-editor",
    about = "Edit bangbang-native snapshot memory artifacts",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (bangbang; Firecracker v1.16.0-compatible command surface)"
    )
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Edit a snapshot memory artifact.
    #[command(subcommand)]
    EditMemory(EditMemoryCommand),
}

#[derive(Debug, Subcommand)]
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

    match cli.command {
        Command::EditMemory(EditMemoryCommand::Rebase {
            memory_path,
            diff_path,
        }) => execute_rebase(
            RebaseTool::SnapshotEditor,
            RebaseRequest::new(memory_path, diff_path),
        ),
    }
}
