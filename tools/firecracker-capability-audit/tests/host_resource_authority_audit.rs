use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, HOST_RESOURCE_AUTHORITY_AUDIT_PATH,
    HOST_RESOURCE_AUTHORITY_CAPABILITY_ID, HostResourceAccess, HostResourceAuthorityAudit,
    HostResourceClauseOutcome, HostResourceNonclaim, Reference, SOURCE_MANIFEST_PATH,
    host_resource_authority_audit_json, read_capability_inventory,
    read_host_resource_authority_audit, read_source_manifest,
    validate_host_resource_authority_audit, validate_host_resource_authority_compatibility,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_host_resource_authority_is_canonical_and_fail_closed() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let path = root.join(HOST_RESOURCE_AUTHORITY_AUDIT_PATH);
    let audit = read_host_resource_authority_audit(&path)
        .expect("checked host-resource authority must parse");

    assert_eq!(
        host_resource_authority_audit_json(&audit)
            .expect("checked host-resource authority must serialize canonically"),
        std::fs::read(path).expect("checked host-resource authority must be readable")
    );
    validate_host_resource_authority_audit(&audit, &manifest, &inventory, &root)
        .expect("checked host-resource authority must validate");

    let mut source_drift = audit.clone();
    source_drift.upstream_sources[0].git_blob = "0".repeat(40);
    assert!(
        validate_host_resource_authority_audit(&source_drift, &manifest, &inventory, &root)
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
        validate_host_resource_authority_audit(&audit, &manifest_source_drift, &inventory, &root,)
            .expect_err("manifest source drift must fail")
            .to_string()
            .contains("source blob drifted: docs/design.md")
    );

    let mut clause_reorder = audit.clone();
    clause_reorder.source_clauses.swap(0, 1);
    assert!(
        validate_host_resource_authority_audit(&clause_reorder, &manifest, &inventory, &root)
            .expect_err("clause reorder must fail")
            .to_string()
            .contains("source clause[0]")
    );
    let mut clause_duplicate = audit.clone();
    clause_duplicate.source_clauses[1] = clause_duplicate.source_clauses[0].clone();
    assert!(
        validate_host_resource_authority_audit(&clause_duplicate, &manifest, &inventory, &root)
            .expect_err("clause duplicate must fail")
            .to_string()
            .contains("duplicate source clause")
    );
    let mut clause_missing = audit.clone();
    clause_missing.source_clauses.pop();
    assert!(
        validate_host_resource_authority_audit(&clause_missing, &manifest, &inventory, &root)
            .expect_err("missing clause must fail")
            .to_string()
            .contains("exactly 30")
    );
    let mut clause_outcome_drift = audit.clone();
    clause_outcome_drift.source_clauses[0].outcome =
        HostResourceClauseOutcome::OperatorOwnedOutcome;
    assert!(
        validate_host_resource_authority_audit(
            &clause_outcome_drift,
            &manifest,
            &inventory,
            &root,
        )
        .expect_err("source outcome drift must fail")
        .to_string()
        .contains("source clause[0]")
    );

    let mut resource_reorder = audit.clone();
    resource_reorder.resource_surface.swap(0, 1);
    assert!(
        validate_host_resource_authority_audit(&resource_reorder, &manifest, &inventory, &root)
            .expect_err("resource reorder must fail")
            .to_string()
            .contains("resource role[0]")
    );
    let mut resource_duplicate = audit.clone();
    resource_duplicate.resource_surface[1] = resource_duplicate.resource_surface[0].clone();
    assert!(
        validate_host_resource_authority_audit(&resource_duplicate, &manifest, &inventory, &root)
            .expect_err("resource duplicate must fail")
            .to_string()
            .contains("duplicate resource role")
    );
    let mut resource_missing = audit.clone();
    resource_missing.resource_surface.pop();
    assert!(
        validate_host_resource_authority_audit(&resource_missing, &manifest, &inventory, &root)
            .expect_err("missing resource must fail")
            .to_string()
            .contains("exactly 18")
    );
    let mut resource_access_drift = audit.clone();
    resource_access_drift.resource_surface[0].access = vec![HostResourceAccess::ReadWrite];
    assert!(
        validate_host_resource_authority_audit(
            &resource_access_drift,
            &manifest,
            &inventory,
            &root,
        )
        .expect_err("resource access drift must fail")
        .to_string()
        .contains("resource role[0]")
    );

    let mut dependency_drift = audit.clone();
    dependency_drift.terminal_dependencies[0].disposition = Disposition::AuditRequired;
    assert!(
        validate_host_resource_authority_audit(&dependency_drift, &manifest, &inventory, &root)
            .expect_err("dependency drift must fail")
            .to_string()
            .contains("exact terminal dependencies")
    );
    let mut external_drift = audit.clone();
    external_drift.external_dependencies[0].disposition = Disposition::ImplementedAndVerified;
    assert!(
        validate_host_resource_authority_audit(&external_drift, &manifest, &inventory, &root)
            .expect_err("external dependency drift must fail")
            .to_string()
            .contains("external dependency[0] drifted")
    );
    let mut external_owner_drift = audit.clone();
    external_owner_drift.external_dependencies[0].owner_issue = "#1916".to_string();
    assert!(
        validate_host_resource_authority_audit(
            &external_owner_drift,
            &manifest,
            &inventory,
            &root,
        )
        .expect_err("external owner drift must fail")
        .to_string()
        .contains("external dependency[0] drifted")
    );
    let mut stale_anchor = audit.clone();
    stale_anchor.evidence_profiles[0].validation[0] = Reference::Local {
        path: "crates/launcher/src/grant_manifest.rs".to_string(),
        anchor: Some("fn missing_host_resource_anchor()".to_string()),
    };
    assert!(
        validate_host_resource_authority_audit(&stale_anchor, &manifest, &inventory, &root)
            .expect_err("stale evidence anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );

    let mut residual_drift = audit.clone();
    residual_drift.residuals.swap(0, 1);
    assert!(
        validate_host_resource_authority_audit(&residual_drift, &manifest, &inventory, &root)
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
        validate_host_resource_authority_audit(&audit, &manifest, &unrelated, &root)
            .expect_err("count-preserving unrelated mutation must fail")
            .to_string()
            .contains("unrelated inventory changed")
    );

    let mut nonclaim_drift = audit;
    nonclaim_drift.nonclaims = vec![HostResourceNonclaim::GeneralDynamicResourceBroker];
    assert!(
        validate_host_resource_authority_audit(&nonclaim_drift, &manifest, &inventory, &root)
            .expect_err("nonclaim drift must fail")
            .to_string()
            .contains("exact ordered nonclaims")
    );
}

#[test]
fn host_resource_terminal_transition_is_exact() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit = read_host_resource_authority_audit(&root.join(HOST_RESOURCE_AUTHORITY_AUDIT_PATH))
        .expect("checked host-resource authority must parse");

    assert_eq!(
        HOST_RESOURCE_AUTHORITY_CAPABILITY_ID,
        "semantic.isolation:host-resource-authority-and-brokerage"
    );
    validate_host_resource_authority_compatibility(&manifest, &inventory, &audit, &root)
        .expect("terminal host-resource authority scope must certify");

    let mut partial = inventory.clone();
    let capability = partial
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
        .expect("owned host-resource capability must exist");
    capability.disposition = Disposition::MissingPlatformFeasible;
    capability.implementation.clear();
    capability.validation.clear();
    capability.delivery_issue =
        Some("https://github.com/seven332/bangbang/issues/1351".to_string());
    assert!(
        validate_host_resource_authority_compatibility(&manifest, &partial, &audit, &root)
            .expect_err("partial host-resource transition must fail")
            .to_string()
            .contains("host-resource authority capability is not terminal")
    );

    let mut evidence_drift = inventory.clone();
    evidence_drift
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
        .expect("owned host-resource capability must exist")
        .implementation[0] = Reference::Local {
        path: "compat/firecracker/v1.16.0/isolation-contract.md".to_string(),
        anchor: Some("## Terminal aggregate jailer outcome".to_string()),
    };
    assert!(
        validate_host_resource_authority_compatibility(&manifest, &evidence_drift, &audit, &root,)
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
        .delivery_issue = Some("#1916".to_string());
    assert!(
        validate_host_resource_authority_compatibility(&manifest, &unrelated_owner, &audit, &root,)
            .expect_err("unrelated #1916 ownership must fail")
            .to_string()
            .contains("unrelated #1916 ownership")
    );
}

#[test]
fn host_resource_authority_schema_rejects_unknown_and_duplicate_fields() {
    let source =
        std::fs::read_to_string(repository_root().join(HOST_RESOURCE_AUTHORITY_AUDIT_PATH))
            .expect("checked host-resource authority must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"all_done\": true,",
        1,
    );
    assert!(
        serde_json::from_str::<HostResourceAuthorityAudit>(&unknown)
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
        serde_json::from_str::<HostResourceAuthorityAudit>(&duplicate)
            .expect_err("duplicate fields must fail")
            .to_string()
            .contains("duplicate field")
    );
}
