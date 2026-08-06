#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        vec!["template", "strip"],
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
