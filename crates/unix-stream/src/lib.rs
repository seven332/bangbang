//! Deadline-bounded byte and descriptor transfer over connected Unix streams.
//!
//! This crate owns no protocol framing, socket path, listener, process, peer
//! authorization, or application semantics. Callers provide an already
//! connected stream and impose their own frame and descriptor contracts.

#[cfg(not(unix))]
compile_error!("bangbang-unix-stream requires Unix descriptor and socket semantics");

use std::fmt;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Largest descriptor array accepted by the shared transport primitive.
pub const MAX_ATTACHED_DESCRIPTORS: usize = 32;

const CONTROL_WORDS: usize = 32;

/// Redacted failure from exact Unix-stream transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixStreamTransportError {
    /// The caller or received transport shape is invalid.
    Invalid,
    /// The absolute operation deadline elapsed.
    Timeout,
    /// The peer closed before any requested byte arrived.
    Disconnected,
    /// The peer closed after transferring only a prefix.
    UnexpectedEof,
    /// A local socket operation failed.
    Io(io::ErrorKind),
}

impl fmt::Display for UnixStreamTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private Unix stream transport failure")
    }
}

impl std::error::Error for UnixStreamTransportError {}

/// Opaque absolute deadline shared by the pieces of one framed operation.
#[derive(Clone, Copy)]
pub struct UnixStreamDeadline(Instant);

impl fmt::Debug for UnixStreamDeadline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UnixStreamDeadline(<redacted>)")
    }
}

/// Exact received bytes and every descriptor attached within their range.
pub struct ReceivedBytes {
    bytes: Vec<u8>,
    descriptors: Vec<OwnedFd>,
}

impl ReceivedBytes {
    /// Borrows the exact received bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the number of owned received descriptors.
    #[must_use]
    pub fn descriptor_count(&self) -> usize {
        self.descriptors.len()
    }

    /// Separates the exact bytes from their owned received descriptors.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Vec<OwnedFd>) {
        (self.bytes, self.descriptors)
    }
}

impl fmt::Debug for ReceivedBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceivedBytes")
            .field("bytes", &"<redacted>")
            .field("descriptors", &"<owned>")
            .finish()
    }
}

/// Exact-I/O driver for one already-connected Unix stream.
pub struct UnixStreamTransport {
    stream: UnixStream,
    timeout: Duration,
}

impl UnixStreamTransport {
    /// Adopts and configures an already-connected Unix stream.
    pub fn new(stream: UnixStream, timeout: Duration) -> Result<Self, UnixStreamTransportError> {
        if timeout.is_zero() {
            return Err(UnixStreamTransportError::Invalid);
        }
        stream
            .set_nonblocking(true)
            .map_err(|error| UnixStreamTransportError::Io(error.kind()))?;
        suppress_socket_sigpipe(stream.as_raw_fd())
            .map_err(|error| UnixStreamTransportError::Io(error.kind()))?;
        Ok(Self { stream, timeout })
    }

    /// Creates one absolute deadline for a complete caller-defined operation.
    pub fn deadline(&self) -> Result<UnixStreamDeadline, UnixStreamTransportError> {
        Instant::now()
            .checked_add(self.timeout)
            .map(UnixStreamDeadline)
            .ok_or(UnixStreamTransportError::Invalid)
    }

    /// Sends one nonempty byte sequence and its borrowed descriptor array.
    pub fn send(
        &self,
        bytes: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) -> Result<(), UnixStreamTransportError> {
        let deadline = self.deadline()?;
        self.send_until(bytes, descriptors, deadline)
    }

    /// Sends under a previously created absolute deadline.
    pub fn send_until(
        &self,
        bytes: &[u8],
        descriptors: &[BorrowedFd<'_>],
        deadline: UnixStreamDeadline,
    ) -> Result<(), UnixStreamTransportError> {
        if bytes.is_empty() || descriptors.len() > MAX_ATTACHED_DESCRIPTORS {
            return Err(UnixStreamTransportError::Invalid);
        }
        let raw: Vec<RawFd> = descriptors.iter().map(AsRawFd::as_raw_fd).collect();
        let mut driver = SystemSendDriver;
        send_all_with_driver(
            &mut driver,
            self.stream.as_raw_fd(),
            bytes,
            &raw,
            deadline.0,
        )
    }

    /// Receives exactly `expected` bytes and at most `max_descriptors` rights.
    pub fn receive_exact(
        &self,
        expected: usize,
        max_descriptors: usize,
    ) -> Result<ReceivedBytes, UnixStreamTransportError> {
        let deadline = self.deadline()?;
        self.receive_exact_until(expected, max_descriptors, deadline)
    }

    /// Receives exact bytes under a previously created absolute deadline.
    pub fn receive_exact_until(
        &self,
        expected: usize,
        max_descriptors: usize,
        deadline: UnixStreamDeadline,
    ) -> Result<ReceivedBytes, UnixStreamTransportError> {
        if max_descriptors > MAX_ATTACHED_DESCRIPTORS {
            return Err(UnixStreamTransportError::Invalid);
        }
        if expected == 0 {
            return Ok(ReceivedBytes {
                bytes: Vec::new(),
                descriptors: Vec::new(),
            });
        }

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(expected)
            .map_err(|_| UnixStreamTransportError::Invalid)?;
        bytes.resize(expected, 0);
        let mut received = 0_usize;
        let mut descriptors = Vec::new();
        while received < expected {
            wait_ready(self.stream.as_raw_fd(), libc::POLLIN, deadline.0)?;
            let remaining = bytes
                .get_mut(received..)
                .ok_or(UnixStreamTransportError::Invalid)?;
            match recvmsg_once(self.stream.as_raw_fd(), remaining) {
                Ok(attempt) => {
                    descriptors.extend(attempt.descriptors);
                    if descriptors.len() > max_descriptors {
                        return Err(UnixStreamTransportError::Invalid);
                    }
                    if attempt.bytes == 0 {
                        return Err(if received == 0 {
                            UnixStreamTransportError::Disconnected
                        } else {
                            UnixStreamTransportError::UnexpectedEof
                        });
                    }
                    received = received
                        .checked_add(attempt.bytes)
                        .filter(|count| *count <= expected)
                        .ok_or(UnixStreamTransportError::Invalid)?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    return Err(UnixStreamTransportError::Invalid);
                }
                Err(error) => return Err(UnixStreamTransportError::Io(error.kind())),
            }
        }
        Ok(ReceivedBytes { bytes, descriptors })
    }

    /// Best-effort terminal shutdown of both stream directions.
    pub fn shutdown(&self) {
        let _ignored = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl fmt::Debug for UnixStreamTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnixStreamTransport")
            .field("stream", &"<redacted>")
            .field("timeout", &"<configured>")
            .finish()
    }
}

/// Validates and converts one received descriptor into a connected Unix stream.
pub fn connected_unix_stream(descriptor: OwnedFd) -> Result<UnixStream, UnixStreamTransportError> {
    let raw = descriptor.as_raw_fd();
    let flags = retry_fcntl(raw, libc::F_GETFD, 0)
        .map_err(|error| UnixStreamTransportError::Io(error.kind()))?;
    if flags & libc::FD_CLOEXEC == 0
        || socket_int_option(raw, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_int_option(raw, libc::SO_ERROR)? != 0
        || socket_family(raw, false)? != libc::AF_UNIX
        || socket_family(raw, true)? != libc::AF_UNIX
    {
        return Err(UnixStreamTransportError::Invalid);
    }
    let stream = UnixStream::from(descriptor);
    stream
        .set_nonblocking(true)
        .map_err(|error| UnixStreamTransportError::Io(error.kind()))?;
    suppress_socket_sigpipe(stream.as_raw_fd())
        .map_err(|error| UnixStreamTransportError::Io(error.kind()))?;
    Ok(stream)
}

trait SendDriver {
    fn wait_writable(
        &mut self,
        descriptor: RawFd,
        deadline: Instant,
    ) -> Result<(), UnixStreamTransportError>;

    fn send_once(
        &mut self,
        descriptor: RawFd,
        bytes: &[u8],
        descriptors: &[RawFd],
    ) -> io::Result<usize>;
}

struct SystemSendDriver;

impl SendDriver for SystemSendDriver {
    fn wait_writable(
        &mut self,
        descriptor: RawFd,
        deadline: Instant,
    ) -> Result<(), UnixStreamTransportError> {
        wait_ready(descriptor, libc::POLLOUT, deadline)
    }

    fn send_once(
        &mut self,
        descriptor: RawFd,
        bytes: &[u8],
        descriptors: &[RawFd],
    ) -> io::Result<usize> {
        sendmsg_once(descriptor, bytes, descriptors)
    }
}

fn send_all_with_driver<D: SendDriver>(
    driver: &mut D,
    socket: RawFd,
    bytes: &[u8],
    descriptors: &[RawFd],
    deadline: Instant,
) -> Result<(), UnixStreamTransportError> {
    if bytes.is_empty() || descriptors.len() > MAX_ATTACHED_DESCRIPTORS {
        return Err(UnixStreamTransportError::Invalid);
    }
    let mut transferred = 0_usize;
    while transferred < bytes.len() {
        driver.wait_writable(socket, deadline)?;
        let remaining = bytes
            .get(transferred..)
            .ok_or(UnixStreamTransportError::Invalid)?;
        let attached = if transferred == 0 { descriptors } else { &[] };
        match driver.send_once(socket, remaining, attached) {
            Ok(0) => return Err(UnixStreamTransportError::Disconnected),
            Ok(sent) if sent <= remaining.len() => {
                transferred = transferred
                    .checked_add(sent)
                    .ok_or(UnixStreamTransportError::Invalid)?;
            }
            Ok(_) => return Err(UnixStreamTransportError::Invalid),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                return Err(UnixStreamTransportError::Disconnected);
            }
            Err(error) => return Err(UnixStreamTransportError::Io(error.kind())),
        }
    }
    Ok(())
}

fn sendmsg_once(socket: RawFd, bytes: &[u8], descriptors: &[RawFd]) -> io::Result<usize> {
    if bytes.is_empty() || descriptors.len() > MAX_ATTACHED_DESCRIPTORS {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut iovec = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_usize; CONTROL_WORDS];
    // SAFETY: A zeroed msghdr is a valid baseline. Live payload and optional
    // control pointers are installed before the synchronous call.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;

    if !descriptors.is_empty() {
        let descriptor_bytes = descriptors
            .len()
            .checked_mul(size_of::<RawFd>())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let descriptor_bytes_u32 = u32::try_from(descriptor_bytes)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: CMSG_SPACE performs platform layout arithmetic on the bounded
        // descriptor byte count and dereferences no pointer.
        let control_space = unsafe { libc::CMSG_SPACE(descriptor_bytes_u32) };
        let control_bytes = usize::try_from(control_space)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if control_bytes > control.len().saturating_mul(size_of::<usize>()) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control_bytes
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: The aligned checked control buffer holds one complete header
        // and the descriptor array; all pointers remain live for sendmsg.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            if header.is_null() {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(descriptor_bytes_u32) as _;
            std::ptr::copy_nonoverlapping(
                descriptors.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(header),
                descriptor_bytes,
            );
        }
    }

    // SAFETY: The msghdr references live readable payload/control buffers and
    // the caller retains ownership of all attached descriptors.
    let result = unsafe { libc::sendmsg(socket, &raw const message, send_flags()) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }
}

struct ReceiveAttempt {
    bytes: usize,
    descriptors: Vec<OwnedFd>,
}

fn recvmsg_once(socket: RawFd, bytes: &mut [u8]) -> io::Result<ReceiveAttempt> {
    if bytes.is_empty() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut iovec = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_usize; CONTROL_WORDS];
    // SAFETY: A zeroed msghdr is a valid baseline and receives only into the
    // live writable payload and aligned control buffers installed below.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control
        .len()
        .saturating_mul(size_of::<usize>())
        .try_into()
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;

    // SAFETY: The msghdr points only to live writable buffers. Every returned
    // descriptor is adopted exactly once by parse_control before return.
    let result = unsafe { libc::recvmsg(socket, &raw mut message, receive_flags()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let received =
        usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let returned_control = usize::try_from(message.msg_controllen)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let capacity = control.len().saturating_mul(size_of::<usize>());
    let descriptors = parse_control(&message, returned_control.min(capacity))?;
    if received > bytes.len()
        || returned_control > capacity
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
    {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(ReceiveAttempt {
        bytes: received,
        descriptors,
    })
}

fn parse_control(message: &libc::msghdr, control_bytes: usize) -> io::Result<Vec<OwnedFd>> {
    if control_bytes == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: CMSG_LEN performs platform layout arithmetic for an empty payload.
    let header_bytes = usize::try_from(unsafe { libc::CMSG_LEN(0) })
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if control_bytes < header_bytes || message.msg_control.is_null() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let start = message.msg_control.cast::<u8>() as usize;
    let end = start
        .checked_add(control_bytes)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    let mut descriptors = Vec::new();
    let mut valid = true;
    // SAFETY: message describes the live aligned kernel-populated control
    // buffer. Header and data ranges are checked before each read, and every
    // nonnegative received descriptor is adopted once.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(message);
        while !header.is_null() {
            let address = header.cast::<u8>() as usize;
            let remaining = end
                .checked_sub(address)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            if address < start || remaining < header_bytes {
                valid = false;
                break;
            }
            let declared = usize::try_from((*header).cmsg_len)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
            if declared < header_bytes
                || declared > remaining
                || (*header).cmsg_level != libc::SOL_SOCKET
                || (*header).cmsg_type != libc::SCM_RIGHTS
            {
                valid = false;
                break;
            }
            let data_bytes = declared.saturating_sub(header_bytes);
            if data_bytes == 0 || data_bytes % size_of::<RawFd>() != 0 {
                valid = false;
                break;
            }
            let count = data_bytes / size_of::<RawFd>();
            if descriptors.len().saturating_add(count) > MAX_ATTACHED_DESCRIPTORS {
                valid = false;
            }
            let data = libc::CMSG_DATA(header);
            for index in 0..count {
                let offset = index
                    .checked_mul(size_of::<RawFd>())
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
                let raw = std::ptr::read_unaligned(data.add(offset).cast::<RawFd>());
                if raw < 0 {
                    valid = false;
                } else {
                    descriptors.push(OwnedFd::from_raw_fd(raw));
                }
            }
            let next = libc::CMSG_NXTHDR(message, header);
            if !next.is_null() && next.cast::<u8>() as usize <= address {
                valid = false;
                break;
            }
            header = next;
        }
    }
    for descriptor in &descriptors {
        set_cloexec(descriptor.as_raw_fd())?;
    }
    if !valid {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(descriptors)
}

fn wait_ready(
    descriptor: RawFd,
    interest: libc::c_short,
    deadline: Instant,
) -> Result<(), UnixStreamTransportError> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(UnixStreamTransportError::Timeout);
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
        // SAFETY: One initialized writable pollfd remains live for this call.
        let result = unsafe { libc::poll(&raw mut poll_descriptor, 1, timeout) };
        if result == 0 {
            return Err(UnixStreamTransportError::Timeout);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(UnixStreamTransportError::Io(error.kind()));
        }
        if poll_descriptor.revents & interest != 0 {
            return Ok(());
        }
        if poll_descriptor.revents & libc::POLLNVAL != 0 {
            return Err(UnixStreamTransportError::Io(io::ErrorKind::InvalidInput));
        }
        if poll_descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(UnixStreamTransportError::Disconnected);
        }
        return Err(UnixStreamTransportError::Invalid);
    }
}

fn socket_int_option(
    descriptor: RawFd,
    option: libc::c_int,
) -> Result<i32, UnixStreamTransportError> {
    let mut value = 0_i32;
    let mut length = libc::socklen_t::try_from(size_of::<i32>())
        .map_err(|_| UnixStreamTransportError::Invalid)?;
    // SAFETY: value and length are live writable storage for this socket query.
    let result = unsafe {
        libc::getsockopt(
            descriptor,
            libc::SOL_SOCKET,
            option,
            (&raw mut value).cast(),
            &raw mut length,
        )
    };
    if result != 0 || usize::try_from(length).ok() != Some(size_of::<i32>()) {
        return Err(if result == 0 {
            UnixStreamTransportError::Invalid
        } else {
            UnixStreamTransportError::Io(io::Error::last_os_error().kind())
        });
    }
    Ok(value)
}

fn socket_family(descriptor: RawFd, peer: bool) -> Result<libc::c_int, UnixStreamTransportError> {
    let mut address = MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut length = libc::socklen_t::try_from(size_of::<libc::sockaddr_storage>())
        .map_err(|_| UnixStreamTransportError::Invalid)?;
    // SAFETY: address and length provide writable storage for the live socket's
    // local or peer address. Successful return initializes the family prefix.
    let result = unsafe {
        if peer {
            libc::getpeername(descriptor, address.as_mut_ptr().cast(), &raw mut length)
        } else {
            libc::getsockname(descriptor, address.as_mut_ptr().cast(), &raw mut length)
        }
    };
    if result != 0
        || usize::try_from(length)
            .ok()
            .is_none_or(|length| length < size_of::<libc::sa_family_t>())
    {
        return Err(if result == 0 {
            UnixStreamTransportError::Invalid
        } else {
            UnixStreamTransportError::Io(io::Error::last_os_error().kind())
        });
    }
    // SAFETY: A successful address query initialized at least the checked family.
    Ok(i32::from(unsafe { address.assume_init() }.ss_family))
}

fn set_cloexec(descriptor: RawFd) -> io::Result<()> {
    let flags = retry_fcntl(descriptor, libc::F_GETFD, 0)?;
    if flags & libc::FD_CLOEXEC == 0 {
        retry_fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC)?;
    }
    Ok(())
}

fn retry_fcntl(descriptor: RawFd, command: libc::c_int, argument: libc::c_int) -> io::Result<i32> {
    loop {
        // SAFETY: command is F_GETFD or F_SETFD with an integer argument and
        // the borrowed descriptor remains live for the synchronous call.
        let result = unsafe { libc::fcntl(descriptor, command, argument) };
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
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
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::net::UnixDatagram;

    use super::*;

    #[derive(Debug)]
    struct ScriptDriver {
        results: VecDeque<io::Result<usize>>,
        attachments: Vec<usize>,
    }

    impl SendDriver for ScriptDriver {
        fn wait_writable(
            &mut self,
            _descriptor: RawFd,
            _deadline: Instant,
        ) -> Result<(), UnixStreamTransportError> {
            Ok(())
        }

        fn send_once(
            &mut self,
            _descriptor: RawFd,
            _bytes: &[u8],
            descriptors: &[RawFd],
        ) -> io::Result<usize> {
            self.attachments.push(descriptors.len());
            self.results
                .pop_front()
                .expect("script should have another result")
        }
    }

    #[test]
    fn descriptors_attach_until_first_positive_send() {
        let mut driver = ScriptDriver {
            results: VecDeque::from([
                Err(io::Error::from(io::ErrorKind::WouldBlock)),
                Err(io::Error::from(io::ErrorKind::Interrupted)),
                Ok(3),
                Ok(5),
            ]),
            attachments: Vec::new(),
        };
        send_all_with_driver(
            &mut driver,
            7,
            &[1; 8],
            &[11, 12],
            Instant::now() + Duration::from_secs(1),
        )
        .expect("scripted send should complete");
        assert_eq!(driver.attachments, vec![2, 2, 2, 0]);
    }

    #[test]
    fn zero_and_impossible_send_counts_fail_closed() {
        for result in [Ok(0), Ok(9)] {
            let mut driver = ScriptDriver {
                results: VecDeque::from([result]),
                attachments: Vec::new(),
            };
            assert!(
                send_all_with_driver(
                    &mut driver,
                    7,
                    &[1; 8],
                    &[],
                    Instant::now() + Duration::from_secs(1),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn public_send_rejects_empty_bytes_and_excess_rights_before_io() {
        let (left, _right) = UnixStream::pair().expect("stream pair should open");
        let sender = UnixStreamTransport::new(left, Duration::from_secs(1))
            .expect("transport should initialize");
        let fixture = File::open("/dev/null").expect("fixture should open");
        let excessive = [fixture.as_fd(); MAX_ATTACHED_DESCRIPTORS + 1];

        assert_eq!(
            sender.send(&[], &[]),
            Err(UnixStreamTransportError::Invalid)
        );
        assert_eq!(
            sender.send(&[1], &excessive),
            Err(UnixStreamTransportError::Invalid)
        );
    }

    #[test]
    fn exact_bytes_and_ordered_descriptors_round_trip() {
        let (left, right) = UnixStream::pair().expect("stream pair should open");
        let sender = UnixStreamTransport::new(left, Duration::from_secs(1))
            .expect("transport should initialize");
        let receiver = UnixStreamTransport::new(right, Duration::from_secs(1))
            .expect("transport should initialize");
        let first = File::open("/dev/null").expect("fixture should open");
        let second = File::open("/dev/zero").expect("fixture should open");
        sender
            .send(&[1, 2, 3], &[first.as_fd(), second.as_fd()])
            .expect("message should send");
        let received = receiver
            .receive_exact(3, 2)
            .expect("message should receive");
        let (bytes, descriptors) = received.into_parts();
        assert_eq!(bytes, [1, 2, 3]);
        assert_eq!(descriptors.len(), 2);
        for descriptor in &descriptors {
            let flags = retry_fcntl(descriptor.as_raw_fd(), libc::F_GETFD, 0)
                .expect("descriptor flags should read");
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
    }

    #[test]
    fn connected_stream_validation_rejects_wrong_socket_classes() {
        let (left, mut right) = UnixStream::pair().expect("stream pair should open");
        let duplicate = left.try_clone().expect("stream should clone").into();
        let mut connected = connected_unix_stream(duplicate).expect("stream should validate");
        connected.write_all(&[9]).expect("byte should send");
        let mut byte = [0];
        right.read_exact(&mut byte).expect("byte should arrive");
        assert_eq!(byte, [9]);

        let file: OwnedFd = File::open("/dev/null").expect("fixture should open").into();
        assert!(matches!(
            connected_unix_stream(file),
            Err(UnixStreamTransportError::Io(_)) | Err(UnixStreamTransportError::Invalid)
        ));
        let (datagram, _peer) = UnixDatagram::pair().expect("datagram pair should open");
        assert_eq!(
            connected_unix_stream(datagram.into()).expect_err("datagram should fail"),
            UnixStreamTransportError::Invalid
        );
    }

    #[test]
    fn timeout_clean_eof_and_partial_eof_are_distinct() {
        let (left, _right) = UnixStream::pair().expect("stream pair should open");
        let timeout = UnixStreamTransport::new(left, Duration::from_millis(10))
            .expect("transport should initialize");
        assert!(matches!(
            timeout.receive_exact(1, 0),
            Err(UnixStreamTransportError::Timeout)
        ));

        let (left, right) = UnixStream::pair().expect("stream pair should open");
        let receiver = UnixStreamTransport::new(left, Duration::from_secs(1))
            .expect("transport should initialize");
        drop(right);
        assert!(matches!(
            receiver.receive_exact(2, 0),
            Err(UnixStreamTransportError::Disconnected)
        ));

        let (left, mut right) = UnixStream::pair().expect("stream pair should open");
        let receiver = UnixStreamTransport::new(left, Duration::from_secs(1))
            .expect("transport should initialize");
        right.write_all(&[1]).expect("prefix should send");
        drop(right);
        assert!(matches!(
            receiver.receive_exact(2, 0),
            Err(UnixStreamTransportError::UnexpectedEof)
        ));
    }

    #[test]
    fn impossible_receive_allocation_fails_before_io() {
        let (left, _right) = UnixStream::pair().expect("stream pair should open");
        let receiver = UnixStreamTransport::new(left, Duration::from_secs(1))
            .expect("transport should initialize");
        assert!(matches!(
            receiver.receive_exact(usize::MAX, 0),
            Err(UnixStreamTransportError::Invalid)
        ));
    }

    #[test]
    fn descriptor_limit_rejection_closes_the_received_alias() {
        let (left, right) = UnixStream::pair().expect("stream pair should open");
        let sender = UnixStreamTransport::new(left, Duration::from_secs(1))
            .expect("transport should initialize");
        let receiver = UnixStreamTransport::new(right, Duration::from_secs(1))
            .expect("transport should initialize");
        let (transferred, mut probe) = UnixStream::pair().expect("descriptor pair should open");
        sender
            .send(&[1], &[transferred.as_fd()])
            .expect("descriptor should send");
        drop(transferred);
        assert!(matches!(
            receiver.receive_exact(1, 0),
            Err(UnixStreamTransportError::Invalid)
        ));
        probe
            .set_nonblocking(true)
            .expect("probe should become nonblocking");
        let mut byte = [0];
        assert_eq!(probe.read(&mut byte).expect("probe should read EOF"), 0);
    }

    #[test]
    fn debug_and_errors_are_value_redacted() {
        let (left, _right) = UnixStream::pair().expect("stream pair should open");
        let transport = UnixStreamTransport::new(left, Duration::from_millis(37))
            .expect("transport should initialize");
        let debug = format!("{transport:?}");
        assert!(!debug.contains("37"));
        assert!(!debug.contains("fd"));
        assert_eq!(
            UnixStreamTransportError::Timeout.to_string(),
            "private Unix stream transport failure"
        );
    }
}
