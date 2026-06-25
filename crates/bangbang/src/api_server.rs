use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bangbang_api::http::{handle_request_bytes, request_total_len, HttpResponse, RequestError};
use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;

const READ_CHUNK_SIZE: usize = 4096;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

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

        let listener = UnixListener::bind(path).map_err(|err| ApiServerError::Bind(err.kind()))?;
        let socket_guard = SocketGuard::new(path)?;

        Ok(Self {
            listener,
            _socket_guard: socket_guard,
        })
    }

    pub(crate) fn run(&self, version: &str) -> Result<(), ApiServerError> {
        loop {
            self.serve_next(version)?;
        }
    }

    fn serve_next(&self, version: &str) -> Result<(), ApiServerError> {
        let (mut stream, _) = self
            .listener
            .accept()
            .map_err(|err| ApiServerError::Accept(err.kind()))?;

        let _ = handle_connection(&mut stream, version);

        Ok(())
    }
}

#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketGuard {
    fn new(path: &Path) -> Result<Self, ApiServerError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|err| ApiServerError::SocketMetadata(err.kind()))?;

        if !metadata.file_type().is_socket() {
            return Err(ApiServerError::SocketPathIsNotSocket);
        }

        Ok(Self {
            path: path.to_path_buf(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }

    fn owns_current_path(&self) -> bool {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return false;
        };

        metadata.file_type().is_socket() && metadata.dev() == self.dev && metadata.ino() == self.ino
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.owns_current_path() {
            let _ = fs::remove_file(&self.path);
        }
    }
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
        let Some(read_timeout) = deadline.checked_duration_since(Instant::now()) else {
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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    const VERSION: &str = "0.1.0";

    fn unique_socket_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        env::temp_dir().join(format!("bb-{name}-{}-{nanos}.sock", std::process::id()))
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
    fn request_read_timeout_applies_to_total_request() {
        let (mut client, mut server) = UnixStream::pair().expect("stream pair should be created");
        let partial_request = b"GET /version HTTP/1.1\r\n";

        client
            .write_all(partial_request)
            .expect("client should write partial request");

        let request = read_request(&mut server, Duration::from_millis(10))
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
