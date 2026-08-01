use std::collections::TryReserveError;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use crate::memory::{GuestMemoryLayout, aarch64};
use crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_VERSION;

use super::*;

const RESULT_ID_BYTES: [u8; IMAGE_ID_BYTES] = *b"result-image-id!";
const BASE_ID: SnapshotV2MemoryImageId = SnapshotV2MemoryImageId::from_bytes(*b"base-image-id!!!");
const OTHER_BASE_ID: SnapshotV2MemoryImageId =
    SnapshotV2MemoryImageId::from_bytes(*b"other-base-id!!!");
const ROOT_ZERO_FIXTURE: &str = include_str!("../fixtures/root-zero.hex");
const PREDECESSOR_DYNAMIC_FIXTURE: &str = include_str!("../fixtures/predecessor-dynamic.hex");

fn range(start: u64, size: u64) -> GuestMemoryRange {
    GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should validate")
}

fn allocate_memory(ranges: &[GuestMemoryRange]) -> GuestMemory {
    let layout = GuestMemoryLayout::new(ranges.to_vec()).expect("test layout should validate");
    GuestMemory::allocate(&layout).expect("test guest memory should allocate")
}

fn patterned_memory(ranges: &[GuestMemoryRange]) -> GuestMemory {
    let mut memory = allocate_memory(ranges);
    for (region_index, range) in ranges.iter().copied().enumerate() {
        let length = usize::try_from(range.size()).expect("test range should fit usize");
        let mut bytes = vec![0_u8; length];
        for (byte_index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from((region_index * 67 + byte_index) % 251)
                .expect("test pattern should fit");
        }
        memory
            .write_slice(&bytes, range.start())
            .expect("test memory should write");
    }
    memory
}

fn write_with_identity<W, C>(
    memory: &GuestMemory,
    writer: &mut W,
    base: SnapshotV2DiffBase,
    selection: &SnapshotV2DiffSelection,
    is_cancelled: C,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffWriteError>
where
    W: Write + Seek,
    C: FnMut(SnapshotV2DiffWriteStage) -> bool,
{
    write_snapshot_v2_diff_layer_with_policy(
        memory,
        writer,
        base,
        selection,
        is_cancelled,
        |identity| {
            *identity = RESULT_ID_BYTES;
            Ok(())
        },
        |buffer, additional| buffer.try_reserve_exact(additional),
    )
}

fn read_guest(memory: &GuestMemory, range: GuestMemoryRange) -> Vec<u8> {
    let mut bytes = vec![0_u8; usize::try_from(range.size()).expect("test size should fit")];
    memory
        .read_slice(&mut bytes, range.start())
        .expect("test guest range should read");
    bytes
}

fn contains_range(outer: GuestMemoryRange, inner: GuestMemoryRange) -> bool {
    outer.start() <= inner.start() && outer.end_exclusive() >= inner.end_exclusive()
}

#[test]
fn explicit_selection_normalizes_sorts_deduplicates_and_coalesces() {
    let start = aarch64::DRAM_MEM_START;
    let memory = allocate_memory(&[range(start, 64 * 1024)]);
    let input = [
        range(start + 12 * 1024 + 17, 64),
        range(start + 1, 31),
        range(start + 8192, 4096),
        range(start + 4090, 5000),
        range(start + 12 * 1024, 4096),
    ];
    let selection = SnapshotV2DiffSelection::try_from_ranges(&memory, &input)
        .expect("mapped ranges should normalize");
    assert_eq!(
        selection.ranges(),
        &[range(start, 16 * 1024)],
        "overlapping and adjacent guest pages should form one canonical range"
    );
}

#[test]
fn selection_splits_adjacent_result_regions_and_rejects_gaps() {
    let start = aarch64::DRAM_MEM_START;
    let adjacent = allocate_memory(&[range(start, 16 * 1024), range(start + 16 * 1024, 16 * 1024)]);
    let crossing = [range(start + 12 * 1024, 8192)];
    let selection = SnapshotV2DiffSelection::try_from_ranges(&adjacent, &crossing)
        .expect("adjacent regions should be fully covered");
    assert_eq!(
        selection.ranges(),
        &[
            range(start + 12 * 1024, 4096),
            range(start + 16 * 1024, 4096),
        ]
    );

    let gapped = allocate_memory(&[range(start, 16 * 1024), range(start + 32 * 1024, 16 * 1024)]);
    assert!(matches!(
        SnapshotV2DiffSelection::try_from_ranges(&gapped, &crossing),
        Err(SnapshotV2DiffSelectionError::OutOfTopology)
    ));
}

#[test]
fn dirty_host_pages_expand_to_guest_pages_and_reject_invalid_inputs() {
    let start = aarch64::DRAM_MEM_START;
    let memory = allocate_memory(&[range(start, 64 * 1024)]);
    let selection = SnapshotV2DiffSelection::try_from_dirty_pages(
        &memory,
        16 * 1024,
        &[GuestAddress::new(start + 16 * 1024)],
    )
    .expect("one aligned host page should normalize");
    assert_eq!(selection.ranges(), &[range(start + 16 * 1024, 16 * 1024)]);

    assert!(matches!(
        SnapshotV2DiffSelection::try_from_dirty_pages(&memory, 6000, &[GuestAddress::new(start)]),
        Err(SnapshotV2DiffSelectionError::InvalidSourcePageSize)
    ));
    assert!(matches!(
        SnapshotV2DiffSelection::try_from_dirty_pages(
            &memory,
            16 * 1024,
            &[GuestAddress::new(start + 4096)]
        ),
        Err(SnapshotV2DiffSelectionError::UnalignedDirtyPage)
    ));
    assert!(matches!(
        SnapshotV2DiffSelection::try_from_dirty_pages(
            &memory,
            16 * 1024,
            &[GuestAddress::new(start + 64 * 1024)]
        ),
        Err(SnapshotV2DiffSelectionError::OutOfTopology)
    ));
}

#[test]
fn all_current_covers_every_region_and_stale_topology_fails_closed() {
    let start = aarch64::DRAM_MEM_START;
    let initial = [
        range(start, 32 * 1024),
        range(start + 128 * 1024, 16 * 1024),
    ];
    let mut memory = allocate_memory(&initial);
    let selection = SnapshotV2DiffSelection::all_current(&memory)
        .expect("all-current selection should validate");
    assert_eq!(selection.ranges(), initial);

    let added = range(start + 256 * 1024, 16 * 1024);
    memory
        .insert_region(added)
        .expect("test region should insert");
    let mut output = Cursor::new(Vec::new());
    assert!(matches!(
        write_with_identity(
            &memory,
            &mut output,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
        ),
        Err(SnapshotV2DiffWriteError::TopologyChanged)
    ));
    assert!(output.into_inner().is_empty());

    let selection_after_add =
        SnapshotV2DiffSelection::all_current(&memory).expect("expanded selection should validate");
    memory
        .remove_region(added)
        .expect("test region should remove");
    let mut output = Cursor::new(Vec::new());
    assert!(matches!(
        write_with_identity(
            &memory,
            &mut output,
            SnapshotV2DiffBase::Zero,
            &selection_after_add,
            |_| false,
        ),
        Err(SnapshotV2DiffWriteError::TopologyChanged)
    ));
    assert!(output.into_inner().is_empty());
}

#[test]
fn current_dynamic_add_and_remove_topologies_write_canonically() {
    let start = aarch64::DRAM_MEM_START;
    let original = range(start, 32 * 1024);
    let added = range(start + 128 * 1024, 16 * 1024);
    let mut memory = patterned_memory(&[original]);
    memory
        .insert_region(added)
        .expect("test region should insert");
    memory
        .write_slice(&vec![0x5a; 16 * 1024], added.start())
        .expect("inserted region should write");

    let added_selection =
        SnapshotV2DiffSelection::all_current(&memory).expect("added topology should select");
    let mut added_output = Cursor::new(Vec::new());
    let added_binding = write_with_identity(
        &memory,
        &mut added_output,
        SnapshotV2DiffBase::Zero,
        &added_selection,
        |_| false,
    )
    .expect("added topology should write");
    assert_eq!(
        added_binding
            .result()
            .extents()
            .iter()
            .map(|extent| extent.range())
            .collect::<Vec<_>>(),
        [original, added]
    );
    assert_eq!(
        added_binding
            .data_extents()
            .iter()
            .map(|extent| extent.range())
            .collect::<Vec<_>>(),
        [original, added]
    );
    verify_snapshot_v2_diff_layer_output(&added_binding, &mut added_output)
        .expect("added topology output should verify");

    memory
        .remove_region(original)
        .expect("original region should remove");
    let removed_selection =
        SnapshotV2DiffSelection::all_current(&memory).expect("removed topology should select");
    let mut removed_output = Cursor::new(Vec::new());
    let removed_binding = write_with_identity(
        &memory,
        &mut removed_output,
        SnapshotV2DiffBase::Image(BASE_ID),
        &removed_selection,
        |_| false,
    )
    .expect("removed topology should write");
    assert_eq!(
        removed_binding
            .result()
            .extents()
            .iter()
            .map(|extent| extent.range())
            .collect::<Vec<_>>(),
        [added]
    );
    assert_eq!(removed_binding.data_extents()[0].range(), added);
    verify_snapshot_v2_diff_layer_output(&removed_binding, &mut removed_output)
        .expect("removed topology output should verify");
}

#[test]
fn fragmented_selection_falls_back_to_complete_touched_regions_without_omission() {
    let start = aarch64::DRAM_MEM_START;
    let selected_count = NATIVE_V2_DIFF_MAX_EXTENTS + 1;
    let required_pages = selected_count
        .checked_mul(2)
        .and_then(|pages| pages.checked_sub(1))
        .expect("test page count should fit");
    let host_aligned_pages = required_pages.next_multiple_of(4);
    let memory = allocate_memory(&[range(
        start,
        u64::try_from(host_aligned_pages).expect("test page count should fit")
            * NATIVE_V2_MEMORY_GUEST_GRANULE,
    )]);
    let dirty_pages = (0..selected_count)
        .map(|index| {
            GuestAddress::new(
                start
                    + u64::try_from(index).expect("test index should fit")
                        * 2
                        * NATIVE_V2_MEMORY_GUEST_GRANULE,
            )
        })
        .collect::<Vec<_>>();
    let selection = SnapshotV2DiffSelection::try_from_dirty_pages(
        &memory,
        NATIVE_V2_MEMORY_GUEST_GRANULE,
        &dirty_pages,
    )
    .expect("fragmented selection should widen safely");
    assert_eq!(selection.ranges().len(), 1);
    assert_eq!(selection.ranges()[0], memory.regions()[0].range());
    for address in dirty_pages {
        let page = range(address.raw_value(), NATIVE_V2_MEMORY_GUEST_GRANULE);
        assert!(
            selection
                .ranges()
                .iter()
                .copied()
                .any(|selected| contains_range(selected, page)),
            "fallback omitted an input page"
        );
    }
}

#[test]
fn root_empty_writer_matches_immutable_fixture_and_verifies() {
    let start = aarch64::DRAM_MEM_START;
    let memory = patterned_memory(&[range(start, 64 * 1024)]);
    let selection = SnapshotV2DiffSelection::try_from_ranges(&memory, &[])
        .expect("empty selection should validate");
    let mut output = Cursor::new(Vec::new());
    let binding = write_with_identity(
        &memory,
        &mut output,
        SnapshotV2DiffBase::Zero,
        &selection,
        |_| false,
    )
    .expect("empty root layer should write");
    assert!(binding.data_extents().is_empty());
    assert_eq!(
        binding.result().version(),
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION
    );
    assert_eq!(NATIVE_V2_SNAPSHOT_VERSION.minor(), 12);
    let bytes = output.into_inner();
    let metadata_length =
        usize::try_from(binding.metadata_length()).expect("metadata length should fit");
    assert_eq!(&bytes[..metadata_length], decode_hex(ROOT_ZERO_FIXTURE));
    assert!(bytes[metadata_length..].iter().all(|byte| *byte == 0));
    verify_snapshot_v2_diff_layer_output(&binding, &mut Cursor::new(bytes))
        .expect("detached root output should verify");
}

#[test]
fn predecessor_writer_matches_fixture_and_packs_only_selected_bytes() {
    let start = aarch64::DRAM_MEM_START;
    let topology = [
        range(start, 16 * 1024),
        range(start + 128 * 1024, 32 * 1024),
    ];
    let memory = patterned_memory(&topology);
    let selected = [
        range(start + 4096, 4096),
        range(start + 128 * 1024 + 8192, 8192),
    ];
    let selection = SnapshotV2DiffSelection::try_from_ranges(&memory, &selected)
        .expect("selected ranges should validate");
    let mut output = Cursor::new(Vec::new());
    let binding = write_with_identity(
        &memory,
        &mut output,
        SnapshotV2DiffBase::Image(BASE_ID),
        &selection,
        |_| false,
    )
    .expect("predecessor layer should write");
    let bytes = output.into_inner();
    let metadata_length =
        usize::try_from(binding.metadata_length()).expect("metadata length should fit");
    assert_eq!(
        &bytes[..metadata_length],
        decode_hex(PREDECESSOR_DYNAMIC_FIXTURE)
    );
    let data_offset = usize::try_from(binding.data_offset()).expect("data offset should fit");
    assert!(
        bytes[metadata_length..data_offset]
            .iter()
            .all(|byte| *byte == 0)
    );
    let first_end = data_offset + usize::try_from(selected[0].size()).expect("size should fit");
    assert_eq!(
        &bytes[data_offset..first_end],
        read_guest(&memory, selected[0])
    );
    assert_eq!(&bytes[first_end..], read_guest(&memory, selected[1]));
    assert_eq!(
        u64::try_from(bytes.len()).expect("file length should fit"),
        binding.file_length()
    );
    verify_snapshot_v2_diff_layer_output(&binding, &mut Cursor::new(bytes))
        .expect("detached predecessor output should verify");
}

#[test]
fn writer_rejects_nonempty_nonzero_identity_and_copy_allocation_failures_before_bytes() {
    let start = aarch64::DRAM_MEM_START;
    let memory = patterned_memory(&[range(start, 16 * 1024)]);
    let selection =
        SnapshotV2DiffSelection::all_current(&memory).expect("complete selection should validate");

    let mut nonempty = Cursor::new(vec![1_u8]);
    assert!(matches!(
        write_with_identity(
            &memory,
            &mut nonempty,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
        ),
        Err(SnapshotV2DiffWriteError::NonEmptyOutput)
    ));

    let mut nonzero = Cursor::new(Vec::new());
    nonzero.set_position(1);
    assert!(matches!(
        write_with_identity(
            &memory,
            &mut nonzero,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
        ),
        Err(SnapshotV2DiffWriteError::InvalidInitialPosition)
    ));

    let mut unavailable = Cursor::new(Vec::new());
    assert!(matches!(
        write_snapshot_v2_diff_layer_with_policy(
            &memory,
            &mut unavailable,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
            |_| Err(()),
            |buffer, additional| buffer.try_reserve_exact(additional),
        ),
        Err(SnapshotV2DiffWriteError::IdentityUnavailable)
    ));
    assert!(unavailable.into_inner().is_empty());

    let mut allocation = Cursor::new(Vec::new());
    assert!(matches!(
        write_snapshot_v2_diff_layer_with_policy(
            &memory,
            &mut allocation,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
            |identity| {
                *identity = RESULT_ID_BYTES;
                Ok(())
            },
            |_, _| Err(allocation_error()),
        ),
        Err(SnapshotV2DiffWriteError::CopyBufferAllocationFailed { .. })
    ));
    assert!(allocation.into_inner().is_empty());
}

#[test]
fn cancellation_covers_every_observed_stage_and_fresh_retry() {
    let start = aarch64::DRAM_MEM_START;
    let memory = patterned_memory(&[
        range(start, 32 * 1024),
        range(start + 128 * 1024, 16 * 1024),
    ]);
    let selection =
        SnapshotV2DiffSelection::all_current(&memory).expect("complete selection should validate");
    let mut complete = Cursor::new(Vec::new());
    let mut stages = Vec::new();
    let complete_binding = write_with_identity(
        &memory,
        &mut complete,
        SnapshotV2DiffBase::Zero,
        &selection,
        |stage| {
            stages.push(stage);
            false
        },
    )
    .expect("observation write should complete");
    assert!(stages.contains(&SnapshotV2DiffWriteStage::InitialPosition));
    assert!(stages.contains(&SnapshotV2DiffWriteStage::Metadata));
    assert!(stages.contains(&SnapshotV2DiffWriteStage::MetadataPadding));
    assert!(stages.contains(&SnapshotV2DiffWriteStage::Data { extent_index: 0 }));
    assert!(stages.contains(&SnapshotV2DiffWriteStage::Data { extent_index: 1 }));
    assert!(stages.contains(&SnapshotV2DiffWriteStage::FinalLength));

    for (cancel_index, expected_stage) in stages.iter().copied().enumerate() {
        let mut output = Cursor::new(Vec::new());
        let mut checkpoint = 0_usize;
        let error = write_with_identity(
            &memory,
            &mut output,
            SnapshotV2DiffBase::Zero,
            &selection,
            |stage| {
                assert_eq!(stage, stages[checkpoint]);
                let cancelled = checkpoint == cancel_index;
                checkpoint += 1;
                cancelled
            },
        )
        .expect_err("selected checkpoint should cancel");
        assert!(matches!(
            error,
            SnapshotV2DiffWriteError::Cancelled { stage } if stage == expected_stage
        ));
        assert_eq!(checkpoint, cancel_index + 1);
    }

    let mut fresh = Cursor::new(Vec::new());
    let fresh_binding = write_with_identity(
        &memory,
        &mut fresh,
        SnapshotV2DiffBase::Zero,
        &selection,
        |_| false,
    )
    .expect("fresh write should complete");
    assert_eq!(fresh_binding, complete_binding);
    assert_eq!(fresh.into_inner(), complete.into_inner());
}

struct ShortWriter {
    inner: Cursor<Vec<u8>>,
    maximum: usize,
    interruptions_remaining: usize,
}

impl Write for ShortWriter {
    fn write(&mut self, source: &[u8]) -> io::Result<usize> {
        if self.interruptions_remaining != 0 {
            self.interruptions_remaining -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = source.len().min(self.maximum);
        let source = source
            .get(..length)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.inner.write(source)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for ShortWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

struct ZeroOrFailWriter {
    inner: Cursor<Vec<u8>>,
    failure: Option<io::Error>,
}

impl Write for ZeroOrFailWriter {
    fn write(&mut self, _source: &[u8]) -> io::Result<usize> {
        match self.failure.take() {
            Some(error) => Err(error),
            None => Ok(0),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for ZeroOrFailWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

struct MisreportingDataSeekWriter {
    inner: Cursor<Vec<u8>>,
}

impl Write for MisreportingDataSeekWriter {
    fn write(&mut self, source: &[u8]) -> io::Result<usize> {
        self.inner.write(source)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for MisreportingDataSeekWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let actual = self.inner.seek(position)?;
        if matches!(position, SeekFrom::Start(requested) if requested != 0) {
            Ok(actual.saturating_add(1))
        } else {
            Ok(actual)
        }
    }
}

#[test]
fn writer_handles_short_io_and_redacts_failures_and_seek_mismatches() {
    let start = aarch64::DRAM_MEM_START;
    let memory = patterned_memory(&[range(start, 32 * 1024)]);
    let selection =
        SnapshotV2DiffSelection::all_current(&memory).expect("complete selection should validate");
    let mut expected = Cursor::new(Vec::new());
    let expected_binding = write_with_identity(
        &memory,
        &mut expected,
        SnapshotV2DiffBase::Zero,
        &selection,
        |_| false,
    )
    .expect("reference write should complete");

    let mut short = ShortWriter {
        inner: Cursor::new(Vec::new()),
        maximum: 13,
        interruptions_remaining: 3,
    };
    let binding = write_with_identity(
        &memory,
        &mut short,
        SnapshotV2DiffBase::Zero,
        &selection,
        |_| false,
    )
    .expect("short writes should complete");
    assert_eq!(binding, expected_binding);
    assert_eq!(short.inner.into_inner(), expected.into_inner());

    let mut zero = ZeroOrFailWriter {
        inner: Cursor::new(Vec::new()),
        failure: None,
    };
    assert!(matches!(
        write_with_identity(
            &memory,
            &mut zero,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
        ),
        Err(SnapshotV2DiffWriteError::Io {
            stage: SnapshotV2DiffWriteStage::Metadata,
            kind: io::ErrorKind::WriteZero,
        })
    ));

    let mut private = ZeroOrFailWriter {
        inner: Cursor::new(Vec::new()),
        failure: Some(io::Error::other("private-output-name")),
    };
    let error = write_with_identity(
        &memory,
        &mut private,
        SnapshotV2DiffBase::Zero,
        &selection,
        |_| false,
    )
    .expect_err("private I/O failure should propagate");
    assert!(!format!("{error:?} {error}").contains("private-output-name"));

    let mut mismatched = MisreportingDataSeekWriter {
        inner: Cursor::new(Vec::new()),
    };
    assert!(matches!(
        write_with_identity(
            &memory,
            &mut mismatched,
            SnapshotV2DiffBase::Zero,
            &selection,
            |_| false,
        ),
        Err(SnapshotV2DiffWriteError::PositionMismatch {
            stage: SnapshotV2DiffWriteStage::Data { extent_index: 0 },
        })
    ));
}

#[test]
fn detached_verifier_rejects_mismatch_corruption_padding_and_length_changes() {
    let start = aarch64::DRAM_MEM_START;
    let memory = patterned_memory(&[range(start, 32 * 1024)]);
    let selection = SnapshotV2DiffSelection::try_from_ranges(&memory, &[range(start + 4096, 4096)])
        .expect("sparse selection should validate");
    let mut output = Cursor::new(Vec::new());
    let binding = write_with_identity(
        &memory,
        &mut output,
        SnapshotV2DiffBase::Image(BASE_ID),
        &selection,
        |_| false,
    )
    .expect("fixture should write");
    let bytes = output.into_inner();

    let mismatched = SnapshotV2DiffLayerBinding::try_from_ranges(
        SnapshotV2DiffBase::Image(OTHER_BASE_ID),
        binding.result().clone(),
        selection.ranges(),
    )
    .expect("mismatched detached binding should construct");
    assert!(matches!(
        verify_snapshot_v2_diff_layer_output(&mismatched, &mut Cursor::new(bytes.clone())),
        Err(SnapshotV2DiffVerifyError::BindingMismatch)
    ));

    let mut corrupt = bytes.clone();
    let checksum_byte = corrupt.get_mut(64).expect("checksum byte should exist");
    *checksum_byte ^= 1;
    assert!(matches!(
        verify_snapshot_v2_diff_layer_output(&binding, &mut Cursor::new(corrupt)),
        Err(SnapshotV2DiffVerifyError::Binding { .. })
    ));

    let mut padding = bytes.clone();
    let padding_index =
        usize::try_from(binding.metadata_length()).expect("metadata length should fit");
    *padding
        .get_mut(padding_index)
        .expect("padding byte should exist") = 1;
    assert!(matches!(
        verify_snapshot_v2_diff_layer_output(&binding, &mut Cursor::new(padding)),
        Err(SnapshotV2DiffVerifyError::NonZeroPadding)
    ));

    let mut truncated = bytes.clone();
    truncated.pop();
    assert!(matches!(
        verify_snapshot_v2_diff_layer_output(&binding, &mut Cursor::new(truncated)),
        Err(SnapshotV2DiffVerifyError::FileLengthMismatch)
    ));
    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        verify_snapshot_v2_diff_layer_output(&binding, &mut Cursor::new(trailing)),
        Err(SnapshotV2DiffVerifyError::FileLengthMismatch)
    ));
}

struct ShortReader {
    inner: Cursor<Vec<u8>>,
    maximum: usize,
    interruptions_remaining: usize,
}

impl Read for ShortReader {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        if self.interruptions_remaining != 0 {
            self.interruptions_remaining -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = destination.len().min(self.maximum);
        let destination = destination
            .get_mut(..length)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.inner.read(destination)
    }
}

impl Seek for ShortReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

struct MisreportingMetadataSeekReader {
    inner: Cursor<Vec<u8>>,
}

impl Read for MisreportingMetadataSeekReader {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        self.inner.read(destination)
    }
}

impl Seek for MisreportingMetadataSeekReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let actual = self.inner.seek(position)?;
        if matches!(position, SeekFrom::Start(0)) {
            Ok(actual.saturating_add(1))
        } else {
            Ok(actual)
        }
    }
}

#[test]
fn detached_verifier_handles_short_reads_seek_mismatch_and_allocation_failure() {
    let start = aarch64::DRAM_MEM_START;
    let memory = patterned_memory(&[range(start, 16 * 1024)]);
    let selection =
        SnapshotV2DiffSelection::all_current(&memory).expect("complete selection should validate");
    let mut output = Cursor::new(Vec::new());
    let binding = write_with_identity(
        &memory,
        &mut output,
        SnapshotV2DiffBase::Zero,
        &selection,
        |_| false,
    )
    .expect("fixture should write");
    let bytes = output.into_inner();

    let mut short = ShortReader {
        inner: Cursor::new(bytes.clone()),
        maximum: 11,
        interruptions_remaining: 3,
    };
    verify_snapshot_v2_diff_layer_output(&binding, &mut short)
        .expect("short and interrupted reads should complete");

    let mut mismatched = MisreportingMetadataSeekReader {
        inner: Cursor::new(bytes.clone()),
    };
    assert!(matches!(
        verify_snapshot_v2_diff_layer_output(&binding, &mut mismatched),
        Err(SnapshotV2DiffVerifyError::PositionMismatch {
            stage: SnapshotV2DiffVerifyStage::Metadata,
        })
    ));

    assert!(matches!(
        verify_snapshot_v2_diff_layer_output_with_reserve(
            &binding,
            &mut Cursor::new(bytes),
            |_, _| Err(allocation_error()),
        ),
        Err(SnapshotV2DiffVerifyError::MetadataAllocationFailed { .. })
    ));
}

#[test]
fn selection_and_error_diagnostics_redact_private_values() {
    let start = aarch64::DRAM_MEM_START;
    let memory = allocate_memory(&[range(start, 16 * 1024)]);
    let selection = SnapshotV2DiffSelection::try_from_ranges(&memory, &[range(start + 4096, 4096)])
        .expect("selection should validate");
    let diagnostics = format!(
        "{selection:?} {:?} {:?}",
        SnapshotV2DiffSelectionError::OutOfTopology,
        SnapshotV2DiffWriteError::GuestMemoryRead {
            stage: SnapshotV2DiffWriteStage::Data { extent_index: 0 }
        }
    );
    assert!(diagnostics.contains(REDACTED));
    assert!(!diagnostics.contains("40001000"));
    assert!(!diagnostics.contains("result-image-id!"));
}

fn allocation_error() -> TryReserveError {
    let mut impossible = Vec::<u8>::new();
    impossible
        .try_reserve(usize::MAX)
        .expect_err("impossible reservation should fail")
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
    assert!(chunks.remainder().is_empty());
    bytes
}
