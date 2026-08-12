use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::inventory_phase::{
    InventoryPhase, WAVE8_SUCCESSOR_ID, classify_inventory_phase, disposition_counts,
    expected_disposition, expected_nonterminal_ids,
};
use crate::validate::{tracked_repository_files, validate_reference};
use crate::{
    CapabilityInventory, Disposition, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION,
    Reference, SourceManifest, ValidationErrors, Wave7AggregateAudit, Wave7AggregateNonclaim,
    Wave7DesignOutcome, Wave7DesignSection, Wave7DeviceApiSection, Wave7HandoffOwner,
    Wave7ReleaseOutcome, Wave7ReleaseSection, Wave7Tool, Wave7ToolExecution, Wave7VirtioMmioClaim,
};

/// Current Wave 7 aggregate authority schema.
pub const WAVE7_AGGREGATE_AUDIT_SCHEMA_VERSION: u32 = 1;
/// Repository-relative Wave 7 aggregate authority path.
pub const WAVE7_AGGREGATE_AUDIT_PATH: &str =
    "compat/firecracker/v1.16.0/wave7-aggregate-audit.json";
/// Exact capability transition owned by #1799.
pub const WAVE7_AGGREGATE_CAPABILITY_IDS: [&str; 5] = [
    "corpus:design",
    "corpus:device-api",
    "corpus:release-changelog",
    "semantic.tools:packaging-help-errors-and-applicable-operations",
    "semantic.transport:virtio-mmio-activation",
];

const DESIGN_RECORDS: [(Wave7DesignSection, &str, Wave7DesignOutcome); 37] = [
    (
        Wave7DesignSection::ScopeAndFeatures,
        "semantic.specification:api-availability-stability-and-failure-information",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::ScopeAndFeatures,
        "semantic.specification:performance-resource-and-telemetry-outcomes",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::HostIntegration,
        "semantic.network:virtio-net-vmnet-policy-and-connectivity",
        Wave7DesignOutcome::Handoff1378,
    ),
    (
        Wave7DesignSection::HostIntegration,
        "semantic.process:cli-config-readiness-and-api-socket",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::HostIntegration,
        "semantic.process:instance-identity-and-version-output",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::HostIntegration,
        "semantic.process:signals-exits-fd-and-cleanup",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::HostIntegration,
        "semantic.storage:block-sync-async-vhost-and-limits",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::HostIntegration,
        "semantic.storage:pmem-root-mapping-flush-and-state",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::InternalArchitecture,
        "semantic.hotplug:runtime-device-manager",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::InternalArchitecture,
        "semantic.lifecycle:pause-resume-quiescence-and-failure",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::InternalArchitecture,
        "semantic.lifecycle:smp-psci-and-vcpu-ownership",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::InternalArchitecture,
        "semantic.transport:pci-msi-and-coexistence",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::InternalArchitecture,
        "semantic.transport:virtio-mmio-activation",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::ThreatContainment,
        "semantic.cross-capability:state-errors-metrics-security-and-snapshots",
        Wave7DesignOutcome::HandoffWave8,
    ),
    (
        Wave7DesignSection::ThreatContainment,
        "semantic.isolation:host-resource-authority-and-brokerage",
        Wave7DesignOutcome::Handoff1351,
    ),
    (
        Wave7DesignSection::ThreatContainment,
        "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes",
        Wave7DesignOutcome::Handoff1351,
    ),
    (
        Wave7DesignSection::ThreatContainment,
        "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
        Wave7DesignOutcome::Handoff1351,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.boot:arm64-cache-fdt",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.boot:kernel-rootfs-fdt-and-cache",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.cpu:configuration-templates-and-feature-state",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.device:entropy-queues-limits-metrics-and-state",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.device:rtc-vmclock-vmgenid-and-pvtime",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.device:serial-stdin-stdout-rx-and-restore",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MachineModel,
        "semantic.memory:machine-sizing-hugepages-and-dirty-tracking",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::StorageNetworkingAndRateLimiting,
        "semantic.mmds:tcp-token-session-and-isolation",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::StorageNetworkingAndRateLimiting,
        "semantic.vsock:live-routing-credit-events-and-cleanup",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::StorageNetworkingAndRateLimiting,
        "semantic.vsock:snapshot-override-reset-and-rx-gating",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MetadataAndSandboxing,
        "semantic.snapshot:diff-dirty-tracking-and-memory-backends",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MetadataAndSandboxing,
        "semantic.snapshot:editor-rebase-and-inspection",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MetadataAndSandboxing,
        "semantic.snapshot:full-create-load-and-public-lifecycle",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MetadataAndSandboxing,
        "semantic.snapshot:multi-vcpu-drives-devices-and-mmds",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MetadataAndSandboxing,
        "semantic.snapshot:network-vsock-overrides-portability-and-clones",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MonitoringAndTooling,
        "semantic.observability:logger-delivery-filtering-loss-and-redaction",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MonitoringAndTooling,
        "semantic.observability:metrics-schema-producers-flush-and-lifecycle",
        Wave7DesignOutcome::Implemented,
    ),
    (
        Wave7DesignSection::MonitoringAndTooling,
        "semantic.tools:packaging-help-errors-and-applicable-operations",
        Wave7DesignOutcome::Implemented,
    ),
];

const REQUIRED_RELATIONS: [&str; 62] = [
    "endpoints|drives/{id}|virtio-block|api-path:/drives/{drive_id}|implemented",
    "endpoints|drives/{id}|vhost-user-block|api-path:/drives/{drive_id}|implemented",
    "endpoints|hotplug/memory|virtio-mem|api-path:/hotplug/memory|implemented",
    "endpoints|mmds|virtio-net|api-path:/mmds|implemented",
    "endpoints|mmds/config|virtio-net|api-path:/mmds/config|implemented",
    "endpoints|network-interfaces/{id}|virtio-net|api-path:/network-interfaces/{iface_id}|implemented",
    "endpoints|entropy|virtio-rng|api-path:/entropy|implemented",
    "endpoints|pmem/{id}|virtio-pmem|api-path:/pmem/{id}|implemented",
    "endpoints|serial|serial console|api-path:/serial|implemented",
    "input-schema|Drive.drive_id|virtio-block|api-property:Drive.drive_id|implemented",
    "input-schema|Drive.drive_id|vhost-user-block|api-property:Drive.drive_id|implemented",
    "input-schema|Drive.is_read_only|virtio-block|api-property:Drive.is_read_only|implemented",
    "input-schema|Drive.is_root_device|virtio-block|api-property:Drive.is_root_device|implemented",
    "input-schema|Drive.is_root_device|vhost-user-block|api-property:Drive.is_root_device|implemented",
    "input-schema|Drive.partuuid|virtio-block|api-property:Drive.partuuid|implemented",
    "input-schema|Drive.partuuid|vhost-user-block|api-property:Drive.partuuid|implemented",
    "input-schema|Drive.path_on_host|virtio-block|api-property:Drive.path_on_host|implemented",
    "input-schema|Drive.rate_limiter|virtio-block|api-property:Drive.rate_limiter|implemented",
    "input-schema|Drive.socket|vhost-user-block|api-property:Drive.socket|implemented",
    "input-schema|MmdsConfig.network_interfaces|virtio-net|api-property:MmdsConfig.network_interfaces|implemented",
    "input-schema|MmdsConfig.version|virtio-net|api-property:MmdsConfig.version|implemented",
    "input-schema|MmdsConfig.ipv4_address|virtio-net|api-property:MmdsConfig.ipv4_address|implemented",
    "input-schema|NetworkInterface.guest_mac|virtio-net|api-property:NetworkInterface.guest_mac|implemented",
    "input-schema|NetworkInterface.host_dev_name|virtio-net|api-property:NetworkInterface.host_dev_name|implemented",
    "input-schema|NetworkInterface.iface_id|virtio-net|api-property:NetworkInterface.iface_id|implemented",
    "input-schema|NetworkInterface.mtu|virtio-net|api-property:NetworkInterface.mtu|implemented",
    "input-schema|NetworkInterface.rx_rate_limiter|virtio-net|api-property:NetworkInterface.rx_rate_limiter|implemented",
    "input-schema|NetworkInterface.tx_rate_limiter|virtio-net|api-property:NetworkInterface.tx_rate_limiter|implemented",
    "input-schema|PartialDrive.drive_id|virtio-block|api-property:PartialDrive.drive_id|implemented",
    "input-schema|PartialDrive.path_on_host|virtio-block|api-property:PartialDrive.path_on_host|implemented",
    "input-schema|PartialNetworkInterface.iface_id|virtio-net|api-property:PartialNetworkInterface.iface_id|implemented",
    "input-schema|PartialNetworkInterface.rx_rate_limiter|virtio-net|api-property:PartialNetworkInterface.rx_rate_limiter|implemented",
    "input-schema|PartialNetworkInterface.tx_rate_limiter|virtio-net|api-property:PartialNetworkInterface.tx_rate_limiter|implemented",
    "input-schema|RateLimiter.bandwidth|virtio-net|api-property:RateLimiter.bandwidth|implemented",
    "input-schema|RateLimiter.ops|virtio-block|api-property:RateLimiter.ops|implemented",
    "input-schema|TokenBucket.one_time_burst|virtio-block|api-property:TokenBucket.one_time_burst|implemented",
    "input-schema|TokenBucket.refill_time|virtio-block|api-property:TokenBucket.refill_time|implemented",
    "input-schema|TokenBucket.size|virtio-block|api-property:TokenBucket.size|implemented",
    "input-schema|TokenBucket.one_time_burst|virtio-net|api-property:TokenBucket.one_time_burst|implemented",
    "input-schema|TokenBucket.refill_time|virtio-net|api-property:TokenBucket.refill_time|implemented",
    "input-schema|TokenBucket.size|virtio-net|api-property:TokenBucket.size|implemented",
    "input-schema|Vsock.guest_cid|virtio-vsock|api-property:Vsock.guest_cid|implemented",
    "input-schema|Vsock.uds_path|virtio-vsock|api-property:Vsock.uds_path|implemented",
    "input-schema|Vsock.vsock_id|virtio-vsock|api-property:Vsock.vsock_id|implemented",
    "input-schema|EntropyDevice.rate_limiter|virtio-rng|api-property:EntropyDevice.rate_limiter|implemented",
    "input-schema|Pmem.id|virtio-pmem|api-property:Pmem.id|implemented",
    "input-schema|Pmem.path_on_host|virtio-pmem|api-property:Pmem.path_on_host|implemented",
    "input-schema|Pmem.root_device|virtio-pmem|api-property:Pmem.root_device|implemented",
    "input-schema|Pmem.read_only|virtio-pmem|api-property:Pmem.read_only|implemented",
    "input-schema|Pmem.rate_limiter|virtio-pmem|api-property:Pmem.rate_limiter|implemented",
    "input-schema|PartialPmem.id|virtio-pmem|api-property:PartialPmem.id|implemented",
    "input-schema|PartialPmem.rate_limiter|virtio-pmem|api-property:PartialPmem.rate_limiter|implemented",
    "input-schema|MemoryHotplugConfig.total_size_mib|virtio-mem|api-property:MemoryHotplugConfig.total_size_mib|implemented",
    "input-schema|MemoryHotplugConfig.slot_size_mib|virtio-mem|api-property:MemoryHotplugConfig.slot_size_mib|implemented",
    "input-schema|MemoryHotplugConfig.block_size_mi|virtio-mem|api-property:MemoryHotplugConfig.block_size_mib|implemented",
    "input-schema|MemoryHotplugSizeUpdate.requested_size_mib|virtio-mem|api-property:MemoryHotplugSizeUpdate.requested_size_mib|implemented",
    "output-schema|MemoryHotplugStatus.total_size_mib|virtio-mem|api-property:MemoryHotplugStatus.total_size_mib|implemented",
    "output-schema|MemoryHotplugStatus.slot_size_mib|virtio-mem|api-property:MemoryHotplugStatus.slot_size_mib|implemented",
    "output-schema|MemoryHotplugStatus.block_size_mib|virtio-mem|api-property:MemoryHotplugStatus.block_size_mib|implemented",
    "output-schema|MemoryHotplugStatus.plugged_size_mib|virtio-mem|api-property:MemoryHotplugStatus.plugged_size_mib|implemented",
    "output-schema|MemoryHotplugStatus.requested_size_mib|virtio-mem|api-property:MemoryHotplugStatus.requested_size_mib|implemented",
    "instance-actions|SendCtrlAltDel|keyboard|corpus:actions-api|arm64-rejected",
];

const RELEASE_ENTRIES: [(&str, Wave7ReleaseSection, &str, Wave7ReleaseOutcome); 21] = [
    (
        "added-5786-pci-hotplug",
        Wave7ReleaseSection::Added,
        "semantic.hotplug:runtime-device-manager",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5323-vsock-restore-override",
        Wave7ReleaseSection::Added,
        "semantic.vsock:snapshot-override-reset-and-rx-gating",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5824-serial-rate-limiter",
        Wave7ReleaseSection::Added,
        "semantic.device:serial-stdin-stdout-rx-and-restore",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5799-log-callsite-rate-limiter",
        Wave7ReleaseSection::Added,
        "semantic.observability:logger-delivery-filtering-loss-and-redaction",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5789-pmem-rate-limiter",
        Wave7ReleaseSection::Added,
        "semantic.storage:pmem-root-mapping-flush-and-state",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5872-vsock-event-idx",
        Wave7ReleaseSection::Added,
        "semantic.vsock:live-routing-credit-events-and-cleanup",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5828-network-mtu",
        Wave7ReleaseSection::Added,
        "api-property:NetworkInterface.mtu",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-5906-arm64-rng-seed",
        Wave7ReleaseSection::Added,
        "semantic.boot:arm64-cache-fdt",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "added-linux-6.18-host-kernels",
        Wave7ReleaseSection::Added,
        "corpus:production-host",
        Wave7ReleaseOutcome::LinuxHostHandoff1373,
    ),
    (
        "fixed-5762-entropy-request-cap",
        Wave7ReleaseSection::Fixed,
        "semantic.device:entropy-queues-limits-metrics-and-state",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5760-vmgenid-hid",
        Wave7ReleaseSection::Fixed,
        "semantic.device:rtc-vmclock-vmgenid-and-pvtime",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5764-uart-snapshot-restore",
        Wave7ReleaseSection::Fixed,
        "semantic.device:serial-stdin-stdout-rx-and-restore",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5780-arm64-cache-fdt",
        Wave7ReleaseSection::Fixed,
        "semantic.boot:arm64-cache-fdt",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5793-virtio-mem-slot-accounting",
        Wave7ReleaseSection::Fixed,
        "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5794-balloon-stat-length",
        Wave7ReleaseSection::Fixed,
        "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5809-clock-realtime",
        Wave7ReleaseSection::Fixed,
        "api-property:SnapshotLoadParams.clock_realtime",
        Wave7ReleaseOutcome::Arm64Rejected,
    ),
    (
        "fixed-5738-x86-kvm-msr-range",
        Wave7ReleaseSection::Fixed,
        "api-schema:MsrModifier",
        Wave7ReleaseOutcome::ProvenPlatformImpossible,
    ),
    (
        "fixed-5818-pci-status-sequencing",
        Wave7ReleaseSection::Fixed,
        "semantic.transport:pci-msi-and-coexistence",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5818-mmio-clear-status-bits",
        Wave7ReleaseSection::Fixed,
        "semantic.transport:virtio-mmio-activation",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5884-balloon-hinting-204",
        Wave7ReleaseSection::Fixed,
        "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
        Wave7ReleaseOutcome::Implemented,
    ),
    (
        "fixed-5882-vsock-rx-reset-gate",
        Wave7ReleaseSection::Fixed,
        "semantic.vsock:snapshot-override-reset-and-rx-gating",
        Wave7ReleaseOutcome::Implemented,
    ),
];

const MMIO_CLAIMS: [Wave7VirtioMmioClaim; 12] = [
    Wave7VirtioMmioClaim::Identity,
    Wave7VirtioMmioClaim::FeatureNegotiation,
    Wave7VirtioMmioClaim::QueueSelectionAndConfiguration,
    Wave7VirtioMmioClaim::QueueNotification,
    Wave7VirtioMmioClaim::InterruptDeliveryAndAcknowledgement,
    Wave7VirtioMmioClaim::OrderedStatusTransitions,
    Wave7VirtioMmioClaim::Reset,
    Wave7VirtioMmioClaim::ActivationFailure,
    Wave7VirtioMmioClaim::DeviceConfigurationAccess,
    Wave7VirtioMmioClaim::TransportStateRestore,
    Wave7VirtioMmioClaim::TypedLogging,
    Wave7VirtioMmioClaim::RedactedTracing,
];

const NONCLAIMS: [Wave7AggregateNonclaim; 6] = [
    Wave7AggregateNonclaim::FirecrackerBinaryOrLinuxKvmParity,
    Wave7AggregateNonclaim::RetainedHandoffCompletion,
    Wave7AggregateNonclaim::PciEvidenceForMmio,
    Wave7AggregateNonclaim::PortablePerformanceThreshold,
    Wave7AggregateNonclaim::TrackedEnvironmentReport,
    Wave7AggregateNonclaim::Wave8InteractionCompletion,
];

/// Validate the checked, source-complete Wave 7 aggregate authority.
pub fn validate_wave7_aggregate_audit(
    audit: &Wave7AggregateAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    validate_baseline_and_sources(audit, manifest, &mut errors);
    validate_scope_and_counts(audit, inventory, &mut errors);
    validate_design(audit, inventory, &mut errors);
    validate_device_api(audit, manifest, inventory, &mut errors);
    validate_release(audit, inventory, &mut errors);

    let tracked = tracked_repository_files(repository_root, &mut errors);
    validate_tools(
        audit,
        manifest,
        inventory,
        repository_root,
        &tracked,
        &mut errors,
    );
    validate_mmio(audit, inventory, repository_root, &tracked, &mut errors);
    validate_handoffs(audit, inventory, &mut errors);
    validate_evidence_and_docs(audit, repository_root, &tracked, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_messages(errors))
    }
}

fn validate_baseline_and_sources(
    audit: &Wave7AggregateAudit,
    manifest: &SourceManifest,
    errors: &mut Vec<String>,
) {
    if audit.schema_version != WAVE7_AGGREGATE_AUDIT_SCHEMA_VERSION {
        errors.push(format!(
            "Wave 7 aggregate audit schema_version must be {WAVE7_AGGREGATE_AUDIT_SCHEMA_VERSION}"
        ));
    }
    if audit.baseline.version != FIRECRACKER_VERSION
        || audit.baseline.commit != FIRECRACKER_COMMIT
        || audit.baseline.target != FIRECRACKER_TARGET
    {
        errors.push("Wave 7 aggregate audit baseline is not pinned".to_string());
    }
    if audit.parent_issue != "#1491" || audit.delivery_issue != "#1799" {
        errors.push("Wave 7 aggregate ownership must be #1491/#1799".to_string());
    }

    let expected = [
        (
            "corpus:design",
            "docs/design.md",
            "entire-file",
            "143fef76410e4f7e45b32d3986e0d78eedf5175a",
        ),
        (
            "corpus:device-api",
            "docs/device-api.md",
            "entire-file",
            "f638cc889f32e0be32fd750a79e65f88cb5c65a1",
        ),
        (
            "corpus:release-changelog",
            "CHANGELOG.md",
            "v1.16.0",
            "d76cfc8a4601638f45fea032982faef8b9e30742",
        ),
    ];
    let actual = audit
        .upstream_sources
        .iter()
        .map(|source| {
            (
                source.capability_id.as_str(),
                source.path.as_str(),
                source.anchor.as_str(),
                source.git_blob.as_str(),
            )
        })
        .collect::<Vec<_>>();
    if actual != expected {
        errors.push("Wave 7 aggregate requires the exact ordered pinned sources".to_string());
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
    for (id, path, anchor, blob) in expected {
        match source_items.get(id) {
            Some(item) if item.path == path && item.anchor == anchor && item.kind == "corpus" => {}
            Some(_) => errors.push(format!("Wave 7 aggregate source identity drifted: {id}")),
            None => errors.push(format!("Wave 7 aggregate source identity is missing: {id}")),
        }
        match inputs.get(path) {
            Some(input) if input.git_blob == blob => {}
            Some(_) => errors.push(format!("Wave 7 aggregate source blob drifted: {path}")),
            None => errors.push(format!("Wave 7 aggregate source input is missing: {path}")),
        }
    }
}

fn validate_scope_and_counts(
    audit: &Wave7AggregateAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    if audit
        .capability_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != WAVE7_AGGREGATE_CAPABILITY_IDS
    {
        errors.push("Wave 7 aggregate requires the exact five #1799 capabilities".to_string());
    }
    if (
        audit.target_counts.implemented_and_verified,
        audit.target_counts.audit_required,
        audit.target_counts.missing_platform_feasible,
        audit.target_counts.proven_platform_impossible,
    ) != (376, 9, 3, 30)
    {
        errors.push("Wave 7 aggregate target counts must be 376/9/3/30".to_string());
    }

    let counts = disposition_counts(inventory);
    match classify_inventory_phase(inventory) {
        Ok(
            InventoryPhase::Wave7
            | InventoryPhase::Wave8
            | InventoryPhase::JailerUidGidPlatformLimit
            | InventoryPhase::JailerChrootPlatformLimit
            | InventoryPhase::JailerAggregate
            | InventoryPhase::MultiprocessIsolation
            | InventoryPhase::HostResourceAuthority
            | InventoryPhase::JailerSeccompContainment,
        ) => {}
        Ok(phase) => errors.push(format!(
            "Wave 7 aggregate inventory cannot use the earlier {} phase",
            phase.name()
        )),
        Err(error) => errors.push(format!(
            "Wave 7 aggregate inventory must be its exact 376/9/3/30 phase, the exact Wave 8 377/8/3/30 successor, the exact post-Wave-8 jailer uid/gid 377/6/3/32 successor, the exact post-uid/gid jailer chroot-base-dir 377/5/3/33 successor, the exact aggregate jailer 379/3/3/33 successor, the exact multiprocess isolation 380/3/2/33 successor, the exact host-resource authority 381/3/1/33 successor, or the exact jailer/seccomp containment 382/3/0/33 successor; found {}/{}/{}/{}: {error}",
            counts.0, counts.1, counts.2, counts.3
        )),
    }
}

fn validate_design(
    audit: &Wave7AggregateAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .design
        .iter()
        .map(|record| {
            (
                record.section,
                record.capability_id.as_str(),
                record.outcome,
            )
        })
        .collect::<Vec<_>>();
    if actual != DESIGN_RECORDS {
        errors
            .push("Wave 7 design ledger must be the exact ordered 37-record partition".to_string());
    }
    let ids = audit
        .design
        .iter()
        .map(|record| record.capability_id.as_str())
        .collect::<BTreeSet<_>>();
    if ids.len() != audit.design.len() {
        errors.push("Wave 7 design ledger contains duplicate semantic identities".to_string());
    }
    let semantic_ids = inventory
        .capabilities
        .iter()
        .filter(|capability| capability.id.starts_with("semantic."))
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    if ids != semantic_ids {
        errors.push(format!(
            "Wave 7 design ledger is not a bijection over semantic capabilities: expected {semantic_ids:?}, found {ids:?}"
        ));
    }

    let capabilities = capability_map(inventory);
    let phase = classify_inventory_phase(inventory).ok();
    for record in &audit.design {
        let expected = match record.outcome {
            Wave7DesignOutcome::Implemented => Disposition::ImplementedAndVerified,
            Wave7DesignOutcome::Handoff1351 => phase
                .map_or(Disposition::MissingPlatformFeasible, |phase| {
                    expected_disposition(phase, &record.capability_id)
                }),
            Wave7DesignOutcome::Handoff1378 => Disposition::AuditRequired,
            Wave7DesignOutcome::HandoffWave8 => phase.map_or(Disposition::AuditRequired, |phase| {
                expected_disposition(phase, WAVE8_SUCCESSOR_ID)
            }),
        };
        if capabilities
            .get(record.capability_id.as_str())
            .is_none_or(|capability| capability.disposition != expected)
        {
            errors.push(format!(
                "Wave 7 design outcome does not match its producer: {}",
                record.capability_id
            ));
        }
    }
}

fn validate_device_api(
    audit: &Wave7AggregateAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let columns9 = vec![
        "keyboard",
        "serial console",
        "virtio-block",
        "vhost-user-block",
        "virtio-net",
        "virtio-vsock",
        "virtio-rng",
        "virtio-pmem",
        "virtio-mem",
    ];
    let columns7 = vec![
        "keyboard",
        "serial console",
        "virtio-block",
        "vhost-user-block",
        "virtio-net",
        "virtio-vsock",
        "virtio-mem",
    ];
    let columns6 = vec![
        "keyboard",
        "serial console",
        "virtio-block",
        "vhost-user-block",
        "virtio-net",
        "virtio-vsock",
    ];
    let dimensions = audit
        .device_api
        .dimensions
        .iter()
        .map(|dimension| {
            (
                dimension.section,
                dimension.rows,
                dimension
                    .device_columns
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                dimension.required_relations,
            )
        })
        .collect::<Vec<_>>();
    let expected_dimensions = vec![
        (Wave7DeviceApiSection::Endpoints, 17, columns9.clone(), 9),
        (Wave7DeviceApiSection::InputSchema, 75, columns9, 47),
        (Wave7DeviceApiSection::OutputSchema, 16, columns7, 5),
        (Wave7DeviceApiSection::InstanceActions, 3, columns6, 1),
    ];
    if dimensions != expected_dimensions {
        errors.push("Wave 7 device API dimensions or device columns drifted".to_string());
    }
    let cells = audit
        .device_api
        .dimensions
        .iter()
        .map(|dimension| dimension.rows * dimension.device_columns.len())
        .sum::<usize>();
    if cells != 958
        || audit.device_api.optional_relations != 896
        || audit.device_api.required_relations.len() != 62
    {
        errors.push("Wave 7 device API relation totals must be 958/62/896".to_string());
    }
    if audit.device_api.required_relations != REQUIRED_RELATIONS {
        errors
            .push("Wave 7 device API requires the exact ordered 62 relation mappings".to_string());
    }
    let normalizations = audit
        .device_api
        .normalizations
        .iter()
        .map(|normalization| {
            (
                normalization.source.as_str(),
                normalization.current.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let expected_normalizations = [
        ("CreateSnapshotParams", "SnapshotCreateParams"),
        ("LoadSnapshotParams", "SnapshotLoadParams"),
        ("SerialConfig", "SerialDevice"),
        (
            "MemoryHotplugConfig.block_size_mi",
            "MemoryHotplugConfig.block_size_mib",
        ),
    ];
    if normalizations != expected_normalizations {
        errors.push("Wave 7 device API schema/property normalizations drifted".to_string());
    }

    let capabilities = capability_map(inventory);
    for relation in &audit.device_api.required_relations {
        let mut fields = relation.split('|');
        let (Some(_section), Some(_row), Some(_device), Some(producer), Some(result), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            errors.push(format!(
                "Wave 7 device API relation is malformed: {relation}"
            ));
            continue;
        };
        match capabilities.get(producer) {
            Some(capability)
                if capability.disposition == Disposition::ImplementedAndVerified
                    && matches!(result, "implemented" | "arm64-rejected") => {}
            Some(_) => errors.push(format!(
                "Wave 7 device API producer is not implemented: {producer}"
            )),
            None => errors.push(format!(
                "Wave 7 device API producer capability is missing: {producer}"
            )),
        }
    }

    let api_ids = manifest
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind.as_str(),
                "api-operation" | "api-path" | "api-schema" | "api-property"
            ) || item.id == "corpus:actions-api"
        })
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut kind_counts = BTreeMap::<&str, usize>::new();
    for item in manifest
        .items
        .iter()
        .filter(|item| api_ids.contains(item.id.as_str()))
    {
        *kind_counts.entry(item.kind.as_str()).or_default() += 1;
    }
    let population = &audit.device_api.api_population;
    if (
        population.operations,
        population.paths,
        population.schemas,
        population.properties,
        population.actions_corpus,
        population.implemented,
        population.proven_platform_impossible,
    ) != (38, 26, 44, 152, 1, 240, 21)
        || kind_counts.get("api-operation") != Some(&38)
        || kind_counts.get("api-path") != Some(&26)
        || kind_counts.get("api-schema") != Some(&44)
        || kind_counts.get("api-property") != Some(&152)
        || kind_counts.get("corpus") != Some(&1)
        || api_ids.len() != 261
    {
        errors.push("Wave 7 device API population must derive as 38/26/44/152/1".to_string());
    }
    let mut implemented = 0;
    let mut impossible = 0;
    for id in &api_ids {
        match capabilities.get(id) {
            Some(capability) if capability.disposition == Disposition::ImplementedAndVerified => {
                implemented += 1;
            }
            Some(capability) if capability.disposition == Disposition::ProvenPlatformImpossible => {
                impossible += 1;
            }
            Some(_) => errors.push(format!("Wave 7 device API producer is nonterminal: {id}")),
            None => errors.push(format!("Wave 7 device API capability is missing: {id}")),
        }
    }
    if (implemented, impossible) != (240, 21) {
        errors.push(format!(
            "Wave 7 device API terminal partition must be 240/21, found {implemented}/{impossible}"
        ));
    }
}

fn validate_release(
    audit: &Wave7AggregateAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let actual = audit
        .release_entries
        .iter()
        .map(|entry| {
            (
                entry.id.as_str(),
                entry.section,
                entry.producer_capability_id.as_str(),
                entry.outcome,
            )
        })
        .collect::<Vec<_>>();
    if actual != RELEASE_ENTRIES {
        errors.push(
            "Wave 7 release ledger requires the exact ordered 9 Added and 12 Fixed entries"
                .to_string(),
        );
    }
    if audit
        .release_entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        != 21
    {
        errors.push("Wave 7 release ledger contains duplicate entry identities".to_string());
    }
    let capabilities = capability_map(inventory);
    for entry in &audit.release_entries {
        let expected = match entry.outcome {
            Wave7ReleaseOutcome::Implemented | Wave7ReleaseOutcome::Arm64Rejected => {
                Disposition::ImplementedAndVerified
            }
            Wave7ReleaseOutcome::ProvenPlatformImpossible => Disposition::ProvenPlatformImpossible,
            Wave7ReleaseOutcome::LinuxHostHandoff1373 => Disposition::AuditRequired,
        };
        if capabilities
            .get(entry.producer_capability_id.as_str())
            .is_none_or(|capability| capability.disposition != expected)
        {
            errors.push(format!(
                "Wave 7 release producer outcome drifted: {}",
                entry.id
            ));
        }
    }
    let issue_5818 = audit
        .release_entries
        .iter()
        .filter(|entry| entry.id.starts_with("fixed-5818-"))
        .count();
    if issue_5818 != 2 {
        errors.push("Wave 7 release ledger must retain two independent #5818 entries".to_string());
    }
}

fn validate_tools(
    audit: &Wave7AggregateAudit,
    manifest: &SourceManifest,
    inventory: &CapabilityInventory,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    let expected = [
        (
            Wave7Tool::CpuTemplateHelper,
            "cpu-template-helper/",
            "bangbang-cpu-template-helper",
            &["cpu-template-helper"][..],
            Wave7ToolExecution::NativeSigned,
            (18, 18, 0, 0),
            &[
                "help-and-version",
                "strict-arguments",
                "canonical-input-output",
                "collision-and-nonmutation",
                "signed-effective-hvf",
            ][..],
            &[(
                "tools/cpu-template-helper/tests/cli.rs",
                "fn help_and_version_are_the_only_portable_stdout_successes()",
            )][..],
        ),
        (
            Wave7Tool::Firecracker,
            "firecracker/",
            "bangbang",
            &["bangbang"][..],
            Wave7ToolExecution::ProductAlternative,
            (1, 1, 0, 0),
            &[
                "help-version-and-errors",
                "strict-config-and-api-socket",
                "signed-process-lifecycle",
            ][..],
            &[(
                "crates/bangbang/tests/process_e2e.rs",
                "fn executable_prints_help_and_exits_before_socket_publication()",
            )][..],
        ),
        (
            Wave7Tool::Jailer,
            "jailer/",
            "bangbang-launcher",
            &["bangbang-bundle", "bangbang-launcher"][..],
            Wave7ToolExecution::ProductAlternative,
            (14, 5, 5, 4),
            &[
                "help-and-version",
                "strict-arguments",
                "production-bundle-alternative",
                "explicit-linux-exclusions",
                "retained-identity-handoffs",
            ][..],
            &[(
                "crates/launcher/tests/production_bundle_e2e.rs",
                "fn launcher_exposes_exact_jailer_help_version_and_policy_validation()",
            )][..],
        ),
        (
            Wave7Tool::RebaseSnap,
            "rebase-snap/",
            "bangbang-snapshot-tools",
            &["rebase-snap"][..],
            Wave7ToolExecution::DeprecatedPortable,
            (3, 3, 0, 0),
            &[
                "help-and-errors",
                "deprecated-operation",
                "canonical-no-clobber-rebase",
                "signed-restore",
            ][..],
            &[(
                "tools/snapshot-tools/tests/cli.rs",
                "fn help_and_version_expose_the_selected_firecracker_surfaces()",
            )][..],
        ),
        (
            Wave7Tool::Seccompiler,
            "seccompiler/",
            "bangbang-seccompiler",
            &["seccompiler-bin"][..],
            Wave7ToolExecution::PortableOffline,
            (6, 6, 0, 0),
            &[
                "help-and-version",
                "strict-arguments",
                "canonical-artifact",
                "linux-oracle-semantics",
            ][..],
            &[(
                "tools/seccompiler/tests/cli.rs",
                "fn help_and_version_identify_the_offline_compatibility_tool()",
            )][..],
        ),
        (
            Wave7Tool::SnapshotEditor,
            "snapshot-editor/",
            "bangbang-snapshot-tools",
            &["snapshot-editor"][..],
            Wave7ToolExecution::PortableOffline,
            (13, 13, 0, 0),
            &[
                "help-and-errors",
                "nested-operations",
                "canonical-no-clobber-output",
                "signed-full-and-diff-restore",
            ][..],
            &[(
                "tools/snapshot-tools/tests/cli.rs",
                "fn invalid_invocations_are_deterministic_and_do_not_echo_values()",
            )][..],
        ),
    ];
    if audit.tools.len() != expected.len() {
        errors.push("Wave 7 tool ledger requires exactly six tool groups".to_string());
        return;
    }

    let tool_items = manifest
        .items
        .iter()
        .filter(|item| matches!(item.kind.as_str(), "tool-operation" | "tool-argument"))
        .collect::<Vec<_>>();
    if tool_items.len() != 55 {
        errors.push(format!(
            "Wave 7 tool manifest population must be 55, found {}",
            tool_items.len()
        ));
    }
    let phase = classify_inventory_phase(inventory).ok();
    let capabilities = capability_map(inventory);
    let mut seen = BTreeSet::new();
    for (index, (record, spec)) in audit.tools.iter().zip(expected).enumerate() {
        let (tool, prefix, package, binaries, execution, counts, scenarios, evidence) = spec;
        if record.tool != tool
            || record.source_prefix != prefix
            || record.package != package
            || record
                .binaries
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                != binaries
            || record.execution != execution
            || (
                record.counts.total,
                record.counts.implemented,
                record.counts.proven_platform_impossible,
                record.counts.audit_handoff_1373,
            ) != counts
            || record
                .scenarios
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                != scenarios
        {
            errors.push(format!("Wave 7 tool metadata drifted at index {index}"));
        }

        let leaves = tool_items
            .iter()
            .filter(|item| {
                item.id
                    .split_once(':')
                    .is_some_and(|(_, name)| name.starts_with(prefix))
            })
            .collect::<Vec<_>>();
        let mut derived = (0, 0, 0);
        for item in leaves {
            if !seen.insert(item.id.as_str()) {
                errors.push(format!("Wave 7 tool leaf is assigned twice: {}", item.id));
            }
            match capabilities.get(item.id.as_str()) {
                Some(capability)
                    if capability.disposition == Disposition::ImplementedAndVerified =>
                {
                    derived.0 += 1;
                }
                Some(capability)
                    if capability.disposition == Disposition::ProvenPlatformImpossible =>
                {
                    derived.1 += 1;
                }
                Some(capability) if capability.disposition == Disposition::AuditRequired => {
                    derived.2 += 1;
                }
                Some(_) => errors.push(format!(
                    "Wave 7 tool leaf has an invalid state: {}",
                    item.id
                )),
                None => errors.push(format!("Wave 7 tool capability is missing: {}", item.id)),
            }
        }
        let expected_live = match (prefix, phase) {
            ("jailer/", Some(InventoryPhase::JailerUidGidPlatformLimit)) => (5, 7, 2),
            ("jailer/", Some(InventoryPhase::JailerChrootPlatformLimit)) => (5, 8, 1),
            (
                "jailer/",
                Some(
                    InventoryPhase::JailerAggregate
                    | InventoryPhase::MultiprocessIsolation
                    | InventoryPhase::HostResourceAuthority
                    | InventoryPhase::JailerSeccompContainment,
                ),
            ) => (6, 8, 0),
            _ => (
                record.counts.implemented,
                record.counts.proven_platform_impossible,
                record.counts.audit_handoff_1373,
            ),
        };
        if (derived.0, derived.1, derived.2) != expected_live
            || derived.0 + derived.1 + derived.2 != record.counts.total
        {
            errors.push(format!(
                "Wave 7 tool disposition partition drifted: {prefix}"
            ));
        }
        validate_reference_set(
            &record.evidence,
            repository_root,
            tracked,
            &format!("Wave 7 tool[{index}] evidence"),
            errors,
        );
        validate_exact_local_references(
            &record.evidence,
            evidence,
            &format!("Wave 7 tool[{index}] evidence"),
            errors,
        );
    }
    if seen.len() != tool_items.len() {
        errors.push("Wave 7 tool groups do not partition all 55 manifest leaves".to_string());
    }
    let totals = audit.tools.iter().fold((0, 0, 0, 0), |mut total, tool| {
        total.0 += tool.counts.total;
        total.1 += tool.counts.implemented;
        total.2 += tool.counts.proven_platform_impossible;
        total.3 += tool.counts.audit_handoff_1373;
        total
    });
    if totals != (55, 46, 5, 4) {
        errors.push(format!(
            "Wave 7 tool aggregate must be 55/46/5/4, found {}/{}/{}/{}",
            totals.0, totals.1, totals.2, totals.3
        ));
    }
}

fn validate_mmio(
    audit: &Wave7AggregateAudit,
    inventory: &CapabilityInventory,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    if audit.virtio_mmio.claims != MMIO_CLAIMS {
        errors
            .push("Wave 7 virtio-MMIO ledger requires the exact twelve common claims".to_string());
    }
    let expected_devices = [
        (
            "block-file",
            "semantic.storage:block-sync-async-vhost-and-limits",
            "crates/runtime/src/block.rs",
        ),
        (
            "block-vhost-user",
            "semantic.storage:block-sync-async-vhost-and-limits",
            "crates/bangbang/src/direct_vhost_user.rs",
        ),
        (
            "pmem",
            "semantic.storage:pmem-root-mapping-flush-and-state",
            "crates/runtime/src/pmem.rs",
        ),
        (
            "network",
            "api-schema:NetworkInterface",
            "crates/runtime/src/network.rs",
        ),
        (
            "vsock",
            "semantic.vsock:live-routing-credit-events-and-cleanup",
            "crates/runtime/src/vsock.rs",
        ),
        (
            "entropy",
            "semantic.device:entropy-queues-limits-metrics-and-state",
            "crates/runtime/src/entropy.rs",
        ),
        (
            "balloon",
            "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
            "crates/runtime/src/balloon.rs",
        ),
        (
            "virtio-mem",
            "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
            "crates/runtime/src/memory_hotplug.rs",
        ),
    ];
    let actual_devices = audit
        .virtio_mmio
        .devices
        .iter()
        .map(|device| {
            (
                device.id.as_str(),
                device.producer_capability_id.as_str(),
                device.implementation_path.as_str(),
            )
        })
        .collect::<Vec<_>>();
    if actual_devices != expected_devices {
        errors.push("Wave 7 virtio-MMIO device profiles drifted".to_string());
    }
    if audit.virtio_mmio.pci_evidence_may_substitute {
        errors.push(
            "Wave 7 virtio-MMIO evidence cannot be satisfied by PCI-only evidence".to_string(),
        );
    }
    let capabilities = capability_map(inventory);
    for device in &audit.virtio_mmio.devices {
        if capabilities
            .get(device.producer_capability_id.as_str())
            .is_none_or(|capability| capability.disposition != Disposition::ImplementedAndVerified)
        {
            errors.push(format!(
                "Wave 7 virtio-MMIO device producer is not terminal: {}",
                device.id
            ));
        }
        if !tracked.contains(Path::new(&device.implementation_path)) {
            errors.push(format!(
                "Wave 7 virtio-MMIO device implementation is not tracked: {}",
                device.implementation_path
            ));
        }
    }
    for (kind, references, expected) in [
        (
            "production",
            audit.virtio_mmio.evidence.production.as_slice(),
            &[(
                "crates/runtime/src/virtio_mmio.rs",
                "pub struct VirtioMmioRegisterHandler",
            )][..],
        ),
        (
            "focused",
            audit.virtio_mmio.evidence.focused.as_slice(),
            &[(
                "crates/runtime/src/virtio_mmio.rs",
                "fn register_handler_implements_mmio_handler_for_dispatcher()",
            )][..],
        ),
        (
            "formal",
            audit.virtio_mmio.evidence.formal.as_slice(),
            &[
                (
                    "compat/firecracker/v1.16.0/formal-verification-audit.json",
                    "\"id\": \"virtio-mmio-status-transitions\"",
                ),
                (
                    "crates/runtime/src/virtio_mmio.rs",
                    "fn verify_virtio_mmio_status_transitions()",
                ),
            ][..],
        ),
        (
            "signed",
            audit.virtio_mmio.evidence.signed.as_slice(),
            &[
                (
                    "crates/bangbang/tests/executable_hvf_e2e.rs",
                    "fn signed_executable_runs_async_block_over_mmio_with_live_patch()",
                ),
                (
                    "crates/hvf/tests/guest_boot.rs",
                    "fn boots_signed_mmio_guest_with_complete_virtio_network_semantics()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci()",
                ),
            ][..],
        ),
    ] {
        validate_reference_set(
            references,
            repository_root,
            tracked,
            &format!("Wave 7 virtio-MMIO {kind}"),
            errors,
        );
        validate_exact_local_references(
            references,
            expected,
            &format!("Wave 7 virtio-MMIO {kind}"),
            errors,
        );
    }
    let runtime_path = repository_root.join("crates/runtime/src/virtio_mmio.rs");
    match std::fs::read_to_string(runtime_path) {
        Ok(contents) => {
            for token in [
                "VIRTIO_MMIO_MAGIC_VALUE",
                "VIRTIO_MMIO_VERSION_1_FEATURE",
                "VirtioMmioQueueRegisters",
                "VirtioMmioQueueNotificationRegisters",
                "VirtioMmioInterruptRegisters",
                "is_valid_status_transition",
                "VirtioMmioDeviceActivation",
                "restore_transport_state",
                "bangbang_runtime::device::virtio_mmio",
            ] {
                if !contents.contains(token) {
                    errors.push(format!(
                        "Wave 7 virtio-MMIO source token is missing: {token}"
                    ));
                }
            }
        }
        Err(_) => errors.push("Wave 7 virtio-MMIO production source is unreadable".to_string()),
    }
}

fn validate_handoffs(
    audit: &Wave7AggregateAudit,
    inventory: &CapabilityInventory,
    errors: &mut Vec<String>,
) {
    let phase = classify_inventory_phase(inventory).ok();
    let expected = [
        (
            "semantic.isolation:host-resource-authority-and-brokerage",
            Wave7HandoffOwner::Issue1351,
            Disposition::MissingPlatformFeasible,
        ),
        (
            "semantic.isolation:jailer-seccomp-and-macos-containment-outcomes",
            Wave7HandoffOwner::Issue1351,
            Disposition::MissingPlatformFeasible,
        ),
        (
            "semantic.isolation:multiprocess-concurrency-redaction-and-failure-atomicity",
            Wave7HandoffOwner::Issue1351,
            Disposition::MissingPlatformFeasible,
        ),
        (
            "corpus:jailer",
            Wave7HandoffOwner::Issue1373,
            Disposition::AuditRequired,
        ),
        (
            "corpus:production-host",
            Wave7HandoffOwner::Issue1373,
            Disposition::AuditRequired,
        ),
        (
            "tool-argument:jailer/chroot-base-dir",
            Wave7HandoffOwner::Issue1373,
            Disposition::AuditRequired,
        ),
        (
            "tool-argument:jailer/gid",
            Wave7HandoffOwner::Issue1373,
            Disposition::AuditRequired,
        ),
        (
            "tool-argument:jailer/uid",
            Wave7HandoffOwner::Issue1373,
            Disposition::AuditRequired,
        ),
        (
            "tool-operation:jailer/run",
            Wave7HandoffOwner::Issue1373,
            Disposition::AuditRequired,
        ),
        (
            "corpus:network-setup",
            Wave7HandoffOwner::Issue1378,
            Disposition::AuditRequired,
        ),
        (
            "semantic.network:virtio-net-vmnet-policy-and-connectivity",
            Wave7HandoffOwner::Issue1378,
            Disposition::AuditRequired,
        ),
        (
            "semantic.cross-capability:state-errors-metrics-security-and-snapshots",
            Wave7HandoffOwner::Wave8,
            Disposition::AuditRequired,
        ),
    ];
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
    if actual != expected {
        errors.push(
            "Wave 7 aggregate requires the exact ordered nine audit and three feasible handoffs"
                .to_string(),
        );
    }
    let capabilities = capability_map(inventory);
    for handoff in &audit.handoffs {
        let Some(capability) = capabilities.get(handoff.capability_id.as_str()) else {
            errors.push(format!(
                "Wave 7 handoff capability is missing: {}",
                handoff.capability_id
            ));
            continue;
        };
        let expected_disposition = phase.map_or(handoff.disposition, |phase| {
            expected_disposition(phase, &handoff.capability_id)
        });
        if capability.disposition != expected_disposition {
            errors.push(format!(
                "Wave 7 handoff disposition drifted: {}",
                handoff.capability_id
            ));
        }
        let expected_issue = match handoff.owner {
            Wave7HandoffOwner::Issue1351
                if expected_disposition == Disposition::MissingPlatformFeasible =>
            {
                Some("https://github.com/seven332/bangbang/issues/1351")
            }
            Wave7HandoffOwner::Issue1351 => None,
            Wave7HandoffOwner::Issue1373
            | Wave7HandoffOwner::Issue1378
            | Wave7HandoffOwner::Wave8 => None,
        };
        if capability.delivery_issue.as_deref() != expected_issue {
            errors.push(format!(
                "Wave 7 handoff issue marker drifted: {}",
                handoff.capability_id
            ));
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
        phase @ (InventoryPhase::Wave7
        | InventoryPhase::Wave8
        | InventoryPhase::JailerUidGidPlatformLimit
        | InventoryPhase::JailerChrootPlatformLimit
        | InventoryPhase::JailerAggregate
        | InventoryPhase::MultiprocessIsolation
        | InventoryPhase::HostResourceAuthority
        | InventoryPhase::JailerSeccompContainment),
    ) = phase
    {
        let expected = expected_nonterminal_ids(phase);
        if nonterminal != expected {
            errors.push(format!(
                "Wave 7 aggregate nonterminal inventory differs from the exact {} phase: expected {expected:?}, found {nonterminal:?}",
                phase.name()
            ));
        }
    }
}

fn validate_evidence_and_docs(
    audit: &Wave7AggregateAudit,
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
) {
    for (kind, references, expected) in [
        (
            "implementation",
            audit.evidence.implementation.as_slice(),
            &[
                (
                    "compat/firecracker/v1.16.0/wave7-aggregate-audit.json",
                    "\"schema_version\": 1",
                ),
                (
                    "tools/firecracker-capability-audit/src/wave7_aggregate_audit_validate.rs",
                    "pub fn validate_wave7_aggregate_audit(",
                ),
                (
                    "tools/firecracker-capability-audit/src/wave7_aggregate_certify.rs",
                    "pub fn validate_wave7_aggregate_compatibility(",
                ),
            ][..],
        ),
        (
            "validation",
            audit.evidence.validation.as_slice(),
            &[
                (
                    "tools/firecracker-capability-audit/tests/checked_inventory.rs",
                    "fn wave_7_ownership_and_core_api_policy_is_stable()",
                ),
                (
                    "tools/firecracker-capability-audit/tests/wave7_aggregate_audit.rs",
                    "fn checked_wave7_aggregate_audit_is_canonical_and_fail_closed()",
                ),
            ][..],
        ),
        (
            "signed",
            audit.evidence.signed.as_slice(),
            &[
                (
                    "crates/bangbang/tests/executable_hvf_e2e.rs",
                    "fn signed_executable_runs_async_block_over_mmio_with_live_patch()",
                ),
                (
                    "crates/launcher/tests/production_bundle_e2e.rs",
                    "fn production_bundle_has_exact_nested_signing_contract()",
                ),
            ][..],
        ),
        (
            "documentation",
            audit.evidence.documentation.as_slice(),
            &[
                (
                    "compat/firecracker/v1.16.0/observability-tools-specification-contract.md",
                    "## Wave 7 aggregate certification",
                ),
                ("docs/testing.md", "## Wave 7 aggregate certification"),
            ][..],
        ),
    ] {
        validate_reference_set(
            references,
            repository_root,
            tracked,
            &format!("Wave 7 aggregate {kind}"),
            errors,
        );
        validate_exact_local_references(
            references,
            expected,
            &format!("Wave 7 aggregate {kind}"),
            errors,
        );
    }

    let expected_docs = [
        ("landing-page", "README.md"),
        ("human-compatibility", "docs/firecracker-compatibility.md"),
        ("validation-index", "docs/firecracker-validation-matrix.md"),
        ("commands", "docs/testing.md"),
        ("trust-boundaries", "docs/security.md"),
        (
            "checked-wave7-contract",
            "compat/firecracker/v1.16.0/observability-tools-specification-contract.md",
        ),
    ];
    let actual_docs = audit
        .document_owners
        .iter()
        .map(|owner| (owner.subject.as_str(), owner.path.as_str()))
        .collect::<Vec<_>>();
    if actual_docs != expected_docs {
        errors.push("Wave 7 aggregate documentation ownership drifted".to_string());
    }
    for owner in &audit.document_owners {
        if !tracked.contains(Path::new(&owner.path)) {
            errors.push(format!(
                "Wave 7 documentation owner is not tracked: {}",
                owner.path
            ));
        }
    }
    if audit.nonclaims != NONCLAIMS {
        errors.push("Wave 7 aggregate requires the exact ordered nonclaims".to_string());
    }
}

fn validate_reference_set(
    references: &[Reference],
    repository_root: &Path,
    tracked: &BTreeSet<PathBuf>,
    label: &str,
    errors: &mut Vec<String>,
) {
    if references.is_empty() {
        errors.push(format!("{label} requires at least one reference"));
    }
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
            | Reference::Authoritative { .. } => errors.push(format!(
                "{label}[{index}] must be an anchored local reference"
            )),
        }
    }
}

fn validate_exact_local_references(
    references: &[Reference],
    expected: &[(&str, &str)],
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
}

fn capability_map(inventory: &CapabilityInventory) -> BTreeMap<&str, &crate::Capability> {
    inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_source_populations_are_internally_consistent() {
        assert_eq!(DESIGN_RECORDS.len(), 37);
        assert_eq!(REQUIRED_RELATIONS.len(), 62);
        assert_eq!(RELEASE_ENTRIES.len(), 21);
        assert_eq!(
            RELEASE_ENTRIES
                .iter()
                .filter(|entry| entry.1 == Wave7ReleaseSection::Added)
                .count(),
            9
        );
        assert_eq!(
            RELEASE_ENTRIES
                .iter()
                .filter(|entry| entry.1 == Wave7ReleaseSection::Fixed)
                .count(),
            12
        );
    }
}
