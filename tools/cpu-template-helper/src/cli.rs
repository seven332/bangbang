//! Strict public CPU-template helper command runner.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{ColorChoice, Parser, Subcommand};

use crate::HelperExitClass;
use crate::input::{InputError, read_regular_utf8};
use crate::profile::{
    CpuTemplateOperationError, EffectiveCpuTemplateProvider, dump_with_provider,
    verify_with_provider,
};
use crate::projection::{InspectionPreparationError, prepare_inspection_request};
use crate::publication::{PublicationError, publish_new_artifact};

const INVOCATION_ERROR: &str =
    "cpu-template-helper: invalid arguments; use --help for the supported interface";

#[derive(Debug, Parser)]
#[command(
    name = "cpu-template-helper",
    about = "Inspect effective arm64 CPU templates with Hypervisor.framework",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (bangbang; Firecracker v1.16.0-compatible command surface)"
    ),
    color = ColorChoice::Never
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Template-related operations.
    #[command(subcommand)]
    Template(TemplateOperation),
}

#[derive(Debug, Subcommand)]
enum TemplateOperation {
    /// Dump the effective CPU configuration in custom-template format.
    Dump {
        /// Path of a Firecracker-shaped configuration document.
        #[arg(short, long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Path of an explicit CPU template applied after the configuration.
        #[arg(short, long, value_name = "PATH")]
        template: Option<PathBuf>,
        /// Absent output path to publish.
        #[arg(short, long, value_name = "PATH", default_value = "cpu_config.json")]
        output: PathBuf,
    },
    /// Verify the selected CPU template at the application/readback checkpoint.
    Verify {
        /// Path of a Firecracker-shaped configuration document.
        #[arg(short, long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Path of an explicit CPU template applied after the configuration.
        #[arg(short, long, value_name = "PATH")]
        template: Option<PathBuf>,
    },
}

/// Run one complete public command with an explicitly supplied effective
/// provider and output streams.
pub fn run_cli_with_provider<I, T>(
    args: I,
    provider: &mut impl EffectiveCpuTemplateProvider,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> HelperExitClass
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return if stdout.write_all(error.to_string().as_bytes()).is_ok() {
                HelperExitClass::Success
            } else {
                HelperExitClass::OperationalFailure
            };
        }
        Err(_) => {
            let _ = writeln!(stderr, "{INVOCATION_ERROR}");
            return HelperExitClass::InvocationFailure;
        }
    };

    match execute(cli, provider) {
        Ok(()) => HelperExitClass::Success,
        Err(error) => {
            let _ = writeln!(stderr, "cpu-template-helper: {error}");
            HelperExitClass::OperationalFailure
        }
    }
}

fn execute(
    cli: Cli,
    provider: &mut impl EffectiveCpuTemplateProvider,
) -> Result<(), CliOperationError> {
    match cli.command {
        Command::Template(TemplateOperation::Dump {
            config,
            template,
            output,
        }) => {
            let request = prepare_from_paths(config.as_deref(), template.as_deref())?;
            let bytes =
                dump_with_provider(provider, &request).map_err(CliOperationError::Operation)?;
            publish_new_artifact(&output, &bytes).map_err(CliOperationError::Publication)
        }
        Command::Template(TemplateOperation::Verify { config, template }) => {
            let request = prepare_from_paths(config.as_deref(), template.as_deref())?;
            verify_with_provider(provider, &request).map_err(CliOperationError::Operation)
        }
    }
}

fn prepare_from_paths(
    config: Option<&Path>,
    template: Option<&Path>,
) -> Result<crate::projection::PreparedCpuTemplateInspection, CliOperationError> {
    let config = config
        .map(read_regular_utf8)
        .transpose()
        .map_err(CliOperationError::Input)?;
    let template = template
        .map(read_regular_utf8)
        .transpose()
        .map_err(CliOperationError::Input)?;
    prepare_inspection_request(config.as_deref(), template.as_deref())
        .map_err(CliOperationError::Preparation)
}

#[derive(Debug)]
enum CliOperationError {
    Input(InputError),
    Preparation(InspectionPreparationError),
    Operation(CpuTemplateOperationError),
    Publication(PublicationError),
}

impl fmt::Display for CliOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(source) => write!(formatter, "{source}"),
            Self::Preparation(source) => write!(formatter, "{source}"),
            Self::Operation(source) => write!(formatter, "{source}"),
            Self::Publication(source) => write!(formatter, "{source}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::cpu::arm64_cpu_template_register_descriptors;

    use super::*;
    use crate::profile::{
        ArmCpuTemplateValue, EffectiveCpuTemplateProfile, EffectiveCpuTemplateProfileEntry,
        EffectiveProfileProviderError, EffectiveRegisterStatus,
    };

    #[derive(Debug)]
    struct FakeProvider {
        profile: EffectiveCpuTemplateProfile,
        error: Option<EffectiveProfileProviderError>,
        calls: usize,
    }

    impl EffectiveCpuTemplateProvider for FakeProvider {
        fn inspect(
            &mut self,
            _: &crate::projection::PreparedCpuTemplateInspection,
        ) -> Result<EffectiveCpuTemplateProfile, EffectiveProfileProviderError> {
            self.calls += 1;
            self.error.map_or_else(|| Ok(self.profile.clone()), Err)
        }
    }

    fn baseline_profile() -> EffectiveCpuTemplateProfile {
        let entries = arm64_cpu_template_register_descriptors()
            .map(|descriptor| {
                let value = match descriptor.width() {
                    bangbang_runtime::cpu::CpuConfigArmRegisterWidth::U32 => {
                        ArmCpuTemplateValue::U32(0)
                    }
                    bangbang_runtime::cpu::CpuConfigArmRegisterWidth::U64 => {
                        ArmCpuTemplateValue::U64(0)
                    }
                    bangbang_runtime::cpu::CpuConfigArmRegisterWidth::U128 => {
                        ArmCpuTemplateValue::U128(0)
                    }
                };
                EffectiveCpuTemplateProfileEntry::available(descriptor.identity(), value)
            })
            .collect();
        EffectiveCpuTemplateProfile::try_new(entries).expect("fixture profile should validate")
    }

    #[test]
    fn help_and_invalid_invocation_use_only_their_reserved_streams() {
        let mut provider = FakeProvider {
            profile: baseline_profile(),
            error: None,
            calls: 0,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_cli_with_provider(
                ["cpu-template-helper", "--help"],
                &mut provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::Success
        );
        assert!(String::from_utf8_lossy(&stdout).contains("template"));
        assert!(stderr.is_empty());
        assert_eq!(provider.calls, 0);

        stdout.clear();
        stderr.clear();
        assert_eq!(
            run_cli_with_provider(
                ["cpu-template-helper", "--private-path", "/secret/value"],
                &mut provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::InvocationFailure
        );
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            format!("{INVOCATION_ERROR}\n")
        );
        assert!(!String::from_utf8_lossy(&stderr).contains("secret"));
    }

    #[test]
    fn verify_without_a_template_is_silent_on_stdout_and_does_not_inspect() {
        let mut provider = FakeProvider {
            profile: baseline_profile(),
            error: None,
            calls: 0,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_cli_with_provider(
                ["cpu-template-helper", "template", "verify"],
                &mut provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::OperationalFailure
        );
        assert!(stdout.is_empty());
        assert_eq!(provider.calls, 0);
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            "cpu-template-helper: no custom CPU template was selected\n"
        );
    }

    #[test]
    fn provider_failures_are_bounded_and_value_free() {
        let mut provider = FakeProvider {
            profile: baseline_profile(),
            error: Some(EffectiveProfileProviderError::Capture),
            calls: 0,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_cli_with_provider(
                ["cpu-template-helper", "template", "dump"],
                &mut provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::OperationalFailure
        );
        assert!(stdout.is_empty());
        assert_eq!(provider.calls, 1);
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            "cpu-template-helper: effective CPU inspection failed\n"
        );
    }

    #[test]
    fn status_debug_never_exposes_effective_values() {
        let status = EffectiveRegisterStatus::Available(ArmCpuTemplateValue::U64(u64::MAX));
        assert_eq!(format!("{status:?}"), "Available(\"<redacted>\")");
    }
}
