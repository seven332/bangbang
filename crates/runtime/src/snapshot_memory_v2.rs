//! Native-v2 guest-memory binding, lazy private-file loading, and image writing.

use std::collections::TryReserveError;
use std::ffi::CString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

use crc64::crc64;

use crate::memory::{
    GuestMemory, GuestMemoryAccessError, GuestMemoryAllocationError, GuestMemoryBacking,
    GuestMemoryRange, aarch64,
};
use crate::snapshot_diff_v2_13::NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION;
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_format_v2::{
    NATIVE_V2_MEMORY_COMPONENT_KEY, NATIVE_V2_SNAPSHOT_VERSION, SnapshotV2Component,
    SnapshotV2ComponentDisposition, SnapshotV2EncodeError, SnapshotV2State,
    encode_snapshot_v2_state,
};

mod materialize;

pub use materialize::{
    SnapshotV2MemoryHotplugMaterializationError, SnapshotV2MemoryHotplugMaterializationStage,
    materialize_snapshot_v2_memory_hotplug_file,
    materialize_snapshot_v2_memory_hotplug_file_with_cancel,
};

/// Fixed magic shared by the v2 state binding and memory-image prefix.
pub const NATIVE_V2_MEMORY_MAGIC: [u8; 8] = *b"BANGM2A\0";

/// Fixed native-v2 memory binding header size.
pub const NATIVE_V2_MEMORY_HEADER_BYTES: usize = 64;

/// Fixed metadata-to-data alignment for native-v2 memory images.
pub const NATIVE_V2_MEMORY_ALIGNMENT: u64 = 64 * 1024;

/// Fixed arm64 guest granule encoded by the native-v2 memory profile.
pub const NATIVE_V2_MEMORY_GUEST_GRANULE: u64 = aarch64::GUEST_PAGE_SIZE;

/// Fixed encoded size of one GPA-to-file extent.
pub const NATIVE_V2_MEMORY_EXTENT_BYTES: usize = 24;

/// Maximum number of retained file mappings in one memory image.
pub const NATIVE_V2_MEMORY_MAX_EXTENTS: usize = 4096;

const FLAGS: u32 = 0;
const MAGIC_OFFSET: usize = 0;
const VERSION_MAJOR_OFFSET: usize = 8;
const VERSION_MINOR_OFFSET: usize = 10;
const VERSION_PATCH_OFFSET: usize = 12;
const HEADER_BYTES_OFFSET: usize = 14;
const FLAGS_OFFSET: usize = 16;
const GUEST_GRANULE_OFFSET: usize = 20;
const ALIGNMENT_OFFSET: usize = 24;
const EXTENT_COUNT_OFFSET: usize = 28;
const IMAGE_ID_OFFSET: usize = 32;
const CHECKSUM_OFFSET: usize = 48;
const FILE_LENGTH_OFFSET: usize = 56;
const EXTENT_GPA_OFFSET: usize = 0;
const EXTENT_LENGTH_OFFSET: usize = 8;
const EXTENT_FILE_OFFSET: usize = 16;
const IMAGE_ID_BYTES: usize = 16;
const COPY_CHUNK_BYTES: usize = 1024 * 1024;
const ZERO_CHUNK_BYTES: usize = 8192;
const REDACTED: &str = "<redacted>";

/// Opaque random identity pairing one v2 state binding with one memory image.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotV2MemoryImageId([u8; IMAGE_ID_BYTES]);

impl SnapshotV2MemoryImageId {
    pub(crate) const fn from_bytes(bytes: [u8; IMAGE_ID_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn to_bytes(self) -> [u8; IMAGE_ID_BYTES] {
        self.0
    }
}

impl fmt::Debug for SnapshotV2MemoryImageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SnapshotV2MemoryImageId(<redacted>)")
    }
}

/// One validated, ordered GPA-to-file mapping in a native-v2 memory image.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MemoryExtent {
    range: GuestMemoryRange,
    file_offset: u64,
}

impl SnapshotV2MemoryExtent {
    /// Returns the guest-physical range.
    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    /// Returns the absolute file offset of this range.
    pub const fn file_offset(self) -> u64 {
        self.file_offset
    }
}

impl fmt::Debug for SnapshotV2MemoryExtent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryExtent")
            .field("mapping", &REDACTED)
            .finish()
    }
}

/// Fully validated native-v2 memory topology and image identity.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MemoryBinding {
    version: SnapshotFormatVersion,
    image_id: SnapshotV2MemoryImageId,
    extents: Vec<SnapshotV2MemoryExtent>,
    file_length: u64,
    metadata_checksum: u64,
}

impl SnapshotV2MemoryBinding {
    /// Returns the exact admitted memory metadata version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        self.version
    }

    /// Returns the opaque state/image pairing identity.
    pub const fn image_id(&self) -> SnapshotV2MemoryImageId {
        self.image_id
    }

    /// Returns the canonical ordered extent table.
    pub fn extents(&self) -> &[SnapshotV2MemoryExtent] {
        &self.extents
    }

    /// Returns the exact complete memory-image length.
    pub const fn file_length(&self) -> u64 {
        self.file_length
    }

    /// Returns the redacted identity checksum to trusted graph-validation code.
    pub const fn metadata_checksum(&self) -> u64 {
        self.metadata_checksum
    }

    /// Encodes the exact state-component payload.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotV2MemoryBindingError> {
        encode_binding(self)
    }

    pub(crate) fn image_header(
        &self,
    ) -> Result<[u8; NATIVE_V2_MEMORY_HEADER_BYTES], SnapshotV2MemoryBindingError> {
        let encoded = self.encode()?;
        encoded
            .get(..NATIVE_V2_MEMORY_HEADER_BYTES)
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?
            .try_into()
            .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)
    }
}

impl fmt::Debug for SnapshotV2MemoryBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryBinding")
            .field("version", &self.version)
            .field("image_id", &REDACTED)
            .field("extent_count", &self.extents.len())
            .field("file_length", &self.file_length)
            .field("metadata_checksum", &REDACTED)
            .finish()
    }
}

/// Native-v2 memory-binding construction or decoding failure.
#[derive(Debug)]
pub enum SnapshotV2MemoryBindingError {
    CountOutOfBounds,
    MetadataAllocationFailed { source: TryReserveError },
    InvalidMagic,
    UnsupportedVersion,
    InvalidHeader,
    InvalidLength,
    LengthOverflow,
    IntegrityMismatch,
    InvalidExtent,
    InvalidExtentTopology,
    NonCanonicalFileOffset,
    DataTooLarge,
    FileLengthMismatch,
}

impl fmt::Display for SnapshotV2MemoryBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountOutOfBounds => {
                formatter.write_str("native-v2 memory extent count is out of bounds")
            }
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("native-v2 memory metadata allocation failed")
            }
            Self::InvalidMagic => formatter.write_str("native-v2 memory magic is invalid"),
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 memory version is unsupported")
            }
            Self::InvalidHeader => formatter.write_str("native-v2 memory header is noncanonical"),
            Self::InvalidLength => {
                formatter.write_str("native-v2 memory binding length is invalid")
            }
            Self::LengthOverflow => {
                formatter.write_str("native-v2 memory length arithmetic overflowed")
            }
            Self::IntegrityMismatch => {
                formatter.write_str("native-v2 memory metadata CRC-64/Jones check failed")
            }
            Self::InvalidExtent => formatter.write_str("native-v2 memory extent is invalid"),
            Self::InvalidExtentTopology => {
                formatter.write_str("native-v2 memory extent topology is noncanonical")
            }
            Self::NonCanonicalFileOffset => {
                formatter.write_str("native-v2 memory file offset is noncanonical")
            }
            Self::DataTooLarge => {
                formatter.write_str("native-v2 memory guest data exceeds arm64 policy")
            }
            Self::FileLengthMismatch => {
                formatter.write_str("native-v2 memory file length is noncanonical")
            }
        }
    }
}

impl std::error::Error for SnapshotV2MemoryBindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            _ => None,
        }
    }
}

/// Typed native-v2 state-memory profile failure.
#[derive(Debug)]
pub enum SnapshotV2MemoryStateError {
    MissingMemoryComponent,
    InvalidMemoryComponentProfile,
    Binding(SnapshotV2MemoryBindingError),
}

impl fmt::Display for SnapshotV2MemoryStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMemoryComponent => {
                formatter.write_str("native-v2 state has no memory component")
            }
            Self::InvalidMemoryComponentProfile => {
                formatter.write_str("native-v2 state memory component profile is invalid")
            }
            Self::Binding(source) => {
                write!(formatter, "invalid native-v2 memory binding: {source}")
            }
        }
    }
}

impl std::error::Error for SnapshotV2MemoryStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(source) => Some(source),
            Self::MissingMemoryComponent | Self::InvalidMemoryComponentProfile => None,
        }
    }
}

impl From<SnapshotV2MemoryBindingError> for SnapshotV2MemoryStateError {
    fn from(source: SnapshotV2MemoryBindingError) -> Self {
        Self::Binding(source)
    }
}

/// Native-v2 state encoding failure when attaching the typed memory component.
#[derive(Debug)]
pub enum SnapshotV2MemoryStateEncodeError {
    Binding(SnapshotV2MemoryBindingError),
    State(SnapshotV2EncodeError),
}

impl fmt::Display for SnapshotV2MemoryStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(source) => write!(formatter, "failed to encode memory binding: {source}"),
            Self::State(source) => write!(formatter, "failed to encode native-v2 state: {source}"),
        }
    }
}

impl std::error::Error for SnapshotV2MemoryStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(source) => Some(source),
            Self::State(source) => Some(source),
        }
    }
}

/// Encodes a canonical native-v2 state containing the singleton memory binding.
pub fn encode_snapshot_v2_state_with_memory(
    binding: &SnapshotV2MemoryBinding,
) -> Result<Vec<u8>, SnapshotV2MemoryStateEncodeError> {
    let payload = binding
        .encode()
        .map_err(SnapshotV2MemoryStateEncodeError::Binding)?;
    let component = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &payload,
    );
    encode_snapshot_v2_state(&[], &[component]).map_err(SnapshotV2MemoryStateEncodeError::State)
}

/// Extracts and validates the singleton memory binding from a compatible state.
pub fn decode_snapshot_v2_memory_binding(
    state: &SnapshotV2State<'_>,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryStateError> {
    let mut selected = None;
    for component in state.components() {
        if component.key().kind() != NATIVE_V2_MEMORY_COMPONENT_KEY.kind() {
            continue;
        }
        if component.key() != NATIVE_V2_MEMORY_COMPONENT_KEY
            || component.disposition() != SnapshotV2ComponentDisposition::Semantic
            || selected.is_some()
        {
            return Err(SnapshotV2MemoryStateError::InvalidMemoryComponentProfile);
        }
        selected = Some(component.payload());
    }
    let payload = selected.ok_or(SnapshotV2MemoryStateError::MissingMemoryComponent)?;
    decode_binding(payload).map_err(Into::into)
}

pub(crate) fn decode_snapshot_v2_memory_binding_payload(
    payload: &[u8],
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
    decode_binding(payload)
}

/// Stages at which cooperative native-v2 memory writing may stop or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryIoStage {
    InitialPosition,
    Header,
    MetadataPadding,
    Data { extent_index: usize },
    FinalLength,
}

impl fmt::Display for SnapshotV2MemoryIoStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialPosition => formatter.write_str("initial output preflight"),
            Self::Header => formatter.write_str("memory metadata header"),
            Self::MetadataPadding => formatter.write_str("memory metadata padding"),
            Self::Data { extent_index } => write!(formatter, "memory extent {extent_index}"),
            Self::FinalLength => formatter.write_str("final memory length"),
        }
    }
}

/// Native-v2 memory image writing failure.
#[derive(Debug)]
pub enum SnapshotV2MemoryWriteError {
    Binding(SnapshotV2MemoryBindingError),
    IdentityUnavailable,
    ChunkAllocationFailed {
        source: TryReserveError,
    },
    InvalidInitialPosition,
    NonEmptyOutput,
    PositionMismatch {
        stage: SnapshotV2MemoryIoStage,
    },
    Cancelled {
        stage: SnapshotV2MemoryIoStage,
    },
    Io {
        stage: SnapshotV2MemoryIoStage,
        kind: io::ErrorKind,
    },
    GuestMemoryRead {
        stage: SnapshotV2MemoryIoStage,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for SnapshotV2MemoryWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(source) => write!(formatter, "invalid native-v2 memory: {source}"),
            Self::IdentityUnavailable => {
                formatter.write_str("native-v2 memory identity is unavailable")
            }
            Self::ChunkAllocationFailed { .. } => {
                formatter.write_str("native-v2 memory copy-buffer allocation failed")
            }
            Self::InvalidInitialPosition => {
                formatter.write_str("native-v2 memory output is not positioned at zero")
            }
            Self::NonEmptyOutput => formatter.write_str("native-v2 memory output is not empty"),
            Self::PositionMismatch { stage } => {
                write!(
                    formatter,
                    "native-v2 memory output position is invalid at {stage}"
                )
            }
            Self::Cancelled { stage } => {
                write!(
                    formatter,
                    "native-v2 memory output was cancelled before {stage}"
                )
            }
            Self::Io { stage, kind } => {
                write!(formatter, "native-v2 memory I/O failed at {stage}: {kind}")
            }
            Self::GuestMemoryRead { stage, source } => {
                write!(formatter, "guest-memory read failed at {stage}: {source}")
            }
        }
    }
}

impl std::error::Error for SnapshotV2MemoryWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(source) => Some(source),
            Self::ChunkAllocationFailed { source } => Some(source),
            Self::GuestMemoryRead { source, .. } => Some(source),
            Self::IdentityUnavailable
            | Self::InvalidInitialPosition
            | Self::NonEmptyOutput
            | Self::PositionMismatch { .. }
            | Self::Cancelled { .. }
            | Self::Io { .. } => None,
        }
    }
}

impl From<SnapshotV2MemoryBindingError> for SnapshotV2MemoryWriteError {
    fn from(source: SnapshotV2MemoryBindingError) -> Self {
        Self::Binding(source)
    }
}

/// Streams one canonical native-v2 memory image without hashing guest bytes.
pub fn write_snapshot_v2_memory_image<W: Write + Seek>(
    memory: &GuestMemory,
    writer: &mut W,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryWriteError> {
    write_snapshot_v2_memory_image_with_cancel(memory, writer, |_| false)
}

/// Streams one image for an explicit known native-v2 compatibility version.
///
/// The ordinary writer remains fixed to [`NATIVE_V2_SNAPSHOT_VERSION`]. This
/// seam lets retained legacy profiles write a binding and image header at
/// their exact version without changing current public snapshot publication.
pub fn write_snapshot_v2_memory_image_with_compatibility_version<W: Write + Seek>(
    memory: &GuestMemory,
    writer: &mut W,
    version: SnapshotFormatVersion,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryWriteError> {
    write_snapshot_v2_memory_image_with_compatibility_version_and_cancel(
        memory,
        writer,
        version,
        |_| false,
    )
}

/// Streams one canonical image with cooperative bounded-stage cancellation.
pub fn write_snapshot_v2_memory_image_with_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    is_cancelled: C,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2MemoryIoStage) -> bool,
{
    write_snapshot_v2_memory_image_with_compatibility_version_and_cancel(
        memory,
        writer,
        NATIVE_V2_SNAPSHOT_VERSION,
        is_cancelled,
    )
}

/// Streams one explicitly versioned image with bounded-stage cancellation.
pub fn write_snapshot_v2_memory_image_with_compatibility_version_and_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    version: SnapshotFormatVersion,
    is_cancelled: C,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2MemoryIoStage) -> bool,
{
    let image_id = generate_image_id()?;
    write_snapshot_v2_memory_image_with_version_id_and_cancel(
        memory,
        writer,
        version,
        image_id,
        is_cancelled,
    )
}

#[cfg(test)]
fn write_snapshot_v2_memory_image_with_id_and_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    image_id: SnapshotV2MemoryImageId,
    is_cancelled: C,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2MemoryIoStage) -> bool,
{
    write_snapshot_v2_memory_image_with_version_id_and_cancel(
        memory,
        writer,
        NATIVE_V2_SNAPSHOT_VERSION,
        image_id,
        is_cancelled,
    )
}

fn write_snapshot_v2_memory_image_with_version_id_and_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    version: SnapshotFormatVersion,
    image_id: SnapshotV2MemoryImageId,
    mut is_cancelled: C,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2MemoryIoStage) -> bool,
{
    check_cancelled(&mut is_cancelled, SnapshotV2MemoryIoStage::InitialPosition)?;
    preflight_empty_output(writer)?;
    let binding =
        snapshot_v2_memory_binding_from_memory_with_version_and_id(memory, version, image_id)?;
    let header = binding.image_header()?;

    check_cancelled(&mut is_cancelled, SnapshotV2MemoryIoStage::Header)?;
    write_all_stage(writer, &header, SnapshotV2MemoryIoStage::Header)?;

    let zeroes = [0_u8; ZERO_CHUNK_BYTES];
    let mut padding = NATIVE_V2_MEMORY_ALIGNMENT
        .checked_sub(
            u64::try_from(NATIVE_V2_MEMORY_HEADER_BYTES)
                .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    while padding != 0 {
        check_cancelled(&mut is_cancelled, SnapshotV2MemoryIoStage::MetadataPadding)?;
        let length = usize::try_from(padding.min(ZERO_CHUNK_BYTES as u64))
            .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?;
        let padding_chunk = zeroes
            .get(..length)
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        write_all_stage(
            writer,
            padding_chunk,
            SnapshotV2MemoryIoStage::MetadataPadding,
        )?;
        padding -=
            u64::try_from(length).map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?;
    }

    let mut chunk = Vec::new();
    chunk
        .try_reserve_exact(COPY_CHUNK_BYTES)
        .map_err(|source| SnapshotV2MemoryWriteError::ChunkAllocationFailed { source })?;
    chunk.resize(COPY_CHUNK_BYTES, 0);
    for (extent_index, extent) in binding.extents().iter().copied().enumerate() {
        let stage = SnapshotV2MemoryIoStage::Data { extent_index };
        check_cancelled(&mut is_cancelled, stage)?;
        seek_exact(writer, extent.file_offset(), stage)?;

        let mut copied = 0_u64;
        while copied < extent.range().size() {
            check_cancelled(&mut is_cancelled, stage)?;
            let remaining = extent.range().size() - copied;
            let length = usize::try_from(remaining.min(COPY_CHUNK_BYTES as u64))
                .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?;
            let address = extent
                .range()
                .start()
                .checked_add(copied)
                .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
            let chunk_slice = chunk
                .get_mut(..length)
                .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
            memory
                .read_slice(chunk_slice, address)
                .map_err(|source| SnapshotV2MemoryWriteError::GuestMemoryRead { stage, source })?;
            write_all_stage(writer, chunk_slice, stage)?;
            copied = copied
                .checked_add(
                    u64::try_from(length)
                        .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?,
                )
                .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        }
    }

    check_cancelled(&mut is_cancelled, SnapshotV2MemoryIoStage::FinalLength)?;
    let position = writer
        .stream_position()
        .map_err(|source| SnapshotV2MemoryWriteError::Io {
            stage: SnapshotV2MemoryIoStage::FinalLength,
            kind: source.kind(),
        })?;
    if position != binding.file_length() {
        return Err(SnapshotV2MemoryWriteError::PositionMismatch {
            stage: SnapshotV2MemoryIoStage::FinalLength,
        });
    }
    let end = writer
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotV2MemoryWriteError::Io {
            stage: SnapshotV2MemoryIoStage::FinalLength,
            kind: source.kind(),
        })?;
    if end != binding.file_length() {
        return Err(SnapshotV2MemoryWriteError::PositionMismatch {
            stage: SnapshotV2MemoryIoStage::FinalLength,
        });
    }
    Ok(binding)
}

fn preflight_empty_output<W: Seek>(writer: &mut W) -> Result<(), SnapshotV2MemoryWriteError> {
    let initial = writer
        .stream_position()
        .map_err(|source| SnapshotV2MemoryWriteError::Io {
            stage: SnapshotV2MemoryIoStage::InitialPosition,
            kind: source.kind(),
        })?;
    if initial != 0 {
        return Err(SnapshotV2MemoryWriteError::InvalidInitialPosition);
    }
    let end = writer
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotV2MemoryWriteError::Io {
            stage: SnapshotV2MemoryIoStage::InitialPosition,
            kind: source.kind(),
        })?;
    let rewind =
        writer
            .seek(SeekFrom::Start(0))
            .map_err(|source| SnapshotV2MemoryWriteError::Io {
                stage: SnapshotV2MemoryIoStage::InitialPosition,
                kind: source.kind(),
            })?;
    if rewind != 0 {
        return Err(SnapshotV2MemoryWriteError::PositionMismatch {
            stage: SnapshotV2MemoryIoStage::InitialPosition,
        });
    }
    if end != 0 {
        return Err(SnapshotV2MemoryWriteError::NonEmptyOutput);
    }
    Ok(())
}

fn check_cancelled<C>(
    is_cancelled: &mut C,
    stage: SnapshotV2MemoryIoStage,
) -> Result<(), SnapshotV2MemoryWriteError>
where
    C: FnMut(SnapshotV2MemoryIoStage) -> bool,
{
    if is_cancelled(stage) {
        Err(SnapshotV2MemoryWriteError::Cancelled { stage })
    } else {
        Ok(())
    }
}

fn seek_exact<W: Seek>(
    writer: &mut W,
    position: u64,
    stage: SnapshotV2MemoryIoStage,
) -> Result<(), SnapshotV2MemoryWriteError> {
    let actual = writer.seek(SeekFrom::Start(position)).map_err(|source| {
        SnapshotV2MemoryWriteError::Io {
            stage,
            kind: source.kind(),
        }
    })?;
    if actual != position {
        return Err(SnapshotV2MemoryWriteError::PositionMismatch { stage });
    }
    Ok(())
}

fn write_all_stage<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    stage: SnapshotV2MemoryIoStage,
) -> Result<(), SnapshotV2MemoryWriteError> {
    writer
        .write_all(bytes)
        .map_err(|source| SnapshotV2MemoryWriteError::Io {
            stage,
            kind: source.kind(),
        })
}

/// Native-v2 retained-file validation or lazy mapping failure.
#[derive(Debug)]
pub enum SnapshotV2MemoryLoadError {
    State(SnapshotV2MemoryStateError),
    InvalidPath,
    Open { kind: io::ErrorKind },
    Inspect { kind: io::ErrorKind },
    PositionMismatch,
    DescriptorNotReadOnly,
    DescriptorNotCloseOnExec,
    NotRegularFile,
    FileLengthMismatch,
    MetadataRead { kind: io::ErrorKind },
    MetadataAllocationFailed { source: TryReserveError },
    MemoryHeaderMismatch,
    NonZeroMetadataPadding,
    SourceChanged,
    Mapping { source: GuestMemoryAllocationError },
}

impl fmt::Display for SnapshotV2MemoryLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(source) => write!(formatter, "invalid native-v2 memory state: {source}"),
            Self::InvalidPath => formatter.write_str("native-v2 memory path is invalid"),
            Self::Open { kind } => write!(formatter, "native-v2 memory open failed: {kind}"),
            Self::Inspect { kind } => {
                write!(
                    formatter,
                    "native-v2 memory descriptor inspection failed: {kind}"
                )
            }
            Self::PositionMismatch => {
                formatter.write_str("native-v2 memory descriptor position does not match state")
            }
            Self::DescriptorNotReadOnly => {
                formatter.write_str("native-v2 memory descriptor is not read-only")
            }
            Self::DescriptorNotCloseOnExec => {
                formatter.write_str("native-v2 memory descriptor is not close-on-exec")
            }
            Self::NotRegularFile => {
                formatter.write_str("native-v2 memory descriptor is not a regular file")
            }
            Self::FileLengthMismatch => {
                formatter.write_str("native-v2 memory descriptor length does not match state")
            }
            Self::MetadataRead { kind } => {
                write!(formatter, "native-v2 memory metadata read failed: {kind}")
            }
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("native-v2 memory loader metadata allocation failed")
            }
            Self::MemoryHeaderMismatch => {
                formatter.write_str("native-v2 memory header does not match state")
            }
            Self::NonZeroMetadataPadding => {
                formatter.write_str("native-v2 memory metadata padding is noncanonical")
            }
            Self::SourceChanged => {
                formatter.write_str("native-v2 memory descriptor changed during validation")
            }
            Self::Mapping { source } => {
                write!(formatter, "native-v2 memory mapping failed: {source}")
            }
        }
    }
}

impl std::error::Error for SnapshotV2MemoryLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::State(source) => Some(source),
            Self::Mapping { source } => Some(source),
            Self::MetadataAllocationFailed { source } => Some(source),
            _ => None,
        }
    }
}

impl From<SnapshotV2MemoryStateError> for SnapshotV2MemoryLoadError {
    fn from(source: SnapshotV2MemoryStateError) -> Self {
        Self::State(source)
    }
}

/// Opens one direct path once and returns demand-paged private guest memory.
pub fn load_snapshot_v2_memory_path(
    state: &SnapshotV2State<'_>,
    path: &Path,
) -> Result<GuestMemory, SnapshotV2MemoryLoadError> {
    let binding = decode_snapshot_v2_memory_binding(state)?;
    let file = open_regular_final(path)?;
    load_snapshot_v2_memory_binding_from_file(&binding, file)
}

/// Adopts one already-opened contained descriptor and returns lazy guest memory.
pub fn load_snapshot_v2_memory_file(
    state: &SnapshotV2State<'_>,
    file: File,
) -> Result<GuestMemory, SnapshotV2MemoryLoadError> {
    let binding = decode_snapshot_v2_memory_binding(state)?;
    load_snapshot_v2_memory_binding_from_file(&binding, file)
}

/// Maps an already validated binding from one adopted descriptor.
fn load_snapshot_v2_memory_binding_from_file(
    binding: &SnapshotV2MemoryBinding,
    file: File,
) -> Result<GuestMemory, SnapshotV2MemoryLoadError> {
    load_snapshot_v2_memory_binding_from_file_with_hook(binding, file, |_, _| {})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotV2MemoryLoadStage {
    Preflight,
    Metadata,
    Mapping,
}

fn load_snapshot_v2_memory_binding_from_file_with_hook(
    binding: &SnapshotV2MemoryBinding,
    file: File,
    mut hook: impl FnMut(SnapshotV2MemoryLoadStage, &File),
) -> Result<GuestMemory, SnapshotV2MemoryLoadError> {
    let source = ValidatedSnapshotV2MemorySource::new_with_hook(binding, file, &mut hook)?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(binding.extents().len())
        .map_err(|source| SnapshotV2MemoryLoadError::MetadataAllocationFailed { source })?;
    ranges.extend(
        binding
            .extents()
            .iter()
            .map(|extent| (extent.range(), extent.file_offset())),
    );
    let memory = GuestMemory::from_private_file_ranges(
        &ranges,
        Arc::clone(source.file()),
        GuestMemoryBacking::Anonymous,
    )
    .map_err(|source| SnapshotV2MemoryLoadError::Mapping { source })?;
    hook(SnapshotV2MemoryLoadStage::Mapping, source.file().as_ref());
    if let Err(error) = source.verify_unchanged() {
        drop(memory);
        return Err(error);
    }
    Ok(memory)
}

pub(crate) struct ValidatedSnapshotV2MemorySource {
    file: Arc<File>,
    facts: FileFacts,
}

impl ValidatedSnapshotV2MemorySource {
    pub(crate) fn new_with_hook(
        binding: &SnapshotV2MemoryBinding,
        file: File,
        mut hook: impl FnMut(SnapshotV2MemoryLoadStage, &File),
    ) -> Result<Self, SnapshotV2MemoryLoadError> {
        let facts = inspect_file(&file)?;
        if facts.length != binding.file_length() {
            return Err(SnapshotV2MemoryLoadError::FileLengthMismatch);
        }
        hook(SnapshotV2MemoryLoadStage::Preflight, &file);

        verify_memory_metadata(binding, &file)?;
        hook(SnapshotV2MemoryLoadStage::Metadata, &file);
        let after_metadata = inspect_file(&file)?;
        if after_metadata != facts {
            return Err(SnapshotV2MemoryLoadError::SourceChanged);
        }

        Ok(Self {
            file: Arc::new(file),
            facts,
        })
    }

    pub(crate) fn file(&self) -> &Arc<File> {
        &self.file
    }

    pub(crate) const fn facts(&self) -> FileFacts {
        self.facts
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), SnapshotV2MemoryLoadError> {
        if inspect_file(self.file.as_ref())? == self.facts {
            Ok(())
        } else {
            Err(SnapshotV2MemoryLoadError::SourceChanged)
        }
    }
}

/// Verifies one transaction-owned native-v2 staging descriptor without mapping it.
///
/// Publication owns a read-write descriptor, unlike the final retained-file
/// loader. This verifier therefore checks the exact output position, length,
/// regular-file identity, header, zero padding, and source stability without
/// applying the final loader's read-only policy.
pub(crate) fn verify_snapshot_v2_memory_image_output(
    binding: &SnapshotV2MemoryBinding,
    file: &mut File,
) -> Result<(), SnapshotV2MemoryLoadError> {
    verify_snapshot_v2_memory_image_output_with_hook(binding, file, |_| {})
}

fn verify_snapshot_v2_memory_image_output_with_hook(
    binding: &SnapshotV2MemoryBinding,
    file: &mut File,
    mut hook: impl FnMut(&File),
) -> Result<(), SnapshotV2MemoryLoadError> {
    let before = inspect_file_facts(file)?;
    validate_regular_file(&before)?;
    let position = file
        .stream_position()
        .map_err(|source| SnapshotV2MemoryLoadError::Inspect {
            kind: source.kind(),
        })?;
    if position != binding.file_length() {
        return Err(SnapshotV2MemoryLoadError::PositionMismatch);
    }
    if before.length != binding.file_length() {
        return Err(SnapshotV2MemoryLoadError::FileLengthMismatch);
    }
    verify_memory_metadata(binding, file)?;
    hook(file);
    let after = inspect_file_facts(file)?;
    validate_regular_file(&after)?;
    if after != before {
        return Err(SnapshotV2MemoryLoadError::SourceChanged);
    }
    Ok(())
}

fn verify_memory_metadata(
    binding: &SnapshotV2MemoryBinding,
    file: &File,
) -> Result<(), SnapshotV2MemoryLoadError> {
    let metadata_bytes = usize::try_from(NATIVE_V2_MEMORY_ALIGNMENT)
        .map_err(|_| SnapshotV2MemoryLoadError::FileLengthMismatch)?;
    let mut metadata = Vec::new();
    metadata
        .try_reserve_exact(metadata_bytes)
        .map_err(|source| SnapshotV2MemoryLoadError::MetadataAllocationFailed { source })?;
    metadata.resize(metadata_bytes, 0);
    read_exact_at(file, &mut metadata, 0)?;

    let expected_header = binding
        .image_header()
        .map_err(SnapshotV2MemoryStateError::Binding)?;
    if metadata.get(..NATIVE_V2_MEMORY_HEADER_BYTES) != Some(expected_header.as_slice()) {
        return Err(SnapshotV2MemoryLoadError::MemoryHeaderMismatch);
    }
    if metadata
        .get(NATIVE_V2_MEMORY_HEADER_BYTES..)
        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        return Err(SnapshotV2MemoryLoadError::NonZeroMetadataPadding);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileFacts {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    status_flags: i32,
    descriptor_flags: i32,
}

impl FileFacts {
    pub(crate) const fn length(self) -> u64 {
        self.length
    }

    pub(crate) const fn is_read_write(self) -> bool {
        self.status_flags & libc::O_ACCMODE == libc::O_RDWR
    }

    pub(crate) const fn is_append(self) -> bool {
        self.status_flags & libc::O_APPEND != 0
    }

    pub(crate) const fn is_close_on_exec(self) -> bool {
        self.descriptor_flags & libc::FD_CLOEXEC != 0
    }

    pub(crate) const fn is_regular(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
    }

    pub(crate) const fn same_object(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

pub(crate) fn inspect_file(file: &File) -> Result<FileFacts, SnapshotV2MemoryLoadError> {
    let facts = inspect_file_facts(file)?;
    if facts.status_flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(SnapshotV2MemoryLoadError::DescriptorNotReadOnly);
    }
    if facts.descriptor_flags & libc::FD_CLOEXEC == 0 {
        return Err(SnapshotV2MemoryLoadError::DescriptorNotCloseOnExec);
    }
    validate_regular_file(&facts)?;
    Ok(facts)
}

pub(crate) fn inspect_file_facts(file: &File) -> Result<FileFacts, SnapshotV2MemoryLoadError> {
    // SAFETY: `file` owns a live descriptor and these commands have no pointer
    // arguments.
    let status_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if status_flags < 0 {
        return Err(SnapshotV2MemoryLoadError::Inspect {
            kind: io::Error::last_os_error().kind(),
        });
    }
    // SAFETY: as above, `F_GETFD` reads descriptor-local flags only.
    let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if descriptor_flags < 0 {
        return Err(SnapshotV2MemoryLoadError::Inspect {
            kind: io::Error::last_os_error().kind(),
        });
    }

    let metadata = file
        .metadata()
        .map_err(|source| SnapshotV2MemoryLoadError::Inspect {
            kind: source.kind(),
        })?;
    Ok(FileFacts {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        length: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        status_flags,
        descriptor_flags,
    })
}

fn validate_regular_file(facts: &FileFacts) -> Result<(), SnapshotV2MemoryLoadError> {
    if facts.mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFREG) {
        Ok(())
    } else {
        Err(SnapshotV2MemoryLoadError::NotRegularFile)
    }
}

fn read_exact_at(
    file: &File,
    mut bytes: &mut [u8],
    mut offset: u64,
) -> Result<(), SnapshotV2MemoryLoadError> {
    while !bytes.is_empty() {
        match file.read_at(bytes, offset) {
            Ok(0) => {
                return Err(SnapshotV2MemoryLoadError::MetadataRead {
                    kind: io::ErrorKind::UnexpectedEof,
                });
            }
            Ok(count) => {
                let count_u64 =
                    u64::try_from(count).map_err(|_| SnapshotV2MemoryLoadError::MetadataRead {
                        kind: io::ErrorKind::InvalidData,
                    })?;
                offset = offset.checked_add(count_u64).ok_or(
                    SnapshotV2MemoryLoadError::MetadataRead {
                        kind: io::ErrorKind::InvalidData,
                    },
                )?;
                bytes = bytes
                    .get_mut(count..)
                    .ok_or(SnapshotV2MemoryLoadError::MetadataRead {
                        kind: io::ErrorKind::InvalidData,
                    })?;
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(SnapshotV2MemoryLoadError::MetadataRead {
                    kind: source.kind(),
                });
            }
        }
    }
    Ok(())
}

fn open_regular_final(path: &Path) -> Result<File, SnapshotV2MemoryLoadError> {
    let component = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(SnapshotV2MemoryLoadError::InvalidPath)?;
    let component =
        CString::new(component.as_bytes()).map_err(|_| SnapshotV2MemoryLoadError::InvalidPath)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(parent)
        .map_err(|source| SnapshotV2MemoryLoadError::Open {
            kind: source.kind(),
        })?;
    // SAFETY: `directory` is live and `component` is one NUL-terminated final
    // component. No-follow/nonblocking prevent final-symlink traversal and
    // special-file hangs before descriptor validation.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(SnapshotV2MemoryLoadError::Open {
            kind: io::Error::last_os_error().kind(),
        });
    }
    // SAFETY: successful `openat` returned one fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

pub(crate) fn snapshot_v2_memory_binding_from_memory_with_version_and_id(
    memory: &GuestMemory,
    version: SnapshotFormatVersion,
    image_id: SnapshotV2MemoryImageId,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
    let count = memory.regions().len();
    validate_extent_count(count)?;
    let mut extents = Vec::new();
    extents
        .try_reserve_exact(count)
        .map_err(|source| SnapshotV2MemoryBindingError::MetadataAllocationFailed { source })?;
    let mut file_offset = NATIVE_V2_MEMORY_ALIGNMENT;
    let mut total_data = 0_u64;
    for region in memory.regions() {
        let range = region.range();
        validate_range(range)?;
        total_data = total_data
            .checked_add(range.size())
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        if total_data > aarch64::DRAM_MEM_MAX_SIZE {
            return Err(SnapshotV2MemoryBindingError::DataTooLarge);
        }
        extents.push(SnapshotV2MemoryExtent { range, file_offset });
        file_offset = file_offset
            .checked_add(range.size())
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        file_offset = align_up(file_offset, NATIVE_V2_MEMORY_ALIGNMENT)?;
    }
    let last = extents
        .last()
        .ok_or(SnapshotV2MemoryBindingError::CountOutOfBounds)?;
    let file_length = last
        .file_offset()
        .checked_add(last.range().size())
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    build_binding_with_version(version, image_id, extents, file_length)
}

#[cfg(test)]
pub(crate) fn snapshot_v2_memory_binding_from_ranges_for_test(
    version: SnapshotFormatVersion,
    image_id: SnapshotV2MemoryImageId,
    ranges: &[GuestMemoryRange],
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
    validate_extent_count(ranges.len())?;
    let mut extents = Vec::new();
    extents
        .try_reserve_exact(ranges.len())
        .map_err(|source| SnapshotV2MemoryBindingError::MetadataAllocationFailed { source })?;
    let mut file_offset = NATIVE_V2_MEMORY_ALIGNMENT;
    for range in ranges.iter().copied() {
        extents.push(SnapshotV2MemoryExtent { range, file_offset });
        file_offset = file_offset
            .checked_add(range.size())
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        file_offset = align_up(file_offset, NATIVE_V2_MEMORY_ALIGNMENT)?;
    }
    let last = extents
        .last()
        .ok_or(SnapshotV2MemoryBindingError::CountOutOfBounds)?;
    let file_length = last
        .file_offset()
        .checked_add(last.range().size())
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    build_binding_with_version(version, image_id, extents, file_length)
}

fn build_binding_with_version(
    version: SnapshotFormatVersion,
    image_id: SnapshotV2MemoryImageId,
    extents: Vec<SnapshotV2MemoryExtent>,
    file_length: u64,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
    validate_memory_version(version)?;
    validate_extents(&extents, file_length)?;
    let mut binding = SnapshotV2MemoryBinding {
        version,
        image_id,
        extents,
        file_length,
        metadata_checksum: 0,
    };
    let encoded = encode_binding(&binding)?;
    binding.metadata_checksum = read_u64(&encoded, CHECKSUM_OFFSET)?;
    Ok(binding)
}

fn encode_binding(
    binding: &SnapshotV2MemoryBinding,
) -> Result<Vec<u8>, SnapshotV2MemoryBindingError> {
    validate_extents(&binding.extents, binding.file_length)?;
    let extent_bytes = binding
        .extents
        .len()
        .checked_mul(NATIVE_V2_MEMORY_EXTENT_BYTES)
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    let length = NATIVE_V2_MEMORY_HEADER_BYTES
        .checked_add(extent_bytes)
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|source| SnapshotV2MemoryBindingError::MetadataAllocationFailed { source })?;
    bytes.extend_from_slice(&NATIVE_V2_MEMORY_MAGIC);
    validate_memory_version(binding.version)?;
    bytes.extend_from_slice(&binding.version.major().to_le_bytes());
    bytes.extend_from_slice(&binding.version.minor().to_le_bytes());
    bytes.extend_from_slice(&binding.version.patch().to_le_bytes());
    bytes.extend_from_slice(
        &u16::try_from(NATIVE_V2_MEMORY_HEADER_BYTES)
            .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&FLAGS.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(NATIVE_V2_MEMORY_GUEST_GRANULE)
            .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(NATIVE_V2_MEMORY_ALIGNMENT)
            .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(binding.extents.len())
            .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&binding.image_id.0);
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(&binding.file_length.to_le_bytes());
    for extent in &binding.extents {
        bytes.extend_from_slice(&extent.range().start().raw_value().to_le_bytes());
        bytes.extend_from_slice(&extent.range().size().to_le_bytes());
        bytes.extend_from_slice(&extent.file_offset().to_le_bytes());
    }
    let checksum = crc64(0, &bytes);
    replace_u64(&mut bytes, CHECKSUM_OFFSET, checksum)?;
    debug_assert_eq!(bytes.len(), length);
    Ok(bytes)
}

fn decode_binding(bytes: &[u8]) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
    if bytes.len() < NATIVE_V2_MEMORY_HEADER_BYTES {
        return Err(SnapshotV2MemoryBindingError::InvalidLength);
    }
    if read_array::<8>(bytes, MAGIC_OFFSET)? != NATIVE_V2_MEMORY_MAGIC {
        return Err(SnapshotV2MemoryBindingError::InvalidMagic);
    }
    let version = SnapshotFormatVersion::new(
        read_u16(bytes, VERSION_MAJOR_OFFSET)?,
        read_u16(bytes, VERSION_MINOR_OFFSET)?,
        read_u16(bytes, VERSION_PATCH_OFFSET)?,
    );
    validate_memory_version(version)?;
    if usize::from(read_u16(bytes, HEADER_BYTES_OFFSET)?) != NATIVE_V2_MEMORY_HEADER_BYTES
        || read_u32(bytes, FLAGS_OFFSET)? != FLAGS
        || u64::from(read_u32(bytes, GUEST_GRANULE_OFFSET)?) != NATIVE_V2_MEMORY_GUEST_GRANULE
        || u64::from(read_u32(bytes, ALIGNMENT_OFFSET)?) != NATIVE_V2_MEMORY_ALIGNMENT
    {
        return Err(SnapshotV2MemoryBindingError::InvalidHeader);
    }
    let count = usize::try_from(read_u32(bytes, EXTENT_COUNT_OFFSET)?)
        .map_err(|_| SnapshotV2MemoryBindingError::LengthOverflow)?;
    validate_extent_count(count)?;
    let expected_length = NATIVE_V2_MEMORY_HEADER_BYTES
        .checked_add(
            count
                .checked_mul(NATIVE_V2_MEMORY_EXTENT_BYTES)
                .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    if bytes.len() != expected_length {
        return Err(SnapshotV2MemoryBindingError::InvalidLength);
    }
    let stored_checksum = read_u64(bytes, CHECKSUM_OFFSET)?;
    if binding_checksum(bytes)? != stored_checksum {
        return Err(SnapshotV2MemoryBindingError::IntegrityMismatch);
    }

    let image_id = SnapshotV2MemoryImageId(read_array(bytes, IMAGE_ID_OFFSET)?);
    let file_length = read_u64(bytes, FILE_LENGTH_OFFSET)?;
    let mut extents = Vec::new();
    extents
        .try_reserve_exact(count)
        .map_err(|source| SnapshotV2MemoryBindingError::MetadataAllocationFailed { source })?;
    for index in 0..count {
        let offset = NATIVE_V2_MEMORY_HEADER_BYTES
            .checked_add(
                index
                    .checked_mul(NATIVE_V2_MEMORY_EXTENT_BYTES)
                    .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?,
            )
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        let range = GuestMemoryRange::new(
            crate::memory::GuestAddress::new(read_u64(bytes, offset + EXTENT_GPA_OFFSET)?),
            read_u64(bytes, offset + EXTENT_LENGTH_OFFSET)?,
        )
        .map_err(|_| SnapshotV2MemoryBindingError::InvalidExtent)?;
        extents.push(SnapshotV2MemoryExtent {
            range,
            file_offset: read_u64(bytes, offset + EXTENT_FILE_OFFSET)?,
        });
    }
    validate_extents(&extents, file_length)?;
    Ok(SnapshotV2MemoryBinding {
        version,
        image_id,
        extents,
        file_length,
        metadata_checksum: stored_checksum,
    })
}

fn validate_memory_version(
    version: SnapshotFormatVersion,
) -> Result<(), SnapshotV2MemoryBindingError> {
    if version.major() == NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION.major()
        && (1..=NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION.minor()).contains(&version.minor())
    {
        Ok(())
    } else {
        Err(SnapshotV2MemoryBindingError::UnsupportedVersion)
    }
}

fn validate_extent_count(count: usize) -> Result<(), SnapshotV2MemoryBindingError> {
    if (1..=NATIVE_V2_MEMORY_MAX_EXTENTS).contains(&count) {
        Ok(())
    } else {
        Err(SnapshotV2MemoryBindingError::CountOutOfBounds)
    }
}

fn validate_extents(
    extents: &[SnapshotV2MemoryExtent],
    file_length: u64,
) -> Result<(), SnapshotV2MemoryBindingError> {
    validate_extent_count(extents.len())?;
    let mut previous = None;
    let mut expected_file_offset = NATIVE_V2_MEMORY_ALIGNMENT;
    let mut total_data = 0_u64;
    for extent in extents {
        validate_range(extent.range())?;
        if previous.is_some_and(|previous: GuestMemoryRange| {
            extent.range().start() <= previous.start() || previous.overlaps(extent.range())
        }) {
            return Err(SnapshotV2MemoryBindingError::InvalidExtentTopology);
        }
        if extent.file_offset() != expected_file_offset {
            return Err(SnapshotV2MemoryBindingError::NonCanonicalFileOffset);
        }
        total_data = total_data
            .checked_add(extent.range().size())
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        if total_data > aarch64::DRAM_MEM_MAX_SIZE {
            return Err(SnapshotV2MemoryBindingError::DataTooLarge);
        }
        let end = extent
            .file_offset()
            .checked_add(extent.range().size())
            .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
        expected_file_offset = align_up(end, NATIVE_V2_MEMORY_ALIGNMENT)?;
        previous = Some(extent.range());
    }
    let last = extents
        .last()
        .ok_or(SnapshotV2MemoryBindingError::CountOutOfBounds)?;
    let expected_length = last
        .file_offset()
        .checked_add(last.range().size())
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    if file_length != expected_length {
        return Err(SnapshotV2MemoryBindingError::FileLengthMismatch);
    }
    Ok(())
}

fn validate_range(range: GuestMemoryRange) -> Result<(), SnapshotV2MemoryBindingError> {
    if !range
        .start()
        .raw_value()
        .is_multiple_of(NATIVE_V2_MEMORY_GUEST_GRANULE)
        || !range.size().is_multiple_of(NATIVE_V2_MEMORY_GUEST_GRANULE)
    {
        Err(SnapshotV2MemoryBindingError::InvalidExtent)
    } else {
        Ok(())
    }
}

fn align_up(value: u64, alignment: u64) -> Result<u64, SnapshotV2MemoryBindingError> {
    let mask = alignment
        .checked_sub(1)
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)
}

fn binding_checksum(bytes: &[u8]) -> Result<u64, SnapshotV2MemoryBindingError> {
    let before = bytes
        .get(..CHECKSUM_OFFSET)
        .ok_or(SnapshotV2MemoryBindingError::InvalidLength)?;
    let after = bytes
        .get(CHECKSUM_OFFSET + size_of::<u64>()..)
        .ok_or(SnapshotV2MemoryBindingError::InvalidLength)?;
    let checksum = crc64(0, before);
    let checksum = crc64(checksum, &[0_u8; size_of::<u64>()]);
    Ok(crc64(checksum, after))
}

fn generate_image_id() -> Result<SnapshotV2MemoryImageId, SnapshotV2MemoryWriteError> {
    let mut bytes = [0_u8; IMAGE_ID_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| SnapshotV2MemoryWriteError::IdentityUnavailable)?;
    Ok(SnapshotV2MemoryImageId(bytes))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2MemoryBindingError> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2MemoryBindingError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2MemoryBindingError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2MemoryBindingError> {
    let end = offset
        .checked_add(LENGTH)
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    let source = bytes
        .get(offset..end)
        .ok_or(SnapshotV2MemoryBindingError::InvalidLength)?;
    let mut value = [0_u8; LENGTH];
    value.copy_from_slice(source);
    Ok(value)
}

fn replace_u64(
    bytes: &mut [u8],
    offset: usize,
    value: u64,
) -> Result<(), SnapshotV2MemoryBindingError> {
    let end = offset
        .checked_add(size_of::<u64>())
        .ok_or(SnapshotV2MemoryBindingError::LengthOverflow)?;
    bytes
        .get_mut(offset..end)
        .ok_or(SnapshotV2MemoryBindingError::InvalidLength)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests;
