use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_START, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO, PCI_SEGMENT_ZERO,
    PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState,
    SnapshotV2PciBarProbeState, SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixState, SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry,
    SnapshotV2PciWritableByte, SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
    SnapshotV2VirtioStateParts,
};
use crate::snapshot_restore::NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK,
    VIRTIO_DEVICE_STATUS_FEATURES_OK,
};
use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE};
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciEndpointPhase,
};
use crate::vsock::{
    MIN_GUEST_CID, VIRTIO_VSOCK_QUEUE_COUNT, VIRTIO_VSOCK_QUEUE_SIZE, VirtioVsockConfigSpace,
    VsockBackendSelector, VsockHostLocalPortCursor,
};

use super::codec::{ReservePolicy, decode_with_policy, encode_with_policy};
use super::*;

const LOCAL_OFFSET: usize = 160;
const COMMON_DIRECTORY_OFFSET: usize = 96;

fn inactive_mmio_state() -> SnapshotV2VsockState {
    let selector = VsockBackendSelector::try_from_path("/tmp/bangbang-vsock-inactive.sock")
        .expect("test selector should validate");
    let queues = (0..VIRTIO_VSOCK_QUEUE_COUNT)
        .map(|_| queue(0, false, 0))
        .collect();
    let virtio = virtio_state(
        0,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        false,
        queues,
        Vec::new(),
        Vec::new(),
    );
    let region = MmioRegion::new(
        MmioRegionId::new(1),
        GuestAddress::new(0x4000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("test MMIO region should validate");
    let interrupt = GuestInterruptLine::new(32).expect("SPI should validate");
    SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
        guest_cid: u64::from(MIN_GUEST_CID),
        backend_selector: selector,
        host_local_port_cursor: VsockHostLocalPortCursor::initial(),
        active_queues: None,
        virtio,
        transport: SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
            0, 0, 0, region, interrupt,
        )),
    })
    .expect("inactive MMIO state should validate")
}

fn active_pci_state() -> SnapshotV2VsockState {
    let selector_text = format!("/{}", "v".repeat(NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES - 1));
    let selector = VsockBackendSelector::try_from_path(&selector_text)
        .expect("maximum selector should validate");
    let available = VirtioVsockConfigSpace::new(u64::from(MIN_GUEST_CID)).available_features();
    let queues = vec![
        queue(0x0010_0000, true, VIRTIO_VSOCK_QUEUE_SIZE),
        queue(0x0020_0000, true, VIRTIO_VSOCK_QUEUE_SIZE),
        queue(0x0030_0000, true, VIRTIO_VSOCK_QUEUE_SIZE),
    ];
    let status = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK
        | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    let virtio = virtio_state(
        available,
        status,
        true,
        queues,
        vec![0, 1, 2],
        vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Queue { queue_index: 1 },
            SnapshotV2InterruptIntent::Queue { queue_index: 2 },
            SnapshotV2InterruptIntent::Configuration,
        ],
    );
    let active_queue = |cursor| SnapshotV2VsockQueueState::new(cursor, cursor, true);
    let active_queues =
        SnapshotV2VsockActiveQueuesState::new(active_queue(7), active_queue(11), active_queue(13));
    let sbdf = PciSbdf::new(
        PCI_SEGMENT_ZERO,
        0,
        PCI_FIRST_ENDPOINT_DEVICE,
        PCI_FUNCTION_ZERO,
    )
    .expect("test SBDF should validate");
    let bar_range = GuestMemoryRange::new(
        GuestAddress::new(PCI_BAR64_START),
        VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    )
    .expect("test BAR should validate");
    let writable_bytes = [0x04, 0x05, 0x0c, 0x3c]
        .into_iter()
        .map(|offset| SnapshotV2PciWritableByte::from_parts(offset, 0))
        .collect();
    let bar_probes = vec![
        SnapshotV2PciBarProbeState::from_parts(0, false),
        SnapshotV2PciBarProbeState::from_parts(1, true),
    ];
    let entries = (0..4)
        .map(|index| {
            SnapshotV2PciMsixTableEntry::from_parts(
                0xfee0_0000 + index * 0x10,
                0,
                0x40 + index,
                index & 1,
            )
        })
        .collect();
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries,
        pending_words: vec![0b1010],
        enabled: true,
        function_masked: false,
        config_vector: 0,
        queue_vectors: vec![1, 2, 3],
        pending_transition_observed: true,
    });
    let pci = SnapshotV2PciDeviceState::from_parts(SnapshotV2PciDeviceStateParts {
        phase: VirtioPciEndpointPhase::Active,
        origin: StorageDeviceOrigin::Startup,
        sbdf,
        bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        bar_address_space: PciBarAddressSpace::Memory64,
        bar_prefetchable: PciBarPrefetchable::No,
        bar_range,
        device_feature_select: 1,
        driver_feature_select: 1,
        queue_select: 2,
        pci_cfg_bar: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        pci_cfg_offset: 0x20,
        pci_cfg_length: 4,
        writable_bytes,
        bar_probes,
        msix,
    });
    SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
        guest_cid: u64::from(MIN_GUEST_CID),
        backend_selector: selector,
        host_local_port_cursor: VsockHostLocalPortCursor::try_from_last_used(1 << 30)
            .expect("test cursor should validate"),
        active_queues: Some(active_queues),
        virtio,
        transport: SnapshotV2DeviceTransport::Pci(pci),
    })
    .expect("active PCI state should validate")
}

fn queue(base: u64, ready: bool, size: u16) -> SnapshotV2VirtioQueueState {
    SnapshotV2VirtioQueueState::from_parts(
        VIRTIO_VSOCK_QUEUE_SIZE,
        size,
        ready,
        GuestAddress::new(base),
        GuestAddress::new(base + if base == 0 { 0 } else { 0x2000 }),
        GuestAddress::new(base + if base == 0 { 0 } else { 0x3000 }),
    )
}

fn virtio_state(
    driver_features: u64,
    status: u32,
    activated: bool,
    queues: Vec<SnapshotV2VirtioQueueState>,
    pending_notifications: Vec<u16>,
    interrupt_intents: Vec<SnapshotV2InterruptIntent>,
) -> SnapshotV2VirtioState {
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: VirtioVsockConfigSpace::new(u64::from(MIN_GUEST_CID))
            .available_features(),
        driver_features,
        config_generation: 0,
        status,
        activated,
        queues,
        pending_notifications,
        interrupt_intents,
    })
}

#[test]
fn deterministic_fixture_values_round_trip_and_report_stable_identity() {
    for state in [inactive_mmio_state(), active_pci_state()] {
        let encoded = state
            .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
            .expect("fixture should encode");
        let decoded =
            SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &encoded)
                .expect("fixture should decode");
        assert_eq!(decoded, state);
        assert_eq!(
            decoded
                .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
                .expect("fixture should re-encode"),
            encoded
        );
        assert_eq!(
            decoded.compatibility_version(),
            NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION
        );
        assert_eq!(decoded.device_key().kind(), 5);
        assert_eq!(decoded.device_key().instance(), 0);
        assert_eq!(NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID, "vsock0");
    }
    assert_eq!(
        active_pci_state()
            .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
            .expect("maximum fixture should encode")
            .len(),
        NATIVE_V2_VSOCK_STATE_WORST_CASE_BYTES
    );
}

#[test]
fn immutable_wire_fixtures_lock_inactive_mmio_and_active_pci_profiles() {
    for (state, fixture) in [
        (
            inactive_mmio_state(),
            fixture_bytes(include_str!("fixtures/inactive-mmio.hex")),
        ),
        (
            active_pci_state(),
            fixture_bytes(include_str!("fixtures/active-pci.hex")),
        ),
    ] {
        let encoded = state
            .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
            .expect("fixture should encode");
        assert_eq!(encoded, fixture);
        let decoded =
            SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &fixture)
                .expect("fixture should decode");
        assert_eq!(decoded, state);
        assert_eq!(
            decoded
                .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
                .expect("fixture should re-encode"),
            fixture
        );
    }
}

#[test]
fn public_debug_and_error_text_redact_private_values() {
    let state = active_pci_state();
    let selector = state
        .backend_selector()
        .path()
        .to_str()
        .expect("selector should be UTF-8");
    let debug = format!("{state:?}");
    assert_eq!(debug, "SnapshotV2VsockState { state: \"<redacted>\" }");
    assert!(!debug.contains(selector));
    assert_eq!(
        format!("{:?}", state.active_queues().expect("active queues")),
        "SnapshotV2VsockActiveQueuesState { state: \"<redacted>\" }"
    );
    assert_eq!(
        format!("{:?}", state.device_key()),
        "SnapshotV2DeviceKey { state: \"<redacted>\" }"
    );

    let error = SnapshotV2VsockStateDecodeError::InvalidState {
        source: SnapshotV2VsockStateBuildError::BackendSelector,
    };
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(selector));
    assert!(!rendered.contains(&state.guest_cid().to_string()));
}

#[test]
fn exact_version_is_required_for_component_codec() {
    let state = inactive_mmio_state();
    assert!(matches!(
        state.encode(crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_VERSION),
        Err(SnapshotV2VsockStateEncodeError::UnsupportedVersion)
    ));
    let encoded = state
        .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    assert!(matches!(
        SnapshotV2VsockState::decode(
            crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_VERSION,
            &encoded,
        ),
        Err(SnapshotV2VsockStateDecodeError::UnsupportedVersion)
    ));
}

#[test]
fn header_and_directory_mutations_fail_closed() {
    let encoded = active_pci_state()
        .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    for offset in [
        0_usize, 8, 10, 12, 14, 16, 20, 24, 32, 40, 48, 64, 66, 68, 72, 80, 88, 96, 98, 100, 104,
        112, 120, 128, 130, 132, 136, 144, 152,
    ] {
        let mut invalid = encoded.clone();
        invalid[offset] ^= 0x80;
        assert!(
            SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &invalid,)
                .is_err(),
            "offset {offset} should reject"
        );
    }

    for length in 0..encoded.len() {
        assert!(
            SnapshotV2VsockState::decode(
                NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
                &encoded[..length],
            )
            .is_err(),
            "truncation {length} should reject"
        );
    }
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(matches!(
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &trailing,),
        Err(SnapshotV2VsockStateDecodeError::TooLarge)
    ));
}

#[test]
fn local_selector_cursor_and_padding_mutations_fail_closed() {
    let encoded = active_pci_state()
        .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    let selector_length_offset = LOCAL_OFFSET + 12;
    let selector_offset = LOCAL_OFFSET + NATIVE_V2_VSOCK_LOCAL_PREFIX_BYTES;

    let mut empty = encoded.clone();
    replace_u16(&mut empty, selector_length_offset, 0);
    assert!(
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &empty,).is_err()
    );

    let mut too_long = encoded.clone();
    replace_u16(
        &mut too_long,
        selector_length_offset,
        (NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES + 1) as u16,
    );
    too_long[selector_offset + NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES] = b'v';
    assert!(
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &too_long,)
            .is_err()
    );

    let mut invalid_utf8 = encoded.clone();
    invalid_utf8[selector_offset] = 0xff;
    assert!(matches!(
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &invalid_utf8,),
        Err(SnapshotV2VsockStateDecodeError::InvalidUtf8)
    ));

    let mut control = encoded.clone();
    control[selector_offset] = b'\n';
    assert!(matches!(
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &control,),
        Err(SnapshotV2VsockStateDecodeError::InvalidValue)
    ));

    let mut cursor = encoded.clone();
    cursor[LOCAL_OFFSET + 8..LOCAL_OFFSET + 12].copy_from_slice(&0_u32.to_le_bytes());
    assert!(
        SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &cursor,)
            .is_err()
    );

    for offset in [
        LOCAL_OFFSET + 17,
        LOCAL_OFFSET + 32,
        selector_offset + NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES,
    ] {
        let mut invalid = encoded.clone();
        invalid[offset] = 1;
        assert!(
            SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &invalid,)
                .is_err()
        );
    }

    let inactive = inactive_mmio_state()
        .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
        .expect("inactive state should encode");
    for offset in [LOCAL_OFFSET + 16, LOCAL_OFFSET + 20, LOCAL_OFFSET + 22] {
        let mut invalid = inactive.clone();
        invalid[offset] = 1;
        assert!(
            SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &invalid,)
                .is_err()
        );
    }
}

#[test]
fn semantic_builder_rejects_cid_features_status_queues_and_placement() {
    let state = inactive_mmio_state();
    let SnapshotV2VsockStateParts {
        backend_selector,
        host_local_port_cursor,
        virtio,
        transport,
        ..
    } = state.into_parts();
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid: u64::from(MIN_GUEST_CID - 1),
            backend_selector: backend_selector.clone(),
            host_local_port_cursor,
            active_queues: None,
            virtio: virtio.clone(),
            transport: transport.clone(),
        }),
        Err(SnapshotV2VsockStateBuildError::GuestCid)
    ));

    let bad_features = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: 0,
        driver_features: 0,
        config_generation: 0,
        status: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        activated: false,
        queues: vec![queue(0, false, 0); VIRTIO_VSOCK_QUEUE_COUNT],
        pending_notifications: Vec::new(),
        interrupt_intents: Vec::new(),
    });
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid: u64::from(MIN_GUEST_CID),
            backend_selector: backend_selector.clone(),
            host_local_port_cursor,
            active_queues: None,
            virtio: bad_features,
            transport: transport.clone(),
        }),
        Err(SnapshotV2VsockStateBuildError::Virtio)
    ));

    let before_features_queue = virtio_state(
        0,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        false,
        vec![queue(0x10000, true, 256); VIRTIO_VSOCK_QUEUE_COUNT],
        Vec::new(),
        Vec::new(),
    );
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid: u64::from(MIN_GUEST_CID),
            backend_selector: backend_selector.clone(),
            host_local_port_cursor,
            active_queues: None,
            virtio: before_features_queue,
            transport: transport.clone(),
        }),
        Err(SnapshotV2VsockStateBuildError::Queue)
    ));

    let version_one_only = VIRTIO_MMIO_VERSION_1_FEATURE;
    let active_status = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK
        | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    let overlapping = virtio_state(
        version_one_only,
        active_status,
        true,
        vec![queue(0x10000, true, 256); VIRTIO_VSOCK_QUEUE_COUNT],
        Vec::new(),
        Vec::new(),
    );
    let cursors = SnapshotV2VsockActiveQueuesState::new(
        SnapshotV2VsockQueueState::new(0, 0, false),
        SnapshotV2VsockQueueState::new(0, 0, false),
        SnapshotV2VsockQueueState::new(0, 0, false),
    );
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid: u64::from(MIN_GUEST_CID),
            backend_selector,
            host_local_port_cursor,
            active_queues: Some(cursors),
            virtio: overlapping,
            transport,
        }),
        Err(SnapshotV2VsockStateBuildError::Queue)
    ));

    let too_large_cid = cloned_parts(&inactive_mmio_state());
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
            guest_cid: u64::from(u32::MAX) + 1,
            ..too_large_cid
        }),
        Err(SnapshotV2VsockStateBuildError::GuestCid)
    ));
}

#[test]
fn virtio_relationship_matrix_rejects_status_activation_cursors_and_intents() {
    let active = active_pci_state();
    let common = active.virtio();
    let make_common = |available_features,
                       driver_features,
                       config_generation,
                       status,
                       activated,
                       queues: Vec<_>,
                       notifications: Vec<_>,
                       intents: Vec<_>| {
        SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features,
            driver_features,
            config_generation,
            status,
            activated,
            queues,
            pending_notifications: notifications,
            interrupt_intents: intents,
        })
    };
    let expected = common.available_features();

    let mut unsupported_features = cloned_parts(&active);
    unsupported_features.virtio = make_common(
        expected,
        expected | (1_u64 << 63),
        0,
        common.status(),
        true,
        common.queues().to_vec(),
        common.pending_notifications().to_vec(),
        common.interrupt_intents().to_vec(),
    );
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(unsupported_features),
        Err(SnapshotV2VsockStateBuildError::Virtio)
    ));

    let mut bad_status = cloned_parts(&active);
    bad_status.virtio = make_common(
        expected,
        expected,
        0,
        common.status() | crate::virtio::VIRTIO_DEVICE_STATUS_FAILED,
        true,
        common.queues().to_vec(),
        common.pending_notifications().to_vec(),
        common.interrupt_intents().to_vec(),
    );
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(bad_status),
        Err(SnapshotV2VsockStateBuildError::Virtio)
    ));

    let mut bad_generation = cloned_parts(&active);
    bad_generation.virtio = make_common(
        expected,
        expected,
        1,
        common.status(),
        true,
        common.queues().to_vec(),
        common.pending_notifications().to_vec(),
        common.interrupt_intents().to_vec(),
    );
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(bad_generation),
        Err(SnapshotV2VsockStateBuildError::Virtio)
    ));

    let mut missing_active_cursors = cloned_parts(&active);
    missing_active_cursors.active_queues = None;
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(missing_active_cursors),
        Err(SnapshotV2VsockStateBuildError::Virtio)
    ));

    let mut unpublished = cloned_parts(&active);
    unpublished.active_queues = Some(SnapshotV2VsockActiveQueuesState::new(
        SnapshotV2VsockQueueState::new(8, 7, true),
        SnapshotV2VsockQueueState::new(11, 11, true),
        SnapshotV2VsockQueueState::new(13, 13, true),
    ));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(unpublished),
        Err(SnapshotV2VsockStateBuildError::Queue)
    ));

    let mut event_idx_mismatch = cloned_parts(&active);
    event_idx_mismatch.active_queues = Some(SnapshotV2VsockActiveQueuesState::new(
        SnapshotV2VsockQueueState::new(7, 7, false),
        SnapshotV2VsockQueueState::new(11, 11, true),
        SnapshotV2VsockQueueState::new(13, 13, true),
    ));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(event_idx_mismatch),
        Err(SnapshotV2VsockStateBuildError::Queue)
    ));

    for (notifications, intents) in [
        (vec![1, 0], common.interrupt_intents().to_vec()),
        (vec![0, 0], common.interrupt_intents().to_vec()),
        (vec![3], common.interrupt_intents().to_vec()),
        (
            common.pending_notifications().to_vec(),
            vec![
                SnapshotV2InterruptIntent::Configuration,
                SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            ],
        ),
        (
            common.pending_notifications().to_vec(),
            vec![SnapshotV2InterruptIntent::Queue { queue_index: 3 }],
        ),
    ] {
        let mut invalid = cloned_parts(&active);
        invalid.virtio = make_common(
            expected,
            expected,
            0,
            common.status(),
            true,
            common.queues().to_vec(),
            notifications,
            intents,
        );
        assert!(matches!(
            SnapshotV2VsockState::try_from_parts(invalid),
            Err(SnapshotV2VsockStateBuildError::Virtio)
        ));
    }
}

#[test]
fn queue_geometry_and_transport_placement_matrix_fail_closed() {
    let inactive = inactive_mmio_state();
    let expected = inactive.virtio().available_features();
    let status = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    let valid_queues = vec![
        queue(0x0010_0000, false, 256),
        queue(0x0020_0000, false, 256),
        queue(0x0030_0000, false, 256),
    ];
    let common_with_queues = |queues| {
        SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features: expected,
            driver_features: VIRTIO_MMIO_VERSION_1_FEATURE,
            config_generation: 0,
            status,
            activated: false,
            queues,
            pending_notifications: Vec::new(),
            interrupt_intents: Vec::new(),
        })
    };

    let bad_queues = [
        vec![
            raw_queue(255, 0, false, 0, 0, 0),
            valid_queues[1],
            valid_queues[2],
        ],
        vec![
            raw_queue(256, 3, false, 0x100000, 0x102000, 0x103000),
            valid_queues[1],
            valid_queues[2],
        ],
        vec![
            raw_queue(256, 0, true, 0, 0, 0),
            valid_queues[1],
            valid_queues[2],
        ],
        vec![
            raw_queue(256, 256, false, 0x100001, 0x102000, 0x103000),
            valid_queues[1],
            valid_queues[2],
        ],
        vec![
            raw_queue(256, 256, false, 0x100000, 0x100000, 0x100000),
            valid_queues[1],
            valid_queues[2],
        ],
        vec![valid_queues[0], valid_queues[1]],
    ];
    for queues in bad_queues {
        let mut invalid = cloned_parts(&inactive);
        invalid.virtio = common_with_queues(queues);
        assert!(
            matches!(
                SnapshotV2VsockState::try_from_parts(invalid),
                Err(SnapshotV2VsockStateBuildError::Queue)
                    | Err(SnapshotV2VsockStateBuildError::Virtio)
            ),
            "invalid queue geometry should reject"
        );
    }

    let region = MmioRegion::new(
        MmioRegionId::new(2),
        GuestAddress::new(0x0010_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("overlapping region should be structurally valid");
    let mut overlap = cloned_parts(&inactive);
    overlap.virtio = common_with_queues(valid_queues.clone());
    overlap.transport = SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
        0,
        0,
        0,
        region,
        GuestInterruptLine::new(32).expect("SPI should validate"),
    ));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(overlap),
        Err(SnapshotV2VsockStateBuildError::Placement)
    ));

    let mut bad_select = cloned_parts(&inactive);
    let SnapshotV2DeviceTransport::Mmio(mmio) = &bad_select.transport else {
        panic!("fixture should use MMIO");
    };
    bad_select.transport = SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
        2,
        0,
        3,
        mmio.region(),
        mmio.interrupt_line(),
    ));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(bad_select),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));

    let mut bad_interrupt = cloned_parts(&inactive);
    let SnapshotV2DeviceTransport::Mmio(mmio) = &bad_interrupt.transport else {
        panic!("fixture should use MMIO");
    };
    bad_interrupt.transport =
        SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
            0,
            0,
            0,
            mmio.region(),
            GuestInterruptLine::new(31).expect("line should be representable"),
        ));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(bad_interrupt),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));
}

#[test]
fn pci_topology_msix_and_control_matrix_fail_closed() {
    let active = active_pci_state();
    let SnapshotV2DeviceTransport::Pci(pci) = active.transport() else {
        panic!("fixture should use PCI");
    };

    let mut runtime_origin = cloned_parts(&active);
    let mut parts = cloned_pci_parts(pci);
    parts.origin = StorageDeviceOrigin::Runtime;
    runtime_origin.transport =
        SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(parts));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(runtime_origin),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));

    let mut queue_select = cloned_parts(&active);
    let mut parts = cloned_pci_parts(pci);
    parts.queue_select = 3;
    queue_select.transport =
        SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(parts));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(queue_select),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));

    let mut writable_order = cloned_parts(&active);
    let mut parts = cloned_pci_parts(pci);
    parts.writable_bytes.swap(0, 1);
    writable_order.transport =
        SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(parts));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(writable_order),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));

    let mut pending_bits = cloned_parts(&active);
    let mut parts = cloned_pci_parts(pci);
    parts.msix = cloned_msix_with(pci.msix(), pci.msix().entries().to_vec(), vec![0b1_0000]);
    pending_bits.transport =
        SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(parts));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(pending_bits),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));

    let mut vector_control = cloned_parts(&active);
    let mut parts = cloned_pci_parts(pci);
    let mut entries = pci.msix().entries().to_vec();
    entries[0] = SnapshotV2PciMsixTableEntry::from_parts(0, 0, 0, 2);
    parts.msix = cloned_msix_with(pci.msix(), entries, pci.msix().pending_words().to_vec());
    vector_control.transport =
        SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(parts));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(vector_control),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));

    let mut entry_count = cloned_parts(&active);
    let mut parts = cloned_pci_parts(pci);
    parts.msix = cloned_msix_with(
        pci.msix(),
        pci.msix().entries()[..3].to_vec(),
        pci.msix().pending_words().to_vec(),
    );
    entry_count.transport =
        SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(parts));
    assert!(matches!(
        SnapshotV2VsockState::try_from_parts(entry_count),
        Err(SnapshotV2VsockStateBuildError::Transport)
    ));
}

#[test]
fn codec_reserve_failures_are_typed_and_transactional() {
    let state = active_pci_state();
    let mut encode_reserve = FailAtReserve::new(0);
    assert!(matches!(
        encode_with_policy(
            NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
            &state,
            &mut encode_reserve,
        ),
        Err(SnapshotV2VsockStateEncodeError::Allocation)
    ));

    let encoded = state
        .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    for fail_at in 0..9 {
        let mut reserve = FailAtReserve::new(fail_at);
        assert!(
            matches!(
                decode_with_policy(
                    NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
                    &encoded,
                    &mut reserve,
                ),
                Err(SnapshotV2VsockStateDecodeError::Allocation)
            ),
            "reserve call {fail_at} should fail"
        );
    }
    let mut reserve = FailAtReserve::new(9);
    decode_with_policy(
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
        &encoded,
        &mut reserve,
    )
    .expect("all nine decode reserve calls should complete");
}

#[test]
fn common_and_pci_count_fields_are_preflighted_before_reserve() {
    let encoded = active_pci_state()
        .encode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    let common_offset =
        usize::try_from(read_u64(&encoded, COMMON_DIRECTORY_OFFSET + 8)).expect("offset");
    for offset in [common_offset + 26, common_offset + 28, common_offset + 30] {
        let mut invalid = encoded.clone();
        replace_u16(&mut invalid, offset, u16::MAX);
        let mut reserve = PanicReserve;
        assert!(
            SnapshotV2VsockState::decode(NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION, &invalid,)
                .is_err()
        );
        assert!(
            decode_with_policy(
                NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
                &invalid,
                &mut reserve,
            )
            .is_err()
        );
    }

    let transport_offset = usize::try_from(read_u64(&encoded, 128 + 8)).expect("transport offset");
    for offset in [42_usize, 44, 46, 48, 50] {
        let mut invalid = encoded.clone();
        replace_u16(&mut invalid, transport_offset + offset, u16::MAX);
        let mut reserve = PanicReserve;
        assert!(
            decode_with_policy(
                NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
                &invalid,
                &mut reserve,
            )
            .is_err()
        );
    }
}

fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert!(hex.len().is_multiple_of(2));
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair).expect("fixture pair should be UTF-8");
        bytes.push(u8::from_str_radix(text, 16).expect("fixture pair should be hexadecimal"));
    }
    bytes
}

fn cloned_parts(state: &SnapshotV2VsockState) -> SnapshotV2VsockStateParts {
    SnapshotV2VsockStateParts {
        guest_cid: state.guest_cid(),
        backend_selector: state.backend_selector().clone(),
        host_local_port_cursor: state.host_local_port_cursor(),
        active_queues: state.active_queues(),
        virtio: state.virtio().clone(),
        transport: state.transport().clone(),
    }
}

fn raw_queue(
    max_size: u16,
    size: u16,
    ready: bool,
    descriptor_table: u64,
    driver_ring: u64,
    device_ring: u64,
) -> SnapshotV2VirtioQueueState {
    SnapshotV2VirtioQueueState::from_parts(
        max_size,
        size,
        ready,
        GuestAddress::new(descriptor_table),
        GuestAddress::new(driver_ring),
        GuestAddress::new(device_ring),
    )
}

fn cloned_pci_parts(state: &SnapshotV2PciDeviceState) -> SnapshotV2PciDeviceStateParts {
    SnapshotV2PciDeviceStateParts {
        phase: state.phase(),
        origin: state.origin(),
        sbdf: state.sbdf(),
        bar_index: state.bar_index(),
        bar_address_space: state.bar_address_space(),
        bar_prefetchable: state.bar_prefetchable(),
        bar_range: state.bar_range(),
        device_feature_select: state.device_feature_select(),
        driver_feature_select: state.driver_feature_select(),
        queue_select: state.queue_select(),
        pci_cfg_bar: state.pci_cfg_bar(),
        pci_cfg_offset: state.pci_cfg_offset(),
        pci_cfg_length: state.pci_cfg_length(),
        writable_bytes: state.writable_bytes().to_vec(),
        bar_probes: state.bar_probes().to_vec(),
        msix: cloned_msix_with(
            state.msix(),
            state.msix().entries().to_vec(),
            state.msix().pending_words().to_vec(),
        ),
    }
}

fn cloned_msix_with(
    state: &SnapshotV2PciMsixState,
    entries: Vec<SnapshotV2PciMsixTableEntry>,
    pending_words: Vec<u64>,
) -> SnapshotV2PciMsixState {
    SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries,
        pending_words,
        enabled: state.enabled(),
        function_masked: state.function_masked(),
        config_vector: state.config_vector(),
        queue_vectors: state.queue_vectors().to_vec(),
        pending_transition_observed: state.pending_transition_observed(),
    })
}

fn replace_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("test field should fit"),
    )
}

struct FailAtReserve {
    calls: usize,
    fail_at: usize,
}

impl FailAtReserve {
    const fn new(fail_at: usize) -> Self {
        Self { calls: 0, fail_at }
    }

    fn check(&mut self) -> Result<(), ()> {
        let call = self.calls;
        self.calls += 1;
        if call == self.fail_at {
            Err(())
        } else {
            Ok(())
        }
    }
}

impl ReservePolicy for FailAtReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        self.check()?;
        values.try_reserve_exact(additional).map_err(|_| ())
    }

    fn reserve_string(&mut self, value: &mut String, additional: usize) -> Result<(), ()> {
        self.check()?;
        value.try_reserve_exact(additional).map_err(|_| ())
    }
}

struct PanicReserve;

impl ReservePolicy for PanicReserve {
    fn reserve_vec<T>(&mut self, _values: &mut Vec<T>, _additional: usize) -> Result<(), ()> {
        panic!("preflight must reject before reserve")
    }

    fn reserve_string(&mut self, _value: &mut String, _additional: usize) -> Result<(), ()> {
        panic!("preflight must reject before reserve")
    }
}
