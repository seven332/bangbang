use std::env;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bangbang_session::macos::{set_cloexec, verify_peer};
use signal_hook::SigId;
use signal_hook::consts::signal::{SIGINT, SIGTERM};

use super::spawn::{
    DAEMON_ENV_KEY, DAEMON_ENV_VALUE, DAEMON_HANDOFF_FD, OwnedWorker, spawn_daemon_suspended,
};
use crate::launch_policy::{LaunchRequest, LaunchTiming};
use crate::{BundleLayout, LauncherError};

const MAGIC: [u8; 4] = *b"BBH1";
const VERSION: u16 = 1;
const FRAME_BYTES: usize = 40;
const HEADER_BYTES: usize = 16;
const KIND_HELLO: u16 = 1;
const KIND_START: u16 = 2;
const KIND_READY: u16 = 3;
const KIND_ACK: u16 = 4;
const KIND_FAILED: u16 = 5;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);
const PARENT_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, PartialEq, Eq)]
enum HandoffMessage {
    Hello,
    Start {
        monotonic_us: u64,
        parent_cpu_us: u64,
    },
    Ready {
        supervisor_pid: libc::pid_t,
    },
    Ack {
        supervisor_pid: libc::pid_t,
    },
    Failed {
        category: FailureCategory,
    },
}

impl std::fmt::Debug for HandoffMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hello => formatter.write_str("Hello"),
            Self::Start { .. } => formatter.write_str("Start(<redacted>)"),
            Self::Ready { .. } => formatter.write_str("Ready(<redacted>)"),
            Self::Ack { .. } => formatter.write_str("Ack(<redacted>)"),
            Self::Failed { .. } => formatter.write_str("Failed(<redacted>)"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HandoffFrame {
    sequence: u64,
    message: HandoffMessage,
}

impl std::fmt::Debug for HandoffFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HandoffFrame")
            .field("sequence", &"<redacted>")
            .field("message", &self.message)
            .finish()
    }
}

pub(crate) struct DaemonChildBootstrap {
    pub(crate) timing: LaunchTiming,
    pub(crate) notifier: DaemonNotifier,
}

impl std::fmt::Debug for DaemonChildBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DaemonChildBootstrap(<redacted>)")
    }
}

pub(crate) fn child_bootstrap() -> Result<Option<DaemonChildBootstrap>, LauncherError> {
    let Some(marker) = env::var_os(DAEMON_ENV_KEY) else {
        return Ok(None);
    };
    // SAFETY: This is called at the first launcher-library boundary before any
    // application thread is created.
    unsafe { env::remove_var(DAEMON_ENV_KEY) };
    if marker != DAEMON_ENV_VALUE {
        return Err(LauncherError::DaemonHandoff);
    }
    set_cloexec(DAEMON_HANDOFF_FD).map_err(|_| LauncherError::DaemonHandoff)?;
    // SAFETY: The authenticated private spawn contract transfers descriptor 6
    // exactly once after the successful descriptor validation above.
    let owned = unsafe { OwnedFd::from_raw_fd(DAEMON_HANDOFF_FD) };
    let mut stream = UnixStream::from(owned);
    // SAFETY: These identity/session getters take no retained pointers.
    let (pid, parent, session, process_group) = unsafe {
        (
            libc::getpid(),
            libc::getppid(),
            libc::getsid(0),
            libc::getpgrp(),
        )
    };
    if pid <= 0 || parent <= 0 || session != pid || process_group != pid {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer(stream.as_raw_fd(), parent).map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    super::code_sign::validate_launcher_process(parent)?;
    let deadline = Instant::now()
        .checked_add(HANDOFF_TIMEOUT)
        .ok_or(LauncherError::DaemonHandoff)?;
    send_frame(
        &mut stream,
        HandoffFrame {
            sequence: 0,
            message: HandoffMessage::Hello,
        },
    )?;
    let start = read_frame_until(&mut stream, deadline, || Ok(()))?;
    let HandoffFrame {
        sequence: 0,
        message:
            HandoffMessage::Start {
                monotonic_us,
                parent_cpu_us,
            },
    } = start
    else {
        return Err(LauncherError::DaemonHandoff);
    };
    verify_peer(stream.as_raw_fd(), parent).map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    let timing = LaunchTiming::from_daemon_handoff(monotonic_us, parent_cpu_us)?;
    Ok(Some(DaemonChildBootstrap {
        timing,
        notifier: DaemonNotifier::new(stream, deadline)?,
    }))
}

pub(crate) fn launch_parent(
    request: &LaunchRequest,
    timing: LaunchTiming,
    executable: &Path,
    layout: &BundleLayout,
) -> Result<(), LauncherError> {
    request.validate(layout.worker_executable(), true)?;
    let signals = DaemonSignals::install()?;
    let (mut child, mut stream) = spawn_daemon_suspended(executable, request.raw_args().to_vec())?;
    super::code_sign::validate_launcher_process(child.pid())?;
    child.resume().map_err(|_| LauncherError::DaemonHandoff)?;
    let deadline = Instant::now()
        .checked_add(HANDOFF_TIMEOUT)
        .ok_or(LauncherError::DaemonHandoff)?;
    let hello = read_parent_frame(&mut stream, &mut child, &signals, deadline)?;
    if hello
        != (HandoffFrame {
            sequence: 0,
            message: HandoffMessage::Hello,
        })
    {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer(stream.as_raw_fd(), child.pid())
        .map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    send_frame(
        &mut stream,
        HandoffFrame {
            sequence: 0,
            message: HandoffMessage::Start {
                monotonic_us: timing.monotonic_us(),
                parent_cpu_us: timing.elapsed_process_cpu_us()?,
            },
        },
    )?;
    let ready = read_parent_frame(&mut stream, &mut child, &signals, deadline)?;
    if let HandoffFrame {
        sequence: 1,
        message: HandoffMessage::Failed { category },
    } = ready
    {
        return Err(category.error());
    }
    let HandoffFrame {
        sequence: 1,
        message: HandoffMessage::Ready { supervisor_pid },
    } = ready
    else {
        return Err(LauncherError::DaemonHandoff);
    };
    if supervisor_pid != child.pid() || child.try_wait()?.is_some() {
        return Err(LauncherError::DaemonHandoff);
    }
    verify_peer(stream.as_raw_fd(), supervisor_pid)
        .map_err(|_| LauncherError::InvalidDaemonIdentity)?;
    send_frame(
        &mut stream,
        HandoffFrame {
            sequence: 1,
            message: HandoffMessage::Ack { supervisor_pid },
        },
    )?;
    if child.try_wait()?.is_some() {
        return Err(LauncherError::DaemonHandoff);
    }
    if signals.received() {
        return Err(LauncherError::DaemonHandoff);
    }
    let pid = child.pid();
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "bangbang daemon pid: {pid}")
        .and_then(|()| stdout.flush())
        .map_err(|_| LauncherError::DaemonHandoff)?;
    let released = child.release();
    debug_assert_eq!(released, pid);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotifierEvent {
    Pending,
    Acknowledged,
    ParentLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotifierState {
    AwaitReady,
    AwaitAck { supervisor_pid: libc::pid_t },
    Detached,
    ParentLost,
}

pub(crate) struct DaemonNotifier {
    stream: Option<UnixStream>,
    decoder: HandoffDecoder,
    state: NotifierState,
    deadline: Instant,
}

impl std::fmt::Debug for DaemonNotifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonNotifier")
            .field("state", &self.state)
            .field("transport", &"<redacted>")
            .field("deadline", &"<redacted>")
            .finish()
    }
}

impl DaemonNotifier {
    fn new(stream: UnixStream, deadline: Instant) -> Result<Self, LauncherError> {
        stream
            .set_nonblocking(true)
            .map_err(|_| LauncherError::DaemonHandoff)?;
        Ok(Self {
            stream: Some(stream),
            decoder: HandoffDecoder::default(),
            state: NotifierState::AwaitReady,
            deadline,
        })
    }

    pub(crate) fn as_raw_fd(&self) -> Result<libc::c_int, LauncherError> {
        self.stream
            .as_ref()
            .map(AsRawFd::as_raw_fd)
            .ok_or(LauncherError::DaemonHandoff)
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        matches!(
            self.state,
            NotifierState::AwaitReady | NotifierState::AwaitAck { .. }
        )
        .then_some(self.deadline)
    }

    pub(crate) fn is_awaiting_ready(&self) -> bool {
        self.state == NotifierState::AwaitReady
    }

    pub(crate) fn check_parent(&mut self) -> Result<NotifierEvent, LauncherError> {
        let descriptor = self.as_raw_fd()?;
        let mut byte = 0_u8;
        // SAFETY: The stream descriptor is live and `byte` is writable for one
        // non-consuming byte probe.
        let result = unsafe {
            libc::recv(
                descriptor,
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if result == 0 {
            self.state = NotifierState::ParentLost;
            return Ok(NotifierEvent::ParentLost);
        }
        if result > 0 {
            if self.state == NotifierState::AwaitReady {
                return Err(LauncherError::DaemonHandoff);
            }
            return self.drain();
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(NotifierEvent::Pending)
        } else {
            Err(LauncherError::DaemonHandoff)
        }
    }

    pub(crate) fn notify_ready(
        &mut self,
        supervisor_pid: libc::pid_t,
    ) -> Result<(), LauncherError> {
        if self.state != NotifierState::AwaitReady || supervisor_pid <= 0 {
            return Err(LauncherError::DaemonHandoff);
        }
        if self.check_parent()? != NotifierEvent::Pending {
            return Err(LauncherError::DaemonHandoff);
        }
        let stream = self.stream.as_mut().ok_or(LauncherError::DaemonHandoff)?;
        send_frame(
            stream,
            HandoffFrame {
                sequence: 1,
                message: HandoffMessage::Ready { supervisor_pid },
            },
        )?;
        self.state = NotifierState::AwaitAck { supervisor_pid };
        Ok(())
    }

    pub(crate) fn notify_failure(&mut self, error: LauncherError) {
        if matches!(
            self.state,
            NotifierState::AwaitReady | NotifierState::AwaitAck { .. }
        ) {
            if let Some(stream) = self.stream.as_mut() {
                let _ = send_frame(
                    stream,
                    HandoffFrame {
                        sequence: 1,
                        message: HandoffMessage::Failed {
                            category: FailureCategory::from_error(error),
                        },
                    },
                );
            }
            self.state = NotifierState::Detached;
            self.close_transport();
        }
    }

    pub(crate) fn drain(&mut self) -> Result<NotifierEvent, LauncherError> {
        let expected_pid = match self.state {
            NotifierState::AwaitAck { supervisor_pid } => Some(supervisor_pid),
            NotifierState::AwaitReady => None,
            NotifierState::Detached => return Ok(NotifierEvent::Acknowledged),
            NotifierState::ParentLost => return Ok(NotifierEvent::ParentLost),
        };
        let mut buffer = [0_u8; FRAME_BYTES];
        loop {
            let read = self
                .stream
                .as_mut()
                .ok_or(LauncherError::DaemonHandoff)?
                .read(&mut buffer);
            match read {
                Ok(0) => {
                    self.state = NotifierState::ParentLost;
                    return Ok(NotifierEvent::ParentLost);
                }
                Ok(length) => {
                    self.decoder
                        .push(buffer.get(..length).ok_or(LauncherError::DaemonHandoff)?)?;
                    if let Some(frame) = self.decoder.take()? {
                        let Some(expected_pid) = expected_pid else {
                            return Err(LauncherError::DaemonHandoff);
                        };
                        if frame
                            != (HandoffFrame {
                                sequence: 1,
                                message: HandoffMessage::Ack {
                                    supervisor_pid: expected_pid,
                                },
                            })
                            || !self.decoder.is_empty()
                        {
                            return Err(LauncherError::DaemonHandoff);
                        }
                        self.state = NotifierState::Detached;
                        if let Some(stream) = self.stream.as_ref() {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }
                        return Ok(NotifierEvent::Acknowledged);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(NotifierEvent::Pending);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(LauncherError::DaemonHandoff),
            }
        }
    }

    pub(crate) fn close_transport(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn read_parent_frame(
    stream: &mut UnixStream,
    child: &mut OwnedWorker,
    signals: &DaemonSignals,
    deadline: Instant,
) -> Result<HandoffFrame, LauncherError> {
    read_frame_until(stream, deadline, || {
        if signals.received() {
            Err(LauncherError::DaemonHandoff)
        } else {
            let _ = child.try_wait()?;
            Ok(())
        }
    })
}

fn read_frame_until(
    stream: &mut UnixStream,
    deadline: Instant,
    mut check: impl FnMut() -> Result<(), LauncherError>,
) -> Result<HandoffFrame, LauncherError> {
    let mut decoder = HandoffDecoder::default();
    let mut buffer = [0_u8; FRAME_BYTES];
    loop {
        check()?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(LauncherError::DaemonHandoff)?;
        stream
            .set_read_timeout(Some(remaining.min(PARENT_POLL)))
            .map_err(|_| LauncherError::DaemonHandoff)?;
        match stream.read(&mut buffer) {
            Ok(0) => return Err(LauncherError::DaemonHandoff),
            Ok(length) => {
                decoder.push(buffer.get(..length).ok_or(LauncherError::DaemonHandoff)?)?;
                if let Some(frame) = decoder.take()? {
                    if !decoder.is_empty() {
                        return Err(LauncherError::DaemonHandoff);
                    }
                    return Ok(frame);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(LauncherError::DaemonHandoff),
        }
    }
}

fn send_frame(stream: &mut UnixStream, frame: HandoffFrame) -> Result<(), LauncherError> {
    stream
        .set_write_timeout(Some(PARENT_POLL))
        .map_err(|_| LauncherError::DaemonHandoff)?;
    stream
        .write_all(&encode_frame(frame)?)
        .map_err(|_| LauncherError::DaemonHandoff)
}

fn encode_frame(frame: HandoffFrame) -> Result<[u8; FRAME_BYTES], LauncherError> {
    let mut bytes = [0_u8; FRAME_BYTES];
    bytes[..4].copy_from_slice(&MAGIC);
    bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
    let kind = match frame.message {
        HandoffMessage::Hello => KIND_HELLO,
        HandoffMessage::Start {
            monotonic_us,
            parent_cpu_us,
        } => {
            bytes[16..24].copy_from_slice(&monotonic_us.to_be_bytes());
            bytes[24..32].copy_from_slice(&parent_cpu_us.to_be_bytes());
            KIND_START
        }
        HandoffMessage::Ready { supervisor_pid } => {
            write_pid(&mut bytes, supervisor_pid)?;
            KIND_READY
        }
        HandoffMessage::Ack { supervisor_pid } => {
            write_pid(&mut bytes, supervisor_pid)?;
            KIND_ACK
        }
        HandoffMessage::Failed { category } => {
            bytes[16] = category as u8;
            KIND_FAILED
        }
    };
    bytes[6..8].copy_from_slice(&kind.to_be_bytes());
    bytes[8..16].copy_from_slice(&frame.sequence.to_be_bytes());
    Ok(bytes)
}

fn write_pid(bytes: &mut [u8; FRAME_BYTES], pid: libc::pid_t) -> Result<(), LauncherError> {
    let pid = u32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        .ok_or(LauncherError::DaemonHandoff)?;
    bytes[16..20].copy_from_slice(&pid.to_be_bytes());
    Ok(())
}

fn decode_frame(bytes: &[u8]) -> Result<HandoffFrame, LauncherError> {
    if bytes.len() != FRAME_BYTES
        || bytes.get(..4) != Some(MAGIC.as_slice())
        || read_u16(bytes, 4)? != VERSION
    {
        return Err(LauncherError::DaemonHandoff);
    }
    let kind = read_u16(bytes, 6)?;
    let sequence = read_u64(bytes, 8)?;
    let payload = bytes
        .get(HEADER_BYTES..)
        .ok_or(LauncherError::DaemonHandoff)?;
    let message = match kind {
        KIND_HELLO if payload.iter().all(|byte| *byte == 0) => HandoffMessage::Hello,
        KIND_START
            if payload
                .get(16..)
                .is_some_and(|reserved| reserved.iter().all(|byte| *byte == 0)) =>
        {
            HandoffMessage::Start {
                monotonic_us: read_u64(payload, 0)?,
                parent_cpu_us: read_u64(payload, 8)?,
            }
        }
        KIND_READY | KIND_ACK
            if payload
                .get(4..)
                .is_some_and(|reserved| reserved.iter().all(|byte| *byte == 0)) =>
        {
            let pid = read_u32(payload, 0)?;
            let pid = i32::try_from(pid)
                .ok()
                .filter(|pid| *pid > 0)
                .ok_or(LauncherError::DaemonHandoff)?;
            if kind == KIND_READY {
                HandoffMessage::Ready {
                    supervisor_pid: pid,
                }
            } else {
                HandoffMessage::Ack {
                    supervisor_pid: pid,
                }
            }
        }
        KIND_FAILED
            if payload
                .get(1..)
                .is_some_and(|reserved| reserved.iter().all(|byte| *byte == 0)) =>
        {
            HandoffMessage::Failed {
                category: FailureCategory::parse(
                    *payload.first().ok_or(LauncherError::DaemonHandoff)?,
                )?,
            }
        }
        _ => return Err(LauncherError::DaemonHandoff),
    };
    Ok(HandoffFrame { sequence, message })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum FailureCategory {
    Bundle = 1,
    Policy = 2,
    Spawn = 3,
    Identity = 4,
    Session = 5,
    Runtime = 6,
    Other = 7,
}

impl FailureCategory {
    const fn from_error(error: LauncherError) -> Self {
        match error {
            LauncherError::InvalidBundleLayout
            | LauncherError::InvalidBundleEntry
            | LauncherError::InvalidBundleSignature => Self::Bundle,
            LauncherError::InvalidGrantInput
            | LauncherError::InvalidLaunchPolicy
            | LauncherError::UnsupportedJailerIsolation(_)
            | LauncherError::GrantPreparation => Self::Policy,
            LauncherError::WorkerSpawn(_) | LauncherError::SessionSetup(_) => Self::Spawn,
            LauncherError::InvalidWorkerIdentity | LauncherError::InvalidDaemonIdentity => {
                Self::Identity
            }
            LauncherError::SessionProtocol
            | LauncherError::GrantProtocol
            | LauncherError::SocketBroker => Self::Session,
            LauncherError::WorkerPolicy | LauncherError::RuntimeNamespace => Self::Runtime,
            LauncherError::SignalSetup(_)
            | LauncherError::DaemonHandoff
            | LauncherError::WorkerWait(_)
            | LauncherError::SignalForward(_)
            | LauncherError::UnsupportedPlatform => Self::Other,
        }
    }

    fn parse(value: u8) -> Result<Self, LauncherError> {
        match value {
            1 => Ok(Self::Bundle),
            2 => Ok(Self::Policy),
            3 => Ok(Self::Spawn),
            4 => Ok(Self::Identity),
            5 => Ok(Self::Session),
            6 => Ok(Self::Runtime),
            7 => Ok(Self::Other),
            _ => Err(LauncherError::DaemonHandoff),
        }
    }

    const fn error(self) -> LauncherError {
        match self {
            Self::Bundle => LauncherError::InvalidBundleSignature,
            Self::Policy => LauncherError::InvalidLaunchPolicy,
            Self::Spawn => LauncherError::WorkerSpawn(io::ErrorKind::Other),
            Self::Identity => LauncherError::InvalidWorkerIdentity,
            Self::Session => LauncherError::SessionProtocol,
            Self::Runtime => LauncherError::RuntimeNamespace,
            Self::Other => LauncherError::DaemonHandoff,
        }
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, LauncherError> {
    let value: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or(LauncherError::DaemonHandoff)?
        .try_into()
        .map_err(|_| LauncherError::DaemonHandoff)?;
    Ok(u16::from_be_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LauncherError> {
    let value: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(LauncherError::DaemonHandoff)?
        .try_into()
        .map_err(|_| LauncherError::DaemonHandoff)?;
    Ok(u32::from_be_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, LauncherError> {
    let value: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or(LauncherError::DaemonHandoff)?
        .try_into()
        .map_err(|_| LauncherError::DaemonHandoff)?;
    Ok(u64::from_be_bytes(value))
}

#[derive(Default)]
struct HandoffDecoder {
    bytes: Vec<u8>,
}

impl HandoffDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<(), LauncherError> {
        if self.bytes.len().saturating_add(bytes.len()) > FRAME_BYTES {
            return Err(LauncherError::DaemonHandoff);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn take(&mut self) -> Result<Option<HandoffFrame>, LauncherError> {
        if self.bytes.len() < FRAME_BYTES {
            return Ok(None);
        }
        let frame = decode_frame(&self.bytes)?;
        self.bytes.clear();
        Ok(Some(frame))
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

struct DaemonSignals {
    received: Arc<AtomicBool>,
    registrations: [SigId; 2],
}

impl DaemonSignals {
    fn install() -> Result<Self, LauncherError> {
        let received = Arc::new(AtomicBool::new(false));
        let interrupt = signal_hook::flag::register(SIGINT, Arc::clone(&received))
            .map_err(|_| LauncherError::DaemonHandoff)?;
        let terminate = match signal_hook::flag::register(SIGTERM, Arc::clone(&received)) {
            Ok(registration) => registration,
            Err(_) => {
                signal_hook::low_level::unregister(interrupt);
                return Err(LauncherError::DaemonHandoff);
            }
        };
        Ok(Self {
            received,
            registrations: [interrupt, terminate],
        })
    }

    fn received(&self) -> bool {
        self.received.load(Ordering::Acquire)
    }
}

impl Drop for DaemonSignals {
    fn drop(&mut self) {
        for registration in self.registrations {
            signal_hook::low_level::unregister(registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_closed_frame_round_trips_and_redacts() {
        let messages = [
            HandoffMessage::Hello,
            HandoffMessage::Start {
                monotonic_us: 1_234_567_891,
                parent_cpu_us: 1_234_567_893,
            },
            HandoffMessage::Ready { supervisor_pid: 42 },
            HandoffMessage::Ack { supervisor_pid: 42 },
            HandoffMessage::Failed {
                category: FailureCategory::Session,
            },
        ];
        for message in messages {
            let frame = HandoffFrame {
                sequence: u64::from(!matches!(message, HandoffMessage::Hello)),
                message,
            };
            let encoded = encode_frame(frame).expect("frame should encode");
            assert_eq!(decode_frame(&encoded), Ok(frame));
            let debug = format!("{frame:?}");
            assert!(!debug.contains("1234567891"));
            assert!(!debug.contains("1234567893"));
            assert!(!debug.contains("42"));
        }
    }

    #[test]
    fn rejects_magic_version_kind_reserved_pid_and_trailing_bytes() {
        let frame = HandoffFrame {
            sequence: 0,
            message: HandoffMessage::Hello,
        };
        for (offset, replacement) in [
            (0, vec![b'X']),
            (4, 2_u16.to_be_bytes().to_vec()),
            (6, 99_u16.to_be_bytes().to_vec()),
            (16, vec![1]),
        ] {
            let mut encoded = encode_frame(frame).expect("frame should encode");
            encoded[offset..offset + replacement.len()].copy_from_slice(&replacement);
            assert_eq!(decode_frame(&encoded), Err(LauncherError::DaemonHandoff));
        }
        let mut oversized = encode_frame(frame).expect("frame should encode").to_vec();
        oversized.push(0);
        assert_eq!(decode_frame(&oversized), Err(LauncherError::DaemonHandoff));
        assert_eq!(
            encode_frame(HandoffFrame {
                sequence: 1,
                message: HandoffMessage::Ready { supervisor_pid: 0 },
            }),
            Err(LauncherError::DaemonHandoff)
        );
    }

    #[test]
    fn decoder_accepts_every_split_and_rejects_coalescing() {
        let frame = HandoffFrame {
            sequence: 1,
            message: HandoffMessage::Ack { supervisor_pid: 77 },
        };
        let encoded = encode_frame(frame).expect("frame should encode");
        for split in 0..=encoded.len() {
            let mut decoder = HandoffDecoder::default();
            decoder.push(&encoded[..split]).expect("prefix should fit");
            assert_eq!(
                decoder.take().expect("prefix should decode"),
                (split == encoded.len()).then_some(frame)
            );
            if split != encoded.len() {
                decoder.push(&encoded[split..]).expect("suffix should fit");
                assert_eq!(decoder.take().expect("frame should decode"), Some(frame));
            }
        }
        let mut decoder = HandoffDecoder::default();
        let mut doubled = encoded.to_vec();
        doubled.extend_from_slice(&encoded);
        assert_eq!(decoder.push(&doubled), Err(LauncherError::DaemonHandoff));
    }

    #[test]
    fn acknowledged_notifier_closes_the_bootstrap_transport() {
        let (stream, mut parent) = UnixStream::pair().expect("handoff pair should open");
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut notifier = DaemonNotifier::new(stream, deadline).expect("notifier should open");
        let descriptor = notifier.as_raw_fd().expect("transport should be live");
        notifier.notify_ready(42).expect("Ready should be written");
        assert_eq!(
            read_frame_until(&mut parent, deadline, || Ok(())).expect("Ready should decode"),
            HandoffFrame {
                sequence: 1,
                message: HandoffMessage::Ready { supervisor_pid: 42 },
            }
        );
        send_frame(
            &mut parent,
            HandoffFrame {
                sequence: 1,
                message: HandoffMessage::Ack { supervisor_pid: 42 },
            },
        )
        .expect("Ack should write");
        assert_eq!(
            notifier.drain().expect("Ack should decode"),
            NotifierEvent::Acknowledged
        );
        notifier.close_transport();
        assert_eq!(notifier.as_raw_fd(), Err(LauncherError::DaemonHandoff));
        // SAFETY: This reads flags from the formerly owned descriptor number.
        assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn notifier_rejects_ack_bytes_sent_before_ready() {
        let (stream, mut parent) = UnixStream::pair().expect("handoff pair should open");
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut notifier = DaemonNotifier::new(stream, deadline).expect("notifier should open");
        let encoded = encode_frame(HandoffFrame {
            sequence: 1,
            message: HandoffMessage::Ack { supervisor_pid: 42 },
        })
        .expect("Ack should encode");
        parent
            .write_all(&encoded[..1])
            .expect("early Ack byte should write");

        assert_eq!(notifier.notify_ready(42), Err(LauncherError::DaemonHandoff));
    }
}
