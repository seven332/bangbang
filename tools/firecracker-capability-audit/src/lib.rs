//! Firecracker capability inventory parsing, validation, and source comparison.

mod cpu_template_helper_audit_model;
mod cpu_template_helper_audit_validate;
mod cpu_template_helper_certify;
mod formal_verification_audit_model;
mod formal_verification_audit_validate;
mod formal_verification_certify;
mod guest_workflow_audit_model;
mod guest_workflow_audit_validate;
mod guest_workflow_certify;
mod host_resource_authority_audit_model;
mod host_resource_authority_audit_validate;
mod host_resource_authority_certify;
mod inventory_phase;
mod jailer_aggregate_audit_model;
mod jailer_aggregate_audit_validate;
mod jailer_aggregate_certify;
mod jailer_seccomp_containment_audit_model;
mod jailer_seccomp_containment_audit_validate;
mod jailer_seccomp_containment_certify;
mod logger_certify;
mod logger_model;
mod logger_upstream;
mod logger_validate;
mod metrics_certify;
mod metrics_device_certify;
mod metrics_device_model;
mod metrics_device_validate;
mod metrics_lifecycle_certify;
mod metrics_lifecycle_model;
mod metrics_lifecycle_validate;
mod metrics_model;
mod metrics_process_certify;
mod metrics_process_model;
mod metrics_process_validate;
mod metrics_upstream;
mod metrics_validate;
mod model;
mod multiprocess_isolation_audit_model;
mod multiprocess_isolation_audit_validate;
mod multiprocess_isolation_certify;
mod production_host_audit_model;
mod production_host_audit_validate;
mod production_host_certify;
mod specification_benchmark_audit_model;
mod specification_benchmark_audit_validate;
mod specification_benchmark_certify;
mod tracing_certify;
mod tracing_model;
mod tracing_validate;
mod upstream;
mod validate;
mod wave7_aggregate_audit_model;
mod wave7_aggregate_audit_validate;
mod wave7_aggregate_certify;
mod wave8_certification_audit_model;
mod wave8_certification_audit_validate;
mod wave8_certification_certify;

pub use cpu_template_helper_audit_model::{
    CpuTemplateHelperArtifact, CpuTemplateHelperAudit, CpuTemplateHelperExecution,
    CpuTemplateHelperFoundations, CpuTemplateHelperNonclaim, CpuTemplateHelperOperationEvidence,
    CpuTemplateHelperOperationRecord, CpuTemplateHelperOutcome, CpuTemplateHelperProvider,
    CpuTemplateHelperScenario, CpuTemplateHelperScenarioRecord, CpuTemplateHelperSelection,
};
pub use cpu_template_helper_audit_validate::{
    CPU_TEMPLATE_HELPER_OPERATION_IDS, CPU_TEMPLATE_HELPER_SCENARIOS,
    CPU_TEMPLATE_IMPLEMENTED_FOUNDATION_IDS, CPU_TEMPLATE_PLATFORM_IMPOSSIBLE_FOUNDATION_IDS,
    validate_cpu_template_helper_audit,
};
pub use cpu_template_helper_certify::{
    CPU_TEMPLATE_AGGREGATE_CAPABILITY_IDS,
    CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
    CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
    CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
    CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS, validate_cpu_template_compatibility,
    validate_cpu_template_fingerprint_compare_compatibility,
    validate_cpu_template_fingerprint_dump_compatibility,
    validate_cpu_template_helper_compatibility, validate_cpu_template_helper_transition,
    validate_cpu_template_strip_compatibility,
};
pub use formal_verification_audit_model::{
    FormalVerificationAudit, FormalVerificationCategory, FormalVerificationEvidence,
    FormalVerificationExecution, FormalVerificationHarness, FormalVerificationNonclaim,
    FormalVerificationToolchain,
};
pub use formal_verification_audit_validate::{
    FORMAL_VERIFICATION_AUDIT_PATH, FORMAL_VERIFICATION_AUDIT_SCHEMA_VERSION,
    FORMAL_VERIFICATION_HARNESS_IDS, KANI_COMPILER_TOOLCHAIN, KANI_VERSION,
    validate_formal_verification_audit,
};
pub use formal_verification_certify::{
    FORMAL_VERIFICATION_COMPATIBILITY_CAPABILITY_IDS, validate_formal_verification_compatibility,
};
pub use guest_workflow_audit_model::{
    Ext4Classification, Ext4Recipe, Ext4SidecarPolicy, GeneratedDeterminism,
    GeneratedGuestArtifact, GuestArtifact, GuestArtifactKind, GuestNetworking, GuestOutputClass,
    GuestOutputPolicy, GuestShutdown, GuestSourceNamespace, GuestWorkflowAudit,
    GuestWorkflowDelivery, GuestWorkflowDeliveryState, GuestWorkflowEvidence,
    GuestWorkflowIdentity, GuestWorkflowMode, GuestWorkflowNonclaim, GuestWorkflowProfile,
    GuestWorkflowProfileState, GuestWorkflowTimeouts,
};
pub use guest_workflow_audit_validate::{
    GUEST_ARTIFACT_IDS, GUEST_EXT4_RECIPE_IDS, GUEST_WORKFLOW_AUDIT_PATH,
    GUEST_WORKFLOW_AUDIT_SCHEMA_VERSION, GUEST_WORKFLOW_PROFILE_IDS, validate_guest_workflow_audit,
};
pub use guest_workflow_certify::{
    GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS, validate_guest_workflow_compatibility,
};
pub use host_resource_authority_audit_model::{
    HostResourceAccess, HostResourceAuthorityAudit, HostResourceClauseOutcome,
    HostResourceDispositionCounts, HostResourceEvidenceProfile, HostResourceEvidenceProfileId,
    HostResourceExternalDependency, HostResourceLifetime, HostResourceNonclaim,
    HostResourceObjectKind, HostResourcePinnedSource, HostResourceRecord,
    HostResourceResidualClassification, HostResourceResidualRecord, HostResourceRole,
    HostResourceSourceClause, HostResourceTerminalDependency,
};
pub use host_resource_authority_audit_validate::{
    HOST_RESOURCE_AUTHORITY_AUDIT_PATH, HOST_RESOURCE_AUTHORITY_AUDIT_SCHEMA_VERSION,
    HOST_RESOURCE_AUTHORITY_CAPABILITY_ID, validate_host_resource_authority_audit,
};
pub use host_resource_authority_certify::validate_host_resource_authority_compatibility;
pub use jailer_aggregate_audit_model::{
    JailerAggregateAudit, JailerAggregateNonclaim, JailerArgumentCardinality,
    JailerArgumentOutcome, JailerArgumentRecord, JailerArgumentRequirement, JailerCorpusSection,
    JailerDispositionCounts, JailerEvidenceProfile, JailerEvidenceProfileId,
    JailerOperationOutcome, JailerOperationStep, JailerPinnedSource,
};
pub use jailer_aggregate_audit_validate::{
    JAILER_AGGREGATE_AUDIT_PATH, JAILER_AGGREGATE_AUDIT_SCHEMA_VERSION,
    JAILER_AGGREGATE_CAPABILITY_IDS, validate_jailer_aggregate_audit,
};
pub use jailer_aggregate_certify::validate_jailer_aggregate_compatibility;
pub use jailer_seccomp_containment_audit_model::{
    ContainmentClauseOutcome, ContainmentDispositionCounts, ContainmentEvidenceProfile,
    ContainmentEvidenceProfileId, ContainmentExternalDependency, ContainmentNonclaim,
    ContainmentPinnedSource, ContainmentResidualClassification, ContainmentResidualRecord,
    ContainmentSourceClause, ContainmentTerminalDependency, JailerSeccompContainmentAudit,
};
pub use jailer_seccomp_containment_audit_validate::{
    JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH, JAILER_SECCOMP_CONTAINMENT_AUDIT_SCHEMA_VERSION,
    JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID, validate_jailer_seccomp_containment_audit,
};
pub use jailer_seccomp_containment_certify::validate_jailer_seccomp_containment_compatibility;
pub use logger_certify::{LOGGER_COMPATIBILITY_CAPABILITY_IDS, validate_logger_compatibility};
pub use logger_model::{
    LoggerClassDisposition, LoggerCompiledEvent, LoggerDeliveryPolicy, LoggerField,
    LoggerInvocation, LoggerInvocationSyntax, LoggerLevelPolicy, LoggerLimiterPolicy, LoggerMacro,
    LoggerModulePolicy, LoggerNonApplicableReason, LoggerOriginPolicy, LoggerProducerAudit,
    LoggerProducerClass, LoggerProducerCounts, LoggerProducerManifest, LoggerProducerMapping,
    LoggerSourceContext, LoggerSubsystem,
};
pub use logger_upstream::derive_logger_producer_manifest;
pub use logger_validate::validate_logger_producers;
pub use metrics_certify::{
    METRICS_AGGREGATE_CAPABILITY_IDS, METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS,
    validate_metrics_schema_compatibility,
};
pub use metrics_device_certify::{
    TERMINAL_DEVICE_POLICY_PROFILE_IDS, validate_metrics_device_compatibility,
};
pub use metrics_device_model::{
    MetricsDeviceProducerAudit, MetricsDeviceProducerBoundary, MetricsDeviceProducerDisposition,
    MetricsDeviceProducerRecord,
};
pub use metrics_device_validate::validate_metrics_device_producers;
pub use metrics_lifecycle_certify::validate_metrics_compatibility;
pub use metrics_lifecycle_model::{
    MetricsLifecycleAudit, MetricsLifecycleBoundary, MetricsLifecycleClaim,
    MetricsLifecycleDisposition, MetricsLifecycleRecord,
};
pub use metrics_lifecycle_validate::{
    METRICS_LIFECYCLE_SCENARIO_IDS, METRICS_PUBLICATION_TRANSACTION_CLAIMS,
    validate_metrics_lifecycle,
};
pub use metrics_model::{
    MetricsAggregation, MetricsArchitecture, MetricsCardinality, MetricsDynamicFamily,
    MetricsFieldPolicy, MetricsJsonType, MetricsPolicyProfile, MetricsProducerDisposition,
    MetricsProducerOwner, MetricsReconciliation, MetricsReconciliationKind, MetricsSchemaAuthority,
    MetricsSchemaCounts, MetricsSchemaDisposition, MetricsSchemaSource,
    MetricsSchemaSourceCandidate, MetricsSourceAnchor, MetricsSourceField, MetricsStaticRoot,
    MetricsUnit, MetricsValueKind,
};
pub use metrics_process_certify::validate_metrics_process_compatibility;
pub use metrics_process_model::{
    MetricsProcessProducerAudit, MetricsProcessProducerBoundary, MetricsProcessProducerDisposition,
    MetricsProcessProducerRecord,
};
pub use metrics_process_validate::validate_metrics_process_producers;
pub use metrics_upstream::derive_metrics_schema_source;
pub use metrics_validate::validate_metrics_schema;
pub use model::{
    AuditMode, Baseline, Capability, CapabilityInventory, Counts, Disposition, Input,
    PlatformExclusion, Reference, SourceItem, SourceManifest,
};
pub use multiprocess_isolation_audit_model::{
    MultiprocessClauseOutcome, MultiprocessDispositionCounts, MultiprocessEvidenceProfile,
    MultiprocessEvidenceProfileId, MultiprocessIsolationAudit, MultiprocessIsolationNonclaim,
    MultiprocessPinnedSource, MultiprocessResidualClassification, MultiprocessResidualRecord,
    MultiprocessSourceClause, MultiprocessTerminalDependency,
};
pub use multiprocess_isolation_audit_validate::{
    MULTIPROCESS_ISOLATION_AUDIT_PATH, MULTIPROCESS_ISOLATION_AUDIT_SCHEMA_VERSION,
    MULTIPROCESS_ISOLATION_CAPABILITY_ID, validate_multiprocess_isolation_audit,
};
pub use multiprocess_isolation_certify::validate_multiprocess_isolation_compatibility;
pub use production_host_audit_model::{
    ProductionHostAudit, ProductionHostClauseOutcome, ProductionHostDispositionCounts,
    ProductionHostEvidenceProfile, ProductionHostEvidenceProfileId,
    ProductionHostExternalDependency, ProductionHostNonclaim, ProductionHostPinnedSource,
    ProductionHostResidualClassification, ProductionHostResidualRecord, ProductionHostSourceClause,
    ProductionHostTerminalDependency,
};
pub use production_host_audit_validate::{
    PRODUCTION_HOST_AUDIT_PATH, PRODUCTION_HOST_AUDIT_SCHEMA_VERSION,
    PRODUCTION_HOST_CAPABILITY_ID, validate_production_host_audit,
    validate_production_host_upstream_source,
};
pub use production_host_certify::validate_production_host_compatibility;
pub use specification_benchmark_audit_model::{
    SpecificationBenchmarkAudit, SpecificationBenchmarkEvidence, SpecificationBenchmarkMeasurement,
    SpecificationBenchmarkNonclaim, SpecificationBenchmarkPolicy,
    SpecificationBenchmarkUpstreamSource,
};
pub use specification_benchmark_audit_validate::{
    SPECIFICATION_BENCHMARK_AUDIT_PATH, SPECIFICATION_BENCHMARK_AUDIT_SCHEMA_VERSION,
    SPECIFICATION_BENCHMARK_CAPABILITY_IDS, validate_specification_benchmark_audit,
};
pub use specification_benchmark_certify::validate_specification_benchmark_compatibility;
pub use tracing_certify::{TRACING_COMPATIBILITY_CAPABILITY_IDS, validate_tracing_compatibility};
pub use tracing_model::{
    TracingAudit, TracingCallSite, TracingCallSiteCategory, TracingDelivery,
    TracingFeatureContract, TracingField, TracingLimits, TracingPhase,
};
pub use tracing_validate::{TRACING_CALL_SITE_IDS, validate_tracing_audit};
pub use upstream::{derive_source_manifest, ensure_pinned_checkout};
pub use validate::{ValidationErrors, validate};
pub use wave7_aggregate_audit_model::{
    Wave7AggregateAudit, Wave7AggregateEvidence, Wave7AggregateNonclaim, Wave7ApiPopulation,
    Wave7DesignOutcome, Wave7DesignRecord, Wave7DesignSection, Wave7DeviceApiDimension,
    Wave7DeviceApiLedger, Wave7DeviceApiNormalization, Wave7DeviceApiSection,
    Wave7DispositionCounts, Wave7DocumentOwner, Wave7Handoff, Wave7HandoffOwner, Wave7PinnedSource,
    Wave7ReleaseEntry, Wave7ReleaseOutcome, Wave7ReleaseSection, Wave7Tool, Wave7ToolCounts,
    Wave7ToolExecution, Wave7ToolRecord, Wave7VirtioMmioClaim, Wave7VirtioMmioDevice,
    Wave7VirtioMmioEvidence, Wave7VirtioMmioLedger,
};
pub use wave7_aggregate_audit_validate::{
    WAVE7_AGGREGATE_AUDIT_PATH, WAVE7_AGGREGATE_AUDIT_SCHEMA_VERSION,
    WAVE7_AGGREGATE_CAPABILITY_IDS, validate_wave7_aggregate_audit,
};
pub use wave7_aggregate_certify::{
    WAVE7_OWNED_CAPABILITY_IDS, WAVE7_PLATFORM_IMPOSSIBLE_CAPABILITY_IDS,
    validate_wave7_aggregate_compatibility,
};
pub use wave8_certification_audit_model::{
    Wave8AuthorityEvidence, Wave8CertificationAudit, Wave8DeliveryHierarchy, Wave8DeliveryOutcome,
    Wave8DeliveryParent, Wave8DispositionCounts, Wave8DocumentOwner, Wave8Domain, Wave8Handoff,
    Wave8HandoffOwner, Wave8InteractionPair, Wave8Nonclaim, Wave8Outcome, Wave8PlatformMechanism,
    Wave8PlatformObservation, Wave8PlatformReview, Wave8RejectedAlternative, Wave8Scenario,
    Wave8ScenarioExecution,
};
pub use wave8_certification_audit_validate::{
    WAVE8_CERTIFICATION_AUDIT_PATH, WAVE8_CERTIFICATION_AUDIT_SCHEMA_VERSION,
    WAVE8_CERTIFICATION_CAPABILITY_ID, validate_wave8_certification_audit,
};
pub use wave8_certification_certify::{
    WAVE8_OWNED_CAPABILITY_IDS, validate_wave8_certification_compatibility,
};

use std::fmt;
use std::path::Path;

/// Firecracker release audited by this inventory.
pub const FIRECRACKER_VERSION: &str = "1.16.0";
/// Exact Firecracker commit audited by this inventory.
pub const FIRECRACKER_COMMIT: &str = "d83d72b710361a10294480131377b1b00b163af8";
/// Compatibility target audited by this inventory.
pub const FIRECRACKER_TARGET: &str = "aarch64-macos-hvf";
/// Current checked-in inventory schema.
pub const SCHEMA_VERSION: u32 = 1;
/// Current generated source-manifest format.
pub const GENERATOR_VERSION: u32 = 1;
/// Current checked-in logger producer schema.
pub const LOGGER_PRODUCER_SCHEMA_VERSION: u32 = 1;
/// Current generated logger producer format.
pub const LOGGER_PRODUCER_GENERATOR_VERSION: u32 = 1;
/// Current checked-in metrics schema authority.
pub const METRICS_SCHEMA_VERSION: u32 = 1;
/// Current generated metrics schema source format.
pub const METRICS_SCHEMA_GENERATOR_VERSION: u32 = 1;
/// Current checked-in process-producer audit format.
pub const METRICS_PROCESS_PRODUCER_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Current checked-in device-producer audit format.
pub const METRICS_DEVICE_PRODUCER_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Current checked aggregate metrics lifecycle audit format.
pub const METRICS_LIFECYCLE_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Current checked developer-tracing audit format.
pub const TRACING_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Current checked aggregate CPU-template-helper audit format.
pub const CPU_TEMPLATE_HELPER_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative generated source manifest path.
pub const SOURCE_MANIFEST_PATH: &str = "compat/firecracker/v1.16.0/source-manifest.json";
/// Repository-relative human capability overlay path.
pub const CAPABILITY_INVENTORY_PATH: &str = "compat/firecracker/v1.16.0/capabilities.json";
/// Repository-relative generated logger producer manifest path.
pub const LOGGER_PRODUCER_MANIFEST_PATH: &str =
    "compat/firecracker/v1.16.0/logger-producer-manifest.json";
/// Repository-relative human logger producer audit path.
pub const LOGGER_PRODUCER_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/logger-producer-audit.json";
/// Repository-relative canonical metrics schema authority path.
pub const METRICS_SCHEMA_AUTHORITY_PATH: &str = "compat/firecracker/v1.16.0/metrics-schema.json";
/// Repository-relative exact process-producer audit path.
pub const METRICS_PROCESS_PRODUCER_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/metrics-process-producer-audit.json";
/// Repository-relative exact device-producer audit path.
pub const METRICS_DEVICE_PRODUCER_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/metrics-device-producer-audit.json";
/// Repository-relative aggregate metrics lifecycle audit path.
pub const METRICS_LIFECYCLE_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/metrics-lifecycle-audit.json";
/// Repository-relative developer-tracing audit path.
pub const TRACING_AUDIT_PATH: &str = "compat/firecracker/v1.16.0/tracing-audit.json";
/// Repository-relative aggregate CPU-template-helper audit path.
pub const CPU_TEMPLATE_HELPER_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/cpu-template-helper-audit.json";

/// Error produced while reading, parsing, or deriving an inventory.
#[derive(Debug)]
pub struct AuditError(String);

impl AuditError {
    /// Create an audit error with a stable redacted diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AuditError {}

/// Read and parse a checked-in source manifest.
pub fn read_source_manifest(path: &Path) -> Result<SourceManifest, AuditError> {
    let bytes = std::fs::read(path)
        .map_err(|error| AuditError::new(format!("failed to read source manifest: {error}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AuditError::new(format!("failed to parse source manifest: {error}")))
}

/// Read and parse a checked-in capability overlay.
pub fn read_capability_inventory(path: &Path) -> Result<CapabilityInventory, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read capability inventory: {error}"))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AuditError::new(format!("failed to parse capability inventory: {error}")))
}

/// Read and parse a checked-in logger producer manifest.
pub fn read_logger_producer_manifest(path: &Path) -> Result<LoggerProducerManifest, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read logger producer manifest: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!("failed to parse logger producer manifest: {error}"))
    })
}

/// Read and parse a checked-in logger producer audit overlay.
pub fn read_logger_producer_audit(path: &Path) -> Result<LoggerProducerAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read logger producer audit: {error}"))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AuditError::new(format!("failed to parse logger producer audit: {error}")))
}

/// Read and parse the checked metrics schema authority.
pub fn read_metrics_schema_authority(path: &Path) -> Result<MetricsSchemaAuthority, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read metrics schema authority: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!("failed to parse metrics schema authority: {error}"))
    })
}

/// Read and parse the checked process-producer audit.
pub fn read_metrics_process_producer_audit(
    path: &Path,
) -> Result<MetricsProcessProducerAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read metrics process producer audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse metrics process producer audit: {error}"
        ))
    })
}

/// Read and parse the checked device-producer audit.
pub fn read_metrics_device_producer_audit(
    path: &Path,
) -> Result<MetricsDeviceProducerAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read metrics device producer audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse metrics device producer audit: {error}"
        ))
    })
}

/// Read and parse the checked aggregate metrics lifecycle audit.
pub fn read_metrics_lifecycle_audit(path: &Path) -> Result<MetricsLifecycleAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read metrics lifecycle audit: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!("failed to parse metrics lifecycle audit: {error}"))
    })
}

/// Read and parse the checked developer-tracing audit.
pub fn read_tracing_audit(path: &Path) -> Result<TracingAudit, AuditError> {
    let bytes = std::fs::read(path)
        .map_err(|error| AuditError::new(format!("failed to read tracing audit: {error}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AuditError::new(format!("failed to parse tracing audit: {error}")))
}

/// Read and parse the checked aggregate CPU-template-helper audit.
pub fn read_cpu_template_helper_audit(path: &Path) -> Result<CpuTemplateHelperAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read CPU-template helper audit: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse CPU-template helper audit: {error}"
        ))
    })
}

/// Read and parse the checked guest-workflow artifact authority.
pub fn read_guest_workflow_audit(path: &Path) -> Result<GuestWorkflowAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read guest workflow audit: {error}"))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AuditError::new(format!("failed to parse guest workflow audit: {error}")))
}

/// Read and parse the checked aggregate jailer authority.
pub fn read_jailer_aggregate_audit(path: &Path) -> Result<JailerAggregateAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read jailer aggregate audit: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!("failed to parse jailer aggregate audit: {error}"))
    })
}

/// Read and parse the checked multiprocess isolation authority.
pub fn read_multiprocess_isolation_audit(
    path: &Path,
) -> Result<MultiprocessIsolationAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read multiprocess isolation audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse multiprocess isolation audit: {error}"
        ))
    })
}

/// Read and parse the checked host-resource authority.
pub fn read_host_resource_authority_audit(
    path: &Path,
) -> Result<HostResourceAuthorityAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read host-resource authority audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse host-resource authority audit: {error}"
        ))
    })
}

/// Read and parse the checked jailer/seccomp containment composition.
pub fn read_jailer_seccomp_containment_audit(
    path: &Path,
) -> Result<JailerSeccompContainmentAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read jailer/seccomp containment audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse jailer/seccomp containment audit: {error}"
        ))
    })
}

/// Read and parse the checked production-host authority.
pub fn read_production_host_audit(path: &Path) -> Result<ProductionHostAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read production-host audit: {error}"))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AuditError::new(format!("failed to parse production-host audit: {error}")))
}

/// Read and parse the checked targeted formal-verification authority.
pub fn read_formal_verification_audit(path: &Path) -> Result<FormalVerificationAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read formal verification audit: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse formal verification audit: {error}"
        ))
    })
}

/// Read and parse the checked specification-benchmark authority.
pub fn read_specification_benchmark_audit(
    path: &Path,
) -> Result<SpecificationBenchmarkAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read specification benchmark audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse specification benchmark audit: {error}"
        ))
    })
}

/// Read and parse the checked Wave 7 aggregate authority.
pub fn read_wave7_aggregate_audit(path: &Path) -> Result<Wave7AggregateAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!("failed to read Wave 7 aggregate audit: {error}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!("failed to parse Wave 7 aggregate audit: {error}"))
    })
}

/// Read and parse the checked Wave 8 certification authority.
pub fn read_wave8_certification_audit(path: &Path) -> Result<Wave8CertificationAudit, AuditError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuditError::new(format!(
            "failed to read Wave 8 certification audit: {error}"
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AuditError::new(format!(
            "failed to parse Wave 8 certification audit: {error}"
        ))
    })
}

/// Serialize a generated source manifest using canonical pretty JSON.
pub fn source_manifest_json(manifest: &SourceManifest) -> Result<Vec<u8>, AuditError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        AuditError::new(format!("failed to serialize source manifest: {error}"))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Serialize a generated logger producer manifest using canonical pretty JSON.
pub fn logger_producer_manifest_json(
    manifest: &LoggerProducerManifest,
) -> Result<Vec<u8>, AuditError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        AuditError::new(format!(
            "failed to serialize logger producer manifest: {error}"
        ))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Serialize a human logger producer audit using canonical pretty JSON.
pub fn logger_producer_audit_json(audit: &LoggerProducerAudit) -> Result<Vec<u8>, AuditError> {
    let mut bytes = serde_json::to_vec_pretty(audit).map_err(|error| {
        AuditError::new(format!(
            "failed to serialize logger producer audit: {error}"
        ))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Serialize a source-only metrics schema candidate using canonical pretty JSON.
pub fn metrics_schema_source_candidate_json(
    candidate: &MetricsSchemaSourceCandidate,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(candidate, "metrics schema source candidate")
}

/// Serialize the checked metrics schema authority using canonical pretty JSON.
pub fn metrics_schema_authority_json(
    authority: &MetricsSchemaAuthority,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(authority, "metrics schema authority")
}

/// Serialize the human process-producer audit using canonical pretty JSON.
pub fn metrics_process_producer_audit_json(
    audit: &MetricsProcessProducerAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "metrics process producer audit")
}

/// Serialize the human device-producer audit using canonical pretty JSON.
pub fn metrics_device_producer_audit_json(
    audit: &MetricsDeviceProducerAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "metrics device producer audit")
}

/// Serialize the aggregate metrics lifecycle audit using canonical pretty JSON.
pub fn metrics_lifecycle_audit_json(audit: &MetricsLifecycleAudit) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "metrics lifecycle audit")
}

/// Serialize the checked developer-tracing audit using canonical pretty JSON.
pub fn tracing_audit_json(audit: &TracingAudit) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "tracing audit")
}

/// Serialize the aggregate CPU-template-helper audit using canonical pretty JSON.
pub fn cpu_template_helper_audit_json(
    audit: &CpuTemplateHelperAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "CPU-template helper audit")
}

/// Serialize the checked guest-workflow authority using canonical pretty JSON.
pub fn guest_workflow_audit_json(audit: &GuestWorkflowAudit) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "guest workflow audit")
}

/// Serialize the checked aggregate jailer authority using canonical pretty JSON.
pub fn jailer_aggregate_audit_json(audit: &JailerAggregateAudit) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "jailer aggregate audit")
}

/// Serialize the checked multiprocess isolation authority using canonical pretty JSON.
pub fn multiprocess_isolation_audit_json(
    audit: &MultiprocessIsolationAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "multiprocess isolation audit")
}

/// Serialize the checked host-resource authority using canonical pretty JSON.
pub fn host_resource_authority_audit_json(
    audit: &HostResourceAuthorityAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "host-resource authority audit")
}

/// Serialize the checked jailer/seccomp containment composition using canonical pretty JSON.
pub fn jailer_seccomp_containment_audit_json(
    audit: &JailerSeccompContainmentAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "jailer/seccomp containment audit")
}

/// Serialize the checked production-host authority using canonical pretty JSON.
pub fn production_host_audit_json(audit: &ProductionHostAudit) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "production-host audit")
}

/// Serialize the checked targeted formal-verification authority using canonical pretty JSON.
pub fn formal_verification_audit_json(
    audit: &FormalVerificationAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "formal verification audit")
}

/// Serialize the checked specification-benchmark authority using canonical pretty JSON.
pub fn specification_benchmark_audit_json(
    audit: &SpecificationBenchmarkAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "specification benchmark audit")
}

/// Serialize the checked Wave 7 aggregate authority using canonical pretty JSON.
pub fn wave7_aggregate_audit_json(audit: &Wave7AggregateAudit) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "Wave 7 aggregate audit")
}

/// Serialize the checked Wave 8 certification authority using canonical pretty JSON.
pub fn wave8_certification_audit_json(
    audit: &Wave8CertificationAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "Wave 8 certification audit")
}

fn canonical_json<T: serde::Serialize>(value: &T, label: &str) -> Result<Vec<u8>, AuditError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AuditError::new(format!("failed to serialize {label}: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}
