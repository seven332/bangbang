use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, ContainmentClauseOutcome, ContainmentNonclaim, Disposition,
    JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH, JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID,
    JailerSeccompContainmentAudit, Reference, SOURCE_MANIFEST_PATH,
    jailer_seccomp_containment_audit_json, read_capability_inventory,
    read_jailer_seccomp_containment_audit, read_source_manifest,
    validate_jailer_seccomp_containment_audit, validate_jailer_seccomp_containment_compatibility,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_jailer_seccomp_containment_is_canonical_and_fail_closed() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let path = root.join(JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH);
    let audit = read_jailer_seccomp_containment_audit(&path)
        .expect("checked jailer/seccomp containment audit must parse");

    assert_eq!(
        jailer_seccomp_containment_audit_json(&audit)
            .expect("checked containment audit must serialize canonically"),
        std::fs::read(path).expect("checked containment audit must be readable")
    );
    validate_jailer_seccomp_containment_audit(&audit, &manifest, &inventory, &root)
        .expect("checked containment audit must validate");

    let mut source_drift = audit.clone();
    source_drift.upstream_sources[0].git_blob = "0".repeat(40);
    assert!(
        validate_jailer_seccomp_containment_audit(&source_drift, &manifest, &inventory, &root)
            .expect_err("source drift must fail")
            .to_string()
            .contains("exact ordered pinned sources")
    );
    let mut manifest_source_drift = manifest.clone();
    manifest_source_drift
        .inputs
        .iter_mut()
        .find(|input| input.path == "docs/design.md")
        .expect("design input must be pinned")
        .git_blob = "0".repeat(40);
    assert!(
        validate_jailer_seccomp_containment_audit(
            &audit,
            &manifest_source_drift,
            &inventory,
            &root,
        )
        .expect_err("manifest source drift must fail")
        .to_string()
        .contains("source blob drifted: docs/design.md")
    );

    let mut clause_reorder = audit.clone();
    clause_reorder.source_clauses.swap(0, 1);
    assert!(
        validate_jailer_seccomp_containment_audit(&clause_reorder, &manifest, &inventory, &root)
            .expect_err("clause reorder must fail")
            .to_string()
            .contains("source clause[0]")
    );
    let mut clause_duplicate = audit.clone();
    clause_duplicate.source_clauses[1] = clause_duplicate.source_clauses[0].clone();
    assert!(
        validate_jailer_seccomp_containment_audit(&clause_duplicate, &manifest, &inventory, &root,)
            .expect_err("clause duplicate must fail")
            .to_string()
            .contains("duplicate source clause")
    );
    let mut clause_missing = audit.clone();
    clause_missing.source_clauses.pop();
    assert!(
        validate_jailer_seccomp_containment_audit(&clause_missing, &manifest, &inventory, &root)
            .expect_err("missing clause must fail")
            .to_string()
            .contains("exactly 46")
    );
    let mut clause_unknown = audit.clone();
    clause_unknown
        .source_clauses
        .push(audit.source_clauses[0].clone());
    assert!(
        validate_jailer_seccomp_containment_audit(&clause_unknown, &manifest, &inventory, &root)
            .expect_err("unknown clause must fail")
            .to_string()
            .contains("unknown source clauses")
    );
    let mut clause_outcome_drift = audit.clone();
    clause_outcome_drift.source_clauses[0].outcome = ContainmentClauseOutcome::OperatorOwnedOutcome;
    assert!(
        validate_jailer_seccomp_containment_audit(
            &clause_outcome_drift,
            &manifest,
            &inventory,
            &root,
        )
        .expect_err("source outcome drift must fail")
        .to_string()
        .contains("source clause[0]")
    );

    let mut dependency_drift = audit.clone();
    dependency_drift.terminal_dependencies[0].disposition = Disposition::AuditRequired;
    assert!(
        validate_jailer_seccomp_containment_audit(&dependency_drift, &manifest, &inventory, &root)
            .expect_err("dependency drift must fail")
            .to_string()
            .contains("exact terminal dependencies")
    );
    let mut external_drift = audit.clone();
    external_drift.external_dependencies[0].disposition = Disposition::ImplementedAndVerified;
    assert!(
        validate_jailer_seccomp_containment_audit(&external_drift, &manifest, &inventory, &root)
            .expect_err("external dependency drift must fail")
            .to_string()
            .contains("external dependency[0] drifted")
    );
    let mut external_owner_drift = audit.clone();
    external_owner_drift.external_dependencies[0].owner_issue = "#1918".to_string();
    assert!(
        validate_jailer_seccomp_containment_audit(
            &external_owner_drift,
            &manifest,
            &inventory,
            &root,
        )
        .expect_err("external owner drift must fail")
        .to_string()
        .contains("external dependency[0] drifted")
    );

    let mut profile_reorder = audit.clone();
    profile_reorder.evidence_profiles.swap(0, 1);
    assert!(
        validate_jailer_seccomp_containment_audit(&profile_reorder, &manifest, &inventory, &root)
            .expect_err("profile reorder must fail")
            .to_string()
            .contains("exact ordered evidence profiles")
    );
    let mut stale_anchor = audit.clone();
    stale_anchor.evidence_profiles[0].validation[0] = Reference::Local {
        path: "crates/launcher/tests/production_bundle_e2e.rs".to_string(),
        anchor: Some("fn missing_containment_anchor()".to_string()),
    };
    assert!(
        validate_jailer_seccomp_containment_audit(&stale_anchor, &manifest, &inventory, &root)
            .expect_err("stale evidence anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );
    let mut stale_path = audit.clone();
    stale_path.evidence_profiles[0].validation[0] = Reference::Local {
        path: "crates/launcher/tests/missing_containment.rs".to_string(),
        anchor: Some("fn missing_containment_anchor()".to_string()),
    };
    assert!(
        validate_jailer_seccomp_containment_audit(&stale_path, &manifest, &inventory, &root)
            .expect_err("stale evidence path must fail")
            .to_string()
            .contains("local reference path does not exist")
    );

    let mut residual_drift = audit.clone();
    residual_drift.residuals.swap(0, 1);
    assert!(
        validate_jailer_seccomp_containment_audit(&residual_drift, &manifest, &inventory, &root)
            .expect_err("residual drift must fail")
            .to_string()
            .contains("exact residual classifications")
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
        validate_jailer_seccomp_containment_audit(&audit, &manifest, &unrelated, &root)
            .expect_err("count-preserving unrelated mutation must fail")
            .to_string()
            .contains("unrelated inventory changed")
    );

    let mut nonclaim_drift = audit;
    nonclaim_drift.nonclaims = vec![ContainmentNonclaim::LinuxMechanismParity];
    assert!(
        validate_jailer_seccomp_containment_audit(&nonclaim_drift, &manifest, &inventory, &root,)
            .expect_err("nonclaim drift must fail")
            .to_string()
            .contains("exact ordered nonclaims")
    );
}

#[test]
fn jailer_seccomp_containment_terminal_transition_is_exact() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit =
        read_jailer_seccomp_containment_audit(&root.join(JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH))
            .expect("checked containment audit must parse");

    assert_eq!(
        JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID,
        "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes"
    );
    validate_jailer_seccomp_containment_compatibility(&manifest, &inventory, &audit, &root)
        .expect("terminal containment scope must certify");

    let mut partial = inventory.clone();
    let capability = partial
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID)
        .expect("owned containment capability must exist");
    capability.source_refs.remove(0);
    capability.disposition = Disposition::MissingPlatformFeasible;
    capability.implementation.clear();
    capability.validation.clear();
    capability.delivery_issue =
        Some("https://github.com/seven332/bangbang/issues/1351".to_string());
    assert!(
        validate_jailer_seccomp_containment_compatibility(&manifest, &partial, &audit, &root,)
            .expect_err("partial containment transition must fail")
            .to_string()
            .contains("382/3/0/33")
    );

    let mut evidence_drift = inventory.clone();
    evidence_drift
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID)
        .expect("owned containment capability must exist")
        .implementation[0] = Reference::Local {
        path: "compat/firecracker/v1.16.0/isolation-contract.md".to_string(),
        anchor: Some("## Certified Linux runtime isolation exclusions".to_string()),
    };
    assert!(
        validate_jailer_seccomp_containment_compatibility(
            &manifest,
            &evidence_drift,
            &audit,
            &root,
        )
        .expect_err("valid but unrelated evidence must fail")
        .to_string()
        .contains("implementation evidence drifted")
    );

    let mut unrelated_owner = inventory;
    unrelated_owner
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:production-host")
        .expect("production-host capability must exist")
        .delivery_issue = Some("#1918".to_string());
    assert!(
        validate_jailer_seccomp_containment_compatibility(
            &manifest,
            &unrelated_owner,
            &audit,
            &root,
        )
        .expect_err("unrelated #1918 ownership must fail")
        .to_string()
        .contains("unrelated #1918 ownership")
    );
}

#[test]
fn jailer_seccomp_containment_schema_rejects_unknown_and_duplicate_fields() {
    let source =
        std::fs::read_to_string(repository_root().join(JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH))
            .expect("checked containment audit must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"all_done\": true,",
        1,
    );
    assert!(
        serde_json::from_str::<JailerSeccompContainmentAudit>(&unknown)
            .expect_err("unknown fields must fail")
            .to_string()
            .contains("unknown field")
    );
    let duplicate = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"schema_version\": 1,",
        1,
    );
    assert!(
        serde_json::from_str::<JailerSeccompContainmentAudit>(&duplicate)
            .expect_err("duplicate fields must fail")
            .to_string()
            .contains("duplicate field")
    );
}
