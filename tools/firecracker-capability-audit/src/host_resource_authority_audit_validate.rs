use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::inventory_phase::{
    HOST_RESOURCE_AUTHORITY_ID, InventoryPhase, JAILER_SECCOMP_CONTAINMENT_ID,
    classify_inventory_phase, disposition_counts,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, HostResourceAccess, HostResourceAuthorityAudit, HostResourceClauseOutcome,
    HostResourceEvidenceProfileId, HostResourceLifetime, HostResourceNonclaim,
    HostResourceObjectKind, HostResourceResidualClassification, HostResourceRole, Reference,
    SourceManifest, ValidationErrors, host_resource_authority_audit_json,
};

/// Current checked host-resource authority schema.
pub const HOST_RESOURCE_AUTHORITY_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative host-resource authority path.
pub const HOST_RESOURCE_AUTHORITY_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/host-resource-authority-audit.json";
/// Exact capability transition owned by #1916.
pub const HOST_RESOURCE_AUTHORITY_CAPABILITY_ID: &str = HOST_RESOURCE_AUTHORITY_ID;

const UNRELATED_INVENTORY_SHA256: &str =
    "fe268291c594ff41ff6bdc71e33a3aa71ee6d7b4997ec112d45dc9a21927e7a1";

const PROFILE_IDS: [HostResourceEvidenceProfileId; 11] = [
    HostResourceEvidenceProfileId::ManifestPreflight,
    HostResourceEvidenceProfileId::AtomicGrantTransport,
    HostResourceEvidenceProfileId::BootAndInputAuthority,
    HostResourceEvidenceProfileId::StorageRuntimeAuthority,
    HostResourceEvidenceProfileId::OutputAndRateBounds,
    HostResourceEvidenceProfileId::SocketVsockVhostAuthority,
    HostResourceEvidenceProfileId::SnapshotAndPagerAuthority,
    HostResourceEvidenceProfileId::NetworkPolicyBoundary,
    HostResourceEvidenceProfileId::LimitsAndFairness,
    HostResourceEvidenceProfileId::FailureCleanupAndConcurrency,
    HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
];

const NONCLAIMS: [HostResourceNonclaim; 12] = [
    HostResourceNonclaim::GeneralDynamicResourceBroker,
    HostResourceNonclaim::HardRevocation,
    HostResourceNonclaim::CrossFilesystemAtomicSocketPublication,
    HostResourceNonclaim::GlobalCrossLauncherResourceAllocation,
    HostResourceNonclaim::PositiveVmnetConnectivityOrCredentials,
    HostResourceNonclaim::HostTapRoutingFirewallOrAddressManagement,
    HostResourceNonclaim::LinuxCgroupNamespaceOrChrootParity,
    HostResourceNonclaim::PositiveArbitraryPerInstanceUidGid,
    HostResourceNonclaim::AutomaticRestartOrReconnect,
    HostResourceNonclaim::VhostUserBackendRateLimiting,
    HostResourceNonclaim::AggressiveUniversalResourceQuotas,
    HostResourceNonclaim::DeveloperIdNotarizationOrDeployment,
];

/// Validate the complete checked host-resource authority against the current tree.
pub fn validate_host_resource_authority_audit(
    audit: &HostResourceAuthorityAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header_and_sources(audit, manifest, inventory, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_source_clauses(audit, &mut errors);
    validate_resource_surface(audit, &mut errors);
    validate_terminal_dependencies(audit, inventory, &mut errors);
    validate_external_dependencies(audit, inventory, &mut errors);
    validate_residuals(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence_profiles(audit, repository_root, &tracked, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if audit.nonclaims != NONCLAIMS {
        errors.push("host-resource authority requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header_and_sources(
    audit: &HostResourceAuthorityAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != HOST_RESOURCE_AUTHORITY_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "host-resource authority schema_version must be {HOST_RESOURCE_AUTHORITY_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
        || audit.baseline != inventory.baseline
    {
        errors.push("host-resource authority baseline is not the pinned release".to_string());
    }
    if audit.parent_issue != "#1351" || audit.delivery_issue != "#1916" {
        errors.push("host-resource authority ownership must be #1351/#1916".to_string());
    }
    if audit.capability_id != HOST_RESOURCE_AUTHORITY_CAPABILITY_ID {
        errors.push("host-resource authority requires the exact #1916 capability".to_string());
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
            "jailer",
            "corpus:jailer",
            "docs/jailer.md",
            "entire-file",
            "fa5e8b4ee769f64ee83a317dce5902ffd0029a1b",
        ),
        (
            "network-setup",
            "corpus:network-setup",
            "docs/network-setup.md",
            "entire-file",
            "c161b6661d4362a49d1978e0cafc5e7a6e5cebf6",
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
        errors
            .push("host-resource authority requires the exact ordered pinned sources".to_string());
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
                "host-resource authority source identity drifted: {manifest_id}"
            )),
            None => errors.push(format!(
                "host-resource authority source identity is missing: {manifest_id}"
            )),
        }
        match inputs.get(path) {
            Some(input) if input.git_blob == blob => {}
            Some(_) => errors.push(format!(
                "host-resource authority source blob drifted: {path}"
            )),
            None => errors.push(format!(
                "host-resource authority source input is missing: {path}"
            )),
        }
    }
}

fn validate_inventory_transition(
    audit: &HostResourceAuthorityAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let previous = &audit.previous_counts;
    if (
        previous.implemented_and_verified,
        previous.audit_required,
        previous.missing_platform_feasible,
        previous.proven_platform_impossible,
    ) != (380, 3, 2, 33)
    {
        errors
            .push("host-resource authority previous counts must be exactly 380/3/2/33".to_string());
    }
    let target = &audit.target_counts;
    if (
        target.implemented_and_verified,
        target.audit_required,
        target.missing_platform_feasible,
        target.proven_platform_impossible,
    ) != (381, 3, 1, 33)
    {
        errors.push("host-resource authority target counts must be exactly 381/3/1/33".to_string());
    }
    if !matches!(
        disposition_counts(inventory),
        (381, 3, 1, 33) | (382, 3, 0, 33) | (383, 2, 0, 33) | (383, 0, 2, 33)
    ) {
        errors.push(
            "host-resource authority live inventory must be exactly 381/3/1/33 or one of its exact successors through 383/0/2/33 vmnet feasibility"
                .to_string(),
        );
    }
    if !matches!(
        classify_inventory_phase(inventory),
        Ok(InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility)
    ) {
        errors.push(
            "host-resource authority live inventory has an inexact successor phase".to_string(),
        );
    }

    match inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
    {
        Some(capability)
            if capability.family == "isolation"
                && capability.source_refs
                    == [
                        "corpus:design",
                        "corpus:jailer",
                        "corpus:network-setup",
                        "corpus:production-host",
                    ]
                && capability.disposition == Disposition::ImplementedAndVerified
                && !capability.implementation.is_empty()
                && !capability.validation.is_empty()
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none() => {}
        Some(_) => errors.push(
            "host-resource authority capability is not terminal with exact ownership".to_string(),
        ),
        None => errors.push("host-resource authority capability is missing".to_string()),
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push(
            "host-resource authority unrelated-inventory digest authority drifted".to_string(),
        );
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "host-resource authority unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push("host-resource authority unrelated inventory is not serializable".to_string()),
    }
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let unrelated = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.id != HOST_RESOURCE_AUTHORITY_CAPABILITY_ID)
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
    outcome: HostResourceClauseOutcome,
    profiles: &'static [HostResourceEvidenceProfileId],
}

const SOURCE_CLAUSES: [ClauseSpec; 30] = [
    ClauseSpec {
        id: "host-network-tap-backing",
        source_id: "firecracker-design",
        anchor: "Firecracker emulated network devices are backed by TAP devices on the host.",
        outcome: HostResourceClauseOutcome::ExternalEvidenceOutcome,
        profiles: &[HostResourceEvidenceProfileId::NetworkPolicyBoundary],
    },
    ClauseSpec {
        id: "block-host-file-backing",
        source_id: "firecracker-design",
        anchor: "Firecracker emulated block devices are backed by files on the host.",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[HostResourceEvidenceProfileId::StorageRuntimeAuthority],
    },
    ClauseSpec {
        id: "network-barrier-rate-limiting",
        source_id: "firecracker-design",
        anchor: "I/O rate limiting is applied at this point.",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            HostResourceEvidenceProfileId::NetworkPolicyBoundary,
            HostResourceEvidenceProfileId::LimitsAndFairness,
        ],
    },
    ClauseSpec {
        id: "host-egress-filtering",
        source_id: "firecracker-design",
        anchor: "should be filtered at the host-level.",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "per-device-fair-rate-limiters",
        source_id: "firecracker-design",
        anchor: "rate limiters to each volume and network interface",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[HostResourceEvidenceProfileId::LimitsAndFairness],
    },
    ClauseSpec {
        id: "vhost-user-backend-rate-limiting",
        source_id: "firecracker-design",
        anchor: "customers should implement rate limiting on the side of the vhost-user backend",
        outcome: HostResourceClauseOutcome::BackendOwnedOutcome,
        profiles: &[
            HostResourceEvidenceProfileId::SocketVsockVhostAuthority,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "privileged-third-party-resource-grants",
        source_id: "firecracker-design",
        anchor: "access resources that a privileged third-party grants access to",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            HostResourceEvidenceProfileId::ManifestPreflight,
            HostResourceEvidenceProfileId::AtomicGrantTransport,
        ],
    },
    ClauseSpec {
        id: "cgroup-affinity-and-cpu-quota",
        source_id: "firecracker-design",
        anchor: "have its own dedicated quota of the CPU time",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::LimitsAndFairness,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "jailer-validation-before-mutation",
        source_id: "jailer",
        anchor: "Validate **all provided paths** and the VM ID.",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[HostResourceEvidenceProfileId::ManifestPreflight],
    },
    ClauseSpec {
        id: "jailer-fsize-and-no-file-limits",
        source_id: "jailer",
        anchor: "Current available resources that can be limited",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[HostResourceEvidenceProfileId::LimitsAndFairness],
    },
    ClauseSpec {
        id: "trusted-input-path-ownership",
        source_id: "jailer",
        anchor: "All inputs to the jailer are considered trusted",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::ManifestPreflight,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "resource-copy-link-and-permissions",
        source_id: "jailer",
        anchor: "The user must create hard links for (or copy) any resources",
        outcome: HostResourceClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            HostResourceEvidenceProfileId::BootAndInputAuthority,
            HostResourceEvidenceProfileId::StorageRuntimeAuthority,
            HostResourceEvidenceProfileId::OutputAndRateBounds,
        ],
    },
    ClauseSpec {
        id: "operator-cgroup-partitioning",
        source_id: "jailer",
        anchor: "user must manage any fine tuning of resource partitioning via cgroups",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "operator-cleanup-and-crash-race",
        source_id: "jailer",
        anchor: "It’s up to the user to handle cleanup after running the jailer.",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::FailureCleanupAndConcurrency,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "bounded-serial-output",
        source_id: "production-host-setup",
        anchor: "Users are responsible for handling the memory and storage usage",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::OutputAndRateBounds,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "bounded-log-output",
        source_id: "production-host-setup",
        anchor: "Users are responsible for consuming and storing this data safely.",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::OutputAndRateBounds,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "external-overwatcher-sigkill",
        source_id: "production-host-setup",
        anchor: "customers have an overwatcher process on the host",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[
            HostResourceEvidenceProfileId::FailureCleanupAndConcurrency,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "production-trusted-paths",
        source_id: "production-host-setup",
        anchor: "their parent directories are not writable by unprivileged users",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::ManifestPreflight,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "least-privilege-resource-identity",
        source_id: "production-host-setup",
        anchor: "All file system resources used for Firecracker should be owned by this user and group.",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::AtomicGrantTransport,
            HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "workload-specific-resource-policy",
        source_id: "production-host-setup",
        anchor: "highly dependent on the workload type and usecase",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "disk-resource-controls",
        source_id: "production-host-setup",
        anchor: "Jailer's `resource-limit` provides control on the disk usage",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::StorageRuntimeAuthority,
            HostResourceEvidenceProfileId::LimitsAndFairness,
        ],
    },
    ClauseSpec {
        id: "memory-and-cpu-cgroup-controls",
        source_id: "production-host-setup",
        anchor: "can guarantee a minimum number of CPU shares when a system is busy",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "network-flood-rate-controls",
        source_id: "production-host-setup",
        anchor: "configuring rate limiters for the network interface",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::NetworkPolicyBoundary,
            HostResourceEvidenceProfileId::LimitsAndFairness,
        ],
    },
    ClauseSpec {
        id: "production-host-egress-firewall",
        source_id: "production-host-setup",
        anchor: "Firewall rules should therefore be implemented on the host",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "storage-contention-controls",
        source_id: "production-host-setup",
        anchor: "Rate limiting functionality is supported for both networking and storage devices",
        outcome: HostResourceClauseOutcome::ImplementedWithTerminalLimit,
        profiles: &[
            HostResourceEvidenceProfileId::StorageRuntimeAuthority,
            HostResourceEvidenceProfileId::LimitsAndFairness,
        ],
    },
    ClauseSpec {
        id: "network-guide-operator-adaptation",
        source_id: "network-setup",
        anchor: "modifying this setup to accommodate your specific needs",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "per-vm-host-interface-and-tap",
        source_id: "network-setup",
        anchor: "Each microVM requires a host network interface",
        outcome: HostResourceClauseOutcome::ExternalEvidenceOutcome,
        profiles: &[HostResourceEvidenceProfileId::NetworkPolicyBoundary],
    },
    ClauseSpec {
        id: "configured-host-device-name",
        source_id: "network-setup",
        anchor: "\"host_dev_name\": \"tap0\"",
        outcome: HostResourceClauseOutcome::ExternalEvidenceOutcome,
        profiles: &[HostResourceEvidenceProfileId::NetworkPolicyBoundary],
    },
    ClauseSpec {
        id: "multi-guest-address-and-rule-allocation",
        source_id: "network-setup",
        anchor: "Each microVM has its own subnet",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
    ClauseSpec {
        id: "bridge-creation-and-cleanup",
        source_id: "network-setup",
        anchor: "make sure to delete the bridge",
        outcome: HostResourceClauseOutcome::OperatorOwnedOutcome,
        profiles: &[HostResourceEvidenceProfileId::TerminalAndOperatorBoundary],
    },
];

fn validate_source_clauses(audit: &HostResourceAuthorityAudit, errors: &mut Vec<String>) {
    if audit.source_clauses.len() != SOURCE_CLAUSES.len() {
        errors
            .push("host-resource authority requires exactly 30 ordered source clauses".to_string());
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
                "host-resource authority source clause[{index}] does not match the exact ordered obligation"
            ));
        }
        if !seen.insert(record.id.as_str()) {
            errors.push(format!(
                "host-resource authority contains a duplicate source clause: {}",
                record.id
            ));
        }
    }
    if audit.source_clauses.len() > SOURCE_CLAUSES.len() {
        errors.push("host-resource authority contains unknown source clauses".to_string());
    }
}

struct ResourceSpec {
    role: HostResourceRole,
    kinds: &'static [HostResourceObjectKind],
    access: &'static [HostResourceAccess],
    lifetime: HostResourceLifetime,
    consumer: &'static str,
    profiles: &'static [HostResourceEvidenceProfileId],
}

const RESOURCE_SURFACE: [ResourceSpec; 18] = [
    ResourceSpec {
        role: HostResourceRole::StartupConfig,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "startup-config-parser",
        profiles: &[HostResourceEvidenceProfileId::BootAndInputAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::StartupMetadata,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "startup-metadata-parser",
        profiles: &[HostResourceEvidenceProfileId::BootAndInputAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::KernelImage,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "guest-kernel-loader",
        profiles: &[HostResourceEvidenceProfileId::BootAndInputAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::InitrdImage,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "guest-initrd-loader",
        profiles: &[HostResourceEvidenceProfileId::BootAndInputAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::DriveBacking,
        kinds: &[
            HostResourceObjectKind::RegularFile,
            HostResourceObjectKind::BlockDevice,
        ],
        access: &[HostResourceAccess::ReadOnly, HostResourceAccess::ReadWrite],
        lifetime: HostResourceLifetime::RuntimeTransactional,
        consumer: "block-startup-hotplug-replace-and-restore",
        profiles: &[HostResourceEvidenceProfileId::StorageRuntimeAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::PmemBacking,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly, HostResourceAccess::ReadWrite],
        lifetime: HostResourceLifetime::RuntimeTransactional,
        consumer: "pmem-startup-hotplug-replace-and-restore",
        profiles: &[HostResourceEvidenceProfileId::StorageRuntimeAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::ApiSocketDirectory,
        kinds: &[HostResourceObjectKind::Directory],
        access: &[HostResourceAccess::CreateChildren],
        lifetime: HostResourceLifetime::SessionRetained,
        consumer: "api-socket-publication-and-cleanup",
        profiles: &[HostResourceEvidenceProfileId::SocketVsockVhostAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::VsockSocketDirectory,
        kinds: &[HostResourceObjectKind::Directory],
        access: &[HostResourceAccess::CreateChildren],
        lifetime: HostResourceLifetime::SessionRetained,
        consumer: "vsock-listener-publication-and-connect",
        profiles: &[HostResourceEvidenceProfileId::SocketVsockVhostAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::LoggerSink,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::WriteOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "logger-output",
        profiles: &[HostResourceEvidenceProfileId::OutputAndRateBounds],
    },
    ResourceSpec {
        role: HostResourceRole::MetricsSink,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::WriteOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "metrics-output",
        profiles: &[HostResourceEvidenceProfileId::OutputAndRateBounds],
    },
    ResourceSpec {
        role: HostResourceRole::SerialSink,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::WriteOnly],
        lifetime: HostResourceLifetime::RuntimeTransactional,
        consumer: "serial-output-and-restore",
        profiles: &[HostResourceEvidenceProfileId::OutputAndRateBounds],
    },
    ResourceSpec {
        role: HostResourceRole::SnapshotDescribeInput,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "snapshot-describe",
        profiles: &[HostResourceEvidenceProfileId::SnapshotAndPagerAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::SnapshotStateInput,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "snapshot-state-restore",
        profiles: &[HostResourceEvidenceProfileId::SnapshotAndPagerAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::SnapshotMemoryInput,
        kinds: &[HostResourceObjectKind::RegularFile],
        access: &[HostResourceAccess::ReadOnly],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "snapshot-memory-restore",
        profiles: &[HostResourceEvidenceProfileId::SnapshotAndPagerAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::SnapshotOutputDirectory,
        kinds: &[HostResourceObjectKind::Directory],
        access: &[HostResourceAccess::CreateChildren],
        lifetime: HostResourceLifetime::RuntimeTransactional,
        consumer: "snapshot-state-memory-and-staging-publication",
        profiles: &[HostResourceEvidenceProfileId::SnapshotAndPagerAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::VhostUserSocketDirectory,
        kinds: &[HostResourceObjectKind::Directory],
        access: &[HostResourceAccess::ConnectChildren],
        lifetime: HostResourceLifetime::SessionRetained,
        consumer: "vhost-user-exact-child-connection",
        profiles: &[HostResourceEvidenceProfileId::SocketVsockVhostAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::SnapshotPagerStream,
        kinds: &[HostResourceObjectKind::ConnectedUnixStream],
        access: &[HostResourceAccess::ReadWrite],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "snapshot-userfault-pager",
        profiles: &[HostResourceEvidenceProfileId::SnapshotAndPagerAuthority],
    },
    ResourceSpec {
        role: HostResourceRole::VmnetProviderStream,
        kinds: &[HostResourceObjectKind::ConnectedUnixStream],
        access: &[HostResourceAccess::ReadWrite],
        lifetime: HostResourceLifetime::OneTimeClaim,
        consumer: "contained-remote-vmnet-provider",
        profiles: &[HostResourceEvidenceProfileId::NetworkPolicyBoundary],
    },
];

fn validate_resource_surface(audit: &HostResourceAuthorityAudit, errors: &mut Vec<String>) {
    if audit.resource_surface.len() != RESOURCE_SURFACE.len() {
        errors
            .push("host-resource authority requires exactly 18 ordered resource roles".to_string());
    }
    let mut seen = BTreeSet::new();
    for (index, (record, expected)) in audit
        .resource_surface
        .iter()
        .zip(RESOURCE_SURFACE.iter())
        .enumerate()
    {
        if usize::from(record.order) != index + 1
            || record.role != expected.role
            || record.object_kinds != expected.kinds
            || record.access != expected.access
            || record.lifetime != expected.lifetime
            || record.consumer != expected.consumer
            || record.evidence_profiles != expected.profiles
        {
            errors.push(format!(
                "host-resource authority resource role[{index}] does not match the exact authority surface"
            ));
        }
        if !seen.insert(record.role) {
            errors.push(format!(
                "host-resource authority contains a duplicate resource role: {:?}",
                record.role
            ));
        }
    }
    if audit.resource_surface.len() > RESOURCE_SURFACE.len() {
        errors.push("host-resource authority contains unknown resource roles".to_string());
    }
}

const TERMINAL_DEPENDENCIES: [(&str, Disposition); 15] = [
    (
        "api-property:NetworkInterface.rx_rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
    (
        "api-property:NetworkInterface.tx_rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
    (
        "api-property:Pmem.rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
    ("corpus:jailer", Disposition::ImplementedAndVerified),
    (
        "semantic.device:serial-stdin-stdout-rx-and-restore",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
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
        "tool-argument:jailer/chroot-base-dir",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "tool-argument:jailer/gid",
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
    (
        "api-property:Drive.rate_limiter",
        Disposition::ImplementedAndVerified,
    ),
];

fn validate_terminal_dependencies(
    audit: &HostResourceAuthorityAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .terminal_dependencies
        .iter()
        .map(|dependency| (dependency.capability_id.as_str(), dependency.disposition))
        .collect::<Vec<_>>();
    if actual != TERMINAL_DEPENDENCIES {
        errors.push("host-resource authority requires the exact terminal dependencies".to_string());
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (id, disposition) in TERMINAL_DEPENDENCIES {
        let Some(capability) = capabilities.get(id) else {
            errors.push(format!(
                "host-resource authority terminal dependency is missing: {id}"
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
                "host-resource authority dependency is not terminal with exact evidence: {id}"
            ));
        }
    }
}

struct ExternalSpec {
    capability_id: &'static str,
    disposition: Disposition,
    owner_issue: &'static str,
    outcomes: &'static [&'static str],
}

const EXTERNAL_DEPENDENCIES: [ExternalSpec; 4] = [
    ExternalSpec {
        capability_id: "corpus:network-setup",
        disposition: Disposition::AuditRequired,
        owner_issue: "#1378",
        outcomes: &[
            "positive-production-vmnet-connectivity",
            "credentialed-failure-cleanup-and-concurrency",
        ],
    },
    ExternalSpec {
        capability_id: "corpus:production-host",
        disposition: Disposition::AuditRequired,
        owner_issue: "#1351",
        outcomes: &["production-host-signing-deployment-and-host-policy-aggregate"],
    },
    ExternalSpec {
        capability_id: "semantic.network:virtio-net-vmnet-policy-and-connectivity",
        disposition: Disposition::AuditRequired,
        owner_issue: "#1378",
        outcomes: &[
            "approved-apple-vmnet-credential",
            "real-start-packets-connectivity-teardown-and-reclamation",
        ],
    },
    ExternalSpec {
        capability_id: "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes",
        disposition: Disposition::MissingPlatformFeasible,
        owner_issue: "#1351",
        outcomes: &["final-jailer-seccomp-and-macos-containment-composition"],
    },
];

fn validate_external_dependencies(
    audit: &HostResourceAuthorityAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.external_dependencies.len() != EXTERNAL_DEPENDENCIES.len() {
        errors.push("host-resource authority requires the exact external dependencies".to_string());
    }
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let phase = classify_inventory_phase(inventory).ok();
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
            || record.disposition != expected.disposition
            || record.owner_issue != expected.owner_issue
            || outcomes != expected.outcomes
        {
            errors.push(format!(
                "host-resource authority external dependency[{index}] drifted"
            ));
        }
        let completed_successor = (expected.capability_id == JAILER_SECCOMP_CONTAINMENT_ID
            && matches!(
                phase,
                Some(
                    InventoryPhase::JailerSeccompContainment
                        | InventoryPhase::ProductionHost
                        | InventoryPhase::NetworkVmnetFeasibility
                )
            ))
            || (expected.capability_id == "corpus:production-host"
                && matches!(
                    phase,
                    Some(InventoryPhase::ProductionHost | InventoryPhase::NetworkVmnetFeasibility)
                ));
        let vmnet_feasibility_successor = phase == Some(InventoryPhase::NetworkVmnetFeasibility)
            && matches!(
                expected.capability_id,
                "corpus:network-setup"
                    | "semantic.network:virtio-net-vmnet-policy-and-connectivity"
            );
        match capabilities.get(expected.capability_id) {
            Some(capability)
                if capability.disposition == expected.disposition
                    && capability.implementation.is_empty()
                    && capability.validation.is_empty()
                    && capability.exclusion.is_none() => {}
            Some(capability)
                if completed_successor
                    && capability.disposition == Disposition::ImplementedAndVerified
                    && !capability.implementation.is_empty()
                    && !capability.validation.is_empty()
                    && capability.delivery_issue.is_none()
                    && capability.exclusion.is_none() => {}
            Some(capability)
                if vmnet_feasibility_successor
                    && capability.disposition == Disposition::MissingPlatformFeasible
                    && capability.implementation.is_empty()
                    && capability.validation.is_empty()
                    && capability.delivery_issue.as_deref()
                        == Some("https://github.com/seven332/bangbang/issues/1378")
                    && capability.exclusion.is_none() => {}
            Some(_) => errors.push(format!(
                "host-resource authority external dependency changed disposition or evidence: {}",
                expected.capability_id
            )),
            None => errors.push(format!(
                "host-resource authority external dependency is missing: {}",
                expected.capability_id
            )),
        }
    }
}

const RESIDUALS: [(
    &str,
    HostResourceResidualClassification,
    HostResourceEvidenceProfileId,
); 14] = [
    (
        "general-dynamic-resource-broker",
        HostResourceResidualClassification::GenericNonrequirement,
        HostResourceEvidenceProfileId::AtomicGrantTransport,
    ),
    (
        "hard-revocation",
        HostResourceResidualClassification::GenericNonrequirement,
        HostResourceEvidenceProfileId::AtomicGrantTransport,
    ),
    (
        "cross-filesystem-atomic-socket-publication",
        HostResourceResidualClassification::ImplementationSpecificNonclaim,
        HostResourceEvidenceProfileId::SocketVsockVhostAuthority,
    ),
    (
        "global-cross-launcher-allocation",
        HostResourceResidualClassification::OperatorOwnedOutcome,
        HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
    ),
    (
        "real-vmnet-connectivity-and-cleanup",
        HostResourceResidualClassification::ExternalDependency,
        HostResourceEvidenceProfileId::NetworkPolicyBoundary,
    ),
    (
        "repository-owned-approved-vmnet-credentials",
        HostResourceResidualClassification::ExternalDependency,
        HostResourceEvidenceProfileId::NetworkPolicyBoundary,
    ),
    (
        "host-tap-routing-firewall-and-addresses",
        HostResourceResidualClassification::OperatorOwnedOutcome,
        HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
    ),
    (
        "cpu-memory-and-blkio-cgroups",
        HostResourceResidualClassification::TerminalPlatformLimit,
        HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
    ),
    (
        "positive-arbitrary-per-instance-uid-gid",
        HostResourceResidualClassification::TerminalPlatformLimit,
        HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
    ),
    (
        "automatic-restart-or-reconnect",
        HostResourceResidualClassification::OperatorOwnedOutcome,
        HostResourceEvidenceProfileId::FailureCleanupAndConcurrency,
    ),
    (
        "vhost-user-backend-rate-limiting",
        HostResourceResidualClassification::BackendOwnedOutcome,
        HostResourceEvidenceProfileId::SocketVsockVhostAuthority,
    ),
    (
        "aggressive-universal-resource-quotas",
        HostResourceResidualClassification::GenericNonrequirement,
        HostResourceEvidenceProfileId::LimitsAndFairness,
    ),
    (
        "developer-id-notarization-and-deployment",
        HostResourceResidualClassification::IndependentlyOwnedOutcome,
        HostResourceEvidenceProfileId::TerminalAndOperatorBoundary,
    ),
    (
        "remaining-resource-specific-mutation-and-cleanup",
        HostResourceResidualClassification::AlreadyImplemented,
        HostResourceEvidenceProfileId::FailureCleanupAndConcurrency,
    ),
];

fn validate_residuals(audit: &HostResourceAuthorityAudit, errors: &mut Vec<String>) {
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
        errors.push(
            "host-resource authority requires the exact residual classifications".to_string(),
        );
    }
}

fn validate_evidence_profiles(
    audit: &HostResourceAuthorityAudit,
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
            "host-resource authority requires the exact ordered evidence profiles".to_string(),
        );
    }
    for profile in &audit.evidence_profiles {
        let (implementation, validation) = expected_evidence(profile.id);
        validate_reference_set(
            &profile.implementation,
            implementation,
            repository_root,
            tracked,
            &format!("host-resource authority {:?} implementation", profile.id),
            errors,
        );
        validate_reference_set(
            &profile.validation,
            validation,
            repository_root,
            tracked,
            &format!("host-resource authority {:?} validation", profile.id),
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
                .resource_surface
                .iter()
                .flat_map(|resource| resource.evidence_profiles.iter().copied()),
        )
        .chain(
            audit
                .residuals
                .iter()
                .map(|residual| residual.evidence_profile),
        )
        .collect::<BTreeSet<_>>();
    if declared != referenced || declared != PROFILE_IDS.into_iter().collect() {
        errors.push("host-resource authority evidence profile coverage is not closed".to_string());
    }
}

type LocalReferenceSpec = (&'static str, &'static str);

fn expected_evidence(
    id: HostResourceEvidenceProfileId,
) -> (&'static [LocalReferenceSpec], &'static [LocalReferenceSpec]) {
    match id {
        HostResourceEvidenceProfileId::ManifestPreflight => (
            &[
                (
                    "crates/launcher/src/grant_manifest.rs",
                    "pub(crate) struct PreparedGrantBatch",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "pub(crate) struct LaunchRequest",
                ),
                ("crates/session/src/grant.rs", "pub enum GrantAccess"),
                ("crates/session/src/grant.rs", "pub enum ResourceRole"),
            ],
            &[
                (
                    "crates/launcher/src/grant_manifest.rs",
                    "fn manifest_enforces_roles_access_cardinality_and_bounds()",
                ),
                (
                    "crates/launcher/src/grant_manifest.rs",
                    "fn safe_open_rejects_symlinks_types_missing_resources_and_aliases()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::AtomicGrantTransport => (
            &[
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "pub struct GrantRegistry",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "pub struct StagedGrantBatch",
                ),
                (
                    "crates/session/src/macos/grant_transport.rs",
                    "pub struct ReceivedGrant",
                ),
            ],
            &[
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "fn directory_registry_batch_adoption_is_failure_atomic()",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "fn file_registry_batch_adoption_is_failure_atomic()",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "fn receiver_rejects_cross_class_descriptor_aliases_and_closes_the_whole_batch()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::BootAndInputAuthority => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "pub(crate) struct GrantAuthority",
                ),
                (
                    "crates/launcher/src/grant_manifest.rs",
                    "pub(crate) struct PreparedGrantBatch",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_delays_boot_claim_until_api_and_keeps_opened_identity()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_grants_external_config_metadata_and_boot_inputs_to_real_guest()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::StorageRuntimeAuthority => (
            &[
                ("crates/runtime/src/block.rs", "pub struct DriveConfigs"),
                ("crates/runtime/src/pmem.rs", "pub struct PmemConfigs"),
                (
                    "crates/session/src/macos/block_control.rs",
                    "pub struct BlockControlTarget",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_adopts_delayed_block_and_pmem_grants_by_descriptor_identity()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_enforces_read_only_drive_grant_against_guest_writes()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_hotplugs_flushes_and_reuses_runtime_pmem_from_exact_unused_grants()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_live_async_block_grant_swap_uses_preauthorized_open_file()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::OutputAndRateBounds => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "pub(crate) struct GrantAuthority",
                ),
                (
                    "crates/runtime/src/serial.rs",
                    "pub struct SerialRateLimiterConfig",
                ),
                ("crates/session/src/grant.rs", "pub enum ResourceRole"),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_adopts_delayed_output_grants_by_descriptor_identity()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_adopts_output_grants_from_config_file_and_startup_cli()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_keeps_concurrent_output_grant_sessions_isolated()",
                ),
                (
                    "crates/runtime/src/serial.rs",
                    "fn rate_limited_output_drops_exhausted_bytes_without_output_error()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::SocketVsockVhostAuthority => (
            &[
                (
                    "crates/bangbang/src/anchored_socket.rs",
                    "pub(crate) struct BoundAnchoredSocket",
                ),
                (
                    "crates/session/src/macos/socket_broker.rs",
                    "pub enum SocketBrokerMessage",
                ),
                (
                    "crates/session/src/macos/vhost_user_broker.rs",
                    "pub enum VhostUserBrokerMessage",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_brokers_multiple_contained_vhost_user_children_without_helpers()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_granted_socket_cleanup_preserves_replacements_in_both_death_orders()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_routes_host_vsock_through_supplied_granted_listener()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::SnapshotAndPagerAuthority => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "pub(crate) struct ContainedSnapshotRestoreTransaction",
                ),
                (
                    "crates/bangbang/src/snapshot_restore_resources.rs",
                    "pub(crate) struct PreparedSnapshotRestoreResources",
                ),
                ("crates/session/src/grant.rs", "pub enum ResourceRole"),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_adopts_native_v2_snapshot_grants_for_create_describe_and_restore()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_pager_grant_completes_and_repeats_under_unchanged_entitlements()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_restore_transaction_covers_logical_abort_cancellation_and_commit_phases()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::NetworkPolicyBoundary => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "pub(crate) struct VmnetProviderGrantAuthority",
                ),
                (
                    "crates/bangbang/src/host_network/remote_vmnet.rs",
                    "pub(crate) struct RemoteVmnetProviderSource",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "pub(crate) struct LaunchRequest",
                ),
                ("crates/session/src/codec.rs", "pub enum VmnetBackendRoute"),
            ],
            &[
                (
                    "crates/bangbang/src/host_network/remote_vmnet.rs",
                    "fn remote_pumps_route_readiness_packets_and_ordered_cleanup()",
                ),
                (
                    "crates/launcher/src/grant_manifest.rs",
                    "fn provider_stream_manifest_connects_once_and_is_classified_for_remote_routing()",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "fn networkless_profile_routes_positive_authority_only_through_provider_grant()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_networkless_worker_uses_authenticated_remote_provider_without_apple_authorization()",
                ),
                (
                    "crates/session/src/codec.rs",
                    "fn vmnet_backend_route_round_trips_and_fails_closed_against_authority()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::LimitsAndFairness => (
            &[
                (
                    "crates/bangbang/src/contained_session.rs",
                    "fn install_resource_limit(",
                ),
                (
                    "crates/runtime/src/block.rs",
                    "pub struct VirtioBlockRateLimiter",
                ),
                (
                    "crates/runtime/src/network.rs",
                    "pub struct VirtioNetworkRateLimiter",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
                (
                    "crates/runtime/src/block.rs",
                    "fn block_device_retries_rate_limited_sync_queue_without_another_notification()",
                ),
                (
                    "crates/runtime/src/network.rs",
                    "fn virtio_network_rx_rate_limit_throttles_without_side_effects_and_retries()",
                ),
                (
                    "crates/runtime/src/network.rs",
                    "fn virtio_network_tx_rate_limit_throttles_without_side_effects_and_retries()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::FailureCleanupAndConcurrency => (
            &[
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn cleanup_namespace_after_worker(",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "pub struct StagedGrantBatch",
                ),
                (
                    "crates/session/src/macos/runtime.rs",
                    "pub struct SocketOwnershipRecord",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn concurrent_signed_grant_sessions_keep_authority_noninterchangeable()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signal_cancels_an_incomplete_grant_phase_without_waiting_for_timeout()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_grant_mismatch_fails_closed_without_mutation()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_grant_scopes_cleanup_across_both_process_crash_orders()",
                ),
            ],
        ),
        HostResourceEvidenceProfileId::TerminalAndOperatorBoundary => (
            &[
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Terminal jailer uid/gid platform limit",
                ),
                (
                    "compat/firecracker/v1.16.0/jailer-aggregate-contract.md",
                    "## Terminal aggregate outcome",
                ),
                (
                    "compat/firecracker/v1.16.0/multiprocess-isolation-contract.md",
                    "## Terminal multiprocess isolation outcome",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation()",
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
    audit: &HostResourceAuthorityAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match host_resource_authority_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize host-resource authority audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(HOST_RESOURCE_AUTHORITY_AUDIT_PATH)) {
        Ok(bytes) if bytes == canonical => {}
        Ok(_) => {
            errors.push("checked host-resource authority audit is not canonical JSON".to_string())
        }
        Err(_) => errors.push("checked host-resource authority audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_host_resource_populations_are_closed() {
        assert_eq!(SOURCE_CLAUSES.len(), 30);
        assert_eq!(RESOURCE_SURFACE.len(), 18);
        assert_eq!(TERMINAL_DEPENDENCIES.len(), 15);
        assert_eq!(EXTERNAL_DEPENDENCIES.len(), 4);
        assert_eq!(PROFILE_IDS.len(), 11);
        assert_eq!(RESIDUALS.len(), 14);
        assert_eq!(NONCLAIMS.len(), 12);
    }
}
