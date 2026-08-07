use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, FormalVerificationAudit, SourceManifest,
    ValidationErrors, validate, validate_formal_verification_audit,
};

/// Exact capability scope certified by the terminal targeted Kani gate.
pub const FORMAL_VERIFICATION_COMPATIBILITY_CAPABILITY_IDS: [&str; 1] =
    ["corpus:formal-verification"];

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/formal-verification-contract.md";

/// Validate the terminal formal-verification slice without requiring unrelated
/// capabilities to have reached terminal dispositions.
pub fn validate_formal_verification_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &FormalVerificationAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_formal_verification_audit(audit, repository_root) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    let source_items = manifest
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    match source_items.get("corpus:formal-verification") {
        Some(item)
            if item.kind == "corpus"
                && item.key == "formal-verification"
                && item.path == "docs/formal-verification.md"
                && item.anchor == "entire-file"
                && item.family == "specifications" => {}
        Some(_) => {
            errors.push("formal verification certification source identity has drifted".to_string())
        }
        None => {
            errors.push("formal verification certification source identity is missing".to_string())
        }
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    match capabilities.get("corpus:formal-verification") {
        Some(capability)
            if capability.family == "specifications"
                && capability.source_refs == ["corpus:formal-verification"]
                && capability.disposition == Disposition::ImplementedAndVerified
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none()
                && !capability.implementation.is_empty()
                && !capability.validation.is_empty() => {}
        Some(_) => errors.push(
            "formal verification certification requires exact implemented-and-verified evidence"
                .to_string(),
        ),
        None => errors.push("formal verification certification capability is missing".to_string()),
    }
    for capability in &inventory.capabilities {
        if capability.id != "corpus:formal-verification"
            && capability.delivery_issue.as_deref() == Some("#1797")
        {
            errors.push(format!(
                "formal verification certification found unrelated #1797 ownership: {}",
                capability.id
            ));
        }
    }

    match std::fs::read_to_string(repository_root.join(CONTRACT_PATH)) {
        Ok(contract) => validate_owned_contract(&contract, &mut errors),
        Err(_) => {
            errors.push("formal verification certification contract is unreadable".to_string())
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_owned_contract(contract: &str, errors: &mut Vec<String>) {
    for token in [
        "Kani 0.67.0",
        "nightly-2025-11-21",
        "pager-limits-admission",
        "virtqueue-ranges",
        "token-bucket-refill-accounting",
        "pager-artifact-ranges",
        "virtio-mmio-status-transitions",
        "FFI/HVF",
        "whole-system correctness",
    ] {
        if !contract.contains(token) {
            errors.push(format!(
                "formal verification certification contract omits required token: {token}"
            ));
        }
    }

    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1797 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|row| row.split_once("` |"))
        else {
            errors
                .push("formal verification certification found a malformed #1797 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "formal verification certification found a duplicate #1797 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "formal verification certification requires terminal #1797 contract row: {id}"
            ));
        }
    }

    let expected = FORMAL_VERIFICATION_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "formal verification certification requires the exact #1797 contract capability set: expected {expected:?}, found {ids:?}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(row: &str) -> String {
        format!(
            "Kani 0.67.0 nightly-2025-11-21 pager-limits-admission virtqueue-ranges token-bucket-refill-accounting pager-artifact-ranges virtio-mmio-status-transitions FFI/HVF whole-system correctness\n{row}"
        )
    }

    #[test]
    fn exact_owned_contract_scope_is_fail_closed() {
        let exact =
            contract("| `corpus:formal-verification` | #1797 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_owned_contract(&exact, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra = format!("{exact}\n| `corpus:unrelated` | #1797 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_owned_contract(&extra, &mut errors);
        assert!(errors.iter().any(|error| error.contains("exact #1797")));

        let nonterminal = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_owned_contract(&nonterminal, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal #1797"))
        );
    }
}
