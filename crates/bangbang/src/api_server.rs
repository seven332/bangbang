use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bangbang_api::http::{handle_request_bytes, request_total_len, HttpResponse, RequestError};
use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;

const READ_CHUNK_SIZE: usize = 4096;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_TEMP_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiServerError {
    Accept(std::io::ErrorKind),
    Bind(std::io::ErrorKind),
    Connection(std::io::ErrorKind),
    SocketMetadata(std::io::ErrorKind),
    SocketPathCheck(std::io::ErrorKind),
    SocketPathExists,
    SocketPathIsNotSocket,
}

impl std::fmt::Display for ApiServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accept(kind) => write!(f, "failed to accept API connection: {kind:?}"),
            Self::Bind(kind) => write!(f, "failed to bind API socket: {kind:?}"),
            Self::Connection(kind) => write!(f, "API connection I/O failed: {kind:?}"),
            Self::SocketMetadata(kind) => {
                write!(f, "failed to inspect bound API socket: {kind:?}")
            }
            Self::SocketPathCheck(kind) => write!(f, "failed to check API socket path: {kind:?}"),
            Self::SocketPathExists => f.write_str("API socket path already exists"),
            Self::SocketPathIsNotSocket => f.write_str("bound API path is not a socket"),
        }
    }
}

impl std::error::Error for ApiServerError {}

#[derive(Debug)]
pub(crate) struct ApiServer {
    listener: UnixListener,
    _socket_guard: SocketGuard,
}

impl ApiServer {
    pub(crate) fn bind(path: impl AsRef<Path>) -> Result<Self, ApiServerError> {
        let path = path.as_ref();

        if path_exists_without_following_links(path)? {
            return Err(ApiServerError::SocketPathExists);
        }

        let (listener, metadata) = bind_unpublished_socket(path)?;
        publish_socket_path(&metadata.path, path).inspect_err(|_| {
            let _ = fs::remove_file(&metadata.path);
        })?;
        let socket_guard = SocketGuard::new(path, metadata);

        Ok(Self {
            listener,
            _socket_guard: socket_guard,
        })
    }

    pub(crate) fn run_until(
        &self,
        version: &str,
        shutdown_requested: &AtomicBool,
        shutdown_wakeup: &mut UnixStream,
    ) -> Result<(), ApiServerError> {
        self.listener
            .set_nonblocking(true)
            .map_err(|err| ApiServerError::Accept(err.kind()))?;
        shutdown_wakeup
            .set_nonblocking(true)
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        loop {
            if shutdown_requested.load(Ordering::Relaxed) {
                return Ok(());
            }

            wait_for_listener_or_shutdown(&self.listener, shutdown_wakeup)?;
            if drain_shutdown_wakeup(shutdown_wakeup)? {
                return Ok(());
            }

            match self.serve_next(version) {
                Ok(()) => {}
                Err(ApiServerError::Accept(std::io::ErrorKind::WouldBlock)) => {}
                Err(ApiServerError::Accept(std::io::ErrorKind::Interrupted)) => {}
                Err(err) => return Err(err),
            }
        }
    }

    fn serve_next(&self, version: &str) -> Result<(), ApiServerError> {
        let (mut stream, _) = self
            .listener
            .accept()
            .map_err(|err| ApiServerError::Accept(err.kind()))?;
        stream
            .set_nonblocking(false)
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        let _ = handle_connection(&mut stream, version);

        Ok(())
    }
}

fn wait_for_listener_or_shutdown(
    listener: &UnixListener,
    shutdown_wakeup: &UnixStream,
) -> Result<(), ApiServerError> {
    let mut poll_fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown_wakeup.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    loop {
        for poll_fd in &mut poll_fds {
            poll_fd.revents = 0;
        }

        // SAFETY: `poll_fds` points to two initialized `pollfd` values and
        // remains valid for the duration of the call. The timeout is infinite.
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, -1) };
        if result > 0 {
            return Ok(());
        }

        let kind = std::io::Error::last_os_error().kind();
        if kind != std::io::ErrorKind::Interrupted {
            return Err(ApiServerError::Accept(kind));
        }
    }
}

fn drain_shutdown_wakeup(shutdown_wakeup: &mut UnixStream) -> Result<bool, ApiServerError> {
    let mut drained = false;
    let mut buffer = [0; 64];

    loop {
        match shutdown_wakeup.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(_) => drained = true,
            Err(err) if matches!(err.kind(), std::io::ErrorKind::WouldBlock) => {
                return Ok(drained);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(ApiServerError::Connection(err.kind())),
        }
    }
}

#[derive(Debug)]
struct BoundSocketMetadata {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketGuard {
    fn new(path: &Path, metadata: BoundSocketMetadata) -> Self {
        Self {
            path: path.to_path_buf(),
            dev: metadata.dev,
            ino: metadata.ino,
        }
    }

    fn owns_current_path(&self) -> bool {
        let Ok(metadata) = socket_path_metadata(&self.path) else {
            return false;
        };

        metadata.dev() == self.dev && metadata.ino() == self.ino
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.owns_current_path() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn socket_path_metadata(path: &Path) -> Result<fs::Metadata, ApiServerError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|err| ApiServerError::SocketMetadata(err.kind()))?;

    if !metadata.file_type().is_socket() {
        return Err(ApiServerError::SocketPathIsNotSocket);
    }

    Ok(metadata)
}

fn bind_unpublished_socket(
    path: &Path,
) -> Result<(UnixListener, BoundSocketMetadata), ApiServerError> {
    for _ in 0..16 {
        let temp_path = next_temporary_socket_path(path);
        let listener = match UnixListener::bind(&temp_path) {
            Ok(listener) => listener,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists
                ) =>
            {
                continue;
            }
            Err(err) => return Err(ApiServerError::Bind(err.kind())),
        };
        let metadata = match socket_path_metadata(&temp_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                let _ = fs::remove_file(&temp_path);
                return Err(err);
            }
        };

        return Ok((
            listener,
            BoundSocketMetadata {
                path: temp_path,
                dev: metadata.dev(),
                ino: metadata.ino(),
            },
        ));
    }

    Err(ApiServerError::Bind(std::io::ErrorKind::AlreadyExists))
}

fn next_temporary_socket_path(path: &Path) -> PathBuf {
    next_temporary_socket_path_from(path, &NEXT_TEMP_SOCKET_ID)
}

fn next_temporary_socket_path_from(path: &Path, next_id: &AtomicU64) -> PathBuf {
    loop {
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        let temp_path = temporary_socket_path(path, id);
        if temp_path != path {
            return temp_path;
        }
    }
}

fn temporary_socket_path(path: &Path, id: u64) -> PathBuf {
    let mut temp_name = OsString::from(".bb.");
    temp_name.push(format!("{}.{}", std::process::id(), id));

    path.with_file_name(temp_name)
}

#[cfg(target_os = "macos")]
fn publish_socket_path(from: &Path, to: &Path) -> Result<(), ApiServerError> {
    use std::os::raw::{c_char, c_int, c_uint};

    const RENAME_EXCL: c_uint = 0x0000_0004;

    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }

    let from = path_to_cstring(from)?;
    let to = path_to_cstring(to)?;
    // SAFETY: both pointers come from live `CString` values and are valid
    // NUL-terminated paths for the duration of this call.
    let result = unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) };
    if result == 0 {
        return Ok(());
    }

    let kind = std::io::Error::last_os_error().kind();
    if kind == std::io::ErrorKind::AlreadyExists {
        Err(ApiServerError::SocketPathExists)
    } else {
        Err(ApiServerError::Bind(kind))
    }
}

#[cfg(not(target_os = "macos"))]
fn publish_socket_path(from: &Path, to: &Path) -> Result<(), ApiServerError> {
    if path_exists_without_following_links(to)? {
        return Err(ApiServerError::SocketPathExists);
    }

    fs::rename(from, to).map_err(|err| ApiServerError::Bind(err.kind()))
}

fn path_to_cstring(path: &Path) -> Result<CString, ApiServerError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ApiServerError::Bind(std::io::ErrorKind::InvalidInput))
}

fn path_exists_without_following_links(path: &Path) -> Result<bool, ApiServerError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(ApiServerError::SocketPathCheck(err.kind())),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RequestRead {
    Complete(Vec<u8>),
    TooLarge,
}

fn handle_connection(stream: &mut UnixStream, version: &str) -> Result<(), ApiServerError> {
    stream
        .set_write_timeout(Some(CONNECTION_TIMEOUT))
        .map_err(|err| ApiServerError::Connection(err.kind()))?;

    let response = match read_request(stream, CONNECTION_TIMEOUT)? {
        RequestRead::Complete(request) => handle_request_bytes(&request, version),
        RequestRead::TooLarge => HttpResponse::fault(RequestError::PayloadTooLarge.fault_message()),
    };

    stream
        .write_all(&response.to_http_bytes())
        .map_err(|err| ApiServerError::Connection(err.kind()))
}

fn read_request(stream: &mut UnixStream, timeout: Duration) -> Result<RequestRead, ApiServerError> {
    let deadline = Instant::now() + timeout;
    let mut now = Instant::now;

    read_request_until(stream, deadline, &mut now)
}

fn read_request_until(
    stream: &mut UnixStream,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> Result<RequestRead, ApiServerError> {
    let mut request = Vec::new();
    let mut chunk = [0; READ_CHUNK_SIZE];

    loop {
        match request_total_len(&request) {
            Ok(Some(total_len)) if request.len() >= total_len => {
                request.truncate(total_len);
                return Ok(RequestRead::Complete(request));
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(RequestError::PayloadTooLarge) => return Ok(RequestRead::TooLarge),
            Err(_) => return Ok(RequestRead::Complete(request)),
        }

        let remaining = HTTP_MAX_PAYLOAD_SIZE.saturating_sub(request.len());
        if remaining == 0 {
            return Ok(RequestRead::TooLarge);
        }

        let read_len = chunk.len().min(remaining);
        let Some(read_timeout) = deadline.checked_duration_since(now()) else {
            return Ok(RequestRead::Complete(request));
        };
        if read_timeout.is_zero() {
            return Ok(RequestRead::Complete(request));
        }
        stream
            .set_read_timeout(Some(read_timeout))
            .map_err(|err| ApiServerError::Connection(err.kind()))?;

        let bytes_read = match stream.read(&mut chunk[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(RequestRead::Complete(request));
            }
            Err(err) => return Err(ApiServerError::Connection(err.kind())),
        };

        if bytes_read == 0 {
            return Ok(RequestRead::Complete(request));
        }

        request.extend_from_slice(&chunk[..bytes_read]);
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    const VERSION: &str = "0.1.0";

    fn unique_socket_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        env::temp_dir().join(format!("bb-{name}-{}-{nanos}.sock", std::process::id()))
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = PathBuf::from("/tmp").join(format!("bb-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).expect("fixture directory should be created");
        path
    }

    fn temporary_socket_entries(dir: &Path) -> Vec<PathBuf> {
        let prefix = format!(".bb.{}.", std::process::id());
        let mut paths = fs::read_dir(dir)
            .expect("fixture directory should be readable")
            .filter_map(|entry| {
                let entry = entry.expect("fixture directory entry should be readable");
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(&prefix).then(|| entry.path())
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    #[test]
    fn temporary_socket_path_skips_requested_path_collision() {
        let id = 7;
        let path = PathBuf::from("/tmp").join(format!(".bb.{}.{}", std::process::id(), id));
        let next_id = AtomicU64::new(id);

        let temp_path = next_temporary_socket_path_from(&path, &next_id);

        assert_ne!(temp_path, path);
        assert_eq!(
            temp_path,
            PathBuf::from("/tmp").join(format!(".bb.{}.{}", std::process::id(), id + 1))
        );
        assert_eq!(next_id.load(Ordering::Relaxed), id + 2);
    }

    #[test]
    fn serves_version_over_unix_socket() {
        let path = unique_socket_path("version");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        server
            .serve_next(VERSION)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains(r#"{"firecracker_version":"0.1.0"}"#));
    }

    #[test]
    fn returns_fault_for_unsupported_path() {
        let path = unique_socket_path("fault");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        server
            .serve_next(VERSION)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains(r#"{"fault_message":"Invalid request method and/or path."}"#));
    }

    #[test]
    fn returns_fault_for_request_over_payload_limit() {
        let path = unique_socket_path("limit");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");
        let request = format!(
            "GET /version HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            HTTP_MAX_PAYLOAD_SIZE + 1
        );

        client
            .write_all(request.as_bytes())
            .expect("client should write request");
        server
            .serve_next(VERSION)
            .expect("server should handle one request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");

        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response
            .contains(r#"{"fault_message":"HTTP request payload exceeds the configured limit."}"#));
    }

    #[test]
    fn client_disconnect_does_not_fail_server() {
        let path = unique_socket_path("disconnect");
        let server = ApiServer::bind(&path).expect("server should bind");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");
        drop(client);

        server
            .serve_next(VERSION)
            .expect("client disconnect should not fail server");
    }

    #[test]
    fn run_until_cleans_socket_after_shutdown_request() {
        let path = unique_socket_path("shutdown");
        let server = ApiServer::bind(&path).expect("server should bind");
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let run_shutdown_requested = Arc::clone(&shutdown_requested);
        let (mut shutdown_reader, mut shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let mut client = UnixStream::connect(&path).expect("client should connect");

        let handle = thread::spawn(move || {
            server.run_until(VERSION, &run_shutdown_requested, &mut shutdown_reader)
        });

        client
            .write_all(b"GET /version HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("client should write request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("client should read response");
        shutdown_requested.store(true, Ordering::Relaxed);
        shutdown_writer
            .write_all(b"x")
            .expect("shutdown wakeup should be written");

        assert_eq!(
            handle.join().expect("server thread should not panic"),
            Ok(())
        );
        assert!(response.contains(r#"{"firecracker_version":"0.1.0"}"#));
        assert!(!path.exists());
    }

    #[test]
    fn run_until_cleans_idle_socket_after_shutdown_request() {
        let path = unique_socket_path("idle-shutdown");
        let server = ApiServer::bind(&path).expect("server should bind");
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let run_shutdown_requested = Arc::clone(&shutdown_requested);
        let (mut shutdown_reader, mut shutdown_writer) =
            UnixStream::pair().expect("shutdown stream pair should be created");
        let handle = thread::spawn(move || {
            server.run_until(VERSION, &run_shutdown_requested, &mut shutdown_reader)
        });

        shutdown_requested.store(true, Ordering::Relaxed);
        shutdown_writer
            .write_all(b"x")
            .expect("shutdown wakeup should be written");

        assert_eq!(
            handle.join().expect("server thread should not panic"),
            Ok(())
        );
        assert!(!path.exists());
    }

    #[test]
    fn request_read_timeout_returns_partial_request_after_expired_deadline() {
        let (mut client, mut server) = UnixStream::pair().expect("stream pair should be created");
        let partial_request = b"GET /version HTTP/1.1\r\n";

        client
            .write_all(partial_request)
            .expect("client should write partial request");

        let start = Instant::now();
        let deadline = start + Duration::from_secs(1);
        let mut first_now = true;
        let mut now = || {
            if std::mem::replace(&mut first_now, false) {
                start
            } else {
                deadline + Duration::from_nanos(1)
            }
        };

        let request = read_request_until(&mut server, deadline, &mut now)
            .expect("read timeout should not fail");

        assert_eq!(request, RequestRead::Complete(partial_request.to_vec()));
    }

    #[test]
    fn fails_when_socket_path_exists_without_deleting_it() {
        let path = unique_socket_path("exists");
        fs::write(&path, "existing file").expect("fixture file should be written");

        let err = ApiServer::bind(&path).expect_err("existing path should fail");

        assert_eq!(err, ApiServerError::SocketPathExists);
        assert_eq!(
            fs::read_to_string(&path).expect("existing file should remain"),
            "existing file"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn fails_when_socket_path_is_broken_symlink_without_deleting_it() {
        let path = unique_socket_path("symlink");
        let target = unique_socket_path("missing-target");
        std::os::unix::fs::symlink(&target, &path).expect("fixture symlink should be created");

        let err = ApiServer::bind(&path).expect_err("existing symlink path should fail");

        assert_eq!(err, ApiServerError::SocketPathExists);
        assert!(fs::symlink_metadata(&path)
            .expect("symlink should remain")
            .file_type()
            .is_symlink());

        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn publish_does_not_replace_existing_socket_path() {
        let path = unique_socket_path("publish-race");
        let temp_path = unique_socket_path("publish-temp");
        let temp_listener = UnixListener::bind(&temp_path).expect("temporary listener should bind");
        fs::write(&path, "replacement").expect("replacement should be written");

        let err = publish_socket_path(&temp_path, &path)
            .expect_err("publishing over an existing path should fail");

        assert_eq!(err, ApiServerError::SocketPathExists);
        assert_eq!(
            fs::read_to_string(&path).expect("replacement should remain"),
            "replacement"
        );
        assert!(temp_path.exists());

        drop(temp_listener);
        fs::remove_file(temp_path).expect("temporary socket should clean up");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn concurrent_binds_allow_only_one_owner() {
        const ATTEMPTS: usize = 8;

        let dir = unique_temp_dir("concurrent");
        let path = dir.join("api.sock");
        let start = Arc::new(Barrier::new(ATTEMPTS));
        let finish = Arc::new(Barrier::new(ATTEMPTS));
        let handles = (0..ATTEMPTS)
            .map(|_| {
                let path = path.clone();
                let start = Arc::clone(&start);
                let finish = Arc::clone(&finish);

                thread::spawn(move || {
                    start.wait();
                    let result = ApiServer::bind(&path);
                    let outcome = (
                        result.is_ok(),
                        matches!(
                            result.as_ref().err(),
                            Some(ApiServerError::SocketPathExists)
                        ),
                    );
                    finish.wait();
                    outcome
                })
            })
            .collect::<Vec<_>>();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("bind thread should not panic"))
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|(is_ok, _)| *is_ok).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|(_, is_path_exists)| *is_path_exists)
                .count(),
            ATTEMPTS - 1
        );
        assert!(!path.exists());
        assert_eq!(temporary_socket_entries(&dir), Vec::<PathBuf>::new());

        fs::remove_dir(dir).expect("fixture directory should clean up");
    }

    #[test]
    fn removes_owned_socket_on_drop() {
        let path = unique_socket_path("cleanup");
        let server = ApiServer::bind(&path).expect("server should bind");

        assert!(path.exists());

        drop(server);

        assert!(!path.exists());
    }

    #[test]
    fn does_not_remove_replaced_socket_path_on_drop() {
        let path = unique_socket_path("replaced");
        let server = ApiServer::bind(&path).expect("server should bind");

        fs::remove_file(&path).expect("socket path should be removable");
        fs::write(&path, "replacement").expect("replacement file should be written");

        drop(server);

        assert_eq!(
            fs::read_to_string(&path).expect("replacement should remain"),
            "replacement"
        );

        fs::remove_file(path).expect("fixture should clean up");
    }
}
