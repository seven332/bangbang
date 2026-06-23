use std::env;
use std::process::ExitCode;

use bangbang_hvf::HvfBackend;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("bangbang: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(env::args().skip(1))?;

    if args.help {
        print_help();
        return Ok(());
    }

    if args.version {
        println!("bangbang {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    println!("bangbang {}", env!("CARGO_PKG_VERSION"));
    println!(
        "hvf target supported: {}",
        HvfBackend::is_supported_target()
    );
    println!("status: first-PR scaffold only");

    Ok(())
}

#[derive(Debug)]
struct Args {
    help: bool,
    version: bool,
}

impl Args {
    fn parse<I>(args: I) -> Result<Self, String>
    where
        I: Iterator<Item = String>,
    {
        let mut parsed = Self {
            help: false,
            version: false,
        };

        for arg in args {
            match arg.as_str() {
                "--version" | "-V" => parsed.version = true,
                "--help" | "-h" => parsed.help = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }

        Ok(parsed)
    }
}

fn print_help() {
    println!(
        "bangbang {}\n\nUsage:\n  bangbang\n\nOptions:\n  -V, --version  Print version\n  -h, --help     Print help",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::Args;

    #[test]
    fn parse_empty_args() {
        let args = Args::parse([].into_iter()).expect("empty args should parse");

        assert!(!args.help);
        assert!(!args.version);
    }

    #[test]
    fn parse_help_arg() {
        let args = Args::parse(["--help".to_string()].into_iter()).expect("help arg should parse");

        assert!(args.help);
        assert!(!args.version);
    }

    #[test]
    fn parse_version_arg() {
        let args =
            Args::parse(["--version".to_string()].into_iter()).expect("version arg should parse");

        assert!(!args.help);
        assert!(args.version);
    }

    #[test]
    fn rejects_unknown_arg() {
        let err = Args::parse(["--api-sock".to_string()].into_iter())
            .expect_err("unknown args should fail");

        assert_eq!(err, "unknown argument: --api-sock");
    }
}
