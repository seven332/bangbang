//! Checked page selection, streaming, and detached verification for Diff layers.

use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryRange, aarch64};
use crate::snapshot_memory_v2::{
    NATIVE_V2_MEMORY_GUEST_GRANULE, NATIVE_V2_MEMORY_MAX_EXTENTS, SnapshotV2MemoryBindingError,
    SnapshotV2MemoryImageId, snapshot_v2_memory_binding_from_memory_with_version_and_id,
};

use super::{
    NATIVE_V2_DIFF_MAX_EXTENTS, NATIVE_V2_DIFF_MAX_METADATA_BYTES,
    NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION, SnapshotV2DiffBase, SnapshotV2DiffLayerBinding,
    SnapshotV2DiffLayerBindingError,
};

const COPY_CHUNK_BYTES: usize = 1024 * 1024;
const ZERO_CHUNK_BYTES: usize = 8192;
const IMAGE_ID_BYTES: usize = 16;
const REDACTED: &str = "<redacted>";

/// A checked, canonical set of current guest-memory ranges for one Diff layer.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2DiffSelection {
    topology: Vec<GuestMemoryRange>,
    ranges: Vec<GuestMemoryRange>,
}

impl SnapshotV2DiffSelection {
    /// Selects every byte in every current active guest-memory region.
    pub fn all_current(memory: &GuestMemory) -> Result<Self, SnapshotV2DiffSelectionError> {
        let topology = capture_topology(memory)?;
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(topology.len())
            .map_err(|source| SnapshotV2DiffSelectionError::MetadataAllocationFailed { source })?;
        ranges.extend(topology.iter().copied());
        Ok(Self { topology, ranges })
    }

    /// Normalizes arbitrary mapped ranges outward to canonical 4-KiB guest pages.
    pub fn try_from_ranges(
        memory: &GuestMemory,
        ranges: &[GuestMemoryRange],
    ) -> Result<Self, SnapshotV2DiffSelectionError> {
        Self::try_from_source_ranges(memory, ranges.iter().copied().map(Ok))
    }

    /// Normalizes host/source dirty-page starts to canonical 4-KiB guest pages.
    pub fn try_from_dirty_pages(
        memory: &GuestMemory,
        source_page_size: u64,
        dirty_pages: &[GuestAddress],
    ) -> Result<Self, SnapshotV2DiffSelectionError> {
        if source_page_size < NATIVE_V2_MEMORY_GUEST_GRANULE
            || !source_page_size.is_power_of_two()
            || !source_page_size.is_multiple_of(NATIVE_V2_MEMORY_GUEST_GRANULE)
        {
            return Err(SnapshotV2DiffSelectionError::InvalidSourcePageSize);
        }
        Self::try_from_source_ranges(
            memory,
            dirty_pages.iter().copied().map(|address| {
                if !address.raw_value().is_multiple_of(source_page_size) {
                    return Err(SnapshotV2DiffSelectionError::UnalignedDirtyPage);
                }
                GuestMemoryRange::new(address, source_page_size)
                    .map_err(|_| SnapshotV2DiffSelectionError::AddressOverflow)
            }),
        )
    }

    /// Returns the final canonical ranges whose bytes will be written.
    pub fn ranges(&self) -> &[GuestMemoryRange] {
        &self.ranges
    }

    fn try_from_source_ranges<I>(
        memory: &GuestMemory,
        source_ranges: I,
    ) -> Result<Self, SnapshotV2DiffSelectionError>
    where
        I: IntoIterator<Item = Result<GuestMemoryRange, SnapshotV2DiffSelectionError>>,
    {
        let topology = capture_topology(memory)?;
        let mut touched = Vec::new();
        touched
            .try_reserve_exact(topology.len())
            .map_err(|source| SnapshotV2DiffSelectionError::MetadataAllocationFailed { source })?;
        touched.resize(topology.len(), false);

        let mut fragments = Vec::new();
        for source in source_ranges {
            let normalized = normalize_outward(source?)?;
            append_normalized_range(&topology, normalized, &mut touched, &mut fragments)?;
        }
        fragments.sort_unstable_by_key(|fragment| {
            (fragment.region_index, fragment.range.start().raw_value())
        });

        let mut canonical: Vec<SelectedRange> = Vec::new();
        canonical
            .try_reserve_exact(fragments.len())
            .map_err(|source| SnapshotV2DiffSelectionError::MetadataAllocationFailed { source })?;
        for fragment in fragments {
            if let Some(previous) = canonical.last_mut()
                && previous.region_index == fragment.region_index
                && fragment.range.start() <= previous.range.end_exclusive()
            {
                let end = previous
                    .range
                    .end_exclusive()
                    .raw_value()
                    .max(fragment.range.end_exclusive().raw_value());
                previous.range = GuestMemoryRange::new(
                    previous.range.start(),
                    end.checked_sub(previous.range.start().raw_value())
                        .ok_or(SnapshotV2DiffSelectionError::AddressOverflow)?,
                )
                .map_err(|_| SnapshotV2DiffSelectionError::AddressOverflow)?;
                continue;
            }
            canonical.push(fragment);
        }

        let mut ranges = Vec::new();
        if canonical.len() > NATIVE_V2_DIFF_MAX_EXTENTS {
            let touched_count = touched.iter().filter(|selected| **selected).count();
            ranges.try_reserve_exact(touched_count).map_err(|source| {
                SnapshotV2DiffSelectionError::MetadataAllocationFailed { source }
            })?;
            for (range, selected) in topology.iter().copied().zip(touched.iter().copied()) {
                if selected {
                    ranges.push(range);
                }
            }
        } else {
            ranges
                .try_reserve_exact(canonical.len())
                .map_err(
                    |source| SnapshotV2DiffSelectionError::MetadataAllocationFailed { source },
                )?;
            ranges.extend(canonical.into_iter().map(|selected| selected.range));
        }

        Ok(Self { topology, ranges })
    }

    fn matches_memory_topology(&self, memory: &GuestMemory) -> bool {
        self.topology.len() == memory.regions().len()
            && self
                .topology
                .iter()
                .copied()
                .zip(memory.regions().iter().map(|region| region.range()))
                .all(|(selected, current)| selected == current)
    }
}

impl fmt::Debug for SnapshotV2DiffSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffSelection")
            .field("topology", &REDACTED)
            .field("selected", &REDACTED)
            .field("region_count", &self.topology.len())
            .field("extent_count", &self.ranges.len())
            .finish()
    }
}

/// Failure while validating or normalizing a Diff page selection.
pub enum SnapshotV2DiffSelectionError {
    /// Current memory cannot form one exact native-v2 result topology.
    InvalidTopology,
    /// A source page size is not a supported multiple of the guest granule.
    InvalidSourcePageSize,
    /// A dirty-page start is not aligned to its declared source page size.
    UnalignedDirtyPage,
    /// Source or normalized guest-address arithmetic overflowed.
    AddressOverflow,
    /// A normalized byte is outside current active guest memory.
    OutOfTopology,
    /// Bounded selection metadata could not be allocated.
    MetadataAllocationFailed {
        /// The failed reservation.
        source: TryReserveError,
    },
}

impl fmt::Debug for SnapshotV2DiffSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SnapshotV2DiffSelectionError")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for SnapshotV2DiffSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidTopology => "native-v2 Diff selection topology is invalid",
            Self::InvalidSourcePageSize => "native-v2 Diff source page size is invalid",
            Self::UnalignedDirtyPage => "native-v2 Diff dirty page is unaligned",
            Self::AddressOverflow => "native-v2 Diff selection address arithmetic overflowed",
            Self::OutOfTopology => "native-v2 Diff selection is outside current memory",
            Self::MetadataAllocationFailed { .. } => {
                "native-v2 Diff selection metadata allocation failed"
            }
        })
    }
}

impl std::error::Error for SnapshotV2DiffSelectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::InvalidTopology
            | Self::InvalidSourcePageSize
            | Self::UnalignedDirtyPage
            | Self::AddressOverflow
            | Self::OutOfTopology => None,
        }
    }
}

#[derive(Clone, Copy)]
struct SelectedRange {
    range: GuestMemoryRange,
    region_index: usize,
}

fn capture_topology(
    memory: &GuestMemory,
) -> Result<Vec<GuestMemoryRange>, SnapshotV2DiffSelectionError> {
    let count = memory.regions().len();
    if !(1..=NATIVE_V2_MEMORY_MAX_EXTENTS).contains(&count) {
        return Err(SnapshotV2DiffSelectionError::InvalidTopology);
    }
    let mut topology = Vec::new();
    topology
        .try_reserve_exact(count)
        .map_err(|source| SnapshotV2DiffSelectionError::MetadataAllocationFailed { source })?;
    let mut previous = None;
    let mut total = 0_u64;
    for range in memory.regions().iter().map(|region| region.range()) {
        if range
            .validate_alignment(NATIVE_V2_MEMORY_GUEST_GRANULE)
            .is_err()
            || previous.is_some_and(|previous: GuestMemoryRange| {
                range.start() <= previous.start() || previous.overlaps(range)
            })
        {
            return Err(SnapshotV2DiffSelectionError::InvalidTopology);
        }
        total = total
            .checked_add(range.size())
            .ok_or(SnapshotV2DiffSelectionError::AddressOverflow)?;
        if total > aarch64::DRAM_MEM_MAX_SIZE {
            return Err(SnapshotV2DiffSelectionError::InvalidTopology);
        }
        topology.push(range);
        previous = Some(range);
    }
    Ok(topology)
}

fn normalize_outward(
    source: GuestMemoryRange,
) -> Result<GuestMemoryRange, SnapshotV2DiffSelectionError> {
    let mask = NATIVE_V2_MEMORY_GUEST_GRANULE - 1;
    let start = source.start().raw_value() & !mask;
    let end = source
        .end_exclusive()
        .raw_value()
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or(SnapshotV2DiffSelectionError::AddressOverflow)?;
    let size = end
        .checked_sub(start)
        .ok_or(SnapshotV2DiffSelectionError::AddressOverflow)?;
    GuestMemoryRange::new(GuestAddress::new(start), size)
        .map_err(|_| SnapshotV2DiffSelectionError::AddressOverflow)
}

fn append_normalized_range(
    topology: &[GuestMemoryRange],
    normalized: GuestMemoryRange,
    touched: &mut [bool],
    fragments: &mut Vec<SelectedRange>,
) -> Result<(), SnapshotV2DiffSelectionError> {
    let mut current = normalized.start().raw_value();
    let end = normalized.end_exclusive().raw_value();
    let mut region_index =
        topology.partition_point(|range| range.end_exclusive().raw_value() <= current);
    while current < end {
        let region = topology
            .get(region_index)
            .copied()
            .ok_or(SnapshotV2DiffSelectionError::OutOfTopology)?;
        if region.start().raw_value() > current {
            return Err(SnapshotV2DiffSelectionError::OutOfTopology);
        }
        let fragment_end = end.min(region.end_exclusive().raw_value());
        let range = GuestMemoryRange::new(
            GuestAddress::new(current),
            fragment_end
                .checked_sub(current)
                .ok_or(SnapshotV2DiffSelectionError::AddressOverflow)?,
        )
        .map_err(|_| SnapshotV2DiffSelectionError::AddressOverflow)?;
        fragments
            .try_reserve(1)
            .map_err(|source| SnapshotV2DiffSelectionError::MetadataAllocationFailed { source })?;
        fragments.push(SelectedRange {
            range,
            region_index,
        });
        let selected = touched
            .get_mut(region_index)
            .ok_or(SnapshotV2DiffSelectionError::InvalidTopology)?;
        *selected = true;
        current = fragment_end;
        region_index = region_index
            .checked_add(1)
            .ok_or(SnapshotV2DiffSelectionError::AddressOverflow)?;
    }
    Ok(())
}

/// Stable stage for cooperative Diff layer writing and redacted diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffWriteStage {
    /// Output-position and emptiness preflight.
    InitialPosition,
    /// Canonical layer metadata.
    Metadata,
    /// Zero padding between metadata and packed data.
    MetadataPadding,
    /// One selected packed data extent.
    Data {
        /// Zero-based canonical extent index.
        extent_index: usize,
    },
    /// Exact final output length.
    FinalLength,
}

impl fmt::Display for SnapshotV2DiffWriteStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialPosition => formatter.write_str("initial output preflight"),
            Self::Metadata => formatter.write_str("Diff layer metadata"),
            Self::MetadataPadding => formatter.write_str("Diff layer metadata padding"),
            Self::Data { extent_index } => write!(formatter, "Diff layer extent {extent_index}"),
            Self::FinalLength => formatter.write_str("final Diff layer length"),
        }
    }
}

/// Failure while streaming one canonical exact-2.13 Diff layer.
pub enum SnapshotV2DiffWriteError {
    /// The checked selection no longer matches current memory topology.
    TopologyChanged,
    /// The complete result-memory binding could not be constructed.
    ResultBinding {
        /// The redacted binding failure.
        source: SnapshotV2MemoryBindingError,
    },
    /// The exact layer binding or metadata could not be constructed.
    LayerBinding {
        /// The redacted layer failure.
        source: SnapshotV2DiffLayerBindingError,
    },
    /// A fresh result identity could not be obtained.
    IdentityUnavailable,
    /// The bounded guest-copy buffer could not be allocated.
    CopyBufferAllocationFailed {
        /// The failed reservation.
        source: TryReserveError,
    },
    /// The supplied output did not start at position zero.
    InvalidInitialPosition,
    /// The supplied output already contained bytes.
    NonEmptyOutput,
    /// A seek returned a different position than requested.
    PositionMismatch {
        /// The stable stage at which the mismatch occurred.
        stage: SnapshotV2DiffWriteStage,
    },
    /// Cooperative cancellation was observed before a stable stage.
    Cancelled {
        /// The stage that did not begin or complete.
        stage: SnapshotV2DiffWriteStage,
    },
    /// Output I/O failed without retaining a private error message.
    Io {
        /// The stable failing stage.
        stage: SnapshotV2DiffWriteStage,
        /// The public I/O classification.
        kind: io::ErrorKind,
    },
    /// Current guest memory could not supply a selected range.
    GuestMemoryRead {
        /// The redacted selected-data stage.
        stage: SnapshotV2DiffWriteStage,
    },
}

impl fmt::Debug for SnapshotV2DiffWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SnapshotV2DiffWriteError")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for SnapshotV2DiffWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TopologyChanged => {
                formatter.write_str("native-v2 Diff selection topology changed")
            }
            Self::ResultBinding { .. } => {
                formatter.write_str("native-v2 Diff result binding is invalid")
            }
            Self::LayerBinding { .. } => {
                formatter.write_str("native-v2 Diff layer binding is invalid")
            }
            Self::IdentityUnavailable => {
                formatter.write_str("native-v2 Diff result identity is unavailable")
            }
            Self::CopyBufferAllocationFailed { .. } => {
                formatter.write_str("native-v2 Diff copy-buffer allocation failed")
            }
            Self::InvalidInitialPosition => {
                formatter.write_str("native-v2 Diff output is not positioned at zero")
            }
            Self::NonEmptyOutput => formatter.write_str("native-v2 Diff output is not empty"),
            Self::PositionMismatch { stage } => {
                write!(
                    formatter,
                    "native-v2 Diff output position is invalid at {stage}"
                )
            }
            Self::Cancelled { stage } => {
                write!(
                    formatter,
                    "native-v2 Diff writing was cancelled before {stage}"
                )
            }
            Self::Io { stage, kind } => {
                write!(formatter, "native-v2 Diff I/O failed at {stage}: {kind}")
            }
            Self::GuestMemoryRead { stage } => {
                write!(
                    formatter,
                    "native-v2 Diff guest-memory read failed at {stage}"
                )
            }
        }
    }
}

impl std::error::Error for SnapshotV2DiffWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultBinding { source } => Some(source),
            Self::LayerBinding { source } => Some(source),
            Self::CopyBufferAllocationFailed { source } => Some(source),
            Self::TopologyChanged
            | Self::IdentityUnavailable
            | Self::InvalidInitialPosition
            | Self::NonEmptyOutput
            | Self::PositionMismatch { .. }
            | Self::Cancelled { .. }
            | Self::Io { .. }
            | Self::GuestMemoryRead { .. } => None,
        }
    }
}

/// Streams one canonical exact-2.13 Diff layer into an empty transaction sink.
///
/// The caller owns the sink transaction. An error may leave only a prefix in
/// that supplied sink; this function opens no path and publishes no output.
pub fn write_snapshot_v2_diff_layer<W: Write + Seek>(
    memory: &GuestMemory,
    writer: &mut W,
    base: SnapshotV2DiffBase,
    selection: &SnapshotV2DiffSelection,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffWriteError> {
    write_snapshot_v2_diff_layer_with_cancel(memory, writer, base, selection, |_| false)
}

/// Streams one canonical Diff layer with cooperative bounded-stage cancellation.
///
/// Cancellation has the same transaction-owned partial-sink behavior as
/// [`write_snapshot_v2_diff_layer`].
pub fn write_snapshot_v2_diff_layer_with_cancel<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    base: SnapshotV2DiffBase,
    selection: &SnapshotV2DiffSelection,
    is_cancelled: C,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2DiffWriteStage) -> bool,
{
    write_snapshot_v2_diff_layer_with_policy(
        memory,
        writer,
        base,
        selection,
        is_cancelled,
        SnapshotV2DiffWritePolicy {
            fill_identity: |identity: &mut [u8; IMAGE_ID_BYTES]| {
                getrandom::fill(identity).map_err(|_| ())
            },
            reserve_copy_buffer: |buffer: &mut Vec<u8>, additional| {
                buffer.try_reserve_exact(additional)
            },
            read_guest: |memory: &GuestMemory, destination: &mut [u8], address| {
                memory.read_slice(destination, address).map_err(|_| ())
            },
        },
    )
}

struct SnapshotV2DiffWritePolicy<I, R, G> {
    fill_identity: I,
    reserve_copy_buffer: R,
    read_guest: G,
}

fn write_snapshot_v2_diff_layer_with_policy<W, C, I, R, G>(
    memory: &GuestMemory,
    writer: &mut W,
    base: SnapshotV2DiffBase,
    selection: &SnapshotV2DiffSelection,
    mut is_cancelled: C,
    mut policy: SnapshotV2DiffWritePolicy<I, R, G>,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2DiffWriteStage) -> bool,
    I: FnMut(&mut [u8; IMAGE_ID_BYTES]) -> Result<(), ()>,
    R: FnMut(&mut Vec<u8>, usize) -> Result<(), TryReserveError>,
    G: FnMut(&GuestMemory, &mut [u8], GuestAddress) -> Result<(), ()>,
{
    check_write_cancelled(&mut is_cancelled, SnapshotV2DiffWriteStage::InitialPosition)?;
    if !selection.matches_memory_topology(memory) {
        return Err(SnapshotV2DiffWriteError::TopologyChanged);
    }
    preflight_empty_output(writer)?;

    let mut identity = [0_u8; IMAGE_ID_BYTES];
    (policy.fill_identity)(&mut identity)
        .map_err(|()| SnapshotV2DiffWriteError::IdentityUnavailable)?;
    let result = snapshot_v2_memory_binding_from_memory_with_version_and_id(
        memory,
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        SnapshotV2MemoryImageId::from_bytes(identity),
    )
    .map_err(|source| SnapshotV2DiffWriteError::ResultBinding { source })?;
    let binding = SnapshotV2DiffLayerBinding::try_from_ranges(base, result, selection.ranges())
        .map_err(|source| SnapshotV2DiffWriteError::LayerBinding { source })?;
    let metadata = binding
        .encode()
        .map_err(|source| SnapshotV2DiffWriteError::LayerBinding { source })?;

    let copy_length = binding
        .data_extents()
        .iter()
        .map(|extent| extent.range().size())
        .max()
        .unwrap_or(0)
        .min(COPY_CHUNK_BYTES as u64);
    let copy_length =
        usize::try_from(copy_length).map_err(|_| SnapshotV2DiffWriteError::LayerBinding {
            source: SnapshotV2DiffLayerBindingError::LengthOverflow,
        })?;
    let mut chunk = Vec::new();
    (policy.reserve_copy_buffer)(&mut chunk, copy_length)
        .map_err(|source| SnapshotV2DiffWriteError::CopyBufferAllocationFailed { source })?;
    chunk.resize(copy_length, 0);

    check_write_cancelled(&mut is_cancelled, SnapshotV2DiffWriteStage::Metadata)?;
    write_all_stage(writer, &metadata, SnapshotV2DiffWriteStage::Metadata)?;

    let zeroes = [0_u8; ZERO_CHUNK_BYTES];
    let metadata_length =
        u64::try_from(metadata.len()).map_err(|_| SnapshotV2DiffWriteError::LayerBinding {
            source: SnapshotV2DiffLayerBindingError::LengthOverflow,
        })?;
    let mut padding = binding.data_offset().checked_sub(metadata_length).ok_or(
        SnapshotV2DiffWriteError::LayerBinding {
            source: SnapshotV2DiffLayerBindingError::InvalidLength,
        },
    )?;
    while padding != 0 {
        check_write_cancelled(&mut is_cancelled, SnapshotV2DiffWriteStage::MetadataPadding)?;
        let length = usize::try_from(padding.min(ZERO_CHUNK_BYTES as u64)).map_err(|_| {
            SnapshotV2DiffWriteError::LayerBinding {
                source: SnapshotV2DiffLayerBindingError::LengthOverflow,
            }
        })?;
        let padding_chunk = zeroes
            .get(..length)
            .ok_or(SnapshotV2DiffWriteError::LayerBinding {
                source: SnapshotV2DiffLayerBindingError::LengthOverflow,
            })?;
        write_all_stage(
            writer,
            padding_chunk,
            SnapshotV2DiffWriteStage::MetadataPadding,
        )?;
        padding = padding.checked_sub(length as u64).ok_or({
            SnapshotV2DiffWriteError::LayerBinding {
                source: SnapshotV2DiffLayerBindingError::LengthOverflow,
            }
        })?;
    }

    for (extent_index, extent) in binding.data_extents().iter().copied().enumerate() {
        let stage = SnapshotV2DiffWriteStage::Data { extent_index };
        check_write_cancelled(&mut is_cancelled, stage)?;
        seek_write_exact(writer, extent.file_offset(), stage)?;
        let mut copied = 0_u64;
        while copied < extent.range().size() {
            check_write_cancelled(&mut is_cancelled, stage)?;
            let remaining = extent.range().size() - copied;
            let length = usize::try_from(remaining.min(COPY_CHUNK_BYTES as u64)).map_err(|_| {
                SnapshotV2DiffWriteError::LayerBinding {
                    source: SnapshotV2DiffLayerBindingError::LengthOverflow,
                }
            })?;
            let address = extent.range().start().checked_add(copied).ok_or({
                SnapshotV2DiffWriteError::LayerBinding {
                    source: SnapshotV2DiffLayerBindingError::LengthOverflow,
                }
            })?;
            let destination =
                chunk
                    .get_mut(..length)
                    .ok_or(SnapshotV2DiffWriteError::LayerBinding {
                        source: SnapshotV2DiffLayerBindingError::LengthOverflow,
                    })?;
            (policy.read_guest)(memory, destination, address)
                .map_err(|_| SnapshotV2DiffWriteError::GuestMemoryRead { stage })?;
            write_all_stage(writer, destination, stage)?;
            copied = copied.checked_add(length as u64).ok_or({
                SnapshotV2DiffWriteError::LayerBinding {
                    source: SnapshotV2DiffLayerBindingError::LengthOverflow,
                }
            })?;
        }
    }

    check_write_cancelled(&mut is_cancelled, SnapshotV2DiffWriteStage::FinalLength)?;
    let position = writer
        .stream_position()
        .map_err(|source| SnapshotV2DiffWriteError::Io {
            stage: SnapshotV2DiffWriteStage::FinalLength,
            kind: source.kind(),
        })?;
    if position != binding.file_length() {
        return Err(SnapshotV2DiffWriteError::PositionMismatch {
            stage: SnapshotV2DiffWriteStage::FinalLength,
        });
    }
    let end = writer
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotV2DiffWriteError::Io {
            stage: SnapshotV2DiffWriteStage::FinalLength,
            kind: source.kind(),
        })?;
    if end != binding.file_length() {
        return Err(SnapshotV2DiffWriteError::PositionMismatch {
            stage: SnapshotV2DiffWriteStage::FinalLength,
        });
    }
    Ok(binding)
}

fn preflight_empty_output<W: Seek>(writer: &mut W) -> Result<(), SnapshotV2DiffWriteError> {
    let initial = writer
        .stream_position()
        .map_err(|source| SnapshotV2DiffWriteError::Io {
            stage: SnapshotV2DiffWriteStage::InitialPosition,
            kind: source.kind(),
        })?;
    if initial != 0 {
        return Err(SnapshotV2DiffWriteError::InvalidInitialPosition);
    }
    let end = writer
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotV2DiffWriteError::Io {
            stage: SnapshotV2DiffWriteStage::InitialPosition,
            kind: source.kind(),
        })?;
    let rewind =
        writer
            .seek(SeekFrom::Start(0))
            .map_err(|source| SnapshotV2DiffWriteError::Io {
                stage: SnapshotV2DiffWriteStage::InitialPosition,
                kind: source.kind(),
            })?;
    if rewind != 0 {
        return Err(SnapshotV2DiffWriteError::PositionMismatch {
            stage: SnapshotV2DiffWriteStage::InitialPosition,
        });
    }
    if end != 0 {
        return Err(SnapshotV2DiffWriteError::NonEmptyOutput);
    }
    Ok(())
}

fn check_write_cancelled<C>(
    is_cancelled: &mut C,
    stage: SnapshotV2DiffWriteStage,
) -> Result<(), SnapshotV2DiffWriteError>
where
    C: FnMut(SnapshotV2DiffWriteStage) -> bool,
{
    if is_cancelled(stage) {
        Err(SnapshotV2DiffWriteError::Cancelled { stage })
    } else {
        Ok(())
    }
}

fn seek_write_exact<W: Seek>(
    writer: &mut W,
    position: u64,
    stage: SnapshotV2DiffWriteStage,
) -> Result<(), SnapshotV2DiffWriteError> {
    let actual =
        writer
            .seek(SeekFrom::Start(position))
            .map_err(|source| SnapshotV2DiffWriteError::Io {
                stage,
                kind: source.kind(),
            })?;
    if actual != position {
        return Err(SnapshotV2DiffWriteError::PositionMismatch { stage });
    }
    Ok(())
}

fn write_all_stage<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    stage: SnapshotV2DiffWriteStage,
) -> Result<(), SnapshotV2DiffWriteError> {
    writer
        .write_all(bytes)
        .map_err(|source| SnapshotV2DiffWriteError::Io {
            stage,
            kind: source.kind(),
        })
}

/// Stable stage for detached Diff layer output verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffVerifyStage {
    /// Exact complete file-length inspection.
    FileLength,
    /// Canonical metadata reading and decoding.
    Metadata,
    /// Zero metadata padding validation.
    MetadataPadding,
}

impl fmt::Display for SnapshotV2DiffVerifyStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileLength => formatter.write_str("Diff layer file length"),
            Self::Metadata => formatter.write_str("Diff layer metadata"),
            Self::MetadataPadding => formatter.write_str("Diff layer metadata padding"),
        }
    }
}

/// Failure while verifying a Diff layer against one detached binding.
pub enum SnapshotV2DiffVerifyError {
    /// The detached or decoded binding is invalid.
    Binding {
        /// The redacted binding failure.
        source: SnapshotV2DiffLayerBindingError,
    },
    /// The bounded metadata read buffer could not be allocated.
    MetadataAllocationFailed {
        /// The failed reservation.
        source: TryReserveError,
    },
    /// Input I/O failed without retaining a private error message.
    Io {
        /// The stable failing stage.
        stage: SnapshotV2DiffVerifyStage,
        /// The public I/O classification.
        kind: io::ErrorKind,
    },
    /// A seek returned a different position than requested.
    PositionMismatch {
        /// The stable failing stage.
        stage: SnapshotV2DiffVerifyStage,
    },
    /// The complete file length differs from the detached binding.
    FileLengthMismatch,
    /// Canonical metadata names a different layer than the detached binding.
    BindingMismatch,
    /// Metadata padding contains a nonzero byte.
    NonZeroPadding,
}

impl fmt::Debug for SnapshotV2DiffVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SnapshotV2DiffVerifyError")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for SnapshotV2DiffVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding { .. } => formatter.write_str("native-v2 Diff binding is invalid"),
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("native-v2 Diff verification allocation failed")
            }
            Self::Io { stage, kind } => {
                write!(
                    formatter,
                    "native-v2 Diff verification failed at {stage}: {kind}"
                )
            }
            Self::PositionMismatch { stage } => {
                write!(
                    formatter,
                    "native-v2 Diff input position is invalid at {stage}"
                )
            }
            Self::FileLengthMismatch => {
                formatter.write_str("native-v2 Diff file length does not match its binding")
            }
            Self::BindingMismatch => {
                formatter.write_str("native-v2 Diff metadata does not match its binding")
            }
            Self::NonZeroPadding => {
                formatter.write_str("native-v2 Diff metadata padding is noncanonical")
            }
        }
    }
}

impl std::error::Error for SnapshotV2DiffVerifyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding { source } => Some(source),
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::Io { .. }
            | Self::PositionMismatch { .. }
            | Self::FileLengthMismatch
            | Self::BindingMismatch
            | Self::NonZeroPadding => None,
        }
    }
}

/// Verifies canonical metadata, zero padding, and length against a detached binding.
pub fn verify_snapshot_v2_diff_layer_output<R: Read + Seek>(
    binding: &SnapshotV2DiffLayerBinding,
    reader: &mut R,
) -> Result<(), SnapshotV2DiffVerifyError> {
    verify_snapshot_v2_diff_layer_output_with_reserve(binding, reader, |buffer, additional| {
        buffer.try_reserve_exact(additional)
    })
}

fn verify_snapshot_v2_diff_layer_output_with_reserve<R, A>(
    binding: &SnapshotV2DiffLayerBinding,
    reader: &mut R,
    mut reserve_metadata: A,
) -> Result<(), SnapshotV2DiffVerifyError>
where
    R: Read + Seek,
    A: FnMut(&mut Vec<u8>, usize) -> Result<(), TryReserveError>,
{
    let expected = binding
        .encode()
        .map_err(|source| SnapshotV2DiffVerifyError::Binding { source })?;
    if expected.len() > NATIVE_V2_DIFF_MAX_METADATA_BYTES {
        return Err(SnapshotV2DiffVerifyError::Binding {
            source: SnapshotV2DiffLayerBindingError::MetadataTooLarge,
        });
    }
    let end = reader
        .seek(SeekFrom::End(0))
        .map_err(|source| SnapshotV2DiffVerifyError::Io {
            stage: SnapshotV2DiffVerifyStage::FileLength,
            kind: source.kind(),
        })?;
    if end != binding.file_length() {
        return Err(SnapshotV2DiffVerifyError::FileLengthMismatch);
    }
    seek_read_exact(reader, 0, SnapshotV2DiffVerifyStage::Metadata)?;

    let mut metadata = Vec::new();
    reserve_metadata(&mut metadata, expected.len())
        .map_err(|source| SnapshotV2DiffVerifyError::MetadataAllocationFailed { source })?;
    metadata.resize(expected.len(), 0);
    read_exact_stage(reader, &mut metadata, SnapshotV2DiffVerifyStage::Metadata)?;
    let decoded = SnapshotV2DiffLayerBinding::decode(&metadata)
        .map_err(|source| SnapshotV2DiffVerifyError::Binding { source })?;
    if metadata != expected || decoded != *binding {
        return Err(SnapshotV2DiffVerifyError::BindingMismatch);
    }

    let metadata_length =
        u64::try_from(metadata.len()).map_err(|_| SnapshotV2DiffVerifyError::Binding {
            source: SnapshotV2DiffLayerBindingError::LengthOverflow,
        })?;
    let mut padding = binding.data_offset().checked_sub(metadata_length).ok_or(
        SnapshotV2DiffVerifyError::Binding {
            source: SnapshotV2DiffLayerBindingError::InvalidLength,
        },
    )?;
    let mut buffer = [0_u8; ZERO_CHUNK_BYTES];
    while padding != 0 {
        let length = usize::try_from(padding.min(ZERO_CHUNK_BYTES as u64)).map_err(|_| {
            SnapshotV2DiffVerifyError::Binding {
                source: SnapshotV2DiffLayerBindingError::LengthOverflow,
            }
        })?;
        let destination = buffer
            .get_mut(..length)
            .ok_or(SnapshotV2DiffVerifyError::Binding {
                source: SnapshotV2DiffLayerBindingError::LengthOverflow,
            })?;
        read_exact_stage(
            reader,
            destination,
            SnapshotV2DiffVerifyStage::MetadataPadding,
        )?;
        if destination.iter().any(|byte| *byte != 0) {
            return Err(SnapshotV2DiffVerifyError::NonZeroPadding);
        }
        padding = padding.checked_sub(length as u64).ok_or({
            SnapshotV2DiffVerifyError::Binding {
                source: SnapshotV2DiffLayerBindingError::LengthOverflow,
            }
        })?;
    }
    Ok(())
}

fn seek_read_exact<R: Seek>(
    reader: &mut R,
    position: u64,
    stage: SnapshotV2DiffVerifyStage,
) -> Result<(), SnapshotV2DiffVerifyError> {
    let actual =
        reader
            .seek(SeekFrom::Start(position))
            .map_err(|source| SnapshotV2DiffVerifyError::Io {
                stage,
                kind: source.kind(),
            })?;
    if actual != position {
        return Err(SnapshotV2DiffVerifyError::PositionMismatch { stage });
    }
    Ok(())
}

fn read_exact_stage<R: Read>(
    reader: &mut R,
    destination: &mut [u8],
    stage: SnapshotV2DiffVerifyStage,
) -> Result<(), SnapshotV2DiffVerifyError> {
    reader
        .read_exact(destination)
        .map_err(|source| SnapshotV2DiffVerifyError::Io {
            stage,
            kind: source.kind(),
        })
}

#[cfg(test)]
mod tests;
