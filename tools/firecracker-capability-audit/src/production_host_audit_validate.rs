use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::inventory_phase::{
    InventoryPhase, PRODUCTION_HOST_ID, classify_inventory_phase, disposition_counts,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, ProductionHostAudit, ProductionHostClauseOutcome,
    ProductionHostEvidenceProfileId, ProductionHostNonclaim, ProductionHostResidualClassification,
    Reference, SourceManifest, ValidationErrors, production_host_audit_json,
};

/// Current checked production-host authority schema.
pub const PRODUCTION_HOST_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative checked production-host authority path.
pub const PRODUCTION_HOST_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/production-host-audit.json";
/// Exact capability transitioned by #1920.
pub const PRODUCTION_HOST_CAPABILITY_ID: &str = PRODUCTION_HOST_ID;

const UNRELATED_INVENTORY_SHA256: &str =
    "7d856c4f0a437618268e601ff024f3495f9955c0149b3557aef8abb4ebb81c86";

const PROFILE_IDS: [ProductionHostEvidenceProfileId; 7] = [
    ProductionHostEvidenceProfileId::ContainmentAndIdentity,
    ProductionHostEvidenceProfileId::OutputAndObservability,
    ProductionHostEvidenceProfileId::ResourceControls,
    ProductionHostEvidenceProfileId::NetworkAndOperatorBoundary,
    ProductionHostEvidenceProfileId::HostAndHardwarePolicy,
    ProductionHostEvidenceProfileId::TimerAndArchitecture,
    ProductionHostEvidenceProfileId::ExternalVmnet,
];

const NONCLAIMS: [ProductionHostNonclaim; 8] = [
    ProductionHostNonclaim::LiteralLinuxKvmCgroupNamespaceAndModuleMechanisms,
    ProductionHostNonclaim::HostKernelGuestKernelMicrocodeAndFirmwareMaintenance,
    ProductionHostNonclaim::HostFirewallSwapCapacityAdmissionAndFleetPolicy,
    ProductionHostNonclaim::OutputRetentionMonitoringRestartAndLongLivedService,
    ProductionHostNonclaim::HardwareSideChannelAndPhysicalHostCertification,
    ProductionHostNonclaim::DeveloperIdNotarizationAndDeployment,
    ProductionHostNonclaim::PositiveVmnetConnectivityOrApprovedCredentials,
    ProductionHostNonclaim::FirecrackerSpecificSignalHandlerHazard,
];

/// Validate the complete checked production-host source authority.
pub fn validate_production_host_audit(
    audit: &ProductionHostAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header_and_source(audit, manifest, inventory, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_source_clauses(audit, &mut errors);
    validate_terminal_dependencies(audit, inventory, &mut errors);
    validate_external_dependencies(audit, inventory, &mut errors);
    validate_residuals(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence_profiles(audit, repository_root, &tracked, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if audit.nonclaims != NONCLAIMS {
        errors.push("production-host authority requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

/// Verify every checked clause anchor against the pinned upstream source bytes.
pub fn validate_production_host_upstream_source(
    audit: &ProductionHostAudit,
    firecracker_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_source_clauses(audit, &mut errors);
    let expected_source = (
        "production-host-setup",
        "corpus:production-host",
        "docs/prod-host-setup.md",
        "entire-file",
        "8939b56a965963d8df1c44c583dcd38361197347",
    );
    let actual_source = (
        audit.upstream_source.id.as_str(),
        audit.upstream_source.manifest_id.as_str(),
        audit.upstream_source.path.as_str(),
        audit.upstream_source.anchor.as_str(),
        audit.upstream_source.git_blob.as_str(),
    );
    if actual_source != expected_source {
        errors.push(
            "production-host upstream comparison requires the exact pinned source identity"
                .to_string(),
        );
    } else {
        match std::fs::read_to_string(firecracker_root.join(expected_source.2)) {
            Ok(source) => validate_upstream_anchors(&audit.source_clauses, &source, &mut errors),
            Err(error) => errors.push(format!(
                "production-host upstream source is unreadable: {error}"
            )),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_upstream_anchors(
    clauses: &[crate::ProductionHostSourceClause],
    source: &str,
    errors: &mut Vec<String>,
) {
    let mut previous = None;
    for clause in clauses {
        let mut matches = source.match_indices(&clause.upstream_anchor);
        let Some((position, _)) = matches.next() else {
            errors.push(format!(
                "production-host upstream anchor is absent for clause: {}",
                clause.id
            ));
            continue;
        };
        if matches.next().is_some() {
            errors.push(format!(
                "production-host upstream anchor is not unique for clause: {}",
                clause.id
            ));
        }
        if let Some((previous_id, previous_position)) = previous
            && position <= previous_position
        {
            errors.push(format!(
                "production-host upstream clause order drifted: {previous_id} must precede {}",
                clause.id
            ));
        }
        previous = Some((clause.id.as_str(), position));
    }
}

fn validate_header_and_source(
    audit: &ProductionHostAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != PRODUCTION_HOST_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "production-host schema_version must be {PRODUCTION_HOST_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
        || audit.baseline != inventory.baseline
    {
        errors.push("production-host baseline is not the pinned release".to_string());
    }
    if audit.parent_issue != "#1351" || audit.delivery_issue != "#1920" {
        errors.push("production-host ownership must be #1351/#1920".to_string());
    }
    if audit.capability_id != PRODUCTION_HOST_CAPABILITY_ID {
        errors.push("production-host authority requires the exact #1920 capability".to_string());
    }

    let expected = (
        "production-host-setup",
        "corpus:production-host",
        "docs/prod-host-setup.md",
        "entire-file",
        "8939b56a965963d8df1c44c583dcd38361197347",
    );
    let actual = (
        audit.upstream_source.id.as_str(),
        audit.upstream_source.manifest_id.as_str(),
        audit.upstream_source.path.as_str(),
        audit.upstream_source.anchor.as_str(),
        audit.upstream_source.git_blob.as_str(),
    );
    if actual != expected {
        errors.push("production-host authority requires the exact pinned source".to_string());
    }

    match manifest.items.iter().find(|item| item.id == expected.1) {
        Some(item) if item.path == expected.2 && item.anchor == expected.3 => {}
        Some(_) => errors.push("production-host source identity drifted".to_string()),
        None => errors.push("production-host source identity is missing".to_string()),
    }
    match manifest
        .inputs
        .iter()
        .find(|input| input.path == expected.2)
    {
        Some(input) if input.git_blob == expected.4 => {}
        Some(_) => errors.push("production-host source blob drifted".to_string()),
        None => errors.push("production-host source input is missing".to_string()),
    }
}

fn validate_inventory_transition(
    audit: &ProductionHostAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let previous = &audit.previous_counts;
    if (
        previous.implemented_and_verified,
        previous.audit_required,
        previous.missing_platform_feasible,
        previous.proven_platform_impossible,
    ) != (382, 3, 0, 33)
    {
        errors.push("production-host previous counts must be exactly 382/3/0/33".to_string());
    }
    let target = &audit.target_counts;
    if (
        target.implemented_and_verified,
        target.audit_required,
        target.missing_platform_feasible,
        target.proven_platform_impossible,
    ) != (383, 2, 0, 33)
    {
        errors.push("production-host target counts must be exactly 383/2/0/33".to_string());
    }
    if disposition_counts(inventory) != (383, 2, 0, 33) {
        errors.push("production-host live inventory must be exactly 383/2/0/33".to_string());
    }
    if classify_inventory_phase(inventory) != Ok(InventoryPhase::ProductionHost) {
        errors.push("production-host live inventory has an inexact successor phase".to_string());
    }

    match inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == PRODUCTION_HOST_CAPABILITY_ID)
    {
        Some(capability)
            if capability.family == "isolation"
                && capability.source_refs == ["corpus:production-host"]
                && capability.disposition == Disposition::ImplementedAndVerified
                && !capability.implementation.is_empty()
                && !capability.validation.is_empty()
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none() => {}
        Some(_) => errors
            .push("production-host capability is not terminal with exact ownership".to_string()),
        None => errors.push("production-host capability is missing".to_string()),
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push("production-host unrelated-inventory digest authority drifted".to_string());
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "production-host unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push("production-host unrelated inventory is not serializable".to_string()),
    }
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let unrelated = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.id != PRODUCTION_HOST_CAPABILITY_ID)
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
    anchor: &'static str,
    outcome: ProductionHostClauseOutcome,
    profiles: &'static [ProductionHostEvidenceProfileId],
}

const SOURCE_CLAUSES: [ClauseSpec; 31] = [
    ClauseSpec {
        id: "kernel-and-microcode-patching",
        anchor: "The host and guest kernels and host microcode must be",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "restrictive-seccomp-production-default",
        anchor: "Production usage of the `--seccomp-filter` or `--no-seccomp` parameters is not",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "bounded-serial-output",
        anchor: "responsible for handling the memory and storage usage of the Firecracker process",
        outcome: ProductionHostClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ProductionHostEvidenceProfileId::OutputAndObservability],
    },
    ClauseSpec {
        id: "serial-production-disable-policy",
        anchor: "we do not recommend that users enable",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::OutputAndObservability],
    },
    ClauseSpec {
        id: "nonblocking-serial-data-loss",
        anchor: "any subsequent writes will fail, resulting in data loss, until",
        outcome: ProductionHostClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ProductionHostEvidenceProfileId::OutputAndObservability],
    },
    ClauseSpec {
        id: "bounded-log-retention",
        anchor: "consuming and storing this data safely.",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::OutputAndObservability],
    },
    ClauseSpec {
        id: "linux-host-logging-performance",
        anchor: "We recommend adding `quiet loglevel=1` to the host kernel command line",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "firecracker-signal-handler-deadlock",
        anchor: "The custom signal handlers used by Firecracker are not async-signal-safe",
        outcome: ProductionHostClauseOutcome::ImplementationSpecificNonrequirement,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "external-overwatcher-sigkill",
        anchor: "customers have an overwatcher process on the host",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "jailer-equivalent-production-constraints",
        anchor: "executed under process constraints equal or more restrictive",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "linux-jailer-mechanisms",
        anchor: "namespace isolation and drops privileges of the Firecracker process.",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "trusted-jailer-paths",
        anchor: "by unprivileged users. The jailer treats all its inputs as trusted",
        outcome: ProductionHostClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "least-privilege-resource-identity",
        anchor: "Firecracker should be owned by this user and group. Apply least privilege to",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "unique-per-instance-uid-gid",
        anchor: "recommended that each runs with its unique `uid` and `gid`",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "workload-specific-resource-policy",
        anchor: "memory or CPU because these are highly dependent on the workload type and",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "linux-block-io-controller",
        anchor: "which allows users to control I/O operations through the following files:",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "file-size-and-descriptor-limits",
        anchor: "Jailer's `resource-limit` provides control on the disk usage",
        outcome: ProductionHostClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "linux-memory-cgroup-controls",
        anchor: "to allow setting upper limits to memory usage:",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "linux-cpu-cgroup-controls",
        anchor: "can guarantee a minimum number of CPU shares when a system is busy",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "x86-kvm-pit-overhead-controls",
        anchor: "guest injects timer interrupts with the help of kvm-pit kernel thread.",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::TimerAndArchitecture],
    },
    ClauseSpec {
        id: "network-flooding-controls",
        anchor: "configuring rate limiters for the network interface",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "host-egress-firewall",
        anchor: "Firewall rules should therefore be implemented on the host",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::NetworkAndOperatorBoundary],
    },
    ClauseSpec {
        id: "storage-contention-controls",
        anchor: "Rate limiting functionality is supported for both networking",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::ResourceControls],
    },
    ClauseSpec {
        id: "host-swap-remanence-policy",
        anchor: "Disabling swap mitigates data remanence issues",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "host-hardware-vulnerability-policy",
        anchor: "Firecracker is not able to mitigate host's hardware vulnerabilities.",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "single-tenant-process-boundary",
        anchor: "each Firecracker process corresponds to a workload of a single tenant.",
        outcome: ProductionHostClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ProductionHostEvidenceProfileId::ContainmentAndIdentity],
    },
    ClauseSpec {
        id: "firmware-and-microcode-maintenance",
        anchor: "microcode as soon as possible. Aside from keeping the system firmware",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "side-channel-host-policy",
        anchor: "Specific mitigations for side channel issues are constantly evolving",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "arm-kvm-physical-counter-offset",
        anchor: "KVM_CAP_COUNTER_OFFSET",
        outcome: ProductionHostClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[ProductionHostEvidenceProfileId::TimerAndArchitecture],
    },
    ClauseSpec {
        id: "host-vulnerability-verification",
        anchor: "spectre-meltdown-checker script",
        outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
    },
    ClauseSpec {
        id: "x86-linux-6-1-kvm-cgroup-regressions",
        anchor: "Linux 6.1 introduced some regressions in the time it takes to boot a VM",
        outcome: ProductionHostClauseOutcome::TerminalPlatformOrArchitectureLimit,
        profiles: &[ProductionHostEvidenceProfileId::TimerAndArchitecture],
    },
];

fn validate_source_clauses(audit: &ProductionHostAudit, errors: &mut Vec<String>) {
    if audit.source_clauses.len() != SOURCE_CLAUSES.len() {
        errors.push("production-host requires exactly 31 ordered source clauses".to_string());
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
            || record.upstream_anchor != expected.anchor
            || record.outcome != expected.outcome
            || record.evidence_profiles != expected.profiles
        {
            errors.push(format!(
                "production-host source clause[{index}] does not match the exact ordered obligation"
            ));
        }
        if !seen.insert(record.id.as_str()) {
            errors.push(format!(
                "production-host contains a duplicate source clause: {}",
                record.id
            ));
        }
    }
    if audit.source_clauses.len() > SOURCE_CLAUSES.len() {
        errors.push("production-host contains unknown source clauses".to_string());
    }
}

const TERMINAL_DEPENDENCIES: [(&str, Disposition); 25] = [
    (
        "api-property:Drive.rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
    (
        "api-property:NetworkInterface.rx_rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
    (
        "api-property:NetworkInterface.tx_rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
    ("corpus:jailer", Disposition::ImplementedAndVerified),
    ("corpus:seccomp", Disposition::ProvenPlatformImpossible),
    (
        "firecracker-argument:no-seccomp",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "firecracker-argument:seccomp-filter",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "semantic.device:rtc-vmclock-vmgenid-and-pvtime",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.device:serial-stdin-stdout-rx-and-restore",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.isolation:host-resource-authority-and-brokerage",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.observability:logger-delivery-filtering-loss-and-redaction",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.process:signals-exits-fd-and-cleanup",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.storage:block-sync-async-vhost-and-limits",
        Disposition::ImplementedAndVerified,
    ),
    (
        "tool-argument:jailer/cgroup",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/cgroup-version",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/chroot-base-dir",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/gid",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/netns",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/new-pid-ns",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/parent-cgroup",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/resource-limit",
        Disposition::ImplementedAndVerified,
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
    audit: &ProductionHostAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .terminal_dependencies
        .iter()
        .map(|dependency| (dependency.capability_id.as_str(), dependency.disposition))
        .collect::<Vec<_>>();
    if actual != TERMINAL_DEPENDENCIES {
        errors.push("production-host requires the exact terminal dependencies".to_string());
    }
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (id, disposition) in TERMINAL_DEPENDENCIES {
        let Some(capability) = capabilities.get(id) else {
            errors.push(format!(
                "production-host terminal dependency is missing: {id}"
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
                "production-host dependency is not terminal with exact evidence: {id}"
            ));
        }
    }
}

struct ExternalSpec {
    capability_id: &'static str,
    owner_issue: &'static str,
    outcomes: &'static [&'static str],
}

const EXTERNAL_DEPENDENCIES: [ExternalSpec; 2] = [
    ExternalSpec {
        capability_id: "corpus:network-setup",
        owner_issue: "#1378",
        outcomes: &[
            "positive-production-vmnet-connectivity",
            "credentialed-service-cleanup-and-concurrency",
        ],
    },
    ExternalSpec {
        capability_id: "semantic.network:virtio-net-vmnet-policy-and-connectivity",
        owner_issue: "#1378",
        outcomes: &[
            "approved-apple-vmnet-credential",
            "real-start-packets-connectivity-teardown-and-reclamation",
        ],
    },
];

fn validate_external_dependencies(
    audit: &ProductionHostAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.external_dependencies.len() != EXTERNAL_DEPENDENCIES.len() {
        errors.push("production-host requires the exact external dependencies".to_string());
    }
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (index, (record, expected)) in audit
        .external_dependencies
        .iter()
        .zip(EXTERNAL_DEPENDENCIES.iter())
        .enumerate()
    {
        let outcomes = record
            .owned_outcomes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if record.capability_id != expected.capability_id
            || record.disposition != Disposition::AuditRequired
            || record.owner_issue != expected.owner_issue
            || outcomes != expected.outcomes
        {
            errors.push(format!(
                "production-host external dependency[{index}] drifted"
            ));
        }
        match capabilities.get(expected.capability_id) {
            Some(capability)
                if capability.disposition == Disposition::AuditRequired
                    && capability.implementation.is_empty()
                    && capability.validation.is_empty()
                    && capability.delivery_issue.is_none()
                    && capability.exclusion.is_none() => {}
            Some(_) => errors.push(format!(
                "production-host external dependency changed disposition, ownership, or evidence: {}",
                expected.capability_id
            )),
            None => errors.push(format!(
                "production-host external dependency is missing: {}",
                expected.capability_id
            )),
        }
    }
}

const RESIDUALS: [(
    &str,
    ProductionHostResidualClassification,
    ProductionHostEvidenceProfileId,
); 8] = [
    (
        "literal-linux-host-mechanisms",
        ProductionHostResidualClassification::TerminalPlatformOrArchitectureLimit,
        ProductionHostEvidenceProfileId::ContainmentAndIdentity,
    ),
    (
        "operator-maintenance-and-host-policy",
        ProductionHostResidualClassification::OperatorOwnedOutcome,
        ProductionHostEvidenceProfileId::HostAndHardwarePolicy,
    ),
    (
        "output-retention-and-monitoring",
        ProductionHostResidualClassification::OperatorOwnedOutcome,
        ProductionHostEvidenceProfileId::OutputAndObservability,
    ),
    (
        "host-firewall-capacity-and-admission",
        ProductionHostResidualClassification::OperatorOwnedOutcome,
        ProductionHostEvidenceProfileId::NetworkAndOperatorBoundary,
    ),
    (
        "firecracker-specific-signal-handler-hazard",
        ProductionHostResidualClassification::ImplementationSpecificNonrequirement,
        ProductionHostEvidenceProfileId::ContainmentAndIdentity,
    ),
    (
        "positive-vmnet-and-approved-credentials",
        ProductionHostResidualClassification::ExternalDependency,
        ProductionHostEvidenceProfileId::ExternalVmnet,
    ),
    (
        "developer-id-notarization-and-deployment",
        ProductionHostResidualClassification::IndependentlyOwnedOutcome,
        ProductionHostEvidenceProfileId::NetworkAndOperatorBoundary,
    ),
    (
        "physical-host-hardware-certification",
        ProductionHostResidualClassification::OperatorOwnedOutcome,
        ProductionHostEvidenceProfileId::HostAndHardwarePolicy,
    ),
];

fn validate_residuals(audit: &ProductionHostAudit, errors: &mut Vec<String>) {
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
        errors.push("production-host requires the exact residual classifications".to_string());
    }
}

type LocalReferenceSpec = (&'static str, &'static str);

fn expected_evidence(
    id: ProductionHostEvidenceProfileId,
) -> (&'static [LocalReferenceSpec], &'static [LocalReferenceSpec]) {
    match id {
        ProductionHostEvidenceProfileId::ContainmentAndIdentity => (
            &[
                (
                    "compat/firecracker/v1.16.0/host-resource-authority-contract.md",
                    "## Terminal host-resource authority outcome",
                ),
                (
                    "compat/firecracker/v1.16.0/jailer-seccomp-containment-contract.md",
                    "## Terminal jailer/seccomp containment outcome",
                ),
                (
                    "compat/firecracker/v1.16.0/multiprocess-isolation-contract.md",
                    "## Terminal multiprocess isolation outcome",
                ),
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
        ProductionHostEvidenceProfileId::OutputAndObservability => (
            &[
                (
                    "compat/firecracker/v1.16.0/logger-contract.md",
                    "## Delivery and limiter policy",
                ),
                (
                    "compat/firecracker/v1.16.0/serial-contract.md",
                    "## Destination endpoint and continuation policy",
                ),
                (
                    "crates/runtime/src/serial.rs",
                    "pub struct SharedSerialOutput",
                ),
            ],
            &[
                (
                    "crates/bangbang/src/main.rs",
                    "fn terminal_logger_failure_is_counted_before_final_metrics_and_preserves_result()",
                ),
                (
                    "crates/runtime/src/serial.rs",
                    "fn shared_serial_output_counts_rate_limited_dropped_bytes()",
                ),
            ],
        ),
        ProductionHostEvidenceProfileId::ResourceControls => (
            &[
                (
                    "compat/firecracker/v1.16.0/host-resource-authority-contract.md",
                    "## Fixed authority behavior",
                ),
                (
                    "compat/firecracker/v1.16.0/network-mmds-contract.md",
                    "## Observable live contract",
                ),
                (
                    "compat/firecracker/v1.16.0/storage-contract.md",
                    "## Observable storage contract",
                ),
            ],
            &[
                (
                    "crates/runtime/src/block.rs",
                    "fn block_rate_limiter_rolls_back_ops_when_bandwidth_throttles()",
                ),
                (
                    "crates/runtime/src/network.rs",
                    "fn network_rate_limiter_rolls_back_ops_when_bandwidth_throttles()",
                ),
            ],
        ),
        ProductionHostEvidenceProfileId::NetworkAndOperatorBoundary => (
            &[
                (
                    "compat/firecracker/v1.16.0/network-mmds-contract.md",
                    "## Explicit nonclaims and handoffs",
                ),
                ("crates/session/src/codec.rs", "pub struct VmnetAuthority"),
                ("docs/security.md", "## Current Non-Goals"),
            ],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation()",
            )],
        ),
        ProductionHostEvidenceProfileId::HostAndHardwarePolicy => (
            &[
                (
                    "compat/firecracker/v1.16.0/production-host-contract.md",
                    "## Operator, hardware, and deployment boundaries",
                ),
                ("docs/security.md", "## Current Non-Goals"),
            ],
            &[(
                "tools/firecracker-capability-audit/tests/production_host_audit.rs",
                "fn checked_production_host_audit_is_canonical_and_fail_closed()",
            )],
        ),
        ProductionHostEvidenceProfileId::TimerAndArchitecture => (
            &[
                (
                    "compat/firecracker/v1.16.0/time-identity-contract.md",
                    "## Certification boundary",
                ),
                (
                    "docs/snapshot-feasibility.md",
                    "Native arm64 timer and VMGenID restore policy",
                ),
            ],
            &[(
                "crates/hvf/src/runner.rs",
                "fn captures_arm64_physical_timer_state_on_runner_thread()",
            )],
        ),
        ProductionHostEvidenceProfileId::ExternalVmnet => (
            &[
                (
                    "compat/firecracker/v1.16.0/network-mmds-contract.md",
                    "## Explicit nonclaims and handoffs",
                ),
                (
                    "scripts/preflight-production-vmnet.sh",
                    "bangbang vmnet preflight: blocked",
                ),
                (
                    "scripts/production_vmnet_certification.py",
                    "def run_certification(",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation()",
                ),
                (
                    "scripts/tests/test_production_vmnet_orchestration.py",
                    "class ProductionVmnetOrchestrationTests",
                ),
            ],
        ),
    }
}

fn validate_evidence_profiles(
    audit: &ProductionHostAudit,
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
        errors.push("production-host requires the exact ordered evidence profiles".to_string());
    }
    for profile in &audit.evidence_profiles {
        let (implementation, validation) = expected_evidence(profile.id);
        validate_reference_set(
            &profile.implementation,
            implementation,
            repository_root,
            tracked,
            &format!("production-host {:?} implementation", profile.id),
            errors,
        );
        validate_reference_set(
            &profile.validation,
            validation,
            repository_root,
            tracked,
            &format!("production-host {:?} validation", profile.id),
            errors,
        );
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
    audit: &ProductionHostAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match production_host_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize production-host audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(PRODUCTION_HOST_AUDIT_PATH)) {
        Ok(bytes) if bytes == canonical => {}
        Ok(_) => errors.push("checked production-host audit is not canonical JSON".to_string()),
        Err(_) => errors.push("checked production-host audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_production_host_populations_are_closed() {
        assert_eq!(SOURCE_CLAUSES.len(), 31);
        assert_eq!(TERMINAL_DEPENDENCIES.len(), 25);
        assert_eq!(EXTERNAL_DEPENDENCIES.len(), 2);
        assert_eq!(PROFILE_IDS.len(), 7);
        assert_eq!(RESIDUALS.len(), 8);
        assert_eq!(NONCLAIMS.len(), 8);
    }

    #[test]
    fn upstream_comparison_rejects_a_missing_clause_anchor() {
        let clauses = [crate::ProductionHostSourceClause {
            order: 1,
            id: "missing".to_string(),
            upstream_anchor: "missing production-host clause".to_string(),
            outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
            evidence_profiles: vec![ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
        }];
        let mut errors = Vec::new();
        validate_upstream_anchors(&clauses, "unrelated source", &mut errors);
        assert_eq!(
            errors,
            ["production-host upstream anchor is absent for clause: missing"]
        );
    }

    #[test]
    fn upstream_comparison_rejects_duplicate_and_reordered_clause_anchors() {
        let clause = |order, id: &str, upstream_anchor: &str| crate::ProductionHostSourceClause {
            order,
            id: id.to_string(),
            upstream_anchor: upstream_anchor.to_string(),
            outcome: ProductionHostClauseOutcome::OperatorOwnedOutcome,
            evidence_profiles: vec![ProductionHostEvidenceProfileId::HostAndHardwarePolicy],
        };
        let clauses = [clause(1, "first", "first"), clause(2, "second", "second")];

        let mut errors = Vec::new();
        validate_upstream_anchors(&clauses, "first second first", &mut errors);
        assert_eq!(
            errors,
            ["production-host upstream anchor is not unique for clause: first"]
        );

        let mut errors = Vec::new();
        validate_upstream_anchors(&clauses, "second first", &mut errors);
        assert_eq!(
            errors,
            ["production-host upstream clause order drifted: first must precede second"]
        );
    }
}
