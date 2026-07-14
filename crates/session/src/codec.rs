use std::fmt;

/// Maximum encoded v1 frame size, including its fixed header.
pub const MAX_FRAME_BYTES: usize = 4096;
/// Encoded v1 header size.
pub const HEADER_BYTES: usize = 56;
const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - HEADER_BYTES;
const MAX_BUFFER_BYTES: usize = MAX_FRAME_BYTES * 2;
const MAGIC: [u8; 4] = *b"BBS1";
const VERSION: u16 = 1;

/// Random identity bound to every frame in one process session.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; 32]);

impl SessionId {
    /// Generates a cryptographically random session identity from the OS.
    pub fn generate() -> Result<Self, ProtocolError> {
        loop {
            let mut bytes = [0_u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| ProtocolError::Randomness)?;
            let identity = Self(bytes);
            if !identity.is_pre_session() {
                return Ok(identity);
            }
        }
    }

    /// Constructs an identity from exact protocol bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the reserved identity accepted only for initial worker `Hello`.
    #[must_use]
    pub const fn pre_session() -> Self {
        Self([0; 32])
    }

    /// Returns whether this is the identity reserved exclusively for `Hello`.
    #[must_use]
    pub fn is_pre_session(self) -> bool {
        self.0 == [0; 32]
    }

    /// Returns the exact identity bytes for framing and private path derivation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the fixed lowercase hexadecimal form used only for private names.
    #[must_use]
    pub fn private_hex(&self) -> String {
        let mut encoded = String::with_capacity(64);
        for byte in self.0 {
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
        encoded
    }
}

fn hex_digit(nibble: u8) -> char {
    let digit = match nibble {
        0..=9 => b'0' + nibble,
        10..=15 => b'a' + (nibble - 10),
        _ => b'?',
    };
    char::from(digit)
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionId(<redacted>)")
    }
}

/// Sender role fixed by each message kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Unsandboxed production launcher.
    Launcher,
    /// Fixed sandboxed VMM worker.
    Worker,
}

/// Graceful cancellation requested by the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSignal {
    /// Interrupt request.
    Interrupt,
    /// Termination request.
    Terminate,
}

/// Committed worker readiness kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The identity-checked API socket is published.
    Api,
    /// No-API startup is committed.
    NoApi,
}

/// Stable, path-free worker terminal category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCategory {
    /// Successful ordinary completion.
    Success,
    /// Public argument or startup configuration failure.
    Configuration,
    /// Runtime process failure.
    ProcessFailure,
    /// Graceful launcher cancellation.
    Cancelled,
    /// Exit observed without a structured worker result.
    Unstructured,
}

/// Closed v1 lifecycle message set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// Proves that the resumed worker reached its no-authority bootstrap.
    Hello,
    /// Begins a freshly spawned worker session.
    Start,
    /// Authorizes startup after launcher namespace validation.
    Proceed,
    /// Requests graceful worker cancellation.
    Cancel(CancelSignal),
    /// Reports the locked worker-container namespace identity.
    Prepared { device: u64, inode: u64 },
    /// Reports entry into public command/startup processing.
    Starting,
    /// Reports committed API or no-API readiness.
    Ready(Readiness),
    /// Reports a structured public process result.
    Terminal {
        category: TerminalCategory,
        exit_code: u8,
    },
}

impl Message {
    /// Returns the only role permitted to send this message.
    #[must_use]
    pub const fn sender(self) -> Role {
        match self {
            Self::Start | Self::Proceed | Self::Cancel(_) => Role::Launcher,
            Self::Hello
            | Self::Prepared { .. }
            | Self::Starting
            | Self::Ready(_)
            | Self::Terminal { .. } => Role::Worker,
        }
    }
}

/// One decoded protocol frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Session identity copied into every frame.
    pub session: SessionId,
    /// Exact per-direction sequence number.
    pub sequence: u64,
    /// Closed lifecycle message.
    pub message: Message,
}

/// Redacted protocol failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    /// OS randomness was unavailable.
    Randomness,
    /// A frame is malformed, oversized, or incompatible.
    InvalidFrame,
    /// A frame has the wrong session, sender, or sequence.
    InvalidPeerState,
    /// A message is invalid for the current lifecycle state.
    InvalidLifecycle,
    /// The stream ended with an incomplete frame.
    UnexpectedEof,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private session protocol failure")
    }
}

impl std::error::Error for ProtocolError {}

/// Encodes one bounded v1 frame.
pub fn encode_frame(frame: Frame) -> Result<Vec<u8>, ProtocolError> {
    let (kind, payload) = encode_message(frame.message);
    let payload_len = u32::try_from(payload.len()).map_err(|_| ProtocolError::InvalidFrame)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::InvalidFrame);
    }

    let mut encoded = Vec::with_capacity(HEADER_BYTES + payload.len());
    encoded.extend_from_slice(&MAGIC);
    encoded.extend_from_slice(&VERSION.to_be_bytes());
    encoded.extend_from_slice(&kind.to_be_bytes());
    encoded.extend_from_slice(&payload_len.to_be_bytes());
    encoded.extend_from_slice(&0_u32.to_be_bytes());
    encoded.extend_from_slice(frame.session.as_bytes());
    encoded.extend_from_slice(&frame.sequence.to_be_bytes());
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

fn encode_message(message: Message) -> (u16, Vec<u8>) {
    match message {
        Message::Hello => (8, Vec::new()),
        Message::Start => (1, Vec::new()),
        Message::Proceed => (2, Vec::new()),
        Message::Cancel(signal) => (
            3,
            vec![match signal {
                CancelSignal::Interrupt => 1,
                CancelSignal::Terminate => 2,
            }],
        ),
        Message::Prepared { device, inode } => {
            let mut payload = Vec::with_capacity(16);
            payload.extend_from_slice(&device.to_be_bytes());
            payload.extend_from_slice(&inode.to_be_bytes());
            (4, payload)
        }
        Message::Starting => (5, Vec::new()),
        Message::Ready(readiness) => (
            6,
            vec![match readiness {
                Readiness::Api => 1,
                Readiness::NoApi => 2,
            }],
        ),
        Message::Terminal {
            category,
            exit_code,
        } => (
            7,
            vec![
                match category {
                    TerminalCategory::Success => 1,
                    TerminalCategory::Configuration => 2,
                    TerminalCategory::ProcessFailure => 3,
                    TerminalCategory::Cancelled => 4,
                    TerminalCategory::Unstructured => 5,
                },
                exit_code,
            ],
        ),
    }
}

/// Incremental bounded decoder for Unix stream transport.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// Appends a bounded input chunk.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_BUFFER_BYTES {
            return Err(ProtocolError::InvalidFrame);
        }
        self.buffer.extend_from_slice(bytes);
        self.validate_advertised_size()
    }

    /// Decodes the next complete frame, retaining any coalesced following data.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtocolError> {
        self.validate_advertised_size()?;
        if self.buffer.len() < HEADER_BYTES {
            return Ok(None);
        }
        let payload_len = header_payload_len(&self.buffer)?;
        let frame_len = HEADER_BYTES
            .checked_add(payload_len)
            .ok_or(ProtocolError::InvalidFrame)?;
        if self.buffer.len() < frame_len {
            return Ok(None);
        }
        let frame = decode_complete_frame(
            self.buffer
                .get(..frame_len)
                .ok_or(ProtocolError::InvalidFrame)?,
        )?;
        self.buffer.drain(..frame_len);
        Ok(Some(frame))
    }

    /// Finishes the stream, rejecting a truncated final frame.
    pub fn finish(&self) -> Result<(), ProtocolError> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::UnexpectedEof)
        }
    }

    fn validate_advertised_size(&self) -> Result<(), ProtocolError> {
        if self.buffer.len() < HEADER_BYTES {
            return Ok(());
        }
        let payload_len = header_payload_len(&self.buffer)?;
        if HEADER_BYTES.saturating_add(payload_len) > MAX_FRAME_BYTES {
            return Err(ProtocolError::InvalidFrame);
        }
        Ok(())
    }
}

fn header_payload_len(bytes: &[u8]) -> Result<usize, ProtocolError> {
    if bytes.get(..4) != Some(MAGIC.as_slice())
        || read_u16(bytes, 4)? != VERSION
        || read_u32(bytes, 12)? != 0
    {
        return Err(ProtocolError::InvalidFrame);
    }
    usize::try_from(read_u32(bytes, 8)?).map_err(|_| ProtocolError::InvalidFrame)
}

fn decode_complete_frame(bytes: &[u8]) -> Result<Frame, ProtocolError> {
    let payload_len = header_payload_len(bytes)?;
    if bytes.len() != HEADER_BYTES.saturating_add(payload_len) {
        return Err(ProtocolError::InvalidFrame);
    }
    let kind = read_u16(bytes, 6)?;
    let session_bytes: [u8; 32] = bytes
        .get(16..48)
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    let sequence = read_u64(bytes, 48)?;
    let payload = bytes
        .get(HEADER_BYTES..)
        .ok_or(ProtocolError::InvalidFrame)?;
    let message = decode_message(kind, payload)?;
    Ok(Frame {
        session: SessionId::from_bytes(session_bytes),
        sequence,
        message,
    })
}

fn decode_message(kind: u16, payload: &[u8]) -> Result<Message, ProtocolError> {
    match (kind, payload) {
        (8, []) => Ok(Message::Hello),
        (1, []) => Ok(Message::Start),
        (2, []) => Ok(Message::Proceed),
        (3, [1]) => Ok(Message::Cancel(CancelSignal::Interrupt)),
        (3, [2]) => Ok(Message::Cancel(CancelSignal::Terminate)),
        (4, payload) if payload.len() == 16 => Ok(Message::Prepared {
            device: read_u64(payload, 0)?,
            inode: read_u64(payload, 8)?,
        }),
        (5, []) => Ok(Message::Starting),
        (6, [1]) => Ok(Message::Ready(Readiness::Api)),
        (6, [2]) => Ok(Message::Ready(Readiness::NoApi)),
        (7, [category, exit_code]) => Ok(Message::Terminal {
            category: match category {
                1 => TerminalCategory::Success,
                2 => TerminalCategory::Configuration,
                3 => TerminalCategory::ProcessFailure,
                4 => TerminalCategory::Cancelled,
                5 => TerminalCategory::Unstructured,
                _ => return Err(ProtocolError::InvalidFrame),
            },
            exit_code: *exit_code,
        }),
        _ => Err(ProtocolError::InvalidFrame),
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ProtocolError> {
    let value: [u8; 2] = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    Ok(u16::from_be_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ProtocolError> {
    let value: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    Ok(u32::from_be_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ProtocolError> {
    let value: [u8; 8] = bytes
        .get(offset..offset.saturating_add(8))
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    Ok(u64::from_be_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> SessionId {
        SessionId::from_bytes([byte; 32])
    }

    #[test]
    fn every_message_round_trips_across_every_split() {
        let messages = [
            Message::Hello,
            Message::Start,
            Message::Proceed,
            Message::Cancel(CancelSignal::Interrupt),
            Message::Cancel(CancelSignal::Terminate),
            Message::Prepared {
                device: 17,
                inode: 29,
            },
            Message::Starting,
            Message::Ready(Readiness::Api),
            Message::Ready(Readiness::NoApi),
            Message::Terminal {
                category: TerminalCategory::Configuration,
                exit_code: 152,
            },
        ];

        for message in messages {
            let frame = Frame {
                session: id(7),
                sequence: 11,
                message,
            };
            let encoded = encode_frame(frame).expect("frame should encode");
            for split in 0..=encoded.len() {
                let mut decoder = FrameDecoder::default();
                decoder
                    .push(&encoded[..split])
                    .expect("prefix should be accepted");
                assert_eq!(
                    decoder.next_frame().expect("prefix should decode"),
                    (split == encoded.len()).then_some(frame)
                );
                decoder
                    .push(&encoded[split..])
                    .expect("suffix should be accepted");
                if split != encoded.len() {
                    assert_eq!(
                        decoder.next_frame().expect("frame should decode"),
                        Some(frame)
                    );
                }
                decoder.finish().expect("decoder should be empty");
            }
        }
    }

    #[test]
    fn coalesced_frames_decode_independently() {
        let first = Frame {
            session: id(1),
            sequence: 0,
            message: Message::Start,
        };
        let second = Frame {
            session: id(1),
            sequence: 1,
            message: Message::Proceed,
        };
        let mut bytes = encode_frame(first).expect("first should encode");
        bytes.extend(encode_frame(second).expect("second should encode"));
        let mut decoder = FrameDecoder::default();
        decoder.push(&bytes).expect("frames should buffer");
        assert_eq!(
            decoder.next_frame().expect("first should decode"),
            Some(first)
        );
        assert_eq!(
            decoder.next_frame().expect("second should decode"),
            Some(second)
        );
        assert_eq!(decoder.next_frame().expect("buffer should empty"), None);
    }

    #[test]
    fn rejects_oversize_before_payload_arrives() {
        let mut encoded = encode_frame(Frame {
            session: id(3),
            sequence: 0,
            message: Message::Start,
        })
        .expect("frame should encode");
        encoded[8..12].copy_from_slice(&(MAX_FRAME_BYTES as u32).to_be_bytes());
        let mut decoder = FrameDecoder::default();
        assert_eq!(
            decoder.push(&encoded[..HEADER_BYTES]),
            Err(ProtocolError::InvalidFrame)
        );
    }

    #[test]
    fn rejects_incompatible_reserved_unknown_and_truncated_data() {
        let frame = Frame {
            session: id(5),
            sequence: 0,
            message: Message::Start,
        };
        for (offset, bytes) in [
            (4, 2_u16.to_be_bytes().to_vec()),
            (12, 1_u32.to_be_bytes().to_vec()),
        ] {
            let mut encoded = encode_frame(frame).expect("frame should encode");
            let end = offset + bytes.len();
            encoded[offset..end].copy_from_slice(&bytes);
            let mut decoder = FrameDecoder::default();
            assert_eq!(decoder.push(&encoded), Err(ProtocolError::InvalidFrame));
        }

        let mut unknown = encode_frame(frame).expect("frame should encode");
        unknown[6..8].copy_from_slice(&99_u16.to_be_bytes());
        let mut decoder = FrameDecoder::default();
        decoder.push(&unknown).expect("bounded frame should buffer");
        assert_eq!(decoder.next_frame(), Err(ProtocolError::InvalidFrame));

        let encoded = encode_frame(frame).expect("frame should encode");
        let mut decoder = FrameDecoder::default();
        decoder
            .push(&encoded[..encoded.len() - 1])
            .expect("truncated frame should buffer");
        assert_eq!(decoder.finish(), Err(ProtocolError::UnexpectedEof));
    }

    #[test]
    fn session_debug_and_errors_are_redacted() {
        let identity = id(0xab);
        assert_eq!(format!("{identity:?}"), "SessionId(<redacted>)");
        assert!(!format!("{identity:?}").contains(&identity.private_hex()));
        assert_eq!(
            ProtocolError::InvalidPeerState.to_string(),
            "private session protocol failure"
        );
    }
}
