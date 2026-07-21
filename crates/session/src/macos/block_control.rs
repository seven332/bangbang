//! Closed launcher-worker transport for contained block-device control.

use std::fmt;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;

use crate::{
    BlockDeviceGrant, GrantAccess, GrantId, MAX_GRANT_ID_BYTES, ObjectIdentity, SessionId,
};

use super::grant_transport::{GrantTransportError, receive_raw, send_raw};

const FRAME_BYTES: usize = 256;
const MAGIC: [u8; 4] = *b"BBC1";
const VERSION: u8 = 1;
const OBJECT_KIND_BLOCK_DEVICE: u8 = 3;
const KIND_INSPECT: u8 = 1;
const KIND_INSPECTED: u8 = 2;
const KIND_SYNCHRONIZE_CACHE: u8 = 3;
const KIND_SYNCHRONIZED: u8 = 4;
const KIND_FAILED: u8 = 5;
const OPERATION_NONE: u8 = 0;
const OPERATION_INSPECT: u8 = 1;
const OPERATION_SYNCHRONIZE_CACHE: u8 = 2;
const STATUS_OK: u8 = 0;
const STATUS_PERMISSION_DENIED: u8 = 1;
const STATUS_TIMED_OUT: u8 = 2;
const STATUS_INVALID_DATA: u8 = 3;
const STATUS_NOT_FOUND: u8 = 4;
const STATUS_OTHER: u8 = 5;
const SESSION_OFFSET: usize = 8;
const SEQUENCE_OFFSET: usize = 40;
const GRANT_LENGTH_OFFSET: usize = 48;
const ACCESS_OFFSET: usize = 49;
const OPERATION_OFFSET: usize = 50;
const STATUS_FLAGS_OFFSET: usize = 52;
const DEVICE_OFFSET: usize = 56;
const INODE_OFFSET: usize = 64;
const TARGET_DEVICE_OFFSET: usize = 72;
const LOGICAL_BLOCK_SIZE_OFFSET: usize = 80;
const BLOCK_COUNT_OFFSET: usize = 88;
const CAPACITY_OFFSET: usize = 96;
const OBSERVED_BLOCK_SIZE_OFFSET: usize = 104;
const OBSERVED_BLOCK_COUNT_OFFSET: usize = 112;
const OBSERVED_CAPACITY_OFFSET: usize = 120;
const GRANT_OFFSET: usize = 128;
const GRANT_END: usize = GRANT_OFFSET + MAX_GRANT_ID_BYTES;

/// Immutable grant tuple bound to every block-control exchange.
#[derive(Clone, PartialEq, Eq)]
pub struct BlockControlTarget {
    grant_id: GrantId,
    access: GrantAccess,
    identity: ObjectIdentity,
    status_flags: u32,
    block_device: BlockDeviceGrant,
}

impl BlockControlTarget {
    /// Builds one role-restricted authenticated block target.
    pub fn new(
        grant_id: GrantId,
        access: GrantAccess,
        identity: ObjectIdentity,
        status_flags: u32,
        block_device: BlockDeviceGrant,
    ) -> Option<Self> {
        if !matches!(access, GrantAccess::ReadOnly | GrantAccess::ReadWrite)
            || !target_status_matches_access(status_flags, access)
        {
            return None;
        }
        Some(Self {
            grant_id,
            access,
            identity,
            status_flags,
            block_device,
        })
    }

    /// Returns the exact redacted grant identifier.
    #[must_use]
    pub const fn grant_id(&self) -> &GrantId {
        &self.grant_id
    }

    /// Returns the authenticated opened access.
    #[must_use]
    pub const fn access(&self) -> GrantAccess {
        self.access
    }

    /// Returns the authenticated containing-device and inode identity.
    #[must_use]
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
    }

    /// Returns the authenticated stable status flags.
    #[must_use]
    pub const fn status_flags(&self) -> u32 {
        self.status_flags
    }

    /// Returns the authenticated target-device and adopted geometry tuple.
    #[must_use]
    pub const fn block_device(&self) -> BlockDeviceGrant {
        self.block_device
    }
}

fn target_status_matches_access(status_flags: u32, access: GrantAccess) -> bool {
    let Ok(flags) = libc::c_int::try_from(status_flags) else {
        return false;
    };
    if super::normalized_block_status_flags(flags) != Some(status_flags)
        || flags & libc::O_APPEND != 0
    {
        return false;
    }
    match access {
        GrantAccess::ReadOnly => flags & libc::O_ACCMODE == libc::O_RDONLY,
        GrantAccess::ReadWrite => flags & libc::O_ACCMODE == libc::O_RDWR,
        GrantAccess::WriteOnly | GrantAccess::CreateChildren | GrantAccess::ConnectChildren => {
            false
        }
    }
}

impl fmt::Debug for BlockControlTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BlockControlTarget(<redacted>)")
    }
}

/// Closed block-control operation used to correlate bounded failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockControlOperation {
    /// Read live logical geometry.
    Inspect,
    /// Persist writes through the media cache.
    SynchronizeCache,
}

/// One session-bound block-control message.
#[derive(Clone, PartialEq, Eq)]
pub enum BlockControlMessage {
    /// Requests a fresh live geometry observation.
    Inspect {
        /// Exact lifecycle session.
        session: SessionId,
        /// Nonzero monotonic request sequence.
        sequence: u64,
        /// Exact adopted grant tuple.
        target: BlockControlTarget,
    },
    /// Returns a fresh checked geometry while echoing the immutable target.
    Inspected {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Exact adopted grant tuple.
        target: BlockControlTarget,
        /// Fresh checked geometry for the same target device.
        observed: BlockDeviceGrant,
    },
    /// Requests cache synchronization for the exact adopted geometry.
    SynchronizeCache {
        /// Exact lifecycle session.
        session: SessionId,
        /// Nonzero monotonic request sequence.
        sequence: u64,
        /// Exact adopted grant tuple.
        target: BlockControlTarget,
    },
    /// Acknowledges cache synchronization and post-operation reinspection.
    Synchronized {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Exact adopted grant tuple.
        target: BlockControlTarget,
    },
    /// Reports one bounded, unambiguous endpoint failure.
    Failed {
        /// Exact lifecycle session.
        session: SessionId,
        /// Matching request sequence.
        sequence: u64,
        /// Exact adopted grant tuple.
        target: BlockControlTarget,
        /// Operation that failed.
        operation: BlockControlOperation,
        /// Stable redacted failure category.
        kind: io::ErrorKind,
    },
}

impl fmt::Debug for BlockControlMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Inspect { .. } => "Inspect(<redacted>)",
            Self::Inspected { .. } => "Inspected(<redacted>)",
            Self::SynchronizeCache { .. } => "SynchronizeCache(<redacted>)",
            Self::Synchronized { .. } => "Synchronized(<redacted>)",
            Self::Failed { .. } => "Failed(<redacted>)",
        })
    }
}

impl BlockControlMessage {
    /// Returns the lifecycle session without formatting it.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Inspect { session, .. }
            | Self::Inspected { session, .. }
            | Self::SynchronizeCache { session, .. }
            | Self::Synchronized { session, .. }
            | Self::Failed { session, .. } => *session,
        }
    }

    /// Returns the request sequence without formatting it.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Inspect { sequence, .. }
            | Self::Inspected { sequence, .. }
            | Self::SynchronizeCache { sequence, .. }
            | Self::Synchronized { sequence, .. }
            | Self::Failed { sequence, .. } => *sequence,
        }
    }

    /// Returns the exact immutable grant tuple.
    #[must_use]
    pub const fn target(&self) -> &BlockControlTarget {
        match self {
            Self::Inspect { target, .. }
            | Self::Inspected { target, .. }
            | Self::SynchronizeCache { target, .. }
            | Self::Synchronized { target, .. }
            | Self::Failed { target, .. } => target,
        }
    }

    /// Returns the operation represented by this message.
    #[must_use]
    pub const fn operation(&self) -> BlockControlOperation {
        match self {
            Self::Inspect { .. } | Self::Inspected { .. } => BlockControlOperation::Inspect,
            Self::SynchronizeCache { .. } | Self::Synchronized { .. } => {
                BlockControlOperation::SynchronizeCache
            }
            Self::Failed { operation, .. } => *operation,
        }
    }
}

/// Redacted block-control framing or transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockControlError {
    /// One local-socket operation failed.
    Io(io::ErrorKind),
    /// Payload or ancillary data violated the closed protocol.
    Invalid,
}

impl fmt::Display for BlockControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private block-control broker failure")
    }
}

impl std::error::Error for BlockControlError {}

impl From<GrantTransportError> for BlockControlError {
    fn from(error: GrantTransportError) -> Self {
        match error {
            GrantTransportError::Io(kind) => Self::Io(kind),
            GrantTransportError::Invalid => Self::Invalid,
        }
    }
}

/// Sends one exact block-control datagram with no ancillary rights.
pub fn send_block_control_message(
    socket: &UnixDatagram,
    message: &BlockControlMessage,
) -> Result<(), BlockControlError> {
    let frame = encode(message)?;
    send_raw(socket.as_raw_fd(), &frame, &[]).map_err(Into::into)
}

/// Receives one exact block-control datagram and rejects all ancillary rights.
pub fn receive_block_control_message(
    socket: &UnixDatagram,
) -> Result<BlockControlMessage, BlockControlError> {
    let mut frame = [0_u8; FRAME_BYTES];
    let (length, descriptors) = receive_raw(socket, &mut frame)?;
    if length != FRAME_BYTES || !descriptors.is_empty() {
        return Err(BlockControlError::Invalid);
    }
    decode(&frame)
}

fn encode(message: &BlockControlMessage) -> Result<[u8; FRAME_BYTES], BlockControlError> {
    if message.session().is_pre_session() || message.sequence() == 0 {
        return Err(BlockControlError::Invalid);
    }
    let target = message.target();
    let grant = target.grant_id().as_bytes();
    let grant_length = u8::try_from(grant.len()).map_err(|_| BlockControlError::Invalid)?;
    let mut frame = [0_u8; FRAME_BYTES];
    frame[..4].copy_from_slice(&MAGIC);
    frame[4] = VERSION;
    frame[7] = OBJECT_KIND_BLOCK_DEVICE;
    frame[SESSION_OFFSET..SEQUENCE_OFFSET].copy_from_slice(message.session().as_bytes());
    frame[SEQUENCE_OFFSET..GRANT_LENGTH_OFFSET].copy_from_slice(&message.sequence().to_be_bytes());
    frame[GRANT_LENGTH_OFFSET] = grant_length;
    frame[ACCESS_OFFSET] = access_byte(target.access());
    frame[STATUS_FLAGS_OFFSET..DEVICE_OFFSET].copy_from_slice(&target.status_flags().to_be_bytes());
    frame[DEVICE_OFFSET..INODE_OFFSET].copy_from_slice(&target.identity().device.to_be_bytes());
    frame[INODE_OFFSET..TARGET_DEVICE_OFFSET]
        .copy_from_slice(&target.identity().inode.to_be_bytes());
    let block = target.block_device();
    frame[TARGET_DEVICE_OFFSET..LOGICAL_BLOCK_SIZE_OFFSET]
        .copy_from_slice(&block.target_device().to_be_bytes());
    frame[LOGICAL_BLOCK_SIZE_OFFSET..LOGICAL_BLOCK_SIZE_OFFSET + 4]
        .copy_from_slice(&block.logical_block_size().to_be_bytes());
    frame[BLOCK_COUNT_OFFSET..CAPACITY_OFFSET].copy_from_slice(&block.block_count().to_be_bytes());
    frame[CAPACITY_OFFSET..OBSERVED_BLOCK_SIZE_OFFSET]
        .copy_from_slice(&block.capacity().to_be_bytes());
    frame
        .get_mut(GRANT_OFFSET..GRANT_OFFSET + grant.len())
        .ok_or(BlockControlError::Invalid)?
        .copy_from_slice(grant);

    match message {
        BlockControlMessage::Inspect { .. } => frame[5] = KIND_INSPECT,
        BlockControlMessage::Inspected { observed, .. } => {
            if observed.target_device() != block.target_device() {
                return Err(BlockControlError::Invalid);
            }
            frame[5] = KIND_INSPECTED;
            frame[OBSERVED_BLOCK_SIZE_OFFSET..OBSERVED_BLOCK_SIZE_OFFSET + 4]
                .copy_from_slice(&observed.logical_block_size().to_be_bytes());
            frame[OBSERVED_BLOCK_COUNT_OFFSET..OBSERVED_CAPACITY_OFFSET]
                .copy_from_slice(&observed.block_count().to_be_bytes());
            frame[OBSERVED_CAPACITY_OFFSET..GRANT_OFFSET]
                .copy_from_slice(&observed.capacity().to_be_bytes());
        }
        BlockControlMessage::SynchronizeCache { .. } => frame[5] = KIND_SYNCHRONIZE_CACHE,
        BlockControlMessage::Synchronized { .. } => frame[5] = KIND_SYNCHRONIZED,
        BlockControlMessage::Failed {
            operation, kind, ..
        } => {
            frame[5] = KIND_FAILED;
            frame[6] = failure_status(*kind);
            frame[OPERATION_OFFSET] = operation_byte(*operation);
        }
    }
    Ok(frame)
}

fn decode(frame: &[u8; FRAME_BYTES]) -> Result<BlockControlMessage, BlockControlError> {
    if frame[..4] != MAGIC
        || frame[4] != VERSION
        || frame[7] != OBJECT_KIND_BLOCK_DEVICE
        || frame[51] != 0
        || frame[84..BLOCK_COUNT_OFFSET].iter().any(|byte| *byte != 0)
        || frame[108..OBSERVED_BLOCK_COUNT_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        || frame[GRANT_END..].iter().any(|byte| *byte != 0)
    {
        return Err(BlockControlError::Invalid);
    }
    let session = SessionId::from_bytes(
        frame[SESSION_OFFSET..SEQUENCE_OFFSET]
            .try_into()
            .map_err(|_| BlockControlError::Invalid)?,
    );
    let sequence = u64::from_be_bytes(
        frame[SEQUENCE_OFFSET..GRANT_LENGTH_OFFSET]
            .try_into()
            .map_err(|_| BlockControlError::Invalid)?,
    );
    let grant_length = usize::from(frame[GRANT_LENGTH_OFFSET]);
    if session.is_pre_session()
        || sequence == 0
        || grant_length == 0
        || grant_length > MAX_GRANT_ID_BYTES
        || frame
            .get(GRANT_OFFSET + grant_length..GRANT_END)
            .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        return Err(BlockControlError::Invalid);
    }
    let grant = frame
        .get(GRANT_OFFSET..GRANT_OFFSET + grant_length)
        .ok_or(BlockControlError::Invalid)?;
    let grant_id = std::str::from_utf8(grant)
        .map_err(|_| BlockControlError::Invalid)
        .and_then(|value| GrantId::parse(value).map_err(|_| BlockControlError::Invalid))?;
    let access = decode_access(frame[ACCESS_OFFSET])?;
    let identity = ObjectIdentity {
        device: read_u64(frame, DEVICE_OFFSET)?,
        inode: read_u64(frame, INODE_OFFSET)?,
    };
    let target_device = read_u64(frame, TARGET_DEVICE_OFFSET)?;
    let block_device = checked_block(
        target_device,
        read_u32(frame, LOGICAL_BLOCK_SIZE_OFFSET)?,
        read_u64(frame, BLOCK_COUNT_OFFSET)?,
        read_u64(frame, CAPACITY_OFFSET)?,
    )?;
    let target = BlockControlTarget::new(
        grant_id,
        access,
        identity,
        read_u32(frame, STATUS_FLAGS_OFFSET)?,
        block_device,
    )
    .ok_or(BlockControlError::Invalid)?;
    let observed_values = (
        read_u32(frame, OBSERVED_BLOCK_SIZE_OFFSET)?,
        read_u64(frame, OBSERVED_BLOCK_COUNT_OFFSET)?,
        read_u64(frame, OBSERVED_CAPACITY_OFFSET)?,
    );
    let observed = if observed_values == (0, 0, 0) {
        None
    } else {
        Some(checked_block(
            target_device,
            observed_values.0,
            observed_values.1,
            observed_values.2,
        )?)
    };

    match (frame[5], frame[6], frame[OPERATION_OFFSET], observed) {
        (KIND_INSPECT, STATUS_OK, OPERATION_NONE, None) => Ok(BlockControlMessage::Inspect {
            session,
            sequence,
            target,
        }),
        (KIND_INSPECTED, STATUS_OK, OPERATION_NONE, Some(observed)) => {
            Ok(BlockControlMessage::Inspected {
                session,
                sequence,
                target,
                observed,
            })
        }
        (KIND_SYNCHRONIZE_CACHE, STATUS_OK, OPERATION_NONE, None) => {
            Ok(BlockControlMessage::SynchronizeCache {
                session,
                sequence,
                target,
            })
        }
        (KIND_SYNCHRONIZED, STATUS_OK, OPERATION_NONE, None) => {
            Ok(BlockControlMessage::Synchronized {
                session,
                sequence,
                target,
            })
        }
        (KIND_FAILED, status, operation, None) if status != STATUS_OK => {
            Ok(BlockControlMessage::Failed {
                session,
                sequence,
                target,
                operation: decode_operation(operation)?,
                kind: failure_kind(status)?,
            })
        }
        _ => Err(BlockControlError::Invalid),
    }
}

fn checked_block(
    target_device: u64,
    logical_block_size: u32,
    block_count: u64,
    capacity: u64,
) -> Result<BlockDeviceGrant, BlockControlError> {
    BlockDeviceGrant::new(target_device, logical_block_size, block_count)
        .filter(|block| block.capacity() == capacity)
        .ok_or(BlockControlError::Invalid)
}

const fn access_byte(access: GrantAccess) -> u8 {
    match access {
        GrantAccess::ReadOnly => 1,
        GrantAccess::ReadWrite => 3,
        GrantAccess::WriteOnly | GrantAccess::CreateChildren | GrantAccess::ConnectChildren => 0,
    }
}

const fn decode_access(value: u8) -> Result<GrantAccess, BlockControlError> {
    match value {
        1 => Ok(GrantAccess::ReadOnly),
        3 => Ok(GrantAccess::ReadWrite),
        _ => Err(BlockControlError::Invalid),
    }
}

const fn operation_byte(operation: BlockControlOperation) -> u8 {
    match operation {
        BlockControlOperation::Inspect => OPERATION_INSPECT,
        BlockControlOperation::SynchronizeCache => OPERATION_SYNCHRONIZE_CACHE,
    }
}

const fn decode_operation(value: u8) -> Result<BlockControlOperation, BlockControlError> {
    match value {
        OPERATION_INSPECT => Ok(BlockControlOperation::Inspect),
        OPERATION_SYNCHRONIZE_CACHE => Ok(BlockControlOperation::SynchronizeCache),
        _ => Err(BlockControlError::Invalid),
    }
}

const fn failure_status(kind: io::ErrorKind) -> u8 {
    match kind {
        io::ErrorKind::PermissionDenied => STATUS_PERMISSION_DENIED,
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => STATUS_TIMED_OUT,
        io::ErrorKind::InvalidData => STATUS_INVALID_DATA,
        io::ErrorKind::NotFound => STATUS_NOT_FOUND,
        _ => STATUS_OTHER,
    }
}

const fn failure_kind(status: u8) -> Result<io::ErrorKind, BlockControlError> {
    match status {
        STATUS_PERMISSION_DENIED => Ok(io::ErrorKind::PermissionDenied),
        STATUS_TIMED_OUT => Ok(io::ErrorKind::TimedOut),
        STATUS_INVALID_DATA => Ok(io::ErrorKind::InvalidData),
        STATUS_NOT_FOUND => Ok(io::ErrorKind::NotFound),
        STATUS_OTHER => Ok(io::ErrorKind::Other),
        _ => Err(BlockControlError::Invalid),
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, BlockControlError> {
    let value: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(BlockControlError::Invalid)?
        .try_into()
        .map_err(|_| BlockControlError::Invalid)?;
    Ok(u32::from_be_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, BlockControlError> {
    let value: [u8; 8] = bytes
        .get(offset..offset.saturating_add(8))
        .ok_or(BlockControlError::Invalid)?
        .try_into()
        .map_err(|_| BlockControlError::Invalid)?;
    Ok(u64::from_be_bytes(value))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    fn session() -> SessionId {
        SessionId::from_bytes([7; 32])
    }

    fn target() -> BlockControlTarget {
        BlockControlTarget::new(
            GrantId::parse("block-drive").expect("grant should parse"),
            GrantAccess::ReadWrite,
            ObjectIdentity {
                device: 11,
                inode: 12,
            },
            u32::try_from(libc::O_RDWR | libc::O_NONBLOCK).expect("flags should fit"),
            BlockDeviceGrant::new(14, 512, 8).expect("block tuple should validate"),
        )
        .expect("target should validate")
    }

    fn inspect() -> BlockControlMessage {
        BlockControlMessage::Inspect {
            session: session(),
            sequence: 1,
            target: target(),
        }
    }

    #[test]
    fn every_message_round_trips_without_rights() {
        let observed = BlockDeviceGrant::new(14, 4096, 2).expect("observation should validate");
        let messages = [
            inspect(),
            BlockControlMessage::Inspected {
                session: session(),
                sequence: 1,
                target: target(),
                observed,
            },
            BlockControlMessage::SynchronizeCache {
                session: session(),
                sequence: 2,
                target: target(),
            },
            BlockControlMessage::Synchronized {
                session: session(),
                sequence: 2,
                target: target(),
            },
            BlockControlMessage::Failed {
                session: session(),
                sequence: 3,
                target: target(),
                operation: BlockControlOperation::Inspect,
                kind: io::ErrorKind::PermissionDenied,
            },
        ];
        let (left, right) = UnixDatagram::pair().expect("pair should open");
        for expected in messages {
            send_block_control_message(&left, &expected).expect("message should send");
            assert_eq!(
                receive_block_control_message(&right).expect("message should receive"),
                expected
            );
        }
    }

    #[test]
    fn rejects_closed_frame_corruption_and_rights() {
        let frame = encode(&inspect()).expect("inspect should encode");
        for index in [0, 4, 5, 6, 7, 48, 49, 51, 84, 108, GRANT_END] {
            let mut corrupted = frame;
            corrupted[index] ^= 0xff;
            assert_eq!(decode(&corrupted), Err(BlockControlError::Invalid));
        }
        let mut zero_session = frame;
        zero_session[SESSION_OFFSET..SEQUENCE_OFFSET].fill(0);
        assert_eq!(decode(&zero_session), Err(BlockControlError::Invalid));
        let mut zero_sequence = frame;
        zero_sequence[SEQUENCE_OFFSET..GRANT_LENGTH_OFFSET].fill(0);
        assert_eq!(decode(&zero_sequence), Err(BlockControlError::Invalid));

        let (left, right) = UnixDatagram::pair().expect("pair should open");
        left.send(&frame[..FRAME_BYTES - 1])
            .expect("short frame should send");
        assert_eq!(
            receive_block_control_message(&right),
            Err(BlockControlError::Invalid)
        );

        let descriptor = File::open("/dev/null").expect("fixture should open");
        send_raw(left.as_raw_fd(), &frame, &[descriptor.as_raw_fd()])
            .expect("rights frame should send");
        assert_eq!(
            receive_block_control_message(&right),
            Err(BlockControlError::Invalid)
        );

        let mut long = frame.to_vec();
        long.push(0);
        send_raw(left.as_raw_fd(), &long, &[]).expect("long frame should send");
        assert_eq!(
            receive_block_control_message(&right),
            Err(BlockControlError::Invalid)
        );
    }

    #[test]
    fn rejects_invalid_correlation_geometry_and_redacts_values() {
        let mut zero_sequence = inspect();
        if let BlockControlMessage::Inspect { sequence, .. } = &mut zero_sequence {
            *sequence = 0;
        }
        assert_eq!(encode(&zero_sequence), Err(BlockControlError::Invalid));

        let wrong_target = BlockDeviceGrant::new(99, 512, 8).expect("tuple should validate");
        let wrong_observation = BlockControlMessage::Inspected {
            session: session(),
            sequence: 1,
            target: target(),
            observed: wrong_target,
        };
        assert_eq!(encode(&wrong_observation), Err(BlockControlError::Invalid));

        assert!(
            BlockControlTarget::new(
                GrantId::parse("block-drive").expect("grant should parse"),
                GrantAccess::ReadOnly,
                ObjectIdentity {
                    device: 11,
                    inode: 12,
                },
                u32::try_from(libc::O_RDWR | libc::O_NONBLOCK).expect("flags should fit"),
                BlockDeviceGrant::new(14, 512, 8).expect("block tuple should validate"),
            )
            .is_none()
        );
        assert!(
            BlockControlTarget::new(
                GrantId::parse("block-drive").expect("grant should parse"),
                GrantAccess::ReadWrite,
                ObjectIdentity {
                    device: 11,
                    inode: 12,
                },
                u32::try_from(libc::O_RDWR | libc::O_NONBLOCK | libc::O_NOFOLLOW)
                    .expect("flags should fit"),
                BlockDeviceGrant::new(14, 512, 8).expect("block tuple should validate"),
            )
            .is_none()
        );

        let message = inspect();
        assert_eq!(format!("{message:?}"), "Inspect(<redacted>)");
        assert_eq!(
            format!("{:?}", message.target()),
            "BlockControlTarget(<redacted>)"
        );
        assert_eq!(
            BlockControlError::Invalid.to_string(),
            "private block-control broker failure"
        );
    }

    #[test]
    fn bounded_failure_categories_round_trip() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
            io::ErrorKind::InvalidData,
            io::ErrorKind::NotFound,
            io::ErrorKind::Other,
        ] {
            let message = BlockControlMessage::Failed {
                session: session(),
                sequence: 9,
                target: target(),
                operation: BlockControlOperation::SynchronizeCache,
                kind,
            };
            assert_eq!(
                decode(&encode(&message).expect("encode should work")),
                Ok(message)
            );
        }
    }

    #[test]
    fn rejects_malformed_geometry_status_operation_and_grant_bytes() {
        let encoded = encode(&BlockControlMessage::Inspected {
            session: session(),
            sequence: u64::MAX,
            target: target(),
            observed: BlockDeviceGrant::new(14, 512, 8).expect("observed tuple should validate"),
        })
        .expect("maximum nonzero sequence should encode");
        assert_eq!(
            decode(&encoded)
                .expect("maximum sequence should decode")
                .sequence(),
            u64::MAX
        );

        for mutate in [
            |frame: &mut [u8; FRAME_BYTES]| frame[6] = STATUS_OTHER,
            |frame: &mut [u8; FRAME_BYTES]| frame[OPERATION_OFFSET] = OPERATION_INSPECT,
            |frame: &mut [u8; FRAME_BYTES]| frame[OBSERVED_CAPACITY_OFFSET + 7] ^= 1,
            |frame: &mut [u8; FRAME_BYTES]| frame[GRANT_OFFSET] = b'/',
        ] {
            let mut malformed = encoded;
            mutate(&mut malformed);
            assert_eq!(decode(&malformed), Err(BlockControlError::Invalid));
        }

        let failed = encode(&BlockControlMessage::Failed {
            session: session(),
            sequence: 4,
            target: target(),
            operation: BlockControlOperation::SynchronizeCache,
            kind: io::ErrorKind::Other,
        })
        .expect("failed response should encode");
        let mut missing_operation = failed;
        missing_operation[OPERATION_OFFSET] = OPERATION_NONE;
        assert_eq!(decode(&missing_operation), Err(BlockControlError::Invalid));
    }
}
