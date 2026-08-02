//! Canonical, confidentiality-aware native snapshot inspection.
//!
//! The public views in this module borrow an already decoded
//! [`HvfNativeSnapshotDocument`](super::HvfNativeSnapshotDocument). They never
//! open paths, acquire restore resources, mutate state, or publish artifacts.
//!
//! Both views use schema `bangbang.snapshot-editor.info.v1`. Object fields are
//! emitted in declaration order and retained collections preserve their checked
//! semantic order. Counts and lengths are decimal JSON integers. Bit-pattern
//! values use type-width-fixed lowercase hexadecimal strings.
//!
//! Ordinary portable semantics—including machine configuration, identification
//! and cache registers, CPU-template values, topology, time policy, queue
//! cursors, transport placement, and device configuration—remain explicit.
//! Confidential high-entropy state—including pointer-authentication keys,
//! vector/debug/SME state, opaque GIC/device buffers, memory identities, and
//! integrity values—is represented only by a domain-separated SHA-256 equality
//! fingerprint plus its byte length.
//!
//! Host authority and low-entropy host choices—including paths, boot arguments,
//! backing/backend selectors, descriptors, inode-derived identities,
//! process/session/grant state, and the FDT checksum derived from boot choices—
//! are represented only by the literal `<redacted>`; they never enter a
//! fingerprint. Internal snapshot types
//! deliberately do not implement `Serialize`, and this module never uses their
//! `Debug` output.

use std::collections::TryReserveError;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};

use bangbang_runtime::machine::MAX_SUPPORTED_VCPUS;
use bangbang_runtime::snapshot_balloon_v2_9::NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES;
use bangbang_runtime::snapshot_device_v2_6::NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS;
use bangbang_runtime::snapshot_diff_v2_13::NATIVE_V2_DIFF_MAX_EXTENTS;
use bangbang_runtime::snapshot_format::NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES;
use bangbang_runtime::snapshot_format_v2::{
    NATIVE_V2_SNAPSHOT_MAX_COMPONENTS, NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
};
use bangbang_runtime::snapshot_memory_hotplug_v2_10::NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS;
use bangbang_runtime::snapshot_memory_v2::NATIVE_V2_MEMORY_MAX_EXTENTS;
use bangbang_runtime::snapshot_network_v2_11::NATIVE_V2_NETWORK_MAX_INTERFACES;
use serde::Serialize;
use serde::ser::SerializeStruct;

use super::{HvfNativeSnapshotDocument, HvfNativeSnapshotDocumentState};

mod common;
mod devices;
mod fingerprint;

#[cfg(test)]
mod tests;

const SCHEMA: &str = "bangbang.snapshot-editor.info.v1";
const VCPU_VIEW: &str = "vcpu-states";
const VM_VIEW: &str = "vm-state";

// The input-derived term covers worst-case JSON escaping for every byte of a
// maximum native document. The remaining terms cover schema labels and the
// expansion of compact binary collections into explicit semantic objects.
const MAX_NATIVE_SNAPSHOT_FILE_BYTES: usize =
    if NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES > NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES {
        NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES
    } else {
        NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES
    };
const MAX_ESCAPED_INPUT_BYTES: usize = MAX_NATIVE_SNAPSHOT_FILE_BYTES * 6;
const MAX_SCHEMA_FIXED_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_BYTES_PER_VCPU: usize = 128 * 1024;
const MAX_JSON_BYTES_PER_COMPONENT: usize = 1024;
const MAX_JSON_BYTES_PER_MEMORY_EXTENT: usize = 256;
const MAX_JSON_BYTES_PER_STORAGE_RECORD: usize = 64 * 1024;
const MAX_JSON_BYTES_PER_NETWORK_INTERFACE: usize = 64 * 1024;
const MAX_JSON_BYTES_PER_BALLOON_RANGE: usize = 128;
const MAX_JSON_BYTES_PER_MEMORY_HOTPLUG_RANGE: usize = 128;
const MAX_JSON_BYTES_PER_DIFF_EXTENT: usize = 256;

/// Maximum pretty-JSON bytes returned by either native snapshot inspection view.
///
/// This ceiling is a conservative sum of existing native-file, vCPU,
/// component, memory-extent, storage, network, balloon, virtio-mem range, and
/// Diff bounds. The formatter measures under this limit before allocating its
/// result.
pub const HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES: usize = MAX_ESCAPED_INPUT_BYTES
    + MAX_SCHEMA_FIXED_BYTES
    + MAX_SUPPORTED_VCPUS as usize * MAX_JSON_BYTES_PER_VCPU
    + NATIVE_V2_SNAPSHOT_MAX_COMPONENTS * MAX_JSON_BYTES_PER_COMPONENT
    + NATIVE_V2_MEMORY_MAX_EXTENTS * MAX_JSON_BYTES_PER_MEMORY_EXTENT
    + NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS as usize * MAX_JSON_BYTES_PER_STORAGE_RECORD
    + NATIVE_V2_NETWORK_MAX_INTERFACES * MAX_JSON_BYTES_PER_NETWORK_INTERFACE
    + NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES * MAX_JSON_BYTES_PER_BALLOON_RANGE
    + NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS.div_ceil(2) * MAX_JSON_BYTES_PER_MEMORY_HOTPLUG_RANGE
    + NATIVE_V2_DIFF_MAX_EXTENTS * MAX_JSON_BYTES_PER_DIFF_EXTENT;

const _: () = assert!(HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES > 0);

/// Borrowed canonical `vcpu-states` inspection DTO.
pub struct HvfNativeSnapshotVcpuStatesInspection<'a> {
    document: &'a HvfNativeSnapshotDocument,
}

impl HvfNativeSnapshotVcpuStatesInspection<'_> {
    /// Serializes this view as bounded deterministic pretty JSON.
    pub fn to_pretty_json(&self) -> Result<String, HvfNativeSnapshotInspectionError> {
        format_pretty_json(
            &VcpuStatesRoot {
                document: self.document,
            },
            HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES,
        )
    }
}

impl fmt::Debug for HvfNativeSnapshotVcpuStatesInspection<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotVcpuStatesInspection")
            .field("schema", &SCHEMA)
            .field("profile", &self.document.profile())
            .field("vcpu_count", &self.document.vcpu_count())
            .finish()
    }
}

/// Borrowed canonical full `vm-state` inspection DTO.
pub struct HvfNativeSnapshotVmStateInspection<'a> {
    document: &'a HvfNativeSnapshotDocument,
}

impl HvfNativeSnapshotVmStateInspection<'_> {
    /// Serializes this view as bounded deterministic pretty JSON.
    pub fn to_pretty_json(&self) -> Result<String, HvfNativeSnapshotInspectionError> {
        format_pretty_json(
            &VmStateRoot {
                document: self.document,
            },
            HVF_NATIVE_SNAPSHOT_INSPECTION_MAX_JSON_BYTES,
        )
    }
}

impl fmt::Debug for HvfNativeSnapshotVmStateInspection<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotVmStateInspection")
            .field("schema", &SCHEMA)
            .field("profile", &self.document.profile())
            .field("vcpu_count", &self.document.vcpu_count())
            .finish()
    }
}

impl HvfNativeSnapshotDocument {
    /// Returns the canonical borrowed vCPU-state inspection DTO.
    pub const fn inspect_vcpu_states(&self) -> HvfNativeSnapshotVcpuStatesInspection<'_> {
        HvfNativeSnapshotVcpuStatesInspection { document: self }
    }

    /// Returns the canonical borrowed full-VM inspection DTO.
    pub const fn inspect_vm_state(&self) -> HvfNativeSnapshotVmStateInspection<'_> {
        HvfNativeSnapshotVmStateInspection { document: self }
    }
}

struct VcpuStatesRoot<'a> {
    document: &'a HvfNativeSnapshotDocument,
}

impl Serialize for VcpuStatesRoot<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("VcpuStatesInspection", 6)?;
        state.serialize_field("schema", SCHEMA)?;
        state.serialize_field("view", VCPU_VIEW)?;
        state.serialize_field("family", &common::Family(self.document.family()))?;
        state.serialize_field("profile", &common::Profile(self.document.profile()))?;
        state.serialize_field("version", &common::Version(self.document.version()))?;
        state.serialize_field("vcpus", &common::Vcpus(self.document))?;
        state.end()
    }
}

struct VmStateRoot<'a> {
    document: &'a HvfNativeSnapshotDocument,
}

impl Serialize for VmStateRoot<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("VmStateInspection", 13)?;
        state.serialize_field("schema", SCHEMA)?;
        state.serialize_field("view", VM_VIEW)?;
        state.serialize_field("family", &common::Family(self.document.family()))?;
        state.serialize_field("profile", &common::Profile(self.document.profile()))?;
        state.serialize_field("version", &common::Version(self.document.version()))?;
        state.serialize_field("memory", &common::Memory(self.document))?;
        state.serialize_field("machine", &common::Machine(self.document))?;
        state.serialize_field("global", &common::Global(self.document))?;
        state.serialize_field("topology", &common::Topology(self.document))?;
        state.serialize_field("time", &common::Time(self.document))?;
        state.serialize_field("vcpus", &common::Vcpus(self.document))?;
        state.serialize_field("devices", &devices::Devices(self.document))?;
        state.serialize_field("diff", &devices::Diff(self.document))?;
        state.end()
    }
}

/// Canonical native snapshot inspection failure.
pub enum HvfNativeSnapshotInspectionError {
    /// Pretty JSON exceeded the published deterministic ceiling.
    OutputTooLarge { maximum: usize },
    /// The exactly measured output allocation failed.
    Allocation { source: TryReserveError },
    /// Serde JSON rejected the explicit schema or its bounded writer.
    Serialization { source: serde_json::Error },
    /// The immutable two-pass serialization produced different byte counts.
    NonDeterministic,
    /// Serde JSON unexpectedly produced non-UTF-8 bytes.
    InvalidUtf8,
}

impl fmt::Debug for HvfNativeSnapshotInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputTooLarge { maximum } => formatter
                .debug_struct("HvfNativeSnapshotInspectionError::OutputTooLarge")
                .field("maximum", maximum)
                .finish(),
            Self::Allocation { .. } => {
                formatter.write_str("HvfNativeSnapshotInspectionError::Allocation(<redacted>)")
            }
            Self::Serialization { .. } => {
                formatter.write_str("HvfNativeSnapshotInspectionError::Serialization(<redacted>)")
            }
            Self::NonDeterministic => {
                formatter.write_str("HvfNativeSnapshotInspectionError::NonDeterministic")
            }
            Self::InvalidUtf8 => {
                formatter.write_str("HvfNativeSnapshotInspectionError::InvalidUtf8")
            }
        }
    }
}

impl fmt::Display for HvfNativeSnapshotInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputTooLarge { maximum } => {
                write!(formatter, "snapshot inspection exceeds {maximum} bytes")
            }
            Self::Allocation { .. } => {
                formatter.write_str("snapshot inspection output allocation failed")
            }
            Self::Serialization { .. } => {
                formatter.write_str("snapshot inspection JSON serialization failed")
            }
            Self::NonDeterministic => {
                formatter.write_str("snapshot inspection serialization was not deterministic")
            }
            Self::InvalidUtf8 => {
                formatter.write_str("snapshot inspection serialization was not UTF-8")
            }
        }
    }
}

impl Error for HvfNativeSnapshotInspectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation { source } => Some(source),
            Self::Serialization { source } => Some(source),
            Self::OutputTooLarge { .. } | Self::NonDeterministic | Self::InvalidUtf8 => None,
        }
    }
}

struct CountingWriter {
    count: usize,
    limit: usize,
    exceeded: bool,
}

impl CountingWriter {
    const fn new(limit: usize) -> Self {
        Self {
            count: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.count.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::other(
                "snapshot inspection output limit exceeded",
            ));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::other(
                "snapshot inspection output limit exceeded",
            ));
        }
        self.count = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BoundedVecWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedVecWriter {
    fn try_new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity)?;
        Ok(Self {
            bytes,
            limit: capacity,
            exceeded: false,
        })
    }
}

impl Write for BoundedVecWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::other(
                "snapshot inspection output limit exceeded",
            ));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::other(
                "snapshot inspection output limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn format_pretty_json<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<String, HvfNativeSnapshotInspectionError> {
    let mut counter = CountingWriter::new(limit);
    if let Err(source) = serde_json::to_writer_pretty(&mut counter, value) {
        return if counter.exceeded {
            Err(HvfNativeSnapshotInspectionError::OutputTooLarge { maximum: limit })
        } else {
            Err(HvfNativeSnapshotInspectionError::Serialization { source })
        };
    }

    let measured = counter.count;
    let mut output = BoundedVecWriter::try_new(measured)
        .map_err(|source| HvfNativeSnapshotInspectionError::Allocation { source })?;
    if let Err(source) = serde_json::to_writer_pretty(&mut output, value) {
        return if output.exceeded {
            Err(HvfNativeSnapshotInspectionError::NonDeterministic)
        } else {
            Err(HvfNativeSnapshotInspectionError::Serialization { source })
        };
    }
    if output.bytes.len() != measured {
        return Err(HvfNativeSnapshotInspectionError::NonDeterministic);
    }
    String::from_utf8(output.bytes).map_err(|_| HvfNativeSnapshotInspectionError::InvalidUtf8)
}

#[cfg(test)]
fn format_pretty_json_with_limit<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<String, HvfNativeSnapshotInspectionError> {
    format_pretty_json(value, limit)
}

fn platform_v2(
    state: &HvfNativeSnapshotDocumentState,
) -> Option<&crate::HvfSnapshotV2PlatformState> {
    match state {
        HvfNativeSnapshotDocumentState::V1(_) => None,
        HvfNativeSnapshotDocumentState::V2LegacyPlatform(state) => Some(state),
        HvfNativeSnapshotDocumentState::V2DeviceGraph(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2MultiBlock(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Storage(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Serial(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Entropy(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Balloon(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2MemoryHotplug(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Network(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Vsock(state) => Some(state.platform()),
        HvfNativeSnapshotDocumentState::V2Diff(state) => Some(state.platform()),
    }
}
