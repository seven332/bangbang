use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, MetricsProcessProducerAudit, MetricsSchemaAuthority,
    SourceManifest, ValidationErrors, validate_metrics_process_producers,
    validate_metrics_schema_compatibility,
};

/// Validate the terminal process-producer scope without requiring later device
/// producers or aggregate metrics capabilities to have reached terminal state.
pub fn validate_metrics_process_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    metrics_authority: &MetricsSchemaAuthority,
    process_audit: &MetricsProcessProducerAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) = validate_metrics_schema_compatibility(
        manifest,
        inventory,
        metrics_authority,
        repository_root,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_metrics_process_producers(
        process_audit,
        metrics_authority,
        repository_root,
        AuditMode::Final,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}
