//! Strict public CPU-template helper command runner.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{ColorChoice, Parser, Subcommand};

use crate::HelperExitClass;
use crate::document::{
    CpuTemplateDocumentError, CpuTemplateEncodeError, decode_cpu_template_document,
};
use crate::fingerprint::{
    CpuFingerprintDocumentError, CpuFingerprintOperationError, HostFingerprint,
    HostFingerprintProvider, HostFingerprintProviderError, decode_cpu_fingerprint_document,
    dump_with_providers as dump_fingerprint_with_providers,
};
use crate::fingerprint_compare::{
    CpuFingerprintCompareError, CpuFingerprintCompareOutcome, CpuFingerprintField,
    CpuFingerprintFilterSelection, compare_cpu_fingerprints,
};
use crate::input::{
    InputError, StripInputError, prepare_strip_input, read_regular_utf8,
    validate_prepared_strip_inputs,
};
use crate::profile::{
    CpuTemplateOperationError, EffectiveCpuTemplateProvider, dump_with_provider,
    verify_with_provider,
};
use crate::projection::{InspectionPreparationError, prepare_inspection_request};
use crate::publication::{PublicationError, publish_new_artifact};
use crate::strip::{CpuTemplateStripError, strip_cpu_template_documents};
use crate::strip_publication::{StripPublicationError, publish_strip_artifacts};

const INVOCATION_ERROR: &str =
    "cpu-template-helper: invalid arguments; use --help for the supported interface";

#[derive(Debug, Parser)]
#[command(
    name = "cpu-template-helper",
    about = "Inspect and transform arm64 CPU templates",
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
    /// Fingerprint-related operations.
    #[command(subcommand)]
    Fingerprint(FingerprintOperation),
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
    /// Strip entries shared between multiple CPU template files.
    Strip {
        /// Paths of input CPU template documents.
        #[arg(short, long, value_name = "PATH", num_args = 2..)]
        paths: Vec<PathBuf>,
        /// Suffix for outputs; an empty value replaces each input.
        #[arg(short, long, default_value = "_stripped")]
        suffix: String,
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

#[derive(Debug, Subcommand)]
enum FingerprintOperation {
    /// Dump host and effective guest CPU change-awareness facts.
    Dump {
        /// Path of a Firecracker-shaped configuration document.
        #[arg(short, long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Path of an explicit CPU template applied after the configuration.
        #[arg(short, long, value_name = "PATH")]
        template: Option<PathBuf>,
        /// Absent output path to publish.
        #[arg(short, long, value_name = "PATH", default_value = "fingerprint.json")]
        output: PathBuf,
    },
    /// Compare two persisted fingerprints with optional field selection.
    Compare {
        /// Previous CPU-fingerprint document.
        #[arg(short, long, value_name = "PATH")]
        prev: PathBuf,
        /// Current CPU-fingerprint document.
        #[arg(short, long, value_name = "PATH")]
        curr: PathBuf,
        /// Fields to compare; absence selects every applicable field.
        #[arg(short, long, value_enum, value_name = "FIELD", num_args = 1..)]
        filters: Option<Vec<CpuFingerprintField>>,
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
    run_cli_with_providers(
        args,
        &mut UnsupportedHostFingerprintProvider,
        provider,
        stdout,
        stderr,
    )
}

/// Run one complete public command with explicitly supplied host/effective
/// providers and output streams.
pub fn run_cli_with_providers<I, T>(
    args: I,
    host_provider: &mut impl HostFingerprintProvider,
    effective_provider: &mut impl EffectiveCpuTemplateProvider,
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

    if !filter_arguments_are_valid(&cli) {
        let _ = writeln!(stderr, "{INVOCATION_ERROR}");
        return HelperExitClass::InvocationFailure;
    }

    match execute(cli, host_provider, effective_provider) {
        Ok(CliExecutionOutcome::Complete) => HelperExitClass::Success,
        Ok(CliExecutionOutcome::Differences(bytes)) => {
            let _ = stderr.write_all(&bytes);
            HelperExitClass::OperationalFailure
        }
        Err(error) => {
            let _ = writeln!(stderr, "cpu-template-helper: {error}");
            HelperExitClass::OperationalFailure
        }
    }
}

fn execute(
    cli: Cli,
    host_provider: &mut impl HostFingerprintProvider,
    effective_provider: &mut impl EffectiveCpuTemplateProvider,
) -> Result<CliExecutionOutcome, CliOperationError> {
    match cli.command {
        Command::Template(TemplateOperation::Dump {
            config,
            template,
            output,
        }) => {
            let request = prepare_from_paths(config.as_deref(), template.as_deref())?;
            let bytes = dump_with_provider(effective_provider, &request)
                .map_err(CliOperationError::Operation)?;
            publish_new_artifact(&output, &bytes).map_err(CliOperationError::Publication)?;
            Ok(CliExecutionOutcome::Complete)
        }
        Command::Template(TemplateOperation::Strip { paths, suffix }) => {
            let mut prepared_inputs = Vec::with_capacity(paths.len());
            let mut documents = Vec::with_capacity(paths.len());
            for path in paths {
                let (prepared, contents) =
                    prepare_strip_input(&path, &suffix).map_err(CliOperationError::StripInput)?;
                let document = decode_cpu_template_document(&contents)
                    .map_err(CliOperationError::StripDocument)?;
                prepared_inputs.push(prepared);
                documents.push(document);
            }
            validate_prepared_strip_inputs(&prepared_inputs)
                .map_err(CliOperationError::StripInput)?;
            let outputs = strip_cpu_template_documents(documents)
                .map_err(CliOperationError::StripTransform)?;
            let artifacts = outputs
                .iter()
                .map(|output| output.canonical_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map_err(CliOperationError::StripEncoding)?;
            publish_strip_artifacts(prepared_inputs, artifacts)
                .map_err(CliOperationError::StripPublication)?;
            Ok(CliExecutionOutcome::Complete)
        }
        Command::Template(TemplateOperation::Verify { config, template }) => {
            let request = prepare_from_paths(config.as_deref(), template.as_deref())?;
            verify_with_provider(effective_provider, &request)
                .map_err(CliOperationError::Operation)?;
            Ok(CliExecutionOutcome::Complete)
        }
        Command::Fingerprint(FingerprintOperation::Dump {
            config,
            template,
            output,
        }) => {
            let request = prepare_from_paths(config.as_deref(), template.as_deref())?;
            let bytes =
                dump_fingerprint_with_providers(host_provider, effective_provider, &request)
                    .map_err(CliOperationError::Fingerprint)?;
            publish_new_artifact(&output, &bytes).map_err(CliOperationError::Publication)?;
            Ok(CliExecutionOutcome::Complete)
        }
        Command::Fingerprint(FingerprintOperation::Compare {
            prev,
            curr,
            filters,
        }) => {
            let filters = filters.map_or_else(
                || Ok(CpuFingerprintFilterSelection::all_applicable()),
                |filters| {
                    CpuFingerprintFilterSelection::explicit(filters)
                        .map_err(CliOperationError::FingerprintFilter)
                },
            )?;
            let prev = read_regular_utf8(&prev).map_err(CliOperationError::Input)?;
            let prev = decode_cpu_fingerprint_document(&prev)
                .map_err(CliOperationError::FingerprintDocument)?;
            let curr = read_regular_utf8(&curr).map_err(CliOperationError::Input)?;
            let curr = decode_cpu_fingerprint_document(&curr)
                .map_err(CliOperationError::FingerprintDocument)?;
            match compare_cpu_fingerprints(&prev, &curr, &filters)
                .map_err(CliOperationError::FingerprintCompare)?
            {
                CpuFingerprintCompareOutcome::Equal => Ok(CliExecutionOutcome::Complete),
                CpuFingerprintCompareOutcome::Different(bytes) => {
                    Ok(CliExecutionOutcome::Differences(bytes))
                }
            }
        }
    }
}

fn filter_arguments_are_valid(cli: &Cli) -> bool {
    let Command::Fingerprint(FingerprintOperation::Compare {
        filters: Some(filters),
        ..
    }) = &cli.command
    else {
        return true;
    };
    CpuFingerprintFilterSelection::explicit(filters.clone()).is_ok()
}

enum CliExecutionOutcome {
    Complete,
    Differences(Vec<u8>),
}

#[derive(Debug)]
struct UnsupportedHostFingerprintProvider;

impl HostFingerprintProvider for UnsupportedHostFingerprintProvider {
    fn capture(&mut self) -> Result<HostFingerprint, HostFingerprintProviderError> {
        Err(HostFingerprintProviderError::Unsupported)
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
    Fingerprint(CpuFingerprintOperationError),
    FingerprintCompare(CpuFingerprintCompareError),
    FingerprintDocument(CpuFingerprintDocumentError),
    FingerprintFilter(crate::fingerprint_compare::CpuFingerprintFilterError),
    Input(InputError),
    StripInput(StripInputError),
    StripDocument(CpuTemplateDocumentError),
    StripTransform(CpuTemplateStripError),
    StripEncoding(CpuTemplateEncodeError),
    StripPublication(StripPublicationError),
    Preparation(InspectionPreparationError),
    Operation(CpuTemplateOperationError),
    Publication(PublicationError),
}

impl fmt::Display for CliOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fingerprint(source) => write!(formatter, "{source}"),
            Self::FingerprintCompare(source) => write!(formatter, "{source}"),
            Self::FingerprintDocument(source) => write!(formatter, "{source}"),
            Self::FingerprintFilter(source) => write!(formatter, "{source}"),
            Self::Input(source) => write!(formatter, "{source}"),
            Self::StripInput(source) => write!(formatter, "{source}"),
            Self::StripDocument(source) => write!(formatter, "{source}"),
            Self::StripTransform(source) => write!(formatter, "{source}"),
            Self::StripEncoding(source) => write!(formatter, "{source}"),
            Self::StripPublication(source) => write!(formatter, "{source}"),
            Self::Preparation(source) => write!(formatter, "{source}"),
            Self::Operation(source) => write!(formatter, "{source}"),
            Self::Publication(source) => write!(formatter, "{source}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use bangbang_runtime::cpu::arm64_cpu_template_register_descriptors;

    use super::*;
    use crate::fingerprint::{
        CpuFingerprintDocument, CpuFingerprintPlatform, HostFingerprint,
        decode_cpu_fingerprint_document,
    };
    use crate::profile::{
        ArmCpuTemplateValue, EffectiveCpuTemplateProfile, EffectiveCpuTemplateProfileEntry,
        EffectiveProfileProviderError, EffectiveRegisterStatus,
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    const EMPTY_TEMPLATE: &str =
        "{\"kvm_capabilities\":[],\"reg_modifiers\":[],\"vcpu_features\":[]}";

    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should follow epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-cpu-template-cli-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug)]
    struct FakeProvider {
        profile: EffectiveCpuTemplateProfile,
        error: Option<EffectiveProfileProviderError>,
        calls: usize,
        events: Option<Rc<RefCell<Vec<&'static str>>>>,
    }

    impl EffectiveCpuTemplateProvider for FakeProvider {
        fn inspect(
            &mut self,
            _: &crate::projection::PreparedCpuTemplateInspection,
        ) -> Result<EffectiveCpuTemplateProfile, EffectiveProfileProviderError> {
            self.calls += 1;
            if let Some(events) = &self.events {
                events.borrow_mut().push("effective");
            }
            self.error.map_or_else(|| Ok(self.profile.clone()), Err)
        }
    }

    #[derive(Debug)]
    struct FakeHostProvider {
        host: HostFingerprint,
        error: Option<HostFingerprintProviderError>,
        calls: usize,
        events: Option<Rc<RefCell<Vec<&'static str>>>>,
    }

    impl HostFingerprintProvider for FakeHostProvider {
        fn capture(&mut self) -> Result<HostFingerprint, HostFingerprintProviderError> {
            self.calls += 1;
            if let Some(events) = &self.events {
                events.borrow_mut().push("host");
            }
            self.error.map_or_else(|| Ok(self.host.clone()), Err)
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

    fn baseline_host() -> HostFingerprint {
        HostFingerprint::try_macos(
            "Darwin".to_owned(),
            "25.5.0".to_owned(),
            "arm64".to_owned(),
            Some("Mac16,1".to_owned()),
            Some("J475cAP".to_owned()),
            Some(0x1b588bb3),
        )
        .expect("fixture host should validate")
    }

    fn write_macos_fingerprint(path: &Path, release: &str, product: Option<&str>) {
        let host = HostFingerprint::try_macos(
            "Darwin".to_owned(),
            release.to_owned(),
            "arm64".to_owned(),
            product.map(str::to_owned),
            None,
            None,
        )
        .expect("fixture host should validate");
        let guest =
            decode_cpu_template_document(EMPTY_TEMPLATE).expect("empty template should decode");
        let bytes = CpuFingerprintDocument::new_current(host, guest)
            .expect("fingerprint should construct")
            .canonical_bytes()
            .expect("fingerprint should encode");
        fs::write(path, bytes).expect("fingerprint fixture should be written");
    }

    #[test]
    fn help_and_invalid_invocation_use_only_their_reserved_streams() {
        let mut provider = FakeProvider {
            profile: baseline_profile(),
            error: None,
            calls: 0,
            events: None,
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
        assert!(String::from_utf8_lossy(&stdout).contains("fingerprint"));
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
            events: None,
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
            events: None,
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
    fn strip_is_silent_and_never_constructs_an_effective_provider_request() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let second = directory.0.join("second.json");
        fs::write(&first, EMPTY_TEMPLATE).expect("first template should be written");
        fs::write(&second, EMPTY_TEMPLATE).expect("second template should be written");
        let mut provider = FakeProvider {
            profile: baseline_profile(),
            error: Some(EffectiveProfileProviderError::Capture),
            calls: 0,
            events: None,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let args = vec![
            OsString::from("cpu-template-helper"),
            OsString::from("template"),
            OsString::from("strip"),
            OsString::from("--paths"),
            first.into_os_string(),
            second.into_os_string(),
        ];

        assert_eq!(
            run_cli_with_provider(args, &mut provider, &mut stdout, &mut stderr),
            HelperExitClass::Success
        );
        assert_eq!(provider.calls, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(directory.0.join("first_stripped.json").is_file());
        assert!(directory.0.join("second_stripped.json").is_file());
    }

    #[test]
    fn status_debug_never_exposes_effective_values() {
        let status = EffectiveRegisterStatus::Available(ArmCpuTemplateValue::U64(u64::MAX));
        assert_eq!(format!("{status:?}"), "Available(\"<redacted>\")");
    }

    #[test]
    fn fingerprint_dump_captures_host_then_effective_and_publishes_strict_bytes() {
        let directory = TestDirectory::new();
        let output = directory.0.join("fingerprint.json");
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut host_provider = FakeHostProvider {
            host: baseline_host(),
            error: None,
            calls: 0,
            events: Some(Rc::clone(&events)),
        };
        let mut effective_provider = FakeProvider {
            profile: baseline_profile(),
            error: None,
            calls: 0,
            events: Some(Rc::clone(&events)),
        };
        let args = vec![
            OsString::from("cpu-template-helper"),
            OsString::from("fingerprint"),
            OsString::from("dump"),
            OsString::from("-o"),
            output.clone().into_os_string(),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(
            run_cli_with_providers(
                args,
                &mut host_provider,
                &mut effective_provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::Success
        );
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(host_provider.calls, 1);
        assert_eq!(effective_provider.calls, 1);
        assert_eq!(*events.borrow(), ["host", "effective"]);

        let contents = fs::read_to_string(output).expect("fingerprint should be published");
        let document =
            decode_cpu_fingerprint_document(&contents).expect("fingerprint should decode");
        assert_eq!(document.host().platform(), CpuFingerprintPlatform::Macos);
        assert_eq!(
            document.canonical_bytes().as_deref(),
            Ok(contents.as_bytes())
        );
    }

    #[test]
    fn fingerprint_host_failure_stops_before_effective_capture_and_publication() {
        let directory = TestDirectory::new();
        let output = directory.0.join("must-not-exist.json");
        let mut host_provider = FakeHostProvider {
            host: baseline_host(),
            error: Some(HostFingerprintProviderError::Product),
            calls: 0,
            events: None,
        };
        let mut effective_provider = FakeProvider {
            profile: baseline_profile(),
            error: None,
            calls: 0,
            events: None,
        };
        let args = vec![
            OsString::from("cpu-template-helper"),
            OsString::from("fingerprint"),
            OsString::from("dump"),
            OsString::from("--output"),
            output.clone().into_os_string(),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(
            run_cli_with_providers(
                args,
                &mut host_provider,
                &mut effective_provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::OperationalFailure
        );
        assert_eq!(host_provider.calls, 1);
        assert_eq!(effective_provider.calls, 0);
        assert!(!output.exists());
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            "cpu-template-helper: host fingerprint capture failed\n"
        );
        assert!(!String::from_utf8_lossy(&stderr).contains("product"));
    }

    #[test]
    fn fingerprint_compare_is_ordered_portable_and_provider_free() {
        let directory = TestDirectory::new();
        let prev = directory.0.join("previous.json");
        let curr = directory.0.join("current.json");
        write_macos_fingerprint(&prev, "25.4.0", None);
        write_macos_fingerprint(&curr, "25.5.0", Some("Mac16,1"));
        let mut host_provider = FakeHostProvider {
            host: baseline_host(),
            error: Some(HostFingerprintProviderError::Product),
            calls: 0,
            events: None,
        };
        let mut effective_provider = FakeProvider {
            profile: baseline_profile(),
            error: Some(EffectiveProfileProviderError::Capture),
            calls: 0,
            events: None,
        };
        let args = vec![
            OsString::from("cpu-template-helper"),
            OsString::from("fingerprint"),
            OsString::from("compare"),
            OsString::from("--prev"),
            prev.clone().into_os_string(),
            OsString::from("--curr"),
            curr.clone().into_os_string(),
            OsString::from("--filters"),
            OsString::from("macos_product"),
            OsString::from("kernel_release"),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(
            run_cli_with_providers(
                args,
                &mut host_provider,
                &mut effective_provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::OperationalFailure
        );
        assert!(stdout.is_empty());
        let diagnostic = String::from_utf8(stderr).expect("diagnostic should be UTF-8");
        let kernel = diagnostic
            .find("kernel_release")
            .expect("kernel difference should be present");
        let product = diagnostic
            .find("macos_product")
            .expect("product difference should be present");
        assert!(kernel < product);
        assert!(diagnostic.ends_with("}\n"));
        assert_eq!(host_provider.calls, 0);
        assert_eq!(effective_provider.calls, 0);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let args = vec![
            OsString::from("cpu-template-helper"),
            OsString::from("fingerprint"),
            OsString::from("compare"),
            OsString::from("-p"),
            prev.into_os_string(),
            OsString::from("-c"),
            curr.into_os_string(),
            OsString::from("-f"),
            OsString::from("producer_version"),
        ];
        assert_eq!(
            run_cli_with_providers(
                args,
                &mut host_provider,
                &mut effective_provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::Success
        );
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(host_provider.calls, 0);
        assert_eq!(effective_provider.calls, 0);
    }

    #[test]
    fn duplicate_compare_filters_fail_as_invocation_before_path_or_provider_access() {
        let mut host_provider = FakeHostProvider {
            host: baseline_host(),
            error: Some(HostFingerprintProviderError::Product),
            calls: 0,
            events: None,
        };
        let mut effective_provider = FakeProvider {
            profile: baseline_profile(),
            error: Some(EffectiveProfileProviderError::Capture),
            calls: 0,
            events: None,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_cli_with_providers(
                [
                    "cpu-template-helper",
                    "fingerprint",
                    "compare",
                    "--prev",
                    "private-missing-prev",
                    "--curr",
                    "private-missing-curr",
                    "--filters",
                    "kernel_release",
                    "kernel_release",
                ],
                &mut host_provider,
                &mut effective_provider,
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
        assert!(!String::from_utf8_lossy(&stderr).contains("private"));
        assert_eq!(host_provider.calls, 0);
        assert_eq!(effective_provider.calls, 0);
    }

    #[test]
    fn compare_stream_failure_retains_the_difference_exit_without_provider_calls() {
        #[derive(Debug)]
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected write failure"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let directory = TestDirectory::new();
        let prev = directory.0.join("previous.json");
        let curr = directory.0.join("current.json");
        write_macos_fingerprint(&prev, "25.4.0", None);
        write_macos_fingerprint(&curr, "25.5.0", None);
        let mut host_provider = FakeHostProvider {
            host: baseline_host(),
            error: None,
            calls: 0,
            events: None,
        };
        let mut effective_provider = FakeProvider {
            profile: baseline_profile(),
            error: None,
            calls: 0,
            events: None,
        };
        let args = vec![
            OsString::from("cpu-template-helper"),
            OsString::from("fingerprint"),
            OsString::from("compare"),
            OsString::from("--prev"),
            prev.into_os_string(),
            OsString::from("--curr"),
            curr.into_os_string(),
        ];
        let mut stdout = Vec::new();
        let mut stderr = FailingWriter;
        assert_eq!(
            run_cli_with_providers(
                args,
                &mut host_provider,
                &mut effective_provider,
                &mut stdout,
                &mut stderr,
            ),
            HelperExitClass::OperationalFailure
        );
        assert!(stdout.is_empty());
        assert_eq!(host_provider.calls, 0);
        assert_eq!(effective_provider.calls, 0);
    }
}
