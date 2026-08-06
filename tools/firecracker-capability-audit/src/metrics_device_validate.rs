use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::validate::{tracked_repository_files, validate_exclusion, validate_reference};
use crate::{
    AuditMode, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    METRICS_DEVICE_PRODUCER_AUDIT_SCHEMA_VERSION, MetricsDeviceProducerAudit,
    MetricsDeviceProducerBoundary, MetricsDeviceProducerDisposition, MetricsDeviceProducerRecord,
    MetricsPolicyProfile, MetricsProducerDisposition, MetricsProducerOwner, MetricsSchemaAuthority,
    MetricsSourceField, PlatformExclusion, Reference, ValidationErrors,
};

const EXPECTED_DEVICE_FIELDS: usize = 231;
const COMPLETED_DELIVERY_ISSUES: &[&str] = &[
    "#1838", "#1839", "#1840", "#1841", "#1842", "#1843", "#1844", "#1845", "#1846",
];

const ARCHITECTURE_RETAINED_CHALLENGE_URL: &str =
    "https://github.com/seven332/bangbang/issues/1846#issuecomment-5205139984";
const APPLE_HVF_EXIT_REASON_URL: &str =
    "https://developer.apple.com/documentation/hypervisor/hv_exit_reason_t";
const APPLE_HVF_VCPU_EXIT_URL: &str =
    "https://developer.apple.com/documentation/hypervisor/hv_vcpu_exit_t";
const FIRECRACKER_SOURCE_URL: &str = "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src";
const I8042_LITERAL_ZERO_ANCHOR: &str = "i8042: I8042Metrics {\n            error_count: 0,\n            missed_read_count: 0,\n            missed_write_count: 0,\n            read_count: 0,\n            reset_count: 0,\n            write_count: 0,\n        },";
const EXIT_IO_IN_AGG_LITERAL_ZERO_ANCHOR: &str = "exit_io_in_agg: LatencyAggregate {\n                min_us: 0,\n                max_us: 0,\n                sum_us: 0,\n            },";
const EXIT_IO_OUT_AGG_LITERAL_ZERO_ANCHOR: &str = "exit_io_out_agg: LatencyAggregate {\n                min_us: 0,\n                max_us: 0,\n                sum_us: 0,\n            },";

/// Validate exact device-producer authority against the resolved metrics schema.
pub fn validate_metrics_device_producers(
    audit: &MetricsDeviceProducerAudit,
    authority: &MetricsSchemaAuthority,
    repository_root: &Path,
    mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, authority, &mut errors);

    let profiles = authority
        .policy_profiles
        .iter()
        .map(|profile| (profile.id.as_str(), profile))
        .collect::<BTreeMap<_, _>>();
    let source_fields = authority
        .source
        .fields
        .iter()
        .map(|field| (field.id.as_str(), field))
        .collect::<BTreeMap<_, _>>();
    let expected = authority
        .field_policies
        .iter()
        .filter_map(|policy| {
            let profile = profiles.get(policy.profile_id.as_str()).copied()?;
            if profile.producer_owner != MetricsProducerOwner::Device {
                return None;
            }
            source_fields
                .get(policy.field_id.as_str())
                .copied()
                .map(|field| (policy.field_id.as_str(), (field, profile)))
        })
        .collect::<BTreeMap<_, _>>();

    if expected.len() != EXPECTED_DEVICE_FIELDS {
        errors.push(format!(
            "metrics device producer schema set must contain {EXPECTED_DEVICE_FIELDS} fields, found {}",
            expected.len()
        ));
    }
    if audit.records.len() != EXPECTED_DEVICE_FIELDS {
        errors.push(format!(
            "metrics device producer audit must contain {EXPECTED_DEVICE_FIELDS} records, found {}",
            audit.records.len()
        ));
    }

    let tracked = tracked_repository_files(repository_root, &mut errors);
    let context = RecordValidationContext {
        repository_root,
        tracked: &tracked,
        mode,
    };
    let mut records = BTreeMap::new();
    let mut previous = None;
    for record in &audit.records {
        if previous.is_some_and(|field_id| record.field_id.as_str() <= field_id) {
            errors.push(
                "metrics device producer records must be sorted and unique by field_id".to_string(),
            );
        }
        previous = Some(record.field_id.as_str());
        if records.insert(record.field_id.as_str(), record).is_some() {
            errors.push(format!(
                "duplicate metrics device producer record: {}",
                record.field_id
            ));
        }

        let Some((field, profile)) = expected.get(record.field_id.as_str()).copied() else {
            errors.push(format!(
                "stale or unowned metrics device producer record: {}",
                record.field_id
            ));
            continue;
        };
        validate_record(record, field, profile, &context, &mut errors);
    }
    for field_id in expected.keys() {
        if !records.contains_key(field_id) {
            errors.push(format!(
                "missing metrics device producer record: {field_id}"
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
    audit: &MetricsDeviceProducerAudit,
    authority: &MetricsSchemaAuthority,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != METRICS_DEVICE_PRODUCER_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "metrics device producer schema_version must be {METRICS_DEVICE_PRODUCER_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline != authority.baseline {
        errors.push("metrics device producer and metrics schema baselines differ".to_string());
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("metrics device producer baseline is not the pinned release".to_string());
    }
}

struct RecordValidationContext<'a> {
    repository_root: &'a Path,
    tracked: &'a BTreeSet<PathBuf>,
    mode: AuditMode,
}

fn validate_record(
    record: &MetricsDeviceProducerRecord,
    field: &MetricsSourceField,
    profile: &MetricsPolicyProfile,
    context: &RecordValidationContext<'_>,
    errors: &mut Vec<String>,
) {
    let path = field.path.as_str();
    let Some(delivery_issue) = expected_delivery_issue(path) else {
        errors.push(format!(
            "metrics device producer field has no closed delivery owner: {}",
            record.field_id
        ));
        return;
    };
    if record.delivery_issue != delivery_issue {
        errors.push(format!(
            "metrics device producer has the wrong delivery issue: {}",
            record.field_id
        ));
    }

    let Some(boundary) = expected_boundary(path) else {
        errors.push(format!(
            "metrics device producer field has no closed boundary: {}",
            record.field_id
        ));
        return;
    };
    if record.boundary != boundary {
        errors.push(format!(
            "metrics device producer has the wrong boundary: {}",
            record.field_id
        ));
    }

    let completed = COMPLETED_DELIVERY_ISSUES.contains(&delivery_issue);
    let rationale = expected_rationale(path, delivery_issue, boundary, completed);
    if record.rationale != rationale {
        errors.push(format!(
            "metrics device producer has a stale rationale: {}",
            record.field_id
        ));
    }

    let provisional = is_provisional_platform_zero(path);
    let profile_matches_platform_transition = if provisional {
        matches!(
            profile.producer_disposition,
            MetricsProducerDisposition::PlatformZero | MetricsProducerDisposition::Implemented
        )
    } else {
        profile.producer_disposition != MetricsProducerDisposition::PlatformZero
    };
    if !profile_matches_platform_transition {
        errors.push(format!(
            "metrics device producer disagrees with the schema platform candidate: {}",
            record.field_id
        ));
    }

    let expected_disposition = if completed {
        expected_terminal_disposition(path)
    } else if provisional {
        Some(MetricsDeviceProducerDisposition::ProvisionalPlatformZero)
    } else if profile.producer_disposition == MetricsProducerDisposition::Planned {
        Some(MetricsDeviceProducerDisposition::Planned)
    } else {
        None
    };
    let Some(expected_disposition) = expected_disposition else {
        errors.push(format!(
            "metrics device producer field has no closed current disposition: {}",
            record.field_id
        ));
        return;
    };
    if record.disposition != expected_disposition {
        errors.push(format!(
            "metrics device producer has the wrong current disposition: {}",
            record.field_id
        ));
    }

    match record.disposition {
        MetricsDeviceProducerDisposition::Planned
        | MetricsDeviceProducerDisposition::ProvisionalPlatformZero => {
            if !record.implementation.is_empty()
                || !record.validation.is_empty()
                || record.platform_exclusion.is_some()
            {
                errors.push(format!(
                    "nonterminal metrics device producer must not claim terminal evidence: {}",
                    record.field_id
                ));
            }
            if context.mode == AuditMode::Final {
                errors.push(format!(
                    "final metrics device producer validation rejects nonterminal record: {}",
                    record.field_id
                ));
            }
        }
        MetricsDeviceProducerDisposition::Implemented
        | MetricsDeviceProducerDisposition::SourceNeutral
        | MetricsDeviceProducerDisposition::PlatformZero => {
            if record.implementation.is_empty() || record.validation.is_empty() {
                errors.push(format!(
                    "terminal metrics device producer needs implementation and validation evidence: {}",
                    record.field_id
                ));
            }
            validate_references(
                &record.implementation,
                "implementation",
                &record.field_id,
                context.repository_root,
                context.tracked,
                errors,
            );
            validate_references(
                &record.validation,
                "validation",
                &record.field_id,
                context.repository_root,
                context.tracked,
                errors,
            );

            match (record.disposition, &record.platform_exclusion) {
                (MetricsDeviceProducerDisposition::PlatformZero, Some(exclusion)) => {
                    validate_exclusion(
                        &record.field_id,
                        exclusion,
                        context.repository_root,
                        context.tracked,
                        errors,
                    );
                    validate_exclusion_anchors(
                        exclusion,
                        &record.field_id,
                        context.repository_root,
                        errors,
                    );
                    if record.delivery_issue == "#1846" {
                        validate_architecture_retained_platform_zero_record(
                            record, path, exclusion, errors,
                        );
                    }
                }
                (MetricsDeviceProducerDisposition::PlatformZero, None) => errors.push(format!(
                    "terminal platform-zero metrics device producer needs structured exclusion evidence: {}",
                    record.field_id
                )),
                (_, Some(_)) => errors.push(format!(
                    "non-platform metrics device producer forbids exclusion evidence: {}",
                    record.field_id
                )),
                (_, None) => {}
            }
        }
    }
}

fn validate_architecture_retained_platform_zero_record(
    record: &MetricsDeviceProducerRecord,
    path: &str,
    exclusion: &PlatformExclusion,
    errors: &mut Vec<String>,
) {
    let Some(expected_exclusion) = expected_architecture_retained_exclusion(path) else {
        errors.push(format!(
            "#1846 terminal platform-zero record is outside the exact architecture-retained set: {}",
            record.field_id
        ));
        return;
    };
    let expected_implementation = architecture_retained_implementation(path);
    if record.implementation != expected_implementation {
        errors.push(format!(
            "#1846 terminal platform-zero record has wrong exact target/backend/machine or literal-zero implementation evidence: {}",
            record.field_id
        ));
    }
    let expected_validation = architecture_retained_validation();
    if record.validation != expected_validation {
        errors.push(format!(
            "#1846 terminal platform-zero record has wrong exact portable/signed validation evidence: {}",
            record.field_id
        ));
    }
    if exclusion != &expected_exclusion {
        errors.push(format!(
            "#1846 terminal platform-zero record has wrong exact field, target, backend, machine, or Challenge exclusion evidence: {}",
            record.field_id
        ));
    }
}

fn expected_architecture_retained_exclusion(path: &str) -> Option<PlatformExclusion> {
    let upstream_contract = architecture_retained_upstream_contract(path)?;
    let alternatives = vec![
        format!(
            "Adding x86 PC port I/O or KVM to the supported arm64 HVF machine would change the target/backend/machine contract for `{path}` rather than implement its current-platform producer."
        ),
        architecture_retained_non_aliasing_alternative(path)?.to_string(),
    ];
    Some(PlatformExclusion {
        upstream_contract,
        platform_evidence: architecture_retained_platform_evidence(),
        alternatives,
        stable_behavior: architecture_retained_implementation(path),
        focused_tests: architecture_retained_validation(),
        compatibility_docs: vec![
            local_reference(
                "compat/firecracker/v1.16.0/metrics-contract.md",
                "### arm64-retained i8042, PIO, and KVM-clock platform zeros",
            ),
            local_reference(
                "docs/firecracker-compatibility.md",
                "### Architecture-retained arm64 metric zeros",
            ),
        ],
        security_docs: vec![local_reference(
            "docs/security.md",
            "### Architecture-retained metric identity and non-aliasing",
        )],
        challenge: Reference::Github {
            url: ARCHITECTURE_RETAINED_CHALLENGE_URL.to_string(),
        },
    })
}

fn architecture_retained_upstream_contract(path: &str) -> Option<Vec<Reference>> {
    let producer = match path {
        "i8042.error_count" | "i8042.reset_count" => "/devices/legacy/i8042.rs#L248-L266",
        "i8042.missed_read_count" | "i8042.read_count" => "/devices/legacy/i8042.rs#L214-L244",
        "i8042.missed_write_count" | "i8042.write_count" => "/devices/legacy/i8042.rs#L248-L336",
        "vcpu.exit_io_in"
        | "vcpu.exit_io_in_agg.max_us"
        | "vcpu.exit_io_in_agg.min_us"
        | "vcpu.exit_io_in_agg.sum_us" => "/arch/x86_64/vcpu.rs#L717-L731",
        "vcpu.exit_io_out"
        | "vcpu.exit_io_out_agg.max_us"
        | "vcpu.exit_io_out_agg.min_us"
        | "vcpu.exit_io_out_agg.sum_us" => "/arch/x86_64/vcpu.rs#L717-L742",
        "vcpu.kvmclock_ctrl_fails" => "/arch/x86_64/vcpu.rs#L268-L283",
        _ => return None,
    };
    let mut references = Vec::new();
    if path.starts_with("i8042.") {
        references.push(authoritative_reference(&format!(
            "{FIRECRACKER_SOURCE_URL}/device_manager/legacy.rs#L7-L66"
        )));
    }
    references.push(authoritative_reference(&format!(
        "{FIRECRACKER_SOURCE_URL}{producer}"
    )));
    Some(references)
}

fn architecture_retained_non_aliasing_alternative(path: &str) -> Option<String> {
    if path.starts_with("i8042.") {
        return Some(format!(
            "Relabeling PL031 RTC or PSCI activity as `{path}` would fabricate an i8042 event."
        ));
    }
    if path.starts_with("vcpu.exit_io_") {
        return Some(format!(
            "Relabeling HVF MMIO activity as `{path}` would fabricate a KVM PIO event and duplicate the existing MMIO family."
        ));
    }
    (path == "vcpu.kvmclock_ctrl_fails").then(|| {
        format!(
            "Relabeling HVF pause/resume or PVTime activity as `{path}` would fabricate a KVM_KVMCLOCK_CTRL failure."
        )
    })
}

fn architecture_retained_implementation(path: &str) -> Vec<Reference> {
    vec![
        local_reference(
            "crates/hvf/src/backend.rs",
            "pub fn is_supported_target() -> bool",
        ),
        local_reference("crates/hvf/src/exit.rs", "pub enum HvfVcpuExit {"),
        local_reference(
            "crates/hvf/src/startup.rs",
            "rtc_device: Some(RuntimeArm64BootRtcDeviceConfig::new(config.rtc_mmio_layout)),",
        ),
        local_reference(
            "crates/runtime/src/metrics/firecracker.rs",
            architecture_retained_literal_anchor(path),
        ),
    ]
}

fn architecture_retained_literal_anchor(path: &str) -> &'static str {
    if path.starts_with("i8042.") {
        I8042_LITERAL_ZERO_ANCHOR
    } else if path == "vcpu.exit_io_in" {
        "exit_io_in: 0,"
    } else if path.starts_with("vcpu.exit_io_in_agg.") {
        EXIT_IO_IN_AGG_LITERAL_ZERO_ANCHOR
    } else if path == "vcpu.exit_io_out" {
        "exit_io_out: 0,"
    } else if path.starts_with("vcpu.exit_io_out_agg.") {
        EXIT_IO_OUT_AGG_LITERAL_ZERO_ANCHOR
    } else {
        "kvmclock_ctrl_fails: 0,"
    }
}

fn architecture_retained_platform_evidence() -> Vec<Reference> {
    vec![
        local_reference(
            "crates/hvf/src/backend.rs",
            "cfg!(all(target_os = \"macos\", target_arch = \"aarch64\"))",
        ),
        local_reference("crates/hvf/src/exit.rs", "pub enum HvfVcpuExit {"),
        local_reference(
            "crates/hvf/src/startup.rs",
            "rtc_device: Some(RuntimeArm64BootRtcDeviceConfig::new(config.rtc_mmio_layout)),",
        ),
        authoritative_reference(APPLE_HVF_EXIT_REASON_URL),
        authoritative_reference(APPLE_HVF_VCPU_EXIT_URL),
    ]
}

fn architecture_retained_validation() -> Vec<Reference> {
    vec![
        local_reference(
            "crates/bangbang/tests/executable_hvf_e2e.rs",
            "fn assert_architecture_retained_platform_zero_metrics(",
        ),
        local_reference(
            "crates/launcher/tests/production_bundle_e2e.rs",
            "fn assert_architecture_retained_platform_zero_metrics(",
        ),
        local_reference(
            "crates/runtime/src/metrics.rs",
            "fn assert_architecture_retained_platform_zero_metrics(",
        ),
        local_reference(
            "tools/firecracker-capability-audit/tests/metrics_schema.rs",
            "fn device_producer_audit_rejects_architecture_retained_platform_scope_drift()",
        ),
    ]
}

fn local_reference(path: &str, anchor: &str) -> Reference {
    Reference::Local {
        path: path.to_string(),
        anchor: Some(anchor.to_string()),
    }
}

fn authoritative_reference(url: &str) -> Reference {
    Reference::Authoritative {
        url: url.to_string(),
    }
}

fn validate_references(
    references: &[Reference],
    kind: &str,
    field_id: &str,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    if references.windows(2).any(|pair| {
        let [previous, current] = pair else {
            return false;
        };
        previous >= current
    }) {
        errors.push(format!(
            "metrics device producer {kind} references must be sorted and unique: {field_id}"
        ));
    }
    if !references
        .iter()
        .any(|reference| matches!(reference, Reference::Local { .. }))
    {
        errors.push(format!(
            "metrics device producer {kind} needs anchored local evidence: {field_id}"
        ));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("metrics device producer {field_id} {kind}[{index}]"),
            errors,
        );
        validate_local_anchor(
            reference,
            repository_root,
            &format!("metrics device producer {field_id} {kind}[{index}]"),
            errors,
        );
    }
}

fn validate_exclusion_anchors(
    exclusion: &crate::PlatformExclusion,
    field_id: &str,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    for (group, references) in [
        ("upstream_contract", &exclusion.upstream_contract),
        ("platform_evidence", &exclusion.platform_evidence),
        ("stable_behavior", &exclusion.stable_behavior),
        ("focused_tests", &exclusion.focused_tests),
        ("compatibility_docs", &exclusion.compatibility_docs),
        ("security_docs", &exclusion.security_docs),
    ] {
        for (index, reference) in references.iter().enumerate() {
            validate_local_anchor(
                reference,
                repository_root,
                &format!("metrics device producer {field_id} exclusion.{group}[{index}]"),
                errors,
            );
        }
    }
}

fn validate_local_anchor(
    reference: &Reference,
    repository_root: &Path,
    label: &str,
    errors: &mut Vec<String>,
) {
    let Reference::Local { path, anchor } = reference else {
        return;
    };
    let Some(anchor) = anchor.as_deref().filter(|anchor| !anchor.trim().is_empty()) else {
        errors.push(format!("local reference needs a stable anchor: {label}"));
        return;
    };
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return;
    }
    let joined = repository_root.join(relative);
    let Ok(metadata) = std::fs::symlink_metadata(&joined) else {
        return;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return;
    }
    let (Ok(canonical_root), Ok(canonical)) =
        (repository_root.canonicalize(), joined.canonicalize())
    else {
        return;
    };
    if !canonical.starts_with(canonical_root) {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(canonical) else {
        errors.push(format!(
            "local reference anchor file must be UTF-8: {label}"
        ));
        return;
    };
    if !contents.contains(anchor) {
        errors.push(format!("local reference anchor does not resolve: {label}"));
    }
}

fn expected_delivery_issue(path: &str) -> Option<&'static str> {
    let (root, suffix) = path.split_once('.').unwrap_or((path, ""));
    Some(match root {
        "entropy" | "pmem" | "rtc" | "uart" => "#1838",
        "balloon" | "memory_hotplug" => "#1839",
        "vsock" => "#1840",
        "block" | "block_{drive_id}" => "#1841",
        "vhost_user_block_{drive_id}" => "#1842",
        "mmds" => "#1843",
        "net" | "net_{iface_id}" if is_tap_gap(suffix) => "#1844",
        "net" | "net_{iface_id}" => "#1843",
        "interrupts" => "#1845",
        "vcpu" if architecture_retained_vcpu_suffix(suffix) => "#1846",
        "vcpu" => "#1845",
        "i8042" => "#1846",
        _ => return None,
    })
}

fn expected_boundary(path: &str) -> Option<MetricsDeviceProducerBoundary> {
    use MetricsDeviceProducerBoundary as Boundary;

    let (root, suffix) = path.split_once('.')?;
    match root {
        "i8042" => return architecture_suffix(suffix).then_some(Boundary::ArchitectureRetained),
        "interrupts" => {
            return matches!(suffix, "config_updates" | "triggers")
                .then_some(Boundary::InterruptLifecycle);
        }
        "mmds" => return mmds_suffix(suffix).then_some(Boundary::MmdsDataPath),
        "vcpu" if architecture_retained_vcpu_suffix(suffix) => {
            return Some(Boundary::ArchitectureRetained);
        }
        "vcpu" if suffix == "failures" => return Some(Boundary::VcpuFailure),
        "vcpu" => return vcpu_mmio_suffix(suffix).then_some(Boundary::VcpuExit),
        _ => {}
    }

    if matches!(
        suffix,
        "activate_fails" | "activate_time_us" | "init_time_us"
    ) {
        return Some(Boundary::Activation);
    }
    if matches!(
        suffix,
        "cfg_fails"
            | "config_change_time_us"
            | "mac_address_updates"
            | "update_count"
            | "update_fails"
    ) {
        return Some(Boundary::Configuration);
    }
    if latency_suffix(suffix) {
        return Some(Boundary::Latency);
    }
    if rate_limiter_suffix(suffix) {
        return Some(Boundary::RateLimiter);
    }
    if queue_event_suffix(suffix) {
        return Some(Boundary::QueueEvent);
    }
    if root == "balloon" && balloon_state_suffix(suffix) {
        return Some(Boundary::DeviceState);
    }
    if root == "memory_hotplug" && memory_hotplug_state_suffix(suffix) {
        return Some(Boundary::DeviceState);
    }
    data_path_suffix(suffix).then_some(Boundary::DataPath)
}

fn expected_terminal_disposition(path: &str) -> Option<MetricsDeviceProducerDisposition> {
    if matches!(path, "uart.flush_count" | "mmds.rx_bad_eth") {
        return Some(MetricsDeviceProducerDisposition::SourceNeutral);
    }
    if matches!(
        path,
        "net.mac_address_updates" | "net_{iface_id}.mac_address_updates"
    ) || is_architecture_retained_platform_zero(path)
    {
        return Some(MetricsDeviceProducerDisposition::PlatformZero);
    }
    expected_delivery_issue(path)
        .is_some_and(|issue| COMPLETED_DELIVERY_ISSUES.contains(&issue))
        .then_some(MetricsDeviceProducerDisposition::Implemented)
}

fn expected_rationale(
    path: &str,
    delivery_issue: &str,
    boundary: MetricsDeviceProducerBoundary,
    completed: bool,
) -> String {
    if completed && delivery_issue == "#1846" {
        return format!(
            "Pinned Firecracker field `{path}` has no identity-preserving producer on Bangbang's aarch64-apple-darwin Hypervisor.framework MMIO/PL031/PSCI machine; #1846 certifies its required public value as a terminal literal zero at the `{}` boundary.",
            boundary.as_str()
        );
    }
    if completed {
        format!(
            "Pinned Firecracker field `{path}` is completed by {delivery_issue} at the `{}` boundary with exact implementation and validation evidence.",
            boundary.as_str()
        )
    } else {
        format!(
            "Pinned Firecracker field `{path}` is assigned to {delivery_issue} at the `{}` boundary; terminal implementation and validation evidence is intentionally deferred until that child is completed.",
            boundary.as_str()
        )
    }
}

fn is_provisional_platform_zero(path: &str) -> bool {
    let (root, suffix) = path.split_once('.').unwrap_or((path, ""));
    (matches!(root, "net" | "net_{iface_id}") && suffix == "mac_address_updates")
        || root == "i8042"
        || (root == "vcpu" && architecture_retained_vcpu_suffix(suffix))
}

fn is_architecture_retained_platform_zero(path: &str) -> bool {
    let (root, suffix) = path.split_once('.').unwrap_or((path, ""));
    (root == "i8042" && architecture_suffix(suffix))
        || (root == "vcpu" && architecture_retained_vcpu_suffix(suffix))
}

fn is_tap_gap(suffix: &str) -> bool {
    matches!(
        suffix,
        "mac_address_updates"
            | "rx_tap_event_count"
            | "tap_read_fails"
            | "tap_write_agg.max_us"
            | "tap_write_agg.min_us"
            | "tap_write_agg.sum_us"
            | "tap_write_fails"
    )
}

fn architecture_retained_vcpu_suffix(suffix: &str) -> bool {
    suffix == "kvmclock_ctrl_fails"
        || suffix == "exit_io_in"
        || suffix.starts_with("exit_io_in_agg.")
        || suffix == "exit_io_out"
        || suffix.starts_with("exit_io_out_agg.")
}

fn architecture_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "error_count"
            | "missed_read_count"
            | "missed_write_count"
            | "read_count"
            | "reset_count"
            | "write_count"
    )
}

fn mmds_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "connections_created"
            | "connections_destroyed"
            | "rx_accepted"
            | "rx_accepted_err"
            | "rx_accepted_unusual"
            | "rx_bad_eth"
            | "rx_count"
            | "rx_invalid_token"
            | "rx_no_token"
            | "tx_bytes"
            | "tx_count"
            | "tx_errors"
            | "tx_frames"
    )
}

fn vcpu_mmio_suffix(suffix: &str) -> bool {
    suffix == "exit_mmio_read"
        || suffix.starts_with("exit_mmio_read_agg.")
        || suffix == "exit_mmio_write"
        || suffix.starts_with("exit_mmio_write_agg.")
}

fn latency_suffix(suffix: &str) -> bool {
    suffix.ends_with("_agg.max_us")
        || suffix.ends_with("_agg.min_us")
        || suffix.ends_with("_agg.sum_us")
}

fn rate_limiter_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "entropy_rate_limiter_throttled"
            | "io_engine_throttled_events"
            | "rate_limiter_dropped_bytes"
            | "rate_limiter_event_count"
            | "rate_limiter_throttled_events"
            | "rx_event_rate_limiter_count"
            | "rx_rate_limiter_throttled"
            | "tx_rate_limiter_event_count"
            | "tx_rate_limiter_throttled"
    )
}

fn queue_event_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "conn_event_fails"
            | "entropy_event_count"
            | "entropy_event_fails"
            | "ev_queue_event_fails"
            | "event_fails"
            | "killq_resync"
            | "muxer_event_fails"
            | "no_avail_buffer"
            | "no_rx_avail_buffer"
            | "no_tx_avail_buffer"
            | "queue_event_count"
            | "queue_event_fails"
            | "remaining_reqs_count"
            | "rx_queue_event_count"
            | "rx_queue_event_fails"
            | "rx_tap_event_count"
            | "tx_queue_event_count"
            | "tx_queue_event_fails"
            | "tx_remaining_reqs_count"
    )
}

fn balloon_state_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "deflate_count"
            | "free_page_hint_count"
            | "free_page_hint_fails"
            | "free_page_hint_freed"
            | "free_page_report_count"
            | "free_page_report_fails"
            | "free_page_report_freed"
            | "inflate_count"
            | "stats_update_fails"
            | "stats_updates_count"
    )
}

fn memory_hotplug_state_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "plug_bytes"
            | "plug_count"
            | "plug_fails"
            | "state_count"
            | "state_fails"
            | "unplug_all_count"
            | "unplug_all_fails"
            | "unplug_bytes"
            | "unplug_count"
            | "unplug_discard_fails"
            | "unplug_fails"
    )
}

fn data_path_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "conns_added"
            | "conns_killed"
            | "conns_removed"
            | "entropy_bytes"
            | "error_count"
            | "execute_fails"
            | "flush_count"
            | "host_rng_fails"
            | "invalid_reqs_count"
            | "missed_read_count"
            | "missed_write_count"
            | "read_bytes"
            | "read_count"
            | "rx_bytes_count"
            | "rx_count"
            | "rx_fails"
            | "rx_packets_count"
            | "rx_read_fails"
            | "tap_read_fails"
            | "tap_write_fails"
            | "tx_bytes_count"
            | "tx_count"
            | "tx_fails"
            | "tx_flush_fails"
            | "tx_malformed_frames"
            | "tx_packets_count"
            | "tx_spoofed_mac_count"
            | "tx_write_fails"
            | "write_bytes"
            | "write_count"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_every_materialized_child_count() {
        let paths = [
            ("entropy.entropy_bytes", "#1838"),
            ("memory_hotplug.plug_count", "#1839"),
            ("vsock.conns_added", "#1840"),
            ("block_{drive_id}.read_count", "#1841"),
            ("vhost_user_block_{drive_id}.init_time_us", "#1842"),
            ("mmds.rx_count", "#1843"),
            ("net.tap_read_fails", "#1844"),
            ("vcpu.exit_mmio_read", "#1845"),
            ("vcpu.exit_io_in", "#1846"),
        ];
        for (path, issue) in paths {
            assert_eq!(expected_delivery_issue(path), Some(issue), "{path}");
            assert!(expected_boundary(path).is_some(), "{path}");
        }
    }
}
