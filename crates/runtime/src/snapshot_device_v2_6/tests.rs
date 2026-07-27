use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::codec;
use super::*;

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::PciSbdf;
use crate::pmem::{PmemRateLimiterConfig, PmemTokenBucketConfig};
use crate::snapshot_device_v2::{
    SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState, SnapshotV2PciBarProbeState,
    SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts, SnapshotV2PciMsixState,
    SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry, SnapshotV2PciWritableByte,
    SnapshotV2VirtioStateParts,
};
use crate::snapshot_device_v2_5::{
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2MultiBlockDeviceGraph,
};
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE};
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciEndpointPhase,
};
use crate::{
    pci::{
        PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
        PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable,
    },
    snapshot_device_v2_5::codec::ReservePolicy,
};

const PROFILE_2_ROOT_MMIO_HEX: &str =
    include_str!("../snapshot_device_v2_5/fixtures/root-mmio.hex");
const PROFILE_2_ROOTLESS_MMIO_HEX: &str =
    include_str!("../snapshot_device_v2_5/fixtures/rootless-mmio.hex");
const PROFILE_2_ROOT_PCI_HEX: &str = include_str!("../snapshot_device_v2_5/fixtures/root-pci.hex");
const PROFILE_2_ROOTLESS_PCI_HEX: &str =
    include_str!("../snapshot_device_v2_5/fixtures/rootless-pci.hex");
const BLOCK_ROOT_MMIO_HEX: &str = include_str!("fixtures/block-root-mmio.hex");
const BLOCK_ROOTLESS_PCI_HEX: &str = include_str!("fixtures/block-rootless-pci.hex");
const PMEM_ROOT_MMIO_HEX: &str = include_str!("fixtures/pmem-root-mmio.hex");
const PMEM_ROOTLESS_PCI_HEX: &str = include_str!("fixtures/pmem-rootless-pci.hex");
const MIXED_BLOCK_ROOT_MMIO_HEX: &str = include_str!("fixtures/mixed-block-root-mmio.hex");
const MIXED_PMEM_ROOT_PCI_HEX: &str = include_str!("fixtures/mixed-pmem-root-pci.hex");

const HEALTHY_DRIVER_OK: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
    | VIRTIO_DEVICE_STATUS_DRIVER
    | VIRTIO_DEVICE_STATUS_FEATURES_OK
    | VIRTIO_DEVICE_STATUS_DRIVER_OK;
static NEXT_RESTORE_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

struct RestoreTempBacking {
    path: PathBuf,
}

impl RestoreTempBacking {
    fn new(name: &str, len: u64) -> Self {
        let sequence = NEXT_RESTORE_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-profile-3-restore-{name}-{}-{sequence}",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("restore backing should create");
        file.set_len(len).expect("restore backing should resize");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RestoreTempBacking {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).expect("fixture pair should be UTF-8"),
                16,
            )
            .expect("fixture pair should decode")
        })
        .collect()
}

fn block_records(
    transport: SnapshotV2DeviceTransportKind,
    with_root: bool,
) -> Vec<SnapshotV2MultiBlockDeviceRecord> {
    let fixture = match (transport, with_root) {
        (SnapshotV2DeviceTransportKind::Mmio, false) => PROFILE_2_ROOTLESS_MMIO_HEX,
        (SnapshotV2DeviceTransportKind::Mmio, true) => PROFILE_2_ROOT_MMIO_HEX,
        (SnapshotV2DeviceTransportKind::Pci, false) => PROFILE_2_ROOTLESS_PCI_HEX,
        (SnapshotV2DeviceTransportKind::Pci, true) => PROFILE_2_ROOT_PCI_HEX,
    };
    SnapshotV2MultiBlockDeviceGraph::decode(
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &decode_hex(fixture),
    )
    .expect("profile-2 fixture should decode")
    .records()
    .to_vec()
}

fn limiter_config() -> PmemRateLimiterConfig {
    PmemRateLimiterConfig::new(
        Some(PmemTokenBucketConfig::new(1_000_000, Some(4096), 10)),
        Some(PmemTokenBucketConfig::new(1_000, None, 20)),
    )
}

fn fixture_pmem_record(
    instance: u32,
    is_root: bool,
    transport_kind: SnapshotV2DeviceTransportKind,
    transport_slot: u32,
) -> SnapshotV2PmemDeviceRecord {
    let config = SnapshotV2PmemConfig::try_new(
        format!("pmem_{instance}"),
        is_root,
        instance.is_multiple_of(2),
        Some(limiter_config()),
        format!("pmem-selector-{instance}"),
    )
    .expect("fixture pmem config should validate");
    let file_bytes = VIRTIO_PMEM_ALIGNMENT + 513 + u64::from(instance);
    let mapped_bytes = VIRTIO_PMEM_ALIGNMENT * 2;
    let guest_start = 0x4_0000_0000 + u64::from(instance) * 0x400_000;
    let guest_range = GuestMemoryRange::new(GuestAddress::new(guest_start), mapped_bytes)
        .expect("fixture pmem range should validate");
    let pmem = SnapshotV2PmemState::try_new(
        file_bytes,
        mapped_bytes,
        guest_range,
        VirtioPmemConfigSpace::new(guest_start, mapped_bytes),
        Some(VirtioPmemQueueState::new(7, 6)),
        SnapshotV2PmemLimiterState::new(
            Some(SnapshotV2PmemBucketState::new(750_000, 1024, 123_456)),
            Some(SnapshotV2PmemBucketState::new(750, 0, 654_321)),
        ),
        true,
        StorageRetryState::After {
            remaining_nanos: 99,
        },
    )
    .expect("fixture pmem state should validate");
    let queue_base = 0x20_0000 + u64::from(instance) * 0x10_000;
    let virtio = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: VIRTIO_MMIO_VERSION_1_FEATURE,
        driver_features: VIRTIO_MMIO_VERSION_1_FEATURE,
        config_generation: instance + 10,
        status: HEALTHY_DRIVER_OK,
        activated: true,
        queues: vec![SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_PMEM_QUEUE_SIZE,
            VIRTIO_PMEM_QUEUE_SIZE,
            true,
            GuestAddress::new(queue_base),
            GuestAddress::new(queue_base + 0x2_000),
            GuestAddress::new(queue_base + 0x4_000),
        )],
        pending_notifications: vec![0],
        interrupt_intents: vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ],
    });
    let transport = match transport_kind {
        SnapshotV2DeviceTransportKind::Mmio => {
            let region = MmioRegion::new(
                MmioRegionId::new(u64::from(transport_slot) + 100),
                GuestAddress::new(
                    0xd000_0000 + u64::from(transport_slot) * VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                ),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("fixture MMIO range should validate");
            SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                instance % 2,
                (instance + 1) % 2,
                0,
                region,
                GuestInterruptLine::new(40 + transport_slot)
                    .expect("fixture interrupt line should validate"),
            ))
        }
        SnapshotV2DeviceTransportKind::Pci => {
            let device = PCI_FIRST_ENDPOINT_DEVICE
                .checked_add(
                    u8::try_from(transport_slot).expect("fixture transport slot should fit"),
                )
                .expect("fixture PCI device should fit");
            let sbdf = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, device, PCI_FUNCTION_ZERO)
                .expect("fixture SBDF should validate");
            let bar_start = PCI_BAR64_START
                .checked_add(
                    u64::from(transport_slot)
                        .checked_mul(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                        .expect("fixture BAR product should fit"),
                )
                .expect("fixture BAR start should fit");
            let bar_range =
                GuestMemoryRange::new(GuestAddress::new(bar_start), VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                    .expect("fixture BAR should validate");
            let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
                entries: vec![
                    SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 72 + instance, 0),
                    SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 104 + instance, 1),
                ],
                pending_words: vec![0b10],
                enabled: true,
                function_masked: false,
                config_vector: 0,
                queue_vectors: vec![1],
                pending_transition_observed: true,
            });
            SnapshotV2DeviceTransport::Pci(SnapshotV2PciDeviceState::from_parts(
                SnapshotV2PciDeviceStateParts {
                    phase: VirtioPciEndpointPhase::Active,
                    origin: if is_root || transport_slot == 0 {
                        StorageDeviceOrigin::Startup
                    } else {
                        StorageDeviceOrigin::Runtime
                    },
                    sbdf,
                    bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
                    bar_address_space: PciBarAddressSpace::Memory64,
                    bar_prefetchable: PciBarPrefetchable::No,
                    bar_range,
                    device_feature_select: instance % 2,
                    driver_feature_select: (instance + 1) % 2,
                    queue_select: 0,
                    pci_cfg_bar: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
                    pci_cfg_offset: 0x30 + instance,
                    pci_cfg_length: 4,
                    writable_bytes: vec![
                        SnapshotV2PciWritableByte::from_parts(0x04, 0x07),
                        SnapshotV2PciWritableByte::from_parts(0x05, 0x80),
                        SnapshotV2PciWritableByte::from_parts(0x0c, 0x40),
                        SnapshotV2PciWritableByte::from_parts(0x3c, 0x32),
                    ],
                    bar_probes: vec![
                        SnapshotV2PciBarProbeState::from_parts(0, false),
                        SnapshotV2PciBarProbeState::from_parts(1, true),
                    ],
                    msix,
                },
            ))
        }
    };
    SnapshotV2PmemDeviceRecord::try_new(instance, config, pmem, virtio, transport)
        .expect("fixture pmem record should validate")
}

fn fixture_graph(
    transport: SnapshotV2DeviceTransportKind,
    block_count: usize,
    pmem_count: usize,
    root_kind: Option<u32>,
) -> SnapshotV2StorageDeviceGraph {
    let block_root = root_kind == Some(DEVICE_KIND_BLOCK);
    let pmem_root = root_kind == Some(DEVICE_KIND_PMEM);
    let mut blocks = if block_count == 0 {
        Vec::new()
    } else {
        block_records(transport, block_root)
    };
    blocks.truncate(block_count);
    let pmem = (0..pmem_count)
        .map(|index| {
            fixture_pmem_record(
                u32::try_from(index).expect("fixture index should fit"),
                pmem_root && index == 0,
                transport,
                u32::try_from(block_count + index).expect("fixture slot should fit"),
            )
        })
        .collect();
    let root_key = match root_kind {
        Some(DEVICE_KIND_BLOCK) => Some(SnapshotV2DeviceKey::block(0)),
        Some(DEVICE_KIND_PMEM) => Some(SnapshotV2DeviceKey::pmem(0)),
        None => None,
        Some(_) => panic!("unsupported fixture root kind"),
    };
    SnapshotV2StorageDeviceGraph::try_from_parts(root_key, transport, blocks, pmem)
        .expect("fixture graph should validate")
}

fn encoded(graph: &SnapshotV2StorageDeviceGraph) -> Vec<u8> {
    graph
        .encode(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION)
        .expect("fixture graph should encode")
}

fn restore_memory_for(graph: &SnapshotV2StorageDeviceGraph) -> GuestMemory {
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(GuestAddress::new(0), 0x80_0000)
            .expect("restore memory range should validate"),
    ])
    .expect("restore memory layout should validate");
    let mut memory = GuestMemory::allocate(&layout).expect("restore memory should allocate");
    for record in graph.block_records() {
        let Some(cursor) = record.block().continuation().active_queue() else {
            continue;
        };
        let queue = record
            .virtio()
            .queues()
            .first()
            .expect("block queue should exist");
        let available_index = if record.block().continuation().retry() == StorageRetryState::None {
            cursor.next_available()
        } else {
            cursor.next_available().wrapping_add(1)
        };
        memory
            .write_slice(
                &available_index.to_le_bytes(),
                GuestAddress::new(queue.driver_ring().raw_value() + 2),
            )
            .expect("block available index should write");
        memory
            .write_slice(
                &cursor.next_used().to_le_bytes(),
                GuestAddress::new(queue.device_ring().raw_value() + 2),
            )
            .expect("block used index should write");
    }
    for record in graph.pmem_records() {
        let Some(cursor) = record.pmem().active_queue() else {
            continue;
        };
        let queue = record
            .virtio()
            .queues()
            .first()
            .expect("pmem queue should exist");
        let available_index = if record.pmem().retry() == StorageRetryState::None {
            cursor.next_available()
        } else {
            cursor.next_available().wrapping_add(1)
        };
        memory
            .write_slice(
                &available_index.to_le_bytes(),
                GuestAddress::new(queue.driver_ring().raw_value() + 2),
            )
            .expect("pmem available index should write");
        memory
            .write_slice(
                &cursor.next_used().to_le_bytes(),
                GuestAddress::new(queue.device_ring().raw_value() + 2),
            )
            .expect("pmem used index should write");
    }
    memory
}

fn restore_backings(
    graph: &SnapshotV2StorageDeviceGraph,
) -> (
    Vec<RestoreTempBacking>,
    Vec<crate::block::BlockFileBacking>,
    Vec<crate::pmem::PmemFileBacking>,
) {
    let mut files = Vec::new();
    let mut blocks = Vec::new();
    let mut pmems = Vec::new();
    for (index, record) in graph.block_records().iter().enumerate() {
        let file =
            RestoreTempBacking::new(&format!("block-{index}"), record.block().backing_bytes());
        let backing = crate::block::BlockFileBacking::open_snapshot(
            file.path(),
            record.config().is_read_only(),
        )
        .expect("block restore backing should open")
        .0;
        files.push(file);
        blocks.push(backing);
    }
    for (index, record) in graph.pmem_records().iter().enumerate() {
        let file = RestoreTempBacking::new(&format!("pmem-{index}"), record.pmem().file_bytes());
        let host_file = OpenOptions::new()
            .read(true)
            .write(!record.config().is_read_only())
            .open(file.path())
            .expect("pmem restore file should open");
        let backing =
            crate::pmem::PmemFileBacking::from_file(host_file, record.config().is_read_only())
                .expect("pmem restore backing should validate");
        files.push(file);
        pmems.push(backing);
    }
    (files, blocks, pmems)
}

#[test]
fn restore_plan_prepares_all_root_transport_and_class_combinations() {
    for (transport, blocks, pmems, root) in [
        (
            SnapshotV2DeviceTransportKind::Mmio,
            1,
            0,
            Some(DEVICE_KIND_BLOCK),
        ),
        (SnapshotV2DeviceTransportKind::Pci, 1, 0, None),
        (
            SnapshotV2DeviceTransportKind::Mmio,
            0,
            1,
            Some(DEVICE_KIND_PMEM),
        ),
        (SnapshotV2DeviceTransportKind::Pci, 0, 1, None),
        (
            SnapshotV2DeviceTransportKind::Mmio,
            1,
            1,
            Some(DEVICE_KIND_BLOCK),
        ),
        (
            SnapshotV2DeviceTransportKind::Pci,
            1,
            1,
            Some(DEVICE_KIND_PMEM),
        ),
    ] {
        let graph = fixture_graph(transport, blocks, pmems, root);
        let memory = restore_memory_for(&graph);
        let expected_root = graph.root_key();
        let (_files, block_backings, pmem_backings) = restore_backings(&graph);
        let plan = SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("profile-3 restore plan should validate");

        assert_eq!(plan.root_key(), expected_root);
        assert_eq!(plan.transport_kind(), transport);
        assert_eq!(plan.block_len(), blocks);
        assert_eq!(plan.pmem_len(), pmems);
        assert_eq!(plan.pmem_configs().as_slice().len(), pmems);
        let bundle = plan
            .prepare_backings(block_backings, pmem_backings, || false)
            .expect("profile-3 restore bundle should prepare");
        assert_eq!(
            bundle
                .block_bundle()
                .map_or(0, |block| block.records().len()),
            blocks
        );
        assert_eq!(bundle.pmem_records().len(), pmems);
        bundle
            .abort()
            .expect("profile-3 restore bundle should cleanly abort");
    }
}

#[test]
fn pmem_restore_bundle_retains_exact_range_queue_limiter_retry_and_value_state() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    let expected = graph.clone();
    let memory = restore_memory_for(&graph);
    let now = Instant::now();
    let (_files, blocks, pmems) = restore_backings(&graph);
    let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, now)
        .expect("pmem restore plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("pmem restore bundle should prepare");

    assert_eq!(bundle.root_key(), expected.root_key());
    assert_eq!(bundle.transport_kind(), SnapshotV2DeviceTransportKind::Mmio);
    assert!(bundle.block_bundle().is_none());
    assert_eq!(bundle.pmem_configs().as_slice().len(), 1);
    let prepared = &bundle.pmem_records()[0];
    let expected_record = &expected.pmem_records()[0];
    assert_eq!(prepared.key(), expected_record.key());
    assert!(prepared.is_root_device());
    assert_eq!(
        prepared.prepared_device().guest_range(),
        expected_record.pmem().guest_range()
    );
    assert_eq!(
        prepared.prepared_device().config_space(),
        expected_record.pmem().config_space()
    );
    assert_eq!(
        prepared
            .device()
            .active_queue()
            .map(crate::pmem::VirtioPmemQueue::snapshot_state),
        expected_record.pmem().active_queue()
    );
    assert!(prepared.device().has_pending_rate_limited_queue());
    assert_eq!(prepared.retry(), expected_record.pmem().retry());
    assert_eq!(
        prepared.retry_deadline(),
        Some(now + Duration::from_nanos(99))
    );
    assert_eq!(prepared.virtio(), expected_record.virtio());
    assert_eq!(prepared.transport(), expected_record.transport());

    let recaptured = prepared
        .device()
        .capture_state_at(
            prepared.prepared_device().config_space(),
            expected_record.pmem().file_bytes(),
            expected_record.config().rate_limiter(),
            now,
        )
        .expect("restored pmem semantics should recapture");
    assert_eq!(
        recaptured.active_queue(),
        expected_record.pmem().active_queue()
    );
    assert_eq!(
        recaptured.pending_rate_limited_queue(),
        expected_record.pmem().pending_rate_limited_queue()
    );
    assert_eq!(
        recaptured.rate_limiter().bandwidth().map(|bucket| (
            bucket.budget(),
            bucket.remaining_burst(),
            bucket.age_nanos(),
        )),
        expected_record.pmem().limiter().bandwidth().map(|bucket| (
            bucket.budget(),
            bucket.remaining_burst(),
            bucket.age_nanos(),
        ))
    );
    assert_eq!(
        format!("{bundle:?}"),
        "PreparedSnapshotV2StorageBundle { block_count: 0, pmem_count: 1, transport: Mmio, state: \"<redacted>\" }"
    );
    bundle
        .abort()
        .expect("pmem-only cleanup should be infallible");
}

#[test]
fn mixed_pmem_root_restore_keeps_the_block_subgraph_rootless() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Pci,
        1,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    let memory = restore_memory_for(&graph);
    let now = Instant::now();
    let (_files, blocks, pmems) = restore_backings(&graph);
    let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, now)
        .expect("mixed restore plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("mixed restore bundle should prepare");

    let block = bundle
        .block_bundle()
        .expect("mixed bundle should retain its block sub-bundle");
    assert_eq!(block.records().len(), 1);
    assert!(!block.records()[0].is_root_device());
    assert_eq!(bundle.pmem_records().len(), 1);
    assert!(bundle.pmem_records()[0].is_root_device());
    bundle.abort().expect("mixed bundle should cleanly abort");
}

#[test]
fn restore_plan_rejects_loaded_ram_overlap_and_stale_queue_indices() {
    let mut overlapping = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    let range = GuestMemoryRange::new(GuestAddress::new(0x40_0000), VIRTIO_PMEM_ALIGNMENT * 2)
        .expect("overlapping pmem range should validate");
    overlapping.pmem_records[0].pmem.guest_range = range;
    overlapping.pmem_records[0].pmem.config_space =
        VirtioPmemConfigSpace::new(range.start().raw_value(), range.size());
    let memory = restore_memory_for(&overlapping);
    assert!(matches!(
        SnapshotV2StorageRestorePlan::prepare(overlapping, &memory, Instant::now()),
        Err(SnapshotV2StorageRestorePlanError::PmemRange)
    ));

    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    let mut memory = restore_memory_for(&graph);
    let used_ring = graph.pmem_records()[0].virtio().queues()[0].device_ring();
    memory
        .write_slice(
            &0_u16.to_le_bytes(),
            GuestAddress::new(used_ring.raw_value() + 2),
        )
        .expect("stale used index should write");
    assert!(matches!(
        SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now()),
        Err(SnapshotV2StorageRestorePlanError::QueueContinuation)
    ));
}

#[test]
fn restore_plan_and_bundle_allocation_faults_precede_owner_construction() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    let memory = restore_memory_for(&graph);
    for fail_at in 0..2 {
        assert!(matches!(
            super::restore::prepare_with_failing_reserve_for_test(
                graph.clone(),
                &memory,
                Instant::now(),
                fail_at,
            ),
            Err(SnapshotV2StorageRestorePlanError::Allocation)
        ));
    }

    let (_files, blocks, pmems) = restore_backings(&graph);
    let plan = SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now())
        .expect("allocation bundle plan should prepare");
    let error =
        super::restore::prepare_backings_with_failing_reserve_for_test(plan, blocks, pmems, 0)
            .expect_err("bundle reserve failure should precede mapping");
    assert!(error.is_retryable());
    assert!(!error.cleanup_failed());
}

#[test]
fn later_pmem_construction_fault_releases_the_prepared_prefix_and_allows_retry() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        2,
        Some(DEVICE_KIND_PMEM),
    );
    let expected = graph.clone();
    let memory = restore_memory_for(&graph);
    let (_files, blocks, pmems) = restore_backings(&graph);
    let plan = SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now())
        .expect("pmem fault plan should prepare");
    let error = super::restore::prepare_backings_with_pmem_fault_for_test(plan, blocks, pmems, 1)
        .expect_err("second pmem construction should fail after the first mapping");
    assert!(!error.is_retryable());
    assert!(!error.cleanup_failed());

    let (_retry_files, blocks, pmems) = restore_backings(&expected);
    let retry = SnapshotV2StorageRestorePlan::prepare(expected, &memory, Instant::now())
        .expect("pmem retry plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("pmem backings should remain reusable after prefix cleanup");
    assert_eq!(retry.pmem_records().len(), 2);
    assert!(
        retry.pmem_records()[0]
            .prepared_device()
            .backing()
            .is_read_only()
    );
    assert!(
        !retry.pmem_records()[1]
            .prepared_device()
            .backing()
            .is_read_only()
    );
    retry
        .abort()
        .expect("pmem retry bundle should abort cleanly");
}

#[test]
fn mmio_handoff_preserves_exact_pmem_transport_semantics_and_storage_order() {
    for (blocks, pmems, root) in [
        (1, 0, Some(DEVICE_KIND_BLOCK)),
        (0, 1, Some(DEVICE_KIND_PMEM)),
        (1, 1, Some(DEVICE_KIND_BLOCK)),
        (0, 1, None),
    ] {
        let graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, blocks, pmems, root);
        let expected = graph.clone();
        let memory = restore_memory_for(&graph);
        let now = Instant::now();
        let (_files, block_backings, pmem_backings) = restore_backings(&graph);
        let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, now)
            .expect("MMIO handoff plan should prepare")
            .prepare_backings(block_backings, pmem_backings, || false)
            .expect("MMIO handoff bundle should prepare");

        let mmio = bundle
            .prepare_mmio_transport()
            .expect("MMIO handoff should reconstruct");
        assert_eq!(mmio.root_key(), expected.root_key());
        assert_eq!(
            mmio.block_bundle()
                .map_or(0, |bundle| bundle.records().len()),
            blocks
        );
        assert_eq!(mmio.pmem_records().len(), pmems);
        assert_eq!(mmio.pmem_configs().as_slice().len(), pmems);
        for (record, expected) in mmio.pmem_records().iter().zip(expected.pmem_records()) {
            let SnapshotV2DeviceTransport::Mmio(expected_mmio) = expected.transport() else {
                panic!("fixture pmem transport should be MMIO");
            };
            let expected_transport =
                crate::snapshot_device_v2::restore_mmio_transport_state_for_device(
                    crate::pmem::VIRTIO_PMEM_DEVICE_ID,
                    expected.virtio(),
                    expected_mmio,
                )
                .expect("fixture retained transport should restore");
            assert_eq!(record.key(), expected.key());
            assert_eq!(record.pmem_id(), expected.config().pmem_id());
            assert_eq!(record.is_root_device(), expected.is_root());
            assert_eq!(record.retry(), expected.pmem().retry());
            assert_eq!(
                record.retry_deadline(),
                Some(now + Duration::from_nanos(99))
            );
            assert_eq!(record.region(), expected_mmio.region());
            assert_eq!(record.interrupt_line(), expected_mmio.interrupt_line());
            assert_eq!(
                record.prepared_device().guest_range(),
                expected.pmem().guest_range()
            );
            assert_eq!(record.handler().transport_state(), expected_transport);
            let recaptured = record
                .handler()
                .capture_pmem_device_state_at(
                    expected.pmem().file_bytes(),
                    expected.config().rate_limiter(),
                    now,
                )
                .expect("reconstructed pmem handler should recapture");
            assert_eq!(recaptured.active_queue(), expected.pmem().active_queue());
            assert_eq!(
                recaptured.pending_rate_limited_queue(),
                expected.pmem().pending_rate_limited_queue()
            );
            assert_eq!(
                recaptured.rate_limiter().bandwidth().map(|bucket| (
                    bucket.budget(),
                    bucket.remaining_burst(),
                    bucket.age_nanos(),
                )),
                expected.pmem().limiter().bandwidth().map(|bucket| (
                    bucket.budget(),
                    bucket.remaining_burst(),
                    bucket.age_nanos(),
                ))
            );
        }
        let debug = format!("{mmio:?}");
        for secret in ["pmem-selector", "root=", "PARTUUID"] {
            assert!(!debug.contains(secret));
        }
        mmio.abort()
            .expect("MMIO handoff should release every unpublished owner");
    }
}

#[test]
fn mmio_handoff_rejects_pci_and_allocation_faults_with_clean_owner_release() {
    let pci = fixture_graph(SnapshotV2DeviceTransportKind::Pci, 0, 1, None);
    let memory = restore_memory_for(&pci);
    let (_files, blocks, pmems) = restore_backings(&pci);
    let bundle = SnapshotV2StorageRestorePlan::prepare(pci, &memory, Instant::now())
        .expect("PCI rejection plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("PCI rejection bundle should prepare");
    let error = bundle
        .prepare_mmio_transport()
        .expect_err("PCI storage must not fall back to MMIO");
    assert!(!error.cleanup_failed());

    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        2,
        1,
        Some(DEVICE_KIND_BLOCK),
    );
    let memory = restore_memory_for(&graph);
    let (_files, blocks, pmems) = restore_backings(&graph);
    let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now())
        .expect("allocation handoff plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("allocation handoff bundle should prepare");
    let async_runtime = bundle
        .block_bundle()
        .and_then(|bundle| bundle.async_runtime())
        .cloned();
    let error = super::restore::prepare_mmio_transport_with_failing_reserve_for_test(bundle)
        .expect_err("pmem MMIO record reservation should fail explicitly");
    assert!(!error.cleanup_failed());
    if let Some(runtime) = async_runtime {
        assert_eq!(
            runtime
                .generation_count()
                .expect("Async runtime should remain observable"),
            0
        );
    }
}

#[test]
fn pci_handoff_preserves_exact_pmem_placement_and_storage_order() {
    for (blocks, pmems, root) in [
        (1, 0, Some(DEVICE_KIND_BLOCK)),
        (0, 1, Some(DEVICE_KIND_PMEM)),
        (1, 1, Some(DEVICE_KIND_BLOCK)),
        (0, 1, None),
    ] {
        let graph = fixture_graph(SnapshotV2DeviceTransportKind::Pci, blocks, pmems, root);
        let expected = graph.clone();
        let memory = restore_memory_for(&graph);
        let now = Instant::now();
        let (_files, block_backings, pmem_backings) = restore_backings(&graph);
        let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, now)
            .expect("PCI handoff plan should prepare")
            .prepare_backings(block_backings, pmem_backings, || false)
            .expect("PCI handoff bundle should prepare");

        let pci = bundle
            .prepare_pci_transport()
            .expect("PCI handoff should reconstruct");
        assert_eq!(pci.root_key(), expected.root_key());
        assert_eq!(
            pci.block_bundle()
                .map_or(0, |bundle| bundle.records().len()),
            blocks
        );
        assert_eq!(pci.pmem_records().len(), pmems);
        assert_eq!(pci.pmem_configs().as_slice().len(), pmems);
        for (record, expected) in pci.pmem_records().iter().zip(expected.pmem_records()) {
            let SnapshotV2DeviceTransport::Pci(expected_pci) = expected.transport() else {
                panic!("fixture pmem transport should be PCI");
            };
            assert_eq!(record.key(), expected.key());
            assert_eq!(record.pmem_id(), expected.config().pmem_id());
            assert_eq!(record.is_root_device(), expected.is_root());
            assert_eq!(record.retry(), expected.pmem().retry());
            assert_eq!(
                record.retry_deadline(),
                Some(now + Duration::from_nanos(99))
            );
            assert_eq!(record.origin(), expected_pci.origin());
            assert_eq!(record.sbdf(), expected_pci.sbdf());
            assert_eq!(record.bar_range(), expected_pci.bar_range());
            assert_eq!(
                record.prepared_device().guest_range(),
                expected.pmem().guest_range()
            );
            assert_eq!(
                record.prepared_device().config_space(),
                expected.pmem().config_space()
            );
        }
        let debug = format!("{pci:?}");
        for secret in ["pmem-selector", "root=", "PARTUUID"] {
            assert!(!debug.contains(secret));
        }
        pci.abort()
            .expect("PCI handoff should release every unpublished owner");
    }
}

#[test]
fn pci_handoff_rejects_mmio_and_allocation_faults_with_clean_owner_release() {
    let mmio = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, 0, 1, None);
    let memory = restore_memory_for(&mmio);
    let (_files, blocks, pmems) = restore_backings(&mmio);
    let bundle = SnapshotV2StorageRestorePlan::prepare(mmio, &memory, Instant::now())
        .expect("MMIO rejection plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("MMIO rejection bundle should prepare");
    let error = bundle
        .prepare_pci_transport()
        .expect_err("MMIO storage must not fall back to PCI");
    assert!(!error.cleanup_failed());

    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Pci,
        2,
        1,
        Some(DEVICE_KIND_BLOCK),
    );
    let memory = restore_memory_for(&graph);
    let (_files, blocks, pmems) = restore_backings(&graph);
    let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now())
        .expect("allocation handoff plan should prepare")
        .prepare_backings(blocks, pmems, || false)
        .expect("allocation handoff bundle should prepare");
    let async_runtime = bundle
        .block_bundle()
        .and_then(|bundle| bundle.async_runtime())
        .cloned();
    let error = super::restore::prepare_pci_transport_with_failing_reserve_for_test(bundle)
        .expect_err("pmem PCI record reservation should fail explicitly");
    assert!(!error.cleanup_failed());
    if let Some(runtime) = async_runtime {
        assert_eq!(
            runtime
                .generation_count()
                .expect("Async runtime should remain observable"),
            0
        );
    }
}

#[test]
fn profile_identity_is_exact_distinct_and_bounded() {
    assert_eq!(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        SnapshotFormatVersion::new(2, 6, 0)
    );
    assert_ne!(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION
    );
    const {
        assert!(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_WORST_CASE_BYTES
                <= NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_BYTES
        );
    }
}

#[test]
fn block_only_pmem_only_and_mixed_products_round_trip() {
    let cases = [
        fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            2,
            0,
            Some(DEVICE_KIND_BLOCK),
        ),
        fixture_graph(SnapshotV2DeviceTransportKind::Pci, 2, 0, None),
        fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            0,
            1,
            Some(DEVICE_KIND_PMEM),
        ),
        fixture_graph(SnapshotV2DeviceTransportKind::Pci, 0, 1, None),
        fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            2,
            1,
            Some(DEVICE_KIND_BLOCK),
        ),
        fixture_graph(
            SnapshotV2DeviceTransportKind::Pci,
            2,
            1,
            Some(DEVICE_KIND_PMEM),
        ),
    ];
    for graph in cases {
        let bytes = encoded(&graph);
        let decoded = SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("profile-3 graph should decode");
        assert_eq!(decoded, graph);
        assert_eq!(encoded(&decoded), bytes);
    }
}

#[test]
fn inactive_limiter_free_pmem_round_trips_on_both_transports() {
    for transport_kind in [
        SnapshotV2DeviceTransportKind::Mmio,
        SnapshotV2DeviceTransportKind::Pci,
    ] {
        let mut record = fixture_pmem_record(0, false, transport_kind, 0);
        record.config.rate_limiter = None;
        record.pmem.active_queue = None;
        record.pmem.limiter = SnapshotV2PmemLimiterState::new(None, None);
        record.pmem.pending_rate_limited_queue = false;
        record.pmem.retry = StorageRetryState::None;
        record.virtio = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features: VIRTIO_MMIO_VERSION_1_FEATURE,
            driver_features: 0,
            config_generation: 11,
            status: VIRTIO_DEVICE_STATUS_INIT,
            activated: false,
            queues: vec![SnapshotV2VirtioQueueState::from_parts(
                VIRTIO_PMEM_QUEUE_SIZE,
                0,
                false,
                GuestAddress::new(0),
                GuestAddress::new(0),
                GuestAddress::new(0),
            )],
            pending_notifications: Vec::new(),
            interrupt_intents: Vec::new(),
        });
        let graph = SnapshotV2StorageDeviceGraph::try_from_parts(
            None,
            transport_kind,
            Vec::new(),
            vec![record],
        )
        .expect("inactive limiter-free pmem graph should validate");
        let bytes = encoded(&graph);
        let decoded = SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("inactive limiter-free pmem graph should decode");
        assert_eq!(decoded, graph);
        assert_eq!(encoded(&decoded), bytes);
    }
}

#[test]
fn immutable_fixture_matrix_is_canonical_and_round_trips() {
    let cases = [
        (
            "block-only rooted MMIO",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Mmio,
                1,
                0,
                Some(DEVICE_KIND_BLOCK),
            ),
            BLOCK_ROOT_MMIO_HEX,
        ),
        (
            "block-only rootless PCI",
            fixture_graph(SnapshotV2DeviceTransportKind::Pci, 1, 0, None),
            BLOCK_ROOTLESS_PCI_HEX,
        ),
        (
            "pmem-only rooted MMIO",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Mmio,
                0,
                1,
                Some(DEVICE_KIND_PMEM),
            ),
            PMEM_ROOT_MMIO_HEX,
        ),
        (
            "pmem-only rootless PCI",
            fixture_graph(SnapshotV2DeviceTransportKind::Pci, 0, 1, None),
            PMEM_ROOTLESS_PCI_HEX,
        ),
        (
            "mixed block-rooted MMIO",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Mmio,
                1,
                1,
                Some(DEVICE_KIND_BLOCK),
            ),
            MIXED_BLOCK_ROOT_MMIO_HEX,
        ),
        (
            "mixed pmem-rooted PCI",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Pci,
                1,
                1,
                Some(DEVICE_KIND_PMEM),
            ),
            MIXED_PMEM_ROOT_PCI_HEX,
        ),
    ];
    for (name, graph, expected) in cases {
        let bytes = encoded(&graph);
        assert_eq!(bytes, decode_hex(expected), "{name} bytes changed");
        let decoded = SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("immutable fixture should decode");
        assert_eq!(decoded, graph, "{name} semantics changed");
        assert_eq!(encoded(&decoded), bytes, "{name} re-encode changed");
    }
}

#[test]
#[ignore = "fixture regeneration is an explicit compatibility operation"]
fn print_canonical_fixture_hex() {
    let cases = [
        (
            "block-root-mmio",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Mmio,
                1,
                0,
                Some(DEVICE_KIND_BLOCK),
            ),
        ),
        (
            "block-rootless-pci",
            fixture_graph(SnapshotV2DeviceTransportKind::Pci, 1, 0, None),
        ),
        (
            "pmem-root-mmio",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Mmio,
                0,
                1,
                Some(DEVICE_KIND_PMEM),
            ),
        ),
        (
            "pmem-rootless-pci",
            fixture_graph(SnapshotV2DeviceTransportKind::Pci, 0, 1, None),
        ),
        (
            "mixed-block-root-mmio",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Mmio,
                1,
                1,
                Some(DEVICE_KIND_BLOCK),
            ),
        ),
        (
            "mixed-pmem-root-pci",
            fixture_graph(
                SnapshotV2DeviceTransportKind::Pci,
                1,
                1,
                Some(DEVICE_KIND_PMEM),
            ),
        ),
    ];
    for (name, graph) in cases {
        let bytes = encoded(&graph);
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        println!("{name}={hex}");
    }
}

#[test]
fn graph_preserves_independent_class_order_and_cross_storage_root() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Pci,
        2,
        2,
        Some(DEVICE_KIND_PMEM),
    );
    assert_eq!(graph.root_key(), Some(SnapshotV2DeviceKey::pmem(0)));
    assert_eq!(
        graph
            .block_records()
            .iter()
            .map(|record| record.key().instance())
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        graph
            .pmem_records()
            .iter()
            .map(|record| record.key().instance())
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(!graph.block_records().iter().any(|record| record.is_root()));
    assert!(graph.pmem_records()[0].is_root());
    assert!(!graph.pmem_records()[1].is_root());
}

#[test]
fn build_rejects_empty_root_order_selector_geometry_and_resource_conflicts() {
    assert!(matches!(
        SnapshotV2StorageDeviceGraph::try_from_parts(
            None,
            SnapshotV2DeviceTransportKind::Mmio,
            Vec::new(),
            Vec::new(),
        ),
        Err(SnapshotV2StorageDeviceGraphBuildError::InvalidGraph)
    ));

    let mut graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        2,
        1,
        Some(DEVICE_KIND_BLOCK),
    );
    graph.root_key = Some(SnapshotV2DeviceKey::pmem(0));
    assert_eq!(validate_graph(&graph), Err(GraphValidationError::Root));

    let mut graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, 2, 1, None);
    graph.pmem_records[0].config.selector = graph.block_records[0].config().selector().to_string();
    assert_eq!(validate_graph(&graph), Err(GraphValidationError::Conflict));

    let mut graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, 0, 1, None);
    graph.pmem_records[0].pmem.mapped_bytes = VIRTIO_PMEM_ALIGNMENT;
    assert_eq!(validate_graph(&graph), Err(GraphValidationError::Pmem));

    let mut graph = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, 2, 1, None);
    let start = graph.block_records[0]
        .transport()
        .as_mmio()
        .expect("fixture should be MMIO")
        .region()
        .range()
        .start();
    let range = GuestMemoryRange::new(start, VIRTIO_PMEM_ALIGNMENT)
        .expect("overlapping range should be structurally valid");
    graph.pmem_records[0].pmem.guest_range = range;
    graph.pmem_records[0].pmem.config_space =
        VirtioPmemConfigSpace::new(start.raw_value(), VIRTIO_PMEM_ALIGNMENT);
    graph.pmem_records[0].pmem.file_bytes = VIRTIO_PMEM_ALIGNMENT;
    graph.pmem_records[0].pmem.mapped_bytes = VIRTIO_PMEM_ALIGNMENT;
    assert_eq!(validate_graph(&graph), Err(GraphValidationError::Conflict));
}

#[test]
fn pending_rate_limit_state_requires_queue_limiter_and_retry() {
    let baseline = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, 0, 1, None);
    for mutate in [
        |record: &mut SnapshotV2PmemDeviceRecord| record.pmem.active_queue = None,
        |record: &mut SnapshotV2PmemDeviceRecord| {
            record.pmem.limiter = SnapshotV2PmemLimiterState::new(None, None);
        },
        |record: &mut SnapshotV2PmemDeviceRecord| {
            record.pmem.retry = StorageRetryState::None;
        },
    ] {
        let mut graph = baseline.clone();
        mutate(&mut graph.pmem_records[0]);
        assert!(validate_graph(&graph).is_err());
    }

    let mut retry_without_pending = baseline;
    retry_without_pending.pmem_records[0]
        .pmem
        .pending_rate_limited_queue = false;
    assert!(validate_graph(&retry_without_pending).is_err());
}

#[test]
fn pmem_strings_limiter_queue_interrupt_and_root_invariants_fail_closed() {
    for (id, selector) in [("", "selector"), ("pmem-bad", "selector"), ("pmem0", "")] {
        assert!(SnapshotV2PmemConfig::try_new(id, false, false, None, selector).is_err());
    }
    assert!(
        SnapshotV2PmemConfig::try_new(
            "pmem0",
            false,
            false,
            None,
            "x".repeat(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES + 1),
        )
        .is_err()
    );
    assert!(
        SnapshotV2PmemConfig::try_new(
            "x".repeat(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES + 1),
            false,
            false,
            None,
            "selector",
        )
        .is_err()
    );
    assert!(
        SnapshotV2PmemConfig::try_new(
            "pmem0",
            false,
            false,
            Some(PmemRateLimiterConfig::new(
                Some(PmemTokenBucketConfig::new(0, None, 10)),
                None,
            )),
            "selector",
        )
        .is_err()
    );

    let mut unicode = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, 0, 1, None);
    unicode.pmem_records[0].config =
        SnapshotV2PmemConfig::try_new("pmem_设备1", false, false, None, "unicode-selector")
            .expect("public Unicode alphanumeric pmem ID should remain valid");
    unicode.pmem_records[0].pmem.limiter = SnapshotV2PmemLimiterState::new(None, None);
    unicode.pmem_records[0].pmem.pending_rate_limited_queue = false;
    unicode.pmem_records[0].pmem.retry = StorageRetryState::None;
    let bytes = encoded(&unicode);
    assert_eq!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("Unicode pmem ID should decode"),
        unicode
    );

    let baseline = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        2,
        Some(DEVICE_KIND_PMEM),
    );

    let mut invalid = baseline.clone();
    invalid.pmem_records[1].config.pmem_id = invalid.pmem_records[0].config.pmem_id.clone();
    assert_eq!(
        validate_graph(&invalid),
        Err(GraphValidationError::Conflict)
    );

    let mut invalid = baseline.clone();
    invalid.pmem_records[1].config.is_root = true;
    assert_eq!(validate_graph(&invalid), Err(GraphValidationError::Root));

    let mut invalid = baseline.clone();
    invalid.pmem_records[0]
        .pmem
        .limiter
        .bandwidth
        .as_mut()
        .expect("fixture limiter should exist")
        .budget = 1_000_001;
    assert_eq!(validate_graph(&invalid), Err(GraphValidationError::Pmem));

    let mut invalid = baseline.clone();
    let common = &invalid.pmem_records[0].virtio;
    invalid.pmem_records[0].virtio =
        SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features: 0,
            driver_features: common.driver_features(),
            config_generation: common.config_generation(),
            status: common.status(),
            activated: common.is_activated(),
            queues: common.queues().to_vec(),
            pending_notifications: common.pending_notifications().to_vec(),
            interrupt_intents: common.interrupt_intents().to_vec(),
        });
    assert_eq!(validate_graph(&invalid), Err(GraphValidationError::Virtio));

    let mut invalid = baseline.clone();
    let common = &invalid.pmem_records[0].virtio;
    invalid.pmem_records[0].virtio =
        SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features: common.available_features(),
            driver_features: common.driver_features(),
            config_generation: common.config_generation(),
            status: common.status(),
            activated: common.is_activated(),
            queues: common.queues().to_vec(),
            pending_notifications: common.pending_notifications().to_vec(),
            interrupt_intents: vec![SnapshotV2InterruptIntent::Queue { queue_index: 1 }],
        });
    assert_eq!(validate_graph(&invalid), Err(GraphValidationError::Virtio));

    let mut invalid = baseline.clone();
    let queues = invalid.pmem_records[0].virtio.queues().to_vec();
    let common = &invalid.pmem_records[1].virtio;
    invalid.pmem_records[1].virtio =
        SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features: common.available_features(),
            driver_features: common.driver_features(),
            config_generation: common.config_generation(),
            status: common.status(),
            activated: common.is_activated(),
            queues,
            pending_notifications: common.pending_notifications().to_vec(),
            interrupt_intents: common.interrupt_intents().to_vec(),
        });
    assert_eq!(
        validate_graph(&invalid),
        Err(GraphValidationError::Conflict)
    );

    let mut invalid = baseline;
    let first_line = match invalid.pmem_records[0].transport {
        SnapshotV2DeviceTransport::Mmio(ref state) => state.interrupt_line(),
        SnapshotV2DeviceTransport::Pci(_) => panic!("fixture should be MMIO"),
    };
    invalid.pmem_records[1].transport = match &invalid.pmem_records[1].transport {
        SnapshotV2DeviceTransport::Mmio(state) => {
            SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                state.device_feature_select(),
                state.driver_feature_select(),
                state.queue_select(),
                state.region(),
                first_line,
            ))
        }
        SnapshotV2DeviceTransport::Pci(_) => panic!("fixture should be MMIO"),
    };
    assert_eq!(
        validate_graph(&invalid),
        Err(GraphValidationError::Conflict)
    );
}

#[test]
fn combined_record_limit_is_exact_and_checked_before_record_validation() {
    let maximum = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS),
        None,
    );
    assert_eq!(
        maximum.record_count(),
        usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS)
    );
    assert!(validate_graph(&maximum).is_ok());

    let mut too_many = maximum;
    too_many.pmem_records.push(fixture_pmem_record(
        u32::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS),
        false,
        SnapshotV2DeviceTransportKind::Mmio,
        u32::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS),
    ));
    assert_eq!(validate_graph(&too_many), Err(GraphValidationError::Root));
}

#[test]
fn decoder_rejects_version_header_directory_geometry_and_trailing_mutations() {
    const PROFILE_OFFSET: usize = 10;
    const ROOT_KIND_OFFSET: usize = 32;
    const RECORD_DIRECTORY_OFFSET: usize = NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES;
    const BLOCK_SECTION_KIND: u16 = 2;
    const SECTION_KIND_OFFSET: usize = 4;
    const SECTION_PAYLOAD_OFFSET: usize = 16;

    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        2,
        1,
        Some(DEVICE_KIND_BLOCK),
    );
    let bytes = encoded(&graph);
    assert_eq!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        ),
        Err(SnapshotV2StorageDeviceGraphDecodeError::UnsupportedVersion)
    );

    let mut invalid = bytes.clone();
    invalid[PROFILE_OFFSET..PROFILE_OFFSET + 2].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid,
        ),
        Err(SnapshotV2StorageDeviceGraphDecodeError::UnsupportedProfile)
    ));

    let mut invalid = bytes.clone();
    invalid[ROOT_KIND_OFFSET..ROOT_KIND_OFFSET + 4].copy_from_slice(&99_u32.to_le_bytes());
    assert!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid,
        )
        .is_err()
    );

    let mut invalid = bytes.clone();
    invalid[RECORD_DIRECTORY_OFFSET..RECORD_DIRECTORY_OFFSET + 4]
        .copy_from_slice(&DEVICE_KIND_PMEM.to_le_bytes());
    assert!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid,
        )
        .is_err()
    );

    let record_count = graph.record_count();
    let section_directory = NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES
        + record_count * NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES;
    let pmem_section_index = graph.block_records().len() * SECTION_COUNT_PER_RECORD + 1;
    let pmem_entry =
        section_directory + pmem_section_index * NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES;

    let mut invalid = bytes.clone();
    invalid[pmem_entry + SECTION_KIND_OFFSET..pmem_entry + SECTION_KIND_OFFSET + 2]
        .copy_from_slice(&BLOCK_SECTION_KIND.to_le_bytes());
    assert!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid,
        )
        .is_err()
    );

    let pmem_payload = usize::try_from(u64::from_le_bytes(
        bytes[pmem_entry + SECTION_PAYLOAD_OFFSET..pmem_entry + SECTION_PAYLOAD_OFFSET + 8]
            .try_into()
            .expect("payload offset should fit"),
    ))
    .expect("payload offset should fit usize");
    let mut invalid = bytes.clone();
    invalid[pmem_payload + 8..pmem_payload + 16].copy_from_slice(&0_u64.to_le_bytes());
    assert!(matches!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &invalid,
        ),
        Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidGraph)
    ));

    assert!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes[..bytes.len() - 1],
        )
        .is_err()
    );
    let mut trailing = bytes;
    trailing.push(0);
    assert!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &trailing,
        )
        .is_err()
    );
}

struct FailingReserve {
    fail_at: usize,
    calls: usize,
}

impl FailingReserve {
    const fn new(fail_at: usize) -> Self {
        Self { fail_at, calls: 0 }
    }

    fn should_fail(&mut self) -> bool {
        let current = self.calls;
        self.calls += 1;
        current == self.fail_at
    }
}

impl ReservePolicy for FailingReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        if self.should_fail() {
            Err(())
        } else {
            values.try_reserve_exact(additional).map_err(|_| ())
        }
    }

    fn reserve_string(&mut self, value: &mut String, additional: usize) -> Result<(), ()> {
        if self.should_fail() {
            Err(())
        } else {
            value.try_reserve_exact(additional).map_err(|_| ())
        }
    }
}

#[test]
fn every_typed_allocation_is_fallible_and_decode_preflight_allocates_nothing() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Pci,
        2,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    assert_eq!(
        codec::encode_with_policy(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &graph,
            &mut FailingReserve::new(0),
        ),
        Err(SnapshotV2StorageDeviceGraphEncodeError::Allocation)
    );
    let bytes = encoded(&graph);
    let mut observed_failures = 0;
    for fail_at in 0..64 {
        match codec::decode_with_policy(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
            &mut FailingReserve::new(fail_at),
        ) {
            Err(SnapshotV2StorageDeviceGraphDecodeError::Allocation) => observed_failures += 1,
            Ok(decoded) => {
                assert_eq!(decoded, graph);
                break;
            }
            Err(error) => panic!("allocation injection produced unexpected error: {error}"),
        }
    }
    assert!(observed_failures >= 10);
}

#[test]
fn debug_output_is_value_redacted() {
    let graph = fixture_graph(
        SnapshotV2DeviceTransportKind::Mmio,
        0,
        1,
        Some(DEVICE_KIND_PMEM),
    );
    let debug = format!("{graph:?} {:?}", graph.pmem_records()[0]);
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("pmem-selector"));
}

trait TransportTestExt {
    fn as_mmio(&self) -> Option<&SnapshotV2MmioDeviceState>;
}

impl TransportTestExt for SnapshotV2DeviceTransport {
    fn as_mmio(&self) -> Option<&SnapshotV2MmioDeviceState> {
        match self {
            Self::Mmio(state) => Some(state),
            Self::Pci(_) => None,
        }
    }
}
