use std::ffi::CString;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr;
use std::time::{Duration, Instant};

use bangbang_session::macos::grant_transport::{GrantTransportError, send_grant};
use bangbang_session::macos::runtime::{
    LauncherNamespace, NamespaceIdentity, RuntimeError, SnapshotStagingOwnershipRecord,
    SocketOwnershipRecord,
};
use bangbang_session::{
    CancelSignal, Frame, FrameDecoder, LauncherLifecycle, Message, TerminalCategory, encode_frame,
};
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::{SigId, low_level};

use super::socket_broker::LauncherSocketBroker;
use super::spawn::OwnedWorker;
use crate::LauncherError;
use crate::grant_manifest::{OutboundGrant, PreparedGrantBatch};

const CANCELLATION_GRACE: Duration = Duration::from_secs(5);
const SESSION_EXIT_GRACE: Duration = Duration::from_secs(5);
const BOOTSTRAP_HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const GRANT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct SignalWakeup {
    signal: i32,
    reader: UnixStream,
    signal_id: SigId,
}

impl SignalWakeup {
    fn install(signal: i32) -> Result<Self, LauncherError> {
        let (reader, writer) =
            UnixStream::pair().map_err(|err| LauncherError::SignalSetup(err.kind()))?;
        reader
            .set_nonblocking(true)
            .map_err(|err| LauncherError::SignalSetup(err.kind()))?;
        let wakeup_fd = writer
            .try_clone()
            .map_err(|err| LauncherError::SignalSetup(err.kind()))?
            .into_raw_fd();
        let signal_id = match low_level::pipe::register_raw(signal, wakeup_fd) {
            Ok(signal_id) => signal_id,
            Err(err) => {
                // SAFETY: Registration failed, so ownership of the duplicated
                // raw descriptor was not transferred to a signal action.
                let _ = unsafe { libc::close(wakeup_fd) };
                return Err(LauncherError::SignalSetup(err.kind()));
            }
        };
        Ok(Self {
            signal,
            reader,
            signal_id,
        })
    }

    fn drain(&mut self) -> Result<bool, LauncherError> {
        let mut drained = false;
        let mut buffer = [0_u8; 64];
        loop {
            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    return Err(LauncherError::SignalSetup(io::ErrorKind::UnexpectedEof));
                }
                Ok(_) => drained = true,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(drained),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(LauncherError::SignalSetup(err.kind())),
            }
        }
    }
}

impl Drop for SignalWakeup {
    fn drop(&mut self) {
        low_level::unregister(self.signal_id);
    }
}

#[derive(Debug)]
pub(crate) struct SignalWakeups {
    signals: [SignalWakeup; 2],
}

impl SignalWakeups {
    pub(crate) fn install() -> Result<Self, LauncherError> {
        let sigint = SignalWakeup::install(SIGINT)?;
        let sigterm = SignalWakeup::install(SIGTERM)?;
        Ok(Self {
            signals: [sigint, sigterm],
        })
    }
}

pub(crate) fn write_frame(stream: &mut UnixStream, frame: Frame) -> Result<(), LauncherError> {
    let encoded = encode_frame(frame).map_err(|_| LauncherError::SessionProtocol)?;
    stream
        .write_all(&encoded)
        .map_err(|_| LauncherError::SessionProtocol)
}

pub(crate) fn read_bootstrap_hello(
    stream: &mut UnixStream,
    lifecycle: &mut LauncherLifecycle,
) -> Result<(), LauncherError> {
    let deadline = Instant::now()
        .checked_add(BOOTSTRAP_HELLO_TIMEOUT)
        .ok_or(LauncherError::SessionProtocol)?;
    let result = (|| {
        let mut decoder = FrameDecoder::default();
        let mut buffer = [0_u8; 4096];
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(LauncherError::SessionProtocol)?;
            if let Some(frame) = decoder
                .next_frame()
                .map_err(|_| LauncherError::SessionProtocol)?
            {
                if lifecycle
                    .receive(frame)
                    .map_err(|_| LauncherError::SessionProtocol)?
                    != Message::Hello
                    || decoder.finish().is_err()
                {
                    return Err(LauncherError::SessionProtocol);
                }
                return Ok(());
            }
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|error| LauncherError::SessionSetup(error.kind()))?;
            let length = match stream.read(&mut buffer) {
                Ok(length) => length,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(LauncherError::SessionProtocol),
            };
            if length == 0 {
                return Err(LauncherError::SessionProtocol);
            }
            decoder
                .push(buffer.get(..length).ok_or(LauncherError::SessionProtocol)?)
                .map_err(|_| LauncherError::SessionProtocol)?;
        }
    })();
    let clear_result = stream
        .set_read_timeout(None)
        .map_err(|error| LauncherError::SessionSetup(error.kind()));
    result.and(clear_result)
}

pub(crate) fn wait_session(
    worker: &mut OwnedWorker,
    session: &mut UnixStream,
    grant_socket: &mut UnixDatagram,
    socket_broker: &mut UnixDatagram,
    mut lifecycle: LauncherLifecycle,
    mut wakeups: SignalWakeups,
    grants: &PreparedGrantBatch,
) -> Result<ExitStatus, LauncherError> {
    let session_id = lifecycle.session();
    let mut observation = SessionObservation::default();
    let auxiliary = AuxiliaryChannels {
        grant_socket,
        socket_broker,
    };
    let result = wait_session_inner(
        worker,
        session,
        auxiliary,
        &mut lifecycle,
        &mut wakeups,
        &mut observation,
        grants,
    );
    if result.is_err() {
        let _ = session.shutdown(std::net::Shutdown::Both);
        shutdown_grants(grant_socket);
        let _ = socket_broker.shutdown(std::net::Shutdown::Both);
        worker.terminate_and_reap();
    }

    let cleanup = if let Some(namespace) = observation.namespace.as_mut() {
        cleanup_namespace_after_worker(namespace, grants)
    } else {
        LauncherNamespace::recover_after_worker_exit(session_id).and_then(|recovered| {
            recovered.map_or(Ok(()), |mut namespace| {
                cleanup_namespace_after_worker(&mut namespace, grants)
            })
        })
    }
    .map_err(|_| LauncherError::RuntimeNamespace);

    match result {
        Ok(status) => {
            cleanup?;
            validate_terminal(status, observation.terminal)?;
            Ok(status)
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
}

fn cleanup_namespace_after_worker(
    namespace: &mut LauncherNamespace,
    grants: &PreparedGrantBatch,
) -> Result<(), RuntimeError> {
    for record in namespace.socket_ownership_records()? {
        let anchor = grants
            .socket_directory_anchor(record.role())
            .ok_or(RuntimeError::InvalidEntry)?;
        unlink_owned_socket(anchor.descriptor(), &record)?;
        namespace.unlink_staged_socket(&record)?;
        namespace.clear_socket_record(&record)?;
    }
    for record in namespace.snapshot_staging_records()? {
        let anchor = grants
            .snapshot_directory_anchor(record.directory_identity())
            .ok_or(RuntimeError::InvalidEntry)?;
        debug_assert_eq!(anchor.identity(), record.directory_identity());
        unlink_owned_snapshot_staging(anchor.descriptor(), &record)?;
        namespace.clear_snapshot_staging_record(&record)?;
    }
    namespace.cleanup()
}

fn unlink_owned_socket(
    directory: RawFd,
    record: &SocketOwnershipRecord,
) -> Result<(), RuntimeError> {
    let child = CString::new(record.child().as_bytes()).map_err(|_| RuntimeError::InvalidEntry)?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The retained anchor and bounded C string remain live; stat is writable.
    if unsafe {
        libc::fstatat(
            directory,
            child.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(RuntimeError::Filesystem(error.kind()))
        };
    }
    // SAFETY: Successful fstatat initialized the complete result.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: `geteuid` has no pointer or ownership contract.
    let uid = unsafe { libc::geteuid() };
    let identity = bangbang_session::ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_uid != uid
        || stat.st_nlink != 1
        || identity != record.identity()
    {
        return Ok(());
    }
    // SAFETY: The retained directory and bounded child name the identity-checked socket.
    if unsafe { libc::unlinkat(directory, child.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(RuntimeError::Filesystem(error.kind()))
    }
}

fn unlink_owned_snapshot_staging(
    directory: RawFd,
    record: &SnapshotStagingOwnershipRecord,
) -> Result<(), RuntimeError> {
    let child = CString::new(record.name().as_bytes()).map_err(|_| RuntimeError::InvalidEntry)?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The retained anchor and bounded C string remain live; stat is writable.
    if unsafe {
        libc::fstatat(
            directory,
            child.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(RuntimeError::Filesystem(error.kind()))
        };
    }
    // SAFETY: Successful fstatat initialized the complete result.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: `geteuid` has no pointer or ownership contract.
    let uid = unsafe { libc::geteuid() };
    let identity = bangbang_session::ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_uid != uid
        || stat.st_nlink != 1
        || identity != record.file_identity()
    {
        return Ok(());
    }
    // SAFETY: The retained directory and bounded child name the identity-checked regular file.
    if unsafe { libc::unlinkat(directory, child.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(RuntimeError::Filesystem(error.kind()))
    }
}

#[derive(Debug, Default)]
struct SessionObservation {
    namespace: Option<LauncherNamespace>,
    terminal: Option<(TerminalCategory, u8)>,
}

struct AuxiliaryChannels<'a> {
    grant_socket: &'a mut UnixDatagram,
    socket_broker: &'a mut UnixDatagram,
}

fn wait_session_inner(
    worker: &mut OwnedWorker,
    session: &mut UnixStream,
    auxiliary: AuxiliaryChannels<'_>,
    lifecycle: &mut LauncherLifecycle,
    wakeups: &mut SignalWakeups,
    observation: &mut SessionObservation,
    grants: &PreparedGrantBatch,
) -> Result<ExitStatus, LauncherError> {
    let AuxiliaryChannels {
        grant_socket,
        socket_broker,
    } = auxiliary;
    session
        .set_nonblocking(true)
        .map_err(|err| LauncherError::SessionSetup(err.kind()))?;
    grant_socket
        .set_nonblocking(true)
        .map_err(|err| LauncherError::SessionSetup(err.kind()))?;
    socket_broker
        .set_nonblocking(true)
        .map_err(|err| LauncherError::SessionSetup(err.kind()))?;
    let kqueue = create_kqueue()?;
    let child_pid = usize::try_from(worker.pid())
        .map_err(|_| LauncherError::WorkerWait(io::ErrorKind::InvalidInput))?;
    let session_fd = usize::try_from(session.as_raw_fd())
        .map_err(|_| LauncherError::SessionSetup(io::ErrorKind::InvalidInput))?;
    let grant_fd = usize::try_from(grant_socket.as_raw_fd())
        .map_err(|_| LauncherError::SessionSetup(io::ErrorKind::InvalidInput))?;
    let broker_fd = usize::try_from(socket_broker.as_raw_fd())
        .map_err(|_| LauncherError::SessionSetup(io::ErrorKind::InvalidInput))?;
    let changes = [
        event(
            usize::try_from(wakeups.signals[0].reader.as_raw_fd())
                .map_err(|_| LauncherError::SignalSetup(io::ErrorKind::InvalidInput))?,
            libc::EVFILT_READ,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            0,
        ),
        event(
            usize::try_from(wakeups.signals[1].reader.as_raw_fd())
                .map_err(|_| LauncherError::SignalSetup(io::ErrorKind::InvalidInput))?,
            libc::EVFILT_READ,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            0,
        ),
        event(
            session_fd,
            libc::EVFILT_READ,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            0,
        ),
        event(
            child_pid,
            libc::EVFILT_PROC,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            libc::NOTE_EXIT,
        ),
        event(
            grant_fd,
            libc::EVFILT_WRITE,
            libc::EV_ADD | libc::EV_DISABLE,
            0,
        ),
        event(
            broker_fd,
            libc::EVFILT_READ,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            0,
        ),
    ];
    register_events(kqueue.as_raw_fd(), &changes)?;

    let mut decoder = FrameDecoder::default();
    let mut cancellation_deadline: Option<Instant> = None;
    let mut exit_deadline: Option<Instant> = None;
    let mut session_closed = false;
    let mut grant_send = GrantSendState::new(grants, lifecycle.session());
    let mut broker = LauncherSocketBroker::new(lifecycle.session());
    loop {
        let deadline = [cancellation_deadline, exit_deadline, grant_send.deadline()]
            .into_iter()
            .flatten()
            .min();
        let timeout = timeout_until(deadline);
        let events = wait_events(kqueue.as_raw_fd(), timeout)?;
        let child_exited = events.iter().any(|queued| {
            queued.filter == libc::EVFILT_PROC
                && queued.ident == child_pid
                && queued.fflags & libc::NOTE_EXIT != 0
        });
        // Protocol state and EOF take precedence over same-batch signals. This
        // preserves an already-observed worker exit and avoids writing Cancel to
        // a channel that the worker has just closed.
        for queued in &events {
            if !session_closed && queued.filter == libc::EVFILT_READ && queued.ident == session_fd {
                session_closed = drain_session(
                    session,
                    grant_socket,
                    &mut decoder,
                    lifecycle,
                    observation,
                    &mut grant_send,
                )?;
                if session_closed {
                    exit_deadline.get_or_insert(Instant::now() + SESSION_EXIT_GRACE);
                    register_events(
                        kqueue.as_raw_fd(),
                        &[event(session_fd, libc::EVFILT_READ, libc::EV_DELETE, 0)],
                    )?;
                }
            }
        }
        if observation.terminal.is_some() {
            exit_deadline.get_or_insert(Instant::now() + SESSION_EXIT_GRACE);
        }

        if !child_exited
            && events
                .iter()
                .any(|queued| queued.filter == libc::EVFILT_READ && queued.ident == broker_fd)
        {
            broker.drain(
                socket_broker,
                worker.pid(),
                lifecycle.state(),
                lifecycle.is_cancelled(),
                grants,
            )?;
        }

        for queued in &events {
            for signal in &mut wakeups.signals {
                let signal_fd = usize::try_from(signal.reader.as_raw_fd())
                    .map_err(|_| LauncherError::SignalSetup(io::ErrorKind::InvalidInput))?;
                if queued.filter == libc::EVFILT_READ
                    && queued.ident == signal_fd
                    && signal.drain()?
                    && !child_exited
                    && !session_closed
                    && observation.terminal.is_none()
                    && !lifecycle.is_cancelled()
                {
                    let signal = match signal.signal {
                        SIGINT => CancelSignal::Interrupt,
                        SIGTERM => CancelSignal::Terminate,
                        _ => return Err(LauncherError::SessionProtocol),
                    };
                    let frame = lifecycle
                        .cancel(signal)
                        .map_err(|_| LauncherError::SessionProtocol)?;
                    write_frame(session, frame)?;
                    grant_send.cancel();
                    shutdown_grants(grant_socket);
                    cancellation_deadline = Some(Instant::now() + CANCELLATION_GRACE);
                }
            }
        }

        sync_grant_write_event(kqueue.as_raw_fd(), grant_fd, &mut grant_send)?;
        if !lifecycle.is_cancelled()
            && events.iter().any(|queued| {
                queued.filter == libc::EVFILT_WRITE
                    && queued.ident == grant_fd
                    && queued.flags & libc::EV_ERROR != 0
            })
        {
            return Err(LauncherError::GrantProtocol);
        }
        if events.iter().any(|queued| {
            queued.filter == libc::EVFILT_WRITE
                && queued.ident == grant_fd
                && queued.flags & libc::EV_ERROR == 0
        }) && !lifecycle.is_cancelled()
            && !child_exited
        {
            grant_send.pump(grant_socket)?;
            sync_grant_write_event(kqueue.as_raw_fd(), grant_fd, &mut grant_send)?;
        }

        if child_exited {
            // Drain all bytes made readable with this exit before reaping, so
            // terminal state cannot lose a same-batch race.
            if !session_closed {
                let _ = drain_session(
                    session,
                    grant_socket,
                    &mut decoder,
                    lifecycle,
                    observation,
                    &mut grant_send,
                )?;
            }
            break;
        }
        if exit_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(LauncherError::SessionProtocol);
        }
        if cancellation_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            worker
                .signal(libc::SIGKILL)
                .map_err(LauncherError::SignalForward)?;
            cancellation_deadline = None;
        }
        if grant_send.has_timed_out(Instant::now()) {
            return Err(LauncherError::GrantProtocol);
        }
    }

    worker.wait()
}

fn timeout_until(deadline: Option<Instant>) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
}

fn drain_session(
    session: &mut UnixStream,
    grant_socket: &mut UnixDatagram,
    decoder: &mut FrameDecoder,
    lifecycle: &mut LauncherLifecycle,
    observation: &mut SessionObservation,
    grant_send: &mut GrantSendState,
) -> Result<bool, LauncherError> {
    let mut buffer = [0_u8; 4096];
    loop {
        match session.read(&mut buffer) {
            Ok(0) => {
                decoder
                    .finish()
                    .map_err(|_| LauncherError::SessionProtocol)?;
                if grant_send.requires_ack() && !lifecycle.is_cancelled() {
                    return Err(LauncherError::GrantProtocol);
                }
                return Ok(true);
            }
            Ok(length) => {
                decoder
                    .push(buffer.get(..length).ok_or(LauncherError::SessionProtocol)?)
                    .map_err(|_| LauncherError::SessionProtocol)?;
                while let Some(frame) = decoder
                    .next_frame()
                    .map_err(|_| LauncherError::SessionProtocol)?
                {
                    if matches!(frame.message, Message::GrantsAccepted { .. })
                        && !grant_send.write_complete()
                    {
                        return Err(LauncherError::GrantProtocol);
                    }
                    let message = lifecycle
                        .receive(frame)
                        .map_err(|_| LauncherError::SessionProtocol)?;
                    match message {
                        Message::Prepared { device, inode } => {
                            if observation.namespace.is_some() {
                                return Err(LauncherError::SessionProtocol);
                            }
                            let validated = LauncherNamespace::validate(
                                lifecycle.session(),
                                NamespaceIdentity { device, inode },
                            )
                            .map_err(|_| LauncherError::RuntimeNamespace)?;
                            observation.namespace = Some(validated);
                            if !lifecycle.is_cancelled() {
                                lifecycle
                                    .expect_grants(
                                        grant_send.batch(),
                                        grant_send.grant_count(),
                                        grant_send.final_sequence(),
                                    )
                                    .map_err(|_| LauncherError::SessionProtocol)?;
                                grant_send.begin()?;
                            }
                        }
                        Message::GrantsAccepted { .. } => {
                            if !lifecycle.is_cancelled() {
                                grant_send.acknowledge()?;
                                let proceed = lifecycle
                                    .proceed()
                                    .map_err(|_| LauncherError::SessionProtocol)?;
                                write_frame(session, proceed)?;
                                shutdown_grants(grant_socket);
                            }
                        }
                        Message::Starting | Message::Ready(_) => {}
                        Message::Terminal {
                            category,
                            exit_code,
                        } => {
                            if observation
                                .terminal
                                .replace((category, exit_code))
                                .is_some()
                            {
                                return Err(LauncherError::SessionProtocol);
                            }
                        }
                        Message::Hello | Message::Start | Message::Proceed | Message::Cancel(_) => {
                            return Err(LauncherError::SessionProtocol);
                        }
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(LauncherError::SessionProtocol),
        }
    }
}

struct GrantSendState {
    batch: bangbang_session::BatchId,
    grant_count: u16,
    final_sequence: u64,
    outbound: Vec<OutboundGrant>,
    next: usize,
    deadline: Option<Instant>,
    event_enabled: bool,
    started: bool,
    acknowledged: bool,
    cancelled: bool,
}

impl std::fmt::Debug for GrantSendState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrantSendState")
            .field("batch", &self.batch)
            .field("progress", &"<redacted>")
            .field("started", &self.started)
            .field("acknowledged", &self.acknowledged)
            .field("cancelled", &self.cancelled)
            .finish()
    }
}

impl GrantSendState {
    fn new(grants: &PreparedGrantBatch, session: bangbang_session::SessionId) -> Self {
        Self {
            batch: grants.batch(),
            grant_count: grants.grant_count(),
            final_sequence: grants.final_sequence(),
            outbound: grants.outbound(session),
            next: 0,
            deadline: None,
            event_enabled: false,
            started: false,
            acknowledged: false,
            cancelled: false,
        }
    }

    const fn batch(&self) -> bangbang_session::BatchId {
        self.batch
    }

    const fn grant_count(&self) -> u16 {
        self.grant_count
    }

    const fn final_sequence(&self) -> u64 {
        self.final_sequence
    }

    fn begin(&mut self) -> Result<(), LauncherError> {
        if self.started || self.acknowledged || self.cancelled || self.outbound.is_empty() {
            return Err(LauncherError::GrantProtocol);
        }
        self.started = true;
        self.deadline = Some(
            Instant::now()
                .checked_add(GRANT_TIMEOUT)
                .ok_or(LauncherError::GrantProtocol)?,
        );
        Ok(())
    }

    fn pump(&mut self, socket: &UnixDatagram) -> Result<(), LauncherError> {
        if !self.started || self.acknowledged || self.cancelled {
            return Err(LauncherError::GrantProtocol);
        }
        while let Some(grant) = self.outbound.get(self.next) {
            match send_grant(socket, &grant.frame, grant.descriptor) {
                Ok(()) => {
                    self.next = self
                        .next
                        .checked_add(1)
                        .ok_or(LauncherError::GrantProtocol)?;
                }
                Err(GrantTransportError::Io(io::ErrorKind::Interrupted)) => {
                    // Return to the level-triggered kqueue so signals, child
                    // exit, and the absolute deadline are observed first.
                    return Ok(());
                }
                Err(GrantTransportError::Io(io::ErrorKind::WouldBlock)) => return Ok(()),
                Err(GrantTransportError::Io(_) | GrantTransportError::Invalid) => {
                    return Err(LauncherError::GrantProtocol);
                }
            }
        }
        Ok(())
    }

    fn write_complete(&self) -> bool {
        self.started && self.next == self.outbound.len()
    }

    fn requires_write_event(&self) -> bool {
        self.started && !self.acknowledged && !self.cancelled && !self.write_complete()
    }

    const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn has_timed_out(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }

    fn requires_ack(&self) -> bool {
        self.started && !self.acknowledged && !self.cancelled
    }

    fn acknowledge(&mut self) -> Result<(), LauncherError> {
        if !self.write_complete() || self.acknowledged || self.cancelled {
            return Err(LauncherError::GrantProtocol);
        }
        self.acknowledged = true;
        self.deadline = None;
        Ok(())
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.deadline = None;
    }
}

fn sync_grant_write_event(
    kqueue: RawFd,
    grant_fd: usize,
    state: &mut GrantSendState,
) -> Result<(), LauncherError> {
    let should_enable = state.requires_write_event();
    if should_enable == state.event_enabled {
        return Ok(());
    }
    let flag = if should_enable {
        libc::EV_ENABLE
    } else {
        libc::EV_DISABLE
    };
    register_events(kqueue, &[event(grant_fd, libc::EVFILT_WRITE, flag, 0)])?;
    state.event_enabled = should_enable;
    Ok(())
}

fn shutdown_grants(socket: &UnixDatagram) {
    // SAFETY: The socket remains owned and shutdown only closes its connected
    // communication directions; final descriptor ownership stays with Rust.
    let _ = unsafe { libc::shutdown(socket.as_raw_fd(), libc::SHUT_RDWR) };
}

fn validate_terminal(
    status: ExitStatus,
    terminal: Option<(TerminalCategory, u8)>,
) -> Result<(), LauncherError> {
    let Some((_category, reported)) = terminal else {
        return Ok(());
    };
    if public_exit_code(status) == Some(reported) {
        Ok(())
    } else {
        Err(LauncherError::SessionProtocol)
    }
}

fn public_exit_code(status: ExitStatus) -> Option<u8> {
    if let Some(code) = status.code() {
        return u8::try_from(code).ok();
    }
    status
        .signal()
        .and_then(|signal| 128_i32.checked_add(signal))
        .and_then(|code| u8::try_from(code).ok())
}

fn create_kqueue() -> Result<OwnedFd, LauncherError> {
    // SAFETY: `kqueue` has no pointer arguments and returns a new descriptor on
    // success, which is immediately transferred into `OwnedFd`.
    let descriptor = unsafe { libc::kqueue() };
    if descriptor < 0 {
        return Err(LauncherError::WorkerWait(io::Error::last_os_error().kind()));
    }
    // SAFETY: `descriptor` is a fresh owned descriptor returned by `kqueue`.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

const fn event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: ptr::null_mut(),
    }
}

fn register_events(kqueue: RawFd, changes: &[libc::kevent]) -> Result<(), LauncherError> {
    let count = i32::try_from(changes.len())
        .map_err(|_| LauncherError::SignalSetup(io::ErrorKind::InvalidInput))?;
    // SAFETY: `changes` points to `count` initialized kevents for the duration
    // of the call; no output event buffer is requested.
    let result = unsafe {
        libc::kevent(
            kqueue,
            changes.as_ptr(),
            count,
            ptr::null_mut(),
            0,
            ptr::null(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(LauncherError::SignalSetup(
            io::Error::last_os_error().kind(),
        ))
    }
}

fn wait_events(
    kqueue: RawFd,
    timeout: Option<Duration>,
) -> Result<Vec<libc::kevent>, LauncherError> {
    let deadline = match timeout {
        Some(timeout) => Some(
            Instant::now()
                .checked_add(timeout)
                .ok_or(LauncherError::WorkerWait(io::ErrorKind::InvalidInput))?,
        ),
        None => None,
    };
    loop {
        let mut events = [MaybeUninit::<libc::kevent>::uninit(); 5];
        let timeout = deadline
            .map(|deadline| duration_timespec(deadline.saturating_duration_since(Instant::now())));
        let timeout_ptr = timeout
            .as_ref()
            .map_or(ptr::null(), |value| value as *const libc::timespec);
        // SAFETY: `events` provides room for four values. The optional timespec
        // remains live, and the kernel initializes exactly the positive count.
        let count = unsafe {
            libc::kevent(
                kqueue,
                ptr::null(),
                0,
                events.as_mut_ptr().cast(),
                5,
                timeout_ptr,
            )
        };
        if count >= 0 {
            let count = usize::try_from(count)
                .map_err(|_| LauncherError::WorkerWait(io::ErrorKind::InvalidData))?;
            return Ok(events
                .into_iter()
                .take(count)
                .map(|queued| {
                    // SAFETY: `kevent` initialized every event below `count`.
                    unsafe { queued.assume_init() }
                })
                .collect());
        }
        let kind = io::Error::last_os_error().kind();
        if kind != io::ErrorKind::Interrupted {
            return Err(LauncherError::WorkerWait(kind));
        }
    }
}

fn duration_timespec(duration: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: libc::time_t::try_from(duration.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::from(duration.subsec_nanos()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use bangbang_session::macos::runtime::{SnapshotStagingKind, SnapshotStagingName};
    use bangbang_session::{ObjectIdentity, ResourceRole, SocketChild};

    use super::*;

    static NEXT_SOCKET_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct SocketTestRoot(PathBuf);

    impl SocketTestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "bangbang-launcher-socket-cleanup-{}-{}",
                std::process::id(),
                NEXT_SOCKET_TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("socket cleanup root should create");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for SocketTestRoot {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("socket cleanup root should remove");
        }
    }

    #[test]
    fn event_batch_defers_reaping_until_all_events_are_available() {
        let signal_event = event(7, libc::EVFILT_READ, 0, 0);
        let exit_event = event(42, libc::EVFILT_PROC, 0, libc::NOTE_EXIT);
        let events = [exit_event, signal_event];
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].filter, libc::EVFILT_PROC);
        assert_eq!(events[1].filter, libc::EVFILT_READ);
    }

    #[test]
    fn terminal_status_must_match_reaped_public_exit() {
        let success = ExitStatus::from_raw(0);
        assert_eq!(
            validate_terminal(success, Some((TerminalCategory::Success, 0))),
            Ok(())
        );
        assert_eq!(
            validate_terminal(success, Some((TerminalCategory::ProcessFailure, 1))),
            Err(LauncherError::SessionProtocol)
        );
        let signaled = ExitStatus::from_raw(libc::SIGTERM);
        assert_eq!(public_exit_code(signaled), Some(128 + 15));
    }

    #[test]
    fn expired_deadline_is_an_immediate_timeout() {
        let deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("test deadline should be representable");
        assert_eq!(timeout_until(Some(deadline)), Some(Duration::ZERO));
        assert_eq!(timeout_until(None), None);
    }

    #[test]
    fn worker_exit_cleanup_removes_only_the_recorded_socket_identity() {
        let root = SocketTestRoot::new();
        let directory = fs::File::open(root.path()).expect("directory should open");
        let child = SocketChild::parse("api.sock").expect("child should parse");
        let path = root.path().join("api.sock");
        let listener = UnixListener::bind(&path).expect("socket should bind");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("socket permissions should tighten");
        let metadata = fs::symlink_metadata(&path).expect("socket metadata should read");
        let record = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            child.clone(),
            ObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        )
        .expect("record should construct");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("socket permissions should change");
        unlink_owned_socket(directory.as_raw_fd(), &record)
            .expect("mode-mismatched socket should be preserved");
        assert!(path.exists());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("socket permissions should restore");
        unlink_owned_socket(directory.as_raw_fd(), &record).expect("recorded socket should clean");
        assert!(!path.exists());
        drop(listener);

        let replacement_listener =
            UnixListener::bind(&path).expect("replacement socket should bind");
        unlink_owned_socket(directory.as_raw_fd(), &record)
            .expect("replacement identity should be preserved");
        assert!(path.exists());
        drop(replacement_listener);
        fs::remove_file(path).expect("replacement socket should remove");
    }

    #[test]
    fn worker_exit_cleanup_removes_only_the_recorded_snapshot_staging_identity() {
        let root = SocketTestRoot::new();
        let directory = fs::File::open(root.path()).expect("directory should open");
        let name = SnapshotStagingName::parse(
            SnapshotStagingKind::State,
            ".bangbang-snapshot-state-0123456789abcdef0123456789abcdef",
        )
        .expect("staging name should parse");
        let path = root
            .path()
            .join(std::str::from_utf8(name.as_bytes()).expect("name should be UTF-8"));
        fs::write(&path, b"staging").expect("staging fixture should write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("staging permissions should tighten");
        let metadata = fs::symlink_metadata(&path).expect("staging metadata should read");
        let directory_metadata = directory
            .metadata()
            .expect("directory metadata should read");
        let record = SnapshotStagingOwnershipRecord::new(
            SnapshotStagingKind::State,
            ObjectIdentity {
                device: directory_metadata.dev(),
                inode: directory_metadata.ino(),
            },
            name.clone(),
            ObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("staging permissions should change");
        unlink_owned_snapshot_staging(directory.as_raw_fd(), &record)
            .expect("mode-mismatched staging file should be preserved");
        assert!(path.exists());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("staging permissions should restore");
        unlink_owned_snapshot_staging(directory.as_raw_fd(), &record)
            .expect("recorded staging file should clean");
        assert!(!path.exists());

        fs::write(&path, b"replacement").expect("replacement fixture should write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("replacement permissions should tighten");
        unlink_owned_snapshot_staging(directory.as_raw_fd(), &record)
            .expect("replacement identity should be preserved");
        assert!(path.exists());
        fs::remove_file(path).expect("replacement staging file should remove");
    }
}
