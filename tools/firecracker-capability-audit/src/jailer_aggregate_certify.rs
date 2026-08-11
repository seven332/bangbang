use std::collections::BTreeMap;
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, JAILER_AGGREGATE_AUDIT_PATH,
    JAILER_AGGREGATE_CAPABILITY_IDS, JailerAggregateAudit, Reference, SourceManifest,
    ValidationErrors, validate, validate_jailer_aggregate_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/jailer-aggregate-contract.md";
const VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/jailer_aggregate_audit_validate.rs";
const TEST_PATH: &str = "tools/firecracker-capability-audit/tests/jailer_aggregate_audit.rs";
const SIGNED_TEST_PATH: &str = "crates/launcher/tests/production_bundle_e2e.rs";

/// Certify the exact terminal aggregate jailer transition without requiring
/// unrelated isolation, production-host, or vmnet records to be terminal.
pub fn validate_jailer_aggregate_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &JailerAggregateAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_jailer_aggregate_audit(audit, manifest, inventory, repository_root)
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
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let expected_implementation = [
        (JAILER_AGGREGATE_AUDIT_PATH, "\"operation_steps\": ["),
        (CONTRACT_PATH, "## Terminal aggregate outcome"),
        (VALIDATOR_PATH, "pub fn validate_jailer_aggregate_audit("),
    ];
    let expected_validation = [
        (
            SIGNED_TEST_PATH,
            "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
        ),
        (TEST_PATH, "fn jailer_terminal_transition_is_exact()"),
    ];

    for id in JAILER_AGGREGATE_CAPABILITY_IDS {
        let Some(capability) = capabilities.get(id) else {
            errors.push(format!("jailer aggregate capability is missing: {id}"));
            continue;
        };
        if capability.family != "isolation"
            || capability.disposition != Disposition::ImplementedAndVerified
            || capability.delivery_issue.is_some()
            || capability.exclusion.is_some()
            || capability.source_refs != [id]
        {
            errors.push(format!(
                "jailer aggregate capability is not terminal with exact ownership: {id}"
            ));
        }
        if !matches_local_reference_pairs(&capability.implementation, &expected_implementation) {
            errors.push(format!(
                "jailer aggregate capability implementation evidence drifted: {id}"
            ));
        }
        if !matches_local_reference_pairs(&capability.validation, &expected_validation) {
            errors.push(format!(
                "jailer aggregate capability validation evidence drifted: {id}"
            ));
        }
    }

    for capability in &inventory.capabilities {
        if !JAILER_AGGREGATE_CAPABILITY_IDS.contains(&capability.id.as_str())
            && capability
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| issue == "#1912" || issue.ends_with("/issues/1912"))
        {
            errors.push(format!(
                "jailer aggregate certification found unrelated #1912 ownership: {}",
                capability.id
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
        errors.push("jailer aggregate contract is unreadable".to_string());
        return;
    };
    validate_contract_contents(&contract, errors);
}

fn validate_contract_contents(contract: &str, errors: &mut Vec<String>) {
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Pinned source identity",
        "13 ordered argument leaves",
        "16 ordered operation steps",
        "seven corpus sections",
        "377/5/3/33",
        "379/3/3/33",
        "validate --jailer-final",
        "default-close",
        "marker-only environment",
        "no-file=2048",
        "literal per-run executable copy",
        "production-host",
        "global `--final`",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "jailer aggregate contract omits required token: {token}"
            ));
        }
    }
    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1912 |"))
        .collect::<Vec<_>>();
    if rows.len() != 2
        || JAILER_AGGREGATE_CAPABILITY_IDS.iter().any(|id| {
            rows.iter()
                .all(|row| !row.contains(id) || !row.contains("| `implemented-and-verified` |"))
        })
    {
        errors.push(
            "jailer aggregate contract requires the exact two terminal #1912 rows".to_string(),
        );
    }
}

fn validate_documented_command(repository_root: &Path, errors: &mut Vec<String>) {
    let command =
        "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --jailer-final";
    for path in [CONTRACT_PATH, "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!("jailer final command is missing from {path}")),
            Err(_) => errors.push(format!("jailer command owner is unreadable: {path}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_contract_scope_is_fail_closed() {
        let rows = JAILER_AGGREGATE_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | #1912 | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let contract = format!(
            "## Pinned source identity 13 ordered argument leaves 16 ordered operation steps seven corpus sections 377/5/3/33 379/3/3/33 validate --jailer-final default-close marker-only environment no-file=2048 literal per-run executable copy production-host global `--final`\n{rows}"
        );
        let mut errors = Vec::new();
        validate_contract_contents(&contract, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let extra =
            format!("{contract}\n| `corpus:unrelated` | #1912 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_contract_contents(&extra, &mut errors);
        assert_eq!(
            errors,
            ["jailer aggregate contract requires the exact two terminal #1912 rows"]
        );

        let missing = contract.replace("marker-only environment", "marker environment");
        let mut errors = Vec::new();
        validate_contract_contents(&missing, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("marker-only environment"))
        );
    }
}
