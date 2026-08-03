//! Firecracker capability inventory parsing, validation, and source comparison.

mod logger_model;
mod logger_upstream;
mod logger_validate;
mod model;
mod upstream;
mod validate;

pub use logger_model::{
    LoggerClassDisposition, LoggerCompiledEvent, LoggerDeliveryPolicy, LoggerField,
    LoggerInvocation, LoggerInvocationSyntax, LoggerLevelPolicy, LoggerLimiterPolicy, LoggerMacro,
    LoggerModulePolicy, LoggerNonApplicableReason, LoggerOriginPolicy, LoggerProducerAudit,
    LoggerProducerClass, LoggerProducerCounts, LoggerProducerManifest, LoggerProducerMapping,
    LoggerSourceContext, LoggerSubsystem,
};
pub use logger_upstream::derive_logger_producer_manifest;
pub use logger_validate::validate_logger_producers;
pub use model::{
    AuditMode, Baseline, Capability, CapabilityInventory, Counts, Disposition, Input,
    PlatformExclusion, Reference, SourceItem, SourceManifest,
};
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
