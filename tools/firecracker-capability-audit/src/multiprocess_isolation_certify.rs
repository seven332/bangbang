use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, MULTIPROCESS_ISOLATION_AUDIT_PATH,
    MULTIPROCESS_ISOLATION_CAPABILITY_ID, MultiprocessIsolationAudit, Reference, SourceManifest,
    ValidationErrors, validate, validate_multiprocess_isolation_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/multiprocess-isolation-contract.md";
const VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/multiprocess_isolation_audit_validate.rs";
const TEST_PATH: &str = "tools/firecracker-capability-audit/tests/multiprocess_isolation_audit.rs";
const SIGNED_TEST_PATH: &str = "crates/launcher/tests/production_bundle_e2e.rs";

/// Certify the exact #1914 multiprocess isolation transition without requiring
/// independently owned production-host, network, or broader isolation records.
pub fn validate_multiprocess_isolation_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &MultiprocessIsolationAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_multiprocess_isolation_audit(audit, manifest, inventory, repository_root)
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
        .find(|capability| capability.id == MULTIPROCESS_ISOLATION_CAPABILITY_ID)
    else {
        errors.push("multiprocess isolation capability is missing".to_string());
        return;
    };

    let expected_implementation = [
        (MULTIPROCESS_ISOLATION_AUDIT_PATH, "\"source_clauses\": ["),
        (CONTRACT_PATH, "## Terminal multiprocess isolation outcome"),
        (
            VALIDATOR_PATH,
            "pub fn validate_multiprocess_isolation_audit(",
        ),
    ];
    let expected_validation = [
        (
            SIGNED_TEST_PATH,
            "fn concurrent_sessions_remain_independent_when_one_worker_crashes()",
        ),
        (TEST_PATH, "fn multiprocess_terminal_transition_is_exact()"),
    ];

    if capability.family != "isolation"
        || capability.disposition != Disposition::ImplementedAndVerified
        || capability.delivery_issue.is_some()
        || capability.exclusion.is_some()
        || capability.source_refs != ["corpus:design", "corpus:production-host"]
    {
        errors.push(
            "multiprocess isolation capability is not terminal with exact ownership".to_string(),
        );
    }
    if !matches_local_reference_pairs(&capability.implementation, &expected_implementation) {
        errors.push("multiprocess isolation implementation evidence drifted".to_string());
    }
    if !matches_local_reference_pairs(&capability.validation, &expected_validation) {
        errors.push("multiprocess isolation validation evidence drifted".to_string());
    }

    for unrelated in &inventory.capabilities {
        if unrelated.id != MULTIPROCESS_ISOLATION_CAPABILITY_ID
            && unrelated
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| issue == "#1914" || issue.ends_with("/issues/1914"))
        {
            errors.push(format!(
                "multiprocess isolation certification found unrelated #1914 ownership: {}",
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
        errors.push("multiprocess isolation contract is unreadable".to_string());
        return;
    };
    validate_contract_contents(&contract, errors);
}

fn validate_contract_contents(contract: &str, errors: &mut Vec<String>) {
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Pinned source identity",
        "13 ordered source clauses",
        "379/3/3/33",
        "380/3/2/33",
        "validate --multiprocess-isolation-final",
        "one and only one microVM",
        "privileged third-party",
        "overwatcher",
        "unique `uid` and `gid`",
        "general dynamic resource broker",
        "simultaneous uncatchable",
        "corpus:production-host",
        "global `--final`",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "multiprocess isolation contract omits required token: {token}"
            ));
        }
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1914 |"))
        .collect::<Vec<_>>();
    if !matches!(
        rows.as_slice(),
        [row]
            if row.contains(MULTIPROCESS_ISOLATION_CAPABILITY_ID)
                && row.contains("| `implemented-and-verified` |")
    ) {
        errors.push(
            "multiprocess isolation contract requires the exact terminal #1914 row".to_string(),
        );
    }
}

fn validate_documented_command(repository_root: &Path, errors: &mut Vec<String>) {
    let command = "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --multiprocess-isolation-final";
    for path in [CONTRACT_PATH, "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!(
                "multiprocess isolation final command is missing from {path}"
            )),
            Err(_) => errors.push(format!(
                "multiprocess isolation command owner is unreadable: {path}"
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
            "| `{MULTIPROCESS_ISOLATION_CAPABILITY_ID}` | #1914 | `implemented-and-verified` |"
        );
        let contract = format!(
            "## Pinned source identity 13 ordered source clauses 379/3/3/33 380/3/2/33 validate --multiprocess-isolation-final one and only one microVM privileged third-party overwatcher unique `uid` and `gid` general dynamic resource broker simultaneous uncatchable corpus:production-host global `--final`\n{row}"
        );
        let mut errors = Vec::new();
        validate_contract_contents(&contract, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra =
            format!("{contract}\n| `corpus:unrelated` | #1914 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_contract_contents(&extra, &mut errors);
        assert_eq!(
            errors,
            ["multiprocess isolation contract requires the exact terminal #1914 row"]
        );

        let missing = contract.replace("overwatcher", "watcher");
        let mut errors = Vec::new();
        validate_contract_contents(&missing, &mut errors);
        assert!(errors.iter().any(|error| error.contains("overwatcher")));
    }
}
