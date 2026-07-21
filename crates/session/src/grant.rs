use std::fmt;

use crate::{ProtocolError, SessionId};

/// Maximum encoded grant datagram, below Darwin's default local datagram limit.
pub const MAX_GRANT_DATAGRAM_BYTES: usize = 1024;
/// Fixed grant datagram header length.
pub const GRANT_HEADER_BYTES: usize = 72;
/// Maximum operator grants in one startup batch.
pub const MAX_GRANTS: u16 = 64;
/// Maximum encoded grant records, including bookmark fragments.
pub const MAX_GRANT_RECORDS: u16 = 512;
/// Maximum bytes in one grant identifier.
pub const MAX_GRANT_ID_BYTES: usize = 64;
/// Maximum bytes in one contained Unix-socket child name.
pub const MAX_SOCKET_CHILD_BYTES: usize = 64;
/// Maximum UTF-8 bytes in one contained snapshot output child name.
pub const MAX_SNAPSHOT_OUTPUT_CHILD_BYTES: usize = 255;
/// Maximum bytes in one ephemeral bookmark.
pub const MAX_BOOKMARK_BYTES: u32 = 64 * 1024;
/// Maximum bookmark bytes across one batch.
pub const MAX_BATCH_BOOKMARK_BYTES: u32 = 256 * 1024;

const GRANT_MAGIC: [u8; 4] = *b"BBG2";
const GRANT_VERSION: u16 = 2;
const GRANT_MAX_PAYLOAD_BYTES: usize = MAX_GRANT_DATAGRAM_BYTES - GRANT_HEADER_BYTES;

/// Random identity bound to one startup grant batch.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BatchId([u8; 16]);

impl BatchId {
    /// Generates a nonzero cryptographically random batch identity.
    pub fn generate() -> Result<Self, ProtocolError> {
        loop {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| ProtocolError::Randomness)?;
            let batch = Self(bytes);
            if !batch.is_zero() {
                return Ok(batch);
            }
        }
    }

    /// Constructs an identity from exact protocol bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the exact protocol bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Returns whether this is the reserved all-zero identity.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Debug for BatchId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BatchId(<redacted>)")
    }
}

/// Bounded opaque identifier used to adopt one typed grant.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GrantId(String);

impl GrantId {
    /// Validates one identifier from the closed protocol character set.
    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        if value.is_empty()
            || value.len() > MAX_GRANT_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ProtocolError::InvalidFrame);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns protocol bytes. These bytes must not be logged.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for GrantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrantId(<redacted>)")
    }
}

/// Bounded single-component child name for a contained Unix socket.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SocketChild(String);

impl SocketChild {
    /// Validates one child from the closed capability-reference character set.
    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        if value.is_empty()
            || value.len() > MAX_SOCKET_CHILD_BYTES
            || matches!(value, "." | "..")
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ProtocolError::InvalidFrame);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact child bytes. These bytes must not be logged.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for SocketChild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SocketChild(<redacted>)")
    }
}

/// Bounded single-component child name for a contained snapshot artifact.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SnapshotOutputChild(String);

impl SnapshotOutputChild {
    /// Validates one UTF-8 child component without traversal or separators.
    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        if value.is_empty()
            || value.len() > MAX_SNAPSHOT_OUTPUT_CHILD_BYTES
            || matches!(value, "." | "..")
            || value.bytes().any(|byte| matches!(byte, 0 | b'/'))
        {
            return Err(ProtocolError::InvalidFrame);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact child bytes. These bytes must not be logged.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for SnapshotOutputChild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SnapshotOutputChild(<redacted>)")
    }
}

/// Closed semantic host-resource role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ResourceRole {
    /// Startup JSON configuration.
    StartupConfig = 1,
    /// Startup MMDS metadata.
    StartupMetadata = 2,
    /// Guest kernel image.
    KernelImage = 3,
    /// Guest initrd image.
    InitrdImage = 4,
    /// Drive backing file, keyed by grant ID.
    DriveBacking = 5,
    /// Persistent-memory backing file, keyed by grant ID.
    PmemBacking = 6,
    /// Parent directory for the API socket.
    ApiSocketDirectory = 7,
    /// Parent directory for the vsock Unix socket.
    VsockSocketDirectory = 8,
    /// Logger output file.
    LoggerSink = 9,
    /// Metrics output file.
    MetricsSink = 10,
    /// Serial output file.
    SerialSink = 11,
    /// Snapshot state inspected by the early describe command.
    SnapshotDescribeInput = 12,
    /// Snapshot state input.
    SnapshotStateInput = 13,
    /// Snapshot memory input.
    SnapshotMemoryInput = 14,
    /// Directory receiving snapshot artifacts.
    SnapshotOutputDirectory = 15,
    /// Parent directory containing connect-only vhost-user sockets.
    VhostUserSocketDirectory = 16,
}

impl ResourceRole {
    /// Returns whether this role may occur more than once in one batch.
    #[must_use]
    pub const fn is_repeatable(self) -> bool {
        matches!(
            self,
            Self::DriveBacking
                | Self::PmemBacking
                | Self::SnapshotOutputDirectory
                | Self::VhostUserSocketDirectory
        )
    }

    /// Returns whether this role carries an ephemeral directory scope.
    #[must_use]
    pub const fn is_scoped_directory(self) -> bool {
        matches!(
            self,
            Self::ApiSocketDirectory
                | Self::VsockSocketDirectory
                | Self::SnapshotOutputDirectory
                | Self::VhostUserSocketDirectory
        )
    }

    /// Checks the only access modes accepted for this role.
    #[must_use]
    pub const fn permits(self, access: GrantAccess) -> bool {
        match self {
            Self::StartupConfig
            | Self::StartupMetadata
            | Self::KernelImage
            | Self::InitrdImage
            | Self::SnapshotDescribeInput
            | Self::SnapshotStateInput
            | Self::SnapshotMemoryInput => matches!(access, GrantAccess::ReadOnly),
            Self::DriveBacking | Self::PmemBacking => {
                matches!(access, GrantAccess::ReadOnly | GrantAccess::ReadWrite)
            }
            Self::LoggerSink | Self::MetricsSink | Self::SerialSink => {
                matches!(access, GrantAccess::WriteOnly)
            }
            Self::ApiSocketDirectory
            | Self::VsockSocketDirectory
            | Self::SnapshotOutputDirectory => matches!(access, GrantAccess::CreateChildren),
            Self::VhostUserSocketDirectory => matches!(access, GrantAccess::ConnectChildren),
        }
    }

    fn from_byte(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::StartupConfig),
            2 => Ok(Self::StartupMetadata),
            3 => Ok(Self::KernelImage),
            4 => Ok(Self::InitrdImage),
            5 => Ok(Self::DriveBacking),
            6 => Ok(Self::PmemBacking),
            7 => Ok(Self::ApiSocketDirectory),
            8 => Ok(Self::VsockSocketDirectory),
            9 => Ok(Self::LoggerSink),
            10 => Ok(Self::MetricsSink),
            11 => Ok(Self::SerialSink),
            12 => Ok(Self::SnapshotDescribeInput),
            13 => Ok(Self::SnapshotStateInput),
            14 => Ok(Self::SnapshotMemoryInput),
            15 => Ok(Self::SnapshotOutputDirectory),
            16 => Ok(Self::VhostUserSocketDirectory),
            _ => Err(ProtocolError::InvalidFrame),
        }
    }
}

/// Exact authority carried by a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GrantAccess {
    /// Existing regular file opened read-only.
    ReadOnly = 1,
    /// Existing regular file opened write-only.
    WriteOnly = 2,
    /// Existing regular file opened read-write.
    ReadWrite = 3,
    /// Existing directory with process-lifetime child-creation scope.
    CreateChildren = 4,
    /// Existing directory whose exact children may only be connected.
    ConnectChildren = 5,
}

impl GrantAccess {
    fn from_byte(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::ReadOnly),
            2 => Ok(Self::WriteOnly),
            3 => Ok(Self::ReadWrite),
            4 => Ok(Self::CreateChildren),
            5 => Ok(Self::ConnectChildren),
            _ => Err(ProtocolError::InvalidFrame),
        }
    }
}

/// Expected kernel object type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GrantObjectKind {
    /// Existing regular file.
    RegularFile = 1,
    /// Existing directory.
    Directory = 2,
    /// Existing macOS block-special device.
    BlockDevice = 3,
}

impl GrantObjectKind {
    fn from_byte(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::RegularFile),
            2 => Ok(Self::Directory),
            3 => Ok(Self::BlockDevice),
            _ => Err(ProtocolError::InvalidFrame),
        }
    }
}

/// Stable identity independently checked on each side.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectIdentity {
    /// Device containing the object.
    pub device: u64,
    /// Inode of the opened object.
    pub inode: u64,
}

impl fmt::Debug for ObjectIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ObjectIdentity(<redacted>)")
    }
}

/// Checked block-special descriptor metadata authenticated by one grant.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockDeviceGrant {
    target_device: u64,
    logical_block_size: u32,
    block_count: u64,
    capacity: u64,
}

impl BlockDeviceGrant {
    /// Builds one internally consistent, virtio-sector-aligned block tuple.
    pub fn new(target_device: u64, logical_block_size: u32, block_count: u64) -> Option<Self> {
        let block_size = u64::from(logical_block_size);
        if target_device == 0
            || block_size == 0
            || block_count == 0
            || !block_size.is_multiple_of(512)
        {
            return None;
        }
        let capacity = block_size.checked_mul(block_count)?;
        if capacity == 0 || !capacity.is_multiple_of(512) || i64::try_from(capacity).is_err() {
            return None;
        }
        Some(Self {
            target_device,
            logical_block_size,
            block_count,
            capacity,
        })
    }

    fn from_wire(
        target_device: u64,
        logical_block_size: u32,
        block_count: u64,
        capacity: u64,
    ) -> Option<Self> {
        let value = Self::new(target_device, logical_block_size, block_count)?;
        (value.capacity == capacity).then_some(value)
    }

    /// Returns the normalized target-device identity (`st_rdev`).
    #[must_use]
    pub const fn target_device(self) -> u64 {
        self.target_device
    }

    /// Returns the logical media block size.
    #[must_use]
    pub const fn logical_block_size(self) -> u32 {
        self.logical_block_size
    }

    /// Returns the logical media block count.
    #[must_use]
    pub const fn block_count(self) -> u64 {
        self.block_count
    }

    /// Returns the checked byte capacity.
    #[must_use]
    pub const fn capacity(self) -> u64 {
        self.capacity
    }
}

impl fmt::Debug for BlockDeviceGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BlockDeviceGrant(<redacted>)")
    }
}

/// One closed grant-channel record.
#[derive(Clone, PartialEq, Eq)]
pub enum GrantRecord {
    /// Declares exact batch bounds.
    Begin {
        /// Number of semantic grants.
        grant_count: u16,
        /// Number of records including Begin and Commit.
        record_count: u16,
        /// Total bookmark bytes.
        bookmark_bytes: u32,
    },
    /// Transfers one existing regular file.
    Descriptor {
        /// Opaque consumer identifier.
        id: GrantId,
        /// Semantic resource role.
        role: ResourceRole,
        /// Kernel access mode.
        access: GrantAccess,
        /// Expected opened object type.
        kind: GrantObjectKind,
        /// Expected stable identity.
        identity: ObjectIdentity,
        /// Expected file status flags after masking mutable noise.
        status_flags: u32,
        /// Authenticated block-special metadata, absent for regular files.
        block_device: Option<BlockDeviceGrant>,
    },
    /// Transfers a directory anchor and declares its bookmark fragments.
    ScopedDirectory {
        /// Opaque consumer identifier.
        id: GrantId,
        /// Semantic resource role.
        role: ResourceRole,
        /// Logical directory authority.
        access: GrantAccess,
        /// Expected stable identity.
        identity: ObjectIdentity,
        /// Exact bookmark length.
        bookmark_bytes: u32,
        /// Exact fragment count.
        fragment_count: u16,
    },
    /// Carries one exact bookmark byte range.
    BookmarkFragment {
        /// Owning grant identifier.
        id: GrantId,
        /// Byte offset in the complete bookmark.
        offset: u32,
        /// Bounded opaque bytes.
        bytes: Vec<u8>,
    },
    /// Commits the exact declared batch.
    Commit {
        /// Number of semantic grants.
        grant_count: u16,
        /// Number of records including Begin and Commit.
        record_count: u16,
        /// Total bookmark bytes.
        bookmark_bytes: u32,
    },
}

impl fmt::Debug for GrantRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Begin { .. } => "Begin",
            Self::Descriptor { .. } => "Descriptor",
            Self::ScopedDirectory { .. } => "ScopedDirectory",
            Self::BookmarkFragment { .. } => "BookmarkFragment",
            Self::Commit { .. } => "Commit",
        };
        write!(formatter, "GrantRecord::{name}(<redacted>)")
    }
}

impl GrantRecord {
    /// Returns the exact SCM_RIGHTS count required for this record.
    #[must_use]
    pub const fn descriptor_count(&self) -> u8 {
        match self {
            Self::Descriptor { .. } | Self::ScopedDirectory { .. } => 1,
            Self::Begin { .. } | Self::BookmarkFragment { .. } | Self::Commit { .. } => 0,
        }
    }
}

/// One decoded grant-channel frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantFrame {
    /// Lifecycle session identity.
    pub session: SessionId,
    /// Startup batch identity.
    pub batch: BatchId,
    /// Exact launcher-to-worker sequence.
    pub sequence: u64,
    /// Declared descriptor count.
    pub descriptor_count: u8,
    /// Closed record.
    pub record: GrantRecord,
}

/// Encodes one complete bounded datagram.
pub fn encode_grant_frame(frame: &GrantFrame) -> Result<Vec<u8>, ProtocolError> {
    if frame.session.is_pre_session() || frame.batch.is_zero() {
        return Err(ProtocolError::InvalidFrame);
    }
    if frame.descriptor_count != frame.record.descriptor_count() {
        return Err(ProtocolError::InvalidFrame);
    }
    let (kind, payload) = encode_record(&frame.record)?;
    if payload.len() > GRANT_MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::InvalidFrame);
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| ProtocolError::InvalidFrame)?;
    let mut encoded = Vec::with_capacity(GRANT_HEADER_BYTES + payload.len());
    encoded.extend_from_slice(&GRANT_MAGIC);
    encoded.extend_from_slice(&GRANT_VERSION.to_be_bytes());
    encoded.extend_from_slice(&kind.to_be_bytes());
    encoded.extend_from_slice(&payload_len.to_be_bytes());
    encoded.push(frame.descriptor_count);
    encoded.extend_from_slice(&[0; 3]);
    encoded.extend_from_slice(frame.session.as_bytes());
    encoded.extend_from_slice(frame.batch.as_bytes());
    encoded.extend_from_slice(&frame.sequence.to_be_bytes());
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

/// Decodes one exact datagram after transport-level ancillary validation.
pub fn decode_grant_frame(bytes: &[u8]) -> Result<GrantFrame, ProtocolError> {
    if bytes.len() < GRANT_HEADER_BYTES
        || bytes.len() > MAX_GRANT_DATAGRAM_BYTES
        || bytes.get(..4) != Some(GRANT_MAGIC.as_slice())
        || read_u16(bytes, 4)? != GRANT_VERSION
        || bytes.get(13..16) != Some([0; 3].as_slice())
    {
        return Err(ProtocolError::InvalidFrame);
    }
    let payload_len =
        usize::try_from(read_u32(bytes, 8)?).map_err(|_| ProtocolError::InvalidFrame)?;
    if GRANT_HEADER_BYTES.saturating_add(payload_len) != bytes.len() {
        return Err(ProtocolError::InvalidFrame);
    }
    let session_bytes: [u8; 32] = bytes
        .get(16..48)
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    let batch_bytes: [u8; 16] = bytes
        .get(48..64)
        .ok_or(ProtocolError::InvalidFrame)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidFrame)?;
    let session = SessionId::from_bytes(session_bytes);
    let batch = BatchId::from_bytes(batch_bytes);
    if session.is_pre_session() || batch.is_zero() {
        return Err(ProtocolError::InvalidFrame);
    }
    let descriptor_count = *bytes.get(12).ok_or(ProtocolError::InvalidFrame)?;
    let record = decode_record(
        read_u16(bytes, 6)?,
        bytes
            .get(GRANT_HEADER_BYTES..)
            .ok_or(ProtocolError::InvalidFrame)?,
    )?;
    if descriptor_count != record.descriptor_count() {
        return Err(ProtocolError::InvalidFrame);
    }
    Ok(GrantFrame {
        session,
        batch,
        sequence: read_u64(bytes, 64)?,
        descriptor_count,
        record,
    })
}

fn encode_record(record: &GrantRecord) -> Result<(u16, Vec<u8>), ProtocolError> {
    match record {
        GrantRecord::Begin {
            grant_count,
            record_count,
            bookmark_bytes,
        } => {
            validate_batch_counts(*grant_count, *record_count, *bookmark_bytes)?;
            Ok((
                1,
                encode_counts(*grant_count, *record_count, *bookmark_bytes),
            ))
        }
        GrantRecord::Descriptor {
            id,
            role,
            access,
            kind,
            identity,
            status_flags,
            block_device,
        } => {
            if role.is_scoped_directory()
                || !role.permits(*access)
                || !valid_descriptor_kind(*role, *access, *kind, *block_device)
            {
                return Err(ProtocolError::InvalidFrame);
            }
            let mut payload = encode_grant_prefix(id, *role, *access, *kind, *identity)?;
            payload.extend_from_slice(&status_flags.to_be_bytes());
            let (target_device, logical_block_size, block_count, capacity) =
                block_device.map_or((0, 0, 0, 0), |block| {
                    (
                        block.target_device(),
                        block.logical_block_size(),
                        block.block_count(),
                        block.capacity(),
                    )
                });
            payload.extend_from_slice(&target_device.to_be_bytes());
            payload.extend_from_slice(&logical_block_size.to_be_bytes());
            payload.extend_from_slice(&block_count.to_be_bytes());
            payload.extend_from_slice(&capacity.to_be_bytes());
            Ok((2, payload))
        }
        GrantRecord::ScopedDirectory {
            id,
            role,
            access,
            identity,
            bookmark_bytes,
            fragment_count,
        } => {
            if !role.is_scoped_directory()
                || !role.permits(*access)
                || *bookmark_bytes == 0
                || *bookmark_bytes > MAX_BOOKMARK_BYTES
                || *fragment_count == 0
                || *fragment_count > MAX_GRANT_RECORDS
            {
                return Err(ProtocolError::InvalidFrame);
            }
            let mut payload =
                encode_grant_prefix(id, *role, *access, GrantObjectKind::Directory, *identity)?;
            payload.extend_from_slice(&bookmark_bytes.to_be_bytes());
            payload.extend_from_slice(&fragment_count.to_be_bytes());
            Ok((3, payload))
        }
        GrantRecord::BookmarkFragment { id, offset, bytes } => {
            if bytes.is_empty() {
                return Err(ProtocolError::InvalidFrame);
            }
            let id_len =
                u8::try_from(id.as_bytes().len()).map_err(|_| ProtocolError::InvalidFrame)?;
            let mut payload = Vec::with_capacity(1 + id.as_bytes().len() + 4 + bytes.len());
            payload.push(id_len);
            payload.extend_from_slice(id.as_bytes());
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(bytes);
            if payload.len() > GRANT_MAX_PAYLOAD_BYTES {
                return Err(ProtocolError::InvalidFrame);
            }
            Ok((4, payload))
        }
        GrantRecord::Commit {
            grant_count,
            record_count,
            bookmark_bytes,
        } => {
            validate_batch_counts(*grant_count, *record_count, *bookmark_bytes)?;
            Ok((
                5,
                encode_counts(*grant_count, *record_count, *bookmark_bytes),
            ))
        }
    }
}

fn decode_record(kind: u16, payload: &[u8]) -> Result<GrantRecord, ProtocolError> {
    match kind {
        1 | 5 => {
            if payload.len() != 8 {
                return Err(ProtocolError::InvalidFrame);
            }
            let grant_count = read_u16(payload, 0)?;
            let record_count = read_u16(payload, 2)?;
            let bookmark_bytes = read_u32(payload, 4)?;
            validate_batch_counts(grant_count, record_count, bookmark_bytes)?;
            if kind == 1 {
                Ok(GrantRecord::Begin {
                    grant_count,
                    record_count,
                    bookmark_bytes,
                })
            } else {
                Ok(GrantRecord::Commit {
                    grant_count,
                    record_count,
                    bookmark_bytes,
                })
            }
        }
        2 => {
            let (id, role, access, object_kind, identity, offset) = decode_grant_prefix(payload)?;
            if payload.len() != offset.saturating_add(32)
                || role.is_scoped_directory()
                || !role.permits(access)
            {
                return Err(ProtocolError::InvalidFrame);
            }
            let target_device = read_u64(payload, offset.saturating_add(4))?;
            let logical_block_size = read_u32(payload, offset.saturating_add(12))?;
            let block_count = read_u64(payload, offset.saturating_add(16))?;
            let capacity = read_u64(payload, offset.saturating_add(24))?;
            let block_device = if target_device == 0
                && logical_block_size == 0
                && block_count == 0
                && capacity == 0
            {
                None
            } else {
                Some(
                    BlockDeviceGrant::from_wire(
                        target_device,
                        logical_block_size,
                        block_count,
                        capacity,
                    )
                    .ok_or(ProtocolError::InvalidFrame)?,
                )
            };
            if !valid_descriptor_kind(role, access, object_kind, block_device) {
                return Err(ProtocolError::InvalidFrame);
            }
            Ok(GrantRecord::Descriptor {
                id,
                role,
                access,
                kind: object_kind,
                identity,
                status_flags: read_u32(payload, offset)?,
                block_device,
            })
        }
        3 => {
            let (id, role, access, object_kind, identity, offset) = decode_grant_prefix(payload)?;
            if payload.len() != offset.saturating_add(6)
                || !role.is_scoped_directory()
                || !role.permits(access)
                || object_kind != GrantObjectKind::Directory
            {
                return Err(ProtocolError::InvalidFrame);
            }
            let bookmark_bytes = read_u32(payload, offset)?;
            let fragment_count = read_u16(payload, offset.saturating_add(4))?;
            if bookmark_bytes == 0
                || bookmark_bytes > MAX_BOOKMARK_BYTES
                || fragment_count == 0
                || fragment_count > MAX_GRANT_RECORDS
            {
                return Err(ProtocolError::InvalidFrame);
            }
            Ok(GrantRecord::ScopedDirectory {
                id,
                role,
                access,
                identity,
                bookmark_bytes,
                fragment_count,
            })
        }
        4 => {
            let id_len = usize::from(*payload.first().ok_or(ProtocolError::InvalidFrame)?);
            let offset_index = 1_usize
                .checked_add(id_len)
                .ok_or(ProtocolError::InvalidFrame)?;
            let bytes_index = offset_index
                .checked_add(4)
                .ok_or(ProtocolError::InvalidFrame)?;
            let id = decode_id(
                payload
                    .get(1..offset_index)
                    .ok_or(ProtocolError::InvalidFrame)?,
            )?;
            let fragment = payload
                .get(bytes_index..)
                .ok_or(ProtocolError::InvalidFrame)?;
            if fragment.is_empty() {
                return Err(ProtocolError::InvalidFrame);
            }
            Ok(GrantRecord::BookmarkFragment {
                id,
                offset: read_u32(payload, offset_index)?,
                bytes: fragment.to_vec(),
            })
        }
        _ => Err(ProtocolError::InvalidFrame),
    }
}

fn valid_descriptor_kind(
    role: ResourceRole,
    access: GrantAccess,
    kind: GrantObjectKind,
    block_device: Option<BlockDeviceGrant>,
) -> bool {
    match (kind, block_device) {
        (GrantObjectKind::RegularFile, None) => true,
        (GrantObjectKind::BlockDevice, Some(_)) => {
            role == ResourceRole::DriveBacking
                && matches!(access, GrantAccess::ReadOnly | GrantAccess::ReadWrite)
        }
        _ => false,
    }
}

fn encode_counts(grant_count: u16, record_count: u16, bookmark_bytes: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8);
    payload.extend_from_slice(&grant_count.to_be_bytes());
    payload.extend_from_slice(&record_count.to_be_bytes());
    payload.extend_from_slice(&bookmark_bytes.to_be_bytes());
    payload
}

fn validate_batch_counts(
    grant_count: u16,
    record_count: u16,
    bookmark_bytes: u32,
) -> Result<(), ProtocolError> {
    if grant_count > MAX_GRANTS
        || !(2..=MAX_GRANT_RECORDS).contains(&record_count)
        || bookmark_bytes > MAX_BATCH_BOOKMARK_BYTES
    {
        return Err(ProtocolError::InvalidFrame);
    }
    Ok(())
}

fn encode_grant_prefix(
    id: &GrantId,
    role: ResourceRole,
    access: GrantAccess,
    kind: GrantObjectKind,
    identity: ObjectIdentity,
) -> Result<Vec<u8>, ProtocolError> {
    let id_len = u8::try_from(id.as_bytes().len()).map_err(|_| ProtocolError::InvalidFrame)?;
    let mut payload = Vec::with_capacity(20 + id.as_bytes().len());
    payload.push(id_len);
    payload.push(role as u8);
    payload.push(access as u8);
    payload.push(kind as u8);
    payload.extend_from_slice(&identity.device.to_be_bytes());
    payload.extend_from_slice(&identity.inode.to_be_bytes());
    payload.extend_from_slice(id.as_bytes());
    Ok(payload)
}

fn decode_grant_prefix(
    payload: &[u8],
) -> Result<
    (
        GrantId,
        ResourceRole,
        GrantAccess,
        GrantObjectKind,
        ObjectIdentity,
        usize,
    ),
    ProtocolError,
> {
    if payload.len() < 20 {
        return Err(ProtocolError::InvalidFrame);
    }
    let id_len = usize::from(*payload.first().ok_or(ProtocolError::InvalidFrame)?);
    let end = 20_usize
        .checked_add(id_len)
        .ok_or(ProtocolError::InvalidFrame)?;
    let id = decode_id(payload.get(20..end).ok_or(ProtocolError::InvalidFrame)?)?;
    Ok((
        id,
        ResourceRole::from_byte(*payload.get(1).ok_or(ProtocolError::InvalidFrame)?)?,
        GrantAccess::from_byte(*payload.get(2).ok_or(ProtocolError::InvalidFrame)?)?,
        GrantObjectKind::from_byte(*payload.get(3).ok_or(ProtocolError::InvalidFrame)?)?,
        ObjectIdentity {
            device: read_u64(payload, 4)?,
            inode: read_u64(payload, 12)?,
        },
        end,
    ))
}

fn decode_id(bytes: &[u8]) -> Result<GrantId, ProtocolError> {
    let value = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidFrame)?;
    GrantId::parse(value)
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

    fn frame(record: GrantRecord) -> GrantFrame {
        GrantFrame {
            session: SessionId::from_bytes([7; 32]),
            batch: BatchId::from_bytes([9; 16]),
            sequence: 11,
            descriptor_count: record.descriptor_count(),
            record,
        }
    }

    fn id(value: &str) -> GrantId {
        GrantId::parse(value).expect("test ID should be valid")
    }

    #[test]
    fn every_record_round_trips_with_redacted_debug() {
        let records = [
            GrantRecord::Begin {
                grant_count: 2,
                record_count: 4,
                bookmark_bytes: 3,
            },
            GrantRecord::Descriptor {
                id: id("kernel"),
                role: ResourceRole::KernelImage,
                access: GrantAccess::ReadOnly,
                kind: GrantObjectKind::RegularFile,
                identity: ObjectIdentity {
                    device: 4,
                    inode: 5,
                },
                status_flags: 0,
                block_device: None,
            },
            GrantRecord::Descriptor {
                id: id("block"),
                role: ResourceRole::DriveBacking,
                access: GrantAccess::ReadWrite,
                kind: GrantObjectKind::BlockDevice,
                identity: ObjectIdentity {
                    device: 10,
                    inode: 11,
                },
                status_flags: 4,
                block_device: Some(
                    BlockDeviceGrant::new(12, 512, 8).expect("block tuple should be valid"),
                ),
            },
            GrantRecord::ScopedDirectory {
                id: id("api"),
                role: ResourceRole::ApiSocketDirectory,
                access: GrantAccess::CreateChildren,
                identity: ObjectIdentity {
                    device: 6,
                    inode: 7,
                },
                bookmark_bytes: 3,
                fragment_count: 1,
            },
            GrantRecord::ScopedDirectory {
                id: id("vhost"),
                role: ResourceRole::VhostUserSocketDirectory,
                access: GrantAccess::ConnectChildren,
                identity: ObjectIdentity {
                    device: 8,
                    inode: 9,
                },
                bookmark_bytes: 3,
                fragment_count: 1,
            },
            GrantRecord::BookmarkFragment {
                id: id("api"),
                offset: 0,
                bytes: vec![1, 2, 3],
            },
            GrantRecord::Commit {
                grant_count: 2,
                record_count: 5,
                bookmark_bytes: 3,
            },
        ];
        for record in records {
            let expected = frame(record);
            let encoded = encode_grant_frame(&expected).expect("record should encode");
            assert_eq!(
                decode_grant_frame(&encoded).expect("record should decode"),
                expected
            );
            let debug = format!("{expected:?}");
            assert!(
                !debug.contains("kernel") && !debug.contains("api") && !debug.contains("vhost")
            );
        }
    }

    #[test]
    fn vhost_user_directories_are_repeatable_scoped_and_connect_only() {
        let role = ResourceRole::VhostUserSocketDirectory;
        assert!(role.is_repeatable());
        assert!(role.is_scoped_directory());
        assert!(role.permits(GrantAccess::ConnectChildren));
        for access in [
            GrantAccess::ReadOnly,
            GrantAccess::WriteOnly,
            GrantAccess::ReadWrite,
            GrantAccess::CreateChildren,
        ] {
            assert!(!role.permits(access));
        }
    }

    #[test]
    fn rejects_invalid_ids_roles_counts_and_descriptor_declarations() {
        for value in ["", "has space", "slash/name"] {
            assert_eq!(GrantId::parse(value), Err(ProtocolError::InvalidFrame));
        }
        assert_eq!(
            GrantId::parse(&"a".repeat(MAX_GRANT_ID_BYTES + 1)),
            Err(ProtocolError::InvalidFrame)
        );

        let mut invalid = frame(GrantRecord::Descriptor {
            id: id("bad"),
            role: ResourceRole::KernelImage,
            access: GrantAccess::ReadWrite,
            kind: GrantObjectKind::RegularFile,
            identity: ObjectIdentity {
                device: 1,
                inode: 2,
            },
            status_flags: 0,
            block_device: None,
        });
        assert_eq!(
            encode_grant_frame(&invalid),
            Err(ProtocolError::InvalidFrame)
        );
        invalid.record = GrantRecord::Begin {
            grant_count: MAX_GRANTS + 1,
            record_count: 2,
            bookmark_bytes: 0,
        };
        invalid.descriptor_count = 0;
        assert_eq!(
            encode_grant_frame(&invalid),
            Err(ProtocolError::InvalidFrame)
        );
        invalid.record = GrantRecord::Commit {
            grant_count: 0,
            record_count: 2,
            bookmark_bytes: 0,
        };
        invalid.descriptor_count = 1;
        assert_eq!(
            encode_grant_frame(&invalid),
            Err(ProtocolError::InvalidFrame)
        );
    }

    #[test]
    fn block_descriptor_geometry_and_role_are_closed() {
        let valid = BlockDeviceGrant::new(3, 4096, 8).expect("geometry should validate");
        assert_eq!(valid.target_device(), 3);
        assert_eq!(valid.logical_block_size(), 4096);
        assert_eq!(valid.block_count(), 8);
        assert_eq!(valid.capacity(), 32_768);
        assert_eq!(format!("{valid:?}"), "BlockDeviceGrant(<redacted>)");
        assert!(BlockDeviceGrant::new(0, 512, 8).is_none());
        assert!(BlockDeviceGrant::new(3, 0, 8).is_none());
        assert!(BlockDeviceGrant::new(3, 768, 8).is_none());
        assert!(BlockDeviceGrant::new(3, 512, 0).is_none());
        assert!(BlockDeviceGrant::new(3, 4096, u64::MAX).is_none());

        for (role, access, kind, block_device) in [
            (
                ResourceRole::KernelImage,
                GrantAccess::ReadOnly,
                GrantObjectKind::BlockDevice,
                Some(valid),
            ),
            (
                ResourceRole::DriveBacking,
                GrantAccess::ReadOnly,
                GrantObjectKind::RegularFile,
                Some(valid),
            ),
            (
                ResourceRole::DriveBacking,
                GrantAccess::ReadOnly,
                GrantObjectKind::BlockDevice,
                None,
            ),
        ] {
            let invalid = frame(GrantRecord::Descriptor {
                id: id("invalid-block"),
                role,
                access,
                kind,
                identity: ObjectIdentity {
                    device: 1,
                    inode: 2,
                },
                status_flags: 0,
                block_device,
            });
            assert_eq!(
                encode_grant_frame(&invalid),
                Err(ProtocolError::InvalidFrame)
            );
        }
    }

    #[test]
    fn block_descriptor_wire_rejects_noncanonical_and_inconsistent_tuples() {
        let block_id = id("block-wire");
        let block = frame(GrantRecord::Descriptor {
            id: block_id.clone(),
            role: ResourceRole::DriveBacking,
            access: GrantAccess::ReadWrite,
            kind: GrantObjectKind::BlockDevice,
            identity: ObjectIdentity {
                device: 21,
                inode: 22,
            },
            status_flags: 4,
            block_device: Some(
                BlockDeviceGrant::new(23, 512, 16).expect("block tuple should validate"),
            ),
        });
        let encoded = encode_grant_frame(&block).expect("block record should encode");
        let record_offset = GRANT_HEADER_BYTES + 20 + block_id.as_bytes().len();
        let target_offset = record_offset + 4;
        let capacity_offset = record_offset + 24;

        let mut inconsistent_capacity = encoded.clone();
        inconsistent_capacity[capacity_offset + 7] ^= 1;
        assert_eq!(
            decode_grant_frame(&inconsistent_capacity),
            Err(ProtocolError::InvalidFrame)
        );

        let mut zero_geometry = encoded.clone();
        zero_geometry[target_offset..record_offset + 32].fill(0);
        assert_eq!(
            decode_grant_frame(&zero_geometry),
            Err(ProtocolError::InvalidFrame)
        );

        let regular_id = id("regular-wire");
        let regular = frame(GrantRecord::Descriptor {
            id: regular_id.clone(),
            role: ResourceRole::DriveBacking,
            access: GrantAccess::ReadOnly,
            kind: GrantObjectKind::RegularFile,
            identity: ObjectIdentity {
                device: 31,
                inode: 32,
            },
            status_flags: 0,
            block_device: None,
        });
        let regular = encode_grant_frame(&regular).expect("regular record should encode");
        let regular_record_offset = GRANT_HEADER_BYTES + 20 + regular_id.as_bytes().len();
        assert!(
            regular[regular_record_offset + 4..regular_record_offset + 32]
                .iter()
                .all(|byte| *byte == 0)
        );
        let mut noncanonical_regular = regular;
        noncanonical_regular[regular_record_offset + 11] = 1;
        assert_eq!(
            decode_grant_frame(&noncanonical_regular),
            Err(ProtocolError::InvalidFrame)
        );

        let mut truncated = encoded.clone();
        truncated.pop();
        assert_eq!(
            decode_grant_frame(&truncated),
            Err(ProtocolError::InvalidFrame)
        );
        let mut surplus = encoded.clone();
        surplus.push(0);
        assert_eq!(
            decode_grant_frame(&surplus),
            Err(ProtocolError::InvalidFrame)
        );
        for index in [0, 4, 5, 12, 13] {
            let mut corrupted = encoded.clone();
            corrupted[index] ^= 1;
            assert_eq!(
                decode_grant_frame(&corrupted),
                Err(ProtocolError::InvalidFrame)
            );
        }
    }

    #[test]
    fn socket_children_use_one_bounded_redacted_component() {
        for value in ["api.sock", "VSOCK_1", "socket-2"] {
            let child = SocketChild::parse(value).expect("safe child should parse");
            assert_eq!(child.as_bytes(), value.as_bytes());
            assert!(!format!("{child:?}").contains(value));
        }
        for value in [
            "",
            ".",
            "..",
            "with/slash",
            "with\\slash",
            "space name",
            "雪",
        ] {
            assert_eq!(SocketChild::parse(value), Err(ProtocolError::InvalidFrame));
        }
        assert_eq!(
            SocketChild::parse(&"a".repeat(MAX_SOCKET_CHILD_BYTES + 1)),
            Err(ProtocolError::InvalidFrame)
        );
        assert!(SocketChild::parse(&"a".repeat(MAX_SOCKET_CHILD_BYTES)).is_ok());
    }

    #[test]
    fn snapshot_output_children_use_one_bounded_redacted_utf8_component() {
        for value in ["state.snap", "memory image", "雪", r"back\\slash"] {
            let child =
                SnapshotOutputChild::parse(value).expect("safe snapshot child should parse");
            assert_eq!(child.as_bytes(), value.as_bytes());
            assert!(!format!("{child:?}").contains(value));
        }
        for value in ["", ".", "..", "with/slash", "nul\0byte"] {
            assert_eq!(
                SnapshotOutputChild::parse(value),
                Err(ProtocolError::InvalidFrame)
            );
        }
        assert_eq!(
            SnapshotOutputChild::parse(&"a".repeat(MAX_SNAPSHOT_OUTPUT_CHILD_BYTES + 1)),
            Err(ProtocolError::InvalidFrame)
        );
        assert!(SnapshotOutputChild::parse(&"a".repeat(MAX_SNAPSHOT_OUTPUT_CHILD_BYTES)).is_ok());
        assert_eq!(
            SnapshotOutputChild::parse(&"雪".repeat(86)),
            Err(ProtocolError::InvalidFrame)
        );
        assert!(SnapshotOutputChild::parse(&"雪".repeat(85)).is_ok());
    }

    #[test]
    fn rejects_header_corruption_and_oversized_fragments() {
        let expected = frame(GrantRecord::BookmarkFragment {
            id: id("directory"),
            offset: 0,
            bytes: vec![1; GRANT_MAX_PAYLOAD_BYTES],
        });
        assert_eq!(
            encode_grant_frame(&expected),
            Err(ProtocolError::InvalidFrame)
        );

        let valid = frame(GrantRecord::Begin {
            grant_count: 0,
            record_count: 2,
            bookmark_bytes: 0,
        });
        let mut encoded = encode_grant_frame(&valid).expect("begin should encode");
        encoded[13] = 1;
        assert_eq!(
            decode_grant_frame(&encoded),
            Err(ProtocolError::InvalidFrame)
        );
    }

    #[test]
    fn accepts_every_exact_limit_and_rejects_the_first_value_over() {
        assert!(GrantId::parse(&"a".repeat(MAX_GRANT_ID_BYTES)).is_ok());

        let exact_counts = frame(GrantRecord::Begin {
            grant_count: MAX_GRANTS,
            record_count: MAX_GRANT_RECORDS,
            bookmark_bytes: MAX_BATCH_BOOKMARK_BYTES,
        });
        assert!(encode_grant_frame(&exact_counts).is_ok());

        let excessive_records = frame(GrantRecord::Begin {
            grant_count: 0,
            record_count: MAX_GRANT_RECORDS + 1,
            bookmark_bytes: 0,
        });
        assert_eq!(
            encode_grant_frame(&excessive_records),
            Err(ProtocolError::InvalidFrame)
        );
        let excessive_bookmarks = frame(GrantRecord::Begin {
            grant_count: 0,
            record_count: 2,
            bookmark_bytes: MAX_BATCH_BOOKMARK_BYTES + 1,
        });
        assert_eq!(
            encode_grant_frame(&excessive_bookmarks),
            Err(ProtocolError::InvalidFrame)
        );

        let directory = frame(GrantRecord::ScopedDirectory {
            id: id("directory"),
            role: ResourceRole::ApiSocketDirectory,
            access: GrantAccess::CreateChildren,
            identity: ObjectIdentity {
                device: 1,
                inode: 2,
            },
            bookmark_bytes: MAX_BOOKMARK_BYTES,
            fragment_count: MAX_GRANT_RECORDS,
        });
        assert!(encode_grant_frame(&directory).is_ok());

        let fragment_id = id(&"f".repeat(MAX_GRANT_ID_BYTES));
        let exact_fragment_bytes =
            MAX_GRANT_DATAGRAM_BYTES - GRANT_HEADER_BYTES - 1 - fragment_id.as_bytes().len() - 4;
        let exact_fragment = frame(GrantRecord::BookmarkFragment {
            id: fragment_id.clone(),
            offset: 0,
            bytes: vec![0; exact_fragment_bytes],
        });
        assert_eq!(
            encode_grant_frame(&exact_fragment)
                .expect("exact datagram should encode")
                .len(),
            MAX_GRANT_DATAGRAM_BYTES
        );
        let excessive_fragment = frame(GrantRecord::BookmarkFragment {
            id: fragment_id,
            offset: 0,
            bytes: vec![0; exact_fragment_bytes + 1],
        });
        assert_eq!(
            encode_grant_frame(&excessive_fragment),
            Err(ProtocolError::InvalidFrame)
        );
    }
}
