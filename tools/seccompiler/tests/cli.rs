#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod support;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use support::{
    ACTION_ALLOW, ACTION_TRAP, AUDIT_ARCH_AARCH64, AUDIT_ARCH_X86_64, SeccompData, execute,
};

static TEST_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

const POLICY: &str = r#"{
  "vmm": {
    "default_action": "trap",
    "filter_action": "allow",
    "filter": [{"syscall":"read","args":[{"index":0,"type":"qword","op":"eq","val":7}]}]
  },
  "api": {
    "default_action": "trap",
    "filter_action": "allow",
    "filter": [{"syscall":"read"}]
  },
  "vcpu": {
    "default_action": "trap",
    "filter_action": "allow",
    "filter": [{"syscall":"read"}]
  }
}"#;

#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = TEST_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-seccompiler-cli-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_seccompiler-bin"))
}

fn write_policy(directory: &TestDirectory) -> PathBuf {
    let path = directory.path("policy.json");
    fs::write(&path, POLICY).expect("policy should be written");
    path
}

fn run_compile(
    directory: &TestDirectory,
    target: &str,
    input: &Path,
    output: &Path,
    additional: &[&OsStr],
) -> Output {
    let mut process = command();
    process
        .current_dir(&directory.0)
        .arg("--target-arch")
        .arg(target)
        .arg("--input-file")
        .arg(input)
        .arg("--output-file")
        .arg(output)
        .args(additional);
    process.output().expect("tool should start")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

fn assert_static_failure(output: &Output, status: i32, message: &str, sensitive: &str) {
    assert_eq!(output.status.code(), Some(status));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr, format!("seccompiler-bin: {message}\n"));
    assert!(!stderr.contains(sensitive));
}

fn decode_combined(path: &Path) -> HashMap<String, Vec<u64>> {
    bitcode::deserialize(&fs::read(path).expect("combined output should be readable"))
        .expect("combined output should use Firecracker's bitcode shape")
}

fn decode_split(path: &Path) -> Vec<u64> {
    let bytes = fs::read(path).expect("split output should be readable");
    assert!(bytes.len().is_multiple_of(size_of::<u64>()));
    bytes
        .chunks_exact(size_of::<u64>())
        .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("chunk should be eight bytes")))
        .collect()
}

#[test]
fn help_and_version_identify_the_offline_compatibility_tool() {
    let help = command().arg("--help").output().expect("help should run");
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let stdout = String::from_utf8(help.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("Firecracker v1.16"));
    assert!(stdout.contains("does not install or enforce seccomp on macOS"));
    for argument in [
        "--target-arch",
        "--input-file",
        "--output-file",
        "--basic",
        "--split-output",
    ] {
        assert!(stdout.contains(argument));
    }

    let version = command()
        .arg("--version")
        .output()
        .expect("version should run");
    assert!(version.status.success());
    assert!(version.stderr.is_empty());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!(
            "seccompiler-bin {} (bangbang; Firecracker v1.16.0-compatible artifact format)\n",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn invalid_invocations_use_exit_two_and_do_not_echo_values() {
    let sensitive = "private-unknown-argument";
    for arguments in [
        vec![],
        vec![sensitive],
        vec!["--target-arch", "x86_64", "--target-arch", "aarch64"],
    ] {
        let output = command()
            .args(arguments)
            .output()
            .expect("invalid invocation should run");
        assert_static_failure(
            &output,
            2,
            "invalid arguments; use --help for the supported interface",
            sensitive,
        );
    }
}

#[test]
fn long_short_attached_and_default_output_forms_are_supported() {
    let directory = TestDirectory::new();
    let input = write_policy(&directory);
    let explicit = directory.path("explicit.bin");
    let output = run_compile(&directory, "x86_64", &input, &explicit, &[]);
    assert_success(&output);
    assert_eq!(decode_combined(&explicit).len(), 3);

    let attached = directory.path("attached.bin");
    let output = command()
        .current_dir(&directory.0)
        .arg("-taarch64")
        .arg(format!("-i{}", input.display()))
        .arg(format!("-o{}", attached.display()))
        .output()
        .expect("attached short options should run");
    assert_success(&output);
    assert_eq!(decode_combined(&attached).len(), 3);

    let output = command()
        .current_dir(&directory.0)
        .args(["-t", "x86_64", "-i"])
        .arg(&input)
        .output()
        .expect("default output invocation should run");
    assert_success(&output);
    assert_eq!(
        decode_combined(&directory.path("seccomp_binary_filter.out")).len(),
        3
    );
}

#[test]
fn combined_output_is_deterministic_complete_and_owner_only() {
    let directory = TestDirectory::new();
    let input = write_policy(&directory);
    let output_path = directory.path("filters.bin");
    fs::write(&output_path, b"old-partial-looking-content").unwrap();

    let first = run_compile(&directory, "x86_64", &input, &output_path, &[]);
    assert_success(&first);
    let first_bytes = fs::read(&output_path).unwrap();
    let second = run_compile(&directory, "x86_64", &input, &output_path, &[]);
    assert_success(&second);
    assert_eq!(fs::read(&output_path).unwrap(), first_bytes);
    assert!(first_bytes.len() <= 100_000);
    assert_eq!(fs::metadata(&output_path).unwrap().mode() & 0o777, 0o600);
    assert!(fs::read_dir(&directory.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".seccompiler-bin.")
    }));
}

#[test]
fn split_and_basic_outputs_preserve_compiled_filter_semantics() {
    let directory = TestDirectory::new();
    let input = write_policy(&directory);
    let selector = directory.path("ignored-selector.bin");
    let split = [OsStr::new("--split-output")];
    let output = run_compile(&directory, "x86_64", &input, &selector, &split);
    assert_success(&output);
    assert!(!selector.exists());
    for name in ["vmm.bpf", "api.bpf", "vcpu.bpf"] {
        assert!(directory.path(name).is_file());
    }

    let vmm = decode_split(&directory.path("vmm.bpf"));
    assert_eq!(
        execute(&vmm, SeccompData::new(0, AUDIT_ARCH_X86_64).with_arg(0, 7)),
        ACTION_ALLOW
    );
    assert_eq!(
        execute(&vmm, SeccompData::new(0, AUDIT_ARCH_X86_64).with_arg(0, 8)),
        ACTION_TRAP
    );

    let basic = [OsStr::new("--split-output"), OsStr::new("--basic")];
    let output = run_compile(&directory, "aarch64", &input, &selector, &basic);
    assert_success(&output);
    let vmm = decode_split(&directory.path("vmm.bpf"));
    assert_eq!(
        execute(
            &vmm,
            SeccompData::new(63, AUDIT_ARCH_AARCH64).with_arg(0, 8)
        ),
        ACTION_ALLOW
    );
}

#[test]
fn input_failures_are_bounded_redacted_and_leave_existing_output_unchanged() {
    let directory = TestDirectory::new();
    let output_path = directory.path("preserved.bin");
    fs::write(&output_path, b"preserve-me").unwrap();

    let cases = [
        (
            "missing-private-input",
            "input file could not be opened safely",
        ),
        (
            "directory-private-input",
            "input path is not a regular file",
        ),
        ("utf8-private-input", "input file is not valid UTF-8"),
        ("large-private-input", "input file exceeds the size limit"),
        ("empty-private-input", "seccomp policy is not valid JSON"),
        (
            "schema-private-input",
            "seccomp policy must contain the required thread categories",
        ),
    ];
    fs::create_dir(directory.path(cases[1].0)).unwrap();
    fs::write(directory.path(cases[2].0), [0xff, 0xfe]).unwrap();
    fs::write(
        directory.path(cases[3].0),
        vec![b' '; bangbang_seccompiler::MAX_JSON_BYTES + 1],
    )
    .unwrap();
    fs::write(directory.path(cases[4].0), []).unwrap();
    fs::write(directory.path(cases[5].0), "{}").unwrap();

    for (name, message) in cases {
        let input = directory.path(name);
        let result = run_compile(&directory, "x86_64", &input, &output_path, &[]);
        assert_static_failure(&result, 1, message, name);
        assert_eq!(fs::read(&output_path).unwrap(), b"preserve-me");
    }

    let symlink_name = "symlink-private-input";
    symlink(
        directory.path("empty-private-input"),
        directory.path(symlink_name),
    )
    .unwrap();
    let result = run_compile(
        &directory,
        "x86_64",
        &directory.path(symlink_name),
        &output_path,
        &[],
    );
    assert_static_failure(
        &result,
        1,
        "input file could not be opened safely",
        symlink_name,
    );
    assert_eq!(fs::read(&output_path).unwrap(), b"preserve-me");

    let fifo_name = "fifo-private-input";
    let fifo_status = Command::new("mkfifo")
        .arg(directory.path(fifo_name))
        .status()
        .expect("mkfifo should start");
    assert!(fifo_status.success());
    let result = run_compile(
        &directory,
        "x86_64",
        &directory.path(fifo_name),
        &output_path,
        &[],
    );
    assert_static_failure(&result, 1, "input path is not a regular file", fifo_name);

    let socket_name = "socket-private-input";
    let _listener = UnixListener::bind(directory.path(socket_name)).unwrap();
    let result = run_compile(
        &directory,
        "x86_64",
        &directory.path(socket_name),
        &output_path,
        &[],
    );
    assert_static_failure(
        &result,
        1,
        "input file could not be opened safely",
        socket_name,
    );
    assert_eq!(fs::read(&output_path).unwrap(), b"preserve-me");

    let syscall_name = "private-unknown-syscall";
    let unknown_policy = POLICY.replace("\"read\"", &format!("\"{syscall_name}\""));
    let unknown_input = directory.path("unknown-syscall-input");
    fs::write(&unknown_input, unknown_policy).unwrap();
    let result = run_compile(&directory, "x86_64", &unknown_input, &output_path, &[]);
    assert_static_failure(
        &result,
        1,
        "seccomp policy contains an unknown target syscall",
        syscall_name,
    );
    assert_eq!(fs::read(&output_path).unwrap(), b"preserve-me");
}

#[test]
fn special_output_targets_are_rejected_without_following_or_truncating_them() {
    let directory = TestDirectory::new();
    let input = write_policy(&directory);
    let sentinel = directory.path("sentinel");
    fs::write(&sentinel, b"do-not-touch").unwrap();

    let symlink_path = directory.path("private-symlink-output");
    symlink(&sentinel, &symlink_path).unwrap();
    let fifo_path = directory.path("private-fifo-output");
    let fifo_status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("mkfifo should start");
    assert!(fifo_status.success());
    let socket_path = directory.path("private-socket-output");
    let _listener = UnixListener::bind(&socket_path).unwrap();
    let directory_path = directory.path("private-directory-output");
    fs::create_dir(&directory_path).unwrap();

    for output_path in [symlink_path, fifo_path, socket_path, directory_path] {
        let sensitive = output_path.file_name().unwrap().to_string_lossy();
        let result = run_compile(&directory, "x86_64", &input, &output_path, &[]);
        assert_static_failure(
            &result,
            1,
            "output target is not absent or a regular file",
            &sensitive,
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"do-not-touch");
    }
}

#[test]
fn unsupported_targets_and_invalid_output_paths_are_static_failures() {
    let directory = TestDirectory::new();
    let input = write_policy(&directory);
    let sensitive = "private-target-value";
    let result = run_compile(
        &directory,
        sensitive,
        &input,
        &directory.path("unused"),
        &[],
    );
    assert_static_failure(&result, 1, "target architecture is unsupported", sensitive);

    let result = run_compile(&directory, "x86_64", &input, Path::new("."), &[]);
    assert_static_failure(&result, 1, "output path must name a file", "private-value");
}
