use serde::{Deserialize, Serialize};

use crate::{Baseline, Input, Reference};

/// Cardinalities recorded for the pinned arm64 metrics schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSchemaCounts {
    pub static_roots: usize,
    pub static_fields: usize,
    pub dynamic_families: usize,
    pub block_dynamic_fields: usize,
    pub net_dynamic_fields: usize,
    pub vhost_user_dynamic_fields: usize,
}

/// Architecture on which a public metrics schema identity is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsArchitecture {
    All,
    Arm64,
}

/// Schema-presence rule for a metrics identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsSchemaDisposition {
    RequiredStatic,
    ConfiguredDynamic,
}

/// Cardinality rule for a metrics root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsCardinality {
    Singleton,
    Aggregate,
    ConfiguredBlock,
    ConfiguredNetwork,
    ConfiguredVhostUserBlock,
}

/// JSON scalar type admitted by the pinned validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsJsonType {
    Number,
}

/// Collection and reset behavior supplied by the pinned Rust metric primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsValueKind {
    AttemptTimestamp,
    IncrementalInterval,
    PersistentStore,
}

/// Exact syntax anchor used to prove one source fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSourceAnchor {
    pub path: String,
    pub symbol: String,
    pub line: usize,
    pub column: usize,
    pub fingerprint: String,
}

/// One ordered, always-present arm64 root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsStaticRoot {
    pub name: String,
    pub architecture: MetricsArchitecture,
    pub schema_disposition: MetricsSchemaDisposition,
    pub cardinality: MetricsCardinality,
    pub producer_anchor: MetricsSourceAnchor,
    pub aggregation_anchor: Option<MetricsSourceAnchor>,
}

/// One scalar field definition shared by static and configured roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSourceField {
    pub ordinal: usize,
    pub id: String,
    pub path: String,
    pub json_type: MetricsJsonType,
    pub value_kind: MetricsValueKind,
    pub rust_type: String,
    pub fixture_anchor: MetricsSourceAnchor,
    pub producer_anchor: MetricsSourceAnchor,
}

/// One configured dynamic-root grammar and its exact scalar population.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsDynamicFamily {
    pub id: String,
    pub fixture_prefix: String,
    pub producer_template: String,
    pub architecture: MetricsArchitecture,
    pub schema_disposition: MetricsSchemaDisposition,
    pub cardinality: MetricsCardinality,
    pub field_ids: Vec<String>,
    pub fixture_anchor: MetricsSourceAnchor,
    pub producer_anchors: Vec<MetricsSourceAnchor>,
}

/// Closed category for a known fixture/producer discrepancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsReconciliationKind {
    DuplicateFixtureField,
    ProducerOnlyDynamicFamily,
}

/// Explicit resolution of a source fact that does not become another schema identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsReconciliation {
    pub id: String,
    pub kind: MetricsReconciliationKind,
    pub resolution: String,
    pub source_anchors: Vec<MetricsSourceAnchor>,
}

/// Machine-derived portion of the checked authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSchemaSource {
    pub inputs: Vec<Input>,
    pub counts: MetricsSchemaCounts,
    pub static_roots: Vec<MetricsStaticRoot>,
    pub fields: Vec<MetricsSourceField>,
    pub dynamic_families: Vec<MetricsDynamicFamily>,
    pub reconciliations: Vec<MetricsReconciliation>,
}

/// Source-only envelope emitted by safe regeneration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSchemaSourceCandidate {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub generator_version: u32,
    pub source: MetricsSchemaSource,
}

/// Unit attached to a resolved public scalar field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsUnit {
    Count,
    Bytes,
    Microseconds,
    MillisecondsSinceUnixEpoch,
}

/// Aggregation behavior attached to a resolved public scalar field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsAggregation {
    None,
    Minimum,
    Maximum,
    Sum,
    SumAcrossConfiguredDevices,
    ZeroInConfiguredDeviceAggregate,
}

/// Bangbang subsystem responsible for eventually producing a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsProducerOwner {
    SchemaRuntime,
    ProcessLifecycle,
    Device,
}

/// Current producer status, independent of schema presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsProducerDisposition {
    Planned,
    Implemented,
    PlatformZero,
}

/// Reusable closed metadata and rationale for a set of fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsPolicyProfile {
    pub id: String,
    pub unit: MetricsUnit,
    pub aggregation: MetricsAggregation,
    pub producer_owner: MetricsProducerOwner,
    pub producer_disposition: MetricsProducerDisposition,
    #[serde(default)]
    pub delivery_issue: Option<String>,
    pub rationale: String,
    #[serde(default)]
    pub implementation: Vec<Reference>,
    #[serde(default)]
    pub validation: Vec<Reference>,
}

/// Exact one-to-one assignment of a source field to reviewed policy metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsFieldPolicy {
    pub field_id: String,
    pub profile_id: String,
}

/// Checked field authority: generated source facts plus reviewed policy facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSchemaAuthority {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub generator_version: u32,
    pub source: MetricsSchemaSource,
    pub policy_profiles: Vec<MetricsPolicyProfile>,
    pub field_policies: Vec<MetricsFieldPolicy>,
}

impl MetricsSchemaAuthority {
    /// Project the machine-owned portion for exact sibling comparison.
    pub fn source_candidate(&self) -> MetricsSchemaSourceCandidate {
        MetricsSchemaSourceCandidate {
            schema_version: self.schema_version,
            baseline: self.baseline.clone(),
            generator_version: self.generator_version,
            source: self.source.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_authority_fields() {
        let error = serde_json::from_str::<MetricsSchemaAuthority>(
            r#"{
                "schema_version":1,
                "baseline":{"version":"1.16.0","commit":"c","target":"t"},
                "generator_version":1,
                "source":{"inputs":[],"counts":{"static_roots":0,"static_fields":0,
                    "dynamic_families":0,"block_dynamic_fields":0,"net_dynamic_fields":0,
                    "vhost_user_dynamic_fields":0},"static_roots":[],"fields":[],
                    "dynamic_families":[],"reconciliations":[]},
                "policy_profiles":[],"field_policies":[],"typo":true
            }"#,
        )
        .expect_err("unknown authority fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_open_ended_policy_values() {
        let error = serde_json::from_str::<MetricsPolicyProfile>(
            r##"{"id":"p","unit":"widgets","aggregation":"none",
                "producer_owner":"device","producer_disposition":"planned",
                "delivery_issue":"#1789","rationale":"test",
                "implementation":[],"validation":[]}"##,
        )
        .expect_err("unknown units must fail");
        assert!(error.to_string().contains("unknown variant"));
    }
}
