use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// One immutable upstream source used by the containment composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentPinnedSource {
    pub id: String,
    pub manifest_id: String,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact inventory cardinalities around the #1918 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// How one pinned source clause maps onto the fixed macOS containment result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainmentClauseOutcome {
    ImplementedMacosOutcome,
    ImplementedPortableToolOutcome,
    TerminalPlatformLimit,
    OperatorOwnedOutcome,
    IndependentlyOwnedOutcome,
    ExternalEvidenceOutcome,
}

/// Closed evidence profile identities shared by the composition records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainmentEvidenceProfileId {
    SignedCodeAndEntitlements,
    LifecycleAndPrivateNamespace,
    JailerOperationAndLimits,
    TypedResourceAuthority,
    LinuxIsolationLimits,
    PortableSeccompiler,
    FailureCleanupAndConcurrency,
    NetworkAndOperatorBoundary,
}

/// One exact containment-relevant clause in pinned source order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentSourceClause {
    pub order: u8,
    pub id: String,
    pub source_id: String,
    pub upstream_anchor: String,
    pub outcome: ContainmentClauseOutcome,
    pub evidence_profiles: Vec<ContainmentEvidenceProfileId>,
}

/// One already-terminal capability composed into the conclusion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentTerminalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
}

/// One independent nonterminal outcome that the composition must not borrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentExternalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
    pub owner_issue: String,
    pub owned_outcomes: Vec<String>,
}

/// Exact current-tree evidence for one closed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentEvidenceProfile {
    pub id: ContainmentEvidenceProfileId,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Why a broad residual phrase is not a missing producer in this scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainmentResidualClassification {
    GenericNonrequirement,
    ImplementationSpecificNonclaim,
    TerminalPlatformLimit,
    OperatorOwnedOutcome,
    IndependentlyOwnedOutcome,
    ExternalDependency,
    AlreadyImplemented,
}

/// One current residual phrase with a checked classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentResidualRecord {
    pub id: String,
    pub classification: ContainmentResidualClassification,
    pub evidence_profile: ContainmentEvidenceProfileId,
}

/// Claims deliberately excluded from the terminal composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainmentNonclaim {
    LinuxMechanismParity,
    CallerDefinedRuntimeSandboxPolicy,
    GeneralDynamicResourceBroker,
    HardRevocation,
    CrossFilesystemAtomicPublication,
    GlobalCrossLauncherAllocation,
    PositiveVmnetConnectivityOrCredentials,
    HostFirewallCapacityOrAdmissionPolicy,
    PositiveArbitraryPerInstanceUidGid,
    AutomaticRestartOrLongLivedService,
    MaliciousSameBundleSiblingIsolation,
    DeveloperIdNotarizationOrDeployment,
}

/// Checked authority for the complete fixed containment composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerSeccompContainmentAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_sources: Vec<ContainmentPinnedSource>,
    pub capability_id: String,
    pub previous_counts: ContainmentDispositionCounts,
    pub target_counts: ContainmentDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub source_clauses: Vec<ContainmentSourceClause>,
    pub terminal_dependencies: Vec<ContainmentTerminalDependency>,
    pub external_dependencies: Vec<ContainmentExternalDependency>,
    pub evidence_profiles: Vec<ContainmentEvidenceProfile>,
    pub residuals: Vec<ContainmentResidualRecord>,
    pub nonclaims: Vec<ContainmentNonclaim>,
}
