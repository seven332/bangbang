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
use crate::snapshot::SnapshotNetworkOverride;
use crate::snapshot_balloon_v2_9::{
    NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, SnapshotV2BalloonState,
    SnapshotV2BalloonStateDecodeError,
};
use crate::snapshot_commit::{SnapshotCommitError, SnapshotCommitRecord};
#[cfg(target_os = "macos")]
use crate::snapshot_commit::{decode_snapshot_commit_envelope, encode_snapshot_commit_envelope};
use crate::snapshot_device_v2::{
    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2DeviceGraph,
    SnapshotV2DeviceGraphDecodeError,
};
use crate::snapshot_device_v2_5::{
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2MultiBlockDeviceGraph,
    SnapshotV2MultiBlockDeviceGraphDecodeError,
};
use crate::snapshot_device_v2_6::{
    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2StorageDeviceGraph,
    SnapshotV2StorageDeviceGraphDecodeError,
};
use crate::snapshot_entropy_v2_8::{
    NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, SnapshotV2EntropyState,
    SnapshotV2EntropyStateDecodeError,
};
#[cfg(target_os = "macos")]
use crate::snapshot_format::NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES;
use crate::snapshot_format::{
    NATIVE_V1_SNAPSHOT_VERSION, NativeSnapshotFormatError, NativeSnapshotState,
    SnapshotFormatVersion, decode_native_snapshot_state,
};
use crate::snapshot_format_v2::{
    NATIVE_V2_BALLOON_COMPONENT_KEY, NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
    NATIVE_V2_ENTROPY_COMPONENT_KEY, NATIVE_V2_LEGACY_PLATFORM_VERSION,
    NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY, NATIVE_V2_NETWORK_COMPONENT_KEY,
    NATIVE_V2_SERIAL_COMPONENT_KEY, NATIVE_V2_SNAPSHOT_VERSION, SnapshotV2ComponentDisposition,
    SnapshotV2DecodeError, decode_snapshot_v2_state_with_compatibility_version,
};
#[cfg(target_os = "macos")]
use crate::snapshot_format_v2::{NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES, decode_snapshot_v2_state};
use crate::snapshot_memory::{
    SnapshotMemoryLoadError, SnapshotMemoryWriteError, write_snapshot_memory_image,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory::{load_snapshot_memory_image, verify_snapshot_memory_image_output};
use crate::snapshot_memory_hotplug_v2_10::{
    NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION, PreparedSnapshotV2MemoryHotplugTopology,
    SnapshotV2MemoryHotplugBindingError, SnapshotV2MemoryHotplugPreparationError,
    SnapshotV2MemoryHotplugState, SnapshotV2MemoryHotplugStateDecodeError,
};
use crate::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, SnapshotV2MemoryHotplugMaterializationError,
    SnapshotV2MemoryHotplugMaterializationStage, SnapshotV2MemoryLoadError,
    SnapshotV2MemoryStateError, decode_snapshot_v2_memory_binding,
    materialize_snapshot_v2_memory_hotplug_file,
    materialize_snapshot_v2_memory_hotplug_file_with_cancel,
};
#[cfg(target_os = "macos")]
use crate::snapshot_memory_v2::{
    load_snapshot_v2_memory_file, verify_snapshot_v2_memory_image_output,
};
use crate::snapshot_network_restore_v2_11::{
    PreparedSnapshotV2NetworkRestoreTopology, SnapshotV2NetworkRestorePreparationError,
    SnapshotV2NetworkRestorePreparationStage,
};
use crate::snapshot_network_v2_11::{
    NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, SnapshotV2NetworkState,
    SnapshotV2NetworkStateDecodeError,
};
use crate::snapshot_restore::{SnapshotRestoreManifest, SnapshotRestoreManifestError};
use crate::snapshot_serial_v2_7::{
    NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, SnapshotV2SerialState,
    SnapshotV2SerialStateDecodeError,
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

/// One complete, exact native-v2 state profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeV2SnapshotArtifactProfile {
    /// Exact 2.3 platform state without a device graph.
    LegacyPlatformV2_3,
    /// Exact 2.4 singleton block-device graph profile 1.
    DeviceGraphV2_4,
    /// Exact 2.5 multi-block device graph profile 2.
    MultiBlockDeviceGraphV2_5,
    /// Exact 2.6 block-and-pmem storage graph profile 3.
    StorageDeviceGraphV2_6,
    /// Exact 2.7 serial profile with optional unchanged profile-3 storage.
    SerialStateV2_7,
    /// Exact 2.8 profile with required serial and optional storage/entropy.
    EntropyStateV2_8,
    /// Exact 2.9 profile with required serial and optional storage/entropy/balloon.
    BalloonStateV2_9,
    /// Exact 2.10 profile with optional storage, entropy, balloon, and virtio-mem.
    MemoryHotplugStateV2_10,
    /// Exact 2.11 profile with optional unchanged devices and network/MMDS.
    NetworkStateV2_11,
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
    /// A compatible native-v2 state does not satisfy one exact state profile.
    V2Profile(NativeV2SnapshotCandidateStateError),
    /// The exact current native-v2 state does not satisfy its required device profile.
    CurrentV2Profile(NativeV2SnapshotCandidateStateError),
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
            Self::V2Profile(source) => {
                write!(formatter, "invalid compatible native-v2 profile: {source}")
            }
            Self::CurrentV2Profile(source) => {
                write!(
                    formatter,
                    "invalid current native-v2 device profile: {source}"
                )
            }
        }
    }
}

impl std::error::Error for NativeSnapshotArtifactStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::V2Profile(source) => Some(source),
            Self::CurrentV2Profile(source) => Some(source),
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
        NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(bytes)
            .map(NativeV2MemoryHotplugSnapshotCandidateState::into_current_artifact_state)
            .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)
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

    /// Classifies one compatible native-v2 state into its exact typed profile.
    ///
    /// This inspects only the already-retained immutable state and embedded
    /// memory commitment. It does not open memory or device backing resources.
    pub fn v2_profile(
        &self,
    ) -> Result<NativeV2SnapshotArtifactProfile, NativeSnapshotArtifactStateError> {
        let NativeSnapshotArtifactStateInner::V2 { bytes, binding, .. } = &self.inner else {
            return Err(NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual: NativeSnapshotArtifactFamily::V1,
            });
        };
        classify_native_v2_profile(bytes, binding)
            .map_err(NativeSnapshotArtifactStateError::V2Profile)
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
            bytes,
            version,
            binding,
        } = &self.inner
        else {
            return Ok(());
        };
        if *version == NATIVE_V2_SNAPSHOT_VERSION && binding.version() == NATIVE_V2_SNAPSHOT_VERSION
        {
            let (actual_binding, _, _, _, _, _) = decode_memory_hotplug_state_v2_10(bytes)
                .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
            if &actual_binding == binding {
                Ok(())
            } else {
                Err(NativeSnapshotArtifactStateError::V2VersionMismatch {
                    state: *version,
                    memory: binding.version(),
                })
            }
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

/// One closed exact native-v2 2.4 candidate.
///
/// This value retains the state bytes, the memory commitment derived from
/// those exact bytes, and the required validated device graph. It can enter the
/// private publication-state contract only through its consuming checked
/// conversion.
pub struct NativeV2SnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: SnapshotV2DeviceGraph,
}

impl NativeV2SnapshotCandidateState {
    /// Validate and retain one exact graph-bearing native-v2 2.4 state.
    pub fn from_device_graph_v2_4(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph) = decode_device_graph_v2_4(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
        })
    }

    /// Return the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
    }

    /// Return the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Return the required validated singleton device graph.
    pub const fn device_graph(&self) -> &SnapshotV2DeviceGraph {
        &self.device_graph
    }

    /// Consumes the closed candidate into its exact committed components.
    pub fn into_parts(self) -> (Vec<u8>, SnapshotV2MemoryBinding, SnapshotV2DeviceGraph) {
        (self.bytes, self.binding, self.device_graph)
    }

    /// Consumes this exact 2.4 candidate into compatible artifact state.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

/// One closed exact native-v2 2.5 multi-block candidate.
///
/// This value retains the original state bytes, their derived memory
/// commitment, and the validated profile-2 graph.
pub struct NativeV2MultiBlockSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: SnapshotV2MultiBlockDeviceGraph,
}

impl NativeV2MultiBlockSnapshotCandidateState {
    /// Validates and retains one exact graph-bearing native-v2 2.5 state.
    pub fn from_device_graph_v2_5(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph) = decode_device_graph_v2_5(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
        })
    }

    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the required validated profile-2 device graph.
    pub const fn device_graph(&self) -> &SnapshotV2MultiBlockDeviceGraph {
        &self.device_graph
    }

    /// Consumes the closed candidate into its exact committed components.
    pub fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        SnapshotV2MemoryBinding,
        SnapshotV2MultiBlockDeviceGraph,
    ) {
        (self.bytes, self.binding, self.device_graph)
    }

    /// Consumes this exact 2.5 candidate into compatible artifact state.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2MultiBlockSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2MultiBlockSnapshotCandidateState")
            .field("version", &self.version())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("device_graph", &REDACTED)
            .finish()
    }
}

/// One closed exact native-v2 2.6 storage candidate.
///
/// This value retains the original state bytes, their derived memory
/// commitment, and the validated profile-3 block-and-pmem graph. It is the
/// only native-v2 candidate that can enter the current publication authority.
pub struct NativeV2StorageSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: SnapshotV2StorageDeviceGraph,
}

impl NativeV2StorageSnapshotCandidateState {
    /// Validates and retains one exact graph-bearing native-v2 2.6 state.
    pub fn from_storage_device_graph_v2_6(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph) = decode_storage_device_graph_v2_6(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
        })
    }

    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the required validated profile-3 storage graph.
    pub const fn device_graph(&self) -> &SnapshotV2StorageDeviceGraph {
        &self.device_graph
    }

    /// Consumes the closed candidate into its exact committed components.
    pub fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        SnapshotV2MemoryBinding,
        SnapshotV2StorageDeviceGraph,
    ) {
        (self.bytes, self.binding, self.device_graph)
    }

    /// Consumes this exact 2.6 candidate into compatible artifact authority.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2StorageSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2StorageSnapshotCandidateState")
            .field("version", &self.version())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("device_graph", &REDACTED)
            .finish()
    }
}

/// One closed exact native-v2 2.7 serial candidate.
///
/// The required serial singleton and optional unchanged profile-3 storage
/// graph are derived from the same immutable bytes as the retained memory
/// commitment. This is the only native-v2 candidate that can enter the
/// retained exact-2.7 compatibility authority.
pub struct NativeV2SerialSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
}

impl NativeV2SerialSnapshotCandidateState {
    /// Validates and retains one exact serial-bearing native-v2 2.7 state.
    pub fn from_serial_state_v2_7(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph, serial) = decode_serial_state_v2_7(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
            serial,
        })
    }

    /// Returns the exact retained compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the optional unchanged profile-3 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required complete serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Consumes the closed candidate into its exact committed components.
    pub fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        SnapshotV2MemoryBinding,
        Option<SnapshotV2StorageDeviceGraph>,
        SnapshotV2SerialState,
    ) {
        (self.bytes, self.binding, self.device_graph, self.serial)
    }

    /// Consumes this exact 2.7 candidate into compatible artifact authority.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2SerialSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2SerialSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

/// One closed exact native-v2 2.8 entropy candidate.
///
/// The required unchanged serial singleton, optional unchanged profile-3
/// storage graph, optional entropy singleton, and memory binding are all
/// derived from the same immutable bytes. This is the only native-v2
/// candidate retained as the exact-2.8 compatibility handoff.
pub struct NativeV2EntropySnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
}

impl NativeV2EntropySnapshotCandidateState {
    /// Validates and retains one exact native-v2 2.8 state.
    pub fn from_entropy_state_v2_8(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph, serial, entropy) = decode_entropy_state_v2_8(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
            serial,
            entropy,
        })
    }

    /// Returns the exact current compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the optional unchanged profile-3 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Consumes the candidate into its exact committed components.
    pub fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        SnapshotV2MemoryBinding,
        Option<SnapshotV2StorageDeviceGraph>,
        SnapshotV2SerialState,
        Option<SnapshotV2EntropyState>,
    ) {
        (
            self.bytes,
            self.binding,
            self.device_graph,
            self.serial,
            self.entropy,
        )
    }

    /// Consumes this exact 2.8 candidate into compatible artifact authority.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _, _, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2EntropySnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2EntropySnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

/// One closed exact native-v2 2.9 balloon candidate.
///
/// The required unchanged serial singleton, independently optional unchanged
/// profile-3 storage graph and entropy singleton, optional balloon singleton,
/// and memory binding are all derived from the same immutable bytes. This
/// candidate retained as the exact-2.9 compatibility handoff.
pub struct NativeV2BalloonSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
    balloon: Option<SnapshotV2BalloonState>,
}

impl NativeV2BalloonSnapshotCandidateState {
    /// Validates and retains one exact native-v2 2.9 state.
    pub fn from_balloon_state_v2_9(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph, serial, entropy, balloon) = decode_balloon_state_v2_9(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
            serial,
            entropy,
            balloon,
        })
    }

    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the optional unchanged profile-3 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional unchanged exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Returns the optional exact-2.9 balloon state.
    pub const fn balloon(&self) -> Option<&SnapshotV2BalloonState> {
        self.balloon.as_ref()
    }

    /// Consumes the candidate into its exact committed components.
    pub fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        SnapshotV2MemoryBinding,
        Option<SnapshotV2StorageDeviceGraph>,
        SnapshotV2SerialState,
        Option<SnapshotV2EntropyState>,
        Option<SnapshotV2BalloonState>,
    ) {
        (
            self.bytes,
            self.binding,
            self.device_graph,
            self.serial,
            self.entropy,
            self.balloon,
        )
    }

    /// Consumes this exact 2.9 candidate into compatible artifact authority.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _, _, _, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2BalloonSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2BalloonSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("has_balloon", &self.balloon.is_some())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

/// One closed exact native-v2 2.10 virtio-mem candidate.
///
/// Required serial, independently optional unchanged profile-3 storage,
/// entropy and balloon state, optional virtio-mem state, and the memory
/// binding are all derived from the same immutable bytes. This is the exact
/// current candidate that can enter public publication authority.
pub struct NativeV2MemoryHotplugSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
    balloon: Option<SnapshotV2BalloonState>,
    memory_hotplug: Option<SnapshotV2MemoryHotplugState>,
}

/// Owned parts retained by one exact native-v2 2.10 virtio-mem candidate.
pub type NativeV2MemoryHotplugSnapshotCandidateParts = (
    Vec<u8>,
    SnapshotV2MemoryBinding,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    Option<SnapshotV2MemoryHotplugState>,
);

/// Prepared exact-2.10 artifact outcome before memory mapping.
pub enum NativeV2MemoryHotplugSnapshotPreparation {
    /// The original exact candidate has no kind-11 component and remains on
    /// its compatible internal path unchanged.
    Compatible(NativeV2MemoryHotplugSnapshotCandidateState),
    /// The memory-bearing candidate has a closed owner-free virtio-mem
    /// topology.
    Prepared(PreparedNativeV2MemoryHotplugSnapshotCandidateState),
}

impl NativeV2MemoryHotplugSnapshotPreparation {
    /// Returns the unchanged compatible candidate when kind 11 was absent.
    pub const fn compatible(&self) -> Option<&NativeV2MemoryHotplugSnapshotCandidateState> {
        match self {
            Self::Compatible(candidate) => Some(candidate),
            Self::Prepared(_) => None,
        }
    }

    /// Returns the prepared memory-bearing candidate when kind 11 was present.
    pub const fn prepared(&self) -> Option<&PreparedNativeV2MemoryHotplugSnapshotCandidateState> {
        match self {
            Self::Compatible(_) => None,
            Self::Prepared(candidate) => Some(candidate),
        }
    }
}

impl fmt::Debug for NativeV2MemoryHotplugSnapshotPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = match self {
            Self::Compatible(_) => "compatible",
            Self::Prepared(_) => "prepared",
        };
        formatter
            .debug_struct("NativeV2MemoryHotplugSnapshotPreparation")
            .field("outcome", &outcome)
            .field("state", &REDACTED)
            .finish()
    }
}

/// One memory-bearing exact-2.10 candidate with prepared topology.
///
/// The candidate keeps all components from the same immutable encoded state.
/// It owns no opened memory image, mapping, host resource, device, or platform
/// VM authority.
pub struct PreparedNativeV2MemoryHotplugSnapshotCandidateState {
    bytes: Vec<u8>,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
    balloon: Option<SnapshotV2BalloonState>,
    topology: PreparedSnapshotV2MemoryHotplugTopology,
}

/// Owned exact components of one prepared memory-bearing 2.10 candidate.
pub type PreparedNativeV2MemoryHotplugSnapshotCandidateParts = (
    Vec<u8>,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    PreparedSnapshotV2MemoryHotplugTopology,
);

impl PreparedNativeV2MemoryHotplugSnapshotCandidateState {
    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the unchanged immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the exact binding attached to the prepared extent partition.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        self.topology.memory().binding()
    }

    /// Returns the optional unchanged profile-3 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional unchanged exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Returns the optional unchanged exact-2.9 balloon state.
    pub const fn balloon(&self) -> Option<&SnapshotV2BalloonState> {
        self.balloon.as_ref()
    }

    /// Returns the closed owner-free kind-1/kind-11 topology.
    pub const fn topology(&self) -> &PreparedSnapshotV2MemoryHotplugTopology {
        &self.topology
    }

    /// Materializes the exact candidate from one adopted memory descriptor.
    pub fn materialize_memory_file(
        self,
        file: File,
    ) -> Result<
        MaterializedNativeV2MemoryHotplugSnapshotCandidateState,
        SnapshotV2MemoryHotplugMaterializationError,
    > {
        let memory = materialize_snapshot_v2_memory_hotplug_file(&self.topology, file)?;
        Ok(self.with_materialized_memory(memory))
    }

    /// Materializes the exact candidate with stable cancellation checkpoints.
    pub fn materialize_memory_file_with_cancel<C>(
        self,
        file: File,
        is_cancelled: C,
    ) -> Result<
        MaterializedNativeV2MemoryHotplugSnapshotCandidateState,
        SnapshotV2MemoryHotplugMaterializationError,
    >
    where
        C: FnMut(SnapshotV2MemoryHotplugMaterializationStage) -> bool,
    {
        let memory = materialize_snapshot_v2_memory_hotplug_file_with_cancel(
            &self.topology,
            file,
            is_cancelled,
        )?;
        Ok(self.with_materialized_memory(memory))
    }

    fn with_materialized_memory(
        self,
        memory: GuestMemory,
    ) -> MaterializedNativeV2MemoryHotplugSnapshotCandidateState {
        let Self {
            bytes,
            device_graph,
            serial,
            entropy,
            balloon,
            topology,
        } = self;
        MaterializedNativeV2MemoryHotplugSnapshotCandidateState {
            bytes,
            device_graph,
            serial,
            entropy,
            balloon,
            topology,
            memory,
        }
    }

    /// Consumes the candidate into its exact still-detached components.
    pub fn into_parts(self) -> PreparedNativeV2MemoryHotplugSnapshotCandidateParts {
        (
            self.bytes,
            self.device_graph,
            self.serial,
            self.entropy,
            self.balloon,
            self.topology,
        )
    }
}

impl fmt::Debug for PreparedNativeV2MemoryHotplugSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedNativeV2MemoryHotplugSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("has_balloon", &self.balloon.is_some())
            .field("state", &REDACTED)
            .field("memory_topology", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

/// One unpublished exact-2.10 candidate with fully materialized mixed memory.
///
/// Exact encoded state, optional product components, prepared topology, and
/// destination memory remain attached until the later platform-planning
/// boundary consumes this value.
pub struct MaterializedNativeV2MemoryHotplugSnapshotCandidateState {
    bytes: Vec<u8>,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
    balloon: Option<SnapshotV2BalloonState>,
    topology: PreparedSnapshotV2MemoryHotplugTopology,
    memory: GuestMemory,
}

/// Owned exact components of one materialized memory-bearing 2.10 candidate.
pub type MaterializedNativeV2MemoryHotplugSnapshotCandidateParts = (
    Vec<u8>,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    PreparedSnapshotV2MemoryHotplugTopology,
    GuestMemory,
);

impl MaterializedNativeV2MemoryHotplugSnapshotCandidateState {
    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the unchanged immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the exact binding attached to the materialized topology.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        self.topology.memory().binding()
    }

    /// Returns the optional unchanged profile-3 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional unchanged exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Returns the optional unchanged exact-2.9 balloon state.
    pub const fn balloon(&self) -> Option<&SnapshotV2BalloonState> {
        self.balloon.as_ref()
    }

    /// Returns the complete prepared kind-1/kind-11 topology.
    pub const fn topology(&self) -> &PreparedSnapshotV2MemoryHotplugTopology {
        &self.topology
    }

    /// Returns the mixed private-base/shared-aperture guest-memory owner.
    pub const fn memory(&self) -> &GuestMemory {
        &self.memory
    }

    /// Consumes the candidate into one inseparable exact-parts handoff.
    pub fn into_parts(self) -> MaterializedNativeV2MemoryHotplugSnapshotCandidateParts {
        (
            self.bytes,
            self.device_graph,
            self.serial,
            self.entropy,
            self.balloon,
            self.topology,
            self.memory,
        )
    }
}

impl fmt::Debug for MaterializedNativeV2MemoryHotplugSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaterializedNativeV2MemoryHotplugSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("has_balloon", &self.balloon.is_some())
            .field("state", &REDACTED)
            .field("memory_topology", &REDACTED)
            .field("memory", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

/// Failure while preparing one exact-2.10 artifact candidate.
pub enum NativeV2MemoryHotplugSnapshotPreparationError {
    /// The closed kind-1/kind-11 topology could not be prepared.
    Topology(SnapshotV2MemoryHotplugPreparationError),
}

impl fmt::Debug for NativeV2MemoryHotplugSnapshotPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for NativeV2MemoryHotplugSnapshotPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topology(_) => {
                formatter.write_str("native-v2 virtio-mem artifact topology is invalid")
            }
        }
    }
}

impl std::error::Error for NativeV2MemoryHotplugSnapshotPreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Topology(source) => Some(source),
        }
    }
}

impl NativeV2MemoryHotplugSnapshotCandidateState {
    /// Validates and retains one exact native-v2 2.10 state.
    pub fn from_memory_hotplug_state_v2_10(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph, serial, entropy, balloon, memory_hotplug) =
            decode_memory_hotplug_state_v2_10(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
            serial,
            entropy,
            balloon,
            memory_hotplug,
        })
    }

    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the encoded state.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the optional unchanged profile-3 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional unchanged exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Returns the optional unchanged exact-2.9 balloon state.
    pub const fn balloon(&self) -> Option<&SnapshotV2BalloonState> {
        self.balloon.as_ref()
    }

    /// Returns the optional exact-2.10 virtio-mem state.
    pub const fn memory_hotplug(&self) -> Option<&SnapshotV2MemoryHotplugState> {
        self.memory_hotplug.as_ref()
    }

    /// Prepares kind-11 topology or preserves this candidate unchanged when
    /// kind 11 is absent.
    pub fn prepare(
        self,
    ) -> Result<
        NativeV2MemoryHotplugSnapshotPreparation,
        NativeV2MemoryHotplugSnapshotPreparationError,
    > {
        if self.memory_hotplug.is_none() {
            return Ok(NativeV2MemoryHotplugSnapshotPreparation::Compatible(self));
        }

        let Self {
            bytes,
            binding,
            device_graph,
            serial,
            entropy,
            balloon,
            memory_hotplug,
        } = self;
        let state =
            memory_hotplug.ok_or(NativeV2MemoryHotplugSnapshotPreparationError::Topology(
                SnapshotV2MemoryHotplugPreparationError::InvalidState,
            ))?;
        let topology = PreparedSnapshotV2MemoryHotplugTopology::prepare(state, binding)
            .map_err(NativeV2MemoryHotplugSnapshotPreparationError::Topology)?;
        Ok(NativeV2MemoryHotplugSnapshotPreparation::Prepared(
            PreparedNativeV2MemoryHotplugSnapshotCandidateState {
                bytes,
                device_graph,
                serial,
                entropy,
                balloon,
                topology,
            },
        ))
    }

    /// Consumes the candidate into its exact committed components.
    pub fn into_parts(self) -> NativeV2MemoryHotplugSnapshotCandidateParts {
        (
            self.bytes,
            self.binding,
            self.device_graph,
            self.serial,
            self.entropy,
            self.balloon,
            self.memory_hotplug,
        )
    }

    /// Consumes this exact current candidate into artifact authority.
    pub fn into_current_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _, _, _, _, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2MemoryHotplugSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2MemoryHotplugSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("has_balloon", &self.balloon.is_some())
            .field("has_memory_hotplug", &self.memory_hotplug.is_some())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

/// One closed exact native-v2 2.11 network/MMDS candidate.
///
/// Required serial and independently optional unchanged storage, entropy,
/// balloon, virtio-mem, and network/MMDS state are all decoded from the same
/// immutable byte vector. This candidate is internal compatibility authority
/// only while public output remains exact 2.10.
pub struct NativeV2NetworkSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
    balloon: Option<SnapshotV2BalloonState>,
    memory_hotplug: Option<SnapshotV2MemoryHotplugState>,
    network: Option<SnapshotV2NetworkState>,
}

/// Owned exact components retained by one exact-2.11 candidate.
pub type NativeV2NetworkSnapshotCandidateParts = (
    Vec<u8>,
    SnapshotV2MemoryBinding,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    Option<SnapshotV2MemoryHotplugState>,
    Option<SnapshotV2NetworkState>,
);

/// Exact-2.11 network preparation outcome before any host operation.
pub enum NativeV2NetworkSnapshotPreparation {
    /// The original candidate has no network component and no caller
    /// overrides, so its compatible internal path remains unchanged.
    Compatible(NativeV2NetworkSnapshotCandidateState),
    /// The network-bearing candidate has a complete resource manifest and
    /// immutable owner-free destination topology.
    Prepared(PreparedNativeV2NetworkSnapshotCandidateState),
}

impl NativeV2NetworkSnapshotPreparation {
    /// Returns the unchanged network-free candidate, when applicable.
    pub const fn compatible(&self) -> Option<&NativeV2NetworkSnapshotCandidateState> {
        match self {
            Self::Compatible(candidate) => Some(candidate),
            Self::Prepared(_) => None,
        }
    }

    /// Returns the prepared network-bearing candidate, when applicable.
    pub const fn prepared(&self) -> Option<&PreparedNativeV2NetworkSnapshotCandidateState> {
        match self {
            Self::Compatible(_) => None,
            Self::Prepared(candidate) => Some(candidate),
        }
    }
}

impl fmt::Debug for NativeV2NetworkSnapshotPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = match self {
            Self::Compatible(_) => "compatible",
            Self::Prepared(_) => "prepared",
        };
        formatter
            .debug_struct("NativeV2NetworkSnapshotPreparation")
            .field("outcome", &outcome)
            .field("state", &REDACTED)
            .finish()
    }
}

/// One network-bearing exact-2.11 candidate with owner-free restore topology.
///
/// All retained components came from the same immutable encoded state. This
/// value owns no descriptor, provider, packet-I/O owner, callback, metric,
/// datastore, platform slot, or VM authority.
pub struct PreparedNativeV2NetworkSnapshotCandidateState {
    bytes: Vec<u8>,
    binding: SnapshotV2MemoryBinding,
    device_graph: Option<SnapshotV2StorageDeviceGraph>,
    serial: SnapshotV2SerialState,
    entropy: Option<SnapshotV2EntropyState>,
    balloon: Option<SnapshotV2BalloonState>,
    memory_hotplug: Option<SnapshotV2MemoryHotplugState>,
    topology: PreparedSnapshotV2NetworkRestoreTopology,
    manifest: SnapshotRestoreManifest,
}

/// Owned exact components of one prepared network-bearing 2.11 candidate.
pub type PreparedNativeV2NetworkSnapshotCandidateParts = (
    Vec<u8>,
    SnapshotV2MemoryBinding,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    Option<SnapshotV2MemoryHotplugState>,
    PreparedSnapshotV2NetworkRestoreTopology,
    SnapshotRestoreManifest,
);

impl PreparedNativeV2NetworkSnapshotCandidateState {
    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the unchanged immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the same bytes.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the optional unchanged exact-2.6 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional unchanged exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Returns the optional unchanged exact-2.9 balloon state.
    pub const fn balloon(&self) -> Option<&SnapshotV2BalloonState> {
        self.balloon.as_ref()
    }

    /// Returns the optional unchanged exact-2.10 virtio-mem state.
    pub const fn memory_hotplug(&self) -> Option<&SnapshotV2MemoryHotplugState> {
        self.memory_hotplug.as_ref()
    }

    /// Returns the immutable owner-free network/MMDS topology.
    pub const fn topology(&self) -> &PreparedSnapshotV2NetworkRestoreTopology {
        &self.topology
    }

    /// Returns the complete storage, serial, and network resource manifest.
    pub const fn manifest(&self) -> &SnapshotRestoreManifest {
        &self.manifest
    }

    /// Consumes the candidate into its exact still-detached components.
    pub fn into_parts(self) -> PreparedNativeV2NetworkSnapshotCandidateParts {
        (
            self.bytes,
            self.binding,
            self.device_graph,
            self.serial,
            self.entropy,
            self.balloon,
            self.memory_hotplug,
            self.topology,
            self.manifest,
        )
    }
}

impl fmt::Debug for PreparedNativeV2NetworkSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedNativeV2NetworkSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("has_balloon", &self.balloon.is_some())
            .field("has_memory_hotplug", &self.memory_hotplug.is_some())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("serial", &REDACTED)
            .field("network_topology", &REDACTED)
            .field("manifest", &REDACTED)
            .finish()
    }
}

/// Failure while preparing one exact-2.11 network artifact candidate.
pub enum NativeV2NetworkSnapshotPreparationError {
    /// Caller overrides were supplied without a saved network component.
    OverridesWithoutNetwork,
    /// The complete restore resource manifest could not be derived.
    Manifest(SnapshotRestoreManifestError),
    /// The owner-free destination topology could not be prepared.
    Topology(SnapshotV2NetworkRestorePreparationError),
}

impl fmt::Debug for NativeV2NetworkSnapshotPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for NativeV2NetworkSnapshotPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OverridesWithoutNetwork => {
                "native-v2 network overrides require saved network state"
            }
            Self::Manifest(_) => "native-v2 network restore manifest is invalid",
            Self::Topology(_) => "native-v2 network restore topology is invalid",
        })
    }
}

impl std::error::Error for NativeV2NetworkSnapshotPreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OverridesWithoutNetwork => None,
            Self::Manifest(source) => Some(source),
            Self::Topology(source) => Some(source),
        }
    }
}

impl NativeV2NetworkSnapshotCandidateState {
    /// Validates and retains one exact native-v2 2.11 state.
    pub fn from_network_state_v2_11(
        bytes: Vec<u8>,
    ) -> Result<Self, NativeV2SnapshotCandidateStateError> {
        let (binding, device_graph, serial, entropy, balloon, memory_hotplug, network) =
            decode_network_state_v2_11(&bytes)?;
        Ok(Self {
            bytes,
            binding,
            device_graph,
            serial,
            entropy,
            balloon,
            memory_hotplug,
            network,
        })
    }

    /// Returns the exact candidate compatibility version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the immutable encoded state bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the memory commitment derived from the same bytes.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the optional unchanged exact-2.6 storage graph.
    pub const fn device_graph(&self) -> Option<&SnapshotV2StorageDeviceGraph> {
        self.device_graph.as_ref()
    }

    /// Returns the required unchanged exact-2.7 serial state.
    pub const fn serial(&self) -> &SnapshotV2SerialState {
        &self.serial
    }

    /// Returns the optional unchanged exact-2.8 entropy state.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyState> {
        self.entropy.as_ref()
    }

    /// Returns the optional unchanged exact-2.9 balloon state.
    pub const fn balloon(&self) -> Option<&SnapshotV2BalloonState> {
        self.balloon.as_ref()
    }

    /// Returns the optional unchanged exact-2.10 virtio-mem state.
    pub const fn memory_hotplug(&self) -> Option<&SnapshotV2MemoryHotplugState> {
        self.memory_hotplug.as_ref()
    }

    /// Returns the optional exact-2.11 network/MMDS aggregate.
    pub const fn network(&self) -> Option<&SnapshotV2NetworkState> {
        self.network.as_ref()
    }

    /// Prepares a complete exact-2.11 network restore candidate.
    ///
    /// A network-free candidate is preserved unchanged only when the caller
    /// also supplies no network overrides.
    pub fn prepare(
        self,
        overrides: &[SnapshotNetworkOverride],
    ) -> Result<NativeV2NetworkSnapshotPreparation, NativeV2NetworkSnapshotPreparationError> {
        self.prepare_with_cancel(overrides, |_| false)
    }

    /// Prepares with stable owner-free topology cancellation checkpoints.
    pub fn prepare_with_cancel<C>(
        self,
        overrides: &[SnapshotNetworkOverride],
        is_cancelled: C,
    ) -> Result<NativeV2NetworkSnapshotPreparation, NativeV2NetworkSnapshotPreparationError>
    where
        C: FnMut(SnapshotV2NetworkRestorePreparationStage) -> bool,
    {
        if self.network.is_none() {
            if overrides.is_empty() {
                return Ok(NativeV2NetworkSnapshotPreparation::Compatible(self));
            }
            return Err(NativeV2NetworkSnapshotPreparationError::OverridesWithoutNetwork);
        }

        let Self {
            bytes,
            binding,
            device_graph,
            serial,
            entropy,
            balloon,
            memory_hotplug,
            network,
        } = self;
        let network =
            network.ok_or(NativeV2NetworkSnapshotPreparationError::OverridesWithoutNetwork)?;
        let manifest = SnapshotRestoreManifest::try_from_native_v2_network_state(
            device_graph.as_ref(),
            &serial,
            Some(&network),
        )
        .map_err(NativeV2NetworkSnapshotPreparationError::Manifest)?;
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare_with_cancel(
            network,
            overrides,
            is_cancelled,
        )
        .map_err(NativeV2NetworkSnapshotPreparationError::Topology)?;

        Ok(NativeV2NetworkSnapshotPreparation::Prepared(
            PreparedNativeV2NetworkSnapshotCandidateState {
                bytes,
                binding,
                device_graph,
                serial,
                entropy,
                balloon,
                memory_hotplug,
                topology,
                manifest,
            },
        ))
    }

    /// Consumes the candidate into its inseparable exact components.
    pub fn into_parts(self) -> NativeV2NetworkSnapshotCandidateParts {
        (
            self.bytes,
            self.binding,
            self.device_graph,
            self.serial,
            self.entropy,
            self.balloon,
            self.memory_hotplug,
            self.network,
        )
    }

    /// Consumes this candidate into compatible internal artifact authority.
    pub fn into_compatible_artifact_state(self) -> NativeSnapshotArtifactState {
        let (bytes, binding, _, _, _, _, _, _) = self.into_parts();
        NativeSnapshotArtifactState {
            inner: NativeSnapshotArtifactStateInner::V2 {
                bytes,
                version: NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                binding,
            },
        }
    }
}

impl fmt::Debug for NativeV2NetworkSnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2NetworkSnapshotCandidateState")
            .field("version", &self.version())
            .field("has_storage", &self.device_graph.is_some())
            .field("has_entropy", &self.entropy.is_some())
            .field("has_balloon", &self.balloon.is_some())
            .field("has_memory_hotplug", &self.memory_hotplug.is_some())
            .field("has_network", &self.network.is_some())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("serial", &REDACTED)
            .finish()
    }
}

fn decode_device_graph_v2_4(
    bytes: &[u8],
) -> Result<(SnapshotV2MemoryBinding, SnapshotV2DeviceGraph), NativeV2SnapshotCandidateStateError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }
    let graph = state
        .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
        .ok_or(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)?;
    if graph.disposition() != SnapshotV2ComponentDisposition::Semantic {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = SnapshotV2DeviceGraph::decode(version, graph.payload())
        .map_err(NativeV2SnapshotCandidateStateError::DeviceGraph)?;
    Ok((binding, device_graph))
}

fn decode_device_graph_v2_5(
    bytes: &[u8],
) -> Result<
    (SnapshotV2MemoryBinding, SnapshotV2MultiBlockDeviceGraph),
    NativeV2SnapshotCandidateStateError,
> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }
    let graph = state
        .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
        .ok_or(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)?;
    if graph.disposition() != SnapshotV2ComponentDisposition::Semantic {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = SnapshotV2MultiBlockDeviceGraph::decode(version, graph.payload())
        .map_err(NativeV2SnapshotCandidateStateError::MultiBlockDeviceGraph)?;
    Ok((binding, device_graph))
}

fn decode_storage_device_graph_v2_6(
    bytes: &[u8],
) -> Result<
    (SnapshotV2MemoryBinding, SnapshotV2StorageDeviceGraph),
    NativeV2SnapshotCandidateStateError,
> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }
    let graph = state
        .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
        .ok_or(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)?;
    if graph.disposition() != SnapshotV2ComponentDisposition::Semantic {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = SnapshotV2StorageDeviceGraph::decode(version, graph.payload())
        .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)?;
    Ok((binding, device_graph))
}

fn decode_serial_state_v2_7(
    bytes: &[u8],
) -> Result<
    (
        SnapshotV2MemoryBinding,
        Option<SnapshotV2StorageDeviceGraph>,
        SnapshotV2SerialState,
    ),
    NativeV2SnapshotCandidateStateError,
> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }

    let mut graph_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind());
    let graph = graph_components.next();
    if graph_components.next().is_some()
        || graph.is_some_and(|component| {
            component.key() != NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = graph
        .map(|component| {
            SnapshotV2StorageDeviceGraph::decode(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)
        })
        .transpose()?;

    let mut serial_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_SERIAL_COMPONENT_KEY.kind());
    let serial = serial_components
        .next()
        .ok_or(NativeV2SnapshotCandidateStateError::MissingSerialState)?;
    if serial_components.next().is_some()
        || serial.key() != NATIVE_V2_SERIAL_COMPONENT_KEY
        || serial.disposition() != SnapshotV2ComponentDisposition::Semantic
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidSerialComponent);
    }
    let serial = SnapshotV2SerialState::decode(version, serial.payload())
        .map_err(NativeV2SnapshotCandidateStateError::SerialState)?;
    Ok((binding, device_graph, serial))
}

fn decode_entropy_state_v2_8(
    bytes: &[u8],
) -> Result<
    (
        SnapshotV2MemoryBinding,
        Option<SnapshotV2StorageDeviceGraph>,
        SnapshotV2SerialState,
        Option<SnapshotV2EntropyState>,
    ),
    NativeV2SnapshotCandidateStateError,
> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }

    let mut graph_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind());
    let graph = graph_components.next();
    if graph_components.next().is_some()
        || graph.is_some_and(|component| {
            component.key() != NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = graph
        .map(|component| {
            SnapshotV2StorageDeviceGraph::decode(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)
        })
        .transpose()?;

    let mut serial_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_SERIAL_COMPONENT_KEY.kind());
    let serial = serial_components
        .next()
        .ok_or(NativeV2SnapshotCandidateStateError::MissingSerialState)?;
    if serial_components.next().is_some()
        || serial.key() != NATIVE_V2_SERIAL_COMPONENT_KEY
        || serial.disposition() != SnapshotV2ComponentDisposition::Semantic
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidSerialComponent);
    }
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        serial.payload(),
    )
    .map_err(NativeV2SnapshotCandidateStateError::SerialState)?;

    let mut entropy_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_ENTROPY_COMPONENT_KEY.kind());
    let entropy = entropy_components.next();
    if entropy_components.next().is_some()
        || entropy.is_some_and(|component| {
            component.key() != NATIVE_V2_ENTROPY_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidEntropyComponent);
    }
    let entropy = entropy
        .map(|component| {
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::EntropyState)
        })
        .transpose()?;
    Ok((binding, device_graph, serial, entropy))
}

type DecodedBalloonSnapshotV2_9 = (
    SnapshotV2MemoryBinding,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
);

fn decode_balloon_state_v2_9(
    bytes: &[u8],
) -> Result<DecodedBalloonSnapshotV2_9, NativeV2SnapshotCandidateStateError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }

    let mut graph_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind());
    let graph = graph_components.next();
    if graph_components.next().is_some()
        || graph.is_some_and(|component| {
            component.key() != NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = graph
        .map(|component| {
            SnapshotV2StorageDeviceGraph::decode(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)
        })
        .transpose()?;

    let mut serial_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_SERIAL_COMPONENT_KEY.kind());
    let serial = serial_components
        .next()
        .ok_or(NativeV2SnapshotCandidateStateError::MissingSerialState)?;
    if serial_components.next().is_some()
        || serial.key() != NATIVE_V2_SERIAL_COMPONENT_KEY
        || serial.disposition() != SnapshotV2ComponentDisposition::Semantic
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidSerialComponent);
    }
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        serial.payload(),
    )
    .map_err(NativeV2SnapshotCandidateStateError::SerialState)?;

    let mut entropy_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_ENTROPY_COMPONENT_KEY.kind());
    let entropy = entropy_components.next();
    if entropy_components.next().is_some()
        || entropy.is_some_and(|component| {
            component.key() != NATIVE_V2_ENTROPY_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidEntropyComponent);
    }
    let entropy = entropy
        .map(|component| {
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::EntropyState)
        })
        .transpose()?;

    let mut balloon_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_BALLOON_COMPONENT_KEY.kind());
    let balloon = balloon_components.next();
    if balloon_components.next().is_some()
        || balloon.is_some_and(|component| {
            component.key() != NATIVE_V2_BALLOON_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidBalloonComponent);
    }
    let balloon = balloon
        .map(|component| {
            SnapshotV2BalloonState::decode(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::BalloonState)
        })
        .transpose()?;
    Ok((binding, device_graph, serial, entropy, balloon))
}

type DecodedMemoryHotplugSnapshotV2_10 = (
    SnapshotV2MemoryBinding,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    Option<SnapshotV2MemoryHotplugState>,
);

fn decode_memory_hotplug_state_v2_10(
    bytes: &[u8],
) -> Result<DecodedMemoryHotplugSnapshotV2_10, NativeV2SnapshotCandidateStateError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }

    let mut graph_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind());
    let graph = graph_components.next();
    if graph_components.next().is_some()
        || graph.is_some_and(|component| {
            component.key() != NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = graph
        .map(|component| {
            SnapshotV2StorageDeviceGraph::decode(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)
        })
        .transpose()?;

    let mut serial_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_SERIAL_COMPONENT_KEY.kind());
    let serial = serial_components
        .next()
        .ok_or(NativeV2SnapshotCandidateStateError::MissingSerialState)?;
    if serial_components.next().is_some()
        || serial.key() != NATIVE_V2_SERIAL_COMPONENT_KEY
        || serial.disposition() != SnapshotV2ComponentDisposition::Semantic
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidSerialComponent);
    }
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        serial.payload(),
    )
    .map_err(NativeV2SnapshotCandidateStateError::SerialState)?;

    let mut entropy_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_ENTROPY_COMPONENT_KEY.kind());
    let entropy = entropy_components.next();
    if entropy_components.next().is_some()
        || entropy.is_some_and(|component| {
            component.key() != NATIVE_V2_ENTROPY_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidEntropyComponent);
    }
    let entropy = entropy
        .map(|component| {
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::EntropyState)
        })
        .transpose()?;

    let mut balloon_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_BALLOON_COMPONENT_KEY.kind());
    let balloon = balloon_components.next();
    if balloon_components.next().is_some()
        || balloon.is_some_and(|component| {
            component.key() != NATIVE_V2_BALLOON_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidBalloonComponent);
    }
    let balloon = balloon
        .map(|component| {
            SnapshotV2BalloonState::decode(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::BalloonState)
        })
        .transpose()?;

    let mut memory_hotplug_components = state.components().filter(|component| {
        component.key().kind() == NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY.kind()
    });
    let memory_hotplug = memory_hotplug_components.next();
    if memory_hotplug_components.next().is_some()
        || memory_hotplug.is_some_and(|component| {
            component.key() != NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidMemoryHotplugComponent);
    }
    let memory_hotplug = memory_hotplug
        .map(|component| {
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::MemoryHotplugState)
        })
        .transpose()?;
    if let Some(memory_hotplug) = &memory_hotplug {
        memory_hotplug
            .validate_memory_binding(&binding)
            .map_err(NativeV2SnapshotCandidateStateError::MemoryHotplugBinding)?;
    }
    Ok((
        binding,
        device_graph,
        serial,
        entropy,
        balloon,
        memory_hotplug,
    ))
}

type DecodedNetworkSnapshotV2_11 = (
    SnapshotV2MemoryBinding,
    Option<SnapshotV2StorageDeviceGraph>,
    SnapshotV2SerialState,
    Option<SnapshotV2EntropyState>,
    Option<SnapshotV2BalloonState>,
    Option<SnapshotV2MemoryHotplugState>,
    Option<SnapshotV2NetworkState>,
);

fn decode_network_state_v2_11(
    bytes: &[u8],
) -> Result<DecodedNetworkSnapshotV2_11, NativeV2SnapshotCandidateStateError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    if version != NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION {
        return Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version });
    }
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }

    let mut graph_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind());
    let graph = graph_components.next();
    if graph_components.next().is_some()
        || graph.is_some_and(|component| {
            component.key() != NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }
    let device_graph = graph
        .map(|component| {
            SnapshotV2StorageDeviceGraph::decode(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)
        })
        .transpose()?;

    let mut serial_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_SERIAL_COMPONENT_KEY.kind());
    let serial = serial_components
        .next()
        .ok_or(NativeV2SnapshotCandidateStateError::MissingSerialState)?;
    if serial_components.next().is_some()
        || serial.key() != NATIVE_V2_SERIAL_COMPONENT_KEY
        || serial.disposition() != SnapshotV2ComponentDisposition::Semantic
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidSerialComponent);
    }
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        serial.payload(),
    )
    .map_err(NativeV2SnapshotCandidateStateError::SerialState)?;

    let mut entropy_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_ENTROPY_COMPONENT_KEY.kind());
    let entropy = entropy_components.next();
    if entropy_components.next().is_some()
        || entropy.is_some_and(|component| {
            component.key() != NATIVE_V2_ENTROPY_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidEntropyComponent);
    }
    let entropy = entropy
        .map(|component| {
            SnapshotV2EntropyState::decode(
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::EntropyState)
        })
        .transpose()?;

    let mut balloon_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_BALLOON_COMPONENT_KEY.kind());
    let balloon = balloon_components.next();
    if balloon_components.next().is_some()
        || balloon.is_some_and(|component| {
            component.key() != NATIVE_V2_BALLOON_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidBalloonComponent);
    }
    let balloon = balloon
        .map(|component| {
            SnapshotV2BalloonState::decode(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::BalloonState)
        })
        .transpose()?;

    let mut memory_hotplug_components = state.components().filter(|component| {
        component.key().kind() == NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY.kind()
    });
    let memory_hotplug = memory_hotplug_components.next();
    if memory_hotplug_components.next().is_some()
        || memory_hotplug.is_some_and(|component| {
            component.key() != NATIVE_V2_MEMORY_HOTPLUG_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidMemoryHotplugComponent);
    }
    let memory_hotplug = memory_hotplug
        .map(|component| {
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::MemoryHotplugState)
        })
        .transpose()?;
    if let Some(memory_hotplug) = &memory_hotplug {
        memory_hotplug
            .validate_memory_binding_for_compatibility_version(
                &binding,
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            )
            .map_err(NativeV2SnapshotCandidateStateError::MemoryHotplugBinding)?;
    }

    let mut network_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_NETWORK_COMPONENT_KEY.kind());
    let network = network_components.next();
    if network_components.next().is_some()
        || network.is_some_and(|component| {
            component.key() != NATIVE_V2_NETWORK_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidNetworkComponent);
    }
    let network = network
        .map(|component| {
            SnapshotV2NetworkState::decode(
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                component.payload(),
            )
            .map_err(NativeV2SnapshotCandidateStateError::NetworkState)
        })
        .transpose()?;

    Ok((
        binding,
        device_graph,
        serial,
        entropy,
        balloon,
        memory_hotplug,
        network,
    ))
}

fn classify_native_v2_profile(
    bytes: &[u8],
    expected_binding: &SnapshotV2MemoryBinding,
) -> Result<NativeV2SnapshotArtifactProfile, NativeV2SnapshotCandidateStateError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
    )
    .map_err(NativeV2SnapshotCandidateStateError::Format)?;
    let version = state.metadata().version();
    let binding = decode_snapshot_v2_memory_binding(&state)
        .map_err(NativeV2SnapshotCandidateStateError::Memory)?;
    if binding.version() != version || &binding != expected_binding {
        return Err(NativeV2SnapshotCandidateStateError::VersionMismatch {
            state: version,
            memory: binding.version(),
        });
    }

    let mut graph_components = state
        .components()
        .filter(|component| component.key().kind() == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind());
    let graph = graph_components.next();
    if graph_components.next().is_some()
        || graph.is_some_and(|component| {
            component.key() != NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY
                || component.disposition() != SnapshotV2ComponentDisposition::Semantic
        })
    {
        return Err(NativeV2SnapshotCandidateStateError::InvalidDeviceGraphComponent);
    }

    match version {
        NATIVE_V2_LEGACY_PLATFORM_VERSION if graph.is_none() => {
            Ok(NativeV2SnapshotArtifactProfile::LegacyPlatformV2_3)
        }
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION => {
            let graph = graph.ok_or(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)?;
            SnapshotV2DeviceGraph::decode(version, graph.payload())
                .map_err(NativeV2SnapshotCandidateStateError::DeviceGraph)?;
            Ok(NativeV2SnapshotArtifactProfile::DeviceGraphV2_4)
        }
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION => {
            let graph = graph.ok_or(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)?;
            SnapshotV2MultiBlockDeviceGraph::decode(version, graph.payload())
                .map_err(NativeV2SnapshotCandidateStateError::MultiBlockDeviceGraph)?;
            Ok(NativeV2SnapshotArtifactProfile::MultiBlockDeviceGraphV2_5)
        }
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION => {
            let graph = graph.ok_or(NativeV2SnapshotCandidateStateError::MissingDeviceGraph)?;
            SnapshotV2StorageDeviceGraph::decode(version, graph.payload())
                .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)?;
            Ok(NativeV2SnapshotArtifactProfile::StorageDeviceGraphV2_6)
        }
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION => {
            if let Some(graph) = graph {
                SnapshotV2StorageDeviceGraph::decode(
                    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                    graph.payload(),
                )
                .map_err(NativeV2SnapshotCandidateStateError::StorageDeviceGraph)?;
            }
            let mut serial_components = state.components().filter(|component| {
                component.key().kind() == NATIVE_V2_SERIAL_COMPONENT_KEY.kind()
            });
            let serial = serial_components
                .next()
                .ok_or(NativeV2SnapshotCandidateStateError::MissingSerialState)?;
            if serial_components.next().is_some()
                || serial.key() != NATIVE_V2_SERIAL_COMPONENT_KEY
                || serial.disposition() != SnapshotV2ComponentDisposition::Semantic
            {
                return Err(NativeV2SnapshotCandidateStateError::InvalidSerialComponent);
            }
            SnapshotV2SerialState::decode(version, serial.payload())
                .map_err(NativeV2SnapshotCandidateStateError::SerialState)?;
            Ok(NativeV2SnapshotArtifactProfile::SerialStateV2_7)
        }
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION => {
            decode_entropy_state_v2_8(bytes)?;
            Ok(NativeV2SnapshotArtifactProfile::EntropyStateV2_8)
        }
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION => {
            decode_balloon_state_v2_9(bytes)?;
            Ok(NativeV2SnapshotArtifactProfile::BalloonStateV2_9)
        }
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION => {
            decode_memory_hotplug_state_v2_10(bytes)?;
            Ok(NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10)
        }
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION => {
            decode_network_state_v2_11(bytes)?;
            Ok(NativeV2SnapshotArtifactProfile::NetworkStateV2_11)
        }
        _ => Err(NativeV2SnapshotCandidateStateError::UnexpectedVersion { found: version }),
    }
}

impl fmt::Debug for NativeV2SnapshotCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeV2SnapshotCandidateState")
            .field("version", &self.version())
            .field("state", &REDACTED)
            .field("memory_binding", &REDACTED)
            .field("device_graph", &REDACTED)
            .finish()
    }
}

/// Validation failure for one exact native-v2 graph-bearing candidate.
#[derive(Debug)]
pub enum NativeV2SnapshotCandidateStateError {
    /// The bytes do not form a known compatible native-v2 state.
    Format(SnapshotV2DecodeError),
    /// The state does not use the requested exact graph compatibility version.
    UnexpectedVersion {
        /// Version encoded by the state.
        found: SnapshotFormatVersion,
    },
    /// The state does not contain one valid memory commitment.
    Memory(SnapshotV2MemoryStateError),
    /// State and memory compatibility versions disagree.
    VersionMismatch {
        /// Version encoded by the outer state.
        state: SnapshotFormatVersion,
        /// Version encoded by the memory commitment.
        memory: SnapshotFormatVersion,
    },
    /// The exact graph-bearing state omits its required device graph.
    MissingDeviceGraph,
    /// The required graph is not a semantic state component.
    InvalidDeviceGraphComponent,
    /// The required device-graph payload is invalid.
    DeviceGraph(SnapshotV2DeviceGraphDecodeError),
    /// The required multi-block device-graph payload is invalid.
    MultiBlockDeviceGraph(SnapshotV2MultiBlockDeviceGraphDecodeError),
    /// The required block-and-pmem storage-graph payload is invalid.
    StorageDeviceGraph(SnapshotV2StorageDeviceGraphDecodeError),
    /// The exact-2.7 state omits its required serial singleton.
    MissingSerialState,
    /// The required serial state is not one semantic singleton component.
    InvalidSerialComponent,
    /// The required serial-state payload is invalid.
    SerialState(SnapshotV2SerialStateDecodeError),
    /// An exact-2.8 entropy component is not one semantic singleton.
    InvalidEntropyComponent,
    /// The optional entropy-state payload is invalid.
    EntropyState(SnapshotV2EntropyStateDecodeError),
    /// An exact-2.9 balloon component is not one semantic singleton.
    InvalidBalloonComponent,
    /// The optional balloon-state payload is invalid.
    BalloonState(SnapshotV2BalloonStateDecodeError),
    /// An exact-2.10 virtio-mem component is not one semantic singleton.
    InvalidMemoryHotplugComponent,
    /// The optional virtio-mem-state payload is invalid.
    MemoryHotplugState(SnapshotV2MemoryHotplugStateDecodeError),
    /// Kind-1 memory and optional kind-11 topology do not form one closed pair.
    MemoryHotplugBinding(SnapshotV2MemoryHotplugBindingError),
    /// An exact-2.11 network component is not one semantic singleton.
    InvalidNetworkComponent,
    /// The optional network/MMDS payload is invalid.
    NetworkState(SnapshotV2NetworkStateDecodeError),
}

impl fmt::Display for NativeV2SnapshotCandidateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(source) => write!(formatter, "invalid native-v2 candidate: {source}"),
            Self::UnexpectedVersion { found } => write!(
                formatter,
                "native-v2 candidate requires exact device-graph version; found {found}"
            ),
            Self::Memory(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate memory state: {source}"
                )
            }
            Self::VersionMismatch { state, memory } => write!(
                formatter,
                "native-v2 candidate state version {state} does not match memory version {memory}"
            ),
            Self::MissingDeviceGraph => {
                formatter.write_str("native-v2 candidate device graph is missing")
            }
            Self::InvalidDeviceGraphComponent => {
                formatter.write_str("native-v2 candidate device graph component is invalid")
            }
            Self::DeviceGraph(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate device graph: {source}"
                )
            }
            Self::MultiBlockDeviceGraph(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate multi-block device graph: {source}"
                )
            }
            Self::StorageDeviceGraph(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate storage device graph: {source}"
                )
            }
            Self::MissingSerialState => {
                formatter.write_str("native-v2 candidate serial state is missing")
            }
            Self::InvalidSerialComponent => {
                formatter.write_str("native-v2 candidate serial component is invalid")
            }
            Self::SerialState(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate serial state: {source}"
                )
            }
            Self::InvalidEntropyComponent => {
                formatter.write_str("native-v2 candidate entropy component is invalid")
            }
            Self::EntropyState(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate entropy state: {source}"
                )
            }
            Self::InvalidBalloonComponent => {
                formatter.write_str("native-v2 candidate balloon component is invalid")
            }
            Self::BalloonState(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate balloon state: {source}"
                )
            }
            Self::InvalidMemoryHotplugComponent => {
                formatter.write_str("native-v2 candidate virtio-mem component is invalid")
            }
            Self::MemoryHotplugState(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate virtio-mem state: {source}"
                )
            }
            Self::MemoryHotplugBinding(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate virtio-mem binding: {source}"
                )
            }
            Self::InvalidNetworkComponent => {
                formatter.write_str("native-v2 candidate network component is invalid")
            }
            Self::NetworkState(source) => {
                write!(
                    formatter,
                    "invalid native-v2 candidate network state: {source}"
                )
            }
        }
    }
}

impl std::error::Error for NativeV2SnapshotCandidateStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::DeviceGraph(source) => Some(source),
            Self::MultiBlockDeviceGraph(source) => Some(source),
            Self::StorageDeviceGraph(source) => Some(source),
            Self::SerialState(source) => Some(source),
            Self::EntropyState(source) => Some(source),
            Self::BalloonState(source) => Some(source),
            Self::MemoryHotplugState(source) => Some(source),
            Self::MemoryHotplugBinding(source) => Some(source),
            Self::NetworkState(source) => Some(source),
            Self::UnexpectedVersion { .. }
            | Self::VersionMismatch { .. }
            | Self::MissingDeviceGraph
            | Self::InvalidDeviceGraphComponent
            | Self::MissingSerialState
            | Self::InvalidSerialComponent
            | Self::InvalidEntropyComponent
            | Self::InvalidBalloonComponent
            | Self::InvalidMemoryHotplugComponent
            | Self::InvalidNetworkComponent => None,
        }
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
    MemoryHotplugPreparation(NativeV2MemoryHotplugSnapshotPreparationError),
    MemoryHotplugMaterialization(SnapshotV2MemoryHotplugMaterializationError),
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
            Self::MemoryHotplugPreparation(source) => {
                write!(f, "invalid native-v2 virtio-mem topology: {source}")
            }
            Self::MemoryHotplugMaterialization(source) => {
                write!(f, "invalid native-v2 virtio-mem memory image: {source}")
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
            Self::MemoryHotplugPreparation(source) => Some(source),
            Self::MemoryHotplugMaterialization(source) => Some(source),
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

    /// Consumes one exact 2.4 graph-bearing pair into the root-load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_v2_4_candidate(
        self,
    ) -> Result<(NativeV2SnapshotCandidateState, GuestMemory), NativeSnapshotArtifactStateError>
    {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate = NativeV2SnapshotCandidateState::from_device_graph_v2_4(bytes)
            .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
    }

    /// Consumes one exact 2.5 pair into the multi-block load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_v2_5_candidate(
        self,
    ) -> Result<
        (NativeV2MultiBlockSnapshotCandidateState, GuestMemory),
        NativeSnapshotArtifactStateError,
    > {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate = NativeV2MultiBlockSnapshotCandidateState::from_device_graph_v2_5(bytes)
            .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
    }

    /// Consumes one exact compatible 2.6 pair into the storage load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_v2_6_candidate(
        self,
    ) -> Result<
        (NativeV2StorageSnapshotCandidateState, GuestMemory),
        NativeSnapshotArtifactStateError,
    > {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate =
            NativeV2StorageSnapshotCandidateState::from_storage_device_graph_v2_6(bytes)
                .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
    }

    /// Consumes one exact retained 2.7 pair into the serial load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_v2_7_candidate(
        self,
    ) -> Result<(NativeV2SerialSnapshotCandidateState, GuestMemory), NativeSnapshotArtifactStateError>
    {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate = NativeV2SerialSnapshotCandidateState::from_serial_state_v2_7(bytes)
            .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
    }

    /// Consumes one exact retained 2.8 pair into the entropy load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_v2_8_candidate(
        self,
    ) -> Result<
        (NativeV2EntropySnapshotCandidateState, GuestMemory),
        NativeSnapshotArtifactStateError,
    > {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate = NativeV2EntropySnapshotCandidateState::from_entropy_state_v2_8(bytes)
            .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
    }

    /// Consumes one exact retained 2.9 pair into the balloon load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_v2_9_candidate(
        self,
    ) -> Result<
        (NativeV2BalloonSnapshotCandidateState, GuestMemory),
        NativeSnapshotArtifactStateError,
    > {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate = NativeV2BalloonSnapshotCandidateState::from_balloon_state_v2_9(bytes)
            .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
    }

    /// Consumes one exact current 2.10 pair into the virtio-mem load handoff.
    ///
    /// The state bytes are neither reopened nor re-encoded, and the already
    /// loaded guest memory remains bound to the candidate derived from them.
    pub fn into_current_v2_candidate(
        self,
    ) -> Result<
        (NativeV2MemoryHotplugSnapshotCandidateState, GuestMemory),
        NativeSnapshotArtifactStateError,
    > {
        let actual = self.family();
        let (state, memory) = self.into_parts();
        let (bytes, binding) = state.into_v2_parts().map_err(|_| {
            NativeSnapshotArtifactStateError::UnexpectedFamily {
                expected: NativeSnapshotArtifactFamily::V2,
                actual,
            }
        })?;
        let candidate =
            NativeV2MemoryHotplugSnapshotCandidateState::from_memory_hotplug_state_v2_10(bytes)
                .map_err(NativeSnapshotArtifactStateError::CurrentV2Profile)?;
        debug_assert_eq!(candidate.memory_binding(), &binding);
        Ok((candidate, memory))
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

    /// Classifies the retained state into one exact native-v2 profile.
    pub fn v2_profile(
        &self,
    ) -> Result<NativeV2SnapshotArtifactProfile, NativeSnapshotArtifactStateError> {
        self.state.v2_profile()
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
