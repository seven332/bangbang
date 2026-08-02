#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::process::{Command, Output};

const DEPRECATION_NOTICE: &str = "This tool is deprecated and will be removed in the future. Please use 'snapshot-editor' instead.";

fn rebase_snap() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rebase-snap"))
}

fn snapshot_editor() -> Command {
    Command::new(env!("CARGO_BIN_EXE_snapshot-editor"))
}

fn assert_invalid(mut command: Command, arguments: &[&str], tool: &str, sensitive: &str) {
    let output = command
        .args(arguments)
        .output()
        .expect("invalid command should start");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("diagnostic should be UTF-8");
    assert_eq!(
        stderr,
        format!("{tool}: invalid arguments; use --help for the supported interface\n")
    );
    assert!(!stderr.contains(sensitive));
}

#[cfg(target_os = "macos")]
fn assert_operational_failure(output: &Output, tool: &str, sensitive: &[&str]) {
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    if tool == "rebase-snap" {
        assert_eq!(stdout, format!("{DEPRECATION_NOTICE}\n"));
    } else {
        assert!(stdout.is_empty());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with(&format!("{tool}: native-v2 Diff rebase failed during ")));
    for value in sensitive {
        assert!(!stderr.contains(value));
    }
}

#[cfg(not(target_os = "macos"))]
fn assert_unsupported_target(output: &Output, tool: &str, sensitive: &[&str]) {
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    if tool == "rebase-snap" {
        assert_eq!(stdout, format!("{DEPRECATION_NOTICE}\n"));
    } else {
        assert!(stdout.is_empty());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr,
        format!("{tool}: native-v2 Diff rebase is supported only on macOS\n")
    );
    for value in sensitive {
        assert!(!stderr.contains(value));
    }
}

#[test]
fn help_and_version_expose_only_the_two_selected_firecracker_surfaces() {
    let help = rebase_snap()
        .arg("--help")
        .output()
        .expect("deprecated help should run");
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help = String::from_utf8(help.stdout).expect("help should be UTF-8");
    assert!(help.contains("Usage: rebase-snap --base-file <PATH> --diff-file <PATH>"));
    assert!(help.contains("--base-file <PATH>"));
    assert!(help.contains("--diff-file <PATH>"));
    assert!(help.ends_with(&format!("{DEPRECATION_NOTICE}\n")));

    let version = rebase_snap()
        .arg("--version")
        .output()
        .expect("deprecated version should run");
    assert!(version.status.success());
    assert!(version.stderr.is_empty());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!(
            "rebase-snap {} (bangbang; Firecracker v1.16.0-compatible command surface)\n\
             {DEPRECATION_NOTICE}\n",
            env!("CARGO_PKG_VERSION")
        )
    );

    let top_help = snapshot_editor()
        .arg("--help")
        .output()
        .expect("editor help should run");
    assert!(top_help.status.success());
    assert!(top_help.stderr.is_empty());
    let top_help = String::from_utf8(top_help.stdout).unwrap();
    assert!(top_help.contains("Usage: snapshot-editor <COMMAND>"));
    assert!(top_help.contains("edit-memory"));
    assert!(!top_help.contains("edit-vmstate"));
    assert!(!top_help.contains("info-vmstate"));

    let edit_help = snapshot_editor()
        .args(["edit-memory", "--help"])
        .output()
        .expect("edit-memory help should run");
    assert!(edit_help.status.success());
    assert!(edit_help.stderr.is_empty());
    let edit_help = String::from_utf8(edit_help.stdout).unwrap();
    assert!(edit_help.contains("Usage: snapshot-editor edit-memory <COMMAND>"));
    assert!(edit_help.contains("rebase"));

    let rebase_help = snapshot_editor()
        .args(["edit-memory", "rebase", "--help"])
        .output()
        .expect("nested rebase help should run");
    assert!(rebase_help.status.success());
    assert!(rebase_help.stderr.is_empty());
    let rebase_help = String::from_utf8(rebase_help.stdout).unwrap();
    assert!(rebase_help.contains(
        "Usage: snapshot-editor edit-memory rebase --memory-path <PATH> --diff-path <PATH>"
    ));
    assert!(rebase_help.contains("-m, --memory-path <PATH>"));
    assert!(rebase_help.contains("-d, --diff-path <PATH>"));

    let version = snapshot_editor()
        .arg("--version")
        .output()
        .expect("editor version should run");
    assert!(version.status.success());
    assert!(version.stderr.is_empty());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!(
            "snapshot-editor {} (bangbang; Firecracker v1.16.0-compatible command surface)\n",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn invalid_invocations_are_deterministic_and_do_not_echo_values() {
    let sensitive = "private-cli-value-9f6f";
    for arguments in [
        vec![],
        vec!["--base-file", sensitive],
        vec!["--base-file", sensitive, "--diff-file"],
        vec![
            "--base-file",
            sensitive,
            "--base-file",
            "second",
            "--diff-file",
            "diff",
        ],
        vec!["--base-file", "base", "--diff-file", "diff", sensitive],
        vec!["--unknown-private-option", sensitive],
    ] {
        assert_invalid(rebase_snap(), &arguments, "rebase-snap", sensitive);
    }

    for arguments in [
        vec![],
        vec!["edit-memory"],
        vec!["edit-memory", "rebase", "--memory-path", sensitive],
        vec![
            "edit-memory",
            "rebase",
            "--memory-path",
            sensitive,
            "-m",
            "second",
            "--diff-path",
            "diff",
        ],
        vec![
            "edit-memory",
            "rebase",
            "-m",
            "base",
            "-d",
            "diff",
            sensitive,
        ],
        vec!["edit-vmstate", sensitive],
        vec!["private-command", sensitive],
    ] {
        assert_invalid(snapshot_editor(), &arguments, "snapshot-editor", sensitive);
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_target_rejects_before_accessing_or_echoing_paths() {
    let base = "secret-missing-base-unsupported";
    let diff = "secret-missing-diff-unsupported";
    let deprecated = rebase_snap()
        .args(["--base-file", base, "--diff-file", diff])
        .output()
        .expect("deprecated command should run");
    assert_unsupported_target(&deprecated, "rebase-snap", &[base, diff]);

    let replacement = snapshot_editor()
        .args([
            "edit-memory",
            "rebase",
            "--memory-path",
            base,
            "--diff-path",
            diff,
        ])
        .output()
        .expect("replacement command should run");
    assert_unsupported_target(&replacement, "snapshot-editor", &[base, diff]);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::{FileExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange, aarch64,
    };
    use bangbang_runtime::snapshot_diff_v2_13::{
        NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION, SnapshotV2DiffBase, SnapshotV2DiffSelection,
        write_snapshot_v2_diff_layer,
    };
    use bangbang_runtime::snapshot_memory_v2::{
        SnapshotV2MemoryBinding, write_snapshot_v2_memory_image_with_compatibility_version,
    };

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let serial = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-snapshot-tools-{label}-{}-{serial}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should create");
            Self { path }
        }

        fn child(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        fn staging_entries(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.path)
                .expect("test directory should read")
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

    fn page_range(first_page: u64, page_count: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START + first_page * aarch64::GUEST_PAGE_SIZE),
            page_count * aarch64::GUEST_PAGE_SIZE,
        )
        .expect("test range should validate")
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

    fn create_file(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .expect("test artifact should create")
    }

    fn write_complete(path: &Path, memory: &GuestMemory) -> SnapshotV2MemoryBinding {
        write_snapshot_v2_memory_image_with_compatibility_version(
            memory,
            &mut create_file(path),
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
        write_snapshot_v2_diff_layer(memory, &mut create_file(path), base, &selection)
            .expect("Diff layer should write")
            .result()
            .clone()
    }

    fn run_deprecated(base: &Path, diff: &Path) -> Output {
        rebase_snap()
            .arg("--base-file")
            .arg(base)
            .arg("--diff-file")
            .arg(diff)
            .output()
            .expect("deprecated command should start")
    }

    fn run_replacement(base: &Path, diff: &Path, short: bool) -> Output {
        let mut command = snapshot_editor();
        command.args(["edit-memory", "rebase"]);
        if short {
            command.arg("-m").arg(base).arg("-d").arg(diff);
        } else {
            command
                .arg("--memory-path")
                .arg(base)
                .arg("--diff-path")
                .arg(diff);
        }
        command.output().expect("replacement command should start")
    }

    fn assert_deprecated_success(output: &Output) {
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("{DEPRECATION_NOTICE}\n")
        );
        assert!(output.stderr.is_empty());
    }

    fn assert_replacement_success(output: &Output) {
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    fn assert_result_payload(path: &Path, binding: &SnapshotV2MemoryBinding, expected_byte: u8) {
        let file = File::open(path).expect("complete result should open");
        for extent in binding.extents().iter().copied() {
            let mut bytes =
                vec![0_u8; usize::try_from(extent.range().size()).expect("extent should fit")];
            file.read_exact_at(&mut bytes, extent.file_offset())
                .expect("complete result should read");
            assert!(bytes.iter().all(|byte| *byte == expected_byte));
        }
        assert_eq!(
            fs::metadata(path)
                .expect("result metadata should read")
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn both_commands_materialize_byte_identical_complete_images() {
        let directory = TestDirectory::new("equivalent");
        let original = directory.child("original.mem");
        let deprecated_base = directory.child("deprecated.mem");
        let replacement_base = directory.child("replacement.mem");
        let diff = directory.child("next.diff");
        let range = page_range(0, 4);
        let base = write_complete(&original, &memory_with_byte(range, 0x21));
        let result = write_layer(
            &diff,
            &memory_with_byte(range, 0xa4),
            SnapshotV2DiffBase::Image(base),
            &[range],
        );
        let diff_before = fs::read(&diff).expect("Diff should read");
        fs::copy(&original, &deprecated_base).expect("deprecated base should copy");
        fs::copy(&original, &replacement_base).expect("replacement base should copy");

        assert_deprecated_success(&run_deprecated(&deprecated_base, &diff));
        assert_replacement_success(&run_replacement(&replacement_base, &diff, true));

        assert_eq!(
            fs::read(&deprecated_base).expect("deprecated result should read"),
            fs::read(&replacement_base).expect("replacement result should read")
        );
        assert_eq!(fs::read(&diff).expect("Diff should remain"), diff_before);
        assert_result_payload(&deprecated_base, &result, 0xa4);
        assert!(directory.staging_entries().is_empty());
    }

    #[test]
    fn malformed_stale_and_alias_inputs_fail_without_mutation_or_leaks() {
        let directory = TestDirectory::new("invalid");
        let range = page_range(0, 4);
        let good_base = directory.child("secret-good-base.mem");
        let good_binding = write_complete(&good_base, &memory_with_byte(range, 0x31));
        let diff = directory.child("secret-next.diff");
        write_layer(
            &diff,
            &memory_with_byte(range, 0x72),
            SnapshotV2DiffBase::Image(good_binding),
            &[range],
        );

        let malformed = directory.child("secret-malformed-guest-bytes.mem");
        fs::write(&malformed, b"private-guest-payload-7b4c").expect("malformed file should write");
        let malformed_before = fs::read(&malformed).unwrap();
        let diff_before = fs::read(&diff).unwrap();
        let output = run_deprecated(&malformed, &diff);
        assert_operational_failure(
            &output,
            "rebase-snap",
            &["secret-malformed-guest-bytes", "private-guest-payload-7b4c"],
        );
        assert_eq!(fs::read(&malformed).unwrap(), malformed_before);
        assert_eq!(fs::read(&diff).unwrap(), diff_before);

        let stale_base = directory.child("secret-stale-base.mem");
        write_complete(&stale_base, &memory_with_byte(range, 0x91));
        let stale_before = fs::read(&stale_base).unwrap();
        let output = run_replacement(&stale_base, &diff, false);
        assert_operational_failure(
            &output,
            "snapshot-editor",
            &["secret-stale-base", "secret-next"],
        );
        assert_eq!(fs::read(&stale_base).unwrap(), stale_before);
        assert_eq!(fs::read(&diff).unwrap(), diff_before);

        let exact_alias = run_deprecated(&good_base, &good_base);
        assert_operational_failure(&exact_alias, "rebase-snap", &["secret-good-base"]);
        assert!(String::from_utf8_lossy(&exact_alias.stderr).contains("source alias check"));

        let alias = directory.child("secret-hard-link-alias.mem");
        fs::hard_link(&good_base, &alias).expect("hard-link alias should create");
        let output = run_replacement(&good_base, &alias, true);
        assert_operational_failure(
            &output,
            "snapshot-editor",
            &["secret-hard-link-alias", "secret-good-base"],
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("source alias check"));
        assert_eq!(
            fs::read(&good_base).expect("aliased base should remain"),
            fs::read(&alias).expect("hard-link alias should remain")
        );
        assert_eq!(fs::read(&diff).unwrap(), diff_before);
        assert!(directory.staging_entries().is_empty());
    }

    #[test]
    fn sequential_commands_apply_repeated_lineage_exactly() {
        let directory = TestDirectory::new("sequential");
        let base_path = directory.child("base.mem");
        let first_diff = directory.child("first.diff");
        let second_diff = directory.child("second.diff");
        let whole = page_range(0, 4);
        let first_page = page_range(0, 1);
        let second_page = page_range(1, 1);
        let base_memory = memory_with_byte(whole, 0x18);
        let base = write_complete(&base_path, &base_memory);

        let mut first_memory = memory_with_byte(whole, 0x18);
        first_memory
            .write_slice(
                &vec![0x41; usize::try_from(first_page.size()).unwrap()],
                first_page.start(),
            )
            .expect("first page should update");
        let first_result = write_layer(
            &first_diff,
            &first_memory,
            SnapshotV2DiffBase::Image(base),
            &[first_page],
        );
        let mut second_memory = first_memory;
        second_memory
            .write_slice(
                &vec![0xb2; usize::try_from(second_page.size()).unwrap()],
                second_page.start(),
            )
            .expect("second page should update");
        let second_result = write_layer(
            &second_diff,
            &second_memory,
            SnapshotV2DiffBase::Image(first_result),
            &[second_page],
        );
        let first_before = fs::read(&first_diff).unwrap();
        let second_before = fs::read(&second_diff).unwrap();

        assert_deprecated_success(&run_deprecated(&base_path, &first_diff));
        assert_replacement_success(&run_replacement(&base_path, &second_diff, false));
        assert_eq!(fs::read(&first_diff).unwrap(), first_before);
        assert_eq!(fs::read(&second_diff).unwrap(), second_before);

        let file = File::open(&base_path).expect("final result should open");
        for extent in second_result.extents().iter().copied() {
            let length = usize::try_from(extent.range().size()).unwrap();
            let mut actual = vec![0_u8; length];
            let mut expected = vec![0_u8; length];
            file.read_exact_at(&mut actual, extent.file_offset())
                .expect("final payload should read");
            second_memory
                .read_slice(&mut expected, extent.range().start())
                .expect("expected payload should read");
            assert_eq!(actual, expected);
        }
        assert!(directory.staging_entries().is_empty());
    }

    fn spawn_deprecated(base: &Path, diff: &Path) -> Child {
        rebase_snap()
            .arg("--base-file")
            .arg(base)
            .arg("--diff-file")
            .arg(diff)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("deprecated command should spawn")
    }

    fn spawn_replacement(base: &Path, diff: &Path) -> Child {
        let mut command = snapshot_editor();
        command
            .args(["edit-memory", "rebase"])
            .arg("--memory-path")
            .arg(base)
            .arg("--diff-path")
            .arg(diff)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("replacement command should spawn")
    }

    fn wait_for_staging(directory: &TestDirectory, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !directory.staging_entries().is_empty() {
                return;
            }
            assert!(
                child
                    .try_wait()
                    .expect("child status should be available")
                    .is_none(),
                "command exited before publishing a staging entry"
            );
            assert!(
                Instant::now() < deadline,
                "command did not publish a staging entry before timeout"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn signal_child(child: &Child, signal: i32) {
        // SAFETY: the PID belongs to the live child process and signal is
        // SIGINT or SIGTERM for this focused cancellation test.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    }

    #[test]
    fn process_signals_and_path_substitutions_preserve_precommit_inputs() {
        const LARGE_PAGE_COUNT: u64 = 32 * 1024;

        let directory = TestDirectory::new("races");
        let original_base = directory.child("original-base.mem");
        let original_diff = directory.child("original-next.diff");
        let range = page_range(0, LARGE_PAGE_COUNT);
        let base = write_complete(&original_base, &memory_with_byte(range, 0x28));
        write_layer(
            &original_diff,
            &memory_with_byte(range, 0xc3),
            SnapshotV2DiffBase::Image(base),
            &[range],
        );
        let base_bytes = fs::read(&original_base).expect("large base should read");
        let diff_bytes = fs::read(&original_diff).expect("large Diff should read");

        for (case, signal, expected_exit, deprecated) in [
            ("sigint", libc::SIGINT, 130, true),
            ("sigterm", libc::SIGTERM, 143, false),
        ] {
            let base_path = directory.child(&format!("{case}-base.mem"));
            let diff_path = directory.child(&format!("{case}-next.diff"));
            fs::copy(&original_base, &base_path).expect("signal base should copy");
            fs::copy(&original_diff, &diff_path).expect("signal Diff should copy");
            let mut child = if deprecated {
                spawn_deprecated(&base_path, &diff_path)
            } else {
                spawn_replacement(&base_path, &diff_path)
            };
            wait_for_staging(&directory, &mut child);
            signal_child(&child, signal);
            let output = child
                .wait_with_output()
                .expect("signalled child should exit");
            assert_eq!(output.status.code(), Some(expected_exit));
            assert_eq!(fs::read(&base_path).unwrap(), base_bytes);
            assert_eq!(fs::read(&diff_path).unwrap(), diff_bytes);
            assert!(directory.staging_entries().is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("cancelled before commit"));
            assert!(!stderr.contains(case));
        }

        let base_path = directory.child("base-replacement-base.mem");
        let diff_path = directory.child("base-replacement-next.diff");
        let moved_base = directory.child("base-replacement-retained.mem");
        fs::copy(&original_base, &base_path).expect("replacement base should copy");
        fs::copy(&original_diff, &diff_path).expect("replacement Diff should copy");
        let mut child = spawn_deprecated(&base_path, &diff_path);
        wait_for_staging(&directory, &mut child);
        fs::rename(&base_path, &moved_base).expect("retained base should move");
        fs::write(&base_path, b"unrelated-private-base-replacement")
            .expect("unrelated base replacement should write");
        let output = child
            .wait_with_output()
            .expect("base replacement child should exit");
        assert_operational_failure(
            &output,
            "rebase-snap",
            &["base-replacement", "unrelated-private-base-replacement"],
        );
        assert_eq!(
            fs::read(&base_path).unwrap(),
            b"unrelated-private-base-replacement"
        );
        assert_eq!(fs::read(&moved_base).unwrap(), base_bytes);
        assert_eq!(fs::read(&diff_path).unwrap(), diff_bytes);
        assert!(directory.staging_entries().is_empty());

        let base_path = directory.child("diff-replacement-base.mem");
        let diff_path = directory.child("diff-replacement-next.diff");
        let moved_diff = directory.child("diff-replacement-retained.diff");
        fs::copy(&original_base, &base_path).expect("replacement base should copy");
        fs::copy(&original_diff, &diff_path).expect("replacement Diff should copy");
        let mut child = spawn_replacement(&base_path, &diff_path);
        wait_for_staging(&directory, &mut child);
        fs::rename(&diff_path, &moved_diff).expect("retained Diff should move");
        fs::write(&diff_path, b"unrelated-private-diff-replacement")
            .expect("unrelated Diff replacement should write");
        let output = child
            .wait_with_output()
            .expect("Diff replacement child should exit");
        assert_operational_failure(
            &output,
            "snapshot-editor",
            &["diff-replacement", "unrelated-private-diff-replacement"],
        );
        assert_eq!(fs::read(&base_path).unwrap(), base_bytes);
        assert_eq!(
            fs::read(&diff_path).unwrap(),
            b"unrelated-private-diff-replacement"
        );
        assert_eq!(fs::read(&moved_diff).unwrap(), diff_bytes);
        assert!(directory.staging_entries().is_empty());
    }
}
