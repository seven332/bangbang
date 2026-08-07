use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, Capability, CapabilityInventory, Disposition, Reference, SourceManifest,
    ValidationErrors, validate,
};

const HELPER_CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/cpu-template-helper-contract.md";
const STRIP_CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/cpu-template-strip-contract.md";
const FINGERPRINT_DUMP_CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/cpu-template-fingerprint-contract.md";
const FINGERPRINT_COMPARE_CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/cpu-template-fingerprint-compare-contract.md";
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

/// Exact portable template-strip capability scope certified by #1793.
pub const CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS: [&str; 3] = [
    "tool-argument:cpu-template-helper/template/strip/paths",
    "tool-argument:cpu-template-helper/template/strip/suffix",
    "tool-operation:cpu-template-helper/template/strip",
];

/// Exact fingerprint-dump capability scope certified by #1866.
pub const CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS: [&str; 4] = [
    "tool-argument:cpu-template-helper/fingerprint/dump/config",
    "tool-argument:cpu-template-helper/fingerprint/dump/output",
    "tool-argument:cpu-template-helper/fingerprint/dump/template",
    "tool-operation:cpu-template-helper/fingerprint/dump",
];

/// Exact fingerprint-compare capability scope certified by #1867.
pub const CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS: [&str; 4] = [
    "tool-argument:cpu-template-helper/fingerprint/compare/curr",
    "tool-argument:cpu-template-helper/fingerprint/compare/filters",
    "tool-argument:cpu-template-helper/fingerprint/compare/prev",
    "tool-operation:cpu-template-helper/fingerprint/compare",
];

/// Exact later aggregate capabilities that #1867 must leave nonterminal.
pub const CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS: [&str; 3] = [
    "corpus:cpu-template-helper",
    "corpus:cpu-templates",
    "semantic.cpu:configuration-templates-and-feature-state",
];

/// Require the CPU-template helper inventory to be one exact ordered delivery
/// phase through helper, strip, and fingerprint-dump certification.
pub fn validate_cpu_template_helper_transition(
    inventory: &CapabilityInventory,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();

    let helper = collect_scope(
        &capabilities,
        &CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template helper certification",
        &mut errors,
    );
    let strip = collect_scope(
        &capabilities,
        &CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template strip certification",
        &mut errors,
    );
    let fingerprint_dump = collect_scope(
        &capabilities,
        &CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-dump certification",
        &mut errors,
    );
    let fingerprint_compare = collect_scope(
        &capabilities,
        &CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-compare certification",
        &mut errors,
    );

    if helper.len() == CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS.len()
        && strip.len() == CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS.len()
        && fingerprint_dump.len()
            == CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS.len()
        && fingerprint_compare.len()
            == CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS.len()
    {
        let (helper_implementation, helper_validation) = helper_capability_evidence();
        let (strip_implementation, strip_validation) = strip_capability_evidence();
        let (fingerprint_implementation, fingerprint_validation) =
            fingerprint_dump_capability_evidence();
        let (compare_implementation, compare_validation) =
            fingerprint_compare_capability_evidence();
        let helper_historical = helper
            .iter()
            .all(|capability| is_exact_retained(capability));
        let helper_terminal = helper.iter().all(|capability| {
            is_exact_terminal(capability, &helper_implementation, &helper_validation)
        });
        let strip_historical = strip.iter().all(|capability| is_exact_retained(capability));
        let strip_terminal = strip.iter().all(|capability| {
            is_exact_terminal(capability, &strip_implementation, &strip_validation)
        });
        let fingerprint_historical = fingerprint_dump
            .iter()
            .all(|capability| is_exact_retained(capability));
        let fingerprint_terminal = fingerprint_dump.iter().all(|capability| {
            is_exact_terminal(
                capability,
                &fingerprint_implementation,
                &fingerprint_validation,
            )
        });
        let compare_historical = fingerprint_compare
            .iter()
            .all(|capability| is_exact_retained(capability));
        let compare_terminal = fingerprint_compare.iter().all(|capability| {
            is_exact_terminal(capability, &compare_implementation, &compare_validation)
        });

        let valid_historical =
            helper_historical && strip_historical && fingerprint_historical && compare_historical;
        let valid_helper_terminal =
            helper_terminal && strip_historical && fingerprint_historical && compare_historical;
        let valid_strip_terminal =
            helper_terminal && strip_terminal && fingerprint_historical && compare_historical;
        let valid_fingerprint_terminal =
            helper_terminal && strip_terminal && fingerprint_terminal && compare_historical;
        let valid_compare_terminal =
            helper_terminal && strip_terminal && fingerprint_terminal && compare_terminal;
        if !valid_historical
            && !valid_helper_terminal
            && !valid_strip_terminal
            && !valid_fingerprint_terminal
            && !valid_compare_terminal
        {
            errors.push(
                "CPU-template helper certification requires one exact ordered historical, #1792 helper, #1793 strip, #1866 fingerprint-dump, or #1867 fingerprint-compare phase"
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

    finish(errors)
}

/// Validate the terminal #1866 fingerprint-dump scope while retaining compare,
/// corpus, and aggregate owners.
pub fn validate_cpu_template_fingerprint_dump_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = common_validation(manifest, inventory, repository_root);
    let capabilities = capability_map(inventory);

    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-dump certification helper dependency",
        &mut errors,
    );
    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-dump certification strip dependency",
        &mut errors,
    );
    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-dump certification",
        &mut errors,
    );
    read_contract(
        repository_root,
        FINGERPRINT_DUMP_CONTRACT_PATH,
        "CPU-template fingerprint-dump certification cannot read the fingerprint contract",
        |contract, errors| {
            validate_contract_rows(
                contract,
                None,
                CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
                "implemented-and-verified",
                "fingerprint-dump contract",
                errors,
            );
        },
        &mut errors,
    );
    read_contract(
        repository_root,
        OWNERSHIP_CONTRACT_PATH,
        "CPU-template fingerprint-dump certification cannot read the Wave 7 ownership contract",
        validate_fingerprint_ownership_contract,
        &mut errors,
    );

    finish(errors)
}

/// Validate the terminal #1867 fingerprint-compare scope while retaining the
/// independently owned corpus and aggregate capabilities.
pub fn validate_cpu_template_fingerprint_compare_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = common_validation(manifest, inventory, repository_root);
    let capabilities = capability_map(inventory);

    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-compare certification helper dependency",
        &mut errors,
    );
    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-compare certification strip dependency",
        &mut errors,
    );
    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-compare certification dump dependency",
        &mut errors,
    );
    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template fingerprint-compare certification",
        &mut errors,
    );
    read_contract(
        repository_root,
        FINGERPRINT_COMPARE_CONTRACT_PATH,
        "CPU-template fingerprint-compare certification cannot read the compare contract",
        |contract, errors| {
            validate_contract_rows(
                contract,
                None,
                CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
                "implemented-and-verified",
                "fingerprint-compare contract",
                errors,
            );
        },
        &mut errors,
    );
    read_contract(
        repository_root,
        OWNERSHIP_CONTRACT_PATH,
        "CPU-template fingerprint-compare certification cannot read the Wave 7 ownership contract",
        validate_fingerprint_compare_ownership_contract,
        &mut errors,
    );

    finish(errors)
}

/// Validate the terminal #1792 dump and verify scope without requiring strip,
/// fingerprint, corpus, or aggregate owners to be complete.
pub fn validate_cpu_template_helper_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = common_validation(manifest, inventory, repository_root);
    let capabilities = capability_map(inventory);

    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template helper certification",
        &mut errors,
    );
    read_contract(
        repository_root,
        HELPER_CONTRACT_PATH,
        "CPU-template helper certification cannot read the helper contract",
        |contract, errors| {
            validate_contract_rows(
                contract,
                None,
                CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
                "implemented-and-verified",
                "helper contract",
                errors,
            );
        },
        &mut errors,
    );
    read_contract(
        repository_root,
        OWNERSHIP_CONTRACT_PATH,
        "CPU-template helper certification cannot read the Wave 7 ownership contract",
        validate_helper_ownership_contract,
        &mut errors,
    );

    finish(errors)
}

/// Validate the terminal #1793 portable strip scope while retaining the
/// independently owned fingerprint, corpus, and aggregate capabilities.
pub fn validate_cpu_template_strip_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = common_validation(manifest, inventory, repository_root);
    let capabilities = capability_map(inventory);

    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template strip certification dependency",
        &mut errors,
    );
    require_terminal_scope(
        &capabilities,
        &CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
        "CPU-template strip certification",
        &mut errors,
    );
    read_contract(
        repository_root,
        STRIP_CONTRACT_PATH,
        "CPU-template strip certification cannot read the strip contract",
        |contract, errors| {
            validate_contract_rows(
                contract,
                None,
                CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
                "implemented-and-verified",
                "strip contract",
                errors,
            );
        },
        &mut errors,
    );
    read_contract(
        repository_root,
        OWNERSHIP_CONTRACT_PATH,
        "CPU-template strip certification cannot read the Wave 7 ownership contract",
        validate_strip_ownership_contract,
        &mut errors,
    );

    finish(errors)
}

fn common_validation(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_cpu_template_helper_transition(inventory) {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    errors
}

fn capability_map(inventory: &CapabilityInventory) -> BTreeMap<&str, &Capability> {
    inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect()
}

fn collect_scope<'a, const N: usize>(
    capabilities: &BTreeMap<&str, &'a Capability>,
    ids: &[&str; N],
    label: &str,
    errors: &mut Vec<String>,
) -> Vec<&'a Capability> {
    ids.iter()
        .filter_map(|id| match capabilities.get(id).copied() {
            Some(capability) => Some(capability),
            None => {
                errors.push(format!("{label} capability is missing: {id}"));
                None
            }
        })
        .collect()
}

fn require_terminal_scope<const N: usize>(
    capabilities: &BTreeMap<&str, &Capability>,
    ids: &[&str; N],
    label: &str,
    errors: &mut Vec<String>,
) {
    for id in ids {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::ImplementedAndVerified => {}
            Some(_) => errors.push(format!(
                "{label} requires implemented-and-verified capability: {id}"
            )),
            None => errors.push(format!("{label} capability is missing: {id}")),
        }
    }
}

fn is_exact_retained(capability: &Capability) -> bool {
    capability.disposition == Disposition::AuditRequired
        && capability.implementation.is_empty()
        && capability.validation.is_empty()
        && capability.delivery_issue.is_none()
        && capability.exclusion.is_none()
}

fn is_exact_terminal(
    capability: &Capability,
    implementation: &[Reference],
    validation: &[Reference],
) -> bool {
    capability.disposition == Disposition::ImplementedAndVerified
        && capability.implementation == implementation
        && capability.validation == validation
        && capability.delivery_issue.is_none()
        && capability.exclusion.is_none()
}

fn helper_capability_evidence() -> (Vec<Reference>, Vec<Reference>) {
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

fn strip_capability_evidence() -> (Vec<Reference>, Vec<Reference>) {
    (
        local_references(&[
            ("tools/cpu-template-helper/src/cli.rs", "Strip {"),
            (
                "tools/cpu-template-helper/src/input.rs",
                "pub(crate) fn prepare_strip_input",
            ),
            (
                "tools/cpu-template-helper/src/strip.rs",
                "pub fn strip_cpu_template_documents",
            ),
            (
                "tools/cpu-template-helper/src/strip_publication.rs",
                "pub(crate) fn publish_strip_artifacts",
            ),
        ]),
        local_references(&[
            (
                "compat/firecracker/v1.16.0/cpu-template-strip-contract.md",
                "Terminal certification",
            ),
            (
                "tools/cpu-template-helper/src/strip.rs",
                "fn strips_native_width_differences_and_preserves_missing_entries",
            ),
            (
                "tools/cpu-template-helper/src/strip_publication.rs",
                "fn rolls_back_every_observed_split_boundary_in_both_modes",
            ),
            (
                "tools/cpu-template-helper/tests/cli.rs",
                "fn strip_default_and_explicit_suffixes_are_portable_and_silent",
            ),
            (
                "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                "fn checked_cpu_template_strip_compatibility_is_terminal_and_fail_closed",
            ),
        ]),
    )
}

fn fingerprint_dump_capability_evidence() -> (Vec<Reference>, Vec<Reference>) {
    (
        local_references(&[
            (
                "tools/cpu-template-helper/src/cli.rs",
                "Fingerprint(FingerprintOperation::Dump",
            ),
            (
                "tools/cpu-template-helper/src/fingerprint.rs",
                "pub fn dump_with_providers",
            ),
            (
                "tools/cpu-template-helper/src/host.rs",
                "pub struct SystemHostFingerprintProvider",
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
                "compat/firecracker/v1.16.0/cpu-template-fingerprint-contract.md",
                "Terminal certification",
            ),
            (
                "tools/cpu-template-helper/src/fingerprint.rs",
                "fn macos_golden_bytes_round_trip_and_accept_other_canonical_producer_versions",
            ),
            (
                "tools/cpu-template-helper/src/host.rs",
                "fn capture_queries_exact_public_facts_once_in_order",
            ),
            (
                "tools/cpu-template-helper/tests/cli.rs",
                "fn fingerprint_failures_are_bounded_and_publish_neither_default_nor_explicit_output",
            ),
            (
                "tools/cpu-template-helper/tests/hvf_e2e.rs",
                "fn signed_fingerprint_dump_covers_real_macos_default_static_and_custom_selection",
            ),
            (
                "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                "fn checked_cpu_template_fingerprint_dump_compatibility_is_terminal_and_fail_closed",
            ),
        ]),
    )
}

fn fingerprint_compare_capability_evidence() -> (Vec<Reference>, Vec<Reference>) {
    (
        local_references(&[
            (
                "tools/cpu-template-helper/src/cli.rs",
                "Fingerprint(FingerprintOperation::Compare",
            ),
            (
                "tools/cpu-template-helper/src/fingerprint.rs",
                "pub fn decode_cpu_fingerprint_document",
            ),
            (
                "tools/cpu-template-helper/src/fingerprint_compare.rs",
                "pub fn compare_cpu_fingerprints",
            ),
            (
                "tools/cpu-template-helper/src/input.rs",
                "pub fn read_regular_utf8",
            ),
            (
                "tools/cpu-template-helper/src/strip.rs",
                "pub fn strip_cpu_template_documents",
            ),
        ]),
        local_references(&[
            (
                "compat/firecracker/v1.16.0/cpu-template-fingerprint-compare-contract.md",
                "Terminal certification",
            ),
            (
                "tools/cpu-template-helper/src/fingerprint_compare.rs",
                "fn guest_difference_reuses_native_width_strip_and_preserves_missing_identity",
            ),
            (
                "tools/cpu-template-helper/src/fingerprint_compare.rs",
                "fn macos_defaults_emit_all_differences_in_public_order_and_repeat",
            ),
            (
                "tools/cpu-template-helper/tests/cli.rs",
                "fn fingerprint_compare_emits_exact_canonical_difference_and_fixed_order",
            ),
            (
                "tools/cpu-template-helper/tests/cli.rs",
                "fn fingerprint_compare_rejects_strict_document_and_file_failures_without_mutation",
            ),
            (
                "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                "fn checked_cpu_template_fingerprint_compare_compatibility_is_terminal_and_fail_closed",
            ),
        ]),
    )
}

fn validate_helper_ownership_contract(contract: &str, errors: &mut Vec<String>) {
    validate_contract_rows(
        contract,
        Some("#1792"),
        CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
        "implemented-and-verified",
        "#1792 ownership contract",
        errors,
    );
    validate_fingerprint_ownership_rows(contract, errors);
}

fn validate_strip_ownership_contract(contract: &str, errors: &mut Vec<String>) {
    validate_helper_ownership_contract(contract, errors);
    validate_contract_rows(
        contract,
        Some("#1793"),
        CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
        "implemented-and-verified",
        "#1793 ownership contract",
        errors,
    );
}

fn validate_fingerprint_ownership_contract(contract: &str, errors: &mut Vec<String>) {
    validate_strip_ownership_contract(contract, errors);
}

fn validate_fingerprint_compare_ownership_contract(contract: &str, errors: &mut Vec<String>) {
    validate_fingerprint_ownership_contract(contract, errors);
}

fn validate_fingerprint_ownership_rows(contract: &str, errors: &mut Vec<String>) {
    let retained = CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let terminal = CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .chain(CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS)
        .collect::<BTreeSet<_>>();
    let expected = retained.union(&terminal).copied().collect::<BTreeSet<_>>();
    let mut found = BTreeSet::new();
    for row in contract.lines().filter(|line| {
        line.starts_with("| `") && (line.contains("| #1794 |") || line.contains("| #1795 |"))
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
        let disposition = if retained.contains(id) {
            "audit-required"
        } else if terminal.contains(id) {
            "implemented-and-verified"
        } else {
            errors.push(format!(
                "CPU-template helper certification found an unexpected #1794-#1795 ownership row: {id}"
            ));
            continue;
        };
        if !row.contains(&format!("| `{disposition}` |")) {
            errors.push(format!(
                "CPU-template helper certification requires {disposition} ownership row: {id}"
            ));
        }
    }
    if found != expected {
        errors.push(format!(
            "CPU-template helper certification requires the exact #1794-#1795 fingerprint ownership set: expected {expected:?}, found {found:?}"
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

fn read_contract(
    repository_root: &Path,
    path: &str,
    read_error: &str,
    validate_contract: impl FnOnce(&str, &mut Vec<String>),
    errors: &mut Vec<String>,
) {
    match std::fs::read_to_string(repository_root.join(path)) {
        Ok(contract) => validate_contract(&contract, errors),
        Err(_) => errors.push(read_error.to_string()),
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

fn finish(errors: Vec<String>) -> Result<(), ValidationErrors> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
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
        validate_contract_rows(
            &exact,
            None,
            CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "helper contract",
            &mut errors,
        );
        assert!(errors.is_empty());

        let hybrid = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_contract_rows(
            &hybrid,
            None,
            CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "helper contract",
            &mut errors,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal"))
        );

        let extra = format!(
            "{exact}\n| `tool-operation:cpu-template-helper/template/extra` | `implemented-and-verified` |"
        );
        let mut errors = Vec::new();
        validate_contract_rows(
            &extra,
            None,
            CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "helper contract",
            &mut errors,
        );
        assert!(errors.iter().any(|error| error.contains("exact helper")));
    }

    #[test]
    fn exact_strip_contract_scope_is_fail_closed() {
        let exact = CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut errors = Vec::new();
        validate_contract_rows(
            &exact,
            None,
            CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "strip contract",
            &mut errors,
        );
        assert!(errors.is_empty());

        let missing = exact.lines().skip(1).collect::<Vec<_>>().join("\n");
        let mut errors = Vec::new();
        validate_contract_rows(
            &missing,
            None,
            CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "strip contract",
            &mut errors,
        );
        assert!(errors.iter().any(|error| error.contains("exact strip")));
    }

    #[test]
    fn exact_fingerprint_dump_contract_scope_is_fail_closed() {
        let exact = CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut errors = Vec::new();
        validate_contract_rows(
            &exact,
            None,
            CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "fingerprint-dump contract",
            &mut errors,
        );
        assert!(errors.is_empty());

        let hybrid = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_contract_rows(
            &hybrid,
            None,
            CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "fingerprint-dump contract",
            &mut errors,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal"))
        );
    }

    #[test]
    fn exact_fingerprint_compare_contract_scope_is_fail_closed() {
        let exact = CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS
            .into_iter()
            .map(|id| format!("| `{id}` | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut errors = Vec::new();
        validate_contract_rows(
            &exact,
            None,
            CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "fingerprint-compare contract",
            &mut errors,
        );
        assert!(errors.is_empty());

        let missing = exact.lines().skip(1).collect::<Vec<_>>().join("\n");
        let mut errors = Vec::new();
        validate_contract_rows(
            &missing,
            None,
            CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
            "implemented-and-verified",
            "fingerprint-compare contract",
            &mut errors,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("exact fingerprint-compare"))
        );
    }
}
