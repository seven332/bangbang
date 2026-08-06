use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    AuditMode, CapabilityInventory, MetricsDeviceProducerAudit, MetricsDeviceProducerDisposition,
    MetricsPolicyProfile, MetricsProcessProducerAudit, MetricsProducerDisposition,
    MetricsProducerOwner, MetricsSchemaAuthority, Reference, SourceManifest, ValidationErrors,
    validate_metrics_device_producers, validate_metrics_process_compatibility,
};

/// Exact terminal shared policy profiles certified by the device gate.
pub const TERMINAL_DEVICE_POLICY_PROFILE_IDS: [&str; 10] = [
    "bytes-none-device-implemented",
    "bytes-sum-across-configured-devices-device-implemented",
    "count-none-device-implemented",
    "count-sum-across-configured-devices-device-implemented",
    "microseconds-maximum-device-implemented",
    "microseconds-minimum-device-implemented",
    "microseconds-none-device-implemented",
    "microseconds-sum-across-configured-devices-device-implemented",
    "microseconds-sum-device-implemented",
    "microseconds-zero-in-configured-device-aggregate-device-implemented",
];

const EXPECTED_DEVICE_FIELDS: usize = 231;
const EXPECTED_IMPLEMENTED_FIELDS: usize = 212;
const EXPECTED_SOURCE_NEUTRAL_FIELDS: usize = 2;
const EXPECTED_PLATFORM_ZERO_FIELDS: usize = 17;

const IMPLEMENTATION_EVIDENCE: [(&str, &str); 4] = [
    (
        "crates/runtime/src/metrics.rs",
        "pub(crate) fn flush_with_diagnostics_and_devices",
    ),
    (
        "crates/runtime/src/metrics/firecracker.rs",
        "pub(super) fn build_metrics_line",
    ),
    (
        "tools/firecracker-capability-audit/src/metrics_device_validate.rs",
        "pub fn validate_metrics_device_producers",
    ),
    (
        "tools/firecracker-capability-audit/src/metrics_device_certify.rs",
        "pub fn validate_metrics_device_compatibility",
    ),
];

const VALIDATION_EVIDENCE: [(&str, &str); 5] = [
    (
        "crates/runtime/src/metrics.rs",
        "fn complete_device_inventory_replays_ambiguous_write_and_preserves_idle_shape",
    ),
    (
        "crates/runtime/src/metrics/firecracker.rs",
        "fn maximum_configured_recipe_has_exact_sorted_exclusive_dynamic_roots",
    ),
    (
        "crates/bangbang/tests/executable_hvf_e2e.rs",
        "fn assert_architecture_retained_platform_zero_metrics",
    ),
    (
        "crates/launcher/tests/production_bundle_e2e.rs",
        "fn wait_for_canonical_output_metrics_lines",
    ),
    (
        "tools/firecracker-capability-audit/tests/checked_inventory.rs",
        "fn checked_metrics_device_compatibility_is_terminal_and_fail_closed",
    ),
];

/// Validate the exact terminal device-producer scope while requiring #1790's
/// combined publication and corpus capabilities to form one coherent state.
pub fn validate_metrics_device_compatibility(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    metrics_authority: &MetricsSchemaAuthority,
    process_audit: &MetricsProcessProducerAudit,
    device_audit: &MetricsDeviceProducerAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    if let Err(validation_errors) = validate_metrics_process_compatibility(
        manifest,
        inventory,
        metrics_authority,
        process_audit,
        repository_root,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }
    if let Err(validation_errors) = validate_metrics_device_producers(
        device_audit,
        metrics_authority,
        repository_root,
        AuditMode::Final,
    ) {
        errors.extend(validation_errors.messages().iter().cloned());
    }

    validate_terminal_profiles(metrics_authority, repository_root, &mut errors);
    validate_terminal_census(device_audit, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_terminal_profiles(
    authority: &MetricsSchemaAuthority,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let profiles = authority
        .policy_profiles
        .iter()
        .filter(|profile| profile.producer_owner == MetricsProducerOwner::Device)
        .map(|profile| (profile.id.as_str(), profile))
        .collect::<BTreeMap<_, _>>();
    let actual_ids = profiles.keys().copied().collect::<BTreeSet<_>>();
    let expected_ids = TERMINAL_DEVICE_POLICY_PROFILE_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if actual_ids != expected_ids {
        errors.push(format!(
            "metrics device certification requires the exact terminal device policy profile set: expected {expected_ids:?}, found {actual_ids:?}"
        ));
    }

    let expected_implementation = local_references(&IMPLEMENTATION_EVIDENCE);
    let expected_validation = local_references(&VALIDATION_EVIDENCE);
    for id in TERMINAL_DEVICE_POLICY_PROFILE_IDS {
        let Some(profile) = profiles.get(id).copied() else {
            continue;
        };
        validate_terminal_profile(
            profile,
            &expected_implementation,
            &expected_validation,
            repository_root,
            errors,
        );
    }
}

fn validate_terminal_profile(
    profile: &MetricsPolicyProfile,
    expected_implementation: &[Reference],
    expected_validation: &[Reference],
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    if profile.producer_disposition != MetricsProducerDisposition::Implemented
        || profile.delivery_issue.is_some()
    {
        errors.push(format!(
            "metrics device certification requires an implemented terminal profile without a delivery issue: {}",
            profile.id
        ));
    }
    if profile.implementation != expected_implementation
        || profile.validation != expected_validation
    {
        errors.push(format!(
            "metrics device certification requires exact common evidence: {}",
            profile.id
        ));
    }
    for reference in profile
        .implementation
        .iter()
        .chain(profile.validation.iter())
    {
        validate_local_anchor(reference, repository_root, &profile.id, errors);
    }
}

fn validate_local_anchor(
    reference: &Reference,
    repository_root: &Path,
    profile_id: &str,
    errors: &mut Vec<String>,
) {
    let Reference::Local {
        path,
        anchor: Some(anchor),
    } = reference
    else {
        errors.push(format!(
            "metrics device certification evidence must be an anchored local reference: {profile_id}"
        ));
        return;
    };
    match std::fs::read_to_string(repository_root.join(path)) {
        Ok(source) if source.contains(anchor) => {}
        Ok(_) => errors.push(format!(
            "metrics device certification evidence anchor does not resolve: {profile_id}: {path}: {anchor}"
        )),
        Err(_) => errors.push(format!(
            "metrics device certification evidence path is unreadable: {profile_id}: {path}"
        )),
    }
}

fn validate_terminal_census(audit: &MetricsDeviceProducerAudit, errors: &mut Vec<String>) {
    let mut implemented = 0;
    let mut source_neutral = 0;
    let mut platform_zero = 0;
    let mut nonterminal = 0;
    for record in &audit.records {
        match record.disposition {
            MetricsDeviceProducerDisposition::Implemented => implemented += 1,
            MetricsDeviceProducerDisposition::SourceNeutral => source_neutral += 1,
            MetricsDeviceProducerDisposition::PlatformZero => platform_zero += 1,
            MetricsDeviceProducerDisposition::Planned
            | MetricsDeviceProducerDisposition::ProvisionalPlatformZero => nonterminal += 1,
        }
    }
    if audit.records.len() != EXPECTED_DEVICE_FIELDS
        || implemented != EXPECTED_IMPLEMENTED_FIELDS
        || source_neutral != EXPECTED_SOURCE_NEUTRAL_FIELDS
        || platform_zero != EXPECTED_PLATFORM_ZERO_FIELDS
        || nonterminal != 0
    {
        errors.push(format!(
            "metrics device certification requires the exact terminal 231-record census: total={}; implemented={implemented}; source-neutral={source_neutral}; platform-zero={platform_zero}; nonterminal={nonterminal}",
            audit.records.len()
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
