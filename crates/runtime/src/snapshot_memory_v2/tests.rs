use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::{GuestAddress, GuestMemoryLayout, GuestMemoryRegionBacking};
use crate::snapshot_format_v2::{
    NATIVE_V2_SNAPSHOT_FOUNDATION_VERSION, SnapshotV2ComponentKey, decode_snapshot_v2_state,
};

use super::*;

const TEST_ID: SnapshotV2MemoryImageId = SnapshotV2MemoryImageId::from_bytes(*b"0123456789abcdef");
const OTHER_ID: SnapshotV2MemoryImageId = SnapshotV2MemoryImageId::from_bytes(*b"fedcba9876543210");
const TWO_EXTENT_BINDING_FIXTURE: [u8; 112] = [
    66, 65, 78, 71, 77, 50, 65, 0, 2, 0, 1, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 1, 0, 2,
    0, 0, 0, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102, 59, 242, 10, 252,
    48, 129, 96, 169, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0,
    0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 128, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0,
    0,
];
const TWO_EXTENT_STATE_FIXTURE: [u8; 216] = [
    66, 65, 78, 71, 86, 50, 65, 0, 2, 0, 1, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
    0, 0, 0, 216, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 96, 0, 0,
    0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 96, 0, 0, 0, 0, 0, 0, 0, 112, 0,
    0, 0, 0, 0, 0, 0, 66, 65, 78, 71, 77, 50, 65, 0, 2, 0, 1, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 16, 0,
    0, 0, 0, 1, 0, 2, 0, 0, 0, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102,
    59, 242, 10, 252, 48, 129, 96, 169, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 128,
    0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 128, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
    0, 2, 0, 0, 0, 0, 0, 94, 239, 232, 126, 123, 199, 153, 135,
];

fn range(start: u64, size: u64) -> GuestMemoryRange {
    GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should be valid")
}

fn test_memory() -> GuestMemory {
    let ranges = vec![
        range(aarch64::DRAM_MEM_START, 32 * 1024),
        range(aarch64::DRAM_MEM_START + 128 * 1024, 64 * 1024),
    ];
    let layout = GuestMemoryLayout::new(ranges.clone()).expect("test layout should be valid");
    let mut memory = GuestMemory::allocate(&layout).expect("test guest memory should allocate");
    for (region_index, range) in ranges.into_iter().enumerate() {
        let length = usize::try_from(range.size()).expect("test range should fit usize");
        let mut bytes = vec![0_u8; length];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from((region_index * 61 + index) % 251).expect("test byte should fit");
        }
        memory
            .write_slice(&bytes, range.start())
            .expect("test bytes should write");
    }
    memory
}

fn write_test_image(
    memory: &GuestMemory,
    image_id: SnapshotV2MemoryImageId,
) -> (Vec<u8>, SnapshotV2MemoryBinding) {
    let mut output = Cursor::new(Vec::new());
    let binding =
        write_snapshot_v2_memory_image_with_id_and_cancel(memory, &mut output, image_id, |_| false)
            .expect("test image should encode");
    (output.into_inner(), binding)
}

#[test]
fn canonical_binding_state_and_image_round_trip() {
    let memory = test_memory();
    let (image, binding) = write_test_image(&memory, TEST_ID);
    let encoded = binding.encode().expect("binding should encode");
    assert_eq!(encoded, TWO_EXTENT_BINDING_FIXTURE);

    assert_eq!(
        encoded.len(),
        NATIVE_V2_MEMORY_HEADER_BYTES + 2 * NATIVE_V2_MEMORY_EXTENT_BYTES
    );
    assert_eq!(
        binding.extents()[0].file_offset(),
        NATIVE_V2_MEMORY_ALIGNMENT
    );
    assert_eq!(
        binding.extents()[1].file_offset(),
        2 * NATIVE_V2_MEMORY_ALIGNMENT
    );
    assert_eq!(binding.file_length(), 3 * NATIVE_V2_MEMORY_ALIGNMENT);
    assert_eq!(
        image.len(),
        usize::try_from(binding.file_length()).expect("test length should fit usize")
    );
    assert_eq!(
        image.get(..NATIVE_V2_MEMORY_HEADER_BYTES),
        encoded.get(..NATIVE_V2_MEMORY_HEADER_BYTES)
    );
    assert!(
        image[NATIVE_V2_MEMORY_HEADER_BYTES
            ..usize::try_from(NATIVE_V2_MEMORY_ALIGNMENT).expect("alignment should fit")]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert!(
        image[96 * 1024..128 * 1024].iter().all(|byte| *byte == 0),
        "the canonical inter-extent alignment gap must remain sparse/zero"
    );

    let state_bytes =
        encode_snapshot_v2_state_with_memory(&binding).expect("typed state should encode");
    assert_eq!(state_bytes, TWO_EXTENT_STATE_FIXTURE);
    let state = decode_snapshot_v2_state(&state_bytes).expect("typed state should decode");
    assert_eq!(state.metadata().version(), NATIVE_V2_SNAPSHOT_VERSION);
    assert_eq!(
        decode_snapshot_v2_memory_binding(&state).expect("memory component should decode"),
        binding
    );
}

#[test]
fn foundation_and_invalid_typed_component_profiles_fail_closed() {
    let memory = test_memory();
    let (_, binding) = write_test_image(&memory, TEST_ID);
    let payload = binding.encode().expect("binding should encode");

    let mut foundation = encode_snapshot_v2_state(&[], &[]).expect("empty state should encode");
    foundation[10..12].copy_from_slice(&0_u16.to_le_bytes());
    let checksum_offset =
        foundation.len() - crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let checksum = crc64(0, &foundation[..checksum_offset]);
    foundation[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
    let state = decode_snapshot_v2_state(&foundation).expect("foundation should decode");
    assert_eq!(
        state.metadata().version(),
        NATIVE_V2_SNAPSHOT_FOUNDATION_VERSION
    );
    assert!(matches!(
        decode_snapshot_v2_memory_binding(&state),
        Err(SnapshotV2MemoryStateError::MissingMemoryComponent)
    ));

    let wrong_instance = [SnapshotV2Component::new(
        SnapshotV2ComponentKey::new(NATIVE_V2_MEMORY_COMPONENT_KEY.kind(), 1),
        SnapshotV2ComponentDisposition::Semantic,
        &payload,
    )];
    let bytes = encode_snapshot_v2_state(&[], &wrong_instance)
        .expect("structural writer should admit a catalogued kind");
    let state = decode_snapshot_v2_state(&bytes).expect("structural state should decode");
    assert!(matches!(
        decode_snapshot_v2_memory_binding(&state),
        Err(SnapshotV2MemoryStateError::InvalidMemoryComponentProfile)
    ));

    let wrong_disposition = [SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::NonSemantic,
        &payload,
    )];
    let bytes = encode_snapshot_v2_state(&[], &wrong_disposition)
        .expect("structural writer should admit nonsemantic extensions");
    let state = decode_snapshot_v2_state(&bytes).expect("structural state should decode");
    assert!(matches!(
        decode_snapshot_v2_memory_binding(&state),
        Err(SnapshotV2MemoryStateError::InvalidMemoryComponentProfile)
    ));
}

#[test]
fn binding_mutations_reject_header_integrity_topology_and_trailing_bytes() {
    let memory = test_memory();
    let (_, binding) = write_test_image(&memory, TEST_ID);
    let encoded = binding.encode().expect("binding should encode");

    let mut invalid_magic = encoded.clone();
    invalid_magic[MAGIC_OFFSET] ^= 0x80;
    assert!(matches!(
        decode_binding(&invalid_magic),
        Err(SnapshotV2MemoryBindingError::InvalidMagic)
    ));

    let mut invalid_version = encoded.clone();
    replace_u16(&mut invalid_version, VERSION_MINOR_OFFSET, 2);
    replace_binding_checksum(&mut invalid_version);
    assert!(matches!(
        decode_binding(&invalid_version),
        Err(SnapshotV2MemoryBindingError::UnsupportedVersion)
    ));

    for offset in [FLAGS_OFFSET, GUEST_GRANULE_OFFSET, ALIGNMENT_OFFSET] {
        let mut invalid = encoded.clone();
        replace_u32(&mut invalid, offset, 1);
        replace_binding_checksum(&mut invalid);
        assert!(matches!(
            decode_binding(&invalid),
            Err(SnapshotV2MemoryBindingError::InvalidHeader)
        ));
    }

    let mut corrupt = encoded.clone();
    *corrupt.last_mut().expect("binding should not be empty") ^= 0x40;
    assert!(matches!(
        decode_binding(&corrupt),
        Err(SnapshotV2MemoryBindingError::IntegrityMismatch)
    ));

    let mut wrong_offset = encoded.clone();
    replace_u64(
        &mut wrong_offset,
        NATIVE_V2_MEMORY_HEADER_BYTES + EXTENT_FILE_OFFSET,
        NATIVE_V2_MEMORY_ALIGNMENT + NATIVE_V2_MEMORY_GUEST_GRANULE,
    )
    .expect("test field should exist");
    replace_binding_checksum(&mut wrong_offset);
    assert!(matches!(
        decode_binding(&wrong_offset),
        Err(SnapshotV2MemoryBindingError::NonCanonicalFileOffset)
    ));

    let mut overlap = encoded.clone();
    replace_u64(
        &mut overlap,
        NATIVE_V2_MEMORY_HEADER_BYTES + NATIVE_V2_MEMORY_EXTENT_BYTES + EXTENT_GPA_OFFSET,
        aarch64::DRAM_MEM_START + 16 * 1024,
    )
    .expect("test field should exist");
    replace_binding_checksum(&mut overlap);
    assert!(matches!(
        decode_binding(&overlap),
        Err(SnapshotV2MemoryBindingError::InvalidExtentTopology)
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert!(matches!(
        decode_binding(&trailing),
        Err(SnapshotV2MemoryBindingError::InvalidLength)
    ));
}

#[test]
fn writer_rejects_nonempty_and_nonzero_position_outputs() {
    let memory = test_memory();
    let mut nonempty = Cursor::new(vec![0_u8]);
    assert!(matches!(
        write_snapshot_v2_memory_image(&memory, &mut nonempty),
        Err(SnapshotV2MemoryWriteError::NonEmptyOutput)
    ));

    let mut nonzero = Cursor::new(Vec::new());
    nonzero.set_position(1);
    assert!(matches!(
        write_snapshot_v2_memory_image(&memory, &mut nonzero),
        Err(SnapshotV2MemoryWriteError::InvalidInitialPosition)
    ));
}

#[test]
fn writer_cancellation_covers_every_bounded_checkpoint_and_fresh_retry() {
    let memory = test_memory();
    let mut complete = Cursor::new(Vec::new());
    let mut expected_stages = Vec::new();
    let complete_binding = write_snapshot_v2_memory_image_with_id_and_cancel(
        &memory,
        &mut complete,
        TEST_ID,
        |stage| {
            expected_stages.push(stage);
            false
        },
    )
    .expect("complete observation write should succeed");
    assert_eq!(
        u64::try_from(complete.get_ref().len()).expect("test image length should fit"),
        complete_binding.file_length()
    );
    assert!(expected_stages.contains(&SnapshotV2MemoryIoStage::InitialPosition));
    assert!(expected_stages.contains(&SnapshotV2MemoryIoStage::Header));
    assert!(expected_stages.contains(&SnapshotV2MemoryIoStage::MetadataPadding));
    assert!(expected_stages.contains(&SnapshotV2MemoryIoStage::Data { extent_index: 0 }));
    assert!(expected_stages.contains(&SnapshotV2MemoryIoStage::Data { extent_index: 1 }));
    assert!(expected_stages.contains(&SnapshotV2MemoryIoStage::FinalLength));

    for (cancel_index, expected_stage) in expected_stages.iter().copied().enumerate() {
        let mut output = Cursor::new(Vec::new());
        let mut checkpoint = 0;
        let error = write_snapshot_v2_memory_image_with_id_and_cancel(
            &memory,
            &mut output,
            TEST_ID,
            |stage| {
                assert_eq!(stage, expected_stages[checkpoint]);
                let cancel = checkpoint == cancel_index;
                checkpoint += 1;
                cancel
            },
        )
        .expect_err("selected checkpoint should cancel");
        assert!(matches!(
            error,
            SnapshotV2MemoryWriteError::Cancelled { stage } if stage == expected_stage
        ));
        assert_eq!(checkpoint, cancel_index + 1);
    }

    let mut fresh = Cursor::new(Vec::new());
    let fresh_binding =
        write_snapshot_v2_memory_image_with_id_and_cancel(&memory, &mut fresh, TEST_ID, |_| false)
            .expect("fresh retry should succeed");
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
        self.inner.write(&source[..length])
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

#[test]
fn writer_accepts_short_and_interrupted_io() {
    let memory = test_memory();
    let (expected, expected_binding) = write_test_image(&memory, TEST_ID);
    let mut writer = ShortWriter {
        inner: Cursor::new(Vec::new()),
        maximum: 13,
        interruptions_remaining: 3,
    };
    let binding =
        write_snapshot_v2_memory_image_with_id_and_cancel(&memory, &mut writer, TEST_ID, |_| false)
            .expect("short and interrupted writes should complete");
    assert_eq!(binding, expected_binding);
    assert_eq!(writer.inner.into_inner(), expected);
}

struct FailingWriter {
    inner: Cursor<Vec<u8>>,
    failure: Option<io::Error>,
}

impl Write for FailingWriter {
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

impl Seek for FailingWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[test]
fn writer_zero_progress_and_io_messages_are_typed_and_redacted() {
    let memory = test_memory();
    let mut zero = FailingWriter {
        inner: Cursor::new(Vec::new()),
        failure: None,
    };
    assert!(matches!(
        write_snapshot_v2_memory_image_with_id_and_cancel(&memory, &mut zero, TEST_ID, |_| false),
        Err(SnapshotV2MemoryWriteError::Io {
            stage: SnapshotV2MemoryIoStage::Header,
            kind: io::ErrorKind::WriteZero,
        })
    ));

    let mut private_message = FailingWriter {
        inner: Cursor::new(Vec::new()),
        failure: Some(io::Error::other("private-output-name")),
    };
    let error = write_snapshot_v2_memory_image_with_id_and_cancel(
        &memory,
        &mut private_message,
        TEST_ID,
        |_| false,
    )
    .expect_err("injected output failure should propagate");
    assert!(matches!(
        error,
        SnapshotV2MemoryWriteError::Io {
            stage: SnapshotV2MemoryIoStage::Header,
            kind: io::ErrorKind::Other,
        }
    ));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("private-output-name"));
    assert!(!diagnostic.contains("0123456789abcdef"));
}

#[test]
fn adopted_file_load_is_lazy_cow_cursor_independent_and_source_preserving() {
    let source_memory = test_memory();
    let (image, binding) = write_test_image(&source_memory, TEST_ID);
    let state_bytes = encode_snapshot_v2_state_with_memory(&binding).expect("state should encode");
    let state = decode_snapshot_v2_state(&state_bytes).expect("state should decode");
    let image_file = TempFile::new("cow", &image);

    let mut first_file = image_file.open_read_only();
    first_file
        .seek(SeekFrom::End(0))
        .expect("test descriptor should seek");
    let mut first =
        load_snapshot_v2_memory_file(&state, first_file).expect("first lazy load should succeed");
    let second = load_snapshot_v2_memory_file(&state, image_file.open_read_only())
        .expect("second lazy load should succeed");

    assert_eq!(first.backing(), GuestMemoryBacking::Anonymous);
    assert!(
        first
            .regions()
            .iter()
            .all(|region| region.backing() == GuestMemoryRegionBacking::PrivateFile)
    );
    assert!(first.dirty_tracker().is_none());

    let address = binding.extents()[0].range().start();
    let mut original = [0_u8; 16];
    first
        .read_slice(&mut original, address)
        .expect("mapped bytes should read");
    let replacement = [0xa5_u8; 16];
    first
        .write_slice(&replacement, address)
        .expect("private mapped bytes should write");

    let mut observed = [0_u8; 16];
    first
        .read_slice(&mut observed, address)
        .expect("first mapping should read");
    assert_eq!(observed, replacement);
    second
        .read_slice(&mut observed, address)
        .expect("second mapping should read");
    assert_eq!(observed, original);
    image_file
        .open_read_only()
        .read_exact_at(&mut observed, binding.extents()[0].file_offset())
        .expect("source bytes should read");
    assert_eq!(observed, original);
}

#[test]
fn loader_rejects_access_flags_length_header_padding_and_binding_substitution() {
    let memory = test_memory();
    let (image, binding) = write_test_image(&memory, TEST_ID);
    let state_bytes = encode_snapshot_v2_state_with_memory(&binding).expect("state should encode");
    let state = decode_snapshot_v2_state(&state_bytes).expect("state should decode");
    let valid = TempFile::new("validation", &image);

    let read_write = valid.open_read_write();
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, read_write),
        Err(SnapshotV2MemoryLoadError::DescriptorNotReadOnly)
    ));

    let no_cloexec = valid.open_read_only();
    // SAFETY: the descriptor is live and the test intentionally clears its
    // close-on-exec flag before transferring ownership to the loader.
    let clear_cloexec = unsafe { libc::fcntl(no_cloexec.as_raw_fd(), libc::F_SETFD, 0) };
    assert_eq!(clear_cloexec, 0);
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, no_cloexec),
        Err(SnapshotV2MemoryLoadError::DescriptorNotCloseOnExec)
    ));

    let mut pipe_descriptors = [-1; 2];
    // SAFETY: the array has room for both descriptors returned by `pipe`.
    assert_eq!(unsafe { libc::pipe(pipe_descriptors.as_mut_ptr()) }, 0);
    let [read_descriptor, write_descriptor] = pipe_descriptors;
    // SAFETY: the read descriptor is live and the test sets only its
    // descriptor-local close-on-exec flag.
    let set_cloexec = unsafe { libc::fcntl(read_descriptor, libc::F_SETFD, libc::FD_CLOEXEC) };
    assert_eq!(set_cloexec, 0);
    // SAFETY: the write descriptor is live and no longer needed by the test.
    assert_eq!(unsafe { libc::close(write_descriptor) }, 0);
    // SAFETY: ownership of the live read descriptor transfers exactly once.
    let pipe_file = unsafe { File::from_raw_fd(read_descriptor) };
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, pipe_file),
        Err(SnapshotV2MemoryLoadError::NotRegularFile)
    ));

    let short = TempFile::new("short", &image[..image.len() - 1]);
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, short.open_read_only()),
        Err(SnapshotV2MemoryLoadError::FileLengthMismatch)
    ));
    let mut extended_bytes = image.clone();
    extended_bytes.push(0);
    let extended = TempFile::new("extended", &extended_bytes);
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, extended.open_read_only()),
        Err(SnapshotV2MemoryLoadError::FileLengthMismatch)
    ));

    let mut invalid_header = image.clone();
    invalid_header[MAGIC_OFFSET] ^= 0x01;
    let invalid_header = TempFile::new("header", &invalid_header);
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, invalid_header.open_read_only()),
        Err(SnapshotV2MemoryLoadError::MemoryHeaderMismatch)
    ));

    let mut invalid_padding = image.clone();
    invalid_padding[NATIVE_V2_MEMORY_HEADER_BYTES] = 1;
    let invalid_padding = TempFile::new("padding", &invalid_padding);
    assert!(matches!(
        load_snapshot_v2_memory_file(&state, invalid_padding.open_read_only()),
        Err(SnapshotV2MemoryLoadError::NonZeroMetadataPadding)
    ));

    let (_, other_binding) = write_test_image(&memory, OTHER_ID);
    let other_state_bytes =
        encode_snapshot_v2_state_with_memory(&other_binding).expect("other state should encode");
    let other_state =
        decode_snapshot_v2_state(&other_state_bytes).expect("other state should decode");
    assert!(matches!(
        load_snapshot_v2_memory_file(&other_state, valid.open_read_only()),
        Err(SnapshotV2MemoryLoadError::MemoryHeaderMismatch)
    ));
}

#[test]
fn loader_detects_descriptor_replacement_and_mutation_at_both_rechecks() {
    let memory = test_memory();
    let (image, binding) = write_test_image(&memory, TEST_ID);
    let replacement = TempFile::new("descriptor-replacement", &image);
    let replacement_file = replacement.open_read_only();
    let original = TempFile::new("descriptor-original", &image);
    let original_file = original.open_read_only();
    let original_fd = original_file.as_raw_fd();
    let error =
        load_snapshot_v2_memory_binding_from_file_with_hook(&binding, original_file, |stage, _| {
            if stage == SnapshotV2MemoryLoadStage::Preflight {
                // SAFETY: both descriptors are live for the call. The test
                // intentionally replaces the adopted descriptor, then restores
                // CLOEXEC so identity—not a weaker flag check—detects it.
                let replaced = unsafe { libc::dup2(replacement_file.as_raw_fd(), original_fd) };
                assert_eq!(replaced, original_fd);
                // SAFETY: the replaced descriptor remains live.
                let restored = unsafe { libc::fcntl(original_fd, libc::F_SETFD, libc::FD_CLOEXEC) };
                assert_eq!(restored, 0);
            }
        })
        .expect_err("descriptor replacement must fail");
    assert!(matches!(error, SnapshotV2MemoryLoadError::SourceChanged));

    for mutation_stage in [
        SnapshotV2MemoryLoadStage::Metadata,
        SnapshotV2MemoryLoadStage::Mapping,
    ] {
        let mutated = TempFile::new("source-mutation", &image);
        let mutator = mutated.open_read_write();
        let error = load_snapshot_v2_memory_binding_from_file_with_hook(
            &binding,
            mutated.open_read_only(),
            |stage, _| {
                if stage == mutation_stage {
                    mutator
                        .set_len(binding.file_length() - 1)
                        .expect("test mutation should truncate the source");
                }
            },
        )
        .expect_err("source mutation must fail");
        assert!(matches!(error, SnapshotV2MemoryLoadError::SourceChanged));
    }
}

#[test]
fn retained_descriptor_survives_path_replacement_without_reopen() {
    let memory = test_memory();
    let (image, binding) = write_test_image(&memory, TEST_ID);
    let original = TempFile::new("path-owner", &image);
    let adopted = open_regular_final(original.path()).expect("direct descriptor should open");
    let moved = TempEntry::new_path("path-owner-moved");
    fs::rename(original.path(), moved.path()).expect("original inode should move");
    fs::write(original.path(), b"untrusted replacement").expect("replacement path should create");

    let loaded = load_snapshot_v2_memory_binding_from_file(&binding, adopted)
        .expect("retained descriptor should ignore later path replacement");
    let mut bytes = [0_u8; 16];
    loaded
        .read_slice(&mut bytes, binding.extents()[0].range().start())
        .expect("retained mapping should read original bytes");
    assert_eq!(
        bytes.as_slice(),
        image
            .get(
                usize::try_from(binding.extents()[0].file_offset()).unwrap()
                    ..usize::try_from(binding.extents()[0].file_offset()).unwrap() + bytes.len()
            )
            .expect("source fixture bytes should exist")
    );
}

#[test]
fn direct_loader_rejects_final_symlink_and_redacts_path() {
    let memory = test_memory();
    let (image, binding) = write_test_image(&memory, TEST_ID);
    let state_bytes = encode_snapshot_v2_state_with_memory(&binding).expect("state should encode");
    let state = decode_snapshot_v2_state(&state_bytes).expect("state should decode");
    let target = TempFile::new("target-private-name", &image);
    let link = TempEntry::new_path("link-private-name");
    symlink(target.path(), link.path()).expect("test symlink should create");

    let error =
        load_snapshot_v2_memory_path(&state, link.path()).expect_err("final symlink must not load");
    assert!(matches!(error, SnapshotV2MemoryLoadError::Open { .. }));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("link-private-name"));
    assert!(!diagnostic.contains("target-private-name"));
}

#[test]
fn private_base_uses_anonymous_dynamic_profile_and_explicit_shared_reservation() {
    let source_memory = test_memory();
    let (image, binding) = write_test_image(&source_memory, TEST_ID);
    let image_file = TempFile::new("mixed", &image);
    let mut memory =
        load_snapshot_v2_memory_binding_from_file(&binding, image_file.open_read_only())
            .expect("lazy memory should load");

    let anonymous = range(aarch64::DRAM_MEM_START + 256 * 1024, 64 * 1024);
    memory
        .insert_region(anonymous)
        .expect("dynamic anonymous region should insert");
    assert_eq!(
        memory
            .regions()
            .iter()
            .find(|region| region.range() == anonymous)
            .expect("dynamic region should exist")
            .backing(),
        GuestMemoryRegionBacking::Anonymous
    );

    let reservation = range(aarch64::DRAM_MEM_START + 512 * 1024, 128 * 1024);
    let online = range(reservation.start().raw_value(), 64 * 1024);
    memory
        .reserve_shared_region(reservation)
        .expect("explicit shared reservation should coexist with private base");
    memory
        .insert_region(online)
        .expect("reservation view should insert");
    assert_eq!(
        memory
            .regions()
            .iter()
            .find(|region| region.range() == online)
            .expect("shared view should exist")
            .backing(),
        GuestMemoryRegionBacking::Shared
    );
    assert_eq!(memory.shared_export_regions().count(), 1);
    memory
        .remove_region(online)
        .expect("shared view should remove before owner drop");
    memory
        .remove_region(anonymous)
        .expect("anonymous dynamic region should remove");
}

#[cfg(target_os = "macos")]
#[test]
fn private_file_discard_becomes_dirty_anonymous_zero_without_source_mutation() {
    let source_memory = test_memory();
    let (image, binding) = write_test_image(&source_memory, TEST_ID);
    let image_file = TempFile::new("discard", &image);
    let mut memory =
        load_snapshot_v2_memory_binding_from_file(&binding, image_file.open_read_only())
            .expect("lazy memory should load");
    let tracker = memory
        .enable_dirty_tracking()
        .expect("clean dirty baseline should enable");
    assert!(
        tracker
            .dirty_pages()
            .expect("dirty query should work")
            .is_empty()
    );

    let target = binding.extents()[0].range();
    let mut source_before = vec![0_u8; usize::try_from(target.size()).unwrap()];
    image_file
        .open_read_only()
        .read_exact_at(&mut source_before, binding.extents()[0].file_offset())
        .expect("source should read");
    assert!(source_before.iter().any(|byte| *byte != 0));

    let outcome = memory.discard_range(target);
    assert!(outcome.is_complete(), "{outcome:?}");
    assert_eq!(outcome.advised_bytes(), target.size());
    let mut zeroed = vec![0xff_u8; source_before.len()];
    memory
        .read_slice(&mut zeroed, target.start())
        .expect("discarded mapping should read");
    assert!(zeroed.iter().all(|byte| *byte == 0));
    assert!(
        !tracker
            .dirty_pages()
            .expect("dirty query should work")
            .is_empty()
    );

    let mut source_after = vec![0_u8; source_before.len()];
    image_file
        .open_read_only()
        .read_exact_at(&mut source_after, binding.extents()[0].file_offset())
        .expect("source should read after discard");
    assert_eq!(source_after, source_before);
}

fn replace_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn replace_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn replace_binding_checksum(bytes: &mut [u8]) {
    bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].fill(0);
    let checksum = crc64(0, bytes);
    bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].copy_from_slice(&checksum.to_le_bytes());
}

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(1);

fn temporary_path(label: &str) -> PathBuf {
    let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "bangbang-v2-memory-{label}-{}-{sequence}",
        std::process::id()
    ))
}

struct TempEntry {
    path: PathBuf,
}

impl TempEntry {
    fn new_path(label: &str) -> Self {
        Self {
            path: temporary_path(label),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempEntry {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct TempFile {
    entry: TempEntry,
}

impl TempFile {
    fn new(label: &str, bytes: &[u8]) -> Self {
        let entry = TempEntry::new_path(label);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(entry.path())
            .expect("test file should create");
        file.write_all(bytes).expect("test file should write");
        drop(file);
        Self { entry }
    }

    fn path(&self) -> &Path {
        self.entry.path()
    }

    fn open_read_only(&self) -> File {
        File::open(self.path()).expect("test file should open read-only")
    }

    fn open_read_write(&self) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.path())
            .expect("test file should open read-write")
    }
}
