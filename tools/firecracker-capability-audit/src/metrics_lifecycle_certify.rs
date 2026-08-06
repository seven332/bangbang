use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, METRICS_AGGREGATE_CAPABILITY_IDS,
    MetricsDeviceProducerAudit, MetricsLifecycleAudit, MetricsProcessProducerAudit,
    MetricsSchemaAuthority, Reference, SourceManifest, ValidationErrors,
    validate_metrics_device_compatibility, validate_metrics_lifecycle,
};

const OBSERVABILITY_CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/observability-tools-specification-contract.md";

/// Validate the complete terminal Firecracker metrics compatibility scope.
pub fn validate_metrics_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    metrics_authority: &MetricsSchemaAuthority,
    process_audit: &MetricsProcessProducerAudit,
    device_audit: &MetricsDeviceProducerAudit,
    lifecycle_audit: &MetricsLifecycleAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) = validate_metrics_device_compatibility(
        manifest,
        inventory,
        metrics_authority,
        process_audit,
        device_audit,
        repository_root,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_metrics_lifecycle(
        lifecycle_audit,
        metrics_authority,
        repository_root,
        AuditMode::Final,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    validate_terminal_capabilities(inventory, &mut errors);
    match std::fs::read_to_string(repository_root.join(OBSERVABILITY_CONTRACT_PATH)) {
        Ok(contract) => validate_owned_contract(&contract, &mut errors),
        Err(_) => errors.push(
            "metrics aggregate certification cannot read the Wave 7 ownership contract".to_string(),
        ),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_terminal_capabilities(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in METRICS_AGGREGATE_CAPABILITY_IDS {
        let Some(capability) = capabilities.get(id).copied() else {
            errors.push(format!(
                "metrics aggregate certification capability is missing: {id}"
            ));
            continue;
        };
        if capability.disposition != Disposition::ImplementedAndVerified {
            errors.push(format!(
                "metrics aggregate certification requires implemented-and-verified capability: {id}"
            ));
        }
        let (implementation, validation) = capability_evidence(id);
        if capability.implementation != implementation || capability.validation != validation {
            errors.push(format!(
                "metrics aggregate certification requires exact capability evidence: {id}"
            ));
        }
    }
}

fn capability_evidence(id: &str) -> (Vec<Reference>, Vec<Reference>) {
    match id {
        "corpus:metrics" => (
            local_references(&[
                (
                    "compat/firecracker/v1.16.0/metrics-lifecycle-audit.json",
                    "metrics.publication-transaction",
                ),
                (
                    "tools/firecracker-capability-audit/src/metrics_lifecycle_certify.rs",
                    "pub fn validate_metrics_compatibility",
                ),
            ]),
            local_references(&[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_metrics_schema_across_real_periodic_and_terminal_lifecycle",
                ),
                (
                    "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                    "fn checked_metrics_aggregate_compatibility_is_terminal_and_fail_closed",
                ),
                (
                    "tools/firecracker-capability-audit/tests/metrics_lifecycle.rs",
                    "fn checked_lifecycle_matrix_is_exact_and_fail_closed",
                ),
            ]),
        ),
        "semantic.observability:metrics-schema-producers-flush-and-lifecycle" => (
            local_references(&[
                (
                    "crates/bangbang/src/periodic_metrics.rs",
                    "FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL",
                ),
                (
                    "crates/bangbang/src/vmm.rs",
                    "pub(crate) fn handle_terminal_observability",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "pub(crate) fn flush_with_diagnostics_and_devices",
                ),
                (
                    "tools/firecracker-capability-audit/src/metrics_lifecycle_certify.rs",
                    "pub fn validate_metrics_compatibility",
                ),
            ]),
            local_references(&[
                (
                    "crates/bangbang/src/main.rs",
                    "fn terminal_metrics_sink_failure_preserves_result_and_consumes_final_attempt",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_metrics_schema_across_real_periodic_and_terminal_lifecycle",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "fn cross_producer_publication_transaction_replays_a_coherent_cut_after_partial_failure",
                ),
                (
                    "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                    "fn checked_metrics_aggregate_compatibility_is_terminal_and_fail_closed",
                ),
            ]),
        ),
        _ => (Vec::new(), Vec::new()),
    }
}

fn validate_owned_contract(contract: &str, errors: &mut Vec<String>) {
    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1790 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|row| row.split_once("` |"))
        else {
            errors.push("metrics aggregate certification found a malformed #1790 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "metrics aggregate certification found a duplicate #1790 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "metrics aggregate certification requires terminal #1790 contract row: {id}"
            ));
        }
    }

    let expected = METRICS_AGGREGATE_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "metrics aggregate certification requires the exact #1790 contract capability set: expected {expected:?}, found {ids:?}"
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
        let exact = METRICS_AGGREGATE_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | #1790 | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut errors = Vec::new();
        validate_owned_contract(&exact, &mut errors);
        assert!(errors.is_empty());

        let nonterminal = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_owned_contract(&nonterminal, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal #1790"))
        );
    }
}
