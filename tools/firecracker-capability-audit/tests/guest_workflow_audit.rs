use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, GUEST_WORKFLOW_AUDIT_PATH,
    GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS, GuestWorkflowDeliveryState, GuestWorkflowNonclaim,
    Reference, SOURCE_MANIFEST_PATH, guest_workflow_audit_json, read_capability_inventory,
    read_guest_workflow_audit, read_source_manifest, validate_guest_workflow_audit,
    validate_guest_workflow_compatibility,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_guest_workflow_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let audit_path = root.join(GUEST_WORKFLOW_AUDIT_PATH);
    let audit =
        read_guest_workflow_audit(&audit_path).expect("checked guest workflow audit must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");

    let canonical = guest_workflow_audit_json(&audit)
        .expect("checked guest workflow audit must serialize canonically");
    assert_eq!(
        canonical,
        std::fs::read(audit_path).expect("checked guest workflow audit must be readable")
    );
    validate_guest_workflow_audit(&audit, &inventory, &root)
        .expect("checked guest workflow audit must satisfy every exact invariant");

    let mut reordered = audit.clone();
    reordered.artifacts.swap(0, 1);
    let error = validate_guest_workflow_audit(&reordered, &inventory, &root)
        .expect_err("reordered artifacts must fail")
        .to_string();
    assert!(error.contains("exact ordered artifact set"));

    let mut wrong_size = audit.clone();
    wrong_size.artifacts[0].size_bytes += 1;
    let error = validate_guest_workflow_audit(&wrong_size, &inventory, &root)
        .expect_err("changed artifact size must fail")
        .to_string();
    assert!(error.contains("stale pin"));

    let mut missing_profile_evidence = audit.clone();
    missing_profile_evidence.profiles[0].implementation.clear();
    let error = validate_guest_workflow_audit(&missing_profile_evidence, &inventory, &root)
        .expect_err("missing terminal profile evidence must fail")
        .to_string();
    assert!(error.contains("exact delivery state"));

    let mut regressed_delivery = audit.clone();
    regressed_delivery.delivery.state = GuestWorkflowDeliveryState::Preparation;
    let error = validate_guest_workflow_audit(&regressed_delivery, &inventory, &root)
        .expect_err("preparation state with terminal evidence must fail")
        .to_string();
    assert!(error.contains("exact delivery state"));

    let mut changed_nonclaims = audit.clone();
    changed_nonclaims.nonclaims = vec![GuestWorkflowNonclaim::ProductionWorkflow];
    let error = validate_guest_workflow_audit(&changed_nonclaims, &inventory, &root)
        .expect_err("open nonclaims must fail")
        .to_string();
    assert!(error.contains("exact ordered nonclaim set"));

    let mut downgraded = inventory.clone();
    let capability = downgraded
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:getting-started")
        .expect("owned capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_guest_workflow_audit(&audit, &downgraded, &root)
        .expect_err("terminal capability downgrade must fail")
        .to_string();
    assert!(error.contains("exact implemented-and-verified evidence"));

    let mut stale_anchor = audit;
    stale_anchor.evidence.validation[0] = Reference::Local {
        path: "scripts/run-integration-tests.sh".to_string(),
        anchor: Some("scripts/run-macos-guest-workflow.py missing".to_string()),
    };
    let error = validate_guest_workflow_audit(&stale_anchor, &inventory, &root)
        .expect_err("stale evidence anchor must fail")
        .to_string();
    assert!(error.contains("anchor does not resolve"));
}

#[test]
fn guest_workflow_terminal_scope_is_exact() {
    let root = repository_root();
    let source = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit = read_guest_workflow_audit(&root.join(GUEST_WORKFLOW_AUDIT_PATH))
        .expect("checked guest workflow audit must parse");

    assert_eq!(
        GUEST_WORKFLOW_COMPATIBILITY_CAPABILITY_IDS,
        ["corpus:getting-started", "corpus:rootfs-and-kernel"]
    );
    validate_guest_workflow_compatibility(&source, &inventory, &audit, &root)
        .expect("terminal guest workflow scope must certify");

    let mut mixed = inventory.clone();
    mixed
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:rootfs-and-kernel")
        .expect("owned capability must exist")
        .disposition = Disposition::AuditRequired;
    let error = validate_guest_workflow_compatibility(&source, &mixed, &audit, &root)
        .expect_err("mixed terminal transition must fail")
        .to_string();
    assert!(error.contains("implemented-and-verified"));
}

#[test]
fn guest_workflow_schema_rejects_unknown_and_duplicate_fields() {
    let root = repository_root();
    let source = std::fs::read_to_string(root.join(GUEST_WORKFLOW_AUDIT_PATH))
        .expect("checked audit must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"final\": true,",
        1,
    );
    let error =
        serde_json::from_str::<bangbang_firecracker_capability_audit::GuestWorkflowAudit>(&unknown)
            .expect_err("unknown fields must fail");
    assert!(error.to_string().contains("unknown field"));

    let duplicate = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"schema_version\": 1,",
        1,
    );
    let error = serde_json::from_str::<bangbang_firecracker_capability_audit::GuestWorkflowAudit>(
        &duplicate,
    )
    .expect_err("duplicate fields must fail");
    assert!(error.to_string().contains("duplicate field"));
}
