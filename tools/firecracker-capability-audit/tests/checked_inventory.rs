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
fn snapshot_paging_feasibility_policy_is_stable() {
    const CAPABILITY_ID: &str = "corpus:snapshot-page-faults";
    const DELIVERY_ISSUE: &str = "https://github.com/seven332/bangbang/issues/1527";

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let capability = inventory
        .capabilities
        .iter()
        .find(|capability| capability.id == CAPABILITY_ID)
        .expect("snapshot page-fault corpus record must exist");

    assert_eq!(
        capability.source_refs,
        [CAPABILITY_ID],
        "snapshot paging must retain its exact pinned source identity"
    );
    assert_eq!(
        capability.disposition,
        Disposition::MissingPlatformFeasible,
        "snapshot paging must remain feasible but nonterminal until certification"
    );
    assert_eq!(
        capability.delivery_issue.as_deref(),
        Some(DELIVERY_ISSUE),
        "snapshot paging must retain its challenged delivery owner"
    );
    assert!(
        capability.implementation.is_empty()
            && capability.validation.is_empty()
            && capability.exclusion.is_none(),
        "feasibility evidence must not masquerade as terminal capability evidence"
    );
    assert!(
        capability.summary.contains(DELIVERY_ISSUE)
            && capability
                .summary
                .contains("Native-v1 Uffd remains rejected")
            && capability.summary.contains("not Linux UFFD"),
        "snapshot paging summary must retain owner, runtime, and compatibility limits"
    );

    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/snapshot-paging-contract.md"),
    )
    .expect("checked snapshot paging contract must be readable");
    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "snapshot paging ledger must have one row");
    assert!(
        rows[0].starts_with(&format!("| `{CAPABILITY_ID}` |"))
            && rows[0].contains("`missing-platform-feasible`")
            && rows[0].contains(DELIVERY_ISSUE)
            && rows[0].ends_with("| `nonterminal` |"),
        "snapshot paging ledger row must pin identity, status, owner, and result"
    );

    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "handling-page-faults-on-snapshot-resume.md",
        "mach_memory_object_memory_entry_64",
        "hv_vm_protect",
        "guest_bypassed_host_protection=true",
        "guest_population value=0x31415926",
        "host_population value=0x00000000 faults=1",
        "removed_guest_population value=0x00000000",
        "handler_death_detected=true",
        "cleanup=complete",
        "com.apple.security.app-sandbox",
        "com.apple.security.hypervisor",
        "bangbang-pager-v1",
        "crates/pager",
        "docs/snapshot-pager-protocol.md",
        "Implemented coordinated lazy anonymous memory",
        "crates/runtime/src/lazy_memory.rs",
        "LazyGuestMemory",
        "cargo test -p bangbang-runtime lazy_memory",
        "BBPAGER\\0",
        "cargo test -p bangbang-pager",
        "classify_v1_load_request",
        "native_v1_load_policy_rejects_each_unsupported_dimension",
        "returns_fault_for_snapshot_endpoint",
        "signed_executable_creates_and_restores_native_v1_snapshot_across_processes",
        "https://github.com/seven332/bangbang/issues/1555",
    ] {
        assert!(
            contract.contains(required),
            "snapshot paging contract must pin {required}"
        );
    }

    let pager_manifest = std::fs::read_to_string(repository_root.join("crates/pager/Cargo.toml"))
        .expect("checked pager manifest must be readable");
    assert!(
        pager_manifest.contains("name = \"bangbang-pager\"")
            && pager_manifest.contains("getrandom = \"0.3\"")
            && pager_manifest.contains("libc = \"0.2\""),
        "pager package identity and narrow dependencies must remain pinned"
    );

    let runtime_manifest =
        std::fs::read_to_string(repository_root.join("crates/runtime/Cargo.toml"))
            .expect("checked runtime manifest must be readable");
    assert!(
        runtime_manifest.contains("bangbang-pager = { path = \"../pager\" }"),
        "runtime must retain its narrow pager type dependency"
    );

    let pager_source = std::fs::read_to_string(repository_root.join("crates/pager/src/frame.rs"))
        .expect("checked pager framing source must be readable");
    for required in [
        "*b\"BBPAGER\\0\"",
        "pub const HEADER_BYTES: usize = 24",
        "pub const MIN_PAGE_SIZE: u32 = 4 * 1024",
        "pub const MAX_PAGE_SIZE: u32 = 2 * 1024 * 1024",
        "pub struct PagerFrameDecoder",
    ] {
        assert!(
            pager_source.contains(required),
            "pager source must retain {required}"
        );
    }

    let lazy_source =
        std::fs::read_to_string(repository_root.join("crates/runtime/src/lazy_memory.rs"))
            .expect("checked lazy-memory coordinator source must be readable");
    for required in [
        "pub struct LazyGuestMemory",
        "pub struct LazyPagePopulation",
        "pub struct LazyPagePublication",
        "pub struct LazyPageRemoval",
        "enum PageTag",
        "PopulationStage::Retired",
        "duplicate_faults_coalesce_to_one_generation_and_result",
        "removal_reserves_a_distinct_slot_before_superseding_loading",
        "removal_stays_counted_and_removing_until_acknowledged",
        "requested_peer_and_teardown_outcomes_wake_waiters",
        "generation_exhaustion_is_owner_terminal",
        "repeated_construction_and_destruction_leaves_no_retained_work",
    ] {
        assert!(
            lazy_source.contains(required),
            "lazy-memory coordinator must retain {required}"
        );
    }

    let protocol = std::fs::read_to_string(repository_root.join("docs/snapshot-pager-protocol.md"))
        .expect("checked pager protocol document must be readable");
    for required in [
        "`BBPAGER\\0`",
        "2,097,248",
        "strictly increasing request IDs",
        "Cancellation is session-wide and terminal",
        "Orderly shutdown is drain-only",
        "Runtime anonymous-memory coordinator",
        "retired-operation accounting",
        "only explicit validated `Removed`",
        "not Linux UFFD descriptor or wire compatibility",
    ] {
        assert!(
            protocol.contains(required),
            "pager protocol document must retain {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 228);
    assert_eq!(count(Disposition::AuditRequired), 169);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 4);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 17);
}

#[test]
fn network_mmds_closure_policy_is_stable() {
    const TERMINAL: [&str; 31] = [
        "api-operation:GET /mmds",
        "api-operation:PATCH /mmds",
        "api-operation:PATCH /network-interfaces/{iface_id}",
        "api-operation:PUT /mmds",
        "api-operation:PUT /mmds/config",
        "api-operation:PUT /network-interfaces/{iface_id}",
        "api-path:/mmds",
        "api-path:/mmds/config",
        "api-path:/network-interfaces/{iface_id}",
        "api-property:FullVmConfiguration.mmds-config",
        "api-property:FullVmConfiguration.network-interfaces",
        "api-property:MmdsConfig.imds_compat",
        "api-property:MmdsConfig.ipv4_address",
        "api-property:MmdsConfig.network_interfaces",
        "api-property:MmdsConfig.version",
        "api-property:NetworkInterface.guest_mac",
        "api-property:NetworkInterface.host_dev_name",
        "api-property:NetworkInterface.iface_id",
        "api-property:NetworkInterface.mtu",
        "api-property:NetworkInterface.rx_rate_limiter",
        "api-property:NetworkInterface.tx_rate_limiter",
        "api-property:PartialNetworkInterface.iface_id",
        "api-property:PartialNetworkInterface.rx_rate_limiter",
        "api-property:PartialNetworkInterface.tx_rate_limiter",
        "api-schema:MmdsConfig",
        "api-schema:MmdsContentsObject",
        "api-schema:NetworkInterface",
        "api-schema:PartialNetworkInterface",
        "corpus:mmds-design",
        "corpus:patch-network-interface",
        "non-swagger-route:DELETE /network-interfaces/{iface_id}",
    ];
    const RETAINED: [(&str, &[&str], &str); 4] = [
        (
            "corpus:mmds-user-guide",
            &["https://github.com/seven332/bangbang/issues/1490"],
            "`W6`",
        ),
        (
            "corpus:network-setup",
            &[
                "https://github.com/seven332/bangbang/issues/1378",
                "https://github.com/seven332/bangbang/issues/1490",
            ],
            "`EXTERNAL-GATE + W6`",
        ),
        (
            "semantic.mmds:tcp-token-session-and-isolation",
            &["https://github.com/seven332/bangbang/issues/1490"],
            "`W6`",
        ),
        (
            "semantic.network:virtio-net-vmnet-policy-and-connectivity",
            &[
                "https://github.com/seven332/bangbang/issues/1378",
                "https://github.com/seven332/bangbang/issues/1490",
                "https://github.com/seven332/bangbang/issues/1491",
            ],
            "`EXTERNAL-GATE + W6 + W7`",
        ),
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/network-mmds-contract.md"),
    )
    .expect("checked network/MMDS contract must be readable");

    let expected_ids = TERMINAL
        .into_iter()
        .chain(RETAINED.iter().map(|(id, _, _)| *id))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_ids.len(),
        35,
        "network/MMDS ledger must stay exact"
    );

    for id in TERMINAL {
        let capability = by_id
            .get(id)
            .expect("terminal network/MMDS record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "terminal network/MMDS disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal network/MMDS evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit ")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("current live subset"),
            "terminal network/MMDS summary still names future audit work: {id}"
        );
    }

    for (id, owner_urls, downstream) in RETAINED {
        let capability = by_id
            .get(id)
            .expect("retained network/MMDS record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "retained network/MMDS disposition drifted: {id}"
        );
        for owner_url in owner_urls {
            assert!(
                capability.summary.contains(owner_url),
                "retained network/MMDS summary must name {owner_url}: {id}"
            );
        }
        for outcome in ["restore", "clone"] {
            assert!(
                capability.summary.contains(outcome),
                "retained network/MMDS summary must name missing {outcome}: {id}"
            );
        }
        if id.contains("network") {
            assert!(
                capability.summary.contains("connectivity"),
                "retained network summary must name missing connectivity: {id}"
            );
        }
        if id.starts_with("semantic.network") {
            assert!(
                capability.summary.contains("performance")
                    && capability.summary.contains("observability"),
                "retained network semantic must name Wave 7 outcomes"
            );
        }

        let row_prefix = format!("| `{id}` |");
        let row = contract
            .lines()
            .find(|line| line.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("network/MMDS contract row must exist: {id}"));
        assert!(
            row.contains("`audit-required`") && row.ends_with(&format!("| {downstream} |")),
            "retained network/MMDS ledger row has the wrong handoff: {id}"
        );
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let contract_ids = rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rows.len(), 35, "network/MMDS contract row count drifted");
    assert_eq!(
        contract_ids, expected_ids,
        "network/MMDS identity set drifted"
    );
    for id in TERMINAL {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("terminal network/MMDS row must exist: {id}"));
        assert_eq!(
            contract.matches(&row_prefix).count(),
            1,
            "network/MMDS contract row must be unique: {id}"
        );
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
    }

    for required in [
        "https://github.com/seven332/bangbang/issues/1378",
        "https://github.com/seven332/bangbang/issues/1490",
        "https://github.com/seven332/bangbang/issues/1491",
        "boots_signed_mmio_guest_with_complete_virtio_network_semantics",
        "boots_signed_pci_guest_with_complete_virtio_network_semantics",
        "capture_ready_network_traverses_signed_mmio_and_pci_owners",
        "signed_executable_serves_mmds_on_two_isolated_guest_interfaces",
        "signed_executable_keeps_concurrent_mmds_processes_isolated",
        "signed_executable_hotplugs_mmds_network_and_reuses_product_pci_slot",
        "normal_bundle_hotplugs_mmds_network_without_vmnet_authority",
        "networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation",
        "bangbang vmnet preflight: blocked",
    ] {
        assert!(
            contract.contains(required),
            "network/MMDS contract must pin {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 228);
    assert_eq!(count(Disposition::AuditRequired), 169);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 4);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 17);
}

#[test]
fn vsock_closure_policy_is_stable() {
    const TERMINAL: [&str; 8] = [
        "api-operation:PUT /vsock",
        "api-path:/vsock",
        "api-property:FullVmConfiguration.vsock",
        "api-property:Vsock.guest_cid",
        "api-property:Vsock.uds_path",
        "api-property:Vsock.vsock_id",
        "api-schema:Vsock",
        "semantic.vsock:live-routing-credit-events-and-cleanup",
    ];
    const RETAINED: [&str; 6] = [
        "api-property:SnapshotLoadParams.vsock_override",
        "api-property:VsockOverride.uds_path",
        "api-schema:VsockOverride",
        "corpus:vsock",
        "semantic.snapshot:network-vsock-overrides-portability-and-clones",
        "semantic.vsock:snapshot-override-reset-and-rx-gating",
    ];

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory")
        .to_path_buf();
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");
    let by_id = inventory
        .capabilities
        .iter()
        .map(|capability| (capability.id.as_str(), capability))
        .collect::<BTreeMap<_, _>>();
    let contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/vsock-contract.md"),
    )
    .expect("checked vsock contract must be readable");

    assert_eq!(
        inventory.capabilities.len(),
        418,
        "the checked v1.16.0 overlay identity count drifted"
    );
    let expected_ids = TERMINAL
        .into_iter()
        .chain(RETAINED)
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_ids.len(), 14, "vsock ledger must stay exact");

    for id in TERMINAL {
        let capability = by_id.get(id).expect("terminal vsock record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "terminal vsock disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal vsock evidence is incomplete: {id}"
        );
        assert!(
            !capability.summary.contains("Audit ")
                && !capability.summary.contains("Continue auditing")
                && !capability.summary.contains("current live subset")
                && !capability.summary.contains("#1518"),
            "terminal vsock summary still names future certification: {id}"
        );
    }

    const WAVE_6_URL: &str = "https://github.com/seven332/bangbang/issues/1490";
    for id in RETAINED {
        let capability = by_id.get(id).expect("retained vsock record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "retained vsock disposition drifted: {id}"
        );
        assert!(
            capability.summary.contains(WAVE_6_URL),
            "retained vsock summary must name the full Wave 6 owner URL: {id}"
        );
        for outcome in [
            "encoding",
            "placement",
            "invocation",
            "restored",
            "acknowledgement",
            "override",
            "clone",
            "version",
            "portability",
        ] {
            assert!(
                capability.summary.contains(outcome),
                "retained vsock summary must name missing {outcome} outcome: {id}"
            );
        }
        assert!(
            capability.implementation.is_empty() && capability.validation.is_empty(),
            "retained aggregate rows must not masquerade producer evidence as completion: {id}"
        );
    }

    let rows = contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let contract_ids = rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(rows.len(), 14, "vsock contract row count drifted");
    assert_eq!(
        contract_ids, expected_ids,
        "vsock contract identity set drifted"
    );

    for id in TERMINAL {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("terminal vsock row must exist: {id}"));
        assert_eq!(
            contract.matches(&row_prefix).count(),
            1,
            "terminal vsock contract row must be unique: {id}"
        );
        assert!(row.contains("`implemented-and-verified`"));
        assert!(row.ends_with("| `terminal` |"));
        for evidence in ["FC-", "FOCUSED-", "SIGNED-"] {
            assert!(
                row.contains(evidence),
                "terminal vsock row must contain {evidence} evidence: {id}"
            );
        }
    }

    for id in RETAINED {
        let row_prefix = format!("| `{id}` |");
        let row = rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("retained vsock row must exist: {id}"));
        assert_eq!(
            contract.matches(&row_prefix).count(),
            1,
            "retained vsock contract row must be unique: {id}"
        );
        assert!(row.contains("`audit-required`"));
        assert!(row.contains("`FC-SNAPSHOT`") || id == "corpus:vsock");
        assert!(row.contains("FOCUSED-"));
        assert!(
            row.contains("SIGNED-CAPTURE")
                && (row.contains("source only") || row.contains("source/live subset")),
            "retained vsock signed evidence must stay source-only: {id}"
        );
        assert!(row.contains("`W6`"));
        assert!(
            row.contains(WAVE_6_URL),
            "retained vsock row must name the full Wave 6 owner URL: {id}"
        );
    }

    for required in [
        "d83d72b710361a10294480131377b1b00b163af8",
        "src/firecracker/swagger/firecracker.yaml",
        "src/firecracker/src/api_server/request/vsock.rs",
        "src/vmm/src/devices/virtio/vsock/persist.rs",
        "tests/integration_tests/functional/test_vsock.py",
        WAVE_6_URL,
        "https://github.com/seven332/bangbang/issues/1491",
        "parses_put_vsock_with_deprecated_vsock_id",
        "snapshot_vsock_selectors_resolve_before_resource_access_and_redact_values",
        "virtio_vsock_transport_reset_publishes_event_and_mmio_interrupt",
        "virtio_vsock_restored_gate_keeps_tx_live_and_buffers_generated_rx",
        "signed_executable_runs_async_block_over_mmio_with_live_patch",
        "signed_executable_handles_guest_initiated_vsock_from_direct_rootfs",
        "signed_executable_handles_guest_initiated_vsock_multistream_from_direct_rootfs",
        "signed_executable_handles_host_initiated_vsock_to_direct_rootfs",
        "signed_executable_handles_host_initiated_vsock_multistream_to_direct_rootfs",
        "signed_executable_resets_live_vsock_before_unsupported_snapshot_over_mmio",
        "signed_executable_resets_live_vsock_before_unsupported_snapshot_over_product_pci",
        "capture_ready_vsock_resets_signed_mmio_and_pci_owners",
        "normal_bundle_routes_guest_vsock_through_launcher_broker_without_helpers",
        "normal_bundle_routes_host_vsock_through_supplied_granted_listener",
    ] {
        assert!(
            contract.contains(required),
            "vsock contract must pin {required}"
        );
    }

    let count = |disposition| {
        inventory
            .capabilities
            .iter()
            .filter(|capability| capability.disposition == disposition)
            .count()
    };
    assert_eq!(count(Disposition::ImplementedAndVerified), 228);
    assert_eq!(count(Disposition::AuditRequired), 169);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 4);
    assert_eq!(count(Disposition::ProvenPlatformImpossible), 17);
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
    const WAVE_6_ISSUE_URL: &str = "https://github.com/seven332/bangbang/issues/1490";
    const BALLOON_TERMINAL: [&str; 50] = [
        "api-operation:GET /balloon",
        "api-operation:GET /balloon/hinting/status",
        "api-operation:GET /balloon/statistics",
        "api-operation:PATCH /balloon",
        "api-operation:PATCH /balloon/hinting/start",
        "api-operation:PATCH /balloon/hinting/stop",
        "api-operation:PATCH /balloon/statistics",
        "api-operation:PUT /balloon",
        "api-path:/balloon",
        "api-path:/balloon/hinting/start",
        "api-path:/balloon/hinting/status",
        "api-path:/balloon/hinting/stop",
        "api-path:/balloon/statistics",
        "api-property:Balloon.amount_mib",
        "api-property:Balloon.deflate_on_oom",
        "api-property:Balloon.free_page_hinting",
        "api-property:Balloon.free_page_reporting",
        "api-property:Balloon.stats_polling_interval_s",
        "api-property:BalloonHintingStatus.guest_cmd",
        "api-property:BalloonHintingStatus.host_cmd",
        "api-property:BalloonStartCmd.acknowledge_on_stop",
        "api-property:BalloonStats.actual_mib",
        "api-property:BalloonStats.actual_pages",
        "api-property:BalloonStats.alloc_stall",
        "api-property:BalloonStats.async_reclaim",
        "api-property:BalloonStats.async_scan",
        "api-property:BalloonStats.available_memory",
        "api-property:BalloonStats.direct_reclaim",
        "api-property:BalloonStats.direct_scan",
        "api-property:BalloonStats.disk_caches",
        "api-property:BalloonStats.free_memory",
        "api-property:BalloonStats.hugetlb_allocations",
        "api-property:BalloonStats.hugetlb_failures",
        "api-property:BalloonStats.major_faults",
        "api-property:BalloonStats.minor_faults",
        "api-property:BalloonStats.oom_kill",
        "api-property:BalloonStats.swap_in",
        "api-property:BalloonStats.swap_out",
        "api-property:BalloonStats.target_mib",
        "api-property:BalloonStats.target_pages",
        "api-property:BalloonStats.total_memory",
        "api-property:BalloonStatsUpdate.stats_polling_interval_s",
        "api-property:BalloonUpdate.amount_mib",
        "api-property:FullVmConfiguration.balloon",
        "api-schema:Balloon",
        "api-schema:BalloonHintingStatus",
        "api-schema:BalloonStartCmd",
        "api-schema:BalloonStats",
        "api-schema:BalloonStatsUpdate",
        "api-schema:BalloonUpdate",
    ];
    const BALLOON_WAVE_6: [&str; 2] = [
        "corpus:ballooning",
        "semantic.memory-device:balloon-oom-stats-hinting-and-reporting",
    ];
    const TIME_IDENTITY_WAVE_6: [&str; 1] = ["semantic.device:rtc-vmclock-vmgenid-and-pvtime"];

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
    const ENTROPY_TERMINAL: [&str; 5] = [
        "api-operation:PUT /entropy",
        "api-path:/entropy",
        "api-property:EntropyDevice.rate_limiter",
        "api-property:FullVmConfiguration.entropy",
        "api-schema:EntropyDevice",
    ];
    const ENTROPY_WAVE_6: [&str; 2] = [
        "corpus:entropy",
        "semantic.device:entropy-queues-limits-metrics-and-state",
    ];
    const SERIAL_TERMINAL: [&str; 5] = [
        "api-operation:PUT /serial",
        "api-path:/serial",
        "api-property:SerialDevice.rate_limiter",
        "api-property:SerialDevice.serial_out_path",
        "api-schema:SerialDevice",
    ];
    const SERIAL_WAVE_6: [&str; 1] = ["semantic.device:serial-stdin-stdout-rx-and-restore"];

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
    assert_eq!(count(Disposition::ImplementedAndVerified), 228);
    assert_eq!(count(Disposition::AuditRequired), 169);
    assert_eq!(count(Disposition::MissingPlatformFeasible), 4);
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

    let entropy_ids = ENTROPY_TERMINAL
        .into_iter()
        .chain(ENTROPY_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entropy_ids.len(),
        7,
        "entropy closure ledger must stay exact"
    );
    for id in ENTROPY_TERMINAL {
        let capability = by_id.get(id).expect("terminal entropy record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "entropy record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal entropy record must retain concrete evidence: {id}"
        );
    }
    for id in ENTROPY_WAVE_6 {
        let capability = by_id.get(id).expect("Wave 6 entropy record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 6 entropy handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 6"),
            "Wave 6 entropy handoff must name its owner: {id}"
        );
    }

    let entropy_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/entropy-contract.md"),
    )
    .expect("checked entropy contract must be readable");
    assert_eq!(
        entropy_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        7,
        "checked entropy contract must contain each exact ledger row once"
    );
    for id in entropy_ids {
        assert_eq!(
            entropy_contract.matches(&format!("| `{id}` |")).count(),
            1,
            "checked entropy contract row must be unique: {id}"
        );
    }

    let serial_ids = SERIAL_TERMINAL
        .into_iter()
        .chain(SERIAL_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(serial_ids.len(), 6, "serial closure ledger must stay exact");
    for id in SERIAL_TERMINAL {
        let capability = by_id.get(id).expect("terminal serial record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "serial record must remain implemented: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "terminal serial record must retain concrete evidence: {id}"
        );
    }
    for id in SERIAL_WAVE_6 {
        let capability = by_id.get(id).expect("Wave 6 serial record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "Wave 6 serial handoff must remain audit-owned: {id}"
        );
        assert!(
            capability.summary.contains("Wave 6"),
            "Wave 6 serial handoff must name its owner: {id}"
        );
    }

    let serial_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/serial-contract.md"),
    )
    .expect("checked serial contract must be readable");
    assert_eq!(
        serial_contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        6,
        "checked serial contract must contain each exact ledger row once"
    );
    for id in serial_ids {
        assert_eq!(
            serial_contract.matches(&format!("| `{id}` |")).count(),
            1,
            "checked serial contract row must be unique: {id}"
        );
    }

    let balloon_ids = BALLOON_TERMINAL
        .into_iter()
        .chain(BALLOON_WAVE_6)
        .collect::<BTreeSet<_>>();
    let memory_hotplug_ids = MEMORY_HOTPLUG_TERMINAL
        .into_iter()
        .chain(MEMORY_HOTPLUG_WAVE_6)
        .collect::<BTreeSet<_>>();
    let entropy_ids = ENTROPY_TERMINAL
        .into_iter()
        .chain(ENTROPY_WAVE_6)
        .collect::<BTreeSet<_>>();
    let serial_ids = SERIAL_TERMINAL
        .into_iter()
        .chain(SERIAL_WAVE_6)
        .collect::<BTreeSet<_>>();
    let time_identity_ids = TIME_IDENTITY_WAVE_6.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(balloon_ids.len(), 52);
    assert_eq!(memory_hotplug_ids.len(), 19);
    assert_eq!(entropy_ids.len(), 7);
    assert_eq!(serial_ids.len(), 6);
    assert_eq!(time_identity_ids.len(), 1);

    let family_sets = [
        &balloon_ids,
        &memory_hotplug_ids,
        &entropy_ids,
        &serial_ids,
        &time_identity_ids,
    ];
    for (index, left) in family_sets.iter().enumerate() {
        for right in family_sets.iter().skip(index + 1) {
            assert!(
                left.is_disjoint(right),
                "remaining-device family ledgers must be disjoint"
            );
        }
    }
    let remaining_ids = family_sets
        .iter()
        .flat_map(|ids| ids.iter().copied())
        .collect::<BTreeSet<_>>();
    assert_eq!(remaining_ids.len(), 85);

    let selected_inventory_ids = inventory
        .capabilities
        .iter()
        .filter(|capability| {
            let id = capability.id.as_str();
            let lower = id.to_ascii_lowercase();
            lower.contains("balloon")
                || lower.contains("entropy")
                || lower.contains("serial")
                || lower.contains("hotplug/memory")
                || lower.contains("memory-hotplug")
                || lower.contains("virtio-mem")
                || id.contains("MemoryHotplug")
                || id == "semantic.device:rtc-vmclock-vmgenid-and-pvtime"
        })
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        selected_inventory_ids, remaining_ids,
        "the reproducible remaining-device selector must resolve to exactly the five ledgers"
    );

    let remaining_terminal_ids = BALLOON_TERMINAL
        .into_iter()
        .chain(MEMORY_HOTPLUG_TERMINAL)
        .chain(ENTROPY_TERMINAL)
        .chain(SERIAL_TERMINAL)
        .collect::<BTreeSet<_>>();
    let remaining_wave_6_ids = BALLOON_WAVE_6
        .into_iter()
        .chain(MEMORY_HOTPLUG_WAVE_6)
        .chain(ENTROPY_WAVE_6)
        .chain(SERIAL_WAVE_6)
        .chain(TIME_IDENTITY_WAVE_6)
        .collect::<BTreeSet<_>>();
    assert_eq!(remaining_terminal_ids.len(), 77);
    assert_eq!(remaining_wave_6_ids.len(), 8);
    assert!(remaining_terminal_ids.is_disjoint(&remaining_wave_6_ids));
    assert_eq!(
        remaining_terminal_ids
            .union(&remaining_wave_6_ids)
            .copied()
            .collect::<BTreeSet<_>>(),
        remaining_ids
    );

    for id in &remaining_terminal_ids {
        let capability = by_id
            .get(id)
            .expect("terminal remaining-device record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::ImplementedAndVerified,
            "remaining-device terminal disposition drifted: {id}"
        );
        assert!(
            !capability.implementation.is_empty() && !capability.validation.is_empty(),
            "remaining-device terminal evidence must remain concrete: {id}"
        );
        assert!(
            !capability.summary.contains("#1440")
                && !capability.summary.contains("#1481")
                && !capability.summary.contains("future remaining-device"),
            "remaining-device terminal summary still names future aggregate work: {id}"
        );
    }
    for id in &remaining_wave_6_ids {
        let capability = by_id
            .get(id)
            .expect("Wave 6 remaining-device record must exist");
        assert_eq!(
            capability.disposition,
            Disposition::AuditRequired,
            "remaining-device Wave 6 disposition drifted: {id}"
        );
        assert!(
            capability.summary.contains(WAVE_6_ISSUE_URL),
            "remaining-device Wave 6 summary must name its exact issue URL: {id}"
        );
        for outcome in ["restore", "portability", "signed restored-guest"] {
            assert!(
                capability.summary.contains(outcome),
                "remaining-device Wave 6 summary must name missing {outcome}: {id}"
            );
        }
        assert!(
            capability.summary.contains("artifact")
                || capability.summary.contains("native artifacts"),
            "remaining-device Wave 6 summary must name missing artifact integration: {id}"
        );
        assert!(
            capability.summary.contains("migration/clone")
                || capability.summary.contains("repeated-clone"),
            "remaining-device Wave 6 summary must name missing clone or migration outcomes: {id}"
        );
    }

    let ledger_contracts = [
        (
            "balloon-contract.md",
            &balloon_ids,
            "checked balloon contract",
        ),
        (
            "memory-hotplug-contract.md",
            &memory_hotplug_ids,
            "checked memory-hotplug contract",
        ),
        (
            "entropy-contract.md",
            &entropy_ids,
            "checked entropy contract",
        ),
        ("serial-contract.md", &serial_ids, "checked serial contract"),
        (
            "time-identity-contract.md",
            &time_identity_ids,
            "checked time/identity contract",
        ),
    ];
    for (filename, expected_ids, context) in ledger_contracts {
        let contract = std::fs::read_to_string(
            repository_root
                .join("compat/firecracker/v1.16.0")
                .join(filename),
        )
        .unwrap_or_else(|error| panic!("{context} must be readable: {error}"));
        let rows = contract
            .lines()
            .filter(|line| line.starts_with("| `"))
            .collect::<Vec<_>>();
        let ids = rows
            .iter()
            .filter_map(|line| {
                line.strip_prefix("| `")
                    .and_then(|line| line.split_once("` |"))
                    .map(|(id, _)| id)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            rows.len(),
            expected_ids.len(),
            "{context} row count drifted"
        );
        assert_eq!(&ids, expected_ids, "{context} identity set drifted");
    }

    let aggregate_contract = std::fs::read_to_string(
        repository_root.join("compat/firecracker/v1.16.0/remaining-device-contract.md"),
    )
    .expect("checked aggregate remaining-device contract must be readable");
    let aggregate_rows = aggregate_contract
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let aggregate_ids = aggregate_rows
        .iter()
        .filter_map(|line| {
            line.strip_prefix("| `")
                .and_then(|line| line.split_once("` |"))
                .map(|(id, _)| id)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(aggregate_rows.len(), 85);
    assert_eq!(aggregate_ids, remaining_ids);
    for id in &remaining_ids {
        let row_prefix = format!("| `{id}` |");
        let row = aggregate_rows
            .iter()
            .copied()
            .find(|row| row.starts_with(&row_prefix))
            .unwrap_or_else(|| panic!("aggregate contract row must exist: {id}"));
        assert_eq!(
            aggregate_contract.matches(&row_prefix).count(),
            1,
            "aggregate contract row must be unique: {id}"
        );
        assert!(
            !row.contains("| `` |"),
            "aggregate evidence key is empty: {id}"
        );
        assert!(
            !row.contains("`W7`"),
            "selected row must not hand off to Wave 7: {id}"
        );
        if remaining_terminal_ids.contains(id) {
            assert!(row.contains("`implemented-and-verified`"));
            assert!(row.ends_with("| `terminal` |"));
        } else {
            assert!(row.contains("`audit-required`"));
            assert!(row.contains("`W6-"));
        }
    }
    for required in [
        "https://github.com/seven332/bangbang/issues/1490",
        "https://github.com/seven332/bangbang/issues/1491",
        "signed_executable_certifies_remaining_devices_over_mmio",
        "signed_executable_certifies_remaining_devices_over_product_pci",
        "aggregate_remaining_device_snapshot_preflight_failures_preserve_order_and_reuse",
        "remaining_device_owner_budget_covers_mmio_and_pci_and_reuses_resources",
        "normal_bundle_isolates_concurrent_default_serial_stdio_sessions",
    ] {
        assert!(
            aggregate_contract.contains(required),
            "aggregate contract must pin {required}"
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
