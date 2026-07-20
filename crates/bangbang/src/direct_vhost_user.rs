//! Bounded direct-process connection to an operator-selected vhost-user socket.

#[cfg(test)]
use std::ffi::OsStr;
use std::fmt;
#[cfg(test)]
use std::fs;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectVhostUserConnectError {
    InvalidPath,
    Timeout,
    Refused,
    Io(io::ErrorKind),
}

impl fmt::Display for DirectVhostUserConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => formatter.write_str("vhost-user socket path is invalid"),
            Self::Timeout => formatter.write_str("vhost-user socket connection timed out"),
            Self::Refused => formatter.write_str("vhost-user socket connection was refused"),
            Self::Io(_) => formatter.write_str("vhost-user socket connection failed"),
        }
    }
}

impl std::error::Error for DirectVhostUserConnectError {}

pub(crate) fn connect(
    path: &Path,
    timeout: Duration,
) -> Result<UnixStream, DirectVhostUserConnectError> {
    if timeout.is_zero() {
        return Err(DirectVhostUserConnectError::Timeout);
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(DirectVhostUserConnectError::Timeout)?;
    let address = unix_socket_address(path)?;
    let socket = nonblocking_cloexec_socket()?;

    loop {
        // SAFETY: `socket` is a live AF_UNIX stream descriptor. `address`
        // contains a fully initialized pathname address with an exact length.
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&raw const address.address).cast::<libc::sockaddr>(),
                address.len,
            )
        };
        if result == 0 {
            return Ok(UnixStream::from(socket));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            if Instant::now() >= deadline {
                return Err(DirectVhostUserConnectError::Timeout);
            }
            continue;
        }
        if !matches!(error.raw_os_error(), Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY)
        {
            return Err(classify_connect_error(error));
        }
        break;
    }

    wait_for_connection(&socket, deadline)?;
    Ok(UnixStream::from(socket))
}

struct UnixSocketAddress {
    address: libc::sockaddr_un,
    len: libc::socklen_t,
}

fn unix_socket_address(path: &Path) -> Result<UnixSocketAddress, DirectVhostUserConnectError> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: A zeroed sockaddr_un is a valid baseline before family and path
    // fields are initialized below.
    let mut address = unsafe { mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(DirectVhostUserConnectError::InvalidPath);
    }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *target = byte as libc::c_char;
    }
    let len = libc::socklen_t::try_from(sockaddr_un_path_offset() + bytes.len() + 1)
        .map_err(|_| DirectVhostUserConnectError::InvalidPath)?;
    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_vendor = "apple"
    ))]
    {
        address.sun_len =
            u8::try_from(len).map_err(|_| DirectVhostUserConnectError::InvalidPath)?;
    }
    Ok(UnixSocketAddress { address, len })
}

const fn sockaddr_un_path_offset() -> usize {
    sockaddr_un_len_prefix_size() + mem::size_of::<libc::sa_family_t>()
}

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_vendor = "apple"
))]
const fn sockaddr_un_len_prefix_size() -> usize {
    mem::size_of::<u8>()
}

#[cfg(not(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_vendor = "apple"
)))]
const fn sockaddr_un_len_prefix_size() -> usize {
    0
}

fn nonblocking_cloexec_socket() -> Result<OwnedFd, DirectVhostUserConnectError> {
    // SAFETY: `socket` has no pointer arguments. A successful descriptor is
    // adopted immediately so every later error closes it.
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return Err(classify_connect_error(io::Error::last_os_error()));
    }
    // SAFETY: `descriptor` is fresh and has no other Rust owner.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    set_fd_flag(&descriptor, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)?;
    set_fd_flag(&descriptor, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)?;
    Ok(descriptor)
}

fn set_fd_flag(
    descriptor: &OwnedFd,
    get_command: libc::c_int,
    set_command: libc::c_int,
    flag: libc::c_int,
) -> Result<(), DirectVhostUserConnectError> {
    // SAFETY: The descriptor is live and the get commands take no pointer.
    let existing = unsafe { libc::fcntl(descriptor.as_raw_fd(), get_command) };
    if existing < 0 {
        return Err(classify_connect_error(io::Error::last_os_error()));
    }
    // SAFETY: The descriptor is live and the integer preserves existing flags.
    if unsafe { libc::fcntl(descriptor.as_raw_fd(), set_command, existing | flag) } < 0 {
        return Err(classify_connect_error(io::Error::last_os_error()));
    }
    Ok(())
}

fn wait_for_connection(
    socket: &OwnedFd,
    deadline: Instant,
) -> Result<(), DirectVhostUserConnectError> {
    let mut poll_fd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DirectVhostUserConnectError::Timeout);
        }
        let timeout_ms = duration_to_poll_timeout(remaining);
        // SAFETY: `poll_fd` is one initialized entry and remains live for the
        // complete bounded call.
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result > 0 {
            break;
        }
        if result == 0 {
            return Err(DirectVhostUserConnectError::Timeout);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(classify_connect_error(error));
        }
    }

    let mut socket_error: libc::c_int = 0;
    let mut socket_error_len = libc::socklen_t::try_from(mem::size_of_val(&socket_error))
        .map_err(|_| DirectVhostUserConnectError::Io(io::ErrorKind::InvalidInput))?;
    // SAFETY: The output pointers describe one writable SO_ERROR integer for a
    // live socket descriptor.
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut socket_error).cast(),
            &raw mut socket_error_len,
        )
    } < 0
    {
        return Err(classify_connect_error(io::Error::last_os_error()));
    }
    if socket_error != 0 {
        return Err(classify_connect_error(io::Error::from_raw_os_error(
            socket_error,
        )));
    }
    Ok(())
}

fn duration_to_poll_timeout(duration: Duration) -> libc::c_int {
    let millis = duration
        .as_nanos()
        .saturating_add(999_999)
        .checked_div(1_000_000)
        .unwrap_or(u128::MAX)
        .min(i32::MAX as u128);
    i32::try_from(millis).unwrap_or(i32::MAX)
}

fn classify_connect_error(error: io::Error) -> DirectVhostUserConnectError {
    match error.kind() {
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => {
            DirectVhostUserConnectError::Refused
        }
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => DirectVhostUserConnectError::Timeout,
        kind => DirectVhostUserConnectError::Io(kind),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;

    static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

    struct TestSocketPath(PathBuf);

    impl TestSocketPath {
        fn new() -> Self {
            let id = NEXT_SOCKET_ID.fetch_add(1, Ordering::SeqCst);
            Self(std::env::temp_dir().join(format!(
                "bangbang-direct-vhost-{}-{id}.sock",
                std::process::id()
            )))
        }

        fn as_path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_file(&self.0)
                && error.kind() != io::ErrorKind::NotFound
            {
                panic!("test socket path should clean up: {error}");
            }
        }
    }

    #[test]
    fn rejects_empty_and_oversized_paths_without_echoing_them() {
        let error = connect(Path::new(OsStr::new("")), Duration::from_millis(1))
            .expect_err("empty path should fail");
        assert_eq!(error, DirectVhostUserConnectError::InvalidPath);
        let private = "private".repeat(64);
        let error = connect(Path::new(&private), Duration::from_millis(1))
            .expect_err("oversized path should fail");
        assert_eq!(error, DirectVhostUserConnectError::InvalidPath);
        assert!(!error.to_string().contains(&private));
    }

    #[test]
    fn errors_are_value_redacted() {
        let error = DirectVhostUserConnectError::Io(io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "vhost-user socket connection failed");
        assert!(!error.to_string().contains("PermissionDenied"));
    }

    #[test]
    fn connects_with_nonblocking_close_on_exec_descriptor_and_can_retry_after_refusal() {
        let path = TestSocketPath::new();
        assert_eq!(
            connect(path.as_path(), Duration::from_millis(100))
                .expect_err("missing socket should refuse"),
            DirectVhostUserConnectError::Refused
        );
        let listener = UnixListener::bind(path.as_path()).expect("test listener should bind");
        let peer = thread::spawn(move || {
            listener
                .accept()
                .expect("test listener should accept direct connection")
        });

        let stream = connect(path.as_path(), Duration::from_secs(1))
            .expect("retry should connect to live listener");
        // SAFETY: F_GETFD/F_GETFL inspect flags on the live owned stream fd.
        let descriptor_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
        // SAFETY: F_GETFL inspects flags on the same live stream fd.
        let status_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        assert!(descriptor_flags >= 0);
        assert!(status_flags >= 0);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
        assert_ne!(status_flags & libc::O_NONBLOCK, 0);
        drop(stream);
        let (accepted, _) = peer.join().expect("test peer should finish");
        drop(accepted);
    }

    #[test]
    fn zero_timeout_fails_before_opening_a_socket() {
        let path = TestSocketPath::new();

        assert_eq!(
            connect(path.as_path(), Duration::ZERO)
                .expect_err("zero timeout should fail immediately"),
            DirectVhostUserConnectError::Timeout
        );
        assert!(!path.as_path().exists());
    }
}
