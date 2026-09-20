use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, PRODUCTION_VMNET_CERTIFICATION_AUDIT_PATH,
    PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS, PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH,
    PRODUCTION_VMNET_MANDATORY_CASES, PRODUCTION_VMNET_OPTIONAL_CASES,
    ProductionVmnetCertificationAudit, Reference, production_vmnet_certification_audit_json,
    production_vmnet_certification_result_json, read_capability_inventory,
    read_production_vmnet_certification_audit, read_production_vmnet_certification_result,
    validate_production_vmnet_certification_audit, validate_production_vmnet_certification_result,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_production_vmnet_certification_is_canonical_and_fail_closed() {
    let root = repository_root();
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit_path = root.join(PRODUCTION_VMNET_CERTIFICATION_AUDIT_PATH);
    let audit = read_production_vmnet_certification_audit(&audit_path)
        .expect("checked production vmnet audit must parse");
    let result_path = root.join(PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH);
    let result = read_production_vmnet_certification_result(&result_path)
        .expect("checked production vmnet evidence must parse");

    assert_eq!(
        production_vmnet_certification_audit_json(&audit)
            .expect("audit must serialize canonically"),
        std::fs::read(audit_path).expect("audit must be readable")
    );
    assert_eq!(
        production_vmnet_certification_result_json(&result)
            .expect("result must serialize canonically"),
        std::fs::read(result_path).expect("result must be readable")
    );
    validate_production_vmnet_certification_audit(&audit, &inventory, &root)
        .expect("checked production vmnet authority must validate");
    validate_production_vmnet_certification_result(&result, &audit)
        .expect("checked production vmnet result must validate");

    assert_eq!(PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS.len(), 2);
    assert_eq!(PRODUCTION_VMNET_MANDATORY_CASES.len(), 27);
    assert_eq!(PRODUCTION_VMNET_OPTIONAL_CASES.len(), 4);

    let mut schema_drift = audit.clone();
    schema_drift.schema_version += 1;
    assert!(
        validate_production_vmnet_certification_audit(&schema_drift, &inventory, &root)
            .expect_err("schema drift must fail")
            .to_string()
            .contains("schema_version")
    );

    let mut baseline_drift = audit.clone();
    baseline_drift.baseline.commit = "0".repeat(40);
    assert!(
        validate_production_vmnet_certification_audit(&baseline_drift, &inventory, &root)
            .expect_err("baseline drift must fail")
            .to_string()
            .contains("baseline is not pinned")
    );

    let mut ownership_drift = audit.clone();
    ownership_drift.delivery_issue = "#1378".to_string();
    assert!(
        validate_production_vmnet_certification_audit(&ownership_drift, &inventory, &root)
            .expect_err("ownership drift must fail")
            .to_string()
            .contains("ownership must be #1378/#1948")
    );

    let mut repetition_drift = audit.clone();
    repetition_drift.evidence.repetitions = 1;
    assert!(
        validate_production_vmnet_certification_audit(&repetition_drift, &inventory, &root)
            .expect_err("repetition drift must fail")
            .to_string()
            .contains("evidence identity drifted")
    );

    let mut digest_drift = audit.clone();
    digest_drift.evidence.sha256 = "0".repeat(64);
    assert!(
        validate_production_vmnet_certification_audit(&digest_drift, &inventory, &root)
            .expect_err("digest authority drift must fail")
            .to_string()
            .contains("evidence identity drifted")
    );

    let mut source_drift = result.clone();
    source_drift.source.commit = "0".repeat(40);
    assert!(
        validate_production_vmnet_certification_result(&source_drift, &audit)
            .expect_err("source drift must fail")
            .to_string()
            .contains("result boundary drifted")
    );

    let mut authority_drift = result.clone();
    authority_drift.authority.provider = "root-vmm".to_string();
    assert!(
        validate_production_vmnet_certification_result(&authority_drift, &audit)
            .expect_err("authority drift must fail")
            .to_string()
            .contains("result boundary drifted")
    );

    let mut entitlement_drift = result.clone();
    entitlement_drift.entitlements.worker_vmnet = true;
    assert!(
        validate_production_vmnet_certification_result(&entitlement_drift, &audit)
            .expect_err("entitlement drift must fail")
            .to_string()
            .contains("result boundary drifted")
    );

    let mut case_order_drift = result.clone();
    case_order_drift.cases.swap(0, 1);
    assert!(
        validate_production_vmnet_certification_result(&case_order_drift, &audit)
            .expect_err("case order drift must fail")
            .to_string()
            .contains("exact 31 case outcomes")
    );

    let mut mandatory_outcome_drift = result.clone();
    mandatory_outcome_drift.cases[0].outcome = "environment-gated".to_string();
    assert!(
        validate_production_vmnet_certification_result(&mandatory_outcome_drift, &audit)
            .expect_err("mandatory outcome drift must fail")
            .to_string()
            .contains("exact 31 case outcomes")
    );

    let mut optional_outcome_drift = result.clone();
    optional_outcome_drift.cases[8].outcome = "passed".to_string();
    assert!(
        validate_production_vmnet_certification_result(&optional_outcome_drift, &audit)
            .expect_err("optional outcome restamping must fail")
            .to_string()
            .contains("exact 31 case outcomes")
    );

    let mut cleanup_drift = result.clone();
    cleanup_drift.cleanup = "incomplete".to_string();
    cleanup_drift.verdict = "failed".to_string();
    assert!(
        validate_production_vmnet_certification_result(&cleanup_drift, &audit)
            .expect_err("cleanup drift must fail")
            .to_string()
            .contains("result boundary drifted")
    );

    let mut optional_order_drift = audit.clone();
    optional_order_drift.optional_non_dependencies.swap(0, 1);
    assert!(
        validate_production_vmnet_certification_audit(&optional_order_drift, &inventory, &root)
            .expect_err("optional order drift must fail")
            .to_string()
            .contains("optional nondependencies")
    );

    let mut optional_required = audit.clone();
    optional_required.claims[0].required_cases[0] = "host-connectivity".to_string();
    assert!(
        validate_production_vmnet_certification_audit(&optional_required, &inventory, &root)
            .expect_err("optional evidence dependency must fail")
            .to_string()
            .contains("no optional case")
    );

    let mut stale_anchor = audit.clone();
    stale_anchor.claims[0].validation[0] = Reference::Local {
        path: PRODUCTION_VMNET_CERTIFICATION_EVIDENCE_PATH.to_string(),
        anchor: Some("missing-production-vmnet-anchor".to_string()),
    };
    assert!(
        validate_production_vmnet_certification_audit(&stale_anchor, &inventory, &root)
            .expect_err("stale anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );

    let mut previous_count_drift = audit.clone();
    previous_count_drift
        .previous_counts
        .missing_platform_feasible = 1;
    assert!(
        validate_production_vmnet_certification_audit(&previous_count_drift, &inventory, &root)
            .expect_err("previous count drift must fail")
            .to_string()
            .contains("previous counts must be exactly 383/0/2/33")
    );

    let mut target_count_drift = audit.clone();
    target_count_drift.target_counts.implemented_and_verified = 384;
    assert!(
        validate_production_vmnet_certification_audit(&target_count_drift, &inventory, &root)
            .expect_err("target count drift must fail")
            .to_string()
            .contains("target counts must be exactly 385/0/0/33")
    );

    let mut transition_drift = audit.clone();
    transition_drift.transitions[0].target_disposition = Disposition::MissingPlatformFeasible;
    assert!(
        validate_production_vmnet_certification_audit(&transition_drift, &inventory, &root)
            .expect_err("transition drift must fail")
            .to_string()
            .contains("transition[0] drifted")
    );

    let mut incomplete_capability = inventory.clone();
    incomplete_capability
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS[0])
        .expect("promoted capability must exist")
        .validation
        .clear();
    assert!(
        validate_production_vmnet_certification_audit(&audit, &incomplete_capability, &root)
            .expect_err("incomplete terminal evidence must fail")
            .to_string()
            .contains("not exact terminal evidence")
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
        validate_production_vmnet_certification_audit(&audit, &unrelated, &root)
            .expect_err("unrelated inventory drift must fail")
            .to_string()
            .contains("unrelated inventory changed")
    );

    let mut summary_drift = inventory.clone();
    summary_drift
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == PRODUCTION_VMNET_CERTIFICATION_CAPABILITY_IDS[0])
        .expect("promoted capability must exist")
        .summary
        .push_str(" drift");
    assert!(
        validate_production_vmnet_certification_audit(&audit, &summary_drift, &root)
            .expect_err("terminal capability summary drift must fail")
            .to_string()
            .contains("not exact terminal evidence")
    );

    let mut unknown = serde_json::to_value(&audit).expect("audit must serialize");
    unknown
        .as_object_mut()
        .expect("audit must be an object")
        .insert("unknown".to_string(), serde_json::Value::Bool(true));
    assert!(
        serde_json::from_value::<ProductionVmnetCertificationAudit>(unknown)
            .expect_err("unknown audit field must fail")
            .to_string()
            .contains("unknown field")
    );

    let mut unknown_result = serde_json::to_value(&result).expect("result must serialize");
    unknown_result
        .as_object_mut()
        .expect("result must be an object")
        .insert("unknown".to_string(), serde_json::Value::Bool(true));
    assert!(
        serde_json::from_value::<
            bangbang_firecracker_capability_audit::ProductionVmnetCertificationResult,
        >(unknown_result)
        .expect_err("unknown result field must fail")
        .to_string()
        .contains("unknown field")
    );
}
