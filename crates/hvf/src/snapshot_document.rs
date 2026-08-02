//! Pure owned documents for exact Bangbang-native HVF snapshot state.

use std::collections::TryReserveError;
use std::fmt;
use std::iter::FusedIterator;
use std::slice;

use bangbang_runtime::snapshot_artifact::{
    NativeSnapshotArtifactFamily, NativeV2SnapshotArtifactProfile,
};
use bangbang_runtime::snapshot_balloon_v2_9::NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_commit::{
    SnapshotCommitError, decode_snapshot_commit_envelope, encode_snapshot_commit_envelope,
};
use bangbang_runtime::snapshot_device_v2::NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_device_v2_5::NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_device_v2_6::NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_diff_v2_13::NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_entropy_v2_8::NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_format::{
    NATIVE_V1_SNAPSHOT_VERSION, NativeSnapshotFormatError, NativeSnapshotState,
    SnapshotFormatVersion, decode_native_snapshot_state,
};
use bangbang_runtime::snapshot_format_v2::NATIVE_V2_LEGACY_PLATFORM_VERSION;
use bangbang_runtime::snapshot_memory::SnapshotMemoryBinding;
use bangbang_runtime::snapshot_memory_hotplug_v2_10::NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_network_v2_11::NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_serial_v2_7::NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION;
use bangbang_runtime::snapshot_vsock_v2_12::NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION;

use crate::snapshot_bundle::{
    HvfSnapshotV1Bundle, HvfSnapshotV1BundleError, HvfSnapshotV1State, HvfSnapshotV1VcpuState,
};
use crate::snapshot_v2::{
    HvfSnapshotV2BalloonState, HvfSnapshotV2BuildError, HvfSnapshotV2DecodeError,
    HvfSnapshotV2DiffState, HvfSnapshotV2EncodeError, HvfSnapshotV2EntropyState,
    HvfSnapshotV2MemoryHotplugState, HvfSnapshotV2MultiBlockState, HvfSnapshotV2NetworkState,
    HvfSnapshotV2PlatformState, HvfSnapshotV2SerialState, HvfSnapshotV2State,
    HvfSnapshotV2StorageState, HvfSnapshotV2VcpuState, HvfSnapshotV2VsockState,
    decode_hvf_snapshot_v2_balloon_state, decode_hvf_snapshot_v2_diff_state,
    decode_hvf_snapshot_v2_entropy_state, decode_hvf_snapshot_v2_memory_hotplug_state,
    decode_hvf_snapshot_v2_multi_block_state, decode_hvf_snapshot_v2_network_state,
    decode_hvf_snapshot_v2_platform_state, decode_hvf_snapshot_v2_serial_state,
    decode_hvf_snapshot_v2_state, decode_hvf_snapshot_v2_storage_state,
    decode_hvf_snapshot_v2_vsock_state, encode_hvf_snapshot_v2_balloon_state,
    encode_hvf_snapshot_v2_diff_state, encode_hvf_snapshot_v2_entropy_state,
    encode_hvf_snapshot_v2_memory_hotplug_state, encode_hvf_snapshot_v2_multi_block_state,
    encode_hvf_snapshot_v2_network_state, encode_hvf_snapshot_v2_platform_state,
    encode_hvf_snapshot_v2_serial_state, encode_hvf_snapshot_v2_state,
    encode_hvf_snapshot_v2_storage_state, encode_hvf_snapshot_v2_vsock_state,
};

const REDACTED: &str = "<redacted>";

mod inspection;

pub use inspection::{
    HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES, HvfNativeSnapshotInspectionError,
    HvfNativeSnapshotVcpuStatesInspection, HvfNativeSnapshotVmStateInspection,
};

/// Exact semantic profile owned by one native HVF snapshot document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfNativeSnapshotDocumentProfile {
    /// Native-v1 `1.0.0` composite commit envelope.
    V1,
    /// One exact native-v2 profile.
    V2(NativeV2SnapshotArtifactProfile),
}

impl fmt::Display for HvfNativeSnapshotDocumentProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1 => formatter.write_str("native-v1 1.0.0"),
            Self::V2(NativeV2SnapshotArtifactProfile::LegacyPlatformV2_3) => {
                formatter.write_str("native-v2 2.3.0 legacy-platform")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::DeviceGraphV2_4) => {
                formatter.write_str("native-v2 2.4.0 device-graph")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::MultiBlockDeviceGraphV2_5) => {
                formatter.write_str("native-v2 2.5.0 multi-block-device-graph")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::StorageDeviceGraphV2_6) => {
                formatter.write_str("native-v2 2.6.0 storage-device-graph")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::SerialStateV2_7) => {
                formatter.write_str("native-v2 2.7.0 serial-state")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::EntropyStateV2_8) => {
                formatter.write_str("native-v2 2.8.0 entropy-state")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::BalloonStateV2_9) => {
                formatter.write_str("native-v2 2.9.0 balloon-state")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10) => {
                formatter.write_str("native-v2 2.10.0 memory-hotplug-state")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::NetworkStateV2_11) => {
                formatter.write_str("native-v2 2.11.0 network-state")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::VsockStateV2_12) => {
                formatter.write_str("native-v2 2.12.0 vsock-state")
            }
            Self::V2(NativeV2SnapshotArtifactProfile::DiffStateV2_13) => {
                formatter.write_str("native-v2 2.13.0 diff-state")
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum HvfNativeSnapshotDocumentState {
    V1(HvfSnapshotV1Bundle),
    V2LegacyPlatform(HvfSnapshotV2PlatformState),
    V2DeviceGraph(HvfSnapshotV2State),
    V2MultiBlock(HvfSnapshotV2MultiBlockState),
    V2Storage(HvfSnapshotV2StorageState),
    V2Serial(HvfSnapshotV2SerialState),
    V2Entropy(HvfSnapshotV2EntropyState),
    V2Balloon(HvfSnapshotV2BalloonState),
    V2MemoryHotplug(HvfSnapshotV2MemoryHotplugState),
    V2Network(HvfSnapshotV2NetworkState),
    V2Vsock(HvfSnapshotV2VsockState),
    V2Diff(HvfSnapshotV2DiffState),
}

/// One complete, owned, exact-profile Bangbang-native HVF state document.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfNativeSnapshotDocument {
    state: HvfNativeSnapshotDocumentState,
}

impl HvfNativeSnapshotDocument {
    /// Decodes one bounded native state file into its exact typed profile.
    pub fn decode(bytes: &[u8]) -> Result<Self, HvfNativeSnapshotDocumentDecodeError> {
        match decode_native_snapshot_state(bytes)
            .map_err(HvfNativeSnapshotDocumentDecodeError::Format)?
        {
            NativeSnapshotState::V1(_) => {
                let record = decode_snapshot_commit_envelope(bytes)
                    .map_err(HvfNativeSnapshotDocumentDecodeError::Commit)?;
                let bundle = HvfSnapshotV1Bundle::try_from_commit_record(record)
                    .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV1)?;
                Ok(Self {
                    state: HvfNativeSnapshotDocumentState::V1(bundle),
                })
            }
            NativeSnapshotState::V2(structural) => {
                let version = structural.metadata().version();
                let state = match version {
                    NATIVE_V2_LEGACY_PLATFORM_VERSION => {
                        HvfNativeSnapshotDocumentState::V2LegacyPlatform(
                            decode_hvf_snapshot_v2_platform_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2DeviceGraph(
                            decode_hvf_snapshot_v2_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2MultiBlock(
                            decode_hvf_snapshot_v2_multi_block_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Storage(
                            decode_hvf_snapshot_v2_storage_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Serial(
                            decode_hvf_snapshot_v2_serial_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Entropy(
                            decode_hvf_snapshot_v2_entropy_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Balloon(
                            decode_hvf_snapshot_v2_balloon_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2MemoryHotplug(
                            decode_hvf_snapshot_v2_memory_hotplug_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Network(
                            decode_hvf_snapshot_v2_network_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Vsock(
                            decode_hvf_snapshot_v2_vsock_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION => {
                        HvfNativeSnapshotDocumentState::V2Diff(
                            decode_hvf_snapshot_v2_diff_state(&structural)
                                .map_err(HvfNativeSnapshotDocumentDecodeError::NativeV2)?,
                        )
                    }
                    _ => {
                        return Err(
                            HvfNativeSnapshotDocumentDecodeError::UnsupportedExactProfile(version),
                        );
                    }
                };
                Ok(Self { state })
            }
        }
    }

    /// Encodes the document through its original exact family/profile codec.
    pub fn encode(&self) -> Result<Vec<u8>, HvfNativeSnapshotDocumentEncodeError> {
        match &self.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                encode_snapshot_commit_envelope(bundle.commit_record())
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV1)
            }
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(state) => {
                encode_hvf_snapshot_v2_platform_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2DeviceGraph(state) => {
                encode_hvf_snapshot_v2_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(state) => {
                encode_hvf_snapshot_v2_multi_block_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Storage(state) => {
                encode_hvf_snapshot_v2_storage_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Serial(state) => {
                encode_hvf_snapshot_v2_serial_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Entropy(state) => {
                encode_hvf_snapshot_v2_entropy_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Balloon(state) => {
                encode_hvf_snapshot_v2_balloon_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) => {
                encode_hvf_snapshot_v2_memory_hotplug_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Network(state) => {
                encode_hvf_snapshot_v2_network_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Vsock(state) => {
                encode_hvf_snapshot_v2_vsock_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
            HvfNativeSnapshotDocumentState::V2Diff(state) => {
                encode_hvf_snapshot_v2_diff_state(state)
                    .map_err(HvfNativeSnapshotDocumentEncodeError::NativeV2)
            }
        }
    }

    /// Returns the native artifact family.
    pub const fn family(&self) -> NativeSnapshotArtifactFamily {
        match self.state {
            HvfNativeSnapshotDocumentState::V1(_) => NativeSnapshotArtifactFamily::V1,
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(_)
            | HvfNativeSnapshotDocumentState::V2DeviceGraph(_)
            | HvfNativeSnapshotDocumentState::V2MultiBlock(_)
            | HvfNativeSnapshotDocumentState::V2Storage(_)
            | HvfNativeSnapshotDocumentState::V2Serial(_)
            | HvfNativeSnapshotDocumentState::V2Entropy(_)
            | HvfNativeSnapshotDocumentState::V2Balloon(_)
            | HvfNativeSnapshotDocumentState::V2MemoryHotplug(_)
            | HvfNativeSnapshotDocumentState::V2Network(_)
            | HvfNativeSnapshotDocumentState::V2Vsock(_)
            | HvfNativeSnapshotDocumentState::V2Diff(_) => NativeSnapshotArtifactFamily::V2,
        }
    }

    /// Returns the complete exact semantic version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        match self.state {
            HvfNativeSnapshotDocumentState::V1(_) => NATIVE_V1_SNAPSHOT_VERSION,
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(_) => {
                NATIVE_V2_LEGACY_PLATFORM_VERSION
            }
            HvfNativeSnapshotDocumentState::V2DeviceGraph(_) => {
                NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(_) => {
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Storage(_) => {
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Serial(_) => {
                NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Entropy(_) => {
                NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Balloon(_) => {
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(_) => {
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Network(_) => {
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Vsock(_) => {
                NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION
            }
            HvfNativeSnapshotDocumentState::V2Diff(_) => NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        }
    }

    /// Returns the exact semantic document profile.
    pub const fn profile(&self) -> HvfNativeSnapshotDocumentProfile {
        let profile = match self.state {
            HvfNativeSnapshotDocumentState::V1(_) => {
                return HvfNativeSnapshotDocumentProfile::V1;
            }
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(_) => {
                NativeV2SnapshotArtifactProfile::LegacyPlatformV2_3
            }
            HvfNativeSnapshotDocumentState::V2DeviceGraph(_) => {
                NativeV2SnapshotArtifactProfile::DeviceGraphV2_4
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(_) => {
                NativeV2SnapshotArtifactProfile::MultiBlockDeviceGraphV2_5
            }
            HvfNativeSnapshotDocumentState::V2Storage(_) => {
                NativeV2SnapshotArtifactProfile::StorageDeviceGraphV2_6
            }
            HvfNativeSnapshotDocumentState::V2Serial(_) => {
                NativeV2SnapshotArtifactProfile::SerialStateV2_7
            }
            HvfNativeSnapshotDocumentState::V2Entropy(_) => {
                NativeV2SnapshotArtifactProfile::EntropyStateV2_8
            }
            HvfNativeSnapshotDocumentState::V2Balloon(_) => {
                NativeV2SnapshotArtifactProfile::BalloonStateV2_9
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(_) => {
                NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10
            }
            HvfNativeSnapshotDocumentState::V2Network(_) => {
                NativeV2SnapshotArtifactProfile::NetworkStateV2_11
            }
            HvfNativeSnapshotDocumentState::V2Vsock(_) => {
                NativeV2SnapshotArtifactProfile::VsockStateV2_12
            }
            HvfNativeSnapshotDocumentState::V2Diff(_) => {
                NativeV2SnapshotArtifactProfile::DiffStateV2_13
            }
        };
        HvfNativeSnapshotDocumentProfile::V2(profile)
    }

    /// Returns a borrowed checked platform view without exposing outer devices.
    pub const fn platform(&self) -> HvfNativeSnapshotPlatformRef<'_> {
        match &self.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => HvfNativeSnapshotPlatformRef::V1 {
                memory_binding: bundle.commit_record().memory_binding(),
                state: bundle.state(),
            },
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(state) => {
                HvfNativeSnapshotPlatformRef::V2(state)
            }
            HvfNativeSnapshotDocumentState::V2DeviceGraph(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Storage(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Serial(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Entropy(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Balloon(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Network(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Vsock(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
            HvfNativeSnapshotDocumentState::V2Diff(state) => {
                HvfNativeSnapshotPlatformRef::V2(state.platform())
            }
        }
    }

    /// Returns complete vCPU states in canonical instance order.
    pub fn vcpus(&self) -> HvfNativeSnapshotVcpus<'_> {
        let inner = match &self.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                HvfNativeSnapshotVcpuIterator::V1(Some(bundle.state().vcpu()))
            }
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2DeviceGraph(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Storage(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Serial(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Entropy(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Balloon(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Network(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Vsock(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
            HvfNativeSnapshotDocumentState::V2Diff(state) => {
                HvfNativeSnapshotVcpuIterator::V2(state.platform().vcpus().iter())
            }
        };
        HvfNativeSnapshotVcpus { inner }
    }

    /// Returns the exact number of complete vCPU states.
    pub fn vcpu_count(&self) -> usize {
        match self.platform() {
            HvfNativeSnapshotPlatformRef::V1 { .. } => 1,
            HvfNativeSnapshotPlatformRef::V2(platform) => platform.vcpus().len(),
        }
    }

    /// Replaces the complete ordered vCPU set and rebuilds the same profile.
    pub fn try_replace_vcpus(
        self,
        replacements: Vec<HvfNativeSnapshotVcpuState>,
    ) -> Result<Self, HvfNativeSnapshotDocumentReplaceError> {
        let expected = self.vcpu_count();
        let actual = replacements.len();
        if actual != expected {
            return Err(HvfNativeSnapshotDocumentReplaceError::VcpuCount { expected, actual });
        }

        let state = match self.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                let replacement = replacements
                    .into_iter()
                    .next()
                    .ok_or(HvfNativeSnapshotDocumentReplaceError::VcpuCount { expected, actual })?;
                let HvfNativeSnapshotVcpuState::V1(vcpu) = replacement else {
                    return Err(HvfNativeSnapshotDocumentReplaceError::VcpuFamily);
                };
                let memory_binding = bundle.commit_record().memory_binding().clone();
                let (machine, compatibility, _, interrupts, device) =
                    bundle.into_state().into_parts();
                let state =
                    HvfSnapshotV1State::new(machine, compatibility, *vcpu, interrupts, device);
                HvfNativeSnapshotDocumentState::V1(
                    HvfSnapshotV1Bundle::try_new(memory_binding, state)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV1)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2LegacyPlatform(state) => {
                HvfNativeSnapshotDocumentState::V2LegacyPlatform(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2DeviceGraph(state) => {
                HvfNativeSnapshotDocumentState::V2DeviceGraph(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2MultiBlock(state) => {
                HvfNativeSnapshotDocumentState::V2MultiBlock(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Storage(state) => {
                HvfNativeSnapshotDocumentState::V2Storage(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Serial(state) => {
                HvfNativeSnapshotDocumentState::V2Serial(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Entropy(state) => {
                HvfNativeSnapshotDocumentState::V2Entropy(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Balloon(state) => {
                HvfNativeSnapshotDocumentState::V2Balloon(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) => {
                HvfNativeSnapshotDocumentState::V2MemoryHotplug(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Network(state) => {
                HvfNativeSnapshotDocumentState::V2Network(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Vsock(state) => {
                HvfNativeSnapshotDocumentState::V2Vsock(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
            HvfNativeSnapshotDocumentState::V2Diff(state) => {
                HvfNativeSnapshotDocumentState::V2Diff(
                    state
                        .try_replace_vcpus(into_v2_vcpus(replacements)?)
                        .map_err(HvfNativeSnapshotDocumentReplaceError::NativeV2)?,
                )
            }
        };
        Ok(Self { state })
    }
}

impl fmt::Debug for HvfNativeSnapshotDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotDocument")
            .field("profile", &self.profile())
            .field("vcpu_count", &self.vcpu_count())
            .field("state", &REDACTED)
            .finish()
    }
}

fn into_v2_vcpus(
    replacements: Vec<HvfNativeSnapshotVcpuState>,
) -> Result<Vec<HvfSnapshotV2VcpuState>, HvfNativeSnapshotDocumentReplaceError> {
    let mut vcpus = Vec::new();
    vcpus
        .try_reserve_exact(replacements.len())
        .map_err(HvfNativeSnapshotDocumentReplaceError::Allocation)?;
    for replacement in replacements {
        let HvfNativeSnapshotVcpuState::V2(vcpu) = replacement else {
            return Err(HvfNativeSnapshotDocumentReplaceError::VcpuFamily);
        };
        vcpus.push(*vcpu);
    }
    Ok(vcpus)
}

/// Borrowed checked platform semantics for one native document.
#[derive(Clone, Copy)]
pub enum HvfNativeSnapshotPlatformRef<'state> {
    /// Native-v1 typed state and its retained memory commitment.
    V1 {
        /// Exact state-to-memory commitment from the complete envelope.
        memory_binding: &'state SnapshotMemoryBinding,
        /// Complete fixed-profile HVF state.
        state: &'state HvfSnapshotV1State,
    },
    /// Common checked native-v2 platform graph.
    V2(&'state HvfSnapshotV2PlatformState),
}

impl fmt::Debug for HvfNativeSnapshotPlatformRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (family, vcpu_count) = match self {
            Self::V1 { .. } => ("native-v1", 1),
            Self::V2(platform) => ("native-v2", platform.vcpus().len()),
        };
        formatter
            .debug_struct("HvfNativeSnapshotPlatformRef")
            .field("family", &family)
            .field("vcpu_count", &vcpu_count)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Borrowed complete vCPU state from one native document.
#[derive(Clone, Copy)]
pub enum HvfNativeSnapshotVcpuRef<'state> {
    /// Native-v1's single mandatory vCPU state.
    V1(&'state HvfSnapshotV1VcpuState),
    /// One ordered native-v2 complete vCPU state.
    V2(&'state HvfSnapshotV2VcpuState),
}

impl HvfNativeSnapshotVcpuRef<'_> {
    /// Returns the canonical zero-based vCPU index.
    pub const fn index(self) -> u32 {
        match self {
            Self::V1(_) => 0,
            Self::V2(state) => state.index(),
        }
    }
}

impl fmt::Debug for HvfNativeSnapshotVcpuRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let family = match self {
            Self::V1(_) => "native-v1",
            Self::V2(_) => "native-v2",
        };
        formatter
            .debug_struct("HvfNativeSnapshotVcpuRef")
            .field("family", &family)
            .field("index", &self.index())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Owned complete vCPU replacement for one native document.
#[derive(Clone, PartialEq, Eq)]
pub enum HvfNativeSnapshotVcpuState {
    /// Native-v1's single mandatory vCPU state.
    V1(Box<HvfSnapshotV1VcpuState>),
    /// One native-v2 complete vCPU state.
    V2(Box<HvfSnapshotV2VcpuState>),
}

impl HvfNativeSnapshotVcpuState {
    /// Returns a borrowed view of this owned state.
    pub const fn as_ref(&self) -> HvfNativeSnapshotVcpuRef<'_> {
        match self {
            Self::V1(state) => HvfNativeSnapshotVcpuRef::V1(state),
            Self::V2(state) => HvfNativeSnapshotVcpuRef::V2(state),
        }
    }

    /// Returns the canonical zero-based vCPU index.
    pub const fn index(&self) -> u32 {
        self.as_ref().index()
    }
}

impl From<HvfNativeSnapshotVcpuRef<'_>> for HvfNativeSnapshotVcpuState {
    fn from(state: HvfNativeSnapshotVcpuRef<'_>) -> Self {
        match state {
            HvfNativeSnapshotVcpuRef::V1(state) => Self::V1(Box::new(state.clone())),
            HvfNativeSnapshotVcpuRef::V2(state) => Self::V2(Box::new(state.clone())),
        }
    }
}

impl fmt::Debug for HvfNativeSnapshotVcpuState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let family = match self {
            Self::V1(_) => "native-v1",
            Self::V2(_) => "native-v2",
        };
        formatter
            .debug_struct("HvfNativeSnapshotVcpuState")
            .field("family", &family)
            .field("index", &self.index())
            .field("state", &REDACTED)
            .finish()
    }
}

enum HvfNativeSnapshotVcpuIterator<'state> {
    V1(Option<&'state HvfSnapshotV1VcpuState>),
    V2(slice::Iter<'state, HvfSnapshotV2VcpuState>),
}

/// Exact-size ordered iterator over complete native vCPU state.
pub struct HvfNativeSnapshotVcpus<'state> {
    inner: HvfNativeSnapshotVcpuIterator<'state>,
}

impl<'state> Iterator for HvfNativeSnapshotVcpus<'state> {
    type Item = HvfNativeSnapshotVcpuRef<'state>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            HvfNativeSnapshotVcpuIterator::V1(state) => {
                state.take().map(HvfNativeSnapshotVcpuRef::V1)
            }
            HvfNativeSnapshotVcpuIterator::V2(states) => {
                states.next().map(HvfNativeSnapshotVcpuRef::V2)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = self.len();
        (length, Some(length))
    }
}

impl ExactSizeIterator for HvfNativeSnapshotVcpus<'_> {
    fn len(&self) -> usize {
        match &self.inner {
            HvfNativeSnapshotVcpuIterator::V1(state) => usize::from(state.is_some()),
            HvfNativeSnapshotVcpuIterator::V2(states) => states.len(),
        }
    }
}

impl FusedIterator for HvfNativeSnapshotVcpus<'_> {}

impl fmt::Debug for HvfNativeSnapshotVcpus<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotVcpus")
            .field("remaining", &self.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure to decode one exact native HVF snapshot document.
pub enum HvfNativeSnapshotDocumentDecodeError {
    /// Native-family structural classification or bounds failed.
    Format(NativeSnapshotFormatError),
    /// A native-v1 envelope did not contain a valid commit payload.
    Commit(SnapshotCommitError),
    /// A native-v1 commit did not form a complete checked HVF bundle.
    NativeV1(HvfSnapshotV1BundleError),
    /// The container version is structurally admitted but has no exact profile.
    UnsupportedExactProfile(SnapshotFormatVersion),
    /// The selected exact native-v2 typed decoder rejected the state.
    NativeV2(HvfSnapshotV2DecodeError),
}

impl fmt::Display for HvfNativeSnapshotDocumentDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(source) => write!(formatter, "invalid native snapshot state: {source}"),
            Self::Commit(source) => write!(formatter, "invalid native-v1 commit: {source}"),
            Self::NativeV1(source) => write!(formatter, "invalid native-v1 HVF state: {source}"),
            Self::UnsupportedExactProfile(version) => {
                write!(
                    formatter,
                    "native snapshot profile {version} is unsupported"
                )
            }
            Self::NativeV2(source) => {
                write!(formatter, "invalid exact native-v2 HVF state: {source}")
            }
        }
    }
}

impl fmt::Debug for HvfNativeSnapshotDocumentDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HvfNativeSnapshotDocumentDecodeError({self})")
    }
}

impl std::error::Error for HvfNativeSnapshotDocumentDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::NativeV1(source) => Some(source),
            Self::NativeV2(source) => Some(source),
            Self::UnsupportedExactProfile(_) => None,
        }
    }
}

/// Failure to encode one already-checked exact native document.
pub enum HvfNativeSnapshotDocumentEncodeError {
    /// Native-v1 commit-envelope encoding failed.
    NativeV1(SnapshotCommitError),
    /// Exact native-v2 typed encoding failed.
    NativeV2(HvfSnapshotV2EncodeError),
}

impl fmt::Display for HvfNativeSnapshotDocumentEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NativeV1(source) => {
                write!(formatter, "failed to encode native-v1 document: {source}")
            }
            Self::NativeV2(source) => {
                write!(
                    formatter,
                    "failed to encode exact native-v2 document: {source}"
                )
            }
        }
    }
}

impl fmt::Debug for HvfNativeSnapshotDocumentEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HvfNativeSnapshotDocumentEncodeError({self})")
    }
}

impl std::error::Error for HvfNativeSnapshotDocumentEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NativeV1(source) => Some(source),
            Self::NativeV2(source) => Some(source),
        }
    }
}

/// Failure to rebuild the same exact document with replacement vCPU state.
pub enum HvfNativeSnapshotDocumentReplaceError {
    /// Replacement cardinality differs from the original platform.
    VcpuCount {
        /// Original exact vCPU count.
        expected: usize,
        /// Supplied replacement count.
        actual: usize,
    },
    /// At least one replacement belongs to the wrong native family.
    VcpuFamily,
    /// Allocation for family-checked v2 replacements failed.
    Allocation(TryReserveError),
    /// Rebuilding the native-v1 bundle failed validation.
    NativeV1(HvfSnapshotV1BundleError),
    /// Rebuilding the exact native-v2 platform/profile failed validation.
    NativeV2(HvfSnapshotV2BuildError),
}

impl fmt::Display for HvfNativeSnapshotDocumentReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VcpuCount { expected, actual } => write!(
                formatter,
                "replacement vCPU count {actual} does not match document count {expected}"
            ),
            Self::VcpuFamily => {
                formatter.write_str("replacement vCPU belongs to the wrong native family")
            }
            Self::Allocation(_) => {
                formatter.write_str("failed to allocate checked replacement vCPU state")
            }
            Self::NativeV1(source) => {
                write!(
                    formatter,
                    "replacement native-v1 state is invalid: {source}"
                )
            }
            Self::NativeV2(source) => {
                write!(
                    formatter,
                    "replacement native-v2 state is invalid: {source}"
                )
            }
        }
    }
}

impl fmt::Debug for HvfNativeSnapshotDocumentReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HvfNativeSnapshotDocumentReplaceError({self})")
    }
}

impl std::error::Error for HvfNativeSnapshotDocumentReplaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::NativeV1(source) => Some(source),
            Self::NativeV2(source) => Some(source),
            Self::VcpuCount { .. } | Self::VcpuFamily => None,
        }
    }
}

#[cfg(test)]
mod tests;
