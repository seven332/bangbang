use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// One pinned upstream document and its environment-conditioned claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecificationBenchmarkUpstreamSource {
    pub id: String,
    pub path: String,
    pub git_blob: String,
    pub environment: Vec<String>,
    pub claims: Vec<String>,
    pub pending: Vec<String>,
}

/// One exact threshold-free Bangbang observation series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecificationBenchmarkMeasurement {
    pub name: String,
    pub method: String,
    pub unit: String,
    pub producer: String,
    pub interpretation: String,
}

/// Closed collection, report, fixture, and completion policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecificationBenchmarkPolicy {
    pub runner: String,
    pub config_example: String,
    pub public_commands: Vec<Vec<String>>,
    pub build_command: Vec<String>,
    pub platform: String,
    pub sessions: Vec<String>,
    pub summary_fields: Vec<String>,
    pub comparison_key: String,
    pub publication: String,
    pub network_default: String,
    pub network_fixture: String,
    pub ci: String,
    pub merged_main_gate: String,
}

/// Categorized repository evidence for the complete benchmark contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecificationBenchmarkEvidence {
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// Behaviors deliberately outside the benchmark evidence envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpecificationBenchmarkNonclaim {
    FirecrackerOrAwsLinuxKvmParity,
    PortableNumericThresholdOrRegressionVerdict,
    GuestMapExcludingVmmOverhead,
    CoremarkFioPingOrIperfEquivalence,
    ControlledPageCacheOrBareMetalRatio,
    NetworkAvailabilityCredentialsRecoveryOrCleanup,
    TrackedHardwareReport,
}

/// Human-owned authority for #1798 reference interpretation and evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecificationBenchmarkAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_sources: Vec<SpecificationBenchmarkUpstreamSource>,
    pub measurements: Vec<SpecificationBenchmarkMeasurement>,
    pub policy: SpecificationBenchmarkPolicy,
    pub capability_ids: Vec<String>,
    pub evidence: SpecificationBenchmarkEvidence,
    pub nonclaims: Vec<SpecificationBenchmarkNonclaim>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_open_nonclaims() {
        let unknown = serde_json::from_str::<SpecificationBenchmarkMeasurement>(
            r#"{"name":"sample","method":"method","unit":"unit","producer":"producer","interpretation":"meaning","threshold":1}"#,
        )
        .expect_err("unknown measurement fields must fail");
        assert!(unknown.to_string().contains("unknown field"));

        let nonclaim =
            serde_json::from_str::<SpecificationBenchmarkNonclaim>(r#""everything-else""#)
                .expect_err("open benchmark nonclaims must fail");
        assert!(nonclaim.to_string().contains("unknown variant"));
    }
}
