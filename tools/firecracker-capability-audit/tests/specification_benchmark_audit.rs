use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, Reference, SOURCE_MANIFEST_PATH,
    SPECIFICATION_BENCHMARK_AUDIT_PATH, SPECIFICATION_BENCHMARK_CAPABILITY_IDS,
    SpecificationBenchmarkAudit, SpecificationBenchmarkNonclaim, read_capability_inventory,
    read_source_manifest, read_specification_benchmark_audit, specification_benchmark_audit_json,
    validate_specification_benchmark_audit, validate_specification_benchmark_compatibility,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_specification_benchmark_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let audit_path = root.join(SPECIFICATION_BENCHMARK_AUDIT_PATH);
    let audit = read_specification_benchmark_audit(&audit_path)
        .expect("checked specification benchmark audit must parse");
    let canonical = specification_benchmark_audit_json(&audit)
        .expect("checked specification benchmark audit must serialize");
    assert_eq!(
        canonical,
        std::fs::read(audit_path).expect("checked audit must be readable")
    );
    validate_specification_benchmark_audit(&audit, &root)
        .expect("checked specification benchmark audit must validate");

    let mut source_drift = audit.clone();
    source_drift.upstream_sources[0].git_blob = "0".repeat(40);
    let error = validate_specification_benchmark_audit(&source_drift, &root)
        .expect_err("upstream blob drift must fail")
        .to_string();
    assert!(error.contains("exact pinned upstream"));

    let mut measurement_drift = audit.clone();
    measurement_drift.measurements[0].unit = "milliseconds".to_string();
    let error = validate_specification_benchmark_audit(&measurement_drift, &root)
        .expect_err("measurement drift must fail")
        .to_string();
    assert!(error.contains("measurement identity drifted"));

    let mut policy_drift = audit.clone();
    policy_drift.policy.summary_fields.push("mean".to_string());
    let error = validate_specification_benchmark_audit(&policy_drift, &root)
        .expect_err("policy drift must fail")
        .to_string();
    assert!(error.contains("policy drifted"));

    let mut scope_drift = audit.clone();
    scope_drift.capability_ids.swap(0, 1);
    let error = validate_specification_benchmark_audit(&scope_drift, &root)
        .expect_err("capability reorder must fail")
        .to_string();
    assert!(error.contains("exact three capabilities"));

    let mut nonclaim_drift = audit.clone();
    nonclaim_drift.nonclaims = vec![SpecificationBenchmarkNonclaim::TrackedHardwareReport];
    let error = validate_specification_benchmark_audit(&nonclaim_drift, &root)
        .expect_err("open nonclaims must fail")
        .to_string();
    assert!(error.contains("exact ordered nonclaims"));

    let mut stale_anchor = audit;
    stale_anchor.evidence.implementation[0] = Reference::Local {
        path: "scripts/specification-benchmark.py".to_string(),
        anchor: Some("def stale_specification_benchmark_anchor(".to_string()),
    };
    let error = validate_specification_benchmark_audit(&stale_anchor, &root)
        .expect_err("stale local anchor must fail")
        .to_string();
    assert!(error.contains("anchor is absent"));
}

#[test]
fn specification_benchmark_terminal_scope_and_totals_are_exact() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked inventory must parse");
    let audit = read_specification_benchmark_audit(&root.join(SPECIFICATION_BENCHMARK_AUDIT_PATH))
        .expect("checked specification benchmark audit must parse");

    assert_eq!(
        SPECIFICATION_BENCHMARK_CAPABILITY_IDS,
        [
            "corpus:network-performance",
            "corpus:specification",
            "semantic.specification:performance-resource-and-telemetry-outcomes",
        ]
    );
    validate_specification_benchmark_compatibility(&manifest, &inventory, &audit, &root)
        .expect("terminal specification benchmark scope must certify");

    let mut downgraded = inventory;
    let capability = downgraded
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:specification")
        .expect("owned capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error =
        validate_specification_benchmark_compatibility(&manifest, &downgraded, &audit, &root)
            .expect_err("owned capability downgrade must fail")
            .to_string();
    assert!(error.contains("not terminal") || error.contains("terminal totals"));
}

#[test]
fn specification_benchmark_schema_rejects_unknown_and_duplicate_fields() {
    let root = repository_root();
    let source = std::fs::read_to_string(root.join(SPECIFICATION_BENCHMARK_AUDIT_PATH))
        .expect("checked audit must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"threshold_passed\": true,",
        1,
    );
    let error = serde_json::from_str::<SpecificationBenchmarkAudit>(&unknown)
        .expect_err("unknown fields must fail");
    assert!(error.to_string().contains("unknown field"));

    let duplicate = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"schema_version\": 1,",
        1,
    );
    let error = serde_json::from_str::<SpecificationBenchmarkAudit>(&duplicate)
        .expect_err("duplicate fields must fail");
    assert!(error.to_string().contains("duplicate field"));
}
