use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    AuditMode, CAPABILITY_INVENTORY_PATH, CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
    CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS, CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS,
    Disposition, LOGGER_COMPATIBILITY_CAPABILITY_IDS, LOGGER_PRODUCER_AUDIT_PATH,
    LOGGER_PRODUCER_MANIFEST_PATH, LoggerClassDisposition, LoggerCompiledEvent,
    LoggerDeliveryPolicy, LoggerNonApplicableReason, METRICS_AGGREGATE_CAPABILITY_IDS,
    METRICS_DEVICE_PRODUCER_AUDIT_PATH, METRICS_LIFECYCLE_AUDIT_PATH,
    METRICS_PROCESS_PRODUCER_AUDIT_PATH, METRICS_SCHEMA_AUTHORITY_PATH,
    METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS, MetricsDeviceProducerDisposition,
    MetricsProcessProducerDisposition, MetricsProducerDisposition, MetricsProducerOwner, Reference,
    SOURCE_MANIFEST_PATH, TERMINAL_DEVICE_POLICY_PROFILE_IDS, TRACING_AUDIT_PATH,
    TRACING_CALL_SITE_IDS, TRACING_COMPATIBILITY_CAPABILITY_IDS, logger_producer_audit_json,
    logger_producer_manifest_json, read_capability_inventory, read_logger_producer_audit,
    read_logger_producer_manifest, read_metrics_device_producer_audit,
    read_metrics_lifecycle_audit, read_metrics_process_producer_audit,
    read_metrics_schema_authority, read_source_manifest, read_tracing_audit, source_manifest_json,
    tracing_audit_json, validate, validate_cpu_template_helper_compatibility,
    validate_cpu_template_helper_transition, validate_cpu_template_strip_compatibility,
    validate_logger_compatibility, validate_logger_producers, validate_metrics_compatibility,
    validate_metrics_device_compatibility, validate_metrics_device_producers,
    validate_metrics_process_compatibility, validate_metrics_schema_compatibility,
    validate_tracing_audit, validate_tracing_compatibility,
};

#[test]
fn checked_inventory_is_valid_for_delivery() {
    let tool_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = tool_root
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory");
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");

    validate(&manifest, &inventory, repository_root, AuditMode::Delivery)
        .expect("checked inventory must satisfy delivery-time invariants");
}

#[test]
fn checked_source_manifest_is_canonical_and_deterministic() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let path = repository_root.join(SOURCE_MANIFEST_PATH);
    let checked_bytes = std::fs::read(&path).expect("checked source manifest must be readable");
    let manifest = read_source_manifest(&path).expect("checked source manifest must parse");

    let first = source_manifest_json(&manifest).expect("source manifest must serialize");
    let second = source_manifest_json(&manifest).expect("source manifest must serialize again");
    assert_eq!(
        first, second,
        "canonical serialization must be deterministic"
    );
    assert_eq!(
        first, checked_bytes,
        "checked source manifest must use canonical serialization"
    );
}

#[test]
fn checked_logger_artifacts_are_canonical_and_valid_for_delivery() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest_path = repository_root.join(LOGGER_PRODUCER_MANIFEST_PATH);
    let audit_path = repository_root.join(LOGGER_PRODUCER_AUDIT_PATH);
    let manifest = read_logger_producer_manifest(&manifest_path)
        .expect("checked logger producer manifest must parse");
    let audit =
        read_logger_producer_audit(&audit_path).expect("checked logger producer audit must parse");

    validate_logger_producers(&manifest, &audit, &repository_root, AuditMode::Delivery)
        .expect("checked logger artifacts must satisfy delivery-time invariants");

    let first_manifest =
        logger_producer_manifest_json(&manifest).expect("logger producer manifest must serialize");
    let second_manifest = logger_producer_manifest_json(&manifest)
        .expect("logger producer manifest must serialize again");
    assert_eq!(first_manifest, second_manifest);
    assert_eq!(
        first_manifest,
        std::fs::read(manifest_path).expect("checked logger producer manifest must be readable")
    );

    let first_audit =
        logger_producer_audit_json(&audit).expect("logger producer audit must serialize");
    let second_audit =
        logger_producer_audit_json(&audit).expect("logger producer audit must serialize again");
    assert_eq!(first_audit, second_audit);
    assert_eq!(
        first_audit,
        std::fs::read(audit_path).expect("checked logger producer audit must be readable")
    );
}

#[test]
fn checked_logger_producer_audit_is_complete_and_stable() {
    const CONTRACT_PATH: &str = "compat/firecracker/v1.16.0/logger-contract.md";
    const EXPECTED_CLASSES: [(&str, LoggerClassDisposition, usize); 31] = [
        (
            "logger.api-control.outcome",
            LoggerClassDisposition::Implemented,
            7,
        ),
        (
            "logger.api-worker.outcome",
            LoggerClassDisposition::Implemented,
            5,
        ),
        ("logger.api.request", LoggerClassDisposition::Implemented, 1),
        ("logger.api.result", LoggerClassDisposition::Implemented, 5),
        (
            "logger.backend.outcome",
            LoggerClassDisposition::Implemented,
            26,
        ),
        (
            "logger.balloon.outcome",
            LoggerClassDisposition::Implemented,
            28,
        ),
        (
            "logger.block.outcome",
            LoggerClassDisposition::Implemented,
            34,
        ),
        ("logger.boot.time", LoggerClassDisposition::Implemented, 1),
        (
            "logger.entropy.outcome",
            LoggerClassDisposition::Implemented,
            16,
        ),
        (
            "logger.lifecycle.outcome",
            LoggerClassDisposition::Implemented,
            24,
        ),
        (
            "logger.limiter.recovery",
            LoggerClassDisposition::Implemented,
            1,
        ),
        (
            "logger.memory-hotplug.outcome",
            LoggerClassDisposition::Implemented,
            22,
        ),
        (
            "logger.network.outcome",
            LoggerClassDisposition::Implemented,
            26,
        ),
        (
            "logger.nonapp.example",
            LoggerClassDisposition::NotApplicable,
            22,
        ),
        (
            "logger.nonapp.fuzzing",
            LoggerClassDisposition::NotApplicable,
            1,
        ),
        (
            "logger.nonapp.gdb",
            LoggerClassDisposition::NotApplicable,
            19,
        ),
        (
            "logger.nonapp.linux-hardening",
            LoggerClassDisposition::NotApplicable,
            2,
        ),
        (
            "logger.nonapp.tool",
            LoggerClassDisposition::NotApplicable,
            1,
        ),
        (
            "logger.nonapp.tracing",
            LoggerClassDisposition::NotApplicable,
            2,
        ),
        (
            "logger.nonapp.x86",
            LoggerClassDisposition::NotApplicable,
            19,
        ),
        (
            "logger.observability.outcome",
            LoggerClassDisposition::Implemented,
            4,
        ),
        (
            "logger.pmem.outcome",
            LoggerClassDisposition::Implemented,
            18,
        ),
        (
            "logger.process-signal.outcome",
            LoggerClassDisposition::Implemented,
            5,
        ),
        (
            "logger.process-startup.outcome",
            LoggerClassDisposition::Implemented,
            5,
        ),
        (
            "logger.process.exit",
            LoggerClassDisposition::Implemented,
            3,
        ),
        (
            "logger.process.panic",
            LoggerClassDisposition::Implemented,
            3,
        ),
        (
            "logger.serial.outcome",
            LoggerClassDisposition::Implemented,
            12,
        ),
        (
            "logger.snapshot.outcome",
            LoggerClassDisposition::Implemented,
            18,
        ),
        (
            "logger.time-identity.outcome",
            LoggerClassDisposition::Implemented,
            12,
        ),
        (
            "logger.transport.outcome",
            LoggerClassDisposition::Implemented,
            74,
        ),
        (
            "logger.vsock.outcome",
            LoggerClassDisposition::Implemented,
            52,
        ),
    ];
    const LOGGER_CAPABILITIES: [&str; 11] = [
        "api-operation:PUT /logger",
        "api-path:/logger",
        "api-property:FullVmConfiguration.logger",
        "api-property:Logger.level",
        "api-property:Logger.log_path",
        "api-property:Logger.module",
        "api-property:Logger.show_level",
        "api-property:Logger.show_log_origin",
        "api-schema:Logger",
        "corpus:logger",
        "semantic.observability:logger-delivery-filtering-loss-and-redaction",
    ];
    const NON_APPLICABLE_POLICY: [(&str, LoggerNonApplicableReason, &str); 7] = [
        (
            "logger.nonapp.example",
            LoggerNonApplicableReason::ExampleOnly,
            "These calls exist only in upstream example programs and are not Firecracker VMM process producers.",
        ),
        (
            "logger.nonapp.fuzzing",
            LoggerNonApplicableReason::DeveloperInstrumentation,
            "The producer exists only in an explicitly insecure developer fuzzing build and is not a production VMM outcome.",
        ),
        (
            "logger.nonapp.gdb",
            LoggerNonApplicableReason::DeveloperInstrumentation,
            "These calls serve Firecracker's optional developer GDB server rather than the production VMM logger contract selected for Bangbang.",
        ),
        (
            "logger.nonapp.linux-hardening",
            LoggerNonApplicableReason::LinuxKvmOnly,
            "The producer reports a Linux prctl mechanism that has no identity-preserving macOS/HVF operation; Bangbang documents its platform security boundary separately.",
        ),
        (
            "logger.nonapp.tool",
            LoggerNonApplicableReason::SeparateToolOwner,
            "This producer belongs to the separately delivered CPU-template-helper command rather than the Bangbang VMM process logger.",
        ),
        (
            "logger.nonapp.tracing",
            LoggerNonApplicableReason::TracingOwned,
            "The log-instrument crate implements developer tracing owned by #1791, not the Firecracker VMM logger compatibility surface.",
        ),
        (
            "logger.nonapp.x86",
            LoggerNonApplicableReason::X86Only,
            "The mapped architecture, CPUID, xstate, interrupt, debug-register, and i8042 producers have no identity-preserving macOS arm64/HVF execution path. Firecracker's i8042 source module is shared, but its construction, PIO registration, API exposure, and runtime ownership are x86_64-only; Bangbang's arm64 platform instead exposes PL031 RTC without an i8042 controller.",
        ),
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest =
        read_logger_producer_manifest(&repository_root.join(LOGGER_PRODUCER_MANIFEST_PATH))
            .expect("checked logger producer manifest must parse");
    let audit = read_logger_producer_audit(&repository_root.join(LOGGER_PRODUCER_AUDIT_PATH))
        .expect("checked logger producer audit must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");

    assert_eq!(manifest.invocations.len(), 468);
    assert_eq!(manifest.inputs.len(), 81);
    assert_eq!(manifest.counts.scanned_rust_files, 362);
    assert_eq!(manifest.counts.ordinary, 429);
    assert_eq!(manifest.counts.unrestricted, 39);
    assert_eq!(manifest.counts.production, 446);
    assert_eq!(manifest.counts.test, 0);
    assert_eq!(manifest.counts.example, 22);
    assert_eq!(manifest.counts.direct, 466);
    assert_eq!(manifest.counts.macro_template, 2);
    assert_eq!(audit.classes.len(), 31);
    assert_eq!(audit.mappings.len(), 468);
    assert_eq!(LOGGER_CAPABILITIES, LOGGER_COMPATIBILITY_CAPABILITY_IDS);

    let classes = audit
        .classes
        .iter()
        .map(|class| (class.id.as_str(), class))
        .collect::<BTreeMap<_, _>>();
    let mapping_counts =
        audit
            .mappings
            .iter()
            .fold(BTreeMap::<&str, usize>::new(), |mut counts, mapping| {
                *counts.entry(mapping.class_id.as_str()).or_default() += 1;
                counts
            });
    let contract = std::fs::read_to_string(repository_root.join(CONTRACT_PATH))
        .expect("logger producer contract must be readable");
    let contract_rows = contract
        .lines()
        .filter(|line| line.starts_with("| `logger."))
        .collect::<Vec<_>>();
    assert_eq!(contract_rows.len(), EXPECTED_CLASSES.len());

    for (id, disposition, expected_mappings) in EXPECTED_CLASSES {
        let class = classes
            .get(id)
            .unwrap_or_else(|| panic!("logger class must exist: {id}"));
        assert_eq!(class.disposition, disposition, "disposition drifted: {id}");
        assert_eq!(mapping_counts.get(id), Some(&expected_mappings));
        let disposition = match disposition {
            LoggerClassDisposition::Implemented => "implemented",
            LoggerClassDisposition::Planned => "planned",
            LoggerClassDisposition::NotApplicable => "not-applicable",
        };
        let prefix = format!("| `{id}` | `{disposition}` |");
        let row = contract_rows
            .iter()
            .find(|row| row.starts_with(&prefix))
            .unwrap_or_else(|| panic!("contract class row must exist: {id}"));
        assert!(
            row.ends_with(&format!("| {expected_mappings} |")),
            "contract mapping count drifted: {id}"
        );
    }
    for (id, reason, rationale) in NON_APPLICABLE_POLICY {
        let class = classes
            .get(id)
            .unwrap_or_else(|| panic!("non-applicable logger class must exist: {id}"));
        assert_eq!(class.non_applicable_reason, Some(reason));
        assert_eq!(class.rationale, rationale);
    }

    assert_eq!(
        audit
            .classes
            .iter()
            .filter(|class| class.disposition == LoggerClassDisposition::Implemented)
            .count(),
        24
    );
    assert_eq!(
        audit
            .classes
            .iter()
            .filter(|class| class.disposition == LoggerClassDisposition::Planned)
            .count(),
        0
    );
    assert_eq!(
        audit
            .classes
            .iter()
            .filter(|class| class.disposition == LoggerClassDisposition::NotApplicable)
            .count(),
        7
    );
    let mapped_dispositions = audit
        .mappings
        .iter()
        .map(|mapping| {
            classes
                .get(mapping.class_id.as_str())
                .expect("mapped class must exist")
                .disposition
        })
        .fold(BTreeMap::new(), |mut counts, disposition| {
            *counts.entry(disposition).or_insert(0_usize) += 1;
            counts
        });
    assert_eq!(
        mapped_dispositions,
        BTreeMap::from([
            (LoggerClassDisposition::Implemented, 402),
            (LoggerClassDisposition::NotApplicable, 66),
        ])
    );
    let planned_owners = audit
        .classes
        .iter()
        .filter(|class| class.disposition == LoggerClassDisposition::Planned)
        .fold(BTreeMap::new(), |mut counts, class| {
            let issue = class
                .delivery_issue
                .as_deref()
                .expect("planned class must name its owner");
            *counts.entry(issue).or_insert(0_usize) += 1;
            counts
        });
    assert_eq!(planned_owners, BTreeMap::new());

    let compiled_events = audit
        .classes
        .iter()
        .flat_map(|class| class.compiled_events.iter().copied())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        compiled_events,
        BTreeSet::from([
            LoggerCompiledEvent::ApiControl,
            LoggerCompiledEvent::ApiWorker,
            LoggerCompiledEvent::ApiRequest,
            LoggerCompiledEvent::ApiResult,
            LoggerCompiledEvent::Backend,
            LoggerCompiledEvent::Balloon,
            LoggerCompiledEvent::Block,
            LoggerCompiledEvent::InstanceStart,
            LoggerCompiledEvent::FlushMetrics,
            LoggerCompiledEvent::BootTime,
            LoggerCompiledEvent::Entropy,
            LoggerCompiledEvent::Lifecycle,
            LoggerCompiledEvent::MemoryHotplug,
            LoggerCompiledEvent::Network,
            LoggerCompiledEvent::Observability,
            LoggerCompiledEvent::Pmem,
            LoggerCompiledEvent::RateLimitRecovery,
            LoggerCompiledEvent::ProcessStartup,
            LoggerCompiledEvent::ProcessPanic,
            LoggerCompiledEvent::ProcessExit,
            LoggerCompiledEvent::ProcessSignal,
            LoggerCompiledEvent::Serial,
            LoggerCompiledEvent::Snapshot,
            LoggerCompiledEvent::TimeIdentity,
            LoggerCompiledEvent::Transport,
            LoggerCompiledEvent::Vsock,
        ])
    );
    let planned_with_compiled_events = audit
        .classes
        .iter()
        .filter(|class| {
            class.disposition == LoggerClassDisposition::Planned
                && !class.compiled_events.is_empty()
        })
        .collect::<Vec<_>>();
    assert!(planned_with_compiled_events.is_empty());

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for id in LOGGER_CAPABILITIES {
        assert_eq!(
            capabilities
                .get(id)
                .unwrap_or_else(|| panic!("logger capability must exist: {id}"))
                .disposition,
            Disposition::ImplementedAndVerified,
            "#1810 capability disposition drifted: {id}"
        );
    }
}

#[test]
fn checked_logger_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let logger_manifest =
        read_logger_producer_manifest(&repository_root.join(LOGGER_PRODUCER_MANIFEST_PATH))
            .expect("checked logger producer manifest must parse");
    let logger_audit =
        read_logger_producer_audit(&repository_root.join(LOGGER_PRODUCER_AUDIT_PATH))
            .expect("checked logger producer audit must parse");

    assert!(
        inventory
            .capabilities
            .iter()
            .any(|capability| capability.disposition == Disposition::AuditRequired)
    );
    assert!(
        inventory
            .capabilities
            .iter()
            .any(|capability| { capability.disposition == Disposition::MissingPlatformFeasible })
    );
    validate_logger_compatibility(
        &manifest,
        &inventory,
        &logger_manifest,
        &logger_audit,
        &repository_root,
    )
    .expect("checked logger compatibility scope must be terminal");

    let mut nonterminal = inventory.clone();
    let capability = nonterminal
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:logger")
        .expect("logger corpus capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_logger_compatibility(
        &manifest,
        &nonterminal,
        &logger_manifest,
        &logger_audit,
        &repository_root,
    )
    .expect_err("nonterminal logger aggregate must fail")
    .to_string();
    assert!(error.contains("logger certification requires implemented-and-verified capability"));

    let mut missing = inventory.clone();
    missing.capabilities.retain(|capability| {
        capability.id != "semantic.observability:logger-delivery-filtering-loss-and-redaction"
    });
    let error = validate_logger_compatibility(
        &manifest,
        &missing,
        &logger_manifest,
        &logger_audit,
        &repository_root,
    )
    .expect_err("missing logger aggregate must fail")
    .to_string();
    assert!(error.contains("logger certification capability is missing"));

    let mut missing_evidence = inventory.clone();
    missing_evidence
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:logger")
        .expect("logger corpus capability must exist")
        .validation
        .clear();
    let error = validate_logger_compatibility(
        &manifest,
        &missing_evidence,
        &logger_manifest,
        &logger_audit,
        &repository_root,
    )
    .expect_err("logger aggregate without validation evidence must fail")
    .to_string();
    assert!(error.contains("implemented-and-verified requires validation references"));

    let mut planned = logger_audit.clone();
    let class = planned
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.api.request")
        .expect("API request class must exist");
    class.disposition = LoggerClassDisposition::Planned;
    class.delivery_issue = Some("#1810".to_string());
    let error = validate_logger_compatibility(
        &manifest,
        &inventory,
        &logger_manifest,
        &planned,
        &repository_root,
    )
    .expect_err("planned logger class must fail terminal certification")
    .to_string();
    assert!(error.contains("final logger validation forbids planned class"));
}

#[test]
fn checked_tracing_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let logger_manifest =
        read_logger_producer_manifest(&repository_root.join(LOGGER_PRODUCER_MANIFEST_PATH))
            .expect("checked logger producer manifest must parse");
    let logger_audit =
        read_logger_producer_audit(&repository_root.join(LOGGER_PRODUCER_AUDIT_PATH))
            .expect("checked logger producer audit must parse");
    let tracing_path = repository_root.join(TRACING_AUDIT_PATH);
    let tracing_audit =
        read_tracing_audit(&tracing_path).expect("checked tracing audit must parse");

    assert_eq!(
        tracing_audit_json(&tracing_audit).expect("tracing audit must serialize canonically"),
        std::fs::read(tracing_path).expect("checked tracing audit must be readable")
    );
    assert_eq!(
        tracing_audit
            .call_sites
            .iter()
            .map(|call_site| call_site.id.as_str())
            .collect::<Vec<_>>(),
        TRACING_CALL_SITE_IDS
    );
    assert_eq!(TRACING_COMPATIBILITY_CAPABILITY_IDS, ["corpus:tracing"]);
    validate_tracing_audit(&tracing_audit, &repository_root, AuditMode::Final)
        .expect("checked tracing audit must be terminal");
    validate_tracing_compatibility(
        &manifest,
        &inventory,
        &logger_manifest,
        &logger_audit,
        &tracing_audit,
        &repository_root,
    )
    .expect("checked tracing compatibility scope must be terminal");

    let mut missing_call = tracing_audit.clone();
    missing_call.call_sites.pop();
    let error = validate_tracing_audit(&missing_call, &repository_root, AuditMode::Final)
        .expect_err("a missing tracing call site must fail closed")
        .to_string();
    assert!(error.contains("must contain 8 call sites"));
    assert!(error.contains("exact call-site id set"));

    let mut changed_policy = tracing_audit.clone();
    changed_policy.call_sites[0].module = "bangbang::changed".to_string();
    let error = validate_tracing_audit(&changed_policy, &repository_root, AuditMode::Final)
        .expect_err("changed tracing policy must fail closed")
        .to_string();
    assert!(error.contains("call-site policy has drifted"));

    let mut nonterminal = inventory.clone();
    let capability = nonterminal
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:tracing")
        .expect("tracing corpus capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_tracing_compatibility(
        &manifest,
        &nonterminal,
        &logger_manifest,
        &logger_audit,
        &tracing_audit,
        &repository_root,
    )
    .expect_err("nonterminal tracing capability must fail")
    .to_string();
    assert!(error.contains("tracing certification requires implemented-and-verified"));

    let mut stale_evidence = inventory.clone();
    stale_evidence
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:tracing")
        .expect("tracing corpus capability must exist")
        .validation
        .pop();
    let error = validate_tracing_compatibility(
        &manifest,
        &stale_evidence,
        &logger_manifest,
        &logger_audit,
        &tracing_audit,
        &repository_root,
    )
    .expect_err("stale tracing evidence must fail")
    .to_string();
    assert!(error.contains("tracing certification requires exact capability evidence"));
}

#[test]
fn checked_metrics_schema_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let authority =
        read_metrics_schema_authority(&repository_root.join(METRICS_SCHEMA_AUTHORITY_PATH))
            .expect("checked metrics schema authority must parse");

    assert_eq!(
        METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS.len()
    );
    assert_eq!(
        METRICS_AGGREGATE_CAPABILITY_IDS
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        METRICS_AGGREGATE_CAPABILITY_IDS.len()
    );
    validate_metrics_schema_compatibility(&manifest, &inventory, &authority, &repository_root)
        .expect("checked metrics API/schema compatibility scope must be terminal");

    assert_eq!(
        inventory
            .capabilities
            .iter()
            .filter(|capability| { capability.disposition == Disposition::ImplementedAndVerified })
            .count(),
        354
    );
    assert_eq!(
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == Disposition::AuditRequired)
            .count(),
        31
    );
    assert_eq!(
        inventory
            .capabilities
            .iter()
            .filter(|capability| { capability.disposition == Disposition::MissingPlatformFeasible })
            .count(),
        3
    );
    assert_eq!(
        inventory
            .capabilities
            .iter()
            .filter(|capability| {
                capability.disposition == Disposition::ProvenPlatformImpossible
            })
            .count(),
        30
    );

    let mut nonterminal = inventory.clone();
    let capability = nonterminal
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "api-schema:Metrics")
        .expect("owned metrics capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_metrics_schema_compatibility(
        &manifest,
        &nonterminal,
        &authority,
        &repository_root,
    )
    .expect_err("nonterminal owned metrics capability must fail")
    .to_string();
    assert!(
        error.contains("metrics schema certification requires implemented-and-verified capability")
    );

    let mut missing = inventory.clone();
    missing
        .capabilities
        .retain(|capability| capability.id != "api-schema:Metrics");
    let error =
        validate_metrics_schema_compatibility(&manifest, &missing, &authority, &repository_root)
            .expect_err("missing owned metrics capability must fail")
            .to_string();
    assert!(error.contains("metrics schema certification capability is missing"));

    let mut missing_evidence = inventory.clone();
    missing_evidence
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "api-schema:Metrics")
        .expect("owned metrics capability must exist")
        .validation
        .clear();
    let error = validate_metrics_schema_compatibility(
        &manifest,
        &missing_evidence,
        &authority,
        &repository_root,
    )
    .expect_err("owned metrics capability without evidence must fail")
    .to_string();
    assert!(error.contains("implemented-and-verified requires validation references"));

    let mut hybrid_aggregate = inventory.clone();
    let aggregate = hybrid_aggregate
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:metrics")
        .expect("terminal metrics corpus must exist");
    aggregate.disposition = Disposition::AuditRequired;
    aggregate.implementation.clear();
    aggregate.validation.clear();
    let error = validate_metrics_schema_compatibility(
        &manifest,
        &hybrid_aggregate,
        &authority,
        &repository_root,
    )
    .expect_err("hybrid #1790 aggregate transition must fail schema certification")
    .to_string();
    assert!(error.contains("requires the exact aggregate transition"));

    let mut stale_schema_runtime = authority.clone();
    let profile = stale_schema_runtime
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.producer_owner == MetricsProducerOwner::SchemaRuntime)
        .expect("schema-runtime profile must exist");
    let implemented_id = profile.id.clone();
    let planned_id = "milliseconds-since-unix-epoch-none-schema-runtime-planned";
    profile.id = planned_id.to_string();
    profile.producer_disposition = MetricsProducerDisposition::Planned;
    profile.delivery_issue = Some("#1822".to_string());
    profile.rationale =
        "Canonical line construction and timestamp publication are delivered by #1822 before producer closure."
            .to_string();
    profile.implementation.clear();
    profile.validation.clear();
    stale_schema_runtime
        .field_policies
        .iter_mut()
        .filter(|policy| policy.profile_id == implemented_id)
        .for_each(|policy| policy.profile_id = planned_id.to_string());
    let error = validate_metrics_schema_compatibility(
        &manifest,
        &inventory,
        &stale_schema_runtime,
        &repository_root,
    )
    .expect_err("stale #1822 schema-runtime handoff must fail")
    .to_string();
    assert!(error.contains("outside the exact completed-process/device transition"));
    assert!(error.contains("exactly 1 implemented schema-runtime"));

    let mut wrong_handoff = authority.clone();
    let profile = wrong_handoff
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.producer_owner == MetricsProducerOwner::ProcessLifecycle)
        .expect("process-lifecycle profile must exist");
    profile.delivery_issue = Some("#9999".to_string());
    let error = validate_metrics_schema_compatibility(
        &manifest,
        &inventory,
        &wrong_handoff,
        &repository_root,
    )
    .expect_err("wrong later-owner handoff must fail")
    .to_string();
    assert!(error.contains("outside the exact completed-process/device transition"));
}

#[test]
fn checked_metrics_process_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let authority =
        read_metrics_schema_authority(&repository_root.join(METRICS_SCHEMA_AUTHORITY_PATH))
            .expect("checked metrics schema authority must parse");
    let audit = read_metrics_process_producer_audit(
        &repository_root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH),
    )
    .expect("checked process producer audit must parse");

    validate_metrics_process_compatibility(
        &manifest,
        &inventory,
        &authority,
        &audit,
        &repository_root,
    )
    .expect("checked process metrics compatibility scope must be terminal");

    let mut unresolved = audit.clone();
    let record = unresolved
        .records
        .iter_mut()
        .find(|record| record.delivery_issue == "#1827")
        .expect("completed API process record must exist");
    record.disposition = MetricsProcessProducerDisposition::Planned;
    record.implementation.clear();
    record.validation.clear();
    let error = validate_metrics_process_compatibility(
        &manifest,
        &inventory,
        &authority,
        &unresolved,
        &repository_root,
    )
    .expect_err("unresolved process record must fail final certification")
    .to_string();
    assert!(error.contains("completed metrics process producer slice must be terminal"));
    assert!(error.contains("final metrics process producer validation rejects planned record"));

    let mut regressed_authority = authority.clone();
    let profile = regressed_authority
        .policy_profiles
        .iter_mut()
        .find(|profile| {
            profile.producer_owner == MetricsProducerOwner::ProcessLifecycle
                && profile.unit == bangbang_firecracker_capability_audit::MetricsUnit::Count
        })
        .expect("implemented process count profile must exist");
    let implemented_id = profile.id.clone();
    let planned_id = "count-none-process-lifecycle-planned";
    profile.id = planned_id.to_string();
    profile.producer_disposition = MetricsProducerDisposition::Planned;
    profile.delivery_issue = Some("#1788".to_string());
    profile.rationale =
        "#1788 owns the exact API, process, logger, signal, boot, and lifecycle producer boundary."
            .to_string();
    profile.implementation.clear();
    profile.validation.clear();
    regressed_authority
        .field_policies
        .iter_mut()
        .filter(|policy| policy.profile_id == implemented_id)
        .for_each(|policy| policy.profile_id = planned_id.to_string());
    let error = validate_metrics_process_compatibility(
        &manifest,
        &inventory,
        &regressed_authority,
        &audit,
        &repository_root,
    )
    .expect_err("regressed process profile must fail final certification")
    .to_string();
    assert!(error.contains("outside the exact completed-process/device transition"));
    assert!(error.contains("exactly 2 implemented process-lifecycle"));
}

#[test]
fn checked_metrics_device_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let authority =
        read_metrics_schema_authority(&repository_root.join(METRICS_SCHEMA_AUTHORITY_PATH))
            .expect("checked metrics schema authority must parse");
    let process_audit = read_metrics_process_producer_audit(
        &repository_root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH),
    )
    .expect("checked process producer audit must parse");
    let device_audit = read_metrics_device_producer_audit(
        &repository_root.join(METRICS_DEVICE_PRODUCER_AUDIT_PATH),
    )
    .expect("checked device producer audit must parse");

    validate_metrics_device_compatibility(
        &manifest,
        &inventory,
        &authority,
        &process_audit,
        &device_audit,
        &repository_root,
    )
    .expect("checked device metrics compatibility scope must be terminal");

    let expected_ids = TERMINAL_DEVICE_POLICY_PROFILE_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let actual_ids = authority
        .policy_profiles
        .iter()
        .filter(|profile| profile.producer_owner == MetricsProducerOwner::Device)
        .map(|profile| profile.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_ids, expected_ids);
    assert_eq!(
        device_audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsDeviceProducerDisposition::Implemented
            })
            .count(),
        212
    );
    assert_eq!(
        device_audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsDeviceProducerDisposition::SourceNeutral
            })
            .count(),
        2
    );
    assert_eq!(
        device_audit
            .records
            .iter()
            .filter(|record| {
                record.disposition == MetricsDeviceProducerDisposition::PlatformZero
            })
            .count(),
        17
    );

    let platform_field_ids = device_audit
        .records
        .iter()
        .filter(|record| record.disposition == MetricsDeviceProducerDisposition::PlatformZero)
        .map(|record| record.field_id.clone())
        .collect::<BTreeSet<_>>();
    let platform_terminal_profile_ids = authority
        .field_policies
        .iter()
        .filter(|policy| platform_field_ids.contains(&policy.field_id))
        .map(|policy| policy.profile_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(platform_terminal_profile_ids.len(), 5);

    let mut historical = authority.clone();
    for profile in historical
        .policy_profiles
        .iter_mut()
        .filter(|profile| profile.producer_owner == MetricsProducerOwner::Device)
    {
        profile.id = profile.id.replace("-implemented", "-planned");
        profile.producer_disposition = MetricsProducerDisposition::Planned;
        profile.delivery_issue = Some("#1789".to_string());
        profile.rationale =
            "#1789 owns the exact supported-device producer, neutral-value, and bounded-key boundary."
                .to_string();
        profile.implementation.clear();
        profile.validation.clear();
    }
    for terminal_id in platform_terminal_profile_ids {
        let planned_id = terminal_id.replace("-implemented", "-planned");
        let mut profile = historical
            .policy_profiles
            .iter()
            .find(|profile| profile.id == planned_id)
            .expect("matching historical planned profile must exist")
            .clone();
        profile.id = terminal_id.replace("-implemented", "-platform-zero");
        profile.producer_disposition = MetricsProducerDisposition::PlatformZero;
        profile.rationale = "The arm64 schema retains this Linux/x86-oriented field as a required numeric neutral value; #1789 owns terminal evidence."
            .to_string();
        historical.policy_profiles.push(profile);
    }
    historical
        .policy_profiles
        .sort_by(|left, right| left.id.cmp(&right.id));
    for policy in &mut historical.field_policies {
        if !policy.profile_id.contains("-device-implemented") {
            continue;
        }
        let disposition = if platform_field_ids.contains(&policy.field_id) {
            "platform-zero"
        } else {
            "planned"
        };
        policy.profile_id = policy
            .profile_id
            .replace("-device-implemented", &format!("-device-{disposition}"));
    }
    validate_metrics_schema_compatibility(&manifest, &inventory, &historical, &repository_root)
        .expect("earlier scoped gates must accept the exact historical device handoff");
    validate_metrics_device_producers(
        &device_audit,
        &historical,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect("terminal field truth must remain valid against the historical profile handoff");
    let error = validate_metrics_device_compatibility(
        &manifest,
        &inventory,
        &historical,
        &process_audit,
        &device_audit,
        &repository_root,
    )
    .expect_err("device-final must reject the historical profile handoff")
    .to_string();
    assert!(error.contains("exact terminal device policy profile set"));

    let certification_error = |authority: &_| {
        validate_metrics_device_compatibility(
            &manifest,
            &inventory,
            authority,
            &process_audit,
            &device_audit,
            &repository_root,
        )
        .expect_err("mutated device authority must fail final certification")
        .to_string()
    };

    let mut partial = authority.clone();
    let profile = partial
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.id == "bytes-none-device-implemented")
        .expect("terminal byte profile must exist");
    profile.id = "bytes-none-device-planned".to_string();
    profile.producer_disposition = MetricsProducerDisposition::Planned;
    profile.delivery_issue = Some("#1789".to_string());
    profile.rationale =
        "#1789 owns the exact supported-device producer, neutral-value, and bounded-key boundary."
            .to_string();
    profile.implementation.clear();
    profile.validation.clear();
    partial
        .field_policies
        .iter_mut()
        .filter(|policy| policy.profile_id == "bytes-none-device-implemented")
        .for_each(|policy| policy.profile_id = "bytes-none-device-planned".to_string());
    let error = certification_error(&partial);
    assert!(error.contains("exact historical #1789 device handoff or the exact terminal"));
    assert!(error.contains("exact terminal device policy profile set"));

    let mut platform_candidate = authority.clone();
    let mut profile = platform_candidate
        .policy_profiles
        .iter()
        .find(|profile| profile.id == "count-none-device-implemented")
        .expect("terminal count profile must exist")
        .clone();
    profile.id = "count-none-device-platform-zero".to_string();
    profile.producer_disposition = MetricsProducerDisposition::PlatformZero;
    profile.delivery_issue = Some("#1789".to_string());
    profile.rationale = "The arm64 schema retains this Linux/x86-oriented field as a required numeric neutral value; #1789 owns terminal evidence."
        .to_string();
    profile.implementation.clear();
    profile.validation.clear();
    platform_candidate.policy_profiles.push(profile);
    platform_candidate
        .policy_profiles
        .sort_by(|left, right| left.id.cmp(&right.id));
    platform_candidate
        .field_policies
        .iter_mut()
        .find(|policy| policy.field_id == "static:i8042.error_count")
        .expect("i8042 platform field must exist")
        .profile_id = "count-none-device-platform-zero".to_string();
    let error = certification_error(&platform_candidate);
    assert!(error.contains("exact historical #1789 device handoff or the exact terminal"));
    assert!(error.contains("exact terminal device policy profile set"));

    let mut evidence_drift = authority.clone();
    let profile = evidence_drift
        .policy_profiles
        .iter_mut()
        .find(|profile| profile.producer_owner == MetricsProducerOwner::Device)
        .expect("terminal device profile must exist");
    profile.implementation[0] = Reference::Local {
        path: "crates/runtime/src/metrics.rs".to_string(),
        anchor: Some("fn missing_device_anchor".to_string()),
    };
    let error = certification_error(&evidence_drift);
    assert!(error.contains("requires exact common evidence"));
    assert!(error.contains("evidence anchor does not resolve"));

    let mut unresolved = device_audit.clone();
    let record = unresolved
        .records
        .iter_mut()
        .find(|record| record.disposition == MetricsDeviceProducerDisposition::Implemented)
        .expect("implemented device record must exist");
    record.disposition = MetricsDeviceProducerDisposition::Planned;
    record.implementation.clear();
    record.validation.clear();
    let error = validate_metrics_device_compatibility(
        &manifest,
        &inventory,
        &authority,
        &process_audit,
        &unresolved,
        &repository_root,
    )
    .expect_err("regressed device record must fail final certification")
    .to_string();
    assert!(error.contains("final metrics device producer validation rejects nonterminal record"));
    assert!(error.contains("exact terminal 231-record census"));

    let mut hybrid = inventory.clone();
    let aggregate = hybrid
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:metrics")
        .expect("terminal metrics corpus must exist");
    aggregate.disposition = Disposition::AuditRequired;
    aggregate.implementation.clear();
    aggregate.validation.clear();
    let error = validate_metrics_device_compatibility(
        &manifest,
        &hybrid,
        &authority,
        &process_audit,
        &device_audit,
        &repository_root,
    )
    .expect_err("hybrid #1790 transition must fail device certification")
    .to_string();
    assert!(error.contains("requires the exact aggregate transition"));
}

#[test]
fn checked_metrics_aggregate_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let authority =
        read_metrics_schema_authority(&repository_root.join(METRICS_SCHEMA_AUTHORITY_PATH))
            .expect("checked metrics schema authority must parse");
    let process_audit = read_metrics_process_producer_audit(
        &repository_root.join(METRICS_PROCESS_PRODUCER_AUDIT_PATH),
    )
    .expect("checked process producer audit must parse");
    let device_audit = read_metrics_device_producer_audit(
        &repository_root.join(METRICS_DEVICE_PRODUCER_AUDIT_PATH),
    )
    .expect("checked device producer audit must parse");
    let lifecycle_audit =
        read_metrics_lifecycle_audit(&repository_root.join(METRICS_LIFECYCLE_AUDIT_PATH))
            .expect("checked metrics lifecycle audit must parse");

    let certification_error = |inventory, lifecycle| {
        validate_metrics_compatibility(
            &manifest,
            inventory,
            &authority,
            &process_audit,
            &device_audit,
            lifecycle,
            &repository_root,
        )
        .expect_err("mutated aggregate metrics scope must fail certification")
        .to_string()
    };

    validate_metrics_compatibility(
        &manifest,
        &inventory,
        &authority,
        &process_audit,
        &device_audit,
        &lifecycle_audit,
        &repository_root,
    )
    .expect("checked aggregate metrics compatibility scope must be terminal");

    let mut hybrid = inventory.clone();
    let capability = hybrid
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == "corpus:metrics")
        .expect("metrics corpus capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = certification_error(&hybrid, &lifecycle_audit);
    assert!(error.contains("requires the exact aggregate transition"));
    assert!(error.contains("requires implemented-and-verified capability"));

    let mut evidence = inventory.clone();
    evidence
        .capabilities
        .iter_mut()
        .find(|capability| {
            capability.id == "semantic.observability:metrics-schema-producers-flush-and-lifecycle"
        })
        .expect("metrics semantic capability must exist")
        .validation
        .swap(0, 1);
    assert!(
        certification_error(&evidence, &lifecycle_audit)
            .contains("requires exact capability evidence")
    );

    let mut lifecycle = lifecycle_audit.clone();
    lifecycle.records[0].claims =
        vec![bangbang_firecracker_capability_audit::MetricsLifecycleClaim::FinalAttemptOnce];
    let error = certification_error(&inventory, &lifecycle);
    assert!(error.contains("stale claims"));
    assert!(error.contains("combined transaction claims must be owned only"));

    let mut regressed_device = device_audit.clone();
    let record = regressed_device
        .records
        .iter_mut()
        .find(|record| record.disposition == MetricsDeviceProducerDisposition::Implemented)
        .expect("implemented device record must exist");
    record.disposition = MetricsDeviceProducerDisposition::Planned;
    record.implementation.clear();
    record.validation.clear();
    let error = validate_metrics_compatibility(
        &manifest,
        &inventory,
        &authority,
        &process_audit,
        &regressed_device,
        &lifecycle_audit,
        &repository_root,
    )
    .expect_err("regressed device producer must fail aggregate certification")
    .to_string();
    assert!(error.contains("final metrics device producer validation rejects nonterminal record"));
}

#[test]
fn logger_audit_mutations_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest =
        read_logger_producer_manifest(&repository_root.join(LOGGER_PRODUCER_MANIFEST_PATH))
            .expect("checked logger producer manifest must parse");
    let audit = read_logger_producer_audit(&repository_root.join(LOGGER_PRODUCER_AUDIT_PATH))
        .expect("checked logger producer audit must parse");

    validate_logger_producers(&manifest, &audit, &repository_root, AuditMode::Final)
        .expect("terminal logger audit must pass final validation");

    let mut planned = audit.clone();
    let class = planned
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.balloon.outcome")
        .expect("balloon class must exist");
    class.disposition = LoggerClassDisposition::Planned;
    class.delivery_issue = Some("#1809".to_string());
    let final_error =
        validate_logger_producers(&manifest, &planned, &repository_root, AuditMode::Final)
            .expect_err("final mode must reject reintroduced planned classes")
            .to_string();
    assert!(final_error.contains("final logger validation forbids planned class"));

    let mut missing = audit.clone();
    missing.mappings.pop();
    let error =
        validate_logger_producers(&manifest, &missing, &repository_root, AuditMode::Delivery)
            .expect_err("missing mapping must fail")
            .to_string();
    assert!(error.contains("logger invocation has no audit mapping"));

    let mut duplicate = audit.clone();
    duplicate.mappings.push(
        duplicate
            .mappings
            .last()
            .expect("mapping must exist")
            .clone(),
    );
    let error =
        validate_logger_producers(&manifest, &duplicate, &repository_root, AuditMode::Delivery)
            .expect_err("duplicate mapping must fail")
            .to_string();
    assert!(error.contains("logger mapping invocation id entries must be sorted and unique"));

    let mut stale = audit.clone();
    let mut stale_mapping = stale.mappings.last().expect("mapping must exist").clone();
    stale_mapping.invocation_id = "logger-invocation:zzz/stale.rs:1:1".to_string();
    stale.mappings.push(stale_mapping);
    let error = validate_logger_producers(&manifest, &stale, &repository_root, AuditMode::Delivery)
        .expect_err("stale mapping must fail")
        .to_string();
    assert!(error.contains("logger audit mapping is stale"));

    let mut malformed_manifest = manifest.clone();
    malformed_manifest.invocations[0].fingerprint = "sha256:NOT-A-DIGEST".to_string();
    let error = validate_logger_producers(
        &malformed_manifest,
        &audit,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("malformed fingerprint must fail")
    .to_string();
    assert!(error.contains("fingerprint is not lowercase SHA-256"));

    let mut overflowing_counts = manifest.clone();
    overflowing_counts.counts.production = usize::MAX;
    let error = validate_logger_producers(
        &overflowing_counts,
        &audit,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("overflowing declared counts must fail without panicking")
    .to_string();
    assert!(error.contains("logger source-context counts must cover every invocation"));

    let mut malformed_blob = manifest.clone();
    malformed_blob.inputs[0].git_blob = "A".repeat(40);
    let error = validate_logger_producers(
        &malformed_blob,
        &audit,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("noncanonical Git blob ID must fail")
    .to_string();
    assert!(error.contains("logger input git_blob is not a Git object id"));

    let mut catch_all = audit.clone();
    let class = catch_all
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.api-control.outcome")
        .expect("planned class must exist");
    class.id = "logger.api.catch-all".to_string();
    let error =
        validate_logger_producers(&manifest, &catch_all, &repository_root, AuditMode::Delivery)
            .expect_err("catch-all class must fail")
            .to_string();
    assert!(error.contains("logger class id uses a catch-all term"));

    let mut mismatched_not_applicable = audit.clone();
    let class = mismatched_not_applicable
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.nonapp.example")
        .expect("example class must exist");
    class.guest_triggerable = true;
    let error = validate_logger_producers(
        &manifest,
        &mismatched_not_applicable,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("not-applicable policy mismatch must fail")
    .to_string();
    assert!(error.contains("must use only not-applicable policy"));

    let mut blocking_guest = audit.clone();
    let class = blocking_guest
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.block.outcome")
        .expect("block class must exist");
    class.delivery = LoggerDeliveryPolicy::BoundedHost;
    let error = validate_logger_producers(
        &manifest,
        &blocking_guest,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("guest-triggerable blocking delivery must fail")
    .to_string();
    assert!(error.contains("requires exact nonblocking-guest delivery"));

    let mut evidence_mismatch = audit.clone();
    let class = evidence_mismatch
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.api.result")
        .expect("API result class must exist");
    class.validation.clear();
    let error = validate_logger_producers(
        &manifest,
        &evidence_mismatch,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("compiled event without complete evidence must fail")
    .to_string();
    assert!(error.contains("compiled-event metadata and exact evidence must appear together"));

    let mut wrong_compiled_owner = audit.clone();
    let boot_class = wrong_compiled_owner
        .classes
        .iter()
        .find(|class| class.id == "logger.boot.time")
        .expect("boot class must exist")
        .clone();
    let class = wrong_compiled_owner
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.boot.time")
        .expect("boot class must exist");
    class.compiled_events.clear();
    class.implementation.clear();
    class.validation.clear();
    let class = wrong_compiled_owner
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.api-control.outcome")
        .expect("API control class must exist");
    class.compiled_events = vec![LoggerCompiledEvent::BootTime];
    class.implementation = boot_class.implementation;
    class.validation = boot_class.validation;
    let error = validate_logger_producers(
        &manifest,
        &wrong_compiled_owner,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("compiled event on the wrong class must fail")
    .to_string();
    assert!(error.contains("logger compiled event must have its exact class"));

    let mut unresolved_evidence = audit;
    let class = unresolved_evidence
        .classes
        .iter_mut()
        .find(|class| class.id == "logger.api.request")
        .expect("API request class must exist");
    class.implementation[0] = Reference::Local {
        path: "crates/runtime/src/missing-logger-evidence.rs".to_string(),
        anchor: None,
    };
    let error = validate_logger_producers(
        &manifest,
        &unresolved_evidence,
        &repository_root,
        AuditMode::Delivery,
    )
    .expect_err("unresolved local evidence must fail")
    .to_string();
    assert!(error.contains("local reference path does not exist"));
}

fn local_reference_paths(references: &[Reference]) -> Option<BTreeSet<&str>> {
    references
        .iter()
        .map(|reference| match reference {
            Reference::Local { path, .. } => Some(path.as_str()),
            Reference::Github { .. } | Reference::Authoritative { .. } => None,
        })
        .collect()
}

#[test]
fn wave_7_ownership_and_core_api_policy_is_stable() {
    const CONTRACT_PATH: &str =
        "compat/firecracker/v1.16.0/observability-tools-specification-contract.md";
    const CHALLENGE_URL: &str =
        "https://github.com/seven332/bangbang/issues/1784#issuecomment-5161129449";
    const WAVE_7_OWNED: [(&str, &str); 93] = [
        ("api-operation:GET /", "#1784"),
        ("api-operation:GET /version", "#1784"),
        ("api-operation:GET /vm/config", "#1784"),
        ("api-operation:PUT /actions", "#1784"),
        ("api-path:/", "#1784"),
        ("api-path:/actions", "#1784"),
        ("api-path:/version", "#1784"),
        ("api-path:/vm/config", "#1784"),
        ("api-property:CpuConfig.cpuid_modifiers", "#1784"),
        ("api-property:CpuConfig.msr_modifiers", "#1784"),
        ("api-property:CpuidLeafModifier.flags", "#1784"),
        ("api-property:CpuidLeafModifier.leaf", "#1784"),
        ("api-property:CpuidLeafModifier.modifiers", "#1784"),
        ("api-property:CpuidLeafModifier.subleaf", "#1784"),
        ("api-property:CpuidRegisterModifier.bitmap", "#1784"),
        ("api-property:CpuidRegisterModifier.register", "#1784"),
        ("api-property:Error.fault_message", "#1784"),
        (
            "api-property:FirecrackerVersion.firecracker_version",
            "#1784",
        ),
        ("api-property:InstanceActionInfo.action_type", "#1784"),
        ("api-property:InstanceInfo.app_name", "#1784"),
        ("api-property:InstanceInfo.id", "#1784"),
        ("api-property:InstanceInfo.state", "#1784"),
        ("api-property:InstanceInfo.vmm_version", "#1784"),
        ("api-property:MsrModifier.addr", "#1784"),
        ("api-property:MsrModifier.bitmap", "#1784"),
        ("api-schema:CpuidLeafModifier", "#1784"),
        ("api-schema:CpuidRegisterModifier", "#1784"),
        ("api-schema:Error", "#1784"),
        ("api-schema:FirecrackerVersion", "#1784"),
        ("api-schema:FullVmConfiguration", "#1784"),
        ("api-schema:InstanceActionInfo", "#1784"),
        ("api-schema:InstanceInfo", "#1784"),
        ("api-schema:MsrModifier", "#1784"),
        ("corpus:actions-api", "#1784"),
        (
            "semantic.specification:api-availability-stability-and-failure-information",
            "#1784",
        ),
        ("api-operation:PUT /logger", "#1786"),
        ("api-path:/logger", "#1786"),
        ("api-property:FullVmConfiguration.logger", "#1786"),
        ("api-property:Logger.level", "#1786"),
        ("api-property:Logger.log_path", "#1786"),
        ("api-property:Logger.module", "#1786"),
        ("api-property:Logger.show_level", "#1786"),
        ("api-property:Logger.show_log_origin", "#1786"),
        ("api-schema:Logger", "#1786"),
        ("corpus:logger", "#1786"),
        (
            "semantic.observability:logger-delivery-filtering-loss-and-redaction",
            "#1786",
        ),
        ("api-operation:PUT /metrics", "#1787"),
        ("api-path:/metrics", "#1787"),
        ("api-property:FullVmConfiguration.metrics", "#1787"),
        ("api-property:Metrics.metrics_path", "#1787"),
        ("api-property:RateLimiter.bandwidth", "#1787"),
        ("api-property:RateLimiter.ops", "#1787"),
        ("api-property:TokenBucket.one_time_burst", "#1787"),
        ("api-property:TokenBucket.refill_time", "#1787"),
        ("api-property:TokenBucket.size", "#1787"),
        ("api-schema:Metrics", "#1787"),
        ("api-schema:RateLimiter", "#1787"),
        ("api-schema:TokenBucket", "#1787"),
        ("corpus:metrics", "#1790"),
        (
            "semantic.observability:metrics-schema-producers-flush-and-lifecycle",
            "#1790",
        ),
        ("corpus:tracing", "#1791"),
        (
            "tool-argument:cpu-template-helper/template/dump/config",
            "#1792",
        ),
        (
            "tool-argument:cpu-template-helper/template/dump/output",
            "#1792",
        ),
        (
            "tool-argument:cpu-template-helper/template/dump/template",
            "#1792",
        ),
        (
            "tool-argument:cpu-template-helper/template/verify/config",
            "#1792",
        ),
        (
            "tool-argument:cpu-template-helper/template/verify/template",
            "#1792",
        ),
        ("tool-operation:cpu-template-helper/template/dump", "#1792"),
        (
            "tool-operation:cpu-template-helper/template/verify",
            "#1792",
        ),
        (
            "tool-argument:cpu-template-helper/template/strip/paths",
            "#1793",
        ),
        (
            "tool-argument:cpu-template-helper/template/strip/suffix",
            "#1793",
        ),
        ("tool-operation:cpu-template-helper/template/strip", "#1793"),
        (
            "tool-argument:cpu-template-helper/fingerprint/compare/curr",
            "#1794",
        ),
        (
            "tool-argument:cpu-template-helper/fingerprint/compare/filters",
            "#1794",
        ),
        (
            "tool-argument:cpu-template-helper/fingerprint/compare/prev",
            "#1794",
        ),
        (
            "tool-argument:cpu-template-helper/fingerprint/dump/config",
            "#1794",
        ),
        (
            "tool-argument:cpu-template-helper/fingerprint/dump/output",
            "#1794",
        ),
        (
            "tool-argument:cpu-template-helper/fingerprint/dump/template",
            "#1794",
        ),
        (
            "tool-operation:cpu-template-helper/fingerprint/compare",
            "#1794",
        ),
        (
            "tool-operation:cpu-template-helper/fingerprint/dump",
            "#1794",
        ),
        ("corpus:cpu-template-helper", "#1795"),
        ("corpus:cpu-templates", "#1795"),
        (
            "semantic.cpu:configuration-templates-and-feature-state",
            "#1795",
        ),
        ("corpus:getting-started", "#1796"),
        ("corpus:rootfs-and-kernel", "#1796"),
        ("corpus:formal-verification", "#1797"),
        ("corpus:network-performance", "#1798"),
        ("corpus:specification", "#1798"),
        (
            "semantic.specification:performance-resource-and-telemetry-outcomes",
            "#1798",
        ),
        ("corpus:design", "#1799"),
        ("corpus:device-api", "#1799"),
        ("corpus:release-changelog", "#1799"),
        (
            "semantic.tools:packaging-help-errors-and-applicable-operations",
            "#1799",
        ),
        ("semantic.transport:virtio-mmio-activation", "#1799"),
    ];
    const RETAINED_HANDOFFS: [(&str, &str); 9] = [
        ("corpus:jailer", "#1373"),
        ("corpus:production-host", "#1373"),
        ("tool-argument:jailer/chroot-base-dir", "#1373"),
        ("tool-argument:jailer/gid", "#1373"),
        ("tool-argument:jailer/uid", "#1373"),
        ("tool-operation:jailer/run", "#1373"),
        ("corpus:network-setup", "#1378"),
        (
            "semantic.network:virtio-net-vmnet-policy-and-connectivity",
            "#1378",
        ),
        (
            "semantic.cross-capability:state-errors-metrics-security-and-snapshots",
            "Wave 8",
        ),
    ];
    const CORE_IMPLEMENTED: [&str; 22] = [
        "api-operation:GET /",
        "api-operation:GET /version",
        "api-operation:GET /vm/config",
        "api-operation:PUT /actions",
        "api-path:/",
        "api-path:/actions",
        "api-path:/version",
        "api-path:/vm/config",
        "api-property:Error.fault_message",
        "api-property:FirecrackerVersion.firecracker_version",
        "api-property:InstanceActionInfo.action_type",
        "api-property:InstanceInfo.app_name",
        "api-property:InstanceInfo.id",
        "api-property:InstanceInfo.state",
        "api-property:InstanceInfo.vmm_version",
        "api-schema:Error",
        "api-schema:FirecrackerVersion",
        "api-schema:FullVmConfiguration",
        "api-schema:InstanceActionInfo",
        "api-schema:InstanceInfo",
        "corpus:actions-api",
        "semantic.specification:api-availability-stability-and-failure-information",
    ];
    const LOGGER_IMPLEMENTED: [&str; 11] = [
        "api-operation:PUT /logger",
        "api-path:/logger",
        "api-property:FullVmConfiguration.logger",
        "api-property:Logger.level",
        "api-property:Logger.log_path",
        "api-property:Logger.module",
        "api-property:Logger.show_level",
        "api-property:Logger.show_log_origin",
        "api-schema:Logger",
        "corpus:logger",
        "semantic.observability:logger-delivery-filtering-loss-and-redaction",
    ];
    const X86_IMPOSSIBLE: [&str; 13] = [
        "api-property:CpuConfig.cpuid_modifiers",
        "api-property:CpuConfig.msr_modifiers",
        "api-property:CpuidLeafModifier.flags",
        "api-property:CpuidLeafModifier.leaf",
        "api-property:CpuidLeafModifier.modifiers",
        "api-property:CpuidLeafModifier.subleaf",
        "api-property:CpuidRegisterModifier.bitmap",
        "api-property:CpuidRegisterModifier.register",
        "api-property:MsrModifier.addr",
        "api-property:MsrModifier.bitmap",
        "api-schema:CpuidLeafModifier",
        "api-schema:CpuidRegisterModifier",
        "api-schema:MsrModifier",
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(repository_root.join(CONTRACT_PATH))
        .expect("Wave 7 contract must be readable");
    let normalized_contract = contract.split_whitespace().collect::<Vec<_>>().join(" ");

    let owned = WAVE_7_OWNED
        .iter()
        .map(|(id, _)| *id)
        .collect::<BTreeSet<_>>();
    let handoffs = RETAINED_HANDOFFS
        .iter()
        .map(|(id, _)| *id)
        .collect::<BTreeSet<_>>();
    let implemented = CORE_IMPLEMENTED.into_iter().collect::<BTreeSet<_>>();
    let logger_implemented = LOGGER_IMPLEMENTED.into_iter().collect::<BTreeSet<_>>();
    let metrics_implemented = METRICS_SCHEMA_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let metrics_aggregate_implemented = METRICS_AGGREGATE_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let tracing_implemented = TRACING_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let cpu_template_helper_implemented = CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let cpu_template_strip_implemented = CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let impossible = X86_IMPOSSIBLE.into_iter().collect::<BTreeSet<_>>();

    assert_eq!(owned.len(), 93, "Wave 7 owner identities must be unique");
    assert_eq!(handoffs.len(), 9, "retained handoffs must be unique");
    assert!(owned.is_disjoint(&handoffs));
    assert_eq!(implemented.len(), 22);
    assert_eq!(logger_implemented.len(), 11);
    assert_eq!(metrics_implemented.len(), 12);
    assert_eq!(metrics_aggregate_implemented.len(), 2);
    assert_eq!(tracing_implemented.len(), 1);
    assert_eq!(cpu_template_helper_implemented.len(), 7);
    assert_eq!(cpu_template_strip_implemented.len(), 3);
    assert_eq!(impossible.len(), 13);
    assert!(implemented.is_disjoint(&impossible));
    assert!(logger_implemented.is_disjoint(&implemented));
    assert!(logger_implemented.is_disjoint(&impossible));
    assert!(metrics_implemented.is_disjoint(&implemented));
    assert!(metrics_implemented.is_disjoint(&logger_implemented));
    assert!(metrics_implemented.is_disjoint(&impossible));
    assert!(metrics_aggregate_implemented.is_disjoint(&implemented));
    assert!(metrics_aggregate_implemented.is_disjoint(&logger_implemented));
    assert!(metrics_aggregate_implemented.is_disjoint(&metrics_implemented));
    assert!(metrics_aggregate_implemented.is_disjoint(&impossible));
    assert!(tracing_implemented.is_disjoint(&implemented));
    assert!(tracing_implemented.is_disjoint(&logger_implemented));
    assert!(tracing_implemented.is_disjoint(&metrics_implemented));
    assert!(tracing_implemented.is_disjoint(&metrics_aggregate_implemented));
    assert!(tracing_implemented.is_disjoint(&impossible));
    assert!(cpu_template_helper_implemented.is_disjoint(&implemented));
    assert!(cpu_template_helper_implemented.is_disjoint(&logger_implemented));
    assert!(cpu_template_helper_implemented.is_disjoint(&metrics_implemented));
    assert!(cpu_template_helper_implemented.is_disjoint(&metrics_aggregate_implemented));
    assert!(cpu_template_helper_implemented.is_disjoint(&tracing_implemented));
    assert!(cpu_template_helper_implemented.is_disjoint(&impossible));
    assert!(cpu_template_strip_implemented.is_disjoint(&implemented));
    assert!(cpu_template_strip_implemented.is_disjoint(&logger_implemented));
    assert!(cpu_template_strip_implemented.is_disjoint(&metrics_implemented));
    assert!(cpu_template_strip_implemented.is_disjoint(&metrics_aggregate_implemented));
    assert!(cpu_template_strip_implemented.is_disjoint(&tracing_implemented));
    assert!(cpu_template_strip_implemented.is_disjoint(&cpu_template_helper_implemented));
    assert!(cpu_template_strip_implemented.is_disjoint(&impossible));
    assert!(implemented.union(&impossible).all(|id| owned.contains(id)));
    assert!(logger_implemented.iter().all(|id| owned.contains(id)));
    assert!(metrics_implemented.iter().all(|id| owned.contains(id)));
    assert!(
        metrics_aggregate_implemented
            .iter()
            .all(|id| owned.contains(id))
    );
    assert!(tracing_implemented.iter().all(|id| owned.contains(id)));
    assert!(
        cpu_template_helper_implemented
            .iter()
            .all(|id| owned.contains(id))
    );

    for (id, owner) in WAVE_7_OWNED.into_iter().chain(RETAINED_HANDOFFS) {
        assert!(
            by_id.contains_key(id),
            "Wave 7 ledger identity must exist: {id}"
        );
        let prefix = format!("| `{id}` | {owner} |");
        assert_eq!(
            contract.matches(&prefix).count(),
            1,
            "Wave 7 contract must contain one exact owner row: {id}"
        );
    }

    for id in &implemented {
        let capability = by_id.get(id).expect("core API identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "core API identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1784 | `implemented-and-verified` |")),
            "contract must record implemented result: {id}"
        );
    }

    for id in &logger_implemented {
        let capability = by_id.get(id).expect("logger API identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "logger API identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1786 | `implemented-and-verified` |")),
            "contract must record implemented logger result: {id}"
        );
    }

    for id in &metrics_implemented {
        let capability = by_id.get(id).expect("metrics API identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "metrics API identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1787 | `implemented-and-verified` |")),
            "contract must record implemented metrics result: {id}"
        );
    }

    for id in &metrics_aggregate_implemented {
        let capability = by_id
            .get(id)
            .expect("aggregate metrics identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "aggregate metrics identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1790 | `implemented-and-verified` |")),
            "contract must record implemented aggregate metrics result: {id}"
        );
    }

    for id in &tracing_implemented {
        let capability = by_id.get(id).expect("tracing identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "tracing identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1791 | `implemented-and-verified` |")),
            "contract must record implemented tracing result: {id}"
        );
    }

    for id in &cpu_template_helper_implemented {
        let capability = by_id
            .get(id)
            .expect("CPU-template helper identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "CPU-template helper identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1792 | `implemented-and-verified` |")),
            "contract must record implemented CPU-template helper result: {id}"
        );
    }

    for id in &cpu_template_strip_implemented {
        let capability = by_id
            .get(id)
            .expect("CPU-template strip identity must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "CPU-template strip identity must be terminal: {id}"
        );
        assert!(!capability.implementation.is_empty());
        assert!(!capability.validation.is_empty());
        assert!(capability.exclusion.is_none());
        assert!(
            contract.contains(&format!("| `{id}` | #1793 | `implemented-and-verified` |")),
            "contract must record implemented CPU-template strip result: {id}"
        );
    }

    for id in &impossible {
        let capability = by_id.get(id).expect("x86 identity must exist");
        assert_eq!(capability.source_refs, [*id]);
        assert_eq!(
            capability.disposition,
            Disposition::ProvenPlatformImpossible,
            "x86 identity must retain terminal platform evidence: {id}"
        );
        let exclusion = capability
            .exclusion
            .as_ref()
            .expect("x86 identity must retain exclusion evidence");
        assert!(exclusion.upstream_contract.iter().any(|reference| matches!(
            reference,
            Reference::Authoritative { url } if url.contains("firecracker.yaml#L")
        )));
        assert!(exclusion.upstream_contract.iter().any(|reference| matches!(
            reference,
            Reference::Authoritative { url }
                if url.contains("cpu_config/x86_64/custom_cpu_template.rs#L")
        )));
        assert!(
            exclusion.platform_evidence.iter().all(|reference| matches!(
                reference,
                Reference::Authoritative { url } if url.starts_with("https://developer.apple.com/documentation/hypervisor/")
            ))
        );
        assert!(exclusion.alternatives.len() >= 3);
        assert_eq!(
            local_reference_paths(&exclusion.stable_behavior),
            Some(BTreeSet::from(["crates/api/src/http.rs"]))
        );
        assert_eq!(
            local_reference_paths(&exclusion.focused_tests),
            Some(BTreeSet::from([
                "crates/api/src/http.rs",
                "crates/bangbang/tests/process_e2e.rs",
            ]))
        );
        assert!(
            local_reference_paths(&exclusion.compatibility_docs)
                .expect("x86 compatibility evidence must be local")
                .contains(CONTRACT_PATH)
        );
        assert_eq!(
            local_reference_paths(&exclusion.security_docs),
            Some(BTreeSet::from(["docs/security.md"]))
        );
        assert_eq!(
            exclusion.challenge,
            Reference::Github {
                url: CHALLENGE_URL.to_string()
            }
        );
        assert!(capability.summary.contains("x86_64"));
        assert!(capability.summary.contains("malformed"));
        assert!(
            contract.contains(&format!(
                "| `{id}` | #1784 | `proven-platform-impossible` |"
            )),
            "contract must record platform result: {id}"
        );
    }

    let selected = implemented
        .iter()
        .chain(logger_implemented.iter())
        .chain(metrics_implemented.iter())
        .chain(metrics_aggregate_implemented.iter())
        .chain(tracing_implemented.iter())
        .chain(cpu_template_helper_implemented.iter())
        .chain(cpu_template_strip_implemented.iter())
        .chain(impossible.iter())
        .copied()
        .collect::<BTreeSet<_>>();
    let expected_audit = owned
        .difference(&selected)
        .copied()
        .chain(handoffs.iter().copied())
        .collect::<BTreeSet<_>>();
    let actual_audit = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.disposition == Disposition::AuditRequired)
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_audit, expected_audit);
    for id in &handoffs {
        assert_eq!(
            by_id.get(id).expect("handoff must exist").disposition,
            Disposition::AuditRequired,
            "external or Wave 8 handoff must not move: {id}"
        );
    }

    assert!(normalized_contract.contains("Producer-only children #1785, #1788, and #1789"));
    assert!(normalized_contract.contains("does not claim comprehensive failure logging (#1786)"));
    assert!(
        normalized_contract
            .contains("numeric startup/resource/performance or telemetry outcomes (#1798)")
    );
    assert!(normalized_contract.contains("final cross-capability interactions (Wave 8)"));
    assert!(normalized_contract.contains("354 implemented, 31 audit-required"));
    assert!(normalized_contract.contains("376/9/3/30"));
}

#[test]
fn checked_cpu_template_helper_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract_path = "compat/firecracker/v1.16.0/cpu-template-helper-contract.md";
    let contract = std::fs::read_to_string(repository_root.join(contract_path))
        .expect("CPU-template helper contract must be readable");

    validate_cpu_template_helper_compatibility(&manifest, &inventory, &repository_root)
        .expect("checked CPU-template dump and verify scope must be terminal");

    assert_eq!(
        CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS.len(),
        "CPU-template helper capability set must remain exact and duplicate-free"
    );
    for id in CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("CPU-template helper capability must exist: {id}"));
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "dump and verify capability must remain terminal: {id}"
        );
        assert_eq!(
            contract
                .matches(&format!("| `{id}` | `implemented-and-verified` |"))
                .count(),
            1,
            "helper contract must contain one exact terminal row: {id}"
        );
    }
    for id in CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("retained helper capability must exist: {id}"));
        assert_eq!(capability.disposition, Disposition::AuditRequired, "{id}");
        assert!(capability.implementation.is_empty(), "{id}");
        assert!(capability.validation.is_empty(), "{id}");
    }

    let package_root = repository_root.join("tools/cpu-template-helper");
    let package_manifest = std::fs::read_to_string(package_root.join("Cargo.toml"))
        .expect("CPU-template helper package manifest must be readable");
    assert!(package_root.join("src/lib.rs").is_file());
    assert!(package_root.join("src/main.rs").is_file());
    assert!(package_manifest.contains("[[bin]]"));
    assert!(package_manifest.contains("bangbang-hvf"));

    let mut hybrid = inventory.clone();
    let capability = hybrid
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS[0])
        .expect("owned capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_cpu_template_helper_transition(&hybrid)
        .expect_err("partial #1792 transition must fail")
        .to_string();
    assert!(error.contains("exact #1793 strip terminal transition"));

    let mut dump_verify_terminal = inventory.clone();
    for capability in &mut dump_verify_terminal.capabilities {
        if CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS.contains(&capability.id.as_str()) {
            capability.disposition = Disposition::AuditRequired;
            capability.implementation.clear();
            capability.validation.clear();
        }
    }
    validate_cpu_template_helper_transition(&dump_verify_terminal)
        .expect("exact #1792 terminal handoff must remain accepted");
    validate_cpu_template_helper_compatibility(&manifest, &dump_verify_terminal, &repository_root)
        .expect("dump and verify certification must not require independent strip completion");

    let mut historical = dump_verify_terminal.clone();
    for capability in &mut historical.capabilities {
        if CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS.contains(&capability.id.as_str()) {
            capability.disposition = Disposition::AuditRequired;
            capability.implementation.clear();
            capability.validation.clear();
        }
    }
    validate_cpu_template_helper_transition(&historical)
        .expect("exact historical #1792 handoff must remain accepted by delivery validation");
    let error =
        validate_cpu_template_helper_compatibility(&manifest, &historical, &repository_root)
            .expect_err("terminal certification must reject the historical handoff")
            .to_string();
    assert!(error.contains("requires implemented-and-verified capability"));

    let mut leaked = inventory.clone();
    leaked
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS[0])
        .expect("retained capability must exist")
        .disposition = Disposition::ImplementedAndVerified;
    let error = validate_cpu_template_helper_transition(&leaked)
        .expect_err("#1792 must not promote later helper scope")
        .to_string();
    assert!(error.contains("remain exactly audit-required"));
}

#[test]
fn checked_cpu_template_strip_compatibility_is_terminal_and_fail_closed() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract_path = "compat/firecracker/v1.16.0/cpu-template-strip-contract.md";
    let contract = std::fs::read_to_string(repository_root.join(contract_path))
        .expect("CPU-template strip contract must be readable");

    validate_cpu_template_strip_compatibility(&manifest, &inventory, &repository_root)
        .expect("checked portable CPU-template strip scope must be terminal");

    let strip_ids = CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        strip_ids.len(),
        CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS.len(),
        "CPU-template strip capability set must remain exact and duplicate-free"
    );
    assert!(
        strip_ids.is_disjoint(
            &CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS
                .into_iter()
                .collect()
        )
    );
    assert!(
        strip_ids.is_disjoint(
            &CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS
                .into_iter()
                .collect()
        )
    );
    for id in CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("CPU-template strip capability must exist: {id}"));
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "strip capability must remain terminal: {id}"
        );
        assert_eq!(
            contract
                .matches(&format!("| `{id}` | `implemented-and-verified` |"))
                .count(),
            1,
            "strip contract must contain one exact terminal row: {id}"
        );
    }
    for id in CPU_TEMPLATE_HELPER_RETAINED_CAPABILITY_IDS {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("retained helper capability must exist: {id}"));
        assert_eq!(capability.disposition, Disposition::AuditRequired, "{id}");
        assert!(capability.implementation.is_empty(), "{id}");
        assert!(capability.validation.is_empty(), "{id}");
    }

    let mut hybrid = inventory.clone();
    let capability = hybrid
        .capabilities
        .iter_mut()
        .find(|capability| capability.id == CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS[0])
        .expect("strip capability must exist");
    capability.disposition = Disposition::AuditRequired;
    capability.implementation.clear();
    capability.validation.clear();
    let error = validate_cpu_template_helper_transition(&hybrid)
        .expect_err("partial #1793 transition must fail")
        .to_string();
    assert!(error.contains("exact #1793 strip terminal transition"));

    let mut dependency_regression = inventory.clone();
    for capability in &mut dependency_regression.capabilities {
        if CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS.contains(&capability.id.as_str()) {
            capability.disposition = Disposition::AuditRequired;
            capability.implementation.clear();
            capability.validation.clear();
        }
    }
    let error = validate_cpu_template_helper_transition(&dependency_regression)
        .expect_err("strip cannot be terminal before dump and verify")
        .to_string();
    assert!(error.contains("exact #1793 strip terminal transition"));

    let mut historical = inventory.clone();
    for capability in &mut historical.capabilities {
        if CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS.contains(&capability.id.as_str()) {
            capability.disposition = Disposition::AuditRequired;
            capability.implementation.clear();
            capability.validation.clear();
        }
    }
    validate_cpu_template_helper_transition(&historical)
        .expect("exact #1792 terminal handoff must remain accepted");
    let error = validate_cpu_template_strip_compatibility(&manifest, &historical, &repository_root)
        .expect_err("strip final gate must reject the historical handoff")
        .to_string();
    assert!(error.contains("requires implemented-and-verified capability"));
}

#[test]
fn snapshot_paging_terminal_policy_is_stable() {
    const CAPABILITY_ID: &str = "corpus:snapshot-page-faults";

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let capability = inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == CAPABILITY_ID)
        .expect("snapshot page-fault corpus record must exist");

    assert_eq!(
        capability.source_refs,
        [CAPABILITY_ID],
        "snapshot paging must retain its exact pinned source identity"
    );
    assert_eq!(
        capability.disposition,
        Disposition::ImplementedAndVerified,
        "snapshot paging must remain terminal after signed certification"
    );
    assert!(
        capability.delivery_issue.is_none() && capability.exclusion.is_none(),
        "terminal snapshot paging must not retain a delivery owner or exclusion"
    );
    assert_eq!(
        local_reference_paths(&capability.implementation),
        Some(BTreeSet::from([
            "crates/bangbang/src/vmm.rs",
            "crates/hvf/src/lazy_guest_fault.rs",
            "crates/hvf/src/lazy_host_fault.rs",
            "crates/launcher/src/grant_manifest.rs",
            "crates/pager/src/lib.rs",
            "crates/runtime/src/lazy_memory.rs",
        ])),
        "snapshot implementation evidence must stay exact"
    );
    assert_eq!(
        local_reference_paths(&capability.validation),
        Some(BTreeSet::from([
            "compat/firecracker/v1.16.0/snapshot-paging-contract.md",
            "crates/bangbang/tests/executable_hvf_e2e.rs",
            "crates/hvf/tests/guest_boot.rs",
            "crates/hvf/tests/hvf_lifecycle.rs",
            "crates/launcher/tests/production_bundle_e2e.rs",
        ])),
        "snapshot validation evidence must stay exact"
    );
    assert!(
        capability.summary.contains("Native-v1 Uffd restores")
            && capability
                .summary
                .contains("without worker memory-file authority")
            && capability.summary.contains("instruction/read/write-first")
            && capability
                .summary
                .contains("before/during/after population")
            && capability.summary.contains("exact entitlements")
            && capability.summary.contains("not Linux UFFD"),
        "snapshot paging summary must retain runtime, signed evidence, and compatibility limits"
    );

    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/snapshot-paging-contract.md"),
    )
    .expect("checked snapshot paging contract must be readable");
    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "snapshot paging ledger must have one row");
    assert!(
        rows[0].starts_with(&format!("| `{CAPABILITY_ID}` |"))
            && rows[0].contains("`implemented-and-verified`")
            && rows[0].contains("| — |")
            && rows[0].ends_with("| `terminal` |"),
        "snapshot paging ledger row must pin identity, terminal status, and result"
    );

    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "handling-page-faults-on-snapshot-resume.md",
        "mach_memory_object_memory_entry_64",
        "hv_vm_protect",
        "guest_bypassed_host_protection=true",
        "guest_population value=0x31415926",
        "host_population value=0x00000000 faults=1",
        "removed_guest_population value=0x00000000",
        "handler_death_detected=true",
        "cleanup=complete",
        "com.apple.security.app-sandbox",
        "com.apple.security.hypervisor",
        "task_swap_exception_ports",
        "bangbang-pager-v1",
        "BBPAGER\\0",
        "docs/snapshot-feasibility.md",
        "docs/snapshot-pager-protocol.md",
        "docs/security.md",
        "docs/testing.md",
        "crates/pager/src/lib.rs",
        "crates/pager/src/frame.rs",
        "crates/pager/src/client.rs",
        "crates/runtime/src/lazy_memory.rs",
        "crates/hvf/src/lazy_host_fault.rs",
        "crates/hvf/src/lazy_guest_fault.rs",
        "crates/bangbang/src/vmm.rs",
        "crates/launcher/src/grant_manifest.rs",
        "crates/bangbang/tests/executable_hvf_e2e.rs",
        "crates/hvf/tests/hvf_lifecycle.rs",
        "crates/hvf/tests/guest_boot.rs",
        "crates/launcher/tests/production_bundle_e2e.rs",
        "ProcessVmm::preflight_native_v1_memory_backend",
        "image_id[16] || crc64_jones_le[8] || data_length_le[8]",
        "current native-v2 rejects Uffd",
        "without worker memory-file authority",
        "before/during/after-population removal generations",
        "exact nested signing",
        "https://github.com/seven332/bangbang/issues/1555",
    ] {
        assert!(
            contract.contains(required),
            "snapshot paging contract must pin {required}"
        );
    }

    let consumer_rows = contract
        .lines()
        .filter(|line| line.starts_with("| consumer:"))
        .collect::<Vec<_>>();
    let expected_consumers = BTreeSet::from([
        "consumer:balloon-control",
        "consumer:balloon-reclaim",
        "consumer:block-sync-async",
        "consumer:boot-fdt",
        "consumer:eager-file-regression",
        "consumer:entropy",
        "consumer:guest-memory-atomic",
        "consumer:guest-memory-raw-pointer",
        "consumer:guest-memory-slices",
        "consumer:hvf-stage-two",
        "consumer:memory-hotplug-control",
        "consumer:memory-hotplug-topology",
        "consumer:network-vmnet-mmds",
        "consumer:pmem",
        "consumer:public-memory-borrow",
        "consumer:snapshot-dirty-diff",
        "consumer:snapshot-full-save",
        "consumer:snapshot-restore-population",
        "consumer:teardown",
        "consumer:transport-mmio-pci",
        "consumer:vhost-user",
        "consumer:virtqueue-core",
        "consumer:vmgenid-vmclock-pvtime",
        "consumer:vsock",
    ]);
    let actual_consumers = consumer_rows
        .iter()
        .map(|row| {
            row.split('|')
                .nth(1)
                .expect("consumer row must have an identity column")
                .trim()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        consumer_rows.len(),
        expected_consumers.len(),
        "snapshot paging consumer row count drifted"
    );
    assert_eq!(
        actual_consumers, expected_consumers,
        "snapshot paging consumer identity set drifted"
    );
    for row in consumer_rows {
        assert_eq!(
            row.split('|').count(),
            7,
            "consumer row must retain exactly five data columns: {row}"
        );
        assert!(
            [
                "bridged",
                "resolver-only",
                "preflight-rejected",
                "gated",
                "ordered-owner",
                "unchanged",
            ]
            .iter()
            .any(|disposition| row.ends_with(&format!("| {disposition} |"))),
            "consumer row must retain a closed disposition: {row}"
        );
    }

    let pager_manifest = std::fs::read_to_string(repository_root.join("crates/pager/Cargo.toml"))
        .expect("checked pager manifest must be readable");
    assert!(
        pager_manifest.contains("name = \"bangbang-pager\"")
            && pager_manifest.contains("getrandom = \"0.3\"")
            && pager_manifest.contains("libc = \"0.2\""),
        "pager package identity and narrow dependencies must remain pinned"
    );

    let runtime_manifest =
        std::fs::read_to_string(repository_root.join("crates/runtime/Cargo.toml"))
            .expect("checked runtime manifest must be readable");
    assert!(
        runtime_manifest.contains("bangbang-pager = { path = \"../pager\" }"),
        "runtime must retain its narrow pager type dependency"
    );

    let pager_source = std::fs::read_to_string(repository_root.join("crates/pager/src/frame.rs"))
        .expect("checked pager framing source must be readable");
    for required in [
        "*b\"BBPAGER\\0\"",
        "pub const HEADER_BYTES: usize = 24",
        "pub const MIN_PAGE_SIZE: u32 = 4 * 1024",
        "pub const MAX_PAGE_SIZE: u32 = 2 * 1024 * 1024",
        "pub struct PagerFrameDecoder",
    ] {
        assert!(
            pager_source.contains(required),
            "pager source must retain {required}"
        );
    }

    let pager_client = std::fs::read_to_string(repository_root.join("crates/pager/src/client.rs"))
        .expect("checked pager client source must be readable");
    for required in [
        "pub struct PagerClient",
        "pub trait PagerClientTerminalObserver",
        "concurrent_page_and_removal_complete_out_of_order",
        "explicit_terminal_fans_out_once_and_rejects_later_work",
        "timeout_releases_every_pending_operation_with_one_terminal_notification",
    ] {
        assert!(
            pager_client.contains(required),
            "pager client must retain {required}"
        );
    }

    let lazy_source =
        std::fs::read_to_string(repository_root.join("crates/runtime/src/lazy_memory.rs"))
            .expect("checked lazy-memory coordinator source must be readable");
    for required in [
        "pub struct LazyGuestMemory",
        "pub struct LazyGuestMemoryConsumer",
        "pub struct LazyGuestMemoryConsumerProfile",
        "pub unsafe fn claim_protected_consumer",
        "pub struct LazyPagePopulation",
        "pub struct LazyPagePublication",
        "pub struct LazyPageRemoval",
        "enum PageTag",
        "PopulationStage::Retired",
        "duplicate_faults_coalesce_to_one_generation_and_result",
        "removal_reserves_a_distinct_slot_before_superseding_loading",
        "removal_stays_counted_and_removing_until_acknowledged",
        "requested_peer_and_teardown_outcomes_wake_waiters",
        "pub fn signal_terminal",
        "nonblocking_terminal_signal_preserves_first_reason_during_removal",
        "nonblocking_terminal_signal_wakes_duplicate_population_waiters",
        "generation_exhaustion_is_owner_terminal",
        "protected_consumer_is_one_shot_and_gates_mutating_memory_operations",
        "consumer_profile_classifies_each_incompatible_behavior",
        "repeated_construction_and_destruction_leaves_no_retained_work",
    ] {
        assert!(
            lazy_source.contains(required),
            "lazy-memory coordinator must retain {required}"
        );
    }

    let hvf_manifest = std::fs::read_to_string(repository_root.join("crates/hvf/Cargo.toml"))
        .expect("checked HVF manifest must be readable");
    assert!(
        hvf_manifest.contains("bangbang-pager = { path = \"../pager\" }")
            && hvf_manifest.contains("bangbang-runtime = { path = \"../runtime\" }"),
        "host adapter must retain only its direct coordinator/protocol type dependencies"
    );

    let hvf_build = std::fs::read_to_string(repository_root.join("crates/hvf/build.rs"))
        .expect("checked HVF build script must be readable");
    for required in [
        "mach/mach_exc.defs",
        "bangbang_mach_exc_user.c",
        "bangbang_mach_exc_server.c",
        "xcrun",
        "MACH_LAZY_FAULT_SHIM_SOURCE",
    ] {
        assert!(
            hvf_build.contains(required),
            "HVF public-Mach build must retain {required}"
        );
    }

    let mach_source =
        std::fs::read_to_string(repository_root.join("crates/hvf/native/mach_lazy_fault.c"))
            .expect("checked Mach host-fault source must be readable");
    for required in [
        "mach_make_memory_entry_64",
        "mach_vm_map",
        "task_swap_exception_ports",
        "EXC_MASK_BAD_ACCESS",
        "forward_exception",
        "restore_previous_if_current",
        "bangbang_mach_lazy_mapping_zero_hidden",
        "BANGBANG_MACH_TERMINAL_EXIT_CODE = 70",
    ] {
        assert!(
            mach_source.contains(required),
            "Mach host-fault source must retain {required}"
        );
    }

    let host_bridge =
        std::fs::read_to_string(repository_root.join("crates/hvf/src/lazy_host_fault.rs"))
            .expect("checked host-fault bridge source must be readable");
    for required in [
        "pub trait HvfLazyPageSource",
        "pub struct HvfLazyPageResolver",
        "pub struct HvfLazyHostFaultBridge",
        "pub struct HvfLazyGuestMemoryConsumer",
        "into_guest_memory_consumer",
        "composite_consumer_resolves_ordinary_access_and_is_send_sync",
        "resolve_host_address",
        "pub fn remove_pages",
        "begin_transition_write",
        "removal_supersedes_blocked_population_and_retries_the_stale_response",
        "removal_revokes_guest_permissions_and_refaults_zero_under_a_new_generation",
        "assume_initialized_by_platform",
        "owner_busy_install_rolls_back_candidate_aliases_without_protection",
        "shutdown_waits_for_an_admitted_host_population",
        "public_diagnostics_redact_fault_authority_and_contents",
    ] {
        assert!(
            host_bridge.contains(required),
            "host-fault bridge must retain {required}"
        );
    }

    let guest_bridge =
        std::fs::read_to_string(repository_root.join("crates/hvf/src/lazy_guest_fault.rs"))
            .expect("checked guest-fault bridge source must be readable");
    for required in [
        "pub(crate) struct HvfLazyGuestFaultHandler",
        "pub enum HvfLazyGuestResolutionFailure",
        "pub struct HvfHandledLazyGuestFault",
        "pub(crate) fn revoke",
        "resolves_every_cross_page_byte_before_publishing_any_permission",
        "publishes_read_write_and_execute_as_serialized_permission_unions",
        "peer_stale_exit_is_admitted_once_then_reports_no_progress",
        "resolver_and_protection_failures_poison_without_later_publication",
        "debug_and_resolution_errors_do_not_expose_guest_addresses",
    ] {
        assert!(
            guest_bridge.contains(required),
            "guest-fault bridge must retain {required}"
        );
    }

    let pager_adapter =
        std::fs::read_to_string(repository_root.join("crates/hvf/src/lazy_pager.rs"))
            .expect("checked HVF pager adapter source must be readable");
    for required in [
        "pub struct HvfLazyPager",
        "CoordinatorTerminalObserver",
        "LazyGuestMemoryTerminalReason::PeerFailure",
        "pager_adapter_completes_remove_refault_and_drained_shutdown",
        "pager_peer_loss_closes_coordinator_once_as_peer_failure",
        "requested_cancel_closes_peer_and_coordinator",
        "reduced_in_flight_selection_fails_before_page_admission",
    ] {
        assert!(
            pager_adapter.contains(required),
            "HVF pager adapter must retain {required}"
        );
    }

    let signed_lifecycle =
        std::fs::read_to_string(repository_root.join("crates/hvf/tests/hvf_lifecycle.rs"))
            .expect("signed HVF lifecycle source must be readable");
    for required in [
        "task_local_lazy_fault_bridge_populates_real_host_accesses_and_repeats",
        "task_local_lazy_fault_bridge_forwards_and_preserves_a_later_owner",
        "task_local_lazy_fault_bridge_uses_fixed_terminal_exit_on_owned_failure",
        "hvf_lazy_guest_faults_populate_execute_read_and_write_before_retry",
        "hvf_lazy_guest_removal_revokes_stage_two_and_refaults_zero",
        "hvf_lazy_guest_two_vcpus_coalesce_one_signed_page_request",
        "hvf_lazy_guest_unowned_instruction_fault_keeps_existing_error_path",
        "hvf_lazy_guest_source_failure_keeps_stage_two_closed_and_cleans_up",
        "hvf_lazy_guest_run_cancellation_does_not_repeat_page_work",
        "map_lazy_guest_memory_with_consumer",
        "bangbang_mach_test_handler_reinstall",
    ] {
        assert!(
            signed_lifecycle.contains(required),
            "signed host-fault evidence must retain {required}"
        );
    }

    let contained_probe = std::fs::read_to_string(
        repository_root.join("crates/bangbang/src/grant_integration_probe.rs"),
    )
    .expect("contained pager probe source must be readable");
    for required in [
        "PagerClient::connect",
        "PagerClientPage::Zero",
        "PagerGeneration::new(4)",
        "run_lazy_consumer_pager",
        "claim_protected_consumer",
        "VirtqueueAvailableRing::new",
        "write_snapshot_memory_image",
    ] {
        assert!(
            contained_probe.contains(required),
            "contained pager probe must retain {required}"
        );
    }

    let signed_guest_boot =
        std::fs::read_to_string(repository_root.join("crates/hvf/tests/guest_boot.rs"))
            .expect("signed guest-boot source must be readable");
    assert!(
        signed_guest_boot.contains("boots_guest_entry_from_a_lazy_instruction_page"),
        "signed guest-boot evidence must retain lazy instruction entry"
    );

    let protocol = std::fs::read_to_string(repository_root.join("docs/snapshot-pager-protocol.md"))
        .expect("checked pager protocol document must be readable");
    for required in [
        "`BBPAGER\\0`",
        "2,097,248",
        "strictly increasing request IDs",
        "Cancellation is session-wide and terminal",
        "Orderly shutdown is drain-only",
        "Runtime anonymous-memory coordinator",
        "HVF integration and removal linearization",
        "`PagerClient` owns the VMM-side live session",
        "retired-operation accounting",
        "only explicit validated `Removed`",
        "not Linux UFFD descriptor or wire compatibility",
    ] {
        assert!(
            protocol.contains(required),
            "pager protocol document must retain {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 354);
    assert_eq!(count(Disposition::AuditRequired), 31);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 3);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 30);
}

#[test]
fn snapshot_diff_rebase_terminal_policy_is_stable() {
    const TERMINAL: [&str; 15] = [
        "api-operation:PUT /snapshot/create",
        "api-path:/snapshot/create",
        "api-property:SnapshotCreateParams.mem_file_path",
        "api-property:SnapshotCreateParams.snapshot_path",
        "api-property:SnapshotCreateParams.snapshot_type",
        "api-schema:SnapshotCreateParams",
        "corpus:snapshot-versioning",
        "firecracker-argument:snapshot-version",
        "semantic.snapshot:diff-dirty-tracking-and-memory-backends",
        "tool-argument:rebase-snap/base-file",
        "tool-argument:rebase-snap/diff-file",
        "tool-argument:snapshot-editor/edit-memory/rebase/diff-path",
        "tool-argument:snapshot-editor/edit-memory/rebase/memory-path",
        "tool-operation:rebase-snap/rebase",
        "tool-operation:snapshot-editor/edit-memory/rebase",
    ];
    const API_TERMINAL: [&str; 6] = [
        "api-operation:PUT /snapshot/create",
        "api-path:/snapshot/create",
        "api-property:SnapshotCreateParams.mem_file_path",
        "api-property:SnapshotCreateParams.snapshot_path",
        "api-property:SnapshotCreateParams.snapshot_type",
        "api-schema:SnapshotCreateParams",
    ];
    const REBASE_SNAP_TERMINAL: [&str; 3] = [
        "tool-argument:rebase-snap/base-file",
        "tool-argument:rebase-snap/diff-file",
        "tool-operation:rebase-snap/rebase",
    ];
    const SNAPSHOT_EDITOR_TERMINAL: [&str; 3] = [
        "tool-argument:snapshot-editor/edit-memory/rebase/diff-path",
        "tool-argument:snapshot-editor/edit-memory/rebase/memory-path",
        "tool-operation:snapshot-editor/edit-memory/rebase",
    ];
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/snapshot-diff-rebase-contract.md"),
    )
    .expect("checked snapshot Diff/rebase contract must be readable");

    assert_eq!(
        inventory.capabilities.len(),
        418,
        "the checked v1.16.0 overlay identity count drifted"
    );
    let expected_ids = TERMINAL.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(expected_ids.len(), 15, "Diff/rebase ledger must stay exact");

    for id in TERMINAL {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("terminal Diff/rebase record must exist: {id}"));
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "terminal Diff/rebase disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal Diff/rebase evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit the")
                && !capability.summary.contains("Continue auditing"),
            "terminal Diff/rebase summary still names placeholder work: {id}"
        );
    }

    let api_implementation = BTreeSet::from([
        "crates/api/src/http.rs",
        "crates/bangbang/src/api_server.rs",
        "crates/bangbang/src/vmm.rs",
        "crates/runtime/src/snapshot.rs",
    ]);
    let api_validation = BTreeSet::from([
        "compat/firecracker/v1.16.0/snapshot-diff-rebase-contract.md",
        "crates/api/src/http.rs",
        "crates/bangbang/src/api_server.rs",
        "crates/bangbang/src/vmm.rs",
        "crates/launcher/tests/production_bundle_e2e.rs",
    ]);
    for id in API_TERMINAL {
        let capability = by_id.get(id).expect("terminal create record must exist");
        assert_eq!(
            local_reference_paths(&capability.implementation),
            Some(api_implementation.clone()),
            "snapshot create implementation evidence drifted: {id}"
        );
        assert_eq!(
            local_reference_paths(&capability.validation),
            Some(api_validation.clone()),
            "snapshot create validation evidence drifted: {id}"
        );
    }

    let tool_validation = BTreeSet::from([
        "compat/firecracker/v1.16.0/snapshot-diff-rebase-contract.md",
        "crates/bangbang/src/vmm.rs",
        "crates/launcher/tests/production_bundle_e2e.rs",
        "crates/runtime/src/snapshot_rebase/tests.rs",
        "tools/snapshot-tools/tests/cli.rs",
    ]);
    for (ids, frontend) in [
        (
            REBASE_SNAP_TERMINAL.as_slice(),
            "tools/snapshot-tools/src/bin/rebase-snap.rs",
        ),
        (
            SNAPSHOT_EDITOR_TERMINAL.as_slice(),
            "tools/snapshot-tools/src/bin/snapshot-editor.rs",
        ),
    ] {
        let implementation = BTreeSet::from([
            "crates/runtime/src/snapshot_rebase.rs",
            frontend,
            "tools/snapshot-tools/src/lib.rs",
        ]);
        for id in ids {
            let capability = by_id
                .get(id)
                .expect("terminal rebase-tool record must exist");
            assert_eq!(
                local_reference_paths(&capability.implementation),
                Some(implementation.clone()),
                "rebase-tool implementation evidence drifted: {id}"
            );
            assert_eq!(
                local_reference_paths(&capability.validation),
                Some(tool_validation.clone()),
                "rebase-tool validation evidence drifted: {id}"
            );
        }
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let contract_ids = rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rows.len(), 15, "Diff/rebase contract row count drifted");
    assert_eq!(
        contract_ids, expected_ids,
        "Diff/rebase contract identity set drifted"
    );
    for id in TERMINAL {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("terminal Diff/rebase row must exist: {id}"));
        assert_eq!(
            contract.matches(&row_prefix).count(),
            1,
            "terminal Diff/rebase row must be unique: {id}"
        );
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
    }
    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "src/firecracker/swagger/firecracker.yaml",
        "docs/snapshotting/snapshot-support.md",
        "src/vmm/src/vstate/vm.rs",
        "src/rebase-snap/src/main.rs",
        "src/snapshot-editor/src/edit_memory.rs",
        "snapshot-editor-contract.md",
        "snapshot-editor state contract",
        "rebase-snap --base-file <path> --diff-file <path>",
        "snapshot-editor edit-memory rebase --memory-path/-m <path> --diff-path/-d <path>",
        "This tool is deprecated and will be removed in the future. Please use 'snapshot-editor' instead.",
        "no state merge",
        "state-last no-clobber pair",
        "Success is exit 0",
        "committed-but-uncertain completion",
        "130/143",
        "current_dynamic_add_and_remove_topologies_write_canonically",
        "repeated_complete_application_handles_add_then_remove",
        "exact_minor_thirteen_diff_closes_all_sixty_four_mmio_and_pci_products",
        "diff_publication_commits_and_zero_root_loads_as_one_closed_pair",
        "diff_load_accepts_a_complete_rebased_result_image",
        "tracked_zero_diff_abort_restores_exact_generation",
        "sparse_cross_directory_and_repeated_rebases_are_exact",
        "every_outer_precommit_stage_failure_preserves_inputs",
        "both_commands_materialize_byte_identical_complete_images",
        "sequential_commands_apply_repeated_lineage_exactly",
        "signed_native_v2_diff_process_loads_zero_root_and_rebased_products",
        "normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox",
        "executable_reports_native_snapshot_versions_before_socket_publication",
        "sandboxed_bundle_reports_current_native_v2_snapshot_version",
        "snapshot-wave6-contract.md",
    ] {
        assert!(
            contract.contains(required),
            "Diff/rebase contract must pin {required}"
        );
    }
}

#[test]
fn snapshot_editor_terminal_policy_is_stable() {
    const TERMINAL: [&str; 12] = [
        "corpus:snapshot-editor",
        "semantic.snapshot:editor-rebase-and-inspection",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/output-path",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/regs",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/vcpu-states/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/version/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/vm-state/vmstate-path",
        "tool-operation:snapshot-editor/edit-vmstate/remove-regs",
        "tool-operation:snapshot-editor/info-vmstate/vcpu-states",
        "tool-operation:snapshot-editor/info-vmstate/version",
        "tool-operation:snapshot-editor/info-vmstate/vm-state",
    ];
    const EDIT: [&str; 4] = [
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/output-path",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/regs",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/vmstate-path",
        "tool-operation:snapshot-editor/edit-vmstate/remove-regs",
    ];
    const INFO: [&str; 6] = [
        "tool-argument:snapshot-editor/info-vmstate/vcpu-states/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/version/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/vm-state/vmstate-path",
        "tool-operation:snapshot-editor/info-vmstate/vcpu-states",
        "tool-operation:snapshot-editor/info-vmstate/version",
        "tool-operation:snapshot-editor/info-vmstate/vm-state",
    ];
    const AGGREGATE: [&str; 2] = [
        "corpus:snapshot-editor",
        "semantic.snapshot:editor-rebase-and-inspection",
    ];
    const WAVE_6_TERMINAL: [&str; 2] = [
        "corpus:snapshot-versioning",
        "semantic.snapshot:diff-dirty-tracking-and-memory-backends",
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/snapshot-editor-contract.md"),
    )
    .expect("checked snapshot-editor contract must be readable");
    let rebase_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/snapshot-diff-rebase-contract.md"),
    )
    .expect("checked snapshot Diff/rebase contract must be readable");

    let expected_ids = TERMINAL.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        expected_ids.len(),
        12,
        "snapshot-editor terminal identity set must stay exact"
    );
    for id in TERMINAL {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("terminal snapshot-editor record must exist: {id}"));
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "snapshot-editor terminal disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "snapshot-editor terminal evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit the")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("issues/1542"),
            "snapshot-editor summary still names pending work: {id}"
        );
    }

    let edit_implementation = BTreeSet::from([
        "crates/hvf/src/snapshot_document/register_removal.rs",
        "crates/runtime/src/snapshot_state_edit.rs",
        "crates/runtime/src/snapshot_state_edit/unix.rs",
        "tools/snapshot-tools/src/bin/snapshot-editor.rs",
        "tools/snapshot-tools/src/lib.rs",
    ]);
    let edit_validation = BTreeSet::from([
        "compat/firecracker/v1.16.0/snapshot-editor-contract.md",
        "crates/hvf/src/snapshot_document/register_removal/tests.rs",
        "crates/launcher/tests/production_bundle_e2e.rs",
        "crates/runtime/src/snapshot_state_edit/tests.rs",
        "scripts/run-integration-tests.sh",
        "tools/snapshot-tools/tests/cli.rs",
    ]);
    for id in EDIT {
        let capability = by_id.get(id).expect("terminal edit record must exist");
        assert_eq!(
            local_reference_paths(&capability.implementation),
            Some(edit_implementation.clone()),
            "snapshot-editor edit implementation evidence drifted: {id}"
        );
        assert_eq!(
            local_reference_paths(&capability.validation),
            Some(edit_validation.clone()),
            "snapshot-editor edit validation evidence drifted: {id}"
        );
    }

    let info_implementation = BTreeSet::from([
        "crates/hvf/src/snapshot_document/inspection.rs",
        "tools/snapshot-tools/src/bin/snapshot-editor.rs",
        "tools/snapshot-tools/src/lib.rs",
    ]);
    let info_validation = BTreeSet::from([
        "compat/firecracker/v1.16.0/snapshot-editor-contract.md",
        "crates/hvf/src/snapshot_document/inspection/tests.rs",
        "crates/launcher/tests/production_bundle_e2e.rs",
        "scripts/run-integration-tests.sh",
        "tools/snapshot-tools/tests/cli.rs",
    ]);
    for id in INFO {
        let capability = by_id.get(id).expect("terminal info record must exist");
        assert_eq!(
            local_reference_paths(&capability.implementation),
            Some(info_implementation.clone()),
            "snapshot-editor info implementation evidence drifted: {id}"
        );
        assert_eq!(
            local_reference_paths(&capability.validation),
            Some(info_validation.clone()),
            "snapshot-editor info validation evidence drifted: {id}"
        );
    }

    let aggregate_implementation = BTreeSet::from([
        "crates/hvf/src/snapshot_document/inspection.rs",
        "crates/hvf/src/snapshot_document/register_removal.rs",
        "crates/runtime/src/snapshot_rebase.rs",
        "crates/runtime/src/snapshot_state_edit.rs",
        "crates/runtime/src/snapshot_state_edit/unix.rs",
        "tools/snapshot-tools/src/bin/snapshot-editor.rs",
        "tools/snapshot-tools/src/lib.rs",
    ]);
    let aggregate_validation = BTreeSet::from([
        "compat/firecracker/v1.16.0/snapshot-diff-rebase-contract.md",
        "compat/firecracker/v1.16.0/snapshot-editor-contract.md",
        "crates/bangbang/src/vmm.rs",
        "crates/hvf/src/snapshot_document/inspection/tests.rs",
        "crates/hvf/src/snapshot_document/register_removal/tests.rs",
        "crates/launcher/tests/production_bundle_e2e.rs",
        "crates/runtime/src/snapshot_rebase/tests.rs",
        "crates/runtime/src/snapshot_state_edit/tests.rs",
        "scripts/run-integration-tests.sh",
        "tools/snapshot-tools/tests/cli.rs",
    ]);
    for id in AGGREGATE {
        let capability = by_id
            .get(id)
            .expect("terminal snapshot-editor aggregate must exist");
        assert_eq!(
            local_reference_paths(&capability.implementation),
            Some(aggregate_implementation.clone()),
            "snapshot-editor aggregate implementation evidence drifted: {id}"
        );
        assert_eq!(
            local_reference_paths(&capability.validation),
            Some(aggregate_validation.clone()),
            "snapshot-editor aggregate validation evidence drifted: {id}"
        );
    }

    for id in WAVE_6_TERMINAL {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("Wave 6 terminal record must exist: {id}"));
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "Wave 6 composed record must remain terminal: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "Wave 6 composed record must retain terminal evidence: {id}"
        );
        assert!(
            !capability.summary.contains("issues/1543"),
            "Wave 6 composed record must not retain pending #1543 ownership: {id}"
        );
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let contract_ids = rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rows.len(), 12, "snapshot-editor contract row count drifted");
    assert_eq!(
        contract_ids, expected_ids,
        "snapshot-editor contract identity set drifted"
    );
    for id in TERMINAL {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("terminal snapshot-editor row must exist: {id}"));
        assert_eq!(contract.matches(&row_prefix).count(), 1);
        assert_eq!(rebase_contract.matches(&row_prefix).count(), 0);
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
    }

    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "src/snapshot-editor/src/main.rs",
        "src/snapshot-editor/src/info.rs",
        "src/snapshot-editor/src/edit_vmstate.rs",
        "docs/snapshotting/snapshot-editor.md",
        "snapshot-diff-rebase-contract.md",
        "bangbang.snapshot-editor.info.v1",
        "67 `u64` IDs",
        "0/1/2/3/130/143",
        "normal_bundle_adopts_native_v2_snapshot_grants_for_create_describe_and_restore",
        "normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox",
        "Full 2.12 and Diff 2.13",
        "MMIO and PCI",
        "DBGBVR0",
        "vcpus[*].debug.reviewed",
        "codesign --verify --strict",
        "resumes only on request",
        "SYSTEM_OFF",
        "Firecracker bitcode",
        "snapshot-wave6-contract.md",
    ] {
        assert!(
            contract.contains(required),
            "snapshot-editor contract must pin {required}"
        );
    }
}

#[test]
fn snapshot_wave6_terminal_policy_is_stable() {
    const API: [&str; 26] = [
        "api-operation:PUT /snapshot/create",
        "api-operation:PUT /snapshot/load",
        "api-path:/snapshot/create",
        "api-path:/snapshot/load",
        "api-property:SnapshotCreateParams.mem_file_path",
        "api-property:SnapshotCreateParams.snapshot_path",
        "api-property:SnapshotCreateParams.snapshot_type",
        "api-property:SnapshotLoadParams.clock_realtime",
        "api-property:SnapshotLoadParams.enable_diff_snapshots",
        "api-property:SnapshotLoadParams.mem_backend",
        "api-property:SnapshotLoadParams.mem_file_path",
        "api-property:SnapshotLoadParams.network_overrides",
        "api-property:SnapshotLoadParams.resume_vm",
        "api-property:SnapshotLoadParams.snapshot_path",
        "api-property:SnapshotLoadParams.track_dirty_pages",
        "api-property:SnapshotLoadParams.vsock_override",
        "api-property:MemoryBackend.backend_path",
        "api-property:MemoryBackend.backend_type",
        "api-property:NetworkOverride.host_dev_name",
        "api-property:NetworkOverride.iface_id",
        "api-property:VsockOverride.uds_path",
        "api-schema:SnapshotCreateParams",
        "api-schema:SnapshotLoadParams",
        "api-schema:MemoryBackend",
        "api-schema:NetworkOverride",
        "api-schema:VsockOverride",
    ];
    const SNAPSHOTS: [&str; 27] = [
        "corpus:snapshot-editor",
        "corpus:snapshot-network-clones",
        "corpus:snapshot-page-faults",
        "corpus:snapshot-random-clones",
        "corpus:snapshot-support",
        "corpus:snapshot-versioning",
        "semantic.snapshot:diff-dirty-tracking-and-memory-backends",
        "semantic.snapshot:editor-rebase-and-inspection",
        "semantic.snapshot:full-create-load-and-public-lifecycle",
        "semantic.snapshot:multi-vcpu-drives-devices-and-mmds",
        "semantic.snapshot:network-vsock-overrides-portability-and-clones",
        "tool-argument:rebase-snap/base-file",
        "tool-argument:rebase-snap/diff-file",
        "tool-argument:snapshot-editor/edit-memory/rebase/diff-path",
        "tool-argument:snapshot-editor/edit-memory/rebase/memory-path",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/output-path",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/regs",
        "tool-argument:snapshot-editor/edit-vmstate/remove-regs/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/vcpu-states/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/version/vmstate-path",
        "tool-argument:snapshot-editor/info-vmstate/vm-state/vmstate-path",
        "tool-operation:rebase-snap/rebase",
        "tool-operation:snapshot-editor/edit-memory/rebase",
        "tool-operation:snapshot-editor/edit-vmstate/remove-regs",
        "tool-operation:snapshot-editor/info-vmstate/vcpu-states",
        "tool-operation:snapshot-editor/info-vmstate/version",
        "tool-operation:snapshot-editor/info-vmstate/vm-state",
    ];
    const PROCESS: [&str; 1] = ["firecracker-argument:snapshot-version"];
    const PRODUCERS: [&str; 16] = [
        "corpus:ballooning",
        "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
        "corpus:memory-hotplug",
        "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
        "corpus:entropy",
        "semantic.device:entropy-queues-limits-metrics-and-state",
        "semantic.device:serial-stdin-stdout-rx-and-restore",
        "semantic.device:rtc-vmclock-vmgenid-and-pvtime",
        "corpus:pmem",
        "semantic.storage:pmem-root-mapping-flush-and-state",
        "corpus:mmds-user-guide",
        "corpus:network-setup",
        "semantic.mmds:tcp-token-session-and-isolation",
        "semantic.network:virtio-net-vmnet-policy-and-connectivity",
        "corpus:vsock",
        "semantic.vsock:snapshot-override-reset-and-rx-gating",
    ];
    const RETAINED: [(&str, &[&str]); 2] = [
        (
            "corpus:network-setup",
            &["https://github.com/seven332/bangbang/issues/1378"],
        ),
        (
            "semantic.network:virtio-net-vmnet-policy-and-connectivity",
            &[
                "https://github.com/seven332/bangbang/issues/1378",
                "https://github.com/seven332/bangbang/issues/1491",
            ],
        ),
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let retained = RETAINED
        .iter()
        .map(|(id, owners)| (*id, *owners))
        .collect::<BTreeMap<_, _>>();

    let groups = [
        API.as_slice(),
        SNAPSHOTS.as_slice(),
        PROCESS.as_slice(),
        PRODUCERS.as_slice(),
    ];
    let mut wave_ids = BTreeSet::new();
    for group in groups {
        for id in group {
            assert!(wave_ids.insert(*id), "Wave 6 identity is duplicated: {id}");
        }
    }
    assert_eq!(wave_ids.len(), 70, "Wave 6 identity set must stay exact");
    assert_eq!(retained.len(), 2, "Wave 6 retained set must stay exact");

    let mut terminal_count = 0;
    for id in &wave_ids {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("Wave 6 capability must exist: {id}"));
        if let Some(owners) = retained.get(id) {
            assert_eq!(
                capability.disposition,
                Disposition::AuditRequired,
                "downstream-owned network aggregate must remain nonterminal: {id}"
            );
            assert!(
                capability.implementation.is_empty() && capability.validation.is_empty(),
                "nonterminal network aggregate must not carry terminal evidence: {id}"
            );
            for owner in *owners {
                assert!(
                    capability.summary.contains(owner),
                    "nonterminal network aggregate must name {owner}: {id}"
                );
            }
            continue;
        }

        terminal_count += 1;
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "Wave 6 terminal disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "Wave 6 terminal evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit the")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("issues/1543"),
            "Wave 6 terminal summary still names pending work: {id}"
        );
    }
    assert_eq!(terminal_count, 68, "Wave 6 terminal count must stay exact");

    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/snapshot-wave6-contract.md"),
    )
    .expect("checked Wave 6 snapshot contract must be readable");
    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 70, "Wave 6 contract row count drifted");
    let mut contract_rows = BTreeMap::new();
    for row in rows {
        let id = row
            .strip_prefix("| `")
            .and_then(|row| row.split_once("` |"))
            .map(|(id, _)| id)
            .expect("Wave 6 ledger row must expose one identity");
        assert!(
            contract_rows.insert(id, row).is_none(),
            "Wave 6 ledger identity is duplicated: {id}"
        );
    }
    assert_eq!(
        contract_rows.keys().copied().collect::<BTreeSet<_>>(),
        wave_ids,
        "Wave 6 contract identity set drifted"
    );
    for id in &wave_ids {
        let row = contract_rows
            .get(id)
            .unwrap_or_else(|| panic!("Wave 6 contract row must exist: {id}"));
        if let Some(owners) = retained.get(id) {
            assert!(row.contains("`audit-required`"));
            assert!(!row.ends_with("| `terminal` |"));
            for owner in *owners {
                assert!(row.contains(owner), "Wave 6 row must name {owner}: {id}");
            }
        } else {
            assert!(row.contains("`implemented-and-verified`"));
            assert!(row.ends_with("| `terminal` |"));
        }
    }

    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "src/firecracker/swagger/firecracker.yaml",
        "docs/snapshotting/snapshot-support.md",
        "docs/snapshotting/versioning.md",
        "docs/snapshotting/random-for-clones.md",
        "docs/snapshotting/network-for-clones.md",
        "docs/snapshotting/handling-page-faults-on-snapshot-resume.md",
        "docs/snapshotting/snapshot-editor.md",
        "bangbang-pager-v1",
        "normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci",
        "assert_production_snapshot_time_identity_transition",
        "exact_minor_thirteen_diff_closes_all_sixty_four_mmio_and_pci_products",
        "current_dynamic_add_and_remove_topologies_write_canonically",
        "repeated_complete_application_handles_add_then_remove",
        "diff_publication_commits_and_zero_root_loads_as_one_closed_pair",
        "diff_load_accepts_a_complete_rebased_result_image",
        "signed_native_v2_diff_process_loads_zero_root_and_rebased_products",
        "normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox",
        "zero tested distinct-physical-host success pairs",
        "https://github.com/seven332/bangbang/issues/1378",
        "https://github.com/seven332/bangbang/issues/1491",
        "Wave 8",
        "block- or per-drive-override field",
    ] {
        assert!(
            contract.contains(required),
            "Wave 6 snapshot contract must pin {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 354);
    assert_eq!(count(Disposition::AuditRequired), 31);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 3);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 30);
}

#[test]
fn native_v2_full_and_diff_summary_policy_is_stable() {
    const RECONCILED: [&str; 8] = [
        "api-path:/pmem/{id}",
        "api-schema:Pmem",
        "corpus:device-hotplug",
        "corpus:memory-hotplug",
        "corpus:pmem",
        "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
        "semantic.storage:pmem-root-mapping-flush-and-state",
        "semantic.transport:pci-msi-and-coexistence",
    ];
    const SNAPSHOT_AGGREGATE: &str = "semantic.snapshot:multi-vcpu-drives-devices-and-mmds";

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(RECONCILED.into_iter().collect::<BTreeSet<_>>().len(), 8);
    for id in RECONCILED {
        let capability = by_id
            .get(id)
            .unwrap_or_else(|| panic!("reconciled native-v2 summary must exist: {id}"));
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "reconciled native-v2 summary must remain terminal: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "reconciled native-v2 summary must retain concrete evidence: {id}"
        );
        assert!(
            capability.summary.contains("Full 2.12") && capability.summary.contains("Diff 2.13"),
            "reconciled summary must distinguish Full 2.12 from Diff 2.13: {id}"
        );
        let summary = capability.summary.to_ascii_lowercase();
        assert!(
            !summary.contains("current native-v2 2.11")
                && !summary.contains("current native-v2 2.12")
                && !summary.contains("current 2.12")
                && !summary.contains("vsock snapshot persistence remains outside")
                && !summary.contains("diff, native-v2 uffd"),
            "reconciled summary retains stale native-v2 wording: {id}"
        );
    }

    let transport = by_id
        .get("semantic.transport:pci-msi-and-coexistence")
        .expect("terminal PCI transport aggregate must exist");
    assert!(
        transport.summary.contains("exact native-v2 2.11")
            && transport.summary.contains("vsock")
            && transport.summary.contains("mandatory Diff state"),
        "PCI transport summary must retain the exact 2.11/Full 2.12/Diff 2.13 ladder"
    );

    let snapshot = by_id
        .get(SNAPSHOT_AGGREGATE)
        .expect("terminal snapshot aggregate must exist");
    assert_eq!(
        snapshot.disposition,
        Disposition::ImplementedAndVerified,
        "already-current snapshot aggregate must remain terminal"
    );
    assert!(
        snapshot.summary.contains("Full 2.12 and Diff 2.13")
            && snapshot
                .summary
                .contains("all 64 optional-component products"),
        "snapshot aggregate must retain its already-current Full/Diff product claim"
    );
}

#[test]
fn network_mmds_closure_policy_is_stable() {
    const TERMINAL: [&str; 33] = [
        "api-operation:GET /mmds",
        "api-operation:PATCH /mmds",
        "api-operation:PATCH /network-interfaces/{iface_id}",
        "api-operation:PUT /mmds",
        "api-operation:PUT /mmds/config",
        "api-operation:PUT /network-interfaces/{iface_id}",
        "api-path:/mmds",
        "api-path:/mmds/config",
        "api-path:/network-interfaces/{iface_id}",
        "api-property:FullVmConfiguration.mmds-config",
        "api-property:FullVmConfiguration.network-interfaces",
        "api-property:MmdsConfig.imds_compat",
        "api-property:MmdsConfig.ipv4_address",
        "api-property:MmdsConfig.network_interfaces",
        "api-property:MmdsConfig.version",
        "api-property:NetworkInterface.guest_mac",
        "api-property:NetworkInterface.host_dev_name",
        "api-property:NetworkInterface.iface_id",
        "api-property:NetworkInterface.mtu",
        "api-property:NetworkInterface.rx_rate_limiter",
        "api-property:NetworkInterface.tx_rate_limiter",
        "api-property:PartialNetworkInterface.iface_id",
        "api-property:PartialNetworkInterface.rx_rate_limiter",
        "api-property:PartialNetworkInterface.tx_rate_limiter",
        "api-schema:MmdsConfig",
        "api-schema:MmdsContentsObject",
        "api-schema:NetworkInterface",
        "api-schema:PartialNetworkInterface",
        "corpus:mmds-design",
        "corpus:mmds-user-guide",
        "corpus:patch-network-interface",
        "non-swagger-route:DELETE /network-interfaces/{iface_id}",
        "semantic.mmds:tcp-token-session-and-isolation",
    ];
    const RETAINED: [(&str, &[&str], &str); 2] = [
        (
            "corpus:network-setup",
            &["https://github.com/seven332/bangbang/issues/1378"],
            "`EXTERNAL-GATE`",
        ),
        (
            "semantic.network:virtio-net-vmnet-policy-and-connectivity",
            &[
                "https://github.com/seven332/bangbang/issues/1378",
                "https://github.com/seven332/bangbang/issues/1491",
            ],
            "`EXTERNAL-GATE + W7`",
        ),
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/network-mmds-contract.md"),
    )
    .expect("checked network/MMDS contract must be readable");

    let expected_ids = TERMINAL
        .into_iter()
        .chain(RETAINED.iter().map(|(id, _, _)| *id))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_ids.len(),
        35,
        "network/MMDS ledger must stay exact"
    );

    for id in TERMINAL {
        let capability = by_id
            .get(id)
            .expect("terminal network/MMDS record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "terminal network/MMDS disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal network/MMDS evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit ")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("current live subset"),
            "terminal network/MMDS summary still names future audit work: {id}"
        );
    }

    for (id, owner_urls, downstream) in RETAINED {
        let capability = by_id
            .get(id)
            .expect("retained network/MMDS record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "retained network/MMDS disposition drifted: {id}"
        );
        for owner_url in owner_urls {
            assert!(
                capability.summary.contains(owner_url),
                "retained network/MMDS summary must name {owner_url}: {id}"
            );
        }
        for outcome in ["Exact native-v2 2.11", "restor", "clone"] {
            assert!(
                capability.summary.contains(outcome),
                "retained network/MMDS summary must retain delivered {outcome}: {id}"
            );
        }
        if id.contains("network") {
            assert!(
                capability.summary.contains("connectivity"),
                "retained network summary must name missing connectivity: {id}"
            );
        }
        if id.starts_with("semantic.network") {
            assert!(
                capability.summary.contains("performance")
                    && capability.summary.contains("observability"),
                "retained network semantic must name Wave 7 outcomes"
            );
        }

        let row_prefix = format!("| `{id}` |");
        let row = contract
            .lines()
            .find(|line| line.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("network/MMDS contract row must exist: {id}"));
        assert!(
            row.contains("`audit-required`") && row.ends_with(&format!("| {downstream} |")),
            "retained network/MMDS ledger row has the wrong handoff: {id}"
        );
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let contract_ids = rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rows.len(), 35, "network/MMDS contract row count drifted");
    assert_eq!(
        contract_ids, expected_ids,
        "network/MMDS identity set drifted"
    );
    for id in TERMINAL {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("terminal network/MMDS row must exist: {id}"));
        assert_eq!(
            contract.matches(&row_prefix).count(),
            1,
            "network/MMDS contract row must be unique: {id}"
        );
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
    }

    for required in [
        "https://github.com/seven332/bangbang/issues/1378",
        "https://github.com/seven332/bangbang/issues/1491",
        "boots_signed_mmio_guest_with_complete_virtio_network_semantics",
        "boots_signed_pci_guest_with_complete_virtio_network_semantics",
        "capture_ready_network_traverses_signed_mmio_and_pci_owners",
        "signed_executable_serves_mmds_on_two_isolated_guest_interfaces",
        "signed_executable_keeps_concurrent_mmds_processes_isolated",
        "signed_executable_hotplugs_mmds_network_and_reuses_product_pci_slot",
        "normal_bundle_hotplugs_mmds_network_without_vmnet_authority",
        "networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation",
        "signed_executable_certifies_native_v2_network_mmds_snapshot_continuation",
        "normal_bundle_certifies_native_v2_network_mmds_snapshot_continuation_and_containment",
        "bangbang vmnet preflight: blocked",
    ] {
        assert!(
            contract.contains(required),
            "network/MMDS contract must pin {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 354);
    assert_eq!(count(Disposition::AuditRequired), 31);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 3);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 30);
}

#[test]
fn vsock_closure_policy_is_stable() {
    const TERMINAL: [&str; 14] = [
        "api-operation:PUT /vsock",
        "api-path:/vsock",
        "api-property:FullVmConfiguration.vsock",
        "api-property:SnapshotLoadParams.vsock_override",
        "api-property:Vsock.guest_cid",
        "api-property:Vsock.uds_path",
        "api-property:Vsock.vsock_id",
        "api-property:VsockOverride.uds_path",
        "api-schema:Vsock",
        "api-schema:VsockOverride",
        "corpus:vsock",
        "semantic.snapshot:network-vsock-overrides-portability-and-clones",
        "semantic.vsock:live-routing-credit-events-and-cleanup",
        "semantic.vsock:snapshot-override-reset-and-rx-gating",
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/vsock-contract.md"),
    )
    .expect("checked vsock contract must be readable");

    assert_eq!(
        inventory.capabilities.len(),
        418,
        "the checked v1.16.0 overlay identity count drifted"
    );
    let expected_ids = TERMINAL.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(expected_ids.len(), 14, "vsock ledger must stay exact");

    for id in TERMINAL {
        let capability = by_id.get(id).expect("terminal vsock record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "terminal vsock disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal vsock evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit ")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("current live subset")
                && !capability.summary.contains("#1518")
                && !capability.summary.contains("Wave 6")
                && !capability.summary.contains("still owns"),
            "terminal vsock summary still names future certification: {id}"
        );
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let contract_ids = rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rows.len(), 14, "vsock contract row count drifted");
    assert_eq!(
        contract_ids, expected_ids,
        "vsock contract identity set drifted"
    );

    for id in TERMINAL {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("terminal vsock row must exist: {id}"));
        assert_eq!(
            contract.matches(&row_prefix).count(),
            1,
            "terminal vsock contract row must be unique: {id}"
        );
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
        for evidence in ["FC-", "FOCUSED-", "SIGNED-"] {
            assert!(
                row.contains(evidence),
                "terminal vsock row must contain {evidence} evidence: {id}"
            );
        }
    }

    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "src/firecracker/swagger/firecracker.yaml",
        "src/firecracker/src/api_server/request/vsock.rs",
        "src/vmm/src/devices/virtio/vsock/persist.rs",
        "tests/integration_tests/functional/test_vsock.py",
        "https://github.com/seven332/bangbang/issues/1491",
        "parses_put_vsock_with_deprecated_vsock_id",
        "snapshot_vsock_selectors_resolve_before_resource_access_and_redact_values",
        "virtio_vsock_transport_reset_publishes_event_and_mmio_interrupt",
        "virtio_vsock_restored_gate_keeps_tx_live_and_buffers_generated_rx",
        "signed_executable_handles_guest_initiated_vsock_from_direct_rootfs",
        "signed_executable_handles_guest_initiated_vsock_multistream_from_direct_rootfs",
        "signed_executable_handles_host_initiated_vsock_to_direct_rootfs",
        "signed_executable_handles_host_initiated_vsock_multistream_to_direct_rootfs",
        "signed_executable_resets_live_vsock_before_unsupported_snapshot_over_mmio",
        "signed_executable_resets_live_vsock_before_unsupported_snapshot_over_product_pci",
        "signed_executable_certifies_native_v2_vsock_snapshot_over_mmio",
        "signed_executable_certifies_native_v2_vsock_snapshot_over_product_pci",
        "capture_ready_vsock_resets_signed_mmio_and_pci_owners",
        "normal_bundle_routes_guest_vsock_through_launcher_broker_without_helpers",
        "normal_bundle_routes_host_vsock_through_supplied_granted_listener",
        "normal_bundle_certifies_native_v2_vsock_restored_guest_lifecycle_and_containment",
    ] {
        assert!(
            contract.contains(required),
            "vsock contract must pin {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 354);
    assert_eq!(count(Disposition::AuditRequired), 31);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 3);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 30);
}

#[test]
fn delivery_closure_policy_is_stable() {
    const IMPLEMENTED_ORIGINAL: [&str; 5] = [
        "corpus:cpu-boot-protocol",
        "semantic.boot:kernel-rootfs-fdt-and-cache",
        "semantic.lifecycle:pause-resume-quiescence-and-failure",
        "semantic.lifecycle:smp-psci-and-vcpu-ownership",
        "semantic.memory:machine-sizing-hugepages-and-dirty-tracking",
    ];
    const WAVE_7_ORIGINAL: [&str; 12] = [
        "corpus:cpu-template-helper",
        "corpus:cpu-templates",
        "corpus:rootfs-and-kernel",
        "semantic.cpu:configuration-templates-and-feature-state",
        "tool-argument:cpu-template-helper/fingerprint/compare/curr",
        "tool-argument:cpu-template-helper/fingerprint/compare/filters",
        "tool-argument:cpu-template-helper/fingerprint/compare/prev",
        "tool-argument:cpu-template-helper/fingerprint/dump/config",
        "tool-argument:cpu-template-helper/fingerprint/dump/output",
        "tool-argument:cpu-template-helper/fingerprint/dump/template",
        "tool-operation:cpu-template-helper/fingerprint/compare",
        "tool-operation:cpu-template-helper/fingerprint/dump",
    ];
    const PROMOTED_API: [&str; 18] = [
        "api-operation:GET /machine-config",
        "api-operation:PATCH /machine-config",
        "api-operation:PUT /boot-source",
        "api-operation:PUT /cpu-config",
        "api-operation:PUT /machine-config",
        "api-path:/boot-source",
        "api-path:/cpu-config",
        "api-path:/machine-config",
        "api-path:/vm",
        "api-property:BootSource.boot_args",
        "api-property:BootSource.initrd_path",
        "api-property:BootSource.kernel_image_path",
        "api-property:FullVmConfiguration.boot-source",
        "api-property:FullVmConfiguration.machine-config",
        "api-property:Vm.state",
        "api-schema:BootSource",
        "api-schema:MachineConfiguration",
        "api-schema:Vm",
    ];
    const RUNTIME_BLOCK_HOTPLUG: [&str; 2] = [
        "api-operation:PUT /drives/{drive_id}",
        "non-swagger-route:DELETE /drives/{drive_id}",
    ];
    const RUNTIME_PMEM_HOTPLUG: [&str; 3] = [
        "api-operation:PUT /pmem/{id}",
        "api-path:/pmem/{id}",
        "non-swagger-route:DELETE /pmem/{id}",
    ];
    const RUNTIME_NETWORK_HOTPLUG: [&str; 2] = [
        "api-operation:PUT /network-interfaces/{iface_id}",
        "non-swagger-route:DELETE /network-interfaces/{iface_id}",
    ];
    const NETWORK_SNAPSHOT_TERMINAL: [&str; 4] = [
        "api-property:SnapshotLoadParams.network_overrides",
        "corpus:mmds-user-guide",
        "corpus:snapshot-network-clones",
        "semantic.mmds:tcp-token-session-and-isolation",
    ];
    const PCI_RUNTIME_HOTPLUG_AGGREGATES: [&str; 3] = [
        "corpus:device-hotplug",
        "semantic.hotplug:runtime-device-manager",
        "semantic.transport:pci-msi-and-coexistence",
    ];
    const STORAGE_TERMINAL: [&str; 40] = [
        "api-operation:PATCH /drives/{drive_id}",
        "api-operation:PATCH /pmem/{id}",
        "api-operation:PUT /drives/{drive_id}",
        "api-operation:PUT /pmem/{id}",
        "api-path:/drives/{drive_id}",
        "api-path:/pmem/{id}",
        "api-property:Drive.cache_type",
        "api-property:Drive.drive_id",
        "api-property:Drive.io_engine",
        "api-property:Drive.is_read_only",
        "api-property:Drive.is_root_device",
        "api-property:Drive.partuuid",
        "api-property:Drive.path_on_host",
        "api-property:Drive.rate_limiter",
        "api-property:Drive.socket",
        "api-property:FullVmConfiguration.drives",
        "api-property:FullVmConfiguration.pmem",
        "api-property:PartialDrive.drive_id",
        "api-property:PartialDrive.path_on_host",
        "api-property:PartialDrive.rate_limiter",
        "api-property:PartialPmem.id",
        "api-property:PartialPmem.rate_limiter",
        "api-property:Pmem.id",
        "api-property:Pmem.path_on_host",
        "api-property:Pmem.rate_limiter",
        "api-property:Pmem.read_only",
        "api-property:Pmem.root_device",
        "api-schema:Drive",
        "api-schema:PartialDrive",
        "api-schema:PartialPmem",
        "api-schema:Pmem",
        "corpus:block-caching",
        "corpus:block-io-engine",
        "corpus:block-vhost-user",
        "corpus:patch-block",
        "corpus:pmem",
        "non-swagger-route:DELETE /drives/{drive_id}",
        "non-swagger-route:DELETE /pmem/{id}",
        "semantic.storage:block-sync-async-vhost-and-limits",
        "semantic.storage:pmem-root-mapping-flush-and-state",
    ];
    const BALLOON_TERMINAL: [&str; 52] = [
        "api-operation:GET /balloon",
        "api-operation:GET /balloon/hinting/status",
        "api-operation:GET /balloon/statistics",
        "api-operation:PATCH /balloon",
        "api-operation:PATCH /balloon/hinting/start",
        "api-operation:PATCH /balloon/hinting/stop",
        "api-operation:PATCH /balloon/statistics",
        "api-operation:PUT /balloon",
        "api-path:/balloon",
        "api-path:/balloon/hinting/start",
        "api-path:/balloon/hinting/status",
        "api-path:/balloon/hinting/stop",
        "api-path:/balloon/statistics",
        "api-property:Balloon.amount_mib",
        "api-property:Balloon.deflate_on_oom",
        "api-property:Balloon.free_page_hinting",
        "api-property:Balloon.free_page_reporting",
        "api-property:Balloon.stats_polling_interval_s",
        "api-property:BalloonHintingStatus.guest_cmd",
        "api-property:BalloonHintingStatus.host_cmd",
        "api-property:BalloonStartCmd.acknowledge_on_stop",
        "api-property:BalloonStats.actual_mib",
        "api-property:BalloonStats.actual_pages",
        "api-property:BalloonStats.alloc_stall",
        "api-property:BalloonStats.async_reclaim",
        "api-property:BalloonStats.async_scan",
        "api-property:BalloonStats.available_memory",
        "api-property:BalloonStats.direct_reclaim",
        "api-property:BalloonStats.direct_scan",
        "api-property:BalloonStats.disk_caches",
        "api-property:BalloonStats.free_memory",
        "api-property:BalloonStats.hugetlb_allocations",
        "api-property:BalloonStats.hugetlb_failures",
        "api-property:BalloonStats.major_faults",
        "api-property:BalloonStats.minor_faults",
        "api-property:BalloonStats.oom_kill",
        "api-property:BalloonStats.swap_in",
        "api-property:BalloonStats.swap_out",
        "api-property:BalloonStats.target_mib",
        "api-property:BalloonStats.target_pages",
        "api-property:BalloonStats.total_memory",
        "api-property:BalloonStatsUpdate.stats_polling_interval_s",
        "api-property:BalloonUpdate.amount_mib",
        "api-property:FullVmConfiguration.balloon",
        "api-schema:Balloon",
        "api-schema:BalloonHintingStatus",
        "api-schema:BalloonStartCmd",
        "api-schema:BalloonStats",
        "api-schema:BalloonStatsUpdate",
        "api-schema:BalloonUpdate",
        "corpus:ballooning",
        "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
    ];
    const BALLOON_WAVE_6: [&str; 0] = [];
    const TIME_IDENTITY_TERMINAL: [&str; 1] = ["semantic.device:rtc-vmclock-vmgenid-and-pvtime"];

    const MEMORY_HOTPLUG_TERMINAL: [&str; 19] = [
        "api-operation:GET /hotplug/memory",
        "api-operation:PATCH /hotplug/memory",
        "api-operation:PUT /hotplug/memory",
        "api-path:/hotplug/memory",
        "api-property:FullVmConfiguration.memory-hotplug",
        "api-property:MemoryHotplugConfig.block_size_mib",
        "api-property:MemoryHotplugConfig.slot_size_mib",
        "api-property:MemoryHotplugConfig.total_size_mib",
        "api-property:MemoryHotplugSizeUpdate.requested_size_mib",
        "api-property:MemoryHotplugStatus.block_size_mib",
        "api-property:MemoryHotplugStatus.plugged_size_mib",
        "api-property:MemoryHotplugStatus.requested_size_mib",
        "api-property:MemoryHotplugStatus.slot_size_mib",
        "api-property:MemoryHotplugStatus.total_size_mib",
        "api-schema:MemoryHotplugConfig",
        "api-schema:MemoryHotplugSizeUpdate",
        "api-schema:MemoryHotplugStatus",
        "corpus:memory-hotplug",
        "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
    ];
    const MEMORY_HOTPLUG_WAVE_6: [&str; 0] = [];
    const ENTROPY_TERMINAL: [&str; 7] = [
        "api-operation:PUT /entropy",
        "api-path:/entropy",
        "api-property:EntropyDevice.rate_limiter",
        "api-property:FullVmConfiguration.entropy",
        "api-schema:EntropyDevice",
        "corpus:entropy",
        "semantic.device:entropy-queues-limits-metrics-and-state",
    ];
    const ENTROPY_WAVE_6: [&str; 0] = [];
    const SERIAL_TERMINAL: [&str; 6] = [
        "api-operation:PUT /serial",
        "api-path:/serial",
        "api-property:SerialDevice.rate_limiter",
        "api-property:SerialDevice.serial_out_path",
        "api-schema:SerialDevice",
        "semantic.device:serial-stdin-stdout-rx-and-restore",
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        manifest.items.len(),
        381,
        "generated identity count drifted"
    );
    assert_eq!(
        inventory.capabilities.len(),
        418,
        "delivery overlay count drifted"
    );
    assert_eq!(
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.id.starts_with("semantic."))
            .count(),
        37,
        "local semantic identity count drifted"
    );
    assert_eq!(
        by_id.len(),
        inventory.capabilities.len(),
        "capability identities must remain unique"
    );

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 354);
    assert_eq!(count(Disposition::AuditRequired), 31);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 3);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 30);

    for id in IMPLEMENTED_ORIGINAL {
        assert_eq!(
            by_id
                .get(id)
                .expect("implemented original record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "original record must remain implemented: {id}"
        );
    }
    for id in CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS {
        assert_eq!(
            by_id
                .get(id)
                .expect("terminal CPU-template helper record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "CPU-template helper record must remain terminal: {id}"
        );
    }
    for id in CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS {
        assert_eq!(
            by_id
                .get(id)
                .expect("terminal CPU-template strip record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "CPU-template strip record must remain terminal: {id}"
        );
    }
    for id in WAVE_7_ORIGINAL {
        let capability = by_id.get(id).expect("Wave 7 original record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 7 handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 7"),
            "Wave 7 handoff must name its owner: {id}"
        );
    }
    assert_eq!(
        by_id
            .get("corpus:hugepages")
            .expect("hugepages corpus must exist")
            .disposition,
        Disposition::ProvenPlatformImpossible
    );

    let original = IMPLEMENTED_ORIGINAL
        .into_iter()
        .chain(WAVE_7_ORIGINAL)
        .chain(CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS)
        .chain(CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS)
        .chain(["corpus:hugepages"])
        .collect::<BTreeSet<_>>();
    assert_eq!(
        original.len(),
        28,
        "original closure ledger must stay exact"
    );

    for id in PROMOTED_API {
        assert_eq!(
            by_id
                .get(id)
                .expect("promoted API record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "bounded API record must remain terminal: {id}"
        );
    }

    for id in RUNTIME_BLOCK_HOTPLUG {
        assert_eq!(
            by_id
                .get(id)
                .expect("runtime block hotplug record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "runtime block hotplug record must remain implemented: {id}"
        );
    }

    for id in RUNTIME_PMEM_HOTPLUG {
        assert_eq!(
            by_id
                .get(id)
                .expect("runtime pmem hotplug record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "runtime pmem hotplug record must remain implemented: {id}"
        );
    }

    for id in RUNTIME_NETWORK_HOTPLUG {
        assert_eq!(
            by_id
                .get(id)
                .expect("runtime network hotplug record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "runtime network hotplug record must remain implemented: {id}"
        );
    }

    for id in NETWORK_SNAPSHOT_TERMINAL {
        let capability = by_id
            .get(id)
            .expect("terminal network snapshot record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "network snapshot record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal network snapshot evidence must remain concrete: {id}"
        );
        assert!(
            capability.summary.contains("native-v2 2.11"),
            "terminal network snapshot summary must pin the exact format: {id}"
        );
    }

    for id in PCI_RUNTIME_HOTPLUG_AGGREGATES {
        let capability = by_id
            .get(id)
            .expect("PCI/runtime-hotplug aggregate record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "PCI/runtime-hotplug aggregate must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "PCI/runtime-hotplug aggregate must retain concrete evidence: {id}"
        );
    }

    let storage_ids = STORAGE_TERMINAL.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        storage_ids.len(),
        40,
        "storage closure ledger must stay exact"
    );

    for id in STORAGE_TERMINAL {
        let capability = by_id.get(id).expect("terminal storage record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "storage record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal storage record must retain concrete evidence: {id}"
        );
        assert!(
            !capability.summary.contains("#1450")
                && !capability.summary.contains("before promotion")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("broad storage audit"),
            "terminal storage summary still names future storage work: {id}"
        );
    }
    let storage_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/storage-contract.md"),
    )
    .expect("checked storage contract must be readable");
    assert_eq!(
        storage_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        40,
        "checked storage contract must contain each exact ledger row once"
    );
    for id in storage_ids {
        assert_eq!(
            storage_contract.matches(&format!("| `{id}` |")).count(),
            1,
            "checked storage contract row must be unique: {id}"
        );
    }

    let memory_hotplug_ids = MEMORY_HOTPLUG_TERMINAL
        .into_iter()
        .chain(MEMORY_HOTPLUG_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        memory_hotplug_ids.len(),
        19,
        "memory-hotplug closure ledger must stay exact"
    );
    for id in MEMORY_HOTPLUG_TERMINAL {
        let capability = by_id
            .get(id)
            .expect("terminal memory-hotplug record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "memory-hotplug record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal memory-hotplug record must retain concrete evidence: {id}"
        );
    }
    for id in MEMORY_HOTPLUG_WAVE_6 {
        let capability = by_id
            .get(id)
            .expect("Wave 6 memory-hotplug record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 6 memory-hotplug handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 6"),
            "Wave 6 memory-hotplug handoff must name its owner: {id}"
        );
    }

    let memory_hotplug_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/memory-hotplug-contract.md"),
    )
    .expect("checked memory-hotplug contract must be readable");
    assert_eq!(
        memory_hotplug_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        19,
        "checked memory-hotplug contract must contain each exact ledger row once"
    );
    for id in memory_hotplug_ids {
        assert_eq!(
            memory_hotplug_contract
                .matches(&format!("| `{id}` |"))
                .count(),
            1,
            "checked memory-hotplug contract row must be unique: {id}"
        );
    }

    let entropy_ids = ENTROPY_TERMINAL
        .into_iter()
        .chain(ENTROPY_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entropy_ids.len(),
        7,
        "entropy closure ledger must stay exact"
    );
    for id in ENTROPY_TERMINAL {
        let capability = by_id.get(id).expect("terminal entropy record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "entropy record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal entropy record must retain concrete evidence: {id}"
        );
    }
    for id in ENTROPY_WAVE_6 {
        let capability = by_id.get(id).expect("Wave 6 entropy record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 6 entropy handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 6"),
            "Wave 6 entropy handoff must name its owner: {id}"
        );
    }

    let entropy_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/entropy-contract.md"),
    )
    .expect("checked entropy contract must be readable");
    assert_eq!(
        entropy_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        7,
        "checked entropy contract must contain each exact ledger row once"
    );
    for id in entropy_ids {
        assert_eq!(
            entropy_contract.matches(&format!("| `{id}` |")).count(),
            1,
            "checked entropy contract row must be unique: {id}"
        );
    }

    let serial_ids = SERIAL_TERMINAL.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(serial_ids.len(), 6, "serial closure ledger must stay exact");
    for id in SERIAL_TERMINAL {
        let capability = by_id.get(id).expect("terminal serial record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "serial record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal serial record must retain concrete evidence: {id}"
        );
    }
    let serial_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/serial-contract.md"),
    )
    .expect("checked serial contract must be readable");
    assert_eq!(
        serial_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        6,
        "checked serial contract must contain each exact ledger row once"
    );
    for id in serial_ids {
        assert_eq!(
            serial_contract.matches(&format!("| `{id}` |")).count(),
            1,
            "checked serial contract row must be unique: {id}"
        );
    }

    let balloon_ids = BALLOON_TERMINAL
        .into_iter()
        .chain(BALLOON_WAVE_6)
        .collect::<BTreeSet<_>>();
    let memory_hotplug_ids = MEMORY_HOTPLUG_TERMINAL
        .into_iter()
        .chain(MEMORY_HOTPLUG_WAVE_6)
        .collect::<BTreeSet<_>>();
    let entropy_ids = ENTROPY_TERMINAL
        .into_iter()
        .chain(ENTROPY_WAVE_6)
        .collect::<BTreeSet<_>>();
    let serial_ids = SERIAL_TERMINAL.into_iter().collect::<BTreeSet<_>>();
    let time_identity_ids = TIME_IDENTITY_TERMINAL.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(balloon_ids.len(), 52);
    assert_eq!(memory_hotplug_ids.len(), 19);
    assert_eq!(entropy_ids.len(), 7);
    assert_eq!(serial_ids.len(), 6);
    assert_eq!(time_identity_ids.len(), 1);

    let family_sets = [
        &balloon_ids,
        &memory_hotplug_ids,
        &entropy_ids,
        &serial_ids,
        &time_identity_ids,
    ];
    for (index, left) in family_sets.iter().enumerate() {
        for right in family_sets.iter().skip(index + 1) {
            assert!(
                left.is_disjoint(right),
                "remaining-device family ledgers must be disjoint"
            );
        }
    }
    let remaining_ids = family_sets
        .iter()
        .flat_map(|ids| ids.iter().copied())
        .collect::<BTreeSet<_>>();
    assert_eq!(remaining_ids.len(), 85);

    let selected_inventory_ids = inventory
        .capabilities
        .iter()
        .filter(|capability| {
            let id = capability.id.as_str();
            let lower = id.to_ascii_lowercase();
            lower.contains("balloon")
                || lower.contains("entropy")
                || lower.contains("serial")
                || lower.contains("hotplug/memory")
                || lower.contains("memory-hotplug")
                || lower.contains("virtio-mem")
                || id.contains("MemoryHotplug")
                || id == "semantic.device:rtc-vmclock-vmgenid-and-pvtime"
        })
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        selected_inventory_ids, remaining_ids,
        "the reproducible remaining-device selector must resolve to exactly the five ledgers"
    );

    let remaining_terminal_ids = BALLOON_TERMINAL
        .into_iter()
        .chain(MEMORY_HOTPLUG_TERMINAL)
        .chain(ENTROPY_TERMINAL)
        .chain(SERIAL_TERMINAL)
        .chain(TIME_IDENTITY_TERMINAL)
        .collect::<BTreeSet<_>>();
    let remaining_wave_6_ids = BALLOON_WAVE_6
        .into_iter()
        .chain(MEMORY_HOTPLUG_WAVE_6)
        .chain(ENTROPY_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(remaining_terminal_ids.len(), 85);
    assert_eq!(remaining_wave_6_ids.len(), 0);
    assert!(remaining_terminal_ids.is_disjoint(&remaining_wave_6_ids));
    assert_eq!(
        remaining_terminal_ids
            .union(&remaining_wave_6_ids)
            .copied()
            .collect::<BTreeSet<_>>(),
        remaining_ids
    );

    for id in &remaining_terminal_ids {
        let capability = by_id
            .get(id)
            .expect("terminal remaining-device record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "remaining-device terminal disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "remaining-device terminal evidence must remain concrete: {id}"
        );
        assert!(
            !capability.summary.contains("#1440")
                && !capability.summary.contains("#1481")
                && !capability.summary.contains("future remaining-device"),
            "remaining-device terminal summary still names future aggregate work: {id}"
        );
    }
    let ledger_contracts = [
        (
            "balloon-contract.md",
            &balloon_ids,
            "checked balloon contract",
        ),
        (
            "memory-hotplug-contract.md",
            &memory_hotplug_ids,
            "checked memory-hotplug contract",
        ),
        (
            "entropy-contract.md",
            &entropy_ids,
            "checked entropy contract",
        ),
        ("serial-contract.md", &serial_ids, "checked serial contract"),
        (
            "time-identity-contract.md",
            &time_identity_ids,
            "checked time/identity contract",
        ),
    ];
    for (filename, expected_ids, context) in ledger_contracts {
        let contract = std::fs::read_to_string(
            repository_root
                .join("compat/firecracker/v1.16.0")
                .join(filename),
        )
        .unwrap_or_else(|error| panic!("{context} must be readable: {error}"));
        let rows = contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .collect::<Vec<_>>();
        let ids = rows
            .iter()
            .filter_map(|line| {
                line.strip_prefix("| `")
                    .and_then(|line| line.split_once("` |"))
                    .map(|(id, _)| id)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            rows.len(),
            expected_ids.len(),
            "{context} row count drifted"
        );
        assert_eq!(&ids, expected_ids, "{context} identity set drifted");
    }

    let aggregate_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/remaining-device-contract.md"),
    )
    .expect("checked aggregate remaining-device contract must be readable");
    let aggregate_rows = aggregate_contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let aggregate_ids = aggregate_rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(aggregate_rows.len(), 85);
    assert_eq!(aggregate_ids, remaining_ids);
    for id in &remaining_ids {
        let row_prefix = format!("| `{id}` |");
        let row = aggregate_rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("aggregate contract row must exist: {id}"));
        assert_eq!(
            aggregate_contract.matches(&row_prefix).count(),
            1,
            "aggregate contract row must be unique: {id}"
        );
        assert!(
            !row.contains("| `` |"),
            "aggregate evidence key is empty: {id}"
        );
        assert!(
            !row.contains("`W7`"),
            "selected row must not hand off to Wave 7: {id}"
        );
        assert!(remaining_terminal_ids.contains(id));
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
    }
    for required in [
        "https://github.com/seven332/bangbang/issues/1491",
        "snapshot-wave6-contract.md",
        "signed_executable_certifies_remaining_devices_over_mmio",
        "signed_executable_certifies_remaining_devices_over_product_pci",
        "aggregate_remaining_device_snapshot_preflight_failures_preserve_order_and_reuse",
        "remaining_device_owner_budget_covers_mmio_and_pci_and_reuses_resources",
        "normal_bundle_isolates_concurrent_default_serial_stdio_sessions",
    ] {
        assert!(
            aggregate_contract.contains(required),
            "aggregate contract must pin {required}"
        );
    }

    for capability in &inventory.capabilities {
        assert!(
            !capability.summary.contains("awaits #1388")
                && !capability.summary.contains("awaits the #1388")
                && !capability.summary.contains("#1388/Wave"),
            "summary still names #1388 as a future owner: {}",
            capability.id
        );
    }
}
