//! Strict typed transport for the one elevated API-listener authority.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;

use crate::elevated_probe::{API_LISTENER_RECORD_BYTES, ApiListenerKind, ApiListenerRecord};

use super::grant_transport::{GrantTransportError, receive_raw, send_raw};

/// One validated acknowledgment and its exactly owned listener descriptor.
#[derive(Debug)]
pub struct ReceivedApiListener {
    /// Canonical launcher acknowledgment bound to the current session.
    pub record: ApiListenerRecord,
    /// Exactly one close-on-exec listener authority received from the launcher.
    pub listener: OwnedFd,
}

/// Sends the exact descriptor-free worker request.
pub fn send_api_listener_request(
    socket: &UnixDatagram,
    record: ApiListenerRecord,
) -> Result<(), GrantTransportError> {
    if record.kind() != ApiListenerKind::Request || record.descriptor_count() != 0 {
        return Err(GrantTransportError::Invalid);
    }
    send_raw(socket.as_raw_fd(), &record.encode(), &[])
}

/// Receives the exact descriptor-free worker request.
pub fn receive_api_listener_request(
    socket: &UnixDatagram,
) -> Result<ApiListenerRecord, GrantTransportError> {
    let (record, descriptors) = receive_record(socket)?;
    if record.kind() != ApiListenerKind::Request
        || record.descriptor_count() != 0
        || !descriptors.is_empty()
    {
        return Err(GrantTransportError::Invalid);
    }
    Ok(record)
}

/// Sends the exact launcher acknowledgment and one borrowed listener alias.
///
/// The caller retains ownership of `listener` on every result, including
/// interrupted or would-block sends.
pub fn send_api_listener_ack(
    socket: &UnixDatagram,
    record: ApiListenerRecord,
    listener: RawFd,
) -> Result<(), GrantTransportError> {
    if record.kind() != ApiListenerKind::Ack || record.descriptor_count() != 1 {
        return Err(GrantTransportError::Invalid);
    }
    send_raw(socket.as_raw_fd(), &record.encode(), &[listener])
}

/// Receives the exact launcher acknowledgment and one owned listener alias.
pub fn receive_api_listener_ack(
    socket: &UnixDatagram,
) -> Result<ReceivedApiListener, GrantTransportError> {
    let (record, descriptors) = receive_record(socket)?;
    if record.kind() != ApiListenerKind::Ack
        || record.descriptor_count() != 1
        || descriptors.len() != 1
    {
        return Err(GrantTransportError::Invalid);
    }
    let mut descriptors = descriptors.into_iter();
    let listener = descriptors.next().ok_or(GrantTransportError::Invalid)?;
    if descriptors.next().is_some() {
        return Err(GrantTransportError::Invalid);
    }
    Ok(ReceivedApiListener { record, listener })
}

fn receive_record(
    socket: &UnixDatagram,
) -> Result<(ApiListenerRecord, Vec<OwnedFd>), GrantTransportError> {
    let mut payload = [0_u8; API_LISTENER_RECORD_BYTES];
    let (length, descriptors) = receive_raw(socket, &mut payload)?;
    if length != payload.len() {
        return Err(GrantTransportError::Invalid);
    }
    let record = ApiListenerRecord::decode(&payload).map_err(|_| GrantTransportError::Invalid)?;
    Ok((record, descriptors))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Read as _;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use crate::elevated_probe::{GuestEvidencePhase, GuestEvidenceRecord, ProbeMode};
    use crate::{ObjectIdentity, SessionId};

    use super::*;

    fn request() -> ApiListenerRecord {
        ApiListenerRecord::worker_request(
            ProbeMode::GuestApiDrop,
            SessionId::from_bytes([1; 32]),
            SessionId::from_bytes([2; 32]),
        )
        .expect("request should construct")
    }

    fn acknowledgment() -> ApiListenerRecord {
        ApiListenerRecord::launcher_ack(
            ProbeMode::GuestApiDrop,
            SessionId::from_bytes([1; 32]),
            SessionId::from_bytes([2; 32]),
            ObjectIdentity {
                device: 3,
                inode: 4,
            },
        )
        .expect("acknowledgment should construct")
    }

    #[test]
    fn request_and_one_descriptor_acknowledgment_round_trip_exactly() {
        let (worker, launcher) = UnixDatagram::pair().expect("datagram pair should open");
        send_api_listener_request(&worker, request()).expect("request should send");
        assert_eq!(receive_api_listener_request(&launcher), Ok(request()));

        let file = File::open("/dev/null").expect("fixture should open");
        send_api_listener_ack(&launcher, acknowledgment(), file.as_raw_fd())
            .expect("acknowledgment should send");
        let received = receive_api_listener_ack(&worker).expect("acknowledgment should receive");
        assert_eq!(received.record, acknowledgment());
        // SAFETY: F_GETFD reads flags from the live owned descriptor.
        let flags = unsafe { libc::fcntl(received.listener.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn senders_reject_the_wrong_record_kind_without_sending() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        let file = File::open("/dev/null").expect("fixture should open");
        assert_eq!(
            send_api_listener_request(&sender, acknowledgment()),
            Err(GrantTransportError::Invalid)
        );
        assert_eq!(
            send_api_listener_ack(&sender, request(), file.as_raw_fd()),
            Err(GrantTransportError::Invalid)
        );
        receiver
            .set_nonblocking(true)
            .expect("receiver should become nonblocking");
        assert!(matches!(
            receive_api_listener_request(&receiver),
            Err(GrantTransportError::Io(std::io::ErrorKind::WouldBlock))
        ));
    }

    #[test]
    fn request_rejects_rights_truncation_excess_and_wrong_family() {
        let file = File::open("/dev/null").expect("fixture should open");
        let guest = GuestEvidenceRecord::worker_request(
            ProbeMode::GuestApiDrop,
            GuestEvidencePhase::ResourceClaim,
            SessionId::from_bytes([1; 32]),
            SessionId::from_bytes([2; 32]),
        )
        .expect("guest record should construct");
        for (payload, descriptors) in [
            (request().encode().to_vec(), vec![file.as_raw_fd()]),
            (
                request().encode()[..API_LISTENER_RECORD_BYTES - 1].to_vec(),
                vec![],
            ),
            (vec![7; API_LISTENER_RECORD_BYTES + 1], vec![]),
            (acknowledgment().encode().to_vec(), vec![]),
            (guest.encode().to_vec(), vec![]),
        ] {
            let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
            send_raw(sender.as_raw_fd(), &payload, &descriptors)
                .expect("hostile record should reach receiver");
            assert_eq!(
                receive_api_listener_request(&receiver),
                Err(GrantTransportError::Invalid)
            );
        }
    }

    #[test]
    fn acknowledgment_rejects_missing_extra_and_wrong_state_rights() {
        let file = File::open("/dev/null").expect("fixture should open");
        for (payload, descriptors) in [
            (acknowledgment().encode().to_vec(), vec![]),
            (
                acknowledgment().encode().to_vec(),
                vec![file.as_raw_fd(), file.as_raw_fd()],
            ),
            (request().encode().to_vec(), vec![file.as_raw_fd()]),
            (
                acknowledgment().encode()[..API_LISTENER_RECORD_BYTES - 1].to_vec(),
                vec![file.as_raw_fd()],
            ),
            (
                vec![7; API_LISTENER_RECORD_BYTES + 1],
                vec![file.as_raw_fd()],
            ),
        ] {
            let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
            send_raw(sender.as_raw_fd(), &payload, &descriptors)
                .expect("hostile record should reach receiver");
            assert!(matches!(
                receive_api_listener_ack(&receiver),
                Err(GrantTransportError::Invalid)
            ));
        }
    }

    #[test]
    fn wrong_queued_record_is_consumed_without_skipping_the_next_request() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        send_raw(sender.as_raw_fd(), &acknowledgment().encode(), &[])
            .expect("wrong-state record should send");
        send_api_listener_request(&sender, request()).expect("request should send");
        assert_eq!(
            receive_api_listener_request(&receiver),
            Err(GrantTransportError::Invalid)
        );
        assert_eq!(receive_api_listener_request(&receiver), Ok(request()));
    }

    #[test]
    fn rejected_request_right_is_closed_before_returning() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        let (transferred, mut peer) = UnixStream::pair().expect("stream pair should open");
        send_raw(
            sender.as_raw_fd(),
            &request().encode(),
            &[transferred.as_raw_fd()],
        )
        .expect("wrong-state right should reach receiver");
        drop(transferred);

        assert_eq!(
            receive_api_listener_request(&receiver),
            Err(GrantTransportError::Invalid)
        );
        peer.set_nonblocking(true)
            .expect("peer should become nonblocking");
        let mut byte = [0_u8; 1];
        assert_eq!(
            peer.read(&mut byte)
                .expect("rejection must close the received right"),
            0
        );
    }
}
