//! Dormant exact native-v2 2.13 differential-memory artifact contract.

use std::collections::TryReserveError;
use std::fmt;

use crate::memory::{GuestMemoryRange, aarch64};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_format_v2::{
    NATIVE_V2_DIFF_COMPONENT_KEY, SnapshotV2ComponentDisposition, SnapshotV2State,
};
use crate::snapshot_memory_v2::{
    NATIVE_V2_MEMORY_ALIGNMENT, NATIVE_V2_MEMORY_EXTENT_BYTES, NATIVE_V2_MEMORY_GUEST_GRANULE,
    NATIVE_V2_MEMORY_HEADER_BYTES, SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError,
    SnapshotV2MemoryImageId, SnapshotV2MemoryStateError, decode_snapshot_v2_memory_binding,
    decode_snapshot_v2_memory_binding_payload,
};

mod codec;
mod writer;

pub use writer::{
    SnapshotV2DiffSelection, SnapshotV2DiffSelectionError, SnapshotV2DiffVerifyError,
    SnapshotV2DiffVerifyStage, SnapshotV2DiffWriteError, SnapshotV2DiffWriteStage,
    verify_snapshot_v2_diff_layer_output, write_snapshot_v2_diff_layer,
    write_snapshot_v2_diff_layer_with_cancel,
};

#[cfg(test)]
mod tests;

/// Exact compatibility identity of the dormant Diff layer contract.
pub const NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 13, 0);

/// Fixed magic shared by the state component and layer-file metadata prefix.
pub const NATIVE_V2_DIFF_MAGIC: [u8; 8] = *b"BANGD2A\0";

/// Fixed profile number of the exact-2.13 Diff layer contract.
pub const NATIVE_V2_DIFF_PROFILE: u16 = 1;

/// Fixed exact-2.13 Diff layer header size.
pub const NATIVE_V2_DIFF_HEADER_BYTES: usize = 96;

/// Fixed encoded size of one selected GPA/data extent.
pub const NATIVE_V2_DIFF_EXTENT_BYTES: usize = 24;

/// Maximum selected data extents retained by one layer.
pub const NATIVE_V2_DIFF_MAX_EXTENTS: usize = 32_768;

/// Maximum encoded state-component and layer-metadata size.
pub const NATIVE_V2_DIFF_MAX_METADATA_BYTES: usize = 1024 * 1024;

const MAX_MEMORY_BINDING_BYTES: usize = NATIVE_V2_MEMORY_HEADER_BYTES
    + NATIVE_V2_MEMORY_EXTENT_BYTES * crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAX_EXTENTS;
const MAX_ENCODED_METADATA_BYTES: usize = NATIVE_V2_DIFF_HEADER_BYTES
    + MAX_MEMORY_BINDING_BYTES * 2
    + NATIVE_V2_DIFF_EXTENT_BYTES * NATIVE_V2_DIFF_MAX_EXTENTS;
const MAX_DATA_OFFSET: usize = align_up_usize_const(
    MAX_ENCODED_METADATA_BYTES,
    NATIVE_V2_MEMORY_ALIGNMENT as usize,
);
const REDACTED: &str = "<redacted>";

const _: () = assert!(MAX_MEMORY_BINDING_BYTES == 98_368);
const _: () = assert!(MAX_ENCODED_METADATA_BYTES == 983_264);
const _: () = assert!(MAX_DATA_OFFSET == NATIVE_V2_DIFF_MAX_METADATA_BYTES);
const _: () = assert!(MAX_DATA_OFFSET <= NATIVE_V2_DIFF_MAX_METADATA_BYTES);
const _: () = assert!(
    NATIVE_V2_DIFF_MAX_METADATA_BYTES
        < crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES
);
const _: () = assert!(NATIVE_V2_MEMORY_GUEST_GRANULE == 4096);

const fn align_up_usize_const(value: usize, alignment: usize) -> usize {
    (value + (alignment - 1)) & !(alignment - 1)
}

/// Required source of bytes omitted from one differential layer.
#[derive(Clone, PartialEq, Eq)]
pub enum SnapshotV2DiffBase {
    /// Omitted bytes inherit the zero-initialized boot root.
    Zero,
    /// Omitted bytes inherit one exact complete predecessor memory image.
    Image(SnapshotV2MemoryBinding),
}

impl SnapshotV2DiffBase {
    /// Returns the predecessor identity when this layer requires an image.
    pub const fn image_id(&self) -> Option<SnapshotV2MemoryImageId> {
        match self {
            Self::Zero => None,
            Self::Image(binding) => Some(binding.image_id()),
        }
    }

    /// Returns the complete predecessor binding when this is an image base.
    pub const fn binding(&self) -> Option<&SnapshotV2MemoryBinding> {
        match self {
            Self::Zero => None,
            Self::Image(binding) => Some(binding),
        }
    }
}

impl fmt::Debug for SnapshotV2DiffBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffBase")
            .field(
                "kind",
                &match self {
                    Self::Zero => "zero",
                    Self::Image(_) => "image",
                },
            )
            .field("identity", &REDACTED)
            .finish()
    }
}

/// One canonical selected GPA range and its packed layer-file position.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2DiffDataExtent {
    range: GuestMemoryRange,
    file_offset: u64,
}

impl SnapshotV2DiffDataExtent {
    /// Returns the selected guest-physical range.
    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    /// Returns the exact packed layer-file offset.
    pub const fn file_offset(self) -> u64 {
        self.file_offset
    }
}

impl fmt::Debug for SnapshotV2DiffDataExtent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffDataExtent")
            .field("mapping", &REDACTED)
            .finish()
    }
}

/// Fully validated exact-2.13 Diff layer metadata.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2DiffLayerBinding {
    version: SnapshotFormatVersion,
    base: SnapshotV2DiffBase,
    result: SnapshotV2MemoryBinding,
    data_extents: Vec<SnapshotV2DiffDataExtent>,
    metadata_length: u64,
    data_offset: u64,
    file_length: u64,
    metadata_checksum: u64,
}

impl SnapshotV2DiffLayerBinding {
    /// Builds one canonical layer binding from selected GPA ranges.
    pub fn try_from_ranges(
        base: SnapshotV2DiffBase,
        result: SnapshotV2MemoryBinding,
        ranges: &[GuestMemoryRange],
    ) -> Result<Self, SnapshotV2DiffLayerBindingError> {
        let mut reserve = FallibleReserve;
        Self::try_from_ranges_with_reserve(base, result, ranges, &mut reserve)
    }

    fn try_from_ranges_with_reserve<R: ReservePolicy>(
        base: SnapshotV2DiffBase,
        result: SnapshotV2MemoryBinding,
        ranges: &[GuestMemoryRange],
        reserve: &mut R,
    ) -> Result<Self, SnapshotV2DiffLayerBindingError> {
        validate_count(ranges.len())?;
        let layout = calculate_layout(&base, &result, ranges.len())?;
        let mut data_extents = Vec::new();
        reserve
            .reserve_extents(&mut data_extents, ranges.len())
            .map_err(
                |source| SnapshotV2DiffLayerBindingError::MetadataAllocationFailed { source },
            )?;
        let mut file_offset = layout.data_offset;
        for range in ranges.iter().copied() {
            data_extents.push(SnapshotV2DiffDataExtent { range, file_offset });
            file_offset = file_offset
                .checked_add(range.size())
                .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
        }
        let mut binding = build_binding(base, result, data_extents, layout, 0)?;
        let encoded = codec::encode_with_reserve(&binding, reserve)?;
        binding.metadata_checksum = codec::metadata_checksum(&encoded)?;
        Ok(binding)
    }

    /// Decodes and fully validates canonical layer metadata bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotV2DiffLayerBindingError> {
        codec::decode(bytes)
    }

    /// Encodes this binding canonically for state or a layer-file prefix.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotV2DiffLayerBindingError> {
        codec::encode(self)
    }

    /// Returns the exact Diff compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        self.version
    }

    /// Returns the required source of omitted bytes.
    pub const fn base(&self) -> &SnapshotV2DiffBase {
        &self.base
    }

    /// Returns the complete materialized result binding.
    pub const fn result(&self) -> &SnapshotV2MemoryBinding {
        &self.result
    }

    /// Returns the canonical selected GPA/data directory.
    pub fn data_extents(&self) -> &[SnapshotV2DiffDataExtent] {
        &self.data_extents
    }

    /// Returns the exact unpadded metadata length.
    pub const fn metadata_length(&self) -> u64 {
        self.metadata_length
    }

    /// Returns the first packed data byte offset.
    pub const fn data_offset(&self) -> u64 {
        self.data_offset
    }

    /// Returns the exact complete layer-file length.
    pub const fn file_length(&self) -> u64 {
        self.file_length
    }

    /// Returns the redacted metadata-integrity value to trusted validators.
    pub const fn metadata_checksum(&self) -> u64 {
        self.metadata_checksum
    }
}

impl fmt::Debug for SnapshotV2DiffLayerBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffLayerBinding")
            .field("version", &self.version)
            .field("base", &self.base)
            .field("result", &REDACTED)
            .field("extent_count", &self.data_extents.len())
            .field("metadata", &REDACTED)
            .finish()
    }
}

/// Construction or codec failure for exact-2.13 Diff layer metadata.
#[derive(Debug)]
pub enum SnapshotV2DiffLayerBindingError {
    /// The fixed layer magic does not match the exact profile.
    InvalidMagic,
    /// The encoded layer or result binding does not use exact version 2.13.
    UnsupportedVersion,
    /// A fixed header field or reserved byte is noncanonical.
    InvalidHeader,
    /// The base kind, identity, and embedded-binding length do not form a valid pair.
    InvalidBase,
    /// The fixed-header predecessor identity differs from its complete binding.
    PredecessorBindingMismatch,
    /// The predecessor and materialized result identities are equal.
    BaseResultIdentityConflict,
    /// The data extent count exceeds the fixed profile bound.
    CountOutOfBounds,
    /// A bounded metadata allocation failed.
    MetadataAllocationFailed {
        /// The failed reservation.
        source: TryReserveError,
    },
    /// The embedded complete result binding is invalid.
    ResultBinding {
        /// The complete-binding validation failure.
        source: SnapshotV2MemoryBindingError,
    },
    /// The embedded complete predecessor binding is invalid.
    PredecessorBinding {
        /// The complete-binding validation failure.
        source: SnapshotV2MemoryBindingError,
    },
    /// An encoded metadata, data, or file length is inconsistent.
    InvalidLength,
    /// Checked length or offset arithmetic overflowed.
    LengthOverflow,
    /// Encoded metadata exceeds the fixed one-MiB bound.
    MetadataTooLarge,
    /// The metadata checksum does not match the encoded bytes.
    IntegrityMismatch,
    /// A data range is empty, unaligned, or outside one result region.
    InvalidExtent,
    /// Data ranges overlap or are not strictly GPA ordered.
    InvalidExtentTopology,
    /// Adjacent data ranges within one result region were not coalesced.
    NonCanonicalExtent,
    /// A data extent does not use the next tightly packed file offset.
    NonCanonicalFileOffset,
    /// Selected data exceeds the architecture DRAM bound.
    DataTooLarge,
    /// The complete file length does not match the packed data directory.
    FileLengthMismatch,
}

impl fmt::Display for SnapshotV2DiffLayerBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidMagic => "native-v2 Diff layer magic is invalid",
            Self::UnsupportedVersion => "native-v2 Diff layer version is unsupported",
            Self::InvalidHeader => "native-v2 Diff layer header is noncanonical",
            Self::InvalidBase => "native-v2 Diff layer base is invalid",
            Self::PredecessorBindingMismatch => {
                "native-v2 Diff predecessor binding does not match its header"
            }
            Self::BaseResultIdentityConflict => {
                "native-v2 Diff layer base and result identities conflict"
            }
            Self::CountOutOfBounds => "native-v2 Diff layer extent count is out of bounds",
            Self::MetadataAllocationFailed { .. } => {
                "native-v2 Diff layer metadata allocation failed"
            }
            Self::ResultBinding { .. } => "native-v2 Diff result binding is invalid",
            Self::PredecessorBinding { .. } => "native-v2 Diff predecessor binding is invalid",
            Self::InvalidLength => "native-v2 Diff layer length is invalid",
            Self::LengthOverflow => "native-v2 Diff layer length arithmetic overflowed",
            Self::MetadataTooLarge => "native-v2 Diff layer metadata exceeds its limit",
            Self::IntegrityMismatch => "native-v2 Diff layer metadata integrity check failed",
            Self::InvalidExtent => "native-v2 Diff layer contains an invalid extent",
            Self::InvalidExtentTopology => "native-v2 Diff layer extent topology is invalid",
            Self::NonCanonicalExtent => "native-v2 Diff layer extents are not coalesced",
            Self::NonCanonicalFileOffset => "native-v2 Diff layer data offsets are noncanonical",
            Self::DataTooLarge => "native-v2 Diff layer data exceeds the architecture limit",
            Self::FileLengthMismatch => "native-v2 Diff layer file length is inconsistent",
        })
    }
}

impl std::error::Error for SnapshotV2DiffLayerBindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::ResultBinding { source } => Some(source),
            Self::PredecessorBinding { source } => Some(source),
            _ => None,
        }
    }
}

/// State/component failure for one exact-2.13 Diff binding.
#[derive(Debug)]
pub enum SnapshotV2DiffStateError {
    /// The outer state does not use exact version 2.13.
    UnsupportedVersion,
    /// The exact Diff singleton is absent.
    MissingDiffComponent,
    /// Kind 14 is duplicated or does not use semantic instance zero.
    InvalidDiffComponentProfile,
    /// The exact Diff layer payload is invalid.
    Layer {
        /// The layer validation failure.
        source: SnapshotV2DiffLayerBindingError,
    },
    /// The outer state does not contain one valid complete memory binding.
    Memory {
        /// The complete-memory state validation failure.
        source: SnapshotV2MemoryStateError,
    },
    /// Kind 1 and the Diff layer name different materialized results.
    ResultBindingMismatch,
}

impl fmt::Display for SnapshotV2DiffStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 Diff state version is unsupported",
            Self::MissingDiffComponent => "native-v2 Diff state component is missing",
            Self::InvalidDiffComponentProfile => {
                "native-v2 Diff state component profile is invalid"
            }
            Self::Layer { .. } => "native-v2 Diff state layer binding is invalid",
            Self::Memory { .. } => "native-v2 Diff state memory binding is invalid",
            Self::ResultBindingMismatch => {
                "native-v2 Diff state and layer result bindings do not match"
            }
        })
    }
}

impl std::error::Error for SnapshotV2DiffStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Layer { source } => Some(source),
            Self::Memory { source } => Some(source),
            _ => None,
        }
    }
}

/// Extracts and cross-validates the exact-2.13 Diff and result bindings.
pub fn decode_snapshot_v2_diff_layer_binding(
    state: &SnapshotV2State<'_>,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffStateError> {
    if state.metadata().version() != NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2DiffStateError::UnsupportedVersion);
    }
    let mut selected = None;
    for component in state.components() {
        if component.key().kind() != NATIVE_V2_DIFF_COMPONENT_KEY.kind() {
            continue;
        }
        if component.key() != NATIVE_V2_DIFF_COMPONENT_KEY
            || component.disposition() != SnapshotV2ComponentDisposition::Semantic
            || selected.is_some()
        {
            return Err(SnapshotV2DiffStateError::InvalidDiffComponentProfile);
        }
        selected = Some(component.payload());
    }
    let payload = selected.ok_or(SnapshotV2DiffStateError::MissingDiffComponent)?;
    let layer = SnapshotV2DiffLayerBinding::decode(payload)
        .map_err(|source| SnapshotV2DiffStateError::Layer { source })?;
    let result = decode_snapshot_v2_memory_binding(state)
        .map_err(|source| SnapshotV2DiffStateError::Memory { source })?;
    if layer.result() != &result {
        return Err(SnapshotV2DiffStateError::ResultBindingMismatch);
    }
    Ok(layer)
}

#[derive(Clone, Copy)]
struct CalculatedLayout {
    metadata_length: u64,
    data_offset: u64,
}

fn calculate_layout(
    base: &SnapshotV2DiffBase,
    result: &SnapshotV2MemoryBinding,
    extent_count: usize,
) -> Result<CalculatedLayout, SnapshotV2DiffLayerBindingError> {
    if result.version() != NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2DiffLayerBindingError::UnsupportedVersion);
    }
    validate_count(extent_count)?;
    let result_length = encoded_memory_binding_length(result)?;
    let predecessor_length = base
        .binding()
        .map(encoded_memory_binding_length)
        .transpose()?
        .unwrap_or(0);
    let metadata_length = NATIVE_V2_DIFF_HEADER_BYTES
        .checked_add(result_length)
        .and_then(|length| length.checked_add(predecessor_length))
        .and_then(|length| {
            extent_count
                .checked_mul(NATIVE_V2_DIFF_EXTENT_BYTES)
                .and_then(|extent_bytes| length.checked_add(extent_bytes))
        })
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    if metadata_length > NATIVE_V2_DIFF_MAX_METADATA_BYTES {
        return Err(SnapshotV2DiffLayerBindingError::MetadataTooLarge);
    }
    let metadata_length = u64::try_from(metadata_length)
        .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    let data_offset = align_up(metadata_length, NATIVE_V2_MEMORY_ALIGNMENT)?;
    Ok(CalculatedLayout {
        metadata_length,
        data_offset,
    })
}

fn encoded_memory_binding_length(
    binding: &SnapshotV2MemoryBinding,
) -> Result<usize, SnapshotV2DiffLayerBindingError> {
    NATIVE_V2_MEMORY_HEADER_BYTES
        .checked_add(
            binding
                .extents()
                .len()
                .checked_mul(NATIVE_V2_MEMORY_EXTENT_BYTES)
                .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)
}

fn build_binding(
    base: SnapshotV2DiffBase,
    result: SnapshotV2MemoryBinding,
    data_extents: Vec<SnapshotV2DiffDataExtent>,
    layout: CalculatedLayout,
    metadata_checksum: u64,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffLayerBindingError> {
    if result.version() != NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2DiffLayerBindingError::UnsupportedVersion);
    }
    if base
        .image_id()
        .is_some_and(|image_id| image_id == result.image_id())
    {
        return Err(SnapshotV2DiffLayerBindingError::BaseResultIdentityConflict);
    }
    validate_count(data_extents.len())?;
    let expected_layout = calculate_layout(&base, &result, data_extents.len())?;
    if layout.metadata_length != expected_layout.metadata_length
        || layout.data_offset != expected_layout.data_offset
    {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }

    let mut result_index = 0_usize;
    let mut previous: Option<(GuestMemoryRange, usize)> = None;
    let mut expected_file_offset = layout.data_offset;
    let mut total_data = 0_u64;
    for extent in &data_extents {
        validate_data_range(extent.range())?;
        while let Some(result_extent) = result.extents().get(result_index) {
            if result_extent.range().end_exclusive() <= extent.range().start() {
                result_index = result_index
                    .checked_add(1)
                    .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
            } else {
                break;
            }
        }
        let result_extent = result
            .extents()
            .get(result_index)
            .ok_or(SnapshotV2DiffLayerBindingError::InvalidExtent)?;
        if extent.range().start() < result_extent.range().start()
            || extent.range().end_exclusive() > result_extent.range().end_exclusive()
        {
            return Err(SnapshotV2DiffLayerBindingError::InvalidExtent);
        }
        if let Some((previous_range, previous_result_index)) = previous {
            if extent.range().start() <= previous_range.start()
                || previous_range.overlaps(extent.range())
            {
                return Err(SnapshotV2DiffLayerBindingError::InvalidExtentTopology);
            }
            if previous_result_index == result_index
                && previous_range.is_adjacent_to(extent.range())
            {
                return Err(SnapshotV2DiffLayerBindingError::NonCanonicalExtent);
            }
        }
        if extent.file_offset() != expected_file_offset {
            return Err(SnapshotV2DiffLayerBindingError::NonCanonicalFileOffset);
        }
        total_data = total_data
            .checked_add(extent.range().size())
            .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
        if total_data > aarch64::DRAM_MEM_MAX_SIZE {
            return Err(SnapshotV2DiffLayerBindingError::DataTooLarge);
        }
        expected_file_offset = expected_file_offset
            .checked_add(extent.range().size())
            .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
        previous = Some((extent.range(), result_index));
    }

    Ok(SnapshotV2DiffLayerBinding {
        version: NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        base,
        result,
        data_extents,
        metadata_length: layout.metadata_length,
        data_offset: layout.data_offset,
        file_length: expected_file_offset,
        metadata_checksum,
    })
}

fn validate_count(count: usize) -> Result<(), SnapshotV2DiffLayerBindingError> {
    if count <= NATIVE_V2_DIFF_MAX_EXTENTS {
        Ok(())
    } else {
        Err(SnapshotV2DiffLayerBindingError::CountOutOfBounds)
    }
}

fn validate_data_range(range: GuestMemoryRange) -> Result<(), SnapshotV2DiffLayerBindingError> {
    if !range
        .start()
        .raw_value()
        .is_multiple_of(NATIVE_V2_MEMORY_GUEST_GRANULE)
        || !range.size().is_multiple_of(NATIVE_V2_MEMORY_GUEST_GRANULE)
    {
        Err(SnapshotV2DiffLayerBindingError::InvalidExtent)
    } else {
        Ok(())
    }
}

fn align_up(value: u64, alignment: u64) -> Result<u64, SnapshotV2DiffLayerBindingError> {
    let mask = alignment
        .checked_sub(1)
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)
}

trait ReservePolicy {
    fn decode_memory_binding(
        &mut self,
        bytes: &[u8],
    ) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError>;

    fn reserve_extents(
        &mut self,
        value: &mut Vec<SnapshotV2DiffDataExtent>,
        additional: usize,
    ) -> Result<(), TryReserveError>;

    fn reserve_bytes(
        &mut self,
        value: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TryReserveError>;
}

struct FallibleReserve;

impl ReservePolicy for FallibleReserve {
    fn decode_memory_binding(
        &mut self,
        bytes: &[u8],
    ) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
        decode_snapshot_v2_memory_binding_payload(bytes)
    }

    fn reserve_extents(
        &mut self,
        value: &mut Vec<SnapshotV2DiffDataExtent>,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        value.try_reserve_exact(additional)
    }

    fn reserve_bytes(
        &mut self,
        value: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        value.try_reserve_exact(additional)
    }
}
