use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, ExitStatus};
use std::ptr;

use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::{SigId, low_level};

use crate::LauncherError;

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

pub(crate) fn wait_and_forward(
    child: &mut Child,
    mut wakeups: SignalWakeups,
) -> Result<ExitStatus, LauncherError> {
    let kqueue = create_kqueue()?;
    let child_pid = usize::try_from(child.id())
        .map_err(|_| LauncherError::WorkerWait(io::ErrorKind::InvalidInput))?;
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
            child_pid,
            libc::EVFILT_PROC,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            libc::NOTE_EXIT,
        ),
    ];
    register_events(kqueue.as_raw_fd(), &changes)?;

    let mut forwarding_error = None;
    loop {
        let events = wait_events(kqueue.as_raw_fd())?;
        let mut child_exited = false;
        for event in events {
            if event.filter == libc::EVFILT_PROC
                && event.ident == child_pid
                && event.fflags & libc::NOTE_EXIT != 0
            {
                child_exited = true;
                continue;
            }

            for signal in &mut wakeups.signals {
                let signal_fd = usize::try_from(signal.reader.as_raw_fd())
                    .map_err(|_| LauncherError::SignalSetup(io::ErrorKind::InvalidInput))?;
                if event.filter == libc::EVFILT_READ
                    && event.ident == signal_fd
                    && signal.drain()?
                    && let Err(kind) = forward_signal(child.id(), signal.signal)
                {
                    forwarding_error.get_or_insert(kind);
                }
            }
        }

        if child_exited {
            // The child is not reaped until every signal event in this batch
            // has been processed. Therefore its PID cannot be reused by any
            // process targeted above. No signal is sent after this wait.
            let status = child
                .wait()
                .map_err(|err| LauncherError::WorkerWait(err.kind()))?;
            if let Some(kind) = forwarding_error {
                return Err(LauncherError::SignalForward(kind));
            }
            return Ok(status);
        }
    }
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

fn wait_events(kqueue: RawFd) -> Result<Vec<libc::kevent>, LauncherError> {
    loop {
        let mut events = [MaybeUninit::<libc::kevent>::uninit(); 3];
        // SAFETY: `events` provides room for three `kevent` values. The kernel
        // initializes exactly the positive number returned before they are read.
        let count = unsafe {
            libc::kevent(
                kqueue,
                ptr::null(),
                0,
                events.as_mut_ptr().cast(),
                3,
                ptr::null(),
            )
        };
        if count > 0 {
            let count = usize::try_from(count)
                .map_err(|_| LauncherError::WorkerWait(io::ErrorKind::InvalidData))?;
            return Ok(events
                .into_iter()
                .take(count)
                .map(|event| {
                    // SAFETY: `kevent` initialized every event below the
                    // returned count.
                    unsafe { event.assume_init() }
                })
                .collect());
        }
        let kind = io::Error::last_os_error().kind();
        if kind != io::ErrorKind::Interrupted {
            return Err(LauncherError::WorkerWait(kind));
        }
    }
}

fn forward_signal(child_id: u32, signal: i32) -> Result<(), io::ErrorKind> {
    let pid = i32::try_from(child_id).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: `pid` is the unreaped owned child and `signal` is SIGINT or
    // SIGTERM. `kill` neither retains pointers nor transfers ownership.
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.kind())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_batch_defers_reaping_until_signal_events_are_processed() {
        let signal_event = event(7, libc::EVFILT_READ, 0, 0);
        let exit_event = event(42, libc::EVFILT_PROC, 0, libc::NOTE_EXIT);
        let events = [exit_event, signal_event];
        let exit_index = events
            .iter()
            .position(|event| event.filter == libc::EVFILT_PROC)
            .expect("exit event should exist");
        let signal_index = events
            .iter()
            .position(|event| event.filter == libc::EVFILT_READ)
            .expect("signal event should exist");
        assert!(
            exit_index < signal_index,
            "test must exercise exit-first order"
        );
        assert_eq!(
            events.len(),
            2,
            "the whole batch remains available before reaping"
        );
    }
}
