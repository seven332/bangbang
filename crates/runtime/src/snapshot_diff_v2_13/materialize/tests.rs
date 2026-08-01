use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange, aarch64};
use crate::snapshot_diff_v2_13::{
    NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION, SnapshotV2DiffSelection,
    write_snapshot_v2_diff_layer,
};
use crate::snapshot_memory_v2::write_snapshot_v2_memory_image_with_compatibility_version;

use super::*;

static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct TestFile {
    path: PathBuf,
}

impl TestFile {
    fn create_empty(label: &str) -> (Self, File) {
        let sequence = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-diff-materialize-{label}-{}-{sequence}.snap",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test file should create");
        (Self { path }, file)
    }

    fn open_read(&self) -> File {
        File::open(&self.path).expect("test file should open read-only")
    }

    fn open_read_write(&self) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .expect("test file should open read-write")
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).expect("test file should read")
    }

    fn length(&self) -> u64 {
        fs::metadata(&self.path)
            .expect("test file metadata should read")
            .len()
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct LayerFixture {
    file: TestFile,
    binding: SnapshotV2DiffLayerBinding,
}

struct CompleteFixture {
    file: TestFile,
    binding: SnapshotV2MemoryBinding,
}

fn page_range(first_page: u64, page_count: u64) -> GuestMemoryRange {
    GuestMemoryRange::new(
        GuestAddress::new(aarch64::DRAM_MEM_START + first_page * aarch64::GUEST_PAGE_SIZE),
        page_count * aarch64::GUEST_PAGE_SIZE,
    )
    .expect("test range should validate")
}

fn memory_with_bytes(ranges: &[(GuestMemoryRange, u8)]) -> GuestMemory {
    let layout = GuestMemoryLayout::new(ranges.iter().map(|(range, _)| *range).collect())
        .expect("test layout should validate");
    let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
    for (range, byte) in ranges.iter().copied() {
        let length = usize::try_from(range.size()).expect("test range should fit usize");
        memory
            .write_slice(&vec![byte; length], range.start())
            .expect("test memory should populate");
    }
    memory
}

fn write_layer(
    memory: &GuestMemory,
    base: SnapshotV2DiffBase,
    selected: &[GuestMemoryRange],
    label: &str,
) -> LayerFixture {
    let selection = SnapshotV2DiffSelection::try_from_ranges(memory, selected)
        .expect("test selection should validate");
    let (file, mut writer) = TestFile::create_empty(label);
    let binding = write_snapshot_v2_diff_layer(memory, &mut writer, base, &selection)
        .expect("test layer should write");
    drop(writer);
    LayerFixture { file, binding }
}

fn write_complete(memory: &GuestMemory, label: &str) -> CompleteFixture {
    let (file, mut writer) = TestFile::create_empty(label);
    let binding = write_snapshot_v2_memory_image_with_compatibility_version(
        memory,
        &mut writer,
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
    )
    .expect("test complete image should write");
    drop(writer);
    CompleteFixture { file, binding }
}

fn read_exact_file(file: &File, mut bytes: &mut [u8], mut offset: u64) {
    while !bytes.is_empty() {
        let count = file
            .read_at(bytes, offset)
            .expect("test positional read should succeed");
        assert_ne!(count, 0, "test positional read should make progress");
        offset += u64::try_from(count).expect("test read length should fit");
        bytes = bytes
            .get_mut(count..)
            .expect("test read count should fit destination");
    }
}

fn assert_complete_matches(expected: &GuestMemory, binding: &SnapshotV2MemoryBinding, file: &File) {
    assert_eq!(
        file.metadata().expect("result metadata should read").len(),
        binding.file_length()
    );
    let mut previous_end = NATIVE_V2_MEMORY_ALIGNMENT;
    for extent in binding.extents().iter().copied() {
        let gap_length = usize::try_from(
            extent
                .file_offset()
                .checked_sub(previous_end)
                .expect("canonical gap should not underflow"),
        )
        .expect("test gap should fit usize");
        let mut gap = vec![0_u8; gap_length];
        read_exact_file(file, &mut gap, previous_end);
        assert!(gap.iter().all(|byte| *byte == 0));

        let length = usize::try_from(extent.range().size()).expect("extent should fit usize");
        let mut actual = vec![0_u8; length];
        let mut wanted = vec![0_u8; length];
        read_exact_file(file, &mut actual, extent.file_offset());
        expected
            .read_slice(&mut wanted, extent.range().start())
            .expect("expected memory should read");
        assert_eq!(actual, wanted);
        previous_end = extent.file_offset() + extent.range().size();
    }
}

fn zero_memory_like(ranges: &[GuestMemoryRange]) -> GuestMemory {
    memory_with_bytes(
        &ranges
            .iter()
            .copied()
            .map(|range| (range, 0))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn promotes_empty_partial_and_full_zero_root_layers() {
    let whole = page_range(0, 4);
    let first = page_range(0, 1);
    let source = memory_with_bytes(&[(whole, 0x3c)]);

    for (label, selected) in [
        ("empty-root", Vec::new()),
        ("partial-root", vec![first]),
        ("full-root", vec![whole]),
    ] {
        let root = write_layer(&source, SnapshotV2DiffBase::Zero, &selected, label);
        let mut expected = zero_memory_like(&[whole]);
        for range in selected.iter().copied() {
            let length = usize::try_from(range.size()).expect("selected range should fit");
            let mut bytes = vec![0_u8; length];
            source
                .read_slice(&mut bytes, range.start())
                .expect("selected bytes should read");
            expected
                .write_slice(&bytes, range.start())
                .expect("selected bytes should populate expected memory");
        }

        let source_before = root.file.bytes();
        let (result_file, mut staging) = TestFile::create_empty("promoted");
        let result = promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut staging)
            .expect("zero-root layer should promote");
        assert_eq!(result, *root.binding.result());
        assert_eq!(source_before, root.file.bytes());
        assert_complete_matches(&expected, &result, &staging);
        assert_eq!(
            staging
                .stream_position()
                .expect("result position should query"),
            result.file_length()
        );
        drop(result_file);
    }
}

#[test]
fn zero_root_and_complete_bases_produce_the_same_next_result() {
    let first = page_range(0, 4);
    let second = page_range(8, 4);
    let root_memory = memory_with_bytes(&[(first, 0x11), (second, 0x22)]);
    let root = write_layer(
        &root_memory,
        SnapshotV2DiffBase::Zero,
        &[first],
        "chain-root",
    );

    let next_memory = memory_with_bytes(&[(first, 0x11), (second, 0x77)]);
    let next = write_layer(
        &next_memory,
        SnapshotV2DiffBase::Image(root.binding.result().clone()),
        &[second],
        "chain-next",
    );
    let expected = memory_with_bytes(&[(first, 0x11), (second, 0x77)]);

    let (from_root_file, mut from_root) = TestFile::create_empty("from-root");
    let root_result = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::ZeroRoot(root.file.open_read()),
        next.file.open_read(),
        &mut from_root,
    )
    .expect("next layer should apply to a proven root");
    assert_eq!(root_result, *next.binding.result());
    assert_complete_matches(&expected, &root_result, &from_root);

    let (promoted_file, mut promoted) = TestFile::create_empty("root-complete");
    let promoted_binding =
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut promoted)
            .expect("root should promote for complete-base comparison");
    assert_eq!(promoted_binding, *root.binding.result());
    drop(promoted);

    let (from_complete_file, mut from_complete) = TestFile::create_empty("from-complete");
    let complete_result = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::Complete(promoted_file.open_read()),
        next.file.open_read(),
        &mut from_complete,
    )
    .expect("next layer should apply to a complete base");
    assert_eq!(complete_result, root_result);
    assert_eq!(from_complete_file.bytes(), from_root_file.bytes());
    assert_complete_matches(&expected, &complete_result, &from_complete);
}

#[test]
fn dynamic_removal_inherits_by_gpa_after_the_target_offset_moves() {
    let removed = page_range(0, 4);
    let retained = page_range(32, 4);
    let base_memory = memory_with_bytes(&[(removed, 0x19), (retained, 0xa7)]);
    let base = write_complete(&base_memory, "remove-base");

    let target_memory = memory_with_bytes(&[(retained, 0xff)]);
    let next = write_layer(
        &target_memory,
        SnapshotV2DiffBase::Image(base.binding.clone()),
        &[],
        "remove-next",
    );
    assert_ne!(
        base.binding
            .extents()
            .iter()
            .find(|extent| extent.range() == retained)
            .expect("retained base extent should exist")
            .file_offset(),
        next.binding.result().extents()[0].file_offset()
    );

    let expected = memory_with_bytes(&[(retained, 0xa7)]);
    let (_result_file, mut staging) = TestFile::create_empty("remove-result");
    let result = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::Complete(base.file.open_read()),
        next.file.open_read(),
        &mut staging,
    )
    .expect("removed topology should inherit the retained GPA");
    assert_complete_matches(&expected, &result, &staging);
}

#[test]
fn simultaneous_add_remove_preserves_distinct_adjacent_result_regions() {
    let removed = page_range(0, 4);
    let retained = page_range(32, 4);
    let added_adjacent = page_range(36, 4);
    let base_memory = memory_with_bytes(&[(removed, 0x10), (retained, 0x20)]);
    let base = write_complete(&base_memory, "replace-base");
    let target_memory = memory_with_bytes(&[(retained, 0x20), (added_adjacent, 0x30)]);
    let next = write_layer(
        &target_memory,
        SnapshotV2DiffBase::Image(base.binding.clone()),
        &[added_adjacent],
        "replace-next",
    );
    assert_eq!(next.binding.result().extents().len(), 2);

    let (_output, mut staging) = TestFile::create_empty("replace-output");
    let result = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::Complete(base.file.open_read()),
        next.file.open_read(),
        &mut staging,
    )
    .expect("simultaneous add/remove should materialize by GPA");
    assert_complete_matches(&target_memory, &result, &staging);
}

#[test]
fn dynamic_addition_requires_complete_explicit_coverage_for_both_base_kinds() {
    let existing = page_range(0, 4);
    let added = page_range(64, 4);
    let base_memory = memory_with_bytes(&[(existing, 0x2a)]);
    let base = write_complete(&base_memory, "add-base");
    let target_memory = memory_with_bytes(&[(existing, 0x2a), (added, 0x6d)]);

    for selected in [Vec::new(), vec![page_range(64, 1)]] {
        let next = write_layer(
            &target_memory,
            SnapshotV2DiffBase::Image(base.binding.clone()),
            &selected,
            "add-incomplete",
        );
        let (staging_file, mut staging) = TestFile::create_empty("add-rejected");
        let error = apply_snapshot_v2_diff_layer_file(
            SnapshotV2DiffMaterializationBaseFile::Complete(base.file.open_read()),
            next.file.open_read(),
            &mut staging,
        )
        .expect_err("an omitted added page must reject");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::MissingCoverage {
                stage: SnapshotV2DiffMaterializationStage::LineagePlanning
            }
        ));
        assert_eq!(staging_file.length(), 0);
    }

    let next = write_layer(
        &target_memory,
        SnapshotV2DiffBase::Image(base.binding.clone()),
        &[added],
        "add-complete",
    );
    let (_staging_file, mut staging) = TestFile::create_empty("add-result");
    let result = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::Complete(base.file.open_read()),
        next.file.open_read(),
        &mut staging,
    )
    .expect("fully explicit added topology should materialize");
    assert_complete_matches(&target_memory, &result, &staging);

    let root = write_layer(
        &base_memory,
        SnapshotV2DiffBase::Zero,
        &[existing],
        "add-root",
    );
    for selected in [Vec::new(), vec![page_range(64, 1)]] {
        let next = write_layer(
            &target_memory,
            SnapshotV2DiffBase::Image(root.binding.result().clone()),
            &selected,
            "add-root-incomplete",
        );
        let (staging_file, mut staging) = TestFile::create_empty("add-root-rejected");
        let error = apply_snapshot_v2_diff_layer_file(
            SnapshotV2DiffMaterializationBaseFile::ZeroRoot(root.file.open_read()),
            next.file.open_read(),
            &mut staging,
        )
        .expect_err("a root base cannot prove zero outside its predecessor topology");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::MissingCoverage { .. }
        ));
        assert_eq!(staging_file.length(), 0);
    }

    let next = write_layer(
        &target_memory,
        SnapshotV2DiffBase::Image(root.binding.result().clone()),
        &[added],
        "add-root-complete",
    );
    let (_staging_file, mut staging) = TestFile::create_empty("add-root-result");
    let result = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::ZeroRoot(root.file.open_read()),
        next.file.open_read(),
        &mut staging,
    )
    .expect("a fully explicit addition should apply to a zero-root base");
    assert_complete_matches(&target_memory, &result, &staging);
}

#[test]
fn repeated_complete_application_handles_add_then_remove() {
    let original = page_range(0, 4);
    let added = page_range(48, 4);
    let root_memory = memory_with_bytes(&[(original, 0x31)]);
    let root = write_layer(
        &root_memory,
        SnapshotV2DiffBase::Zero,
        &[original],
        "repeat-root",
    );
    let (first_file, mut first) = TestFile::create_empty("repeat-first");
    let first_binding = promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut first)
        .expect("first result should promote");
    drop(first);

    let added_memory = memory_with_bytes(&[(original, 0x31), (added, 0x42)]);
    let add_layer = write_layer(
        &added_memory,
        SnapshotV2DiffBase::Image(first_binding),
        &[added],
        "repeat-add",
    );
    let (second_file, mut second) = TestFile::create_empty("repeat-second");
    let second_binding = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::Complete(first_file.open_read()),
        add_layer.file.open_read(),
        &mut second,
    )
    .expect("added result should apply");
    drop(second);

    let removed_memory = memory_with_bytes(&[(added, 0xff)]);
    let remove_layer = write_layer(
        &removed_memory,
        SnapshotV2DiffBase::Image(second_binding),
        &[],
        "repeat-remove",
    );
    let (_third_file, mut third) = TestFile::create_empty("repeat-third");
    let third_binding = apply_snapshot_v2_diff_layer_file(
        SnapshotV2DiffMaterializationBaseFile::Complete(second_file.open_read()),
        remove_layer.file.open_read(),
        &mut third,
    )
    .expect("removed result should apply");
    let expected = memory_with_bytes(&[(added, 0x42)]);
    assert_complete_matches(&expected, &third_binding, &third);
}

#[test]
fn malformed_layer_files_and_lineage_fail_closed() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x55)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "malformed-root",
    );

    let trailing = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "trailing-root");
    trailing
        .file
        .open_read_write()
        .set_len(trailing.binding.file_length() + 1)
        .expect("trailing byte should append");
    let (_trailing_output, mut staging) = TestFile::create_empty("trailing-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(trailing.file.open_read(), &mut staging),
        Err(SnapshotV2DiffMaterializationError::LayerFileLengthMismatch { .. })
    ));

    let padded = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "padded-root");
    padded
        .file
        .open_read_write()
        .write_at(&[1], padded.binding.metadata_length())
        .expect("padding byte should mutate");
    let (_padding_output, mut staging) = TestFile::create_empty("padding-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(padded.file.open_read(), &mut staging),
        Err(SnapshotV2DiffMaterializationError::NonZeroLayerPadding { .. })
    ));

    let truncated = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "truncated-root",
    );
    truncated
        .file
        .open_read_write()
        .set_len(
            u64::try_from(NATIVE_V2_DIFF_HEADER_BYTES - 1).expect("test header length should fit"),
        )
        .expect("layer header should truncate");
    let (_truncated_output, mut staging) = TestFile::create_empty("truncated-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(truncated.file.open_read(), &mut staging),
        Err(SnapshotV2DiffMaterializationError::LayerFileLengthMismatch { .. })
    ));

    let corrupt = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "corrupt-root");
    let corrupt_writer = corrupt.file.open_read_write();
    let mut checksum_byte = [0_u8; 1];
    read_exact_file(&corrupt_writer, &mut checksum_byte, 64);
    checksum_byte[0] ^= 1;
    corrupt_writer
        .write_at(&checksum_byte, 64)
        .expect("layer checksum should corrupt");
    let (_corrupt_output, mut staging) = TestFile::create_empty("corrupt-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(corrupt.file.open_read(), &mut staging),
        Err(SnapshotV2DiffMaterializationError::Layer {
            source: SnapshotV2DiffLayerBindingError::IntegrityMismatch,
            ..
        })
    ));

    let complete = write_complete(&memory, "lineage-complete");
    let next = write_layer(
        &memory,
        SnapshotV2DiffBase::Image(complete.binding.clone()),
        &[],
        "image-layer",
    );
    let (_root_output, mut staging) = TestFile::create_empty("image-as-root");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(next.file.open_read(), &mut staging),
        Err(SnapshotV2DiffMaterializationError::InvalidZeroRoot { .. })
    ));

    let (_next_output, mut staging) = TestFile::create_empty("zero-as-next");
    assert!(matches!(
        apply_snapshot_v2_diff_layer_file(
            SnapshotV2DiffMaterializationBaseFile::Complete(complete.file.open_read()),
            root.file.open_read(),
            &mut staging,
        ),
        Err(SnapshotV2DiffMaterializationError::InvalidNextLayer { .. })
    ));

    let other_root = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "other-root");
    let (_mismatch_output, mut staging) = TestFile::create_empty("mismatch-output");
    assert!(matches!(
        apply_snapshot_v2_diff_layer_file(
            SnapshotV2DiffMaterializationBaseFile::ZeroRoot(other_root.file.open_read()),
            next.file.open_read(),
            &mut staging,
        ),
        Err(SnapshotV2DiffMaterializationError::PredecessorMismatch { .. })
    ));
}

fn clear_cloexec(file: &File) {
    // SAFETY: `file` owns a live descriptor and `F_SETFD` receives no pointer.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
    assert_eq!(result, 0, "test should clear close-on-exec");
}

#[test]
fn descriptor_policy_rejects_mutable_special_aliasing_and_invalid_output_files() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x61)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "descriptor-root",
    );

    let (_writable_output, mut staging) = TestFile::create_empty("writable-source-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read_write(), &mut staging),
        Err(SnapshotV2DiffMaterializationError::Source {
            source: SnapshotV2MemoryLoadError::DescriptorNotReadOnly,
            ..
        })
    ));

    let source = root.file.open_read();
    clear_cloexec(&source);
    let (_cloexec_output, mut staging) = TestFile::create_empty("cloexec-source-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(source, &mut staging),
        Err(SnapshotV2DiffMaterializationError::Source {
            source: SnapshotV2MemoryLoadError::DescriptorNotCloseOnExec,
            ..
        })
    ));

    let (_special_output, mut staging) = TestFile::create_empty("special-output");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(
            File::open("/dev/null").expect("null device should open"),
            &mut staging,
        ),
        Err(SnapshotV2DiffMaterializationError::Source {
            source: SnapshotV2MemoryLoadError::NotRegularFile,
            ..
        })
    ));

    let mut alias = root.file.open_read_write();
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut alias),
        Err(SnapshotV2DiffMaterializationError::SourceOutputAlias { .. })
    ));

    let (nonempty_file, mut nonempty) = TestFile::create_empty("nonempty-output");
    nonempty
        .write_all(&[1])
        .expect("nonempty output should write");
    nonempty
        .seek(SeekFrom::Start(0))
        .expect("nonempty output should rewind");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut nonempty),
        Err(SnapshotV2DiffMaterializationError::InvalidOutput { .. })
    ));
    assert_eq!(nonempty_file.bytes(), vec![1]);

    let (_position_file, mut positioned) = TestFile::create_empty("position-output");
    positioned
        .seek(SeekFrom::Start(1))
        .expect("empty output position should move");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut positioned),
        Err(SnapshotV2DiffMaterializationError::InvalidOutput { .. })
    ));

    let (_cloexec_file, mut no_cloexec) = TestFile::create_empty("no-cloexec-output");
    clear_cloexec(&no_cloexec);
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut no_cloexec),
        Err(SnapshotV2DiffMaterializationError::InvalidOutput { .. })
    ));

    let (write_only_file, read_write) = TestFile::create_empty("write-only-output");
    drop(read_write);
    let mut write_only = OpenOptions::new()
        .write(true)
        .open(&write_only_file.path)
        .expect("write-only staging should open");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut write_only),
        Err(SnapshotV2DiffMaterializationError::InvalidOutput { .. })
    ));

    let (append_file, read_write) = TestFile::create_empty("append-output");
    drop(read_write);
    let mut append = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&append_file.path)
        .expect("append staging should open");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut append),
        Err(SnapshotV2DiffMaterializationError::InvalidOutput { .. })
    ));

    let mut special_output = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .expect("null device should open read-write");
    assert!(matches!(
        promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut special_output),
        Err(SnapshotV2DiffMaterializationError::InvalidOutput { .. })
    ));

    let complete = write_complete(&memory, "mutable-complete-base");
    let next = write_layer(
        &memory,
        SnapshotV2DiffBase::Image(complete.binding.clone()),
        &[],
        "mutable-complete-next",
    );
    let (_complete_output, mut staging) = TestFile::create_empty("mutable-complete-output");
    assert!(matches!(
        apply_snapshot_v2_diff_layer_file(
            SnapshotV2DiffMaterializationBaseFile::Complete(complete.file.open_read_write()),
            next.file.open_read(),
            &mut staging,
        ),
        Err(SnapshotV2DiffMaterializationError::Source {
            source: SnapshotV2MemoryLoadError::DescriptorNotReadOnly,
            ..
        })
    ));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AllocationFailure {
    LayerMetadata,
    Routes,
    CopyBuffer,
}

struct MetadataReserveObservation {
    called: bool,
}

impl MaterializationPolicy for MetadataReserveObservation {
    fn checkpoint(
        &mut self,
        _stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        Ok(())
    }

    fn reserve_layer_metadata(
        &mut self,
        metadata: &mut Vec<u8>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        self.called = true;
        metadata.try_reserve_exact(count)
    }
}

#[test]
fn hostile_fixed_prefix_rejects_oversized_metadata_before_allocation() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x18)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "oversized-prefix-root",
    );
    root.file
        .open_read_write()
        .write_at(&u32::MAX.to_le_bytes(), 32)
        .expect("result-binding length should mutate");
    let (output, mut staging) = TestFile::create_empty("oversized-prefix-output");
    let mut policy = MetadataReserveObservation { called: false };
    let error = promote_with_policy(root.file.open_read(), &mut staging, &mut policy)
        .expect_err("oversized fixed prefix should reject");
    assert!(matches!(
        error,
        SnapshotV2DiffMaterializationError::Layer {
            source: SnapshotV2DiffLayerBindingError::MetadataTooLarge,
            ..
        }
    ));
    assert!(!policy.called);
    assert_eq!(output.length(), 0);
}

struct AllocationFailurePolicy {
    target: AllocationFailure,
}

impl MaterializationPolicy for AllocationFailurePolicy {
    fn checkpoint(
        &mut self,
        _stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        Ok(())
    }

    fn reserve_layer_metadata(
        &mut self,
        metadata: &mut Vec<u8>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        if self.target == AllocationFailure::LayerMetadata {
            metadata.try_reserve_exact(usize::MAX)
        } else {
            metadata.try_reserve_exact(count)
        }
    }

    fn reserve_routes(
        &mut self,
        routes: &mut Vec<Route>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        if self.target == AllocationFailure::Routes {
            routes.try_reserve_exact(usize::MAX)
        } else {
            routes.try_reserve_exact(count)
        }
    }

    fn reserve_copy_buffer(
        &mut self,
        buffer: &mut Vec<u8>,
        count: usize,
    ) -> Result<(), TryReserveError> {
        if self.target == AllocationFailure::CopyBuffer {
            buffer.try_reserve_exact(usize::MAX)
        } else {
            buffer.try_reserve_exact(count)
        }
    }
}

#[test]
fn bounded_allocation_failures_happen_before_the_first_output_byte() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x8c)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "allocation-root",
    );
    for target in [
        AllocationFailure::LayerMetadata,
        AllocationFailure::Routes,
        AllocationFailure::CopyBuffer,
    ] {
        let (output, mut staging) = TestFile::create_empty("allocation-output");
        let error = promote_with_policy(
            root.file.open_read(),
            &mut staging,
            &mut AllocationFailurePolicy { target },
        )
        .expect_err("injected allocation should fail");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::MetadataAllocation { .. }
        ));
        assert_eq!(output.length(), 0);
    }
}

struct ShortIoPolicy {
    interrupted_read: bool,
    interrupted_write: bool,
}

impl MaterializationPolicy for ShortIoPolicy {
    fn checkpoint(
        &mut self,
        _stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        Ok(())
    }

    fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        if !self.interrupted_read {
            self.interrupted_read = true;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = buffer.len().min(17);
        file.read_at(
            buffer
                .get_mut(..length)
                .expect("short read slice should fit"),
            offset,
        )
    }

    fn write_at(&mut self, file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
        if !self.interrupted_write {
            self.interrupted_write = true;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = buffer.len().min(19);
        file.write_at(
            buffer.get(..length).expect("short write slice should fit"),
            offset,
        )
    }
}

#[test]
fn interrupted_and_short_positional_io_retries_to_the_exact_result() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0xd2)]);
    let root = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "short-io-root");
    let (_output, mut staging) = TestFile::create_empty("short-io-output");
    let mut policy = ShortIoPolicy {
        interrupted_read: false,
        interrupted_write: false,
    };
    let result = promote_with_policy(root.file.open_read(), &mut staging, &mut policy)
        .expect("short interrupted I/O should retry");
    assert!(policy.interrupted_read);
    assert!(policy.interrupted_write);
    assert_complete_matches(&memory, &result, &staging);
}

struct ZeroProgressPolicy {
    streaming: bool,
    fail_read: bool,
}

impl MaterializationPolicy for ZeroProgressPolicy {
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        if stage == SnapshotV2DiffMaterializationStage::DataStreaming {
            self.streaming = true;
        }
        Ok(())
    }

    fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        if self.streaming && self.fail_read {
            Ok(0)
        } else {
            file.read_at(buffer, offset)
        }
    }

    fn write_at(&mut self, file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
        if self.streaming && !self.fail_read {
            Ok(0)
        } else {
            file.write_at(buffer, offset)
        }
    }
}

#[test]
fn zero_progress_reads_and_writes_fail_with_stable_redacted_io_classes() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0xe4)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "zero-progress-root",
    );
    for (fail_read, expected) in [
        (true, io::ErrorKind::UnexpectedEof),
        (false, io::ErrorKind::WriteZero),
    ] {
        let (_output, mut staging) = TestFile::create_empty("zero-progress-output");
        let error = promote_with_policy(
            root.file.open_read(),
            &mut staging,
            &mut ZeroProgressPolicy {
                streaming: false,
                fail_read,
            },
        )
        .expect_err("zero-progress I/O should fail");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::Io {
                stage: SnapshotV2DiffMaterializationStage::DataStreaming,
                kind,
            } if kind == expected
        ));
    }
}

struct FailingStreamingIoPolicy {
    streaming: bool,
    fail_read: bool,
    operation_count: usize,
}

impl MaterializationPolicy for FailingStreamingIoPolicy {
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        if stage == SnapshotV2DiffMaterializationStage::DataStreaming {
            self.streaming = true;
        }
        Ok(())
    }

    fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        if self.streaming && self.fail_read {
            self.operation_count += 1;
            if self.operation_count == 2 {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let length = buffer.len().min(23);
            return file.read_at(
                buffer
                    .get_mut(..length)
                    .expect("partial failing read should fit"),
                offset,
            );
        }
        file.read_at(buffer, offset)
    }

    fn write_at(&mut self, file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
        if self.streaming && !self.fail_read {
            self.operation_count += 1;
            if self.operation_count == 2 {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let length = buffer.len().min(29);
            return file.write_at(
                buffer
                    .get(..length)
                    .expect("partial failing write should fit"),
                offset,
            );
        }
        file.write_at(buffer, offset)
    }
}

#[test]
fn positional_errors_after_partial_streaming_leave_only_caller_staging_changed() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x6a)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "failing-io-root",
    );
    let source_before = root.file.bytes();
    for fail_read in [true, false] {
        let (output, mut staging) = TestFile::create_empty("failing-io-output");
        let error = promote_with_policy(
            root.file.open_read(),
            &mut staging,
            &mut FailingStreamingIoPolicy {
                streaming: false,
                fail_read,
                operation_count: 0,
            },
        )
        .expect_err("injected positional failure should reject");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::Io {
                stage: SnapshotV2DiffMaterializationStage::DataStreaming,
                kind: io::ErrorKind::PermissionDenied,
            }
        ));
        assert_eq!(output.length(), root.binding.result().file_length());
        assert_eq!(source_before, root.file.bytes());
    }
}

struct TruncatingReadPolicy {
    streaming: bool,
    truncated: bool,
    writer: File,
}

impl MaterializationPolicy for TruncatingReadPolicy {
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        if stage == SnapshotV2DiffMaterializationStage::DataStreaming {
            self.streaming = true;
        }
        Ok(())
    }

    fn read_at(&mut self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        if self.streaming && !self.truncated {
            self.writer.set_len(offset)?;
            self.truncated = true;
        }
        file.read_at(buffer, offset)
    }
}

#[test]
fn truncation_during_copy_fails_at_the_bounded_read() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x44)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "truncate-copy-root",
    );
    let (_output, mut staging) = TestFile::create_empty("truncate-copy-output");
    let error = promote_with_policy(
        root.file.open_read(),
        &mut staging,
        &mut TruncatingReadPolicy {
            streaming: false,
            truncated: false,
            writer: root.file.open_read_write(),
        },
    )
    .expect_err("source truncation should stop copy");
    assert!(matches!(
        error,
        SnapshotV2DiffMaterializationError::Io {
            stage: SnapshotV2DiffMaterializationStage::DataStreaming,
            kind: io::ErrorKind::UnexpectedEof,
        }
    ));
}

struct MutationPolicy {
    role: SourceRole,
    target: SourceObservation,
    writer: File,
    changed: bool,
}

impl MaterializationPolicy for MutationPolicy {
    fn checkpoint(
        &mut self,
        _stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        Ok(())
    }

    fn source_hook(&mut self, role: SourceRole, observation: SourceObservation, _file: &File) {
        if role == self.role && observation == self.target && !self.changed {
            self.writer
                .write_at(&[0x5a], NATIVE_V2_MEMORY_ALIGNMENT)
                .expect("test source mutation should write");
            self.changed = true;
        }
    }
}

#[test]
fn source_fact_changes_at_every_observation_fail_without_committing_authority() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x93)]);
    for target in [
        SourceObservation::DuringValidation,
        SourceObservation::BeforeOutput,
        SourceObservation::AfterStreaming,
        SourceObservation::Final,
    ] {
        let root = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "mutation-root");
        let (output, mut staging) = TestFile::create_empty("mutation-output");
        let error = promote_with_policy(
            root.file.open_read(),
            &mut staging,
            &mut MutationPolicy {
                role: SourceRole::ZeroRoot,
                target,
                writer: root.file.open_read_write(),
                changed: false,
            },
        )
        .expect_err("source mutation should fail closed");
        let expected_stage = if target == SourceObservation::DuringValidation {
            SnapshotV2DiffMaterializationStage::SourceValidation
        } else {
            SnapshotV2DiffMaterializationStage::SourceStability
        };
        assert_eq!(error.stage(), expected_stage);
        if matches!(
            target,
            SourceObservation::DuringValidation | SourceObservation::BeforeOutput
        ) {
            assert_eq!(output.length(), 0);
        }
    }
}

#[test]
fn complete_base_changes_after_inherited_copy_fail_the_source_recheck() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x81)]);
    let complete = write_complete(&memory, "mutation-complete");
    let next = write_layer(
        &memory,
        SnapshotV2DiffBase::Image(complete.binding.clone()),
        &[],
        "mutation-complete-next",
    );
    let (output, mut staging) = TestFile::create_empty("mutation-complete-output");
    let error = apply_with_policy(
        SnapshotV2DiffMaterializationBaseFile::Complete(complete.file.open_read()),
        next.file.open_read(),
        &mut staging,
        &mut MutationPolicy {
            role: SourceRole::CompleteBase,
            target: SourceObservation::AfterStreaming,
            writer: complete.file.open_read_write(),
            changed: false,
        },
    )
    .expect_err("a changed complete predecessor should fail closed");
    assert!(matches!(
        error,
        SnapshotV2DiffMaterializationError::Source {
            stage: SnapshotV2DiffMaterializationStage::SourceStability,
            source: SnapshotV2MemoryLoadError::SourceChanged,
        }
    ));
    assert_eq!(output.length(), next.binding.result().file_length());
}

struct VerificationFailurePolicy;

impl MaterializationPolicy for VerificationFailurePolicy {
    fn checkpoint(
        &mut self,
        _stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        Ok(())
    }

    fn result_verification_hook(&mut self, file: &mut File) {
        file.write_at(&[0], 0)
            .expect("test result header should corrupt");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputFailure {
    HeaderWrite,
    SetLength,
    FinalSeek,
}

struct OutputFailurePolicy {
    target: OutputFailure,
    stage: SnapshotV2DiffMaterializationStage,
}

impl MaterializationPolicy for OutputFailurePolicy {
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffMaterializationStage,
    ) -> Result<(), SnapshotV2DiffMaterializationError> {
        self.stage = stage;
        Ok(())
    }

    fn write_at(&mut self, file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
        if self.target == OutputFailure::HeaderWrite
            && self.stage == SnapshotV2DiffMaterializationStage::OutputHeader
        {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else {
            file.write_at(buffer, offset)
        }
    }

    fn set_len(&mut self, file: &File, length: u64) -> io::Result<()> {
        if self.target == OutputFailure::SetLength {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else {
            file.set_len(length)
        }
    }

    fn seek_output(&mut self, file: &mut File, position: u64) -> io::Result<u64> {
        if self.target == OutputFailure::FinalSeek {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else {
            file.seek(SeekFrom::Start(position))
        }
    }
}

#[test]
fn output_operation_failures_are_staged_and_leave_sources_immutable() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x72)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "output-failure-root",
    );
    let source_before = root.file.bytes();
    for (target, expected_stage) in [
        (
            OutputFailure::HeaderWrite,
            SnapshotV2DiffMaterializationStage::OutputHeader,
        ),
        (
            OutputFailure::SetLength,
            SnapshotV2DiffMaterializationStage::OutputPadding,
        ),
        (
            OutputFailure::FinalSeek,
            SnapshotV2DiffMaterializationStage::ResultVerification,
        ),
    ] {
        let (_output, mut staging) = TestFile::create_empty("output-failure-result");
        let error = promote_with_policy(
            root.file.open_read(),
            &mut staging,
            &mut OutputFailurePolicy {
                target,
                stage: SnapshotV2DiffMaterializationStage::SourceValidation,
            },
        )
        .expect_err("injected output operation should fail");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::Io {
                stage,
                kind: io::ErrorKind::PermissionDenied,
            } if stage == expected_stage
        ));
        assert_eq!(source_before, root.file.bytes());
    }
}

#[test]
fn detached_result_verification_failure_is_staged_after_streaming() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0xab)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "verification-root",
    );
    let source_before = root.file.bytes();
    let (output, mut staging) = TestFile::create_empty("verification-output");
    let error = promote_with_policy(
        root.file.open_read(),
        &mut staging,
        &mut VerificationFailurePolicy,
    )
    .expect_err("corrupt result should fail verification");
    assert!(matches!(
        error,
        SnapshotV2DiffMaterializationError::ResultVerification {
            stage: SnapshotV2DiffMaterializationStage::ResultVerification,
            ..
        }
    ));
    assert_eq!(output.length(), root.binding.result().file_length());
    assert_eq!(source_before, root.file.bytes());
}

#[test]
fn cancellation_at_every_observed_checkpoint_preserves_sources_and_allows_retry() {
    let range = page_range(0, 512);
    let memory = memory_with_bytes(&[(range, 0xc3)]);
    let root = write_layer(&memory, SnapshotV2DiffBase::Zero, &[range], "cancel-root");
    let source_before = root.file.bytes();
    let observed = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&observed);
    let (_success_file, mut success) = TestFile::create_empty("cancel-observe");
    promote_snapshot_v2_diff_zero_root_file_with_cancel(
        root.file.open_read(),
        &mut success,
        move |stage| {
            captured.borrow_mut().push(stage);
            false
        },
    )
    .expect("checkpoint observation run should succeed");
    let checkpoints = observed.borrow().clone();
    assert!(checkpoints.len() > 8);
    assert!(
        checkpoints
            .iter()
            .filter(|stage| **stage == SnapshotV2DiffMaterializationStage::DataStreaming)
            .count()
            > 1
    );

    for (cancellation_index, expected_stage) in checkpoints.iter().copied().enumerate() {
        let (_output, mut staging) = TestFile::create_empty("cancel-output");
        let mut index = 0_usize;
        let error = promote_snapshot_v2_diff_zero_root_file_with_cancel(
            root.file.open_read(),
            &mut staging,
            |_| {
                let cancelled = index == cancellation_index;
                index += 1;
                cancelled
            },
        )
        .expect_err("selected checkpoint should cancel");
        assert!(matches!(
            error,
            SnapshotV2DiffMaterializationError::Cancelled { stage }
                if stage == expected_stage
        ));
        assert_eq!(source_before, root.file.bytes());
    }

    let (_retry_file, mut retry) = TestFile::create_empty("cancel-retry");
    let result = promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut retry)
        .expect("fresh retry should succeed");
    assert_complete_matches(&memory, &result, &retry);
}

#[test]
fn source_positions_and_error_diagnostics_do_not_disclose_private_values() {
    let range = page_range(0, 4);
    let memory = memory_with_bytes(&[(range, 0x7e)]);
    let root = write_layer(
        &memory,
        SnapshotV2DiffBase::Zero,
        &[range],
        "redacted-private-marker",
    );
    let mut source = root.file.open_read();
    source
        .seek(SeekFrom::Start(13))
        .expect("source position should set");
    let mut position_probe = source
        .try_clone()
        .expect("source position probe should clone");
    let (_output, mut staging) = TestFile::create_empty("position-result");
    promote_snapshot_v2_diff_zero_root_file(source, &mut staging)
        .expect("nonzero source position should remain irrelevant");
    assert_eq!(
        position_probe
            .stream_position()
            .expect("source position should query"),
        13
    );

    let (_bad_output, mut bad) = TestFile::create_empty("redaction-output");
    bad.write_all(&[1])
        .expect("bad output should become nonempty");
    bad.seek(SeekFrom::Start(0))
        .expect("bad output should rewind");
    let error = promote_snapshot_v2_diff_zero_root_file(root.file.open_read(), &mut bad)
        .expect_err("bad output should reject");
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert!(debug.contains("<redacted>"));
    for private in [
        root.file.path.to_string_lossy().as_ref(),
        "redacted-private-marker",
        "2147483648",
        "65536",
        "0x7e",
    ] {
        assert!(!debug.contains(private));
        assert!(!display.contains(private));
    }
}

#[test]
fn checked_route_overflow_is_rejected_without_panicking() {
    let stage = SnapshotV2DiffMaterializationStage::LineagePlanning;
    assert!(matches!(
        checked_length(u64::MAX, 0, stage),
        Err(SnapshotV2DiffMaterializationError::InvalidRoute { .. })
    ));

    let mut routes = vec![Route {
        source: RouteSource::CompleteBase,
        source_offset: 0,
        target_offset: 0,
        length: 1,
    }];
    assert!(matches!(
        append_route(&mut routes, 1, RouteSource::NextLayer, 0, 2, 1, stage),
        Err(SnapshotV2DiffMaterializationError::InvalidRoute { .. })
    ));
}
