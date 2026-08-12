use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, HOST_RESOURCE_AUTHORITY_CAPABILITY_ID,
    JAILER_AGGREGATE_CAPABILITY_IDS, MULTIPROCESS_ISOLATION_CAPABILITY_ID, Reference,
    SOURCE_MANIFEST_PATH, WAVE8_CERTIFICATION_AUDIT_PATH, WAVE8_CERTIFICATION_CAPABILITY_ID,
    WAVE8_OWNED_CAPABILITY_IDS, Wave8CertificationAudit, Wave8HandoffOwner,
    read_capability_inventory, read_source_manifest, read_wave8_certification_audit,
    validate_wave8_certification_audit, validate_wave8_certification_compatibility,
    wave8_certification_audit_json,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_wave8_certification_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let path = root.join(WAVE8_CERTIFICATION_AUDIT_PATH);
    let audit = read_wave8_certification_audit(&path).expect("checked Wave 8 audit must parse");
    let canonical =
        wave8_certification_audit_json(&audit).expect("checked Wave 8 audit must serialize");
    assert_eq!(
        canonical,
        std::fs::read(path).expect("audit must be readable")
    );
    validate_wave8_certification_audit(&audit, &manifest, &inventory, &root)
        .expect("checked Wave 8 authority must validate");

    let mut domain_order = audit.clone();
    domain_order.domains.swap(0, 1);
    assert!(
        validate_wave8_certification_audit(&domain_order, &manifest, &inventory, &root)
            .expect_err("domain reorder must fail")
            .to_string()
            .contains("exact ordered seven")
    );

    let mut missing_pair = audit.clone();
    missing_pair.interactions.pop();
    assert!(
        validate_wave8_certification_audit(&missing_pair, &manifest, &inventory, &root)
            .expect_err("missing pair must fail")
            .to_string()
            .contains("exact derived 21")
    );

    let mut hybrid_scenario = audit.clone();
    hybrid_scenario.scenarios[0].domains.pop();
    let error = validate_wave8_certification_audit(&hybrid_scenario, &manifest, &inventory, &root)
        .expect_err("hybrid scenario membership must fail")
        .to_string();
    assert!(error.contains("scenario metadata drifted") || error.contains("exact derived 21"));

    let mut unrelated_evidence = audit.clone();
    unrelated_evidence.scenarios[0].evidence[0] = Reference::Local {
        path: "tools/firecracker-capability-audit/src/wave8_certification_audit_model.rs"
            .to_string(),
        anchor: Some("pub struct Wave8CertificationAudit".to_string()),
    };
    assert!(
        validate_wave8_certification_audit(&unrelated_evidence, &manifest, &inventory, &root,)
            .expect_err("valid but unrelated evidence must fail")
            .to_string()
            .contains("exact path and anchor set")
    );

    let mut stale_anchor = audit.clone();
    stale_anchor.scenarios[0].evidence[0] = Reference::Local {
        path: "crates/bangbang/src/api_server.rs".to_string(),
        anchor: Some("fn missing_wave8_anchor()".to_string()),
    };
    assert!(
        validate_wave8_certification_audit(&stale_anchor, &manifest, &inventory, &root)
            .expect_err("stale anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );

    let mut missing_outcome = audit.clone();
    missing_outcome.scenarios[3].outcomes.pop();
    let error = validate_wave8_certification_audit(&missing_outcome, &manifest, &inventory, &root)
        .expect_err("missing failure outcome must fail")
        .to_string();
    assert!(error.contains("scenario metadata drifted") || error.contains("outcome set"));

    let mut missing_impossible = audit.clone();
    missing_impossible.platform_reviews[0].capability_ids.pop();
    let error =
        validate_wave8_certification_audit(&missing_impossible, &manifest, &inventory, &root)
            .expect_err("missing impossible identity must fail")
            .to_string();
    assert!(error.contains("mechanism review drifted") || error.contains("exact 30"));

    let mut source_drift = audit.clone();
    source_drift.platform_reviews[2].platform_sources[0] = Reference::Authoritative {
        url: "https://github.com/apple-oss-distributions/xnu".to_string(),
    };
    assert!(
        validate_wave8_certification_audit(&source_drift, &manifest, &inventory, &root)
            .expect_err("platform source drift must fail")
            .to_string()
            .contains("exact primary-source set")
    );

    let mut challenge_drift = audit.clone();
    challenge_drift.platform_reviews[0].challenge = Reference::Github {
        url: "https://github.com/seven332/bangbang/issues/1881".to_string(),
    };
    assert!(
        validate_wave8_certification_audit(&challenge_drift, &manifest, &inventory, &root)
            .expect_err("Challenge drift must fail")
            .to_string()
            .contains("Challenge authority drifted")
    );

    let mut handoff_drift = audit.clone();
    handoff_drift.handoffs[0].owner = Wave8HandoffOwner::Issue1378;
    assert!(
        validate_wave8_certification_audit(&handoff_drift, &manifest, &inventory, &root)
            .expect_err("handoff owner drift must fail")
            .to_string()
            .contains("exact ordered 11")
    );

    let mut count_drift = audit.clone();
    count_drift.target_counts.audit_required += 1;
    assert!(
        validate_wave8_certification_audit(&count_drift, &manifest, &inventory, &root)
            .expect_err("target count drift must fail")
            .to_string()
            .contains("377/8/3/30")
    );

    let mut hierarchy_drift = audit.clone();
    hierarchy_drift.delivery_hierarchy.preceding_parents[1].outcome =
        bangbang_firecracker_capability_audit::Wave8DeliveryOutcome::Completed;
    assert!(
        validate_wave8_certification_audit(&hierarchy_drift, &manifest, &inventory, &root)
            .expect_err("external hierarchy drift must fail")
            .to_string()
            .contains("delivery-parent policy drifted")
    );

    let mut nonclaim_drift = audit;
    nonclaim_drift.nonclaims.pop();
    assert!(
        validate_wave8_certification_audit(&nonclaim_drift, &manifest, &inventory, &root)
            .expect_err("nonclaim drift must fail")
            .to_string()
            .contains("exact ordered nonclaims")
    );
}

#[test]
fn wave8_terminal_transition_is_exact() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit = read_wave8_certification_audit(&root.join(WAVE8_CERTIFICATION_AUDIT_PATH))
        .expect("checked Wave 8 audit must parse");

    assert_eq!(
        WAVE8_OWNED_CAPABILITY_IDS,
        [WAVE8_CERTIFICATION_CAPABILITY_ID]
    );
    validate_wave8_certification_compatibility(&manifest, &inventory, &audit, &root)
        .expect("terminal Wave 8 transition must certify");

    let mut historical_wave8 = inventory.clone();
    let containment = historical_wave8
        .capabilities
        .iter_mut()
        .find(|capability| {
            capability.id == "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes"
        })
        .expect("containment successor capability must exist");
    containment.source_refs = vec![
        "corpus:jailer".to_string(),
        "corpus:production-host".to_string(),
        "corpus:seccomp".to_string(),
        "corpus:seccompiler".to_string(),
    ];
    containment.disposition = Disposition::MissingPlatformFeasible;
    containment.implementation.clear();
    containment.validation.clear();
    containment.delivery_issue =
        Some("https://github.com/seven332/bangbang/issues/1351".to_string());
    containment.exclusion = None;
    let host_resource = historical_wave8
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
        .expect("host-resource-authority successor capability must exist");
    host_resource.source_refs = vec![
        "corpus:jailer".to_string(),
        "corpus:network-setup".to_string(),
        "corpus:production-host".to_string(),
    ];
    host_resource.disposition = Disposition::MissingPlatformFeasible;
    host_resource.implementation.clear();
    host_resource.validation.clear();
    host_resource.delivery_issue =
        Some("https://github.com/seven332/bangbang/issues/1351".to_string());
    let multiprocess = historical_wave8
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == MULTIPROCESS_ISOLATION_CAPABILITY_ID)
        .expect("multiprocess-isolation successor capability must exist");
    multiprocess.disposition = Disposition::MissingPlatformFeasible;
    multiprocess.implementation.clear();
    multiprocess.validation.clear();
    multiprocess.delivery_issue =
        Some("https://github.com/seven332/bangbang/issues/1351".to_string());
    for id in JAILER_AGGREGATE_CAPABILITY_IDS {
        let capability = historical_wave8
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("aggregate-jailer successor capability must exist");
        capability.disposition = Disposition::AuditRequired;
        capability.implementation.clear();
        capability.validation.clear();
    }
    for id in [
        "tool-argument:jailer/chroot-base-dir",
        "tool-argument:jailer/gid",
        "tool-argument:jailer/uid",
    ] {
        let capability = historical_wave8
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("post-Wave 8 jailer capability must exist");
        capability.disposition = Disposition::AuditRequired;
        capability.exclusion = None;
    }
    let production_host = historical_wave8
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:production-host")
        .expect("production-host successor capability must exist");
    production_host.disposition = Disposition::AuditRequired;
    production_host.implementation.clear();
    production_host.validation.clear();
    validate_wave8_certification_compatibility(&manifest, &historical_wave8, &audit, &root)
        .expect("historical 377/8/3/30 Wave 8 phase must remain valid");

    let mut downgraded = historical_wave8;
    let capability = downgraded
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == WAVE8_CERTIFICATION_CAPABILITY_ID)
        .expect("Wave 8 capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_wave8_certification_compatibility(&manifest, &downgraded, &audit, &root)
        .expect_err("partial Wave 8 transition must fail")
        .to_string();
    assert!(error.contains("377/8/3/30") || error.contains("not terminal"));

    let mut evidence_drift = inventory.clone();
    let capability = evidence_drift
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == WAVE8_CERTIFICATION_CAPABILITY_ID)
        .expect("Wave 8 capability must exist");
    capability.validation[0] = Reference::Local {
        path: "tools/firecracker-capability-audit/tests/wave8_certification_audit.rs".to_string(),
        anchor: Some(
            "fn checked_wave8_certification_audit_is_canonical_and_fail_closed()".to_string(),
        ),
    };
    assert!(
        validate_wave8_certification_compatibility(&manifest, &evidence_drift, &audit, &root,)
            .expect_err("valid but wrong transition evidence must fail")
            .to_string()
            .contains("validation evidence drifted")
    );

    let mut extra_evidence = inventory.clone();
    let capability = extra_evidence
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == WAVE8_CERTIFICATION_CAPABILITY_ID)
        .expect("Wave 8 capability must exist");
    capability.validation.push(Reference::Github {
        url: "https://github.com/seven332/bangbang/issues/1881".to_string(),
    });
    assert!(
        validate_wave8_certification_compatibility(&manifest, &extra_evidence, &audit, &root,)
            .expect_err("extra transition evidence must fail")
            .to_string()
            .contains("validation evidence drifted")
    );

    let mut unrelated = inventory;
    let capability = unrelated
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:logger")
        .expect("unrelated capability must exist");
    capability.delivery_issue = Some("#1881".to_string());
    assert!(
        validate_wave8_certification_compatibility(&manifest, &unrelated, &audit, &root)
            .expect_err("unrelated #1881 ownership must fail")
            .to_string()
            .contains("unrelated #1881 ownership")
    );
}

#[test]
fn wave8_schema_rejects_unknown_and_duplicate_fields() {
    let source = std::fs::read_to_string(repository_root().join(WAVE8_CERTIFICATION_AUDIT_PATH))
        .expect("checked Wave 8 audit must be readable");
    let unknown = source.replacen(
        "{\n  \"schema_version\": 1,",
        "{\n  \"schema_version\": 1,\n  \"planned\": true,",
        1,
    );
    assert!(
        serde_json::from_str::<Wave8CertificationAudit>(&unknown)
            .expect_err("unknown fields must fail")
            .to_string()
            .contains("unknown field")
    );

    let duplicate = source.replacen(
        "{\n  \"schema_version\": 1,",
        "{\n  \"schema_version\": 1,\n  \"schema_version\": 1,",
        1,
    );
    assert!(
        serde_json::from_str::<Wave8CertificationAudit>(&duplicate)
            .expect_err("duplicate fields must fail")
            .to_string()
            .contains("duplicate field")
    );
}
