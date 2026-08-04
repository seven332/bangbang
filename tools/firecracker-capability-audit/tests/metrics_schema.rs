#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]

use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    AuditMode, METRICS_SCHEMA_AUTHORITY_PATH, MetricsAggregation, MetricsArchitecture,
    MetricsProducerDisposition, MetricsSchemaAuthority, MetricsUnit, MetricsValueKind, Reference,
    SOURCE_MANIFEST_PATH, metrics_schema_authority_json, read_metrics_schema_authority,
    read_source_manifest, validate_metrics_schema,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root must resolve")
}

fn checked_authority() -> MetricsSchemaAuthority {
    let root = repository_root();
    read_metrics_schema_authority(&root.join(METRICS_SCHEMA_AUTHORITY_PATH))
        .expect("checked metrics authority must parse")
}

fn validation_error(authority: &MetricsSchemaAuthority) -> String {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    validate_metrics_schema(authority, &manifest, &root, AuditMode::Delivery)
        .expect_err("mutated metrics authority must fail")
        .to_string()
}

#[test]
fn checked_metrics_authority_is_canonical_and_valid_without_a_sibling() {
    let root = repository_root();
    let path = root.join(METRICS_SCHEMA_AUTHORITY_PATH);
    let authority = checked_authority();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    validate_metrics_schema(&authority, &manifest, &root, AuditMode::Delivery)
        .expect("checked metrics authority must validate locally");
    assert_eq!(
        std::fs::read(path).expect("checked authority must be readable"),
        metrics_schema_authority_json(&authority).expect("authority must serialize canonically")
    );
    assert_eq!(authority.source.counts.static_roots, 24);
    assert_eq!(authority.source.counts.static_fields, 243);
    assert_eq!(authority.source.fields.len(), 301);
    assert_eq!(authority.source.dynamic_families[0].field_ids.len(), 24);
    assert_eq!(authority.source.dynamic_families[1].field_ids.len(), 29);
    assert_eq!(authority.source.dynamic_families[2].field_ids.len(), 5);
}

#[test]
fn checked_metrics_authority_keeps_all_producers_nonterminal() {
    let authority = checked_authority();
    assert!(authority.policy_profiles.iter().all(|profile| matches!(
        profile.producer_disposition,
        MetricsProducerDisposition::Planned | MetricsProducerDisposition::PlatformZero
    )));
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let error = validate_metrics_schema(&authority, &manifest, &root, AuditMode::Final)
        .expect_err("planned metrics producers must fail final mode");
    assert!(error.to_string().contains("nonterminal producer policy"));
}

#[test]
fn rejects_missing_duplicate_and_stale_field_identities() {
    let mut missing = checked_authority();
    let removed = missing.source.fields.remove(0);
    let error = validation_error(&missing);
    assert!(error.contains("field count"));
    assert!(error.contains(&format!("stale metrics field policy: {}", removed.id)));

    let mut duplicate = checked_authority();
    duplicate
        .source
        .fields
        .push(duplicate.source.fields[0].clone());
    let error = validation_error(&duplicate);
    assert!(error.contains("duplicate metrics field id"));
    assert!(error.contains("duplicate metrics field path"));

    let mut stale = checked_authority();
    stale.field_policies[0].field_id = "static:not.a.real.field".to_string();
    let error = validation_error(&stale);
    assert!(error.contains("stale metrics field policy"));
    assert!(error.contains("missing metrics field policy"));
}

#[test]
fn rejects_field_order_drift_even_when_the_identity_set_is_unchanged() {
    let mut authority = checked_authority();
    authority.source.fields.swap(0, 1);
    assert!(validation_error(&authority).contains("canonical wire order"));
}

#[test]
fn rejects_extra_and_duplicate_field_policies() {
    let mut extra = checked_authority();
    let mut policy = extra.field_policies[0].clone();
    policy.field_id = "static:extra.field".to_string();
    extra.field_policies.push(policy);
    let error = validation_error(&extra);
    assert!(error.contains("stale metrics field policy"));

    let mut duplicate = checked_authority();
    duplicate
        .field_policies
        .push(duplicate.field_policies[0].clone());
    let error = validation_error(&duplicate);
    assert!(error.contains("duplicate metrics field policy"));
}

#[test]
fn rejects_unknown_json_types_during_strict_parsing() {
    let authority = checked_authority();
    let mut value = serde_json::to_value(authority).expect("authority must become JSON");
    value["source"]["fields"][0]["json_type"] = serde_json::Value::String("string".to_string());
    let error = serde_json::from_value::<MetricsSchemaAuthority>(value)
        .expect_err("open-ended JSON types must fail")
        .to_string();
    assert!(error.contains("unknown variant"));
}

#[test]
fn rejects_wrong_reset_unit_and_aggregation_semantics() {
    let mut reset = checked_authority();
    let field = reset
        .source
        .fields
        .iter_mut()
        .find(|field| field.rust_type == "SharedIncMetric")
        .expect("incremental field must exist");
    field.value_kind = MetricsValueKind::PersistentStore;
    assert!(validation_error(&reset).contains("reset/value kind"));

    let mut unit = checked_authority();
    let profile = unit
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.unit == MetricsUnit::Bytes)
        .expect("byte profile must exist");
    profile.unit = MetricsUnit::Count;
    let error = validation_error(&unit);
    assert!(error.contains("wrong unit"));
    assert!(error.contains("profile id must encode"));

    let mut aggregation = checked_authority();
    let profile = aggregation
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.aggregation == MetricsAggregation::SumAcrossConfiguredDevices)
        .expect("device aggregate profile must exist");
    profile.aggregation = MetricsAggregation::None;
    assert!(validation_error(&aggregation).contains("wrong aggregation policy"));
}

#[test]
fn rejects_dynamic_grammar_and_architecture_drift() {
    let mut dynamic = checked_authority();
    dynamic.source.dynamic_families[2].producer_template = "vhost_user_{drive_id}".to_string();
    dynamic.source.dynamic_families[2].field_ids[0] =
        "dynamic:vhost_user_{drive_id}.activate_fails".to_string();
    let error = validation_error(&dynamic);
    assert!(error.contains("dynamic family contract is wrong"));
    assert!(error.contains("malformed template identity"));

    let mut architecture = checked_authority();
    let rtc = architecture
        .source
        .static_roots
        .iter_mut()
        .find(|root| root.name == "rtc")
        .expect("rtc root must exist");
    rtc.architecture = MetricsArchitecture::All;
    assert!(validation_error(&architecture).contains("root architecture is wrong"));
}

#[test]
fn rejects_disposition_issue_and_rationale_drift() {
    let mut disposition = checked_authority();
    let profile = disposition
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.producer_disposition == MetricsProducerDisposition::Planned)
        .expect("planned profile must exist");
    profile.producer_disposition = MetricsProducerDisposition::PlatformZero;
    assert!(validation_error(&disposition).contains("wrong platform producer disposition"));

    let mut issue = checked_authority();
    issue.policy_profiles[0].delivery_issue = Some("#9999".to_string());
    assert!(validation_error(&issue).contains("wrong delivery issue"));

    let mut rationale = checked_authority();
    rationale.policy_profiles[0].rationale.clear();
    assert!(validation_error(&rationale).contains("stale rationale"));
}

#[test]
fn rejects_unresolved_or_unsafe_implementation_evidence() {
    let mut authority = checked_authority();
    let profile = authority
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.producer_disposition == MetricsProducerDisposition::Planned)
        .expect("planned profile must exist");
    profile.producer_disposition = MetricsProducerDisposition::Implemented;
    profile.delivery_issue = None;
    profile.rationale =
        "The producer has exact implementation and validation evidence for this checked field policy."
            .to_string();
    profile.implementation.push(Reference::Local {
        path: "../escape".to_string(),
        anchor: None,
    });
    profile.validation.push(Reference::Local {
        path: "../escape".to_string(),
        anchor: None,
    });
    let error = validation_error(&authority);
    assert!(error.contains("path escapes repository"));
}

#[test]
fn rejects_input_source_and_anchor_fingerprint_drift() {
    let mut input = checked_authority();
    input.source.inputs[0].git_blob = "0".repeat(39);
    assert!(validation_error(&input).contains("git_blob"));

    let mut fingerprint = checked_authority();
    fingerprint.source.fields[0].producer_anchor.fingerprint = "sha256:not-hex".to_string();
    assert!(validation_error(&fingerprint).contains("64 lowercase hex"));

    let mut anchor = checked_authority();
    anchor.source.fields[0].producer_anchor.path = "../outside.rs".to_string();
    let error = validation_error(&anchor);
    assert!(error.contains("untracked input"));
    assert!(error.contains("path is unsafe"));
}

#[test]
fn rejects_source_manifest_and_population_count_drift() {
    let root = repository_root();
    let authority = checked_authority();
    let mut manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    manifest.items.retain(|item| item.id != "corpus:metrics");
    let error = validate_metrics_schema(&authority, &manifest, &root, AuditMode::Delivery)
        .expect_err("missing corpus authority must fail")
        .to_string();
    assert!(error.contains("missing corpus:metrics"));

    let mut count = checked_authority();
    count.source.counts.net_dynamic_fields -= 1;
    assert!(validation_error(&count).contains("net_dynamic_fields must be 29"));
}
