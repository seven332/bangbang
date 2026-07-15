use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use bangbang_launcher::{
    PackageOptions, PackageProfile, VMNET_AUTHORIZATION_PROBE_ARG, build_bundle, preflight_bundle,
};

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if is_authorization_probe(&args) {
        return ExitCode::SUCCESS;
    }
    match parse_args(args) {
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
        Ok(Command::Preflight(options)) => match preflight_bundle(&options) {
            Ok(()) => {
                println!("bangbang vmnet preflight: ready");
                ExitCode::SUCCESS
            }
            Err(_) => {
                eprintln!("bangbang vmnet preflight: blocked");
                ExitCode::from(3)
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
    Preflight(PackageOptions),
}

fn parse_args<I>(args: I) -> Result<Command, &'static str>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();
    let preflight = match args.peek().and_then(|argument| argument.to_str()) {
        Some("build") => {
            args.next();
            false
        }
        Some("preflight") => {
            args.next();
            true
        }
        _ => false,
    };
    let mut launcher_binary = None;
    let mut worker_binary = None;
    let mut output_bundle = None;
    let mut signing_identity = None;
    let mut profile = None;
    let mut provisioning_profile = None;
    let mut test_worker_resources = None;

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
            Some("--worker-profile") => {
                let value = required_value(&mut args, "--worker-profile requires a value")?;
                let value = match value.to_str() {
                    Some("networkless") => PackageProfile::Networkless,
                    Some("vmnet") => PackageProfile::Vmnet,
                    Some(_) | None => return Err("invalid worker profile"),
                };
                set_once(&mut profile, value)?;
            }
            Some("--provisioning-profile") => {
                set_once(
                    &mut provisioning_profile,
                    PathBuf::from(required_value(
                        &mut args,
                        "--provisioning-profile requires a path",
                    )?),
                )?;
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

    let profile = profile.unwrap_or_default();
    match profile {
        PackageProfile::Networkless if provisioning_profile.is_some() => {
            return Err("networkless profile rejects provisioning input");
        }
        PackageProfile::Vmnet
            if signing_identity
                .as_deref()
                .is_none_or(|identity| identity == "-")
                || provisioning_profile.is_none()
                || test_worker_resources.is_some() =>
        {
            return Err("vmnet profile requires named signing and provisioning input");
        }
        PackageProfile::Networkless | PackageProfile::Vmnet => {}
    }
    if preflight && profile != PackageProfile::Vmnet {
        return Err("preflight requires the vmnet worker profile");
    }

    let options = PackageOptions {
        launcher_binary: launcher_binary.ok_or("--launcher is required")?,
        worker_binary: worker_binary.ok_or("--worker is required")?,
        output_bundle: output_bundle.ok_or("--output is required")?,
        signing_identity: signing_identity.unwrap_or_else(|| OsString::from("-")),
        profile,
        provisioning_profile,
        test_worker_resources,
    };
    if preflight {
        Ok(Command::Preflight(options))
    } else {
        Ok(Command::Build(options))
    }
}

fn is_authorization_probe(args: &[OsString]) -> bool {
    args.len() == 1
        && args
            .first()
            .is_some_and(|argument| argument == VMNET_AUTHORIZATION_PROBE_ARG)
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
        "Usage:\n  bangbang-bundle [build] --launcher PATH --worker PATH --output PATH [--signing-identity IDENTITY] [--worker-profile networkless|vmnet] [--provisioning-profile PATH]\n  bangbang-bundle preflight --launcher PATH --worker PATH --output PATH --signing-identity IDENTITY --worker-profile vmnet --provisioning-profile PATH"
    );
}

fn print_usage_error() {
    eprintln!(
        "Usage:\n  bangbang-bundle [build] --launcher PATH --worker PATH --output PATH [--signing-identity IDENTITY] [--worker-profile networkless|vmnet] [--provisioning-profile PATH]\n  bangbang-bundle preflight --launcher PATH --worker PATH --output PATH --signing-identity IDENTITY --worker-profile vmnet --provisioning-profile PATH"
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
        assert_eq!(options.profile, PackageProfile::Networkless);
        assert_eq!(options.provisioning_profile, None);
        assert_eq!(options.test_worker_resources, None);
    }

    #[test]
    fn parses_explicit_vmnet_build_and_preflight() {
        for mode in ["build", "preflight"] {
            let command = parse_args([
                OsString::from(mode),
                OsString::from("--launcher"),
                OsString::from("launcher"),
                OsString::from("--worker"),
                OsString::from("worker"),
                OsString::from("--output"),
                OsString::from("Bangbang.app"),
                OsString::from("--signing-identity"),
                OsString::from("Developer ID Application: Private"),
                OsString::from("--worker-profile"),
                OsString::from("vmnet"),
                OsString::from("--provisioning-profile"),
                OsString::from("approved.provisionprofile"),
            ])
            .expect("vmnet arguments should parse");
            let options = match command {
                Command::Build(options) if mode == "build" => options,
                Command::Preflight(options) if mode == "preflight" => options,
                _ => panic!("unexpected command mode"),
            };
            assert_eq!(options.profile, PackageProfile::Vmnet);
            assert_eq!(
                options.provisioning_profile,
                Some(PathBuf::from("approved.provisionprofile"))
            );
        }
    }

    #[test]
    fn rejects_contradictory_profile_inputs_during_argument_parsing() {
        assert_eq!(
            parse_args([
                OsString::from("--provisioning-profile"),
                OsString::from("private.provisionprofile"),
            ]),
            Err("networkless profile rejects provisioning input")
        );
        assert_eq!(
            parse_args([
                OsString::from("--worker-profile"),
                OsString::from("vmnet"),
                OsString::from("--signing-identity"),
                OsString::from("-"),
                OsString::from("--provisioning-profile"),
                OsString::from("private.provisionprofile"),
            ]),
            Err("vmnet profile requires named signing and provisioning input")
        );
        assert_eq!(
            parse_args([
                OsString::from("preflight"),
                OsString::from("--worker-profile"),
                OsString::from("networkless"),
            ]),
            Err("preflight requires the vmnet worker profile")
        );
    }

    #[test]
    fn authorization_probe_requires_one_exact_private_argument() {
        assert!(is_authorization_probe(&[OsString::from(
            VMNET_AUTHORIZATION_PROBE_ARG
        )]));
        assert!(!is_authorization_probe(&[]));
        assert!(!is_authorization_probe(&[
            OsString::from(VMNET_AUTHORIZATION_PROBE_ARG),
            OsString::from("extra"),
        ]));
        assert!(!is_authorization_probe(&[OsString::from(
            "--private-vmnet-authorization-probe=1"
        )]));
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
    fn rejects_duplicate_or_invalid_profile_options_without_echoing_values() {
        assert_eq!(
            parse_args([
                OsString::from("--worker-profile"),
                OsString::from("networkless"),
                OsString::from("--worker-profile"),
                OsString::from("networkless"),
            ]),
            Err("duplicate option")
        );
        let result = parse_args([
            OsString::from("--worker-profile"),
            OsString::from("private-secret-profile"),
        ]);
        assert_eq!(result, Err("invalid worker profile"));
        assert!(!result.unwrap_err().contains("secret"));
    }

    #[test]
    fn rejects_unknown_argument_without_echoing_it() {
        let result = parse_args([OsString::from("--private-path=/secret")]);
        assert_eq!(result, Err("unknown or non-Unicode argument"));
        assert!(!result.unwrap_err().contains("secret"));
    }
}
