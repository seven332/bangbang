//! Reviewed Firecracker KVM register removal for owned snapshot documents.

use std::collections::TryReserveError;
use std::fmt;

use crate::optional_state::{
    HvfArm64ReviewedOptionalStateBuildError, HvfArm64ReviewedOptionalStateTarget as Target,
};
use crate::snapshot_v2::{HvfSnapshotV2BuildError, HvfSnapshotV2VcpuState};

use super::{
    HvfNativeSnapshotDocument, HvfNativeSnapshotDocumentReplaceError, HvfNativeSnapshotVcpuState,
};

/// Number of exact Firecracker v1.16.0 aarch64 KVM U64 register identifiers
/// accepted by the reviewed removal operation.
pub const HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT: usize = 67;

/// Result for one requested register on one vCPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfNativeSnapshotRegisterRemovalStatus {
    /// An explicitly persisted value was reset to its destination default.
    Removed,
    /// The register was already defaulted or is not implemented by this vCPU.
    NotPresent,
}

impl fmt::Display for HvfNativeSnapshotRegisterRemovalStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Removed => formatter.write_str("removed"),
            Self::NotPresent => formatter.write_str("not present"),
        }
    }
}

/// Ordered removal results for one vCPU.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfNativeSnapshotVcpuRegisterRemovalReport {
    vcpu_index: u32,
    statuses:
        [HvfNativeSnapshotRegisterRemovalStatus; HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT],
    request_count: usize,
    removed_count: usize,
}

impl HvfNativeSnapshotVcpuRegisterRemovalReport {
    /// Return the canonical vCPU index.
    pub const fn vcpu_index(&self) -> u32 {
        self.vcpu_index
    }

    /// Return statuses in the caller's request order.
    pub fn statuses(&self) -> &[HvfNativeSnapshotRegisterRemovalStatus] {
        self.statuses.get(..self.request_count).unwrap_or_default()
    }

    /// Return the number of explicitly persisted values removed on this vCPU.
    pub const fn removed_count(&self) -> usize {
        self.removed_count
    }

    /// Return the number of requested values that were not present.
    pub const fn not_present_count(&self) -> usize {
        self.request_count - self.removed_count
    }
}

impl fmt::Debug for HvfNativeSnapshotVcpuRegisterRemovalReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotVcpuRegisterRemovalReport")
            .field("vcpu_index", &self.vcpu_index)
            .field("request_count", &self.request_count)
            .field("removed_count", &self.removed_count)
            .field("not_present_count", &self.not_present_count())
            .finish()
    }
}

/// Aggregate ordered report for one reviewed register-removal operation.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfNativeSnapshotRegisterRemovalReport {
    vcpus: Vec<HvfNativeSnapshotVcpuRegisterRemovalReport>,
    request_count: usize,
    removed_count: usize,
    not_present_count: usize,
}

impl HvfNativeSnapshotRegisterRemovalReport {
    /// Return the number of requested register identifiers.
    pub const fn request_count(&self) -> usize {
        self.request_count
    }

    /// Return ordered per-vCPU results.
    pub fn vcpus(&self) -> &[HvfNativeSnapshotVcpuRegisterRemovalReport] {
        &self.vcpus
    }

    /// Return the total number of explicitly persisted values removed.
    pub const fn removed_count(&self) -> usize {
        self.removed_count
    }

    /// Return the total number of requested values that were not present.
    pub const fn not_present_count(&self) -> usize {
        self.not_present_count
    }
}

impl fmt::Debug for HvfNativeSnapshotRegisterRemovalReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotRegisterRemovalReport")
            .field("vcpu_count", &self.vcpus.len())
            .field("request_count", &self.request_count)
            .field("removed_count", &self.removed_count)
            .field("not_present_count", &self.not_present_count)
            .finish()
    }
}

/// Successfully rebuilt document and its value-free removal report.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfNativeSnapshotRegisterRemovalOutcome {
    document: HvfNativeSnapshotDocument,
    report: HvfNativeSnapshotRegisterRemovalReport,
}

impl HvfNativeSnapshotRegisterRemovalOutcome {
    /// Return the rebuilt document.
    pub const fn document(&self) -> &HvfNativeSnapshotDocument {
        &self.document
    }

    /// Return the value-free removal report.
    pub const fn report(&self) -> &HvfNativeSnapshotRegisterRemovalReport {
        &self.report
    }

    /// Consume the outcome into its rebuilt document and report.
    pub fn into_parts(
        self,
    ) -> (
        HvfNativeSnapshotDocument,
        HvfNativeSnapshotRegisterRemovalReport,
    ) {
        (self.document, self.report)
    }
}

impl fmt::Debug for HvfNativeSnapshotRegisterRemovalOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfNativeSnapshotRegisterRemovalOutcome")
            .field("profile", &self.document.profile())
            .field("report", &self.report)
            .field("document", &"<redacted>")
            .finish()
    }
}

/// Value-free failure to remove reviewed Firecracker KVM register state.
pub enum HvfNativeSnapshotRegisterRemovalError {
    /// The request contained no register identifiers.
    EmptyRequest,
    /// One register identifier is not in the exact reviewed registry.
    UnsupportedRegister {
        /// Zero-based position in the submitted request.
        request_index: usize,
    },
    /// One reviewed register appeared more than once.
    DuplicateRegister {
        /// Position of the first occurrence.
        first_request_index: usize,
        /// Position of the duplicate occurrence.
        duplicate_request_index: usize,
    },
    /// Checked result storage could not be allocated.
    Allocation(TryReserveError),
    /// Rebuilding checked optional state failed.
    OptionalState(HvfArm64ReviewedOptionalStateBuildError),
    /// Rebuilding one native-v2 vCPU failed.
    Vcpu(HvfSnapshotV2BuildError),
    /// Rebuilding the exact outer document profile failed.
    Document(HvfNativeSnapshotDocumentReplaceError),
}

impl fmt::Display for HvfNativeSnapshotRegisterRemovalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRequest => formatter.write_str("reviewed register-removal request is empty"),
            Self::UnsupportedRegister { request_index } => write!(
                formatter,
                "register-removal request position {request_index} is not reviewed"
            ),
            Self::DuplicateRegister {
                first_request_index,
                duplicate_request_index,
            } => write!(
                formatter,
                "register-removal request position {duplicate_request_index} duplicates position {first_request_index}"
            ),
            Self::Allocation(_) => {
                formatter.write_str("failed to allocate checked register-removal results")
            }
            Self::OptionalState(source) => {
                write!(
                    formatter,
                    "failed to rebuild reviewed optional state: {source}"
                )
            }
            Self::Vcpu(source) => write!(formatter, "failed to rebuild native-v2 vCPU: {source}"),
            Self::Document(source) => {
                write!(
                    formatter,
                    "failed to rebuild native snapshot document: {source}"
                )
            }
        }
    }
}

impl fmt::Debug for HvfNativeSnapshotRegisterRemovalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HvfNativeSnapshotRegisterRemovalError({self})")
    }
}

impl std::error::Error for HvfNativeSnapshotRegisterRemovalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::OptionalState(source) => Some(source),
            Self::Vcpu(source) => Some(source),
            Self::Document(source) => Some(source),
            Self::EmptyRequest
            | Self::UnsupportedRegister { .. }
            | Self::DuplicateRegister { .. } => None,
        }
    }
}

#[derive(Clone, Copy)]
struct ReviewedKvmRegister {
    id: u64,
    target: Target,
}

// Pinned to Firecracker v1.16.0 commit d83d72b710361a10294480131377b1b00b163af8.
// These are literal KVM U64 system-register IDs by design: this registry must
// not accept neighboring encodings through a family or range decoder.
const REVIEWED_KVM_REGISTERS: [ReviewedKvmRegister;
    HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT] = [
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8004,
        target: Target::BreakpointValue(0),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_800c,
        target: Target::BreakpointValue(1),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8014,
        target: Target::BreakpointValue(2),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_801c,
        target: Target::BreakpointValue(3),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8024,
        target: Target::BreakpointValue(4),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_802c,
        target: Target::BreakpointValue(5),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8034,
        target: Target::BreakpointValue(6),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_803c,
        target: Target::BreakpointValue(7),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8044,
        target: Target::BreakpointValue(8),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_804c,
        target: Target::BreakpointValue(9),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8054,
        target: Target::BreakpointValue(10),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_805c,
        target: Target::BreakpointValue(11),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8064,
        target: Target::BreakpointValue(12),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_806c,
        target: Target::BreakpointValue(13),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8074,
        target: Target::BreakpointValue(14),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_807c,
        target: Target::BreakpointValue(15),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8005,
        target: Target::BreakpointControl(0),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_800d,
        target: Target::BreakpointControl(1),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8015,
        target: Target::BreakpointControl(2),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_801d,
        target: Target::BreakpointControl(3),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8025,
        target: Target::BreakpointControl(4),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_802d,
        target: Target::BreakpointControl(5),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8035,
        target: Target::BreakpointControl(6),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_803d,
        target: Target::BreakpointControl(7),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8045,
        target: Target::BreakpointControl(8),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_804d,
        target: Target::BreakpointControl(9),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8055,
        target: Target::BreakpointControl(10),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_805d,
        target: Target::BreakpointControl(11),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8065,
        target: Target::BreakpointControl(12),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_806d,
        target: Target::BreakpointControl(13),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8075,
        target: Target::BreakpointControl(14),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_807d,
        target: Target::BreakpointControl(15),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8006,
        target: Target::WatchpointValue(0),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_800e,
        target: Target::WatchpointValue(1),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8016,
        target: Target::WatchpointValue(2),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_801e,
        target: Target::WatchpointValue(3),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8026,
        target: Target::WatchpointValue(4),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_802e,
        target: Target::WatchpointValue(5),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8036,
        target: Target::WatchpointValue(6),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_803e,
        target: Target::WatchpointValue(7),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8046,
        target: Target::WatchpointValue(8),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_804e,
        target: Target::WatchpointValue(9),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8056,
        target: Target::WatchpointValue(10),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_805e,
        target: Target::WatchpointValue(11),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8066,
        target: Target::WatchpointValue(12),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_806e,
        target: Target::WatchpointValue(13),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8076,
        target: Target::WatchpointValue(14),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_807e,
        target: Target::WatchpointValue(15),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8007,
        target: Target::WatchpointControl(0),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_800f,
        target: Target::WatchpointControl(1),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8017,
        target: Target::WatchpointControl(2),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_801f,
        target: Target::WatchpointControl(3),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8027,
        target: Target::WatchpointControl(4),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_802f,
        target: Target::WatchpointControl(5),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8037,
        target: Target::WatchpointControl(6),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_803f,
        target: Target::WatchpointControl(7),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8047,
        target: Target::WatchpointControl(8),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_804f,
        target: Target::WatchpointControl(9),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8057,
        target: Target::WatchpointControl(10),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_805f,
        target: Target::WatchpointControl(11),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8067,
        target: Target::WatchpointControl(12),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_806f,
        target: Target::WatchpointControl(13),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_8077,
        target: Target::WatchpointControl(14),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_807f,
        target: Target::WatchpointControl(15),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_c096,
        target: Target::SmeSystemRegister(0),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_c094,
        target: Target::SmeSystemRegister(1),
    },
    ReviewedKvmRegister {
        id: 0x6030_0000_0013_de85,
        target: Target::SmeSystemRegister(2),
    },
];

#[derive(Debug)]
struct ReviewedRequest {
    targets: [Target; HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT],
    len: usize,
}

impl ReviewedRequest {
    fn try_new(ids: &[u64]) -> Result<Self, HvfNativeSnapshotRegisterRemovalError> {
        if ids.is_empty() {
            return Err(HvfNativeSnapshotRegisterRemovalError::EmptyRequest);
        }

        let mut request = Self {
            targets: [Target::BreakpointValue(0); HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT],
            len: 0,
        };
        for (request_index, id) in ids.iter().enumerate() {
            let target = REVIEWED_KVM_REGISTERS
                .iter()
                .find(|register| register.id == *id)
                .map(|register| register.target)
                .ok_or(HvfNativeSnapshotRegisterRemovalError::UnsupportedRegister {
                    request_index,
                })?;
            if let Some(first_request_index) = request
                .targets
                .get(..request.len)
                .and_then(|targets| targets.iter().position(|candidate| *candidate == target))
            {
                return Err(HvfNativeSnapshotRegisterRemovalError::DuplicateRegister {
                    first_request_index,
                    duplicate_request_index: request_index,
                });
            }
            let slot = request.targets.get_mut(request.len).ok_or(
                HvfNativeSnapshotRegisterRemovalError::UnsupportedRegister { request_index },
            )?;
            *slot = target;
            request.len += 1;
        }
        Ok(request)
    }

    fn targets(&self) -> &[Target] {
        self.targets.get(..self.len).unwrap_or_default()
    }
}

impl HvfNativeSnapshotDocument {
    /// Reset exact reviewed Firecracker aarch64 KVM U64 register values to
    /// destination defaults while preserving the document's exact profile.
    pub fn try_remove_reviewed_kvm_registers(
        self,
        register_ids: &[u64],
    ) -> Result<HvfNativeSnapshotRegisterRemovalOutcome, HvfNativeSnapshotRegisterRemovalError>
    {
        let request = ReviewedRequest::try_new(register_ids)?;
        let vcpu_count = self.vcpu_count();
        let mut replacements = Vec::new();
        replacements
            .try_reserve_exact(vcpu_count)
            .map_err(HvfNativeSnapshotRegisterRemovalError::Allocation)?;
        let mut vcpu_reports = Vec::new();
        vcpu_reports
            .try_reserve_exact(vcpu_count)
            .map_err(HvfNativeSnapshotRegisterRemovalError::Allocation)?;

        let mut removed_count = 0_usize;
        let mut not_present_count = 0_usize;
        for state in self.vcpus() {
            let state = HvfNativeSnapshotVcpuState::from(state);
            let (replacement, vcpu_report) = transform_vcpu(state, &request)?;
            removed_count += vcpu_report.removed_count();
            not_present_count += vcpu_report.not_present_count();
            replacements.push(replacement);
            vcpu_reports.push(vcpu_report);
        }
        let document = self
            .try_replace_vcpus(replacements)
            .map_err(HvfNativeSnapshotRegisterRemovalError::Document)?;
        Ok(HvfNativeSnapshotRegisterRemovalOutcome {
            document,
            report: HvfNativeSnapshotRegisterRemovalReport {
                vcpus: vcpu_reports,
                request_count: request.len,
                removed_count,
                not_present_count,
            },
        })
    }
}

fn transform_vcpu(
    state: HvfNativeSnapshotVcpuState,
    request: &ReviewedRequest,
) -> Result<
    (
        HvfNativeSnapshotVcpuState,
        HvfNativeSnapshotVcpuRegisterRemovalReport,
    ),
    HvfNativeSnapshotRegisterRemovalError,
> {
    let mut removed = [false; HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT];
    let (replacement, vcpu_index) = match state {
        HvfNativeSnapshotVcpuState::V1(state) => (HvfNativeSnapshotVcpuState::V1(state), 0),
        HvfNativeSnapshotVcpuState::V2(state) => {
            let (index, mpidr, mandatory, timer, pending_interrupts, gic_icc, reviewed_optional) =
                (*state).into_parts();
            let removed = removed.get_mut(..request.len).unwrap_or_default();
            let reviewed_optional = reviewed_optional
                .try_with_destination_defaults(request.targets(), removed)
                .map_err(HvfNativeSnapshotRegisterRemovalError::OptionalState)?;
            let state = HvfSnapshotV2VcpuState::try_new(
                index,
                mpidr,
                mandatory,
                timer,
                pending_interrupts,
                gic_icc,
                reviewed_optional,
            )
            .map_err(HvfNativeSnapshotRegisterRemovalError::Vcpu)?;
            (HvfNativeSnapshotVcpuState::V2(Box::new(state)), index)
        }
    };
    Ok((replacement, report_vcpu(vcpu_index, &removed, request.len)))
}

fn report_vcpu(
    vcpu_index: u32,
    removed: &[bool; HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT],
    request_count: usize,
) -> HvfNativeSnapshotVcpuRegisterRemovalReport {
    let mut statuses = [HvfNativeSnapshotRegisterRemovalStatus::NotPresent;
        HVF_NATIVE_SNAPSHOT_REVIEWED_KVM_REGISTER_COUNT];
    let mut removed_count = 0;
    for (status, was_removed) in statuses.iter_mut().zip(removed).take(request_count) {
        if *was_removed {
            *status = HvfNativeSnapshotRegisterRemovalStatus::Removed;
            removed_count += 1;
        }
    }
    HvfNativeSnapshotVcpuRegisterRemovalReport {
        vcpu_index,
        statuses,
        request_count,
        removed_count,
    }
}

#[cfg(test)]
mod tests;
