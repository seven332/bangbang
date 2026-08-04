use std::collections::BTreeMap;
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, LoggerProducerAudit, LoggerProducerManifest,
    SourceManifest, ValidationErrors, validate, validate_logger_producers,
};

/// Exact capability scope certified by the Firecracker logger aggregate gate.
pub const LOGGER_COMPATIBILITY_CAPABILITY_IDS: [&str; 11] = [
    "api-operation:PUT /logger",
    "api-path:/logger",
    "api-property:FullVmConfiguration.logger",
    "api-property:Logger.level",
    "api-property:Logger.log_path",
    "api-property:Logger.module",
    "api-property:Logger.show_level",
    "api-property:Logger.show_log_origin",
    "api-schema:Logger",
    "corpus:logger",
    "semantic.observability:logger-delivery-filtering-loss-and-redaction",
];

/// Validate the terminal logger slice without requiring unrelated capabilities
/// to have reached a terminal disposition.
pub fn validate_logger_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    logger_manifest: &LoggerProducerManifest,
    logger_audit: &LoggerProducerAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_logger_producers(
        logger_manifest,
        logger_audit,
        repository_root,
        AuditMode::Final,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in LOGGER_COMPATIBILITY_CAPABILITY_IDS {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::ImplementedAndVerified => {}
            Some(_) => errors.push(format!(
                "logger certification requires implemented-and-verified capability: {id}"
            )),
            None => errors.push(format!("logger certification capability is missing: {id}")),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}
