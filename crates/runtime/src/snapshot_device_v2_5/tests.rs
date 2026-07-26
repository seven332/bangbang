use super::codec::{self, ReservePolicy};
use super::*;

use crate::block::{VIRTIO_BLOCK_ID_BYTES, VirtioBlockDeviceId, VirtioBlockQueueState};
use crate::interrupt::GuestInterruptLine;
use crate::memory::GuestAddress;
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::PciSbdf;
use crate::snapshot_device_v2::{
    SnapshotV2BlockBucketState, SnapshotV2PciBarProbeState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry, SnapshotV2PciWritableByte,
    SnapshotV2VirtioStateParts,
};

const ROOTLESS_MMIO_FIXTURE_HEX: &str = include_str!("fixtures/rootless-mmio.hex");
const ROOT_MMIO_FIXTURE_HEX: &str = include_str!("fixtures/root-mmio.hex");
const ROOTLESS_PCI_FIXTURE_HEX: &str = include_str!("fixtures/rootless-pci.hex");
const ROOT_PCI_FIXTURE_HEX: &str = include_str!("fixtures/root-pci.hex");
const PROFILE_1_MMIO_FIXTURE_HEX: &str = include_str!("../snapshot_device_v2/fixtures/mmio.hex");
const PROFILE_1_PCI_FIXTURE_HEX: &str = include_str!("../snapshot_device_v2/fixtures/pci.hex");

const HEALTHY_DRIVER_OK: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
    | VIRTIO_DEVICE_STATUS_DRIVER
    | VIRTIO_DEVICE_STATUS_FEATURES_OK
    | VIRTIO_DEVICE_STATUS_DRIVER_OK;
const FIXTURE_RECORDS: usize = 2;

fn fixture_graph(
    transport_kind: SnapshotV2DeviceTransportKind,
    with_root: bool,
) -> SnapshotV2MultiBlockDeviceGraph {
    let records = (0..FIXTURE_RECORDS)
        .map(|index| {
            let index = u32::try_from(index).expect("fixture index should fit");
            fixture_record(
                index,
                with_root && index == 0,
                !(with_root && index == 1),
                index == 0,
                transport_kind,
            )
        })
        .collect();
    SnapshotV2MultiBlockDeviceGraph::try_from_parts(
        with_root.then(|| SnapshotV2DeviceKey::block(0)),
        transport_kind,
        records,
    )
    .expect("fixture graph should validate")
}

fn boundary_graph(record_count: usize, with_root: bool) -> SnapshotV2MultiBlockDeviceGraph {
    let records = (0..record_count)
        .map(|index| {
            let index = u32::try_from(index).expect("boundary index should fit");
            fixture_record(
                index,
                with_root && index == 0,
                true,
                false,
                SnapshotV2DeviceTransportKind::Mmio,
            )
        })
        .collect();
    SnapshotV2MultiBlockDeviceGraph::try_from_parts(
        with_root.then(|| SnapshotV2DeviceKey::block(0)),
        SnapshotV2DeviceTransportKind::Mmio,
        records,
    )
    .expect("boundary graph should validate")
}

fn fixture_record(
    index: u32,
    is_root: bool,
    activated: bool,
    with_limiter: bool,
    transport_kind: SnapshotV2DeviceTransportKind,
) -> SnapshotV2MultiBlockDeviceRecord {
    let backing_bytes = 0x20_0000_u64
        .checked_add(u64::from(index) * 513)
        .expect("fixture backing length should fit");
    let config = fixture_config(index, is_root, with_limiter);
    let block = fixture_block(index, backing_bytes, activated, with_limiter);
    let virtio = fixture_virtio(index, backing_bytes, &config, activated);
    let transport = match transport_kind {
        SnapshotV2DeviceTransportKind::Mmio => SnapshotV2DeviceTransport::Mmio(fixture_mmio(index)),
        SnapshotV2DeviceTransportKind::Pci => {
            SnapshotV2DeviceTransport::Pci(fixture_pci(index, is_root))
        }
    };
    SnapshotV2MultiBlockDeviceRecord {
        key: SnapshotV2DeviceKey::block(index),
        config,
        block,
        virtio,
        transport,
    }
}

fn fixture_config(index: u32, is_root: bool, with_limiter: bool) -> SnapshotV2MultiBlockConfig {
    SnapshotV2MultiBlockConfig {
        drive_id: if is_root {
            "rootfs".to_string()
        } else {
            format!("data_{index}")
        },
        partuuid: (index == 0).then(|| "1111-2222".to_string()),
        is_root,
        is_read_only: index.is_multiple_of(2),
        cache_type: if index.is_multiple_of(2) {
            DriveCacheType::Writeback
        } else {
            DriveCacheType::Unsafe
        },
        io_engine: if index.is_multiple_of(2) {
            DriveIoEngine::Sync
        } else {
            DriveIoEngine::Async
        },
        rate_limiter: with_limiter.then(fixture_limiter_config),
        selector: format!("logical-selector-{index}"),
    }
}

fn fixture_limiter_config() -> DriveRateLimiterConfig {
    DriveRateLimiterConfig::new(
        Some(DriveTokenBucketConfig::new(1_000_000, Some(4096), 10)),
        Some(DriveTokenBucketConfig::new(1_000, None, 20)),
    )
}

fn fixture_block(
    index: u32,
    backing_bytes: u64,
    activated: bool,
    with_limiter: bool,
) -> SnapshotV2MultiBlockState {
    let mut device_id = [0_u8; VIRTIO_BLOCK_ID_BYTES as usize];
    for (offset, byte) in device_id.iter_mut().enumerate() {
        *byte = u8::try_from((u64::from(index) + 1 + offset as u64) % 255 + 1)
            .expect("fixture device ID byte should fit");
    }
    let active_queue = activated.then(|| {
        if with_limiter {
            VirtioBlockQueueState::new(7, 6)
        } else {
            VirtioBlockQueueState::new(2, 2)
        }
    });
    let limiter = if with_limiter {
        SnapshotV2BlockLimiterState::from_parts(
            Some(SnapshotV2BlockBucketState::from_parts(
                750_000, 1024, 123_456,
            )),
            Some(SnapshotV2BlockBucketState::from_parts(750, 0, 654_321)),
        )
    } else {
        SnapshotV2BlockLimiterState::from_parts(None, None)
    };
    SnapshotV2MultiBlockState {
        backing_bytes,
        continuation: SnapshotV2BlockState::from_parts(
            backing_bytes >> 9,
            VirtioBlockDeviceId::new(device_id),
            active_queue,
            limiter,
            if with_limiter {
                StorageRetryState::After {
                    remaining_nanos: 99,
                }
            } else {
                StorageRetryState::None
            },
        ),
    }
}

fn fixture_virtio(
    index: u32,
    backing_bytes: u64,
    config: &SnapshotV2MultiBlockConfig,
    activated: bool,
) -> SnapshotV2VirtioState {
    let available_features =
        VirtioBlockConfigSpace::new(backing_bytes, config.is_read_only, config.cache_type)
            .available_features();
    let queue_base = 0x10_0000_u64
        .checked_add(u64::from(index) * 0x10_000)
        .expect("fixture queue base should fit");
    let queue = if activated {
        SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_BLOCK_QUEUE_SIZE,
            VIRTIO_BLOCK_QUEUE_SIZE,
            true,
            GuestAddress::new(queue_base),
            GuestAddress::new(queue_base + 0x2_000),
            GuestAddress::new(queue_base + 0x4_000),
        )
    } else {
        SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_BLOCK_QUEUE_SIZE,
            0,
            false,
            GuestAddress::new(0),
            GuestAddress::new(0),
            GuestAddress::new(0),
        )
    };
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features,
        driver_features: if activated { available_features } else { 0 },
        config_generation: index + 1,
        status: if activated {
            HEALTHY_DRIVER_OK
        } else {
            VIRTIO_DEVICE_STATUS_INIT
        },
        activated,
        queues: vec![queue],
        pending_notifications: if activated { vec![0] } else { Vec::new() },
        interrupt_intents: if activated {
            vec![
                SnapshotV2InterruptIntent::Queue { queue_index: 0 },
                SnapshotV2InterruptIntent::Configuration,
            ]
        } else {
            Vec::new()
        },
    })
}

fn fixture_mmio(index: u32) -> SnapshotV2MmioDeviceState {
    let region = MmioRegion::new(
        MmioRegionId::new(u64::from(index) + 100),
        GuestAddress::new(0xd000_0000 + u64::from(index) * VIRTIO_MMIO_DEVICE_WINDOW_SIZE),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("fixture MMIO region should validate");
    SnapshotV2MmioDeviceState::from_parts(
        index % 2,
        (index + 1) % 2,
        0,
        region,
        GuestInterruptLine::new(index + 32).expect("fixture SPI should validate"),
    )
}

fn fixture_pci(index: u32, is_root: bool) -> SnapshotV2PciDeviceState {
    let device = PCI_FIRST_ENDPOINT_DEVICE
        .checked_add(u8::try_from(index).expect("fixture PCI index should fit"))
        .expect("fixture PCI device should fit");
    let sbdf = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, device, PCI_FUNCTION_ZERO)
        .expect("fixture SBDF should validate");
    let bar_start = PCI_BAR64_START
        .checked_add(
            u64::from(index)
                .checked_mul(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                .expect("fixture BAR product should fit"),
        )
        .expect("fixture BAR start should fit");
    let bar_range =
        GuestMemoryRange::new(GuestAddress::new(bar_start), VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("fixture BAR should validate");
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries: vec![
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 64 + index, 0),
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 96 + index, 1),
        ],
        pending_words: vec![0b10],
        enabled: true,
        function_masked: false,
        config_vector: 0,
        queue_vectors: vec![1],
        pending_transition_observed: true,
    });
    SnapshotV2PciDeviceState::from_parts(SnapshotV2PciDeviceStateParts {
        phase: VirtioPciEndpointPhase::Active,
        origin: if is_root || index == 0 {
            StorageDeviceOrigin::Startup
        } else {
            StorageDeviceOrigin::Runtime
        },
        sbdf,
        bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        bar_address_space: PciBarAddressSpace::Memory64,
        bar_prefetchable: PciBarPrefetchable::No,
        bar_range,
        device_feature_select: index % 2,
        driver_feature_select: (index + 1) % 2,
        queue_select: 0,
        pci_cfg_bar: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        pci_cfg_offset: 0x24 + index,
        pci_cfg_length: 4,
        writable_bytes: vec![
            SnapshotV2PciWritableByte::from_parts(0x04, 0x07),
            SnapshotV2PciWritableByte::from_parts(0x05, 0x80),
            SnapshotV2PciWritableByte::from_parts(0x0c, 0x40),
            SnapshotV2PciWritableByte::from_parts(
                0x3c,
                0x2a_u8
                    .checked_add(u8::try_from(index).expect("fixture index should fit"))
                    .expect("fixture writable value should fit"),
            ),
        ],
        bar_probes: vec![
            SnapshotV2PciBarProbeState::from_parts(0, false),
            SnapshotV2PciBarProbeState::from_parts(1, true),
        ],
        msix,
    })
}

fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert!(hex.len().is_multiple_of(2));
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
        bytes.push(u8::from_str_radix(pair, 16).expect("fixture hex should decode"));
    }
    bytes
}

fn encoded(graph: &SnapshotV2MultiBlockDeviceGraph) -> Vec<u8> {
    graph
        .encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION)
        .expect("fixture graph should encode")
}

fn fixture_cases() -> [(
    SnapshotV2DeviceTransportKind,
    bool,
    &'static str,
    &'static str,
); 4] {
    [
        (
            SnapshotV2DeviceTransportKind::Mmio,
            false,
            "rootless MMIO",
            ROOTLESS_MMIO_FIXTURE_HEX,
        ),
        (
            SnapshotV2DeviceTransportKind::Mmio,
            true,
            "root MMIO",
            ROOT_MMIO_FIXTURE_HEX,
        ),
        (
            SnapshotV2DeviceTransportKind::Pci,
            false,
            "rootless PCI",
            ROOTLESS_PCI_FIXTURE_HEX,
        ),
        (
            SnapshotV2DeviceTransportKind::Pci,
            true,
            "root PCI",
            ROOT_PCI_FIXTURE_HEX,
        ),
    ]
}

#[test]
fn immutable_fixture_matrix_is_canonical_and_round_trips() {
    for (transport, with_root, name, expected) in fixture_cases() {
        let graph = fixture_graph(transport, with_root);
        let actual = encoded(&graph);
        assert_eq!(actual, fixture_bytes(expected), "{name} bytes changed");
        assert_eq!(encoded(&graph), actual, "{name} second encode changed");
        let decoded = SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &actual,
        )
        .expect("immutable fixture should decode");
        assert_eq!(decoded, graph, "{name} decode changed");
        assert_eq!(encoded(&decoded), actual, "{name} re-encode changed");
    }
}

#[test]
fn fixture_matrix_covers_required_semantic_variants() {
    let rootless = fixture_graph(SnapshotV2DeviceTransportKind::Pci, false);
    assert!(rootless.root_key().is_none());
    assert!(rootless.records().iter().all(|record| !record.is_root()));
    assert_eq!(
        rootless.records()[0].transport().kind(),
        SnapshotV2DeviceTransportKind::Pci
    );
    assert!(matches!(
        rootless.records()[1].transport(),
        SnapshotV2DeviceTransport::Pci(state)
            if state.origin() == StorageDeviceOrigin::Runtime
    ));
    assert!(rootless.records()[0].config().is_read_only());
    assert!(!rootless.records()[1].config().is_read_only());
    assert_eq!(
        rootless.records()[0].config().io_engine(),
        DriveIoEngine::Sync
    );
    assert_eq!(
        rootless.records()[1].config().io_engine(),
        DriveIoEngine::Async
    );
    assert_eq!(
        rootless.records()[0].config().cache_type(),
        DriveCacheType::Writeback
    );
    assert_eq!(
        rootless.records()[1].config().cache_type(),
        DriveCacheType::Unsafe
    );
    assert!(rootless.records()[0].config().partuuid().is_some());
    assert!(rootless.records()[1].config().partuuid().is_none());
    assert!(rootless.records()[0].config().rate_limiter().is_some());
    assert!(rootless.records()[1].config().rate_limiter().is_none());
    assert!(rootless.records().iter().all(|record| {
        record.virtio().is_activated() && record.block().continuation().active_queue().is_some()
    }));

    let rooted = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, true);
    assert_eq!(rooted.root_key(), Some(SnapshotV2DeviceKey::block(0)));
    assert!(rooted.records()[0].is_root());
    assert!(!rooted.records()[1].is_root());
    assert!(!rooted.records()[1].virtio().is_activated());
    assert!(
        rooted.records()[1]
            .block()
            .continuation()
            .active_queue()
            .is_none()
    );
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .expect("fixture u16 should exist")
            .try_into()
            .expect("fixture u16 should have exact length"),
    )
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .expect("fixture u64 should exist")
            .try_into()
            .expect("fixture u64 should have exact length"),
    )
}

fn overwrite_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes
        .get_mut(offset..offset + 2)
        .expect("fixture u16 should exist")
        .copy_from_slice(&value.to_le_bytes());
}

fn overwrite_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes
        .get_mut(offset..offset + 4)
        .expect("fixture u32 should exist")
        .copy_from_slice(&value.to_le_bytes());
}

fn overwrite_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes
        .get_mut(offset..offset + 8)
        .expect("fixture u64 should exist")
        .copy_from_slice(&value.to_le_bytes());
}

fn section_directory_offset(bytes: &[u8]) -> usize {
    usize::try_from(read_u64(bytes, 48)).expect("section directory offset should fit")
}

fn section_entry_offset(bytes: &[u8], index: usize) -> usize {
    section_directory_offset(bytes)
        .checked_add(
            index
                .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                .expect("section entry product should fit"),
        )
        .expect("section entry offset should fit")
}

fn section_offset(bytes: &[u8], index: usize) -> usize {
    let entry = section_entry_offset(bytes, index);
    usize::try_from(read_u64(bytes, entry + 16)).expect("section offset should fit")
}

fn section_length(bytes: &[u8], index: usize) -> usize {
    let entry = section_entry_offset(bytes, index);
    usize::try_from(read_u64(bytes, entry + 24)).expect("section length should fit")
}

fn assert_rejected(cases: &[Vec<u8>], context: &str) {
    for (index, bytes) in cases.iter().enumerate() {
        assert!(
            SnapshotV2MultiBlockDeviceGraph::decode(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                bytes,
            )
            .is_err(),
            "{context} case {index} unexpectedly decoded",
        );
    }
}

#[test]
fn profile_context_and_cross_version_dispatch_are_exact() {
    let profile_2 = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false));
    let profile_1 = fixture_bytes(PROFILE_1_MMIO_FIXTURE_HEX);
    assert_eq!(
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &profile_2,
        ),
        Err(SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedVersion)
    );
    assert_eq!(
        fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false)
            .encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2MultiBlockDeviceGraphEncodeError::UnsupportedVersion)
    );
    assert!(
        crate::snapshot_device_v2::SnapshotV2DeviceGraph::decode(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &profile_2,
        )
        .is_err()
    );
    assert!(
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &profile_1,
        )
        .is_err()
    );
    assert!(
        crate::snapshot_device_v2::SnapshotV2DeviceGraph::decode(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &fixture_bytes(PROFILE_1_PCI_FIXTURE_HEX),
        )
        .is_ok()
    );
}

#[test]
fn every_truncated_prefix_trailing_data_and_size_bound_fail_closed() {
    let bytes = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Pci, false));
    for end in 0..bytes.len() {
        assert!(
            SnapshotV2MultiBlockDeviceGraph::decode(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                bytes
                    .get(..end)
                    .expect("truncated fixture prefix should exist"),
            )
            .is_err(),
            "truncated prefix {end} unexpectedly decoded",
        );
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &trailing,
        ),
        Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)
    );

    let exact_cap = vec![0; NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES];
    assert_eq!(
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &exact_cap,
        ),
        Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidMagic)
    );
    let beyond_cap = vec![0; NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES + 1];
    assert_eq!(
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &beyond_cap,
        ),
        Err(SnapshotV2MultiBlockDeviceGraphDecodeError::TooLarge)
    );
}

#[test]
fn hostile_header_and_directory_mutations_fail_closed() {
    let original = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false));
    let mut cases = Vec::new();
    for mutation in [
        (8, 63_u64, 2_usize),
        (10, 1, 2),
        (12, 0, 2),
        (14, 0, 2),
        (14, 65, 2),
        (16, 7, 2),
        (18, 1, 2),
        (20, 1, 4),
        (24, 0, 8),
        (32, 2, 4),
        (36, 1, 4),
        (40, 0, 8),
        (48, 0, 8),
        (56, 0, 8),
    ] {
        let mut bytes = original.clone();
        match mutation.2 {
            2 => overwrite_u16(
                &mut bytes,
                mutation.0,
                u16::try_from(mutation.1).expect("mutation should fit"),
            ),
            4 => overwrite_u32(
                &mut bytes,
                mutation.0,
                u32::try_from(mutation.1).expect("mutation should fit"),
            ),
            8 => overwrite_u64(&mut bytes, mutation.0, mutation.1),
            _ => panic!("closed mutation widths"),
        }
        cases.push(bytes);
    }
    let mut magic = original.clone();
    magic[0] ^= 1;
    cases.push(magic);

    for (relative, width, value) in [(0, 4, 2_u64), (4, 4, 1), (8, 4, 1), (12, 4, 3), (16, 1, 1)] {
        let mut bytes = original.clone();
        let offset = NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES + relative;
        match width {
            1 => bytes[offset] = u8::try_from(value).expect("mutation should fit"),
            4 => overwrite_u32(
                &mut bytes,
                offset,
                u32::try_from(value).expect("mutation should fit"),
            ),
            _ => panic!("closed mutation widths"),
        }
        cases.push(bytes);
    }

    let section_entry = section_entry_offset(&original, 0);
    for (relative, width, value) in [
        (0, 4, 1_u64),
        (4, 2, 2),
        (6, 2, 1),
        (8, 8, 1),
        (16, 8, 1),
        (24, 8, 0),
        (24, 8, 7),
    ] {
        let mut bytes = original.clone();
        let offset = section_entry + relative;
        match width {
            2 => overwrite_u16(
                &mut bytes,
                offset,
                u16::try_from(value).expect("mutation should fit"),
            ),
            4 => overwrite_u32(
                &mut bytes,
                offset,
                u32::try_from(value).expect("mutation should fit"),
            ),
            8 => overwrite_u64(&mut bytes, offset, value),
            _ => panic!("closed mutation widths"),
        }
        cases.push(bytes);
    }
    let mut gap = original.clone();
    overwrite_u64(
        &mut gap,
        section_entry + 16,
        read_u64(&original, section_entry + 16) + 8,
    );
    cases.push(gap);
    let mut overlap = original.clone();
    let second = section_entry_offset(&original, 1);
    overwrite_u64(
        &mut overlap,
        second + 16,
        read_u64(&original, second + 16) - 8,
    );
    cases.push(overlap);
    let mut nonzero_padding = original.clone();
    let config_end = section_offset(&original, 0) + section_length(&original, 0);
    nonzero_padding[config_end - 1] = 1;
    cases.push(nonzero_padding);
    let mut extra_config_padding = original.clone();
    extra_config_padding.splice(config_end..config_end, [0; ALIGNMENT]);
    overwrite_u64(
        &mut extra_config_padding,
        24,
        u64::try_from(original.len() + ALIGNMENT).expect("expanded length should fit"),
    );
    let first_section_entry = section_entry_offset(&extra_config_padding, 0);
    overwrite_u64(
        &mut extra_config_padding,
        first_section_entry + 24,
        u64::try_from(section_length(&original, 0) + ALIGNMENT)
            .expect("expanded section should fit"),
    );
    for section_index in 1..FIXTURE_RECORDS * SECTION_COUNT_PER_RECORD {
        let entry = section_entry_offset(&extra_config_padding, section_index);
        let expanded_offset = read_u64(&extra_config_padding, entry + 16)
            .checked_add(u64::try_from(ALIGNMENT).expect("alignment should fit"))
            .expect("expanded offset should fit");
        overwrite_u64(&mut extra_config_padding, entry + 16, expanded_offset);
    }
    cases.push(extra_config_padding);
    let mut missing_root_header =
        encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Mmio, true));
    overwrite_u32(&mut missing_root_header, 32, 0);
    overwrite_u32(&mut missing_root_header, 36, 0);
    cases.push(missing_root_header);
    assert_rejected(&cases, "hostile framing");
}

#[test]
fn hostile_config_block_and_common_mutations_fail_closed() {
    let original = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false));
    let config = section_offset(&original, 0);
    let block = section_offset(&original, 1);
    let common = section_offset(&original, 2);
    let mut cases = Vec::new();

    for (relative, value) in [(0, 2), (1, 0), (2, 2), (3, 1), (4, 0), (7, 2)] {
        let mut bytes = original.clone();
        bytes[config + relative] = value;
        cases.push(bytes);
    }
    for (relative, value) in [(8, 0), (8, 256), (12, 0), (12, 4097)] {
        let mut bytes = original.clone();
        overwrite_u16(&mut bytes, config + relative, value);
        cases.push(bytes);
    }
    let mut invalid_utf8 = original.clone();
    let drive_length = usize::from(read_u16(&original, config + 8));
    invalid_utf8[config + CONFIG_FIXED_BYTES + drive_length - 1] = 0xff;
    cases.push(invalid_utf8);
    let mut absent_bucket_payload = original.clone();
    absent_bucket_payload[config + 5] = 0;
    cases.push(absent_bucket_payload);
    let mut config_reserved = original.clone();
    config_reserved[config + 14] = 1;
    cases.push(config_reserved);

    let mut duplicate_id = original.clone();
    let second_config = section_offset(&original, SECTION_COUNT_PER_RECORD);
    let first_id = original
        .get(config + CONFIG_FIXED_BYTES..config + CONFIG_FIXED_BYTES + drive_length)
        .expect("first fixture ID should exist")
        .to_vec();
    duplicate_id
        .get_mut(
            second_config + CONFIG_FIXED_BYTES..second_config + CONFIG_FIXED_BYTES + drive_length,
        )
        .expect("second fixture ID should exist")
        .copy_from_slice(&first_id);
    cases.push(duplicate_id);

    for (relative, width, value) in [
        (0, 2, 7_u64),
        (2, 1, 2),
        (5, 1, 9),
        (6, 1, 1),
        (8, 8, 1),
        (44, 1, 1),
        (104, 8, 0),
    ] {
        let mut bytes = original.clone();
        let offset = block + relative;
        match width {
            1 => bytes[offset] = u8::try_from(value).expect("mutation should fit"),
            2 => overwrite_u16(
                &mut bytes,
                offset,
                u16::try_from(value).expect("mutation should fit"),
            ),
            8 => overwrite_u64(&mut bytes, offset, value),
            _ => panic!("closed mutation widths"),
        }
        cases.push(bytes);
    }
    let mut zero_device_id = original.clone();
    zero_device_id
        .get_mut(block + 24..block + 44)
        .expect("block device ID should exist")
        .fill(0);
    cases.push(zero_device_id);

    for (relative, width, value) in [
        (0, 8, 0_u64),
        (8, 8, u64::MAX),
        (20, 4, u64::from(VIRTIO_DEVICE_STATUS_FAILED)),
        (24, 1, 2),
        (25, 1, 1),
        (26, 2, 0),
        (28, 2, 2),
        (30, 2, 3),
        (32, 2, 255),
        (34, 2, 3),
        (36, 1, 0),
        (40, 8, 1),
    ] {
        let mut bytes = original.clone();
        let offset = common + relative;
        match width {
            1 => bytes[offset] = u8::try_from(value).expect("mutation should fit"),
            2 => overwrite_u16(
                &mut bytes,
                offset,
                u16::try_from(value).expect("mutation should fit"),
            ),
            4 => overwrite_u32(
                &mut bytes,
                offset,
                u32::try_from(value).expect("mutation should fit"),
            ),
            8 => overwrite_u64(&mut bytes, offset, value),
            _ => panic!("closed mutation widths"),
        }
        cases.push(bytes);
    }
    assert_rejected(&cases, "hostile semantic section");
}

#[test]
fn hostile_mmio_and_pci_mutations_fail_closed() {
    let mmio = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false));
    let mmio_transport = section_offset(&mmio, 3);
    let mut mmio_cases = Vec::new();
    for (relative, width, value) in [
        (0, 4, 2_u64),
        (4, 4, 2),
        (8, 4, 1),
        (12, 4, 31),
        (16, 8, 0),
        (24, 8, 1),
        (32, 8, VIRTIO_MMIO_DEVICE_WINDOW_SIZE - 1),
        (40, 1, 1),
    ] {
        let mut bytes = mmio.clone();
        let offset = mmio_transport + relative;
        match width {
            1 => bytes[offset] = u8::try_from(value).expect("mutation should fit"),
            4 => overwrite_u32(
                &mut bytes,
                offset,
                u32::try_from(value).expect("mutation should fit"),
            ),
            8 => overwrite_u64(&mut bytes, offset, value),
            _ => panic!("closed mutation widths"),
        }
        mmio_cases.push(bytes);
    }
    assert_rejected(&mmio_cases, "hostile MMIO");

    let pci = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Pci, false));
    let pci_transport = section_offset(&pci, 3);
    let mut pci_cases = Vec::new();
    for (relative, width, value) in [
        (0, 1, 0_u64),
        (1, 1, 0),
        (2, 1, 1),
        (3, 1, 0),
        (4, 1, 1),
        (6, 1, 1),
        (7, 1, 1),
        (8, 2, 1),
        (10, 1, 1),
        (11, 1, 0),
        (12, 1, 1),
        (16, 8, PCI_BAR64_START + 1),
        (24, 8, VIRTIO_PCI_CAPABILITY_BAR_SIZE - 1),
        (32, 4, 2),
        (36, 4, 2),
        (40, 2, 1),
        (42, 2, 3),
        (44, 2, 1),
        (46, 2, 1),
        (48, 2, 2),
        (50, 2, 2),
        (52, 1, 1),
        (64, 1, 2),
        (67, 1, 1),
        (68, 2, 2),
        (72, 2, 5),
        (75, 1, 1),
        (88, 1, 1),
        (90, 1, 1),
        (108, 4, 2),
        (128, 8, 4),
        (136, 2, 2),
        (138, 1, 1),
    ] {
        let mut bytes = pci.clone();
        let offset = pci_transport + relative;
        match width {
            1 => bytes[offset] = u8::try_from(value).expect("mutation should fit"),
            2 => overwrite_u16(
                &mut bytes,
                offset,
                u16::try_from(value).expect("mutation should fit"),
            ),
            4 => overwrite_u32(
                &mut bytes,
                offset,
                u32::try_from(value).expect("mutation should fit"),
            ),
            8 => overwrite_u64(&mut bytes, offset, value),
            _ => panic!("closed mutation widths"),
        }
        pci_cases.push(bytes);
    }
    let mut runtime_root = encoded(&fixture_graph(SnapshotV2DeviceTransportKind::Pci, true));
    let root_transport = section_offset(&runtime_root, 3);
    runtime_root[root_transport + 1] = 2;
    pci_cases.push(runtime_root);
    assert_rejected(&pci_cases, "hostile PCI");
}

fn replace_queue(
    state: &SnapshotV2VirtioState,
    queue: SnapshotV2VirtioQueueState,
) -> SnapshotV2VirtioState {
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: state.available_features(),
        driver_features: state.driver_features(),
        config_generation: state.config_generation(),
        status: state.status(),
        activated: state.is_activated(),
        queues: vec![queue],
        pending_notifications: state.pending_notifications().to_vec(),
        interrupt_intents: state.interrupt_intents().to_vec(),
    })
}

fn replace_mmio(
    state: &SnapshotV2MmioDeviceState,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
) -> SnapshotV2MmioDeviceState {
    SnapshotV2MmioDeviceState::from_parts(
        state.device_feature_select(),
        state.driver_feature_select(),
        state.queue_select(),
        region,
        interrupt_line,
    )
}

fn replace_pci(
    state: &SnapshotV2PciDeviceState,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
) -> SnapshotV2PciDeviceState {
    SnapshotV2PciDeviceState::from_parts(SnapshotV2PciDeviceStateParts {
        phase: state.phase(),
        origin,
        sbdf,
        bar_index: state.bar_index(),
        bar_address_space: state.bar_address_space(),
        bar_prefetchable: state.bar_prefetchable(),
        bar_range,
        device_feature_select: state.device_feature_select(),
        driver_feature_select: state.driver_feature_select(),
        queue_select: state.queue_select(),
        pci_cfg_bar: state.pci_cfg_bar(),
        pci_cfg_offset: state.pci_cfg_offset(),
        pci_cfg_length: state.pci_cfg_length(),
        writable_bytes: state.writable_bytes().to_vec(),
        bar_probes: state.bar_probes().to_vec(),
        msix: state.msix().clone(),
    })
}

fn raw_graph(
    root_key: Option<SnapshotV2DeviceKey>,
    transport_kind: SnapshotV2DeviceTransportKind,
    records: Vec<SnapshotV2MultiBlockDeviceRecord>,
) -> SnapshotV2MultiBlockDeviceGraph {
    SnapshotV2MultiBlockDeviceGraph {
        root_key,
        transport_kind,
        records,
    }
}

fn assert_graph_invalid(graph: &SnapshotV2MultiBlockDeviceGraph, context: &str) {
    assert!(
        validate_graph(graph).is_err(),
        "{context} unexpectedly valid"
    );
    assert_eq!(
        graph.encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph),
        "{context} unexpectedly encoded",
    );
}

#[test]
fn whole_graph_identity_root_transport_and_conflicts_are_closed() {
    let graph = boundary_graph(2, false);

    let mut records = graph.records().to_vec();
    records[1].config.drive_id = records[0].config.drive_id.clone();
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "duplicate public ID",
    );

    let mut records = graph.records().to_vec();
    records[1].key = SnapshotV2DeviceKey::block(9);
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "noncontiguous key",
    );

    let mut records = graph.records().to_vec();
    records[0].config.is_root = true;
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "root role without header",
    );
    assert_graph_invalid(
        &raw_graph(
            Some(SnapshotV2DeviceKey::block(0)),
            SnapshotV2DeviceTransportKind::Mmio,
            graph.records().to_vec(),
        ),
        "root header without role",
    );

    let mut records = graph.records().to_vec();
    records[1].transport = SnapshotV2DeviceTransport::Pci(fixture_pci(1, false));
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "mixed transport",
    );

    let mut records = graph.records().to_vec();
    let first_queue = records[0].virtio.queues()[0];
    records[1].virtio = replace_queue(&records[1].virtio, first_queue);
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "queue overlap",
    );

    let mut records = graph.records().to_vec();
    let SnapshotV2DeviceTransport::Mmio(first) = &records[0].transport else {
        panic!("fixture should use MMIO");
    };
    let SnapshotV2DeviceTransport::Mmio(second) = &records[1].transport else {
        panic!("fixture should use MMIO");
    };
    let same_placement = MmioRegion::new(
        second.region().id(),
        first.region().range().start(),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("overlap region should be structurally valid");
    records[1].transport = SnapshotV2DeviceTransport::Mmio(replace_mmio(
        second,
        same_placement,
        second.interrupt_line(),
    ));
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "placement overlap",
    );

    let mut records = graph.records().to_vec();
    let SnapshotV2DeviceTransport::Mmio(first) = &records[0].transport else {
        panic!("fixture should use MMIO");
    };
    let SnapshotV2DeviceTransport::Mmio(second) = &records[1].transport else {
        panic!("fixture should use MMIO");
    };
    let duplicate_id = MmioRegion::new(
        first.region().id(),
        second.region().range().start(),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("duplicate-ID region should be structurally valid");
    records[1].transport = SnapshotV2DeviceTransport::Mmio(replace_mmio(
        second,
        duplicate_id,
        second.interrupt_line(),
    ));
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "MMIO region-ID conflict",
    );

    let mut records = graph.records().to_vec();
    let SnapshotV2DeviceTransport::Mmio(first) = &records[0].transport else {
        panic!("fixture should use MMIO");
    };
    let SnapshotV2DeviceTransport::Mmio(second) = &records[1].transport else {
        panic!("fixture should use MMIO");
    };
    records[1].transport = SnapshotV2DeviceTransport::Mmio(replace_mmio(
        second,
        second.region(),
        first.interrupt_line(),
    ));
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "MMIO SPI conflict",
    );

    let mut records = graph.records().to_vec();
    let queue_placement = MmioRegion::new(
        MmioRegionId::new(999),
        records[0].virtio.queues()[0].descriptor_table(),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("queue-overlap region should be structurally valid");
    let SnapshotV2DeviceTransport::Mmio(second) = &records[1].transport else {
        panic!("fixture should use MMIO");
    };
    records[1].transport = SnapshotV2DeviceTransport::Mmio(replace_mmio(
        second,
        queue_placement,
        second.interrupt_line(),
    ));
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "queue and placement overlap",
    );
}

#[test]
fn pci_sbdf_conflict_and_runtime_root_are_rejected() {
    let graph = fixture_graph(SnapshotV2DeviceTransportKind::Pci, false);
    let mut records = graph.records().to_vec();
    let SnapshotV2DeviceTransport::Pci(first) = &records[0].transport else {
        panic!("fixture should use PCI");
    };
    let SnapshotV2DeviceTransport::Pci(second) = &records[1].transport else {
        panic!("fixture should use PCI");
    };
    records[1].transport = SnapshotV2DeviceTransport::Pci(replace_pci(
        second,
        second.origin(),
        first.sbdf(),
        second.bar_range(),
    ));
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Pci, records),
        "PCI SBDF conflict",
    );

    let graph = fixture_graph(SnapshotV2DeviceTransportKind::Pci, true);
    let mut records = graph.records().to_vec();
    let SnapshotV2DeviceTransport::Pci(root) = &records[0].transport else {
        panic!("fixture should use PCI");
    };
    records[0].transport = SnapshotV2DeviceTransport::Pci(replace_pci(
        root,
        StorageDeviceOrigin::Runtime,
        root.sbdf(),
        root.bar_range(),
    ));
    assert_graph_invalid(
        &raw_graph(
            Some(SnapshotV2DeviceKey::block(0)),
            SnapshotV2DeviceTransportKind::Pci,
            records,
        ),
        "runtime PCI root",
    );
}

#[test]
fn zero_sixty_four_and_sixty_five_record_boundaries_are_exact() {
    for with_root in [false, true] {
        let singleton = boundary_graph(1, with_root);
        let singleton_bytes = encoded(&singleton);
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::decode(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &singleton_bytes,
            )
            .expect("one-record graph should decode"),
            singleton
        );

        let graph = boundary_graph(
            usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS),
            with_root,
        );
        assert_eq!(
            graph.records().len(),
            usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS)
        );
        let bytes = encoded(&graph);
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::decode(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &bytes,
            )
            .expect("64-record graph should decode"),
            graph
        );
    }

    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, Vec::new()),
        "zero records",
    );
    let records = (0..=usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS))
        .map(|index| {
            fixture_record(
                u32::try_from(index).expect("65-record index should fit"),
                false,
                true,
                false,
                SnapshotV2DeviceTransportKind::Mmio,
            )
        })
        .collect();
    assert_graph_invalid(
        &raw_graph(None, SnapshotV2DeviceTransportKind::Mmio, records),
        "65 records",
    );
}

#[test]
fn checked_worst_case_and_maximum_valid_mmio_graph_fit_the_cap() {
    assert_eq!(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_WORST_CASE_BYTES, 331_840);
    let mut graph = boundary_graph(
        usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS),
        false,
    );
    for (index, record) in graph.records.iter_mut().enumerate() {
        let prefix = format!("{index:02}_");
        record.config.drive_id = format!(
            "{prefix}{}",
            "_".repeat(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES - prefix.len())
        );
        record.config.partuuid =
            Some("p".repeat(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES));
        record.config.selector = "s".repeat(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES);
    }
    validate_graph(&graph).expect("maximum-string graph should validate");
    let bytes = encoded(&graph);
    assert_eq!(bytes.len(), 325_696);
    assert!(bytes.len() <= NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES);
}

struct FailingReserve {
    calls: usize,
    fail_at: usize,
}

impl FailingReserve {
    fn hit(&mut self) -> Result<(), ()> {
        let current = self.calls;
        self.calls = self.calls.checked_add(1).ok_or(())?;
        if current == self.fail_at {
            Err(())
        } else {
            Ok(())
        }
    }
}

impl ReservePolicy for FailingReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        self.hit()?;
        values.try_reserve_exact(additional).map_err(|_| ())
    }

    fn reserve_string(&mut self, value: &mut String, additional: usize) -> Result<(), ()> {
        self.hit()?;
        value.try_reserve_exact(additional).map_err(|_| ())
    }
}

#[test]
fn preflight_precedes_all_owned_allocation_and_each_reserve_failure_is_closed() {
    let graph = fixture_graph(SnapshotV2DeviceTransportKind::Pci, false);
    let bytes = encoded(&graph);

    let mut malformed = bytes.clone();
    overwrite_u16(&mut malformed, 14, 65);
    let mut no_allocation = FailingReserve {
        calls: 0,
        fail_at: 0,
    };
    assert_eq!(
        codec::decode_with_policy(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &malformed,
            &mut no_allocation,
        ),
        Err(SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedProfile)
    );
    assert_eq!(no_allocation.calls, 0);

    let mut successful_decode = FailingReserve {
        calls: 0,
        fail_at: usize::MAX,
    };
    assert_eq!(
        codec::decode_with_policy(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
            &mut successful_decode,
        )
        .expect("unfailing policy should decode"),
        graph
    );
    assert!(successful_decode.calls > 1);
    for fail_at in 0..successful_decode.calls {
        let mut reserve = FailingReserve { calls: 0, fail_at };
        assert_eq!(
            codec::decode_with_policy(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &bytes,
                &mut reserve,
            ),
            Err(SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation),
            "decode reserve ordinal {fail_at} did not fail deterministically",
        );
    }

    let mut encode_success = FailingReserve {
        calls: 0,
        fail_at: usize::MAX,
    };
    assert_eq!(
        codec::encode_with_policy(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &graph,
            &mut encode_success,
        )
        .expect("unfailing policy should encode"),
        bytes
    );
    assert_eq!(encode_success.calls, 1);
    let mut encode_failure = FailingReserve {
        calls: 0,
        fail_at: 0,
    };
    assert_eq!(
        codec::encode_with_policy(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &graph,
            &mut encode_failure,
        ),
        Err(SnapshotV2MultiBlockDeviceGraphEncodeError::Allocation)
    );
}

#[test]
fn diagnostics_and_source_ownership_are_value_redacted() {
    let graph = fixture_graph(SnapshotV2DeviceTransportKind::Pci, true);
    let rendered = [
        format!("{graph:?}"),
        format!("{:?}", graph.records()[0]),
        format!("{:?}", graph.records()[0].config()),
        format!("{:?}", graph.records()[0].block()),
        format!("{:?}", graph.records()[0].virtio()),
        format!("{:?}", graph.records()[0].transport()),
        SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString.to_string(),
        format!(
            "{:?}",
            SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidGraph
        ),
        SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph.to_string(),
    ]
    .join("\n");
    for secret in [
        "rootfs",
        "1111-2222",
        "logical-selector-0",
        "d000",
        "08000040",
        "2097152",
    ] {
        assert!(
            !rendered.contains(secret),
            "diagnostic leaked fixture value {secret}"
        );
    }
    assert!(rendered.contains("<redacted>"));

    let model_source = include_str!("../snapshot_device_v2_5.rs");
    let capture_source = include_str!("capture.rs")
        .split("#[cfg(test)]")
        .next()
        .expect("capture source should have a production prefix");
    let codec_source = include_str!("codec.rs");
    for forbidden in [
        "CaptureReadyBlockDeviceState",
        "BlockFileBackingIdentity",
        "OwnedFd",
        "RawFd",
        "std::fs",
        "std::path",
    ] {
        assert!(!model_source.contains(forbidden));
        assert!(!codec_source.contains(forbidden));
    }
    for forbidden in [
        "async_state.generation()",
        "next_operation_id()",
        "next_sequence()",
        "pressure_pending()",
        "BlockFileBackingIdentity",
        "SharedBlockAsyncRuntime",
        "OwnedFd",
        "RawFd",
    ] {
        assert!(
            !capture_source.contains(forbidden),
            "live conversion retained forbidden authority field {forbidden}",
        );
    }
}
