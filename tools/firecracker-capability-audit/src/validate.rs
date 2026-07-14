use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

use crate::{
    AuditMode, Baseline, Capability, CapabilityInventory, Counts, Disposition, FIRECRACKER_COMMIT,
    FIRECRACKER_TARGET, FIRECRACKER_VERSION, GENERATOR_VERSION, Input, PlatformExclusion,
    Reference, SCHEMA_VERSION, SourceItem, SourceManifest,
};

const EXPECTED_SWAGGER_PATHS: usize = 26;
const EXPECTED_SWAGGER_OPERATIONS: usize = 38;
const EXPECTED_SWAGGER_DEFINITIONS: usize = 44;
const EXPECTED_SWAGGER_PROPERTIES: usize = 152;
const EXPECTED_FIRECRACKER_ARGUMENTS: usize = 23;
const EXPECTED_NON_SWAGGER_ROUTES: usize = 3;
const EXPECTED_PUBLIC_TOOL_OPERATIONS: usize = 14;
const EXPECTED_PUBLIC_TOOL_ARGUMENTS: usize = 41;
const EXPECTED_CORPUS_ITEMS: usize = 40;
const SOURCE_KINDS: &[&str] = &[
    "api-operation",
    "api-path",
    "api-property",
    "api-schema",
    "corpus",
    "firecracker-argument",
    "non-swagger-route",
    "tool-argument",
    "tool-operation",
];
const EXTRACTORS: &[&str] = &[
    "curated-source-v1",
    "firecracker-cli-v1",
    "parsed-delete-routes-v1",
    "swagger-v1",
];

/// Complete deterministic set of inventory validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(Vec<String>);

impl ValidationErrors {
    /// Individual validation failures in deterministic order.
    pub fn messages(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for message in &self.0 {
            writeln!(formatter, "- {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Validate a generated manifest and its human capability overlay.
pub fn validate(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
    mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    validate_baseline(manifest, inventory, &mut errors);
    validate_inputs(&manifest.inputs, &mut errors);
    validate_source_items(manifest, &mut errors);
    validate_families(&inventory.families, &mut errors);
    validate_capabilities(manifest, inventory, repository_root, mode, &mut errors);

    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errors))
    }
}

fn validate_baseline(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if manifest.schema_version != SCHEMA_VERSION {
        errors.push(format!(
            "source manifest schema_version must be {SCHEMA_VERSION}, found {}",
            manifest.schema_version
        ));
    }
    if inventory.schema_version != SCHEMA_VERSION {
        errors.push(format!(
            "capability inventory schema_version must be {SCHEMA_VERSION}, found {}",
            inventory.schema_version
        ));
    }
    if manifest.baseline != inventory.baseline {
        errors.push("source manifest and capability inventory baselines differ".to_string());
    }
    validate_expected_baseline("source manifest", &manifest.baseline, errors);
    validate_expected_baseline("capability inventory", &inventory.baseline, errors);
    if manifest.generator_version != GENERATOR_VERSION {
        errors.push(format!(
            "source manifest generator_version must be {GENERATOR_VERSION}, found {}",
            manifest.generator_version
        ));
    }
}

fn validate_expected_baseline(label: &str, baseline: &Baseline, errors: &mut Vec<String>) {
    if baseline.version != FIRECRACKER_VERSION {
        errors.push(format!(
            "{label} version must be {FIRECRACKER_VERSION}, found {}",
            baseline.version
        ));
    }
    if baseline.commit != FIRECRACKER_COMMIT {
        errors.push(format!(
            "{label} commit must be {FIRECRACKER_COMMIT}, found {}",
            baseline.commit
        ));
    }
    if baseline.target != FIRECRACKER_TARGET {
        errors.push(format!(
            "{label} target must be {FIRECRACKER_TARGET}, found {}",
            baseline.target
        ));
    }
}

fn validate_inputs(inputs: &[Input], errors: &mut Vec<String>) {
    check_sorted_unique(
        inputs.iter().map(|input| input.path.as_str()),
        "input path",
        errors,
    );
    for input in inputs {
        if !is_safe_relative_path(Path::new(&input.path)) {
            errors.push(format!(
                "input path must be repository-relative: {}",
                input.path
            ));
        }
        if input.git_blob.len() < 40 || !input.git_blob.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            errors.push(format!(
                "input git_blob is not a Git object id: {}",
                input.path
            ));
        }
        if !EXTRACTORS.contains(&input.extractor.as_str()) {
            errors.push(format!(
                "input extractor is not recognized: {} -> {}",
                input.path, input.extractor
            ));
        }
    }
}

fn validate_source_items(manifest: &SourceManifest, errors: &mut Vec<String>) {
    check_sorted_unique(
        manifest.items.iter().map(|item| item.id.as_str()),
        "source item id",
        errors,
    );

    let input_paths: BTreeSet<&str> = manifest
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect();
    let mut source_keys = BTreeSet::new();
    for item in &manifest.items {
        if item.id != canonical_source_id(item) {
            errors.push(format!("source item id is not canonical: {}", item.id));
        }
        if item.kind.trim().is_empty() || item.key.trim().is_empty() {
            errors.push(format!(
                "source item kind/key must not be empty: {}",
                item.id
            ));
        }
        if !SOURCE_KINDS.contains(&item.kind.as_str()) {
            errors.push(format!(
                "source item kind is not recognized: {} -> {}",
                item.id, item.kind
            ));
        }
        if item.family.trim().is_empty() {
            errors.push(format!("source item family must not be empty: {}", item.id));
        }
        if !input_paths.contains(item.path.as_str()) {
            errors.push(format!(
                "source item references undeclared input {}: {}",
                item.path, item.id
            ));
        }
        if item.anchor.trim().is_empty() {
            errors.push(format!("source item anchor must not be empty: {}", item.id));
        }
        if !source_keys.insert((item.kind.as_str(), item.key.as_str())) {
            errors.push(format!(
                "duplicate source kind/key: {}:{}",
                item.kind, item.key
            ));
        }
    }

    let actual = computed_counts(&manifest.items);
    if actual != manifest.counts {
        errors.push(format!(
            "declared source counts do not match exact item kinds: declared {:?}, actual {:?}",
            manifest.counts, actual
        ));
    }
    validate_expected_counts(&manifest.counts, errors);
}

fn canonical_source_id(item: &SourceItem) -> String {
    format!("{}:{}", item.kind, item.key)
}

fn computed_counts(items: &[SourceItem]) -> Counts {
    Counts {
        swagger_paths: count_kind(items, "api-path"),
        swagger_operations: count_kind(items, "api-operation"),
        swagger_definitions: count_kind(items, "api-schema"),
        swagger_properties: count_kind(items, "api-property"),
        firecracker_arguments: count_kind(items, "firecracker-argument"),
        non_swagger_routes: count_kind(items, "non-swagger-route"),
        public_tool_operations: count_kind(items, "tool-operation"),
        public_tool_arguments: count_kind(items, "tool-argument"),
        corpus_items: count_kind(items, "corpus"),
    }
}

fn count_kind(items: &[SourceItem], kind: &str) -> usize {
    items.iter().filter(|item| item.kind == kind).count()
}

fn validate_expected_counts(counts: &Counts, errors: &mut Vec<String>) {
    let expected = [
        (
            "Swagger paths",
            EXPECTED_SWAGGER_PATHS,
            counts.swagger_paths,
        ),
        (
            "Swagger operations",
            EXPECTED_SWAGGER_OPERATIONS,
            counts.swagger_operations,
        ),
        (
            "Swagger definitions",
            EXPECTED_SWAGGER_DEFINITIONS,
            counts.swagger_definitions,
        ),
        (
            "Swagger properties",
            EXPECTED_SWAGGER_PROPERTIES,
            counts.swagger_properties,
        ),
        (
            "Firecracker arguments",
            EXPECTED_FIRECRACKER_ARGUMENTS,
            counts.firecracker_arguments,
        ),
        (
            "non-Swagger routes",
            EXPECTED_NON_SWAGGER_ROUTES,
            counts.non_swagger_routes,
        ),
        (
            "public tool operations",
            EXPECTED_PUBLIC_TOOL_OPERATIONS,
            counts.public_tool_operations,
        ),
        (
            "public tool arguments",
            EXPECTED_PUBLIC_TOOL_ARGUMENTS,
            counts.public_tool_arguments,
        ),
        (
            "source corpus items",
            EXPECTED_CORPUS_ITEMS,
            counts.corpus_items,
        ),
    ];
    for (label, wanted, actual) in expected {
        if actual != wanted {
            errors.push(format!(
                "{label} must contain {wanted} identities, found {actual}"
            ));
        }
    }
}

fn validate_families(families: &[String], errors: &mut Vec<String>) {
    check_sorted_unique(families.iter().map(String::as_str), "family", errors);
    for family in families {
        if !is_slug(family) {
            errors.push(format!("family is not a lowercase slug: {family}"));
        }
    }
}

fn validate_capabilities(
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    check_sorted_unique(
        inventory
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str()),
        "capability id",
        errors,
    );

    let source_by_id: BTreeMap<&str, &SourceItem> = manifest
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let capability_by_id: BTreeMap<&str, &Capability> = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect();
    let families: BTreeSet<&str> = inventory.families.iter().map(String::as_str).collect();

    for item in &manifest.items {
        match capability_by_id.get(item.id.as_str()) {
            Some(capability) => {
                if capability.family != item.family {
                    errors.push(format!(
                        "generated overlay family differs from source item: {}",
                        item.id
                    ));
                }
                if !capability
                    .source_refs
                    .iter()
                    .any(|source| source == &item.id)
                {
                    errors.push(format!(
                        "generated overlay does not reference its own source identity: {}",
                        item.id
                    ));
                }
            }
            None => errors.push(format!(
                "source item has no capability overlay: {}",
                item.id
            )),
        }
    }

    for capability in &inventory.capabilities {
        if !source_by_id.contains_key(capability.id.as_str()) {
            if !capability.id.starts_with("semantic.") {
                errors.push(format!(
                    "non-generated capability id must start with semantic.: {}",
                    capability.id
                ));
            } else if !is_semantic_id(&capability.id) {
                errors.push(format!(
                    "semantic capability id is not canonical: {}",
                    capability.id
                ));
            }
        }
        if !families.contains(capability.family.as_str()) {
            errors.push(format!(
                "capability references undeclared family {}: {}",
                capability.family, capability.id
            ));
        }
        if capability.summary.trim().is_empty() {
            errors.push(format!(
                "capability summary must not be empty: {}",
                capability.id
            ));
        }
        if capability.source_refs.is_empty() {
            errors.push(format!(
                "capability source_refs must not be empty: {}",
                capability.id
            ));
        }
        check_sorted_unique(
            capability.source_refs.iter().map(String::as_str),
            &format!("source reference for {}", capability.id),
            errors,
        );
        for source_ref in &capability.source_refs {
            if !source_by_id.contains_key(source_ref.as_str()) {
                errors.push(format!(
                    "capability source reference does not resolve: {} -> {source_ref}",
                    capability.id
                ));
            }
        }
        validate_disposition(capability, repository_root, mode, errors);
    }
}

fn validate_disposition(
    capability: &Capability,
    repository_root: &Path,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    match capability.disposition {
        Disposition::AuditRequired => {
            if mode == AuditMode::Final {
                errors.push(format!(
                    "final validation forbids audit-required: {}",
                    capability.id
                ));
            }
            forbid_delivery_and_exclusion(capability, errors);
        }
        Disposition::MissingPlatformFeasible => {
            if mode == AuditMode::Final {
                errors.push(format!(
                    "final validation forbids missing-platform-feasible: {}",
                    capability.id
                ));
            }
            if capability
                .delivery_issue
                .as_deref()
                .is_none_or(|issue| !is_delivery_issue(issue))
            {
                errors.push(format!(
                    "missing-platform-feasible requires a delivery issue: {}",
                    capability.id
                ));
            }
            if capability.exclusion.is_some() {
                errors.push(format!(
                    "missing-platform-feasible forbids exclusion evidence: {}",
                    capability.id
                ));
            }
        }
        Disposition::ImplementedAndVerified => {
            if capability.implementation.is_empty() {
                errors.push(format!(
                    "implemented-and-verified requires implementation references: {}",
                    capability.id
                ));
            }
            if capability.validation.is_empty() {
                errors.push(format!(
                    "implemented-and-verified requires validation references: {}",
                    capability.id
                ));
            }
            forbid_delivery_and_exclusion(capability, errors);
        }
        Disposition::ProvenPlatformImpossible => {
            if capability.delivery_issue.is_some() {
                errors.push(format!(
                    "proven-platform-impossible forbids a delivery issue: {}",
                    capability.id
                ));
            }
            match &capability.exclusion {
                Some(exclusion) => {
                    validate_exclusion(&capability.id, exclusion, repository_root, errors)
                }
                None => errors.push(format!(
                    "proven-platform-impossible requires exclusion evidence: {}",
                    capability.id
                )),
            }
        }
    }

    for (index, reference) in capability.implementation.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            &format!("{} implementation[{index}]", capability.id),
            errors,
        );
    }
    check_sorted_unique_references(
        &capability.implementation,
        &format!("{} implementation reference", capability.id),
        errors,
    );
    for (index, reference) in capability.validation.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            &format!("{} validation[{index}]", capability.id),
            errors,
        );
    }
    check_sorted_unique_references(
        &capability.validation,
        &format!("{} validation reference", capability.id),
        errors,
    );
}

fn forbid_delivery_and_exclusion(capability: &Capability, errors: &mut Vec<String>) {
    if capability.delivery_issue.is_some() {
        errors.push(format!(
            "{} forbids a delivery issue in its current disposition",
            capability.id
        ));
    }
    if capability.exclusion.is_some() {
        errors.push(format!(
            "{} forbids exclusion evidence in its current disposition",
            capability.id
        ));
    }
}

fn validate_exclusion(
    id: &str,
    exclusion: &PlatformExclusion,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let groups = [
        ("upstream_contract", &exclusion.upstream_contract),
        ("platform_evidence", &exclusion.platform_evidence),
        ("stable_behavior", &exclusion.stable_behavior),
        ("focused_tests", &exclusion.focused_tests),
        ("compatibility_docs", &exclusion.compatibility_docs),
        ("security_docs", &exclusion.security_docs),
    ];
    for (name, references) in groups {
        if references.is_empty() {
            errors.push(format!("platform exclusion {name} must not be empty: {id}"));
        }
        check_sorted_unique_references(
            references,
            &format!("{id} exclusion.{name} reference"),
            errors,
        );
        for (index, reference) in references.iter().enumerate() {
            validate_reference(
                reference,
                repository_root,
                &format!("{id} exclusion.{name}[{index}]"),
                errors,
            );
        }
    }
    if exclusion.alternatives.is_empty()
        || exclusion
            .alternatives
            .iter()
            .any(|alternative| alternative.trim().is_empty())
    {
        errors.push(format!(
            "platform exclusion alternatives must contain reviewed reasons: {id}"
        ));
    }
    check_sorted_unique(
        exclusion.alternatives.iter().map(String::as_str),
        &format!("{id} exclusion alternative"),
        errors,
    );
    validate_reference(
        &exclusion.challenge,
        repository_root,
        &format!("{id} exclusion.challenge"),
        errors,
    );
    match &exclusion.challenge {
        Reference::Github { url } if !is_github_challenge_url(url) => errors.push(format!(
            "platform exclusion challenge must link a GitHub issue Challenge comment: {id}"
        )),
        Reference::Github { .. } => {}
        _ => errors.push(format!(
            "platform exclusion challenge must be a GitHub reference: {id}"
        )),
    }
}

fn validate_reference(
    reference: &Reference,
    repository_root: &Path,
    label: &str,
    errors: &mut Vec<String>,
) {
    match reference {
        Reference::Local { path, anchor } => {
            let relative = Path::new(path);
            if !is_safe_relative_path(relative) {
                errors.push(format!("local reference path escapes repository: {label}"));
                return;
            }
            if anchor
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                errors.push(format!("local reference anchor must not be empty: {label}"));
            }
            let joined = repository_root.join(relative);
            let metadata = match std::fs::symlink_metadata(&joined) {
                Ok(metadata) => metadata,
                Err(_) => {
                    errors.push(format!("local reference path does not exist: {label}"));
                    return;
                }
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                errors.push(format!("local reference must name a regular file: {label}"));
                return;
            }
            let canonical_root = match repository_root.canonicalize() {
                Ok(path) => path,
                Err(_) => {
                    errors.push(format!("repository root is not accessible: {label}"));
                    return;
                }
            };
            let canonical = match joined.canonicalize() {
                Ok(path) => path,
                Err(_) => {
                    errors.push(format!("local reference path is not accessible: {label}"));
                    return;
                }
            };
            if !canonical.starts_with(&canonical_root) {
                errors.push(format!("local reference path escapes repository: {label}"));
            }
        }
        Reference::Github { url } => {
            if !is_github_reference_url(url) {
                errors.push(format!(
                    "GitHub reference must name an HTTPS repository path: {label}"
                ));
            }
        }
        Reference::Authoritative { url } => {
            if !is_https_reference_url(url) {
                errors.push(format!(
                    "authoritative reference must name an HTTPS host: {label}"
                ));
            }
        }
    }
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn is_semantic_id(value: &str) -> bool {
    value
        .strip_prefix("semantic.")
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(namespace, name)| is_slug(namespace) && is_slug(name))
}

fn is_delivery_issue(value: &str) -> bool {
    value.strip_prefix('#').is_some_and(is_positive_decimal) || is_github_issue_url(value)
}

fn is_github_reference_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    if rest.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let mut segments = path.split('/');
    segments.next().is_some_and(|owner| !owner.is_empty())
        && segments
            .next()
            .is_some_and(|repository| !repository.is_empty())
}

fn is_github_issue_url(url: &str) -> bool {
    if !is_github_reference_url(url) {
        return false;
    }
    let path = url
        .strip_prefix("https://github.com/")
        .unwrap_or_default()
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let mut segments = path.split('/');
    matches!(
        (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next()
        ),
        (Some(_), Some(_), Some("issues"), Some(number), None) if is_positive_decimal(number)
    )
}

fn is_github_challenge_url(url: &str) -> bool {
    is_github_issue_url(url)
        && url
            .split_once("#issuecomment-")
            .is_some_and(|(_, comment)| is_positive_decimal(comment))
}

fn is_https_reference_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if rest.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    rest.split(['/', '?', '#'])
        .next()
        .is_some_and(|host| host.contains('.') && !host.starts_with('.') && !host.ends_with('.'))
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte != b'0')
}

fn check_sorted_unique<'a>(
    values: impl Iterator<Item = &'a str>,
    label: &str,
    errors: &mut Vec<String>,
) {
    let mut previous: Option<&str> = None;
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            errors.push(format!("duplicate {label}: {value}"));
        }
        if let Some(old) = previous
            && old > value
        {
            errors.push(format!(
                "{label} values are not canonically sorted: {old} > {value}"
            ));
        }
        previous = Some(value);
    }
}

fn check_sorted_unique_references(references: &[Reference], label: &str, errors: &mut Vec<String>) {
    for pair in references.windows(2) {
        let [previous, current] = pair else {
            continue;
        };
        if previous == current {
            errors.push(format!("duplicate {label}: {current:?}"));
        } else if previous > current {
            errors.push(format!(
                "{label} values are not canonically sorted: {previous:?} > {current:?}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-capability-validation-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("temporary directory should be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn baseline() -> Baseline {
        Baseline {
            version: FIRECRACKER_VERSION.to_string(),
            commit: FIRECRACKER_COMMIT.to_string(),
            target: FIRECRACKER_TARGET.to_string(),
        }
    }

    fn add_items(items: &mut Vec<SourceItem>, kind: &str, count: usize, family: &str) {
        for index in 0..count {
            let key = format!("{index:03}");
            items.push(SourceItem {
                id: format!("{kind}:{key}"),
                kind: kind.to_string(),
                key,
                path: "upstream/source".to_string(),
                anchor: format!("item-{index}"),
                family: family.to_string(),
            });
        }
    }

    fn valid_fixture() -> (SourceManifest, CapabilityInventory) {
        let mut items = Vec::new();
        add_items(&mut items, "api-path", 26, "api-contract");
        add_items(&mut items, "api-operation", 38, "api-contract");
        add_items(&mut items, "api-schema", 44, "api-contract");
        add_items(&mut items, "api-property", 152, "api-contract");
        add_items(&mut items, "firecracker-argument", 23, "process");
        add_items(
            &mut items,
            "non-swagger-route",
            3,
            "runtime-device-management",
        );
        add_items(
            &mut items,
            "tool-operation",
            EXPECTED_PUBLIC_TOOL_OPERATIONS,
            "public-tools",
        );
        add_items(
            &mut items,
            "tool-argument",
            EXPECTED_PUBLIC_TOOL_ARGUMENTS,
            "public-tools",
        );
        add_items(
            &mut items,
            "corpus",
            EXPECTED_CORPUS_ITEMS,
            "specifications",
        );
        items.sort_by(|left, right| left.id.cmp(&right.id));

        let counts = computed_counts(&items);
        let capabilities = items
            .iter()
            .map(|item| Capability {
                id: item.id.clone(),
                family: item.family.clone(),
                summary: format!("Audit {}.", item.key),
                source_refs: vec![item.id.clone()],
                disposition: Disposition::AuditRequired,
                implementation: Vec::new(),
                validation: Vec::new(),
                delivery_issue: None,
                exclusion: None,
            })
            .collect();
        let manifest = SourceManifest {
            schema_version: SCHEMA_VERSION,
            baseline: baseline(),
            generator_version: GENERATOR_VERSION,
            inputs: vec![Input {
                path: "upstream/source".to_string(),
                git_blob: "0123456789012345678901234567890123456789".to_string(),
                extractor: "curated-source-v1".to_string(),
            }],
            counts,
            items,
        };
        let inventory = CapabilityInventory {
            schema_version: SCHEMA_VERSION,
            baseline: baseline(),
            families: vec![
                "api-contract".to_string(),
                "process".to_string(),
                "public-tools".to_string(),
                "runtime-device-management".to_string(),
                "specifications".to_string(),
            ],
            capabilities,
        };
        (manifest, inventory)
    }

    #[test]
    fn accepts_delivery_inventory() {
        let (manifest, inventory) = valid_fixture();
        validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect("fixture should validate");
    }

    #[test]
    fn validates_external_reference_and_issue_url_shapes() {
        assert!(is_github_reference_url(
            "https://github.com/seven332/bangbang"
        ));
        assert!(!is_github_reference_url("https://github.com/"));
        assert!(!is_github_reference_url(
            "https://github.com/seven332/bang bang"
        ));
        assert!(is_github_issue_url(
            "https://github.com/seven332/bangbang/issues/1349"
        ));
        assert!(!is_github_issue_url(
            "https://github.com/seven332/bangbang/pull/1350"
        ));
        assert!(is_github_challenge_url(
            "https://github.com/seven332/bangbang/issues/1349#issuecomment-4971005774"
        ));
        assert!(!is_github_challenge_url(
            "https://github.com/seven332/bangbang/issues/1349"
        ));
        assert!(is_https_reference_url(
            "https://developer.apple.com/documentation/hypervisor"
        ));
        assert!(!is_https_reference_url("https://"));
        assert!(is_delivery_issue("#1349"));
        assert!(!is_delivery_issue("#0"));
    }

    #[test]
    fn final_mode_rejects_audit_required() {
        let (manifest, inventory) = valid_fixture();
        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Final)
            .expect_err("audit-required should fail final mode");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("final validation forbids audit-required"))
        );
    }

    #[test]
    fn rejects_replaced_identity_even_when_count_is_unchanged() {
        let (mut manifest, inventory) = valid_fixture();
        let item = manifest
            .items
            .iter_mut()
            .find(|item| item.kind == "api-path")
            .expect("path item should exist");
        item.key = "replacement".to_string();
        item.id = "api-path:replacement".to_string();
        manifest.items.sort_by(|left, right| left.id.cmp(&right.id));

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("replacement should break exact overlay join");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("has no capability overlay"))
        );
    }

    #[test]
    fn rejects_missing_implemented_evidence() {
        let (manifest, mut inventory) = valid_fixture();
        let capability = inventory
            .capabilities
            .first_mut()
            .expect("capability should exist");
        capability.disposition = Disposition::ImplementedAndVerified;

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("missing evidence should fail");
        assert!(errors.messages().iter().any(|message| {
            message.contains("implemented-and-verified requires implementation references")
        }));
        assert!(errors.messages().iter().any(|message| {
            message.contains("implemented-and-verified requires validation references")
        }));
    }

    #[test]
    fn rejects_missing_delivery_issue() {
        let (manifest, mut inventory) = valid_fixture();
        inventory
            .capabilities
            .first_mut()
            .expect("capability should exist")
            .disposition = Disposition::MissingPlatformFeasible;

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("missing issue should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("requires a delivery issue"))
        );
    }

    #[test]
    fn rejects_missing_platform_exclusion() {
        let (manifest, mut inventory) = valid_fixture();
        inventory
            .capabilities
            .first_mut()
            .expect("capability should exist")
            .disposition = Disposition::ProvenPlatformImpossible;

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("missing exclusion should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("requires exclusion evidence"))
        );
    }

    #[test]
    fn rejects_duplicate_and_unsorted_ids() {
        let (manifest, mut inventory) = valid_fixture();
        let duplicate = inventory
            .capabilities
            .first()
            .expect("capability should exist")
            .clone();
        inventory.capabilities.push(duplicate);

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("duplicate should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("duplicate capability id"))
        );
    }

    #[test]
    fn rejects_escaping_local_reference() {
        let (manifest, mut inventory) = valid_fixture();
        let capability = inventory
            .capabilities
            .first_mut()
            .expect("capability should exist");
        capability.implementation.push(Reference::Local {
            path: "../secret".to_string(),
            anchor: None,
        });

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("escaping path should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("escapes repository"))
        );
    }

    #[test]
    fn rejects_baseline_drift() {
        let (mut manifest, inventory) = valid_fixture();
        manifest.baseline.version = "1.17.0".to_string();

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("baseline drift should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("version must be 1.16.0"))
        );
    }

    #[test]
    fn rejects_target_generator_kind_and_extractor_drift() {
        let (mut manifest, inventory) = valid_fixture();
        manifest.baseline.target = "x86_64-linux-kvm".to_string();
        manifest.generator_version += 1;
        manifest.inputs[0].extractor = "unknown-extractor".to_string();
        let item = manifest
            .items
            .first_mut()
            .expect("fixture should have a source item");
        item.kind = "unknown-kind".to_string();
        item.id = format!("unknown-kind:{}", item.key);

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("frozen manifest metadata should reject drift");
        for expected in [
            "target must be aarch64-macos-hvf",
            "generator_version must be 1",
            "input extractor is not recognized",
            "source item kind is not recognized",
        ] {
            assert!(
                errors
                    .messages()
                    .iter()
                    .any(|message| message.contains(expected)),
                "missing expected validation error: {expected}"
            );
        }
    }

    #[test]
    fn rejects_noncanonical_semantic_id() {
        let (manifest, mut inventory) = valid_fixture();
        let source_id = manifest.items[0].id.clone();
        inventory.capabilities.push(Capability {
            id: "semantic.Bad_Name:contract".to_string(),
            family: "api-contract".to_string(),
            summary: "Audit an invalid semantic identity.".to_string(),
            source_refs: vec![source_id],
            disposition: Disposition::AuditRequired,
            implementation: Vec::new(),
            validation: Vec::new(),
            delivery_issue: None,
            exclusion: None,
        });
        inventory
            .capabilities
            .sort_by(|left, right| left.id.cmp(&right.id));

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("noncanonical semantic identity should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("semantic capability id is not canonical"))
        );
    }

    #[test]
    fn rejects_duplicate_evidence_references() {
        let (manifest, mut inventory) = valid_fixture();
        let reference = Reference::Github {
            url: "https://github.com/seven332/bangbang/issues/1349".to_string(),
        };
        inventory.capabilities[0].implementation = vec![reference.clone(), reference];

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("duplicate references should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("duplicate") && message.contains("reference"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_local_reference_through_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let root = TempDirectory::new();
        let outside = TempDirectory::new();
        std::fs::write(outside.path().join("evidence.md"), "outside\n")
            .expect("outside evidence should be written");
        symlink(outside.path(), root.path().join("linked"))
            .expect("linked evidence directory should be created");

        let (manifest, mut inventory) = valid_fixture();
        inventory.capabilities[0]
            .implementation
            .push(Reference::Local {
                path: "linked/evidence.md".to_string(),
                anchor: None,
            });

        let errors = validate(&manifest, &inventory, root.path(), AuditMode::Delivery)
            .expect_err("a parent symlink must not escape the repository");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("escapes repository"))
        );
    }

    #[test]
    fn rejects_missing_overlay_and_unresolved_source_reference() {
        let (manifest, mut inventory) = valid_fixture();
        let removed = inventory.capabilities.remove(0);
        let capability = inventory
            .capabilities
            .first_mut()
            .expect("another capability should exist");
        capability.source_refs = vec!["corpus:unknown".to_string()];

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("missing and unresolved entries should fail");
        assert!(errors.messages().iter().any(|message| {
            message.contains(&format!("has no capability overlay: {}", removed.id))
        }));
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("source reference does not resolve"))
        );
    }

    #[test]
    fn rejects_missing_owner_and_summary() {
        let (manifest, mut inventory) = valid_fixture();
        let capability = inventory
            .capabilities
            .first_mut()
            .expect("capability should exist");
        capability.family = "missing-family".to_string();
        capability.summary.clear();

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("missing metadata should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("undeclared family"))
        );
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("summary must not be empty"))
        );
    }

    #[test]
    fn final_mode_rejects_missing_feasible_even_with_delivery_issue() {
        let (manifest, mut inventory) = valid_fixture();
        let capability = inventory
            .capabilities
            .first_mut()
            .expect("capability should exist");
        capability.disposition = Disposition::MissingPlatformFeasible;
        capability.delivery_issue = Some("#1349".to_string());

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Final)
            .expect_err("missing feasible should fail final mode");
        assert!(errors.messages().iter().any(|message| {
            message.contains("final validation forbids missing-platform-feasible")
        }));
    }

    #[test]
    fn rejects_incomplete_platform_exclusion() {
        let (manifest, mut inventory) = valid_fixture();
        let capability = inventory
            .capabilities
            .first_mut()
            .expect("capability should exist");
        capability.disposition = Disposition::ProvenPlatformImpossible;
        capability.exclusion = Some(PlatformExclusion {
            upstream_contract: Vec::new(),
            platform_evidence: Vec::new(),
            alternatives: Vec::new(),
            stable_behavior: Vec::new(),
            focused_tests: Vec::new(),
            compatibility_docs: Vec::new(),
            security_docs: Vec::new(),
            challenge: Reference::Authoritative {
                url: "https://example.com/challenge".to_string(),
            },
        });

        let errors = validate(&manifest, &inventory, Path::new("."), AuditMode::Delivery)
            .expect_err("incomplete exclusion should fail");
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("upstream_contract must not be empty"))
        );
        assert!(
            errors
                .messages()
                .iter()
                .any(|message| message.contains("challenge must be a GitHub reference"))
        );
    }
}
