use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::inventory_phase::{
    InventoryPhase, WAVE8_ARM_KVM_TEMPLATE_IDS, WAVE8_HUGETLBFS_IDS, WAVE8_LINUX_ISOLATION_IDS,
    WAVE8_X86_CPUID_MSR_IDS, classify_inventory_phase, expected_disposition,
    expected_impossible_ids, expected_nonterminal_ids, wave8_historical_impossible_ids,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    Reference, SourceManifest, ValidationErrors, Wave8CertificationAudit, Wave8DeliveryOutcome,
    Wave8Domain, Wave8HandoffOwner, Wave8Nonclaim, Wave8Outcome, Wave8PlatformMechanism,
    Wave8PlatformObservation, Wave8RejectedAlternative, Wave8ScenarioExecution,
};

/// Current Wave 8 certification authority schema.
pub const WAVE8_CERTIFICATION_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative Wave 8 certification authority path.
pub const WAVE8_CERTIFICATION_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/wave8-certification-audit.json";
/// Exact capability transition owned by #1881.
pub const WAVE8_CERTIFICATION_CAPABILITY_ID: &str =
    "semantic.cross-capability:state-errors-metrics-security-and-snapshots";

const DOMAINS: [Wave8Domain; 7] = [
    Wave8Domain::LifecycleState,
    Wave8Domain::ApiErrors,
    Wave8Domain::Observability,
    Wave8Domain::SecurityResourceAuthority,
    Wave8Domain::Devices,
    Wave8Domain::NetworkMmds,
    Wave8Domain::SnapshotsRestore,
];

const OUTCOMES: [Wave8Outcome; 11] = [
    Wave8Outcome::LifecycleIdempotency,
    Wave8Outcome::StrictErrorsAndFailureAtomicity,
    Wave8Outcome::LoggerMetricsLifecycle,
    Wave8Outcome::GrantContainmentAndRedaction,
    Wave8Outcome::DeviceNetworkLivePatch,
    Wave8Outcome::SnapshotCaptureReady,
    Wave8Outcome::SnapshotRestoreContinuation,
    Wave8Outcome::SnapshotSerialization,
    Wave8Outcome::CancellationWithoutArtifacts,
    Wave8Outcome::ClaimFailureNonconsumption,
    Wave8Outcome::TerminalCleanup,
];

const HANDOFFS: [(&str, Wave8HandoffOwner, Disposition); 11] = [
    (
        "corpus:jailer",
        Wave8HandoffOwner::Issue1373,
        Disposition::AuditRequired,
    ),
    (
        "corpus:production-host",
        Wave8HandoffOwner::Issue1373,
        Disposition::AuditRequired,
    ),
    (
        "tool-argument:jailer/chroot-base-dir",
        Wave8HandoffOwner::Issue1373,
        Disposition::AuditRequired,
    ),
    (
        "tool-argument:jailer/gid",
        Wave8HandoffOwner::Issue1373,
        Disposition::AuditRequired,
    ),
    (
        "tool-argument:jailer/uid",
        Wave8HandoffOwner::Issue1373,
        Disposition::AuditRequired,
    ),
    (
        "tool-operation:jailer/run",
        Wave8HandoffOwner::Issue1373,
        Disposition::AuditRequired,
    ),
    (
        "corpus:network-setup",
        Wave8HandoffOwner::Issue1378,
        Disposition::AuditRequired,
    ),
    (
        "semantic.network:virtio-net-vmnet-policy-and-connectivity",
        Wave8HandoffOwner::Issue1378,
        Disposition::AuditRequired,
    ),
    (
        "semantic.isolation:host-resource-authority-and-brokerage",
        Wave8HandoffOwner::Issue1351,
        Disposition::MissingPlatformFeasible,
    ),
    (
        "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes",
        Wave8HandoffOwner::Issue1351,
        Disposition::MissingPlatformFeasible,
    ),
    (
        "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
        Wave8HandoffOwner::Issue1351,
        Disposition::MissingPlatformFeasible,
    ),
];

const DELIVERY_PARENTS: [(&str, Wave8DeliveryOutcome); 10] = [
    ("#1349", Wave8DeliveryOutcome::Completed),
    ("#1351", Wave8DeliveryOutcome::RetainedExternal),
    ("#1388", Wave8DeliveryOutcome::Completed),
    ("#1410", Wave8DeliveryOutcome::Completed),
    ("#1439", Wave8DeliveryOutcome::Completed),
    ("#1440", Wave8DeliveryOutcome::Completed),
    ("#1490", Wave8DeliveryOutcome::Completed),
    ("#1491", Wave8DeliveryOutcome::Completed),
    ("#1493", Wave8DeliveryOutcome::Completed),
    ("#1494", Wave8DeliveryOutcome::Completed),
];

const RETAINED_EXTERNAL_ISSUES: [&str; 5] = ["#1371", "#1373", "#1374", "#1375", "#1378"];

const NONCLAIMS: [Wave8Nonclaim; 8] = [
    Wave8Nonclaim::ExternalEvidenceCompletion,
    Wave8Nonclaim::LinuxKvmOrFirecrackerBinaryParity,
    Wave8Nonclaim::ArbitraryGuestOrCrossHostPortability,
    Wave8Nonclaim::PortablePerformanceParity,
    Wave8Nonclaim::WholeSystemFormalCorrectness,
    Wave8Nonclaim::AllPossibleRuntimeInterleavings,
    Wave8Nonclaim::PrivateOrPrivilegedFallback,
    Wave8Nonclaim::LiveGithubStateFromOfflineValidator,
];

/// Validate the complete, checked Wave 8 authority against the current tree.
pub fn validate_wave8_certification_audit(
    audit: &Wave8CertificationAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    let tracked = tracked_repository_files(repository_root, &mut errors);

    validate_header(audit, manifest, inventory, &mut errors);
    validate_scenarios(audit, repository_root, &tracked, &mut errors);
    validate_interactions(audit, &mut errors);
    validate_platform_reviews(audit, inventory, repository_root, &tracked, &mut errors);
    validate_handoffs(audit, inventory, &mut errors);
    validate_delivery_hierarchy(audit, &mut errors);
    validate_authority_evidence(audit, repository_root, &tracked, &mut errors);
    validate_document_owners(audit, repository_root, &tracked, &mut errors);
    if audit.nonclaims != NONCLAIMS {
        errors.push("Wave 8 requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header(
    audit: &Wave8CertificationAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != WAVE8_CERTIFICATION_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "Wave 8 schema version must be {}, found {}",
            WAVE8_CERTIFICATION_AUDIT_SCHEMA_VERSION, audit.schema_version
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
        || audit.baseline != inventory.baseline
    {
        errors.push(
            "Wave 8 baseline must match the exact pinned inventory and source manifest".to_string(),
        );
    }
    if audit.parent_issue != "#1348"
        || audit.delivery_issue != "#1881"
        || audit.capability_id != WAVE8_CERTIFICATION_CAPABILITY_ID
    {
        errors.push("Wave 8 issue and capability ownership drifted".to_string());
    }
    let counts = &audit.target_counts;
    if (
        counts.implemented_and_verified,
        counts.audit_required,
        counts.missing_platform_feasible,
        counts.proven_platform_impossible,
    ) != (377, 8, 3, 30)
    {
        errors.push("Wave 8 target counts must be exactly 377/8/3/30".to_string());
    }
    if audit.domains != DOMAINS {
        errors.push("Wave 8 requires the exact ordered seven interaction domains".to_string());
    }
    match classify_inventory_phase(inventory) {
        Ok(
            InventoryPhase::Wave8
            | InventoryPhase::JailerUidGidPlatformLimit
            | InventoryPhase::JailerChrootPlatformLimit
            | InventoryPhase::JailerAggregate
            | InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost,
        ) => {}
        Ok(phase) => errors.push(format!(
            "Wave 8 live inventory must be its exact 377/8/3/30 phase, the exact post-Wave-8 jailer uid/gid 377/6/3/32 successor, the exact post-uid/gid jailer chroot-base-dir 377/5/3/33 successor, the exact aggregate jailer 379/3/3/33 successor, the exact multiprocess isolation 380/3/2/33 successor, the exact host-resource authority 381/3/1/33 successor, the exact jailer/seccomp containment 382/3/0/33 successor, or the exact production-host 383/2/0/33 successor; found {}",
            phase.name()
        )),
        Err(error) => errors.push(format!("Wave 8 live inventory phase is invalid: {error}")),
    }
}

fn validate_scenarios(
    audit: &Wave8CertificationAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    struct ExpectedScenario {
        id: &'static str,
        execution: Wave8ScenarioExecution,
        domains: &'static [Wave8Domain],
        outcomes: &'static [Wave8Outcome],
        path: &'static str,
        anchor: &'static str,
    }

    const EXPECTED: [ExpectedScenario; 4] = [
        ExpectedScenario {
            id: "portable-snapshot-serialization",
            execution: Wave8ScenarioExecution::Portable,
            domains: &[
                Wave8Domain::LifecycleState,
                Wave8Domain::ApiErrors,
                Wave8Domain::Observability,
                Wave8Domain::NetworkMmds,
                Wave8Domain::SnapshotsRestore,
            ],
            outcomes: &[
                Wave8Outcome::StrictErrorsAndFailureAtomicity,
                Wave8Outcome::SnapshotSerialization,
                Wave8Outcome::CancellationWithoutArtifacts,
            ],
            path: "crates/bangbang/src/api_server.rs",
            anchor: "fn synchronous_snapshot_serializes_api_mmds_and_periodic_work_but_observes_cancellation()",
        },
        ExpectedScenario {
            id: "signed-direct-live-patch",
            execution: Wave8ScenarioExecution::SignedDirectHvf,
            domains: &[
                Wave8Domain::LifecycleState,
                Wave8Domain::ApiErrors,
                Wave8Domain::Observability,
                Wave8Domain::Devices,
                Wave8Domain::NetworkMmds,
                Wave8Domain::SnapshotsRestore,
            ],
            outcomes: &[
                Wave8Outcome::LifecycleIdempotency,
                Wave8Outcome::StrictErrorsAndFailureAtomicity,
                Wave8Outcome::LoggerMetricsLifecycle,
                Wave8Outcome::DeviceNetworkLivePatch,
                Wave8Outcome::SnapshotCaptureReady,
                Wave8Outcome::TerminalCleanup,
            ],
            path: "crates/bangbang/tests/executable_hvf_e2e.rs",
            anchor: "fn signed_executable_runs_async_block_over_mmio_with_live_patch()",
        },
        ExpectedScenario {
            id: "signed-production-snapshot-containment",
            execution: Wave8ScenarioExecution::SignedProductionBundle,
            domains: &[
                Wave8Domain::LifecycleState,
                Wave8Domain::Observability,
                Wave8Domain::SecurityResourceAuthority,
                Wave8Domain::Devices,
                Wave8Domain::NetworkMmds,
                Wave8Domain::SnapshotsRestore,
            ],
            outcomes: &[
                Wave8Outcome::LifecycleIdempotency,
                Wave8Outcome::LoggerMetricsLifecycle,
                Wave8Outcome::GrantContainmentAndRedaction,
                Wave8Outcome::SnapshotRestoreContinuation,
                Wave8Outcome::TerminalCleanup,
            ],
            path: "crates/launcher/tests/production_bundle_e2e.rs",
            anchor: "fn normal_bundle_certifies_native_v2_network_mmds_snapshot_continuation_and_containment()",
        },
        ExpectedScenario {
            id: "signed-production-claim-rejection",
            execution: Wave8ScenarioExecution::SignedProductionBundle,
            domains: &[
                Wave8Domain::ApiErrors,
                Wave8Domain::SecurityResourceAuthority,
            ],
            outcomes: &[
                Wave8Outcome::StrictErrorsAndFailureAtomicity,
                Wave8Outcome::GrantContainmentAndRedaction,
                Wave8Outcome::ClaimFailureNonconsumption,
            ],
            path: "crates/launcher/tests/production_bundle_e2e.rs",
            anchor: "fn normal_bundle_rejects_wrong_and_missing_boot_claims_without_consuming_pair()",
        },
    ];

    if audit.scenarios.len() != EXPECTED.len() {
        errors.push("Wave 8 requires the exact four leaf scenarios".to_string());
    }
    for (index, expected) in EXPECTED.iter().enumerate() {
        let Some(scenario) = audit.scenarios.get(index) else {
            continue;
        };
        if scenario.id != expected.id
            || scenario.execution != expected.execution
            || scenario.domains != expected.domains
            || scenario.outcomes != expected.outcomes
        {
            errors.push(format!("Wave 8 scenario metadata drifted: {}", expected.id));
        }
        validate_local_reference_set(
            &scenario.evidence,
            &[(expected.path, expected.anchor)],
            repository_root,
            tracked,
            &format!("Wave 8 scenario {} evidence", expected.id),
            errors,
        );
    }

    let supplied_outcomes = audit
        .scenarios
        .iter()
        .flat_map(|scenario| scenario.outcomes.iter().copied())
        .collect::<BTreeSet<_>>();
    if supplied_outcomes != OUTCOMES.into_iter().collect() {
        errors.push("Wave 8 scenarios do not supply the exact required outcome set".to_string());
    }
}

fn validate_interactions(audit: &Wave8CertificationAudit, errors: &mut Vec<String>) {
    let mut expected = Vec::new();
    for (left_index, left) in DOMAINS.iter().copied().enumerate() {
        for right in DOMAINS.iter().copied().skip(left_index + 1) {
            let scenario_ids = audit
                .scenarios
                .iter()
                .filter(|scenario| {
                    scenario.domains.contains(&left) && scenario.domains.contains(&right)
                })
                .map(|scenario| scenario.id.clone())
                .collect::<Vec<_>>();
            if scenario_ids.is_empty() {
                errors.push(format!(
                    "Wave 8 interaction pair has no leaf scenario: {left:?}/{right:?}"
                ));
            }
            expected.push((left, right, scenario_ids));
        }
    }
    let actual = audit
        .interactions
        .iter()
        .map(|pair| (pair.left, pair.right, pair.scenario_ids.clone()))
        .collect::<Vec<_>>();
    if expected.len() != 21 || actual != expected {
        errors.push(
            "Wave 8 interactions must be the exact derived 21 unordered domain pairs".to_string(),
        );
    }
}

fn validate_platform_reviews(
    audit: &Wave8CertificationAudit,
    inventory: &CapabilityInventory,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    struct ExpectedReview {
        mechanism: Wave8PlatformMechanism,
        ids: &'static [&'static str],
        observation: Wave8PlatformObservation,
        upstream_sources: &'static [&'static str],
        platform_sources: &'static [&'static str],
        alternatives: &'static [Wave8RejectedAlternative],
        challenge: &'static str,
    }

    const EXPECTED: [ExpectedReview; 4] = [
        ExpectedReview {
            mechanism: Wave8PlatformMechanism::X86CpuidMsr,
            ids: &WAVE8_X86_CPUID_MSR_IDS,
            observation: Wave8PlatformObservation::Arm64SdkLacksX86CpuidMsr,
            upstream_sources: &[
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/firecracker/swagger/firecracker.yaml#L1092-L1165",
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/cpu_config/x86_64/custom_cpu_template.rs#L70-L159",
            ],
            platform_sources: &[
                "https://developer.apple.com/documentation/hypervisor",
                "https://developer.apple.com/documentation/hypervisor/hv_vcpu_get_sys_reg%28_%3A_%3A_%3A%29",
            ],
            alternatives: &[
                Wave8RejectedAlternative::IgnoreCpuRequests,
                Wave8RejectedAlternative::CrossArchitectureRegisterTranslation,
                Wave8RejectedAlternative::EmulationOrDifferentBackend,
            ],
            challenge: "https://github.com/seven332/bangbang/issues/1784#issuecomment-5161129449",
        },
        ExpectedReview {
            mechanism: Wave8PlatformMechanism::ArmKvmFeatureTemplate,
            ids: &WAVE8_ARM_KVM_TEMPLATE_IDS,
            observation: Wave8PlatformObservation::HvfRegistersDoNotPreserveKvmIdentity,
            upstream_sources: &[
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/arch/aarch64/vcpu.rs#L202-L216",
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/cpu_config/aarch64/custom_cpu_template.rs#L34-L96",
            ],
            platform_sources: &[
                "https://developer.apple.com/documentation/hypervisor/hv_vcpu_config_get_feature_reg%28_%3A_%3A_%3A%29",
                "https://developer.apple.com/documentation/hypervisor/hv_vcpu_set_sys_reg%28_%3A_%3A_%3A%29",
            ],
            alternatives: &[
                Wave8RejectedAlternative::PrivateKvmCapabilityMapping,
                Wave8RejectedAlternative::FeatureWordRegisterReinterpretation,
                Wave8RejectedAlternative::DifferentCpuSourceModel,
            ],
            challenge: "https://github.com/seven332/bangbang/issues/1393#issuecomment-4993017798",
        },
        ExpectedReview {
            mechanism: Wave8PlatformMechanism::LinuxHugetlbfs2m,
            ids: &WAVE8_HUGETLBFS_IDS,
            observation: Wave8PlatformObservation::Arm64XnuRejectsTwoMibSuperpages,
            upstream_sources: &[
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/docs/hugepages.md",
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/vmm/src/vmm_config/machine_config.rs#L36-L88",
            ],
            platform_sources: &[
                "https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/osfmk/vm/vm_map.c#L2942-L2964",
                "https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/osfmk/arm/pmap/pmap.h#L269-L270",
                "https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/osfmk/arm64/sptm/pmap/pmap.h#L230-L231",
                "https://developer.apple.com/documentation/hypervisor/hv_ipa_granule_t",
            ],
            alternatives: &[
                Wave8RejectedAlternative::VirtualAlignmentOrBatching,
                Wave8RejectedAlternative::HvfIpaGranule,
                Wave8RejectedAlternative::PrivilegedHostOrLinuxSidecar,
            ],
            challenge: "https://github.com/seven332/bangbang/issues/1391#issuecomment-4989883731",
        },
        ExpectedReview {
            mechanism: Wave8PlatformMechanism::LinuxRuntimeIsolation,
            ids: &WAVE8_LINUX_ISOLATION_IDS,
            observation: Wave8PlatformObservation::MacosLacksLinuxIsolationPrimitives,
            upstream_sources: &[
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/firecracker/src/main.rs",
                "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/src/jailer/src/env.rs",
            ],
            platform_sources: &[
                "https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/kern/syscalls.master",
                "https://developer.apple.com/documentation/security/app-sandbox",
                "https://developer.apple.com/documentation/networkextension/packet-tunnel-provider",
                "https://developer.apple.com/documentation/vmnet",
                "https://developer.apple.com/documentation/endpointsecurity",
            ],
            alternatives: &[
                Wave8RejectedAlternative::AppSandboxOrRlimits,
                Wave8RejectedAlternative::NetworkExtensionOrVmnet,
                Wave8RejectedAlternative::LaunchdEndpointSecurityOrSidecar,
            ],
            challenge: "https://github.com/seven332/bangbang/issues/1384#issuecomment-4987589364",
        },
    ];

    if audit.platform_reviews.len() != EXPECTED.len() {
        errors.push("Wave 8 requires the exact four platform mechanism reviews".to_string());
    }
    for (index, expected) in EXPECTED.iter().enumerate() {
        let Some(review) = audit.platform_reviews.get(index) else {
            continue;
        };
        let ids = review
            .capability_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if review.mechanism != expected.mechanism
            || ids != expected.ids
            || review.observation != expected.observation
            || review.rejected_alternatives != expected.alternatives
        {
            errors.push(format!(
                "Wave 8 platform mechanism review drifted: {:?}",
                expected.mechanism
            ));
        }
        validate_exact_authoritative_references(
            &review.upstream_sources,
            expected.upstream_sources,
            repository_root,
            tracked,
            &format!("Wave 8 {:?} upstream sources", expected.mechanism),
            errors,
        );
        validate_exact_authoritative_references(
            &review.platform_sources,
            expected.platform_sources,
            repository_root,
            tracked,
            &format!("Wave 8 {:?} platform sources", expected.mechanism),
            errors,
        );
        match &review.challenge {
            Reference::Github { url } if url == expected.challenge => {}
            _ => errors.push(format!(
                "Wave 8 {:?} Challenge authority drifted",
                expected.mechanism
            )),
        }
        validate_reference(
            &review.challenge,
            repository_root,
            tracked,
            &format!("Wave 8 {:?} Challenge", expected.mechanism),
            errors,
        );
    }

    let reviewed = audit
        .platform_reviews
        .iter()
        .flat_map(|review| review.capability_ids.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let historical_impossible = wave8_historical_impossible_ids();
    if reviewed.len() != 30 || reviewed != historical_impossible {
        errors.push(
            "Wave 8 platform reviews must partition the exact historical 30 impossible capabilities"
                .to_string(),
        );
    }

    let impossible = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.disposition == Disposition::ProvenPlatformImpossible)
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    if let Ok(
        phase @ (InventoryPhase::Wave8
        | InventoryPhase::JailerUidGidPlatformLimit
        | InventoryPhase::JailerChrootPlatformLimit
        | InventoryPhase::JailerAggregate
        | InventoryPhase::MultiprocessIsolation
        | InventoryPhase::HostResourceAuthority
        | InventoryPhase::JailerSeccompContainment
        | InventoryPhase::ProductionHost),
    ) = classify_inventory_phase(inventory)
    {
        let expected = expected_impossible_ids(phase);
        if impossible != expected {
            errors.push(format!(
                "Wave 8 live impossible set differs from the exact {} successor: expected {expected:?}, found {impossible:?}",
                phase.name()
            ));
        }
    }
    for capability in inventory
        .capabilities
        .iter()
        .filter(|capability| impossible.contains(capability.id.as_str()))
    {
        if capability.exclusion.is_none() {
            errors.push(format!(
                "Wave 8 reviewed impossible capability lacks per-ID exclusion evidence: {}",
                capability.id
            ));
        }
    }
}

fn validate_handoffs(
    audit: &Wave8CertificationAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .handoffs
        .iter()
        .map(|handoff| {
            (
                handoff.capability_id.as_str(),
                handoff.owner,
                handoff.disposition,
            )
        })
        .collect::<Vec<_>>();
    if actual != HANDOFFS {
        errors.push("Wave 8 requires the exact ordered 11 external handoffs".to_string());
    }
    let phase = classify_inventory_phase(inventory).ok();
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (id, _, historical_disposition) in HANDOFFS {
        let disposition = phase.map_or(historical_disposition, |phase| {
            expected_disposition(phase, id)
        });
        if capabilities
            .get(id)
            .is_none_or(|capability| capability.disposition != disposition)
        {
            errors.push(format!("Wave 8 retained handoff disposition drifted: {id}"));
        }
    }
    let nonterminal = inventory
        .capabilities
        .iter()
        .filter(|capability| {
            matches!(
                capability.disposition,
                Disposition::AuditRequired | Disposition::MissingPlatformFeasible
            )
        })
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(
        phase @ (InventoryPhase::Wave8
        | InventoryPhase::JailerUidGidPlatformLimit
        | InventoryPhase::JailerChrootPlatformLimit
        | InventoryPhase::JailerAggregate
        | InventoryPhase::MultiprocessIsolation
        | InventoryPhase::HostResourceAuthority
        | InventoryPhase::JailerSeccompContainment
        | InventoryPhase::ProductionHost),
    ) = phase
    {
        let expected = expected_nonterminal_ids(phase);
        if nonterminal != expected {
            errors.push(format!(
                "Wave 8 retained handoffs differ from the exact {} nonterminal set: expected {expected:?}, found {nonterminal:?}",
                phase.name()
            ));
        }
    }
}

fn validate_delivery_hierarchy(audit: &Wave8CertificationAudit, errors: &mut Vec<String>) {
    let actual = audit
        .delivery_hierarchy
        .preceding_parents
        .iter()
        .map(|parent| (parent.issue.as_str(), parent.outcome))
        .collect::<Vec<_>>();
    if actual != DELIVERY_PARENTS {
        errors.push("Wave 8 delivery-parent policy drifted".to_string());
    }
    if audit
        .delivery_hierarchy
        .retained_external_issues
        .iter()
        .map(String::as_str)
        .ne(RETAINED_EXTERNAL_ISSUES)
    {
        errors.push("Wave 8 retained external issue policy drifted".to_string());
    }
    if audit.delivery_hierarchy.offline_validator_queries_github {
        errors.push("Wave 8 offline validation must not claim live GitHub queries".to_string());
    }
}

fn validate_authority_evidence(
    audit: &Wave8CertificationAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    validate_local_reference_set(
        &audit.evidence.implementation,
        &[
            (WAVE8_CERTIFICATION_AUDIT_PATH, "\"domains\": ["),
            (
                "tools/firecracker-capability-audit/src/wave8_certification_audit_model.rs",
                "pub struct Wave8CertificationAudit",
            ),
            (
                "tools/firecracker-capability-audit/src/wave8_certification_audit_validate.rs",
                "pub fn validate_wave8_certification_audit",
            ),
        ],
        repository_root,
        tracked,
        "Wave 8 authority implementation evidence",
        errors,
    );
    validate_local_reference_set(
        &audit.evidence.validation,
        &[(
            "tools/firecracker-capability-audit/tests/wave8_certification_audit.rs",
            "fn checked_wave8_certification_audit_is_canonical_and_fail_closed()",
        )],
        repository_root,
        tracked,
        "Wave 8 authority validation evidence",
        errors,
    );
    validate_local_reference_set(
        &audit.evidence.documentation,
        &[
            (
                "compat/firecracker/v1.16.0/wave8-certification-contract.md",
                "## Certified interaction matrix",
            ),
            ("docs/testing.md", "## Wave 8 final certification"),
        ],
        repository_root,
        tracked,
        "Wave 8 authority documentation evidence",
        errors,
    );
}

fn validate_document_owners(
    audit: &Wave8CertificationAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    const EXPECTED: [(&str, &str); 5] = [
        (
            "wave8-authority",
            "compat/firecracker/v1.16.0/wave8-certification-contract.md",
        ),
        (
            "compatibility-boundary",
            "docs/firecracker-compatibility.md",
        ),
        (
            "current-validation-matrix",
            "docs/firecracker-validation-matrix.md",
        ),
        ("operator-verification", "docs/testing.md"),
        ("security-boundary", "docs/security.md"),
    ];
    let actual = audit
        .document_owners
        .iter()
        .map(|owner| (owner.subject.as_str(), owner.path.as_str()))
        .collect::<Vec<_>>();
    if actual != EXPECTED {
        errors.push("Wave 8 document owners drifted".to_string());
    }
    for (index, owner) in audit.document_owners.iter().enumerate() {
        validate_reference(
            &Reference::Local {
                path: owner.path.clone(),
                anchor: None,
            },
            repository_root,
            tracked,
            &format!("Wave 8 document owner[{index}]"),
            errors,
        );
    }
}

fn validate_local_reference_set(
    references: &[Reference],
    expected: &[(&str, &str)],
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    label: &str,
    errors: &mut Vec<String>,
) {
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
            _ => errors.push(format!(
                "{label}[{index}] must be an anchored local reference"
            )),
        }
    }
}

fn validate_exact_authoritative_references(
    references: &[Reference],
    expected: &[&str],
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    label: &str,
    errors: &mut Vec<String>,
) {
    let actual = references
        .iter()
        .filter_map(|reference| match reference {
            Reference::Authoritative { url } => Some(url.as_str()),
            Reference::Local { .. } | Reference::Github { .. } => None,
        })
        .collect::<Vec<_>>();
    if actual != expected {
        errors.push(format!("{label} must match the exact primary-source set"));
    }
    for (index, reference) in references.iter().enumerate() {
        validate_reference(
            reference,
            repository_root,
            tracked,
            &format!("{label}[{index}]"),
            errors,
        );
        if !matches!(reference, Reference::Authoritative { .. }) {
            errors.push(format!("{label}[{index}] must be authoritative"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_partitions_are_exact_and_unique() {
        let impossible = WAVE8_X86_CPUID_MSR_IDS
            .into_iter()
            .chain(WAVE8_ARM_KVM_TEMPLATE_IDS)
            .chain(WAVE8_HUGETLBFS_IDS)
            .chain(WAVE8_LINUX_ISOLATION_IDS)
            .collect::<BTreeSet<_>>();
        assert_eq!(impossible.len(), 30);
        assert_eq!(HANDOFFS.len(), 11);
        assert_eq!(DOMAINS.len(), 7);
        assert_eq!(DOMAINS.len() * (DOMAINS.len() - 1) / 2, 21);
    }
}
