use serde::{Deserialize, Serialize};

/// Validation strictness for an inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditMode {
    /// Permit delivery-time audit and missing-feasible states.
    Delivery,
    /// Require every record to be terminal for parent certification.
    Final,
}

/// Immutable Firecracker baseline metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub version: String,
    pub commit: String,
    pub target: String,
}

/// One pinned upstream input used to derive the source manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub path: String,
    pub git_blob: String,
    pub extractor: String,
}

/// Cardinalities used as diagnostics in addition to exact-set validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub swagger_paths: usize,
    pub swagger_operations: usize,
    pub swagger_definitions: usize,
    pub swagger_properties: usize,
    pub firecracker_arguments: usize,
    pub non_swagger_routes: usize,
    pub public_tool_operations: usize,
    pub public_tool_arguments: usize,
    pub corpus_items: usize,
}

/// One immutable identity discovered from the pinned upstream checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceItem {
    pub id: String,
    pub kind: String,
    pub key: String,
    pub path: String,
    pub anchor: String,
    pub family: String,
}

/// Machine-owned Firecracker source manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub generator_version: u32,
    pub inputs: Vec<Input>,
    pub counts: Counts,
    pub items: Vec<SourceItem>,
}

/// One implementation or validation evidence reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Reference {
    /// A repository-relative tracked file and optional stable symbol/heading anchor.
    Local {
        path: String,
        #[serde(default)]
        anchor: Option<String>,
    },
    /// A GitHub issue, pull request, comment, or review URL.
    Github { url: String },
    /// An authoritative upstream or platform URL.
    Authoritative { url: String },
}

/// Evidence required before a platform-impossible disposition can be terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformExclusion {
    pub upstream_contract: Vec<Reference>,
    pub platform_evidence: Vec<Reference>,
    pub alternatives: Vec<String>,
    pub stable_behavior: Vec<Reference>,
    pub focused_tests: Vec<Reference>,
    pub compatibility_docs: Vec<Reference>,
    pub security_docs: Vec<Reference>,
    pub challenge: Reference,
}

/// Current compatibility disposition for one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    AuditRequired,
    MissingPlatformFeasible,
    ImplementedAndVerified,
    ProvenPlatformImpossible,
}

/// Human-owned overlay for one generated or semantic capability identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub family: String,
    pub summary: String,
    pub source_refs: Vec<String>,
    pub disposition: Disposition,
    #[serde(default)]
    pub implementation: Vec<Reference>,
    #[serde(default)]
    pub validation: Vec<Reference>,
    #[serde(default)]
    pub delivery_issue: Option<String>,
    #[serde(default)]
    pub exclusion: Option<PlatformExclusion>,
}

/// Human-owned capability overlay collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInventory {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub families: Vec<String>,
    pub capabilities: Vec<Capability>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_capability_fields() {
        let json = r#"{
            "id":"semantic.test:one",
            "family":"test",
            "summary":"test",
            "source_refs":["corpus:test"],
            "disposition":"audit-required",
            "typo":true
        }"#;
        let error = serde_json::from_str::<Capability>(json)
            .expect_err("unknown capability fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unknown_disposition() {
        let json = r#"{
            "id":"semantic.test:one",
            "family":"test",
            "summary":"test",
            "source_refs":["corpus:test"],
            "disposition":"deferred"
        }"#;
        let error =
            serde_json::from_str::<Capability>(json).expect_err("legacy disposition must fail");
        assert!(error.to_string().contains("unknown variant"));
    }
}
