use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::inventory_phase::{classify_inventory_phase, disposition_counts};
use crate::specification_benchmark_audit_validate::SPECIFICATION_BENCHMARK_CAPABILITY_IDS;
use crate::{
    AuditMode, CapabilityInventory, Disposition, SourceManifest, SpecificationBenchmarkAudit,
    ValidationErrors, validate, validate_specification_benchmark_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/specification-benchmark-contract.md";

/// Certify exactly the terminal #1798 capability transition and totals.
pub fn validate_specification_benchmark_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &SpecificationBenchmarkAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_specification_benchmark_audit(audit, repository_root) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    validate_source_identities(manifest, &mut errors);
    validate_capabilities(inventory, &mut errors);
    validate_totals(inventory, &mut errors);
    match std::fs::read_to_string(repository_root.join(CONTRACT_PATH)) {
        Ok(contract) => validate_contract(&contract, &mut errors),
        Err(_) => errors.push("specification benchmark contract is unreadable".to_string()),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_source_identities(manifest: &SourceManifest, errors: &mut Vec<String>) {
    let sources = manifest
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    for (id, path) in [
        ("corpus:network-performance", "docs/network-performance.md"),
        ("corpus:specification", "SPECIFICATION.md"),
    ] {
        match sources.get(id) {
            Some(item)
                if item.kind == "corpus"
                    && item.path == path
                    && item.anchor == "entire-file"
                    && item.family == "specifications" => {}
            Some(_) => errors.push(format!(
                "specification benchmark source identity drifted: {id}"
            )),
            None => errors.push(format!(
                "specification benchmark source identity is missing: {id}"
            )),
        }
    }
}

fn validate_capabilities(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in SPECIFICATION_BENCHMARK_CAPABILITY_IDS {
        match capabilities.get(id) {
            Some(capability)
                if capability.family == "specifications"
                    && capability.disposition == Disposition::ImplementedAndVerified
                    && capability.delivery_issue.is_none()
                    && capability.exclusion.is_none()
                    && !capability.implementation.is_empty()
                    && !capability.validation.is_empty() => {}
            Some(_) => errors.push(format!(
                "specification benchmark capability is not terminal with evidence: {id}"
            )),
            None => errors.push(format!(
                "specification benchmark capability is missing: {id}"
            )),
        }
    }
    for capability in &inventory.capabilities {
        if !SPECIFICATION_BENCHMARK_CAPABILITY_IDS.contains(&capability.id.as_str())
            && capability.delivery_issue.as_deref() == Some("#1798")
        {
            errors.push(format!(
                "specification benchmark certification found unrelated #1798 ownership: {}",
                capability.id
            ));
        }
    }
}

fn validate_totals(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    if let Err(error) = classify_inventory_phase(inventory) {
        let (implemented, audit_required, missing, impossible) = disposition_counts(inventory);
        errors.push(format!(
            "specification benchmark terminal totals must be its exact 371/14/3/30 phase, the exact Wave 7 376/9/3/30 successor, the exact Wave 8 377/8/3/30 successor, or the exact post-Wave-8 jailer uid/gid 377/6/3/32 successor; found {implemented}/{audit_required}/{missing}/{impossible}: {error}"
        ));
    }
}

fn validate_contract(contract: &str, errors: &mut Vec<String>) {
    for token in [
        "SPECIFICATION.md",
        "docs/network-performance.md",
        "scripts/specification-benchmark.py",
        "whole-process RSS",
        "logger.missed_metrics_count",
        "#1378",
        "371 implemented-and-verified",
        "377/6/3/32",
    ] {
        if !contract.contains(token) {
            errors.push(format!(
                "specification benchmark contract omits required token: {token}"
            ));
        }
    }
    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1798 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|value| value.split_once("` |"))
        else {
            errors.push("specification benchmark contract has a malformed #1798 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "specification benchmark contract has a duplicate #1798 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "specification benchmark contract requires a terminal row: {id}"
            ));
        }
    }
    let expected = SPECIFICATION_BENCHMARK_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "specification benchmark contract requires the exact #1798 set: expected {expected:?}, found {ids:?}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_contract_scope_is_fail_closed() {
        let rows = SPECIFICATION_BENCHMARK_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | #1798 | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let exact = format!(
            "SPECIFICATION.md docs/network-performance.md scripts/specification-benchmark.py whole-process RSS logger.missed_metrics_count #1378 371 implemented-and-verified 377/6/3/32\n{rows}"
        );
        let mut errors = Vec::new();
        validate_contract(&exact, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra = format!("{exact}\n| `corpus:unrelated` | #1798 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_contract(&extra, &mut errors);
        assert!(errors.iter().any(|error| error.contains("exact #1798")));
    }
}
