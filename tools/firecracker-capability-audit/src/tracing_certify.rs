use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, LoggerProducerAudit, LoggerProducerManifest,
    Reference, SourceManifest, TracingAudit, ValidationErrors, validate_logger_compatibility,
    validate_tracing_audit,
};

const OBSERVABILITY_CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/observability-tools-specification-contract.md";

/// Exact capability scope certified by the Firecracker-shaped tracing gate.
pub const TRACING_COMPATIBILITY_CAPABILITY_IDS: [&str; 1] = ["corpus:tracing"];

/// Validate the terminal tracing slice on top of the terminal logger delivery
/// foundation without requiring unrelated Wave 7 rows to be complete.
pub fn validate_tracing_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    logger_manifest: &LoggerProducerManifest,
    logger_audit: &LoggerProducerAudit,
    tracing_audit: &TracingAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) = validate_logger_compatibility(
        manifest,
        inventory,
        logger_manifest,
        logger_audit,
        repository_root,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_tracing_audit(tracing_audit, repository_root, AuditMode::Final)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    validate_terminal_capability(inventory, &mut errors);
    match std::fs::read_to_string(repository_root.join(OBSERVABILITY_CONTRACT_PATH)) {
        Ok(contract) => validate_owned_contract(&contract, &mut errors),
        Err(_) => errors
            .push("tracing certification cannot read the Wave 7 ownership contract".to_string()),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_terminal_capability(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in TRACING_COMPATIBILITY_CAPABILITY_IDS {
        let Some(capability) = capabilities.get(id).copied() else {
            errors.push(format!("tracing certification capability is missing: {id}"));
            continue;
        };
        if capability.disposition != Disposition::ImplementedAndVerified {
            errors.push(format!(
                "tracing certification requires implemented-and-verified capability: {id}"
            ));
        }
        let (implementation, validation) = capability_evidence();
        if capability.implementation != implementation || capability.validation != validation {
            errors.push(format!(
                "tracing certification requires exact capability evidence: {id}"
            ));
        }
    }
}

fn capability_evidence() -> (Vec<Reference>, Vec<Reference>) {
    (
        local_references(&[
            (
                "compat/firecracker/v1.16.0/tracing-audit.json",
                "api.handle-request",
            ),
            (
                "crates/runtime/src/lib.rs",
                "macro_rules! bangbang_trace_scope",
            ),
            (
                "crates/runtime/src/logger/event.rs",
                "pub(super) fn encode_trace",
            ),
            (
                "crates/runtime/src/logger/tracing.rs",
                "pub struct TraceLogger",
            ),
            (
                "tools/firecracker-capability-audit/src/tracing_validate.rs",
                "pub fn validate_tracing_audit",
            ),
        ]),
        local_references(&[
            (
                "compat/firecracker/v1.16.0/tracing-contract.md",
                "Terminal certification",
            ),
            (
                "crates/bangbang/tests/process_e2e.rs",
                "fn executable_tracing_is_nested_filtered_and_value_free",
            ),
            (
                "crates/runtime/src/logger/tracing.rs",
                "fn records_nested_entry_exit_and_normalized_origin",
            ),
            (
                "crates/runtime/src/virtio_mmio.rs",
                "fn tracing_records_typed_reads_and_writes_without_guest_values",
            ),
            (
                "crates/runtime/tests/tracing_feature.rs",
                "fn trace_scope_macro_evaluates_only_in_tracing_builds",
            ),
            (
                "scripts/report-tracing-overhead.sh",
                "trace_marker=\"trace module=\"",
            ),
            (
                "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                "fn checked_tracing_compatibility_is_terminal_and_fail_closed",
            ),
            (
                "tools/snapshot-tools/tests/cli.rs",
                "fn tool_tracing_requires_a_matching_runtime_filter_and_preserves_diagnostics",
            ),
        ]),
    )
}

fn validate_owned_contract(contract: &str, errors: &mut Vec<String>) {
    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1791 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|row| row.split_once("` |"))
        else {
            errors.push("tracing certification found a malformed #1791 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "tracing certification found a duplicate #1791 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "tracing certification requires terminal #1791 contract row: {id}"
            ));
        }
    }

    let expected = TRACING_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "tracing certification requires the exact #1791 contract capability set: expected {expected:?}, found {ids:?}"
        ));
    }
}

fn local_references(entries: &[(&str, &str)]) -> Vec<Reference> {
    entries
        .iter()
        .map(|(path, anchor)| Reference::Local {
            path: (*path).to_string(),
            anchor: Some((*anchor).to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_owned_contract_scope_is_fail_closed() {
        let exact = "| `corpus:tracing` | #1791 | `implemented-and-verified` |";
        let mut errors = Vec::new();
        validate_owned_contract(exact, &mut errors);
        assert!(errors.is_empty());

        let mut errors = Vec::new();
        validate_owned_contract(
            "| `corpus:tracing` | #1791 | `audit-required` |",
            &mut errors,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal"))
        );
    }
}
