use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    AuditMode, Baseline, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    LOGGER_PRODUCER_GENERATOR_VERSION, LOGGER_PRODUCER_SCHEMA_VERSION, LoggerClassDisposition,
    LoggerCompiledEvent, LoggerDeliveryPolicy, LoggerInvocation, LoggerInvocationSyntax,
    LoggerLevelPolicy, LoggerLimiterPolicy, LoggerModulePolicy, LoggerNonApplicableReason,
    LoggerOriginPolicy, LoggerProducerAudit, LoggerProducerClass, LoggerProducerCounts,
    LoggerProducerManifest, LoggerSourceContext, ValidationErrors,
};

const EXPECTED_SCANNED_RUST_FILES: usize = 362;
const EXPECTED_MATCHING_INPUT_FILES: usize = 81;
const EXPECTED_ORDINARY: usize = 429;
const EXPECTED_UNRESTRICTED: usize = 39;
const EXPECTED_ERROR: usize = 180;
const EXPECTED_WARN: usize = 138;
const EXPECTED_INFO: usize = 54;
const EXPECTED_DEBUG: usize = 47;
const EXPECTED_TRACE: usize = 10;
const EXPECTED_ERROR_UNRESTRICTED: usize = 22;
const EXPECTED_WARN_UNRESTRICTED: usize = 7;
const EXPECTED_INFO_UNRESTRICTED: usize = 10;
const EXPECTED_PRODUCTION: usize = 446;
const EXPECTED_TEST: usize = 0;
const EXPECTED_EXAMPLE: usize = 22;
const EXPECTED_DIRECT: usize = 466;
const EXPECTED_MACRO_TEMPLATE: usize = 2;
const LOGGER_EXTRACTOR: &str = "rust-logger-macro-v1";
const PLANNED_ISSUES: [&str; 3] = ["#1807", "#1808", "#1809"];

/// Validate the checked logger source manifest and human classification overlay.
pub fn validate_logger_producers(
    manifest: &LoggerProducerManifest,
    audit: &LoggerProducerAudit,
    repository_root: &Path,
    mode: AuditMode,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baselines(manifest, audit, &mut errors);
    validate_manifest(manifest, &mut errors);
    validate_audit(manifest, audit, repository_root, mode, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baselines(
    manifest: &LoggerProducerManifest,
    audit: &LoggerProducerAudit,
    errors: &mut Vec<String>,
) {
    if manifest.schema_version != LOGGER_PRODUCER_SCHEMA_VERSION {
        errors.push(format!(
            "logger producer manifest schema_version must be {LOGGER_PRODUCER_SCHEMA_VERSION}, found {}",
            manifest.schema_version
        ));
    }
    if audit.schema_version != LOGGER_PRODUCER_SCHEMA_VERSION {
        errors.push(format!(
            "logger producer audit schema_version must be {LOGGER_PRODUCER_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if manifest.generator_version != LOGGER_PRODUCER_GENERATOR_VERSION {
        errors.push(format!(
            "logger producer generator_version must be {LOGGER_PRODUCER_GENERATOR_VERSION}, found {}",
            manifest.generator_version
        ));
    }
    if manifest.baseline != audit.baseline {
        errors.push("logger producer manifest and audit baselines differ".to_string());
    }
    validate_expected_baseline("logger producer manifest", &manifest.baseline, errors);
    validate_expected_baseline("logger producer audit", &audit.baseline, errors);
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

fn validate_manifest(manifest: &LoggerProducerManifest, errors: &mut Vec<String>) {
    check_sorted_unique(
        manifest.inputs.iter().map(|input| input.path.as_str()),
        "logger input path",
        errors,
    );
    let input_paths = manifest
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect::<BTreeSet<_>>();
    for input in &manifest.inputs {
        if !is_safe_relative_path(Path::new(&input.path))
            || !input.path.starts_with("src/")
            || !input.path.ends_with(".rs")
        {
            errors.push(format!(
                "logger input path must be a safe src Rust path: {}",
                input.path
            ));
        }
        if input.git_blob.len() != 40
            || !input
                .git_blob
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            errors.push(format!(
                "logger input git_blob is not a Git object id: {}",
                input.path
            ));
        }
        if input.extractor != LOGGER_EXTRACTOR {
            errors.push(format!(
                "logger input extractor must be {LOGGER_EXTRACTOR}: {} -> {}",
                input.path, input.extractor
            ));
        }
    }

    check_sorted_unique(
        manifest
            .invocations
            .iter()
            .map(|invocation| invocation.id.as_str()),
        "logger invocation id",
        errors,
    );
    let mut invoked_paths = BTreeSet::new();
    for invocation in &manifest.invocations {
        validate_invocation(invocation, &input_paths, errors);
        invoked_paths.insert(invocation.path.as_str());
    }
    for path in input_paths.difference(&invoked_paths) {
        errors.push(format!("logger input contains no invocation: {path}"));
    }

    let actual_counts = computed_counts(
        manifest.counts.scanned_rust_files,
        manifest.inputs.len(),
        &manifest.invocations,
    );
    if actual_counts != manifest.counts {
        errors.push(format!(
            "declared logger producer counts do not match entries: declared {:?}, actual {:?}",
            manifest.counts, actual_counts
        ));
    }
    validate_expected_counts(&manifest.counts, errors);
}

fn validate_invocation(
    invocation: &LoggerInvocation,
    input_paths: &BTreeSet<&str>,
    errors: &mut Vec<String>,
) {
    if !input_paths.contains(invocation.path.as_str()) {
        errors.push(format!(
            "logger invocation references undeclared input: {}",
            invocation.id
        ));
    }
    if invocation.line == 0 || invocation.column == 0 {
        errors.push(format!(
            "logger invocation coordinates must be one-based: {}",
            invocation.id
        ));
    }
    let canonical_id = format!(
        "logger-invocation:{}:{}:{}",
        invocation.path, invocation.line, invocation.column
    );
    if invocation.id != canonical_id {
        errors.push(format!(
            "logger invocation id is not canonical: {}",
            invocation.id
        ));
    }
    let Some(digest) = invocation.fingerprint.strip_prefix("sha256:") else {
        errors.push(format!(
            "logger invocation fingerprint must use sha256: {}",
            invocation.id
        ));
        return;
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        errors.push(format!(
            "logger invocation fingerprint is not lowercase SHA-256: {}",
            invocation.id
        ));
    }
}

fn computed_counts(
    scanned_rust_files: usize,
    matching_input_files: usize,
    invocations: &[LoggerInvocation],
) -> LoggerProducerCounts {
    let macro_count = |name| {
        invocations
            .iter()
            .filter(|invocation| invocation.macro_name == name)
            .count()
    };
    let context_count = |context| {
        invocations
            .iter()
            .filter(|invocation| invocation.source_context == context)
            .count()
    };
    use crate::LoggerMacro;
    LoggerProducerCounts {
        scanned_rust_files,
        matching_input_files,
        ordinary: invocations
            .iter()
            .filter(|invocation| !invocation.macro_name.is_unrestricted())
            .count(),
        unrestricted: invocations
            .iter()
            .filter(|invocation| invocation.macro_name.is_unrestricted())
            .count(),
        error: macro_count(LoggerMacro::Error),
        warn: macro_count(LoggerMacro::Warn),
        info: macro_count(LoggerMacro::Info),
        debug: macro_count(LoggerMacro::Debug),
        trace: macro_count(LoggerMacro::Trace),
        error_unrestricted: macro_count(LoggerMacro::ErrorUnrestricted),
        warn_unrestricted: macro_count(LoggerMacro::WarnUnrestricted),
        info_unrestricted: macro_count(LoggerMacro::InfoUnrestricted),
        production: context_count(LoggerSourceContext::Production),
        test: context_count(LoggerSourceContext::Test),
        example: context_count(LoggerSourceContext::Example),
        direct: invocations
            .iter()
            .filter(|invocation| invocation.syntax == LoggerInvocationSyntax::Direct)
            .count(),
        macro_template: invocations
            .iter()
            .filter(|invocation| invocation.syntax == LoggerInvocationSyntax::MacroTemplate)
            .count(),
    }
}

fn validate_expected_counts(counts: &LoggerProducerCounts, errors: &mut Vec<String>) {
    let expected = [
        (
            "scanned Rust files",
            EXPECTED_SCANNED_RUST_FILES,
            counts.scanned_rust_files,
        ),
        (
            "matching input files",
            EXPECTED_MATCHING_INPUT_FILES,
            counts.matching_input_files,
        ),
        ("ordinary invocations", EXPECTED_ORDINARY, counts.ordinary),
        (
            "unrestricted invocations",
            EXPECTED_UNRESTRICTED,
            counts.unrestricted,
        ),
        ("error invocations", EXPECTED_ERROR, counts.error),
        ("warn invocations", EXPECTED_WARN, counts.warn),
        ("info invocations", EXPECTED_INFO, counts.info),
        ("debug invocations", EXPECTED_DEBUG, counts.debug),
        ("trace invocations", EXPECTED_TRACE, counts.trace),
        (
            "error_unrestricted invocations",
            EXPECTED_ERROR_UNRESTRICTED,
            counts.error_unrestricted,
        ),
        (
            "warn_unrestricted invocations",
            EXPECTED_WARN_UNRESTRICTED,
            counts.warn_unrestricted,
        ),
        (
            "info_unrestricted invocations",
            EXPECTED_INFO_UNRESTRICTED,
            counts.info_unrestricted,
        ),
        (
            "production invocations",
            EXPECTED_PRODUCTION,
            counts.production,
        ),
        ("test invocations", EXPECTED_TEST, counts.test),
        ("example invocations", EXPECTED_EXAMPLE, counts.example),
        ("direct invocations", EXPECTED_DIRECT, counts.direct),
        (
            "macro-template invocations",
            EXPECTED_MACRO_TEMPLATE,
            counts.macro_template,
        ),
    ];
    for (label, expected, actual) in expected {
        if actual != expected {
            errors.push(format!(
                "logger producer {label} must be {expected}, found {actual}"
            ));
        }
    }
    let invocation_total = counts.ordinary.checked_add(counts.unrestricted);
    let context_total = counts
        .production
        .checked_add(counts.test)
        .and_then(|total| total.checked_add(counts.example));
    if context_total.is_none() || context_total != invocation_total {
        errors.push("logger source-context counts must cover every invocation".to_string());
    }
    let syntax_total = counts.direct.checked_add(counts.macro_template);
    if syntax_total.is_none() || syntax_total != invocation_total {
        errors.push("logger syntax-kind counts must cover every invocation".to_string());
    }
}

fn validate_audit(
    manifest: &LoggerProducerManifest,
    audit: &LoggerProducerAudit,
    repository_root: &Path,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    let tracked_files = tracked_repository_files(repository_root, errors);
    check_sorted_unique(
        audit.classes.iter().map(|class| class.id.as_str()),
        "logger class id",
        errors,
    );
    let classes = audit
        .classes
        .iter()
        .map(|class| (class.id.as_str(), class))
        .collect::<BTreeMap<_, _>>();
    for class in &audit.classes {
        validate_class(class, repository_root, &tracked_files, mode, errors);
    }

    check_sorted_unique(
        audit
            .mappings
            .iter()
            .map(|mapping| mapping.invocation_id.as_str()),
        "logger mapping invocation id",
        errors,
    );
    let invocation_ids = manifest
        .invocations
        .iter()
        .map(|invocation| invocation.id.as_str())
        .collect::<BTreeSet<_>>();
    let mapping_ids = audit
        .mappings
        .iter()
        .map(|mapping| mapping.invocation_id.as_str())
        .collect::<BTreeSet<_>>();
    for missing in invocation_ids.difference(&mapping_ids) {
        errors.push(format!("logger invocation has no audit mapping: {missing}"));
    }
    for stale in mapping_ids.difference(&invocation_ids) {
        errors.push(format!("logger audit mapping is stale: {stale}"));
    }

    let invocations = manifest
        .invocations
        .iter()
        .map(|invocation| (invocation.id.as_str(), invocation))
        .collect::<BTreeMap<_, _>>();
    let mut referenced_classes = BTreeSet::new();
    for mapping in &audit.mappings {
        let Some(class) = classes.get(mapping.class_id.as_str()) else {
            errors.push(format!(
                "logger mapping references unknown class: {} -> {}",
                mapping.invocation_id, mapping.class_id
            ));
            continue;
        };
        referenced_classes.insert(class.id.as_str());
        if let Some(invocation) = invocations.get(mapping.invocation_id.as_str()) {
            validate_context_mapping(invocation, class, errors);
        }
    }
    for class_id in classes.keys() {
        if !referenced_classes.contains(class_id) {
            errors.push(format!(
                "logger class has no invocation mapping: {class_id}"
            ));
        }
    }

    validate_compiled_event_set(&audit.classes, errors);
    let planned_owners = audit
        .classes
        .iter()
        .filter(|class| class.disposition == LoggerClassDisposition::Planned)
        .filter_map(|class| class.delivery_issue.as_deref())
        .collect::<BTreeSet<_>>();
    for issue in PLANNED_ISSUES {
        if !planned_owners.contains(issue) {
            errors.push(format!(
                "logger audit has no planned class owned by {issue}"
            ));
        }
    }
}

fn validate_class(
    class: &LoggerProducerClass,
    repository_root: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    mode: AuditMode,
    errors: &mut Vec<String>,
) {
    if !is_logger_class_id(&class.id) {
        errors.push(format!("logger class id is not canonical: {}", class.id));
    }
    let lower_id = class.id.to_ascii_lowercase();
    let has_catch_all_term = lower_id.split('.').any(|segment| {
        segment == "catch-all"
            || segment
                .split('-')
                .any(|part| matches!(part, "other" | "unknown" | "catchall"))
    });
    if has_catch_all_term {
        errors.push(format!(
            "logger class id uses a catch-all term: {}",
            class.id
        ));
    }
    if class.summary.trim().is_empty() || class.rationale.trim().is_empty() {
        errors.push(format!(
            "logger class summary and rationale must not be empty: {}",
            class.id
        ));
    }
    check_sorted_unique(
        class.allowed_fields.iter().copied(),
        &format!("logger class allowed field for {}", class.id),
        errors,
    );
    check_sorted_unique(
        class.compiled_events.iter().copied(),
        &format!("logger class compiled event for {}", class.id),
        errors,
    );
    validate_limiter(class, errors);
    check_sorted_unique(
        class.implementation.iter(),
        &format!("logger class implementation reference for {}", class.id),
        errors,
    );
    check_sorted_unique(
        class.validation.iter(),
        &format!("logger class validation reference for {}", class.id),
        errors,
    );
    for reference in class.implementation.iter().chain(&class.validation) {
        validate_reference(
            reference,
            repository_root,
            tracked_files,
            &format!("logger class {} evidence", class.id),
            errors,
        );
    }
    if (class.compiled_events.is_empty()
        && (!class.implementation.is_empty() || !class.validation.is_empty()))
        || (!class.compiled_events.is_empty()
            && (class.implementation.is_empty() || class.validation.is_empty()))
    {
        errors.push(format!(
            "logger compiled-event metadata and exact evidence must appear together: {}",
            class.id
        ));
    }

    match class.disposition {
        LoggerClassDisposition::Implemented => {
            if class.non_applicable_reason.is_some() || class.delivery_issue.is_some() {
                errors.push(format!(
                    "implemented logger class must not retain a reason or delivery issue: {}",
                    class.id
                ));
            }
            if class.compiled_events.is_empty()
                || class.implementation.is_empty()
                || class.validation.is_empty()
            {
                errors.push(format!(
                    "implemented logger class requires compiled event and exact evidence: {}",
                    class.id
                ));
            }
            validate_applicable_policy(class, errors);
        }
        LoggerClassDisposition::Planned => {
            if mode == AuditMode::Final {
                errors.push(format!(
                    "final logger validation forbids planned class: {}",
                    class.id
                ));
            }
            if class.non_applicable_reason.is_some() {
                errors.push(format!(
                    "planned logger class must not claim a non-applicable reason: {}",
                    class.id
                ));
            }
            if !class
                .delivery_issue
                .as_deref()
                .is_some_and(|issue| PLANNED_ISSUES.contains(&issue))
            {
                errors.push(format!(
                    "planned logger class must be owned by #1807, #1808, or #1809: {}",
                    class.id
                ));
            }
            validate_applicable_policy(class, errors);
        }
        LoggerClassDisposition::NotApplicable => {
            if class.non_applicable_reason.is_none()
                || class.delivery_issue.is_some()
                || !class.compiled_events.is_empty()
                || !class.implementation.is_empty()
                || !class.validation.is_empty()
            {
                errors.push(format!(
                    "not-applicable logger class requires one reason and no owner/event/evidence: {}",
                    class.id
                ));
            }
            if class.guest_triggerable
                || class.delivery != LoggerDeliveryPolicy::NotApplicable
                || class.level != LoggerLevelPolicy::NotApplicable
                || class.module != LoggerModulePolicy::NotApplicable
                || class.origin != LoggerOriginPolicy::NotApplicable
                || class.limiter != LoggerLimiterPolicy::NotApplicable
                || class.limiter_identity.is_some()
                || !class.allowed_fields.is_empty()
            {
                errors.push(format!(
                    "not-applicable logger class must use only not-applicable policy: {}",
                    class.id
                ));
            }
        }
    }
}

fn validate_applicable_policy(class: &LoggerProducerClass, errors: &mut Vec<String>) {
    if class.delivery == LoggerDeliveryPolicy::NotApplicable
        || class.level == LoggerLevelPolicy::NotApplicable
        || class.module == LoggerModulePolicy::NotApplicable
        || class.origin == LoggerOriginPolicy::NotApplicable
        || class.limiter == LoggerLimiterPolicy::NotApplicable
    {
        errors.push(format!(
            "applicable logger class must define complete delivery policy: {}",
            class.id
        ));
    }
    let is_recovery = class.compiled_events == [LoggerCompiledEvent::RateLimitRecovery];
    if class.guest_triggerable != (class.delivery == LoggerDeliveryPolicy::NonblockingGuest) {
        errors.push(format!(
            "logger guest triggerability requires exact nonblocking-guest delivery: {}",
            class.id
        ));
    }
    if class.guest_triggerable && class.limiter != LoggerLimiterPolicy::RateLimited && !is_recovery
    {
        errors.push(format!(
            "guest-triggerable logger class must be rate limited: {}",
            class.id
        ));
    }
    if is_recovery && class.limiter != LoggerLimiterPolicy::Unrestricted {
        errors.push(format!(
            "logger limiter recovery must be unrestricted: {}",
            class.id
        ));
    }
}

fn validate_limiter(class: &LoggerProducerClass, errors: &mut Vec<String>) {
    match class.limiter {
        LoggerLimiterPolicy::RateLimited => {
            if !class
                .limiter_identity
                .as_deref()
                .is_some_and(is_limiter_identity)
            {
                errors.push(format!(
                    "rate-limited logger class requires canonical limiter identity: {}",
                    class.id
                ));
            }
        }
        LoggerLimiterPolicy::Unrestricted | LoggerLimiterPolicy::NotApplicable => {
            if class.limiter_identity.is_some() {
                errors.push(format!(
                    "unrestricted or inapplicable logger class must not name a limiter: {}",
                    class.id
                ));
            }
        }
    }
}

fn validate_context_mapping(
    invocation: &LoggerInvocation,
    class: &LoggerProducerClass,
    errors: &mut Vec<String>,
) {
    match invocation.source_context {
        LoggerSourceContext::Test => {
            if class.disposition != LoggerClassDisposition::NotApplicable
                || class.non_applicable_reason != Some(LoggerNonApplicableReason::TestOnly)
            {
                errors.push(format!(
                    "test logger invocation must map to test-only class: {}",
                    invocation.id
                ));
            }
        }
        LoggerSourceContext::Example => {
            if class.disposition != LoggerClassDisposition::NotApplicable
                || class.non_applicable_reason != Some(LoggerNonApplicableReason::ExampleOnly)
            {
                errors.push(format!(
                    "example logger invocation must map to example-only class: {}",
                    invocation.id
                ));
            }
        }
        LoggerSourceContext::Production => {
            if matches!(
                class.non_applicable_reason,
                Some(LoggerNonApplicableReason::TestOnly | LoggerNonApplicableReason::ExampleOnly)
            ) {
                errors.push(format!(
                    "production logger invocation cannot map to test/example class: {}",
                    invocation.id
                ));
            }
        }
    }
}

fn validate_compiled_event_set(classes: &[LoggerProducerClass], errors: &mut Vec<String>) {
    let expected = BTreeMap::from([
        (LoggerCompiledEvent::ApiRequest, "logger.api.request"),
        (LoggerCompiledEvent::InstanceStart, "logger.api.result"),
        (LoggerCompiledEvent::FlushMetrics, "logger.api.result"),
        (LoggerCompiledEvent::BootTime, "logger.boot.time"),
        (
            LoggerCompiledEvent::RateLimitRecovery,
            "logger.limiter.recovery",
        ),
        (LoggerCompiledEvent::ProcessPanic, "logger.process.panic"),
        (LoggerCompiledEvent::ProcessExit, "logger.process.exit"),
    ]);
    let actual = classes
        .iter()
        .flat_map(|class| class.compiled_events.iter().copied())
        .collect::<BTreeSet<_>>();
    let expected_events = expected.keys().copied().collect::<BTreeSet<_>>();
    if actual != expected_events {
        errors.push(format!(
            "logger compiled event set differs: expected {expected_events:?}, found {actual:?}"
        ));
    }
    for (event, expected_class) in expected {
        let owners = classes
            .iter()
            .filter(|class| class.compiled_events.contains(&event))
            .map(|class| class.id.as_str())
            .collect::<Vec<_>>();
        if owners != [expected_class] {
            errors.push(format!(
                "logger compiled event must have its exact class: {event:?} -> {owners:?}, expected {expected_class}"
            ));
        }
    }
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_logger_class_id(value: &str) -> bool {
    value
        .strip_prefix("logger.")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.split('.').all(is_slug))
}

fn is_limiter_identity(value: &str) -> bool {
    value
        .strip_prefix("logger-rate.")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.split('.').all(is_slug))
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn check_sorted_unique<I, T>(values: I, label: &str, errors: &mut Vec<String>)
where
    I: IntoIterator<Item = T>,
    T: Ord + std::fmt::Debug,
{
    let values = values.into_iter().collect::<Vec<_>>();
    for pair in values.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if left >= right {
            errors.push(format!(
                "{label} entries must be sorted and unique: {:?} then {:?}",
                left, right
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_and_limiter_ids_are_closed_slugs() {
        assert!(is_logger_class_id("logger.api.request"));
        assert!(is_limiter_identity("logger-rate.device.queue"));
        assert!(!is_logger_class_id("logger.api.other_value"));
        assert!(!is_limiter_identity("device.queue"));
    }

    #[test]
    fn safe_paths_reject_parent_and_absolute_components() {
        assert!(is_safe_relative_path(Path::new(
            "crates/runtime/src/logger.rs"
        )));
        assert!(!is_safe_relative_path(Path::new("../logger.rs")));
        assert!(!is_safe_relative_path(Path::new("/tmp/logger.rs")));
    }
}
