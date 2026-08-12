use std::collections::BTreeMap;
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, Reference, SourceManifest, ValidationErrors,
    WAVE8_CERTIFICATION_AUDIT_PATH, WAVE8_CERTIFICATION_CAPABILITY_ID, Wave8CertificationAudit,
    validate, validate_wave8_certification_audit,
};

const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/wave8-certification-contract.md";
const VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/wave8_certification_audit_validate.rs";
const TEST_PATH: &str = "tools/firecracker-capability-audit/tests/wave8_certification_audit.rs";

/// Exact capability identity delivered by #1881.
pub const WAVE8_OWNED_CAPABILITY_IDS: [&str; 1] = [WAVE8_CERTIFICATION_CAPABILITY_ID];

/// Certify the exact final platform-feasible Wave 8 transition.
pub fn validate_wave8_certification_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &Wave8CertificationAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_wave8_certification_audit(audit, manifest, inventory, repository_root)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    validate_capability_transition(inventory, &mut errors);
    validate_contract(repository_root, &mut errors);
    validate_documented_commands(repository_root, &mut errors);

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
    let Some(capability) = capabilities.get(WAVE8_CERTIFICATION_CAPABILITY_ID) else {
        errors.push("Wave 8 capability is missing".to_string());
        return;
    };
    if capability.disposition != Disposition::ImplementedAndVerified
        || capability.delivery_issue.is_some()
        || capability.exclusion.is_some()
    {
        errors.push("Wave 8 capability is not terminal without open ownership".to_string());
    }
    let expected_implementation = [
        (WAVE8_CERTIFICATION_AUDIT_PATH, "\"scenarios\": ["),
        (VALIDATOR_PATH, "fn validate_interactions("),
    ];
    let expected_validation = [(TEST_PATH, "fn wave8_terminal_transition_is_exact()")];
    if !matches_local_reference_pairs(&capability.implementation, &expected_implementation) {
        errors.push("Wave 8 capability implementation evidence drifted".to_string());
    }
    if !matches_local_reference_pairs(&capability.validation, &expected_validation) {
        errors.push("Wave 8 capability validation evidence drifted".to_string());
    }

    for candidate in &inventory.capabilities {
        if candidate.id != WAVE8_CERTIFICATION_CAPABILITY_ID
            && candidate
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| issue == "#1881" || issue.ends_with("/issues/1881"))
        {
            errors.push(format!(
                "Wave 8 certification found unrelated #1881 ownership: {}",
                candidate.id
            ));
        }
    }
}

fn validate_contract(repository_root: &Path, errors: &mut Vec<String>) {
    let Ok(contract) = std::fs::read_to_string(repository_root.join(CONTRACT_PATH)) else {
        errors.push("Wave 8 certification contract is unreadable".to_string());
        return;
    };
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Certified interaction matrix",
        "seven domains",
        "21 unordered pairs",
        "377 implemented",
        "eight audit-required",
        "three missing-platform-feasible",
        "30 proven-platform-impossible",
        "six #1373",
        "377/6/3/32",
        "five audit-required",
        "33 proven-platform-impossible",
        "377/5/3/33",
        "four #1373",
        "two #1378",
        "three #1351",
        "379/3/3/33",
        "380/3/2/33",
        "381/3/1/33",
        "three audit-required",
        "one #1373",
        "two #1351",
        "validate --wave8-final",
        "global `--final`",
        "live GitHub",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "Wave 8 certification contract omits required token: {token}"
            ));
        }
    }
    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1881 |"))
        .collect::<Vec<_>>();
    if !matches!(
        rows.as_slice(),
        [row]
            if row.contains(WAVE8_CERTIFICATION_CAPABILITY_ID)
                && row.contains("| `implemented-and-verified` |")
    ) {
        errors.push("Wave 8 contract requires exactly one terminal #1881 row".to_string());
    }
}

fn validate_documented_commands(repository_root: &Path, errors: &mut Vec<String>) {
    let command =
        "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --wave8-final";
    for path in [".github/workflows/ci.yml", "AGENTS.md", "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!("Wave 8 final command is missing from {path}")),
            Err(_) => errors.push(format!("Wave 8 command owner is unreadable: {path}")),
        }
    }
}

fn matches_local_reference_pairs(references: &[Reference], expected: &[(&str, &str)]) -> bool {
    references.len() == expected.len()
        && references
            .iter()
            .zip(expected)
            .all(|(reference, (expected_path, expected_anchor))| {
                matches!(
                    reference,
                    Reference::Local {
                        path,
                        anchor: Some(anchor),
                    } if path == expected_path && anchor == expected_anchor
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_identity_is_exact() {
        assert_eq!(
            WAVE8_OWNED_CAPABILITY_IDS,
            [WAVE8_CERTIFICATION_CAPABILITY_ID]
        );
    }
}
