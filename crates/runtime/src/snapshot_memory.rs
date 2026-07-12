//! Native snapshot guest-memory image encoding and anonymous loading.

use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

use crc64::crc64;

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryAllocationError,
    GuestMemoryError, GuestMemoryLayout, GuestMemoryRange, aarch64,
};
use crate::snapshot_format::{
    NATIVE_V1_ARM64_ARCHITECTURE_ID, NATIVE_V1_GUEST_PAGE_SIZE,
    NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES, NATIVE_V1_SNAPSHOT_VERSION, SnapshotArchitecture,
    SnapshotFormatVersion, SnapshotIntegrity,
};

const SNAPSHOT_MEMORY_IMAGE_MAGIC: [u8; 8] = *b"BANGMEM\0";
const SNAPSHOT_MEMORY_BINDING_MAGIC: [u8; 8] = *b"BANGMBND";
const SNAPSHOT_MEMORY_RESERVED_FLAGS: u32 = 0;
const SNAPSHOT_MEMORY_BINDING_RESERVED: u32 = 0;
const SNAPSHOT_MEMORY_IMAGE_ID_BYTES: usize = 16;
const REDACTED: &str = "<redacted>";

const IMAGE_MAGIC_OFFSET: usize = 0;
const IMAGE_VERSION_MAJOR_OFFSET: usize = 8;
const IMAGE_VERSION_MINOR_OFFSET: usize = 10;
const IMAGE_VERSION_PATCH_OFFSET: usize = 12;
const IMAGE_ARCHITECTURE_OFFSET: usize = 14;
const IMAGE_GUEST_PAGE_SIZE_OFFSET: usize = 16;
const IMAGE_RESERVED_FLAGS_OFFSET: usize = 20;
const IMAGE_ID_OFFSET: usize = 24;
const IMAGE_DATA_LENGTH_OFFSET: usize = 40;

const BINDING_MAGIC_OFFSET: usize = 0;
const BINDING_VERSION_MAJOR_OFFSET: usize = 8;
const BINDING_VERSION_MINOR_OFFSET: usize = 10;
const BINDING_VERSION_PATCH_OFFSET: usize = 12;
const BINDING_ARCHITECTURE_OFFSET: usize = 14;
const BINDING_GUEST_PAGE_SIZE_OFFSET: usize = 16;
const BINDING_RESERVED_FLAGS_OFFSET: usize = 20;
const BINDING_IMAGE_ID_OFFSET: usize = 24;
const BINDING_DATA_LENGTH_OFFSET: usize = 40;
const BINDING_FILE_LENGTH_OFFSET: usize = 48;
const BINDING_CHECKSUM_OFFSET: usize = 56;
const BINDING_RANGE_COUNT_OFFSET: usize = 64;
const BINDING_RESERVED_OFFSET: usize = 68;
const BINDING_RANGE_START_OFFSET: usize = 0;
const BINDING_RANGE_SIZE_OFFSET: usize = 8;
const BINDING_RANGE_FILE_OFFSET: usize = 16;

/// Fixed native-v1 guest-memory image header size in bytes.
pub const SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES: usize = 48;

/// Native-v1 guest-memory image integrity trailer size in bytes.
pub const SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES: usize = 8;

/// Fixed native-v1 state-embeddable memory-binding header size.
pub const SNAPSHOT_MEMORY_BINDING_HEADER_BYTES: usize = 72;

/// Size of one native-v1 GPA range binding.
pub const SNAPSHOT_MEMORY_BINDING_RANGE_BYTES: usize = 24;

/// Maximum number of exact guest-memory regions described by native v1.
pub const NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES: usize = 4096;

/// Maximum native-v1 guest-memory data length.
pub const NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES: u64 = 0x00ff_8000_0000;

/// Maximum encoded native-v1 memory-binding size.
pub const NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES: usize = SNAPSHOT_MEMORY_BINDING_HEADER_BYTES
    + NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES * SNAPSHOT_MEMORY_BINDING_RANGE_BYTES;

/// Reusable buffer size for native-v1 memory image I/O.
pub const SNAPSHOT_MEMORY_IO_CHUNK_BYTES: usize = 1024 * 1024;

const SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64: u64 = SNAPSHOT_MEMORY_IO_CHUNK_BYTES as u64;

/// Persistent native-v1 state-to-memory image identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotMemoryImageId([u8; SNAPSHOT_MEMORY_IMAGE_ID_BYTES]);

impl SnapshotMemoryImageId {
    /// Returns the exact identity bytes stored in snapshot artifacts.
    pub const fn as_bytes(&self) -> &[u8; SNAPSHOT_MEMORY_IMAGE_ID_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SnapshotMemoryImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// One exact GPA range and its absolute memory-image file offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotMemoryRangeBinding {
    range: GuestMemoryRange,
    file_offset: u64,
}

impl SnapshotMemoryRangeBinding {
    /// Returns the exact guest-physical range.
    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    /// Returns the absolute file offset of this range's first byte.
    pub const fn file_offset(self) -> u64 {
        self.file_offset
    }
}

/// Validated native-v1 state-to-memory artifact binding.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotMemoryBinding {
    image_id: SnapshotMemoryImageId,
    data_length: u64,
    file_length: u64,
    checksum: u64,
    ranges: Vec<SnapshotMemoryRangeBinding>,
}

impl SnapshotMemoryBinding {
    /// Returns the exact native snapshot data-format version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V1_SNAPSHOT_VERSION
    }

    /// Returns the guest architecture required by this binding.
    pub const fn architecture(&self) -> SnapshotArchitecture {
        SnapshotArchitecture::Arm64
    }

    /// Returns the guest-memory granule required by this binding.
    pub const fn guest_page_size(&self) -> u32 {
        NATIVE_V1_GUEST_PAGE_SIZE
    }

    /// Returns the persistent memory-image identity.
    pub const fn image_id(&self) -> SnapshotMemoryImageId {
        self.image_id
    }

    /// Returns the exact concatenated guest-data length.
    pub const fn data_length(&self) -> u64 {
        self.data_length
    }

    /// Returns the exact complete memory-image file length.
    pub const fn file_length(&self) -> u64 {
        self.file_length
    }

    /// Returns the memory-image integrity algorithm.
    pub const fn integrity(&self) -> SnapshotIntegrity {
        SnapshotIntegrity::Crc64Jones
    }

    /// Returns the CRC-64/Jones value bound by the state payload.
    pub const fn checksum(&self) -> u64 {
        self.checksum
    }

    /// Returns the exact ordered GPA range bindings.
    pub fn ranges(&self) -> &[SnapshotMemoryRangeBinding] {
        &self.ranges
    }
}

impl fmt::Debug for SnapshotMemoryBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotMemoryBinding")
            .field("version", &NATIVE_V1_SNAPSHOT_VERSION)
            .field("architecture", &SnapshotArchitecture::Arm64)
            .field("guest_page_size", &NATIVE_V1_GUEST_PAGE_SIZE)
            .field("image_id", &REDACTED)
            .field("data_length", &self.data_length)
            .field("file_length", &self.file_length)
            .field("checksum", &REDACTED)
            .field("range_count", &self.ranges.len())
            .finish()
    }
}

/// Native-v1 memory-binding encoding or validation failure.
#[derive(Debug)]
pub enum SnapshotMemoryBindingError {
    /// Input ended before the declared or minimum binding length.
    Truncated { expected: usize, actual: usize },
    /// Bytes remain after the exact declared binding length.
    TrailingData { expected: usize, actual: usize },
    /// The binding does not carry bangbang native memory-binding magic.
    InvalidMagic,
    /// The binding semantic version is unsupported.
    UnsupportedVersion(SnapshotFormatVersion),
    /// The binding architecture identifier is incompatible.
    IncompatibleArchitecture(u16),
    /// The binding guest-memory granule is incompatible.
    IncompatibleGuestPageSize(u32),
    /// Native-v1 binding flags are nonzero.
    UnsupportedFlags(u32),
    /// Native-v1 binding reserved bytes are nonzero.
    UnsupportedReserved(u32),
    /// The range count is empty or exceeds the reader policy.
    RangeCountOutOfBounds { count: usize, maximum: usize },
    /// Binding length or offset arithmetic overflowed.
    LengthOverflow,
    /// The declared memory data exceeds the native-v1 policy.
    DataTooLarge { length: u64, maximum: u64 },
    /// The complete image length is inconsistent with the data length.
    FileLengthMismatch { expected: u64, actual: u64 },
    /// Range metadata allocation failed.
    MetadataAllocationFailed { source: TryReserveError },
    /// A GPA range is invalid for native v1.
    InvalidRange {
        index: usize,
        source: GuestMemoryError,
    },
    /// A range has a noncanonical absolute file offset.
    NonCanonicalFileOffset {
        index: usize,
        expected: u64,
        actual: u64,
    },
    /// The declared data length differs from the range-size sum.
    DataLengthMismatch { expected: u64, actual: u64 },
}

impl fmt::Display for SnapshotMemoryBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { expected, actual } => write!(
                f,
                "snapshot memory binding is truncated: expected {expected} bytes, found {actual}"
            ),
            Self::TrailingData { expected, actual } => write!(
                f,
                "snapshot memory binding has trailing data: expected {expected} bytes, found {actual}"
            ),
            Self::InvalidMagic => f.write_str("snapshot memory binding magic is invalid"),
            Self::UnsupportedVersion(version) => {
                write!(
                    f,
                    "snapshot memory binding version {version} is unsupported"
                )
            }
            Self::IncompatibleArchitecture(architecture) => write!(
                f,
                "snapshot memory binding architecture identifier {architecture} is incompatible"
            ),
            Self::IncompatibleGuestPageSize(page_size) => write!(
                f,
                "snapshot memory binding guest page size {page_size} is incompatible"
            ),
            Self::UnsupportedFlags(flags) => write!(
                f,
                "snapshot memory binding has unsupported flags 0x{flags:08x}"
            ),
            Self::UnsupportedReserved(reserved) => write!(
                f,
                "snapshot memory binding has unsupported reserved value 0x{reserved:08x}"
            ),
            Self::RangeCountOutOfBounds { count, maximum } => write!(
                f,
                "snapshot memory binding range count {count} is outside 1..={maximum}"
            ),
            Self::LengthOverflow => {
                f.write_str("snapshot memory binding length arithmetic overflowed")
            }
            Self::DataTooLarge { length, maximum } => write!(
                f,
                "snapshot memory data length {length} exceeds {maximum} byte limit"
            ),
            Self::FileLengthMismatch { expected, actual } => write!(
                f,
                "snapshot memory file length {actual} does not match expected {expected}"
            ),
            Self::MetadataAllocationFailed { source } => write!(
                f,
                "failed to allocate snapshot memory binding metadata: {source}"
            ),
            Self::InvalidRange { index, source } => {
                write!(
                    f,
                    "snapshot memory binding range {index} is invalid: {source}"
                )
            }
            Self::NonCanonicalFileOffset {
                index,
                expected,
                actual,
            } => write!(
                f,
                "snapshot memory binding range {index} file offset {actual} does not match canonical offset {expected}"
            ),
            Self::DataLengthMismatch { expected, actual } => write!(
                f,
                "snapshot memory binding range bytes {actual} do not match declared data length {expected}"
            ),
        }
    }
}

impl std::error::Error for SnapshotMemoryBindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::InvalidRange { source, .. } => Some(source),
            Self::Truncated { .. }
            | Self::TrailingData { .. }
            | Self::InvalidMagic
            | Self::UnsupportedVersion(_)
            | Self::IncompatibleArchitecture(_)
            | Self::IncompatibleGuestPageSize(_)
            | Self::UnsupportedFlags(_)
            | Self::UnsupportedReserved(_)
            | Self::RangeCountOutOfBounds { .. }
            | Self::LengthOverflow
            | Self::DataTooLarge { .. }
            | Self::FileLengthMismatch { .. }
            | Self::NonCanonicalFileOffset { .. }
            | Self::DataLengthMismatch { .. } => None,
        }
    }
}

/// Stable stage associated with a redacted memory-image I/O failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMemoryIoStage {
    InitialPosition,
    EndLength,
    Rewind,
    Header,
    Data { range_index: usize },
    Trailer,
    TrailingProbe,
    FinalPosition,
    FinalEndLength,
}

impl fmt::Display for SnapshotMemoryIoStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialPosition => f.write_str("initial position"),
            Self::EndLength => f.write_str("end length"),
            Self::Rewind => f.write_str("rewind"),
            Self::Header => f.write_str("header"),
            Self::Data { range_index } => write!(f, "range {range_index} data"),
            Self::Trailer => f.write_str("integrity trailer"),
            Self::TrailingProbe => f.write_str("trailing-data probe"),
            Self::FinalPosition => f.write_str("final position"),
            Self::FinalEndLength => f.write_str("final end length"),
        }
    }
}

/// Native-v1 guest-memory image write failure.
#[derive(Debug)]
pub enum SnapshotMemoryWriteError {
    Binding(SnapshotMemoryBindingError),
    IdentityUnavailable,
    ChunkAllocationFailed {
        source: TryReserveError,
    },
    InvalidInitialPosition {
        actual: u64,
    },
    NonEmptyOutput {
        length: u64,
    },
    PositionMismatch {
        stage: SnapshotMemoryIoStage,
        expected: u64,
        actual: u64,
    },
    OutputLengthMismatch {
        expected: u64,
        actual: u64,
    },
    Io {
        stage: SnapshotMemoryIoStage,
        kind: io::ErrorKind,
    },
    GuestMemoryRead {
        range_index: usize,
        source: GuestMemoryAccessError,
    },
    Cancelled {
        stage: SnapshotMemoryIoStage,
    },
}

impl fmt::Display for SnapshotMemoryWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(source) => write!(f, "invalid snapshot memory binding: {source}"),
            Self::IdentityUnavailable => {
                f.write_str("snapshot memory image identity randomness is unavailable")
            }
            Self::ChunkAllocationFailed { source } => {
                write!(f, "failed to allocate snapshot memory I/O buffer: {source}")
            }
            Self::InvalidInitialPosition { actual } => write!(
                f,
                "snapshot memory output starts at position {actual}, expected 0"
            ),
            Self::NonEmptyOutput { length } => write!(
                f,
                "snapshot memory output is not empty: observed length {length}"
            ),
            Self::PositionMismatch {
                stage,
                expected,
                actual,
            } => write!(
                f,
                "snapshot memory output {stage} is {actual}, expected {expected}"
            ),
            Self::OutputLengthMismatch { expected, actual } => write!(
                f,
                "snapshot memory output length {actual} does not match expected {expected}"
            ),
            Self::Io { stage, kind } => {
                write!(f, "snapshot memory output {stage} failed with {kind:?}")
            }
            Self::GuestMemoryRead {
                range_index,
                source,
            } => write!(
                f,
                "snapshot memory range {range_index} read failed: {source}"
            ),
            Self::Cancelled { stage } => {
                write!(f, "snapshot memory output was cancelled before {stage}")
            }
        }
    }
}

impl std::error::Error for SnapshotMemoryWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(source) => Some(source),
            Self::ChunkAllocationFailed { source } => Some(source),
            Self::GuestMemoryRead { source, .. } => Some(source),
            Self::IdentityUnavailable
            | Self::InvalidInitialPosition { .. }
            | Self::NonEmptyOutput { .. }
            | Self::PositionMismatch { .. }
            | Self::OutputLengthMismatch { .. }
            | Self::Io { .. }
            | Self::Cancelled { .. } => None,
        }
    }
}

impl From<SnapshotMemoryBindingError> for SnapshotMemoryWriteError {
    fn from(source: SnapshotMemoryBindingError) -> Self {
        Self::Binding(source)
    }
}

/// Native-v1 guest-memory image anonymous-load failure.
#[derive(Debug)]
pub enum SnapshotMemoryLoadError {
    ChunkAllocationFailed {
        source: TryReserveError,
    },
    LayoutMetadataAllocationFailed {
        source: TryReserveError,
    },
    InvalidInitialPosition {
        actual: u64,
    },
    InputLengthMismatch {
        expected: u64,
        actual: u64,
    },
    PositionMismatch {
        stage: SnapshotMemoryIoStage,
        expected: u64,
        actual: u64,
    },
    Io {
        stage: SnapshotMemoryIoStage,
        kind: io::ErrorKind,
    },
    UnexpectedEnd {
        stage: SnapshotMemoryIoStage,
    },
    InvalidHeader,
    InvalidMagic,
    UnsupportedVersion(SnapshotFormatVersion),
    IncompatibleArchitecture(u16),
    IncompatibleGuestPageSize(u32),
    UnsupportedFlags(u32),
    ImageIdMismatch,
    DataLengthMismatch {
        expected: u64,
        actual: u64,
    },
    InvalidBindingLayout {
        source: GuestMemoryError,
    },
    GuestMemoryAllocation {
        source: GuestMemoryAllocationError,
    },
    GuestMemoryWrite {
        range_index: usize,
        source: GuestMemoryAccessError,
    },
    TrailingData,
    IntegrityMismatch,
    BindingChecksumMismatch,
}

impl fmt::Display for SnapshotMemoryLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChunkAllocationFailed { source } => {
                write!(f, "failed to allocate snapshot memory I/O buffer: {source}")
            }
            Self::LayoutMetadataAllocationFailed { source } => write!(
                f,
                "failed to allocate snapshot memory layout metadata: {source}"
            ),
            Self::InvalidInitialPosition { actual } => write!(
                f,
                "snapshot memory input starts at position {actual}, expected 0"
            ),
            Self::InputLengthMismatch { expected, actual } => write!(
                f,
                "snapshot memory input length {actual} does not match expected {expected}"
            ),
            Self::PositionMismatch {
                stage,
                expected,
                actual,
            } => write!(
                f,
                "snapshot memory input {stage} is {actual}, expected {expected}"
            ),
            Self::Io { stage, kind } => {
                write!(f, "snapshot memory input {stage} failed with {kind:?}")
            }
            Self::UnexpectedEnd { stage } => {
                write!(f, "snapshot memory input ended during {stage}")
            }
            Self::InvalidHeader => f.write_str("snapshot memory image header is invalid"),
            Self::InvalidMagic => f.write_str("snapshot memory image magic is invalid"),
            Self::UnsupportedVersion(version) => {
                write!(f, "snapshot memory image version {version} is unsupported")
            }
            Self::IncompatibleArchitecture(architecture) => write!(
                f,
                "snapshot memory image architecture identifier {architecture} is incompatible"
            ),
            Self::IncompatibleGuestPageSize(page_size) => write!(
                f,
                "snapshot memory image guest page size {page_size} is incompatible"
            ),
            Self::UnsupportedFlags(flags) => write!(
                f,
                "snapshot memory image has unsupported flags 0x{flags:08x}"
            ),
            Self::ImageIdMismatch => {
                f.write_str("snapshot state and memory image identities do not match")
            }
            Self::DataLengthMismatch { expected, actual } => write!(
                f,
                "snapshot memory image data length {actual} does not match bound length {expected}"
            ),
            Self::InvalidBindingLayout { source } => {
                write!(f, "snapshot memory binding layout is invalid: {source}")
            }
            Self::GuestMemoryAllocation { source } => {
                write!(f, "failed to allocate snapshot guest memory: {source}")
            }
            Self::GuestMemoryWrite {
                range_index,
                source,
            } => write!(
                f,
                "snapshot memory range {range_index} load failed: {source}"
            ),
            Self::TrailingData => f.write_str("snapshot memory image has trailing data"),
            Self::IntegrityMismatch => {
                f.write_str("snapshot memory image CRC-64/Jones integrity check failed")
            }
            Self::BindingChecksumMismatch => {
                f.write_str("snapshot state and memory image checksums do not match")
            }
        }
    }
}

impl std::error::Error for SnapshotMemoryLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ChunkAllocationFailed { source }
            | Self::LayoutMetadataAllocationFailed { source } => Some(source),
            Self::InvalidBindingLayout { source } => Some(source),
            Self::GuestMemoryAllocation { source } => Some(source),
            Self::GuestMemoryWrite { source, .. } => Some(source),
            Self::InvalidInitialPosition { .. }
            | Self::InputLengthMismatch { .. }
            | Self::PositionMismatch { .. }
            | Self::Io { .. }
            | Self::UnexpectedEnd { .. }
            | Self::InvalidHeader
            | Self::InvalidMagic
            | Self::UnsupportedVersion(_)
            | Self::IncompatibleArchitecture(_)
            | Self::IncompatibleGuestPageSize(_)
            | Self::UnsupportedFlags(_)
            | Self::ImageIdMismatch
            | Self::DataLengthMismatch { .. }
            | Self::TrailingData
            | Self::IntegrityMismatch
            | Self::BindingChecksumMismatch => None,
        }
    }
}

/// Deterministically encodes a validated native-v1 memory binding.
pub fn encode_snapshot_memory_binding(
    binding: &SnapshotMemoryBinding,
) -> Result<Vec<u8>, SnapshotMemoryBindingError> {
    let range_bytes = binding
        .ranges
        .len()
        .checked_mul(SNAPSHOT_MEMORY_BINDING_RANGE_BYTES)
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
    let encoded_length = SNAPSHOT_MEMORY_BINDING_HEADER_BYTES
        .checked_add(range_bytes)
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
    let range_count = u32::try_from(binding.ranges.len())
        .map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;

    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(encoded_length)
        .map_err(|source| SnapshotMemoryBindingError::MetadataAllocationFailed { source })?;
    encoded.extend_from_slice(&SNAPSHOT_MEMORY_BINDING_MAGIC);
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.major().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.minor().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.patch().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_ARM64_ARCHITECTURE_ID.to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_GUEST_PAGE_SIZE.to_le_bytes());
    encoded.extend_from_slice(&SNAPSHOT_MEMORY_RESERVED_FLAGS.to_le_bytes());
    encoded.extend_from_slice(binding.image_id.as_bytes());
    encoded.extend_from_slice(&binding.data_length.to_le_bytes());
    encoded.extend_from_slice(&binding.file_length.to_le_bytes());
    encoded.extend_from_slice(&binding.checksum.to_le_bytes());
    encoded.extend_from_slice(&range_count.to_le_bytes());
    encoded.extend_from_slice(&SNAPSHOT_MEMORY_BINDING_RESERVED.to_le_bytes());
    for range in &binding.ranges {
        encoded.extend_from_slice(&range.range.start().raw_value().to_le_bytes());
        encoded.extend_from_slice(&range.range.size().to_le_bytes());
        encoded.extend_from_slice(&range.file_offset.to_le_bytes());
    }

    Ok(encoded)
}

/// Decodes and fully validates a native-v1 state-embeddable memory binding.
pub fn decode_snapshot_memory_binding(
    bytes: &[u8],
) -> Result<SnapshotMemoryBinding, SnapshotMemoryBindingError> {
    if bytes.len() < SNAPSHOT_MEMORY_BINDING_HEADER_BYTES {
        return Err(SnapshotMemoryBindingError::Truncated {
            expected: SNAPSHOT_MEMORY_BINDING_HEADER_BYTES,
            actual: bytes.len(),
        });
    }
    if read_array::<8>(bytes, BINDING_MAGIC_OFFSET)? != SNAPSHOT_MEMORY_BINDING_MAGIC {
        return Err(SnapshotMemoryBindingError::InvalidMagic);
    }

    let range_count = u32::from_le_bytes(read_array::<4>(bytes, BINDING_RANGE_COUNT_OFFSET)?);
    let range_count =
        usize::try_from(range_count).map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;
    validate_range_count(range_count)?;
    let range_bytes = range_count
        .checked_mul(SNAPSHOT_MEMORY_BINDING_RANGE_BYTES)
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
    let expected_length = SNAPSHOT_MEMORY_BINDING_HEADER_BYTES
        .checked_add(range_bytes)
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
    if bytes.len() < expected_length {
        return Err(SnapshotMemoryBindingError::Truncated {
            expected: expected_length,
            actual: bytes.len(),
        });
    }
    if bytes.len() > expected_length {
        return Err(SnapshotMemoryBindingError::TrailingData {
            expected: expected_length,
            actual: bytes.len(),
        });
    }

    let version = SnapshotFormatVersion::new(
        u16::from_le_bytes(read_array::<2>(bytes, BINDING_VERSION_MAJOR_OFFSET)?),
        u16::from_le_bytes(read_array::<2>(bytes, BINDING_VERSION_MINOR_OFFSET)?),
        u16::from_le_bytes(read_array::<2>(bytes, BINDING_VERSION_PATCH_OFFSET)?),
    );
    if version != NATIVE_V1_SNAPSHOT_VERSION {
        return Err(SnapshotMemoryBindingError::UnsupportedVersion(version));
    }
    let architecture = u16::from_le_bytes(read_array::<2>(bytes, BINDING_ARCHITECTURE_OFFSET)?);
    if architecture != NATIVE_V1_ARM64_ARCHITECTURE_ID {
        return Err(SnapshotMemoryBindingError::IncompatibleArchitecture(
            architecture,
        ));
    }
    let guest_page_size =
        u32::from_le_bytes(read_array::<4>(bytes, BINDING_GUEST_PAGE_SIZE_OFFSET)?);
    if guest_page_size != NATIVE_V1_GUEST_PAGE_SIZE {
        return Err(SnapshotMemoryBindingError::IncompatibleGuestPageSize(
            guest_page_size,
        ));
    }
    let flags = u32::from_le_bytes(read_array::<4>(bytes, BINDING_RESERVED_FLAGS_OFFSET)?);
    if flags != SNAPSHOT_MEMORY_RESERVED_FLAGS {
        return Err(SnapshotMemoryBindingError::UnsupportedFlags(flags));
    }
    let reserved = u32::from_le_bytes(read_array::<4>(bytes, BINDING_RESERVED_OFFSET)?);
    if reserved != SNAPSHOT_MEMORY_BINDING_RESERVED {
        return Err(SnapshotMemoryBindingError::UnsupportedReserved(reserved));
    }

    let image_id = SnapshotMemoryImageId(read_array::<SNAPSHOT_MEMORY_IMAGE_ID_BYTES>(
        bytes,
        BINDING_IMAGE_ID_OFFSET,
    )?);
    let data_length = u64::from_le_bytes(read_array::<8>(bytes, BINDING_DATA_LENGTH_OFFSET)?);
    validate_data_length(data_length)?;
    let file_length = u64::from_le_bytes(read_array::<8>(bytes, BINDING_FILE_LENGTH_OFFSET)?);
    let expected_file_length = memory_file_length(data_length)?;
    if file_length != expected_file_length {
        return Err(SnapshotMemoryBindingError::FileLengthMismatch {
            expected: expected_file_length,
            actual: file_length,
        });
    }
    let checksum = u64::from_le_bytes(read_array::<8>(bytes, BINDING_CHECKSUM_OFFSET)?);

    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(range_count)
        .map_err(|source| SnapshotMemoryBindingError::MetadataAllocationFailed { source })?;
    let mut expected_file_offset = u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
        .map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;
    let mut accumulated_data_length = 0_u64;
    let mut previous = None;
    for index in 0..range_count {
        let entry_offset = SNAPSHOT_MEMORY_BINDING_HEADER_BYTES
            .checked_add(
                index
                    .checked_mul(SNAPSHOT_MEMORY_BINDING_RANGE_BYTES)
                    .ok_or(SnapshotMemoryBindingError::LengthOverflow)?,
            )
            .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
        let start = u64::from_le_bytes(read_array::<8>(
            bytes,
            entry_offset
                .checked_add(BINDING_RANGE_START_OFFSET)
                .ok_or(SnapshotMemoryBindingError::LengthOverflow)?,
        )?);
        let size = u64::from_le_bytes(read_array::<8>(
            bytes,
            entry_offset
                .checked_add(BINDING_RANGE_SIZE_OFFSET)
                .ok_or(SnapshotMemoryBindingError::LengthOverflow)?,
        )?);
        let file_offset = u64::from_le_bytes(read_array::<8>(
            bytes,
            entry_offset
                .checked_add(BINDING_RANGE_FILE_OFFSET)
                .ok_or(SnapshotMemoryBindingError::LengthOverflow)?,
        )?);
        let range = GuestMemoryRange::new(GuestAddress::new(start), size)
            .map_err(|source| SnapshotMemoryBindingError::InvalidRange { index, source })?;
        validate_native_range(index, range, previous)?;
        if file_offset != expected_file_offset {
            return Err(SnapshotMemoryBindingError::NonCanonicalFileOffset {
                index,
                expected: expected_file_offset,
                actual: file_offset,
            });
        }

        accumulated_data_length = accumulated_data_length
            .checked_add(size)
            .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
        expected_file_offset = expected_file_offset
            .checked_add(size)
            .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
        ranges.push(SnapshotMemoryRangeBinding { range, file_offset });
        previous = Some(range);
    }
    if accumulated_data_length != data_length {
        return Err(SnapshotMemoryBindingError::DataLengthMismatch {
            expected: data_length,
            actual: accumulated_data_length,
        });
    }

    Ok(SnapshotMemoryBinding {
        image_id,
        data_length,
        file_length,
        checksum,
        ranges,
    })
}

/// Streams a complete native-v1 image from anonymous guest memory.
pub fn write_snapshot_memory_image<W: Write + Seek>(
    memory: &GuestMemory,
    writer: &mut W,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError> {
    write_snapshot_memory_image_with_cancel(memory, writer, |_| false)
}

/// Streams a complete native-v1 image with cooperative cancellation checkpoints.
///
/// The callback runs before output preflight, every bounded data chunk, the
/// integrity trailer, and final length validation. Returning `true` cancels the
/// operation without publishing a binding. It cannot preempt one in-progress
/// `Write` or `Seek` call supplied by the caller.
pub fn write_snapshot_memory_image_with_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    is_cancelled: C,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotMemoryIoStage) -> bool,
{
    let image_id = generate_image_id()?;
    write_snapshot_memory_image_with_id_and_cancel(memory, writer, image_id, is_cancelled)
}

/// Loads a complete native-v1 image into newly allocated anonymous guest memory.
pub fn load_snapshot_memory_image<R: Read + Seek>(
    binding: &SnapshotMemoryBinding,
    reader: &mut R,
) -> Result<GuestMemory, SnapshotMemoryLoadError> {
    load_snapshot_memory_image_with_allocator(binding, reader, GuestMemory::allocate)
}

/// Verifies fixed output evidence against a trusted codec-produced binding.
///
/// This checks positions, length, the complete fixed header, and the stored
/// integrity trailer without allocating guest memory or re-reading the data.
/// Full CRC and GPA-range validation remains the responsibility of
/// [`load_snapshot_memory_image`].
pub(crate) fn verify_snapshot_memory_image_output<R: Read + Seek>(
    binding: &SnapshotMemoryBinding,
    reader: &mut R,
) -> Result<(), SnapshotMemoryLoadError> {
    let position = reader
        .stream_position()
        .map_err(|source| SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::FinalPosition,
            kind: source.kind(),
        })?;
    if position != binding.file_length {
        return Err(SnapshotMemoryLoadError::PositionMismatch {
            stage: SnapshotMemoryIoStage::FinalPosition,
            expected: binding.file_length,
            actual: position,
        });
    }

    let rewind = reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::Rewind,
            kind: source.kind(),
        })?;
    if rewind != 0 {
        return Err(SnapshotMemoryLoadError::PositionMismatch {
            stage: SnapshotMemoryIoStage::Rewind,
            expected: 0,
            actual: rewind,
        });
    }
    preflight_input(reader, binding.file_length)?;

    let mut header = [0_u8; SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES];
    read_exact_stage(reader, &mut header, SnapshotMemoryIoStage::Header)?;
    validate_image_header(&header, binding)?;

    let trailer_offset = binding
        .file_length
        .checked_sub(SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES as u64)
        .ok_or(SnapshotMemoryLoadError::InvalidHeader)?;
    reader
        .seek(SeekFrom::Start(trailer_offset))
        .map_err(|source| SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::Trailer,
            kind: source.kind(),
        })?;
    let mut trailer = [0_u8; SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES];
    read_exact_stage(reader, &mut trailer, SnapshotMemoryIoStage::Trailer)?;
    require_observed_end(reader)?;
    let final_position =
        reader
            .stream_position()
            .map_err(|source| SnapshotMemoryLoadError::Io {
                stage: SnapshotMemoryIoStage::FinalPosition,
                kind: source.kind(),
            })?;
    if final_position != binding.file_length {
        return Err(SnapshotMemoryLoadError::PositionMismatch {
            stage: SnapshotMemoryIoStage::FinalPosition,
            expected: binding.file_length,
            actual: final_position,
        });
    }
    if u64::from_le_bytes(trailer) != binding.checksum {
        return Err(SnapshotMemoryLoadError::BindingChecksumMismatch);
    }
    Ok(())
}

#[cfg(test)]
fn write_snapshot_memory_image_with_id<W: Write + Seek>(
    memory: &GuestMemory,
    writer: &mut W,
    image_id: SnapshotMemoryImageId,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError> {
    write_snapshot_memory_image_with_id_and_cancel(memory, writer, image_id, |_| false)
}

fn write_snapshot_memory_image_with_id_and_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    image_id: SnapshotMemoryImageId,
    is_cancelled: C,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotMemoryIoStage) -> bool,
{
    let (ranges, data_length, file_length) = ranges_from_memory(memory)?;
    write_snapshot_memory_image_from_parts_with_cancel(
        writer,
        image_id,
        ranges,
        data_length,
        file_length,
        |destination, address| memory.read_slice(destination, address),
        is_cancelled,
    )
}

#[cfg(test)]
fn write_snapshot_memory_image_from_parts<W, F>(
    writer: &mut W,
    image_id: SnapshotMemoryImageId,
    ranges: Vec<SnapshotMemoryRangeBinding>,
    data_length: u64,
    file_length: u64,
    read_memory: F,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError>
where
    W: Write + Seek,
    F: FnMut(&mut [u8], GuestAddress) -> Result<(), GuestMemoryAccessError>,
{
    write_snapshot_memory_image_from_parts_with_cancel(
        writer,
        image_id,
        ranges,
        data_length,
        file_length,
        read_memory,
        |_| false,
    )
}

fn write_snapshot_memory_image_from_parts_with_cancel<W, F, C>(
    writer: &mut W,
    image_id: SnapshotMemoryImageId,
    ranges: Vec<SnapshotMemoryRangeBinding>,
    data_length: u64,
    file_length: u64,
    read_memory: F,
    is_cancelled: C,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError>
where
    W: Write + Seek,
    F: FnMut(&mut [u8], GuestAddress) -> Result<(), GuestMemoryAccessError>,
    C: FnMut(SnapshotMemoryIoStage) -> bool,
{
    write_snapshot_memory_image_from_parts_with_buffer_and_cancel(
        writer,
        image_id,
        ranges,
        SnapshotMemoryImageLengths {
            data_length,
            file_length,
        },
        allocate_io_buffer,
        read_memory,
        is_cancelled,
    )
}

#[cfg(test)]
fn write_snapshot_memory_image_from_parts_with_buffer<W, B, F>(
    writer: &mut W,
    image_id: SnapshotMemoryImageId,
    ranges: Vec<SnapshotMemoryRangeBinding>,
    data_length: u64,
    file_length: u64,
    allocate_buffer: B,
    read_memory: F,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError>
where
    W: Write + Seek,
    B: FnOnce() -> Result<Vec<u8>, TryReserveError>,
    F: FnMut(&mut [u8], GuestAddress) -> Result<(), GuestMemoryAccessError>,
{
    write_snapshot_memory_image_from_parts_with_buffer_and_cancel(
        writer,
        image_id,
        ranges,
        SnapshotMemoryImageLengths {
            data_length,
            file_length,
        },
        allocate_buffer,
        read_memory,
        |_| false,
    )
}

#[derive(Clone, Copy)]
struct SnapshotMemoryImageLengths {
    data_length: u64,
    file_length: u64,
}

fn write_snapshot_memory_image_from_parts_with_buffer_and_cancel<W, B, F, C>(
    writer: &mut W,
    image_id: SnapshotMemoryImageId,
    ranges: Vec<SnapshotMemoryRangeBinding>,
    lengths: SnapshotMemoryImageLengths,
    allocate_buffer: B,
    mut read_memory: F,
    mut is_cancelled: C,
) -> Result<SnapshotMemoryBinding, SnapshotMemoryWriteError>
where
    W: Write + Seek,
    B: FnOnce() -> Result<Vec<u8>, TryReserveError>,
    F: FnMut(&mut [u8], GuestAddress) -> Result<(), GuestMemoryAccessError>,
    C: FnMut(SnapshotMemoryIoStage) -> bool,
{
    let mut buffer = allocate_buffer()
        .map_err(|source| SnapshotMemoryWriteError::ChunkAllocationFailed { source })?;
    check_write_cancelled(&mut is_cancelled, SnapshotMemoryIoStage::InitialPosition)?;
    preflight_empty_output(writer)?;

    let header = encode_image_header(image_id, lengths.data_length)?;
    check_write_cancelled(&mut is_cancelled, SnapshotMemoryIoStage::Header)?;
    write_all_stage(writer, &header, SnapshotMemoryIoStage::Header)?;
    let mut checksum = crc64(0, &header);
    for (range_index, range_binding) in ranges.iter().copied().enumerate() {
        let range = range_binding.range;
        let mut current = range.start();
        let mut remaining = range.size();
        while remaining > 0 {
            let stage = SnapshotMemoryIoStage::Data { range_index };
            check_write_cancelled(&mut is_cancelled, stage)?;
            let chunk_length_u64 = remaining.min(SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64);
            let chunk_length = usize::try_from(chunk_length_u64)
                .map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;
            let chunk = buffer
                .get_mut(..chunk_length)
                .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
            read_memory(chunk, current).map_err(|source| {
                SnapshotMemoryWriteError::GuestMemoryRead {
                    range_index,
                    source,
                }
            })?;
            checksum = crc64(checksum, chunk);
            write_all_stage(writer, chunk, stage)?;
            current = current
                .checked_add(chunk_length_u64)
                .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
            remaining -= chunk_length_u64;
        }
    }
    check_write_cancelled(&mut is_cancelled, SnapshotMemoryIoStage::Trailer)?;
    write_all_stage(
        writer,
        &checksum.to_le_bytes(),
        SnapshotMemoryIoStage::Trailer,
    )?;
    check_write_cancelled(&mut is_cancelled, SnapshotMemoryIoStage::FinalEndLength)?;
    validate_final_output_length(writer, lengths.file_length)?;

    Ok(SnapshotMemoryBinding {
        image_id,
        data_length: lengths.data_length,
        file_length: lengths.file_length,
        checksum,
        ranges,
    })
}

fn check_write_cancelled<C>(
    is_cancelled: &mut C,
    stage: SnapshotMemoryIoStage,
) -> Result<(), SnapshotMemoryWriteError>
where
    C: FnMut(SnapshotMemoryIoStage) -> bool,
{
    if is_cancelled(stage) {
        Err(SnapshotMemoryWriteError::Cancelled { stage })
    } else {
        Ok(())
    }
}

fn load_snapshot_memory_image_with_allocator<R, A>(
    binding: &SnapshotMemoryBinding,
    reader: &mut R,
    allocate: A,
) -> Result<GuestMemory, SnapshotMemoryLoadError>
where
    R: Read + Seek,
    A: FnOnce(&GuestMemoryLayout) -> Result<GuestMemory, GuestMemoryAllocationError>,
{
    load_snapshot_memory_image_with(binding, reader, allocate, |memory, source, address| {
        memory.write_slice(source, address)
    })
}

fn load_snapshot_memory_image_with<R, M, A, F>(
    binding: &SnapshotMemoryBinding,
    reader: &mut R,
    allocate: A,
    write_memory: F,
) -> Result<M, SnapshotMemoryLoadError>
where
    R: Read + Seek,
    A: FnOnce(&GuestMemoryLayout) -> Result<M, GuestMemoryAllocationError>,
    F: FnMut(&mut M, &[u8], GuestAddress) -> Result<(), GuestMemoryAccessError>,
{
    load_snapshot_memory_image_with_buffer(
        binding,
        reader,
        allocate_io_buffer,
        allocate,
        write_memory,
    )
}

fn load_snapshot_memory_image_with_buffer<R, M, B, A, F>(
    binding: &SnapshotMemoryBinding,
    reader: &mut R,
    allocate_buffer: B,
    allocate: A,
    mut write_memory: F,
) -> Result<M, SnapshotMemoryLoadError>
where
    R: Read + Seek,
    B: FnOnce() -> Result<Vec<u8>, TryReserveError>,
    A: FnOnce(&GuestMemoryLayout) -> Result<M, GuestMemoryAllocationError>,
    F: FnMut(&mut M, &[u8], GuestAddress) -> Result<(), GuestMemoryAccessError>,
{
    let mut buffer = allocate_buffer()
        .map_err(|source| SnapshotMemoryLoadError::ChunkAllocationFailed { source })?;
    preflight_input(reader, binding.file_length)?;

    let mut header = [0_u8; SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES];
    read_exact_stage(reader, &mut header, SnapshotMemoryIoStage::Header)?;
    validate_image_header(&header, binding)?;
    let mut checksum = crc64(0, &header);

    let layout = binding_layout(binding)?;
    let mut memory = allocate(&layout)
        .map_err(|source| SnapshotMemoryLoadError::GuestMemoryAllocation { source })?;
    for (range_index, range_binding) in binding.ranges.iter().copied().enumerate() {
        let range = range_binding.range;
        let mut current = range.start();
        let mut remaining = range.size();
        while remaining > 0 {
            let chunk_length_u64 = remaining.min(SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64);
            let chunk_length = usize::try_from(chunk_length_u64)
                .map_err(|_| SnapshotMemoryLoadError::InvalidHeader)?;
            let chunk = buffer
                .get_mut(..chunk_length)
                .ok_or(SnapshotMemoryLoadError::InvalidHeader)?;
            read_exact_stage(reader, chunk, SnapshotMemoryIoStage::Data { range_index })?;
            checksum = crc64(checksum, chunk);
            write_memory(&mut memory, chunk, current).map_err(|source| {
                SnapshotMemoryLoadError::GuestMemoryWrite {
                    range_index,
                    source,
                }
            })?;
            current = current
                .checked_add(chunk_length_u64)
                .ok_or(SnapshotMemoryLoadError::InvalidHeader)?;
            remaining -= chunk_length_u64;
        }
    }

    let mut trailer = [0_u8; SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES];
    read_exact_stage(reader, &mut trailer, SnapshotMemoryIoStage::Trailer)?;
    require_observed_end(reader)?;
    let final_position =
        reader
            .stream_position()
            .map_err(|source| SnapshotMemoryLoadError::Io {
                stage: SnapshotMemoryIoStage::FinalPosition,
                kind: source.kind(),
            })?;
    if final_position != binding.file_length {
        return Err(SnapshotMemoryLoadError::PositionMismatch {
            stage: SnapshotMemoryIoStage::FinalPosition,
            expected: binding.file_length,
            actual: final_position,
        });
    }

    let stored_checksum = u64::from_le_bytes(trailer);
    if checksum != stored_checksum {
        return Err(SnapshotMemoryLoadError::IntegrityMismatch);
    }
    if stored_checksum != binding.checksum {
        return Err(SnapshotMemoryLoadError::BindingChecksumMismatch);
    }
    Ok(memory)
}

fn ranges_from_memory(
    memory: &GuestMemory,
) -> Result<(Vec<SnapshotMemoryRangeBinding>, u64, u64), SnapshotMemoryBindingError> {
    let regions = memory.regions();
    validate_range_count(regions.len())?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(regions.len())
        .map_err(|source| SnapshotMemoryBindingError::MetadataAllocationFailed { source })?;
    let mut file_offset = u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
        .map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;
    let mut data_length = 0_u64;
    let mut previous = None;
    for (index, region) in regions.iter().enumerate() {
        let range = region.range();
        validate_native_range(index, range, previous)?;
        ranges.push(SnapshotMemoryRangeBinding { range, file_offset });
        data_length = data_length
            .checked_add(range.size())
            .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
        validate_data_length(data_length)?;
        file_offset = file_offset
            .checked_add(range.size())
            .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
        previous = Some(range);
    }
    let file_length = memory_file_length(data_length)?;
    Ok((ranges, data_length, file_length))
}

fn validate_range_count(count: usize) -> Result<(), SnapshotMemoryBindingError> {
    if count == 0 || count > NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES {
        Err(SnapshotMemoryBindingError::RangeCountOutOfBounds {
            count,
            maximum: NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES,
        })
    } else {
        Ok(())
    }
}

fn validate_data_length(length: u64) -> Result<(), SnapshotMemoryBindingError> {
    if length > NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES {
        Err(SnapshotMemoryBindingError::DataTooLarge {
            length,
            maximum: NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES,
        })
    } else {
        Ok(())
    }
}

fn validate_native_range(
    index: usize,
    range: GuestMemoryRange,
    previous: Option<GuestMemoryRange>,
) -> Result<(), SnapshotMemoryBindingError> {
    range
        .validate_alignment(u64::from(NATIVE_V1_GUEST_PAGE_SIZE))
        .map_err(|source| SnapshotMemoryBindingError::InvalidRange { index, source })?;
    if let Some(previous) = previous {
        if range.start() < previous.start() {
            return Err(SnapshotMemoryBindingError::InvalidRange {
                index,
                source: GuestMemoryError::UnorderedRange {
                    previous,
                    next: range,
                },
            });
        }
        if previous.overlaps(range) {
            return Err(SnapshotMemoryBindingError::InvalidRange {
                index,
                source: GuestMemoryError::OverlappingRange {
                    previous,
                    next: range,
                },
            });
        }
    }
    Ok(())
}

fn memory_file_length(data_length: u64) -> Result<u64, SnapshotMemoryBindingError> {
    let header = u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
        .map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;
    let trailer = u64::try_from(SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES)
        .map_err(|_| SnapshotMemoryBindingError::LengthOverflow)?;
    header
        .checked_add(data_length)
        .and_then(|length| length.checked_add(trailer))
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)
}

fn encode_image_header(
    image_id: SnapshotMemoryImageId,
    data_length: u64,
) -> Result<[u8; SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES], SnapshotMemoryBindingError> {
    let mut header = [0_u8; SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES];
    copy_field(
        &mut header,
        IMAGE_MAGIC_OFFSET,
        &SNAPSHOT_MEMORY_IMAGE_MAGIC,
    )?;
    copy_field(
        &mut header,
        IMAGE_VERSION_MAJOR_OFFSET,
        &NATIVE_V1_SNAPSHOT_VERSION.major().to_le_bytes(),
    )?;
    copy_field(
        &mut header,
        IMAGE_VERSION_MINOR_OFFSET,
        &NATIVE_V1_SNAPSHOT_VERSION.minor().to_le_bytes(),
    )?;
    copy_field(
        &mut header,
        IMAGE_VERSION_PATCH_OFFSET,
        &NATIVE_V1_SNAPSHOT_VERSION.patch().to_le_bytes(),
    )?;
    copy_field(
        &mut header,
        IMAGE_ARCHITECTURE_OFFSET,
        &NATIVE_V1_ARM64_ARCHITECTURE_ID.to_le_bytes(),
    )?;
    copy_field(
        &mut header,
        IMAGE_GUEST_PAGE_SIZE_OFFSET,
        &NATIVE_V1_GUEST_PAGE_SIZE.to_le_bytes(),
    )?;
    copy_field(
        &mut header,
        IMAGE_RESERVED_FLAGS_OFFSET,
        &SNAPSHOT_MEMORY_RESERVED_FLAGS.to_le_bytes(),
    )?;
    copy_field(&mut header, IMAGE_ID_OFFSET, image_id.as_bytes())?;
    copy_field(
        &mut header,
        IMAGE_DATA_LENGTH_OFFSET,
        &data_length.to_le_bytes(),
    )?;
    Ok(header)
}

fn validate_image_header(
    header: &[u8; SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES],
    binding: &SnapshotMemoryBinding,
) -> Result<(), SnapshotMemoryLoadError> {
    if array_at::<8>(header, IMAGE_MAGIC_OFFSET).ok_or(SnapshotMemoryLoadError::InvalidHeader)?
        != SNAPSHOT_MEMORY_IMAGE_MAGIC
    {
        return Err(SnapshotMemoryLoadError::InvalidMagic);
    }
    let version = SnapshotFormatVersion::new(
        u16::from_le_bytes(
            array_at::<2>(header, IMAGE_VERSION_MAJOR_OFFSET)
                .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
        ),
        u16::from_le_bytes(
            array_at::<2>(header, IMAGE_VERSION_MINOR_OFFSET)
                .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
        ),
        u16::from_le_bytes(
            array_at::<2>(header, IMAGE_VERSION_PATCH_OFFSET)
                .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
        ),
    );
    if version != NATIVE_V1_SNAPSHOT_VERSION {
        return Err(SnapshotMemoryLoadError::UnsupportedVersion(version));
    }
    let architecture = u16::from_le_bytes(
        array_at::<2>(header, IMAGE_ARCHITECTURE_OFFSET)
            .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
    );
    if architecture != NATIVE_V1_ARM64_ARCHITECTURE_ID {
        return Err(SnapshotMemoryLoadError::IncompatibleArchitecture(
            architecture,
        ));
    }
    let page_size = u32::from_le_bytes(
        array_at::<4>(header, IMAGE_GUEST_PAGE_SIZE_OFFSET)
            .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
    );
    if page_size != NATIVE_V1_GUEST_PAGE_SIZE {
        return Err(SnapshotMemoryLoadError::IncompatibleGuestPageSize(
            page_size,
        ));
    }
    let flags = u32::from_le_bytes(
        array_at::<4>(header, IMAGE_RESERVED_FLAGS_OFFSET)
            .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
    );
    if flags != SNAPSHOT_MEMORY_RESERVED_FLAGS {
        return Err(SnapshotMemoryLoadError::UnsupportedFlags(flags));
    }
    let image_id = SnapshotMemoryImageId(
        array_at::<SNAPSHOT_MEMORY_IMAGE_ID_BYTES>(header, IMAGE_ID_OFFSET)
            .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
    );
    if image_id != binding.image_id {
        return Err(SnapshotMemoryLoadError::ImageIdMismatch);
    }
    let data_length = u64::from_le_bytes(
        array_at::<8>(header, IMAGE_DATA_LENGTH_OFFSET)
            .ok_or(SnapshotMemoryLoadError::InvalidHeader)?,
    );
    if data_length != binding.data_length {
        return Err(SnapshotMemoryLoadError::DataLengthMismatch {
            expected: binding.data_length,
            actual: data_length,
        });
    }
    Ok(())
}

fn binding_layout(
    binding: &SnapshotMemoryBinding,
) -> Result<GuestMemoryLayout, SnapshotMemoryLoadError> {
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(binding.ranges.len())
        .map_err(|source| SnapshotMemoryLoadError::LayoutMetadataAllocationFailed { source })?;
    ranges.extend(binding.ranges.iter().map(|range| range.range));
    GuestMemoryLayout::new(ranges)
        .map_err(|source| SnapshotMemoryLoadError::InvalidBindingLayout { source })
}

fn generate_image_id() -> Result<SnapshotMemoryImageId, SnapshotMemoryWriteError> {
    generate_image_id_with(|bytes| getrandom::fill(bytes))
}

fn generate_image_id_with<E>(
    fill: impl FnOnce(&mut [u8; SNAPSHOT_MEMORY_IMAGE_ID_BYTES]) -> Result<(), E>,
) -> Result<SnapshotMemoryImageId, SnapshotMemoryWriteError> {
    let mut bytes = [0_u8; SNAPSHOT_MEMORY_IMAGE_ID_BYTES];
    fill(&mut bytes).map_err(|_| SnapshotMemoryWriteError::IdentityUnavailable)?;
    Ok(SnapshotMemoryImageId(bytes))
}

fn allocate_io_buffer() -> Result<Vec<u8>, TryReserveError> {
    allocate_io_buffer_with_size(SNAPSHOT_MEMORY_IO_CHUNK_BYTES)
}

fn allocate_io_buffer_with_size(size: usize) -> Result<Vec<u8>, TryReserveError> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(size)?;
    buffer.resize(size, 0);
    Ok(buffer)
}

fn preflight_empty_output<W: Seek>(writer: &mut W) -> Result<(), SnapshotMemoryWriteError> {
    let initial = writer
        .stream_position()
        .map_err(|source| SnapshotMemoryWriteError::Io {
            stage: SnapshotMemoryIoStage::InitialPosition,
            kind: source.kind(),
        })?;
    if initial != 0 {
        return Err(SnapshotMemoryWriteError::InvalidInitialPosition { actual: initial });
    }
    let end = writer
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotMemoryWriteError::Io {
            stage: SnapshotMemoryIoStage::EndLength,
            kind: source.kind(),
        })?;
    let rewind =
        writer
            .seek(SeekFrom::Start(0))
            .map_err(|source| SnapshotMemoryWriteError::Io {
                stage: SnapshotMemoryIoStage::Rewind,
                kind: source.kind(),
            })?;
    if rewind != 0 {
        return Err(SnapshotMemoryWriteError::PositionMismatch {
            stage: SnapshotMemoryIoStage::Rewind,
            expected: 0,
            actual: rewind,
        });
    }
    if end != 0 {
        return Err(SnapshotMemoryWriteError::NonEmptyOutput { length: end });
    }
    Ok(())
}

fn preflight_input<R: Seek>(
    reader: &mut R,
    expected_length: u64,
) -> Result<(), SnapshotMemoryLoadError> {
    let initial = reader
        .stream_position()
        .map_err(|source| SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::InitialPosition,
            kind: source.kind(),
        })?;
    if initial != 0 {
        return Err(SnapshotMemoryLoadError::InvalidInitialPosition { actual: initial });
    }
    let end = reader
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::EndLength,
            kind: source.kind(),
        })?;
    let rewind = reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::Rewind,
            kind: source.kind(),
        })?;
    if rewind != 0 {
        return Err(SnapshotMemoryLoadError::PositionMismatch {
            stage: SnapshotMemoryIoStage::Rewind,
            expected: 0,
            actual: rewind,
        });
    }
    if end != expected_length {
        return Err(SnapshotMemoryLoadError::InputLengthMismatch {
            expected: expected_length,
            actual: end,
        });
    }
    Ok(())
}

fn validate_final_output_length<W: Seek>(
    writer: &mut W,
    expected_length: u64,
) -> Result<(), SnapshotMemoryWriteError> {
    let position = writer
        .stream_position()
        .map_err(|source| SnapshotMemoryWriteError::Io {
            stage: SnapshotMemoryIoStage::FinalPosition,
            kind: source.kind(),
        })?;
    if position != expected_length {
        return Err(SnapshotMemoryWriteError::PositionMismatch {
            stage: SnapshotMemoryIoStage::FinalPosition,
            expected: expected_length,
            actual: position,
        });
    }
    let end = writer
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotMemoryWriteError::Io {
            stage: SnapshotMemoryIoStage::FinalEndLength,
            kind: source.kind(),
        })?;
    if end != expected_length {
        return Err(SnapshotMemoryWriteError::OutputLengthMismatch {
            expected: expected_length,
            actual: end,
        });
    }
    Ok(())
}

fn write_all_stage<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    stage: SnapshotMemoryIoStage,
) -> Result<(), SnapshotMemoryWriteError> {
    writer
        .write_all(bytes)
        .map_err(|source| SnapshotMemoryWriteError::Io {
            stage,
            kind: source.kind(),
        })
}

fn read_exact_stage<R: Read>(
    reader: &mut R,
    bytes: &mut [u8],
    stage: SnapshotMemoryIoStage,
) -> Result<(), SnapshotMemoryLoadError> {
    reader.read_exact(bytes).map_err(|source| {
        if source.kind() == io::ErrorKind::UnexpectedEof {
            SnapshotMemoryLoadError::UnexpectedEnd { stage }
        } else {
            SnapshotMemoryLoadError::Io {
                stage,
                kind: source.kind(),
            }
        }
    })
}

fn require_observed_end<R: Read>(reader: &mut R) -> Result<(), SnapshotMemoryLoadError> {
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(SnapshotMemoryLoadError::TrailingData),
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(SnapshotMemoryLoadError::Io {
                    stage: SnapshotMemoryIoStage::TrailingProbe,
                    kind: source.kind(),
                });
            }
        }
    }
}

fn copy_field(
    destination: &mut [u8],
    offset: usize,
    source: &[u8],
) -> Result<(), SnapshotMemoryBindingError> {
    let end = offset
        .checked_add(source.len())
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
    let field = destination
        .get_mut(offset..end)
        .ok_or(SnapshotMemoryBindingError::LengthOverflow)?;
    field.copy_from_slice(source);
    Ok(())
}

fn read_array<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotMemoryBindingError> {
    array_at(bytes, offset).ok_or_else(|| {
        let expected = offset.saturating_add(LENGTH);
        SnapshotMemoryBindingError::Truncated {
            expected,
            actual: bytes.len(),
        }
    })
}

fn array_at<const LENGTH: usize>(bytes: &[u8], offset: usize) -> Option<[u8; LENGTH]> {
    let end = offset.checked_add(LENGTH)?;
    let source = bytes.get(offset..end)?;
    let mut result = [0_u8; LENGTH];
    result.copy_from_slice(source);
    Some(result)
}

const _: () = {
    assert!(NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES <= NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES);
    assert!(NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES == aarch64::DRAM_MEM_MAX_SIZE);
};

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Error, ErrorKind, Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const TEST_ALIGNMENT: u64 = 64 * 1024;
    const TEST_IMAGE_ID: SnapshotMemoryImageId = SnapshotMemoryImageId([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should be valid")
    }

    fn memory(ranges: Vec<GuestMemoryRange>) -> GuestMemory {
        let layout = GuestMemoryLayout::new(ranges).expect("test layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn bytes_for_range(range: GuestMemoryRange, seed: u8) -> Vec<u8> {
        let size = usize::try_from(range.size()).expect("test range size should fit usize");
        (0..size)
            .map(|offset| seed.wrapping_add(u8::try_from(offset % 251).expect("modulo fits u8")))
            .collect()
    }

    fn fill_memory(memory: &mut GuestMemory) -> Vec<(GuestMemoryRange, Vec<u8>)> {
        let ranges: Vec<_> = memory
            .regions()
            .iter()
            .map(|region| region.range())
            .collect();
        ranges
            .into_iter()
            .enumerate()
            .map(|(index, range)| {
                let bytes = bytes_for_range(
                    range,
                    u8::try_from(index + 1).expect("test range index should fit u8") * 17,
                );
                memory
                    .write_slice(&bytes, range.start())
                    .expect("test bytes should write");
                (range, bytes)
            })
            .collect()
    }

    fn assert_memory_bytes(memory: &GuestMemory, expected: &[(GuestMemoryRange, Vec<u8>)]) {
        assert_eq!(
            memory
                .regions()
                .iter()
                .map(|region| region.range())
                .collect::<Vec<_>>(),
            expected.iter().map(|(range, _)| *range).collect::<Vec<_>>()
        );
        for (range, expected_bytes) in expected {
            let mut actual = vec![0; expected_bytes.len()];
            memory
                .read_slice(&mut actual, range.start())
                .expect("loaded bytes should read");
            assert_eq!(&actual, expected_bytes);
        }
    }

    fn write_image(memory: &GuestMemory) -> (Vec<u8>, SnapshotMemoryBinding) {
        let mut writer = Cursor::new(Vec::new());
        let binding = write_snapshot_memory_image_with_id(memory, &mut writer, TEST_IMAGE_ID)
            .expect("test image should write");
        (writer.into_inner(), binding)
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(array_at::<8>(bytes, offset).expect("test field should exist"))
    }

    fn set_u16(bytes: &mut [u8], offset: usize, value: u16) {
        copy_field(bytes, offset, &value.to_le_bytes()).expect("test field should exist");
    }

    fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
        copy_field(bytes, offset, &value.to_le_bytes()).expect("test field should exist");
    }

    fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
        copy_field(bytes, offset, &value.to_le_bytes()).expect("test field should exist");
    }

    #[test]
    fn image_header_and_binding_match_native_v1_layout() {
        let guest_range = range(0x20000, TEST_ALIGNMENT);
        let mut guest_memory = memory(vec![guest_range]);
        let expected = fill_memory(&mut guest_memory);
        let (image, binding) = write_image(&guest_memory);
        let (repeated_image, repeated_binding) = write_image(&guest_memory);
        assert_eq!(repeated_image, image);
        assert_eq!(repeated_binding, binding);

        let expected_header = [
            0x42, 0x41, 0x4e, 0x47, 0x4d, 0x45, 0x4d, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x22, 0x33,
            0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(
            image.get(..SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES),
            Some(expected_header.as_slice())
        );
        assert_eq!(binding.version(), NATIVE_V1_SNAPSHOT_VERSION);
        assert_eq!(binding.architecture(), SnapshotArchitecture::Arm64);
        assert_eq!(binding.guest_page_size(), NATIVE_V1_GUEST_PAGE_SIZE);
        assert_eq!(binding.image_id(), TEST_IMAGE_ID);
        assert_eq!(binding.data_length(), TEST_ALIGNMENT);
        assert_eq!(
            binding.file_length(),
            u64::try_from(image.len()).expect("image size should fit u64")
        );
        assert_eq!(binding.integrity(), SnapshotIntegrity::Crc64Jones);
        assert_eq!(binding.ranges().len(), 1);
        assert_eq!(binding.ranges()[0].range(), guest_range);
        assert_eq!(
            binding.ranges()[0].file_offset(),
            u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES).expect("header size should fit u64")
        );
        assert_eq!(
            read_u64(
                &image,
                SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES + expected[0].1.len()
            ),
            binding.checksum()
        );

        let encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");
        let expected_binding = [
            0x42, 0x41, 0x4e, 0x47, 0x4d, 0x42, 0x4e, 0x44, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x22, 0x33,
            0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x38, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x43, 0xfd, 0x6e, 0x69, 0x33, 0x16, 0xb7, 0xaf, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(binding.checksum(), 0xafb7_1633_696e_fd43);
        assert_eq!(encoded, expected_binding);
        assert_eq!(
            image.get(image.len() - SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES..),
            Some([0x43, 0xfd, 0x6e, 0x69, 0x33, 0x16, 0xb7, 0xaf].as_slice())
        );
        assert_eq!(encoded.len(), SNAPSHOT_MEMORY_BINDING_HEADER_BYTES + 24);
        assert_eq!(encoded.get(..8), Some(b"BANGMBND".as_slice()));
        assert_eq!(
            read_u64(&encoded, BINDING_DATA_LENGTH_OFFSET),
            TEST_ALIGNMENT
        );
        assert_eq!(
            read_u64(&encoded, BINDING_FILE_LENGTH_OFFSET),
            binding.file_length()
        );
        assert_eq!(
            read_u64(&encoded, BINDING_CHECKSUM_OFFSET),
            binding.checksum()
        );
        assert_eq!(
            read_u64(&encoded, SNAPSHOT_MEMORY_BINDING_HEADER_BYTES),
            0x20000
        );
        assert_eq!(
            read_u64(&encoded, SNAPSHOT_MEMORY_BINDING_HEADER_BYTES + 8),
            TEST_ALIGNMENT
        );
        assert_eq!(
            read_u64(&encoded, SNAPSHOT_MEMORY_BINDING_HEADER_BYTES + 16),
            48
        );
        assert_eq!(
            decode_snapshot_memory_binding(&encoded).expect("binding should decode"),
            binding
        );
        assert_eq!(
            encode_snapshot_memory_binding(
                &decode_snapshot_memory_binding(&encoded).expect("binding should decode")
            )
            .expect("binding should re-encode"),
            encoded
        );
    }

    #[test]
    fn cooperative_write_cancellation_covers_every_bounded_checkpoint() {
        let guest_range = range(
            0x20_0000,
            u64::try_from(SNAPSHOT_MEMORY_IO_CHUNK_BYTES + 16 * 1024)
                .expect("test range size should fit u64"),
        );
        let mut guest_memory = memory(vec![guest_range]);
        fill_memory(&mut guest_memory);
        let expected_stages = [
            SnapshotMemoryIoStage::InitialPosition,
            SnapshotMemoryIoStage::Header,
            SnapshotMemoryIoStage::Data { range_index: 0 },
            SnapshotMemoryIoStage::Data { range_index: 0 },
            SnapshotMemoryIoStage::Trailer,
            SnapshotMemoryIoStage::FinalEndLength,
        ];

        for (cancel_index, expected_stage) in expected_stages.into_iter().enumerate() {
            let mut writer = Cursor::new(Vec::new());
            let mut checkpoint_index = 0;
            let error = write_snapshot_memory_image_with_id_and_cancel(
                &guest_memory,
                &mut writer,
                TEST_IMAGE_ID,
                |stage| {
                    assert_eq!(stage, expected_stages[checkpoint_index]);
                    let cancel = checkpoint_index == cancel_index;
                    checkpoint_index += 1;
                    cancel
                },
            )
            .expect_err("selected checkpoint should cancel");

            assert!(matches!(
                error,
                SnapshotMemoryWriteError::Cancelled { stage } if stage == expected_stage
            ));
            assert_eq!(checkpoint_index, cancel_index + 1);
        }

        let mut fresh = Cursor::new(Vec::new());
        let mut observed = Vec::new();
        let binding = write_snapshot_memory_image_with_id_and_cancel(
            &guest_memory,
            &mut fresh,
            TEST_IMAGE_ID,
            |stage| {
                observed.push(stage);
                false
            },
        )
        .expect("fresh complete write should succeed");
        assert_eq!(observed, expected_stages);
        assert_eq!(
            u64::try_from(fresh.into_inner().len()).expect("image length should fit u64"),
            binding.file_length()
        );
    }

    #[test]
    fn public_writer_generates_an_identity_bound_to_the_header() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let expected = fill_memory(&mut guest_memory);
        let mut writer = Cursor::new(Vec::new());
        let binding = write_snapshot_memory_image(&guest_memory, &mut writer)
            .expect("public writer should generate an image identity");
        let image = writer.into_inner();
        assert_eq!(
            array_at::<SNAPSHOT_MEMORY_IMAGE_ID_BYTES>(&image, IMAGE_ID_OFFSET)
                .expect("image identity should exist"),
            *binding.image_id().as_bytes()
        );
        let loaded = load_snapshot_memory_image(&binding, &mut Cursor::new(image))
            .expect("publicly written image should load");
        assert_memory_bytes(&loaded, &expected);
    }

    #[test]
    fn round_trips_discontiguous_adjacent_and_dynamic_regions() {
        let cases = [
            vec![
                range(0, TEST_ALIGNMENT),
                range(TEST_ALIGNMENT * 4, TEST_ALIGNMENT * 2),
            ],
            vec![
                range(0, TEST_ALIGNMENT),
                range(TEST_ALIGNMENT, TEST_ALIGNMENT),
            ],
        ];

        for ranges in cases {
            let mut guest_memory = memory(ranges);
            let expected = fill_memory(&mut guest_memory);
            let (image, binding) = write_image(&guest_memory);
            let mut reader = Cursor::new(image);
            let loaded = load_snapshot_memory_image(&binding, &mut reader)
                .expect("memory image should load");
            assert_memory_bytes(&loaded, &expected);
        }

        let mut guest_memory = memory(vec![range(TEST_ALIGNMENT * 2, TEST_ALIGNMENT)]);
        guest_memory
            .insert_region(range(0, TEST_ALIGNMENT))
            .expect("dynamic range before should insert");
        guest_memory
            .insert_region(range(TEST_ALIGNMENT * 5, TEST_ALIGNMENT))
            .expect("dynamic range after should insert");
        let expected = fill_memory(&mut guest_memory);
        let (image, binding) = write_image(&guest_memory);
        assert_eq!(
            binding
                .ranges()
                .iter()
                .map(|binding| binding.range())
                .collect::<Vec<_>>(),
            expected.iter().map(|(range, _)| *range).collect::<Vec<_>>()
        );
        let loaded = load_snapshot_memory_image(&binding, &mut Cursor::new(image))
            .expect("dynamic memory image should load");
        assert_memory_bytes(&loaded, &expected);
    }

    #[test]
    fn round_trips_across_chunk_boundaries() {
        for size in [
            SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64 - TEST_ALIGNMENT,
            SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64,
            SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64 + TEST_ALIGNMENT,
        ] {
            let guest_range = range(0, size);
            let mut guest_memory = memory(vec![guest_range]);
            let expected = fill_memory(&mut guest_memory);
            let (image, binding) = write_image(&guest_memory);
            let loaded = load_snapshot_memory_image(&binding, &mut Cursor::new(image))
                .expect("chunked memory image should load");
            assert_memory_bytes(&loaded, &expected);
        }
    }

    #[test]
    fn binding_decoder_rejects_fixed_prefix_truncation_and_trailing_data() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        fill_memory(&mut guest_memory);
        let (_, binding) = write_image(&guest_memory);
        let encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");

        for actual in 0..SNAPSHOT_MEMORY_BINDING_HEADER_BYTES {
            assert!(matches!(
                decode_snapshot_memory_binding(&encoded[..actual]),
                Err(SnapshotMemoryBindingError::Truncated {
                    expected: SNAPSHOT_MEMORY_BINDING_HEADER_BYTES,
                    actual: found,
                }) if found == actual
            ));
        }
        for actual in SNAPSHOT_MEMORY_BINDING_HEADER_BYTES..encoded.len() {
            assert!(matches!(
                decode_snapshot_memory_binding(&encoded[..actual]),
                Err(SnapshotMemoryBindingError::Truncated {
                    expected,
                    actual: found,
                }) if expected == encoded.len() && found == actual
            ));
        }
        let mut trailing = encoded.clone();
        trailing.push(0xa5);
        assert!(matches!(
            decode_snapshot_memory_binding(&trailing),
            Err(SnapshotMemoryBindingError::TrailingData { expected, actual })
                if expected == encoded.len() && actual == trailing.len()
        ));
    }

    #[test]
    fn binding_decoder_rejects_compatibility_and_reserved_fields() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        fill_memory(&mut guest_memory);
        let (_, binding) = write_image(&guest_memory);
        let encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");

        let mut invalid_magic = encoded.clone();
        invalid_magic[0] ^= 0xff;
        assert!(matches!(
            decode_snapshot_memory_binding(&invalid_magic),
            Err(SnapshotMemoryBindingError::InvalidMagic)
        ));

        let mut version = encoded.clone();
        set_u16(&mut version, BINDING_VERSION_MAJOR_OFFSET, 2);
        assert!(matches!(
            decode_snapshot_memory_binding(&version),
            Err(SnapshotMemoryBindingError::UnsupportedVersion(value)) if value.major() == 2
        ));

        let mut architecture = encoded.clone();
        set_u16(&mut architecture, BINDING_ARCHITECTURE_OFFSET, 9);
        assert!(matches!(
            decode_snapshot_memory_binding(&architecture),
            Err(SnapshotMemoryBindingError::IncompatibleArchitecture(9))
        ));

        let mut page_size = encoded.clone();
        set_u32(&mut page_size, BINDING_GUEST_PAGE_SIZE_OFFSET, 8192);
        assert!(matches!(
            decode_snapshot_memory_binding(&page_size),
            Err(SnapshotMemoryBindingError::IncompatibleGuestPageSize(8192))
        ));

        let mut flags = encoded.clone();
        set_u32(&mut flags, BINDING_RESERVED_FLAGS_OFFSET, 1);
        assert!(matches!(
            decode_snapshot_memory_binding(&flags),
            Err(SnapshotMemoryBindingError::UnsupportedFlags(1))
        ));

        let mut reserved = encoded;
        set_u32(&mut reserved, BINDING_RESERVED_OFFSET, 1);
        assert!(matches!(
            decode_snapshot_memory_binding(&reserved),
            Err(SnapshotMemoryBindingError::UnsupportedReserved(1))
        ));
    }

    #[test]
    fn binding_decoder_rejects_range_count_and_length_policies() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        fill_memory(&mut guest_memory);
        let (_, binding) = write_image(&guest_memory);
        let encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");

        for count in [0, NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES + 1] {
            let mut bytes = encoded.clone();
            set_u32(
                &mut bytes,
                BINDING_RANGE_COUNT_OFFSET,
                u32::try_from(count).expect("test count should fit u32"),
            );
            assert!(matches!(
                decode_snapshot_memory_binding(&bytes),
                Err(SnapshotMemoryBindingError::RangeCountOutOfBounds {
                    count: found,
                    maximum: NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES,
                }) if found == count
            ));
        }

        let mut exact_maximum = encoded.clone();
        set_u64(
            &mut exact_maximum,
            BINDING_DATA_LENGTH_OFFSET,
            NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES,
        );
        set_u64(
            &mut exact_maximum,
            BINDING_FILE_LENGTH_OFFSET,
            memory_file_length(NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES)
                .expect("maximum data file length should fit"),
        );
        set_u64(
            &mut exact_maximum,
            SNAPSHOT_MEMORY_BINDING_HEADER_BYTES + BINDING_RANGE_SIZE_OFFSET,
            NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES,
        );
        let maximum = decode_snapshot_memory_binding(&exact_maximum)
            .expect("exact maximum data length should decode without allocating guest memory");
        assert_eq!(
            maximum.data_length(),
            NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES
        );

        let mut too_large = encoded.clone();
        set_u64(
            &mut too_large,
            BINDING_DATA_LENGTH_OFFSET,
            NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES + 1,
        );
        assert!(matches!(
            decode_snapshot_memory_binding(&too_large),
            Err(SnapshotMemoryBindingError::DataTooLarge { .. })
        ));

        let mut file_length = encoded.clone();
        set_u64(
            &mut file_length,
            BINDING_FILE_LENGTH_OFFSET,
            binding.file_length() + 1,
        );
        assert!(matches!(
            decode_snapshot_memory_binding(&file_length),
            Err(SnapshotMemoryBindingError::FileLengthMismatch { .. })
        ));

        let mut data_length = encoded;
        set_u64(
            &mut data_length,
            BINDING_DATA_LENGTH_OFFSET,
            binding.data_length() + TEST_ALIGNMENT,
        );
        set_u64(
            &mut data_length,
            BINDING_FILE_LENGTH_OFFSET,
            binding.file_length() + TEST_ALIGNMENT,
        );
        assert!(matches!(
            decode_snapshot_memory_binding(&data_length),
            Err(SnapshotMemoryBindingError::DataLengthMismatch { .. })
        ));
        assert!(matches!(
            memory_file_length(u64::MAX),
            Err(SnapshotMemoryBindingError::LengthOverflow)
        ));
    }

    #[test]
    fn binding_decoder_rejects_invalid_ranges_and_offsets() {
        let mut guest_memory = memory(vec![
            range(0, TEST_ALIGNMENT),
            range(TEST_ALIGNMENT * 2, TEST_ALIGNMENT),
        ]);
        fill_memory(&mut guest_memory);
        let (_, binding) = write_image(&guest_memory);
        let encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");
        let first = SNAPSHOT_MEMORY_BINDING_HEADER_BYTES;
        let second = first + SNAPSHOT_MEMORY_BINDING_RANGE_BYTES;

        let mut zero_size = encoded.clone();
        set_u64(&mut zero_size, first + BINDING_RANGE_SIZE_OFFSET, 0);
        assert!(matches!(
            decode_snapshot_memory_binding(&zero_size),
            Err(SnapshotMemoryBindingError::InvalidRange {
                index: 0,
                source: GuestMemoryError::EmptyRange { .. }
            })
        ));

        let mut unaligned = encoded.clone();
        set_u64(&mut unaligned, first + BINDING_RANGE_START_OFFSET, 1);
        assert!(matches!(
            decode_snapshot_memory_binding(&unaligned),
            Err(SnapshotMemoryBindingError::InvalidRange {
                index: 0,
                source: GuestMemoryError::UnalignedRange { .. }
            })
        ));

        let mut unordered = encoded.clone();
        set_u64(
            &mut unordered,
            first + BINDING_RANGE_START_OFFSET,
            TEST_ALIGNMENT * 4,
        );
        assert!(matches!(
            decode_snapshot_memory_binding(&unordered),
            Err(SnapshotMemoryBindingError::InvalidRange {
                index: 1,
                source: GuestMemoryError::UnorderedRange { .. }
            })
        ));

        let mut overlap = encoded.clone();
        set_u64(&mut overlap, second + BINDING_RANGE_START_OFFSET, 0);
        assert!(matches!(
            decode_snapshot_memory_binding(&overlap),
            Err(SnapshotMemoryBindingError::InvalidRange {
                index: 1,
                source: GuestMemoryError::OverlappingRange { .. }
            })
        ));

        for file_offset in [
            binding.ranges()[1].file_offset() - 1,
            binding.ranges()[1].file_offset() + 1,
        ] {
            let mut offset = encoded.clone();
            set_u64(&mut offset, second + BINDING_RANGE_FILE_OFFSET, file_offset);
            assert!(matches!(
                decode_snapshot_memory_binding(&offset),
                Err(SnapshotMemoryBindingError::NonCanonicalFileOffset { index: 1, .. })
            ));
        }
    }

    #[test]
    fn loader_rejects_header_binding_and_integrity_mismatches() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        fill_memory(&mut guest_memory);
        let (image, binding) = write_image(&guest_memory);

        let mut invalid_magic = image.clone();
        invalid_magic[0] ^= 0xff;
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(invalid_magic)),
            Err(SnapshotMemoryLoadError::InvalidMagic)
        ));

        let mut version = image.clone();
        set_u16(&mut version, IMAGE_VERSION_MAJOR_OFFSET, 2);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(version)),
            Err(SnapshotMemoryLoadError::UnsupportedVersion(value)) if value.major() == 2
        ));

        let mut architecture = image.clone();
        set_u16(&mut architecture, IMAGE_ARCHITECTURE_OFFSET, 9);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(architecture)),
            Err(SnapshotMemoryLoadError::IncompatibleArchitecture(9))
        ));

        let mut page_size = image.clone();
        set_u32(&mut page_size, IMAGE_GUEST_PAGE_SIZE_OFFSET, 8192);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(page_size)),
            Err(SnapshotMemoryLoadError::IncompatibleGuestPageSize(8192))
        ));

        let mut flags = image.clone();
        set_u32(&mut flags, IMAGE_RESERVED_FLAGS_OFFSET, 1);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(flags)),
            Err(SnapshotMemoryLoadError::UnsupportedFlags(1))
        ));

        let mut identity = image.clone();
        identity[IMAGE_ID_OFFSET] ^= 0xff;
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(identity)),
            Err(SnapshotMemoryLoadError::ImageIdMismatch)
        ));

        let mut length = image.clone();
        set_u64(
            &mut length,
            IMAGE_DATA_LENGTH_OFFSET,
            binding.data_length() + TEST_ALIGNMENT,
        );
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(length)),
            Err(SnapshotMemoryLoadError::DataLengthMismatch { .. })
        ));

        let mut corrupt_data = image.clone();
        corrupt_data[SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES] ^= 0xff;
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(corrupt_data)),
            Err(SnapshotMemoryLoadError::IntegrityMismatch)
        ));

        let mut corrupt_trailer = image.clone();
        let trailer_index = corrupt_trailer.len() - 1;
        corrupt_trailer[trailer_index] ^= 0xff;
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut Cursor::new(corrupt_trailer)),
            Err(SnapshotMemoryLoadError::IntegrityMismatch)
        ));

        let mut wrong_binding = binding.clone();
        wrong_binding.checksum ^= 1;
        assert!(matches!(
            load_snapshot_memory_image(&wrong_binding, &mut Cursor::new(image)),
            Err(SnapshotMemoryLoadError::BindingChecksumMismatch)
        ));
    }

    #[test]
    fn seek_preflights_preserve_zero_position_on_length_rejection() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        fill_memory(&mut guest_memory);
        let (image, binding) = write_image(&guest_memory);

        let original = vec![0xa5; 17];
        let mut nonempty = Cursor::new(original.clone());
        assert!(matches!(
            write_snapshot_memory_image_with_id(&guest_memory, &mut nonempty, TEST_IMAGE_ID),
            Err(SnapshotMemoryWriteError::NonEmptyOutput { length: 17 })
        ));
        assert_eq!(nonempty.position(), 0);
        assert_eq!(nonempty.into_inner(), original);

        let mut short = Cursor::new(image[..image.len() - 1].to_vec());
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut short),
            Err(SnapshotMemoryLoadError::InputLengthMismatch { .. })
        ));
        assert_eq!(short.position(), 0);

        let mut long_bytes = image;
        long_bytes.push(0);
        let mut long = Cursor::new(long_bytes);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut long),
            Err(SnapshotMemoryLoadError::InputLengthMismatch { .. })
        ));
        assert_eq!(long.position(), 0);
    }

    #[test]
    fn seek_preflights_reject_nonzero_initial_positions() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        fill_memory(&mut guest_memory);
        let (image, binding) = write_image(&guest_memory);

        let mut writer = Cursor::new(Vec::new());
        writer.set_position(7);
        assert!(matches!(
            write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID),
            Err(SnapshotMemoryWriteError::InvalidInitialPosition { actual: 7 })
        ));
        assert_eq!(writer.position(), 7);

        let mut reader = Cursor::new(image);
        reader.set_position(9);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut reader),
            Err(SnapshotMemoryLoadError::InvalidInitialPosition { actual: 9 })
        ));
        assert_eq!(reader.position(), 9);
    }

    struct ShortIo {
        inner: Cursor<Vec<u8>>,
        maximum: usize,
        read_interruptions_remaining: usize,
        write_interruptions_remaining: usize,
    }

    impl ShortIo {
        fn empty(maximum: usize) -> Self {
            Self {
                inner: Cursor::new(Vec::new()),
                maximum,
                read_interruptions_remaining: 0,
                write_interruptions_remaining: 3,
            }
        }

        fn with_bytes(bytes: Vec<u8>, maximum: usize) -> Self {
            Self {
                inner: Cursor::new(bytes),
                maximum,
                read_interruptions_remaining: 3,
                write_interruptions_remaining: 0,
            }
        }
    }

    impl Read for ShortIo {
        fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
            if self.read_interruptions_remaining > 0 {
                self.read_interruptions_remaining -= 1;
                return Err(Error::from(ErrorKind::Interrupted));
            }
            let length = destination.len().min(self.maximum);
            self.inner.read(&mut destination[..length])
        }
    }

    impl Write for ShortIo {
        fn write(&mut self, source: &[u8]) -> io::Result<usize> {
            if self.write_interruptions_remaining > 0 {
                self.write_interruptions_remaining -= 1;
                return Err(Error::from(ErrorKind::Interrupted));
            }
            let length = source.len().min(self.maximum);
            self.inner.write(&source[..length])
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for ShortIo {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn short_and_interrupted_io_round_trips() {
        let mut guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let expected = fill_memory(&mut guest_memory);
        let mut writer = ShortIo::empty(13);
        let binding =
            write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID)
                .expect("short writes should complete");
        let image = writer.inner.into_inner();

        let mut reader = ShortIo::with_bytes(image, 11);
        let loaded =
            load_snapshot_memory_image(&binding, &mut reader).expect("short reads should complete");
        assert_memory_bytes(&loaded, &expected);
    }

    struct ZeroWriter {
        cursor: Cursor<Vec<u8>>,
    }

    impl Write for ZeroWriter {
        fn write(&mut self, _source: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for ZeroWriter {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    #[test]
    fn zero_progress_write_is_typed_and_redacted() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let mut writer = ZeroWriter {
            cursor: Cursor::new(Vec::new()),
        };
        let error = write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID)
            .expect_err("zero progress should fail");
        assert!(matches!(
            error,
            SnapshotMemoryWriteError::Io {
                stage: SnapshotMemoryIoStage::Header,
                kind: ErrorKind::WriteZero,
            }
        ));
        assert!(!error.to_string().contains("00112233"));
    }

    #[test]
    fn loader_propagates_guest_allocation_failure() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (image, binding) = write_image(&guest_memory);
        let error =
            load_snapshot_memory_image_with_allocator(&binding, &mut Cursor::new(image), |_| {
                Err(GuestMemoryAllocationError::InvalidLayout(
                    GuestMemoryError::EmptyLayout,
                ))
            })
            .expect_err("injected guest allocation failure should propagate");
        assert!(matches!(
            error,
            SnapshotMemoryLoadError::GuestMemoryAllocation {
                source: GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::EmptyLayout)
            }
        ));
    }

    #[test]
    fn writer_propagates_guest_memory_access_failure() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (ranges, data_length, file_length) =
            ranges_from_memory(&guest_memory).expect("test ranges should bind");
        let access_error = GuestMemoryAccessError::UnmappedRange {
            range: range(0, TEST_ALIGNMENT),
        };
        let mut writer = Cursor::new(Vec::new());
        let error = write_snapshot_memory_image_from_parts(
            &mut writer,
            TEST_IMAGE_ID,
            ranges,
            data_length,
            file_length,
            |_, _| Err(access_error),
        )
        .expect_err("injected guest-memory read should fail");
        assert!(matches!(
            error,
            SnapshotMemoryWriteError::GuestMemoryRead {
                range_index: 0,
                source,
            } if source == access_error
        ));
        assert_eq!(
            writer.into_inner().len(),
            SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES
        );
    }

    #[derive(Debug)]
    struct DropProbe {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn loader_drops_partial_memory_after_guest_write_failure() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (image, binding) = write_image(&guest_memory);
        let drops = Arc::new(AtomicUsize::new(0));
        let allocated_drops = Arc::clone(&drops);
        let access_error = GuestMemoryAccessError::UnmappedRange {
            range: range(0, TEST_ALIGNMENT),
        };
        let error = load_snapshot_memory_image_with(
            &binding,
            &mut Cursor::new(image),
            move |_| {
                Ok(DropProbe {
                    drops: allocated_drops,
                })
            },
            |_, _, _| Err(access_error),
        )
        .expect_err("injected guest-memory write should fail");
        assert!(matches!(
            error,
            SnapshotMemoryLoadError::GuestMemoryWrite {
                range_index: 0,
                source,
            } if source == access_error
        ));
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn binding_and_error_debug_redact_identity_checksum_and_io_text() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (_, binding) = write_image(&guest_memory);
        let debug = format!("{binding:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("00112233"));
        assert!(!debug.contains(&binding.checksum().to_string()));
        assert_eq!(format!("{:?}", binding.image_id()), "<redacted>");

        let error = SnapshotMemoryLoadError::Io {
            stage: SnapshotMemoryIoStage::Header,
            kind: Error::other("host-path-sentinel").kind(),
        };
        assert!(!error.to_string().contains("host-path-sentinel"));
        assert!(!format!("{error:?}").contains("host-path-sentinel"));
    }

    #[test]
    fn maximum_binding_metadata_fits_outer_payload_policy() {
        assert_eq!(NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES, 98_376);
        assert_eq!(SNAPSHOT_MEMORY_IO_CHUNK_BYTES, 1024 * 1024);
        assert_eq!(NATIVE_V1_SNAPSHOT_MEMORY_MAX_DATA_BYTES, 0x00ff_8000_0000);
    }

    #[test]
    fn binding_decoder_accepts_exact_maximum_range_count() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (_, binding) = write_image(&guest_memory);
        let mut encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");
        encoded.resize(NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES, 0);
        set_u32(
            &mut encoded,
            BINDING_RANGE_COUNT_OFFSET,
            u32::try_from(NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES)
                .expect("maximum range count should fit u32"),
        );
        let range_size = u64::from(NATIVE_V1_GUEST_PAGE_SIZE);
        let data_length = u64::try_from(NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES)
            .expect("maximum range count should fit u64")
            * range_size;
        set_u64(&mut encoded, BINDING_DATA_LENGTH_OFFSET, data_length);
        set_u64(
            &mut encoded,
            BINDING_FILE_LENGTH_OFFSET,
            memory_file_length(data_length).expect("maximum binding file length should fit"),
        );
        for index in 0..NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES {
            let index_u64 = u64::try_from(index).expect("range index should fit u64");
            let entry =
                SNAPSHOT_MEMORY_BINDING_HEADER_BYTES + index * SNAPSHOT_MEMORY_BINDING_RANGE_BYTES;
            set_u64(
                &mut encoded,
                entry + BINDING_RANGE_START_OFFSET,
                index_u64 * range_size,
            );
            set_u64(&mut encoded, entry + BINDING_RANGE_SIZE_OFFSET, range_size);
            set_u64(
                &mut encoded,
                entry + BINDING_RANGE_FILE_OFFSET,
                u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
                    .expect("header size should fit u64")
                    + index_u64 * range_size,
            );
        }

        let decoded =
            decode_snapshot_memory_binding(&encoded).expect("maximum binding should decode");
        assert_eq!(decoded.ranges().len(), NATIVE_V1_SNAPSHOT_MEMORY_MAX_RANGES);
        assert_eq!(decoded.data_length(), data_length);
        assert_eq!(
            encode_snapshot_memory_binding(&decoded).expect("maximum binding should re-encode"),
            encoded
        );
    }

    #[test]
    fn binding_decoder_rejects_address_overflow_and_unaligned_size() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (_, binding) = write_image(&guest_memory);
        let encoded = encode_snapshot_memory_binding(&binding).expect("binding should encode");
        let entry = SNAPSHOT_MEMORY_BINDING_HEADER_BYTES;

        let mut overflow = encoded.clone();
        set_u64(
            &mut overflow,
            entry + BINDING_RANGE_START_OFFSET,
            u64::MAX - (u64::from(NATIVE_V1_GUEST_PAGE_SIZE) - 1),
        );
        set_u64(
            &mut overflow,
            entry + BINDING_RANGE_SIZE_OFFSET,
            u64::from(NATIVE_V1_GUEST_PAGE_SIZE),
        );
        assert!(matches!(
            decode_snapshot_memory_binding(&overflow),
            Err(SnapshotMemoryBindingError::InvalidRange {
                index: 0,
                source: GuestMemoryError::AddressOverflow { .. }
            })
        ));

        let mut unaligned = encoded;
        set_u64(
            &mut unaligned,
            entry + BINDING_RANGE_SIZE_OFFSET,
            TEST_ALIGNMENT + 1,
        );
        assert!(matches!(
            decode_snapshot_memory_binding(&unaligned),
            Err(SnapshotMemoryBindingError::InvalidRange {
                index: 0,
                source: GuestMemoryError::UnalignedRange { .. }
            })
        ));
    }

    #[test]
    fn local_resource_failures_are_typed_before_io() {
        assert!(matches!(
            generate_image_id_with(|_| Err::<(), ()>(())),
            Err(SnapshotMemoryWriteError::IdentityUnavailable)
        ));

        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (ranges, data_length, file_length) =
            ranges_from_memory(&guest_memory).expect("test ranges should bind");
        let write_allocation_error =
            allocate_io_buffer_with_size(usize::MAX).expect_err("oversized buffer should fail");
        let mut writer = Cursor::new(Vec::new());
        let write_error = write_snapshot_memory_image_from_parts_with_buffer(
            &mut writer,
            TEST_IMAGE_ID,
            ranges,
            data_length,
            file_length,
            move || Err(write_allocation_error),
            |_, _| panic!("guest memory must not be read after buffer allocation failure"),
        )
        .expect_err("writer buffer allocation failure should propagate");
        assert!(matches!(
            write_error,
            SnapshotMemoryWriteError::ChunkAllocationFailed { .. }
        ));
        assert_eq!(writer.position(), 0);
        assert!(writer.into_inner().is_empty());

        let (image, binding) = write_image(&guest_memory);
        let load_allocation_error =
            allocate_io_buffer_with_size(usize::MAX).expect_err("oversized buffer should fail");
        let mut reader = Cursor::new(image);
        let load_error = load_snapshot_memory_image_with_buffer(
            &binding,
            &mut reader,
            move || Err(load_allocation_error),
            |_| -> Result<(), GuestMemoryAllocationError> {
                panic!("guest memory must not allocate after buffer allocation failure")
            },
            |_: &mut (), _: &[u8], _: GuestAddress| -> Result<(), GuestMemoryAccessError> {
                panic!("guest memory must not be written after buffer allocation failure")
            },
        )
        .expect_err("loader buffer allocation failure should propagate");
        assert!(matches!(
            load_error,
            SnapshotMemoryLoadError::ChunkAllocationFailed { .. }
        ));
        assert_eq!(reader.position(), 0);
    }

    struct FailingIo {
        inner: Cursor<Vec<u8>>,
        fail_read_at: Option<u64>,
        fail_write_at: Option<u64>,
    }

    impl FailingIo {
        fn reader(bytes: Vec<u8>, fail_at: u64) -> Self {
            Self {
                inner: Cursor::new(bytes),
                fail_read_at: Some(fail_at),
                fail_write_at: None,
            }
        }

        fn writer(fail_at: u64) -> Self {
            Self {
                inner: Cursor::new(Vec::new()),
                fail_read_at: None,
                fail_write_at: Some(fail_at),
            }
        }
    }

    impl Read for FailingIo {
        fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
            let Some(fail_at) = self.fail_read_at else {
                return self.inner.read(destination);
            };
            let position = self.inner.position();
            if position >= fail_at {
                return Err(Error::other("host-path-read-sentinel"));
            }
            let remaining = usize::try_from(fail_at - position).unwrap_or(usize::MAX);
            let length = destination.len().min(remaining);
            self.inner.read(&mut destination[..length])
        }
    }

    impl Write for FailingIo {
        fn write(&mut self, source: &[u8]) -> io::Result<usize> {
            let Some(fail_at) = self.fail_write_at else {
                return self.inner.write(source);
            };
            let position = self.inner.position();
            if position >= fail_at {
                return Err(Error::other("host-path-write-sentinel"));
            }
            let remaining = usize::try_from(fail_at - position).unwrap_or(usize::MAX);
            let length = source.len().min(remaining);
            self.inner.write(&source[..length])
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for FailingIo {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn writer_reports_exact_failing_io_stage_without_source_text() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let cases = [
            (0, SnapshotMemoryIoStage::Header),
            (
                u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
                    .expect("header size should fit u64")
                    + 7,
                SnapshotMemoryIoStage::Data { range_index: 0 },
            ),
            (
                u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
                    .expect("header size should fit u64")
                    + TEST_ALIGNMENT,
                SnapshotMemoryIoStage::Trailer,
            ),
        ];
        for (fail_at, expected_stage) in cases {
            let mut writer = FailingIo::writer(fail_at);
            let error =
                write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID)
                    .expect_err("injected output failure should propagate");
            assert!(matches!(
                error,
                SnapshotMemoryWriteError::Io {
                    stage,
                    kind: ErrorKind::Other,
                } if stage == expected_stage
            ));
            assert!(!error.to_string().contains("host-path-write-sentinel"));
            assert!(!format!("{error:?}").contains("host-path-write-sentinel"));
        }
    }

    #[test]
    fn io_failures_identify_later_ranges_and_chunks() {
        let guest_memory = memory(vec![
            range(0, SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64 + TEST_ALIGNMENT),
            range(
                SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64 + TEST_ALIGNMENT * 4,
                TEST_ALIGNMENT,
            ),
        ]);
        let header =
            u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES).expect("header size should fit u64");
        let second_chunk_failure = header + SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64 + 7;
        let second_range_failure = header + SNAPSHOT_MEMORY_IO_CHUNK_BYTES_U64 + TEST_ALIGNMENT + 7;

        let mut second_chunk_writer = FailingIo::writer(second_chunk_failure);
        assert!(matches!(
            write_snapshot_memory_image_with_id(
                &guest_memory,
                &mut second_chunk_writer,
                TEST_IMAGE_ID,
            ),
            Err(SnapshotMemoryWriteError::Io {
                stage: SnapshotMemoryIoStage::Data { range_index: 0 },
                kind: ErrorKind::Other,
            })
        ));

        let mut second_range_writer = FailingIo::writer(second_range_failure);
        assert!(matches!(
            write_snapshot_memory_image_with_id(
                &guest_memory,
                &mut second_range_writer,
                TEST_IMAGE_ID,
            ),
            Err(SnapshotMemoryWriteError::Io {
                stage: SnapshotMemoryIoStage::Data { range_index: 1 },
                kind: ErrorKind::Other,
            })
        ));

        let (image, binding) = write_image(&guest_memory);
        let mut second_chunk_reader = FailingIo::reader(image.clone(), second_chunk_failure);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut second_chunk_reader),
            Err(SnapshotMemoryLoadError::Io {
                stage: SnapshotMemoryIoStage::Data { range_index: 0 },
                kind: ErrorKind::Other,
            })
        ));
        let mut second_range_reader = FailingIo::reader(image, second_range_failure);
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut second_range_reader),
            Err(SnapshotMemoryLoadError::Io {
                stage: SnapshotMemoryIoStage::Data { range_index: 1 },
                kind: ErrorKind::Other,
            })
        ));
    }

    #[test]
    fn loader_reports_exact_failing_io_stage_without_source_text() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (image, binding) = write_image(&guest_memory);
        let header =
            u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES).expect("header size should fit u64");
        let cases = [
            (0, SnapshotMemoryIoStage::Header),
            (header + 7, SnapshotMemoryIoStage::Data { range_index: 0 }),
            (header + TEST_ALIGNMENT, SnapshotMemoryIoStage::Trailer),
            (binding.file_length(), SnapshotMemoryIoStage::TrailingProbe),
        ];
        for (fail_at, expected_stage) in cases {
            let mut reader = FailingIo::reader(image.clone(), fail_at);
            let error = load_snapshot_memory_image(&binding, &mut reader)
                .expect_err("injected input failure should propagate");
            assert!(matches!(
                error,
                SnapshotMemoryLoadError::Io {
                    stage,
                    kind: ErrorKind::Other,
                } if stage == expected_stage
            ));
            assert!(!error.to_string().contains("host-path-read-sentinel"));
            assert!(!format!("{error:?}").contains("host-path-read-sentinel"));
        }
    }

    struct SeekFailIo {
        inner: Cursor<Vec<u8>>,
        fail_on_call: usize,
        calls: usize,
    }

    impl SeekFailIo {
        fn writer(fail_on_call: usize) -> Self {
            Self {
                inner: Cursor::new(Vec::new()),
                fail_on_call,
                calls: 0,
            }
        }

        fn reader(bytes: Vec<u8>, fail_on_call: usize) -> Self {
            Self {
                inner: Cursor::new(bytes),
                fail_on_call,
                calls: 0,
            }
        }
    }

    impl Read for SeekFailIo {
        fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
            self.inner.read(destination)
        }
    }

    impl Write for SeekFailIo {
        fn write(&mut self, source: &[u8]) -> io::Result<usize> {
            self.inner.write(source)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for SeekFailIo {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.calls += 1;
            if self.calls == self.fail_on_call {
                Err(Error::other("host-path-seek-sentinel"))
            } else {
                self.inner.seek(position)
            }
        }
    }

    #[test]
    fn seek_failures_report_every_preflight_and_final_stage() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (image, binding) = write_image(&guest_memory);

        for (fail_on_call, expected_stage) in [
            (1, SnapshotMemoryIoStage::InitialPosition),
            (2, SnapshotMemoryIoStage::EndLength),
            (4, SnapshotMemoryIoStage::FinalPosition),
            (5, SnapshotMemoryIoStage::FinalEndLength),
        ] {
            let mut writer = SeekFailIo::writer(fail_on_call);
            let error =
                write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID)
                    .expect_err("injected writer seek failure should propagate");
            assert!(matches!(
                error,
                SnapshotMemoryWriteError::Io {
                    stage,
                    kind: ErrorKind::Other,
                } if stage == expected_stage
            ));
            assert!(!error.to_string().contains("host-path-seek-sentinel"));
        }

        for (fail_on_call, expected_stage) in [
            (1, SnapshotMemoryIoStage::InitialPosition),
            (2, SnapshotMemoryIoStage::EndLength),
            (4, SnapshotMemoryIoStage::FinalPosition),
        ] {
            let mut reader = SeekFailIo::reader(image.clone(), fail_on_call);
            let error = load_snapshot_memory_image(&binding, &mut reader)
                .expect_err("injected reader seek failure should propagate");
            assert!(matches!(
                error,
                SnapshotMemoryLoadError::Io {
                    stage,
                    kind: ErrorKind::Other,
                } if stage == expected_stage
            ));
            assert!(!error.to_string().contains("host-path-seek-sentinel"));
        }
    }

    struct RewindFailIo {
        inner: Cursor<Vec<u8>>,
    }

    impl Read for RewindFailIo {
        fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
            self.inner.read(destination)
        }
    }

    impl Write for RewindFailIo {
        fn write(&mut self, source: &[u8]) -> io::Result<usize> {
            self.inner.write(source)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for RewindFailIo {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if position == SeekFrom::Start(0) {
                Err(Error::other("host-path-rewind-sentinel"))
            } else {
                self.inner.seek(position)
            }
        }
    }

    #[test]
    fn rewind_failure_precedes_pending_length_errors() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (image, binding) = write_image(&guest_memory);

        let original = vec![0xa5; 17];
        let mut writer = RewindFailIo {
            inner: Cursor::new(original.clone()),
        };
        let write_error =
            write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID)
                .expect_err("rewind should fail before nonempty result");
        assert!(matches!(
            write_error,
            SnapshotMemoryWriteError::Io {
                stage: SnapshotMemoryIoStage::Rewind,
                kind: ErrorKind::Other,
            }
        ));
        assert_eq!(writer.inner.into_inner(), original);
        assert!(
            !write_error
                .to_string()
                .contains("host-path-rewind-sentinel")
        );

        let mut reader = RewindFailIo {
            inner: Cursor::new(image[..image.len() - 1].to_vec()),
        };
        let load_error = load_snapshot_memory_image(&binding, &mut reader)
            .expect_err("rewind should fail before short-length result");
        assert!(matches!(
            load_error,
            SnapshotMemoryLoadError::Io {
                stage: SnapshotMemoryIoStage::Rewind,
                kind: ErrorKind::Other,
            }
        ));
        assert!(!load_error.to_string().contains("host-path-rewind-sentinel"));
    }

    enum RewindMutation {
        Truncate(usize),
        Append(u8),
    }

    struct MutatingReader {
        inner: Cursor<Vec<u8>>,
        mutation: Option<RewindMutation>,
    }

    impl Read for MutatingReader {
        fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
            self.inner.read(destination)
        }
    }

    impl Seek for MutatingReader {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if position == SeekFrom::Start(0)
                && let Some(mutation) = self.mutation.take()
            {
                match mutation {
                    RewindMutation::Truncate(length) => self.inner.get_mut().truncate(length),
                    RewindMutation::Append(byte) => self.inner.get_mut().push(byte),
                }
            }
            self.inner.seek(position)
        }
    }

    #[test]
    fn loader_detects_truncation_and_growth_after_length_preflight() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let (image, binding) = write_image(&guest_memory);

        for (length, expected_stage) in [
            (7, SnapshotMemoryIoStage::Header),
            (
                SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES + 7,
                SnapshotMemoryIoStage::Data { range_index: 0 },
            ),
            (image.len() - 1, SnapshotMemoryIoStage::Trailer),
        ] {
            let mut truncated = MutatingReader {
                inner: Cursor::new(image.clone()),
                mutation: Some(RewindMutation::Truncate(length)),
            };
            assert!(matches!(
                load_snapshot_memory_image(&binding, &mut truncated),
                Err(SnapshotMemoryLoadError::UnexpectedEnd { stage })
                    if stage == expected_stage
            ));
        }

        let mut grown = MutatingReader {
            inner: Cursor::new(image),
            mutation: Some(RewindMutation::Append(0xa5)),
        };
        assert!(matches!(
            load_snapshot_memory_image(&binding, &mut grown),
            Err(SnapshotMemoryLoadError::TrailingData)
        ));
    }

    struct AppendOnFinalCheckWriter {
        inner: Cursor<Vec<u8>>,
        current_queries: usize,
    }

    impl Write for AppendOnFinalCheckWriter {
        fn write(&mut self, source: &[u8]) -> io::Result<usize> {
            self.inner.write(source)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for AppendOnFinalCheckWriter {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if position == SeekFrom::Current(0) {
                self.current_queries += 1;
                if self.current_queries == 2 {
                    self.inner.get_mut().push(0xa5);
                }
            }
            self.inner.seek(position)
        }
    }

    #[test]
    fn writer_detects_growth_before_final_length_commit() {
        let guest_memory = memory(vec![range(0, TEST_ALIGNMENT)]);
        let mut writer = AppendOnFinalCheckWriter {
            inner: Cursor::new(Vec::new()),
            current_queries: 0,
        };
        let expected = memory_file_length(TEST_ALIGNMENT).expect("test file length should fit");
        assert!(matches!(
            write_snapshot_memory_image_with_id(&guest_memory, &mut writer, TEST_IMAGE_ID),
            Err(SnapshotMemoryWriteError::OutputLengthMismatch {
                expected: found_expected,
                actual,
            }) if found_expected == expected && actual == expected + 1
        ));
    }
}
