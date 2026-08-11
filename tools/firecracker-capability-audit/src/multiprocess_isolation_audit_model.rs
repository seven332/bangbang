use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// One immutable upstream source used by the multiprocess isolation authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessPinnedSource {
    pub id: String,
    pub manifest_id: String,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact inventory cardinalities around the #1914 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// How one pinned source clause maps onto the fixed macOS topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultiprocessClauseOutcome {
    ImplementedMacosOutcome,
    ImplementedWithTerminalLimit,
    ComposedTerminalPlatformLimit,
    OperatorOwnedRecommendation,
    IndependentlyOwnedOutcome,
}

/// Closed evidence profile identities shared by source and residual records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultiprocessEvidenceProfileId {
    ProcessPerVmBoundary,
    LifecycleIdentityAndRedaction,
    AtomicResourceAuthority,
    CrashCancellationAndRecovery,
    ReplacementSafePublication,
    ConcurrentNoninterchangeability,
    TerminalIdentityLimit,
    OperatorBoundary,
}

/// One exact multiprocess-relevant clause in pinned source order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessSourceClause {
    pub order: u8,
    pub id: String,
    pub source_id: String,
    pub upstream_anchor: String,
    pub outcome: MultiprocessClauseOutcome,
    pub evidence_profiles: Vec<MultiprocessEvidenceProfileId>,
}

/// One already-terminal capability composed into the aggregate conclusion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessTerminalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
}

/// Exact current-tree evidence for one closed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessEvidenceProfile {
    pub id: MultiprocessEvidenceProfileId,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Why a broad phrase does or does not remain product implementation scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultiprocessResidualClassification {
    GenericNonrequirement,
    ImplementationSpecificNonclaim,
    TerminalPlatformLimit,
    OperatorOwnedNonrequirement,
}

/// One current residual phrase with a checked terminal classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessResidualRecord {
    pub id: String,
    pub classification: MultiprocessResidualClassification,
    pub evidence_profile: MultiprocessEvidenceProfileId,
}

/// Claims deliberately excluded from the aggregate macOS conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultiprocessIsolationNonclaim {
    LinuxJailerMechanismParity,
    GeneralDynamicResourceBroker,
    HardRevocation,
    ImmediateZeroSnapshotCreateWindow,
    ImmediateZeroResidueAfterDualDeath,
    MaliciousSameBundleSiblingIsolation,
    PositiveUniqueUidGidPerInstance,
    AutomaticRestartOrReconnect,
    GlobalCrossLauncherPathAllocation,
    ProductionVmnetOrHostDeployment,
}

/// Checked authority for the complete observable multiprocess isolation result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiprocessIsolationAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_sources: Vec<MultiprocessPinnedSource>,
    pub capability_id: String,
    pub previous_counts: MultiprocessDispositionCounts,
    pub target_counts: MultiprocessDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub source_clauses: Vec<MultiprocessSourceClause>,
    pub terminal_dependencies: Vec<MultiprocessTerminalDependency>,
    pub evidence_profiles: Vec<MultiprocessEvidenceProfile>,
    pub residuals: Vec<MultiprocessResidualRecord>,
    pub nonclaims: Vec<MultiprocessIsolationNonclaim>,
}
