use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::inventory_phase::{InventoryPhase, classify_inventory_phase, disposition_counts};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, JailerAggregateAudit, JailerAggregateNonclaim, JailerArgumentCardinality,
    JailerArgumentOutcome, JailerArgumentRequirement, JailerEvidenceProfileId,
    JailerOperationOutcome, Reference, SourceManifest, ValidationErrors,
    jailer_aggregate_audit_json,
};

/// Current checked aggregate jailer authority schema.
pub const JAILER_AGGREGATE_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative aggregate jailer authority path.
pub const JAILER_AGGREGATE_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/jailer-aggregate-audit.json";
/// Exact capability transition owned by #1912.
pub const JAILER_AGGREGATE_CAPABILITY_IDS: [&str; 2] =
    ["corpus:jailer", "tool-operation:jailer/run"];

const UNRELATED_INVENTORY_SHA256: &str =
    "55b63f0633245792f8c8d02c8c6d0075d51cec407a335fc83d10a350f173d10e";

const PROFILE_IDS: [JailerEvidenceProfileId; 9] = [
    JailerEvidenceProfileId::GrammarAndEarlyCommands,
    JailerEvidenceProfileId::ValidationAndRedaction,
    JailerEvidenceProfileId::FixedCodeAndPublication,
    JailerEvidenceProfileId::ClosedProcessBoundary,
    JailerEvidenceProfileId::PrivateNamespaceAndCleanup,
    JailerEvidenceProfileId::ResourceLimits,
    JailerEvidenceProfileId::DaemonLifecycle,
    JailerEvidenceProfileId::TerminalIsolationLimits,
    JailerEvidenceProfileId::SignedGuestExecution,
];

const NONCLAIMS: [JailerAggregateNonclaim; 11] = [
    JailerAggregateNonclaim::LinuxJailerMechanismParity,
    JailerAggregateNonclaim::LiteralPerRunExecutableCopy,
    JailerAggregateNonclaim::NoSharedReadOnlyCodePages,
    JailerAggregateNonclaim::ArbitraryTrustedPathAuthority,
    JailerAggregateNonclaim::PositiveArbitraryCredentialTransition,
    JailerAggregateNonclaim::PositiveConfigurableChroot,
    JailerAggregateNonclaim::LinuxCgroupNamespaceOrDeviceNode,
    JailerAggregateNonclaim::ExternalVmnetConnectivity,
    JailerAggregateNonclaim::ProductionHostDeployment,
    JailerAggregateNonclaim::DeveloperIdOrNotarization,
    JailerAggregateNonclaim::AutomaticRestartOrLongLivedService,
];

/// Validate the complete checked aggregate jailer authority against the current tree.
pub fn validate_jailer_aggregate_audit(
    audit: &JailerAggregateAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header_and_sources(audit, manifest, inventory, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_arguments(audit, manifest, inventory, &mut errors);
    validate_operation_steps(audit, &mut errors);
    validate_corpus_sections(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence_profiles(audit, repository_root, &tracked, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if audit.nonclaims != NONCLAIMS {
        errors.push("jailer aggregate requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header_and_sources(
    audit: &JailerAggregateAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != JAILER_AGGREGATE_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "jailer aggregate schema_version must be {JAILER_AGGREGATE_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
        || audit.baseline != inventory.baseline
    {
        errors.push("jailer aggregate baseline is not the pinned release".to_string());
    }
    if audit.parent_issue != "#1351" || audit.delivery_issue != "#1912" {
        errors.push("jailer aggregate ownership must be #1351/#1912".to_string());
    }
    if audit
        .capability_ids
        .iter()
        .map(String::as_str)
        .ne(JAILER_AGGREGATE_CAPABILITY_IDS)
    {
        errors.push("jailer aggregate requires the exact two #1912 capabilities".to_string());
    }

    let expected = [
        (
            "jailer-document",
            Some("corpus:jailer"),
            "docs/jailer.md",
            "entire-file",
            "fa5e8b4ee769f64ee83a317dce5902ffd0029a1b",
        ),
        (
            "jailer-parser-and-entrypoint",
            Some("tool-operation:jailer/run"),
            "src/jailer/src/main.rs",
            "build_arg_parser",
            "4f87f2563c6f6ef47cecbb2829fe91bf27a6f603",
        ),
        (
            "jailer-operation",
            None,
            "src/jailer/src/env.rs",
            "Env::run",
            "cb3261c039cc6b83932c0bcdb9271faece107e0f",
        ),
        (
            "jailer-root-transition",
            None,
            "src/jailer/src/chroot.rs",
            "chroot",
            "56335c03a747067f81c297ad8b299837bae85d57",
        ),
    ];
    let actual = audit
        .upstream_sources
        .iter()
        .map(|source| {
            (
                source.id.as_str(),
                source.manifest_id.as_deref(),
                source.path.as_str(),
                source.anchor.as_str(),
                source.git_blob.as_str(),
            )
        })
        .collect::<Vec<_>>();
    if actual != expected {
        errors.push("jailer aggregate requires the exact ordered pinned sources".to_string());
    }

    let source_items = manifest
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let inputs = manifest
        .inputs
        .iter()
        .map(|input| (input.path.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    for (_, manifest_id, path, anchor, blob) in expected {
        if let Some(manifest_id) = manifest_id {
            match source_items.get(manifest_id) {
                Some(item) if item.path == path && item.anchor == anchor => {}
                Some(_) => errors.push(format!(
                    "jailer aggregate source identity drifted: {manifest_id}"
                )),
                None => errors.push(format!(
                    "jailer aggregate source identity is missing: {manifest_id}"
                )),
            }
        }
        match inputs.get(path) {
            Some(input) if input.git_blob == blob => {}
            Some(_) => errors.push(format!("jailer aggregate source blob drifted: {path}")),
            None => errors.push(format!("jailer aggregate source input is missing: {path}")),
        }
    }
}

fn validate_inventory_transition(
    audit: &JailerAggregateAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let previous = &audit.previous_counts;
    if (
        previous.implemented_and_verified,
        previous.audit_required,
        previous.missing_platform_feasible,
        previous.proven_platform_impossible,
    ) != (377, 5, 3, 33)
    {
        errors.push("jailer aggregate previous counts must be exactly 377/5/3/33".to_string());
    }
    let target = &audit.target_counts;
    if (
        target.implemented_and_verified,
        target.audit_required,
        target.missing_platform_feasible,
        target.proven_platform_impossible,
    ) != (379, 3, 3, 33)
    {
        errors.push("jailer aggregate target counts must be exactly 379/3/3/33".to_string());
    }
    if !matches!(
        disposition_counts(inventory),
        (379, 3, 3, 33) | (380, 3, 2, 33) | (381, 3, 1, 33)
    ) {
        errors.push(
            "jailer aggregate live inventory must be exactly 379/3/3/33, its 380/3/2/33 multiprocess successor, or its 381/3/1/33 host-resource successor"
                .to_string(),
        );
    }
    if !matches!(
        classify_inventory_phase(inventory),
        Ok(InventoryPhase::JailerAggregate
            | InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority)
    ) {
        errors.push("jailer aggregate live inventory has an inexact successor phase".to_string());
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push("jailer aggregate unrelated-inventory digest authority drifted".to_string());
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "jailer aggregate unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push("jailer aggregate unrelated inventory is not serializable".to_string()),
    }
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let unrelated = inventory
        .capabilities
        .iter()
        .filter(|capability| !JAILER_AGGREGATE_CAPABILITY_IDS.contains(&capability.id.as_str()))
        .collect::<Vec<&Capability>>();
    serde_json::to_vec(&unrelated).map(|bytes| {
        let hex_digit = |nibble: u8| {
            char::from(if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            })
        };
        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity(64);
        for byte in digest {
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
        encoded
    })
}

struct ArgumentSpec {
    id: &'static str,
    option: &'static str,
    requirement: JailerArgumentRequirement,
    cardinality: JailerArgumentCardinality,
    upstream_default: Option<&'static str>,
    outcome: JailerArgumentOutcome,
    profile: JailerEvidenceProfileId,
}

const ARGUMENTS: [ArgumentSpec; 13] = [
    ArgumentSpec {
        id: "tool-argument:jailer/id",
        option: "--id",
        requirement: JailerArgumentRequirement::Required,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ImplementedAndVerified,
        profile: JailerEvidenceProfileId::GrammarAndEarlyCommands,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/exec-file",
        option: "--exec-file",
        requirement: JailerArgumentRequirement::Required,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ImplementedAndVerified,
        profile: JailerEvidenceProfileId::FixedCodeAndPublication,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/uid",
        option: "--uid",
        requirement: JailerArgumentRequirement::Required,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/gid",
        option: "--gid",
        requirement: JailerArgumentRequirement::Required,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/chroot-base-dir",
        option: "--chroot-base-dir",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: Some("/srv/jailer"),
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/netns",
        option: "--netns",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/daemonize",
        option: "--daemonize",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::Flag,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ImplementedAndVerified,
        profile: JailerEvidenceProfileId::DaemonLifecycle,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/new-pid-ns",
        option: "--new-pid-ns",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::Flag,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/cgroup",
        option: "--cgroup",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::RepeatableValue,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/resource-limit",
        option: "--resource-limit",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::RepeatableValue,
        upstream_default: Some("no-file=2048"),
        outcome: JailerArgumentOutcome::ImplementedAndVerified,
        profile: JailerEvidenceProfileId::ResourceLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/cgroup-version",
        option: "--cgroup-version",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: Some("1"),
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/parent-cgroup",
        option: "--parent-cgroup",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::SingleValue,
        upstream_default: Some("exec-file basename"),
        outcome: JailerArgumentOutcome::ProvenPlatformImpossible,
        profile: JailerEvidenceProfileId::TerminalIsolationLimits,
    },
    ArgumentSpec {
        id: "tool-argument:jailer/version",
        option: "--version",
        requirement: JailerArgumentRequirement::Optional,
        cardinality: JailerArgumentCardinality::Flag,
        upstream_default: None,
        outcome: JailerArgumentOutcome::ImplementedAndVerified,
        profile: JailerEvidenceProfileId::GrammarAndEarlyCommands,
    },
];

fn validate_arguments(
    audit: &JailerAggregateAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.arguments.len() != ARGUMENTS.len() {
        errors.push("jailer aggregate requires exactly 13 ordered arguments".to_string());
    }
    let source_items = manifest
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for (index, (record, expected)) in audit.arguments.iter().zip(ARGUMENTS.iter()).enumerate() {
        if record.capability_id != expected.id
            || record.option != expected.option
            || record.requirement != expected.requirement
            || record.cardinality != expected.cardinality
            || record.upstream_default.as_deref() != expected.upstream_default
            || record.outcome != expected.outcome
            || record.evidence_profile != expected.profile
        {
            errors.push(format!(
                "jailer aggregate argument[{index}] does not match the exact parser record"
            ));
        }
        if !seen.insert(record.capability_id.as_str()) {
            errors.push(format!(
                "jailer aggregate contains a duplicate argument: {}",
                record.capability_id
            ));
        }
        match source_items.get(record.capability_id.as_str()) {
            Some(item)
                if item.kind == "tool-argument"
                    && item.path == "src/jailer/src/main.rs"
                    && item.anchor == record.option => {}
            Some(_) => errors.push(format!(
                "jailer aggregate argument source drifted: {}",
                record.capability_id
            )),
            None => errors.push(format!(
                "jailer aggregate argument source is missing: {}",
                record.capability_id
            )),
        }
        validate_argument_capability(
            record,
            capabilities.get(record.capability_id.as_str()),
            errors,
        );
    }
    if audit.arguments.len() > ARGUMENTS.len() {
        errors.push("jailer aggregate contains unknown argument records".to_string());
    }
}

fn validate_argument_capability(
    record: &crate::JailerArgumentRecord,
    capability: Option<&&Capability>,
    errors: &mut Vec<String>,
) {
    let Some(capability) = capability else {
        errors.push(format!(
            "jailer aggregate argument capability is missing: {}",
            record.capability_id
        ));
        return;
    };
    let expected = match record.outcome {
        JailerArgumentOutcome::ImplementedAndVerified => Disposition::ImplementedAndVerified,
        JailerArgumentOutcome::ProvenPlatformImpossible => Disposition::ProvenPlatformImpossible,
    };
    if capability.disposition != expected
        || capability.source_refs != [record.capability_id.as_str()]
        || capability.delivery_issue.is_some()
    {
        errors.push(format!(
            "jailer aggregate argument disposition or source ownership drifted: {}",
            record.capability_id
        ));
    }
    match record.outcome {
        JailerArgumentOutcome::ImplementedAndVerified
            if capability.implementation.is_empty()
                || capability.validation.is_empty()
                || capability.exclusion.is_some() =>
        {
            errors.push(format!(
                "jailer aggregate implemented argument lacks exact evidence: {}",
                record.capability_id
            ));
        }
        JailerArgumentOutcome::ProvenPlatformImpossible
            if capability.exclusion.is_none()
                || !capability.implementation.is_empty()
                || !capability.validation.is_empty() =>
        {
            errors.push(format!(
                "jailer aggregate platform argument lacks its terminal exclusion: {}",
                record.capability_id
            ));
        }
        _ => {}
    }
}

struct StepSpec {
    id: &'static str,
    anchor: &'static str,
    outcome: JailerOperationOutcome,
    profiles: &'static [JailerEvidenceProfileId],
}

const OPERATION_STEPS: [StepSpec; 16] = [
    StepSpec {
        id: "validate-paths-and-id",
        anchor: "Validate all provided paths and the VM ID",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[
            JailerEvidenceProfileId::GrammarAndEarlyCommands,
            JailerEvidenceProfileId::ValidationAndRedaction,
        ],
    },
    StepSpec {
        id: "close-inherited-file-descriptors",
        anchor: "Close all open file descriptors",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[JailerEvidenceProfileId::ClosedProcessBoundary],
    },
    StepSpec {
        id: "clear-inherited-environment",
        anchor: "Cleanup all environment variables",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[JailerEvidenceProfileId::ClosedProcessBoundary],
    },
    StepSpec {
        id: "create-private-runtime-root",
        anchor: "Create the <chroot_base>",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::PrivateNamespaceAndCleanup,
            JailerEvidenceProfileId::TerminalIsolationLimits,
        ],
    },
    StepSpec {
        id: "bind-isolated-worker-code",
        anchor: "Copy the file specified with --exec-file",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[JailerEvidenceProfileId::FixedCodeAndPublication],
    },
    StepSpec {
        id: "install-resource-limits",
        anchor: "Set resource bounds for current process",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[JailerEvidenceProfileId::ResourceLimits],
    },
    StepSpec {
        id: "configure-cgroups",
        anchor: "Create the cgroup sub-folders",
        outcome: JailerOperationOutcome::ProvenPlatformImpossible,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
    StepSpec {
        id: "enter-contained-root",
        anchor: "Call unshare() into a new mount namespace",
        outcome: JailerOperationOutcome::ProvenPlatformImpossible,
        profiles: &[
            JailerEvidenceProfileId::PrivateNamespaceAndCleanup,
            JailerEvidenceProfileId::TerminalIsolationLimits,
        ],
    },
    StepSpec {
        id: "provide-network-device-authority",
        anchor: "Use mknod to create a /dev/net/tun equivalent",
        outcome: JailerOperationOutcome::PlatformInapplicable,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
    StepSpec {
        id: "provide-hypervisor-authority",
        anchor: "Use mknod to create a /dev/kvm equivalent",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[JailerEvidenceProfileId::SignedGuestExecution],
    },
    StepSpec {
        id: "apply-root-and-device-ownership",
        anchor: "Use chown to change ownership",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
    StepSpec {
        id: "join-network-namespace",
        anchor: "If --netns <netns> is present",
        outcome: JailerOperationOutcome::ProvenPlatformImpossible,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
    StepSpec {
        id: "detach-session-and-standard-streams",
        anchor: "If --daemonize is specified",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[JailerEvidenceProfileId::DaemonLifecycle],
    },
    StepSpec {
        id: "create-pid-namespace",
        anchor: "If --new-pid-ns is specified",
        outcome: JailerOperationOutcome::ProvenPlatformImpossible,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
    StepSpec {
        id: "apply-process-identity",
        anchor: "Drop privileges via setting the provided uid and gid",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
    StepSpec {
        id: "execute-worker-with-owned-arguments",
        anchor: "Exec into",
        outcome: JailerOperationOutcome::ImplementedMacosOutcome,
        profiles: &[
            JailerEvidenceProfileId::GrammarAndEarlyCommands,
            JailerEvidenceProfileId::FixedCodeAndPublication,
            JailerEvidenceProfileId::SignedGuestExecution,
        ],
    },
];

fn validate_operation_steps(audit: &JailerAggregateAudit, errors: &mut Vec<String>) {
    if audit.operation_steps.len() != OPERATION_STEPS.len() {
        errors.push("jailer aggregate requires exactly 16 ordered operation steps".to_string());
    }
    let mut seen = BTreeSet::new();
    for (index, (record, expected)) in audit
        .operation_steps
        .iter()
        .zip(OPERATION_STEPS.iter())
        .enumerate()
    {
        if usize::from(record.order) != index + 1
            || record.id != expected.id
            || record.upstream_anchor != expected.anchor
            || record.outcome != expected.outcome
            || record.evidence_profiles != expected.profiles
        {
            errors.push(format!(
                "jailer aggregate operation step[{index}] does not match the exact ordered operation"
            ));
        }
        if !seen.insert(record.id.as_str()) {
            errors.push(format!(
                "jailer aggregate contains a duplicate operation step: {}",
                record.id
            ));
        }
    }
    if audit.operation_steps.len() > OPERATION_STEPS.len() {
        errors.push("jailer aggregate contains unknown operation steps".to_string());
    }
}

struct SectionSpec {
    id: &'static str,
    anchor: &'static str,
    outcome: JailerOperationOutcome,
    profiles: &'static [JailerEvidenceProfileId],
}

const CORPUS_SECTIONS: [SectionSpec; 7] = [
    SectionSpec {
        id: "disclaimer",
        anchor: "## Disclaimer",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::FixedCodeAndPublication,
            JailerEvidenceProfileId::TerminalIsolationLimits,
        ],
    },
    SectionSpec {
        id: "usage",
        anchor: "## Jailer Usage",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::GrammarAndEarlyCommands,
            JailerEvidenceProfileId::TerminalIsolationLimits,
        ],
    },
    SectionSpec {
        id: "operation",
        anchor: "## Jailer Operation",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::ClosedProcessBoundary,
            JailerEvidenceProfileId::PrivateNamespaceAndCleanup,
            JailerEvidenceProfileId::ResourceLimits,
            JailerEvidenceProfileId::DaemonLifecycle,
        ],
    },
    SectionSpec {
        id: "example-run-and-notes",
        anchor: "## Example Run and Notes",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::GrammarAndEarlyCommands,
            JailerEvidenceProfileId::SignedGuestExecution,
        ],
    },
    SectionSpec {
        id: "observations",
        anchor: "### Observations",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::ValidationAndRedaction,
            JailerEvidenceProfileId::PrivateNamespaceAndCleanup,
            JailerEvidenceProfileId::TerminalIsolationLimits,
        ],
    },
    SectionSpec {
        id: "known-limitations",
        anchor: "### Known limitations",
        outcome: JailerOperationOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            JailerEvidenceProfileId::DaemonLifecycle,
            JailerEvidenceProfileId::TerminalIsolationLimits,
        ],
    },
    SectionSpec {
        id: "caveats",
        anchor: "## Caveats",
        outcome: JailerOperationOutcome::PlatformInapplicable,
        profiles: &[JailerEvidenceProfileId::TerminalIsolationLimits],
    },
];

fn validate_corpus_sections(audit: &JailerAggregateAudit, errors: &mut Vec<String>) {
    let actual = audit
        .corpus_sections
        .iter()
        .map(|section| {
            (
                section.id.as_str(),
                section.upstream_anchor.as_str(),
                section.outcome,
                section.evidence_profiles.as_slice(),
            )
        })
        .collect::<Vec<_>>();
    let expected = CORPUS_SECTIONS
        .iter()
        .map(|section| {
            (
                section.id,
                section.anchor,
                section.outcome,
                section.profiles,
            )
        })
        .collect::<Vec<_>>();
    if actual != expected {
        errors.push("jailer aggregate requires the exact seven corpus sections".to_string());
    }
}

fn validate_evidence_profiles(
    audit: &JailerAggregateAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let ids = audit
        .evidence_profiles
        .iter()
        .map(|profile| profile.id)
        .collect::<Vec<_>>();
    if ids != PROFILE_IDS {
        errors.push("jailer aggregate requires the exact ordered evidence profiles".to_string());
    }
    for profile in &audit.evidence_profiles {
        let (implementation, validation) = expected_evidence(profile.id);
        validate_reference_set(
            &profile.implementation,
            implementation,
            repository_root,
            tracked,
            &format!("jailer aggregate {:?} implementation", profile.id),
            errors,
        );
        validate_reference_set(
            &profile.validation,
            validation,
            repository_root,
            tracked,
            &format!("jailer aggregate {:?} validation", profile.id),
            errors,
        );
    }

    let declared = audit
        .evidence_profiles
        .iter()
        .map(|profile| profile.id)
        .collect::<BTreeSet<_>>();
    let referenced = audit
        .arguments
        .iter()
        .map(|argument| argument.evidence_profile)
        .chain(
            audit
                .operation_steps
                .iter()
                .flat_map(|step| step.evidence_profiles.iter().copied()),
        )
        .chain(
            audit
                .corpus_sections
                .iter()
                .flat_map(|section| section.evidence_profiles.iter().copied()),
        )
        .collect::<BTreeSet<_>>();
    if declared != referenced || declared != PROFILE_IDS.into_iter().collect() {
        errors.push(
            "jailer aggregate evidence profile coverage is not a closed bijection".to_string(),
        );
    }
}

type LocalReferenceSpec = (&'static str, &'static str);

fn expected_evidence(
    id: JailerEvidenceProfileId,
) -> (&'static [LocalReferenceSpec], &'static [LocalReferenceSpec]) {
    match id {
        JailerEvidenceProfileId::GrammarAndEarlyCommands => (
            &[("crates/launcher/src/launch_policy.rs", "fn parse_jailer(")],
            &[
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn parses_exact_policy_and_injects_owned_arguments()",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn rejects_missing_duplicate_unknown_and_forwarded_inputs()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn launcher_exposes_exact_jailer_help_version_and_policy_validation()",
                ),
            ],
        ),
        JailerEvidenceProfileId::ValidationAndRedaction => (
            &[
                (
                    "crates/launcher/src/error.rs",
                    "pub enum JailerIsolationArgument",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn unsupported_jailer_isolation_argument(",
                ),
            ],
            &[
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn rejects_named_unsupported_isolation_before_consuming_values()",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn unsupported_isolation_names_are_exact_and_pre_delimiter_only()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_rejects_unsupported_isolation_before_grants_sessions_and_worker()",
                ),
            ],
        ),
        JailerEvidenceProfileId::FixedCodeAndPublication => (
            &[
                (
                    "crates/launcher/src/macos/code_sign.rs",
                    "pub(crate) fn validate_bundle(",
                ),
                (
                    "crates/launcher/src/macos/code_sign.rs",
                    "pub(crate) fn validate_worker_process(",
                ),
                (
                    "crates/launcher/src/macos/publish.rs",
                    "pub(crate) fn publish_exclusive(",
                ),
                ("crates/launcher/src/package.rs", "fn assemble_bundle_with("),
                ("crates/launcher/src/supervisor.rs", "fn launch_prepared("),
            ],
            &[
                (
                    "crates/launcher/src/package.rs",
                    "fn assembles_signs_inspects_and_publishes_fixed_layout()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn launcher_rejects_modified_missing_or_wrongly_signed_worker_before_execution()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn production_bundle_has_exact_nested_signing_contract()",
                ),
            ],
        ),
        JailerEvidenceProfileId::ClosedProcessBoundary => (
            &[
                ("crates/launcher/src/macos/spawn.rs", "fn environment()"),
                (
                    "crates/launcher/src/macos/spawn.rs",
                    "libc::POSIX_SPAWN_CLOEXEC_DEFAULT",
                ),
            ],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
            )],
        ),
        JailerEvidenceProfileId::PrivateNamespaceAndCleanup => (
            &[
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn cleanup_namespace_after_worker(",
                ),
                (
                    "crates/session/src/macos/runtime.rs",
                    "pub fn create(session: SessionId)",
                ),
                (
                    "crates/session/src/macos/runtime.rs",
                    "pub struct WorkerNamespace",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_granted_socket_cleanup_preserves_replacements_in_both_death_orders()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_grant_scopes_cleanup_across_both_process_crash_orders()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
            ],
        ),
        JailerEvidenceProfileId::ResourceLimits => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "fn install_resource_limit(",
                ),
                (
                    "crates/bangbang/src/contained_session.rs",
                    "fn install_worker_policy(",
                ),
                ("crates/launcher/src/launch_policy.rs", "fn parse_jailer("),
            ],
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "fn exact_limit_never_raises_the_inherited_hard_limit()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
            ],
        ),
        JailerEvidenceProfileId::DaemonLifecycle => (
            &[
                (
                    "crates/launcher/src/macos/daemon.rs",
                    "pub(crate) fn launch_parent(",
                ),
                (
                    "crates/launcher/src/macos/spawn.rs",
                    "pub(crate) fn spawn_daemon_suspended(",
                ),
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn wait_session_inner(",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_daemon_handoff_waits_for_ready_and_keeps_concurrent_supervisors_isolated()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_daemon_parent_loss_before_ack_cancels_worker_and_private_state()",
                ),
            ],
        ),
        JailerEvidenceProfileId::TerminalIsolationLimits => (
            &[
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Certified Linux runtime isolation exclusions",
                ),
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Terminal jailer configurable-chroot platform limit",
                ),
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Terminal jailer uid/gid platform limit",
                ),
                (
                    "crates/launcher/src/error.rs",
                    "pub enum JailerIsolationArgument",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn unsupported_jailer_isolation_argument(",
                ),
            ],
            &[
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn launch_identity_classifier_covers_current_and_exact_root_targets()",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn rejects_named_unsupported_isolation_before_consuming_values()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_rejects_unsupported_isolation_before_grants_sessions_and_worker()",
                ),
            ],
        ),
        JailerEvidenceProfileId::SignedGuestExecution => (
            &[("crates/launcher/src/supervisor.rs", "fn launch_prepared(")],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn launcher_runs_real_sandboxed_hvf_guest_to_system_off()",
            )],
        ),
    }
}

fn validate_reference_set(
    references: &[Reference],
    expected: &[LocalReferenceSpec],
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    label: &str,
    errors: &mut Vec<String>,
) {
    if references
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left >= right))
    {
        errors.push(format!("{label} references must be unique and sorted"));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("{label}[{index}]"),
            errors,
        );
        match reference {
            Reference::Local {
                path,
                anchor: Some(anchor),
            } => match std::fs::read_to_string(repository_root.join(path)) {
                Ok(contents) if contents.contains(anchor) => {}
                Ok(_) => errors.push(format!(
                    "local reference anchor is absent: {label}[{index}]"
                )),
                Err(_) => {}
            },
            Reference::Local { anchor: None, .. }
            | Reference::Github { .. }
            | Reference::Authoritative { .. } => {
                errors.push(format!(
                    "{label}[{index}] must be an anchored local reference"
                ));
            }
        }
    }
    let actual = references
        .iter()
        .filter_map(|reference| match reference {
            Reference::Local {
                path,
                anchor: Some(anchor),
            } => Some((path.as_str(), anchor.as_str())),
            Reference::Local { anchor: None, .. }
            | Reference::Github { .. }
            | Reference::Authoritative { .. } => None,
        })
        .collect::<Vec<_>>();
    if actual != expected {
        errors.push(format!("{label} must match its exact path and anchor set"));
    }
}

fn validate_canonical_bytes(
    audit: &JailerAggregateAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match jailer_aggregate_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize jailer aggregate audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(JAILER_AGGREGATE_AUDIT_PATH)) {
        Ok(bytes) if bytes == canonical => {}
        Ok(_) => errors.push("checked jailer aggregate audit is not canonical JSON".to_string()),
        Err(_) => errors.push("checked jailer aggregate audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_argument_and_operation_populations_are_closed() {
        assert_eq!(ARGUMENTS.len(), 13);
        assert_eq!(OPERATION_STEPS.len(), 16);
        assert_eq!(CORPUS_SECTIONS.len(), 7);
        assert_eq!(PROFILE_IDS.len(), 9);
        assert_eq!(NONCLAIMS.len(), 11);
    }
}
