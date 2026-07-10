#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_BANGBANG_BIN: &str = env!("CARGO_BIN_EXE_bangbang");
const BANGBANG_PROCESS_E2E_BIN_ENV: &str = "BANGBANG_PROCESS_E2E_BIN";
const API_STARTUP_READY_LINE: &str = "status: API server listening";
const NO_API_STARTUP_READY_LINE: &str = "status: VM running without API";
const STARTUP_READY_LINES: &[&str] = &[API_STARTUP_READY_LINE, NO_API_STARTUP_READY_LINE];
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn http_get(socket_path: &Path, path: &str) -> String {
    http_no_body(socket_path, "GET", path)
}

pub(crate) fn http_no_body(socket_path: &Path, method: &str, path: &str) -> String {
    http_request(socket_path, method, path, None)
}

pub(crate) fn http_put_json(socket_path: &Path, path: &str, body: &str) -> String {
    http_json(socket_path, "PUT", path, body)
}

pub(crate) fn http_json(socket_path: &Path, method: &str, path: &str, body: &str) -> String {
    http_request(socket_path, method, path, Some(body))
}

pub(crate) fn http_json_with_io_timeout(
    socket_path: &Path,
    method: &str,
    path: &str,
    body: &str,
    io_timeout: Duration,
) -> String {
    http_request_with_io_timeout(socket_path, method, path, Some(body), io_timeout)
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled once per test target"
)]
pub(crate) fn http_raw(socket_path: &Path, request: &[u8]) -> String {
    http_raw_with_io_timeout(socket_path, request, HTTP_IO_TIMEOUT)
}

fn http_raw_with_io_timeout(socket_path: &Path, request: &[u8], io_timeout: Duration) -> String {
    let mut stream = UnixStream::connect(socket_path)
        .unwrap_or_else(|err| panic!("client should connect to {}: {err}", socket_path.display()));
    stream
        .set_read_timeout(Some(io_timeout))
        .unwrap_or_else(|err| panic!("client should set read timeout {io_timeout:?}: {err}"));
    stream
        .set_write_timeout(Some(io_timeout))
        .unwrap_or_else(|err| panic!("client should set write timeout {io_timeout:?}: {err}"));
    stream.write_all(request).unwrap_or_else(|err| {
        panic!(
            "client should write {} request bytes within {io_timeout:?}: {err}",
            request.len()
        )
    });

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .unwrap_or_else(|err| panic!("client should read response within {io_timeout:?}: {err}"));
    response
}

fn http_request(socket_path: &Path, method: &str, path: &str, body: Option<&str>) -> String {
    http_request_with_io_timeout(socket_path, method, path, body, HTTP_IO_TIMEOUT)
}

fn http_request_with_io_timeout(
    socket_path: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
    io_timeout: Duration,
) -> String {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(body) = body {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        request.push_str("\r\n");
        request.push_str(body);
    } else {
        request.push_str("\r\n");
    }

    http_raw_with_io_timeout(socket_path, request.as_bytes(), io_timeout)
}

pub(crate) fn path_text(path: &Path) -> &str {
    path.to_str().expect("test path should be valid UTF-8")
}

pub(crate) fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                write!(&mut escaped, "\\u{:04x}", u32::from(control))
                    .expect("writing to String should succeed");
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

pub(crate) fn assert_clean_shutdown(
    output: CompletedProcess,
    socket_path: &Path,
    process_name: &str,
) {
    assert!(
        output.status.success(),
        "{process_name} shutdown signal should make bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "{process_name} should remove its owned API socket on normal shutdown"
    );
}

pub(crate) fn assert_ok_response(response: &str, request_name: &str) {
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "{request_name} should return 200 OK; response:\n{response}"
    );
}

pub(crate) fn assert_no_content_response(response: &str, request_name: &str) {
    assert!(
        response.starts_with("HTTP/1.1 204 No Content\r\n"),
        "{request_name} should return 204 No Content; response:\n{response}"
    );
    assert_response_contains(response, "Content-Length: 0\r\n", request_name);
    assert!(
        response.ends_with("\r\n\r\n"),
        "{request_name} should not return a response body; response:\n{response}"
    );
}

pub(crate) fn assert_bad_request_response(response: &str, request_name: &str) {
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "{request_name} should return 400 Bad Request; response:\n{response}"
    );
}

pub(crate) fn assert_response_contains(response: &str, expected: &str, request_name: &str) {
    assert!(
        response.contains(expected),
        "{request_name} response should contain {expected:?}; response:\n{response}"
    );
}

#[derive(Debug)]
pub(crate) struct TestDir {
    path: PathBuf,
    id: u64,
}

impl TestDir {
    pub(crate) fn new() -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_millis();
        let path = std::env::temp_dir().join(format!("bb-{}-{id}-{millis}", std::process::id()));

        fs::create_dir(&path).expect("temporary test directory should be created");

        Self { path, id }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn instance_id(&self) -> String {
        format!("process-e2e-{}", self.id)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
pub(crate) struct BangbangProcess {
    binary_path: PathBuf,
    child: Option<Child>,
    stdout: Option<OutputReader>,
    stderr: Option<OutputReader>,
    ready: Receiver<()>,
}

impl BangbangProcess {
    pub(crate) fn start(socket_path: &Path, instance_id: &str) -> Self {
        Self::start_with_extra_args(socket_path, instance_id, &[])
    }

    pub(crate) fn start_with_extra_args(
        socket_path: &Path,
        instance_id: &str,
        extra_args: &[&str],
    ) -> Self {
        let mut process = Self::spawn_with_extra_args(socket_path, instance_id, extra_args);
        process.wait_until_ready();
        process
    }

    #[allow(
        dead_code,
        reason = "shared integration-test support is compiled once per test target"
    )]
    pub(crate) fn start_expect_failure(socket_path: &Path, instance_id: &str) -> CompletedProcess {
        let mut process = Self::spawn(socket_path, instance_id);

        process.wait_for_startup_failure()
    }

    #[allow(
        dead_code,
        reason = "shared integration-test support is compiled once per test target"
    )]
    pub(crate) fn start_with_extra_args_expect_failure(
        socket_path: &Path,
        instance_id: &str,
        extra_args: &[&str],
    ) -> CompletedProcess {
        let mut process = Self::spawn_with_extra_args(socket_path, instance_id, extra_args);

        process.wait_for_startup_failure()
    }

    fn wait_for_startup_failure(&mut self) -> CompletedProcess {
        match self.ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(()) => {
                let output = self.force_stop_and_collect();
                panic!(
                    "bangbang reported startup readiness but startup failure was expected; binary: {}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    self.binary_path.display(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
            Err(RecvTimeoutError::Disconnected) => {
                let child = self.child.take().expect("child should still be owned");
                let status = wait_for_child_exit(child, SHUTDOWN_TIMEOUT);
                self.collect_output(status)
            }
            Err(RecvTimeoutError::Timeout) => {
                let output = self.force_stop_and_collect();
                panic!(
                    "bangbang did not fail before timeout; binary: {}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    self.binary_path.display(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
        }
    }

    fn spawn(socket_path: &Path, instance_id: &str) -> Self {
        Self::spawn_with_extra_args(socket_path, instance_id, &[])
    }

    fn spawn_with_extra_args(socket_path: &Path, instance_id: &str, extra_args: &[&str]) -> Self {
        let binary_path = bangbang_bin();
        let mut child = Command::new(&binary_path)
            .arg("--api-sock")
            .arg(socket_path)
            .arg("--id")
            .arg(instance_id)
            .args(extra_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| {
                panic!(
                    "bangbang process should start from {}: {err}",
                    binary_path.display()
                )
            });

        let stdout = child.stdout.take().expect("stdout should be piped");
        let stderr = child.stderr.take().expect("stderr should be piped");
        let (stdout, ready) = OutputReader::stdout(stdout);
        let stderr = OutputReader::stderr(stderr);
        Self {
            binary_path,
            child: Some(child),
            stdout: Some(stdout),
            stderr: Some(stderr),
            ready,
        }
    }

    fn wait_until_ready(&mut self) {
        match self.ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(()) => {}
            Err(err) => {
                let output = self.force_stop_and_collect();
                panic!(
                    "bangbang did not report startup readiness before timeout: {err:?}; binary: {}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    self.binary_path.display(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
        }
    }

    pub(crate) fn terminate(self) -> CompletedProcess {
        self.stop_with_signal(libc::SIGTERM, "SIGTERM")
    }

    #[allow(
        dead_code,
        reason = "shared integration-test support is compiled once per test target"
    )]
    pub(crate) fn wait_for_exit(mut self) -> CompletedProcess {
        let child = self.child.take().expect("child should still be owned");
        let status = wait_for_child_exit(child, SHUTDOWN_TIMEOUT);

        self.collect_output(status)
    }

    #[allow(
        dead_code,
        reason = "shared integration-test support is compiled once per test target"
    )]
    pub(crate) fn interrupt(self) -> CompletedProcess {
        self.stop_with_signal(libc::SIGINT, "SIGINT")
    }

    pub(crate) fn send_signal(&self, signal: i32, signal_name: &str) {
        let child = self.child.as_ref().expect("child should still be running");
        if let Err(err) = send_signal(child.id(), signal) {
            panic!("{signal_name} should be delivered: {err}");
        }
    }

    #[allow(
        dead_code,
        reason = "shared integration-test support is compiled once per test target"
    )]
    pub(crate) fn stop_with_signal(mut self, signal: i32, signal_name: &str) -> CompletedProcess {
        self.send_signal(signal, signal_name);
        let child = self.child.take().expect("child should still be owned");
        let status = wait_for_child_exit(child, SHUTDOWN_TIMEOUT);

        self.collect_output(status)
    }

    pub(crate) fn force_stop_and_collect(&mut self) -> CompletedProcess {
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

fn bangbang_bin() -> PathBuf {
    match std::env::var_os(BANGBANG_PROCESS_E2E_BIN_ENV) {
        Some(path) if path.is_empty() => {
            panic!("{BANGBANG_PROCESS_E2E_BIN_ENV} must not be empty")
        }
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(DEFAULT_BANGBANG_BIN),
    }
}

#[derive(Debug)]
pub(crate) struct CompletedProcess {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
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
                        if STARTUP_READY_LINES
                            .iter()
                            .any(|ready_line| line.contains(ready_line))
                        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_json_with_io_timeout_reads_response_from_unix_socket() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let listener =
            UnixListener::bind(&socket_path).expect("test API socket should be bindable");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server should accept client");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 64];

            while !request.ends_with(b"\r\n\r\n{}") {
                let read = stream
                    .read(&mut buffer)
                    .expect("server should read request bytes");
                assert_ne!(read, 0, "client should send request before closing");
                request.extend_from_slice(&buffer[..read]);
                assert!(
                    request.len() <= 512,
                    "helper request should stay bounded: {} bytes",
                    request.len()
                );
            }

            let request = String::from_utf8(request).expect("request should be UTF-8");
            assert!(request.starts_with("PATCH /hotplug/memory HTTP/1.1\r\n"));
            assert!(request.contains("Connection: close\r\n"));
            assert!(request.contains("Content-Length: 2\r\n"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .expect("server should write response");
        });

        let response = http_json_with_io_timeout(
            &socket_path,
            "PATCH",
            "/hotplug/memory",
            "{}",
            Duration::from_secs(1),
        );

        assert_no_content_response(&response, "fake PATCH /hotplug/memory");
        server.join().expect("server thread should join");
    }
}
