use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// Execution authority needed by one public CPU-template-helper operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperExecution {
    /// The operation inspects effective state through a separately signed HVF binary.
    SignedEffective,
    /// The operation is portable and must not construct a host or HVF provider.
    Portable,
}

/// CPU-template selection states admitted by an effective helper operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperSelection {
    OmittedDefault,
    ExplicitNone,
    PendingV1n1,
    ExplicitCustom,
}

/// Persisted document classes read or written by helper operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperArtifact {
    ConfigurationDocument,
    CpuTemplateDocument,
    CpuFingerprintDocument,
}

/// Provider boundary used by a public helper operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperProvider {
    EffectiveHvf,
    SystemHost,
}

/// Closed process result and stream behavior of helper operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperOutcome {
    SilentSuccess,
    DifferenceExitOneStderr,
    OperationalExitOneStderr,
    InvalidInvocationExitTwoStderr,
}

/// Categorized evidence owned by one operation producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuTemplateHelperOperationEvidence {
    pub pinned: Vec<Reference>,
    pub implementation: Vec<Reference>,
    pub focused_validation: Vec<Reference>,
    pub process_validation: Vec<Reference>,
    pub signed_validation: Vec<Reference>,
    pub failure_validation: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// One public operation producer and all argument identities it owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuTemplateHelperOperationRecord {
    pub capability_id: String,
    pub argument_ids: Vec<String>,
    pub execution: CpuTemplateHelperExecution,
    pub selections: Vec<CpuTemplateHelperSelection>,
    pub input_artifacts: Vec<CpuTemplateHelperArtifact>,
    pub output_artifacts: Vec<CpuTemplateHelperArtifact>,
    pub providers: Vec<CpuTemplateHelperProvider>,
    pub outcomes: Vec<CpuTemplateHelperOutcome>,
    pub evidence: CpuTemplateHelperOperationEvidence,
}

/// Exact terminal dependency dispositions consumed by the aggregate CPU rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuTemplateHelperFoundations {
    pub implemented_and_verified: Vec<String>,
    pub proven_platform_impossible: Vec<String>,
}

/// Closed aggregate scenario identities certified by the checked ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperScenario {
    InstalledCli,
    DefaultNoneEquivalence,
    CustomPrecedence,
    PendingStaticRejection,
    CanonicalTemplatePipeline,
    FingerprintChangePipeline,
    PortableProviderIndependence,
    SignedEntitlementEffectiveState,
    CollisionNonmutation,
    BoundedRedactionFailure,
    TransactionalRuntimeSelection,
    AllVcpuApplyReadbackBootPrecedence,
    NativeV1NoTemplateSnapshot,
    HeterogeneousFleetWorkflow,
}

/// One closed helper, runtime, or fleet scenario with exact evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuTemplateHelperScenarioRecord {
    pub id: CpuTemplateHelperScenario,
    pub rationale: String,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// Explicit claims intentionally excluded from aggregate CPU compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplateHelperNonclaim {
    TemplateSensibilityOrSecurity,
    DistinctHostEquivalenceOrSafety,
    X86KvmMechanismIdentity,
    HostAuthentication,
    SnapshotPortability,
    MigrationSafety,
    GlobalCrashAtomicMultiPathPublication,
}

/// Human-reviewed producer ledger for complete applicable CPU-helper behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuTemplateHelperAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub delivery_issue: String,
    pub operations: Vec<CpuTemplateHelperOperationRecord>,
    pub foundations: CpuTemplateHelperFoundations,
    pub scenarios: Vec<CpuTemplateHelperScenarioRecord>,
    pub nonclaims: Vec<CpuTemplateHelperNonclaim>,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_audit_fields() {
        let error = serde_json::from_str::<CpuTemplateHelperAudit>(
            r##"{
                "schema_version":1,
                "baseline":{"version":"1.16.0","commit":"c","target":"t"},
                "delivery_issue":"#1795",
                "operations":[],
                "foundations":{"implemented_and_verified":[],"proven_platform_impossible":[]},
                "scenarios":[],
                "nonclaims":[],
                "implementation":[],
                "validation":[],
                "typo":true
            }"##,
        )
        .expect_err("unknown CPU-helper audit fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_open_ended_scenarios_and_outcomes() {
        let scenario = serde_json::from_str::<CpuTemplateHelperScenario>(r#""future-fleet""#)
            .expect_err("unknown scenarios must fail");
        assert!(scenario.to_string().contains("unknown variant"));

        let outcome = serde_json::from_str::<CpuTemplateHelperOutcome>(r#""partial-success""#)
            .expect_err("unknown outcomes must fail");
        assert!(outcome.to_string().contains("unknown variant"));
    }
}
