use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, CPU_TEMPLATE_HELPER_AUDIT_PATH,
    CPU_TEMPLATE_IMPLEMENTED_FOUNDATION_IDS, CpuTemplateHelperNonclaim, Disposition, Reference,
    cpu_template_helper_audit_json, read_capability_inventory, read_cpu_template_helper_audit,
    validate_cpu_template_helper_audit,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_cpu_template_helper_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let audit_path = root.join(CPU_TEMPLATE_HELPER_AUDIT_PATH);
    let audit = read_cpu_template_helper_audit(&audit_path)
        .expect("checked CPU-template helper audit must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");

    let canonical = cpu_template_helper_audit_json(&audit)
        .expect("checked CPU-template helper audit must serialize canonically");
    assert_eq!(
        canonical,
        std::fs::read(audit_path).expect("checked CPU-template helper audit must be readable")
    );
    validate_cpu_template_helper_audit(&audit, &inventory, &root)
        .expect("checked CPU-template helper audit must satisfy every exact invariant");

    let mut reordered = audit.clone();
    reordered.operations.swap(0, 1);
    let error = validate_cpu_template_helper_audit(&reordered, &inventory, &root)
        .expect_err("reordered operation producers must fail")
        .to_string();
    assert!(error.contains("exact ordered operation set"));

    let mut missing_argument = audit.clone();
    missing_argument.operations[0].argument_ids.pop();
    let error = validate_cpu_template_helper_audit(&missing_argument, &inventory, &root)
        .expect_err("missing argument membership must fail")
        .to_string();
    assert!(error.contains("exact 18 helper identities once"));

    let mut missing_foundation = audit.clone();
    missing_foundation
        .foundations
        .implemented_and_verified
        .pop();
    let error = validate_cpu_template_helper_audit(&missing_foundation, &inventory, &root)
        .expect_err("missing runtime foundation must fail")
        .to_string();
    assert!(error.contains("exact ordered implemented foundation set"));

    let mut reclassified = inventory.clone();
    let foundation = reclassified
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == CPU_TEMPLATE_IMPLEMENTED_FOUNDATION_IDS[0])
        .expect("implemented foundation must exist");
    foundation.disposition = Disposition::AuditRequired;
    foundation.implementation.clear();
    foundation.validation.clear();
    let error = validate_cpu_template_helper_audit(&audit, &reclassified, &root)
        .expect_err("reclassified runtime foundation must fail")
        .to_string();
    assert!(error.contains("foundation has the wrong terminal disposition"));

    let mut missing_scenario = audit.clone();
    missing_scenario.scenarios.pop();
    let error = validate_cpu_template_helper_audit(&missing_scenario, &inventory, &root)
        .expect_err("missing aggregate scenario must fail")
        .to_string();
    assert!(error.contains("exact ordered scenario set"));

    let mut changed_nonclaims = audit.clone();
    changed_nonclaims.nonclaims = vec![CpuTemplateHelperNonclaim::MigrationSafety];
    let error = validate_cpu_template_helper_audit(&changed_nonclaims, &inventory, &root)
        .expect_err("open aggregate claims must fail")
        .to_string();
    assert!(error.contains("exact ordered nonclaim set"));

    let mut stale_anchor = audit;
    stale_anchor.scenarios[0].validation[0] = Reference::Local {
        path: "tools/cpu-template-helper/tests/cli.rs".to_string(),
        anchor: Some("fn missing_cpu_template_helper_test".to_string()),
    };
    let error = validate_cpu_template_helper_audit(&stale_anchor, &inventory, &root)
        .expect_err("stale evidence anchor must fail")
        .to_string();
    assert!(error.contains("anchor does not resolve"));
}
