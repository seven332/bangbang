//! Deprecated Firecracker-shaped memory snapshot rebase command.

use std::path::PathBuf;
use std::process::ExitCode;

use bangbang_snapshot_tools::{RebaseRequest, RebaseTool, execute_rebase};
use clap::Parser;
use clap::error::ErrorKind;

const DEPRECATION_NOTICE: &str = "This tool is deprecated and will be removed in the future. Please use 'snapshot-editor' instead.";
const INVOCATION_ERROR: &str =
    "rebase-snap: invalid arguments; use --help for the supported interface";

#[derive(Parser)]
#[command(
    name = "rebase-snap",
    about = "Apply a native-v2 differential memory snapshot to a base image",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (bangbang; Firecracker v1.16.0-compatible command surface)"
    )
)]
struct Cli {
    /// File path of the base memory snapshot.
    #[arg(long = "base-file", value_name = "PATH")]
    base_file: PathBuf,

    /// File path of the differential memory snapshot.
    #[arg(long = "diff-file", value_name = "PATH")]
    diff_file: PathBuf,
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
            if error.print().is_err() {
                return ExitCode::FAILURE;
            }
            println!("{DEPRECATION_NOTICE}");
            return ExitCode::SUCCESS;
        }
        Err(_) => {
            eprintln!("{INVOCATION_ERROR}");
            return ExitCode::from(2);
        }
    };

    println!("{DEPRECATION_NOTICE}");
    execute_rebase(
        RebaseTool::RebaseSnap,
        RebaseRequest::new(cli.base_file, cli.diff_file),
    )
}
