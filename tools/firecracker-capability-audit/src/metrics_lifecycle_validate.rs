use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    AuditMode, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    METRICS_LIFECYCLE_AUDIT_SCHEMA_VERSION, MetricsLifecycleAudit, MetricsLifecycleBoundary,
    MetricsLifecycleClaim, MetricsLifecycleDisposition, MetricsLifecycleRecord,
    MetricsSchemaAuthority, Reference, ValidationErrors,
};

/// Exact scenario identities certified by the aggregate metrics lifecycle gate.
pub const METRICS_LIFECYCLE_SCENARIO_IDS: [&str; 10] = [
    "metrics.backpressure-loss",
    "metrics.configured-cardinality",
    "metrics.explicit-flush",
    "metrics.hotplug-reuse",
    "metrics.initial-session",
    "metrics.periodic-60s",
    "metrics.process-isolation",
    "metrics.publication-transaction",
    "metrics.snapshot-destination",
    "metrics.terminal-final-attempt",
];

/// Claims that must occur together on the distinguished publication transaction.
pub const METRICS_PUBLICATION_TRANSACTION_CLAIMS: [MetricsLifecycleClaim; 5] = [
    MetricsLifecycleClaim::CompleteLineCommitAtomicity,
    MetricsLifecycleClaim::PreviousSuccessRetry,
    MetricsLifecycleClaim::ConcurrentCutOwnership,
    MetricsLifecycleClaim::LostOutputAccounting,
    MetricsLifecycleClaim::FinalAttemptOnce,
];

/// Validate the exact aggregate metrics lifecycle authority.
pub fn validate_metrics_lifecycle(
    audit: &MetricsLifecycleAudit,
    authority: &MetricsSchemaAuthority,
    repository_root: &Path,
    mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, authority, &mut errors);

    if audit.records.len() != METRICS_LIFECYCLE_SCENARIO_IDS.len() {
        errors.push(format!(
            "metrics lifecycle audit must contain {} records, found {}",
            METRICS_LIFECYCLE_SCENARIO_IDS.len(),
            audit.records.len()
        ));
    }

    let tracked = tracked_repository_files(repository_root, &mut errors);
    let mut records = BTreeMap::new();
    let mut previous = None;
    for record in &audit.records {
        if previous.is_some_and(|id| record.id.as_str() <= id) {
            errors.push("metrics lifecycle records must be sorted and unique by id".to_string());
        }
        previous = Some(record.id.as_str());
        if records.insert(record.id.as_str(), record).is_some() {
            errors.push(format!("duplicate metrics lifecycle record: {}", record.id));
        }
        validate_record(record, repository_root, &tracked, mode, &mut errors);
    }

    let actual = records.keys().copied().collect::<BTreeSet<_>>();
    let expected = METRICS_LIFECYCLE_SCENARIO_IDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    if actual != expected {
        errors.push(format!(
            "metrics lifecycle audit requires the exact scenario set: expected {expected:?}, found {actual:?}"
        ));
    }

    let transaction_claim_owners = audit
        .records
        .iter()
        .filter(|record| {
            record
                .claims
                .iter()
                .any(|claim| METRICS_PUBLICATION_TRANSACTION_CLAIMS.contains(claim))
        })
        .map(|record| record.id.as_str())
        .collect::<BTreeSet<_>>();
    if transaction_claim_owners != BTreeSet::from(["metrics.publication-transaction"]) {
        errors.push(format!(
            "metrics lifecycle combined transaction claims must be owned only by metrics.publication-transaction, found {transaction_claim_owners:?}"
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(
    audit: &MetricsLifecycleAudit,
    authority: &MetricsSchemaAuthority,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != METRICS_LIFECYCLE_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "metrics lifecycle schema_version must be {METRICS_LIFECYCLE_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline != authority.baseline {
        errors.push("metrics lifecycle and metrics schema baselines differ".to_string());
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("metrics lifecycle baseline is not the pinned release".to_string());
    }
}

fn validate_record(
    record: &MetricsLifecycleRecord,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    let Some(spec) = scenario_spec(&record.id) else {
        errors.push(format!("unknown metrics lifecycle scenario: {}", record.id));
        return;
    };

    if record.boundary != spec.boundary {
        errors.push(format!(
            "metrics lifecycle scenario has the wrong boundary: {}",
            record.id
        ));
    }
    if record.disposition != spec.disposition {
        errors.push(format!(
            "metrics lifecycle scenario has the wrong disposition: {}",
            record.id
        ));
    }
    if record.delivery_issue != "#1790" {
        errors.push(format!(
            "metrics lifecycle scenario has the wrong delivery issue: {}",
            record.id
        ));
    }
    if record.claims != spec.claims {
        errors.push(format!(
            "metrics lifecycle scenario has stale claims: {}",
            record.id
        ));
    }
    if record.rationale != spec.rationale {
        errors.push(format!(
            "metrics lifecycle scenario has a stale rationale: {}",
            record.id
        ));
    }

    if !record.disposition.is_terminal() {
        if !record.implementation.is_empty() || !record.validation.is_empty() {
            errors.push(format!(
                "planned metrics lifecycle scenario must not claim terminal evidence: {}",
                record.id
            ));
        }
        if mode == AuditMode::Final {
            errors.push(format!(
                "final metrics lifecycle validation rejects planned record: {}",
                record.id
            ));
        }
        return;
    }

    let expected_implementation = local_references(spec.implementation);
    let expected_validation = local_references(spec.validation);
    if record.implementation != expected_implementation {
        errors.push(format!(
            "metrics lifecycle scenario requires exact implementation evidence: {}",
            record.id
        ));
    }
    if record.validation != expected_validation {
        errors.push(format!(
            "metrics lifecycle scenario requires exact validation evidence: {}",
            record.id
        ));
    }
    validate_references(
        &record.implementation,
        "implementation",
        &record.id,
        repository_root,
        tracked,
        errors,
    );
    validate_references(
        &record.validation,
        "validation",
        &record.id,
        repository_root,
        tracked,
        errors,
    );
}

fn validate_references(
    references: &[Reference],
    kind: &str,
    id: &str,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    if references.is_empty() {
        errors.push(format!(
            "terminal metrics lifecycle scenario requires {kind} evidence: {id}"
        ));
    }
    if references
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left >= right))
    {
        errors.push(format!(
            "metrics lifecycle {kind} references must be sorted and unique: {id}"
        ));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("metrics lifecycle {id} {kind}[{index}]"),
            errors,
        );
        if !matches!(
            reference,
            Reference::Local {
                anchor: Some(_),
                ..
            }
        ) {
            errors.push(format!(
                "metrics lifecycle evidence must be an anchored local reference: {id} {kind}[{index}]"
            ));
        }
        if let Reference::Local {
            path,
            anchor: Some(anchor),
        } = reference
        {
            match std::fs::read_to_string(repository_root.join(path)) {
                Ok(source) if source.contains(anchor) => {}
                Ok(_) => errors.push(format!(
                    "metrics lifecycle evidence anchor does not resolve: {id}: {path}: {anchor}"
                )),
                Err(_) => errors.push(format!(
                    "metrics lifecycle evidence path is unreadable: {id}: {path}"
                )),
            }
        }
    }
}

struct ScenarioSpec {
    boundary: MetricsLifecycleBoundary,
    disposition: MetricsLifecycleDisposition,
    claims: Vec<MetricsLifecycleClaim>,
    rationale: &'static str,
    implementation: &'static [(&'static str, &'static str)],
    validation: &'static [(&'static str, &'static str)],
}

fn scenario_spec(id: &str) -> Option<ScenarioSpec> {
    Some(match id {
        "metrics.backpressure-loss" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::Backpressure,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::BackpressureLossReported],
            rationale: "Nonblocking automatic publication never stalls the VMM indefinitely: a full sink records one missed output, emits only a redacted worker outcome, retains the prior successful baseline, and permits a later scheduled retry.",
            implementation: &[
                (
                    "crates/bangbang/src/vmm.rs",
                    "fn handle_periodic_metrics_flush",
                ),
                ("crates/runtime/src/metrics.rs", "fn write_metrics_line"),
            ],
            validation: &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn no_api_periodic_metrics_failure_reschedules_and_retries_delta",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "fn repeated_failed_flushes_accumulate_missed_metrics",
                ),
            ],
        },
        "metrics.configured-cardinality" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::ConfiguredCardinality,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::ConfiguredDeviceCardinality],
            rationale: "Canonical publication admits only validated configured block, network, and vhost-user block identities, preserves exact static/configured aggregation, and fails closed beyond the 985-root and 64 MiB bounds.",
            implementation: &[
                (
                    "crates/runtime/src/metrics.rs",
                    "pub(crate) fn flush_with_diagnostics_and_devices",
                ),
                (
                    "crates/runtime/src/metrics/firecracker.rs",
                    "pub(super) fn build_metrics_line",
                ),
            ],
            validation: &[
                (
                    "crates/runtime/src/metrics.rs",
                    "fn complete_device_inventory_replays_ambiguous_write_and_preserves_idle_shape",
                ),
                (
                    "crates/runtime/src/metrics/firecracker.rs",
                    "fn maximum_configured_recipe_has_exact_sorted_exclusive_dynamic_roots",
                ),
            ],
        },
        "metrics.explicit-flush" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::ExplicitFlush,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::ExplicitFallible],
            rationale: "Runtime FlushMetrics uses the common publication transaction, returns a stable action error on configured sink failure, and remains a successful no-op when no sink is configured.",
            implementation: &[
                ("crates/bangbang/src/vmm.rs", "fn flush_metrics(&mut self)"),
                (
                    "crates/runtime/src/lib.rs",
                    "pub fn flush_metrics_with_diagnostics",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/src/api_server.rs",
                    "fn returns_state_fault_for_preboot_flush_metrics_without_mutating_state",
                ),
                (
                    "crates/runtime/src/lib.rs",
                    "fn flush_metrics_after_start_writes_configured_logger_action",
                ),
            ],
        },
        "metrics.hotplug-reuse" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::HotplugReuse,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::HotplugGenerationFreshness],
            rationale: "Configured metric owners join and leave canonical snapshots with device hotplug, removed identities disappear, and later reuse of the same identifier starts a fresh owner generation without replaying the retired interval.",
            implementation: &[
                (
                    "crates/runtime/src/metrics.rs",
                    "pub struct SharedBlockDeviceMetricsRegistry",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "pub struct SharedPmemDeviceMetricsRegistry",
                ),
            ],
            validation: &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_hotplugs_flushes_and_reuses_runtime_pmem_from_exact_unused_grants",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "fn vhost_metrics_are_typed_scoped_and_fresh_across_same_id_reuse",
                ),
            ],
        },
        "metrics.initial-session" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::InitialSession,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::SessionInitialOnce],
            rationale: "A successfully started session makes one immediate best-effort metrics attempt, never writes before a session exists, and consumes the initial attempt even when its sink fails.",
            implementation: &[
                (
                    "crates/bangbang/src/vmm.rs",
                    "pub(crate) fn handle_initial_metrics_flush",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "pub(crate) fn flush_with_diagnostics_and_devices",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn initial_metrics_failure_preserves_session_and_consumes_initial_attempt",
                ),
                (
                    "crates/bangbang/src/vmm.rs",
                    "fn automatic_initial_and_terminal_metrics_are_session_gated_and_idempotent",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_metrics_schema_across_real_periodic_and_terminal_lifecycle",
                ),
            ],
        },
        "metrics.periodic-60s" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::PeriodicSixtySeconds,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::PeriodicSixtySeconds],
            rationale: "API and no-API owners derive the same 60-second deadline from the started session epoch, publish while Running or Paused, and schedule the next deadline after every best-effort attempt.",
            implementation: &[
                (
                    "crates/bangbang/src/api_server.rs",
                    "fn handle_due_periodic_schedulers",
                ),
                (
                    "crates/bangbang/src/main.rs",
                    "handle_due_no_api_periodic_schedulers",
                ),
                (
                    "crates/bangbang/src/periodic_metrics.rs",
                    "FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL",
                ),
                (
                    "crates/bangbang/src/vmm.rs",
                    "fn handle_periodic_metrics_flush",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn no_api_wait_periodic_metrics_timeout_flushes_after_start_without_sleeping",
                ),
                (
                    "crates/bangbang/src/periodic_metrics.rs",
                    "fn metrics_scheduler_uses_firecracker_interval_from_session_epoch",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_metrics_schema_across_real_periodic_and_terminal_lifecycle",
                ),
            ],
        },
        "metrics.process-isolation" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::ProcessIsolation,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::ProcessIsolation],
            rationale: "Each VMM controller owns an independent process snapshot, previous-success baseline, configured device registries, and output sink; contained grants preserve that isolation across concurrent launcher/worker sessions.",
            implementation: &[
                (
                    "crates/runtime/src/lib.rs",
                    "metrics_state: metrics::MetricsState",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "pub(crate) fn with_shared_process_metrics",
                ),
            ],
            validation: &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_keeps_concurrent_output_grant_sessions_isolated",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "fn independent_metrics_states_do_not_consume_each_others_deltas",
                ),
            ],
        },
        "metrics.publication-transaction" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::PublicationTransaction,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: METRICS_PUBLICATION_TRANSACTION_CLAIMS.to_vec(),
            rationale: "One immutable producer cut is committed only after complete JSON, newline, and flush success; a partial or ambiguously accepted failure retains the prior successful baseline, counts one lost output, replays at least once with post-cut events in the next generation, and does not reopen a consumed final attempt.",
            implementation: &[
                (
                    "crates/bangbang/src/vmm.rs",
                    "pub(crate) fn handle_terminal_metrics_flush",
                ),
                ("crates/runtime/src/metrics.rs", "fn write_metrics_line"),
                (
                    "crates/runtime/src/metrics.rs",
                    "pub(crate) fn flush_with_diagnostics_and_devices",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn terminal_metrics_sink_failure_preserves_result_and_consumes_final_attempt",
                ),
                (
                    "crates/runtime/src/metrics.rs",
                    "fn cross_producer_publication_transaction_replays_a_coherent_cut_after_partial_failure",
                ),
            ],
        },
        "metrics.snapshot-destination" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::SnapshotDestination,
            disposition: MetricsLifecycleDisposition::ProductBoundary,
            claims: vec![MetricsLifecycleClaim::SnapshotDestinationFreshness],
            rationale: "Metrics configuration, sink ownership, previous-success intervals, and live producer counters are not snapshot state; every restored process uses its independently configured destination and fresh product-local metric owners.",
            implementation: &[
                ("crates/bangbang/src/vmm.rs", "fn load_snapshot"),
                (
                    "crates/runtime/src/lib.rs",
                    "metrics_state: metrics::MetricsState",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/tests/executable_hvf_e2e.rs",
                    "fn configure_snapshot_destination_metrics",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci",
                ),
            ],
        },
        "metrics.terminal-final-attempt" => ScenarioSpec {
            boundary: MetricsLifecycleBoundary::TerminalFinalAttempt,
            disposition: MetricsLifecycleDisposition::Implemented,
            claims: vec![MetricsLifecycleClaim::TerminalBestEffortOnce],
            rationale: "Ordinary convergence settles shutdown and logger loss before one best-effort final metrics attempt; sink failure never replaces the process result and repeated convergence cannot write another final line.",
            implementation: &[
                (
                    "crates/bangbang/src/vmm.rs",
                    "pub(crate) fn handle_terminal_metrics_flush",
                ),
                (
                    "crates/bangbang/src/vmm.rs",
                    "pub(crate) fn handle_terminal_observability",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn terminal_logger_failure_is_counted_before_final_metrics_and_preserves_result",
                ),
                (
                    "crates/bangbang/src/main.rs",
                    "fn terminal_metrics_sink_failure_preserves_result_and_consumes_final_attempt",
                ),
                (
                    "crates/bangbang/src/vmm.rs",
                    "fn automatic_initial_and_terminal_metrics_are_session_gated_and_idempotent",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_metrics_schema_across_real_periodic_and_terminal_lifecycle",
                ),
            ],
        },
        _ => return None,
    })
}

fn local_references(entries: &[(&str, &str)]) -> Vec<Reference> {
    entries
        .iter()
        .map(|(path, anchor)| Reference::Local {
            path: (*path).to_string(),
            anchor: Some((*anchor).to_string()),
        })
        .collect()
}
