use crate::balloon::{BalloonConfigInput, VIRTIO_BALLOON_FREE_PAGE_HINT_STOP};
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_START, PCI_BUS_ZERO, PCI_FUNCTION_ZERO, PCI_SEGMENT_ZERO, PciBarAddressSpace,
    PciBarPrefetchable, PciSbdf,
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
    VIRTIO_DEVICE_STATUS_FEATURES_OK,
};
use crate::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciEndpointPhase,
};

use super::codec::{ReservePolicy, decode_with_policy, encode_with_policy};
use super::*;

const DIRECTORY_OFFSET: usize = 64;
const DIRECTORY_ENTRY_BYTES: usize = 32;
const DIRECTORY_PAYLOAD_OFFSET: usize = 8;
const DIRECTORY_LENGTH_OFFSET: usize = 16;
const PAYLOAD_OFFSET: usize = 192;

fn config(stats: bool, hinting: bool, reporting: bool) -> BalloonConfig {
    BalloonConfigInput::new(64, true)
        .with_stats_polling_interval_s(if stats { 5 } else { 0 })
        .with_free_page_hinting(hinting)
        .with_free_page_reporting(reporting)
        .validate()
        .expect("test balloon configuration should validate")
}

fn queue(index: usize, activated: bool) -> SnapshotV2VirtioQueueState {
    if !activated {
        return SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_BALLOON_QUEUE_SIZE,
            0,
            false,
            GuestAddress::new(0),
            GuestAddress::new(0),
            GuestAddress::new(0),
        );
    }
    let base = 0x8000_0000_u64 + index as u64 * 0x1_0000;
    SnapshotV2VirtioQueueState::from_parts(
        VIRTIO_BALLOON_QUEUE_SIZE,
        VIRTIO_BALLOON_QUEUE_SIZE,
        true,
        GuestAddress::new(base),
        GuestAddress::new(base + 0x2000),
        GuestAddress::new(base + 0x4000),
    )
}

fn active_queues(
    config: BalloonConfig,
    pending_statistics: bool,
) -> SnapshotV2BalloonActiveQueuesState {
    let layout = VirtioBalloonQueueLayout::from_config(config);
    let idle = SnapshotV2BalloonQueueState::try_new(7, 7, VIRTIO_BALLOON_QUEUE_SIZE)
        .expect("idle cursor should validate");
    let pending = SnapshotV2BalloonQueueState::try_new(8, 7, VIRTIO_BALLOON_QUEUE_SIZE)
        .expect("pending cursor should validate");
    SnapshotV2BalloonActiveQueuesState::try_new(
        config,
        idle,
        idle,
        layout
            .statistics()
            .map(|_| if pending_statistics { pending } else { idle }),
        layout.free_page_hinting().map(|_| idle),
        layout.free_page_reporting().map(|_| idle),
    )
    .expect("active cursor shape should validate")
}

fn common(config: BalloonConfig, activated: bool, pci: bool) -> SnapshotV2VirtioState {
    let queue_count = VirtioBalloonQueueLayout::from_config(config).queue_count();
    let queues = (0..queue_count)
        .map(|index| queue(index, activated))
        .collect();
    let pending_notifications = if activated {
        vec![
            0,
            u16::try_from(queue_count - 1).expect("queue count should fit"),
        ]
    } else {
        Vec::new()
    };
    let interrupt_intents = if activated && pci {
        vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Queue {
                queue_index: u16::try_from(queue_count - 1).expect("queue count should fit"),
            },
            SnapshotV2InterruptIntent::Configuration,
        ]
    } else if activated {
        vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ]
    } else {
        Vec::new()
    };
    let status = if activated {
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK
            | VIRTIO_DEVICE_STATUS_DRIVER_OK
    } else {
        0
    };
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: available_features(config),
        driver_features: if activated {
            available_features(config)
        } else {
            0
        },
        config_generation: if activated { 9 } else { 0 },
        status,
        activated,
        queues,
        pending_notifications,
        interrupt_intents,
    })
}

fn mmio_transport() -> SnapshotV2DeviceTransport {
    let region = MmioRegion::new(
        MmioRegionId::new(1),
        GuestAddress::new(0x1000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("test MMIO region should validate");
    SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
        0,
        0,
        0,
        region,
        GuestInterruptLine::new(32).expect("test interrupt should validate"),
    ))
}

fn pci_transport(queue_count: usize) -> SnapshotV2DeviceTransport {
    let entry_count = queue_count + 1;
    let entries = (0..entry_count)
        .map(|index| {
            SnapshotV2PciMsixTableEntry::from_parts(
                0xfee0_0000,
                0,
                u32::try_from(index).expect("entry index should fit"),
                u32::from(index == 0),
            )
        })
        .collect();
    let queue_vectors = (1..=queue_count)
        .map(|index| u16::try_from(index).expect("queue vector should fit"))
        .collect();
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries,
        pending_words: vec![0b10],
        enabled: true,
        function_masked: false,
        config_vector: 0,
        queue_vectors,
        pending_transition_observed: true,
    });
    let writable_bytes = [0x04, 0x05, 0x0c, 0x3c]
        .into_iter()
        .map(|offset| SnapshotV2PciWritableByte::from_parts(offset, 0))
        .collect();
    let bar_probes = [0, 1]
        .into_iter()
        .map(|index| SnapshotV2PciBarProbeState::from_parts(index, false))
        .collect();
    SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(
        SnapshotV2PciDeviceStateParts {
            phase: VirtioPciEndpointPhase::Active,
            origin: StorageDeviceOrigin::Startup,
            sbdf: PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, 1, PCI_FUNCTION_ZERO)
                .expect("test SBDF should validate"),
            bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
            bar_address_space: PciBarAddressSpace::Memory64,
            bar_prefetchable: PciBarPrefetchable::No,
            bar_range: GuestMemoryRange::new(
                GuestAddress::new(PCI_BAR64_START),
                VIRTIO_PCI_CAPABILITY_BAR_SIZE,
            )
            .expect("test BAR should validate"),
            device_feature_select: 0,
            driver_feature_select: 0,
            queue_select: u16::try_from(queue_count - 1).expect("queue selector should fit"),
            pci_cfg_bar: 0,
            pci_cfg_offset: 0,
            pci_cfg_length: 0,
            writable_bytes,
            bar_probes,
            msix,
        },
    ))
}

fn state(
    config: BalloonConfig,
    activated: bool,
    pci: bool,
    pending_statistics: bool,
) -> SnapshotV2BalloonState {
    let layout = VirtioBalloonQueueLayout::from_config(config);
    let config_space = VirtioBalloonConfigSpace::new(
        mib_to_4k_pages(config.amount_mib()).expect("test target should fit"),
        if activated { 17 } else { 0 },
        VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
    );
    let statistics = if activated && layout.statistics().is_some() {
        SnapshotV2BalloonStatistics::new([
            Some(1),
            None,
            Some(3),
            None,
            Some(5),
            None,
            Some(7),
            None,
            Some(9),
            None,
            Some(11),
            None,
            Some(13),
            None,
            Some(15),
            None,
        ])
    } else {
        SnapshotV2BalloonStatistics::default()
    };
    let continuation = SnapshotV2BalloonContinuationState::new(
        activated.then(|| active_queues(config, pending_statistics)),
        config.stats_polling_interval_s(),
        statistics,
        (activated && pending_statistics).then_some(0),
        SnapshotV2BalloonHintState::new(
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            None,
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            true,
        ),
    );
    let accounting = if activated {
        SnapshotV2BalloonAccountingState::try_new(
            vec![
                SnapshotV2BalloonPfnRange::try_new(1, 2).expect("range should validate"),
                SnapshotV2BalloonPfnRange::try_new(4, 1).expect("range should validate"),
            ],
            3,
        )
        .expect("accounting should validate")
    } else {
        SnapshotV2BalloonAccountingState::empty()
    };
    SnapshotV2BalloonState::try_new(
        config,
        config_space,
        continuation,
        accounting,
        common(config, activated, pci),
        if pci {
            pci_transport(layout.queue_count())
        } else {
            mmio_transport()
        },
    )
    .expect("test state should validate")
}

#[test]
fn every_queue_layout_and_transport_round_trips_inactive_and_active() {
    for statistics in [false, true] {
        for hinting in [false, true] {
            for reporting in [false, true] {
                let config = config(statistics, hinting, reporting);
                for pci in [false, true] {
                    for activated in [false, true] {
                        let pending_statistics = activated && statistics;
                        let expected = state(config, activated, pci, pending_statistics);
                        let encoded = expected
                            .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
                            .expect("valid balloon state should encode");
                        assert!(encoded.len() <= NATIVE_V2_BALLOON_STATE_MAX_BYTES);
                        let decoded = SnapshotV2BalloonState::decode(
                            NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                            &encoded,
                        )
                        .expect("valid balloon state should decode");
                        assert_eq!(decoded, expected);
                        assert_eq!(
                            decoded
                                .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
                                .expect("decoded state should re-encode"),
                            encoded
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn component_header_and_section_geometry_are_exact() {
    let encoded = state(config(true, true, true), true, true, true)
        .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    assert_eq!(&encoded[..8], b"BANGBL2\0");
    assert_eq!(read_u16(&encoded, 8), 64);
    assert_eq!(read_u16(&encoded, 10), 1);
    assert_eq!(read_u16(&encoded, 12), 2);
    assert_eq!(read_u16(&encoded, 14), 4);
    assert_eq!(
        usize::try_from(read_u64(&encoded, 24)).expect("length should fit"),
        encoded.len()
    );
    assert_eq!(read_u64(&encoded, 32), 64);
    assert_eq!(read_u64(&encoded, 40), 192);
    assert!(encoded[48..64].iter().all(|byte| *byte == 0));

    let mut expected_offset = PAYLOAD_OFFSET;
    for (index, expected_kind) in [1_u16, 2, 3, 4].into_iter().enumerate() {
        let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        assert_eq!(read_u16(&encoded, entry), expected_kind);
        assert_eq!(read_u16(&encoded, entry + 2), 0);
        assert_eq!(read_u32(&encoded, entry + 4), 0);
        assert_eq!(
            usize::try_from(read_u64(&encoded, entry + DIRECTORY_PAYLOAD_OFFSET))
                .expect("offset should fit"),
            expected_offset
        );
        let length = usize::try_from(read_u64(&encoded, entry + DIRECTORY_LENGTH_OFFSET))
            .expect("length should fit");
        assert!(length.is_multiple_of(8));
        expected_offset += length;
        assert_eq!(read_u64(&encoded, entry + 24), 0);
    }
    assert_eq!(expected_offset, encoded.len());
}

#[test]
fn immutable_wire_fixtures_lock_inactive_mmio_and_active_pci_profiles() {
    for (state, expected) in [
        (
            state(config(false, false, false), false, false, false),
            fixture_bytes(include_str!("fixtures/inactive-mmio.hex")),
        ),
        (
            state(config(true, true, true), true, true, true),
            fixture_bytes(include_str!("fixtures/active-pci.hex")),
        ),
    ] {
        let encoded = state
            .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
            .expect("fixture should encode");
        assert_eq!(encoded, expected);
        assert_eq!(
            SnapshotV2BalloonState::decode(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                &expected,
            )
            .expect("immutable fixture should decode"),
            state
        );
    }
}

#[test]
fn statistics_descriptor_and_legal_hint_histories_round_trip_canonically() {
    let config = config(true, true, true);
    let mut empty = state(config, true, false, false);
    empty.continuation.statistics = SnapshotV2BalloonStatistics::default();
    let sparse_without_descriptor = state(config, true, false, false);
    let sparse_with_descriptor = state(config, true, false, true);
    let mut full = state(config, true, false, false);
    full.continuation.statistics = SnapshotV2BalloonStatistics::new(std::array::from_fn(|index| {
        Some(u64::try_from(index).expect("statistic index should fit") + 1)
    }));

    for expected in [
        empty,
        sparse_without_descriptor,
        sparse_with_descriptor,
        full,
    ] {
        let encoded = expected
            .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
            .expect("statistics fixture should encode");
        assert_eq!(
            SnapshotV2BalloonState::decode(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                &encoded,
            )
            .expect("statistics fixture should decode"),
            expected
        );
    }

    let running_cmd = VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1;
    for (host_cmd, guest_cmd, last_cmd, acknowledge_on_stop) in [
        (
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            None,
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            true,
        ),
        (running_cmd, None, running_cmd, false),
        (running_cmd, Some(running_cmd), running_cmd, false),
        (
            VIRTIO_BALLOON_FREE_PAGE_HINT_DONE,
            Some(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE),
            running_cmd,
            true,
        ),
    ] {
        let mut expected = state(config, true, false, false);
        expected.config_space = VirtioBalloonConfigSpace::new(
            expected.config_space.num_pages(),
            expected.config_space.actual_pages(),
            host_cmd,
        );
        expected.continuation.hinting =
            SnapshotV2BalloonHintState::new(host_cmd, guest_cmd, last_cmd, acknowledge_on_stop);
        let encoded = expected
            .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
            .expect("legal hint history should encode");
        assert_eq!(
            SnapshotV2BalloonState::decode(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                &encoded,
            )
            .expect("legal hint history should decode"),
            expected
        );
    }
}

#[test]
fn maximum_accounting_profile_round_trips_within_compile_time_bound() {
    let ranges = (0..NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES)
        .map(|index| {
            SnapshotV2BalloonPfnRange::try_new(
                u32::try_from(index * 2).expect("test PFN should fit"),
                1,
            )
            .expect("isolated test range should validate")
        })
        .collect();
    let accounting = SnapshotV2BalloonAccountingState::try_new(
        ranges,
        NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES as u64,
    )
    .expect("maximum accounting should validate");
    let mut expected = state(config(false, false, false), true, true, false);
    expected.accounting = accounting;
    let encoded = expected
        .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
        .expect("maximum accounting should encode");
    assert_eq!(encoded.len(), 2_097_888);
    assert!(encoded.len() < NATIVE_V2_BALLOON_STATE_MAX_BYTES);
    assert_eq!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &encoded)
            .expect("maximum accounting should decode"),
        expected
    );
}

#[test]
fn accounting_constructor_rejects_every_noncanonical_boundary() {
    assert_eq!(
        SnapshotV2BalloonPfnRange::try_new(1, 0),
        Err(SnapshotV2BalloonStateBuildError::Accounting)
    );
    assert_eq!(
        SnapshotV2BalloonPfnRange::try_new(u32::MAX, 2),
        Err(SnapshotV2BalloonStateBuildError::Accounting)
    );
    for ranges in [
        vec![
            SnapshotV2BalloonPfnRange::from_parts(4, 1),
            SnapshotV2BalloonPfnRange::from_parts(1, 1),
        ],
        vec![
            SnapshotV2BalloonPfnRange::from_parts(1, 2),
            SnapshotV2BalloonPfnRange::from_parts(2, 1),
        ],
        vec![
            SnapshotV2BalloonPfnRange::from_parts(1, 2),
            SnapshotV2BalloonPfnRange::from_parts(3, 1),
        ],
    ] {
        assert!(matches!(
            SnapshotV2BalloonAccountingState::try_new(ranges, 2),
            Err(SnapshotV2BalloonStateBuildError::Accounting)
        ));
    }
    assert!(matches!(
        SnapshotV2BalloonAccountingState::try_new(
            vec![SnapshotV2BalloonPfnRange::from_parts(1, 1)],
            2,
        ),
        Err(SnapshotV2BalloonStateBuildError::Accounting)
    ));
    let over_limit = (0..=NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES)
        .map(|index| {
            SnapshotV2BalloonPfnRange::from_parts(
                u32::try_from(index * 2).expect("test PFN should fit"),
                1,
            )
        })
        .collect();
    assert!(matches!(
        SnapshotV2BalloonAccountingState::try_new(
            over_limit,
            NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES as u64 + 1,
        ),
        Err(SnapshotV2BalloonStateBuildError::Accounting)
    ));
}

#[test]
fn constructor_rejects_cross_field_relationship_failures() {
    let expected = state(config(true, true, true), true, true, true);

    let mut invalid = expected.clone();
    invalid.config_space = VirtioBalloonConfigSpace::new(1, 17, VIRTIO_BALLOON_FREE_PAGE_HINT_STOP);
    assert_eq!(
        validate_balloon_state(&invalid),
        Err(SnapshotV2BalloonStateBuildError::Configuration)
    );

    let mut invalid = expected.clone();
    invalid.continuation.stats_polling_interval_s = 4;
    assert_eq!(
        validate_balloon_state(&invalid),
        Err(SnapshotV2BalloonStateBuildError::Configuration)
    );

    let mut invalid = expected.clone();
    invalid.continuation.hinting = SnapshotV2BalloonHintState::new(2, None, 3, true);
    assert_eq!(
        validate_balloon_state(&invalid),
        Err(SnapshotV2BalloonStateBuildError::Hinting)
    );

    let mut invalid = expected.clone();
    invalid.continuation.statistics_pending_descriptor_head = Some(VIRTIO_BALLOON_QUEUE_SIZE);
    assert_eq!(
        validate_balloon_state(&invalid),
        Err(SnapshotV2BalloonStateBuildError::Statistics)
    );

    let mut invalid = expected.clone();
    let mut queues = invalid.virtio.queues().to_vec();
    queues[1] = queues[0];
    invalid.virtio = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: invalid.virtio.available_features(),
        driver_features: invalid.virtio.driver_features(),
        config_generation: invalid.virtio.config_generation(),
        status: invalid.virtio.status(),
        activated: true,
        queues,
        pending_notifications: invalid.virtio.pending_notifications().to_vec(),
        interrupt_intents: invalid.virtio.interrupt_intents().to_vec(),
    });
    assert_eq!(
        validate_balloon_state(&invalid),
        Err(SnapshotV2BalloonStateBuildError::Queue)
    );

    let mut invalid = expected;
    if let SnapshotV2DeviceTransport::Pci(pci) = &invalid.transport {
        let mut queue_vectors = pci.msix().queue_vectors().to_vec();
        queue_vectors.pop();
        let replacement = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
            entries: pci.msix().entries().to_vec(),
            pending_words: pci.msix().pending_words().to_vec(),
            enabled: pci.msix().enabled(),
            function_masked: pci.msix().function_masked(),
            config_vector: pci.msix().config_vector(),
            queue_vectors,
            pending_transition_observed: pci.msix().pending_transition_observed(),
        });
        invalid.transport = SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(
            SnapshotV2PciDeviceStateParts {
                phase: pci.phase(),
                origin: pci.origin(),
                sbdf: pci.sbdf(),
                bar_index: pci.bar_index(),
                bar_address_space: pci.bar_address_space(),
                bar_prefetchable: pci.bar_prefetchable(),
                bar_range: pci.bar_range(),
                device_feature_select: pci.device_feature_select(),
                driver_feature_select: pci.driver_feature_select(),
                queue_select: pci.queue_select(),
                pci_cfg_bar: pci.pci_cfg_bar(),
                pci_cfg_offset: pci.pci_cfg_offset(),
                pci_cfg_length: pci.pci_cfg_length(),
                writable_bytes: pci.writable_bytes().to_vec(),
                bar_probes: pci.bar_probes().to_vec(),
                msix: replacement,
            },
        ));
    }
    assert_eq!(
        validate_balloon_state(&invalid),
        Err(SnapshotV2BalloonStateBuildError::Transport)
    );
}

#[test]
fn structural_mutations_fail_closed() {
    let encoded = state(config(true, true, true), true, true, true)
        .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");

    assert!(matches!(
        SnapshotV2BalloonState::decode(SnapshotFormatVersion::new(2, 8, 0), &encoded),
        Err(SnapshotV2BalloonStateDecodeError::UnsupportedVersion)
    ));
    assert!(matches!(
        state(config(false, false, false), false, false, false)
            .encode(SnapshotFormatVersion::new(2, 8, 0)),
        Err(SnapshotV2BalloonStateEncodeError::UnsupportedVersion)
    ));

    let mut invalid = encoded.clone();
    invalid[0] ^= 0x80;
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidMagic)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, 10, 2);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidProfile)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, 12, 3);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidTransport)
    ));

    let mut invalid = encoded.clone();
    invalid[48] = 1;
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::NonzeroReserved)
    ));

    let mut invalid = encoded.clone();
    replace_u32(&mut invalid, 16, 1);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, DIRECTORY_OFFSET, 9);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, DIRECTORY_OFFSET + 2, 1);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let mut invalid = encoded.clone();
    let common_entry = DIRECTORY_OFFSET + DIRECTORY_ENTRY_BYTES;
    let common_offset = read_u64(&invalid, common_entry + DIRECTORY_PAYLOAD_OFFSET);
    replace_u64(
        &mut invalid,
        common_entry + DIRECTORY_PAYLOAD_OFFSET,
        common_offset + 8,
    );
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let mut invalid = encoded.clone();
    replace_u64(
        &mut invalid,
        common_entry + DIRECTORY_PAYLOAD_OFFSET,
        common_offset - 8,
    );
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let mut invalid = encoded.clone();
    let common_length = read_u64(&invalid, common_entry + DIRECTORY_LENGTH_OFFSET);
    replace_u64(
        &mut invalid,
        common_entry + DIRECTORY_LENGTH_OFFSET,
        common_length + 1,
    );
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let mut invalid = encoded.clone();
    let transport_entry = DIRECTORY_OFFSET + 3 * DIRECTORY_ENTRY_BYTES;
    let transport_offset = usize::try_from(read_u64(
        &invalid,
        transport_entry + DIRECTORY_PAYLOAD_OFFSET,
    ))
    .expect("transport offset should fit");
    let transport_length = usize::try_from(read_u64(
        &invalid,
        transport_entry + DIRECTORY_LENGTH_OFFSET,
    ))
    .expect("transport length should fit");
    invalid[transport_offset + transport_length - 1] = 1;
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::NonzeroReserved)
    ));

    assert!(matches!(
        SnapshotV2BalloonState::decode(
            NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
            &encoded[..encoded.len() - 1]
        ),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure
            | SnapshotV2BalloonStateDecodeError::Truncated)
    ));

    let mut trailing = encoded;
    trailing.extend_from_slice(&[0; 8]);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &trailing),
        Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
    ));

    let oversized = vec![0; NATIVE_V2_BALLOON_STATE_MAX_BYTES + 1];
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &oversized),
        Err(SnapshotV2BalloonStateDecodeError::TooLarge)
    ));
}

#[test]
fn local_and_accounting_noncanonical_values_reject_before_typed_publication() {
    let encoded = state(config(true, true, true), true, false, true)
        .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");

    let mut invalid = encoded.clone();
    invalid[PAYLOAD_OFFSET + 200] = 1;
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::NonzeroReserved)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, PAYLOAD_OFFSET + 4, 1 << 15);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidValue)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, PAYLOAD_OFFSET + 22, 0);
    replace_u16(&mut invalid, PAYLOAD_OFFSET + 28, 1);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidValue)
    ));

    let mut invalid = encoded.clone();
    invalid[PAYLOAD_OFFSET + 26] = 0b11;
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidValue)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, PAYLOAD_OFFSET + 24, 0);
    replace_u64(&mut invalid, PAYLOAD_OFFSET + 72, 1);
    assert!(matches!(
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &invalid),
        Err(SnapshotV2BalloonStateDecodeError::InvalidValue)
    ));

    let accounting_entry = DIRECTORY_OFFSET + 2 * DIRECTORY_ENTRY_BYTES;
    let accounting_offset = usize::try_from(read_u64(
        &encoded,
        accounting_entry + DIRECTORY_PAYLOAD_OFFSET,
    ))
    .expect("accounting offset should fit");
    let mut invalid = encoded;
    replace_u32(&mut invalid, accounting_offset + 16 + 4, 0);
    let mut reserve = FailAtReserve::new(0);
    assert!(matches!(
        decode_with_policy(
            NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
            &invalid,
            &mut reserve,
        ),
        Err(SnapshotV2BalloonStateDecodeError::InvalidValue)
    ));
    assert_eq!(reserve.calls, 0);
}

#[test]
fn allocation_failures_are_deterministic_and_redacted() {
    let expected = state(config(true, true, true), true, true, true);
    let mut reserve = FailAtReserve::new(0);
    assert!(matches!(
        encode_with_policy(
            NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
            &expected,
            &mut reserve,
        ),
        Err(SnapshotV2BalloonStateEncodeError::Allocation)
    ));

    let encoded = expected
        .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    for fail_at in 0..9 {
        let mut reserve = FailAtReserve::new(fail_at);
        assert!(matches!(
            decode_with_policy(
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                &encoded,
                &mut reserve,
            ),
            Err(SnapshotV2BalloonStateDecodeError::Allocation)
        ));
    }
    let mut reserve = FailAtReserve::new(9);
    assert_eq!(
        decode_with_policy(
            NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
            &encoded,
            &mut reserve,
        )
        .expect("all nine reservations should succeed"),
        expected
    );

    let debug = format!("{expected:?}");
    assert!(!debug.contains("2147483648"));
    assert!(!debug.contains("4276092928"));
    for error in [
        SnapshotV2BalloonStateDecodeError::InvalidValue,
        SnapshotV2BalloonStateDecodeError::Allocation,
        SnapshotV2BalloonStateDecodeError::InvalidState(
            SnapshotV2BalloonStateBuildError::Accounting,
        ),
    ] {
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains("2147483648"));
        assert!(!debug.contains("2147483648"));
    }
}

struct FailAtReserve {
    fail_at: usize,
    calls: usize,
}

impl FailAtReserve {
    const fn new(fail_at: usize) -> Self {
        Self { fail_at, calls: 0 }
    }
}

impl ReservePolicy for FailAtReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        let call = self.calls;
        self.calls += 1;
        if call == self.fail_at {
            Err(())
        } else {
            values.try_reserve_exact(additional).map_err(|_| ())
        }
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("u16 field should be present"),
    )
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("u32 field should be present"),
    )
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("u64 field should be present"),
    )
}

fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.split_whitespace().collect::<String>();
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
            u8::from_str_radix(pair, 16).expect("fixture hex should decode")
        })
        .collect()
}

fn replace_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn replace_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn replace_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
