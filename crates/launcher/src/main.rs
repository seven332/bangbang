use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1).collect::<Vec<OsString>>();
    match bangbang_launcher::launch_embedded_worker(args) {
        Ok(exit) => ExitCode::from(exit.code()),
        Err(err) => {
            eprintln!("bangbang launcher: {err}");
            ExitCode::FAILURE
        }
    }
}
