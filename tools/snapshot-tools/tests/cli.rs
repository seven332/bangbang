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
fn help_and_version_expose_the_selected_firecracker_surfaces() {
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
    assert!(top_help.contains("edit-vmstate"));
    assert!(top_help.contains("info-vmstate"));

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

    let info_help = snapshot_editor()
        .args(["info-vmstate", "--help"])
        .output()
        .expect("info-vmstate help should run");
    assert!(info_help.status.success());
    assert!(info_help.stderr.is_empty());
    let info_help = String::from_utf8(info_help.stdout).unwrap();
    assert!(info_help.contains("Usage: snapshot-editor info-vmstate <COMMAND>"));
    assert!(info_help.contains("version"));
    assert!(info_help.contains("vcpu-states"));
    assert!(info_help.contains("vm-state"));

    for command in ["version", "vcpu-states", "vm-state"] {
        let help = snapshot_editor()
            .args(["info-vmstate", command, "--help"])
            .output()
            .expect("nested info help should run");
        assert!(help.status.success());
        assert!(help.stderr.is_empty());
        let help = String::from_utf8(help.stdout).unwrap();
        assert!(help.contains(&format!(
            "Usage: snapshot-editor info-vmstate {command} --vmstate-path <PATH>"
        )));
        assert!(help.contains("-v, --vmstate-path <PATH>"));
    }

    let edit_state_help = snapshot_editor()
        .args(["edit-vmstate", "--help"])
        .output()
        .expect("edit-vmstate help should run");
    assert!(edit_state_help.status.success());
    assert!(edit_state_help.stderr.is_empty());
    let edit_state_help = String::from_utf8(edit_state_help.stdout).unwrap();
    assert!(edit_state_help.contains("Usage: snapshot-editor edit-vmstate <COMMAND>"));
    assert!(edit_state_help.contains("remove-regs"));

    let remove_help = snapshot_editor()
        .args(["edit-vmstate", "remove-regs", "--help"])
        .output()
        .expect("remove-regs help should run");
    assert!(remove_help.status.success());
    assert!(remove_help.stderr.is_empty());
    let remove_help = String::from_utf8(remove_help.stdout).unwrap();
    assert!(remove_help.contains(
        "Usage: snapshot-editor edit-vmstate remove-regs --vmstate-path <PATH> \
         --output-path <PATH> [REGS]..."
    ));
    assert!(remove_help.contains("-v, --vmstate-path <PATH>"));
    assert!(remove_help.contains("-o, --output-path <PATH>"));

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
        vec![
            "edit-vmstate",
            "remove-regs",
            "-1",
            "-v",
            "input",
            "-o",
            "output",
        ],
        vec![
            "edit-vmstate",
            "remove-regs",
            "0x",
            "-v",
            "input",
            "-o",
            "output",
        ],
        vec![
            "edit-vmstate",
            "remove-regs",
            "18446744073709551616",
            "-v",
            "input",
            "-o",
            "output",
        ],
        vec!["private-command", sensitive],
    ] {
        assert_invalid(snapshot_editor(), &arguments, "snapshot-editor", sensitive);
    }
}

#[cfg(all(feature = "tracing", unix))]
#[test]
fn tool_tracing_requires_a_matching_runtime_filter_and_preserves_diagnostics() {
    const SENSITIVE_PATH: &str = "/private/tool-tracing-secret-7d91.vmstate";
    const ARGUMENTS: [&str; 4] = ["info-vmstate", "version", "--vmstate-path", SENSITIVE_PATH];

    let baseline = snapshot_editor()
        .env_remove("BANGBANG_TRACE")
        .args(ARGUMENTS)
        .output()
        .expect("untraced inspection should run");
    assert_eq!(baseline.status.code(), Some(1));
    assert!(baseline.stdout.is_empty());
    let baseline_stderr =
        String::from_utf8(baseline.stderr.clone()).expect("baseline diagnostic should be UTF-8");

    let nonmatching = snapshot_editor()
        .env("BANGBANG_TRACE", "other::module")
        .args(ARGUMENTS)
        .output()
        .expect("filtered inspection should run");
    assert_eq!(nonmatching.status, baseline.status);
    assert_eq!(nonmatching.stdout, baseline.stdout);
    assert_eq!(nonmatching.stderr, baseline.stderr);

    let matching = snapshot_editor()
        .env("BANGBANG_TRACE", "bangbang_snapshot_tools::command")
        .args(ARGUMENTS)
        .output()
        .expect("traced inspection should run");
    assert_eq!(matching.status, baseline.status);
    assert_eq!(matching.stdout, baseline.stdout);
    let stderr = String::from_utf8(matching.stderr).expect("trace diagnostics should be UTF-8");
    let lines = stderr.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), baseline_stderr.lines().count() + 2);
    assert!(
        lines.first().is_some_and(|line| {
            line.contains("level=Trace trace module=bangbang_snapshot_tools::command")
                && line.contains("scope=execute_snapshot_info phase=enter")
        }),
        "trace entry should be first: {stderr}"
    );
    assert!(
        lines.last().is_some_and(|line| {
            line.contains("level=Trace trace module=bangbang_snapshot_tools::command")
                && line.contains("scope=execute_snapshot_info phase=exit")
        }),
        "trace exit should be last: {stderr}"
    );
    let ordinary = lines
        .iter()
        .filter(|line| !line.contains(" trace module="))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    assert_eq!(ordinary, baseline_stderr);
    assert!(!stderr.contains(SENSITIVE_PATH));
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

#[cfg(unix)]
mod state {
    use std::fs::{self, File};
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use bangbang_hvf::{HvfNativeSnapshotDocument, HvfNativeSnapshotRegisterRemovalReport};
    use bangbang_runtime::snapshot_format_v2::{
        NATIVE_V2_GLOBAL_COMPONENT_KEY, NATIVE_V2_LEGACY_PLATFORM_VERSION, SnapshotV2Component,
        decode_snapshot_v2_state_with_compatibility_version,
        encode_snapshot_v2_state_with_compatibility_version,
    };

    use super::*;

    const DBGBVR0: u64 = 0x6030_0000_0013_8004;
    const DBGBCR0: u64 = 0x6030_0000_0013_8005;
    const SMCR_EL1: u64 = 0x6030_0000_0013_c096;
    const SMPRI_EL1: u64 = 0x6030_0000_0013_c094;
    const TPIDR2_EL0: u64 = 0x6030_0000_0013_de85;
    const KVM_SVE_Z0: u64 = 0x6080_0000_0015_0000;
    const KVM_CNTV_CVAL_EL0: u64 = 0x6030_0000_0013_df1a;
    const STAGING_PREFIX: &[u8] = b".bangbang-snapshot-edit-";

    const FIXTURES: &[(&str, &str, &str)] = &[
        (
            "native-v1-1.0.0",
            "1.0.0",
            include_str!("fixtures/native-v1-1.0.0.hex"),
        ),
        (
            "native-v2-2.3.0",
            "2.3.0",
            include_str!("fixtures/native-v2-2.3.0.hex"),
        ),
        (
            "native-v2-2.4.0",
            "2.4.0",
            include_str!("fixtures/native-v2-2.4.0.hex"),
        ),
        (
            "native-v2-2.5.0",
            "2.5.0",
            include_str!("fixtures/native-v2-2.5.0.hex"),
        ),
        (
            "native-v2-2.6.0",
            "2.6.0",
            include_str!("fixtures/native-v2-2.6.0.hex"),
        ),
        (
            "native-v2-2.7.0",
            "2.7.0",
            include_str!("fixtures/native-v2-2.7.0.hex"),
        ),
        (
            "native-v2-2.8.0",
            "2.8.0",
            include_str!("fixtures/native-v2-2.8.0.hex"),
        ),
        (
            "native-v2-2.9.0",
            "2.9.0",
            include_str!("fixtures/native-v2-2.9.0.hex"),
        ),
        (
            "native-v2-2.10.0",
            "2.10.0",
            include_str!("fixtures/native-v2-2.10.0.hex"),
        ),
        (
            "native-v2-2.11.0",
            "2.11.0",
            include_str!("fixtures/native-v2-2.11.0.hex"),
        ),
        (
            "native-v2-2.12.0",
            "2.12.0",
            include_str!("fixtures/native-v2-2.12.0.hex"),
        ),
        (
            "native-v2-2.13.0",
            "2.13.0",
            include_str!("fixtures/native-v2-2.13.0.hex"),
        ),
    ];
    const SME_FIXTURE: &str = include_str!("fixtures/native-v2-2.3.0-sme.hex");
    // Generated by Firecracker v1.16.0 commit
    // d83d72b710361a10294480131377b1b00b163af8 with
    // `Snapshot::new(MicrovmState::default()).save(...)` on Linux/aarch64.
    const FIRECRACKER_FIXTURE: &str = include_str!("fixtures/firecracker-v1.16.0-bitcode.hex");

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let serial = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-snapshot-state-{label}-{}-{serial}",
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
                        .starts_with(STAGING_PREFIX)
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

    fn decode_hex(source: &str) -> Vec<u8> {
        let digits = source
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        assert_eq!(digits.len() % 2, 0, "fixture hex should be paired");
        digits
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).expect("fixture digits should be UTF-8");
                u8::from_str_radix(text, 16).expect("fixture should contain only hex")
            })
            .collect()
    }

    fn fixture(name: &str) -> Vec<u8> {
        let source = FIXTURES
            .iter()
            .find(|(candidate, _, _)| *candidate == name)
            .map(|(_, _, source)| *source)
            .expect("fixture name should exist");
        decode_hex(source)
    }

    fn write_fixture(directory: &TestDirectory, name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.child(name);
        fs::write(&path, bytes).expect("fixture should write");
        path
    }

    fn run_info(view: &str, path: &Path, short: bool) -> Output {
        let mut command = snapshot_editor();
        command.args(["info-vmstate", view]);
        command.arg(if short { "-v" } else { "--vmstate-path" });
        command.arg(path);
        command.output().expect("inspection command should run")
    }

    fn run_removal(ids: &[String], input: &Path, output: &Path, short: bool) -> Output {
        let mut command = snapshot_editor();
        command.args(["edit-vmstate", "remove-regs"]);
        command.args(ids);
        if short {
            command.arg("-v").arg(input).arg("-o").arg(output);
        } else {
            command
                .arg("--vmstate-path")
                .arg(input)
                .arg("--output-path")
                .arg(output);
        }
        command.output().expect("register edit should run")
    }

    fn expected_summary(report: &HvfNativeSnapshotRegisterRemovalReport) -> String {
        let mut summary = String::new();
        for vcpu in report.vcpus() {
            summary.push_str(&format!(
                "vcpu {}: removed {}, not-present {}\n",
                vcpu.vcpu_index(),
                vcpu.removed_count(),
                vcpu.not_present_count(),
            ));
        }
        summary.push_str(&format!(
            "total: requested {}, removed {}, not-present {}\n",
            report.request_count(),
            report.removed_count(),
            report.not_present_count(),
        ));
        summary
    }

    fn assert_redacted(output: &Output, sensitive: &[&str]) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for value in sensitive {
            assert!(!stdout.contains(value), "stdout exposed {value}");
            assert!(!stderr.contains(value), "stderr exposed {value}");
        }
    }

    #[test]
    fn checked_full_document_fixtures_are_canonical_and_firecracker_is_distinct() {
        for (name, expected_version, source) in FIXTURES {
            let bytes = decode_hex(source);
            let document = HvfNativeSnapshotDocument::decode(&bytes)
                .unwrap_or_else(|_| panic!("{name} should decode"));
            assert_eq!(document.version().to_string(), *expected_version);
            assert_eq!(
                document.encode().expect("fixture should re-encode"),
                bytes,
                "{name} should be canonical"
            );
        }
        let sme = decode_hex(SME_FIXTURE);
        let document = HvfNativeSnapshotDocument::decode(&sme).expect("SME fixture should decode");
        assert_eq!(document.version(), NATIVE_V2_LEGACY_PLATFORM_VERSION);
        assert_eq!(document.encode().unwrap(), sme);

        let firecracker = decode_hex(FIRECRACKER_FIXTURE);
        assert!(
            firecracker.len() > 8,
            "Firecracker fixture should include CRC"
        );
        assert_eq!(
            crc64::crc64(0, &firecracker),
            0,
            "Firecracker fixture should retain its valid CRC-64 trailer"
        );
        assert!(HvfNativeSnapshotDocument::decode(&firecracker).is_err());
    }

    #[test]
    fn every_exact_profile_emits_exact_version_and_canonical_json() {
        let directory = TestDirectory::new("all-info-secret-42a7");
        for (index, (name, expected_version, source)) in FIXTURES.iter().enumerate() {
            let bytes = decode_hex(source);
            let document = HvfNativeSnapshotDocument::decode(&bytes).unwrap();
            let path = write_fixture(&directory, &format!("secret-{index}.vmstate"), &bytes);

            let version = run_info("version", &path, index % 2 == 0);
            assert!(version.status.success(), "{name}: {:?}", version.stderr);
            assert_eq!(version.stdout, format!("v{expected_version}\n").as_bytes());
            assert!(version.stderr.is_empty());

            let vcpus = run_info("vcpu-states", &path, index % 2 != 0);
            assert!(vcpus.status.success(), "{name}: {:?}", vcpus.stderr);
            assert_eq!(
                vcpus.stdout,
                format!(
                    "{}\n",
                    document.inspect_vcpu_states().to_pretty_json().unwrap()
                )
                .as_bytes()
            );
            assert!(vcpus.stderr.is_empty());

            let vm = run_info("vm-state", &path, index % 2 == 0);
            assert!(vm.status.success(), "{name}: {:?}", vm.stderr);
            assert_eq!(
                vm.stdout,
                format!(
                    "{}\n",
                    document.inspect_vm_state().to_pretty_json().unwrap()
                )
                .as_bytes()
            );
            assert!(vm.stderr.is_empty());
            assert_redacted(
                &vm,
                &[
                    "all-info-secret-42a7",
                    "sensitive-gic-state",
                    "/tmp/rootfs.img",
                ],
            );
        }
    }

    #[test]
    fn malformed_future_and_firecracker_documents_are_rejected_without_disclosure() {
        let directory = TestDirectory::new("invalid-state-secret-b9c1");
        let valid = fixture("native-v2-2.3.0");
        let mut future = valid.clone();
        future[10..12].copy_from_slice(&99_u16.to_le_bytes());
        let cases = [
            (
                "private-malformed.vmstate",
                b"private-state-payload-d41e".to_vec(),
            ),
            ("private-truncated.vmstate", valid[..100].to_vec()),
            ("private-future.vmstate", future),
            (
                "private-firecracker.vmstate",
                decode_hex(FIRECRACKER_FIXTURE),
            ),
        ];
        for (name, bytes) in cases {
            let input = write_fixture(&directory, name, &bytes);
            for view in ["version", "vcpu-states", "vm-state"] {
                let output = run_info(view, &input, false);
                assert_eq!(output.status.code(), Some(1));
                assert!(output.stdout.is_empty());
                assert_redacted(
                    &output,
                    &[
                        "invalid-state-secret-b9c1",
                        name,
                        "private-state-payload-d41e",
                    ],
                );
            }
            let destination = directory.child(&format!("{name}.edited"));
            let output = run_removal(&[format!("0x{DBGBVR0:x}")], &input, &destination, true);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(!destination.exists());
            assert_redacted(&output, &[name, "private-state-payload-d41e", "6030"]);
        }
        assert!(directory.staging_entries().is_empty());
    }

    #[test]
    fn decimal_hex_and_space_delimited_edits_preserve_exact_profiles() {
        let directory = TestDirectory::new("edit-success-secret-e518");
        let cases = [
            ("native-v1-1.0.0", vec![DBGBVR0], vec![DBGBVR0.to_string()]),
            (
                "native-v2-2.3.0",
                vec![DBGBVR0, DBGBCR0],
                vec![format!("0X{DBGBVR0:X} 0x{DBGBCR0:x}")],
            ),
        ];
        for (index, (name, ids, arguments)) in cases.into_iter().enumerate() {
            let bytes = fixture(name);
            let input = write_fixture(&directory, &format!("secret-input-{index}"), &bytes);
            let output_path = directory.child(&format!("secret-output-{index}"));
            let original = HvfNativeSnapshotDocument::decode(&bytes).unwrap();
            let expected = original
                .clone()
                .try_remove_reviewed_kvm_registers(&ids)
                .expect("reviewed removal should succeed");

            let output = run_removal(&arguments, &input, &output_path, index % 2 == 0);
            assert!(output.status.success(), "{name}: {:?}", output.stderr);
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                expected_summary(expected.report())
            );
            assert!(output.stderr.is_empty());
            assert_eq!(fs::read(&input).unwrap(), bytes);
            let edited = fs::read(&output_path).expect("edited output should exist");
            assert_eq!(edited, expected.document().encode().unwrap());
            let decoded = HvfNativeSnapshotDocument::decode(&edited).unwrap();
            assert_eq!(decoded.version(), original.version());
            assert_eq!(decoded.profile(), original.profile());
            assert_eq!(decoded.encode().unwrap(), edited);
            assert_eq!(
                fs::metadata(&output_path).unwrap().permissions().mode() & 0o7777,
                0o600
            );
            assert_redacted(&output, &["edit-success-secret-e518", "6030"]);
        }
        assert!(directory.staging_entries().is_empty());
    }

    #[test]
    fn sme_and_repeated_edits_report_removed_then_not_present() {
        let directory = TestDirectory::new("sme-secret-1b8d");
        let bytes = decode_hex(SME_FIXTURE);
        let input = write_fixture(&directory, "private-sme-input", &bytes);
        let first_output = directory.child("private-sme-first");
        let ids = [TPIDR2_EL0, SMPRI_EL1, SMCR_EL1];
        let arguments = ids.iter().map(|id| format!("0x{id:x}")).collect::<Vec<_>>();
        let original = HvfNativeSnapshotDocument::decode(&bytes).unwrap();
        let expected = original
            .clone()
            .try_remove_reviewed_kvm_registers(&ids)
            .unwrap();

        let first = run_removal(&arguments, &input, &first_output, true);
        assert!(first.status.success(), "{:?}", first.stderr);
        assert_eq!(
            String::from_utf8(first.stdout).unwrap(),
            expected_summary(expected.report())
        );
        assert_eq!(
            fs::read(&first_output).unwrap(),
            expected.document().encode().unwrap()
        );

        let second_output = directory.child("private-sme-second");
        let second_expected = expected
            .document()
            .clone()
            .try_remove_reviewed_kvm_registers(&ids)
            .unwrap();
        let second = run_removal(&arguments, &first_output, &second_output, false);
        assert!(second.status.success(), "{:?}", second.stderr);
        assert_eq!(
            String::from_utf8(second.stdout).unwrap(),
            expected_summary(second_expected.report())
        );
        assert_eq!(second_expected.report().removed_count(), 0);
        assert!(second.stderr.is_empty());
        assert!(directory.staging_entries().is_empty());
    }

    #[test]
    fn reviewed_request_admission_precedes_all_path_access() {
        let directory = TestDirectory::new("admission-secret-a8c4");
        let missing_input = directory.child("private-missing-input");
        let output_path = directory.child("private-never-output");
        let invalid = [
            vec![],
            vec![format!("0x{DBGBVR0:x}"), format!("0X{DBGBVR0:X}")],
            vec!["16045690984503098046".to_string()],
            vec![format!("0x{KVM_CNTV_CVAL_EL0:x}")],
            vec![format!("0x{KVM_SVE_Z0:x}")],
        ];
        for arguments in invalid {
            let output = run_removal(&arguments, &missing_input, &output_path, true);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                "snapshot-editor: reviewed register-removal request is invalid\n"
            );
            assert!(!output_path.exists());
        }
        assert!(directory.staging_entries().is_empty());
    }

    #[test]
    fn alias_and_existing_output_fail_without_mutation_or_staging() {
        let directory = TestDirectory::new("no-clobber-secret-8df2");
        let bytes = fixture("native-v2-2.3.0");
        let input = write_fixture(&directory, "private-input", &bytes);
        let existing = write_fixture(&directory, "private-existing", b"foreign-output-marker");
        let alias = directory.child("private-hard-link-alias");
        fs::hard_link(&input, &alias).expect("hard-link alias should create");
        let id = vec![format!("0x{DBGBVR0:x}")];

        for output_path in [&input, &alias, &existing] {
            let before = fs::read(output_path).unwrap();
            let output = run_removal(&id, &input, output_path, false);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(fs::read(output_path).unwrap(), before);
            assert_eq!(fs::read(&input).unwrap(), bytes);
            assert_redacted(
                &output,
                &["no-clobber-secret-8df2", "private-input", "6030"],
            );
            assert!(directory.staging_entries().is_empty());
        }
    }

    fn closed_pipe() -> Stdio {
        let mut descriptors = [0; 2];
        // SAFETY: `pipe` initializes both descriptor slots on success.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // SAFETY: the initialized read descriptor is owned here and deliberately
        // closed so writes in the child observe a broken pipe.
        assert_eq!(unsafe { libc::close(descriptors[0]) }, 0);
        // SAFETY: the initialized write descriptor is uniquely transferred to
        // `File`, then to `Stdio`.
        Stdio::from(unsafe { File::from_raw_fd(descriptors[1]) })
    }

    fn wait_without_stdin(mut child: Child) -> Output {
        let held_stdin = child.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child
                .try_wait()
                .expect("child status should be readable")
                .is_some()
            {
                drop(held_stdin);
                return child
                    .wait_with_output()
                    .expect("child output should collect");
            }
            assert!(Instant::now() < deadline, "command waited for stdin");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn commands_do_not_read_stdin_and_stream_closure_never_panics() {
        let directory = TestDirectory::new("streams-secret-5c93");
        let missing = directory.child("private-missing");
        let child = snapshot_editor()
            .args(["info-vmstate", "version", "-v"])
            .arg(&missing)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("held-stdin command should spawn");
        let output = wait_without_stdin(child);
        assert_eq!(output.status.code(), Some(1));
        assert_redacted(&output, &["streams-secret-5c93", "private-missing"]);

        let sme = decode_hex(SME_FIXTURE);
        let input = write_fixture(&directory, "private-stream-input", &sme);
        let inspection = snapshot_editor()
            .args(["info-vmstate", "vm-state", "-v"])
            .arg(&input)
            .stdout(closed_pipe())
            .stderr(Stdio::piped())
            .output()
            .expect("closed inspection stream should run");
        assert_eq!(inspection.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(inspection.stderr).unwrap(),
            "snapshot-editor: failed to write snapshot inspection output\n"
        );

        let committed_output = directory.child("private-committed-output");
        let edit = snapshot_editor()
            .args(["edit-vmstate", "remove-regs", "0x6030000000138004", "-v"])
            .arg(&input)
            .arg("-o")
            .arg(&committed_output)
            .stdout(closed_pipe())
            .stderr(Stdio::piped())
            .output()
            .expect("closed summary stream should run");
        assert_eq!(edit.status.code(), Some(3));
        assert_eq!(
            String::from_utf8(edit.stderr).unwrap(),
            "snapshot-editor: edited snapshot state committed, but summary output failed\n"
        );
        assert!(HvfNativeSnapshotDocument::decode(&fs::read(&committed_output).unwrap()).is_ok());

        let diagnostic_closed = snapshot_editor()
            .args(["info-vmstate", "version", "-v"])
            .arg(&missing)
            .stdout(Stdio::piped())
            .stderr(closed_pipe())
            .output()
            .expect("closed diagnostic stream should run");
        assert_eq!(diagnostic_closed.status.code(), Some(1));
        assert!(directory.staging_entries().is_empty());
    }

    fn large_state_fixture() -> Vec<u8> {
        const GLOBAL_HEADER_AND_COMPATIBILITY_BYTES: usize = 24 + 376;
        const GIC_LENGTH_OFFSET: usize = 16;
        const LARGE_GIC_BYTES: usize = 12 * 1024 * 1024;

        let original = fixture("native-v2-2.3.0");
        let state = decode_snapshot_v2_state_with_compatibility_version(
            &original,
            NATIVE_V2_LEGACY_PLATFORM_VERSION,
        )
        .expect("small structural fixture should decode");
        let version = state.metadata().version();
        let required_features = state.required_features().collect::<Vec<_>>();
        let mut owned = state
            .components()
            .map(|component| {
                (
                    component.key(),
                    component.disposition(),
                    component.payload().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        let global = owned
            .iter_mut()
            .find(|(key, _, _)| *key == NATIVE_V2_GLOBAL_COMPONENT_KEY)
            .expect("global component should exist");
        global.2.truncate(GLOBAL_HEADER_AND_COMPATIBILITY_BYTES);
        global.2.extend(std::iter::repeat_n(0x6d, LARGE_GIC_BYTES));
        global.2[GIC_LENGTH_OFFSET..GIC_LENGTH_OFFSET + 4]
            .copy_from_slice(&(LARGE_GIC_BYTES as u32).to_le_bytes());
        let components = owned
            .iter()
            .map(|(key, disposition, payload)| {
                SnapshotV2Component::new(*key, *disposition, payload)
            })
            .collect::<Vec<_>>();
        let bytes = encode_snapshot_v2_state_with_compatibility_version(
            version,
            &required_features,
            &components,
        )
        .expect("large structural fixture should encode");
        HvfNativeSnapshotDocument::decode(&bytes).expect("large production document should decode");
        bytes
    }

    fn spawn_large_edit(input: &Path, output: &Path) -> Child {
        snapshot_editor()
            .args(["edit-vmstate", "remove-regs", "0x6030000000138004", "-v"])
            .arg(input)
            .arg("-o")
            .arg(output)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("large register edit should spawn")
    }

    fn stop_after_staging(directory: &TestDirectory, child: &mut Child) -> PathBuf {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let entries = directory.staging_entries();
            if let Some(staging) = entries.first() {
                // SAFETY: the PID belongs to the live child and SIGSTOP does
                // not invoke application code.
                assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGSTOP) }, 0);
                let mut status = 0;
                // SAFETY: `status` is a valid output slot and the PID belongs
                // to this test's child process.
                let waited =
                    unsafe { libc::waitpid(child.id() as i32, &mut status, libc::WUNTRACED) };
                assert_eq!(waited, child.id() as i32);
                assert!(libc::WIFSTOPPED(status));
                return staging.clone();
            }
            assert!(
                child
                    .try_wait()
                    .expect("child status should be readable")
                    .is_none(),
                "edit exited before staging became observable"
            );
            assert!(Instant::now() < deadline, "staging was not observed");
            thread::sleep(Duration::from_micros(100));
        }
    }

    fn signal_child(child: &Child, signal: i32) {
        // SAFETY: the PID belongs to this test's live child process and the
        // supplied signals are fixed Unix process-control signals.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    }

    fn resume_and_collect(child: Child) -> Output {
        signal_child(&child, libc::SIGCONT);
        child
            .wait_with_output()
            .expect("resumed child should complete")
    }

    #[test]
    fn actual_process_detects_source_output_and_staging_races() {
        let large = large_state_fixture();

        {
            let directory = TestDirectory::new("source-race-secret-76c2");
            let input = write_fixture(&directory, "private-source", &large);
            let retained = directory.child("private-retained-source");
            let output_path = directory.child("private-output");
            let mut child = spawn_large_edit(&input, &output_path);
            let _staging = stop_after_staging(&directory, &mut child);
            assert!(!output_path.exists(), "output must be absent before commit");
            fs::rename(&input, &retained).expect("source should move");
            fs::write(&input, b"foreign-source-marker").expect("foreign source should create");
            let output = resume_and_collect(child);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(!output_path.exists());
            assert_eq!(fs::read(&input).unwrap(), b"foreign-source-marker");
            assert_eq!(fs::read(&retained).unwrap(), large);
            assert!(directory.staging_entries().is_empty());
            assert_redacted(
                &output,
                &["source-race-secret-76c2", "foreign-source-marker", "6030"],
            );
        }

        {
            let directory = TestDirectory::new("output-race-secret-91e3");
            let input = write_fixture(&directory, "private-source", &large);
            let output_path = directory.child("private-output");
            let mut child = spawn_large_edit(&input, &output_path);
            let _staging = stop_after_staging(&directory, &mut child);
            assert!(!output_path.exists(), "output must be absent before commit");
            fs::write(&output_path, b"foreign-output-marker")
                .expect("foreign output should create");
            let output = resume_and_collect(child);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(fs::read(&input).unwrap(), large);
            assert_eq!(fs::read(&output_path).unwrap(), b"foreign-output-marker");
            assert!(directory.staging_entries().is_empty());
            assert_redacted(
                &output,
                &["output-race-secret-91e3", "foreign-output-marker", "6030"],
            );
        }

        {
            let directory = TestDirectory::new("staging-race-secret-a2f4");
            let input = write_fixture(&directory, "private-source", &large);
            let output_path = directory.child("private-output");
            let retained_staging = directory.child("private-retained-staging");
            let mut child = spawn_large_edit(&input, &output_path);
            let staging = stop_after_staging(&directory, &mut child);
            assert!(!output_path.exists(), "output must be absent before commit");
            fs::rename(&staging, &retained_staging).expect("owned staging should move");
            fs::write(&staging, b"foreign-staging-marker").expect("foreign staging should create");
            let output = resume_and_collect(child);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(!output_path.exists());
            assert_eq!(fs::read(&input).unwrap(), large);
            assert_eq!(fs::read(&staging).unwrap(), b"foreign-staging-marker");
            assert!(retained_staging.exists());
            assert_eq!(directory.staging_entries(), vec![staging]);
            assert_redacted(
                &output,
                &["staging-race-secret-a2f4", "foreign-staging-marker", "6030"],
            );
        }
    }

    #[test]
    fn actual_process_maps_stable_signals_before_commit() {
        let large = large_state_fixture();
        for (label, signal, expected_exit) in [
            ("sigint-secret-14b7", libc::SIGINT, 130),
            ("sigterm-secret-2ce8", libc::SIGTERM, 143),
        ] {
            let directory = TestDirectory::new(label);
            let input = write_fixture(&directory, "private-source", &large);
            let output_path = directory.child("private-output");
            let mut child = spawn_large_edit(&input, &output_path);
            let _staging = stop_after_staging(&directory, &mut child);
            assert!(!output_path.exists(), "output must be absent before commit");
            signal_child(&child, signal);
            let output = resume_and_collect(child);
            assert_eq!(output.status.code(), Some(expected_exit));
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("cancelled before commit"));
            assert!(!output_path.exists());
            assert_eq!(fs::read(&input).unwrap(), large);
            assert!(directory.staging_entries().is_empty());
            assert_redacted(&output, &[label, "private-source", "6030"]);
        }
    }
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
