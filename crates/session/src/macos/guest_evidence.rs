//! Strict descriptor-free transport for post-grant elevated guest evidence.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;

use crate::elevated_probe::{GUEST_EVIDENCE_RECORD_BYTES, GuestEvidenceRecord};

use super::grant_transport::{GrantTransportError, receive_raw, send_raw};

/// Sends one exact guest-evidence record without ancillary authority.
pub fn send_guest_evidence(
    socket: &UnixDatagram,
    record: GuestEvidenceRecord,
) -> Result<(), GrantTransportError> {
    send_raw(socket.as_raw_fd(), &record.encode(), &[])
}

/// Receives one exact guest-evidence record and rejects all descriptors.
pub fn receive_guest_evidence(
    socket: &UnixDatagram,
) -> Result<GuestEvidenceRecord, GrantTransportError> {
    let mut payload = [0_u8; GUEST_EVIDENCE_RECORD_BYTES];
    let (length, descriptors) = receive_raw(socket, &mut payload)?;
    if length != payload.len() || !descriptors.is_empty() {
        return Err(GrantTransportError::Invalid);
    }
    GuestEvidenceRecord::decode(&payload).map_err(|_| GrantTransportError::Invalid)
}

/// Returns whether the connected datagram currently has no queued payload.
#[must_use]
pub fn guest_evidence_transport_is_empty(socket: &UnixDatagram) -> bool {
    let mut byte = 0_u8;
    // SAFETY: `byte` is writable for one non-consuming probe and the socket is live.
    let result = unsafe {
        libc::recv(
            socket.as_raw_fd(),
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
    use std::os::fd::AsRawFd;

    use crate::SessionId;
    use crate::elevated_probe::{GuestEvidencePhase, GuestEvidenceRecord, ProbeMode};

    use super::*;

    fn request() -> GuestEvidenceRecord {
        GuestEvidenceRecord::worker_request(
            ProbeMode::GuestApiDrop,
            GuestEvidencePhase::ResourceClaim,
            SessionId::from_bytes([1; 32]),
            SessionId::from_bytes([2; 32]),
        )
        .expect("request should construct")
    }

    #[test]
    fn descriptor_free_record_round_trips_exactly() {
        let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
        assert!(guest_evidence_transport_is_empty(&receiver));
        send_guest_evidence(&sender, request()).expect("record should send");
        assert!(!guest_evidence_transport_is_empty(&receiver));
        assert_eq!(receive_guest_evidence(&receiver), Ok(request()));
        assert!(guest_evidence_transport_is_empty(&receiver));
    }

    #[test]
    fn rejects_descriptors_truncation_excess_and_wrong_record_family() {
        let file = File::open("/dev/null").expect("fixture should open");
        for (payload, descriptors) in [
            (request().encode().to_vec(), vec![file.as_raw_fd()]),
            (
                request().encode()[..GUEST_EVIDENCE_RECORD_BYTES - 1].to_vec(),
                vec![],
            ),
            (vec![7; GUEST_EVIDENCE_RECORD_BYTES + 1], vec![]),
            (vec![7; GUEST_EVIDENCE_RECORD_BYTES], vec![]),
        ] {
            let (sender, receiver) = UnixDatagram::pair().expect("datagram pair should open");
            send_raw(sender.as_raw_fd(), &payload, &descriptors)
                .expect("hostile record should reach receiver");
            assert_eq!(
                receive_guest_evidence(&receiver),
                Err(GrantTransportError::Invalid)
            );
        }
    }
}
