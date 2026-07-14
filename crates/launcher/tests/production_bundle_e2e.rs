#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_launcher::{
    LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME,
    WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};

const BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_BUNDLE_PATH";
const BAD_CONFIGURATION_EXIT_CODE: i32 = 152;
const ARGUMENT_PARSING_EXIT_CODE: i32 = 153;
const PROCESS_FAILURE_EXIT_CODE: i32 = 1;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

fn production_bundle() -> PathBuf {
    let path = std::env::var_os(BUNDLE_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the production bundle path");
    let path = PathBuf::from(path);
    assert_eq!(path.file_name(), Some(OsStr::new(OUTER_BUNDLE_NAME)));
    path
}

fn launcher(bundle: &Path) -> PathBuf {
    bundle.join("Contents/MacOS").join(LAUNCHER_EXECUTABLE_NAME)
}

fn worker_bundle(bundle: &Path) -> PathBuf {
    bundle.join("Contents/Helpers").join(WORKER_BUNDLE_NAME)
}

fn worker_executable(bundle: &Path) -> PathBuf {
    worker_bundle(bundle)
        .join("Contents/MacOS")
        .join(WORKER_EXECUTABLE_NAME)
}

fn run_launcher(bundle: &Path, args: &[&OsStr]) -> Output {
    Command::new(launcher(bundle))
        .args(args)
        .output()
        .expect("production launcher should execute")
}

#[test]
fn production_bundle_has_exact_nested_signing_contract() {
    let bundle = production_bundle();
    let worker = worker_bundle(&bundle);
    let verify = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=4"])
        .arg(&bundle)
        .output()
        .expect("codesign verification should execute");
    assert_output_success(&verify, "strict recursive bundle verification");

    let outer_display = codesign_display(&bundle);
    let worker_display = codesign_display(&worker);
    assert!(
        outer_display.contains(&format!("Identifier={LAUNCHER_BUNDLE_IDENTIFIER}")),
        "outer identifier should match; display:\n{outer_display}"
    );
    assert!(
        worker_display.contains(&format!("Identifier={WORKER_BUNDLE_IDENTIFIER}")),
        "worker identifier should match; display:\n{worker_display}"
    );
    assert!(outer_display.contains("runtime"));
    assert!(worker_display.contains("runtime"));

    let outer_entitlements = codesign_entitlements(&bundle);
    let worker_entitlements = codesign_entitlements(&worker);
    assert!(
        !outer_entitlements.contains("com.apple.security.app-sandbox")
            && !outer_entitlements.contains("com.apple.security.hypervisor"),
        "launcher must not inherit worker entitlements: {outer_entitlements}"
    );
    assert_eq!(worker_entitlements.matches("<key>").count(), 2);
    assert!(worker_entitlements.contains("<key>com.apple.security.app-sandbox</key>"));
    assert!(worker_entitlements.contains("<key>com.apple.security.hypervisor</key>"));
}

#[test]
fn launcher_forwards_help_and_argument_parsing_exit() {
    let bundle = production_bundle();
    let help = run_launcher(&bundle, &[OsStr::new("--help")]);
    assert_output_success(&help, "launcher help");
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    assert!(help_stdout.contains("Usage:\n  bangbang [OPTIONS]"));

    let version = run_launcher(&bundle, &[OsStr::new("--version")]);
    assert_output_success(&version, "launcher version");
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("bangbang "));

    let opaque = OsString::from_vec(vec![0xff, 0xfe]);
    let opaque_version = run_launcher(
        &bundle,
        &[
            OsStr::new("--version"),
            OsStr::new("--"),
            opaque.as_os_str(),
        ],
    );
    assert_output_success(&opaque_version, "opaque argument forwarding");
    assert!(String::from_utf8_lossy(&opaque_version.stdout).starts_with("bangbang "));

    let bad = run_launcher(&bundle, &[OsStr::new("--no-api")]);
    assert_eq!(bad.status.code(), Some(ARGUMENT_PARSING_EXIT_CODE));
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("--no-api requires --config-file"));
    assert!(!stderr.contains("launcher signal"));
}

#[test]
fn launcher_rejects_modified_missing_or_wrongly_signed_worker_before_execution() {
    let source = production_bundle();

    let modified = TestDir::new("modified");
    let modified_bundle = modified.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &modified_bundle);
    OpenOptions::new()
        .append(true)
        .open(worker_executable(&modified_bundle))
        .expect("copied worker should open")
        .write_all(b"tamper")
        .expect("copied worker should be modified");
    assert_invalid_bundle(run_launcher(&modified_bundle, &[OsStr::new("--help")]));

    let missing = TestDir::new("missing");
    let missing_bundle = missing.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &missing_bundle);
    fs::remove_file(worker_executable(&missing_bundle)).expect("copied worker should be removed");
    assert_invalid_bundle(run_launcher(&missing_bundle, &[OsStr::new("--help")]));

    let false_entitlement = TestDir::new("false-entitlement");
    let false_bundle = false_entitlement.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &false_bundle);
    resign_worker_and_outer(
        &false_bundle,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><false/>
<key>com.apple.security.hypervisor</key><true/>
</dict></plist>"#,
        true,
        true,
    );
    assert_invalid_bundle(run_launcher(&false_bundle, &[OsStr::new("--help")]));

    let extra_entitlement = TestDir::new("extra-entitlement");
    let extra_bundle = extra_entitlement.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &extra_bundle);
    resign_worker_and_outer(
        &extra_bundle,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
<key>com.apple.security.network.client</key><true/>
</dict></plist>"#,
        true,
        true,
    );
    assert_invalid_bundle(run_launcher(&extra_bundle, &[OsStr::new("--help")]));

    let valid_entitlements = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
</dict></plist>"#;

    let worker_without_runtime = TestDir::new("worker-without-runtime");
    let worker_without_runtime_bundle = worker_without_runtime.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &worker_without_runtime_bundle);
    resign_worker_and_outer(
        &worker_without_runtime_bundle,
        valid_entitlements,
        false,
        true,
    );
    assert_invalid_bundle(run_launcher(
        &worker_without_runtime_bundle,
        &[OsStr::new("--help")],
    ));

    let outer_without_runtime = TestDir::new("outer-without-runtime");
    let outer_without_runtime_bundle = outer_without_runtime.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &outer_without_runtime_bundle);
    resign_worker_and_outer(
        &outer_without_runtime_bundle,
        valid_entitlements,
        true,
        false,
    );
    assert_invalid_bundle(run_launcher(
        &outer_without_runtime_bundle,
        &[OsStr::new("--help")],
    ));
}

#[test]
fn launcher_preserves_sandbox_outside_path_denial_and_redaction() {
    let bundle = production_bundle();
    let denied = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = run_launcher(
        &bundle,
        &[
            OsStr::new("--config-file"),
            denied.as_os_str(),
            OsStr::new("--no-api"),
        ],
    );
    assert_eq!(output.status.code(), Some(BAD_CONFIGURATION_EXIT_CODE));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config-file error: failed to read config file: PermissionDenied"));
    let denied = denied.to_string_lossy();
    assert!(!stdout.contains(denied.as_ref()) && !stderr.contains(denied.as_ref()));
    assert!(!stdout.contains("status: VM running without API"));
}

#[test]
fn launcher_forwards_graceful_signals_and_worker_cleans_owned_socket() {
    run_graceful_signal_case(libc::SIGINT, "sigint");
    run_graceful_signal_case(libc::SIGTERM, "sigterm");
}

#[test]
fn launcher_runs_real_sandboxed_hvf_guest_to_system_off() {
    let bundle = production_bundle();
    let config = worker_bundle(&bundle).join("Contents/Resources/vm-config.json");
    assert!(config.is_file(), "signed runner must seal the guest config");
    let output = run_with_timeout(
        Command::new(launcher(&bundle))
            .args([OsStr::new("--config-file"), config.as_os_str()])
            .arg("--no-api"),
        PROCESS_TIMEOUT,
        "production sandbox guest SYSTEM_OFF",
    );
    assert_output_success(&output, "production sandbox guest SYSTEM_OFF");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("status: VM running without API"));
    assert!(!stdout.contains("status: API server listening"));
}

fn run_graceful_signal_case(signal: i32, name: &str) {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    let socket = container_tmp_dir().join(format!(
        "bb-production-{}-{}-{name}.sock",
        std::process::id(),
        NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let mut child = Command::new(launcher(&bundle))
        .args(["--api-sock", path_text(&socket), "--id"])
        .arg(format!("production-{name}-{}", std::process::id()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("production launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(err) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!("worker should publish API readiness: {err}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }

    let response = http_get(&socket, "/");
    assert!(
        response.starts_with("HTTP/1.1 200 "),
        "response:\n{response}"
    );
    assert!(response.contains(r#""state":"Not started""#));

    let pid = i32::try_from(child.id()).expect("launcher PID should fit");
    // SAFETY: `pid` is the live owned launcher and `signal` is SIGINT or
    // SIGTERM for this test case.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    let status = wait_child_with_timeout(child, PROCESS_TIMEOUT, name);
    let stdout = stdout_reader.join().expect("stdout reader should join");
    let stderr = stderr_reader.join().expect("stderr reader should join");
    assert!(
        status.success(),
        "{name} should stop launcher and worker successfully; status: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !socket.exists(),
        "{name} should remove the owned API socket"
    );
}

fn initialize_worker_container(bundle: &Path) {
    let output = run_launcher(bundle, &[OsStr::new("--help")]);
    assert_output_success(&output, "worker container initialization");
    fs::create_dir_all(container_tmp_dir()).expect("worker container tmp should exist");
}

fn container_tmp_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME should exist"))
        .join("Library/Containers")
        .join(WORKER_BUNDLE_IDENTIFIER)
        .join("Data/tmp")
}

fn read_stdout_until_ready(child: &mut Child) -> (Receiver<()>, JoinHandle<String>) {
    let stdout = child.stdout.take().expect("stdout should be piped");
    let (ready_sender, ready_receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut collected = String::new();
        let mut ready_sender = Some(ready_sender);
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("launcher stdout should be readable");
            if line == "status: API server listening"
                && let Some(sender) = ready_sender.take()
            {
                let _ = sender.send(());
            }
            collected.push_str(&line);
            collected.push('\n');
        }
        collected
    });
    (ready_receiver, reader)
}

fn read_stream<R>(mut stream: R) -> JoinHandle<String>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = String::new();
        stream
            .read_to_string(&mut output)
            .expect("child stream should be readable");
        output
    })
}

fn wait_child_with_timeout(mut child: Child, timeout: Duration, context: &str) -> ExitStatus {
    if wait_for_child_exit(&child, timeout) {
        return child.wait().expect("launcher wait should succeed");
    }
    kill_child_group(&mut child);
    let _ = child.wait();
    panic!("timed out waiting for {context}");
}

fn run_with_timeout(command: &mut Command, timeout: Duration, context: &str) -> Output {
    let mut child = command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bounded command should start");
    let stdout = read_stream(child.stdout.take().expect("stdout should be piped"));
    let stderr = read_stream(child.stderr.take().expect("stderr should be piped"));
    let status = wait_child_with_timeout(child, timeout, context);
    Output {
        status,
        stdout: stdout
            .join()
            .expect("stdout reader should join")
            .into_bytes(),
        stderr: stderr
            .join()
            .expect("stderr reader should join")
            .into_bytes(),
    }
}

fn wait_for_child_exit(child: &Child, timeout: Duration) -> bool {
    // SAFETY: `kqueue` has no pointer arguments and returns a fresh descriptor
    // on success, which is transferred immediately into `OwnedFd`.
    let descriptor = unsafe { libc::kqueue() };
    assert!(descriptor >= 0, "test kqueue should be created");
    // SAFETY: `descriptor` is the fresh owned descriptor returned above.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let child_id = usize::try_from(child.id()).expect("launcher PID should fit");
    let change = libc::kevent {
        ident: child_id,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: `change` is one initialized registration event and no result
    // buffer is requested by this call.
    let registered = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &raw const change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    assert_eq!(registered, 0, "child exit event should register");

    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("test timeout should fit Instant");
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = libc::timespec {
            tv_sec: libc::time_t::try_from(remaining.as_secs())
                .expect("timeout seconds should fit"),
            tv_nsec: libc::c_long::from(remaining.subsec_nanos()),
        };
        let mut event = MaybeUninit::<libc::kevent>::uninit();
        // SAFETY: `event` has room for one result and is read only when the
        // kernel reports that it initialized exactly one entry.
        let count = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                &raw const timeout,
            )
        };
        if count == 1 {
            // SAFETY: `kevent` reported one initialized result above.
            let event = unsafe { event.assume_init() };
            let event_filter = event.filter;
            let event_ident = event.ident;
            let event_fflags = event.fflags;
            assert_eq!(event_filter, libc::EVFILT_PROC);
            assert_eq!(event_ident, child_id);
            assert_ne!(event_fflags & libc::NOTE_EXIT, 0);
            return true;
        }
        if count == 0 {
            return false;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            panic!("waiting for child exit failed: {error:?}");
        }
    }
}

fn kill_child_group(child: &mut Child) {
    let pid = i32::try_from(child.id()).expect("launcher PID should fit");
    // SAFETY: Test children are leaders of fresh process groups. The leader
    // remains unreaped here, so its PID/group id cannot be reused while
    // SIGKILL bounds both launcher and nested worker cleanup.
    let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
}

fn http_get(socket: &Path, path: &str) -> String {
    let mut stream = UnixStream::connect(socket).expect("API socket should accept connections");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("API read timeout should be configured");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("HTTP request should be written");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("HTTP request write should close");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("HTTP response should be read");
    response
}

fn assert_invalid_bundle(output: Output) {
    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid production bundle entry")
            || stderr.contains("production bundle signature validation failed"),
        "expected stable package rejection; stderr:\n{stderr}"
    );
    assert!(!stdout.contains("Usage:\n  bangbang [OPTIONS]"));
    assert!(!stdout.contains("status: API server listening"));
}

fn resign_worker_and_outer(
    bundle: &Path,
    worker_entitlements: &[u8],
    worker_runtime: bool,
    outer_runtime: bool,
) {
    let entitlement_file = bundle
        .parent()
        .expect("test bundle should have a parent")
        .join("worker.entitlements.plist");
    fs::write(&entitlement_file, worker_entitlements)
        .expect("replacement entitlements should be written");
    let worker = worker_bundle(bundle);
    let mut worker_sign = Command::new("/usr/bin/codesign");
    worker_sign.args(["--force", "--sign", "-"]);
    if worker_runtime {
        worker_sign.args(["--options", "runtime"]);
    }
    let worker_sign = worker_sign
        .arg("--entitlements")
        .arg(&entitlement_file)
        .arg(&worker)
        .output()
        .expect("replacement worker signing should execute");
    assert_output_success(&worker_sign, "replacement worker signing");
    let mut outer_sign = Command::new("/usr/bin/codesign");
    outer_sign.args(["--force", "--sign", "-"]);
    if outer_runtime {
        outer_sign.args(["--options", "runtime"]);
    }
    let outer_sign = outer_sign
        .arg(bundle)
        .output()
        .expect("replacement outer signing should execute");
    assert_output_success(&outer_sign, "replacement outer signing");
}

fn codesign_display(path: &Path) -> String {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(path)
        .output()
        .expect("codesign display should execute");
    assert_output_success(&output, "codesign display");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn codesign_entitlements(path: &Path) -> String {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--entitlements", "-", "--xml"])
        .arg(path)
        .output()
        .expect("codesign entitlement display should execute");
    assert_output_success(&output, "codesign entitlement display");
    String::from_utf8(output.stdout).expect("entitlements should be UTF-8")
}

fn assert_output_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} should succeed; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    let metadata = fs::symlink_metadata(source).expect("source metadata should exist");
    assert!(!metadata.file_type().is_symlink());
    if metadata.is_file() {
        fs::copy(source, destination).expect("file should copy");
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
        )
        .expect("file permissions should copy");
        return;
    }
    assert!(metadata.is_dir());
    fs::create_dir(destination).expect("destination directory should be created");
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
    )
    .expect("directory permissions should copy");
    let mut entries = fs::read_dir(source)
        .expect("source directory should be readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("source entries should be readable");
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let entry_metadata =
            fs::symlink_metadata(&source_path).expect("entry metadata should exist");
        if entry_metadata.file_type().is_symlink() {
            let target = fs::read_link(&source_path).expect("symlink target should be readable");
            symlink(target, destination_path).expect("symlink should copy");
        } else {
            copy_tree(&source_path, &destination_path);
        }
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("test path should be UTF-8")
}

#[derive(Debug)]
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "bangbang-production-e2e-{}-{id}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
