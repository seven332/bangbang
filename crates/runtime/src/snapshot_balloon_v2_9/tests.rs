use crate::balloon::{BalloonConfigInput, VIRTIO_BALLOON_FREE_PAGE_HINT_STOP};
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
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
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioInterruptIntent,
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
const RESTORE_LOW_MEMORY_SIZE: u64 = 0x10_0000;
const RESTORE_QUEUE_MEMORY_START: u64 = 0x8000_0000;
const RESTORE_QUEUE_STRIDE: u64 = 0x1_0000;
const AVAILABLE_INDEX_OFFSET: u64 = 2;
const AVAILABLE_RING_OFFSET: u64 = 4;
const USED_INDEX_OFFSET: u64 = 2;

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

fn restore_memory(state: &SnapshotV2BalloonState) -> GuestMemory {
    let mut ranges = vec![
        GuestMemoryRange::new(GuestAddress::new(0), RESTORE_LOW_MEMORY_SIZE)
            .expect("low restore memory should validate"),
    ];
    if state.virtio().is_activated() {
        ranges.push(
            GuestMemoryRange::new(
                GuestAddress::new(RESTORE_QUEUE_MEMORY_START),
                u64::try_from(state.virtio().queues().len()).expect("queue count should fit")
                    * RESTORE_QUEUE_STRIDE,
            )
            .expect("queue restore memory should validate"),
        );
    }
    let layout = GuestMemoryLayout::new(ranges).expect("restore memory layout should validate");
    GuestMemory::allocate(&layout).expect("restore memory should allocate")
}

fn queue_only_restore_memory(state: &SnapshotV2BalloonState) -> GuestMemory {
    let queue_size = u64::try_from(state.virtio().queues().len()).expect("queue count should fit")
        * RESTORE_QUEUE_STRIDE;
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(GuestAddress::new(RESTORE_QUEUE_MEMORY_START), queue_size)
            .expect("queue restore memory should validate"),
    ])
    .expect("queue-only restore layout should validate");
    GuestMemory::allocate(&layout).expect("queue-only restore memory should allocate")
}

fn initialize_restore_queue_memory(memory: &mut GuestMemory, state: &SnapshotV2BalloonState) {
    let Some(active) = state.continuation().active_queues() else {
        return;
    };
    let layout = VirtioBalloonQueueLayout::from_config(state.config());
    for (index, queue) in state.virtio().queues().iter().enumerate() {
        let cursor = active
            .cursor_for_layout(layout, index)
            .expect("active queue should have retained cursors");
        write_memory_u16(
            memory,
            queue
                .driver_ring()
                .checked_add(AVAILABLE_INDEX_OFFSET)
                .expect("available index address should fit"),
            cursor.next_available(),
        );
        write_memory_u16(
            memory,
            queue
                .device_ring()
                .checked_add(USED_INDEX_OFFSET)
                .expect("used index address should fit"),
            cursor.next_used(),
        );
    }

    if let Some(pending_head) = state.continuation().statistics_pending_descriptor_head() {
        let statistics = layout
            .statistics()
            .expect("pending statistics require a statistics queue");
        let cursor = active
            .statistics()
            .expect("pending statistics require active statistics cursors");
        let queue = state
            .virtio()
            .queues()
            .get(statistics.index())
            .expect("statistics queue should exist");
        let ring_index = cursor.next_available().wrapping_sub(1) % queue.size();
        write_memory_u16(
            memory,
            queue
                .driver_ring()
                .checked_add(AVAILABLE_RING_OFFSET + u64::from(ring_index) * 2)
                .expect("pending statistics entry should fit"),
            pending_head,
        );
    }
}

fn write_memory_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
    memory
        .write_slice(&value.to_le_bytes(), address)
        .expect("u16 restore fixture write should succeed");
}

fn restore_memory_bytes(memory: &GuestMemory) -> Vec<u8> {
    let total = memory
        .regions()
        .iter()
        .map(|region| {
            usize::try_from(region.range().size()).expect("test memory region size should fit")
        })
        .sum();
    let mut bytes = Vec::with_capacity(total);
    for region in memory.regions() {
        let start = bytes.len();
        bytes.resize(
            start
                + usize::try_from(region.range().size())
                    .expect("test memory region size should fit"),
            0,
        );
        memory
            .read_slice(&mut bytes[start..], region.range().start())
            .expect("restore fixture memory should read");
    }
    bytes
}

fn prepared_device(plan: &SnapshotV2BalloonRestorePlan) -> &VirtioBalloonDevice {
    match plan.transport() {
        PreparedSnapshotV2BalloonTransport::Mmio(mmio) => mmio.device(),
        PreparedSnapshotV2BalloonTransport::Pci(pci) => pci.device(),
    }
}

fn assert_restored_queue_cursors(
    device: &VirtioBalloonDevice,
    expected: SnapshotV2BalloonActiveQueuesState,
) {
    let active = device
        .active_queues()
        .expect("expected active restored queues");
    let pairs = [
        (Some(active.inflate()), Some(expected.inflate())),
        (Some(active.deflate()), Some(expected.deflate())),
        (active.statistics(), expected.statistics()),
        (active.free_page_hinting(), expected.free_page_hinting()),
        (active.free_page_reporting(), expected.free_page_reporting()),
    ];
    for (actual, expected) in pairs {
        match (actual, expected) {
            (Some(actual), Some(expected)) => {
                assert_eq!(
                    actual.available_ring().next_avail(),
                    expected.next_available()
                );
                assert_eq!(actual.used_ring().next_used(), expected.next_used());
            }
            (None, None) => {}
            _ => panic!("restored active queue shape should match"),
        }
    }
}

fn assert_restored_device_registers(
    actual: &crate::virtio_mmio::VirtioMmioDeviceRegisters,
    expected: &SnapshotV2VirtioState,
) {
    assert_eq!(actual.device_id(), VIRTIO_BALLOON_DEVICE_ID);
    assert_eq!(actual.device_features(), expected.available_features());
    assert_eq!(actual.driver_features(), expected.driver_features());
    assert_eq!(actual.config_generation(), expected.config_generation());
    assert_eq!(actual.status(), expected.status());
}

fn assert_restored_queue_state(
    actual: &VirtioMmioQueueState,
    expected: &SnapshotV2VirtioQueueState,
) {
    assert_eq!(actual.max_size(), expected.max_size());
    assert_eq!(actual.size(), expected.size());
    assert_eq!(actual.ready(), expected.ready());
    assert_eq!(actual.descriptor_table(), expected.descriptor_table());
    assert_eq!(actual.driver_ring(), expected.driver_ring());
    assert_eq!(actual.device_ring(), expected.device_ring());
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

#[test]
fn restore_plan_reconstructs_every_queue_layout_and_transport() {
    for statistics_enabled in [false, true] {
        for hinting_enabled in [false, true] {
            for reporting_enabled in [false, true] {
                let config = config(statistics_enabled, hinting_enabled, reporting_enabled);
                for pci in [false, true] {
                    for activated in [false, true] {
                        let expected =
                            state(config, activated, pci, activated && statistics_enabled);
                        let mut memory = restore_memory(&expected);
                        initialize_restore_queue_memory(&mut memory, &expected);
                        let plan = SnapshotV2BalloonRestorePlan::prepare(expected.clone(), &memory)
                            .expect("valid balloon restore state should prepare");

                        assert_eq!(plan.config(), config);
                        assert_eq!(
                            plan.config_space().num_pages(),
                            mib_to_4k_pages(config.amount_mib())
                                .expect("test page count should fit")
                        );
                        assert_eq!(
                            plan.config_space().actual_pages(),
                            if activated { 17 } else { 0 }
                        );
                        assert_eq!(
                            plan.config_space().free_page_hint_cmd_id(),
                            if hinting_enabled {
                                VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
                            } else {
                                VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
                            }
                        );
                        assert_eq!(
                            plan.queue_ranges().len(),
                            if activated {
                                VirtioBalloonQueueLayout::from_config(config).queue_count()
                            } else {
                                0
                            }
                        );
                        assert_eq!(
                            plan.transport_kind(),
                            if pci {
                                SnapshotV2DeviceTransportKind::Pci
                            } else {
                                SnapshotV2DeviceTransportKind::Mmio
                            }
                        );

                        let device = prepared_device(&plan);
                        assert_eq!(
                            device.queue_layout(),
                            VirtioBalloonQueueLayout::from_config(config)
                        );
                        assert_eq!(device.is_activated(), activated);
                        assert_eq!(
                            device.memory_accounting().inflated_page_count(),
                            if activated { 3 } else { 0 }
                        );
                        assert_eq!(
                            device
                                .memory_accounting()
                                .inflated_page_ranges()
                                .iter()
                                .map(|range| (range.start_pfn(), range.page_count()))
                                .collect::<Vec<_>>(),
                            expected
                                .accounting()
                                .ranges()
                                .iter()
                                .map(|range| (range.start_pfn(), range.page_count()))
                                .collect::<Vec<_>>()
                        );
                        assert_eq!(
                            device.stats_polling_interval_s(),
                            config.stats_polling_interval_s()
                        );
                        assert_eq!(
                            device.statistics_pending_descriptor_head(),
                            (activated && statistics_enabled).then_some(0)
                        );
                        assert_eq!(
                            device.statistics(),
                            restore_balloon_statistics(expected.continuation().statistics())
                        );
                        if let Some(active) = expected.continuation().active_queues() {
                            assert_restored_queue_cursors(device, active);
                        } else {
                            assert!(device.active_queues().is_none());
                        }

                        if hinting_enabled {
                            let hinting = device
                                .hinting_status()
                                .expect("restored hinting should remain enabled");
                            assert_eq!(hinting.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
                            assert_eq!(hinting.guest_cmd(), None);
                            assert_eq!(
                                device.hinting_last_cmd(),
                                VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
                            );
                            assert!(
                                device
                                    .hinting_acknowledge_on_stop()
                                    .expect("restored hint policy should exist")
                            );
                        } else {
                            assert!(device.hinting_status().is_err());
                            assert_eq!(
                                device.hinting_last_cmd(),
                                VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
                            );
                        }

                        match plan.transport() {
                            PreparedSnapshotV2BalloonTransport::Mmio(mmio) => {
                                assert!(!pci);
                                let SnapshotV2DeviceTransport::Mmio(expected_mmio) =
                                    expected.transport()
                                else {
                                    panic!("expected MMIO source transport");
                                };
                                assert_eq!(mmio.region(), expected_mmio.region());
                                assert_eq!(mmio.interrupt_line(), expected_mmio.interrupt_line());
                                assert_restored_device_registers(
                                    mmio.retained().device_registers(),
                                    expected.virtio(),
                                );
                                assert_eq!(
                                    mmio.retained().device_registers().device_features_select(),
                                    expected_mmio.device_feature_select()
                                );
                                assert_eq!(
                                    mmio.retained().device_registers().driver_features_select(),
                                    expected_mmio.driver_feature_select()
                                );
                                assert_eq!(
                                    mmio.retained().queue_select(),
                                    expected_mmio.queue_select()
                                );
                                assert_eq!(
                                    mmio.retained().queues().len(),
                                    VirtioBalloonQueueLayout::from_config(config).queue_count()
                                );
                                for (actual, expected_queue) in mmio
                                    .retained()
                                    .queues()
                                    .iter()
                                    .zip(expected.virtio().queues())
                                {
                                    assert_restored_queue_state(actual, expected_queue);
                                }
                                assert_eq!(mmio.retained().is_device_activated(), activated);
                                assert!(mmio.retained().requires_device_config_write_status());
                                assert_eq!(
                                    mmio.retained()
                                        .pending_notifications()
                                        .iter()
                                        .copied()
                                        .enumerate()
                                        .filter(|(_, pending)| *pending)
                                        .map(|(index, _)| {
                                            u16::try_from(index)
                                                .expect("queue index should fit u16")
                                        })
                                        .collect::<Vec<_>>(),
                                    expected.virtio().pending_notifications()
                                );
                            }
                            PreparedSnapshotV2BalloonTransport::Pci(pci_transport) => {
                                assert!(pci);
                                let SnapshotV2DeviceTransport::Pci(expected_pci) =
                                    expected.transport()
                                else {
                                    panic!("expected PCI source transport");
                                };
                                assert_eq!(pci_transport.origin(), expected_pci.origin());
                                assert_eq!(pci_transport.sbdf(), expected_pci.sbdf());
                                assert_eq!(pci_transport.bar_range(), expected_pci.bar_range());
                                assert_eq!(
                                    pci_transport.identity().device_type().raw_value(),
                                    VIRTIO_BALLOON_DEVICE_ID
                                );
                                assert_restored_device_registers(
                                    pci_transport.retained().device_registers(),
                                    expected.virtio(),
                                );
                                assert_eq!(
                                    pci_transport.retained().device_feature_select(),
                                    expected_pci.device_feature_select()
                                );
                                assert_eq!(
                                    pci_transport.retained().driver_feature_select(),
                                    expected_pci.driver_feature_select()
                                );
                                assert_eq!(
                                    pci_transport.retained().queue_select(),
                                    expected_pci.queue_select()
                                );
                                assert_eq!(
                                    pci_transport.retained().queues().queue_count(),
                                    VirtioBalloonQueueLayout::from_config(config).queue_count()
                                );
                                for (index, expected_queue) in
                                    expected.virtio().queues().iter().enumerate()
                                {
                                    let index =
                                        u32::try_from(index).expect("queue index should fit u32");
                                    let actual = pci_transport
                                        .retained()
                                        .queues()
                                        .queue(index)
                                        .expect("restored PCI queue should exist");
                                    assert_restored_queue_state(actual, expected_queue);
                                }
                                assert_eq!(
                                    pci_transport
                                        .retained()
                                        .queue_notifications()
                                        .pending_queue_notifications()
                                        .into_iter()
                                        .map(|index| {
                                            u16::try_from(index)
                                                .expect("queue index should fit u16")
                                        })
                                        .collect::<Vec<_>>(),
                                    expected.virtio().pending_notifications()
                                );
                                assert_eq!(
                                    pci_transport
                                        .retained()
                                        .interrupt_intents()
                                        .iter()
                                        .copied()
                                        .map(|intent| match intent {
                                            VirtioInterruptIntent::Queue { queue_index } => {
                                                SnapshotV2InterruptIntent::Queue { queue_index }
                                            }
                                            VirtioInterruptIntent::Configuration => {
                                                SnapshotV2InterruptIntent::Configuration
                                            }
                                        })
                                        .collect::<Vec<_>>(),
                                    expected.virtio().interrupt_intents()
                                );
                                assert_eq!(
                                    pci_transport.retained().msix_vector_count(),
                                    VirtioBalloonQueueLayout::from_config(config).queue_count() + 1
                                );
                                let actual_msix = pci_transport.retained().msix_state();
                                let expected_msix = expected_pci.msix();
                                assert_eq!(
                                    actual_msix.pending_words(),
                                    expected_msix.pending_words()
                                );
                                assert_eq!(actual_msix.enabled(), expected_msix.enabled());
                                assert_eq!(
                                    actual_msix.function_masked(),
                                    expected_msix.function_masked()
                                );
                                assert_eq!(
                                    actual_msix.config_vector(),
                                    expected_msix.config_vector()
                                );
                                assert_eq!(
                                    actual_msix.queue_vectors(),
                                    expected_msix.queue_vectors()
                                );
                                assert_eq!(
                                    actual_msix.pending_transition_observed(),
                                    expected_msix.pending_transition_observed()
                                );
                                for (actual, expected_entry) in
                                    actual_msix.entries().iter().zip(expected_msix.entries())
                                {
                                    assert_eq!(
                                        actual.message_address_low(),
                                        expected_entry.message_address_low()
                                    );
                                    assert_eq!(
                                        actual.message_address_high(),
                                        expected_entry.message_address_high()
                                    );
                                    assert_eq!(
                                        actual.message_data(),
                                        expected_entry.message_data()
                                    );
                                    assert_eq!(
                                        actual.vector_control(),
                                        expected_entry.vector_control()
                                    );
                                }
                                assert_eq!(
                                    pci_transport.retained().is_device_activated(),
                                    activated
                                );
                                assert!(
                                    !pci_transport
                                        .retained()
                                        .requires_device_config_write_status()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn restore_plan_normalizes_enabled_hinting_after_retaining_checked_history() {
    let config = config(false, true, false);
    let mut expected = state(config, true, false, false);
    expected.config_space = expected.config_space.with_free_page_hint_cmd_id(9);
    expected.continuation.hinting = SnapshotV2BalloonHintState::new(9, Some(9), 9, false);
    validate_balloon_state(&expected).expect("source hint history should validate");

    let mut memory = restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    let plan = SnapshotV2BalloonRestorePlan::prepare(expected, &memory)
        .expect("enabled hint history should prepare");
    let device = prepared_device(&plan);
    let hinting = device
        .hinting_status()
        .expect("restored hinting should remain enabled");

    assert_eq!(
        plan.config_space().free_page_hint_cmd_id(),
        VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
    );
    assert_eq!(hinting.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
    assert_eq!(hinting.guest_cmd(), Some(9));
    assert_eq!(device.hinting_last_cmd(), 9);
    assert!(
        !device
            .hinting_acknowledge_on_stop()
            .expect("restored hint policy should exist")
    );
}

#[test]
fn restore_plan_preserves_wrapping_queue_cursors_and_pending_statistics() {
    let config = config(true, false, false);
    let mut expected = state(config, true, false, true);
    let idle = SnapshotV2BalloonQueueState::try_new(u16::MAX, u16::MAX, VIRTIO_BALLOON_QUEUE_SIZE)
        .expect("wrapping idle cursors should validate");
    let pending = SnapshotV2BalloonQueueState::try_new(0, u16::MAX, VIRTIO_BALLOON_QUEUE_SIZE)
        .expect("wrapping pending cursors should validate");
    expected.continuation.active_queues = Some(
        SnapshotV2BalloonActiveQueuesState::try_new(config, idle, idle, Some(pending), None, None)
            .expect("wrapping active queue cursors should validate"),
    );
    validate_balloon_state(&expected).expect("wrapping source state should validate");

    let mut memory = restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    let plan = SnapshotV2BalloonRestorePlan::prepare(expected, &memory)
        .expect("wrapping queue continuation should prepare");
    let active = prepared_device(&plan)
        .active_queues()
        .expect("restored queues should remain active");
    assert_eq!(active.inflate().available_ring().next_avail(), u16::MAX);
    assert_eq!(active.inflate().used_ring().next_used(), u16::MAX);
    let statistics = active
        .statistics()
        .expect("restored statistics queue should exist");
    assert_eq!(statistics.available_ring().next_avail(), 0);
    assert_eq!(statistics.used_ring().next_used(), u16::MAX);
    assert_eq!(
        prepared_device(&plan).statistics_pending_descriptor_head(),
        Some(0)
    );
}

#[test]
fn restore_plan_rejects_destination_memory_and_queue_continuation_mismatches() {
    let config = config(true, true, true);
    let expected = state(config, true, false, true);

    let mut invalid = expected.clone();
    invalid.config_space = invalid
        .config_space
        .with_num_pages(invalid.config_space.num_pages() + 1);
    let memory = restore_memory(&expected);
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare(invalid, &memory).unwrap_err(),
        SnapshotV2BalloonRestorePlanError::InvalidState
    );

    let low = GuestMemoryRange::new(GuestAddress::new(0), RESTORE_LOW_MEMORY_SIZE)
        .expect("low memory should validate");
    let truncated_queue =
        GuestMemoryRange::new(GuestAddress::new(RESTORE_QUEUE_MEMORY_START), 0x4000)
            .expect("truncated queue memory should validate");
    let memory = GuestMemory::allocate(
        &GuestMemoryLayout::new(vec![low, truncated_queue])
            .expect("truncated layout should validate"),
    )
    .expect("truncated restore memory should allocate");
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare(expected.clone(), &memory).unwrap_err(),
        SnapshotV2BalloonRestorePlanError::QueueMemory
    );

    let mut memory = queue_only_restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare(expected.clone(), &memory).unwrap_err(),
        SnapshotV2BalloonRestorePlanError::AccountingMemory
    );

    let mut memory = restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    let inflate = expected
        .virtio()
        .queues()
        .first()
        .expect("inflate queue should exist");
    write_memory_u16(
        &mut memory,
        inflate
            .device_ring()
            .checked_add(USED_INDEX_OFFSET)
            .expect("used index address should fit"),
        8,
    );
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare(expected.clone(), &memory).unwrap_err(),
        SnapshotV2BalloonRestorePlanError::QueueContinuation
    );

    let mut memory = restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    let layout = VirtioBalloonQueueLayout::from_config(config);
    let statistics = layout.statistics().expect("statistics queue should exist");
    let queue = expected
        .virtio()
        .queues()
        .get(statistics.index())
        .expect("statistics queue state should exist");
    let cursor = expected
        .continuation()
        .active_queues()
        .and_then(SnapshotV2BalloonActiveQueuesState::statistics)
        .expect("statistics cursors should exist");
    let pending_ring_index = cursor.next_available().wrapping_sub(1) % queue.size();
    write_memory_u16(
        &mut memory,
        queue
            .driver_ring()
            .checked_add(AVAILABLE_RING_OFFSET + u64::from(pending_ring_index) * 2)
            .expect("pending entry address should fit"),
        1,
    );
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare(expected.clone(), &memory).unwrap_err(),
        SnapshotV2BalloonRestorePlanError::QueueContinuation
    );

    let mut memory = restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    write_memory_u16(
        &mut memory,
        queue
            .driver_ring()
            .checked_add(AVAILABLE_INDEX_OFFSET)
            .expect("available index address should fit"),
        cursor.next_available().wrapping_add(1),
    );
    let duplicate_ring_index = cursor.next_available() % queue.size();
    write_memory_u16(
        &mut memory,
        queue
            .driver_ring()
            .checked_add(AVAILABLE_RING_OFFSET + u64::from(duplicate_ring_index) * 2)
            .expect("duplicate entry address should fit"),
        0,
    );
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare(expected, &memory).unwrap_err(),
        SnapshotV2BalloonRestorePlanError::QueueContinuation
    );
}

#[test]
fn restore_plan_accepts_accounting_across_adjacent_mapped_regions() {
    let config = config(false, false, false);
    let mut expected = state(config, true, false, false);
    expected.accounting = SnapshotV2BalloonAccountingState::try_new(
        vec![
            SnapshotV2BalloonPfnRange::try_new(3, 2)
                .expect("cross-region accounting range should validate"),
        ],
        2,
    )
    .expect("cross-region accounting should validate");
    let queue_size = u64::try_from(expected.virtio().queues().len())
        .expect("queue count should fit")
        * RESTORE_QUEUE_STRIDE;
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(GuestAddress::new(0), 0x4000)
            .expect("first accounting region should validate"),
        GuestMemoryRange::new(GuestAddress::new(0x4000), RESTORE_LOW_MEMORY_SIZE - 0x4000)
            .expect("second accounting region should validate"),
        GuestMemoryRange::new(GuestAddress::new(RESTORE_QUEUE_MEMORY_START), queue_size)
            .expect("queue region should validate"),
    ])
    .expect("adjacent accounting layout should validate");
    let mut memory = GuestMemory::allocate(&layout).expect("restore memory should allocate");
    initialize_restore_queue_memory(&mut memory, &expected);

    let plan = SnapshotV2BalloonRestorePlan::prepare(expected, &memory)
        .expect("accounting spanning adjacent mapped regions should prepare");
    assert_eq!(
        prepared_device(&plan)
            .memory_accounting()
            .inflated_page_count(),
        2
    );
}

#[test]
fn restore_plan_allocation_isolation_and_diagnostics_are_deterministic() {
    let config = config(true, true, true);
    let expected = state(config, true, false, true);
    let mut memory = restore_memory(&expected);
    initialize_restore_queue_memory(&mut memory, &expected);
    let before = restore_memory_bytes(&memory);

    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare_with_queue_range_allocation_failure(
            expected.clone(),
            &memory,
        )
        .unwrap_err(),
        SnapshotV2BalloonRestorePlanError::Allocation
    );
    assert_eq!(
        SnapshotV2BalloonRestorePlan::prepare_with_accounting_allocation_failure(
            expected.clone(),
            &memory,
        )
        .unwrap_err(),
        SnapshotV2BalloonRestorePlanError::Allocation
    );

    let first = SnapshotV2BalloonRestorePlan::prepare(expected.clone(), &memory)
        .expect("first restore plan should prepare");
    let second = SnapshotV2BalloonRestorePlan::prepare(expected, &memory)
        .expect("second restore plan should prepare");
    assert_eq!(restore_memory_bytes(&memory), before);
    assert_ne!(
        first.queue_ranges().as_ptr(),
        second.queue_ranges().as_ptr()
    );
    assert_ne!(
        prepared_device(&first)
            .memory_accounting()
            .inflated_page_ranges()
            .as_ptr(),
        prepared_device(&second)
            .memory_accounting()
            .inflated_page_ranges()
            .as_ptr()
    );

    let debug = format!("{first:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("2147483648"));
    for error in [
        SnapshotV2BalloonRestorePlanError::InvalidState,
        SnapshotV2BalloonRestorePlanError::QueueMemory,
        SnapshotV2BalloonRestorePlanError::AccountingMemory,
        SnapshotV2BalloonRestorePlanError::QueueContinuation,
        SnapshotV2BalloonRestorePlanError::Allocation,
    ] {
        assert!(!format!("{error:?}").contains("2147483648"));
        assert!(!error.to_string().contains("2147483648"));
    }

    let (_, _, _, first_transport) = first.into_parts();
    let (_, _, _, second_transport) = second.into_parts();
    let PreparedSnapshotV2BalloonTransport::Mmio(first) = first_transport else {
        panic!("expected MMIO restore transport");
    };
    let PreparedSnapshotV2BalloonTransport::Mmio(second) = second_transport else {
        panic!("expected MMIO restore transport");
    };
    let (_, _, mut first_device, _) = (*first).into_parts();
    let (_, _, second_device, _) = (*second).into_parts();
    first_device.reset();
    assert!(first_device.memory_accounting().is_empty());
    assert_eq!(second_device.memory_accounting().inflated_page_count(), 3);
    assert!(second_device.is_activated());
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
