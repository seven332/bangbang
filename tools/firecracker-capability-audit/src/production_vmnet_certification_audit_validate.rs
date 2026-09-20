use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::inventory_phase::{
    InventoryPhase, NETWORK_VMNET_FEASIBLE_IDS, classify_inventory_phase, disposition_counts,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, ProductionVmnetCapabilityClaim, ProductionVmnetCertificationAudit,
    ProductionVmnetCertificationResult, Reference, ValidationErrors,
    production_vmnet_certification_audit_json, production_vmnet_certification_result_json,
};

/// Current checked terminal production-vmnet audit schema.
pub const PRODUCTION_VMNET_CERTIFICATION_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative terminal production-vmnet audit path.
pub const PRODUCTION_VMNET_CERTIFICATION_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/production-vmnet-certification-audit.json";
/// Repository-relative unchanged generated result path.
pub const PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH: &str =
    "compat/firecracker/v1.16.0/production-vmnet-certification-evidence.json";
/// Exact capabilities promoted by #1948.
pub const PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS: [&str; 2] = NETWORK_VMNET_FEASIBLE_IDS;
/// Exact mandatory schema-v2 case set, in canonical order without optional rows.
pub const PRODUCTION_VMNET_MANDATORY_CASES: [&str; 27] = [
    "authority-split",
    "networkless-denial",
    "missing-policy-denial",
    "mismatched-policy-denial",
    "bridge-allowlist-denial",
    "active-interface-count-exhaustion",
    "mmds-only-no-consumption",
    "shared-connectivity",
    "startup-interface-remove",
    "runtime-hotplug-remove",
    "normal-teardown",
    "partial-start-cleanup",
    "pre-ready-cancellation",
    "post-ready-cancellation",
    "provider-startup-death",
    "broker-runtime-death",
    "owner-runtime-death",
    "launcher-first-death",
    "worker-first-death",
    "provider-sigkill-reclamation",
    "broker-sigkill-reclamation",
    "owner-sigkill-reclamation",
    "launcher-sigkill-reclamation",
    "worker-sigkill-reclamation",
    "clean-repeat",
    "capture-restore-fresh-ownership",
    "concurrent-noninterchangeability",
];
/// Exact environment-dependent schema-v2 case set.
pub const PRODUCTION_VMNET_OPTIONAL_CASES: [&str; 4] = [
    "host-connectivity",
    "bridged-connectivity",
    "not-authorized",
    "sharing-service-busy",
];

const RESULT_SCHEMA_VERSION: u32 = 2;
const RESULT_SHA256: &str = "8f3dd5c3669f281e31536f617bc50648e8cf5d40cb38d1b661fec80b4fa87dca";
const SOURCE_COMMIT: &str = "b4fe638a7fbbedbb118516285b4d21209c344f4e";
const SOURCE_TREE: &str = "2ccd3b8ae6f3742be9e102fc2428d2edeb0a7a04";
const RESULT_COMMENT: &str =
    "https://github.com/seven332/bangbang/issues/1948#issuecomment-5747681356";
const CHALLENGE_COMMENT: &str =
    "https://github.com/seven332/bangbang/issues/1948#issuecomment-5747683837";
const UNRELATED_INVENTORY_SHA256: &str =
    "eb54c4cf9cadaaf7a2ddb7a86b5642a93af1cc913bdd3edaedff5cc53472dc00";

const EXPECTED_CASES: [(&str, &str); 31] = [
    ("authority-split", "passed"),
    ("networkless-denial", "passed"),
    ("missing-policy-denial", "passed"),
    ("mismatched-policy-denial", "passed"),
    ("bridge-allowlist-denial", "passed"),
    ("active-interface-count-exhaustion", "passed"),
    ("mmds-only-no-consumption", "passed"),
    ("shared-connectivity", "passed"),
    ("host-connectivity", "environment-gated"),
    ("bridged-connectivity", "environment-gated"),
    ("not-authorized", "environment-gated"),
    ("sharing-service-busy", "environment-gated"),
    ("startup-interface-remove", "passed"),
    ("runtime-hotplug-remove", "passed"),
    ("normal-teardown", "passed"),
    ("partial-start-cleanup", "passed"),
    ("pre-ready-cancellation", "passed"),
    ("post-ready-cancellation", "passed"),
    ("provider-startup-death", "passed"),
    ("broker-runtime-death", "passed"),
    ("owner-runtime-death", "passed"),
    ("launcher-first-death", "passed"),
    ("worker-first-death", "passed"),
    ("provider-sigkill-reclamation", "passed"),
    ("broker-sigkill-reclamation", "passed"),
    ("owner-sigkill-reclamation", "passed"),
    ("launcher-sigkill-reclamation", "passed"),
    ("worker-sigkill-reclamation", "passed"),
    ("clean-repeat", "passed"),
    ("capture-restore-fresh-ownership", "passed"),
    ("concurrent-noninterchangeability", "passed"),
];

/// Validate the complete checked terminal no-Apple production-vmnet authority.
pub fn validate_production_vmnet_certification_audit(
    audit: &ProductionVmnetCertificationAudit,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header(audit, &mut errors);
    let result = validate_evidence(audit, repository_root, &mut errors);
    validate_claims(audit, inventory, repository_root, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_canonical_audit(audit, repository_root, &mut errors);
    if let Some(result) = result {
        validate_result(&result, audit, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

/// Validate the fixed public fields of one schema-v2 production-vmnet result.
pub fn validate_production_vmnet_certification_result(
    result: &ProductionVmnetCertificationResult,
    audit: &ProductionVmnetCertificationAudit,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_result(result, audit, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header(audit: &ProductionVmnetCertificationAudit, errors: &mut Vec<String>) {
    if audit.schema_version != PRODUCTION_VMNET_CERTIFICATION_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "production vmnet certification schema_version must be {PRODUCTION_VMNET_CERTIFICATION_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("production vmnet certification baseline is not pinned".to_string());
    }
    if audit.parent_issue != "#1378" || audit.delivery_issue != "#1948" {
        errors.push("production vmnet certification ownership must be #1378/#1948".to_string());
    }
}

fn validate_evidence(
    audit: &ProductionVmnetCertificationAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) -> Option<ProductionVmnetCertificationResult> {
    let evidence = &audit.evidence;
    if evidence.path != PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH
        || evidence.sha256 != RESULT_SHA256
        || evidence.repetitions != 2
        || !evidence.byte_identical
        || evidence.source.commit != SOURCE_COMMIT
        || evidence.source.tree != SOURCE_TREE
        || (
            evidence.platform.architecture.as_str(),
            evidence.platform.hvf.as_str(),
            evidence.platform.macos.as_str(),
            evidence.platform.sdk.as_str(),
        ) != ("arm64", "supported", "26.6.2", "26.5")
        || evidence.result_comment
            != (Reference::Github {
                url: RESULT_COMMENT.to_string(),
            })
        || evidence.challenge
            != (Reference::Github {
                url: CHALLENGE_COMMENT.to_string(),
            })
    {
        errors.push("production vmnet certification evidence identity drifted".to_string());
    }

    let tracked = tracked_repository_files(repository_root, errors);
    for (label, reference) in [
        ("production vmnet result comment", &evidence.result_comment),
        ("production vmnet challenge", &evidence.challenge),
    ] {
        validate_reference(reference, repository_root, &tracked, label, errors);
    }

    let bytes = match std::fs::read(repository_root.join(&evidence.path)) {
        Ok(bytes) => bytes,
        Err(_) => {
            errors.push("production vmnet certification evidence is unreadable".to_string());
            return None;
        }
    };
    if encode_sha256(Sha256::digest(&bytes)) != RESULT_SHA256 {
        errors.push("production vmnet certification evidence digest drifted".to_string());
    }
    let result = match serde_json::from_slice::<ProductionVmnetCertificationResult>(&bytes) {
        Ok(result) => result,
        Err(_) => {
            errors.push("production vmnet certification evidence does not parse".to_string());
            return None;
        }
    };
    match production_vmnet_certification_result_json(&result) {
        Ok(canonical) if canonical == bytes => {}
        Ok(_) => {
            errors.push("production vmnet certification evidence is not canonical JSON".to_string())
        }
        Err(_) => {
            errors.push("production vmnet certification evidence is not serializable".to_string())
        }
    }
    Some(result)
}

fn validate_result(
    result: &ProductionVmnetCertificationResult,
    audit: &ProductionVmnetCertificationAudit,
    errors: &mut Vec<String>,
) {
    if result.schema_version != RESULT_SCHEMA_VERSION
        || result.source != audit.evidence.source
        || result.platform != audit.evidence.platform
        || (
            result.authority.controller.as_str(),
            result.authority.kind.as_str(),
            result.authority.outer.as_str(),
            result.authority.owner.as_str(),
            result.authority.provider.as_str(),
            result.authority.route.as_str(),
        ) != (
            "ordinary",
            "elevated-provider",
            "ordinary",
            "irreversibly-ordinary",
            "bounded-root",
            "remote-only",
        )
        || !result.entitlements.outer_empty
        || !result.entitlements.provider_empty
        || !result.entitlements.worker_app_sandbox_hvf
        || result.entitlements.worker_vmnet
        || result.cleanup != "complete"
        || result.verdict != "passed"
    {
        errors.push("production vmnet certification result boundary drifted".to_string());
    }
    let cases = result
        .cases
        .iter()
        .map(|case| (case.name.as_str(), case.outcome.as_str()))
        .collect::<Vec<_>>();
    if cases != EXPECTED_CASES {
        errors
            .push("production vmnet certification requires the exact 31 case outcomes".to_string());
    }
}

fn validate_claims(
    audit: &ProductionVmnetCertificationAudit,
    inventory: &CapabilityInventory,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    if audit.optional_non_dependencies != PRODUCTION_VMNET_OPTIONAL_CASES {
        errors.push(
            "production vmnet certification requires the exact optional nondependencies"
                .to_string(),
        );
    }
    let expected = expected_claims();
    if audit.claims != expected {
        errors.push("production vmnet certification claim mapping drifted".to_string());
    }

    let tracked = tracked_repository_files(repository_root, errors);
    for (claim_index, claim) in audit.claims.iter().enumerate() {
        let required = claim
            .required_cases
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mandatory = PRODUCTION_VMNET_MANDATORY_CASES
            .into_iter()
            .collect::<BTreeSet<_>>();
        let optional = PRODUCTION_VMNET_OPTIONAL_CASES
            .into_iter()
            .collect::<BTreeSet<_>>();
        if required != mandatory || !required.is_disjoint(&optional) {
            errors.push(format!(
                "production vmnet certification claim[{claim_index}] must use every mandatory case and no optional case"
            ));
        }
        for (kind, references) in [
            ("implementation", &claim.implementation),
            ("validation", &claim.validation),
        ] {
            for (reference_index, reference) in references.iter().enumerate() {
                let label = format!(
                    "production vmnet certification claim[{claim_index}] {kind}[{reference_index}]"
                );
                validate_reference(reference, repository_root, &tracked, &label, errors);
                validate_local_anchor(reference, repository_root, &label, errors);
            }
        }
        match inventory
            .capabilities
            .iter()
            .find(|capability| capability.id == claim.capability_id)
        {
            Some(capability) if capability_is_exact_terminal(capability, claim) => {}
            Some(_) => errors.push(format!(
                "production vmnet capability is not exact terminal evidence: {}",
                claim.capability_id
            )),
            None => errors.push(format!(
                "production vmnet capability is missing: {}",
                claim.capability_id
            )),
        }
    }
}

fn expected_claims() -> Vec<ProductionVmnetCapabilityClaim> {
    let mandatory = || {
        PRODUCTION_VMNET_MANDATORY_CASES
            .into_iter()
            .map(str::to_string)
            .collect()
    };
    vec![
        ProductionVmnetCapabilityClaim {
            capability_id: "corpus:network-setup".to_string(),
            required_cases: mandatory(),
            implementation: local_references(&[
                (
                    "compat/firecracker/v1.16.0/network-mmds-contract.md",
                    "## Terminal production vmnet certification",
                ),
                (
                    "crates/launcher/src/macos/vmnet_topology.rs",
                    "pub(crate) fn child_bootstrap()",
                ),
                (
                    "crates/vmnet-provider/src/macos/topology.rs",
                    "pub fn run_public_bootstrap(",
                ),
            ]),
            validation: local_references(&[
                (
                    "compat/firecracker/v1.16.0/production-vmnet-certification-contract.md",
                    "## Terminal production vmnet outcome",
                ),
                (
                    PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH,
                    "shared-connectivity",
                ),
                (
                    "scripts/tests/test_production_vmnet_elevated.py",
                    "test_elevated_matrix_runs_exact_rows_and_publishes_canonical_result",
                ),
                (
                    "tools/firecracker-capability-audit/tests/production_vmnet_certification_audit.rs",
                    "fn checked_production_vmnet_certification_is_canonical_and_fail_closed()",
                ),
            ]),
        },
        ProductionVmnetCapabilityClaim {
            capability_id: "semantic.network:virtio-net-vmnet-policy-and-connectivity".to_string(),
            required_cases: mandatory(),
            implementation: local_references(&[
                (
                    "crates/bangbang/src/host_network/remote_vmnet.rs",
                    "pub(crate) struct RemoteVmnetProviderSource",
                ),
                (
                    "crates/runtime/src/snapshot_network_restore_v2_11.rs",
                    "pub struct PreparedSnapshotV2NetworkRestoreTopology",
                ),
                (
                    "crates/session/src/vmnet_provider/mod.rs",
                    "Closed, bounded vmnet-provider protocol",
                ),
                (
                    "crates/vmnet-provider/src/macos/topology.rs",
                    "pub fn run_public_bootstrap(",
                ),
            ]),
            validation: local_references(&[
                (
                    "compat/firecracker/v1.16.0/production-vmnet-certification-contract.md",
                    "## Mandatory claim mapping",
                ),
                (
                    PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH,
                    "concurrent-noninterchangeability",
                ),
                (
                    "scripts/tests/test_production_vmnet_elevated.py",
                    "test_restore_orchestration_requires_fresh_owner_and_exact_barrier",
                ),
                (
                    "tools/firecracker-capability-audit/tests/production_vmnet_certification_audit.rs",
                    "fn checked_production_vmnet_certification_is_canonical_and_fail_closed()",
                ),
            ]),
        },
    ]
}

fn local_references(values: &[(&str, &str)]) -> Vec<Reference> {
    values
        .iter()
        .map(|(path, anchor)| Reference::Local {
            path: (*path).to_string(),
            anchor: Some((*anchor).to_string()),
        })
        .collect()
}

fn validate_local_anchor(
    reference: &Reference,
    repository_root: &Path,
    label: &str,
    errors: &mut Vec<String>,
) {
    let Reference::Local {
        path,
        anchor: Some(anchor),
    } = reference
    else {
        return;
    };
    match std::fs::read(repository_root.join(path)) {
        Ok(bytes)
            if bytes
                .windows(anchor.len())
                .any(|window| window == anchor.as_bytes()) => {}
        Ok(_) => errors.push(format!("local reference anchor is absent: {label}")),
        Err(_) => {}
    }
}

fn capability_is_exact_terminal(
    capability: &Capability,
    claim: &ProductionVmnetCapabilityClaim,
) -> bool {
    let source_refs = if capability.id == "corpus:network-setup" {
        &["corpus:network-setup"][..]
    } else {
        &[
            "corpus:network-performance",
            "corpus:network-setup",
            "corpus:patch-network-interface",
        ][..]
    };
    capability.family == "network-and-mmds"
        && capability
            .source_refs
            .iter()
            .map(String::as_str)
            .eq(source_refs.iter().copied())
        && capability.disposition == Disposition::ImplementedAndVerified
        && capability.implementation == claim.implementation
        && capability.validation == claim.validation
        && capability.delivery_issue.is_none()
        && capability.exclusion.is_none()
}

fn validate_inventory_transition(
    audit: &ProductionVmnetCertificationAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if (
        audit.previous_counts.implemented_and_verified,
        audit.previous_counts.audit_required,
        audit.previous_counts.missing_platform_feasible,
        audit.previous_counts.proven_platform_impossible,
    ) != (383, 0, 2, 33)
    {
        errors.push("production vmnet previous counts must be exactly 383/0/2/33".to_string());
    }
    if (
        audit.target_counts.implemented_and_verified,
        audit.target_counts.audit_required,
        audit.target_counts.missing_platform_feasible,
        audit.target_counts.proven_platform_impossible,
    ) != (385, 0, 0, 33)
    {
        errors.push("production vmnet target counts must be exactly 385/0/0/33".to_string());
    }
    if disposition_counts(inventory) != (385, 0, 0, 33)
        || classify_inventory_phase(inventory) != Ok(InventoryPhase::ProductionVmnet)
    {
        errors.push("production vmnet live inventory must be exact 385/0/0/33".to_string());
    }

    if audit.transitions.len() != PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS.len() {
        errors.push("production vmnet certification requires exactly two transitions".to_string());
    }
    for (index, (transition, expected_id)) in audit
        .transitions
        .iter()
        .zip(PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS)
        .enumerate()
    {
        if transition.capability_id != expected_id
            || transition.previous_disposition != Disposition::MissingPlatformFeasible
            || transition.target_disposition != Disposition::ImplementedAndVerified
        {
            errors.push(format!("production vmnet transition[{index}] drifted"));
        }
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push("production vmnet unrelated-inventory digest authority drifted".to_string());
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "production vmnet unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push("production vmnet unrelated inventory is not serializable".to_string()),
    }
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let excluded = PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let unrelated = inventory
        .capabilities
        .iter()
        .filter(|capability| !excluded.contains(capability.id.as_str()))
        .collect::<Vec<&Capability>>();
    serde_json::to_vec(&unrelated).map(|bytes| encode_sha256(Sha256::digest(bytes)))
}

fn encode_sha256(digest: impl IntoIterator<Item = u8>) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(if byte >> 4 < 10 {
            b'0' + (byte >> 4)
        } else {
            b'a' + (byte >> 4) - 10
        }));
        encoded.push(char::from(if byte & 0x0f < 10 {
            b'0' + (byte & 0x0f)
        } else {
            b'a' + (byte & 0x0f) - 10
        }));
    }
    encoded
}

fn validate_canonical_audit(
    audit: &ProductionVmnetCertificationAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let expected = match production_vmnet_certification_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(_) => {
            errors.push("production vmnet certification audit is not serializable".to_string());
            return;
        }
    };
    match std::fs::read(repository_root.join(PRODUCTION_VMNET_CERTIFICATION_AUDIT_PATH)) {
        Ok(actual) if actual == expected => {}
        Ok(_) => {
            errors.push("production vmnet certification audit is not canonical JSON".to_string())
        }
        Err(_) => errors.push("production vmnet certification audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_case_partition_is_closed() {
        let all = EXPECTED_CASES
            .into_iter()
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>();
        let partition = PRODUCTION_VMNET_MANDATORY_CASES
            .into_iter()
            .chain(PRODUCTION_VMNET_OPTIONAL_CASES)
            .collect::<BTreeSet<_>>();
        assert_eq!(all, partition);
        assert_eq!(all.len(), 31);
    }
}
