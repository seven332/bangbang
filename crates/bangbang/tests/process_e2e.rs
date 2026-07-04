// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BANGBANG_BIN: &str = env!("CARGO_BIN_EXE_bangbang");
const BANGBANG_VERSION: &str = env!("CARGO_PKG_VERSION");
const STARTUP_READY_LINE: &str = "status: API server listening";
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn executable_serves_api_and_shuts_down_cleanly() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    assert!(
        socket_path.exists(),
        "bangbang should publish the configured API socket"
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET /");
    assert_response_contains(&instance_info, r#""app_name":"bangbang""#, "GET /");
    assert_response_contains(&instance_info, &format!(r#""id":"{instance_id}""#), "GET /");
    assert_response_contains(&instance_info, r#""state":"Not started""#, "GET /");
    assert_response_contains(
        &instance_info,
        &format!(r#""vmm_version":"{BANGBANG_VERSION}""#),
        "GET /",
    );

    let version = http_get(&socket_path, "/version");
    assert_ok_response(&version, "GET /version");
    assert_response_contains(
        &version,
        &format!(r#""firecracker_version":"{BANGBANG_VERSION}""#),
        "GET /version",
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config");
    assert_response_contains(&vm_config, r#""machine-config":"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""drives":[]"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""network-interfaces":[]"#, "GET /vm/config");

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn concurrent_executables_keep_api_sockets_isolated() {
    let first_dir = TestDir::new();
    let second_dir = TestDir::new();
    let first_socket_path = first_dir.path().join("api.socket");
    let second_socket_path = second_dir.path().join("api.socket");
    let first_instance_id = first_dir.instance_id();
    let second_instance_id = second_dir.instance_id();

    let first_bangbang = BangbangProcess::start(&first_socket_path, &first_instance_id);
    let second_bangbang = BangbangProcess::start(&second_socket_path, &second_instance_id);

    assert_instance_info_matches(
        &first_socket_path,
        &first_instance_id,
        &second_instance_id,
        "first bangbang",
    );
    assert_instance_info_matches(
        &second_socket_path,
        &second_instance_id,
        &first_instance_id,
        "second bangbang",
    );

    let first_output = first_bangbang.terminate();
    let second_output = second_bangbang.terminate();
    assert_clean_shutdown(first_output, &first_socket_path, "first bangbang");
    assert_clean_shutdown(second_output, &second_socket_path, "second bangbang");
}

fn http_get(socket_path: &Path, path: &str) -> String {
    let mut stream = UnixStream::connect(socket_path).expect("client should connect");
    stream
        .set_read_timeout(Some(HTTP_IO_TIMEOUT))
        .expect("client should set read timeout");
    stream
        .set_write_timeout(Some(HTTP_IO_TIMEOUT))
        .expect("client should set write timeout");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");

    stream
        .write_all(request.as_bytes())
        .expect("client should write request");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("client should read response");
    response
}

fn assert_instance_info_matches(
    socket_path: &Path,
    expected_instance_id: &str,
    unexpected_instance_id: &str,
    process_name: &str,
) {
    let response = http_get(socket_path, "/");
    assert_ok_response(&response, process_name);
    assert_response_contains(
        &response,
        &format!(r#""id":"{expected_instance_id}""#),
        process_name,
    );
    assert!(
        !response.contains(&format!(r#""id":"{unexpected_instance_id}""#)),
        "{process_name} response should not contain another process id; response:\n{response}"
    );
}

fn assert_clean_shutdown(output: CompletedProcess, socket_path: &Path, process_name: &str) {
    assert!(
        output.status.success(),
        "{process_name} SIGTERM should make bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "{process_name} should remove its owned API socket on normal shutdown"
    );
}

fn assert_ok_response(response: &str, request_name: &str) {
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "{request_name} should return 200 OK; response:\n{response}"
    );
}

fn assert_response_contains(response: &str, expected: &str, request_name: &str) {
    assert!(
        response.contains(expected),
        "{request_name} response should contain {expected:?}; response:\n{response}"
    );
}

#[derive(Debug)]
struct TestDir {
    path: PathBuf,
    id: u64,
}

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_millis();
        let path = std::env::temp_dir().join(format!("bb-{}-{id}-{millis}", std::process::id()));

        fs::create_dir(&path).expect("temporary test directory should be created");

        Self { path, id }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn instance_id(&self) -> String {
        format!("process-e2e-{}", self.id)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
struct BangbangProcess {
    child: Option<Child>,
    stdout: Option<OutputReader>,
    stderr: Option<OutputReader>,
    ready: Receiver<()>,
}

impl BangbangProcess {
    fn start(socket_path: &Path, instance_id: &str) -> Self {
        let mut child = Command::new(BANGBANG_BIN)
            .arg("--api-sock")
            .arg(socket_path)
            .arg("--id")
            .arg(instance_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bangbang process should start");

        let stdout = child.stdout.take().expect("stdout should be piped");
        let stderr = child.stderr.take().expect("stderr should be piped");
        let (stdout, ready) = OutputReader::stdout(stdout);
        let stderr = OutputReader::stderr(stderr);
        let mut process = Self {
            child: Some(child),
            stdout: Some(stdout),
            stderr: Some(stderr),
            ready,
        };

        process.wait_until_ready();
        process
    }

    fn wait_until_ready(&mut self) {
        match self.ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(()) => {}
            Err(err) => {
                let output = self.force_stop_and_collect();
                panic!(
                    "bangbang did not report API readiness before timeout: {err:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }
    }

    fn terminate(mut self) -> CompletedProcess {
        let child = self.child.as_ref().expect("child should still be running");
        send_signal(child.id(), libc::SIGTERM).expect("SIGTERM should be delivered");
        let child = self.child.take().expect("child should still be owned");
        let status = wait_for_child_exit(child, SHUTDOWN_TIMEOUT);

        self.collect_output(status)
    }

    fn force_stop_and_collect(&mut self) -> CompletedProcess {
        let child = self.child.take().expect("child should still be owned");
        force_kill(child.id());
        let status = wait_for_child_exit(child, SHUTDOWN_TIMEOUT);

        self.collect_output(status)
    }

    fn collect_output(&mut self, status: ExitStatus) -> CompletedProcess {
        let stdout = self
            .stdout
            .take()
            .expect("stdout reader should be present")
            .join();
        let stderr = self
            .stderr
            .take()
            .expect("stderr reader should be present")
            .join();

        CompletedProcess {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for BangbangProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.try_join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.try_join();
        }
    }
}

#[derive(Debug)]
struct CompletedProcess {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct OutputReader {
    handle: JoinHandle<String>,
}

impl OutputReader {
    fn stdout(stdout: impl Read + Send + 'static) -> (Self, Receiver<()>) {
        let (ready_sender, ready_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut output = String::new();
            let mut reader = BufReader::new(stdout);

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.contains(STARTUP_READY_LINE) {
                            let _ = ready_sender.send(());
                        }
                        output.push_str(&line);
                    }
                    Err(err) => {
                        output.push_str(&format!("\n<stdout read error: {err}>\n"));
                        break;
                    }
                }
            }

            output
        });

        (Self { handle }, ready_receiver)
    }

    fn stderr(stderr: impl Read + Send + 'static) -> Self {
        let handle = thread::spawn(move || {
            let mut output = String::new();
            let mut reader = BufReader::new(stderr);
            match reader.read_to_string(&mut output) {
                Ok(_) => output,
                Err(err) => format!("<stderr read error: {err}>"),
            }
        });

        Self { handle }
    }

    fn join(self) -> String {
        self.handle
            .join()
            .expect("output reader thread should join")
    }

    fn try_join(self) -> Result<String, Box<dyn std::any::Any + Send + 'static>> {
        self.handle.join()
    }
}

fn wait_for_child_exit(child: Child, timeout: Duration) -> ExitStatus {
    let pid = child.id();
    let (status_sender, status_receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let mut child = child;
        let status = child.wait();
        let _ = status_sender.send(status);
    });

    let status = match status_receiver.recv_timeout(timeout) {
        Ok(status) => status.expect("child wait should succeed"),
        Err(RecvTimeoutError::Timeout) => {
            force_kill(pid);
            status_receiver
                .recv_timeout(timeout)
                .expect("child should exit after SIGKILL")
                .expect("child wait after SIGKILL should succeed")
        }
        Err(RecvTimeoutError::Disconnected) => panic!("child wait thread disconnected"),
    };
    waiter.join().expect("child wait thread should join");

    status
}

fn send_signal(pid: u32, signal: i32) -> std::io::Result<()> {
    let pid = i32::try_from(pid).expect("child pid should fit in pid_t");

    // SAFETY: `pid` is the process id returned by `std::process::Child`, and
    // `signal` is supplied by libc signal constants used only for this child.
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn force_kill(pid: u32) {
    if let Err(err) = send_signal(pid, libc::SIGKILL)
        && err.raw_os_error() != Some(libc::ESRCH)
    {
        panic!("failed to force-kill bangbang child {pid}: {err}");
    }
}
