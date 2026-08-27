use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::inventory_phase::{
    InventoryPhase, NETWORK_VMNET_FEASIBLE_IDS, classify_inventory_phase, disposition_counts,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, Reference, SourceManifest, ValidationErrors, VmnetFeasibilityAudit,
    VmnetFeasibilityEvidenceId, VmnetFeasibilityNonclaim, vmnet_feasibility_audit_json,
};

/// Current checked entitlement-free vmnet feasibility schema.
pub const VMNET_FEASIBILITY_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative checked entitlement-free vmnet feasibility path.
pub const VMNET_FEASIBILITY_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/vmnet-feasibility-audit.json";
/// Exact capabilities transitioned by #1930.
pub const VMNET_FEASIBILITY_CAPABILITY_IDS: [&str; 2] = NETWORK_VMNET_FEASIBLE_IDS;

const UNRELATED_INVENTORY_SHA256: &str =
    "eb54c4cf9cadaaf7a2ddb7a86b5642a93af1cc913bdd3edaedff5cc53472dc00";

const NONCLAIMS: [VmnetFeasibilityNonclaim; 6] = [
    VmnetFeasibilityNonclaim::RootDirectProductionTopology,
    VmnetFeasibilityNonclaim::AppleAuthorizedVmnetPath,
    VmnetFeasibilityNonclaim::PrivilegedProviderProtocolOrBroker,
    VmnetFeasibilityNonclaim::SandboxWorkerRemoteProvider,
    VmnetFeasibilityNonclaim::ProductionServiceCrashReclamationAndConcurrency,
    VmnetFeasibilityNonclaim::CapabilityImplementationOrParentCompletion,
];

struct EvidenceSpec {
    id: VmnetFeasibilityEvidenceId,
    repetitions: u8,
    checks: &'static [&'static str],
    implementation: &'static [(&'static str, &'static str)],
    validation: &'static [(&'static str, &'static str)],
}

const EVIDENCE: [EvidenceSpec; 3] = [
    EvidenceSpec {
        id: VmnetFeasibilityEvidenceId::OrdinaryUserDenial,
        repetitions: 1,
        checks: &[
            "same-ad-hoc-hvf-signed-binary",
            "http-400",
            "vmnet-start-interface-denied",
            "normal-process-and-socket-cleanup",
        ],
        implementation: &[
            (
                "crates/bangbang/tests/elevated_vmnet_e2e.rs",
                "fn ordinary_user_vmnet_start_is_denied()",
            ),
            (
                "scripts/prepare-elevated-vmnet-evidence.sh",
                "bangbang elevated vmnet prepare: ordinary denial failed",
            ),
        ],
        validation: &[
            (
                "scripts/tests/test_elevated_vmnet_evidence.py",
                "class ElevatedVmnetEvidenceTests",
            ),
            ("scripts/run-elevated-vmnet-evidence.sh", "denial=passed"),
        ],
    },
    EvidenceSpec {
        id: VmnetFeasibilityEvidenceId::DroppedOwnerDataPlane,
        repetitions: 1,
        checks: &[
            "root-start",
            "parameter-bounds",
            "irreversible-uid-gid-and-groups-drop",
            "callback",
            "bounded-read",
            "bounded-write",
            "stop",
            "no-residue",
        ],
        implementation: &[
            (
                "crates/bangbang/tests/elevated_vmnet_e2e.rs",
                "fn dropped_owner_retains_bounded_vmnet_io()",
            ),
            (
                "crates/bangbang/src/host_network/vmnet.rs",
                "pub struct SystemVmnetInterfaceBackend",
            ),
            (
                "crates/session/src/macos/credential.rs",
                "pub fn transition_process",
            ),
        ],
        validation: &[(
            "scripts/run-elevated-vmnet-evidence.sh",
            "dropped owner failed",
        )],
    },
    EvidenceSpec {
        id: VmnetFeasibilityEvidenceId::DirectGuestConnectivity,
        repetitions: 2,
        checks: &[
            "public-unix-http-api",
            "direct-boot-v111",
            "shared-vmnet",
            "strict-dhcp",
            "router-derived-endpoint",
            "nonce-bound-request-response",
            "normal-stop",
            "no-residue",
            "clean-repeat",
        ],
        implementation: &[
            (
                "crates/bangbang/tests/elevated_vmnet_e2e.rs",
                "fn elevated_direct_guest_uses_shared_vmnet()",
            ),
            (
                "scripts/guest/elevated_vmnet_certification.rs",
                "pub extern \"C\" fn _start()",
            ),
            (
                "scripts/fetch-firecracker-rootfs.sh",
                "bangbang.elevated-vmnet-certification=1",
            ),
        ],
        validation: &[
            (
                "scripts/run-elevated-vmnet-evidence.sh",
                "guest=passed repeat=passed cleanup=passed",
            ),
            (
                "compat/firecracker/v1.16.0/vmnet-feasibility-contract.md",
                "## Exact evidence result",
            ),
            (
                "tools/firecracker-capability-audit/tests/vmnet_feasibility_audit.rs",
                "fn checked_vmnet_feasibility_audit_is_canonical_and_fail_closed()",
            ),
        ],
    },
];

/// Validate the complete checked entitlement-free vmnet feasibility authority.
pub fn validate_vmnet_feasibility_audit(
    audit: &VmnetFeasibilityAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header_and_sources(audit, manifest, &mut errors);
    validate_boundary(audit, &mut errors);
    validate_evidence(audit, repository_root, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);
    if audit.nonclaims != NONCLAIMS {
        errors.push("vmnet feasibility requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header_and_sources(
    audit: &VmnetFeasibilityAudit,
    manifest: &SourceManifest,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != VMNET_FEASIBILITY_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "vmnet feasibility schema_version must be {VMNET_FEASIBILITY_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
    {
        errors.push("vmnet feasibility baseline is not the pinned release".to_string());
    }
    if audit.parent_issue != "#1378" || audit.delivery_issue != "#1930" {
        errors.push("vmnet feasibility ownership must be #1378/#1930".to_string());
    }

    let expected_source = (
        "corpus:network-setup",
        "docs/network-setup.md",
        "entire-file",
        "c161b6661d4362a49d1978e0cafc5e7a6e5cebf6",
    );
    let actual_source = (
        audit.upstream_source.manifest_id.as_str(),
        audit.upstream_source.path.as_str(),
        audit.upstream_source.anchor.as_str(),
        audit.upstream_source.git_blob.as_str(),
    );
    if actual_source != expected_source {
        errors.push("vmnet feasibility requires the exact pinned network source".to_string());
    }
    match manifest
        .items
        .iter()
        .find(|item| item.id == expected_source.0)
    {
        Some(item) if item.path == expected_source.1 && item.anchor == expected_source.2 => {}
        Some(_) => errors.push("vmnet feasibility source identity drifted".to_string()),
        None => errors.push("vmnet feasibility source identity is missing".to_string()),
    }
    match manifest
        .inputs
        .iter()
        .find(|input| input.path == expected_source.1)
    {
        Some(input) if input.git_blob == expected_source.3 => {}
        Some(_) => errors.push("vmnet feasibility source blob drifted".to_string()),
        None => errors.push("vmnet feasibility source input is missing".to_string()),
    }

    let expected_platform_sources = [
        (
            "apple-vmnet",
            "https://developer.apple.com/documentation/vmnet",
            "public-vmnet-packet-and-shared-network-api",
        ),
        (
            "apple-vm-networking-entitlement",
            "https://developer.apple.com/documentation/bundleresources/entitlements/com_apple_vm_networking",
            "restricted-entitlement-enables-vmnet-without-root-escalation",
        ),
    ];
    if audit.platform_sources.len() != expected_platform_sources.len() {
        errors.push("vmnet feasibility requires the exact public platform sources".to_string());
    }
    for (index, (source, expected)) in audit
        .platform_sources
        .iter()
        .zip(expected_platform_sources)
        .enumerate()
    {
        let url = match &source.reference {
            Reference::Authoritative { url } => url.as_str(),
            _ => "",
        };
        if (source.id.as_str(), url, source.reviewed_claim.as_str()) != expected {
            errors.push(format!(
                "vmnet feasibility platform source[{index}] drifted"
            ));
        }
    }
}

fn validate_boundary(audit: &VmnetFeasibilityAudit, errors: &mut Vec<String>) {
    let boundary = &audit.boundary;
    if (
        boundary.platform.as_str(),
        boundary.preparation_identity.as_str(),
        boundary.runtime_authority.as_str(),
        boundary.apple_authorization.as_str(),
        boundary.root_direct_topology.as_str(),
        boundary.elevated_build_download_or_discovery.as_str(),
        boundary.diagnostic_class.as_str(),
    ) != (
        "macos-arm64-hvf",
        "ordinary-user",
        "explicit-exact-root",
        "absent",
        "evidence-only",
        "forbidden",
        "fixed-categorical-only",
    ) {
        errors.push("vmnet feasibility authorization or topology boundary drifted".to_string());
    }
}

fn validate_evidence(
    audit: &VmnetFeasibilityAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    if audit.evidence.len() != EVIDENCE.len() {
        errors.push("vmnet feasibility requires exactly three evidence profiles".to_string());
    }
    let tracked = tracked_repository_files(repository_root, errors);
    for (index, (record, expected)) in audit.evidence.iter().zip(EVIDENCE.iter()).enumerate() {
        let checks = record
            .required_checks
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let implementation = references(expected.implementation);
        let validation = references(expected.validation);
        if record.id != expected.id
            || record.repetitions != expected.repetitions
            || record.outcome != "passed"
            || checks != expected.checks
            || record.implementation != implementation
            || record.validation != validation
        {
            errors.push(format!("vmnet feasibility evidence[{index}] drifted"));
        }
        for (kind, references) in [
            ("implementation", &record.implementation),
            ("validation", &record.validation),
        ] {
            for (reference_index, reference) in references.iter().enumerate() {
                let label =
                    format!("vmnet feasibility evidence[{index}] {kind}[{reference_index}]");
                validate_reference(reference, repository_root, &tracked, &label, errors);
                validate_local_anchor(reference, repository_root, &label, errors);
            }
        }
    }
}

fn references(values: &[(&str, &str)]) -> Vec<Reference> {
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

fn validate_inventory_transition(
    audit: &VmnetFeasibilityAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let previous = &audit.previous_counts;
    if (
        previous.implemented_and_verified,
        previous.audit_required,
        previous.missing_platform_feasible,
        previous.proven_platform_impossible,
    ) != (383, 2, 0, 33)
    {
        errors.push("vmnet feasibility previous counts must be exactly 383/2/0/33".to_string());
    }
    let target = &audit.target_counts;
    if (
        target.implemented_and_verified,
        target.audit_required,
        target.missing_platform_feasible,
        target.proven_platform_impossible,
    ) != (383, 0, 2, 33)
    {
        errors.push("vmnet feasibility target counts must be exactly 383/0/2/33".to_string());
    }
    if disposition_counts(inventory) != (383, 0, 2, 33)
        || classify_inventory_phase(inventory) != Ok(InventoryPhase::NetworkVmnetFeasibility)
    {
        errors.push("vmnet feasibility live inventory must be exact 383/0/2/33".to_string());
    }

    if audit.transitions.len() != VMNET_FEASIBILITY_CAPABILITY_IDS.len() {
        errors.push("vmnet feasibility requires exactly two transitions".to_string());
    }
    for (index, (transition, expected_id)) in audit
        .transitions
        .iter()
        .zip(VMNET_FEASIBILITY_CAPABILITY_IDS)
        .enumerate()
    {
        if transition.capability_id != expected_id
            || transition.previous_disposition != Disposition::AuditRequired
            || transition.target_disposition != Disposition::MissingPlatformFeasible
            || transition.delivery_issue != "#1378"
        {
            errors.push(format!("vmnet feasibility transition[{index}] drifted"));
        }
        match inventory
            .capabilities
            .iter()
            .find(|capability| capability.id == expected_id)
        {
            Some(capability) if capability_is_exact_handoff(capability, expected_id) => {}
            Some(_) => errors.push(format!(
                "vmnet feasibility capability is not the exact feasible handoff: {expected_id}"
            )),
            None => errors.push(format!(
                "vmnet feasibility capability is missing: {expected_id}"
            )),
        }
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push("vmnet feasibility unrelated-inventory digest authority drifted".to_string());
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "vmnet feasibility unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push("vmnet feasibility unrelated inventory is not serializable".to_string()),
    }
}

fn capability_is_exact_handoff(capability: &Capability, id: &str) -> bool {
    let source_refs = if id == "corpus:network-setup" {
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
        && capability.disposition == Disposition::MissingPlatformFeasible
        && capability.implementation.is_empty()
        && capability.validation.is_empty()
        && capability.delivery_issue.as_deref()
            == Some("https://github.com/seven332/bangbang/issues/1378")
        && capability.exclusion.is_none()
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let excluded = VMNET_FEASIBILITY_CAPABILITY_IDS
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

fn validate_canonical_bytes(
    audit: &VmnetFeasibilityAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let expected = match vmnet_feasibility_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(_) => {
            errors.push("vmnet feasibility audit is not serializable".to_string());
            return;
        }
    };
    match std::fs::read(repository_root.join(VMNET_FEASIBILITY_AUDIT_PATH)) {
        Ok(actual) if actual == expected => {}
        Ok(_) => errors.push("vmnet feasibility audit is not canonical JSON".to_string()),
        Err(_) => errors.push("vmnet feasibility audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_encoding_is_lowercase_and_exact() {
        assert_eq!(encode_sha256([0x00, 0x1f, 0xa0, 0xff]), "001fa0ff");
    }

    #[test]
    fn exact_capability_partition_is_closed() {
        assert_eq!(
            VMNET_FEASIBILITY_CAPABILITY_IDS
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "corpus:network-setup",
                "semantic.network:virtio-net-vmnet-policy-and-connectivity",
            ])
        );
    }
}
