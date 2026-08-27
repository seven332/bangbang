use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, Reference, SOURCE_MANIFEST_PATH,
    VMNET_FEASIBILITY_AUDIT_PATH, VMNET_FEASIBILITY_CAPABILITY_IDS, VmnetFeasibilityNonclaim,
    read_capability_inventory, read_source_manifest, read_vmnet_feasibility_audit,
    validate_vmnet_feasibility_audit, vmnet_feasibility_audit_json,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_vmnet_feasibility_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let path = root.join(VMNET_FEASIBILITY_AUDIT_PATH);
    let audit =
        read_vmnet_feasibility_audit(&path).expect("checked vmnet feasibility audit must parse");

    assert_eq!(
        vmnet_feasibility_audit_json(&audit)
            .expect("checked vmnet feasibility audit must serialize canonically"),
        std::fs::read(path).expect("checked vmnet feasibility audit must be readable")
    );
    validate_vmnet_feasibility_audit(&audit, &manifest, &inventory, &root)
        .expect("checked vmnet feasibility audit must validate");
    assert_eq!(
        VMNET_FEASIBILITY_CAPABILITY_IDS,
        [
            "corpus:network-setup",
            "semantic.network:virtio-net-vmnet-policy-and-connectivity",
        ]
    );

    let mut schema_drift = audit.clone();
    schema_drift.schema_version += 1;
    assert!(
        validate_vmnet_feasibility_audit(&schema_drift, &manifest, &inventory, &root)
            .expect_err("schema drift must fail")
            .to_string()
            .contains("schema_version")
    );

    let mut baseline_drift = audit.clone();
    baseline_drift.baseline.commit = "0".repeat(40);
    assert!(
        validate_vmnet_feasibility_audit(&baseline_drift, &manifest, &inventory, &root)
            .expect_err("baseline drift must fail")
            .to_string()
            .contains("baseline is not the pinned release")
    );

    let mut ownership_drift = audit.clone();
    ownership_drift.delivery_issue = "#1378".to_string();
    assert!(
        validate_vmnet_feasibility_audit(&ownership_drift, &manifest, &inventory, &root)
            .expect_err("ownership drift must fail")
            .to_string()
            .contains("ownership must be #1378/#1930")
    );

    let mut source_drift = audit.clone();
    source_drift.upstream_source.git_blob = "0".repeat(40);
    assert!(
        validate_vmnet_feasibility_audit(&source_drift, &manifest, &inventory, &root)
            .expect_err("source drift must fail")
            .to_string()
            .contains("exact pinned network source")
    );

    let mut manifest_source_drift = manifest.clone();
    manifest_source_drift
        .inputs
        .iter_mut()
        .find(|input| input.path == "docs/network-setup.md")
        .expect("network source input must be pinned")
        .git_blob = "0".repeat(40);
    assert!(
        validate_vmnet_feasibility_audit(&audit, &manifest_source_drift, &inventory, &root)
            .expect_err("manifest source drift must fail")
            .to_string()
            .contains("source blob drifted")
    );

    let mut platform_source_drift = audit.clone();
    platform_source_drift.platform_sources.swap(0, 1);
    assert!(
        validate_vmnet_feasibility_audit(&platform_source_drift, &manifest, &inventory, &root)
            .expect_err("platform source order drift must fail")
            .to_string()
            .contains("platform source[0] drifted")
    );

    let mut boundary_drift = audit.clone();
    boundary_drift.boundary.apple_authorization = "present".to_string();
    assert!(
        validate_vmnet_feasibility_audit(&boundary_drift, &manifest, &inventory, &root)
            .expect_err("authorization boundary drift must fail")
            .to_string()
            .contains("authorization or topology boundary drifted")
    );

    let mut evidence_order_drift = audit.clone();
    evidence_order_drift.evidence.swap(0, 1);
    assert!(
        validate_vmnet_feasibility_audit(&evidence_order_drift, &manifest, &inventory, &root)
            .expect_err("evidence order drift must fail")
            .to_string()
            .contains("evidence[0] drifted")
    );

    let mut repetition_drift = audit.clone();
    repetition_drift.evidence[2].repetitions = 1;
    assert!(
        validate_vmnet_feasibility_audit(&repetition_drift, &manifest, &inventory, &root)
            .expect_err("repetition drift must fail")
            .to_string()
            .contains("evidence[2] drifted")
    );

    let mut check_drift = audit.clone();
    check_drift.evidence[2].required_checks.swap(0, 1);
    assert!(
        validate_vmnet_feasibility_audit(&check_drift, &manifest, &inventory, &root)
            .expect_err("evidence check order drift must fail")
            .to_string()
            .contains("evidence[2] drifted")
    );

    let mut stale_anchor = audit.clone();
    stale_anchor.evidence[0].implementation[0] = Reference::Local {
        path: "crates/bangbang/tests/elevated_vmnet_e2e.rs".to_string(),
        anchor: Some("fn missing_elevated_vmnet_anchor()".to_string()),
    };
    assert!(
        validate_vmnet_feasibility_audit(&stale_anchor, &manifest, &inventory, &root)
            .expect_err("stale evidence anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );

    let mut previous_count_drift = audit.clone();
    previous_count_drift.previous_counts.audit_required -= 1;
    assert!(
        validate_vmnet_feasibility_audit(&previous_count_drift, &manifest, &inventory, &root)
            .expect_err("previous count drift must fail")
            .to_string()
            .contains("previous counts must be exactly 383/2/0/33")
    );

    let mut target_count_drift = audit.clone();
    target_count_drift.target_counts.missing_platform_feasible -= 1;
    assert!(
        validate_vmnet_feasibility_audit(&target_count_drift, &manifest, &inventory, &root)
            .expect_err("target count drift must fail")
            .to_string()
            .contains("target counts must be exactly 383/0/2/33")
    );

    let mut transition_order_drift = audit.clone();
    transition_order_drift.transitions.swap(0, 1);
    assert!(
        validate_vmnet_feasibility_audit(&transition_order_drift, &manifest, &inventory, &root)
            .expect_err("transition order drift must fail")
            .to_string()
            .contains("transition[0] drifted")
    );

    let mut transition_drift = audit.clone();
    transition_drift.transitions[0].target_disposition = Disposition::AuditRequired;
    assert!(
        validate_vmnet_feasibility_audit(&transition_drift, &manifest, &inventory, &root)
            .expect_err("transition disposition drift must fail")
            .to_string()
            .contains("transition[0] drifted")
    );

    let mut owned_capability_drift = inventory.clone();
    owned_capability_drift
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == VMNET_FEASIBILITY_CAPABILITY_IDS[0])
        .expect("owned capability must exist")
        .delivery_issue = None;
    assert!(
        validate_vmnet_feasibility_audit(&audit, &manifest, &owned_capability_drift, &root)
            .expect_err("owned capability evidence drift must fail")
            .to_string()
            .contains("not the exact feasible handoff")
    );

    let mut digest_authority_drift = audit.clone();
    digest_authority_drift.unrelated_inventory_sha256 = "0".repeat(64);
    assert!(
        validate_vmnet_feasibility_audit(&digest_authority_drift, &manifest, &inventory, &root)
            .expect_err("digest authority drift must fail")
            .to_string()
            .contains("digest authority drifted")
    );

    let mut unrelated = inventory.clone();
    unrelated
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:logger")
        .expect("unrelated capability must exist")
        .summary
        .push_str(" drift");
    assert!(
        validate_vmnet_feasibility_audit(&audit, &manifest, &unrelated, &root)
            .expect_err("count-preserving unrelated mutation must fail")
            .to_string()
            .contains("unrelated inventory changed")
    );

    let mut nonclaim_drift = audit;
    nonclaim_drift.nonclaims = vec![VmnetFeasibilityNonclaim::AppleAuthorizedVmnetPath];
    assert!(
        validate_vmnet_feasibility_audit(&nonclaim_drift, &manifest, &inventory, &root)
            .expect_err("nonclaim drift must fail")
            .to_string()
            .contains("exact ordered nonclaims")
    );
}
