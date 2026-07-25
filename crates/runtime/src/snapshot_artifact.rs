//! No-clobber native snapshot artifact publication and loading.

#[cfg(target_os = "macos")]
use std::borrow::Cow;
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
use crate::snapshot_format::{
    NATIVE_V1_SNAPSHOT_VERSION, NativeSnapshotFormatError, NativeSnapshotState,
    SnapshotFormatVersion, decode_native_snapshot_state,
};
use crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_VERSION;
#[cfg(target_os = "macos")]
use crate::snapshot_format_v2::{NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES, decode_snapshot_v2_state};
use crate::snapshot_memory::{
    SnapshotMemoryLoadError, SnapshotMemoryWriteError, write_snapshot_memory_image,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory::{load_snapshot_memory_image, verify_snapshot_memory_image_output};
use crate::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, SnapshotV2MemoryLoadError, SnapshotV2MemoryStateError,
    decode_snapshot_v2_memory_binding,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory_v2::{
    load_snapshot_v2_memory_file, verify_snapshot_v2_memory_image_output,
};

const REDACTED: &str = "<redacted>";

/// A validated bangbang-native snapshot artifact family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSnapshotArtifactFamily {
    /// Native-v1 state commit plus eagerly loaded memory.
    V1,
    /// Native-v2 structural state plus retained private-file memory.
    V2,
}

impl fmt::Display for NativeSnapshotArtifactFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1 => formatter.write_str("native-v1"),
            Self::V2 => formatter.write_str("native-v2"),
        }
    }
}

/// Validation failure for one owned native snapshot artifact state.
#[derive(Debug)]
pub enum NativeSnapshotArtifactStateError {
    /// The bytes do not form a supported native state family.
    Format(NativeSnapshotFormatError),
    /// A native-v1 envelope does not contain a valid commit record.
    Commit(SnapshotCommitError),
    /// A native-v2 state does not contain one valid memory binding.
    Memory(SnapshotV2MemoryStateError),
    /// A constructor expected a different already-valid native family.
    UnexpectedFamily {
        expected: NativeSnapshotArtifactFamily,
        actual: NativeSnapshotArtifactFamily,
    },
    /// The native-v2 state and its embedded memory binding use different versions.
    V2VersionMismatch {
        state: SnapshotFormatVersion,
        memory: SnapshotFormatVersion,
    },
    /// Publication accepts only the exact current native-v2 writer version.
    NonCurrentV2Publication {
        state: SnapshotFormatVersion,
        memory: SnapshotFormatVersion,
    },
}

impl fmt::Display for NativeSnapshotArtifactStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(source) => write!(formatter, "invalid native snapshot state: {source}"),
            Self::Commit(source) => write!(formatter, "invalid native-v1 commit: {source}"),
            Self::Memory(source) => write!(formatter, "invalid native-v2 memory state: {source}"),
            Self::UnexpectedFamily { expected, actual } => {
                write!(formatter, "expected {expected} state, found {actual}")
            }
            Self::V2VersionMismatch { state, memory } => write!(
                formatter,
                "native-v2 state version {state} does not match memory version {memory}"
            ),
            Self::NonCurrentV2Publication { state, memory } => write!(
                formatter,
                "native-v2 publication requires current state and memory versions; found state {state} and memory {memory}"
            ),
        }
    }
}

impl std::error::Error for NativeSnapshotArtifactStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::UnexpectedFamily { .. }
            | Self::V2VersionMismatch { .. }
            | Self::NonCurrentV2Publication { .. } => None,
        }
    }
}

enum NativeSnapshotArtifactStateInner {
    V1(SnapshotCommitRecord),
    V2 {
        bytes: Vec<u8>,
        version: SnapshotFormatVersion,
        binding: SnapshotV2MemoryBinding,
    },
}

/// An owned, closed state-to-memory commitment for one native artifact family.
///
/// Native-v2 callers supply only encoded state bytes. The memory binding is
/// always derived from those exact bytes and cannot be substituted separately.
pub struct NativeSnapshotArtifactState {
    inner: NativeSnapshotArtifactStateInner,
}

impl NativeSnapshotArtifactState {
    /// Retains one already validated native-v1 commit record.
    pub const fn from_v1(record: SnapshotCommitRecord) -> Self {
        Self {
            inner: NativeSnapshotArtifactStateInner::V1(record),
        }
    }

    /// Validates exact current-version native-v2 bytes for publication.
    pub fn from_current_v2(bytes: Vec<u8>) -> Result<Self, NativeSnapshotArtifactStateError> {
        let state = decode_native_snapshot_state(&bytes)
            .map_err(NativeSnapshotArtifactStateError::Format)?;
        let NativeSnapshotState::V2(state) = state else {
            return Err(NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual: NativeSnapshotArtifactFamily::V1,
            });
        };
        let version = state.metadata().version();
        let binding = decode_snapshot_v2_memory_binding(&state)
            .map_err(NativeSnapshotArtifactStateError::Memory)?;
        if version != binding.version() {
            return Err(NativeSnapshotArtifactStateError::V2VersionMismatch {
                state: version,
                memory: binding.version(),
            });
        }
        if version != NATIVE_V2_SNAPSHOT_VERSION || binding.version() != NATIVE_V2_SNAPSHOT_VERSION
        {
            return Err(NativeSnapshotArtifactStateError::NonCurrentV2Publication {
                state: version,
                memory: binding.version(),
            });
        }
        Ok(Self {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version,
                binding,
            },
        })
    }

    #[cfg(target_os = "macos")]
    fn from_compatible_bytes(bytes: Vec<u8>) -> Result<Self, NativeSnapshotArtifactStateError> {
        match decode_native_snapshot_state(&bytes)
            .map_err(NativeSnapshotArtifactStateError::Format)?
        {
            NativeSnapshotState::V1(_) => {
                let record = decode_snapshot_commit_envelope(&bytes)
                    .map_err(NativeSnapshotArtifactStateError::Commit)?;
                Ok(Self::from_v1(record))
            }
            NativeSnapshotState::V2(state) => {
                let version = state.metadata().version();
                let binding = decode_snapshot_v2_memory_binding(&state)
                    .map_err(NativeSnapshotArtifactStateError::Memory)?;
                if version != binding.version() {
                    return Err(NativeSnapshotArtifactStateError::V2VersionMismatch {
                        state: version,
                        memory: binding.version(),
                    });
                }
                Ok(Self {
                    inner: NativeSnapshotArtifactStateInner::V2 {
                        bytes,
                        version,
                        binding,
                    },
                })
            }
        }
    }

    /// Returns the validated native artifact family.
    pub const fn family(&self) -> NativeSnapshotArtifactFamily {
        match &self.inner {
            NativeSnapshotArtifactStateInner::V1(_) => NativeSnapshotArtifactFamily::V1,
            NativeSnapshotArtifactStateInner::V2 { .. } => NativeSnapshotArtifactFamily::V2,
        }
    }

    /// Returns the exact admitted state-format version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        match &self.inner {
            NativeSnapshotArtifactStateInner::V1(_) => NATIVE_V1_SNAPSHOT_VERSION,
            NativeSnapshotArtifactStateInner::V2 { version, .. } => *version,
        }
    }

    /// Returns the native-v1 record when this value belongs to native-v1.
    pub const fn v1_record(&self) -> Option<&SnapshotCommitRecord> {
        match &self.inner {
            NativeSnapshotArtifactStateInner::V1(record) => Some(record),
            NativeSnapshotArtifactStateInner::V2 { .. } => None,
        }
    }

    /// Returns the immutable encoded native-v2 state bytes.
    pub fn v2_bytes(&self) -> Option<&[u8]> {
        match &self.inner {
            NativeSnapshotArtifactStateInner::V1(_) => None,
            NativeSnapshotArtifactStateInner::V2 { bytes, .. } => Some(bytes),
        }
    }

    /// Returns the binding derived from the immutable native-v2 state bytes.
    pub const fn v2_memory_binding(&self) -> Option<&SnapshotV2MemoryBinding> {
        match &self.inner {
            NativeSnapshotArtifactStateInner::V1(_) => None,
            NativeSnapshotArtifactStateInner::V2 { binding, .. } => Some(binding),
        }
    }

    /// Consumes a native-v1 value into its exact commit record.
    pub fn into_v1_record(self) -> Result<SnapshotCommitRecord, Self> {
        match self.inner {
            NativeSnapshotArtifactStateInner::V1(record) => Ok(record),
            inner @ NativeSnapshotArtifactStateInner::V2 { .. } => Err(Self { inner }),
        }
    }

    /// Consumes a native-v2 value into its exact bytes and derived binding.
    pub fn into_v2_parts(self) -> Result<(Vec<u8>, SnapshotV2MemoryBinding), Self> {
        match self.inner {
            NativeSnapshotArtifactStateInner::V1(record) => Err(Self::from_v1(record)),
            NativeSnapshotArtifactStateInner::V2 { bytes, binding, .. } => Ok((bytes, binding)),
        }
    }

    #[cfg(target_os = "macos")]
    fn publication_bytes(&self) -> Result<Cow<'_, [u8]>, SnapshotCommitError> {
        match &self.inner {
            NativeSnapshotArtifactStateInner::V1(record) => {
                encode_snapshot_commit_envelope(record).map(Cow::Owned)
            }
            NativeSnapshotArtifactStateInner::V2 { bytes, .. } => {
                Ok(Cow::Borrowed(bytes.as_slice()))
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn validate_for_publication(&self) -> Result<(), NativeSnapshotArtifactStateError> {
        let NativeSnapshotArtifactStateInner::V2 {
            version, binding, ..
        } = &self.inner
        else {
            return Ok(());
        };
        if *version == NATIVE_V2_SNAPSHOT_VERSION && binding.version() == NATIVE_V2_SNAPSHOT_VERSION
        {
            Ok(())
        } else {
            Err(NativeSnapshotArtifactStateError::NonCurrentV2Publication {
                state: *version,
                memory: binding.version(),
            })
        }
    }
}

impl fmt::Debug for NativeSnapshotArtifactState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSnapshotArtifactState")
            .field("family", &self.family())
            .field("version", &self.version())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .finish()
    }
}

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

enum SnapshotArtifactOutputLocation {
    Path(PathBuf),
    Anchored {
        directory: File,
        child: Vec<u8>,
        tracker: Option<Arc<dyn SnapshotStagingTracker>>,
    },
}

/// One native snapshot final destination, either path-based or anchor-relative.
pub struct SnapshotArtifactOutput {
    location: SnapshotArtifactOutputLocation,
}

impl SnapshotArtifactOutput {
    /// Creates one ordinary path-based final destination.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            location: SnapshotArtifactOutputLocation::Path(path.into()),
        }
    }

    /// Creates one final destination relative to an already-opened directory.
    pub fn anchored(directory: File, child: impl Into<Vec<u8>>) -> Self {
        Self {
            location: SnapshotArtifactOutputLocation::Anchored {
                directory,
                child: child.into(),
                tracker: None,
            },
        }
    }

    /// Creates an anchored destination with durable worker-first staging evidence.
    pub fn anchored_tracked(
        directory: File,
        child: impl Into<Vec<u8>>,
        tracker: Arc<dyn SnapshotStagingTracker>,
    ) -> Self {
        Self {
            location: SnapshotArtifactOutputLocation::Anchored {
                directory,
                child: child.into(),
                tracker: Some(tracker),
            },
        }
    }
}

impl fmt::Debug for SnapshotArtifactOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotArtifactOutput")
            .field("destination", &REDACTED)
            .finish()
    }
}

/// Independently authorized state and memory final destinations.
pub struct SnapshotArtifactOutputs {
    state: SnapshotArtifactOutput,
    memory: SnapshotArtifactOutput,
}

impl SnapshotArtifactOutputs {
    /// Creates one state/memory destination pair.
    pub const fn new(state: SnapshotArtifactOutput, memory: SnapshotArtifactOutput) -> Self {
        Self { state, memory }
    }

    fn from_paths(paths: &SnapshotArtifactPaths) -> Self {
        Self::new(
            SnapshotArtifactOutput::path(paths.state()),
            SnapshotArtifactOutput::path(paths.memory()),
        )
    }

    fn state(&self) -> &SnapshotArtifactOutput {
        &self.state
    }

    fn memory(&self) -> &SnapshotArtifactOutput {
        &self.memory
    }
}

impl fmt::Debug for SnapshotArtifactOutputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotArtifactOutputs")
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

/// Stable device/inode identity used only by private staging cleanup.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotArtifactIdentity {
    device: u64,
    inode: u64,
}

impl SnapshotArtifactIdentity {
    /// Creates one exact filesystem identity.
    pub const fn new(device: u64, inode: u64) -> Self {
        Self { device, inode }
    }

    /// Returns the normalized device number.
    pub const fn device(self) -> u64 {
        self.device
    }

    /// Returns the inode number.
    pub const fn inode(self) -> u64 {
        self.inode
    }
}

impl fmt::Debug for SnapshotArtifactIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SnapshotArtifactIdentity(<redacted>)")
    }
}

/// Private exact ownership evidence for one active staging inode.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotStagingOwnership {
    artifact: SnapshotArtifactKind,
    directory_identity: SnapshotArtifactIdentity,
    component: Vec<u8>,
    file_identity: SnapshotArtifactIdentity,
}

impl SnapshotStagingOwnership {
    fn new(
        artifact: SnapshotArtifactKind,
        directory_identity: SnapshotArtifactIdentity,
        component: Vec<u8>,
        file_identity: SnapshotArtifactIdentity,
    ) -> Self {
        Self {
            artifact,
            directory_identity,
            component,
            file_identity,
        }
    }

    /// Returns the state or memory artifact kind.
    pub const fn artifact(&self) -> SnapshotArtifactKind {
        self.artifact
    }

    /// Returns the exact opened directory identity.
    pub const fn directory_identity(&self) -> SnapshotArtifactIdentity {
        self.directory_identity
    }

    /// Returns the private random staging component.
    pub fn component(&self) -> &[u8] {
        &self.component
    }

    /// Returns the exact staging inode identity.
    pub const fn file_identity(&self) -> SnapshotArtifactIdentity {
        self.file_identity
    }
}

impl fmt::Debug for SnapshotStagingOwnership {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotStagingOwnership")
            .field("artifact", &self.artifact)
            .field("directory_identity", &REDACTED)
            .field("component", &REDACTED)
            .field("file_identity", &REDACTED)
            .finish()
    }
}

/// Redacted failure to persist or clear private staging ownership evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStagingTrackingError;

/// Session-owned durable tracker for granted external staging inodes.
pub trait SnapshotStagingTracker: fmt::Debug + Send + Sync {
    /// Persists exact evidence before artifact content is produced.
    fn record(
        &self,
        ownership: &SnapshotStagingOwnership,
    ) -> Result<(), SnapshotStagingTrackingError>;

    /// Clears only the exact current evidence after conclusive disposition.
    fn clear(
        &self,
        ownership: &SnapshotStagingOwnership,
    ) -> Result<(), SnapshotStagingTrackingError>;
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
    MemoryV2Verify(SnapshotV2MemoryLoadError),
    NativeState(NativeSnapshotArtifactStateError),
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
            Self::MemoryV2Verify(source) => {
                write!(f, "native-v2 memory staging verification failed: {source}")
            }
            Self::NativeState(source) => {
                write!(f, "snapshot state cannot be published: {source}")
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
            Self::MemoryV2Verify(source) => Some(source),
            Self::NativeState(source) => Some(source),
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
    /// Creates a typed producer failure before publication owns staging.
    ///
    /// Cleanup dispositions are absent because no private staging artifact has
    /// been created by the publication transaction yet.
    pub fn from_producer(source: E) -> Self {
        Self::Producer(SnapshotPublicationProducerError::new(source))
    }

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

/// Successful or visibly committed native-family artifact publication.
pub struct NativeSnapshotPublicationOutcome {
    state: NativeSnapshotArtifactState,
    durability: SnapshotCommitDurability,
}

impl NativeSnapshotPublicationOutcome {
    /// Returns the closed state-to-memory commitment that was published.
    pub const fn state(&self) -> &NativeSnapshotArtifactState {
        &self.state
    }

    /// Returns the published native artifact family.
    pub const fn family(&self) -> NativeSnapshotArtifactFamily {
        self.state.family()
    }

    /// Returns the post-commit durability classification.
    pub const fn durability(&self) -> SnapshotCommitDurability {
        self.durability
    }

    /// Consumes the outcome into its state commitment and durability.
    pub fn into_parts(self) -> (NativeSnapshotArtifactState, SnapshotCommitDurability) {
        (self.state, self.durability)
    }
}

impl fmt::Debug for NativeSnapshotPublicationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSnapshotPublicationOutcome")
            .field("family", &self.family())
            .field("version", &self.state.version())
            .field("state", &REDACTED)
            .field("durability", &self.durability)
            .finish()
    }
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
    NativeState(NativeSnapshotArtifactStateError),
    MemoryV2(SnapshotV2MemoryLoadError),
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
            Self::NativeState(source) => write!(f, "invalid native snapshot state: {source}"),
            Self::MemoryV2(source) => {
                write!(f, "invalid native-v2 snapshot memory image: {source}")
            }
        }
    }
}

impl std::error::Error for SnapshotArtifactLoadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocationFailed { source } => Some(source),
            Self::Commit(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::NativeState(source) => Some(source),
            Self::MemoryV2(source) => Some(source),
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

/// A fully validated native-family artifact pair loaded without constructing a VM.
pub struct LoadedNativeSnapshotArtifacts {
    state: NativeSnapshotArtifactState,
    memory: GuestMemory,
}

impl LoadedNativeSnapshotArtifacts {
    /// Returns the validated state-to-memory commitment.
    pub const fn state(&self) -> &NativeSnapshotArtifactState {
        &self.state
    }

    /// Returns the loaded native family.
    pub const fn family(&self) -> NativeSnapshotArtifactFamily {
        self.state.family()
    }

    /// Returns the loaded guest memory.
    pub const fn memory(&self) -> &GuestMemory {
        &self.memory
    }

    /// Consumes the result into its state commitment and guest memory.
    pub fn into_parts(self) -> (NativeSnapshotArtifactState, GuestMemory) {
        (self.state, self.memory)
    }

    /// Consumes an already-validated native-v1 pair into the frozen legacy
    /// loader representation without reopening or re-encoding either
    /// artifact.
    pub fn into_v1(self) -> Result<LoadedSnapshotArtifacts, NativeSnapshotArtifactStateError> {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let record = state.into_v1_record().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V1,
                actual,
            }
        })?;
        Ok(LoadedSnapshotArtifacts { record, memory })
    }
}

impl fmt::Debug for LoadedNativeSnapshotArtifacts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedNativeSnapshotArtifacts")
            .field("family", &self.family())
            .field("version", &self.state.version())
            .field("state", &REDACTED)
            .field("memory_range_count", &self.memory.regions().len())
            .field("memory_bytes", &self.memory.total_size())
            .finish()
    }
}

/// A bounded native-family state retained for later exact memory adoption.
pub struct PreparedNativeSnapshotState {
    state: NativeSnapshotArtifactState,
}

impl PreparedNativeSnapshotState {
    /// Retains one already validated closed state commitment.
    pub const fn from_state(state: NativeSnapshotArtifactState) -> Self {
        Self { state }
    }

    /// Returns the validated closed state commitment.
    pub const fn state(&self) -> &NativeSnapshotArtifactState {
        &self.state
    }

    /// Returns the prepared native family.
    pub const fn family(&self) -> NativeSnapshotArtifactFamily {
        self.state.family()
    }

    /// Consumes the prepared value into its state commitment.
    pub fn into_state(self) -> NativeSnapshotArtifactState {
        self.state
    }

    /// Consumes already-decoded native-v1 state into the frozen legacy
    /// prepared-state representation without reopening or re-encoding it.
    pub fn into_v1(self) -> Result<PreparedSnapshotState, NativeSnapshotArtifactStateError> {
        let actual = self.family();
        let record = self.state.into_v1_record().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V1,
                actual,
            }
        })?;
        Ok(PreparedSnapshotState::from_record(record))
    }
}

impl fmt::Debug for PreparedNativeSnapshotState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedNativeSnapshotState")
            .field("family", &self.family())
            .field("version", &self.state.version())
            .field("state", &REDACTED)
            .finish()
    }
}

/// A fully validated committed pair loaded into anonymous guest memory.
pub struct LoadedSnapshotArtifacts {
    record: SnapshotCommitRecord,
    memory: GuestMemory,
}

/// A bounded, decoded state commit retained for later exact memory adoption.
pub struct PreparedSnapshotState {
    record: SnapshotCommitRecord,
}

impl PreparedSnapshotState {
    /// Retains an already validated commit record for a later memory load.
    pub const fn from_record(record: SnapshotCommitRecord) -> Self {
        Self { record }
    }

    /// Returns the validated commit record without exposing artifact paths.
    pub const fn record(&self) -> &SnapshotCommitRecord {
        &self.record
    }

    /// Consumes the prepared state into its validated commit record.
    pub fn into_record(self) -> SnapshotCommitRecord {
        self.record
    }
}

impl fmt::Debug for PreparedSnapshotState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedSnapshotState")
            .field("record", &REDACTED)
            .finish()
    }
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
    let outputs = SnapshotArtifactOutputs::from_paths(paths);
    publish_snapshot_artifacts_to_with(&outputs, producer)
}

/// Publishes caller-produced native-v1 or native-v2 artifacts in one transaction.
///
/// The producer must return a closed state value whose memory commitment was
/// derived from that state. Publication verifies the staged memory against the
/// selected family before either final name becomes visible.
pub fn publish_native_snapshot_artifacts_with<E, F>(
    paths: &SnapshotArtifactPaths,
    producer: F,
) -> Result<NativeSnapshotPublicationOutcome, SnapshotPublicationTransactionError<E>>
where
    F: FnOnce(SnapshotMemoryStagingWriter) -> Result<NativeSnapshotArtifactState, E>,
{
    let outputs = SnapshotArtifactOutputs::from_paths(paths);
    publish_native_snapshot_artifacts_to_with(&outputs, producer)
}

/// Publishes through path-based or already-opened directory destinations.
pub fn publish_snapshot_artifacts_to_with<E, F>(
    outputs: &SnapshotArtifactOutputs,
    producer: F,
) -> Result<SnapshotPublicationOutcome, SnapshotPublicationTransactionError<E>>
where
    F: FnOnce(SnapshotMemoryStagingWriter) -> Result<SnapshotCommitRecord, E>,
{
    #[cfg(target_os = "macos")]
    {
        publish_snapshot_artifacts_macos_with(outputs, producer)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (outputs, producer);
        Err(SnapshotPublicationTransactionError::Publication(
            publication_error(
                SnapshotPublicationStage::PlatformCheck,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::UnsupportedPlatform,
            ),
        ))
    }
}

/// Publishes one closed native-family pair to path or anchored destinations.
pub fn publish_native_snapshot_artifacts_to_with<E, F>(
    outputs: &SnapshotArtifactOutputs,
    producer: F,
) -> Result<NativeSnapshotPublicationOutcome, SnapshotPublicationTransactionError<E>>
where
    F: FnOnce(SnapshotMemoryStagingWriter) -> Result<NativeSnapshotArtifactState, E>,
{
    #[cfg(target_os = "macos")]
    {
        publish_native_snapshot_artifacts_macos_with(outputs, producer)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (outputs, producer);
        Err(SnapshotPublicationTransactionError::Publication(
            publication_error(
                SnapshotPublicationStage::PlatformCheck,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::UnsupportedPlatform,
            ),
        ))
    }
}

/// Loads a validated native-v1 or native-v2 pair without constructing a VM.
pub fn load_native_snapshot_artifacts(
    paths: &SnapshotArtifactPaths,
) -> Result<LoadedNativeSnapshotArtifacts, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        load_native_snapshot_artifacts_macos(paths)
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

/// Decodes one already-opened native-v1 or native-v2 state artifact.
pub fn prepare_native_snapshot_state_file(
    file: File,
) -> Result<PreparedNativeSnapshotState, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        prepare_native_snapshot_state_file_macos(file)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Decodes one already-opened regular state artifact without consuming a VM.
pub fn prepare_snapshot_state_file(
    file: File,
) -> Result<PreparedSnapshotState, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        prepare_snapshot_state_file_macos(file)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Opens and decodes one native-family state artifact path.
pub fn prepare_native_snapshot_state_path(
    path: &Path,
) -> Result<PreparedNativeSnapshotState, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        prepare_native_snapshot_state_path_macos(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Opens and decodes one state artifact path without loading guest memory.
pub fn prepare_snapshot_state_path(
    path: &Path,
) -> Result<PreparedSnapshotState, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        prepare_snapshot_state_path_macos(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Loads one opened memory artifact against prepared native-family state.
pub fn load_prepared_native_snapshot_memory_file(
    prepared: PreparedNativeSnapshotState,
    file: File,
) -> Result<LoadedNativeSnapshotArtifacts, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        load_prepared_native_snapshot_memory_file_macos(prepared, file)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (prepared, file);
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Loads one already-opened memory artifact against a prepared state commit.
pub fn load_prepared_snapshot_memory_file(
    prepared: PreparedSnapshotState,
    file: File,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        load_prepared_snapshot_memory_file_macos(prepared, file)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (prepared, file);
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Opens and loads memory against prepared native-family state.
pub fn load_prepared_native_snapshot_memory_path(
    prepared: PreparedNativeSnapshotState,
    path: &Path,
) -> Result<LoadedNativeSnapshotArtifacts, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        load_prepared_native_snapshot_memory_path_macos(prepared, path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (prepared, path);
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Opens and loads one memory artifact path against a prepared state commit.
pub fn load_prepared_snapshot_memory_path(
    prepared: PreparedSnapshotState,
    path: &Path,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    #[cfg(target_os = "macos")]
    {
        load_prepared_snapshot_memory_path_macos(prepared, path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (prepared, path);
        Err(load_error(
            SnapshotArtifactLoadStage::PlatformCheck,
            SnapshotArtifactLoadFailure::UnsupportedPlatform,
        ))
    }
}

/// Loads opened native-family state and memory descriptors in state-first order.
pub fn load_native_snapshot_artifact_files(
    state: File,
    memory: File,
) -> Result<LoadedNativeSnapshotArtifacts, SnapshotArtifactLoadError> {
    let prepared = prepare_native_snapshot_state_file(state)?;
    load_prepared_native_snapshot_memory_file(prepared, memory)
}

/// Loads an already-opened state/memory pair through the ordinary validation path.
pub fn load_snapshot_artifact_files(
    state: File,
    memory: File,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    let prepared = prepare_snapshot_state_file(state)?;
    load_prepared_snapshot_memory_file(prepared, memory)
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
use macos::{
    load_native_snapshot_artifacts_macos, load_prepared_native_snapshot_memory_file_macos,
    load_prepared_native_snapshot_memory_path_macos, load_prepared_snapshot_memory_file_macos,
    load_prepared_snapshot_memory_path_macos, load_snapshot_artifacts_macos,
    prepare_native_snapshot_state_file_macos, prepare_native_snapshot_state_path_macos,
    prepare_snapshot_state_file_macos, prepare_snapshot_state_path_macos,
    publish_native_snapshot_artifacts_macos_with, publish_snapshot_artifacts_macos_with,
};

#[cfg(test)]
mod tests;
