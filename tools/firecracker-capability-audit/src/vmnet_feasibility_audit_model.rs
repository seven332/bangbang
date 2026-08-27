use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// Immutable pinned Firecracker network source identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityPinnedSource {
    pub manifest_id: String,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact inventory cardinalities around the #1930 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// Reviewed public platform source and the narrow claim derived from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityPlatformSource {
    pub id: String,
    pub reference: Reference,
    pub reviewed_claim: String,
}

/// Closed authorization and topology boundary for the feasibility evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityBoundary {
    pub platform: String,
    pub preparation_identity: String,
    pub runtime_authority: String,
    pub apple_authorization: String,
    pub root_direct_topology: String,
    pub elevated_build_download_or_discovery: String,
    pub diagnostic_class: String,
}

/// Exact evidence identities executed before the inventory transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmnetFeasibilityEvidenceId {
    OrdinaryUserDenial,
    DroppedOwnerDataPlane,
    DirectGuestConnectivity,
}

/// One checked categorical feasibility gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityEvidence {
    pub id: VmnetFeasibilityEvidenceId,
    pub repetitions: u8,
    pub outcome: String,
    pub required_checks: Vec<String>,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// One exact capability disposition transition owned by #1930.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityTransition {
    pub capability_id: String,
    pub previous_disposition: Disposition,
    pub target_disposition: Disposition,
    pub delivery_issue: String,
}

/// Claims deliberately excluded from the #1930 feasibility result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmnetFeasibilityNonclaim {
    RootDirectProductionTopology,
    AppleAuthorizedVmnetPath,
    PrivilegedProviderProtocolOrBroker,
    SandboxWorkerRemoteProvider,
    ProductionServiceCrashReclamationAndConcurrency,
    CapabilityImplementationOrParentCompletion,
}

/// Checked authority for entitlement-free shared-vmnet feasibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmnetFeasibilityAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_source: VmnetFeasibilityPinnedSource,
    pub platform_sources: Vec<VmnetFeasibilityPlatformSource>,
    pub boundary: VmnetFeasibilityBoundary,
    pub previous_counts: VmnetFeasibilityDispositionCounts,
    pub target_counts: VmnetFeasibilityDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub evidence: Vec<VmnetFeasibilityEvidence>,
    pub transitions: Vec<VmnetFeasibilityTransition>,
    pub nonclaims: Vec<VmnetFeasibilityNonclaim>,
}
