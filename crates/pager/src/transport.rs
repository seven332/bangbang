use std::fmt;
use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::frame::declared_frame;
use crate::{HEADER_BYTES, PagerError, PagerFrame, decode_frame, encode_frame};

/// Absolute-deadline transport over one already connected Unix stream.
pub struct PagerTransport {
    stream: UnixStream,
    timeout: Duration,
    poisoned: bool,
}

impl PagerTransport {
    /// Adopts one connected stream, makes it nonblocking, and suppresses SIGPIPE.
    pub fn new(stream: UnixStream, timeout: Duration) -> Result<Self, PagerError> {
        if timeout.is_zero() {
            return Err(PagerError::InvalidConfiguration);
        }
        stream
            .set_nonblocking(true)
            .map_err(|error| PagerError::Io(error.kind()))?;
        suppress_socket_sigpipe(stream.as_raw_fd())
            .map_err(|error| PagerError::Io(error.kind()))?;
        Ok(Self {
            stream,
            timeout,
            poisoned: false,
        })
    }

    /// Returns the complete-operation timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns whether an earlier failure made stream framing terminal.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Sends exactly one frame under one absolute deadline.
    pub fn send(&mut self, frame: &PagerFrame) -> Result<(), PagerError> {
        self.require_live()?;
        let encoded = match encode_frame(frame) {
            Ok(encoded) => encoded,
            Err(error) => return self.poison(error),
        };
        let deadline = match self.deadline() {
            Ok(deadline) => deadline,
            Err(error) => return self.poison(error),
        };
        match send_exact(self.stream.as_raw_fd(), &encoded, deadline) {
            Ok(()) => Ok(()),
            Err(error) => self.poison(error),
        }
    }

    /// Receives exactly one header-first frame under one absolute deadline.
    pub fn receive(&mut self) -> Result<PagerFrame, PagerError> {
        self.require_live()?;
        let deadline = match self.deadline() {
            Ok(deadline) => deadline,
            Err(error) => return self.poison(error),
        };
        let mut header = [0_u8; HEADER_BYTES];
        if let Err(error) = receive_exact(self.stream.as_raw_fd(), &mut header, deadline, false) {
            return self.poison(error);
        }
        let (_, total) = match declared_frame(&header) {
            Ok(declared) => declared,
            Err(error) => return self.poison(error),
        };
        let mut encoded = vec![0_u8; total];
        let target = match encoded.get_mut(..HEADER_BYTES) {
            Some(target) => target,
            None => return self.poison(PagerError::InvalidFrame),
        };
        target.copy_from_slice(&header);
        if total > HEADER_BYTES {
            let body = match encoded.get_mut(HEADER_BYTES..) {
                Some(body) => body,
                None => return self.poison(PagerError::InvalidFrame),
            };
            if let Err(error) = receive_exact(self.stream.as_raw_fd(), body, deadline, true) {
                return self.poison(error);
            }
        }
        match decode_frame(&encoded) {
            Ok(frame) => Ok(frame),
            Err(error) => self.poison(error),
        }
    }

    fn require_live(&self) -> Result<(), PagerError> {
        if self.poisoned {
            Err(PagerError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn deadline(&self) -> Result<Instant, PagerError> {
        Instant::now()
            .checked_add(self.timeout)
            .ok_or(PagerError::InvalidConfiguration)
    }

    fn poison<T>(&mut self, error: PagerError) -> Result<T, PagerError> {
        self.poisoned = true;
        Err(error)
    }
}

impl fmt::Debug for PagerTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PagerTransport")
            .field("stream", &"<redacted>")
            .field("timeout", &self.timeout)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

fn send_exact(socket: RawFd, bytes: &[u8], deadline: Instant) -> Result<(), PagerError> {
    let mut sent = 0_usize;
    while sent < bytes.len() {
        wait_ready(socket, libc::POLLOUT, deadline)?;
        let remaining = bytes.get(sent..).ok_or(PagerError::InvalidFrame)?;
        // SAFETY: `remaining` is a live readable byte slice and `socket`
        // remains owned by the caller for this synchronous send.
        let result = unsafe {
            libc::send(
                socket,
                remaining.as_ptr().cast(),
                remaining.len(),
                send_flags(),
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
                io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset => return Err(PagerError::Disconnected),
                kind => return Err(PagerError::Io(kind)),
            }
        }
        let transferred = usize::try_from(result).map_err(|_| PagerError::InvalidFrame)?;
        if transferred == 0 || transferred > remaining.len() {
            return Err(PagerError::InvalidFrame);
        }
        sent = sent
            .checked_add(transferred)
            .ok_or(PagerError::InvalidFrame)?;
    }
    Ok(())
}

fn receive_exact(
    socket: RawFd,
    bytes: &mut [u8],
    deadline: Instant,
    frame_started: bool,
) -> Result<(), PagerError> {
    let mut received = 0_usize;
    while received < bytes.len() {
        if let Err(error) = wait_ready(socket, libc::POLLIN, deadline) {
            return if error == PagerError::Disconnected && (frame_started || received != 0) {
                Err(PagerError::UnexpectedEof)
            } else {
                Err(error)
            };
        }
        let remaining = bytes.get_mut(received..).ok_or(PagerError::InvalidFrame)?;
        // SAFETY: `remaining` is a live writable byte slice and `socket`
        // remains owned by the caller for this synchronous receive.
        let result =
            unsafe { libc::recv(socket, remaining.as_mut_ptr().cast(), remaining.len(), 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
                io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset => {
                    return if frame_started || received != 0 {
                        Err(PagerError::UnexpectedEof)
                    } else {
                        Err(PagerError::Disconnected)
                    };
                }
                kind => return Err(PagerError::Io(kind)),
            }
        }
        let transferred = usize::try_from(result).map_err(|_| PagerError::InvalidFrame)?;
        if transferred == 0 {
            return if frame_started || received != 0 {
                Err(PagerError::UnexpectedEof)
            } else {
                Err(PagerError::Disconnected)
            };
        }
        if transferred > remaining.len() {
            return Err(PagerError::InvalidFrame);
        }
        received = received
            .checked_add(transferred)
            .ok_or(PagerError::InvalidFrame)?;
    }
    Ok(())
}

fn wait_ready(
    descriptor: RawFd,
    interest: libc::c_short,
    deadline: Instant,
) -> Result<(), PagerError> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(PagerError::Timeout);
        }
        let remaining = deadline.saturating_duration_since(now);
        let whole_millis = remaining.as_millis();
        let rounded_millis = if remaining.subsec_nanos().is_multiple_of(1_000_000) {
            whole_millis
        } else {
            whole_millis.saturating_add(1)
        };
        let timeout = i32::try_from(rounded_millis).unwrap_or(i32::MAX).max(1);
        let mut poll_descriptor = libc::pollfd {
            fd: descriptor,
            events: interest,
            revents: 0,
        };
        // SAFETY: `poll_descriptor` is one initialized writable entry and
        // does not escape this synchronous call.
        let result = unsafe { libc::poll(&raw mut poll_descriptor, 1, timeout) };
        if result == 0 {
            return Err(PagerError::Timeout);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(PagerError::Io(error.kind()));
        }
        if poll_descriptor.revents & interest != 0 {
            return Ok(());
        }
        if poll_descriptor.revents & libc::POLLNVAL != 0 {
            return Err(PagerError::Io(io::ErrorKind::InvalidInput));
        }
        if poll_descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(PagerError::Disconnected);
        }
        return Err(PagerError::InvalidFrame);
    }
}

#[cfg(target_vendor = "apple")]
fn suppress_socket_sigpipe(descriptor: RawFd) -> io::Result<()> {
    let enabled: libc::c_int = 1;
    // SAFETY: The option pointer references one initialized integer for this
    // synchronous setsockopt call on the owned Unix stream descriptor.
    let result = unsafe {
        libc::setsockopt(
            descriptor,
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const enabled).cast(),
            size_of::<libc::c_int>()
                .try_into()
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_vendor = "apple"))]
fn suppress_socket_sigpipe(_descriptor: RawFd) -> io::Result<()> {
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn send_flags() -> libc::c_int {
    libc::MSG_NOSIGNAL
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
const fn send_flags() -> libc::c_int {
    0
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::frame::PagerMessage;
    use crate::{MIN_PAGE_SIZE, PagerLimits, PagerOperations, PagerSessionId};

    fn hello() -> PagerFrame {
        PagerFrame::new(
            PagerSessionId::from_bytes([3; 32]).expect("session should be valid"),
            PagerMessage::Hello(
                PagerLimits::new(MIN_PAGE_SIZE, 1, 1, 8 * 1024, PagerOperations::v1())
                    .expect("limits should be valid"),
            ),
        )
    }

    #[test]
    fn real_stream_round_trips_one_frame() {
        let (left, right) = UnixStream::pair().expect("stream pair should open");
        let mut sender =
            PagerTransport::new(left, Duration::from_secs(1)).expect("sender should initialize");
        let mut receiver =
            PagerTransport::new(right, Duration::from_secs(1)).expect("receiver should initialize");
        let frame = hello();
        sender.send(&frame).expect("frame should send");
        assert_eq!(receiver.receive(), Ok(frame));
    }

    #[test]
    fn timeout_clean_eof_and_partial_eof_are_terminal() {
        let (left, _right) = UnixStream::pair().expect("stream pair should open");
        let mut timeout = PagerTransport::new(left, Duration::from_millis(10))
            .expect("transport should initialize");
        assert_eq!(timeout.receive(), Err(PagerError::Timeout));
        assert!(timeout.is_poisoned());
        assert_eq!(timeout.receive(), Err(PagerError::Poisoned));

        let (left, right) = UnixStream::pair().expect("stream pair should open");
        let mut clean =
            PagerTransport::new(left, Duration::from_secs(1)).expect("transport should initialize");
        drop(right);
        assert_eq!(clean.receive(), Err(PagerError::Disconnected));

        let (left, mut right) = UnixStream::pair().expect("stream pair should open");
        let mut partial =
            PagerTransport::new(left, Duration::from_secs(1)).expect("transport should initialize");
        let encoded = encode_frame(&hello()).expect("frame should encode");
        right
            .write_all(
                encoded
                    .get(..HEADER_BYTES + 1)
                    .expect("partial frame should exist"),
            )
            .expect("partial frame should write");
        drop(right);
        assert_eq!(partial.receive(), Err(PagerError::UnexpectedEof));
    }

    #[test]
    fn oversized_advertisement_and_broken_pipe_fail_without_allocation_or_signal() {
        let (left, mut right) = UnixStream::pair().expect("stream pair should open");
        let mut receiver =
            PagerTransport::new(left, Duration::from_secs(1)).expect("transport should initialize");
        let mut header = encode_frame(&hello())
            .expect("frame should encode")
            .get(..HEADER_BYTES)
            .expect("header should exist")
            .to_vec();
        header
            .get_mut(12..16)
            .expect("length field should exist")
            .copy_from_slice(&u32::MAX.to_be_bytes());
        right
            .write_all(&header)
            .expect("malformed header should write");
        assert_eq!(receiver.receive(), Err(PagerError::LimitExceeded));

        let (left, right) = UnixStream::pair().expect("stream pair should open");
        let mut sender =
            PagerTransport::new(left, Duration::from_secs(1)).expect("transport should initialize");
        drop(right);
        assert!(matches!(
            sender.send(&hello()),
            Err(PagerError::Disconnected) | Err(PagerError::Io(_))
        ));
    }

    #[test]
    fn debug_redacts_stream_identity() {
        let (left, _right) = UnixStream::pair().expect("stream pair should open");
        let transport =
            PagerTransport::new(left, Duration::from_secs(1)).expect("transport should initialize");
        let debug = format!("{transport:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("fd:"));
    }
}
