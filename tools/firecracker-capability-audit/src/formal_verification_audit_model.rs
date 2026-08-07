use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// Closed risk categories covered by the targeted proof set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormalVerificationCategory {
    CapabilityInputArithmetic,
    QueueIndexRanges,
    RateLimitAccounting,
    ArtifactRangeValidation,
    StateTransitions,
}

/// Exact pinned Kani release and compiler boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormalVerificationToolchain {
    pub version: String,
    pub release_tag: String,
    pub release_commit: String,
    pub compiler_toolchain: String,
    pub list_format_version: String,
    pub install_command: Vec<String>,
    pub setup_command: Vec<String>,
}

/// Reproducible repository and CI execution boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormalVerificationExecution {
    pub platform: String,
    pub runner: String,
    pub command: Vec<String>,
    pub packages: Vec<String>,
    pub workflow: String,
    pub timeout_minutes: u64,
    pub sequential: bool,
}

/// One manifest-owned proof harness and its bounded claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormalVerificationHarness {
    pub id: String,
    pub category: FormalVerificationCategory,
    pub package: String,
    pub source: String,
    pub harness: String,
    pub command: Vec<String>,
    pub owner: String,
    pub assumptions: Vec<String>,
    pub bounds: Vec<String>,
    pub invariant: String,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Checked evidence shared by the complete proof set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormalVerificationEvidence {
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// Behaviors intentionally outside the Kani evidence envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormalVerificationNonclaim {
    UnrestrictedOrWholeSystemCorrectness,
    FfiOrHvfBehavior,
    GuestMemoryOrDescriptorTraversal,
    WallClockOrTimerBehavior,
    ConcurrencyOrLiveness,
    FilesystemOrTransportBehavior,
    PerformanceOrResourceBounds,
}

/// Human-owned authority for targeted Bangbang Kani evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormalVerificationAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub delivery_issue: String,
    pub toolchain: FormalVerificationToolchain,
    pub execution: FormalVerificationExecution,
    pub harnesses: Vec<FormalVerificationHarness>,
    pub evidence: FormalVerificationEvidence,
    pub nonclaims: Vec<FormalVerificationNonclaim>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_open_enums() {
        let unknown = serde_json::from_str::<FormalVerificationExecution>(
            r#"{"platform":"linux","runner":"runner","command":[],"packages":[],"workflow":"ci","timeout_minutes":1,"sequential":true,"parallel":false}"#,
        )
        .expect_err("unknown execution fields must fail");
        assert!(unknown.to_string().contains("unknown field"));

        let category = serde_json::from_str::<FormalVerificationCategory>(r#""all""#)
            .expect_err("open proof categories must fail");
        assert!(category.to_string().contains("unknown variant"));

        let nonclaim = serde_json::from_str::<FormalVerificationNonclaim>(r#""everything-else""#)
            .expect_err("open proof nonclaims must fail");
        assert!(nonclaim.to_string().contains("unknown variant"));
    }
}
