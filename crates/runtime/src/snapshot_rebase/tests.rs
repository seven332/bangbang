use super::*;

#[test]
fn paths_debug_redacts_both_values() {
    let paths = SnapshotV2DiffRebasePaths::new("secret-base", "secret-diff");
    let debug = format!("{paths:?}");
    assert!(!debug.contains("secret-base"));
    assert!(!debug.contains("secret-diff"));
    assert_eq!(paths.base(), Path::new("secret-base"));
    assert_eq!(paths.diff(), Path::new("secret-diff"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_fails_before_callback_or_path_access() {
    let paths = SnapshotV2DiffRebasePaths::new("", "");
    let mut callbacks = 0;
    let error = rebase_snapshot_v2_diff_paths_with_cancel(&paths, |_| {
        callbacks += 1;
        false
    })
    .expect_err("unsupported target must reject the operation");
    assert_eq!(callbacks, 0);
    assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::PlatformCheck);
    assert!(matches!(
        error.failure(),
        SnapshotV2DiffRebaseFailure::UnsupportedPlatform
    ));
    assert_eq!(error.staging_cleanup(), None);
}

#[cfg(target_os = "macos")]
mod macos_tests {
    use std::ffi::CString;
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange, aarch64};
    use crate::snapshot_diff_v2_13::{
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION, SnapshotV2DiffBase, SnapshotV2DiffSelection,
        write_snapshot_v2_diff_layer,
    };
    use crate::snapshot_memory_v2::{
        SnapshotV2MemoryBinding, write_snapshot_v2_memory_image_with_compatibility_version,
    };

    use super::super::macos::{
        with_exchange_failure, with_parent_replacement, with_path_removal, with_path_replacement,
        with_rebase_failures, with_staging_corruption, with_staging_mode,
        with_staging_random_failure, with_staging_random_names, with_staging_removal,
        with_staging_replacement,
    };
    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("bb-rb-{label}-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).expect("test directory should create");
            Self { path }
        }

        fn child(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        fn staging_entries(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.path)
                .expect("test directory should enumerate")
                .map(|entry| entry.expect("test entry should read"))
                .filter(|entry| {
                    entry
                        .file_name()
                        .as_encoded_bytes()
                        .starts_with(b".bangbang-snapshot-rebase-")
                })
                .map(|entry| entry.path())
                .collect()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct RebaseFixture {
        directory: TestDirectory,
        paths: SnapshotV2DiffRebasePaths,
        base_before: Vec<u8>,
        base_before_facts: TestFileFacts,
        diff_before: Vec<u8>,
        diff_before_facts: TestFileFacts,
        result: SnapshotV2MemoryBinding,
        expected_byte: u8,
    }

    type TestFileFacts = (u64, u64, u32, u64, i64, i64, i64, i64);

    impl RebaseFixture {
        fn complete(label: &str) -> Self {
            let directory = TestDirectory::new(label);
            let base_path = directory.child("base.mem");
            let diff_path = directory.child("next.diff");
            let range = page_range(0, 4);
            let base_memory = memory_with_byte(range, 0x19);
            let base = write_complete(&base_path, &base_memory);
            let result_memory = memory_with_byte(range, 0xa7);
            let result = write_layer(
                &diff_path,
                &result_memory,
                SnapshotV2DiffBase::Image(base),
                &[range],
            );
            Self::from_parts(directory, base_path, diff_path, result, 0xa7)
        }

        fn zero_root(label: &str) -> Self {
            let directory = TestDirectory::new(label);
            let base_path = directory.child("base.diff");
            let diff_path = directory.child("next.diff");
            let range = page_range(0, 4);
            let root_memory = memory_with_byte(range, 0x31);
            let root = write_layer(&base_path, &root_memory, SnapshotV2DiffBase::Zero, &[range]);
            let result_memory = memory_with_byte(range, 0xc4);
            let result = write_layer(
                &diff_path,
                &result_memory,
                SnapshotV2DiffBase::Image(root),
                &[range],
            );
            Self::from_parts(directory, base_path, diff_path, result, 0xc4)
        }

        fn from_parts(
            directory: TestDirectory,
            base_path: PathBuf,
            diff_path: PathBuf,
            result: SnapshotV2MemoryBinding,
            expected_byte: u8,
        ) -> Self {
            let base_before = fs::read(&base_path).expect("base should read");
            let base_before_facts = test_file_facts(&base_path);
            let diff_before = fs::read(&diff_path).expect("diff should read");
            let diff_before_facts = test_file_facts(&diff_path);
            Self {
                directory,
                paths: SnapshotV2DiffRebasePaths::new(base_path, diff_path),
                base_before,
                base_before_facts,
                diff_before,
                diff_before_facts,
                result,
                expected_byte,
            }
        }

        fn assert_uncommitted_and_clean(&self) {
            self.assert_uncommitted_bytes_and_clean();
            assert_eq!(test_file_facts(self.paths.base()), self.base_before_facts);
            assert_eq!(test_file_facts(self.paths.diff()), self.diff_before_facts);
        }

        fn assert_uncommitted_bytes_and_clean(&self) {
            assert_eq!(
                fs::read(self.paths.base()).expect("base should remain readable"),
                self.base_before
            );
            assert_eq!(
                fs::read(self.paths.diff()).expect("diff should remain readable"),
                self.diff_before
            );
            assert!(self.directory.staging_entries().is_empty());
        }

        fn assert_committed(&self, outcome: &SnapshotV2DiffRebaseOutcome) {
            assert_eq!(outcome.binding(), &self.result);
            assert_ne!(
                fs::read(self.paths.base()).expect("committed base should read"),
                self.base_before
            );
            assert_eq!(
                fs::read(self.paths.diff()).expect("diff should remain readable"),
                self.diff_before
            );
            assert_eq!(test_file_facts(self.paths.diff()), self.diff_before_facts);
            let file = File::open(self.paths.base()).expect("committed base should open");
            for extent in self.result.extents().iter().copied() {
                let mut bytes = vec![
                    0_u8;
                    usize::try_from(extent.range().size())
                        .expect("extent should fit test memory")
                ];
                file.read_exact_at(&mut bytes, extent.file_offset())
                    .expect("committed payload should read");
                assert!(bytes.iter().all(|byte| *byte == self.expected_byte));
            }
            assert_eq!(
                fs::metadata(self.paths.base())
                    .expect("committed mode should read")
                    .mode()
                    & 0o7777,
                0o600
            );
        }
    }

    fn page_range(first_page: u64, page_count: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START + first_page * aarch64::GUEST_PAGE_SIZE),
            page_count * aarch64::GUEST_PAGE_SIZE,
        )
        .expect("test range should validate")
    }

    fn test_file_facts(path: &Path) -> TestFileFacts {
        let metadata = fs::metadata(path).expect("test file facts should read");
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.size(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }

    fn memory_with_byte(range: GuestMemoryRange, byte: u8) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![range]).expect("test layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        memory
            .write_slice(
                &vec![byte; usize::try_from(range.size()).expect("range should fit")],
                range.start(),
            )
            .expect("test memory should populate");
        memory
    }

    fn writable_file(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .expect("test artifact should create")
    }

    fn write_complete(path: &Path, memory: &GuestMemory) -> SnapshotV2MemoryBinding {
        let mut file = writable_file(path);
        write_snapshot_v2_memory_image_with_compatibility_version(
            memory,
            &mut file,
            NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION,
        )
        .expect("complete image should write")
    }

    fn write_layer(
        path: &Path,
        memory: &GuestMemory,
        base: SnapshotV2DiffBase,
        selected: &[GuestMemoryRange],
    ) -> SnapshotV2MemoryBinding {
        let selection = SnapshotV2DiffSelection::try_from_ranges(memory, selected)
            .expect("selection should validate");
        let mut file = writable_file(path);
        write_snapshot_v2_diff_layer(memory, &mut file, base, &selection)
            .expect("Diff layer should write")
            .result()
            .clone()
    }

    fn assert_file_matches_memory(
        path: &Path,
        binding: &SnapshotV2MemoryBinding,
        expected: &GuestMemory,
    ) {
        let file = File::open(path).expect("complete result should open");
        for extent in binding.extents().iter().copied() {
            let length = usize::try_from(extent.range().size()).expect("extent should fit usize");
            let mut actual = vec![0_u8; length];
            let mut wanted = vec![0_u8; length];
            file.read_exact_at(&mut actual, extent.file_offset())
                .expect("result extent should read");
            expected
                .read_slice(&mut wanted, extent.range().start())
                .expect("expected extent should read");
            assert_eq!(actual, wanted);
        }
    }

    fn expect_materialization_failure(
        directory: &TestDirectory,
        paths: &SnapshotV2DiffRebasePaths,
    ) -> SnapshotV2DiffRebaseError {
        let base_before = fs::read(paths.base()).expect("rejected base should read");
        let diff_before = fs::read(paths.diff()).expect("rejected diff should read");
        let error = rebase_snapshot_v2_diff_paths(paths)
            .expect_err("invalid materialization inputs should fail");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::Materialization);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::Materialization { .. }
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        assert_eq!(
            fs::read(paths.base()).expect("base should remain"),
            base_before
        );
        assert_eq!(
            fs::read(paths.diff()).expect("diff should remain"),
            diff_before
        );
        assert!(directory.staging_entries().is_empty());
        error
    }

    #[test]
    fn complete_and_zero_root_bases_exchange_durably() {
        for fixture in [
            RebaseFixture::complete("durable-complete"),
            RebaseFixture::zero_root("durable-zero-root"),
        ] {
            let outcome =
                rebase_snapshot_v2_diff_paths(&fixture.paths).expect("valid pair should commit");
            assert_eq!(outcome.commit(), SnapshotV2DiffRebaseCommit::Durable);
            fixture.assert_committed(&outcome);
            assert!(fixture.directory.staging_entries().is_empty());
        }
    }

    #[test]
    fn explicit_mode_fixup_does_not_depend_on_creation_mode() {
        let fixture = RebaseFixture::complete("explicit-mode");
        let outcome = with_staging_mode(0o000, || {
            rebase_snapshot_v2_diff_paths(&fixture.paths)
                .expect("explicit chmod should recover a zero creation mode")
        });
        assert_eq!(outcome.commit(), SnapshotV2DiffRebaseCommit::Durable);
        fixture.assert_committed(&outcome);
        assert!(fixture.directory.staging_entries().is_empty());
    }

    #[test]
    fn stable_parent_symlink_resolves_to_one_retained_anchor() {
        let fixture = RebaseFixture::complete("parent-symlink");
        let parent_alias = fixture.directory.child("parent-alias");
        std::os::unix::fs::symlink(&fixture.directory.path, &parent_alias)
            .expect("parent alias should create");
        let paths =
            SnapshotV2DiffRebasePaths::new(parent_alias.join("base.mem"), fixture.paths.diff());
        let outcome = rebase_snapshot_v2_diff_paths(&paths)
            .expect("stable parent alias should retain the same directory");
        assert_eq!(outcome.commit(), SnapshotV2DiffRebaseCommit::Durable);
        fixture.assert_committed(&outcome);
        assert!(fixture.directory.staging_entries().is_empty());
    }

    #[test]
    fn sparse_cross_directory_and_repeated_rebases_are_exact() {
        let directory = TestDirectory::new("sparse-repeated");
        let diff_directory = TestDirectory::new("cross-directory-diff");
        let base_path = directory.child("base.mem");
        let first_diff = diff_directory.child("first.diff");
        let whole = page_range(0, 4);
        let changed = page_range(0, 1);
        let base_memory = memory_with_byte(whole, 0x21);
        let base = write_complete(&base_path, &base_memory);

        let mut first_memory = memory_with_byte(whole, 0x21);
        first_memory
            .write_slice(
                &vec![0x72; usize::try_from(changed.size()).expect("page should fit")],
                changed.start(),
            )
            .expect("selected page should update");
        let first_result = write_layer(
            &first_diff,
            &first_memory,
            SnapshotV2DiffBase::Image(base),
            &[changed],
        );
        let first_diff_before = fs::read(&first_diff).expect("first diff should read");
        let first_paths = SnapshotV2DiffRebasePaths::new(&base_path, &first_diff);
        let first = rebase_snapshot_v2_diff_paths(&first_paths)
            .expect("cross-directory sparse rebase should commit");
        assert_eq!(first.commit(), SnapshotV2DiffRebaseCommit::Durable);
        assert_eq!(first.binding(), &first_result);
        assert_file_matches_memory(&base_path, &first_result, &first_memory);
        assert_eq!(
            fs::read(&first_diff).expect("first diff should remain"),
            first_diff_before
        );

        let second_diff = directory.child("second.diff");
        let second_memory = memory_with_byte(whole, 0xe3);
        let second_result = write_layer(
            &second_diff,
            &second_memory,
            SnapshotV2DiffBase::Image(first_result),
            &[whole],
        );
        let second_diff_before = fs::read(&second_diff).expect("second diff should read");
        let second_paths = SnapshotV2DiffRebasePaths::new(&base_path, &second_diff);
        let second =
            rebase_snapshot_v2_diff_paths(&second_paths).expect("sequential rebase should commit");
        assert_eq!(second.commit(), SnapshotV2DiffRebaseCommit::Durable);
        assert_eq!(second.binding(), &second_result);
        assert_file_matches_memory(&base_path, &second_result, &second_memory);
        assert_eq!(
            fs::read(&second_diff).expect("second diff should remain"),
            second_diff_before
        );
        assert!(directory.staging_entries().is_empty());
        assert!(diff_directory.staging_entries().is_empty());
    }

    #[test]
    fn cancellation_before_exchange_preserves_base_and_cleans_staging() {
        let fixture = RebaseFixture::complete("cancel-exchange");
        let error = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |stage| {
            stage == SnapshotV2DiffRebaseStage::AtomicExchange
        })
        .expect_err("exchange checkpoint cancellation should fail");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::AtomicExchange);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::Cancelled
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn every_outer_precommit_checkpoint_can_cancel_without_committing() {
        for stage in [
            SnapshotV2DiffRebaseStage::PlatformCheck,
            SnapshotV2DiffRebaseStage::BasePathValidation,
            SnapshotV2DiffRebaseStage::BaseDirectoryOpen,
            SnapshotV2DiffRebaseStage::BaseFileOpen,
            SnapshotV2DiffRebaseStage::BaseValidation,
            SnapshotV2DiffRebaseStage::DiffPathValidation,
            SnapshotV2DiffRebaseStage::DiffDirectoryOpen,
            SnapshotV2DiffRebaseStage::DiffFileOpen,
            SnapshotV2DiffRebaseStage::DiffValidation,
            SnapshotV2DiffRebaseStage::SourceAliasCheck,
            SnapshotV2DiffRebaseStage::SourceDuplication,
            SnapshotV2DiffRebaseStage::StagingCreate,
            SnapshotV2DiffRebaseStage::Materialization,
            SnapshotV2DiffRebaseStage::ResultFileSync,
            SnapshotV2DiffRebaseStage::SourceStability,
            SnapshotV2DiffRebaseStage::DirectoryStability,
            SnapshotV2DiffRebaseStage::EntryStability,
            SnapshotV2DiffRebaseStage::AtomicExchange,
        ] {
            let fixture = RebaseFixture::complete(&format!("cancel-{stage:?}"));
            let error = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |current| {
                current == stage
            })
            .expect_err("precommit checkpoint should cancel");
            assert_eq!(error.stage(), stage);
            assert!(matches!(
                error.failure(),
                SnapshotV2DiffRebaseFailure::Cancelled
            ));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn every_outer_precommit_stage_failure_preserves_inputs() {
        for stage in [
            SnapshotV2DiffRebaseStage::PlatformCheck,
            SnapshotV2DiffRebaseStage::BasePathValidation,
            SnapshotV2DiffRebaseStage::BaseDirectoryOpen,
            SnapshotV2DiffRebaseStage::BaseFileOpen,
            SnapshotV2DiffRebaseStage::BaseValidation,
            SnapshotV2DiffRebaseStage::DiffPathValidation,
            SnapshotV2DiffRebaseStage::DiffDirectoryOpen,
            SnapshotV2DiffRebaseStage::DiffFileOpen,
            SnapshotV2DiffRebaseStage::DiffValidation,
            SnapshotV2DiffRebaseStage::SourceAliasCheck,
            SnapshotV2DiffRebaseStage::SourceDuplication,
            SnapshotV2DiffRebaseStage::StagingCreate,
            SnapshotV2DiffRebaseStage::Materialization,
            SnapshotV2DiffRebaseStage::ResultFileSync,
            SnapshotV2DiffRebaseStage::SourceStability,
            SnapshotV2DiffRebaseStage::DirectoryStability,
            SnapshotV2DiffRebaseStage::EntryStability,
            SnapshotV2DiffRebaseStage::AtomicExchange,
        ] {
            let fixture = RebaseFixture::complete(&format!("failure-{stage:?}"));
            let (result, order) = with_rebase_failures(vec![stage], || {
                rebase_snapshot_v2_diff_paths(&fixture.paths)
            });
            let error = result.expect_err("injected precommit stage should fail");
            assert_eq!(error.stage(), stage);
            assert!(matches!(
                error.failure(),
                SnapshotV2DiffRebaseFailure::Io {
                    kind: io::ErrorKind::Other
                }
            ));
            assert!(order.contains(&stage));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn nested_materialization_cancellation_is_a_precommit_cancel() {
        let fixture = RebaseFixture::complete("cancel-materialization");
        let mut materialization_callbacks = 0;
        let error = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |stage| {
            if stage == SnapshotV2DiffRebaseStage::Materialization {
                materialization_callbacks += 1;
                materialization_callbacks == 2
            } else {
                false
            }
        })
        .expect_err("nested materialization should cancel");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::Materialization);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::Cancelled
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn final_callback_mutation_is_detected_before_swap() {
        let fixture = RebaseFixture::complete("final-race");
        let moved = fixture.directory.child("old-base.mem");
        let base = fixture.paths.base().to_path_buf();
        let error = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |stage| {
            if stage == SnapshotV2DiffRebaseStage::AtomicExchange {
                fs::rename(&base, &moved).expect("base should move during test callback");
                fs::write(&base, b"replacement").expect("replacement should create");
            }
            false
        })
        .expect_err("final identity checks should reject the replacement");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::SourceStability);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::SourceChanged {
                input: SnapshotV2DiffRebaseInput::Base
            }
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        assert_eq!(
            fs::read(&base).expect("replacement should read"),
            b"replacement"
        );
        assert!(fixture.directory.staging_entries().is_empty());
    }

    #[test]
    fn final_callback_same_inode_staging_mutation_is_detected() {
        let fixture = RebaseFixture::complete("final-staging-mutation");
        let error = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |stage| {
            if stage == SnapshotV2DiffRebaseStage::AtomicExchange {
                let staging = fixture.directory.staging_entries();
                assert_eq!(staging.len(), 1);
                fs::write(&staging[0], b"same-inode-corruption")
                    .expect("staging should corrupt through its name");
            }
            false
        })
        .expect_err("final full-facts gate should reject same-inode mutation");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::EntryStability);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::StagingChanged
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn panic_unwind_removes_only_owned_staging() {
        let fixture = RebaseFixture::complete("panic-cleanup");
        let mut callbacks = 0;
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |stage| {
                if stage == SnapshotV2DiffRebaseStage::Materialization {
                    callbacks += 1;
                    if callbacks == 2 {
                        panic!("injected caller panic");
                    }
                }
                false
            });
        }));
        assert!(panic.is_err());
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn injected_precommit_failure_keeps_primary_error_and_cleanup() {
        let fixture = RebaseFixture::complete("precommit-failure");
        let (result, order) =
            with_rebase_failures(vec![SnapshotV2DiffRebaseStage::ResultFileSync], || {
                rebase_snapshot_v2_diff_paths(&fixture.paths)
            });
        let error = result.expect_err("injected sync failure should abort");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::ResultFileSync);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::Io {
                kind: io::ErrorKind::Other
            }
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        assert!(order.contains(&SnapshotV2DiffRebaseStage::ResultFileSync));
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn precommit_cleanup_failure_does_not_mask_the_primary_cancel() {
        let fixture = RebaseFixture::complete("precommit-cleanup-failure");
        let parent = fixture.directory.path.clone();
        let error = rebase_snapshot_v2_diff_paths_with_cancel(&fixture.paths, |stage| {
            if stage == SnapshotV2DiffRebaseStage::ResultFileSync {
                fs::set_permissions(&parent, fs::Permissions::from_mode(0o500))
                    .expect("test parent should become unwritable");
                true
            } else {
                false
            }
        })
        .expect_err("result-sync cancellation should remain primary");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))
            .expect("test parent permissions should restore");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::ResultFileSync);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::Cancelled
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Failed(
                io::ErrorKind::PermissionDenied
            ))
        );
        assert_eq!(fixture.directory.staging_entries().len(), 1);
        assert_eq!(
            fs::read(fixture.paths.base()).expect("base should remain"),
            fixture.base_before
        );
        assert_eq!(
            fs::read(fixture.paths.diff()).expect("diff should remain"),
            fixture.diff_before
        );
    }

    #[test]
    fn exchange_syscall_failure_is_precommit_and_retryable() {
        let fixture = RebaseFixture::complete("exchange-failure");
        let error = with_exchange_failure(|| {
            rebase_snapshot_v2_diff_paths(&fixture.paths)
                .expect_err("injected exchange failure should abort")
        });
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::AtomicExchange);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::AtomicExchangeUnavailable {
                kind: io::ErrorKind::Other
            }
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        fixture.assert_uncommitted_and_clean();

        let outcome = rebase_snapshot_v2_diff_paths(&fixture.paths)
            .expect("fresh retry should commit unchanged inputs");
        assert_eq!(outcome.commit(), SnapshotV2DiffRebaseCommit::Durable);
        fixture.assert_committed(&outcome);
    }

    #[test]
    fn observed_component_and_parent_replacements_abort_safely() {
        let fixture = RebaseFixture::complete("diff-removal");
        let moved_diff = fixture.directory.child("moved-next.diff");
        let (result, _) = with_path_removal(
            SnapshotV2DiffRebaseStage::EntryStability,
            fixture.paths.diff().to_path_buf(),
            moved_diff.clone(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let error = result.expect_err("missing diff entry should abort");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::EntryStability);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::EntryChanged {
                input: SnapshotV2DiffRebaseInput::Diff
            }
        ));
        fs::rename(moved_diff, fixture.paths.diff()).expect("diff should restore");
        fixture.assert_uncommitted_bytes_and_clean();

        let fixture = RebaseFixture::complete("parent-replacement");
        let parent = fixture.directory.path.clone();
        let moved_parent = parent.with_extension("moved");
        let (result, _) = with_parent_replacement(
            SnapshotV2DiffRebaseStage::AtomicExchange,
            parent.clone(),
            moved_parent.clone(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let error = result.expect_err("replaced parent path should abort");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::DirectoryStability);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::DirectoryChanged {
                input: SnapshotV2DiffRebaseInput::Base
            }
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        fs::remove_dir(&parent).expect("replacement parent should be empty");
        fs::rename(moved_parent, &parent).expect("original parent should restore");
        fixture.assert_uncommitted_bytes_and_clean();
    }

    #[test]
    fn observed_staging_replacement_is_retained_and_classified() {
        let fixture = RebaseFixture::complete("staging-replacement");
        let moved = fixture.directory.child("owned-result.moved");
        let (result, _) = with_staging_replacement(
            SnapshotV2DiffRebaseStage::EntryStability,
            fixture.directory.path.clone(),
            moved,
            b"foreign-staging".to_vec(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let error = result.expect_err("staging substitution should abort");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::EntryStability);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::StagingChanged
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::ChangedRefused)
        );
        assert_eq!(fixture.directory.staging_entries().len(), 1);
        assert_eq!(
            fs::read(&fixture.directory.staging_entries()[0])
                .expect("foreign staging should survive"),
            b"foreign-staging"
        );
        assert_eq!(
            fs::read(fixture.paths.base()).expect("base should remain"),
            fixture.base_before
        );
        assert_eq!(
            fs::read(fixture.paths.diff()).expect("diff should remain"),
            fixture.diff_before
        );
    }

    #[test]
    fn observed_staging_removal_is_not_retried_against_another_name() {
        let fixture = RebaseFixture::complete("staging-removal");
        let moved = fixture.directory.child("moved-owned-result");
        let (result, _) = with_staging_removal(
            SnapshotV2DiffRebaseStage::EntryStability,
            fixture.directory.path.clone(),
            moved.clone(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let error = result.expect_err("missing staging name should abort");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::EntryStability);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::StagingChanged
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::AlreadyAbsent)
        );
        assert!(moved.is_file());
        assert!(fixture.directory.staging_entries().is_empty());
        assert_eq!(
            fs::read(fixture.paths.base()).expect("base should remain"),
            fixture.base_before
        );
        assert_eq!(
            fs::read(fixture.paths.diff()).expect("diff should remain"),
            fixture.diff_before
        );
    }

    #[test]
    fn synchronized_result_corruption_fails_before_exchange() {
        let fixture = RebaseFixture::complete("result-corruption");
        let (result, _) = with_staging_corruption(
            SnapshotV2DiffRebaseStage::ResultFileSync,
            fixture.directory.path.clone(),
            b"corrupt-result".to_vec(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let error = result.expect_err("outer result verification should reject corruption");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::ResultFileSync);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::ResultVerification { .. }
        ));
        assert_eq!(
            error.staging_cleanup(),
            Some(SnapshotV2DiffRebaseCleanup::Removed)
        );
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn postcommit_cleanup_failure_still_runs_directory_sync() {
        let fixture = RebaseFixture::complete("cleanup-failure");
        let (result, order) =
            with_rebase_failures(vec![SnapshotV2DiffRebaseStage::DisplacedCleanup], || {
                rebase_snapshot_v2_diff_paths(&fixture.paths)
            });
        let outcome = result.expect("postcommit failure must return an outcome");
        assert_eq!(
            outcome.commit(),
            SnapshotV2DiffRebaseCommit::Uncertain {
                stage: SnapshotV2DiffRebaseStage::DisplacedCleanup,
                failure: SnapshotV2DiffRebaseCommitFailure::Cleanup,
                cleanup: SnapshotV2DiffRebaseCleanup::Failed(io::ErrorKind::Other),
                directory_sync: None,
            }
        );
        fixture.assert_committed(&outcome);
        let cleanup_index = order
            .iter()
            .position(|stage| *stage == SnapshotV2DiffRebaseStage::DisplacedCleanup)
            .expect("cleanup stage should run");
        let sync_index = order
            .iter()
            .position(|stage| *stage == SnapshotV2DiffRebaseStage::BaseDirectorySync)
            .expect("directory sync stage should run");
        assert!(cleanup_index < sync_index);
        assert_eq!(fixture.directory.staging_entries().len(), 1);
    }

    #[test]
    fn postcommit_preserves_first_uncertainty_and_records_later_sync() {
        let fixture = RebaseFixture::complete("postcommit-first-error");
        let (result, order) = with_rebase_failures(
            vec![
                SnapshotV2DiffRebaseStage::CommitVerification,
                SnapshotV2DiffRebaseStage::BaseDirectorySync,
            ],
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let outcome = result.expect("committed failures must return an outcome");
        assert_eq!(
            outcome.commit(),
            SnapshotV2DiffRebaseCommit::Uncertain {
                stage: SnapshotV2DiffRebaseStage::CommitVerification,
                failure: SnapshotV2DiffRebaseCommitFailure::Io(io::ErrorKind::Other),
                cleanup: SnapshotV2DiffRebaseCleanup::Removed,
                directory_sync: Some(io::ErrorKind::Other),
            }
        );
        fixture.assert_committed(&outcome);
        assert!(fixture.directory.staging_entries().is_empty());
        assert!(order.ends_with(&[
            SnapshotV2DiffRebaseStage::DisplacedCleanup,
            SnapshotV2DiffRebaseStage::BaseDirectorySync,
            SnapshotV2DiffRebaseStage::Complete,
        ]));
    }

    #[test]
    fn postcommit_base_replacement_is_reported_without_rollback() {
        let fixture = RebaseFixture::complete("committed-base-replacement");
        let committed_result = fixture.directory.child("committed-result.moved");
        let (result, order) = with_path_replacement(
            SnapshotV2DiffRebaseStage::CommitVerification,
            fixture.paths.base().to_path_buf(),
            committed_result.clone(),
            b"foreign-base".to_vec(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let outcome = result.expect("postcommit substitution must return an outcome");
        assert_eq!(
            outcome.commit(),
            SnapshotV2DiffRebaseCommit::Uncertain {
                stage: SnapshotV2DiffRebaseStage::CommitVerification,
                failure: SnapshotV2DiffRebaseCommitFailure::BaseEntryChanged,
                cleanup: SnapshotV2DiffRebaseCleanup::Removed,
                directory_sync: None,
            }
        );
        assert_eq!(
            fs::read(fixture.paths.base()).expect("replacement base should survive"),
            b"foreign-base"
        );
        assert_ne!(
            fs::read(committed_result).expect("committed result should remain reachable"),
            fixture.base_before
        );
        assert!(fixture.directory.staging_entries().is_empty());
        assert!(order.contains(&SnapshotV2DiffRebaseStage::BaseDirectorySync));
    }

    #[test]
    fn postcommit_displaced_replacement_is_refused_and_synced() {
        let fixture = RebaseFixture::complete("displaced-replacement");
        let displaced = fixture.directory.child("displaced-base.moved");
        let (result, order) = with_staging_replacement(
            SnapshotV2DiffRebaseStage::DisplacedCleanup,
            fixture.directory.path.clone(),
            displaced,
            b"foreign-displaced".to_vec(),
            || rebase_snapshot_v2_diff_paths(&fixture.paths),
        );
        let outcome = result.expect("committed cleanup mismatch must return an outcome");
        assert_eq!(
            outcome.commit(),
            SnapshotV2DiffRebaseCommit::Uncertain {
                stage: SnapshotV2DiffRebaseStage::DisplacedCleanup,
                failure: SnapshotV2DiffRebaseCommitFailure::Cleanup,
                cleanup: SnapshotV2DiffRebaseCleanup::ChangedRefused,
                directory_sync: None,
            }
        );
        fixture.assert_committed(&outcome);
        assert_eq!(fixture.directory.staging_entries().len(), 1);
        assert_eq!(
            fs::read(&fixture.directory.staging_entries()[0])
                .expect("foreign displaced entry should survive"),
            b"foreign-displaced"
        );
        assert!(order.contains(&SnapshotV2DiffRebaseStage::BaseDirectorySync));
    }

    #[test]
    fn randomness_failure_happens_before_any_staging_ownership() {
        let fixture = RebaseFixture::complete("random-failure");
        let error = with_staging_random_failure(|| {
            rebase_snapshot_v2_diff_paths(&fixture.paths)
                .expect_err("randomness failure should abort")
        });
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::StagingCreate);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::RandomnessUnavailable
        ));
        assert_eq!(error.staging_cleanup(), None);
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn random_name_collision_retries_without_claiming_foreign_entry() {
        let fixture = RebaseFixture::complete("random-collision");
        let collision = fixture
            .directory
            .child(".bangbang-snapshot-rebase-00000000000000000000000000000000");
        fs::write(&collision, b"foreign").expect("collision entry should create");
        let outcome = with_staging_random_names(vec![[0_u8; 16], [1_u8; 16]], || {
            rebase_snapshot_v2_diff_paths(&fixture.paths).expect("second name should succeed")
        });
        assert_eq!(outcome.commit(), SnapshotV2DiffRebaseCommit::Durable);
        fixture.assert_committed(&outcome);
        assert_eq!(
            fs::read(&collision).expect("foreign collision should remain"),
            b"foreign"
        );
        assert_eq!(fixture.directory.staging_entries(), vec![collision]);
    }

    #[test]
    fn random_name_collisions_exhaust_at_the_fixed_bound() {
        let fixture = RebaseFixture::complete("random-collision-bound");
        let collision = fixture
            .directory
            .child(".bangbang-snapshot-rebase-00000000000000000000000000000000");
        fs::write(&collision, b"foreign").expect("collision entry should create");
        let error = with_staging_random_names(vec![[0_u8; 16]; 16], || {
            rebase_snapshot_v2_diff_paths(&fixture.paths)
                .expect_err("sixteen collisions should exhaust the bound")
        });
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::StagingCreate);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::Io {
                kind: io::ErrorKind::AlreadyExists
            }
        ));
        assert_eq!(error.staging_cleanup(), None);
        assert_eq!(
            fs::read(&collision).expect("collision should remain"),
            b"foreign"
        );
        assert_eq!(
            fs::read(fixture.paths.base()).expect("base should remain"),
            fixture.base_before
        );
        assert_eq!(
            fs::read(fixture.paths.diff()).expect("diff should remain"),
            fixture.diff_before
        );
        assert_eq!(fixture.directory.staging_entries(), vec![collision]);
    }

    #[test]
    fn hard_link_alias_is_rejected_before_materialization() {
        let fixture = RebaseFixture::complete("source-alias");
        for alias in [
            fixture.paths.base().to_path_buf(),
            fixture.directory.path.join(".").join("base.mem"),
        ] {
            let paths = SnapshotV2DiffRebasePaths::new(fixture.paths.base(), alias);
            let error = rebase_snapshot_v2_diff_paths(&paths)
                .expect_err("exact and syntactic aliases should be rejected");
            assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::SourceAliasCheck);
            assert!(matches!(
                error.failure(),
                SnapshotV2DiffRebaseFailure::SourceAlias
            ));
            assert_eq!(error.staging_cleanup(), None);
        }
        let original_diff = fixture.directory.child("original-next.diff");
        fs::rename(fixture.paths.diff(), &original_diff).expect("diff should move aside");
        fs::hard_link(fixture.paths.base(), fixture.paths.diff()).expect("alias should create");
        let error = rebase_snapshot_v2_diff_paths(&fixture.paths)
            .expect_err("source alias should be rejected");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::SourceAliasCheck);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::SourceAlias
        ));
        assert_eq!(error.staging_cleanup(), None);
        assert_eq!(
            fs::read(fixture.paths.base()).expect("base should remain"),
            fixture.base_before
        );
    }

    #[test]
    fn final_symlink_and_invalid_component_are_rejected() {
        let fixture = RebaseFixture::complete("invalid-paths");
        let link = fixture.directory.child("base-link.mem");
        std::os::unix::fs::symlink(fixture.paths.base(), &link)
            .expect("test symlink should create");
        let symlink_paths = SnapshotV2DiffRebasePaths::new(link, fixture.paths.diff());
        let symlink_error = rebase_snapshot_v2_diff_paths(&symlink_paths)
            .expect_err("final symlink should be rejected");
        assert_eq!(
            symlink_error.stage(),
            SnapshotV2DiffRebaseStage::BaseFileOpen
        );

        let invalid = SnapshotV2DiffRebasePaths::new("secret-base/..", fixture.paths.diff());
        let invalid_error = rebase_snapshot_v2_diff_paths(&invalid)
            .expect_err("invalid final component should be rejected");
        assert_eq!(
            invalid_error.stage(),
            SnapshotV2DiffRebaseStage::BasePathValidation
        );
        assert!(matches!(
            invalid_error.failure(),
            SnapshotV2DiffRebaseFailure::InvalidPath {
                input: SnapshotV2DiffRebaseInput::Base
            }
        ));
        assert!(!format!("{invalid_error:?}").contains("secret-base"));
    }

    #[test]
    fn path_validation_rejects_all_non_components_without_disclosure() {
        let fixture = RebaseFixture::complete("path-validation");
        let mut invalid = vec![
            PathBuf::from(""),
            PathBuf::from("/"),
            PathBuf::from("trailing/"),
            PathBuf::from("."),
            PathBuf::from(".."),
        ];
        invalid.push(PathBuf::from(std::ffi::OsString::from_vec(
            b"secret\0base".to_vec(),
        )));
        for base in invalid {
            let paths = SnapshotV2DiffRebasePaths::new(base, fixture.paths.diff());
            let error =
                rebase_snapshot_v2_diff_paths(&paths).expect_err("invalid base syntax should fail");
            assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::BasePathValidation);
            assert!(matches!(
                error.failure(),
                SnapshotV2DiffRebaseFailure::InvalidPath {
                    input: SnapshotV2DiffRebaseInput::Base
                }
            ));
            assert_eq!(error.staging_cleanup(), None);
            assert!(!format!("{error:?}").contains("secret"));
        }

        let paths = SnapshotV2DiffRebasePaths::new(fixture.paths.base(), "bad-diff/");
        let error = rebase_snapshot_v2_diff_paths(&paths)
            .expect_err("invalid diff syntax should fail after base adoption");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::DiffPathValidation);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::InvalidPath {
                input: SnapshotV2DiffRebaseInput::Diff
            }
        ));
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn formatted_error_source_chain_redacts_host_and_snapshot_values() {
        let fixture = RebaseFixture::complete("secret-host-directory");
        OpenOptions::new()
            .write(true)
            .open(fixture.paths.diff())
            .expect("diff should open for malformed diagnostic")
            .set_len(8)
            .expect("diff should truncate");
        let error =
            rebase_snapshot_v2_diff_paths(&fixture.paths).expect_err("malformed layer should fail");
        let mut diagnostics = format!("{error:?}\n{error}");
        let mut source = std::error::Error::source(&error);
        while let Some(current) = source {
            diagnostics.push_str(&format!("\n{current:?}\n{current}"));
            source = current.source();
        }
        let directory = fixture.directory.path.to_string_lossy().into_owned();
        let base = fixture.paths.base().to_string_lossy().into_owned();
        let diff = fixture.paths.diff().to_string_lossy().into_owned();
        for private in [
            directory.as_str(),
            base.as_str(),
            diff.as_str(),
            ".bangbang-snapshot-rebase-",
            "BANGM2A",
            "BANGD2A",
        ] {
            assert!(!diagnostics.contains(private));
        }
        assert!(!diagnostics.contains("secret-host-directory"));
        assert!(!diagnostics.contains("0xa7"));
    }

    #[test]
    fn missing_and_special_inputs_fail_before_staging_without_blocking() {
        let fixture = RebaseFixture::complete("special-inputs");

        let missing_parent = fixture.directory.child("missing/base.mem");
        let paths = SnapshotV2DiffRebasePaths::new(missing_parent, fixture.paths.diff());
        let error = rebase_snapshot_v2_diff_paths(&paths).expect_err("missing parent should fail");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::BaseDirectoryOpen);

        let missing_file = fixture.directory.child("missing.mem");
        let paths = SnapshotV2DiffRebasePaths::new(missing_file, fixture.paths.diff());
        let error = rebase_snapshot_v2_diff_paths(&paths).expect_err("missing file should fail");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::BaseFileOpen);

        let directory_source = fixture.directory.child("source-directory");
        fs::create_dir(&directory_source).expect("source directory should create");
        let paths = SnapshotV2DiffRebasePaths::new(&directory_source, fixture.paths.diff());
        let error = rebase_snapshot_v2_diff_paths(&paths)
            .expect_err("directory source should fail validation");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::BaseValidation);

        let fifo = fixture.directory.child("source-fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path should encode");
        // SAFETY: `fifo_name` is a live NUL-terminated path and the mode is valid.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let paths = SnapshotV2DiffRebasePaths::new(&fifo, fixture.paths.diff());
        let error = rebase_snapshot_v2_diff_paths(&paths)
            .expect_err("nonblocking FIFO adoption should reject the type");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::BaseValidation);

        let socket = fixture.directory.child("source.sock");
        let _listener = UnixListener::bind(&socket).expect("test socket should bind");
        let paths = SnapshotV2DiffRebasePaths::new(&socket, fixture.paths.diff());
        let error = rebase_snapshot_v2_diff_paths(&paths)
            .expect_err("socket source should fail without staging");
        assert!(matches!(
            error.stage(),
            SnapshotV2DiffRebaseStage::BaseFileOpen | SnapshotV2DiffRebaseStage::BaseValidation
        ));

        assert!(fixture.directory.staging_entries().is_empty());
        assert_eq!(
            fs::read(fixture.paths.base()).expect("base should remain"),
            fixture.base_before
        );
        assert_eq!(
            fs::read(fixture.paths.diff()).expect("diff should remain"),
            fixture.diff_before
        );
    }

    #[test]
    fn unwritable_base_parent_fails_before_staging_creation() {
        let fixture = RebaseFixture::complete("unwritable-parent");
        fs::set_permissions(&fixture.directory.path, fs::Permissions::from_mode(0o500))
            .expect("parent permissions should tighten");
        let result = rebase_snapshot_v2_diff_paths(&fixture.paths);
        fs::set_permissions(&fixture.directory.path, fs::Permissions::from_mode(0o700))
            .expect("parent permissions should restore");
        let error = result.expect_err("unwritable parent should reject staging");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::StagingCreate);
        assert_eq!(error.staging_cleanup(), None);
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn invalid_base_magic_and_layer_semantics_preserve_both_inputs() {
        let fixture = RebaseFixture::complete("invalid-base-magic");
        let base = OpenOptions::new()
            .write(true)
            .open(fixture.paths.base())
            .expect("base should open for corruption");
        base.write_all_at(b"NOTBASE!", 0)
            .expect("base magic should corrupt");
        let corrupted = fs::read(fixture.paths.base()).expect("corrupt base should read");
        let error = rebase_snapshot_v2_diff_paths(&fixture.paths)
            .expect_err("invalid base magic should fail");
        assert_eq!(error.stage(), SnapshotV2DiffRebaseStage::BaseValidation);
        assert!(matches!(
            error.failure(),
            SnapshotV2DiffRebaseFailure::InvalidBaseKind
        ));
        assert_eq!(
            fs::read(fixture.paths.base()).expect("corrupt base should remain"),
            corrupted
        );
        assert!(fixture.directory.staging_entries().is_empty());

        let fixture = RebaseFixture::complete("truncated-layer");
        OpenOptions::new()
            .write(true)
            .open(fixture.paths.diff())
            .expect("diff should open for truncation")
            .set_len(8)
            .expect("diff should truncate");
        let _ = expect_materialization_failure(&fixture.directory, &fixture.paths);

        let directory = TestDirectory::new("zero-root-next");
        let range = page_range(0, 4);
        let base_path = directory.child("base.mem");
        let diff_path = directory.child("next.diff");
        let base_memory = memory_with_byte(range, 0x10);
        let _base = write_complete(&base_path, &base_memory);
        let next_memory = memory_with_byte(range, 0x20);
        let _next = write_layer(&diff_path, &next_memory, SnapshotV2DiffBase::Zero, &[range]);
        let paths = SnapshotV2DiffRebasePaths::new(base_path, diff_path);
        let _ = expect_materialization_failure(&directory, &paths);

        let directory = TestDirectory::new("image-based-root");
        let predecessor_path = directory.child("predecessor.mem");
        let base_path = directory.child("base.diff");
        let diff_path = directory.child("next.diff");
        let predecessor_memory = memory_with_byte(range, 0x30);
        let predecessor = write_complete(&predecessor_path, &predecessor_memory);
        let base_memory = memory_with_byte(range, 0x40);
        let base_result = write_layer(
            &base_path,
            &base_memory,
            SnapshotV2DiffBase::Image(predecessor),
            &[range],
        );
        let next_memory = memory_with_byte(range, 0x50);
        let _next = write_layer(
            &diff_path,
            &next_memory,
            SnapshotV2DiffBase::Image(base_result),
            &[range],
        );
        let paths = SnapshotV2DiffRebasePaths::new(base_path, diff_path);
        let _ = expect_materialization_failure(&directory, &paths);
    }

    #[test]
    fn stale_lineage_and_missing_coverage_are_retry_safe() {
        let range = page_range(0, 4);
        let directory = TestDirectory::new("stale-lineage");
        let base_path = directory.child("base.mem");
        let unrelated_path = directory.child("unrelated.mem");
        let diff_path = directory.child("next.diff");
        let base = write_complete(&base_path, &memory_with_byte(range, 0x11));
        let unrelated = write_complete(&unrelated_path, &memory_with_byte(range, 0x22));
        assert_ne!(base.image_id(), unrelated.image_id());
        let _next = write_layer(
            &diff_path,
            &memory_with_byte(range, 0x33),
            SnapshotV2DiffBase::Image(unrelated),
            &[range],
        );
        let paths = SnapshotV2DiffRebasePaths::new(base_path, diff_path);
        let _ = expect_materialization_failure(&directory, &paths);

        let directory = TestDirectory::new("missing-coverage");
        let base_path = directory.child("base.mem");
        let diff_path = directory.child("next.diff");
        let inherited = page_range(0, 4);
        let added = page_range(8, 4);
        let base = write_complete(&base_path, &memory_with_byte(inherited, 0x44));
        let layout = GuestMemoryLayout::new(vec![inherited, added])
            .expect("expanded layout should validate");
        let mut target = GuestMemory::allocate(&layout).expect("expanded memory should allocate");
        for (range, byte) in [(inherited, 0x44), (added, 0x55)] {
            target
                .write_slice(
                    &vec![byte; usize::try_from(range.size()).expect("range should fit")],
                    range.start(),
                )
                .expect("expanded memory should populate");
        }
        let _next = write_layer(&diff_path, &target, SnapshotV2DiffBase::Image(base), &[]);
        let paths = SnapshotV2DiffRebasePaths::new(base_path, diff_path);
        let _ = expect_materialization_failure(&directory, &paths);
    }
}
