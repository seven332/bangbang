use std::fmt;

use crate::BatchId;

/// Maximum encoded v4 frame size, including its fixed header.
pub const MAX_FRAME_BYTES: usize = 4096;
/// Encoded v4 header size.
pub const HEADER_BYTES: usize = 56;
const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - HEADER_BYTES;
const MAX_BUFFER_BYTES: usize = MAX_FRAME_BYTES * 2;
const MAGIC: [u8; 4] = *b"BBS4";
const VERSION: u16 = 4;
const CREDENTIAL_POLICY_BYTES: usize = 32;
const VMNET_AUTHORITY_BYTES: usize = 64;
const WORKER_POLICY_BYTES: usize = CREDENTIAL_POLICY_BYTES + VMNET_AUTHORITY_BYTES;
const POLICY_FLAG_FILE_SIZE: u16 = 1 << 0;
const POLICY_FLAG_DAEMONIZED: u16 = 1 << 1;
const POLICY_FLAGS: u16 = POLICY_FLAG_FILE_SIZE | POLICY_FLAG_DAEMONIZED;
const VMNET_FLAG_HOST: u8 = 1 << 0;
const VMNET_FLAG_SHARED: u8 = 1 << 1;
const VMNET_FLAGS: u8 = VMNET_FLAG_HOST | VMNET_FLAG_SHARED;

/// Maximum number of active vmnet interfaces admitted for one worker.
pub const MAX_VMNET_ACTIVE_INTERFACES: u8 = 4;
/// Maximum number of exact bridged-interface names in one authority.
pub const MAX_VMNET_BRIDGE_NAMES: usize = 4;
/// Maximum byte length of one bridged-interface name in production policy.
pub const MAX_VMNET_BRIDGE_NAME_BYTES: usize = 15;

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

/// A malformed or internally inconsistent production vmnet authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmnetAuthorityError;

impl fmt::Display for VmnetAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid private vmnet authority")
    }
}

impl std::error::Error for VmnetAuthorityError {}

/// Fixed, bounded production authority for system vmnet acquisition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VmnetAuthority {
    allow_host: bool,
    allow_shared: bool,
    max_interfaces: u8,
    bridge_count: u8,
    bridges: [[u8; MAX_VMNET_BRIDGE_NAME_BYTES]; MAX_VMNET_BRIDGE_NAMES],
}

impl VmnetAuthority {
    /// Returns the unique authority that admits no system vmnet interfaces.
    #[must_use]
    pub const fn denied() -> Self {
        Self {
            allow_host: false,
            allow_shared: false,
            max_interfaces: 0,
            bridge_count: 0,
            bridges: [[0; MAX_VMNET_BRIDGE_NAME_BYTES]; MAX_VMNET_BRIDGE_NAMES],
        }
    }

    /// Constructs one nonempty, canonical production vmnet authority.
    pub fn try_new(
        allow_host: bool,
        allow_shared: bool,
        max_interfaces: u8,
        bridges: &[&str],
    ) -> Result<Self, VmnetAuthorityError> {
        if !(1..=MAX_VMNET_ACTIVE_INTERFACES).contains(&max_interfaces)
            || (!allow_host && !allow_shared && bridges.is_empty())
            || bridges.len() > MAX_VMNET_BRIDGE_NAMES
        {
            return Err(VmnetAuthorityError);
        }

        let mut encoded_bridges = [[0_u8; MAX_VMNET_BRIDGE_NAME_BYTES]; MAX_VMNET_BRIDGE_NAMES];
        for (index, bridge) in bridges.iter().enumerate() {
            let bytes = bridge.as_bytes();
            if bytes.is_empty()
                || bytes.len() > MAX_VMNET_BRIDGE_NAME_BYTES
                || !bytes.iter().copied().all(is_vmnet_bridge_name_byte)
                || bridges
                    .get(..index)
                    .ok_or(VmnetAuthorityError)?
                    .contains(bridge)
            {
                return Err(VmnetAuthorityError);
            }
            encoded_bridges
                .get_mut(index)
                .and_then(|encoded| encoded.get_mut(..bytes.len()))
                .ok_or(VmnetAuthorityError)?
                .copy_from_slice(bytes);
        }

        Ok(Self {
            allow_host,
            allow_shared,
            max_interfaces,
            bridge_count: u8::try_from(bridges.len()).map_err(|_| VmnetAuthorityError)?,
            bridges: encoded_bridges,
        })
    }

    /// Returns whether this authority admits no system vmnet acquisition.
    #[must_use]
    pub const fn is_denied(self) -> bool {
        self.max_interfaces == 0
    }

    /// Returns whether host-mode vmnet is admitted.
    #[must_use]
    pub const fn allows_host(self) -> bool {
        self.allow_host
    }

    /// Returns whether shared-mode vmnet is admitted.
    #[must_use]
    pub const fn allows_shared(self) -> bool {
        self.allow_shared
    }

    /// Returns the admitted active-interface maximum, or `None` when denied.
    #[must_use]
    pub const fn max_interfaces(self) -> Option<u8> {
        if self.is_denied() {
            None
        } else {
            Some(self.max_interfaces)
        }
    }

    /// Returns whether the exact bridged-interface name is admitted.
    #[must_use]
    pub fn allows_bridge(self, bridge: &str) -> bool {
        let candidate = bridge.as_bytes();
        self.bridges
            .get(..usize::from(self.bridge_count))
            .is_some_and(|bridges| {
                bridges
                    .iter()
                    .any(|encoded| bridge_name_bytes(encoded) == candidate)
            })
    }

    const fn flags(self) -> u8 {
        (if self.allow_host { VMNET_FLAG_HOST } else { 0 })
            | (if self.allow_shared {
                VMNET_FLAG_SHARED
            } else {
                0
            })
    }
}

impl Default for VmnetAuthority {
    fn default() -> Self {
        Self::denied()
    }
}

impl fmt::Debug for VmnetAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetAuthority(<redacted>)")
    }
}

const fn is_vmnet_bridge_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn bridge_name_bytes(encoded: &[u8; MAX_VMNET_BRIDGE_NAME_BYTES]) -> &[u8] {
    let len = encoded
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(encoded.len());
    encoded.get(..len).unwrap_or(&[])
}

/// Fixed production-worker launch policy authenticated by the private lifecycle.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WorkerPolicy {
    uid: u32,
    gid: u32,
    no_file: u64,
    file_size: Option<u64>,
    daemonized: bool,
    vmnet_authority: VmnetAuthority,
}

impl WorkerPolicy {
    /// Constructs an exact worker launch policy.
    #[must_use]
    pub const fn new(
        uid: u32,
        gid: u32,
        no_file: u64,
        file_size: Option<u64>,
        daemonized: bool,
    ) -> Self {
        Self {
            uid,
            gid,
            no_file,
            file_size,
            daemonized,
            vmnet_authority: VmnetAuthority::denied(),
        }
    }

    /// Attaches the immutable production vmnet authority to this policy.
    #[must_use]
    pub const fn with_vmnet_authority(mut self, authority: VmnetAuthority) -> Self {
        self.vmnet_authority = authority;
        self
    }

    /// Returns the required real and effective user identity.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the required real and effective group identity.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }

    /// Returns the exact worker `RLIMIT_NOFILE` soft and hard value.
    #[must_use]
    pub const fn no_file(self) -> u64 {
        self.no_file
    }

    /// Returns the optional exact worker `RLIMIT_FSIZE` soft and hard value.
    #[must_use]
    pub const fn file_size(self) -> Option<u64> {
        self.file_size
    }

    /// Returns whether the outer launcher detached into its daemon session.
    #[must_use]
    pub const fn is_daemonized(self) -> bool {
        self.daemonized
    }

    /// Returns the immutable production vmnet authority.
    #[must_use]
    pub const fn vmnet_authority(self) -> VmnetAuthority {
        self.vmnet_authority
    }
}

impl fmt::Debug for WorkerPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorkerPolicy(<redacted>)")
    }
}

/// Closed v4 lifecycle message set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// Proves that the resumed worker reached its no-authority bootstrap.
    Hello,
    /// Begins a freshly spawned worker session.
    Start(WorkerPolicy),
    /// Authorizes startup after launcher namespace validation.
    Proceed,
    /// Requests graceful worker cancellation.
    Cancel(CancelSignal),
    /// Reports the locked worker-container namespace identity.
    Prepared { device: u64, inode: u64 },
    /// Reports atomic acceptance of the startup grant batch.
    GrantsAccepted {
        /// Exact redacted batch identity.
        batch: BatchId,
        /// Number of semantic grants accepted.
        grant_count: u16,
        /// Final launcher-to-worker grant record sequence.
        final_sequence: u64,
    },
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

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hello => formatter.write_str("Hello"),
            Self::Start(_) => formatter.write_str("Start(<redacted>)"),
            Self::Proceed => formatter.write_str("Proceed"),
            Self::Cancel(_) => formatter.write_str("Cancel(<redacted>)"),
            Self::Prepared { .. } => {
                formatter.write_str("Prepared { device: <redacted>, inode: <redacted> }")
            }
            Self::GrantsAccepted { .. } => formatter.write_str(
                "GrantsAccepted { batch: <redacted>, grant_count: <redacted>, final_sequence: <redacted> }",
            ),
            Self::Starting => formatter.write_str("Starting"),
            Self::Ready(_) => formatter.write_str("Ready(<redacted>)"),
            Self::Terminal { .. } => {
                formatter.write_str("Terminal { category: <redacted>, exit_code: <redacted> }")
            }
        }
    }
}

impl Message {
    /// Returns the only role permitted to send this message.
    #[must_use]
    pub const fn sender(self) -> Role {
        match self {
            Self::Start(_) | Self::Proceed | Self::Cancel(_) => Role::Launcher,
            Self::Hello
            | Self::Prepared { .. }
            | Self::GrantsAccepted { .. }
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

/// Encodes one bounded v4 frame.
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
        Message::Start(policy) => (1, encode_worker_policy(policy)),
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
        Message::GrantsAccepted {
            batch,
            grant_count,
            final_sequence,
        } => {
            let mut payload = Vec::with_capacity(26);
            payload.extend_from_slice(batch.as_bytes());
            payload.extend_from_slice(&grant_count.to_be_bytes());
            payload.extend_from_slice(&final_sequence.to_be_bytes());
            (9, payload)
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

fn encode_worker_policy(policy: WorkerPolicy) -> Vec<u8> {
    let mut flags = 0_u16;
    if policy.file_size.is_some() {
        flags |= POLICY_FLAG_FILE_SIZE;
    }
    if policy.daemonized {
        flags |= POLICY_FLAG_DAEMONIZED;
    }
    let mut payload = Vec::with_capacity(WORKER_POLICY_BYTES);
    payload.extend_from_slice(&flags.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    payload.extend_from_slice(&policy.uid.to_be_bytes());
    payload.extend_from_slice(&policy.gid.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&policy.no_file.to_be_bytes());
    payload.extend_from_slice(&policy.file_size.unwrap_or(0).to_be_bytes());
    let authority = policy.vmnet_authority;
    payload.push(authority.flags());
    payload.push(authority.max_interfaces);
    payload.push(authority.bridge_count);
    payload.push(0);
    for bridge in authority.bridges {
        payload.extend_from_slice(&bridge);
    }
    debug_assert_eq!(payload.len(), WORKER_POLICY_BYTES);
    payload
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
        (1, payload) if payload.len() == WORKER_POLICY_BYTES => {
            Ok(Message::Start(decode_worker_policy(payload)?))
        }
        (2, []) => Ok(Message::Proceed),
        (3, [1]) => Ok(Message::Cancel(CancelSignal::Interrupt)),
        (3, [2]) => Ok(Message::Cancel(CancelSignal::Terminate)),
        (4, payload) if payload.len() == 16 => Ok(Message::Prepared {
            device: read_u64(payload, 0)?,
            inode: read_u64(payload, 8)?,
        }),
        (9, payload) if payload.len() == 26 => {
            let batch: [u8; 16] = payload
                .get(..16)
                .ok_or(ProtocolError::InvalidFrame)?
                .try_into()
                .map_err(|_| ProtocolError::InvalidFrame)?;
            let batch = BatchId::from_bytes(batch);
            if batch.is_zero() {
                return Err(ProtocolError::InvalidFrame);
            }
            Ok(Message::GrantsAccepted {
                batch,
                grant_count: read_u16(payload, 16)?,
                final_sequence: read_u64(payload, 18)?,
            })
        }
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

fn decode_worker_policy(payload: &[u8]) -> Result<WorkerPolicy, ProtocolError> {
    let flags = read_u16(payload, 0)?;
    if flags & !POLICY_FLAGS != 0 || read_u16(payload, 2)? != 0 || read_u32(payload, 12)? != 0 {
        return Err(ProtocolError::InvalidFrame);
    }
    let encoded_file_size = read_u64(payload, 24)?;
    let file_size = if flags & POLICY_FLAG_FILE_SIZE == 0 {
        if encoded_file_size != 0 {
            return Err(ProtocolError::InvalidFrame);
        }
        None
    } else {
        Some(encoded_file_size)
    };
    let vmnet_authority = decode_vmnet_authority(
        payload
            .get(CREDENTIAL_POLICY_BYTES..)
            .ok_or(ProtocolError::InvalidFrame)?,
    )?;
    Ok(WorkerPolicy::new(
        read_u32(payload, 4)?,
        read_u32(payload, 8)?,
        read_u64(payload, 16)?,
        file_size,
        flags & POLICY_FLAG_DAEMONIZED != 0,
    )
    .with_vmnet_authority(vmnet_authority))
}

fn decode_vmnet_authority(payload: &[u8]) -> Result<VmnetAuthority, ProtocolError> {
    if payload.len() != VMNET_AUTHORITY_BYTES {
        return Err(ProtocolError::InvalidFrame);
    }
    let [flags, max_interfaces, encoded_bridge_count, reserved]: [u8; 4] = payload
        .get(..4)
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    if flags & !VMNET_FLAGS != 0
        || reserved != 0
        || usize::from(encoded_bridge_count) > MAX_VMNET_BRIDGE_NAMES
    {
        return Err(ProtocolError::InvalidFrame);
    }

    let bridge_count = usize::from(encoded_bridge_count);
    let mut bridges = [""; MAX_VMNET_BRIDGE_NAMES];
    for (index, bridge) in bridges.iter_mut().enumerate() {
        let offset = 4 + index * MAX_VMNET_BRIDGE_NAME_BYTES;
        let slot = payload
            .get(offset..offset + MAX_VMNET_BRIDGE_NAME_BYTES)
            .ok_or(ProtocolError::InvalidFrame)?;
        if index >= bridge_count {
            if slot.iter().any(|byte| *byte != 0) {
                return Err(ProtocolError::InvalidFrame);
            }
            continue;
        }

        let len = slot
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(slot.len());
        if len == 0
            || slot
                .get(len..)
                .ok_or(ProtocolError::InvalidFrame)?
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(ProtocolError::InvalidFrame);
        }
        *bridge = std::str::from_utf8(slot.get(..len).ok_or(ProtocolError::InvalidFrame)?)
            .map_err(|_| ProtocolError::InvalidFrame)?;
    }

    let allow_host = flags & VMNET_FLAG_HOST != 0;
    let allow_shared = flags & VMNET_FLAG_SHARED != 0;
    if !allow_host && !allow_shared && bridge_count == 0 {
        return (max_interfaces == 0)
            .then_some(VmnetAuthority::denied())
            .ok_or(ProtocolError::InvalidFrame);
    }

    VmnetAuthority::try_new(
        allow_host,
        allow_shared,
        max_interfaces,
        bridges
            .get(..bridge_count)
            .ok_or(ProtocolError::InvalidFrame)?,
    )
    .map_err(|_| ProtocolError::InvalidFrame)
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

    const fn policy() -> WorkerPolicy {
        WorkerPolicy::new(501, 20, 2048, Some(4096), true)
    }

    #[test]
    fn every_message_round_trips_across_every_split() {
        let messages = [
            Message::Hello,
            Message::Start(policy()),
            Message::Proceed,
            Message::Cancel(CancelSignal::Interrupt),
            Message::Cancel(CancelSignal::Terminate),
            Message::Prepared {
                device: 17,
                inode: 29,
            },
            Message::GrantsAccepted {
                batch: BatchId::from_bytes([3; 16]),
                grant_count: 2,
                final_sequence: 4,
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
            message: Message::Start(policy()),
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
            message: Message::Start(policy()),
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
    fn accepts_the_exact_frame_and_buffer_limits_before_shape_validation() {
        let mut exact = encode_frame(Frame {
            session: id(4),
            sequence: 0,
            message: Message::Start(policy()),
        })
        .expect("frame should encode");
        exact[6..8].copy_from_slice(&99_u16.to_be_bytes());
        exact[8..12].copy_from_slice(&(MAX_PAYLOAD_BYTES as u32).to_be_bytes());
        exact.resize(MAX_FRAME_BYTES, 0);

        let mut exact_decoder = FrameDecoder::default();
        exact_decoder
            .push(&exact)
            .expect("the exact frame limit should buffer");
        assert_eq!(
            exact_decoder.next_frame(),
            Err(ProtocolError::InvalidFrame),
            "the closed message shape should be rejected after the size is admitted"
        );

        let mut buffer_decoder = FrameDecoder::default();
        buffer_decoder
            .push(&exact)
            .expect("first maximum frame should buffer");
        buffer_decoder
            .push(&exact)
            .expect("the exact two-frame buffer limit should buffer");
        assert_eq!(buffer_decoder.push(&[0]), Err(ProtocolError::InvalidFrame));
    }

    #[test]
    fn rejects_incompatible_reserved_unknown_and_truncated_data() {
        let frame = Frame {
            session: id(5),
            sequence: 0,
            message: Message::Start(policy()),
        };
        for (offset, bytes) in [
            (0, b"FAIL".to_vec()),
            (4, 1_u16.to_be_bytes().to_vec()),
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

        let prepared = Message::Prepared {
            device: 1_234_567_891,
            inode: 1_234_567_893,
        };
        let terminal = Message::Terminal {
            category: TerminalCategory::Configuration,
            exit_code: 152,
        };
        assert_eq!(
            format!("{prepared:?}"),
            "Prepared { device: <redacted>, inode: <redacted> }"
        );
        assert_eq!(
            format!("{terminal:?}"),
            "Terminal { category: <redacted>, exit_code: <redacted> }"
        );
        let policy = WorkerPolicy::new(1_234_567_891, 1_234_567_893, 65_535, Some(8_192), true);
        assert_eq!(format!("{policy:?}"), "WorkerPolicy(<redacted>)");
        assert_eq!(format!("{:?}", Message::Start(policy)), "Start(<redacted>)");
        let debug = format!("{policy:?} {:?}", Message::Start(policy));
        for sensitive in ["1234567891", "1234567893", "65535", "8192"] {
            assert!(!debug.contains(sensitive));
        }
    }

    #[test]
    fn worker_policy_rejects_unknown_flags_reserved_bytes_and_ambiguous_optional_values() {
        let frame = Frame {
            session: id(6),
            sequence: 0,
            message: Message::Start(WorkerPolicy::new(501, 20, 2048, None, false)),
        };
        for (offset, bytes) in [
            (HEADER_BYTES, 4_u16.to_be_bytes().to_vec()),
            (HEADER_BYTES + 2, 1_u16.to_be_bytes().to_vec()),
            (HEADER_BYTES + 12, 1_u32.to_be_bytes().to_vec()),
            (HEADER_BYTES + 24, 1_u64.to_be_bytes().to_vec()),
        ] {
            let mut encoded = encode_frame(frame).expect("policy should encode");
            encoded[offset..offset + bytes.len()].copy_from_slice(&bytes);
            let mut decoder = FrameDecoder::default();
            decoder.push(&encoded).expect("bounded frame should buffer");
            assert_eq!(decoder.next_frame(), Err(ProtocolError::InvalidFrame));
        }
    }

    #[test]
    fn vmnet_authority_validates_exact_bounded_modes_and_names() {
        let denied = VmnetAuthority::denied();
        assert!(denied.is_denied());
        assert_eq!(denied.max_interfaces(), None);
        assert!(!denied.allows_host());
        assert!(!denied.allows_shared());
        assert!(!denied.allows_bridge("en0"));

        let authority = VmnetAuthority::try_new(
            true,
            true,
            4,
            &["en0", "bridge_1", "a.b-c", "abcdefghijklmno"],
        )
        .expect("bounded policy should validate");
        assert!(!authority.is_denied());
        assert_eq!(authority.max_interfaces(), Some(4));
        assert!(authority.allows_host());
        assert!(authority.allows_shared());
        for bridge in ["en0", "bridge_1", "a.b-c", "abcdefghijklmno"] {
            assert!(authority.allows_bridge(bridge));
        }
        assert!(!authority.allows_bridge("EN0"));
        assert!(!authority.allows_bridge("en"));

        for (allow_host, allow_shared, maximum, bridges) in [
            (false, false, 1, Vec::new()),
            (true, false, 0, Vec::new()),
            (false, true, 5, Vec::new()),
            (false, false, 1, vec![""]),
            (false, false, 1, vec!["en$0"]),
            (false, false, 1, vec!["abcdefghijklmnop"]),
            (false, false, 1, vec!["en0", "en0"]),
            (false, false, 4, vec!["a", "b", "c", "d", "e"]),
        ] {
            assert_eq!(
                VmnetAuthority::try_new(allow_host, allow_shared, maximum, &bridges),
                Err(VmnetAuthorityError)
            );
        }
    }

    #[test]
    fn vmnet_authority_round_trips_in_the_fixed_start_payload() {
        let authority = VmnetAuthority::try_new(true, true, 3, &["en0", "bridge_1"])
            .expect("authority should validate");
        let policy =
            WorkerPolicy::new(501, 20, 2048, Some(4096), true).with_vmnet_authority(authority);
        let frame = Frame {
            session: id(0x71),
            sequence: 19,
            message: Message::Start(policy),
        };
        let encoded = encode_frame(frame).expect("policy should encode");
        assert_eq!(encoded.len(), HEADER_BYTES + WORKER_POLICY_BYTES);
        assert_eq!(&encoded[..4], b"BBS4");
        assert_eq!(&encoded[4..6], &4_u16.to_be_bytes());

        let mut decoder = FrameDecoder::default();
        decoder.push(&encoded).expect("frame should buffer");
        assert_eq!(decoder.next_frame(), Ok(Some(frame)));
        assert_eq!(policy.vmnet_authority(), authority);
    }

    #[test]
    fn vmnet_authority_decode_rejects_every_noncanonical_shape() {
        let denied_frame = Frame {
            session: id(0x72),
            sequence: 0,
            message: Message::Start(WorkerPolicy::new(501, 20, 2048, None, false)),
        };
        let authority_offset = HEADER_BYTES + CREDENTIAL_POLICY_BYTES;
        for (offset, value) in [
            (authority_offset, 4),
            (authority_offset + 1, 1),
            (authority_offset + 2, 5),
            (authority_offset + 3, 1),
            (authority_offset + 4, b'e'),
        ] {
            let mut encoded = encode_frame(denied_frame).expect("policy should encode");
            encoded[offset] = value;
            let mut decoder = FrameDecoder::default();
            decoder.push(&encoded).expect("bounded frame should buffer");
            assert_eq!(decoder.next_frame(), Err(ProtocolError::InvalidFrame));
        }

        let authority = VmnetAuthority::try_new(false, false, 2, &["en0", "en1"])
            .expect("authority should validate");
        let frame = Frame {
            session: id(0x73),
            sequence: 0,
            message: Message::Start(
                WorkerPolicy::new(501, 20, 2048, None, false).with_vmnet_authority(authority),
            ),
        };
        let mutations: [fn(&mut [u8], usize); 6] = [
            |bytes, offset| bytes[offset + 1] = 0,
            |bytes, offset| bytes[offset + 1] = 5,
            |bytes, offset| bytes[offset + 4] = b'$',
            |bytes, offset| bytes[offset + 4 + 4] = b'x',
            |bytes, offset| {
                let first = offset + 4;
                let second = first + MAX_VMNET_BRIDGE_NAME_BYTES;
                bytes.copy_within(first..first + 3, second);
            },
            |bytes, offset| bytes[offset + 2] = 0,
        ];
        for mutate in mutations {
            let mut encoded = encode_frame(frame).expect("policy should encode");
            mutate(&mut encoded, authority_offset);
            let mut decoder = FrameDecoder::default();
            decoder.push(&encoded).expect("bounded frame should buffer");
            assert_eq!(decoder.next_frame(), Err(ProtocolError::InvalidFrame));
        }
    }

    #[test]
    fn vmnet_authority_debug_and_errors_do_not_reveal_policy_values() {
        let authority = VmnetAuthority::try_new(false, false, 1, &["secret_bridge"])
            .expect("authority should validate");
        assert_eq!(format!("{authority:?}"), "VmnetAuthority(<redacted>)");
        assert!(!format!("{authority:?}").contains("secret_bridge"));
        assert_eq!(
            VmnetAuthorityError.to_string(),
            "invalid private vmnet authority"
        );
    }
}
