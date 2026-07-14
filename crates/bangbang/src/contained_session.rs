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
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use bangbang_session::macos::runtime::WorkerNamespace;
    use bangbang_session::macos::{set_cloexec, verify_peer};
    use bangbang_session::{
        Frame, FrameDecoder, Message, Readiness, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD,
        TerminalCategory, WorkerLifecycle, encode_frame,
    };

    use super::ContainedSessionError;

    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

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
            // SAFETY: The validated private bootstrap contract transfers the
            // fixed descriptor exactly once into this process object.
            let owned = unsafe { OwnedFd::from_raw_fd(SESSION_FD) };
            let mut stream = UnixStream::from(owned);
            stream
                .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| ContainedSessionError)?;
            stream
                .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| ContainedSessionError)?;
            // SAFETY: `getppid` has no pointer or ownership contract.
            let parent = unsafe { libc::getppid() };
            verify_peer(stream.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;

            let mut decoder = FrameDecoder::default();
            let mut lifecycle = WorkerLifecycle::new();
            let hello = lifecycle.hello().map_err(|_| ContainedSessionError)?;
            write_frame(&mut stream, hello)?;
            let start = read_frame(&mut stream, &mut decoder)?;
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

            let next = read_frame(&mut stream, &mut decoder)?;
            let next = lifecycle.receive(next).map_err(|_| ContainedSessionError)?;
            let started = match next {
                Message::Proceed => {
                    verify_peer(stream.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;
                    let starting = lifecycle.starting().map_err(|_| ContainedSessionError)?;
                    write_frame(&mut stream, starting)?;
                    true
                }
                Message::Cancel(_) => false,
                _ => return Err(ContainedSessionError),
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
    ) -> Result<Frame, ContainedSessionError> {
        loop {
            if let Some(frame) = decoder.next_frame().map_err(|_| ContainedSessionError)? {
                return Ok(frame);
            }
            let mut bytes = [0_u8; 4096];
            let length = stream.read(&mut bytes).map_err(|_| ContainedSessionError)?;
            if length == 0 {
                return Err(ContainedSessionError);
            }
            decoder
                .push(bytes.get(..length).ok_or(ContainedSessionError)?)
                .map_err(|_| ContainedSessionError)?;
        }
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
