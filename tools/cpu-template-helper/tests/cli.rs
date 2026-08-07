#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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

#[test]
fn help_and_version_are_the_only_portable_stdout_successes() {
    for args in [
        vec!["--help"],
        vec!["template", "--help"],
        vec!["template", "dump", "--help"],
        vec!["template", "strip", "--help"],
        vec!["template", "verify", "--help"],
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
    ] {
        let output = run(&args, None);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty(), "args: {args:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), expected);
    }
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
