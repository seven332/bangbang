use super::*;

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::block::{
    BlockFileBacking, DriveConfigInput, PreparedBlockDevice, VIRTIO_BLOCK_QUEUE_SIZES,
    VirtioBlockConfigSpace, VirtioBlockDevice,
};
use crate::memory::{GuestMemory, GuestMemoryLayout};
use crate::message_interrupt::{
    GuestMessage, GuestMessageInterrupt, GuestMessageInterruptRegistry,
    GuestMessageInterruptSignalError,
};
use crate::mmio::{MmioAccessBytes, MmioBus, MmioHandler, MmioRegionId};
use crate::pci::{
    PCI_BAR64_START, PCI_FIRST_ENDPOINT_DEVICE, PciBarAddressSpace, PciBarAllocator, PciBarLease,
    PciConfigFunction,
};
use crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_VERSION;
use crate::storage_capture::{StorageMmioTransportState, StoragePciTransportState};
use crate::virtio::{VirtioDeviceType, VirtioInterruptIntent};
use crate::virtio_mmio::{VirtioMmioRegister, VirtioMmioRegisterHandler};
use crate::virtio_pci::{VIRTIO_PCI_NOTIFICATION_OFFSET, VirtioPciEndpoint, VirtioPciIdentity};

const MMIO_FIXTURE_HEX: &str = include_str!("fixtures/mmio.hex");
const PCI_FIXTURE_HEX: &str = include_str!("fixtures/pci.hex");

fn fixture_config() -> SnapshotV2RootBlockConfig {
    SnapshotV2RootBlockConfig {
        drive_id: "rootfs".to_string(),
        partuuid: Some("1111-2222".to_string()),
        cache_type: DriveCacheType::Writeback,
        rate_limiter: Some(DriveRateLimiterConfig::new(
            Some(DriveTokenBucketConfig::new(1_000_000, Some(4096), 10)),
            Some(DriveTokenBucketConfig::new(1_000, None, 20)),
        )),
        selector: "root-selector".to_string(),
    }
}

fn fixture_block() -> SnapshotV2BlockState {
    let mut device_id = [0_u8; VIRTIO_BLOCK_ID_BYTES as usize];
    for (index, byte) in device_id.iter_mut().enumerate() {
        *byte = u8::try_from(index + 1).expect("fixture block ID index should fit");
    }
    SnapshotV2BlockState {
        capacity_sectors: 2048,
        device_id: VirtioBlockDeviceId::new(device_id),
        active_queue: Some(crate::block::VirtioBlockQueueState::new(7, 6)),
        limiter: SnapshotV2BlockLimiterState {
            bandwidth: Some(SnapshotV2BlockBucketState {
                budget: 750_000,
                remaining_burst: 1024,
                age_nanos: 123_456,
            }),
            ops: Some(SnapshotV2BlockBucketState {
                budget: 750,
                remaining_burst: 0,
                age_nanos: 654_321,
            }),
        },
        retry: StorageRetryState::After {
            remaining_nanos: 99,
        },
    }
}

fn fixture_virtio() -> SnapshotV2VirtioState {
    let features = expected_block_features(DriveCacheType::Writeback);
    SnapshotV2VirtioState {
        available_features: features,
        driver_features: features,
        config_generation: 9,
        status: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK
            | VIRTIO_DEVICE_STATUS_DRIVER_OK,
        activated: true,
        queues: vec![SnapshotV2VirtioQueueState {
            max_size: VIRTIO_BLOCK_QUEUE_SIZE,
            size: VIRTIO_BLOCK_QUEUE_SIZE,
            ready: true,
            descriptor_table: GuestAddress::new(0x1_0000),
            driver_ring: GuestAddress::new(0x2_0000),
            device_ring: GuestAddress::new(0x3_0000),
        }],
        pending_notifications: vec![0],
        interrupt_intents: vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ],
    }
}

fn fixture_graph(transport: SnapshotV2DeviceTransport) -> SnapshotV2DeviceGraph {
    let key = SnapshotV2DeviceKey {
        kind: DEVICE_KIND_BLOCK,
        instance: DEVICE_INSTANCE_ROOT,
    };
    let graph = SnapshotV2DeviceGraph {
        root_key: key,
        record: SnapshotV2DeviceRecord {
            key,
            config: fixture_config(),
            block: fixture_block(),
            virtio: fixture_virtio(),
            transport,
        },
    };
    validate_graph(&graph).expect("fixture graph should validate");
    graph
}

fn mmio_graph() -> SnapshotV2DeviceGraph {
    let region = MmioRegion::new(
        MmioRegionId::new(9),
        GuestAddress::new(0xd000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("fixture MMIO region should validate");
    fixture_graph(SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState {
        device_feature_select: 1,
        driver_feature_select: 0,
        queue_select: 0,
        region,
        interrupt_line: GuestInterruptLine::new(32)
            .expect("fixture interrupt line should validate"),
    }))
}

fn pci_graph() -> SnapshotV2DeviceGraph {
    let sbdf = PciSbdf::new(
        PCI_SEGMENT_ZERO,
        PCI_BUS_ZERO,
        PCI_FIRST_ENDPOINT_DEVICE,
        PCI_FUNCTION_ZERO,
    )
    .expect("fixture SBDF should validate");
    let bar_range = GuestMemoryRange::new(
        GuestAddress::new(PCI_BAR64_START),
        VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    )
    .expect("fixture capability BAR should validate");
    fixture_graph(SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState {
        phase: VirtioPciEndpointPhase::Active,
        origin: StorageDeviceOrigin::Startup,
        sbdf,
        bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        bar_address_space: PciBarAddressSpace::Memory64,
        bar_prefetchable: PciBarPrefetchable::No,
        bar_range,
        device_feature_select: 1,
        driver_feature_select: 0,
        queue_select: 0,
        pci_cfg_bar: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        pci_cfg_offset: 0x24,
        pci_cfg_length: 4,
        writable_bytes: vec![
            SnapshotV2PciWritableByte {
                offset: 0x04,
                value: 0x07,
            },
            SnapshotV2PciWritableByte {
                offset: 0x05,
                value: 0x80,
            },
            SnapshotV2PciWritableByte {
                offset: 0x0c,
                value: 0x40,
            },
            SnapshotV2PciWritableByte {
                offset: 0x3c,
                value: 0x2a,
            },
        ],
        bar_probes: vec![
            SnapshotV2PciBarProbeState {
                index: 0,
                pending: false,
            },
            SnapshotV2PciBarProbeState {
                index: 1,
                pending: true,
            },
        ],
        msix: SnapshotV2PciMsixState {
            entries: vec![
                SnapshotV2PciMsixTableEntry {
                    message_address_low: 0x0800_0040,
                    message_address_high: 0,
                    message_data: 64,
                    vector_control: 0,
                },
                SnapshotV2PciMsixTableEntry {
                    message_address_low: 0x0800_0040,
                    message_address_high: 0,
                    message_data: 65,
                    vector_control: 1,
                },
            ],
            pending_words: vec![0b10],
            enabled: true,
            function_masked: false,
            config_vector: 0,
            queue_vectors: vec![1],
            pending_transition_observed: true,
        },
    }))
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

fn encoded(graph: &SnapshotV2DeviceGraph) -> Vec<u8> {
    graph
        .encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION)
        .expect("fixture graph should encode")
}

fn section_offset(bytes: &[u8], index: usize) -> usize {
    let entry =
        DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET + index * NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
    usize::try_from(u64::from_le_bytes(
        bytes[entry + SECTION_PAYLOAD_OFFSET..entry + SECTION_PAYLOAD_OFFSET + 8]
            .try_into()
            .expect("fixture section offset should exist"),
    ))
    .expect("fixture section offset should fit")
}

fn section_length(bytes: &[u8], index: usize) -> usize {
    let entry =
        DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET + index * NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
    usize::try_from(u64::from_le_bytes(
        bytes[entry + SECTION_LENGTH_OFFSET..entry + SECTION_LENGTH_OFFSET + 8]
            .try_into()
            .expect("fixture section length should exist"),
    ))
    .expect("fixture section length should fit")
}

fn overwrite_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn overwrite_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn overwrite_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn assert_decode_cases_reject(cases: &[Vec<u8>], context: &str) {
    for (index, bytes) in cases.iter().enumerate() {
        assert!(
            SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, bytes,)
                .is_err(),
            "{context} case {index} unexpectedly decoded",
        );
    }
}

#[test]
fn exact_mmio_fixture_is_deterministic_and_round_trips() {
    let graph = mmio_graph();
    let actual = encoded(&graph);
    assert_eq!(actual, fixture_bytes(MMIO_FIXTURE_HEX));
    assert_eq!(encoded(&graph), actual);

    let decoded =
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &actual)
            .expect("exact MMIO fixture should decode");
    assert_eq!(decoded, graph);
    assert_eq!(encoded(&decoded), actual);
}

#[test]
fn exact_pci_fixture_is_deterministic_and_round_trips() {
    let graph = pci_graph();
    let actual = encoded(&graph);
    assert_eq!(actual, fixture_bytes(PCI_FIXTURE_HEX));
    assert_eq!(encoded(&graph), actual);

    let decoded =
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &actual)
            .expect("exact PCI fixture should decode");
    assert_eq!(decoded, graph);
    assert_eq!(encoded(&decoded), actual);
}

#[test]
fn semantic_accessors_and_debug_output_are_stable_and_redacted() {
    let graph = pci_graph();
    assert_eq!(
        graph.compatibility_version(),
        SnapshotFormatVersion::new(2, 4, 0)
    );
    assert_eq!(graph.root_key().kind(), DEVICE_KIND_BLOCK);
    assert_eq!(graph.root_key().instance(), 0);
    assert!(graph.record_is_root());
    assert_eq!(graph.transport_kind(), SnapshotV2DeviceTransportKind::Pci);
    assert_eq!(graph.record().config().drive_id(), "rootfs");
    assert_eq!(graph.record().config().partuuid(), Some("1111-2222"));
    assert_eq!(graph.record().config().selector(), "root-selector");
    assert!(graph.record().config().is_read_only());
    assert_eq!(graph.record().config().io_engine(), DriveIoEngine::Sync);
    assert_eq!(graph.record().block().capacity_sectors(), 2048);
    assert_eq!(graph.record().virtio().queues().len(), 1);
    let SnapshotV2DeviceTransport::Pci(pci) = graph.record().transport() else {
        panic!("fixture should use PCI");
    };
    assert_eq!(pci.writable_bytes().len(), 4);
    assert_eq!(pci.msix().entries().len(), 2);
    assert_eq!(pci.msix().queue_vectors(), [1]);

    let sensitive = ["rootfs", "1111-2222", "root-selector", "134217792", "65"];
    for debug in [
        format!("{graph:?}"),
        format!("{:?}", graph.record()),
        format!("{:?}", graph.record().config()),
        format!("{:?}", graph.record().block()),
        format!("{:?}", graph.record().virtio()),
        format!("{:?}", graph.record().transport()),
        format!("{pci:?}"),
        format!("{:?}", pci.msix()),
    ] {
        assert!(debug.contains("<redacted>"));
        for value in sensitive {
            assert!(!debug.contains(value));
        }
    }
    for error in [
        SnapshotV2DeviceGraphCaptureError::InvalidString.to_string(),
        SnapshotV2DeviceGraphEncodeError::InvalidGraph.to_string(),
        SnapshotV2DeviceGraphDecodeError::InvalidString.to_string(),
    ] {
        for value in sensitive {
            assert!(!error.contains(value));
        }
    }
}

#[test]
fn exact_outer_version_is_required_without_advancing_public_native_v2() {
    assert_eq!(
        NATIVE_V2_SNAPSHOT_VERSION,
        SnapshotFormatVersion::new(2, 3, 0)
    );
    let graph = mmio_graph();
    let bytes = encoded(&graph);
    for version in [
        SnapshotFormatVersion::new(1, 4, 0),
        SnapshotFormatVersion::new(2, 3, 0),
        SnapshotFormatVersion::new(2, 4, 1),
        SnapshotFormatVersion::new(2, 5, 0),
        SnapshotFormatVersion::new(3, 4, 0),
    ] {
        assert_eq!(
            graph.encode(version),
            Err(SnapshotV2DeviceGraphEncodeError::UnsupportedVersion)
        );
        assert_eq!(
            SnapshotV2DeviceGraph::decode(version, &bytes),
            Err(SnapshotV2DeviceGraphDecodeError::UnsupportedVersion)
        );
    }
}

#[test]
fn every_reachable_healthy_pre_activation_status_round_trips_for_both_transports() {
    let statuses = [
        VIRTIO_DEVICE_STATUS_INIT,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK,
    ];
    for template in [mmio_graph(), pci_graph()] {
        for status in statuses {
            let mut graph = template.clone();
            graph.record.block.active_queue = None;
            graph.record.block.retry = StorageRetryState::None;
            graph.record.virtio.status = status;
            graph.record.virtio.activated = false;
            graph.record.virtio.driver_features = if status & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
            {
                0
            } else {
                graph.record.virtio.available_features
            };
            graph.record.virtio.queues[0].size = 0;
            graph.record.virtio.queues[0].ready = false;
            graph.record.virtio.queues[0].descriptor_table = GuestAddress::new(0);
            graph.record.virtio.queues[0].driver_ring = GuestAddress::new(0);
            graph.record.virtio.queues[0].device_ring = GuestAddress::new(0);
            graph.record.virtio.pending_notifications.clear();
            graph.record.virtio.interrupt_intents.clear();

            let bytes = graph
                .encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION)
                .expect("healthy pre-activation state should encode");
            assert_eq!(
                SnapshotV2DeviceGraph::decode(
                    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                    &bytes,
                )
                .expect("healthy pre-activation state should decode"),
                graph
            );
        }
    }
}

#[test]
fn every_truncated_prefix_and_oversized_payload_fails_closed() {
    let bytes = encoded(&pci_graph());
    for end in 0..bytes.len() {
        assert!(
            SnapshotV2DeviceGraph::decode(
                NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &bytes[..end],
            )
            .is_err(),
            "prefix ending at {end} unexpectedly decoded",
        );
    }
    let oversized = vec![0; NATIVE_V2_DEVICE_GRAPH_MAX_BYTES + 1];
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &oversized,),
        Err(SnapshotV2DeviceGraphDecodeError::TooLarge)
    );
}

#[test]
fn hostile_header_record_and_directory_mutations_fail_closed() {
    let original = encoded(&mmio_graph());
    let mut cases = Vec::new();

    let mut bytes = original.clone();
    bytes[HEADER_MAGIC_OFFSET] ^= 1;
    cases.push(bytes);

    for (offset, value) in [
        (HEADER_BYTES_OFFSET, 63_u16),
        (HEADER_PROFILE_OFFSET, 2),
        (HEADER_TRANSPORT_OFFSET, 3),
        (HEADER_RECORD_COUNT_OFFSET, 2),
        (HEADER_SECTION_COUNT_OFFSET, 3),
        (HEADER_RESERVED_OFFSET, 1),
    ] {
        let mut bytes = original.clone();
        overwrite_u16(&mut bytes, offset, value);
        cases.push(bytes);
    }
    for (offset, value) in [
        (HEADER_FLAGS_OFFSET, 1_u32),
        (HEADER_ROOT_KIND_OFFSET, 2),
        (HEADER_ROOT_INSTANCE_OFFSET, 1),
    ] {
        let mut bytes = original.clone();
        overwrite_u32(&mut bytes, offset, value);
        cases.push(bytes);
    }
    for (offset, value) in [
        (HEADER_TOTAL_LENGTH_OFFSET, 1_u64),
        (HEADER_RECORD_DIRECTORY_OFFSET_OFFSET, 0),
        (HEADER_SECTION_DIRECTORY_OFFSET_OFFSET, 0),
        (HEADER_PAYLOAD_OFFSET_OFFSET, 0),
    ] {
        let mut bytes = original.clone();
        overwrite_u64(&mut bytes, offset, value);
        cases.push(bytes);
    }

    let record = DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET;
    for (offset, value) in [
        (record + RECORD_KIND_OFFSET, 2_u32),
        (record + RECORD_INSTANCE_OFFSET, 1),
        (record + RECORD_FIRST_SECTION_OFFSET, 1),
        (record + RECORD_SECTION_COUNT_OFFSET, 3),
    ] {
        let mut bytes = original.clone();
        overwrite_u32(&mut bytes, offset, value);
        cases.push(bytes);
    }
    let mut bytes = original.clone();
    bytes[record + RECORD_RESERVED_OFFSET] = 1;
    cases.push(bytes);

    for index in 0..DEVICE_GRAPH_SECTION_COUNT_USIZE {
        let entry = DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET
            + index * NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
        for (offset, value) in [
            (entry + SECTION_RECORD_INDEX_OFFSET, 1_u32),
            (
                entry + SECTION_PAYLOAD_OFFSET,
                u32::try_from(DEVICE_GRAPH_PAYLOAD_OFFSET + 1)
                    .expect("test payload offset should fit"),
            ),
            (entry + SECTION_LENGTH_OFFSET, 0),
        ] {
            let mut bytes = original.clone();
            if offset == entry + SECTION_RECORD_INDEX_OFFSET {
                overwrite_u32(&mut bytes, offset, value);
            } else {
                overwrite_u64(&mut bytes, offset, u64::from(value));
            }
            cases.push(bytes);
        }
        let mut bytes = original.clone();
        overwrite_u16(&mut bytes, entry + SECTION_KIND_OFFSET, 99);
        cases.push(bytes);
        let mut bytes = original.clone();
        overwrite_u16(&mut bytes, entry + SECTION_FLAGS_OFFSET, 1);
        cases.push(bytes);
        let mut bytes = original.clone();
        bytes[entry + SECTION_RESERVED_OFFSET] = 1;
        cases.push(bytes);
    }

    let mut trailing = original.clone();
    trailing.push(0);
    let trailing_len = u64::try_from(trailing.len()).expect("test length should fit");
    overwrite_u64(&mut trailing, HEADER_TOTAL_LENGTH_OFFSET, trailing_len);
    cases.push(trailing);

    for (index, bytes) in cases.iter().enumerate() {
        assert!(
            SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, bytes,)
                .is_err(),
            "hostile structural case {index} unexpectedly decoded",
        );
    }
}

#[test]
fn section_directory_rejects_gaps_overlaps_unaligned_bounds_and_reordering() {
    let original = encoded(&mmio_graph());
    let first_entry = DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET;
    let second_entry = first_entry + NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
    let last_entry = first_entry
        + (DEVICE_GRAPH_SECTION_COUNT_USIZE - 1) * NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
    let first_length = section_length(&original, 0);
    let second_offset = section_offset(&original, 1);
    let last_length = section_length(&original, 3);
    let mut cases = Vec::new();

    let mut bytes = original.clone();
    overwrite_u64(
        &mut bytes,
        first_entry + SECTION_LENGTH_OFFSET,
        u64::try_from(first_length - DEVICE_GRAPH_ALIGNMENT)
            .expect("test first section length should fit"),
    );
    cases.push(bytes);

    for offset in [
        second_offset - DEVICE_GRAPH_ALIGNMENT,
        second_offset + DEVICE_GRAPH_ALIGNMENT,
        second_offset + 1,
    ] {
        let mut bytes = original.clone();
        overwrite_u64(
            &mut bytes,
            second_entry + SECTION_PAYLOAD_OFFSET,
            u64::try_from(offset).expect("test section offset should fit"),
        );
        cases.push(bytes);
    }

    let mut bytes = original.clone();
    overwrite_u64(
        &mut bytes,
        first_entry + SECTION_LENGTH_OFFSET,
        u64::try_from(first_length + 1).expect("test unaligned length should fit"),
    );
    cases.push(bytes);

    let mut bytes = original.clone();
    overwrite_u64(
        &mut bytes,
        last_entry + SECTION_LENGTH_OFFSET,
        u64::try_from(last_length + DEVICE_GRAPH_ALIGNMENT)
            .expect("test oversized last section should fit"),
    );
    cases.push(bytes);

    let mut bytes = original.clone();
    let first =
        bytes[first_entry..first_entry + NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES].to_vec();
    let second =
        bytes[second_entry..second_entry + NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES].to_vec();
    bytes[first_entry..first_entry + NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES]
        .copy_from_slice(&second);
    bytes[second_entry..second_entry + NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES]
        .copy_from_slice(&first);
    cases.push(bytes);

    assert_decode_cases_reject(&cases, "section directory canonicality");
}

#[test]
fn nonzero_section_padding_is_rejected() {
    let mut bytes = encoded(&mmio_graph());
    let config_offset = section_offset(&bytes, 0);
    let config_length = section_length(&bytes, 0);
    let semantic_length =
        CONFIG_FIXED_BYTES + "rootfs".len() + "1111-2222".len() + "root-selector".len();
    assert!(semantic_length < config_length);
    bytes[config_offset + semantic_length] = 1;
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,),
        Err(SnapshotV2DeviceGraphDecodeError::NonzeroReserved)
    );

    let mut bytes = encoded(&pci_graph());
    let pci_offset = section_offset(&bytes, 3);
    let pci_length = section_length(&bytes, 3);
    bytes[pci_offset + pci_length - 1] = 1;
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,),
        Err(SnapshotV2DeviceGraphDecodeError::NonzeroReserved)
    );
}

#[test]
fn bounded_strings_accept_each_maximum_and_reject_empty_or_one_past() {
    let mut graph = mmio_graph();
    graph.record.config.drive_id = "d".repeat(NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES);
    graph.record.config.partuuid = Some("p".repeat(NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES));
    graph.record.config.selector = "s".repeat(NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES);
    let bytes = encoded(&graph);
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,)
            .expect("maximum strings should decode"),
        graph
    );

    let mut invalid = graph.clone();
    invalid.record.config.drive_id.clear();
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.config.partuuid = Some(String::new());
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.config.selector.clear();
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.config.drive_id.push('d');
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid
        .record
        .config
        .partuuid
        .as_mut()
        .expect("fixture partuuid should exist")
        .push('p');
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph;
    invalid.record.config.selector.push('s');
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
}

#[test]
fn invalid_utf8_and_noncanonical_string_presence_fail_closed() {
    let original = encoded(&mmio_graph());
    let config = section_offset(&original, 0);

    let mut bytes = original.clone();
    bytes[config + CONFIG_FIXED_BYTES] = 0xff;
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,),
        Err(SnapshotV2DeviceGraphDecodeError::InvalidString)
    );

    let mut bytes = original.clone();
    bytes[config + 3] = 0;
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,),
        Err(SnapshotV2DeviceGraphDecodeError::InvalidString)
    );

    let mut invalid = mmio_graph();
    invalid.record.config.drive_id = "bad/id".to_string();
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );

    let mut bytes = original;
    bytes[config + CONFIG_FIXED_BYTES] = b'/';
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,),
        Err(SnapshotV2DeviceGraphDecodeError::InvalidGraph)
    );
}

#[test]
fn hostile_config_block_and_common_section_mutations_fail_closed() {
    let original = encoded(&mmio_graph());
    let config = section_offset(&original, 0);
    let block = section_offset(&original, 1);
    let common = section_offset(&original, 2);
    let mut cases = Vec::new();

    for (offset, value) in [
        (config, 0_u8),
        (config + 1, 2),
        (config + 2, 2),
        (config + 3, 2),
        (config + 4, 0),
        (config + 5, 0),
        (config + 6, 1),
        (config + 40, 2),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        cases.push(bytes);
    }
    for (offset, value) in [
        (config + 8, 0_u16),
        (
            config + 10,
            u16::try_from(NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES + 1)
                .expect("test string bound should fit"),
        ),
        (
            config + 12,
            u16::try_from(NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES + 1)
                .expect("test string bound should fit"),
        ),
    ] {
        let mut bytes = original.clone();
        overwrite_u16(&mut bytes, offset, value);
        cases.push(bytes);
    }
    let mut bytes = original.clone();
    overwrite_u64(&mut bytes, config + 16, 0);
    cases.push(bytes);

    let mut bytes = original.clone();
    overwrite_u16(
        &mut bytes,
        block,
        u16::try_from(VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE + 1)
            .expect("test block config size should fit"),
    );
    cases.push(bytes);
    for (offset, value) in [
        (block + 2, 2_u8),
        (block + 3, 0),
        (block + 4, 0),
        (block + 5, 3),
        (block + 6, 1),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        cases.push(bytes);
    }
    let mut bytes = original.clone();
    bytes[block + 16..block + 16 + VIRTIO_BLOCK_ID_BYTES as usize].fill(0);
    cases.push(bytes);
    let mut bytes = original.clone();
    bytes[block + 2] = 0;
    cases.push(bytes);
    let mut bytes = original.clone();
    overwrite_u64(&mut bytes, block + 48, 1_000_001);
    cases.push(bytes);
    let mut bytes = original.clone();
    overwrite_u64(&mut bytes, block + 96, 0);
    cases.push(bytes);
    let mut bytes = original.clone();
    overwrite_u16(
        &mut bytes,
        block + 40,
        6_u16.wrapping_add(VIRTIO_BLOCK_QUEUE_SIZE).wrapping_add(1),
    );
    cases.push(bytes);

    let mut bytes = original.clone();
    overwrite_u64(&mut bytes, common, 0);
    cases.push(bytes);
    let mut bytes = original.clone();
    overwrite_u64(&mut bytes, common + 8, u64::MAX);
    cases.push(bytes);
    let mut bytes = original.clone();
    overwrite_u32(&mut bytes, common + 20, VIRTIO_DEVICE_STATUS_FAILED);
    cases.push(bytes);
    for (offset, value) in [
        (common + 24, 0_u8),
        (common + 25, 1),
        (common + 36, 2),
        (common + 37, 1),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        cases.push(bytes);
    }
    for (offset, value) in [
        (common + 26, 0_u16),
        (common + 28, 2),
        (common + 30, 3),
        (common + 32, VIRTIO_BLOCK_QUEUE_SIZE - 1),
        (common + 34, 3),
        (common + 64, 1),
    ] {
        let mut bytes = original.clone();
        overwrite_u16(&mut bytes, offset, value);
        cases.push(bytes);
    }
    let mut bytes = original.clone();
    overwrite_u64(&mut bytes, common + 40, 0x1_0001);
    cases.push(bytes);
    let mut bytes = original.clone();
    let first_intent = bytes[common + 66..common + 70].to_vec();
    let second_intent = bytes[common + 70..common + 74].to_vec();
    bytes[common + 66..common + 70].copy_from_slice(&second_intent);
    bytes[common + 70..common + 74].copy_from_slice(&first_intent);
    cases.push(bytes);
    let mut bytes = original.clone();
    bytes[common + 70..common + 74].copy_from_slice(&[INTERRUPT_QUEUE, 0, 0, 0]);
    cases.push(bytes);

    assert_decode_cases_reject(&cases, "config/block/common mutation");
}

#[test]
fn hostile_mmio_and_pci_transport_section_mutations_fail_closed() {
    let mmio_original = encoded(&mmio_graph());
    let mmio = section_offset(&mmio_original, 3);
    let mut mmio_cases = Vec::new();
    for (offset, value) in [(mmio, 2_u32), (mmio + 4, 2), (mmio + 8, 1), (mmio + 12, 31)] {
        let mut bytes = mmio_original.clone();
        overwrite_u32(&mut bytes, offset, value);
        mmio_cases.push(bytes);
    }
    for (offset, value) in [
        (mmio + 16, 0_u64),
        (mmio + 24, 0xd000_0001),
        (mmio + 32, VIRTIO_MMIO_DEVICE_WINDOW_SIZE - 1),
        (mmio + 40, 1),
    ] {
        let mut bytes = mmio_original.clone();
        overwrite_u64(&mut bytes, offset, value);
        mmio_cases.push(bytes);
    }
    assert_decode_cases_reject(&mmio_cases, "MMIO mutation");

    let pci_original = encoded(&pci_graph());
    let pci = section_offset(&pci_original, 3);
    let mut pci_cases = Vec::new();
    for (offset, value) in [
        (pci, 2_u8),
        (pci + 1, 2),
        (pci + 2, 1),
        (pci + 3, 1),
        (pci + 4, 1),
        (pci + 6, 1),
        (pci + 7, 1),
        (pci + 10, 1),
        (pci + 11, 0),
        (pci + 64, 2),
        (pci + 65, 2),
        (pci + 66, 2),
        (pci + 67, 1),
    ] {
        let mut bytes = pci_original.clone();
        bytes[offset] = value;
        pci_cases.push(bytes);
    }
    for (offset, value) in [
        (pci + 8, 1_u16),
        (pci + 42, 3),
        (pci + 44, 1),
        (pci + 46, 1),
        (pci + 48, 2),
        (pci + 50, 2),
        (pci + 68, 2),
    ] {
        let mut bytes = pci_original.clone();
        overwrite_u16(&mut bytes, offset, value);
        pci_cases.push(bytes);
    }
    let mut bytes = pci_original.clone();
    overwrite_u32(&mut bytes, pci + 52, 1);
    pci_cases.push(bytes);
    for (offset, value) in [
        (pci + 16, PCI_BAR64_START + 1),
        (pci + 24, VIRTIO_PCI_CAPABILITY_BAR_SIZE - 1),
    ] {
        let mut bytes = pci_original.clone();
        overwrite_u64(&mut bytes, offset, value);
        pci_cases.push(bytes);
    }
    let writable_start = pci + PCI_FIXED_BYTES;
    let mut bytes = pci_original.clone();
    overwrite_u16(&mut bytes, writable_start, 0x05);
    pci_cases.push(bytes);
    let probes_start = writable_start + PCI_GENERIC_WRITABLE_BYTES.len() * PCI_WRITABLE_ENTRY_BYTES;
    let mut bytes = pci_original.clone();
    bytes[probes_start + PCI_BAR_PROBE_ENTRY_BYTES] = 0;
    pci_cases.push(bytes);
    let entries_start = probes_start + 2 * PCI_BAR_PROBE_ENTRY_BYTES;
    let mut bytes = pci_original.clone();
    overwrite_u32(&mut bytes, entries_start + 12, 2);
    pci_cases.push(bytes);
    let pending_start = entries_start + 2 * PCI_MSIX_ENTRY_BYTES;
    let mut bytes = pci_original.clone();
    overwrite_u64(&mut bytes, pending_start, 0b100);
    pci_cases.push(bytes);
    let vectors_start = pending_start + PCI_PENDING_WORD_BYTES;
    let mut bytes = pci_original.clone();
    overwrite_u16(&mut bytes, vectors_start, 2);
    pci_cases.push(bytes);

    assert_decode_cases_reject(&pci_cases, "PCI mutation");
}

#[test]
fn invalid_block_common_and_mmio_semantics_are_rejected_before_encode() {
    let graph = mmio_graph();

    let mut invalid = graph.clone();
    invalid.record.block.device_id = VirtioBlockDeviceId::new([0; VIRTIO_BLOCK_ID_BYTES as usize]);
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.block.limiter.bandwidth = None;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.block.active_queue = Some(crate::block::VirtioBlockQueueState::new(7, 7));
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.block.active_queue = Some(crate::block::VirtioBlockQueueState::new(
        6_u16.wrapping_add(VIRTIO_BLOCK_QUEUE_SIZE).wrapping_add(1),
        6,
    ));
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.block.retry = StorageRetryState::After { remaining_nanos: 0 };
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.virtio.queues[0].ready = false;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.virtio.driver_features |= 1_u64 << 63;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.virtio.status = VIRTIO_DEVICE_STATUS_FAILED;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.virtio.pending_notifications = vec![1];
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.virtio.interrupt_intents.swap(0, 1);
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    invalid.record.virtio.queues[0].descriptor_table = GuestAddress::new(0x2_0000);
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph.clone();
    let SnapshotV2DeviceTransport::Mmio(mmio) = &mut invalid.record.transport else {
        panic!("fixture should use MMIO");
    };
    mmio.region = MmioRegion::new(
        MmioRegionId::new(9),
        invalid.record.virtio.queues[0].descriptor_table,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("overlapping MMIO range should be structurally valid");
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
    let mut invalid = graph;
    let SnapshotV2DeviceTransport::Mmio(mmio) = &mut invalid.record.transport else {
        panic!("fixture should use MMIO");
    };
    mmio.queue_select = 1;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
}

#[test]
fn invalid_pci_placement_and_msix_semantics_are_rejected() {
    let graph = pci_graph();

    let mut invalid = graph.clone();
    let SnapshotV2DeviceTransport::Pci(pci) = &mut invalid.record.transport else {
        panic!("fixture should use PCI");
    };
    pci.origin = StorageDeviceOrigin::Runtime;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );

    let mut invalid = graph.clone();
    let SnapshotV2DeviceTransport::Pci(pci) = &mut invalid.record.transport else {
        panic!("fixture should use PCI");
    };
    pci.writable_bytes.swap(0, 1);
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );

    let mut invalid = graph.clone();
    let SnapshotV2DeviceTransport::Pci(pci) = &mut invalid.record.transport else {
        panic!("fixture should use PCI");
    };
    pci.bar_probes[1].index = 0;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );

    let mut invalid = graph.clone();
    let SnapshotV2DeviceTransport::Pci(pci) = &mut invalid.record.transport else {
        panic!("fixture should use PCI");
    };
    pci.msix.pending_words[0] = 0b100;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );

    let mut invalid = graph.clone();
    let SnapshotV2DeviceTransport::Pci(pci) = &mut invalid.record.transport else {
        panic!("fixture should use PCI");
    };
    pci.msix.entries[0].vector_control = 2;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );

    let mut invalid = graph;
    let SnapshotV2DeviceTransport::Pci(pci) = &mut invalid.record.transport else {
        panic!("fixture should use PCI");
    };
    pci.msix.queue_vectors[0] = 2;
    assert_eq!(
        invalid.encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION),
        Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph)
    );
}

#[test]
fn every_supported_pci_endpoint_slot_is_canonical() {
    for device in PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE {
        let mut graph = pci_graph();
        let SnapshotV2DeviceTransport::Pci(pci) = &mut graph.record.transport else {
            panic!("fixture should use PCI");
        };
        pci.sbdf = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, device, PCI_FUNCTION_ZERO)
            .expect("endpoint SBDF should validate");
        assert!(
            graph
                .encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION)
                .is_ok(),
            "endpoint device {device} unexpectedly rejected",
        );
    }
}

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(name: &str, len: u64) -> Self {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-snapshot-v2-device-{name}-{}-{sequence}",
            std::process::id(),
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test block backing should create");
        file.set_len(len).expect("test block backing should resize");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn root_restore_memory(ranges: Vec<GuestMemoryRange>) -> GuestMemory {
    let layout = GuestMemoryLayout::new(ranges).expect("restore memory layout should validate");
    let mut memory = GuestMemory::allocate(&layout).expect("restore memory should allocate");
    memory
        .write_slice(&8_u16.to_le_bytes(), GuestAddress::new(0x2_0002))
        .expect("available cursor should write");
    memory
        .write_slice(&6_u16.to_le_bytes(), GuestAddress::new(0x3_0002))
        .expect("used cursor should write");
    memory
}

fn contiguous_root_restore_memory() -> GuestMemory {
    root_restore_memory(vec![
        GuestMemoryRange::new(GuestAddress::new(0), 0x4_0000)
            .expect("restore memory range should validate"),
    ])
}

#[test]
fn root_restore_plan_prepares_pathless_mmio_and_pci_backings() {
    for graph in [mmio_graph(), pci_graph()] {
        let memory = contiguous_root_restore_memory();
        let plan = SnapshotV2RootRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("root restore graph should prepare");
        assert_eq!(plan.selector(), "root-selector");
        assert_eq!(plan.drive_id(), "rootfs");
        assert_eq!(plan.partuuid(), Some("1111-2222"));
        assert_eq!(plan.capacity_sectors(), 2048);
        assert!(!format!("{plan:?}").contains(plan.selector()));
        let drive = plan
            .drive_config()
            .expect("validated root graph should reconstruct controller state");
        assert_eq!(drive.drive_id(), "rootfs");
        assert_eq!(drive.path_on_host(), Some(Path::new("root-selector")));
        assert!(drive.is_root_device());
        assert_eq!(drive.is_read_only(), Some(true));
        assert_eq!(drive.partuuid(), Some("1111-2222"));
        assert_eq!(drive.cache_type(), DriveCacheType::Writeback);
        assert_eq!(drive.io_engine(), Some(DriveIoEngine::Sync));
        assert_eq!(drive.rate_limiter(), fixture_config().rate_limiter());

        let file = TempFile::new("restore-root.img", 2048 << VIRTIO_BLOCK_SECTOR_SHIFT);
        let (backing, _) = BlockFileBacking::open_snapshot_read_only(file.path())
            .expect("restore root backing should open");
        let prepared = plan
            .prepare_backing(backing)
            .expect("restore root backing should prepare");
        assert_eq!(prepared.config_space().capacity_sectors(), 2048);
        assert!(prepared.config_space().is_read_only());
        assert_eq!(
            prepared
                .device()
                .backing()
                .expect("prepared device should retain file backing")
                .len(),
            2048 << VIRTIO_BLOCK_SECTOR_SHIFT
        );
        assert_eq!(prepared.continuation().drive_id(), "rootfs");
        assert_eq!(
            prepared.continuation().retry(),
            StorageRetryState::After {
                remaining_nanos: 99
            }
        );
        assert!(!format!("{prepared:?}").contains("root-selector"));
    }
}

#[test]
fn prepared_root_transport_recaptures_exact_mmio_and_pci_state() {
    for graph in [mmio_graph(), pci_graph()] {
        let expected_virtio = graph.record.virtio.clone();
        let expected_transport = graph.record.transport.clone();
        let memory = contiguous_root_restore_memory();
        let plan = SnapshotV2RootRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("root restore graph should prepare");
        let file = TempFile::new(
            "restore-root-transport.img",
            2048 << VIRTIO_BLOCK_SECTOR_SHIFT,
        );
        let (backing, _) = BlockFileBacking::open_snapshot_read_only(file.path())
            .expect("restore root backing should open");
        let transport = plan
            .prepare_backing(backing)
            .expect("restore root backing should prepare")
            .prepare_transport()
            .expect("root transport should prepare");

        match (transport, expected_transport) {
            (
                PreparedSnapshotV2RootTransport::Mmio(prepared),
                SnapshotV2DeviceTransport::Mmio(expected),
            ) => {
                let (_, _, _, region, interrupt_line, handler) = prepared.into_parts();
                let retained = handler.transport_state();
                assert_eq!(
                    capture_mmio_common(&retained, expected_virtio.available_features())
                        .expect("restored MMIO common state should recapture"),
                    expected_virtio
                );
                assert_eq!(
                    capture_mmio_transport(region, interrupt_line, &retained)
                        .expect("restored MMIO transport should recapture"),
                    expected
                );
            }
            (
                PreparedSnapshotV2RootTransport::Pci(prepared),
                SnapshotV2DeviceTransport::Pci(expected),
            ) => {
                assert_eq!(
                    capture_pci_common(&prepared.retained, expected_virtio.available_features())
                        .expect("restored PCI common state should recapture"),
                    expected_virtio
                );
                assert_eq!(
                    capture_pci_transport(
                        prepared.origin,
                        prepared.sbdf,
                        prepared.bar_range,
                        &prepared.retained,
                    )
                    .expect("restored PCI transport should recapture"),
                    expected
                );
            }
            _ => panic!("prepared root transport kind changed"),
        }
    }
}

#[test]
fn root_restore_plan_rejects_cross_region_and_cursor_mismatches() {
    let mut cross_region = mmio_graph();
    cross_region.record.virtio.queues[0].descriptor_table = GuestAddress::new(0xf800);
    validate_graph(&cross_region).expect("cross-region graph should be structurally valid");
    let memory = root_restore_memory(vec![
        GuestMemoryRange::new(GuestAddress::new(0), 0x1_0000)
            .expect("first restore region should validate"),
        GuestMemoryRange::new(GuestAddress::new(0x1_0000), 0x3_0000)
            .expect("second restore region should validate"),
    ]);
    assert_eq!(
        SnapshotV2RootRestorePlan::prepare(cross_region, &memory, Instant::now())
            .expect_err("one queue range spanning regions must fail"),
        SnapshotV2RootRestorePlanError::QueueMemory
    );

    let mut memory = contiguous_root_restore_memory();
    memory
        .write_slice(&7_u16.to_le_bytes(), GuestAddress::new(0x2_0002))
        .expect("available cursor mismatch should write");
    assert_eq!(
        SnapshotV2RootRestorePlan::prepare(mmio_graph(), &memory, Instant::now())
            .expect_err("retry without one pending descriptor must fail"),
        SnapshotV2RootRestorePlanError::QueueContinuation
    );
}

#[test]
fn root_restore_plan_rejects_backing_geometry_after_pure_validation() {
    let memory = contiguous_root_restore_memory();
    let plan = SnapshotV2RootRestorePlan::prepare(mmio_graph(), &memory, Instant::now())
        .expect("root restore graph should prepare");
    let file = TempFile::new("restore-root-wrong-size.img", 4096);
    let (backing, _) = BlockFileBacking::open_snapshot_read_only(file.path())
        .expect("wrong-size restore root backing should open");
    assert_eq!(
        plan.prepare_backing(backing)
            .expect_err("wrong backing geometry must fail"),
        SnapshotV2RootBackingError::GeometryMismatch
    );
}

fn root_config(path: &Path) -> DriveConfig {
    DriveConfigInput::new("rootfs", "rootfs", path, true)
        .with_is_read_only(true)
        .with_cache_type(DriveCacheType::Writeback)
        .with_io_engine(DriveIoEngine::Sync)
        .with_partuuid("1111-2222")
        .validate()
        .expect("test root configuration should validate")
}

fn activate_mmio_block(
    handler: &mut VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
    features: u64,
) {
    handler
        .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
        .expect("MMIO acknowledge status should write");
    handler
        .write_register(
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        )
        .expect("MMIO driver status should write");
    handler
        .write_register(VirtioMmioRegister::DriverFeaturesSel, 0)
        .expect("MMIO low feature page should select");
    handler
        .write_register(VirtioMmioRegister::DriverFeatures, features as u32)
        .expect("MMIO low driver features should write");
    handler
        .write_register(VirtioMmioRegister::DriverFeaturesSel, 1)
        .expect("MMIO high feature page should select");
    handler
        .write_register(
            VirtioMmioRegister::DriverFeatures,
            u32::try_from(features >> 32).expect("MMIO high feature page should fit"),
        )
        .expect("MMIO high driver features should write");
    handler
        .write_register(VirtioMmioRegister::DeviceFeaturesSel, 1)
        .expect("MMIO device feature page should select");
    handler
        .write_register(
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK,
        )
        .expect("MMIO features-ok status should write");
    for (register, value) in [
        (
            VirtioMmioRegister::QueueNum,
            u32::from(VIRTIO_BLOCK_QUEUE_SIZE),
        ),
        (VirtioMmioRegister::QueueDescLow, 0x1_0000),
        (VirtioMmioRegister::QueueDriverLow, 0x2_0000),
        (VirtioMmioRegister::QueueDeviceLow, 0x3_0000),
        (VirtioMmioRegister::QueueReady, 1),
    ] {
        handler
            .write_register(register, value)
            .expect("MMIO queue state should write");
    }
    handler
        .write_register(
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
                | VIRTIO_DEVICE_STATUS_DRIVER_OK,
        )
        .expect("MMIO driver-ok status should activate");
    handler
        .write_register(VirtioMmioRegister::QueueNotify, 0)
        .expect("MMIO queue notification should retain");
    handler.mark_interrupt_pending(DeviceInterruptKind::Queue);
    handler.mark_interrupt_pending(DeviceInterruptKind::Config);
}

fn capture_ready_mmio(path: &Path) -> CaptureReadyBlockDeviceState {
    let config = root_config(path);
    let prepared = PreparedBlockDevice::from_config_with_backing(&config, None)
        .expect("test block device should prepare");
    let (_, _, config_space, device) = prepared.into_parts();
    let mut handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
        VIRTIO_BLOCK_DEVICE_ID,
        config_space.available_features(),
        &VIRTIO_BLOCK_QUEUE_SIZES,
        config_space,
        device,
    )
    .expect("test MMIO block handler should build");
    activate_mmio_block(&mut handler, config_space.available_features());
    let device = handler
        .capture_block_device_state_at(&config, Instant::now())
        .expect("test MMIO block state should capture");
    let transport = handler.transport_state();
    let region = MmioRegion::new(
        MmioRegionId::new(7),
        GuestAddress::new(0xd000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("test MMIO region should validate");
    CaptureReadyBlockDeviceState::new(
        config,
        StorageTransportState::Mmio(StorageMmioTransportState::new(
            region,
            GuestInterruptLine::new(32).expect("test IRQ should validate"),
            transport,
        )),
        StorageRetryState::None,
        device,
    )
}

fn capture_ready_mmio_with_legacy_device_id(path: &Path) -> CaptureReadyBlockDeviceState {
    let config = root_config(path);
    let backing = BlockFileBacking::open(&config).expect("legacy test backing should open");
    let config_space = VirtioBlockConfigSpace::from_backing(&backing, config.cache_type());
    let legacy_id = VirtioBlockDeviceId::from_bytes(config.drive_id().as_bytes());
    let device = VirtioBlockDevice::new(backing, legacy_id);
    let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
        VIRTIO_BLOCK_DEVICE_ID,
        config_space.available_features(),
        &VIRTIO_BLOCK_QUEUE_SIZES,
        config_space,
        device,
    )
    .expect("legacy MMIO block handler should build");
    let device = handler
        .capture_block_device_state_at(&config, Instant::now())
        .expect("legacy-compatible block state should capture");
    let region = MmioRegion::new(
        MmioRegionId::new(7),
        GuestAddress::new(0xd000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("legacy test MMIO region should validate");
    CaptureReadyBlockDeviceState::new(
        config,
        StorageTransportState::Mmio(StorageMmioTransportState::new(
            region,
            GuestInterruptLine::new(32).expect("legacy test IRQ should validate"),
            handler.transport_state(),
        )),
        StorageRetryState::None,
        device,
    )
}

#[derive(Debug)]
struct TestMessageRoute(GuestMessage);

impl GuestMessageInterrupt for TestMessageRoute {
    fn matches(&self, message: GuestMessage) -> bool {
        self.0 == message
    }

    fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError> {
        if self.matches(message) {
            Ok(())
        } else {
            Err(GuestMessageInterruptSignalError::new(
                "test route rejected an unknown message",
                false,
            ))
        }
    }
}

fn pci_bar_write(
    handler: &mut impl MmioHandler,
    bus: &MmioBus,
    bar: &PciBarLease,
    offset: u64,
    data: &[u8],
) {
    let address = bar
        .range()
        .start()
        .checked_add(offset)
        .expect("test PCI BAR address should not overflow");
    let access = bus
        .lookup(
            address,
            u64::try_from(data.len()).expect("test PCI write width should fit"),
        )
        .expect("test PCI BAR access should resolve");
    handler
        .write(
            access,
            MmioAccessBytes::new(data).expect("test PCI BAR bytes should validate"),
        )
        .expect("test PCI BAR write should succeed");
}

fn activate_pci_block(
    endpoint: &VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
    bar: &PciBarLease,
    features: u64,
) {
    let mut bus = MmioBus::new();
    bus.insert(
        MmioRegionId::new(77),
        bar.range().start(),
        bar.range().size(),
    )
    .expect("test PCI BAR should register");
    let mut handler = endpoint.bar_handler();

    pci_bar_write(&mut handler, &bus, bar, 0x14, &[1]);
    pci_bar_write(&mut handler, &bus, bar, 0x14, &[3]);
    pci_bar_write(&mut handler, &bus, bar, 0x08, &0_u32.to_le_bytes());
    pci_bar_write(
        &mut handler,
        &bus,
        bar,
        0x0c,
        &(features as u32).to_le_bytes(),
    );
    pci_bar_write(&mut handler, &bus, bar, 0x08, &1_u32.to_le_bytes());
    pci_bar_write(
        &mut handler,
        &bus,
        bar,
        0x0c,
        &u32::try_from(features >> 32)
            .expect("test high PCI feature page should fit")
            .to_le_bytes(),
    );
    pci_bar_write(&mut handler, &bus, bar, 0x00, &1_u32.to_le_bytes());
    pci_bar_write(&mut handler, &bus, bar, 0x14, &[11]);
    pci_bar_write(&mut handler, &bus, bar, 0x16, &0_u16.to_le_bytes());
    pci_bar_write(
        &mut handler,
        &bus,
        bar,
        0x18,
        &VIRTIO_BLOCK_QUEUE_SIZE.to_le_bytes(),
    );
    pci_bar_write(&mut handler, &bus, bar, 0x20, &0x1_0000_u32.to_le_bytes());
    pci_bar_write(&mut handler, &bus, bar, 0x28, &0x2_0000_u32.to_le_bytes());
    pci_bar_write(&mut handler, &bus, bar, 0x30, &0x3_0000_u32.to_le_bytes());
    pci_bar_write(&mut handler, &bus, bar, 0x1c, &1_u16.to_le_bytes());
    pci_bar_write(&mut handler, &bus, bar, 0x14, &[15]);
    pci_bar_write(
        &mut handler,
        &bus,
        bar,
        VIRTIO_PCI_NOTIFICATION_OFFSET,
        &0_u16.to_le_bytes(),
    );
    drop(handler);
    let work = endpoint
        .admit_device_work()
        .expect("test PCI device work should admit");
    work.with_core_mut(|core| {
        core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
        core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
    })
    .expect("test PCI interrupt intents should record");
}

struct PciRuntimeFixture {
    config: DriveConfig,
    endpoint: VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
    bar: PciBarLease,
    _allocator: PciBarAllocator,
}

impl PciRuntimeFixture {
    fn new(path: &Path) -> Self {
        let config = root_config(path);
        let prepared = PreparedBlockDevice::from_config_with_backing(&config, None)
            .expect("test block device should prepare");
        let (_, _, config_space, device) = prepared.into_parts();
        let capacity = GuestMemoryRange::new(
            GuestAddress::new(PCI_BAR64_START),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE * 2,
        )
        .expect("test PCI BAR allocator range should validate");
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, capacity);
        let bar = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("test PCI BAR should allocate");
        let routes: Vec<Arc<dyn GuestMessageInterrupt>> = (0..2)
            .map(|index| {
                Arc::new(TestMessageRoute(GuestMessage::new(0x0800_0040, 64 + index)))
                    as Arc<dyn GuestMessageInterrupt>
            })
            .collect();
        let registry =
            GuestMessageInterruptRegistry::new(routes).expect("test PCI routes should validate");
        let endpoint = VirtioPciEndpoint::new(
            VirtioPciIdentity::new(
                VirtioDeviceType::new(VIRTIO_BLOCK_DEVICE_ID)
                    .expect("block virtio type should validate"),
                config_space.available_features(),
            ),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config_space,
            device,
            false,
            &bar,
            registry,
        )
        .expect("test PCI block endpoint should build");
        activate_pci_block(&endpoint, &bar, config_space.available_features());
        Self {
            config,
            endpoint,
            bar,
            _allocator: allocator,
        }
    }

    fn capture(&self) -> CaptureReadyBlockDeviceState {
        let (device, transport) = self
            .endpoint
            .capture_block_device_state_at(&self.config, Instant::now())
            .expect("test PCI block state should capture");
        let sbdf = PciSbdf::new(
            PCI_SEGMENT_ZERO,
            PCI_BUS_ZERO,
            PCI_FIRST_ENDPOINT_DEVICE,
            PCI_FUNCTION_ZERO,
        )
        .expect("test PCI identity should validate");
        CaptureReadyBlockDeviceState::new(
            self.config.clone(),
            StorageTransportState::Pci(StoragePciTransportState::new(
                StorageDeviceOrigin::Startup,
                sbdf,
                self.bar.range(),
                transport,
            )),
            StorageRetryState::None,
            device,
        )
    }
}

fn capture_ready_pci(path: &Path) -> CaptureReadyBlockDeviceState {
    PciRuntimeFixture::new(path).capture()
}

#[test]
fn real_mmio_capture_converts_without_host_identity_and_preserves_stable_id() {
    let file = TempFile::new("mmio.img", 4096);
    let state = capture_ready_mmio(file.path());
    let captured_id = state.device().device_id();
    assert_ne!(
        captured_id,
        VirtioBlockDeviceId::from_bytes(state.drive_id().as_bytes())
    );

    let graph = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &state,
    )
    .expect("real MMIO capture should convert");
    assert_eq!(graph.record().block().device_id(), captured_id);
    assert_eq!(
        graph.record().config().selector(),
        file.path().to_str().unwrap()
    );
    assert_eq!(graph.transport_kind(), SnapshotV2DeviceTransportKind::Mmio);
    assert!(graph.record().virtio().is_activated());
    assert!(graph.record().block().active_queue().is_some());
    assert_eq!(graph.record().virtio().pending_notifications(), [0]);
    assert_eq!(
        graph.record().virtio().interrupt_intents(),
        [
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ]
    );
    let SnapshotV2DeviceTransport::Mmio(mmio) = graph.record().transport() else {
        panic!("converted graph should retain MMIO");
    };
    assert_eq!(mmio.device_feature_select(), 1);
    assert_eq!(mmio.driver_feature_select(), 1);
    let bytes = encoded(&graph);
    assert_eq!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes,)
            .expect("converted MMIO graph should decode"),
        graph
    );
}

#[test]
fn legacy_drive_derived_guest_block_id_remains_admitted_and_independent() {
    let file = TempFile::new("legacy-id.img", 4096);
    let state = capture_ready_mmio_with_legacy_device_id(file.path());
    let legacy_id = VirtioBlockDeviceId::from_bytes(state.drive_id().as_bytes());
    assert_eq!(state.device().device_id(), legacy_id);

    let graph = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &state,
    )
    .expect("legacy-compatible block ID should convert");
    assert_eq!(graph.record().block().device_id(), legacy_id);
    assert_eq!(
        SnapshotV2DeviceGraph::decode(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &encoded(&graph),
        )
        .expect("legacy-compatible graph should decode")
        .record()
        .block()
        .device_id(),
        legacy_id
    );
}

#[test]
fn runtime_conversion_rejects_configuration_limiter_retry_and_mmio_policy_mismatches() {
    let file = TempFile::new("capture-rejections.img", 4096);
    let state = capture_ready_mmio(file.path());

    let configurations = [
        (
            DriveConfigInput::new("rootfs", "rootfs", file.path(), false)
                .with_is_read_only(true)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Sync)
                .validate()
                .expect("non-root test config should validate"),
            SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration,
        ),
        (
            DriveConfigInput::new("rootfs", "rootfs", file.path(), true)
                .with_is_read_only(false)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Sync)
                .validate()
                .expect("writable test config should validate"),
            SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration,
        ),
        (
            DriveConfigInput::new("rootfs", "rootfs", file.path(), true)
                .with_is_read_only(true)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Async)
                .validate()
                .expect("Async test config should validate"),
            SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration,
        ),
        (
            DriveConfigInput::new("rootfs", "rootfs", file.path(), true)
                .with_is_read_only(true)
                .with_cache_type(DriveCacheType::Unsafe)
                .with_io_engine(DriveIoEngine::Sync)
                .validate()
                .expect("cache-mismatch test config should validate"),
            SnapshotV2DeviceGraphCaptureError::InconsistentBlockState,
        ),
        (
            DriveConfigInput::new("rootfs", "rootfs", file.path(), true)
                .with_is_read_only(true)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Sync)
                .with_rate_limiter(DriveRateLimiterConfig::new(
                    Some(DriveTokenBucketConfig::new(100, None, 1)),
                    None,
                ))
                .validate()
                .expect("limiter-mismatch test config should validate"),
            SnapshotV2DeviceGraphCaptureError::InconsistentBlockState,
        ),
    ];
    for (config, expected) in configurations {
        let invalid = CaptureReadyBlockDeviceState::new(
            config,
            state.transport().clone(),
            state.retry(),
            *state.device(),
        );
        assert_eq!(
            SnapshotV2DeviceGraph::from_capture_ready_root(
                NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &invalid,
            ),
            Err(expected)
        );
    }

    let invalid_retry = CaptureReadyBlockDeviceState::new(
        state.config().clone(),
        state.transport().clone(),
        StorageRetryState::Immediate,
        *state.device(),
    );
    assert_eq!(
        SnapshotV2DeviceGraph::from_capture_ready_root(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid_retry,
        ),
        Err(SnapshotV2DeviceGraphCaptureError::InconsistentBlockState)
    );

    let StorageTransportState::Mmio(mmio) = state.transport() else {
        panic!("test capture should use MMIO");
    };
    let transport = mmio.transport();
    let false_policy = VirtioMmioTransportState::from_parts(
        *transport.device_registers(),
        transport.queue_select(),
        transport.queues().to_vec(),
        transport.pending_notifications().to_vec(),
        transport.interrupt_status(),
        transport.is_device_activated(),
        false,
    );
    let invalid_policy = CaptureReadyBlockDeviceState::new(
        state.config().clone(),
        StorageTransportState::Mmio(StorageMmioTransportState::new(
            mmio.region(),
            mmio.interrupt_line(),
            false_policy,
        )),
        state.retry(),
        *state.device(),
    );
    assert_eq!(
        SnapshotV2DeviceGraph::from_capture_ready_root(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid_policy,
        ),
        Err(SnapshotV2DeviceGraphCaptureError::InvalidMmioState)
    );
}

#[test]
fn real_pci_capture_uses_checked_configuration_profile_and_preserves_stable_id() {
    let file = TempFile::new("pci.img", 8192);
    let state = capture_ready_pci(file.path());
    let captured_id = state.device().device_id();
    assert_ne!(
        captured_id,
        VirtioBlockDeviceId::from_bytes(state.drive_id().as_bytes())
    );

    let graph = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &state,
    )
    .expect("real PCI capture should convert");
    assert_eq!(graph.record().block().device_id(), captured_id);
    assert_eq!(graph.transport_kind(), SnapshotV2DeviceTransportKind::Pci);
    let SnapshotV2DeviceTransport::Pci(pci) = graph.record().transport() else {
        panic!("converted graph should retain PCI");
    };
    assert!(graph.record().virtio().is_activated());
    assert!(graph.record().block().active_queue().is_some());
    assert_eq!(graph.record().virtio().pending_notifications(), [0]);
    assert_eq!(
        graph.record().virtio().interrupt_intents(),
        [
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ]
    );
    assert_eq!(pci.device_feature_select(), 1);
    assert_eq!(pci.driver_feature_select(), 1);
    assert_eq!(
        pci.writable_bytes()
            .iter()
            .map(|byte| byte.offset())
            .collect::<Vec<_>>(),
        [0x04, 0x05, 0x0c, 0x3c]
    );
    assert_eq!(
        pci.bar_probes()
            .iter()
            .map(|probe| probe.index())
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(pci.msix().entries().len(), 2);
    assert_eq!(
        encoded(
            &SnapshotV2DeviceGraph::decode(
                NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &encoded(&graph),
            )
            .expect("converted PCI graph should decode")
        ),
        encoded(&graph)
    );
}

#[test]
fn real_pci_capture_preserves_inert_guest_selectors_exactly() {
    let file = TempFile::new("pci-selectors.img", 8192);
    let fixture = PciRuntimeFixture::new(file.path());
    let mut bus = MmioBus::new();
    bus.insert(
        MmioRegionId::new(78),
        fixture.bar.range().start(),
        fixture.bar.range().size(),
    )
    .expect("test PCI BAR should register");
    let mut handler = fixture.endpoint.bar_handler();
    pci_bar_write(
        &mut handler,
        &bus,
        &fixture.bar,
        0x00,
        &u32::MAX.to_le_bytes(),
    );
    pci_bar_write(
        &mut handler,
        &bus,
        &fixture.bar,
        0x08,
        &0x8000_0002_u32.to_le_bytes(),
    );
    pci_bar_write(
        &mut handler,
        &bus,
        &fixture.bar,
        0x16,
        &u16::MAX.to_le_bytes(),
    );
    drop(handler);

    let pci_cfg_cap_offset = fixture
        .endpoint
        .transport_state()
        .expect("test PCI transport should capture")
        .pci_cfg_cap_offset();
    let mut configuration = fixture.endpoint.config_function();
    configuration
        .write_config(
            pci_cfg_cap_offset
                .checked_add(4)
                .expect("test PCI selector BAR should fit"),
            &[5],
        )
        .expect("test PCI selector BAR should write");
    configuration
        .write_config(
            pci_cfg_cap_offset
                .checked_add(8)
                .expect("test PCI selector offset should fit"),
            &u32::MAX.to_le_bytes(),
        )
        .expect("test PCI selector offset should write");
    configuration
        .write_config(
            pci_cfg_cap_offset
                .checked_add(12)
                .expect("test PCI selector length should fit"),
            &3_u32.to_le_bytes(),
        )
        .expect("test PCI selector length should write");
    drop(configuration);

    let graph = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture.capture(),
    )
    .expect("reachable inert PCI selectors should convert");
    let SnapshotV2DeviceTransport::Pci(pci) = graph.record().transport() else {
        panic!("converted graph should retain PCI");
    };
    assert_eq!(pci.device_feature_select(), u32::MAX);
    assert_eq!(pci.driver_feature_select(), 0x8000_0002);
    assert_eq!(pci.queue_select(), u16::MAX);
    assert_eq!(pci.pci_cfg_bar(), 5);
    assert_eq!(pci.pci_cfg_offset(), u32::MAX);
    assert_eq!(pci.pci_cfg_length(), 3);
    assert_eq!(
        SnapshotV2DeviceGraph::decode(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &encoded(&graph),
        )
        .expect("reachable inert PCI selectors should decode"),
        graph
    );
}

#[test]
fn runtime_conversion_rejects_noncanonical_pci_origin_identity_and_bar() {
    let file = TempFile::new("pci-placement-rejections.img", 8192);
    let state = capture_ready_pci(file.path());
    let StorageTransportState::Pci(pci) = state.transport() else {
        panic!("test capture should use PCI");
    };
    let host_bridge = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, 0, PCI_FUNCTION_ZERO)
        .expect("host-bridge SBDF should be structurally valid");
    let unaligned_bar = GuestMemoryRange::new(
        GuestAddress::new(PCI_BAR64_START + 1),
        VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    )
    .expect("unaligned BAR should be a valid raw range");
    for transport in [
        StoragePciTransportState::new(
            StorageDeviceOrigin::Runtime,
            pci.sbdf(),
            pci.bar_range(),
            pci.transport().clone(),
        ),
        StoragePciTransportState::new(
            StorageDeviceOrigin::Startup,
            host_bridge,
            pci.bar_range(),
            pci.transport().clone(),
        ),
        StoragePciTransportState::new(
            StorageDeviceOrigin::Startup,
            pci.sbdf(),
            unaligned_bar,
            pci.transport().clone(),
        ),
    ] {
        let invalid = CaptureReadyBlockDeviceState::new(
            state.config().clone(),
            StorageTransportState::Pci(transport),
            state.retry(),
            *state.device(),
        );
        assert_eq!(
            SnapshotV2DeviceGraph::from_capture_ready_root(
                NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &invalid,
            ),
            Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState)
        );
    }
}

#[test]
fn pci_config_data_aperture_scratch_is_not_artifact_state() {
    let file = TempFile::new("pci-scratch.img", 8192);
    let fixture = PciRuntimeFixture::new(file.path());
    let transport_before = fixture
        .endpoint
        .transport_state()
        .expect("test PCI transport should capture");
    let before = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture.capture(),
    )
    .expect("test PCI graph should convert");

    let mut configuration = fixture.endpoint.config_function();
    configuration
        .write_config(
            transport_before
                .pci_cfg_cap_offset()
                .checked_add(16)
                .expect("test PCI data aperture should fit"),
            &[0xde, 0xad, 0xbe, 0xef],
        )
        .expect("test PCI data aperture should write");
    drop(configuration);

    let transport_after = fixture
        .endpoint
        .transport_state()
        .expect("updated PCI transport should capture");
    assert_ne!(transport_after, transport_before);
    let after = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture.capture(),
    )
    .expect("updated PCI graph should convert");
    assert_eq!(after, before);
    assert_eq!(encoded(&after), encoded(&before));
}

#[test]
fn real_pci_generic_writes_and_bar_probe_state_are_captured_canonically() {
    let file = TempFile::new("pci-guest-state.img", 8192);
    let fixture = PciRuntimeFixture::new(file.path());
    let mut configuration = fixture.endpoint.config_function();
    for (offset, value) in [(0x04, 0x07_u8), (0x05, 0x80), (0x0c, 0x40), (0x3c, 0x2a)] {
        configuration
            .write_config(offset, &[value])
            .expect("test generic PCI byte should write");
    }
    configuration
        .write_config(0x10, &[u8::MAX; 4])
        .expect("test PCI BAR probe should arm");
    drop(configuration);

    let graph = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture.capture(),
    )
    .expect("test PCI graph should convert");
    let SnapshotV2DeviceTransport::Pci(pci) = graph.record().transport() else {
        panic!("converted graph should retain PCI");
    };
    assert_eq!(
        pci.writable_bytes()
            .iter()
            .map(|byte| (byte.offset(), byte.value()))
            .collect::<Vec<_>>(),
        [(0x04, 0x07), (0x05, 0x80), (0x0c, 0x40), (0x3c, 0x2a)]
    );
    assert_eq!(
        pci.bar_probes()
            .iter()
            .map(|probe| (probe.index(), probe.pending()))
            .collect::<Vec<_>>(),
        [(0, true), (1, false)]
    );

    let recaptured = SnapshotV2DeviceGraph::from_capture_ready_root(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture.capture(),
    )
    .expect("side-effect-free PCI graph recapture should convert");
    assert_eq!(recaptured, graph);
}
