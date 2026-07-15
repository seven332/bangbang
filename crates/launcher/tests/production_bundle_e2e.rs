#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_launcher::{
    LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME, OUTER_BUNDLE_NAME,
    WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};
use bangbang_session::{
    Frame, FrameDecoder, GRANT_FD, Message, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD,
    SessionId, encode_frame,
};

const BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_BUNDLE_PATH";
const GRANT_TEST_BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_GRANT_TEST_BUNDLE_PATH";
const GUEST_EXT4_ROOTFS_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
const GRANT_MANIFEST_OPTION: &str = "--bangbang-grant-manifest";
const GRANT_PROBE_OPTION: &str = "--bangbang-internal-grant-probe-v1";
const GRANT_PROBE_READY: &str = "status: grant integration probe ready";
const GRANT_DELAY_OPTION: &str = "--bangbang-internal-grant-delay-v1";
const GRANT_DELAY_READY: &str = "status: grant integration delay ready";
const GRANT_PROBE_MARKER: &str = "grant-integration-probe.enabled";
const GRANT_PROBE_OUTSIDE: &str = "bangbang-grant-probe-outside";
const STARTUP_CONFIG_ID: &str = "grant-config-1360";
const STARTUP_METADATA_ID: &str = "grant-metadata-1360";
const KERNEL_ID: &str = "grant-kernel-1360";
const INITRD_ID: &str = "grant-initrd-1360";
const STARTUP_CONFIG_REF: &str = "bangbang-grant:grant-config-1360";
const STARTUP_METADATA_REF: &str = "bangbang-grant:grant-metadata-1360";
const KERNEL_REF: &str = "bangbang-grant:grant-kernel-1360";
const INITRD_REF: &str = "bangbang-grant:grant-initrd-1360";
const STARTUP_DRIVE_RO_ID: &str = "grant-startup-drive-ro-1362";
const STARTUP_DRIVE_RW_ID: &str = "grant-startup-drive-rw-1362";
const STARTUP_PMEM_RO_ID: &str = "grant-startup-pmem-ro-1362";
const STARTUP_PMEM_RW_ID: &str = "grant-startup-pmem-rw-1362";
const STARTUP_DRIVE_RO_REF: &str = "bangbang-grant:grant-startup-drive-ro-1362";
const STARTUP_DRIVE_RW_REF: &str = "bangbang-grant:grant-startup-drive-rw-1362";
const STARTUP_PMEM_RO_REF: &str = "bangbang-grant:grant-startup-pmem-ro-1362";
const STARTUP_PMEM_RW_REF: &str = "bangbang-grant:grant-startup-pmem-rw-1362";
const GUEST_ROOTFS_ID: &str = "grant-guest-rootfs-1362";
const GUEST_DATA_ID: &str = "grant-guest-data-1362";
const GUEST_REPLACEMENT_ID: &str = "grant-guest-replacement-1362";
const GUEST_PMEM_ID: &str = "grant-guest-pmem-1362";
const GUEST_READ_ONLY_DATA_ID: &str = "grant-guest-read-only-data-1362";
const GUEST_ROOTFS_REF: &str = "bangbang-grant:grant-guest-rootfs-1362";
const GUEST_DATA_REF: &str = "bangbang-grant:grant-guest-data-1362";
const GUEST_REPLACEMENT_REF: &str = "bangbang-grant:grant-guest-replacement-1362";
const GUEST_PMEM_REF: &str = "bangbang-grant:grant-guest-pmem-1362";
const GUEST_READ_ONLY_DATA_REF: &str = "bangbang-grant:grant-guest-read-only-data-1362";
const GUEST_MISSING_REF: &str = "bangbang-grant:grant-guest-missing-1362";
const DIRECT_ROOTFS_PMEM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-read-flush=1";
const DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.memory-hotplug-check=1";
const DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-writeback-flush=1";
const PMEM_HOST_MARKER: &[u8] = b"BANGBANG_PMEM_HOST_MARKER";
const PMEM_GUEST_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_GUEST_FLUSH_OK";
const PMEM_GUEST_FLUSH_OFFSET: u64 = 4096;
const PMEM_BACKING_LEN: u64 = 2 * 1024 * 1024;
const PMEM_RESULT_MARKER: &[u8] = b"BANGBANG_PMEM_READ_FLUSH_OK";
const MEMORY_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_READY";
const MEMORY_HOTPLUG_GROWN_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_GROWN";
const MEMORY_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK";
const READ_ONLY_BLOCK_FAILURE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_WRITE";
const BAD_CONFIGURATION_EXIT_CODE: i32 = 152;
const ARGUMENT_PARSING_EXIT_CODE: i32 = 153;
const PROCESS_FAILURE_EXIT_CODE: i32 = 1;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

fn production_bundle() -> PathBuf {
    let path = std::env::var_os(BUNDLE_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the production bundle path");
    let path = PathBuf::from(path);
    assert_eq!(path.file_name(), Some(OsStr::new(OUTER_BUNDLE_NAME)));
    path
}

fn grant_test_bundle() -> PathBuf {
    let path = std::env::var_os(GRANT_TEST_BUNDLE_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the grant test bundle path");
    let path = PathBuf::from(path);
    assert_eq!(path.file_name(), Some(OsStr::new(OUTER_BUNDLE_NAME)));
    assert!(
        worker_bundle(&path)
            .join("Contents/Resources")
            .join(GRANT_PROBE_MARKER)
            .is_file(),
        "grant exerciser bundle must carry a visible test-only marker"
    );
    path
}

fn guest_ext4_rootfs() -> PathBuf {
    let path = std::env::var_os(GUEST_EXT4_ROOTFS_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the direct-rootfs fixture path");
    let path = PathBuf::from(path);
    assert!(path.is_file(), "direct-rootfs fixture must be a file");
    path
}

fn launcher(bundle: &Path) -> PathBuf {
    bundle.join("Contents/MacOS").join(LAUNCHER_EXECUTABLE_NAME)
}

fn worker_bundle(bundle: &Path) -> PathBuf {
    bundle.join("Contents/Helpers").join(WORKER_BUNDLE_NAME)
}

fn worker_executable(bundle: &Path) -> PathBuf {
    worker_bundle(bundle)
        .join("Contents/MacOS")
        .join(WORKER_EXECUTABLE_NAME)
}

fn run_launcher(bundle: &Path, args: &[&OsStr]) -> Output {
    Command::new(launcher(bundle))
        .args(args)
        .output()
        .expect("production launcher should execute")
}

#[test]
fn production_bundle_has_exact_nested_signing_contract() {
    let bundle = production_bundle();
    let worker = worker_bundle(&bundle);
    let verify = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=4"])
        .arg(&bundle)
        .output()
        .expect("codesign verification should execute");
    assert_output_success(&verify, "strict recursive bundle verification");

    let outer_display = codesign_display(&bundle);
    let worker_display = codesign_display(&worker);
    assert!(
        outer_display.contains(&format!("Identifier={LAUNCHER_BUNDLE_IDENTIFIER}")),
        "outer identifier should match; display:\n{outer_display}"
    );
    assert!(
        worker_display.contains(&format!("Identifier={WORKER_BUNDLE_IDENTIFIER}")),
        "worker identifier should match; display:\n{worker_display}"
    );
    assert!(outer_display.contains("runtime"));
    assert!(worker_display.contains("runtime"));

    let outer_entitlements = codesign_entitlements(&bundle);
    let worker_entitlements = codesign_entitlements(&worker);
    assert!(
        !outer_entitlements.contains("com.apple.security.app-sandbox")
            && !outer_entitlements.contains("com.apple.security.hypervisor"),
        "launcher must not inherit worker entitlements: {outer_entitlements}"
    );
    assert_eq!(worker_entitlements.matches("<key>").count(), 2);
    assert!(worker_entitlements.contains("<key>com.apple.security.app-sandbox</key>"));
    assert!(worker_entitlements.contains("<key>com.apple.security.hypervisor</key>"));
}

#[test]
fn launcher_forwards_help_and_argument_parsing_exit() {
    let bundle = production_bundle();
    let help = run_launcher(&bundle, &[OsStr::new("--help")]);
    assert_output_success(&help, "launcher help");
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    assert!(help_stdout.contains("Usage:\n  bangbang [OPTIONS]"));

    let version = run_launcher(&bundle, &[OsStr::new("--version")]);
    assert_output_success(&version, "launcher version");
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("bangbang "));

    let opaque = OsString::from_vec(vec![0xff, 0xfe]);
    let opaque_version = run_launcher(
        &bundle,
        &[
            OsStr::new("--version"),
            OsStr::new("--"),
            opaque.as_os_str(),
        ],
    );
    assert_output_success(&opaque_version, "opaque argument forwarding");
    assert!(String::from_utf8_lossy(&opaque_version.stdout).starts_with("bangbang "));

    let bad = run_launcher(&bundle, &[OsStr::new("--no-api")]);
    assert_eq!(bad.status.code(), Some(ARGUMENT_PARSING_EXIT_CODE));
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("--no-api requires --config-file"));
    assert!(!stderr.contains("launcher signal"));
}

#[test]
fn launcher_rejects_modified_missing_or_wrongly_signed_worker_before_execution() {
    let source = production_bundle();

    let modified = TestDir::new("modified");
    let modified_bundle = modified.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &modified_bundle);
    OpenOptions::new()
        .append(true)
        .open(worker_executable(&modified_bundle))
        .expect("copied worker should open")
        .write_all(b"tamper")
        .expect("copied worker should be modified");
    assert_invalid_bundle(run_launcher(&modified_bundle, &[OsStr::new("--help")]));

    let missing = TestDir::new("missing");
    let missing_bundle = missing.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &missing_bundle);
    fs::remove_file(worker_executable(&missing_bundle)).expect("copied worker should be removed");
    assert_invalid_bundle(run_launcher(&missing_bundle, &[OsStr::new("--help")]));

    let false_entitlement = TestDir::new("false-entitlement");
    let false_bundle = false_entitlement.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &false_bundle);
    resign_worker_and_outer(
        &false_bundle,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><false/>
<key>com.apple.security.hypervisor</key><true/>
</dict></plist>"#,
        true,
        true,
    );
    assert_invalid_bundle(run_launcher(&false_bundle, &[OsStr::new("--help")]));

    let extra_entitlement = TestDir::new("extra-entitlement");
    let extra_bundle = extra_entitlement.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &extra_bundle);
    resign_worker_and_outer(
        &extra_bundle,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
<key>com.apple.security.network.client</key><true/>
</dict></plist>"#,
        true,
        true,
    );
    assert_invalid_bundle(run_launcher(&extra_bundle, &[OsStr::new("--help")]));

    let valid_entitlements = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
</dict></plist>"#;

    let worker_without_runtime = TestDir::new("worker-without-runtime");
    let worker_without_runtime_bundle = worker_without_runtime.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &worker_without_runtime_bundle);
    resign_worker_and_outer(
        &worker_without_runtime_bundle,
        valid_entitlements,
        false,
        true,
    );
    assert_invalid_bundle(run_launcher(
        &worker_without_runtime_bundle,
        &[OsStr::new("--help")],
    ));

    let outer_without_runtime = TestDir::new("outer-without-runtime");
    let outer_without_runtime_bundle = outer_without_runtime.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &outer_without_runtime_bundle);
    resign_worker_and_outer(
        &outer_without_runtime_bundle,
        valid_entitlements,
        true,
        false,
    );
    assert_invalid_bundle(run_launcher(
        &outer_without_runtime_bundle,
        &[OsStr::new("--help")],
    ));
}

#[test]
fn launcher_preserves_sandbox_outside_path_denial_and_redaction() {
    let bundle = production_bundle();
    let denied = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = run_launcher(
        &bundle,
        &[
            OsStr::new("--config-file"),
            denied.as_os_str(),
            OsStr::new("--no-api"),
        ],
    );
    assert_eq!(output.status.code(), Some(BAD_CONFIGURATION_EXIT_CODE));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config-file error: failed to read config file: PermissionDenied"));
    let denied = denied.to_string_lossy();
    assert!(!stdout.contains(denied.as_ref()) && !stderr.contains(denied.as_ref()));
    assert!(!stdout.contains("status: VM running without API"));
}

#[test]
fn normal_bundle_grants_external_config_metadata_and_boot_inputs_to_real_guest() {
    let bundle = production_bundle();
    let fixture = StartupGrantFixture::new(&bundle, "no-api");
    let output = run_with_timeout(
        Command::new(launcher(&bundle))
            .arg(GRANT_MANIFEST_OPTION)
            .arg(&fixture.manifest)
            .arg("--")
            .args(["--config-file", STARTUP_CONFIG_REF])
            .args(["--metadata", STARTUP_METADATA_REF])
            .arg("--no-api"),
        PROCESS_TIMEOUT,
        "external startup-grant guest SYSTEM_OFF",
    );

    assert_output_success(&output, "external startup-grant guest SYSTEM_OFF");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("status: VM running without API"));
    assert!(!stdout.contains("status: API server listening"));
    fixture.assert_output_redacted(&output);
}

#[test]
fn normal_bundle_delays_boot_claim_until_api_and_keeps_opened_identity() {
    let bundle = production_bundle();
    let mut fixture = StartupGrantFixture::new(&bundle, "api-identity");
    let mut running = spawn_ready_startup_grant_api_launcher(&bundle, &fixture, true);

    let metadata = http_get(&running.socket, "/mmds");
    assert!(
        metadata.starts_with("HTTP/1.1 200 "),
        "response:\n{metadata}"
    );
    assert!(metadata.contains(&fixture.metadata_marker));

    fixture.replace_boot_pathnames();
    let boot_source = serde_json::json!({
        "kernel_image_path": KERNEL_REF,
        "initrd_path": INITRD_REF,
        "boot_args": "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init",
    });
    let boot_response = http_put(
        &running.socket,
        "/boot-source",
        &serde_json::to_string(&boot_source).expect("boot request should serialize"),
    );
    assert!(
        boot_response.starts_with("HTTP/1.1 204 "),
        "response:\n{boot_response}"
    );
    let config = http_get(&running.socket, "/vm/config");
    assert!(config.starts_with("HTTP/1.1 200 "), "response:\n{config}");
    assert!(config.contains(KERNEL_REF));
    assert!(config.contains(INITRD_REF));

    let start_response = http_put(
        &running.socket,
        "/actions",
        r#"{"action_type":"InstanceStart"}"#,
    );
    assert!(
        start_response.starts_with("HTTP/1.1 204 "),
        "response:\n{start_response}"
    );
    let status = running.wait("external delayed-grant guest SYSTEM_OFF");
    assert!(
        status.success(),
        "guest should reach SYSTEM_OFF: {status:?}"
    );
    assert!(!running.socket.exists());
}

#[test]
fn normal_bundle_rejects_wrong_and_missing_boot_claims_without_consuming_pair() {
    let bundle = production_bundle();
    let fixture = StartupGrantFixture::new(&bundle, "api-mismatch");
    let mut running = spawn_ready_startup_grant_api_launcher(&bundle, &fixture, false);

    let prior_kernel = "/sealed/prior-kernel";
    let prior = serde_json::json!({"kernel_image_path": prior_kernel});
    let prior_response = http_put(
        &running.socket,
        "/boot-source",
        &serde_json::to_string(&prior).expect("prior request should serialize"),
    );
    assert!(
        prior_response.starts_with("HTTP/1.1 204 "),
        "response:\n{prior_response}"
    );

    let invalid_command_line = serde_json::json!({
        "kernel_image_path": KERNEL_REF,
        "initrd_path": INITRD_REF,
        "boot_args": "invalid\0command-line",
    });
    let invalid_response = http_put(
        &running.socket,
        "/boot-source",
        &serde_json::to_string(&invalid_command_line)
            .expect("invalid command-line request should serialize"),
    );
    assert!(
        invalid_response.starts_with("HTTP/1.1 400 "),
        "response:\n{invalid_response}"
    );
    assert!(invalid_response.contains("kernel command line is invalid"));
    for sensitive in fixture.sensitive_strings() {
        assert!(!invalid_response.contains(&sensitive));
    }
    let unchanged = http_get(&running.socket, "/vm/config");
    assert!(unchanged.contains(prior_kernel));

    let wrong_role = serde_json::json!({
        "kernel_image_path": KERNEL_REF,
        "initrd_path": STARTUP_METADATA_REF,
    });
    let wrong_response = http_put(
        &running.socket,
        "/boot-source",
        &serde_json::to_string(&wrong_role).expect("wrong-role request should serialize"),
    );
    assert_private_grant_fault(&wrong_response, &fixture);
    let unchanged = http_get(&running.socket, "/vm/config");
    assert!(unchanged.contains(prior_kernel));
    assert!(!unchanged.contains(KERNEL_REF));

    let missing = serde_json::json!({
        "kernel_image_path": "bangbang-grant:missing",
        "initrd_path": INITRD_REF,
    });
    let missing_response = http_put(
        &running.socket,
        "/boot-source",
        &serde_json::to_string(&missing).expect("missing request should serialize"),
    );
    assert_private_grant_fault(&missing_response, &fixture);
    let unchanged = http_get(&running.socket, "/vm/config");
    assert!(unchanged.contains(prior_kernel));

    let valid = serde_json::json!({
        "kernel_image_path": KERNEL_REF,
        "initrd_path": INITRD_REF,
    });
    let valid_response = http_put(
        &running.socket,
        "/boot-source",
        &serde_json::to_string(&valid).expect("valid request should serialize"),
    );
    assert!(
        valid_response.starts_with("HTTP/1.1 204 "),
        "response:\n{valid_response}"
    );

    let pid = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: `pid` is the live unreaped launcher owned by this test.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let status = running.wait("grant mismatch graceful stop");
    assert!(status.success());
    assert!(!running.socket.exists());
}

#[test]
fn normal_bundle_adopts_delayed_block_and_pmem_grants_by_descriptor_identity() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("delayed-pmem");
    let mut running = spawn_ready_device_grant_api_launcher(&bundle, &fixture, "delayed-pmem");
    fixture.replace_source_pathnames();

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT /machine-config for delayed pmem grants",
    );

    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_PMEM_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot request should serialize"),
        ),
        204,
        "PUT /boot-source for delayed pmem grants",
    );

    let prior_path = "/sealed/prior-data";
    let prior_data = serde_json::json!({
        "drive_id": "data",
        "path_on_host": prior_path,
        "is_root_device": false,
        "is_read_only": false,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/data",
            &serde_json::to_string(&prior_data).expect("prior drive should serialize"),
        ),
        204,
        "PUT prior /drives/data",
    );

    let wrong_role = serde_json::json!({
        "drive_id": "data",
        "path_on_host": GUEST_PMEM_REF,
        "is_root_device": false,
        "is_read_only": false,
    });
    let wrong_role_response = http_put(
        &running.socket,
        "/drives/data",
        &serde_json::to_string(&wrong_role).expect("wrong-role drive should serialize"),
    );
    assert_device_private_grant_fault(&wrong_role_response, &fixture);
    let unchanged = http_get(&running.socket, "/vm/config");
    assert_http_status(&unchanged, 200, "GET /vm/config after wrong role");
    assert!(unchanged.contains(prior_path));
    assert!(!unchanged.contains(GUEST_PMEM_REF));

    let missing = serde_json::json!({
        "drive_id": "data",
        "path_on_host": GUEST_MISSING_REF,
        "is_root_device": false,
        "is_read_only": false,
    });
    let missing_response = http_put(
        &running.socket,
        "/drives/data",
        &serde_json::to_string(&missing).expect("missing drive should serialize"),
    );
    assert_device_private_grant_fault(&missing_response, &fixture);
    assert!(!missing_response.contains(GUEST_MISSING_REF));
    let unchanged = http_get(&running.socket, "/vm/config");
    assert_http_status(&unchanged, 200, "GET /vm/config after missing grant");
    assert!(unchanged.contains(prior_path));
    assert!(!unchanged.contains(GUEST_MISSING_REF));

    let wrong_access = serde_json::json!({
        "drive_id": "data",
        "path_on_host": GUEST_ROOTFS_REF,
        "is_root_device": false,
        "is_read_only": false,
    });
    let wrong_access_response = http_put(
        &running.socket,
        "/drives/data",
        &serde_json::to_string(&wrong_access).expect("wrong-access drive should serialize"),
    );
    assert_device_private_grant_fault(&wrong_access_response, &fixture);

    let malformed = serde_json::json!({
        "drive_id": "data",
        "path_on_host": "bangbang-grant:",
        "is_root_device": false,
        "is_read_only": false,
    });
    let malformed_response = http_put(
        &running.socket,
        "/drives/data",
        &serde_json::to_string(&malformed).expect("malformed drive should serialize"),
    );
    assert_device_private_grant_fault(&malformed_response, &fixture);

    let rootfs = serde_json::json!({
        "drive_id": "rootfs",
        "path_on_host": GUEST_ROOTFS_REF,
        "is_root_device": true,
        "is_read_only": true,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/rootfs",
            &serde_json::to_string(&rootfs).expect("rootfs drive should serialize"),
        ),
        204,
        "PUT granted rootfs",
    );

    let data = serde_json::json!({
        "drive_id": "data",
        "path_on_host": GUEST_DATA_REF,
        "is_root_device": false,
        "is_read_only": false,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/data",
            &serde_json::to_string(&data).expect("data drive should serialize"),
        ),
        204,
        "PUT granted data drive",
    );

    let duplicate = serde_json::json!({
        "drive_id": "duplicate",
        "path_on_host": GUEST_DATA_REF,
        "is_root_device": false,
        "is_read_only": false,
    });
    let duplicate_response = http_put(
        &running.socket,
        "/drives/duplicate",
        &serde_json::to_string(&duplicate).expect("duplicate drive should serialize"),
    );
    assert_device_private_grant_fault(&duplicate_response, &fixture);

    let pmem = serde_json::json!({
        "id": "pmem0",
        "path_on_host": GUEST_PMEM_REF,
        "read_only": false,
        "rate_limiter": {
            "bandwidth": {"size": PMEM_BACKING_LEN, "refill_time": 1000},
            "ops": {"size": 1, "refill_time": 1000},
        },
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/pmem0",
            &serde_json::to_string(&pmem).expect("pmem request should serialize"),
        ),
        204,
        "PUT granted pmem",
    );

    let config = http_get(&running.socket, "/vm/config");
    assert_http_status(&config, 200, "GET /vm/config for device grants");
    for reference in [GUEST_ROOTFS_REF, GUEST_DATA_REF, GUEST_PMEM_REF] {
        assert!(
            config.contains(reference),
            "authorized config response should retain {reference:?}: {config}"
        );
    }
    assert!(!config.contains(prior_path));
    assert!(!config.contains(r#""drive_id":"duplicate""#));

    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start delayed block and pmem guest",
    );
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/drives/data",
            r#"{"drive_id":"data","rate_limiter":{"bandwidth":{"size":1000,"one_time_burst":1000,"refill_time":100}}}"#,
        ),
        204,
        "path-free live block update",
    );
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/pmem/pmem0",
            r#"{"id":"pmem0","rate_limiter":{"bandwidth":null,"ops":null}}"#,
        ),
        204,
        "live pmem rate-limiter update",
    );

    wait_for_file_prefix(&fixture.opened_data, PMEM_RESULT_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| panic!("guest should report pmem success: {error}"));
    assert_eq!(
        file_bytes_at(
            &fixture.opened_pmem,
            PMEM_GUEST_FLUSH_OFFSET,
            PMEM_GUEST_FLUSH_MARKER.len(),
        ),
        PMEM_GUEST_FLUSH_MARKER,
        "guest pmem flush should update the launcher-opened object"
    );
    assert_eq!(
        file_bytes_at(&fixture.data, 0, PMEM_RESULT_MARKER.len()),
        vec![0; PMEM_RESULT_MARKER.len()],
        "replacement source pathname must not receive guest block writes"
    );
    assert_eq!(
        file_bytes_at(
            &fixture.pmem,
            PMEM_GUEST_FLUSH_OFFSET,
            PMEM_GUEST_FLUSH_MARKER.len(),
        ),
        vec![0; PMEM_GUEST_FLUSH_MARKER.len()],
        "replacement pmem pathname must not receive guest flushes"
    );

    stop_running_launcher(&mut running, "delayed block and pmem grant guest");
}

#[test]
fn normal_bundle_live_block_grant_swap_uses_preauthorized_open_file() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("live-block");
    let mut running = spawn_ready_device_grant_api_launcher(&bundle, &fixture, "live-block");
    fixture.replace_source_pathnames();

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT live-block machine config",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/hotplug/memory",
            r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
        ),
        204,
        "PUT live-block memory hotplug config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot request should serialize"),
        ),
        204,
        "PUT live-block boot source",
    );
    for (path, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": GUEST_ROOTFS_REF,
                "is_root_device": true,
                "is_read_only": true,
            }),
            "PUT live-block rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
            }),
            "PUT live-block data",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive should serialize"),
            ),
            204,
            context,
        );
    }
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start live-block guest",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        MEMORY_HOTPLUG_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("guest should reach live-update checkpoint: {error}"));

    let replacement = serde_json::json!({
        "drive_id": "data",
        "path_on_host": GUEST_REPLACEMENT_REF,
    });
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/drives/data",
            &serde_json::to_string(&replacement).expect("replacement should serialize"),
        ),
        204,
        "PATCH live block grant replacement",
    );
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/drives/data",
            r#"{"drive_id":"data","rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
        ),
        204,
        "PATCH live block limiter without replacing backing",
    );
    let config = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &config,
        200,
        "GET config after live block grant replacement",
    );
    assert!(config.contains(GUEST_REPLACEMENT_REF));
    assert!(!config.contains(GUEST_DATA_REF));

    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
        ),
        204,
        "grow memory after live block swap",
    );
    wait_for_file_prefix(
        &fixture.opened_replacement,
        MEMORY_HOTPLUG_GROWN_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("replacement backing should receive grown marker: {error}"));
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":0}"#,
        ),
        204,
        "shrink memory after live block swap",
    );
    wait_for_file_prefix(
        &fixture.opened_replacement,
        MEMORY_HOTPLUG_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("replacement backing should receive success marker: {error}"));
    assert_eq!(
        file_bytes_at(&fixture.replacement, 0, MEMORY_HOTPLUG_SUCCESS_MARKER.len(),),
        vec![0; MEMORY_HOTPLUG_SUCCESS_MARKER.len()],
        "planted replacement pathname must remain unused"
    );

    stop_running_launcher(&mut running, "live block grant guest");
}

#[test]
fn normal_bundle_enforces_read_only_drive_grant_against_guest_writes() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("read-only-block");
    let mut running = spawn_ready_device_grant_api_launcher(&bundle, &fixture, "read-only-block");
    fixture.replace_source_pathnames();
    let serial_file = TestFilePath::new(container_tmp_dir().join(format!(
        "bb-read-only-{:x}-{}.serial",
        std::process::id(),
        NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst)
    )));
    running
        .sensitive
        .push(path_text(serial_file.path()).to_owned());

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT read-only machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot request should serialize"),
        ),
        204,
        "PUT read-only boot source",
    );
    for (path, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": GUEST_ROOTFS_REF,
                "is_root_device": true,
                "is_read_only": true,
            }),
            "PUT read-only rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": GUEST_READ_ONLY_DATA_REF,
                "is_root_device": false,
                "is_read_only": true,
                "cache_type": "Writeback",
            }),
            "PUT read-only data drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive should serialize"),
            ),
            204,
            context,
        );
    }
    let serial = serde_json::json!({"serial_out_path": path_text(serial_file.path())});
    assert_http_status(
        &http_put(
            &running.socket,
            "/serial",
            &serde_json::to_string(&serial).expect("serial config should serialize"),
        ),
        204,
        "PUT contained serial output",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start read-only block guest",
    );
    wait_for_file_contains(
        serial_file.path(),
        READ_ONLY_BLOCK_FAILURE_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("guest should report read-only write rejection: {error}"));
    assert_eq!(
        file_bytes_at(
            &fixture.opened_read_only_data,
            0,
            READ_ONLY_BLOCK_FAILURE_MARKER.len(),
        ),
        vec![0; READ_ONLY_BLOCK_FAILURE_MARKER.len()],
        "read-only granted backing must remain unchanged"
    );

    stop_running_launcher(&mut running, "read-only block grant guest");
}

#[test]
fn normal_production_bundle_excludes_grant_probe_behavior() {
    let bundle = production_bundle();
    assert!(
        !worker_bundle(&bundle)
            .join("Contents/Resources")
            .join(GRANT_PROBE_MARKER)
            .exists(),
        "normal production bundle must not carry the probe marker"
    );
    let fixture = GrantProbeFixture::new("single", false);
    let output = run_grant_probe(&bundle, &fixture, "single");
    assert_eq!(output.status.code(), Some(ARGUMENT_PARSING_EXIT_CODE));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(GRANT_PROBE_READY));
    fixture.assert_unmodified();
}

#[test]
fn signed_grants_authorize_only_typed_read_write_and_directory_operations() {
    let bundle = grant_test_bundle();
    let fixture = GrantProbeFixture::new("single", false);
    let output = run_grant_probe(&bundle, &fixture, "single");
    assert_output_success(&output, "signed resource grant probe");
    fixture.assert_completed();
    assert_grant_output_redacted(&output, &fixture);
}

#[test]
fn signed_grant_mismatch_fails_closed_without_mutation() {
    let bundle = grant_test_bundle();
    let fixture = GrantProbeFixture::new("single", true);
    let output = run_grant_probe(&bundle, &fixture, "single");
    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "bangbang: private launcher session failed\n"
    );
    fixture.assert_unmodified();
    assert_grant_output_redacted(&output, &fixture);
}

#[test]
fn signal_cancels_an_incomplete_grant_phase_without_waiting_for_timeout() {
    let bundle = grant_test_bundle();
    let fixture = GrantProbeFixture::new("single", false);
    let mut delayed = spawn_holding_grant_delay(&bundle, &fixture);
    let started = Instant::now();
    delayed.stop(libc::SIGTERM, "delayed grant cancellation");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "event-driven cancellation must beat the grant deadline"
    );
    fixture.assert_unmodified();
}

#[test]
fn incomplete_grant_phase_obeys_one_absolute_deadline() {
    let bundle = grant_test_bundle();
    let fixture = GrantProbeFixture::new("single", false);
    let started = Instant::now();
    let output = run_with_timeout(
        &mut grant_delay_command(&bundle, &fixture),
        PROCESS_TIMEOUT,
        "grant absolute deadline",
    );
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert!(elapsed >= Duration::from_secs(4));
    assert!(elapsed < Duration::from_secs(10));
    fixture.assert_unmodified();
    assert_grant_output_redacted(&output, &fixture);
}

#[test]
fn concurrent_signed_grant_sessions_keep_authority_noninterchangeable() {
    let bundle = grant_test_bundle();
    recover_session_root(&bundle);
    let alpha_fixture = GrantProbeFixture::new("alpha", false);
    let beta_fixture = GrantProbeFixture::new("beta", false);
    let mut alpha = spawn_holding_grant_probe(&bundle, &alpha_fixture, "hold-alpha");
    let mut beta = spawn_holding_grant_probe(&bundle, &beta_fixture, "hold-beta");
    assert_eq!(session_entries().len(), 2);
    alpha.stop(libc::SIGTERM, "alpha grant probe");
    beta.stop(libc::SIGTERM, "beta grant probe");
    alpha_fixture.assert_completed();
    beta_fixture.assert_completed();
    assert!(session_entries().is_empty());
}

#[test]
fn signed_grant_scopes_cleanup_across_both_process_crash_orders() {
    let bundle = grant_test_bundle();
    recover_session_root(&bundle);

    let launcher_fixture = GrantProbeFixture::new("hold", false);
    let mut launcher_first = spawn_holding_grant_probe(&bundle, &launcher_fixture, "hold");
    let worker_pid = only_worker_pid(&launcher_first.child);
    let worker_exit = ProcessExitWatch::new(worker_pid);
    let launcher_pid = i32::try_from(launcher_first.child.id()).expect("launcher PID should fit");
    // SAFETY: The unreaped launcher owns this PID and its worker observes the
    // authenticated lifecycle EOF independently.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGKILL) }, 0);
    let launcher_status = launcher_first.wait("grant launcher SIGKILL");
    assert_eq!(launcher_status.signal(), Some(libc::SIGKILL));
    assert!(
        worker_exit.wait(PROCESS_TIMEOUT),
        "grant worker should exit after launcher EOF"
    );
    launcher_fixture.assert_completed();
    assert!(session_entries().is_empty());

    let worker_fixture = GrantProbeFixture::new("hold", false);
    let mut worker_first = spawn_holding_grant_probe(&bundle, &worker_fixture, "hold");
    let worker_pid = only_worker_pid(&worker_first.child);
    // SAFETY: The worker is the one live child of the unreaped launcher.
    assert_eq!(unsafe { libc::kill(worker_pid, libc::SIGKILL) }, 0);
    let worker_status = worker_first.wait("grant worker SIGKILL");
    assert_eq!(worker_status.code(), Some(128 + libc::SIGKILL));
    worker_fixture.assert_completed();
    assert!(session_entries().is_empty());
}

#[test]
fn launcher_forwards_graceful_signals_and_worker_cleans_owned_socket() {
    run_graceful_signal_case(libc::SIGINT, "sigint");
    run_graceful_signal_case(libc::SIGTERM, "sigterm");
}

#[test]
fn launcher_runs_real_sandboxed_hvf_guest_to_system_off() {
    let bundle = production_bundle();
    let config = worker_bundle(&bundle).join("Contents/Resources/vm-config.json");
    assert!(config.is_file(), "signed runner must seal the guest config");
    let output = run_with_timeout(
        Command::new(launcher(&bundle))
            .args([OsStr::new("--config-file"), config.as_os_str()])
            .arg("--no-api"),
        PROCESS_TIMEOUT,
        "production sandbox guest SYSTEM_OFF",
    );
    assert_output_success(&output, "production sandbox guest SYSTEM_OFF");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("status: VM running without API"));
    assert!(!stdout.contains("status: API server listening"));
}

#[test]
fn contained_worker_closes_unexpected_inherited_descriptor() {
    let bundle = production_bundle();
    let fixture = TestDir::new("inherited-fd");
    let config = fixture.path().join("config.json");
    fs::write(&config, b"{}").expect("probe config should be written");
    let file = fs::File::open(&config).expect("probe config should open");
    // SAFETY: `file` remains live and the returned descriptor is independently owned.
    let inherited = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 200) };
    assert!(inherited >= 200, "high probe descriptor should duplicate");
    // SAFETY: `inherited` is the fresh descriptor above and ownership transfers once.
    let inherited = unsafe { OwnedFd::from_raw_fd(inherited) };
    // SAFETY: The test deliberately makes this descriptor inheritable by the
    // launcher; the production launcher's default-close spawn must remove it
    // from the worker image.
    let result = unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_SETFD, 0) };
    assert_eq!(result, 0);
    let descriptor_path = format!("/dev/fd/{}", inherited.as_raw_fd());
    let output = run_launcher(
        &bundle,
        &[
            OsStr::new("--config-file"),
            OsStr::new(&descriptor_path),
            OsStr::new("--no-api"),
        ],
    );
    assert_eq!(output.status.code(), Some(BAD_CONFIGURATION_EXIT_CODE));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read config file"),
        "closed descriptor should fail at read: {stderr}"
    );
    assert!(
        !stderr.contains("missing required section"),
        "worker must not read inherited fixture contents: {stderr}"
    );
    assert!(!stderr.contains(&descriptor_path));
}

#[test]
fn worker_rejects_malformed_forged_bootstrap_before_public_processing() {
    let bundle = production_bundle();
    let (mut parent, child_endpoint) =
        UnixStream::pair().expect("bootstrap socketpair should open");
    let (_grant_parent, grant_child_endpoint) =
        UnixDatagram::pair().expect("grant socketpair should open");
    parent
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("bootstrap read timeout should set");
    let child_fd = child_endpoint.as_raw_fd();
    let grant_child_fd = grant_child_endpoint.as_raw_fd();
    let mut command = Command::new(worker_executable(&bundle));
    command
        .env(SESSION_ENV_KEY, SESSION_ENV_VALUE)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: The closure performs only async-signal-safe `dup2` before exec,
    // captures one raw descriptor kept live through spawn, and reports failure
    // through `io::Error` without touching shared Rust state.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(child_fd, SESSION_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(grant_child_fd, GRANT_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("forged worker should execute");
    let stdout_reader = read_stream(child.stdout.take().expect("stdout should be piped"));
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    drop(child_endpoint);
    drop(grant_child_endpoint);

    let mut hello_bytes = vec![0_u8; 56];
    parent
        .read_exact(&mut hello_bytes)
        .expect("fixed bootstrap hello should arrive");
    let mut decoder = FrameDecoder::default();
    decoder.push(&hello_bytes).expect("hello should be bounded");
    let hello = decoder
        .next_frame()
        .expect("hello should decode")
        .expect("hello should be complete");
    assert_eq!(hello.message, Message::Hello);
    assert_eq!(hello.session, SessionId::pre_session());

    let mut malformed = encode_frame(Frame {
        session: SessionId::from_bytes([7; 32]),
        sequence: 0,
        message: Message::Start,
    })
    .expect("start frame should encode");
    malformed[4..6].copy_from_slice(&1_u16.to_be_bytes());
    parent
        .write_all(&malformed)
        .expect("malformed bootstrap should write");
    let status = wait_child_with_timeout(child, PROCESS_TIMEOUT, "malformed bootstrap worker");
    let stdout = stdout_reader.join().expect("stdout reader should join");
    let stderr = stderr_reader.join().expect("stderr reader should join");
    assert_eq!(status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert!(
        stdout.is_empty(),
        "public readiness must not be emitted: {stdout}"
    );
    assert_eq!(stderr, "bangbang: private launcher session failed\n");
    assert!(!stderr.contains("BBS1") && !stderr.contains("session-"));
}

#[test]
fn launcher_first_and_both_killed_orders_follow_namespace_ownership() {
    let bundle = production_bundle();
    recover_session_root(&bundle);

    let mut launcher_first = spawn_ready_api_launcher(&bundle, "launcher-first");
    let worker_pid = only_worker_pid(&launcher_first.child);
    let worker_exit = ProcessExitWatch::new(worker_pid);
    assert_eq!(session_entries().len(), 1);
    let launcher_pid = i32::try_from(launcher_first.child.id()).expect("launcher PID should fit");
    // SAFETY: This targets the one owned launcher while its unreaped Child
    // prevents PID reuse. The worker remains alive to observe socket EOF.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGKILL) }, 0);
    let launcher_status = launcher_first.wait("launcher-first SIGKILL");
    assert_eq!(launcher_status.signal(), Some(libc::SIGKILL));
    assert!(
        worker_exit.wait(PROCESS_TIMEOUT),
        "worker should exit after launcher EOF"
    );
    assert!(session_entries().is_empty());
    assert!(!launcher_first.socket.exists());

    let mut both_killed = spawn_ready_api_launcher(&bundle, "both-killed");
    assert_eq!(session_entries().len(), 1);
    kill_child_group(&mut both_killed.child);
    let status = both_killed.wait("both processes SIGKILL");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(
        session_entries().len(),
        1,
        "both-killed residue should remain locked only until kernel teardown"
    );
    let _ = fs::remove_file(&both_killed.socket);

    let recovery = run_launcher(&bundle, &[OsStr::new("--help")]);
    assert_output_success(&recovery, "both-killed stale recovery");
    assert!(session_entries().is_empty());
}

#[test]
fn concurrent_sessions_remain_independent_when_one_worker_crashes() {
    let bundle = production_bundle();
    recover_session_root(&bundle);
    let mut first = spawn_ready_api_launcher(&bundle, "concurrent-first");
    let mut second = spawn_ready_api_launcher(&bundle, "concurrent-second");
    assert_eq!(session_entries().len(), 2);
    assert!(http_get(&first.socket, "/").starts_with("HTTP/1.1 200 "));
    assert!(http_get(&second.socket, "/").starts_with("HTTP/1.1 200 "));

    let first_worker = only_worker_pid(&first.child);
    // SAFETY: `first_worker` is the live child of the unreaped first launcher.
    assert_eq!(unsafe { libc::kill(first_worker, libc::SIGKILL) }, 0);
    let first_status = first.wait("first concurrent worker SIGKILL");
    assert_eq!(first_status.signal(), None);
    assert_eq!(first_status.code(), Some(128 + libc::SIGKILL));
    assert_eq!(session_entries().len(), 1);
    assert!(http_get(&second.socket, "/").starts_with("HTTP/1.1 200 "));

    let second_pid = i32::try_from(second.child.id()).expect("launcher PID should fit");
    // SAFETY: `second_pid` is the live unreaped second launcher.
    assert_eq!(unsafe { libc::kill(second_pid, libc::SIGTERM) }, 0);
    let second_status = second.wait("second concurrent graceful stop");
    assert!(second_status.success());
    assert!(session_entries().is_empty());
    let _ = fs::remove_file(&first.socket);
    assert!(!second.socket.exists());
}

fn run_graceful_signal_case(signal: i32, name: &str) {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    let socket = container_tmp_dir().join(format!(
        "bb-production-{}-{}-{name}.sock",
        std::process::id(),
        NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let mut child = Command::new(launcher(&bundle))
        .args(["--api-sock", path_text(&socket), "--id"])
        .arg(format!("production-{name}-{}", std::process::id()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("production launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(err) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!("worker should publish API readiness: {err}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }

    let response = http_get(&socket, "/");
    assert!(
        response.starts_with("HTTP/1.1 200 "),
        "response:\n{response}"
    );
    assert!(response.contains(r#""state":"Not started""#));

    let pid = i32::try_from(child.id()).expect("launcher PID should fit");
    // SAFETY: `pid` is the live owned launcher and `signal` is SIGINT or
    // SIGTERM for this test case.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    let status = wait_child_with_timeout(child, PROCESS_TIMEOUT, name);
    let stdout = stdout_reader.join().expect("stdout reader should join");
    let stderr = stderr_reader.join().expect("stderr reader should join");
    assert!(
        status.success(),
        "{name} should stop launcher and worker successfully; status: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !socket.exists(),
        "{name} should remove the owned API socket"
    );
}

#[derive(Debug)]
struct StartupGrantFixture {
    _root: TestDir,
    config: PathBuf,
    metadata: PathBuf,
    kernel: PathBuf,
    initrd: PathBuf,
    drive_read_only: PathBuf,
    drive_read_write: PathBuf,
    pmem_read_only: PathBuf,
    pmem_read_write: PathBuf,
    manifest: PathBuf,
    metadata_marker: String,
}

impl StartupGrantFixture {
    fn new(bundle: &Path, case: &str) -> Self {
        let root = TestDir::new(&format!("startup-grant-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("startup grant root should canonicalize");
        let config = canonical_root.join("external-config.json");
        let metadata = canonical_root.join("external-metadata.json");
        let kernel = canonical_root.join("external-kernel");
        let initrd = canonical_root.join("external-initrd");
        let drive_read_only = canonical_root.join("external-drive-read-only.img");
        let drive_read_write = canonical_root.join("external-drive-read-write.img");
        let pmem_read_only = canonical_root.join("external-pmem-read-only.img");
        let pmem_read_write = canonical_root.join("external-pmem-read-write.img");
        let manifest = canonical_root.join("grant-manifest.json");
        let metadata_marker = format!("startup-grant-metadata-{case}");
        let resources = worker_bundle(bundle).join("Contents/Resources");
        fs::copy(resources.join("guest-kernel"), &kernel)
            .expect("external kernel fixture should copy");
        fs::copy(resources.join("guest-initrd"), &initrd)
            .expect("external initrd fixture should copy");
        create_sized_file(&drive_read_only, 512);
        create_sized_file(&drive_read_write, 512);
        create_sized_file(&pmem_read_only, PMEM_BACKING_LEN);
        create_sized_file(&pmem_read_write, PMEM_BACKING_LEN);
        fs::write(
            &metadata,
            serde_json::to_vec(&serde_json::json!({"grant-proof": metadata_marker}))
                .expect("metadata fixture should serialize"),
        )
        .expect("external metadata fixture should write");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "machine-config": {"vcpu_count": 1, "mem_size_mib": 256},
                "boot-source": {
                    "kernel_image_path": KERNEL_REF,
                    "initrd_path": INITRD_REF,
                    "boot_args": "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init",
                },
                "drives": [
                    {
                        "drive_id": "grant_ro",
                        "path_on_host": STARTUP_DRIVE_RO_REF,
                        "is_root_device": false,
                        "is_read_only": true,
                    },
                    {
                        "drive_id": "grant_rw",
                        "path_on_host": STARTUP_DRIVE_RW_REF,
                        "is_root_device": false,
                        "is_read_only": false,
                    },
                ],
                "pmem": [
                    {
                        "id": "grant_pmem_ro",
                        "path_on_host": STARTUP_PMEM_RO_REF,
                        "read_only": true,
                    },
                    {
                        "id": "grant_pmem_rw",
                        "path_on_host": STARTUP_PMEM_RW_REF,
                        "read_only": false,
                    },
                ],
            }))
            .expect("config fixture should serialize"),
        )
        .expect("external config fixture should write");
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": STARTUP_CONFIG_ID,
                    "role": "startup-config",
                    "access": "read-only",
                    "source": path_text(&config),
                },
                {
                    "id": STARTUP_METADATA_ID,
                    "role": "startup-metadata",
                    "access": "read-only",
                    "source": path_text(&metadata),
                },
                {
                    "id": KERNEL_ID,
                    "role": "kernel-image",
                    "access": "read-only",
                    "source": path_text(&kernel),
                },
                {
                    "id": INITRD_ID,
                    "role": "initrd-image",
                    "access": "read-only",
                    "source": path_text(&initrd),
                },
                {
                    "id": STARTUP_DRIVE_RO_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&drive_read_only),
                },
                {
                    "id": STARTUP_DRIVE_RW_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&drive_read_write),
                },
                {
                    "id": STARTUP_PMEM_RO_ID,
                    "role": "pmem-backing",
                    "access": "read-only",
                    "source": path_text(&pmem_read_only),
                },
                {
                    "id": STARTUP_PMEM_RW_ID,
                    "role": "pmem-backing",
                    "access": "read-write",
                    "source": path_text(&pmem_read_write),
                },
            ],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("grant manifest should serialize"),
        )
        .expect("startup grant manifest should write");

        Self {
            _root: root,
            config,
            metadata,
            kernel,
            initrd,
            drive_read_only,
            drive_read_write,
            pmem_read_only,
            pmem_read_write,
            manifest,
            metadata_marker,
        }
    }

    fn replace_boot_pathnames(&mut self) {
        let kernel_original = self
            .kernel
            .parent()
            .expect("kernel path should have parent")
            .join("opened-kernel");
        let initrd_original = self
            .initrd
            .parent()
            .expect("initrd path should have parent")
            .join("opened-initrd");
        fs::rename(&self.kernel, kernel_original).expect("opened kernel path should move");
        fs::rename(&self.initrd, initrd_original).expect("opened initrd path should move");
        fs::write(&self.kernel, b"replacement kernel must not boot")
            .expect("replacement kernel should write");
        fs::write(&self.initrd, b"replacement initrd must not boot")
            .expect("replacement initrd should write");
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.config),
            path_text(&self.metadata),
            path_text(&self.kernel),
            path_text(&self.initrd),
            path_text(&self.drive_read_only),
            path_text(&self.drive_read_write),
            path_text(&self.pmem_read_only),
            path_text(&self.pmem_read_write),
            path_text(&self.manifest),
            STARTUP_CONFIG_ID,
            STARTUP_METADATA_ID,
            KERNEL_ID,
            INITRD_ID,
            STARTUP_DRIVE_RO_ID,
            STARTUP_DRIVE_RW_ID,
            STARTUP_PMEM_RO_ID,
            STARTUP_PMEM_RW_ID,
            STARTUP_CONFIG_REF,
            STARTUP_METADATA_REF,
            KERNEL_REF,
            INITRD_REF,
            STARTUP_DRIVE_RO_REF,
            STARTUP_DRIVE_RW_REF,
            STARTUP_PMEM_RO_REF,
            STARTUP_PMEM_RW_REF,
            &self.metadata_marker,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn assert_output_redacted(&self, output: &Output) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for sensitive in self.sensitive_strings() {
            assert!(
                !stdout.contains(&sensitive),
                "stdout leaked startup grant data"
            );
            assert!(
                !stderr.contains(&sensitive),
                "stderr leaked startup grant data"
            );
        }
    }
}

#[derive(Debug)]
struct GuestDeviceGrantFixture {
    _root: TestDir,
    rootfs: PathBuf,
    data: PathBuf,
    replacement: PathBuf,
    pmem: PathBuf,
    read_only_data: PathBuf,
    opened_rootfs: PathBuf,
    opened_data: PathBuf,
    opened_replacement: PathBuf,
    opened_pmem: PathBuf,
    opened_read_only_data: PathBuf,
    manifest: PathBuf,
}

impl GuestDeviceGrantFixture {
    fn new(case: &str) -> Self {
        let root = TestDir::new(&format!("device-grant-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("device grant root should canonicalize");
        let rootfs = canonical_root.join("external-rootfs.ext4");
        let data = canonical_root.join("external-data.img");
        let replacement = canonical_root.join("external-replacement.img");
        let pmem = canonical_root.join("external-pmem.img");
        let read_only_data = canonical_root.join("external-read-only-data.img");
        let opened_rootfs = canonical_root.join("opened-rootfs.ext4");
        let opened_data = canonical_root.join("opened-data.img");
        let opened_replacement = canonical_root.join("opened-replacement.img");
        let opened_pmem = canonical_root.join("opened-pmem.img");
        let opened_read_only_data = canonical_root.join("opened-read-only-data.img");
        let manifest = canonical_root.join("grant-manifest.json");

        fs::copy(guest_ext4_rootfs(), &rootfs).expect("external rootfs fixture should copy");
        create_sized_file(&data, 512);
        create_sized_file(&replacement, 512);
        create_pmem_file(&pmem, PMEM_HOST_MARKER);
        create_sized_file(&read_only_data, 512);

        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": GUEST_ROOTFS_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&rootfs),
                },
                {
                    "id": GUEST_DATA_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&data),
                },
                {
                    "id": GUEST_REPLACEMENT_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&replacement),
                },
                {
                    "id": GUEST_PMEM_ID,
                    "role": "pmem-backing",
                    "access": "read-write",
                    "source": path_text(&pmem),
                },
                {
                    "id": GUEST_READ_ONLY_DATA_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&read_only_data),
                },
            ],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("device grant manifest should serialize"),
        )
        .expect("device grant manifest should write");

        Self {
            _root: root,
            rootfs,
            data,
            replacement,
            pmem,
            read_only_data,
            opened_rootfs,
            opened_data,
            opened_replacement,
            opened_pmem,
            opened_read_only_data,
            manifest,
        }
    }

    fn replace_source_pathnames(&self) {
        for (source, opened) in [
            (&self.rootfs, &self.opened_rootfs),
            (&self.data, &self.opened_data),
            (&self.replacement, &self.opened_replacement),
            (&self.pmem, &self.opened_pmem),
            (&self.read_only_data, &self.opened_read_only_data),
        ] {
            fs::rename(source, opened).expect("launcher-opened source should move");
        }
        create_sized_file(&self.rootfs, 512);
        create_sized_file(&self.data, 512);
        create_sized_file(&self.replacement, 512);
        create_sized_file(&self.pmem, PMEM_BACKING_LEN);
        create_sized_file(&self.read_only_data, 512);
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.rootfs),
            path_text(&self.data),
            path_text(&self.replacement),
            path_text(&self.pmem),
            path_text(&self.read_only_data),
            path_text(&self.opened_rootfs),
            path_text(&self.opened_data),
            path_text(&self.opened_replacement),
            path_text(&self.opened_pmem),
            path_text(&self.opened_read_only_data),
            path_text(&self.manifest),
            GUEST_ROOTFS_ID,
            GUEST_DATA_ID,
            GUEST_REPLACEMENT_ID,
            GUEST_PMEM_ID,
            GUEST_READ_ONLY_DATA_ID,
            GUEST_ROOTFS_REF,
            GUEST_DATA_REF,
            GUEST_REPLACEMENT_REF,
            GUEST_PMEM_REF,
            GUEST_READ_ONLY_DATA_REF,
            std::str::from_utf8(PMEM_HOST_MARKER).expect("pmem marker should be UTF-8"),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

fn assert_private_grant_fault(response: &str, fixture: &StartupGrantFixture) {
    assert_redacted_private_grant_fault(response, fixture.sensitive_strings());
    assert!(!response.contains("bangbang-grant:missing"));
}

fn assert_device_private_grant_fault(response: &str, fixture: &GuestDeviceGrantFixture) {
    assert_redacted_private_grant_fault(response, fixture.sensitive_strings());
}

fn assert_redacted_private_grant_fault(
    response: &str,
    sensitive_strings: impl IntoIterator<Item = String>,
) {
    assert!(
        response.starts_with("HTTP/1.1 400 "),
        "response:\n{response}"
    );
    assert!(response.contains(r#"{"fault_message":"private resource grant failed"}"#));
    for sensitive in sensitive_strings {
        assert!(
            !response.contains(&sensitive),
            "grant fault leaked private data"
        );
    }
}

#[derive(Debug)]
struct GrantProbeFixture {
    _root: TestDir,
    read: PathBuf,
    write: PathBuf,
    directory: PathBuf,
    manifest: PathBuf,
    outside: PathBuf,
    case: String,
    initial_write: Vec<u8>,
}

impl GrantProbeFixture {
    fn new(case: &str, mismatched_read_role: bool) -> Self {
        let root = TestDir::new(&format!("grant-{case}"));
        let canonical_root = fs::canonicalize(root.path()).expect("grant root should canonicalize");
        let read = canonical_root.join("read.input");
        let write = canonical_root.join("write.output");
        let directory = canonical_root.join("authorized-directory");
        let manifest = canonical_root.join("grant-manifest.json");
        let outside = canonical_root.join(GRANT_PROBE_OUTSIDE);
        let expected_read = Self::expected_read(case);
        let expected_write = Self::expected_write(case);
        let initial_write = vec![b'?'; expected_write.len()];
        fs::write(&read, expected_read).expect("grant read fixture should be written");
        fs::write(&write, &initial_write).expect("grant write fixture should be written");
        fs::create_dir(&directory).expect("grant directory should be created");
        fs::write(&outside, b"outside-authority\n").expect("outside fixture should be written");

        let read_role = if mismatched_read_role {
            "initrd-image"
        } else {
            "kernel-image"
        };
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": format!("probe-read-{case}"),
                    "role": read_role,
                    "access": "read-only",
                    "source": path_text(&read),
                },
                {
                    "id": format!("probe-write-{case}"),
                    "role": "logger-sink",
                    "access": "write-only",
                    "source": path_text(&write),
                },
                {
                    "id": format!("probe-dir-{case}"),
                    "role": "api-socket-directory",
                    "access": "create-children",
                    "source": path_text(&directory),
                }
            ]
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("grant manifest should serialize"),
        )
        .expect("grant manifest should be written");
        Self {
            _root: root,
            read,
            write,
            directory,
            manifest,
            outside,
            case: case.to_owned(),
            initial_write,
        }
    }

    fn expected_read(case: &str) -> Vec<u8> {
        format!("bangbang-grant-read-{case}\n").into_bytes()
    }

    fn expected_write(case: &str) -> Vec<u8> {
        format!("bangbang-grant-write-{case}\n").into_bytes()
    }

    fn child(&self) -> PathBuf {
        self.directory
            .join(format!("bangbang-grant-{}.out", self.case))
    }

    fn assert_unmodified(&self) {
        assert_eq!(
            fs::read(&self.read).expect("read fixture should remain readable"),
            Self::expected_read(&self.case)
        );
        assert_eq!(
            fs::read(&self.write).expect("write fixture should remain readable"),
            self.initial_write
        );
        assert!(!self.child().exists());
        assert_eq!(
            fs::read(&self.outside).expect("outside fixture should remain readable"),
            b"outside-authority\n"
        );
    }

    fn assert_completed(&self) {
        assert_eq!(
            fs::read(&self.read).expect("read fixture should remain readable"),
            Self::expected_read(&self.case)
        );
        assert_eq!(
            fs::read(&self.write).expect("granted write should be readable by host"),
            Self::expected_write(&self.case)
        );
        assert_eq!(
            fs::read(self.child()).expect("granted child should be readable by host"),
            Self::expected_write(&self.case)
        );
        assert_eq!(
            fs::read(&self.outside).expect("outside fixture should remain readable"),
            b"outside-authority\n"
        );
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            &self.read,
            &self.write,
            &self.directory,
            &self.manifest,
            &self.outside,
        ]
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .chain([
            format!("probe-read-{}", self.case),
            format!("probe-write-{}", self.case),
            format!("probe-dir-{}", self.case),
            String::from_utf8(Self::expected_read(&self.case))
                .expect("expected read should be UTF-8"),
            String::from_utf8(Self::expected_write(&self.case))
                .expect("expected write should be UTF-8"),
        ])
        .collect()
    }
}

fn grant_probe_command(bundle: &Path, fixture: &GrantProbeFixture, case: &str) -> Command {
    let mut command = Command::new(launcher(bundle));
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .arg(GRANT_PROBE_OPTION)
        .arg(case);
    command
}

fn grant_delay_command(bundle: &Path, fixture: &GrantProbeFixture) -> Command {
    let mut command = Command::new(launcher(bundle));
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .arg(GRANT_DELAY_OPTION);
    command
}

fn run_grant_probe(bundle: &Path, fixture: &GrantProbeFixture, case: &str) -> Output {
    run_with_timeout(
        &mut grant_probe_command(bundle, fixture, case),
        PROCESS_TIMEOUT,
        "signed grant probe",
    )
}

fn assert_grant_output_redacted(output: &Output, fixture: &GrantProbeFixture) {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for sensitive in fixture.sensitive_strings() {
        assert!(
            !combined.contains(&sensitive),
            "grant diagnostics must redact sensitive input"
        );
    }
}

#[derive(Debug)]
struct HoldingGrantProbe {
    child: Child,
    stdout_reader: Option<JoinHandle<String>>,
    stderr_reader: Option<JoinHandle<String>>,
    sensitive: Vec<String>,
    completed: bool,
}

impl HoldingGrantProbe {
    fn wait(&mut self, context: &str) -> ExitStatus {
        let status = if wait_for_child_exit(&self.child, PROCESS_TIMEOUT) {
            self.child
                .wait()
                .expect("grant launcher wait should succeed")
        } else {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
            panic!("timed out waiting for {context}");
        };
        self.completed = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("grant stdout reader should exist")
            .join()
            .expect("grant stdout reader should join");
        let stderr = self
            .stderr_reader
            .take()
            .expect("grant stderr reader should exist")
            .join()
            .expect("grant stderr reader should join");
        let combined = format!("{stdout}{stderr}");
        for sensitive in &self.sensitive {
            assert!(!combined.contains(sensitive));
        }
        status
    }

    fn stop(&mut self, signal: i32, context: &str) {
        let pid = i32::try_from(self.child.id()).expect("grant launcher PID should fit");
        // SAFETY: The unreaped launcher owns this PID and signal is fixed by the test.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
        let status = self.wait(context);
        assert!(status.success(), "{context} should stop successfully");
    }
}

impl Drop for HoldingGrantProbe {
    fn drop(&mut self) {
        if !self.completed {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

fn spawn_holding_grant_probe(
    bundle: &Path,
    fixture: &GrantProbeFixture,
    case: &str,
) -> HoldingGrantProbe {
    let mut command = grant_probe_command(bundle, fixture, case);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("holding grant probe should start");
    let (ready, stdout_reader) = read_stdout_until_line(&mut child, GRANT_PROBE_READY);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!("grant probe should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
    HoldingGrantProbe {
        child,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: fixture.sensitive_strings(),
        completed: false,
    }
}

fn spawn_holding_grant_delay(bundle: &Path, fixture: &GrantProbeFixture) -> HoldingGrantProbe {
    let mut command = grant_delay_command(bundle, fixture);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("delayed grant probe should start");
    let (ready, stdout_reader) = read_stdout_until_line(&mut child, GRANT_DELAY_READY);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "delayed grant phase should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    HoldingGrantProbe {
        child,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: fixture.sensitive_strings(),
        completed: false,
    }
}

#[derive(Debug)]
struct RunningApiLauncher {
    child: Child,
    socket: PathBuf,
    stdout_reader: Option<JoinHandle<String>>,
    stderr_reader: Option<JoinHandle<String>>,
    sensitive: Vec<String>,
    completed: bool,
}

impl RunningApiLauncher {
    fn wait(&mut self, context: &str) -> ExitStatus {
        let status = if wait_for_child_exit(&self.child, PROCESS_TIMEOUT) {
            self.child.wait().expect("launcher wait should succeed")
        } else {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
            panic!("timed out waiting for {context}");
        };
        self.completed = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("stdout reader should exist")
            .join()
            .expect("stdout reader should join");
        let stderr = self
            .stderr_reader
            .take()
            .expect("stderr reader should exist")
            .join()
            .expect("stderr reader should join");
        assert!(
            !stderr.contains("session-debug"),
            "private diagnostics must stay absent\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        for sensitive in &self.sensitive {
            assert!(
                !stdout.contains(sensitive),
                "stdout leaked startup grant data"
            );
            assert!(
                !stderr.contains(sensitive),
                "stderr leaked startup grant data"
            );
        }
        status
    }
}

impl Drop for RunningApiLauncher {
    fn drop(&mut self) {
        if !self.completed {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

fn spawn_ready_api_launcher(bundle: &Path, name: &str) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbp-{:x}-{test_id:x}.sock", std::process::id(),));
    let mut child = Command::new(launcher(bundle))
        .args(["--api-sock", path_text(&socket), "--id"])
        .arg(format!("{name}-{}", std::process::id()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("production launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!("{name} should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
    RunningApiLauncher {
        child,
        socket,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: Vec::new(),
        completed: false,
    }
}

fn spawn_ready_startup_grant_api_launcher(
    bundle: &Path,
    fixture: &StartupGrantFixture,
    consume_metadata: bool,
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbg-{:x}-{test_id:x}.sock", std::process::id(),));
    let mut command = Command::new(launcher(bundle));
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .args(["--api-sock", path_text(&socket)])
        .args(["--id", &format!("grant-{test_id}")]);
    if consume_metadata {
        command.args(["--metadata", STARTUP_METADATA_REF]);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("startup-grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "startup-grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningApiLauncher {
        child,
        socket,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: fixture.sensitive_strings(),
        completed: false,
    }
}

fn spawn_ready_device_grant_api_launcher(
    bundle: &Path,
    fixture: &GuestDeviceGrantFixture,
    name: &str,
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbd-{:x}-{test_id:x}.sock", std::process::id(),));
    let mut child = Command::new(launcher(bundle))
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .args(["--api-sock", path_text(&socket)])
        .args(["--id", &format!("{name}-{test_id}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("device-grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "device-grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningApiLauncher {
        child,
        socket,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: fixture.sensitive_strings(),
        completed: false,
    }
}

fn recover_session_root(bundle: &Path) {
    let output = run_launcher(bundle, &[OsStr::new("--help")]);
    assert_output_success(&output, "session-root recovery");
    assert!(
        session_entries().is_empty(),
        "session root should start empty"
    );
}

fn session_root() -> PathBuf {
    container_tmp_dir().join("bangbang-sessions-v1")
}

fn session_entries() -> Vec<PathBuf> {
    let root = session_root();
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .expect("session root should be readable")
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"session-")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn only_worker_pid(launcher: &Child) -> libc::pid_t {
    let parent = libc::pid_t::try_from(launcher.id()).expect("launcher PID should fit");
    let mut pids = [0 as libc::pid_t; 16];
    let buffer_bytes =
        i32::try_from(std::mem::size_of_val(&pids)).expect("child PID buffer should fit");
    // SAFETY: `pids` is writable for `buffer_bytes`, and the launcher remains
    // live and unreaped while libproc takes this synchronous snapshot.
    let returned =
        unsafe { libc::proc_listchildpids(parent, pids.as_mut_ptr().cast(), buffer_bytes) };
    assert!(returned > 0, "launcher should own one worker");
    let count = usize::try_from(returned).expect("libproc child count should fit");
    let children = pids
        .get(..count)
        .expect("libproc count should fit buffer")
        .iter()
        .copied()
        .filter(|pid| *pid > 0)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1, "launcher should own exactly one worker");
    children[0]
}

#[derive(Debug)]
struct ProcessExitWatch {
    queue: OwnedFd,
    pid: usize,
}

impl ProcessExitWatch {
    fn new(pid: libc::pid_t) -> Self {
        // SAFETY: `kqueue` returns a fresh descriptor on success.
        let queue = unsafe { libc::kqueue() };
        assert!(queue >= 0, "process watch kqueue should open");
        // SAFETY: `queue` is a fresh owned descriptor.
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        let pid = usize::try_from(pid).expect("watched PID should fit");
        let change = libc::kevent {
            ident: pid,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: `change` is one initialized registration and no output is requested.
        let result = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "process exit watch should register");
        Self { queue, pid }
    }

    fn wait(self, timeout: Duration) -> bool {
        let deadline = Instant::now()
            .checked_add(timeout)
            .expect("process-watch deadline should fit Instant");
        let mut event = MaybeUninit::<libc::kevent>::uninit();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout = libc::timespec {
                tv_sec: libc::time_t::try_from(remaining.as_secs())
                    .expect("timeout seconds should fit"),
                tv_nsec: libc::c_long::from(remaining.subsec_nanos()),
            };
            // SAFETY: `event` has room for one result and `timeout` remains live.
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    event.as_mut_ptr(),
                    1,
                    &raw const timeout,
                )
            };
            if count == 1 {
                // SAFETY: One result was initialized above.
                let event = unsafe { event.assume_init() };
                return event.filter == libc::EVFILT_PROC
                    && event.ident == self.pid
                    && event.fflags & libc::NOTE_EXIT != 0;
            }
            if count == 0 {
                return false;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return false;
            }
        }
    }
}

fn initialize_worker_container(bundle: &Path) {
    let output = run_launcher(bundle, &[OsStr::new("--help")]);
    assert_output_success(&output, "worker container initialization");
    fs::create_dir_all(container_tmp_dir()).expect("worker container tmp should exist");
}

fn container_tmp_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME should exist"))
        .join("Library/Containers")
        .join(WORKER_BUNDLE_IDENTIFIER)
        .join("Data/tmp")
}

fn read_stdout_until_ready(child: &mut Child) -> (Receiver<()>, JoinHandle<String>) {
    read_stdout_until_line(child, "status: API server listening")
}

fn read_stdout_until_line(
    child: &mut Child,
    expected_line: &'static str,
) -> (Receiver<()>, JoinHandle<String>) {
    let stdout = child.stdout.take().expect("stdout should be piped");
    let (ready_sender, ready_receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut collected = String::new();
        let mut ready_sender = Some(ready_sender);
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("launcher stdout should be readable");
            if line == expected_line
                && let Some(sender) = ready_sender.take()
            {
                let _ = sender.send(());
            }
            collected.push_str(&line);
            collected.push('\n');
        }
        collected
    });
    (ready_receiver, reader)
}

fn read_stream<R>(mut stream: R) -> JoinHandle<String>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = String::new();
        stream
            .read_to_string(&mut output)
            .expect("child stream should be readable");
        output
    })
}

fn wait_child_with_timeout(mut child: Child, timeout: Duration, context: &str) -> ExitStatus {
    if wait_for_child_exit(&child, timeout) {
        return child.wait().expect("launcher wait should succeed");
    }
    kill_child_group(&mut child);
    let _ = child.wait();
    panic!("timed out waiting for {context}");
}

fn run_with_timeout(command: &mut Command, timeout: Duration, context: &str) -> Output {
    let mut child = command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bounded command should start");
    let stdout = read_stream(child.stdout.take().expect("stdout should be piped"));
    let stderr = read_stream(child.stderr.take().expect("stderr should be piped"));
    let status = wait_child_with_timeout(child, timeout, context);
    Output {
        status,
        stdout: stdout
            .join()
            .expect("stdout reader should join")
            .into_bytes(),
        stderr: stderr
            .join()
            .expect("stderr reader should join")
            .into_bytes(),
    }
}

fn wait_for_child_exit(child: &Child, timeout: Duration) -> bool {
    // SAFETY: `kqueue` has no pointer arguments and returns a fresh descriptor
    // on success, which is transferred immediately into `OwnedFd`.
    let descriptor = unsafe { libc::kqueue() };
    assert!(descriptor >= 0, "test kqueue should be created");
    // SAFETY: `descriptor` is the fresh owned descriptor returned above.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let child_id = usize::try_from(child.id()).expect("launcher PID should fit");
    let change = libc::kevent {
        ident: child_id,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: `change` is one initialized registration event and no result
    // buffer is requested by this call.
    let registered = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &raw const change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    assert_eq!(registered, 0, "child exit event should register");

    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("test timeout should fit Instant");
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = libc::timespec {
            tv_sec: libc::time_t::try_from(remaining.as_secs())
                .expect("timeout seconds should fit"),
            tv_nsec: libc::c_long::from(remaining.subsec_nanos()),
        };
        let mut event = MaybeUninit::<libc::kevent>::uninit();
        // SAFETY: `event` has room for one result and is read only when the
        // kernel reports that it initialized exactly one entry.
        let count = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                &raw const timeout,
            )
        };
        if count == 1 {
            // SAFETY: `kevent` reported one initialized result above.
            let event = unsafe { event.assume_init() };
            let event_filter = event.filter;
            let event_ident = event.ident;
            let event_fflags = event.fflags;
            assert_eq!(event_filter, libc::EVFILT_PROC);
            assert_eq!(event_ident, child_id);
            assert_ne!(event_fflags & libc::NOTE_EXIT, 0);
            return true;
        }
        if count == 0 {
            return false;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            panic!("waiting for child exit failed: {error:?}");
        }
    }
}

fn kill_child_group(child: &mut Child) {
    let pid = i32::try_from(child.id()).expect("launcher PID should fit");
    // SAFETY: Test children are leaders of fresh process groups. The leader
    // remains unreaped here, so its PID/group id cannot be reused while
    // SIGKILL bounds both launcher and nested worker cleanup.
    let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
}

fn stop_running_launcher(running: &mut RunningApiLauncher, context: &str) {
    let pid = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: `pid` is the live unreaped launcher owned by `running`.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let status = running.wait(context);
    assert!(
        status.success(),
        "{context} should stop cleanly: {status:?}"
    );
    assert!(
        !running.socket.exists(),
        "{context} should remove the API socket"
    );
}

fn create_sized_file(path: &Path, len: u64) {
    assert!(len > 0, "test backing length must be nonzero");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .expect("test backing should create");
    file.set_len(len).expect("test backing length should set");
}

fn create_pmem_file(path: &Path, marker: &[u8]) {
    create_sized_file(path, PMEM_BACKING_LEN);
    OpenOptions::new()
        .write(true)
        .open(path)
        .expect("pmem backing should reopen")
        .write_all(marker)
        .expect("pmem host marker should write");
}

fn file_bytes_at(path: &Path, offset: u64, len: usize) -> Vec<u8> {
    let mut file = fs::File::open(path).expect("test backing should open");
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))
        .expect("test backing should seek");
    let mut bytes = vec![0_u8; len];
    file.read_exact(&mut bytes)
        .expect("test backing bytes should read");
    bytes
}

fn wait_for_file_prefix(path: &Path, marker: &[u8], timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if fs::metadata(path).is_ok() && file_bytes_at(path, 0, marker.len()) == marker {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "timed out after {timeout:?} waiting for marker {:?}",
                String::from_utf8_lossy(marker)
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_file_contains(path: &Path, marker: &[u8], timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if fs::read(path).is_ok_and(|contents| {
            contents
                .windows(marker.len())
                .any(|window| window == marker)
        }) {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "timed out after {timeout:?} waiting for output marker {:?}",
                String::from_utf8_lossy(marker)
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn http_get(socket: &Path, path: &str) -> String {
    http_request(socket, "GET", path, "")
}

fn http_put(socket: &Path, path: &str, body: &str) -> String {
    http_request(socket, "PUT", path, body)
}

fn http_request(socket: &Path, method: &str, path: &str, body: &str) -> String {
    let mut stream = UnixStream::connect(socket).expect("API socket should accept connections");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("API read timeout should be configured");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .expect("HTTP request should be written");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("HTTP request write should close");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("HTTP response should be read");
    response
}

fn assert_http_status(response: &str, expected: u16, context: &str) {
    assert!(
        response.starts_with(&format!("HTTP/1.1 {expected} ")),
        "{context} returned an unexpected response:\n{response}"
    );
}

fn assert_invalid_bundle(output: Output) {
    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid production bundle entry")
            || stderr.contains("production bundle signature validation failed"),
        "expected stable package rejection; stderr:\n{stderr}"
    );
    assert!(!stdout.contains("Usage:\n  bangbang [OPTIONS]"));
    assert!(!stdout.contains("status: API server listening"));
}

fn resign_worker_and_outer(
    bundle: &Path,
    worker_entitlements: &[u8],
    worker_runtime: bool,
    outer_runtime: bool,
) {
    let entitlement_file = bundle
        .parent()
        .expect("test bundle should have a parent")
        .join("worker.entitlements.plist");
    fs::write(&entitlement_file, worker_entitlements)
        .expect("replacement entitlements should be written");
    let worker = worker_bundle(bundle);
    let mut worker_sign = Command::new("/usr/bin/codesign");
    worker_sign.args(["--force", "--sign", "-"]);
    if worker_runtime {
        worker_sign.args(["--options", "runtime"]);
    }
    let worker_sign = worker_sign
        .arg("--entitlements")
        .arg(&entitlement_file)
        .arg(&worker)
        .output()
        .expect("replacement worker signing should execute");
    assert_output_success(&worker_sign, "replacement worker signing");
    let mut outer_sign = Command::new("/usr/bin/codesign");
    outer_sign.args(["--force", "--sign", "-"]);
    if outer_runtime {
        outer_sign.args(["--options", "runtime"]);
    }
    let outer_sign = outer_sign
        .arg(bundle)
        .output()
        .expect("replacement outer signing should execute");
    assert_output_success(&outer_sign, "replacement outer signing");
}

fn codesign_display(path: &Path) -> String {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(path)
        .output()
        .expect("codesign display should execute");
    assert_output_success(&output, "codesign display");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn codesign_entitlements(path: &Path) -> String {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--entitlements", "-", "--xml"])
        .arg(path)
        .output()
        .expect("codesign entitlement display should execute");
    assert_output_success(&output, "codesign entitlement display");
    String::from_utf8(output.stdout).expect("entitlements should be UTF-8")
}

fn assert_output_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} should succeed; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    let metadata = fs::symlink_metadata(source).expect("source metadata should exist");
    assert!(!metadata.file_type().is_symlink());
    if metadata.is_file() {
        fs::copy(source, destination).expect("file should copy");
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
        )
        .expect("file permissions should copy");
        return;
    }
    assert!(metadata.is_dir());
    fs::create_dir(destination).expect("destination directory should be created");
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
    )
    .expect("directory permissions should copy");
    let mut entries = fs::read_dir(source)
        .expect("source directory should be readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("source entries should be readable");
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let entry_metadata =
            fs::symlink_metadata(&source_path).expect("entry metadata should exist");
        if entry_metadata.file_type().is_symlink() {
            let target = fs::read_link(&source_path).expect("symlink target should be readable");
            symlink(target, destination_path).expect("symlink should copy");
        } else {
            copy_tree(&source_path, &destination_path);
        }
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("test path should be UTF-8")
}

#[derive(Debug)]
struct TestDir(PathBuf);

#[derive(Debug)]
struct TestFilePath(PathBuf);

impl TestFilePath {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestFilePath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl TestDir {
    fn new(name: &str) -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "bangbang-production-e2e-{}-{id}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
