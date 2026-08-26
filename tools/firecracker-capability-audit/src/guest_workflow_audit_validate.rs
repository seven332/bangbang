use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    CapabilityInventory, Disposition, Ext4Classification, FIRECRACKER_COMMIT, FIRECRACKER_TARGET,
    FIRECRACKER_VERSION, GeneratedDeterminism, GuestArtifact, GuestArtifactKind, GuestNetworking,
    GuestOutputClass, GuestOutputPolicy, GuestShutdown, GuestWorkflowAudit,
    GuestWorkflowDeliveryState, GuestWorkflowMode, GuestWorkflowNonclaim,
    GuestWorkflowProfileState, GuestWorkflowTimeouts, Reference, ValidationErrors,
    guest_workflow_audit_json,
};

/// Current checked guest-workflow authority schema.
pub const GUEST_WORKFLOW_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative guest-workflow authority path.
pub const GUEST_WORKFLOW_AUDIT_PATH: &str = "compat/firecracker/v1.16.0/guest-workflow-audit.json";

/// Exact ordered artifact identities owned by the authority.
pub const GUEST_ARTIFACT_IDS: [&str; 2] = ["kernel", "rootfs"];
/// Exact ordered ext4 recipe identities owned by the authority.
pub const GUEST_EXT4_RECIPE_IDS: [&str; 2] = ["rootfs-ext4", "rootfs-ext4-direct-boot-v110"];
/// Exact ordered planned workflow identities reserved for the completion slice.
pub const GUEST_WORKFLOW_PROFILE_IDS: [&str; 2] =
    ["macos-api-rootfs-smoke", "macos-no-api-rootfs-smoke"];

const KERNEL_SHA256: &str = "e3544b10603acbf3db492cb52e000d22ba202cb4b63b9add027565683e11c591";
const ROOTFS_SHA256: &str = "0efb6a3ff2982baa6ca7e3d940966516ba7ddd2df5deb3e6c2161d369a15d608";
const INITRD_SHA256: &str = "1057079b072452a762396113867ebc5afa699a0b5c3121e28970ecadd4ba11d0";
const GUEST_IDENTITY_SHA256: &str =
    "3e5851448bae5b36f351becde037a8b13b77307279f484eda808f8177d9a4293";
const BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/rootfs-poweroff-init";
const SUCCESS_MARKER: &str = "BANGBANG_ROOTFS_WORKFLOW_OK";
const FAILURE_MARKER: &str = "BANGBANG_ROOTFS_WORKFLOW_FAIL";
const GETTING_STARTED_SUMMARY: &str = "Provide a checked rootless networkless Apple Silicon workflow that prepares pinned guest artifacts and proves signed API and no-api rootfs boot, exact guest identity, guest-requested poweroff, process exit, and session/socket cleanup; Linux/KVM, root, TAP/iptables, jailer, and production deployment instructions remain explicit nonclaims.";
const ROOTFS_AND_KERNEL_SUMMARY: &str = "Prepare and verify the pinned Firecracker CI arm64 kernel and read-only squashfs plus Bangbang's deterministic initrd, then prove exact guest-visible os-release bytes and poweroff; upstream Linux/FreeBSD build recipes, arbitrary images, redistribution, and byte-reproducible ext4 remain outside the macOS workflow.";
const SIDECAR_FIELDS: [&str; 10] = [
    "schema_version",
    "source_sha256",
    "source_size_bytes",
    "requested_size_bytes",
    "variant",
    "recipe_sha256",
    "tool_versions",
    "output_sha256",
    "output_size_bytes",
    "filesystem_check",
];
const TOOL_ROLES: [&str; 3] = ["unsquashfs", "mkfs.ext4", "e2fsck"];
const NONCLAIMS: [GuestWorkflowNonclaim; 8] = [
    GuestWorkflowNonclaim::ByteReproducibleExt4,
    GuestWorkflowNonclaim::HostileParentTraversalSafety,
    GuestWorkflowNonclaim::ArtifactRedistributionOrAuthentication,
    GuestWorkflowNonclaim::ArbitraryUrlOrProfileInput,
    GuestWorkflowNonclaim::ProductionWorkflow,
    GuestWorkflowNonclaim::ExternalGuestNetworking,
    GuestWorkflowNonclaim::ArbitraryDistroOrFreebsdGuestSupport,
    GuestWorkflowNonclaim::CrashAtomicImageSidecarPair,
];

/// Validate the exact delivery-time guest artifact and workflow authority.
pub fn validate_guest_workflow_audit(
    audit: &GuestWorkflowAudit,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline(audit, &mut errors);
    validate_artifacts(&audit.artifacts, &mut errors);
    validate_generated(audit, &mut errors);
    validate_ext4_recipes(audit, &mut errors);
    validate_output_classes(audit, &mut errors);
    validate_runtime_contract(audit, &mut errors);
    validate_profiles(audit, &mut errors);
    validate_nonclaims(audit, &mut errors);
    validate_capability_transition(audit, inventory, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_evidence(audit, repository_root, &tracked, &mut errors);
    validate_source_tokens(repository_root, &mut errors);
    validate_canonical_bytes(audit, repository_root, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    if audit.schema_version != GUEST_WORKFLOW_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "guest workflow audit schema_version must be {GUEST_WORKFLOW_AUDIT_SCHEMA_VERSION}, found {}",
            audit.schema_version
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("guest workflow audit baseline is not the pinned release".to_string());
    }
    if audit.delivery.parent_issue != "#1796"
        || audit.delivery.preparation_issue != "#1871"
        || audit.delivery.completion_issue != "#1872"
    {
        errors.push(
            "guest workflow audit requires the exact two-slice delivery ownership".to_string(),
        );
    }
    let namespace = &audit.source_namespace;
    if namespace.release != "v1.15"
        || namespace.architecture != "aarch64"
        || namespace.provider != "Firecracker CI"
        || namespace.provenance_url
            != "https://github.com/firecracker-microvm/firecracker/blob/d83d72b710361a10294480131377b1b00b163af8/docs/rootfs-and-kernel-setup.md"
        || namespace.redistribution
            != "download-only; Bangbang does not redistribute guest artifact bytes"
    {
        errors.push("guest workflow audit has stale Firecracker CI namespace policy".to_string());
    }
}

fn validate_artifacts(artifacts: &[GuestArtifact], errors: &mut Vec<String>) {
    let ids = artifacts
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    if ids != GUEST_ARTIFACT_IDS {
        errors.push(format!(
            "guest workflow audit requires the exact ordered artifact set: expected {GUEST_ARTIFACT_IDS:?}, found {ids:?}"
        ));
        return;
    }
    let [kernel, rootfs] = artifacts else {
        return;
    };
    validate_artifact(
        kernel,
        &ArtifactSpec {
            kind: GuestArtifactKind::LinuxKernel,
            filename: "vmlinux-6.1.155",
            url: "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.15/aarch64/vmlinux-6.1.155",
            sha256: KERNEL_SHA256,
            size_bytes: 17_111_552,
            cache_path: "firecracker-ci/v1.15/aarch64/vmlinux-6.1.155",
            provenance: "Firecracker CI arm64 kernel artifact",
        },
        errors,
    );
    validate_artifact(
        rootfs,
        &ArtifactSpec {
            kind: GuestArtifactKind::SquashfsRootfs,
            filename: "ubuntu-24.04.squashfs",
            url: "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.15/aarch64/ubuntu-24.04.squashfs",
            sha256: ROOTFS_SHA256,
            size_bytes: 105_332_736,
            cache_path: "firecracker-ci/v1.15/aarch64/ubuntu-24.04.squashfs",
            provenance: "Firecracker CI Ubuntu 24.04 arm64 rootfs artifact",
        },
        errors,
    );
}

struct ArtifactSpec<'a> {
    kind: GuestArtifactKind,
    filename: &'a str,
    url: &'a str,
    sha256: &'a str,
    size_bytes: u64,
    cache_path: &'a str,
    provenance: &'a str,
}

fn validate_artifact(artifact: &GuestArtifact, spec: &ArtifactSpec<'_>, errors: &mut Vec<String>) {
    if artifact.kind != spec.kind
        || artifact.filename != spec.filename
        || artifact.url != spec.url
        || artifact.sha256 != spec.sha256
        || artifact.size_bytes != spec.size_bytes
        || artifact.cache_path != spec.cache_path
        || artifact.output_class != GuestOutputClass::VerifiedRepairableCache
        || artifact.provenance != spec.provenance
        || artifact.redistribution != "not vendored; downloaded from the pinned HTTPS URL"
    {
        errors.push(format!(
            "guest workflow audit artifact has stale pin, path, provenance, or policy: {}",
            artifact.id
        ));
    }
    if !is_sha256(&artifact.sha256)
        || !artifact.url.starts_with("https://")
        || !is_safe_relative_path(&artifact.cache_path)
        || artifact.size_bytes == 0
    {
        errors.push(format!(
            "guest workflow audit artifact has an invalid digest, URL, size, or cache path: {}",
            artifact.id
        ));
    }
}

fn validate_generated(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    let [generated] = audit.generated.as_slice() else {
        errors.push("guest workflow audit requires exactly one generated initrd".to_string());
        return;
    };
    if generated.id != "guest-boot-initrd"
        || generated.generator_path != "scripts/build-guest-boot-initrd.py"
        || generated.cache_path != "bangbang/guest-boot/initrd.cpio"
        || generated.sha256 != INITRD_SHA256
        || generated.size_bytes != 54_272
        || generated.output_class != GuestOutputClass::DeterministicGeneratedCache
        || generated.determinism != GeneratedDeterminism::ByteIdentical
    {
        errors.push(
            "guest workflow audit requires the exact current deterministic initrd".to_string(),
        );
    }
    if !is_safe_relative_path(&generated.generator_path)
        || !is_safe_relative_path(&generated.cache_path)
        || !is_sha256(&generated.sha256)
    {
        errors.push(
            "guest workflow audit generated artifact has invalid paths or digest".to_string(),
        );
    }
}

fn validate_runtime_contract(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    if audit.guest_identity.path != "/etc/os-release"
        || audit.guest_identity.size_bytes != 400
        || audit.guest_identity.sha256 != GUEST_IDENTITY_SHA256
        || !is_sha256(&audit.guest_identity.sha256)
    {
        errors.push("guest workflow audit requires the exact pinned guest identity".to_string());
    }
    if audit.timeouts
        != (GuestWorkflowTimeouts {
            artifact_seconds: 600,
            build_seconds: 900,
            startup_seconds: 30,
            request_seconds: 5,
            guest_seconds: 60,
            terminate_seconds: 5,
        })
    {
        errors.push("guest workflow audit requires the exact bounded timeout policy".to_string());
    }
}

fn validate_ext4_recipes(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    let ids = audit
        .ext4_recipes
        .iter()
        .map(|recipe| recipe.id.as_str())
        .collect::<Vec<_>>();
    if ids != GUEST_EXT4_RECIPE_IDS {
        errors.push("guest workflow audit requires the exact ordered ext4 recipe set".to_string());
        return;
    }

    for (index, recipe) in audit.ext4_recipes.iter().enumerate() {
        let (variant, template, default_size, inputs): (&str, &str, &str, &[&str]) = if index == 0 {
            (
                "normal",
                "ubuntu-24.04-{size}.ext4",
                "1G",
                &[
                    GUEST_WORKFLOW_AUDIT_PATH,
                    "scripts/fetch-firecracker-rootfs.sh",
                    "scripts/guest_artifact_policy.py",
                ],
            )
        } else {
            (
                "direct-boot-v110",
                "ubuntu-24.04-{size}-direct-boot-v110.ext4",
                "512M",
                &[
                    GUEST_WORKFLOW_AUDIT_PATH,
                    "scripts/fetch-firecracker-rootfs.sh",
                    "scripts/guest/arm64-id-register-report.rs",
                    "scripts/guest/production_vmnet_certification.py",
                    "scripts/guest/specification-benchmark.rs",
                    "scripts/guest_artifact_policy.py",
                ],
            )
        };
        if recipe.source_artifact != "rootfs"
            || recipe.variant != variant
            || recipe.filename_template != template
            || recipe.default_size != default_size
            || recipe.minimum_size_bytes != 1024
            || recipe.classification != Ext4Classification::RecipeDeterministic
            || recipe.output_class != GuestOutputClass::VerifiedRepairableCache
            || recipe.tool_roles != TOOL_ROLES
            || recipe
                .tracked_inputs
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                != inputs
            || recipe.sidecar.schema_version != 1
            || recipe.sidecar.suffix != ".bangbang.json"
            || recipe.sidecar.fields != SIDECAR_FIELDS
            || recipe.sidecar.filesystem_check != "e2fsck -fn"
        {
            errors.push(format!(
                "guest workflow audit ext4 recipe has stale inputs, tools, or sidecar policy: {}",
                recipe.id
            ));
        }
        if !recipe
            .tracked_inputs
            .iter()
            .all(|path| is_safe_relative_path(path))
        {
            errors.push(format!(
                "guest workflow audit ext4 recipe has an unsafe tracked input: {}",
                recipe.id
            ));
        }
    }
}

fn validate_output_classes(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    let expected = [
        GuestOutputPolicy {
            id: GuestOutputClass::VerifiedRepairableCache,
            reuse: "manifest-size-sha-or-matching-sidecar-and-filesystem-check".to_string(),
            repair: "announced-validated-replacement".to_string(),
            publication: "validated-stage-with-sidecar-last-for-pairs".to_string(),
            collision: "reject-final-symlink-or-nonregular".to_string(),
            locking: "nonblocking-advisory".to_string(),
        },
        GuestOutputPolicy {
            id: GuestOutputClass::DeterministicGeneratedCache,
            reuse: "byte-identical".to_string(),
            repair: "announced-atomic-refresh".to_string(),
            publication: "validated-sibling-stage".to_string(),
            collision: "reject-final-symlink-or-nonregular".to_string(),
            locking: "nonblocking-advisory".to_string(),
        },
        GuestOutputPolicy {
            id: GuestOutputClass::CallerOwnedAbsentOnly,
            reuse: "byte-identical-when-explicitly-allowed".to_string(),
            repair: "never".to_string(),
            publication: "atomic-hard-link-no-clobber".to_string(),
            collision: "leave-occupied-destination-unchanged".to_string(),
            locking: "none".to_string(),
        },
        GuestOutputPolicy {
            id: GuestOutputClass::UniqueEphemeralSession,
            reuse: "never".to_string(),
            repair: "never".to_string(),
            publication: "owner-only-unique-creation".to_string(),
            collision: "fail-closed".to_string(),
            locking: "none".to_string(),
        },
    ];
    if audit.output_classes != expected {
        errors.push(
            "guest workflow audit requires the exact ordered output-class policy".to_string(),
        );
    }
}

fn validate_profiles(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    let ids = audit
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<Vec<_>>();
    if ids != GUEST_WORKFLOW_PROFILE_IDS {
        errors
            .push("guest workflow audit requires the exact ordered workflow profiles".to_string());
        return;
    }
    let terminal = audit.delivery.state == GuestWorkflowDeliveryState::Complete;
    for (index, profile) in audit.profiles.iter().enumerate() {
        let expected_mode = if index == 0 {
            GuestWorkflowMode::Api
        } else {
            GuestWorkflowMode::NoApi
        };
        let expected_implementation = vec![local_reference(
            "scripts/run-macos-guest-workflow.py",
            "def run_workflow(",
        )];
        let expected_validation = vec![local_reference(
            "scripts/run-integration-tests.sh",
            if index == 0 {
                "scripts/run-macos-guest-workflow.py api"
            } else {
                "scripts/run-macos-guest-workflow.py no-api"
            },
        )];
        let expected_state = if terminal {
            GuestWorkflowProfileState::ImplementedAndVerified
        } else {
            GuestWorkflowProfileState::Planned
        };
        let evidence_matches = if terminal {
            profile.implementation == expected_implementation
                && profile.validation == expected_validation
        } else {
            profile.implementation.is_empty() && profile.validation.is_empty()
        };
        if profile.state != expected_state
            || profile.mode != expected_mode
            || profile.kernel_artifact != "kernel"
            || profile.rootfs_artifact != "rootfs"
            || profile.initrd_artifact != "guest-boot-initrd"
            || profile.boot_args != BOOT_ARGS
            || !profile.rootfs_read_only
            || profile.success_marker != SUCCESS_MARKER
            || profile.failure_marker != FAILURE_MARKER
            || profile.shutdown != GuestShutdown::GuestPoweroff
            || profile.networking != GuestNetworking::None
            || profile.platform != "aarch64-apple-darwin-hvf"
            || !evidence_matches
        {
            errors.push(format!(
                "guest workflow profile does not match its exact delivery state: {}",
                profile.id
            ));
        }
    }
}

fn validate_nonclaims(audit: &GuestWorkflowAudit, errors: &mut Vec<String>) {
    if audit.nonclaims != NONCLAIMS {
        errors.push("guest workflow audit requires the exact ordered nonclaim set".to_string());
    }
}

fn validate_capability_transition(
    audit: &GuestWorkflowAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let capabilities = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    for (id, family, summary) in [
        ("corpus:getting-started", "process", GETTING_STARTED_SUMMARY),
        (
            "corpus:rootfs-and-kernel",
            "boot-and-lifecycle",
            ROOTFS_AND_KERNEL_SUMMARY,
        ),
    ] {
        match capabilities.get(id) {
            Some(capability) if audit.delivery.state == GuestWorkflowDeliveryState::Preparation => {
                if capability.disposition != Disposition::AuditRequired
                    || capability.delivery_issue.is_some()
                    || capability.exclusion.is_some()
                    || !capability.implementation.is_empty()
                    || !capability.validation.is_empty()
                {
                    errors.push(format!(
                        "guest workflow preparation requires {id} to remain exactly audit-required"
                    ));
                }
            }
            Some(capability) => {
                if capability.family != family
                    || capability.summary != summary
                    || capability.source_refs != [id]
                    || capability.disposition != Disposition::ImplementedAndVerified
                    || capability.implementation != terminal_capability_implementation()
                    || capability.validation != terminal_capability_validation()
                    || capability.delivery_issue.is_some()
                    || capability.exclusion.is_some()
                {
                    errors.push(format!(
                        "guest workflow completion requires exact implemented-and-verified evidence: {id}"
                    ));
                }
            }
            None => errors.push(format!("guest workflow capability is missing: {id}")),
        }
    }
}

fn terminal_capability_implementation() -> Vec<Reference> {
    vec![
        local_reference(
            "scripts/build-guest-boot-initrd.py",
            "def build_rootfs_poweroff_init_code(",
        ),
        local_reference(
            "scripts/guest_artifact_policy.py",
            "class GuestWorkflowProfile",
        ),
        local_reference("scripts/run-macos-guest-workflow.py", "def run_workflow("),
    ]
}

fn terminal_capability_validation() -> Vec<Reference> {
    vec![
        local_reference(
            "scripts/run-integration-tests.sh",
            "scripts/run-macos-guest-workflow.py api",
        ),
        local_reference(
            "scripts/run-integration-tests.sh",
            "scripts/run-macos-guest-workflow.py no-api",
        ),
        local_reference(
            "scripts/tests/test_macos_guest_workflow.py",
            "class MacosGuestWorkflowTests",
        ),
        local_reference(
            "tools/firecracker-capability-audit/tests/guest_workflow_audit.rs",
            "guest_workflow_terminal_scope_is_exact",
        ),
    ]
}

fn validate_evidence(
    audit: &GuestWorkflowAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let expected_implementation = if audit.delivery.state == GuestWorkflowDeliveryState::Complete {
        vec![
            local_reference(
                "scripts/build-guest-boot-initrd.py",
                "def build_rootfs_poweroff_init_code(",
            ),
            local_reference(
                "scripts/fetch-firecracker-kernel.sh",
                "guest_artifact_policy.py",
            ),
            local_reference(
                "scripts/fetch-firecracker-rootfs.sh",
                "--internal-populate-direct",
            ),
            local_reference(
                "scripts/guest_artifact_policy.py",
                "class GuestWorkflowProfile",
            ),
            local_reference("scripts/run-macos-guest-workflow.py", "def run_workflow("),
            local_reference("scripts/sign-hvf-binary.sh", "publish signed"),
        ]
    } else {
        vec![
            local_reference(
                "scripts/build-guest-boot-initrd.py",
                "publish_generated_bytes(",
            ),
            local_reference(
                "scripts/fetch-firecracker-kernel.sh",
                "guest_artifact_policy.py",
            ),
            local_reference(
                "scripts/fetch-firecracker-rootfs.sh",
                "--internal-populate-direct",
            ),
            local_reference(
                "scripts/guest_artifact_policy.py",
                "class ArtifactPolicyError",
            ),
            local_reference("scripts/sign-hvf-binary.sh", "publish signed"),
        ]
    };
    let expected_validation = if audit.delivery.state == GuestWorkflowDeliveryState::Complete {
        vec![
            local_reference(
                "scripts/run-integration-tests.sh",
                "scripts/run-macos-guest-workflow.py api",
            ),
            local_reference(
                "scripts/tests/test_guest_artifact_policy.py",
                "class GuestArtifactPolicyTests",
            ),
            local_reference(
                "scripts/tests/test_macos_guest_workflow.py",
                "class MacosGuestWorkflowTests",
            ),
            local_reference(
                "tools/firecracker-capability-audit/tests/guest_workflow_audit.rs",
                "checked_guest_workflow_audit_is_canonical_and_fail_closed",
            ),
        ]
    } else {
        vec![
            local_reference(
                "scripts/tests/test_guest_artifact_policy.py",
                "class GuestArtifactPolicyTests",
            ),
            local_reference(
                "tools/firecracker-capability-audit/tests/guest_workflow_audit.rs",
                "checked_guest_workflow_audit_is_canonical_and_fail_closed",
            ),
        ]
    };
    let expected_documentation = if audit.delivery.state == GuestWorkflowDeliveryState::Complete {
        vec![
            local_reference(
                "compat/firecracker/v1.16.0/guest-workflow-contract.md",
                "# macOS Guest Workflow Contract",
            ),
            local_reference("docs/macos-guest-workflow.md", "# macOS Guest Workflow"),
        ]
    } else {
        vec![
            local_reference(
                "compat/firecracker/v1.16.0/README.md",
                "Guest workflow artifact authority",
            ),
            local_reference("docs/testing.md", "## Guest Boot Artifacts"),
        ]
    };
    if audit.evidence.implementation != expected_implementation
        || audit.evidence.validation != expected_validation
        || audit.evidence.documentation != expected_documentation
    {
        errors
            .push("guest workflow audit requires exact categorized delivery evidence".to_string());
    }
    for (kind, references) in [
        ("implementation", audit.evidence.implementation.as_slice()),
        ("validation", audit.evidence.validation.as_slice()),
        ("documentation", audit.evidence.documentation.as_slice()),
    ] {
        if references.is_empty() {
            errors.push(format!("guest workflow audit requires {kind} evidence"));
        }
        if references
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            errors.push(format!(
                "guest workflow audit {kind} evidence must be sorted and unique"
            ));
        }
        for (index, reference) in references.iter().enumerate() {
            validate_reference(
                reference,
                repository_root,
                tracked,
                &format!("guest workflow audit {kind}[{index}]"),
                errors,
            );
            let Reference::Local {
                path,
                anchor: Some(anchor),
            } = reference
            else {
                errors.push(format!(
                    "guest workflow audit {kind} evidence must be anchored local evidence"
                ));
                continue;
            };
            match std::fs::read_to_string(repository_root.join(path)) {
                Ok(source) if source.contains(anchor) => {}
                Ok(_) => errors.push(format!(
                    "guest workflow audit {kind} anchor does not resolve: {path}: {anchor}"
                )),
                Err(_) => errors.push(format!(
                    "guest workflow audit {kind} path is unreadable: {path}"
                )),
            }
        }
    }
}

fn validate_source_tokens(repository_root: &Path, errors: &mut Vec<String>) {
    for (path, tokens) in [
        (
            ".github/workflows/ci.yml",
            &[
                "vmlinux-6.1.155",
                KERNEL_SHA256,
                ROOTFS_SHA256,
                "--guest-workflow-final",
            ][..],
        ),
        (
            "scripts/guest_artifact_policy.py",
            &[
                GUEST_WORKFLOW_AUDIT_PATH,
                "class GuestWorkflowProfile",
                "fetch",
                "prepare-ext4",
            ][..],
        ),
        (
            "scripts/fetch-firecracker-kernel.sh",
            &["guest_artifact_policy.py", "fetch kernel"][..],
        ),
        (
            "scripts/fetch-firecracker-rootfs.sh",
            &["direct-boot-v110", "guest_artifact_policy.py"][..],
        ),
        (
            "scripts/build-guest-boot-initrd.py",
            &[
                "load_manifest",
                "guest-boot-initrd",
                "publish_generated_bytes",
                "ROOTFS_WORKFLOW_OS_RELEASE",
                "rootfs-poweroff-init",
            ][..],
        ),
        (
            "scripts/run-macos-guest-workflow.py",
            &[
                "def run_workflow(",
                "EXPECTED_NO_CONTENT_RESPONSE",
                "profile.failure_marker.encode",
            ][..],
        ),
        (
            "scripts/run-integration-tests.sh",
            &[
                "scripts/run-macos-guest-workflow.py api",
                "scripts/run-macos-guest-workflow.py no-api",
            ][..],
        ),
        (
            "scripts/sign-hvf-binary.sh",
            &["guest_artifact_policy.py", "publish signed"][..],
        ),
        (
            "compat/firecracker/v1.16.0/guest-workflow-contract.md",
            &["# macOS Guest Workflow Contract", "corpus:getting-started"][..],
        ),
        (
            "docs/macos-guest-workflow.md",
            &[
                "# macOS Guest Workflow",
                "scripts/run-macos-guest-workflow.py api",
            ][..],
        ),
    ] {
        let source = match std::fs::read_to_string(repository_root.join(path)) {
            Ok(source) => source,
            Err(_) => {
                errors.push(format!("guest workflow audit source is unreadable: {path}"));
                continue;
            }
        };
        for token in tokens {
            if !source.contains(token) {
                errors.push(format!(
                    "guest workflow audit source token drifted: {path}: {token}"
                ));
            }
        }
    }
}

fn validate_canonical_bytes(
    audit: &GuestWorkflowAudit,
    repository_root: &Path,
    errors: &mut Vec<String>,
) {
    let canonical = match guest_workflow_audit_json(audit) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!(
                "failed to serialize canonical guest workflow audit: {error}"
            ));
            return;
        }
    };
    match std::fs::read(repository_root.join(GUEST_WORKFLOW_AUDIT_PATH)) {
        Ok(checked) if checked == canonical => {}
        Ok(_) => errors.push("checked guest workflow audit bytes are not canonical".to_string()),
        Err(_) => errors.push("checked guest workflow audit is unreadable".to_string()),
    }
}

fn local_reference(path: &str, anchor: &str) -> Reference {
    Reference::Local {
        path: path.to_string(),
        anchor: Some(anchor.to_string()),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_digest_and_relative_path_shapes() {
        assert!(is_sha256(KERNEL_SHA256));
        assert!(!is_sha256("ABC"));
        assert!(is_safe_relative_path("a/b"));
        assert!(!is_safe_relative_path("../a"));
        assert!(!is_safe_relative_path("/a"));
    }
}
