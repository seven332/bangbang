use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, FORMAL_VERIFICATION_AUDIT_PATH,
    FORMAL_VERIFICATION_COMPATIBILITY_CAPABILITY_IDS, FormalVerificationAudit,
    FormalVerificationNonclaim, Reference, SOURCE_MANIFEST_PATH, formal_verification_audit_json,
    read_capability_inventory, read_formal_verification_audit, read_source_manifest,
    validate_formal_verification_audit, validate_formal_verification_compatibility,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_formal_verification_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let audit_path = root.join(FORMAL_VERIFICATION_AUDIT_PATH);
    let audit = read_formal_verification_audit(&audit_path)
        .expect("checked formal verification audit must parse");

    let canonical = formal_verification_audit_json(&audit)
        .expect("checked formal verification audit must serialize canonically");
    assert_eq!(
        canonical,
        std::fs::read(audit_path).expect("checked formal verification audit must be readable")
    );
    validate_formal_verification_audit(&audit, &root)
        .expect("checked formal verification audit must satisfy every exact invariant");

    let mut reordered = audit.clone();
    reordered.harnesses.swap(0, 1);
    let error = validate_formal_verification_audit(&reordered, &root)
        .expect_err("reordered harnesses must fail")
        .to_string();
    assert!(error.contains("exact ordered harness set"));

    let mut renamed = audit.clone();
    renamed.harnesses[0].harness.push_str("_stale");
    let error = validate_formal_verification_audit(&renamed, &root)
        .expect_err("renamed manifest harness must fail")
        .to_string();
    assert!(error.contains("stale identity") || error.contains("bijection differs"));

    let mut changed_command = audit.clone();
    changed_command.harnesses[1]
        .command
        .push("--quiet".to_string());
    let error = validate_formal_verification_audit(&changed_command, &root)
        .expect_err("noncanonical harness commands must fail")
        .to_string();
    assert!(error.contains("stale identity, owner, or command"));

    let mut missing_bounds = audit.clone();
    missing_bounds.harnesses[2].bounds.clear();
    let error = validate_formal_verification_audit(&missing_bounds, &root)
        .expect_err("missing proof bounds must fail")
        .to_string();
    assert!(error.contains("requires assumptions, bounds"));

    let mut stale_anchor = audit.clone();
    stale_anchor.harnesses[0].implementation[0] = Reference::Local {
        path: "crates/pager/src/frame.rs".to_string(),
        anchor: Some("fn stale_formal_verification_anchor()".to_string()),
    };
    let error = validate_formal_verification_audit(&stale_anchor, &root)
        .expect_err("stale local evidence anchors must fail")
        .to_string();
    assert!(error.contains("local reference anchor is absent"));

    let mut missing_anchor = audit.clone();
    missing_anchor.harnesses[0].implementation[0] = Reference::Local {
        path: "crates/pager/src/frame.rs".to_string(),
        anchor: None,
    };
    let error = validate_formal_verification_audit(&missing_anchor, &root)
        .expect_err("unanchored local evidence must fail")
        .to_string();
    assert!(error.contains("must be an anchored local reference"));

    let mut reordered_evidence = audit.clone();
    reordered_evidence.evidence.validation.swap(0, 1);
    let error = validate_formal_verification_audit(&reordered_evidence, &root)
        .expect_err("noncanonical evidence order must fail")
        .to_string();
    assert!(error.contains("unique and canonically sorted"));

    let mut changed_nonclaims = audit;
    changed_nonclaims.nonclaims = vec![FormalVerificationNonclaim::FfiOrHvfBehavior];
    let error = validate_formal_verification_audit(&changed_nonclaims, &root)
        .expect_err("open proof nonclaims must fail")
        .to_string();
    assert!(error.contains("exact ordered nonclaims"));
}

#[test]
fn formal_verification_terminal_scope_is_exact() {
    let root = repository_root();
    let source = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit = read_formal_verification_audit(&root.join(FORMAL_VERIFICATION_AUDIT_PATH))
        .expect("checked formal verification audit must parse");

    assert_eq!(
        FORMAL_VERIFICATION_COMPATIBILITY_CAPABILITY_IDS,
        ["corpus:formal-verification"]
    );
    validate_formal_verification_compatibility(&source, &inventory, &audit, &root)
        .expect("terminal formal verification scope must certify");

    let mut downgraded = inventory;
    let capability = downgraded
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:formal-verification")
        .expect("owned capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_formal_verification_compatibility(&source, &downgraded, &audit, &root)
        .expect_err("terminal capability downgrade must fail")
        .to_string();
    assert!(error.contains("implemented-and-verified"));
}

#[test]
fn formal_verification_schema_rejects_unknown_and_duplicate_fields() {
    let root = repository_root();
    let source = std::fs::read_to_string(root.join(FORMAL_VERIFICATION_AUDIT_PATH))
        .expect("checked audit must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"proof_complete\": true,",
        1,
    );
    let error = serde_json::from_str::<FormalVerificationAudit>(&unknown)
        .expect_err("unknown fields must fail");
    assert!(error.to_string().contains("unknown field"));

    let duplicate = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"schema_version\": 1,",
        1,
    );
    let error = serde_json::from_str::<FormalVerificationAudit>(&duplicate)
        .expect_err("duplicate fields must fail");
    assert!(error.to_string().contains("duplicate field"));
}
