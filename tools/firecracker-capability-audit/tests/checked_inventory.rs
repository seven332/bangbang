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
fn machine_lifecycle_closure_policy_is_stable() {
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
    assert_eq!(count(Disposition::ImplementedAndVerified), 73);
    assert_eq!(count(Disposition::AuditRequired), 325);
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
