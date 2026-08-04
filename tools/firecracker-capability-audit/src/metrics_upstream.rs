use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::spanned::Spanned;
use syn::{Attribute, Fields, Item, ItemStruct, Type};

use crate::upstream::{ensure_regular_input, git_output, read_input};
use crate::{
    AuditError, Baseline, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION, Input,
    METRICS_SCHEMA_GENERATOR_VERSION, METRICS_SCHEMA_VERSION, MetricsArchitecture,
    MetricsCardinality, MetricsDynamicFamily, MetricsJsonType, MetricsReconciliation,
    MetricsReconciliationKind, MetricsSchemaCounts, MetricsSchemaDisposition, MetricsSchemaSource,
    MetricsSchemaSourceCandidate, MetricsSourceAnchor, MetricsSourceField, MetricsStaticRoot,
    MetricsValueKind, ensure_pinned_checkout,
};

const PYTHON_EXTRACTOR: &str = "python-metrics-schema-v1";
const RUST_EXTRACTOR: &str = "rust-metrics-schema-v1";
const FIXTURE_PATH: &str = "tests/host_tools/fcmetrics.py";
const ROOT_METRICS_PATH: &str = "src/vmm/src/logger/metrics.rs";
const LEGACY_PATH: &str = "src/vmm/src/devices/legacy/mod.rs";
const I8042_PATH: &str = "src/vmm/src/devices/legacy/i8042.rs";
const RTC_PATH: &str = "src/vmm/src/devices/legacy/rtc_pl031.rs";
const SERIAL_PATH: &str = "src/vmm/src/devices/legacy/serial.rs";
const BALLOON_PATH: &str = "src/vmm/src/devices/virtio/balloon/metrics.rs";
const BLOCK_PATH: &str = "src/vmm/src/devices/virtio/block/virtio/metrics.rs";
const VHOST_BLOCK_PATH: &str = "src/vmm/src/devices/virtio/block/vhost_user/device.rs";
const MEMORY_HOTPLUG_PATH: &str = "src/vmm/src/devices/virtio/mem/metrics.rs";
const NET_PATH: &str = "src/vmm/src/devices/virtio/net/metrics.rs";
const PMEM_PATH: &str = "src/vmm/src/devices/virtio/pmem/metrics.rs";
const ENTROPY_PATH: &str = "src/vmm/src/devices/virtio/rng/metrics.rs";
const VHOST_USER_PATH: &str = "src/vmm/src/devices/virtio/vhost_user_metrics.rs";
const VSOCK_PATH: &str = "src/vmm/src/devices/virtio/vsock/metrics.rs";

const INPUT_PATHS: &[(&str, &str)] = &[
    (I8042_PATH, RUST_EXTRACTOR),
    (LEGACY_PATH, RUST_EXTRACTOR),
    (RTC_PATH, RUST_EXTRACTOR),
    (SERIAL_PATH, RUST_EXTRACTOR),
    (BALLOON_PATH, RUST_EXTRACTOR),
    (BLOCK_PATH, RUST_EXTRACTOR),
    (VHOST_BLOCK_PATH, RUST_EXTRACTOR),
    (MEMORY_HOTPLUG_PATH, RUST_EXTRACTOR),
    (NET_PATH, RUST_EXTRACTOR),
    (PMEM_PATH, RUST_EXTRACTOR),
    (ENTROPY_PATH, RUST_EXTRACTOR),
    (VHOST_USER_PATH, RUST_EXTRACTOR),
    (VSOCK_PATH, RUST_EXTRACTOR),
    (ROOT_METRICS_PATH, RUST_EXTRACTOR),
    (FIXTURE_PATH, PYTHON_EXTRACTOR),
];

#[derive(Clone, Copy)]
struct RootSpec {
    name: &'static str,
    rust_path: &'static str,
    rust_struct: &'static str,
    architecture: MetricsArchitecture,
    cardinality: MetricsCardinality,
}

const ROOT_SPECS: &[RootSpec] = &[
    root_spec("api_server", ROOT_METRICS_PATH, "ApiServerMetrics"),
    root_spec("balloon", BALLOON_PATH, "BalloonDeviceMetrics"),
    aggregate_root_spec("block", BLOCK_PATH, "BlockDeviceMetrics"),
    root_spec("deprecated_api", ROOT_METRICS_PATH, "DeprecatedApiMetrics"),
    root_spec("get_api_requests", ROOT_METRICS_PATH, "GetRequestsMetrics"),
    root_spec("i8042", I8042_PATH, "I8042DeviceMetrics"),
    RootSpec {
        name: "rtc",
        rust_path: RTC_PATH,
        rust_struct: "RTCDeviceMetrics",
        architecture: MetricsArchitecture::Arm64,
        cardinality: MetricsCardinality::Singleton,
    },
    root_spec("uart", SERIAL_PATH, "SerialDeviceMetrics"),
    root_spec("latencies_us", ROOT_METRICS_PATH, "PerformanceMetrics"),
    root_spec("logger", ROOT_METRICS_PATH, "LoggerSystemMetrics"),
    root_spec("mmds", ROOT_METRICS_PATH, "MmdsMetrics"),
    aggregate_root_spec("net", NET_PATH, "NetDeviceMetrics"),
    root_spec(
        "patch_api_requests",
        ROOT_METRICS_PATH,
        "PatchRequestsMetrics",
    ),
    root_spec("put_api_requests", ROOT_METRICS_PATH, "PutRequestsMetrics"),
    root_spec("seccomp", ROOT_METRICS_PATH, "SeccompMetrics"),
    root_spec("vcpu", ROOT_METRICS_PATH, "VcpuMetrics"),
    root_spec("vmm", ROOT_METRICS_PATH, "VmmMetrics"),
    root_spec("signals", ROOT_METRICS_PATH, "SignalMetrics"),
    root_spec("vsock", VSOCK_PATH, "VsockDeviceMetrics"),
    root_spec("entropy", ENTROPY_PATH, "EntropyDeviceMetrics"),
    aggregate_root_spec("pmem", PMEM_PATH, "PmemMetrics"),
    root_spec("interrupts", ROOT_METRICS_PATH, "InterruptMetrics"),
    root_spec(
        "memory_hotplug",
        MEMORY_HOTPLUG_PATH,
        "VirtioMemDeviceMetrics",
    ),
];

const fn root_spec(
    name: &'static str,
    rust_path: &'static str,
    rust_struct: &'static str,
) -> RootSpec {
    RootSpec {
        name,
        rust_path,
        rust_struct,
        architecture: MetricsArchitecture::All,
        cardinality: MetricsCardinality::Singleton,
    }
}

const fn aggregate_root_spec(
    name: &'static str,
    rust_path: &'static str,
    rust_struct: &'static str,
) -> RootSpec {
    RootSpec {
        name,
        rust_path,
        rust_struct,
        architecture: MetricsArchitecture::All,
        cardinality: MetricsCardinality::Aggregate,
    }
}

#[derive(Debug, Clone)]
struct PythonString {
    value: String,
    anchor: MetricsSourceAnchor,
}

#[derive(Debug, Clone)]
enum PythonValue {
    String(PythonString),
    Name(String),
    List(Vec<PythonValue>),
    Dict(Vec<(PythonString, PythonValue)>),
}

#[derive(Debug)]
struct FixtureSchema {
    roots: Vec<PythonString>,
    static_fields: BTreeMap<String, Vec<MetricsSourceAnchor>>,
    dynamic_fields: Vec<FixtureDynamicFamily>,
}

#[derive(Debug)]
struct FixtureDynamicFamily {
    id: String,
    fields: Vec<(String, MetricsSourceAnchor)>,
    anchor: MetricsSourceAnchor,
}

#[derive(Debug, Clone)]
struct RustField {
    name: String,
    wire_name: String,
    rust_type: String,
    flatten: bool,
    anchor: MetricsSourceAnchor,
}

#[derive(Debug, Clone)]
struct RustStruct {
    path: String,
    name: String,
    fields: Vec<RustField>,
}

#[derive(Debug, Clone)]
struct FlattenedRustField {
    path: String,
    value_kind: MetricsValueKind,
    rust_type: String,
    anchor: MetricsSourceAnchor,
}

/// Derive the machine-owned metrics schema source from pinned upstream syntax.
pub fn derive_metrics_schema_source(
    path: &Path,
) -> Result<MetricsSchemaSourceCandidate, AuditError> {
    let checkout = ensure_pinned_checkout(path)?;
    let (inputs, sources) = read_inputs(&checkout)?;
    let fixture_source = sources
        .get(FIXTURE_PATH)
        .ok_or_else(|| AuditError::new("missing checked metrics fixture input"))?;
    let fixture = extract_fixture_schema(fixture_source)?;
    let structs = extract_rust_structs(&sources)?;
    let root_order = extract_static_root_order(&structs, &sources)?;
    let root_specs = ROOT_SPECS
        .iter()
        .map(|spec| (spec.name, *spec))
        .collect::<BTreeMap<_, _>>();

    let fixture_roots = fixture
        .roots
        .iter()
        .map(|root| root.value.as_str())
        .collect::<BTreeSet<_>>();
    let derived_roots = root_order
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if fixture_roots != derived_roots {
        return Err(AuditError::new(format!(
            "metrics fixture and Rust serializers disagree on arm64 static roots: fixture={fixture_roots:?}; rust={derived_roots:?}"
        )));
    }

    let mut static_roots = Vec::new();
    let mut fields = Vec::new();
    for root_name in &root_order {
        if root_name == "utc_timestamp_ms" {
            let root_field = fixture
                .static_fields
                .get(root_name)
                .and_then(|anchors| anchors.first())
                .ok_or_else(|| AuditError::new("fixture is missing utc_timestamp_ms"))?;
            let rust_field =
                find_struct_field(&structs, ROOT_METRICS_PATH, "FirecrackerMetrics", root_name)?;
            static_roots.push(MetricsStaticRoot {
                name: root_name.clone(),
                architecture: MetricsArchitecture::All,
                schema_disposition: MetricsSchemaDisposition::RequiredStatic,
                cardinality: MetricsCardinality::Singleton,
                producer_anchor: rust_field.anchor.clone(),
                aggregation_anchor: None,
            });
            fields.push(MetricsSourceField {
                ordinal: fields.len(),
                id: format!("static:{root_name}"),
                path: root_name.clone(),
                json_type: MetricsJsonType::Number,
                value_kind: MetricsValueKind::AttemptTimestamp,
                rust_type: rust_field.rust_type.clone(),
                fixture_anchor: root_field.clone(),
                producer_anchor: rust_field.anchor.clone(),
            });
            continue;
        }

        let spec = root_specs.get(root_name.as_str()).ok_or_else(|| {
            AuditError::new(format!(
                "unsupported metrics root derived from Rust: {root_name}"
            ))
        })?;
        let producer_anchor = root_producer_anchor(spec, &structs, &sources)?;
        static_roots.push(MetricsStaticRoot {
            name: root_name.clone(),
            architecture: spec.architecture,
            schema_disposition: MetricsSchemaDisposition::RequiredStatic,
            cardinality: spec.cardinality,
            producer_anchor,
            aggregation_anchor: root_aggregation_anchor(spec, &sources)?,
        });
        let rust_fields = flatten_struct(&structs, spec.rust_path, spec.rust_struct)?;
        append_scope_fields(
            &mut fields,
            "static",
            root_name,
            &rust_fields,
            &fixture.static_fields,
        )?;
    }

    let mut reconciliations = duplicate_reconciliations(&fixture.static_fields);
    let dynamic_families = build_dynamic_families(
        &fixture,
        &structs,
        &sources,
        &mut fields,
        &mut reconciliations,
    )?;

    let counts = MetricsSchemaCounts {
        static_roots: static_roots.len(),
        static_fields: fields
            .iter()
            .filter(|field| field.id.starts_with("static:"))
            .count(),
        dynamic_families: dynamic_families.len(),
        block_dynamic_fields: dynamic_field_count(&dynamic_families, "block"),
        net_dynamic_fields: dynamic_field_count(&dynamic_families, "net"),
        vhost_user_dynamic_fields: dynamic_field_count(&dynamic_families, "vhost-user-block"),
    };

    Ok(MetricsSchemaSourceCandidate {
        schema_version: METRICS_SCHEMA_VERSION,
        baseline: Baseline {
            version: FIRECRACKER_VERSION.to_string(),
            commit: FIRECRACKER_COMMIT.to_string(),
            target: FIRECRACKER_TARGET.to_string(),
        },
        generator_version: METRICS_SCHEMA_GENERATOR_VERSION,
        source: MetricsSchemaSource {
            inputs,
            counts,
            static_roots,
            fields,
            dynamic_families,
            reconciliations,
        },
    })
}

fn extract_rust_structs(
    sources: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, RustStruct>, AuditError> {
    let mut structs = BTreeMap::new();
    for (path, source) in sources {
        if !path.ends_with(".rs") {
            continue;
        }
        let syntax = syn::parse_file(source)
            .map_err(|_| AuditError::new(format!("failed to parse Rust metrics input: {path}")))?;
        for item in syntax.items {
            let Item::Struct(item_struct) = item else {
                continue;
            };
            if !wanted_metrics_struct(path, &item_struct.ident.to_string()) {
                continue;
            }
            let Some(parsed) = parse_rust_struct(path, &item_struct)? else {
                continue;
            };
            let key = rust_struct_key(path, &parsed.name);
            if structs.insert(key.clone(), parsed).is_some() {
                return Err(AuditError::new(format!(
                    "duplicate Rust metrics struct identity: {key}"
                )));
            }
        }
    }
    Ok(structs)
}

fn parse_rust_struct(path: &str, item: &ItemStruct) -> Result<Option<RustStruct>, AuditError> {
    if !item
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .any(|attr| {
            attr.meta
                .to_token_stream()
                .to_string()
                .contains("Serialize")
        })
    {
        return Err(AuditError::new(format!(
            "Rust metrics struct must derive Serialize: {path}:{}",
            item.ident
        )));
    }
    let Fields::Named(named) = &item.fields else {
        return Ok(None);
    };
    let mut fields = Vec::new();
    for field in &named.named {
        let Some(ident) = &field.ident else {
            return Err(AuditError::new(format!(
                "named Rust metrics struct has an unnamed field: {path}:{}",
                item.ident
            )));
        };
        let rust_type = rust_type_name(&field.ty).ok_or_else(|| {
            AuditError::new(format!(
                "unsupported Rust metrics field type: {path}:{}.{}",
                item.ident, ident
            ))
        })?;
        let (wire_name, flatten) = serde_field_policy(&field.attrs, &ident.to_string())?;
        let start = field.span().start();
        fields.push(RustField {
            name: ident.to_string(),
            wire_name,
            rust_type,
            flatten,
            anchor: MetricsSourceAnchor {
                path: path.to_string(),
                symbol: format!("{}.{}", item.ident, ident),
                line: start.line,
                column: start.column + 1,
                fingerprint: fingerprint_tokens(field.to_token_stream()),
            },
        });
    }
    Ok(Some(RustStruct {
        path: path.to_string(),
        name: item.ident.to_string(),
        fields,
    }))
}

fn wanted_metrics_struct(path: &str, name: &str) -> bool {
    (path == ROOT_METRICS_PATH && matches!(name, "FirecrackerMetrics" | "LatencyAggregateMetrics"))
        || ROOT_SPECS
            .iter()
            .any(|spec| spec.rust_path == path && spec.rust_struct == name)
        || (path == VHOST_USER_PATH && name == "VhostUserDeviceMetrics")
}

fn rust_type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    (path.qself.is_none())
        .then(|| {
            path.path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
        })
        .flatten()
}

fn serde_field_policy(
    attrs: &[Attribute],
    default_name: &str,
) -> Result<(String, bool), AuditError> {
    let mut wire_name = default_name.to_string();
    let mut flatten = false;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("flatten") {
                flatten = true;
                return Ok(());
            }
            if meta.path.is_ident("rename") {
                let value = meta.value()?;
                let literal: syn::LitStr = value.parse()?;
                wire_name = literal.value();
                return Ok(());
            }
            Err(meta.error("unsupported serde attribute on metrics field"))
        })
        .map_err(|_| {
            AuditError::new(format!(
                "unsupported serde attribute on metrics field: {default_name}"
            ))
        })?;
    }
    Ok((wire_name, flatten))
}

fn extract_static_root_order(
    structs: &BTreeMap<String, RustStruct>,
    sources: &BTreeMap<String, String>,
) -> Result<Vec<String>, AuditError> {
    let root = find_struct(structs, ROOT_METRICS_PATH, "FirecrackerMetrics")?;
    let root_source = sources
        .get(ROOT_METRICS_PATH)
        .ok_or_else(|| AuditError::new("missing root metrics serializer input"))?;
    verify_ordered_snippets(
        root_source,
        &[
            "create_serialize_proxy!(BlockMetricsSerializeProxy, block_metrics);",
            "create_serialize_proxy!(NetMetricsSerializeProxy, net_metrics);",
            "create_serialize_proxy!(VhostUserMetricsSerializeProxy, vhost_user_metrics);",
            "create_serialize_proxy!(BalloonMetricsSerializeProxy, balloon_metrics);",
            "create_serialize_proxy!(EntropyMetricsSerializeProxy, entropy_metrics);",
            "create_serialize_proxy!(VsockMetricsSerializeProxy, vsock_metrics);",
            "create_serialize_proxy!(PmemMetricsSerializeProxy, pmem_metrics);",
            "create_serialize_proxy!(LegacyDevMetricsSerializeProxy, legacy);",
            "create_serialize_proxy!(MemoryHotplugSerializeProxy, virtio_mem_metrics);",
        ],
        "FirecrackerMetrics flattening proxies",
    )?;
    let root_types = ROOT_SPECS
        .iter()
        .map(|spec| (spec.rust_struct, spec.name))
        .collect::<BTreeMap<_, _>>();
    let mut roots = Vec::new();
    for field in &root.fields {
        let expansion: &[&str] = match field.rust_type.as_str() {
            "SerializeToUtcTimestampMs" => &["utc_timestamp_ms"],
            "BalloonMetricsSerializeProxy" => &["balloon"],
            "BlockMetricsSerializeProxy" => &["block"],
            "LegacyDevMetricsSerializeProxy" => &["i8042", "rtc", "uart"],
            "NetMetricsSerializeProxy" => &["net"],
            "VhostUserMetricsSerializeProxy" => &[],
            "EntropyMetricsSerializeProxy" => &["entropy"],
            "VsockMetricsSerializeProxy" => &["vsock"],
            "PmemMetricsSerializeProxy" => &["pmem"],
            "MemoryHotplugSerializeProxy" => &["memory_hotplug"],
            other => {
                let expected = root_types.get(other).ok_or_else(|| {
                    AuditError::new(format!(
                        "unsupported FirecrackerMetrics field type: {} -> {other}",
                        field.name
                    ))
                })?;
                if field.flatten || field.wire_name != *expected {
                    return Err(AuditError::new(format!(
                        "unexpected FirecrackerMetrics field serialization: {}",
                        field.name
                    )));
                }
                roots.push((*expected).to_string());
                continue;
            }
        };
        if field.rust_type.ends_with("SerializeProxy") && !field.flatten {
            return Err(AuditError::new(format!(
                "metrics serialize proxy is not flattened: {}",
                field.name
            )));
        }
        roots.extend(expansion.iter().map(|root| (*root).to_string()));
    }

    let legacy = sources
        .get(LEGACY_PATH)
        .ok_or_else(|| AuditError::new("missing legacy metrics serializer input"))?;
    verify_ordered_snippets(
        legacy,
        &[
            "serialize_entry(\"i8042\"",
            "serialize_entry(\"rtc\"",
            "serialize_entry(\"uart\"",
        ],
        "legacy metrics roots",
    )?;
    let rtc_offset = unique_offset(legacy, "serialize_entry(\"rtc\"")?;
    let rtc_prefix = legacy
        .get(rtc_offset.saturating_sub(96)..rtc_offset)
        .ok_or_else(|| AuditError::new("invalid rtc serializer source range"))?;
    if !rtc_prefix.contains("#[cfg(target_arch = \"aarch64\")]") {
        return Err(AuditError::new(
            "rtc metrics root must be guarded by target_arch = aarch64",
        ));
    }
    Ok(roots)
}

fn root_producer_anchor(
    spec: &RootSpec,
    structs: &BTreeMap<String, RustStruct>,
    sources: &BTreeMap<String, String>,
) -> Result<MetricsSourceAnchor, AuditError> {
    if spec.rust_path == ROOT_METRICS_PATH {
        return Ok(
            find_struct_field(structs, ROOT_METRICS_PATH, "FirecrackerMetrics", spec.name)?
                .anchor
                .clone(),
        );
    }
    let serializer_path = match spec.name {
        "i8042" | "rtc" | "uart" => LEGACY_PATH,
        _ => spec.rust_path,
    };
    let source = sources.get(serializer_path).ok_or_else(|| {
        AuditError::new(format!(
            "missing metrics serializer input: {serializer_path}"
        ))
    })?;
    anchor_for_snippet(
        source,
        serializer_path,
        &format!("serialize_entry(\"{}\"", spec.name),
        format!("serialize-root:{}", spec.name),
    )
}

fn root_aggregation_anchor(
    spec: &RootSpec,
    sources: &BTreeMap<String, String>,
) -> Result<Option<MetricsSourceAnchor>, AuditError> {
    if spec.cardinality != MetricsCardinality::Aggregate {
        return Ok(None);
    }
    let source = sources.get(spec.rust_path).ok_or_else(|| {
        AuditError::new(format!(
            "missing aggregate metrics input: {}",
            spec.rust_path
        ))
    })?;
    let syntax = syn::parse_file(source).map_err(|_| {
        AuditError::new(format!(
            "failed to parse aggregate metrics input: {}",
            spec.rust_path
        ))
    })?;
    let mut matches = Vec::new();
    for item in syntax.items {
        let Item::Impl(item_impl) = item else {
            continue;
        };
        let Some(self_type) = rust_type_name(&item_impl.self_ty) else {
            continue;
        };
        if self_type != spec.rust_struct {
            continue;
        }
        for impl_item in item_impl.items {
            let syn::ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if method.sig.ident == "aggregate" {
                let start = method.span().start();
                matches.push(MetricsSourceAnchor {
                    path: spec.rust_path.to_string(),
                    symbol: format!("{}::aggregate", spec.rust_struct),
                    line: start.line,
                    column: start.column + 1,
                    fingerprint: fingerprint_tokens(method.to_token_stream()),
                });
            }
        }
    }
    match matches.as_slice() {
        [anchor] => Ok(Some(anchor.clone())),
        [] => Err(AuditError::new(format!(
            "aggregate metrics root is missing its aggregate method: {}",
            spec.name
        ))),
        _ => Err(AuditError::new(format!(
            "aggregate metrics root has multiple aggregate methods: {}",
            spec.name
        ))),
    }
}

fn flatten_struct(
    structs: &BTreeMap<String, RustStruct>,
    path: &str,
    name: &str,
) -> Result<Vec<FlattenedRustField>, AuditError> {
    let root = find_struct(structs, path, name)?;
    let mut fields = Vec::new();
    let mut stack = BTreeSet::new();
    flatten_rust_struct(structs, root, "", &mut stack, &mut fields)?;
    Ok(fields)
}

fn flatten_rust_struct(
    structs: &BTreeMap<String, RustStruct>,
    item: &RustStruct,
    prefix: &str,
    stack: &mut BTreeSet<String>,
    output: &mut Vec<FlattenedRustField>,
) -> Result<(), AuditError> {
    let identity = rust_struct_key(&item.path, &item.name);
    if !stack.insert(identity.clone()) {
        return Err(AuditError::new(format!(
            "recursive Rust metrics struct: {identity}"
        )));
    }
    for field in &item.fields {
        if field.flatten {
            return Err(AuditError::new(format!(
                "nested metrics value struct unexpectedly flattens field: {identity}.{}",
                field.name
            )));
        }
        let field_path = join_path(prefix, &field.wire_name);
        let value_kind = match field.rust_type.as_str() {
            "SharedIncMetric" => Some(MetricsValueKind::IncrementalInterval),
            "SharedStoreMetric" => Some(MetricsValueKind::PersistentStore),
            _ => None,
        };
        if let Some(value_kind) = value_kind {
            output.push(FlattenedRustField {
                path: field_path,
                value_kind,
                rust_type: field.rust_type.clone(),
                anchor: field.anchor.clone(),
            });
            continue;
        }
        let nested = find_struct_by_name(structs, &field.rust_type)?;
        flatten_rust_struct(structs, nested, &field_path, stack, output)?;
    }
    stack.remove(&identity);
    Ok(())
}

fn append_scope_fields(
    output: &mut Vec<MetricsSourceField>,
    scope: &str,
    root_or_template: &str,
    rust_fields: &[FlattenedRustField],
    fixture_fields: &BTreeMap<String, Vec<MetricsSourceAnchor>>,
) -> Result<Vec<String>, AuditError> {
    let fixture_prefix = format!("{root_or_template}.");
    let expected = fixture_fields
        .keys()
        .filter(|path| path.starts_with(&fixture_prefix))
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual = rust_fields
        .iter()
        .map(|field| join_path(root_or_template, &field.path))
        .collect::<BTreeSet<_>>();
    if expected != actual {
        return Err(AuditError::new(format!(
            "metrics fixture and Rust struct disagree for {root_or_template}: fixture={expected:?}; rust={actual:?}"
        )));
    }

    let mut ids = Vec::new();
    for field in rust_fields {
        let path = join_path(root_or_template, &field.path);
        let anchors = fixture_fields
            .get(&path)
            .ok_or_else(|| AuditError::new(format!("metrics fixture field is missing: {path}")))?;
        let fixture_anchor = anchors.first().ok_or_else(|| {
            AuditError::new(format!("metrics fixture field has no anchor: {path}"))
        })?;
        let id = format!("{scope}:{path}");
        ids.push(id.clone());
        output.push(MetricsSourceField {
            ordinal: output.len(),
            id,
            path,
            json_type: MetricsJsonType::Number,
            value_kind: field.value_kind,
            rust_type: field.rust_type.clone(),
            fixture_anchor: fixture_anchor.clone(),
            producer_anchor: field.anchor.clone(),
        });
    }
    Ok(ids)
}

fn build_dynamic_families(
    fixture: &FixtureSchema,
    structs: &BTreeMap<String, RustStruct>,
    sources: &BTreeMap<String, String>,
    fields: &mut Vec<MetricsSourceField>,
    reconciliations: &mut Vec<MetricsReconciliation>,
) -> Result<Vec<MetricsDynamicFamily>, AuditError> {
    struct DynamicSpec {
        id: &'static str,
        prefix: &'static str,
        template: &'static str,
        cardinality: MetricsCardinality,
        rust_path: &'static str,
        rust_struct: &'static str,
    }
    let specs = [
        DynamicSpec {
            id: "block",
            prefix: "block_",
            template: "block_{drive_id}",
            cardinality: MetricsCardinality::ConfiguredBlock,
            rust_path: BLOCK_PATH,
            rust_struct: "BlockDeviceMetrics",
        },
        DynamicSpec {
            id: "net",
            prefix: "net_",
            template: "net_{iface_id}",
            cardinality: MetricsCardinality::ConfiguredNetwork,
            rust_path: NET_PATH,
            rust_struct: "NetDeviceMetrics",
        },
        DynamicSpec {
            id: "vhost-user-block",
            prefix: "vhost_user_",
            template: "vhost_user_block_{drive_id}",
            cardinality: MetricsCardinality::ConfiguredVhostUserBlock,
            rust_path: VHOST_USER_PATH,
            rust_struct: "VhostUserDeviceMetrics",
        },
    ];

    let mut families = Vec::new();
    for spec in specs {
        let fixture_family = fixture
            .dynamic_fields
            .iter()
            .find(|family| family.id == spec.id)
            .ok_or_else(|| {
                AuditError::new(format!(
                    "metrics fixture dynamic family is missing: {}",
                    spec.id
                ))
            })?;
        let fixture_map = fixture_family
            .fields
            .iter()
            .map(|(path, anchor)| (join_path(spec.template, path), vec![anchor.clone()]))
            .collect::<BTreeMap<_, _>>();
        let rust_fields = flatten_struct(structs, spec.rust_path, spec.rust_struct)?;
        let field_ids =
            append_scope_fields(fields, "dynamic", spec.template, &rust_fields, &fixture_map)?;
        let producer_anchors = dynamic_producer_anchors(spec.id, sources)?;
        families.push(MetricsDynamicFamily {
            id: spec.id.to_string(),
            fixture_prefix: spec.prefix.to_string(),
            producer_template: spec.template.to_string(),
            architecture: MetricsArchitecture::All,
            schema_disposition: MetricsSchemaDisposition::ConfiguredDynamic,
            cardinality: spec.cardinality,
            field_ids,
            fixture_anchor: fixture_family.anchor.clone(),
            producer_anchors,
        });
    }

    let pmem_source = sources
        .get(PMEM_PATH)
        .ok_or_else(|| AuditError::new("missing pmem metrics source"))?;
    let fixture_source = sources
        .get(FIXTURE_PATH)
        .ok_or_else(|| AuditError::new("missing metrics fixture source"))?;
    reconciliations.push(MetricsReconciliation {
        id: "producer-only:pmem_{pmem_id}".to_string(),
        kind: MetricsReconciliationKind::ProducerOnlyDynamicFamily,
        resolution: "Pinned Rust emits pmem_{pmem_id}, but the strict v1.16.0 fixture admits no pmem_ prefix; retain the producer fact without adding it to the canonical schema."
            .to_string(),
        source_anchors: vec![
            anchor_for_snippet(
                pmem_source,
                PMEM_PATH,
                "format!(\"pmem_{}\", name)",
                "producer-template:pmem_{pmem_id}".to_string(),
            )?,
            anchor_for_snippet(
                fixture_source,
                FIXTURE_PATH,
                "for metrics_name in metrics.keys():",
                "validate_fc_metrics:dynamic-prefix-closure".to_string(),
            )?,
        ],
    });
    Ok(families)
}

fn dynamic_producer_anchors(
    id: &str,
    sources: &BTreeMap<String, String>,
) -> Result<Vec<MetricsSourceAnchor>, AuditError> {
    let specs: &[(&str, &str, &str)] = match id {
        "block" => &[(
            BLOCK_PATH,
            "format!(\"block_{}\", name)",
            "producer-template:block_{drive_id}",
        )],
        "net" => &[(
            NET_PATH,
            "format!(\"net_{}\", name)",
            "producer-template:net_{iface_id}",
        )],
        "vhost-user-block" => &[
            (
                VHOST_USER_PATH,
                "format!(\"vhost_user_{}\", name)",
                "producer-template:vhost_user_{module_name}",
            ),
            (
                VHOST_BLOCK_PATH,
                "format!(\"block_{}\", config.drive_id)",
                "producer-module:block_{drive_id}",
            ),
            (
                VHOST_BLOCK_PATH,
                "VhostUserMetricsPerDevice::alloc(vhost_user_block_metrics_name)",
                "producer-registration:vhost-user-block",
            ),
        ],
        _ => {
            return Err(AuditError::new(format!(
                "unsupported metrics dynamic family: {id}"
            )));
        }
    };
    if id == "vhost-user-block"
        && sources
            .get(VHOST_BLOCK_PATH)
            .is_some_and(|source| source.contains("BlockMetricsPerDevice::alloc"))
    {
        return Err(AuditError::new(
            "vhost-user block must not also register ordinary block metrics",
        ));
    }
    specs
        .iter()
        .map(|(path, snippet, symbol)| {
            let source = sources
                .get(*path)
                .ok_or_else(|| AuditError::new(format!("missing dynamic metrics input: {path}")))?;
            anchor_for_snippet(source, path, snippet, (*symbol).to_string())
        })
        .collect()
}

fn duplicate_reconciliations(
    fields: &BTreeMap<String, Vec<MetricsSourceAnchor>>,
) -> Vec<MetricsReconciliation> {
    fields
        .iter()
        .filter(|(_, anchors)| anchors.len() > 1)
        .map(|(path, anchors)| MetricsReconciliation {
            id: format!("duplicate-fixture-field:{path}"),
            kind: MetricsReconciliationKind::DuplicateFixtureField,
            resolution: "Repeated fixture literals identify one required JSON property and are deduplicated by path."
                .to_string(),
            source_anchors: anchors.clone(),
        })
        .collect()
}

fn find_struct<'a>(
    structs: &'a BTreeMap<String, RustStruct>,
    path: &str,
    name: &str,
) -> Result<&'a RustStruct, AuditError> {
    structs
        .get(&rust_struct_key(path, name))
        .ok_or_else(|| AuditError::new(format!("missing Rust metrics struct: {path}:{name}")))
}

fn find_struct_by_name<'a>(
    structs: &'a BTreeMap<String, RustStruct>,
    name: &str,
) -> Result<&'a RustStruct, AuditError> {
    let matches = structs
        .values()
        .filter(|item| item.name == name)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item] => Ok(*item),
        [] => Err(AuditError::new(format!(
            "missing nested Rust metrics struct: {name}"
        ))),
        _ => Err(AuditError::new(format!(
            "ambiguous nested Rust metrics struct: {name}"
        ))),
    }
}

fn find_struct_field<'a>(
    structs: &'a BTreeMap<String, RustStruct>,
    path: &str,
    name: &str,
    field: &str,
) -> Result<&'a RustField, AuditError> {
    find_struct(structs, path, name)?
        .fields
        .iter()
        .find(|candidate| candidate.wire_name == field)
        .ok_or_else(|| {
            AuditError::new(format!("missing Rust metrics field: {path}:{name}.{field}"))
        })
}

fn rust_struct_key(path: &str, name: &str) -> String {
    format!("{path}#{name}")
}

fn unique_offset(source: &str, needle: &str) -> Result<usize, AuditError> {
    unique_offset_in(source, 0..source.len(), needle)
}

fn unique_offset_in(
    source: &str,
    range: std::ops::Range<usize>,
    needle: &str,
) -> Result<usize, AuditError> {
    let haystack = source
        .get(range.clone())
        .ok_or_else(|| AuditError::new("invalid metrics source search range"))?;
    let mut matches = haystack.match_indices(needle);
    let first = matches.next().map(|(offset, _)| range.start + offset);
    if matches.next().is_some() {
        return Err(AuditError::new(format!(
            "metrics source anchor is not unique: {needle}"
        )));
    }
    first.ok_or_else(|| AuditError::new(format!("metrics source anchor is missing: {needle}")))
}

fn verify_ordered_snippets(source: &str, snippets: &[&str], label: &str) -> Result<(), AuditError> {
    let mut previous = None;
    for snippet in snippets {
        let offset = unique_offset(source, snippet)?;
        if previous.is_some_and(|previous| offset <= previous) {
            return Err(AuditError::new(format!(
                "metrics source snippets are not in canonical order: {label}"
            )));
        }
        previous = Some(offset);
    }
    Ok(())
}

fn anchor_for_snippet(
    source: &str,
    path: &str,
    snippet: &str,
    symbol: String,
) -> Result<MetricsSourceAnchor, AuditError> {
    let start = unique_offset(source, snippet)?;
    let (line, column) = line_column(source, start);
    Ok(MetricsSourceAnchor {
        path: path.to_string(),
        symbol,
        line,
        column,
        fingerprint: fingerprint_text(snippet),
    })
}

fn anchor_for_range(source: &str, start: usize, end: usize, symbol: String) -> MetricsSourceAnchor {
    let snippet = source.get(start..end).unwrap_or_default();
    let (line, column) = line_column(source, start);
    MetricsSourceAnchor {
        path: FIXTURE_PATH.to_string(),
        symbol,
        line,
        column,
        fingerprint: fingerprint_text(snippet),
    }
}

fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let prefix = source.get(..offset).unwrap_or_default();
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len() + 1, |(_, tail)| tail.len() + 1);
    (line, column)
}

fn fingerprint_text(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    sha256_fingerprint(normalized.as_bytes())
}

fn fingerprint_tokens(tokens: TokenStream) -> String {
    let mut normalized = String::new();
    append_normalized_tokens(tokens, &mut normalized);
    sha256_fingerprint(normalized.as_bytes())
}

fn sha256_fingerprint(bytes: &[u8]) -> String {
    let hex_digit = |nibble: u8| {
        char::from(if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        })
    };
    let digest = Sha256::digest(bytes);
    let mut fingerprint = String::with_capacity(71);
    fingerprint.push_str("sha256:");
    for byte in digest {
        fingerprint.push(hex_digit(byte >> 4));
        fingerprint.push(hex_digit(byte & 0x0f));
    }
    fingerprint
}

fn append_normalized_tokens(tokens: TokenStream, output: &mut String) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                output.push(match group.delimiter() {
                    proc_macro2::Delimiter::Parenthesis => '(',
                    proc_macro2::Delimiter::Brace => '{',
                    proc_macro2::Delimiter::Bracket => '[',
                    proc_macro2::Delimiter::None => '<',
                });
                append_normalized_tokens(group.stream(), output);
                output.push(match group.delimiter() {
                    proc_macro2::Delimiter::Parenthesis => ')',
                    proc_macro2::Delimiter::Brace => '}',
                    proc_macro2::Delimiter::Bracket => ']',
                    proc_macro2::Delimiter::None => '>',
                });
            }
            other => {
                output.push_str(&other.to_string());
                output.push(' ');
            }
        }
    }
}

fn read_inputs(checkout: &Path) -> Result<(Vec<Input>, BTreeMap<String, String>), AuditError> {
    let mut inputs = Vec::new();
    let mut sources = BTreeMap::new();
    for (path, extractor) in INPUT_PATHS {
        ensure_regular_input(checkout, path)?;
        let source = read_input(checkout, path)?;
        let object = format!("HEAD:{path}");
        let git_blob = git_output(checkout, &["rev-parse", &object])?;
        let worktree_blob = git_output(checkout, &["hash-object", "--", path])?;
        if git_blob != worktree_blob {
            return Err(AuditError::new(format!(
                "metrics source input differs from its pinned Git blob: {path}"
            )));
        }
        inputs.push(Input {
            path: (*path).to_string(),
            git_blob,
            extractor: (*extractor).to_string(),
        });
        sources.insert((*path).to_string(), source);
    }
    inputs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((inputs, sources))
}

fn dynamic_field_count(families: &[MetricsDynamicFamily], id: &str) -> usize {
    families
        .iter()
        .find(|family| family.id == id)
        .map_or(0, |family| family.field_ids.len())
}

fn extract_fixture_schema(source: &str) -> Result<FixtureSchema, AuditError> {
    let function = function_slice(
        source,
        "def validate_fc_metrics(metrics):",
        "class FcDeviceMetrics:",
    )?;
    let mut values = BTreeMap::new();
    for name in [
        "latency_agg_metrics_fields",
        "block_metrics",
        "net_metrics",
        "firecracker_metrics",
    ] {
        values.insert(
            name.to_string(),
            parse_assignment(source, function.clone(), name)?,
        );
    }

    validate_fixture_assignment_boundary(source, function.clone())?;
    let static_value = values
        .get("firecracker_metrics")
        .ok_or_else(|| AuditError::new("metrics fixture static dictionary is missing"))?;
    let PythonValue::Dict(entries) = static_value else {
        return Err(AuditError::new(
            "metrics fixture firecracker_metrics must be a dictionary literal",
        ));
    };
    let mut roots = Vec::new();
    let mut static_fields = BTreeMap::<String, Vec<MetricsSourceAnchor>>::new();
    for (root, value) in entries {
        roots.push(root.clone());
        if root.value == "utc_timestamp_ms" {
            if !matches!(value, PythonValue::String(value) if value.value.is_empty()) {
                return Err(AuditError::new(
                    "metrics fixture utc_timestamp_ms sentinel must be an empty string",
                ));
            }
            static_fields
                .entry(root.value.clone())
                .or_default()
                .push(root.anchor.clone());
        } else {
            flatten_python_fields(value, &values, &root.value, &mut static_fields, 0)?;
        }
    }

    let rtc_value =
        parse_index_assignment(source, function.clone(), "firecracker_metrics[\"rtc\"]")?;
    let rtc_assignment = unique_offset_in(
        source,
        function.clone(),
        "        firecracker_metrics[\"rtc\"] =",
    )?;
    let arm64_condition = unique_offset_in(
        source,
        function.start..rtc_assignment,
        "    if platform.machine() == \"aarch64\":",
    )?;
    let condition_end = arm64_condition + "    if platform.machine() == \"aarch64\":".len();
    if rtc_assignment <= condition_end
        || source
            .get(condition_end..rtc_assignment)
            .is_none_or(|between| !between.trim().is_empty())
    {
        return Err(AuditError::new(
            "metrics fixture rtc insertion must be the exact arm64 conditional body",
        ));
    }
    let rtc_literal_start = rtc_assignment + "        firecracker_metrics[".len();
    let rtc_literal = anchor_for_range(
        source,
        rtc_literal_start,
        rtc_literal_start + "\"rtc\"".len(),
        "validate_fc_metrics:arm64-root:rtc".to_string(),
    );
    roots.push(PythonString {
        value: "rtc".to_string(),
        anchor: rtc_literal,
    });
    flatten_python_fields(&rtc_value, &values, "rtc", &mut static_fields, 0)?;

    let prefixes = startswith_prefixes(source, function.clone())?;
    let expected = ["vhost_user_", "block_", "net_"];
    if prefixes.as_slice() != expected {
        return Err(AuditError::new(format!(
            "metrics fixture dynamic prefixes must be {expected:?}, found {prefixes:?}"
        )));
    }

    let mut dynamic_fields = Vec::new();
    for (id, prefix) in [
        ("block", "block_"),
        ("net", "net_"),
        ("vhost-user-block", "vhost_user_"),
    ] {
        let (value, anchor) = parse_dynamic_assignment(source, function.clone(), prefix)?;
        let mut flattened = BTreeMap::<String, Vec<MetricsSourceAnchor>>::new();
        flatten_python_fields(&value, &values, "", &mut flattened, 0)?;
        let fields = flattened
            .into_iter()
            .map(|(path, anchors)| {
                let [anchor] = anchors.as_slice() else {
                    return Err(AuditError::new(format!(
                        "dynamic metrics fixture field must occur exactly once: {prefix}{path}"
                    )));
                };
                Ok((path, anchor.clone()))
            })
            .collect::<Result<Vec<_>, AuditError>>()?;
        dynamic_fields.push(FixtureDynamicFamily {
            id: id.to_string(),
            fields,
            anchor,
        });
    }

    Ok(FixtureSchema {
        roots,
        static_fields,
        dynamic_fields,
    })
}

fn function_slice(
    source: &str,
    start_marker: &str,
    end_marker: &str,
) -> Result<std::ops::Range<usize>, AuditError> {
    let start = unique_offset(source, start_marker)?;
    let remainder = source
        .get(start..)
        .ok_or_else(|| AuditError::new("invalid metrics fixture function offset"))?;
    let relative_end = remainder.find(end_marker).ok_or_else(|| {
        AuditError::new(format!("metrics fixture is missing marker: {end_marker}"))
    })?;
    Ok(start..start + relative_end)
}

fn parse_assignment(
    source: &str,
    function: std::ops::Range<usize>,
    name: &str,
) -> Result<PythonValue, AuditError> {
    let needle = format!("    {name} =");
    let assignment = unique_offset_in(source, function.clone(), &needle)?;
    parse_assignment_value(source, assignment + needle.len(), function.end, name)
}

fn parse_index_assignment(
    source: &str,
    function: std::ops::Range<usize>,
    left: &str,
) -> Result<PythonValue, AuditError> {
    let needle = format!("        {left} =");
    let assignment = unique_offset_in(source, function.clone(), &needle)?;
    parse_assignment_value(source, assignment + needle.len(), function.end, left)
}

fn parse_dynamic_assignment(
    source: &str,
    function: std::ops::Range<usize>,
    prefix: &str,
) -> Result<(PythonValue, MetricsSourceAnchor), AuditError> {
    let condition = format!("        if metrics_name.startswith(\"{prefix}\"):");
    let condition_offset = unique_offset_in(source, function.clone(), &condition)?;
    let next_condition = source
        .get(condition_offset + condition.len()..function.end)
        .and_then(|remaining| remaining.find("\n        if metrics_name.startswith("))
        .map_or(function.end, |relative| {
            condition_offset + condition.len() + relative
        });
    let branch = condition_offset..next_condition;
    let left = "            firecracker_metrics[metrics_name] =";
    let assignment = unique_offset_in(source, branch, left)?;
    let value = parse_assignment_value(source, assignment + left.len(), next_condition, prefix)?;
    let prefix_offset = condition_offset
        + condition
            .find('"')
            .ok_or_else(|| AuditError::new("metrics dynamic prefix quote is missing"))?;
    let anchor = anchor_for_range(
        source,
        prefix_offset,
        prefix_offset + prefix.len() + 2,
        format!("validate_fc_metrics:dynamic-prefix:{prefix}"),
    );
    Ok((value, anchor))
}

fn parse_assignment_value(
    source: &str,
    start: usize,
    limit: usize,
    label: &str,
) -> Result<PythonValue, AuditError> {
    let mut parser = PythonLiteralParser::new(source, start, limit);
    let value = parser.parse_value()?;
    let line_end = source
        .get(parser.position..limit)
        .and_then(|remaining| remaining.find('\n'))
        .map_or(limit, |relative| parser.position + relative);
    let trailing = source
        .get(parser.position..line_end)
        .ok_or_else(|| AuditError::new("invalid metrics fixture assignment suffix"))?
        .trim();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        return Err(AuditError::new(format!(
            "unsupported trailing metrics fixture syntax for {label}"
        )));
    }
    Ok(value)
}

fn validate_fixture_assignment_boundary(
    source: &str,
    function: std::ops::Range<usize>,
) -> Result<(), AuditError> {
    let marker = "    # validate timestamp before jsonschema validation";
    let end = unique_offset_in(source, function.clone(), marker)?;
    let start = unique_offset_in(
        source,
        function.start..end,
        "    latency_agg_metrics_fields =",
    )?;
    let prefix = source
        .get(start..end)
        .ok_or_else(|| AuditError::new("invalid metrics fixture schema boundary"))?;
    let mut assignments = Vec::new();
    for line in prefix.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("    ") else {
            return Err(AuditError::new(
                "metrics fixture schema statements must use exact function indentation",
            ));
        };
        if rest.starts_with(' ') {
            continue;
        }
        let statement = rest.trim();
        if statement.is_empty() || statement.starts_with('#') || matches!(statement, "]" | "}") {
            continue;
        }
        let Some((name, _)) = statement.split_once(" = ") else {
            return Err(AuditError::new(format!(
                "unsupported top-level metrics fixture schema statement: {statement}"
            )));
        };
        if !name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            return Err(AuditError::new(format!(
                "unsupported top-level metrics fixture schema assignment: {name}"
            )));
        }
        assignments.push(name.to_string());
    }
    let expected = [
        "latency_agg_metrics_fields",
        "block_metrics",
        "net_metrics",
        "firecracker_metrics",
    ];
    if assignments.iter().map(String::as_str).collect::<Vec<_>>() != expected {
        return Err(AuditError::new(format!(
            "unsupported metrics fixture schema assignments: {assignments:?}"
        )));
    }
    Ok(())
}

fn startswith_prefixes(
    source: &str,
    function: std::ops::Range<usize>,
) -> Result<Vec<String>, AuditError> {
    let function_source = source
        .get(function)
        .ok_or_else(|| AuditError::new("invalid metrics fixture function range"))?;
    let marker = "if metrics_name.startswith(\"";
    let mut prefixes = Vec::new();
    let mut remaining = function_source;
    while let Some(offset) = remaining.find(marker) {
        let after = remaining
            .get(offset + marker.len()..)
            .ok_or_else(|| AuditError::new("invalid metrics dynamic prefix offset"))?;
        let end = after.find("\"):").ok_or_else(|| {
            AuditError::new("unterminated metrics fixture dynamic prefix condition")
        })?;
        let prefix = after
            .get(..end)
            .ok_or_else(|| AuditError::new("invalid metrics dynamic prefix range"))?;
        if prefix.is_empty()
            || !prefix
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            return Err(AuditError::new(
                "unsupported metrics fixture dynamic prefix literal",
            ));
        }
        prefixes.push(prefix.to_string());
        remaining = after
            .get(end + 3..)
            .ok_or_else(|| AuditError::new("invalid metrics dynamic condition suffix"))?;
    }
    Ok(prefixes)
}

fn flatten_python_fields(
    value: &PythonValue,
    values: &BTreeMap<String, PythonValue>,
    prefix: &str,
    fields: &mut BTreeMap<String, Vec<MetricsSourceAnchor>>,
    depth: usize,
) -> Result<(), AuditError> {
    if depth > 8 {
        return Err(AuditError::new(
            "metrics fixture literal references are recursively nested",
        ));
    }
    match value {
        PythonValue::String(field) => {
            if field.value.is_empty() {
                return Err(AuditError::new(
                    "empty metrics fixture field is allowed only for utc_timestamp_ms",
                ));
            }
            let path = join_path(prefix, &field.value);
            fields.entry(path).or_default().push(field.anchor.clone());
        }
        PythonValue::Name(name) => {
            let referenced = values.get(name).ok_or_else(|| {
                AuditError::new(format!("unknown metrics fixture literal reference: {name}"))
            })?;
            flatten_python_fields(referenced, values, prefix, fields, depth + 1)?;
        }
        PythonValue::List(items) => {
            for item in items {
                flatten_python_fields(item, values, prefix, fields, depth + 1)?;
            }
        }
        PythonValue::Dict(entries) => {
            for (name, nested) in entries {
                let nested_prefix = join_path(prefix, &name.value);
                flatten_python_fields(nested, values, &nested_prefix, fields, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn join_path(prefix: &str, field: &str) -> String {
    if prefix.is_empty() {
        field.to_string()
    } else {
        format!("{prefix}.{field}")
    }
}

struct PythonLiteralParser<'a> {
    source: &'a str,
    position: usize,
    limit: usize,
}

impl<'a> PythonLiteralParser<'a> {
    fn new(source: &'a str, position: usize, limit: usize) -> Self {
        Self {
            source,
            position,
            limit,
        }
    }

    fn parse_value(&mut self) -> Result<PythonValue, AuditError> {
        self.skip_space_and_comments();
        match self.peek_byte() {
            Some(b'[') => self.parse_list(),
            Some(b'{') => self.parse_dict(),
            Some(b'\'' | b'"') => self.parse_string().map(PythonValue::String),
            Some(byte) if byte == b'_' || byte.is_ascii_alphabetic() => {
                self.parse_name().map(PythonValue::Name)
            }
            _ => Err(AuditError::new(
                "unsupported value in restricted metrics fixture literal",
            )),
        }
    }

    fn parse_list(&mut self) -> Result<PythonValue, AuditError> {
        self.expect_byte(b'[')?;
        let mut items = Vec::new();
        loop {
            self.skip_space_and_comments();
            if self.consume_byte(b']') {
                return Ok(PythonValue::List(items));
            }
            items.push(self.parse_value()?);
            self.skip_space_and_comments();
            if self.consume_byte(b']') {
                return Ok(PythonValue::List(items));
            }
            self.expect_byte(b',')?;
        }
    }

    fn parse_dict(&mut self) -> Result<PythonValue, AuditError> {
        self.expect_byte(b'{')?;
        let mut entries = Vec::new();
        loop {
            self.skip_space_and_comments();
            if self.consume_byte(b'}') {
                return Ok(PythonValue::Dict(entries));
            }
            let key = self.parse_string()?;
            self.skip_space_and_comments();
            self.expect_byte(b':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_space_and_comments();
            if self.consume_byte(b'}') {
                return Ok(PythonValue::Dict(entries));
            }
            self.expect_byte(b',')?;
        }
    }

    fn parse_string(&mut self) -> Result<PythonString, AuditError> {
        self.skip_space_and_comments();
        let start = self.position;
        let quote = self
            .peek_byte()
            .filter(|byte| matches!(byte, b'\'' | b'"'))
            .ok_or_else(|| AuditError::new("metrics fixture dictionary keys must be strings"))?;
        self.position += 1;
        let value_start = self.position;
        while let Some(byte) = self.peek_byte() {
            match byte {
                b'\\' => {
                    return Err(AuditError::new(
                        "escaped metrics fixture strings are outside the restricted grammar",
                    ));
                }
                byte if byte == quote => {
                    let value = self
                        .source
                        .get(value_start..self.position)
                        .ok_or_else(|| AuditError::new("invalid metrics fixture string range"))?;
                    if !value
                        .chars()
                        .all(|character| character == '_' || character.is_ascii_alphanumeric())
                    {
                        return Err(AuditError::new(
                            "metrics fixture schema strings must use identifier characters",
                        ));
                    }
                    self.position += 1;
                    return Ok(PythonString {
                        value: value.to_string(),
                        anchor: anchor_for_range(
                            self.source,
                            start,
                            self.position,
                            format!("validate_fc_metrics:literal:{value}"),
                        ),
                    });
                }
                b'\n' | b'\r' => {
                    return Err(AuditError::new(
                        "unterminated metrics fixture schema string",
                    ));
                }
                _ => self.position += 1,
            }
        }
        Err(AuditError::new(
            "unterminated metrics fixture schema string",
        ))
    }

    fn parse_name(&mut self) -> Result<String, AuditError> {
        self.skip_space_and_comments();
        let start = self.position;
        while self
            .peek_byte()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            self.position += 1;
        }
        let name = self
            .source
            .get(start..self.position)
            .ok_or_else(|| AuditError::new("invalid metrics fixture name range"))?;
        if name.is_empty() || name.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return Err(AuditError::new("invalid metrics fixture literal reference"));
        }
        Ok(name.to_string())
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self
                .peek_byte()
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                self.position += 1;
            }
            if self.peek_byte() != Some(b'#') {
                break;
            }
            while self.peek_byte().is_some_and(|byte| byte != b'\n') {
                self.position += 1;
            }
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), AuditError> {
        self.skip_space_and_comments();
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(AuditError::new(format!(
                "restricted metrics fixture literal expected `{}`",
                char::from(expected)
            )))
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.peek_byte() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        (self.position < self.limit)
            .then(|| self.source.as_bytes().get(self.position).copied())
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_python_literals_resolve_nested_references() {
        let source = r#"def validate_fc_metrics(metrics):
    latency_agg_metrics_fields = ["min_us", "max_us", "sum_us"]
    block_metrics = [{"read_agg": latency_agg_metrics_fields}, "read_count"]
    net_metrics = ["rx_count"]
    firecracker_metrics = {"utc_timestamp_ms": "", "block": block_metrics}
    # validate timestamp before jsonschema validation which some more time
    if platform.machine() == "aarch64":
        firecracker_metrics["rtc"] = ["error_count"]
    for metrics_name in metrics.keys():
        if metrics_name.startswith("vhost_user_"):
            firecracker_metrics[metrics_name] = ["activate_fails"]
        if metrics_name.startswith("block_"):
            firecracker_metrics[metrics_name] = block_metrics
        if metrics_name.startswith("net_"):
            firecracker_metrics[metrics_name] = net_metrics

class FcDeviceMetrics:
    pass
"#;
        let fixture = extract_fixture_schema(source).expect("restricted fixture must parse");
        assert_eq!(
            fixture
                .static_fields
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "block.read_agg.max_us",
                "block.read_agg.min_us",
                "block.read_agg.sum_us",
                "block.read_count",
                "rtc.error_count",
                "utc_timestamp_ms",
            ]
        );
        assert_eq!(fixture.dynamic_fields.len(), 3);
    }

    #[test]
    fn restricted_python_literals_reject_calls() {
        let source = "[metric_name()]";
        let mut parser = PythonLiteralParser::new(source, 0, source.len());
        let error = parser
            .parse_value()
            .expect_err("calls must be outside the restricted grammar");
        assert!(error.to_string().contains("expected `,`"));
    }

    #[test]
    fn restricted_python_literals_reject_escaped_schema_names() {
        let source = r#"["read\x5fcount"]"#;
        let mut parser = PythonLiteralParser::new(source, 0, source.len());
        let error = parser
            .parse_value()
            .expect_err("escaped names must be outside the restricted grammar");
        assert!(error.to_string().contains("escaped"));
    }

    #[test]
    fn restricted_fixture_rejects_an_unparsed_schema_mutation() {
        let source = r#"def validate_fc_metrics(metrics):
    latency_agg_metrics_fields = ["min_us", "max_us", "sum_us"]
    block_metrics = [{"read_agg": latency_agg_metrics_fields}, "read_count"]
    net_metrics = ["rx_count"]
    firecracker_metrics = {"utc_timestamp_ms": "", "block": block_metrics}
    firecracker_metrics["unparsed"] = ["count"]
    # validate timestamp before jsonschema validation which some more time
    if platform.machine() == "aarch64":
        firecracker_metrics["rtc"] = ["error_count"]
    for metrics_name in metrics.keys():
        if metrics_name.startswith("vhost_user_"):
            firecracker_metrics[metrics_name] = ["activate_fails"]
        if metrics_name.startswith("block_"):
            firecracker_metrics[metrics_name] = block_metrics
        if metrics_name.startswith("net_"):
            firecracker_metrics[metrics_name] = net_metrics

class FcDeviceMetrics:
    pass
"#;
        let error =
            extract_fixture_schema(source).expect_err("unparsed schema mutations must fail closed");
        assert!(error.to_string().contains("unsupported top-level"));
    }

    #[test]
    fn token_fingerprints_ignore_formatting_but_not_syntax() {
        let compact: syn::Field = syn::parse_quote!(pub count: SharedIncMetric);
        let documented: syn::Field = syn::parse_quote!(
            /// Count.
            pub count: SharedIncMetric
        );
        let store: syn::Field = syn::parse_quote!(pub count: SharedStoreMetric);
        assert_ne!(
            fingerprint_tokens(compact.to_token_stream()),
            fingerprint_tokens(documented.to_token_stream())
        );
        assert_ne!(
            fingerprint_tokens(compact.to_token_stream()),
            fingerprint_tokens(store.to_token_stream())
        );
    }
}
