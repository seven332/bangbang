use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use syn::{Attribute, Item, Meta};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION, FormalVerificationAudit,
    FormalVerificationCategory, FormalVerificationHarness, FormalVerificationNonclaim, Reference,
    ValidationErrors,
};

/// Current targeted formal-verification authority schema.
pub const FORMAL_VERIFICATION_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative targeted formal-verification authority path.
pub const FORMAL_VERIFICATION_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/formal-verification-audit.json";
/// Exact Kani release used by the checked proof set.
pub const KANI_VERSION: &str = "0.67.0";
/// Exact Kani compiler toolchain used by the checked proof set.
pub const KANI_COMPILER_TOOLCHAIN: &str = "nightly-2025-11-21";
/// Exact ordered proof identities owned by #1797.
pub const FORMAL_VERIFICATION_HARNESS_IDS: [&str; 5] = [
    "pager-limits-admission",
    "virtqueue-ranges",
    "token-bucket-refill-accounting",
    "pager-artifact-ranges",
    "virtio-mmio-status-transitions",
];

const KANI_RELEASE_COMMIT: &str = "4feaaad1d6a2378a6ff6caa3b4fc5d6999c7bb5d";
const RUNNER_PATH: &str = "scripts/run-kani.py";
const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";
const PACKAGES: [&str; 2] = ["bangbang-pager", "bangbang-runtime"];
const NONCLAIMS: [FormalVerificationNonclaim; 7] = [
    FormalVerificationNonclaim::UnrestrictedOrWholeSystemCorrectness,
    FormalVerificationNonclaim::FfiOrHvfBehavior,
    FormalVerificationNonclaim::GuestMemoryOrDescriptorTraversal,
    FormalVerificationNonclaim::WallClockOrTimerBehavior,
    FormalVerificationNonclaim::ConcurrencyOrLiveness,
    FormalVerificationNonclaim::FilesystemOrTransportBehavior,
    FormalVerificationNonclaim::PerformanceOrResourceBounds,
];

struct HarnessSpec {
    id: &'static str,
    category: FormalVerificationCategory,
    package: &'static str,
    source: &'static str,
    harness: &'static str,
    owner: &'static str,
}

const HARNESS_SPECS: [HarnessSpec; 5] = [
    HarnessSpec {
        id: "pager-limits-admission",
        category: FormalVerificationCategory::CapabilityInputArithmetic,
        package: "bangbang-pager",
        source: "crates/pager/src/frame.rs",
        harness: "frame::verification::verify_pager_limits_admission",
        owner: "crates/pager/src/frame.rs::PagerLimits::new",
    },
    HarnessSpec {
        id: "virtqueue-ranges",
        category: FormalVerificationCategory::QueueIndexRanges,
        package: "bangbang-runtime",
        source: "crates/runtime/src/virtio_queue.rs",
        harness: "virtio_queue::verification::verify_virtqueue_ranges",
        owner: "crates/runtime/src/virtio_queue.rs::virtqueue geometry and EVENT_IDX",
    },
    HarnessSpec {
        id: "token-bucket-refill-accounting",
        category: FormalVerificationCategory::RateLimitAccounting,
        package: "bangbang-runtime",
        source: "crates/runtime/src/token_bucket.rs",
        harness: "token_bucket::verification::verify_token_bucket_refill_accounting",
        owner: "crates/runtime/src/token_bucket.rs::token_bucket_refill_native",
    },
    HarnessSpec {
        id: "pager-artifact-ranges",
        category: FormalVerificationCategory::ArtifactRangeValidation,
        package: "bangbang-pager",
        source: "crates/pager/src/state.rs",
        harness: "state::verification::verify_pager_artifact_ranges",
        owner: "crates/pager/src/state.rs::pager source range validation",
    },
    HarnessSpec {
        id: "virtio-mmio-status-transitions",
        category: FormalVerificationCategory::StateTransitions,
        package: "bangbang-runtime",
        source: "crates/runtime/src/virtio_mmio.rs",
        harness: "virtio_mmio::verification::verify_virtio_mmio_status_transitions",
        owner: "crates/runtime/src/virtio_mmio.rs::is_valid_status_transition",
    },
];

/// Validate the exact delivery-time Kani authority and source bijection.
pub fn validate_formal_verification_audit(
    audit: &FormalVerificationAudit,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline_and_toolchain(audit, &mut errors);
    validate_harness_records(&audit.harnesses, &mut errors);
    validate_nonclaims(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_references(audit, repository_root, &tracked, &mut errors);
    validate_source_bijection(audit, repository_root, &tracked, &mut errors);
    validate_execution_sources(repository_root, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline_and_toolchain(audit: &FormalVerificationAudit, errors: &mut Vec<String>) {
    if audit.schema_version != FORMAL_VERIFICATION_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "formal verification audit schema_version must be {FORMAL_VERIFICATION_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("formal verification audit baseline is not the pinned release".to_string());
    }
    if audit.delivery_issue != "#1797" {
        errors.push("formal verification audit must be owned by #1797".to_string());
    }

    let toolchain = &audit.toolchain;
    if toolchain.version != KANI_VERSION
        || toolchain.release_tag != "kani-0.67.0"
        || toolchain.release_commit != KANI_RELEASE_COMMIT
        || toolchain.compiler_toolchain != KANI_COMPILER_TOOLCHAIN
        || toolchain.list_format_version != "0.1"
        || toolchain.install_command
            != [
                "cargo",
                "+nightly-2025-11-21",
                "install",
                "--locked",
                "--version",
                KANI_VERSION,
                "kani-verifier",
            ]
        || toolchain.setup_command != ["cargo", "kani", "setup"]
    {
        errors.push("formal verification audit has stale Kani release or setup pins".to_string());
    }

    let execution = &audit.execution;
    if execution.platform != "ubuntu-24.04"
        || execution.runner != RUNNER_PATH
        || execution.command != ["python3", RUNNER_PATH]
        || execution.packages != PACKAGES
        || execution.workflow != WORKFLOW_PATH
        || execution.timeout_minutes != 45
        || !execution.sequential
    {
        errors.push("formal verification audit has stale execution policy".to_string());
    }
}

fn validate_harness_records(harnesses: &[FormalVerificationHarness], errors: &mut Vec<String>) {
    let ids = harnesses
        .iter()
        .map(|harness| harness.id.as_str())
        .collect::<Vec<_>>();
    if ids != FORMAL_VERIFICATION_HARNESS_IDS {
        errors.push(format!(
            "formal verification audit requires the exact ordered harness set: expected {FORMAL_VERIFICATION_HARNESS_IDS:?}, found {ids:?}"
        ));
        return;
    }

    let categories = harnesses
        .iter()
        .map(|harness| harness.category)
        .collect::<BTreeSet<_>>();
    if categories.len() != HARNESS_SPECS.len() {
        errors.push(
            "formal verification audit requires one harness per closed risk category".to_string(),
        );
    }

    for (harness, spec) in harnesses.iter().zip(HARNESS_SPECS.iter()) {
        if harness.id != spec.id
            || harness.category != spec.category
            || harness.package != spec.package
            || harness.source != spec.source
            || harness.harness != spec.harness
            || harness.owner != spec.owner
            || harness.command != canonical_harness_command(spec.id, spec.package, spec.harness)
        {
            errors.push(format!(
                "formal verification harness has stale identity, owner, or command: {}",
                harness.id
            ));
        }
        if harness.assumptions.is_empty()
            || harness.bounds.is_empty()
            || harness.invariant.trim().is_empty()
            || harness.implementation.is_empty()
            || harness.validation.is_empty()
        {
            errors.push(format!(
                "formal verification harness requires assumptions, bounds, invariant, owner evidence, and validation: {}",
                harness.id
            ));
        }
        if has_empty_or_duplicate_strings(&harness.assumptions)
            || has_empty_or_duplicate_strings(&harness.bounds)
        {
            errors.push(format!(
                "formal verification harness has empty or duplicate assumptions/bounds: {}",
                harness.id
            ));
        }
    }
}

fn canonical_harness_command(id: &str, package: &str, harness: &str) -> Vec<String> {
    let mut command = [
        "cargo",
        "kani",
        "--package",
        package,
        "--lib",
        "--harness",
        harness,
        "--exact",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if id == "token-bucket-refill-accounting" {
        command.extend(["--solver".to_string(), "kissat".to_string()]);
    }
    command
}

fn has_empty_or_duplicate_strings(values: &[String]) -> bool {
    let mut unique = BTreeSet::new();
    values
        .iter()
        .any(|value| value.trim().is_empty() || !unique.insert(value.as_str()))
}

fn validate_nonclaims(audit: &FormalVerificationAudit, errors: &mut Vec<String>) {
    if audit.nonclaims != NONCLAIMS {
        errors.push("formal verification audit requires the exact ordered nonclaims".to_string());
    }
}

fn validate_references(
    audit: &FormalVerificationAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    for harness in &audit.harnesses {
        for (kind, references) in [
            ("implementation", harness.implementation.as_slice()),
            ("validation", harness.validation.as_slice()),
        ] {
            let label = format!("formal verification {} {kind}", harness.id);
            validate_reference_collection(references, &label, errors);
            for (index, reference) in references.iter().enumerate() {
                validate_reference(
                    reference,
                    repository_root,
                    tracked,
                    &format!("{label}[{index}]"),
                    errors,
                );
                validate_local_reference_anchor(
                    reference,
                    repository_root,
                    &format!("{label}[{index}]"),
                    errors,
                );
            }
        }
    }
    for (kind, references) in [
        ("implementation", audit.evidence.implementation.as_slice()),
        ("validation", audit.evidence.validation.as_slice()),
        ("documentation", audit.evidence.documentation.as_slice()),
    ] {
        if references.is_empty() {
            errors.push(format!(
                "formal verification audit requires shared {kind} evidence"
            ));
        }
        let label = format!("formal verification evidence {kind}");
        validate_reference_collection(references, &label, errors);
        for (index, reference) in references.iter().enumerate() {
            validate_reference(
                reference,
                repository_root,
                tracked,
                &format!("{label}[{index}]"),
                errors,
            );
            validate_local_reference_anchor(
                reference,
                repository_root,
                &format!("{label}[{index}]"),
                errors,
            );
        }
    }
}

fn validate_reference_collection(references: &[Reference], label: &str, errors: &mut Vec<String>) {
    if references
        .windows(2)
        .any(|pair| matches!(pair, [previous, current] if previous >= current))
    {
        errors.push(format!(
            "formal verification references must be unique and canonically sorted: {label}"
        ));
    }
    for reference in references {
        if !matches!(
            reference,
            Reference::Local {
                anchor: Some(_),
                ..
            }
        ) {
            errors.push(format!(
                "formal verification evidence must be an anchored local reference: {label}"
            ));
        }
    }
}

fn validate_local_reference_anchor(
    reference: &Reference,
    repository_root: &Path,
    label: &str,
    errors: &mut Vec<String>,
) {
    let Reference::Local {
        path,
        anchor: Some(anchor),
    } = reference
    else {
        return;
    };
    let relative = Path::new(path);
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(repository_root.join(relative)) else {
        return;
    };
    if !contents.contains(anchor) {
        errors.push(format!(
            "local reference anchor is absent from its file: {label}"
        ));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DerivedHarness {
    package: String,
    source: String,
    harness: String,
}

fn validate_source_bijection(
    audit: &FormalVerificationAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let derived = derive_source_harnesses(repository_root, tracked, errors);
    let expected = audit
        .harnesses
        .iter()
        .map(|harness| DerivedHarness {
            package: harness.package.clone(),
            source: harness.source.clone(),
            harness: harness.harness.clone(),
        })
        .collect::<BTreeSet<_>>();
    if derived != expected {
        errors.push(format!(
            "formal verification manifest/source harness bijection differs: expected {expected:?}, derived {derived:?}"
        ));
    }
}

fn derive_source_harnesses(
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) -> BTreeSet<DerivedHarness> {
    let mut harnesses = BTreeSet::new();
    for path in tracked.iter().filter(|path| is_workspace_rust_source(path)) {
        let full_path = repository_root.join(path);
        let source = match std::fs::read_to_string(&full_path) {
            Ok(source) => source,
            Err(_) => {
                errors.push(format!(
                    "failed to read tracked Rust source while deriving Kani harnesses: {}",
                    path.display()
                ));
                continue;
            }
        };
        if !source.contains("kani::proof") {
            continue;
        }
        let syntax = match syn::parse_file(&source) {
            Ok(syntax) => syntax,
            Err(_) => {
                errors.push(format!(
                    "failed to parse tracked Rust source while deriving Kani harnesses: {}",
                    path.display()
                ));
                continue;
            }
        };
        let Some(mut modules) = source_module_path(path) else {
            errors.push(format!(
                "Kani proof source does not have a derivable module path: {}",
                path.display()
            ));
            continue;
        };
        let mut file_harnesses = Vec::new();
        collect_proof_items(
            &syntax.items,
            &mut modules,
            false,
            &mut file_harnesses,
            errors,
            path,
        );
        if file_harnesses.is_empty() {
            continue;
        }
        let Some(package) = package_for_source(path) else {
            errors.push(format!(
                "Kani proof exists outside the closed pager/runtime package set: {}",
                path.display()
            ));
            continue;
        };
        for harness in file_harnesses {
            let derived = DerivedHarness {
                package: package.to_string(),
                source: path.to_string_lossy().into_owned(),
                harness,
            };
            if !harnesses.insert(derived.clone()) {
                errors.push(format!(
                    "duplicate derived Kani proof identity: {} {}",
                    derived.source, derived.harness
                ));
            }
        }
    }
    harnesses
}

fn is_workspace_rust_source(path: &Path) -> bool {
    let components = path.components().collect::<Vec<_>>();
    matches!(components.first(), Some(Component::Normal(root)) if *root == "crates" || *root == "tools")
        && components
            .iter()
            .any(|component| matches!(component, Component::Normal(name) if *name == "src"))
        && path.extension().is_some_and(|extension| extension == "rs")
}

fn package_for_source(path: &Path) -> Option<&'static str> {
    let value = path.to_string_lossy();
    if value.starts_with("crates/pager/src/") {
        Some("bangbang-pager")
    } else if value.starts_with("crates/runtime/src/") {
        Some("bangbang-runtime")
    } else {
        None
    }
}

fn source_module_path(path: &Path) -> Option<Vec<String>> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_string),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    let src_index = components.iter().position(|component| component == "src")?;
    let mut modules = components.get(src_index + 1..)?.to_vec();
    let filename = modules.pop()?;
    match filename.as_str() {
        "lib.rs" | "main.rs" | "mod.rs" => {}
        _ => modules.push(filename.strip_suffix(".rs")?.to_string()),
    }
    Some(modules)
}

fn collect_proof_items(
    items: &[Item],
    modules: &mut Vec<String>,
    kani_guarded: bool,
    harnesses: &mut Vec<String>,
    errors: &mut Vec<String>,
    source_path: &Path,
) {
    for item in items {
        match item {
            Item::Mod(item_mod) => {
                let guarded = kani_guarded || attrs_have_exact_kani_cfg(&item_mod.attrs);
                let Some((_, nested)) = &item_mod.content else {
                    continue;
                };
                modules.push(item_mod.ident.to_string());
                collect_proof_items(nested, modules, guarded, harnesses, errors, source_path);
                modules.pop();
            }
            Item::Fn(item_fn) if attrs_have_kani_proof(&item_fn.attrs) => {
                if !(kani_guarded || attrs_have_exact_kani_cfg(&item_fn.attrs)) {
                    errors.push(format!(
                        "Kani proof is not guarded by exact cfg(kani): {}::{}",
                        source_path.display(),
                        item_fn.sig.ident
                    ));
                }
                let mut symbol = modules.clone();
                symbol.push(item_fn.sig.ident.to_string());
                harnesses.push(symbol.join("::"));
            }
            _ => {}
        }
    }
}

fn attrs_have_kani_proof(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        let segments = attribute
            .path()
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        segments == ["kani", "proof"]
    })
}

fn attrs_have_exact_kani_cfg(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Meta>()
                .is_ok_and(|meta| matches!(meta, Meta::Path(path) if path.is_ident("kani")))
    })
}

fn validate_execution_sources(repository_root: &Path, errors: &mut Vec<String>) {
    validate_required_tokens(
        repository_root,
        RUNNER_PATH,
        &[
            "cargo kani --version",
            "--formal-verification-final",
            "cargo",
            "kani",
            "list",
            "--format",
            "json",
            "--exact",
            "--solver",
            "kissat",
            "TemporaryDirectory",
        ],
        errors,
    );
    validate_required_tokens(
        repository_root,
        WORKFLOW_PATH,
        &[
            "kani:",
            "runs-on: ubuntu-24.04",
            "timeout-minutes: 45",
            "nightly-2025-11-21",
            "kani-verifier",
            "0.67.0",
            "cargo kani setup",
            "python3 scripts/run-kani.py",
        ],
        errors,
    );
}

fn validate_required_tokens(
    repository_root: &Path,
    path: &str,
    tokens: &[&str],
    errors: &mut Vec<String>,
) {
    let source = match std::fs::read_to_string(repository_root.join(path)) {
        Ok(source) => source,
        Err(_) => {
            errors.push(format!(
                "formal verification execution source is unreadable: {path}"
            ));
            return;
        }
    };
    for token in tokens {
        if !source.contains(token) {
            errors.push(format!(
                "formal verification execution source {path} omits required token: {token}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_module_paths_are_stable() {
        assert_eq!(
            source_module_path(Path::new("crates/runtime/src/virtio_queue.rs")),
            Some(vec!["virtio_queue".to_string()])
        );
        assert_eq!(
            source_module_path(Path::new("crates/runtime/src/block/mod.rs")),
            Some(vec!["block".to_string()])
        );
        assert_eq!(
            source_module_path(Path::new("crates/pager/src/lib.rs")),
            Some(Vec::new())
        );
    }

    #[test]
    fn proof_derivation_requires_exact_kani_guard() {
        let guarded = syn::parse_file(
            r#"
                #[cfg(kani)]
                mod verification {
                    #[kani::proof]
                    fn verify_range() {}
                }
            "#,
        )
        .expect("guarded source should parse");
        let mut modules = vec!["queue".to_string()];
        let mut harnesses = Vec::new();
        let mut errors = Vec::new();
        collect_proof_items(
            &guarded.items,
            &mut modules,
            false,
            &mut harnesses,
            &mut errors,
            Path::new("queue.rs"),
        );
        assert_eq!(harnesses, ["queue::verification::verify_range".to_string()]);
        assert!(errors.is_empty());

        let unguarded = syn::parse_file("#[kani::proof] fn verify_unbounded() {}")
            .expect("unguarded source should parse");
        let mut harnesses = Vec::new();
        collect_proof_items(
            &unguarded.items,
            &mut Vec::new(),
            false,
            &mut harnesses,
            &mut errors,
            Path::new("lib.rs"),
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("not guarded by exact cfg(kani)"))
        );
    }

    #[test]
    fn proof_text_inside_a_string_is_not_a_harness() {
        let syntax =
            syn::parse_file(r##"const FIXTURE: &str = "#[kani::proof] fn verify_fixture() {}";"##)
                .expect("string fixture source should parse");
        let mut harnesses = Vec::new();
        let mut errors = Vec::new();
        collect_proof_items(
            &syntax.items,
            &mut Vec::new(),
            false,
            &mut harnesses,
            &mut errors,
            Path::new("fixture.rs"),
        );
        assert!(harnesses.is_empty());
        assert!(errors.is_empty());
    }
}
