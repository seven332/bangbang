use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// Exact inventory cardinalities around the #1948 terminal transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetDispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// Source identity recorded by the generated schema-v2 result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetSourceIdentity {
    pub commit: String,
    pub tree: String,
}

/// Public capable-host identity recorded by the generated result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetPlatformIdentity {
    pub architecture: String,
    pub hvf: String,
    pub macos: String,
    pub sdk: String,
}

/// Fixed authority split exposed by the schema-v2 result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetAuthority {
    pub controller: String,
    pub kind: String,
    pub outer: String,
    pub owner: String,
    pub provider: String,
    pub route: String,
}

/// Fixed entitlement split exposed by the schema-v2 result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetEntitlements {
    pub outer_empty: bool,
    pub provider_empty: bool,
    pub worker_app_sandbox_hvf: bool,
    pub worker_vmnet: bool,
}

/// One ordered public certification outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetCase {
    pub name: String,
    pub outcome: String,
}

/// Unchanged canonical result emitted by the merged schema-v2 runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetCertificationResult {
    pub authority: ProductionVmnetAuthority,
    pub cases: Vec<ProductionVmnetCase>,
    pub cleanup: String,
    pub entitlements: ProductionVmnetEntitlements,
    pub platform: ProductionVmnetPlatformIdentity,
    pub schema_version: u32,
    pub source: ProductionVmnetSourceIdentity,
    pub verdict: String,
}

/// Reviewed publication and repetition facts for the generated result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetEvidence {
    pub path: String,
    pub sha256: String,
    pub repetitions: u8,
    pub byte_identical: bool,
    pub source: ProductionVmnetSourceIdentity,
    pub platform: ProductionVmnetPlatformIdentity,
    pub result_comment: Reference,
    pub challenge: Reference,
}

/// Exact mandatory result rows and tracked evidence supporting one promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetCapabilityClaim {
    pub capability_id: String,
    pub summary: String,
    pub required_cases: Vec<String>,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// One exact capability disposition transition owned by #1948.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetTransition {
    pub capability_id: String,
    pub previous_disposition: Disposition,
    pub target_disposition: Disposition,
}

/// Checked terminal no-Apple production-vmnet certification authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionVmnetCertificationAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub evidence: ProductionVmnetEvidence,
    pub previous_counts: ProductionVmnetDispositionCounts,
    pub target_counts: ProductionVmnetDispositionCounts,
    pub unrelated_inventory_sha256: String,
    pub optional_non_dependencies: Vec<String>,
    pub claims: Vec<ProductionVmnetCapabilityClaim>,
    pub transitions: Vec<ProductionVmnetTransition>,
}
