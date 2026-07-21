#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[path = "../../../tests/support/macos_virtual_block.rs"]
mod macos_virtual_block;
#[path = "../../../tests/support/vhost_user_block.rs"]
mod vhost_user_block;

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt as _, PermissionsExt, symlink};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_launcher::{
    JailerIsolationArgument, LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME,
    OUTER_BUNDLE_NAME, WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};
use bangbang_session::{
    BLOCK_CONTROL_BROKER_FD, Frame, FrameDecoder, GRANT_FD, Message, SESSION_ENV_KEY,
    SESSION_ENV_VALUE, SESSION_FD, SOCKET_BROKER_FD, SessionId, VHOST_USER_BROKER_FD, WorkerPolicy,
    encode_frame,
};
use macos_virtual_block::{MacosVirtualBlock, MacosVirtualBlockAccess};
use vhost_user_block::{VhostUserBlockBackend, VhostUserBlockBackendOptions};

const BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_BUNDLE_PATH";
const GRANT_TEST_BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_GRANT_TEST_BUNDLE_PATH";
const GUEST_EXT4_ROOTFS_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
const GRANT_MANIFEST_OPTION: &str = "--bangbang-grant-manifest";
const JAILER_OPTION: &str = "--bangbang-jailer-v1";
const GRANT_PROBE_OPTION: &str = "--bangbang-internal-grant-probe-v1";
const GRANT_PROBE_READY: &str = "status: grant integration probe ready";
const GRANT_DELAY_OPTION: &str = "--bangbang-internal-grant-delay-v1";
const GRANT_DELAY_READY: &str = "status: grant integration delay ready";
const GRANT_PROBE_MARKER: &str = "grant-integration-probe.enabled";
const GRANT_PROBE_OUTSIDE: &str = "bangbang-grant-probe-outside";
const BLOCK_CONTROL_GRANT_ID: &str = "probe-block-control";
const BLOCK_CONTROL_GRANT_REF: &str = "bangbang-grant:probe-block-control";
const BLOCK_CONTROL_INITIAL_MARKER: &[u8] = b"BANGBANG_BLOCK_CONTROL_INITIAL";
const BLOCK_CONTROL_WRITTEN_MARKER: &[u8] = b"BANGBANG_BLOCK_CONTROL_WRITTEN";
const BLOCK_CONTROL_WRITE_BLOCK: u64 = 8;
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
const GUEST_HOTPLUG_REUSE_ID: &str = "grant-guest-hotplug-reuse-1420";
const GUEST_PMEM_ID: &str = "grant-guest-pmem-1362";
const GUEST_PMEM_REUSE_ID: &str = "grant-guest-pmem-reuse-1421";
const GUEST_PMEM_ROOT_ID: &str = "grant-guest-pmem-root-1444";
const GUEST_READ_ONLY_DATA_ID: &str = "grant-guest-read-only-data-1362";
const GUEST_ROOTFS_REF: &str = "bangbang-grant:grant-guest-rootfs-1362";
const GUEST_DATA_REF: &str = "bangbang-grant:grant-guest-data-1362";
const GUEST_REPLACEMENT_REF: &str = "bangbang-grant:grant-guest-replacement-1362";
const GUEST_HOTPLUG_REUSE_REF: &str = "bangbang-grant:grant-guest-hotplug-reuse-1420";
const GUEST_PMEM_REF: &str = "bangbang-grant:grant-guest-pmem-1362";
const GUEST_PMEM_REUSE_REF: &str = "bangbang-grant:grant-guest-pmem-reuse-1421";
const GUEST_PMEM_ROOT_REF: &str = "bangbang-grant:grant-guest-pmem-root-1444";
const GUEST_READ_ONLY_DATA_REF: &str = "bangbang-grant:grant-guest-read-only-data-1362";
const GUEST_MISSING_REF: &str = "bangbang-grant:grant-guest-missing-1362";
const OUTPUT_LOGGER_ID: &str = "grant-logger-sink-1364";
const OUTPUT_METRICS_ID: &str = "grant-metrics-sink-1364";
const OUTPUT_SERIAL_ID: &str = "grant-serial-sink-1364";
const OUTPUT_LOGGER_REF: &str = "bangbang-grant:grant-logger-sink-1364";
const OUTPUT_METRICS_REF: &str = "bangbang-grant:grant-metrics-sink-1364";
const OUTPUT_SERIAL_REF: &str = "bangbang-grant:grant-serial-sink-1364";
const OUTPUT_MISSING_REF: &str = "bangbang-grant:grant-missing-sink-1364";
const OUTPUT_CONFIG_ID: &str = "grant-output-config-1364";
const OUTPUT_CONFIG_REF: &str = "bangbang-grant:grant-output-config-1364";
const OUTPUT_LOGGER_SEED: &[u8] = b"logger-seed\n";
const OUTPUT_METRICS_SEED: &[u8] = b"metrics-seed\n";
const OUTPUT_SERIAL_SEED: &[u8] = b"serial-seed\n";
const OUTPUT_REPLACEMENT: &[u8] = b"replacement-path-must-remain-unused\n";
const API_SOCKET_DIRECTORY_ID: &str = "grant-api-socket-directory-1365";
const VSOCK_SOCKET_DIRECTORY_ID: &str = "grant-vsock-socket-directory-1365";
const VHOST_USER_SOCKET_DIRECTORY_ID: &str = "grant-vhost-user-socket-directory-1449";
const API_SOCKET_CHILD: &str = "api-1365.sock";
const VSOCK_SOCKET_CHILD: &str = "vsock-1365.sock";
const VHOST_USER_SOCKET_CHILD_ONE: &str = "vhost-one.sock";
const VHOST_USER_SOCKET_CHILD_TWO: &str = "vhost-two.sock";
const VHOST_USER_SOCKET_CHILD_THREE: &str = "vhost-three.sock";
const API_SOCKET_REF: &str = "bangbang-grant:grant-api-socket-directory-1365/api-1365.sock";
const VSOCK_SOCKET_REF: &str = "bangbang-grant:grant-vsock-socket-directory-1365/vsock-1365.sock";
const VHOST_USER_SOCKET_REF_ONE: &str =
    "bangbang-grant:grant-vhost-user-socket-directory-1449/vhost-one.sock";
const VHOST_USER_SOCKET_REF_TWO: &str =
    "bangbang-grant:grant-vhost-user-socket-directory-1449/vhost-two.sock";
const VHOST_USER_SOCKET_REF_THREE: &str =
    "bangbang-grant:grant-vhost-user-socket-directory-1449/vhost-three.sock";
const CONTAINED_VHOST_USER_HOST_MARKER: &[u8] = b"BANGBANG_VHOST_USER_BLOCK_HOST";
const CONTAINED_VHOST_USER_SUCCESS_MARKER: &[u8] = b"BANGBANG_VHOST_USER_BLOCK_ro_OK";
const VHOST_CONFIG_RESIZED_MARKER: &[u8] = b"BANGBANG_VHOST_CONFIG_RESIZED";
const SNAPSHOT_KERNEL_ID: &str = "grant-snapshot-kernel-1368";
const SNAPSHOT_ROOT_ID: &str = "grant-snapshot-root-1368";
const SNAPSHOT_METRICS_ID: &str = "grant-snapshot-metrics-1368";
const SNAPSHOT_STATE_OUTPUT_ID: &str = "grant-snapshot-state-output-1368";
const SNAPSHOT_MEMORY_OUTPUT_ID: &str = "grant-snapshot-memory-output-1368";
const SNAPSHOT_STATE_INPUT_ID: &str = "grant-snapshot-state-input-1368";
const SNAPSHOT_MEMORY_INPUT_ID: &str = "grant-snapshot-memory-input-1368";
const SNAPSHOT_DESCRIBE_INPUT_ID: &str = "grant-snapshot-describe-input-1368";
const SNAPSHOT_KERNEL_REF: &str = "bangbang-grant:grant-snapshot-kernel-1368";
const SNAPSHOT_ROOT_REF: &str = "bangbang-grant:grant-snapshot-root-1368";
const SNAPSHOT_METRICS_REF: &str = "bangbang-grant:grant-snapshot-metrics-1368";
const SNAPSHOT_STATE_OUTPUT_REF: &str =
    "bangbang-grant:grant-snapshot-state-output-1368/state-1368.snap";
const SNAPSHOT_MEMORY_OUTPUT_REF: &str =
    "bangbang-grant:grant-snapshot-memory-output-1368/memory-1368.snap";
const SNAPSHOT_REPEAT_STATE_OUTPUT_REF: &str =
    "bangbang-grant:grant-snapshot-state-output-1368/state-repeat-1368.snap";
const SNAPSHOT_REPEAT_MEMORY_OUTPUT_REF: &str =
    "bangbang-grant:grant-snapshot-memory-output-1368/memory-repeat-1368.snap";
const SNAPSHOT_STATE_INPUT_REF: &str = "bangbang-grant:grant-snapshot-state-input-1368";
const SNAPSHOT_MEMORY_INPUT_REF: &str = "bangbang-grant:grant-snapshot-memory-input-1368";
const SNAPSHOT_DESCRIBE_INPUT_REF: &str = "bangbang-grant:grant-snapshot-describe-input-1368";
const SNAPSHOT_STAGING_HOLD_OPTION: &str = "--bangbang-internal-snapshot-staging-hold-v1";
const SNAPSHOT_STAGING_RECORD_BYTES: u64 = 128;
const SNAPSHOT_STATE_CHILD: &str = "state-1368.snap";
const SNAPSHOT_MEMORY_CHILD: &str = "memory-1368.snap";
const SNAPSHOT_REPEAT_STATE_CHILD: &str = "state-repeat-1368.snap";
const SNAPSHOT_REPEAT_MEMORY_CHILD: &str = "memory-repeat-1368.snap";
const SNAPSHOT_GUEST_IMAGE_HEADER_SIZE: usize = 64;
const SNAPSHOT_GUEST_IMAGE_MAGIC: u32 = 0x644d_5241;
const SNAPSHOT_GUEST_UART_ADDRESS: u64 = 0x4000_2000;
const SNAPSHOT_GUEST_VMGENID_ADDRESS: u64 = 0x801f_eff0;
const GRANTED_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-multistream=1";
const GRANTED_VSOCK_MARKER: &[u8] = b"BANGBANG_VSOCK_GUEST_MULTISTREAM_OK";
const GRANTED_VSOCK_EXCHANGES: &[(u32, &[u8], &[u8])] = &[
    (
        5007,
        b"BANGBANG_VSOCK_GUEST_MULTI_ONE",
        b"BANGBANG_VSOCK_HOST_MULTI_ONE",
    ),
    (
        5008,
        b"BANGBANG_VSOCK_GUEST_MULTI_TWO",
        b"BANGBANG_VSOCK_HOST_MULTI_TWO",
    ),
];
const GRANTED_HOST_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-connect=1";
const GRANTED_HOST_VSOCK_READY_MARKER: &[u8] = b"BANGBANG_VSOCK_HOST_CONNECT_READY";
const GRANTED_HOST_VSOCK_MARKER: &[u8] = b"BANGBANG_VSOCK_HOST_CONNECT_OK";
const GRANTED_HOST_VSOCK_PORT: u32 = 5006;
const GRANTED_HOST_VSOCK_STREAM_BYTES: usize = 1024 * 1024;
const GRANTED_HOST_VSOCK_CHUNK_BYTES: usize = 16 * 1024;
const GRANTED_HOST_VSOCK_GUEST_SEED: u8 = 0x3d;
const GRANTED_HOST_VSOCK_HOST_SEED: u8 = 0xa7;
const GUEST_SERIAL_MARKER: &[u8] = b"Linux version";
const DIRECT_ROOTFS_PMEM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-read-flush=1";
const DIRECT_ROOTFS_PMEM_ROOT_RO_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.pmem-root=ro";
const DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER: &[u8] = b"BANGBANG_PMEM_ROOT_RO_OK";
const DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.memory-hotplug-check=1";
const DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-writeback-flush=1";
const BLOCK_SERIAL_BEGIN_MARKER: &[u8] = b"BANGBANG_BLOCK_SERIAL_BEGIN";
const BLOCK_SERIAL_END_MARKER: &[u8] = b"BANGBANG_BLOCK_SERIAL_END";
const DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-hotplug=1";
const DIRECT_ROOTFS_PMEM_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-hotplug=1";
const DIRECT_ROOTFS_NETWORK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.network-hotplug=1";
const BLOCK_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_READY";
const BLOCK_HOTPLUG_HOST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_HOST_ONE";
const BLOCK_HOTPLUG_GUEST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_GUEST_ONE";
const BLOCK_HOTPLUG_FIRST_REMOVED_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_FIRST_REMOVED";
const BLOCK_HOTPLUG_CONTINUE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_CONTINUE";
const BLOCK_HOTPLUG_HOST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_HOST_TWO";
const BLOCK_HOTPLUG_GUEST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_GUEST_TWO";
const BLOCK_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_SUCCESS";
const PMEM_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_READY";
const PMEM_HOTPLUG_HOST_ONE_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_HOST_ONE";
const PMEM_HOTPLUG_GUEST_ONE_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_GUEST_ONE";
const PMEM_HOTPLUG_FIRST_REMOVED_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_FIRST_REMOVED";
const PMEM_HOTPLUG_CONTINUE_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_CONTINUE";
const PMEM_HOTPLUG_HOST_TWO_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_HOST_TWO";
const PMEM_HOTPLUG_GUEST_TWO_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_GUEST_TWO";
const PMEM_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_PMEM_HOTPLUG_SUCCESS";
const NETWORK_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_NETWORK_HOTPLUG_READY";
const NETWORK_HOTPLUG_FIRST_CONTINUE_MARKER: &[u8] = b"BANGBANG_NETWORK_HOTPLUG_FIRST_CONTINUE";
const NETWORK_HOTPLUG_FIRST_REMOVED_MARKER: &[u8] = b"BANGBANG_NETWORK_HOTPLUG_FIRST_REMOVED";
const NETWORK_HOTPLUG_SECOND_CONTINUE_MARKER: &[u8] = b"BANGBANG_NETWORK_HOTPLUG_SECOND_CONTINUE";
const NETWORK_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_NETWORK_HOTPLUG_SUCCESS";
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
const DROP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
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

fn jailer_command(bundle: &Path, id: &str, limits: &[&str], daemonize: bool) -> Command {
    jailer_command_with_policy(bundle, id, limits, daemonize, &[])
}

fn jailer_command_with_policy(
    bundle: &Path,
    id: &str,
    limits: &[&str],
    daemonize: bool,
    policy_args: &[OsString],
) -> Command {
    // SAFETY: Credential getters have no pointer or ownership contract.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut command = Command::new(launcher(bundle));
    command
        .arg(JAILER_OPTION)
        .args(["--id", id])
        .arg("--exec-file")
        .arg(worker_executable(bundle))
        .args(["--uid", &uid.to_string(), "--gid", &gid.to_string()]);
    for limit in limits {
        command.args(["--resource-limit", limit]);
    }
    if daemonize {
        command.arg("--daemonize");
    }
    command.args(policy_args);
    command.arg("--");
    command
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
    assert!(
        !worker.join("Contents/embedded.provisionprofile").exists(),
        "networkless production worker must not embed a provisioning profile"
    );
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
fn launcher_exposes_exact_jailer_help_version_and_policy_validation() {
    let bundle = production_bundle();
    let help = run_launcher(&bundle, &[OsStr::new(JAILER_OPTION), OsStr::new("--help")]);
    assert_output_success(&help, "jailer help");
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("Usage: bangbang-launcher"));

    let version = run_launcher(
        &bundle,
        &[OsStr::new(JAILER_OPTION), OsStr::new("--version")],
    );
    assert_output_success(&version, "jailer version");
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("Jailer v"));

    let assert_invalid = |mut command: Command, context: &str| {
        let invalid = run_with_timeout(&mut command, PROCESS_TIMEOUT, context);
        assert_eq!(invalid.status.code(), Some(1));
        assert!(
            invalid.stdout.is_empty(),
            "invalid policy must not execute the worker; {context} stdout:\n{}",
            String::from_utf8_lossy(&invalid.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&invalid.stderr),
            "bangbang launcher: invalid production launch policy\n"
        );
    };

    let mut duplicate = jailer_command(&bundle, "invalid-policy", &[], false);
    duplicate
        .args(["--id", "forged-duplicate"])
        .arg("--version");
    assert_invalid(duplicate, "duplicate jailer policy");

    // SAFETY: Credential getters have no pointer or ownership contract.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let policy_command = |executable: &Path, requested_uid: u32, requested_gid: u32| {
        let mut command = Command::new(launcher(&bundle));
        command
            .arg(JAILER_OPTION)
            .args(["--id", "fixed-policy"])
            .arg("--exec-file")
            .arg(executable)
            .args([
                "--uid",
                &requested_uid.to_string(),
                "--gid",
                &requested_gid.to_string(),
                "--",
                "--version",
            ]);
        command
    };
    assert_invalid(
        policy_command(Path::new("/usr/bin/false"), uid, gid),
        "substituted jailer executable",
    );
    assert_invalid(
        policy_command(&worker_executable(&bundle), uid.wrapping_add(1), gid),
        "mismatched jailer credential",
    );

    let mut vmnet = Command::new(launcher(&bundle));
    vmnet
        .arg(JAILER_OPTION)
        .args(["--id", "networkless-profile"])
        .arg("--exec-file")
        .arg(worker_executable(&bundle))
        .args([
            "--uid",
            &uid.to_string(),
            "--gid",
            &gid.to_string(),
            "--vmnet-allow",
            "shared",
            "--vmnet-max-interfaces",
            "1",
            "--",
            "--version",
        ]);
    assert_invalid(
        vmnet,
        "networkless signed profile with positive vmnet authority",
    );
}

#[test]
fn signed_jailer_rejects_linux_isolation_before_grants_sessions_and_worker() {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    let private = TestDir::new("linux-isolation-rejection");

    let run_case = |case: &str,
                    argument: JailerIsolationArgument,
                    policy_args: Vec<OsString>,
                    private_values: &[&str]| {
        let baseline_sessions = session_entries();
        let private_manifest = private
            .path()
            .join(format!("private-grant-{case}-must-not-open.json"));
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket_path =
            container_tmp_dir().join(format!("i-{:x}-{socket_id:x}.sock", std::process::id()));
        assert!(!private_manifest.exists());
        assert!(!socket_path.exists());

        let mut command = jailer_command_with_policy(&bundle, case, &[], false, &policy_args);
        command
            .arg(GRANT_MANIFEST_OPTION)
            .arg(&private_manifest)
            .arg("--")
            .arg("--api-sock")
            .arg(&socket_path);
        let output = run_with_timeout(
            &mut command,
            PROCESS_TIMEOUT,
            "signed Linux isolation rejection",
        );

        assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
        assert!(
            output.stdout.is_empty(),
            "{case} must not execute the worker or publish readiness; stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "bangbang launcher: unsupported Firecracker jailer isolation argument on macOS: --{}\n",
                argument.name()
            )
        );
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for private_value in private_values {
            assert!(!diagnostics.contains(private_value));
        }
        assert!(!diagnostics.contains(path_text(&private_manifest)));
        assert!(!diagnostics.contains(path_text(&socket_path)));
        assert!(!private_manifest.exists());
        assert!(!socket_path.exists());
        assert_eq!(session_entries(), baseline_sessions);
    };

    let arguments = [
        JailerIsolationArgument::Cgroup,
        JailerIsolationArgument::CgroupVersion,
        JailerIsolationArgument::ParentCgroup,
        JailerIsolationArgument::NetworkNamespace,
        JailerIsolationArgument::PidNamespace,
    ];
    for argument in arguments {
        let name = argument.name();
        run_case(
            &format!("{name}-exact"),
            argument,
            vec![OsString::from(format!("--{name}"))],
            &[],
        );

        let private_value = format!("private-{name}-attached-value");
        run_case(
            &format!("{name}-attached"),
            argument,
            vec![OsString::from(format!("--{name}={private_value}"))],
            &[&private_value],
        );
    }

    for argument in [
        JailerIsolationArgument::Cgroup,
        JailerIsolationArgument::CgroupVersion,
        JailerIsolationArgument::ParentCgroup,
        JailerIsolationArgument::NetworkNamespace,
    ] {
        let name = argument.name();
        let private_value = format!("private-{name}-separated-value");
        run_case(
            &format!("{name}-separated"),
            argument,
            vec![
                OsString::from(format!("--{name}")),
                OsString::from(&private_value),
            ],
            &[&private_value],
        );
    }
}

#[test]
fn signed_jailer_policy_enforces_empty_environment_private_root_and_exact_limits() {
    let bundle = grant_test_bundle();
    for (case, limits) in [
        ("policy-default", Vec::<&str>::new()),
        ("policy-explicit", vec!["no-file=1024", "fsize=4096"]),
        (
            "policy-last",
            vec!["no-file=4096", "fsize=8192", "no-file=2048", "fsize=4096"],
        ),
    ] {
        let fixture = GrantProbeFixture::new(case, false);
        let mut command = jailer_command(&bundle, case, &limits, false);
        command
            .arg(GRANT_MANIFEST_OPTION)
            .arg(&fixture.manifest)
            .arg("--")
            .arg(GRANT_PROBE_OPTION)
            .arg(case)
            .env("BANGBANG_POLICY_SECRET", "secret-must-not-reach-worker")
            .env(
                "BANGBANG_ORDINARY_AMBIENT",
                "ordinary-must-not-reach-worker",
            )
            .env("DYLD_LIBRARY_PATH", "loader-must-not-reach-worker")
            .env("RUST_LOG", "debug-must-not-reach-worker")
            .env(SESSION_ENV_KEY, "forged-internal-marker");
        let output = run_with_timeout(&mut command, PROCESS_TIMEOUT, "signed jailer policy probe");
        assert_output_success(&output, "signed jailer policy probe");
        assert_grant_output_redacted(&output, &fixture);
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for value in [
            "secret-must-not-reach-worker",
            "ordinary-must-not-reach-worker",
            "loader-must-not-reach-worker",
            "debug-must-not-reach-worker",
            "forged-internal-marker",
        ] {
            assert!(!diagnostics.contains(value));
        }
        fixture.assert_completed();
    }

    let nofile_fixture = GrantProbeFixture::new("policy-nofile-exhaustion", false);
    let mut nofile = jailer_command(
        &bundle,
        "policy-nofile-exhaustion",
        &["no-file=1024"],
        false,
    );
    nofile
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&nofile_fixture.manifest)
        .arg("--")
        .arg(GRANT_PROBE_OPTION)
        .arg("policy-nofile-exhaustion");
    let nofile = run_with_timeout(&mut nofile, PROCESS_TIMEOUT, "RLIMIT_NOFILE exhaustion");
    assert_output_success(&nofile, "RLIMIT_NOFILE exhaustion");
    assert_grant_output_redacted(&nofile, &nofile_fixture);
    nofile_fixture.assert_completed();

    let fsize_fixture = GrantProbeFixture::new("policy-fsize-exhaustion", false);
    let mut fsize = jailer_command(
        &bundle,
        "policy-fsize-exhaustion",
        &["no-file=1024", "fsize=4096"],
        false,
    );
    fsize
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fsize_fixture.manifest)
        .arg("--")
        .arg(GRANT_PROBE_OPTION)
        .arg("policy-fsize-exhaustion");
    let fsize = run_with_timeout(&mut fsize, PROCESS_TIMEOUT, "RLIMIT_FSIZE exhaustion");
    assert_eq!(
        fsize.status.code(),
        Some(128 + libc::SIGXFSZ),
        "the kernel should terminate the worker at the exact file-size boundary"
    );
    assert_grant_output_redacted(&fsize, &fsize_fixture);
    assert!(session_entries().is_empty());
}

#[test]
fn signed_daemon_handoff_waits_for_ready_and_keeps_concurrent_supervisors_isolated() {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    let start = |name: &str| {
        let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket =
            container_tmp_dir().join(format!("bbd-{:x}-{test_id:x}.sock", std::process::id()));
        let mut command = jailer_command(&bundle, name, &[], true);
        command.args(["--api-sock", path_text(&socket)]);
        let output = run_with_timeout(&mut command, PROCESS_TIMEOUT, "daemon readiness handoff");
        assert_output_success(&output, "daemon readiness handoff");
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("daemon PID line should be UTF-8");
        let mut lines = stdout.lines();
        let pid = lines
            .next()
            .and_then(|line| line.strip_prefix("bangbang daemon pid: "))
            .and_then(|value| value.parse::<libc::pid_t>().ok())
            .filter(|pid| *pid > 0)
            .expect("daemon PID line should be exact");
        assert!(
            lines.next().is_none(),
            "daemon output should contain one PID line"
        );
        assert!(
            fs::symlink_metadata(&socket)
                .expect("Ready must publish the API socket")
                .file_type()
                .is_socket(),
            "Ready must follow API socket publication"
        );
        assert_http_status(&http_get(&socket, "/"), 200, "daemon API readiness");
        (pid, socket)
    };

    let (first_pid, first_socket) = start("daemon-policy-alpha");
    let (second_pid, second_socket) = start("daemon-policy-beta");
    assert_ne!(first_pid, second_pid);

    // SAFETY: The authenticated PID was returned by the handoff and has not
    // been observed exiting or reused.
    assert_eq!(unsafe { libc::kill(first_pid, libc::SIGTERM) }, 0);
    assert!(wait_for_process_exit(first_pid, PROCESS_TIMEOUT));
    assert!(!first_socket.exists());
    assert_http_status(
        &http_get(&second_socket, "/"),
        200,
        "concurrent daemon survives peer termination",
    );

    // SAFETY: The second authenticated supervisor is still live above.
    assert_eq!(unsafe { libc::kill(second_pid, libc::SIGTERM) }, 0);
    assert!(wait_for_process_exit(second_pid, PROCESS_TIMEOUT));
    assert!(!second_socket.exists());
    assert!(session_entries().is_empty());
}

#[test]
fn signed_daemon_parent_loss_before_ack_cancels_worker_and_private_state() {
    let bundle = grant_test_bundle();
    initialize_worker_container(&bundle);
    let baseline_sessions = session_entries();
    let fixture = GrantProbeFixture::new("daemon-parent-loss", false);
    let mut command = jailer_command(&bundle, "daemon-parent-loss", &[], true);
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .arg(GRANT_DELAY_OPTION);
    let parent = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("daemon handoff parent should start");
    let parent_pid = libc::pid_t::try_from(parent.id()).expect("parent PID should fit");
    let daemon_pid = wait_for_only_child_pid(parent_pid, PROCESS_TIMEOUT, "daemon launcher");
    assert!(
        wait_for_new_session(&baseline_sessions, PROCESS_TIMEOUT),
        "daemon worker should prepare its private namespace before the handoff"
    );
    let worker_pid = wait_for_only_child_pid(daemon_pid, PROCESS_TIMEOUT, "daemon worker");
    let daemon_exit = ProcessExitWatch::new(daemon_pid);
    let worker_exit = ProcessExitWatch::new(worker_pid);

    // SAFETY: The unreaped original launcher still owns this exact PID. SIGKILL
    // closes the only parent handoff endpoint without signaling the new session.
    assert_eq!(unsafe { libc::kill(parent_pid, libc::SIGKILL) }, 0);
    let output = parent
        .wait_with_output()
        .expect("killed handoff parent should be reaped");
    assert_eq!(output.status.signal(), Some(libc::SIGKILL));
    assert!(
        output.stdout.is_empty(),
        "pre-ack launch must not publish a PID"
    );
    assert!(
        output.stderr.is_empty(),
        "pre-ack failure must remain private"
    );

    let worker_stopped = worker_exit.wait(PROCESS_TIMEOUT);
    let daemon_stopped = daemon_exit.wait(PROCESS_TIMEOUT);
    if !worker_stopped || !daemon_stopped {
        // SAFETY: The daemon established a fresh session/process group and the
        // test has not observed its exit, so this bounds a failed cleanup path.
        let _ = unsafe { libc::kill(-daemon_pid, libc::SIGKILL) };
    }
    assert!(
        worker_stopped,
        "parent loss should cancel and reap the worker"
    );
    assert!(
        daemon_stopped,
        "parent loss should stop the daemon supervisor"
    );
    assert_eq!(session_entries(), baseline_sessions);
    fixture.assert_unmodified();
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

    let unexpected_profile = TestDir::new("unexpected-profile");
    let unexpected_profile_bundle = unexpected_profile.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &unexpected_profile_bundle);
    fs::write(
        worker_bundle(&unexpected_profile_bundle).join("Contents/embedded.provisionprofile"),
        b"networkless-profile-must-remain-absent",
    )
    .expect("unexpected profile should be written");
    resign_worker_and_outer(&unexpected_profile_bundle, valid_entitlements, true, true);
    assert_invalid_bundle(run_launcher(
        &unexpected_profile_bundle,
        &[OsStr::new("--help")],
    ));

    let vmnet_entitlements = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
<key>com.apple.vm.networking</key><true/>
<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>
<key>com.apple.developer.team-identifier</key><string>TEAM123456</string>
</dict></plist>"#;

    let vmnet_without_profile = TestDir::new("vmnet-without-profile");
    let vmnet_without_profile_bundle = vmnet_without_profile.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &vmnet_without_profile_bundle);
    resign_worker_and_outer(
        &vmnet_without_profile_bundle,
        vmnet_entitlements,
        true,
        true,
    );
    assert_invalid_bundle(run_launcher(
        &vmnet_without_profile_bundle,
        &[OsStr::new("--help")],
    ));

    let developer_extra = TestDir::new("developer-vmnet-extra");
    let developer_extra_bundle = developer_extra.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &developer_extra_bundle);
    fs::write(
        worker_bundle(&developer_extra_bundle).join("Contents/embedded.provisionprofile"),
        b"negative-static-profile-fixture",
    )
    .expect("negative profile should be written");
    resign_worker_and_outer(
        &developer_extra_bundle,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.app-sandbox</key><true/>
<key>com.apple.security.hypervisor</key><true/>
<key>com.apple.vm.networking</key><true/>
<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>
<key>com.apple.developer.team-identifier</key><string>TEAM123456</string>
<key>com.apple.developer.networking.vmnet</key><true/>
</dict></plist>"#,
        true,
        true,
    );
    assert_invalid_bundle(run_launcher(
        &developer_extra_bundle,
        &[OsStr::new("--help")],
    ));

    let denied_vmnet = TestDir::new("denied-vmnet-policy");
    let denied_vmnet_bundle = denied_vmnet.path().join(OUTER_BUNDLE_NAME);
    copy_tree(&source, &denied_vmnet_bundle);
    fs::write(
        worker_bundle(&denied_vmnet_bundle).join("Contents/embedded.provisionprofile"),
        b"negative-policy-profile-fixture",
    )
    .expect("negative profile should be written");
    resign_worker_and_outer(&denied_vmnet_bundle, vmnet_entitlements, true, true);
    let denied = run_launcher(&denied_vmnet_bundle, &[OsStr::new("--help")]);
    assert_eq!(denied.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert!(denied.stdout.is_empty(), "worker must not execute");
    assert_eq!(
        String::from_utf8_lossy(&denied.stderr),
        "bangbang launcher: invalid production launch policy\n"
    );

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
fn normal_bundle_adopts_snapshot_grants_for_create_describe_and_restore() {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    let baseline_sessions = session_entries();
    let source_fixture = SnapshotSourceGrantFixture::new("continuity");
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        &bundle,
        &source_fixture.manifest,
        source_fixture.sensitive_strings(),
        "snapshot-source",
        false,
    );
    source_fixture.replace_source_file_pathnames();
    configure_and_pause_snapshot_source(&source, &source_fixture.opened_metrics);

    let create_body = snapshot_create_body();
    let create = http_put(&source.socket, "/snapshot/create", &create_body);
    assert_http_status(&create, 204, "create granted snapshot");
    let artifacts = source_fixture.artifacts();
    assert!(
        artifacts.state.is_file(),
        "granted state output should exist"
    );
    assert!(
        artifacts.memory.is_file(),
        "granted memory output should exist"
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let state_before = fs::read(&artifacts.state).expect("granted state should read");
    let memory_before = fs::read(&artifacts.memory).expect("granted memory should read");

    let repeated_create = http_put(
        &source.socket,
        "/snapshot/create",
        &repeated_snapshot_create_body(),
    );
    assert_http_status(
        &repeated_create,
        204,
        "reuse granted snapshot output directories",
    );
    let repeated_artifacts = source_fixture.repeated_artifacts();
    assert!(
        repeated_artifacts.state.is_file(),
        "reused state output grant should publish another child"
    );
    assert!(
        repeated_artifacts.memory.is_file(),
        "reused memory output grant should publish another child"
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let repeated_state_before =
        fs::read(&repeated_artifacts.state).expect("repeated state should read");
    let repeated_memory_before =
        fs::read(&repeated_artifacts.memory).expect("repeated memory should read");

    let collision = http_put(&source.socket, "/snapshot/create", &create_body);
    assert_http_status(&collision, 400, "colliding granted snapshot create");
    for private in [
        SNAPSHOT_STATE_OUTPUT_REF,
        SNAPSHOT_MEMORY_OUTPUT_REF,
        SNAPSHOT_REPEAT_STATE_OUTPUT_REF,
        SNAPSHOT_REPEAT_MEMORY_OUTPUT_REF,
        SNAPSHOT_STATE_CHILD,
        SNAPSHOT_MEMORY_CHILD,
        SNAPSHOT_REPEAT_STATE_CHILD,
        SNAPSHOT_REPEAT_MEMORY_CHILD,
    ] {
        assert!(!collision.contains(private));
    }
    assert_eq!(
        fs::read(&artifacts.state).expect("state should survive collision"),
        state_before
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("memory should survive collision"),
        memory_before
    );
    assert_eq!(
        fs::read(&repeated_artifacts.state).expect("repeated state should survive collision"),
        repeated_state_before
    );
    assert_eq!(
        fs::read(&repeated_artifacts.memory).expect("repeated memory should survive collision"),
        repeated_memory_before
    );

    let peer_fixture = SnapshotSourceGrantFixture::new("concurrent-peer");
    let mut peer = spawn_ready_snapshot_grant_api_launcher(
        &bundle,
        &peer_fixture.manifest,
        peer_fixture.sensitive_strings(),
        "snapshot-concurrent-peer",
        false,
    );
    peer_fixture.replace_source_file_pathnames();
    configure_and_pause_snapshot_source(&peer, &peer_fixture.opened_metrics);
    let peer_artifacts = peer_fixture.artifacts();
    assert!(!peer_artifacts.state.exists());
    assert!(!peer_artifacts.memory.exists());
    let peer_create = http_put(&peer.socket, "/snapshot/create", &create_body);
    assert_http_status(&peer_create, 204, "create concurrent granted snapshot");
    assert!(peer_artifacts.state.is_file());
    assert!(peer_artifacts.memory.is_file());
    assert_no_snapshot_staging(&peer_fixture.state_directory);
    assert_no_snapshot_staging(&peer_fixture.memory_directory);
    assert_eq!(
        fs::read(&artifacts.state).expect("peer must not rewrite source state"),
        state_before
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("peer must not rewrite source memory"),
        memory_before
    );
    stop_running_launcher(&mut peer, "concurrent granted snapshot peer");
    stop_running_launcher(&mut source, "granted snapshot source");
    assert_eq!(session_entries(), baseline_sessions);

    let describe = SnapshotDescribeGrantFixture::new("valid", &artifacts.state, true);
    let describe_output = run_snapshot_describe(&bundle, &describe);
    assert_output_success(&describe_output, "granted snapshot description");
    assert_eq!(
        String::from_utf8_lossy(&describe_output.stdout).trim(),
        "v1.0.0"
    );
    assert_snapshot_output_redacted(&describe_output, &describe.sensitive_strings());

    let mismatch = SnapshotDescribeGrantFixture::new("wrong-role", &artifacts.state, false);
    let mismatch_output = run_snapshot_describe(&bundle, &mismatch);
    assert_eq!(
        mismatch_output.status.code(),
        Some(BAD_CONFIGURATION_EXIT_CODE)
    );
    assert!(String::from_utf8_lossy(&mismatch_output.stderr).contains("snapshot inspection"));
    assert_snapshot_output_redacted(&mismatch_output, &mismatch.sensitive_strings());
    assert_eq!(session_entries(), baseline_sessions);

    let paused_fixture = SnapshotInputGrantFixture::new("paused", artifacts);
    let mut paused = spawn_ready_snapshot_grant_api_launcher(
        &bundle,
        &paused_fixture.manifest,
        paused_fixture.sensitive_strings(),
        "snapshot-paused",
        false,
    );
    let next_artifacts = paused_fixture.replace_source_pathnames();
    let paused_load = http_put(&paused.socket, "/snapshot/load", &snapshot_load_body(false));
    assert_http_status(&paused_load, 204, "load granted snapshot paused");
    let paused_state = http_get(&paused.socket, "/");
    assert_http_status(&paused_state, 200, "read granted paused snapshot state");
    assert!(paused_state.contains(r#""state":"Paused""#));
    assert_http_status(
        &http_request(&paused.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume granted snapshot",
    );
    assert!(
        paused.wait("granted snapshot explicit resume").success(),
        "explicitly resumed granted snapshot should reach SYSTEM_OFF"
    );
    assert_eq!(session_entries(), baseline_sessions);

    let resumed_fixture = SnapshotInputGrantFixture::new("automatic", next_artifacts);
    let mut resumed = spawn_ready_snapshot_grant_api_launcher(
        &bundle,
        &resumed_fixture.manifest,
        resumed_fixture.sensitive_strings(),
        "snapshot-automatic",
        false,
    );
    let final_artifacts = resumed_fixture.replace_source_pathnames();
    let resumed_load = http_put(&resumed.socket, "/snapshot/load", &snapshot_load_body(true));
    assert_http_status(
        &resumed_load,
        204,
        "load and automatically resume granted snapshot",
    );
    assert!(
        resumed.wait("granted snapshot automatic resume").success(),
        "automatically resumed granted snapshot should reach SYSTEM_OFF"
    );
    assert_eq!(
        fs::read(&final_artifacts.state).expect("final state should read"),
        state_before
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final memory should read"),
        memory_before
    );
    assert_eq!(session_entries(), baseline_sessions);
}

#[test]
fn grant_test_bundle_recovers_recorded_snapshot_staging_after_worker_sigkill() {
    let bundle = grant_test_bundle();
    initialize_worker_container(&bundle);
    let baseline_sessions = session_entries();

    for preserve_replacement in [false, true] {
        let case = if preserve_replacement {
            "staging-replacement"
        } else {
            "staging-exact"
        };
        let fixture = SnapshotSourceGrantFixture::new(case);
        let mut running = spawn_ready_snapshot_grant_api_launcher(
            &bundle,
            &fixture.manifest,
            fixture.sensitive_strings(),
            case,
            true,
        );
        fixture.replace_source_file_pathnames();
        configure_and_pause_snapshot_source(&running, &fixture.opened_metrics);
        let active_session = session_entries()
            .into_iter()
            .find(|entry| !baseline_sessions.contains(entry))
            .expect("snapshot crash session should exist");
        let watch = DirectoryChangeWatch::new(&fixture.memory_directory);
        let record_watch = DirectoryChangeWatch::new(&active_session);
        let request = begin_snapshot_create_request(&running.socket);
        let staging = watch
            .wait_for_snapshot_staging(PROCESS_TIMEOUT)
            .expect("recorded memory staging file should appear");
        record_watch
            .wait_for_child_with_len(
                ".snapshot-memory-owner",
                SNAPSHOT_STAGING_RECORD_BYTES,
                PROCESS_TIMEOUT,
            )
            .expect("worker must durably record ownership before the test hold");

        let mut moved_owned = None;
        if preserve_replacement {
            let moved = fixture
                .memory_directory
                .join("moved-recorded-memory-staging");
            fs::rename(&staging, &moved).expect("recorded staging inode should move");
            fs::write(&staging, b"replacement staging must survive\n")
                .expect("replacement staging should write");
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))
                .expect("replacement staging permissions should tighten");
            moved_owned = Some(moved);
        }

        let worker_pid = only_worker_pid(&running.child);
        let worker_exit = ProcessExitWatch::new(worker_pid);
        // SAFETY: The live worker is the sole child of the retained launcher.
        assert_eq!(unsafe { libc::kill(worker_pid, libc::SIGKILL) }, 0);
        assert!(
            worker_exit.wait(PROCESS_TIMEOUT),
            "snapshot worker should exit after SIGKILL"
        );
        drop(request);
        let status = running.wait("recorded snapshot staging worker SIGKILL");
        assert_eq!(status.code(), Some(128 + libc::SIGKILL));
        assert_eq!(session_entries(), baseline_sessions);
        assert!(!fixture.artifacts().state.exists());
        assert!(!fixture.artifacts().memory.exists());

        if preserve_replacement {
            assert_eq!(
                fs::read(&staging).expect("replacement staging should remain"),
                b"replacement staging must survive\n"
            );
            fs::remove_file(&staging).expect("replacement staging should clean");
            fs::remove_file(
                moved_owned
                    .as_ref()
                    .expect("moved recorded staging should exist"),
            )
            .expect("moved recorded staging should clean");
        } else {
            assert!(
                !staging.exists(),
                "exact recorded staging should be removed"
            );
            assert_no_snapshot_staging(&fixture.memory_directory);
        }
    }
}

#[test]
fn normal_bundle_routes_guest_vsock_through_launcher_broker_without_helpers() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new("guest-vsock");
    let mut listeners = Vec::new();
    for &(port, _, _) in GRANTED_VSOCK_EXCHANGES {
        let path = fixture.vsock_port_path(port);
        let listener = UnixListener::bind(&path).expect("granted vsock port listener should bind");
        listener
            .set_nonblocking(true)
            .expect("granted vsock port listener should be nonblocking");
        listeners.push((port, path, listener));
    }

    let mut running = spawn_ready_socket_grant_api_launcher(&bundle, &fixture, "guest-vsock");
    assert_socket_mode(&fixture.api_socket(), 0o600, "granted API socket");
    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "short-lived API binder must be reaped before readiness"
    );

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT granted-vsock machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": GRANTED_VSOCK_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot source should serialize"),
        ),
        204,
        "PUT granted-vsock boot source",
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
            "PUT granted-vsock rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
            }),
            "PUT granted-vsock data drive",
        ),
        (
            "/vsock",
            serde_json::json!({"guest_cid": 3, "uds_path": VSOCK_SOCKET_REF}),
            "PUT granted-vsock device",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("granted-vsock request should serialize"),
            ),
            204,
            context,
        );
    }
    assert!(
        !fixture.vsock_socket().exists(),
        "vsock directory claim must remain deferred until VM start"
    );

    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start granted-vsock guest",
    );
    assert_socket_mode(&fixture.vsock_socket(), 0o600, "granted vsock socket");
    assert!(
        child_pids(worker).is_empty(),
        "granted vsock must not retain a connector helper"
    );

    let mut streams = Vec::new();
    for ((port, path, listener), &(expected_port, _, _)) in
        listeners.into_iter().zip(GRANTED_VSOCK_EXCHANGES)
    {
        assert_eq!(port, expected_port);
        let stream = wait_for_unix_listener_accept(&listener, PROCESS_TIMEOUT)
            .unwrap_or_else(|error| panic!("guest vsock port {port} should connect: {error}"));
        drop(listener);
        fs::remove_file(&path).expect("host-owned vsock port path should clean up");
        stream
            .set_nonblocking(true)
            .expect("accepted vsock stream should remain nonblocking");
        streams.push(stream);
    }

    for (stream, &(_, guest_payload, _)) in streams.iter_mut().zip(GRANTED_VSOCK_EXCHANGES) {
        let mut received = vec![0_u8; guest_payload.len()];
        read_exact_nonblocking(stream, &mut received, PROCESS_TIMEOUT)
            .expect("guest vsock payload should arrive");
        assert_eq!(received, guest_payload);
    }
    for (stream, &(_, _, host_payload)) in streams.iter_mut().zip(GRANTED_VSOCK_EXCHANGES) {
        write_all_nonblocking(stream, host_payload, PROCESS_TIMEOUT)
            .expect("host vsock reply should write");
    }

    wait_for_file_contains(&fixture.devices.data, GRANTED_VSOCK_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| panic!("guest vsock marker should reach data drive: {error}"));
    drop(streams);
    stop_running_launcher(&mut running, "granted-vsock guest shutdown");
    assert!(!fixture.api_socket().exists());
    assert!(!fixture.vsock_socket().exists());
    assert!(session_entries().is_empty());
}

#[test]
fn normal_bundle_brokers_multiple_contained_vhost_user_children_without_helpers() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new_with_vhost_user("vhost-user");
    let root_socket = fixture.vhost_user_socket(VHOST_USER_SOCKET_CHILD_ONE);
    let scratch_socket = fixture.vhost_user_socket(VHOST_USER_SOCKET_CHILD_TWO);
    let root_backing = fixture.devices.rootfs.clone();
    let scratch_backing = fixture.vhost_user_backing("vhost-scratch.img");
    let backing_len = 8 * 512_u64;
    create_sized_file(&scratch_backing, backing_len);
    OpenOptions::new()
        .write(true)
        .open(&scratch_backing)
        .expect("contained vhost scratch backing should open")
        .write_all(CONTAINED_VHOST_USER_HOST_MARKER)
        .expect("contained vhost host marker should write");
    let root_backend = VhostUserBlockBackend::start(
        &root_socket,
        &root_backing,
        VhostUserBlockBackendOptions::regular(true),
    )
    .expect("contained vhost root backend should start");
    let scratch_backend = VhostUserBlockBackend::start(
        &scratch_socket,
        &scratch_backing,
        VhostUserBlockBackendOptions::regular(false),
    )
    .expect("contained vhost scratch backend should start");

    let mut running = spawn_ready_socket_grant_api_launcher(&bundle, &fixture, "vhost-user");
    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "contained vhost connection must not retain a helper"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained-vhost machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.vhost-user-block=ro bangbang.expect-vhost-resize=1",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot source should serialize"),
        ),
        204,
        "PUT contained-vhost boot source",
    );
    for (path, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "is_root_device": true,
                "socket": VHOST_USER_SOCKET_REF_ONE,
            }),
            "PUT contained-vhost root device",
        ),
        (
            "/drives/scratch",
            serde_json::json!({
                "drive_id": "scratch",
                "is_root_device": false,
                "cache_type": "Writeback",
                "socket": VHOST_USER_SOCKET_REF_TWO,
            }),
            "PUT contained-vhost scratch device",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive request should serialize"),
            ),
            204,
            context,
        );
    }
    assert_http_status(
        &http_put(
            &running.socket,
            "/vsock",
            &serde_json::to_string(&serde_json::json!({
                "guest_cid": 3,
                "uds_path": VSOCK_SOCKET_REF,
            }))
            .expect("vsock request should serialize"),
        ),
        204,
        "PUT vsock alongside contained vhost children",
    );
    let before_start = http_get(&running.socket, "/vm/config");
    assert!(before_start.contains(VHOST_USER_SOCKET_REF_ONE));
    assert!(before_start.contains(VHOST_USER_SOCKET_REF_TWO));
    assert!(before_start.contains(VSOCK_SOCKET_REF));
    assert!(!fixture.vsock_socket().exists());

    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained-vhost guest",
    );
    root_backend
        .wait_for_activation(PROCESS_TIMEOUT)
        .expect("contained vhost root backend should activate");
    scratch_backend
        .wait_for_activation(PROCESS_TIMEOUT)
        .expect("contained vhost scratch backend should activate");
    assert_socket_mode(
        &fixture.vsock_socket(),
        0o600,
        "coexisting granted vsock socket",
    );
    assert!(
        child_pids(worker).is_empty(),
        "active contained vhost streams must not retain a helper"
    );
    wait_for_file_prefix(
        &scratch_backing,
        CONTAINED_VHOST_USER_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should boot from vhost root and complete scratch I/O");
    scratch_backend
        .wait_for_flush(PROCESS_TIMEOUT)
        .expect("contained vhost scratch should observe the synchronous guest write flush");
    OpenOptions::new()
        .write(true)
        .open(&scratch_backing)
        .expect("contained vhost scratch should reopen for resize")
        .set_len(10 * 512)
        .expect("contained vhost scratch should resize");
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/drives/scratch",
            r#"{"drive_id":"scratch"}"#,
        ),
        204,
        "PATCH active contained-vhost scratch",
    );
    wait_for_file_contains(
        &scratch_backing,
        VHOST_CONFIG_RESIZED_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should observe contained vhost scratch capacity refresh");
    assert_eq!(
        root_backend.report().config_requests,
        1,
        "contained vhost root should use one startup discovery"
    );
    let scratch_report = scratch_backend.report();
    assert_eq!(
        scratch_report.config_requests, 2,
        "startup and PATCH should use the existing scratch frontend"
    );
    assert!(scratch_report.reads > 0);
    assert!(scratch_report.writes > 0);
    assert!(scratch_report.flushes > 0);

    stop_running_launcher(&mut running, "contained-vhost guest shutdown");
    root_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("contained vhost root frontend should close");
    scratch_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("contained vhost scratch frontend should close");
    let root_report = root_backend
        .finish()
        .expect("contained vhost root backend should finish");
    let scratch_report = scratch_backend
        .finish()
        .expect("contained vhost scratch backend should finish");
    assert!(root_report.activated && scratch_report.activated);
    assert!(root_report.frontend_closed && scratch_report.frontend_closed);
    assert!(!root_socket.exists() && !scratch_socket.exists());
    assert!(!fixture.vsock_socket().exists());
    assert!(session_entries().is_empty());
}

#[test]
fn normal_bundle_retries_hotplugs_deletes_and_reuses_contained_vhost_user_block() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new_with_vhost_user("vhost-user-runtime");
    let control_socket = fixture.vhost_user_socket(VHOST_USER_SOCKET_CHILD_ONE);
    let first_socket = fixture.vhost_user_socket(VHOST_USER_SOCKET_CHILD_TWO);
    let second_socket = fixture.vhost_user_socket(VHOST_USER_SOCKET_CHILD_THREE);
    let invalid_child = "not-a-socket.sock";
    let invalid_socket = fixture.vhost_user_socket(invalid_child);
    let invalid_ref = format!("bangbang-grant:{VHOST_USER_SOCKET_DIRECTORY_ID}/{invalid_child}");
    let control_backing = fixture.vhost_user_backing("runtime-control.img");
    let first_backing = fixture.vhost_user_backing("runtime-first.img");
    let second_backing = fixture.vhost_user_backing("runtime-second.img");
    create_sized_file(&control_backing, 1024);
    create_sized_file(&first_backing, 512);
    create_sized_file(&second_backing, 512);
    resize_and_write_file_marker_at(&first_backing, 512, 0, BLOCK_HOTPLUG_HOST_ONE_MARKER);
    resize_and_write_file_marker_at(&second_backing, 512, 0, BLOCK_HOTPLUG_HOST_TWO_MARKER);
    fs::write(&invalid_socket, b"not a socket").expect("invalid endpoint fixture should create");
    let control_backend = VhostUserBlockBackend::start(
        &control_socket,
        &control_backing,
        VhostUserBlockBackendOptions::regular(false),
    )
    .expect("contained runtime-vhost control backend should start");

    let mut running = spawn_ready_socket_grant_api_launcher_with_extra_args(
        &bundle,
        &fixture,
        "vhost-user-runtime",
        &["--enable-pci"],
    );
    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "contained runtime vhost setup must not retain a helper"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained runtime-vhost machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot source should serialize"),
        ),
        204,
        "PUT contained runtime-vhost boot source",
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
            "PUT contained runtime-vhost rootfs",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "is_root_device": false,
                "cache_type": "Writeback",
                "socket": VHOST_USER_SOCKET_REF_ONE,
            }),
            "PUT contained runtime-vhost control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive request should serialize"),
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
        "start contained runtime-vhost guest",
    );
    control_backend
        .wait_for_activation(PROCESS_TIMEOUT)
        .expect("contained runtime-vhost control backend should activate");
    wait_for_file_prefix(
        &control_backing,
        BLOCK_HOTPLUG_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained runtime-vhost guest should become ready");

    let invalid = serde_json::json!({
        "drive_id": "invalid",
        "is_root_device": false,
        "socket": invalid_ref,
    });
    let invalid_response = http_put(
        &running.socket,
        "/drives/invalid",
        &serde_json::to_string(&invalid).expect("invalid endpoint request should serialize"),
    );
    assert_http_status(
        &invalid_response,
        400,
        "runtime PUT rejected contained vhost target",
    );
    assert!(
        invalid_response.contains("contained vhost-user socket connection failed"),
        "response:\n{invalid_response}"
    );
    assert!(!invalid_response.contains(VHOST_USER_SOCKET_DIRECTORY_ID));
    assert!(!invalid_response.contains(invalid_child));
    assert!(!http_get(&running.socket, "/vm/config").contains(r#""drive_id":"invalid""#));
    assert!(http_get(&running.socket, "/").contains(r#""state":"Running""#));

    let rejected_backend = VhostUserBlockBackend::start(
        &first_socket,
        &first_backing,
        VhostUserBlockBackendOptions::regular(false).without_config_protocol(),
    )
    .expect("rejecting contained runtime-vhost backend should start");
    let rejected = serde_json::json!({
        "drive_id": "rejected",
        "is_root_device": false,
        "socket": VHOST_USER_SOCKET_REF_TWO,
    });
    let rejected_response = http_put(
        &running.socket,
        "/drives/rejected",
        &serde_json::to_string(&rejected).expect("rejected drive request should serialize"),
    );
    assert_http_status(
        &rejected_response,
        400,
        "runtime PUT rejected contained vhost negotiation",
    );
    assert!(rejected_response.contains("vhost-user backend lacks configuration protocol support"));
    assert!(!rejected_response.contains(VHOST_USER_SOCKET_REF_TWO));
    assert!(!http_get(&running.socket, "/vm/config").contains(r#""drive_id":"rejected""#));
    let rejected_report = rejected_backend
        .finish()
        .expect("rejecting contained runtime-vhost backend should finish");
    assert!(rejected_report.discovery_rejected);

    let first_backend = VhostUserBlockBackend::start(
        &first_socket,
        &first_backing,
        VhostUserBlockBackendOptions::regular(false),
    )
    .expect("first contained runtime-vhost backend should start");
    let second_backend = VhostUserBlockBackend::start(
        &second_socket,
        &second_backing,
        VhostUserBlockBackendOptions::regular(false),
    )
    .expect("second contained runtime-vhost backend should start");
    let first = serde_json::json!({
        "drive_id": "hotdata",
        "is_root_device": false,
        "cache_type": "Writeback",
        "socket": VHOST_USER_SOCKET_REF_TWO,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/hotdata",
            &serde_json::to_string(&first).expect("first runtime drive should serialize"),
        ),
        204,
        "runtime PUT first contained vhost block",
    );
    let second = serde_json::json!({
        "drive_id": "hotdata",
        "is_root_device": false,
        "cache_type": "Writeback",
        "socket": VHOST_USER_SOCKET_REF_THREE,
    });
    let duplicate_response = http_put(
        &running.socket,
        "/drives/hotdata",
        &serde_json::to_string(&second).expect("duplicate runtime drive should serialize"),
    );
    assert_http_status(
        &duplicate_response,
        400,
        "duplicate runtime PUT contained vhost block",
    );
    assert!(duplicate_response.contains("drive is already configured"));
    assert_eq!(
        second_backend.report().owner_requests,
        0,
        "duplicate same-ID PUT must not request another broker connection"
    );
    first_backend
        .wait_for_activation(PROCESS_TIMEOUT)
        .expect("first contained runtime-vhost backend should activate");
    wait_for_file_prefix(
        &first_backing,
        BLOCK_HOTPLUG_GUEST_ONE_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("first contained runtime-vhost block should complete guest I/O");
    wait_for_file_prefix(
        &control_backing,
        BLOCK_HOTPLUG_FIRST_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should remove first contained runtime-vhost PCI function");

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause before contained runtime-vhost reuse",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "DELETE first contained runtime-vhost block",
    );
    first_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("first contained runtime-vhost frontend should close after DELETE");
    assert!(!http_get(&running.socket, "/vm/config").contains(r#""drive_id":"hotdata""#));
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/hotdata",
            &serde_json::to_string(&second).expect("reused runtime drive should serialize"),
        ),
        204,
        "paused PUT reused contained vhost block",
    );
    assert!(http_get(&running.socket, "/vm/config").contains(VHOST_USER_SOCKET_REF_THREE));
    resize_and_write_file_marker_at(&control_backing, 1024, 512, BLOCK_HOTPLUG_CONTINUE_MARKER);
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume after contained runtime-vhost reuse",
    );
    second_backend
        .wait_for_activation(PROCESS_TIMEOUT)
        .expect("reused contained runtime-vhost backend should activate");
    wait_for_file_prefix(
        &second_backing,
        BLOCK_HOTPLUG_GUEST_TWO_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("reused contained runtime-vhost block should complete guest I/O");
    wait_for_file_prefix(
        &control_backing,
        BLOCK_HOTPLUG_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should remove reused contained runtime-vhost PCI function");
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "final DELETE contained runtime-vhost block",
    );
    second_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("reused contained runtime-vhost frontend should close after DELETE");
    assert!(
        child_pids(worker).is_empty(),
        "contained runtime vhost lifecycle must not retain a helper"
    );

    stop_running_launcher(&mut running, "contained runtime-vhost guest shutdown");
    control_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("contained runtime-vhost control frontend should close at shutdown");
    let control_report = control_backend
        .finish()
        .expect("contained runtime-vhost control backend should finish");
    let first_report = first_backend
        .finish()
        .expect("first contained runtime-vhost backend should finish");
    let second_report = second_backend
        .finish()
        .expect("second contained runtime-vhost backend should finish");
    assert!(control_report.activated && control_report.frontend_closed);
    assert!(first_report.activated && first_report.frontend_closed);
    assert!(second_report.activated && second_report.frontend_closed);
    assert!(!control_socket.exists() && !first_socket.exists() && !second_socket.exists());
    assert!(session_entries().is_empty());
}

#[test]
fn normal_bundle_routes_host_vsock_through_supplied_granted_listener() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new("host-vsock");
    let mut running = spawn_ready_socket_grant_api_launcher(&bundle, &fixture, "host-vsock");
    let worker = only_worker_pid(&running.child);

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT granted host-vsock machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": GRANTED_HOST_VSOCK_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot source should serialize"),
        ),
        204,
        "PUT granted host-vsock boot source",
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
            "PUT granted host-vsock rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
            }),
            "PUT granted host-vsock data drive",
        ),
        (
            "/vsock",
            serde_json::json!({"guest_cid": 3, "uds_path": VSOCK_SOCKET_REF}),
            "PUT granted host-vsock device",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("host-vsock request should serialize"),
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
        "start granted host-vsock guest",
    );
    assert_socket_mode(&fixture.vsock_socket(), 0o600, "granted host-vsock socket");
    assert!(
        child_pids(worker).is_empty(),
        "granted host-vsock must not retain a connector helper"
    );
    wait_for_file_contains(
        &fixture.devices.data,
        GRANTED_HOST_VSOCK_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("host-vsock ready marker should reach data drive: {error}"));

    let mut stream = UnixStream::connect(fixture.vsock_socket())
        .expect("host should connect to the granted main vsock listener");
    stream
        .set_nonblocking(true)
        .expect("host-vsock stream should become nonblocking");
    let connect = format!("CONNECT {GRANTED_HOST_VSOCK_PORT}\n");
    write_all_nonblocking(&mut stream, connect.as_bytes(), PROCESS_TIMEOUT)
        .expect("host-vsock CONNECT request should write");
    let response = read_line_nonblocking(&mut stream, 32, PROCESS_TIMEOUT)
        .expect("host-vsock CONNECT response should arrive");
    let response = std::str::from_utf8(&response).expect("CONNECT response should be UTF-8");
    let local_port = response
        .strip_prefix("OK ")
        .and_then(|value| value.strip_suffix('\n'))
        .and_then(|value| value.parse::<u32>().ok());
    assert!(
        local_port.is_some(),
        "CONNECT response should contain a local port"
    );

    verify_deterministic_stream(&mut stream, GRANTED_HOST_VSOCK_GUEST_SEED, PROCESS_TIMEOUT)
        .expect("guest-to-host deterministic stream should verify");
    write_deterministic_stream(&mut stream, GRANTED_HOST_VSOCK_HOST_SEED, PROCESS_TIMEOUT)
        .expect("host-to-guest deterministic stream should write");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("host-vsock stream should half-close writes");
    wait_for_file_contains(
        &fixture.devices.data,
        GRANTED_HOST_VSOCK_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        let marker = fs::read(&fixture.devices.data).unwrap_or_default();
        panic!(
            "host-vsock success marker should reach data drive: {error}; guest marker: {:?}",
            String::from_utf8_lossy(&marker)
        )
    });
    wait_for_nonblocking_eof(&mut stream, PROCESS_TIMEOUT)
        .expect("guest should half-close and close the host-vsock stream");

    drop(stream);
    stop_running_launcher(&mut running, "granted host-vsock shutdown");
    assert!(!fixture.api_socket().exists());
    assert!(!fixture.vsock_socket().exists());
    assert!(session_entries().is_empty());
}

#[test]
fn normal_bundle_granted_socket_cleanup_preserves_replacements_in_both_death_orders() {
    let bundle = production_bundle();
    recover_session_root(&bundle);

    let launcher_fixture = SocketDirectoryGrantFixture::new("socket-launcher-first");
    let mut launcher_first =
        spawn_ready_socket_grant_api_launcher(&bundle, &launcher_fixture, "socket-launcher-first");
    let launcher_owned = launcher_fixture.api_directory.join("launcher-owned.sock");
    fs::rename(launcher_fixture.api_socket(), &launcher_owned)
        .expect("launcher-first owned socket should move aside");
    let launcher_replacement = UnixListener::bind(launcher_fixture.api_socket())
        .expect("launcher-first replacement socket should bind");
    let worker_pid = only_worker_pid(&launcher_first.child);
    let worker_exit = ProcessExitWatch::new(worker_pid);
    let launcher_pid = i32::try_from(launcher_first.child.id()).expect("launcher PID should fit");
    // SAFETY: This targets the live unreaped launcher while its worker remains
    // bound to the inherited lifecycle endpoint.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGKILL) }, 0);
    let status = launcher_first.wait("granted socket launcher-first SIGKILL");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(
        worker_exit.wait(PROCESS_TIMEOUT),
        "worker should exit after granted socket launcher EOF"
    );
    assert!(
        launcher_fixture.api_socket().exists(),
        "worker cleanup must preserve a replacement socket"
    );
    assert!(launcher_owned.exists());
    assert!(session_entries().is_empty());
    drop(launcher_replacement);

    let worker_fixture = SocketDirectoryGrantFixture::new("socket-worker-first");
    let mut worker_first =
        spawn_ready_socket_grant_api_launcher(&bundle, &worker_fixture, "socket-worker-first");
    let worker_owned = worker_fixture.api_directory.join("worker-owned.sock");
    fs::rename(worker_fixture.api_socket(), &worker_owned)
        .expect("worker-first owned socket should move aside");
    let worker_replacement = UnixListener::bind(worker_fixture.api_socket())
        .expect("worker-first replacement socket should bind");
    let worker_pid = only_worker_pid(&worker_first.child);
    // SAFETY: This targets the live child of the unreaped launcher.
    assert_eq!(unsafe { libc::kill(worker_pid, libc::SIGKILL) }, 0);
    let status = worker_first.wait("granted socket worker-first SIGKILL");
    assert_eq!(status.signal(), None);
    assert_eq!(status.code(), Some(128 + libc::SIGKILL));
    assert!(
        worker_fixture.api_socket().exists(),
        "launcher cleanup must preserve a replacement socket"
    );
    assert!(worker_owned.exists());
    assert!(session_entries().is_empty());
    drop(worker_replacement);
}

#[test]
fn normal_bundle_adopts_delayed_output_grants_by_descriptor_identity() {
    let bundle = production_bundle();
    let fixture = OutputGrantFixture::new("delayed-output");
    let mut running = spawn_ready_output_grant_api_launcher(&bundle, &fixture, "delayed-output");
    fixture.replace_source_pathnames();

    for body in [
        serde_json::json!({"log_path": OUTPUT_METRICS_REF}),
        serde_json::json!({"log_path": OUTPUT_MISSING_REF}),
        serde_json::json!({"log_path": "bangbang-grant:"}),
    ] {
        let response = http_put(
            &running.socket,
            "/logger",
            &serde_json::to_string(&body).expect("logger mismatch should serialize"),
        );
        assert_output_private_grant_fault(&response, &fixture);
    }

    assert_http_status(
        &http_put(
            &running.socket,
            "/logger",
            &serde_json::to_string(&serde_json::json!({
                "log_path": OUTPUT_LOGGER_REF,
                "level": "Info",
                "show_level": true,
            }))
            .expect("logger request should serialize"),
        ),
        204,
        "PUT granted logger",
    );
    assert_http_status(
        &http_put(&running.socket, "/logger", r#"{"show_level":false}"#),
        204,
        "PUT path-free logger update",
    );
    assert_http_status(
        &http_get(&running.socket, "/"),
        200,
        "GET instance after path-free logger update",
    );
    let duplicate_logger = http_put(
        &running.socket,
        "/logger",
        &serde_json::to_string(&serde_json::json!({"log_path": OUTPUT_LOGGER_REF}))
            .expect("duplicate logger should serialize"),
    );
    assert_output_private_grant_fault(&duplicate_logger, &fixture);

    let wrong_serial_role = http_put(
        &running.socket,
        "/serial",
        &serde_json::to_string(&serde_json::json!({
            "serial_out_path": OUTPUT_METRICS_REF,
        }))
        .expect("wrong-role serial should serialize"),
    );
    assert_output_private_grant_fault(&wrong_serial_role, &fixture);
    let wrong_metrics_role = http_put(
        &running.socket,
        "/metrics",
        &serde_json::to_string(&serde_json::json!({
            "metrics_path": OUTPUT_SERIAL_REF,
        }))
        .expect("wrong-role metrics should serialize"),
    );
    assert_output_private_grant_fault(&wrong_metrics_role, &fixture);

    assert_http_status(
        &http_put(
            &running.socket,
            "/metrics",
            &serde_json::to_string(&serde_json::json!({
                "metrics_path": OUTPUT_METRICS_REF,
            }))
            .expect("metrics request should serialize"),
        ),
        204,
        "PUT granted metrics",
    );
    let repeated_metrics = http_put(
        &running.socket,
        "/metrics",
        &serde_json::to_string(&serde_json::json!({
            "metrics_path": OUTPUT_MISSING_REF,
        }))
        .expect("repeated metrics should serialize"),
    );
    assert!(
        repeated_metrics.starts_with("HTTP/1.1 400 "),
        "repeated metrics should reject"
    );
    assert!(repeated_metrics.contains("metrics system is already initialized"));
    assert!(!repeated_metrics.contains(OUTPUT_MISSING_REF));
    assert!(!repeated_metrics.contains("private resource grant failed"));

    assert_http_status(
        &http_put(
            &running.socket,
            "/serial",
            &serde_json::to_string(&serde_json::json!({
                "serial_out_path": OUTPUT_SERIAL_REF,
            }))
            .expect("serial request should serialize"),
        ),
        204,
        "PUT granted serial",
    );
    let duplicate_serial = http_put(
        &running.socket,
        "/serial",
        &serde_json::to_string(&serde_json::json!({
            "serial_out_path": OUTPUT_SERIAL_REF,
        }))
        .expect("duplicate serial should serialize"),
    );
    assert_output_private_grant_fault(&duplicate_serial, &fixture);

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT output-grant machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "initrd_path": path_text(&resources.join("guest-initrd")),
        "boot_args": "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot source should serialize"),
        ),
        204,
        "PUT output-grant boot source",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start output-grant guest",
    );

    wait_for_file_contains(&fixture.opened_serial, GUEST_SERIAL_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| panic!("guest serial output should reach granted file: {error}"));
    let status = running.wait("output-grant guest SYSTEM_OFF");
    assert!(
        status.success(),
        "guest should reach SYSTEM_OFF: {status:?}"
    );
    assert!(!running.socket.exists());
    assert!(session_entries().is_empty());

    fixture.assert_original_outputs();
    fixture.assert_replacement_outputs_unchanged();
}

#[test]
fn normal_bundle_adopts_output_grants_from_config_file_and_startup_cli() {
    let bundle = production_bundle();
    for (case, mode) in [
        ("config-file-output", OutputStartupMode::ConfigFile),
        ("startup-cli-output", OutputStartupMode::StartupCli),
    ] {
        let fixture = OutputStartupGrantFixture::new(&bundle, case, mode);
        let mut command = Command::new(launcher(&bundle));
        command
            .arg(GRANT_MANIFEST_OPTION)
            .arg(&fixture.manifest)
            .arg("--");
        if matches!(mode, OutputStartupMode::StartupCli) {
            command.args(["--log-path", OUTPUT_LOGGER_REF]);
            command.args(["--metrics-path", OUTPUT_METRICS_REF]);
        }
        command.args(["--config-file", OUTPUT_CONFIG_REF, "--no-api"]);

        let output = run_with_timeout(
            &mut command,
            PROCESS_TIMEOUT,
            "startup output-grant guest SYSTEM_OFF",
        );

        assert_output_success(&output, "startup output-grant guest SYSTEM_OFF");
        fixture.assert_output_redacted(&output);
        fixture.outputs.assert_current_outputs();
        assert!(session_entries().is_empty());
    }
}

#[test]
fn normal_bundle_keeps_concurrent_output_grant_sessions_isolated() {
    let bundle = production_bundle();
    let first_fixture = OutputGrantFixture::new("concurrent-output-a");
    let second_fixture = OutputGrantFixture::new("concurrent-output-b");
    let mut first =
        spawn_ready_output_grant_api_launcher(&bundle, &first_fixture, "concurrent-output-a");
    let mut second =
        spawn_ready_output_grant_api_launcher(&bundle, &second_fixture, "concurrent-output-b");
    assert_eq!(session_entries().len(), 2);
    first_fixture.replace_source_pathnames();
    second_fixture.replace_source_pathnames();

    configure_output_grant_session(&bundle, &first, "bangbang_runtime::vmm_action");
    configure_output_grant_session(&bundle, &second, "bangbang_runtime::api_server");

    assert_http_status(
        &http_put(
            &first.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start first concurrent output-grant guest",
    );
    assert_http_status(
        &http_put(
            &second.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start second concurrent output-grant guest",
    );

    for fixture in [&first_fixture, &second_fixture] {
        wait_for_file_contains(&fixture.opened_serial, GUEST_SERIAL_MARKER, PROCESS_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("concurrent guest serial should reach granted file: {error}")
            });
    }
    assert!(first.wait("first concurrent output-grant guest").success());
    assert!(
        second
            .wait("second concurrent output-grant guest")
            .success()
    );
    assert!(session_entries().is_empty());

    first_fixture.assert_original_outputs_with_logger_expectations(false, true);
    second_fixture.assert_original_outputs_with_logger_expectations(true, false);
    first_fixture.assert_replacement_outputs_unchanged();
    second_fixture.assert_replacement_outputs_unchanged();
    let first_logger =
        fs::read(&first_fixture.opened_logger).expect("first concurrent logger should read");
    let second_logger =
        fs::read(&second_fixture.opened_logger).expect("second concurrent logger should read");
    assert!(
        !first_logger
            .windows(b"The API server received".len())
            .any(|window| window == b"The API server received")
    );
    assert!(
        !second_logger
            .windows(b"action=InstanceStart\n".len())
            .any(|window| window == b"action=InstanceStart\n")
    );
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
fn normal_bundle_boots_read_only_pmem_root_from_exact_granted_descriptor() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("pmem-root");
    let pmem_root = fixture
        .rootfs
        .parent()
        .expect("pmem-root fixture should have a parent")
        .join("external-pmem-root.ext4");
    let opened_pmem_root = pmem_root.with_file_name("opened-pmem-root.ext4");
    fs::copy(guest_ext4_rootfs(), &pmem_root).expect("contained pmem-root fixture should copy");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(&fixture.manifest).expect("device grant manifest should read"),
    )
    .expect("device grant manifest should parse");
    manifest["grants"]
        .as_array_mut()
        .expect("device grant manifest grants should be an array")
        .push(serde_json::json!({
            "id": GUEST_PMEM_ROOT_ID,
            "role": "pmem-backing",
            "access": "read-only",
            "source": path_text(&pmem_root),
        }));
    fs::write(
        &fixture.manifest,
        serde_json::to_vec(&manifest).expect("extended device grant manifest should serialize"),
    )
    .expect("extended device grant manifest should write");
    let mut running = spawn_ready_device_grant_api_launcher(&bundle, &fixture, "pmem-root");
    running.sensitive.extend([
        path_text(&pmem_root).to_string(),
        path_text(&opened_pmem_root).to_string(),
        GUEST_PMEM_ROOT_ID.to_string(),
        GUEST_PMEM_ROOT_REF.to_string(),
    ]);
    fixture.replace_source_pathnames();
    fs::rename(&pmem_root, &opened_pmem_root)
        .expect("launcher-opened pmem-root source should move");
    create_sized_file(&pmem_root, 512);

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained pmem-root machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_PMEM_ROOT_RO_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("pmem-root boot request should serialize"),
        ),
        204,
        "PUT contained pmem-root boot source",
    );
    let control = serde_json::json!({
        "drive_id": "control",
        "path_on_host": GUEST_DATA_REF,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/control",
            &serde_json::to_string(&control).expect("pmem-root control drive should serialize"),
        ),
        204,
        "PUT contained pmem-root control drive",
    );
    let root = serde_json::json!({
        "id": "root_pmem",
        "path_on_host": GUEST_PMEM_ROOT_REF,
        "root_device": true,
        "read_only": true,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/root_pmem",
            &serde_json::to_string(&root).expect("pmem root request should serialize"),
        ),
        204,
        "PUT contained read-only pmem root",
    );

    let config = http_get(&running.socket, "/vm/config");
    assert_http_status(&config, 200, "GET contained pmem-root config");
    assert!(config.contains(GUEST_PMEM_ROOT_REF));
    assert!(config.contains(r#""root_device":true"#));
    assert!(config.contains(r#""read_only":true"#));

    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained read-only pmem-root guest",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained read-only pmem root should boot: {error}"));
    assert_eq!(
        file_bytes_at(&fixture.data, 0, DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER.len(),),
        vec![0; DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER.len()],
        "replacement control pathname must not receive the pmem-root guest marker"
    );
    assert_eq!(
        fs::metadata(&pmem_root)
            .expect("replacement pmem-root pathname should remain present")
            .len(),
        512,
        "the worker must boot the launcher-opened rootfs object instead of reopening its replacement pathname"
    );

    stop_running_launcher(&mut running, "contained read-only pmem-root guest");
}

#[test]
fn normal_bundle_live_async_block_grant_swap_uses_preauthorized_open_file() {
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
                "io_engine": "Async",
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
        "is_root_device": false,
        "is_read_only": false,
        "io_engine": "Async",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/data",
            &serde_json::to_string(&replacement).expect("replacement should serialize"),
        ),
        204,
        "same-ID PUT live block grant Sync to Async replacement",
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
    assert!(config.contains(r#""io_engine":"Async""#));

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
fn normal_bundle_hotplugs_async_runtime_block_from_exact_unused_grants() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("runtime-block-hotplug");
    resize_and_write_file_marker_at(&fixture.data, 1024, 0, &[]);
    resize_and_write_file_marker_at(&fixture.replacement, 512, 0, BLOCK_HOTPLUG_HOST_ONE_MARKER);
    resize_and_write_file_marker_at(
        &fixture.hotplug_reuse,
        512,
        0,
        BLOCK_HOTPLUG_HOST_TWO_MARKER,
    );
    let mut running = spawn_ready_device_grant_api_launcher_with_extra_args(
        &bundle,
        &fixture,
        "runtime-block-hotplug",
        &["--enable-pci"],
    );
    fixture.replace_source_pathnames();
    let expected_rootfs_device_id = expected_block_device_id(&fixture.opened_rootfs);
    let serial_file = TestFilePath::new(container_tmp_dir().join(format!(
        "bb-block-id-{:x}-{}.serial",
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
        "PUT contained block-hotplug machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": format!(
            "{DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS} bangbang.block-serial=vda"
        ),
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot request should serialize"),
        ),
        204,
        "PUT contained block-hotplug boot source",
    );
    for (path, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": GUEST_ROOTFS_REF,
                "is_root_device": true,
                "is_read_only": true,
                "io_engine": "Async",
            }),
            "PUT contained block-hotplug rootfs",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Async",
            }),
            "PUT contained block-hotplug control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive request should serialize"),
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
        "PUT contained PCI Async block identity serial output",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained block-hotplug guest",
    );
    wait_for_file_contains(serial_file.path(), BLOCK_SERIAL_END_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("contained guest should report rootfs block identity: {error}")
        });
    assert_block_serial_report(
        serial_file.path(),
        &expected_rootfs_device_id,
        "contained PCI Async rootfs",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        BLOCK_HOTPLUG_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained block-hotplug guest should become ready: {error}"));

    let wrong_access = serde_json::json!({
        "drive_id": "hotdata",
        "path_on_host": GUEST_REPLACEMENT_REF,
        "is_root_device": false,
        "is_read_only": true,
    });
    let wrong_access_response = http_put(
        &running.socket,
        "/drives/hotdata",
        &serde_json::to_string(&wrong_access).expect("wrong-access request should serialize"),
    );
    assert_device_private_grant_fault(&wrong_access_response, &fixture);
    let unchanged = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &unchanged,
        200,
        "GET /vm/config after failed contained runtime grant claim",
    );
    assert!(!unchanged.contains(r#""drive_id":"hotdata""#));

    let first = serde_json::json!({
        "drive_id": "hotdata",
        "path_on_host": GUEST_REPLACEMENT_REF,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
        "io_engine": "Async",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/hotdata",
            &serde_json::to_string(&first).expect("first runtime drive should serialize"),
        ),
        204,
        "runtime PUT contained first block after retained grant failure",
    );
    wait_for_file_prefix(
        &fixture.opened_replacement,
        BLOCK_HOTPLUG_GUEST_ONE_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained first runtime block should complete I/O: {error}"));
    wait_for_file_prefix(
        &fixture.opened_data,
        BLOCK_HOTPLUG_FIRST_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained guest should remove first PCI function: {error}"));

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained guest before block reuse",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "paused DELETE contained first runtime block",
    );
    let removed = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &removed,
        200,
        "GET /vm/config after contained runtime DELETE",
    );
    assert!(!removed.contains(r#""drive_id":"hotdata""#));

    let second = serde_json::json!({
        "drive_id": "hotdata",
        "path_on_host": GUEST_HOTPLUG_REUSE_REF,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
        "io_engine": "Async",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/hotdata",
            &serde_json::to_string(&second).expect("reused runtime drive should serialize"),
        ),
        204,
        "paused PUT contained reused runtime block",
    );
    let reused = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &reused,
        200,
        "GET /vm/config after contained runtime block reuse",
    );
    assert!(reused.contains(GUEST_HOTPLUG_REUSE_REF));
    assert!(!reused.contains(GUEST_REPLACEMENT_REF));
    assert!(reused.contains(r#""io_engine":"Async""#));
    resize_and_write_file_marker_at(
        &fixture.opened_data,
        1024,
        512,
        BLOCK_HOTPLUG_CONTINUE_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained guest after block reuse",
    );

    wait_for_file_prefix(
        &fixture.opened_hotplug_reuse,
        BLOCK_HOTPLUG_GUEST_TWO_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained reused runtime block should complete I/O: {error}"));
    wait_for_file_prefix(
        &fixture.opened_data,
        BLOCK_HOTPLUG_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained guest should remove reused PCI function: {error}"));
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "final DELETE contained runtime block",
    );

    for (planted, marker) in [
        (&fixture.data, BLOCK_HOTPLUG_SUCCESS_MARKER),
        (&fixture.replacement, BLOCK_HOTPLUG_GUEST_ONE_MARKER),
        (&fixture.hotplug_reuse, BLOCK_HOTPLUG_GUEST_TWO_MARKER),
    ] {
        assert_eq!(
            file_bytes_at(planted, 0, marker.len()),
            vec![0; marker.len()],
            "replacement source pathname must not receive contained runtime block writes"
        );
    }

    stop_running_launcher(&mut running, "contained runtime block hotplug guest");
}

#[test]
fn normal_bundle_hotplugs_mmds_network_without_vmnet_authority() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("runtime-network-hotplug");
    resize_and_write_file_marker_at(&fixture.data, 1536, 0, &[]);
    let mut running = spawn_ready_device_grant_api_launcher_with_extra_args(
        &bundle,
        &fixture,
        "runtime-network-hotplug",
        &["--enable-pci"],
    );
    fixture.replace_source_pathnames();

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained network-hotplug machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_NETWORK_HOTPLUG_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot request should serialize"),
        ),
        204,
        "PUT contained network-hotplug boot source",
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
            "PUT contained network-hotplug rootfs",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
            }),
            "PUT contained network-hotplug control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive request should serialize"),
            ),
            204,
            context,
        );
    }
    let network_body =
        r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"06:00:00:00:00:42"}"#;
    assert_http_status(
        &http_put(&running.socket, "/network-interfaces/eth0", network_body),
        204,
        "PUT contained startup MMDS network",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/mmds/config",
            r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
        ),
        204,
        "PUT contained network-hotplug MMDS config",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/mmds",
            r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_GUEST_VALUE"}}"#,
        ),
        204,
        "PUT contained network-hotplug MMDS data",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained network-hotplug guest",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        NETWORK_HOTPLUG_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("contained network-hotplug guest should remove its startup function: {error}")
    });

    let denied = http_put(
        &running.socket,
        "/network-interfaces/private_iface",
        r#"{"iface_id":"private_iface","host_dev_name":"vmnet:bridged:private_bridge","guest_mac":"06:00:00:00:00:43"}"#,
    );
    assert_http_status(&denied, 400, "contained runtime vmnet denial");
    assert!(denied.contains(r#"{"fault_message":"system host networking is not authorized"}"#));
    assert!(!denied.contains("private_iface"));
    assert!(!denied.contains("private_bridge"));
    assert!(!denied.contains("06:00:00:00:00:43"));
    let unchanged = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &unchanged,
        200,
        "GET /vm/config after contained runtime vmnet denial",
    );
    assert!(unchanged.contains(r#""iface_id":"eth0""#));
    assert!(!unchanged.contains("private_iface"));

    assert_http_status(
        &http_request(&running.socket, "DELETE", "/network-interfaces/eth0", ""),
        204,
        "DELETE contained startup MMDS network",
    );
    assert_http_status(
        &http_put(&running.socket, "/network-interfaces/eth0", network_body),
        204,
        "runtime PUT contained first MMDS network",
    );
    resize_and_write_file_marker_at(
        &fixture.opened_data,
        1536,
        512,
        NETWORK_HOTPLUG_FIRST_CONTINUE_MARKER,
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        NETWORK_HOTPLUG_FIRST_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("contained first runtime network should exchange MMDS traffic: {error}")
    });

    assert_http_status(
        &http_request(&running.socket, "DELETE", "/network-interfaces/eth0", ""),
        204,
        "DELETE contained first runtime MMDS network",
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained guest before network reuse",
    );
    assert_http_status(
        &http_put(&running.socket, "/network-interfaces/eth0", network_body),
        204,
        "paused PUT contained reused MMDS network",
    );
    let reused = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &reused,
        200,
        "GET /vm/config after contained runtime network reuse",
    );
    assert!(reused.contains(r#""iface_id":"eth0""#));
    resize_and_write_file_marker_at(
        &fixture.opened_data,
        1536,
        1024,
        NETWORK_HOTPLUG_SECOND_CONTINUE_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained guest after network reuse",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        NETWORK_HOTPLUG_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("contained reused runtime network should preserve PCI/MMDS identity: {error}")
    });
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/network-interfaces/eth0", ""),
        204,
        "final DELETE contained runtime MMDS network",
    );
    let removed = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &removed,
        200,
        "GET /vm/config after contained final network DELETE",
    );
    assert!(removed.contains(r#""network-interfaces":[]"#));
    assert_eq!(
        file_bytes_at(&fixture.data, 0, NETWORK_HOTPLUG_SUCCESS_MARKER.len()),
        vec![0; NETWORK_HOTPLUG_SUCCESS_MARKER.len()],
        "replacement source pathname must not receive contained network markers"
    );

    stop_running_launcher(&mut running, "contained runtime network hotplug guest");
}

#[test]
fn normal_bundle_hotplugs_flushes_and_reuses_runtime_pmem_from_exact_unused_grants() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("runtime-pmem-hotplug");
    resize_and_write_file_marker_at(&fixture.data, 1024, 0, &[]);
    resize_and_write_file_marker_at(
        &fixture.pmem,
        PMEM_BACKING_LEN,
        0,
        PMEM_HOTPLUG_HOST_ONE_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.pmem_reuse,
        PMEM_BACKING_LEN,
        0,
        PMEM_HOTPLUG_HOST_TWO_MARKER,
    );
    let mut running = spawn_ready_device_grant_api_launcher_with_extra_args(
        &bundle,
        &fixture,
        "runtime-pmem-hotplug",
        &["--enable-pci"],
    );
    fixture.replace_source_pathnames();

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained pmem-hotplug machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_PMEM_HOTPLUG_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot request should serialize"),
        ),
        204,
        "PUT contained pmem-hotplug boot source",
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
            "PUT contained pmem-hotplug rootfs",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
            }),
            "PUT contained pmem-hotplug control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("drive request should serialize"),
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
        "start contained pmem-hotplug guest",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        PMEM_HOTPLUG_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained pmem-hotplug guest should become ready: {error}"));

    let wrong_access = serde_json::json!({
        "id": "hotpmem",
        "path_on_host": GUEST_PMEM_REF,
        "read_only": true,
    });
    let wrong_access_response = http_put(
        &running.socket,
        "/pmem/hotpmem",
        &serde_json::to_string(&wrong_access).expect("wrong-access request should serialize"),
    );
    assert_device_private_grant_fault(&wrong_access_response, &fixture);
    let unchanged = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &unchanged,
        200,
        "GET /vm/config after failed contained runtime pmem grant claim",
    );
    assert!(!unchanged.contains(r#""id":"hotpmem""#));

    let first = serde_json::json!({
        "id": "hotpmem",
        "path_on_host": GUEST_PMEM_REF,
        "read_only": false,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/hotpmem",
            &serde_json::to_string(&first).expect("first runtime pmem should serialize"),
        ),
        204,
        "runtime PUT contained first pmem after retained grant failure",
    );
    wait_for_file_prefix(
        &fixture.opened_data,
        PMEM_HOTPLUG_FIRST_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("contained first runtime pmem should flush: {error}"));
    assert_eq!(
        file_bytes_at(
            &fixture.opened_pmem,
            PMEM_GUEST_FLUSH_OFFSET,
            PMEM_HOTPLUG_GUEST_ONE_MARKER.len(),
        ),
        PMEM_HOTPLUG_GUEST_ONE_MARKER,
        "first contained runtime pmem flush should reach the granted object"
    );

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained guest before pmem reuse",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/pmem/hotpmem", ""),
        204,
        "paused DELETE contained first runtime pmem",
    );
    let removed = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &removed,
        200,
        "GET /vm/config after contained runtime pmem DELETE",
    );
    assert!(!removed.contains(r#""id":"hotpmem""#));

    let second = serde_json::json!({
        "id": "hotpmem",
        "path_on_host": GUEST_PMEM_REUSE_REF,
        "read_only": false,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/hotpmem",
            &serde_json::to_string(&second).expect("reused runtime pmem should serialize"),
        ),
        204,
        "paused PUT contained reused runtime pmem",
    );
    let reused = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &reused,
        200,
        "GET /vm/config after contained runtime pmem reuse",
    );
    assert!(reused.contains(GUEST_PMEM_REUSE_REF));
    assert!(!reused.contains(GUEST_PMEM_REF));
    resize_and_write_file_marker_at(
        &fixture.opened_data,
        1024,
        512,
        PMEM_HOTPLUG_CONTINUE_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained guest after pmem reuse",
    );

    wait_for_file_prefix(
        &fixture.opened_data,
        PMEM_HOTPLUG_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("contained reused runtime pmem should preserve slot and range: {error}")
    });
    assert_eq!(
        file_bytes_at(
            &fixture.opened_pmem_reuse,
            PMEM_GUEST_FLUSH_OFFSET,
            PMEM_HOTPLUG_GUEST_TWO_MARKER.len(),
        ),
        PMEM_HOTPLUG_GUEST_TWO_MARKER,
        "reused contained runtime pmem flush should reach the second granted object"
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/pmem/hotpmem", ""),
        204,
        "final DELETE contained runtime pmem",
    );

    for (planted, marker) in [
        (&fixture.data, PMEM_HOTPLUG_SUCCESS_MARKER),
        (&fixture.pmem, PMEM_HOTPLUG_GUEST_ONE_MARKER),
        (&fixture.pmem_reuse, PMEM_HOTPLUG_GUEST_TWO_MARKER),
    ] {
        assert_eq!(
            file_bytes_at(planted, 0, marker.len()),
            vec![0; marker.len()],
            "replacement source pathname must not receive contained runtime pmem writes"
        );
    }

    stop_running_launcher(&mut running, "contained runtime pmem hotplug guest");
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
fn signed_contained_block_device_uses_launcher_control_broker() {
    let bundle = grant_test_bundle();
    let launcher_entitlements = codesign_entitlements(&bundle);
    let worker_entitlements = codesign_entitlements(&worker_bundle(&bundle));
    assert!(!launcher_entitlements.contains("com.apple.security.app-sandbox"));
    assert!(!launcher_entitlements.contains("com.apple.security.hypervisor"));
    assert_eq!(worker_entitlements.matches("<key>").count(), 2);
    assert!(worker_entitlements.contains("<key>com.apple.security.app-sandbox</key>"));
    assert!(worker_entitlements.contains("<key>com.apple.security.hypervisor</key>"));
    let mut media = MacosVirtualBlock::create(MacosVirtualBlockAccess::ReadWrite)
        .expect("temporary block media should attach read-write");
    let logical_block_size = usize::try_from(
        media
            .logical_block_size()
            .expect("temporary media should report logical geometry"),
    )
    .expect("logical block size should fit usize");
    let block_count = media
        .block_count()
        .expect("temporary media should report a block count");
    let identity = media
        .identity()
        .expect("temporary media should report exact identity");
    assert_ne!(identity.device(), 0);
    assert_ne!(identity.inode(), 0);
    assert_ne!(identity.target_device(), 0);
    assert_eq!(
        media.len().expect("temporary media should report capacity"),
        u64::try_from(logical_block_size)
            .expect("block size should fit u64")
            .checked_mul(block_count)
            .expect("temporary media capacity should not overflow")
    );
    assert!(logical_block_size >= BLOCK_CONTROL_INITIAL_MARKER.len());
    assert!(logical_block_size >= BLOCK_CONTROL_WRITTEN_MARKER.len());

    let mut initial_block = vec![0_u8; logical_block_size];
    initial_block[..BLOCK_CONTROL_INITIAL_MARKER.len()]
        .copy_from_slice(BLOCK_CONTROL_INITIAL_MARKER);
    media
        .write_at(0, &initial_block)
        .expect("initial block marker should persist before launch");

    let root = TestDir::new("block-control-grant");
    let manifest = fs::canonicalize(root.path())
        .expect("block-control fixture should canonicalize")
        .join("grant-manifest.json");
    let device_path = media
        .device_path()
        .expect("attached media should expose a device path")
        .to_path_buf();
    let manifest_json = serde_json::json!({
        "version": 1,
        "grants": [{
            "id": BLOCK_CONTROL_GRANT_ID,
            "role": "drive-backing",
            "access": "read-write",
            "source": path_text(&device_path),
        }],
    });
    fs::write(
        &manifest,
        serde_json::to_vec(&manifest_json).expect("block-control manifest should serialize"),
    )
    .expect("block-control manifest should write");

    let output = run_with_timeout(
        Command::new(launcher(&bundle))
            .arg(GRANT_MANIFEST_OPTION)
            .arg(&manifest)
            .arg("--")
            .arg(GRANT_PROBE_OPTION)
            .arg("block-control"),
        PROCESS_TIMEOUT,
        "signed block-control grant probe",
    );
    assert_output_success(&output, "signed block-control grant probe");
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for sensitive in [
        path_text(&device_path),
        path_text(&manifest),
        BLOCK_CONTROL_GRANT_ID,
        BLOCK_CONTROL_GRANT_REF,
    ] {
        assert!(!diagnostics.contains(sensitive));
    }

    media
        .reattach(MacosVirtualBlockAccess::ReadOnly)
        .expect("completed broker session should release media for read-only reattach");
    assert_eq!(
        media
            .read_at(0, logical_block_size)
            .expect("initial block should remain readable"),
        initial_block
    );
    let mut expected_written = vec![0_u8; logical_block_size];
    expected_written[..BLOCK_CONTROL_WRITTEN_MARKER.len()]
        .copy_from_slice(BLOCK_CONTROL_WRITTEN_MARKER);
    assert_eq!(
        media
            .read_at(
                u64::try_from(logical_block_size)
                    .expect("block size should fit u64")
                    .checked_mul(BLOCK_CONTROL_WRITE_BLOCK)
                    .expect("marker offset should not overflow"),
                logical_block_size,
            )
            .expect("broker-synchronized block should persist"),
        expected_written
    );
    media
        .cleanup()
        .expect("temporary block media should detach and clean up");
}

#[test]
fn contained_worker_maps_unlinked_shared_guest_memory_with_hvf() {
    let bundle = grant_test_bundle();
    let fixture = GrantProbeFixture::new("shared-memory", false);
    let output = run_grant_probe(&bundle, &fixture, "shared-memory");

    assert_output_success(&output, "contained shared guest-memory probe");
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
    let regular = fs::File::open(&config).expect("probe config should open");
    let directory = fs::File::open(fixture.path()).expect("probe directory should open");
    let (stream, _stream_peer) = UnixStream::pair().expect("probe stream pair should open");
    let (datagram, _datagram_peer) = UnixDatagram::pair().expect("probe datagram pair should open");
    let mut pipe = [-1; 2];
    // SAFETY: `pipe` is writable storage for exactly two fresh descriptors.
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    // SAFETY: Both successful pipe descriptors transfer ownership exactly once.
    let pipe_reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    // SAFETY: This is the distinct second descriptor returned by the same call.
    let _pipe_writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };

    for (kind, descriptor) in [
        ("regular file", regular.as_raw_fd()),
        ("directory", directory.as_raw_fd()),
        ("stream socket", stream.as_raw_fd()),
        ("datagram socket", datagram.as_raw_fd()),
        ("pipe", pipe_reader.as_raw_fd()),
    ] {
        assert_unexpected_descriptor_closed(&bundle, descriptor, kind);
    }
}

fn assert_unexpected_descriptor_closed(bundle: &Path, source: libc::c_int, kind: &str) {
    // SAFETY: `source` remains live and the returned descriptor is independently owned.
    let inherited = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 200) };
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
        bundle,
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
        "closed {kind} descriptor should fail at read: {stderr}"
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
    let (_broker_parent, broker_child_endpoint) =
        UnixDatagram::pair().expect("broker socketpair should open");
    let (_vhost_broker_parent, vhost_broker_child_endpoint) =
        UnixDatagram::pair().expect("vhost broker socketpair should open");
    let (_block_control_parent, block_control_child_endpoint) =
        UnixDatagram::pair().expect("block-control socketpair should open");
    parent
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("bootstrap read timeout should set");
    let child_fd = child_endpoint.as_raw_fd();
    let grant_child_fd = grant_child_endpoint.as_raw_fd();
    let broker_child_fd = broker_child_endpoint.as_raw_fd();
    let vhost_broker_child_fd = vhost_broker_child_endpoint.as_raw_fd();
    let block_control_child_fd = block_control_child_endpoint.as_raw_fd();
    let mut command = Command::new(worker_executable(&bundle));
    command
        .env_clear()
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
            if libc::dup2(broker_child_fd, SOCKET_BROKER_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(vhost_broker_child_fd, VHOST_USER_BROKER_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(block_control_child_fd, BLOCK_CONTROL_BROKER_FD) < 0 {
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
    drop(broker_child_endpoint);
    drop(vhost_broker_child_endpoint);
    drop(block_control_child_endpoint);

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
        message: Message::Start(WorkerPolicy::new(501, 20, 2048, None, false)),
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

#[derive(Debug, Clone)]
struct SnapshotArtifactSet {
    state: PathBuf,
    memory: PathBuf,
    root: PathBuf,
}

#[derive(Debug)]
struct SnapshotSourceGrantFixture {
    _root: TestDir,
    manifest: PathBuf,
    kernel: PathBuf,
    root: PathBuf,
    metrics: PathBuf,
    state_directory: PathBuf,
    memory_directory: PathBuf,
    opened_kernel: PathBuf,
    opened_root: PathBuf,
    opened_metrics: PathBuf,
    opened_state_directory: PathBuf,
    opened_memory_directory: PathBuf,
}

impl SnapshotSourceGrantFixture {
    fn new(case: &str) -> Self {
        let root = TestDir::new(&format!("snapshot-source-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("snapshot source root should canonicalize");
        let manifest = canonical_root.join("grant-manifest.json");
        let kernel = canonical_root.join("snapshot-kernel.image");
        let root_backing = canonical_root.join("snapshot-root.img");
        let metrics = canonical_root.join("snapshot.metrics");
        let state_directory = canonical_root.join("state-output");
        let memory_directory = canonical_root.join("memory-output");
        let opened_kernel = canonical_root.join("opened-snapshot-kernel.image");
        let opened_root = canonical_root.join("opened-snapshot-root.img");
        let opened_metrics = canonical_root.join("opened-snapshot.metrics");
        let opened_state_directory = canonical_root.join("opened-state-output");
        let opened_memory_directory = canonical_root.join("opened-memory-output");

        fs::write(&kernel, snapshot_continuity_guest_image())
            .expect("snapshot guest image should write");
        create_sized_file(&root_backing, 512);
        fs::write(&metrics, b"").expect("snapshot metrics fixture should write");
        fs::create_dir(&state_directory).expect("state output directory should create");
        fs::create_dir(&memory_directory).expect("memory output directory should create");
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": SNAPSHOT_KERNEL_ID,
                    "role": "kernel-image",
                    "access": "read-only",
                    "source": path_text(&kernel),
                },
                {
                    "id": SNAPSHOT_ROOT_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&root_backing),
                },
                {
                    "id": SNAPSHOT_METRICS_ID,
                    "role": "metrics-sink",
                    "access": "write-only",
                    "source": path_text(&metrics),
                },
                {
                    "id": SNAPSHOT_STATE_OUTPUT_ID,
                    "role": "snapshot-output-directory",
                    "access": "create-children",
                    "source": path_text(&state_directory),
                },
                {
                    "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                    "role": "snapshot-output-directory",
                    "access": "create-children",
                    "source": path_text(&memory_directory),
                },
            ],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("snapshot manifest should serialize"),
        )
        .expect("snapshot manifest should write");

        Self {
            _root: root,
            manifest,
            kernel,
            root: root_backing,
            metrics,
            state_directory,
            memory_directory,
            opened_kernel,
            opened_root,
            opened_metrics,
            opened_state_directory,
            opened_memory_directory,
        }
    }

    fn replace_source_file_pathnames(&self) {
        for (source, opened) in [
            (&self.kernel, &self.opened_kernel),
            (&self.root, &self.opened_root),
            (&self.metrics, &self.opened_metrics),
        ] {
            fs::rename(source, opened).expect("launcher-opened snapshot file should move");
        }
        fs::write(&self.kernel, b"replacement kernel must not boot")
            .expect("replacement snapshot kernel should write");
        create_sized_file(&self.root, 512);
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement metrics should write");
    }

    fn artifacts(&self) -> SnapshotArtifactSet {
        self.artifacts_with_children(SNAPSHOT_STATE_CHILD, SNAPSHOT_MEMORY_CHILD)
    }

    fn repeated_artifacts(&self) -> SnapshotArtifactSet {
        self.artifacts_with_children(SNAPSHOT_REPEAT_STATE_CHILD, SNAPSHOT_REPEAT_MEMORY_CHILD)
    }

    fn artifacts_with_children(
        &self,
        state_child: &str,
        memory_child: &str,
    ) -> SnapshotArtifactSet {
        SnapshotArtifactSet {
            state: self.state_directory.join(state_child),
            memory: self.memory_directory.join(memory_child),
            root: self.opened_root.clone(),
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.manifest),
            path_text(&self.kernel),
            path_text(&self.root),
            path_text(&self.metrics),
            path_text(&self.state_directory),
            path_text(&self.memory_directory),
            path_text(&self.opened_kernel),
            path_text(&self.opened_root),
            path_text(&self.opened_metrics),
            path_text(&self.opened_state_directory),
            path_text(&self.opened_memory_directory),
            SNAPSHOT_KERNEL_ID,
            SNAPSHOT_ROOT_ID,
            SNAPSHOT_METRICS_ID,
            SNAPSHOT_STATE_OUTPUT_ID,
            SNAPSHOT_MEMORY_OUTPUT_ID,
            SNAPSHOT_KERNEL_REF,
            SNAPSHOT_ROOT_REF,
            SNAPSHOT_METRICS_REF,
            SNAPSHOT_STATE_OUTPUT_REF,
            SNAPSHOT_MEMORY_OUTPUT_REF,
            SNAPSHOT_REPEAT_STATE_OUTPUT_REF,
            SNAPSHOT_REPEAT_MEMORY_OUTPUT_REF,
            SNAPSHOT_STATE_CHILD,
            SNAPSHOT_MEMORY_CHILD,
            SNAPSHOT_REPEAT_STATE_CHILD,
            SNAPSHOT_REPEAT_MEMORY_CHILD,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#[derive(Debug)]
struct SnapshotInputGrantFixture {
    _root: TestDir,
    manifest: PathBuf,
    sources: SnapshotArtifactSet,
    opened: SnapshotArtifactSet,
}

impl SnapshotInputGrantFixture {
    fn new(case: &str, sources: SnapshotArtifactSet) -> Self {
        let root = TestDir::new(&format!("snapshot-input-{case}"));
        let manifest = fs::canonicalize(root.path())
            .expect("snapshot input root should canonicalize")
            .join("grant-manifest.json");
        let opened = SnapshotArtifactSet {
            state: replacement_opened_path(&sources.state, case),
            memory: replacement_opened_path(&sources.memory, case),
            root: sources.root.clone(),
        };
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": SNAPSHOT_STATE_INPUT_ID,
                    "role": "snapshot-state-input",
                    "access": "read-only",
                    "source": path_text(&sources.state),
                },
                {
                    "id": SNAPSHOT_MEMORY_INPUT_ID,
                    "role": "snapshot-memory-input",
                    "access": "read-only",
                    "source": path_text(&sources.memory),
                },
                {
                    "id": SNAPSHOT_ROOT_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&sources.root),
                },
            ],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("snapshot input manifest should serialize"),
        )
        .expect("snapshot input manifest should write");
        Self {
            _root: root,
            manifest,
            sources,
            opened,
        }
    }

    fn replace_source_pathnames(&self) -> SnapshotArtifactSet {
        for (source, opened) in [
            (&self.sources.state, &self.opened.state),
            (&self.sources.memory, &self.opened.memory),
        ] {
            fs::rename(source, opened).expect("launcher-opened snapshot input should move");
        }
        fs::write(&self.sources.state, b"replacement state must not load")
            .expect("replacement snapshot state should write");
        fs::write(&self.sources.memory, b"replacement memory must not load")
            .expect("replacement snapshot memory should write");
        self.opened.clone()
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.manifest),
            path_text(&self.sources.state),
            path_text(&self.sources.memory),
            path_text(&self.sources.root),
            path_text(&self.opened.state),
            path_text(&self.opened.memory),
            path_text(&self.opened.root),
            SNAPSHOT_STATE_INPUT_ID,
            SNAPSHOT_MEMORY_INPUT_ID,
            SNAPSHOT_ROOT_ID,
            SNAPSHOT_STATE_INPUT_REF,
            SNAPSHOT_MEMORY_INPUT_REF,
            SNAPSHOT_ROOT_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#[derive(Debug)]
struct SnapshotDescribeGrantFixture {
    _root: TestDir,
    manifest: PathBuf,
    state: PathBuf,
}

impl SnapshotDescribeGrantFixture {
    fn new(case: &str, state: &Path, correct_role: bool) -> Self {
        let root = TestDir::new(&format!("snapshot-describe-{case}"));
        let manifest = fs::canonicalize(root.path())
            .expect("snapshot describe root should canonicalize")
            .join("grant-manifest.json");
        let role = if correct_role {
            "snapshot-describe-input"
        } else {
            "snapshot-state-input"
        };
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [{
                "id": SNAPSHOT_DESCRIBE_INPUT_ID,
                "role": role,
                "access": "read-only",
                "source": path_text(state),
            }],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json)
                .expect("snapshot describe manifest should serialize"),
        )
        .expect("snapshot describe manifest should write");
        Self {
            _root: root,
            manifest,
            state: state.to_path_buf(),
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.manifest),
            path_text(&self.state),
            SNAPSHOT_DESCRIBE_INPUT_ID,
            SNAPSHOT_DESCRIBE_INPUT_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

fn replacement_opened_path(source: &Path, case: &str) -> PathBuf {
    let name = source
        .file_name()
        .expect("snapshot source should have a file name")
        .to_string_lossy();
    source.with_file_name(format!("opened-{case}-{name}"))
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
    hotplug_reuse: PathBuf,
    pmem: PathBuf,
    pmem_reuse: PathBuf,
    read_only_data: PathBuf,
    opened_rootfs: PathBuf,
    opened_data: PathBuf,
    opened_replacement: PathBuf,
    opened_hotplug_reuse: PathBuf,
    opened_pmem: PathBuf,
    opened_pmem_reuse: PathBuf,
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
        let hotplug_reuse = canonical_root.join("external-hotplug-reuse.img");
        let pmem = canonical_root.join("external-pmem.img");
        let pmem_reuse = canonical_root.join("external-pmem-reuse.img");
        let read_only_data = canonical_root.join("external-read-only-data.img");
        let opened_rootfs = canonical_root.join("opened-rootfs.ext4");
        let opened_data = canonical_root.join("opened-data.img");
        let opened_replacement = canonical_root.join("opened-replacement.img");
        let opened_hotplug_reuse = canonical_root.join("opened-hotplug-reuse.img");
        let opened_pmem = canonical_root.join("opened-pmem.img");
        let opened_pmem_reuse = canonical_root.join("opened-pmem-reuse.img");
        let opened_read_only_data = canonical_root.join("opened-read-only-data.img");
        let manifest = canonical_root.join("grant-manifest.json");

        fs::copy(guest_ext4_rootfs(), &rootfs).expect("external rootfs fixture should copy");
        create_sized_file(&data, 512);
        create_sized_file(&replacement, 512);
        create_sized_file(&hotplug_reuse, 512);
        create_pmem_file(&pmem, PMEM_HOST_MARKER);
        create_pmem_file(&pmem_reuse, PMEM_HOTPLUG_HOST_TWO_MARKER);
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
                    "id": GUEST_HOTPLUG_REUSE_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&hotplug_reuse),
                },
                {
                    "id": GUEST_PMEM_ID,
                    "role": "pmem-backing",
                    "access": "read-write",
                    "source": path_text(&pmem),
                },
                {
                    "id": GUEST_PMEM_REUSE_ID,
                    "role": "pmem-backing",
                    "access": "read-write",
                    "source": path_text(&pmem_reuse),
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
            hotplug_reuse,
            pmem,
            pmem_reuse,
            read_only_data,
            opened_rootfs,
            opened_data,
            opened_replacement,
            opened_hotplug_reuse,
            opened_pmem,
            opened_pmem_reuse,
            opened_read_only_data,
            manifest,
        }
    }

    fn replace_source_pathnames(&self) {
        for (source, opened) in [
            (&self.rootfs, &self.opened_rootfs),
            (&self.data, &self.opened_data),
            (&self.replacement, &self.opened_replacement),
            (&self.hotplug_reuse, &self.opened_hotplug_reuse),
            (&self.pmem, &self.opened_pmem),
            (&self.pmem_reuse, &self.opened_pmem_reuse),
            (&self.read_only_data, &self.opened_read_only_data),
        ] {
            fs::rename(source, opened).expect("launcher-opened source should move");
        }
        create_sized_file(&self.rootfs, 512);
        create_sized_file(&self.data, 512);
        create_sized_file(&self.replacement, 512);
        create_sized_file(&self.hotplug_reuse, 512);
        create_sized_file(&self.pmem, PMEM_BACKING_LEN);
        create_sized_file(&self.pmem_reuse, PMEM_BACKING_LEN);
        create_sized_file(&self.read_only_data, 512);
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.rootfs),
            path_text(&self.data),
            path_text(&self.replacement),
            path_text(&self.hotplug_reuse),
            path_text(&self.pmem),
            path_text(&self.pmem_reuse),
            path_text(&self.read_only_data),
            path_text(&self.opened_rootfs),
            path_text(&self.opened_data),
            path_text(&self.opened_replacement),
            path_text(&self.opened_hotplug_reuse),
            path_text(&self.opened_pmem),
            path_text(&self.opened_pmem_reuse),
            path_text(&self.opened_read_only_data),
            path_text(&self.manifest),
            GUEST_ROOTFS_ID,
            GUEST_DATA_ID,
            GUEST_REPLACEMENT_ID,
            GUEST_HOTPLUG_REUSE_ID,
            GUEST_PMEM_ID,
            GUEST_PMEM_REUSE_ID,
            GUEST_PMEM_ROOT_ID,
            GUEST_READ_ONLY_DATA_ID,
            GUEST_ROOTFS_REF,
            GUEST_DATA_REF,
            GUEST_REPLACEMENT_REF,
            GUEST_HOTPLUG_REUSE_REF,
            GUEST_PMEM_REF,
            GUEST_PMEM_REUSE_REF,
            GUEST_PMEM_ROOT_REF,
            GUEST_READ_ONLY_DATA_REF,
            std::str::from_utf8(PMEM_HOST_MARKER).expect("pmem marker should be UTF-8"),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#[derive(Debug)]
struct SocketDirectoryGrantFixture {
    devices: GuestDeviceGrantFixture,
    _socket_root: TestDir,
    api_directory: PathBuf,
    vsock_directory: PathBuf,
    vhost_user_directory: PathBuf,
}

impl SocketDirectoryGrantFixture {
    fn new(case: &str) -> Self {
        Self::build(case, false)
    }

    fn new_with_vhost_user(case: &str) -> Self {
        Self::build(case, true)
    }

    fn build(case: &str, include_vhost_user: bool) -> Self {
        let devices = GuestDeviceGrantFixture::new(case);
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket_root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbs-{}-{socket_id}", std::process::id())),
        );
        fs::create_dir(socket_root.path()).expect("short socket root should be created");
        let api_directory = socket_root.path().join("a");
        let vsock_directory = socket_root.path().join("v");
        let vhost_user_directory = socket_root.path().join("u");
        fs::create_dir(&api_directory).expect("API socket directory should be created");
        fs::create_dir(&vsock_directory).expect("vsock socket directory should be created");
        fs::create_dir(&vhost_user_directory)
            .expect("vhost-user socket directory should be created");

        let mut manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(&devices.manifest).expect("device grant manifest should read"),
        )
        .expect("device grant manifest should parse");
        let grants = manifest
            .get_mut("grants")
            .and_then(serde_json::Value::as_array_mut)
            .expect("device grant manifest should contain grants");
        grants.extend([
            serde_json::json!({
                "id": API_SOCKET_DIRECTORY_ID,
                "role": "api-socket-directory",
                "access": "create-children",
                "source": path_text(&api_directory),
            }),
            serde_json::json!({
                "id": VSOCK_SOCKET_DIRECTORY_ID,
                "role": "vsock-socket-directory",
                "access": "create-children",
                "source": path_text(&vsock_directory),
            }),
        ]);
        if include_vhost_user {
            grants.push(serde_json::json!({
                "id": VHOST_USER_SOCKET_DIRECTORY_ID,
                "role": "vhost-user-socket-directory",
                "access": "connect-children",
                "source": path_text(&vhost_user_directory),
            }));
        }
        fs::write(
            &devices.manifest,
            serde_json::to_vec(&manifest).expect("socket grant manifest should serialize"),
        )
        .expect("socket grant manifest should write");

        Self {
            devices,
            _socket_root: socket_root,
            api_directory,
            vsock_directory,
            vhost_user_directory,
        }
    }

    fn api_socket(&self) -> PathBuf {
        self.api_directory.join(API_SOCKET_CHILD)
    }

    fn vsock_socket(&self) -> PathBuf {
        self.vsock_directory.join(VSOCK_SOCKET_CHILD)
    }

    fn vsock_port_path(&self, port: u32) -> PathBuf {
        let mut path = self.vsock_socket().into_os_string();
        path.push(format!("_{port}"));
        PathBuf::from(path)
    }

    fn vhost_user_socket(&self, child: &str) -> PathBuf {
        self.vhost_user_directory.join(child)
    }

    fn vhost_user_backing(&self, child: &str) -> PathBuf {
        self.vhost_user_directory.join(child)
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = self.devices.sensitive_strings();
        sensitive.extend([
            path_text(&self.api_directory).to_owned(),
            path_text(&self.vsock_directory).to_owned(),
            path_text(&self.vhost_user_directory).to_owned(),
            API_SOCKET_DIRECTORY_ID.to_owned(),
            VSOCK_SOCKET_DIRECTORY_ID.to_owned(),
            VHOST_USER_SOCKET_DIRECTORY_ID.to_owned(),
            API_SOCKET_REF.to_owned(),
            VSOCK_SOCKET_REF.to_owned(),
            VHOST_USER_SOCKET_REF_ONE.to_owned(),
            VHOST_USER_SOCKET_REF_TWO.to_owned(),
            VHOST_USER_SOCKET_REF_THREE.to_owned(),
            API_SOCKET_CHILD.to_owned(),
            VSOCK_SOCKET_CHILD.to_owned(),
            VHOST_USER_SOCKET_CHILD_ONE.to_owned(),
            VHOST_USER_SOCKET_CHILD_TWO.to_owned(),
            VHOST_USER_SOCKET_CHILD_THREE.to_owned(),
        ]);
        sensitive
    }
}

#[derive(Debug)]
struct OutputGrantFixture {
    _root: TestDir,
    logger: PathBuf,
    metrics: PathBuf,
    serial: PathBuf,
    opened_logger: PathBuf,
    opened_metrics: PathBuf,
    opened_serial: PathBuf,
    manifest: PathBuf,
}

impl OutputGrantFixture {
    fn new(case: &str) -> Self {
        let root = TestDir::new(&format!("output-grant-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("output grant root should canonicalize");
        let logger = canonical_root.join("external-logger.out");
        let metrics = canonical_root.join("external-metrics.out");
        let serial = canonical_root.join("external-serial.out");
        let opened_logger = canonical_root.join("opened-logger.out");
        let opened_metrics = canonical_root.join("opened-metrics.out");
        let opened_serial = canonical_root.join("opened-serial.out");
        let manifest = canonical_root.join("grant-manifest.json");

        fs::write(&logger, OUTPUT_LOGGER_SEED).expect("logger fixture should write");
        fs::write(&metrics, OUTPUT_METRICS_SEED).expect("metrics fixture should write");
        fs::write(&serial, OUTPUT_SERIAL_SEED).expect("serial fixture should write");

        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": OUTPUT_LOGGER_ID,
                    "role": "logger-sink",
                    "access": "write-only",
                    "source": path_text(&logger),
                },
                {
                    "id": OUTPUT_METRICS_ID,
                    "role": "metrics-sink",
                    "access": "write-only",
                    "source": path_text(&metrics),
                },
                {
                    "id": OUTPUT_SERIAL_ID,
                    "role": "serial-sink",
                    "access": "write-only",
                    "source": path_text(&serial),
                },
            ],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("output grant manifest should serialize"),
        )
        .expect("output grant manifest should write");

        Self {
            _root: root,
            logger,
            metrics,
            serial,
            opened_logger,
            opened_metrics,
            opened_serial,
            manifest,
        }
    }

    fn replace_source_pathnames(&self) {
        for (source, opened) in [
            (&self.logger, &self.opened_logger),
            (&self.metrics, &self.opened_metrics),
            (&self.serial, &self.opened_serial),
        ] {
            fs::rename(source, opened).expect("launcher-opened output should move");
            fs::write(source, OUTPUT_REPLACEMENT).expect("replacement output should write");
        }
    }

    fn assert_original_outputs(&self) {
        self.assert_original_outputs_with_logger_expectations(true, true);
    }

    fn assert_original_outputs_with_logger_expectations(&self, api: bool, action: bool) {
        Self::assert_outputs_at(
            &self.opened_logger,
            &self.opened_metrics,
            &self.opened_serial,
            api,
            action,
        );
    }

    fn assert_current_outputs(&self) {
        Self::assert_outputs_at(&self.logger, &self.metrics, &self.serial, false, true);
    }

    fn assert_outputs_at(
        logger_path: &Path,
        metrics_path: &Path,
        serial_path: &Path,
        api: bool,
        action: bool,
    ) {
        let logger = fs::read(logger_path).expect("granted logger output should read");
        assert!(logger.starts_with(OUTPUT_LOGGER_SEED));
        if api {
            assert!(
                logger
                    .windows(b"The API server received".len())
                    .any(|window| window == b"The API server received")
            );
        }
        if action {
            assert!(
                logger
                    .windows(b"action=InstanceStart\n".len())
                    .any(|window| window == b"action=InstanceStart\n")
            );
        }

        let metrics = fs::read_to_string(metrics_path).expect("granted metrics output should read");
        let seed = std::str::from_utf8(OUTPUT_METRICS_SEED).expect("metrics seed should be UTF-8");
        let payload = metrics
            .strip_prefix(seed)
            .expect("metrics writes should append after existing bytes");
        let lines = payload.lines().collect::<Vec<_>>();
        assert!(
            lines.len() >= 2,
            "initial and terminal metrics writes should both be present"
        );
        assert!(
            lines
                .iter()
                .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
            "each appended metrics line should be valid JSON"
        );
        assert!(lines.iter().any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/vmm/metrics_flush_count")
                        .and_then(serde_json::Value::as_u64)
                })
                == Some(1)
        }));

        let serial = fs::read(serial_path).expect("granted serial output should read");
        assert!(serial.starts_with(OUTPUT_SERIAL_SEED));
        assert!(
            serial
                .windows(GUEST_SERIAL_MARKER.len())
                .any(|window| window == GUEST_SERIAL_MARKER)
        );
    }

    fn assert_replacement_outputs_unchanged(&self) {
        for path in [&self.logger, &self.metrics, &self.serial] {
            assert_eq!(
                fs::read(path).expect("replacement output should read"),
                OUTPUT_REPLACEMENT
            );
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.logger),
            path_text(&self.metrics),
            path_text(&self.serial),
            path_text(&self.opened_logger),
            path_text(&self.opened_metrics),
            path_text(&self.opened_serial),
            path_text(&self.manifest),
            OUTPUT_LOGGER_ID,
            OUTPUT_METRICS_ID,
            OUTPUT_SERIAL_ID,
            OUTPUT_LOGGER_REF,
            OUTPUT_METRICS_REF,
            OUTPUT_SERIAL_REF,
            OUTPUT_MISSING_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#[derive(Debug, Clone, Copy)]
enum OutputStartupMode {
    ConfigFile,
    StartupCli,
}

#[derive(Debug)]
struct OutputStartupGrantFixture {
    outputs: OutputGrantFixture,
    config: PathBuf,
    manifest: PathBuf,
}

impl OutputStartupGrantFixture {
    fn new(bundle: &Path, case: &str, mode: OutputStartupMode) -> Self {
        let outputs = OutputGrantFixture::new(case);
        let root = outputs
            .logger
            .parent()
            .expect("output fixture should have a root");
        let config = root.join("external-config.json");
        let manifest = root.join("startup-grant-manifest.json");
        let resources = worker_bundle(bundle).join("Contents/Resources");
        let mut config_json = serde_json::json!({
            "machine-config": {"vcpu_count": 1, "mem_size_mib": 256},
            "boot-source": {
                "kernel_image_path": path_text(&resources.join("guest-kernel")),
                "initrd_path": path_text(&resources.join("guest-initrd")),
                "boot_args": "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init",
            },
            "serial": {"serial_out_path": OUTPUT_SERIAL_REF},
        });
        if matches!(mode, OutputStartupMode::ConfigFile) {
            let object = config_json
                .as_object_mut()
                .expect("startup config should be an object");
            object.insert(
                "metrics".to_owned(),
                serde_json::json!({"metrics_path": OUTPUT_METRICS_REF}),
            );
            object.insert(
                "logger".to_owned(),
                serde_json::json!({"log_path": OUTPUT_LOGGER_REF}),
            );
        }
        fs::write(
            &config,
            serde_json::to_vec(&config_json).expect("output startup config should serialize"),
        )
        .expect("output startup config should write");

        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": OUTPUT_CONFIG_ID,
                    "role": "startup-config",
                    "access": "read-only",
                    "source": path_text(&config),
                },
                {
                    "id": OUTPUT_LOGGER_ID,
                    "role": "logger-sink",
                    "access": "write-only",
                    "source": path_text(&outputs.logger),
                },
                {
                    "id": OUTPUT_METRICS_ID,
                    "role": "metrics-sink",
                    "access": "write-only",
                    "source": path_text(&outputs.metrics),
                },
                {
                    "id": OUTPUT_SERIAL_ID,
                    "role": "serial-sink",
                    "access": "write-only",
                    "source": path_text(&outputs.serial),
                },
            ],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json)
                .expect("output startup grant manifest should serialize"),
        )
        .expect("output startup grant manifest should write");

        Self {
            outputs,
            config,
            manifest,
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = self.outputs.sensitive_strings();
        sensitive.extend([
            path_text(&self.config).to_owned(),
            path_text(&self.manifest).to_owned(),
            OUTPUT_CONFIG_ID.to_owned(),
            OUTPUT_CONFIG_REF.to_owned(),
        ]);
        sensitive
    }

    fn assert_output_redacted(&self, output: &Output) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for sensitive in self.sensitive_strings() {
            assert!(
                !stdout.contains(&sensitive),
                "stdout leaked output grant data"
            );
            assert!(
                !stderr.contains(&sensitive),
                "stderr leaked output grant data"
            );
        }
    }
}

fn assert_private_grant_fault(response: &str, fixture: &StartupGrantFixture) {
    assert_redacted_private_grant_fault(response, fixture.sensitive_strings());
    assert!(!response.contains("bangbang-grant:missing"));
}

fn assert_device_private_grant_fault(response: &str, fixture: &GuestDeviceGrantFixture) {
    assert_redacted_private_grant_fault(response, fixture.sensitive_strings());
}

fn assert_output_private_grant_fault(response: &str, fixture: &OutputGrantFixture) {
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
            let pid = i32::try_from(self.child.id()).expect("launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID. Give it a bounded
            // chance to cancel and reap its worker so namespace cleanup runs.
            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
            let deadline = Instant::now() + DROP_CLEANUP_TIMEOUT;
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) | Err(_) => {
                        kill_child_group(&mut self.child);
                        let _ = self.child.wait();
                        break;
                    }
                }
            }
            if let Some(reader) = self.stdout_reader.take() {
                let _ = reader.join();
            }
            if let Some(reader) = self.stderr_reader.take() {
                let _ = reader.join();
            }
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
    spawn_ready_device_grant_api_launcher_with_extra_args(bundle, fixture, name, &[])
}

fn spawn_ready_device_grant_api_launcher_with_extra_args(
    bundle: &Path,
    fixture: &GuestDeviceGrantFixture,
    name: &str,
    worker_args: &[&str],
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbd-{:x}-{test_id:x}.sock", std::process::id(),));
    let mut child = Command::new(launcher(bundle))
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .args(worker_args)
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

fn spawn_ready_socket_grant_api_launcher(
    bundle: &Path,
    fixture: &SocketDirectoryGrantFixture,
    name: &str,
) -> RunningApiLauncher {
    spawn_ready_socket_grant_api_launcher_with_extra_args(bundle, fixture, name, &[])
}

fn spawn_ready_socket_grant_api_launcher_with_extra_args(
    bundle: &Path,
    fixture: &SocketDirectoryGrantFixture,
    name: &str,
    worker_args: &[&str],
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket = fixture.api_socket();
    let mut child = Command::new(launcher(bundle))
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.devices.manifest)
        .arg("--")
        .args(worker_args)
        .args(["--api-sock", API_SOCKET_REF])
        .args(["--id", &format!("{name}-{test_id}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("socket-directory grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "socket-directory grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
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

fn spawn_ready_output_grant_api_launcher(
    bundle: &Path,
    fixture: &OutputGrantFixture,
    name: &str,
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbo-{:x}-{test_id:x}.sock", std::process::id(),));
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
        .expect("output-grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "output-grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
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

fn spawn_ready_snapshot_grant_api_launcher(
    bundle: &Path,
    manifest: &Path,
    sensitive: Vec<String>,
    name: &str,
    hold_after_staging_record: bool,
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbsn-{:x}-{test_id:x}.sock", std::process::id()));
    let mut command = Command::new(launcher(bundle));
    command.arg(GRANT_MANIFEST_OPTION).arg(manifest).arg("--");
    if hold_after_staging_record {
        command.arg(SNAPSHOT_STAGING_HOLD_OPTION);
    }
    let mut child = command
        .args(["--api-sock", path_text(&socket)])
        .args(["--id", &format!("{name}-{test_id}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("snapshot-grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "snapshot-grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningApiLauncher {
        child,
        socket,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive,
        completed: false,
    }
}

fn configure_and_pause_snapshot_source(running: &RunningApiLauncher, metrics_path: &Path) {
    for (path, body, context) in [
        (
            "/machine-config",
            serde_json::json!({"vcpu_count": 1, "mem_size_mib": 16}),
            "PUT snapshot machine config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "PUT snapshot metrics",
        ),
        (
            "/boot-source",
            serde_json::json!({"kernel_image_path": SNAPSHOT_KERNEL_REF}),
            "PUT snapshot boot source",
        ),
        (
            "/drives/root",
            serde_json::json!({
                "drive_id": "root",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": true,
            }),
            "PUT snapshot root drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("snapshot request should serialize"),
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
        "start snapshot source",
    );
    wait_for_snapshot_uart_write(&running.socket, metrics_path, PROCESS_TIMEOUT);
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause snapshot source",
    );
}

fn wait_for_snapshot_uart_write(socket: &Path, metrics: &Path, timeout: Duration) {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("snapshot metric deadline should fit");
    loop {
        assert_http_status(
            &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
            204,
            "flush snapshot metrics",
        );
        if latest_snapshot_uart_write_count(metrics).is_some_and(|count| count >= 1) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "snapshot guest did not write readiness byte before timeout"
        );
        thread::yield_now();
    }
}

fn latest_snapshot_uart_write_count(path: &Path) -> Option<u64> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("uart")?
                .get("write_count")?
                .as_u64()
        })
}

fn snapshot_create_body() -> String {
    snapshot_create_body_for(SNAPSHOT_STATE_OUTPUT_REF, SNAPSHOT_MEMORY_OUTPUT_REF)
}

fn repeated_snapshot_create_body() -> String {
    snapshot_create_body_for(
        SNAPSHOT_REPEAT_STATE_OUTPUT_REF,
        SNAPSHOT_REPEAT_MEMORY_OUTPUT_REF,
    )
}

fn snapshot_create_body_for(state: &str, memory: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "snapshot_type": "Full",
        "snapshot_path": state,
        "mem_file_path": memory,
    }))
    .expect("snapshot create body should serialize")
}

fn snapshot_load_body(resume_vm: bool) -> String {
    serde_json::to_string(&serde_json::json!({
        "snapshot_path": SNAPSHOT_STATE_INPUT_REF,
        "mem_backend": {
            "backend_path": SNAPSHOT_MEMORY_INPUT_REF,
            "backend_type": "File",
        },
        "resume_vm": resume_vm,
    }))
    .expect("snapshot load body should serialize")
}

fn assert_no_snapshot_staging(directory: &Path) {
    let staging = fs::read_dir(directory)
        .expect("snapshot directory should remain readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("snapshot entries should read")
        .into_iter()
        .map(|entry| entry.file_name())
        .filter(|name| {
            let name = name.to_string_lossy();
            name.starts_with(".bangbang-snapshot-state-")
                || name.starts_with(".bangbang-snapshot-memory-")
        })
        .collect::<Vec<_>>();
    assert!(staging.is_empty(), "snapshot staging remains: {staging:?}");
}

fn run_snapshot_describe(bundle: &Path, fixture: &SnapshotDescribeGrantFixture) -> Output {
    let mut command = Command::new(launcher(bundle));
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .args(["--describe-snapshot", SNAPSHOT_DESCRIBE_INPUT_REF]);
    run_with_timeout(
        &mut command,
        PROCESS_TIMEOUT,
        "granted snapshot description",
    )
}

fn assert_snapshot_output_redacted(output: &Output, sensitive: &[String]) {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for value in sensitive {
        assert!(
            !combined.contains(value),
            "snapshot process output leaked private grant data"
        );
    }
}

fn configure_output_grant_session(
    bundle: &Path,
    running: &RunningApiLauncher,
    logger_module: &str,
) {
    for (path, body, context) in [
        (
            "/logger",
            serde_json::json!({
                "log_path": OUTPUT_LOGGER_REF,
                "module": logger_module,
            }),
            "PUT concurrent granted logger",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": OUTPUT_METRICS_REF}),
            "PUT concurrent granted metrics",
        ),
        (
            "/serial",
            serde_json::json!({"serial_out_path": OUTPUT_SERIAL_REF}),
            "PUT concurrent granted serial",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("output grant request should serialize"),
            ),
            204,
            context,
        );
    }
    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT concurrent output-grant machine config",
    );
    let resources = worker_bundle(bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "initrd_path": path_text(&resources.join("guest-initrd")),
        "boot_args": "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("boot source should serialize"),
        ),
        204,
        "PUT concurrent output-grant boot source",
    );
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

fn wait_for_new_session(baseline: &[PathBuf], timeout: Duration) -> bool {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("session deadline should fit");
    loop {
        if session_entries()
            .iter()
            .any(|entry| !baseline.contains(entry))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn only_worker_pid(launcher: &Child) -> libc::pid_t {
    let parent = libc::pid_t::try_from(launcher.id()).expect("launcher PID should fit");
    let children = child_pids(parent);
    assert_eq!(children.len(), 1, "launcher should own exactly one worker");
    children[0]
}

fn wait_for_only_child_pid(parent: libc::pid_t, timeout: Duration, context: &str) -> libc::pid_t {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("child PID deadline should fit");
    loop {
        let children = child_pids(parent);
        if let [pid] = children.as_slice() {
            return *pid;
        }
        assert!(
            children.is_empty(),
            "{context} should have at most one child: {children:?}"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context} child PID"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn child_pids(parent: libc::pid_t) -> Vec<libc::pid_t> {
    let mut pids = [0 as libc::pid_t; 16];
    let buffer_bytes =
        i32::try_from(std::mem::size_of_val(&pids)).expect("child PID buffer should fit");
    // SAFETY: `pids` is writable for `buffer_bytes`, and the launcher remains
    // live and unreaped while libproc takes this synchronous snapshot.
    let returned =
        unsafe { libc::proc_listchildpids(parent, pids.as_mut_ptr().cast(), buffer_bytes) };
    if returned <= 0 {
        return Vec::new();
    }
    let count = usize::try_from(returned).expect("libproc child count should fit");
    pids.get(..count)
        .expect("libproc count should fit buffer")
        .iter()
        .copied()
        .filter(|pid| *pid > 0)
        .collect::<Vec<_>>()
}

#[derive(Debug)]
struct ProcessExitWatch {
    queue: OwnedFd,
    pid: usize,
}

#[derive(Debug)]
struct DirectoryChangeWatch {
    queue: OwnedFd,
    _directory: fs::File,
    path: PathBuf,
}

impl DirectoryChangeWatch {
    fn new(path: &Path) -> Self {
        let directory = fs::File::open(path).expect("watched snapshot directory should open");
        // SAFETY: `kqueue` returns a fresh descriptor on success.
        let queue = unsafe { libc::kqueue() };
        assert!(queue >= 0, "snapshot directory watch kqueue should open");
        // SAFETY: `queue` is a fresh owned descriptor.
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        let ident = usize::try_from(directory.as_raw_fd())
            .expect("snapshot directory descriptor should fit usize");
        let change = libc::kevent {
            ident,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_RENAME,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        assert_eq!(
            // SAFETY: The queue, directory, and initialized registration remain live.
            unsafe {
                libc::kevent(
                    queue.as_raw_fd(),
                    &raw const change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            },
            0,
            "snapshot directory watch should register"
        );
        Self {
            queue,
            _directory: directory,
            path: path.to_path_buf(),
        }
    }

    fn wait_for_snapshot_staging(&self, timeout: Duration) -> Result<PathBuf, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "snapshot staging deadline overflowed".to_owned())?;
        loop {
            if let Some(staging) = find_snapshot_staging(&self.path)? {
                return Ok(staging);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out waiting for snapshot staging entry".to_owned());
            }
            let timeout = libc::timespec {
                tv_sec: libc::time_t::try_from(remaining.as_secs())
                    .map_err(|_| "snapshot staging timeout did not fit time_t".to_owned())?,
                tv_nsec: libc::c_long::from(remaining.subsec_nanos()),
            };
            let mut event = MaybeUninit::<libc::kevent>::uninit();
            // SAFETY: The live queue has one writable output event and a live timeout.
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
                continue;
            }
            if count == 0 {
                return Err("timed out waiting for snapshot staging event".to_owned());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("snapshot staging watch failed: {error}"));
            }
        }
    }

    fn wait_for_child_with_len(
        &self,
        child: &str,
        expected_len: u64,
        timeout: Duration,
    ) -> Result<PathBuf, String> {
        let child = self.path.join(child);
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "directory child deadline overflowed".to_owned())?;
        loop {
            if child
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected_len)
            {
                return Ok(child);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out waiting for directory child".to_owned());
            }
            let poll = remaining.min(Duration::from_millis(10));
            let timeout = libc::timespec {
                tv_sec: libc::time_t::try_from(poll.as_secs())
                    .map_err(|_| "directory child timeout did not fit time_t".to_owned())?,
                tv_nsec: libc::c_long::from(poll.subsec_nanos()),
            };
            let mut event = MaybeUninit::<libc::kevent>::uninit();
            // SAFETY: The live queue has one writable output event and a live timeout.
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
                continue;
            }
            if count == 0 {
                continue;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("directory child watch failed: {error}"));
            }
        }
    }
}

fn find_snapshot_staging(directory: &Path) -> Result<Option<PathBuf>, String> {
    let mut staging = fs::read_dir(directory)
        .map_err(|error| format!("snapshot staging directory could not be read: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot staging entry could not be read: {error}"))?
        .into_iter()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".bangbang-snapshot-memory-")
                || name.starts_with(".bangbang-snapshot-state-")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    staging.sort();
    if staging.len() > 1 {
        return Err("multiple snapshot staging entries appeared before the hold".to_owned());
    }
    Ok(staging.pop())
}

fn begin_snapshot_create_request(socket: &Path) -> UnixStream {
    let body = snapshot_create_body();
    let mut stream = UnixStream::connect(socket).expect("snapshot API should accept request");
    write!(
        stream,
        "PUT /snapshot/create HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .expect("snapshot create request should write");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("snapshot create request write should close");
    stream
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
    if registered < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return true;
    }
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

fn wait_for_process_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    // SAFETY: `kqueue` returns a fresh descriptor on success.
    let descriptor = unsafe { libc::kqueue() };
    assert!(descriptor >= 0, "process-exit kqueue should be created");
    // SAFETY: Ownership of the fresh descriptor transfers exactly once.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let change = libc::kevent {
        ident: usize::try_from(pid).expect("daemon PID should fit"),
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: `change` is one initialized registration and no output buffer is used.
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
    if registered < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return true;
    }
    assert_eq!(registered, 0, "daemon exit event should register");
    let timeout = libc::timespec {
        tv_sec: libc::time_t::try_from(timeout.as_secs()).expect("timeout seconds should fit"),
        tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
    };
    let mut event = MaybeUninit::<libc::kevent>::uninit();
    // SAFETY: `event` has room for one result and `timeout` remains live.
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
    count == 1
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

fn resize_and_write_file_marker_at(path: &Path, len: u64, offset: u64, marker: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("test backing should reopen for marker write");
    file.set_len(len)
        .expect("test backing length should resize");
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))
        .expect("test backing marker offset should seek");
    file.write_all(marker)
        .expect("test backing marker should write");
    file.sync_all().expect("test backing marker should fsync");
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

fn expected_block_device_id(path: &Path) -> String {
    let metadata = fs::metadata(path).expect("block backing metadata should be readable");
    format!("{}{}{}", metadata.dev(), metadata.rdev(), metadata.ino())
        .chars()
        .take(20)
        .collect()
}

fn assert_block_serial_report(path: &Path, expected: &str, context: &str) {
    let output = fs::read(path).expect("block serial output should be readable");
    let normalized = String::from_utf8_lossy(&output).replace('\r', "");
    let expected_report = format!(
        "{}\n{expected}\n{}",
        String::from_utf8_lossy(BLOCK_SERIAL_BEGIN_MARKER),
        String::from_utf8_lossy(BLOCK_SERIAL_END_MARKER),
    );
    assert!(
        normalized.contains(&expected_report),
        "{context} guest block serial must equal the exact launcher-opened backing metadata identity"
    );
}

fn wait_for_unix_listener_accept(
    listener: &UnixListener,
    timeout: Duration,
) -> std::io::Result<UnixStream> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("listener deadline should fit Instant");
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_socket_event(listener.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn read_exact_nonblocking(
    stream: &mut UnixStream,
    bytes: &mut [u8],
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("read deadline should fit Instant");
    let mut offset = 0;
    while offset < bytes.len() {
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(length) => offset += length,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_socket_event(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_nonblocking(
    stream: &mut UnixStream,
    bytes: &[u8],
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("write deadline should fit Instant");
    let mut offset = 0;
    while offset < bytes.len() {
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(length) => offset += length,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_socket_event(stream.as_raw_fd(), libc::POLLOUT, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_line_nonblocking(
    stream: &mut UnixStream,
    maximum: usize,
    timeout: Duration,
) -> std::io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(maximum);
    while line.len() < maximum {
        let mut byte = [0_u8; 1];
        read_exact_nonblocking(stream, &mut byte, timeout)?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(line);
        }
    }
    Err(std::io::ErrorKind::InvalidData.into())
}

fn deterministic_vsock_chunk(offset: usize, length: usize, seed: u8) -> Vec<u8> {
    (offset..offset + length)
        .map(|position| {
            let value = (position * 131 + usize::from(seed)) ^ (position >> 8) ^ (position >> 16);
            u8::try_from(value & 0xff).expect("deterministic byte should fit")
        })
        .collect()
}

fn verify_deterministic_stream(
    stream: &mut UnixStream,
    seed: u8,
    timeout: Duration,
) -> std::io::Result<()> {
    let mut offset = 0;
    while offset < GRANTED_HOST_VSOCK_STREAM_BYTES {
        let length = GRANTED_HOST_VSOCK_CHUNK_BYTES.min(GRANTED_HOST_VSOCK_STREAM_BYTES - offset);
        let mut received = vec![0_u8; length];
        read_exact_nonblocking(stream, &mut received, timeout).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("{error} after {offset} deterministic bytes"),
            )
        })?;
        if received != deterministic_vsock_chunk(offset, length, seed) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        offset += length;
    }
    Ok(())
}

fn write_deterministic_stream(
    stream: &mut UnixStream,
    seed: u8,
    timeout: Duration,
) -> std::io::Result<()> {
    let mut offset = 0;
    while offset < GRANTED_HOST_VSOCK_STREAM_BYTES {
        let length = GRANTED_HOST_VSOCK_CHUNK_BYTES.min(GRANTED_HOST_VSOCK_STREAM_BYTES - offset);
        write_all_nonblocking(
            stream,
            &deterministic_vsock_chunk(offset, length, seed),
            timeout,
        )?;
        offset += length;
    }
    Ok(())
}

fn wait_for_nonblocking_eof(stream: &mut UnixStream, timeout: Duration) -> std::io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("EOF deadline should fit Instant");
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(std::io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_socket_event(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn wait_for_socket_event(
    descriptor: libc::c_int,
    events: libc::c_short,
    deadline: Instant,
) -> std::io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let rounded_millis = remaining.as_millis().saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ));
        let timeout = i32::try_from(rounded_millis).unwrap_or(i32::MAX);
        let mut poll_fd = libc::pollfd {
            fd: descriptor,
            events,
            revents: 0,
        };
        // SAFETY: The single initialized poll entry is writable for this
        // bounded synchronous event wait.
        let ready = unsafe { libc::poll(&raw mut poll_fd, 1, timeout) };
        if ready > 0 {
            if poll_fd.revents & libc::POLLNVAL != 0 {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }
            if poll_fd.revents & (events | libc::POLLERR | libc::POLLHUP) != 0 {
                return Ok(());
            }
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        if ready == 0 {
            if Instant::now() >= deadline {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn assert_socket_mode(path: &Path, expected_mode: u32, context: &str) {
    let metadata = fs::symlink_metadata(path).expect("published socket metadata should exist");
    assert!(
        metadata.file_type().is_socket(),
        "{context} should be a socket"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        expected_mode,
        "{context} should have exact owner-only permissions"
    );
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

fn snapshot_continuity_guest_image() -> Vec<u8> {
    let instructions = [
        aarch64_movz_x(1, low_u16(SNAPSHOT_GUEST_VMGENID_ADDRESS, 0), 0),
        aarch64_movk_x(1, low_u16(SNAPSHOT_GUEST_VMGENID_ADDRESS, 16), 16),
        aarch64_ldp_x(2, 3, 1),
        aarch64_movz_x(4, low_u16(SNAPSHOT_GUEST_UART_ADDRESS, 0), 0),
        aarch64_movk_x(4, low_u16(SNAPSHOT_GUEST_UART_ADDRESS, 16), 16),
        aarch64_movz_x(7, u16::from(b'R'), 0),
        aarch64_strb_w(7, 4),
        aarch64_ldp_x(5, 6, 1),
        aarch64_cmp_x(5, 2),
        0x5400_0061,
        aarch64_cmp_x(6, 3),
        0x54ff_ff80,
        aarch64_movz_x(7, u16::from(b'C'), 0),
        aarch64_strb_w(7, 4),
        aarch64_movz_x(0, 0x0008, 0),
        aarch64_movk_x(0, 0x8400, 16),
        0xd400_0002,
        0x1400_0000,
    ];
    let mut image = vec![0; SNAPSHOT_GUEST_IMAGE_HEADER_SIZE];
    write_snapshot_test_u32(&mut image, 0, 0x1400_0010);
    write_snapshot_test_u32(&mut image, 4, 0xd503_201f);
    write_snapshot_test_u64(&mut image, 8, 0);
    write_snapshot_test_u32(&mut image, 56, SNAPSHOT_GUEST_IMAGE_MAGIC);
    image.extend(instructions.into_iter().flat_map(u32::to_le_bytes));
    let image_size = u64::try_from(image.len()).expect("snapshot guest image length should fit");
    write_snapshot_test_u64(&mut image, 16, image_size);
    image
}

fn aarch64_movz_x(register: u32, immediate: u16, shift: u32) -> u32 {
    0xd280_0000 | ((shift / 16) << 21) | (u32::from(immediate) << 5) | register
}

fn aarch64_movk_x(register: u32, immediate: u16, shift: u32) -> u32 {
    0xf280_0000 | ((shift / 16) << 21) | (u32::from(immediate) << 5) | register
}

fn aarch64_ldp_x(first: u32, second: u32, base: u32) -> u32 {
    0xa940_0000 | (second << 10) | (base << 5) | first
}

fn aarch64_cmp_x(left: u32, right: u32) -> u32 {
    0xeb00_001f | (right << 16) | (left << 5)
}

fn aarch64_strb_w(source: u32, base: u32) -> u32 {
    0x3900_0000 | (base << 5) | source
}

fn low_u16(value: u64, shift: u32) -> u16 {
    u16::try_from((value >> shift) & u64::from(u16::MAX))
        .expect("masked snapshot immediate should fit")
}

fn write_snapshot_test_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_snapshot_test_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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
