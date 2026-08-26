//! Narrow launcher-side transport for granted API publication and vsock connections.

use std::ffi::{CStr, CString};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use bangbang_session::macos::runtime::LauncherNamespace;
use bangbang_session::macos::socket_broker::{
    SocketBrokerError, SocketBrokerMessage, receive_socket_broker_message,
    send_socket_broker_message,
};
use bangbang_session::macos::{set_cloexec, verify_peer_pid};
use bangbang_session::{LauncherState, ObjectIdentity, ResourceRole, SessionId, SocketChild};

use crate::LauncherError;
use crate::grant_manifest::PreparedGrantBatch;

use super::api_listener::{ApiListenerPublication, publish_api_listener};

const CONNECT_INTERRUPTED_RETRY_LIMIT: usize = 8;
const API_PUBLICATION_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Session-bound state for the one dormant or active broker endpoint.
pub(crate) struct LauncherSocketBroker {
    session: SessionId,
    next_sequence: u64,
    state: BrokerState,
    api: ApiPublicationState,
}

impl std::fmt::Debug for LauncherSocketBroker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LauncherSocketBroker")
            .field("session", &"<redacted>")
            .field("next_sequence", &"<redacted>")
            .field("state", &self.state)
            .field("api", &self.api)
            .finish()
    }
}

#[derive(Debug)]
enum BrokerState {
    Dormant,
    Active(SocketChild),
    Complete,
}

enum ApiPublicationState {
    Available,
    Pending(PendingApiPublicationReply),
    Published,
    Failed,
}

impl std::fmt::Debug for ApiPublicationState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Available => "Available",
            Self::Pending(_) => "Pending(<redacted>)",
            Self::Published => "Published",
            Self::Failed => "Failed",
        })
    }
}

struct PendingApiPublicationReply {
    sequence: u64,
    deadline: Instant,
    outcome: ApiPublicationOutcome,
}

enum ApiPublicationOutcome {
    Published(ApiListenerPublication),
    Failed(io::ErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketBrokerDrainStatus {
    /// The authenticated broker remains open for another request.
    Pending,
    /// The authenticated terminal handshake completed and no authority remains.
    Complete,
}

/// Narrow launcher authority required only by ordinary API publication.
#[derive(Clone, Copy)]
pub(crate) struct ApiPublicationContext<'a> {
    namespace: Option<&'a LauncherNamespace>,
    grants_acknowledged: bool,
}

impl<'a> ApiPublicationContext<'a> {
    pub(crate) const fn new(
        namespace: Option<&'a LauncherNamespace>,
        grants_acknowledged: bool,
    ) -> Self {
        Self {
            namespace,
            grants_acknowledged,
        }
    }
}

impl LauncherSocketBroker {
    pub(crate) const fn new(session: SessionId) -> Self {
        Self {
            session,
            next_sequence: 1,
            state: BrokerState::Dormant,
            api: ApiPublicationState::Available,
        }
    }

    pub(crate) const fn api_readiness_is_valid(&self) -> bool {
        matches!(
            self.api,
            ApiPublicationState::Available | ApiPublicationState::Published
        )
    }

    pub(crate) const fn requires_write_event(&self) -> bool {
        matches!(self.api, ApiPublicationState::Pending(_))
    }

    pub(crate) const fn deadline(&self) -> Option<Instant> {
        match &self.api {
            ApiPublicationState::Pending(pending) => Some(pending.deadline),
            ApiPublicationState::Available
            | ApiPublicationState::Published
            | ApiPublicationState::Failed => None,
        }
    }

    pub(crate) fn has_timed_out(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }

    pub(crate) fn pump(&mut self, socket: &UnixDatagram) -> Result<(), LauncherError> {
        let ApiPublicationState::Pending(pending) = &self.api else {
            return Ok(());
        };
        let message = match &pending.outcome {
            ApiPublicationOutcome::Published(publication) => SocketBrokerMessage::ApiPublished {
                session: self.session,
                sequence: pending.sequence,
                identity: publication.identity(),
            },
            ApiPublicationOutcome::Failed(kind) => SocketBrokerMessage::ApiPublishFailed {
                session: self.session,
                sequence: pending.sequence,
                kind: *kind,
            },
        };
        let descriptor = match &pending.outcome {
            ApiPublicationOutcome::Published(publication) => publication.listener_fd(),
            ApiPublicationOutcome::Failed(_) => None,
        };
        match send_socket_broker_message(socket, &message, descriptor) {
            Ok(()) => {
                let sent = std::mem::replace(&mut self.api, ApiPublicationState::Failed);
                match sent {
                    ApiPublicationState::Pending(PendingApiPublicationReply {
                        outcome: ApiPublicationOutcome::Published(mut publication),
                        ..
                    }) => {
                        publication
                            .release_listener_alias()
                            .map_err(|_| LauncherError::SocketBroker)?;
                        self.api = ApiPublicationState::Published;
                    }
                    ApiPublicationState::Pending(PendingApiPublicationReply {
                        outcome: ApiPublicationOutcome::Failed(_),
                        ..
                    }) => {}
                    ApiPublicationState::Available
                    | ApiPublicationState::Published
                    | ApiPublicationState::Failed => return Err(LauncherError::SocketBroker),
                }
                Ok(())
            }
            Err(SocketBrokerError::Io(io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock)) => {
                Ok(())
            }
            Err(SocketBrokerError::Io(_) | SocketBrokerError::Invalid) => {
                Err(LauncherError::SocketBroker)
            }
        }
    }

    pub(crate) fn drain(
        &mut self,
        socket: &UnixDatagram,
        worker_pid: libc::pid_t,
        lifecycle_state: LauncherState,
        lifecycle_cancelled: bool,
        grants: &PreparedGrantBatch,
        api_context: ApiPublicationContext<'_>,
    ) -> Result<SocketBrokerDrainStatus, LauncherError> {
        let ApiPublicationContext {
            namespace,
            grants_acknowledged,
        } = api_context;
        self.pump(socket)?;
        if self.requires_write_event() {
            return Ok(SocketBrokerDrainStatus::Pending);
        }
        loop {
            let received = match receive_socket_broker_message(socket) {
                Ok(received) => received,
                Err(SocketBrokerError::Io(io::ErrorKind::WouldBlock)) => {
                    return Ok(SocketBrokerDrainStatus::Pending);
                }
                Err(_) => return Err(LauncherError::SocketBroker),
            };
            verify_peer_pid(socket.as_raw_fd(), worker_pid)
                .map_err(|_| LauncherError::SocketBroker)?;
            if received.descriptor.is_some()
                || received.message.session() != self.session
                || received.message.sequence() != self.next_sequence
            {
                return Err(LauncherError::SocketBroker);
            }
            let sequence = self.next_sequence;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .ok_or(LauncherError::SocketBroker)?;

            match received.message {
                SocketBrokerMessage::PublishApi { child, .. }
                    if matches!(self.api, ApiPublicationState::Available)
                        && matches!(self.state, BrokerState::Dormant)
                        && grants_acknowledged
                        && !lifecycle_cancelled
                        && matches!(
                            lifecycle_state,
                            LauncherState::AwaitStarting | LauncherState::Starting
                        ) =>
                {
                    let namespace = namespace
                        .ok_or(LauncherError::SocketBroker)?
                        .socket_namespace()
                        .map_err(|_| LauncherError::SocketBroker)?;
                    let anchor = grants
                        .socket_directory_anchor(ResourceRole::ApiSocketDirectory)
                        .ok_or(LauncherError::SocketBroker)?;
                    let outcome = match publish_api_listener(namespace, anchor, child) {
                        Ok(publication) => ApiPublicationOutcome::Published(publication),
                        Err(error) => ApiPublicationOutcome::Failed(error.category()),
                    };
                    self.api = ApiPublicationState::Pending(PendingApiPublicationReply {
                        sequence,
                        deadline: Instant::now()
                            .checked_add(API_PUBLICATION_REPLY_TIMEOUT)
                            .ok_or(LauncherError::SocketBroker)?,
                        outcome,
                    });
                    self.pump(socket)?;
                    if self.requires_write_event() {
                        return Ok(SocketBrokerDrainStatus::Pending);
                    }
                }
                SocketBrokerMessage::Activate { child, .. }
                    if matches!(self.state, BrokerState::Dormant)
                        && !matches!(self.api, ApiPublicationState::Failed)
                        && !lifecycle_cancelled
                        && matches!(
                            lifecycle_state,
                            LauncherState::AwaitStarting
                                | LauncherState::Starting
                                | LauncherState::Ready(_)
                        ) =>
                {
                    activate_directory(grants)?;
                    self.state = BrokerState::Active(child);
                    send(
                        socket,
                        &SocketBrokerMessage::Ready {
                            session: self.session,
                            sequence,
                        },
                        None,
                    )?;
                }
                SocketBrokerMessage::Connect { port, .. }
                    if matches!(self.state, BrokerState::Active(_)) && !lifecycle_cancelled =>
                {
                    let BrokerState::Active(child) = &self.state else {
                        return Err(LauncherError::SocketBroker);
                    };
                    match connect_relative_vsock_port(child, port) {
                        Ok(stream) => send(
                            socket,
                            &SocketBrokerMessage::Connected {
                                session: self.session,
                                sequence,
                                port,
                            },
                            Some(stream.as_raw_fd()),
                        )?,
                        Err(BrokerConnectError::Failure(kind)) => send(
                            socket,
                            &SocketBrokerMessage::Failed {
                                session: self.session,
                                sequence,
                                port,
                                kind,
                            },
                            None,
                        )?,
                        Err(BrokerConnectError::Invalid) => {
                            return Err(LauncherError::SocketBroker);
                        }
                    }
                }
                SocketBrokerMessage::Shutdown { .. }
                    if matches!(self.state, BrokerState::Active(_)) =>
                {
                    self.state = BrokerState::Complete;
                    send(
                        socket,
                        &SocketBrokerMessage::Complete {
                            session: self.session,
                            sequence,
                        },
                        None,
                    )?;
                    // The worker closes its datagram immediately after receiving
                    // Complete. Do not turn that valid terminal close into one
                    // more frame-decoding attempt in this drain.
                    return Ok(SocketBrokerDrainStatus::Complete);
                }
                _ => return Err(LauncherError::SocketBroker),
            }
        }
    }
}

fn send(
    socket: &UnixDatagram,
    message: &SocketBrokerMessage,
    descriptor: Option<RawFd>,
) -> Result<(), LauncherError> {
    send_socket_broker_message(socket, message, descriptor).map_err(|_| LauncherError::SocketBroker)
}

fn activate_directory(grants: &PreparedGrantBatch) -> Result<(), LauncherError> {
    let anchor = grants
        .socket_directory_anchor(ResourceRole::VsockSocketDirectory)
        .ok_or(LauncherError::SocketBroker)?;
    // SAFETY: The retained manifest descriptor is an identity-checked live directory anchor.
    if unsafe { libc::fchdir(anchor.descriptor()) } != 0 {
        return Err(LauncherError::SocketBroker);
    }
    if current_directory_identity().map_err(|_| LauncherError::SocketBroker)? != anchor.identity() {
        return Err(LauncherError::SocketBroker);
    }
    Ok(())
}

fn current_directory_identity() -> Result<ObjectIdentity, BrokerConnectError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The fixed relative name is live and output storage is writable.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            c".".as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful `fstatat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(BrokerConnectError::Invalid);
    }
    Ok(stat_identity(&stat))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrokerConnectError {
    Failure(io::ErrorKind),
    Invalid,
}

fn connect_relative_vsock_port(
    child: &SocketChild,
    port: u32,
) -> Result<std::os::unix::net::UnixStream, BrokerConnectError> {
    let mut name = Vec::with_capacity(child.as_bytes().len().saturating_add(12));
    name.extend_from_slice(child.as_bytes());
    name.push(b'_');
    name.extend_from_slice(port.to_string().as_bytes());
    let name = CString::new(name).map_err(|_| BrokerConnectError::Invalid)?;
    let before = relative_connect_target_identity(&name)?;
    let address = relative_unix_socket_address(&name)?;

    // SAFETY: A successful descriptor is immediately wrapped for unique ownership.
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: The fresh descriptor has not been transferred or wrapped elsewhere.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    set_cloexec(descriptor.as_raw_fd())
        .map_err(|error| BrokerConnectError::Failure(error.kind()))?;
    set_nonblocking(descriptor.as_raw_fd())?;

    let mut interrupted = 0_usize;
    loop {
        // SAFETY: Descriptor and fully initialized local address remain live.
        let result = unsafe {
            libc::connect(
                descriptor.as_raw_fd(),
                (&raw const address.0).cast(),
                address.1,
            )
        };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted
            && interrupted < CONNECT_INTERRUPTED_RETRY_LIMIT
        {
            interrupted += 1;
            continue;
        }
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(BrokerConnectError::Failure(error.kind()));
        }
        finish_nonblocking_connect(descriptor.as_raw_fd())?;
        break;
    }

    let after = relative_connect_target_identity(&name)?;
    if after != before {
        return Err(BrokerConnectError::Invalid);
    }
    validate_connected_peer(descriptor.as_raw_fd(), &name)?;
    Ok(std::os::unix::net::UnixStream::from(descriptor))
}

fn relative_connect_target_identity(name: &CStr) -> Result<ObjectIdentity, BrokerConnectError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The fixed cwd anchor and live bounded name are valid; output is writable.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful `fstatat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: `geteuid` has no pointer or ownership contract.
    let expected_uid = unsafe { libc::geteuid() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_nlink != 1
        || stat.st_uid != expected_uid
    {
        return Err(BrokerConnectError::Invalid);
    }
    Ok(stat_identity(&stat))
}

fn relative_unix_socket_address(
    name: &CStr,
) -> Result<(libc::sockaddr_un, libc::socklen_t), BrokerConnectError> {
    let bytes = name.to_bytes_with_nul();
    let address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    // SAFETY: Zeroed `sockaddr_un` is valid before its fields are initialized.
    let mut address = unsafe { address.assume_init() };
    if bytes.len() > address.sun_path.len() {
        return Err(BrokerConnectError::Invalid);
    }
    address.sun_family =
        libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| BrokerConnectError::Invalid)?;
    address.sun_len = u8::try_from(
        std::mem::offset_of!(libc::sockaddr_un, sun_path)
            .checked_add(bytes.len())
            .ok_or(BrokerConnectError::Invalid)?,
    )
    .map_err(|_| BrokerConnectError::Invalid)?;
    // SAFETY: The bounded source including NUL fits the destination path array.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    Ok((address, libc::socklen_t::from(address.sun_len)))
}

fn set_nonblocking(descriptor: RawFd) -> Result<(), BrokerConnectError> {
    // SAFETY: `F_GETFL` inspects one live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: `F_SETFL` changes only status flags on the same live descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    Ok(())
}

fn finish_nonblocking_connect(descriptor: RawFd) -> Result<(), BrokerConnectError> {
    let mut poll_fd = libc::pollfd {
        fd: descriptor,
        events: libc::POLLOUT,
        revents: 0,
    };
    let mut interrupted = 0_usize;
    let ready = loop {
        // SAFETY: One initialized poll entry is writable for an immediate observation.
        let result = unsafe { libc::poll(&raw mut poll_fd, 1, 0) };
        if result >= 0 {
            break result;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted
            || interrupted >= CONNECT_INTERRUPTED_RETRY_LIMIT
        {
            return Err(BrokerConnectError::Failure(error.kind()));
        }
        interrupted += 1;
    };
    if ready == 0 {
        return Err(BrokerConnectError::Failure(io::ErrorKind::WouldBlock));
    }
    let socket_error = socket_int_option(descriptor, libc::SO_ERROR)?;
    if socket_error == 0 {
        Ok(())
    } else {
        Err(BrokerConnectError::Failure(
            io::Error::from_raw_os_error(socket_error).kind(),
        ))
    }
}

fn validate_connected_peer(descriptor: RawFd, expected: &CStr) -> Result<(), BrokerConnectError> {
    if socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_int_option(descriptor, libc::SO_ERROR)? != 0
    {
        return Err(BrokerConnectError::Invalid);
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>())
        .map_err(|_| BrokerConnectError::Invalid)?;
    // SAFETY: Address storage and length are writable for this live connected socket.
    if unsafe { libc::getpeername(descriptor, address.as_mut_ptr().cast(), &raw mut length) } != 0 {
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
    }
    // SAFETY: Successful `getpeername` initialized the returned prefix.
    let address = unsafe { address.assume_init() };
    if address.sun_family
        != libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| BrokerConnectError::Invalid)?
    {
        return Err(BrokerConnectError::Invalid);
    }
    let returned = usize::try_from(length).map_err(|_| BrokerConnectError::Invalid)?;
    let path_length = returned
        .checked_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
        .ok_or(BrokerConnectError::Invalid)?;
    if path_length > address.sun_path.len() {
        return Err(BrokerConnectError::Invalid);
    }
    // SAFETY: Kernel-returned length bounds this read within `address`.
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
        Err(BrokerConnectError::Invalid)
    }
}

fn socket_int_option(descriptor: RawFd, option: libc::c_int) -> Result<i32, BrokerConnectError> {
    let mut value = 0_i32;
    let mut length =
        libc::socklen_t::try_from(size_of::<i32>()).map_err(|_| BrokerConnectError::Invalid)?;
    // SAFETY: Option value and length are writable for this live socket descriptor.
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
        return Err(BrokerConnectError::Failure(
            io::Error::last_os_error().kind(),
        ));
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
    use std::fs::File;

    use super::*;
    use crate::grant_manifest::LaunchInput;

    fn session() -> SessionId {
        SessionId::from_bytes([9; 32])
    }

    fn empty_grants() -> PreparedGrantBatch {
        LaunchInput::parse(Vec::new())
            .expect("empty launch input should parse")
            .prepare()
            .expect("empty grant batch should prepare")
            .1
    }

    fn rejected_message(
        broker_session: SessionId,
        message: SocketBrokerMessage,
        descriptor: Option<RawFd>,
        lifecycle_state: LauncherState,
        cancelled: bool,
    ) -> Result<SocketBrokerDrainStatus, LauncherError> {
        let (launcher, worker) = UnixDatagram::pair().expect("broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher endpoint should become nonblocking");
        send_socket_broker_message(&worker, &message, descriptor)
            .expect("test broker message should send");
        let mut broker = LauncherSocketBroker::new(broker_session);
        // SAFETY: The test process is both authenticated socketpair peers.
        let pid = unsafe { libc::getpid() };
        broker.drain(
            &launcher,
            pid,
            lifecycle_state,
            cancelled,
            &empty_grants(),
            ApiPublicationContext::new(None, true),
        )
    }

    fn rejected_api_request(
        lifecycle_state: LauncherState,
        cancelled: bool,
        acknowledged: bool,
    ) -> Result<SocketBrokerDrainStatus, LauncherError> {
        let (launcher, worker) = UnixDatagram::pair().expect("broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher endpoint should become nonblocking");
        send_socket_broker_message(
            &worker,
            &SocketBrokerMessage::PublishApi {
                session: session(),
                sequence: 1,
                child: SocketChild::parse("api.sock").expect("child should parse"),
            },
            None,
        )
        .expect("API request should send");
        let mut broker = LauncherSocketBroker::new(session());
        // SAFETY: The test process owns both authenticated peers.
        let pid = unsafe { libc::getpid() };
        broker.drain(
            &launcher,
            pid,
            lifecycle_state,
            cancelled,
            &empty_grants(),
            ApiPublicationContext::new(None, acknowledged),
        )
    }

    #[test]
    fn broker_state_and_errors_are_redacted() {
        let mut broker = LauncherSocketBroker::new(session());
        broker.next_sequence = 52;
        broker.state = BrokerState::Active(
            SocketChild::parse("sensitive-vsock.sock").expect("child should parse"),
        );
        let debug = format!("{broker:?}");
        assert!(!debug.contains("0909"));
        assert!(!debug.contains("52"));
        assert!(!debug.contains("sensitive-vsock.sock"));
        assert_eq!(
            LauncherError::SocketBroker.to_string(),
            "private socket broker failed"
        );
    }

    #[test]
    fn broker_rejects_wrong_phase_session_sequence_operation_and_rights() {
        let child = SocketChild::parse("vm.vsock").expect("child should parse");
        let activate = |message_session, sequence| SocketBrokerMessage::Activate {
            session: message_session,
            sequence,
            child: child.clone(),
        };
        for result in [
            rejected_message(
                session(),
                activate(session(), 1),
                None,
                LauncherState::ReadyToProceed,
                false,
            ),
            rejected_message(
                session(),
                activate(SessionId::from_bytes([8; 32]), 1),
                None,
                LauncherState::AwaitStarting,
                false,
            ),
            rejected_message(
                session(),
                activate(session(), 2),
                None,
                LauncherState::AwaitStarting,
                false,
            ),
            rejected_message(
                session(),
                SocketBrokerMessage::Connect {
                    session: session(),
                    sequence: 1,
                    port: 52,
                },
                None,
                LauncherState::Starting,
                false,
            ),
            rejected_message(
                session(),
                activate(session(), 1),
                None,
                LauncherState::AwaitStarting,
                true,
            ),
        ] {
            assert_eq!(result, Err(LauncherError::SocketBroker));
        }

        let descriptor = File::open("/dev/null").expect("fixture descriptor should open");
        assert_eq!(
            rejected_message(
                session(),
                SocketBrokerMessage::Connected {
                    session: session(),
                    sequence: 1,
                    port: 52,
                },
                Some(descriptor.as_raw_fd()),
                LauncherState::Starting,
                false,
            ),
            Err(LauncherError::SocketBroker)
        );
    }

    #[test]
    fn api_publication_requires_start_authority_ack_namespace_and_anchor() {
        for result in [
            rejected_api_request(LauncherState::ReadyToProceed, false, true),
            rejected_api_request(LauncherState::AwaitStarting, false, false),
            rejected_api_request(LauncherState::AwaitStarting, true, true),
            rejected_api_request(LauncherState::AwaitStarting, false, true),
        ] {
            assert_eq!(result, Err(LauncherError::SocketBroker));
        }
    }

    #[test]
    fn pending_api_reply_survives_backpressure_and_keeps_one_deadline() {
        let (launcher, worker) = UnixDatagram::pair().expect("broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher endpoint should become nonblocking");
        worker
            .set_nonblocking(true)
            .expect("worker endpoint should become nonblocking");
        let filler = SocketBrokerMessage::Ready {
            session: session(),
            sequence: 1,
        };
        loop {
            match send_socket_broker_message(&launcher, &filler, None) {
                Ok(()) | Err(SocketBrokerError::Io(io::ErrorKind::Interrupted)) => {}
                Err(SocketBrokerError::Io(io::ErrorKind::WouldBlock)) => break,
                Err(error) => panic!("filler send should only backpressure: {error:?}"),
            }
        }

        let deadline = Instant::now()
            .checked_add(API_PUBLICATION_REPLY_TIMEOUT)
            .expect("deadline should fit");
        let mut broker = LauncherSocketBroker::new(session());
        assert!(broker.api_readiness_is_valid());
        broker.api = ApiPublicationState::Pending(PendingApiPublicationReply {
            sequence: 1,
            deadline,
            outcome: ApiPublicationOutcome::Failed(io::ErrorKind::NotFound),
        });
        broker
            .pump(&launcher)
            .expect("backpressure should retain the reply");
        assert!(broker.requires_write_event());
        assert!(!broker.api_readiness_is_valid());
        assert_eq!(broker.deadline(), Some(deadline));
        assert!(!broker.has_timed_out(Instant::now()));

        loop {
            match receive_socket_broker_message(&worker) {
                Ok(received) => assert_eq!(received.message, filler),
                Err(SocketBrokerError::Io(io::ErrorKind::Interrupted)) => {}
                Err(SocketBrokerError::Io(io::ErrorKind::WouldBlock)) => break,
                Err(error) => panic!("filler drain should remain valid: {error:?}"),
            }
        }
        broker
            .pump(&launcher)
            .expect("retained reply should send once writable");
        assert!(!broker.requires_write_event());
        assert_eq!(broker.deadline(), None);
        assert!(matches!(broker.api, ApiPublicationState::Failed));
        assert!(!broker.api_readiness_is_valid());
        assert!(matches!(
            receive_socket_broker_message(&worker),
            Ok(bangbang_session::macos::socket_broker::ReceivedSocketBrokerMessage {
                message: SocketBrokerMessage::ApiPublishFailed {
                    session: response_session,
                    sequence: 1,
                    kind: io::ErrorKind::NotFound,
                },
                descriptor: None,
            }) if response_session == session()
        ));

        loop {
            match send_socket_broker_message(&launcher, &filler, None) {
                Ok(()) | Err(SocketBrokerError::Io(io::ErrorKind::Interrupted)) => {}
                Err(SocketBrokerError::Io(io::ErrorKind::WouldBlock)) => break,
                Err(error) => panic!("second filler send should only backpressure: {error:?}"),
            }
        }
        let (stream, stream_peer) =
            std::os::unix::net::UnixStream::pair().expect("test descriptor pair should open");
        let identity = ObjectIdentity {
            device: 17,
            inode: 23,
        };
        let publication = ApiListenerPublication::from_test_descriptor(stream.into(), identity);
        let retained_descriptor = publication
            .listener_fd()
            .expect("test publication should retain its alias");
        broker.api = ApiPublicationState::Pending(PendingApiPublicationReply {
            sequence: 2,
            deadline,
            outcome: ApiPublicationOutcome::Published(publication),
        });
        broker
            .pump(&launcher)
            .expect("published reply backpressure should retain the alias");
        assert!(matches!(
            &broker.api,
            ApiPublicationState::Pending(PendingApiPublicationReply {
                outcome: ApiPublicationOutcome::Published(publication),
                ..
            }) if publication.listener_fd() == Some(retained_descriptor)
        ));
        loop {
            match receive_socket_broker_message(&worker) {
                Ok(received) => assert_eq!(received.message, filler),
                Err(SocketBrokerError::Io(io::ErrorKind::Interrupted)) => {}
                Err(SocketBrokerError::Io(io::ErrorKind::WouldBlock)) => break,
                Err(error) => panic!("second filler drain should remain valid: {error:?}"),
            }
        }
        broker
            .pump(&launcher)
            .expect("retained published reply should send once writable");
        assert!(broker.api_readiness_is_valid());
        assert!(matches!(broker.api, ApiPublicationState::Published));
        assert!(matches!(
            receive_socket_broker_message(&worker),
            Ok(bangbang_session::macos::socket_broker::ReceivedSocketBrokerMessage {
                message: SocketBrokerMessage::ApiPublished {
                    session: response_session,
                    sequence: 2,
                    identity: response_identity,
                },
                descriptor: Some(_),
            }) if response_session == session() && response_identity == identity
        ));
        drop(stream_peer);

        broker.api = ApiPublicationState::Pending(PendingApiPublicationReply {
            sequence: 3,
            deadline: Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("past deadline should fit"),
            outcome: ApiPublicationOutcome::Failed(io::ErrorKind::Other),
        });
        assert!(broker.has_timed_out(Instant::now()));
        assert!(!format!("{broker:?}").contains("NotFound"));

        broker.api = ApiPublicationState::Published;
        assert!(broker.api_readiness_is_valid());
    }

    #[test]
    fn broker_rejects_another_activation_after_completion() {
        let (launcher, worker) = UnixDatagram::pair().expect("broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher endpoint should become nonblocking");
        let mut broker = LauncherSocketBroker::new(session());
        broker.state = BrokerState::Complete;
        broker.next_sequence = 3;
        send_socket_broker_message(
            &worker,
            &SocketBrokerMessage::Activate {
                session: session(),
                sequence: 3,
                child: SocketChild::parse("second.sock").expect("child should parse"),
            },
            None,
        )
        .expect("second activation should send");
        // SAFETY: The test process owns both authenticated socketpair peers.
        let pid = unsafe { libc::getpid() };

        assert_eq!(
            broker.drain(
                &launcher,
                pid,
                LauncherState::ReadyToProceed,
                false,
                &empty_grants(),
                ApiPublicationContext::new(None, true),
            ),
            Err(LauncherError::SocketBroker)
        );
    }

    #[test]
    fn broker_shutdown_reports_a_terminal_drain() {
        let (launcher, worker) = UnixDatagram::pair().expect("broker pair should open");
        launcher
            .set_nonblocking(true)
            .expect("launcher endpoint should become nonblocking");
        let mut broker = LauncherSocketBroker::new(session());
        broker.state =
            BrokerState::Active(SocketChild::parse("vm.vsock").expect("broker child should parse"));
        broker.next_sequence = 2;
        send_socket_broker_message(
            &worker,
            &SocketBrokerMessage::Shutdown {
                session: session(),
                sequence: 2,
            },
            None,
        )
        .expect("shutdown should send");
        // SAFETY: The test process owns both authenticated socketpair peers.
        let pid = unsafe { libc::getpid() };

        assert_eq!(
            broker.drain(
                &launcher,
                pid,
                LauncherState::ReadyToProceed,
                true,
                &empty_grants(),
                ApiPublicationContext::new(None, true),
            ),
            Ok(SocketBrokerDrainStatus::Complete)
        );
        assert!(matches!(
            receive_socket_broker_message(&worker),
            Ok(bangbang_session::macos::socket_broker::ReceivedSocketBrokerMessage {
                message: SocketBrokerMessage::Complete {
                    session: response_session,
                    sequence: 2,
                },
                descriptor: None,
            }) if response_session == session()
        ));
    }
}
