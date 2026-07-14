use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr;
use std::time::{Duration, Instant};

use bangbang_session::macos::runtime::{LauncherNamespace, NamespaceIdentity};
use bangbang_session::{
    CancelSignal, Frame, FrameDecoder, LauncherLifecycle, Message, TerminalCategory, encode_frame,
};
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::{SigId, low_level};

use super::spawn::OwnedWorker;
use crate::LauncherError;

const CANCELLATION_GRACE: Duration = Duration::from_secs(5);
const DISCONNECT_GRACE: Duration = Duration::from_secs(5);
const BOOTSTRAP_HELLO_TIMEOUT: Duration = Duration::from_secs(5);

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
    stream
        .set_read_timeout(Some(BOOTSTRAP_HELLO_TIMEOUT))
        .map_err(|error| LauncherError::SessionSetup(error.kind()))?;
    let result = (|| {
        let mut decoder = FrameDecoder::default();
        let mut buffer = [0_u8; 4096];
        loop {
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
            let length = stream
                .read(&mut buffer)
                .map_err(|_| LauncherError::SessionProtocol)?;
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
    mut lifecycle: LauncherLifecycle,
    mut wakeups: SignalWakeups,
) -> Result<ExitStatus, LauncherError> {
    let session_id = lifecycle.session();
    let mut namespace = None;
    let mut terminal = None;
    let result = wait_session_inner(
        worker,
        session,
        &mut lifecycle,
        &mut wakeups,
        &mut namespace,
        &mut terminal,
    );
    if result.is_err() {
        let _ = session.shutdown(std::net::Shutdown::Both);
        worker.terminate_and_reap();
    }

    let cleanup = if let Some(namespace) = namespace.as_mut() {
        namespace.cleanup()
    } else {
        LauncherNamespace::recover_after_worker_exit(session_id)
            .and_then(|recovered| recovered.map_or(Ok(()), |mut namespace| namespace.cleanup()))
    }
    .map_err(|_| LauncherError::RuntimeNamespace);

    match result {
        Ok(status) => {
            cleanup?;
            validate_terminal(status, terminal)?;
            Ok(status)
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
}

fn wait_session_inner(
    worker: &mut OwnedWorker,
    session: &mut UnixStream,
    lifecycle: &mut LauncherLifecycle,
    wakeups: &mut SignalWakeups,
    namespace: &mut Option<LauncherNamespace>,
    terminal: &mut Option<(TerminalCategory, u8)>,
) -> Result<ExitStatus, LauncherError> {
    session
        .set_nonblocking(true)
        .map_err(|err| LauncherError::SessionSetup(err.kind()))?;
    let kqueue = create_kqueue()?;
    let child_pid = usize::try_from(worker.pid())
        .map_err(|_| LauncherError::WorkerWait(io::ErrorKind::InvalidInput))?;
    let session_fd = usize::try_from(session.as_raw_fd())
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
    ];
    register_events(kqueue.as_raw_fd(), &changes)?;

    let mut decoder = FrameDecoder::default();
    let mut cancellation_deadline: Option<Instant> = None;
    let mut disconnect_deadline: Option<Instant> = None;
    let mut child_exited;
    loop {
        let deadline = match (cancellation_deadline, disconnect_deadline) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        let timeout =
            deadline.and_then(|deadline: Instant| deadline.checked_duration_since(Instant::now()));
        let events = wait_events(kqueue.as_raw_fd(), timeout)?;
        child_exited = false;
        for queued in events {
            if queued.filter == libc::EVFILT_PROC
                && queued.ident == child_pid
                && queued.fflags & libc::NOTE_EXIT != 0
            {
                child_exited = true;
                continue;
            }
            if queued.filter == libc::EVFILT_READ && queued.ident == session_fd {
                if disconnect_deadline.is_none()
                    && drain_session(session, &mut decoder, lifecycle, namespace, terminal)?
                {
                    disconnect_deadline = Some(Instant::now() + DISCONNECT_GRACE);
                    register_events(
                        kqueue.as_raw_fd(),
                        &[event(session_fd, libc::EVFILT_READ, libc::EV_DELETE, 0)],
                    )?;
                }
                continue;
            }
            for signal in &mut wakeups.signals {
                let signal_fd = usize::try_from(signal.reader.as_raw_fd())
                    .map_err(|_| LauncherError::SignalSetup(io::ErrorKind::InvalidInput))?;
                if queued.filter == libc::EVFILT_READ
                    && queued.ident == signal_fd
                    && signal.drain()?
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
                    cancellation_deadline = Some(Instant::now() + CANCELLATION_GRACE);
                }
            }
        }

        if child_exited {
            // Drain all bytes made readable with this exit before reaping, so
            // terminal state cannot lose a same-batch race.
            let _ = drain_session(session, &mut decoder, lifecycle, namespace, terminal)?;
            break;
        }
        if disconnect_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(LauncherError::SessionProtocol);
        }
        if cancellation_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            worker
                .signal(libc::SIGKILL)
                .map_err(LauncherError::SignalForward)?;
            cancellation_deadline = None;
        }
    }

    worker.wait()
}

fn drain_session(
    session: &mut UnixStream,
    decoder: &mut FrameDecoder,
    lifecycle: &mut LauncherLifecycle,
    namespace: &mut Option<LauncherNamespace>,
    terminal: &mut Option<(TerminalCategory, u8)>,
) -> Result<bool, LauncherError> {
    let mut buffer = [0_u8; 4096];
    loop {
        match session.read(&mut buffer) {
            Ok(0) => {
                decoder
                    .finish()
                    .map_err(|_| LauncherError::SessionProtocol)?;
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
                    let message = lifecycle
                        .receive(frame)
                        .map_err(|_| LauncherError::SessionProtocol)?;
                    match message {
                        Message::Prepared { device, inode } => {
                            if namespace.is_some() {
                                return Err(LauncherError::SessionProtocol);
                            }
                            let validated = LauncherNamespace::validate(
                                lifecycle.session(),
                                NamespaceIdentity { device, inode },
                            )
                            .map_err(|_| LauncherError::RuntimeNamespace)?;
                            *namespace = Some(validated);
                            if !lifecycle.is_cancelled() {
                                let proceed = lifecycle
                                    .proceed()
                                    .map_err(|_| LauncherError::SessionProtocol)?;
                                write_frame(session, proceed)?;
                            }
                        }
                        Message::Starting | Message::Ready(_) => {}
                        Message::Terminal {
                            category,
                            exit_code,
                        } => {
                            if terminal.replace((category, exit_code)).is_some() {
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
    loop {
        let mut events = [MaybeUninit::<libc::kevent>::uninit(); 4];
        let timeout = timeout.map(duration_timespec);
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
                4,
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
    use super::*;

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
}
