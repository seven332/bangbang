//! Strict one-shot transport for launcher-created runtime-session authority.

use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;

use crate::elevated_probe::{RUNTIME_SESSION_AUTHORITY_BYTES, RuntimeSessionAuthority};

use super::grant_transport::{GrantTransportError, receive_raw, send_raw};

/// One canonical authority record and its exact session descriptor.
pub struct ReceivedRuntimeSessionAuthority {
    /// Bootstrap- and lifecycle-bound authority record.
    pub authority: RuntimeSessionAuthority,
    /// Exact launcher-created session directory.
    pub descriptor: OwnedFd,
}

impl fmt::Debug for ReceivedRuntimeSessionAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReceivedRuntimeSessionAuthority(<redacted>)")
    }
}

/// Sends one canonical authority and consumes the launcher's descriptor alias.
pub fn send_runtime_session_authority(
    socket: &UnixDatagram,
    authority: RuntimeSessionAuthority,
    descriptor: OwnedFd,
) -> Result<(), GrantTransportError> {
    let encoded = authority.encode();
    let raw = descriptor.as_raw_fd();
    let result = send_raw(socket.as_raw_fd(), &encoded, &[raw]);
    drop(descriptor);
    result
}

/// Receives exactly one canonical authority and one close-on-exec descriptor.
pub fn receive_runtime_session_authority(
    socket: &UnixDatagram,
) -> Result<ReceivedRuntimeSessionAuthority, GrantTransportError> {
    let mut payload = [0_u8; RUNTIME_SESSION_AUTHORITY_BYTES];
    let (length, descriptors) = receive_raw(socket, &mut payload)?;
    if length != payload.len() || descriptors.len() != 1 {
        return Err(GrantTransportError::Invalid);
    }
    let authority =
        RuntimeSessionAuthority::decode(&payload).map_err(|_| GrantTransportError::Invalid)?;
    let mut descriptors = descriptors.into_iter();
    let descriptor = descriptors.next().ok_or(GrantTransportError::Invalid)?;
    if descriptors.next().is_some() {
        return Err(GrantTransportError::Invalid);
    }
    if !transport_is_empty(socket.as_raw_fd()) {
        return Err(GrantTransportError::Invalid);
    }
    Ok(ReceivedRuntimeSessionAuthority {
        authority,
        descriptor,
    })
}

fn transport_is_empty(fd: libc::c_int) -> bool {
    let mut byte = 0_u8;
    // SAFETY: `byte` is writable for one non-consuming byte probe and `fd` is
    // the live connected authority datagram endpoint.
    let result = unsafe {
        libc::recv(
            fd,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        return error
            .raw_os_error()
            .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK);
    }
    false
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::Duration;

    use crate::elevated_probe::{ProbeMode, RuntimeSessionAuthority};
    use crate::{ObjectIdentity, SessionId};

    use super::*;

    fn authority() -> RuntimeSessionAuthority {
        RuntimeSessionAuthority::launcher(
            ProbeMode::RuntimeDrop,
            501,
            20,
            ObjectIdentity {
                device: 11,
                inode: 12,
            },
            ObjectIdentity {
                device: 13,
                inode: 14,
            },
            SessionId::from_bytes([1; 32]),
            SessionId::from_bytes([2; 32]),
        )
        .expect("authority should construct")
    }

    fn duplicate(file: &File) -> OwnedFd {
        // SAFETY: `file` remains live and success returns one new owned descriptor.
        let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(descriptor >= 0, "descriptor duplication should succeed");
        // SAFETY: The successful fresh descriptor transfers into this owner once.
        unsafe { OwnedFd::from_raw_fd(descriptor) }
    }

    #[test]
    fn authority_round_trips_and_consumes_sender_alias() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        let file = File::open("/dev/null").expect("fixture should open");
        let descriptor = duplicate(&file);
        let raw = descriptor.as_raw_fd();
        send_runtime_session_authority(&sender, authority(), descriptor)
            .expect("authority should send");
        // SAFETY: The sender function consumed the exact descriptor number.
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);

        let received =
            receive_runtime_session_authority(&receiver).expect("authority should receive");
        assert_eq!(received.authority, authority());
        // SAFETY: The received descriptor is live and queried without mutation.
        let flags = unsafe { libc::fcntl(received.descriptor.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        assert_eq!(
            format!("{received:?}"),
            "ReceivedRuntimeSessionAuthority(<redacted>)"
        );
    }

    #[test]
    fn authority_rejects_missing_and_extra_descriptors() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        let encoded = authority().encode();
        send_raw(sender.as_raw_fd(), &encoded, &[]).expect("missing-rights record should send");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));

        let first = File::open("/dev/null").expect("first fixture should open");
        let second = File::open("/dev/null").expect("second fixture should open");
        let third = File::open("/dev/null").expect("third fixture should open");
        send_raw(
            sender.as_raw_fd(),
            &encoded,
            &[first.as_raw_fd(), second.as_raw_fd(), third.as_raw_fd()],
        )
        .expect("control-truncating record should send");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));

        send_raw(
            sender.as_raw_fd(),
            &encoded,
            &[first.as_raw_fd(), second.as_raw_fd()],
        )
        .expect("extra-rights record should send");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));
    }

    #[test]
    fn authority_rejects_wrong_payload_shape() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        let file = File::open("/dev/null").expect("fixture should open");
        send_raw(sender.as_raw_fd(), b"short", &[file.as_raw_fd()])
            .expect("short record should send");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));

        let mut malformed = authority().encode();
        malformed[9] = 1;
        send_raw(sender.as_raw_fd(), &malformed, &[file.as_raw_fd()])
            .expect("malformed record should send");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));

        let oversized = [0_u8; RUNTIME_SESSION_AUTHORITY_BYTES + 1];
        send_raw(sender.as_raw_fd(), &oversized, &[file.as_raw_fd()])
            .expect("oversized record should send at the raw boundary");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));
    }

    #[test]
    fn authority_rejects_an_already_queued_replay() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        let first = File::open("/dev/null").expect("first fixture should open");
        let second = File::open("/dev/null").expect("second fixture should open");
        let encoded = authority().encode();
        send_raw(sender.as_raw_fd(), &encoded, &[first.as_raw_fd()])
            .expect("first authority should queue");
        send_raw(sender.as_raw_fd(), &encoded, &[second.as_raw_fd()])
            .expect("replayed authority should queue");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Invalid)
        ));
    }

    #[test]
    fn failed_send_still_consumes_the_sender_alias() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        drop(receiver);
        let file = File::open("/dev/null").expect("fixture should open");
        let descriptor = duplicate(&file);
        let raw = descriptor.as_raw_fd();
        assert!(send_runtime_session_authority(&sender, authority(), descriptor).is_err());
        // SAFETY: The send function consumes its exact descriptor on failure.
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);
    }

    #[test]
    fn authority_receive_fails_closed_on_timeout_and_peer_close() {
        let (_sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        receiver
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("receive timeout should install");
        assert!(matches!(
            receive_runtime_session_authority(&receiver),
            Err(GrantTransportError::Io(
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ))
        ));

        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should create");
        drop(sender);
        assert!(receive_runtime_session_authority(&receiver).is_err());
    }
}
