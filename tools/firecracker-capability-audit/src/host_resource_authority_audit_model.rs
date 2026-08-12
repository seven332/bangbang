use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// One immutable upstream source used by the host-resource authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourcePinnedSource {
    pub id: String,
    pub manifest_id: String,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact inventory cardinalities around the #1916 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// How one pinned source clause maps onto the fixed macOS resource boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceClauseOutcome {
    ImplementedMacosOutcome,
    ImplementedWithTerminalLimit,
    OperatorOwnedOutcome,
    IndependentlyOwnedOutcome,
    BackendOwnedOutcome,
    ExternalEvidenceOutcome,
}

/// Closed evidence profile identities shared by all authority records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceEvidenceProfileId {
    ManifestPreflight,
    AtomicGrantTransport,
    BootAndInputAuthority,
    StorageRuntimeAuthority,
    OutputAndRateBounds,
    SocketVsockVhostAuthority,
    SnapshotAndPagerAuthority,
    NetworkPolicyBoundary,
    LimitsAndFairness,
    FailureCleanupAndConcurrency,
    TerminalAndOperatorBoundary,
}

/// One exact host-resource-relevant clause in pinned source order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceSourceClause {
    pub order: u8,
    pub id: String,
    pub source_id: String,
    pub upstream_anchor: String,
    pub outcome: HostResourceClauseOutcome,
    pub evidence_profiles: Vec<HostResourceEvidenceProfileId>,
}

/// Closed semantic resource roles accepted by the production grant protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceRole {
    StartupConfig,
    StartupMetadata,
    KernelImage,
    InitrdImage,
    DriveBacking,
    PmemBacking,
    ApiSocketDirectory,
    VsockSocketDirectory,
    LoggerSink,
    MetricsSink,
    SerialSink,
    SnapshotDescribeInput,
    SnapshotStateInput,
    SnapshotMemoryInput,
    SnapshotOutputDirectory,
    VhostUserSocketDirectory,
    SnapshotPagerStream,
}

/// Exact access authority accepted by a resource role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceAccess {
    ReadOnly,
    WriteOnly,
    ReadWrite,
    CreateChildren,
    ConnectChildren,
}

/// Kernel object kind accepted by one grant record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceObjectKind {
    RegularFile,
    Directory,
    BlockDevice,
    ConnectedUnixStream,
}

/// Authority lifetime after a complete grant batch is adopted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceLifetime {
    OneTimeClaim,
    RuntimeTransactional,
    SessionRetained,
}

/// One exact accepted resource role and its closed authority shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceRecord {
    pub order: u8,
    pub role: HostResourceRole,
    pub object_kinds: Vec<HostResourceObjectKind>,
    pub access: Vec<HostResourceAccess>,
    pub lifetime: HostResourceLifetime,
    pub consumer: String,
    pub evidence_profiles: Vec<HostResourceEvidenceProfileId>,
}

/// One already-terminal capability composed into the authority conclusion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceTerminalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
}

/// One independent nonterminal outcome that this authority must not borrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceExternalDependency {
    pub capability_id: String,
    pub disposition: Disposition,
    pub owner_issue: String,
    pub owned_outcomes: Vec<String>,
}

/// Exact current-tree evidence for one closed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceEvidenceProfile {
    pub id: HostResourceEvidenceProfileId,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Why a broad residual phrase is not a missing producer in this scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceResidualClassification {
    GenericNonrequirement,
    ImplementationSpecificNonclaim,
    TerminalPlatformLimit,
    OperatorOwnedOutcome,
    IndependentlyOwnedOutcome,
    ExternalDependency,
    BackendOwnedOutcome,
    AlreadyImplemented,
}

/// One current residual phrase with a checked classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceResidualRecord {
    pub id: String,
    pub classification: HostResourceResidualClassification,
    pub evidence_profile: HostResourceEvidenceProfileId,
}

/// Claims deliberately excluded from the terminal host-resource conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostResourceNonclaim {
    GeneralDynamicResourceBroker,
    HardRevocation,
    CrossFilesystemAtomicSocketPublication,
    GlobalCrossLauncherResourceAllocation,
    PositiveVmnetConnectivityOrCredentials,
    HostTapRoutingFirewallOrAddressManagement,
    LinuxCgroupNamespaceOrChrootParity,
    PositiveArbitraryPerInstanceUidGid,
    AutomaticRestartOrReconnect,
    VhostUserBackendRateLimiting,
    AggressiveUniversalResourceQuotas,
    DeveloperIdNotarizationOrDeployment,
}

/// Checked authority for the complete fixed host-resource result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResourceAuthorityAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_sources: Vec<HostResourcePinnedSource>,
    pub capability_id: String,
    pub previous_counts: HostResourceDispositionCounts,
    pub target_counts: HostResourceDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub source_clauses: Vec<HostResourceSourceClause>,
    pub resource_surface: Vec<HostResourceRecord>,
    pub terminal_dependencies: Vec<HostResourceTerminalDependency>,
    pub external_dependencies: Vec<HostResourceExternalDependency>,
    pub evidence_profiles: Vec<HostResourceEvidenceProfile>,
    pub residuals: Vec<HostResourceResidualRecord>,
    pub nonclaims: Vec<HostResourceNonclaim>,
}
