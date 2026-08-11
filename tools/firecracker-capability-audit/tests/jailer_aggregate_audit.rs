use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, JAILER_AGGREGATE_AUDIT_PATH,
    JAILER_AGGREGATE_CAPABILITY_IDS, JailerAggregateAudit, JailerAggregateNonclaim, Reference,
    SOURCE_MANIFEST_PATH, jailer_aggregate_audit_json, read_capability_inventory,
    read_jailer_aggregate_audit, read_source_manifest, validate_jailer_aggregate_audit,
    validate_jailer_aggregate_compatibility,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_jailer_aggregate_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let path = root.join(JAILER_AGGREGATE_AUDIT_PATH);
    let audit =
        read_jailer_aggregate_audit(&path).expect("checked jailer aggregate audit must parse");

    assert_eq!(
        jailer_aggregate_audit_json(&audit)
            .expect("checked jailer aggregate audit must serialize canonically"),
        std::fs::read(path).expect("checked jailer aggregate audit must be readable")
    );
    validate_jailer_aggregate_audit(&audit, &manifest, &inventory, &root)
        .expect("checked jailer aggregate audit must validate");

    let mut source_drift = audit.clone();
    source_drift.upstream_sources[0].git_blob = "0".repeat(40);
    assert!(
        validate_jailer_aggregate_audit(&source_drift, &manifest, &inventory, &root)
            .expect_err("source drift must fail")
            .to_string()
            .contains("exact ordered pinned sources")
    );
    let mut manifest_source_drift = manifest.clone();
    manifest_source_drift
        .inputs
        .iter_mut()
        .find(|input| input.path == "src/jailer/src/env.rs")
        .expect("jailer operation input must be pinned")
        .git_blob = "0".repeat(40);
    assert!(
        validate_jailer_aggregate_audit(&audit, &manifest_source_drift, &inventory, &root)
            .expect_err("manifest source drift must fail")
            .to_string()
            .contains("source blob drifted: src/jailer/src/env.rs")
    );

    let mut argument_reorder = audit.clone();
    argument_reorder.arguments.swap(0, 1);
    assert!(
        validate_jailer_aggregate_audit(&argument_reorder, &manifest, &inventory, &root)
            .expect_err("argument reorder must fail")
            .to_string()
            .contains("argument[0]")
    );
    let mut argument_duplicate = audit.clone();
    argument_duplicate.arguments[1] = argument_duplicate.arguments[0].clone();
    assert!(
        validate_jailer_aggregate_audit(&argument_duplicate, &manifest, &inventory, &root)
            .expect_err("argument duplicate must fail")
            .to_string()
            .contains("duplicate argument")
    );
    let mut argument_missing = audit.clone();
    argument_missing.arguments.pop();
    assert!(
        validate_jailer_aggregate_audit(&argument_missing, &manifest, &inventory, &root)
            .expect_err("missing argument must fail")
            .to_string()
            .contains("exactly 13")
    );

    let mut operation_reorder = audit.clone();
    operation_reorder.operation_steps.swap(0, 1);
    assert!(
        validate_jailer_aggregate_audit(&operation_reorder, &manifest, &inventory, &root)
            .expect_err("operation reorder must fail")
            .to_string()
            .contains("operation step[0]")
    );
    let mut operation_duplicate = audit.clone();
    operation_duplicate.operation_steps[1] = operation_duplicate.operation_steps[0].clone();
    assert!(
        validate_jailer_aggregate_audit(&operation_duplicate, &manifest, &inventory, &root)
            .expect_err("operation duplicate must fail")
            .to_string()
            .contains("duplicate operation step")
    );
    let mut operation_missing = audit.clone();
    operation_missing.operation_steps.pop();
    assert!(
        validate_jailer_aggregate_audit(&operation_missing, &manifest, &inventory, &root)
            .expect_err("missing operation must fail")
            .to_string()
            .contains("exactly 16")
    );

    let mut section_missing = audit.clone();
    section_missing.corpus_sections.pop();
    assert!(
        validate_jailer_aggregate_audit(&section_missing, &manifest, &inventory, &root)
            .expect_err("missing corpus section must fail")
            .to_string()
            .contains("exact seven corpus sections")
    );

    let mut stale_anchor = audit.clone();
    stale_anchor.evidence_profiles[0].validation[0] = Reference::Local {
        path: "crates/launcher/src/launch_policy.rs".to_string(),
        anchor: Some("fn missing_jailer_anchor()".to_string()),
    };
    assert!(
        validate_jailer_aggregate_audit(&stale_anchor, &manifest, &inventory, &root)
            .expect_err("stale evidence anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );

    let mut wrong_leaf = inventory.clone();
    wrong_leaf
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "tool-argument:jailer/cgroup")
        .expect("jailer cgroup leaf must exist")
        .disposition = Disposition::AuditRequired;
    assert!(
        validate_jailer_aggregate_audit(&audit, &manifest, &wrong_leaf, &root)
            .expect_err("wrong terminal leaf disposition must fail")
            .to_string()
            .contains("argument disposition")
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
        validate_jailer_aggregate_audit(&audit, &manifest, &unrelated, &root)
            .expect_err("count-preserving unrelated mutation must fail")
            .to_string()
            .contains("unrelated inventory changed")
    );

    let mut nonclaim_drift = audit;
    nonclaim_drift.nonclaims = vec![JailerAggregateNonclaim::LinuxJailerMechanismParity];
    assert!(
        validate_jailer_aggregate_audit(&nonclaim_drift, &manifest, &inventory, &root)
            .expect_err("nonclaim drift must fail")
            .to_string()
            .contains("exact ordered nonclaims")
    );
}

#[test]
fn jailer_terminal_transition_is_exact() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit = read_jailer_aggregate_audit(&root.join(JAILER_AGGREGATE_AUDIT_PATH))
        .expect("checked jailer aggregate audit must parse");

    assert_eq!(
        JAILER_AGGREGATE_CAPABILITY_IDS,
        ["corpus:jailer", "tool-operation:jailer/run"]
    );
    validate_jailer_aggregate_compatibility(&manifest, &inventory, &audit, &root)
        .expect("terminal aggregate jailer scope must certify");

    for id in JAILER_AGGREGATE_CAPABILITY_IDS {
        let mut partial = inventory.clone();
        let capability = partial
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("owned jailer capability must exist");
        capability.disposition = Disposition::AuditRequired;
        capability.implementation.clear();
        capability.validation.clear();
        assert!(
            validate_jailer_aggregate_compatibility(&manifest, &partial, &audit, &root)
                .expect_err("partial aggregate transition must fail")
                .to_string()
                .contains("379/3/3/33")
        );
    }

    let mut evidence_drift = inventory.clone();
    evidence_drift
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:jailer")
        .expect("owned jailer capability must exist")
        .implementation[0] = Reference::Local {
        path: "compat/firecracker/v1.16.0/isolation-contract.md".to_string(),
        anchor: Some("## Delivered Production Boundary".to_string()),
    };
    assert!(
        validate_jailer_aggregate_compatibility(&manifest, &evidence_drift, &audit, &root)
            .expect_err("valid but unrelated implementation evidence must fail")
            .to_string()
            .contains("implementation evidence drifted")
    );

    let mut unrelated_owner = inventory;
    unrelated_owner
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:production-host")
        .expect("production-host capability must exist")
        .delivery_issue = Some("#1912".to_string());
    assert!(
        validate_jailer_aggregate_compatibility(&manifest, &unrelated_owner, &audit, &root)
            .expect_err("unrelated #1912 ownership must fail")
            .to_string()
            .contains("unrelated #1912 ownership")
    );
}

#[test]
fn jailer_aggregate_schema_rejects_unknown_and_duplicate_fields() {
    let source = std::fs::read_to_string(repository_root().join(JAILER_AGGREGATE_AUDIT_PATH))
        .expect("checked jailer aggregate audit must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"all_done\": true,",
        1,
    );
    assert!(
        serde_json::from_str::<JailerAggregateAudit>(&unknown)
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
        serde_json::from_str::<JailerAggregateAudit>(&duplicate)
            .expect_err("duplicate fields must fail")
            .to_string()
            .contains("duplicate field")
    );
}
