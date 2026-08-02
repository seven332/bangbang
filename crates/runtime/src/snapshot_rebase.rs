//! Failure-atomic path transaction for dormant native-v2 Diff rebases.
//!
//! Mutation is anchored to retained directory descriptors and every stable
//! identity is rechecked before the atomic exchange. Darwin does not expose an
//! identity-conditioned rename or unlink, so callers must retain trusted
//! authority over the base directory during the transaction. These checks
//! narrow the residual race; they do not make an uncooperative writer with the
//! same directory authority safe. Callers must likewise prevent concurrent
//! mutation of the already-open source inodes; repeated fact checks are
//! detection boundaries, not filesystem seals.
//!
//! Ordinary errors and panic unwinding clean only an identity-matching private
//! staging entry. Abrupt process termination such as `SIGKILL` can leave a
//! random staging or displaced-base entry because this dormant transaction has
//! no selected persistent supervisor or recovery ledger.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::snapshot_diff_v2_13::SnapshotV2DiffMaterializationError;
use crate::snapshot_memory_v2::{SnapshotV2MemoryBinding, SnapshotV2MemoryLoadError};

const REDACTED: &str = "<redacted>";

/// The two Firecracker-shaped memory paths used by one Diff rebase.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2DiffRebasePaths {
    base: PathBuf,
    diff: PathBuf,
}

impl SnapshotV2DiffRebasePaths {
    /// Creates one base/diff path pair.
    pub fn new(base: impl Into<PathBuf>, diff: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            diff: diff.into(),
        }
    }

    /// Returns the base path to a trusted caller.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Returns the differential-layer path to a trusted caller.
    pub fn diff(&self) -> &Path {
        &self.diff
    }
}

impl fmt::Debug for SnapshotV2DiffRebasePaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffRebasePaths")
            .field("base", &REDACTED)
            .field("diff", &REDACTED)
            .finish()
    }
}

/// Stable role of one path in a Diff rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffRebaseInput {
    /// Complete-image or proven zero-root base replaced on commit.
    Base,
    /// Immutable next layer that is never mutated.
    Diff,
}

impl fmt::Display for SnapshotV2DiffRebaseInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Base => "base",
            Self::Diff => "diff",
        })
    }
}

/// Stable, value-redacted checkpoint in the rebase transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffRebaseStage {
    /// Reject unsupported compile targets before inspecting either path.
    PlatformCheck,
    /// Validate the base path syntax.
    BasePathValidation,
    /// Open and retain the base parent directory.
    BaseDirectoryOpen,
    /// Open the base final component without following it.
    BaseFileOpen,
    /// Validate and classify the immutable base descriptor.
    BaseValidation,
    /// Validate the diff path syntax.
    DiffPathValidation,
    /// Open and retain the diff parent directory.
    DiffDirectoryOpen,
    /// Open the diff final component without following it.
    DiffFileOpen,
    /// Validate the immutable diff descriptor.
    DiffValidation,
    /// Reject base/diff object aliases.
    SourceAliasCheck,
    /// Duplicate immutable descriptors for the consuming materializer.
    SourceDuplication,
    /// Create and validate private result staging.
    StagingCreate,
    /// Materialize and verify the complete result.
    Materialization,
    /// Synchronize and reverify the staged result.
    ResultFileSync,
    /// Recheck immutable source facts.
    SourceStability,
    /// Recheck retained and path-resolved directory identities.
    DirectoryStability,
    /// Recheck source and staging component identities.
    EntryStability,
    /// Atomically exchange result staging with the base.
    AtomicExchange,
    /// Verify the identities observed after the commit point.
    CommitVerification,
    /// Remove the exact displaced old base.
    DisplacedCleanup,
    /// Synchronize the committed base directory.
    BaseDirectorySync,
    /// Finish without another fallible transition.
    Complete,
}

impl fmt::Display for SnapshotV2DiffRebaseStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformCheck => "platform check",
            Self::BasePathValidation => "base path validation",
            Self::BaseDirectoryOpen => "base directory open",
            Self::BaseFileOpen => "base file open",
            Self::BaseValidation => "base validation",
            Self::DiffPathValidation => "diff path validation",
            Self::DiffDirectoryOpen => "diff directory open",
            Self::DiffFileOpen => "diff file open",
            Self::DiffValidation => "diff validation",
            Self::SourceAliasCheck => "source alias check",
            Self::SourceDuplication => "source duplication",
            Self::StagingCreate => "staging creation",
            Self::Materialization => "materialization",
            Self::ResultFileSync => "result file synchronization",
            Self::SourceStability => "source stability",
            Self::DirectoryStability => "directory stability",
            Self::EntryStability => "entry stability",
            Self::AtomicExchange => "atomic exchange",
            Self::CommitVerification => "committed identity verification",
            Self::DisplacedCleanup => "displaced-base cleanup",
            Self::BaseDirectorySync => "base directory synchronization",
            Self::Complete => "completion",
        })
    }
}

/// Disposition of a private staging or displaced-base entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffRebaseCleanup {
    /// The exact owned entry was removed.
    Removed,
    /// The owned entry was already absent.
    AlreadyAbsent,
    /// A changed entry was deliberately retained.
    ChangedRefused,
    /// Inspection or removal failed.
    Failed(io::ErrorKind),
}

/// Redacted reason for a failure before the exchange commit point.
pub enum SnapshotV2DiffRebaseFailure {
    /// The caller cancelled at a stable checkpoint.
    Cancelled,
    /// The compile target cannot provide the required atomic exchange.
    UnsupportedPlatform,
    /// One submitted path has no valid final component.
    InvalidPath {
        /// Path role, without its value.
        input: SnapshotV2DiffRebaseInput,
    },
    /// One immutable source descriptor failed validation.
    Source {
        /// Source role, without its value.
        input: SnapshotV2DiffRebaseInput,
        /// Existing redacted descriptor-validation failure.
        source: SnapshotV2MemoryLoadError,
    },
    /// The base magic is neither a complete image nor a Diff layer.
    InvalidBaseKind,
    /// Base and diff name the same filesystem object.
    SourceAlias,
    /// Private staging-name randomness is unavailable.
    RandomnessUnavailable,
    /// A retained source changed before commit.
    SourceChanged {
        /// Changed source role.
        input: SnapshotV2DiffRebaseInput,
    },
    /// A parent path no longer resolves to its retained directory.
    DirectoryChanged {
        /// Changed input's directory role.
        input: SnapshotV2DiffRebaseInput,
    },
    /// A final input component no longer names its retained object.
    EntryChanged {
        /// Changed input role.
        input: SnapshotV2DiffRebaseInput,
    },
    /// The private staging component no longer names the result object.
    StagingChanged,
    /// The newly created staging descriptor failed validation.
    StagingValidation {
        /// Existing redacted descriptor-inspection failure.
        source: SnapshotV2MemoryLoadError,
    },
    /// GPA-correct descriptor materialization failed.
    Materialization {
        /// Existing redacted materialization failure.
        source: SnapshotV2DiffMaterializationError,
    },
    /// The synchronized staged complete image failed detached verification.
    ResultVerification {
        /// Existing redacted complete-image verification failure.
        source: SnapshotV2MemoryLoadError,
    },
    /// The filesystem rejected the required atomic exchange.
    AtomicExchangeUnavailable {
        /// Stable OS error class.
        kind: io::ErrorKind,
    },
    /// Another filesystem operation failed.
    Io {
        /// Stable OS error class.
        kind: io::ErrorKind,
    },
}

impl fmt::Debug for SnapshotV2DiffRebaseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, input) = match self {
            Self::Cancelled => ("cancelled", None),
            Self::UnsupportedPlatform => ("unsupported platform", None),
            Self::InvalidPath { input } => ("invalid path", Some(*input)),
            Self::Source { input, .. } => ("source", Some(*input)),
            Self::InvalidBaseKind => ("base kind", Some(SnapshotV2DiffRebaseInput::Base)),
            Self::SourceAlias => ("source alias", None),
            Self::RandomnessUnavailable => ("randomness", None),
            Self::SourceChanged { input } => ("source changed", Some(*input)),
            Self::DirectoryChanged { input } => ("directory changed", Some(*input)),
            Self::EntryChanged { input } => ("entry changed", Some(*input)),
            Self::StagingChanged => ("staging changed", None),
            Self::StagingValidation { .. } => ("staging validation", None),
            Self::Materialization { .. } => ("materialization", None),
            Self::ResultVerification { .. } => ("result verification", None),
            Self::AtomicExchangeUnavailable { .. } => ("atomic exchange", None),
            Self::Io { .. } => ("I/O", None),
        };
        formatter
            .debug_struct("SnapshotV2DiffRebaseFailure")
            .field("kind", &kind)
            .field("input", &input)
            .field("details", &REDACTED)
            .finish()
    }
}

impl fmt::Display for SnapshotV2DiffRebaseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("the caller cancelled the rebase"),
            Self::UnsupportedPlatform => {
                formatter.write_str("atomic snapshot rebase is supported only on macOS")
            }
            Self::InvalidPath { input } => write!(formatter, "the {input} path is invalid"),
            Self::Source { input, .. } => {
                write!(formatter, "the {input} source descriptor is invalid")
            }
            Self::InvalidBaseKind => formatter.write_str("the base artifact kind is invalid"),
            Self::SourceAlias => formatter.write_str("the base and diff sources alias"),
            Self::RandomnessUnavailable => {
                formatter.write_str("private staging-name randomness is unavailable")
            }
            Self::SourceChanged { input } => {
                write!(formatter, "the {input} source changed")
            }
            Self::DirectoryChanged { input } => {
                write!(formatter, "the {input} directory changed")
            }
            Self::EntryChanged { input } => {
                write!(formatter, "the {input} entry changed")
            }
            Self::StagingChanged => formatter.write_str("the private staging entry changed"),
            Self::StagingValidation { .. } => {
                formatter.write_str("the private staging descriptor is invalid")
            }
            Self::Materialization { .. } => {
                formatter.write_str("differential materialization failed")
            }
            Self::ResultVerification { .. } => {
                formatter.write_str("the synchronized result failed verification")
            }
            Self::AtomicExchangeUnavailable { kind } => {
                write!(formatter, "atomic exchange failed with {kind:?}")
            }
            Self::Io { kind } => write!(formatter, "filesystem operation failed with {kind:?}"),
        }
    }
}

impl std::error::Error for SnapshotV2DiffRebaseFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source { source, .. } => Some(source),
            Self::StagingValidation { source } => Some(source),
            Self::Materialization { source } => Some(source),
            Self::ResultVerification { source } => Some(source),
            Self::Cancelled
            | Self::UnsupportedPlatform
            | Self::InvalidPath { .. }
            | Self::InvalidBaseKind
            | Self::SourceAlias
            | Self::RandomnessUnavailable
            | Self::SourceChanged { .. }
            | Self::DirectoryChanged { .. }
            | Self::EntryChanged { .. }
            | Self::StagingChanged
            | Self::AtomicExchangeUnavailable { .. }
            | Self::Io { .. } => None,
        }
    }
}

/// A failed rebase whose `Err` contract proves no exchange committed.
pub struct SnapshotV2DiffRebaseError {
    stage: SnapshotV2DiffRebaseStage,
    failure: SnapshotV2DiffRebaseFailure,
    staging_cleanup: Option<SnapshotV2DiffRebaseCleanup>,
}

impl SnapshotV2DiffRebaseError {
    /// Returns the pre-commit stage that failed.
    pub const fn stage(&self) -> SnapshotV2DiffRebaseStage {
        self.stage
    }

    /// Returns the redacted primary failure.
    pub const fn failure(&self) -> &SnapshotV2DiffRebaseFailure {
        &self.failure
    }

    /// Returns the explicit private-staging cleanup disposition, when present.
    pub const fn staging_cleanup(&self) -> Option<SnapshotV2DiffRebaseCleanup> {
        self.staging_cleanup
    }
}

impl fmt::Debug for SnapshotV2DiffRebaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffRebaseError")
            .field("stage", &self.stage)
            .field("failure", &self.failure)
            .field("staging_cleanup", &self.staging_cleanup)
            .finish()
    }
}

impl fmt::Display for SnapshotV2DiffRebaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "native-v2 Diff rebase failed during {}: {}",
            self.stage, self.failure
        )
    }
}

impl std::error::Error for SnapshotV2DiffRebaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// Redacted reason that a committed rebase is not proven durable and clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffRebaseCommitFailure {
    /// A retained or path-resolved parent directory changed.
    DirectoryChanged {
        /// Directory role, without its path.
        input: SnapshotV2DiffRebaseInput,
    },
    /// The committed base entry did not retain the result identity.
    BaseEntryChanged,
    /// The displaced staging entry did not retain the old-base identity.
    DisplacedEntryChanged,
    /// The immutable diff changed across the commit.
    DiffChanged,
    /// The retained old-base descriptor no longer names its original object.
    BaseSourceChanged,
    /// Displaced-base cleanup was not conclusive.
    Cleanup,
    /// Another post-commit filesystem operation failed.
    Io(io::ErrorKind),
}

/// Durability and cleanup classification after a successful exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DiffRebaseCommit {
    /// Result identity, displaced cleanup, and directory synchronization succeeded.
    Durable,
    /// The result was exchanged into the base, but later proof is incomplete.
    Uncertain {
        /// First post-commit stage that became uncertain.
        stage: SnapshotV2DiffRebaseStage,
        /// Stable value-redacted first uncertainty.
        failure: SnapshotV2DiffRebaseCommitFailure,
        /// Best-effort displaced-entry cleanup disposition.
        cleanup: SnapshotV2DiffRebaseCleanup,
        /// Directory synchronization failure, or `None` when the barrier succeeded.
        directory_sync: Option<io::ErrorKind>,
    },
}

/// Binding and commit classification returned after an atomic base exchange.
pub struct SnapshotV2DiffRebaseOutcome {
    binding: SnapshotV2MemoryBinding,
    commit: SnapshotV2DiffRebaseCommit,
}

impl SnapshotV2DiffRebaseOutcome {
    /// Returns the complete canonical result binding.
    pub const fn binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the durable or committed-uncertain classification.
    pub const fn commit(&self) -> SnapshotV2DiffRebaseCommit {
        self.commit
    }
}

impl fmt::Debug for SnapshotV2DiffRebaseOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2DiffRebaseOutcome")
            .field("binding", &REDACTED)
            .field("commit", &self.commit)
            .finish()
    }
}

/// Applies one next Diff layer and failure-atomically replaces the named base.
pub fn rebase_snapshot_v2_diff_paths(
    paths: &SnapshotV2DiffRebasePaths,
) -> Result<SnapshotV2DiffRebaseOutcome, SnapshotV2DiffRebaseError> {
    rebase_snapshot_v2_diff_paths_with_cancel(paths, |_| false)
}

/// Applies one next Diff layer with bounded pre-commit cancellation.
///
/// A successful atomic exchange disables cancellation while mandatory
/// committed verification, cleanup, and directory synchronization finish.
/// A pre-exchange `Err` means no exchange committed; after exchange the return
/// value always carries a durable or committed-uncertain classification.
pub fn rebase_snapshot_v2_diff_paths_with_cancel<C>(
    paths: &SnapshotV2DiffRebasePaths,
    is_cancelled: C,
) -> Result<SnapshotV2DiffRebaseOutcome, SnapshotV2DiffRebaseError>
where
    C: FnMut(SnapshotV2DiffRebaseStage) -> bool,
{
    #[cfg(target_os = "macos")]
    {
        macos::rebase_snapshot_v2_diff_paths_macos(paths, is_cancelled)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, is_cancelled);
        Err(precommit_error(
            SnapshotV2DiffRebaseStage::PlatformCheck,
            SnapshotV2DiffRebaseFailure::UnsupportedPlatform,
        ))
    }
}

fn precommit_error(
    stage: SnapshotV2DiffRebaseStage,
    failure: SnapshotV2DiffRebaseFailure,
) -> SnapshotV2DiffRebaseError {
    SnapshotV2DiffRebaseError {
        stage,
        failure,
        staging_cleanup: None,
    }
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(test)]
mod tests;
