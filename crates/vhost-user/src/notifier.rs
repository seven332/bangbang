//! Portable directional eight-byte pipe notifiers.

use std::fmt;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use crate::error::VhostUserNotifierError;

const NOTIFICATION_BYTES: usize = 8;
const DRAIN_UNITS: usize = 64;

/// Outcome of one frontend-to-backend queue kick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KickSignalOutcome {
    /// One complete eight-byte notification was written.
    Signaled,
    /// The nonblocking pipe was full, so an existing wakeup coalesces this one.
    Coalesced,
}

/// Outcome of draining backend-to-frontend queue calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDrainOutcome {
    /// At least one complete notification was drained before the pipe emptied.
    Drained(u64),
    /// No notification was currently available.
    WouldBlock,
    /// The backend endpoint closed; the count was drained before EOF.
    Closed(u64),
}

/// Frontend-owned writer used to kick a backend queue.
pub struct KickNotifier {
    descriptor: OwnedFd,
}

/// Backend-facing reader transferred for queue kicks.
pub struct BackendKickEndpoint {
    descriptor: OwnedFd,
}

/// Frontend-owned reader used to receive backend queue calls.
pub struct CallNotifier {
    descriptor: OwnedFd,
}

/// Backend-facing writer transferred for queue calls.
pub struct BackendCallEndpoint {
    descriptor: OwnedFd,
}

macro_rules! redacted_debug {
    ($type_name:ty, $label:literal) => {
        impl fmt::Debug for $type_name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_tuple($label).field(&"redacted").finish()
            }
        }
    };
}

redacted_debug!(KickNotifier, "KickNotifier");
redacted_debug!(BackendKickEndpoint, "BackendKickEndpoint");
redacted_debug!(CallNotifier, "CallNotifier");
redacted_debug!(BackendCallEndpoint, "BackendCallEndpoint");

/// Creates a directional queue-kick pipe.
pub fn create_kick_notifier() -> Result<(KickNotifier, BackendKickEndpoint), VhostUserNotifierError>
{
    let (reader, writer) = create_pipe()?;
    Ok((
        KickNotifier { descriptor: writer },
        BackendKickEndpoint { descriptor: reader },
    ))
}

/// Creates a directional backend-call pipe.
pub fn create_call_notifier() -> Result<(CallNotifier, BackendCallEndpoint), VhostUserNotifierError>
{
    let (reader, writer) = create_pipe()?;
    Ok((
        CallNotifier { descriptor: reader },
        BackendCallEndpoint { descriptor: writer },
    ))
}

impl KickNotifier {
    /// Writes one exact protocol-permitted eight-byte notification.
    pub fn signal(&self) -> Result<KickSignalOutcome, VhostUserNotifierError> {
        write_notification(self.descriptor.as_raw_fd())
    }

    /// Creates an independently owned duplicate of the frontend endpoint.
    pub fn try_clone(&self) -> Result<Self, VhostUserNotifierError> {
        Ok(Self {
            descriptor: duplicate(self.descriptor.as_raw_fd(), true)?,
        })
    }
}

impl BackendKickEndpoint {
    /// Creates an independently owned duplicate for descriptor transfer.
    pub fn try_clone(&self) -> Result<Self, VhostUserNotifierError> {
        Ok(Self {
            descriptor: duplicate(self.descriptor.as_raw_fd(), false)?,
        })
    }
}

impl CallNotifier {
    /// Drains all currently available complete eight-byte notifications.
    pub fn drain(&self) -> Result<CallDrainOutcome, VhostUserNotifierError> {
        let mut buffer = [0_u8; NOTIFICATION_BYTES * DRAIN_UNITS];
        let mut notifications = 0_u64;
        loop {
            // SAFETY: The descriptor is a live nonblocking pipe reader and the
            // buffer is writable for its complete declared length.
            let result = unsafe {
                libc::read(
                    self.descriptor.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if result == 0 {
                return Ok(CallDrainOutcome::Closed(notifications));
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock if notifications == 0 => {
                        return Ok(CallDrainOutcome::WouldBlock);
                    }
                    io::ErrorKind::WouldBlock => {
                        return Ok(CallDrainOutcome::Drained(notifications));
                    }
                    kind => return Err(VhostUserNotifierError::Io(kind)),
                }
            }
            let bytes =
                usize::try_from(result).map_err(|_| VhostUserNotifierError::InvalidNotification)?;
            if bytes == 0 || bytes % NOTIFICATION_BYTES != 0 {
                return Err(VhostUserNotifierError::InvalidNotification);
            }
            notifications = notifications
                .checked_add(
                    u64::try_from(bytes / NOTIFICATION_BYTES)
                        .map_err(|_| VhostUserNotifierError::InvalidNotification)?,
                )
                .ok_or(VhostUserNotifierError::InvalidNotification)?;
        }
    }

    /// Creates an independently owned duplicate of the frontend endpoint.
    pub fn try_clone(&self) -> Result<Self, VhostUserNotifierError> {
        Ok(Self {
            descriptor: duplicate(self.descriptor.as_raw_fd(), false)?,
        })
    }
}

impl BackendCallEndpoint {
    /// Creates an independently owned duplicate for descriptor transfer.
    pub fn try_clone(&self) -> Result<Self, VhostUserNotifierError> {
        Ok(Self {
            descriptor: duplicate(self.descriptor.as_raw_fd(), true)?,
        })
    }
}

impl AsFd for KickNotifier {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }
}

impl AsFd for BackendKickEndpoint {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }
}

impl AsFd for CallNotifier {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }
}

impl AsFd for BackendCallEndpoint {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }
}

fn write_notification(descriptor: RawFd) -> Result<KickSignalOutcome, VhostUserNotifierError> {
    let notification = [0_u8; NOTIFICATION_BYTES];
    loop {
        // SAFETY: The descriptor is a live configured pipe writer and the
        // fixed notification buffer is readable for exactly eight bytes.
        let result =
            unsafe { libc::write(descriptor, notification.as_ptr().cast(), notification.len()) };
        if result < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(KickSignalOutcome::Coalesced),
                kind => return Err(VhostUserNotifierError::Io(kind)),
            }
        }
        return if usize::try_from(result).ok() == Some(NOTIFICATION_BYTES) {
            Ok(KickSignalOutcome::Signaled)
        } else {
            Err(VhostUserNotifierError::InvalidNotification)
        };
    }
}

fn create_pipe() -> Result<(OwnedFd, OwnedFd), VhostUserNotifierError> {
    let mut descriptors = [0_i32; 2];
    // SAFETY: `descriptors` has exactly the two writable entries required by
    // pipe. Successful returned descriptors are adopted immediately below.
    let result = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
    if result != 0 {
        return Err(VhostUserNotifierError::Io(
            io::Error::last_os_error().kind(),
        ));
    }
    let reader_raw = descriptors
        .first()
        .copied()
        .ok_or(VhostUserNotifierError::InvalidNotification)?;
    let writer_raw = descriptors
        .get(1)
        .copied()
        .ok_or(VhostUserNotifierError::InvalidNotification)?;
    // SAFETY: A successful pipe call returned two new descriptors, each
    // adopted exactly once by these owners.
    let reader = unsafe { OwnedFd::from_raw_fd(reader_raw) };
    // SAFETY: See the successful pipe ownership argument above.
    let writer = unsafe { OwnedFd::from_raw_fd(writer_raw) };
    configure_descriptor(reader.as_raw_fd(), false)?;
    configure_descriptor(writer.as_raw_fd(), true)?;
    Ok((reader, writer))
}

fn configure_descriptor(descriptor: RawFd, writer: bool) -> Result<(), VhostUserNotifierError> {
    let status = retry_fcntl(descriptor, libc::F_GETFL, 0)?;
    retry_fcntl(descriptor, libc::F_SETFL, status | libc::O_NONBLOCK)?;
    let descriptor_flags = retry_fcntl(descriptor, libc::F_GETFD, 0)?;
    retry_fcntl(
        descriptor,
        libc::F_SETFD,
        descriptor_flags | libc::FD_CLOEXEC,
    )?;
    if writer {
        suppress_pipe_sigpipe(descriptor)?;
    }
    Ok(())
}

fn duplicate(descriptor: RawFd, writer: bool) -> Result<OwnedFd, VhostUserNotifierError> {
    let duplicate = retry_fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0)?;
    // SAFETY: F_DUPFD_CLOEXEC returned one new descriptor which is adopted
    // exactly once here.
    let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
    configure_descriptor(duplicate.as_raw_fd(), writer)?;
    Ok(duplicate)
}

fn retry_fcntl(
    descriptor: RawFd,
    command: libc::c_int,
    argument: libc::c_int,
) -> Result<i32, VhostUserNotifierError> {
    loop {
        // SAFETY: Every caller supplies an integer fcntl operation valid for a
        // live borrowed descriptor and no pointer argument.
        let result = unsafe { libc::fcntl(descriptor, command, argument) };
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(VhostUserNotifierError::Io(error.kind()));
        }
    }
}

#[cfg(target_vendor = "apple")]
fn suppress_pipe_sigpipe(descriptor: RawFd) -> Result<(), VhostUserNotifierError> {
    // Darwin's public `<sys/fcntl.h>` defines F_SETNOSIGPIPE as command 73;
    // libc exposes SO_NOSIGPIPE but not this pipe-capable fcntl command.
    const DARWIN_F_SETNOSIGPIPE: libc::c_int = 73;
    retry_fcntl(descriptor, DARWIN_F_SETNOSIGPIPE, 1).map(|_| ())
}

#[cfg(not(target_vendor = "apple"))]
fn suppress_pipe_sigpipe(_descriptor: RawFd) -> Result<(), VhostUserNotifierError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(target_vendor = "apple")]
    use std::mem::MaybeUninit;

    use super::*;

    fn write_raw(descriptor: RawFd, bytes: &[u8]) -> io::Result<usize> {
        // SAFETY: The borrowed descriptor is live and the byte slice remains
        // readable for the synchronous write.
        let result = unsafe { libc::write(descriptor, bytes.as_ptr().cast(), bytes.len()) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
        }
    }

    fn read_raw(descriptor: RawFd, bytes: &mut [u8]) -> io::Result<usize> {
        // SAFETY: The borrowed descriptor is live and the byte slice remains
        // writable for the synchronous read.
        let result = unsafe { libc::read(descriptor, bytes.as_mut_ptr().cast(), bytes.len()) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
        }
    }

    #[test]
    fn kick_direction_writes_exact_eight_byte_units() {
        let (frontend, backend) = create_kick_notifier().expect("pipe should open");
        assert_eq!(frontend.signal(), Ok(KickSignalOutcome::Signaled));
        let mut bytes = [1_u8; NOTIFICATION_BYTES];
        assert_eq!(
            read_raw(backend.as_fd().as_raw_fd(), &mut bytes).expect("kick should read"),
            NOTIFICATION_BYTES
        );
        assert_eq!(bytes, [0; NOTIFICATION_BYTES]);
    }

    #[test]
    fn call_direction_drains_complete_units_and_reports_empty() {
        let (frontend, backend) = create_call_notifier().expect("pipe should open");
        assert_eq!(frontend.drain(), Ok(CallDrainOutcome::WouldBlock));
        let notifications = [7_u8; NOTIFICATION_BYTES * 3];
        assert_eq!(
            write_raw(backend.as_fd().as_raw_fd(), &notifications).expect("calls should write"),
            notifications.len()
        );
        assert_eq!(frontend.drain(), Ok(CallDrainOutcome::Drained(3)));
        assert_eq!(frontend.drain(), Ok(CallDrainOutcome::WouldBlock));
    }

    #[test]
    fn call_drain_rejects_partial_units_and_reports_eof() {
        let (frontend, backend) = create_call_notifier().expect("pipe should open");
        write_raw(backend.as_fd().as_raw_fd(), &[1]).expect("partial call should write");
        assert_eq!(
            frontend.drain(),
            Err(VhostUserNotifierError::InvalidNotification)
        );
        drop(backend);
        assert_eq!(frontend.drain(), Ok(CallDrainOutcome::Closed(0)));
    }

    #[test]
    fn kick_saturation_coalesces_and_closed_reader_is_typed() {
        let (frontend, backend) = create_kick_notifier().expect("pipe should open");
        let mut signaled = 0_u64;
        while let KickSignalOutcome::Signaled = frontend.signal().expect("open pipe should signal")
        {
            signaled += 1;
        }
        assert!(signaled > 0);
        drop(backend);
        assert!(matches!(
            frontend.signal(),
            Err(VhostUserNotifierError::Io(io::ErrorKind::BrokenPipe))
        ));
    }

    #[test]
    fn duplicates_are_independently_owned_and_configured() {
        let (frontend, backend) = create_kick_notifier().expect("pipe should open");
        let duplicate = backend.try_clone().expect("endpoint should duplicate");
        drop(backend);
        frontend
            .signal()
            .expect("duplicate reader should keep pipe live");
        let flags = retry_fcntl(duplicate.as_fd().as_raw_fd(), libc::F_GETFD, 0)
            .expect("flags should read");
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn debug_output_redacts_descriptor_numbers() {
        let (kick, backend_kick) = create_kick_notifier().expect("pipe should open");
        let (call, backend_call) = create_call_notifier().expect("pipe should open");
        for debug in [
            format!("{kick:?}"),
            format!("{backend_kick:?}"),
            format!("{call:?}"),
            format!("{backend_call:?}"),
        ] {
            assert!(debug.contains("redacted"));
        }
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn call_pipe_is_kqueue_readable_and_clears_after_drain() {
        let (frontend, backend) = create_call_notifier().expect("pipe should open");
        // SAFETY: kqueue creates one new descriptor with no pointer arguments.
        let kqueue = unsafe { libc::kqueue() };
        assert!(kqueue >= 0);
        // SAFETY: Successful kqueue returned one new descriptor adopted once.
        let kqueue = unsafe { OwnedFd::from_raw_fd(kqueue) };
        let change = libc::kevent {
            ident: usize::try_from(frontend.as_fd().as_raw_fd())
                .expect("descriptor should be nonnegative"),
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: One initialized change is readable; no output list is used.
        let registered = unsafe {
            libc::kevent(
                kqueue.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(registered, 0);
        write_raw(backend.as_fd().as_raw_fd(), &[0; NOTIFICATION_BYTES])
            .expect("backend should signal");

        // SAFETY: A zeroed kevent is valid output storage initialized by the
        // kernel before it is read after a successful count.
        let mut event: libc::kevent = unsafe { MaybeUninit::zeroed().assume_init() };
        let timeout = libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        // SAFETY: One live output event and timeout are provided for this wait.
        let ready = unsafe {
            libc::kevent(
                kqueue.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut event,
                1,
                &raw const timeout,
            )
        };
        assert_eq!(ready, 1);
        assert_eq!(event.filter, libc::EVFILT_READ);
        assert_eq!(frontend.drain(), Ok(CallDrainOutcome::Drained(1)));

        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: The same live output event is reused for a nonblocking poll.
        let residual = unsafe {
            libc::kevent(
                kqueue.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut event,
                1,
                &raw const zero,
            )
        };
        assert_eq!(residual, 0);
    }
}
