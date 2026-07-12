//! No-clobber native snapshot artifact publication and loading.

use std::collections::TryReserveError;
use std::fmt;
use std::fs::File;
#[cfg(target_os = "macos")]
use std::io::Read;
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::memory::GuestMemory;
use crate::snapshot_commit::{SnapshotCommitError, SnapshotCommitRecord};
#[cfg(target_os = "macos")]
use crate::snapshot_commit::{decode_snapshot_commit_envelope, encode_snapshot_commit_envelope};
#[cfg(target_os = "macos")]
use crate::snapshot_format::NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES;
use crate::snapshot_memory::{
    SnapshotMemoryLoadError, SnapshotMemoryWriteError, write_snapshot_memory_image,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory::{load_snapshot_memory_image, verify_snapshot_memory_image_output};

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

/// A pathless, move-only writer for one private memory staging inode.
///
/// The producer must let this value drop before returning success. Publication
/// verifies that close proof before reading, synchronizing, or renaming the
/// staging inode.
pub struct SnapshotMemoryStagingWriter {
    file: Option<File>,
    closed: Arc<AtomicBool>,
}

impl SnapshotMemoryStagingWriter {
    fn new(file: File, closed: Arc<AtomicBool>) -> Self {
        Self {
            file: Some(file),
            closed,
        }
    }

    /// Explicitly closes the staging-writer alias.
    pub fn close(mut self) {
        self.close_file();
    }

    fn close_file(&mut self) {
        drop(self.file.take());
        self.closed.store(true, Ordering::Release);
    }

    fn file_mut(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

impl Write for SnapshotMemoryStagingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file_mut()?.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file_mut()?.flush()
    }
}

impl Seek for SnapshotMemoryStagingWriter {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.file_mut()?.seek(position)
    }
}

impl Drop for SnapshotMemoryStagingWriter {
    fn drop(&mut self) {
        self.close_file();
    }
}

impl fmt::Debug for SnapshotMemoryStagingWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotMemoryStagingWriter")
            .field("staging", &REDACTED)
            .field("closed", &self.file.is_none())
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
    MemoryWriterClose,
    MemoryWriteVerify,
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
            Self::MemoryWriterClose => "memory staging writer close",
            Self::MemoryWriteVerify => "memory staging verification",
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
    StagingWriterRetained,
    Io(io::ErrorKind),
    MemoryWrite(SnapshotMemoryWriteError),
    MemoryVerify(SnapshotMemoryLoadError),
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
            Self::StagingWriterRetained => {
                f.write_str("snapshot memory staging writer remained open")
            }
            Self::Io(kind) => write!(f, "filesystem operation failed with {kind:?}"),
            Self::MemoryWrite(source) => write!(f, "snapshot memory write failed: {source}"),
            Self::MemoryVerify(source) => {
                write!(f, "snapshot memory staging verification failed: {source}")
            }
            Self::Commit(source) => write!(f, "snapshot commit encoding failed: {source}"),
        }
    }
}

impl std::error::Error for SnapshotPublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MemoryWrite(source) => Some(source),
            Self::MemoryVerify(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::UnsupportedPlatform
            | Self::InvalidFinalPath { .. }
            | Self::SameArtifact
            | Self::FinalAlreadyExists { .. }
            | Self::RandomnessUnavailable { .. }
            | Self::StagingChanged { .. }
            | Self::StagingWriterRetained
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

/// A content-producer failure before either final artifact was published.
pub struct SnapshotPublicationProducerError<E> {
    source: E,
    memory_cleanup: Option<SnapshotStagingCleanup>,
    state_cleanup: Option<SnapshotStagingCleanup>,
}

impl<E> SnapshotPublicationProducerError<E> {
    fn new(source: E) -> Self {
        Self {
            source,
            memory_cleanup: None,
            state_cleanup: None,
        }
    }

    /// Returns the typed producer failure to a trusted caller.
    pub const fn source(&self) -> &E {
        &self.source
    }

    /// Returns the explicit memory-staging cleanup disposition.
    pub const fn memory_cleanup(&self) -> Option<SnapshotStagingCleanup> {
        self.memory_cleanup
    }

    /// Returns the explicit state-staging cleanup disposition.
    pub const fn state_cleanup(&self) -> Option<SnapshotStagingCleanup> {
        self.state_cleanup
    }

    fn into_parts(
        self,
    ) -> (
        E,
        Option<SnapshotStagingCleanup>,
        Option<SnapshotStagingCleanup>,
    ) {
        (self.source, self.memory_cleanup, self.state_cleanup)
    }
}

impl<E> fmt::Debug for SnapshotPublicationProducerError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotPublicationProducerError")
            .field("source", &REDACTED)
            .field("memory_cleanup", &self.memory_cleanup)
            .field("state_cleanup", &self.state_cleanup)
            .finish()
    }
}

impl<E> fmt::Display for SnapshotPublicationProducerError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("snapshot artifact content producer failed")
    }
}

impl<E> std::error::Error for SnapshotPublicationProducerError<E> {}

/// Failure from either publication infrastructure or its typed content producer.
pub enum SnapshotPublicationTransactionError<E> {
    /// Publication infrastructure or validation failed.
    Publication(SnapshotPublicationError),
    /// The producer failed before either final name became visible.
    Producer(SnapshotPublicationProducerError<E>),
}

impl<E> From<SnapshotPublicationError> for SnapshotPublicationTransactionError<E> {
    fn from(source: SnapshotPublicationError) -> Self {
        Self::Publication(source)
    }
}

impl<E> SnapshotPublicationTransactionError<E> {
    /// Returns the infrastructure failure, when publication itself failed.
    pub const fn publication(&self) -> Option<&SnapshotPublicationError> {
        match self {
            Self::Publication(source) => Some(source),
            Self::Producer(_) => None,
        }
    }

    /// Returns the typed producer failure, when content preparation failed.
    pub const fn producer(&self) -> Option<&SnapshotPublicationProducerError<E>> {
        match self {
            Self::Publication(_) => None,
            Self::Producer(source) => Some(source),
        }
    }
}

impl<E> fmt::Debug for SnapshotPublicationTransactionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Publication(source) => f.debug_tuple("Publication").field(source).finish(),
            Self::Producer(source) => f.debug_tuple("Producer").field(source).finish(),
        }
    }
}

impl<E> fmt::Display for SnapshotPublicationTransactionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Publication(source) => write!(f, "{source}"),
            Self::Producer(source) => write!(f, "{source}"),
        }
    }
}

impl<E: 'static> std::error::Error for SnapshotPublicationTransactionError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Publication(source) => Some(source),
            Self::Producer(source) => Some(source),
        }
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
    match publish_snapshot_artifacts_with(paths, |mut writer| {
        let binding = write_snapshot_memory_image(memory, &mut writer)?;
        Ok::<_, SnapshotMemoryWriteError>(SnapshotCommitRecord::new(binding))
    }) {
        Ok(outcome) => Ok(outcome),
        Err(SnapshotPublicationTransactionError::Publication(source)) => Err(source),
        Err(SnapshotPublicationTransactionError::Producer(source)) => {
            let (source, memory_cleanup, state_cleanup) = source.into_parts();
            let mut error = publication_error(
                SnapshotPublicationStage::MemoryWrite,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::MemoryWrite(source),
            );
            error.memory_cleanup = memory_cleanup;
            error.state_cleanup = state_cleanup;
            Err(error)
        }
    }
}

/// Publishes caller-produced memory and state content through one no-clobber transaction.
///
/// The producer receives a pathless writer for the private memory staging
/// inode and must return the exact record that binds its output. The writer
/// must be dropped before producer success; publication verifies that close
/// proof before any synchronization or rename.
pub fn publish_snapshot_artifacts_with<E, F>(
    paths: &SnapshotArtifactPaths,
    producer: F,
) -> Result<SnapshotPublicationOutcome, SnapshotPublicationTransactionError<E>>
where
    F: FnOnce(SnapshotMemoryStagingWriter) -> Result<SnapshotCommitRecord, E>,
{
    #[cfg(target_os = "macos")]
    {
        publish_snapshot_artifacts_macos_with(paths, producer)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, producer);
        Err(SnapshotPublicationTransactionError::Publication(
            publication_error(
                SnapshotPublicationStage::PlatformCheck,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::UnsupportedPlatform,
            ),
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
use macos::{load_snapshot_artifacts_macos, publish_snapshot_artifacts_macos_with};

#[cfg(test)]
mod tests;
