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
    namespace: WorkerSocketNamespace,
    claim: ClaimedSocketDirectory,
    record: SocketOwnershipRecord,
}

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

impl Drop for AnchoredSocketGuard {
    fn drop(&mut self) {
        cleanup_published_record(&self.namespace, &self.claim, &self.record);
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
            .and_then(prepare_connector_endpoint);
        if let Err(error) = prepared {
            cleanup_published_claim_checked(&namespace, &claim, &record)?;
            return Err(error);
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
                cleanup_published_record(&namespace, &claim, &record);
                return Err(AnchoredSocketError::Broker);
            }
        }
    } else {
        (claim.commit(), None)
    };

    Ok(BoundAnchoredSocket {
        listener,
        guard: AnchoredSocketGuard {
            namespace,
            claim,
            record,
        },
        connector,
    })
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

    let executable = env::current_exe().map_err(|error| AnchoredSocketError::Io(error.kind()))?;
    let executable = CString::new(executable.as_os_str().as_bytes())
        .map_err(|_| AnchoredSocketError::Invalid)?;
    let argument = CString::new(BINDER_ARGUMENT).map_err(|_| AnchoredSocketError::Invalid)?;
    let argv = [
        executable.as_ptr().cast_mut(),
        argument.as_ptr().cast_mut(),
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
    binder.wait_until(BINDER_TIMEOUT)?;
    Ok((listener, identity))
}

fn spawn_connector(
    claim: &ClaimedSocketDirectory,
    endpoint: SocketBrokerEndpoint,
) -> Result<AnchoredVsockConnector, AnchoredSocketError> {
    prepare_connector_endpoint(&endpoint)?;
    let activate = SocketBrokerMessage::Activate {
        session: endpoint.session,
        sequence: 1,
        child: claim.child.clone(),
    };
    send_socket_broker_message(&endpoint.socket, &activate, None)
        .map_err(|_| AnchoredSocketError::Binder)?;
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
    Ok(AnchoredVsockConnector {
        socket: endpoint.socket,
        session: endpoint.session,
        launcher_pid: endpoint.launcher_pid,
        next_sequence: 2,
        healthy: true,
    })
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
    let staging = socket_staging_name(role).map_err(|_| AnchoredSocketError::Invalid)?;
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
    let expected = staging.to_bytes_with_nul();
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
    // SAFETY: `geteuid` has no pointer or ownership contract.
    if stat.st_uid != unsafe { libc::geteuid() } {
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
    let child = child_cstring(child)?;
    unlink_relative_if_socket_at(directory, &child, Some(identity))
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
) {
    let cleanup =
        unlink_socket_if_owned(claim.directory.anchor_fd(), &claim.child, record.identity());
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
    let current = match relative_socket_identity(directory, name) {
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
    use super::*;

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
}
