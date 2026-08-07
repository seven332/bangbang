#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bangbang_cpu_template_helper::fingerprint::decode_cpu_fingerprint_document;
use bangbang_runtime::cpu::KVM_REG_ARM64_CORE_FPCR;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const EMPTY_TEMPLATE: &str = "{\"kvm_capabilities\":[],\"reg_modifiers\":[],\"vcpu_features\":[]}";

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
            "bangbang-cpu-template-helper-cli-{}-{nonce}-{sequence}",
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

fn run(args: &[&str], current_dir: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"));
    command.args(args);
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    command.output().expect("helper process should execute")
}

fn json_optional_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| format!(r#""{value}""#))
}

fn macos_fingerprint(
    version: &str,
    release: &str,
    product: Option<&str>,
    target: Option<&str>,
    cpu_family: Option<&str>,
    guest: &str,
) -> String {
    let product = json_optional_string(product);
    let target = json_optional_string(target);
    let cpu_family = json_optional_string(cpu_family);
    let contents = format!(
        r#"{{
            "schema_version": 1,
            "producer": {{
                "name": "bangbang-cpu-template-helper",
                "version": "{version}",
                "firecracker_compatibility": "1.16.0"
            }},
            "kernel": {{
                "operating_system": "Darwin",
                "release": "{release}",
                "machine": "arm64"
            }},
            "host": {{
                "platform": "macos",
                "product": {product},
                "target": {target},
                "cpu_family": {cpu_family}
            }},
            "guest_cpu_config": {guest}
        }}"#,
    );
    decode_cpu_fingerprint_document(&contents).expect("macOS fixture should strictly decode");
    contents
}

fn linux_fingerprint(release: &str) -> String {
    let contents = format!(
        r#"{{
            "schema_version": 1,
            "producer": {{
                "name": "bangbang-cpu-template-helper",
                "version": "0.1.0",
                "firecracker_compatibility": "1.16.0"
            }},
            "kernel": {{
                "operating_system": "Linux",
                "release": "{release}",
                "machine": "aarch64"
            }},
            "host": {{
                "platform": "linux",
                "microcode_version": "r1",
                "bios_version": "v1",
                "bios_revision": "r1"
            }},
            "guest_cpu_config": {EMPTY_TEMPLATE}
        }}"#,
    );
    decode_cpu_fingerprint_document(&contents).expect("Linux fixture should strictly decode");
    contents
}

#[test]
fn help_and_version_are_the_only_portable_stdout_successes() {
    for args in [
        vec!["--help"],
        vec!["template", "--help"],
        vec!["template", "dump", "--help"],
        vec!["template", "strip", "--help"],
        vec!["template", "verify", "--help"],
        vec!["fingerprint", "--help"],
        vec!["fingerprint", "dump", "--help"],
        vec!["fingerprint", "compare", "--help"],
    ] {
        let output = run(&args, None);
        assert!(output.status.success(), "help should succeed: {args:?}");
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    let output = run(&["--version"], None);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            "cpu-template-helper ",
            env!("CARGO_PKG_VERSION"),
            " (bangbang; Firecracker v1.16.0-compatible command surface)\n"
        )
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn every_invalid_invocation_is_fixed_and_does_not_echo_values() {
    let expected =
        "cpu-template-helper: invalid arguments; use --help for the supported interface\n";
    for args in [
        vec![],
        vec!["private-command", "/private/value"],
        vec!["template"],
        vec!["template", "dump", "--unknown", "/private/value"],
        vec!["template", "dump", "--config"],
        vec!["template", "verify", "extra-private-value"],
        vec!["fingerprint"],
        vec!["fingerprint", "dump", "--output"],
        vec!["fingerprint", "dump", "--private", "/private/value"],
        vec!["fingerprint", "compare"],
        vec!["fingerprint", "compare", "--prev", "/private/value"],
        vec![
            "fingerprint",
            "compare",
            "--prev",
            "/private/prev",
            "--curr",
            "/private/curr",
            "--filters",
        ],
        vec![
            "fingerprint",
            "compare",
            "--prev",
            "/private/prev",
            "--curr",
            "/private/curr",
            "--filters",
            "private_field",
        ],
    ] {
        let output = run(&args, None);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty(), "args: {args:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), expected);
    }
}

#[test]
fn fingerprint_compare_help_locks_the_closed_filter_vocabulary() {
    let output = run(&["fingerprint", "compare", "--help"], None);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--prev <PATH>"));
    assert!(help.contains("--curr <PATH>"));
    assert!(help.contains("--filters <FIELD>..."));
    let expected = [
        "producer_version",
        "kernel_release",
        "macos_product",
        "macos_target",
        "macos_cpu_family",
        "linux_microcode_version",
        "linux_bios_version",
        "linux_bios_revision",
        "guest_cpu_config",
    ];
    let mut previous = None;
    for field in expected {
        assert_eq!(help.matches(field).count(), 1, "field: {field}");
        let offset = help.find(field).expect("filter should be documented");
        if let Some(previous) = previous {
            assert!(previous < offset, "filter order should be stable");
        }
        previous = Some(offset);
    }
}

#[test]
fn fingerprint_compare_equal_default_and_aliases_are_portable_silent_and_nonmutating() {
    let directory = TestDirectory::new();
    let fingerprint = directory.0.join("private-fingerprint.json");
    let alias = directory.0.join("private-fingerprint-alias.json");
    let contents = macos_fingerprint(
        "0.1.0",
        "25.5.0",
        Some("Mac16,1"),
        None,
        Some("0x1b588bb3"),
        EMPTY_TEMPLATE,
    );
    fs::write(&fingerprint, &contents).unwrap();
    fs::hard_link(&fingerprint, &alias).unwrap();
    let inode = fs::metadata(&fingerprint).unwrap().ino();

    for curr in [&fingerprint, &alias] {
        let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
            .args(["fingerprint", "compare", "--prev"])
            .arg(&fingerprint)
            .args(["--curr"])
            .arg(curr)
            .output()
            .expect("helper process should execute");
        assert!(output.status.success(), "stderr: {:?}", output.stderr);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    assert_eq!(fs::read_to_string(&fingerprint).unwrap(), contents);
    assert_eq!(fs::read_to_string(&alias).unwrap(), contents);
    assert_eq!(fs::metadata(&fingerprint).unwrap().ino(), inode);
    assert_eq!(fs::metadata(&alias).unwrap().ino(), inode);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn fingerprint_compare_emits_exact_canonical_difference_and_fixed_order() {
    let directory = TestDirectory::new();
    let prev = directory.0.join("private-prev.json");
    let curr = directory.0.join("private-curr.json");
    let prev_contents = macos_fingerprint(
        "0.1.0",
        "25.4.0",
        None,
        Some("OldTarget"),
        None,
        EMPTY_TEMPLATE,
    );
    let curr_contents = macos_fingerprint(
        "0.2.0",
        "25.5.0",
        Some("Mac16,1"),
        Some("NewTarget"),
        None,
        EMPTY_TEMPLATE,
    );
    fs::write(&prev, &prev_contents).unwrap();
    fs::write(&curr, &curr_contents).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "compare", "-p"])
        .arg(&prev)
        .args(["-c"])
        .arg(&curr)
        .args([
            "-f",
            "macos_product",
            "-f",
            "kernel_release",
            "producer_version",
        ])
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let expected = concat!(
        "{\n",
        "  \"differences\": [\n",
        "    {\n",
        "      \"name\": \"producer_version\",\n",
        "      \"prev\": \"0.1.0\",\n",
        "      \"curr\": \"0.2.0\"\n",
        "    },\n",
        "    {\n",
        "      \"name\": \"kernel_release\",\n",
        "      \"prev\": \"25.4.0\",\n",
        "      \"curr\": \"25.5.0\"\n",
        "    },\n",
        "    {\n",
        "      \"name\": \"macos_product\",\n",
        "      \"prev\": null,\n",
        "      \"curr\": \"Mac16,1\"\n",
        "    }\n",
        "  ]\n",
        "}\n",
    );
    assert_eq!(output.stderr, expected.as_bytes());

    let repeated = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "compare", "--prev"])
        .arg(&prev)
        .args(["--curr"])
        .arg(&curr)
        .args([
            "--filters",
            "producer_version",
            "kernel_release",
            "macos_product",
        ])
        .output()
        .expect("helper process should execute");
    assert_eq!(repeated.status.code(), Some(1));
    assert_eq!(repeated.stderr, output.stderr);
    assert_eq!(fs::read_to_string(&prev).unwrap(), prev_contents);
    assert_eq!(fs::read_to_string(&curr).unwrap(), curr_contents);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn fingerprint_compare_guest_difference_is_stripped_in_the_real_process() {
    let directory = TestDirectory::new();
    let prev = directory.0.join("previous.json");
    let curr = directory.0.join("current.json");
    let prev_guest = format!(
        r#"{{"reg_modifiers":[{{"addr":"0x{KVM_REG_ARM64_CORE_FPCR:x}","bitmap":"0b0"}}]}}"#
    );
    let curr_guest = format!(
        r#"{{"reg_modifiers":[{{"addr":"0x{KVM_REG_ARM64_CORE_FPCR:x}","bitmap":"0b1"}}]}}"#
    );
    fs::write(
        &prev,
        macos_fingerprint("0.1.0", "25.5.0", None, None, None, &prev_guest),
    )
    .unwrap();
    fs::write(
        &curr,
        macos_fingerprint("0.1.0", "25.5.0", None, None, None, &curr_guest),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "compare", "-p"])
        .arg(&prev)
        .args(["-c"])
        .arg(&curr)
        .args(["-f", "guest_cpu_config"])
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.starts_with("{\n  \"differences\": ["));
    assert!(diagnostic.contains(r#""name": "guest_cpu_config""#));
    assert!(diagnostic.contains(&format!("0x{KVM_REG_ARM64_CORE_FPCR:016x}")));
    assert!(diagnostic.contains(&format!(r#""bitmap": "0b{}0""#, "x".repeat(31))));
    assert!(diagnostic.contains(&format!(r#""bitmap": "0b{}1""#, "x".repeat(31))));
    assert!(diagnostic.ends_with("}\n"));
}

#[test]
fn fingerprint_compare_filter_and_platform_errors_are_fixed_and_value_redacted() {
    let directory = TestDirectory::new();
    let macos = directory.0.join("private-macos.json");
    let linux = directory.0.join("private-linux.json");
    fs::write(
        &macos,
        macos_fingerprint("0.1.0", "25.5.0", None, None, None, EMPTY_TEMPLATE),
    )
    .unwrap();
    fs::write(&linux, linux_fingerprint("private-linux-release")).unwrap();

    let duplicate = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "compare", "--prev"])
        .arg(&macos)
        .args(["--curr"])
        .arg(&macos)
        .args(["--filters", "kernel_release", "kernel_release"])
        .output()
        .unwrap();
    assert_eq!(duplicate.status.code(), Some(2));
    assert!(duplicate.stdout.is_empty());
    assert_eq!(
        String::from_utf8(duplicate.stderr).unwrap(),
        "cpu-template-helper: invalid arguments; use --help for the supported interface\n"
    );

    let unavailable = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "compare", "--prev"])
        .arg(&macos)
        .args(["--curr"])
        .arg(&macos)
        .args(["--filters", "linux_bios_version"])
        .output()
        .unwrap();
    assert_eq!(unavailable.status.code(), Some(1));
    assert!(unavailable.stdout.is_empty());
    assert_eq!(
        String::from_utf8(unavailable.stderr).unwrap(),
        "cpu-template-helper: CPU-fingerprint filter is unavailable for the host platform\n"
    );

    let mismatch = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "compare", "--prev"])
        .arg(&macos)
        .args(["--curr"])
        .arg(&linux)
        .args(["--filters", "producer_version"])
        .output()
        .unwrap();
    assert_eq!(mismatch.status.code(), Some(1));
    assert!(mismatch.stdout.is_empty());
    let stderr = String::from_utf8(mismatch.stderr).unwrap();
    assert_eq!(
        stderr,
        "cpu-template-helper: CPU-fingerprint platforms do not match\n"
    );
    assert!(!stderr.contains("private"));
}

#[test]
fn fingerprint_compare_rejects_strict_document_and_file_failures_without_mutation() {
    let directory = TestDirectory::new();
    let valid = directory.0.join("private-valid.json");
    let valid_contents = macos_fingerprint("0.1.0", "25.5.0", None, None, None, EMPTY_TEMPLATE);
    fs::write(&valid, &valid_contents).unwrap();
    let malformed = directory.0.join("private-malformed.json");
    fs::write(&malformed, b"{").unwrap();
    let unsupported = directory.0.join("private-unsupported.json");
    fs::write(
        &unsupported,
        valid_contents.replacen(r#""schema_version": 1"#, r#""schema_version": 2"#, 1),
    )
    .unwrap();
    let duplicate_field = directory.0.join("private-duplicate-field.json");
    fs::write(
        &duplicate_field,
        valid_contents.replacen(
            r#""schema_version": 1,"#,
            r#""schema_version": 1,"schema_version": 1,"#,
            1,
        ),
    )
    .unwrap();
    let unknown_field = directory.0.join("private-unknown-field.json");
    fs::write(
        &unknown_field,
        valid_contents.replacen(
            r#""schema_version": 1,"#,
            r#""schema_version": 1,"private_unknown": true,"#,
            1,
        ),
    )
    .unwrap();
    let unsupported_producer = directory.0.join("private-unsupported-producer.json");
    fs::write(
        &unsupported_producer,
        valid_contents.replacen(
            "bangbang-cpu-template-helper",
            "private-unsupported-helper",
            1,
        ),
    )
    .unwrap();
    let unsupported_compatibility = directory.0.join("private-unsupported-compatibility.json");
    fs::write(
        &unsupported_compatibility,
        valid_contents.replacen("1.16.0", "private-compatibility", 1),
    )
    .unwrap();
    let noncanonical_version = directory.0.join("private-noncanonical-version.json");
    fs::write(
        &noncanonical_version,
        valid_contents.replacen(r#""version": "0.1.0""#, r#""version": "01.0.0""#, 1),
    )
    .unwrap();
    let noncanonical_fact = directory.0.join("private-noncanonical-fact.json");
    fs::write(
        &noncanonical_fact,
        valid_contents.replacen(r#""release": "25.5.0""#, r#""release": " 25.5.0""#, 1),
    )
    .unwrap();
    let invalid_null = directory.0.join("private-invalid-null.json");
    fs::write(
        &invalid_null,
        valid_contents.replacen(r#""product": null"#, r#""product": false"#, 1),
    )
    .unwrap();
    let mixed_host = directory.0.join("private-mixed-host.json");
    fs::write(
        &mixed_host,
        valid_contents.replacen(
            r#""target": null"#,
            r#""microcode_version": "private-microcode""#,
            1,
        ),
    )
    .unwrap();
    let invalid_guest = directory.0.join("private-invalid-guest.json");
    fs::write(
        &invalid_guest,
        valid_contents.replacen(
            r#""kvm_capabilities":[]"#,
            r#""kvm_capabilities":["private-capability"]"#,
            1,
        ),
    )
    .unwrap();
    let invalid_utf8 = directory.0.join("private-invalid-utf8.json");
    fs::write(&invalid_utf8, [0xff]).unwrap();
    let oversized = directory.0.join("private-oversized.json");
    fs::write(
        &oversized,
        vec![b' '; bangbang_cpu_template_helper::CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1],
    )
    .unwrap();
    let link = directory.0.join("private-link.json");
    symlink(&valid, &link).unwrap();
    let fifo = directory.0.join("private-fifo.json");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );

    for bad in [
        &directory.0.join("private-missing.json"),
        &malformed,
        &unsupported,
        &duplicate_field,
        &unknown_field,
        &unsupported_producer,
        &unsupported_compatibility,
        &noncanonical_version,
        &noncanonical_fact,
        &invalid_null,
        &mixed_host,
        &invalid_guest,
        &invalid_utf8,
        &oversized,
        &link,
        &directory.0,
        &fifo,
    ] {
        for bad_is_prev in [true, false] {
            let (prev, curr) = if bad_is_prev {
                (bad, &valid)
            } else {
                (&valid, bad)
            };
            let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
                .args(["fingerprint", "compare", "--prev"])
                .arg(prev)
                .args(["--curr"])
                .arg(curr)
                .output()
                .expect("helper process should execute");
            assert_eq!(output.status.code(), Some(1), "bad input: {bad:?}");
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.starts_with("cpu-template-helper: "));
            assert!(!stderr.contains("private"));
        }
    }

    assert_eq!(fs::read_to_string(&valid).unwrap(), valid_contents);
    assert_eq!(fs::read(&malformed).unwrap(), b"{");
    assert_eq!(fs::read(&invalid_utf8).unwrap(), [0xff]);
    assert_eq!(
        fs::metadata(&oversized).unwrap().len(),
        (bangbang_cpu_template_helper::CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1) as u64
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn bounded_input_failures_are_path_and_value_redacted_before_inspection() {
    let directory = TestDirectory::new();
    let malformed = directory.0.join("private-config-name.json");
    fs::write(&malformed, r#"{"private-secret":true}"#)
        .expect("malformed fixture should be written");
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "dump", "--config"])
        .arg(&malformed)
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "cpu-template-helper: invalid helper configuration document\n"
    );
    assert!(!stderr.contains("private"));

    let target = directory.0.join("template.json");
    fs::write(&target, "{}").expect("target fixture should be written");
    let link = directory.0.join("private-template-link.json");
    symlink(&target, &link).expect("symlink fixture should be created");
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "verify", "--template"])
        .arg(&link)
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: helper input could not be opened safely\n"
    );
}

#[test]
fn no_template_and_unavailable_hvf_never_publish_output() {
    let directory = TestDirectory::new();

    let output = run(&["template", "verify"], Some(&directory.0));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: no custom CPU template was selected\n"
    );

    let explicit = directory.0.join("explicit-output.json");
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "dump", "--output"])
        .arg(&explicit)
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: effective CPU inspection failed\n"
    );
    assert!(!explicit.exists());
    assert!(!directory.0.join("cpu_config.json").exists());
}

#[test]
fn fingerprint_failures_are_bounded_and_publish_neither_default_nor_explicit_output() {
    let directory = TestDirectory::new();
    let output = run(&["fingerprint", "dump"], Some(&directory.0));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    #[cfg(target_os = "macos")]
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: effective CPU fingerprint capture failed\n"
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: host fingerprint capture failed\n"
    );
    assert!(!directory.0.join("fingerprint.json").exists());

    let explicit = directory.0.join("private-fingerprint-output.json");
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "dump", "-o"])
        .arg(&explicit)
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("private")
    );
    assert!(!explicit.exists());
}

#[test]
fn fingerprint_inputs_fail_before_host_or_effective_capture() {
    let directory = TestDirectory::new();
    let malformed = directory.0.join("private-malformed-config.json");
    let explicit = directory.0.join("private-fingerprint-output.json");
    fs::write(&malformed, r#"{"private-secret":true}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["fingerprint", "dump", "--config"])
        .arg(&malformed)
        .args(["--output"])
        .arg(&explicit)
        .output()
        .expect("helper process should execute");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: invalid helper configuration document\n"
    );
    assert!(!explicit.exists());
}

fn assert_canonical_template(path: &Path) {
    let bytes = fs::read(path).expect("strip output should be readable");
    let text = std::str::from_utf8(&bytes).expect("strip output should be UTF-8");
    let document = bangbang_cpu_template_helper::document::decode_cpu_template_document(text)
        .expect("strip output should strictly parse");
    assert_eq!(
        document.canonical_bytes().as_deref(),
        Ok(bytes.as_slice()),
        "strip output should already be canonical"
    );
}

#[test]
fn strip_default_and_explicit_suffixes_are_portable_and_silent() {
    for (suffix, expected_names) in [
        (None, ["first_stripped.json", "second_stripped.json"]),
        (
            Some(".portable"),
            ["first.portable.json", "second.portable.json"],
        ),
    ] {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let second = directory.0.join("second.json");
        fs::write(&first, EMPTY_TEMPLATE).expect("fixture should be written");
        fs::write(&second, EMPTY_TEMPLATE).expect("fixture should be written");
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"));
        command
            .args(["template", "strip", "--paths"])
            .arg(&first)
            .arg(&second);
        if let Some(suffix) = suffix {
            command.args(["--suffix", suffix]);
        }
        let output = command.output().expect("helper process should execute");
        assert!(output.status.success(), "stderr: {:?}", output.stderr);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert_eq!(fs::read(&first).unwrap(), EMPTY_TEMPLATE.as_bytes());
        assert_eq!(fs::read(&second).unwrap(), EMPTY_TEMPLATE.as_bytes());
        for name in expected_names {
            assert_canonical_template(&directory.0.join(name));
        }
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 4);
    }
}

#[test]
fn strip_empty_suffix_replaces_exact_inputs_with_canonical_outputs() {
    let directory = TestDirectory::new();
    let first = directory.0.join("first.json");
    let second = directory.0.join("second.json");
    fs::write(&first, EMPTY_TEMPLATE).expect("fixture should be written");
    fs::write(&second, EMPTY_TEMPLATE).expect("fixture should be written");
    let original_inodes = [
        fs::metadata(&first).unwrap().ino(),
        fs::metadata(&second).unwrap().ino(),
    ];
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "strip", "-p"])
        .arg(&first)
        .arg(&second)
        .args(["-s", ""])
        .output()
        .expect("helper process should execute");
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_canonical_template(&first);
    assert_canonical_template(&second);
    assert_ne!(fs::metadata(&first).unwrap().ino(), original_inodes[0]);
    assert_ne!(fs::metadata(&second).unwrap().ino(), original_inodes[1]);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn strip_precommit_failures_preserve_inputs_winners_and_aliases() {
    let collision_directory = TestDirectory::new();
    let first = collision_directory.0.join("private-first.json");
    let second = collision_directory.0.join("private-second.json");
    let winner = collision_directory.0.join("private-first_stripped.json");
    fs::write(&first, EMPTY_TEMPLATE).unwrap();
    fs::write(&second, EMPTY_TEMPLATE).unwrap();
    fs::write(&winner, b"concurrent-winner").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "strip", "-p"])
        .arg(&first)
        .arg(&second)
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: strip output target already exists\n"
    );
    assert_eq!(fs::read(&first).unwrap(), EMPTY_TEMPLATE.as_bytes());
    assert_eq!(fs::read(&second).unwrap(), EMPTY_TEMPLATE.as_bytes());
    assert_eq!(fs::read(&winner).unwrap(), b"concurrent-winner");
    assert!(
        !collision_directory
            .0
            .join("private-second_stripped.json")
            .exists()
    );

    let hardlink_directory = TestDirectory::new();
    let first = hardlink_directory.0.join("private-first.json");
    let alias = hardlink_directory.0.join("private-alias.json");
    let second = hardlink_directory.0.join("private-second.json");
    fs::write(&first, EMPTY_TEMPLATE).unwrap();
    fs::hard_link(&first, &alias).unwrap();
    fs::write(&second, EMPTY_TEMPLATE).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "strip", "-p"])
        .arg(&first)
        .arg(&second)
        .args(["-s", ""])
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "cpu-template-helper: CPU-template strip replacement input has multiple links\n"
    );
    assert!(!stderr.contains("private"));
    assert_eq!(fs::read(&first).unwrap(), EMPTY_TEMPLATE.as_bytes());
    assert_eq!(fs::read(&alias).unwrap(), EMPTY_TEMPLATE.as_bytes());
    assert_eq!(fs::read(&second).unwrap(), EMPTY_TEMPLATE.as_bytes());
}

#[test]
fn strip_bad_documents_and_file_types_fail_before_any_output() {
    let directory = TestDirectory::new();
    let valid = directory.0.join("private-valid.json");
    let sentinel = directory.0.join("private-sentinel");
    fs::write(&valid, EMPTY_TEMPLATE).unwrap();
    fs::write(&sentinel, b"sentinel").unwrap();

    let malformed = directory.0.join("private-malformed.json");
    fs::write(&malformed, b"{").unwrap();
    let oversized = directory.0.join("private-oversized.json");
    fs::write(
        &oversized,
        vec![b'x'; bangbang_cpu_template_helper::CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1],
    )
    .unwrap();
    let target = directory.0.join("private-target.json");
    fs::write(&target, EMPTY_TEMPLATE).unwrap();
    let link = directory.0.join("private-link.json");
    symlink(&target, &link).unwrap();
    let fifo = directory.0.join("private-fifo.json");
    let status = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(status.success());

    for input in [&malformed, &oversized, &link, &fifo] {
        let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
            .args(["template", "strip", "-p"])
            .arg(input)
            .arg(&valid)
            .output()
            .expect("helper process should execute");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("private")
        );
        assert!(!directory.0.join("private-valid_stripped.json").exists());
        let output_name = format!(
            "{}_stripped.json",
            input.file_stem().unwrap().to_string_lossy()
        );
        assert!(!directory.0.join(output_name).exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"sentinel");
    }

    assert_eq!(fs::read(&malformed).unwrap(), b"{");
    assert_eq!(
        fs::metadata(&oversized).unwrap().len(),
        (bangbang_cpu_template_helper::CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1) as u64
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&valid).unwrap(), EMPTY_TEMPLATE.as_bytes());
}

#[test]
fn strip_unsafe_suffix_and_minimum_arity_use_bounded_exit_classes() {
    let directory = TestDirectory::new();
    let first = directory.0.join("private-first.json");
    let second = directory.0.join("private-second.json");
    fs::write(&first, EMPTY_TEMPLATE).unwrap();
    fs::write(&second, EMPTY_TEMPLATE).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "strip", "-p"])
        .arg(&first)
        .arg(&second)
        .args(["-s", "/private-destination"])
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: CPU-template strip suffix is unsafe\n"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cpu-template-helper"))
        .args(["template", "strip", "-p"])
        .arg(&first)
        .output()
        .expect("helper process should execute");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: invalid arguments; use --help for the supported interface\n"
    );

    let output = run(&["template", "strip"], None);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cpu-template-helper: CPU-template strip requires at least two inputs\n"
    );
}
