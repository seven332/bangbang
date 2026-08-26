//! Contained macOS Unix-socket binding relative to verified directory anchors.

use std::env;
use std::ffi::{CString, OsStr, c_char};
use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

use bangbang_runtime::vsock::VsockGuestConnector;
#[cfg(feature = "elevated-bootstrap-probe")]
use bangbang_session::macos::api_listener::ReceivedApiListener;
use bangbang_session::macos::runtime::{
    SocketOwnershipRecord, WorkerSocketNamespace, socket_staging_name,
};
use bangbang_session::macos::socket_broker::{
    SocketBrokerMessage, receive_socket_broker_message, send_socket_broker_message,
};
use bangbang_session::macos::{set_cloexec, verify_peer, verify_peer_pid};
use bangbang_session::{ObjectIdentity, ResourceRole, SocketChild};

use crate::contained_session::{
    ClaimedSocketDirectory, PreparedSocketBrokerEndpoint, PreparedSocketDirectoryClaim,
    SocketBrokerEndpoint,
};

const BINDER_ARGUMENT: &str = "--bangbang-internal-socket-binder-v1";
const BINDER_FD: RawFd = 5;
const HELPER_DIRECTORY_FD: RawFd = 6;
const MIN_PARENT_FD: RawFd = 10;
const BINDER_TIMEOUT: Duration = Duration::from_secs(5);
const BINDER_COMMAND_BYTES: usize = 24;
const BINDER_COMMAND_MAGIC: [u8; 4] = *b"BBI1";
const BINDER_COMMAND_VERSION: u8 = 1;
const RESPONSE_BYTES: usize = 24;
const RESPONSE_MAGIC: [u8; 4] = *b"BBB1";
const RESPONSE_VERSION: u8 = 1;
const BINDER_ACK_BYTES: usize = 24;
const BINDER_ACK_MAGIC: [u8; 4] = *b"BBA1";
const BINDER_ACK_VERSION: u8 = 1;
const HELLO_BYTES: usize = 8;
const HELLO_MAGIC: [u8; 4] = *b"BBH1";
const HELLO_VERSION: u8 = 1;
const HELLO_BINDER: u8 = 1;
const CMSG_ALIGNMENT: usize = size_of::<u32>();
const CONTROL_WORDS: usize = 16;
const RENAME_EXCL: libc::c_uint = 0x0000_0004;

unsafe extern "C" {
    fn renameatx_np(
        from_fd: libc::c_int,
        from: *const c_char,
        to_fd: libc::c_int,
        to: *const c_char,
        flags: libc::c_uint,
    ) -> libc::c_int;
}

/// Value-redacted contained socket construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchoredSocketError {
    Binder,
    Broker,
    Cancelled,
    Cleanup,
    CrossFilesystem,
    Invalid,
    Io(io::ErrorKind),
    PathChanged,
    PathExists,
}

impl fmt::Display for AnchoredSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private anchored socket operation failed")
    }
}

impl std::error::Error for AnchoredSocketError {}

/// Listener and owner-thread cleanup authority produced by one publication.
pub(crate) struct BoundAnchoredSocket {
    listener: UnixListener,
    guard: AnchoredSocketGuard,
    connector: Option<AnchoredVsockConnector>,
}

impl fmt::Debug for BoundAnchoredSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundAnchoredSocket")
            .field("listener", &"<owned>")
            .field("guard", &self.guard)
            .field("connector", &self.connector.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

impl BoundAnchoredSocket {
    pub(crate) fn into_parts(self) -> (UnixListener, AnchoredSocketGuard) {
        let Self {
            listener,
            guard,
            connector,
        } = self;
        debug_assert!(connector.is_none());
        drop(connector);
        (listener, guard)
    }

    pub(crate) fn into_vsock_parts(
        self,
    ) -> Result<(UnixListener, AnchoredSocketGuard, AnchoredVsockConnector), AnchoredSocketError>
    {
        let Self {
            listener,
            guard,
            connector,
        } = self;
        Ok((
            listener,
            guard,
            connector.ok_or(AnchoredSocketError::Invalid)?,
        ))
    }
}

/// Descriptor-only client for the authenticated launcher vsock broker.
pub(crate) struct AnchoredVsockConnector {
    socket: UnixDatagram,
    session: bangbang_session::SessionId,
    launcher_pid: libc::pid_t,
    next_sequence: u64,
    healthy: bool,
}

impl fmt::Debug for AnchoredVsockConnector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnchoredVsockConnector")
            .field("socket", &"<owned>")
            .field("session", &"<redacted>")
            .field("launcher_pid", &"<redacted>")
            .field("sequence", &"<redacted>")
            .field("healthy", &self.healthy)
            .finish()
    }
}

impl AnchoredVsockConnector {
    fn exchange(&mut self, shutdown: bool, port: u32) -> io::Result<Option<UnixStream>> {
        if !self.healthy {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let request = if shutdown {
            SocketBrokerMessage::Shutdown {
                session: self.session,
                sequence,
            }
        } else {
            SocketBrokerMessage::Connect {
                session: self.session,
                sequence,
                port,
            }
        };
        if send_socket_broker_message(&self.socket, &request, None).is_err()
            || verify_peer_pid(self.socket.as_raw_fd(), self.launcher_pid).is_err()
        {
            self.healthy = false;
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let response = receive_socket_broker_message(&self.socket);
        if verify_peer_pid(self.socket.as_raw_fd(), self.launcher_pid).is_err() {
            self.healthy = false;
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        match response.map(|response| (response.message, response.descriptor)) {
            Ok((
                SocketBrokerMessage::Connected {
                    session,
                    sequence: response_sequence,
                    port: response_port,
                },
                Some(descriptor),
            )) if !shutdown
                && session == self.session
                && response_sequence == sequence
                && response_port == port =>
            {
                validate_connected_stream_descriptor(descriptor.as_raw_fd()).map_err(|_| {
                    self.healthy = false;
                    io::Error::from(io::ErrorKind::InvalidData)
                })?;
                Ok(Some(UnixStream::from(descriptor)))
            }
            Ok((
                SocketBrokerMessage::Failed {
                    session,
                    sequence: response_sequence,
                    port: response_port,
                    kind,
                },
                None,
            )) if !shutdown
                && session == self.session
                && response_sequence == sequence
                && response_port == port =>
            {
                Err(io::Error::from(kind))
            }
            Ok((
                SocketBrokerMessage::Complete {
                    session,
                    sequence: response_sequence,
                },
                None,
            )) if shutdown && session == self.session && response_sequence == sequence => Ok(None),
            Err(_) => {
                self.healthy = false;
                Err(io::Error::from(io::ErrorKind::InvalidData))
            }
            Ok(_) => {
                self.healthy = false;
                Err(io::Error::from(io::ErrorKind::InvalidData))
            }
        }
    }
}

impl VsockGuestConnector for AnchoredVsockConnector {
    fn connect(&mut self, host_port: u32) -> io::Result<UnixStream> {
        self.exchange(false, host_port)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
    }
}

impl Drop for AnchoredVsockConnector {
    fn drop(&mut self) {
        let _ = self.healthy && self.exchange(true, 0).is_ok_and(|stream| stream.is_none());
    }
}

/// Exact lifetime authority for one externally published socket.
pub(crate) struct AnchoredSocketGuard {
    #[cfg(feature = "elevated-bootstrap-probe")]
    ownership: AnchoredSocketOwnership,
    #[cfg(not(feature = "elevated-bootstrap-probe"))]
    namespace: WorkerSocketNamespace,
    claim: ClaimedSocketDirectory,
    record: SocketOwnershipRecord,
    expected_owner: Option<(u32, u32)>,
}

#[cfg(feature = "elevated-bootstrap-probe")]
enum AnchoredSocketOwnership {
    Recorded(WorkerSocketNamespace),
    Transferred,
}

#[cfg(not(feature = "elevated-bootstrap-probe"))]
impl fmt::Debug for AnchoredSocketGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnchoredSocketGuard")
            .field("namespace", &self.namespace)
            .field("claim", &"<owned>")
            .field("record", &self.record)
            .finish()
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl fmt::Debug for AnchoredSocketGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnchoredSocketGuard")
            .field(
                "ownership",
                &match &self.ownership {
                    AnchoredSocketOwnership::Recorded(_) => "recorded",
                    AnchoredSocketOwnership::Transferred => "transferred",
                },
            )
            .field("claim", &"<owned>")
            .field("record", &self.record)
            .finish()
    }
}

#[cfg(not(feature = "elevated-bootstrap-probe"))]
impl Drop for AnchoredSocketGuard {
    fn drop(&mut self) {
        cleanup_published_record(
            &self.namespace,
            &self.claim,
            &self.record,
            self.expected_owner,
        );
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl Drop for AnchoredSocketGuard {
    fn drop(&mut self) {
        match &self.ownership {
            AnchoredSocketOwnership::Recorded(namespace) => {
                cleanup_published_record(namespace, &self.claim, &self.record, self.expected_owner)
            }
            AnchoredSocketOwnership::Transferred => {
                let _ = unlink_socket_if_owned_with_owner(
                    self.claim.directory.anchor_fd(),
                    &self.claim.child,
                    self.record.identity(),
                    self.expected_owner,
                );
            }
        }
    }
}

/// Returns whether this process was invoked through the private binder contract.
pub(crate) fn is_binder_invocation() -> bool {
    is_helper_invocation(BINDER_ARGUMENT)
}

fn is_helper_invocation(argument: &str) -> bool {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new(argument)) || arguments.next().is_some() {
        return false;
    }
    // SAFETY: `F_GETFD` only inspects the two fixed integer descriptors.
    (unsafe { libc::fcntl(BINDER_FD, libc::F_GETFD) }) >= 0
        && (unsafe { libc::fcntl(HELPER_DIRECTORY_FD, libc::F_GETFD) }) >= 0
}

/// Runs the closed helper protocol and returns only a process success bit.
pub(crate) fn run_binder() -> bool {
    run_binder_inner().is_ok()
}

fn run_binder_inner() -> Result<(), AnchoredSocketError> {
    set_cloexec(BINDER_FD).map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    // SAFETY: The private spawn contract transfers fd 5 exactly once here.
    let mut socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(BINDER_FD) });
    socket
        .set_read_timeout(Some(BINDER_TIMEOUT))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    socket
        .set_write_timeout(Some(BINDER_TIMEOUT))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    // SAFETY: `getppid` has no pointer or ownership contract.
    let parent = unsafe { libc::getppid() };
    verify_peer(socket.as_raw_fd(), parent).map_err(|_| AnchoredSocketError::Invalid)?;
    send_exact_frame(&socket, &helper_hello(HELLO_BINDER)?)?;

    let mut command = [0_u8; BINDER_COMMAND_BYTES];
    socket
        .read_exact(&mut command)
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    let (role, expected_directory) = parse_binder_command(&command)?;
    verify_peer(socket.as_raw_fd(), parent).map_err(|_| AnchoredSocketError::Invalid)?;
    enter_helper_directory(expected_directory)?;
    let staging = socket_staging_name(role).map_err(|_| AnchoredSocketError::Invalid)?;
    ensure_relative_absent(libc::AT_FDCWD, staging)?;
    let listener = UnixListener::bind(Path::new(OsStr::from_bytes(staging.to_bytes())))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    // SAFETY: `staging` is a fixed live relative C string naming the socket just bound.
    if unsafe { libc::chmod(staging.as_ptr(), 0o600) } != 0 {
        let error = AnchoredSocketError::Io(io::Error::last_os_error().kind());
        let _ = unlink_relative_if_socket(staging, None);
        return Err(error);
    }
    let identity = match relative_socket_identity(libc::AT_FDCWD, staging) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = unlink_relative_if_socket(staging, None);
            return Err(error);
        }
    };
    if let Err(error) = send_listener(&socket, role, identity, listener.as_raw_fd()) {
        let _ = unlink_relative_if_socket(staging, Some(identity));
        return Err(error);
    }
    let mut acknowledgment = [0_u8; BINDER_ACK_BYTES];
    if socket.read_exact(&mut acknowledgment).is_err()
        || parse_binder_ack(&acknowledgment) != Ok((role, identity))
        || verify_peer(socket.as_raw_fd(), parent).is_err()
    {
        let _ = unlink_relative_if_socket(staging, Some(identity));
        return Err(AnchoredSocketError::Binder);
    }
    Ok(())
}

/// Binds and exclusively publishes one live socket through verified anchors.
pub(crate) fn bind(
    namespace: WorkerSocketNamespace,
    claim: ClaimedSocketDirectory,
    role: ResourceRole,
    broker: Option<SocketBrokerEndpoint>,
) -> Result<BoundAnchoredSocket, AnchoredSocketError> {
    bind_inner(
        namespace,
        SocketDirectoryClaim::Committed(claim),
        role,
        broker.map(SocketBrokerClaim::Committed),
        || false,
    )
}

/// Adopts the fixed final API listener created by the elevated launcher.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn adopt_elevated_api_listener(
    claim: ClaimedSocketDirectory,
    received: ReceivedApiListener,
) -> Result<BoundAnchoredSocket, AnchoredSocketError> {
    use bangbang_session::elevated_probe::{GUEST_API_DIRECTORY_GRANT_ID, GUEST_API_SOCKET_CHILD};

    let expected_grant = bangbang_session::GrantId::parse(GUEST_API_DIRECTORY_GRANT_ID)
        .map_err(|_| AnchoredSocketError::Invalid)?;
    let expected_child =
        SocketChild::parse(GUEST_API_SOCKET_CHILD).map_err(|_| AnchoredSocketError::Invalid)?;
    if claim.grant_id != expected_grant || claim.child != expected_child {
        return Err(AnchoredSocketError::Invalid);
    }
    // SAFETY: Effective identity calls have no pointer or ownership contract.
    let expected_owner = unsafe { (libc::geteuid(), libc::getegid()) };
    let path_identity = received
        .record
        .path_identity()
        .ok_or(AnchoredSocketError::Invalid)?;
    if path_identity.device != claim.directory.identity().device {
        return Err(AnchoredSocketError::Invalid);
    }
    let child = child_cstring(&claim.child)?;
    let current = relative_socket_identity_with_owner(
        claim.directory.anchor_fd(),
        &child,
        Some(expected_owner),
    )?;
    if current != path_identity {
        return Err(AnchoredSocketError::PathChanged);
    }
    let record = SocketOwnershipRecord::new(
        ResourceRole::ApiSocketDirectory,
        claim.child.clone(),
        path_identity,
    )
    .map_err(|_| AnchoredSocketError::Invalid)?;
    let listener = UnixListener::from(received.listener);
    let adopted = (|| {
        validate_elevated_listener_descriptor(listener.as_raw_fd(), &child)?;
        if relative_socket_identity_with_owner(
            claim.directory.anchor_fd(),
            &child,
            Some(expected_owner),
        )? != path_identity
        {
            return Err(AnchoredSocketError::PathChanged);
        }
        validate_elevated_listener_descriptor(listener.as_raw_fd(), &child)
    })();
    if let Err(error) = adopted {
        let cleanup = checked_cleanup(unlink_socket_if_owned_with_owner(
            claim.directory.anchor_fd(),
            &claim.child,
            path_identity,
            Some(expected_owner),
        ));
        if cleanup.is_err() {
            return Err(AnchoredSocketError::Cleanup);
        }
        return Err(error);
    }
    Ok(BoundAnchoredSocket {
        listener,
        guard: AnchoredSocketGuard {
            ownership: AnchoredSocketOwnership::Transferred,
            claim,
            record,
            expected_owner: Some(expected_owner),
        },
        connector: None,
    })
}

/// Binds a restore vsock while retaining reusable authority until activation.
pub(crate) fn bind_prepared_vsock(
    namespace: WorkerSocketNamespace,
    claim: PreparedSocketDirectoryClaim,
    broker: PreparedSocketBrokerEndpoint,
    cancelled: impl FnOnce() -> bool,
) -> Result<BoundAnchoredSocket, AnchoredSocketError> {
    bind_inner(
        namespace,
        SocketDirectoryClaim::Prepared(claim),
        ResourceRole::VsockSocketDirectory,
        Some(SocketBrokerClaim::Prepared(broker)),
        cancelled,
    )
}

enum SocketDirectoryClaim {
    Committed(ClaimedSocketDirectory),
    Prepared(PreparedSocketDirectoryClaim),
}

impl SocketDirectoryClaim {
    fn directory_anchor_fd(&self) -> Result<RawFd, AnchoredSocketError> {
        match self {
            Self::Committed(claim) => Ok(claim.directory.anchor_fd()),
            Self::Prepared(claim) => claim
                .directory()
                .map(|directory| directory.anchor_fd())
                .map_err(|_| AnchoredSocketError::Invalid),
        }
    }

    fn directory_device(&self) -> Result<u64, AnchoredSocketError> {
        match self {
            Self::Committed(claim) => Ok(claim.directory.identity().device),
            Self::Prepared(claim) => claim
                .directory()
                .map(|directory| directory.identity().device)
                .map_err(|_| AnchoredSocketError::Invalid),
        }
    }

    fn child(&self) -> &SocketChild {
        match self {
            Self::Committed(claim) => &claim.child,
            Self::Prepared(claim) => claim.child(),
        }
    }

    fn commit(self) -> ClaimedSocketDirectory {
        match self {
            Self::Committed(claim) => claim,
            Self::Prepared(claim) => claim.commit(),
        }
    }
}

enum SocketBrokerClaim {
    Committed(SocketBrokerEndpoint),
    Prepared(PreparedSocketBrokerEndpoint),
}

impl SocketBrokerClaim {
    fn endpoint(&self) -> Result<&SocketBrokerEndpoint, AnchoredSocketError> {
        match self {
            Self::Committed(endpoint) => Ok(endpoint),
            Self::Prepared(endpoint) => endpoint
                .endpoint()
                .map_err(|_| AnchoredSocketError::Invalid),
        }
    }

    fn commit(self) -> Result<SocketBrokerEndpoint, AnchoredSocketError> {
        match self {
            Self::Committed(endpoint) => Ok(endpoint),
            Self::Prepared(endpoint) => endpoint.commit().map_err(|_| AnchoredSocketError::Invalid),
        }
    }
}

fn bind_inner(
    namespace: WorkerSocketNamespace,
    claim: SocketDirectoryClaim,
    role: ResourceRole,
    broker: Option<SocketBrokerClaim>,
    cancelled: impl FnOnce() -> bool,
) -> Result<BoundAnchoredSocket, AnchoredSocketError> {
    if broker.is_some() != (role == ResourceRole::VsockSocketDirectory) {
        return Err(AnchoredSocketError::Invalid);
    }
    if namespace.identity().device != claim.directory_device()? {
        return Err(AnchoredSocketError::CrossFilesystem);
    }
    let directory_anchor = claim.directory_anchor_fd()?;
    let staging = socket_staging_name(role).map_err(|_| AnchoredSocketError::Invalid)?;
    ensure_relative_absent(namespace.anchor_fd(), staging)?;

    let (listener, identity) = match spawn_binder(&namespace, role) {
        Ok(prepared) => prepared,
        Err(error) => {
            cleanup_staged_socket_checked(&namespace, staging, None)?;
            return Err(error);
        }
    };
    let staged_identity = match relative_socket_identity(namespace.anchor_fd(), staging) {
        Ok(identity) => identity,
        Err(error) => {
            cleanup_staged_socket_checked(&namespace, staging, Some(identity))?;
            return Err(error);
        }
    };
    if staged_identity != identity {
        cleanup_staged_socket_checked(&namespace, staging, Some(identity))?;
        return Err(AnchoredSocketError::PathChanged);
    }

    let record = match SocketOwnershipRecord::new(role, claim.child().clone(), identity) {
        Ok(record) => record,
        Err(_) => {
            cleanup_staged_socket_checked(&namespace, staging, Some(identity))?;
            return Err(AnchoredSocketError::Invalid);
        }
    };
    if namespace.write_socket_record(&record).is_err() {
        cleanup_staged_record_checked(&namespace, staging, &record)?;
        return Err(AnchoredSocketError::Invalid);
    }

    let child = match child_cstring(claim.child()) {
        Ok(child) => child,
        Err(error) => {
            cleanup_staged_record_checked(&namespace, staging, &record)?;
            return Err(error);
        }
    };
    // SAFETY: Both anchors and C strings remain live for this synchronous exclusive rename.
    let published = unsafe {
        renameatx_np(
            namespace.anchor_fd(),
            staging.as_ptr(),
            directory_anchor,
            child.as_ptr(),
            RENAME_EXCL,
        )
    };
    if published != 0 {
        let error = io::Error::last_os_error();
        cleanup_staged_record_checked(&namespace, staging, &record)?;
        return if matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::AddrInUse
        ) {
            Err(AnchoredSocketError::PathExists)
        } else {
            Err(AnchoredSocketError::Io(error.kind()))
        };
    }

    if !matches!(
        relative_socket_identity(directory_anchor, &child),
        Ok(final_identity) if final_identity == identity
    ) {
        cleanup_published_claim_checked(&namespace, &claim, &record)?;
        return Err(AnchoredSocketError::PathChanged);
    }

    if role == ResourceRole::VsockSocketDirectory {
        let prepared = broker
            .as_ref()
            .ok_or(AnchoredSocketError::Invalid)
            .and_then(SocketBrokerClaim::endpoint)
            .and_then(|endpoint| {
                prepare_connector_endpoint(endpoint)?;
                wait_for_broker(endpoint, libc::POLLOUT, AnchoredSocketError::Cancelled)
            });
        if let Err(error) = prepared {
            cleanup_published_claim_checked(&namespace, &claim, &record)?;
            return Err(pre_activation_broker_error(error));
        }
    }
    if cancelled() {
        cleanup_published_claim_checked(&namespace, &claim, &record)?;
        return Err(AnchoredSocketError::Cancelled);
    }

    let (claim, connector) = if role == ResourceRole::VsockSocketDirectory {
        let endpoint = match broker
            .ok_or(AnchoredSocketError::Invalid)
            .and_then(SocketBrokerClaim::commit)
        {
            Ok(endpoint) => endpoint,
            Err(_) => {
                cleanup_published_claim_checked(&namespace, &claim, &record)?;
                return Err(AnchoredSocketError::Broker);
            }
        };
        // The authenticated launcher may observe activation after this point,
        // so neither authority is restored on subsequent failures.
        let claim = claim.commit();
        match spawn_connector(&claim, endpoint) {
            Ok(connector) => (claim, Some(connector)),
            Err(_) => {
                cleanup_published_record(&namespace, &claim, &record, None);
                return Err(AnchoredSocketError::Broker);
            }
        }
    } else {
        (claim.commit(), None)
    };

    Ok(BoundAnchoredSocket {
        listener,
        guard: AnchoredSocketGuard {
            #[cfg(feature = "elevated-bootstrap-probe")]
            ownership: AnchoredSocketOwnership::Recorded(namespace),
            #[cfg(not(feature = "elevated-bootstrap-probe"))]
            namespace,
            claim,
            record,
            expected_owner: None,
        },
        connector,
    })
}

const fn pre_activation_broker_error(error: AnchoredSocketError) -> AnchoredSocketError {
    if matches!(error, AnchoredSocketError::Cancelled) {
        AnchoredSocketError::Cancelled
    } else {
        AnchoredSocketError::Broker
    }
}

fn spawn_binder(
    namespace: &WorkerSocketNamespace,
    role: ResourceRole,
) -> Result<(UnixListener, ObjectIdentity), AnchoredSocketError> {
    let (parent_endpoint, child_endpoint) =
        UnixStream::pair().map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    let parent = duplicate_stream(parent_endpoint)?;
    let child = duplicate_stream(child_endpoint)?;
    parent
        .set_read_timeout(Some(BINDER_TIMEOUT))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    parent
        .set_write_timeout(Some(BINDER_TIMEOUT))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;

    let executable = binder_executable()?;
    let executable = CString::new(executable.as_os_str().as_bytes())
        .map_err(|_| AnchoredSocketError::Invalid)?;
    #[cfg(not(test))]
    let argument = CString::new(BINDER_ARGUMENT).map_err(|_| AnchoredSocketError::Invalid)?;
    #[cfg(not(test))]
    let argv = [
        executable.as_ptr().cast_mut(),
        argument.as_ptr().cast_mut(),
        std::ptr::null_mut(),
    ];
    #[cfg(test)]
    let ignored = CString::new("--ignored").map_err(|_| AnchoredSocketError::Invalid)?;
    #[cfg(test)]
    let exact = CString::new("--exact").map_err(|_| AnchoredSocketError::Invalid)?;
    #[cfg(test)]
    let binder_test = CString::new("anchored_socket::tests::binder_process_entry")
        .map_err(|_| AnchoredSocketError::Invalid)?;
    #[cfg(test)]
    let single_thread =
        CString::new("--test-threads=1").map_err(|_| AnchoredSocketError::Invalid)?;
    #[cfg(test)]
    let argv = [
        executable.as_ptr().cast_mut(),
        ignored.as_ptr().cast_mut(),
        exact.as_ptr().cast_mut(),
        binder_test.as_ptr().cast_mut(),
        single_thread.as_ptr().cast_mut(),
        std::ptr::null_mut(),
    ];
    let environment = spawn_environment()?;
    let environment_pointers = pointer_array(&environment);
    let mut attributes = SpawnAttributes::new()?;
    attributes.configure()?;
    let mut actions = SpawnFileActions::new()?;
    let directory = duplicate_descriptor(namespace.anchor_fd())?;
    actions.duplicate(directory.as_raw_fd(), HELPER_DIRECTORY_FD)?;
    actions.close(directory.as_raw_fd())?;
    actions.duplicate(child.as_raw_fd(), BINDER_FD)?;
    actions.close(child.as_raw_fd())?;

    let mut pid = 0;
    // SAFETY: C strings, pointer arrays, attributes, actions, and writable PID remain live.
    let result = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            executable.as_ptr(),
            actions.as_ptr(),
            attributes.as_ptr(),
            argv.as_ptr(),
            environment_pointers.as_ptr(),
        )
    };
    if result != 0 {
        return Err(AnchoredSocketError::Io(
            io::Error::from_raw_os_error(result).kind(),
        ));
    }
    drop(child);
    let mut binder = OwnedHelper::new(pid);
    receive_helper_hello(&parent, HELLO_BINDER)?;
    verify_peer(parent.as_raw_fd(), pid).map_err(|_| AnchoredSocketError::Binder)?;
    let namespace_identity = namespace.identity();
    let command = binder_command(
        role,
        ObjectIdentity {
            device: namespace_identity.device,
            inode: namespace_identity.inode,
        },
    )?;
    send_exact_frame(&parent, &command)?;
    let (listener, response_role, identity) = receive_listener(&parent)?;
    if response_role != role {
        return Err(AnchoredSocketError::Invalid);
    }
    validate_listener_descriptor(listener.as_raw_fd(), role)?;
    verify_peer(parent.as_raw_fd(), pid).map_err(|_| AnchoredSocketError::Binder)?;
    send_exact_frame(&parent, &binder_ack(role, identity)?)?;
    binder.wait_until(BINDER_TIMEOUT)?;
    Ok((listener, identity))
}

fn binder_executable() -> Result<std::path::PathBuf, AnchoredSocketError> {
    env::current_exe().map_err(|error| AnchoredSocketError::Io(error.kind()))
}

fn spawn_connector(
    claim: &ClaimedSocketDirectory,
    endpoint: SocketBrokerEndpoint,
) -> Result<AnchoredVsockConnector, AnchoredSocketError> {
    prepare_connector_endpoint(&endpoint)?;
    wait_for_broker(&endpoint, libc::POLLOUT, AnchoredSocketError::Broker)?;
    let activate = SocketBrokerMessage::Activate {
        session: endpoint.session,
        sequence: 1,
        child: claim.child.clone(),
    };
    send_socket_broker_message(&endpoint.socket, &activate, None)
        .map_err(|_| AnchoredSocketError::Binder)?;
    wait_for_broker(&endpoint, libc::POLLIN, AnchoredSocketError::Broker)?;
    let response =
        receive_socket_broker_message(&endpoint.socket).map_err(|_| AnchoredSocketError::Binder)?;
    verify_peer_pid(endpoint.socket.as_raw_fd(), endpoint.launcher_pid)
        .map_err(|_| AnchoredSocketError::Binder)?;
    if !matches!(
        response,
        bangbang_session::macos::socket_broker::ReceivedSocketBrokerMessage {
            message: SocketBrokerMessage::Ready { session, sequence: 1 },
            descriptor: None,
        } if session == endpoint.session
    ) {
        return Err(AnchoredSocketError::Invalid);
    }
    let SocketBrokerEndpoint {
        socket,
        session,
        launcher_pid,
        wakeup: _,
    } = endpoint;
    Ok(AnchoredVsockConnector {
        socket,
        session,
        launcher_pid,
        next_sequence: 2,
        healthy: true,
    })
}

fn wait_for_broker(
    endpoint: &SocketBrokerEndpoint,
    broker_events: libc::c_short,
    wakeup_error: AnchoredSocketError,
) -> Result<(), AnchoredSocketError> {
    let mut descriptors = [
        libc::pollfd {
            fd: endpoint.socket.as_raw_fd(),
            events: broker_events,
            revents: 0,
        },
        libc::pollfd {
            fd: endpoint
                .wakeup
                .as_ref()
                .map_or(-1, |wakeup| wakeup.as_raw_fd()),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let descriptor_count = if endpoint.wakeup.is_some() {
        descriptors.len()
    } else {
        descriptors.len() - 1
    };
    let timeout =
        i32::try_from(BINDER_TIMEOUT.as_millis()).map_err(|_| AnchoredSocketError::Invalid)?;
    loop {
        // SAFETY: The initialized poll array remains writable for the call.
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptor_count as libc::nfds_t,
                timeout,
            )
        };
        if result < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(AnchoredSocketError::Broker);
        }
        if result == 0 {
            return Err(AnchoredSocketError::Broker);
        }
        let invalid = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
        if endpoint.wakeup.is_some() && descriptors[1].revents & (libc::POLLIN | invalid) != 0 {
            return Err(wakeup_error);
        }
        if descriptors[0].revents & invalid != 0 {
            return Err(AnchoredSocketError::Broker);
        }
        if descriptors[0].revents & broker_events != 0 {
            if broker_events & libc::POLLIN != 0 {
                let mut byte = 0_u8;
                // SAFETY: The byte is writable and MSG_PEEK preserves the
                // complete authenticated broker datagram for its decoder.
                let peeked = unsafe {
                    libc::recv(
                        endpoint.socket.as_raw_fd(),
                        (&raw mut byte).cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                if peeked <= 0 {
                    if peeked < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    return Err(AnchoredSocketError::Broker);
                }
            }
            return Ok(());
        }
    }
}

fn prepare_connector_endpoint(endpoint: &SocketBrokerEndpoint) -> Result<(), AnchoredSocketError> {
    endpoint
        .socket
        .set_read_timeout(Some(BINDER_TIMEOUT))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    endpoint
        .socket
        .set_write_timeout(Some(BINDER_TIMEOUT))
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    verify_peer_pid(endpoint.socket.as_raw_fd(), endpoint.launcher_pid)
        .map_err(|_| AnchoredSocketError::Binder)?;
    Ok(())
}

fn helper_hello(kind: u8) -> Result<[u8; HELLO_BYTES], AnchoredSocketError> {
    if kind != HELLO_BINDER {
        return Err(AnchoredSocketError::Invalid);
    }
    let mut hello = [0_u8; HELLO_BYTES];
    hello[..4].copy_from_slice(&HELLO_MAGIC);
    hello[4] = HELLO_VERSION;
    hello[5] = kind;
    Ok(hello)
}

fn binder_command(
    role: ResourceRole,
    identity: ObjectIdentity,
) -> Result<[u8; BINDER_COMMAND_BYTES], AnchoredSocketError> {
    let mut command = [0_u8; BINDER_COMMAND_BYTES];
    command[..4].copy_from_slice(&BINDER_COMMAND_MAGIC);
    command[4] = BINDER_COMMAND_VERSION;
    command[5] = role_byte(role)?;
    command[8..16].copy_from_slice(&identity.device.to_be_bytes());
    command[16..24].copy_from_slice(&identity.inode.to_be_bytes());
    Ok(command)
}

fn parse_binder_command(
    command: &[u8],
) -> Result<(ResourceRole, ObjectIdentity), AnchoredSocketError> {
    if command.len() != BINDER_COMMAND_BYTES
        || command.get(..4) != Some(BINDER_COMMAND_MAGIC.as_slice())
        || command.get(4) != Some(&BINDER_COMMAND_VERSION)
        || command.get(6..8) != Some([0, 0].as_slice())
    {
        return Err(AnchoredSocketError::Invalid);
    }
    let role = socket_role(*command.get(5).ok_or(AnchoredSocketError::Invalid)?)
        .ok_or(AnchoredSocketError::Invalid)?;
    Ok((
        role,
        ObjectIdentity {
            device: parse_u64(command, 8)?,
            inode: parse_u64(command, 16)?,
        },
    ))
}

fn receive_helper_hello(socket: &UnixStream, expected_kind: u8) -> Result<(), AnchoredSocketError> {
    let mut hello = [0_u8; HELLO_BYTES];
    let mut reader = socket;
    reader
        .read_exact(&mut hello)
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    if hello == helper_hello(expected_kind)? {
        Ok(())
    } else {
        Err(AnchoredSocketError::Invalid)
    }
}

fn send_exact_frame(socket: &UnixStream, bytes: &[u8]) -> Result<(), AnchoredSocketError> {
    let mut writer = socket;
    writer
        .write_all(bytes)
        .map_err(|error| AnchoredSocketError::Io(error.kind()))
}

fn send_descriptor_frame(
    socket: &UnixStream,
    bytes: &[u8],
    descriptor: RawFd,
) -> Result<(), AnchoredSocketError> {
    let mut iovec = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: 1,
    };
    let mut control = [0_u32; CONTROL_WORDS];
    // SAFETY: An all-zero header is valid before its live buffer fields are assigned.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    let control_length = cmsg_space(size_of::<RawFd>()).ok_or(AnchoredSocketError::Invalid)?;
    message.msg_controllen =
        libc::socklen_t::try_from(control_length).map_err(|_| AnchoredSocketError::Invalid)?;
    let header = message.msg_control.cast::<libc::cmsghdr>();
    // SAFETY: The aligned control buffer has room for one header and descriptor.
    unsafe {
        (*header).cmsg_len = libc::socklen_t::try_from(
            cmsg_len(size_of::<RawFd>()).ok_or(AnchoredSocketError::Invalid)?,
        )
        .map_err(|_| AnchoredSocketError::Invalid)?;
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        std::ptr::copy_nonoverlapping(
            (&raw const descriptor).cast::<u8>(),
            message.msg_control.cast::<u8>().add(cmsg_aligned_header()),
            size_of::<RawFd>(),
        );
    }
    // SAFETY: The message borrows only live stack storage for this synchronous send.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &raw const message, 0) };
    if sent < 0 {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    if sent != 1 {
        return Err(AnchoredSocketError::Invalid);
    }
    send_exact_frame(socket, bytes.get(1..).ok_or(AnchoredSocketError::Invalid)?)
}

fn receive_frame_with_rights(
    socket: &UnixStream,
    frame: &mut [u8],
) -> Result<Vec<OwnedFd>, AnchoredSocketError> {
    if frame.is_empty() {
        return Err(AnchoredSocketError::Invalid);
    }
    let mut control = [0_u32; CONTROL_WORDS];
    let mut iovec = libc::iovec {
        iov_base: frame.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // SAFETY: An all-zero header is valid before its live buffer fields are assigned.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    let receive_control = cmsg_space(
        size_of::<RawFd>()
            .checked_mul(2)
            .ok_or(AnchoredSocketError::Invalid)?,
    )
    .ok_or(AnchoredSocketError::Invalid)?;
    message.msg_controllen =
        libc::socklen_t::try_from(receive_control).map_err(|_| AnchoredSocketError::Invalid)?;
    // SAFETY: The message points only to live writable stack buffers.
    let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut message, 0) };
    if received < 0 {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    let returned_control =
        usize::try_from(message.msg_controllen).map_err(|_| AnchoredSocketError::Invalid)?;
    let descriptors = if returned_control == 0 {
        Vec::new()
    } else {
        parse_control(
            control.as_ptr().cast(),
            returned_control.min(control.len() * size_of::<u32>()),
        )?
    };
    if received != 1
        || returned_control > receive_control
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
    {
        return Err(AnchoredSocketError::Invalid);
    }
    let mut reader = socket;
    reader
        .read_exact(frame.get_mut(1..).ok_or(AnchoredSocketError::Invalid)?)
        .map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    Ok(descriptors)
}

fn current_directory_identity() -> Result<ObjectIdentity, AnchoredSocketError> {
    let current = c".";
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The output is writable and the fixed relative name is live.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            current.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    // SAFETY: Successful `fstatat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(AnchoredSocketError::Invalid);
    }
    Ok(stat_identity(&stat))
}

fn enter_helper_directory(expected: ObjectIdentity) -> Result<(), AnchoredSocketError> {
    set_cloexec(HELPER_DIRECTORY_FD).map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    // SAFETY: The private spawn contract transfers fd 6 exactly once here.
    let directory = unsafe { OwnedFd::from_raw_fd(HELPER_DIRECTORY_FD) };
    // SAFETY: The transferred descriptor is the live directory anchor supplied by the parent.
    if unsafe { libc::fchdir(directory.as_raw_fd()) } != 0 {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    if current_directory_identity()? != expected {
        return Err(AnchoredSocketError::PathChanged);
    }
    Ok(())
}

fn validate_connected_stream_descriptor(descriptor: RawFd) -> Result<(), AnchoredSocketError> {
    if socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_int_option(descriptor, libc::SO_ERROR)? != 0
    {
        return Err(AnchoredSocketError::Invalid);
    }
    // SAFETY: `F_GETFL` only inspects the live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || flags & libc::O_NONBLOCK == 0 {
        return Err(AnchoredSocketError::Invalid);
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>())
        .map_err(|_| AnchoredSocketError::Invalid)?;
    // SAFETY: Address storage and its length are writable for this live connected socket.
    if unsafe { libc::getpeername(descriptor, address.as_mut_ptr().cast(), &raw mut length) } != 0 {
        return Err(AnchoredSocketError::Invalid);
    }
    // SAFETY: Successful `getpeername` initialized the returned family.
    let address = unsafe { address.assume_init() };
    if address.sun_family
        != libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| AnchoredSocketError::Invalid)?
        || usize::try_from(length).map_err(|_| AnchoredSocketError::Invalid)?
            <= std::mem::offset_of!(libc::sockaddr_un, sun_path)
    {
        return Err(AnchoredSocketError::Invalid);
    }
    Ok(())
}

fn socket_int_option(descriptor: RawFd, option: libc::c_int) -> Result<i32, AnchoredSocketError> {
    let mut value = 0_i32;
    let mut length =
        libc::socklen_t::try_from(size_of::<i32>()).map_err(|_| AnchoredSocketError::Invalid)?;
    // SAFETY: The option value and length are writable for this live socket descriptor.
    if unsafe {
        libc::getsockopt(
            descriptor,
            libc::SOL_SOCKET,
            option,
            (&raw mut value).cast(),
            &raw mut length,
        )
    } != 0
        || usize::try_from(length).ok() != Some(size_of::<i32>())
    {
        return Err(AnchoredSocketError::Invalid);
    }
    Ok(value)
}

fn stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}

fn parse_u64(message: &[u8], offset: usize) -> Result<u64, AnchoredSocketError> {
    let bytes = message
        .get(offset..offset.saturating_add(size_of::<u64>()))
        .ok_or(AnchoredSocketError::Invalid)?;
    Ok(u64::from_be_bytes(
        bytes.try_into().map_err(|_| AnchoredSocketError::Invalid)?,
    ))
}

fn validate_listener_descriptor(
    descriptor: RawFd,
    role: ResourceRole,
) -> Result<(), AnchoredSocketError> {
    let staging = socket_staging_name(role).map_err(|_| AnchoredSocketError::Invalid)?;
    validate_listener_descriptor_for_name(descriptor, staging)
}

fn validate_listener_descriptor_for_name(
    descriptor: RawFd,
    expected_name: &std::ffi::CStr,
) -> Result<(), AnchoredSocketError> {
    let mut socket_type = 0;
    let mut socket_type_len = libc::socklen_t::try_from(size_of::<libc::c_int>())
        .map_err(|_| AnchoredSocketError::Invalid)?;
    // SAFETY: Both option buffers are writable and descriptor remains live.
    if unsafe {
        libc::getsockopt(
            descriptor,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut socket_type).cast(),
            &raw mut socket_type_len,
        )
    } != 0
        || socket_type_len as usize != size_of::<libc::c_int>()
        || socket_type != libc::SOCK_STREAM
    {
        return Err(AnchoredSocketError::Invalid);
    }
    validate_accepting_listener(descriptor)?;
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut address_len = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>())
        .map_err(|_| AnchoredSocketError::Invalid)?;
    // SAFETY: Address storage and its length are writable for this live local socket.
    if unsafe {
        libc::getsockname(
            descriptor,
            address.as_mut_ptr().cast(),
            &raw mut address_len,
        )
    } != 0
    {
        return Err(AnchoredSocketError::Invalid);
    }
    // SAFETY: Successful getsockname initialized the returned address prefix.
    let address = unsafe { address.assume_init() };
    let family =
        libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| AnchoredSocketError::Invalid)?;
    if address.sun_family != family {
        return Err(AnchoredSocketError::Invalid);
    }
    let path_offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    let returned = usize::try_from(address_len).map_err(|_| AnchoredSocketError::Invalid)?;
    let path_len = returned
        .checked_sub(path_offset)
        .ok_or(AnchoredSocketError::Invalid)?;
    let expected = expected_name.to_bytes_with_nul();
    if path_len != expected.len() {
        return Err(AnchoredSocketError::Invalid);
    }
    // SAFETY: `path_len` was derived from the kernel-returned bounded sockaddr length.
    let path =
        unsafe { std::slice::from_raw_parts(address.sun_path.as_ptr().cast::<u8>(), path_len) };
    if path != expected {
        return Err(AnchoredSocketError::Invalid);
    }
    Ok(())
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn validate_elevated_listener_descriptor(
    descriptor: RawFd,
    expected_name: &std::ffi::CStr,
) -> Result<(), AnchoredSocketError> {
    // SAFETY: F_GETFD and F_GETFL inspect the live received descriptor.
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    // SAFETY: F_GETFL inspects the same live received descriptor.
    let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if descriptor_flags < 0
        || status_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || status_flags & libc::O_NONBLOCK == 0
        || socket_int_option(descriptor, libc::SO_ERROR)? != 0
    {
        return Err(AnchoredSocketError::Invalid);
    }
    validate_listener_descriptor_for_name(descriptor, expected_name)
}

fn validate_accepting_listener(descriptor: RawFd) -> Result<(), AnchoredSocketError> {
    // SAFETY: `F_GETFL` only inspects the live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    let restore_flags = flags & libc::O_NONBLOCK == 0;
    if restore_flags {
        // SAFETY: `F_SETFL` changes only status flags on the live staging listener.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
        }
    }

    // SAFETY: Null address arguments request only a new descriptor from this staging listener.
    let accepted = unsafe { libc::accept(descriptor, std::ptr::null_mut(), std::ptr::null_mut()) };
    let accept_error = (accepted < 0).then(io::Error::last_os_error);
    if accepted >= 0 {
        // SAFETY: A successful accept returns a uniquely owned descriptor.
        drop(unsafe { OwnedFd::from_raw_fd(accepted) });
    }
    if restore_flags {
        // SAFETY: Restores the exact status flags observed before the probe.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags) } < 0 {
            return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
        }
    }

    match accept_error.map(|error| error.kind()) {
        Some(io::ErrorKind::WouldBlock) => Ok(()),
        Some(_) | None => Err(AnchoredSocketError::Invalid),
    }
}

fn role_byte(role: ResourceRole) -> Result<u8, AnchoredSocketError> {
    match role {
        ResourceRole::ApiSocketDirectory => Ok(7),
        ResourceRole::VsockSocketDirectory => Ok(8),
        _ => Err(AnchoredSocketError::Invalid),
    }
}

fn socket_role(byte: u8) -> Option<ResourceRole> {
    match byte {
        7 => Some(ResourceRole::ApiSocketDirectory),
        8 => Some(ResourceRole::VsockSocketDirectory),
        _ => None,
    }
}

fn response(
    role: ResourceRole,
    identity: ObjectIdentity,
) -> Result<[u8; RESPONSE_BYTES], AnchoredSocketError> {
    let mut response = [0_u8; RESPONSE_BYTES];
    response[..4].copy_from_slice(&RESPONSE_MAGIC);
    response[4] = RESPONSE_VERSION;
    response[5] = role_byte(role)?;
    response[8..16].copy_from_slice(&identity.device.to_be_bytes());
    response[16..24].copy_from_slice(&identity.inode.to_be_bytes());
    Ok(response)
}

fn parse_response(bytes: &[u8]) -> Result<(ResourceRole, ObjectIdentity), AnchoredSocketError> {
    if bytes.len() != RESPONSE_BYTES
        || bytes.get(..4) != Some(RESPONSE_MAGIC.as_slice())
        || bytes.get(4) != Some(&RESPONSE_VERSION)
        || bytes.get(6..8) != Some([0, 0].as_slice())
    {
        return Err(AnchoredSocketError::Invalid);
    }
    let role = socket_role(*bytes.get(5).ok_or(AnchoredSocketError::Invalid)?)
        .ok_or(AnchoredSocketError::Invalid)?;
    let device = u64::from_be_bytes(
        bytes
            .get(8..16)
            .ok_or(AnchoredSocketError::Invalid)?
            .try_into()
            .map_err(|_| AnchoredSocketError::Invalid)?,
    );
    let inode = u64::from_be_bytes(
        bytes
            .get(16..24)
            .ok_or(AnchoredSocketError::Invalid)?
            .try_into()
            .map_err(|_| AnchoredSocketError::Invalid)?,
    );
    Ok((role, ObjectIdentity { device, inode }))
}

fn binder_ack(
    role: ResourceRole,
    identity: ObjectIdentity,
) -> Result<[u8; BINDER_ACK_BYTES], AnchoredSocketError> {
    let mut acknowledgment = [0_u8; BINDER_ACK_BYTES];
    acknowledgment[..4].copy_from_slice(&BINDER_ACK_MAGIC);
    acknowledgment[4] = BINDER_ACK_VERSION;
    acknowledgment[5] = role_byte(role)?;
    acknowledgment[8..16].copy_from_slice(&identity.device.to_be_bytes());
    acknowledgment[16..24].copy_from_slice(&identity.inode.to_be_bytes());
    Ok(acknowledgment)
}

fn parse_binder_ack(bytes: &[u8]) -> Result<(ResourceRole, ObjectIdentity), AnchoredSocketError> {
    if bytes.len() != BINDER_ACK_BYTES
        || bytes.get(..4) != Some(BINDER_ACK_MAGIC.as_slice())
        || bytes.get(4) != Some(&BINDER_ACK_VERSION)
        || bytes.get(6..8) != Some([0, 0].as_slice())
    {
        return Err(AnchoredSocketError::Invalid);
    }
    let role = socket_role(*bytes.get(5).ok_or(AnchoredSocketError::Invalid)?)
        .ok_or(AnchoredSocketError::Invalid)?;
    let device = u64::from_be_bytes(
        bytes
            .get(8..16)
            .ok_or(AnchoredSocketError::Invalid)?
            .try_into()
            .map_err(|_| AnchoredSocketError::Invalid)?,
    );
    let inode = u64::from_be_bytes(
        bytes
            .get(16..24)
            .ok_or(AnchoredSocketError::Invalid)?
            .try_into()
            .map_err(|_| AnchoredSocketError::Invalid)?,
    );
    Ok((role, ObjectIdentity { device, inode }))
}

fn send_listener(
    socket: &UnixStream,
    role: ResourceRole,
    identity: ObjectIdentity,
    listener: RawFd,
) -> Result<(), AnchoredSocketError> {
    let response = response(role, identity)?;
    send_descriptor_frame(socket, &response, listener)
}

fn receive_listener(
    socket: &UnixStream,
) -> Result<(UnixListener, ResourceRole, ObjectIdentity), AnchoredSocketError> {
    let mut response = [0_u8; RESPONSE_BYTES];
    let descriptors = receive_frame_with_rights(socket, &mut response)?;
    if descriptors.len() != 1 {
        return Err(AnchoredSocketError::Invalid);
    }
    let (role, identity) = parse_response(&response)?;
    let mut descriptors = descriptors.into_iter();
    let descriptor = descriptors.next().ok_or(AnchoredSocketError::Invalid)?;
    set_cloexec(descriptor.as_raw_fd()).map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    let listener = UnixListener::from(descriptor);
    Ok((listener, role, identity))
}

fn parse_control(control: *const u8, length: usize) -> Result<Vec<OwnedFd>, AnchoredSocketError> {
    if length < size_of::<libc::cmsghdr>() {
        return Err(AnchoredSocketError::Invalid);
    }
    let mut descriptors = Vec::new();
    let mut offset = 0_usize;
    let mut valid = true;
    while offset < length {
        let remaining = length.saturating_sub(offset);
        if remaining < size_of::<libc::cmsghdr>() {
            valid = false;
            break;
        }
        // SAFETY: Bounds make one possibly unaligned kernel-populated header readable.
        let header: libc::cmsghdr = unsafe { std::ptr::read_unaligned(control.add(offset).cast()) };
        let declared =
            usize::try_from(header.cmsg_len).map_err(|_| AnchoredSocketError::Invalid)?;
        let header_bytes = cmsg_aligned_header();
        if declared < header_bytes || declared > remaining {
            valid = false;
            break;
        }
        let data_bytes = declared.saturating_sub(header_bytes);
        if header.cmsg_level != libc::SOL_SOCKET
            || header.cmsg_type != libc::SCM_RIGHTS
            || data_bytes == 0
            || data_bytes % size_of::<RawFd>() != 0
        {
            valid = false;
            break;
        }
        for index in 0..(data_bytes / size_of::<RawFd>()) {
            let descriptor_offset = offset
                .checked_add(header_bytes)
                .and_then(|value| value.checked_add(index * size_of::<RawFd>()))
                .ok_or(AnchoredSocketError::Invalid)?;
            // SAFETY: The complete descriptor lies in returned control storage.
            let descriptor =
                unsafe { std::ptr::read_unaligned(control.add(descriptor_offset).cast::<RawFd>()) };
            if descriptor < 0 {
                valid = false;
            } else {
                // SAFETY: Each SCM_RIGHTS descriptor is newly owned by this process.
                descriptors.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
            }
        }
        let next = align_up(declared, CMSG_ALIGNMENT)
            .and_then(|value| offset.checked_add(value))
            .ok_or(AnchoredSocketError::Invalid)?;
        if next > length {
            valid = false;
            break;
        }
        offset = next;
    }
    if !valid || descriptors.len() > 2 {
        return Err(AnchoredSocketError::Invalid);
    }
    Ok(descriptors)
}

fn relative_socket_identity(
    directory: RawFd,
    name: &std::ffi::CStr,
) -> Result<ObjectIdentity, AnchoredSocketError> {
    relative_socket_identity_with_owner(directory, name, None)
}

fn relative_socket_identity_with_owner(
    directory: RawFd,
    name: &std::ffi::CStr,
    expected_owner: Option<(u32, u32)>,
) -> Result<ObjectIdentity, AnchoredSocketError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable, the directory is live, and the C string is fixed/live.
    if unsafe {
        libc::fstatat(
            directory,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    // SAFETY: Successful `fstatat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
    {
        return Err(AnchoredSocketError::Invalid);
    }
    // SAFETY: Effective identity calls have no pointer or ownership contract.
    let default_uid = unsafe { libc::geteuid() };
    if stat.st_uid != expected_owner.map_or(default_uid, |(uid, _)| uid)
        || expected_owner.is_some_and(|(_, gid)| stat.st_gid != gid)
    {
        return Err(AnchoredSocketError::Invalid);
    }
    Ok(ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    })
}

fn ensure_relative_absent(
    directory: RawFd,
    name: &std::ffi::CStr,
) -> Result<(), AnchoredSocketError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable and both directory and name remain live.
    if unsafe {
        libc::fstatat(
            directory,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(AnchoredSocketError::PathExists);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(AnchoredSocketError::Io(error.kind()))
    }
}

fn unlink_socket_if_owned(
    directory: RawFd,
    child: &SocketChild,
    identity: ObjectIdentity,
) -> Result<(), AnchoredSocketError> {
    unlink_socket_if_owned_with_owner(directory, child, identity, None)
}

fn unlink_socket_if_owned_with_owner(
    directory: RawFd,
    child: &SocketChild,
    identity: ObjectIdentity,
    expected_owner: Option<(u32, u32)>,
) -> Result<(), AnchoredSocketError> {
    let child = child_cstring(child)?;
    unlink_relative_if_socket_at_with_owner(directory, &child, Some(identity), expected_owner)
}

fn cleanup_staged_socket_checked(
    namespace: &WorkerSocketNamespace,
    staging: &std::ffi::CStr,
    identity: Option<ObjectIdentity>,
) -> Result<(), AnchoredSocketError> {
    checked_cleanup(unlink_relative_if_socket_at(
        namespace.anchor_fd(),
        staging,
        identity,
    ))
}

fn cleanup_staged_record_checked(
    namespace: &WorkerSocketNamespace,
    staging: &std::ffi::CStr,
    record: &SocketOwnershipRecord,
) -> Result<(), AnchoredSocketError> {
    cleanup_staged_socket_checked(namespace, staging, Some(record.identity()))?;
    namespace
        .clear_socket_record(record)
        .map_err(|_| AnchoredSocketError::Cleanup)
}

fn cleanup_published_record(
    namespace: &WorkerSocketNamespace,
    claim: &ClaimedSocketDirectory,
    record: &SocketOwnershipRecord,
    expected_owner: Option<(u32, u32)>,
) {
    let cleanup = unlink_socket_if_owned_with_owner(
        claim.directory.anchor_fd(),
        &claim.child,
        record.identity(),
        expected_owner,
    );
    if matches!(
        cleanup,
        Ok(()) | Err(AnchoredSocketError::Invalid | AnchoredSocketError::PathChanged)
    ) {
        let _ = namespace.clear_socket_record(record);
    }
}

fn cleanup_published_claim_checked(
    namespace: &WorkerSocketNamespace,
    claim: &SocketDirectoryClaim,
    record: &SocketOwnershipRecord,
) -> Result<(), AnchoredSocketError> {
    let directory = claim
        .directory_anchor_fd()
        .map_err(|_| AnchoredSocketError::Cleanup)?;
    checked_cleanup(unlink_socket_if_owned(
        directory,
        claim.child(),
        record.identity(),
    ))?;
    namespace
        .clear_socket_record(record)
        .map_err(|_| AnchoredSocketError::Cleanup)
}

fn checked_cleanup(result: Result<(), AnchoredSocketError>) -> Result<(), AnchoredSocketError> {
    match result {
        Ok(()) | Err(AnchoredSocketError::Invalid | AnchoredSocketError::PathChanged) => Ok(()),
        Err(_) => Err(AnchoredSocketError::Cleanup),
    }
}

fn unlink_relative_if_socket(
    name: &std::ffi::CStr,
    identity: Option<ObjectIdentity>,
) -> Result<(), AnchoredSocketError> {
    unlink_relative_if_socket_at(libc::AT_FDCWD, name, identity)
}

fn unlink_relative_if_socket_at(
    directory: RawFd,
    name: &std::ffi::CStr,
    identity: Option<ObjectIdentity>,
) -> Result<(), AnchoredSocketError> {
    unlink_relative_if_socket_at_with_owner(directory, name, identity, None)
}

fn unlink_relative_if_socket_at_with_owner(
    directory: RawFd,
    name: &std::ffi::CStr,
    identity: Option<ObjectIdentity>,
    expected_owner: Option<(u32, u32)>,
) -> Result<(), AnchoredSocketError> {
    let current = match relative_socket_identity_with_owner(directory, name, expected_owner) {
        Ok(current) => current,
        Err(AnchoredSocketError::Io(io::ErrorKind::NotFound)) => return Ok(()),
        Err(error) => return Err(error),
    };
    if identity.is_some_and(|identity| identity != current) {
        return Err(AnchoredSocketError::PathChanged);
    }
    // SAFETY: The directory and live C string name the identity-checked socket.
    if unsafe { libc::unlinkat(directory, name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()))
    }
}

fn child_cstring(child: &SocketChild) -> Result<CString, AnchoredSocketError> {
    CString::new(child.as_bytes()).map_err(|_| AnchoredSocketError::Invalid)
}

fn duplicate_stream(socket: UnixStream) -> Result<UnixStream, AnchoredSocketError> {
    Ok(UnixStream::from(duplicate_descriptor(socket.as_raw_fd())?))
}

fn duplicate_descriptor(descriptor: RawFd) -> Result<OwnedFd, AnchoredSocketError> {
    // SAFETY: Source is live; a successful result is a new close-on-exec descriptor.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, MIN_PARENT_FD) };
    if duplicate < 0 {
        return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
    }
    // SAFETY: The successful result is a uniquely owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn spawn_environment() -> Result<Vec<CString>, AnchoredSocketError> {
    env::vars_os()
        .map(|(name, value)| {
            let mut entry = Vec::with_capacity(name.len().saturating_add(value.len()) + 1);
            entry.extend_from_slice(name.as_bytes());
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).map_err(|_| AnchoredSocketError::Invalid)
        })
        .collect()
}

fn pointer_array(values: &[CString]) -> Vec<*mut c_char> {
    values
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect()
}

fn cmsg_aligned_header() -> usize {
    align_up(size_of::<libc::cmsghdr>(), CMSG_ALIGNMENT).unwrap_or(size_of::<libc::cmsghdr>())
}

fn cmsg_len(data_bytes: usize) -> Option<usize> {
    cmsg_aligned_header().checked_add(data_bytes)
}

fn cmsg_space(data_bytes: usize) -> Option<usize> {
    cmsg_aligned_header().checked_add(align_up(data_bytes, CMSG_ALIGNMENT)?)
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|value| value & !(alignment - 1))
}

struct OwnedHelper {
    pid: libc::pid_t,
    reaped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelperExitRegistrationRecovery {
    Reaped(i32),
    ReapBlocking,
    Error(io::ErrorKind),
}

fn helper_exit_registration_recovery(
    error: &io::Error,
    reaped: Option<i32>,
) -> HelperExitRegistrationRecovery {
    match reaped {
        Some(status) => HelperExitRegistrationRecovery::Reaped(status),
        None if error.raw_os_error() == Some(libc::ESRCH) => {
            HelperExitRegistrationRecovery::ReapBlocking
        }
        None => HelperExitRegistrationRecovery::Error(error.kind()),
    }
}

impl OwnedHelper {
    const fn new(pid: libc::pid_t) -> Self {
        Self { pid, reaped: false }
    }

    fn wait_until(&mut self, timeout: Duration) -> Result<(), AnchoredSocketError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(AnchoredSocketError::Binder)?;
        if let Some(status) = self.try_reap()? {
            return helper_status(status);
        }

        // SAFETY: `kqueue` has no pointer arguments and returns a fresh
        // descriptor on success.
        let kqueue = unsafe { libc::kqueue() };
        if kqueue < 0 {
            return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
        }
        // SAFETY: The successful descriptor is uniquely owned here.
        let kqueue = unsafe { OwnedFd::from_raw_fd(kqueue) };
        set_cloexec(kqueue.as_raw_fd()).map_err(|error| AnchoredSocketError::Io(error.kind()))?;
        let child = usize::try_from(self.pid).map_err(|_| AnchoredSocketError::Binder)?;
        let change = libc::kevent {
            ident: child,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: The change remains live for this synchronous registration;
        // no output event buffer or timeout is supplied.
        if unsafe {
            libc::kevent(
                kqueue.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            return match helper_exit_registration_recovery(&error, self.try_reap()?) {
                HelperExitRegistrationRecovery::Reaped(status) => helper_status(status),
                HelperExitRegistrationRecovery::ReapBlocking => {
                    helper_status(self.reap_blocking()?)
                }
                HelperExitRegistrationRecovery::Error(kind) => Err(AnchoredSocketError::Io(kind)),
            };
        }

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout = libc::timespec {
                tv_sec: libc::time_t::try_from(remaining.as_secs()).unwrap_or(libc::time_t::MAX),
                tv_nsec: libc::c_long::from(remaining.subsec_nanos()),
            };
            let mut event = MaybeUninit::<libc::kevent>::uninit();
            // SAFETY: `event` provides room for one kernel-initialized result;
            // the bounded timeout remains live for the synchronous wait.
            let count = unsafe {
                libc::kevent(
                    kqueue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    event.as_mut_ptr(),
                    1,
                    &raw const timeout,
                )
            };
            if count == 0 {
                return Err(AnchoredSocketError::Binder);
            }
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(AnchoredSocketError::Io(error.kind()));
            }
            // SAFETY: A positive count initialized the single output event.
            let event = unsafe { event.assume_init() };
            if event.ident != child
                || event.filter != libc::EVFILT_PROC
                || event.fflags & libc::NOTE_EXIT == 0
            {
                return Err(AnchoredSocketError::Binder);
            }
            return helper_status(self.reap_blocking()?);
        }
    }

    fn try_reap(&mut self) -> Result<Option<i32>, AnchoredSocketError> {
        loop {
            let mut status = 0;
            // SAFETY: PID is this object's unreaped child and status is writable.
            let result = unsafe { libc::waitpid(self.pid, &raw mut status, libc::WNOHANG) };
            if result == self.pid {
                self.reaped = true;
                return Ok(Some(status));
            }
            if result == 0 {
                return Ok(None);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(AnchoredSocketError::Io(error.kind()));
            }
        }
    }

    fn reap_blocking(&mut self) -> Result<i32, AnchoredSocketError> {
        loop {
            let mut status = 0;
            // SAFETY: NOTE_EXIT or an ESRCH registration race established that
            // this owned child exited; status is writable for the blocking reap.
            let result = unsafe { libc::waitpid(self.pid, &raw mut status, 0) };
            if result == self.pid {
                self.reaped = true;
                return Ok(status);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(AnchoredSocketError::Io(error.kind()));
            }
        }
    }

    fn terminate_and_reap(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: PID is the owned unreaped child; ESRCH is harmless here.
        let _ = unsafe { libc::kill(self.pid, libc::SIGKILL) };
        loop {
            let mut status = 0;
            // SAFETY: PID is the owned unreaped child and status is writable.
            let result = unsafe { libc::waitpid(self.pid, &raw mut status, 0) };
            if result == self.pid {
                self.reaped = true;
                return;
            }
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return;
            }
        }
    }
}

const fn helper_status(status: i32) -> Result<(), AnchoredSocketError> {
    if status == 0 {
        Ok(())
    } else {
        Err(AnchoredSocketError::Binder)
    }
}

impl Drop for OwnedHelper {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

struct SpawnAttributes {
    value: MaybeUninit<libc::posix_spawnattr_t>,
    initialized: bool,
}

impl SpawnAttributes {
    fn new() -> Result<Self, AnchoredSocketError> {
        let mut attributes = Self {
            value: MaybeUninit::uninit(),
            initialized: false,
        };
        // SAFETY: Value is writable storage for one Darwin spawn attribute.
        cvt_spawn(unsafe { libc::posix_spawnattr_init(attributes.value.as_mut_ptr()) })?;
        attributes.initialized = true;
        Ok(attributes)
    }

    fn configure(&mut self) -> Result<(), AnchoredSocketError> {
        let mut defaults = MaybeUninit::<libc::sigset_t>::uninit();
        let mut mask = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `defaults` is writable signal-set storage.
        let defaults_result = unsafe { libc::sigfillset(defaults.as_mut_ptr()) };
        // SAFETY: `mask` is distinct writable signal-set storage.
        let mask_result = unsafe { libc::sigemptyset(mask.as_mut_ptr()) };
        if defaults_result != 0 || mask_result != 0 {
            return Err(AnchoredSocketError::Io(io::Error::last_os_error().kind()));
        }
        // SAFETY: Successful initialization made both sets readable for these calls.
        cvt_spawn(unsafe {
            libc::posix_spawnattr_setsigdefault(self.value.as_mut_ptr(), defaults.as_ptr())
        })?;
        // SAFETY: This wrapper and the initialized empty mask remain live.
        cvt_spawn(unsafe {
            libc::posix_spawnattr_setsigmask(self.value.as_mut_ptr(), mask.as_ptr())
        })?;
        let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | libc::POSIX_SPAWN_SETSIGDEF
            | libc::POSIX_SPAWN_SETSIGMASK;
        let flags = libc::c_short::try_from(flags).map_err(|_| AnchoredSocketError::Invalid)?;
        // SAFETY: This wrapper owns one initialized attribute object.
        cvt_spawn(unsafe { libc::posix_spawnattr_setflags(self.value.as_mut_ptr(), flags) })
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        self.value.as_ptr()
    }
}

impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: This wrapper owns one initialized attribute object.
            let _ = unsafe { libc::posix_spawnattr_destroy(self.value.as_mut_ptr()) };
        }
    }
}

struct SpawnFileActions {
    value: MaybeUninit<libc::posix_spawn_file_actions_t>,
    initialized: bool,
}

impl SpawnFileActions {
    fn new() -> Result<Self, AnchoredSocketError> {
        let mut actions = Self {
            value: MaybeUninit::uninit(),
            initialized: false,
        };
        // SAFETY: Value is writable storage for one Darwin file-actions object.
        cvt_spawn(unsafe { libc::posix_spawn_file_actions_init(actions.value.as_mut_ptr()) })?;
        actions.initialized = true;
        Ok(actions)
    }

    fn duplicate(&mut self, source: RawFd, destination: RawFd) -> Result<(), AnchoredSocketError> {
        // SAFETY: Source is live and the child interprets the fixed destination integer.
        cvt_spawn(unsafe {
            libc::posix_spawn_file_actions_adddup2(self.value.as_mut_ptr(), source, destination)
        })
    }

    fn close(&mut self, fd: RawFd) -> Result<(), AnchoredSocketError> {
        // SAFETY: The initialized actions object interprets fd in the child.
        cvt_spawn(unsafe { libc::posix_spawn_file_actions_addclose(self.value.as_mut_ptr(), fd) })
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        self.value.as_ptr()
    }
}

impl Drop for SpawnFileActions {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: This wrapper owns one initialized file-actions object.
            let _ = unsafe { libc::posix_spawn_file_actions_destroy(self.value.as_mut_ptr()) };
        }
    }
}

fn cvt_spawn(result: libc::c_int) -> Result<(), AnchoredSocketError> {
    if result == 0 {
        Ok(())
    } else {
        Err(AnchoredSocketError::Io(
            io::Error::from_raw_os_error(result).kind(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::rc::Rc;

    use bangbang_session::SessionId;

    use super::*;

    #[test]
    #[ignore = "private binder subprocess entry point"]
    fn binder_process_entry() {
        assert!(run_binder());
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    const ELEVATED_LISTENER_TEST_FD: RawFd = 9;

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    #[ignore = "private elevated-listener subprocess entry point"]
    fn elevated_listener_process_entry() {
        use bangbang_session::elevated_probe::GUEST_API_SOCKET_CHILD;

        set_cloexec(ELEVATED_LISTENER_TEST_FD)
            .expect("inherited test transport should become close-on-exec");
        // SAFETY: The test spawn contract transfers fd 9 exactly once here.
        let transport =
            UnixStream::from(unsafe { OwnedFd::from_raw_fd(ELEVATED_LISTENER_TEST_FD) });
        let listener = UnixListener::bind(GUEST_API_SOCKET_CHILD)
            .expect("relative elevated listener should bind");
        fs::set_permissions(GUEST_API_SOCKET_CHILD, fs::Permissions::from_mode(0o600))
            .expect("elevated listener mode should install");
        listener
            .set_nonblocking(true)
            .expect("elevated listener should become nonblocking");
        let child = CString::new(GUEST_API_SOCKET_CHILD).expect("fixed child should encode");
        let identity = relative_socket_identity(libc::AT_FDCWD, &child)
            .expect("relative listener identity should validate");
        send_listener(
            &transport,
            ResourceRole::ApiSocketDirectory,
            identity,
            listener.as_raw_fd(),
        )
        .expect("elevated listener should transfer");
        let mut acknowledgment = [0_u8; BINDER_ACK_BYTES];
        let mut transport_reader = &transport;
        transport_reader
            .read_exact(&mut acknowledgment)
            .expect("elevated listener adoption should be acknowledged");
        assert_eq!(
            parse_binder_ack(&acknowledgment),
            Ok((ResourceRole::ApiSocketDirectory, identity))
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn receive_relative_elevated_listener(directory: &Path) -> (UnixListener, ObjectIdentity) {
        let (parent, child) = UnixStream::pair().expect("test transport should create");
        parent
            .set_read_timeout(Some(BINDER_TIMEOUT))
            .expect("test transport should be bounded");
        let child_descriptor = child.as_raw_fd();
        let mut command = Command::new(env::current_exe().expect("test executable should resolve"));
        command
            .args([
                "--ignored",
                "--exact",
                "anchored_socket::tests::elevated_listener_process_entry",
                "--test-threads=1",
            ])
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: The child hook performs only async-signal-safe descriptor operations.
        unsafe {
            command.pre_exec(move || {
                if child_descriptor == ELEVATED_LISTENER_TEST_FD {
                    let flags = libc::fcntl(child_descriptor, libc::F_GETFD);
                    if flags < 0
                        || libc::fcntl(child_descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC)
                            < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                } else if libc::dup2(child_descriptor, ELEVATED_LISTENER_TEST_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut process = command
            .spawn()
            .expect("relative listener child should spawn");
        drop(child);
        let (listener, role, identity) =
            receive_listener(&parent).expect("relative listener child should return one listener");
        assert_eq!(role, ResourceRole::ApiSocketDirectory);
        send_exact_frame(
            &parent,
            &binder_ack(ResourceRole::ApiSocketDirectory, identity)
                .expect("listener acknowledgment should encode"),
        )
        .expect("relative listener child should receive adoption acknowledgment");
        assert!(
            process
                .wait()
                .expect("relative listener child should reap")
                .success()
        );
        (listener, identity)
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn prepare_elevated_test_directory(path: &Path) {
        let directory = fs::File::open(path).expect("test directory should open");
        // SAFETY: The descriptor is live and effective identity calls have no pointer contract.
        let status =
            unsafe { libc::fchown(directory.as_raw_fd(), libc::geteuid(), libc::getegid()) };
        assert_eq!(status, 0, "test directory owner and group should install");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("test directory mode should install");
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn elevated_api_adoption_fixture() -> (
        WorkerSocketNamespace,
        ClaimedSocketDirectory,
        crate::contained_session::TestVhostDirectory,
        crate::contained_session::TestVhostDirectory,
        ReceivedApiListener,
    ) {
        use crate::contained_session::{TestVhostDirectory, api_directory_authority_for_test};
        use bangbang_session::elevated_probe::{
            ApiListenerRecord, GUEST_API_SOCKET_REFERENCE, ProbeMode,
        };

        let (authority, api_directory) = api_directory_authority_for_test();
        prepare_elevated_test_directory(api_directory.path());
        let claim = authority
            .claim_socket_directory(
                Path::new(GUEST_API_SOCKET_REFERENCE),
                ResourceRole::ApiSocketDirectory,
            )
            .expect("API directory claim should validate")
            .expect("fixed API reference should claim");
        let namespace_directory = TestVhostDirectory::new();
        prepare_elevated_test_directory(namespace_directory.path());
        let namespace = WorkerSocketNamespace::from_directory_for_test(namespace_directory.path())
            .expect("test namespace should validate");
        let (listener, identity) = receive_relative_elevated_listener(api_directory.path());
        let child = CString::new(bangbang_session::elevated_probe::GUEST_API_SOCKET_CHILD)
            .expect("fixed child should encode");
        // SAFETY: Effective identity calls have no pointer or ownership contract.
        let expected_owner = unsafe { (libc::geteuid(), libc::getegid()) };
        assert_eq!(
            relative_socket_identity_with_owner(
                claim.directory.anchor_fd(),
                &child,
                Some(expected_owner),
            ),
            Ok(identity),
            "fixture path metadata must satisfy elevated adoption"
        );
        assert_eq!(
            validate_elevated_listener_descriptor(listener.as_raw_fd(), &child),
            Ok(()),
            "fixture descriptor must satisfy elevated adoption"
        );
        let record = ApiListenerRecord::launcher_ack(
            ProbeMode::GuestApiDrop,
            SessionId::from_bytes([73; 32]),
            SessionId::from_bytes([74; 32]),
            identity,
        )
        .expect("listener acknowledgment should construct");
        (
            namespace,
            claim,
            namespace_directory,
            api_directory,
            ReceivedApiListener {
                record,
                listener: listener.into(),
            },
        )
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn elevated_listener_adoption_installs_live_authority_and_cleans_exactly() {
        use bangbang_session::elevated_probe::GUEST_API_SOCKET_CHILD;

        let (_namespace, claim, namespace_directory, api_directory, received) =
            elevated_api_adoption_fixture();
        let path = api_directory.path().join(GUEST_API_SOCKET_CHILD);
        let bound = adopt_elevated_api_listener(claim, received)
            .expect("exact elevated listener should adopt");
        assert!(
            fs::symlink_metadata(&path)
                .expect("published listener should exist")
                .file_type()
                .is_socket()
        );
        assert_eq!(
            fs::read_dir(namespace_directory.path())
                .expect("namespace should read")
                .count(),
            0,
            "transferred ownership must not mutate the runtime namespace"
        );
        let client = UnixStream::connect(&path).expect("adopted listener should accept clients");
        let (listener, guard) = bound.into_parts();
        let (server, _) = listener.accept().expect("queued client should accept");
        drop((client, server, listener));
        drop(guard);
        assert_eq!(
            fs::symlink_metadata(&path)
                .expect_err("guard should remove the exact listener")
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            fs::read_dir(namespace_directory.path())
                .expect("namespace should read")
                .count(),
            0
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn elevated_listener_adoption_rejects_flags_substitution_and_queued_clients() {
        use bangbang_session::elevated_probe::GUEST_API_SOCKET_CHILD;

        let (_namespace, claim, namespace_directory, api_directory, received) =
            elevated_api_adoption_fixture();
        let path = api_directory.path().join(GUEST_API_SOCKET_CHILD);
        // SAFETY: F_GETFL and F_SETFL inspect and update the owned test descriptor.
        let flags = unsafe { libc::fcntl(received.listener.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        // SAFETY: The descriptor remains live and the new flags preserve every other bit.
        let status = unsafe {
            libc::fcntl(
                received.listener.as_raw_fd(),
                libc::F_SETFL,
                flags & !libc::O_NONBLOCK,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(
            adopt_elevated_api_listener(claim, received)
                .expect_err("blocking listener should reject"),
            AnchoredSocketError::Invalid
        );
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(namespace_directory.path())
                .expect("namespace should read")
                .count(),
            0
        );

        let (_namespace, claim, _namespace_directory, api_directory, mut received) =
            elevated_api_adoption_fixture();
        let path = api_directory.path().join(GUEST_API_SOCKET_CHILD);
        let (bogus, bogus_peer) = UnixStream::pair().expect("substitute stream should create");
        bogus
            .set_nonblocking(true)
            .expect("substitute should become nonblocking");
        let retained_listener = std::mem::replace(&mut received.listener, bogus.into());
        assert_eq!(
            adopt_elevated_api_listener(claim, received)
                .expect_err("connected descriptor substitution should reject"),
            AnchoredSocketError::Invalid
        );
        drop((bogus_peer, retained_listener));
        assert!(!path.exists());

        let (_namespace, claim, _namespace_directory, api_directory, received) =
            elevated_api_adoption_fixture();
        let path = api_directory.path().join(GUEST_API_SOCKET_CHILD);
        let queued = UnixStream::connect(&path).expect("queued client should connect");
        assert_eq!(
            adopt_elevated_api_listener(claim, received).expect_err("queued client should reject"),
            AnchoredSocketError::Invalid
        );
        drop(queued);
        assert!(!path.exists());
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn elevated_listener_cleanup_preserves_post_adoption_path_replacement() {
        use bangbang_session::elevated_probe::GUEST_API_SOCKET_CHILD;

        let (_namespace, claim, namespace_directory, api_directory, received) =
            elevated_api_adoption_fixture();
        let path = api_directory.path().join(GUEST_API_SOCKET_CHILD);
        let displaced = api_directory.path().join("displaced-evidence-api.sock");
        let bound = adopt_elevated_api_listener(claim, received)
            .expect("exact elevated listener should adopt");
        fs::rename(&path, &displaced).expect("owned listener should displace");
        fs::write(&path, b"replacement\n").expect("replacement should create");
        let (listener, guard) = bound.into_parts();
        drop(listener);
        drop(guard);
        assert_eq!(
            fs::read(&path).expect("replacement should remain"),
            b"replacement\n"
        );
        assert!(
            fs::symlink_metadata(&displaced)
                .expect("displaced listener should remain")
                .file_type()
                .is_socket()
        );
        assert_eq!(
            fs::read_dir(namespace_directory.path())
                .expect("namespace should read")
                .count(),
            0,
            "transferred replacement cleanup must not create an ownership record"
        );
    }

    #[test]
    fn binder_protocol_is_closed_and_redacted() {
        for (role, byte) in [
            (ResourceRole::ApiSocketDirectory, 7),
            (ResourceRole::VsockSocketDirectory, 8),
        ] {
            assert_eq!(role_byte(role), Ok(byte));
            assert_eq!(socket_role(byte), Some(role));
            let identity = ObjectIdentity {
                device: 101,
                inode: 103,
            };
            let encoded = response(role, identity).expect("response should encode");
            assert_eq!(parse_response(&encoded), Ok((role, identity)));
            let acknowledgment = binder_ack(role, identity).expect("acknowledgment should encode");
            assert_eq!(parse_binder_ack(&acknowledgment), Ok((role, identity)));
            let mut malformed_acknowledgment = acknowledgment;
            malformed_acknowledgment[6] = 1;
            assert_eq!(
                parse_binder_ack(&malformed_acknowledgment),
                Err(AnchoredSocketError::Invalid)
            );
            let command = binder_command(role, identity).expect("binder command should encode");
            assert_eq!(parse_binder_command(&command), Ok((role, identity)));

            let mut malformed_command = command;
            malformed_command[6] = 1;
            assert_eq!(
                parse_binder_command(&malformed_command),
                Err(AnchoredSocketError::Invalid)
            );
        }
        assert_eq!(socket_role(0), None);
        assert_eq!(
            role_byte(ResourceRole::KernelImage),
            Err(AnchoredSocketError::Invalid)
        );
        assert_eq!(
            AnchoredSocketError::Invalid.to_string(),
            "private anchored socket operation failed"
        );
    }

    #[test]
    fn missing_fast_helper_registration_falls_back_to_blocking_reap() {
        let missing = io::Error::from_raw_os_error(libc::ESRCH);
        assert_eq!(
            helper_exit_registration_recovery(&missing, None),
            HelperExitRegistrationRecovery::ReapBlocking
        );
        assert_eq!(
            helper_exit_registration_recovery(&missing, Some(0)),
            HelperExitRegistrationRecovery::Reaped(0)
        );

        let invalid = io::Error::from_raw_os_error(libc::EINVAL);
        assert_eq!(
            helper_exit_registration_recovery(&invalid, None),
            HelperExitRegistrationRecovery::Error(io::ErrorKind::InvalidInput)
        );
    }

    #[test]
    fn broker_wait_observes_broker_and_wakeup_without_consuming_either() {
        let (worker, launcher) = UnixDatagram::pair().expect("broker pair should create");
        let (wakeup_reader, mut wakeup_writer) =
            UnixStream::pair().expect("wakeup pair should create");
        // SAFETY: Both endpoints belong to this test process.
        let pid = unsafe { libc::getpid() };
        let endpoint = SocketBrokerEndpoint {
            socket: worker,
            session: SessionId::from_bytes([71; 32]),
            launcher_pid: pid,
            wakeup: Some(Rc::new(wakeup_reader)),
        };

        launcher
            .send(b"ready")
            .expect("launcher readiness should send");
        wait_for_broker(&endpoint, libc::POLLIN, AnchoredSocketError::Broker)
            .expect("broker readiness should wake poll");
        let mut broker_bytes = [0_u8; 5];
        endpoint
            .socket
            .recv(&mut broker_bytes)
            .expect("broker wait must not consume broker data");
        assert_eq!(&broker_bytes, b"ready");

        wakeup_writer
            .write_all(&[1])
            .expect("worker shutdown should signal");
        assert_eq!(
            wait_for_broker(&endpoint, libc::POLLIN, AnchoredSocketError::Cancelled),
            Err(AnchoredSocketError::Cancelled)
        );
        assert_eq!(
            wait_for_broker(&endpoint, libc::POLLIN, AnchoredSocketError::Broker),
            Err(AnchoredSocketError::Broker)
        );
        let mut observed_wakeup = endpoint
            .wakeup
            .as_ref()
            .expect("endpoint should retain wakeup")
            .try_clone()
            .expect("wakeup should clone");
        let mut wakeup_byte = [0_u8; 1];
        observed_wakeup
            .read_exact(&mut wakeup_byte)
            .expect("broker wait must leave shutdown evidence for the outer loop");
        assert_eq!(wakeup_byte, [1]);
    }

    #[test]
    fn launcher_death_interrupts_broker_wait_independently() {
        let (worker, launcher) = UnixDatagram::pair().expect("broker pair should create");
        // SAFETY: Both endpoints belong to this test process.
        let pid = unsafe { libc::getpid() };
        let endpoint = SocketBrokerEndpoint {
            socket: worker,
            session: SessionId::from_bytes([72; 32]),
            launcher_pid: pid,
            wakeup: None,
        };
        launcher
            .shutdown(std::net::Shutdown::Both)
            .expect("launcher endpoint should shut down");
        drop(launcher);
        assert_eq!(
            wait_for_broker(&endpoint, libc::POLLIN, AnchoredSocketError::Broker),
            Err(AnchoredSocketError::Broker)
        );
    }

    #[test]
    fn only_pre_activation_wakeup_cancellation_remains_retryable() {
        assert_eq!(
            pre_activation_broker_error(AnchoredSocketError::Cancelled),
            AnchoredSocketError::Cancelled
        );
        for error in [
            AnchoredSocketError::Binder,
            AnchoredSocketError::Broker,
            AnchoredSocketError::Invalid,
            AnchoredSocketError::Io(io::ErrorKind::Other),
        ] {
            assert_eq!(
                pre_activation_broker_error(error),
                AnchoredSocketError::Broker
            );
        }
    }
}
