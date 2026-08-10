//! Launcher-side connector for exact granted vhost-user socket children.

use std::ffi::{CStr, CString};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::time::{Duration, Instant};

use bangbang_session::macos::vhost_user_broker::{
    VhostUserBrokerError, VhostUserBrokerMessage, receive_vhost_user_broker_message,
    send_vhost_user_broker_message,
};
use bangbang_session::macos::{set_cloexec, verify_peer_pid};
use bangbang_session::{LauncherState, ObjectIdentity, SessionId};

use crate::LauncherError;
use crate::grant_manifest::{PreparedGrantBatch, SocketDirectoryAnchor};

use super::scoped_cwd::{ScopedCwdOperationError, with_scoped_cwd};
#[cfg(test)]
use super::scoped_cwd::{current_directory_identity, directory_descriptor_identity};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const CONNECT_INTERRUPTED_RETRY_LIMIT: usize = 8;

/// Session-bound serial connector for worker vhost-user requests.
pub(crate) struct LauncherVhostUserBroker {
    session: SessionId,
    next_sequence: u64,
}

impl std::fmt::Debug for LauncherVhostUserBroker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LauncherVhostUserBroker")
            .field("session", &"<redacted>")
            .field("next_sequence", &"<redacted>")
            .finish()
    }
}

impl LauncherVhostUserBroker {
    pub(crate) const fn new(session: SessionId) -> Self {
        Self {
            session,
            next_sequence: 1,
        }
    }

    pub(crate) fn drain(
        &mut self,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        lifecycle_state: LauncherState,
        lifecycle_cancelled: bool,
        grants: &PreparedGrantBatch,
    ) -> Result<(), LauncherError> {
        loop {
            let received = match receive_vhost_user_broker_message(socket) {
                Ok(received) => received,
                Err(VhostUserBrokerError::Io(io::ErrorKind::WouldBlock)) => return Ok(()),
                Err(_) => return Err(LauncherError::VhostUserBroker),
            };
            verify_peer_pid(socket.as_raw_fd(), worker_pid)
                .map_err(|_| LauncherError::VhostUserBroker)?;
            if received.descriptor.is_some()
                || received.message.session() != self.session
                || received.message.sequence() != self.next_sequence
                || lifecycle_cancelled
                || !matches!(
                    lifecycle_state,
                    LauncherState::Starting | LauncherState::Ready(_)
                )
            {
                return Err(LauncherError::VhostUserBroker);
            }
            let sequence = self.next_sequence;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .ok_or(LauncherError::VhostUserBroker)?;

            let VhostUserBrokerMessage::Connect {
                grant_id, child, ..
            } = received.message
            else {
                return Err(LauncherError::VhostUserBroker);
            };
            let anchor = grants
                .vhost_user_directory_anchor(&grant_id)
                .ok_or(LauncherError::VhostUserBroker)?;
            let result = connect_scoped(anchor, &child);
            match result {
                Ok(stream) => send(
                    socket,
                    &VhostUserBrokerMessage::Connected {
                        session: self.session,
                        sequence,
                        grant_id,
                        child,
                    },
                    Some(stream.as_raw_fd()),
                )?,
                Err(ScopedConnectError::Failure(kind)) => send(
                    socket,
                    &VhostUserBrokerMessage::Failed {
                        session: self.session,
                        sequence,
                        grant_id,
                        child,
                        kind,
                    },
                    None,
                )?,
                Err(ScopedConnectError::Rejected) => send(
                    socket,
                    &VhostUserBrokerMessage::Failed {
                        session: self.session,
                        sequence,
                        grant_id,
                        child,
                        kind: io::ErrorKind::Other,
                    },
                    None,
                )?,
                Err(ScopedConnectError::Invalid) => {
                    return Err(LauncherError::VhostUserBroker);
                }
            }
        }
    }
}

fn send(
    socket: &UnixDatagram,
    message: &VhostUserBrokerMessage,
    descriptor: Option<RawFd>,
) -> Result<(), LauncherError> {
    send_vhost_user_broker_message(socket, message, descriptor)
        .map_err(|_| LauncherError::VhostUserBroker)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedConnectError {
    Failure(io::ErrorKind),
    Rejected,
    Invalid,
}

fn map_scoped_cwd_error(error: ScopedCwdOperationError<ScopedConnectError>) -> ScopedConnectError {
    match error {
        ScopedCwdOperationError::Boundary(_) => ScopedConnectError::Invalid,
        ScopedCwdOperationError::Operation(error) => error,
    }
}

/// Result of beginning one descriptor-relative nonblocking connection.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) enum NonblockingAnchoredConnect {
    Connected(UnixStream, ObjectIdentity),
    Pending(PendingAnchoredConnect),
}

/// In-progress descriptor-relative connection with its exact source identity.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) struct PendingAnchoredConnect {
    stream: UnixStream,
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: CString,
    source_identity: ObjectIdentity,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl std::fmt::Debug for PendingAnchoredConnect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PendingAnchoredConnect(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl PendingAnchoredConnect {
    pub(crate) fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    pub(crate) const fn source_identity(&self) -> ObjectIdentity {
        self.source_identity
    }
}

/// Starts one exact nonblocking connection without polling the supervising thread.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn begin_anchored_exact_nonblocking(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
) -> Result<NonblockingAnchoredConnect, ScopedConnectError> {
    if name.to_bytes().is_empty() || name.to_bytes().contains(&b'/') || expected_mode & !0o777 != 0
    {
        return Err(ScopedConnectError::Invalid);
    }
    with_scoped_cwd(anchor_descriptor, anchor_identity, || {
        let source_identity = relative_connect_target_identity_with_mode(
            name,
            expected_uid,
            expected_gid,
            expected_mode,
        )?;
        let (address, address_length) = relative_unix_socket_address(name)?;
        // SAFETY: A successful descriptor is immediately wrapped for unique ownership.
        let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if descriptor < 0 {
            return Err(ScopedConnectError::Failure(
                io::Error::last_os_error().kind(),
            ));
        }
        // SAFETY: The fresh descriptor has no other owner.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        set_cloexec(descriptor.as_raw_fd())
            .map_err(|error| ScopedConnectError::Failure(error.kind()))?;
        set_nonblocking(descriptor.as_raw_fd())?;
        let mut interrupted = 0_usize;
        loop {
            // SAFETY: Descriptor and fully initialized local address remain live.
            let result = unsafe {
                libc::connect(
                    descriptor.as_raw_fd(),
                    (&raw const address).cast(),
                    address_length,
                )
            };
            if result == 0 {
                let after = relative_connect_target_identity_with_mode(
                    name,
                    expected_uid,
                    expected_gid,
                    expected_mode,
                )?;
                if after != source_identity {
                    return Err(ScopedConnectError::Rejected);
                }
                validate_connected_peer(descriptor.as_raw_fd(), name)?;
                return Ok(NonblockingAnchoredConnect::Connected(
                    UnixStream::from(descriptor),
                    source_identity,
                ));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted
                && interrupted < CONNECT_INTERRUPTED_RETRY_LIMIT
            {
                interrupted += 1;
                continue;
            }
            if matches!(error.raw_os_error(), Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY)
            {
                return Ok(NonblockingAnchoredConnect::Pending(
                    PendingAnchoredConnect {
                        stream: UnixStream::from(descriptor),
                        anchor_descriptor,
                        anchor_identity,
                        name: name.to_owned(),
                        source_identity,
                        expected_uid,
                        expected_gid,
                        expected_mode,
                    },
                ));
            }
            return Err(ScopedConnectError::Failure(error.kind()));
        }
    })
    .map_err(map_scoped_cwd_error)
}

/// Completes a writable nonblocking connection and repeats exact pathname validation.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn finish_anchored_exact_nonblocking(
    pending: PendingAnchoredConnect,
) -> Result<NonblockingAnchoredConnect, ScopedConnectError> {
    let socket_error = socket_int_option(pending.stream.as_raw_fd(), libc::SO_ERROR)?;
    if matches!(socket_error, libc::EINPROGRESS | libc::EALREADY) {
        return Ok(NonblockingAnchoredConnect::Pending(pending));
    }
    if socket_error != 0 {
        return Err(ScopedConnectError::Failure(
            io::Error::from_raw_os_error(socket_error).kind(),
        ));
    }
    with_scoped_cwd(pending.anchor_descriptor, pending.anchor_identity, || {
        let after = relative_connect_target_identity_with_mode(
            &pending.name,
            pending.expected_uid,
            pending.expected_gid,
            pending.expected_mode,
        )?;
        if after != pending.source_identity {
            return Err(ScopedConnectError::Rejected);
        }
        validate_connected_peer(pending.stream.as_raw_fd(), &pending.name)?;
        Ok(NonblockingAnchoredConnect::Connected(
            pending.stream,
            pending.source_identity,
        ))
    })
    .map_err(map_scoped_cwd_error)
}

/// Revalidates one exact anchored socket child without opening a connection.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn validate_anchored_socket_child(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
    expected_identity: ObjectIdentity,
) -> Result<(), ScopedConnectError> {
    with_scoped_cwd(anchor_descriptor, anchor_identity, || {
        relative_connect_target_identity_with_mode(name, expected_uid, expected_gid, expected_mode)
            .and_then(|identity| {
                if identity == expected_identity {
                    Ok(())
                } else {
                    Err(ScopedConnectError::Rejected)
                }
            })
    })
    .map_err(map_scoped_cwd_error)
}

/// Returns whether one fixed child is absent beneath an exact retained anchor.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn anchored_socket_child_is_absent(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
) -> Result<bool, ScopedConnectError> {
    with_scoped_cwd(anchor_descriptor, anchor_identity, || {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: The bounded fixed child and writable stat storage remain valid.
        let result = unsafe {
            libc::fstatat(
                libc::AT_FDCWD,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            Ok(false)
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                Ok(true)
            } else {
                Err(ScopedConnectError::Failure(error.kind()))
            }
        }
    })
    .map_err(map_scoped_cwd_error)
}

fn connect_scoped(
    anchor: SocketDirectoryAnchor,
    child: &bangbang_session::SocketChild,
) -> Result<UnixStream, ScopedConnectError> {
    let name = CString::new(child.as_bytes()).map_err(|_| ScopedConnectError::Invalid)?;
    connect_anchored_exact(
        anchor.descriptor(),
        anchor.identity(),
        &name,
        CONNECT_TIMEOUT,
    )
    .map(|(stream, _)| stream)
}

pub(crate) fn connect_anchored_exact(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
    timeout: Duration,
) -> Result<(UnixStream, ObjectIdentity), ScopedConnectError> {
    if timeout.is_zero() || name.to_bytes().is_empty() || name.to_bytes().contains(&b'/') {
        return Err(ScopedConnectError::Invalid);
    }
    with_scoped_cwd(anchor_descriptor, anchor_identity, || {
        connect_relative_child(name, timeout)
    })
    .map_err(map_scoped_cwd_error)
}

fn connect_relative_child(
    name: &CStr,
    timeout: Duration,
) -> Result<(UnixStream, ObjectIdentity), ScopedConnectError> {
    let before = relative_connect_target_identity(name)?;
    let (address, address_length) = relative_unix_socket_address(name)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ScopedConnectError::Failure(io::ErrorKind::TimedOut))?;

    // SAFETY: A successful descriptor is immediately wrapped for unique ownership.
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return Err(ScopedConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: The fresh descriptor has no other owner.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    set_cloexec(descriptor.as_raw_fd())
        .map_err(|error| ScopedConnectError::Failure(error.kind()))?;
    set_nonblocking(descriptor.as_raw_fd())?;

    let mut interrupted = 0_usize;
    loop {
        // SAFETY: The descriptor and complete local address remain live.
        let result = unsafe {
            libc::connect(
                descriptor.as_raw_fd(),
                (&raw const address).cast(),
                address_length,
            )
        };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted
            && interrupted < CONNECT_INTERRUPTED_RETRY_LIMIT
            && Instant::now() < deadline
        {
            interrupted += 1;
            continue;
        }
        if matches!(error.raw_os_error(), Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY)
        {
            finish_nonblocking_connect(descriptor.as_raw_fd(), deadline)?;
            break;
        }
        return Err(ScopedConnectError::Failure(error.kind()));
    }

    let after = relative_connect_target_identity(name)?;
    if after != before {
        return Err(ScopedConnectError::Rejected);
    }
    validate_connected_peer(descriptor.as_raw_fd(), name)?;
    Ok((UnixStream::from(descriptor), before))
}

fn relative_connect_target_identity(name: &CStr) -> Result<ObjectIdentity, ScopedConnectError> {
    relative_connect_target_identity_inner(name, None)
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn relative_connect_target_identity_with_mode(
    name: &CStr,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
) -> Result<ObjectIdentity, ScopedConnectError> {
    relative_connect_target_identity_inner(name, Some((expected_uid, expected_gid, expected_mode)))
}

fn relative_connect_target_identity_inner(
    name: &CStr,
    expected_owner_and_mode: Option<(u32, u32, u32)>,
) -> Result<ObjectIdentity, ScopedConnectError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The cwd anchor and bounded name are valid; output is writable.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ScopedConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful fstatat initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: geteuid has no pointer or ownership contract.
    let default_uid = unsafe { libc::geteuid() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_nlink != 1
        || stat.st_uid
            != expected_owner_and_mode.map_or(default_uid, |(expected_uid, _, _)| expected_uid)
        || expected_owner_and_mode.is_some_and(|(_, expected_gid, expected_mode)| {
            stat.st_gid != expected_gid || u32::from(stat.st_mode & 0o777) != expected_mode
        })
    {
        return Err(ScopedConnectError::Rejected);
    }
    Ok(stat_identity(&stat))
}

fn relative_unix_socket_address(
    name: &CStr,
) -> Result<(libc::sockaddr_un, libc::socklen_t), ScopedConnectError> {
    let bytes = name.to_bytes_with_nul();
    let address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    // SAFETY: Zeroed sockaddr_un is valid before initialization below.
    let mut address = unsafe { address.assume_init() };
    if bytes.len() > address.sun_path.len() {
        return Err(ScopedConnectError::Invalid);
    }
    address.sun_family =
        libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| ScopedConnectError::Invalid)?;
    address.sun_len = u8::try_from(
        std::mem::offset_of!(libc::sockaddr_un, sun_path)
            .checked_add(bytes.len())
            .ok_or(ScopedConnectError::Invalid)?,
    )
    .map_err(|_| ScopedConnectError::Invalid)?;
    // SAFETY: The bounded source including NUL fits the destination array.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    Ok((address, libc::socklen_t::from(address.sun_len)))
}

fn set_nonblocking(descriptor: RawFd) -> Result<(), ScopedConnectError> {
    // SAFETY: F_GETFL inspects one live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(ScopedConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: F_SETFL changes status flags on the same live descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ScopedConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    Ok(())
}

fn finish_nonblocking_connect(
    descriptor: RawFd,
    deadline: Instant,
) -> Result<(), ScopedConnectError> {
    let mut poll_fd = libc::pollfd {
        fd: descriptor,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ScopedConnectError::Failure(io::ErrorKind::TimedOut));
        }
        // SAFETY: One initialized poll entry remains writable for the bounded call.
        let result =
            unsafe { libc::poll(&raw mut poll_fd, 1, duration_to_poll_timeout(remaining)) };
        if result > 0 {
            break;
        }
        if result == 0 {
            return Err(ScopedConnectError::Failure(io::ErrorKind::TimedOut));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(ScopedConnectError::Failure(error.kind()));
        }
    }
    let socket_error = socket_int_option(descriptor, libc::SO_ERROR)?;
    if socket_error == 0 {
        Ok(())
    } else {
        Err(ScopedConnectError::Failure(
            io::Error::from_raw_os_error(socket_error).kind(),
        ))
    }
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

fn validate_connected_peer(descriptor: RawFd, expected: &CStr) -> Result<(), ScopedConnectError> {
    if socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_int_option(descriptor, libc::SO_ERROR)? != 0
    {
        return Err(ScopedConnectError::Rejected);
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>())
        .map_err(|_| ScopedConnectError::Invalid)?;
    // SAFETY: Address storage and length are writable for the connected socket.
    if unsafe { libc::getpeername(descriptor, address.as_mut_ptr().cast(), &raw mut length) } != 0 {
        return Err(ScopedConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful getpeername initialized the returned prefix.
    let address = unsafe { address.assume_init() };
    if address.sun_family
        != libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| ScopedConnectError::Invalid)?
    {
        return Err(ScopedConnectError::Rejected);
    }
    let returned = usize::try_from(length).map_err(|_| ScopedConnectError::Invalid)?;
    let path_length = returned
        .checked_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
        .ok_or(ScopedConnectError::Rejected)?;
    if path_length > address.sun_path.len() {
        return Err(ScopedConnectError::Rejected);
    }
    // SAFETY: The kernel-returned length bounds this read within address.
    let path =
        unsafe { std::slice::from_raw_parts(address.sun_path.as_ptr().cast::<u8>(), path_length) };
    let expected = expected.to_bytes_with_nul();
    if path == expected
        || path
            .strip_suffix(expected)
            .is_some_and(|prefix| prefix.first() == Some(&b'/') && prefix.last() == Some(&b'/'))
    {
        Ok(())
    } else {
        Err(ScopedConnectError::Rejected)
    }
}

fn socket_int_option(descriptor: RawFd, option: libc::c_int) -> Result<i32, ScopedConnectError> {
    let mut value = 0_i32;
    let mut length =
        libc::socklen_t::try_from(size_of::<i32>()).map_err(|_| ScopedConnectError::Invalid)?;
    // SAFETY: Option output and length are writable for this live socket.
    if unsafe {
        libc::getsockopt(
            descriptor,
            libc::SOL_SOCKET,
            option,
            (&raw mut value).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(ScopedConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    if usize::try_from(length).ok() != Some(size_of::<i32>()) {
        return Err(ScopedConnectError::Rejected);
    }
    Ok(value)
}

fn stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::symlink;
    #[cfg(feature = "elevated-bootstrap-probe")]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const SCOPED_CONNECT_CHILD_ENV: &str = "BANGBANG_TEST_SCOPED_VHOST_CONNECT_CHILD";
    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = PathBuf::from("/tmp").join(format!(
                "bb-vub-{}-{}",
                std::process::id(),
                NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("test directory should create");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn broker_state_and_error_are_redacted() {
        let mut broker = LauncherVhostUserBroker::new(SessionId::from_bytes([9; 32]));
        broker.next_sequence = 81;
        let debug = format!("{broker:?}");
        assert!(!debug.contains("0909"));
        assert!(!debug.contains("81"));
        assert_eq!(
            LauncherError::VhostUserBroker.to_string(),
            "private vhost-user broker failed"
        );
    }

    #[test]
    fn poll_timeout_rounds_up_and_saturates() {
        assert_eq!(duration_to_poll_timeout(Duration::from_nanos(1)), 1);
        assert_eq!(duration_to_poll_timeout(Duration::from_millis(9)), 9);
        assert_eq!(duration_to_poll_timeout(Duration::MAX), i32::MAX);
    }

    fn broker_message(
        session: SessionId,
        sequence: u64,
        connected: bool,
    ) -> VhostUserBrokerMessage {
        let grant_id =
            bangbang_session::GrantId::parse("vhost-directory").expect("grant ID should parse");
        let child = bangbang_session::SocketChild::parse("backend.sock")
            .expect("socket child should parse");
        if connected {
            VhostUserBrokerMessage::Connected {
                session,
                sequence,
                grant_id,
                child,
            }
        } else {
            VhostUserBrokerMessage::Connect {
                session,
                sequence,
                grant_id,
                child,
            }
        }
    }

    fn assert_drain_rejects(
        message: VhostUserBrokerMessage,
        descriptor: Option<RawFd>,
        lifecycle_state: LauncherState,
        lifecycle_cancelled: bool,
    ) {
        let session = SessionId::from_bytes([11; 32]);
        let (worker, launcher) = UnixDatagram::pair().expect("broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher broker should be nonblocking");
        send_vhost_user_broker_message(&worker, &message, descriptor)
            .expect("closed test message should send");
        let mut broker = LauncherVhostUserBroker::new(session);
        // SAFETY: Both broker endpoints belong to the current test process.
        let worker_pid = unsafe { libc::getpid() };
        assert!(matches!(
            broker.drain(
                &launcher,
                worker_pid,
                lifecycle_state,
                lifecycle_cancelled,
                &PreparedGrantBatch::empty_for_test(),
            ),
            Err(LauncherError::VhostUserBroker)
        ));
    }

    #[test]
    fn drain_rejects_wrong_correlation_phase_cancellation_operation_and_rights() {
        let session = SessionId::from_bytes([11; 32]);
        assert_drain_rejects(
            broker_message(SessionId::from_bytes([12; 32]), 1, false),
            None,
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            broker_message(session, 2, false),
            None,
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            broker_message(session, 1, false),
            None,
            LauncherState::AwaitHello,
            false,
        );
        assert_drain_rejects(
            broker_message(session, 1, false),
            None,
            LauncherState::Starting,
            true,
        );
        let descriptor = File::open("/dev/null").expect("descriptor fixture should open");
        assert_drain_rejects(
            broker_message(session, 1, true),
            Some(descriptor.as_raw_fd()),
            LauncherState::Starting,
            false,
        );
        assert_drain_rejects(
            broker_message(session, 1, false),
            None,
            LauncherState::Starting,
            false,
        );
    }

    #[test]
    fn scoped_connect_uses_exact_anchor_and_restores_cwd() {
        if std::env::var_os(SCOPED_CONNECT_CHILD_ENV).is_none() {
            let status =
                Command::new(std::env::current_exe().expect("test executable should exist"))
                    .arg("scoped_connect_uses_exact_anchor_and_restores_cwd")
                    .arg("--nocapture")
                    .current_dir("/")
                    .env(SCOPED_CONNECT_CHILD_ENV, "1")
                    .status()
                    .expect("isolated cwd test should launch");
            assert!(status.success(), "isolated cwd test should pass: {status}");
            return;
        }

        let root = TestDir::new();
        let anchor_path = root.0.join("anchor");
        let unrelated_path = root.0.join("unrelated");
        fs::create_dir(&anchor_path).expect("anchor directory should create");
        fs::create_dir(&unrelated_path).expect("unrelated directory should create");
        let listener = UnixListener::bind(anchor_path.join("backend.sock"))
            .expect("anchored listener should bind");
        let anchor_file = File::open(&anchor_path).expect("anchor directory should open");
        let identity = directory_descriptor_identity(anchor_file.as_raw_fd())
            .expect("anchor identity should inspect");
        let anchor = SocketDirectoryAnchor::for_test(anchor_file.as_raw_fd(), identity);
        let child = bangbang_session::SocketChild::parse("backend.sock")
            .expect("socket child should parse");
        let original = std::env::current_dir().expect("original cwd should read");
        std::env::set_current_dir(&unrelated_path).expect("isolated cwd should change");
        let unrelated_identity =
            current_directory_identity().expect("unrelated cwd should inspect");

        let stream = connect_scoped(anchor, &child).expect("anchored socket should connect");
        let (_accepted, _) = listener.accept().expect("anchored listener should accept");
        assert_eq!(
            current_directory_identity().expect("restored cwd should inspect"),
            unrelated_identity
        );
        // SAFETY: F_GETFD and F_GETFL only inspect the live connected descriptor.
        let descriptor_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
        // SAFETY: See above.
        let status_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
        assert_ne!(status_flags & libc::O_NONBLOCK, 0);

        let missing = bangbang_session::SocketChild::parse("missing.sock")
            .expect("missing child should parse");
        assert_eq!(
            connect_scoped(anchor, &missing).expect_err("missing child should fail"),
            ScopedConnectError::Failure(io::ErrorKind::NotFound)
        );
        assert_eq!(
            current_directory_identity().expect("cwd after missing child should inspect"),
            unrelated_identity
        );

        let outside_listener =
            UnixListener::bind(root.0.join("outside.sock")).expect("outside listener should bind");
        symlink(root.0.join("outside.sock"), anchor_path.join("link.sock"))
            .expect("socket symlink fixture should create");
        let link =
            bangbang_session::SocketChild::parse("link.sock").expect("symlink child should parse");
        assert_eq!(
            connect_scoped(anchor, &link).expect_err("socket symlink should fail closed"),
            ScopedConnectError::Rejected
        );
        assert_eq!(
            current_directory_identity().expect("cwd after symlink should inspect"),
            unrelated_identity
        );
        drop(outside_listener);

        fs::write(anchor_path.join("regular.sock"), b"not a socket")
            .expect("regular target fixture should create");
        let regular = bangbang_session::SocketChild::parse("regular.sock")
            .expect("regular child should parse");
        assert_eq!(
            connect_scoped(anchor, &regular).expect_err("regular target should fail closed"),
            ScopedConnectError::Rejected
        );
        assert_eq!(
            current_directory_identity().expect("cwd after regular target should inspect"),
            unrelated_identity
        );

        #[cfg(feature = "elevated-bootstrap-probe")]
        {
            let api_path = anchor_path.join("api.sock");
            // SAFETY: These calls have no pointer or ownership contract.
            let expected_uid = unsafe { libc::geteuid() };
            // SAFETY: See above.
            let expected_gid = unsafe { libc::getegid() };
            // SAFETY: The live test anchor is owned by the current process; the
            // requested owner is its current effective identity.
            let chown_result =
                unsafe { libc::fchown(anchor_file.as_raw_fd(), expected_uid, expected_gid) };
            assert_eq!(chown_result, 0, "nonblocking anchor owner should set",);
            let api_listener =
                UnixListener::bind(&api_path).expect("nonblocking listener should bind");
            fs::set_permissions(&api_path, fs::Permissions::from_mode(0o600))
                .expect("nonblocking socket mode should set");
            let api_metadata = fs::symlink_metadata(&api_path)
                .expect("nonblocking socket metadata should inspect");
            assert_eq!(
                (
                    api_metadata.uid(),
                    api_metadata.gid(),
                    api_metadata.mode() & 0o777,
                    api_metadata.nlink(),
                ),
                (expected_uid, expected_gid, 0o600, 1),
                "nonblocking socket fixture should have exact target metadata",
            );
            assert!(
                !anchored_socket_child_is_absent(anchor_file.as_raw_fd(), identity, c"api.sock",)
                    .expect("present anchored child should inspect")
            );
            assert!(
                anchored_socket_child_is_absent(anchor_file.as_raw_fd(), identity, c"absent.sock",)
                    .expect("absent anchored child should inspect")
            );

            let mut connection = begin_anchored_exact_nonblocking(
                anchor_file.as_raw_fd(),
                identity,
                c"api.sock",
                expected_uid,
                expected_gid,
                0o600,
            )
            .expect("nonblocking anchored connection should begin");
            let (stream, source_identity) = loop {
                match connection {
                    NonblockingAnchoredConnect::Connected(stream, source_identity) => {
                        break (stream, source_identity);
                    }
                    NonblockingAnchoredConnect::Pending(pending) => {
                        let mut poll_fd = libc::pollfd {
                            fd: pending.as_raw_fd(),
                            events: libc::POLLOUT,
                            revents: 0,
                        };
                        // SAFETY: One initialized poll entry remains writable for the call.
                        assert!(unsafe { libc::poll(&raw mut poll_fd, 1, 1_000) } > 0);
                        connection = finish_anchored_exact_nonblocking(pending)
                            .expect("writable anchored connection should finish");
                    }
                }
            };
            let (_accepted, _) = api_listener
                .accept()
                .expect("nonblocking connection should be accepted");
            validate_anchored_socket_child(
                anchor_file.as_raw_fd(),
                identity,
                c"api.sock",
                expected_uid,
                expected_gid,
                0o600,
                source_identity,
            )
            .expect("unchanged anchored child should revalidate");
            assert!(matches!(
                begin_anchored_exact_nonblocking(
                    anchor_file.as_raw_fd(),
                    identity,
                    c"api.sock",
                    expected_uid.wrapping_add(1),
                    expected_gid,
                    0o600,
                ),
                Err(ScopedConnectError::Rejected)
            ));
            assert_eq!(
                validate_anchored_socket_child(
                    anchor_file.as_raw_fd(),
                    identity,
                    c"api.sock",
                    expected_uid,
                    expected_gid,
                    0o601,
                    source_identity,
                )
                .expect_err("wrong target mode should fail closed"),
                ScopedConnectError::Rejected
            );
            drop(stream);
            assert_eq!(
                current_directory_identity()
                    .expect("cwd after nonblocking connections should inspect"),
                unrelated_identity
            );
        }

        std::env::set_current_dir(&original).expect("original cwd should restore for cleanup");
    }
}
