use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// Exact inventory cardinalities after the Wave 8 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8DispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// One stable domain in the final cross-capability interaction matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8Domain {
    LifecycleState,
    ApiErrors,
    Observability,
    SecurityResourceAuthority,
    Devices,
    NetworkMmds,
    SnapshotsRestore,
}

/// Execution boundary used by one exact interaction scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8ScenarioExecution {
    Portable,
    SignedDirectHvf,
    SignedProductionBundle,
}

/// Required behavior supplied by the selected leaf scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8Outcome {
    LifecycleIdempotency,
    StrictErrorsAndFailureAtomicity,
    LoggerMetricsLifecycle,
    GrantContainmentAndRedaction,
    DeviceNetworkLivePatch,
    SnapshotCaptureReady,
    SnapshotRestoreContinuation,
    SnapshotSerialization,
    CancellationWithoutArtifacts,
    ClaimFailureNonconsumption,
    TerminalCleanup,
}

/// One exact leaf scenario and its role in the interaction matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8Scenario {
    pub id: String,
    pub execution: Wave8ScenarioExecution,
    pub domains: Vec<Wave8Domain>,
    pub outcomes: Vec<Wave8Outcome>,
    pub evidence: Vec<Reference>,
}

/// One canonical unordered pair and every selected scenario covering it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8InteractionPair {
    pub left: Wave8Domain,
    pub right: Wave8Domain,
    pub scenario_ids: Vec<String>,
}

/// Public platform mechanism shared by one exact exclusion family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8PlatformMechanism {
    X86CpuidMsr,
    ArmKvmFeatureTemplate,
    LinuxHugetlbfs2m,
    LinuxRuntimeIsolation,
}

/// Current primary-source observation supporting an exclusion family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8PlatformObservation {
    Arm64SdkLacksX86CpuidMsr,
    HvfRegistersDoNotPreserveKvmIdentity,
    Arm64XnuRejectsTwoMibSuperpages,
    MacosLacksLinuxIsolationPrimitives,
}

/// Credible alternative rejected because it changes the requested contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8RejectedAlternative {
    IgnoreCpuRequests,
    CrossArchitectureRegisterTranslation,
    EmulationOrDifferentBackend,
    PrivateKvmCapabilityMapping,
    FeatureWordRegisterReinterpretation,
    DifferentCpuSourceModel,
    VirtualAlignmentOrBatching,
    HvfIpaGranule,
    PrivilegedHostOrLinuxSidecar,
    AppSandboxOrRlimits,
    NetworkExtensionOrVmnet,
    LaunchdEndpointSecurityOrSidecar,
}

/// Current mechanism-level review of an exact platform-impossible partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8PlatformReview {
    pub mechanism: Wave8PlatformMechanism,
    pub capability_ids: Vec<String>,
    pub observation: Wave8PlatformObservation,
    pub upstream_sources: Vec<Reference>,
    pub platform_sources: Vec<Reference>,
    pub rejected_alternatives: Vec<Wave8RejectedAlternative>,
    pub challenge: Reference,
}

/// Named external owner for one retained nonterminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8HandoffOwner {
    Issue1351,
    Issue1373,
    Issue1378,
}

/// Exact audit or feasible outcome deliberately retained after Wave 8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8Handoff {
    pub capability_id: String,
    pub owner: Wave8HandoffOwner,
    pub disposition: Disposition,
}

/// Required delivery-parent state policy at the live completion gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8DeliveryOutcome {
    Completed,
    RetainedExternal,
}

/// One direct #1348 delivery parent preceding Wave 8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8DeliveryParent {
    pub issue: String,
    pub outcome: Wave8DeliveryOutcome,
}

/// Declarative hierarchy policy checked live by the delivery workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8DeliveryHierarchy {
    pub preceding_parents: Vec<Wave8DeliveryParent>,
    pub retained_external_issues: Vec<String>,
    pub offline_validator_queries_github: bool,
}

/// One canonical document owner for a Wave 8 subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8DocumentOwner {
    pub subject: String,
    pub path: String,
}

/// Deliberate limits on the final platform-feasible certification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave8Nonclaim {
    ExternalEvidenceCompletion,
    LinuxKvmOrFirecrackerBinaryParity,
    ArbitraryGuestOrCrossHostPortability,
    PortablePerformanceParity,
    WholeSystemFormalCorrectness,
    AllPossibleRuntimeInterleavings,
    PrivateOrPrivilegedFallback,
    LiveGithubStateFromOfflineValidator,
}

/// Evidence for the checked authority and its validation mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8AuthorityEvidence {
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// Human-owned authority for the final platform-feasible interaction claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave8CertificationAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub capability_id: String,
    pub target_counts: Wave8DispositionCounts,
    pub domains: Vec<Wave8Domain>,
    pub scenarios: Vec<Wave8Scenario>,
    pub interactions: Vec<Wave8InteractionPair>,
    pub platform_reviews: Vec<Wave8PlatformReview>,
    pub handoffs: Vec<Wave8Handoff>,
    pub delivery_hierarchy: Wave8DeliveryHierarchy,
    pub evidence: Wave8AuthorityEvidence,
    pub document_owners: Vec<Wave8DocumentOwner>,
    pub nonclaims: Vec<Wave8Nonclaim>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_open_enums() {
        let unknown = serde_json::from_str::<Wave8DispositionCounts>(
            r#"{"implemented_and_verified":377,"audit_required":8,"missing_platform_feasible":3,"proven_platform_impossible":30,"planned":0}"#,
        )
        .expect_err("unknown Wave 8 count fields must fail");
        assert!(unknown.to_string().contains("unknown field"));

        let domain = serde_json::from_str::<Wave8Domain>(r#""everything""#)
            .expect_err("open interaction domains must fail");
        assert!(domain.to_string().contains("unknown variant"));
    }
}
