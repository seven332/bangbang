use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    AuditMode, METRICS_LIFECYCLE_AUDIT_PATH, METRICS_LIFECYCLE_SCENARIO_IDS,
    METRICS_SCHEMA_AUTHORITY_PATH, MetricsLifecycleBoundary, MetricsLifecycleClaim,
    MetricsLifecycleDisposition, Reference, metrics_lifecycle_audit_json,
    read_metrics_lifecycle_audit, read_metrics_schema_authority, validate_metrics_lifecycle,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_lifecycle_matrix_is_exact_and_fail_closed() {
    let root = repository_root();
    let authority = read_metrics_schema_authority(&root.join(METRICS_SCHEMA_AUTHORITY_PATH))
        .expect("checked metrics schema authority must parse");
    let audit = read_metrics_lifecycle_audit(&root.join(METRICS_LIFECYCLE_AUDIT_PATH))
        .expect("checked metrics lifecycle audit must parse");

    assert_eq!(
        metrics_lifecycle_audit_json(&audit)
            .expect("metrics lifecycle audit must serialize canonically"),
        std::fs::read(root.join(METRICS_LIFECYCLE_AUDIT_PATH))
            .expect("checked metrics lifecycle audit must be readable")
    );

    validate_metrics_lifecycle(&audit, &authority, &root, AuditMode::Final)
        .expect("checked metrics lifecycle matrix must be terminal");
    assert_eq!(audit.records.len(), METRICS_LIFECYCLE_SCENARIO_IDS.len());
    assert_eq!(
        audit
            .records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        METRICS_LIFECYCLE_SCENARIO_IDS
    );

    let error_for = |candidate| {
        validate_metrics_lifecycle(candidate, &authority, &root, AuditMode::Final)
            .expect_err("mutated lifecycle matrix must fail closed")
            .to_string()
    };

    let mut missing = audit.clone();
    missing.records.pop();
    let error = error_for(&missing);
    assert!(error.contains("must contain 10 records"));
    assert!(error.contains("exact scenario set"));

    let mut reordered = audit.clone();
    reordered.records.swap(0, 1);
    assert!(error_for(&reordered).contains("sorted and unique"));

    let mut boundary = audit.clone();
    boundary.records[0].boundary = MetricsLifecycleBoundary::ExplicitFlush;
    assert!(error_for(&boundary).contains("wrong boundary"));

    let mut disposition = audit.clone();
    disposition.records[0].disposition = MetricsLifecycleDisposition::Planned;
    let error = error_for(&disposition);
    assert!(error.contains("wrong disposition"));
    assert!(error.contains("planned metrics lifecycle scenario must not claim terminal evidence"));
    assert!(error.contains("final metrics lifecycle validation rejects planned record"));

    let mut claim = audit.clone();
    claim.records[0].claims = vec![MetricsLifecycleClaim::FinalAttemptOnce];
    let error = error_for(&claim);
    assert!(error.contains("stale claims"));
    assert!(error.contains("combined transaction claims must be owned only"));

    let mut rationale = audit.clone();
    rationale.records[0].rationale.push_str(" stale");
    assert!(error_for(&rationale).contains("stale rationale"));

    let mut issue = audit.clone();
    issue.records[0].delivery_issue = "#1789".to_string();
    assert!(error_for(&issue).contains("wrong delivery issue"));

    let mut evidence = audit.clone();
    evidence.records[0].validation[0] = Reference::Local {
        path: "crates/bangbang/src/main.rs".to_string(),
        anchor: Some("fn missing_metrics_lifecycle_anchor".to_string()),
    };
    let error = error_for(&evidence);
    assert!(error.contains("requires exact validation evidence"));
    assert!(error.contains("evidence anchor does not resolve"));
}

#[test]
fn lifecycle_model_rejects_unknown_values() {
    let error =
        serde_json::from_str::<bangbang_firecracker_capability_audit::MetricsLifecycleRecord>(
            r##"{
            "id":"metrics.test",
            "boundary":"future-trigger",
            "disposition":"implemented",
            "delivery_issue":"#1790",
            "claims":["process-isolation"],
            "rationale":"test"
        }"##,
        )
        .expect_err("unknown lifecycle boundary must fail parsing");
    assert!(error.to_string().contains("unknown variant"));
}
