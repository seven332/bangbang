use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// Closed publication or product boundary certified by the aggregate metrics ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsLifecycleBoundary {
    InitialSession,
    PeriodicSixtySeconds,
    ExplicitFlush,
    TerminalFinalAttempt,
    Backpressure,
    PublicationTransaction,
    ConfiguredCardinality,
    SnapshotDestination,
    HotplugReuse,
    ProcessIsolation,
}

/// Delivery state of one aggregate metrics lifecycle scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsLifecycleDisposition {
    Planned,
    Implemented,
    ProductBoundary,
}

impl MetricsLifecycleDisposition {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Planned)
    }
}

/// Closed semantic claim made by one aggregate lifecycle scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsLifecycleClaim {
    SessionInitialOnce,
    PeriodicSixtySeconds,
    ExplicitFallible,
    TerminalBestEffortOnce,
    BackpressureLossReported,
    CompleteLineCommitAtomicity,
    PreviousSuccessRetry,
    ConcurrentCutOwnership,
    LostOutputAccounting,
    FinalAttemptOnce,
    ConfiguredDeviceCardinality,
    SnapshotDestinationFreshness,
    HotplugGenerationFreshness,
    ProcessIsolation,
}

/// One exact scenario in the aggregate metrics lifecycle matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsLifecycleRecord {
    pub id: String,
    pub boundary: MetricsLifecycleBoundary,
    pub disposition: MetricsLifecycleDisposition,
    pub delivery_issue: String,
    pub claims: Vec<MetricsLifecycleClaim>,
    pub rationale: String,
    #[serde(default)]
    pub implementation: Vec<Reference>,
    #[serde(default)]
    pub validation: Vec<Reference>,
}

/// Human-reviewed aggregate metrics publication and lifecycle authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsLifecycleAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub records: Vec<MetricsLifecycleRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_lifecycle_audit_fields() {
        let error = serde_json::from_str::<MetricsLifecycleAudit>(
            r#"{
                "schema_version":1,
                "baseline":{"version":"1.16.0","commit":"c","target":"t"},
                "records":[],
                "typo":true
            }"#,
        )
        .expect_err("unknown lifecycle audit fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_open_ended_lifecycle_claims() {
        let error = serde_json::from_str::<MetricsLifecycleRecord>(
            r##"{
                "id":"metrics.test",
                "boundary":"publication-transaction",
                "disposition":"implemented",
                "delivery_issue":"#1790",
                "claims":["eventually-consistent"],
                "rationale":"test"
            }"##,
        )
        .expect_err("unknown lifecycle claims must fail");
        assert!(error.to_string().contains("unknown variant"));
    }
}
