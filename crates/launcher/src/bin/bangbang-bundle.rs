use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use bangbang_launcher::{PackageOptions, build_bundle};

fn main() -> ExitCode {
    match parse_args(std::env::args_os().skip(1)) {
        Ok(Command::Help) => {
            print_usage();
            ExitCode::SUCCESS
        }
        Ok(Command::Build(options)) => match build_bundle(&options) {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("bangbang bundle: {err}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("bangbang bundle: {message}");
            print_usage_error();
            ExitCode::from(2)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Build(PackageOptions),
}

fn parse_args<I>(args: I) -> Result<Command, &'static str>
where
    I: IntoIterator<Item = OsString>,
{
    let mut launcher_binary = None;
    let mut worker_binary = None;
    let mut output_bundle = None;
    let mut signing_identity = None;
    let mut test_worker_resources = None;
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("--launcher") => {
                set_once(
                    &mut launcher_binary,
                    PathBuf::from(required_value(&mut args, "--launcher requires a path")?),
                )?;
            }
            Some("--worker") => {
                set_once(
                    &mut worker_binary,
                    PathBuf::from(required_value(&mut args, "--worker requires a path")?),
                )?;
            }
            Some("--output") => {
                set_once(
                    &mut output_bundle,
                    PathBuf::from(required_value(&mut args, "--output requires a path")?),
                )?;
            }
            Some("--signing-identity") => {
                let identity =
                    required_value(&mut args, "--signing-identity requires a non-empty value")?;
                set_once(&mut signing_identity, identity)?;
            }
            Some("--test-worker-resources") => {
                set_once(
                    &mut test_worker_resources,
                    PathBuf::from(required_value(
                        &mut args,
                        "--test-worker-resources requires a path",
                    )?),
                )?;
            }
            _ => return Err("unknown or non-Unicode argument"),
        }
    }

    Ok(Command::Build(PackageOptions {
        launcher_binary: launcher_binary.ok_or("--launcher is required")?,
        worker_binary: worker_binary.ok_or("--worker is required")?,
        output_bundle: output_bundle.ok_or("--output is required")?,
        signing_identity: signing_identity.unwrap_or_else(|| OsString::from("-")),
        test_worker_resources,
    }))
}

fn required_value<I>(args: &mut I, missing: &'static str) -> Result<OsString, &'static str>
where
    I: Iterator<Item = OsString>,
{
    args.next().filter(|value| !value.is_empty()).ok_or(missing)
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), &'static str> {
    if slot.replace(value).is_some() {
        return Err("duplicate option");
    }
    Ok(())
}

fn print_usage() {
    println!(
        "Usage:\n  bangbang-bundle --launcher PATH --worker PATH --output PATH [--signing-identity IDENTITY]"
    );
}

fn print_usage_error() {
    eprintln!(
        "Usage:\n  bangbang-bundle --launcher PATH --worker PATH --output PATH [--signing-identity IDENTITY]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_inputs_and_ad_hoc_default() {
        let command = parse_args([
            OsString::from("--launcher"),
            OsString::from("launcher"),
            OsString::from("--worker"),
            OsString::from("worker"),
            OsString::from("--output"),
            OsString::from("Bangbang.app"),
        ])
        .expect("arguments should parse");
        let Command::Build(options) = command else {
            panic!("expected build command");
        };
        assert_eq!(options.signing_identity, std::ffi::OsStr::new("-"));
        assert_eq!(options.test_worker_resources, None);
    }

    #[test]
    fn rejects_duplicate_path_option() {
        let result = parse_args([
            OsString::from("--launcher"),
            OsString::from("one"),
            OsString::from("--launcher"),
            OsString::from("two"),
        ]);
        assert_eq!(result, Err("duplicate option"));
    }

    #[test]
    fn rejects_duplicate_signing_identity() {
        let result = parse_args([
            OsString::from("--signing-identity"),
            OsString::from("one"),
            OsString::from("--signing-identity"),
            OsString::from("two"),
        ]);
        assert_eq!(result, Err("duplicate option"));
    }

    #[test]
    fn rejects_unknown_argument_without_echoing_it() {
        let result = parse_args([OsString::from("--private-path=/secret")]);
        assert_eq!(result, Err("unknown or non-Unicode argument"));
        assert!(!result.unwrap_err().contains("secret"));
    }
}
