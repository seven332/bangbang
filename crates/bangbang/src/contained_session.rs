use std::fmt;
#[cfg(not(target_os = "macos"))]
use std::os::unix::net::UnixStream;

#[cfg(not(target_os = "macos"))]
use bangbang_session::{Readiness, TerminalCategory};

/// Stable private bootstrap failure that never includes identity or path data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContainedSessionError;

impl fmt::Display for ContainedSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private launcher session failed")
    }
}

impl std::error::Error for ContainedSessionError {}

#[cfg(target_os = "macos")]
mod platform {
    use std::env;
    use std::ffi::OsStr;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::{UnixDatagram, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use bangbang_session::macos::grant_registry::{
        CommittedGrantBatch, GrantRegistry, StagedGrantBatch,
    };
    use bangbang_session::macos::grant_transport::receive_grant;
    use bangbang_session::macos::runtime::WorkerNamespace;
    use bangbang_session::macos::{set_cloexec, verify_peer, verify_peer_pid};
    use bangbang_session::{
        Frame, FrameDecoder, GRANT_FD, Message, Readiness, SESSION_ENV_KEY, SESSION_ENV_VALUE,
        SESSION_FD, TerminalCategory, WorkerLifecycle, encode_frame,
    };

    use super::ContainedSessionError;

    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    #[cfg(feature = "grant-integration-probe")]
    const GRANT_DELAY_PROBE: &str = "--bangbang-internal-grant-delay-v1";
    #[cfg(feature = "grant-integration-probe")]
    const GRANT_DELAY_READY: &str = "status: grant integration delay ready";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ControlState {
        Running,
        Cancelled,
        Disconnected,
        ProtocolFailure,
    }

    #[derive(Debug)]
    struct SharedControl {
        state: Mutex<ControlState>,
        closing: AtomicBool,
    }

    impl SharedControl {
        fn new(state: ControlState) -> Self {
            Self {
                state: Mutex::new(state),
                closing: AtomicBool::new(false),
            }
        }

        fn state(&self) -> Result<ControlState, ContainedSessionError> {
            self.state
                .lock()
                .map(|state| *state)
                .map_err(|_| ContainedSessionError)
        }

        fn set(&self, state: ControlState) -> Result<(), ContainedSessionError> {
            let mut current = self.state.lock().map_err(|_| ContainedSessionError)?;
            if *current == ControlState::Running {
                *current = state;
            }
            Ok(())
        }
    }

    pub(crate) struct ContainedSession {
        stream: UnixStream,
        lifecycle: Arc<Mutex<WorkerLifecycle>>,
        namespace: Arc<Mutex<Option<WorkerNamespace>>>,
        control: Arc<SharedControl>,
        wakeup_reader: Option<UnixStream>,
        wakeup_writer: Option<UnixStream>,
        reader: Option<JoinHandle<()>>,
        grants: Option<GrantRegistry>,
        started: bool,
        closed: bool,
    }

    impl std::fmt::Debug for ContainedSession {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ContainedSession")
                .field("identity", &"<redacted>")
                .field("started", &self.started)
                .field("closed", &self.closed)
                .finish()
        }
    }

    impl ContainedSession {
        pub(crate) fn bootstrap() -> Result<Option<Self>, ContainedSessionError> {
            let Some(value) = env::var_os(SESSION_ENV_KEY) else {
                return Ok(None);
            };
            // SAFETY: Bootstrap runs at the first line of process main before
            // any application thread is created; removing the private marker
            // prevents it from leaking to later child processes.
            unsafe { env::remove_var(SESSION_ENV_KEY) };
            if value != OsStr::new(SESSION_ENV_VALUE) {
                return Err(ContainedSessionError);
            }
            set_cloexec(SESSION_FD).map_err(|_| ContainedSessionError)?;
            set_cloexec(GRANT_FD).map_err(|_| ContainedSessionError)?;
            // SAFETY: The validated private bootstrap contract transfers the
            // fixed descriptor exactly once into this process object.
            let owned = unsafe { OwnedFd::from_raw_fd(SESSION_FD) };
            let mut stream = UnixStream::from(owned);
            // SAFETY: The same validated bootstrap contract transfers fixed
            // grant descriptor 4 exactly once into this process object.
            let grant_owned = unsafe { OwnedFd::from_raw_fd(GRANT_FD) };
            let grant_socket = UnixDatagram::from(grant_owned);
            stream
                .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| ContainedSessionError)?;
            // SAFETY: `getppid` has no pointer or ownership contract.
            let parent = unsafe { libc::getppid() };
            verify_peer(stream.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;
            verify_peer_pid(grant_socket.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;

            let mut decoder = FrameDecoder::default();
            let mut lifecycle = WorkerLifecycle::new();
            let hello = lifecycle.hello().map_err(|_| ContainedSessionError)?;
            write_frame(&mut stream, hello)?;
            let start = read_frame(&mut stream, &mut decoder, handshake_deadline()?)?;
            if lifecycle
                .receive(start)
                .map_err(|_| ContainedSessionError)?
                != Message::Start
            {
                return Err(ContainedSessionError);
            }
            let session = lifecycle.session().ok_or(ContainedSessionError)?;
            let namespace = WorkerNamespace::create(session).map_err(|_| ContainedSessionError)?;
            let identity = namespace.identity();
            let prepared = lifecycle
                .prepared(identity.device, identity.inode)
                .map_err(|_| ContainedSessionError)?;
            write_frame(&mut stream, prepared)?;

            #[cfg(feature = "grant-integration-probe")]
            let receive_grants =
                if env::args_os().nth(1).as_deref() == Some(OsStr::new(GRANT_DELAY_PROBE)) {
                    println!("{GRANT_DELAY_READY}");
                    std::io::stdout()
                        .flush()
                        .map_err(|_| ContainedSessionError)?;
                    false
                } else {
                    true
                };
            #[cfg(not(feature = "grant-integration-probe"))]
            let receive_grants = true;

            let grant_outcome = receive_grant_batch(
                &mut stream,
                &mut decoder,
                &mut lifecycle,
                &grant_socket,
                session,
                handshake_deadline()?,
                receive_grants,
            )?;
            drop(grant_socket);
            let (grants, cancelled) = match grant_outcome {
                GrantPhaseOutcome::Committed(committed) => {
                    let accepted = lifecycle
                        .grants_accepted(
                            committed.batch,
                            committed.grant_count,
                            committed.final_sequence,
                        )
                        .map_err(|_| ContainedSessionError)?;
                    write_frame(&mut stream, accepted)?;
                    (committed.registry, false)
                }
                GrantPhaseOutcome::Cancelled => (GrantRegistry::default(), true),
            };
            let started = if cancelled {
                false
            } else {
                let next = read_frame(&mut stream, &mut decoder, handshake_deadline()?)?;
                let next = lifecycle.receive(next).map_err(|_| ContainedSessionError)?;
                match next {
                    Message::Proceed => {
                        verify_peer(stream.as_raw_fd(), parent)
                            .map_err(|_| ContainedSessionError)?;
                        let starting = lifecycle.starting().map_err(|_| ContainedSessionError)?;
                        write_frame(&mut stream, starting)?;
                        true
                    }
                    Message::Cancel(_) => false,
                    _ => return Err(ContainedSessionError),
                }
            };
            stream
                .set_read_timeout(None)
                .map_err(|_| ContainedSessionError)?;
            stream
                .set_write_timeout(None)
                .map_err(|_| ContainedSessionError)?;

            let initial_state = if started {
                ControlState::Running
            } else {
                ControlState::Cancelled
            };
            let control = Arc::new(SharedControl::new(initial_state));
            let lifecycle = Arc::new(Mutex::new(lifecycle));
            let namespace = Arc::new(Mutex::new(Some(namespace)));
            let (wakeup_reader, mut wakeup_writer) =
                UnixStream::pair().map_err(|_| ContainedSessionError)?;
            let reader = if started {
                let reader_stream = stream.try_clone().map_err(|_| ContainedSessionError)?;
                let reader_wakeup = wakeup_writer
                    .try_clone()
                    .map_err(|_| ContainedSessionError)?;
                Some(spawn_reader(
                    reader_stream,
                    decoder,
                    Arc::clone(&lifecycle),
                    Arc::clone(&namespace),
                    Arc::clone(&control),
                    reader_wakeup,
                )?)
            } else {
                wakeup_writer
                    .write_all(&[1])
                    .map_err(|_| ContainedSessionError)?;
                None
            };
            Ok(Some(Self {
                stream,
                lifecycle,
                namespace,
                control,
                wakeup_reader: Some(wakeup_reader),
                wakeup_writer: Some(wakeup_writer),
                reader,
                grants: Some(grants),
                started,
                closed: false,
            }))
        }

        pub(crate) fn take_wakeup_pair(
            &mut self,
        ) -> Result<(UnixStream, UnixStream), ContainedSessionError> {
            Ok((
                self.wakeup_reader.take().ok_or(ContainedSessionError)?,
                self.wakeup_writer.take().ok_or(ContainedSessionError)?,
            ))
        }

        pub(crate) fn shutdown_requested(&self) -> Result<bool, ContainedSessionError> {
            match self.control.state()? {
                ControlState::Running => Ok(false),
                ControlState::Cancelled => Ok(true),
                ControlState::Disconnected | ControlState::ProtocolFailure => {
                    Err(ContainedSessionError)
                }
            }
        }

        pub(crate) fn was_cancelled(&self) -> bool {
            self.control.state().ok() == Some(ControlState::Cancelled)
        }

        #[cfg(feature = "grant-integration-probe")]
        pub(crate) fn grant_registry_mut(
            &mut self,
        ) -> Result<&mut GrantRegistry, ContainedSessionError> {
            self.grants.as_mut().ok_or(ContainedSessionError)
        }

        pub(crate) fn send_ready(
            &mut self,
            readiness: Readiness,
        ) -> Result<(), ContainedSessionError> {
            if self.shutdown_requested()? {
                return Err(ContainedSessionError);
            }
            let frame = self
                .lifecycle
                .lock()
                .map_err(|_| ContainedSessionError)?
                .ready(readiness)
                .map_err(|_| ContainedSessionError)?;
            write_frame(&mut self.stream, frame)
        }

        pub(crate) fn finish(
            &mut self,
            category: TerminalCategory,
            exit_code: u8,
        ) -> Result<(), ContainedSessionError> {
            if self.closed {
                return Ok(());
            }
            let terminal_result = if self.started {
                let frame = self
                    .lifecycle
                    .lock()
                    .map_err(|_| ContainedSessionError)?
                    .terminal(category, exit_code)
                    .map_err(|_| ContainedSessionError)?;
                write_frame(&mut self.stream, frame)
            } else {
                Ok(())
            };
            self.close();
            terminal_result
        }

        fn close(&mut self) {
            if self.closed {
                return;
            }
            self.closed = true;
            self.control.closing.store(true, Ordering::Release);
            self.grants.take();
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            cleanup_namespace(&self.namespace);
        }
    }

    impl Drop for ContainedSession {
        fn drop(&mut self) {
            self.close();
        }
    }

    enum GrantPhaseOutcome {
        Committed(CommittedGrantBatch),
        Cancelled,
    }

    fn receive_grant_batch(
        stream: &mut UnixStream,
        decoder: &mut FrameDecoder,
        lifecycle: &mut WorkerLifecycle,
        grant_socket: &UnixDatagram,
        session: bangbang_session::SessionId,
        deadline: Instant,
        receive_grants: bool,
    ) -> Result<GrantPhaseOutcome, ContainedSessionError> {
        let mut staged = StagedGrantBatch::new(session);
        loop {
            if let Some(frame) = decoder.next_frame().map_err(|_| ContainedSessionError)? {
                return receive_grant_control(lifecycle, frame);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(ContainedSessionError)?;
            let millis = remaining.as_millis().max(1);
            let timeout = i32::try_from(millis).unwrap_or(i32::MAX);
            let mut descriptors = [
                libc::pollfd {
                    fd: stream.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: grant_socket.as_raw_fd(),
                    events: if receive_grants { libc::POLLIN } else { 0 },
                    revents: 0,
                },
            ];
            // SAFETY: descriptors is writable for its exact element count and
            // both descriptors remain owned during the synchronous poll.
            let result = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    libc::nfds_t::try_from(descriptors.len()).map_err(|_| ContainedSessionError)?,
                    timeout,
                )
            };
            if result < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(ContainedSessionError);
            }
            if result == 0 {
                return Err(ContainedSessionError);
            }
            if descriptors
                .first()
                .is_some_and(|descriptor| descriptor.revents & libc::POLLIN != 0)
            {
                let frame = read_frame(stream, decoder, deadline)?;
                return receive_grant_control(lifecycle, frame);
            }
            if descriptors
                .get(1)
                .is_some_and(|descriptor| descriptor.revents & libc::POLLIN != 0)
            {
                let received = receive_grant(grant_socket).map_err(|_| ContainedSessionError)?;
                if let Some(committed) =
                    staged.accept(received).map_err(|_| ContainedSessionError)?
                {
                    return Ok(GrantPhaseOutcome::Committed(committed));
                }
            }
            let invalid = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
            if descriptors
                .iter()
                .any(|descriptor| descriptor.revents & invalid != 0)
            {
                return Err(ContainedSessionError);
            }
        }
    }

    fn receive_grant_control(
        lifecycle: &mut WorkerLifecycle,
        frame: Frame,
    ) -> Result<GrantPhaseOutcome, ContainedSessionError> {
        match lifecycle
            .receive(frame)
            .map_err(|_| ContainedSessionError)?
        {
            Message::Cancel(_) => Ok(GrantPhaseOutcome::Cancelled),
            _ => Err(ContainedSessionError),
        }
    }

    fn spawn_reader(
        mut stream: UnixStream,
        mut decoder: FrameDecoder,
        lifecycle: Arc<Mutex<WorkerLifecycle>>,
        namespace: Arc<Mutex<Option<WorkerNamespace>>>,
        control: Arc<SharedControl>,
        mut wakeup: UnixStream,
    ) -> Result<JoinHandle<()>, ContainedSessionError> {
        thread::Builder::new()
            .name("bangbang-session-control".to_string())
            .spawn(move || {
                let state = reader_loop(&mut stream, &mut decoder, &lifecycle);
                if !control.closing.load(Ordering::Acquire) {
                    if state == ControlState::Disconnected {
                        cleanup_namespace(&namespace);
                    }
                    let _ = control.set(state);
                    let _ = wakeup.write_all(&[1]);
                }
            })
            .map_err(|_| ContainedSessionError)
    }

    fn reader_loop(
        stream: &mut UnixStream,
        decoder: &mut FrameDecoder,
        lifecycle: &Mutex<WorkerLifecycle>,
    ) -> ControlState {
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => {
                    let message = lifecycle
                        .lock()
                        .map_err(|_| ())
                        .and_then(|mut lifecycle| lifecycle.receive(frame).map_err(|_| ()));
                    return match message {
                        Ok(Message::Cancel(_)) => ControlState::Cancelled,
                        _ => ControlState::ProtocolFailure,
                    };
                }
                Ok(None) => {}
                Err(_) => return ControlState::ProtocolFailure,
            }

            let mut bytes = [0_u8; 4096];
            match stream.read(&mut bytes) {
                Ok(0) => {
                    return if decoder.finish().is_ok() {
                        ControlState::Disconnected
                    } else {
                        ControlState::ProtocolFailure
                    };
                }
                Ok(length) => {
                    let Some(bytes) = bytes.get(..length) else {
                        return ControlState::ProtocolFailure;
                    };
                    if decoder.push(bytes).is_err() {
                        return ControlState::ProtocolFailure;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return ControlState::Disconnected,
            }
        }
    }

    fn read_frame(
        stream: &mut UnixStream,
        decoder: &mut FrameDecoder,
        deadline: Instant,
    ) -> Result<Frame, ContainedSessionError> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(ContainedSessionError)?;
            if let Some(frame) = decoder.next_frame().map_err(|_| ContainedSessionError)? {
                return Ok(frame);
            }
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|_| ContainedSessionError)?;
            let mut bytes = [0_u8; 4096];
            let length = match stream.read(&mut bytes) {
                Ok(length) => length,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(ContainedSessionError),
            };
            if length == 0 {
                return Err(ContainedSessionError);
            }
            decoder
                .push(bytes.get(..length).ok_or(ContainedSessionError)?)
                .map_err(|_| ContainedSessionError)?;
        }
    }

    fn handshake_deadline() -> Result<Instant, ContainedSessionError> {
        Instant::now()
            .checked_add(HANDSHAKE_TIMEOUT)
            .ok_or(ContainedSessionError)
    }

    fn write_frame(stream: &mut UnixStream, frame: Frame) -> Result<(), ContainedSessionError> {
        let encoded = encode_frame(frame).map_err(|_| ContainedSessionError)?;
        stream
            .write_all(&encoded)
            .map_err(|_| ContainedSessionError)
    }

    fn cleanup_namespace(namespace: &Mutex<Option<WorkerNamespace>>) {
        if let Ok(mut namespace) = namespace.lock()
            && let Some(namespace) = namespace.as_mut()
        {
            let _ = namespace.cleanup();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{ContainedSessionError, Readiness, TerminalCategory, UnixStream};

    #[derive(Debug)]
    pub(crate) struct ContainedSession;

    impl ContainedSession {
        pub(crate) fn bootstrap() -> Result<Option<Self>, ContainedSessionError> {
            Ok(None)
        }

        pub(crate) fn take_wakeup_pair(
            &mut self,
        ) -> Result<(UnixStream, UnixStream), ContainedSessionError> {
            Err(ContainedSessionError)
        }

        pub(crate) fn shutdown_requested(&self) -> Result<bool, ContainedSessionError> {
            Ok(false)
        }

        pub(crate) fn was_cancelled(&self) -> bool {
            false
        }

        pub(crate) fn send_ready(
            &mut self,
            _readiness: Readiness,
        ) -> Result<(), ContainedSessionError> {
            Err(ContainedSessionError)
        }

        pub(crate) fn finish(
            &mut self,
            _category: TerminalCategory,
            _exit_code: u8,
        ) -> Result<(), ContainedSessionError> {
            Err(ContainedSessionError)
        }
    }
}

pub(crate) use platform::ContainedSession;
