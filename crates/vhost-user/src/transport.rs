//! Deadline-bounded Unix stream and SCM_RIGHTS framing.

use std::fmt;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::error::VhostUserError;
use crate::message::{HEADER_BYTES, Header, MAX_ATTACHED_FDS, Request, frame};

const CONTROL_WORDS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportError {
    Invalid,
    Timeout,
    Disconnected,
    Io(io::ErrorKind),
}

impl From<TransportError> for VhostUserError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Invalid => Self::InvalidMessage,
            TransportError::Timeout => Self::Timeout,
            TransportError::Disconnected => Self::Disconnected,
            TransportError::Io(kind) => Self::Io(kind),
        }
    }
}

pub(crate) struct Transport {
    stream: UnixStream,
    timeout: Duration,
}

impl fmt::Debug for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Transport")
            .field("stream", &"redacted")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Transport {
    pub(crate) fn new(stream: UnixStream, timeout: Duration) -> Result<Self, VhostUserError> {
        stream
            .set_nonblocking(true)
            .map_err(|error| VhostUserError::Io(error.kind()))?;
        suppress_socket_sigpipe(stream.as_raw_fd())
            .map_err(|error| VhostUserError::Io(error.kind()))?;
        Ok(Self { stream, timeout })
    }

    pub(crate) fn send(
        &self,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
        need_reply: bool,
    ) -> Result<(), TransportError> {
        let encoded = frame(request, body, need_reply).map_err(|_| TransportError::Invalid)?;
        let raw_descriptors: Vec<RawFd> = descriptors.iter().map(AsRawFd::as_raw_fd).collect();
        let deadline = self.deadline()?;
        let mut driver = SystemSendDriver;
        send_frame_with_driver(
            &mut driver,
            self.stream.as_raw_fd(),
            &encoded,
            &raw_descriptors,
            deadline,
        )
    }

    pub(crate) fn request_reply(
        &self,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
        need_reply: bool,
    ) -> Result<Vec<u8>, TransportError> {
        let deadline = self.deadline()?;
        let encoded = frame(request, body, need_reply).map_err(|_| TransportError::Invalid)?;
        let raw_descriptors: Vec<RawFd> = descriptors.iter().map(AsRawFd::as_raw_fd).collect();
        let mut driver = SystemSendDriver;
        send_frame_with_driver(
            &mut driver,
            self.stream.as_raw_fd(),
            &encoded,
            &raw_descriptors,
            deadline,
        )?;
        self.receive_reply(request, deadline)
    }

    pub(crate) fn shutdown(&self) {
        let _ignored = self.stream.shutdown(std::net::Shutdown::Both);
    }

    fn deadline(&self) -> Result<Instant, TransportError> {
        Instant::now()
            .checked_add(self.timeout)
            .ok_or(TransportError::Invalid)
    }

    fn receive_reply(
        &self,
        expected_request: Request,
        deadline: Instant,
    ) -> Result<Vec<u8>, TransportError> {
        let header_bytes = receive_exact(self.stream.as_raw_fd(), HEADER_BYTES, deadline)?;
        let header = Header::decode(&header_bytes).map_err(|_| TransportError::Invalid)?;
        if !header.is_reply || header.need_reply || header.request != expected_request {
            return Err(TransportError::Invalid);
        }
        receive_exact(self.stream.as_raw_fd(), header.body_size, deadline)
    }
}

trait SendDriver {
    fn wait_writable(&mut self, descriptor: RawFd, deadline: Instant)
    -> Result<(), TransportError>;

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
    ) -> Result<(), TransportError> {
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

fn send_frame_with_driver<D: SendDriver>(
    driver: &mut D,
    socket: RawFd,
    frame: &[u8],
    descriptors: &[RawFd],
    deadline: Instant,
) -> Result<(), TransportError> {
    if frame.is_empty() || descriptors.len() > MAX_ATTACHED_FDS {
        return Err(TransportError::Invalid);
    }
    let mut transferred = 0_usize;
    while transferred < frame.len() {
        driver.wait_writable(socket, deadline)?;
        let remaining = frame.get(transferred..).ok_or(TransportError::Invalid)?;
        let attached = if transferred == 0 { descriptors } else { &[] };
        match driver.send_once(socket, remaining, attached) {
            Ok(0) => return Err(TransportError::Disconnected),
            Ok(sent) if sent <= remaining.len() => {
                transferred = transferred
                    .checked_add(sent)
                    .ok_or(TransportError::Invalid)?;
            }
            Ok(_) => return Err(TransportError::Invalid),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                return Err(TransportError::Disconnected);
            }
            Err(error) => return Err(TransportError::Io(error.kind())),
        }
    }
    Ok(())
}

fn receive_exact(
    socket: RawFd,
    expected: usize,
    deadline: Instant,
) -> Result<Vec<u8>, TransportError> {
    let mut bytes = vec![0_u8; expected];
    let mut received = 0_usize;
    while received < expected {
        wait_ready(socket, libc::POLLIN, deadline)?;
        let remaining = bytes.get_mut(received..).ok_or(TransportError::Invalid)?;
        match recvmsg_once(socket, remaining) {
            Ok(attempt) => {
                if !attempt.descriptors.is_empty() {
                    return Err(TransportError::Invalid);
                }
                if attempt.bytes == 0 {
                    return Err(TransportError::Disconnected);
                }
                received = received
                    .checked_add(attempt.bytes)
                    .ok_or(TransportError::Invalid)?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Err(TransportError::Invalid);
            }
            Err(error) => return Err(TransportError::Io(error.kind())),
        }
    }
    Ok(bytes)
}

fn wait_ready(
    descriptor: RawFd,
    interest: libc::c_short,
    deadline: Instant,
) -> Result<(), TransportError> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(TransportError::Timeout);
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
        // SAFETY: `poll_descriptor` is one initialized writable entry and does
        // not escape this synchronous call.
        let result = unsafe { libc::poll(&raw mut poll_descriptor, 1, timeout) };
        if result == 0 {
            return Err(TransportError::Timeout);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(TransportError::Io(error.kind()));
        }
        if poll_descriptor.revents & interest != 0 {
            return Ok(());
        }
        if poll_descriptor.revents & libc::POLLNVAL != 0 {
            return Err(TransportError::Io(io::ErrorKind::InvalidInput));
        }
        if poll_descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(TransportError::Disconnected);
        }
        return Err(TransportError::Invalid);
    }
}

pub(crate) fn sendmsg_once(
    socket: RawFd,
    bytes: &[u8],
    descriptors: &[RawFd],
) -> io::Result<usize> {
    if bytes.is_empty() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut iovec = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_usize; CONTROL_WORDS];
    // SAFETY: An all-zero msghdr is a valid empty header. Live iovec and
    // optional control pointers are installed below before the call.
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
        // SAFETY: CMSG_SPACE performs checked platform layout arithmetic on
        // the bounded descriptor payload length and dereferences no pointer.
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
        // SAFETY: The checked aligned control buffer is large enough for one
        // cmsghdr and the complete descriptor array. No pointer escapes.
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

    // SAFETY: The msghdr points only to live readable buffers for this
    // synchronous call. Attached descriptors remain borrowed by the caller.
    let result = unsafe { libc::sendmsg(socket, &raw const message, send_flags()) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }
}

pub(crate) struct ReceiveAttempt {
    pub(crate) bytes: usize,
    pub(crate) descriptors: Vec<OwnedFd>,
}

pub(crate) fn recvmsg_once(socket: RawFd, bytes: &mut [u8]) -> io::Result<ReceiveAttempt> {
    if bytes.is_empty() {
        return Ok(ReceiveAttempt {
            bytes: 0,
            descriptors: Vec::new(),
        });
    }
    let mut iovec = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_usize; CONTROL_WORDS];
    // SAFETY: An all-zero msghdr is valid and receives live writable payload
    // and aligned control buffers below.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control
        .len()
        .saturating_mul(size_of::<usize>())
        .try_into()
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;

    // SAFETY: The msghdr points only to live writable buffers for this
    // synchronous call. Any returned descriptor is adopted below.
    let result = unsafe { libc::recvmsg(socket, &raw mut message, 0) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let received =
        usize::try_from(result).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let returned_control = usize::try_from(message.msg_controllen)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let control_capacity = control.len().saturating_mul(size_of::<usize>());
    let descriptors = parse_control(&message, returned_control.min(control_capacity))?;
    if received > bytes.len()
        || returned_control > control_capacity
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
    // SAFETY: CMSG_LEN performs platform layout arithmetic for an empty data
    // payload and dereferences no pointer.
    let empty_header_len = unsafe { libc::CMSG_LEN(0) };
    let header_bytes = usize::try_from(empty_header_len)
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
    // SAFETY: `message` describes the live aligned control buffer returned by
    // recvmsg. Every header/data range is checked against its returned bounds
    // before being read, and each received descriptor is adopted once.
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
            if descriptors.len().saturating_add(count) > MAX_ATTACHED_FDS {
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
                    continue;
                }
                descriptors.push(OwnedFd::from_raw_fd(raw));
            }
            let next = libc::CMSG_NXTHDR(message, header);
            if !next.is_null() && next.cast::<u8>() as usize <= address {
                valid = false;
                break;
            }
            header = next;
        }
    }
    let mut cloexec_error = None;
    for descriptor in &descriptors {
        if let Err(error) = set_cloexec(descriptor.as_raw_fd())
            && cloexec_error.is_none()
        {
            cloexec_error = Some(error);
        }
    }
    if let Some(error) = cloexec_error {
        return Err(error);
    }
    if descriptors.is_empty() || !valid {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(descriptors)
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
        // SAFETY: The command is either F_GETFD or F_SETFD with an integer
        // argument and the borrowed descriptor remains live for the call.
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs::File;
    use std::os::fd::AsFd;

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
        ) -> Result<(), TransportError> {
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
    fn descriptors_are_attached_only_until_first_positive_send() {
        let mut driver = ScriptDriver {
            results: VecDeque::from([
                Err(io::Error::from(io::ErrorKind::WouldBlock)),
                Err(io::Error::from(io::ErrorKind::Interrupted)),
                Ok(3),
                Ok(5),
            ]),
            attachments: Vec::new(),
        };
        send_frame_with_driver(
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
                send_frame_with_driver(
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
    fn real_stream_transfers_ordered_close_on_exec_descriptors() {
        let (sender, receiver) = UnixStream::pair().expect("stream pair should open");
        let transport =
            Transport::new(sender, Duration::from_secs(1)).expect("transport should initialize");
        let first = File::open("/dev/null").expect("fixture should open");
        let second = File::open("/dev/zero").expect("fixture should open");
        transport
            .send(
                Request::SetMemoryTable,
                &[1, 2, 3],
                &[first.as_fd(), second.as_fd()],
                false,
            )
            .expect("message should send");

        let mut payload = [0_u8; 64];
        let received =
            recvmsg_once(receiver.as_raw_fd(), &mut payload).expect("message should receive");
        assert_eq!(received.bytes, HEADER_BYTES + 3);
        assert_eq!(received.descriptors.len(), 2);
        let mut descriptors = received.descriptors.into_iter();
        let first_received = descriptors.next().expect("first descriptor should arrive");
        let second_received = descriptors.next().expect("second descriptor should arrive");
        assert!(descriptors.next().is_none());
        for descriptor in [&first_received, &second_received] {
            let flags = retry_fcntl(descriptor.as_raw_fd(), libc::F_GETFD, 0)
                .expect("descriptor flags should read");
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
        let mut byte = [7_u8; 1];
        // SAFETY: The first received descriptor is the live /dev/null entry and
        // the one-byte buffer is writable.
        let null_read = unsafe {
            libc::read(
                first_received.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                byte.len(),
            )
        };
        assert_eq!(null_read, 0);
        // SAFETY: The second received descriptor is the live /dev/zero entry
        // and the same one-byte buffer is writable.
        let zero_read = unsafe {
            libc::read(
                second_received.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                byte.len(),
            )
        };
        assert_eq!(zero_read, 1);
        assert_eq!(byte, [0]);
    }

    #[test]
    fn disconnected_socket_is_a_typed_failure() {
        let (sender, receiver) = UnixStream::pair().expect("stream pair should open");
        let transport = Transport::new(sender, Duration::from_millis(100))
            .expect("transport should initialize");
        drop(receiver);
        assert!(matches!(
            transport.send(Request::SetOwner, &[], &[], false),
            Err(TransportError::Disconnected) | Err(TransportError::Io(io::ErrorKind::BrokenPipe))
        ));
    }
}
