use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use syn::parse::{Parse, ParseStream};
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprMethodCall, ImplItemFn, ItemFn, ItemMod, LitStr, Macro, Meta, Token,
    TraitItemFn,
};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    AuditMode, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION, Reference,
    TRACING_AUDIT_SCHEMA_VERSION, TracingAudit, TracingCallSite, TracingCallSiteCategory,
    TracingDelivery, TracingFeatureContract, TracingField, TracingLimits, TracingPhase,
    ValidationErrors,
};

/// Exact checked production tracing call-site identities.
pub const TRACING_CALL_SITE_IDS: [&str; 8] = [
    "api.handle-request",
    "device.virtio-mmio-read",
    "device.virtio-mmio-write",
    "runtime.handle-action",
    "tool.rebase",
    "tool.snapshot-info",
    "tool.snapshot-register-removal",
    "vmm.handle-action",
];

const EXPECTED_FORBIDDEN_FIELDS: [&str; 12] = [
    "addresses",
    "credentials",
    "descriptors",
    "environment-values",
    "errors",
    "guest-values",
    "host-paths",
    "identities",
    "payloads",
    "registers",
    "selectors",
    "timestamps",
];

const EXPECTED_NONCLAIMS: [&str; 4] = [
    "Firecracker source-rewrite or instrumentation-mechanism identity",
    "durable trace delivery",
    "platform-independent timing thresholds",
    "production tracing enabled by default",
];

/// Validate the checked tracing authority and the exact production macro set.
pub fn validate_tracing_audit(
    audit: &TracingAudit,
    repository_root: &Path,
    _mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, &mut errors);
    validate_contract(audit, &mut errors);
    validate_implementation_shape(repository_root, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_references(
        &audit.implementation,
        "tracing audit implementation",
        repository_root,
        &tracked,
        &mut errors,
    );
    validate_references(
        &audit.validation,
        "tracing audit validation",
        repository_root,
        &tracked,
        &mut errors,
    );
    validate_call_sites(audit, repository_root, &tracked, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(audit: &TracingAudit, errors: &mut Vec<String>) {
    if audit.schema_version != TRACING_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "tracing audit schema_version must be {TRACING_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("tracing audit baseline is not the pinned release".to_string());
    }
    if audit.issue != "#1791" {
        errors.push("tracing audit must be owned by #1791".to_string());
    }
}

fn validate_contract(audit: &TracingAudit, errors: &mut Vec<String>) {
    let expected_feature = TracingFeatureContract {
        name: "tracing".to_string(),
        default_enabled: false,
        release_default_enabled: false,
        tool_runtime_filter_environment: "BANGBANG_TRACE".to_string(),
    };
    if audit.feature != expected_feature {
        errors.push("tracing feature contract has drifted".to_string());
    }
    let expected_limits = TracingLimits {
        max_depth: 32,
        max_record_bytes: 512,
        max_records_per_scope: 2,
        tool_queue_capacity: 8,
        tool_receipt_timeout_ms: 100,
    };
    if audit.limits != expected_limits {
        errors.push("tracing fixed limits have drifted".to_string());
    }
    if audit.phases != [TracingPhase::Enter, TracingPhase::Exit] {
        errors.push("tracing phases must be exactly enter then exit".to_string());
    }
    if audit.allowed_fields
        != [
            TracingField::Module,
            TracingField::Thread,
            TracingField::Scope,
            TracingField::Phase,
        ]
    {
        errors.push("tracing allowed fields have drifted".to_string());
    }
    if audit.forbidden_fields != EXPECTED_FORBIDDEN_FIELDS.map(str::to_string).to_vec() {
        errors.push("tracing forbidden fields have drifted".to_string());
    }
    if audit.nonclaims != EXPECTED_NONCLAIMS.map(str::to_string).to_vec() {
        errors.push("tracing nonclaims have drifted".to_string());
    }
}

fn validate_implementation_shape(repository_root: &Path, errors: &mut Vec<String>) {
    require_source_fragments(
        repository_root,
        "crates/runtime/Cargo.toml",
        &["default = []", "tracing = []"],
        errors,
    );
    require_source_fragments(
        repository_root,
        "crates/bangbang/Cargo.toml",
        &["default = []", "tracing = [\"bangbang-runtime/tracing\"]"],
        errors,
    );
    require_source_fragments(
        repository_root,
        "tools/snapshot-tools/Cargo.toml",
        &["default = []", "tracing = [\"bangbang-runtime/tracing\"]"],
        errors,
    );
    require_source_fragments(
        repository_root,
        "crates/runtime/src/lib.rs",
        &[
            "macro_rules! bangbang_trace_scope",
            "$module:literal, $scope:literal",
            "#[cfg(feature = \"tracing\")]",
            "let _bangbang_trace_scope = ($logger).enter_fixed($module, $scope);",
        ],
        errors,
    );
    require_source_fragments(
        repository_root,
        "crates/runtime/src/logger/tracing.rs",
        &[
            "pub const MAX_TRACE_DEPTH: usize = 32;",
            "const TOOL_TRACE_ENVIRONMENT: &str = \"BANGBANG_TRACE\";",
            "not_send: PhantomData<Rc<()>>",
            ".try_with(|stack|",
            "stack.try_borrow_mut().ok()?",
            "TraceDelivery::BoundedHost => producer.deliver_host(batch)",
            "TraceDelivery::NonblockingGuest => producer.deliver_nonblocking(batch)",
        ],
        errors,
    );
    require_source_fragments(
        repository_root,
        "crates/runtime/src/logger/event.rs",
        &[
            "const MAX_LOG_RECORD_BYTES: usize = 512;",
            "pub(super) fn encode_trace(",
            "encoder.push_str(\"trace module=\");",
            "encoder.push_str(\" thread=\");",
            "encoder.push_str(\" scope=\");",
            "encoder.push_str(\" phase=\");",
        ],
        errors,
    );
    require_source_fragments(
        repository_root,
        "crates/runtime/src/logger/delivery.rs",
        &[
            "pub(super) const fn for_tool_tracing() -> Self",
            "const TOOL_TRACE_TIMEOUT: Duration = Duration::from_millis(100);",
            "queue_capacity: 8,",
        ],
        errors,
    );
}

fn require_source_fragments(
    repository_root: &Path,
    path: &str,
    fragments: &[&str],
    errors: &mut Vec<String>,
) {
    let Ok(source) = std::fs::read_to_string(repository_root.join(path)) else {
        errors.push(format!(
            "tracing audit cannot read implementation source: {path}"
        ));
        return;
    };
    for fragment in fragments {
        if !source.contains(fragment) {
            errors.push(format!(
                "tracing implementation source is missing required shape: {path}: {fragment}"
            ));
        }
    }
}

fn validate_call_sites(
    audit: &TracingAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    if audit.call_sites.len() != TRACING_CALL_SITE_IDS.len() {
        errors.push(format!(
            "tracing audit must contain {} call sites, found {}",
            TRACING_CALL_SITE_IDS.len(),
            audit.call_sites.len()
        ));
    }
    let mut previous = None;
    let mut declared = BTreeMap::new();
    for call_site in &audit.call_sites {
        if previous.is_some_and(|id| call_site.id.as_str() <= id) {
            errors.push("tracing call sites must be sorted and unique by id".to_string());
        }
        previous = Some(call_site.id.as_str());
        if declared.insert(call_site.id.as_str(), call_site).is_some() {
            errors.push(format!("duplicate tracing call site: {}", call_site.id));
        }
        validate_call_site(call_site, repository_root, tracked, errors);
    }

    let actual_ids = declared.keys().copied().collect::<BTreeSet<_>>();
    let expected_ids = TRACING_CALL_SITE_IDS.into_iter().collect::<BTreeSet<_>>();
    if actual_ids != expected_ids {
        errors.push(format!(
            "tracing audit requires the exact call-site id set: expected {expected_ids:?}, found {actual_ids:?}"
        ));
    }

    let expected_calls = call_site_specs()
        .into_iter()
        .map(|spec| ObservedCallSite {
            path: spec.path.to_string(),
            module: spec.module.to_string(),
            scope: spec.scope.to_string(),
        })
        .collect::<BTreeSet<_>>();
    let observed_calls = observed_tracing_call_sites(repository_root, tracked, errors);
    if observed_calls != expected_calls {
        errors.push(format!(
            "production tracing macro set has drifted: expected {expected_calls:?}, found {observed_calls:?}"
        ));
    }
}

fn validate_call_site(
    call_site: &TracingCallSite,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let Some(spec) = call_site_specs()
        .into_iter()
        .find(|spec| spec.id == call_site.id)
    else {
        return;
    };
    if call_site.path != spec.path
        || call_site.category != spec.category
        || call_site.module != spec.module
        || call_site.scope != spec.scope
        || call_site.delivery != spec.delivery
        || call_site.rationale != spec.rationale
    {
        errors.push(format!(
            "tracing call-site policy has drifted: {}",
            call_site.id
        ));
    }
    validate_references(
        &call_site.implementation,
        &format!("tracing call site {} implementation", call_site.id),
        repository_root,
        tracked,
        errors,
    );
    validate_references(
        &call_site.validation,
        &format!("tracing call site {} validation", call_site.id),
        repository_root,
        tracked,
        errors,
    );
}

fn validate_references(
    references: &[Reference],
    label: &str,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    if references.is_empty() {
        errors.push(format!("{label} requires evidence"));
    }
    if references
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left >= right))
    {
        errors.push(format!("{label} references must be sorted and unique"));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("{label}[{index}]"),
            errors,
        );
        let Reference::Local {
            path,
            anchor: Some(anchor),
        } = reference
        else {
            errors.push(format!(
                "{label}[{index}] must be an anchored local reference"
            ));
            continue;
        };
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(source) if source.contains(anchor) => {}
            Ok(_) => errors.push(format!("{label}[{index}] anchor does not resolve")),
            Err(_) => errors.push(format!("{label}[{index}] path is unreadable")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ObservedCallSite {
    path: String,
    module: String,
    scope: String,
}

fn observed_tracing_call_sites(
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) -> BTreeSet<ObservedCallSite> {
    let mut observed = BTreeSet::new();
    for path in tracked
        .iter()
        .filter(|path| is_production_rust_source(path))
    {
        let display = path.to_string_lossy().replace('\\', "/");
        let source = match std::fs::read_to_string(repository_root.join(path)) {
            Ok(source) => source,
            Err(_) => {
                errors.push(format!(
                    "tracing scanner cannot read Rust source: {display}"
                ));
                continue;
            }
        };
        let syntax = match syn::parse_file(&source) {
            Ok(syntax) => syntax,
            Err(error) => {
                errors.push(format!(
                    "tracing scanner cannot parse Rust source: {display}: {error}"
                ));
                continue;
            }
        };
        let mut visitor = TraceMacroVisitor {
            path: &display,
            observed: &mut observed,
            errors,
        };
        visitor.visit_file(&syntax);
    }
    observed
}

fn is_production_rust_source(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    matches!(components.as_slice(), ["crates" | "tools", _, "src", ..])
        && path.extension().is_some_and(|extension| extension == "rs")
}

struct TraceMacroVisitor<'a> {
    path: &'a str,
    observed: &'a mut BTreeSet<ObservedCallSite>,
    errors: &'a mut Vec<String>,
}

impl<'ast> Visit<'ast> for TraceMacroVisitor<'_> {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if node.method == "enter_fixed" {
            self.errors.push(format!(
                "production tracing must use bangbang_trace_scope! instead of direct enter_fixed: {}",
                self.path
            ));
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        if node
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "bangbang_trace_scope")
        {
            match syn::parse2::<TraceMacroArguments>(node.tokens.clone()) {
                Ok(arguments) => {
                    let call = ObservedCallSite {
                        path: self.path.to_string(),
                        module: arguments.module.value(),
                        scope: arguments.scope.value(),
                    };
                    if !self.observed.insert(call.clone()) {
                        self.errors.push(format!(
                            "duplicate production tracing macro identity: {call:?}"
                        ));
                    }
                }
                Err(error) => self.errors.push(format!(
                    "production tracing macro requires expression, literal module, and literal scope: {}: {error}",
                    self.path
                )),
            }
        }
        visit::visit_macro(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !is_test_only(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if !is_test_only(&node.attrs) {
            visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        if !is_test_only(&node.attrs) {
            visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        if !is_test_only(&node.attrs) {
            visit::visit_trait_item_fn(self, node);
        }
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if attribute.path().is_ident("test") {
            return true;
        }
        let Meta::List(list) = &attribute.meta else {
            return false;
        };
        attribute.path().is_ident("cfg")
            && list
                .parse_args_with(syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated)
                .is_ok_and(|conditions| conditions.iter().any(meta_requires_test))
    })
}

fn meta_requires_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated)
            .is_ok_and(|conditions| conditions.iter().any(meta_requires_test)),
        Meta::List(list) if list.path.is_ident("any") => list
            .parse_args_with(syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated)
            .is_ok_and(|conditions| {
                !conditions.is_empty() && conditions.iter().all(meta_requires_test)
            }),
        Meta::List(_) => false,
        Meta::NameValue(_) => false,
    }
}

struct TraceMacroArguments {
    _logger: Expr,
    module: LitStr,
    scope: LitStr,
}

impl Parse for TraceMacroArguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let logger = input.parse()?;
        input.parse::<Token![,]>()?;
        let module = input.parse()?;
        input.parse::<Token![,]>()?;
        let scope = input.parse()?;
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error("unexpected trailing tracing macro tokens"));
        }
        Ok(Self {
            _logger: logger,
            module,
            scope,
        })
    }
}

struct CallSiteSpec {
    id: &'static str,
    path: &'static str,
    category: TracingCallSiteCategory,
    module: &'static str,
    scope: &'static str,
    delivery: TracingDelivery,
    rationale: &'static str,
}

fn call_site_specs() -> [CallSiteSpec; 8] {
    [
        CallSiteSpec {
            id: "api.handle-request",
            path: "crates/bangbang/src/api_server.rs",
            category: TracingCallSiteCategory::Api,
            module: "bangbang::api_server",
            scope: "handle_request_bytes_with_limit",
            delivery: TracingDelivery::BoundedHost,
            rationale: "Bounds one parsed HTTP request across typed dispatch without recording request bytes or values.",
        },
        CallSiteSpec {
            id: "device.virtio-mmio-read",
            path: "crates/runtime/src/virtio_mmio.rs",
            category: TracingCallSiteCategory::Device,
            module: "bangbang_runtime::device::virtio_mmio",
            scope: "read_access",
            delivery: TracingDelivery::NonblockingGuest,
            rationale: "Marks one typed virtio-MMIO read boundary without recording address, register, or guest value.",
        },
        CallSiteSpec {
            id: "device.virtio-mmio-write",
            path: "crates/runtime/src/virtio_mmio.rs",
            category: TracingCallSiteCategory::Device,
            module: "bangbang_runtime::device::virtio_mmio",
            scope: "write_access",
            delivery: TracingDelivery::NonblockingGuest,
            rationale: "Marks one typed virtio-MMIO write boundary without recording address, register, or guest value.",
        },
        CallSiteSpec {
            id: "runtime.handle-action",
            path: "crates/runtime/src/lib.rs",
            category: TracingCallSiteCategory::Vmm,
            module: "bangbang_runtime::controller",
            scope: "handle_action",
            delivery: TracingDelivery::BoundedHost,
            rationale: "Bounds controller action execution while leaving action values and results to their typed owners.",
        },
        CallSiteSpec {
            id: "tool.rebase",
            path: "tools/snapshot-tools/src/lib.rs",
            category: TracingCallSiteCategory::Tool,
            module: "bangbang_snapshot_tools::command",
            scope: "execute_rebase",
            delivery: TracingDelivery::BoundedTool,
            rationale: "Bounds the public rebase command without recording artifact paths, failures, or snapshot contents.",
        },
        CallSiteSpec {
            id: "tool.snapshot-info",
            path: "tools/snapshot-tools/src/lib.rs",
            category: TracingCallSiteCategory::Tool,
            module: "bangbang_snapshot_tools::command",
            scope: "execute_snapshot_info",
            delivery: TracingDelivery::BoundedTool,
            rationale: "Bounds the public inspection command without recording artifact paths, failures, or decoded values.",
        },
        CallSiteSpec {
            id: "tool.snapshot-register-removal",
            path: "tools/snapshot-tools/src/lib.rs",
            category: TracingCallSiteCategory::Tool,
            module: "bangbang_snapshot_tools::command",
            scope: "execute_snapshot_register_removal",
            delivery: TracingDelivery::BoundedTool,
            rationale: "Bounds the public register-removal command without recording artifact paths, registers, or values.",
        },
        CallSiteSpec {
            id: "vmm.handle-action",
            path: "crates/bangbang/src/vmm.rs",
            category: TracingCallSiteCategory::Vmm,
            module: "bangbang::vmm",
            scope: "handle_action",
            delivery: TracingDelivery::BoundedHost,
            rationale: "Bounds process-owned VMM action dispatch without recording action bodies, resources, or results.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_macro_parser_requires_literal_module_and_scope() {
        let parsed =
            syn::parse_str::<TraceMacroArguments>("logger(), \"bangbang::test\", \"operation\",");
        assert!(parsed.is_ok());
        let dynamic =
            syn::parse_str::<TraceMacroArguments>("logger(), dynamic_module, \"operation\"");
        assert!(dynamic.is_err());
    }

    #[test]
    fn tracing_scanner_test_filter_handles_nested_cfg_without_negation() {
        let test: ItemFn = syn::parse_quote! {
            #[cfg(all(feature = "tracing", test))]
            fn helper() {}
        };
        assert!(is_test_only(&test.attrs));

        let production: ItemFn = syn::parse_quote! {
            #[cfg(not(test))]
            fn helper() {}
        };
        assert!(!is_test_only(&production.attrs));

        let mixed: ItemFn = syn::parse_quote! {
            #[cfg(any(feature = "tracing", test))]
            fn helper() {}
        };
        assert!(!is_test_only(&mixed.attrs));
    }

    #[test]
    fn tracing_scanner_rejects_direct_production_entry_and_skips_test_modules() {
        let syntax: syn::File = syn::parse_quote! {
            fn production(logger: &TraceLogger) {
                let _guard = logger.enter_fixed("module", "scope");
            }

            #[cfg(test)]
            mod tests {
                fn test_only(logger: &TraceLogger) {
                    let _guard = logger.enter_fixed("module", "test");
                }
            }
        };
        let mut observed = BTreeSet::new();
        let mut errors = Vec::new();
        TraceMacroVisitor {
            path: "crates/example/src/lib.rs",
            observed: &mut observed,
            errors: &mut errors,
        }
        .visit_file(&syntax);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("direct enter_fixed"));
    }
}
