use std::fmt;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::frame::declared_frame;
use crate::{HEADER_BYTES, PagerError, PagerFrame, decode_frame, encode_frame};

// XNU accepts at most 512 descriptors in one control message; Linux accepts
// 253. One extra word covers the platform cmsghdr alignment.
const MAX_ANCILLARY_DESCRIPTORS: usize = 512;
const ANCILLARY_BUFFER_BYTES: usize =
    size_of::<libc::cmsghdr>() + MAX_ANCILLARY_DESCRIPTORS * size_of::<RawFd>() + size_of::<u64>();
const ANCILLARY_BUFFER_WORDS: usize = ANCILLARY_BUFFER_BYTES.div_ceil(size_of::<u64>());

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
        let (transferred, has_ancillary_data) = match receive_once(socket, remaining) {
            Ok(attempt) => attempt,
            Err(error) => match error.kind() {
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
                io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset => {
                    return if frame_started || received != 0 {
                        Err(PagerError::UnexpectedEof)
                    } else {
                        Err(PagerError::Disconnected)
                    };
                }
                io::ErrorKind::InvalidData => return Err(PagerError::InvalidFrame),
                kind => return Err(PagerError::Io(kind)),
            },
        };
        if has_ancillary_data {
            return Err(PagerError::InvalidFrame);
        }
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

fn receive_once(socket: RawFd, bytes: &mut [u8]) -> io::Result<(usize, bool)> {
    if bytes.is_empty() {
        return Ok((0, false));
    }
    let mut iovec = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_u64; ANCILLARY_BUFFER_WORDS];
    // SAFETY: An all-zero msghdr is a valid empty header. The live writable
    // payload and aligned bounded control buffers are installed below.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control
        .len()
        .checked_mul(size_of::<u64>())
        .ok_or_else(invalid_data)?
        .try_into()
        .map_err(|_| invalid_data())?;
    // SAFETY: The msghdr points only to the live writable payload slice for
    // this synchronous receive. The aligned control buffer covers the larger
    // supported-kernel descriptor limit, and received descriptors are closed
    // below before this function returns.
    let result = unsafe { libc::recvmsg(socket, &raw mut message, receive_flags()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let received =
        usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if received > bytes.len() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let has_ancillary_data = discard_ancillary_descriptors(&message, &control)?
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
    Ok((received, has_ancillary_data))
}

fn discard_ancillary_descriptors(message: &libc::msghdr, control: &[u64]) -> io::Result<bool> {
    let available = usize::try_from(message.msg_controllen).map_err(|_| invalid_data())?;
    let capacity = control
        .len()
        .checked_mul(size_of::<u64>())
        .ok_or_else(invalid_data)?;
    if available > capacity {
        return Err(invalid_data());
    }
    if available == 0 {
        return Ok(false);
    }

    // SAFETY: CMSG_LEN performs platform layout arithmetic for an empty
    // payload and dereferences no pointer.
    let header_bytes = usize::try_from(unsafe { libc::CMSG_LEN(0) }).map_err(|_| invalid_data())?;
    if header_bytes < size_of::<libc::cmsghdr>() || available < header_bytes {
        return Err(invalid_data());
    }

    let base = control.as_ptr().cast::<u8>();
    let mut offset = 0_usize;
    loop {
        let remaining = available.checked_sub(offset).ok_or_else(invalid_data)?;
        if remaining < size_of::<libc::cmsghdr>() {
            break;
        }
        // SAFETY: `offset` is bounded by `available`, which is bounded by the
        // aligned live control buffer. read_unaligned copies one cmsghdr.
        let header = unsafe { base.add(offset).cast::<libc::cmsghdr>().read_unaligned() };
        let declared_message_bytes =
            usize::try_from(header.cmsg_len).map_err(|_| invalid_data())?;
        if declared_message_bytes < header_bytes {
            return Err(invalid_data());
        }
        let message_was_truncated = declared_message_bytes > remaining;
        let message_bytes = declared_message_bytes.min(remaining);
        let payload_bytes = message_bytes
            .checked_sub(header_bytes)
            .ok_or_else(invalid_data)?;
        let mut malformed_rights = false;
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            if !payload_bytes.is_multiple_of(size_of::<RawFd>()) {
                malformed_rights = true;
            }
            let descriptor_count = payload_bytes / size_of::<RawFd>();
            for index in 0..descriptor_count {
                let descriptor_offset = offset
                    .checked_add(header_bytes)
                    .and_then(|value| {
                        index
                            .checked_mul(size_of::<RawFd>())
                            .and_then(|delta| value.checked_add(delta))
                    })
                    .ok_or_else(invalid_data)?;
                // SAFETY: The validated SCM_RIGHTS payload contains one
                // kernel-created RawFd at this checked in-buffer offset.
                let descriptor =
                    unsafe { base.add(descriptor_offset).cast::<RawFd>().read_unaligned() };
                if descriptor < 0 {
                    malformed_rights = true;
                    continue;
                }
                // SAFETY: recvmsg transferred ownership of this new,
                // nonnegative descriptor exactly once. Immediate drop closes
                // it because this protocol grants no descriptor authority.
                drop(unsafe { OwnedFd::from_raw_fd(descriptor) });
            }
        }
        if message_was_truncated || malformed_rights {
            return Err(invalid_data());
        }

        let payload = u32::try_from(payload_bytes).map_err(|_| invalid_data())?;
        // SAFETY: CMSG_SPACE performs platform layout arithmetic for the
        // already bounded payload and dereferences no pointer.
        let padded =
            usize::try_from(unsafe { libc::CMSG_SPACE(payload) }).map_err(|_| invalid_data())?;
        if padded < message_bytes {
            return Err(invalid_data());
        }
        let next = offset.checked_add(padded).ok_or_else(invalid_data)?;
        if next > available || available - next < size_of::<libc::cmsghdr>() {
            break;
        }
        offset = next;
    }
    Ok(true)
}

fn invalid_data() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
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

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn receive_flags() -> libc::c_int {
    libc::MSG_CMSG_CLOEXEC
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
const fn receive_flags() -> libc::c_int {
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
    fn ancillary_descriptors_are_closed_and_rejected() {
        assert_ancillary_descriptors_closed(0);
        assert_ancillary_descriptors_closed(HEADER_BYTES + 1);
    }

    fn assert_ancillary_descriptors_closed(prefix_len: usize) {
        let (left, mut right) = UnixStream::pair().expect("stream pair should open");
        let mut receiver =
            PagerTransport::new(left, Duration::from_secs(1)).expect("transport should initialize");
        let encoded = encode_frame(&hello()).expect("frame should encode");
        right
            .write_all(
                encoded
                    .get(..prefix_len)
                    .expect("plain frame prefix should exist"),
            )
            .expect("plain frame prefix should send");
        let (transferred, probe) = UnixStream::pair().expect("descriptor pair should open");
        suppress_socket_sigpipe(probe.as_raw_fd()).expect("probe should suppress SIGPIPE");
        let descriptors = [transferred.as_raw_fd(); 3];
        let descriptor_bytes = encoded
            .get(prefix_len..)
            .expect("descriptor-bearing suffix should exist");
        assert_eq!(
            send_with_descriptors(right.as_raw_fd(), descriptor_bytes, &descriptors)
                .expect("descriptor-bearing frame should send"),
            descriptor_bytes.len()
        );
        drop(transferred);
        assert_eq!(receiver.receive(), Err(PagerError::InvalidFrame));
        assert!(receiver.is_poisoned());

        let byte = [0_u8];
        // SAFETY: The borrowed probe descriptor and one-byte readable slice
        // remain live for this synchronous, SIGPIPE-suppressed send.
        let result = unsafe {
            libc::send(
                probe.as_raw_fd(),
                byte.as_ptr().cast(),
                byte.len(),
                send_flags(),
            )
        };
        assert_eq!(result, -1);
        assert!(matches!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
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

    fn send_with_descriptors(
        socket: RawFd,
        bytes: &[u8],
        descriptors: &[RawFd],
    ) -> io::Result<usize> {
        if descriptors.is_empty() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let mut iovec = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0_usize; 8];
        // SAFETY: An all-zero msghdr is valid before the live payload and
        // aligned control buffers are installed below.
        let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
        message.msg_iov = &raw mut iovec;
        message.msg_iovlen = 1;
        let descriptor_bytes = descriptors
            .len()
            .checked_mul(size_of::<RawFd>())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let descriptor_bytes = u32::try_from(descriptor_bytes)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: CMSG_SPACE performs platform layout arithmetic on the
        // bounded descriptor slice and dereferences no pointer.
        let control_space = unsafe { libc::CMSG_SPACE(descriptor_bytes) };
        let control_bytes = usize::try_from(control_space)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if control_bytes > control.len().saturating_mul(size_of::<usize>()) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control_bytes
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: The checked aligned control buffer has room for one
        // cmsghdr and the complete descriptor slice. All pointers remain live
        // for sendmsg.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            if header.is_null() {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(descriptor_bytes) as _;
            std::ptr::copy_nonoverlapping(
                descriptors.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(header),
                usize::try_from(descriptor_bytes)
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
            );
        }
        // SAFETY: The msghdr references only live readable payload/control
        // buffers and the descriptors remain borrowed by the caller.
        let result = unsafe { libc::sendmsg(socket, &raw const message, send_flags()) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
        }
    }
}
