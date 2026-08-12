use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH,
    JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID, JailerSeccompContainmentAudit, Reference,
    SourceManifest, ValidationErrors, validate, validate_jailer_seccomp_containment_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/jailer-seccomp-containment-contract.md";
const VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/jailer_seccomp_containment_audit_validate.rs";
const TEST_PATH: &str =
    "tools/firecracker-capability-audit/tests/jailer_seccomp_containment_audit.rs";
const SIGNED_TEST_PATH: &str = "crates/launcher/tests/production_bundle_e2e.rs";

/// Certify #1918 without requiring independently owned positive vmnet or
/// production-host aggregate evidence.
pub fn validate_jailer_seccomp_containment_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &JailerSeccompContainmentAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_jailer_seccomp_containment_audit(audit, manifest, inventory, repository_root)
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
        .find(|capability| capability.id == JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID)
    else {
        errors.push("jailer/seccomp containment capability is missing".to_string());
        return;
    };

    let expected_implementation = [
        (
            JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH,
            "\"source_clauses\": [",
        ),
        (
            CONTRACT_PATH,
            "## Terminal jailer/seccomp containment outcome",
        ),
        (
            VALIDATOR_PATH,
            "pub fn validate_jailer_seccomp_containment_audit(",
        ),
    ];
    let expected_validation = [
        (
            SIGNED_TEST_PATH,
            "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
        ),
        (
            TEST_PATH,
            "fn jailer_seccomp_containment_terminal_transition_is_exact()",
        ),
    ];

    if capability.family != "isolation"
        || capability.disposition != Disposition::ImplementedAndVerified
        || capability.delivery_issue.is_some()
        || capability.exclusion.is_some()
        || capability.source_refs
            != [
                "corpus:design",
                "corpus:jailer",
                "corpus:production-host",
                "corpus:seccomp",
                "corpus:seccompiler",
            ]
    {
        errors.push(
            "jailer/seccomp containment capability is not terminal with exact ownership"
                .to_string(),
        );
    }
    if !matches_local_reference_pairs(&capability.implementation, &expected_implementation) {
        errors.push("jailer/seccomp containment implementation evidence drifted".to_string());
    }
    if !matches_local_reference_pairs(&capability.validation, &expected_validation) {
        errors.push("jailer/seccomp containment validation evidence drifted".to_string());
    }

    for unrelated in &inventory.capabilities {
        if unrelated.id != JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID
            && unrelated
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| issue == "#1918" || issue.ends_with("/issues/1918"))
        {
            errors.push(format!(
                "jailer/seccomp containment certification found unrelated #1918 ownership: {}",
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
        errors.push("jailer/seccomp containment contract is unreadable".to_string());
        return;
    };
    validate_contract_contents(&contract, errors);
}

fn validate_contract_contents(contract: &str, errors: &mut Vec<String>) {
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Pinned source identity",
        "46 ordered source clauses",
        "381/3/1/33",
        "382/3/0/33",
        "validate --jailer-seccomp-containment-final",
        "corpus:design",
        "portable seccompiler",
        "Linux seccomp",
        "positive vmnet connectivity",
        "General dynamic resource brokerage",
        "Developer ID/notarization",
        "corpus:production-host",
        "global `--final`",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "jailer/seccomp containment contract omits required token: {token}"
            ));
        }
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1918 |"))
        .collect::<Vec<_>>();
    if !matches!(
        rows.as_slice(),
        [row]
            if row.contains(JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID)
                && row.contains("| `implemented-and-verified` |")
    ) {
        errors.push(
            "jailer/seccomp containment contract requires the exact terminal #1918 row".to_string(),
        );
    }
}

fn validate_documented_command(repository_root: &Path, errors: &mut Vec<String>) {
    let command = "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --jailer-seccomp-containment-final";
    for path in [CONTRACT_PATH, "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!(
                "jailer/seccomp containment final command is missing from {path}"
            )),
            Err(_) => errors.push(format!(
                "jailer/seccomp containment command owner is unreadable: {path}"
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
            "| `{JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID}` | #1918 | `implemented-and-verified` |"
        );
        let contract = format!(
            "## Pinned source identity 46 ordered source clauses 381/3/1/33 382/3/0/33 validate --jailer-seccomp-containment-final corpus:design portable seccompiler Linux seccomp positive vmnet connectivity General dynamic resource brokerage Developer ID/notarization corpus:production-host global `--final`\n{row}"
        );
        let mut errors = Vec::new();
        validate_contract_contents(&contract, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra =
            format!("{contract}\n| `corpus:unrelated` | #1918 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_contract_contents(&extra, &mut errors);
        assert_eq!(
            errors,
            ["jailer/seccomp containment contract requires the exact terminal #1918 row"]
        );

        let missing = contract.replace("portable seccompiler", "compiler");
        let mut errors = Vec::new();
        validate_contract_contents(&missing, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("portable seccompiler"))
        );
    }
}
