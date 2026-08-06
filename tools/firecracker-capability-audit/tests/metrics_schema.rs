#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]

use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    AuditMode, METRICS_DEVICE_PRODUCER_AUDIT_PATH, METRICS_PROCESS_PRODUCER_AUDIT_PATH,
    METRICS_SCHEMA_AUTHORITY_PATH, MetricsAggregation, MetricsArchitecture,
    MetricsDeviceProducerAudit, MetricsDeviceProducerBoundary, MetricsDeviceProducerDisposition,
    MetricsDeviceProducerRecord, MetricsProcessProducerAudit, MetricsProcessProducerBoundary,
    MetricsProcessProducerDisposition, MetricsProducerDisposition, MetricsProducerOwner,
    MetricsSchemaAuthority, MetricsUnit, MetricsValueKind, PlatformExclusion, Reference,
    SOURCE_MANIFEST_PATH, metrics_device_producer_audit_json, metrics_process_producer_audit_json,
    metrics_schema_authority_json, read_metrics_device_producer_audit,
    read_metrics_process_producer_audit, read_metrics_schema_authority, read_source_manifest,
    validate_metrics_device_producers, validate_metrics_process_producers, validate_metrics_schema,
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

fn checked_process_audit() -> MetricsProcessProducerAudit {
    let root = repository_root();
    read_metrics_process_producer_audit(&root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH))
        .expect("checked metrics process producer audit must parse")
}

fn checked_device_audit() -> MetricsDeviceProducerAudit {
    let root = repository_root();
    read_metrics_device_producer_audit(&root.join(METRICS_DEVICE_PRODUCER_AUDIT_PATH))
        .expect("checked metrics device producer audit must parse")
}

fn validation_error(authority: &MetricsSchemaAuthority) -> String {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    validate_metrics_schema(authority, &manifest, &root, AuditMode::Delivery)
        .expect_err("mutated metrics authority must fail")
        .to_string()
}

fn process_validation_error(audit: &MetricsProcessProducerAudit) -> String {
    let root = repository_root();
    validate_metrics_process_producers(audit, &checked_authority(), &root, AuditMode::Delivery)
        .expect_err("mutated metrics process producer audit must fail")
        .to_string()
}

fn device_validation_error(audit: &MetricsDeviceProducerAudit) -> String {
    let root = repository_root();
    validate_metrics_device_producers(audit, &checked_authority(), &root, AuditMode::Delivery)
        .expect_err("mutated metrics device producer audit must fail")
        .to_string()
}

fn anchored_local_reference() -> Reference {
    Reference::Local {
        path: "tools/firecracker-capability-audit/src/metrics_process_validate.rs".to_string(),
        anchor: Some("pub fn validate_metrics_process_producers".to_string()),
    }
}

fn implemented_device_record(
    audit: &mut MetricsDeviceProducerAudit,
) -> &mut MetricsDeviceProducerRecord {
    audit
        .records
        .iter_mut()
        .find(|record| {
            record.delivery_issue == "#1845"
                && record.disposition == MetricsDeviceProducerDisposition::Implemented
        })
        .expect("implemented #1845 device record must exist")
}

fn architecture_retained_record<'a>(
    audit: &'a mut MetricsDeviceProducerAudit,
    field_id: &str,
) -> &'a mut MetricsDeviceProducerRecord {
    audit
        .records
        .iter_mut()
        .find(|record| record.field_id == field_id)
        .expect("architecture-retained device record must exist")
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
fn checked_process_producer_audit_is_canonical_and_exact() {
    let root = repository_root();
    let path = root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH);
    let authority = checked_authority();
    let audit = checked_process_audit();
    validate_metrics_process_producers(&audit, &authority, &root, AuditMode::Delivery)
        .expect("checked process producer audit must validate locally");
    assert_eq!(
        std::fs::read(path).expect("checked process producer audit must be readable"),
        metrics_process_producer_audit_json(&audit)
            .expect("process producer audit must serialize canonically")
    );
    assert_eq!(audit.records.len(), 69);
    for (issue, count) in [("#1827", 44), ("#1828", 12), ("#1829", 5), ("#1830", 8)] {
        assert_eq!(
            audit
                .records
                .iter()
                .filter(|record| record.delivery_issue == issue)
                .count(),
            count,
            "{issue}"
        );
    }
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsProcessProducerDisposition::Implemented
            })
            .count(),
        64
    );
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsProcessProducerDisposition::SourceNeutral
            })
            .count(),
        1
    );
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsProcessProducerDisposition::PlatformZero
            })
            .count(),
        4
    );
    for record in audit
        .records
        .iter()
        .filter(|record| record.delivery_issue == "#1829")
    {
        let expected = if record.field_id == "static:logger.metrics_fails" {
            MetricsProcessProducerDisposition::SourceNeutral
        } else {
            MetricsProcessProducerDisposition::Implemented
        };
        assert_eq!(record.disposition, expected, "{}", record.field_id);
    }
    for record in audit
        .records
        .iter()
        .filter(|record| record.delivery_issue == "#1830")
    {
        let expected = if matches!(
            record.field_id.as_str(),
            "static:signals.sighup"
                | "static:signals.sigxcpu"
                | "static:signals.sigxfsz"
                | "static:vmm.panic_count"
        ) {
            MetricsProcessProducerDisposition::Implemented
        } else {
            MetricsProcessProducerDisposition::PlatformZero
        };
        assert_eq!(record.disposition, expected, "{}", record.field_id);
    }

    validate_metrics_process_producers(&audit, &authority, &root, AuditMode::Final)
        .expect("all process producer records should now be terminal");
}

#[test]
fn checked_device_producer_audit_is_canonical_and_exact() {
    let root = repository_root();
    let path = root.join(METRICS_DEVICE_PRODUCER_AUDIT_PATH);
    let authority = checked_authority();
    let audit = checked_device_audit();
    validate_metrics_device_producers(&audit, &authority, &root, AuditMode::Delivery)
        .expect("checked device producer audit must validate locally");
    assert_eq!(
        std::fs::read(path).expect("checked device producer audit must be readable"),
        metrics_device_producer_audit_json(&audit)
            .expect("device producer audit must serialize canonically")
    );
    assert_eq!(audit.records.len(), 231);
    for (issue, count) in [
        ("#1838", 23),
        ("#1839", 38),
        ("#1840", 20),
        ("#1841", 48),
        ("#1842", 5),
        ("#1843", 57),
        ("#1844", 14),
        ("#1845", 11),
        ("#1846", 15),
    ] {
        assert_eq!(
            audit
                .records
                .iter()
                .filter(|record| record.delivery_issue == issue)
                .count(),
            count,
            "{issue}"
        );
    }
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| record.disposition == MetricsDeviceProducerDisposition::Planned)
            .count(),
        0
    );
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsDeviceProducerDisposition::ProvisionalPlatformZero
            })
            .count(),
        0
    );
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| record.disposition == MetricsDeviceProducerDisposition::Implemented)
            .count(),
        212
    );
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsDeviceProducerDisposition::SourceNeutral
            })
            .count(),
        2
    );
    assert_eq!(
        audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsDeviceProducerDisposition::PlatformZero
            })
            .count(),
        17
    );
    for record in audit.records.iter().filter(|record| {
        matches!(
            record.delivery_issue.as_str(),
            "#1838" | "#1839" | "#1840" | "#1841" | "#1842" | "#1843" | "#1844" | "#1845" | "#1846"
        )
    }) {
        let expected = if matches!(
            record.field_id.as_str(),
            "static:uart.flush_count" | "static:mmds.rx_bad_eth"
        ) {
            MetricsDeviceProducerDisposition::SourceNeutral
        } else if matches!(
            record.field_id.as_str(),
            "dynamic:net_{iface_id}.mac_address_updates"
                | "static:net.mac_address_updates"
                | "static:i8042.error_count"
                | "static:i8042.missed_read_count"
                | "static:i8042.missed_write_count"
                | "static:i8042.read_count"
                | "static:i8042.reset_count"
                | "static:i8042.write_count"
                | "static:vcpu.exit_io_in"
                | "static:vcpu.exit_io_in_agg.max_us"
                | "static:vcpu.exit_io_in_agg.min_us"
                | "static:vcpu.exit_io_in_agg.sum_us"
                | "static:vcpu.exit_io_out"
                | "static:vcpu.exit_io_out_agg.max_us"
                | "static:vcpu.exit_io_out_agg.min_us"
                | "static:vcpu.exit_io_out_agg.sum_us"
                | "static:vcpu.kvmclock_ctrl_fails"
        ) {
            MetricsDeviceProducerDisposition::PlatformZero
        } else {
            MetricsDeviceProducerDisposition::Implemented
        };
        assert_eq!(record.disposition, expected, "{}", record.field_id);
        assert!(!record.implementation.is_empty(), "{}", record.field_id);
        assert!(!record.validation.is_empty(), "{}", record.field_id);
        assert_eq!(
            record.platform_exclusion.is_some(),
            expected == MetricsDeviceProducerDisposition::PlatformZero,
            "{}",
            record.field_id
        );
    }
    assert!(
        audit
            .records
            .iter()
            .filter(|record| {
                !matches!(
                    record.delivery_issue.as_str(),
                    "#1838"
                        | "#1839"
                        | "#1840"
                        | "#1841"
                        | "#1842"
                        | "#1843"
                        | "#1844"
                        | "#1845"
                        | "#1846"
                )
            })
            .all(|record| {
                record.implementation.is_empty()
                    && record.validation.is_empty()
                    && record.platform_exclusion.is_none()
            })
    );

    validate_metrics_device_producers(&audit, &authority, &root, AuditMode::Final)
        .expect("all device producer records should now be terminal");
}

// exact #1838 disposition mutation tests, extended through #1840
// exact #1838 disposition mutation tests, extended through #1841
// exact #1838 disposition mutation tests, extended through #1842
// exact #1838 disposition mutation tests, extended through #1843
// exact #1838 disposition mutation tests, extended through #1844
// exact #1838 disposition mutation tests, extended through #1845
// exact #1838 disposition mutation tests, extended through #1846
#[test]
fn device_producer_audit_rejects_completed_child_regression() {
    let mut implemented_regression = checked_device_audit();
    implemented_regression
        .records
        .iter_mut()
        .find(|record| {
            record.delivery_issue == "#1845"
                && record.disposition == MetricsDeviceProducerDisposition::Implemented
        })
        .expect("implemented #1845 record must exist")
        .disposition = MetricsDeviceProducerDisposition::Planned;
    assert!(device_validation_error(&implemented_regression).contains("wrong current disposition"));

    let mut source_neutral_drift = checked_device_audit();
    source_neutral_drift
        .records
        .iter_mut()
        .find(|record| record.field_id == "static:uart.flush_count")
        .expect("UART flush record must exist")
        .disposition = MetricsDeviceProducerDisposition::Implemented;
    assert!(device_validation_error(&source_neutral_drift).contains("wrong current disposition"));

    let mut mmds_source_neutral_drift = checked_device_audit();
    mmds_source_neutral_drift
        .records
        .iter_mut()
        .find(|record| record.field_id == "static:mmds.rx_bad_eth")
        .expect("MMDS rx_bad_eth record must exist")
        .disposition = MetricsDeviceProducerDisposition::Implemented;
    assert!(
        device_validation_error(&mmds_source_neutral_drift).contains("wrong current disposition")
    );

    let mut platform_zero_drift = checked_device_audit();
    let record = architecture_retained_record(&mut platform_zero_drift, "static:vcpu.exit_io_in");
    record.disposition = MetricsDeviceProducerDisposition::Implemented;
    record.platform_exclusion = None;
    assert!(device_validation_error(&platform_zero_drift).contains("wrong current disposition"));
}

#[test]
fn device_producer_audit_rejects_membership_and_order_drift() {
    let mut missing = checked_device_audit();
    let removed = missing.records.remove(0);
    let error = device_validation_error(&missing);
    assert!(error.contains("must contain 231 records"));
    assert!(error.contains(&format!(
        "missing metrics device producer record: {}",
        removed.field_id
    )));

    let mut duplicate = checked_device_audit();
    duplicate.records.push(duplicate.records[0].clone());
    let error = device_validation_error(&duplicate);
    assert!(error.contains("sorted and unique"));
    assert!(error.contains("duplicate metrics device producer record"));

    let mut stale = checked_device_audit();
    stale.records[0].field_id = "static:not.device_owned".to_string();
    let error = device_validation_error(&stale);
    assert!(error.contains("stale or unowned"));
    assert!(error.contains("missing metrics device producer record"));

    let mut order = checked_device_audit();
    order.records.swap(0, 1);
    assert!(device_validation_error(&order).contains("sorted and unique"));

    let mut static_for_dynamic = checked_device_audit();
    let dynamic = static_for_dynamic
        .records
        .iter_mut()
        .find(|record| record.field_id == "dynamic:block_{drive_id}.activate_fails")
        .expect("configured block record must exist");
    dynamic.field_id = "static:block.activate_fails".to_string();
    let error = device_validation_error(&static_for_dynamic);
    assert!(error.contains("duplicate metrics device producer record"));
    assert!(error.contains(
        "missing metrics device producer record: dynamic:block_{drive_id}.activate_fails"
    ));
}

#[test]
fn device_producer_audit_rejects_child_boundary_and_rationale_drift() {
    let mut boundary = checked_device_audit();
    boundary.records[0].boundary = MetricsDeviceProducerBoundary::VcpuFailure;
    assert!(device_validation_error(&boundary).contains("wrong boundary"));

    let mut rationale = checked_device_audit();
    rationale.records[0].rationale.clear();
    assert!(device_validation_error(&rationale).contains("stale rationale"));

    for (field_id, wrong_issue) in [
        ("static:entropy.entropy_bytes", "#1839"),
        ("static:memory_hotplug.plug_count", "#1840"),
        ("static:vsock.conns_added", "#1841"),
        ("dynamic:block_{drive_id}.read_count", "#1842"),
        ("dynamic:vhost_user_block_{drive_id}.init_time_us", "#1843"),
        ("static:mmds.rx_count", "#1844"),
        ("dynamic:net_{iface_id}.tx_count", "#1844"),
        ("static:net.tap_read_fails", "#1843"),
        ("static:vcpu.exit_mmio_read", "#1846"),
        ("static:vcpu.exit_io_in", "#1845"),
    ] {
        let mut audit = checked_device_audit();
        audit
            .records
            .iter_mut()
            .find(|record| record.field_id == field_id)
            .expect("representative device record must exist")
            .delivery_issue = wrong_issue.to_string();
        assert!(
            device_validation_error(&audit).contains("wrong delivery issue"),
            "{field_id}"
        );
    }
}

#[test]
fn device_producer_audit_rejects_disposition_drift() {
    let mut implemented_as_planned = checked_device_audit();
    implemented_as_planned
        .records
        .iter_mut()
        .find(|record| {
            record.delivery_issue == "#1845"
                && record.disposition == MetricsDeviceProducerDisposition::Implemented
        })
        .expect("implemented #1845 device record must exist")
        .disposition = MetricsDeviceProducerDisposition::Planned;
    assert!(device_validation_error(&implemented_as_planned).contains("wrong current disposition"));

    let mut platform_zero_as_planned = checked_device_audit();
    architecture_retained_record(&mut platform_zero_as_planned, "static:i8042.error_count")
        .disposition = MetricsDeviceProducerDisposition::Planned;
    assert!(
        device_validation_error(&platform_zero_as_planned).contains("wrong current disposition")
    );
}

#[test]
fn device_producer_audit_rejects_bad_terminal_evidence() {
    let mut missing = checked_device_audit();
    implemented_device_record(&mut missing)
        .implementation
        .clear();
    assert!(
        device_validation_error(&missing).contains("needs implementation and validation evidence")
    );

    let mut duplicate = checked_device_audit();
    let record = implemented_device_record(&mut duplicate);
    record.implementation.push(record.implementation[0].clone());
    assert!(
        device_validation_error(&duplicate)
            .contains("implementation references must be sorted and unique")
    );

    let mut unsorted = checked_device_audit();
    implemented_device_record(&mut unsorted).implementation = vec![
        Reference::Github {
            url: "https://github.com/seven332/bangbang/issues/1837".to_string(),
        },
        anchored_local_reference(),
    ];
    assert!(
        device_validation_error(&unsorted)
            .contains("implementation references must be sorted and unique")
    );

    let mut unsafe_path = checked_device_audit();
    implemented_device_record(&mut unsafe_path).implementation[0] = Reference::Local {
        path: "../escape.rs".to_string(),
        anchor: Some("anything".to_string()),
    };
    assert!(device_validation_error(&unsafe_path).contains("path escapes repository"));

    let mut missing_anchor = checked_device_audit();
    implemented_device_record(&mut missing_anchor).implementation[0] = Reference::Local {
        path: "tools/firecracker-capability-audit/src/metrics_process_validate.rs".to_string(),
        anchor: None,
    };
    assert!(device_validation_error(&missing_anchor).contains("needs a stable anchor"));

    let mut unresolved_anchor = checked_device_audit();
    implemented_device_record(&mut unresolved_anchor).implementation[0] = Reference::Local {
        path: "tools/firecracker-capability-audit/src/metrics_process_validate.rs".to_string(),
        anchor: Some("this symbol does not exist".to_string()),
    };
    assert!(device_validation_error(&unresolved_anchor).contains("anchor does not resolve"));

    let mut invalid_urls = checked_device_audit();
    implemented_device_record(&mut invalid_urls)
        .implementation
        .push(Reference::Github {
            url: "http://example.invalid/not-github".to_string(),
        });
    assert!(device_validation_error(&invalid_urls).contains("GitHub reference must name an HTTPS"));

    let mut invalid_authority = checked_device_audit();
    implemented_device_record(&mut invalid_authority)
        .implementation
        .push(Reference::Authoritative {
            url: "http://example.invalid/platform".to_string(),
        });
    assert!(
        device_validation_error(&invalid_authority)
            .contains("authoritative reference must name an HTTPS")
    );
}

#[test]
fn device_producer_audit_rejects_invalid_platform_exclusions() {
    let empty_exclusion = || PlatformExclusion {
        upstream_contract: Vec::new(),
        platform_evidence: Vec::new(),
        alternatives: Vec::new(),
        stable_behavior: Vec::new(),
        focused_tests: Vec::new(),
        compatibility_docs: Vec::new(),
        security_docs: Vec::new(),
        challenge: Reference::Github {
            url: "https://github.com/seven332/bangbang/issues/1837".to_string(),
        },
    };

    let mut non_platform_terminal = checked_device_audit();
    implemented_device_record(&mut non_platform_terminal).platform_exclusion =
        Some(empty_exclusion());
    assert!(
        device_validation_error(&non_platform_terminal)
            .contains("non-platform metrics device producer forbids exclusion evidence")
    );

    let mut missing = checked_device_audit();
    architecture_retained_record(&mut missing, "static:i8042.error_count").platform_exclusion =
        None;
    assert!(device_validation_error(&missing).contains("needs structured exclusion evidence"));

    let mut incomplete = checked_device_audit();
    let record = architecture_retained_record(&mut incomplete, "static:i8042.error_count");
    record.platform_exclusion = Some(PlatformExclusion {
        upstream_contract: Vec::new(),
        platform_evidence: Vec::new(),
        alternatives: Vec::new(),
        stable_behavior: Vec::new(),
        focused_tests: Vec::new(),
        compatibility_docs: Vec::new(),
        security_docs: Vec::new(),
        challenge: Reference::Local {
            path: "README.md".to_string(),
            anchor: Some("Bangbang".to_string()),
        },
    });
    let error = device_validation_error(&incomplete);
    assert!(error.contains("platform exclusion upstream_contract must not be empty"));
    assert!(error.contains("alternatives must contain reviewed reasons"));
    assert!(error.contains("challenge must be a GitHub reference"));
}

#[test]
fn device_producer_audit_rejects_architecture_retained_platform_scope_drift() {
    const EXACT_EXCLUSION_ERROR: &str =
        "wrong exact field, target, backend, machine, or Challenge exclusion evidence";

    let mut generic = checked_device_audit();
    architecture_retained_record(&mut generic, "static:i8042.error_count")
        .platform_exclusion
        .as_mut()
        .expect("platform exclusion must exist")
        .upstream_contract = vec![Reference::Authoritative {
        url: "https://developer.apple.com/documentation/hypervisor".to_string(),
    }];
    assert!(device_validation_error(&generic).contains(EXACT_EXCLUSION_ERROR));

    let mut cross_field = checked_device_audit();
    let input_exclusion =
        architecture_retained_record(&mut cross_field, "static:vcpu.exit_io_in_agg.min_us")
            .platform_exclusion
            .clone();
    architecture_retained_record(&mut cross_field, "static:vcpu.exit_io_in_agg.max_us")
        .platform_exclusion = input_exclusion;
    assert!(device_validation_error(&cross_field).contains(EXACT_EXCLUSION_ERROR));

    let mut target = checked_device_audit();
    architecture_retained_record(&mut target, "static:vcpu.exit_io_in")
        .platform_exclusion
        .as_mut()
        .expect("platform exclusion must exist")
        .platform_evidence[0] = Reference::Local {
        path: "crates/hvf/src/backend.rs".to_string(),
        anchor: Some("pub fn validate_pci_support()".to_string()),
    };
    assert!(device_validation_error(&target).contains(EXACT_EXCLUSION_ERROR));

    let mut backend = checked_device_audit();
    architecture_retained_record(&mut backend, "static:vcpu.exit_io_out")
        .platform_exclusion
        .as_mut()
        .expect("platform exclusion must exist")
        .platform_evidence[3] = Reference::Authoritative {
        url: "https://developer.apple.com/documentation/virtualization".to_string(),
    };
    assert!(device_validation_error(&backend).contains(EXACT_EXCLUSION_ERROR));

    let mut machine = checked_device_audit();
    architecture_retained_record(&mut machine, "static:i8042.reset_count")
        .platform_exclusion
        .as_mut()
        .expect("platform exclusion must exist")
        .stable_behavior[2] = Reference::Local {
        path: "crates/hvf/src/startup.rs".to_string(),
        anchor: Some("let power = PsciCpuPowerCoordinator::new(&mpidrs)".to_string()),
    };
    assert!(device_validation_error(&machine).contains(EXACT_EXCLUSION_ERROR));

    let mut nonliteral_i8042 = checked_device_audit();
    architecture_retained_record(&mut nonliteral_i8042, "static:i8042.read_count").implementation
        [3] = Reference::Local {
        path: "crates/runtime/src/metrics/firecracker.rs".to_string(),
        anchor: Some("i8042: I8042Metrics {".to_string()),
    };
    assert!(
        device_validation_error(&nonliteral_i8042)
            .contains("wrong exact target/backend/machine or literal-zero implementation evidence")
    );

    let mut nonliteral_aggregate = checked_device_audit();
    architecture_retained_record(
        &mut nonliteral_aggregate,
        "static:vcpu.exit_io_out_agg.sum_us",
    )
    .implementation[3] = Reference::Local {
        path: "crates/runtime/src/metrics/firecracker.rs".to_string(),
        anchor: Some("exit_io_out_agg: LatencyAggregate {".to_string()),
    };
    assert!(
        device_validation_error(&nonliteral_aggregate)
            .contains("wrong exact target/backend/machine or literal-zero implementation evidence")
    );

    let mut challenge = checked_device_audit();
    architecture_retained_record(&mut challenge, "static:i8042.write_count")
        .platform_exclusion
        .as_mut()
        .expect("platform exclusion must exist")
        .challenge = Reference::Github {
        url: "https://github.com/seven332/bangbang/issues/1844#issuecomment-5202056404".to_string(),
    };
    assert!(device_validation_error(&challenge).contains(EXACT_EXCLUSION_ERROR));
}

#[test]
fn device_producer_audit_rejects_schema_version_and_baseline_drift() {
    let mut schema = checked_device_audit();
    schema.schema_version += 1;
    assert!(device_validation_error(&schema).contains("schema_version must be 1"));

    let mut baseline = checked_device_audit();
    baseline.baseline.commit = "0".repeat(40);
    let error = device_validation_error(&baseline);
    assert!(error.contains("baselines differ"));
    assert!(error.contains("not the pinned release"));
}

#[test]
fn process_producer_audit_rejects_membership_and_order_drift() {
    let mut missing = checked_process_audit();
    let removed = missing.records.remove(0);
    let error = process_validation_error(&missing);
    assert!(error.contains("must contain 69 records"));
    assert!(error.contains(&format!(
        "missing metrics process producer record: {}",
        removed.field_id
    )));

    let mut duplicate = checked_process_audit();
    duplicate.records.push(duplicate.records[0].clone());
    let error = process_validation_error(&duplicate);
    assert!(error.contains("sorted and unique"));
    assert!(error.contains("duplicate metrics process producer record"));

    let mut stale = checked_process_audit();
    stale.records[0].field_id = "static:not.process_owned".to_string();
    let error = process_validation_error(&stale);
    assert!(error.contains("stale or unowned"));
    assert!(error.contains("missing metrics process producer record"));

    let mut order = checked_process_audit();
    order.records.swap(0, 1);
    assert!(process_validation_error(&order).contains("sorted and unique"));
}

#[test]
fn process_producer_audit_rejects_boundary_child_and_completion_drift() {
    let mut boundary = checked_process_audit();
    boundary.records[0].boundary = MetricsProcessProducerBoundary::PanicLifecycle;
    assert!(process_validation_error(&boundary).contains("wrong boundary"));

    let mut child = checked_process_audit();
    child.records[0].delivery_issue = "#1830".to_string();
    assert!(process_validation_error(&child).contains("wrong delivery issue"));

    let mut completed = checked_process_audit();
    let api = completed
        .records
        .iter_mut()
        .find(|record| record.delivery_issue == "#1827")
        .expect("API record must exist");
    api.disposition = MetricsProcessProducerDisposition::Planned;
    api.implementation.clear();
    api.validation.clear();
    let error = process_validation_error(&completed);
    assert!(error.contains("completed metrics process producer slice must be terminal"));
    assert!(error.contains("completed process metrics producer must be implemented"));

    let mut completed_latency = checked_process_audit();
    let latency = completed_latency
        .records
        .iter_mut()
        .find(|record| record.delivery_issue == "#1828")
        .expect("latency record must exist");
    latency.disposition = MetricsProcessProducerDisposition::Planned;
    latency.implementation.clear();
    latency.validation.clear();
    let error = process_validation_error(&completed_latency);
    assert!(error.contains("completed metrics process producer slice must be terminal"));
    assert!(error.contains("completed process metrics producer must be implemented"));

    let mut incomplete = checked_process_audit();
    let downstream = incomplete
        .records
        .iter_mut()
        .find(|record| record.delivery_issue == "#1830")
        .expect("downstream record must exist");
    downstream.disposition = MetricsProcessProducerDisposition::Planned;
    downstream.implementation.clear();
    downstream.validation.clear();
    let error = process_validation_error(&incomplete);
    assert!(error.contains("completed metrics process producer slice must be terminal"));
    assert!(error.contains("completed #1830 process metric has the wrong exact disposition"));
}

#[test]
fn process_producer_audit_rejects_neutral_aliases_and_bad_evidence() {
    let mut neutral = checked_process_audit();
    let api = neutral
        .records
        .iter_mut()
        .find(|record| record.delivery_issue == "#1827")
        .expect("API record must exist");
    api.disposition = MetricsProcessProducerDisposition::SourceNeutral;
    assert!(
        process_validation_error(&neutral)
            .contains("completed process metrics producer must be implemented")
    );

    let mut fabricated_metrics_fails_producer = checked_process_audit();
    fabricated_metrics_fails_producer
        .records
        .iter_mut()
        .find(|record| record.field_id == "static:logger.metrics_fails")
        .expect("logger metrics_fails record must exist")
        .disposition = MetricsProcessProducerDisposition::Implemented;
    assert!(
        process_validation_error(&fabricated_metrics_fails_producer)
            .contains("completed #1829 process metric has the wrong exact disposition")
    );

    let mut neutral_missed_log = checked_process_audit();
    neutral_missed_log
        .records
        .iter_mut()
        .find(|record| record.field_id == "static:logger.missed_log_count")
        .expect("missed log record must exist")
        .disposition = MetricsProcessProducerDisposition::SourceNeutral;
    assert!(
        process_validation_error(&neutral_missed_log)
            .contains("completed #1829 process metric has the wrong exact disposition")
    );

    for field_id in [
        "static:seccomp.num_faults",
        "static:signals.sigbus",
        "static:signals.sighup",
        "static:signals.sigill",
        "static:signals.sigsegv",
        "static:signals.sigxcpu",
        "static:signals.sigxfsz",
        "static:vmm.panic_count",
    ] {
        let mut swapped = checked_process_audit();
        let record = swapped
            .records
            .iter_mut()
            .find(|record| record.field_id == field_id)
            .expect("#1830 record must exist");
        record.disposition = match record.disposition {
            MetricsProcessProducerDisposition::Implemented => {
                MetricsProcessProducerDisposition::PlatformZero
            }
            MetricsProcessProducerDisposition::PlatformZero => {
                MetricsProcessProducerDisposition::Implemented
            }
            _ => panic!("#1830 records must have an exact terminal disposition"),
        };
        assert!(
            process_validation_error(&swapped)
                .contains("completed #1830 process metric has the wrong exact disposition"),
            "{field_id}"
        );
    }

    let mut missing = checked_process_audit();
    missing
        .records
        .iter_mut()
        .find(|record| record.disposition == MetricsProcessProducerDisposition::Implemented)
        .expect("implemented record must exist")
        .implementation
        .clear();
    assert!(
        process_validation_error(&missing).contains("needs implementation and validation evidence")
    );

    let mut unsafe_reference = checked_process_audit();
    let implemented = unsafe_reference
        .records
        .iter_mut()
        .find(|record| record.disposition == MetricsProcessProducerDisposition::Implemented)
        .expect("implemented record must exist");
    implemented.implementation[0] = Reference::Local {
        path: "../escape".to_string(),
        anchor: None,
    };
    assert!(process_validation_error(&unsafe_reference).contains("path escapes repository"));

    let mut rationale = checked_process_audit();
    rationale.records[0].rationale.clear();
    assert!(process_validation_error(&rationale).contains("stale rationale"));

    let mut baseline = checked_process_audit();
    baseline.baseline.commit = "0".repeat(40);
    let error = process_validation_error(&baseline);
    assert!(error.contains("baselines differ"));
    assert!(error.contains("not the pinned release"));
}

#[test]
fn checked_metrics_authority_has_exact_terminal_producer_partition() {
    let authority = checked_authority();
    let count = |owner, disposition| {
        authority
            .policy_profiles
            .iter()
            .filter(|profile| {
                profile.producer_owner == owner && profile.producer_disposition == disposition
            })
            .count()
    };
    assert_eq!(
        count(
            MetricsProducerOwner::SchemaRuntime,
            MetricsProducerDisposition::Implemented,
        ),
        1
    );
    assert_eq!(
        count(
            MetricsProducerOwner::ProcessLifecycle,
            MetricsProducerDisposition::Implemented,
        ),
        2
    );
    assert_eq!(
        count(
            MetricsProducerOwner::Device,
            MetricsProducerDisposition::Implemented,
        ),
        10
    );
    assert_eq!(
        count(
            MetricsProducerOwner::Device,
            MetricsProducerDisposition::PlatformZero,
        ),
        0
    );
    assert_eq!(
        count(
            MetricsProducerOwner::Device,
            MetricsProducerDisposition::Planned,
        ),
        0
    );
    assert_eq!(authority.policy_profiles.len(), 13);
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    validate_metrics_schema(&authority, &manifest, &root, AuditMode::Final)
        .expect("terminal metrics producer profiles must pass final schema validation");
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
        .find(|profile| {
            profile.producer_owner == MetricsProducerOwner::Device
                && profile.producer_disposition == MetricsProducerDisposition::Implemented
        })
        .expect("implemented device profile must exist");
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
        .find(|profile| profile.producer_owner == MetricsProducerOwner::Device)
        .expect("implemented device profile must exist");
    profile.implementation[0] = Reference::Local {
        path: "../escape".to_string(),
        anchor: None,
    };
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
