//! Closed launcher-worker transport for granted-vsock guest connections.

use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;

use crate::{MAX_SOCKET_CHILD_BYTES, SessionId, SocketChild};

use super::grant_transport::{GrantTransportError, receive_raw, send_raw};

const FRAME_BYTES: usize = 128;
const MAGIC: [u8; 4] = *b"BBV1";
const VERSION: u8 = 1;
const KIND_ACTIVATE: u8 = 1;
const KIND_CONNECT: u8 = 2;
const KIND_SHUTDOWN: u8 = 3;
const KIND_READY: u8 = 4;
const KIND_CONNECTED: u8 = 5;
const KIND_FAILED: u8 = 6;
const KIND_COMPLETE: u8 = 7;
const STATUS_OK: u8 = 0;
const STATUS_NOT_FOUND: u8 = 1;
const STATUS_PERMISSION_DENIED: u8 = 2;
const STATUS_CONNECTION_REFUSED: u8 = 3;
const STATUS_WOULD_BLOCK: u8 = 4;
const STATUS_OTHER: u8 = 5;
const SESSION_OFFSET: usize = 8;
const SEQUENCE_OFFSET: usize = 40;
const PORT_OFFSET: usize = 48;
const CHILD_LENGTH_OFFSET: usize = 52;
const CHILD_OFFSET: usize = 56;
const CHILD_END: usize = CHILD_OFFSET + MAX_SOCKET_CHILD_BYTES;

/// One closed session-bound broker message.
#[derive(Clone, PartialEq, Eq)]
pub enum SocketBrokerMessage {
    /// Fixes the dormant broker to one already-claimed safe child.
    Activate {
        /// Exact lifecycle session.
        session: SessionId,
        /// Monotonic request sequence.
        sequence: u64,
        /// Exact child fixed for the broker lifetime.
        child: SocketChild,
    },
    /// Requests one Firecracker-compatible host-port connection.
    Connect {
        /// Exact lifecycle session.
        session: SessionId,
        /// Monotonic request sequence.
        sequence: u64,
        /// Guest-selected host port.
        port: u32,
    },
    /// Closes one healthy broker session.
    Shutdown {
        /// Exact lifecycle session.
        session: SessionId,
        /// Monotonic request sequence.
        sequence: u64,
    },
    /// Confirms activation.
    Ready {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
    },
    /// Returns exactly one connected stream descriptor.
    Connected {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Matching host port.
        port: u32,
    },
    /// Reports one bounded per-port connection failure.
    Failed {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Matching host port.
        port: u32,
        /// Stable redacted failure category.
        kind: io::ErrorKind,
    },
    /// Confirms shutdown.
    Complete {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
    },
}

impl fmt::Debug for SocketBrokerMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Activate { .. } => "Activate(<redacted>)",
            Self::Connect { .. } => "Connect(<redacted>)",
            Self::Shutdown { .. } => "Shutdown(<redacted>)",
            Self::Ready { .. } => "Ready(<redacted>)",
            Self::Connected { .. } => "Connected(<redacted>)",
            Self::Failed { .. } => "Failed(<redacted>)",
            Self::Complete { .. } => "Complete(<redacted>)",
        })
    }
}

impl SocketBrokerMessage {
    /// Returns the exact lifecycle session without formatting it.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Activate { session, .. }
            | Self::Connect { session, .. }
            | Self::Shutdown { session, .. }
            | Self::Ready { session, .. }
            | Self::Connected { session, .. }
            | Self::Failed { session, .. }
            | Self::Complete { session, .. } => *session,
        }
    }

    /// Returns the nonzero monotonic sequence without formatting it.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Activate { sequence, .. }
            | Self::Connect { sequence, .. }
            | Self::Shutdown { sequence, .. }
            | Self::Ready { sequence, .. }
            | Self::Connected { sequence, .. }
            | Self::Failed { sequence, .. }
            | Self::Complete { sequence, .. } => *sequence,
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

/// Redacted broker framing or transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketBrokerError {
    /// One local-socket operation failed.
    Io(io::ErrorKind),
    /// Payload or ancillary data violated the closed protocol.
    Invalid,
}

impl fmt::Display for SocketBrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private socket broker failure")
    }
}

impl std::error::Error for SocketBrokerError {}

impl From<GrantTransportError> for SocketBrokerError {
    fn from(error: GrantTransportError) -> Self {
        match error {
            GrantTransportError::Io(kind) => Self::Io(kind),
            GrantTransportError::Invalid => Self::Invalid,
        }
    }
}

/// One validated message and its exact optional descriptor.
pub struct ReceivedSocketBrokerMessage {
    /// Decoded session-bound message.
    pub message: SocketBrokerMessage,
    /// Descriptor required by `Connected`, if any.
    pub descriptor: Option<OwnedFd>,
}

impl fmt::Debug for ReceivedSocketBrokerMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceivedSocketBrokerMessage")
            .field("message", &self.message)
            .field("descriptor", &self.descriptor.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

/// Sends one exact broker datagram and optional descriptor.
pub fn send_socket_broker_message(
    socket: &UnixDatagram,
    message: &SocketBrokerMessage,
    descriptor: Option<RawFd>,
) -> Result<(), SocketBrokerError> {
    if descriptor.is_some() != (message.descriptor_count() == 1) {
        return Err(SocketBrokerError::Invalid);
    }
    let frame = encode(message)?;
    send_raw(socket.as_raw_fd(), &frame, descriptor.as_slice()).map_err(Into::into)
}

/// Receives one exact broker datagram and validates all ancillary data.
pub fn receive_socket_broker_message(
    socket: &UnixDatagram,
) -> Result<ReceivedSocketBrokerMessage, SocketBrokerError> {
    let mut frame = [0_u8; FRAME_BYTES];
    let (length, descriptors) = receive_raw(socket, &mut frame)?;
    if length != FRAME_BYTES {
        return Err(SocketBrokerError::Invalid);
    }
    let message = decode(&frame)?;
    if usize::from(message.descriptor_count()) != descriptors.len() {
        return Err(SocketBrokerError::Invalid);
    }
    let mut descriptors = descriptors.into_iter();
    let descriptor = descriptors.next();
    if descriptors.next().is_some() {
        return Err(SocketBrokerError::Invalid);
    }
    Ok(ReceivedSocketBrokerMessage {
        message,
        descriptor,
    })
}

fn encode(message: &SocketBrokerMessage) -> Result<[u8; FRAME_BYTES], SocketBrokerError> {
    let sequence = message.sequence();
    if message.session().is_pre_session() || sequence == 0 {
        return Err(SocketBrokerError::Invalid);
    }
    let mut frame = [0_u8; FRAME_BYTES];
    frame[..4].copy_from_slice(&MAGIC);
    frame[4] = VERSION;
    frame[7] = message.descriptor_count();
    frame[SESSION_OFFSET..SEQUENCE_OFFSET].copy_from_slice(message.session().as_bytes());
    frame[SEQUENCE_OFFSET..PORT_OFFSET].copy_from_slice(&sequence.to_be_bytes());

    match message {
        SocketBrokerMessage::Activate { child, .. } => {
            frame[5] = KIND_ACTIVATE;
            let length =
                u8::try_from(child.as_bytes().len()).map_err(|_| SocketBrokerError::Invalid)?;
            frame[CHILD_LENGTH_OFFSET] = length;
            let child_end = CHILD_OFFSET
                .checked_add(child.as_bytes().len())
                .ok_or(SocketBrokerError::Invalid)?;
            frame
                .get_mut(CHILD_OFFSET..child_end)
                .ok_or(SocketBrokerError::Invalid)?
                .copy_from_slice(child.as_bytes());
        }
        SocketBrokerMessage::Connect { port, .. } => {
            frame[5] = KIND_CONNECT;
            frame[PORT_OFFSET..PORT_OFFSET + 4].copy_from_slice(&port.to_be_bytes());
        }
        SocketBrokerMessage::Shutdown { .. } => frame[5] = KIND_SHUTDOWN,
        SocketBrokerMessage::Ready { .. } => frame[5] = KIND_READY,
        SocketBrokerMessage::Connected { port, .. } => {
            frame[5] = KIND_CONNECTED;
            frame[PORT_OFFSET..PORT_OFFSET + 4].copy_from_slice(&port.to_be_bytes());
        }
        SocketBrokerMessage::Failed { port, kind, .. } => {
            frame[5] = KIND_FAILED;
            frame[6] = failure_status(*kind);
            frame[PORT_OFFSET..PORT_OFFSET + 4].copy_from_slice(&port.to_be_bytes());
        }
        SocketBrokerMessage::Complete { .. } => frame[5] = KIND_COMPLETE,
    }
    Ok(frame)
}

fn decode(frame: &[u8; FRAME_BYTES]) -> Result<SocketBrokerMessage, SocketBrokerError> {
    if frame[..4] != MAGIC
        || frame[4] != VERSION
        || frame[53..56].iter().any(|byte| *byte != 0)
        || frame[CHILD_END..].iter().any(|byte| *byte != 0)
    {
        return Err(SocketBrokerError::Invalid);
    }
    let session = SessionId::from_bytes(
        frame[SESSION_OFFSET..SEQUENCE_OFFSET]
            .try_into()
            .map_err(|_| SocketBrokerError::Invalid)?,
    );
    let sequence = u64::from_be_bytes(
        frame[SEQUENCE_OFFSET..PORT_OFFSET]
            .try_into()
            .map_err(|_| SocketBrokerError::Invalid)?,
    );
    let port = u32::from_be_bytes(
        frame[PORT_OFFSET..PORT_OFFSET + 4]
            .try_into()
            .map_err(|_| SocketBrokerError::Invalid)?,
    );
    let child_length = usize::from(frame[CHILD_LENGTH_OFFSET]);
    let child_end = CHILD_OFFSET
        .checked_add(child_length)
        .ok_or(SocketBrokerError::Invalid)?;
    if session.is_pre_session()
        || sequence == 0
        || child_length > MAX_SOCKET_CHILD_BYTES
        || frame
            .get(child_end..CHILD_END)
            .ok_or(SocketBrokerError::Invalid)?
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(SocketBrokerError::Invalid);
    }
    let kind = frame[5];
    let status = frame[6];
    let descriptor_count = frame[7];
    let no_child = child_length == 0;
    let child = || {
        std::str::from_utf8(
            frame
                .get(CHILD_OFFSET..child_end)
                .ok_or(SocketBrokerError::Invalid)?,
        )
        .map_err(|_| SocketBrokerError::Invalid)
        .and_then(|value| SocketChild::parse(value).map_err(|_| SocketBrokerError::Invalid))
    };

    match (kind, status, descriptor_count, port, no_child) {
        (KIND_ACTIVATE, STATUS_OK, 0, 0, false) => Ok(SocketBrokerMessage::Activate {
            session,
            sequence,
            child: child()?,
        }),
        (KIND_CONNECT, STATUS_OK, 0, _, true) => Ok(SocketBrokerMessage::Connect {
            session,
            sequence,
            port,
        }),
        (KIND_SHUTDOWN, STATUS_OK, 0, 0, true) => {
            Ok(SocketBrokerMessage::Shutdown { session, sequence })
        }
        (KIND_READY, STATUS_OK, 0, 0, true) => Ok(SocketBrokerMessage::Ready { session, sequence }),
        (KIND_CONNECTED, STATUS_OK, 1, _, true) => Ok(SocketBrokerMessage::Connected {
            session,
            sequence,
            port,
        }),
        (KIND_FAILED, status, 0, _, true) => Ok(SocketBrokerMessage::Failed {
            session,
            sequence,
            port,
            kind: failure_kind(status)?,
        }),
        (KIND_COMPLETE, STATUS_OK, 0, 0, true) => {
            Ok(SocketBrokerMessage::Complete { session, sequence })
        }
        _ => Err(SocketBrokerError::Invalid),
    }
}

const fn failure_status(kind: io::ErrorKind) -> u8 {
    match kind {
        io::ErrorKind::NotFound => STATUS_NOT_FOUND,
        io::ErrorKind::PermissionDenied => STATUS_PERMISSION_DENIED,
        io::ErrorKind::ConnectionRefused => STATUS_CONNECTION_REFUSED,
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => STATUS_WOULD_BLOCK,
        _ => STATUS_OTHER,
    }
}

const fn failure_kind(status: u8) -> Result<io::ErrorKind, SocketBrokerError> {
    match status {
        STATUS_NOT_FOUND => Ok(io::ErrorKind::NotFound),
        STATUS_PERMISSION_DENIED => Ok(io::ErrorKind::PermissionDenied),
        STATUS_CONNECTION_REFUSED => Ok(io::ErrorKind::ConnectionRefused),
        STATUS_WOULD_BLOCK => Ok(io::ErrorKind::WouldBlock),
        STATUS_OTHER => Ok(io::ErrorKind::Other),
        _ => Err(SocketBrokerError::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    fn session() -> SessionId {
        SessionId::from_bytes([7; 32])
    }

    #[test]
    fn round_trips_closed_messages_and_one_descriptor() {
        let child = SocketChild::parse("vm.vsock").expect("child should parse");
        let messages = [
            SocketBrokerMessage::Activate {
                session: session(),
                sequence: 1,
                child,
            },
            SocketBrokerMessage::Connect {
                session: session(),
                sequence: 2,
                port: u32::MAX,
            },
            SocketBrokerMessage::Failed {
                session: session(),
                sequence: 2,
                port: u32::MAX,
                kind: io::ErrorKind::ConnectionRefused,
            },
            SocketBrokerMessage::Shutdown {
                session: session(),
                sequence: 3,
            },
            SocketBrokerMessage::Ready {
                session: session(),
                sequence: 1,
            },
            SocketBrokerMessage::Complete {
                session: session(),
                sequence: 3,
            },
        ];
        for message in messages {
            let encoded = encode(&message).expect("message should encode");
            assert_eq!(decode(&encoded), Ok(message));
        }

        let (sender, receiver) = UnixDatagram::pair().expect("pair should open");
        let file = File::open("/dev/null").expect("fixture should open");
        let message = SocketBrokerMessage::Connected {
            session: session(),
            sequence: 4,
            port: 52,
        };
        send_socket_broker_message(&sender, &message, Some(file.as_raw_fd()))
            .expect("descriptor response should send");
        let received =
            receive_socket_broker_message(&receiver).expect("descriptor response should receive");
        assert_eq!(received.message, message);
        assert!(received.descriptor.is_some());
    }

    #[test]
    fn rejects_reserved_fields_and_descriptor_count_mismatch() {
        let message = SocketBrokerMessage::Connect {
            session: session(),
            sequence: 2,
            port: 52,
        };
        let mut encoded = encode(&message).expect("message should encode");
        encoded[53] = 1;
        assert_eq!(decode(&encoded), Err(SocketBrokerError::Invalid));

        let (sender, receiver) = UnixDatagram::pair().expect("pair should open");
        send_raw(
            sender.as_raw_fd(),
            &encode(&message).expect("message should encode"),
            &[],
        )
        .expect("raw message should send");
        let received = receive_socket_broker_message(&receiver).expect("message should receive");
        assert!(received.descriptor.is_none());

        assert!(matches!(
            send_socket_broker_message(&sender, &message, Some(libc::STDIN_FILENO)),
            Err(SocketBrokerError::Invalid)
        ));
        assert_eq!(
            format!("{message:?} {}", SocketBrokerError::Invalid),
            "Connect(<redacted>) private socket broker failure"
        );
    }
}
