use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, GuestWorkflowAudit, GuestWorkflowDeliveryState,
    GuestWorkflowProfileState, SourceManifest, ValidationErrors, validate,
    validate_guest_workflow_audit,
};

/// Exact capability scope certified by the terminal macOS guest workflow gate.
pub const GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS: [&str; 2] =
    ["corpus:getting-started", "corpus:rootfs-and-kernel"];

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/guest-workflow-contract.md";

/// Validate the terminal guest-workflow slice without requiring unrelated
/// capabilities to have reached terminal dispositions.
pub fn validate_guest_workflow_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &GuestWorkflowAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_guest_workflow_audit(audit, inventory, repository_root)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    if audit.delivery.state != GuestWorkflowDeliveryState::Complete
        || audit.profiles.len() != 2
        || audit
            .profiles
            .iter()
            .any(|profile| profile.state != GuestWorkflowProfileState::ImplementedAndVerified)
    {
        errors.push(
            "guest workflow certification requires the exact terminal delivery and profiles"
                .to_string(),
        );
    }

    let source_ids = manifest
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS {
        if !source_ids.contains(id) {
            errors.push(format!(
                "guest workflow certification source identity is missing: {id}"
            ));
        }
        match capabilities.get(id) {
            Some(capability)
                if capability.disposition == Disposition::ImplementedAndVerified
                    && capability.delivery_issue.is_none()
                    && capability.exclusion.is_none()
                    && !capability.implementation.is_empty()
                    && !capability.validation.is_empty() => {}
            Some(_) => errors.push(format!(
                "guest workflow certification requires implemented-and-verified evidence: {id}"
            )),
            None => errors.push(format!(
                "guest workflow certification capability is missing: {id}"
            )),
        }
    }

    match std::fs::read_to_string(repository_root.join(CONTRACT_PATH)) {
        Ok(contract) => validate_owned_contract(&contract, &mut errors),
        Err(_) => errors.push("guest workflow certification contract is unreadable".to_string()),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_owned_contract(contract: &str, errors: &mut Vec<String>) {
    for token in [
        "docs/getting-started.md",
        "docs/rootfs-and-kernel-setup.md",
        "scripts/run-macos-guest-workflow.py api",
        "scripts/run-macos-guest-workflow.py no-api",
        "BANGBANG_ROOTFS_WORKFLOW_OK",
        "BANGBANG_ROOTFS_WORKFLOW_FAIL",
        "Linux/KVM",
        "FreeBSD",
    ] {
        if !contract.contains(token) {
            errors.push(format!(
                "guest workflow certification contract omits required token: {token}"
            ));
        }
    }

    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1796 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|row| row.split_once("` |"))
        else {
            errors.push("guest workflow certification found a malformed #1796 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "guest workflow certification found a duplicate #1796 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "guest workflow certification requires terminal #1796 contract row: {id}"
            ));
        }
    }

    let expected = GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "guest workflow certification requires the exact #1796 contract capability set: expected {expected:?}, found {ids:?}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract_rows(ids: impl IntoIterator<Item = &'static str>) -> String {
        let rows = ids
            .into_iter()
            .map(|id| format!("| `{id}` | #1796 | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "docs/getting-started.md docs/rootfs-and-kernel-setup.md scripts/run-macos-guest-workflow.py api scripts/run-macos-guest-workflow.py no-api BANGBANG_ROOTFS_WORKFLOW_OK BANGBANG_ROOTFS_WORKFLOW_FAIL Linux/KVM FreeBSD\n{rows}"
        )
    }

    #[test]
    fn exact_owned_contract_scope_is_fail_closed() {
        let exact = contract_rows(GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS);
        let mut errors = Vec::new();
        validate_owned_contract(&exact, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra = format!("{exact}\n| `corpus:unrelated` | #1796 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_owned_contract(&extra, &mut errors);
        assert!(errors.iter().any(|error| error.contains("exact #1796")));

        let nonterminal = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_owned_contract(&nonterminal, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal #1796"))
        );
    }
}
