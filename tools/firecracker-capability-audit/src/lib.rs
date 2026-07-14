//! Firecracker capability inventory parsing, validation, and source comparison.

mod model;
mod upstream;
mod validate;

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
/// Current checked-in inventory schema.
pub const SCHEMA_VERSION: u32 = 1;
/// Repository-relative generated source manifest path.
pub const SOURCE_MANIFEST_PATH: &str = "compat/firecracker/v1.16.0/source-manifest.json";
/// Repository-relative human capability overlay path.
pub const CAPABILITY_INVENTORY_PATH: &str = "compat/firecracker/v1.16.0/capabilities.json";

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

/// Serialize a generated source manifest using canonical pretty JSON.
pub fn source_manifest_json(manifest: &SourceManifest) -> Result<Vec<u8>, AuditError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        AuditError::new(format!("failed to serialize source manifest: {error}"))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}
