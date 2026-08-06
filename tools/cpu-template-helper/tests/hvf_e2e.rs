// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bangbang_api::config::parse_cpu_config_document;
use bangbang_cpu_template_helper::HelperExitClass;
use bangbang_cpu_template_helper::cli::run_cli_with_provider;
use bangbang_cpu_template_helper::document::decode_cpu_template_document;
use bangbang_cpu_template_helper::profile::{
    ArmCpuTemplateValue, EffectiveCpuTemplateProfile, EffectiveCpuTemplateProfileEntry,
    EffectiveCpuTemplateProvider, EffectiveProfileProviderError, EffectiveRegisterStatus,
};
use bangbang_cpu_template_helper::projection::{
    PreparedCpuTemplateInspection, prepare_inspection_request,
};
use bangbang_cpu_template_helper::provider::HvfEffectiveCpuTemplateProvider;
use bangbang_runtime::cpu::{
    KVM_REG_ARM64_ACTLR_EL1, KVM_REG_ARM64_CORE_FPCR, KVM_REG_ARM64_CORE_PC,
    KVM_REG_ARM64_CORE_PSTATE, KVM_REG_ARM64_CORE_SP_EL0, KVM_REG_ARM64_ID_AA64SMFR0_EL1,
    KVM_REG_ARM64_ID_AA64ZFR0_EL1, kvm_reg_arm64_core_q, kvm_reg_arm64_core_x,
};

const CHILD_TIMEOUT: Duration = Duration::from_secs(30);
static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should follow epoch")
            .as_nanos();
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-cpu-template-helper-hvf-{label}-{}-{nonce}-{sequence}",
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

fn signed_helper() -> PathBuf {
    required_executable("BANGBANG_CPU_TEMPLATE_HELPER_E2E_BIN")
}

fn unsigned_helper() -> PathBuf {
    required_executable("BANGBANG_CPU_TEMPLATE_HELPER_E2E_UNSIGNED_BIN")
}

fn required_executable(name: &str) -> PathBuf {
    let value = std::env::var_os(name).unwrap_or_else(|| panic!("{name} must be set"));
    let path = PathBuf::from(value);
    assert!(path.is_file(), "configured helper executable must exist");
    path
}

fn run_with_timeout(command: &mut Command) -> Output {
    let mut child = command.spawn().expect("helper process should spawn");
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child
            .try_wait()
            .expect("helper process should be observable")
        {
            Some(_) => {
                return child
                    .wait_with_output()
                    .expect("helper output should collect");
            }
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                let _ = child.wait();
                panic!("helper process exceeded its bounded deadline")
            }
        }
    }
}

fn write_config(path: &Path, vcpu_count: u8, cpu_config: Option<&str>) {
    let cpu_section = cpu_config
        .map(|cpu| format!(",\"cpu-config\":{cpu}"))
        .unwrap_or_default();
    fs::write(
        path,
        format!(
            "{{\"boot-source\":{{\"kernel_image_path\":\"/not-opened-by-helper\"}},\"machine-config\":{{\"vcpu_count\":{vcpu_count},\"mem_size_mib\":128}}{cpu_section}}}"
        ),
    )
    .expect("config fixture should be written");
}

fn modifier(identity: u64, width: usize, low_bit: Option<bool>) -> String {
    let mut bits = vec!['x'; width];
    if let Some(value) = low_bit {
        bits[width - 1] = if value { '1' } else { '0' };
    }
    format!(
        "{{\"addr\":\"0x{identity:016x}\",\"bitmap\":\"0b{}\"}}",
        bits.into_iter().collect::<String>()
    )
}

fn template(modifiers: &[String]) -> String {
    format!(
        "{{\"kvm_capabilities\":[],\"reg_modifiers\":[{}],\"vcpu_features\":[]}}",
        modifiers.join(",")
    )
}

fn assert_silent_success(output: Output) {
    assert!(
        output.status.success(),
        "signed helper operation should succeed"
    );
    assert!(output.stdout.is_empty(), "operation stdout must be empty");
    assert!(output.stderr.is_empty(), "operation stderr must be empty");
}

#[test]
fn signed_two_vcpu_default_dump_is_canonical_private_and_reparseable() {
    let directory = TestDirectory::new("default-dump");
    let config = directory.0.join("config.json");
    let output = directory.0.join("effective.json");
    write_config(&config, 2, None);

    let mut command = Command::new(signed_helper());
    command
        .args(["template", "dump", "--config"])
        .arg(&config)
        .args(["--output"])
        .arg(&output)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_silent_success(run_with_timeout(&mut command));

    let metadata = fs::metadata(&output).expect("dump output should exist");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let contents = fs::read_to_string(&output).expect("dump should be UTF-8");
    let document = decode_cpu_template_document(&contents).expect("dump should strictly reparse");
    parse_cpu_config_document(&contents).expect("dump should re-enter the public CPU parser");
    assert_eq!(
        document.canonical_bytes().expect("dump should re-encode"),
        contents.as_bytes()
    );
    assert!((74..=77).contains(&document.modifiers().len()));
    let x0 = kvm_reg_arm64_core_x(0).expect("X0 identity should exist");
    for excluded in [x0, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE] {
        assert!(
            document
                .modifiers()
                .iter()
                .all(|entry| entry.identity() != excluded),
            "boot-owned target must be excluded"
        );
    }
}

#[test]
fn signed_mixed_width_verify_and_explicit_precedence_use_real_hvf_state() {
    let directory = TestDirectory::new("mixed-width");
    let q0 = kvm_reg_arm64_core_q(0).expect("Q0 identity should exist");
    let mixed = template(&[
        modifier(KVM_REG_ARM64_CORE_FPCR, 32, None),
        modifier(KVM_REG_ARM64_CORE_SP_EL0, 64, None),
        modifier(q0, 128, None),
    ]);
    let mixed_path = directory.0.join("mixed.json");
    fs::write(&mixed_path, mixed).expect("mixed template should be written");
    let mut command = Command::new(signed_helper());
    command
        .args(["template", "verify", "--template"])
        .arg(&mixed_path)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_silent_success(run_with_timeout(&mut command));

    let config_template = template(&[modifier(KVM_REG_ARM64_CORE_SP_EL0, 64, Some(false))]);
    let config = directory.0.join("config.json");
    write_config(&config, 2, Some(&config_template));
    let explicit = directory.0.join("explicit.json");
    fs::write(
        &explicit,
        template(&[modifier(KVM_REG_ARM64_CORE_SP_EL0, 64, Some(true))]),
    )
    .expect("explicit template should be written");
    let output = directory.0.join("precedence.json");
    let mut command = Command::new(signed_helper());
    command
        .args(["template", "dump", "--config"])
        .arg(&config)
        .args(["--template"])
        .arg(&explicit)
        .args(["--output"])
        .arg(&output)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_silent_success(run_with_timeout(&mut command));
    let contents = fs::read_to_string(&output).expect("precedence output should exist");
    let document = decode_cpu_template_document(&contents).expect("output should reparse");
    let selected = document
        .modifiers()
        .iter()
        .find(|entry| entry.identity() == KVM_REG_ARM64_CORE_SP_EL0)
        .expect("SP_EL0 should be dumped");
    assert!(selected.value() & 1 == 1, "explicit template must win");
}

#[test]
fn signed_optional_register_outcomes_are_asserted_without_skips() {
    let directory = TestDirectory::new("optional");
    let request = prepare_inspection_request(None, None).expect("default request should prepare");
    let mut provider = HvfEffectiveCpuTemplateProvider::new();
    let profile = provider
        .inspect(&request)
        .expect("signed real profile should inspect");

    let output = directory.0.join("default.json");
    let mut command = Command::new(signed_helper());
    command
        .args(["template", "dump", "--output"])
        .arg(&output)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_silent_success(run_with_timeout(&mut command));
    let contents = fs::read_to_string(&output).expect("default dump should exist");
    let dump = decode_cpu_template_document(&contents).expect("default dump should reparse");

    for (identity, width) in [
        (KVM_REG_ARM64_ACTLR_EL1, 64),
        (KVM_REG_ARM64_ID_AA64ZFR0_EL1, 64),
        (KVM_REG_ARM64_ID_AA64SMFR0_EL1, 64),
    ] {
        let status = profile
            .entries()
            .iter()
            .copied()
            .find(|entry| entry.identity() == identity)
            .expect("optional descriptor must have an explicit status")
            .status();
        let dumped = dump
            .modifiers()
            .iter()
            .any(|entry| entry.identity() == identity);
        assert_eq!(
            dumped,
            matches!(status, EffectiveRegisterStatus::Available(_))
        );

        let path = directory.0.join(format!("optional-{identity}.json"));
        fs::write(&path, template(&[modifier(identity, width, None)]))
            .expect("optional template should be written");
        let mut command = Command::new(signed_helper());
        command
            .args(["template", "verify", "--template"])
            .arg(&path)
            .current_dir(&directory.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let result = run_with_timeout(&mut command);
        assert!(result.stdout.is_empty());
        match status {
            EffectiveRegisterStatus::Available(_) => {
                assert!(result.status.success());
                assert!(result.stderr.is_empty());
            }
            EffectiveRegisterStatus::Unavailable => {
                assert_eq!(result.status.code(), Some(1));
                assert_eq!(
                    String::from_utf8(result.stderr).expect("stderr should be UTF-8"),
                    "cpu-template-helper: effective CPU inspection failed\n"
                );
            }
        }
    }
}

#[derive(Debug, Default)]
struct MismatchAfterRealCapture {
    inner: HvfEffectiveCpuTemplateProvider,
}

impl EffectiveCpuTemplateProvider for MismatchAfterRealCapture {
    fn inspect(
        &mut self,
        request: &PreparedCpuTemplateInspection,
    ) -> Result<EffectiveCpuTemplateProfile, EffectiveProfileProviderError> {
        let profile = self.inner.inspect(request)?;
        let entries = profile
            .entries()
            .iter()
            .copied()
            .map(|entry| {
                if entry.identity() != KVM_REG_ARM64_CORE_SP_EL0 {
                    return entry;
                }
                let EffectiveRegisterStatus::Available(ArmCpuTemplateValue::U64(value)) =
                    entry.status()
                else {
                    panic!("SP_EL0 must be an available U64 entry")
                };
                EffectiveCpuTemplateProfileEntry::available(
                    entry.identity(),
                    ArmCpuTemplateValue::U64(value ^ 1),
                )
            })
            .collect();
        EffectiveCpuTemplateProfile::try_new(entries)
            .map_err(|_| EffectiveProfileProviderError::Capture)
    }
}

#[test]
fn signed_mismatch_collision_and_unsigned_failures_leave_resources_reusable() {
    let directory = TestDirectory::new("failures-reuse");
    let template_path = directory.0.join("verify.json");
    fs::write(
        &template_path,
        template(&[modifier(KVM_REG_ARM64_CORE_SP_EL0, 64, Some(true))]),
    )
    .expect("verify template should be written");

    let mut mismatch = MismatchAfterRealCapture::default();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let template_arg = template_path.as_os_str().to_os_string();
    assert_eq!(
        run_cli_with_provider(
            [
                std::ffi::OsString::from("cpu-template-helper"),
                std::ffi::OsString::from("template"),
                std::ffi::OsString::from("verify"),
                std::ffi::OsString::from("--template"),
                template_arg,
            ],
            &mut mismatch,
            &mut stdout,
            &mut stderr,
        ),
        HelperExitClass::OperationalFailure
    );
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr).expect("stderr should be UTF-8"),
        "cpu-template-helper: effective CPU-template verification failed\n"
    );

    let collision = directory.0.join("collision.json");
    fs::write(&collision, "preserve-me").expect("collision fixture should be written");
    let mut real = HvfEffectiveCpuTemplateProvider::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        run_cli_with_provider(
            [
                std::ffi::OsString::from("cpu-template-helper"),
                std::ffi::OsString::from("template"),
                std::ffi::OsString::from("dump"),
                std::ffi::OsString::from("--output"),
                collision.as_os_str().to_os_string(),
            ],
            &mut real,
            &mut stdout,
            &mut stderr,
        ),
        HelperExitClass::OperationalFailure
    );
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr).expect("stderr should be UTF-8"),
        "cpu-template-helper: output target already exists\n"
    );
    assert_eq!(
        fs::read_to_string(&collision).expect("collision bytes should remain"),
        "preserve-me"
    );

    let retry = directory.0.join("retry.json");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        run_cli_with_provider(
            [
                std::ffi::OsString::from("cpu-template-helper"),
                std::ffi::OsString::from("template"),
                std::ffi::OsString::from("dump"),
                std::ffi::OsString::from("--output"),
                retry.as_os_str().to_os_string(),
            ],
            &mut real,
            &mut stdout,
            &mut stderr,
        ),
        HelperExitClass::Success
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(retry.is_file());

    let unsigned_output = directory.0.join("unsigned.json");
    let mut command = Command::new(unsigned_helper());
    command
        .args(["template", "dump", "--output"])
        .arg(&unsigned_output)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = run_with_timeout(&mut command);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(
        String::from_utf8(result.stderr).expect("stderr should be UTF-8"),
        "cpu-template-helper: effective CPU inspection failed\n"
    );
    assert!(!unsigned_output.exists());
}

#[test]
fn signed_boot_owned_modifiers_verify_only_at_application_checkpoint_and_stay_undumped() {
    let directory = TestDirectory::new("boot-owned");
    let path = directory.0.join("boot-owned.json");
    fs::write(
        &path,
        template(&[
            modifier(
                kvm_reg_arm64_core_x(0).expect("X0 identity should exist"),
                64,
                None,
            ),
            modifier(KVM_REG_ARM64_CORE_PC, 64, None),
            modifier(KVM_REG_ARM64_CORE_PSTATE, 64, None),
        ]),
    )
    .expect("boot-owned template should be written");

    let mut command = Command::new(signed_helper());
    command
        .args(["template", "verify", "--template"])
        .arg(&path)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_silent_success(run_with_timeout(&mut command));

    let output = directory.0.join("boot-owned-dump.json");
    let mut command = Command::new(signed_helper());
    command
        .args(["template", "dump", "--template"])
        .arg(&path)
        .args(["--output"])
        .arg(&output)
        .current_dir(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_silent_success(run_with_timeout(&mut command));
    let contents = fs::read_to_string(&output).expect("dump should exist");
    let document = decode_cpu_template_document(&contents).expect("dump should reparse");
    let x0 = kvm_reg_arm64_core_x(0).expect("X0 identity should exist");
    for excluded in [x0, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE] {
        assert!(
            document
                .modifiers()
                .iter()
                .all(|entry| entry.identity() != excluded),
            "boot-owned target must stay excluded"
        );
    }
}
