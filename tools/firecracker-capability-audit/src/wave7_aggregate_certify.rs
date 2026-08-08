use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, Reference, SourceManifest, ValidationErrors,
    WAVE7_AGGREGATE_CAPABILITY_IDS, Wave7AggregateAudit, validate, validate_wave7_aggregate_audit,
};

const CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/observability-tools-specification-contract.md";
const AGGREGATE_AUDIT_PATH: &str = "compat/firecracker/v1.16.0/wave7-aggregate-audit.json";
const AGGREGATE_VALIDATOR_PATH: &str =
    "tools/firecracker-capability-audit/src/wave7_aggregate_audit_validate.rs";
const AGGREGATE_TEST_PATH: &str =
    "tools/firecracker-capability-audit/tests/wave7_aggregate_audit.rs";

/// Exact capability identities delivered by the Wave 7 parent.
pub const WAVE7_OWNED_CAPABILITY_IDS: [&str; 93] = [
    "api-operation:GET /",
    "api-operation:GET /version",
    "api-operation:GET /vm/config",
    "api-operation:PUT /actions",
    "api-path:/",
    "api-path:/actions",
    "api-path:/version",
    "api-path:/vm/config",
    "api-property:CpuConfig.cpuid_modifiers",
    "api-property:CpuConfig.msr_modifiers",
    "api-property:CpuidLeafModifier.flags",
    "api-property:CpuidLeafModifier.leaf",
    "api-property:CpuidLeafModifier.modifiers",
    "api-property:CpuidLeafModifier.subleaf",
    "api-property:CpuidRegisterModifier.bitmap",
    "api-property:CpuidRegisterModifier.register",
    "api-property:Error.fault_message",
    "api-property:FirecrackerVersion.firecracker_version",
    "api-property:InstanceActionInfo.action_type",
    "api-property:InstanceInfo.app_name",
    "api-property:InstanceInfo.id",
    "api-property:InstanceInfo.state",
    "api-property:InstanceInfo.vmm_version",
    "api-property:MsrModifier.addr",
    "api-property:MsrModifier.bitmap",
    "api-schema:CpuidLeafModifier",
    "api-schema:CpuidRegisterModifier",
    "api-schema:Error",
    "api-schema:FirecrackerVersion",
    "api-schema:FullVmConfiguration",
    "api-schema:InstanceActionInfo",
    "api-schema:InstanceInfo",
    "api-schema:MsrModifier",
    "corpus:actions-api",
    "semantic.specification:api-availability-stability-and-failure-information",
    "api-operation:PUT /logger",
    "api-path:/logger",
    "api-property:FullVmConfiguration.logger",
    "api-property:Logger.level",
    "api-property:Logger.log_path",
    "api-property:Logger.module",
    "api-property:Logger.show_level",
    "api-property:Logger.show_log_origin",
    "api-schema:Logger",
    "corpus:logger",
    "semantic.observability:logger-delivery-filtering-loss-and-redaction",
    "api-operation:PUT /metrics",
    "api-path:/metrics",
    "api-property:FullVmConfiguration.metrics",
    "api-property:Metrics.metrics_path",
    "api-property:RateLimiter.bandwidth",
    "api-property:RateLimiter.ops",
    "api-property:TokenBucket.one_time_burst",
    "api-property:TokenBucket.refill_time",
    "api-property:TokenBucket.size",
    "api-schema:Metrics",
    "api-schema:RateLimiter",
    "api-schema:TokenBucket",
    "corpus:metrics",
    "semantic.observability:metrics-schema-producers-flush-and-lifecycle",
    "corpus:tracing",
    "tool-argument:cpu-template-helper/template/dump/config",
    "tool-argument:cpu-template-helper/template/dump/output",
    "tool-argument:cpu-template-helper/template/dump/template",
    "tool-argument:cpu-template-helper/template/verify/config",
    "tool-argument:cpu-template-helper/template/verify/template",
    "tool-operation:cpu-template-helper/template/dump",
    "tool-operation:cpu-template-helper/template/verify",
    "tool-argument:cpu-template-helper/template/strip/paths",
    "tool-argument:cpu-template-helper/template/strip/suffix",
    "tool-operation:cpu-template-helper/template/strip",
    "tool-argument:cpu-template-helper/fingerprint/compare/curr",
    "tool-argument:cpu-template-helper/fingerprint/compare/filters",
    "tool-argument:cpu-template-helper/fingerprint/compare/prev",
    "tool-argument:cpu-template-helper/fingerprint/dump/config",
    "tool-argument:cpu-template-helper/fingerprint/dump/output",
    "tool-argument:cpu-template-helper/fingerprint/dump/template",
    "tool-operation:cpu-template-helper/fingerprint/compare",
    "tool-operation:cpu-template-helper/fingerprint/dump",
    "corpus:cpu-template-helper",
    "corpus:cpu-templates",
    "semantic.cpu:configuration-templates-and-feature-state",
    "corpus:getting-started",
    "corpus:rootfs-and-kernel",
    "corpus:formal-verification",
    "corpus:network-performance",
    "corpus:specification",
    "semantic.specification:performance-resource-and-telemetry-outcomes",
    "corpus:design",
    "corpus:device-api",
    "corpus:release-changelog",
    "semantic.tools:packaging-help-errors-and-applicable-operations",
    "semantic.transport:virtio-mmio-activation",
];

/// Exact architecture-impossible subset of the Wave 7 ownership set.
pub const WAVE7_PLATFORM_IMPOSSIBLE_CAPABILITY_IDS: [&str; 13] = [
    "api-property:CpuConfig.cpuid_modifiers",
    "api-property:CpuConfig.msr_modifiers",
    "api-property:CpuidLeafModifier.flags",
    "api-property:CpuidLeafModifier.leaf",
    "api-property:CpuidLeafModifier.modifiers",
    "api-property:CpuidLeafModifier.subleaf",
    "api-property:CpuidRegisterModifier.bitmap",
    "api-property:CpuidRegisterModifier.register",
    "api-property:MsrModifier.addr",
    "api-property:MsrModifier.bitmap",
    "api-schema:CpuidLeafModifier",
    "api-schema:CpuidRegisterModifier",
    "api-schema:MsrModifier",
];

/// Certify the exact terminal #1799 transition and complete Wave 7 parent set.
pub fn validate_wave7_aggregate_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    audit: &Wave7AggregateAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) =
        validate_wave7_aggregate_audit(audit, manifest, inventory, repository_root)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    validate_owned_distribution(inventory, &mut errors);
    validate_aggregate_capabilities(inventory, &mut errors);
    validate_contract(repository_root, &mut errors);
    validate_documented_commands(repository_root, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_owned_distribution(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    let owned = WAVE7_OWNED_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let impossible = WAVE7_PLATFORM_IMPOSSIBLE_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if owned.len() != 93 || impossible.len() != 13 || !impossible.is_subset(&owned) {
        errors.push("Wave 7 owned identity constants are internally inconsistent".to_string());
    }
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let mut implemented_count = 0;
    let mut impossible_count = 0;
    for id in &owned {
        let expected = if impossible.contains(id) {
            impossible_count += 1;
            Disposition::ProvenPlatformImpossible
        } else {
            implemented_count += 1;
            Disposition::ImplementedAndVerified
        };
        if capabilities
            .get(id)
            .is_none_or(|capability| capability.disposition != expected)
        {
            errors.push(format!(
                "Wave 7 owned capability is not terminal as required: {id}"
            ));
        }
    }
    if (implemented_count, impossible_count) != (80, 13) {
        errors.push(format!(
            "Wave 7 owned terminal distribution must be 80/13, found {implemented_count}/{impossible_count}"
        ));
    }
}

fn validate_aggregate_capabilities(inventory: &CapabilityInventory, errors: &mut Vec<String>) {
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let expected = [
        (
            "corpus:design",
            &[
                (AGGREGATE_AUDIT_PATH, "\"design\": ["),
                (AGGREGATE_VALIDATOR_PATH, "fn validate_design("),
            ][..],
            &[(
                AGGREGATE_TEST_PATH,
                "fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed()",
            )][..],
        ),
        (
            "corpus:device-api",
            &[
                (AGGREGATE_AUDIT_PATH, "\"device_api\": {"),
                (AGGREGATE_VALIDATOR_PATH, "fn validate_device_api("),
            ][..],
            &[(
                AGGREGATE_TEST_PATH,
                "fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed()",
            )][..],
        ),
        (
            "corpus:release-changelog",
            &[
                (AGGREGATE_AUDIT_PATH, "\"release_entries\": ["),
                (AGGREGATE_VALIDATOR_PATH, "fn validate_release("),
            ][..],
            &[(
                AGGREGATE_TEST_PATH,
                "fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed()",
            )][..],
        ),
        (
            "semantic.tools:packaging-help-errors-and-applicable-operations",
            &[
                (AGGREGATE_AUDIT_PATH, "\"tools\": ["),
                (AGGREGATE_VALIDATOR_PATH, "fn validate_tools("),
            ][..],
            &[(
                AGGREGATE_TEST_PATH,
                "fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed()",
            )][..],
        ),
        (
            "semantic.transport:virtio-mmio-activation",
            &[
                (AGGREGATE_AUDIT_PATH, "\"virtio_mmio\": {"),
                (
                    "crates/runtime/src/virtio_mmio.rs",
                    "pub struct VirtioMmioRegisterHandler",
                ),
                (AGGREGATE_VALIDATOR_PATH, "fn validate_mmio("),
            ][..],
            &[
                (
                    "crates/runtime/src/virtio_mmio.rs",
                    "fn register_handler_implements_mmio_handler_for_dispatcher()",
                ),
                (
                    AGGREGATE_TEST_PATH,
                    "fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed()",
                ),
            ][..],
        ),
    ];
    for (id, expected_implementation, expected_validation) in expected {
        let Some(capability) = capabilities.get(id) else {
            errors.push(format!("Wave 7 aggregate capability is missing: {id}"));
            continue;
        };
        if capability.disposition != Disposition::ImplementedAndVerified
            || capability.delivery_issue.is_some()
            || capability.exclusion.is_some()
        {
            errors.push(format!("Wave 7 aggregate capability is not terminal: {id}"));
        }
        if local_reference_pairs(&capability.implementation) != expected_implementation {
            errors.push(format!(
                "Wave 7 aggregate capability implementation evidence drifted: {id}"
            ));
        }
        if local_reference_pairs(&capability.validation) != expected_validation {
            errors.push(format!(
                "Wave 7 aggregate capability validation evidence drifted: {id}"
            ));
        }
    }
    for capability in &inventory.capabilities {
        if !WAVE7_AGGREGATE_CAPABILITY_IDS.contains(&capability.id.as_str())
            && capability.delivery_issue.as_deref() == Some("#1799")
        {
            errors.push(format!(
                "Wave 7 aggregate certification found unrelated #1799 ownership: {}",
                capability.id
            ));
        }
    }
}

fn validate_contract(repository_root: &Path, errors: &mut Vec<String>) {
    let Ok(contract) = std::fs::read_to_string(repository_root.join(CONTRACT_PATH)) else {
        errors.push("Wave 7 aggregate contract is unreadable".to_string());
        return;
    };
    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1799 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|value| value.split_once("` |"))
        else {
            errors.push("Wave 7 aggregate contract has a malformed #1799 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "Wave 7 aggregate contract duplicates #1799 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "Wave 7 aggregate contract requires a terminal row: {id}"
            ));
        }
    }
    let expected = WAVE7_AGGREGATE_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "Wave 7 aggregate contract requires the exact #1799 set: expected {expected:?}, found {ids:?}"
        ));
    }
    let normalized = contract.split_whitespace().collect::<Vec<_>>().join(" ");
    for token in [
        "## Wave 7 aggregate certification",
        "376 implemented",
        "nine audit-required",
        "three missing-platform-feasible",
        "30 proven-platform-impossible",
        "wave7-aggregate-audit.json",
        "validate --wave7-final",
        "#1351",
        "#1373",
        "#1378",
        "Wave 8",
    ] {
        if !normalized.contains(token) {
            errors.push(format!(
                "Wave 7 aggregate contract omits required token: {token}"
            ));
        }
    }
}

fn validate_documented_commands(repository_root: &Path, errors: &mut Vec<String>) {
    let command =
        "cargo run -p bangbang-firecracker-capability-audit --locked -- validate --wave7-final";
    for path in [".github/workflows/ci.yml", "docs/testing.md"] {
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(contents)
                if contents
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(command) => {}
            Ok(_) => errors.push(format!("Wave 7 final command is missing from {path}")),
            Err(_) => errors.push(format!("Wave 7 command owner is unreadable: {path}")),
        }
    }
}

fn local_reference_pairs(references: &[Reference]) -> Vec<(&str, &str)> {
    references
        .iter()
        .filter_map(|reference| match reference {
            Reference::Local {
                path,
                anchor: Some(anchor),
            } => Some((path.as_str(), anchor.as_str())),
            Reference::Local { anchor: None, .. } => None,
            Reference::Github { .. } | Reference::Authoritative { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_distribution_is_exact_and_unique() {
        let owned = WAVE7_OWNED_CAPABILITY_IDS
            .into_iter()
            .collect::<BTreeSet<_>>();
        let impossible = WAVE7_PLATFORM_IMPOSSIBLE_CAPABILITY_IDS
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(owned.len(), 93);
        assert_eq!(impossible.len(), 13);
        assert!(impossible.is_subset(&owned));
        assert_eq!(owned.difference(&impossible).count(), 80);
    }
}
