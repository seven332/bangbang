use serde::{Deserialize, Serialize};

use crate::{Baseline, Disposition, Reference};

/// One pinned broad upstream source certified by the Wave 7 aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7PinnedSource {
    pub capability_id: String,
    pub path: String,
    pub anchor: String,
    pub git_blob: String,
}

/// Exact terminal inventory cardinalities after the #1799 transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7DispositionCounts {
    pub implemented_and_verified: usize,
    pub audit_required: usize,
    pub missing_platform_feasible: usize,
    pub proven_platform_impossible: usize,
}

/// Design-document section used to partition every semantic capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7DesignSection {
    ScopeAndFeatures,
    HostIntegration,
    InternalArchitecture,
    ThreatContainment,
    MachineModel,
    StorageNetworkingAndRateLimiting,
    MetadataAndSandboxing,
    MonitoringAndTooling,
}

/// Terminal result or explicit external handoff for one design semantic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7DesignOutcome {
    Implemented,
    Handoff1351,
    Handoff1378,
    HandoffWave8,
}

/// One member of the complete, ordered design-semantic partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7DesignRecord {
    pub section: Wave7DesignSection,
    pub capability_id: String,
    pub outcome: Wave7DesignOutcome,
}

/// One of the four tables in the pinned device API document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7DeviceApiSection {
    Endpoints,
    InputSchema,
    OutputSchema,
    InstanceActions,
}

/// Dimensions and required-cell count for one device API table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7DeviceApiDimension {
    pub section: Wave7DeviceApiSection,
    pub rows: usize,
    pub device_columns: Vec<String>,
    pub required_relations: usize,
}

/// Historical source spelling and its current Swagger identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7DeviceApiNormalization {
    pub source: String,
    pub current: String,
}

/// Exact generated API population covered by the device API ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7ApiPopulation {
    pub operations: usize,
    pub paths: usize,
    pub schemas: usize,
    pub properties: usize,
    pub actions_corpus: usize,
    pub implemented: usize,
    pub proven_platform_impossible: usize,
}

/// Complete compact representation of the device API tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7DeviceApiLedger {
    pub dimensions: Vec<Wave7DeviceApiDimension>,
    pub normalizations: Vec<Wave7DeviceApiNormalization>,
    /// `section|source row|device column|producer capability|result`.
    pub required_relations: Vec<String>,
    pub optional_relations: usize,
    pub api_population: Wave7ApiPopulation,
}

/// Changelog section containing one independent v1.16.0 entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7ReleaseSection {
    Added,
    Fixed,
}

/// Platform interpretation of one release entry's producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7ReleaseOutcome {
    Implemented,
    Arm64Rejected,
    ProvenPlatformImpossible,
    LinuxHostHandoff1373,
}

/// One exact, ordered v1.16.0 release entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7ReleaseEntry {
    pub id: String,
    pub section: Wave7ReleaseSection,
    pub producer_capability_id: String,
    pub outcome: Wave7ReleaseOutcome,
}

/// Closed public-tool identities in the pinned manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7Tool {
    CpuTemplateHelper,
    Firecracker,
    Jailer,
    RebaseSnap,
    Seccompiler,
    SnapshotEditor,
}

/// How Bangbang realizes an applicable pinned tool surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7ToolExecution {
    NativeSigned,
    ProductAlternative,
    PortableOffline,
    DeprecatedPortable,
}

/// Exact disposition partition for a public-tool group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7ToolCounts {
    pub total: usize,
    pub implemented: usize,
    pub proven_platform_impossible: usize,
    pub audit_handoff_1373: usize,
}

/// One complete public-tool group derived from manifest leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7ToolRecord {
    pub tool: Wave7Tool,
    pub source_prefix: String,
    pub package: String,
    pub binaries: Vec<String>,
    pub execution: Wave7ToolExecution,
    pub counts: Wave7ToolCounts,
    pub scenarios: Vec<String>,
    pub evidence: Vec<Reference>,
}

/// Closed common virtio-MMIO behavior groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7VirtioMmioClaim {
    Identity,
    FeatureNegotiation,
    QueueSelectionAndConfiguration,
    QueueNotification,
    InterruptDeliveryAndAcknowledgement,
    OrderedStatusTransitions,
    Reset,
    ActivationFailure,
    DeviceConfigurationAccess,
    TransportStateRestore,
    TypedLogging,
    RedactedTracing,
}

/// One production device profile composed through the common MMIO transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7VirtioMmioDevice {
    pub id: String,
    pub producer_capability_id: String,
    pub implementation_path: String,
}

/// Categorized evidence for the complete virtio-MMIO aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7VirtioMmioEvidence {
    pub production: Vec<Reference>,
    pub focused: Vec<Reference>,
    pub formal: Vec<Reference>,
    pub signed: Vec<Reference>,
}

/// Common virtio-MMIO claims, device profiles, and non-substitution rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7VirtioMmioLedger {
    pub claims: Vec<Wave7VirtioMmioClaim>,
    pub devices: Vec<Wave7VirtioMmioDevice>,
    pub evidence: Wave7VirtioMmioEvidence,
    pub pci_evidence_may_substitute: bool,
}

/// Named owner for a retained nonterminal producer outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7HandoffOwner {
    Issue1351,
    Issue1373,
    Issue1378,
    Wave8,
}

/// Exact audit or feasible handoff retained by the Wave 7 aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7Handoff {
    pub capability_id: String,
    pub owner: Wave7HandoffOwner,
    pub disposition: Disposition,
}

/// One canonical documentation owner for an aggregate subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7DocumentOwner {
    pub subject: String,
    pub path: String,
}

/// Deliberate boundaries outside this aggregate certification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wave7AggregateNonclaim {
    FirecrackerBinaryOrLinuxKvmParity,
    RetainedHandoffCompletion,
    PciEvidenceForMmio,
    PortablePerformanceThreshold,
    TrackedEnvironmentReport,
    Wave8InteractionCompletion,
}

/// Categorized local evidence for the aggregate authority itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7AggregateEvidence {
    pub implementation: Vec<Reference>,
    pub validation: Vec<Reference>,
    pub signed: Vec<Reference>,
    pub documentation: Vec<Reference>,
}

/// Human-owned, source-complete authority for the terminal Wave 7 aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wave7AggregateAudit {
    pub schema_version: u32,
    pub baseline: Baseline,
    pub parent_issue: String,
    pub delivery_issue: String,
    pub upstream_sources: Vec<Wave7PinnedSource>,
    pub capability_ids: Vec<String>,
    pub target_counts: Wave7DispositionCounts,
    pub design: Vec<Wave7DesignRecord>,
    pub device_api: Wave7DeviceApiLedger,
    pub release_entries: Vec<Wave7ReleaseEntry>,
    pub tools: Vec<Wave7ToolRecord>,
    pub virtio_mmio: Wave7VirtioMmioLedger,
    pub handoffs: Vec<Wave7Handoff>,
    pub evidence: Wave7AggregateEvidence,
    pub document_owners: Vec<Wave7DocumentOwner>,
    pub nonclaims: Vec<Wave7AggregateNonclaim>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_open_enums() {
        let unknown = serde_json::from_str::<Wave7DispositionCounts>(
            r#"{"implemented_and_verified":376,"audit_required":9,"missing_platform_feasible":3,"proven_platform_impossible":30,"planned":0}"#,
        )
        .expect_err("unknown aggregate count fields must fail");
        assert!(unknown.to_string().contains("unknown field"));

        let nonclaim = serde_json::from_str::<Wave7AggregateNonclaim>(r#""everything-else""#)
            .expect_err("open aggregate nonclaims must fail");
        assert!(nonclaim.to_string().contains("unknown variant"));
    }
}
