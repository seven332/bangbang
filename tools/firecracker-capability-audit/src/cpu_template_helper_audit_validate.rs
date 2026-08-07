use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS,
    CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS, CPU_TEMPLATE_HELPER_AUDIT_PATH,
    CPU_TEMPLATE_HELPER_AUDIT_SCHEMA_VERSION, CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS,
    CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS, Capability, CapabilityInventory,
    CpuTemplateHelperArtifact, CpuTemplateHelperAudit, CpuTemplateHelperExecution,
    CpuTemplateHelperNonclaim, CpuTemplateHelperOperationRecord, CpuTemplateHelperOutcome,
    CpuTemplateHelperProvider, CpuTemplateHelperScenario, CpuTemplateHelperScenarioRecord,
    CpuTemplateHelperSelection, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, Reference, ValidationErrors, cpu_template_helper_audit_json,
};

/// Exact operation order in the checked CPU-helper producer ledger.
pub const CPU_TEMPLATE_HELPER_OPERATION_IDS: [&str; 5] = [
    "tool-operation:cpu-template-helper/fingerprint/compare",
    "tool-operation:cpu-template-helper/fingerprint/dump",
    "tool-operation:cpu-template-helper/template/dump",
    "tool-operation:cpu-template-helper/template/strip",
    "tool-operation:cpu-template-helper/template/verify",
];

/// Exact supported runtime foundations consumed by aggregate CPU certification.
pub const CPU_TEMPLATE_IMPLEMENTED_FOUNDATION_IDS: [&str; 18] = [
    "api-operation:GET /machine-config",
    "api-operation:GET /vm/config",
    "api-operation:PATCH /machine-config",
    "api-operation:PUT /cpu-config",
    "api-operation:PUT /machine-config",
    "api-path:/cpu-config",
    "api-path:/machine-config",
    "api-path:/vm/config",
    "api-property:ArmRegisterModifier.addr",
    "api-property:ArmRegisterModifier.bitmap",
    "api-property:CpuConfig.reg_modifiers",
    "api-property:FullVmConfiguration.cpu-config",
    "api-property:FullVmConfiguration.machine-config",
    "api-schema:ArmRegisterModifier",
    "api-schema:CpuConfig",
    "api-schema:FullVmConfiguration",
    "api-schema:MachineConfiguration",
    "corpus:cpu-boot-protocol",
];

/// Exact architecture/platform exclusions consumed by aggregate CPU certification.
pub const CPU_TEMPLATE_PLATFORM_IMPOSSIBLE_FOUNDATION_IDS: [&str; 17] = [
    "api-property:CpuConfig.cpuid_modifiers",
    "api-property:CpuConfig.kvm_capabilities",
    "api-property:CpuConfig.msr_modifiers",
    "api-property:CpuConfig.vcpu_features",
    "api-property:CpuidLeafModifier.flags",
    "api-property:CpuidLeafModifier.leaf",
    "api-property:CpuidLeafModifier.modifiers",
    "api-property:CpuidLeafModifier.subleaf",
    "api-property:CpuidRegisterModifier.bitmap",
    "api-property:CpuidRegisterModifier.register",
    "api-property:MachineConfiguration.cpu_template",
    "api-property:MsrModifier.addr",
    "api-property:MsrModifier.bitmap",
    "api-schema:CpuTemplate",
    "api-schema:CpuidLeafModifier",
    "api-schema:CpuidRegisterModifier",
    "api-schema:MsrModifier",
];

/// Exact closed scenario order in the checked CPU-helper producer ledger.
pub const CPU_TEMPLATE_HELPER_SCENARIOS: [CpuTemplateHelperScenario; 14] = [
    CpuTemplateHelperScenario::InstalledCli,
    CpuTemplateHelperScenario::DefaultNoneEquivalence,
    CpuTemplateHelperScenario::CustomPrecedence,
    CpuTemplateHelperScenario::PendingStaticRejection,
    CpuTemplateHelperScenario::CanonicalTemplatePipeline,
    CpuTemplateHelperScenario::FingerprintChangePipeline,
    CpuTemplateHelperScenario::PortableProviderIndependence,
    CpuTemplateHelperScenario::SignedEntitlementEffectiveState,
    CpuTemplateHelperScenario::CollisionNonmutation,
    CpuTemplateHelperScenario::BoundedRedactionFailure,
    CpuTemplateHelperScenario::TransactionalRuntimeSelection,
    CpuTemplateHelperScenario::AllVcpuApplyReadbackBootPrecedence,
    CpuTemplateHelperScenario::NativeV1NoTemplateSnapshot,
    CpuTemplateHelperScenario::HeterogeneousFleetWorkflow,
];

const ALL_SELECTIONS: [CpuTemplateHelperSelection; 4] = [
    CpuTemplateHelperSelection::OmittedDefault,
    CpuTemplateHelperSelection::ExplicitNone,
    CpuTemplateHelperSelection::PendingV1n1,
    CpuTemplateHelperSelection::ExplicitCustom,
];

const STANDARD_OUTCOMES: [CpuTemplateHelperOutcome; 3] = [
    CpuTemplateHelperOutcome::SilentSuccess,
    CpuTemplateHelperOutcome::OperationalExitOneStderr,
    CpuTemplateHelperOutcome::InvalidInvocationExitTwoStderr,
];

const COMPARE_OUTCOMES: [CpuTemplateHelperOutcome; 4] = [
    CpuTemplateHelperOutcome::SilentSuccess,
    CpuTemplateHelperOutcome::DifferenceExitOneStderr,
    CpuTemplateHelperOutcome::OperationalExitOneStderr,
    CpuTemplateHelperOutcome::InvalidInvocationExitTwoStderr,
];

const EXPECTED_NONCLAIMS: [CpuTemplateHelperNonclaim; 7] = [
    CpuTemplateHelperNonclaim::TemplateSensibilityOrSecurity,
    CpuTemplateHelperNonclaim::DistinctHostEquivalenceOrSafety,
    CpuTemplateHelperNonclaim::X86KvmMechanismIdentity,
    CpuTemplateHelperNonclaim::ArtifactOrHostAuthentication,
    CpuTemplateHelperNonclaim::SnapshotPortability,
    CpuTemplateHelperNonclaim::MigrationSafety,
    CpuTemplateHelperNonclaim::GlobalCrashAtomicMultiPathPublication,
];

/// Validate the complete canonical CPU-helper producer ledger and its exact foundations.
pub fn validate_cpu_template_helper_audit(
    audit: &CpuTemplateHelperAudit,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, &mut errors);
    let tracked = tracked_repository_files(repository_root, &mut errors);
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();

    validate_operations(
        &audit.operations,
        &capabilities,
        repository_root,
        &tracked,
        &mut errors,
    );
    validate_foundations(audit, &capabilities, &mut errors);
    validate_scenarios(&audit.scenarios, repository_root, &tracked, &mut errors);

    if audit.nonclaims != EXPECTED_NONCLAIMS {
        errors
            .push("CPU-template helper audit requires the exact ordered nonclaim set".to_string());
    }

    let expected_implementation = aggregate_implementation();
    let expected_validation = aggregate_validation();
    if audit.implementation != expected_implementation {
        errors.push(
            "CPU-template helper audit requires exact aggregate implementation evidence"
                .to_string(),
        );
    }
    if audit.validation != expected_validation {
        errors.push(
            "CPU-template helper audit requires exact aggregate validation evidence".to_string(),
        );
    }
    validate_references(
        &audit.implementation,
        "aggregate implementation",
        true,
        repository_root,
        &tracked,
        &mut errors,
    );
    validate_references(
        &audit.validation,
        "aggregate validation",
        true,
        repository_root,
        &tracked,
        &mut errors,
    );
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(audit: &CpuTemplateHelperAudit, errors: &mut Vec<String>) {
    if audit.schema_version != CPU_TEMPLATE_HELPER_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "CPU-template helper audit schema_version must be {CPU_TEMPLATE_HELPER_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("CPU-template helper audit baseline is not the pinned release".to_string());
    }
    if audit.delivery_issue != "#1795" {
        errors.push("CPU-template helper audit delivery issue must be #1795".to_string());
    }
}

fn validate_operations(
    records: &[CpuTemplateHelperOperationRecord],
    capabilities: &BTreeMap<&str, &Capability>,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let actual_ids = records
        .iter()
        .map(|record| record.capability_id.as_str())
        .collect::<Vec<_>>();
    if actual_ids != CPU_TEMPLATE_HELPER_OPERATION_IDS {
        errors.push(format!(
            "CPU-template helper audit requires the exact ordered operation set: expected {:?}, found {actual_ids:?}",
            CPU_TEMPLATE_HELPER_OPERATION_IDS
        ));
    }

    let mut members = Vec::new();
    for record in records {
        members.extend(record.argument_ids.iter().map(String::as_str));
        members.push(record.capability_id.as_str());
        validate_operation(record, repository_root, tracked, errors);
    }
    let unique_members = members.iter().copied().collect::<BTreeSet<_>>();
    let expected_members = CPU_TEMPLATE_HELPER_COMPATIBILITY_CAPABILITY_IDS
        .into_iter()
        .chain(CPU_TEMPLATE_STRIP_COMPATIBILITY_CAPABILITY_IDS)
        .chain(CPU_TEMPLATE_FINGERPRINT_DUMP_COMPATIBILITY_CAPABILITY_IDS)
        .chain(CPU_TEMPLATE_FINGERPRINT_COMPARE_COMPATIBILITY_CAPABILITY_IDS)
        .collect::<BTreeSet<_>>();
    if members.len() != expected_members.len() || unique_members != expected_members {
        errors.push(format!(
            "CPU-template helper audit requires every one of the exact 18 helper identities once: expected {expected_members:?}, found {unique_members:?} across {} memberships",
            members.len()
        ));
    }
    for id in expected_members {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::ImplementedAndVerified => {}
            Some(_) => errors.push(format!(
                "CPU-template helper audit requires implemented-and-verified helper identity: {id}"
            )),
            None => errors.push(format!(
                "CPU-template helper audit identity is missing: {id}"
            )),
        }
    }
}

fn validate_operation(
    record: &CpuTemplateHelperOperationRecord,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let Some(spec) = operation_spec(&record.capability_id) else {
        errors.push(format!(
            "unknown CPU-template helper operation producer: {}",
            record.capability_id
        ));
        return;
    };

    if record.argument_ids != spec.argument_ids {
        errors.push(format!(
            "CPU-template helper operation has stale argument ownership: {}",
            record.capability_id
        ));
    }
    if record.execution != spec.execution
        || record.selections != spec.selections
        || record.input_artifacts != spec.input_artifacts
        || record.output_artifacts != spec.output_artifacts
        || record.providers != spec.providers
        || record.outcomes != spec.outcomes
    {
        errors.push(format!(
            "CPU-template helper operation has stale execution, selection, artifact, provider, or outcome claims: {}",
            record.capability_id
        ));
    }

    let expected_pinned = pinned_references(&record.capability_id, &record.argument_ids);
    let expected_implementation = local_references(spec.implementation);
    let expected_focused = local_references(spec.focused_validation);
    let expected_process = local_references(spec.process_validation);
    let expected_signed = local_references(spec.signed_validation);
    let expected_failure = local_references(spec.failure_validation);
    let expected_documentation = local_references(spec.documentation);
    let evidence = &record.evidence;
    if evidence.pinned != expected_pinned
        || evidence.implementation != expected_implementation
        || evidence.focused_validation != expected_focused
        || evidence.process_validation != expected_process
        || evidence.signed_validation != expected_signed
        || evidence.failure_validation != expected_failure
        || evidence.documentation != expected_documentation
    {
        errors.push(format!(
            "CPU-template helper operation requires exact categorized evidence: {}",
            record.capability_id
        ));
    }

    for (kind, references, required) in [
        ("pinned", evidence.pinned.as_slice(), true),
        ("implementation", evidence.implementation.as_slice(), true),
        (
            "focused validation",
            evidence.focused_validation.as_slice(),
            true,
        ),
        (
            "process validation",
            evidence.process_validation.as_slice(),
            true,
        ),
        (
            "signed validation",
            evidence.signed_validation.as_slice(),
            record.execution == CpuTemplateHelperExecution::SignedEffective,
        ),
        (
            "failure validation",
            evidence.failure_validation.as_slice(),
            true,
        ),
        ("documentation", evidence.documentation.as_slice(), true),
    ] {
        validate_references(
            references,
            &format!("{} {kind}", record.capability_id),
            required,
            repository_root,
            tracked,
            errors,
        );
    }
    if record.execution == CpuTemplateHelperExecution::Portable
        && !evidence.signed_validation.is_empty()
    {
        errors.push(format!(
            "portable CPU-template helper operation must not claim signed validation: {}",
            record.capability_id
        ));
    }
}

fn validate_foundations(
    audit: &CpuTemplateHelperAudit,
    capabilities: &BTreeMap<&str, &Capability>,
    errors: &mut Vec<String>,
) {
    if audit.foundations.implemented_and_verified != CPU_TEMPLATE_IMPLEMENTED_FOUNDATION_IDS {
        errors.push(
            "CPU-template helper audit requires the exact ordered implemented foundation set"
                .to_string(),
        );
    }
    if audit.foundations.proven_platform_impossible
        != CPU_TEMPLATE_PLATFORM_IMPOSSIBLE_FOUNDATION_IDS
    {
        errors.push("CPU-template helper audit requires the exact ordered platform-impossible foundation set".to_string());
    }

    let implemented = audit
        .foundations
        .implemented_and_verified
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let impossible = audit
        .foundations
        .proven_platform_impossible
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if implemented.len() != audit.foundations.implemented_and_verified.len()
        || impossible.len() != audit.foundations.proven_platform_impossible.len()
        || !implemented.is_disjoint(&impossible)
    {
        errors.push(
            "CPU-template helper foundation identities must be unique and disjoint".to_string(),
        );
    }

    require_foundation_disposition(
        CPU_TEMPLATE_IMPLEMENTED_FOUNDATION_IDS,
        Disposition::ImplementedAndVerified,
        capabilities,
        errors,
    );
    require_foundation_disposition(
        CPU_TEMPLATE_PLATFORM_IMPOSSIBLE_FOUNDATION_IDS,
        Disposition::ProvenPlatformImpossible,
        capabilities,
        errors,
    );
}

fn require_foundation_disposition<const N: usize>(
    ids: [&str; N],
    disposition: Disposition,
    capabilities: &BTreeMap<&str, &Capability>,
    errors: &mut Vec<String>,
) {
    for id in ids {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == disposition => {}
            Some(_) => errors.push(format!(
                "CPU-template helper foundation has the wrong terminal disposition: {id}"
            )),
            None => errors.push(format!(
                "CPU-template helper foundation capability is missing: {id}"
            )),
        }
    }
}

fn validate_scenarios(
    records: &[CpuTemplateHelperScenarioRecord],
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let actual = records.iter().map(|record| record.id).collect::<Vec<_>>();
    if actual != CPU_TEMPLATE_HELPER_SCENARIOS {
        errors
            .push("CPU-template helper audit requires the exact ordered scenario set".to_string());
    }
    let unique = actual.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != actual.len() {
        errors.push("CPU-template helper scenarios must be unique".to_string());
    }

    for record in records {
        let spec = scenario_spec(record.id);
        let expected_implementation = local_references(spec.implementation);
        let expected_validation = local_references(spec.validation);
        let expected_documentation = local_references(spec.documentation);
        if record.rationale != spec.rationale
            || record.implementation != expected_implementation
            || record.validation != expected_validation
            || record.documentation != expected_documentation
        {
            errors.push(format!(
                "CPU-template helper scenario requires exact rationale and evidence: {:?}",
                record.id
            ));
        }
        for (kind, references) in [
            ("implementation", record.implementation.as_slice()),
            ("validation", record.validation.as_slice()),
            ("documentation", record.documentation.as_slice()),
        ] {
            validate_references(
                references,
                &format!("scenario {:?} {kind}", record.id),
                true,
                repository_root,
                tracked,
                errors,
            );
        }
    }
}

fn validate_references(
    references: &[Reference],
    label: &str,
    required: bool,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    if required && references.is_empty() {
        errors.push(format!(
            "CPU-template helper audit requires {label} evidence"
        ));
    }
    if references
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left >= right))
    {
        errors.push(format!(
            "CPU-template helper audit {label} references must be sorted and unique"
        ));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("CPU-template helper audit {label}[{index}]"),
            errors,
        );
        let Reference::Local {
            path,
            anchor: Some(anchor),
        } = reference
        else {
            errors.push(format!(
                "CPU-template helper audit {label} evidence must be an anchored local reference"
            ));
            continue;
        };
        match std::fs::read_to_string(repository_root.join(path)) {
            Ok(source) if source.contains(anchor) => {}
            Ok(_) => errors.push(format!(
                "CPU-template helper audit {label} anchor does not resolve: {path}: {anchor}"
            )),
            Err(_) => errors.push(format!(
                "CPU-template helper audit {label} path is unreadable: {path}"
            )),
        }
    }
}

fn validate_canonical_bytes(
    audit: &CpuTemplateHelperAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match cpu_template_helper_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize canonical CPU-template helper audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(CPU_TEMPLATE_HELPER_AUDIT_PATH)) {
        Ok(checked) if checked == canonical => {}
        Ok(_) => {
            errors.push("checked CPU-template helper audit bytes are not canonical".to_string())
        }
        Err(_) => errors.push("checked CPU-template helper audit is unreadable".to_string()),
    }
}

struct OperationSpec {
    argument_ids: &'static [&'static str],
    execution: CpuTemplateHelperExecution,
    selections: &'static [CpuTemplateHelperSelection],
    input_artifacts: &'static [CpuTemplateHelperArtifact],
    output_artifacts: &'static [CpuTemplateHelperArtifact],
    providers: &'static [CpuTemplateHelperProvider],
    outcomes: &'static [CpuTemplateHelperOutcome],
    implementation: &'static [(&'static str, &'static str)],
    focused_validation: &'static [(&'static str, &'static str)],
    process_validation: &'static [(&'static str, &'static str)],
    signed_validation: &'static [(&'static str, &'static str)],
    failure_validation: &'static [(&'static str, &'static str)],
    documentation: &'static [(&'static str, &'static str)],
}

fn operation_spec(id: &str) -> Option<OperationSpec> {
    Some(match id {
        "tool-operation:cpu-template-helper/fingerprint/compare" => OperationSpec {
            argument_ids: &[
                "tool-argument:cpu-template-helper/fingerprint/compare/curr",
                "tool-argument:cpu-template-helper/fingerprint/compare/filters",
                "tool-argument:cpu-template-helper/fingerprint/compare/prev",
            ],
            execution: CpuTemplateHelperExecution::Portable,
            selections: &[],
            input_artifacts: &[CpuTemplateHelperArtifact::CpuFingerprintDocument],
            output_artifacts: &[],
            providers: &[],
            outcomes: &COMPARE_OUTCOMES,
            implementation: &[
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "Fingerprint(FingerprintOperation::Compare",
                ),
                (
                    "tools/cpu-template-helper/src/fingerprint_compare.rs",
                    "pub fn compare_cpu_fingerprints",
                ),
                (
                    "tools/cpu-template-helper/src/input.rs",
                    "pub fn read_regular_utf8",
                ),
                (
                    "tools/cpu-template-helper/src/strip.rs",
                    "pub fn strip_cpu_template_documents",
                ),
            ],
            focused_validation: &[
                (
                    "tools/cpu-template-helper/src/fingerprint_compare.rs",
                    "fn guest_difference_reuses_native_width_strip_and_preserves_missing_identity",
                ),
                (
                    "tools/cpu-template-helper/src/fingerprint_compare.rs",
                    "fn macos_defaults_emit_all_differences_in_public_order_and_repeat",
                ),
            ],
            process_validation: &[
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn fingerprint_compare_emits_exact_canonical_difference_and_fixed_order",
                ),
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn fingerprint_compare_equal_default_and_aliases_are_portable_silent_and_nonmutating",
                ),
            ],
            signed_validation: &[],
            failure_validation: &[
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn fingerprint_compare_filter_and_platform_errors_are_fixed_and_value_redacted",
                ),
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn fingerprint_compare_rejects_strict_document_and_file_failures_without_mutation",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-fingerprint-compare-contract.md",
                "Terminal certification",
            )],
        },
        "tool-operation:cpu-template-helper/fingerprint/dump" => OperationSpec {
            argument_ids: &[
                "tool-argument:cpu-template-helper/fingerprint/dump/config",
                "tool-argument:cpu-template-helper/fingerprint/dump/output",
                "tool-argument:cpu-template-helper/fingerprint/dump/template",
            ],
            execution: CpuTemplateHelperExecution::SignedEffective,
            selections: &ALL_SELECTIONS,
            input_artifacts: &[
                CpuTemplateHelperArtifact::ConfigurationDocument,
                CpuTemplateHelperArtifact::CpuTemplateDocument,
            ],
            output_artifacts: &[CpuTemplateHelperArtifact::CpuFingerprintDocument],
            providers: &[
                CpuTemplateHelperProvider::EffectiveHvf,
                CpuTemplateHelperProvider::SystemHost,
            ],
            outcomes: &STANDARD_OUTCOMES,
            implementation: &[
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "Fingerprint(FingerprintOperation::Dump",
                ),
                (
                    "tools/cpu-template-helper/src/fingerprint.rs",
                    "pub fn dump_with_providers",
                ),
                (
                    "tools/cpu-template-helper/src/host.rs",
                    "pub struct SystemHostFingerprintProvider",
                ),
                (
                    "tools/cpu-template-helper/src/provider.rs",
                    "pub struct HvfEffectiveCpuTemplateProvider",
                ),
                (
                    "tools/cpu-template-helper/src/publication.rs",
                    "pub fn publish_new_artifact",
                ),
            ],
            focused_validation: &[
                (
                    "tools/cpu-template-helper/src/fingerprint.rs",
                    "fn macos_golden_bytes_round_trip_and_accept_other_canonical_producer_versions",
                ),
                (
                    "tools/cpu-template-helper/src/host.rs",
                    "fn capture_queries_exact_public_facts_once_in_order",
                ),
            ],
            process_validation: &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn fingerprint_failures_are_bounded_and_publish_neither_default_nor_explicit_output",
            )],
            signed_validation: &[
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_all_operations_compose_canonical_artifacts",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_fingerprint_dump_covers_real_macos_default_static_and_custom_selection",
                ),
            ],
            failure_validation: &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn fingerprint_inputs_fail_before_host_or_effective_capture",
            )],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-fingerprint-contract.md",
                "Terminal certification",
            )],
        },
        "tool-operation:cpu-template-helper/template/dump" => OperationSpec {
            argument_ids: &[
                "tool-argument:cpu-template-helper/template/dump/config",
                "tool-argument:cpu-template-helper/template/dump/output",
                "tool-argument:cpu-template-helper/template/dump/template",
            ],
            execution: CpuTemplateHelperExecution::SignedEffective,
            selections: &ALL_SELECTIONS,
            input_artifacts: &[
                CpuTemplateHelperArtifact::ConfigurationDocument,
                CpuTemplateHelperArtifact::CpuTemplateDocument,
            ],
            output_artifacts: &[CpuTemplateHelperArtifact::CpuTemplateDocument],
            providers: &[CpuTemplateHelperProvider::EffectiveHvf],
            outcomes: &STANDARD_OUTCOMES,
            implementation: &[
                (
                    "crates/hvf/src/cpu_template_inspection.rs",
                    "pub fn inspect_effective_arm64_cpu_template",
                ),
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "Template(TemplateOperation::Dump",
                ),
                (
                    "tools/cpu-template-helper/src/provider.rs",
                    "pub struct HvfEffectiveCpuTemplateProvider",
                ),
                (
                    "tools/cpu-template-helper/src/publication.rs",
                    "pub fn publish_new_artifact",
                ),
            ],
            focused_validation: &[
                (
                    "crates/hvf/src/cpu_template_inspection.rs",
                    "fn capture_plan_uses_the_exact_runtime_census_and_native_widths",
                ),
                (
                    "tools/cpu-template-helper/src/profile.rs",
                    "fn dump_uses_one_profile_and_excludes_boot_overridden_targets",
                ),
            ],
            process_validation: &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn no_template_and_unavailable_hvf_never_publish_output",
            )],
            signed_validation: &[
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_all_operations_compose_canonical_artifacts",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_two_vcpu_default_dump_is_canonical_private_and_reparseable",
                ),
            ],
            failure_validation: &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn bounded_input_failures_are_path_and_value_redacted_before_inspection",
            )],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                "Terminal certification",
            )],
        },
        "tool-operation:cpu-template-helper/template/strip" => OperationSpec {
            argument_ids: &[
                "tool-argument:cpu-template-helper/template/strip/paths",
                "tool-argument:cpu-template-helper/template/strip/suffix",
            ],
            execution: CpuTemplateHelperExecution::Portable,
            selections: &[],
            input_artifacts: &[CpuTemplateHelperArtifact::CpuTemplateDocument],
            output_artifacts: &[CpuTemplateHelperArtifact::CpuTemplateDocument],
            providers: &[],
            outcomes: &STANDARD_OUTCOMES,
            implementation: &[
                ("tools/cpu-template-helper/src/cli.rs", "Strip {"),
                (
                    "tools/cpu-template-helper/src/input.rs",
                    "pub(crate) fn prepare_strip_input",
                ),
                (
                    "tools/cpu-template-helper/src/strip.rs",
                    "pub fn strip_cpu_template_documents",
                ),
                (
                    "tools/cpu-template-helper/src/strip_publication.rs",
                    "pub(crate) fn publish_strip_artifacts",
                ),
            ],
            focused_validation: &[
                (
                    "tools/cpu-template-helper/src/strip.rs",
                    "fn strips_native_width_differences_and_preserves_missing_entries",
                ),
                (
                    "tools/cpu-template-helper/src/strip_publication.rs",
                    "fn rolls_back_every_observed_split_boundary_in_both_modes",
                ),
            ],
            process_validation: &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn strip_default_and_explicit_suffixes_are_portable_and_silent",
            )],
            signed_validation: &[],
            failure_validation: &[
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn strip_bad_documents_and_file_types_fail_before_any_output",
                ),
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn strip_precommit_failures_preserve_inputs_winners_and_aliases",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-strip-contract.md",
                "Terminal certification",
            )],
        },
        "tool-operation:cpu-template-helper/template/verify" => OperationSpec {
            argument_ids: &[
                "tool-argument:cpu-template-helper/template/verify/config",
                "tool-argument:cpu-template-helper/template/verify/template",
            ],
            execution: CpuTemplateHelperExecution::SignedEffective,
            selections: &ALL_SELECTIONS,
            input_artifacts: &[
                CpuTemplateHelperArtifact::ConfigurationDocument,
                CpuTemplateHelperArtifact::CpuTemplateDocument,
            ],
            output_artifacts: &[],
            providers: &[CpuTemplateHelperProvider::EffectiveHvf],
            outcomes: &STANDARD_OUTCOMES,
            implementation: &[
                (
                    "crates/hvf/src/cpu_template_inspection.rs",
                    "pub fn inspect_effective_arm64_cpu_template",
                ),
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "Template(TemplateOperation::Verify",
                ),
                (
                    "tools/cpu-template-helper/src/profile.rs",
                    "pub fn verify_with_provider",
                ),
                (
                    "tools/cpu-template-helper/src/provider.rs",
                    "pub struct HvfEffectiveCpuTemplateProvider",
                ),
            ],
            focused_validation: &[(
                "tools/cpu-template-helper/src/profile.rs",
                "fn verify_uses_filters_and_checks_boot_overridden_application_values",
            )],
            process_validation: &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn no_template_and_unavailable_hvf_never_publish_output",
            )],
            signed_validation: &[
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_all_operations_compose_canonical_artifacts",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_mixed_width_verify_and_explicit_precedence_use_real_hvf_state",
                ),
            ],
            failure_validation: &[(
                "tools/cpu-template-helper/tests/hvf_e2e.rs",
                "fn signed_mismatch_collision_and_unsigned_failures_leave_resources_reusable",
            )],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                "Terminal certification",
            )],
        },
        _ => return None,
    })
}

struct ScenarioSpec {
    rationale: &'static str,
    implementation: &'static [(&'static str, &'static str)],
    validation: &'static [(&'static str, &'static str)],
    documentation: &'static [(&'static str, &'static str)],
}

fn scenario_spec(id: CpuTemplateHelperScenario) -> ScenarioSpec {
    match id {
        CpuTemplateHelperScenario::InstalledCli => ScenarioSpec {
            rationale: "The installed binary exposes exactly five operations, thirteen arguments, fixed help and version stdout, silent successful operations, operational or difference exit 1, and invalid-invocation exit 2 without echoing caller values.",
            implementation: &[
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "pub fn run_cli_with_provider",
                ),
                (
                    "tools/cpu-template-helper/src/main.rs",
                    "fn main() -> ExitCode",
                ),
            ],
            validation: &[
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn every_invalid_invocation_is_fixed_and_does_not_echo_values",
                ),
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn help_and_version_are_the_only_portable_stdout_successes",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                "Diagnostics, exit classes, and evidence",
            )],
        },
        CpuTemplateHelperScenario::DefaultNoneEquivalence => ScenarioSpec {
            rationale: "Omitted default and explicit None remain distinct inputs but select the same no-template effective profile and produce separately canonical artifacts.",
            implementation: &[(
                "tools/cpu-template-helper/src/projection.rs",
                "pub fn prepare_inspection_request",
            )],
            validation: &[
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_all_operations_compose_canonical_artifacts",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_fingerprint_dump_covers_real_macos_default_static_and_custom_selection",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                "Configuration and selection",
            )],
        },
        CpuTemplateHelperScenario::CustomPrecedence => ScenarioSpec {
            rationale: "Config-file machine then custom ordering and the explicit template argument apply transactionally, with the last valid custom selection replacing pending static state before effective capture.",
            implementation: &[
                (
                    "crates/runtime/src/lib.rs",
                    "VmmAction::PutCpuConfig(config) => {",
                ),
                (
                    "tools/cpu-template-helper/src/projection.rs",
                    "pub fn prepare_inspection_request",
                ),
            ],
            validation: &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn config_file_custom_cpu_config_overrides_v1n1_and_starts_instance",
                ),
                (
                    "tools/cpu-template-helper/src/projection.rs",
                    "fn explicit_template_replaces_config_selection_after_valid_projection",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_mixed_width_verify_and_explicit_precedence_use_real_hvf_state",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-contract.md",
                "Replacement and serialization",
            )],
        },
        CpuTemplateHelperScenario::PendingStaticRejection => ScenarioSpec {
            rationale: "V1N1 remains visible pending configuration but fails before backend construction on Apple Silicon, while explicit None or a valid custom template can replace it without aliasing the upstream source model.",
            implementation: &[(
                "crates/runtime/src/lib.rs",
                "pub fn preflight_instance_start",
            )],
            validation: &[
                (
                    "crates/bangbang/src/api_server.rs",
                    "fn machine_v1n1_is_visible_and_start_gated_while_x86_template_faults_preserve_it",
                ),
                (
                    "crates/runtime/src/lib.rs",
                    "fn effective_v1n1_fails_before_executor_and_custom_replacement_can_retry_start",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_fingerprint_dump_covers_real_macos_default_static_and_custom_selection",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-contract.md",
                "Replacement and serialization",
            )],
        },
        CpuTemplateHelperScenario::CanonicalTemplatePipeline => ScenarioSpec {
            rationale: "Signed dump output is private and canonical, portable strip consumes immutable inputs and republishes canonical outputs, and signed verify consumes the resulting strict template without stream output.",
            implementation: &[
                (
                    "tools/cpu-template-helper/src/profile.rs",
                    "pub fn dump_with_provider",
                ),
                (
                    "tools/cpu-template-helper/src/strip.rs",
                    "pub fn strip_cpu_template_documents",
                ),
            ],
            validation: &[(
                "tools/cpu-template-helper/tests/hvf_e2e.rs",
                "fn signed_all_operations_compose_canonical_artifacts",
            )],
            documentation: &[
                (
                    "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                    "Template document",
                ),
                (
                    "compat/firecracker/v1.16.0/cpu-template-strip-contract.md",
                    "Normalized strip transformation",
                ),
            ],
        },
        CpuTemplateHelperScenario::FingerprintChangePipeline => ScenarioSpec {
            rationale: "Signed default and custom fingerprint dumps are canonical and platform tagged; portable compare is silent for equality and emits only deterministic selected differences with the stable difference exit.",
            implementation: &[
                (
                    "tools/cpu-template-helper/src/fingerprint.rs",
                    "pub fn dump_with_providers",
                ),
                (
                    "tools/cpu-template-helper/src/fingerprint_compare.rs",
                    "pub fn compare_cpu_fingerprints",
                ),
            ],
            validation: &[(
                "tools/cpu-template-helper/tests/hvf_e2e.rs",
                "fn signed_all_operations_compose_canonical_artifacts",
            )],
            documentation: &[
                (
                    "compat/firecracker/v1.16.0/cpu-template-fingerprint-compare-contract.md",
                    "Deterministic difference diagnostic",
                ),
                (
                    "compat/firecracker/v1.16.0/cpu-template-fingerprint-contract.md",
                    "Version-1 document",
                ),
            ],
        },
        CpuTemplateHelperScenario::PortableProviderIndependence => ScenarioSpec {
            rationale: "Template strip and fingerprint compare decode and transform persisted documents without constructing an HVF effective-state or host-fact provider on supported or unsupported targets.",
            implementation: &[
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "TemplateOperation::Strip",
                ),
                (
                    "tools/cpu-template-helper/src/fingerprint_compare.rs",
                    "pub fn compare_cpu_fingerprints",
                ),
            ],
            validation: &[
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "fn fingerprint_compare_is_ordered_portable_and_provider_free",
                ),
                (
                    "tools/cpu-template-helper/src/cli.rs",
                    "fn strip_is_silent_and_never_constructs_an_effective_provider_request",
                ),
            ],
            documentation: &[
                (
                    "compat/firecracker/v1.16.0/cpu-template-fingerprint-compare-contract.md",
                    "Execution, diagnostics, and exit classes",
                ),
                (
                    "compat/firecracker/v1.16.0/cpu-template-strip-contract.md",
                    "Diagnostics and evidence",
                ),
            ],
        },
        CpuTemplateHelperScenario::SignedEntitlementEffectiveState => ScenarioSpec {
            rationale: "Effective dump, verify, and fingerprint dump require the public HVF entitlement, inspect one disposable real topology at the production apply/readback checkpoint, and fail closed when unsigned or unsupported.",
            implementation: &[
                (
                    "crates/hvf/src/cpu_template_inspection.rs",
                    "pub fn inspect_effective_arm64_cpu_template",
                ),
                ("scripts/run-integration-tests.sh", "cpu_template_helper"),
            ],
            validation: &[
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_mismatch_collision_and_unsigned_failures_leave_resources_reusable",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_two_vcpu_default_dump_is_canonical_private_and_reparseable",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                "Descriptor and provider authority",
            )],
        },
        CpuTemplateHelperScenario::CollisionNonmutation => ScenarioSpec {
            rationale: "Descriptor-bound reads never mutate inputs, single outputs are absent-only, strip publication preserves winners and exact inputs across collisions, and reusable HVF resources survive rejected publication.",
            implementation: &[
                (
                    "tools/cpu-template-helper/src/publication.rs",
                    "pub fn publish_new_artifact",
                ),
                (
                    "tools/cpu-template-helper/src/strip_publication.rs",
                    "pub(crate) fn publish_strip_artifacts",
                ),
            ],
            validation: &[
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn strip_precommit_failures_preserve_inputs_winners_and_aliases",
                ),
                (
                    "tools/cpu-template-helper/tests/hvf_e2e.rs",
                    "fn signed_mismatch_collision_and_unsigned_failures_leave_resources_reusable",
                ),
            ],
            documentation: &[
                (
                    "compat/firecracker/v1.16.0/cpu-template-helper-contract.md",
                    "Input and publication boundary",
                ),
                (
                    "compat/firecracker/v1.16.0/cpu-template-strip-contract.md",
                    "Multi-path publication boundary",
                ),
            ],
        },
        CpuTemplateHelperScenario::BoundedRedactionFailure => ScenarioSpec {
            rationale: "Malformed, oversized, special-file, provider, mismatch, filter, collision, and publication failures remain bounded and value redacted, preserve inputs and committed winners, and use only fixed stderr classes.",
            implementation: &[
                (
                    "tools/cpu-template-helper/src/input.rs",
                    "pub fn read_regular_utf8",
                ),
                (
                    "tools/cpu-template-helper/src/profile.rs",
                    "pub enum EffectiveCpuTemplateProfileError",
                ),
            ],
            validation: &[
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn bounded_input_failures_are_path_and_value_redacted_before_inspection",
                ),
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn fingerprint_compare_rejects_strict_document_and_file_failures_without_mutation",
                ),
                (
                    "tools/cpu-template-helper/tests/cli.rs",
                    "fn strip_bad_documents_and_file_types_fail_before_any_output",
                ),
            ],
            documentation: &[("docs/security.md", "The public CPU-template helper")],
        },
        CpuTemplateHelperScenario::TransactionalRuntimeSelection => ScenarioSpec {
            rationale: "Complete candidate validation precedes every static or custom replacement; valid custom, empty, pending V1N1, and explicit None updates replace atomically while malformed or unavailable requests preserve the previous effective selection.",
            implementation: &[
                (
                    "crates/runtime/src/lib.rs",
                    "VmmAction::PutCpuConfig(config) => {",
                ),
                (
                    "crates/runtime/src/machine.rs",
                    "pub fn validate(self) -> Result<MachineConfig, MachineConfigError>",
                ),
            ],
            validation: &[
                (
                    "crates/runtime/src/lib.rs",
                    "fn failed_cpu_template_replacements_preserve_the_effective_selection",
                ),
                (
                    "crates/runtime/src/lib.rs",
                    "fn static_and_custom_cpu_template_updates_replace_only_when_explicit",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-contract.md",
                "Replacement and serialization",
            )],
        },
        CpuTemplateHelperScenario::AllVcpuApplyReadbackBootPrecedence => ScenarioSpec {
            rationale: "Production startup reads one common requested baseline across every vCPU, writes and immediately rereads every target, then primary and PSCI-secondary Linux boot setup owns X0, PC, and PSTATE while retained feature targets remain guest visible.",
            implementation: &[
                (
                    "crates/hvf/src/cpu_template.rs",
                    "pub(crate) fn capture_common_arm64_cpu_template_values",
                ),
                (
                    "crates/hvf/src/topology.rs",
                    "pub(crate) fn apply_arm64_cpu_template_with_state",
                ),
            ],
            validation: &[
                (
                    "crates/hvf/tests/guest_boot.rs",
                    "fn boots_firecracker_kernel_and_executes_userspace_on_secondary_cpu",
                ),
                (
                    "crates/hvf/tests/guest_boot.rs",
                    "fn two_cpu_linux_observes_exact_custom_id_register_mask_results",
                ),
                (
                    "crates/hvf/tests/hvf_lifecycle.rs",
                    "fn applies_and_verifies_mixed_width_arm64_cpu_template_on_two_hvf_vcpus",
                ),
            ],
            documentation: &[(
                "compat/firecracker/v1.16.0/cpu-template-contract.md",
                "HVF startup and failure atomicity",
            )],
        },
        CpuTemplateHelperScenario::NativeV1NoTemplateSnapshot => ScenarioSpec {
            rationale: "Native-v1 creation rejects an effective custom template before capture or publication, and native-v1 load requires a pristine no-template destination; None or empty custom state retains that existing profile without serializing helper documents.",
            implementation: &[
                ("crates/runtime/src/lib.rs", "fn snapshot_v1_load_profile"),
                ("crates/runtime/src/lib.rs", "fn snapshot_v1_vm_profile"),
            ],
            validation: &[
                (
                    "crates/runtime/src/lib.rs",
                    "fn controller_native_v1_create_profile_is_fail_closed",
                ),
                (
                    "crates/runtime/src/lib.rs",
                    "fn successful_vm_configuration_actions_make_snapshot_load_non_fresh",
                ),
            ],
            documentation: &[
                (
                    "compat/firecracker/v1.16.0/cpu-template-contract.md",
                    "Snapshot boundary",
                ),
                ("docs/snapshot-feasibility.md", "Native V1 State Envelope"),
            ],
        },
        CpuTemplateHelperScenario::HeterogeneousFleetWorkflow => ScenarioSpec {
            rationale: "Applicable fleet work has a complete public creation, inspection, stripping, verification, and platform-tagged comparison workflow plus expert and platform guidance, without claiming an untested distinct-host safety, equivalence, migration, or snapshot-portability result.",
            implementation: &[
                ("tools/cpu-template-helper/src/cli.rs", "enum Command"),
                (
                    "tools/cpu-template-helper/src/fingerprint.rs",
                    "pub struct CpuFingerprintDocument",
                ),
            ],
            validation: &[(
                "tools/cpu-template-helper/tests/hvf_e2e.rs",
                "fn signed_all_operations_compose_canonical_artifacts",
            )],
            documentation: &[
                (
                    "docs/firecracker-compatibility.md",
                    "Arm64 CPU-Template Subset",
                ),
                (
                    "docs/firecracker-compatibility.md",
                    "CPU-Template Fingerprint Compare",
                ),
            ],
        },
    }
}

fn pinned_references(operation_id: &str, argument_ids: &[String]) -> Vec<Reference> {
    let mut ids = argument_ids
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(operation_id))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.into_iter()
        .map(|id| Reference::Local {
            path: "compat/firecracker/v1.16.0/source-manifest.json".to_string(),
            anchor: Some(id.to_string()),
        })
        .collect()
}

fn aggregate_implementation() -> Vec<Reference> {
    local_references(&[
        (
            "crates/hvf/src/cpu_template.rs",
            "pub(crate) fn capture_common_arm64_cpu_template_values",
        ),
        (
            "tools/cpu-template-helper/src/cli.rs",
            "pub fn run_cli_with_provider",
        ),
        (
            "tools/firecracker-capability-audit/src/cpu_template_helper_audit_validate.rs",
            "pub fn validate_cpu_template_helper_audit",
        ),
    ])
}

fn aggregate_validation() -> Vec<Reference> {
    local_references(&[
        (
            "crates/hvf/tests/guest_boot.rs",
            "fn two_cpu_linux_observes_exact_custom_id_register_mask_results",
        ),
        (
            "crates/hvf/tests/hvf_lifecycle.rs",
            "fn applies_and_verifies_mixed_width_arm64_cpu_template_on_two_hvf_vcpus",
        ),
        (
            "tools/cpu-template-helper/tests/hvf_e2e.rs",
            "fn signed_all_operations_compose_canonical_artifacts",
        ),
        (
            "tools/firecracker-capability-audit/tests/checked_inventory.rs",
            "fn checked_cpu_template_aggregate_compatibility_is_terminal_and_fail_closed",
        ),
        (
            "tools/firecracker-capability-audit/tests/cpu_template_helper_audit.rs",
            "fn checked_cpu_template_helper_audit_is_canonical_and_fail_closed",
        ),
    ])
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
