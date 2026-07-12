use super::*;
#[cfg(target_os = "macos")]
use crate::memory::{GuestAddress, GuestMemoryLayout, GuestMemoryRange};

#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::io::{BufRead, BufReader, Cursor, Seek, SeekFrom, Write};
#[cfg(target_os = "macos")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(target_os = "macos")]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
#[cfg(target_os = "macos")]
use std::os::unix::net::UnixListener;
#[cfg(target_os = "macos")]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[cfg(target_os = "macos")]
const TEST_MEMORY_BYTES: usize = 16 * 1024;

#[test]
fn paths_and_load_results_redact_host_paths_and_memory() {
    let paths = SnapshotArtifactPaths::new(
        "/sentinel/private/state.snap",
        "/sentinel/private/memory.snap",
    );
    let debug = format!("{paths:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("sentinel"));
    assert!(!debug.contains("state.snap"));
    assert!(!debug.contains("memory.snap"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn generalized_publication_rejects_platform_without_invoking_producer() {
    let paths = SnapshotArtifactPaths::new("state.snap", "memory.snap");
    let called = std::cell::Cell::new(false);
    let error = publish_snapshot_artifacts_with::<std::io::Error, _>(&paths, |_writer| {
        called.set(true);
        Err(std::io::Error::other("producer must not run"))
    })
    .expect_err("non-macOS publication should reject at platform preflight");

    assert!(!called.get());
    assert_eq!(
        error
            .publication()
            .expect("platform rejection should be a publication failure")
            .stage(),
        SnapshotPublicationStage::PlatformCheck
    );
    assert!(error.producer().is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn publishes_and_loads_same_directory_pair() {
    let directory = TestDirectory::new("same-directory");
    let paths = directory.paths("state.snap", "memory.snap");
    let memory = test_memory();

    let outcome = publish_snapshot_artifacts(&paths, &memory).expect("publish should succeed");
    assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
    assert!(paths.state().is_file());
    assert!(paths.memory().is_file());
    assert_eq!(
        fs::metadata(paths.state())
            .expect("state metadata should exist")
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(paths.memory())
            .expect("memory metadata should exist")
            .mode()
            & 0o777,
        0o600
    );
    assert_no_staging(&directory.path);

    let loaded = load_snapshot_artifacts(&paths).expect("committed pair should load");
    assert_eq!(loaded.record(), outcome.record());
    let mut actual = vec![0; TEST_MEMORY_BYTES];
    loaded
        .memory()
        .read_slice(&mut actual, GuestAddress::new(0x4000))
        .expect("loaded memory should be readable");
    assert_eq!(actual, test_bytes());
}

#[cfg(target_os = "macos")]
#[test]
fn producer_publishes_exact_composite_record_after_staging_creation() {
    use crate::snapshot_commit::SnapshotCommitKind;

    let directory = TestDirectory::new("producer-composite");
    let paths = directory.paths("state.snap", "memory.snap");
    let calls = std::cell::Cell::new(0_u8);

    let outcome = publish_snapshot_artifacts_with(&paths, |mut writer| {
        calls.set(calls.get() + 1);
        assert_eq!(staging_entry_count(&directory.path), 2);
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("producer memory should write");
        let record = SnapshotCommitRecord::try_new_composite(binding, b"composite-state".to_vec())
            .expect("composite record should validate");
        Ok::<_, io::Error>(record)
    })
    .expect("composite producer should publish");

    assert_eq!(calls.get(), 1);
    assert_eq!(outcome.record().kind(), SnapshotCommitKind::Composite);
    assert_eq!(
        outcome.record().composite_state(),
        Some(b"composite-state".as_slice())
    );
    let loaded = load_snapshot_artifacts(&paths).expect("composite pair should load");
    assert_eq!(loaded.record(), outcome.record());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn producer_is_not_called_before_private_staging_is_ready() {
    for (index, stage) in [
        SnapshotPublicationStage::StatePathValidation,
        SnapshotPublicationStage::MemoryPathValidation,
        SnapshotPublicationStage::StateDirectoryOpen,
        SnapshotPublicationStage::MemoryDirectoryOpen,
        SnapshotPublicationStage::AliasCheck,
        SnapshotPublicationStage::StateFinalPreflight,
        SnapshotPublicationStage::MemoryFinalPreflight,
        SnapshotPublicationStage::MemoryStagingCreate,
        SnapshotPublicationStage::StateStagingCreate,
        SnapshotPublicationStage::MemoryWrite,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = TestDirectory::new(&format!("producer-not-called-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let calls = std::cell::Cell::new(0_u8);
        let (result, _) = macos::with_publication_failure(stage, || {
            publish_snapshot_artifacts_with(&paths, |mut writer| {
                calls.set(calls.get() + 1);
                let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
                    .expect("fixture memory should write");
                Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
            })
        });

        let error = result.expect_err("injected pre-producer stage should fail");
        assert_eq!(
            error
                .publication()
                .expect("stage injection should be a publication failure")
                .stage(),
            stage
        );
        assert_eq!(calls.get(), 0);
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn producer_explicit_close_satisfies_publication_gate() {
    let directory = TestDirectory::new("producer-explicit-close");
    let paths = directory.paths("state.snap", "memory.snap");

    publish_snapshot_artifacts_with(&paths, |mut writer| {
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("producer memory should write");
        writer.close();
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("explicitly closed producer should publish");

    load_snapshot_artifacts(&paths).expect("explicit-close pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn retained_or_forgotten_success_writer_never_publishes() {
    let retained_directory = TestDirectory::new("producer-retained");
    let retained_paths = retained_directory.paths("state.snap", "memory.snap");
    let retained = std::cell::RefCell::new(None);
    let record = test_memory_only_record();
    let error = publish_snapshot_artifacts_with(&retained_paths, |writer| {
        *retained.borrow_mut() = Some(writer);
        Ok::<_, io::Error>(record)
    })
    .expect_err("retained writer should reject publication");
    let publication = error
        .publication()
        .expect("retained writer should be a publication failure");
    assert_eq!(
        publication.stage(),
        SnapshotPublicationStage::MemoryWriterClose
    );
    assert!(matches!(
        publication.failure(),
        SnapshotPublicationFailure::StagingWriterRetained
    ));
    assert!(!retained_paths.state().exists());
    assert!(!retained_paths.memory().exists());
    assert_no_staging(&retained_directory.path);
    drop(retained.borrow_mut().take());

    let forgotten_directory = TestDirectory::new("producer-forgotten");
    let forgotten_paths = forgotten_directory.paths("state.snap", "memory.snap");
    let record = test_memory_only_record();
    let error = publish_snapshot_artifacts_with(&forgotten_paths, |writer| {
        std::mem::forget(writer);
        Ok::<_, io::Error>(record)
    })
    .expect_err("forgotten writer should reject publication");
    assert!(matches!(
        error.publication().map(SnapshotPublicationError::failure),
        Some(SnapshotPublicationFailure::StagingWriterRetained)
    ));
    assert!(!forgotten_paths.state().exists());
    assert!(!forgotten_paths.memory().exists());
    assert_no_staging(&forgotten_directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn producer_error_owns_writer_without_leaking_diagnostics_or_staging_name() {
    struct ProducerFailure {
        _writer: SnapshotMemoryStagingWriter,
        private: &'static str,
    }

    let directory = TestDirectory::new("producer-error-writer");
    let paths = directory.paths("private-state-sentinel", "private-memory-sentinel");
    let error = publish_snapshot_artifacts_with(&paths, |writer| {
        Err::<SnapshotCommitRecord, _>(ProducerFailure {
            _writer: writer,
            private: "private-producer-sentinel",
        })
    })
    .expect_err("producer failure should abort publication");

    let producer = error
        .producer()
        .expect("typed producer error should be retained");
    assert_eq!(producer.source().private, "private-producer-sentinel");
    assert_eq!(
        producer.memory_cleanup(),
        Some(SnapshotStagingCleanup::Removed)
    );
    assert_eq!(
        producer.state_cleanup(),
        Some(SnapshotStagingCleanup::Removed)
    );
    let diagnostics = format!("{error:?} {error}");
    assert!(!diagnostics.contains("private-producer-sentinel"));
    assert!(!diagnostics.contains("private-state-sentinel"));
    assert!(!diagnostics.contains("private-memory-sentinel"));
    let diagnostic_source = std::error::Error::source(&error)
        .expect("transaction should expose only its redacted producer wrapper");
    assert!(diagnostic_source.source().is_none());
    assert_no_staging(&directory.path);

    publish_snapshot_artifacts_with(&paths, |mut writer| {
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("retry memory should write");
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("producer failure should leave the final names retryable");
    load_snapshot_artifacts(&paths).expect("retry pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn producer_error_remains_primary_when_staging_cleanup_fails() {
    for (index, (cleanup_stage, artifact)) in [
        (
            SnapshotPublicationStage::MemoryStagingCleanup,
            SnapshotArtifactKind::Memory,
        ),
        (
            SnapshotPublicationStage::StateStagingCleanup,
            SnapshotArtifactKind::State,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = TestDirectory::new(&format!("producer-cleanup-failure-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, _) = macos::with_publication_failure(cleanup_stage, || {
            publish_snapshot_artifacts_with(&paths, |_writer| {
                Err::<SnapshotCommitRecord, _>("typed producer sentinel")
            })
        });
        let error = result.expect_err("producer failure should remain primary");
        let producer = error
            .producer()
            .expect("typed producer failure should be retained");

        assert_eq!(producer.source(), &"typed producer sentinel");
        let disposition = match artifact {
            SnapshotArtifactKind::State => producer.state_cleanup(),
            SnapshotArtifactKind::Memory => producer.memory_cleanup(),
        };
        let other_disposition = match artifact {
            SnapshotArtifactKind::State => producer.memory_cleanup(),
            SnapshotArtifactKind::Memory => producer.state_cleanup(),
        };
        assert_eq!(
            disposition,
            Some(SnapshotStagingCleanup::Failed(io::ErrorKind::Other))
        );
        assert_eq!(other_disposition, Some(SnapshotStagingCleanup::Removed));
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn producer_panic_unwinds_staging_without_publishing_and_allows_retry() {
    let directory = TestDirectory::new("producer-panic");
    let paths = directory.paths("state.snap", "memory.snap");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = publish_snapshot_artifacts_with::<io::Error, _>(&paths, |_writer| {
            panic!("private producer panic sentinel")
        });
    }));

    assert!(panic.is_err());
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);

    publish_snapshot_artifacts_with(&paths, |mut writer| {
        let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
            .expect("retry memory should write");
        Ok::<_, io::Error>(SnapshotCommitRecord::new(binding))
    })
    .expect("panic unwind should leave final names retryable");
    load_snapshot_artifacts(&paths).expect("retry pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn producer_output_mismatch_fails_before_any_final_publication() {
    for (name, operation) in [
        ("empty", ProducerMismatch::ReturnOtherBindingWithoutWrite),
        ("extra", ProducerMismatch::AppendTrailingByte),
        ("identity", ProducerMismatch::ReturnOtherBindingAfterWrite),
        (
            "data-length",
            ProducerMismatch::ReturnDifferentLengthBindingAfterWrite,
        ),
        ("trailer", ProducerMismatch::CorruptTrailer),
    ] {
        let directory = TestDirectory::new(&format!("producer-mismatch-{name}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let error = publish_snapshot_artifacts_with(&paths, |mut writer| {
            let record = match operation {
                ProducerMismatch::ReturnOtherBindingWithoutWrite => test_memory_only_record(),
                ProducerMismatch::AppendTrailingByte => {
                    let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    writer
                        .write_all(&[0xaa])
                        .expect("extra fixture byte should write");
                    SnapshotCommitRecord::new(binding)
                }
                ProducerMismatch::ReturnOtherBindingAfterWrite => {
                    write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    test_memory_only_record()
                }
                ProducerMismatch::ReturnDifferentLengthBindingAfterWrite => {
                    write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    test_memory_only_record_with_bytes(TEST_MEMORY_BYTES * 2)
                }
                ProducerMismatch::CorruptTrailer => {
                    let binding = write_snapshot_memory_image(&test_memory(), &mut writer)
                        .expect("fixture memory should write");
                    let trailer = binding
                        .file_length()
                        .checked_sub(8)
                        .expect("fixture should contain a trailer");
                    writer
                        .seek(SeekFrom::Start(trailer))
                        .expect("fixture trailer should seek");
                    writer
                        .write_all(&(binding.checksum() ^ u64::MAX).to_le_bytes())
                        .expect("fixture trailer should overwrite");
                    writer
                        .seek(SeekFrom::End(0))
                        .expect("fixture should return to end");
                    SnapshotCommitRecord::new(binding)
                }
            };
            Ok::<_, io::Error>(record)
        })
        .expect_err("mismatched producer output should fail");

        assert_eq!(
            error
                .publication()
                .expect("mismatch should be a publication failure")
                .stage(),
            SnapshotPublicationStage::MemoryWriteVerify
        );
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn publishes_and_loads_across_directories() {
    let root = TestDirectory::new("cross-directory");
    let state_directory = root.path.join("state");
    let memory_directory = root.path.join("memory");
    fs::create_dir(&state_directory).expect("state directory should create");
    fs::create_dir(&memory_directory).expect("memory directory should create");
    let paths = SnapshotArtifactPaths::new(
        state_directory.join("state.snap"),
        memory_directory.join("memory.snap"),
    );

    publish_snapshot_artifacts(&paths, &test_memory()).expect("publish should succeed");
    load_snapshot_artifacts(&paths).expect("committed pair should load");
    assert_no_staging(&state_directory);
    assert_no_staging(&memory_directory);
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_exact_alias_before_staging() {
    let directory = TestDirectory::new("alias");
    let path = directory.path.join("same.snap");
    let paths = SnapshotArtifactPaths::new(&path, &path);

    let error =
        publish_snapshot_artifacts(&paths, &test_memory()).expect_err("alias should be rejected");
    assert_eq!(error.stage(), SnapshotPublicationStage::AliasCheck);
    assert_eq!(
        error.visibility(),
        SnapshotArtifactVisibility::NoFinalArtifact
    );
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::SameArtifact
    ));
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn existing_final_entries_are_never_replaced() {
    for artifact in [SnapshotArtifactKind::State, SnapshotArtifactKind::Memory] {
        let directory = TestDirectory::new(match artifact {
            SnapshotArtifactKind::State => "existing-state",
            SnapshotArtifactKind::Memory => "existing-memory",
        });
        let paths = directory.paths("state.snap", "memory.snap");
        let existing = match artifact {
            SnapshotArtifactKind::State => paths.state(),
            SnapshotArtifactKind::Memory => paths.memory(),
        };
        fs::write(existing, b"sentinel").expect("fixture should create");

        let error = publish_snapshot_artifacts(&paths, &test_memory())
            .expect_err("existing final should fail");
        assert!(matches!(
            error.failure(),
            SnapshotPublicationFailure::FinalAlreadyExists { artifact: actual }
                if *actual == artifact
        ));
        assert_eq!(
            fs::read(existing).expect("fixture should remain"),
            b"sentinel"
        );
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn final_symlinks_are_not_followed_or_replaced() {
    let directory = TestDirectory::new("symlink");
    let paths = directory.paths("state.snap", "memory.snap");
    let target = directory.path.join("target");
    fs::write(&target, b"sentinel").expect("target should create");
    symlink(&target, paths.state()).expect("symlink should create");

    publish_snapshot_artifacts(&paths, &test_memory()).expect_err("symlink final should fail");
    assert_eq!(
        fs::read(&target).expect("target should remain"),
        b"sentinel"
    );
    assert!(
        fs::symlink_metadata(paths.state())
            .expect("symlink should remain")
            .file_type()
            .is_symlink()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn all_existing_special_entry_types_are_preserved_on_both_paths() {
    for artifact in [SnapshotArtifactKind::State, SnapshotArtifactKind::Memory] {
        for entry_kind in [
            ExistingEntryKind::Directory,
            ExistingEntryKind::Fifo,
            ExistingEntryKind::Socket,
            ExistingEntryKind::ValidSymlink,
            ExistingEntryKind::BrokenSymlink,
        ] {
            let directory = TestDirectory::new(&format!("{artifact}-{entry_kind:?}"));
            let paths = directory.paths("state.snap", "memory.snap");
            let path = match artifact {
                SnapshotArtifactKind::State => paths.state(),
                SnapshotArtifactKind::Memory => paths.memory(),
            };
            let _guard = create_special_entry(path, entry_kind, &directory.path);
            let before = fs::symlink_metadata(path)
                .expect("special entry should exist")
                .mode()
                & u32::from(libc::S_IFMT);

            let error = publish_snapshot_artifacts(&paths, &test_memory())
                .expect_err("existing special entry should fail");

            assert!(matches!(
                error.failure(),
                SnapshotPublicationFailure::FinalAlreadyExists { artifact: actual }
                    if *actual == artifact
            ));
            let after = fs::symlink_metadata(path)
                .expect("special entry should remain")
                .mode()
                & u32::from(libc::S_IFMT);
            assert_eq!(after, before);
            assert_no_staging(&directory.path);
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn parent_symlink_aliases_use_opened_directory_identity() {
    let root = TestDirectory::new("parent-symlink-alias");
    let destination = root.path.join("destination");
    let first_parent = root.path.join("first-parent");
    let second_parent = root.path.join("second-parent");
    fs::create_dir(&destination).expect("destination should create");
    symlink(&destination, &first_parent).expect("first parent symlink should create");
    symlink(&destination, &second_parent).expect("second parent symlink should create");

    let alias = SnapshotArtifactPaths::new(
        first_parent.join("same.snap"),
        second_parent.join("same.snap"),
    );
    let error = publish_snapshot_artifacts(&alias, &test_memory())
        .expect_err("opened-directory alias should fail");
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::SameArtifact
    ));

    let distinct = SnapshotArtifactPaths::new(
        first_parent.join("state.snap"),
        second_parent.join("memory.snap"),
    );
    publish_snapshot_artifacts(&distinct, &test_memory())
        .expect("distinct entries in aliased parent should publish");
    load_snapshot_artifacts(&distinct).expect("aliased-parent pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn parent_path_replacement_cannot_redirect_opened_directories() {
    let root = TestDirectory::new("parent-replace");
    let parent = root.path.join("destination");
    let moved = root.path.join("opened-destination");
    fs::create_dir(&parent).expect("destination should create");
    let paths = SnapshotArtifactPaths::new(parent.join("state.snap"), parent.join("memory.snap"));
    let outcome = macos::with_parent_replacement(
        SnapshotPublicationStage::AliasCheck,
        parent.clone(),
        moved.clone(),
        || publish_snapshot_artifacts(&paths, &test_memory()),
    )
    .expect("opened directory should remain usable");

    assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    let moved_paths =
        SnapshotArtifactPaths::new(moved.join("state.snap"), moved.join("memory.snap"));
    load_snapshot_artifacts(&moved_paths).expect("opened-directory pair should load");
    assert_no_staging(&parent);
    assert_no_staging(&moved);
}

#[cfg(target_os = "macos")]
#[test]
fn case_equivalent_names_fail_safe_or_publish_when_distinct() {
    let directory = TestDirectory::new("case-policy");
    let probe_upper = directory.path.join("CASE-PROBE");
    let probe_lower = directory.path.join("case-probe");
    fs::write(&probe_upper, b"probe").expect("probe should create");
    let case_insensitive = probe_lower.exists();
    fs::remove_file(&probe_upper).expect("probe should remove");

    let paths = directory.paths("PAIR.snap", "pair.snap");
    let result = publish_snapshot_artifacts(&paths, &test_memory());
    if case_insensitive {
        let error = result.expect_err("equivalent names should collide safely");
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::MemoryOrphanVisible
        );
        assert!(paths.memory().exists());
    } else {
        result.expect("case-distinct names should publish");
        load_snapshot_artifacts(&paths).expect("case-distinct pair should load");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_non_normal_final_components_before_io() {
    let directory = TestDirectory::new("invalid-final-components");
    let invalid = [
        PathBuf::new(),
        PathBuf::from("/"),
        PathBuf::from("."),
        PathBuf::from(".."),
        PathBuf::from("trailing/"),
        PathBuf::from("trailing/."),
        PathBuf::from("trailing/.."),
        PathBuf::from(std::ffi::OsString::from_vec(b"nul\0component".to_vec())),
        PathBuf::from(std::ffi::OsString::from_vec(
            b"nul\0parent/state.snap".to_vec(),
        )),
    ];

    for (index, state) in invalid.into_iter().enumerate() {
        let paths =
            SnapshotArtifactPaths::new(state, directory.path.join(format!("memory-{index}.snap")));
        let error = publish_snapshot_artifacts(&paths, &test_memory())
            .expect_err("invalid final component should fail");
        assert_eq!(error.stage(), SnapshotPublicationStage::StatePathValidation);
        assert!(matches!(
            error.failure(),
            SnapshotPublicationFailure::InvalidFinalPath {
                artifact: SnapshotArtifactKind::State
            }
        ));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn missing_and_unwritable_parents_fail_with_owned_staging_cleanup() {
    let missing_root = TestDirectory::new("missing-parent");
    let missing_state = SnapshotArtifactPaths::new(
        missing_root.path.join("missing/state.snap"),
        missing_root.path.join("memory.snap"),
    );
    let error = publish_snapshot_artifacts(&missing_state, &test_memory())
        .expect_err("missing state parent should fail");
    assert_eq!(error.stage(), SnapshotPublicationStage::StateDirectoryOpen);

    let missing_memory = SnapshotArtifactPaths::new(
        missing_root.path.join("state.snap"),
        missing_root.path.join("missing/memory.snap"),
    );
    let error = publish_snapshot_artifacts(&missing_memory, &test_memory())
        .expect_err("missing memory parent should fail");
    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryDirectoryOpen);
    assert_no_staging(&missing_root.path);

    // Root bypasses ordinary mode permission checks.
    // SAFETY: `geteuid` has no arguments and does not mutate memory.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    for artifact in [SnapshotArtifactKind::Memory, SnapshotArtifactKind::State] {
        let root = TestDirectory::new(match artifact {
            SnapshotArtifactKind::State => "unwritable-state",
            SnapshotArtifactKind::Memory => "unwritable-memory",
        });
        let state_directory = root.path.join("state");
        let memory_directory = root.path.join("memory");
        fs::create_dir(&state_directory).expect("state directory should create");
        fs::create_dir(&memory_directory).expect("memory directory should create");
        let restricted = match artifact {
            SnapshotArtifactKind::State => &state_directory,
            SnapshotArtifactKind::Memory => &memory_directory,
        };
        fs::set_permissions(restricted, fs::Permissions::from_mode(0o500))
            .expect("directory should become unwritable");
        let paths = SnapshotArtifactPaths::new(
            state_directory.join("state.snap"),
            memory_directory.join("memory.snap"),
        );

        let error = publish_snapshot_artifacts(&paths, &test_memory())
            .expect_err("unwritable destination should fail");

        fs::set_permissions(restricted, fs::Permissions::from_mode(0o700))
            .expect("directory permissions should restore");
        let expected = match artifact {
            SnapshotArtifactKind::State => SnapshotPublicationStage::StateStagingCreate,
            SnapshotArtifactKind::Memory => SnapshotPublicationStage::MemoryStagingCreate,
        };
        assert_eq!(error.stage(), expected);
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::NoFinalArtifact
        );
        assert_no_staging(&state_directory);
        assert_no_staging(&memory_directory);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn publication_and_load_errors_redact_paths_and_staging_names() {
    let directory = TestDirectory::new("diagnostic-redaction");
    let paths = directory.paths("SENTINEL-STATE.snap", "SENTINEL-MEMORY.snap");
    let (result, _) =
        macos::with_publication_failure(SnapshotPublicationStage::StateFileSync, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
    let error = result.expect_err("injected sync should fail");
    for diagnostic in [format!("{error}"), format!("{error:?}")] {
        assert!(!diagnostic.contains("SENTINEL"));
        assert!(!diagnostic.contains(".bangbang-snapshot-"));
    }

    let load_paths = SnapshotArtifactPaths::new(
        directory.path.join("MISSING-STATE.snap"),
        directory.path.join("MISSING-MEMORY.snap"),
    );
    let error = load_snapshot_artifacts(&load_paths).expect_err("missing state should fail");
    for diagnostic in [format!("{error}"), format!("{error:?}")] {
        assert!(!diagnostic.contains("MISSING"));
        assert!(!diagnostic.contains(directory.path.to_string_lossy().as_ref()));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn state_publish_failure_leaves_typed_memory_orphan() {
    let directory = TestDirectory::new("state-publish-failure");
    let paths = directory.paths("state.snap", "memory.snap");
    let (result, _) =
        macos::with_publication_failure(SnapshotPublicationStage::StatePublish, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
    let error = result.expect_err("injected state publish should fail");

    assert_eq!(
        error.visibility(),
        SnapshotArtifactVisibility::MemoryOrphanVisible
    );
    assert!(paths.memory().is_file());
    assert!(!paths.state().exists());
    assert_eq!(error.memory_cleanup(), None);
    assert_eq!(error.state_cleanup(), Some(SnapshotStagingCleanup::Removed));
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn state_directory_sync_failure_is_committed_uncertain_not_error() {
    let directory = TestDirectory::new("state-directory-sync-failure");
    let paths = directory.paths("state.snap", "memory.snap");
    let (result, _) =
        macos::with_publication_failure(SnapshotPublicationStage::StateDirectorySync, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
    let outcome = result.expect("state rename should remain committed");

    assert_eq!(
        outcome.durability(),
        SnapshotCommitDurability::Uncertain {
            kind: io::ErrorKind::Other
        }
    );
    assert!(paths.state().is_file());
    assert!(paths.memory().is_file());
    load_snapshot_artifacts(&paths).expect("visible committed pair should load");
}

#[cfg(target_os = "macos")]
#[test]
fn successful_trace_orders_file_and_directory_barriers() {
    let directory = TestDirectory::new("trace");
    let paths = directory.paths("state.snap", "memory.snap");
    let (result, order) =
        macos::with_publication_trace(|| publish_snapshot_artifacts(&paths, &test_memory()));
    result.expect("publish should succeed");

    assert_before(
        &order,
        SnapshotPublicationStage::MemoryFileSync,
        SnapshotPublicationStage::MemoryPublish,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::StateFileSync,
        SnapshotPublicationStage::MemoryPublish,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::MemoryPublish,
        SnapshotPublicationStage::MemoryDirectorySync,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::MemoryDirectorySync,
        SnapshotPublicationStage::StatePublish,
    );
    assert_before(
        &order,
        SnapshotPublicationStage::StatePublish,
        SnapshotPublicationStage::StateDirectorySync,
    );
}

#[cfg(target_os = "macos")]
#[test]
fn every_pre_memory_publication_stage_failure_leaves_no_final() {
    let stages = [
        SnapshotPublicationStage::StatePathValidation,
        SnapshotPublicationStage::MemoryPathValidation,
        SnapshotPublicationStage::StateDirectoryOpen,
        SnapshotPublicationStage::MemoryDirectoryOpen,
        SnapshotPublicationStage::AliasCheck,
        SnapshotPublicationStage::StateFinalPreflight,
        SnapshotPublicationStage::MemoryFinalPreflight,
        SnapshotPublicationStage::MemoryStagingCreate,
        SnapshotPublicationStage::StateStagingCreate,
        SnapshotPublicationStage::MemoryWrite,
        SnapshotPublicationStage::MemoryWriterClose,
        SnapshotPublicationStage::MemoryWriteVerify,
        SnapshotPublicationStage::StateEncode,
        SnapshotPublicationStage::StateWrite,
        SnapshotPublicationStage::StateWriteVerify,
        SnapshotPublicationStage::MemoryFileSync,
        SnapshotPublicationStage::StateFileSync,
        SnapshotPublicationStage::MemoryPublishCheck,
        SnapshotPublicationStage::MemoryPublish,
    ];

    for (index, stage) in stages.into_iter().enumerate() {
        let directory = TestDirectory::new(&format!("pre-memory-failure-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, order) = macos::with_publication_failure(stage, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
        let error = result.expect_err("injected stage should fail");

        assert_eq!(error.stage(), stage);
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::NoFinalArtifact
        );
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
        assert!(order.contains(&stage));
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn post_memory_pre_state_failures_leave_one_memory_orphan() {
    for (index, stage) in [
        SnapshotPublicationStage::MemoryDirectorySync,
        SnapshotPublicationStage::StatePublishCheck,
        SnapshotPublicationStage::StatePublish,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = TestDirectory::new(&format!("memory-orphan-failure-{index}"));
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, _) = macos::with_publication_failure(stage, || {
            publish_snapshot_artifacts(&paths, &test_memory())
        });
        let error = result.expect_err("injected stage should fail");

        assert_eq!(error.stage(), stage);
        assert_eq!(
            error.visibility(),
            SnapshotArtifactVisibility::MemoryOrphanVisible
        );
        assert!(!paths.state().exists());
        assert!(paths.memory().is_file());
        assert_eq!(error.memory_cleanup(), None);
        assert_no_staging(&directory.path);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn final_collisions_after_preflight_never_replace_the_winner() {
    let memory_directory = TestDirectory::new("late-memory-collision");
    let memory_paths = memory_directory.paths("state.snap", "memory.snap");
    let memory_result = macos::with_final_collision(
        SnapshotPublicationStage::MemoryPublish,
        memory_paths.memory().to_path_buf(),
        || publish_snapshot_artifacts(&memory_paths, &test_memory()),
    );
    let memory_error = memory_result.expect_err("late memory collision should fail");
    assert_eq!(
        memory_error.visibility(),
        SnapshotArtifactVisibility::NoFinalArtifact
    );
    assert_eq!(
        fs::read(memory_paths.memory()).expect("winner should remain"),
        b"concurrent-final"
    );
    assert!(!memory_paths.state().exists());
    assert_no_staging(&memory_directory.path);

    let state_directory = TestDirectory::new("late-state-collision");
    let state_paths = state_directory.paths("state.snap", "memory.snap");
    let state_result = macos::with_final_collision(
        SnapshotPublicationStage::StatePublish,
        state_paths.state().to_path_buf(),
        || publish_snapshot_artifacts(&state_paths, &test_memory()),
    );
    let state_error = state_result.expect_err("late state collision should fail");
    assert_eq!(
        state_error.visibility(),
        SnapshotArtifactVisibility::MemoryOrphanVisible
    );
    assert_eq!(
        fs::read(state_paths.state()).expect("winner should remain"),
        b"concurrent-final"
    );
    assert!(state_paths.memory().is_file());
    assert_no_staging(&state_directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn observed_staging_replacement_is_retained_and_refused() {
    let directory = TestDirectory::new("staging-replacement");
    let paths = directory.paths("state.snap", "memory.snap");
    let result = macos::with_staging_replacement(
        SnapshotPublicationStage::MemoryPublishCheck,
        directory.path.clone(),
        SnapshotArtifactKind::Memory,
        || publish_snapshot_artifacts(&paths, &test_memory()),
    );
    let error = result.expect_err("observed replacement should fail");

    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryPublishCheck);
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::StagingChanged {
            artifact: SnapshotArtifactKind::Memory
        }
    ));
    assert_eq!(
        error.memory_cleanup(),
        Some(SnapshotStagingCleanup::ChangedRefused)
    );
    assert_eq!(error.state_cleanup(), Some(SnapshotStagingCleanup::Removed));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_eq!(
        find_staging_contents(&directory.path),
        b"replacement-staging"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn missing_staging_entry_is_reported_without_unlink_retry() {
    let directory = TestDirectory::new("staging-missing");
    let paths = directory.paths("state.snap", "memory.snap");
    let result = macos::with_staging_removal(
        SnapshotPublicationStage::MemoryPublishCheck,
        directory.path.clone(),
        SnapshotArtifactKind::Memory,
        || publish_snapshot_artifacts(&paths, &test_memory()),
    );
    let error = result.expect_err("missing staging entry should fail publication");

    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryPublishCheck);
    assert_eq!(
        error.memory_cleanup(),
        Some(SnapshotStagingCleanup::AlreadyAbsent)
    );
    assert_eq!(error.state_cleanup(), Some(SnapshotStagingCleanup::Removed));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn cleanup_failures_do_not_mask_the_primary_failure() {
    for (cleanup_stage, artifact) in [
        (
            SnapshotPublicationStage::MemoryStagingCleanup,
            SnapshotArtifactKind::Memory,
        ),
        (
            SnapshotPublicationStage::StateStagingCleanup,
            SnapshotArtifactKind::State,
        ),
    ] {
        let directory = TestDirectory::new(match artifact {
            SnapshotArtifactKind::State => "state-cleanup-failure",
            SnapshotArtifactKind::Memory => "memory-cleanup-failure",
        });
        let paths = directory.paths("state.snap", "memory.snap");
        let (result, _) = macos::with_publication_failures(
            vec![SnapshotPublicationStage::MemoryFileSync, cleanup_stage],
            || publish_snapshot_artifacts(&paths, &test_memory()),
        );
        let error = result.expect_err("injected file sync should fail");

        assert_eq!(error.stage(), SnapshotPublicationStage::MemoryFileSync);
        let disposition = match artifact {
            SnapshotArtifactKind::State => error.state_cleanup(),
            SnapshotArtifactKind::Memory => error.memory_cleanup(),
        };
        assert_eq!(
            disposition,
            Some(SnapshotStagingCleanup::Failed(io::ErrorKind::Other))
        );
        assert!(!paths.state().exists());
        assert!(!paths.memory().exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn staging_name_collisions_retry_boundedly_and_exhaust_without_clobber() {
    let retry_directory = TestDirectory::new("staging-retry");
    let retry_paths = retry_directory.paths("state.snap", "memory.snap");
    let collision = [0x11; 16];
    let collision_path = staging_fixture_path(
        &retry_directory.path,
        SnapshotArtifactKind::Memory,
        collision,
    );
    fs::write(&collision_path, b"collision-winner").expect("collision fixture should create");
    let result = macos::with_staging_random_names(vec![collision, [0x22; 16], [0x33; 16]], || {
        publish_snapshot_artifacts(&retry_paths, &test_memory())
    });
    result.expect("publisher should retry one staging collision");
    assert_eq!(
        fs::read(&collision_path).expect("collision winner should remain"),
        b"collision-winner"
    );
    fs::remove_file(&collision_path).expect("collision fixture should remove");
    assert_no_staging(&retry_directory.path);

    let exhausted_directory = TestDirectory::new("staging-exhaust");
    let exhausted_paths = exhausted_directory.paths("state.snap", "memory.snap");
    let exhausted = [0x44; 16];
    let exhausted_path = staging_fixture_path(
        &exhausted_directory.path,
        SnapshotArtifactKind::Memory,
        exhausted,
    );
    fs::write(&exhausted_path, b"collision-winner").expect("exhaustion fixture should create");
    let result = macos::with_staging_random_names(vec![exhausted; 16], || {
        publish_snapshot_artifacts(&exhausted_paths, &test_memory())
    });
    let error = result.expect_err("bounded collisions should exhaust");
    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryStagingCreate);
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::Io(io::ErrorKind::AlreadyExists)
    ));
    assert_eq!(
        fs::read(&exhausted_path).expect("collision winner should remain"),
        b"collision-winner"
    );
    assert!(!exhausted_paths.state().exists());
    assert!(!exhausted_paths.memory().exists());
}

#[cfg(target_os = "macos")]
#[test]
fn staging_randomness_failure_precedes_creation() {
    let directory = TestDirectory::new("staging-random");
    let paths = directory.paths("state.snap", "memory.snap");
    let result =
        macos::with_staging_random_failure(|| publish_snapshot_artifacts(&paths, &test_memory()));
    let error = result.expect_err("randomness failure should abort staging");

    assert_eq!(error.stage(), SnapshotPublicationStage::MemoryStagingCreate);
    assert!(matches!(
        error.failure(),
        SnapshotPublicationFailure::RandomnessUnavailable {
            artifact: SnapshotArtifactKind::Memory
        }
    ));
    assert!(!paths.state().exists());
    assert!(!paths.memory().exists());
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
fn multiprocess_contention_has_exactly_one_durable_winner() {
    const CHILD_COUNT: usize = 6;

    let directory = TestDirectory::new("multiprocess");
    let paths = directory.paths("state.snap", "memory.snap");
    let executable = std::env::current_exe().expect("test executable should resolve");
    let mut children = Vec::new();
    for _ in 0..CHILD_COUNT {
        let mut child = Command::new(&executable)
            .arg("--ignored")
            .arg("--exact")
            .arg("snapshot_artifact::tests::multiprocess_publication_child")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env("BANGBANG_SNAPSHOT_CHILD_STATE", paths.state())
            .env("BANGBANG_SNAPSHOT_CHILD_MEMORY", paths.memory())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("publication child should spawn");
        let stdin = child.stdin.take().expect("child stdin should exist");
        let stdout = child.stdout.take().expect("child stdout should exist");
        let mut run = PublicationChild {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        };
        run.wait_ready();
        children.push(run);
    }

    for child in &mut children {
        child.start();
    }

    let mut winners = 0;
    for child in children {
        let output = child.finish();
        if output.contains("publication-result:winner") {
            winners += 1;
        } else {
            assert!(output.contains("publication-result:loser"), "{output}");
        }
    }
    assert_eq!(winners, 1);
    load_snapshot_artifacts(&paths).expect("winner pair should load");
    assert_no_staging(&directory.path);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "launched by the multiprocess contention parent"]
fn multiprocess_publication_child() {
    let Some(state) = std::env::var_os("BANGBANG_SNAPSHOT_CHILD_STATE") else {
        return;
    };
    let Some(memory) = std::env::var_os("BANGBANG_SNAPSHOT_CHILD_MEMORY") else {
        return;
    };
    println!("publication-child:ready");
    io::stdout().flush().expect("ready signal should flush");
    let mut start = [0_u8; 1];
    io::stdin()
        .read_exact(&mut start)
        .expect("start signal should arrive");

    let paths = SnapshotArtifactPaths::new(state, memory);
    match publish_snapshot_artifacts(&paths, &test_memory()) {
        Ok(outcome) => {
            assert_eq!(outcome.durability(), SnapshotCommitDurability::Durable);
            println!("publication-result:winner");
        }
        Err(error) => {
            assert_eq!(
                error.visibility(),
                SnapshotArtifactVisibility::NoFinalArtifact
            );
            assert!(matches!(
                error.failure(),
                SnapshotPublicationFailure::FinalAlreadyExists { .. }
            ));
            println!("publication-result:loser");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn load_stops_at_absent_state_before_memory() {
    let directory = TestDirectory::new("state-absent");
    let paths = directory.paths("state.snap", "memory.snap");
    fs::write(paths.memory(), b"orphan").expect("orphan should create");

    let error = load_snapshot_artifacts(&paths).expect_err("absent state should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateOpen);
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_corrupt_state_and_mismatched_memory() {
    let directory = TestDirectory::new("corruption");
    let paths = directory.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&paths, &test_memory()).expect("publish should succeed");
    let mut state = fs::read(paths.state()).expect("state should read");
    *state.get_mut(0).expect("state byte should exist") ^= 0xff;
    fs::write(paths.state(), &state).expect("state should rewrite");
    let error = load_snapshot_artifacts(&paths).expect_err("corrupt state should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateDecode);

    fs::remove_file(paths.state()).expect("state should remove");
    fs::remove_file(paths.memory()).expect("memory should remove");
    publish_snapshot_artifacts(&paths, &test_memory()).expect("republish should succeed");
    let memory_file = OpenOptionsForTest::append(paths.memory());
    drop(memory_file);
    let error = load_snapshot_artifacts(&paths).expect_err("extended memory should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_nonregular_state_and_memory_without_blocking() {
    for entry_kind in [
        ExistingEntryKind::Directory,
        ExistingEntryKind::Fifo,
        ExistingEntryKind::Socket,
        ExistingEntryKind::ValidSymlink,
        ExistingEntryKind::BrokenSymlink,
    ] {
        let state_directory = TestDirectory::new(&format!("load-state-{entry_kind:?}"));
        let state_paths = state_directory.paths("state.snap", "memory.snap");
        let _guard = create_special_entry(state_paths.state(), entry_kind, &state_directory.path);
        let error =
            load_snapshot_artifacts(&state_paths).expect_err("nonregular state should be rejected");
        assert!(matches!(
            error.stage(),
            SnapshotArtifactLoadStage::StateOpen | SnapshotArtifactLoadStage::StateTypeCheck
        ));

        let memory_directory = TestDirectory::new(&format!("load-memory-{entry_kind:?}"));
        let memory_paths = memory_directory.paths("state.snap", "memory.snap");
        publish_snapshot_artifacts(&memory_paths, &test_memory())
            .expect("fixture pair should publish");
        fs::remove_file(memory_paths.memory()).expect("memory fixture should remove");
        let _guard =
            create_special_entry(memory_paths.memory(), entry_kind, &memory_directory.path);
        let error = load_snapshot_artifacts(&memory_paths)
            .expect_err("nonregular memory should be rejected");
        assert!(matches!(
            error.stage(),
            SnapshotArtifactLoadStage::MemoryOpen | SnapshotArtifactLoadStage::MemoryTypeCheck
        ));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_oversized_state_before_reading() {
    let directory = TestDirectory::new("oversized-state");
    let paths = directory.paths("state.snap", "memory.snap");
    let file = fs::File::create(paths.state()).expect("state fixture should create");
    file.set_len(
        u64::try_from(NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES).expect("maximum should fit u64") + 1,
    )
    .expect("state fixture should resize");

    let error = load_snapshot_artifacts(&paths).expect_err("oversized state should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::StateSizeCheck);
    assert!(matches!(
        error.failure(),
        SnapshotArtifactLoadFailure::StateTooLarge { .. }
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn load_rejects_swapped_truncated_and_corrupt_memory_images() {
    let first = TestDirectory::new("first-pair");
    let second = TestDirectory::new("second-pair");
    let first_paths = first.paths("state.snap", "memory.snap");
    let second_paths = second.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&first_paths, &test_memory()).expect("first pair should publish");
    publish_snapshot_artifacts(&second_paths, &test_memory()).expect("second pair should publish");
    let temporary = first.path.join("temporary-memory");
    fs::rename(first_paths.memory(), &temporary).expect("first memory should move");
    fs::rename(second_paths.memory(), first_paths.memory()).expect("second memory should swap in");
    fs::rename(&temporary, second_paths.memory()).expect("first memory should swap out");
    let error = load_snapshot_artifacts(&first_paths).expect_err("swapped pair should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);

    let truncated = TestDirectory::new("truncated-memory");
    let truncated_paths = truncated.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&truncated_paths, &test_memory())
        .expect("truncated fixture should publish");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(truncated_paths.memory())
        .expect("memory should open");
    let length = file.metadata().expect("memory metadata should read").len();
    file.set_len(length - 1).expect("memory should truncate");
    let error =
        load_snapshot_artifacts(&truncated_paths).expect_err("truncated memory should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);

    let corrupt = TestDirectory::new("corrupt-memory");
    let corrupt_paths = corrupt.paths("state.snap", "memory.snap");
    publish_snapshot_artifacts(&corrupt_paths, &test_memory())
        .expect("corrupt fixture should publish");
    let mut bytes = fs::read(corrupt_paths.memory()).expect("memory should read");
    let byte = bytes.get_mut(64).expect("guest data byte should exist");
    *byte ^= 0xff;
    fs::write(corrupt_paths.memory(), bytes).expect("memory should rewrite");
    let error = load_snapshot_artifacts(&corrupt_paths).expect_err("corrupt memory should fail");
    assert_eq!(error.stage(), SnapshotArtifactLoadStage::MemoryLoad);
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct TestDirectory {
    path: PathBuf,
}

#[cfg(target_os = "macos")]
impl TestDirectory {
    fn new(name: &str) -> Self {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).expect("test randomness should be available");
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let short_name = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(8)
            .collect::<String>();
        let path = Path::new("/tmp").join(format!(
            "bb-sa-{}-{short_name}-{suffix}",
            std::process::id(),
        ));
        fs::create_dir(&path).expect("test directory should create");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("test permissions should set");
        Self { path }
    }

    fn paths(&self, state: &str, memory: &str) -> SnapshotArtifactPaths {
        SnapshotArtifactPaths::new(self.path.join(state), self.path.join(memory))
    }
}

#[cfg(target_os = "macos")]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(target_os = "macos")]
struct OpenOptionsForTest;

#[cfg(target_os = "macos")]
impl OpenOptionsForTest {
    fn append(path: &Path) -> fs::File {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .expect("memory should open");
        file.write_all(&[0]).expect("memory should extend");
        file
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
enum ExistingEntryKind {
    Directory,
    Fifo,
    Socket,
    ValidSymlink,
    BrokenSymlink,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ExistingEntryGuard {
    _listener: Option<UnixListener>,
}

#[cfg(target_os = "macos")]
fn create_special_entry(
    path: &Path,
    kind: ExistingEntryKind,
    directory: &Path,
) -> ExistingEntryGuard {
    let listener = match kind {
        ExistingEntryKind::Directory => {
            fs::create_dir(path).expect("directory entry should create");
            None
        }
        ExistingEntryKind::Fifo => {
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .expect("fixture path should not contain NUL");
            // SAFETY: the fixture path is a live NUL-terminated string and
            // the test owns its private parent directory.
            let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
            assert_eq!(result, 0, "FIFO fixture should create");
            None
        }
        ExistingEntryKind::Socket => {
            Some(UnixListener::bind(path).expect("Unix socket entry should create"))
        }
        ExistingEntryKind::ValidSymlink => {
            let target = directory.join("special-target");
            fs::write(&target, b"target").expect("symlink target should create");
            symlink(target, path).expect("valid symlink should create");
            None
        }
        ExistingEntryKind::BrokenSymlink => {
            symlink(directory.join("missing-target"), path).expect("broken symlink should create");
            None
        }
    };
    ExistingEntryGuard {
        _listener: listener,
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct PublicationChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

#[cfg(target_os = "macos")]
impl PublicationChild {
    fn wait_ready(&mut self) {
        loop {
            let mut line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .expect("child ready output should read");
            assert_ne!(bytes, 0, "child exited before ready");
            if line.contains("publication-child:ready") {
                break;
            }
        }
    }

    fn start(&mut self) {
        let mut stdin = self.stdin.take().expect("child stdin should be retained");
        stdin
            .write_all(&[1])
            .expect("child start signal should write");
    }

    fn finish(mut self) -> String {
        let mut output = String::new();
        self.stdout
            .read_to_string(&mut output)
            .expect("child output should read");
        let status = self.child.wait().expect("child should wait");
        assert!(status.success(), "{output}");
        output
    }
}

#[cfg(target_os = "macos")]
fn test_memory() -> GuestMemory {
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(
            GuestAddress::new(0x4000),
            u64::try_from(TEST_MEMORY_BYTES).expect("fixture size should fit u64"),
        )
        .expect("fixture range should be valid"),
    ])
    .expect("fixture layout should be valid");
    let mut memory = GuestMemory::allocate(&layout).expect("fixture memory should allocate");
    memory
        .write_slice(&test_bytes(), GuestAddress::new(0x4000))
        .expect("fixture bytes should write");
    memory
}

#[cfg(target_os = "macos")]
fn test_bytes() -> Vec<u8> {
    (0..TEST_MEMORY_BYTES)
        .map(|value| u8::try_from(value % 251).expect("fixture byte should fit"))
        .collect()
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum ProducerMismatch {
    ReturnOtherBindingWithoutWrite,
    AppendTrailingByte,
    ReturnOtherBindingAfterWrite,
    ReturnDifferentLengthBindingAfterWrite,
    CorruptTrailer,
}

#[cfg(target_os = "macos")]
fn test_memory_only_record() -> SnapshotCommitRecord {
    test_memory_only_record_with_bytes(TEST_MEMORY_BYTES)
}

#[cfg(target_os = "macos")]
fn test_memory_only_record_with_bytes(bytes: usize) -> SnapshotCommitRecord {
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(
            GuestAddress::new(0x4000),
            u64::try_from(bytes).expect("fixture size should fit u64"),
        )
        .expect("fixture range should be valid"),
    ])
    .expect("fixture layout should be valid");
    let memory = GuestMemory::allocate(&layout).expect("fixture memory should allocate");
    let mut output = Cursor::new(Vec::new());
    let binding = write_snapshot_memory_image(&memory, &mut output)
        .expect("fixture memory record should encode");
    SnapshotCommitRecord::new(binding)
}

#[cfg(target_os = "macos")]
fn staging_entry_count(directory: &Path) -> usize {
    fs::read_dir(directory)
        .expect("directory should read")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".bangbang-snapshot-")
        })
        .count()
}

#[cfg(target_os = "macos")]
fn assert_no_staging(directory: &Path) {
    let entries = fs::read_dir(directory).expect("directory should read");
    for entry in entries {
        let name = entry
            .expect("entry should read")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(!name.starts_with(".bangbang-snapshot-"), "{name}");
    }
}

#[cfg(target_os = "macos")]
fn find_staging_contents(directory: &Path) -> Vec<u8> {
    let entries = fs::read_dir(directory).expect("directory should read");
    for entry in entries {
        let entry = entry.expect("entry should read");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".bangbang-snapshot-") {
            return fs::read(entry.path()).expect("retained staging should read");
        }
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
fn staging_fixture_path(
    directory: &Path,
    artifact: SnapshotArtifactKind,
    random: [u8; 16],
) -> PathBuf {
    let role = match artifact {
        SnapshotArtifactKind::State => "state",
        SnapshotArtifactKind::Memory => "memory",
    };
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    directory.join(format!(".bangbang-snapshot-{role}-{suffix}"))
}

#[cfg(target_os = "macos")]
fn assert_before(
    order: &[SnapshotPublicationStage],
    first: SnapshotPublicationStage,
    second: SnapshotPublicationStage,
) {
    let first = order
        .iter()
        .position(|stage| *stage == first)
        .expect("first stage should be recorded");
    let second = order
        .iter()
        .position(|stage| *stage == second)
        .expect("second stage should be recorded");
    assert!(first < second);
}
