use std::collections::TryReserveError;
use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryAllocationError,
    GuestMemoryBacking, GuestMemoryRange,
};
use crate::memory_dirty::GuestMemoryDirtyTrackerError;
use crate::snapshot_memory_hotplug_v2_10::{
    PreparedSnapshotV2MemoryHotplugTopology, SnapshotV2MemoryHotplugExtentClass,
};

use super::{
    COPY_CHUNK_BYTES, SnapshotV2MemoryLoadError, SnapshotV2MemoryLoadStage,
    ValidatedSnapshotV2MemorySource,
};

/// Stable, value-redacted checkpoint in exact-2.10 memory materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryHotplugMaterializationStage {
    /// Validate the adopted memory-image descriptor and its bound metadata.
    SourceValidation,
    /// Collect the ordered private base-memory inventory.
    BaseInventory,
    /// Construct private File/COW mappings for all base extents.
    BaseMappings,
    /// Recheck the retained source after base mappings exist.
    BaseStability,
    /// Create the fresh shared reservation for the whole virtio-mem aperture.
    ApertureReservation,
    /// Insert one block-granular active view into the shared reservation.
    PluggedViews,
    /// Allocate the bounded positional-copy buffer.
    CopyBuffer,
    /// Copy one bounded chunk from a committed dynamic extent.
    DynamicCopy,
    /// Establish the clean destination dirty-tracking generation.
    DirtyTracking,
    /// Recheck the retained source after all destination bytes exist.
    FinalStability,
    /// Complete the transaction without another fallible ownership transition.
    Complete,
}

impl fmt::Display for SnapshotV2MemoryHotplugMaterializationStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SourceValidation => "source validation",
            Self::BaseInventory => "base inventory",
            Self::BaseMappings => "base mappings",
            Self::BaseStability => "base stability",
            Self::ApertureReservation => "aperture reservation",
            Self::PluggedViews => "plugged views",
            Self::CopyBuffer => "copy buffer",
            Self::DynamicCopy => "dynamic copy",
            Self::DirtyTracking => "dirty tracking",
            Self::FinalStability => "final stability",
            Self::Complete => "completion",
        })
    }
}

/// Failure while constructing unpublished mixed exact-2.10 guest memory.
pub enum SnapshotV2MemoryHotplugMaterializationError {
    /// The caller cancelled at a stable, value-redacted checkpoint.
    Cancelled {
        /// Checkpoint at which cancellation was observed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
    },
    /// The adopted native-v2 image failed validation or changed.
    Source {
        /// Checkpoint at which source validation failed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
        /// Existing native-v2 retained-source failure.
        source: SnapshotV2MemoryLoadError,
    },
    /// Bounded transaction metadata could not be reserved.
    MetadataAllocation {
        /// Checkpoint at which metadata allocation failed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
        /// Fallible collection reservation failure.
        source: TryReserveError,
    },
    /// Destination memory ownership could not be constructed.
    Memory {
        /// Checkpoint at which guest-memory construction failed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
        /// Guest-memory allocation or topology failure.
        source: GuestMemoryAllocationError,
    },
    /// A bounded positional source read failed or ended early.
    Read {
        /// Copy checkpoint at which reading failed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
        /// Stable I/O failure class without a path or descriptor.
        kind: io::ErrorKind,
    },
    /// A copied chunk could not be written into active destination memory.
    Write {
        /// Copy checkpoint at which the destination write failed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
        /// Guest-memory access failure.
        source: GuestMemoryAccessError,
    },
    /// The clean destination dirty generation could not be created.
    DirtyTracking {
        /// Checkpoint at which dirty metadata creation failed.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
        /// Dirty-tracker construction failure.
        source: GuestMemoryDirtyTrackerError,
    },
    /// Prepared topology or checked copy arithmetic was internally inconsistent.
    InvalidTopology {
        /// Checkpoint at which the inconsistency was detected.
        stage: SnapshotV2MemoryHotplugMaterializationStage,
    },
}

impl SnapshotV2MemoryHotplugMaterializationError {
    /// Returns the stable materialization checkpoint associated with the error.
    pub const fn stage(&self) -> SnapshotV2MemoryHotplugMaterializationStage {
        match self {
            Self::Cancelled { stage }
            | Self::Source { stage, .. }
            | Self::MetadataAllocation { stage, .. }
            | Self::Memory { stage, .. }
            | Self::Read { stage, .. }
            | Self::Write { stage, .. }
            | Self::DirtyTracking { stage, .. }
            | Self::InvalidTopology { stage } => *stage,
        }
    }
}

impl fmt::Debug for SnapshotV2MemoryHotplugMaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Cancelled { .. } => "cancelled",
            Self::Source { .. } => "source",
            Self::MetadataAllocation { .. } => "metadata allocation",
            Self::Memory { .. } => "memory",
            Self::Read { .. } => "read",
            Self::Write { .. } => "write",
            Self::DirtyTracking { .. } => "dirty tracking",
            Self::InvalidTopology { .. } => "invalid topology",
        };
        formatter
            .debug_struct("SnapshotV2MemoryHotplugMaterializationError")
            .field("stage", &self.stage())
            .field("kind", &kind)
            .field("details", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2MemoryHotplugMaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = match self {
            Self::Cancelled { .. } => "was cancelled",
            Self::Source { .. } => "source validation failed",
            Self::MetadataAllocation { .. } => "metadata allocation failed",
            Self::Memory { .. } => "memory construction failed",
            Self::Read { .. } => "source read failed",
            Self::Write { .. } => "destination write failed",
            Self::DirtyTracking { .. } => "dirty-tracking setup failed",
            Self::InvalidTopology { .. } => "found an invalid prepared topology",
        };
        write!(
            formatter,
            "native-v2 memory-hotplug materialization {action} during {}",
            self.stage()
        )
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugMaterializationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source { source, .. } => Some(source),
            Self::MetadataAllocation { source, .. } => Some(source),
            Self::Memory { source, .. } => Some(source),
            Self::Write { source, .. } => Some(source),
            Self::DirtyTracking { source, .. } => Some(source),
            Self::Cancelled { .. } | Self::Read { .. } | Self::InvalidTopology { .. } => None,
        }
    }
}

/// Materializes one prepared exact-2.10 topology from an adopted descriptor.
///
/// The result keeps base memory as private File/COW mappings and creates a
/// fresh descriptor-backed shared reservation for the virtio-mem aperture.
/// No path is opened and no platform or device authority is constructed.
pub fn materialize_snapshot_v2_memory_hotplug_file(
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    file: File,
) -> Result<GuestMemory, SnapshotV2MemoryHotplugMaterializationError> {
    materialize_snapshot_v2_memory_hotplug_file_with_cancel(topology, file, |_| false)
}

/// Materializes one prepared topology with stable cancellation checkpoints.
///
/// Repeated `PluggedViews` and `DynamicCopy` observations intentionally carry
/// no range, index, file offset, descriptor, or host-address value.
pub fn materialize_snapshot_v2_memory_hotplug_file_with_cancel<C>(
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    file: File,
    is_cancelled: C,
) -> Result<GuestMemory, SnapshotV2MemoryHotplugMaterializationError>
where
    C: FnMut(SnapshotV2MemoryHotplugMaterializationStage) -> bool,
{
    materialize_with_policy(
        topology,
        file,
        &mut SystemMaterializationPolicy { is_cancelled },
    )
}

trait MaterializationPolicy {
    fn checkpoint(
        &mut self,
        stage: SnapshotV2MemoryHotplugMaterializationStage,
    ) -> Result<(), SnapshotV2MemoryHotplugMaterializationError>;

    fn source_validation_hook(&mut self, _stage: SnapshotV2MemoryLoadStage, _file: &File) {}

    fn reserve_base_ranges(
        &mut self,
        ranges: &mut Vec<(GuestMemoryRange, u64)>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        ranges.try_reserve_exact(count)
    }

    fn reserve_copy_buffer(
        &mut self,
        buffer: &mut Vec<u8>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        buffer.try_reserve_exact(count)
    }

    fn map_base_memory(
        &mut self,
        ranges: &[(GuestMemoryRange, u64)],
        file: Arc<File>,
    ) -> Result<GuestMemory, GuestMemoryAllocationError> {
        GuestMemory::from_private_file_ranges(ranges, file, GuestMemoryBacking::Anonymous)
    }

    fn reserve_aperture(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryAllocationError> {
        memory.reserve_shared_region(range)
    }

    fn reserve_plugged_view_metadata(
        &mut self,
        memory: &mut GuestMemory,
        count: usize,
    ) -> Result<(), TryReserveError> {
        memory.try_reserve_region_metadata(count)
    }

    fn insert_view(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryAllocationError> {
        memory.insert_region(range)
    }

    fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        file.read_at(buffer, offset)
    }

    fn write_chunk(
        &mut self,
        memory: &mut GuestMemory,
        buffer: &[u8],
        address: GuestAddress,
    ) -> Result<(), GuestMemoryAccessError> {
        memory.write_slice(buffer, address)
    }

    fn enable_dirty_tracking(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<(), GuestMemoryDirtyTrackerError> {
        memory.enable_dirty_tracking().map(|_| ())
    }
}

struct SystemMaterializationPolicy<C> {
    is_cancelled: C,
}

impl<C> MaterializationPolicy for SystemMaterializationPolicy<C>
where
    C: FnMut(SnapshotV2MemoryHotplugMaterializationStage) -> bool,
{
    fn checkpoint(
        &mut self,
        stage: SnapshotV2MemoryHotplugMaterializationStage,
    ) -> Result<(), SnapshotV2MemoryHotplugMaterializationError> {
        if (self.is_cancelled)(stage) {
            Err(SnapshotV2MemoryHotplugMaterializationError::Cancelled { stage })
        } else {
            Ok(())
        }
    }
}

fn materialize_with_policy(
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    file: File,
    policy: &mut impl MaterializationPolicy,
) -> Result<GuestMemory, SnapshotV2MemoryHotplugMaterializationError> {
    use SnapshotV2MemoryHotplugMaterializationStage as Stage;

    policy.checkpoint(Stage::SourceValidation)?;
    let source = ValidatedSnapshotV2MemorySource::new_with_hook(
        topology.memory().binding(),
        file,
        |stage, file| policy.source_validation_hook(stage, file),
    )
    .map_err(
        |source| SnapshotV2MemoryHotplugMaterializationError::Source {
            stage: Stage::SourceValidation,
            source,
        },
    )?;

    policy.checkpoint(Stage::BaseInventory)?;
    let mut base_ranges = Vec::new();
    policy
        .reserve_base_ranges(&mut base_ranges, topology.memory().extent_count())
        .map_err(
            |source| SnapshotV2MemoryHotplugMaterializationError::MetadataAllocation {
                stage: Stage::BaseInventory,
                source,
            },
        )?;
    let mut has_dynamic_extents = false;
    for classified in topology.memory().classified_extents() {
        match classified.class() {
            SnapshotV2MemoryHotplugExtentClass::Base => {
                let extent = classified.extent();
                base_ranges.push((extent.range(), extent.file_offset()));
            }
            SnapshotV2MemoryHotplugExtentClass::Dynamic => has_dynamic_extents = true,
        }
    }

    policy.checkpoint(Stage::BaseMappings)?;
    let mut memory = if base_ranges.is_empty() {
        GuestMemory::empty_with_backing(GuestMemoryBacking::Anonymous)
    } else {
        policy
            .map_base_memory(&base_ranges, Arc::clone(source.file()))
            .map_err(
                |source| SnapshotV2MemoryHotplugMaterializationError::Memory {
                    stage: Stage::BaseMappings,
                    source,
                },
            )?
    };

    policy.checkpoint(Stage::BaseStability)?;
    source.verify_unchanged().map_err(|source| {
        SnapshotV2MemoryHotplugMaterializationError::Source {
            stage: Stage::BaseStability,
            source,
        }
    })?;

    let config = topology.state().config_space();
    let aperture = GuestMemoryRange::new(GuestAddress::new(config.addr()), config.region_size())
        .map_err(
            |_| SnapshotV2MemoryHotplugMaterializationError::InvalidTopology {
                stage: Stage::ApertureReservation,
            },
        )?;
    policy.checkpoint(Stage::ApertureReservation)?;
    policy
        .reserve_aperture(&mut memory, aperture)
        .map_err(
            |source| SnapshotV2MemoryHotplugMaterializationError::Memory {
                stage: Stage::ApertureReservation,
                source,
            },
        )?;

    let block_size = config.block_size();
    if block_size == 0 {
        return Err(
            SnapshotV2MemoryHotplugMaterializationError::InvalidTopology {
                stage: Stage::PluggedViews,
            },
        );
    }
    if !config.plugged_size().is_multiple_of(block_size) {
        return Err(
            SnapshotV2MemoryHotplugMaterializationError::InvalidTopology {
                stage: Stage::PluggedViews,
            },
        );
    }
    let plugged_view_count = usize::try_from(config.plugged_size() / block_size).map_err(|_| {
        SnapshotV2MemoryHotplugMaterializationError::InvalidTopology {
            stage: Stage::PluggedViews,
        }
    })?;
    policy
        .reserve_plugged_view_metadata(&mut memory, plugged_view_count)
        .map_err(
            |source| SnapshotV2MemoryHotplugMaterializationError::MetadataAllocation {
                stage: Stage::PluggedViews,
                source,
            },
        )?;
    if topology.plugged_ranges().is_empty() {
        policy.checkpoint(Stage::PluggedViews)?;
    }
    for range in topology.plugged_ranges().iter().copied() {
        let mut start = range.start().raw_value();
        while start < range.end_exclusive().raw_value() {
            policy.checkpoint(Stage::PluggedViews)?;
            let view =
                GuestMemoryRange::new(GuestAddress::new(start), block_size).map_err(|_| {
                    SnapshotV2MemoryHotplugMaterializationError::InvalidTopology {
                        stage: Stage::PluggedViews,
                    }
                })?;
            if view.end_exclusive().raw_value() > range.end_exclusive().raw_value() {
                return Err(
                    SnapshotV2MemoryHotplugMaterializationError::InvalidTopology {
                        stage: Stage::PluggedViews,
                    },
                );
            }
            policy.insert_view(&mut memory, view).map_err(|source| {
                SnapshotV2MemoryHotplugMaterializationError::Memory {
                    stage: Stage::PluggedViews,
                    source,
                }
            })?;
            start = view.end_exclusive().raw_value();
        }
    }

    if has_dynamic_extents {
        policy.checkpoint(Stage::CopyBuffer)?;
        let mut buffer = Vec::new();
        policy
            .reserve_copy_buffer(&mut buffer, COPY_CHUNK_BYTES)
            .map_err(
                |source| SnapshotV2MemoryHotplugMaterializationError::MetadataAllocation {
                    stage: Stage::CopyBuffer,
                    source,
                },
            )?;
        buffer.resize(COPY_CHUNK_BYTES, 0);

        for classified in topology.memory().classified_extents() {
            if classified.class() != SnapshotV2MemoryHotplugExtentClass::Dynamic {
                continue;
            }
            copy_dynamic_extent(
                &source,
                &mut memory,
                classified.extent().range(),
                classified.extent().file_offset(),
                &mut buffer,
                policy,
            )?;
        }
    }

    policy.checkpoint(Stage::DirtyTracking)?;
    policy
        .enable_dirty_tracking(&mut memory)
        .map_err(
            |source| SnapshotV2MemoryHotplugMaterializationError::DirtyTracking {
                stage: Stage::DirtyTracking,
                source,
            },
        )?;

    policy.checkpoint(Stage::FinalStability)?;
    source.verify_unchanged().map_err(|source| {
        SnapshotV2MemoryHotplugMaterializationError::Source {
            stage: Stage::FinalStability,
            source,
        }
    })?;
    policy.checkpoint(Stage::Complete)?;
    Ok(memory)
}

fn copy_dynamic_extent(
    source: &ValidatedSnapshotV2MemorySource,
    memory: &mut GuestMemory,
    range: GuestMemoryRange,
    file_offset: u64,
    buffer: &mut [u8],
    policy: &mut impl MaterializationPolicy,
) -> Result<(), SnapshotV2MemoryHotplugMaterializationError> {
    let stage = SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy;
    let mut copied = 0_u64;
    while copied < range.size() {
        policy.checkpoint(stage)?;
        let remaining = range.size() - copied;
        let buffer_length = u64::try_from(buffer.len())
            .map_err(|_| SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
        let chunk_length = usize::try_from(remaining.min(buffer_length))
            .map_err(|_| SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
        let chunk = buffer
            .get_mut(..chunk_length)
            .ok_or(SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
        let position = file_offset
            .checked_add(copied)
            .ok_or(SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
        read_exact_at(source.file().as_ref(), chunk, position, policy).map_err(|error| {
            SnapshotV2MemoryHotplugMaterializationError::Read {
                stage,
                kind: error.kind(),
            }
        })?;
        let destination = range
            .start()
            .checked_add(copied)
            .ok_or(SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
        policy
            .write_chunk(memory, chunk, destination)
            .map_err(
                |source| SnapshotV2MemoryHotplugMaterializationError::Write { stage, source },
            )?;
        let chunk_length = u64::try_from(chunk_length)
            .map_err(|_| SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
        copied = copied
            .checked_add(chunk_length)
            .ok_or(SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })?;
    }
    Ok(())
}

fn read_exact_at(
    file: &File,
    mut buffer: &mut [u8],
    mut offset: u64,
    policy: &mut impl MaterializationPolicy,
) -> io::Result<()> {
    while !buffer.is_empty() {
        match policy.read_at(file, buffer, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => {
                let count_u64 = u64::try_from(count)
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
                offset = offset
                    .checked_add(count_u64)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
                buffer = buffer
                    .get_mut(count..)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::memory::{
        GuestMemoryLayout, GuestMemoryOwnerProbe, GuestMemoryRegionBacking, aarch64,
    };
    use crate::snapshot_memory_hotplug_v2_10::{
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION, SnapshotV2MemoryHotplugState,
    };
    use crate::snapshot_memory_v2::write_snapshot_v2_memory_image_with_compatibility_version;

    use super::*;

    static NEXT_TEST_IMAGE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestImage {
        path: PathBuf,
    }

    impl TestImage {
        fn create(bytes: &[u8]) -> Self {
            let (image, mut file) = Self::create_empty();
            file.write_all(bytes).expect("test image should write");
            drop(file);
            image
        }

        fn create_empty() -> (Self, File) {
            let sequence = NEXT_TEST_IMAGE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-mixed-memory-{}-{sequence}.snap",
                std::process::id()
            ));
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("test image should create");
            (Self { path }, file)
        }

        fn open(&self) -> File {
            File::open(&self.path).expect("test image should open read-only")
        }
    }

    impl Drop for TestImage {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn fixture_bytes(hex: &str) -> Vec<u8> {
        let compact = hex.split_whitespace().collect::<String>();
        compact
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
                u8::from_str_radix(pair, 16).expect("fixture hex should decode")
            })
            .collect()
    }

    fn mixed_fixture() -> (PreparedSnapshotV2MemoryHotplugTopology, TestImage) {
        let state = SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(include_str!(
                "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
            )),
        )
        .expect("inactive exact 2.10 state should decode");
        let mut ranges = vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::DRAM_MEM_START),
                4 * aarch64::GUEST_PAGE_SIZE,
            )
            .expect("base range should validate"),
        ];
        let config = state.config_space();
        ranges.extend(state.plugged_ranges().map(|plugged| {
            let start = config.addr() + plugged.start_block() * config.block_size();
            GuestMemoryRange::new(
                GuestAddress::new(start),
                plugged.block_count() * config.block_size(),
            )
            .expect("plugged range should validate")
        }));
        ranges.sort_by_key(|range| range.start());
        let layout = GuestMemoryLayout::new(ranges).expect("mixed layout should validate");
        let memory = GuestMemory::allocate(&layout).expect("mixed memory should allocate");
        let mut image = Cursor::new(Vec::new());
        let binding = write_snapshot_v2_memory_image_with_compatibility_version(
            &memory,
            &mut image,
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        )
        .expect("mixed image should encode");
        let topology = PreparedSnapshotV2MemoryHotplugTopology::prepare(state, binding)
            .expect("mixed topology should prepare");
        (topology, TestImage::create(&image.into_inner()))
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FailurePoint {
        BaseMapping,
        Reservation,
        LaterView,
        LaterWrite,
        DirtyTracking,
        FinalStability,
    }

    struct OperationFailurePolicy {
        target: FailurePoint,
        view_count: usize,
        write_count: usize,
        probe: Option<GuestMemoryOwnerProbe>,
    }

    impl OperationFailurePolicy {
        fn new(target: FailurePoint) -> Self {
            Self {
                target,
                view_count: 0,
                write_count: 0,
                probe: None,
            }
        }

        fn capture(&mut self, memory: &GuestMemory) {
            self.probe = Some(
                memory
                    .try_owner_probe()
                    .expect("test owner probe should allocate"),
            );
        }
    }

    impl MaterializationPolicy for OperationFailurePolicy {
        fn checkpoint(
            &mut self,
            stage: SnapshotV2MemoryHotplugMaterializationStage,
        ) -> Result<(), SnapshotV2MemoryHotplugMaterializationError> {
            if self.target == FailurePoint::FinalStability
                && stage == SnapshotV2MemoryHotplugMaterializationStage::FinalStability
            {
                Err(SnapshotV2MemoryHotplugMaterializationError::InvalidTopology { stage })
            } else {
                Ok(())
            }
        }

        fn map_base_memory(
            &mut self,
            ranges: &[(GuestMemoryRange, u64)],
            file: Arc<File>,
        ) -> Result<GuestMemory, GuestMemoryAllocationError> {
            if self.target == FailurePoint::BaseMapping {
                Err(GuestMemoryAllocationError::InvalidHostPageSize)
            } else {
                GuestMemory::from_private_file_ranges(ranges, file, GuestMemoryBacking::Anonymous)
            }
        }

        fn reserve_aperture(
            &mut self,
            memory: &mut GuestMemory,
            range: GuestMemoryRange,
        ) -> Result<(), GuestMemoryAllocationError> {
            if self.target == FailurePoint::Reservation {
                self.capture(memory);
                Err(GuestMemoryAllocationError::ProtectedLazyMutation)
            } else {
                memory.reserve_shared_region(range)
            }
        }

        fn insert_view(
            &mut self,
            memory: &mut GuestMemory,
            range: GuestMemoryRange,
        ) -> Result<(), GuestMemoryAllocationError> {
            self.view_count += 1;
            if self.target == FailurePoint::LaterView && self.view_count == 2 {
                self.capture(memory);
                Err(GuestMemoryAllocationError::ProtectedLazyMutation)
            } else {
                memory.insert_region(range)
            }
        }

        fn write_chunk(
            &mut self,
            memory: &mut GuestMemory,
            buffer: &[u8],
            address: GuestAddress,
        ) -> Result<(), GuestMemoryAccessError> {
            self.write_count += 1;
            if self.target == FailurePoint::LaterWrite && self.write_count == 2 {
                self.capture(memory);
                Err(GuestMemoryAccessError::DirtyTrackingState)
            } else {
                memory.write_slice(buffer, address)
            }
        }

        fn enable_dirty_tracking(
            &mut self,
            memory: &mut GuestMemory,
        ) -> Result<(), GuestMemoryDirtyTrackerError> {
            if self.target == FailurePoint::DirtyTracking {
                self.capture(memory);
                let mut metadata = Vec::<u8>::new();
                let source = metadata
                    .try_reserve_exact(usize::MAX)
                    .expect_err("test dirty metadata allocation should fail");
                Err(GuestMemoryDirtyTrackerError::MetadataAllocationFailed { source })
            } else {
                memory.enable_dirty_tracking().map(|_| {
                    if self.target == FailurePoint::FinalStability {
                        self.capture(memory);
                    }
                })
            }
        }
    }

    #[test]
    fn injected_failures_release_every_accumulated_owner_and_allow_retry() {
        let (topology, image) = mixed_fixture();
        for (target, expected_stage) in [
            (
                FailurePoint::BaseMapping,
                SnapshotV2MemoryHotplugMaterializationStage::BaseMappings,
            ),
            (
                FailurePoint::Reservation,
                SnapshotV2MemoryHotplugMaterializationStage::ApertureReservation,
            ),
            (
                FailurePoint::LaterView,
                SnapshotV2MemoryHotplugMaterializationStage::PluggedViews,
            ),
            (
                FailurePoint::LaterWrite,
                SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy,
            ),
            (
                FailurePoint::DirtyTracking,
                SnapshotV2MemoryHotplugMaterializationStage::DirtyTracking,
            ),
            (
                FailurePoint::FinalStability,
                SnapshotV2MemoryHotplugMaterializationStage::FinalStability,
            ),
        ] {
            let mut policy = OperationFailurePolicy::new(target);
            let error = materialize_with_policy(&topology, image.open(), &mut policy)
                .expect_err("injected operation should fail");
            assert_eq!(error.stage(), expected_stage);
            if target != FailurePoint::BaseMapping {
                assert!(
                    policy
                        .probe
                        .as_ref()
                        .expect("failure after ownership should retain a weak probe")
                        .all_released(),
                    "every accumulated mapping should be released"
                );
            }
        }

        let memory = materialize_snapshot_v2_memory_hotplug_file(&topology, image.open())
            .expect("fresh materialization should succeed after injected rollback");
        assert!(
            memory
                .regions()
                .iter()
                .any(|region| region.backing() == GuestMemoryRegionBacking::PrivateFile)
        );
        assert!(
            memory
                .regions()
                .iter()
                .any(|region| region.backing() == GuestMemoryRegionBacking::Shared)
        );
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum AllocationFailurePoint {
        BaseInventory,
        PluggedViews,
        CopyBuffer,
    }

    struct AllocationFailurePolicy {
        target: AllocationFailurePoint,
        probe: Option<GuestMemoryOwnerProbe>,
    }

    impl MaterializationPolicy for AllocationFailurePolicy {
        fn checkpoint(
            &mut self,
            _stage: SnapshotV2MemoryHotplugMaterializationStage,
        ) -> Result<(), SnapshotV2MemoryHotplugMaterializationError> {
            Ok(())
        }

        fn reserve_base_ranges(
            &mut self,
            ranges: &mut Vec<(GuestMemoryRange, u64)>,
            count: usize,
        ) -> Result<(), TryReserveError> {
            if self.target == AllocationFailurePoint::BaseInventory {
                ranges.try_reserve_exact(usize::MAX)
            } else {
                ranges.try_reserve_exact(count)
            }
        }

        fn reserve_plugged_view_metadata(
            &mut self,
            memory: &mut GuestMemory,
            count: usize,
        ) -> Result<(), TryReserveError> {
            if self.target == AllocationFailurePoint::PluggedViews {
                self.probe = Some(
                    memory
                        .try_owner_probe()
                        .expect("test owner probe should allocate"),
                );
                let mut metadata = Vec::<u8>::new();
                Err(metadata
                    .try_reserve_exact(usize::MAX)
                    .expect_err("test plugged-view metadata allocation should fail"))
            } else {
                memory.try_reserve_region_metadata(count)
            }
        }

        fn insert_view(
            &mut self,
            memory: &mut GuestMemory,
            range: GuestMemoryRange,
        ) -> Result<(), GuestMemoryAllocationError> {
            memory.insert_region(range)?;
            if self.target == AllocationFailurePoint::CopyBuffer {
                self.probe = Some(
                    memory
                        .try_owner_probe()
                        .expect("test owner probe should allocate"),
                );
            }
            Ok(())
        }

        fn reserve_copy_buffer(
            &mut self,
            buffer: &mut Vec<u8>,
            count: usize,
        ) -> Result<(), TryReserveError> {
            if self.target == AllocationFailurePoint::CopyBuffer {
                buffer.try_reserve_exact(usize::MAX)
            } else {
                buffer.try_reserve_exact(count)
            }
        }
    }

    #[test]
    fn bounded_metadata_allocation_failures_are_staged_and_transactional() {
        let (topology, image) = mixed_fixture();
        for (target, expected_stage) in [
            (
                AllocationFailurePoint::BaseInventory,
                SnapshotV2MemoryHotplugMaterializationStage::BaseInventory,
            ),
            (
                AllocationFailurePoint::PluggedViews,
                SnapshotV2MemoryHotplugMaterializationStage::PluggedViews,
            ),
            (
                AllocationFailurePoint::CopyBuffer,
                SnapshotV2MemoryHotplugMaterializationStage::CopyBuffer,
            ),
        ] {
            let mut policy = AllocationFailurePolicy {
                target,
                probe: None,
            };
            let error = materialize_with_policy(&topology, image.open(), &mut policy)
                .expect_err("injected metadata allocation should fail");
            assert!(matches!(
                error,
                SnapshotV2MemoryHotplugMaterializationError::MetadataAllocation { stage, .. }
                    if stage == expected_stage
            ));
            if target != AllocationFailurePoint::BaseInventory {
                assert!(
                    policy
                        .probe
                        .as_ref()
                        .expect("copy-buffer failure should follow accumulated ownership")
                        .all_released()
                );
            }
        }
    }

    struct InterruptedReadPolicy {
        interrupted: bool,
    }

    impl MaterializationPolicy for InterruptedReadPolicy {
        fn checkpoint(
            &mut self,
            _stage: SnapshotV2MemoryHotplugMaterializationStage,
        ) -> Result<(), SnapshotV2MemoryHotplugMaterializationError> {
            Ok(())
        }

        fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                file.read_at(buffer, offset)
            }
        }
    }

    #[test]
    fn interrupted_positional_read_is_retried_without_changing_the_clean_result() {
        let (topology, image) = mixed_fixture();
        let mut policy = InterruptedReadPolicy { interrupted: false };
        let memory = materialize_with_policy(&topology, image.open(), &mut policy)
            .expect("interrupted read should retry");
        assert!(policy.interrupted);
        let tracker = memory
            .dirty_tracker()
            .expect("successful materialization should enable dirty tracking");
        assert_eq!(tracker.epoch(), 0);
        assert!(
            tracker
                .dirty_pages()
                .expect("clean dirty pages should query")
                .is_empty()
        );
    }

    struct FailingReadPolicy {
        read_count: usize,
        probe: Option<GuestMemoryOwnerProbe>,
    }

    impl MaterializationPolicy for FailingReadPolicy {
        fn checkpoint(
            &mut self,
            _stage: SnapshotV2MemoryHotplugMaterializationStage,
        ) -> Result<(), SnapshotV2MemoryHotplugMaterializationError> {
            Ok(())
        }

        fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
            self.read_count += 1;
            if self.read_count == 2 {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                file.read_at(buffer, offset)
            }
        }

        fn write_chunk(
            &mut self,
            memory: &mut GuestMemory,
            buffer: &[u8],
            address: GuestAddress,
        ) -> Result<(), GuestMemoryAccessError> {
            memory.write_slice(buffer, address)?;
            if self.probe.is_none() {
                self.probe = Some(
                    memory
                        .try_owner_probe()
                        .expect("test owner probe should allocate"),
                );
            }
            Ok(())
        }
    }

    #[test]
    fn positional_read_error_after_partial_population_releases_owners() {
        let (topology, image) = mixed_fixture();
        let mut policy = FailingReadPolicy {
            read_count: 0,
            probe: None,
        };
        let error = materialize_with_policy(&topology, image.open(), &mut policy)
            .expect_err("injected positional read should fail");
        assert!(matches!(
            error,
            SnapshotV2MemoryHotplugMaterializationError::Read {
                stage: SnapshotV2MemoryHotplugMaterializationStage::DynamicCopy,
                kind: io::ErrorKind::PermissionDenied,
            }
        ));
        assert!(
            policy
                .probe
                .as_ref()
                .expect("partial population should capture accumulated owners")
                .all_released()
        );
    }

    #[test]
    fn empty_plugged_topology_retains_only_private_base_and_an_offline_reservation() {
        let original = SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(include_str!(
                "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
            )),
        )
        .expect("inactive exact 2.10 state should decode");
        let (config, config_space, active_queue, bitmap, virtio, transport) = original.into_parts();
        let state = SnapshotV2MemoryHotplugState::try_new(
            config,
            config_space.with_plugged_size(0),
            active_queue,
            vec![0_u8; bitmap.len()],
            virtio,
            transport,
        )
        .expect("empty plugged topology should validate");
        let base = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            4 * aarch64::GUEST_PAGE_SIZE,
        )
        .expect("base range should validate");
        let layout = GuestMemoryLayout::new(vec![base]).expect("base layout should validate");
        let source = GuestMemory::allocate(&layout).expect("base memory should allocate");
        let mut image_bytes = Cursor::new(Vec::new());
        let binding = write_snapshot_v2_memory_image_with_compatibility_version(
            &source,
            &mut image_bytes,
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        )
        .expect("base image should encode");
        let topology = PreparedSnapshotV2MemoryHotplugTopology::prepare(state, binding)
            .expect("empty plugged topology should prepare");
        assert!(topology.plugged_ranges().is_empty());
        let image = TestImage::create(&image_bytes.into_inner());

        let memory = materialize_snapshot_v2_memory_hotplug_file(&topology, image.open())
            .expect("empty plugged topology should materialize");
        assert_eq!(memory.regions().len(), 1);
        assert_eq!(
            memory.regions()[0].backing(),
            GuestMemoryRegionBacking::PrivateFile
        );
        let config = topology.state().config_space();
        let aperture =
            GuestMemoryRange::new(GuestAddress::new(config.addr()), config.region_size())
                .expect("empty aperture should validate");
        memory
            .shared_reservation_capture_state(aperture)
            .expect("offline aperture reservation should remain owned");
        assert!(
            memory.read_slice(&mut [0], aperture.start()).is_err(),
            "offline aperture must remain absent from active memory"
        );
        assert!(
            memory
                .dirty_tracker()
                .expect("base dirty tracker should exist")
                .dirty_pages()
                .expect("base dirty pages should query")
                .is_empty()
        );
    }

    #[test]
    fn maximum_fragment_inventory_normalizes_into_block_granular_shared_views() {
        const MIB: u64 = 1024 * 1024;
        const FRAGMENT_BYTES: u64 = 16 * 1024;
        const EXTENT_COUNT: usize = 4096;

        let original = SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(include_str!(
                "../snapshot_memory_hotplug_v2_10/fixtures/inactive-mmio.hex"
            )),
        )
        .expect("inactive exact 2.10 state should decode");
        let (config, config_space, active_queue, _, virtio, transport) = original.into_parts();
        assert!(active_queue.is_none());
        let mut bitmap = vec![0_u8; 64];
        bitmap
            .get_mut(..4)
            .expect("first 32 blocks should fit the bitmap")
            .fill(u8::MAX);
        let state = SnapshotV2MemoryHotplugState::try_new(
            config,
            config_space.with_plugged_size(64 * MIB),
            active_queue,
            bitmap,
            virtio,
            transport,
        )
        .expect("maximum-fragment topology state should validate");

        let aperture_start = state.config_space().addr();
        let ranges = (0..EXTENT_COUNT)
            .map(|index| {
                let offset =
                    u64::try_from(index).expect("extent index should fit") * FRAGMENT_BYTES;
                GuestMemoryRange::new(GuestAddress::new(aperture_start + offset), FRAGMENT_BYTES)
                    .expect("fragment range should validate")
            })
            .collect::<Vec<_>>();
        let layout =
            GuestMemoryLayout::new(ranges.clone()).expect("fragment layout should validate");
        let mut source = GuestMemory::allocate(&layout).expect("fragment memory should allocate");
        for (index, range) in ranges.iter().copied().enumerate() {
            source
                .write_slice(
                    &[u8::try_from(index % 251).expect("sentinel should fit")],
                    range.start(),
                )
                .expect("fragment sentinel should write");
        }

        let (image, mut writer) = TestImage::create_empty();
        let binding = write_snapshot_v2_memory_image_with_compatibility_version(
            &source,
            &mut writer,
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        )
        .expect("maximum-fragment image should encode");
        writer.flush().expect("maximum-fragment image should flush");
        drop(writer);
        assert_eq!(binding.extents().len(), EXTENT_COUNT);
        let topology = PreparedSnapshotV2MemoryHotplugTopology::prepare(state, binding)
            .expect("maximum-fragment topology should prepare");
        assert_eq!(topology.memory().extent_count(), EXTENT_COUNT);
        assert_eq!(topology.plugged_ranges().len(), 1);

        let memory = materialize_snapshot_v2_memory_hotplug_file(&topology, image.open())
            .expect("maximum-fragment image should materialize");
        assert_eq!(memory.regions().len(), 32);
        assert!(
            memory
                .regions()
                .iter()
                .all(|region| region.backing() == GuestMemoryRegionBacking::Shared)
        );
        for index in [0, 127, 2048, EXTENT_COUNT - 1] {
            let mut actual = [0_u8; 1];
            memory
                .read_slice(&mut actual, ranges[index].start())
                .expect("materialized sentinel should read");
            assert_eq!(
                actual[0],
                u8::try_from(index % 251).expect("sentinel should fit")
            );
        }
    }
}
