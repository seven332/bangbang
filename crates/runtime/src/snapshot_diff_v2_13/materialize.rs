//! Immutable descriptor adoption and GPA-correct Diff materialization.

use std::collections::TryReserveError;
use std::fmt;
use std::fs::File;
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::FileExt;

use crate::snapshot_memory_v2::{
    FileFacts, NATIVE_V2_MEMORY_ALIGNMENT, NATIVE_V2_MEMORY_HEADER_BYTES, SnapshotV2MemoryBinding,
    SnapshotV2MemoryBindingError, SnapshotV2MemoryLoadError, ValidatedSnapshotV2MemorySource,
    inspect_file, inspect_file_facts, verify_snapshot_v2_memory_image_output,
};

use super::{
    NATIVE_V2_DIFF_HEADER_BYTES, NATIVE_V2_DIFF_MAX_METADATA_BYTES, SnapshotV2DiffBase,
    SnapshotV2DiffDataExtent, SnapshotV2DiffLayerBinding, SnapshotV2DiffLayerBindingError, codec,
};

const COPY_CHUNK_BYTES: usize = 1024 * 1024;
const ZERO_CHUNK_BYTES: usize = 8192;

/// One adopted base file accepted by next-layer materialization.
pub enum SnapshotV2DiffMaterializationBaseFile {
    /// A canonical complete `BANGM2A` predecessor image.
    Complete(File),
    /// A canonical exact-2.13 layer whose provenance is the zero root.
    ZeroRoot(File),
}

impl fmt::Debug for SnapshotV2DiffMaterializationBaseFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffMaterializationBaseFile")
            .field(
                "kind",
                &match self {
                    Self::Complete(_) => "complete",
                    Self::ZeroRoot(_) => "zero root",
                },
            )
            .field("descriptor", &"<redacted>")
            .finish()
    }
}

/// Stable, value-redacted checkpoint in exact-2.13 Diff materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffMaterializationStage {
    /// Validate immutable adopted source descriptors and their metadata.
    SourceValidation,
    /// Validate lineage and precompute every target GPA route.
    LineagePlanning,
    /// Validate the caller-owned empty read-write staging descriptor.
    OutputPreflight,
    /// Write the complete result image header.
    OutputHeader,
    /// Write canonical metadata padding and establish the result length.
    OutputPadding,
    /// Copy one bounded explicit or inherited data chunk.
    DataStreaming,
    /// Recheck all immutable source facts.
    SourceStability,
    /// Verify the complete staged result against its detached binding.
    ResultVerification,
    /// Complete without another fallible ownership transition.
    Complete,
}

impl fmt::Display for SnapshotV2DiffMaterializationStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SourceValidation => "source validation",
            Self::LineagePlanning => "lineage and route planning",
            Self::OutputPreflight => "output preflight",
            Self::OutputHeader => "output header",
            Self::OutputPadding => "output padding",
            Self::DataStreaming => "data streaming",
            Self::SourceStability => "source stability",
            Self::ResultVerification => "result verification",
            Self::Complete => "completion",
        })
    }
}

/// Failure while producing an unpublished complete image from Diff inputs.
pub enum SnapshotV2DiffMaterializationError {
    /// The caller cancelled at a stable, value-redacted checkpoint.
    Cancelled {
        /// Checkpoint at which cancellation was observed.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// A source descriptor failed immutable complete-image or file inspection.
    Source {
        /// Checkpoint at which source validation failed.
        stage: SnapshotV2DiffMaterializationStage,
        /// Existing native-v2 descriptor validation failure.
        source: SnapshotV2MemoryLoadError,
    },
    /// A layer's bounded canonical metadata failed validation.
    Layer {
        /// Checkpoint at which layer validation failed.
        stage: SnapshotV2DiffMaterializationStage,
        /// Exact-2.13 layer binding failure.
        source: SnapshotV2DiffLayerBindingError,
    },
    /// Bounded transaction metadata could not be reserved.
    MetadataAllocation {
        /// Checkpoint at which allocation failed.
        stage: SnapshotV2DiffMaterializationStage,
        /// Failed fallible reservation.
        source: TryReserveError,
    },
    /// Target complete-binding encoding failed before output began.
    ResultBinding {
        /// Checkpoint at which binding preparation failed.
        stage: SnapshotV2DiffMaterializationStage,
        /// Complete memory-binding failure.
        source: SnapshotV2MemoryBindingError,
    },
    /// A positional descriptor operation failed or made no progress.
    Io {
        /// Checkpoint at which I/O failed.
        stage: SnapshotV2DiffMaterializationStage,
        /// Stable I/O class without a path or descriptor number.
        kind: io::ErrorKind,
    },
    /// The layer descriptor length differs from its canonical binding.
    LayerFileLengthMismatch {
        /// Source-validation checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// Layer metadata-to-data padding contains a nonzero byte.
    NonZeroLayerPadding {
        /// Source-validation checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// Root promotion or a zero-root base used nonzero provenance.
    InvalidZeroRoot {
        /// Lineage checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// A next layer did not name one complete image predecessor.
    InvalidNextLayer {
        /// Lineage checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// The supplied base does not equal the next layer's embedded predecessor.
    PredecessorMismatch {
        /// Lineage checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// At least one target GPA has no explicit, inherited, or proven-zero source.
    MissingCoverage {
        /// Lineage checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// Checked route arithmetic or an internal canonical bound was inconsistent.
    InvalidRoute {
        /// Lineage checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// Staging is not an empty position-zero read-write CLOEXEC regular file.
    InvalidOutput {
        /// Output-preflight or final-position checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// Staging aliases one immutable source object.
    SourceOutputAlias {
        /// Output-preflight checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
    },
    /// The staged complete result failed detached output verification.
    ResultVerification {
        /// Result-verification checkpoint.
        stage: SnapshotV2DiffMaterializationStage,
        /// Complete image verification failure.
        source: SnapshotV2MemoryLoadError,
    },
}

impl SnapshotV2DiffMaterializationError {
    /// Returns the stable materialization checkpoint associated with the error.
    pub const fn stage(&self) -> SnapshotV2DiffMaterializationStage {
        match self {
            Self::Cancelled { stage }
            | Self::Source { stage, .. }
            | Self::Layer { stage, .. }
            | Self::MetadataAllocation { stage, .. }
            | Self::ResultBinding { stage, .. }
            | Self::Io { stage, .. }
            | Self::LayerFileLengthMismatch { stage }
            | Self::NonZeroLayerPadding { stage }
            | Self::InvalidZeroRoot { stage }
            | Self::InvalidNextLayer { stage }
            | Self::PredecessorMismatch { stage }
            | Self::MissingCoverage { stage }
            | Self::InvalidRoute { stage }
            | Self::InvalidOutput { stage }
            | Self::SourceOutputAlias { stage }
            | Self::ResultVerification { stage, .. } => *stage,
        }
    }
}

impl fmt::Debug for SnapshotV2DiffMaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Cancelled { .. } => "cancelled",
            Self::Source { .. } => "source",
            Self::Layer { .. } => "layer",
            Self::MetadataAllocation { .. } => "metadata allocation",
            Self::ResultBinding { .. } => "result binding",
            Self::Io { .. } => "I/O",
            Self::LayerFileLengthMismatch { .. } => "layer length",
            Self::NonZeroLayerPadding { .. } => "layer padding",
            Self::InvalidZeroRoot { .. } => "zero-root provenance",
            Self::InvalidNextLayer { .. } => "next-layer provenance",
            Self::PredecessorMismatch { .. } => "predecessor",
            Self::MissingCoverage { .. } => "coverage",
            Self::InvalidRoute { .. } => "route",
            Self::InvalidOutput { .. } => "output",
            Self::SourceOutputAlias { .. } => "alias",
            Self::ResultVerification { .. } => "verification",
        };
        formatter
            .debug_struct("SnapshotV2DiffMaterializationError")
            .field("stage", &self.stage())
            .field("kind", &kind)
            .field("details", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2DiffMaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = match self {
            Self::Cancelled { .. } => "was cancelled",
            Self::Source { .. } => "source validation failed",
            Self::Layer { .. } => "layer validation failed",
            Self::MetadataAllocation { .. } => "metadata allocation failed",
            Self::ResultBinding { .. } => "result binding preparation failed",
            Self::Io { .. } => "descriptor I/O failed",
            Self::LayerFileLengthMismatch { .. } => "found an invalid layer length",
            Self::NonZeroLayerPadding { .. } => "found noncanonical layer padding",
            Self::InvalidZeroRoot { .. } => "rejected zero-root provenance",
            Self::InvalidNextLayer { .. } => "rejected next-layer provenance",
            Self::PredecessorMismatch { .. } => "rejected a stale predecessor",
            Self::MissingCoverage { .. } => "found an uncovered target range",
            Self::InvalidRoute { .. } => "found invalid route arithmetic",
            Self::InvalidOutput { .. } => "rejected the staging descriptor",
            Self::SourceOutputAlias { .. } => "rejected a source/output alias",
            Self::ResultVerification { .. } => "result verification failed",
        };
        write!(
            formatter,
            "native-v2 Diff materialization {action} during {}",
            self.stage()
        )
    }
}

impl std::error::Error for SnapshotV2DiffMaterializationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source { source, .. } => Some(source),
            Self::Layer { source, .. } => Some(source),
            Self::MetadataAllocation { source, .. } => Some(source),
            Self::ResultBinding { source, .. } => Some(source),
            Self::ResultVerification { source, .. } => Some(source),
            Self::Cancelled { .. }
            | Self::Io { .. }
            | Self::LayerFileLengthMismatch { .. }
            | Self::NonZeroLayerPadding { .. }
            | Self::InvalidZeroRoot { .. }
            | Self::InvalidNextLayer { .. }
            | Self::PredecessorMismatch { .. }
            | Self::MissingCoverage { .. }
            | Self::InvalidRoute { .. }
            | Self::InvalidOutput { .. }
            | Self::SourceOutputAlias { .. } => None,
        }
    }
}

/// Promotes one immutable zero-root layer into a complete memory image.
pub fn promote_snapshot_v2_diff_zero_root_file(
    root: File,
    staging: &mut File,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError> {
    promote_snapshot_v2_diff_zero_root_file_with_cancel(root, staging, |_| false)
}

/// Promotes one zero-root layer with bounded cooperative cancellation.
pub fn promote_snapshot_v2_diff_zero_root_file_with_cancel<C>(
    root: File,
    staging: &mut File,
    is_cancelled: C,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError>
where
    C: FnMut(SnapshotV2DiffMaterializationStage) -> bool,
{
    promote_with_policy(
        root,
        staging,
        &mut SystemMaterializationPolicy { is_cancelled },
    )
}

/// Applies one immutable next layer to a complete or proven zero-root base.
pub fn apply_snapshot_v2_diff_layer_file(
    base: SnapshotV2DiffMaterializationBaseFile,
    next: File,
    staging: &mut File,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError> {
    apply_snapshot_v2_diff_layer_file_with_cancel(base, next, staging, |_| false)
}

/// Applies one next layer with bounded cooperative cancellation.
pub fn apply_snapshot_v2_diff_layer_file_with_cancel<C>(
    base: SnapshotV2DiffMaterializationBaseFile,
    next: File,
    staging: &mut File,
    is_cancelled: C,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError>
where
    C: FnMut(SnapshotV2DiffMaterializationStage) -> bool,
{
    apply_with_policy(
        base,
        next,
        staging,
        &mut SystemMaterializationPolicy { is_cancelled },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceRole {
    CompleteBase,
    ZeroRoot,
    NextLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceObservation {
    DuringValidation,
    BeforeOutput,
    AfterStreaming,
    Final,
}

trait MaterializationPolicy {
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError>;

    fn reserve_layer_metadata(
        &mut self,
        metadata: &mut Vec<u8>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        metadata.try_reserve_exact(count)
    }

    fn reserve_routes(
        &mut self,
        routes: &mut Vec<Route>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        routes.try_reserve_exact(count)
    }

    fn reserve_copy_buffer(
        &mut self,
        buffer: &mut Vec<u8>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        buffer.try_reserve_exact(count)
    }

    fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        file.read_at(buffer, offset)
    }

    fn write_at(&mut self, file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
        file.write_at(buffer, offset)
    }

    fn set_len(&mut self, file: &File, length: u64) -> io::Result<()> {
        file.set_len(length)
    }

    fn seek_output(&mut self, file: &mut File, position: u64) -> io::Result<u64> {
        file.seek(SeekFrom::Start(position))
    }

    fn source_hook(&mut self, _role: SourceRole, _observation: SourceObservation, _file: &File) {}

    fn result_verification_hook(&mut self, _file: &mut File) {}
}

struct SystemMaterializationPolicy<C> {
    is_cancelled: C,
}

impl<C> MaterializationPolicy for SystemMaterializationPolicy<C>
where
    C: FnMut(SnapshotV2DiffMaterializationStage) -> bool,
{
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        if (self.is_cancelled)(stage) {
            Err(SnapshotV2DiffMaterializationError::Cancelled { stage })
        } else {
            Ok(())
        }
    }
}

struct ValidatedLayerSource {
    file: File,
    binding: SnapshotV2DiffLayerBinding,
    facts: FileFacts,
    role: SourceRole,
}

impl ValidatedLayerSource {
    fn new(
        file: File,
        role: SourceRole,
        policy: &mut impl MaterializationPolicy,
    ) -> Result<Self, SnapshotV2DiffMaterializationError> {
        let stage = SnapshotV2DiffMaterializationStage::SourceValidation;
        let facts = inspect_file(&file)
            .map_err(|source| SnapshotV2DiffMaterializationError::Source { stage, source })?;
        if facts.length()
            < u64::try_from(NATIVE_V2_DIFF_HEADER_BYTES)
                .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?
        {
            return Err(SnapshotV2DiffMaterializationError::LayerFileLengthMismatch { stage });
        }

        let mut header = [0_u8; NATIVE_V2_DIFF_HEADER_BYTES];
        read_exact_at(&file, &mut header, 0, stage, policy)?;
        let metadata_length = codec::preflight_metadata_length(&header)
            .map_err(|source| SnapshotV2DiffMaterializationError::Layer { stage, source })?;
        if metadata_length > NATIVE_V2_DIFF_MAX_METADATA_BYTES
            || u64::try_from(metadata_length)
                .ok()
                .is_none_or(|length| length > facts.length())
        {
            return Err(SnapshotV2DiffMaterializationError::LayerFileLengthMismatch { stage });
        }

        let mut metadata = Vec::new();
        policy
            .reserve_layer_metadata(&mut metadata, metadata_length)
            .map_err(
                |source| SnapshotV2DiffMaterializationError::MetadataAllocation { stage, source },
            )?;
        metadata.resize(metadata_length, 0);
        read_exact_at(&file, &mut metadata, 0, stage, policy)?;
        let binding = SnapshotV2DiffLayerBinding::decode(&metadata)
            .map_err(|source| SnapshotV2DiffMaterializationError::Layer { stage, source })?;
        if binding.file_length() != facts.length() {
            return Err(SnapshotV2DiffMaterializationError::LayerFileLengthMismatch { stage });
        }

        let mut offset = binding.metadata_length();
        let mut zeroes = [0_u8; ZERO_CHUNK_BYTES];
        while offset < binding.data_offset() {
            let remaining = binding
                .data_offset()
                .checked_sub(offset)
                .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
            let length = usize::try_from(remaining.min(ZERO_CHUNK_BYTES as u64))
                .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
            let chunk = zeroes
                .get_mut(..length)
                .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
            read_exact_at(&file, chunk, offset, stage, policy)?;
            if chunk.iter().any(|byte| *byte != 0) {
                return Err(SnapshotV2DiffMaterializationError::NonZeroLayerPadding { stage });
            }
            offset = offset
                .checked_add(
                    u64::try_from(length)
                        .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?,
                )
                .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
        }

        policy.source_hook(role, SourceObservation::DuringValidation, &file);
        let after = inspect_file(&file)
            .map_err(|source| SnapshotV2DiffMaterializationError::Source { stage, source })?;
        if after != facts {
            return Err(SnapshotV2DiffMaterializationError::Source {
                stage,
                source: SnapshotV2MemoryLoadError::SourceChanged,
            });
        }
        Ok(Self {
            file,
            binding,
            facts,
            role,
        })
    }

    fn verify_unchanged(
        &self,
        observation: SourceObservation,
        policy: &mut impl MaterializationPolicy,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        let stage = SnapshotV2DiffMaterializationStage::SourceStability;
        policy.source_hook(self.role, observation, &self.file);
        let current = inspect_file(&self.file)
            .map_err(|source| SnapshotV2DiffMaterializationError::Source { stage, source })?;
        if current == self.facts {
            Ok(())
        } else {
            Err(SnapshotV2DiffMaterializationError::Source {
                stage,
                source: SnapshotV2MemoryLoadError::SourceChanged,
            })
        }
    }
}

enum ValidatedBase {
    Complete(ValidatedSnapshotV2MemorySource),
    ZeroRoot(ValidatedLayerSource),
}

impl ValidatedBase {
    fn facts(&self) -> FileFacts {
        match self {
            Self::Complete(source) => source.facts(),
            Self::ZeroRoot(source) => source.facts,
        }
    }

    fn verify_unchanged(
        &self,
        observation: SourceObservation,
        policy: &mut impl MaterializationPolicy,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        let stage = SnapshotV2DiffMaterializationStage::SourceStability;
        match self {
            Self::Complete(source) => {
                policy.source_hook(
                    SourceRole::CompleteBase,
                    observation,
                    source.file().as_ref(),
                );
                source
                    .verify_unchanged()
                    .map_err(|source| SnapshotV2DiffMaterializationError::Source { stage, source })
            }
            Self::ZeroRoot(source) => source.verify_unchanged(observation, policy),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteSource {
    Zero,
    CompleteBase,
    ZeroRoot,
    NextLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    source: RouteSource,
    source_offset: u64,
    target_offset: u64,
    length: u64,
}

enum Inheritance<'a> {
    Zero,
    Complete(&'a SnapshotV2MemoryBinding),
    ZeroRoot(&'a SnapshotV2DiffLayerBinding),
}

fn promote_with_policy(
    root: File,
    staging: &mut File,
    policy: &mut impl MaterializationPolicy,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError> {
    use SnapshotV2DiffMaterializationStage as Stage;

    policy.checkpoint(Stage::SourceValidation)?;
    let root = ValidatedLayerSource::new(root, SourceRole::ZeroRoot, policy)?;
    if !matches!(root.binding.base(), SnapshotV2DiffBase::Zero) {
        return Err(SnapshotV2DiffMaterializationError::InvalidZeroRoot {
            stage: Stage::LineagePlanning,
        });
    }

    policy.checkpoint(Stage::LineagePlanning)?;
    let target = root.binding.result().clone();
    let routes = build_routes(&target, &root.binding, Inheritance::Zero, policy)?;
    execute(target, routes, &root, None, staging, policy)
}

fn apply_with_policy(
    base: SnapshotV2DiffMaterializationBaseFile,
    next: File,
    staging: &mut File,
    policy: &mut impl MaterializationPolicy,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError> {
    use SnapshotV2DiffMaterializationStage as Stage;

    policy.checkpoint(Stage::SourceValidation)?;
    let next = ValidatedLayerSource::new(next, SourceRole::NextLayer, policy)?;
    let predecessor = next.binding.base().binding().ok_or(
        SnapshotV2DiffMaterializationError::InvalidNextLayer {
            stage: Stage::LineagePlanning,
        },
    )?;

    let base = match base {
        SnapshotV2DiffMaterializationBaseFile::Complete(file) => {
            let source =
                ValidatedSnapshotV2MemorySource::new_with_hook(predecessor, file, |_, file| {
                    policy.source_hook(
                        SourceRole::CompleteBase,
                        SourceObservation::DuringValidation,
                        file,
                    );
                })
                .map_err(|source| SnapshotV2DiffMaterializationError::Source {
                    stage: Stage::SourceValidation,
                    source,
                })?;
            ValidatedBase::Complete(source)
        }
        SnapshotV2DiffMaterializationBaseFile::ZeroRoot(file) => {
            let root = ValidatedLayerSource::new(file, SourceRole::ZeroRoot, policy)?;
            if !matches!(root.binding.base(), SnapshotV2DiffBase::Zero) {
                return Err(SnapshotV2DiffMaterializationError::InvalidZeroRoot {
                    stage: Stage::LineagePlanning,
                });
            }
            if root.binding.result() != predecessor {
                return Err(SnapshotV2DiffMaterializationError::PredecessorMismatch {
                    stage: Stage::LineagePlanning,
                });
            }
            ValidatedBase::ZeroRoot(root)
        }
    };

    policy.checkpoint(Stage::LineagePlanning)?;
    let target = next.binding.result().clone();
    let inheritance = match &base {
        ValidatedBase::Complete(_) => Inheritance::Complete(predecessor),
        ValidatedBase::ZeroRoot(root) => Inheritance::ZeroRoot(&root.binding),
    };
    let routes = build_routes(&target, &next.binding, inheritance, policy)?;
    execute(target, routes, &next, Some(&base), staging, policy)
}

fn build_routes(
    target: &SnapshotV2MemoryBinding,
    explicit: &SnapshotV2DiffLayerBinding,
    inheritance: Inheritance<'_>,
    policy: &mut impl MaterializationPolicy,
) -> Result<Vec<Route>, SnapshotV2DiffMaterializationError> {
    let stage = SnapshotV2DiffMaterializationStage::LineagePlanning;
    let predecessor_count = match &inheritance {
        Inheritance::Zero => 0,
        Inheritance::Complete(binding) => binding.extents().len(),
        Inheritance::ZeroRoot(root) => root.result().extents().len(),
    };
    let root_data_count = match &inheritance {
        Inheritance::ZeroRoot(root) => root.data_extents().len(),
        Inheritance::Zero | Inheritance::Complete(_) => 0,
    };
    let route_bound = target
        .extents()
        .len()
        .checked_add(explicit.data_extents().len())
        .and_then(|count| count.checked_add(predecessor_count))
        .and_then(|count| count.checked_add(root_data_count))
        .and_then(|count| count.checked_mul(2))
        .and_then(|count| count.checked_add(1))
        .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
    let mut routes = Vec::new();
    policy
        .reserve_routes(&mut routes, route_bound)
        .map_err(
            |source| SnapshotV2DiffMaterializationError::MetadataAllocation { stage, source },
        )?;

    let mut explicit_index = 0_usize;
    let mut predecessor_index = 0_usize;
    let mut root_data_index = 0_usize;
    for target_extent in target.extents().iter().copied() {
        let mut gpa = target_extent.range().start().raw_value();
        let target_end = target_extent.range().end_exclusive().raw_value();
        while gpa < target_end {
            advance_diff_extent(explicit.data_extents(), &mut explicit_index, gpa, stage)?;
            let explicit_extent = explicit.data_extents().get(explicit_index).copied();
            if explicit_extent.is_some_and(|extent| contains_diff(extent, gpa)) {
                let extent = explicit_extent
                    .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                let end = target_end.min(extent.range().end_exclusive().raw_value());
                append_route(
                    &mut routes,
                    route_bound,
                    RouteSource::NextLayer,
                    diff_source_offset(extent, gpa, stage)?,
                    target_offset(target_extent, gpa, stage)?,
                    checked_length(gpa, end, stage)?,
                    stage,
                )?;
                gpa = end;
                continue;
            }

            let explicit_start = explicit_extent
                .map(|extent| extent.range().start().raw_value())
                .unwrap_or(target_end)
                .min(target_end);
            match &inheritance {
                Inheritance::Zero => {
                    append_route(
                        &mut routes,
                        route_bound,
                        RouteSource::Zero,
                        0,
                        target_offset(target_extent, gpa, stage)?,
                        checked_length(gpa, explicit_start, stage)?,
                        stage,
                    )?;
                    gpa = explicit_start;
                }
                Inheritance::Complete(binding) => {
                    advance_memory_extent(binding, &mut predecessor_index, gpa, stage)?;
                    let predecessor = binding
                        .extents()
                        .get(predecessor_index)
                        .copied()
                        .ok_or(SnapshotV2DiffMaterializationError::MissingCoverage { stage })?;
                    if !contains_memory(predecessor, gpa) {
                        return Err(SnapshotV2DiffMaterializationError::MissingCoverage { stage });
                    }
                    let end = target_end
                        .min(explicit_start)
                        .min(predecessor.range().end_exclusive().raw_value());
                    append_route(
                        &mut routes,
                        route_bound,
                        RouteSource::CompleteBase,
                        memory_source_offset(predecessor, gpa, stage)?,
                        target_offset(target_extent, gpa, stage)?,
                        checked_length(gpa, end, stage)?,
                        stage,
                    )?;
                    gpa = end;
                }
                Inheritance::ZeroRoot(root) => {
                    advance_memory_extent(root.result(), &mut predecessor_index, gpa, stage)?;
                    let predecessor = root
                        .result()
                        .extents()
                        .get(predecessor_index)
                        .copied()
                        .ok_or(SnapshotV2DiffMaterializationError::MissingCoverage { stage })?;
                    if !contains_memory(predecessor, gpa) {
                        return Err(SnapshotV2DiffMaterializationError::MissingCoverage { stage });
                    }
                    advance_diff_extent(root.data_extents(), &mut root_data_index, gpa, stage)?;
                    let root_data = root.data_extents().get(root_data_index).copied();
                    if root_data.is_some_and(|extent| contains_diff(extent, gpa)) {
                        let extent = root_data
                            .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                        let end = target_end
                            .min(explicit_start)
                            .min(predecessor.range().end_exclusive().raw_value())
                            .min(extent.range().end_exclusive().raw_value());
                        append_route(
                            &mut routes,
                            route_bound,
                            RouteSource::ZeroRoot,
                            diff_source_offset(extent, gpa, stage)?,
                            target_offset(target_extent, gpa, stage)?,
                            checked_length(gpa, end, stage)?,
                            stage,
                        )?;
                        gpa = end;
                    } else {
                        let root_data_start = root_data
                            .map(|extent| extent.range().start().raw_value())
                            .unwrap_or(target_end);
                        let end = target_end
                            .min(explicit_start)
                            .min(predecessor.range().end_exclusive().raw_value())
                            .min(root_data_start);
                        append_route(
                            &mut routes,
                            route_bound,
                            RouteSource::Zero,
                            0,
                            target_offset(target_extent, gpa, stage)?,
                            checked_length(gpa, end, stage)?,
                            stage,
                        )?;
                        gpa = end;
                    }
                }
            }
        }
    }
    Ok(routes)
}

fn advance_memory_extent(
    binding: &SnapshotV2MemoryBinding,
    index: &mut usize,
    gpa: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<(), SnapshotV2DiffMaterializationError> {
    while binding
        .extents()
        .get(*index)
        .is_some_and(|extent| extent.range().end_exclusive().raw_value() <= gpa)
    {
        *index = index
            .checked_add(1)
            .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
    }
    Ok(())
}

fn advance_diff_extent(
    extents: &[SnapshotV2DiffDataExtent],
    index: &mut usize,
    gpa: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<(), SnapshotV2DiffMaterializationError> {
    while extents
        .get(*index)
        .is_some_and(|extent| extent.range().end_exclusive().raw_value() <= gpa)
    {
        *index = index
            .checked_add(1)
            .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
    }
    Ok(())
}

fn contains_memory(extent: crate::snapshot_memory_v2::SnapshotV2MemoryExtent, gpa: u64) -> bool {
    extent.range().start().raw_value() <= gpa && gpa < extent.range().end_exclusive().raw_value()
}

fn contains_diff(extent: SnapshotV2DiffDataExtent, gpa: u64) -> bool {
    extent.range().start().raw_value() <= gpa && gpa < extent.range().end_exclusive().raw_value()
}

fn memory_source_offset(
    extent: crate::snapshot_memory_v2::SnapshotV2MemoryExtent,
    gpa: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<u64, SnapshotV2DiffMaterializationError> {
    gpa.checked_sub(extent.range().start().raw_value())
        .and_then(|delta| extent.file_offset().checked_add(delta))
        .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })
}

fn diff_source_offset(
    extent: SnapshotV2DiffDataExtent,
    gpa: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<u64, SnapshotV2DiffMaterializationError> {
    gpa.checked_sub(extent.range().start().raw_value())
        .and_then(|delta| extent.file_offset().checked_add(delta))
        .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })
}

fn target_offset(
    extent: crate::snapshot_memory_v2::SnapshotV2MemoryExtent,
    gpa: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<u64, SnapshotV2DiffMaterializationError> {
    memory_source_offset(extent, gpa, stage)
}

fn checked_length(
    start: u64,
    end: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<u64, SnapshotV2DiffMaterializationError> {
    end.checked_sub(start)
        .filter(|length| *length != 0)
        .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })
}

fn append_route(
    routes: &mut Vec<Route>,
    route_bound: usize,
    source: RouteSource,
    source_offset: u64,
    target_offset: u64,
    length: u64,
    stage: SnapshotV2DiffMaterializationStage,
) -> Result<(), SnapshotV2DiffMaterializationError> {
    if let Some(previous) = routes.last_mut() {
        let target_contiguous = previous
            .target_offset
            .checked_add(previous.length)
            .is_some_and(|end| end == target_offset);
        let source_contiguous = source == RouteSource::Zero
            || previous
                .source_offset
                .checked_add(previous.length)
                .is_some_and(|end| end == source_offset);
        if previous.source == source && target_contiguous && source_contiguous {
            previous.length = previous
                .length
                .checked_add(length)
                .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
            return Ok(());
        }
    }
    if routes.len() >= route_bound {
        return Err(SnapshotV2DiffMaterializationError::InvalidRoute { stage });
    }
    routes.push(Route {
        source,
        source_offset,
        target_offset,
        length,
    });
    Ok(())
}

fn execute(
    target: SnapshotV2MemoryBinding,
    routes: Vec<Route>,
    next: &ValidatedLayerSource,
    base: Option<&ValidatedBase>,
    staging: &mut File,
    policy: &mut impl MaterializationPolicy,
) -> Result<SnapshotV2MemoryBinding, SnapshotV2DiffMaterializationError> {
    use SnapshotV2DiffMaterializationStage as Stage;

    policy.checkpoint(Stage::OutputPreflight)?;
    let output_facts = inspect_file_facts(staging).map_err(|source| {
        SnapshotV2DiffMaterializationError::Source {
            stage: Stage::OutputPreflight,
            source,
        }
    })?;
    let position =
        staging
            .stream_position()
            .map_err(|source| SnapshotV2DiffMaterializationError::Io {
                stage: Stage::OutputPreflight,
                kind: source.kind(),
            })?;
    if output_facts.same_object(next.facts)
        || base.is_some_and(|source| output_facts.same_object(source.facts()))
    {
        return Err(SnapshotV2DiffMaterializationError::SourceOutputAlias {
            stage: Stage::OutputPreflight,
        });
    }
    if !output_facts.is_regular()
        || !output_facts.is_read_write()
        || output_facts.is_append()
        || !output_facts.is_close_on_exec()
        || output_facts.length() != 0
        || position != 0
    {
        return Err(SnapshotV2DiffMaterializationError::InvalidOutput {
            stage: Stage::OutputPreflight,
        });
    }
    let copy_length = routes
        .iter()
        .filter(|route| route.source != RouteSource::Zero)
        .map(|route| route.length)
        .max()
        .unwrap_or(0)
        .min(COPY_CHUNK_BYTES as u64);
    let copy_length = usize::try_from(copy_length).map_err(|_| {
        SnapshotV2DiffMaterializationError::InvalidRoute {
            stage: Stage::LineagePlanning,
        }
    })?;
    let mut buffer = Vec::new();
    policy
        .reserve_copy_buffer(&mut buffer, copy_length)
        .map_err(
            |source| SnapshotV2DiffMaterializationError::MetadataAllocation {
                stage: Stage::LineagePlanning,
                source,
            },
        )?;
    buffer.resize(copy_length, 0);
    let header = target.image_header().map_err(|source| {
        SnapshotV2DiffMaterializationError::ResultBinding {
            stage: Stage::LineagePlanning,
            source,
        }
    })?;

    verify_sources(next, base, SourceObservation::BeforeOutput, policy)?;

    policy.checkpoint(Stage::OutputHeader)?;
    write_exact_at(staging, &header, 0, Stage::OutputHeader, policy)?;

    let zeroes = [0_u8; ZERO_CHUNK_BYTES];
    let mut offset = u64::try_from(NATIVE_V2_MEMORY_HEADER_BYTES).map_err(|_| {
        SnapshotV2DiffMaterializationError::InvalidRoute {
            stage: Stage::OutputPadding,
        }
    })?;
    while offset < NATIVE_V2_MEMORY_ALIGNMENT {
        policy.checkpoint(Stage::OutputPadding)?;
        let remaining = NATIVE_V2_MEMORY_ALIGNMENT.checked_sub(offset).ok_or(
            SnapshotV2DiffMaterializationError::InvalidRoute {
                stage: Stage::OutputPadding,
            },
        )?;
        let length = usize::try_from(remaining.min(ZERO_CHUNK_BYTES as u64)).map_err(|_| {
            SnapshotV2DiffMaterializationError::InvalidRoute {
                stage: Stage::OutputPadding,
            }
        })?;
        let chunk =
            zeroes
                .get(..length)
                .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::OutputPadding,
                })?;
        write_exact_at(staging, chunk, offset, Stage::OutputPadding, policy)?;
        offset = offset
            .checked_add(u64::try_from(length).map_err(|_| {
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::OutputPadding,
                }
            })?)
            .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute {
                stage: Stage::OutputPadding,
            })?;
    }
    policy
        .set_len(staging, target.file_length())
        .map_err(|source| SnapshotV2DiffMaterializationError::Io {
            stage: Stage::OutputPadding,
            kind: source.kind(),
        })?;

    for route in routes {
        if route.source == RouteSource::Zero {
            policy.checkpoint(Stage::DataStreaming)?;
            continue;
        }
        let mut copied = 0_u64;
        while copied < route.length {
            policy.checkpoint(Stage::DataStreaming)?;
            let remaining = route.length.checked_sub(copied).ok_or(
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                },
            )?;
            let length = usize::try_from(remaining.min(COPY_CHUNK_BYTES as u64)).map_err(|_| {
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                }
            })?;
            let chunk = buffer.get_mut(..length).ok_or(
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                },
            )?;
            let source = source_file(route.source, next, base).ok_or(
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                },
            )?;
            let source_offset = route.source_offset.checked_add(copied).ok_or(
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                },
            )?;
            read_exact_at(source, chunk, source_offset, Stage::DataStreaming, policy)?;
            let target_offset = route.target_offset.checked_add(copied).ok_or(
                SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                },
            )?;
            write_exact_at(staging, chunk, target_offset, Stage::DataStreaming, policy)?;
            copied = copied
                .checked_add(u64::try_from(length).map_err(|_| {
                    SnapshotV2DiffMaterializationError::InvalidRoute {
                        stage: Stage::DataStreaming,
                    }
                })?)
                .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute {
                    stage: Stage::DataStreaming,
                })?;
        }
    }

    verify_sources(next, base, SourceObservation::AfterStreaming, policy)?;
    let position = policy
        .seek_output(staging, target.file_length())
        .map_err(|source| SnapshotV2DiffMaterializationError::Io {
            stage: Stage::ResultVerification,
            kind: source.kind(),
        })?;
    if position != target.file_length() {
        return Err(SnapshotV2DiffMaterializationError::InvalidOutput {
            stage: Stage::ResultVerification,
        });
    }

    policy.checkpoint(Stage::ResultVerification)?;
    policy.result_verification_hook(staging);
    verify_snapshot_v2_memory_image_output(&target, staging).map_err(|source| {
        SnapshotV2DiffMaterializationError::ResultVerification {
            stage: Stage::ResultVerification,
            source,
        }
    })?;

    verify_sources(next, base, SourceObservation::Final, policy)?;
    policy.checkpoint(Stage::Complete)?;
    Ok(target)
}

fn source_file<'a>(
    source: RouteSource,
    next: &'a ValidatedLayerSource,
    base: Option<&'a ValidatedBase>,
) -> Option<&'a File> {
    match source {
        RouteSource::NextLayer => Some(&next.file),
        RouteSource::CompleteBase => match base {
            Some(ValidatedBase::Complete(source)) => Some(source.file().as_ref()),
            Some(ValidatedBase::ZeroRoot(_)) | None => None,
        },
        RouteSource::ZeroRoot => match base {
            Some(ValidatedBase::ZeroRoot(source)) => Some(&source.file),
            Some(ValidatedBase::Complete(_)) | None => None,
        },
        RouteSource::Zero => None,
    }
}

fn verify_sources(
    next: &ValidatedLayerSource,
    base: Option<&ValidatedBase>,
    observation: SourceObservation,
    policy: &mut impl MaterializationPolicy,
) -> Result<(), SnapshotV2DiffMaterializationError> {
    let stage = SnapshotV2DiffMaterializationStage::SourceStability;
    policy.checkpoint(stage)?;
    next.verify_unchanged(observation, policy)?;
    if let Some(base) = base {
        base.verify_unchanged(observation, policy)?;
    }
    Ok(())
}

fn read_exact_at(
    file: &File,
    mut buffer: &mut [u8],
    mut offset: u64,
    stage: SnapshotV2DiffMaterializationStage,
    policy: &mut impl MaterializationPolicy,
) -> Result<(), SnapshotV2DiffMaterializationError> {
    while !buffer.is_empty() {
        match policy.read_at(file, buffer, offset) {
            Ok(0) => {
                return Err(SnapshotV2DiffMaterializationError::Io {
                    stage,
                    kind: io::ErrorKind::UnexpectedEof,
                });
            }
            Ok(count) => {
                let count = u64::try_from(count)
                    .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                offset = offset
                    .checked_add(count)
                    .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                let count = usize::try_from(count)
                    .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                buffer = buffer
                    .get_mut(count..)
                    .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(SnapshotV2DiffMaterializationError::Io {
                    stage,
                    kind: source.kind(),
                });
            }
        }
    }
    Ok(())
}

fn write_exact_at(
    file: &File,
    mut buffer: &[u8],
    mut offset: u64,
    stage: SnapshotV2DiffMaterializationStage,
    policy: &mut impl MaterializationPolicy,
) -> Result<(), SnapshotV2DiffMaterializationError> {
    while !buffer.is_empty() {
        match policy.write_at(file, buffer, offset) {
            Ok(0) => {
                return Err(SnapshotV2DiffMaterializationError::Io {
                    stage,
                    kind: io::ErrorKind::WriteZero,
                });
            }
            Ok(count) => {
                let count = u64::try_from(count)
                    .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                offset = offset
                    .checked_add(count)
                    .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                let count = usize::try_from(count)
                    .map_err(|_| SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
                buffer = buffer
                    .get(count..)
                    .ok_or(SnapshotV2DiffMaterializationError::InvalidRoute { stage })?;
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(SnapshotV2DiffMaterializationError::Io {
                    stage,
                    kind: source.kind(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
