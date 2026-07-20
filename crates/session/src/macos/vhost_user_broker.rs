//! Closed launcher-worker transport for contained vhost-user connections.

use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;

use crate::{GrantId, MAX_GRANT_ID_BYTES, MAX_SOCKET_CHILD_BYTES, SessionId, SocketChild};

use super::grant_transport::{GrantTransportError, receive_raw, send_raw};

const FRAME_BYTES: usize = 256;
const MAGIC: [u8; 4] = *b"BBU1";
const VERSION: u8 = 1;
const KIND_CONNECT: u8 = 1;
const KIND_CONNECTED: u8 = 2;
const KIND_FAILED: u8 = 3;
const STATUS_OK: u8 = 0;
const STATUS_NOT_FOUND: u8 = 1;
const STATUS_PERMISSION_DENIED: u8 = 2;
const STATUS_CONNECTION_REFUSED: u8 = 3;
const STATUS_TIMED_OUT: u8 = 4;
const STATUS_OTHER: u8 = 5;
const SESSION_OFFSET: usize = 8;
const SEQUENCE_OFFSET: usize = 40;
const GRANT_LENGTH_OFFSET: usize = 48;
const CHILD_LENGTH_OFFSET: usize = 49;
const GRANT_OFFSET: usize = 56;
const GRANT_END: usize = GRANT_OFFSET + MAX_GRANT_ID_BYTES;
const CHILD_OFFSET: usize = GRANT_END;
const CHILD_END: usize = CHILD_OFFSET + MAX_SOCKET_CHILD_BYTES;

/// One session-bound vhost-user broker message.
#[derive(Clone, PartialEq, Eq)]
pub enum VhostUserBrokerMessage {
    /// Requests a connection to one exact granted child.
    Connect {
        /// Exact lifecycle session.
        session: SessionId,
        /// Nonzero monotonic request sequence.
        sequence: u64,
        /// Exact retained grant identifier.
        grant_id: GrantId,
        /// Exact socket child.
        child: SocketChild,
    },
    /// Returns exactly one connected stream descriptor.
    Connected {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Matching grant identifier.
        grant_id: GrantId,
        /// Matching socket child.
        child: SocketChild,
    },
    /// Reports a bounded endpoint connection failure.
    Failed {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Matching grant identifier.
        grant_id: GrantId,
        /// Matching socket child.
        child: SocketChild,
        /// Stable redacted failure category.
        kind: io::ErrorKind,
    },
}

impl fmt::Debug for VhostUserBrokerMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Connect { .. } => "Connect(<redacted>)",
            Self::Connected { .. } => "Connected(<redacted>)",
            Self::Failed { .. } => "Failed(<redacted>)",
        })
    }
}

impl VhostUserBrokerMessage {
    /// Returns the lifecycle session without formatting it.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Connect { session, .. }
            | Self::Connected { session, .. }
            | Self::Failed { session, .. } => *session,
        }
    }

    /// Returns the request sequence without formatting it.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Connect { sequence, .. }
            | Self::Connected { sequence, .. }
            | Self::Failed { sequence, .. } => *sequence,
        }
    }

    /// Returns the exact redacted grant identifier.
    #[must_use]
    pub const fn grant_id(&self) -> &GrantId {
        match self {
            Self::Connect { grant_id, .. }
            | Self::Connected { grant_id, .. }
            | Self::Failed { grant_id, .. } => grant_id,
        }
    }

    /// Returns the exact redacted child.
    #[must_use]
    pub const fn child(&self) -> &SocketChild {
        match self {
            Self::Connect { child, .. }
            | Self::Connected { child, .. }
            | Self::Failed { child, .. } => child,
        }
    }

    const fn descriptor_count(&self) -> u8 {
        if matches!(self, Self::Connected { .. }) {
            1
        } else {
            0
        }
    }
}

/// Redacted vhost broker framing or transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhostUserBrokerError {
    /// One local-socket operation failed.
    Io(io::ErrorKind),
    /// Payload or ancillary data violated the closed protocol.
    Invalid,
}

impl fmt::Display for VhostUserBrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private vhost-user broker failure")
    }
}

impl std::error::Error for VhostUserBrokerError {}

impl From<GrantTransportError> for VhostUserBrokerError {
    fn from(error: GrantTransportError) -> Self {
        match error {
            GrantTransportError::Io(kind) => Self::Io(kind),
            GrantTransportError::Invalid => Self::Invalid,
        }
    }
}

/// One validated message and its exact optional descriptor.
pub struct ReceivedVhostUserBrokerMessage {
    /// Decoded session-bound message.
    pub message: VhostUserBrokerMessage,
    /// Descriptor required by `Connected`, if any.
    pub descriptor: Option<OwnedFd>,
}

impl fmt::Debug for ReceivedVhostUserBrokerMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceivedVhostUserBrokerMessage")
            .field("message", &self.message)
            .field("descriptor", &self.descriptor.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

/// Sends one exact broker datagram and optional descriptor.
pub fn send_vhost_user_broker_message(
    socket: &UnixDatagram,
    message: &VhostUserBrokerMessage,
    descriptor: Option<RawFd>,
) -> Result<(), VhostUserBrokerError> {
    if descriptor.is_some() != (message.descriptor_count() == 1) {
        return Err(VhostUserBrokerError::Invalid);
    }
    let frame = encode(message)?;
    send_raw(socket.as_raw_fd(), &frame, descriptor.as_slice()).map_err(Into::into)
}

/// Receives one exact broker datagram and validates all ancillary data.
pub fn receive_vhost_user_broker_message(
    socket: &UnixDatagram,
) -> Result<ReceivedVhostUserBrokerMessage, VhostUserBrokerError> {
    let mut frame = [0_u8; FRAME_BYTES];
    let (length, descriptors) = receive_raw(socket, &mut frame)?;
    if length != FRAME_BYTES {
        return Err(VhostUserBrokerError::Invalid);
    }
    let message = decode(&frame)?;
    if usize::from(message.descriptor_count()) != descriptors.len() {
        return Err(VhostUserBrokerError::Invalid);
    }
    let mut descriptors = descriptors.into_iter();
    let descriptor = descriptors.next();
    if descriptors.next().is_some() {
        return Err(VhostUserBrokerError::Invalid);
    }
    Ok(ReceivedVhostUserBrokerMessage {
        message,
        descriptor,
    })
}

fn encode(message: &VhostUserBrokerMessage) -> Result<[u8; FRAME_BYTES], VhostUserBrokerError> {
    if message.session().is_pre_session() || message.sequence() == 0 {
        return Err(VhostUserBrokerError::Invalid);
    }
    let grant = message.grant_id().as_bytes();
    let child = message.child().as_bytes();
    let grant_length = u8::try_from(grant.len()).map_err(|_| VhostUserBrokerError::Invalid)?;
    let child_length = u8::try_from(child.len()).map_err(|_| VhostUserBrokerError::Invalid)?;
    let mut frame = [0_u8; FRAME_BYTES];
    frame[..4].copy_from_slice(&MAGIC);
    frame[4] = VERSION;
    frame[7] = message.descriptor_count();
    frame[SESSION_OFFSET..SEQUENCE_OFFSET].copy_from_slice(message.session().as_bytes());
    frame[SEQUENCE_OFFSET..GRANT_LENGTH_OFFSET].copy_from_slice(&message.sequence().to_be_bytes());
    frame[GRANT_LENGTH_OFFSET] = grant_length;
    frame[CHILD_LENGTH_OFFSET] = child_length;
    frame
        .get_mut(GRANT_OFFSET..GRANT_OFFSET + grant.len())
        .ok_or(VhostUserBrokerError::Invalid)?
        .copy_from_slice(grant);
    frame
        .get_mut(CHILD_OFFSET..CHILD_OFFSET + child.len())
        .ok_or(VhostUserBrokerError::Invalid)?
        .copy_from_slice(child);
    match message {
        VhostUserBrokerMessage::Connect { .. } => frame[5] = KIND_CONNECT,
        VhostUserBrokerMessage::Connected { .. } => frame[5] = KIND_CONNECTED,
        VhostUserBrokerMessage::Failed { kind, .. } => {
            frame[5] = KIND_FAILED;
            frame[6] = failure_status(*kind);
        }
    }
    Ok(frame)
}

fn decode(frame: &[u8; FRAME_BYTES]) -> Result<VhostUserBrokerMessage, VhostUserBrokerError> {
    if frame[..4] != MAGIC
        || frame[4] != VERSION
        || frame[50..GRANT_OFFSET].iter().any(|byte| *byte != 0)
        || frame[CHILD_END..].iter().any(|byte| *byte != 0)
    {
        return Err(VhostUserBrokerError::Invalid);
    }
    let session = SessionId::from_bytes(
        frame[SESSION_OFFSET..SEQUENCE_OFFSET]
            .try_into()
            .map_err(|_| VhostUserBrokerError::Invalid)?,
    );
    let sequence = u64::from_be_bytes(
        frame[SEQUENCE_OFFSET..GRANT_LENGTH_OFFSET]
            .try_into()
            .map_err(|_| VhostUserBrokerError::Invalid)?,
    );
    let grant_length = usize::from(frame[GRANT_LENGTH_OFFSET]);
    let child_length = usize::from(frame[CHILD_LENGTH_OFFSET]);
    if session.is_pre_session()
        || sequence == 0
        || grant_length == 0
        || grant_length > MAX_GRANT_ID_BYTES
        || child_length == 0
        || child_length > MAX_SOCKET_CHILD_BYTES
        || frame
            .get(GRANT_OFFSET + grant_length..GRANT_END)
            .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
        || frame
            .get(CHILD_OFFSET + child_length..CHILD_END)
            .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        return Err(VhostUserBrokerError::Invalid);
    }
    let grant = frame
        .get(GRANT_OFFSET..GRANT_OFFSET + grant_length)
        .ok_or(VhostUserBrokerError::Invalid)?;
    let child = frame
        .get(CHILD_OFFSET..CHILD_OFFSET + child_length)
        .ok_or(VhostUserBrokerError::Invalid)?;
    let grant_id = std::str::from_utf8(grant)
        .map_err(|_| VhostUserBrokerError::Invalid)
        .and_then(|value| GrantId::parse(value).map_err(|_| VhostUserBrokerError::Invalid))?;
    let child = std::str::from_utf8(child)
        .map_err(|_| VhostUserBrokerError::Invalid)
        .and_then(|value| SocketChild::parse(value).map_err(|_| VhostUserBrokerError::Invalid))?;
    match (frame[5], frame[6], frame[7]) {
        (KIND_CONNECT, STATUS_OK, 0) => Ok(VhostUserBrokerMessage::Connect {
            session,
            sequence,
            grant_id,
            child,
        }),
        (KIND_CONNECTED, STATUS_OK, 1) => Ok(VhostUserBrokerMessage::Connected {
            session,
            sequence,
            grant_id,
            child,
        }),
        (KIND_FAILED, status, 0) => Ok(VhostUserBrokerMessage::Failed {
            session,
            sequence,
            grant_id,
            child,
            kind: failure_kind(status)?,
        }),
        _ => Err(VhostUserBrokerError::Invalid),
    }
}

const fn failure_status(kind: io::ErrorKind) -> u8 {
    match kind {
        io::ErrorKind::NotFound => STATUS_NOT_FOUND,
        io::ErrorKind::PermissionDenied => STATUS_PERMISSION_DENIED,
        io::ErrorKind::ConnectionRefused => STATUS_CONNECTION_REFUSED,
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => STATUS_TIMED_OUT,
        _ => STATUS_OTHER,
    }
}

const fn failure_kind(status: u8) -> Result<io::ErrorKind, VhostUserBrokerError> {
    match status {
        STATUS_NOT_FOUND => Ok(io::ErrorKind::NotFound),
        STATUS_PERMISSION_DENIED => Ok(io::ErrorKind::PermissionDenied),
        STATUS_CONNECTION_REFUSED => Ok(io::ErrorKind::ConnectionRefused),
        STATUS_TIMED_OUT => Ok(io::ErrorKind::TimedOut),
        STATUS_OTHER => Ok(io::ErrorKind::Other),
        _ => Err(VhostUserBrokerError::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    use super::*;

    fn session() -> SessionId {
        SessionId::from_bytes([7; 32])
    }

    fn correlation() -> (GrantId, SocketChild) {
        (
            GrantId::parse("private-directory").expect("grant should parse"),
            SocketChild::parse("backend.sock").expect("child should parse"),
        )
    }

    fn connect_frame() -> [u8; FRAME_BYTES] {
        let (grant_id, child) = correlation();
        encode(&VhostUserBrokerMessage::Connect {
            session: session(),
            sequence: 1,
            grant_id,
            child,
        })
        .expect("valid connect frame should encode")
    }

    #[test]
    fn messages_round_trip_with_exact_descriptor_contract() {
        let (left, right) = UnixDatagram::pair().expect("pair should open");
        let (grant_id, child) = correlation();
        let connect = VhostUserBrokerMessage::Connect {
            session: session(),
            sequence: 1,
            grant_id: grant_id.clone(),
            child: child.clone(),
        };
        send_vhost_user_broker_message(&left, &connect, None).expect("request should send");
        let received = receive_vhost_user_broker_message(&right).expect("request should receive");
        assert_eq!(received.message, connect);
        assert!(received.descriptor.is_none());

        let descriptor = File::open("/dev/null").expect("fixture should open");
        let connected = VhostUserBrokerMessage::Connected {
            session: session(),
            sequence: 1,
            grant_id,
            child,
        };
        send_vhost_user_broker_message(&right, &connected, Some(descriptor.as_raw_fd()))
            .expect("response should send");
        let received = receive_vhost_user_broker_message(&left).expect("response should receive");
        assert_eq!(received.message, connected);
        assert!(received.descriptor.is_some());
    }

    #[test]
    fn rejects_wrong_rights_and_redacts_values() {
        let (left, _right) = UnixDatagram::pair().expect("pair should open");
        let (grant_id, child) = correlation();
        let message = VhostUserBrokerMessage::Connect {
            session: session(),
            sequence: 1,
            grant_id,
            child,
        };
        let descriptor = File::open("/dev/null").expect("fixture should open");
        assert_eq!(
            send_vhost_user_broker_message(&left, &message, Some(descriptor.as_raw_fd())),
            Err(VhostUserBrokerError::Invalid)
        );
        let debug = format!("{message:?}");
        assert_eq!(debug, "Connect(<redacted>)");
        assert_eq!(
            VhostUserBrokerError::Invalid.to_string(),
            "private vhost-user broker failure"
        );
    }

    #[test]
    fn failed_categories_round_trip_without_values() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::Other,
        ] {
            let (grant_id, child) = correlation();
            let message = VhostUserBrokerMessage::Failed {
                session: session(),
                sequence: 9,
                grant_id,
                child,
                kind,
            };
            assert_eq!(
                decode(&encode(&message).expect("encode should work")),
                Ok(message)
            );
        }
    }

    #[test]
    fn rejects_every_closed_frame_invariant() {
        type FrameCorruption = Box<dyn Fn(&mut [u8; FRAME_BYTES])>;
        let mut corruptions: Vec<FrameCorruption> = vec![
            Box::new(|frame| frame[0] = b'X'),
            Box::new(|frame| frame[4] = VERSION + 1),
            Box::new(|frame| frame[5] = 0),
            Box::new(|frame| frame[6] = STATUS_NOT_FOUND),
            Box::new(|frame| frame[7] = 1),
            Box::new(|frame| frame[SESSION_OFFSET..SEQUENCE_OFFSET].fill(0)),
            Box::new(|frame| frame[SEQUENCE_OFFSET..GRANT_LENGTH_OFFSET].fill(0)),
            Box::new(|frame| frame[GRANT_LENGTH_OFFSET] = 0),
            Box::new(|frame| frame[GRANT_LENGTH_OFFSET] = (MAX_GRANT_ID_BYTES + 1) as u8),
            Box::new(|frame| frame[CHILD_LENGTH_OFFSET] = 0),
            Box::new(|frame| frame[CHILD_LENGTH_OFFSET] = (MAX_SOCKET_CHILD_BYTES + 1) as u8),
            Box::new(|frame| frame[50] = 1),
            Box::new(|frame| frame[GRANT_OFFSET + "private-directory".len()] = 1),
            Box::new(|frame| frame[CHILD_OFFSET + "backend.sock".len()] = 1),
            Box::new(|frame| frame[CHILD_END] = 1),
        ];
        for corrupt in corruptions.drain(..) {
            let mut frame = connect_frame();
            corrupt(&mut frame);
            assert_eq!(decode(&frame), Err(VhostUserBrokerError::Invalid));
        }

        let (grant_id, child) = correlation();
        assert_eq!(
            encode(&VhostUserBrokerMessage::Connect {
                session: session(),
                sequence: 0,
                grant_id: grant_id.clone(),
                child: child.clone(),
            }),
            Err(VhostUserBrokerError::Invalid)
        );
        assert_eq!(
            encode(&VhostUserBrokerMessage::Connect {
                session: SessionId::pre_session(),
                sequence: 1,
                grant_id,
                child,
            }),
            Err(VhostUserBrokerError::Invalid)
        );
    }

    #[test]
    fn receive_rejects_short_frames_and_remote_rights_mismatches() {
        let (left, right) = UnixDatagram::pair().expect("pair should open");
        let frame = connect_frame();
        left.send(&frame[..FRAME_BYTES - 1])
            .expect("short datagram should send");
        assert!(matches!(
            receive_vhost_user_broker_message(&right),
            Err(VhostUserBrokerError::Invalid)
        ));

        let descriptor = File::open("/dev/null").expect("fixture should open");
        send_raw(left.as_raw_fd(), &frame, &[descriptor.as_raw_fd()])
            .expect("malformed rights datagram should send");
        assert!(matches!(
            receive_vhost_user_broker_message(&right),
            Err(VhostUserBrokerError::Invalid)
        ));

        let (grant_id, child) = correlation();
        let connected = encode(&VhostUserBrokerMessage::Connected {
            session: session(),
            sequence: 1,
            grant_id,
            child,
        })
        .expect("connected frame should encode");
        send_raw(left.as_raw_fd(), &connected, &[]).expect("missing-rights datagram should send");
        assert!(matches!(
            receive_vhost_user_broker_message(&right),
            Err(VhostUserBrokerError::Invalid)
        ));
    }
}
