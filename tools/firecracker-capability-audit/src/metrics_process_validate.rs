use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    AuditMode, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    METRICS_PROCESS_PRODUCER_AUDIT_SCHEMA_VERSION, MetricsProcessProducerAudit,
    MetricsProcessProducerBoundary, MetricsProcessProducerDisposition, MetricsProducerOwner,
    MetricsSchemaAuthority, Reference, ValidationErrors,
};

const EXPECTED_PROCESS_FIELDS: usize = 69;
const COMPLETED_DELIVERY_ISSUES: &[&str] = &["#1827"];

/// Validate exact process-producer authority against the resolved metrics schema.
pub fn validate_metrics_process_producers(
    audit: &MetricsProcessProducerAudit,
    authority: &MetricsSchemaAuthority,
    repository_root: &Path,
    mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, authority, &mut errors);

    let process_profiles = authority
        .policy_profiles
        .iter()
        .filter(|profile| profile.producer_owner == MetricsProducerOwner::ProcessLifecycle)
        .map(|profile| profile.id.as_str())
        .collect::<BTreeSet<_>>();
    let source_paths = authority
        .source
        .fields
        .iter()
        .map(|field| (field.id.as_str(), field.path.as_str()))
        .collect::<BTreeMap<_, _>>();
    let expected = authority
        .field_policies
        .iter()
        .filter(|policy| process_profiles.contains(policy.profile_id.as_str()))
        .filter_map(|policy| {
            source_paths
                .get(policy.field_id.as_str())
                .map(|path| (policy.field_id.as_str(), *path))
        })
        .collect::<BTreeMap<_, _>>();

    if expected.len() != EXPECTED_PROCESS_FIELDS {
        errors.push(format!(
            "metrics process producer schema set must contain {EXPECTED_PROCESS_FIELDS} fields, found {}",
            expected.len()
        ));
    }
    if audit.records.len() != EXPECTED_PROCESS_FIELDS {
        errors.push(format!(
            "metrics process producer audit must contain {EXPECTED_PROCESS_FIELDS} records, found {}",
            audit.records.len()
        ));
    }

    let tracked = tracked_repository_files(repository_root, &mut errors);
    let mut records = BTreeMap::new();
    let mut previous = None;
    for record in &audit.records {
        if previous.is_some_and(|field_id| record.field_id.as_str() <= field_id) {
            errors.push(
                "metrics process producer records must be sorted and unique by field_id"
                    .to_string(),
            );
        }
        previous = Some(record.field_id.as_str());
        if records.insert(record.field_id.as_str(), record).is_some() {
            errors.push(format!(
                "duplicate metrics process producer record: {}",
                record.field_id
            ));
        }

        let Some(path) = expected.get(record.field_id.as_str()).copied() else {
            errors.push(format!(
                "stale or unowned metrics process producer record: {}",
                record.field_id
            ));
            continue;
        };
        validate_record(record, path, repository_root, &tracked, mode, &mut errors);
    }
    for field_id in expected.keys() {
        if !records.contains_key(field_id) {
            errors.push(format!(
                "missing metrics process producer record: {field_id}"
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(
    audit: &MetricsProcessProducerAudit,
    authority: &MetricsSchemaAuthority,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != METRICS_PROCESS_PRODUCER_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "metrics process producer schema_version must be {METRICS_PROCESS_PRODUCER_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline != authority.baseline {
        errors.push("metrics process producer and metrics schema baselines differ".to_string());
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("metrics process producer baseline is not the pinned release".to_string());
    }
}

fn validate_record(
    record: &crate::MetricsProcessProducerRecord,
    path: &str,
    repository_root: &Path,
    tracked: &BTreeSet<std::path::PathBuf>,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    let Some(delivery_issue) = expected_delivery_issue(path) else {
        errors.push(format!(
            "metrics process producer field has no closed delivery owner: {}",
            record.field_id
        ));
        return;
    };
    if record.delivery_issue != delivery_issue {
        errors.push(format!(
            "metrics process producer has the wrong delivery issue: {}",
            record.field_id
        ));
    }

    let Some(boundary) = expected_boundary(path) else {
        errors.push(format!(
            "metrics process producer field has no closed boundary: {}",
            record.field_id
        ));
        return;
    };
    if record.boundary != boundary {
        errors.push(format!(
            "metrics process producer has the wrong boundary: {}",
            record.field_id
        ));
    }
    let Some(rationale) = expected_rationale(path, delivery_issue, boundary) else {
        errors.push(format!(
            "metrics process producer field has no closed rationale: {}",
            record.field_id
        ));
        return;
    };
    if record.rationale != rationale {
        errors.push(format!(
            "metrics process producer has a stale rationale: {}",
            record.field_id
        ));
    }

    let completed = COMPLETED_DELIVERY_ISSUES.contains(&delivery_issue);
    if completed && !record.disposition.is_terminal() {
        errors.push(format!(
            "completed metrics process producer slice must be terminal: {}",
            record.field_id
        ));
    }
    if !completed && record.disposition != MetricsProcessProducerDisposition::Planned {
        errors.push(format!(
            "uncompleted metrics process producer slice must remain planned: {}",
            record.field_id
        ));
    }
    if delivery_issue == "#1827"
        && record.disposition != MetricsProcessProducerDisposition::Implemented
    {
        errors.push(format!(
            "API metrics producer must be implemented, not neutral or zero: {}",
            record.field_id
        ));
    }

    match record.disposition {
        MetricsProcessProducerDisposition::Planned => {
            if !record.implementation.is_empty() || !record.validation.is_empty() {
                errors.push(format!(
                    "planned metrics process producer must not claim terminal evidence: {}",
                    record.field_id
                ));
            }
            if mode == AuditMode::Final {
                errors.push(format!(
                    "final metrics process producer validation rejects planned record: {}",
                    record.field_id
                ));
            }
        }
        MetricsProcessProducerDisposition::Implemented
        | MetricsProcessProducerDisposition::SourceNeutral
        | MetricsProcessProducerDisposition::PlatformZero => {
            if record.implementation.is_empty() || record.validation.is_empty() {
                errors.push(format!(
                    "terminal metrics process producer needs implementation and validation evidence: {}",
                    record.field_id
                ));
            }
            validate_references(
                &record.implementation,
                "implementation",
                &record.field_id,
                repository_root,
                tracked,
                errors,
            );
            validate_references(
                &record.validation,
                "validation",
                &record.field_id,
                repository_root,
                tracked,
                errors,
            );
        }
    }
}

fn validate_references(
    references: &[Reference],
    kind: &str,
    field_id: &str,
    repository_root: &Path,
    tracked: &BTreeSet<std::path::PathBuf>,
    errors: &mut Vec<String>,
) {
    if references.windows(2).any(|pair| {
        let [previous, current] = pair else {
            return false;
        };
        previous >= current
    }) {
        errors.push(format!(
            "metrics process producer {kind} references must be sorted and unique: {field_id}"
        ));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("metrics process producer {field_id} {kind}[{index}]"),
            errors,
        );
    }
}

fn expected_delivery_issue(path: &str) -> Option<&'static str> {
    if path == "deprecated_api.deprecated_http_api_calls"
        || path.starts_with("get_api_requests.")
        || path.starts_with("patch_api_requests.")
        || path.starts_with("put_api_requests.")
    {
        Some("#1827")
    } else if path.starts_with("api_server.process_startup_time_")
        || path.starts_with("latencies_us.")
    {
        Some("#1828")
    } else if path.starts_with("logger.") || path == "signals.sigpipe" {
        Some("#1829")
    } else if path == "seccomp.num_faults"
        || (path.starts_with("signals.") && path != "signals.sigpipe")
        || path == "vmm.panic_count"
    {
        Some("#1830")
    } else {
        None
    }
}

fn expected_boundary(path: &str) -> Option<MetricsProcessProducerBoundary> {
    if path == "deprecated_api.deprecated_http_api_calls" {
        Some(MetricsProcessProducerBoundary::AcceptedDeprecatedApiValue)
    } else if path.starts_with("get_api_requests.")
        || ((path.starts_with("patch_api_requests.") || path.starts_with("put_api_requests."))
            && path.ends_with("_count"))
    {
        Some(MetricsProcessProducerBoundary::RequestParserEntry)
    } else if (path.starts_with("patch_api_requests.") || path.starts_with("put_api_requests."))
        && path.ends_with("_fails")
    {
        Some(MetricsProcessProducerBoundary::RequestParserFailure)
    } else if path.starts_with("api_server.process_startup_time_") {
        Some(MetricsProcessProducerBoundary::ProcessStartup)
    } else if path.starts_with("latencies_us.vmm_") {
        Some(MetricsProcessProducerBoundary::SuccessfulInnerVmmOperation)
    } else if path.starts_with("latencies_us.") {
        Some(MetricsProcessProducerBoundary::SuccessfulOuterApiOperation)
    } else if path.starts_with("logger.") {
        Some(MetricsProcessProducerBoundary::LoggerLifecycle)
    } else if path.starts_with("signals.") {
        Some(MetricsProcessProducerBoundary::SignalLifecycle)
    } else if path == "vmm.panic_count" {
        Some(MetricsProcessProducerBoundary::PanicLifecycle)
    } else if path == "seccomp.num_faults" {
        Some(MetricsProcessProducerBoundary::SeccompFault)
    } else {
        None
    }
}

fn expected_rationale(
    path: &str,
    delivery_issue: &str,
    boundary: MetricsProcessProducerBoundary,
) -> Option<&'static str> {
    if path == "patch_api_requests.network_count" {
        return Some(
            "Pinned Firecracker increments this count at parser entry and again for a missing or mismatched interface ID; Bangbang applies the bounded typed effect once.",
        );
    }
    if path == "patch_api_requests.network_fails" {
        return Some(
            "Pinned Firecracker increments this failure only for typed PATCH body conversion or validation, not for missing or mismatched interface IDs or later action errors.",
        );
    }
    Some(match (delivery_issue, boundary) {
        ("#1827", MetricsProcessProducerBoundary::RequestParserEntry) => {
            "Pinned Firecracker increments this request count at endpoint parser entry; Bangbang applies the value-free typed parser effect once before dispatch."
        }
        ("#1827", MetricsProcessProducerBoundary::RequestParserFailure) => {
            "Pinned Firecracker increments this failure only on endpoint conversion or validation failure; later VMM action errors do not change it."
        }
        ("#1827", MetricsProcessProducerBoundary::AcceptedDeprecatedApiValue) => {
            "Pinned Firecracker increments this counter only after typed parsing accepts a deprecated API value; Bangbang records the redacted parse effect once."
        }
        ("#1828", MetricsProcessProducerBoundary::ProcessStartup) => {
            "Delivery child #1828 owns the exact process startup clock boundary and remains unresolved in this audit slice."
        }
        ("#1828", MetricsProcessProducerBoundary::SuccessfulOuterApiOperation) => {
            "Delivery child #1828 owns the successful outer API operation latency boundary and remains unresolved in this audit slice."
        }
        ("#1828", MetricsProcessProducerBoundary::SuccessfulInnerVmmOperation) => {
            "Delivery child #1828 owns the successful inner VMM operation latency boundary and remains unresolved in this audit slice."
        }
        ("#1829", MetricsProcessProducerBoundary::LoggerLifecycle) => {
            "Delivery child #1829 owns generation-consistent logger lifecycle capture and remains unresolved in this audit slice."
        }
        ("#1829", MetricsProcessProducerBoundary::SignalLifecycle) => {
            "Delivery child #1829 owns the generation-consistent SIGPIPE capture boundary and remains unresolved in this audit slice."
        }
        ("#1830", MetricsProcessProducerBoundary::SignalLifecycle) => {
            "Delivery child #1830 owns fatal process-signal classification and remains unresolved in this audit slice."
        }
        ("#1830", MetricsProcessProducerBoundary::PanicLifecycle) => {
            "Delivery child #1830 owns panic capture and fatal convergence and remains unresolved in this audit slice."
        }
        ("#1830", MetricsProcessProducerBoundary::SeccompFault) => {
            "Delivery child #1830 owns the platform seccomp-fault disposition and evidence and remains unresolved in this audit slice."
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_exact_known_process_paths() {
        assert_eq!(
            expected_delivery_issue("put_api_requests.actions_count"),
            Some("#1827")
        );
        assert_eq!(
            expected_delivery_issue("latencies_us.vmm_pause_vm"),
            Some("#1828")
        );
        assert_eq!(expected_delivery_issue("signals.sigpipe"), Some("#1829"));
        assert_eq!(expected_delivery_issue("signals.sigsegv"), Some("#1830"));
        assert_eq!(expected_delivery_issue("block.read_count"), None);
        assert_eq!(
            expected_rationale(
                "future.field",
                "#9999",
                MetricsProcessProducerBoundary::PanicLifecycle,
            ),
            None
        );
    }
}
