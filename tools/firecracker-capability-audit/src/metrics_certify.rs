use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, Disposition, MetricsProducerDisposition, MetricsProducerOwner,
    MetricsSchemaAuthority, SourceManifest, ValidationErrors, validate, validate_metrics_schema,
};

/// Exact capability scope certified by the Firecracker metrics schema gate.
pub const METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS: [&str; 12] = [
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
];

/// Aggregate metrics capabilities intentionally retained for #1790.
pub const RETAINED_METRICS_AGGREGATE_CAPABILITY_IDS: [&str; 2] = [
    "corpus:metrics",
    "semantic.observability:metrics-schema-producers-flush-and-lifecycle",
];

const EXPECTED_SCHEMA_RUNTIME_IMPLEMENTED_PROFILES: usize = 1;
const EXPECTED_PROCESS_LIFECYCLE_IMPLEMENTED_PROFILES: usize = 2;
const EXPECTED_DEVICE_IMPLEMENTED_PROFILES: usize = 10;
const EXPECTED_DEVICE_PLANNED_PROFILES: usize = 10;
const EXPECTED_DEVICE_PLATFORM_ZERO_PROFILES: usize = 5;
const OBSERVABILITY_CONTRACT_PATH: &str =
    "compat/firecracker/v1.16.0/observability-tools-specification-contract.md";

/// Validate the terminal metrics API/schema slice without requiring later
/// device producer or aggregate owners to have reached terminal dispositions.
pub fn validate_metrics_schema_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    metrics_authority: &MetricsSchemaAuthority,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) =
        validate(manifest, inventory, repository_root, AuditMode::Delivery)
    {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_metrics_schema(
        metrics_authority,
        manifest,
        repository_root,
        AuditMode::Delivery,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::ImplementedAndVerified => {}
            Some(_) => errors.push(format!(
                "metrics schema certification requires implemented-and-verified capability: {id}"
            )),
            None => errors.push(format!(
                "metrics schema certification capability is missing: {id}"
            )),
        }
    }
    for id in RETAINED_METRICS_AGGREGATE_CAPABILITY_IDS {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::AuditRequired => {}
            Some(_) => errors.push(format!(
                "metrics schema certification requires retained audit-required capability: {id}"
            )),
            None => errors.push(format!(
                "retained metrics aggregate capability is missing: {id}"
            )),
        }
    }
    match std::fs::read_to_string(repository_root.join(OBSERVABILITY_CONTRACT_PATH)) {
        Ok(contract) => validate_owned_contract(&contract, &mut errors),
        Err(_) => errors.push(
            "metrics schema certification cannot read the Wave 7 ownership contract".to_string(),
        ),
    }

    let mut schema_runtime_implemented = 0;
    let mut process_lifecycle_implemented = 0;
    let mut device_implemented = 0;
    let mut device_planned = 0;
    let mut device_platform_zero = 0;
    for profile in &metrics_authority.policy_profiles {
        match (profile.producer_owner, profile.producer_disposition) {
            (
                MetricsProducerOwner::SchemaRuntime,
                MetricsProducerDisposition::Implemented,
            ) if profile.delivery_issue.is_none() => schema_runtime_implemented += 1,
            (
                MetricsProducerOwner::ProcessLifecycle,
                MetricsProducerDisposition::Implemented,
            ) if profile.delivery_issue.is_none() => {
                process_lifecycle_implemented += 1;
            }
            (MetricsProducerOwner::Device, MetricsProducerDisposition::Planned)
                if profile.delivery_issue.as_deref() == Some("#1789") =>
            {
                device_planned += 1;
            }
            (MetricsProducerOwner::Device, MetricsProducerDisposition::PlatformZero)
                if profile.delivery_issue.as_deref() == Some("#1789") =>
            {
                device_platform_zero += 1;
            }
            (MetricsProducerOwner::Device, MetricsProducerDisposition::Implemented)
                if profile.delivery_issue.is_none() =>
            {
                device_implemented += 1;
            }
            _ => errors.push(format!(
                "metrics schema certification rejects producer policy outside the exact completed-process/device transition: {}",
                profile.id
            )),
        }
    }

    for (label, actual, expected) in [
        (
            "implemented schema-runtime",
            schema_runtime_implemented,
            EXPECTED_SCHEMA_RUNTIME_IMPLEMENTED_PROFILES,
        ),
        (
            "implemented process-lifecycle",
            process_lifecycle_implemented,
            EXPECTED_PROCESS_LIFECYCLE_IMPLEMENTED_PROFILES,
        ),
    ] {
        if actual != expected {
            errors.push(format!(
                "metrics schema certification requires exactly {expected} {label} policy profiles, found {actual}"
            ));
        }
    }

    let historical_device_handoff = device_implemented == 0
        && device_planned == EXPECTED_DEVICE_PLANNED_PROFILES
        && device_platform_zero == EXPECTED_DEVICE_PLATFORM_ZERO_PROFILES;
    let terminal_device_handoff = device_implemented == EXPECTED_DEVICE_IMPLEMENTED_PROFILES
        && device_planned == 0
        && device_platform_zero == 0;
    if !historical_device_handoff && !terminal_device_handoff {
        errors.push(format!(
            "metrics schema certification requires either the exact historical #1789 device handoff or the exact terminal device projection: implemented={device_implemented}; planned={device_planned}; platform-zero={device_platform_zero}"
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_owned_contract(contract: &str, errors: &mut Vec<String>) {
    let mut ids = BTreeSet::new();
    for row in contract
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("| #1787 |"))
    {
        let Some((id, _)) = row
            .strip_prefix("| `")
            .and_then(|row| row.split_once("` |"))
        else {
            errors.push("metrics schema certification found a malformed #1787 row".to_string());
            continue;
        };
        if !ids.insert(id) {
            errors.push(format!(
                "metrics schema certification found a duplicate #1787 row: {id}"
            ));
        }
        if !row.contains("| `implemented-and-verified` |") {
            errors.push(format!(
                "metrics schema certification requires terminal #1787 contract row: {id}"
            ));
        }
    }

    let expected = METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if ids != expected {
        errors.push(format!(
            "metrics schema certification requires the exact #1787 contract capability set: expected {expected:?}, found {ids:?}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract_rows(ids: impl IntoIterator<Item = &'static str>) -> String {
        ids.into_iter()
            .map(|id| format!("| `{id}` | #1787 | `implemented-and-verified` |"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn exact_owned_contract_scope_is_fail_closed() {
        let exact = contract_rows(METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS);
        let mut errors = Vec::new();
        validate_owned_contract(&exact, &mut errors);
        assert!(errors.is_empty());

        let extra = format!("{exact}\n| `api-schema:Extra` | #1787 | `implemented-and-verified` |");
        let mut errors = Vec::new();
        validate_owned_contract(&extra, &mut errors);
        assert!(errors.iter().any(|error| error.contains("exact #1787")));

        let nonterminal = exact.replacen("`implemented-and-verified`", "`audit-required`", 1);
        let mut errors = Vec::new();
        validate_owned_contract(&nonterminal, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requires terminal #1787"))
        );
    }
}
