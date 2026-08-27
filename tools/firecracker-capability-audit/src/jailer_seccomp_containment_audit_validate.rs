use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::inventory_phase::{
    InventoryPhase, JAILER_SECCOMP_CONTAINMENT_ID, classify_inventory_phase, disposition_counts,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    Capability, CapabilityInventory, ContainmentClauseOutcome, ContainmentEvidenceProfileId,
    ContainmentNonclaim, ContainmentResidualClassification, Disposition, FIRECRACKER_COMMIT,
    FIRECRACKER_TARGET, FIRECRACKER_VERSION, JailerSeccompContainmentAudit, Reference,
    SourceManifest, ValidationErrors, jailer_seccomp_containment_audit_json,
};

/// Current checked jailer/seccomp containment composition schema.
pub const JAILER_SECCOMP_CONTAINMENT_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative jailer/seccomp containment composition path.
pub const JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/jailer-seccomp-containment-audit.json";
/// Exact capability transition owned by #1918.
pub const JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID: &str = JAILER_SECCOMP_CONTAINMENT_ID;

const UNRELATED_INVENTORY_SHA256: &str =
    "6814700a121fee3eeaca71e2b69a1165a47c1fc7a5c9f2cfa7642d56ad37ca63";

const PROFILE_IDS: [ContainmentEvidenceProfileId; 8] = [
    ContainmentEvidenceProfileId::SignedCodeAndEntitlements,
    ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace,
    ContainmentEvidenceProfileId::JailerOperationAndLimits,
    ContainmentEvidenceProfileId::TypedResourceAuthority,
    ContainmentEvidenceProfileId::LinuxIsolationLimits,
    ContainmentEvidenceProfileId::PortableSeccompiler,
    ContainmentEvidenceProfileId::FailureCleanupAndConcurrency,
    ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
];

const NONCLAIMS: [ContainmentNonclaim; 12] = [
    ContainmentNonclaim::LinuxMechanismParity,
    ContainmentNonclaim::CallerDefinedRuntimeSandboxPolicy,
    ContainmentNonclaim::GeneralDynamicResourceBroker,
    ContainmentNonclaim::HardRevocation,
    ContainmentNonclaim::CrossFilesystemAtomicPublication,
    ContainmentNonclaim::GlobalCrossLauncherAllocation,
    ContainmentNonclaim::PositiveVmnetConnectivityOrCredentials,
    ContainmentNonclaim::HostFirewallCapacityOrAdmissionPolicy,
    ContainmentNonclaim::PositiveArbitraryPerInstanceUidGid,
    ContainmentNonclaim::AutomaticRestartOrLongLivedService,
    ContainmentNonclaim::MaliciousSameBundleSiblingIsolation,
    ContainmentNonclaim::DeveloperIdNotarizationOrDeployment,
];

/// Validate the complete checked jailer/seccomp containment composition.
pub fn validate_jailer_seccomp_containment_audit(
    audit: &JailerSeccompContainmentAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_header_and_sources(audit, manifest, inventory, &mut errors);
    validate_inventory_transition(audit, inventory, &mut errors);
    validate_source_clauses(audit, &mut errors);
    validate_terminal_dependencies(audit, inventory, &mut errors);
    validate_external_dependencies(audit, inventory, &mut errors);
    validate_residuals(audit, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence_profiles(audit, repository_root, &tracked, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if audit.nonclaims != NONCLAIMS {
        errors.push("jailer/seccomp containment requires the exact ordered nonclaims".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_header_and_sources(
    audit: &JailerSeccompContainmentAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != JAILER_SECCOMP_CONTAINMENT_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "jailer/seccomp containment schema_version must be {JAILER_SECCOMP_CONTAINMENT_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
        || audit.baseline != manifest.baseline
        || audit.baseline != inventory.baseline
    {
        errors.push("jailer/seccomp containment baseline is not the pinned release".to_string());
    }
    if audit.parent_issue != "#1351" || audit.delivery_issue != "#1918" {
        errors.push("jailer/seccomp containment ownership must be #1351/#1918".to_string());
    }
    if audit.capability_id != JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID {
        errors.push("jailer/seccomp containment requires the exact #1918 capability".to_string());
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
            "production-host-setup",
            "corpus:production-host",
            "docs/prod-host-setup.md",
            "entire-file",
            "8939b56a965963d8df1c44c583dcd38361197347",
        ),
        (
            "seccomp",
            "corpus:seccomp",
            "docs/seccomp.md",
            "entire-file",
            "0611fd8d602a08deaa3e5174a4b32953427c9dc9",
        ),
        (
            "seccompiler",
            "corpus:seccompiler",
            "docs/seccompiler.md",
            "entire-file",
            "50f44097cece19d2538e054ee7e3b6ba457c7a55",
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
        errors.push(
            "jailer/seccomp containment requires the exact ordered pinned sources".to_string(),
        );
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
                "jailer/seccomp containment source identity drifted: {manifest_id}"
            )),
            None => errors.push(format!(
                "jailer/seccomp containment source identity is missing: {manifest_id}"
            )),
        }
        match inputs.get(path) {
            Some(input) if input.git_blob == blob => {}
            Some(_) => errors.push(format!(
                "jailer/seccomp containment source blob drifted: {path}"
            )),
            None => errors.push(format!(
                "jailer/seccomp containment source input is missing: {path}"
            )),
        }
    }
}

fn validate_inventory_transition(
    audit: &JailerSeccompContainmentAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let previous = &audit.previous_counts;
    if (
        previous.implemented_and_verified,
        previous.audit_required,
        previous.missing_platform_feasible,
        previous.proven_platform_impossible,
    ) != (381, 3, 1, 33)
    {
        errors.push(
            "jailer/seccomp containment previous counts must be exactly 381/3/1/33".to_string(),
        );
    }
    let target = &audit.target_counts;
    if (
        target.implemented_and_verified,
        target.audit_required,
        target.missing_platform_feasible,
        target.proven_platform_impossible,
    ) != (382, 3, 0, 33)
    {
        errors.push(
            "jailer/seccomp containment target counts must be exactly 382/3/0/33".to_string(),
        );
    }
    if !matches!(
        disposition_counts(inventory),
        (382, 3, 0, 33) | (383, 2, 0, 33) | (383, 0, 2, 33)
    ) {
        errors.push(
            "jailer/seccomp containment live inventory must be exactly 382/3/0/33 or one of its exact successors through 383/0/2/33 vmnet feasibility".to_string(),
        );
    }
    if !matches!(
        classify_inventory_phase(inventory),
        Ok(InventoryPhase::JailerSeccompContainment
            | InventoryPhase::ProductionHost
            | InventoryPhase::NetworkVmnetFeasibility)
    ) {
        errors.push(
            "jailer/seccomp containment live inventory has an inexact successor phase".to_string(),
        );
    }

    match inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID)
    {
        Some(capability)
            if capability.family == "isolation"
                && capability.source_refs
                    == [
                        "corpus:design",
                        "corpus:jailer",
                        "corpus:production-host",
                        "corpus:seccomp",
                        "corpus:seccompiler",
                    ]
                && capability.disposition == Disposition::ImplementedAndVerified
                && !capability.implementation.is_empty()
                && !capability.validation.is_empty()
                && capability.delivery_issue.is_none()
                && capability.exclusion.is_none() => {}
        Some(_) => errors.push(
            "jailer/seccomp containment capability is not terminal with exact ownership"
                .to_string(),
        ),
        None => errors.push("jailer/seccomp containment capability is missing".to_string()),
    }

    if audit.unrelated_inventory_sha256 != UNRELATED_INVENTORY_SHA256 {
        errors.push(
            "jailer/seccomp containment unrelated-inventory digest authority drifted".to_string(),
        );
    }
    match unrelated_inventory_sha256(inventory) {
        Ok(actual) if actual == UNRELATED_INVENTORY_SHA256 => {}
        Ok(actual) => errors.push(format!(
            "jailer/seccomp containment unrelated inventory changed: expected {UNRELATED_INVENTORY_SHA256}, found {actual}"
        )),
        Err(_) => errors.push(
            "jailer/seccomp containment unrelated inventory is not serializable".to_string(),
        ),
    }
}

fn unrelated_inventory_sha256(
    inventory: &CapabilityInventory,
) -> Result<String, serde_json::Error> {
    let unrelated = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.id != JAILER_SECCOMP_CONTAINMENT_CAPABILITY_ID)
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
    outcome: ContainmentClauseOutcome,
    profiles: &'static [ContainmentEvidenceProfileId],
}

const SOURCE_CLAUSES: [ClauseSpec; 46] = [
    ClauseSpec {
        id: "different-customer-workloads",
        source_id: "firecracker-design",
        anchor: "Firecracker can safely run workloads from different customers on the same machine.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::FailureCleanupAndConcurrency],
    },
    ClauseSpec {
        id: "one-process-one-microvm",
        source_id: "firecracker-design",
        anchor: "Each Firecracker process encapsulates one and only one microVM.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace],
    },
    ClauseSpec {
        id: "malicious-vcpu-code",
        source_id: "firecracker-design",
        anchor: "all vCPU threads are considered to be running malicious code",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::SignedCodeAndEntitlements],
    },
    ClauseSpec {
        id: "nested-trust-zones",
        source_id: "firecracker-design",
        anchor: "Containment is achieved by nesting several trust zones",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::SignedCodeAndEntitlements],
    },
    ClauseSpec {
        id: "device-io-rate-barrier",
        source_id: "firecracker-design",
        anchor: "I/O rate limiting is applied at this point.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::JailerOperationAndLimits],
    },
    ClauseSpec {
        id: "host-egress-filtering",
        source_id: "firecracker-design",
        anchor: "should be filtered at the host-level.",
        outcome: ContainmentClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ContainmentEvidenceProfileId::NetworkAndOperatorBoundary],
    },
    ClauseSpec {
        id: "process-level-defense-in-depth",
        source_id: "firecracker-design",
        anchor: "Firecracker should only run constrained at the process level.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::SignedCodeAndEntitlements],
    },
    ClauseSpec {
        id: "per-thread-seccomp-before-guest",
        source_id: "firecracker-design",
        anchor: "The filters are loaded in the Firecracker process, on a per-thread basis, before executing any guest code.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "privileged-third-party-resource-grants",
        source_id: "firecracker-design",
        anchor: "access resources that a privileged third-party grants access to",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::TypedResourceAuthority],
    },
    ClauseSpec {
        id: "cgroup-cpu-quota-and-fairness",
        source_id: "firecracker-design",
        anchor: "have its own dedicated quota of the CPU time",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[
            ContainmentEvidenceProfileId::LinuxIsolationLimits,
            ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "validate-paths-and-vm-id",
        source_id: "jailer",
        anchor: "Validate **all provided paths** and the VM ID.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::JailerOperationAndLimits],
    },
    ClauseSpec {
        id: "close-open-descriptors",
        source_id: "jailer",
        anchor: "Close all open file descriptors based on `/proc/<jailer-pid>/fd` except input, output and error.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace],
    },
    ClauseSpec {
        id: "clean-parent-environment",
        source_id: "jailer",
        anchor: "Cleanup all environment variables received from the parent process.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace],
    },
    ClauseSpec {
        id: "create-private-root",
        source_id: "jailer",
        anchor: "Create the `<chroot_base>/<exec_file_name>/<id>/root` folder",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace],
    },
    ClauseSpec {
        id: "fix-executable-code-identity",
        source_id: "jailer",
        anchor: "Copy the file specified with `--exec-file` to `<chroot_dir>/<exec_file_name>`.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::SignedCodeAndEntitlements],
    },
    ClauseSpec {
        id: "set-resource-bounds",
        source_id: "jailer",
        anchor: "Set resource bounds for current process and its children through `--resource-limit` argument",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::JailerOperationAndLimits],
    },
    ClauseSpec {
        id: "create-cgroup-subfolders",
        source_id: "jailer",
        anchor: "Create the cgroup sub-folders.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "unshare-pivot-and-chroot",
        source_id: "jailer",
        anchor: "Call `unshare()` into a new mount namespace, use `pivot_root()`",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "create-tun-device",
        source_id: "jailer",
        anchor: "Use `mknod` to create a `/dev/net/tun` equivalent inside the jail.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "create-kvm-device",
        source_id: "jailer",
        anchor: "Use `mknod` to create a `/dev/kvm` equivalent inside the jail.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "chown-root-and-devices",
        source_id: "jailer",
        anchor: "Use `chown` to change ownership of the `<chroot_dir>`",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "join-network-namespace",
        source_id: "jailer",
        anchor: "attempt to join the specified network namespace.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "daemon-session-and-standard-streams",
        source_id: "jailer",
        anchor: "call `setsid()` and redirect `STDIN`, `STDOUT`, and `STDERR` to `/dev/null`.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace,
            ContainmentEvidenceProfileId::JailerOperationAndLimits,
        ],
    },
    ClauseSpec {
        id: "clone-pid-namespace",
        source_id: "jailer",
        anchor: "call `clone()` with `CLONE_NEWPID` flag",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "drop-uid-and-gid",
        source_id: "jailer",
        anchor: "Drop privileges via setting the provided `uid` and `gid`.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "exec-with-injected-identity-and-times",
        source_id: "jailer",
        anchor: "<exec_file_name> --id=<id> --start-time-us=<opaque> --start-time-cpu-us=<opaque>",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace,
            ContainmentEvidenceProfileId::JailerOperationAndLimits,
        ],
    },
    ClauseSpec {
        id: "production-default-seccomp-policy",
        source_id: "production-host-setup",
        anchor: "Production usage of the `--seccomp-filter` or `--no-seccomp` parameters is not recommended.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "bounded-serial-output",
        source_id: "production-host-setup",
        anchor: "Users are responsible for handling the memory and storage usage",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::JailerOperationAndLimits,
            ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "bounded-log-output",
        source_id: "production-host-setup",
        anchor: "Users are responsible for consuming and storing this data safely.",
        outcome: ContainmentClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ContainmentEvidenceProfileId::NetworkAndOperatorBoundary],
    },
    ClauseSpec {
        id: "external-overwatcher",
        source_id: "production-host-setup",
        anchor: "customers have an overwatcher process on the host",
        outcome: ContainmentClauseOutcome::OperatorOwnedOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::FailureCleanupAndConcurrency,
            ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "jailer-equivalent-production-constraints",
        source_id: "production-host-setup",
        anchor: "executed under process constraints equal or more restrictive than those in the jailer.",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::SignedCodeAndEntitlements,
            ContainmentEvidenceProfileId::JailerOperationAndLimits,
        ],
    },
    ClauseSpec {
        id: "trusted-production-paths",
        source_id: "production-host-setup",
        anchor: "their parent directories are not writable by unprivileged users",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::SignedCodeAndEntitlements,
            ContainmentEvidenceProfileId::TypedResourceAuthority,
        ],
    },
    ClauseSpec {
        id: "dedicated-posix-identity",
        source_id: "production-host-setup",
        anchor: "Create a dedicated non-privileged POSIX user and group",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "unique-per-instance-uid-gid",
        source_id: "production-host-setup",
        anchor: "recommended that each runs with its unique `uid` and `gid`",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "workload-specific-resource-policy",
        source_id: "production-host-setup",
        anchor: "highly dependent on the workload type and usecase",
        outcome: ContainmentClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ContainmentEvidenceProfileId::NetworkAndOperatorBoundary],
    },
    ClauseSpec {
        id: "disk-resource-limits",
        source_id: "production-host-setup",
        anchor: "Jailer's `resource-limit` provides control on the disk usage",
        outcome: ContainmentClauseOutcome::ImplementedMacosOutcome,
        profiles: &[ContainmentEvidenceProfileId::JailerOperationAndLimits],
    },
    ClauseSpec {
        id: "cgroup-cpu-and-memory-controls",
        source_id: "production-host-setup",
        anchor: "can guarantee a minimum number of CPU shares when a system is busy",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[
            ContainmentEvidenceProfileId::LinuxIsolationLimits,
            ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
        ],
    },
    ClauseSpec {
        id: "production-host-firewall",
        source_id: "production-host-setup",
        anchor: "Firewall rules should therefore be implemented on the host",
        outcome: ContainmentClauseOutcome::OperatorOwnedOutcome,
        profiles: &[ContainmentEvidenceProfileId::NetworkAndOperatorBoundary],
    },
    ClauseSpec {
        id: "minimal-default-filters",
        source_id: "seccomp",
        anchor: "The default filters only allow the bare minimum set of system calls",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "per-thread-filter-installation",
        source_id: "seccomp",
        anchor: "The filters are loaded in the Firecracker process, on a per-thread basis",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "compiled-default-filter-artifact",
        source_id: "seccomp",
        anchor: "serialized binary file, using seccompiler-bin, and gets embedded in the Firecracker binary.",
        outcome: ContainmentClauseOutcome::ImplementedPortableToolOutcome,
        profiles: &[
            ContainmentEvidenceProfileId::PortableSeccompiler,
            ContainmentEvidenceProfileId::LinuxIsolationLimits,
        ],
    },
    ClauseSpec {
        id: "dangerous-custom-filter-override",
        source_id: "seccomp",
        anchor: "misconfiguration can result in abruptly terminating the process",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "disabled-filter-production-boundary",
        source_id: "seccomp",
        anchor: "Do **not** use in production.",
        outcome: ContainmentClauseOutcome::TerminalPlatformLimit,
        profiles: &[ContainmentEvidenceProfileId::LinuxIsolationLimits],
    },
    ClauseSpec {
        id: "json-to-bpf-compilation",
        source_id: "seccompiler",
        anchor: "compiles seccomp filters expressed as JSON files",
        outcome: ContainmentClauseOutcome::ImplementedPortableToolOutcome,
        profiles: &[ContainmentEvidenceProfileId::PortableSeccompiler],
    },
    ClauseSpec {
        id: "bitcode-thread-map-output",
        source_id: "seccompiler",
        anchor: "output file contains a bitcode-serialized map of thread names",
        outcome: ContainmentClauseOutcome::ImplementedPortableToolOutcome,
        profiles: &[ContainmentEvidenceProfileId::PortableSeccompiler],
    },
    ClauseSpec {
        id: "thread-category-json-schema",
        source_id: "seccompiler",
        anchor: "At the top level, the file requires an object that maps thread categories",
        outcome: ContainmentClauseOutcome::ImplementedPortableToolOutcome,
        profiles: &[ContainmentEvidenceProfileId::PortableSeccompiler],
    },
];

fn validate_source_clauses(audit: &JailerSeccompContainmentAudit, errors: &mut Vec<String>) {
    if audit.source_clauses.len() != SOURCE_CLAUSES.len() {
        errors.push(
            "jailer/seccomp containment requires exactly 46 ordered source clauses".to_string(),
        );
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
                "jailer/seccomp containment source clause[{index}] does not match the exact ordered obligation"
            ));
        }
        if !seen.insert(record.id.as_str()) {
            errors.push(format!(
                "jailer/seccomp containment contains a duplicate source clause: {}",
                record.id
            ));
        }
    }
    if audit.source_clauses.len() > SOURCE_CLAUSES.len() {
        errors.push("jailer/seccomp containment contains unknown source clauses".to_string());
    }
}

const TERMINAL_DEPENDENCIES: [(&str, Disposition); 19] = [
    ("corpus:jailer", Disposition::ImplementedAndVerified),
    ("corpus:seccomp", Disposition::ProvenPlatformImpossible),
    ("corpus:seccompiler", Disposition::ImplementedAndVerified),
    (
        "firecracker-argument:no-seccomp",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "firecracker-argument:seccomp-filter",
        Disposition::ProvenPlatformImpossible,
    ),
    (
        "semantic.isolation:host-resource-authority-and-brokerage",
        Disposition::ImplementedAndVerified,
    ),
    (
        "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
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
        "tool-argument:jailer/daemonize",
        Disposition::ImplementedAndVerified,
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
    (
        "tool-operation:seccompiler/compile",
        Disposition::ImplementedAndVerified,
    ),
];

fn validate_terminal_dependencies(
    audit: &JailerSeccompContainmentAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .terminal_dependencies
        .iter()
        .map(|dependency| (dependency.capability_id.as_str(), dependency.disposition))
        .collect::<Vec<_>>();
    if actual != TERMINAL_DEPENDENCIES {
        errors.push(
            "jailer/seccomp containment requires the exact terminal dependencies".to_string(),
        );
    }

    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (id, disposition) in TERMINAL_DEPENDENCIES {
        let Some(capability) = capabilities.get(id) else {
            errors.push(format!(
                "jailer/seccomp containment terminal dependency is missing: {id}"
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
                "jailer/seccomp containment dependency is not terminal with exact evidence: {id}"
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

const EXTERNAL_DEPENDENCIES: [ExternalSpec; 3] = [
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
];

fn validate_external_dependencies(
    audit: &JailerSeccompContainmentAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit.external_dependencies.len() != EXTERNAL_DEPENDENCIES.len() {
        errors.push(
            "jailer/seccomp containment requires the exact external dependencies".to_string(),
        );
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
                "jailer/seccomp containment external dependency[{index}] drifted"
            ));
        }
        let completed_production_host = expected.capability_id == "corpus:production-host"
            && matches!(
                phase,
                Some(InventoryPhase::ProductionHost | InventoryPhase::NetworkVmnetFeasibility)
            );
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
                    && capability.delivery_issue.is_none()
                    && capability.exclusion.is_none() => {}
            Some(capability)
                if completed_production_host
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
                "jailer/seccomp containment external dependency changed disposition, ownership, or evidence: {}",
                expected.capability_id
            )),
            None => errors.push(format!(
                "jailer/seccomp containment external dependency is missing: {}",
                expected.capability_id
            )),
        }
    }
}

const RESIDUALS: [(
    &str,
    ContainmentResidualClassification,
    ContainmentEvidenceProfileId,
); 12] = [
    (
        "literal-linux-mechanism-parity",
        ContainmentResidualClassification::TerminalPlatformLimit,
        ContainmentEvidenceProfileId::LinuxIsolationLimits,
    ),
    (
        "caller-defined-runtime-sandbox-policy",
        ContainmentResidualClassification::TerminalPlatformLimit,
        ContainmentEvidenceProfileId::SignedCodeAndEntitlements,
    ),
    (
        "general-dynamic-resource-broker",
        ContainmentResidualClassification::GenericNonrequirement,
        ContainmentEvidenceProfileId::TypedResourceAuthority,
    ),
    (
        "hard-revocation",
        ContainmentResidualClassification::GenericNonrequirement,
        ContainmentEvidenceProfileId::TypedResourceAuthority,
    ),
    (
        "cross-filesystem-atomic-publication",
        ContainmentResidualClassification::ImplementationSpecificNonclaim,
        ContainmentEvidenceProfileId::FailureCleanupAndConcurrency,
    ),
    (
        "global-cross-launcher-allocation",
        ContainmentResidualClassification::OperatorOwnedOutcome,
        ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
    ),
    (
        "positive-vmnet-and-approved-credentials",
        ContainmentResidualClassification::ExternalDependency,
        ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
    ),
    (
        "host-firewall-capacity-and-admission-policy",
        ContainmentResidualClassification::OperatorOwnedOutcome,
        ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
    ),
    (
        "positive-arbitrary-per-instance-uid-gid",
        ContainmentResidualClassification::TerminalPlatformLimit,
        ContainmentEvidenceProfileId::LinuxIsolationLimits,
    ),
    (
        "automatic-restart-or-long-lived-service",
        ContainmentResidualClassification::OperatorOwnedOutcome,
        ContainmentEvidenceProfileId::FailureCleanupAndConcurrency,
    ),
    (
        "malicious-same-bundle-sibling-isolation",
        ContainmentResidualClassification::ImplementationSpecificNonclaim,
        ContainmentEvidenceProfileId::SignedCodeAndEntitlements,
    ),
    (
        "developer-id-notarization-and-deployment",
        ContainmentResidualClassification::IndependentlyOwnedOutcome,
        ContainmentEvidenceProfileId::NetworkAndOperatorBoundary,
    ),
];

fn validate_residuals(audit: &JailerSeccompContainmentAudit, errors: &mut Vec<String>) {
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
            "jailer/seccomp containment requires the exact residual classifications".to_string(),
        );
    }
}

fn validate_evidence_profiles(
    audit: &JailerSeccompContainmentAudit,
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
            "jailer/seccomp containment requires the exact ordered evidence profiles".to_string(),
        );
    }
    for profile in &audit.evidence_profiles {
        let (implementation, validation) = expected_evidence(profile.id);
        validate_reference_set(
            &profile.implementation,
            implementation,
            repository_root,
            tracked,
            &format!("jailer/seccomp containment {:?} implementation", profile.id),
            errors,
        );
        validate_reference_set(
            &profile.validation,
            validation,
            repository_root,
            tracked,
            &format!("jailer/seccomp containment {:?} validation", profile.id),
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
        errors
            .push("jailer/seccomp containment evidence profile coverage is not closed".to_string());
    }
}

type LocalReferenceSpec = (&'static str, &'static str);

fn expected_evidence(
    id: ContainmentEvidenceProfileId,
) -> (&'static [LocalReferenceSpec], &'static [LocalReferenceSpec]) {
    match id {
        ContainmentEvidenceProfileId::SignedCodeAndEntitlements => (
            &[
                (
                    "crates/launcher/src/macos/code_sign.rs",
                    "pub(crate) fn validate_bundle(",
                ),
                ("crates/launcher/src/package.rs", "pub fn build_bundle("),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn assert_exact_networkless_bundle_entitlements(bundle: &Path)",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn production_bundle_has_exact_nested_signing_contract()",
                ),
            ],
        ),
        ContainmentEvidenceProfileId::LifecycleAndPrivateNamespace => (
            &[
                (
                    "crates/launcher/src/launch_policy.rs",
                    "pub(crate) struct LaunchRequest",
                ),
                (
                    "crates/launcher/src/macos/spawn.rs",
                    "pub(crate) struct SuspendedWorker",
                ),
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn cleanup_namespace_after_worker(",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn launcher_preserves_sandbox_outside_path_denial_and_redaction()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
            ],
        ),
        ContainmentEvidenceProfileId::JailerOperationAndLimits => (
            &[
                (
                    "compat/firecracker/v1.16.0/jailer-aggregate-contract.md",
                    "## Terminal aggregate outcome",
                ),
                (
                    "crates/launcher/src/launch_policy.rs",
                    "pub(crate) struct LaunchRequest",
                ),
            ],
            &[
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_rejects_unsupported_isolation_before_grants_sessions_and_worker()",
                ),
            ],
        ),
        ContainmentEvidenceProfileId::TypedResourceAuthority => (
            &[
                (
                    "compat/firecracker/v1.16.0/host-resource-authority-contract.md",
                    "## Terminal host-resource authority outcome",
                ),
                (
                    "crates/launcher/src/grant_manifest.rs",
                    "pub(crate) struct PreparedGrantBatch",
                ),
                (
                    "crates/session/src/macos/grant_registry.rs",
                    "pub struct GrantRegistry",
                ),
            ],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn signed_grants_authorize_only_typed_read_write_and_directory_operations()",
            )],
        ),
        ContainmentEvidenceProfileId::LinuxIsolationLimits => (
            &[
                (
                    "compat/firecracker/v1.16.0/isolation-contract.md",
                    "## Certified Linux runtime isolation exclusions",
                ),
                (
                    "crates/bangbang/src/main.rs",
                    "fn unsupported_firecracker_arg(",
                ),
                (
                    "crates/launcher/src/error.rs",
                    "pub enum JailerIsolationArgument",
                ),
            ],
            &[
                (
                    "crates/bangbang/tests/process_e2e.rs",
                    "fn executable_rejects_unsupported_firecracker_process_flags_before_socket_publication()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn signed_jailer_rejects_unsupported_isolation_before_grants_sessions_and_worker()",
                ),
            ],
        ),
        ContainmentEvidenceProfileId::PortableSeccompiler => (
            &[
                (
                    "tools/seccompiler/src/artifact.rs",
                    "pub(super) fn publish(",
                ),
                ("tools/seccompiler/src/lib.rs", "pub fn compile_json("),
                ("tools/seccompiler/src/tool.rs", "pub(super) fn run("),
            ],
            &[(
                "tools/seccompiler/tests/cli.rs",
                "fn split_and_basic_outputs_preserve_compiled_filter_semantics()",
            )],
        ),
        ContainmentEvidenceProfileId::FailureCleanupAndConcurrency => (
            &[
                (
                    "compat/firecracker/v1.16.0/multiprocess-isolation-contract.md",
                    "## Terminal multiprocess isolation outcome",
                ),
                (
                    "crates/launcher/src/macos/supervise.rs",
                    "fn cleanup_namespace_after_worker(",
                ),
            ],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn concurrent_sessions_remain_independent_when_one_worker_crashes()",
            )],
        ),
        ContainmentEvidenceProfileId::NetworkAndOperatorBoundary => (
            &[
                (
                    "compat/firecracker/v1.16.0/host-resource-authority-contract.md",
                    "## Terminal host-resource authority outcome",
                ),
                ("crates/session/src/codec.rs", "pub struct VmnetAuthority"),
            ],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation()",
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
    audit: &JailerSeccompContainmentAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match jailer_seccomp_containment_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize jailer/seccomp containment audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(JAILER_SECCOMP_CONTAINMENT_AUDIT_PATH)) {
        Ok(bytes) if bytes == canonical => {}
        Ok(_) => errors
            .push("checked jailer/seccomp containment audit is not canonical JSON".to_string()),
        Err(_) => errors.push("checked jailer/seccomp containment audit is unreadable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_containment_populations_are_closed() {
        assert_eq!(SOURCE_CLAUSES.len(), 46);
        assert_eq!(TERMINAL_DEPENDENCIES.len(), 19);
        assert_eq!(EXTERNAL_DEPENDENCIES.len(), 3);
        assert_eq!(PROFILE_IDS.len(), 8);
        assert_eq!(RESIDUALS.len(), 12);
        assert_eq!(NONCLAIMS.len(), 12);
    }
}
