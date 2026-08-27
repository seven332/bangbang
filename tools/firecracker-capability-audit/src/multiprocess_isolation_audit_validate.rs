use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::inventory_phase::{
    InventoryPhase, MULTIPROCESS_ISOLATION_ID, classify_inventory_phase, disposition_counts,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, MultiprocessClauseOutcome, MultiprocessEvidenceProfileId,
    MultiprocessIsolationAudit, MultiprocessIsolationNonclaim, MultiprocessResidualClassification,
    Reference, SourceManifest, ValidationErrors, multiprocess_isolation_audit_json,
};

/// Current checked multiprocess isolation authority schema.
pub const MULTIPROCESS_ISOLATION_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative multiprocess isolation authority path.
pub const MULTIPROCESS_ISOLATION_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/multiprocess-isolation-audit.json";
/// Exact capability transition owned by #1914.
pub const MULTIPROCESS_ISOLATION_CAPABILITY_ID: &str = MULTIPROCESS_ISOLATION_ID;

const UNRELATED_INVENTORY_SHA256: &str =
    "8ef0d3602893599ca24feaab9039cbd8b2b27d553813c953b88ca314caa151b7";

const PROFILE_IDS: [MultiprocessEvidenceProfileId; 8] = [
    MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
    MultiprocessEvidenceProfileId::LifecycleIdentityAndRedaction,
    MultiprocessEvidenceProfileId::AtomicResourceAuthority,
    MultiprocessEvidenceProfileId::CrashCancellationAndRecovery,
    MultiprocessEvidenceProfileId::ReplacementSafePublication,
    MultiprocessEvidenceProfileId::ConcurrentNoninterchangeability,
    MultiprocessEvidenceProfileId::TerminalIdentityLimit,
    MultiprocessEvidenceProfileId::OperatorBoundary,
];

const NONCLAIMS: [MultiprocessIsolationNonclaim; 10] = [
    MultiprocessIsolationNonclaim::LinuxJailerMechanismParity,
    MultiprocessIsolationNonclaim::GeneralDynamicResourceBroker,
    MultiprocessIsolationNonclaim::HardRevocation,
    MultiprocessIsolationNonclaim::ImmediateZeroSnapshotCreateWindow,
    MultiprocessIsolationNonclaim::ImmediateZeroResidueAfterDualDeath,
    MultiprocessIsolationNonclaim::MaliciousSameBundleSiblingIsolation,
    MultiprocessIsolationNonclaim::PositiveUniqueUidGidPerInstance,
    MultiprocessIsolationNonclaim::AutomaticRestartOrReconnect,
    MultiprocessIsolationNonclaim::GlobalCrossLauncherPathAllocation,
    MultiprocessIsolationNonclaim::ProductionVmnetOrHostDeployment,
];

/// Validate the complete checked multiprocess isolation authority against the current tree.
pub fn validate_multiprocess_isolation_audit(
    audit: &MultiprocessIsolationAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header_and_sources(audit, manifest, inventory, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_source_clauses(audit, &mut errors);
    validate_terminal_dependencies(audit, inventory, &mut errors);
    validate_residuals(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence_profiles(audit, repository_root, &tracked, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if audit.nonclaims != NONCLAIMS {
        errors.push("multiprocess isolation requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header_and_sources(
    audit: &MultiprocessIsolationAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != MULTIPROCESS_ISOLATION_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "multiprocess isolation schema_version must be {MULTIPROCESS_ISOLATION_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
        || audit.baseline != inventory.baseline
    {
        errors.push("multiprocess isolation baseline is not the pinned release".to_string());
    }
    if audit.parent_issue != "#1351" || audit.delivery_issue != "#1914" {
        errors.push("multiprocess isolation ownership must be #1351/#1914".to_string());
    }
    if audit.capability_id != MULTIPROCESS_ISOLATION_CAPABILITY_ID {
        errors.push("multiprocess isolation requires the exact #1914 capability".to_string());
    }

    let expected = [
        (
            "firecracker-design",
            "corpus:design",
            "docs/design.md",
            "entire-file",
            "143fef76410e4f7e45b32d3986e0d78eedf5175a",
        ),
        (
            "production-host-setup",
            "corpus:production-host",
            "docs/prod-host-setup.md",
            "entire-file",
            "8939b56a965963d8df1c44c583dcd38361197347",
        ),
    ];
    let actual = audit
        .upstream_sources
        .iter()
        .map(|source| {
            (
                source.id.as_str(),
                source.manifest_id.as_str(),
                source.path.as_str(),
                source.anchor.as_str(),
                source.git_blob.as_str(),
            )
        })
        .collect::<Vec<_>>();
    if actual != expected {
        errors.push("multiprocess isolation requires the exact ordered pinned sources".to_string());
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
        match source_items.get(manifest_id) {
            Some(item) if item.path == path && item.anchor == anchor => {}
            Some(_) => errors.push(format!(
                "multiprocess isolation source identity drifted: {manifest_id}"
            )),
            None => errors.push(format!(
                "multiprocess isolation source identity is missing: {manifest_id}"
            )),
        }
        match inputs.get(path) {
            Some(input) if input.git_blob == blob => {}
            Some(_) => errors.push(format!(
                "multiprocess isolation source blob drifted: {path}"
            )),
            None => errors.push(format!(
                "multiprocess isolation source input is missing: {path}"
            )),
        }
    }
}

fn validate_inventory_transition(
    audit: &MultiprocessIsolationAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let previous = &audit.previous_counts;
    if (
        previous.implemented_and_verified,
        previous.audit_required,
        previous.missing_platform_feasible,
        previous.proven_platform_impossible,
    ) != (379, 3, 3, 33)
    {
        errors
            .push("multiprocess isolation previous counts must be exactly 379/3/3/33".to_string());
    }
    let target = &audit.target_counts;
    if (
        target.implemented_and_verified,
        target.audit_required,
        target.missing_platform_feasible,
        target.proven_platform_impossible,
    ) != (380, 3, 2, 33)
    {
        errors.push("multiprocess isolation target counts must be exactly 380/3/2/33".to_string());
    }
    if !matches!(
        disposition_counts(inventory),
        (380, 3, 2, 33) | (381, 3, 1, 33) | (382, 3, 0, 33) | (383, 2, 0, 33) | (383, 0, 2, 33)
    ) {
        errors.push(
            "multiprocess isolation live inventory must be exactly 380/3/2/33 or one of its exact successors through 383/0/2/33 vmnet feasibility"
                .to_string(),
        );
    }
    if !matches!(
        classify_inventory_phase(inventory),
        Ok(InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility)
    ) {
        errors.push(
            "multiprocess isolation live inventory has an inexact successor phase".to_string(),
        );
    }

    match inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == MULTIPROCESS_ISOLATION_CAPABILITY_ID)
    {
        Some(capability)
            if capability.family == "isolation"
                && capability.source_refs == ["corpus:design", "corpus:production-host"]
                && capability.disposition == Disposition::ImplementedAndVerified
                && !capability.implementation.is_empty()
                && !capability.validation.is_empty()
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none() => {}
        Some(_) => errors.push(
            "multiprocess isolation capability is not terminal with exact ownership".to_string(),
        ),
        None => errors.push("multiprocess isolation capability is missing".to_string()),
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push(
            "multiprocess isolation unrelated-inventory digest authority drifted".to_string(),
        );
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "multiprocess isolation unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push(
            "multiprocess isolation unrelated inventory is not serializable".to_string(),
        ),
    }
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let unrelated = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.id != MULTIPROCESS_ISOLATION_CAPABILITY_ID)
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

struct ClauseSpec {
    id: &'static str,
    source_id: &'static str,
    anchor: &'static str,
    outcome: MultiprocessClauseOutcome,
    profiles: &'static [MultiprocessEvidenceProfileId],
}

const SOURCE_CLAUSES: [ClauseSpec; 13] = [
    ClauseSpec {
        id: "different-customer-multitenancy",
        source_id: "firecracker-design",
        anchor: "Firecracker can safely run workloads from different customers on the same machine.",
        outcome: MultiprocessClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
            MultiprocessEvidenceProfileId::ConcurrentNoninterchangeability,
            MultiprocessEvidenceProfileId::TerminalIdentityLimit,
        ],
    },
    ClauseSpec {
        id: "simultaneous-microvms-resource-bounded",
        source_id: "firecracker-design",
        anchor: "The number of Firecracker microVMs running simultaneously on a host is limited only by the availability of hardware resources.",
        outcome: MultiprocessClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
            MultiprocessEvidenceProfileId::OperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "one-process-one-microvm",
        source_id: "firecracker-design",
        anchor: "Each Firecracker process encapsulates one and only one microVM.",
        outcome: MultiprocessClauseOutcome::ImplementedMacosOutcome,
        profiles: &[MultiprocessEvidenceProfileId::ProcessPerVmBoundary],
    },
    ClauseSpec {
        id: "malicious-vcpu-trust-zones",
        source_id: "firecracker-design",
        anchor: "all vCPU threads are considered to be running malicious code",
        outcome: MultiprocessClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
            MultiprocessEvidenceProfileId::LifecycleIdentityAndRedaction,
            MultiprocessEvidenceProfileId::TerminalIdentityLimit,
        ],
    },
    ClauseSpec {
        id: "process-level-defense-in-depth",
        source_id: "firecracker-design",
        anchor: "To assure defense in depth, Firecracker should only run constrained at the process level.",
        outcome: MultiprocessClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
            MultiprocessEvidenceProfileId::LifecycleIdentityAndRedaction,
            MultiprocessEvidenceProfileId::TerminalIdentityLimit,
        ],
    },
    ClauseSpec {
        id: "privileged-third-party-resource-grants",
        source_id: "firecracker-design",
        anchor: "Firecracker can only access resources that a privileged third-party grants access to",
        outcome: MultiprocessClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            MultiprocessEvidenceProfileId::LifecycleIdentityAndRedaction,
            MultiprocessEvidenceProfileId::AtomicResourceAuthority,
        ],
    },
    ClauseSpec {
        id: "per-microvm-fair-resource-controls",
        source_id: "firecracker-design",
        anchor: "each Firecracker microVM can have its own dedicated quota of the CPU time",
        outcome: MultiprocessClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::AtomicResourceAuthority,
            MultiprocessEvidenceProfileId::OperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "external-overwatcher-sigkill",
        source_id: "production-host-setup",
        anchor: "customers have an overwatcher process on the host",
        outcome: MultiprocessClauseOutcome::OperatorOwnedRecommendation,
        profiles: &[
            MultiprocessEvidenceProfileId::CrashCancellationAndRecovery,
            MultiprocessEvidenceProfileId::OperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "jailer-or-equivalent-constraints",
        source_id: "production-host-setup",
        anchor: "process constraints equal or more restrictive than those in the jailer",
        outcome: MultiprocessClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
            MultiprocessEvidenceProfileId::AtomicResourceAuthority,
            MultiprocessEvidenceProfileId::TerminalIdentityLimit,
        ],
    },
    ClauseSpec {
        id: "trusted-jailer-inputs",
        source_id: "production-host-setup",
        anchor: "The jailer treats all its inputs as trusted",
        outcome: MultiprocessClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            MultiprocessEvidenceProfileId::AtomicResourceAuthority,
            MultiprocessEvidenceProfileId::ReplacementSafePublication,
            MultiprocessEvidenceProfileId::OperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "dedicated-identity-and-unique-instance-layer",
        source_id: "production-host-setup",
        anchor: "When running multiple Firecracker instances it is recommended that each runs with its unique `uid` and `gid`",
        outcome: MultiprocessClauseOutcome::ComposedTerminalPlatformLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::AtomicResourceAuthority,
            MultiprocessEvidenceProfileId::TerminalIdentityLimit,
        ],
    },
    ClauseSpec {
        id: "operator-workload-resource-controls",
        source_id: "production-host-setup",
        anchor: "use the provided `resource-limits` and `cgroup` functionalities",
        outcome: MultiprocessClauseOutcome::IndependentlyOwnedOutcome,
        profiles: &[
            MultiprocessEvidenceProfileId::AtomicResourceAuthority,
            MultiprocessEvidenceProfileId::OperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "single-tenant-process-boundary",
        source_id: "production-host-setup",
        anchor: "each Firecracker process corresponds to a workload of a single tenant.",
        outcome: MultiprocessClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            MultiprocessEvidenceProfileId::ProcessPerVmBoundary,
            MultiprocessEvidenceProfileId::ConcurrentNoninterchangeability,
            MultiprocessEvidenceProfileId::TerminalIdentityLimit,
        ],
    },
];

fn validate_source_clauses(audit: &MultiprocessIsolationAudit, errors: &mut Vec<String>) {
    if audit.source_clauses.len() != SOURCE_CLAUSES.len() {
        errors
            .push("multiprocess isolation requires exactly 13 ordered source clauses".to_string());
    }
    let mut seen = BTreeSet::new();
    for (index, (record, expected)) in audit
        .source_clauses
        .iter()
        .zip(SOURCE_CLAUSES.iter())
        .enumerate()
    {
        if usize::from(record.order) != index + 1
            || record.id != expected.id
            || record.source_id != expected.source_id
            || record.upstream_anchor != expected.anchor
            || record.outcome != expected.outcome
            || record.evidence_profiles != expected.profiles
        {
            errors.push(format!(
                "multiprocess isolation source clause[{index}] does not match the exact ordered obligation"
            ));
        }
        if !seen.insert(record.id.as_str()) {
            errors.push(format!(
                "multiprocess isolation contains a duplicate source clause: {}",
                record.id
            ));
        }
    }
    if audit.source_clauses.len() > SOURCE_CLAUSES.len() {
        errors.push("multiprocess isolation contains unknown source clauses".to_string());
    }
}

const TERMINAL_DEPENDENCIES: [(&str, Disposition); 5] = [
    ("corpus:jailer", Disposition::ImplementedAndVerified),
    (
        "semantic.process:signals-exits-fd-and-cleanup",
        Disposition::ImplementedAndVerified,
    ),
    (
        "tool-argument:jailer/gid",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/uid",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-operation:jailer/run",
        Disposition::ImplementedAndVerified,
    ),
];

fn validate_terminal_dependencies(
    audit: &MultiprocessIsolationAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .terminal_dependencies
        .iter()
        .map(|dependency| (dependency.capability_id.as_str(), dependency.disposition))
        .collect::<Vec<_>>();
    if actual != TERMINAL_DEPENDENCIES {
        errors.push("multiprocess isolation requires the exact terminal dependencies".to_string());
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (id, disposition) in TERMINAL_DEPENDENCIES {
        let Some(capability) = capabilities.get(id) else {
            errors.push(format!(
                "multiprocess isolation terminal dependency is missing: {id}"
            ));
            continue;
        };
        let evidence_is_terminal = match disposition {
            Disposition::ImplementedAndVerified => {
                !capability.implementation.is_empty()
                    && !capability.validation.is_empty()
                    && capability.exclusion.is_none()
            }
            Disposition::ProvenPlatformImpossible => {
                capability.implementation.is_empty()
                    && capability.validation.is_empty()
                    && capability.exclusion.is_some()
            }
            Disposition::AuditRequired | Disposition::MissingPlatformFeasible => false,
        };
        if capability.disposition != disposition
            || capability.delivery_issue.is_some()
            || !evidence_is_terminal
        {
            errors.push(format!(
                "multiprocess isolation dependency is not terminal with exact evidence: {id}"
            ));
        }
    }
}

const RESIDUALS: [(
    &str,
    MultiprocessResidualClassification,
    MultiprocessEvidenceProfileId,
); 7] = [
    (
        "general-dynamic-resource-races",
        MultiprocessResidualClassification::GenericNonrequirement,
        MultiprocessEvidenceProfileId::AtomicResourceAuthority,
    ),
    (
        "hard-revocation",
        MultiprocessResidualClassification::GenericNonrequirement,
        MultiprocessEvidenceProfileId::AtomicResourceAuthority,
    ),
    (
        "snapshot-create-before-record",
        MultiprocessResidualClassification::ImplementationSpecificNonclaim,
        MultiprocessEvidenceProfileId::ReplacementSafePublication,
    ),
    (
        "simultaneous-uncatchable-death-residue",
        MultiprocessResidualClassification::ImplementationSpecificNonclaim,
        MultiprocessEvidenceProfileId::CrashCancellationAndRecovery,
    ),
    (
        "malicious-same-container-sibling",
        MultiprocessResidualClassification::TerminalPlatformLimit,
        MultiprocessEvidenceProfileId::TerminalIdentityLimit,
    ),
    (
        "automatic-restart-reconnect",
        MultiprocessResidualClassification::OperatorOwnedNonrequirement,
        MultiprocessEvidenceProfileId::OperatorBoundary,
    ),
    (
        "global-cross-launcher-path-coordination",
        MultiprocessResidualClassification::OperatorOwnedNonrequirement,
        MultiprocessEvidenceProfileId::OperatorBoundary,
    ),
];

fn validate_residuals(audit: &MultiprocessIsolationAudit, errors: &mut Vec<String>) {
    let actual = audit
        .residuals
        .iter()
        .map(|residual| {
            (
                residual.id.as_str(),
                residual.classification,
                residual.evidence_profile,
            )
        })
        .collect::<Vec<_>>();
    if actual != RESIDUALS {
        errors
            .push("multiprocess isolation requires the exact residual classifications".to_string());
    }
}

fn validate_evidence_profiles(
    audit: &MultiprocessIsolationAudit,
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
        errors.push(
            "multiprocess isolation requires the exact ordered evidence profiles".to_string(),
        );
    }
    for profile in &audit.evidence_profiles {
        let (implementation, validation) = expected_evidence(profile.id);
        validate_reference_set(
            &profile.implementation,
            implementation,
            repository_root,
            tracked,
            &format!("multiprocess isolation {:?} implementation", profile.id),
            errors,
        );
        validate_reference_set(
            &profile.validation,
            validation,
            repository_root,
            tracked,
            &format!("multiprocess isolation {:?} validation", profile.id),
            errors,
        );
    }

    let declared = audit
        .evidence_profiles
        .iter()
        .map(|profile| profile.id)
        .collect::<BTreeSet<_>>();
    let referenced = audit
        .source_clauses
        .iter()
        .flat_map(|clause| clause.evidence_profiles.iter().copied())
        .chain(
            audit
                .residuals
                .iter()
                .map(|residual| residual.evidence_profile),
        )
        .collect::<BTreeSet<_>>();
    if declared != referenced || declared != PROFILE_IDS.into_iter().collect() {
        errors.push(
            "multiprocess isolation evidence profile coverage is not a closed bijection"
                .to_string(),
        );
    }
}

type LocalReferenceSpec = (&'static str, &'static str);

fn expected_evidence(
    id: MultiprocessEvidenceProfileId,
) -> (&'static [LocalReferenceSpec], &'static [LocalReferenceSpec]) {
    match id {
        MultiprocessEvidenceProfileId::ProcessPerVmBoundary => (
            &[
                ("crates/launcher/src/supervisor.rs", "fn launch_prepared("),
                (
                    "crates/session/src/macos/runtime.rs",
                    "pub struct WorkerNamespace",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn concurrent_sessions_remain_independent_when_one_worker_crashes()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn launcher_runs_real_sandboxed_hvf_guest_to_system_off()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::LifecycleIdentityAndRedaction => (
            &[
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn wait_session_inner(",
                ),
                ("crates/session/src/codec.rs", "pub enum Message"),
                (
                    "crates/session/src/state.rs",
                    "pub struct LauncherLifecycle",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn worker_rejects_malformed_forged_bootstrap_before_public_processing()",
                ),
                (
                    "crates/session/src/codec.rs",
                    "fn session_debug_and_errors_are_redacted()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::AtomicResourceAuthority => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "pub(crate) struct GrantAuthority",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "pub struct GrantRegistry",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "pub struct StagedGrantBatch",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_grant_mismatch_fails_closed_without_mutation()",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "fn directory_registry_batch_adoption_is_failure_atomic()",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "fn file_registry_batch_adoption_is_failure_atomic()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::CrashCancellationAndRecovery => (
            &[
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn cleanup_namespace_after_worker(",
                ),
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn wait_session_inner(",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn incomplete_grant_phase_obeys_one_absolute_deadline()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn launcher_first_and_both_killed_orders_follow_namespace_ownership()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signal_cancels_an_incomplete_grant_phase_without_waiting_for_timeout()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_grant_scopes_cleanup_across_both_process_crash_orders()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::ReplacementSafePublication => (
            &[
                (
                    "crates/session/src/macos/runtime.rs",
                    "pub struct SnapshotStagingOwnershipRecord",
                ),
                (
                    "crates/session/src/macos/runtime.rs",
                    "pub struct SocketOwnershipRecord",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn grant_test_bundle_recovers_recorded_snapshot_staging_after_worker_sigkill()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_granted_socket_cleanup_preserves_replacements_in_both_death_orders()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_restore_transaction_preserves_a_published_socket_replacement()",
                ),
                (
                    "crates/session/src/macos/runtime.rs",
                    "fn staged_socket_cleanup_requires_the_recorded_identity()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::ConcurrentNoninterchangeability => (
            &[
                ("crates/launcher/src/supervisor.rs", "fn launch_prepared("),
                (
                    "crates/session/src/state.rs",
                    "pub struct LauncherLifecycle",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn concurrent_sessions_remain_independent_when_one_worker_crashes()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn concurrent_signed_grant_sessions_keep_authority_noninterchangeable()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn concurrent_signed_restore_transactions_keep_same_ids_noninterchangeable()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::TerminalIdentityLimit => (
            &[
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Terminal jailer uid/gid platform limit",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn classify_launch_identity(",
                ),
                (
                    "docs/security.md",
                    "## Jailer uid/gid fixed-topology platform limit",
                ),
            ],
            &[
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn launch_identity_classifier_covers_current_and_exact_root_targets()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
            ],
        ),
        MultiprocessEvidenceProfileId::OperatorBoundary => (
            &[
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Remaining External Isolation Work",
                ),
                ("docs/security.md", "## Multi-Process Operation"),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn concurrent_sessions_remain_independent_when_one_worker_crashes()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
            ],
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
    audit: &MultiprocessIsolationAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match multiprocess_isolation_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize multiprocess isolation audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(MULTIPROCESS_ISOLATION_AUDIT_PATH)) {
        Ok(bytes) if bytes == canonical => {}
        Ok(_) => {
            errors.push("checked multiprocess isolation audit is not canonical JSON".to_string())
        }
        Err(_) => errors.push("checked multiprocess isolation audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_multiprocess_populations_are_closed() {
        assert_eq!(SOURCE_CLAUSES.len(), 13);
        assert_eq!(TERMINAL_DEPENDENCIES.len(), 5);
        assert_eq!(PROFILE_IDS.len(), 8);
        assert_eq!(RESIDUALS.len(), 7);
        assert_eq!(NONCLAIMS.len(), 10);
    }
}
