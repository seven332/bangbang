use std::collections::TryReserveError;
use std::fmt::Write as _;
use std::mem::size_of;

use crc64::crc64;

use crate::memory::{GuestAddress, GuestMemoryRange, aarch64};
use crate::snapshot_artifact::{
    NativeSnapshotArtifactState, NativeSnapshotArtifactStateError,
    NativeV2SnapshotCandidateStateError,
};
use crate::snapshot_format_v2::{
    NATIVE_V2_DIFF_COMPONENT_KEY, NATIVE_V2_MEMORY_COMPONENT_KEY, SnapshotV2Component,
    SnapshotV2ComponentDisposition, SnapshotV2ComponentKey, SnapshotV2DecodeError,
    decode_snapshot_v2_state_with_compatibility_version,
    encode_snapshot_v2_state_with_compatibility_version,
};
use crate::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError, SnapshotV2MemoryImageId,
    decode_snapshot_v2_memory_binding_payload, snapshot_v2_memory_binding_from_ranges_for_test,
};
use crate::snapshot_vsock_v2_12::NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION;

use super::*;

const RESULT_ID: SnapshotV2MemoryImageId =
    SnapshotV2MemoryImageId::from_bytes(*b"result-image-id!");
const OTHER_RESULT_ID: SnapshotV2MemoryImageId =
    SnapshotV2MemoryImageId::from_bytes(*b"other-result-id!");
const BASE_ID: SnapshotV2MemoryImageId = SnapshotV2MemoryImageId::from_bytes(*b"base-image-id!!!");
const ROOT_ZERO_FIXTURE: &str = include_str!("fixtures/root-zero.hex");
const PREDECESSOR_DYNAMIC_FIXTURE: &str = include_str!("fixtures/predecessor-dynamic.hex");

const VERSION_MINOR_OFFSET: usize = 10;
const HEADER_BYTES_OFFSET: usize = 14;
const PROFILE_OFFSET: usize = 16;
const BASE_KIND_OFFSET: usize = 18;
const FLAGS_OFFSET: usize = 20;
const GUEST_GRANULE_OFFSET: usize = 24;
const EXTENT_COUNT_OFFSET: usize = 28;
const RESULT_BINDING_LENGTH_OFFSET: usize = 32;
const EXTENT_BYTES_OFFSET: usize = 36;
const METADATA_LENGTH_OFFSET: usize = 40;
const DATA_OFFSET_OFFSET: usize = 48;
const FILE_LENGTH_OFFSET: usize = 56;
const CHECKSUM_OFFSET: usize = 64;
const BASE_IMAGE_ID_OFFSET: usize = 72;
const PREDECESSOR_BINDING_LENGTH_OFFSET: usize = 88;
const RESERVED_OFFSET: usize = 92;
const MEMORY_BINDING_CHECKSUM_OFFSET: usize = 48;
const EXTENT_GPA_OFFSET: usize = 0;
const EXTENT_LENGTH_OFFSET: usize = 8;
const EXTENT_FILE_OFFSET: usize = 16;

fn range(start: u64, size: u64) -> GuestMemoryRange {
    GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should validate")
}

fn result_binding_with_id(
    image_id: SnapshotV2MemoryImageId,
    ranges: &[GuestMemoryRange],
) -> SnapshotV2MemoryBinding {
    snapshot_v2_memory_binding_from_ranges_for_test(
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        image_id,
        ranges,
    )
    .expect("test result binding should validate")
}

fn result_binding(ranges: &[GuestMemoryRange]) -> SnapshotV2MemoryBinding {
    result_binding_with_id(RESULT_ID, ranges)
}

fn one_region_result() -> SnapshotV2MemoryBinding {
    result_binding(&[range(aarch64::DRAM_MEM_START, 64 * 1024)])
}

fn predecessor_binding_with_id(
    image_id: SnapshotV2MemoryImageId,
    ranges: &[GuestMemoryRange],
) -> SnapshotV2MemoryBinding {
    snapshot_v2_memory_binding_from_ranges_for_test(
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
        image_id,
        ranges,
    )
    .expect("test predecessor binding should validate")
}

fn predecessor_binding(ranges: &[GuestMemoryRange]) -> SnapshotV2MemoryBinding {
    predecessor_binding_with_id(BASE_ID, ranges)
}

fn image_base(ranges: &[GuestMemoryRange]) -> SnapshotV2DiffBase {
    SnapshotV2DiffBase::Image(predecessor_binding(ranges))
}

fn image_base_with_id(
    image_id: SnapshotV2MemoryImageId,
    ranges: &[GuestMemoryRange],
) -> SnapshotV2DiffBase {
    SnapshotV2DiffBase::Image(predecessor_binding_with_id(image_id, ranges))
}

fn data_directory_offset(base: &SnapshotV2DiffBase, result: &SnapshotV2MemoryBinding) -> usize {
    NATIVE_V2_DIFF_HEADER_BYTES
        + result.encode().expect("result binding should encode").len()
        + base
            .binding()
            .map(|binding| {
                binding
                    .encode()
                    .expect("predecessor binding should encode")
                    .len()
            })
            .unwrap_or(0)
}

fn encoded_layer(
    base: SnapshotV2DiffBase,
    result: SnapshotV2MemoryBinding,
    ranges: &[GuestMemoryRange],
) -> Vec<u8> {
    SnapshotV2DiffLayerBinding::try_from_ranges(base, result, ranges)
        .expect("test layer should build")
        .encode()
        .expect("test layer should encode")
}

#[test]
fn root_zero_empty_and_selected_layers_round_trip_canonically() {
    let result = one_region_result();
    let empty =
        SnapshotV2DiffLayerBinding::try_from_ranges(SnapshotV2DiffBase::Zero, result.clone(), &[])
            .expect("empty root layer should validate");
    assert_eq!(empty.version(), NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION);
    assert_eq!(empty.base(), &SnapshotV2DiffBase::Zero);
    assert_eq!(empty.result(), &result);
    assert!(empty.data_extents().is_empty());
    assert_eq!(empty.metadata_length(), 184);
    assert_eq!(empty.data_offset(), NATIVE_V2_MEMORY_ALIGNMENT);
    assert_eq!(empty.file_length(), NATIVE_V2_MEMORY_ALIGNMENT);
    let encoded = empty.encode().expect("empty root layer should encode");
    assert_eq!(encoded.len(), 184);
    assert_eq!(
        SnapshotV2DiffLayerBinding::decode(&encoded).expect("empty root layer should decode"),
        empty
    );
    assert_eq!(
        encoded,
        decode_hex(ROOT_ZERO_FIXTURE),
        "root-zero fixture changed; actual {}",
        encode_hex(&encoded)
    );

    let selected_range = range(aarch64::DRAM_MEM_START + 4096, 8192);
    let selected = SnapshotV2DiffLayerBinding::try_from_ranges(
        SnapshotV2DiffBase::Zero,
        result,
        &[selected_range],
    )
    .expect("selected root layer should validate");
    assert_eq!(selected.data_extents().len(), 1);
    assert_eq!(selected.data_extents()[0].range(), selected_range);
    assert_eq!(
        selected.data_extents()[0].file_offset(),
        NATIVE_V2_MEMORY_ALIGNMENT
    );
    assert_eq!(
        selected.file_length(),
        NATIVE_V2_MEMORY_ALIGNMENT + selected_range.size()
    );
    let encoded = selected.encode().expect("selected layer should encode");
    assert_eq!(
        SnapshotV2DiffLayerBinding::decode(&encoded)
            .expect("selected layer should decode")
            .encode()
            .expect("decoded layer should re-encode"),
        encoded
    );
}

#[test]
fn predecessor_dynamic_fixture_round_trips_and_retains_packed_offsets() {
    let predecessor_topology = [
        range(aarch64::DRAM_MEM_START, 32 * 1024),
        range(aarch64::DRAM_MEM_START + 128 * 1024, 64 * 1024),
    ];
    let topology = [
        range(aarch64::DRAM_MEM_START, 16 * 1024),
        range(aarch64::DRAM_MEM_START + 128 * 1024, 32 * 1024),
    ];
    let result = result_binding(&topology);
    let selected = [
        range(aarch64::DRAM_MEM_START + 4096, 4096),
        range(aarch64::DRAM_MEM_START + 128 * 1024 + 8192, 8192),
    ];
    let layer = SnapshotV2DiffLayerBinding::try_from_ranges(
        image_base(&predecessor_topology),
        result,
        &selected,
    )
    .expect("dynamic predecessor layer should validate");
    assert_eq!(
        layer
            .base()
            .binding()
            .expect("image layer should retain predecessor")
            .extents()
            .iter()
            .map(|extent| extent.range())
            .collect::<Vec<_>>(),
        predecessor_topology
    );
    assert_eq!(layer.data_extents().len(), 2);
    assert_eq!(layer.data_extents()[0].file_offset(), layer.data_offset());
    assert_eq!(
        layer.data_extents()[1].file_offset(),
        layer.data_offset() + selected[0].size()
    );
    assert_eq!(
        layer.file_length(),
        layer.data_offset() + selected.iter().map(|range| range.size()).sum::<u64>()
    );
    let encoded = layer.encode().expect("dynamic layer should encode");
    assert_eq!(
        SnapshotV2DiffLayerBinding::decode(&encoded).expect("dynamic layer should decode"),
        layer
    );
    assert_eq!(
        encoded,
        decode_hex(PREDECESSOR_DYNAMIC_FIXTURE),
        "predecessor fixture changed; actual {}",
        encode_hex(&encoded)
    );
}

#[test]
fn adjacent_ranges_are_canonical_only_across_distinct_result_regions() {
    let start = aarch64::DRAM_MEM_START;
    let result = result_binding(&[range(start, 8192), range(start + 8192, 8192)]);
    let ranges = [range(start + 4096, 4096), range(start + 8192, 4096)];
    SnapshotV2DiffLayerBinding::try_from_ranges(SnapshotV2DiffBase::Zero, result, &ranges)
        .expect("adjacent ranges in distinct result regions should stay separate");

    let result = result_binding(&[range(start, 16 * 1024)]);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::try_from_ranges(SnapshotV2DiffBase::Zero, result, &ranges,),
        Err(SnapshotV2DiffLayerBindingError::NonCanonicalExtent)
    ));
}

#[test]
fn checked_construction_rejects_identity_version_and_extent_failures() {
    let start = aarch64::DRAM_MEM_START;
    assert!(
        snapshot_v2_memory_binding_from_ranges_for_test(
            NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
            RESULT_ID,
            &[],
        )
        .is_err()
    );
    let result = result_binding(&[range(start, 64 * 1024)]);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::try_from_ranges(
            image_base_with_id(RESULT_ID, &[range(start, 64 * 1024)]),
            result.clone(),
            &[],
        ),
        Err(SnapshotV2DiffLayerBindingError::BaseResultIdentityConflict)
    ));

    let old_result = snapshot_v2_memory_binding_from_ranges_for_test(
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
        RESULT_ID,
        &[range(start, 64 * 1024)],
    )
    .expect("old result binding should validate independently");
    assert!(matches!(
        SnapshotV2DiffLayerBinding::try_from_ranges(SnapshotV2DiffBase::Zero, old_result, &[],),
        Err(SnapshotV2DiffLayerBindingError::UnsupportedVersion)
    ));

    for invalid in [
        vec![range(start + 1, 4096)],
        vec![range(start, 4097)],
        vec![range(start + 64 * 1024, 4096)],
        vec![range(start + 8192, 4096), range(start, 4096)],
        vec![range(start, 8192), range(start + 4096, 4096)],
    ] {
        assert!(
            SnapshotV2DiffLayerBinding::try_from_ranges(
                SnapshotV2DiffBase::Zero,
                result.clone(),
                &invalid,
            )
            .is_err()
        );
    }

    let split_result = result_binding(&[range(start, 8192), range(start + 16 * 1024, 8192)]);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::try_from_ranges(
            SnapshotV2DiffBase::Zero,
            split_result,
            &[range(start + 4096, 16 * 1024)],
        ),
        Err(SnapshotV2DiffLayerBindingError::InvalidExtent)
    ));

    let excessive = vec![range(start, 4096); NATIVE_V2_DIFF_MAX_EXTENTS + 1];
    assert!(matches!(
        SnapshotV2DiffLayerBinding::try_from_ranges(SnapshotV2DiffBase::Zero, result, &excessive,),
        Err(SnapshotV2DiffLayerBindingError::CountOutOfBounds)
    ));
}

#[test]
fn maximum_metadata_shape_is_exact_and_within_the_frozen_cap() {
    let mut topology = Vec::new();
    topology.reserve_exact(crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAX_EXTENTS);
    let mut selected = Vec::new();
    selected.reserve_exact(NATIVE_V2_DIFF_MAX_EXTENTS);
    for region_index in 0..crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAX_EXTENTS {
        let region_start = aarch64::DRAM_MEM_START
            + u64::try_from(region_index).expect("test index should fit") * 128 * 1024;
        topology.push(range(region_start, 64 * 1024));
        for page_index in 0..8_u64 {
            selected.push(range(region_start + page_index * 8192, 4096));
        }
    }
    assert_eq!(selected.len(), NATIVE_V2_DIFF_MAX_EXTENTS);
    let layer = SnapshotV2DiffLayerBinding::try_from_ranges(
        image_base(&topology),
        result_binding(&topology),
        &selected,
    )
    .expect("maximum metadata layer should validate");
    assert_eq!(
        layer
            .base()
            .binding()
            .expect("maximum image layer should retain predecessor")
            .extents()
            .len(),
        crate::snapshot_memory_v2::NATIVE_V2_MEMORY_MAX_EXTENTS
    );
    assert_eq!(
        usize::try_from(layer.metadata_length()).expect("metadata should fit"),
        MAX_ENCODED_METADATA_BYTES
    );
    assert_eq!(
        usize::try_from(layer.data_offset()).expect("data offset should fit"),
        MAX_DATA_OFFSET
    );
    assert_eq!(
        layer
            .encode()
            .expect("maximum metadata should encode")
            .len(),
        MAX_ENCODED_METADATA_BYTES
    );
}

#[test]
fn fixed_header_and_integrity_mutations_fail_closed() {
    let result = one_region_result();
    let encoded = encoded_layer(SnapshotV2DiffBase::Zero, result, &[]);

    let mut invalid = encoded.clone();
    invalid[0] ^= 0x80;
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::InvalidMagic)
    ));

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, VERSION_MINOR_OFFSET, 14);
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::UnsupportedVersion)
    ));

    for mutation in [
        HeaderMutation::U16(HEADER_BYTES_OFFSET, 64),
        HeaderMutation::U16(PROFILE_OFFSET, 2),
        HeaderMutation::U32(FLAGS_OFFSET, 1),
        HeaderMutation::U32(GUEST_GRANULE_OFFSET, 16 * 1024),
        HeaderMutation::U32(EXTENT_BYTES_OFFSET, 16),
        HeaderMutation::Byte(RESERVED_OFFSET, 1),
    ] {
        let mut invalid = encoded.clone();
        mutation.apply(&mut invalid);
        repair_checksum(&mut invalid);
        assert!(matches!(
            SnapshotV2DiffLayerBinding::decode(&invalid),
            Err(SnapshotV2DiffLayerBindingError::InvalidHeader)
        ));
    }

    let mut invalid = encoded.clone();
    replace_u16(&mut invalid, BASE_KIND_OFFSET, 7);
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::InvalidBase)
    ));

    let mut invalid = encoded.clone();
    *invalid
        .get_mut(BASE_IMAGE_ID_OFFSET)
        .expect("base identity byte should exist") = 1;
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::InvalidBase)
    ));

    let mut invalid = encoded.clone();
    replace_u32(&mut invalid, PREDECESSOR_BINDING_LENGTH_OFFSET, 1);
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::InvalidBase)
    ));

    let mut invalid = encoded.clone();
    replace_u32(
        &mut invalid,
        EXTENT_COUNT_OFFSET,
        u32::try_from(NATIVE_V2_DIFF_MAX_EXTENTS + 1).expect("test count should fit"),
    );
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::CountOutOfBounds)
    ));

    for (offset, value) in [
        (RESULT_BINDING_LENGTH_OFFSET, 0_u64),
        (METADATA_LENGTH_OFFSET, 0),
        (DATA_OFFSET_OFFSET, 0),
    ] {
        let mut invalid = encoded.clone();
        if offset == RESULT_BINDING_LENGTH_OFFSET {
            replace_u32(
                &mut invalid,
                offset,
                u32::try_from(value).expect("value should fit"),
            );
        } else {
            replace_u64(&mut invalid, offset, value);
        }
        repair_checksum(&mut invalid);
        assert!(SnapshotV2DiffLayerBinding::decode(&invalid).is_err());
    }

    let mut invalid = encoded.clone();
    replace_u64(&mut invalid, FILE_LENGTH_OFFSET, 0);
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::FileLengthMismatch)
    ));

    let mut corrupt = encoded.clone();
    *corrupt.last_mut().expect("metadata should be nonempty") ^= 0x40;
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&corrupt),
        Err(SnapshotV2DiffLayerBindingError::IntegrityMismatch)
    ));
    assert!(SnapshotV2DiffLayerBinding::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut trailing = encoded;
    trailing.push(0);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&trailing),
        Err(SnapshotV2DiffLayerBindingError::InvalidLength)
    ));
}

#[test]
fn malformed_embedded_result_binding_fails_as_a_result_binding() {
    let encoded = encoded_layer(SnapshotV2DiffBase::Zero, one_region_result(), &[]);

    for offset in [
        NATIVE_V2_DIFF_HEADER_BYTES,
        NATIVE_V2_DIFF_HEADER_BYTES + 10,
    ] {
        let mut invalid = encoded.clone();
        *invalid
            .get_mut(offset)
            .expect("embedded result byte should exist") ^= 0x40;
        repair_checksum(&mut invalid);
        assert!(matches!(
            SnapshotV2DiffLayerBinding::decode(&invalid),
            Err(SnapshotV2DiffLayerBindingError::ResultBinding { .. })
        ));
    }
}

#[test]
fn malformed_embedded_predecessor_binding_and_pairing_fail_closed() {
    let start = aarch64::DRAM_MEM_START;
    let topology = [range(start, 64 * 1024)];
    let result = result_binding(&topology);
    let predecessor = predecessor_binding(&topology);
    let predecessor_bytes = predecessor
        .encode()
        .expect("predecessor binding should encode");
    let predecessor_start =
        NATIVE_V2_DIFF_HEADER_BYTES + result.encode().expect("result binding should encode").len();
    let predecessor_end = predecessor_start + predecessor_bytes.len();
    let encoded = encoded_layer(SnapshotV2DiffBase::Image(predecessor), result.clone(), &[]);

    let mut missing = encoded.clone();
    replace_u32(&mut missing, PREDECESSOR_BINDING_LENGTH_OFFSET, 0);
    repair_checksum(&mut missing);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&missing),
        Err(SnapshotV2DiffLayerBindingError::InvalidBase)
    ));

    let mut zero_with_binding = encoded.clone();
    replace_u16(&mut zero_with_binding, BASE_KIND_OFFSET, 0);
    zero_with_binding[BASE_IMAGE_ID_OFFSET..BASE_IMAGE_ID_OFFSET + 16].fill(0);
    repair_checksum(&mut zero_with_binding);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&zero_with_binding),
        Err(SnapshotV2DiffLayerBindingError::InvalidBase)
    ));

    let mut bad_magic = encoded.clone();
    *bad_magic
        .get_mut(predecessor_start)
        .expect("predecessor magic byte should exist") ^= 0x80;
    repair_checksum(&mut bad_magic);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&bad_magic),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBinding {
            source: SnapshotV2MemoryBindingError::InvalidMagic
        })
    ));

    let mut unsupported = encoded.clone();
    replace_u16(
        &mut unsupported,
        predecessor_start + VERSION_MINOR_OFFSET,
        14,
    );
    repair_memory_binding_checksum(
        unsupported
            .get_mut(predecessor_start..predecessor_end)
            .expect("predecessor binding should exist"),
    );
    repair_checksum(&mut unsupported);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&unsupported),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBinding {
            source: SnapshotV2MemoryBindingError::UnsupportedVersion
        })
    ));

    let mut corrupt = encoded.clone();
    *corrupt
        .get_mut(predecessor_start + MEMORY_BINDING_CHECKSUM_OFFSET)
        .expect("predecessor checksum byte should exist") ^= 1;
    repair_checksum(&mut corrupt);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&corrupt),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBinding {
            source: SnapshotV2MemoryBindingError::IntegrityMismatch
        })
    ));

    let mut truncated = encoded.clone();
    truncated.truncate(
        truncated
            .len()
            .checked_sub(1)
            .expect("predecessor metadata should be nonempty"),
    );
    replace_u32(
        &mut truncated,
        PREDECESSOR_BINDING_LENGTH_OFFSET,
        u32::try_from(predecessor_bytes.len() - 1).expect("predecessor length should fit"),
    );
    replace_u64(
        &mut truncated,
        METADATA_LENGTH_OFFSET,
        u64::try_from(predecessor_end - 1).expect("metadata length should fit"),
    );
    repair_checksum(&mut truncated);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&truncated),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBinding {
            source: SnapshotV2MemoryBindingError::InvalidLength
        })
    ));

    let mut trailing = encoded.clone();
    trailing.insert(predecessor_end, 0);
    replace_u32(
        &mut trailing,
        PREDECESSOR_BINDING_LENGTH_OFFSET,
        u32::try_from(predecessor_bytes.len() + 1).expect("predecessor length should fit"),
    );
    replace_u64(
        &mut trailing,
        METADATA_LENGTH_OFFSET,
        u64::try_from(predecessor_end + 1).expect("metadata length should fit"),
    );
    repair_checksum(&mut trailing);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&trailing),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBinding {
            source: SnapshotV2MemoryBindingError::InvalidLength
        })
    ));

    let mut noncanonical = encoded.clone();
    replace_u64(
        &mut noncanonical,
        predecessor_start
            + crate::snapshot_memory_v2::NATIVE_V2_MEMORY_HEADER_BYTES
            + EXTENT_FILE_OFFSET,
        crate::snapshot_memory_v2::NATIVE_V2_MEMORY_ALIGNMENT + 4096,
    );
    repair_memory_binding_checksum(
        noncanonical
            .get_mut(predecessor_start..predecessor_end)
            .expect("predecessor binding should exist"),
    );
    repair_checksum(&mut noncanonical);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&noncanonical),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBinding {
            source: SnapshotV2MemoryBindingError::NonCanonicalFileOffset
        })
    ));

    let conflicting = predecessor_binding_with_id(RESULT_ID, &topology)
        .encode()
        .expect("conflicting predecessor should encode");
    assert_eq!(conflicting.len(), predecessor_bytes.len());
    let mut conflict = encoded;
    conflict
        .get_mut(predecessor_start..predecessor_end)
        .expect("predecessor binding should exist")
        .copy_from_slice(&conflicting);
    conflict[BASE_IMAGE_ID_OFFSET..BASE_IMAGE_ID_OFFSET + 16]
        .copy_from_slice(&RESULT_ID.to_bytes());
    repair_checksum(&mut conflict);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&conflict),
        Err(SnapshotV2DiffLayerBindingError::BaseResultIdentityConflict)
    ));
}

#[test]
fn predecessor_identity_and_extent_mutations_reject_canonically() {
    let start = aarch64::DRAM_MEM_START;
    let result = result_binding(&[range(start, 64 * 1024)]);
    let base = image_base(&[range(start, 64 * 1024)]);
    let ranges = [range(start, 4096), range(start + 3 * 4096, 4096)];
    let directory = data_directory_offset(&base, &result);
    let encoded = encoded_layer(base, result.clone(), &ranges);

    let mut invalid = encoded.clone();
    invalid[BASE_IMAGE_ID_OFFSET..BASE_IMAGE_ID_OFFSET + 16].copy_from_slice(&RESULT_ID.to_bytes());
    repair_checksum(&mut invalid);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&invalid),
        Err(SnapshotV2DiffLayerBindingError::PredecessorBindingMismatch)
    ));

    for (field_offset, value, expected) in [
        (directory + EXTENT_GPA_OFFSET, start + 1, "unaligned GPA"),
        (
            directory + EXTENT_GPA_OFFSET,
            u64::MAX - (4096 - 1),
            "overflowing GPA range",
        ),
        (directory + EXTENT_LENGTH_OFFSET, 0, "empty range"),
        (directory + EXTENT_LENGTH_OFFSET, 4097, "unaligned length"),
        (
            directory + EXTENT_GPA_OFFSET,
            start + 64 * 1024,
            "outside result",
        ),
        (
            directory + NATIVE_V2_DIFF_EXTENT_BYTES + EXTENT_GPA_OFFSET,
            start,
            "unordered range",
        ),
        (
            directory + NATIVE_V2_DIFF_EXTENT_BYTES + EXTENT_GPA_OFFSET,
            start + 4096,
            "uncoalesced adjacency",
        ),
        (
            directory + EXTENT_FILE_OFFSET,
            NATIVE_V2_MEMORY_ALIGNMENT + 4096,
            "noncanonical offset",
        ),
    ] {
        let mut invalid = encoded.clone();
        replace_u64(&mut invalid, field_offset, value);
        repair_checksum(&mut invalid);
        assert!(
            SnapshotV2DiffLayerBinding::decode(&invalid).is_err(),
            "{expected} should reject"
        );
    }

    let split_result = result_binding(&[range(start, 8192), range(start + 16 * 1024, 8192)]);
    let split_base = image_base(&[range(start, 8192), range(start + 16 * 1024, 8192)]);
    let split_directory = data_directory_offset(&split_base, &split_result);
    let mut crossing = encoded_layer(
        split_base,
        split_result.clone(),
        &[range(start + 4096, 4096)],
    );
    replace_u64(
        &mut crossing,
        split_directory + EXTENT_LENGTH_OFFSET,
        16 * 1024,
    );
    replace_u64(
        &mut crossing,
        FILE_LENGTH_OFFSET,
        NATIVE_V2_MEMORY_ALIGNMENT + 16 * 1024,
    );
    repair_checksum(&mut crossing);
    assert!(matches!(
        SnapshotV2DiffLayerBinding::decode(&crossing),
        Err(SnapshotV2DiffLayerBindingError::InvalidExtent)
    ));
}

#[test]
fn state_decoder_cross_checks_component_profile_and_result_binding() {
    let result = one_region_result();
    let layer = SnapshotV2DiffLayerBinding::try_from_ranges(
        image_base(&[range(aarch64::DRAM_MEM_START, 64 * 1024)]),
        result.clone(),
        &[range(aarch64::DRAM_MEM_START, 4096)],
    )
    .expect("state test layer should validate");
    let memory_payload = result.encode().expect("memory payload should encode");
    let diff_payload = layer.encode().expect("Diff payload should encode");
    let memory = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &memory_payload,
    );
    let diff = SnapshotV2Component::new(
        NATIVE_V2_DIFF_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &diff_payload,
    );
    let state_bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        &[],
        &[memory, diff],
    )
    .expect("exact Diff state should encode explicitly");
    let state = decode_snapshot_v2_state_with_compatibility_version(
        &state_bytes,
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
    )
    .expect("exact Diff state should decode explicitly");
    assert_eq!(
        decode_snapshot_v2_diff_layer_binding(&state)
            .expect("matching state and Diff should cross-validate"),
        layer
    );

    let old_state_bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
        &[],
        &[memory],
    )
    .expect("current memory-only state should encode");
    let old_state = decode_snapshot_v2_state_with_compatibility_version(
        &old_state_bytes,
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
    )
    .expect("current memory-only state should decode");
    assert!(matches!(
        decode_snapshot_v2_diff_layer_binding(&old_state),
        Err(SnapshotV2DiffStateError::UnsupportedVersion)
    ));

    for components in [
        vec![memory],
        vec![
            memory,
            SnapshotV2Component::new(
                SnapshotV2ComponentKey::new(NATIVE_V2_DIFF_COMPONENT_KEY.kind(), 1),
                SnapshotV2ComponentDisposition::Semantic,
                &diff_payload,
            ),
        ],
        vec![
            memory,
            SnapshotV2Component::new(
                NATIVE_V2_DIFF_COMPONENT_KEY,
                SnapshotV2ComponentDisposition::NonSemantic,
                &diff_payload,
            ),
        ],
        vec![
            memory,
            diff,
            SnapshotV2Component::new(
                SnapshotV2ComponentKey::new(NATIVE_V2_DIFF_COMPONENT_KEY.kind(), 1),
                SnapshotV2ComponentDisposition::Semantic,
                &diff_payload,
            ),
        ],
    ] {
        let bytes = encode_snapshot_v2_state_with_compatibility_version(
            NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
            &[],
            &components,
        )
        .expect("structural invalid-profile fixture should encode");
        let state = decode_snapshot_v2_state_with_compatibility_version(
            &bytes,
            NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        )
        .expect("structural invalid-profile fixture should decode");
        assert!(decode_snapshot_v2_diff_layer_binding(&state).is_err());
    }

    let bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        &[],
        &[diff],
    )
    .expect("missing-memory state should encode structurally");
    let state = decode_snapshot_v2_state_with_compatibility_version(
        &bytes,
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
    )
    .expect("missing-memory state should decode structurally");
    assert!(matches!(
        decode_snapshot_v2_diff_layer_binding(&state),
        Err(SnapshotV2DiffStateError::Memory { .. })
    ));

    let other = result_binding_with_id(
        OTHER_RESULT_ID,
        &[range(aarch64::DRAM_MEM_START, 64 * 1024)],
    );
    let other_payload = other.encode().expect("other result should encode");
    let mismatched_memory = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &other_payload,
    );
    let bytes = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        &[],
        &[mismatched_memory, diff],
    )
    .expect("mismatched state should encode structurally");
    let state = decode_snapshot_v2_state_with_compatibility_version(
        &bytes,
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mismatched state should decode structurally");
    assert!(matches!(
        decode_snapshot_v2_diff_layer_binding(&state),
        Err(SnapshotV2DiffStateError::ResultBindingMismatch)
    ));

    assert!(matches!(
        NativeSnapshotArtifactState::from_current_v2(state_bytes),
        Err(NativeSnapshotArtifactStateError::CurrentV2Profile(
            NativeV2SnapshotCandidateStateError::Format(
                SnapshotV2DecodeError::UnsupportedVersion {
                    found: NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
                    supported: NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION,
                }
            )
        ))
    ));
}

#[test]
fn construction_and_decode_inject_each_new_allocation_boundary() {
    let result = one_region_result();
    let selected = [range(aarch64::DRAM_MEM_START, 4096)];
    for fail_at in 0..2 {
        let mut reserve = FailingReserve::new(fail_at);
        assert!(matches!(
            SnapshotV2DiffLayerBinding::try_from_ranges_with_reserve(
                SnapshotV2DiffBase::Zero,
                result.clone(),
                &selected,
                &mut reserve,
            ),
            Err(SnapshotV2DiffLayerBindingError::MetadataAllocationFailed { .. })
        ));
    }

    let encoded = encoded_layer(SnapshotV2DiffBase::Zero, result.clone(), &selected);
    for fail_at in 0..3 {
        let mut reserve = FailingReserve::new(fail_at);
        let error = super::codec::decode_with_reserve(&encoded, &mut reserve)
            .expect_err("injected root decode allocation should fail");
        if fail_at == 0 {
            assert!(matches!(
                error,
                SnapshotV2DiffLayerBindingError::ResultBinding {
                    source: SnapshotV2MemoryBindingError::MetadataAllocationFailed { .. }
                }
            ));
        } else {
            assert!(matches!(
                error,
                SnapshotV2DiffLayerBindingError::MetadataAllocationFailed { .. }
            ));
        }
    }

    let image = encoded_layer(
        image_base(&[range(aarch64::DRAM_MEM_START, 64 * 1024)]),
        result,
        &selected,
    );
    for fail_at in 0..4 {
        let mut reserve = FailingReserve::new(fail_at);
        let error = super::codec::decode_with_reserve(&image, &mut reserve)
            .expect_err("injected image decode allocation should fail");
        match fail_at {
            0 => assert!(matches!(
                error,
                SnapshotV2DiffLayerBindingError::ResultBinding {
                    source: SnapshotV2MemoryBindingError::MetadataAllocationFailed { .. }
                }
            )),
            1 => assert!(matches!(
                error,
                SnapshotV2DiffLayerBindingError::PredecessorBinding {
                    source: SnapshotV2MemoryBindingError::MetadataAllocationFailed { .. }
                }
            )),
            _ => assert!(matches!(
                error,
                SnapshotV2DiffLayerBindingError::MetadataAllocationFailed { .. }
            )),
        }
    }
}

#[test]
fn diagnostics_redact_all_private_layer_values() {
    let result = one_region_result();
    let layer = SnapshotV2DiffLayerBinding::try_from_ranges(
        image_base(&[range(aarch64::DRAM_MEM_START, 64 * 1024)]),
        result,
        &[range(aarch64::DRAM_MEM_START + 4096, 4096)],
    )
    .expect("redaction layer should validate");
    let debug = format!("{layer:?} {:?} {:?}", layer.base(), layer.data_extents()[0]);
    for private in [
        "base-image-id!!!",
        "result-image-id!",
        "40001000",
        &layer.file_length().to_string(),
        &layer.metadata_checksum().to_string(),
    ] {
        assert!(
            !debug.contains(private),
            "debug leaked a private layer value"
        );
    }
    assert!(debug.contains("<redacted>"));
    let error = SnapshotV2DiffLayerBindingError::InvalidExtent.to_string();
    assert!(!error.contains("40001000"));
}

enum HeaderMutation {
    U16(usize, u16),
    U32(usize, u32),
    Byte(usize, u8),
}

impl HeaderMutation {
    fn apply(self, bytes: &mut [u8]) {
        match self {
            Self::U16(offset, value) => replace_u16(bytes, offset, value),
            Self::U32(offset, value) => replace_u32(bytes, offset, value),
            Self::Byte(offset, value) => {
                *bytes.get_mut(offset).expect("test byte should exist") = value;
            }
        }
    }
}

struct FailingReserve {
    fail_at: usize,
    calls: usize,
}

impl FailingReserve {
    const fn new(fail_at: usize) -> Self {
        Self { fail_at, calls: 0 }
    }

    fn check(&mut self) -> Result<(), TryReserveError> {
        let call = self.calls;
        self.calls += 1;
        if call == self.fail_at {
            let mut impossible = Vec::<u8>::new();
            return Err(impossible
                .try_reserve(usize::MAX)
                .expect_err("impossible reservation should fail"));
        }
        Ok(())
    }
}

impl ReservePolicy for FailingReserve {
    fn decode_memory_binding(
        &mut self,
        bytes: &[u8],
    ) -> Result<SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError> {
        self.check()
            .map_err(|source| SnapshotV2MemoryBindingError::MetadataAllocationFailed { source })?;
        decode_snapshot_v2_memory_binding_payload(bytes)
    }

    fn reserve_extents(
        &mut self,
        value: &mut Vec<SnapshotV2DiffDataExtent>,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        self.check()?;
        value.try_reserve_exact(additional)
    }

    fn reserve_bytes(
        &mut self,
        value: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        self.check()?;
        value.try_reserve_exact(additional)
    }
}

fn repair_checksum(bytes: &mut [u8]) {
    let checksum = super::codec::metadata_checksum(bytes).expect("test checksum should calculate");
    replace_u64(bytes, CHECKSUM_OFFSET, checksum);
}

fn repair_memory_binding_checksum(bytes: &mut [u8]) {
    let before = bytes
        .get(..MEMORY_BINDING_CHECKSUM_OFFSET)
        .expect("memory checksum prefix should exist");
    let after = bytes
        .get(MEMORY_BINDING_CHECKSUM_OFFSET + size_of::<u64>()..)
        .expect("memory checksum suffix should exist");
    let checksum = crc64(0, before);
    let checksum = crc64(checksum, &[0_u8; size_of::<u64>()]);
    let checksum = crc64(checksum, after);
    replace_u64(bytes, MEMORY_BINDING_CHECKSUM_OFFSET, checksum);
}

fn replace_u16(bytes: &mut [u8], offset: usize, value: u16) {
    let target = bytes
        .get_mut(offset..offset + size_of::<u16>())
        .expect("test u16 field should exist");
    target.copy_from_slice(&value.to_le_bytes());
}

fn replace_u32(bytes: &mut [u8], offset: usize, value: u32) {
    let target = bytes
        .get_mut(offset..offset + size_of::<u32>())
        .expect("test u32 field should exist");
    target.copy_from_slice(&value.to_le_bytes());
}

fn replace_u64(bytes: &mut [u8], offset: usize, value: u64) {
    let target = bytes
        .get_mut(offset..offset + size_of::<u64>())
        .expect("test u64 field should exist");
    target.copy_from_slice(&value.to_le_bytes());
}

fn decode_hex(text: &str) -> Vec<u8> {
    let text = text.trim();
    let mut chunks = text.as_bytes().chunks_exact(2);
    let mut bytes = Vec::new();
    bytes.reserve_exact(chunks.len());
    for chunk in &mut chunks {
        let pair = std::str::from_utf8(chunk).expect("fixture hex should be UTF-8");
        bytes.push(u8::from_str_radix(pair, 16).expect("fixture hex should decode"));
    }
    assert!(chunks.remainder().is_empty(), "fixture hex must be even");
    bytes
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut text = String::new();
    text.reserve_exact(bytes.len() * 2);
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("writing to String should succeed");
    }
    text
}
