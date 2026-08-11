use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// One immutable upstream source used by the aggregate jailer authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerPinnedSource {
    pub id: String,
    pub manifest_id: Option<String>,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact terminal inventory cardinalities around the #1912 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// Whether one pinned jailer argument is required for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JailerArgumentRequirement {
    Required,
    Optional,
}

/// Cardinality and value shape of one pinned jailer argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JailerArgumentCardinality {
    SingleValue,
    RepeatableValue,
    Flag,
}

/// Terminal macOS disposition of one pinned jailer argument leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JailerArgumentOutcome {
    ImplementedAndVerified,
    ProvenPlatformImpossible,
}

/// Closed evidence profile identities shared by argument and operation records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JailerEvidenceProfileId {
    GrammarAndEarlyCommands,
    ValidationAndRedaction,
    FixedCodeAndPublication,
    ClosedProcessBoundary,
    PrivateNamespaceAndCleanup,
    ResourceLimits,
    DaemonLifecycle,
    TerminalIsolationLimits,
    SignedGuestExecution,
}

/// One exact argument leaf in upstream parser order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerArgumentRecord {
    pub capability_id: String,
    pub option: String,
    pub requirement: JailerArgumentRequirement,
    pub cardinality: JailerArgumentCardinality,
    pub upstream_default: Option<String>,
    pub outcome: JailerArgumentOutcome,
    pub evidence_profile: JailerEvidenceProfileId,
}

/// How an upstream operation step maps onto the fixed macOS topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JailerOperationOutcome {
    ImplementedMacosOutcome,
    ImplementedWithTerminalLimit,
    ProvenPlatformImpossible,
    PlatformInapplicable,
}

/// One ordered step from the pinned Jailer Operation section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerOperationStep {
    pub order: u8,
    pub id: String,
    pub upstream_anchor: String,
    pub outcome: JailerOperationOutcome,
    pub evidence_profiles: Vec<JailerEvidenceProfileId>,
}

/// One complete section of the pinned jailer corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerCorpusSection {
    pub id: String,
    pub upstream_anchor: String,
    pub outcome: JailerOperationOutcome,
    pub evidence_profiles: Vec<JailerEvidenceProfileId>,
}

/// Exact current-tree evidence for one closed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerEvidenceProfile {
    pub id: JailerEvidenceProfileId,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Claims deliberately excluded from the aggregate macOS conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JailerAggregateNonclaim {
    LinuxJailerMechanismParity,
    LiteralPerRunExecutableCopy,
    NoSharedReadOnlyCodePages,
    ArbitraryTrustedPathAuthority,
    PositiveArbitraryCredentialTransition,
    PositiveConfigurableChroot,
    LinuxCgroupNamespaceOrDeviceNode,
    ExternalVmnetConnectivity,
    ProductionHostDeployment,
    DeveloperIdOrNotarization,
    AutomaticRestartOrLongLivedService,
}

/// Checked authority for the complete observable Firecracker jailer operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JailerAggregateAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_sources: Vec<JailerPinnedSource>,
    pub capability_ids: Vec<String>,
    pub previous_counts: JailerDispositionCounts,
    pub target_counts: JailerDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub arguments: Vec<JailerArgumentRecord>,
    pub operation_steps: Vec<JailerOperationStep>,
    pub corpus_sections: Vec<JailerCorpusSection>,
    pub evidence_profiles: Vec<JailerEvidenceProfile>,
    pub nonclaims: Vec<JailerAggregateNonclaim>,
}
