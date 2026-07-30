use super::codec::{ReservePolicy, decode_with_policy, encode_with_policy};
use super::*;
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO, PCI_SEGMENT_ZERO,
    PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState,
    SnapshotV2PciBarProbeState, SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixState, SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry,
    SnapshotV2PciWritableByte, SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
    SnapshotV2VirtioStateParts,
};
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciEndpointPhase,
};

const HEALTHY_DRIVER_OK: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
    | VIRTIO_DEVICE_STATUS_DRIVER
    | VIRTIO_DEVICE_STATUS_FEATURES_OK
    | VIRTIO_DEVICE_STATUS_DRIVER_OK;

fn fixture_bytes(fixture: &str) -> Vec<u8> {
    let compact = fixture.split_ascii_whitespace().collect::<String>();
    assert!(
        compact.len().is_multiple_of(2),
        "fixture must contain complete hexadecimal bytes"
    );
    compact
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).expect("fixture should be ASCII"),
                16,
            )
            .expect("fixture should contain hexadecimal bytes")
        })
        .collect()
}

fn guest_mac(index: usize) -> GuestMacAddress {
    GuestMacAddress::from_bytes([
        0x02,
        0,
        0,
        0,
        u8::try_from(index / 256).expect("test MAC high byte should fit"),
        u8::try_from(index % 256).expect("test MAC low byte should fit"),
    ])
}

fn profile(index: usize) -> NetworkDeviceProfile {
    NetworkDeviceProfile::new(Some(guest_mac(index)), Some(1500))
}

fn inactive_virtio(profile: NetworkDeviceProfile) -> SnapshotV2VirtioState {
    let available_features = VirtioNetworkConfigSpace::with_feature_capabilities(
        profile.guest_mac(),
        profile.mtu(),
        profile.feature_capabilities(),
    )
    .available_features();
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features,
        driver_features: 0,
        config_generation: 0,
        status: VIRTIO_DEVICE_STATUS_INIT,
        activated: false,
        queues: vec![
            SnapshotV2VirtioQueueState::from_parts(
                VIRTIO_NET_QUEUE_SIZE,
                0,
                false,
                GuestAddress::new(0),
                GuestAddress::new(0),
                GuestAddress::new(0),
            ),
            SnapshotV2VirtioQueueState::from_parts(
                VIRTIO_NET_QUEUE_SIZE,
                0,
                false,
                GuestAddress::new(0),
                GuestAddress::new(0),
                GuestAddress::new(0),
            ),
        ],
        pending_notifications: Vec::new(),
        interrupt_intents: vec![SnapshotV2InterruptIntent::Configuration],
    })
}

fn active_queue(base: u64) -> SnapshotV2VirtioQueueState {
    SnapshotV2VirtioQueueState::from_parts(
        VIRTIO_NET_QUEUE_SIZE,
        VIRTIO_NET_QUEUE_SIZE,
        true,
        GuestAddress::new(base),
        GuestAddress::new(base + 0x2000),
        GuestAddress::new(base + 0x4000),
    )
}

fn active_virtio(index: usize, profile: NetworkDeviceProfile) -> SnapshotV2VirtioState {
    let available_features = VirtioNetworkConfigSpace::with_feature_capabilities(
        profile.guest_mac(),
        profile.mtu(),
        profile.feature_capabilities(),
    )
    .available_features();
    let base = 0x10_0000 + u64::try_from(index).expect("test index should fit") * 0x20_0000;
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features,
        driver_features: available_features,
        config_generation: u32::try_from(index).expect("test generation should fit"),
        status: HEALTHY_DRIVER_OK,
        activated: true,
        queues: vec![active_queue(base), active_queue(base + 0x10_000)],
        pending_notifications: vec![0, 1],
        interrupt_intents: vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Queue { queue_index: 1 },
            SnapshotV2InterruptIntent::Configuration,
        ],
    })
}

fn mmio_transport(index: usize) -> SnapshotV2DeviceTransport {
    let index_u64 = u64::try_from(index).expect("test index should fit");
    let region = MmioRegion::new(
        MmioRegionId::new(index_u64 + 1),
        GuestAddress::new(0xd000_0000 + index_u64 * VIRTIO_MMIO_DEVICE_WINDOW_SIZE),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("test MMIO region should validate");
    SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
        u32::try_from(index % 2).expect("test selector should fit"),
        u32::try_from((index + 1) % 2).expect("test selector should fit"),
        u32::try_from(index % VIRTIO_NET_QUEUE_COUNT).expect("test queue should fit"),
        region,
        GuestInterruptLine::new(
            32 + u32::try_from(index).expect("test interrupt index should fit"),
        )
        .expect("test SPI should validate"),
    ))
}

fn pci_transport(index: usize) -> SnapshotV2DeviceTransport {
    let device = PCI_FIRST_ENDPOINT_DEVICE
        .checked_add(u8::try_from(index).expect("test PCI index should fit"))
        .expect("test PCI device should fit");
    let sbdf = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, device, PCI_FUNCTION_ZERO)
        .expect("test SBDF should validate");
    let bar_start = PCI_BAR64_START
        + u64::try_from(index).expect("test PCI index should fit") * VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    let bar_range =
        GuestMemoryRange::new(GuestAddress::new(bar_start), VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("test BAR should validate");
    let message_data = u32::try_from(index).expect("test message index should fit");
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries: vec![
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 64 + message_data, 0),
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 96 + message_data, 1),
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 128 + message_data, 0),
        ],
        pending_words: vec![0b010],
        enabled: true,
        function_masked: false,
        config_vector: 0,
        queue_vectors: vec![1, 2],
        pending_transition_observed: true,
    });
    SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(
        SnapshotV2PciDeviceStateParts {
            phase: VirtioPciEndpointPhase::Active,
            origin: if index.is_multiple_of(2) {
                StorageDeviceOrigin::Startup
            } else {
                StorageDeviceOrigin::Runtime
            },
            sbdf,
            bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
            bar_address_space: PciBarAddressSpace::Memory64,
            bar_prefetchable: PciBarPrefetchable::No,
            bar_range,
            device_feature_select: u32::try_from(index % 2)
                .expect("test feature selector should fit"),
            driver_feature_select: u32::try_from((index + 1) % 2)
                .expect("test feature selector should fit"),
            queue_select: u16::try_from(index % VIRTIO_NET_QUEUE_COUNT)
                .expect("test queue selector should fit"),
            pci_cfg_bar: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
            pci_cfg_offset: 0x24,
            pci_cfg_length: 4,
            writable_bytes: vec![
                SnapshotV2PciWritableByte::from_parts(0x04, 0x07),
                SnapshotV2PciWritableByte::from_parts(0x05, 0x80),
                SnapshotV2PciWritableByte::from_parts(0x0c, 0x40),
                SnapshotV2PciWritableByte::from_parts(0x3c, 0x2a),
            ],
            bar_probes: vec![
                SnapshotV2PciBarProbeState::from_parts(0, false),
                SnapshotV2PciBarProbeState::from_parts(1, true),
            ],
            msix,
        },
    ))
}

fn limiter() -> SnapshotV2NetworkLimiterState {
    SnapshotV2NetworkLimiterState::new(
        Some(SnapshotV2NetworkTokenBucketState::new(
            100,
            Some(20),
            50,
            75,
            10,
            1_000,
        )),
        Some(SnapshotV2NetworkTokenBucketState::new(
            50, None, 100, 25, 0, 2_000,
        )),
    )
}

fn interface(
    index: usize,
    backend: SnapshotV2NetworkBackendClass,
    transport: SnapshotV2DeviceTransport,
    active: bool,
    selector: String,
    iface_id: String,
) -> SnapshotV2NetworkInterfaceState {
    let profile = profile(index);
    let (virtio, local, tx_limiter) = if active {
        (
            active_virtio(index, profile),
            SnapshotV2NetworkLocalState::new(
                Some(SnapshotV2NetworkQueueState::new(7, 7)),
                Some(SnapshotV2NetworkQueueState::new(9, 9)),
                SnapshotV2NetworkRetryState::After {
                    remaining_nanos: 25,
                },
            ),
            limiter(),
        )
    } else {
        (
            inactive_virtio(profile),
            SnapshotV2NetworkLocalState::new(None, None, SnapshotV2NetworkRetryState::None),
            SnapshotV2NetworkLimiterState::new(None, None),
        )
    };
    SnapshotV2NetworkInterfaceState::try_from_parts(SnapshotV2NetworkInterfaceStateParts {
        iface_id,
        captured_selector: selector,
        requested_guest_mac: Some(guest_mac(index)),
        requested_mtu: Some(1500),
        profile,
        backend,
        local,
        virtio,
        rx_limiter: if active {
            limiter()
        } else {
            SnapshotV2NetworkLimiterState::new(None, None)
        },
        tx_limiter,
        transport,
    })
    .expect("test network interface should validate")
}

fn inactive_mmio_state() -> SnapshotV2NetworkState {
    SnapshotV2NetworkState::try_new(
        vec![interface(
            0,
            SnapshotV2NetworkBackendClass::Vmnet,
            mmio_transport(0),
            false,
            "vmnet:host".to_owned(),
            "eth0".to_owned(),
        )],
        None,
    )
    .expect("inactive MMIO network state should validate")
}

fn mmds_state(count: usize) -> SnapshotV2MmdsState {
    SnapshotV2MmdsState::new(
        MmdsVersion::V2,
        Some(Ipv4Addr::new(169, 254, 100, 10)),
        true,
        (0..count)
            .map(|index| {
                SnapshotV2MmdsInterfaceState::new(
                    u16::try_from(index).expect("MMDS test index should fit"),
                    DEFAULT_MMDS_MAC_ADDRESS,
                    Ipv4Addr::new(169, 254, 100, 10),
                    MMDS_GUEST_TCP_PORT,
                )
            })
            .collect(),
    )
}

fn active_pci_mmds_state(count: usize, maximum_strings: bool) -> SnapshotV2NetworkState {
    let interfaces = (0..count)
        .map(|index| {
            let iface_id = if maximum_strings {
                format!(
                    "{index:02}{}",
                    "a".repeat(NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES - 2)
                )
            } else {
                format!("eth{index}")
            };
            let selector = if maximum_strings {
                "s".repeat(NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES)
            } else {
                "vmnet:shared".to_owned()
            };
            interface(
                index,
                SnapshotV2NetworkBackendClass::MmdsOnly,
                pci_transport(index),
                true,
                selector,
                iface_id,
            )
        })
        .collect();
    SnapshotV2NetworkState::try_new(interfaces, Some(mmds_state(count)))
        .expect("active PCI MMDS state should validate")
}

fn read_u64(bytes: &[u8], offset: usize) -> usize {
    usize::try_from(u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("test field should be complete"),
    ))
    .expect("test length should fit")
}

fn write_u64(bytes: &mut [u8], offset: usize, value: usize) {
    bytes[offset..offset + 8].copy_from_slice(
        &u64::try_from(value)
            .expect("test length should fit")
            .to_le_bytes(),
    );
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn cloned_virtio_parts(state: &SnapshotV2VirtioState) -> SnapshotV2VirtioStateParts {
    SnapshotV2VirtioStateParts {
        available_features: state.available_features(),
        driver_features: state.driver_features(),
        config_generation: state.config_generation(),
        status: state.status(),
        activated: state.is_activated(),
        queues: state.queues().to_vec(),
        pending_notifications: state.pending_notifications().to_vec(),
        interrupt_intents: state.interrupt_intents().to_vec(),
    }
}

fn assert_state_error(state: &SnapshotV2NetworkState, expected: SnapshotV2NetworkStateBuildError) {
    assert_eq!(validate_network_state(state), Err(expected));
}

fn assert_mutation_rejected(encoded: &[u8], name: &str, mutate: impl FnOnce(&mut [u8])) {
    let mut hostile = encoded.to_vec();
    mutate(&mut hostile);
    assert!(
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &hostile,)
            .is_err(),
        "semantic mutation {name} should fail"
    );
}

#[test]
fn exact_bounds_are_derived_from_every_record_field() {
    assert_eq!(NATIVE_V2_NETWORK_MAX_INTERFACES, 16);
    assert_eq!(NATIVE_V2_NETWORK_INTERFACE_RECORD_MAX_BYTES, 5_168);
    assert_eq!(NATIVE_V2_NETWORK_MMDS_STATE_MAX_BYTES, 288);
    assert_eq!(NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES, 83_552);
    assert_eq!(NATIVE_V2_NETWORK_STATE_MAX_BYTES, 512 * 1024);
    assert_eq!(
        crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
        16 * 1024 * 1024
    );
}

#[test]
fn inactive_mmio_state_round_trips_canonically() {
    let state = inactive_mmio_state();
    let encoded = state
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("inactive MMIO state should encode");
    let fixture = fixture_bytes(include_str!("fixtures/inactive-mmio.hex"));
    assert_eq!(encoded, fixture);
    let decoded =
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &fixture)
            .expect("inactive MMIO state should decode");

    assert_eq!(decoded, state);
    assert_eq!(
        decoded
            .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
            .expect("decoded state should re-encode"),
        encoded
    );
}

#[test]
fn active_pci_mmds_state_round_trips_all_continuation_fields() {
    let state = active_pci_mmds_state(2, false);
    let encoded = state
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("active PCI state should encode");
    let decoded =
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &encoded)
            .expect("active PCI state should decode");

    assert_eq!(decoded, state);
    assert_eq!(
        decoded
            .mmds()
            .expect("MMDS should survive")
            .interfaces()
            .len(),
        2
    );
    assert!(decoded.interfaces()[0].local().tx_retry().has_retry());
    assert!(decoded.interfaces()[0].tx_limiter().is_configured());
    assert!(matches!(
        decoded.interfaces()[1].transport(),
        SnapshotV2DeviceTransport::Pci(pci)
            if pci.origin() == StorageDeviceOrigin::Runtime
    ));
}

#[test]
fn subset_mmds_and_every_retry_disposition_round_trip() {
    let subset = SnapshotV2NetworkState::try_new(
        vec![
            interface(
                0,
                SnapshotV2NetworkBackendClass::Vmnet,
                pci_transport(0),
                true,
                "vmnet:shared".to_owned(),
                "eth0".to_owned(),
            ),
            interface(
                1,
                SnapshotV2NetworkBackendClass::Vmnet,
                pci_transport(1),
                true,
                "vmnet:shared".to_owned(),
                "eth1".to_owned(),
            ),
        ],
        Some(mmds_state(1)),
    )
    .expect("subset MMDS should retain VMNET as the fresh backend class");
    let subset_bytes = subset
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("subset MMDS should encode");
    assert_eq!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &subset_bytes,
        )
        .expect("subset MMDS should decode"),
        subset
    );

    let mut immediate = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        true,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    immediate.local = SnapshotV2NetworkLocalState::new(
        Some(SnapshotV2NetworkQueueState::new(7, 7)),
        Some(SnapshotV2NetworkQueueState::new(9, 9)),
        SnapshotV2NetworkRetryState::Immediate,
    );
    let immediate = SnapshotV2NetworkState::try_new(vec![immediate], None)
        .expect("immediate retry with an active TX limiter should validate");
    let immediate_bytes = immediate
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("immediate retry should encode");
    assert_eq!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &immediate_bytes,
        )
        .expect("immediate retry should decode"),
        immediate
    );
}

#[test]
fn typed_validator_rejects_identity_profile_limiter_retry_and_queue_faults() {
    fn expect_interface_error(
        interface: SnapshotV2NetworkInterfaceState,
        expected: SnapshotV2NetworkStateBuildError,
    ) {
        assert!(matches!(
            SnapshotV2NetworkState::try_new(vec![interface], None),
            Err(found) if found == expected
        ));
    }

    let valid = || {
        interface(
            0,
            SnapshotV2NetworkBackendClass::Vmnet,
            mmio_transport(0),
            false,
            "vmnet:host".to_owned(),
            "eth0".to_owned(),
        )
    };

    let mut empty_id = valid();
    empty_id.iface_id.clear();
    expect_interface_error(
        empty_id,
        SnapshotV2NetworkStateBuildError::InterfaceIdentity,
    );

    let mut invalid_id = valid();
    invalid_id.iface_id = "invalid-id".to_owned();
    expect_interface_error(
        invalid_id,
        SnapshotV2NetworkStateBuildError::InterfaceIdentity,
    );

    let mut overlong_id = valid();
    overlong_id.iface_id = "a".repeat(NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES + 1);
    expect_interface_error(
        overlong_id,
        SnapshotV2NetworkStateBuildError::InterfaceIdentity,
    );

    let mut invalid_selector = valid();
    invalid_selector.captured_selector = "vmnet:\0host".to_owned();
    expect_interface_error(
        invalid_selector,
        SnapshotV2NetworkStateBuildError::InterfaceIdentity,
    );

    let mut overlong_selector = valid();
    overlong_selector.captured_selector =
        "s".repeat(NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES + 1);
    expect_interface_error(
        overlong_selector,
        SnapshotV2NetworkStateBuildError::InterfaceIdentity,
    );

    let mut mismatched_request = valid();
    mismatched_request.requested_mtu = Some(1_400);
    expect_interface_error(
        mismatched_request,
        SnapshotV2NetworkStateBuildError::InterfaceProfile,
    );

    let mut invalid_limiter = valid();
    invalid_limiter.rx_limiter = SnapshotV2NetworkLimiterState::new(
        Some(SnapshotV2NetworkTokenBucketState::new(0, None, 1, 0, 0, 0)),
        None,
    );
    expect_interface_error(invalid_limiter, SnapshotV2NetworkStateBuildError::Limiter);

    let mut active_without_limiter = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        true,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    active_without_limiter.tx_limiter = SnapshotV2NetworkLimiterState::new(None, None);
    expect_interface_error(
        active_without_limiter,
        SnapshotV2NetworkStateBuildError::Retry,
    );

    let mut mismatched_cursor = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        true,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    mismatched_cursor.local.active_rx_queue = Some(SnapshotV2NetworkQueueState::new(7, 8));
    expect_interface_error(mismatched_cursor, SnapshotV2NetworkStateBuildError::Queue);

    let mut invalid_common = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        true,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    let common = invalid_common.virtio();
    invalid_common.virtio = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: common.available_features(),
        driver_features: common.driver_features(),
        config_generation: common.config_generation(),
        status: common.status(),
        activated: common.is_activated(),
        queues: common.queues().to_vec(),
        pending_notifications: vec![1, 0],
        interrupt_intents: common.interrupt_intents().to_vec(),
    });
    expect_interface_error(invalid_common, SnapshotV2NetworkStateBuildError::Virtio);
}

#[test]
fn aggregate_validator_rejects_duplicate_transport_and_memory_placements() {
    let first = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        false,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );

    let mut duplicate_id = interface(
        1,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(1),
        false,
        "vmnet:shared".to_owned(),
        "eth1".to_owned(),
    );
    duplicate_id.iface_id = first.iface_id.clone();
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![first.clone(), duplicate_id], None),
        Err(SnapshotV2NetworkStateBuildError::DuplicateInterface)
    ));

    let mut duplicate_mac = interface(
        1,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(1),
        false,
        "vmnet:shared".to_owned(),
        "eth1".to_owned(),
    );
    duplicate_mac.requested_guest_mac = first.requested_guest_mac;
    duplicate_mac.profile = first.profile;
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![first.clone(), duplicate_mac], None),
        Err(SnapshotV2NetworkStateBuildError::DuplicateMac)
    ));

    let mixed = interface(
        1,
        SnapshotV2NetworkBackendClass::Vmnet,
        pci_transport(1),
        false,
        "vmnet:shared".to_owned(),
        "eth1".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![first.clone(), mixed], None),
        Err(SnapshotV2NetworkStateBuildError::Transport)
    ));

    let duplicate_placement = interface(
        1,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        false,
        "vmnet:shared".to_owned(),
        "eth1".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![first, duplicate_placement], None),
        Err(SnapshotV2NetworkStateBuildError::DuplicatePlacement)
    ));

    let first_active = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        pci_transport(0),
        true,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    let mut overlapping_queue = interface(
        1,
        SnapshotV2NetworkBackendClass::Vmnet,
        pci_transport(1),
        true,
        "vmnet:shared".to_owned(),
        "eth1".to_owned(),
    );
    overlapping_queue.virtio = active_virtio(0, overlapping_queue.profile);
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![first_active, overlapping_queue], None),
        Err(SnapshotV2NetworkStateBuildError::Queue)
    ));

    let queue_region = MmioRegion::new(
        MmioRegionId::new(99),
        GuestAddress::new(0x10_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("overlapping MMIO fixture should validate structurally");
    let overlapping_transport =
        SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
            0,
            1,
            0,
            queue_region,
            GuestInterruptLine::new(99).expect("fixture interrupt should validate"),
        ));
    let overlaps_own_queue = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        overlapping_transport,
        true,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![overlaps_own_queue], None),
        Err(SnapshotV2NetworkStateBuildError::Queue)
    ));
}

#[test]
fn mmds_policy_rejects_backend_and_stack_disagreement() {
    let mmds_only_without_mmds = interface(
        0,
        SnapshotV2NetworkBackendClass::MmdsOnly,
        mmio_transport(0),
        false,
        "mmds-only".to_owned(),
        "eth0".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![mmds_only_without_mmds], None),
        Err(SnapshotV2NetworkStateBuildError::Mmds)
    ));

    let all_selected_but_vmnet = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        false,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![all_selected_but_vmnet], Some(mmds_state(1))),
        Err(SnapshotV2NetworkStateBuildError::Mmds)
    ));

    let selected = SnapshotV2MmdsState::new(
        MmdsVersion::V2,
        None,
        false,
        vec![SnapshotV2MmdsInterfaceState::new(
            0,
            EthernetMacAddress::from_octets([0x02, 0, 0, 0, 0, 1]),
            DEFAULT_MMDS_IPV4_ADDRESS,
            MMDS_GUEST_TCP_PORT,
        )],
    );
    let mmds_only = interface(
        0,
        SnapshotV2NetworkBackendClass::MmdsOnly,
        mmio_transport(0),
        false,
        "mmds-only".to_owned(),
        "eth0".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![mmds_only], Some(selected)),
        Err(SnapshotV2NetworkStateBuildError::Mmds)
    ));
}

#[test]
fn maximum_state_reaches_the_exact_derived_worst_case() {
    let state = active_pci_mmds_state(NATIVE_V2_NETWORK_MAX_INTERFACES, true);
    let encoded = state
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("maximum state should encode");

    assert_eq!(encoded.len(), NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES);
    assert_eq!(
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &encoded,)
            .expect("maximum state should decode"),
        state
    );
}

#[test]
fn aggregate_rejects_empty_duplicate_and_incoherent_mmds_forms() {
    assert!(matches!(
        SnapshotV2NetworkState::try_new(Vec::new(), None),
        Err(SnapshotV2NetworkStateBuildError::InterfaceCount)
    ));

    let first = interface(
        0,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(0),
        false,
        "vmnet:host".to_owned(),
        "eth0".to_owned(),
    );
    let duplicate = interface(
        1,
        SnapshotV2NetworkBackendClass::Vmnet,
        mmio_transport(1),
        false,
        "vmnet:shared".to_owned(),
        "eth0".to_owned(),
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(vec![first, duplicate], None),
        Err(SnapshotV2NetworkStateBuildError::DuplicateInterface)
    ));

    let interfaces = vec![
        interface(
            0,
            SnapshotV2NetworkBackendClass::Vmnet,
            mmio_transport(0),
            false,
            "vmnet:host".to_owned(),
            "eth0".to_owned(),
        ),
        interface(
            1,
            SnapshotV2NetworkBackendClass::Vmnet,
            mmio_transport(1),
            false,
            "vmnet:shared".to_owned(),
            "eth1".to_owned(),
        ),
    ];
    let unordered = SnapshotV2MmdsState::new(
        MmdsVersion::V1,
        None,
        false,
        vec![
            SnapshotV2MmdsInterfaceState::new(
                1,
                DEFAULT_MMDS_MAC_ADDRESS,
                DEFAULT_MMDS_IPV4_ADDRESS,
                MMDS_GUEST_TCP_PORT,
            ),
            SnapshotV2MmdsInterfaceState::new(
                0,
                DEFAULT_MMDS_MAC_ADDRESS,
                DEFAULT_MMDS_IPV4_ADDRESS,
                MMDS_GUEST_TCP_PORT,
            ),
        ],
    );
    assert!(matches!(
        SnapshotV2NetworkState::try_new(interfaces, Some(unordered)),
        Err(SnapshotV2NetworkStateBuildError::Mmds)
    ));
}

#[test]
fn subset_mmds_state_round_trips_with_vmnet_backends() {
    let interfaces = (0..3)
        .map(|index| {
            interface(
                index,
                SnapshotV2NetworkBackendClass::Vmnet,
                pci_transport(index),
                true,
                format!("vmnet:subset-{index}"),
                format!("eth{index}"),
            )
        })
        .collect();
    let state = SnapshotV2NetworkState::try_new(interfaces, Some(mmds_state(1)))
        .expect("strict MMDS subset should retain vmnet backends");
    let encoded = state
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("strict MMDS subset should encode");

    assert_eq!(
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &encoded)
            .expect("strict MMDS subset should decode"),
        state
    );
}

#[test]
fn typed_identity_profile_and_duplicate_relationships_are_closed() {
    for invalid_id in [
        String::new(),
        "eth-0".to_owned(),
        "a".repeat(NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES + 1),
    ] {
        let mut state = inactive_mmio_state();
        state.interfaces[0].iface_id = invalid_id;
        assert_state_error(&state, SnapshotV2NetworkStateBuildError::InterfaceIdentity);
    }
    for invalid_selector in [
        String::new(),
        "vmnet:\0host".to_owned(),
        "s".repeat(NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES + 1),
    ] {
        let mut state = inactive_mmio_state();
        state.interfaces[0].captured_selector = invalid_selector;
        assert_state_error(&state, SnapshotV2NetworkStateBuildError::InterfaceIdentity);
    }

    let mut requested_mac_mismatch = inactive_mmio_state();
    requested_mac_mismatch.interfaces[0].requested_guest_mac = Some(guest_mac(1));
    assert_state_error(
        &requested_mac_mismatch,
        SnapshotV2NetworkStateBuildError::InterfaceProfile,
    );

    let mut requested_mtu_mismatch = inactive_mmio_state();
    requested_mtu_mismatch.interfaces[0].requested_mtu = Some(1400);
    assert_state_error(
        &requested_mtu_mismatch,
        SnapshotV2NetworkStateBuildError::InterfaceProfile,
    );

    let mut incomplete_features = inactive_mmio_state();
    incomplete_features.interfaces[0].profile = incomplete_features.interfaces[0]
        .profile
        .with_feature_capabilities(VirtioNetworkFeatureCapabilities::none().with_guest_tso4(true));
    assert_state_error(
        &incomplete_features,
        SnapshotV2NetworkStateBuildError::InterfaceProfile,
    );

    let mut direct_mmds = active_pci_mmds_state(1, false);
    direct_mmds.interfaces[0].profile = direct_mmds.interfaces[0].profile.with_packet_envelope(
        crate::network_packet::VirtioNetworkPacketEnvelope::DirectVirtioHeader,
    );
    assert_state_error(
        &direct_mmds,
        SnapshotV2NetworkStateBuildError::InterfaceProfile,
    );

    let duplicate_mac = SnapshotV2NetworkState {
        interfaces: vec![
            interface(
                0,
                SnapshotV2NetworkBackendClass::Vmnet,
                mmio_transport(0),
                false,
                "vmnet:first".to_owned(),
                "eth0".to_owned(),
            ),
            interface(
                0,
                SnapshotV2NetworkBackendClass::Vmnet,
                mmio_transport(1),
                false,
                "vmnet:second".to_owned(),
                "eth1".to_owned(),
            ),
        ],
        mmds: None,
    };
    assert_state_error(
        &duplicate_mac,
        SnapshotV2NetworkStateBuildError::DuplicateMac,
    );
}

#[test]
fn typed_virtio_queue_limiter_and_retry_relationships_are_closed() {
    let mut feature_mismatch = inactive_mmio_state();
    let mut parts = cloned_virtio_parts(&feature_mismatch.interfaces[0].virtio);
    parts.available_features ^= 1;
    feature_mismatch.interfaces[0].virtio = SnapshotV2VirtioState::from_parts(parts);
    assert_state_error(&feature_mismatch, SnapshotV2NetworkStateBuildError::Virtio);

    let mut driver_without_status = inactive_mmio_state();
    let mut parts = cloned_virtio_parts(&driver_without_status.interfaces[0].virtio);
    parts.driver_features = VIRTIO_MMIO_VERSION_1_FEATURE;
    driver_without_status.interfaces[0].virtio = SnapshotV2VirtioState::from_parts(parts);
    assert_state_error(
        &driver_without_status,
        SnapshotV2NetworkStateBuildError::Virtio,
    );

    let mut duplicate_notifications = active_pci_mmds_state(1, false);
    let mut parts = cloned_virtio_parts(&duplicate_notifications.interfaces[0].virtio);
    parts.pending_notifications = vec![0, 0];
    duplicate_notifications.interfaces[0].virtio = SnapshotV2VirtioState::from_parts(parts);
    assert_state_error(
        &duplicate_notifications,
        SnapshotV2NetworkStateBuildError::Virtio,
    );

    let mut invalid_queue = active_pci_mmds_state(1, false);
    let mut parts = cloned_virtio_parts(&invalid_queue.interfaces[0].virtio);
    let queue = parts.queues[0];
    parts.queues[0] = SnapshotV2VirtioQueueState::from_parts(
        VIRTIO_NET_QUEUE_SIZE - 1,
        queue.size(),
        queue.ready(),
        queue.descriptor_table(),
        queue.driver_ring(),
        queue.device_ring(),
    );
    invalid_queue.interfaces[0].virtio = SnapshotV2VirtioState::from_parts(parts);
    assert_state_error(&invalid_queue, SnapshotV2NetworkStateBuildError::Queue);

    let mut overlapping_queues = active_pci_mmds_state(1, false);
    let mut parts = cloned_virtio_parts(&overlapping_queues.interfaces[0].virtio);
    let first = parts.queues[0];
    let second = parts.queues[1];
    parts.queues[1] = SnapshotV2VirtioQueueState::from_parts(
        second.max_size(),
        second.size(),
        second.ready(),
        first.descriptor_table(),
        second.driver_ring(),
        second.device_ring(),
    );
    overlapping_queues.interfaces[0].virtio = SnapshotV2VirtioState::from_parts(parts);
    assert_state_error(&overlapping_queues, SnapshotV2NetworkStateBuildError::Queue);

    let mut cursor_mismatch = active_pci_mmds_state(1, false);
    cursor_mismatch.interfaces[0].local.active_tx_queue =
        Some(SnapshotV2NetworkQueueState::new(9, 8));
    assert_state_error(&cursor_mismatch, SnapshotV2NetworkStateBuildError::Queue);

    for bucket in [
        SnapshotV2NetworkTokenBucketState::new(0, None, 1, 0, 0, 0),
        SnapshotV2NetworkTokenBucketState::new(1, None, 0, 0, 0, 0),
        SnapshotV2NetworkTokenBucketState::new(1, None, u64::MAX, 0, 0, 0),
        SnapshotV2NetworkTokenBucketState::new(1, None, 1, 2, 0, 0),
        SnapshotV2NetworkTokenBucketState::new(1, Some(1), 1, 1, 2, 0),
    ] {
        let mut invalid_limiter = inactive_mmio_state();
        invalid_limiter.interfaces[0].rx_limiter =
            SnapshotV2NetworkLimiterState::new(Some(bucket), None);
        assert_state_error(&invalid_limiter, SnapshotV2NetworkStateBuildError::Limiter);
    }

    let mut delayed_zero = active_pci_mmds_state(1, false);
    delayed_zero.interfaces[0].local.tx_retry =
        SnapshotV2NetworkRetryState::After { remaining_nanos: 0 };
    assert_state_error(&delayed_zero, SnapshotV2NetworkStateBuildError::Retry);

    let mut retry_without_limiter = active_pci_mmds_state(1, false);
    retry_without_limiter.interfaces[0].tx_limiter = SnapshotV2NetworkLimiterState::new(None, None);
    assert_state_error(
        &retry_without_limiter,
        SnapshotV2NetworkStateBuildError::Retry,
    );
}

#[test]
fn aggregate_transport_placement_queue_and_mmds_relationships_are_closed() {
    let mixed_transport = SnapshotV2NetworkState {
        interfaces: vec![
            interface(
                0,
                SnapshotV2NetworkBackendClass::Vmnet,
                mmio_transport(0),
                false,
                "vmnet:first".to_owned(),
                "eth0".to_owned(),
            ),
            interface(
                1,
                SnapshotV2NetworkBackendClass::Vmnet,
                pci_transport(1),
                false,
                "vmnet:second".to_owned(),
                "eth1".to_owned(),
            ),
        ],
        mmds: None,
    };
    assert_state_error(
        &mixed_transport,
        SnapshotV2NetworkStateBuildError::Transport,
    );

    let duplicate_placement = SnapshotV2NetworkState {
        interfaces: vec![
            interface(
                0,
                SnapshotV2NetworkBackendClass::Vmnet,
                mmio_transport(0),
                false,
                "vmnet:first".to_owned(),
                "eth0".to_owned(),
            ),
            interface(
                1,
                SnapshotV2NetworkBackendClass::Vmnet,
                mmio_transport(0),
                false,
                "vmnet:second".to_owned(),
                "eth1".to_owned(),
            ),
        ],
        mmds: None,
    };
    assert_state_error(
        &duplicate_placement,
        SnapshotV2NetworkStateBuildError::DuplicatePlacement,
    );

    let overlapping_region = MmioRegion::new(
        MmioRegionId::new(99),
        GuestAddress::new(0x10_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("queue-overlap MMIO region should validate structurally");
    let queue_overlapping_transport =
        SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
            0,
            0,
            0,
            overlapping_region,
            GuestInterruptLine::new(99).expect("queue-overlap SPI should validate"),
        ));
    let placement_overlaps_previous_queue = SnapshotV2NetworkState {
        interfaces: vec![
            interface(
                0,
                SnapshotV2NetworkBackendClass::Vmnet,
                mmio_transport(0),
                true,
                "vmnet:first".to_owned(),
                "eth0".to_owned(),
            ),
            interface(
                1,
                SnapshotV2NetworkBackendClass::Vmnet,
                queue_overlapping_transport,
                false,
                "vmnet:second".to_owned(),
                "eth1".to_owned(),
            ),
        ],
        mmds: None,
    };
    assert_state_error(
        &placement_overlaps_previous_queue,
        SnapshotV2NetworkStateBuildError::Queue,
    );

    let mut absent_mmds_with_local_backends = active_pci_mmds_state(2, false);
    absent_mmds_with_local_backends.mmds = None;
    assert_state_error(
        &absent_mmds_with_local_backends,
        SnapshotV2NetworkStateBuildError::Mmds,
    );

    let mut subset_with_local_backends = active_pci_mmds_state(2, false);
    subset_with_local_backends.mmds = Some(mmds_state(1));
    assert_state_error(
        &subset_with_local_backends,
        SnapshotV2NetworkStateBuildError::Mmds,
    );

    let mut invalid_stack = active_pci_mmds_state(1, false);
    invalid_stack
        .mmds
        .as_mut()
        .expect("test MMDS should exist")
        .interfaces[0]
        .tcp_port = MMDS_GUEST_TCP_PORT + 1;
    assert_state_error(&invalid_stack, SnapshotV2NetworkStateBuildError::Mmds);
}

#[test]
fn decoder_rejects_hostile_outer_record_and_field_mutations() {
    let state = inactive_mmio_state();
    let encoded = state
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");

    assert!(matches!(
        SnapshotV2NetworkState::decode(SnapshotFormatVersion::new(2, 10, 0), &encoded),
        Err(SnapshotV2NetworkStateDecodeError::UnsupportedVersion)
    ));
    assert!(matches!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &vec![0; NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES + 1],
        ),
        Err(SnapshotV2NetworkStateDecodeError::TooLarge)
    ));

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(matches!(
        SnapshotV2NetworkState::decode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION, &trailing,),
        Err(SnapshotV2NetworkStateDecodeError::InvalidLayout)
    ));

    for offset in [0, 20] {
        let mut hostile = encoded.clone();
        hostile[offset] ^= 1;
        assert!(
            SnapshotV2NetworkState::decode(
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                &hostile,
            )
            .is_err(),
            "outer mutation at {offset} should fail"
        );
    }

    let record_offset = read_u64(&encoded, 64 + 8);
    let mut wrong_section = encoded.clone();
    wrong_section[record_offset + 64] = 9;
    assert!(matches!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &wrong_section,
        ),
        Err(SnapshotV2NetworkStateDecodeError::InvalidLayout)
    ));

    let identity_offset = record_offset + read_u64(&encoded, record_offset + 64 + 8);
    let identity_length = read_u64(&encoded, record_offset + 64 + 16);
    let id_length = usize::from(u16::from_le_bytes(
        encoded[identity_offset..identity_offset + 2]
            .try_into()
            .expect("ID length should exist"),
    ));
    let selector_length = usize::from(u16::from_le_bytes(
        encoded[identity_offset + 2..identity_offset + 4]
            .try_into()
            .expect("selector length should exist"),
    ));
    let semantic_end = identity_offset + 32 + id_length + selector_length;
    assert!(semantic_end < identity_offset + identity_length);
    let mut nonzero_padding = encoded.clone();
    nonzero_padding[semantic_end] = 1;
    assert!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &nonzero_padding,
        )
        .is_err()
    );

    let mut unknown_envelope = encoded.clone();
    unknown_envelope[identity_offset + 6] = 99;
    assert!(matches!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &unknown_envelope,
        ),
        Err(SnapshotV2NetworkStateDecodeError::InvalidField)
    ));

    let mut overlapping_record = encoded.clone();
    write_u64(&mut overlapping_record, 64 + 8, record_offset + 8);
    assert!(matches!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &overlapping_record,
        ),
        Err(SnapshotV2NetworkStateDecodeError::InvalidLayout)
    ));
}

#[test]
fn decoder_rejects_semantic_tag_count_and_relationship_mutations() {
    let encoded = inactive_mmio_state()
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("inactive mutation fixture should encode");
    let record_offset = read_u64(&encoded, 64 + 8);
    let section_directory = record_offset + NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES;
    let identity_offset = record_offset + read_u64(&encoded, section_directory + 8);
    let id_length = usize::from(u16::from_le_bytes(
        encoded[identity_offset..identity_offset + 2]
            .try_into()
            .expect("identity ID length should exist"),
    ));
    let selector_offset = identity_offset + 32 + id_length;
    let local_directory = section_directory + NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let local_offset = record_offset + read_u64(&encoded, local_directory + 8);
    let common_directory = section_directory + 2 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let common_offset = record_offset + read_u64(&encoded, common_directory + 8);
    let transport_directory =
        section_directory + 4 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let transport_offset = record_offset + read_u64(&encoded, transport_directory + 8);

    assert_mutation_rejected(&encoded, "unknown identity flag", |bytes| {
        write_u16(bytes, identity_offset + 4, 0x800f);
    });
    assert_mutation_rejected(&encoded, "absent requested MAC payload", |bytes| {
        write_u16(bytes, identity_offset + 4, 0x000e);
    });
    assert_mutation_rejected(&encoded, "unknown packet envelope", |bytes| {
        bytes[identity_offset + 6] = 99;
    });
    assert_mutation_rejected(&encoded, "unknown feature capability", |bytes| {
        bytes[identity_offset + 15] |= 0x80;
    });
    assert_mutation_rejected(&encoded, "empty interface ID", |bytes| {
        write_u16(bytes, identity_offset, 0);
    });
    assert_mutation_rejected(&encoded, "empty captured selector", |bytes| {
        write_u16(bytes, identity_offset + 2, 0);
    });
    assert_mutation_rejected(&encoded, "invalid interface ID UTF-8", |bytes| {
        bytes[identity_offset + 32] = 0xff;
    });
    assert_mutation_rejected(&encoded, "invalid interface ID character", |bytes| {
        bytes[identity_offset + 32] = b'-';
    });
    assert_mutation_rejected(&encoded, "captured selector control character", |bytes| {
        bytes[selector_offset] = 0;
    });

    assert_mutation_rejected(&encoded, "unknown backend", |bytes| {
        bytes[local_offset] = 99;
    });
    assert_mutation_rejected(&encoded, "invalid RX presence boolean", |bytes| {
        bytes[local_offset + 1] = 2;
    });
    assert_mutation_rejected(&encoded, "unknown retry tag", |bytes| {
        bytes[local_offset + 3] = 99;
    });
    assert_mutation_rejected(&encoded, "absent RX cursor payload", |bytes| {
        write_u16(bytes, local_offset + 4, 1);
    });
    assert_mutation_rejected(&encoded, "none retry with duration", |bytes| {
        write_u64(bytes, local_offset + 12, 1);
    });

    assert_mutation_rejected(&encoded, "available feature mismatch", |bytes| {
        bytes[common_offset] ^= 1;
    });
    assert_mutation_rejected(&encoded, "driver features before DRIVER", |bytes| {
        write_u64(bytes, common_offset + 8, 1);
    });
    assert_mutation_rejected(&encoded, "failed device status", |bytes| {
        write_u32(bytes, common_offset + 20, VIRTIO_DEVICE_STATUS_FAILED);
    });
    assert_mutation_rejected(&encoded, "invalid activated boolean", |bytes| {
        bytes[common_offset + 24] = 2;
    });
    assert_mutation_rejected(&encoded, "wrong queue count", |bytes| {
        write_u16(bytes, common_offset + 26, 1);
    });
    assert_mutation_rejected(&encoded, "excess notification count", |bytes| {
        write_u16(bytes, common_offset + 28, 3);
    });
    assert_mutation_rejected(&encoded, "excess interrupt count", |bytes| {
        write_u16(bytes, common_offset + 30, 4);
    });
    assert_mutation_rejected(&encoded, "wrong queue maximum", |bytes| {
        write_u16(bytes, common_offset + 32, VIRTIO_NET_QUEUE_SIZE - 1);
    });
    assert_mutation_rejected(&encoded, "invalid queue ready boolean", |bytes| {
        bytes[common_offset + 36] = 2;
    });
    assert_mutation_rejected(
        &encoded,
        "noncanonical configuration intent index",
        |bytes| {
            write_u16(bytes, common_offset + 98, 1);
        },
    );

    assert_mutation_rejected(&encoded, "MMIO device selector", |bytes| {
        write_u32(bytes, transport_offset, 2);
    });
    assert_mutation_rejected(&encoded, "MMIO driver selector", |bytes| {
        write_u32(bytes, transport_offset + 4, 2);
    });
    assert_mutation_rejected(&encoded, "MMIO queue selector", |bytes| {
        write_u32(bytes, transport_offset + 8, 2);
    });
    assert_mutation_rejected(&encoded, "MMIO interrupt line", |bytes| {
        write_u32(bytes, transport_offset + 12, 31);
    });
    assert_mutation_rejected(&encoded, "MMIO region identity", |bytes| {
        write_u64(bytes, transport_offset + 16, 0);
    });
    assert_mutation_rejected(&encoded, "MMIO region alignment", |bytes| {
        write_u64(bytes, transport_offset + 24, 1);
    });
    assert_mutation_rejected(&encoded, "MMIO region size", |bytes| {
        write_u64(bytes, transport_offset + 32, 1);
    });

    let active = active_pci_mmds_state(1, false)
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("active PCI/MMDS mutation fixture should encode");
    let active_record = read_u64(&active, 64 + 8);
    let active_transport_directory = active_record
        + NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
        + 4 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let active_transport = active_record + read_u64(&active, active_transport_directory + 8);
    let mmds_offset = read_u64(&active, 48);

    for (name, offset, value) in [
        ("PCI phase", active_transport, 99),
        ("PCI origin", active_transport + 1, 99),
        ("PCI BAR index", active_transport + 2, 1),
        ("PCI address space", active_transport + 3, 99),
        ("PCI prefetch policy", active_transport + 4, 1),
        ("PCI function", active_transport + 6, 1),
    ] {
        assert_mutation_rejected(&active, name, |bytes| bytes[offset] = value);
    }
    for (name, offset, value) in [
        ("PCI writable count", active_transport + 42, 3),
        ("PCI probe count", active_transport + 44, 1),
        ("PCI MSI-X entry count", active_transport + 46, 2),
        ("PCI pending-word count", active_transport + 48, 2),
        ("PCI queue-vector count", active_transport + 50, 1),
    ] {
        assert_mutation_rejected(&active, name, |bytes| {
            write_u16(bytes, offset, value);
        });
    }
    assert_mutation_rejected(&active, "PCI MSI-X enabled boolean", |bytes| {
        bytes[active_transport + 64] = 2;
    });
    assert_mutation_rejected(&active, "PCI writable offset ordering", |bytes| {
        write_u16(bytes, active_transport + 72, 0);
    });
    assert_mutation_rejected(&active, "PCI probe ordering", |bytes| {
        bytes[active_transport + 88] = 2;
    });
    assert_mutation_rejected(&active, "PCI MSI-X vector control", |bytes| {
        write_u32(bytes, active_transport + 108, 2);
    });
    assert_mutation_rejected(&active, "PCI pending vector mask", |bytes| {
        write_u64(bytes, active_transport + 144, 0b1000);
    });
    assert_mutation_rejected(&active, "PCI queue vector", |bytes| {
        write_u16(bytes, active_transport + 152, 3);
    });

    assert_mutation_rejected(&active, "MMDS magic", |bytes| {
        bytes[mmds_offset] ^= 1;
    });
    assert_mutation_rejected(&active, "MMDS version", |bytes| {
        bytes[mmds_offset + 8] = 99;
    });
    assert_mutation_rejected(&active, "MMDS compatibility boolean", |bytes| {
        bytes[mmds_offset + 9] = 2;
    });
    assert_mutation_rejected(&active, "MMDS address presence boolean", |bytes| {
        bytes[mmds_offset + 10] = 2;
    });
    assert_mutation_rejected(&active, "MMDS empty interface set", |bytes| {
        bytes[mmds_offset + 11] = 0;
    });
    assert_mutation_rejected(&active, "MMDS section length", |bytes| {
        write_u64(bytes, mmds_offset + 16, 0);
    });
    assert_mutation_rejected(&active, "MMDS interface index", |bytes| {
        write_u16(bytes, mmds_offset + 32, 1);
    });
    assert_mutation_rejected(&active, "MMDS stack MAC", |bytes| {
        bytes[mmds_offset + 34] ^= 1;
    });
    assert_mutation_rejected(&active, "MMDS stack IPv4", |bytes| {
        bytes[mmds_offset + 40] ^= 1;
    });
    assert_mutation_rejected(&active, "MMDS stack port", |bytes| {
        write_u16(bytes, mmds_offset + 44, MMDS_GUEST_TCP_PORT + 1);
    });
}

#[test]
fn decoder_rejects_every_truncation_and_framing_reserved_byte_mutation() {
    let encoded = inactive_mmio_state()
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for length in 0..encoded.len() {
        assert!(
            SnapshotV2NetworkState::decode(
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                encoded
                    .get(..length)
                    .expect("prefix length should stay in bounds"),
            )
            .is_err(),
            "truncation at {length} bytes should fail"
        );
    }

    let record_offset = read_u64(&encoded, 64 + 8);
    let framing_offsets = (0..NATIVE_V2_NETWORK_STATE_HEADER_BYTES)
        .chain(64..64 + NATIVE_V2_NETWORK_INTERFACE_DIRECTORY_ENTRY_BYTES)
        .chain(
            record_offset
                ..record_offset
                    + NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
                    + NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT
                        * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES,
        );
    for offset in framing_offsets {
        let mut hostile = encoded.clone();
        hostile[offset] ^= 1;
        assert!(
            SnapshotV2NetworkState::decode(
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                &hostile,
            )
            .is_err(),
            "framing mutation at {offset} should fail"
        );
    }

    let section_directory = record_offset + NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES;
    let identity_offset = record_offset + read_u64(&encoded, section_directory + 8);
    let identity_length = read_u64(&encoded, section_directory + 16);
    let id_length = usize::from(u16::from_le_bytes(
        encoded[identity_offset..identity_offset + 2]
            .try_into()
            .expect("identity ID length should exist"),
    ));
    let selector_length = usize::from(u16::from_le_bytes(
        encoded[identity_offset + 2..identity_offset + 4]
            .try_into()
            .expect("identity selector length should exist"),
    ));
    let identity_semantic_end = identity_offset + 32 + id_length + selector_length;
    let local_directory = section_directory + NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let local_offset = record_offset + read_u64(&encoded, local_directory + 8);
    let common_directory = section_directory + 2 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let common_offset = record_offset + read_u64(&encoded, common_directory + 8);
    let common_length = read_u64(&encoded, common_directory + 16);
    let limiter_directory = section_directory + 3 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let limiter_offset = record_offset + read_u64(&encoded, limiter_directory + 8);
    let transport_directory =
        section_directory + 4 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let transport_offset = record_offset + read_u64(&encoded, transport_directory + 8);
    let semantic_reserved_offsets = (identity_semantic_end..identity_offset + identity_length)
        .chain(local_offset + 20..local_offset + NATIVE_V2_NETWORK_LOCAL_STATE_BYTES)
        .chain(std::iter::once(common_offset + 25))
        .chain(common_offset + 32 + 5..common_offset + 32 + 8)
        .chain(common_offset + 64 + 5..common_offset + 64 + 8)
        .chain(std::iter::once(common_offset + 97))
        .chain(common_offset + 100..common_offset + common_length)
        .chain(limiter_offset..limiter_offset + NATIVE_V2_NETWORK_LIMITER_STATE_BYTES)
        .chain(transport_offset + 40..transport_offset + 48);
    for offset in semantic_reserved_offsets {
        let mut hostile = encoded.clone();
        hostile[offset] = 1;
        assert!(
            SnapshotV2NetworkState::decode(
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                &hostile,
            )
            .is_err(),
            "reserved or padding mutation at {offset} should fail"
        );
    }
}

#[test]
fn decoder_rejects_pci_and_mmds_reserved_shape_mutations() {
    let encoded = active_pci_mmds_state(1, false)
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("PCI/MMDS fixture should encode");
    let record_offset = read_u64(&encoded, 64 + 8);
    let transport_directory = record_offset
        + NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
        + 4 * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;
    let transport_offset = record_offset + read_u64(&encoded, transport_directory + 8);
    let mmds_offset = read_u64(&encoded, 48);
    let reserved_offsets = [
        transport_offset + 7,
        transport_offset + 12,
        transport_offset + 13,
        transport_offset + 14,
        transport_offset + 15,
        transport_offset + 52,
        transport_offset + 53,
        transport_offset + 54,
        transport_offset + 55,
        transport_offset + 67,
        transport_offset + 70,
        transport_offset + 71,
        transport_offset + 75,
        transport_offset + 79,
        transport_offset + 83,
        transport_offset + 87,
        transport_offset + 90,
        transport_offset + 91,
        transport_offset + 94,
        transport_offset + 95,
        transport_offset + 156,
        transport_offset + 157,
        transport_offset + 158,
        transport_offset + 159,
        mmds_offset + 24,
        mmds_offset + 25,
        mmds_offset + 26,
        mmds_offset + 27,
        mmds_offset + 28,
        mmds_offset + 29,
        mmds_offset + 30,
        mmds_offset + 31,
        mmds_offset + 46,
        mmds_offset + 47,
    ];
    for offset in reserved_offsets {
        let mut hostile = encoded.clone();
        hostile[offset] = 1;
        assert!(
            SnapshotV2NetworkState::decode(
                NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
                &hostile,
            )
            .is_err(),
            "PCI/MMDS reserved mutation at {offset} should fail"
        );
    }
}

#[test]
fn delayed_zero_retry_and_noncanonical_mmds_indices_are_rejected() {
    let active = active_pci_mmds_state(2, false)
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("active state should encode");
    let record_offset = read_u64(&active, 64 + 8);
    let local_directory = record_offset + 64 + 32;
    let local_offset = record_offset + read_u64(&active, local_directory + 8);
    let mut delayed_zero = active.clone();
    delayed_zero[local_offset + 3] = 2;
    delayed_zero[local_offset + 12..local_offset + 20].fill(0);
    assert!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &delayed_zero,
        )
        .is_err()
    );

    let mmds_offset = read_u64(&active, 48);
    let mut duplicate_index = active;
    duplicate_index[mmds_offset + 32 + 16..mmds_offset + 32 + 18]
        .copy_from_slice(&0_u16.to_le_bytes());
    assert!(
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &duplicate_index,
        )
        .is_err()
    );
}

#[derive(Debug)]
struct FailAtReserve {
    remaining: usize,
}

impl ReservePolicy for FailAtReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        if self.remaining == 0 {
            return Err(());
        }
        self.remaining -= 1;
        values.try_reserve_exact(additional).map_err(|_| ())
    }

    fn reserve_string(&mut self, value: &mut String, additional: usize) -> Result<(), ()> {
        if self.remaining == 0 {
            return Err(());
        }
        self.remaining -= 1;
        value.try_reserve_exact(additional).map_err(|_| ())
    }
}

#[test]
fn allocation_failures_are_deterministic_and_escape_no_partial_state() {
    let state = active_pci_mmds_state(1, false);
    assert!(matches!(
        encode_with_policy(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &state,
            &mut FailAtReserve { remaining: 0 },
        ),
        Err(SnapshotV2NetworkStateEncodeError::Allocation)
    ));
    let encoded = state
        .encode(NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION)
        .expect("allocation fixture should encode");

    let mut first_success = None;
    for remaining in 0..32 {
        match decode_with_policy(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &encoded,
            &mut FailAtReserve { remaining },
        ) {
            Err(SnapshotV2NetworkStateDecodeError::Allocation) => {}
            Ok(decoded) => {
                assert_eq!(decoded, state);
                first_success = Some(remaining);
                break;
            }
            Err(other) => panic!("unexpected decode failure: {other}"),
        }
    }
    assert!(
        first_success.is_some_and(|count| count >= 8),
        "decode should exercise every bounded reservation"
    );
}

#[test]
fn debug_output_redacts_interface_selector_and_state_values() {
    let state = inactive_mmio_state();
    let debug = format!("{state:?}");
    let interface_debug = format!("{:?}", &state.interfaces()[0]);

    assert!(!debug.contains("eth0"));
    assert!(!debug.contains("vmnet:host"));
    assert!(!interface_debug.contains("eth0"));
    assert!(!interface_debug.contains("vmnet:host"));
    assert!(debug.contains("<redacted>"));
}
