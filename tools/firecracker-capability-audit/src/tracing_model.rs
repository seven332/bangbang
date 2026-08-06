use serde::{Deserialize, Serialize};

use crate::{Baseline, Reference};

/// Human-owned terminal audit for the Firecracker-shaped tracing slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub issue: String,
    pub feature: TracingFeatureContract,
    pub limits: TracingLimits,
    pub phases: Vec<TracingPhase>,
    pub allowed_fields: Vec<TracingField>,
    pub forbidden_fields: Vec<String>,
    pub call_sites: Vec<TracingCallSite>,
    pub nonclaims: Vec<String>,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}

/// Compile-time and standalone-tool admission policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingFeatureContract {
    pub name: String,
    pub default_enabled: bool,
    pub release_default_enabled: bool,
    pub tool_runtime_filter_environment: String,
}

/// Fixed memory, record, and bounded-tool-delivery envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingLimits {
    pub max_depth: usize,
    pub max_record_bytes: usize,
    pub max_records_per_scope: usize,
    pub tool_queue_capacity: usize,
    pub tool_receipt_timeout_ms: u64,
}

/// Closed tracing phase vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TracingPhase {
    Enter,
    Exit,
}

/// Closed dynamic field vocabulary admitted to a trace record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TracingField {
    Module,
    Thread,
    Scope,
    Phase,
}

/// Semantic owner of a production trace scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TracingCallSiteCategory {
    Api,
    Vmm,
    Device,
    Tool,
}

/// Delivery policy required at a production trace scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TracingDelivery {
    BoundedHost,
    NonblockingGuest,
    BoundedTool,
}

/// One exact literal production invocation of `bangbang_trace_scope!`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingCallSite {
    pub id: String,
    pub path: String,
    pub category: TracingCallSiteCategory,
    pub module: String,
    pub scope: String,
    pub delivery: TracingDelivery,
    pub rationale: String,
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
}
