use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    CAPABILITY_INVENTORY_PATH, Disposition, Reference, SOURCE_MANIFEST_PATH,
    WAVE7_AGGREGATE_AUDIT_PATH, WAVE7_AGGREGATE_CAPABILITY_IDS, WAVE7_OWNED_CAPABILITY_IDS,
    WAVE7_PLATFORM_IMPOSSIBLE_CAPABILITY_IDS, Wave7AggregateAudit, Wave7AggregateNonclaim,
    Wave7HandoffOwner, read_capability_inventory, read_source_manifest, read_wave7_aggregate_audit,
    validate_wave7_aggregate_audit, validate_wave7_aggregate_compatibility,
    wave7_aggregate_audit_json,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let path = root.join(WAVE7_AGGREGATE_AUDIT_PATH);
    let audit = read_wave7_aggregate_audit(&path).expect("checked Wave 7 audit must parse");
    let canonical =
        wave7_aggregate_audit_json(&audit).expect("checked Wave 7 audit must serialize");
    assert_eq!(
        canonical,
        std::fs::read(path).expect("audit must be readable")
    );
    validate_wave7_aggregate_audit(&audit, &manifest, &inventory, &root)
        .expect("checked Wave 7 aggregate must validate");

    let mut source_drift = audit.clone();
    source_drift.upstream_sources[0].git_blob = "0".repeat(40);
    assert!(
        validate_wave7_aggregate_audit(&source_drift, &manifest, &inventory, &root)
            .expect_err("source blob drift must fail")
            .to_string()
            .contains("exact ordered pinned sources")
    );

    let mut design_reorder = audit.clone();
    design_reorder.design.swap(0, 1);
    assert!(
        validate_wave7_aggregate_audit(&design_reorder, &manifest, &inventory, &root)
            .expect_err("design reorder must fail")
            .to_string()
            .contains("exact ordered 37-record partition")
    );
    let mut design_duplicate = audit.clone();
    design_duplicate.design[1] = design_duplicate.design[0].clone();
    let error = validate_wave7_aggregate_audit(&design_duplicate, &manifest, &inventory, &root)
        .expect_err("duplicate design identity must fail")
        .to_string();
    assert!(error.contains("duplicate semantic") || error.contains("bijection"));

    let mut relation_missing = audit.clone();
    relation_missing.device_api.required_relations.pop();
    assert!(
        validate_wave7_aggregate_audit(&relation_missing, &manifest, &inventory, &root)
            .expect_err("missing required device relation must fail")
            .to_string()
            .contains("958/62/896")
    );
    let mut alias_drift = audit.clone();
    alias_drift.device_api.normalizations[0].current = "CreateSnapshotParams".to_string();
    assert!(
        validate_wave7_aggregate_audit(&alias_drift, &manifest, &inventory, &root)
            .expect_err("device API alias drift must fail")
            .to_string()
            .contains("normalizations drifted")
    );

    let mut release_collapse = audit.clone();
    release_collapse.release_entries.remove(18);
    let error = validate_wave7_aggregate_audit(&release_collapse, &manifest, &inventory, &root)
        .expect_err("collapsed #5818 release entry must fail")
        .to_string();
    assert!(error.contains("9 Added and 12 Fixed") || error.contains("two independent #5818"));

    let mut tool_count = audit.clone();
    tool_count.tools[2].counts.audit_handoff_1373 -= 1;
    assert!(
        validate_wave7_aggregate_audit(&tool_count, &manifest, &inventory, &root)
            .expect_err("tool count drift must fail")
            .to_string()
            .contains("tool metadata drifted")
    );
    let mut tool_evidence = audit.clone();
    tool_evidence.tools[0].evidence[0] = Reference::Local {
        path: "tools/seccompiler/tests/cli.rs".to_string(),
        anchor: Some("fn help_and_version_identify_the_offline_compatibility_tool()".to_string()),
    };
    assert!(
        validate_wave7_aggregate_audit(&tool_evidence, &manifest, &inventory, &root)
            .expect_err("valid but unrelated tool evidence must fail")
            .to_string()
            .contains("must match its exact path and anchor set")
    );

    let mut mmio_substitution = audit.clone();
    mmio_substitution.virtio_mmio.pci_evidence_may_substitute = true;
    assert!(
        validate_wave7_aggregate_audit(&mmio_substitution, &manifest, &inventory, &root)
            .expect_err("PCI substitution must fail")
            .to_string()
            .contains("PCI-only")
    );
    let mut mmio_device = audit.clone();
    mmio_device.virtio_mmio.devices.pop();
    assert!(
        validate_wave7_aggregate_audit(&mmio_device, &manifest, &inventory, &root)
            .expect_err("missing MMIO device must fail")
            .to_string()
            .contains("device profiles drifted")
    );
    let mut mmio_evidence = audit.clone();
    mmio_evidence.virtio_mmio.evidence.focused[0] = Reference::Local {
        path: "crates/runtime/src/virtio_mmio.rs".to_string(),
        anchor: Some("pub struct VirtioMmioRegisterHandler".to_string()),
    };
    assert!(
        validate_wave7_aggregate_audit(&mmio_evidence, &manifest, &inventory, &root)
            .expect_err("valid production evidence cannot replace focused evidence")
            .to_string()
            .contains("must match its exact path and anchor set")
    );

    let mut handoff_owner = audit.clone();
    handoff_owner.handoffs[9].owner = Wave7HandoffOwner::Issue1373;
    assert!(
        validate_wave7_aggregate_audit(&handoff_owner, &manifest, &inventory, &root)
            .expect_err("wrong handoff owner must fail")
            .to_string()
            .contains("exact ordered nine audit and three feasible handoffs")
    );

    let mut stale_anchor = audit.clone();
    stale_anchor.evidence.implementation[0] = Reference::Local {
        path: WAVE7_AGGREGATE_AUDIT_PATH.to_string(),
        anchor: Some("missing Wave 7 anchor".to_string()),
    };
    assert!(
        validate_wave7_aggregate_audit(&stale_anchor, &manifest, &inventory, &root)
            .expect_err("stale evidence anchor must fail")
            .to_string()
            .contains("anchor is absent")
    );
    let mut aggregate_evidence = audit.clone();
    aggregate_evidence.evidence.validation[0] = Reference::Local {
        path: "tools/seccompiler/tests/cli.rs".to_string(),
        anchor: Some("fn help_and_version_identify_the_offline_compatibility_tool()".to_string()),
    };
    assert!(
        validate_wave7_aggregate_audit(&aggregate_evidence, &manifest, &inventory, &root)
            .expect_err("valid but unrelated aggregate evidence must fail")
            .to_string()
            .contains("must match its exact path and anchor set")
    );

    let mut nonclaim_drift = audit;
    nonclaim_drift.nonclaims = vec![Wave7AggregateNonclaim::Wave8InteractionCompletion];
    assert!(
        validate_wave7_aggregate_audit(&nonclaim_drift, &manifest, &inventory, &root)
            .expect_err("nonclaim drift must fail")
            .to_string()
            .contains("exact ordered nonclaims")
    );
}

#[test]
fn wave7_terminal_distribution_and_transition_are_exact() {
    let root = repository_root();
    let manifest = read_source_manifest(&root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let audit = read_wave7_aggregate_audit(&root.join(WAVE7_AGGREGATE_AUDIT_PATH))
        .expect("checked Wave 7 audit must parse");

    assert_eq!(WAVE7_AGGREGATE_CAPABILITY_IDS.len(), 5);
    assert_eq!(WAVE7_OWNED_CAPABILITY_IDS.len(), 93);
    assert_eq!(WAVE7_PLATFORM_IMPOSSIBLE_CAPABILITY_IDS.len(), 13);
    validate_wave7_aggregate_compatibility(&manifest, &inventory, &audit, &root)
        .expect("terminal Wave 7 aggregate must certify");

    let mut historical = inventory.clone();
    for id in ["tool-argument:jailer/gid", "tool-argument:jailer/uid"] {
        let capability = historical
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("post-Wave 8 jailer capability must exist");
        capability.disposition = Disposition::AuditRequired;
        capability.exclusion = None;
    }
    let wave8 = historical
        .capabilities
        .iter_mut()
        .find(|capability| {
            capability.id == "semantic.cross-capability:state-errors-metrics-security-and-snapshots"
        })
        .expect("Wave 8 successor capability must exist");
    wave8.disposition = Disposition::AuditRequired;
    wave8.implementation.clear();
    wave8.validation.clear();
    validate_wave7_aggregate_compatibility(&manifest, &historical, &audit, &root)
        .expect("historical 376/9/3/30 Wave 7 phase must remain valid");

    for id in WAVE7_AGGREGATE_CAPABILITY_IDS {
        let mut downgraded = inventory.clone();
        let capability = downgraded
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("owned aggregate capability must exist");
        capability.disposition = Disposition::AuditRequired;
        capability.implementation.clear();
        capability.validation.clear();
        let error = validate_wave7_aggregate_compatibility(&manifest, &downgraded, &audit, &root)
            .expect_err("every partial #1799 transition must fail")
            .to_string();
        assert!(
            error.contains("376/9/3/30") || error.contains("not terminal"),
            "{id}: {error}"
        );

        let mut evidence_drifted = inventory.clone();
        let capability = evidence_drifted
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == id)
            .expect("owned aggregate capability must exist");
        capability.validation[0] = Reference::Local {
            path: "tools/firecracker-capability-audit/tests/wave7_aggregate_audit.rs".to_string(),
            anchor: Some("fn wave7_terminal_distribution_and_transition_are_exact()".to_string()),
        };
        let error =
            validate_wave7_aggregate_compatibility(&manifest, &evidence_drifted, &audit, &root)
                .expect_err("valid but inexact aggregate capability evidence must fail")
                .to_string();
        assert!(
            error.contains("validation evidence drifted"),
            "{id}: {error}"
        );
    }

    let mut unrelated = inventory;
    let capability = unrelated
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:logger")
        .expect("unrelated capability must exist");
    capability.delivery_issue = Some("#1799".to_string());
    assert!(
        validate_wave7_aggregate_compatibility(&manifest, &unrelated, &audit, &root)
            .expect_err("unrelated #1799 ownership must fail")
            .to_string()
            .contains("unrelated #1799 ownership")
    );
}

#[test]
fn wave7_schema_rejects_unknown_duplicate_and_open_values() {
    let source = std::fs::read_to_string(repository_root().join(WAVE7_AGGREGATE_AUDIT_PATH))
        .expect("checked Wave 7 audit must be readable");
    let unknown = source.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1,\n  \"all_done\": true,",
        1,
    );
    assert!(
        serde_json::from_str::<Wave7AggregateAudit>(&unknown)
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
        serde_json::from_str::<Wave7AggregateAudit>(&duplicate)
            .expect_err("duplicate fields must fail")
            .to_string()
            .contains("duplicate field")
    );

    let open_value = source.replacen("\"scope-and-features\"", "\"everything-else\"", 1);
    assert!(
        serde_json::from_str::<Wave7AggregateAudit>(&open_value)
            .expect_err("open design section must fail")
            .to_string()
            .contains("unknown variant")
    );
}
