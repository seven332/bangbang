use super::*;

#[test]
fn paths_debug_redacts_both_values() {
    let paths = SnapshotStateEditPaths::new("sensitive-input", "sensitive-output");
    let debug = format!("{paths:?}");
    assert!(!debug.contains("sensitive-input"));
    assert!(!debug.contains("sensitive-output"));
    assert_eq!(paths.input(), Path::new("sensitive-input"));
    assert_eq!(paths.output(), Path::new("sensitive-output"));
}

#[cfg(not(unix))]
#[test]
fn unsupported_platform_rejects_before_callbacks() {
    let paths = SnapshotStateEditPaths::new("", "");
    let mut callbacks = 0;
    let error = publish_edited_snapshot_state_with_cancel(
        &paths,
        |_| {
            callbacks += 1;
            Ok::<_, ()>(())
        },
        |_| {
            callbacks += 1;
            Ok(Vec::new())
        },
        |_, _| {
            callbacks += 1;
            Ok(())
        },
        |_| {
            callbacks += 1;
            false
        },
    )
    .expect_err("unsupported target should reject");
    assert_eq!(callbacks, 0);
    let publication = error
        .publication()
        .expect("failure should be infrastructure");
    assert_eq!(publication.stage(), SnapshotStateEditStage::PlatformCheck);
    assert!(matches!(
        publication.failure(),
        SnapshotStateEditFailure::UnsupportedPlatform
    ));
}

#[cfg(unix)]
mod unix_tests {
    use std::ffi::CString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    use super::super::unix::{
        with_state_edit_action, with_state_edit_action_and_cleanup_failure,
        with_state_edit_cleanup_failure, with_state_edit_failure_and_cleanup_failure,
        with_state_edit_failures, with_state_edit_hard_link_failure,
        with_state_edit_random_failure, with_state_edit_random_names, with_state_edit_staging_mode,
    };
    use super::*;

    const SOURCE: &[u8] = b"bounded-source-state";
    const SUFFIX: &[u8] = b"-edited";
    const READ_STAGES: [SnapshotStateEditStage; 9] = [
        SnapshotStateEditStage::PlatformCheck,
        SnapshotStateEditStage::InputPathValidation,
        SnapshotStateEditStage::InputDirectoryOpen,
        SnapshotStateEditStage::InputFileOpen,
        SnapshotStateEditStage::InputValidation,
        SnapshotStateEditStage::InputRead,
        SnapshotStateEditStage::SourceStability,
        SnapshotStateEditStage::DirectoryStability,
        SnapshotStateEditStage::EntryStability,
    ];
    const PRECOMMIT_STAGES: [SnapshotStateEditStage; 23] = [
        SnapshotStateEditStage::PlatformCheck,
        SnapshotStateEditStage::InputPathValidation,
        SnapshotStateEditStage::InputDirectoryOpen,
        SnapshotStateEditStage::InputFileOpen,
        SnapshotStateEditStage::InputValidation,
        SnapshotStateEditStage::OutputPathValidation,
        SnapshotStateEditStage::OutputDirectoryOpen,
        SnapshotStateEditStage::AliasCheck,
        SnapshotStateEditStage::OutputPreflight,
        SnapshotStateEditStage::InputRead,
        SnapshotStateEditStage::Transform,
        SnapshotStateEditStage::Encode,
        SnapshotStateEditStage::StagingCreate,
        SnapshotStateEditStage::StagingWrite,
        SnapshotStateEditStage::StagingFlush,
        SnapshotStateEditStage::StagingFileSync,
        SnapshotStateEditStage::StagingSeek,
        SnapshotStateEditStage::StagingRead,
        SnapshotStateEditStage::StagingVerify,
        SnapshotStateEditStage::SourceStability,
        SnapshotStateEditStage::DirectoryStability,
        SnapshotStateEditStage::EntryStability,
        SnapshotStateEditStage::Commit,
    ];

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bb-state-edit-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should create");
            Self { path }
        }

        fn child(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        fn staging_entries(&self) -> Vec<PathBuf> {
            staging_entries(&self.path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ToyProduct {
        bytes: Vec<u8>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ToyError(&'static str);

    impl fmt::Display for ToyError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl std::error::Error for ToyError {}

    type TestFileFacts = (u64, u64, u32, u64, i64, i64, i64, i64);

    struct EditFixture {
        directory: TestDirectory,
        paths: SnapshotStateEditPaths,
        source_facts: TestFileFacts,
    }

    impl EditFixture {
        fn new(label: &str) -> Self {
            let directory = TestDirectory::new(label);
            let input = directory.child("input.state");
            let output = directory.child("output.state");
            fs::write(&input, SOURCE).expect("source should write");
            let source_facts = test_file_facts(&input);
            Self {
                directory,
                paths: SnapshotStateEditPaths::new(input, output),
                source_facts,
            }
        }

        fn expected(&self) -> Vec<u8> {
            let mut expected = SOURCE.to_vec();
            expected.extend_from_slice(SUFFIX);
            expected
        }

        fn assert_source_unchanged(&self) {
            assert_eq!(
                fs::read(self.paths.input()).expect("source should read"),
                SOURCE
            );
            assert_eq!(test_file_facts(self.paths.input()), self.source_facts);
        }

        fn assert_uncommitted_and_clean(&self) {
            self.assert_source_unchanged();
            assert!(!self.paths.output().exists());
            assert!(self.directory.staging_entries().is_empty());
        }

        fn assert_durable(&self, outcome: &SnapshotStateEditOutcome<ToyProduct>) {
            assert_eq!(outcome.commit(), SnapshotStateEditCommit::Durable);
            assert_eq!(outcome.product().bytes, self.expected());
            assert_eq!(
                fs::read(self.paths.output()).expect("output should read"),
                self.expected()
            );
            assert_eq!(
                fs::metadata(self.paths.output())
                    .expect("output metadata should read")
                    .mode()
                    & 0o7777,
                0o600
            );
            self.assert_source_unchanged();
            assert!(self.directory.staging_entries().is_empty());
        }
    }

    fn test_file_facts(path: &Path) -> TestFileFacts {
        let metadata = fs::metadata(path).expect("file facts should read");
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

    fn publish_toy(
        paths: &SnapshotStateEditPaths,
    ) -> Result<SnapshotStateEditOutcome<ToyProduct>, SnapshotStateEditTransactionError<ToyError>>
    {
        publish_edited_snapshot_state_with(
            paths,
            |input| {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(input.len() + SUFFIX.len())
                    .map_err(|_| ToyError("secret transform allocation"))?;
                bytes.extend_from_slice(input);
                bytes.extend_from_slice(SUFFIX);
                Ok(ToyProduct { bytes })
            },
            |product| Ok(product.bytes.clone()),
            |staged, product| {
                if staged == product.bytes {
                    Ok(())
                } else {
                    Err(ToyError("secret semantic mismatch"))
                }
            },
        )
    }

    fn staging_entries(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .expect("directory should enumerate")
            .map(|entry| entry.expect("entry should read"))
            .filter(|entry| {
                entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b".bangbang-snapshot-edit-")
            })
            .map(|entry| entry.path())
            .collect()
    }

    fn one_staging(directory: &Path) -> PathBuf {
        let entries = staging_entries(directory);
        assert_eq!(entries.len(), 1);
        entries
            .into_iter()
            .next()
            .expect("one staging entry should exist")
    }

    fn create_fifo(path: &Path) {
        let path = CString::new(path.as_os_str().as_bytes()).expect("FIFO path should encode");
        // SAFETY: the CString is NUL-terminated and the test owns the absent path.
        let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "FIFO fixture should create");
    }

    fn staging_name(random: [u8; 16]) -> String {
        let mut name = String::from(".bangbang-snapshot-edit-");
        for byte in random {
            use std::fmt::Write as _;
            write!(&mut name, "{byte:02x}").expect("staging name should format");
        }
        name
    }

    #[test]
    fn read_only_capture_is_exact_and_uses_only_input_stages() {
        let fixture = EditFixture::new("read-exact");
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let callback_order = std::rc::Rc::clone(&order);
        let bytes = read_snapshot_state_file_with_cancel(fixture.paths.input(), move |stage| {
            callback_order.borrow_mut().push(stage);
            false
        })
        .expect("read-only capture should succeed");

        assert_eq!(bytes, SOURCE);
        assert_eq!(&*order.borrow(), &READ_STAGES);
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn every_read_stage_can_cancel_without_staging_or_output() {
        for target in READ_STAGES {
            let fixture = EditFixture::new(&format!("read-cancel-{target:?}"));
            let error = read_snapshot_state_file_with_cancel(fixture.paths.input(), |stage| {
                stage == target
            })
            .expect_err("target read checkpoint should cancel");
            assert_eq!(error.stage(), target);
            assert!(matches!(
                error.failure(),
                SnapshotStateEditFailure::Cancelled
            ));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn every_read_stage_failure_is_typed_and_redacted() {
        for target in READ_STAGES {
            let fixture = EditFixture::new(&format!("secret-read-failure-{target:?}"));
            let error = with_state_edit_failures([(target, io::ErrorKind::Other)], || {
                read_snapshot_state_file(fixture.paths.input())
                    .expect_err("target read stage should fail")
            });
            assert_eq!(error.stage(), target);
            assert!(matches!(
                error.failure(),
                SnapshotStateEditFailure::Io {
                    kind: io::ErrorKind::Other
                }
            ));
            let rendered = format!("{error:?} / {error}");
            assert!(!rendered.contains("secret-read-failure"));
            assert!(!rendered.contains(SOURCE.escape_ascii().to_string().as_str()));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn read_only_capture_rejects_invalid_special_and_oversized_inputs() {
        let directory = TestDirectory::new("read-input-policy");

        let invalid =
            read_snapshot_state_file(Path::new("")).expect_err("empty input path should reject");
        assert_eq!(invalid.stage(), SnapshotStateEditStage::InputPathValidation);
        assert!(matches!(
            invalid.failure(),
            SnapshotStateEditFailure::InvalidPath {
                path: SnapshotStateEditPathRole::Input
            }
        ));

        let missing = directory.child("missing.state");
        let missing = read_snapshot_state_file(&missing).expect_err("missing input should reject");
        assert_eq!(missing.stage(), SnapshotStateEditStage::InputFileOpen);

        let target = directory.child("target.state");
        fs::write(&target, SOURCE).expect("symlink target should write");
        let link = directory.child("link.state");
        symlink(&target, &link).expect("input symlink should create");
        let link = read_snapshot_state_file(&link).expect_err("final symlink should reject");
        assert_eq!(link.stage(), SnapshotStateEditStage::InputFileOpen);

        let child_directory = directory.child("directory.state");
        fs::create_dir(&child_directory).expect("input directory should create");
        let child_directory =
            read_snapshot_state_file(&child_directory).expect_err("directory input should reject");
        assert_eq!(
            child_directory.stage(),
            SnapshotStateEditStage::InputValidation
        );
        assert!(matches!(
            child_directory.failure(),
            SnapshotStateEditFailure::InvalidInput
        ));

        let fifo = directory.child("fifo.state");
        create_fifo(&fifo);
        let fifo = read_snapshot_state_file(&fifo).expect_err("FIFO input should reject");
        assert_eq!(fifo.stage(), SnapshotStateEditStage::InputValidation);

        let socket = directory.child("socket.state");
        let _listener = UnixListener::bind(&socket).expect("input socket should bind");
        let socket = read_snapshot_state_file(&socket).expect_err("socket input should reject");
        assert!(matches!(
            socket.stage(),
            SnapshotStateEditStage::InputFileOpen | SnapshotStateEditStage::InputValidation
        ));

        let oversized = directory.child("oversized.state");
        let file = File::create(&oversized).expect("oversized input should create");
        file.set_len(u64::try_from(SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES).unwrap() + 1)
            .expect("oversized input should size");
        let oversized =
            read_snapshot_state_file(&oversized).expect_err("oversized input should reject");
        assert_eq!(oversized.stage(), SnapshotStateEditStage::InputValidation);
        assert!(matches!(
            oversized.failure(),
            SnapshotStateEditFailure::InputTooLarge {
                maximum: SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES
            }
        ));
    }

    #[test]
    fn read_only_capture_detects_source_entry_and_parent_replacement() {
        let source_fixture = EditFixture::new("read-source-change");
        let source = source_fixture.paths.input().to_path_buf();
        let error = with_state_edit_action(
            SnapshotStateEditStage::EntryStability,
            move || fs::write(&source, b"changed-source-state!").expect("source should mutate"),
            || {
                read_snapshot_state_file(source_fixture.paths.input())
                    .expect_err("source mutation should reject")
            },
        );
        assert_eq!(error.stage(), SnapshotStateEditStage::EntryStability);
        assert!(matches!(
            error.failure(),
            SnapshotStateEditFailure::SourceChanged
        ));
        assert!(source_fixture.directory.staging_entries().is_empty());
        assert!(!source_fixture.paths.output().exists());

        let entry_fixture = EditFixture::new("read-entry-change");
        let source = entry_fixture.paths.input().to_path_buf();
        let moved = entry_fixture.directory.child("retained.state");
        let action_source = source.clone();
        let error = with_state_edit_action(
            SnapshotStateEditStage::EntryStability,
            move || {
                fs::rename(&action_source, &moved).expect("input should move");
                fs::write(&action_source, b"foreign-input-state")
                    .expect("foreign input should replace");
            },
            || read_snapshot_state_file(&source).expect_err("entry replacement should reject"),
        );
        assert_eq!(error.stage(), SnapshotStateEditStage::EntryStability);
        assert!(matches!(
            error.failure(),
            SnapshotStateEditFailure::EntryChanged {
                path: SnapshotStateEditPathRole::Input
            }
        ));

        let root = TestDirectory::new("read-parent-change");
        let parent = root.child("parent");
        let moved_parent = root.child("retained-parent");
        fs::create_dir(&parent).expect("input parent should create");
        let input = parent.join("input.state");
        fs::write(&input, SOURCE).expect("parent fixture source should write");
        let action_parent = parent.clone();
        let error = with_state_edit_action(
            SnapshotStateEditStage::DirectoryStability,
            move || {
                fs::rename(&action_parent, &moved_parent).expect("input parent should move");
                fs::create_dir(&action_parent).expect("foreign parent should replace");
                fs::write(action_parent.join("input.state"), b"foreign-parent-state")
                    .expect("foreign parent input should write");
            },
            || read_snapshot_state_file(&input).expect_err("parent replacement should reject"),
        );
        assert_eq!(error.stage(), SnapshotStateEditStage::DirectoryStability);
        assert!(matches!(
            error.failure(),
            SnapshotStateEditFailure::DirectoryChanged {
                path: SnapshotStateEditPathRole::Input
            }
        ));
    }

    #[test]
    fn same_and_cross_directory_edits_commit_durably_after_all_callbacks() {
        for cross_directory in [false, true] {
            let source = TestDirectory::new(if cross_directory {
                "cross-source"
            } else {
                "same"
            });
            let destination = cross_directory.then(|| TestDirectory::new("cross-output"));
            let input = source.child("input.state");
            fs::write(&input, SOURCE).expect("source should write");
            let source_facts = test_file_facts(&input);
            let output = destination.as_ref().map_or_else(
                || source.child("output.state"),
                |root| root.child("output.state"),
            );
            let paths = SnapshotStateEditPaths::new(input, output);
            let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let transform_order = std::rc::Rc::clone(&order);
            let encode_order = std::rc::Rc::clone(&order);
            let verify_order = std::rc::Rc::clone(&order);
            let commit_order = std::rc::Rc::clone(&order);
            let outcome = publish_edited_snapshot_state_with_cancel(
                &paths,
                move |input| {
                    transform_order.borrow_mut().push("transform");
                    let mut bytes = input.to_vec();
                    bytes.extend_from_slice(SUFFIX);
                    Ok::<_, ToyError>(ToyProduct { bytes })
                },
                move |product| {
                    encode_order.borrow_mut().push("encode");
                    Ok(product.bytes.clone())
                },
                move |staged, product| {
                    verify_order.borrow_mut().push("verify");
                    assert_eq!(staged, product.bytes);
                    Ok(())
                },
                move |stage| {
                    if stage == SnapshotStateEditStage::Commit {
                        assert_eq!(&*commit_order.borrow(), &["transform", "encode", "verify"]);
                    }
                    false
                },
            )
            .expect("edit should commit");
            assert_eq!(outcome.commit(), SnapshotStateEditCommit::Durable);
            assert_eq!(fs::read(paths.input()).expect("input should read"), SOURCE);
            assert_eq!(test_file_facts(paths.input()), source_facts);
            assert_eq!(
                fs::read(paths.output()).expect("output should read"),
                outcome.product().bytes
            );
            assert_eq!(
                fs::metadata(paths.output())
                    .expect("output mode should read")
                    .mode()
                    & 0o7777,
                0o600
            );
            let output_directory = paths.output().parent().expect("output should have parent");
            assert!(staging_entries(output_directory).is_empty());
        }
    }

    #[test]
    fn every_precommit_stage_can_cancel_without_output() {
        for target in PRECOMMIT_STAGES {
            let fixture = EditFixture::new(&format!("cancel-{target:?}"));
            let error = publish_edited_snapshot_state_with_cancel(
                &fixture.paths,
                |input| {
                    let mut bytes = input.to_vec();
                    bytes.extend_from_slice(SUFFIX);
                    Ok::<_, ToyError>(ToyProduct { bytes })
                },
                |product| Ok(product.bytes.clone()),
                |staged, product| {
                    assert_eq!(staged, product.bytes);
                    Ok(())
                },
                |stage| stage == target,
            )
            .expect_err("target checkpoint should cancel");
            let publication = error
                .publication()
                .expect("cancel should be infrastructure");
            assert_eq!(publication.stage(), target);
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::Cancelled
            ));
            if PRECOMMIT_STAGES
                .iter()
                .position(|stage| *stage == target)
                .expect("stage should be listed")
                > PRECOMMIT_STAGES
                    .iter()
                    .position(|stage| *stage == SnapshotStateEditStage::StagingCreate)
                    .expect("staging stage should be listed")
            {
                assert_eq!(
                    publication.staging_cleanup(),
                    Some(SnapshotStateEditCleanup::Removed)
                );
            } else {
                assert_eq!(publication.staging_cleanup(), None);
            }
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn every_precommit_stage_failure_preserves_primary_error_and_input() {
        for target in PRECOMMIT_STAGES {
            let fixture = EditFixture::new(&format!("failure-{target:?}"));
            let error = with_state_edit_failures([(target, io::ErrorKind::Other)], || {
                publish_toy(&fixture.paths).expect_err("target stage should fail")
            });
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert_eq!(publication.stage(), target);
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::Io {
                    kind: io::ErrorKind::Other
                }
            ));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn typed_callback_failures_are_redacted_and_clean_staging() {
        let fixture = EditFixture::new("typed-errors");
        for (target, expected_cleanup) in [
            (SnapshotStateEditStage::Transform, None),
            (SnapshotStateEditStage::Encode, None),
            (
                SnapshotStateEditStage::StagingVerify,
                Some(SnapshotStateEditCleanup::Removed),
            ),
        ] {
            let error = publish_edited_snapshot_state_with(
                &fixture.paths,
                |input| {
                    if target == SnapshotStateEditStage::Transform {
                        return Err(ToyError("sensitive-transform-value"));
                    }
                    Ok(ToyProduct {
                        bytes: input.to_vec(),
                    })
                },
                |product| {
                    if target == SnapshotStateEditStage::Encode {
                        return Err(ToyError("sensitive-encode-value"));
                    }
                    Ok(product.bytes.clone())
                },
                |_, _| {
                    if target == SnapshotStateEditStage::StagingVerify {
                        return Err(ToyError("sensitive-verify-value"));
                    }
                    Ok(())
                },
            )
            .expect_err("selected callback should fail");
            let operation = error.operation().expect("failure should remain typed");
            assert_eq!(operation.stage(), target);
            assert_eq!(operation.staging_cleanup(), expected_cleanup);
            assert!(operation.source().0.starts_with("sensitive-"));
            let rendered = format!("{error:?} / {error}");
            assert!(!rendered.contains("sensitive"));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn verifier_panic_unwinds_only_owned_staging() {
        let fixture = EditFixture::new("panic");
        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _ = publish_edited_snapshot_state_with(
                &fixture.paths,
                |input| {
                    Ok::<_, ToyError>(ToyProduct {
                        bytes: input.to_vec(),
                    })
                },
                |product| Ok(product.bytes.clone()),
                |_, _| -> Result<(), ToyError> { panic!("sensitive verifier panic") },
            );
        }));
        assert!(unwind.is_err());
        fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn invalid_special_and_oversized_inputs_fail_without_blocking_or_staging() {
        enum InputKind {
            Missing,
            Oversized,
            Directory,
            Symlink,
            Fifo,
            Socket,
            Unreadable,
        }

        for (label, kind) in [
            ("missing", InputKind::Missing),
            ("oversized", InputKind::Oversized),
            ("directory", InputKind::Directory),
            ("symlink", InputKind::Symlink),
            ("fifo", InputKind::Fifo),
            ("socket", InputKind::Socket),
            ("unreadable", InputKind::Unreadable),
        ] {
            let directory = TestDirectory::new(label);
            let input = directory.child("input");
            let output = directory.child("output");
            let mut listener = None;
            match kind {
                InputKind::Missing => {}
                InputKind::Oversized => {
                    let file = File::create(&input).expect("oversized fixture should create");
                    file.set_len(
                        u64::try_from(SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES)
                            .expect("limit should fit")
                            + 1,
                    )
                    .expect("oversized sparse fixture should extend");
                }
                InputKind::Directory => fs::create_dir(&input).expect("directory should create"),
                InputKind::Symlink => {
                    let target = directory.child("target");
                    fs::write(&target, SOURCE).expect("target should write");
                    symlink(&target, &input).expect("symlink should create");
                }
                InputKind::Fifo => create_fifo(&input),
                InputKind::Socket => {
                    listener = Some(UnixListener::bind(&input).expect("socket should bind"));
                }
                InputKind::Unreadable => {
                    fs::write(&input, SOURCE).expect("unreadable input should write");
                    fs::set_permissions(&input, fs::Permissions::from_mode(0o000))
                        .expect("permissions should change");
                }
            }
            let paths = SnapshotStateEditPaths::new(&input, &output);
            let error = publish_toy(&paths).expect_err("invalid input should fail");
            assert!(error.publication().is_some());
            assert!(!output.exists());
            assert!(directory.staging_entries().is_empty());
            if matches!(kind, InputKind::Unreadable) {
                fs::set_permissions(&input, fs::Permissions::from_mode(0o600))
                    .expect("permissions should restore");
            }
            drop(listener);
        }
    }

    #[test]
    fn invalid_components_and_special_output_collisions_are_rejected() {
        let directory = TestDirectory::new("paths");
        let input = directory.child("input");
        fs::write(&input, SOURCE).expect("input should write");
        let invalid = [
            PathBuf::new(),
            PathBuf::from("."),
            PathBuf::from(".."),
            directory.path.join("trailing/"),
            PathBuf::from(std::ffi::OsString::from_vec(b"bad\0component".to_vec())),
        ];
        for output in invalid {
            let paths = SnapshotStateEditPaths::new(&input, output);
            let error = publish_toy(&paths).expect_err("invalid output should fail");
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert_eq!(
                publication.stage(),
                SnapshotStateEditStage::OutputPathValidation
            );
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::InvalidPath {
                    path: SnapshotStateEditPathRole::Output
                }
            ));
        }

        enum OutputKind {
            Regular,
            Directory,
            Symlink,
            Fifo,
            Socket,
        }
        for (label, kind) in [
            ("regular", OutputKind::Regular),
            ("directory", OutputKind::Directory),
            ("symlink", OutputKind::Symlink),
            ("fifo", OutputKind::Fifo),
            ("socket", OutputKind::Socket),
        ] {
            let root = TestDirectory::new(&format!("output-{label}"));
            let source = root.child("input");
            let output = root.child("output");
            fs::write(&source, SOURCE).expect("source should write");
            let mut listener = None;
            match kind {
                OutputKind::Regular => fs::write(&output, b"foreign").expect("output should write"),
                OutputKind::Directory => fs::create_dir(&output).expect("output dir should create"),
                OutputKind::Symlink => {
                    symlink(&source, &output).expect("output symlink should create")
                }
                OutputKind::Fifo => create_fifo(&output),
                OutputKind::Socket => {
                    listener =
                        Some(UnixListener::bind(&output).expect("output socket should bind"));
                }
            }
            let paths = SnapshotStateEditPaths::new(&source, &output);
            let error = publish_toy(&paths).expect_err("existing output should fail");
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::OutputAlreadyExists
                    | SnapshotStateEditFailure::InputOutputAlias
            ));
            assert!(root.staging_entries().is_empty());
            drop(listener);
        }
    }

    #[test]
    fn exact_resolved_and_hard_link_aliases_fail_before_staging() {
        let fixture = EditFixture::new("aliases");
        for paths in [
            SnapshotStateEditPaths::new(fixture.paths.input(), fixture.paths.input()),
            {
                let alias_parent = fixture.directory.child("alias-parent");
                symlink(&fixture.directory.path, &alias_parent)
                    .expect("parent alias should create");
                SnapshotStateEditPaths::new(fixture.paths.input(), alias_parent.join("input.state"))
            },
            {
                let output = fixture.directory.child("hard-link-output");
                fs::hard_link(fixture.paths.input(), &output).expect("hard link should create");
                SnapshotStateEditPaths::new(fixture.paths.input(), output)
            },
        ] {
            let error = publish_toy(&paths).expect_err("alias should reject");
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::InputOutputAlias
            ));
        }
        assert!(fixture.directory.staging_entries().is_empty());
    }

    #[test]
    fn random_failure_collision_retry_exhaustion_and_mode_fixup_are_bounded() {
        let failure_fixture = EditFixture::new("random-failure");
        let error = with_state_edit_random_failure(|| {
            publish_toy(&failure_fixture.paths).expect_err("random failure should reject")
        });
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::RandomnessUnavailable
        ));
        failure_fixture.assert_uncommitted_and_clean();

        let retry_fixture = EditFixture::new("random-retry");
        let collision = [0x11; 16];
        let winner = [0x22; 16];
        let collision_path = retry_fixture.directory.child(&staging_name(collision));
        fs::write(&collision_path, b"foreign").expect("collision should create");
        let outcome = with_state_edit_random_names([collision, winner], || {
            publish_toy(&retry_fixture.paths).expect("second random name should win")
        });
        assert_eq!(outcome.commit(), SnapshotStateEditCommit::Durable);
        assert_eq!(
            fs::read(&collision_path).expect("collision should remain"),
            b"foreign"
        );

        let exhausted_fixture = EditFixture::new("random-exhausted");
        let repeated = [0x33; 16];
        fs::write(
            exhausted_fixture.directory.child(&staging_name(repeated)),
            b"foreign",
        )
        .expect("collision should create");
        let error = with_state_edit_random_names(std::iter::repeat_n(repeated, 16), || {
            publish_toy(&exhausted_fixture.paths).expect_err("collisions should exhaust")
        });
        assert!(matches!(
            error
                .publication()
                .expect("failure should be infrastructure")
                .failure(),
            SnapshotStateEditFailure::StagingNameExhausted
        ));
        assert!(!exhausted_fixture.paths.output().exists());

        let mode_fixture = EditFixture::new("mode-fixup");
        let outcome = with_state_edit_staging_mode(0o000, || {
            publish_toy(&mode_fixture.paths).expect("explicit chmod should normalize mode")
        });
        mode_fixture.assert_durable(&outcome);
    }

    #[test]
    fn write_flush_sync_seek_read_and_verify_failures_clean_staging() {
        for stage in [
            SnapshotStateEditStage::StagingWrite,
            SnapshotStateEditStage::StagingFlush,
            SnapshotStateEditStage::StagingFileSync,
            SnapshotStateEditStage::StagingSeek,
            SnapshotStateEditStage::StagingRead,
            SnapshotStateEditStage::StagingVerify,
        ] {
            let fixture = EditFixture::new(&format!("staging-{stage:?}"));
            let error = with_state_edit_failures([(stage, io::ErrorKind::Other)], || {
                publish_toy(&fixture.paths).expect_err("injected staging stage should fail")
            });
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert_eq!(publication.stage(), stage);
            assert_eq!(
                publication.staging_cleanup(),
                Some(SnapshotStateEditCleanup::Removed)
            );
            fixture.assert_uncommitted_and_clean();
        }

        let mismatch_fixture = EditFixture::new("staging-mismatch");
        let directory = mismatch_fixture.directory.path.clone();
        let error = with_state_edit_action(
            SnapshotStateEditStage::StagingRead,
            move || {
                let staging = one_staging(&directory);
                fs::write(staging, b"changed").expect("staging should corrupt");
            },
            || publish_toy(&mismatch_fixture.paths).expect_err("corruption should reject"),
        );
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert_eq!(publication.stage(), SnapshotStateEditStage::StagingRead);
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::StagingContentMismatch | SnapshotStateEditFailure::Io { .. }
        ));
        mismatch_fixture.assert_uncommitted_and_clean();
    }

    #[test]
    fn final_callback_detects_source_output_parent_and_staging_mutations() {
        let source_fixture = EditFixture::new("source-mutation");
        let source = source_fixture.paths.input().to_path_buf();
        let error = publish_edited_snapshot_state_with_cancel(
            &source_fixture.paths,
            |input| {
                let mut bytes = input.to_vec();
                bytes.extend_from_slice(SUFFIX);
                Ok::<_, ToyError>(ToyProduct { bytes })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    fs::write(&source, b"changed-source-state").expect("source should mutate");
                }
                false
            },
        )
        .expect_err("source mutation should reject");
        assert!(matches!(
            error
                .publication()
                .expect("failure should be infrastructure")
                .failure(),
            SnapshotStateEditFailure::SourceChanged
        ));
        assert!(!source_fixture.paths.output().exists());
        assert!(source_fixture.directory.staging_entries().is_empty());

        let output_fixture = EditFixture::new("output-race");
        let output = output_fixture.paths.output().to_path_buf();
        let error = publish_edited_snapshot_state_with_cancel(
            &output_fixture.paths,
            |input| {
                Ok::<_, ToyError>(ToyProduct {
                    bytes: input.to_vec(),
                })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    fs::write(&output, b"foreign-winner").expect("foreign output should win");
                }
                false
            },
        )
        .expect_err("late output should reject");
        assert!(matches!(
            error
                .publication()
                .expect("failure should be infrastructure")
                .failure(),
            SnapshotStateEditFailure::OutputAlreadyExists
        ));
        assert_eq!(
            fs::read(output_fixture.paths.output()).expect("foreign output should remain"),
            b"foreign-winner"
        );
        assert!(output_fixture.directory.staging_entries().is_empty());

        let staging_fixture = EditFixture::new("staging-replacement");
        let directory = staging_fixture.directory.path.clone();
        let error = publish_edited_snapshot_state_with_cancel(
            &staging_fixture.paths,
            |input| {
                Ok::<_, ToyError>(ToyProduct {
                    bytes: input.to_vec(),
                })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    let staging = one_staging(&directory);
                    fs::remove_file(&staging).expect("owned staging should remove");
                    fs::write(&staging, b"foreign-staging")
                        .expect("foreign staging should replace");
                }
                false
            },
        )
        .expect_err("staging replacement should reject");
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::StagingChanged
        ));
        assert_eq!(
            publication.staging_cleanup(),
            Some(SnapshotStateEditCleanup::ChangedRefused)
        );
        assert!(!staging_fixture.paths.output().exists());
        assert_eq!(staging_fixture.directory.staging_entries().len(), 1);

        let parent_root = TestDirectory::new("parent-replacement");
        let input_parent = parent_root.child("input-parent");
        fs::create_dir(&input_parent).expect("input parent should create");
        let input = input_parent.join("input.state");
        fs::write(&input, SOURCE).expect("input should write");
        let output = parent_root.child("output.state");
        let moved = parent_root.child("moved-parent");
        let input_parent_for_action = input_parent.clone();
        let paths = SnapshotStateEditPaths::new(&input, &output);
        let error = publish_edited_snapshot_state_with_cancel(
            &paths,
            |input| {
                Ok::<_, ToyError>(ToyProduct {
                    bytes: input.to_vec(),
                })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    fs::rename(&input_parent_for_action, &moved).expect("input parent should move");
                    fs::create_dir(&input_parent_for_action)
                        .expect("replacement parent should create");
                }
                false
            },
        )
        .expect_err("parent replacement should reject");
        assert!(matches!(
            error
                .publication()
                .expect("failure should be infrastructure")
                .failure(),
            SnapshotStateEditFailure::DirectoryChanged {
                path: SnapshotStateEditPathRole::Input
            }
        ));
        assert!(!output.exists());
        assert!(staging_entries(&parent_root.path).is_empty());
    }

    #[test]
    fn final_callback_detects_input_name_output_parent_and_staging_removal() {
        let input_fixture = EditFixture::new("input-name-replacement");
        let input = input_fixture.paths.input().to_path_buf();
        let displaced = input_fixture.directory.child("displaced-input");
        let error = publish_edited_snapshot_state_with_cancel(
            &input_fixture.paths,
            |bytes| {
                Ok::<_, ToyError>(ToyProduct {
                    bytes: bytes.to_vec(),
                })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    fs::rename(&input, &displaced).expect("input name should move");
                    fs::write(&input, b"foreign-input-name")
                        .expect("foreign input name should replace");
                }
                false
            },
        )
        .expect_err("input-name replacement should reject");
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::EntryChanged {
                path: SnapshotStateEditPathRole::Input
            } | SnapshotStateEditFailure::SourceChanged
        ));
        assert_eq!(
            publication.staging_cleanup(),
            Some(SnapshotStateEditCleanup::Removed)
        );
        assert!(!input_fixture.paths.output().exists());
        assert!(input_fixture.directory.staging_entries().is_empty());

        let parent_root = TestDirectory::new("output-parent-replacement");
        let input = parent_root.child("input.state");
        fs::write(&input, SOURCE).expect("input should write");
        let output_parent = parent_root.child("output-parent");
        fs::create_dir(&output_parent).expect("output parent should create");
        let output = output_parent.join("output.state");
        let moved = parent_root.child("moved-output-parent");
        let output_parent_for_action = output_parent.clone();
        let paths = SnapshotStateEditPaths::new(&input, &output);
        let error = publish_edited_snapshot_state_with_cancel(
            &paths,
            |bytes| {
                Ok::<_, ToyError>(ToyProduct {
                    bytes: bytes.to_vec(),
                })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    fs::rename(&output_parent_for_action, &moved)
                        .expect("output parent should move");
                    fs::create_dir(&output_parent_for_action)
                        .expect("replacement output parent should create");
                }
                false
            },
        )
        .expect_err("output-parent replacement should reject");
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::DirectoryChanged {
                path: SnapshotStateEditPathRole::Output
            }
        ));
        assert_eq!(
            publication.staging_cleanup(),
            Some(SnapshotStateEditCleanup::Removed)
        );
        assert!(!output.exists());
        assert!(staging_entries(&parent_root.path).is_empty());
        assert!(staging_entries(&parent_root.child("moved-output-parent")).is_empty());

        let staging_fixture = EditFixture::new("staging-removal");
        let staging_directory = staging_fixture.directory.path.clone();
        let error = publish_edited_snapshot_state_with_cancel(
            &staging_fixture.paths,
            |bytes| {
                Ok::<_, ToyError>(ToyProduct {
                    bytes: bytes.to_vec(),
                })
            },
            |product| Ok(product.bytes.clone()),
            |_, _| Ok(()),
            move |stage| {
                if stage == SnapshotStateEditStage::Commit {
                    fs::remove_file(one_staging(&staging_directory))
                        .expect("staging should remove");
                }
                false
            },
        )
        .expect_err("staging removal should reject");
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::StagingChanged
        ));
        assert_eq!(
            publication.staging_cleanup(),
            Some(SnapshotStateEditCleanup::AlreadyAbsent)
        );
        assert!(!staging_fixture.paths.output().exists());
        assert!(staging_fixture.directory.staging_entries().is_empty());
    }

    #[test]
    fn cleanup_failure_preserves_primary_error_and_foreign_names() {
        let failure_fixture = EditFixture::new("cleanup-failure");
        let error = with_state_edit_failure_and_cleanup_failure(
            SnapshotStateEditStage::StagingWrite,
            io::ErrorKind::WriteZero,
            io::ErrorKind::PermissionDenied,
            || publish_toy(&failure_fixture.paths).expect_err("write should fail"),
        );
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert_eq!(publication.stage(), SnapshotStateEditStage::StagingWrite);
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::Io {
                kind: io::ErrorKind::WriteZero
            }
        ));
        assert_eq!(
            publication.staging_cleanup(),
            Some(SnapshotStateEditCleanup::Failed(
                io::ErrorKind::PermissionDenied
            ))
        );
        assert!(!failure_fixture.paths.output().exists());
        assert_eq!(failure_fixture.directory.staging_entries().len(), 1);
    }

    #[test]
    fn hard_link_failure_is_precommit_and_fresh_retry_succeeds() {
        let fixture = EditFixture::new("link-failure");
        let error = with_state_edit_hard_link_failure(io::ErrorKind::Unsupported, || {
            publish_toy(&fixture.paths).expect_err("unsupported hard link should reject")
        });
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert_eq!(publication.stage(), SnapshotStateEditStage::Commit);
        assert!(matches!(
            publication.failure(),
            SnapshotStateEditFailure::HardLinkUnavailable {
                kind: io::ErrorKind::Unsupported
            }
        ));
        assert_eq!(
            publication.staging_cleanup(),
            Some(SnapshotStateEditCleanup::Removed)
        );
        fixture.assert_uncommitted_and_clean();

        let outcome = publish_toy(&fixture.paths).expect("fresh retry should commit");
        fixture.assert_durable(&outcome);
    }

    #[test]
    fn committed_cleanup_and_directory_sync_failures_never_rollback_output() {
        let cleanup_fixture = EditFixture::new("postcommit-cleanup");
        let outcome = with_state_edit_cleanup_failure(io::ErrorKind::PermissionDenied, || {
            publish_toy(&cleanup_fixture.paths).expect("commit should return an outcome")
        });
        assert!(matches!(
            outcome.commit(),
            SnapshotStateEditCommit::Uncertain {
                stage: SnapshotStateEditStage::StagingCleanup,
                failure: SnapshotStateEditCommitFailure::Cleanup,
                staging_cleanup: SnapshotStateEditCleanup::Failed(io::ErrorKind::PermissionDenied),
                directory_sync: None,
            }
        ));
        assert_eq!(
            fs::read(cleanup_fixture.paths.output()).expect("committed output should remain"),
            cleanup_fixture.expected()
        );
        assert_eq!(cleanup_fixture.directory.staging_entries().len(), 1);

        let sync_fixture = EditFixture::new("postcommit-sync");
        let outcome = with_state_edit_failures(
            [(
                SnapshotStateEditStage::OutputDirectorySync,
                io::ErrorKind::Other,
            )],
            || publish_toy(&sync_fixture.paths).expect("commit should return an outcome"),
        );
        assert!(matches!(
            outcome.commit(),
            SnapshotStateEditCommit::Uncertain {
                stage: SnapshotStateEditStage::OutputDirectorySync,
                failure: SnapshotStateEditCommitFailure::Io(io::ErrorKind::Other),
                staging_cleanup: SnapshotStateEditCleanup::Removed,
                directory_sync: Some(io::ErrorKind::Other),
            }
        ));
        assert_eq!(
            fs::read(sync_fixture.paths.output()).expect("committed output should remain"),
            sync_fixture.expected()
        );
        assert!(sync_fixture.directory.staging_entries().is_empty());

        let precedence_fixture = EditFixture::new("postcommit-precedence");
        let outcome = with_state_edit_failures(
            [
                (
                    SnapshotStateEditStage::CommitVerification,
                    io::ErrorKind::InvalidData,
                ),
                (
                    SnapshotStateEditStage::OutputDirectorySync,
                    io::ErrorKind::Other,
                ),
            ],
            || publish_toy(&precedence_fixture.paths).expect("commit should return outcome"),
        );
        assert!(matches!(
            outcome.commit(),
            SnapshotStateEditCommit::Uncertain {
                stage: SnapshotStateEditStage::CommitVerification,
                failure: SnapshotStateEditCommitFailure::Io(io::ErrorKind::InvalidData),
                staging_cleanup: SnapshotStateEditCleanup::Removed,
                directory_sync: Some(io::ErrorKind::Other),
            }
        ));
        assert!(precedence_fixture.paths.output().exists());
    }

    #[test]
    fn committed_identity_changes_are_uncertain_without_final_rollback() {
        let output_fixture = EditFixture::new("postcommit-output-change");
        let output = output_fixture.paths.output().to_path_buf();
        let outcome = with_state_edit_action(
            SnapshotStateEditStage::CommitVerification,
            move || {
                fs::remove_file(&output).expect("committed output should remove");
                fs::write(&output, b"foreign-after-commit").expect("foreign output should replace");
            },
            || publish_toy(&output_fixture.paths).expect("commit should return outcome"),
        );
        assert!(matches!(
            outcome.commit(),
            SnapshotStateEditCommit::Uncertain {
                failure: SnapshotStateEditCommitFailure::OutputEntryChanged,
                ..
            }
        ));
        assert_eq!(
            fs::read(output_fixture.paths.output()).expect("foreign output should remain"),
            b"foreign-after-commit"
        );

        let staging_fixture = EditFixture::new("postcommit-staging-change");
        let directory = staging_fixture.directory.path.clone();
        let outcome = with_state_edit_action(
            SnapshotStateEditStage::CommitVerification,
            move || {
                let staging = one_staging(&directory);
                fs::remove_file(&staging).expect("committed staging should remove");
                fs::write(&staging, b"foreign-staging").expect("foreign staging should replace");
            },
            || publish_toy(&staging_fixture.paths).expect("commit should return outcome"),
        );
        assert!(matches!(
            outcome.commit(),
            SnapshotStateEditCommit::Uncertain {
                failure: SnapshotStateEditCommitFailure::StagingEntryChanged,
                staging_cleanup: SnapshotStateEditCleanup::ChangedRefused,
                ..
            }
        ));
        assert_eq!(
            fs::read(staging_fixture.paths.output()).expect("committed output should remain"),
            staging_fixture.expected()
        );
        assert_eq!(staging_fixture.directory.staging_entries().len(), 1);
    }

    #[test]
    fn concurrent_publishers_have_exactly_one_no_clobber_winner() {
        let fixture = EditFixture::new("concurrent");
        let paths = Arc::new(fixture.paths.clone());
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let paths = Arc::clone(&paths);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                publish_toy(&paths)
            }));
        }
        barrier.wait();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().expect("publisher should not panic"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let loser = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one publisher should lose");
        assert!(matches!(
            loser
                .publication()
                .expect("loser should be infrastructure")
                .failure(),
            SnapshotStateEditFailure::OutputAlreadyExists
        ));
        assert_eq!(
            fs::read(fixture.paths.output()).expect("winner output should read"),
            fixture.expected()
        );
        fixture.assert_source_unchanged();
        assert!(fixture.directory.staging_entries().is_empty());
    }

    #[test]
    fn generic_debug_and_errors_do_not_expose_products_paths_or_operation_values() {
        let fixture = EditFixture::new("redaction");
        let outcome = publish_toy(&fixture.paths).expect("edit should succeed");
        let rendered = format!("{outcome:?}");
        assert!(!rendered.contains("bounded-source-state"));
        assert!(!rendered.contains("edited"));
        assert!(
            !rendered.contains(
                fixture
                    .paths
                    .output()
                    .to_str()
                    .expect("test output should be UTF-8")
            )
        );

        let collision = publish_toy(&fixture.paths).expect_err("second edit should collide");
        let rendered = format!("{collision:?} / {collision}");
        assert!(
            !rendered.contains(
                fixture
                    .paths
                    .input()
                    .to_str()
                    .expect("test input should be UTF-8")
            )
        );
        assert!(
            !rendered.contains(
                fixture
                    .paths
                    .output()
                    .to_str()
                    .expect("test output should be UTF-8")
            )
        );
    }

    #[test]
    fn unwritable_output_parent_fails_before_private_staging() {
        let source = TestDirectory::new("unwritable-source");
        let destination = TestDirectory::new("unwritable-output");
        let input = source.child("input");
        fs::write(&input, SOURCE).expect("source should write");
        fs::set_permissions(&destination.path, fs::Permissions::from_mode(0o500))
            .expect("output parent should become unwritable");
        let paths = SnapshotStateEditPaths::new(&input, destination.child("output"));
        let result = publish_toy(&paths);
        fs::set_permissions(&destination.path, fs::Permissions::from_mode(0o700))
            .expect("output parent should restore");
        let error = result.expect_err("unwritable parent should fail");
        assert!(error.publication().is_some());
        assert!(!paths.output().exists());
        assert!(destination.staging_entries().is_empty());
    }

    #[test]
    fn encoded_length_bounds_reject_empty_and_oversized_results_before_staging() {
        for oversized in [false, true] {
            let fixture = EditFixture::new(if oversized {
                "encoded-large"
            } else {
                "encoded-empty"
            });
            let error = publish_edited_snapshot_state_with(
                &fixture.paths,
                |input| {
                    Ok::<_, ToyError>(ToyProduct {
                        bytes: input.to_vec(),
                    })
                },
                move |_| {
                    Ok(if oversized {
                        vec![0_u8; SNAPSHOT_STATE_EDIT_MAX_FILE_BYTES + 1]
                    } else {
                        Vec::new()
                    })
                },
                |_, _| Ok(()),
            )
            .expect_err("invalid encoded length should reject");
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert_eq!(publication.stage(), SnapshotStateEditStage::Encode);
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::InvalidEncodedStateLength { .. }
            ));
            fixture.assert_uncommitted_and_clean();
        }
    }

    #[test]
    fn explicit_cleanup_failure_after_verify_is_not_retried_on_drop() {
        let fixture = EditFixture::new("cleanup-once");
        let error = with_state_edit_failure_and_cleanup_failure(
            SnapshotStateEditStage::EntryStability,
            io::ErrorKind::Other,
            io::ErrorKind::PermissionDenied,
            || publish_toy(&fixture.paths).expect_err("entry stage should fail"),
        );
        assert_eq!(
            error
                .publication()
                .expect("failure should be infrastructure")
                .staging_cleanup(),
            Some(SnapshotStateEditCleanup::Failed(
                io::ErrorKind::PermissionDenied
            ))
        );
        assert_eq!(fixture.directory.staging_entries().len(), 1);
        assert!(!fixture.paths.output().exists());
    }

    #[test]
    fn committed_cleanup_failure_still_attempts_directory_sync() {
        let fixture = EditFixture::new("cleanup-plus-sync");
        let directory = fixture.directory.path.clone();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_action = Arc::clone(&observed);
        let outcome = with_state_edit_action_and_cleanup_failure(
            SnapshotStateEditStage::OutputDirectorySync,
            move || {
                assert!(one_staging(&directory).exists());
                observed_action.store(true, Ordering::Release);
            },
            io::ErrorKind::PermissionDenied,
            || publish_toy(&fixture.paths).expect("commit should return outcome"),
        );
        assert!(observed.load(Ordering::Acquire));
        assert!(fixture.paths.output().exists());
        assert!(matches!(
            outcome.commit(),
            SnapshotStateEditCommit::Uncertain { .. }
        ));
    }

    #[test]
    fn input_and_output_parent_symlinks_stay_anchored_when_stable() {
        let root = TestDirectory::new("stable-parent-links");
        let actual_input = root.child("actual-input");
        let actual_output = root.child("actual-output");
        fs::create_dir(&actual_input).expect("input dir should create");
        fs::create_dir(&actual_output).expect("output dir should create");
        let input_link = root.child("input-link");
        let output_link = root.child("output-link");
        symlink(&actual_input, &input_link).expect("input parent symlink should create");
        symlink(&actual_output, &output_link).expect("output parent symlink should create");
        let input = input_link.join("state");
        let output = output_link.join("edited");
        fs::write(actual_input.join("state"), SOURCE).expect("source should write");
        let paths = SnapshotStateEditPaths::new(input, output);
        let outcome = publish_toy(&paths).expect("stable parent links should commit");
        assert_eq!(outcome.commit(), SnapshotStateEditCommit::Durable);
        assert_eq!(
            fs::read(actual_output.join("edited")).expect("output should read"),
            outcome.product().bytes
        );
    }

    #[test]
    fn output_parent_that_is_not_a_directory_fails_closed() {
        let root = TestDirectory::new("special-parent");
        let input = root.child("input");
        let parent = root.child("not-a-directory");
        fs::write(&input, SOURCE).expect("input should write");
        fs::write(&parent, b"ordinary").expect("special parent should write");
        let paths = SnapshotStateEditPaths::new(&input, parent.join("output"));
        let error = publish_toy(&paths).expect_err("special parent should reject");
        let publication = error
            .publication()
            .expect("failure should be infrastructure");
        assert_eq!(
            publication.stage(),
            SnapshotStateEditStage::OutputDirectoryOpen
        );
        assert!(!paths.output().exists());
    }

    #[test]
    fn staging_same_inode_mode_and_content_drift_abort_before_commit() {
        for mode_drift in [false, true] {
            let fixture = EditFixture::new(if mode_drift {
                "staging-mode-drift"
            } else {
                "staging-content-drift"
            });
            let directory = fixture.directory.path.clone();
            let error = publish_edited_snapshot_state_with_cancel(
                &fixture.paths,
                |input| {
                    Ok::<_, ToyError>(ToyProduct {
                        bytes: input.to_vec(),
                    })
                },
                |product| Ok(product.bytes.clone()),
                |_, _| Ok(()),
                move |stage| {
                    if stage == SnapshotStateEditStage::Commit {
                        let staging = one_staging(&directory);
                        if mode_drift {
                            fs::set_permissions(&staging, fs::Permissions::from_mode(0o640))
                                .expect("staging mode should change");
                        } else {
                            let mut file = OpenOptions::new()
                                .write(true)
                                .truncate(true)
                                .open(&staging)
                                .expect("staging should reopen");
                            file.write_all(b"changed-staging-state")
                                .expect("staging should change");
                        }
                    }
                    false
                },
            )
            .expect_err("same-inode staging drift should reject");
            let publication = error
                .publication()
                .expect("failure should be infrastructure");
            assert!(matches!(
                publication.failure(),
                SnapshotStateEditFailure::StagingChanged
                    | SnapshotStateEditFailure::StagingContentMismatch
            ));
            assert_eq!(
                publication.staging_cleanup(),
                Some(SnapshotStateEditCleanup::Removed)
            );
            assert!(!fixture.paths.output().exists());
            assert!(fixture.directory.staging_entries().is_empty());
        }
    }
}
