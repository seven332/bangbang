use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    AuditMode, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    METRICS_SCHEMA_GENERATOR_VERSION, METRICS_SCHEMA_VERSION, MetricsAggregation,
    MetricsArchitecture, MetricsCardinality, MetricsFieldPolicy, MetricsPolicyProfile,
    MetricsProducerDisposition, MetricsProducerOwner, MetricsReconciliationKind,
    MetricsSchemaAuthority, MetricsSchemaDisposition, MetricsSourceAnchor, MetricsSourceField,
    MetricsUnit, MetricsValueKind, SourceManifest, ValidationErrors,
};

const EXPECTED_STATIC_ROOTS: usize = 24;
const EXPECTED_STATIC_FIELDS: usize = 243;
const EXPECTED_DYNAMIC_FAMILIES: usize = 3;
const EXPECTED_BLOCK_DYNAMIC_FIELDS: usize = 24;
const EXPECTED_NET_DYNAMIC_FIELDS: usize = 29;
const EXPECTED_VHOST_USER_DYNAMIC_FIELDS: usize = 5;
const EXPECTED_TOTAL_FIELDS: usize = EXPECTED_STATIC_FIELDS
    + EXPECTED_BLOCK_DYNAMIC_FIELDS
    + EXPECTED_NET_DYNAMIC_FIELDS
    + EXPECTED_VHOST_USER_DYNAMIC_FIELDS;
const PYTHON_EXTRACTOR: &str = "python-metrics-schema-v1";
const RUST_EXTRACTOR: &str = "rust-metrics-schema-v1";
const FIXTURE_PATH: &str = "tests/host_tools/fcmetrics.py";

const EXPECTED_ROOT_ORDER: &[&str] = &[
    "utc_timestamp_ms",
    "api_server",
    "balloon",
    "block",
    "deprecated_api",
    "get_api_requests",
    "i8042",
    "rtc",
    "uart",
    "latencies_us",
    "logger",
    "mmds",
    "net",
    "patch_api_requests",
    "put_api_requests",
    "seccomp",
    "vcpu",
    "vmm",
    "signals",
    "vsock",
    "entropy",
    "pmem",
    "interrupts",
    "memory_hotplug",
];

const EXPECTED_INPUTS: &[(&str, &str)] = &[
    ("src/vmm/src/devices/legacy/i8042.rs", RUST_EXTRACTOR),
    ("src/vmm/src/devices/legacy/mod.rs", RUST_EXTRACTOR),
    ("src/vmm/src/devices/legacy/rtc_pl031.rs", RUST_EXTRACTOR),
    ("src/vmm/src/devices/legacy/serial.rs", RUST_EXTRACTOR),
    (
        "src/vmm/src/devices/virtio/balloon/metrics.rs",
        RUST_EXTRACTOR,
    ),
    (
        "src/vmm/src/devices/virtio/block/vhost_user/device.rs",
        RUST_EXTRACTOR,
    ),
    (
        "src/vmm/src/devices/virtio/block/virtio/metrics.rs",
        RUST_EXTRACTOR,
    ),
    ("src/vmm/src/devices/virtio/mem/metrics.rs", RUST_EXTRACTOR),
    ("src/vmm/src/devices/virtio/net/metrics.rs", RUST_EXTRACTOR),
    ("src/vmm/src/devices/virtio/pmem/metrics.rs", RUST_EXTRACTOR),
    ("src/vmm/src/devices/virtio/rng/metrics.rs", RUST_EXTRACTOR),
    (
        "src/vmm/src/devices/virtio/vhost_user_metrics.rs",
        RUST_EXTRACTOR,
    ),
    (
        "src/vmm/src/devices/virtio/vsock/metrics.rs",
        RUST_EXTRACTOR,
    ),
    ("src/vmm/src/logger/metrics.rs", RUST_EXTRACTOR),
    (FIXTURE_PATH, PYTHON_EXTRACTOR),
];

/// Validate the checked metrics source and reviewed field policy locally.
pub fn validate_metrics_schema(
    authority: &MetricsSchemaAuthority,
    manifest: &SourceManifest,
    repository_root: &Path,
    mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(authority, manifest, &mut errors);
    validate_source_manifest_agreement(manifest, &mut errors);
    validate_inputs(authority, &mut errors);
    validate_counts(authority, &mut errors);
    validate_roots(authority, &mut errors);
    validate_fields(authority, &mut errors);
    validate_dynamic_families(authority, &mut errors);
    validate_reconciliations(authority, &mut errors);
    validate_policies(authority, repository_root, mode, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(
    authority: &MetricsSchemaAuthority,
    manifest: &SourceManifest,
    errors: &mut Vec<String>,
) {
    if authority.schema_version != METRICS_SCHEMA_VERSION {
        errors.push(format!(
            "metrics schema_version must be {METRICS_SCHEMA_VERSION}, found {}",
            authority.schema_version
        ));
    }
    if authority.generator_version != METRICS_SCHEMA_GENERATOR_VERSION {
        errors.push(format!(
            "metrics generator_version must be {METRICS_SCHEMA_GENERATOR_VERSION}, found {}",
            authority.generator_version
        ));
    }
    if authority.baseline != manifest.baseline {
        errors.push("metrics authority and source manifest baselines differ".to_string());
    }
    if authority.baseline.version != FIRECRACKER_VERSION {
        errors.push(format!(
            "metrics version must be {FIRECRACKER_VERSION}, found {}",
            authority.baseline.version
        ));
    }
    if authority.baseline.commit != FIRECRACKER_COMMIT {
        errors.push(format!(
            "metrics commit must be {FIRECRACKER_COMMIT}, found {}",
            authority.baseline.commit
        ));
    }
    if authority.baseline.target != FIRECRACKER_TARGET {
        errors.push(format!(
            "metrics target must be {FIRECRACKER_TARGET}, found {}",
            authority.baseline.target
        ));
    }
}

fn validate_source_manifest_agreement(manifest: &SourceManifest, errors: &mut Vec<String>) {
    let matches = manifest
        .items
        .iter()
        .filter(|item| item.id == "corpus:metrics")
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item]
            if item.kind == "corpus"
                && item.key == "metrics"
                && item.path == "docs/metrics.md"
                && item.family == "observability" => {}
        [_] => errors.push(
            "metrics authority requires the exact corpus:metrics source-manifest identity"
                .to_string(),
        ),
        [] => errors.push("source manifest is missing corpus:metrics".to_string()),
        _ => {
            errors.push("source manifest contains duplicate corpus:metrics identities".to_string())
        }
    }
}

fn validate_inputs(authority: &MetricsSchemaAuthority, errors: &mut Vec<String>) {
    let actual = authority
        .source
        .inputs
        .iter()
        .map(|input| (input.path.as_str(), input.extractor.as_str()))
        .collect::<Vec<_>>();
    if actual != EXPECTED_INPUTS {
        errors.push(format!(
            "metrics source inputs must match the exact curated set: found {actual:?}"
        ));
    }
    for input in &authority.source.inputs {
        if !is_safe_upstream_path(&input.path) {
            errors.push(format!("metrics input path is unsafe: {}", input.path));
        }
        if !is_hex(&input.git_blob, 40) {
            errors.push(format!(
                "metrics input git_blob must be a 40-character lowercase hex object id: {}",
                input.path
            ));
        }
    }
}

fn validate_counts(authority: &MetricsSchemaAuthority, errors: &mut Vec<String>) {
    let counts = &authority.source.counts;
    let expected = [
        ("static_roots", counts.static_roots, EXPECTED_STATIC_ROOTS),
        (
            "static_fields",
            counts.static_fields,
            EXPECTED_STATIC_FIELDS,
        ),
        (
            "dynamic_families",
            counts.dynamic_families,
            EXPECTED_DYNAMIC_FAMILIES,
        ),
        (
            "block_dynamic_fields",
            counts.block_dynamic_fields,
            EXPECTED_BLOCK_DYNAMIC_FIELDS,
        ),
        (
            "net_dynamic_fields",
            counts.net_dynamic_fields,
            EXPECTED_NET_DYNAMIC_FIELDS,
        ),
        (
            "vhost_user_dynamic_fields",
            counts.vhost_user_dynamic_fields,
            EXPECTED_VHOST_USER_DYNAMIC_FIELDS,
        ),
    ];
    for (name, actual, expected) in expected {
        if actual != expected {
            errors.push(format!("metrics {name} must be {expected}, found {actual}"));
        }
    }
    let actual_static = authority
        .source
        .fields
        .iter()
        .filter(|field| field.id.starts_with("static:"))
        .count();
    if actual_static != counts.static_fields {
        errors.push(format!(
            "metrics static field count does not match records: declared={}; actual={actual_static}",
            counts.static_fields
        ));
    }
    if authority.source.fields.len() != EXPECTED_TOTAL_FIELDS {
        errors.push(format!(
            "metrics total field population must be {EXPECTED_TOTAL_FIELDS}, found {}",
            authority.source.fields.len()
        ));
    }
}

fn validate_roots(authority: &MetricsSchemaAuthority, errors: &mut Vec<String>) {
    let names = authority
        .source
        .static_roots
        .iter()
        .map(|root| root.name.as_str())
        .collect::<Vec<_>>();
    if names != EXPECTED_ROOT_ORDER {
        errors.push(format!(
            "metrics static root order must be {EXPECTED_ROOT_ORDER:?}, found {names:?}"
        ));
    }
    for root in &authority.source.static_roots {
        let expected_architecture = if root.name == "rtc" {
            MetricsArchitecture::Arm64
        } else {
            MetricsArchitecture::All
        };
        if root.architecture != expected_architecture {
            errors.push(format!("metrics root architecture is wrong: {}", root.name));
        }
        if root.schema_disposition != MetricsSchemaDisposition::RequiredStatic {
            errors.push(format!(
                "metrics static root must be required-static: {}",
                root.name
            ));
        }
        let expected_cardinality = if matches!(root.name.as_str(), "block" | "net" | "pmem") {
            MetricsCardinality::Aggregate
        } else {
            MetricsCardinality::Singleton
        };
        if root.cardinality != expected_cardinality {
            errors.push(format!("metrics root cardinality is wrong: {}", root.name));
        }
        validate_anchor(authority, &root.producer_anchor, "root producer", errors);
        match (&root.aggregation_anchor, expected_cardinality) {
            (Some(anchor), MetricsCardinality::Aggregate) => {
                validate_anchor(authority, anchor, "root aggregation", errors);
            }
            (None, MetricsCardinality::Aggregate) => errors.push(format!(
                "aggregate metrics root is missing its aggregation anchor: {}",
                root.name
            )),
            (Some(_), _) => errors.push(format!(
                "singleton metrics root must not have an aggregation anchor: {}",
                root.name
            )),
            (None, _) => {}
        }
    }
}

fn validate_fields(authority: &MetricsSchemaAuthority, errors: &mut Vec<String>) {
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let roots = authority
        .source
        .static_roots
        .iter()
        .map(|root| root.name.as_str())
        .collect::<BTreeSet<_>>();
    for (ordinal, field) in authority.source.fields.iter().enumerate() {
        if field.ordinal != ordinal {
            errors.push(format!(
                "metrics field ordinal must match canonical wire order: {} -> {} instead of {ordinal}",
                field.id, field.ordinal
            ));
        }
        if !ids.insert(field.id.as_str()) {
            errors.push(format!("duplicate metrics field id: {}", field.id));
        }
        if !paths.insert(field.path.as_str()) {
            errors.push(format!("duplicate metrics field path: {}", field.path));
        }
        let scope = if field.id.starts_with("static:") {
            "static"
        } else if field.id.starts_with("dynamic:") {
            "dynamic"
        } else {
            errors.push(format!(
                "metrics field id has an unknown scope: {}",
                field.id
            ));
            "invalid"
        };
        if field.id != format!("{scope}:{}", field.path) {
            errors.push(format!(
                "metrics field id must match its exact path: {}",
                field.id
            ));
        }
        if scope == "static" {
            let root = field.path.split('.').next().unwrap_or_default();
            if !roots.contains(root) {
                errors.push(format!(
                    "metrics static field names an unknown root: {}",
                    field.id
                ));
            }
        }
        let expected_kind = match field.rust_type.as_str() {
            "SharedIncMetric" => MetricsValueKind::IncrementalInterval,
            "SharedStoreMetric" => MetricsValueKind::PersistentStore,
            "SerializeToUtcTimestampMs" => MetricsValueKind::AttemptTimestamp,
            _ => {
                errors.push(format!(
                    "metrics field has an unsupported Rust scalar type: {} -> {}",
                    field.id, field.rust_type
                ));
                field.value_kind
            }
        };
        if field.value_kind != expected_kind {
            errors.push(format!(
                "metrics field reset/value kind disagrees with its Rust type: {}",
                field.id
            ));
        }
        if field.fixture_anchor.path != FIXTURE_PATH {
            errors.push(format!(
                "metrics field fixture anchor must name {FIXTURE_PATH}: {}",
                field.id
            ));
        }
        if !field.producer_anchor.path.ends_with(".rs") {
            errors.push(format!(
                "metrics field producer anchor must name Rust source: {}",
                field.id
            ));
        }
        validate_anchor(authority, &field.fixture_anchor, "field fixture", errors);
        validate_anchor(authority, &field.producer_anchor, "field producer", errors);
    }
}

fn validate_dynamic_families(authority: &MetricsSchemaAuthority, errors: &mut Vec<String>) {
    let expected = [
        (
            "block",
            "block_",
            "block_{drive_id}",
            MetricsCardinality::ConfiguredBlock,
            EXPECTED_BLOCK_DYNAMIC_FIELDS,
            1,
        ),
        (
            "net",
            "net_",
            "net_{iface_id}",
            MetricsCardinality::ConfiguredNetwork,
            EXPECTED_NET_DYNAMIC_FIELDS,
            1,
        ),
        (
            "vhost-user-block",
            "vhost_user_",
            "vhost_user_block_{drive_id}",
            MetricsCardinality::ConfiguredVhostUserBlock,
            EXPECTED_VHOST_USER_DYNAMIC_FIELDS,
            3,
        ),
    ];
    let field_ids = authority
        .source
        .fields
        .iter()
        .map(|field| field.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut family_field_ids = BTreeSet::new();
    for (index, family) in authority.source.dynamic_families.iter().enumerate() {
        let Some((id, prefix, template, cardinality, count, producer_anchor_count)) =
            expected.get(index)
        else {
            errors.push(format!("unexpected metrics dynamic family: {}", family.id));
            continue;
        };
        if family.id != *id
            || family.fixture_prefix != *prefix
            || family.producer_template != *template
            || family.cardinality != *cardinality
            || family.field_ids.len() != *count
        {
            errors.push(format!(
                "metrics dynamic family contract is wrong: {}",
                family.id
            ));
        }
        if family.architecture != MetricsArchitecture::All
            || family.schema_disposition != MetricsSchemaDisposition::ConfiguredDynamic
        {
            errors.push(format!(
                "metrics dynamic family architecture/disposition is wrong: {}",
                family.id
            ));
        }
        if family.producer_anchors.len() != *producer_anchor_count {
            errors.push(format!(
                "metrics dynamic family producer anchor count is wrong: {}",
                family.id
            ));
        }
        validate_anchor(authority, &family.fixture_anchor, "dynamic fixture", errors);
        for anchor in &family.producer_anchors {
            validate_anchor(authority, anchor, "dynamic producer", errors);
        }
        for field_id in &family.field_ids {
            if !field_ids.contains(field_id.as_str()) {
                errors.push(format!(
                    "metrics dynamic family references a missing field: {} -> {field_id}",
                    family.id
                ));
            }
            if !field_id.starts_with(&format!("dynamic:{template}.")) {
                errors.push(format!(
                    "metrics dynamic family field has malformed template identity: {} -> {field_id}",
                    family.id
                ));
            }
            if !family_field_ids.insert(field_id.as_str()) {
                errors.push(format!(
                    "metrics dynamic field belongs to multiple families: {field_id}"
                ));
            }
        }
    }
    let all_dynamic = authority
        .source
        .fields
        .iter()
        .filter(|field| field.id.starts_with("dynamic:"))
        .map(|field| field.id.as_str())
        .collect::<BTreeSet<_>>();
    if all_dynamic != family_field_ids {
        errors.push(
            "metrics dynamic family membership does not exactly cover dynamic fields".to_string(),
        );
    }
}

fn validate_reconciliations(authority: &MetricsSchemaAuthority, errors: &mut Vec<String>) {
    let expected = [(
        "producer-only:pmem_{pmem_id}",
        MetricsReconciliationKind::ProducerOnlyDynamicFamily,
        2,
    )];
    if authority.source.reconciliations.len() != expected.len() {
        errors.push(format!(
            "metrics source must contain exactly {} reconciliations, found {}",
            expected.len(),
            authority.source.reconciliations.len()
        ));
    }
    for (index, reconciliation) in authority.source.reconciliations.iter().enumerate() {
        let Some((id, kind, anchor_count)) = expected.get(index) else {
            errors.push(format!(
                "unexpected metrics source reconciliation: {}",
                reconciliation.id
            ));
            continue;
        };
        if reconciliation.id != *id
            || reconciliation.kind != *kind
            || reconciliation.source_anchors.len() != *anchor_count
        {
            errors.push(format!(
                "metrics source reconciliation is wrong: {}",
                reconciliation.id
            ));
        }
        if reconciliation.resolution.trim().len() < 32 {
            errors.push(format!(
                "metrics source reconciliation needs an exact resolution: {}",
                reconciliation.id
            ));
        }
        for anchor in &reconciliation.source_anchors {
            validate_anchor(authority, anchor, "source reconciliation", errors);
        }
    }
}

fn validate_policies(
    authority: &MetricsSchemaAuthority,
    repository_root: &Path,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    let mut profiles = BTreeMap::new();
    let mut previous_profile = None;
    for profile in &authority.policy_profiles {
        if previous_profile.is_some_and(|previous| profile.id.as_str() <= previous) {
            errors.push("metrics policy profiles must be sorted and unique by id".to_string());
        }
        previous_profile = Some(profile.id.as_str());
        if profiles.insert(profile.id.as_str(), profile).is_some() {
            errors.push(format!("duplicate metrics policy profile: {}", profile.id));
        }
        validate_profile(profile, repository_root, mode, errors);
    }

    let source_fields = authority
        .source
        .fields
        .iter()
        .map(|field| (field.id.as_str(), field))
        .collect::<BTreeMap<_, _>>();
    let mut mappings = BTreeMap::<&str, &MetricsFieldPolicy>::new();
    let mut previous_field = None;
    for policy in &authority.field_policies {
        if previous_field.is_some_and(|previous| policy.field_id.as_str() <= previous) {
            errors.push("metrics field policies must be sorted and unique by field_id".to_string());
        }
        previous_field = Some(policy.field_id.as_str());
        if mappings.insert(policy.field_id.as_str(), policy).is_some() {
            errors.push(format!(
                "duplicate metrics field policy: {}",
                policy.field_id
            ));
        }
        let Some(field) = source_fields.get(policy.field_id.as_str()) else {
            errors.push(format!("stale metrics field policy: {}", policy.field_id));
            continue;
        };
        let Some(profile) = profiles.get(policy.profile_id.as_str()) else {
            errors.push(format!(
                "metrics field policy references an unknown profile: {} -> {}",
                policy.field_id, policy.profile_id
            ));
            continue;
        };
        validate_field_policy(field, profile, errors);
    }
    for field_id in source_fields.keys() {
        if !mappings.contains_key(field_id) {
            errors.push(format!("missing metrics field policy: {field_id}"));
        }
    }
    let used_profiles = mappings
        .values()
        .map(|policy| policy.profile_id.as_str())
        .collect::<BTreeSet<_>>();
    for profile_id in profiles.keys() {
        if !used_profiles.contains(profile_id) {
            errors.push(format!("unused metrics policy profile: {profile_id}"));
        }
    }
}

fn validate_profile(
    profile: &MetricsPolicyProfile,
    repository_root: &Path,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    let expected_id = policy_profile_id(profile);
    if profile.id != expected_id {
        errors.push(format!(
            "metrics policy profile id must encode its closed metadata: {} != {expected_id}",
            profile.id
        ));
    }
    let expected_issue = match profile.producer_disposition {
        MetricsProducerDisposition::Planned | MetricsProducerDisposition::PlatformZero => {
            Some(match profile.producer_owner {
                MetricsProducerOwner::SchemaRuntime => "#1822",
                MetricsProducerOwner::ProcessLifecycle => "#1788",
                MetricsProducerOwner::Device => "#1789",
            })
        }
        MetricsProducerDisposition::Implemented => None,
    };
    if profile.delivery_issue.as_deref() != expected_issue {
        errors.push(format!(
            "metrics policy profile has the wrong delivery issue: {}",
            profile.id
        ));
    }
    if profile.rationale != expected_rationale(profile) {
        errors.push(format!(
            "metrics policy profile has a stale rationale: {}",
            profile.id
        ));
    }
    match profile.producer_disposition {
        MetricsProducerDisposition::Planned | MetricsProducerDisposition::PlatformZero => {
            if !profile.implementation.is_empty() || !profile.validation.is_empty() {
                errors.push(format!(
                    "nonterminal metrics policy must not claim implementation evidence: {}",
                    profile.id
                ));
            }
            if mode == AuditMode::Final {
                errors.push(format!(
                    "final metrics validation rejects nonterminal producer policy: {}",
                    profile.id
                ));
            }
        }
        MetricsProducerDisposition::Implemented => {
            if profile.implementation.is_empty() || profile.validation.is_empty() {
                errors.push(format!(
                    "implemented metrics policy needs implementation and validation evidence: {}",
                    profile.id
                ));
            }
            let tracked = tracked_repository_files(repository_root, errors);
            for (kind, references) in [
                ("implementation", &profile.implementation),
                ("validation", &profile.validation),
            ] {
                for reference in references {
                    validate_reference(
                        reference,
                        repository_root,
                        &tracked,
                        &format!("metrics profile {} {kind}", profile.id),
                        errors,
                    );
                }
            }
        }
    }
}

fn validate_field_policy(
    field: &MetricsSourceField,
    profile: &MetricsPolicyProfile,
    errors: &mut Vec<String>,
) {
    let expected_unit = expected_unit(&field.path);
    if profile.unit != expected_unit {
        errors.push(format!("metrics field has the wrong unit: {}", field.id));
    }
    let expected_aggregation = expected_aggregation(&field.path);
    if profile.aggregation != expected_aggregation {
        errors.push(format!(
            "metrics field has the wrong aggregation policy: {}",
            field.id
        ));
    }
    let expected_owner = expected_owner(&field.path);
    if profile.producer_owner != expected_owner {
        errors.push(format!(
            "metrics field has the wrong producer owner: {}",
            field.id
        ));
    }
    let platform_zero = is_platform_zero(&field.path);
    if platform_zero != (profile.producer_disposition == MetricsProducerDisposition::PlatformZero) {
        errors.push(format!(
            "metrics field has the wrong platform producer disposition: {}",
            field.id
        ));
    }
}

fn expected_unit(path: &str) -> MetricsUnit {
    if path == "utc_timestamp_ms" {
        MetricsUnit::MillisecondsSinceUnixEpoch
    } else if path.starts_with("latencies_us.")
        || path.ends_with("_us")
        || path.contains("_agg.min_us")
        || path.contains("_agg.max_us")
        || path.contains("_agg.sum_us")
    {
        MetricsUnit::Microseconds
    } else if path.contains("bytes")
        || path.ends_with("free_page_report_freed")
        || path.ends_with("free_page_hint_freed")
    {
        MetricsUnit::Bytes
    } else {
        MetricsUnit::Count
    }
}

fn expected_aggregation(path: &str) -> MetricsAggregation {
    let static_device_aggregate =
        path.starts_with("block.") || path.starts_with("net.") || path.starts_with("pmem.");
    if static_device_aggregate {
        if path.ends_with(".min_us") || path.ends_with(".max_us") {
            MetricsAggregation::ZeroInConfiguredDeviceAggregate
        } else {
            MetricsAggregation::SumAcrossConfiguredDevices
        }
    } else if path.ends_with(".min_us") {
        MetricsAggregation::Minimum
    } else if path.ends_with(".max_us") {
        MetricsAggregation::Maximum
    } else if path.ends_with(".sum_us") {
        MetricsAggregation::Sum
    } else {
        MetricsAggregation::None
    }
}

fn expected_owner(path: &str) -> MetricsProducerOwner {
    if path == "utc_timestamp_ms" {
        return MetricsProducerOwner::SchemaRuntime;
    }
    let root = path.split('.').next().unwrap_or_default();
    if root.starts_with("block_{")
        || root.starts_with("net_{")
        || root.starts_with("vhost_user_block_{")
        || matches!(
            root,
            "balloon"
                | "block"
                | "i8042"
                | "mmds"
                | "net"
                | "vcpu"
                | "uart"
                | "vsock"
                | "entropy"
                | "interrupts"
                | "pmem"
                | "memory_hotplug"
                | "rtc"
        )
    {
        MetricsProducerOwner::Device
    } else {
        MetricsProducerOwner::ProcessLifecycle
    }
}

fn is_platform_zero(path: &str) -> bool {
    path.starts_with("i8042.")
        || matches!(
            path,
            "net.mac_address_updates"
                | "net_{iface_id}.mac_address_updates"
                | "vcpu.exit_io_in"
                | "vcpu.exit_io_out"
                | "vcpu.exit_io_in_agg.min_us"
                | "vcpu.exit_io_in_agg.max_us"
                | "vcpu.exit_io_in_agg.sum_us"
                | "vcpu.exit_io_out_agg.min_us"
                | "vcpu.exit_io_out_agg.max_us"
                | "vcpu.exit_io_out_agg.sum_us"
                | "vcpu.kvmclock_ctrl_fails"
        )
}

fn policy_profile_id(profile: &MetricsPolicyProfile) -> String {
    format!(
        "{}-{}-{}-{}",
        enum_json_name(&profile.unit),
        enum_json_name(&profile.aggregation),
        enum_json_name(&profile.producer_owner),
        enum_json_name(&profile.producer_disposition),
    )
}

fn enum_json_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "invalid".to_string())
}

fn expected_rationale(profile: &MetricsPolicyProfile) -> &'static str {
    match profile.producer_disposition {
        MetricsProducerDisposition::PlatformZero => {
            "The arm64 schema retains this Linux/x86-oriented field as a required numeric neutral value; #1789 owns terminal evidence."
        }
        MetricsProducerDisposition::Planned => match profile.producer_owner {
            MetricsProducerOwner::SchemaRuntime => {
                "Canonical line construction and timestamp publication are delivered by #1822 before producer closure."
            }
            MetricsProducerOwner::ProcessLifecycle => {
                "#1788 owns the exact API, process, logger, signal, boot, and lifecycle producer boundary."
            }
            MetricsProducerOwner::Device => {
                "#1789 owns the exact supported-device producer, neutral-value, and bounded-key boundary."
            }
        },
        MetricsProducerDisposition::Implemented => {
            "The producer has exact implementation and validation evidence for this checked field policy."
        }
    }
}

fn validate_anchor(
    authority: &MetricsSchemaAuthority,
    anchor: &MetricsSourceAnchor,
    label: &str,
    errors: &mut Vec<String>,
) {
    let input_paths = authority
        .source
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect::<BTreeSet<_>>();
    if !input_paths.contains(anchor.path.as_str()) {
        errors.push(format!(
            "metrics {label} anchor names an untracked input: {}",
            anchor.path
        ));
    }
    if !is_safe_upstream_path(&anchor.path) {
        errors.push(format!(
            "metrics {label} anchor path is unsafe: {}",
            anchor.path
        ));
    }
    if anchor.symbol.trim().is_empty() || anchor.line == 0 || anchor.column == 0 {
        errors.push(format!(
            "metrics {label} anchor is incomplete: {}",
            anchor.path
        ));
    }
    let Some(hash) = anchor.fingerprint.strip_prefix("sha256:") else {
        errors.push(format!(
            "metrics {label} anchor fingerprint lacks sha256 prefix: {}",
            anchor.path
        ));
        return;
    };
    if !is_hex(hash, 64) {
        errors.push(format!(
            "metrics {label} anchor fingerprint must contain 64 lowercase hex characters: {}",
            anchor.path
        ));
    }
}

fn is_safe_upstream_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_units_and_aggregation_without_name_substrings_only() {
        assert_eq!(
            expected_unit("utc_timestamp_ms"),
            MetricsUnit::MillisecondsSinceUnixEpoch
        );
        assert_eq!(
            expected_unit("memory_hotplug.plug_agg.sum_us"),
            MetricsUnit::Microseconds
        );
        assert_eq!(
            expected_unit("balloon.free_page_report_freed"),
            MetricsUnit::Bytes
        );
        assert_eq!(
            expected_aggregation("block.read_agg.min_us"),
            MetricsAggregation::ZeroInConfiguredDeviceAggregate
        );
        assert_eq!(expected_aggregation("dynamic"), MetricsAggregation::None);
    }

    #[test]
    fn only_exact_arm64_neutral_fields_are_platform_zero() {
        assert!(is_platform_zero("i8042.read_count"));
        assert!(is_platform_zero("net.mac_address_updates"));
        assert!(is_platform_zero("net_{iface_id}.mac_address_updates"));
        assert!(is_platform_zero("vcpu.exit_io_in_agg.sum_us"));
        assert!(!is_platform_zero("vcpu.exit_mmio_read_agg.sum_us"));
        assert!(!is_platform_zero("rtc.error_count"));
    }
}
