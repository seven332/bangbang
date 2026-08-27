use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, HOST_RESOURCE_AUTHORITY_AUDIT_PATH,
    HOST_RESOURCE_AUTHORITY_CAPABILITY_ID, HostResourceAuthorityAudit, Reference, SourceManifest,
    ValidationErrors, validate, validate_host_resource_authority_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/host-resource-authority-contract.md";
const VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/host_resource_authority_audit_validate.rs";
const TEST_PATH: &str = "tools/firecracker-capability-audit/tests/host_resource_authority_audit.rs";
const SIGNED_TEST_PATH: &str = "crates/launcher/tests/production_bundle_e2e.rs";

/// Certify the exact #1916 host-resource transition without requiring
/// independently owned production-host or positive vmnet evidence.
pub fn validate_host_resource_authority_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &HostResourceAuthorityAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_host_resource_authority_audit(audit, manifest, inventory, repository_root)
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
        .find(|capability| capability.id == HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
    else {
        errors.push("host-resource authority capability is missing".to_string());
        return;
    };

    let expected_implementation = [
        (HOST_RESOURCE_AUTHORITY_AUDIT_PATH, "\"source_clauses\": ["),
        (CONTRACT_PATH, "## Terminal host-resource authority outcome"),
        (
            VALIDATOR_PATH,
            "pub fn validate_host_resource_authority_audit(",
        ),
    ];
    let expected_validation = [
        (
            SIGNED_TEST_PATH,
            "fn signed_grants_authorize_only_typed_read_write_and_directory_operations()",
        ),
        (TEST_PATH, "fn host_resource_terminal_transition_is_exact()"),
    ];

    if capability.family != "isolation"
        || capability.disposition != Disposition::ImplementedAndVerified
        || capability.delivery_issue.is_some()
        || capability.exclusion.is_some()
        || capability.source_refs
            != [
                "corpus:design",
                "corpus:jailer",
                "corpus:network-setup",
                "corpus:production-host",
            ]
    {
        errors.push(
            "host-resource authority capability is not terminal with exact ownership".to_string(),
        );
    }
    if !matches_local_reference_pairs(&capability.implementation, &expected_implementation) {
        errors.push("host-resource authority implementation evidence drifted".to_string());
    }
    if !matches_local_reference_pairs(&capability.validation, &expected_validation) {
        errors.push("host-resource authority validation evidence drifted".to_string());
    }

    for unrelated in &inventory.capabilities {
        if unrelated.id != HOST_RESOURCE_AUTHORITY_CAPABILITY_ID
            && unrelated
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| issue == "#1916" || issue.ends_with("/issues/1916"))
        {
            errors.push(format!(
                "host-resource authority certification found unrelated #1916 ownership: {}",
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
        errors.push("host-resource authority contract is unreadable".to_string());
        return;
    };
    validate_contract_contents(&contract, errors);
}

fn validate_contract_contents(contract: &str, errors: &mut Vec<String>) {
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Pinned source identity",
        "30 ordered source clauses",
        "18 resource roles",
        "five access modes",
        "380/3/2/33",
        "381/3/1/33",
        "validate --host-resource-authority-final",
        "privileged third-party",
        "vhost-user backend",
        "general dynamic resource broker",
        "positive vmnet connectivity",
        "corpus:design",
        "corpus:production-host",
        "global `--final`",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "host-resource authority contract omits required token: {token}"
            ));
        }
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1916 |"))
        .collect::<Vec<_>>();
    if !matches!(
        rows.as_slice(),
        [row]
            if row.contains(HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
                && row.contains("| `implemented-and-verified` |")
    ) {
        errors.push(
            "host-resource authority contract requires the exact terminal #1916 row".to_string(),
        );
    }
}

fn validate_documented_command(repository_root: &Path, errors: &mut Vec<String>) {
    let command = "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --host-resource-authority-final";
    for path in [CONTRACT_PATH, "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!(
                "host-resource authority final command is missing from {path}"
            )),
            Err(_) => errors.push(format!(
                "host-resource authority command owner is unreadable: {path}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_contract_scope_is_fail_closed() {
        let row = format!(
            "| `{HOST_RESOURCE_AUTHORITY_CAPABILITY_ID}` | #1916 | `implemented-and-verified` |"
        );
        let contract = format!(
            "## Pinned source identity 30 ordered source clauses 18 resource roles five access modes 380/3/2/33 381/3/1/33 validate --host-resource-authority-final privileged third-party vhost-user backend general dynamic resource broker positive vmnet connectivity corpus:design corpus:production-host global `--final`\n{row}"
        );
        let mut errors = Vec::new();
        validate_contract_contents(&contract, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra =
            format!("{contract}\n| `corpus:unrelated` | #1916 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_contract_contents(&extra, &mut errors);
        assert_eq!(
            errors,
            ["host-resource authority contract requires the exact terminal #1916 row"]
        );

        let missing = contract.replace("vhost-user backend", "backend");
        let mut errors = Vec::new();
        validate_contract_contents(&missing, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("vhost-user backend"))
        );
    }
}
