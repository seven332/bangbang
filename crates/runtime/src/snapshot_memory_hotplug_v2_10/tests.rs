use super::codec::{self, ReservePolicy};
use super::*;

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
use crate::memory_hotplug::{
    MemoryHotplugConfigInput, VIRTIO_MEM_DEFAULT_REGION_ADDRESS, VIRTIO_MEM_QUEUE_SIZE,
};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO, PCI_SEGMENT_ZERO,
    PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState, SnapshotV2PciBarProbeState,
    SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts, SnapshotV2PciMsixState,
    SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry, SnapshotV2PciWritableByte,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioStateParts,
};
use crate::snapshot_memory_v2::write_snapshot_v2_memory_image_with_compatibility_version;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VirtioPciEndpointPhase,
};

use std::io::Cursor;

const HEALTHY_DRIVER_OK: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
    | VIRTIO_DEVICE_STATUS_DRIVER
    | VIRTIO_DEVICE_STATUS_FEATURES_OK
    | VIRTIO_DEVICE_STATUS_DRIVER_OK;
const PAYLOAD_OFFSET: usize = 64 + 4 * 32;
const DIRECTORY_OFFSET: usize = 64;
const DIRECTORY_ENTRY_BYTES: usize = 32;
const DIRECTORY_PAYLOAD_OFFSET: usize = 8;
const DIRECTORY_LENGTH_OFFSET: usize = 16;
const INACTIVE_MMIO_FIXTURE_HEX: &str = include_str!("fixtures/inactive-mmio.hex");
const ACTIVE_PCI_FIXTURE_HEX: &str = include_str!("fixtures/active-pci.hex");

fn config(total_size_mib: u64, block_size_mib: u64, slot_size_mib: u64) -> MemoryHotplugConfig {
    MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(
        total_size_mib,
        block_size_mib,
        slot_size_mib,
    ))
    .expect("test memory-hotplug configuration should validate")
}

fn base_config() -> MemoryHotplugConfig {
    config(1024, 2, 128)
}

fn base_config_space(plugged_blocks: u64) -> VirtioMemConfigSpace {
    VirtioMemConfigSpace::new(
        2 * MIB,
        VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
        1024 * MIB,
    )
    .with_node_id(7)
    .with_usable_region_size(256 * MIB)
    .with_plugged_size(plugged_blocks * 2 * MIB)
    .with_requested_size(128 * MIB)
}

fn bitmap_with_ranges(ranges: &[(usize, usize)]) -> Vec<u8> {
    let mut bitmap = vec![0_u8; 64];
    for (start, count) in ranges {
        for block in *start..start + count {
            bitmap[block / 8] |= 1_u8 << (block % 8);
        }
    }
    bitmap
}

fn inactive_virtio() -> SnapshotV2VirtioState {
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: REQUIRED_FEATURES,
        driver_features: 0,
        config_generation: 3,
        status: VIRTIO_DEVICE_STATUS_INIT,
        activated: false,
        queues: vec![SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_MEM_QUEUE_SIZE,
            0,
            false,
            GuestAddress::new(0),
            GuestAddress::new(0),
            GuestAddress::new(0),
        )],
        pending_notifications: Vec::new(),
        interrupt_intents: vec![SnapshotV2InterruptIntent::Configuration],
    })
}

fn active_virtio() -> SnapshotV2VirtioState {
    SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
        available_features: REQUIRED_FEATURES,
        driver_features: REQUIRED_FEATURES,
        config_generation: 4,
        status: HEALTHY_DRIVER_OK,
        activated: true,
        queues: vec![SnapshotV2VirtioQueueState::from_parts(
            VIRTIO_MEM_QUEUE_SIZE,
            VIRTIO_MEM_QUEUE_SIZE,
            true,
            GuestAddress::new(0x10_0000),
            GuestAddress::new(0x12_0000),
            GuestAddress::new(0x14_0000),
        )],
        pending_notifications: vec![0],
        interrupt_intents: vec![
            SnapshotV2InterruptIntent::Queue { queue_index: 0 },
            SnapshotV2InterruptIntent::Configuration,
        ],
    })
}

fn mmio_transport() -> SnapshotV2MmioDeviceState {
    let region = MmioRegion::new(
        MmioRegionId::new(101),
        GuestAddress::new(0xd000_0000),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .expect("test MMIO region should validate");
    SnapshotV2MmioDeviceState::from_parts(
        0,
        1,
        0,
        region,
        GuestInterruptLine::new(32).expect("test SPI should validate"),
    )
}

fn pci_transport() -> SnapshotV2PciDeviceState {
    let sbdf = PciSbdf::new(
        PCI_SEGMENT_ZERO,
        PCI_BUS_ZERO,
        PCI_FIRST_ENDPOINT_DEVICE,
        PCI_FUNCTION_ZERO,
    )
    .expect("test SBDF should validate");
    let bar_range = GuestMemoryRange::new(
        GuestAddress::new(PCI_BAR64_START),
        VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    )
    .expect("test BAR should validate");
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries: vec![
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 64, 0),
            SnapshotV2PciMsixTableEntry::from_parts(0x0800_0040, 0, 96, 1),
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
    })
}

fn inactive_mmio_state() -> SnapshotV2MemoryHotplugState {
    let ranges = [(1, 2), (5, 3)];
    SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(5),
        None,
        bitmap_with_ranges(&ranges),
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("inactive MMIO state should validate")
}

fn active_pci_state() -> SnapshotV2MemoryHotplugState {
    let ranges = [(0, 1), (7, 2), (127, 1)];
    SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(4),
        Some(
            SnapshotV2MemoryHotplugQueueState::try_new(7, 7)
                .expect("equal active cursors should validate"),
        ),
        bitmap_with_ranges(&ranges),
        active_virtio(),
        SnapshotV2DeviceTransport::Pci(pci_transport()),
    )
    .expect("active PCI state should validate")
}

fn active_mmio_state() -> SnapshotV2MemoryHotplugState {
    let ranges = [(0, 1), (7, 2), (127, 1)];
    SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(4),
        Some(
            SnapshotV2MemoryHotplugQueueState::try_new(7, 7)
                .expect("equal active cursors should validate"),
        ),
        bitmap_with_ranges(&ranges),
        active_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("active MMIO state should validate")
}

fn inactive_pci_state() -> SnapshotV2MemoryHotplugState {
    let ranges = [(1, 2), (5, 3)];
    SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(5),
        None,
        bitmap_with_ranges(&ranges),
        inactive_virtio(),
        SnapshotV2DeviceTransport::Pci(pci_transport()),
    )
    .expect("inactive PCI state should validate")
}

fn binding_for_ranges(
    version: SnapshotFormatVersion,
    ranges: Vec<GuestMemoryRange>,
) -> SnapshotV2MemoryBinding {
    let layout = GuestMemoryLayout::new(ranges).expect("binding fixture layout should validate");
    let memory = GuestMemory::allocate(&layout).expect("binding fixture memory should allocate");
    write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut Cursor::new(Vec::new()),
        version,
    )
    .expect("binding fixture should encode")
}

#[test]
fn kind_one_binding_closes_fragmented_plugged_unions_and_rejects_hostile_coverage() {
    let state = inactive_mmio_state();
    let config_space = state.config_space();
    let block_size = config_space.block_size();
    let first_start = config_space
        .addr()
        .checked_add(block_size)
        .expect("first plugged start should fit");
    let second_start = config_space
        .addr()
        .checked_add(5 * block_size)
        .expect("second plugged start should fit");
    let range = |start, length| {
        GuestMemoryRange::new(GuestAddress::new(start), length)
            .expect("binding fixture range should validate")
    };
    let fragmented = binding_for_ranges(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        vec![
            range(first_start, block_size),
            range(first_start + block_size, block_size),
            range(second_start, 3 * block_size),
        ],
    );
    state
        .validate_memory_binding(&fragmented)
        .expect("union-equivalent source fragmentation should validate");

    let missing = binding_for_ranges(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        vec![
            range(first_start, block_size),
            range(second_start, 3 * block_size),
        ],
    );
    assert_eq!(
        state.validate_memory_binding(&missing),
        Err(SnapshotV2MemoryHotplugBindingError::Coverage)
    );

    let extra = binding_for_ranges(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        vec![
            range(config_space.addr(), block_size),
            range(first_start, 2 * block_size),
            range(second_start, 3 * block_size),
        ],
    );
    assert_eq!(
        state.validate_memory_binding(&extra),
        Err(SnapshotV2MemoryHotplugBindingError::Coverage)
    );

    let crossing = binding_for_ranges(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        vec![range(config_space.addr() - 16_384, 32_768)],
    );
    assert_eq!(
        state.validate_memory_binding(&crossing),
        Err(SnapshotV2MemoryHotplugBindingError::BoundaryCrossing)
    );

    let wrong_version = binding_for_ranges(
        SnapshotFormatVersion::new(2, 9, 0),
        vec![
            range(first_start, 2 * block_size),
            range(second_start, 3 * block_size),
        ],
    );
    assert_eq!(
        state.validate_memory_binding(&wrong_version),
        Err(SnapshotV2MemoryHotplugBindingError::Version)
    );

    for error in [
        SnapshotV2MemoryHotplugBindingError::Version,
        SnapshotV2MemoryHotplugBindingError::Overflow,
        SnapshotV2MemoryHotplugBindingError::BoundaryCrossing,
        SnapshotV2MemoryHotplugBindingError::Coverage,
    ] {
        let diagnostics = format!("{error:?} {error}");
        assert!(!diagnostics.contains(&config_space.addr().to_string()));
        assert!(!diagnostics.contains(&first_start.to_string()));
    }
}

fn section_offset(bytes: &[u8], index: usize) -> usize {
    let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
    usize::try_from(u64::from_le_bytes(
        bytes[entry + DIRECTORY_PAYLOAD_OFFSET..entry + DIRECTORY_PAYLOAD_OFFSET + 8]
            .try_into()
            .expect("directory payload offset should fit"),
    ))
    .expect("test payload offset should fit usize")
}

fn section_length(bytes: &[u8], index: usize) -> usize {
    let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
    usize::try_from(u64::from_le_bytes(
        bytes[entry + DIRECTORY_LENGTH_OFFSET..entry + DIRECTORY_LENGTH_OFFSET + 8]
            .try_into()
            .expect("directory length should fit"),
    ))
    .expect("test section length should fit usize")
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

#[test]
fn exact_profile_constants_lock_the_complete_bound() {
    assert_eq!(
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        SnapshotFormatVersion::new(2, 10, 0)
    );
    assert_eq!(NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS, 523_264);
    assert_eq!(NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES, 65_408);
    assert_eq!(NATIVE_V2_MEMORY_HOTPLUG_WORST_CASE_BYTES, 65_920);
    assert_eq!(NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES, 128 * 1024);
}

#[test]
fn inactive_mmio_and_active_pci_round_trip_canonically() {
    for (state, fixture) in [
        (inactive_mmio_state(), INACTIVE_MMIO_FIXTURE_HEX),
        (active_pci_state(), ACTIVE_PCI_FIXTURE_HEX),
    ] {
        let encoded = state
            .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
            .expect("state should encode");
        assert_eq!(&encoded[..8], b"BANGME2\0");
        assert_eq!(encoded, fixture_bytes(fixture));
        assert!(encoded.len() <= NATIVE_V2_MEMORY_HOTPLUG_WORST_CASE_BYTES);
        let decoded = SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &encoded,
        )
        .expect("state should decode");
        assert_eq!(decoded, state);
        assert_eq!(
            decoded
                .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
                .expect("decoded state should re-encode"),
            encoded
        );
    }
}

#[test]
fn activation_and_transport_products_round_trip_independently() {
    for state in [active_mmio_state(), inactive_pci_state()] {
        let encoded = state
            .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
            .expect("cross-product state should encode");
        let decoded = SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &encoded,
        )
        .expect("cross-product state should decode");

        assert_eq!(decoded, state);
        assert_eq!(
            decoded.active_queue().is_some(),
            decoded.virtio().is_activated()
        );
        assert_eq!(decoded.transport().kind(), state.transport().kind());
    }
}

#[test]
fn bitmap_iterator_coalesces_maximal_ranges_and_survives_inactive_reset() {
    let state = inactive_mmio_state();
    let mut ranges = state.plugged_ranges();

    assert_eq!(ranges.len(), 2);
    assert_eq!(
        ranges.next(),
        Some(SnapshotV2MemoryHotplugPluggedRange {
            start_block: 1,
            block_count: 2,
        })
    );
    assert_eq!(ranges.len(), 1);
    assert_eq!(
        ranges.next(),
        Some(SnapshotV2MemoryHotplugPluggedRange {
            start_block: 5,
            block_count: 3,
        })
    );
    assert_eq!(ranges.next(), None);
    assert_eq!(ranges.next(), None);
    assert!(!state.virtio().is_activated());
    assert!(state.active_queue().is_none());
    assert!(state.plugged_bitmap().iter().any(|byte| *byte != 0));
}

#[test]
fn constructor_rejects_bitmap_geometry_accounting_and_cursor_mismatches() {
    let wrong_length = SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(0),
        None,
        vec![0; 63],
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    );
    assert!(matches!(
        wrong_length,
        Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap)
    ));

    let mut outside_usable = bitmap_with_ranges(&[]);
    outside_usable[200 / 8] |= 1 << (200 % 8);
    let outside = SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(1),
        None,
        outside_usable,
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    );
    assert!(matches!(
        outside,
        Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap)
    ));

    assert!(matches!(
        SnapshotV2MemoryHotplugQueueState::try_new(8, 7),
        Err(SnapshotV2MemoryHotplugStateBuildError::Queue)
    ));

    let one_block_config = config(128, 128, 128);
    let one_block_space = VirtioMemConfigSpace::new(
        128 * MIB,
        VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
        128 * MIB,
    )
    .with_usable_region_size(128 * MIB);
    let high_bits = SnapshotV2MemoryHotplugState::try_new(
        one_block_config,
        one_block_space,
        None,
        vec![0b1000_0000],
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    );
    assert!(matches!(
        high_bits,
        Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap)
    ));
}

#[test]
fn decoder_rejects_structure_reserved_bitmap_and_cursor_mutations() {
    let encoded = inactive_mmio_state()
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");

    let mut wrong_magic = encoded.clone();
    wrong_magic[0] ^= 0xff;
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &wrong_magic
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidMagic)
    ));

    let mut reserved = encoded.clone();
    reserved[PAYLOAD_OFFSET + 24 + 10] = 1;
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &reserved
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::NonzeroReserved)
    ));

    let bitmap_offset = section_offset(&encoded, 2);
    let mut outside_usable = encoded.clone();
    outside_usable[bitmap_offset + 200 / 8] |= 1 << (200 % 8);
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &outside_usable
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidState(
            SnapshotV2MemoryHotplugStateBuildError::Bitmap
        ))
    ));

    let active = active_pci_state()
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("active fixture should encode");
    let mut cursor_mismatch = active;
    cursor_mismatch[PAYLOAD_OFFSET + 86..PAYLOAD_OFFSET + 88].copy_from_slice(&6_u16.to_le_bytes());
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &cursor_mismatch
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidState(
            SnapshotV2MemoryHotplugStateBuildError::Queue
        ))
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &trailing
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)
    ));
}

#[test]
fn every_header_directory_and_complete_bound_control_fails_closed() {
    let encoded = inactive_mmio_state()
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("fixture should encode");
    for length in [0, 63, PAYLOAD_OFFSET - 1, encoded.len() - 1] {
        assert!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &encoded[..length],
            )
            .is_err(),
            "truncation at {length} should fail"
        );
    }

    let mut control_offsets = vec![8, 10, 12, 14, 16, 20, 24, 32, 40, 48];
    for index in 0..4 {
        let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        control_offsets.extend([
            entry,
            entry + 2,
            entry + 4,
            entry + 8,
            entry + 16,
            entry + 24,
        ]);
    }
    for offset in control_offsets {
        let mut mutated = encoded.clone();
        mutated[offset] ^= 1;
        assert!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "control mutation at byte {offset} should fail"
        );
    }

    let oversized = vec![0; NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES + 1];
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &oversized,
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::TooLarge)
    ));
}

#[test]
fn local_geometry_and_common_virtio_hostile_fields_fail_closed() {
    let active = active_pci_state()
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("active fixture should encode");
    let mut local_mutations = Vec::new();
    for offset in [0, 8, 16, 24, 40, 48, 56, 64, 72] {
        let mut mutated = active.clone();
        mutated[PAYLOAD_OFFSET + offset] ^= 1;
        local_mutations.push(mutated);
    }
    for offset in [34, 81, 88] {
        let mut mutated = active.clone();
        mutated[PAYLOAD_OFFSET + offset] = 1;
        local_mutations.push(mutated);
    }
    let overflowing_total_mib = (u64::MAX / MIB / 128 + 1) * 128;
    for (offset, value) in [
        (0, overflowing_total_mib),
        (8, 4),
        (16, 64),
        (
            40,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value() - 128 * MIB,
        ),
        (40, VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value() + 1),
        (40, aarch64::DRAM_MEM_START + aarch64::DRAM_MEM_MAX_SIZE),
        (56, 130 * MIB),
        (56, 1152 * MIB),
        (64, MIB),
        (64, 1026 * MIB),
        (72, 129 * MIB),
        (72, 1026 * MIB),
    ] {
        let mut mutated = active.clone();
        replace_u64(&mut mutated, PAYLOAD_OFFSET + offset, value);
        local_mutations.push(mutated);
    }
    let mut inactive_cursor_tag = active.clone();
    inactive_cursor_tag[PAYLOAD_OFFSET + 80] = 0;
    local_mutations.push(inactive_cursor_tag);
    for (index, mutated) in local_mutations.into_iter().enumerate() {
        assert!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "local mutation {index} should fail"
        );
    }

    let common = section_offset(&active, 1);
    let mut common_mutations = Vec::new();
    for offset in [0, 8, 20, 24, 25, 36, 37, 40, 48, 56, 64, 66, 67, 70, 71] {
        let mut mutated = active.clone();
        mutated[common + offset] ^= 1;
        common_mutations.push(mutated);
    }
    for (offset, value) in [(26, 2_u16), (28, 2), (30, 3), (32, 255), (34, 3)] {
        let mut mutated = active.clone();
        replace_u16(&mut mutated, common + offset, value);
        common_mutations.push(mutated);
    }
    for (index, mutated) in common_mutations.into_iter().enumerate() {
        assert!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "common mutation {index} should fail"
        );
    }
}

#[test]
fn mmio_and_pci_transport_hostile_fields_fail_closed() {
    let mmio = inactive_mmio_state()
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("MMIO fixture should encode");
    let transport = section_offset(&mmio, 3);
    let mut mmio_mutations = Vec::new();
    for (offset, value) in [(0, 2_u32), (4, 2), (8, 1), (12, 31)] {
        let mut mutated = mmio.clone();
        replace_u32(&mut mutated, transport + offset, value);
        mmio_mutations.push(mutated);
    }
    for (offset, value) in [(16, 0_u64), (24, 0xd000_0001), (32, 0x2000)] {
        let mut mutated = mmio.clone();
        replace_u64(&mut mutated, transport + offset, value);
        mmio_mutations.push(mutated);
    }
    let mut mmio_reserved = mmio;
    mmio_reserved[transport + 40] = 1;
    mmio_mutations.push(mmio_reserved);
    for mutated in mmio_mutations {
        assert!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err()
        );
    }

    let pci = active_pci_state()
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("PCI fixture should encode");
    let transport = section_offset(&pci, 3);
    let mut pci_mutations = Vec::new();
    for offset in [0, 1, 2, 3, 4, 6, 7, 8, 10, 11, 12, 16, 24, 40, 67, 70] {
        let mut mutated = pci.clone();
        mutated[transport + offset] ^= 1;
        pci_mutations.push(mutated);
    }
    for offset in [32, 36] {
        let mut mutated = pci.clone();
        replace_u32(&mut mutated, transport + offset, 2);
        pci_mutations.push(mutated);
    }
    for offset in [64, 65, 66] {
        let mut mutated = pci.clone();
        mutated[transport + offset] = 2;
        pci_mutations.push(mutated);
    }
    for offset in [42, 44, 46, 48, 50] {
        let mut mutated = pci.clone();
        replace_u16(&mut mutated, transport + offset, 0);
        pci_mutations.push(mutated);
    }
    for (index, mutated) in pci_mutations.into_iter().enumerate() {
        assert!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &mutated,
            )
            .is_err(),
            "PCI mutation {index} should fail"
        );
    }
}

#[test]
fn largest_legal_aperture_retains_alternating_bitmap_without_range_allocation() {
    let total_size = aarch64::DRAM_MEM_START + aarch64::DRAM_MEM_MAX_SIZE
        - VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value();
    assert_eq!(total_size, 512 * 1024 * MIB);
    let total_size_mib = total_size / MIB;
    let block_count = usize::try_from(total_size / (2 * MIB))
        .expect("largest legal block count should fit usize");
    let bitmap = vec![0x55; block_count / 8];
    let state = SnapshotV2MemoryHotplugState::try_new(
        config(total_size_mib, 2, 128),
        VirtioMemConfigSpace::new(
            2 * MIB,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
            total_size,
        )
        .with_usable_region_size(total_size)
        .with_plugged_size(total_size / 2),
        None,
        bitmap,
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("largest legal alternating bitmap should validate");

    let ranges = state.plugged_ranges();
    assert_eq!(ranges.len(), block_count / 2);
    assert_eq!(
        ranges.clone().next(),
        Some(SnapshotV2MemoryHotplugPluggedRange {
            start_block: 0,
            block_count: 1,
        })
    );
    assert_eq!(
        ranges.last(),
        Some(SnapshotV2MemoryHotplugPluggedRange {
            start_block: u64::try_from(block_count - 2).expect("last block should fit"),
            block_count: 1,
        })
    );
    let encoded = state
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("largest legal alternating bitmap should encode");
    assert!(encoded.len() < NATIVE_V2_MEMORY_HOTPLUG_WORST_CASE_BYTES);
    assert_eq!(section_length(&encoded, 2), block_count / 8);
    assert_eq!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &encoded,
        )
        .expect("largest legal bitmap should decode"),
        state
    );
}

#[test]
fn empty_full_usable_and_largest_contiguous_bitmaps_round_trip() {
    let empty = SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(0),
        None,
        bitmap_with_ranges(&[]),
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("empty bitmap should validate");
    assert_eq!(empty.plugged_ranges().len(), 0);

    let mut full_usable_bitmap = bitmap_with_ranges(&[]);
    full_usable_bitmap[..16].fill(u8::MAX);
    let full_usable = SnapshotV2MemoryHotplugState::try_new(
        base_config(),
        base_config_space(128),
        None,
        full_usable_bitmap,
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("full usable bitmap should validate");
    assert_eq!(
        full_usable.plugged_ranges().collect::<Vec<_>>(),
        vec![SnapshotV2MemoryHotplugPluggedRange {
            start_block: 0,
            block_count: 128,
        }]
    );

    for state in [empty, full_usable] {
        let encoded = state
            .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
            .expect("boundary bitmap should encode");
        assert_eq!(
            SnapshotV2MemoryHotplugState::decode(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &encoded,
            )
            .expect("boundary bitmap should decode"),
            state
        );
    }

    let total_size = aarch64::DRAM_MEM_START + aarch64::DRAM_MEM_MAX_SIZE
        - VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value();
    let block_count = total_size / (2 * MIB);
    let largest = SnapshotV2MemoryHotplugState::try_new(
        config(total_size / MIB, 2, 128),
        VirtioMemConfigSpace::new(
            2 * MIB,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
            total_size,
        )
        .with_usable_region_size(total_size)
        .with_plugged_size(total_size),
        None,
        vec![u8::MAX; usize::try_from(block_count / 8).expect("largest bitmap should fit usize")],
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("largest contiguous bitmap should validate");
    assert_eq!(
        largest.plugged_ranges().collect::<Vec<_>>(),
        vec![SnapshotV2MemoryHotplugPluggedRange {
            start_block: 0,
            block_count,
        }]
    );
    let encoded = largest
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("largest contiguous bitmap should encode");
    assert_eq!(
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &encoded,
        )
        .expect("largest contiguous bitmap should decode"),
        largest
    );
}

#[test]
fn bitmap_padding_is_canonical_and_checked_before_allocation() {
    let one_block_config = config(128, 128, 128);
    let one_block_space = VirtioMemConfigSpace::new(
        128 * MIB,
        VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
        128 * MIB,
    )
    .with_usable_region_size(128 * MIB)
    .with_plugged_size(128 * MIB);
    let state = SnapshotV2MemoryHotplugState::try_new(
        one_block_config,
        one_block_space,
        None,
        vec![1],
        inactive_virtio(),
        SnapshotV2DeviceTransport::Mmio(mmio_transport()),
    )
    .expect("single-block state should validate");
    let encoded = state
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("single-block state should encode");
    assert_eq!(section_length(&encoded, 2), 8);
    let bitmap_offset = section_offset(&encoded, 2);
    let mut nonzero_padding = encoded;
    nonzero_padding[bitmap_offset + 1] = 1;
    let mut reserve = CountingReserve::default();
    assert!(matches!(
        codec::decode_with_policy(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &nonzero_padding,
            &mut reserve,
        ),
        Err(SnapshotV2MemoryHotplugStateDecodeError::NonzeroReserved)
    ));
    assert_eq!(reserve.calls, 0);
}

#[derive(Default)]
struct CountingReserve {
    calls: usize,
    fail_at: Option<usize>,
}

impl ReservePolicy for CountingReserve {
    fn reserve_vec<T>(&mut self, _values: &mut Vec<T>, _additional: usize) -> Result<(), ()> {
        let call = self.calls;
        self.calls += 1;
        if self.fail_at == Some(call) {
            Err(())
        } else {
            Ok(())
        }
    }
}

#[test]
fn every_encode_and_decode_reservation_failure_is_deterministic() {
    let state = active_pci_state();
    let mut encode_reserve = CountingReserve {
        calls: 0,
        fail_at: Some(0),
    };
    assert!(matches!(
        codec::encode_with_policy(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &state,
            &mut encode_reserve,
        ),
        Err(SnapshotV2MemoryHotplugStateEncodeError::Allocation)
    ));

    let encoded = state
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("active PCI state should encode");
    for fail_at in 0..9 {
        let mut reserve = CountingReserve {
            calls: 0,
            fail_at: Some(fail_at),
        };
        assert!(matches!(
            codec::decode_with_policy(
                NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
                &encoded,
                &mut reserve,
            ),
            Err(SnapshotV2MemoryHotplugStateDecodeError::Allocation)
        ));
        assert_eq!(reserve.calls, fail_at + 1);
    }
    let mut reserve = CountingReserve::default();
    assert!(
        codec::decode_with_policy(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &encoded,
            &mut reserve,
        )
        .is_ok()
    );
    assert_eq!(reserve.calls, 9);
}

#[test]
fn exact_version_and_redaction_boundaries_are_closed() {
    let state = inactive_mmio_state();
    let earlier = SnapshotFormatVersion::new(2, 9, 0);
    let future = SnapshotFormatVersion::new(2, 11, 0);
    assert!(matches!(
        state.encode(earlier),
        Err(SnapshotV2MemoryHotplugStateEncodeError::UnsupportedVersion)
    ));
    assert!(matches!(
        state.encode(future),
        Err(SnapshotV2MemoryHotplugStateEncodeError::UnsupportedVersion)
    ));
    let encoded = state
        .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
        .expect("state should encode");
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(earlier, &encoded),
        Err(SnapshotV2MemoryHotplugStateDecodeError::UnsupportedVersion)
    ));
    assert!(matches!(
        SnapshotV2MemoryHotplugState::decode(future, &encoded),
        Err(SnapshotV2MemoryHotplugStateDecodeError::UnsupportedVersion)
    ));

    let debug = format!("{state:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("536870912"));
    assert!(!debug.contains("1073741824"));
    let error = SnapshotV2MemoryHotplugStateDecodeError::InvalidState(
        SnapshotV2MemoryHotplugStateBuildError::Geometry,
    );
    assert!(!format!("{error:?}").contains("536870912"));
    assert!(!error.to_string().contains("536870912"));
    assert!(format!("{:?}", state.plugged_ranges()).contains(REDACTED));
    assert!(
        format!(
            "{:?}",
            state
                .plugged_ranges()
                .next()
                .expect("fixture should contain a range")
        )
        .contains(REDACTED)
    );
    assert!(
        format!(
            "{:?}",
            SnapshotV2MemoryHotplugQueueState::try_new(7, 7)
                .expect("equal cursors should validate")
        )
        .contains(REDACTED)
    );
    let capture_error = SnapshotV2MemoryHotplugStateCaptureError::Build {
        source: SnapshotV2MemoryHotplugStateBuildError::Geometry,
    };
    assert!(!format!("{capture_error:?}").contains("536870912"));
    assert!(!capture_error.to_string().contains("536870912"));
}
