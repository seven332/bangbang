use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    AuditMode, CAPABILITY_INVENTORY_PATH, Disposition, SOURCE_MANIFEST_PATH,
    read_capability_inventory, read_source_manifest, source_manifest_json, validate,
};

#[test]
fn checked_inventory_is_valid_for_delivery() {
    let tool_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = tool_root
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory");
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");

    validate(&manifest, &inventory, repository_root, AuditMode::Delivery)
        .expect("checked inventory must satisfy delivery-time invariants");
}

#[test]
fn checked_source_manifest_is_canonical_and_deterministic() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let path = repository_root.join(SOURCE_MANIFEST_PATH);
    let checked_bytes = std::fs::read(&path).expect("checked source manifest must be readable");
    let manifest = read_source_manifest(&path).expect("checked source manifest must parse");

    let first = source_manifest_json(&manifest).expect("source manifest must serialize");
    let second = source_manifest_json(&manifest).expect("source manifest must serialize again");
    assert_eq!(
        first, second,
        "canonical serialization must be deterministic"
    );
    assert_eq!(
        first, checked_bytes,
        "checked source manifest must use canonical serialization"
    );
}

#[test]
fn delivery_closure_policy_is_stable() {
    const IMPLEMENTED_ORIGINAL: [&str; 5] = [
        "corpus:cpu-boot-protocol",
        "semantic.boot:kernel-rootfs-fdt-and-cache",
        "semantic.lifecycle:pause-resume-quiescence-and-failure",
        "semantic.lifecycle:smp-psci-and-vcpu-ownership",
        "semantic.memory:machine-sizing-hugepages-and-dirty-tracking",
    ];
    const WAVE_7_ORIGINAL: [&str; 22] = [
        "corpus:cpu-template-helper",
        "corpus:cpu-templates",
        "corpus:rootfs-and-kernel",
        "semantic.cpu:configuration-templates-and-feature-state",
        "tool-argument:cpu-template-helper/fingerprint/compare/curr",
        "tool-argument:cpu-template-helper/fingerprint/compare/filters",
        "tool-argument:cpu-template-helper/fingerprint/compare/prev",
        "tool-argument:cpu-template-helper/fingerprint/dump/config",
        "tool-argument:cpu-template-helper/fingerprint/dump/output",
        "tool-argument:cpu-template-helper/fingerprint/dump/template",
        "tool-argument:cpu-template-helper/template/dump/config",
        "tool-argument:cpu-template-helper/template/dump/output",
        "tool-argument:cpu-template-helper/template/dump/template",
        "tool-argument:cpu-template-helper/template/strip/paths",
        "tool-argument:cpu-template-helper/template/strip/suffix",
        "tool-argument:cpu-template-helper/template/verify/config",
        "tool-argument:cpu-template-helper/template/verify/template",
        "tool-operation:cpu-template-helper/fingerprint/compare",
        "tool-operation:cpu-template-helper/fingerprint/dump",
        "tool-operation:cpu-template-helper/template/dump",
        "tool-operation:cpu-template-helper/template/strip",
        "tool-operation:cpu-template-helper/template/verify",
    ];
    const PROMOTED_API: [&str; 18] = [
        "api-operation:GET /machine-config",
        "api-operation:PATCH /machine-config",
        "api-operation:PUT /boot-source",
        "api-operation:PUT /cpu-config",
        "api-operation:PUT /machine-config",
        "api-path:/boot-source",
        "api-path:/cpu-config",
        "api-path:/machine-config",
        "api-path:/vm",
        "api-property:BootSource.boot_args",
        "api-property:BootSource.initrd_path",
        "api-property:BootSource.kernel_image_path",
        "api-property:FullVmConfiguration.boot-source",
        "api-property:FullVmConfiguration.machine-config",
        "api-property:Vm.state",
        "api-schema:BootSource",
        "api-schema:MachineConfiguration",
        "api-schema:Vm",
    ];
    const RUNTIME_BLOCK_HOTPLUG: [&str; 2] = [
        "api-operation:PUT /drives/{drive_id}",
        "non-swagger-route:DELETE /drives/{drive_id}",
    ];
    const RUNTIME_PMEM_HOTPLUG: [&str; 3] = [
        "api-operation:PUT /pmem/{id}",
        "api-path:/pmem/{id}",
        "non-swagger-route:DELETE /pmem/{id}",
    ];
    const RUNTIME_NETWORK_HOTPLUG: [&str; 2] = [
        "api-operation:PUT /network-interfaces/{iface_id}",
        "non-swagger-route:DELETE /network-interfaces/{iface_id}",
    ];
    const PCI_RUNTIME_HOTPLUG_AGGREGATES: [&str; 3] = [
        "corpus:device-hotplug",
        "semantic.hotplug:runtime-device-manager",
        "semantic.transport:pci-msi-and-coexistence",
    ];
    const STORAGE_TERMINAL: [&str; 38] = [
        "api-operation:PATCH /drives/{drive_id}",
        "api-operation:PATCH /pmem/{id}",
        "api-operation:PUT /drives/{drive_id}",
        "api-operation:PUT /pmem/{id}",
        "api-path:/drives/{drive_id}",
        "api-path:/pmem/{id}",
        "api-property:Drive.cache_type",
        "api-property:Drive.drive_id",
        "api-property:Drive.io_engine",
        "api-property:Drive.is_read_only",
        "api-property:Drive.is_root_device",
        "api-property:Drive.partuuid",
        "api-property:Drive.path_on_host",
        "api-property:Drive.rate_limiter",
        "api-property:Drive.socket",
        "api-property:FullVmConfiguration.drives",
        "api-property:FullVmConfiguration.pmem",
        "api-property:PartialDrive.drive_id",
        "api-property:PartialDrive.path_on_host",
        "api-property:PartialDrive.rate_limiter",
        "api-property:PartialPmem.id",
        "api-property:PartialPmem.rate_limiter",
        "api-property:Pmem.id",
        "api-property:Pmem.path_on_host",
        "api-property:Pmem.rate_limiter",
        "api-property:Pmem.read_only",
        "api-property:Pmem.root_device",
        "api-schema:Drive",
        "api-schema:PartialDrive",
        "api-schema:PartialPmem",
        "api-schema:Pmem",
        "corpus:block-caching",
        "corpus:block-io-engine",
        "corpus:block-vhost-user",
        "corpus:patch-block",
        "non-swagger-route:DELETE /drives/{drive_id}",
        "non-swagger-route:DELETE /pmem/{id}",
        "semantic.storage:block-sync-async-vhost-and-limits",
    ];
    const STORAGE_WAVE_6: [&str; 2] = [
        "corpus:pmem",
        "semantic.storage:pmem-root-mapping-flush-and-state",
    ];
    const MEMORY_HOTPLUG_TERMINAL: [&str; 17] = [
        "api-operation:GET /hotplug/memory",
        "api-operation:PATCH /hotplug/memory",
        "api-operation:PUT /hotplug/memory",
        "api-path:/hotplug/memory",
        "api-property:FullVmConfiguration.memory-hotplug",
        "api-property:MemoryHotplugConfig.block_size_mib",
        "api-property:MemoryHotplugConfig.slot_size_mib",
        "api-property:MemoryHotplugConfig.total_size_mib",
        "api-property:MemoryHotplugSizeUpdate.requested_size_mib",
        "api-property:MemoryHotplugStatus.block_size_mib",
        "api-property:MemoryHotplugStatus.plugged_size_mib",
        "api-property:MemoryHotplugStatus.requested_size_mib",
        "api-property:MemoryHotplugStatus.slot_size_mib",
        "api-property:MemoryHotplugStatus.total_size_mib",
        "api-schema:MemoryHotplugConfig",
        "api-schema:MemoryHotplugSizeUpdate",
        "api-schema:MemoryHotplugStatus",
    ];
    const MEMORY_HOTPLUG_WAVE_6: [&str; 2] = [
        "corpus:memory-hotplug",
        "semantic.memory-device:virtio-mem-lifecycle-accounting-and-state",
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        manifest.items.len(),
        381,
        "generated identity count drifted"
    );
    assert_eq!(
        inventory.capabilities.len(),
        418,
        "delivery overlay count drifted"
    );
    assert_eq!(
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.id.starts_with("semantic."))
            .count(),
        37,
        "local semantic identity count drifted"
    );
    assert_eq!(
        by_id.len(),
        inventory.capabilities.len(),
        "capability identities must remain unique"
    );

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 181);
    assert_eq!(count(Disposition::AuditRequired), 217);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 3);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 17);

    for id in IMPLEMENTED_ORIGINAL {
        assert_eq!(
            by_id
                .get(id)
                .expect("implemented original record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "original record must remain implemented: {id}"
        );
    }
    for id in WAVE_7_ORIGINAL {
        let capability = by_id.get(id).expect("Wave 7 original record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 7 handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 7"),
            "Wave 7 handoff must name its owner: {id}"
        );
    }
    assert_eq!(
        by_id
            .get("corpus:hugepages")
            .expect("hugepages corpus must exist")
            .disposition,
        Disposition::ProvenPlatformImpossible
    );

    let original = IMPLEMENTED_ORIGINAL
        .into_iter()
        .chain(WAVE_7_ORIGINAL)
        .chain(["corpus:hugepages"])
        .collect::<BTreeSet<_>>();
    assert_eq!(
        original.len(),
        28,
        "original closure ledger must stay exact"
    );

    for id in PROMOTED_API {
        assert_eq!(
            by_id
                .get(id)
                .expect("promoted API record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "bounded API record must remain terminal: {id}"
        );
    }

    for id in RUNTIME_BLOCK_HOTPLUG {
        assert_eq!(
            by_id
                .get(id)
                .expect("runtime block hotplug record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "runtime block hotplug record must remain implemented: {id}"
        );
    }

    for id in RUNTIME_PMEM_HOTPLUG {
        assert_eq!(
            by_id
                .get(id)
                .expect("runtime pmem hotplug record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "runtime pmem hotplug record must remain implemented: {id}"
        );
    }

    for id in RUNTIME_NETWORK_HOTPLUG {
        assert_eq!(
            by_id
                .get(id)
                .expect("runtime network hotplug record must exist")
                .disposition,
            Disposition::ImplementedAndVerified,
            "runtime network hotplug record must remain implemented: {id}"
        );
    }

    for id in PCI_RUNTIME_HOTPLUG_AGGREGATES {
        let capability = by_id
            .get(id)
            .expect("PCI/runtime-hotplug aggregate record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "PCI/runtime-hotplug aggregate must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "PCI/runtime-hotplug aggregate must retain concrete evidence: {id}"
        );
    }

    let storage_ids = STORAGE_TERMINAL
        .into_iter()
        .chain(STORAGE_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        storage_ids.len(),
        40,
        "storage closure ledger must stay exact"
    );

    for id in STORAGE_TERMINAL {
        let capability = by_id.get(id).expect("terminal storage record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "storage record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal storage record must retain concrete evidence: {id}"
        );
        assert!(
            !capability.summary.contains("#1450")
                && !capability.summary.contains("before promotion")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("broad storage audit"),
            "terminal storage summary still names future storage work: {id}"
        );
    }
    for id in STORAGE_WAVE_6 {
        let capability = by_id.get(id).expect("Wave 6 storage record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 6 storage handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 6"),
            "Wave 6 storage handoff must name its owner: {id}"
        );
    }

    let storage_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/storage-contract.md"),
    )
    .expect("checked storage contract must be readable");
    assert_eq!(
        storage_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        40,
        "checked storage contract must contain each exact ledger row once"
    );
    for id in storage_ids {
        assert_eq!(
            storage_contract.matches(&format!("| `{id}` |")).count(),
            1,
            "checked storage contract row must be unique: {id}"
        );
    }

    let memory_hotplug_ids = MEMORY_HOTPLUG_TERMINAL
        .into_iter()
        .chain(MEMORY_HOTPLUG_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        memory_hotplug_ids.len(),
        19,
        "memory-hotplug closure ledger must stay exact"
    );
    for id in MEMORY_HOTPLUG_TERMINAL {
        let capability = by_id
            .get(id)
            .expect("terminal memory-hotplug record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "memory-hotplug record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal memory-hotplug record must retain concrete evidence: {id}"
        );
    }
    for id in MEMORY_HOTPLUG_WAVE_6 {
        let capability = by_id
            .get(id)
            .expect("Wave 6 memory-hotplug record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 6 memory-hotplug handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 6"),
            "Wave 6 memory-hotplug handoff must name its owner: {id}"
        );
    }

    let memory_hotplug_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/memory-hotplug-contract.md"),
    )
    .expect("checked memory-hotplug contract must be readable");
    assert_eq!(
        memory_hotplug_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        19,
        "checked memory-hotplug contract must contain each exact ledger row once"
    );
    for id in memory_hotplug_ids {
        assert_eq!(
            memory_hotplug_contract
                .matches(&format!("| `{id}` |"))
                .count(),
            1,
            "checked memory-hotplug contract row must be unique: {id}"
        );
    }

    for capability in &inventory.capabilities {
        assert!(
            !capability.summary.contains("awaits #1388")
                && !capability.summary.contains("awaits the #1388")
                && !capability.summary.contains("#1388/Wave"),
            "summary still names #1388 as a future owner: {}",
            capability.id
        );
    }
}
