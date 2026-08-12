use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// Immutable pinned production-host source identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostPinnedSource {
    pub id: String,
    pub manifest_id: String,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact inventory cardinalities around the #1920 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// How one pinned production-host clause is accounted for on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProductionHostClauseOutcome {
    ImplementedMacosOutcome,
    ImplementedWithTerminalLimit,
    TerminalPlatformOrArchitectureLimit,
    OperatorOwnedOutcome,
    ImplementationSpecificNonrequirement,
    ExternalEvidenceOutcome,
}

/// Closed evidence-profile identities for the production-host corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProductionHostEvidenceProfileId {
    ContainmentAndIdentity,
    OutputAndObservability,
    ResourceControls,
    NetworkAndOperatorBoundary,
    HostAndHardwarePolicy,
    TimerAndArchitecture,
    ExternalVmnet,
}

/// One exact normalized source clause in pinned document order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostSourceClause {
    pub order: u8,
    pub id: String,
    pub upstream_anchor: String,
    pub outcome: ProductionHostClauseOutcome,
    pub evidence_profiles: Vec<ProductionHostEvidenceProfileId>,
}

/// One already-terminal capability composed into corpus accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostTerminalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
}

/// One nonterminal result whose positive proof stays outside #1920.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostExternalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
    pub owner_issue: String,
    pub owned_outcomes: Vec<String>,
}

/// Exact current-tree evidence for one closed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostEvidenceProfile {
    pub id: ProductionHostEvidenceProfileId,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Why one broad production phrase is not an unowned product gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProductionHostResidualClassification {
    AlreadyImplemented,
    TerminalPlatformOrArchitectureLimit,
    OperatorOwnedOutcome,
    ImplementationSpecificNonrequirement,
    IndependentlyOwnedOutcome,
    ExternalDependency,
}

/// One broad residual with a checked classification and evidence profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostResidualRecord {
    pub id: String,
    pub classification: ProductionHostResidualClassification,
    pub evidence_profile: ProductionHostEvidenceProfileId,
}

/// Claims deliberately excluded from terminal corpus accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProductionHostNonclaim {
    LiteralLinuxKvmCgroupNamespaceAndModuleMechanisms,
    HostKernelGuestKernelMicrocodeAndFirmwareMaintenance,
    HostFirewallSwapCapacityAdmissionAndFleetPolicy,
    OutputRetentionMonitoringRestartAndLongLivedService,
    HardwareSideChannelAndPhysicalHostCertification,
    DeveloperIdNotarizationAndDeployment,
    PositiveVmnetConnectivityOrApprovedCredentials,
    FirecrackerSpecificSignalHandlerHazard,
}

/// Checked authority for the complete pinned production-host corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionHostAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_source: ProductionHostPinnedSource,
    pub capability_id: String,
    pub previous_counts: ProductionHostDispositionCounts,
    pub target_counts: ProductionHostDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub source_clauses: Vec<ProductionHostSourceClause>,
    pub terminal_dependencies: Vec<ProductionHostTerminalDependency>,
    pub external_dependencies: Vec<ProductionHostExternalDependency>,
    pub evidence_profiles: Vec<ProductionHostEvidenceProfile>,
    pub residuals: Vec<ProductionHostResidualRecord>,
    pub nonclaims: Vec<ProductionHostNonclaim>,
}
