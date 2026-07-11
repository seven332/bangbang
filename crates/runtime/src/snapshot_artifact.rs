//! No-clobber native snapshot artifact publication and loading.

use std::collections::TryReserveError;
use std::fmt;
use std::io;
#[cfg(target_os = "macos")]
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::memory::GuestMemory;
use crate::snapshot_commit::{SnapshotCommitError, SnapshotCommitRecord};
#[cfg(target_os = "macos")]
use crate::snapshot_commit::{decode_snapshot_commit_envelope, encode_snapshot_commit_envelope};
#[cfg(target_os = "macos")]
use crate::snapshot_format::NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES;
use crate::snapshot_memory::{SnapshotMemoryLoadError, SnapshotMemoryWriteError};
#[cfg(target_os = "macos")]
use crate::snapshot_memory::{load_snapshot_memory_image, write_snapshot_memory_image};

const REDACTED: &str = "<redacted>";

/// The two independently supplied final paths in a native snapshot artifact pair.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotArtifactPaths {
    state: PathBuf,
    memory: PathBuf,
}

impl SnapshotArtifactPaths {
    /// Creates one state/memory final-path pair.
    pub fn new(state: impl Into<PathBuf>, memory: impl Into<PathBuf>) -> Self {
        Self {
            state: state.into(),
            memory: memory.into(),
        }
    }

    /// Returns the final state path to a trusted caller.
    pub fn state(&self) -> &Path {
        &self.state
    }

    /// Returns the final memory path to a trusted caller.
    pub fn memory(&self) -> &Path {
        &self.memory
    }
}

impl fmt::Debug for SnapshotArtifactPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotArtifactPaths")
            .field("state", &REDACTED)
            .field("memory", &REDACTED)
            .finish()
    }
}

/// One member of a snapshot artifact pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotArtifactKind {
    /// The state envelope and commit marker.
    State,
    /// The guest-memory image.
    Memory,
}

impl fmt::Display for SnapshotArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State => f.write_str("state"),
            Self::Memory => f.write_str("memory"),
        }
    }
}

/// Stable publication stage retained without exposing a host path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPublicationStage {
    PlatformCheck,
    StatePathValidation,
    MemoryPathValidation,
    StateDirectoryOpen,
    MemoryDirectoryOpen,
    AliasCheck,
    StateFinalPreflight,
    MemoryFinalPreflight,
    MemoryStagingCreate,
    StateStagingCreate,
    MemoryWrite,
    StateEncode,
    StateWrite,
    StateWriteVerify,
    MemoryFileSync,
    StateFileSync,
    MemoryPublishCheck,
    MemoryPublish,
    MemoryDirectorySync,
    StatePublishCheck,
    StatePublish,
    StateDirectorySync,
    MemoryStagingCleanup,
    StateStagingCleanup,
}

impl fmt::Display for SnapshotPublicationStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::PlatformCheck => "platform check",
            Self::StatePathValidation => "state path validation",
            Self::MemoryPathValidation => "memory path validation",
            Self::StateDirectoryOpen => "state directory open",
            Self::MemoryDirectoryOpen => "memory directory open",
            Self::AliasCheck => "artifact alias check",
            Self::StateFinalPreflight => "state final preflight",
            Self::MemoryFinalPreflight => "memory final preflight",
            Self::MemoryStagingCreate => "memory staging creation",
            Self::StateStagingCreate => "state staging creation",
            Self::MemoryWrite => "memory staging write",
            Self::StateEncode => "state commit encoding",
            Self::StateWrite => "state staging write",
            Self::StateWriteVerify => "state staging verification",
            Self::MemoryFileSync => "memory file synchronization",
            Self::StateFileSync => "state file synchronization",
            Self::MemoryPublishCheck => "memory staging identity check",
            Self::MemoryPublish => "memory exclusive publication",
            Self::MemoryDirectorySync => "memory directory synchronization",
            Self::StatePublishCheck => "state staging identity check",
            Self::StatePublish => "state exclusive publication",
            Self::StateDirectorySync => "state directory synchronization",
            Self::MemoryStagingCleanup => "memory staging cleanup",
            Self::StateStagingCleanup => "state staging cleanup",
        };
        f.write_str(name)
    }
}

/// Observable final-artifact state after a failed publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotArtifactVisibility {
    /// Neither final name was published by this operation.
    NoFinalArtifact,
    /// The memory final is visible, but no state commit was published.
    MemoryOrphanVisible,
}

/// Best-effort disposition of one private staging entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStagingCleanup {
    Removed,
    AlreadyAbsent,
    ChangedRefused,
    Failed(io::ErrorKind),
}

/// Redacted reason for a native snapshot publication failure.
#[derive(Debug)]
pub enum SnapshotPublicationFailure {
    UnsupportedPlatform,
    InvalidFinalPath { artifact: SnapshotArtifactKind },
    SameArtifact,
    FinalAlreadyExists { artifact: SnapshotArtifactKind },
    RandomnessUnavailable { artifact: SnapshotArtifactKind },
    StagingChanged { artifact: SnapshotArtifactKind },
    Io(io::ErrorKind),
    MemoryWrite(SnapshotMemoryWriteError),
    Commit(SnapshotCommitError),
}

impl fmt::Display for SnapshotPublicationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                f.write_str("snapshot publication is supported only on macOS")
            }
            Self::InvalidFinalPath { artifact } => {
                write!(f, "{artifact} final path is invalid")
            }
            Self::SameArtifact => {
                f.write_str("state and memory final paths identify the same entry")
            }
            Self::FinalAlreadyExists { artifact } => {
                write!(f, "{artifact} final entry already exists")
            }
            Self::RandomnessUnavailable { artifact } => {
                write!(f, "{artifact} staging-name randomness is unavailable")
            }
            Self::StagingChanged { artifact } => {
                write!(f, "{artifact} private staging entry changed")
            }
            Self::Io(kind) => write!(f, "filesystem operation failed with {kind:?}"),
            Self::MemoryWrite(source) => write!(f, "snapshot memory write failed: {source}"),
            Self::Commit(source) => write!(f, "snapshot commit encoding failed: {source}"),
        }
    }
}

impl std::error::Error for SnapshotPublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MemoryWrite(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::UnsupportedPlatform
            | Self::InvalidFinalPath { .. }
            | Self::SameArtifact
            | Self::FinalAlreadyExists { .. }
            | Self::RandomnessUnavailable { .. }
            | Self::StagingChanged { .. }
            | Self::Io(_) => None,
        }
    }
}

/// A failed publication whose `Err` contract guarantees no state commit was published.
#[derive(Debug)]
pub struct SnapshotPublicationError {
    stage: SnapshotPublicationStage,
    visibility: SnapshotArtifactVisibility,
    failure: SnapshotPublicationFailure,
    memory_cleanup: Option<SnapshotStagingCleanup>,
    state_cleanup: Option<SnapshotStagingCleanup>,
}

impl SnapshotPublicationError {
    /// Returns the stage at which the primary failure occurred.
    pub const fn stage(&self) -> SnapshotPublicationStage {
        self.stage
    }

    /// Returns the observable final-artifact state.
    pub const fn visibility(&self) -> SnapshotArtifactVisibility {
        self.visibility
    }

    /// Returns the redacted primary failure.
    pub const fn failure(&self) -> &SnapshotPublicationFailure {
        &self.failure
    }

    /// Returns the explicit memory-staging cleanup disposition, when applicable.
    pub const fn memory_cleanup(&self) -> Option<SnapshotStagingCleanup> {
        self.memory_cleanup
    }

    /// Returns the explicit state-staging cleanup disposition, when applicable.
    pub const fn state_cleanup(&self) -> Option<SnapshotStagingCleanup> {
        self.state_cleanup
    }
}

impl fmt::Display for SnapshotPublicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "snapshot artifact publication failed during {}: {}",
            self.stage, self.failure
        )
    }
}

impl std::error::Error for SnapshotPublicationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// Durability of a pair whose state commit name is already visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotCommitDurability {
    /// Both published names have passed their directory synchronization barriers.
    Durable,
    /// The state name is committed, but its final directory barrier failed.
    Uncertain { kind: io::ErrorKind },
}

/// Successful or visibly committed result of snapshot artifact publication.
#[derive(Debug)]
pub struct SnapshotPublicationOutcome {
    record: SnapshotCommitRecord,
    durability: SnapshotCommitDurability,
}

impl SnapshotPublicationOutcome {
    /// Returns the exact committed state-to-memory record.
    pub const fn record(&self) -> &SnapshotCommitRecord {
        &self.record
    }

    /// Returns the post-commit durability classification.
    pub const fn durability(&self) -> SnapshotCommitDurability {
        self.durability
    }
}

/// Stable stage associated with committed-pair loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotArtifactLoadStage {
    PlatformCheck,
    StatePathValidation,
    StateDirectoryOpen,
    StateOpen,
    StateTypeCheck,
    StateSizeCheck,
    StateRead,
    StateDecode,
    MemoryPathValidation,
    MemoryDirectoryOpen,
    MemoryOpen,
    MemoryTypeCheck,
    MemoryLoad,
}

impl fmt::Display for SnapshotArtifactLoadStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::PlatformCheck => "platform check",
            Self::StatePathValidation => "state path validation",
            Self::StateDirectoryOpen => "state directory open",
            Self::StateOpen => "state final open",
            Self::StateTypeCheck => "state file type check",
            Self::StateSizeCheck => "state size check",
            Self::StateRead => "state read",
            Self::StateDecode => "state commit decode",
            Self::MemoryPathValidation => "memory path validation",
            Self::MemoryDirectoryOpen => "memory directory open",
            Self::MemoryOpen => "memory final open",
            Self::MemoryTypeCheck => "memory file type check",
            Self::MemoryLoad => "memory image load",
        };
        f.write_str(name)
    }
}

/// Redacted reason for a committed-pair load failure.
#[derive(Debug)]
pub enum SnapshotArtifactLoadFailure {
    UnsupportedPlatform,
    InvalidFinalPath { artifact: SnapshotArtifactKind },
    NotRegularFile { artifact: SnapshotArtifactKind },
    StateTooLarge { length: u64, maximum: usize },
    LengthOverflow,
    AllocationFailed { source: TryReserveError },
    Io(io::ErrorKind),
    Commit(SnapshotCommitError),
    Memory(SnapshotMemoryLoadError),
}

impl fmt::Display for SnapshotArtifactLoadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                f.write_str("snapshot artifact loading is supported only on macOS")
            }
            Self::InvalidFinalPath { artifact } => {
                write!(f, "{artifact} final path is invalid")
            }
            Self::NotRegularFile { artifact } => {
                write!(f, "{artifact} artifact is not a regular file")
            }
            Self::StateTooLarge { length, maximum } => write!(
                f,
                "snapshot state file length {length} exceeds {maximum} byte limit"
            ),
            Self::LengthOverflow => f.write_str("snapshot state length cannot be represented"),
            Self::AllocationFailed { source } => {
                write!(f, "failed to allocate snapshot state buffer: {source}")
            }
            Self::Io(kind) => write!(f, "filesystem operation failed with {kind:?}"),
            Self::Commit(source) => write!(f, "invalid snapshot commit: {source}"),
            Self::Memory(source) => write!(f, "invalid snapshot memory image: {source}"),
        }
    }
}

impl std::error::Error for SnapshotArtifactLoadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocationFailed { source } => Some(source),
            Self::Commit(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::UnsupportedPlatform
            | Self::InvalidFinalPath { .. }
            | Self::NotRegularFile { .. }
            | Self::StateTooLarge { .. }
            | Self::LengthOverflow
            | Self::Io(_) => None,
        }
    }
}

/// A redacted committed-pair load failure.
#[derive(Debug)]
pub struct SnapshotArtifactLoadError {
    stage: SnapshotArtifactLoadStage,
    failure: SnapshotArtifactLoadFailure,
}

impl SnapshotArtifactLoadError {
    /// Returns the load stage at which validation failed.
    pub const fn stage(&self) -> SnapshotArtifactLoadStage {
        self.stage
    }

    /// Returns the redacted failure reason.
    pub const fn failure(&self) -> &SnapshotArtifactLoadFailure {
        &self.failure
    }
}

impl fmt::Display for SnapshotArtifactLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "snapshot artifact load failed during {}: {}",
            self.stage, self.failure
        )
    }
}

impl std::error::Error for SnapshotArtifactLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// A fully validated committed pair loaded into anonymous guest memory.
pub struct LoadedSnapshotArtifacts {
    record: SnapshotCommitRecord,
    memory: GuestMemory,
}

impl LoadedSnapshotArtifacts {
    /// Returns the validated commit record.
    pub const fn record(&self) -> &SnapshotCommitRecord {
        &self.record
    }

    /// Returns the newly allocated anonymous guest memory.
    pub const fn memory(&self) -> &GuestMemory {
        &self.memory
    }

    /// Consumes the result into its validated commit record and guest memory.
    pub fn into_parts(self) -> (SnapshotCommitRecord, GuestMemory) {
        (self.record, self.memory)
    }
}

impl fmt::Debug for LoadedSnapshotArtifacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedSnapshotArtifacts")
            .field("record", &REDACTED)
            .field("memory_range_count", &self.memory.regions().len())
            .field("memory_bytes", &self.memory.total_size())
            .finish()
    }
}

/// Publishes complete memory first and the state commit marker last, without replacement.
pub fn publish_snapshot_artifacts(
    paths: &SnapshotArtifactPaths,
    memory: &GuestMemory,
) -> Result<SnapshotPublicationOutcome, SnapshotPublicationError> {
    #[cfg(target_os = "macos")]
    {
        publish_snapshot_artifacts_macos(paths, memory)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, memory);
        Err(publication_error(
            SnapshotPublicationStage::PlatformCheck,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::UnsupportedPlatform,
        ))
    }
}

/// Loads a state-committed artifact pair without constructing or mutating a VM.
pub fn load_snapshot_artifacts(
    paths: &SnapshotArtifactPaths,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        load_snapshot_artifacts_macos(paths)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

fn publication_error(
    stage: SnapshotPublicationStage,
    visibility: SnapshotArtifactVisibility,
    failure: SnapshotPublicationFailure,
) -> SnapshotPublicationError {
    SnapshotPublicationError {
        stage,
        visibility,
        failure,
        memory_cleanup: None,
        state_cleanup: None,
    }
}

fn load_error(
    stage: SnapshotArtifactLoadStage,
    failure: SnapshotArtifactLoadFailure,
) -> SnapshotArtifactLoadError {
    SnapshotArtifactLoadError { stage, failure }
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
use macos::{load_snapshot_artifacts_macos, publish_snapshot_artifacts_macos};

#[cfg(test)]
mod tests;
