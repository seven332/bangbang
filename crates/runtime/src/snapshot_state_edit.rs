//! Failure-atomic publication for one edited native snapshot state document.
//!
//! The transaction retains one immutable input and publishes one distinct,
//! absent output from private same-directory staging. A successful `linkat` is
//! the commit point. Before that point, `Err` guarantees that no final output
//! was created; after it, cleanup or durability uncertainty is returned only as
//! a committed outcome.
//!
//! Retained descriptors and repeated identity/fact checks are detection
//! boundaries. POSIX has no inode-conditioned `linkat` or `unlinkat`, so callers
//! must retain trusted mutation authority over both directories and prevent
//! concurrent mutation of the opened input inode.

use std::collections::TryReserveError;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::snapshot_format::NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES;
use crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES;

const REDACTED: &str = "<redacted>";

/// Maximum accepted input or encoded output bytes for one native state edit.
pub const SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES: usize =
    if NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES > NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES {
        NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES
    } else {
        NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES
    };

/// The immutable input and distinct absent output of one state edit.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotStateEditPaths {
    input: PathBuf,
    output: PathBuf,
}

impl SnapshotStateEditPaths {
    /// Creates one input/output path request without opening either path.
    pub fn new(input: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
        }
    }

    /// Returns the immutable input path to a trusted caller.
    pub fn input(&self) -> &Path {
        &self.input
    }

    /// Returns the final output path to a trusted caller.
    pub fn output(&self) -> &Path {
        &self.output
    }
}

impl fmt::Debug for SnapshotStateEditPaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotStateEditPaths")
            .field("input", &REDACTED)
            .field("output", &REDACTED)
            .finish()
    }
}

/// Stable role of one submitted state-edit path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStateEditPathRole {
    /// Immutable source state.
    Input,
    /// Distinct no-clobber final destination.
    Output,
}

impl fmt::Display for SnapshotStateEditPathRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Input => "input",
            Self::Output => "output",
        })
    }
}

/// Stable checkpoint in one state-edit transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStateEditStage {
    /// Reject unsupported compile targets before path or callback access.
    PlatformCheck,
    /// Validate input path syntax.
    InputPathValidation,
    /// Open and retain the input parent directory.
    InputDirectoryOpen,
    /// Open the final input component without following it.
    InputFileOpen,
    /// Validate and retain immutable input descriptor facts.
    InputValidation,
    /// Validate output path syntax.
    OutputPathValidation,
    /// Open and retain the output parent directory.
    OutputDirectoryOpen,
    /// Reject input/output path or object aliases.
    AliasCheck,
    /// Require an absent final output.
    OutputPreflight,
    /// Read the bounded immutable input bytes.
    InputRead,
    /// Transform input bytes into a typed edited product.
    Transform,
    /// Encode the typed product into canonical bounded bytes.
    Encode,
    /// Create and validate private output-directory staging.
    StagingCreate,
    /// Write the complete canonical bytes.
    StagingWrite,
    /// Flush the staging writer.
    StagingFlush,
    /// Synchronize the staging file.
    StagingFileSync,
    /// Seek staging back to its first byte.
    StagingSeek,
    /// Reread the complete staged bytes and EOF.
    StagingRead,
    /// Semantically verify the reread staged document.
    StagingVerify,
    /// Recheck the immutable input descriptor facts.
    SourceStability,
    /// Recheck retained and path-resolved directory identities.
    DirectoryStability,
    /// Recheck input, absent output, and private staging entries.
    EntryStability,
    /// Perform final checks and atomically hard-link the absent output.
    Commit,
    /// Verify identities after the hard-link commit point.
    CommitVerification,
    /// Remove only the identity-matching private staging entry.
    StagingCleanup,
    /// Synchronize the committed output directory.
    OutputDirectorySync,
    /// Finish without another fallible transition.
    Complete,
}

impl fmt::Display for SnapshotStateEditStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformCheck => "platform check",
            Self::InputPathValidation => "input path validation",
            Self::InputDirectoryOpen => "input directory open",
            Self::InputFileOpen => "input file open",
            Self::InputValidation => "input validation",
            Self::OutputPathValidation => "output path validation",
            Self::OutputDirectoryOpen => "output directory open",
            Self::AliasCheck => "input/output alias check",
            Self::OutputPreflight => "output preflight",
            Self::InputRead => "input read",
            Self::Transform => "typed transformation",
            Self::Encode => "state encoding",
            Self::StagingCreate => "staging creation",
            Self::StagingWrite => "staging write",
            Self::StagingFlush => "staging flush",
            Self::StagingFileSync => "staging file synchronization",
            Self::StagingSeek => "staging seek",
            Self::StagingRead => "staging reread",
            Self::StagingVerify => "staging semantic verification",
            Self::SourceStability => "source stability check",
            Self::DirectoryStability => "directory stability check",
            Self::EntryStability => "entry stability check",
            Self::Commit => "hard-link commit",
            Self::CommitVerification => "committed identity verification",
            Self::StagingCleanup => "staging cleanup",
            Self::OutputDirectorySync => "output directory synchronization",
            Self::Complete => "completion",
        })
    }
}

/// Disposition of one private staging name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStateEditCleanup {
    /// The exact owned staging entry was removed.
    Removed,
    /// The owned staging entry was already absent.
    AlreadyAbsent,
    /// A replacement entry was deliberately retained.
    ChangedRefused,
    /// Inspection or removal failed.
    Failed(io::ErrorKind),
}

/// Value-redacted infrastructure failure before the hard-link commit point.
pub enum SnapshotStateEditFailure {
    /// The compile target cannot provide the required Unix transaction.
    UnsupportedPlatform,
    /// The caller cancelled at a stable precommit checkpoint.
    Cancelled,
    /// One submitted path has no valid exact final component.
    InvalidPath {
        /// Path role, without its value.
        path: SnapshotStateEditPathRole,
    },
    /// The opened input descriptor is not one valid immutable regular file.
    InvalidInput,
    /// Input length exceeds the fixed native-state ceiling.
    InputTooLarge {
        /// Public fixed maximum; the submitted length is not retained.
        maximum: usize,
    },
    /// Encoded state is empty or exceeds the fixed native-state ceiling.
    InvalidEncodedStateLength {
        /// Public fixed maximum; the observed length is not retained.
        maximum: usize,
    },
    /// Input and output name the same path or filesystem object.
    InputOutputAlias,
    /// The no-clobber final output already exists.
    OutputAlreadyExists,
    /// Private staging-name randomness is unavailable.
    RandomnessUnavailable,
    /// Every fixed staging-name creation attempt collided.
    StagingNameExhausted,
    /// The fresh staging descriptor did not retain its required facts.
    InvalidStaging,
    /// The retained input descriptor facts changed.
    SourceChanged,
    /// A parent path no longer resolves to its retained directory.
    DirectoryChanged {
        /// Changed path's role.
        path: SnapshotStateEditPathRole,
    },
    /// A final component no longer has its required identity.
    EntryChanged {
        /// Changed path's role.
        path: SnapshotStateEditPathRole,
    },
    /// The private staging name, descriptor, or facts changed.
    StagingChanged,
    /// Staging bytes differ from the canonical encoded state.
    StagingContentMismatch,
    /// The filesystem rejected the required no-clobber hard-link commit.
    HardLinkUnavailable {
        /// Stable OS error class.
        kind: io::ErrorKind,
    },
    /// A bounded buffer reservation failed.
    Allocation(TryReserveError),
    /// Another filesystem operation failed.
    Io {
        /// Stable OS error class.
        kind: io::ErrorKind,
    },
}

impl fmt::Debug for SnapshotStateEditFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, path) = match self {
            Self::UnsupportedPlatform => ("unsupported platform", None),
            Self::Cancelled => ("cancelled", None),
            Self::InvalidPath { path } => ("invalid path", Some(*path)),
            Self::InvalidInput => ("invalid input", Some(SnapshotStateEditPathRole::Input)),
            Self::InputTooLarge { .. } => {
                ("input too large", Some(SnapshotStateEditPathRole::Input))
            }
            Self::InvalidEncodedStateLength { .. } => ("encoded state length", None),
            Self::InputOutputAlias => ("input/output alias", None),
            Self::OutputAlreadyExists => ("output exists", Some(SnapshotStateEditPathRole::Output)),
            Self::RandomnessUnavailable => ("randomness unavailable", None),
            Self::StagingNameExhausted => ("staging names exhausted", None),
            Self::InvalidStaging => ("invalid staging", None),
            Self::SourceChanged => ("source changed", Some(SnapshotStateEditPathRole::Input)),
            Self::DirectoryChanged { path } => ("directory changed", Some(*path)),
            Self::EntryChanged { path } => ("entry changed", Some(*path)),
            Self::StagingChanged => ("staging changed", None),
            Self::StagingContentMismatch => ("staging content mismatch", None),
            Self::HardLinkUnavailable { .. } => ("hard link unavailable", None),
            Self::Allocation(_) => ("allocation", None),
            Self::Io { .. } => ("I/O", None),
        };
        formatter
            .debug_struct("SnapshotStateEditFailure")
            .field("kind", &kind)
            .field("path", &path)
            .field("details", &REDACTED)
            .finish()
    }
}

impl fmt::Display for SnapshotStateEditFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                formatter.write_str("edited state publication requires a Unix target")
            }
            Self::Cancelled => formatter.write_str("the caller cancelled before commit"),
            Self::InvalidPath { path } => write!(formatter, "the {path} path is invalid"),
            Self::InvalidInput => formatter.write_str("the input descriptor is invalid"),
            Self::InputTooLarge { maximum } => {
                write!(
                    formatter,
                    "the input exceeds the {maximum}-byte state limit"
                )
            }
            Self::InvalidEncodedStateLength { maximum } => write!(
                formatter,
                "the encoded state length is outside the 1..={maximum} byte limit"
            ),
            Self::InputOutputAlias => formatter.write_str("input and output alias"),
            Self::OutputAlreadyExists => formatter.write_str("the output already exists"),
            Self::RandomnessUnavailable => {
                formatter.write_str("private staging-name randomness is unavailable")
            }
            Self::StagingNameExhausted => {
                formatter.write_str("private staging-name attempts were exhausted")
            }
            Self::InvalidStaging => formatter.write_str("the private staging file is invalid"),
            Self::SourceChanged => formatter.write_str("the immutable input changed"),
            Self::DirectoryChanged { path } => write!(formatter, "the {path} directory changed"),
            Self::EntryChanged { path } => write!(formatter, "the {path} entry changed"),
            Self::StagingChanged => formatter.write_str("the private staging entry changed"),
            Self::StagingContentMismatch => formatter.write_str("private staging content changed"),
            Self::HardLinkUnavailable { kind } => {
                write!(
                    formatter,
                    "no-clobber hard-link commit failed with {kind:?}"
                )
            }
            Self::Allocation(_) => formatter.write_str("bounded state allocation failed"),
            Self::Io { kind } => write!(formatter, "filesystem operation failed with {kind:?}"),
        }
    }
}

impl std::error::Error for SnapshotStateEditFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::UnsupportedPlatform
            | Self::Cancelled
            | Self::InvalidPath { .. }
            | Self::InvalidInput
            | Self::InputTooLarge { .. }
            | Self::InvalidEncodedStateLength { .. }
            | Self::InputOutputAlias
            | Self::OutputAlreadyExists
            | Self::RandomnessUnavailable
            | Self::StagingNameExhausted
            | Self::InvalidStaging
            | Self::SourceChanged
            | Self::DirectoryChanged { .. }
            | Self::EntryChanged { .. }
            | Self::StagingChanged
            | Self::StagingContentMismatch
            | Self::HardLinkUnavailable { .. }
            | Self::Io { .. } => None,
        }
    }
}

/// Infrastructure failure whose `Err` contract proves no final output committed.
pub struct SnapshotStateEditError {
    stage: SnapshotStateEditStage,
    failure: SnapshotStateEditFailure,
    staging_cleanup: Option<SnapshotStateEditCleanup>,
}

impl SnapshotStateEditError {
    /// Returns the precommit stage that failed.
    pub const fn stage(&self) -> SnapshotStateEditStage {
        self.stage
    }

    /// Returns the value-redacted primary failure.
    pub const fn failure(&self) -> &SnapshotStateEditFailure {
        &self.failure
    }

    /// Returns the explicit private-staging cleanup disposition, when present.
    pub const fn staging_cleanup(&self) -> Option<SnapshotStateEditCleanup> {
        self.staging_cleanup
    }
}

impl fmt::Debug for SnapshotStateEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotStateEditError")
            .field("stage", &self.stage)
            .field("failure", &self.failure)
            .field("staging_cleanup", &self.staging_cleanup)
            .finish()
    }
}

impl fmt::Display for SnapshotStateEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "edited snapshot state publication failed during {}: {}",
            self.stage, self.failure
        )
    }
}

impl std::error::Error for SnapshotStateEditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// Failure to capture one immutable native snapshot state input.
pub struct SnapshotStateReadError {
    stage: SnapshotStateEditStage,
    failure: SnapshotStateEditFailure,
}

impl SnapshotStateReadError {
    /// Returns the read stage that failed.
    pub const fn stage(&self) -> SnapshotStateEditStage {
        self.stage
    }

    /// Returns the value-redacted primary failure.
    pub const fn failure(&self) -> &SnapshotStateEditFailure {
        &self.failure
    }
}

impl From<SnapshotStateEditError> for SnapshotStateReadError {
    fn from(error: SnapshotStateEditError) -> Self {
        Self {
            stage: error.stage,
            failure: error.failure,
        }
    }
}

impl fmt::Debug for SnapshotStateReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotStateReadError")
            .field("stage", &self.stage)
            .field("failure", &self.failure)
            .finish()
    }
}

impl fmt::Display for SnapshotStateReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "snapshot state read failed during {}: {}",
            self.stage, self.failure
        )
    }
}

impl std::error::Error for SnapshotStateReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// Typed caller-operation failure before the hard-link commit point.
pub struct SnapshotStateEditOperationError<E> {
    stage: SnapshotStateEditStage,
    source: E,
    staging_cleanup: Option<SnapshotStateEditCleanup>,
}

impl<E> SnapshotStateEditOperationError<E> {
    /// Returns the transform, encode, or semantic-verification stage.
    pub const fn stage(&self) -> SnapshotStateEditStage {
        self.stage
    }

    /// Returns the typed operation failure to a trusted caller.
    pub const fn source(&self) -> &E {
        &self.source
    }

    /// Returns the explicit private-staging cleanup disposition, when present.
    pub const fn staging_cleanup(&self) -> Option<SnapshotStateEditCleanup> {
        self.staging_cleanup
    }
}

impl<E> fmt::Debug for SnapshotStateEditOperationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotStateEditOperationError")
            .field("stage", &self.stage)
            .field("source", &REDACTED)
            .field("staging_cleanup", &self.staging_cleanup)
            .finish()
    }
}

impl<E> fmt::Display for SnapshotStateEditOperationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "edited snapshot state operation failed during {}",
            self.stage
        )
    }
}

impl<E> std::error::Error for SnapshotStateEditOperationError<E> {}

/// Failure from publication infrastructure or a typed caller operation.
pub enum SnapshotStateEditTransactionError<E> {
    /// Path, descriptor, staging, commit, or cancellation failure.
    Publication(SnapshotStateEditError),
    /// Typed transform, encode, or semantic-verification failure.
    Operation(SnapshotStateEditOperationError<E>),
}

impl<E> SnapshotStateEditTransactionError<E> {
    /// Returns the infrastructure failure, when present.
    pub const fn publication(&self) -> Option<&SnapshotStateEditError> {
        match self {
            Self::Publication(error) => Some(error),
            Self::Operation(_) => None,
        }
    }

    /// Returns the typed operation failure, when present.
    pub const fn operation(&self) -> Option<&SnapshotStateEditOperationError<E>> {
        match self {
            Self::Publication(_) => None,
            Self::Operation(error) => Some(error),
        }
    }
}

impl<E> From<SnapshotStateEditError> for SnapshotStateEditTransactionError<E> {
    fn from(error: SnapshotStateEditError) -> Self {
        Self::Publication(error)
    }
}

impl<E> fmt::Debug for SnapshotStateEditTransactionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Publication(error) => formatter.debug_tuple("Publication").field(error).finish(),
            Self::Operation(error) => formatter.debug_tuple("Operation").field(error).finish(),
        }
    }
}

impl<E> fmt::Display for SnapshotStateEditTransactionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Publication(error) => error.fmt(formatter),
            Self::Operation(error) => error.fmt(formatter),
        }
    }
}

impl<E> std::error::Error for SnapshotStateEditTransactionError<E> {}

/// Value-redacted reason that a committed edit is not proven durable and clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStateEditCommitFailure {
    /// A retained or path-resolved parent directory changed.
    DirectoryChanged {
        /// Changed path's role.
        path: SnapshotStateEditPathRole,
    },
    /// The immutable input facts or name changed after commit.
    InputChanged,
    /// The final output no longer names the committed inode.
    OutputEntryChanged,
    /// The staging name no longer names the committed inode.
    StagingEntryChanged,
    /// Staging cleanup was not conclusive.
    Cleanup,
    /// Another postcommit filesystem operation failed.
    Io(io::ErrorKind),
}

/// Durability and cleanup classification after successful hard-link commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStateEditCommit {
    /// Output identity, staging cleanup, and directory synchronization succeeded.
    Durable,
    /// The output link committed, but later proof is incomplete.
    Uncertain {
        /// First postcommit stage that became uncertain.
        stage: SnapshotStateEditStage,
        /// Stable first uncertainty.
        failure: SnapshotStateEditCommitFailure,
        /// Best-effort staging cleanup disposition.
        staging_cleanup: SnapshotStateEditCleanup,
        /// Output-directory synchronization failure, if any.
        directory_sync: Option<io::ErrorKind>,
    },
}

/// Typed edited product and classification returned after hard-link commit.
pub struct SnapshotStateEditOutcome<T> {
    product: T,
    commit: SnapshotStateEditCommit,
}

impl<T> SnapshotStateEditOutcome<T> {
    /// Returns the caller's typed transformed product.
    pub const fn product(&self) -> &T {
        &self.product
    }

    /// Returns the durable or committed-uncertain classification.
    pub const fn commit(&self) -> SnapshotStateEditCommit {
        self.commit
    }

    /// Consumes the outcome into its typed product and commit classification.
    pub fn into_parts(self) -> (T, SnapshotStateEditCommit) {
        (self.product, self.commit)
    }
}

impl<T> fmt::Debug for SnapshotStateEditOutcome<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotStateEditOutcome")
            .field("product", &REDACTED)
            .field("commit", &self.commit)
            .finish()
    }
}

/// Captures one bounded immutable native snapshot state file.
pub fn read_snapshot_state_file(path: &Path) -> Result<Vec<u8>, SnapshotStateReadError> {
    read_snapshot_state_file_with_cancel(path, |_| false)
}

/// Captures one native snapshot state file with stable cancellation.
///
/// On Unix, the reader retains the input file and parent directory, rejects a
/// final symlink or special file, reads the validated exact length and EOF, and
/// repeats source, directory, entry, and content checks after the last caller
/// callback. The callback receives only the input-related subset of
/// [`SnapshotStateEditStage`].
pub fn read_snapshot_state_file_with_cancel<Cancel>(
    path: &Path,
    is_cancelled: Cancel,
) -> Result<Vec<u8>, SnapshotStateReadError>
where
    Cancel: FnMut(SnapshotStateEditStage) -> bool,
{
    #[cfg(unix)]
    {
        unix::read_snapshot_state_file_unix_with_cancel(path, is_cancelled)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, is_cancelled);
        Err(SnapshotStateReadError {
            stage: SnapshotStateEditStage::PlatformCheck,
            failure: SnapshotStateEditFailure::UnsupportedPlatform,
        })
    }
}

/// Publishes one synchronously transformed, encoded, and verified state document.
pub fn publish_edited_snapshot_state_with<T, E, Transform, Encode, Verify>(
    paths: &SnapshotStateEditPaths,
    transform: Transform,
    encode: Encode,
    verify: Verify,
) -> Result<SnapshotStateEditOutcome<T>, SnapshotStateEditTransactionError<E>>
where
    Transform: FnOnce(&[u8]) -> Result<T, E>,
    Encode: FnOnce(&T) -> Result<Vec<u8>, E>,
    Verify: FnOnce(&[u8], &T) -> Result<(), E>,
{
    publish_edited_snapshot_state_with_cancel(paths, transform, encode, verify, |_| false)
}

/// Publishes one edited state document with stable precommit cancellation.
///
/// Transform, encode, semantic verification, and the cancellation callback all
/// finish before final repeated checks. After those checks, the implementation
/// calls `linkat` directly without invoking caller code or allocating.
pub fn publish_edited_snapshot_state_with_cancel<T, E, Transform, Encode, Verify, Cancel>(
    paths: &SnapshotStateEditPaths,
    transform: Transform,
    encode: Encode,
    verify: Verify,
    is_cancelled: Cancel,
) -> Result<SnapshotStateEditOutcome<T>, SnapshotStateEditTransactionError<E>>
where
    Transform: FnOnce(&[u8]) -> Result<T, E>,
    Encode: FnOnce(&T) -> Result<Vec<u8>, E>,
    Verify: FnOnce(&[u8], &T) -> Result<(), E>,
    Cancel: FnMut(SnapshotStateEditStage) -> bool,
{
    #[cfg(unix)]
    {
        unix::publish_edited_snapshot_state_unix_with_cancel(
            paths,
            transform,
            encode,
            verify,
            is_cancelled,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (paths, transform, encode, verify, is_cancelled);
        Err(precommit_error(
            SnapshotStateEditStage::PlatformCheck,
            SnapshotStateEditFailure::UnsupportedPlatform,
        )
        .into())
    }
}

fn precommit_error(
    stage: SnapshotStateEditStage,
    failure: SnapshotStateEditFailure,
) -> SnapshotStateEditError {
    SnapshotStateEditError {
        stage,
        failure,
        staging_cleanup: None,
    }
}

fn operation_error<E>(
    stage: SnapshotStateEditStage,
    source: E,
) -> SnapshotStateEditOperationError<E> {
    SnapshotStateEditOperationError {
        stage,
        source,
        staging_cleanup: None,
    }
}

#[cfg(unix)]
mod unix;

#[cfg(test)]
mod tests;
