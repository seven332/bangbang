use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, PRODUCTION_HOST_AUDIT_PATH,
    PRODUCTION_HOST_CAPABILITY_ID, ProductionHostAudit, Reference, SourceManifest,
    ValidationErrors, validate, validate_production_host_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/production-host-contract.md";
const VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/production_host_audit_validate.rs";
const TEST_PATH: &str = "tools/firecracker-capability-audit/tests/production_host_audit.rs";

/// Certify #1920 while retaining the two #1378 production-vmnet records.
pub fn validate_production_host_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &ProductionHostAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_production_host_audit(audit, manifest, inventory, repository_root)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    validate_capability_transition(inventory, &mut errors);
    validate_contract(repository_root, &mut errors);
    validate_documented_command(repository_root, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_capability_transition(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    let Some(capability) = inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == PRODUCTION_HOST_CAPABILITY_ID)
    else {
        errors.push("production-host capability is missing".to_string());
        return;
    };

    let expected_implementation = [
        (PRODUCTION_HOST_AUDIT_PATH, "\"source_clauses\": ["),
        (CONTRACT_PATH, "## Terminal production-host corpus outcome"),
        (VALIDATOR_PATH, "pub fn validate_production_host_audit("),
    ];
    let expected_validation = [(
        TEST_PATH,
        "fn production_host_terminal_transition_is_exact()",
    )];

    if capability.family != "isolation"
        || capability.disposition != Disposition::ImplementedAndVerified
        || capability.delivery_issue.is_some()
        || capability.exclusion.is_some()
        || capability.source_refs != ["corpus:production-host"]
    {
        errors.push("production-host capability is not terminal with exact ownership".to_string());
    }
    if !matches_local_reference_pairs(&capability.implementation, &expected_implementation) {
        errors.push("production-host implementation evidence drifted".to_string());
    }
    if !matches_local_reference_pairs(&capability.validation, &expected_validation) {
        errors.push("production-host validation evidence drifted".to_string());
    }

    for unrelated in &inventory.capabilities {
        if unrelated.id != PRODUCTION_HOST_CAPABILITY_ID
            && unrelated
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| issue == "#1920" || issue.ends_with("/issues/1920"))
        {
            errors.push(format!(
                "production-host certification found unrelated #1920 ownership: {}",
                unrelated.id
            ));
        }
    }
}

fn matches_local_reference_pairs(references: &[Reference], expected: &[(&str, &str)]) -> bool {
    references
        .iter()
        .filter_map(|reference| match reference {
            Reference::Local {
                path,
                anchor: Some(anchor),
            } => Some((path.as_str(), anchor.as_str())),
            Reference::Local { anchor: None, .. }
            | Reference::Github { .. }
            | Reference::Authoritative { .. } => None,
        })
        .eq(expected.iter().copied())
        && references.len() == expected.len()
}

fn validate_contract(repository_root: &Path, errors: &mut Vec<String>) {
    let Ok(contract) = std::fs::read_to_string(repository_root.join(CONTRACT_PATH)) else {
        errors.push("production-host contract is unreadable".to_string());
        return;
    };
    validate_contract_contents(&contract, errors);
}

fn validate_contract_contents(contract: &str, errors: &mut Vec<String>) {
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Pinned source identity",
        "31 ordered source clauses",
        "382/3/0/33",
        "383/2/0/33",
        "validate --production-host-final",
        "Operator, hardware, and deployment boundaries",
        "Linux/KVM",
        "positive production vmnet",
        "Developer ID/notarization",
        "corpus:production-host",
        "global `--final`",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "production-host contract omits required token: {token}"
            ));
        }
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1920 |"))
        .collect::<Vec<_>>();
    if !matches!(
        rows.as_slice(),
        [row]
            if row.contains(PRODUCTION_HOST_CAPABILITY_ID)
                && row.contains("| `implemented-and-verified` |")
    ) {
        errors.push("production-host contract requires the exact terminal #1920 row".to_string());
    }
}

fn validate_documented_command(repository_root: &Path, errors: &mut Vec<String>) {
    let command = "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --production-host-final";
    for path in [CONTRACT_PATH, "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!(
                "production-host final command is missing from {path}"
            )),
            Err(_) => errors.push(format!(
                "production-host command owner is unreadable: {path}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_contract_scope_is_fail_closed() {
        let row =
            format!("| `{PRODUCTION_HOST_CAPABILITY_ID}` | #1920 | `implemented-and-verified` |");
        let contract = format!(
            "## Pinned source identity 31 ordered source clauses 382/3/0/33 383/2/0/33 validate --production-host-final Operator, hardware, and deployment boundaries Linux/KVM positive production vmnet Developer ID/notarization corpus:production-host global `--final`\n{row}"
        );
        let mut errors = Vec::new();
        validate_contract_contents(&contract, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra =
            format!("{contract}\n| `corpus:unrelated` | #1920 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_contract_contents(&extra, &mut errors);
        assert_eq!(
            errors,
            ["production-host contract requires the exact terminal #1920 row"]
        );
    }
}
