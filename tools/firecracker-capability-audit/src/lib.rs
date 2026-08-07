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
mod tracing_certify;
mod tracing_model;
mod tracing_validate;
mod upstream;
mod validate;

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
pub use tracing_certify::{TRACING_COMPATIBILITY_CAPABILITY_IDS, validate_tracing_compatibility};
pub use tracing_model::{
    TracingAudit, TracingCallSite, TracingCallSiteCategory, TracingDelivery,
    TracingFeatureContract, TracingField, TracingLimits, TracingPhase,
};
pub use tracing_validate::{TRACING_CALL_SITE_IDS, validate_tracing_audit};
pub use upstream::{derive_source_manifest, ensure_pinned_checkout};
pub use validate::{ValidationErrors, validate};

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

/// Serialize the checked targeted formal-verification authority using canonical pretty JSON.
pub fn formal_verification_audit_json(
    audit: &FormalVerificationAudit,
) -> Result<Vec<u8>, AuditError> {
    canonical_json(audit, "formal verification audit")
}

fn canonical_json<T: serde::Serialize>(value: &T, label: &str) -> Result<Vec<u8>, AuditError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AuditError::new(format!("failed to serialize {label}: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}
