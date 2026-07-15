//! Strict Darwin datagram and SCM_RIGHTS transport for startup grants.

use std::fmt;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;

use crate::{
    GrantFrame, MAX_GRANT_DATAGRAM_BYTES, ProtocolError, decode_grant_frame, encode_grant_frame,
};

const MAX_RECEIVED_DESCRIPTORS: usize = 2;
const MAX_RAW_SEND_DESCRIPTORS: usize = 3;
const CONTROL_WORDS: usize = 16;
const CMSG_ALIGNMENT: usize = size_of::<u32>();

/// Redacted grant transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantTransportError {
    /// A local socket operation failed.
    Io(io::ErrorKind),
    /// Payload or ancillary data violated the closed protocol.
    Invalid,
}

impl fmt::Display for GrantTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private grant transport failure")
    }
}

impl std::error::Error for GrantTransportError {}

impl From<ProtocolError> for GrantTransportError {
    fn from(_: ProtocolError) -> Self {
        Self::Invalid
    }
}

/// One validated datagram and its exact optional descriptor.
#[derive(Debug)]
pub struct ReceivedGrant {
    /// Decoded session-bound protocol frame.
    pub frame: GrantFrame,
    /// Descriptor required by the frame, if any.
    pub descriptor: Option<OwnedFd>,
}

/// Sends one exact grant datagram and optional descriptor.
pub fn send_grant(
    socket: &UnixDatagram,
    frame: &GrantFrame,
    descriptor: Option<RawFd>,
) -> Result<(), GrantTransportError> {
    let encoded = encode_grant_frame(frame)?;
    if descriptor.is_some() != (frame.descriptor_count == 1) {
        return Err(GrantTransportError::Invalid);
    }
    send_raw(socket.as_raw_fd(), &encoded, descriptor.as_slice())
}

/// Receives and validates one exact grant datagram.
pub fn receive_grant(socket: &UnixDatagram) -> Result<ReceivedGrant, GrantTransportError> {
    let mut payload = [0_u8; MAX_GRANT_DATAGRAM_BYTES];
    let mut control = [0_u32; CONTROL_WORDS];
    let mut iovec = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    // SAFETY: An all-zero msghdr is a valid empty message header whose pointer
    // and length fields are initialized immediately below before recvmsg.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    let receive_control_bytes = cmsg_space(
        MAX_RECEIVED_DESCRIPTORS
            .checked_mul(size_of::<RawFd>())
            .ok_or(GrantTransportError::Invalid)?,
    )
    .ok_or(GrantTransportError::Invalid)?;
    message.msg_controllen = libc::socklen_t::try_from(receive_control_bytes)
        .map_err(|_| GrantTransportError::Invalid)?;

    // SAFETY: The message points only to live writable stack buffers for this
    // synchronous call. The socket remains owned by the caller.
    let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut message, 0) };
    if received < 0 {
        return Err(GrantTransportError::Io(io::Error::last_os_error().kind()));
    }
    let payload_len = usize::try_from(received).map_err(|_| GrantTransportError::Invalid)?;
    if payload_len == 0 || payload_len > payload.len() {
        return Err(GrantTransportError::Invalid);
    }

    let returned_control =
        usize::try_from(message.msg_controllen).map_err(|_| GrantTransportError::Invalid)?;
    let descriptors = parse_control(
        control.as_ptr().cast(),
        returned_control.min(control.len() * size_of::<u32>()),
    )?;
    for descriptor in &descriptors {
        super::set_cloexec(descriptor.as_raw_fd())
            .map_err(|error| GrantTransportError::Io(error.kind()))?;
    }
    if returned_control > receive_control_bytes
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
    {
        return Err(GrantTransportError::Invalid);
    }
    let frame = decode_grant_frame(
        payload
            .get(..payload_len)
            .ok_or(GrantTransportError::Invalid)?,
    )?;
    if usize::from(frame.descriptor_count) != descriptors.len() {
        return Err(GrantTransportError::Invalid);
    }
    let mut descriptors = descriptors.into_iter();
    let descriptor = descriptors.next();
    if descriptors.next().is_some() {
        return Err(GrantTransportError::Invalid);
    }
    Ok(ReceivedGrant { frame, descriptor })
}

fn send_raw(
    socket: RawFd,
    payload: &[u8],
    descriptors: &[RawFd],
) -> Result<(), GrantTransportError> {
    if payload.is_empty()
        || payload.len() > MAX_GRANT_DATAGRAM_BYTES
        || descriptors.len() > MAX_RAW_SEND_DESCRIPTORS
    {
        return Err(GrantTransportError::Invalid);
    }
    let mut iovec = libc::iovec {
        iov_base: payload.as_ptr().cast_mut().cast(),
        iov_len: payload.len(),
    };
    let mut control = [0_u32; CONTROL_WORDS];
    // SAFETY: An all-zero msghdr is a valid empty message header whose pointer
    // and length fields are initialized below before sendmsg.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;

    if !descriptors.is_empty() {
        let data_bytes = descriptors
            .len()
            .checked_mul(size_of::<RawFd>())
            .ok_or(GrantTransportError::Invalid)?;
        let control_len = cmsg_space(data_bytes).ok_or(GrantTransportError::Invalid)?;
        if control_len > control.len() * size_of::<u32>() {
            return Err(GrantTransportError::Invalid);
        }
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            libc::socklen_t::try_from(control_len).map_err(|_| GrantTransportError::Invalid)?;
        let header = message.msg_control.cast::<libc::cmsghdr>();
        // SAFETY: The aligned control buffer has room for the checked header and
        // descriptor array. No pointer escapes the synchronous send.
        unsafe {
            (*header).cmsg_len = libc::socklen_t::try_from(
                cmsg_len(data_bytes).ok_or(GrantTransportError::Invalid)?,
            )
            .map_err(|_| GrantTransportError::Invalid)?;
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            std::ptr::copy_nonoverlapping(
                descriptors.as_ptr().cast::<u8>(),
                message.msg_control.cast::<u8>().add(cmsg_aligned_header()),
                data_bytes,
            );
        }
    }

    // SAFETY: The message points to live readable buffers for this synchronous
    // call. The raw descriptors remain owned by the caller.
    let sent = unsafe { libc::sendmsg(socket, &raw const message, 0) };
    if sent < 0 {
        return Err(GrantTransportError::Io(io::Error::last_os_error().kind()));
    }
    if usize::try_from(sent).ok() != Some(payload.len()) {
        return Err(GrantTransportError::Invalid);
    }
    Ok(())
}

fn parse_control(
    control: *const u8,
    control_len: usize,
) -> Result<Vec<OwnedFd>, GrantTransportError> {
    if control_len == 0 {
        return Ok(Vec::new());
    }
    if control_len < size_of::<libc::cmsghdr>() {
        return Err(GrantTransportError::Invalid);
    }
    let mut descriptors = Vec::new();
    let mut offset = 0_usize;
    let mut valid = true;
    while offset < control_len {
        let remaining = control_len.saturating_sub(offset);
        if remaining < size_of::<libc::cmsghdr>() {
            valid = false;
            break;
        }
        // SAFETY: The bounds check above makes one possibly unaligned header
        // readable from the kernel-populated control buffer.
        let header =
            unsafe { std::ptr::read_unaligned(control.add(offset).cast::<libc::cmsghdr>()) };
        let declared =
            usize::try_from(header.cmsg_len).map_err(|_| GrantTransportError::Invalid)?;
        let header_bytes = cmsg_aligned_header();
        if declared < header_bytes {
            valid = false;
            break;
        }
        let available = remaining.min(declared);
        let data_available = available.saturating_sub(header_bytes);
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            let complete_descriptors = data_available / size_of::<RawFd>();
            for index in 0..complete_descriptors {
                let descriptor_offset = offset
                    .checked_add(header_bytes)
                    .and_then(|value| value.checked_add(index * size_of::<RawFd>()))
                    .ok_or(GrantTransportError::Invalid)?;
                // SAFETY: The complete-descriptor count is bounded by the
                // actual returned control storage.
                let descriptor = unsafe {
                    std::ptr::read_unaligned(control.add(descriptor_offset).cast::<RawFd>())
                };
                if descriptor < 0 {
                    valid = false;
                    continue;
                }
                // SAFETY: Each nonnegative descriptor in SCM_RIGHTS control
                // storage is newly owned by this receiving process exactly once.
                descriptors.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
            }
            if complete_descriptors == 0
                || data_available % size_of::<RawFd>() != 0
                || complete_descriptors > MAX_RECEIVED_DESCRIPTORS
            {
                valid = false;
            }
        } else {
            valid = false;
        }
        if declared > remaining {
            valid = false;
            break;
        }
        let Some(next) =
            align_up(declared, CMSG_ALIGNMENT).and_then(|value| offset.checked_add(value))
        else {
            valid = false;
            break;
        };
        if next > control_len {
            valid = false;
            break;
        }
        offset = next;
    }
    if !valid {
        return Err(GrantTransportError::Invalid);
    }
    Ok(descriptors)
}

fn cmsg_aligned_header() -> usize {
    align_up(size_of::<libc::cmsghdr>(), CMSG_ALIGNMENT).unwrap_or(size_of::<libc::cmsghdr>())
}

fn cmsg_len(data_bytes: usize) -> Option<usize> {
    cmsg_aligned_header().checked_add(data_bytes)
}

fn cmsg_space(data_bytes: usize) -> Option<usize> {
    cmsg_aligned_header().checked_add(align_up(data_bytes, CMSG_ALIGNMENT)?)
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|value| value & !(alignment - 1))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    use crate::{
        BatchId, GrantAccess, GrantId, GrantObjectKind, GrantRecord, ObjectIdentity, ResourceRole,
        SessionId,
    };

    use super::*;

    fn descriptor_frame() -> GrantFrame {
        let record = GrantRecord::Descriptor {
            id: GrantId::parse("kernel").expect("test ID should parse"),
            role: ResourceRole::KernelImage,
            access: GrantAccess::ReadOnly,
            kind: GrantObjectKind::RegularFile,
            identity: ObjectIdentity {
                device: 1,
                inode: 2,
            },
            status_flags: 0,
        };
        GrantFrame {
            session: SessionId::from_bytes([1; 32]),
            batch: BatchId::from_bytes([2; 16]),
            sequence: 1,
            descriptor_count: record.descriptor_count(),
            record,
        }
    }

    #[test]
    fn transfers_one_descriptor_and_restores_close_on_exec() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        let file = File::open("/dev/null").expect("fixture should open");
        let frame = descriptor_frame();
        send_grant(&sender, &frame, Some(file.as_raw_fd())).expect("grant should send");
        let received = receive_grant(&receiver).expect("grant should receive");
        assert_eq!(received.frame, frame);
        let descriptor = received.descriptor.expect("descriptor should arrive");
        // SAFETY: F_GETFD reads flags from the live owned descriptor.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn rejects_payload_and_control_truncation() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        let oversized = vec![7_u8; MAX_GRANT_DATAGRAM_BYTES + 1];
        sender
            .send(&oversized)
            .expect("kernel limit should admit probe");
        assert!(matches!(
            receive_grant(&receiver),
            Err(GrantTransportError::Invalid)
        ));

        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        let file = File::open("/dev/null").expect("fixture should open");
        let frame = descriptor_frame();
        let encoded = encode_grant_frame(&frame).expect("frame should encode");
        send_raw(
            sender.as_raw_fd(),
            &encoded,
            &[file.as_raw_fd(), file.as_raw_fd(), file.as_raw_fd()],
        )
        .expect("truncated control fixture should send");
        assert!(matches!(
            receive_grant(&receiver),
            Err(GrantTransportError::Invalid)
        ));
    }

    #[test]
    fn malformed_control_closes_every_already_delivered_descriptor() {
        let file = File::open("/dev/null").expect("fixture should open");
        // SAFETY: The source remains live and a successful result is an
        // independently owned descriptor deliberately transferred to parser data.
        let duplicate = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 200) };
        assert!(duplicate >= 200);

        let mut control = [0_u32; CONTROL_WORDS];
        let header = control.as_mut_ptr().cast::<libc::cmsghdr>();
        // SAFETY: The aligned control buffer has room for one header, one fd,
        // and the deliberate trailing malformed byte.
        unsafe {
            (*header).cmsg_len = libc::socklen_t::try_from(
                cmsg_len(size_of::<RawFd>()).expect("control length should fit"),
            )
            .expect("control length should fit socklen");
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            std::ptr::write_unaligned(
                control
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(cmsg_aligned_header())
                    .cast::<RawFd>(),
                duplicate,
            );
        }
        let malformed_length = cmsg_space(size_of::<RawFd>())
            .expect("control space should fit")
            .checked_add(1)
            .expect("malformed length should fit");
        assert!(matches!(
            parse_control(control.as_ptr().cast(), malformed_length),
            Err(GrantTransportError::Invalid)
        ));
        // SAFETY: F_GETFD only inspects whether the parser-owned descriptor was
        // closed when the later malformed byte rejected the complete message.
        assert_eq!(unsafe { libc::fcntl(duplicate, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn rejects_short_or_unexpected_control_headers() {
        let short = [0_u8; size_of::<libc::cmsghdr>() - 1];
        assert!(matches!(
            parse_control(short.as_ptr(), short.len()),
            Err(GrantTransportError::Invalid)
        ));

        let mut control = [0_u32; CONTROL_WORDS];
        let header = control.as_mut_ptr().cast::<libc::cmsghdr>();
        // SAFETY: The aligned buffer has room for the initialized header.
        unsafe {
            (*header).cmsg_len =
                libc::socklen_t::try_from(cmsg_aligned_header()).expect("header length should fit");
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = 0;
        }
        assert!(matches!(
            parse_control(control.as_ptr().cast(), cmsg_aligned_header()),
            Err(GrantTransportError::Invalid)
        ));
    }
}
