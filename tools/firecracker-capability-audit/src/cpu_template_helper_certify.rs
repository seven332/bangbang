use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, Capability, CapabilityInventory, Disposition, Reference, SourceManifest,
    ValidationErrors, validate,
};

const HELPER_CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/cpu-template-helper-contract.md";
const OWNERSHIP_CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/observability-tools-specification-contract.md";

/// Exact dump and verify capability scope certified by #1792.
pub const CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS: [&str; 7] = [
    "tool-argument:cpu-template-helper/template/dump/config",
    "tool-argument:cpu-template-helper/template/dump/output",
    "tool-argument:cpu-template-helper/template/dump/template",
    "tool-argument:cpu-template-helper/template/verify/config",
    "tool-argument:cpu-template-helper/template/verify/template",
    "tool-operation:cpu-template-helper/template/dump",
    "tool-operation:cpu-template-helper/template/verify",
];

/// Exact later helper capabilities that #1792 must leave nonterminal.
pub const CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS: [&str; 14] = [
    "corpus:cpu-template-helper",
    "corpus:cpu-templates",
    "semantic.cpu:configuration-templates-and-feature-state",
    "tool-argument:cpu-template-helper/fingerprint/compare/curr",
    "tool-argument:cpu-template-helper/fingerprint/compare/filters",
    "tool-argument:cpu-template-helper/fingerprint/compare/prev",
    "tool-argument:cpu-template-helper/fingerprint/dump/config",
    "tool-argument:cpu-template-helper/fingerprint/dump/output",
    "tool-argument:cpu-template-helper/fingerprint/dump/template",
    "tool-argument:cpu-template-helper/template/strip/paths",
    "tool-argument:cpu-template-helper/template/strip/suffix",
    "tool-operation:cpu-template-helper/fingerprint/compare",
    "tool-operation:cpu-template-helper/fingerprint/dump",
    "tool-operation:cpu-template-helper/template/strip",
];

/// Require #1792 to be either its exact historical handoff or its exact
/// terminal state, while retaining the separately owned helper capabilities.
pub fn validate_cpu_template_helper_transition(
    inventory: &CapabilityInventory,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();

    let owned = CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS
        .iter()
        .filter_map(|id| match capabilities.get(id).copied() {
            Some(capability) => Some(capability),
            None => {
                errors.push(format!(
                    "CPU-template helper certification capability is missing: {id}"
                ));
                None
            }
        })
        .collect::<Vec<_>>();

    if owned.len() == CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS.len() {
        let historical = owned.iter().all(|capability| {
            capability.disposition == Disposition::AuditRequired
                && capability.implementation.is_empty()
                && capability.validation.is_empty()
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none()
        });
        let (implementation, validation) = capability_evidence();
        let terminal = owned.iter().all(|capability| {
            capability.disposition == Disposition::ImplementedAndVerified
                && capability.implementation == implementation
                && capability.validation == validation
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none()
        });
        if !historical && !terminal {
            errors.push(
                "CPU-template helper certification requires the exact historical #1792 handoff or the exact terminal transition"
                    .to_string(),
            );
        }
    }

    for id in CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS {
        match capabilities.get(id).copied() {
            Some(capability) if is_exact_retained(capability) => {}
            Some(_) => errors.push(format!(
                "CPU-template helper certification requires the later capability to remain exactly audit-required: {id}"
            )),
            None => errors.push(format!(
                "CPU-template helper retained capability is missing: {id}"
            )),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

/// Validate the terminal #1792 dump and verify scope without requiring later
/// strip, fingerprint, corpus, or aggregate owners to be complete.
pub fn validate_cpu_template_helper_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_cpu_template_helper_transition(inventory) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::ImplementedAndVerified => {}
            Some(_) => errors.push(format!(
                "CPU-template helper certification requires implemented-and-verified capability: {id}"
            )),
            None => errors.push(format!(
                "CPU-template helper certification capability is missing: {id}"
            )),
        }
    }

    match std::fs::read_to_string(repository_root.join(HELPER_CONTRACT_PATH)) {
        Ok(contract) => validate_helper_contract(&contract, &mut errors),
        Err(_) => errors
            .push("CPU-template helper certification cannot read the helper contract".to_string()),
    }
    match std::fs::read_to_string(repository_root.join(OWNERSHIP_CONTRACT_PATH)) {
        Ok(contract) => validate_ownership_contract(&contract, &mut errors),
        Err(_) => errors.push(
            "CPU-template helper certification cannot read the Wave 7 ownership contract"
                .to_string(),
        ),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn is_exact_retained(capability: &Capability) -> bool {
    capability.disposition == Disposition::AuditRequired
        && capability.implementation.is_empty()
        && capability.validation.is_empty()
        && capability.delivery_issue.is_none()
        && capability.exclusion.is_none()
}

fn capability_evidence() -> (Vec<Reference>, Vec<Reference>) {
    (
        local_references(&[
            (
                "crates/hvf/src/cpu_template.rs",
                "pub(crate) fn capture_common_arm64_cpu_template_values",
            ),
            (
                "crates/hvf/src/cpu_template_inspection.rs",
                "pub fn inspect_effective_arm64_cpu_template",
            ),
            (
                "tools/cpu-template-helper/src/cli.rs",
                "pub fn run_cli_with_provider",
            ),
            (
                "tools/cpu-template-helper/src/main.rs",
                "fn main() -> ExitCode",
            ),
            (
                "tools/cpu-template-helper/src/provider.rs",
                "pub struct HvfEffectiveCpuTemplateProvider",
            ),
            (
                "tools/cpu-template-helper/src/publication.rs",
                "pub fn publish_new_artifact",
            ),
        ]),
        local_references(&[
            (
                "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                "Terminal certification",
            ),
            (
                "crates/hvf/src/cpu_template_inspection.rs",
                "fn capture_plan_uses_the_exact_runtime_census_and_native_widths",
            ),
            (
                "tools/cpu-template-helper/tests/cli.rs",
                "fn help_and_version_are_the_only_portable_stdout_successes",
            ),
            (
                "tools/cpu-template-helper/tests/hvf_e2e.rs",
                "fn signed_two_vcpu_default_dump_is_canonical_private_and_reparseable",
            ),
            (
                "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                "fn checked_cpu_template_helper_compatibility_is_terminal_and_fail_closed",
            ),
        ]),
    )
}

fn validate_helper_contract(contract: &str, errors: &mut Vec<String>) {
    validate_contract_rows(
        contract,
        None,
        CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "implemented-and-verified",
        "helper contract",
        errors,
    );
}

fn validate_ownership_contract(contract: &str, errors: &mut Vec<String>) {
    validate_contract_rows(
        contract,
        Some("#1792"),
        CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "implemented-and-verified",
        "#1792 ownership contract",
        errors,
    );

    let expected = CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut found = BTreeSet::new();
    for row in contract.lines().filter(|line| {
        line.starts_with("| `")
            && (line.contains("| #1793 |")
                || line.contains("| #1794 |")
                || line.contains("| #1795 |"))
    }) {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|value| value.split_once("` |"))
        else {
            errors.push(
                "CPU-template helper certification found a malformed retained ownership row"
                    .to_string(),
            );
            continue;
        };
        if !found.insert(id) {
            errors.push(format!(
                "CPU-template helper certification found a duplicate retained ownership row: {id}"
            ));
        }
        if !row.contains("| `audit-required` |") {
            errors.push(format!(
                "CPU-template helper certification requires retained ownership row to remain audit-required: {id}"
            ));
        }
    }
    if found != expected {
        errors.push(format!(
            "CPU-template helper certification requires the exact #1793-#1795 retained capability set: expected {expected:?}, found {found:?}"
        ));
    }
}

fn validate_contract_rows<const N: usize>(
    contract: &str,
    owner: Option<&str>,
    expected_ids: [&str; N],
    disposition: &str,
    label: &str,
    errors: &mut Vec<String>,
) {
    let expected = expected_ids.into_iter().collect::<BTreeSet<_>>();
    let mut found = BTreeSet::new();
    for row in contract.lines().filter(|line| {
        line.starts_with("| `") && owner.is_none_or(|owner| line.contains(&format!("| {owner} |")))
    }) {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|value| value.split_once("` |"))
        else {
            errors.push(format!(
                "CPU-template helper certification found a malformed {label} row"
            ));
            continue;
        };
        if !found.insert(id) {
            errors.push(format!(
                "CPU-template helper certification found a duplicate {label} row: {id}"
            ));
        }
        if !row.contains(&format!("| `{disposition}` |")) {
            errors.push(format!(
                "CPU-template helper certification requires terminal {label} row: {id}"
            ));
        }
    }
    if found != expected {
        errors.push(format!(
            "CPU-template helper certification requires the exact {label} capability set: expected {expected:?}, found {found:?}"
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
    fn exact_helper_contract_scope_is_fail_closed() {
        let exact = CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut errors = Vec::new();
        validate_helper_contract(&exact, &mut errors);
        assert!(errors.is_empty());

        let hybrid = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_helper_contract(&hybrid, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal"))
        );

        let extra = format!(
            "{exact}\n| `tool-operation:cpu-template-helper/template/extra` | `implemented-and-verified` |"
        );
        let mut errors = Vec::new();
        validate_helper_contract(&extra, &mut errors);
        assert!(errors.iter().any(|error| error.contains("exact helper")));
    }
}
