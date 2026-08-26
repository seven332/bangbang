#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[path = "../../../tests/support/macos_virtual_block.rs"]
mod macos_virtual_block;
#[path = "../../../tests/support/snapshot_serial.rs"]
mod snapshot_serial;
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
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_hvf::{
    HvfNativeSnapshotDocument, decode_hvf_snapshot_v2_diff_state,
    decode_hvf_snapshot_v2_vsock_state,
};
use bangbang_launcher::{
    JailerIsolationArgument, LAUNCHER_BUNDLE_IDENTIFIER, LAUNCHER_EXECUTABLE_NAME,
    OUTER_BUNDLE_NAME, WORKER_BUNDLE_IDENTIFIER, WORKER_BUNDLE_NAME, WORKER_EXECUTABLE_NAME,
};
use bangbang_pager::{
    PagerError, PagerFrameKind, PagerTransport, PeerSession, ReferencePeer,
    ReferencePeerTermination,
};
use bangbang_runtime::balloon::VIRTIO_BALLOON_FREE_PAGE_HINT_DONE;
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmds::MmdsVersion;
use bangbang_runtime::snapshot_balloon_v2_9::SnapshotV2BalloonState;
use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransportKind;
use bangbang_runtime::snapshot_diff_v2_13::{
    SnapshotV2DiffBase, verify_snapshot_v2_diff_layer_output,
};
use bangbang_runtime::snapshot_format_v2::{
    NATIVE_V2_VSOCK_COMPONENT_KEY, SnapshotV2Component, decode_snapshot_v2_state,
    encode_snapshot_v2_state_with_compatibility_version,
};
use bangbang_runtime::snapshot_memory_hotplug_v2_10::SnapshotV2MemoryHotplugState;
use bangbang_runtime::snapshot_memory_v2::load_snapshot_v2_memory_path;
use bangbang_runtime::snapshot_network_v2_11::{
    SnapshotV2NetworkBackendClass, SnapshotV2NetworkLimiterState, SnapshotV2NetworkState,
};
use bangbang_runtime::vsock::VSOCK_HOST_LOCAL_PORT_BASE;
use bangbang_session::{
    BLOCK_CONTROL_BROKER_FD, Frame, FrameDecoder, GRANT_FD, Message, SESSION_ENV_KEY,
    SESSION_ENV_VALUE, SESSION_FD, SOCKET_BROKER_FD, SessionId, VHOST_USER_BROKER_FD, WorkerPolicy,
    encode_frame,
};
use macos_virtual_block::{MacosVirtualBlock, MacosVirtualBlockAccess, MacosVirtualBlockSize};
use vhost_user_block::{
    VhostUserBlockBackend, VhostUserBlockBackendOptions, VhostUserBlockBackendReport,
};

const BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_BUNDLE_PATH";
const GRANT_TEST_BUNDLE_ENV: &str = "BANGBANG_PRODUCTION_GRANT_TEST_BUNDLE_PATH";
const SNAPSHOT_EDITOR_ENV: &str = "BANGBANG_SNAPSHOT_EDITOR_PATH";
const GUEST_KERNEL_ENV: &str = "BANGBANG_GUEST_KERNEL_PATH";
const GUEST_INITRD_ENV: &str = "BANGBANG_GUEST_INITRD_PATH";
const GUEST_EXT4_ROOTFS_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
const GRANT_MANIFEST_OPTION: &str = "--bangbang-grant-manifest";
const JAILER_OPTION: &str = "--bangbang-jailer-v1";
const GRANT_PROBE_OPTION: &str = "--bangbang-internal-grant-probe-v1";
const GRANT_PROBE_READY: &str = "status: grant integration probe ready";
const GRANT_DELAY_OPTION: &str = "--bangbang-internal-grant-delay-v1";
const GRANT_DELAY_READY: &str = "status: grant integration delay ready";
const GRANT_PROBE_MARKER: &str = "grant-integration-probe.enabled";
const ELEVATED_PROBE_OPTION: &str = "--bangbang-internal-elevated-bootstrap-probe-v2";
const ELEVATED_WORKER_OPTION: &[u8] = b"--bangbang-internal-elevated-bootstrap-worker-v2";
const ELEVATED_READY_RECORD: &[u8] = b"BBEP-READY-V2";
const ELEVATED_BLOCKED_STATUS: &[u8] = b"status: elevated bootstrap blocked";
const ELEVATED_INHERITED_MODE: &[u8] = b"inherited-root";
const ELEVATED_HVF_STAGE: &[u8] = b"hvf-create";
const ELEVATED_CREDENTIAL_DROP_MODE: &[u8] = b"credential-drop";
const ELEVATED_CREDENTIAL_RETAIN_MODE: &[u8] = b"credential-retain-root";
const ELEVATED_CREDENTIAL_UNMAPPED_MODE: &[u8] = b"credential-unmapped";
const ELEVATED_CREDENTIAL_CONTROL_MODE: &[u8] = b"credential-control";
const ELEVATED_CREDENTIAL_STATUS: &[u8] = b"status: elevated credential";
const ELEVATED_CREDENTIAL_RECORD: &[u8] = b"BBC1";
const ELEVATED_CREDENTIAL_DATAGRAM: &[u8] = b"BBG1";
const ELEVATED_CREDENTIAL_STEP: &[u8] = b"restore-groups";
const ELEVATED_CREDENTIAL_LAUNCHER_ARTIFACT: &[u8] =
    b"bangbang-elevated-credential-launcher-v1-credential-drop-BBC1-BBG1-restore-groups";
const ELEVATED_CREDENTIAL_WORKER_ARTIFACT: &[u8] =
    b"bangbang-elevated-credential-worker-v1-credential-drop-BBC1-BBG1-restore-groups";
const ELEVATED_RUNTIME_DROP_MODE: &[u8] = b"runtime-drop";
const ELEVATED_RUNTIME_RETAIN_MODE: &[u8] = b"runtime-retain-root";
const ELEVATED_RUNTIME_UNMAPPED_MODE: &[u8] = b"runtime-unmapped";
const ELEVATED_RUNTIME_STATUS: &[u8] = b"status: elevated runtime";
const ELEVATED_CONTINUATION_RECORD: &[u8] = b"BBA1";
const ELEVATED_RUNTIME_AUTHORITY_RECORD: &[u8] = b"BBN1";
const ELEVATED_RUNTIME_GRANT_CASE: &[u8] = b"target-runtime";
const ELEVATED_RUNTIME_LAUNCHER_BOUNDARIES: &[u8] = b"bangbang-elevated-runtime-launcher-boundaries-v2-pre-ack-post-ack-session-create-session-open-authority-send-authority-receive-authority-validate-session-lock-session-enter-prepared-namespace-grant-transfer-proceed-terminal-continuation-ack-lifecycle-hello-runtime-session-create-runtime-session-open-runtime-authority-send-runtime-authority-receive-runtime-authority-validate-runtime-session-lock-runtime-session-enter-lifecycle-prepared-runtime-namespace-grant-accepted-lifecycle-proceed-lifecycle-terminal-runtime-cleanup-complete-continuation-boundary-identity-boundary-explicit-root-boundary-namespace-boundary-grant-boundary-lifecycle-boundary-namespace-retire-before-unlink-namespace-retire-after-unlink-namespace-retire-observe-runtime-namespace-retirement-retired-record-free-namespace-record-write";
const ELEVATED_RUNTIME_WORKER_BOUNDARIES: &[u8] = b"bangbang-elevated-runtime-worker-boundaries-v2-pre-ack-post-ack-session-create-session-open-authority-send-authority-receive-authority-validate-session-lock-session-enter-prepared-namespace-grant-transfer-proceed-terminal-continuation-ack-lifecycle-hello-runtime-session-create-runtime-session-open-runtime-authority-send-runtime-authority-receive-runtime-authority-validate-runtime-session-lock-runtime-session-enter-lifecycle-prepared-runtime-namespace-grant-accepted-lifecycle-proceed-lifecycle-terminal-runtime-cleanup-complete-continuation-boundary-identity-boundary-explicit-root-boundary-namespace-boundary-grant-boundary-lifecycle-boundary-namespace-retire-before-unlink-namespace-retire-after-unlink-namespace-retire-observe-runtime-namespace-retirement-retired-record-free-namespace-record-write";
const ELEVATED_API_LISTENER_LAUNCHER_BOUNDARY: &[u8] = b"bangbang-elevated-api-listener-launcher-v2-BBL1-request-bind-transfer-adoption-final-child-one-right-retained-exact-post-reap-cleanup";
const ELEVATED_API_LISTENER_WORKER_BOUNDARY: &[u8] =
    b"bangbang-elevated-api-listener-worker-v2-BBL1-request-ack-adoption-record-free-exact-owner-readiness";
const ELEVATED_GUEST_MARKERS: &[&[u8]] = &[
    b"guest-no-api-drop",
    b"guest-no-api-retain-root",
    b"guest-no-api-unmapped",
    b"guest-api-drop",
    b"guest-api-retain-root",
    b"guest-api-unmapped",
    b"BBW1",
    b"guest-grant-contract",
    b"guest-grant-accepted",
    b"guest-transport-contamination",
    b"guest-resource-witness",
    b"api-listener-request",
    b"api-listener-bind",
    b"api-listener-transfer",
    b"api-listener-adoption",
    b"api-listener-endpoint-death",
    b"api-socket-publication",
    b"api-logger-configuration",
    b"api-metrics-configuration",
    b"api-serial-configuration",
    b"api-machine-configuration",
    b"api-boot-configuration",
    b"api-drive-configuration",
    b"api-instance-start",
    b"no-api-startup",
    b"guest-hvf-witness",
    b"guest-hvf-create",
    b"guest-execution",
    b"guest-oracle",
    b"guest-poweroff",
    b"guest-timeout",
    b"guest-endpoint-death",
    b"guest-terminal-evidence",
    b"guest-cleanup",
    b"api-boundary",
    b"hvf-boundary",
    b"guest-boundary",
    b"evidence-guest-config",
    b"evidence-guest-kernel",
    b"evidence-guest-initrd",
    b"evidence-guest-rootfs",
    b"evidence-guest-logger",
    b"evidence-guest-metrics",
    b"evidence-guest-serial",
    b"evidence-guest-api",
    b"BANGBANG_ROOTFS_WORKFLOW_OK",
    b"--bangbang-internal-post-adoption-stop-v1",
    b"resources=consumed workload=no-api",
    b"resources=consumed workload=api",
];
const ELEVATED_PROBE_MARKER: &str = "elevated-bootstrap-probe.enabled";
const ELEVATED_RUNTIME_MARKER: &str = "target-runtime-grant-probe.enabled";
const GRANT_PROBE_OUTSIDE: &str = "bangbang-grant-probe-outside";
const RESTORE_ROOT_ID: &str = "restore-root-1601";
const RESTORE_VSOCK_ID: &str = "restore-vsock-1601";
const RESTORE_ROOT_REF: &str = "bangbang-grant:restore-root-1601";
const RESTORE_VSOCK_REF: &str = "bangbang-grant:restore-vsock-1601/restore-1601.sock";
const RESTORE_SOCKET_CHILD: &str = "restore-1601.sock";
const RESTORE_ROOT_MARKER: &[u8] = b"BANGBANG_RESTORE_TRANSACTION_ROOT_1601\n";
const RESTORE_REPLACEMENT_MARKER: &[u8] = b"BANGBANG_RESTORE_REPLACEMENT_1601\n";
const RESTORE_ACTIVE_READY: &str = "status: restore transaction active";
const RESTORE_PREPARED_READY: &str = "status: restore transaction prepared";
const RESTORE_REPLACE_READY: &str = "status: restore transaction awaiting replacement";
const PAGER_GRANT_ID: &str = "probe-pager";
const PAGER_GRANT_REF: &str = "bangbang-grant:probe-pager";
const PAGER_PROBE_READY: &str = "status: pager integration probe ready";
const PAGER_PEER_LISTENING: &str = "status: pager reference peer listening";
const PAGER_PEER_PATH_ENV: &str = "BANGBANG_PAGER_REFERENCE_PATH";
const PAGER_PEER_MODE_ENV: &str = "BANGBANG_PAGER_REFERENCE_MODE";
const BLOCK_CONTROL_GRANT_ID: &str = "probe-block-control";
const BLOCK_CONTROL_GRANT_REF: &str = "bangbang-grant:probe-block-control";
const BLOCK_CONTROL_INITIAL_MARKER: &[u8] = b"BANGBANG_BLOCK_CONTROL_INITIAL";
const BLOCK_CONTROL_WRITTEN_MARKER: &[u8] = b"BANGBANG_BLOCK_CONTROL_WRITTEN";
const BLOCK_CONTROL_WRITE_BLOCK: u64 = 8;
const VIRTIO_BLOCK_SECTOR_BYTES: u64 = 512;
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
const GUEST_STORAGE_BLOCK_ONE_ID: &str = "grant-guest-storage-block-one-1471";
const GUEST_STORAGE_BLOCK_TWO_ID: &str = "grant-guest-storage-block-two-1471";
const GUEST_STORAGE_PMEM_ID: &str = "grant-guest-storage-pmem-1471";
const GUEST_PMEM_ID: &str = "grant-guest-pmem-1362";
const GUEST_PMEM_REUSE_ID: &str = "grant-guest-pmem-reuse-1421";
const GUEST_PMEM_ROOT_ID: &str = "grant-guest-pmem-root-1444";
const GUEST_READ_ONLY_DATA_ID: &str = "grant-guest-read-only-data-1362";
const GUEST_ROOTFS_REF: &str = "bangbang-grant:grant-guest-rootfs-1362";
const GUEST_DATA_REF: &str = "bangbang-grant:grant-guest-data-1362";
const GUEST_REPLACEMENT_REF: &str = "bangbang-grant:grant-guest-replacement-1362";
const GUEST_HOTPLUG_REUSE_REF: &str = "bangbang-grant:grant-guest-hotplug-reuse-1420";
const GUEST_STORAGE_BLOCK_ONE_REF: &str = "bangbang-grant:grant-guest-storage-block-one-1471";
const GUEST_STORAGE_BLOCK_TWO_REF: &str = "bangbang-grant:grant-guest-storage-block-two-1471";
const GUEST_STORAGE_PMEM_REF: &str = "bangbang-grant:grant-guest-storage-pmem-1471";
const GUEST_PMEM_REF: &str = "bangbang-grant:grant-guest-pmem-1362";
const GUEST_PMEM_REUSE_REF: &str = "bangbang-grant:grant-guest-pmem-reuse-1421";
const GUEST_PMEM_ROOT_REF: &str = "bangbang-grant:grant-guest-pmem-root-1444";
const GUEST_READ_ONLY_DATA_REF: &str = "bangbang-grant:grant-guest-read-only-data-1362";
const GUEST_MISSING_REF: &str = "bangbang-grant:grant-guest-missing-1362";
const BLOCK_SPECIAL_ROOT_ID: &str = "grant-block-special-root-1466";
const BLOCK_SPECIAL_CONTROL_ID: &str = "grant-block-special-control-1466";
const BLOCK_SPECIAL_FIRST_ID: &str = "grant-block-special-first-1466";
const BLOCK_SPECIAL_SECOND_ID: &str = "grant-block-special-second-1466";
const BLOCK_SPECIAL_READ_ONLY_ID: &str = "grant-block-special-read-only-1466";
const BLOCK_SPECIAL_SERIAL_ID: &str = "grant-block-special-serial-1466";
const BLOCK_SPECIAL_ROOT_REF: &str = "bangbang-grant:grant-block-special-root-1466";
const BLOCK_SPECIAL_CONTROL_REF: &str = "bangbang-grant:grant-block-special-control-1466";
const BLOCK_SPECIAL_FIRST_REF: &str = "bangbang-grant:grant-block-special-first-1466";
const BLOCK_SPECIAL_SECOND_REF: &str = "bangbang-grant:grant-block-special-second-1466";
const BLOCK_SPECIAL_READ_ONLY_REF: &str = "bangbang-grant:grant-block-special-read-only-1466";
const BLOCK_SPECIAL_SERIAL_REF: &str = "bangbang-grant:grant-block-special-serial-1466";
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
const VHOST_USER_METRICS_DELAY: Duration = Duration::from_millis(10);
const SNAPSHOT_KERNEL_ID: &str = "grant-snapshot-kernel-1368";
const SNAPSHOT_INITRD_ID: &str = "grant-snapshot-initrd-1617";
const SNAPSHOT_METRICS_ID: &str = "grant-snapshot-metrics-1368";
const SNAPSHOT_ROOT_ID: &str = "grant-snapshot-root-1589";
const SNAPSHOT_DATA_ID: &str = "grant-snapshot-data-1616";
const SNAPSHOT_AUDIT_ID: &str = "grant-snapshot-audit-1616";
const SNAPSHOT_PMEM_RW_ID: &str = "grant-snapshot-pmem-rw-1634";
const SNAPSHOT_PMEM_RO_ID: &str = "grant-snapshot-pmem-ro-1634";
const SNAPSHOT_STATE_OUTPUT_ID: &str = "grant-snapshot-state-output-1368";
const SNAPSHOT_MEMORY_OUTPUT_ID: &str = "grant-snapshot-memory-output-1368";
const SNAPSHOT_STATE_INPUT_ID: &str = "grant-snapshot-state-input-1368";
const SNAPSHOT_MEMORY_INPUT_ID: &str = "grant-snapshot-memory-input-1368";
const SNAPSHOT_DESCRIBE_INPUT_ID: &str = "grant-snapshot-describe-input-1368";
const SNAPSHOT_KERNEL_REF: &str = "bangbang-grant:grant-snapshot-kernel-1368";
const SNAPSHOT_INITRD_REF: &str = "bangbang-grant:grant-snapshot-initrd-1617";
const SNAPSHOT_METRICS_REF: &str = "bangbang-grant:grant-snapshot-metrics-1368";
const SNAPSHOT_ROOT_REF: &str = "bangbang-grant:grant-snapshot-root-1589";
const SNAPSHOT_DATA_REF: &str = "bangbang-grant:grant-snapshot-data-1616";
const SNAPSHOT_AUDIT_REF: &str = "bangbang-grant:grant-snapshot-audit-1616";
const SNAPSHOT_PMEM_RW_REF: &str = "bangbang-grant:grant-snapshot-pmem-rw-1634";
const SNAPSHOT_PMEM_RO_REF: &str = "bangbang-grant:grant-snapshot-pmem-ro-1634";
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
const SNAPSHOT_SERIAL_SINK_ID: &str = "grant-snapshot-serial-sink-1652";
const SNAPSHOT_SERIAL_SINK_REF: &str = "bangbang-grant:grant-snapshot-serial-sink-1652";
const SNAPSHOT_STAGING_HOLD_OPTION: &str = "--bangbang-internal-snapshot-staging-hold-v1";
const SNAPSHOT_STAGING_RECORD_BYTES: u64 = 128;
const SNAPSHOT_STATE_CHILD: &str = "state-1368.snap";
const SNAPSHOT_MEMORY_CHILD: &str = "memory-1368.snap";
const SNAPSHOT_REPEAT_STATE_CHILD: &str = "state-repeat-1368.snap";
const SNAPSHOT_REPEAT_MEMORY_CHILD: &str = "memory-repeat-1368.snap";
const SNAPSHOT_EDITOR_OUTPUT_CHILD: &str = "state-editor-1776.snap";
const SNAPSHOT_EDITOR_DBGBVR0: &str = "0x6030000000138004";
const SNAPSHOT_ROOT_BOOT_ARGS: &str = "console=null reboot=k panic=0 quiet loglevel=1 root=/dev/vda ro rootwait init=/bangbang-direct-rootfs-init bangbang.native-v2-root-snapshot=1";
const SNAPSHOT_BLOCK_BOOT_ARGS: &str =
    "console=null reboot=k panic=1 quiet loglevel=1 rdinit=/snapshot-block-init";
const SNAPSHOT_ENTROPY_BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/snapshot-entropy-init";
const SNAPSHOT_ENTROPY_READY_MARKER: &str = "BANGBANG_SNAPSHOT_ENTROPY_READY";
const SNAPSHOT_ENTROPY_SUCCESS_MARKER: &str = "BANGBANG_SNAPSHOT_ENTROPY_OK";
const SNAPSHOT_ENTROPY_FAILURE_MARKER: &str = "BANGBANG_SNAPSHOT_ENTROPY_FAIL";
const SNAPSHOT_ENTROPY_READ_BYTES: u64 = 64;
const SNAPSHOT_ENTROPY_REFILL_MS: u64 = 3_000;
const SNAPSHOT_BALLOON_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.balloon-check=1";
const SNAPSHOT_BALLOON_MARKER: &[u8] = b"BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK";
const SNAPSHOT_MEMORY_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.memory-hotplug-snapshot=1";
const SNAPSHOT_MEMORY_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_READY";
const SNAPSHOT_MEMORY_HOTPLUG_CAPTURE_READY_MARKER: &[u8] =
    b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_CAPTURE_READY";
const SNAPSHOT_MEMORY_HOTPLUG_RESTORED_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_BYTES_OK";
const SNAPSHOT_MEMORY_HOTPLUG_OFFLINE_READY_MARKER: &[u8] =
    b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OFFLINE_READY";
const SNAPSHOT_MEMORY_HOTPLUG_SHRUNK_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_SHRUNK";
const SNAPSHOT_MEMORY_HOTPLUG_UNPLUG_ALL_MARKER: &[u8] =
    b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_UNPLUG_ALL";
const SNAPSHOT_MEMORY_HOTPLUG_REPROBED_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_REPROBED";
const SNAPSHOT_MEMORY_HOTPLUG_REGROWN_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_REGROWN";
const SNAPSHOT_MEMORY_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OK";
const SNAPSHOT_MEMORY_HOTPLUG_FAILURE_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL";
const SNAPSHOT_MEMORY_HOTPLUG_CONTINUE_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_CONTINUE";
const SNAPSHOT_MEMORY_HOTPLUG_OFFLINE_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OFFLINE";
const SNAPSHOT_MEMORY_HOTPLUG_REPROBE_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_REPROBE";
const SNAPSHOT_MEMORY_HOTPLUG_SECTORS: u64 = 3;
const SNAPSHOT_MEMORY_HOTPLUG_CONTINUE_OFFSET: u64 = 512;
const SNAPSHOT_MEMORY_HOTPLUG_REPROBE_OFFSET: u64 = 2 * 512;
const SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT: Duration = Duration::from_secs(120);
const SNAPSHOT_NETWORK_MMDS_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-snapshot=1 bangbang.mmds-mtu=1280";
const SNAPSHOT_NETWORK_MMDS_SOURCE_CONTENT: &str =
    r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_SNAPSHOT_SOURCE"}}"#;
const SNAPSHOT_NETWORK_MMDS_DESTINATION_CONTENT: &str =
    r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_SNAPSHOT_DESTINATION"}}"#;
const SNAPSHOT_NETWORK_MMDS_READY_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_CAPTURE_READY";
const SNAPSHOT_NETWORK_MMDS_CONTINUE_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_CONTINUE";
const SNAPSHOT_NETWORK_MMDS_CONNECTION_LOST_MARKER: &[u8] =
    b"BANGBANG_MMDS_SNAPSHOT_CONNECTION_LOST";
const SNAPSHOT_NETWORK_MMDS_TOKEN_REJECTED_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_TOKEN_REJECTED";
const SNAPSHOT_NETWORK_MMDS_V1_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_V1_NO_TOKEN";
const SNAPSHOT_NETWORK_MMDS_FRESH_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_FRESH_FETCH_OK";
const SNAPSHOT_NETWORK_MMDS_SUCCESS_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_OK";
const SNAPSHOT_NETWORK_MMDS_FAILURE_MARKER: &[u8] = b"BANGBANG_MMDS_SNAPSHOT_FAIL";
const SNAPSHOT_NETWORK_MMDS_SECTORS: u64 = 5;
const SNAPSHOT_NETWORK_MMDS_CONTINUE_OFFSET: u64 = VIRTIO_BLOCK_SECTOR_BYTES;
const SNAPSHOT_NETWORK_MMDS_CONNECTION_OFFSET: u64 = 2 * VIRTIO_BLOCK_SECTOR_BYTES;
const SNAPSHOT_NETWORK_MMDS_TOKEN_RESULT_OFFSET: u64 = 3 * VIRTIO_BLOCK_SECTOR_BYTES;
const SNAPSHOT_NETWORK_MMDS_FRESH_OFFSET: u64 = 4 * VIRTIO_BLOCK_SECTOR_BYTES;
const SNAPSHOT_NETWORK_MMDS_TIMEOUT: Duration = Duration::from_secs(120);
const SNAPSHOT_VSOCK_SOURCE_DIRECTORY_ID: &str = "grant-snapshot-vsock-source-1735";
const SNAPSHOT_VSOCK_OVERRIDE_DIRECTORY_ID: &str = "grant-snapshot-vsock-override-1735";
const SNAPSHOT_VSOCK_SOURCE_CHILD: &str = "snapshot-vsock-source.sock";
const SNAPSHOT_VSOCK_OVERRIDE_CHILD: &str = "snapshot-vsock-override.sock";
const SNAPSHOT_VSOCK_SOURCE_REF: &str =
    "bangbang-grant:grant-snapshot-vsock-source-1735/snapshot-vsock-source.sock";
const SNAPSHOT_VSOCK_OVERRIDE_REF: &str =
    "bangbang-grant:grant-snapshot-vsock-override-1735/snapshot-vsock-override.sock";
const SNAPSHOT_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.vsock-snapshot-reset=1";
const SNAPSHOT_VSOCK_OLD_PORT: u32 = 5011;
const SNAPSHOT_VSOCK_FRESH_PORT: u32 = 5012;
const SNAPSHOT_VSOCK_OLD_READY: &[u8] = b"BANGBANG_VSOCK_SNAPSHOT_OLD_READY";
const SNAPSHOT_VSOCK_FRESH_READY: &[u8] = b"BANGBANG_VSOCK_SNAPSHOT_FRESH_READY";
const SNAPSHOT_VSOCK_FRESH_ACK: &[u8] = b"BANGBANG_VSOCK_SNAPSHOT_FRESH_ACK";
const SNAPSHOT_VSOCK_SUCCESS: &[u8] = b"BANGBANG_VSOCK_SNAPSHOT_RESET_OK";
const SNAPSHOT_VSOCK_CERTIFY_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.vsock-snapshot-certify=1";
const SNAPSHOT_VSOCK_CERTIFY_SOURCE_GUEST_PORTS: [u32; 1] = [5011];
const SNAPSHOT_VSOCK_CERTIFY_FRESH_GUEST_PORTS: [u32; 4] = [5021, 5022, 5023, 5024];
const SNAPSHOT_VSOCK_CERTIFY_GUEST_LISTEN_PORT: u32 = 6011;
const SNAPSHOT_VSOCK_CERTIFY_HOST_STREAMS: usize = 16;
const SNAPSHOT_VSOCK_CERTIFY_SOURCE_READY: &str = "BANGBANG_VSOCK_SNAPSHOT_SOURCE_READY";
const SNAPSHOT_VSOCK_CERTIFY_RESET_OBSERVED: &str = "BANGBANG_VSOCK_SNAPSHOT_RESET_OBSERVED";
const SNAPSHOT_VSOCK_CERTIFY_FRESH_G2H_OK: &str = "BANGBANG_VSOCK_SNAPSHOT_FRESH_G2H_OK";
const SNAPSHOT_VSOCK_CERTIFY_PRESERVED_LISTENER_OK: &str =
    "BANGBANG_VSOCK_SNAPSHOT_PRESERVED_LISTENER_OK";
const SNAPSHOT_VSOCK_CERTIFY_FAILURE: &str = "BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_";
const SNAPSHOT_BLOCK_SECTOR_SIZE: usize = 512;
const SNAPSHOT_BLOCK_DRIVE_A_INITIAL_BYTE: u8 = 0x11;
const SNAPSHOT_BLOCK_DRIVE_A_PRE_CAPTURE_BYTE: u8 = 0x12;
const SNAPSHOT_BLOCK_DRIVE_A_DESTINATION_ONE_BYTE: u8 = 0x13;
const SNAPSHOT_BLOCK_DRIVE_A_DESTINATION_TWO_BYTE: u8 = 0x14;
const SNAPSHOT_BLOCK_DRIVE_B_INITIAL_BYTE: u8 = 0x21;
const SNAPSHOT_BLOCK_DRIVE_B_PRE_CAPTURE_BYTE: u8 = 0x22;
const SNAPSHOT_BLOCK_DRIVE_B_DESTINATION_ONE_BYTE: u8 = 0x23;
const SNAPSHOT_BLOCK_DRIVE_B_DESTINATION_TWO_BYTE: u8 = 0x24;
const SNAPSHOT_BLOCK_AUDIT_BYTE: u8 = 0x31;
const SNAPSHOT_BLOCK_PARTUUID: &str = "1617-CAFE";
const SNAPSHOT_PMEM_SECTOR_SIZE: usize = 512;
const SNAPSHOT_PMEM_FILE_BYTES: usize = (2 * 1024 * 1024) + (16 * 1024);
const SNAPSHOT_PMEM_WRITABLE_INITIAL_BYTE: u8 = 0x41;
const SNAPSHOT_PMEM_WRITABLE_PRE_CAPTURE_BYTE: u8 = 0x42;
const SNAPSHOT_PMEM_WRITABLE_DESTINATION_ONE_BYTE: u8 = 0x43;
const SNAPSHOT_PMEM_WRITABLE_DESTINATION_TWO_BYTE: u8 = 0x44;
const SNAPSHOT_PMEM_READ_ONLY_BYTE: u8 = 0x51;
const SNAPSHOT_PMEM_WRITABLE_REPLACEMENT_BYTE: u8 = 0xf1;
const SNAPSHOT_PMEM_READ_ONLY_REPLACEMENT_BYTE: u8 = 0xf2;
const SNAPSHOT_PMEM_LIMITER_REFILL_MS: u64 = 5000;
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
const GUEST_SERIAL_RX_BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/serial-rx-init";
const GUEST_SERIAL_RX_READY_MARKER: &str = "BANGBANG_SERIAL_RX_READY";
const GUEST_SERIAL_RX_SUCCESS_MARKER: &str = "BANGBANG_SERIAL_RX_OK";
const GUEST_SERIAL_RX_FAILURE_MARKER: &str = "BANGBANG_SERIAL_RX_FAIL";
const DIRECT_ROOTFS_PMEM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-read-flush=1";
const DIRECT_ROOTFS_PMEM_ROOT_RO_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.pmem-root=ro";
const DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER: &[u8] = b"BANGBANG_PMEM_ROOT_RO_OK";
const DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.memory-hotplug-check=1";
const DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-writeback-flush=1";
const BLOCK_SERIAL_BEGIN_MARKER: &[u8] = b"BANGBANG_BLOCK_SERIAL_BEGIN";
const BLOCK_SERIAL_END_MARKER: &[u8] = b"BANGBANG_BLOCK_SERIAL_END";
const DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-hotplug=1";
const DIRECT_ROOTFS_VHOST_BLOCK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.block-hotplug=1";
const DIRECT_ROOTFS_BLOCK_LIFECYCLE_TWO_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-backing-lifecycle=two bangbang.expect-block-limiter-patch=1";
const DIRECT_ROOTFS_PMEM_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-hotplug=1";
const DIRECT_ROOTFS_NETWORK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.network-hotplug=1";
const DIRECT_ROOTFS_STORAGE_CERTIFICATION_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 root=/dev/vda ro rootwait memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.storage-certification=1";
const STORAGE_CONTROL_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTROL_HOST";
const STORAGE_CONTROL_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTROL_GUEST";
const STORAGE_ASYNC_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_HOST";
const STORAGE_ASYNC_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_GUEST";
const STORAGE_ASYNC_REPLACEMENT_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_REPLACEMENT_HOST";
const STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_REPLACEMENT_GUEST";
const STORAGE_VHOST_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_VHOST_HOST";
const STORAGE_VHOST_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_VHOST_GUEST";
const STORAGE_PMEM_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_PMEM_HOST";
const STORAGE_PMEM_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_PMEM_GUEST";
const STORAGE_RUNTIME_BLOCK_ONE_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_BLOCK_ONE_HOST";
const STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_BLOCK_ONE_GUEST";
const STORAGE_RUNTIME_BLOCK_TWO_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_BLOCK_TWO_HOST";
const STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_BLOCK_TWO_GUEST";
const STORAGE_RUNTIME_PMEM_ONE_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_PMEM_ONE_HOST";
const STORAGE_RUNTIME_PMEM_ONE_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_PMEM_ONE_GUEST";
const STORAGE_RUNTIME_PMEM_TWO_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_PMEM_TWO_HOST";
const STORAGE_RUNTIME_PMEM_TWO_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_PMEM_TWO_GUEST";
const STORAGE_READY_MARKER: &[u8] = b"BANGBANG_STORAGE_READY";
const STORAGE_CONTINUE_ONE_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTINUE_ONE";
const STORAGE_FIRST_REMOVED_MARKER: &[u8] = b"BANGBANG_STORAGE_FIRST_REMOVED";
const STORAGE_CONTINUE_TWO_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTINUE_TWO";
const STORAGE_SECOND_BLOCK_REMOVED_MARKER: &[u8] = b"BANGBANG_STORAGE_SECOND_BLOCK_REMOVED";
const STORAGE_CONTINUE_PMEM_ONE_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTINUE_PMEM_ONE";
const STORAGE_FIRST_PMEM_REMOVED_MARKER: &[u8] = b"BANGBANG_STORAGE_FIRST_PMEM_REMOVED";
const STORAGE_CONTINUE_PMEM_TWO_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTINUE_PMEM_TWO";
const STORAGE_SUCCESS_MARKER: &[u8] = b"BANGBANG_STORAGE_SUCCESS";
const STORAGE_CONTROL_GUEST_OFFSET: u64 = 2 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_READY_OFFSET: u64 = VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_CONTINUE_ONE_OFFSET: u64 = 3 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_FIRST_REMOVED_OFFSET: u64 = 4 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_CONTINUE_TWO_OFFSET: u64 = 5 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_SUCCESS_OFFSET: u64 = 6 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_SECOND_BLOCK_REMOVED_OFFSET: u64 = 7 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_CONTINUE_PMEM_ONE_OFFSET: u64 = 8 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_FIRST_PMEM_REMOVED_OFFSET: u64 = 9 * VIRTIO_BLOCK_SECTOR_BYTES;
const STORAGE_CONTINUE_PMEM_TWO_OFFSET: u64 = 10 * VIRTIO_BLOCK_SECTOR_BYTES;
const BLOCK_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_READY";
const BLOCK_HOTPLUG_HOST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_HOST_ONE";
const BLOCK_HOTPLUG_GUEST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_GUEST_ONE";
const BLOCK_HOTPLUG_FIRST_REMOVED_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_FIRST_REMOVED";
const BLOCK_HOTPLUG_CONTINUE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_CONTINUE";
const BLOCK_HOTPLUG_HOST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_HOST_TWO";
const BLOCK_HOTPLUG_GUEST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_GUEST_TWO";
const BLOCK_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_SUCCESS";
const BLOCK_HOTPLUG_FIRST_SERIAL_BEGIN_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_FIRST_SERIAL_BEGIN";
const BLOCK_HOTPLUG_FIRST_SERIAL_END_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_FIRST_SERIAL_END";
const BLOCK_HOTPLUG_SECOND_SERIAL_BEGIN_MARKER: &[u8] =
    b"BANGBANG_BLOCK_HOTPLUG_SECOND_SERIAL_BEGIN";
const BLOCK_HOTPLUG_SECOND_SERIAL_END_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_SECOND_SERIAL_END";
const BLOCK_LIFECYCLE_INITIAL_SERIAL_BEGIN_MARKER: &[u8] =
    b"BANGBANG_BLOCK_LIFECYCLE_INITIAL_SERIAL_BEGIN";
const BLOCK_LIFECYCLE_INITIAL_SERIAL_END_MARKER: &[u8] =
    b"BANGBANG_BLOCK_LIFECYCLE_INITIAL_SERIAL_END";
const BLOCK_LIFECYCLE_HOST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_HOST_ONE";
const BLOCK_LIFECYCLE_GUEST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_GUEST_ONE";
const BLOCK_LIFECYCLE_PHASE_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_PHASE_ONE";
const BLOCK_LIFECYCLE_LIMITER_READY_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_LIMITER_READY";
const BLOCK_LIFECYCLE_LIMITER_CONTINUE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_LIMITER_CONTINUE";
const BLOCK_LIFECYCLE_HOST_THREE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_HOST_THREE";
const BLOCK_LIFECYCLE_GUEST_THREE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_GUEST_THREE";
const BLOCK_LIFECYCLE_READ_ONLY_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_READ_ONLY";
const BLOCK_LIFECYCLE_SUCCESS_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_SUCCESS";
const BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET: u64 = 2 * VIRTIO_BLOCK_SECTOR_BYTES;
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
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const REAL_PERIODIC_METRICS_TIMEOUT: Duration = Duration::from_secs(90);
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

fn snapshot_editor() -> PathBuf {
    let path = std::env::var_os(SNAPSHOT_EDITOR_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the snapshot-editor path");
    let path = PathBuf::from(path);
    assert_eq!(path.file_name(), Some(OsStr::new("snapshot-editor")));
    assert!(path.is_file(), "snapshot-editor must be a regular file");
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

fn guest_kernel() -> PathBuf {
    let path = std::env::var_os(GUEST_KERNEL_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the guest kernel fixture path");
    let path = PathBuf::from(path);
    assert!(path.is_file(), "guest kernel fixture must be a file");
    path
}

fn guest_initrd() -> PathBuf {
    let path = std::env::var_os(GUEST_INITRD_ENV)
        .filter(|value| !value.is_empty())
        .expect("signed runner must provide the guest initrd fixture path");
    let path = PathBuf::from(path);
    assert!(path.is_file(), "guest initrd fixture must be a file");
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
fn pager_reference_peer_child() {
    let Some(path) = std::env::var_os(PAGER_PEER_PATH_ENV) else {
        return;
    };
    let mode = std::env::var(PAGER_PEER_MODE_ENV).expect("pager peer mode should be present");
    let listener =
        UnixListener::bind(PathBuf::from(path)).expect("pager reference listener should bind");
    println!("{PAGER_PEER_LISTENING}");
    std::io::stdout()
        .flush()
        .expect("pager reference readiness should flush");
    let (stream, _) = listener
        .accept()
        .expect("pager reference stream should accept");
    match mode.as_str() {
        "complete" => {
            let report = ReferencePeer::new(4)
                .expect("reference bound should validate")
                .serve(stream, Duration::from_secs(5))
                .expect("complete reference session should succeed");
            assert_eq!(report.page_data(), 1);
            assert_eq!(report.page_zero(), 2);
            assert_eq!(report.removals(), 1);
            assert_eq!(report.termination(), ReferencePeerTermination::Shutdown);
        }
        "cancel" => {
            let report = ReferencePeer::new(1)
                .expect("reference bound should validate")
                .serve(stream, Duration::from_secs(5))
                .expect("cancel reference session should succeed");
            assert_eq!(report.termination(), ReferencePeerTermination::Cancelled);
        }
        "terminal" => {
            let report = ReferencePeer::new(1)
                .expect("reference bound should validate")
                .serve(stream, Duration::from_secs(5))
                .expect("terminal reference session should succeed");
            assert_eq!(
                report.termination(),
                ReferencePeerTermination::Terminal(bangbang_pager::TerminalCode::Internal)
            );
        }
        "hold" => run_holding_pager_peer(stream),
        "corrupt" => {
            let mut stream = stream;
            stream
                .write_all(b"{\"uffd\":true}")
                .expect("corrupt peer bytes should write");
        }
        "eof" => {}
        "stall" => {
            thread::sleep(Duration::from_secs(2));
        }
        _ => panic!("unexpected pager reference mode"),
    }
}

fn run_holding_pager_peer(stream: UnixStream) {
    let mut transport =
        PagerTransport::new(stream, Duration::from_secs(5)).expect("transport should initialize");
    let mut peer = PeerSession::new();
    let hello = peer
        .receive(transport.receive().expect("hello should arrive"))
        .expect("hello should validate");
    assert_eq!(hello.kind(), PagerFrameKind::Hello);
    let selected = peer
        .offered_limits()
        .expect("hello should contain bounded limits");
    transport
        .send(&peer.hello_ack(selected).expect("hello ack should build"))
        .expect("hello ack should send");
    let region = peer
        .receive(transport.receive().expect("region should arrive"))
        .expect("region should validate");
    assert_eq!(region.kind(), PagerFrameKind::Region);
    let start = peer
        .receive(transport.receive().expect("start should arrive"))
        .expect("start should validate");
    assert_eq!(start.kind(), PagerFrameKind::Start);
    transport
        .send(&peer.ready().expect("ready should build"))
        .expect("ready should send");
    let request = peer
        .receive(transport.receive().expect("page request should arrive"))
        .expect("page request should validate");
    assert_eq!(request.kind(), PagerFrameKind::PageRequest);
    assert!(matches!(
        transport.receive(),
        Err(PagerError::Disconnected | PagerError::UnexpectedEof)
            | Err(PagerError::Io(
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ))
    ));
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

    assert_exact_networkless_bundle_entitlements(&bundle);
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
fn networkless_bundle_rejects_every_positive_vmnet_mode_before_session_creation() {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    // SAFETY: Credential getters have no pointer or ownership contract.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };

    for (case, allowed_mode) in [
        ("host", "host"),
        ("shared", "shared"),
        ("bridged", "bridged:bridge_7"),
    ] {
        let baseline_sessions = session_entries();
        let mut command = Command::new(launcher(&bundle));
        command
            .arg(JAILER_OPTION)
            .args(["--id", &format!("networkless-{case}")])
            .arg("--exec-file")
            .arg(worker_executable(&bundle))
            .args([
                "--uid",
                &uid.to_string(),
                "--gid",
                &gid.to_string(),
                "--vmnet-allow",
                allowed_mode,
                "--vmnet-max-interfaces",
                "1",
                "--",
                "--version",
            ]);

        let output = run_with_timeout(
            &mut command,
            PROCESS_TIMEOUT,
            "networkless positive vmnet policy rejection",
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "bangbang launcher: invalid production launch policy\n"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("bridge_7"));
        assert_eq!(session_entries(), baseline_sessions);
    }
}

#[test]
fn signed_jailer_rejects_unsupported_isolation_before_grants_sessions_and_worker() {
    let bundle = production_bundle();
    initialize_worker_container(&bundle);
    let private = TestDir::new("unsupported-isolation-rejection");

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
            "signed unsupported isolation rejection",
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
        JailerIsolationArgument::ChrootBaseDirectory,
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
        JailerIsolationArgument::ChrootBaseDirectory,
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
fn normal_bundle_adopts_native_v2_snapshot_grants_for_create_describe_and_restore() {
    let bundle = production_bundle();
    for enable_pci in [false, true] {
        run_native_v2_snapshot_grant_case(&bundle, enable_pci);
    }
}

#[test]
fn normal_bundle_certifies_native_v2_diff_snapshot_grants_and_app_sandbox() {
    let bundle = production_bundle();
    for enable_pci in [false, true] {
        run_native_v2_diff_snapshot_grant_case(&bundle, enable_pci);
    }
}

#[test]
fn normal_bundle_certifies_native_v2_serial_snapshot_continuation_and_containment() {
    snapshot_serial::assert_guest_images();
    let bundle = production_bundle();
    let baseline_sessions = session_entries();

    run_production_default_serial_snapshot_continuation(&bundle);
    for enable_pci in [false, true] {
        run_production_configured_serial_snapshot_continuation(&bundle, enable_pci);
    }

    assert_eq!(
        session_entries(),
        baseline_sessions,
        "serial snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_production_default_serial_snapshot_continuation(bundle: &Path) {
    let source_fixture = SerialSnapshotSourceGrantFixture::new("stdio", false, false);
    let source_logger = DeviceLoggerGrant::add_to_manifest(
        &source_fixture.manifest,
        "serial-snapshot-stdio-source",
    );
    let source_sensitive = source_fixture
        .sensitive_strings()
        .into_iter()
        .chain(source_logger.sensitive_strings())
        .collect::<Vec<_>>();
    let mut source = spawn_ready_serial_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        "serial-snapshot-stdio-source",
        false,
    );
    source_fixture.replace_source_pathnames();
    source_logger.replace_source_pathname();
    source_logger.configure(&source.socket, "production serial snapshot source");
    configure_and_start_serial_snapshot_grant_source(&source.socket, false, false);
    source
        .wait_for_stdout_marker(snapshot_serial::SOURCE_READY_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| panic!("production serial source should become ready: {error}"));
    let source_input = snapshot_serial::source_input();
    source.write_stdin(&source_input);
    wait_for_file_contains(
        &source_logger.opened,
        b"device-kind=serial operation=input-read outcome=succeeded",
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production serial source input should reach the UART owner: {error}")
    });
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause production serial source",
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        "create production serial snapshot",
    );
    let artifacts = source_fixture.artifacts();
    assert!(artifacts.state.is_file());
    assert!(artifacts.memory.is_file());
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let state_before = fs::read(&artifacts.state).expect("production serial state should read");
    let memory_before = fs::read(&artifacts.memory).expect("production serial memory should read");

    let source_pid = i32::try_from(source.child.id()).expect("launcher PID should fit");
    // SAFETY: The unreaped source launcher owns this PID.
    assert_eq!(unsafe { libc::kill(source_pid, libc::SIGTERM) }, 0);
    let (source_status, source_stdout, source_stderr) =
        source.wait("production serial snapshot source");
    assert!(
        source_status.success(),
        "production serial source should stop cleanly: {source_status:?}\nstdout:\n{source_stdout}\nstderr:\n{source_stderr}"
    );
    assert!(source_stdout.contains(snapshot_serial::SOURCE_READY_MARKER));
    assert!(!source_stdout.contains(snapshot_serial::RESTORED_SUCCESS_MARKER));
    assert_serial_snapshot_output_redacted(
        &source_stdout,
        &source_stderr,
        &source_sensitive,
        "production serial source",
    );
    source_logger.assert_records(
        &[
            "device-kind=serial operation=input-read outcome=succeeded",
            "device-kind=time-identity operation=platform-publication outcome=succeeded",
        ],
        source_fixture.sensitive_strings().into_iter().chain([
            String::from_utf8_lossy(&source_input).into_owned(),
            snapshot_serial::SOURCE_READY_MARKER.to_owned(),
        ]),
    );
    assert_production_uart_extensions_absent(
        &source_fixture.opened_metrics,
        "production serial source",
    );
    assert!(!source.socket.exists());

    let destination_fixture = SerialSnapshotInputGrantFixture::new("stdio", artifacts, false);
    let destination_sensitive = destination_fixture.sensitive_strings();
    let mut destination = spawn_ready_serial_snapshot_grant_api_launcher(
        bundle,
        &destination_fixture.manifest,
        "serial-snapshot-stdio-destination",
        false,
    );
    let opened = destination_fixture.replace_source_pathnames();
    configure_serial_snapshot_grant_destination_metrics(&destination.socket);
    assert_http_status(
        &http_put(
            &destination.socket,
            "/snapshot/load",
            &snapshot_load_body(false),
        ),
        204,
        "load production serial snapshot paused",
    );
    assert!(
        http_get(&destination.socket, "/").contains(r#""state":"Paused""#),
        "production serial destination should remain paused until explicitly resumed"
    );
    destination.write_stdin(&snapshot_serial::destination_input());
    destination.close_stdin();
    assert_http_status(
        &http_request(
            &destination.socket,
            "PATCH",
            "/vm",
            r#"{"state":"Resumed"}"#,
        ),
        204,
        "resume production serial destination",
    );
    let (destination_status, destination_stdout, destination_stderr) =
        destination.wait("production restored serial guest");
    assert!(
        destination_status.success(),
        "production serial destination should exit cleanly: {destination_status:?}\nstdout:\n{destination_stdout}\nstderr:\n{destination_stderr}"
    );
    assert!(
        destination_stdout.contains(snapshot_serial::RESTORED_SUCCESS_MARKER),
        "production destination should validate restored UART state and fresh input; stdout:\n{destination_stdout}"
    );
    assert!(!destination_stdout.contains(snapshot_serial::RESTORED_FAILURE_MARKER));
    assert_serial_snapshot_output_redacted(
        &destination_stdout,
        &destination_stderr,
        &destination_sensitive,
        "production serial destination",
    );
    assert!(!destination.socket.exists());
    assert_production_uart_extensions_absent(
        &destination_fixture.opened_metrics,
        "production serial destination",
    );
    assert!(
        production_uart_metric_total(&destination_fixture.opened_metrics, "read_count")
            >= u64::try_from(
                snapshot_serial::SOURCE_PREFIX_LEN + snapshot_serial::DESTINATION_SUFFIX_LEN,
            )
            .expect("restored serial read count should fit"),
        "destination guest must drain the restored FIFO and fresh destination input"
    );
    assert_eq!(
        fs::read(&opened.state).expect("opened production serial state should read"),
        state_before,
        "destination load must not mutate the immutable state artifact"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("opened production serial memory should read"),
        memory_before,
        "destination load must not mutate the immutable memory artifact"
    );
    assert_eq!(
        fs::read(&destination_fixture.metrics)
            .expect("replacement destination metrics should read"),
        b"replacement metrics must remain unused\n"
    );
}

fn run_production_configured_serial_snapshot_continuation(bundle: &Path, enable_pci: bool) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let source_fixture = SerialSnapshotSourceGrantFixture::new(transport, true, true);
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        source_fixture.sensitive_strings(),
        &format!("serial-snapshot-{transport}-source"),
        false,
        enable_pci,
    );
    source_fixture.replace_source_pathnames();
    configure_and_start_serial_snapshot_grant_source(&source.socket, true, true);
    let source_serial = source_fixture
        .opened_serial
        .as_ref()
        .expect("configured source serial output should exist");
    wait_for_file_contains(
        source_serial,
        snapshot_serial::CONFIGURED_SOURCE_MARKER.as_bytes(),
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production configured {transport} serial source should become ready: {error}")
    });
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production configured {transport} serial source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production configured {transport} serial snapshot"),
    );
    let artifacts = source_fixture.artifacts();
    let state_before =
        fs::read(&artifacts.state).expect("configured production serial state should read");
    let memory_before =
        fs::read(&artifacts.memory).expect("configured production serial memory should read");
    let drive_before = fs::read(
        artifacts
            .drive
            .as_ref()
            .expect("configured production serial drive should exist"),
    )
    .expect("configured production serial drive should read");
    stop_running_launcher(
        &mut source,
        &format!("production configured {transport} serial source"),
    );
    let source_output =
        fs::read_to_string(source_serial).expect("configured source serial output should read");
    assert!(source_output.contains(snapshot_serial::CONFIGURED_SOURCE_MARKER));
    assert!(!source_output.contains(snapshot_serial::CONFIGURED_RESTORED_MARKER));
    assert_eq!(
        fs::read(
            source_fixture
                .serial
                .as_ref()
                .expect("configured source replacement serial should exist")
        )
        .expect("configured source replacement serial should read"),
        b"replacement serial output must remain unused\n"
    );

    let destination_fixture = SerialSnapshotInputGrantFixture::new(transport, artifacts, true);
    let mut destination = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &destination_fixture.manifest,
        destination_fixture.sensitive_strings(),
        &format!("serial-snapshot-{transport}-destination"),
        false,
        enable_pci,
    );
    let opened = destination_fixture.replace_source_pathnames();
    configure_serial_snapshot_grant_destination_metrics(&destination.socket);
    assert_http_status(
        &http_put(
            &destination.socket,
            "/snapshot/load",
            &snapshot_load_body(true),
        ),
        204,
        &format!("load production configured {transport} serial snapshot"),
    );
    let status = destination.wait(&format!(
        "production configured {transport} restored serial guest"
    ));
    assert!(
        status.success(),
        "production configured {transport} destination should exit cleanly: {status:?}"
    );
    assert!(!destination.socket.exists());
    let destination_serial = destination_fixture
        .opened_serial
        .as_ref()
        .expect("configured destination serial output should exist");
    wait_for_file_contains(
        destination_serial,
        snapshot_serial::CONFIGURED_RESTORED_MARKER.as_bytes(),
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production configured {transport} destination should publish success: {error}")
    });
    let destination_output = fs::read_to_string(destination_serial)
        .expect("configured destination serial output should read");
    assert!(destination_output.contains(snapshot_serial::CONFIGURED_RESTORED_MARKER));
    assert!(!destination_output.contains(snapshot_serial::CONFIGURED_FAILURE_MARKER));
    assert_eq!(
        fs::read(
            destination_fixture
                .serial
                .as_ref()
                .expect("configured destination replacement serial should exist")
        )
        .expect("configured destination replacement serial should read"),
        b"replacement serial output must remain unused\n"
    );
    assert_eq!(
        fs::read(&opened.state).expect("opened configured serial state should read"),
        state_before
    );
    assert_eq!(
        fs::read(&opened.memory).expect("opened configured serial memory should read"),
        memory_before
    );
    assert_eq!(
        fs::read(
            opened
                .drive
                .as_ref()
                .expect("opened configured serial drive should exist")
        )
        .expect("opened configured serial drive should read"),
        drive_before
    );
}

#[test]
fn normal_bundle_certifies_native_v2_vsock_snapshot_continuation_and_containment() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    for enable_pci in [false, true] {
        run_production_vsock_snapshot_continuation(&bundle, enable_pci, &baseline_sessions);
    }
    assert_eq!(
        session_entries(),
        baseline_sessions,
        "vsock snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_production_vsock_snapshot_continuation(
    bundle: &Path,
    enable_pci: bool,
    baseline_sessions: &[PathBuf],
) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let source_fixture = SnapshotVsockSourceGrantFixture::new(&format!("{transport}-vsock-source"));
    reset_zeroed_file(
        &source_fixture.snapshot.data_backing,
        8 * VIRTIO_BLOCK_SECTOR_BYTES,
    );
    let old_port_path = source_fixture.port_path(SNAPSHOT_VSOCK_OLD_PORT);
    let old_listener = UnixListener::bind(&old_port_path)
        .expect("production snapshot-vsock old listener should bind");
    old_listener
        .set_nonblocking(true)
        .expect("production snapshot-vsock old listener should be nonblocking");
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.snapshot.manifest,
        source_fixture.sensitive_strings(),
        &format!("vsock-snapshot-{transport}-source"),
        false,
        enable_pci,
    );
    source_fixture.snapshot.replace_source_file_pathnames();
    configure_and_start_production_vsock_snapshot_source(&source.socket, transport);
    assert!(
        source_fixture.socket().exists(),
        "production {transport} source should publish its granted main vsock listener"
    );
    let worker = only_worker_pid(&source.child);
    assert!(
        child_pids(worker).is_empty(),
        "production {transport} source vsock must not retain a helper"
    );
    let mut old_stream = wait_for_unix_listener_accept(&old_listener, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("production {transport} old snapshot-vsock connection should arrive: {error}")
        });
    old_stream
        .set_nonblocking(true)
        .expect("production old snapshot-vsock stream should be nonblocking");
    let mut old_ready = vec![0_u8; SNAPSHOT_VSOCK_OLD_READY.len()];
    read_exact_nonblocking(&mut old_stream, &mut old_ready, PROCESS_TIMEOUT)
        .expect("production snapshot-vsock old readiness should arrive");
    assert_eq!(
        old_ready, SNAPSHOT_VSOCK_OLD_READY,
        "production {transport} old readiness should match"
    );
    // Accept can win before the launcher completes its post-connect identity
    // check and SCM_RIGHTS handoff. Exact guest readiness proves that boundary.
    drop(old_listener);
    fs::remove_file(&old_port_path)
        .expect("production snapshot-vsock old listener path should clean up");

    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production {transport} vsock snapshot source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production {transport} vsock snapshot"),
    );
    let artifacts = source_fixture.snapshot.artifacts();
    assert_production_vsock_snapshot(&artifacts.state, enable_pci, &format!("{transport} source"));
    let state_before =
        fs::read(&artifacts.state).expect("production vsock source state should read");
    let memory_before =
        fs::read(&artifacts.memory).expect("production vsock source memory should read");
    wait_for_stream_eof_nonblocking(&mut old_stream, PROCESS_TIMEOUT)
        .expect("production snapshot capture should reset the source-only old stream");
    drop(old_stream);
    assert_no_snapshot_staging(&source_fixture.snapshot.state_directory);
    assert_no_snapshot_staging(&source_fixture.snapshot.memory_directory);
    stop_running_launcher(
        &mut source,
        &format!("production {transport} vsock snapshot source"),
    );
    assert!(
        !source_fixture.socket().exists(),
        "production {transport} source shutdown should clean its granted listener"
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {transport} vsock snapshot source"),
    );
    source_fixture
        .snapshot
        .assert_replacement_pathnames_unused(&format!(
            "production {transport} vsock snapshot source"
        ));

    let explicit_case = format!("{transport}-vsock-explicit");
    let current = run_production_vsock_snapshot_destination(ProductionVsockSnapshotDestination {
        bundle,
        artifacts,
        enable_pci,
        resume_vm: false,
        use_override: false,
        recapture: true,
        case: &explicit_case,
        baseline_sessions,
    });
    assert_eq!(
        fs::read(&current.state).expect("production repeated vsock state should read"),
        state_before,
        "production {transport} first load must not mutate state"
    );
    assert_eq!(
        fs::read(&current.memory).expect("production repeated vsock memory should read"),
        memory_before,
        "production {transport} first load must not mutate memory"
    );

    let automatic_case = format!("{transport}-vsock-automatic");
    let final_artifacts =
        run_production_vsock_snapshot_destination(ProductionVsockSnapshotDestination {
            bundle,
            artifacts: current,
            enable_pci,
            resume_vm: true,
            use_override: true,
            recapture: false,
            case: &automatic_case,
            baseline_sessions,
        });
    assert_eq!(
        fs::read(&final_artifacts.state).expect("final production vsock state should read"),
        state_before,
        "production {transport} repeated loads must keep state immutable"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final production vsock memory should read"),
        memory_before,
        "production {transport} repeated loads must keep memory immutable"
    );
}

fn configure_and_start_production_vsock_snapshot_source(socket: &Path, context: &str) {
    for (path, body, request) in [
        (
            "/machine-config",
            serde_json::json!({"vcpu_count": 1, "mem_size_mib": 256}),
            "machine config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "metrics",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "boot_args": SNAPSHOT_VSOCK_BOOT_ARGS,
            }),
            "boot source",
        ),
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": false,
                "cache_type": "Unsafe",
                "io_engine": "Async",
            }),
            "rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": SNAPSHOT_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "io_engine": "Sync",
            }),
            "data drive",
        ),
        (
            "/vsock",
            serde_json::json!({
                "guest_cid": 3,
                "uds_path": SNAPSHOT_VSOCK_SOURCE_REF,
            }),
            "vsock",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production snapshot-vsock request should serialize"),
            ),
            204,
            &format!("PUT production {context} snapshot-vsock {request}"),
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        &format!("start production {context} snapshot-vsock source"),
    );
}

struct ProductionVsockSnapshotDestination<'a> {
    bundle: &'a Path,
    artifacts: SnapshotArtifactSet,
    enable_pci: bool,
    resume_vm: bool,
    use_override: bool,
    recapture: bool,
    case: &'a str,
    baseline_sessions: &'a [PathBuf],
}

fn run_production_vsock_snapshot_destination(
    destination: ProductionVsockSnapshotDestination<'_>,
) -> SnapshotArtifactSet {
    let ProductionVsockSnapshotDestination {
        bundle,
        artifacts,
        enable_pci,
        resume_vm,
        use_override,
        recapture,
        case,
        baseline_sessions,
    } = destination;
    let fixture =
        SnapshotVsockContinuationInputGrantFixture::new(case, artifacts, recapture, use_override);
    let fresh_port_path = fixture.port_path(SNAPSHOT_VSOCK_FRESH_PORT);
    let fresh_listener = UnixListener::bind(&fresh_port_path)
        .expect("production restored snapshot-vsock fresh listener should bind");
    fresh_listener
        .set_nonblocking(true)
        .expect("production restored snapshot-vsock listener should be nonblocking");
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.snapshot.manifest,
        &fixture.snapshot.api_socket(),
        fixture.sensitive_strings(),
        &format!("vsock-snapshot-{case}"),
        enable_pci,
    );
    let worker = only_worker_pid(&running.child);
    let opened = fixture.snapshot.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("production destination vsock state should read");
    let memory_before =
        fs::read(&opened.memory).expect("production destination vsock memory should read");
    reset_zeroed_file(&opened.data, 8 * VIRTIO_BLOCK_SECTOR_BYTES);
    let load_body = production_vsock_snapshot_load_body(
        resume_vm,
        use_override.then_some(fixture.selector_ref),
    );
    assert_http_status(
        &http_put(&running.socket, "/snapshot/load", &load_body),
        204,
        &format!("load production {case} vsock snapshot"),
    );
    assert!(
        http_get(&running.socket, "/").contains(if resume_vm {
            r#""state":"Running""#
        } else {
            r#""state":"Paused""#
        }),
        "production {case} destination should publish the requested resume state"
    );
    let config = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &config,
        200,
        &format!("read production {case} restored vsock config"),
    );
    assert!(
        config.contains(fixture.selector_ref),
        "production {case} controller commit should retain the selected vsock reference"
    );
    assert!(
        fixture.socket().exists(),
        "production {case} destination should own its selected granted listener"
    );
    assert!(
        child_pids(worker).is_empty(),
        "production {case} restored vsock must not retain a helper"
    );
    if !resume_vm {
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            204,
            &format!("resume production {case} vsock destination"),
        );
    }

    let mut fresh_stream = wait_for_unix_listener_accept(&fresh_listener, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("production {case} restored fresh vsock connection should arrive: {error}")
        });
    fresh_stream
        .set_nonblocking(true)
        .expect("production restored fresh vsock stream should be nonblocking");
    let mut fresh_ready = vec![0_u8; SNAPSHOT_VSOCK_FRESH_READY.len()];
    read_exact_nonblocking(&mut fresh_stream, &mut fresh_ready, PROCESS_TIMEOUT)
        .expect("production restored fresh vsock readiness should arrive");
    assert_eq!(
        fresh_ready, SNAPSHOT_VSOCK_FRESH_READY,
        "production {case} fresh readiness should match"
    );
    // Keep pathname authority through the launcher's identity check and FD
    // handoff; exact restored-guest readiness proves both have completed.
    drop(fresh_listener);
    fs::remove_file(&fresh_port_path)
        .expect("production restored snapshot-vsock listener path should clean up");
    write_all_nonblocking(&mut fresh_stream, SNAPSHOT_VSOCK_FRESH_ACK, PROCESS_TIMEOUT)
        .expect("production restored fresh vsock acknowledgement should write");
    wait_for_stream_eof_nonblocking(&mut fresh_stream, PROCESS_TIMEOUT)
        .expect("production restored fresh vsock stream should close cleanly");
    wait_for_file_contains(&opened.data, SNAPSHOT_VSOCK_SUCCESS, PROCESS_TIMEOUT).unwrap_or_else(
        |error| {
            panic!("production {case} restored guest should confirm fresh vsock traffic: {error}")
        },
    );

    if recapture {
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            204,
            &format!("pause production {case} vsock destination before recapture"),
        );
        assert_http_status(
            &http_put(&running.socket, "/snapshot/create", &snapshot_create_body()),
            204,
            &format!("recapture production {case} vsock snapshot"),
        );
        let recaptured = fixture.snapshot.recaptured_artifacts();
        assert_production_vsock_snapshot(
            &recaptured.state,
            enable_pci,
            &format!("{case} recapture"),
        );
        fixture.snapshot.assert_no_recapture_staging();
    }

    stop_running_launcher(
        &mut running,
        &format!("production {case} restored vsock destination"),
    );
    assert!(
        !fixture.socket().exists(),
        "production {case} shutdown should clean its selected listener"
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {case} restored vsock destination"),
    );
    assert_eq!(
        fs::read(&opened.state).expect("production destination vsock state should remain"),
        state_before,
        "production {case} load must not mutate state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("production destination vsock memory should remain"),
        memory_before,
        "production {case} load must not mutate memory"
    );
    fixture
        .snapshot
        .assert_replacement_pathnames_unused(&format!(
            "production {case} restored vsock destination"
        ));
    opened
}

fn production_vsock_snapshot_load_body(resume_vm: bool, selector: Option<&str>) -> String {
    let mut body = serde_json::json!({
        "snapshot_path": SNAPSHOT_STATE_INPUT_REF,
        "mem_backend": {
            "backend_path": SNAPSHOT_MEMORY_INPUT_REF,
            "backend_type": "File",
        },
        "resume_vm": resume_vm,
    });
    if let Some(selector) = selector {
        body["vsock_override"] = serde_json::json!({"uds_path": selector});
    }
    body.to_string()
}

fn assert_production_vsock_snapshot(path: &Path, enable_pci: bool, context: &str) -> u32 {
    let bytes = fs::read(path).expect("production exact-2.12 vsock state should read");
    let structural =
        decode_snapshot_v2_state(&bytes).expect("production exact-2.12 vsock state should decode");
    let state = decode_hvf_snapshot_v2_vsock_state(&structural)
        .expect("production exact-2.12 vsock state should decode semantically");
    let vsock = state
        .vsock()
        .expect("production exact-2.12 activation state should contain kind 13");
    assert_eq!(
        vsock.guest_cid(),
        3,
        "production {context} guest CID should persist"
    );
    assert_eq!(
        vsock.transport().kind(),
        if enable_pci {
            SnapshotV2DeviceTransportKind::Pci
        } else {
            SnapshotV2DeviceTransportKind::Mmio
        },
        "production {context} vsock transport should persist"
    );
    vsock.host_local_port_cursor().last_used()
}

#[test]
fn normal_bundle_certifies_native_v2_vsock_restored_guest_lifecycle_and_containment() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    for enable_pci in [false, true] {
        run_certified_production_vsock_snapshot_restore(&bundle, enable_pci, &baseline_sessions);
    }
    assert_eq!(
        session_entries(),
        baseline_sessions,
        "certified vsock snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_certified_production_vsock_snapshot_restore(
    bundle: &Path,
    enable_pci: bool,
    baseline_sessions: &[PathBuf],
) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let source_fixture = SnapshotVsockSourceGrantFixture::new_with_read_only_root(&format!(
        "{transport}-vsock-certified-source"
    ));
    let source_logger = DeviceLoggerGrant::add_to_manifest(
        &source_fixture.snapshot.manifest,
        &format!("{transport}-vsock-certified-source"),
    );
    let source_sensitive = source_fixture
        .sensitive_strings()
        .into_iter()
        .chain(source_logger.sensitive_strings())
        .collect::<Vec<_>>();
    let source_guest_listeners = bind_certified_production_vsock_guest_listeners(
        &source_fixture,
        &SNAPSHOT_VSOCK_CERTIFY_SOURCE_GUEST_PORTS,
        &format!("production {transport} source"),
    );
    let mut source = spawn_ready_serial_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.snapshot.manifest,
        &format!("vsock-certified-{transport}-source"),
        enable_pci,
    );
    source_fixture.snapshot.replace_source_file_pathnames();
    source_logger.replace_source_pathname();
    configure_and_start_certified_production_vsock_source(&source.socket, transport);
    assert_socket_mode(
        &source_fixture.socket(),
        0o600,
        &format!("production {transport} certified source vsock"),
    );
    let worker = only_worker_pid(&source.child);
    assert!(
        child_pids(worker).is_empty(),
        "production {transport} certified source must not retain a helper"
    );

    let mut source_guest_streams = accept_certified_production_vsock_guest_streams(
        source_guest_listeners,
        CertifiedProductionVsockGuestExchange {
            request_kind: "SOURCE_G2H",
            response_kind: "SOURCE_G2H_ACK",
            request_size: 1024,
            response_size: 128,
            finish_exchange: false,
        },
        &format!("production {transport} source"),
        &source,
        &source_logger.opened,
        &source_fixture.snapshot.opened_metrics,
    );
    let (mut source_host_streams, source_host_ports) =
        connect_certified_production_vsock_source_host_streams(
            &source_fixture.socket(),
            &format!("production {transport} source"),
            &source,
            &source_logger.opened,
            &source_fixture.snapshot.opened_metrics,
        );
    assert_eq!(
        source_host_ports,
        vec![VSOCK_HOST_LOCAL_PORT_BASE],
        "production {transport} source host-local port must start at the pinned Firecracker base"
    );
    source
        .wait_for_stdout_marker(SNAPSHOT_VSOCK_CERTIFY_SOURCE_READY, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("production {transport} certified source should become ready: {error}")
        });
    flush_certified_production_vsock_metrics(
        &source.socket,
        &source_fixture.snapshot.opened_metrics,
        2,
        &format!("production {transport} source"),
    );

    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production {transport} certified vsock source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production {transport} certified vsock snapshot"),
    );
    let mut artifacts = source_fixture.snapshot.artifacts();
    let saved_cursor = assert_production_vsock_snapshot(
        &artifacts.state,
        enable_pci,
        &format!("{transport} certified source"),
    );
    assert_eq!(
        saved_cursor, VSOCK_HOST_LOCAL_PORT_BASE,
        "production {transport} snapshot must retain the exact source host-local cursor"
    );
    let state_before =
        fs::read(&artifacts.state).expect("production certified vsock state should read");
    let memory_before =
        fs::read(&artifacts.memory).expect("production certified vsock memory should read");
    for (kind, streams) in [
        ("guest-to-host", &mut source_guest_streams),
        ("host-to-guest", &mut source_host_streams),
    ] {
        for (index, stream) in streams.iter_mut().enumerate() {
            wait_for_stream_eof_nonblocking(stream, PROCESS_TIMEOUT).unwrap_or_else(|error| {
                panic!(
                    "production {transport} source {kind} stream {index} must be lost at capture: {error}"
                )
            });
        }
    }
    assert_no_snapshot_staging(&source_fixture.snapshot.state_directory);
    assert_no_snapshot_staging(&source_fixture.snapshot.memory_directory);
    let (source_stdout, source_stderr) = stop_certified_production_vsock_launcher(
        &mut source,
        &source_sensitive,
        &format!("production {transport} certified vsock source"),
    );
    assert!(
        source_stdout.contains(SNAPSHOT_VSOCK_CERTIFY_SOURCE_READY),
        "production {transport} source output must retain its readiness evidence"
    );
    assert!(
        !source_stdout.contains(SNAPSHOT_VSOCK_CERTIFY_RESET_OBSERVED)
            && !source_stdout.contains(SNAPSHOT_VSOCK_CERTIFY_FAILURE),
        "production {transport} terminated source must not claim restored work\nstdout:\n{source_stdout}\nstderr:\n{source_stderr}"
    );
    assert!(
        !source_fixture.socket().exists(),
        "production {transport} source shutdown must clean its granted listener"
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {transport} certified vsock source"),
    );
    source_fixture
        .snapshot
        .assert_replacement_pathnames_unused(&format!(
            "production {transport} certified vsock source"
        ));
    source_logger.assert_records(
        &[
            "device-kind=vsock operation=rx outcome=succeeded",
            "device-kind=vsock operation=tx outcome=succeeded",
        ],
        source_fixture.sensitive_strings(),
    );

    if !enable_pci {
        run_certified_production_vsock_hostile_loads(bundle, &artifacts, baseline_sessions);
        artifacts = run_certified_production_vsock_missing_grant_rejection(
            bundle,
            artifacts,
            baseline_sessions,
        );
        for order in [
            CertifiedProductionVsockShutdownOrder::Graceful,
            CertifiedProductionVsockShutdownOrder::WorkerFirst,
            CertifiedProductionVsockShutdownOrder::LauncherFirst,
        ] {
            artifacts = run_certified_production_vsock_paused_shutdown(
                bundle,
                artifacts,
                order,
                baseline_sessions,
            );
        }
    }

    let explicit_case = format!("{transport}-vsock-certified-explicit");
    let current = run_certified_production_vsock_destination(CertifiedProductionVsockDestination {
        bundle,
        artifacts,
        enable_pci,
        resume_vm: false,
        use_override: false,
        recapture: true,
        saved_cursor,
        case: &explicit_case,
        baseline_sessions,
    });
    assert_eq!(
        fs::read(&current.state).expect("production certified repeated state should read"),
        state_before,
        "production {transport} explicit load must not mutate the immutable state"
    );
    assert_eq!(
        fs::read(&current.memory).expect("production certified repeated memory should read"),
        memory_before,
        "production {transport} explicit load must not mutate the immutable memory"
    );

    let automatic_case = format!("{transport}-vsock-certified-automatic");
    let final_artifacts =
        run_certified_production_vsock_destination(CertifiedProductionVsockDestination {
            bundle,
            artifacts: current,
            enable_pci,
            resume_vm: true,
            use_override: true,
            recapture: false,
            saved_cursor,
            case: &automatic_case,
            baseline_sessions,
        });
    assert_eq!(
        fs::read(&final_artifacts.state)
            .expect("final production certified vsock state should read"),
        state_before,
        "production {transport} repeated clones must keep state immutable"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory)
            .expect("final production certified vsock memory should read"),
        memory_before,
        "production {transport} repeated clones must keep memory immutable"
    );
}

struct CertifiedProductionVsockHostileArtifacts {
    _root: TestDir,
    artifacts: SnapshotArtifactSet,
}

impl CertifiedProductionVsockHostileArtifacts {
    fn new(case: &str, sources: &SnapshotArtifactSet, copy_state: bool, copy_memory: bool) -> Self {
        let root = TestDir::new(&format!("certified-vsock-hostile-{case}"));
        let canonical_root = fs::canonicalize(root.path())
            .expect("certified hostile vsock root should canonicalize");
        let artifacts = SnapshotArtifactSet {
            state: canonical_root.join("snapshot.state"),
            memory: canonical_root.join("snapshot.memory"),
            root: canonical_root.join("root.img"),
            data: canonical_root.join("data.img"),
            audit: canonical_root.join("audit.img"),
        };
        for (source, destination, copy) in [
            (&sources.state, &artifacts.state, copy_state),
            (&sources.memory, &artifacts.memory, copy_memory),
            (&sources.root, &artifacts.root, false),
            (&sources.data, &artifacts.data, false),
            (&sources.audit, &artifacts.audit, false),
        ] {
            if copy {
                fs::copy(source, destination)
                    .unwrap_or_else(|error| panic!("{case} hostile artifact should copy: {error}"));
            } else {
                hard_link_or_copy_fixture(source, destination, case);
            }
        }
        Self {
            _root: root,
            artifacts,
        }
    }
}

fn run_certified_production_vsock_hostile_loads(
    bundle: &Path,
    sources: &SnapshotArtifactSet,
    baseline_sessions: &[PathBuf],
) {
    let checksum = CertifiedProductionVsockHostileArtifacts::new("checksum", sources, true, false);
    let mut checksum_bytes =
        fs::read(&checksum.artifacts.state).expect("hostile checksum state should read");
    let checksum_index = checksum_bytes.len() / 2;
    checksum_bytes[checksum_index] ^= 0x80;
    fs::write(&checksum.artifacts.state, checksum_bytes)
        .expect("hostile checksum state should update");
    run_certified_production_vsock_rejected_load(
        bundle,
        &checksum.artifacts,
        "checksum",
        baseline_sessions,
    );

    let truncated =
        CertifiedProductionVsockHostileArtifacts::new("truncated-memory", sources, false, true);
    let memory_len = fs::metadata(&truncated.artifacts.memory)
        .expect("hostile memory metadata should read")
        .len();
    assert!(
        memory_len > 4096,
        "hostile memory fixture should be large enough to truncate"
    );
    OpenOptions::new()
        .write(true)
        .open(&truncated.artifacts.memory)
        .expect("hostile memory should reopen")
        .set_len(memory_len - 4096)
        .expect("hostile memory should truncate");
    run_certified_production_vsock_rejected_load(
        bundle,
        &truncated.artifacts,
        "truncated-memory",
        baseline_sessions,
    );

    let no_vsock = CertifiedProductionVsockHostileArtifacts::new("no-vsock", sources, true, false);
    write_certified_production_snapshot_without_vsock(&sources.state, &no_vsock.artifacts.state);
    run_certified_production_vsock_rejected_load(
        bundle,
        &no_vsock.artifacts,
        "no-vsock",
        baseline_sessions,
    );
}

fn write_certified_production_snapshot_without_vsock(source: &Path, destination: &Path) {
    let bytes = fs::read(source).expect("production certified source state should read");
    let structural =
        decode_snapshot_v2_state(&bytes).expect("production certified source state should decode");
    let required_features = structural.required_features().collect::<Vec<_>>();
    let mut removed = 0;
    let components = structural
        .components()
        .filter(|component| {
            let retain = component.key() != NATIVE_V2_VSOCK_COMPONENT_KEY;
            removed += usize::from(!retain);
            retain
        })
        .collect::<Vec<SnapshotV2Component<'_>>>();
    assert_eq!(
        removed, 1,
        "production certified fixture should remove one kind 13 component"
    );
    let encoded = encode_snapshot_v2_state_with_compatibility_version(
        structural.metadata().version(),
        &required_features,
        &components,
    )
    .expect("production certified no-vsock state should encode");
    fs::write(destination, encoded).expect("production certified no-vsock state should write");
}

fn run_certified_production_vsock_rejected_load(
    bundle: &Path,
    sources: &SnapshotArtifactSet,
    case: &str,
    baseline_sessions: &[PathBuf],
) {
    let fixture = SnapshotVsockContinuationInputGrantFixture::new_with_read_only_root(
        &format!("certified-hostile-{case}"),
        sources.clone(),
        false,
        true,
    );
    let sensitive = fixture.sensitive_strings();
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.snapshot.manifest,
        &fixture.snapshot.api_socket(),
        sensitive.clone(),
        &format!("vsock-certified-hostile-{case}"),
        false,
    );
    let opened = fixture.snapshot.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("hostile production state should remain readable");
    let memory_before =
        fs::read(&opened.memory).expect("hostile production memory should remain readable");
    assert_http_status(
        &http_put(
            &running.socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        &format!("configure hostile production {case} metrics"),
    );
    let response = http_put(
        &running.socket,
        "/snapshot/load",
        &production_vsock_snapshot_load_body(false, Some(fixture.selector_ref)),
    );
    assert_http_status(
        &response,
        400,
        &format!("reject hostile production {case} vsock snapshot"),
    );
    for private in &sensitive {
        assert!(
            !response.contains(private),
            "hostile production {case} response must redact private authority"
        );
    }
    thread::sleep(Duration::from_millis(100));
    if running
        .child
        .try_wait()
        .expect("hostile production launcher status should read")
        .is_some()
        || !running.socket.exists()
    {
        let status = running.wait(&format!("terminal hostile production {case} rejection"));
        assert!(
            !status.success(),
            "terminal hostile production {case} rejection must fail closed"
        );
    } else {
        assert!(
            http_get(&running.socket, "/").contains(r#""state":"Not started""#),
            "hostile production {case} destination must remain Not started"
        );
        stop_running_launcher(
            &mut running,
            &format!("hostile production {case} vsock rejection"),
        );
    }
    assert!(
        !running.socket.exists() && !fixture.socket().exists(),
        "hostile production {case} rejection must not retain API or vsock listeners"
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("hostile production {case} vsock rejection"),
    );
    assert_eq!(
        fs::read(&opened.state).expect("hostile production state should remain"),
        state_before,
        "hostile production {case} rejection must not mutate state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("hostile production memory should remain"),
        memory_before,
        "hostile production {case} rejection must not mutate memory"
    );
    fixture
        .snapshot
        .assert_replacement_pathnames_unused(&format!("hostile production {case} vsock rejection"));
}

fn run_certified_production_vsock_missing_grant_rejection(
    bundle: &Path,
    artifacts: SnapshotArtifactSet,
    baseline_sessions: &[PathBuf],
) -> SnapshotArtifactSet {
    let fixture = SnapshotVsockContinuationInputGrantFixture::new_with_read_only_root(
        "certified-missing-vsock-grant",
        artifacts,
        false,
        true,
    );
    remove_snapshot_vsock_grant(&fixture.snapshot.manifest, fixture.selector_id);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.snapshot.manifest,
        &fixture.snapshot.api_socket(),
        fixture.sensitive_strings(),
        "vsock-certified-missing-grant",
        false,
    );
    let opened = fixture.snapshot.replace_source_pathnames();
    let state_before = fs::read(&opened.state).expect("missing-grant production state should read");
    let memory_before =
        fs::read(&opened.memory).expect("missing-grant production memory should read");
    let response = http_put(
        &running.socket,
        "/snapshot/load",
        &production_vsock_snapshot_load_body(false, Some(fixture.selector_ref)),
    );
    assert_http_status(
        &response,
        400,
        "reject production vsock override without its exact grant",
    );
    assert!(
        response.contains("current-product destination transaction failed (retryable)")
            && !response.contains(fixture.selector_ref)
            && !response.contains(path_text(&fixture.vsock_directory)),
        "missing-grant production retryable fault must remain stable and redacted: {response}"
    );
    assert!(
        !fixture.socket().exists(),
        "missing-grant production rejection must not publish a vsock listener"
    );
    thread::sleep(Duration::from_millis(250));
    if running
        .child
        .try_wait()
        .expect("missing-grant production launcher status should read")
        .is_some()
    {
        let status = running.wait("terminal production certified missing-vsock-grant rejection");
        assert!(
            !status.success(),
            "terminal missing-grant production rejection must fail closed"
        );
    } else {
        let launcher =
            i32::try_from(running.child.id()).expect("missing-grant launcher PID should fit");
        // SAFETY: `launcher` is the live unreaped process owned by `running`.
        assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
        let _ = running
            .wait("production certified missing-vsock-grant rejection after retryable response");
    }
    assert_session_entries_eventually_restored(
        baseline_sessions,
        "production certified missing-vsock-grant rejection",
    );
    assert_eq!(
        fs::read(&opened.state).expect("missing-grant state should remain"),
        state_before
    );
    assert_eq!(
        fs::read(&opened.memory).expect("missing-grant memory should remain"),
        memory_before
    );
    fixture
        .snapshot
        .assert_replacement_pathnames_unused("production certified missing-vsock-grant rejection");
    opened
}

fn remove_snapshot_vsock_grant(manifest: &Path, id: &str) {
    let mut value: serde_json::Value = serde_json::from_slice(
        &fs::read(manifest).expect("production certified manifest should read"),
    )
    .expect("production certified manifest should parse");
    let grants = value
        .get_mut("grants")
        .and_then(serde_json::Value::as_array_mut)
        .expect("production certified manifest should contain grants");
    let before = grants.len();
    grants.retain(|grant| grant.get("id").and_then(serde_json::Value::as_str) != Some(id));
    assert_eq!(
        grants.len() + 1,
        before,
        "production certified manifest should remove exactly one vsock grant"
    );
    fs::write(
        manifest,
        serde_json::to_vec(&value).expect("production certified manifest should serialize"),
    )
    .expect("production certified manifest should update");
}

#[derive(Debug, Clone, Copy)]
enum CertifiedProductionVsockShutdownOrder {
    Graceful,
    WorkerFirst,
    LauncherFirst,
}

fn run_certified_production_vsock_paused_shutdown(
    bundle: &Path,
    artifacts: SnapshotArtifactSet,
    order: CertifiedProductionVsockShutdownOrder,
    baseline_sessions: &[PathBuf],
) -> SnapshotArtifactSet {
    let (name, use_override) = match order {
        CertifiedProductionVsockShutdownOrder::Graceful => ("graceful", false),
        CertifiedProductionVsockShutdownOrder::WorkerFirst => ("worker-first", true),
        CertifiedProductionVsockShutdownOrder::LauncherFirst => ("launcher-first", false),
    };
    let fixture = SnapshotVsockContinuationInputGrantFixture::new_with_read_only_root(
        &format!("certified-paused-{name}"),
        artifacts,
        false,
        use_override,
    );
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.snapshot.manifest,
        &fixture.snapshot.api_socket(),
        fixture.sensitive_strings(),
        &format!("vsock-certified-paused-{name}"),
        false,
    );
    let opened = fixture.snapshot.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("paused shutdown production state should read");
    let memory_before =
        fs::read(&opened.memory).expect("paused shutdown production memory should read");
    assert_http_status(
        &http_put(
            &running.socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        &format!("configure production {name} shutdown metrics"),
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &production_vsock_snapshot_load_body(
                false,
                use_override.then_some(fixture.selector_ref),
            ),
        ),
        204,
        &format!("load production {name} shutdown vsock pair"),
    );
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Paused""#),
        "production {name} shutdown destination must publish Paused"
    );
    assert!(
        fixture.socket().exists(),
        "production {name} shutdown destination must publish its selected listener"
    );

    match order {
        CertifiedProductionVsockShutdownOrder::Graceful => {
            stop_running_launcher(
                &mut running,
                "production certified Paused graceful cancellation",
            );
        }
        CertifiedProductionVsockShutdownOrder::WorkerFirst => {
            let worker = only_worker_pid(&running.child);
            // SAFETY: The worker is the live child of this unreaped launcher.
            assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
            assert_eq!(
                running
                    .wait("production certified Paused worker-first death")
                    .code(),
                Some(128 + libc::SIGKILL)
            );
        }
        CertifiedProductionVsockShutdownOrder::LauncherFirst => {
            let worker = only_worker_pid(&running.child);
            let worker_exit = ProcessExitWatch::new(worker);
            let launcher =
                i32::try_from(running.child.id()).expect("production launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID and the worker
            // observes authenticated lifecycle EOF independently.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
            assert_eq!(
                running
                    .wait("production certified Paused launcher-first death")
                    .signal(),
                Some(libc::SIGKILL)
            );
            assert!(
                worker_exit.wait(PROCESS_TIMEOUT),
                "production certified worker must observe launcher-first death"
            );
        }
    }

    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production certified Paused {name} shutdown"),
    );
    assert!(
        !running.socket.exists() && !fixture.socket().exists(),
        "production certified Paused {name} shutdown must clean API and vsock listeners"
    );
    assert_eq!(
        fs::read(&opened.state).expect("paused shutdown state should remain"),
        state_before,
        "production certified Paused {name} shutdown must preserve state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("paused shutdown memory should remain"),
        memory_before,
        "production certified Paused {name} shutdown must preserve memory"
    );
    fixture
        .snapshot
        .assert_replacement_pathnames_unused(&format!(
            "production certified Paused {name} shutdown"
        ));
    opened
}

fn configure_and_start_certified_production_vsock_source(socket: &Path, context: &str) {
    for (path, body, request) in [
        (
            "/machine-config",
            serde_json::json!({"vcpu_count": 1, "mem_size_mib": 256}),
            "machine config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "metrics",
        ),
        (
            "/logger",
            serde_json::json!({"log_path": OUTPUT_LOGGER_REF}),
            "logger",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "boot_args": SNAPSHOT_VSOCK_CERTIFY_BOOT_ARGS,
            }),
            "boot source",
        ),
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": true,
                "cache_type": "Unsafe",
                "io_engine": "Async",
            }),
            "rootfs",
        ),
        (
            "/vsock",
            serde_json::json!({
                "guest_cid": 3,
                "uds_path": SNAPSHOT_VSOCK_SOURCE_REF,
            }),
            "vsock",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("certified production snapshot-vsock request should serialize"),
            ),
            204,
            &format!("PUT production {context} certified snapshot-vsock {request}"),
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        &format!("start production {context} certified snapshot-vsock source"),
    );
}

struct CertifiedProductionVsockDestination<'a> {
    bundle: &'a Path,
    artifacts: SnapshotArtifactSet,
    enable_pci: bool,
    resume_vm: bool,
    use_override: bool,
    recapture: bool,
    saved_cursor: u32,
    case: &'a str,
    baseline_sessions: &'a [PathBuf],
}

fn run_certified_production_vsock_destination(
    destination: CertifiedProductionVsockDestination<'_>,
) -> SnapshotArtifactSet {
    let CertifiedProductionVsockDestination {
        bundle,
        artifacts,
        enable_pci,
        resume_vm,
        use_override,
        recapture,
        saved_cursor,
        case,
        baseline_sessions,
    } = destination;
    let fixture = SnapshotVsockContinuationInputGrantFixture::new_with_read_only_root(
        case,
        artifacts,
        recapture,
        use_override,
    );
    let logger = DeviceLoggerGrant::add_to_manifest(&fixture.snapshot.manifest, case);
    let sensitive = fixture
        .sensitive_strings()
        .into_iter()
        .chain(logger.sensitive_strings())
        .collect::<Vec<_>>();
    let guest_listeners = bind_certified_production_vsock_guest_listeners(
        &fixture,
        &SNAPSHOT_VSOCK_CERTIFY_FRESH_GUEST_PORTS,
        &format!("production {case} destination"),
    );
    let mut running = spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
        bundle,
        &fixture.snapshot.manifest,
        &fixture.snapshot.api_socket(),
        &format!("vsock-certified-{case}"),
        enable_pci,
    );
    let worker = only_worker_pid(&running.child);
    let opened = fixture.snapshot.replace_source_pathnames();
    logger.replace_source_pathname();
    let state_before =
        fs::read(&opened.state).expect("production certified destination state should read");
    let memory_before =
        fs::read(&opened.memory).expect("production certified destination memory should read");
    assert_http_status(
        &http_put(
            &running.socket,
            "/logger",
            &serde_json::json!({"log_path": OUTPUT_LOGGER_REF}).to_string(),
        ),
        204,
        &format!("configure production {case} certified destination logger"),
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        &format!("configure production {case} certified destination metrics"),
    );
    let load_body = production_vsock_snapshot_load_body(
        resume_vm,
        use_override.then_some(fixture.selector_ref),
    );
    assert_http_status(
        &http_put(&running.socket, "/snapshot/load", &load_body),
        204,
        &format!("load production {case} certified vsock snapshot"),
    );
    let state = http_get(&running.socket, "/");
    assert!(
        state.contains(if resume_vm {
            r#""state":"Running""#
        } else {
            r#""state":"Paused""#
        }),
        "production {case} certified destination must publish the requested resume state: {state}"
    );
    let config = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &config,
        200,
        &format!("read production {case} certified restored config"),
    );
    assert!(
        config.contains(fixture.selector_ref),
        "production {case} controller commit must retain the selected vsock reference"
    );
    assert_socket_mode(
        &fixture.socket(),
        0o600,
        &format!("production {case} selected vsock"),
    );
    assert!(
        child_pids(worker).is_empty(),
        "production {case} certified destination must not retain a helper"
    );

    let mut host_streams = queue_certified_production_vsock_host_streams(
        &fixture.socket(),
        &format!("production {case}"),
    );
    if !resume_vm {
        assert_certified_production_vsock_streams_blocked_while_paused(
            &mut host_streams,
            &format!("production {case}"),
        );
        assert!(
            !running
                .stdout_snapshot()
                .contains(SNAPSHOT_VSOCK_CERTIFY_RESET_OBSERVED),
            "production {case} Paused destination must not process reset or queued RX"
        );
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            204,
            &format!("resume production {case} certified vsock destination"),
        );
    }

    let fresh_guest_streams = accept_certified_production_vsock_guest_streams(
        guest_listeners,
        CertifiedProductionVsockGuestExchange {
            request_kind: "FRESH_G2H",
            response_kind: "FRESH_G2H_REPLY",
            request_size: 4096,
            response_size: 4096,
            finish_exchange: true,
        },
        &format!("production {case}"),
        &running,
        &logger.opened,
        &fixture.snapshot.opened_metrics,
    );
    drop(fresh_guest_streams);
    for index in 0..host_streams.len() {
        let local_port = match read_certified_production_vsock_connect_ok(
            &mut host_streams[index],
            PROCESS_TIMEOUT,
        ) {
            Ok(local_port) => local_port,
            Err(error) => panic!(
                "production {case} preserved-listener stream {index} should connect: {error}; host stream states: {}; {}",
                certified_production_vsock_host_stream_states(&host_streams),
                certified_production_vsock_failure_diagnostics(
                    &running,
                    &logger.opened,
                    &fixture.snapshot.opened_metrics,
                )
            ),
        };
        assert_eq!(
            local_port,
            saved_cursor
                + 1
                + u32::try_from(index).expect("production host stream index should fit u32"),
            "production {case} clone host-local cursor must continue independently at stream {index}"
        );
        let mut guest_reply = vec![0_u8; 4096];
        if let Err(error) =
            read_exact_nonblocking(&mut host_streams[index], &mut guest_reply, PROCESS_TIMEOUT)
        {
            panic!(
                "production {case} host stream {index} guest reply should arrive: {error}; host stream states: {}; {}",
                certified_production_vsock_host_stream_states(&host_streams),
                certified_production_vsock_failure_diagnostics(
                    &running,
                    &logger.opened,
                    &fixture.snapshot.opened_metrics,
                )
            );
        }
        assert_eq!(
            guest_reply,
            certified_snapshot_vsock_payload("FRESH_H2G_REPLY", index, 4096),
            "production {case} host stream {index} guest reply must remain isolated"
        );
        write_all_nonblocking(
            &mut host_streams[index],
            &certified_snapshot_vsock_payload("FRESH_H2G", index, 4096),
            PROCESS_TIMEOUT,
        )
        .unwrap_or_else(|error| {
            panic!(
                "production {case} host stream {index} payload should write: {error}; {}",
                certified_production_vsock_failure_diagnostics(
                    &running,
                    &logger.opened,
                    &fixture.snapshot.opened_metrics,
                )
            )
        });
        host_streams[index]
            .shutdown(std::net::Shutdown::Write)
            .unwrap_or_else(|error| {
                panic!("production {case} host stream {index} should half-close: {error}")
            });
        wait_for_stream_eof_nonblocking(&mut host_streams[index], PROCESS_TIMEOUT).unwrap_or_else(
            |error| {
                panic!(
                    "production {case} host stream {index} EOF should arrive: {error}; {}",
                    certified_production_vsock_failure_diagnostics(
                        &running,
                        &logger.opened,
                        &fixture.snapshot.opened_metrics,
                    )
                )
            },
        );
    }
    for marker in [
        SNAPSHOT_VSOCK_CERTIFY_RESET_OBSERVED,
        SNAPSHOT_VSOCK_CERTIFY_FRESH_G2H_OK,
        SNAPSHOT_VSOCK_CERTIFY_PRESERVED_LISTENER_OK,
        std::str::from_utf8(SNAPSHOT_VSOCK_SUCCESS)
            .expect("production snapshot-vsock success marker should be UTF-8"),
    ] {
        running
            .wait_for_stdout_marker(marker, PROCESS_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("production {case} restored guest must publish {marker:?}: {error}")
            });
    }
    assert!(
        !running
            .stdout_snapshot()
            .contains(SNAPSHOT_VSOCK_CERTIFY_FAILURE),
        "production {case} restored guest must not publish a failure marker"
    );
    flush_certified_production_vsock_metrics(
        &running.socket,
        &fixture.snapshot.opened_metrics,
        u64::try_from(
            SNAPSHOT_VSOCK_CERTIFY_FRESH_GUEST_PORTS.len() + SNAPSHOT_VSOCK_CERTIFY_HOST_STREAMS,
        )
        .expect("production destination connection count should fit u64"),
        &format!("production {case} destination"),
    );

    if recapture {
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            204,
            &format!("pause production {case} certified destination before recapture"),
        );
        assert_http_status(
            &http_put(&running.socket, "/snapshot/create", &snapshot_create_body()),
            204,
            &format!("recapture production {case} certified vsock snapshot"),
        );
        let recaptured = fixture.snapshot.recaptured_artifacts();
        assert_eq!(
            assert_production_vsock_snapshot(
                &recaptured.state,
                enable_pci,
                &format!("{case} certified recapture"),
            ),
            saved_cursor
                + u32::try_from(SNAPSHOT_VSOCK_CERTIFY_HOST_STREAMS)
                    .expect("production host stream count should fit u32"),
            "production {case} recapture must retain destination-local cursor progression"
        );
        fixture.snapshot.assert_no_recapture_staging();
    }

    let (stdout, stderr) = stop_certified_production_vsock_launcher(
        &mut running,
        &sensitive,
        &format!("production {case} certified vsock destination"),
    );
    assert!(
        stdout.contains(SNAPSHOT_VSOCK_CERTIFY_RESET_OBSERVED)
            && stdout.contains(SNAPSHOT_VSOCK_CERTIFY_FRESH_G2H_OK)
            && stdout.contains(SNAPSHOT_VSOCK_CERTIFY_PRESERVED_LISTENER_OK)
            && stdout.contains(
                std::str::from_utf8(SNAPSHOT_VSOCK_SUCCESS)
                    .expect("production snapshot-vsock success marker should be UTF-8")
            )
            && !stdout.contains(SNAPSHOT_VSOCK_CERTIFY_FAILURE),
        "production {case} output must retain complete process-local success evidence\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !fixture.socket().exists(),
        "production {case} shutdown must clean its selected listener"
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {case} certified vsock destination"),
    );
    assert_eq!(
        fs::read(&opened.state).expect("production certified destination state should remain"),
        state_before,
        "production {case} load must not mutate state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("production certified destination memory should remain"),
        memory_before,
        "production {case} load must not mutate memory"
    );
    fixture
        .snapshot
        .assert_replacement_pathnames_unused(&format!(
            "production {case} certified vsock destination"
        ));
    logger.assert_records(
        &[
            "device-kind=vsock operation=transport-reset outcome=succeeded",
            "device-kind=vsock operation=rx outcome=succeeded",
            "device-kind=vsock operation=tx outcome=succeeded",
        ],
        fixture.sensitive_strings(),
    );
    opened
}

trait CertifiedProductionVsockSocketFixture {
    fn certified_socket(&self) -> PathBuf;
}

impl CertifiedProductionVsockSocketFixture for SnapshotVsockSourceGrantFixture {
    fn certified_socket(&self) -> PathBuf {
        self.socket()
    }
}

impl CertifiedProductionVsockSocketFixture for SnapshotVsockContinuationInputGrantFixture {
    fn certified_socket(&self) -> PathBuf {
        self.socket()
    }
}

fn bind_certified_production_vsock_guest_listeners(
    fixture: &impl CertifiedProductionVsockSocketFixture,
    ports: &[u32],
    context: &str,
) -> Vec<(PathBuf, UnixListener)> {
    let socket = fixture.certified_socket();
    ports
        .iter()
        .map(|port| {
            let path = snapshot_vsock_port_path(&socket, *port);
            let listener = UnixListener::bind(&path).unwrap_or_else(|error| {
                panic!("{context} guest-port listener {port} should bind: {error}")
            });
            listener
                .set_nonblocking(true)
                .expect("production certified guest-port listener should be nonblocking");
            (path, listener)
        })
        .collect()
}

struct CertifiedProductionVsockGuestExchange {
    request_kind: &'static str,
    response_kind: &'static str,
    request_size: usize,
    response_size: usize,
    finish_exchange: bool,
}

fn accept_certified_production_vsock_guest_streams(
    listeners: Vec<(PathBuf, UnixListener)>,
    exchange: CertifiedProductionVsockGuestExchange,
    context: &str,
    running: &RunningSerialApiLauncher,
    logger: &Path,
    metrics: &Path,
) -> Vec<UnixStream> {
    let CertifiedProductionVsockGuestExchange {
        request_kind,
        response_kind,
        request_size,
        response_size,
        finish_exchange,
    } = exchange;
    listeners
        .into_iter()
        .enumerate()
        .map(|(index, (path, listener))| {
            let mut stream =
                wait_for_unix_listener_accept(&listener, PROCESS_TIMEOUT).unwrap_or_else(|error| {
                    panic!(
                        "{context} guest stream {request_kind}[{index}] should arrive: {error}; {}",
                        certified_production_vsock_failure_diagnostics(running, logger, metrics)
                    )
                });
            stream
                .set_nonblocking(true)
                .expect("production certified guest stream should be nonblocking");
            let expected = certified_snapshot_vsock_payload(request_kind, index, request_size);
            let mut received = vec![0_u8; expected.len()];
            read_exact_nonblocking(&mut stream, &mut received, PROCESS_TIMEOUT).unwrap_or_else(
                |error| {
                    panic!(
                        "{context} guest stream {request_kind}[{index}] payload should arrive: {error}; {}",
                        certified_production_vsock_failure_diagnostics(running, logger, metrics)
                    )
                },
            );
            // `accept` can win before the launcher finishes its post-connect
            // pathname identity check and SCM_RIGHTS handoff. The exact payload
            // proves that boundary before the test withdraws the pathname.
            drop(listener);
            fs::remove_file(&path)
                .expect("production certified guest listener path should clean after payload");
            assert_eq!(
                received, expected,
                "{context} guest stream {request_kind}[{index}] payload must remain isolated"
            );
            write_all_nonblocking(
                &mut stream,
                &certified_snapshot_vsock_payload(response_kind, index, response_size),
                PROCESS_TIMEOUT,
            )
            .unwrap_or_else(|error| {
                panic!("{context} guest stream {request_kind}[{index}] response should write: {error}")
            });
            if finish_exchange {
                stream
                    .shutdown(std::net::Shutdown::Write)
                    .unwrap_or_else(|error| {
                        panic!(
                            "{context} guest stream {request_kind}[{index}] response should half-close: {error}"
                        )
                    });
                wait_for_stream_eof_nonblocking(&mut stream, PROCESS_TIMEOUT).unwrap_or_else(
                    |error| {
                        panic!(
                            "{context} guest stream {request_kind}[{index}] EOF should arrive: {error}"
                        )
                    },
                );
            }
            stream
        })
        .collect()
}

fn certified_production_vsock_failure_diagnostics(
    running: &RunningSerialApiLauncher,
    logger: &Path,
    metrics: &Path,
) -> String {
    let flush = try_http_request(
        &running.socket,
        "PUT",
        "/actions",
        r#"{"action_type":"FlushMetrics"}"#,
    )
    .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    let logger = fs::read(logger)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter(|line| line.contains("device-kind=vsock"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    let metrics = fs::read_to_string(metrics)
        .ok()
        .and_then(|output| output.lines().last().map(str::to_owned))
        .and_then(|generation| serde_json::from_str::<serde_json::Value>(&generation).ok())
        .map(|generation| {
            serde_json::json!({
                "logger": generation.get("logger"),
                "signals": generation.get("signals"),
                "vsock": generation.get("vsock"),
            })
            .to_string()
        })
        .unwrap_or_else(|| "<unavailable>".to_owned());
    format!(
        "metrics flush response:\n{flush}\nstdout:\n{}\ndevice logger:\n{logger}\nmetrics:\n{metrics}",
        running.stdout_snapshot()
    )
}

fn connect_certified_production_vsock_source_host_streams(
    socket: &Path,
    context: &str,
    running: &RunningSerialApiLauncher,
    logger: &Path,
    metrics: &Path,
) -> (Vec<UnixStream>, Vec<u32>) {
    let mut streams = Vec::with_capacity(1);
    let mut local_ports = Vec::with_capacity(1);
    for index in 0..1 {
        let mut stream = UnixStream::connect(socket)
            .unwrap_or_else(|error| panic!("{context} source host stream should connect: {error}"));
        stream
            .set_nonblocking(true)
            .expect("production source host stream should be nonblocking");
        write_all_nonblocking(
            &mut stream,
            format!(
                "CONNECT {}\n",
                SNAPSHOT_VSOCK_CERTIFY_GUEST_LISTEN_PORT
                    + u32::try_from(index).expect("source host stream index should fit u32")
            )
            .as_bytes(),
            PROCESS_TIMEOUT,
        )
        .expect("production source host CONNECT should write");
        local_ports.push(
            read_certified_production_vsock_connect_ok(&mut stream, PROCESS_TIMEOUT)
                .unwrap_or_else(|error| {
                    panic!(
                        "{context} source CONNECT {index} should succeed: {error}; stdout:\n{}",
                        running.stdout_snapshot()
                    )
                }),
        );
        write_all_nonblocking(
            &mut stream,
            &certified_snapshot_vsock_payload("SOURCE_H2G", index, 1024),
            PROCESS_TIMEOUT,
        )
        .expect("production source host payload should write");
        let expected = certified_snapshot_vsock_payload("SOURCE_H2G_ACK", index, 128);
        let mut received = vec![0_u8; expected.len()];
        read_exact_nonblocking(&mut stream, &mut received, PROCESS_TIMEOUT).unwrap_or_else(
            |error| {
                panic!(
                    "{context} source host acknowledgement {index} should arrive: {error}; host stream state: {}; {}",
                    certified_production_vsock_host_stream_states(std::slice::from_ref(&stream)),
                    certified_production_vsock_failure_diagnostics(running, logger, metrics)
                )
            },
        );
        assert_eq!(
            received, expected,
            "{context} source host acknowledgement should remain isolated"
        );
        streams.push(stream);
    }
    (streams, local_ports)
}

fn queue_certified_production_vsock_host_streams(socket: &Path, context: &str) -> Vec<UnixStream> {
    (0..SNAPSHOT_VSOCK_CERTIFY_HOST_STREAMS)
        .map(|index| {
            let mut stream = UnixStream::connect(socket).unwrap_or_else(|error| {
                panic!("{context} queued host stream {index} should connect: {error}")
            });
            stream
                .set_nonblocking(true)
                .expect("production queued host stream should be nonblocking");
            write_all_nonblocking(
                &mut stream,
                format!(
                    "CONNECT {}\n",
                    SNAPSHOT_VSOCK_CERTIFY_GUEST_LISTEN_PORT
                        + u32::try_from(index).expect("queued host stream index should fit u32")
                )
                .as_bytes(),
                PROCESS_TIMEOUT,
            )
            .expect("production queued host CONNECT should write");
            stream
        })
        .collect()
}

fn assert_certified_production_vsock_streams_blocked_while_paused(
    streams: &mut [UnixStream],
    context: &str,
) {
    for (index, stream) in streams.iter_mut().enumerate() {
        let mut probe = [0_u8; 1];
        match stream.read(&mut probe) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(0) => panic!("{context} queued stream {index} closed while Paused"),
            Ok(read) => {
                panic!("{context} queued stream {index} advanced by {read} byte(s) while Paused")
            }
            Err(error) => {
                panic!("{context} queued stream {index} probe failed while Paused: {error}")
            }
        }
    }
}

fn read_certified_production_vsock_connect_ok(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<u32, String> {
    let response = read_line_nonblocking(stream, 32, timeout)
        .map_err(|error| format!("CONNECT response read failed: {error}"))?;
    let response =
        std::str::from_utf8(&response).map_err(|_| "CONNECT response was not UTF-8".to_owned())?;
    let Some(local_port) = response
        .strip_prefix("OK ")
        .and_then(|value| value.strip_suffix('\n'))
    else {
        return Err("unexpected CONNECT response".to_owned());
    };
    local_port
        .parse::<u32>()
        .map_err(|_| "CONNECT response had an invalid local port".to_owned())
}

fn certified_production_vsock_host_stream_states(streams: &[UnixStream]) -> String {
    streams
        .iter()
        .enumerate()
        .map(|(index, stream)| {
            let mut byte = 0_u8;
            // SAFETY: `byte` is writable for this non-consuming one-byte peek,
            // and the borrowed stream descriptor remains live for the call.
            let result = unsafe {
                libc::recv(
                    stream.as_raw_fd(),
                    (&raw mut byte).cast(),
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                )
            };
            let state = if result > 0 {
                "readable".to_owned()
            } else if result == 0 {
                "eof".to_owned()
            } else {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    "open".to_owned()
                } else {
                    format!("error:{:?}", error.kind())
                }
            };
            format!("{index}:{state}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn certified_snapshot_vsock_payload(kind: &str, index: usize, size: usize) -> Vec<u8> {
    let prefix = format!("BANGBANG_VSOCK_SNAPSHOT_{kind}_{index}:").into_bytes();
    assert!(
        prefix.len() <= size,
        "certified production snapshot-vsock payload prefix must fit"
    );
    let fill = b'A' + u8::try_from(index % 26).expect("production payload fill should fit u8");
    let mut payload = prefix;
    payload.resize(size, fill);
    payload
}

const CERTIFIED_VSOCK_METRIC_FIELDS: [&str; 20] = [
    "activate_fails",
    "cfg_fails",
    "conn_event_fails",
    "conns_added",
    "conns_killed",
    "conns_removed",
    "ev_queue_event_fails",
    "killq_resync",
    "muxer_event_fails",
    "rx_bytes_count",
    "rx_packets_count",
    "rx_queue_event_count",
    "rx_queue_event_fails",
    "rx_read_fails",
    "tx_bytes_count",
    "tx_flush_fails",
    "tx_packets_count",
    "tx_queue_event_count",
    "tx_queue_event_fails",
    "tx_write_fails",
];

fn assert_certified_vsock_metrics_shape(vsock: &serde_json::Value, context: &str) {
    let object = vsock
        .as_object()
        .expect("production certified vsock metrics should be an object");
    assert_eq!(
        object.len(),
        CERTIFIED_VSOCK_METRIC_FIELDS.len(),
        "{context} metrics must contain exactly the pinned vsock fields: {vsock}"
    );
    for field in CERTIFIED_VSOCK_METRIC_FIELDS {
        assert!(
            object
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .is_some(),
            "{context} metrics must contain numeric vsock field {field}: {vsock}"
        );
    }
}

fn certified_vsock_metric(vsock: &serde_json::Value, field: &str) -> u64 {
    vsock
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .expect("certified vsock metric should be an unsigned integer")
}

fn latest_certified_vsock_metrics(metrics: &Path) -> (serde_json::Value, serde_json::Value) {
    let output =
        fs::read_to_string(metrics).expect("production certified vsock metrics should read");
    let generation: serde_json::Value = serde_json::from_str(
        output
            .lines()
            .last()
            .expect("production certified vsock metrics should contain a generation"),
    )
    .expect("production certified vsock metrics generation should be JSON");
    let vsock = generation
        .get("vsock")
        .cloned()
        .expect("production certified metrics should contain a vsock object");
    (generation, vsock)
}

fn flush_certified_production_vsock_metrics(
    socket: &Path,
    metrics: &Path,
    expected_connections: u64,
    context: &str,
) {
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
        204,
        &format!("flush {context} certified snapshot-vsock metrics"),
    );
    let (generation, vsock) = latest_certified_vsock_metrics(metrics);
    assert_certified_vsock_metrics_shape(&vsock, context);
    assert!(
        certified_vsock_metric(&vsock, "conns_added") >= expected_connections,
        "{context} metrics must record at least {expected_connections} fresh connections: {generation}"
    );
    for field in [
        "rx_bytes_count",
        "rx_packets_count",
        "tx_bytes_count",
        "tx_packets_count",
        "tx_queue_event_count",
    ] {
        assert!(
            certified_vsock_metric(&vsock, field) > 0,
            "{context} metrics must record {field}: {generation}"
        );
    }
    // A restored queue can retain guest-posted RX buffers and deliver host work
    // without a fresh guest notification. The exact field is still pinned by
    // the object-shape and focused source-admission tests above.
    assert!(
        certified_vsock_metric(&vsock, "conns_removed")
            <= certified_vsock_metric(&vsock, "conns_added"),
        "{context} removals must not exceed successful additions: {generation}"
    );
    for field in [
        "activate_fails",
        "cfg_fails",
        "conn_event_fails",
        "conns_killed",
        "ev_queue_event_fails",
        "killq_resync",
        "muxer_event_fails",
        "rx_queue_event_fails",
        "rx_read_fails",
        "tx_flush_fails",
        "tx_queue_event_fails",
        "tx_write_fails",
    ] {
        assert_eq!(
            certified_vsock_metric(&vsock, field),
            0,
            "{context} successful traffic must leave {field} at zero: {generation}"
        );
    }

    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
        204,
        &format!("flush {context} zero-event snapshot-vsock metrics interval"),
    );
    let (zero_generation, zero_vsock) = latest_certified_vsock_metrics(metrics);
    assert_certified_vsock_metrics_shape(&zero_vsock, context);
    for field in CERTIFIED_VSOCK_METRIC_FIELDS {
        assert_eq!(
            certified_vsock_metric(&zero_vsock, field),
            0,
            "{context} immediate zero-event interval must reset {field}: {zero_generation}"
        );
    }
}

fn stop_certified_production_vsock_launcher(
    running: &mut RunningSerialApiLauncher,
    sensitive: &[String],
    context: &str,
) -> (String, String) {
    let pid = i32::try_from(running.child.id()).expect("production launcher PID should fit");
    // SAFETY: `pid` is the live unreaped launcher owned by `running`.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let (status, stdout, stderr) = running.wait(context);
    assert!(
        status.success(),
        "{context} should stop cleanly: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_serial_snapshot_output_redacted(&stdout, &stderr, sensitive, context);
    assert!(
        !running.socket.exists(),
        "{context} should remove the granted API socket"
    );
    (stdout, stderr)
}

#[test]
fn normal_bundle_certifies_native_v2_network_mmds_snapshot_continuation_and_containment() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    for enable_pci in [false, true] {
        for mmds_v2 in [false, true] {
            run_production_network_mmds_snapshot_continuation(
                &bundle,
                enable_pci,
                mmds_v2,
                &baseline_sessions,
            );
        }
    }
    assert_eq!(
        session_entries(),
        baseline_sessions,
        "network/MMDS snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_production_network_mmds_snapshot_continuation(
    bundle: &Path,
    enable_pci: bool,
    mmds_v2: bool,
    baseline_sessions: &[PathBuf],
) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let version = if mmds_v2 { "v2" } else { "v1" };
    let case = format!("{transport}-{version}");
    let source_fixture = SnapshotSourceGrantFixture::new(&format!("{case}-network-mmds-source"));
    reset_zeroed_file(
        &source_fixture.data_backing,
        SNAPSHOT_NETWORK_MMDS_SECTORS * VIRTIO_BLOCK_SECTOR_BYTES,
    );
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        source_fixture.sensitive_strings(),
        &format!("network-mmds-snapshot-{case}-source"),
        false,
        enable_pci,
    );
    source_fixture.replace_source_file_pathnames();
    configure_and_start_network_mmds_snapshot_source(&source.socket, enable_pci, mmds_v2, &case);
    wait_for_network_mmds_snapshot_markers(
        &source_fixture.opened_data_backing,
        &[(0, SNAPSHOT_NETWORK_MMDS_READY_MARKER)],
        &format!("production {case} network/MMDS source readiness"),
    );
    flush_production_metrics(
        &source.socket,
        &format!("{case} network/MMDS source traffic"),
    );
    assert_production_network_metrics(&source_fixture.opened_metrics, "eth0", &case);
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production {case} network/MMDS source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production {case} network/MMDS snapshot"),
    );
    let artifacts = source_fixture.artifacts();
    let source_network = assert_production_network_mmds_snapshot(
        &artifacts.state,
        enable_pci,
        mmds_v2,
        "vmnet:bridged:source-private",
        &format!("{case} source"),
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let state_before =
        fs::read(&artifacts.state).expect("production network/MMDS source state should read");
    let memory_before =
        fs::read(&artifacts.memory).expect("production network/MMDS source memory should read");
    stop_running_launcher(
        &mut source,
        &format!("production {case} network/MMDS snapshot source"),
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {case} network/MMDS snapshot source"),
    );
    source_fixture.assert_replacement_pathnames_unused(&format!(
        "production {case} network/MMDS snapshot source"
    ));

    let mut current = artifacts;
    if !enable_pci && mmds_v2 {
        current =
            run_production_network_snapshot_override_rejections(bundle, current, baseline_sessions);
        for malformed in [
            NetworkSnapshotMalformedInput::StateChecksum,
            NetworkSnapshotMalformedInput::TruncatedMemory,
        ] {
            run_production_network_mmds_malformed_case(
                bundle,
                &current,
                malformed,
                baseline_sessions,
            );
        }
        for shutdown in [
            SnapshotContinuationShutdown::GracefulCancellation,
            SnapshotContinuationShutdown::WorkerFirst,
            SnapshotContinuationShutdown::LauncherFirst,
        ] {
            current = run_production_network_mmds_paused_shutdown_case(
                bundle,
                current,
                shutdown,
                baseline_sessions,
            );
        }
    }

    let explicit_case = format!("{transport}-{version}-net-exp");
    current = run_production_network_mmds_snapshot_destination(ProductionNetworkMmdsDestination {
        bundle,
        artifacts: current,
        source_network: &source_network,
        enable_pci,
        mmds_v2,
        resume_vm: false,
        recapture: true,
        selector: "vmnet:host",
        case: &explicit_case,
        baseline_sessions,
    });
    let automatic_case = format!("{transport}-{version}-net-auto");
    let final_artifacts =
        run_production_network_mmds_snapshot_destination(ProductionNetworkMmdsDestination {
            bundle,
            artifacts: current,
            source_network: &source_network,
            enable_pci,
            mmds_v2,
            resume_vm: true,
            recapture: false,
            selector: "vmnet:shared",
            case: &automatic_case,
            baseline_sessions,
        });
    assert_eq!(
        fs::read(&final_artifacts.state).expect("final network/MMDS state should read"),
        state_before,
        "{case} contained repeated loads must not mutate state"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final network/MMDS memory should read"),
        memory_before,
        "{case} contained repeated loads must not mutate memory"
    );
}

fn configure_and_start_network_mmds_snapshot_source(
    socket: &Path,
    enable_pci: bool,
    mmds_v2: bool,
    context: &str,
) {
    for (path, body, request) in [
        (
            "/machine-config",
            serde_json::json!({
                "vcpu_count": 1,
                "mem_size_mib": 256,
            }),
            "machine config",
        ),
        (
            "/network-interfaces/eth0",
            serde_json::json!({
                "iface_id": "eth0",
                "host_dev_name": "vmnet:bridged:source-private",
                "guest_mac": "06:00:00:00:00:71",
                "mtu": 1280,
                "rx_rate_limiter": {
                    "bandwidth": {
                        "size": 1_048_576,
                        "one_time_burst": 65_536,
                        "refill_time": 1_000,
                    },
                    "ops": {
                        "size": 1_000,
                        "one_time_burst": 100,
                        "refill_time": 1_000,
                    },
                },
                "tx_rate_limiter": {
                    "bandwidth": {
                        "size": 2_097_152,
                        "one_time_burst": 131_072,
                        "refill_time": 2_000,
                    },
                    "ops": {
                        "size": 2_000,
                        "one_time_burst": 200,
                        "refill_time": 2_000,
                    },
                },
            }),
            "network interface",
        ),
        (
            "/mmds/config",
            serde_json::json!({
                "network_interfaces": ["eth0"],
                "version": if mmds_v2 { "V2" } else { "V1" },
                "ipv4_address": "169.254.169.254",
                "imds_compat": mmds_v2,
            }),
            "MMDS config",
        ),
        (
            "/mmds",
            serde_json::from_str(SNAPSHOT_NETWORK_MMDS_SOURCE_CONTENT)
                .expect("network/MMDS source data should be valid JSON"),
            "source MMDS data",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "metrics",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production network/MMDS snapshot request should serialize"),
            ),
            204,
            &format!("PUT production {context} network/MMDS snapshot {request}"),
        );
    }

    let mut boot_args = SNAPSHOT_NETWORK_MMDS_BOOT_ARGS.to_owned();
    if mmds_v2 {
        boot_args.push_str(" bangbang.mmds-snapshot-v2=1");
    }
    if enable_pci {
        boot_args.push_str(" bangbang.expect-pci-data=1");
    }
    for (path, body, request) in [
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "boot_args": boot_args,
            }),
            "boot source",
        ),
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": false,
            }),
            "root drive",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": SNAPSHOT_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Sync",
            }),
            "control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production network/MMDS boot request should serialize"),
            ),
            204,
            &format!("PUT production {context} network/MMDS snapshot {request}"),
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        &format!("start production {context} network/MMDS snapshot source"),
    );
}

fn assert_production_network_mmds_snapshot(
    state_path: &Path,
    enable_pci: bool,
    mmds_v2: bool,
    selector: &str,
    context: &str,
) -> SnapshotV2NetworkState {
    let bytes = fs::read(state_path).unwrap_or_else(|error| {
        panic!(
            "production {context} network/MMDS state {} should read: {error}",
            state_path.display()
        )
    });
    let structural =
        decode_snapshot_v2_state(&bytes).expect("production network/MMDS state should decode");
    let state = decode_hvf_snapshot_v2_vsock_state(&structural)
        .expect("production network/MMDS state should be exact native-v2 2.12");
    let graph = state
        .device_graph()
        .expect("production network/MMDS artifact should retain root and data");
    assert_eq!(
        graph.block_records().len(),
        2,
        "production {context} should retain root and data drives"
    );
    let expected_transport = if enable_pci {
        SnapshotV2DeviceTransportKind::Pci
    } else {
        SnapshotV2DeviceTransportKind::Mmio
    };
    assert_eq!(graph.transport_kind(), expected_transport);
    let network = state
        .network()
        .expect("production certification artifact should contain kind 12");
    assert_eq!(network.interfaces().len(), 1);
    let interface = &network.interfaces()[0];
    assert_eq!(interface.iface_id(), "eth0");
    assert_eq!(interface.captured_selector(), selector);
    assert_eq!(
        interface
            .requested_guest_mac()
            .expect("production network snapshot should retain requested MAC")
            .octets(),
        [0x06, 0, 0, 0, 0, 0x71]
    );
    assert_eq!(interface.requested_mtu(), Some(1280));
    assert_eq!(
        interface
            .profile()
            .guest_mac()
            .expect("production network snapshot should retain realized MAC")
            .octets(),
        [0x06, 0, 0, 0, 0, 0x71]
    );
    assert_eq!(interface.profile().mtu(), Some(1280));
    assert_eq!(interface.backend(), SnapshotV2NetworkBackendClass::MmdsOnly);
    assert!(interface.local().active_rx_queue().is_some());
    assert!(interface.local().active_tx_queue().is_some());
    assert_eq!(interface.virtio().queues().len(), 2);
    assert!(interface.rx_limiter().is_configured());
    assert!(interface.tx_limiter().is_configured());
    assert_eq!(
        interface
            .rx_limiter()
            .bandwidth()
            .expect("production RX bandwidth bucket should persist")
            .size(),
        1_048_576
    );
    assert_eq!(
        interface
            .tx_limiter()
            .ops()
            .expect("production TX operations bucket should persist")
            .size(),
        2_000
    );
    assert_eq!(interface.transport().kind(), expected_transport);
    let mmds = network
        .mmds()
        .expect("production network snapshot should contain MMDS config");
    assert_eq!(
        mmds.version(),
        if mmds_v2 {
            MmdsVersion::V2
        } else {
            MmdsVersion::V1
        }
    );
    assert_eq!(mmds.effective_ipv4_address().to_string(), "169.254.169.254");
    assert_eq!(mmds.imds_compat(), mmds_v2);
    assert_eq!(mmds.interfaces().len(), 1);
    assert_eq!(mmds.interfaces()[0].interface_index(), 0);
    network.clone()
}

fn assert_normalized_production_network_mmds_recapture(
    source: &SnapshotV2NetworkState,
    recaptured: &SnapshotV2NetworkState,
    context: &str,
) {
    assert_eq!(source.interfaces().len(), recaptured.interfaces().len());
    let source_interface = &source.interfaces()[0];
    let recaptured_interface = &recaptured.interfaces()[0];
    assert_eq!(source_interface.iface_id(), recaptured_interface.iface_id());
    assert_eq!(
        source_interface.requested_guest_mac(),
        recaptured_interface.requested_guest_mac(),
        "production {context} requested MAC should remain normalized"
    );
    assert_eq!(
        source_interface.requested_mtu(),
        recaptured_interface.requested_mtu(),
        "production {context} requested MTU should remain normalized"
    );
    assert_eq!(
        source_interface.profile(),
        recaptured_interface.profile(),
        "production {context} realized profile should remain normalized"
    );
    assert_eq!(
        source_interface.backend(),
        recaptured_interface.backend(),
        "production {context} backend class should remain normalized"
    );
    assert_eq!(
        source_interface.local(),
        recaptured_interface.local(),
        "production {context} local queue state should remain normalized"
    );
    assert_eq!(
        source_interface.virtio(),
        recaptured_interface.virtio(),
        "production {context} common virtio state should remain normalized"
    );
    assert_eq!(
        production_network_limiter_config(source_interface.rx_limiter()),
        production_network_limiter_config(recaptured_interface.rx_limiter()),
        "production {context} RX limiter configuration should remain normalized"
    );
    assert_eq!(
        production_network_limiter_config(source_interface.tx_limiter()),
        production_network_limiter_config(recaptured_interface.tx_limiter()),
        "production {context} TX limiter configuration should remain normalized"
    );
    assert_eq!(
        source_interface.transport(),
        recaptured_interface.transport(),
        "production {context} transport placement should remain normalized"
    );
    assert_eq!(
        source.mmds(),
        recaptured.mmds(),
        "production {context} MMDS identity should remain normalized"
    );
}

type ProductionNetworkLimiterConfig = (
    Option<(u64, Option<u64>, u64)>,
    Option<(u64, Option<u64>, u64)>,
);

fn production_network_limiter_config(
    limiter: SnapshotV2NetworkLimiterState,
) -> ProductionNetworkLimiterConfig {
    let bucket =
        |bucket: bangbang_runtime::snapshot_network_v2_11::SnapshotV2NetworkTokenBucketState| {
            (
                bucket.size(),
                bucket.configured_burst(),
                bucket.refill_time_millis(),
            )
        };
    (limiter.bandwidth().map(bucket), limiter.ops().map(bucket))
}

fn run_production_network_snapshot_override_rejections(
    bundle: &Path,
    artifacts: SnapshotArtifactSet,
    baseline_sessions: &[PathBuf],
) -> SnapshotArtifactSet {
    const DUPLICATE_SELECTOR: &str = "vmnet:bridged:private-duplicate-selector";
    const UNKNOWN_SELECTOR: &str = "vmnet:bridged:private-unknown-selector";
    const CAPTURED_SELECTOR: &str = "vmnet:bridged:source-private";

    let requests = [
        ("missing", snapshot_load_body(false), Vec::<&str>::new()),
        (
            "duplicate",
            network_snapshot_load_body(
                false,
                &[("eth0", DUPLICATE_SELECTOR), ("eth0", "vmnet:shared")],
            ),
            vec![DUPLICATE_SELECTOR],
        ),
        (
            "unknown",
            network_snapshot_load_body(false, &[("unknown", UNKNOWN_SELECTOR)]),
            vec![UNKNOWN_SELECTOR],
        ),
    ];
    let mut current = artifacts;
    for (name, body, private_selectors) in requests {
        let case = format!("net-ov-{name}");
        let fixture = SnapshotContinuationInputGrantFixture::new(&case, current, false);
        let mut sensitive = fixture.sensitive_strings();
        sensitive.extend(
            [DUPLICATE_SELECTOR, UNKNOWN_SELECTOR, CAPTURED_SELECTOR]
                .into_iter()
                .map(str::to_owned),
        );
        let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
            bundle,
            &fixture.manifest,
            &fixture.api_socket(),
            sensitive.clone(),
            &case,
            false,
        );
        let opened = fixture.replace_source_pathnames();
        let state_before =
            fs::read(&opened.state).expect("override-rejection network state should read");
        let memory_before =
            fs::read(&opened.memory).expect("override-rejection network memory should read");
        configure_network_mmds_snapshot_destination_metrics(&running.socket, &case);
        assert!(
            fs::read(&fixture.opened_metrics)
                .expect("override-rejection network metrics should read")
                .is_empty(),
            "network {name} override rejection metrics should start empty"
        );
        let response = http_put(&running.socket, "/snapshot/load", &body);
        assert_http_status(
            &response,
            400,
            &format!("reject production network snapshot {name} override set"),
        );
        for private in sensitive
            .iter()
            .map(String::as_str)
            .chain(private_selectors)
        {
            assert!(
                !response.contains(private),
                "production network {name} override fault must redact {private:?}"
            );
        }
        assert!(
            fs::read(&fixture.opened_metrics)
                .expect("rejected network metrics should read")
                .is_empty(),
            "network {name} override rejection must not publish destination metrics"
        );
        thread::sleep(Duration::from_millis(100));
        if running
            .child
            .try_wait()
            .expect("network override rejection launcher status should read")
            .is_some()
            || !running.socket.exists()
        {
            let status = running.wait(&format!(
                "terminal production network {name} override rejection"
            ));
            assert!(
                !status.success(),
                "terminal network {name} override rejection should fail closed"
            );
            assert!(
                !running.socket.exists(),
                "terminal network {name} override rejection should remove its API socket"
            );
        } else {
            assert!(
                http_get(&running.socket, "/").contains(r#""state":"Not started""#),
                "production network {name} override rejection must not publish a VM"
            );
            stop_running_launcher(
                &mut running,
                &format!("production network {name} override rejection destination"),
            );
        }
        assert_session_entries_eventually_restored(
            baseline_sessions,
            &format!("production network {name} override rejection destination"),
        );
        assert_eq!(
            fs::read(&opened.state).expect("rejected network state should remain"),
            state_before,
            "network {name} override rejection must preserve immutable state"
        );
        assert_eq!(
            fs::read(&opened.memory).expect("rejected network memory should remain"),
            memory_before,
            "network {name} override rejection must preserve immutable memory"
        );
        fixture.assert_replacement_pathnames_unused(&format!(
            "production network {name} override rejection destination"
        ));
        current = opened;
    }
    current
}

#[derive(Debug, Clone, Copy)]
enum NetworkSnapshotMalformedInput {
    StateChecksum,
    TruncatedMemory,
}

fn run_production_network_mmds_malformed_case(
    bundle: &Path,
    artifacts: &SnapshotArtifactSet,
    malformed_input: NetworkSnapshotMalformedInput,
    baseline_sessions: &[PathBuf],
) {
    let case = match malformed_input {
        NetworkSnapshotMalformedInput::StateChecksum => "network-mmds-malformed-state",
        NetworkSnapshotMalformedInput::TruncatedMemory => "network-mmds-truncated-memory",
    };
    let original_state = fs::read(&artifacts.state).expect("valid network/MMDS state should read");
    let original_memory =
        fs::read(&artifacts.memory).expect("valid network/MMDS memory should read");
    let malformed_root = TestDir::new(case);
    let canonical_root = fs::canonicalize(malformed_root.path())
        .expect("malformed network/MMDS fixture root should canonicalize");
    let malformed = SnapshotArtifactSet {
        state: canonical_root.join("malformed-state.snap"),
        memory: canonical_root.join("malformed-memory.snap"),
        root: canonical_root.join("malformed-root.img"),
        data: canonical_root.join("malformed-data.img"),
        audit: canonical_root.join("malformed-audit.img"),
    };
    fs::copy(&artifacts.state, &malformed.state)
        .expect("malformed network/MMDS state fixture should copy");
    fs::copy(&artifacts.memory, &malformed.memory)
        .expect("malformed network/MMDS memory fixture should copy");
    for (source, destination, context) in [
        (
            &artifacts.root,
            &malformed.root,
            "malformed network/MMDS root",
        ),
        (
            &artifacts.data,
            &malformed.data,
            "malformed network/MMDS data",
        ),
        (
            &artifacts.audit,
            &malformed.audit,
            "malformed network/MMDS audit",
        ),
    ] {
        hard_link_or_copy_fixture(source, destination, context);
    }
    match malformed_input {
        NetworkSnapshotMalformedInput::StateChecksum => {
            let mut malformed_bytes =
                fs::read(&malformed.state).expect("malformed network state should read");
            let last = malformed_bytes
                .len()
                .checked_sub(1)
                .expect("native-v2 network state must be nonempty");
            malformed_bytes[last] ^= 0x80;
            fs::write(&malformed.state, malformed_bytes)
                .expect("malformed network checksum fixture should write");
        }
        NetworkSnapshotMalformedInput::TruncatedMemory => {
            let len = fs::metadata(&malformed.memory)
                .expect("malformed network memory metadata should read")
                .len();
            let truncated = len
                .checked_sub(4096)
                .expect("native-v2 network memory should exceed one page");
            OpenOptions::new()
                .write(true)
                .open(&malformed.memory)
                .expect("malformed network memory should reopen")
                .set_len(truncated)
                .expect("malformed network memory should truncate");
        }
    }

    let fixture = SnapshotContinuationInputGrantFixture::new(case, malformed, false);
    let sensitive = fixture.sensitive_strings();
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        sensitive.clone(),
        case,
        false,
    );
    fixture.replace_source_pathnames();
    configure_network_mmds_snapshot_destination_metrics(&running.socket, case);
    let response = http_put(
        &running.socket,
        "/snapshot/load",
        &network_snapshot_load_body(false, &[("eth0", "vmnet:host")]),
    );
    assert_http_status(
        &response,
        400,
        &format!("reject production {case} network/MMDS snapshot"),
    );
    for private in &sensitive {
        assert!(
            !response.contains(private),
            "production {case} restore fault must redact private grant data"
        );
    }
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("malformed network/MMDS metrics should read")
            .is_empty(),
        "production {case} restore must not publish metrics"
    );
    thread::sleep(Duration::from_millis(100));
    if running
        .child
        .try_wait()
        .expect("malformed network/MMDS launcher status should read")
        .is_some()
        || !running.socket.exists()
    {
        let status = running.wait(&format!("terminal production {case} destination"));
        assert!(
            !status.success(),
            "terminal production {case} rejection should fail closed"
        );
        assert!(
            !running.socket.exists(),
            "terminal production {case} rejection should remove its API socket"
        );
    } else {
        assert!(
            http_get(&running.socket, "/").contains(r#""state":"Not started""#),
            "production {case} restore must not publish a VM"
        );
        stop_running_launcher(&mut running, &format!("production {case} destination"));
    }
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {case} malformed destination"),
    );
    assert_eq!(
        fs::read(&artifacts.state).expect("valid network state should survive malformed load"),
        original_state
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("valid network memory should survive malformed load"),
        original_memory
    );
    fixture.assert_replacement_pathnames_unused(&format!(
        "production {case} malformed network/MMDS destination"
    ));
}

fn run_production_network_mmds_paused_shutdown_case(
    bundle: &Path,
    artifacts: SnapshotArtifactSet,
    shutdown: SnapshotContinuationShutdown,
    baseline_sessions: &[PathBuf],
) -> SnapshotArtifactSet {
    let name = match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => "net-cancel",
        SnapshotContinuationShutdown::WorkerFirst => "net-worker",
        SnapshotContinuationShutdown::LauncherFirst => "net-launcher",
    };
    let fixture = SnapshotContinuationInputGrantFixture::new(name, artifacts, false);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("network-mmds-snapshot-{name}"),
        false,
    );
    let opened = fixture.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("shutdown-case network/MMDS state should read");
    let memory_before =
        fs::read(&opened.memory).expect("shutdown-case network/MMDS memory should read");
    reset_zeroed_file(
        &opened.data,
        SNAPSHOT_NETWORK_MMDS_SECTORS * VIRTIO_BLOCK_SECTOR_BYTES,
    );
    configure_network_mmds_snapshot_destination_metrics(&running.socket, name);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("shutdown-case network/MMDS metrics should read")
            .is_empty(),
        "fresh production {name} metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &network_snapshot_load_body(false, &[("eth0", "vmnet:host")]),
        ),
        204,
        &format!("load production network/MMDS snapshot before {name}"),
    );
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Paused""#),
        "production network/MMDS destination should remain Paused before {name}"
    );
    assert_production_network_mmds_config(&running.socket, "vmnet:host", true, name);
    assert_eq!(session_entries().len(), baseline_sessions.len() + 1);

    let status = match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => {
            let launcher =
                i32::try_from(running.child.id()).expect("network/MMDS launcher PID should fit");
            // SAFETY: The unreaped launcher owns this exact PID.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
            running.wait("Paused network/MMDS restoration cancellation")
        }
        SnapshotContinuationShutdown::WorkerFirst => {
            let worker = only_worker_pid(&running.child);
            // SAFETY: The worker is the sole live child of the unreaped launcher.
            assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
            running.wait("Paused network/MMDS worker-first death")
        }
        SnapshotContinuationShutdown::LauncherFirst => {
            let worker = only_worker_pid(&running.child);
            let worker_exit = ProcessExitWatch::new(worker);
            let launcher =
                i32::try_from(running.child.id()).expect("network/MMDS launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID and its worker
            // independently observes authenticated lifecycle EOF.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
            let result = running.wait("Paused network/MMDS launcher-first death");
            assert!(
                worker_exit.wait(PROCESS_TIMEOUT),
                "network/MMDS worker should observe launcher death"
            );
            result
        }
    };
    match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => {
            assert!(
                status.success(),
                "network/MMDS cancellation should be graceful"
            );
        }
        SnapshotContinuationShutdown::WorkerFirst => {
            assert_eq!(status.code(), Some(128 + libc::SIGKILL));
        }
        SnapshotContinuationShutdown::LauncherFirst => {
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
    }
    assert!(
        !running.socket.exists(),
        "production {name} destination should remove its API socket"
    );
    assert_session_entries_eventually_restored(baseline_sessions, name);
    assert_eq!(
        fs::read(&opened.state).expect("shutdown-case network state should remain"),
        state_before,
        "production {name} must preserve immutable state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("shutdown-case network memory should remain"),
        memory_before,
        "production {name} must preserve immutable memory"
    );
    fixture.assert_replacement_pathnames_unused(&format!(
        "production {name} network/MMDS shutdown destination"
    ));
    opened
}

struct ProductionNetworkMmdsDestination<'a> {
    bundle: &'a Path,
    artifacts: SnapshotArtifactSet,
    source_network: &'a SnapshotV2NetworkState,
    enable_pci: bool,
    mmds_v2: bool,
    resume_vm: bool,
    recapture: bool,
    selector: &'a str,
    case: &'a str,
    baseline_sessions: &'a [PathBuf],
}

fn run_production_network_mmds_snapshot_destination(
    destination: ProductionNetworkMmdsDestination<'_>,
) -> SnapshotArtifactSet {
    let ProductionNetworkMmdsDestination {
        bundle,
        artifacts,
        source_network,
        enable_pci,
        mmds_v2,
        resume_vm,
        recapture,
        selector,
        case,
        baseline_sessions,
    } = destination;
    let fixture = SnapshotContinuationInputGrantFixture::new(case, artifacts, recapture);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("network-mmds-snapshot-{case}"),
        enable_pci,
    );
    let opened = fixture.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("destination network/MMDS state should read before load");
    let memory_before =
        fs::read(&opened.memory).expect("destination network/MMDS memory should read before load");
    reset_zeroed_file(
        &opened.data,
        SNAPSHOT_NETWORK_MMDS_SECTORS * VIRTIO_BLOCK_SECTOR_BYTES,
    );
    configure_network_mmds_snapshot_destination_metrics(&running.socket, case);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("destination network/MMDS metrics should read")
            .is_empty(),
        "production {case} destination metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &network_snapshot_load_body(resume_vm, &[("eth0", selector)]),
        ),
        204,
        &format!("load production {case} network/MMDS snapshot"),
    );
    assert!(
        http_get(&running.socket, "/").contains(if resume_vm {
            r#""state":"Running""#
        } else {
            r#""state":"Paused""#
        }),
        "production {case} destination should publish the requested resume state"
    );
    assert_production_network_mmds_config(&running.socket, selector, mmds_v2, case);
    let empty_mmds = http_get(&running.socket, "/mmds");
    assert_http_status(
        &empty_mmds,
        200,
        &format!("GET production {case} empty restored MMDS"),
    );
    assert!(
        empty_mmds.ends_with("\r\n\r\nnull"),
        "production {case} empty restored MMDS should return exact JSON null; response:\n{empty_mmds}"
    );
    assert!(
        !empty_mmds.contains("BANGBANG_MMDS_SNAPSHOT_SOURCE")
            && !empty_mmds.contains("BANGBANG_MMDS_SNAPSHOT_DESTINATION"),
        "production {case} restored MMDS must exclude source and future destination data"
    );

    if recapture {
        assert_http_status(
            &http_put(&running.socket, "/snapshot/create", &snapshot_create_body()),
            204,
            &format!("recapture production {case} network/MMDS snapshot"),
        );
        let recaptured = fixture.recaptured_artifacts();
        let recaptured_network = assert_production_network_mmds_snapshot(
            &recaptured.state,
            enable_pci,
            mmds_v2,
            selector,
            &format!("{case} recapture"),
        );
        assert_normalized_production_network_mmds_recapture(
            source_network,
            &recaptured_network,
            case,
        );
        fixture.assert_no_recapture_staging();
    }

    assert_http_status(
        &http_put(
            &running.socket,
            "/mmds",
            SNAPSHOT_NETWORK_MMDS_DESTINATION_CONTENT,
        ),
        204,
        &format!("PUT production {case} destination MMDS"),
    );
    resize_and_write_file_marker_at(
        &opened.data,
        SNAPSHOT_NETWORK_MMDS_SECTORS * VIRTIO_BLOCK_SECTOR_BYTES,
        SNAPSHOT_NETWORK_MMDS_CONTINUE_OFFSET,
        SNAPSHOT_NETWORK_MMDS_CONTINUE_MARKER,
    );
    if !resume_vm {
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            204,
            &format!("resume production {case} network/MMDS destination"),
        );
    }
    let token_marker = if mmds_v2 {
        SNAPSHOT_NETWORK_MMDS_TOKEN_REJECTED_MARKER
    } else {
        SNAPSHOT_NETWORK_MMDS_V1_MARKER
    };
    wait_for_network_mmds_snapshot_markers(
        &opened.data,
        &[
            (0, SNAPSHOT_NETWORK_MMDS_SUCCESS_MARKER),
            (
                SNAPSHOT_NETWORK_MMDS_CONNECTION_OFFSET,
                SNAPSHOT_NETWORK_MMDS_CONNECTION_LOST_MARKER,
            ),
            (SNAPSHOT_NETWORK_MMDS_TOKEN_RESULT_OFFSET, token_marker),
            (
                SNAPSHOT_NETWORK_MMDS_FRESH_OFFSET,
                SNAPSHOT_NETWORK_MMDS_FRESH_MARKER,
            ),
        ],
        &format!("production {case} restored MMDS guest"),
    );
    let status = running.wait(&format!("production {case} restored MMDS guest poweroff"));
    assert!(
        status.success(),
        "production {case} restored MMDS guest should reach SYSTEM_OFF: {status:?}"
    );
    assert!(
        !running.socket.exists(),
        "production {case} guest poweroff should remove its API socket"
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {case} restored network/MMDS destination"),
    );
    assert_production_network_metrics(&fixture.opened_metrics, "eth0", case);
    assert_eq!(
        fs::read(&opened.state).expect("destination network state should remain"),
        state_before,
        "production {case} load must not mutate state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("destination network memory should remain"),
        memory_before,
        "production {case} load must not mutate memory"
    );
    fixture.assert_replacement_pathnames_unused(&format!(
        "production {case} restored network/MMDS destination"
    ));
    opened
}

fn configure_network_mmds_snapshot_destination_metrics(socket: &Path, context: &str) {
    assert_http_status(
        &http_put(
            socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        &format!("PUT production {context} network/MMDS destination metrics"),
    );
}

fn assert_production_network_mmds_config(
    socket: &Path,
    selector: &str,
    mmds_v2: bool,
    context: &str,
) {
    let config = http_get(socket, "/vm/config");
    assert_http_status(
        &config,
        200,
        &format!("read production {context} restored VM config"),
    );
    for expected in [
        r#""iface_id":"eth0""#,
        &format!(r#""host_dev_name":"{selector}""#),
        r#""guest_mac":"06:00:00:00:00:71""#,
        r#""mtu":1280"#,
        if mmds_v2 {
            r#""version":"V2""#
        } else {
            r#""version":"V1""#
        },
    ] {
        assert!(
            config.contains(expected),
            "production {context} restored config should contain {expected}; response:\n{config}"
        );
    }
    assert_eq!(
        config.matches(r#""drive_id":"#).count(),
        2,
        "production {context} restored config should retain root and control drives"
    );
}

fn assert_production_network_metrics(path: &Path, iface_id: &str, context: &str) {
    let output = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "production {context} network metrics {} should read: {error}",
            path.display()
        )
    });
    let latest_line = output
        .lines()
        .rev()
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| {
            panic!("production {context} network metrics should contain JSON: {output}")
        });
    let latest: serde_json::Value = serde_json::from_str(latest_line).unwrap_or_else(|error| {
        panic!(
            "production {context} network metrics should be valid JSON: {error}; line:\n{latest_line}"
        )
    });
    let key = format!("net_{iface_id}");
    let metrics = latest.get(&key).unwrap_or_else(|| {
        panic!("production {context} metrics should include {key}; line:\n{latest_line}")
    });
    for field in ["rx_count", "rx_packets_count", "rx_bytes_count"] {
        assert!(
            metrics
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|value| value > 0),
            "production {context} metrics should report nonzero {key}.{field}; line:\n{latest_line}"
        );
    }
    assert_eq!(metrics["rx_count"], metrics["rx_packets_count"]);
    for field in [
        "tx_count",
        "tx_packets_count",
        "tx_bytes_count",
        "tx_spoofed_mac_count",
        "rx_fails",
        "tx_fails",
        "tx_malformed_frames",
    ] {
        assert_eq!(
            metrics.get(field).and_then(serde_json::Value::as_u64),
            Some(0),
            "production {context} metrics should report zero {key}.{field}; line:\n{latest_line}"
        );
    }
    assert_eq!(
        metrics
            .get("event_fails")
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "production {context} metrics should report no {key} event failures; line:\n{latest_line}"
    );
    let mmds = latest.get("mmds").unwrap_or_else(|| {
        panic!("production {context} metrics should include MMDS activity; line:\n{latest_line}")
    });
    for field in [
        "rx_accepted",
        "rx_count",
        "tx_count",
        "tx_frames",
        "tx_bytes",
        "connections_created",
    ] {
        assert!(
            mmds.get(field)
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|value| value > 0),
            "production {context} metrics should report nonzero mmds.{field}; line:\n{latest_line}"
        );
    }
    for field in [
        "rx_accepted_err",
        "rx_accepted_unusual",
        "rx_bad_eth",
        "tx_errors",
    ] {
        assert_eq!(mmds[field], 0);
    }
    assert!(
        mmds["connections_destroyed"]
            .as_u64()
            .zip(mmds["connections_created"].as_u64())
            .is_some_and(|(destroyed, created)| destroyed <= created)
    );
    let net = latest
        .get("net")
        .expect("static network metrics should exist");
    for field in [
        "activate_fails",
        "cfg_fails",
        "event_fails",
        "no_rx_avail_buffer",
        "no_tx_avail_buffer",
        "rx_bytes_count",
        "rx_count",
        "rx_event_rate_limiter_count",
        "rx_fails",
        "rx_packets_count",
        "rx_queue_event_count",
        "rx_rate_limiter_throttled",
        "tx_bytes_count",
        "tx_count",
        "tx_fails",
        "tx_malformed_frames",
        "tx_packets_count",
        "tx_queue_event_count",
        "tx_rate_limiter_event_count",
        "tx_rate_limiter_throttled",
        "tx_remaining_reqs_count",
        "tx_spoofed_mac_count",
    ] {
        assert_eq!(net[field], metrics[field]);
    }
}

fn wait_for_network_mmds_snapshot_markers(path: &Path, markers: &[(u64, &[u8])], context: &str) {
    let deadline = Instant::now()
        .checked_add(SNAPSHOT_NETWORK_MMDS_TIMEOUT)
        .expect("network/MMDS marker deadline should fit");
    loop {
        let all_present = markers
            .iter()
            .all(|(offset, marker)| file_bytes_at(path, *offset, marker.len()) == *marker);
        if all_present {
            return;
        }
        let failure = file_bytes_at(path, 0, SNAPSHOT_NETWORK_MMDS_FAILURE_MARKER.len());
        assert_ne!(
            failure,
            SNAPSHOT_NETWORK_MMDS_FAILURE_MARKER,
            "{context} reported guest failure; backing prefix: {:?}",
            String::from_utf8_lossy(&file_bytes_at(
                path,
                0,
                usize::try_from(SNAPSHOT_NETWORK_MMDS_SECTORS * VIRTIO_BLOCK_SECTOR_BYTES)
                    .expect("network/MMDS diagnostic length should fit")
            ))
        );
        assert!(
            Instant::now() < deadline,
            "{context} timed out waiting for markers; backing prefix: {:?}",
            String::from_utf8_lossy(&file_bytes_at(
                path,
                0,
                usize::try_from(SNAPSHOT_NETWORK_MMDS_SECTORS * VIRTIO_BLOCK_SECTOR_BYTES)
                    .expect("network/MMDS diagnostic length should fit")
            ))
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn network_snapshot_load_body(resume_vm: bool, overrides: &[(&str, &str)]) -> String {
    serde_json::to_string(&serde_json::json!({
        "snapshot_path": SNAPSHOT_STATE_INPUT_REF,
        "mem_backend": {
            "backend_path": SNAPSHOT_MEMORY_INPUT_REF,
            "backend_type": "File",
        },
        "network_overrides": overrides
            .iter()
            .map(|(iface_id, host_dev_name)| serde_json::json!({
                "iface_id": iface_id,
                "host_dev_name": host_dev_name,
            }))
            .collect::<Vec<_>>(),
        "resume_vm": resume_vm,
    }))
    .expect("production network snapshot load body should serialize")
}

#[test]
fn normal_bundle_certifies_native_v2_memory_hotplug_snapshot_continuation_and_containment() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    for enable_pci in [false, true] {
        run_production_memory_hotplug_snapshot_continuation(
            &bundle,
            enable_pci,
            &baseline_sessions,
        );
    }
    assert_eq!(
        session_entries(),
        baseline_sessions,
        "memory-hotplug snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_production_memory_hotplug_snapshot_continuation(
    bundle: &Path,
    enable_pci: bool,
    baseline_sessions: &[PathBuf],
) {
    const MIB: u64 = 1024 * 1024;

    let transport = if enable_pci { "pci" } else { "mmio" };
    let source_fixture =
        SnapshotSourceGrantFixture::new(&format!("{transport}-memory-hotplug-source"));
    let source_logger = DeviceLoggerGrant::add_to_manifest(
        &source_fixture.manifest,
        &format!("{transport}-memory-hotplug-source"),
    );
    reset_zeroed_file(
        &source_fixture.data_backing,
        SNAPSHOT_MEMORY_HOTPLUG_SECTORS * 512,
    );
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        source_fixture
            .sensitive_strings()
            .into_iter()
            .chain(source_logger.sensitive_strings())
            .collect(),
        &format!("memory-hotplug-snapshot-{transport}-source"),
        false,
        enable_pci,
    );
    source_fixture.replace_source_file_pathnames();
    source_logger.replace_source_pathname();
    source_logger.configure(
        &source.socket,
        &format!("production {transport} memory-hotplug source"),
    );
    configure_and_start_memory_hotplug_snapshot_source(&source.socket, transport);
    wait_for_memory_hotplug_snapshot_marker(
        &source_fixture.opened_data_backing,
        SNAPSHOT_MEMORY_HOTPLUG_READY_MARKER,
        &format!("production {transport} memory-hotplug source readiness"),
    );
    assert_http_status(
        &http_request(
            &source.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
        ),
        204,
        &format!("grow production {transport} memory-hotplug source"),
    );
    wait_for_memory_hotplug_snapshot_marker(
        &source_fixture.opened_data_backing,
        SNAPSHOT_MEMORY_HOTPLUG_CAPTURE_READY_MARKER,
        &format!("production {transport} memory-hotplug source sentinel planting"),
    );
    wait_for_http_response_fragment(
        &source.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":128"#,
        SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production {transport} source should report 128 MiB plugged: {error}")
    });
    assert_production_memory_hotplug_config(&source.socket, 128, 128, transport);
    let source_metrics = flush_production_memory_hotplug_metrics(
        &source.socket,
        &source_fixture.opened_metrics,
        &format!("{transport} source grow"),
    );
    assert_eq!(source_metrics["plug_bytes"].as_u64(), Some(128 * MIB));
    assert!(
        source_metrics["plug_count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "production {transport} source should process guest PLUG requests"
    );
    assert_eq!(source_metrics["plug_fails"].as_u64(), Some(0));
    assert_production_memory_hotplug_latency_aggregates(
        &source_metrics,
        &format!("production {transport} memory-hotplug source"),
    );
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production {transport} memory-hotplug source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production {transport} memory-hotplug snapshot"),
    );
    let artifacts = source_fixture.artifacts();
    let source_memory_hotplug = assert_production_memory_hotplug_snapshot(
        &artifacts.state,
        enable_pci,
        &format!("{transport} source"),
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let state_before =
        fs::read(&artifacts.state).expect("production memory-hotplug source state should read");
    let memory_before =
        fs::read(&artifacts.memory).expect("production memory-hotplug source memory should read");
    stop_running_launcher(
        &mut source,
        &format!("production {transport} memory-hotplug snapshot source"),
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {transport} memory-hotplug snapshot source"),
    );
    source_fixture.assert_replacement_pathnames_unused(&format!(
        "production {transport} memory-hotplug snapshot source"
    ));
    source_logger.assert_records(
        &["device-kind=memory-hotplug operation=configuration-update outcome=succeeded"],
        source_fixture.sensitive_strings().into_iter().chain([
            String::from_utf8_lossy(SNAPSHOT_MEMORY_HOTPLUG_READY_MARKER).into_owned(),
            String::from_utf8_lossy(SNAPSHOT_MEMORY_HOTPLUG_CAPTURE_READY_MARKER).into_owned(),
        ]),
    );

    let mut current = artifacts;
    if !enable_pci {
        for malformed in [
            MemoryHotplugMalformedInput::StateChecksum,
            MemoryHotplugMalformedInput::TruncatedMemory,
        ] {
            run_production_memory_hotplug_malformed_case(
                bundle,
                &current,
                malformed,
                baseline_sessions,
            );
        }
        for shutdown in [
            SnapshotContinuationShutdown::GracefulCancellation,
            SnapshotContinuationShutdown::WorkerFirst,
            SnapshotContinuationShutdown::LauncherFirst,
        ] {
            current = run_production_memory_hotplug_paused_shutdown_case(
                bundle,
                current,
                shutdown,
                baseline_sessions,
            );
        }
    }

    let explicit_case = format!("{transport}-memory-hotplug-explicit");
    current =
        run_production_memory_hotplug_snapshot_destination(ProductionMemoryHotplugDestination {
            bundle,
            artifacts: current,
            source_memory_hotplug: &source_memory_hotplug,
            enable_pci,
            resume_vm: false,
            recapture: true,
            case: &explicit_case,
            baseline_sessions,
        });
    let automatic_case = format!("{transport}-memory-hotplug-automatic");
    let final_artifacts =
        run_production_memory_hotplug_snapshot_destination(ProductionMemoryHotplugDestination {
            bundle,
            artifacts: current,
            source_memory_hotplug: &source_memory_hotplug,
            enable_pci,
            resume_vm: true,
            recapture: false,
            case: &automatic_case,
            baseline_sessions,
        });
    assert_eq!(
        fs::read(&final_artifacts.state).expect("final memory-hotplug state should read"),
        state_before,
        "{transport} contained repeated loads must not mutate state"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final memory-hotplug memory should read"),
        memory_before,
        "{transport} contained repeated loads must not mutate memory"
    );
}

fn configure_and_start_memory_hotplug_snapshot_source(socket: &Path, context: &str) {
    for (path, body, request) in [
        (
            "/machine-config",
            serde_json::json!({
                "vcpu_count": 1,
                "mem_size_mib": 256,
                "track_dirty_pages": true,
            }),
            "machine config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "metrics",
        ),
        (
            "/hotplug/memory",
            serde_json::json!({
                "total_size_mib": 128,
                "block_size_mib": 2,
                "slot_size_mib": 128,
            }),
            "memory-hotplug config",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "boot_args": SNAPSHOT_MEMORY_HOTPLUG_BOOT_ARGS,
            }),
            "boot source",
        ),
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": false,
            }),
            "root drive",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": SNAPSHOT_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
            }),
            "data drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production memory-hotplug snapshot request should serialize"),
            ),
            204,
            &format!("PUT production {context} memory-hotplug snapshot {request}"),
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        &format!("start production {context} memory-hotplug snapshot source"),
    );
}

fn assert_production_memory_hotplug_snapshot(
    state_path: &Path,
    enable_pci: bool,
    context: &str,
) -> SnapshotV2MemoryHotplugState {
    const MIB: u64 = 1024 * 1024;

    let bytes = fs::read(state_path).unwrap_or_else(|error| {
        panic!(
            "production {context} memory-hotplug state {} should read: {error}",
            state_path.display()
        )
    });
    let structural =
        decode_snapshot_v2_state(&bytes).expect("production memory-hotplug state should decode");
    let state = decode_hvf_snapshot_v2_vsock_state(&structural)
        .expect("production memory-hotplug state should be exact native-v2 2.12");
    let graph = state
        .device_graph()
        .expect("production memory-hotplug artifact should retain root and data");
    assert_eq!(
        graph.block_records().len(),
        2,
        "production {context} should retain root and data drives"
    );
    assert!(state.entropy().is_none());
    assert!(state.balloon().is_none());
    let memory_hotplug = state
        .memory_hotplug()
        .expect("production certification artifact should contain kind 11");
    let expected_transport = if enable_pci {
        SnapshotV2DeviceTransportKind::Pci
    } else {
        SnapshotV2DeviceTransportKind::Mmio
    };
    assert_eq!(graph.transport_kind(), expected_transport);
    assert_eq!(memory_hotplug.transport().kind(), expected_transport);
    assert_eq!(memory_hotplug.config().total_size_mib(), 128);
    assert_eq!(memory_hotplug.config().block_size_mib(), 2);
    assert_eq!(memory_hotplug.config().slot_size_mib(), 128);
    assert_eq!(memory_hotplug.config_space().region_size(), 128 * MIB);
    assert_eq!(
        memory_hotplug.config_space().usable_region_size(),
        128 * MIB
    );
    assert_eq!(memory_hotplug.config_space().requested_size(), 128 * MIB);
    assert_eq!(memory_hotplug.config_space().plugged_size(), 128 * MIB);
    let queue = memory_hotplug
        .active_queue()
        .expect("production active Linux virtio-mem should retain queue cursors");
    assert_eq!(queue.next_available(), queue.next_used());
    let plugged_ranges = memory_hotplug.plugged_ranges().collect::<Vec<_>>();
    assert_eq!(plugged_ranges.len(), 1);
    assert_eq!(plugged_ranges[0].start_block(), 0);
    assert_eq!(plugged_ranges[0].block_count(), 64);
    memory_hotplug
        .validate_memory_binding_for_compatibility_version(
            state.platform().memory(),
            state.platform().memory().version(),
        )
        .expect("production kind-11 bitmap should close the kind-1 memory extents");
    memory_hotplug.clone()
}

fn assert_production_memory_hotplug_config(
    socket: &Path,
    plugged_size_mib: u64,
    requested_size_mib: u64,
    context: &str,
) {
    let status = http_get(socket, "/hotplug/memory");
    assert_http_status(
        &status,
        200,
        &format!("read production {context} memory-hotplug status"),
    );
    for expected in [
        r#""block_size_mib":2"#.to_owned(),
        format!(r#""plugged_size_mib":{plugged_size_mib}"#),
        format!(r#""requested_size_mib":{requested_size_mib}"#),
        r#""slot_size_mib":128"#.to_owned(),
        r#""total_size_mib":128"#.to_owned(),
    ] {
        assert!(
            status.contains(&expected),
            "production {context} memory-hotplug status should contain {expected}; response:\n{status}"
        );
    }
    let config = http_get(socket, "/vm/config");
    assert_http_status(
        &config,
        200,
        &format!("read production {context} restored VM config"),
    );
    assert!(config.contains(r#""memory-hotplug":"#));
    assert_eq!(config.matches(r#""drive_id":"#).count(), 2);
}

fn flush_production_memory_hotplug_metrics(
    socket: &Path,
    metrics_path: &Path,
    context: &str,
) -> serde_json::Value {
    flush_production_metrics(socket, context);
    let output = fs::read_to_string(metrics_path).unwrap_or_else(|error| {
        panic!(
            "production memory-hotplug metrics {} should read: {error}",
            metrics_path.display()
        )
    });
    output
        .lines()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("memory_hotplug")
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!("production {context} should emit memory_hotplug metrics; output:\n{output}")
        })
}

#[derive(Debug, Clone, Copy)]
enum MemoryHotplugMalformedInput {
    StateChecksum,
    TruncatedMemory,
}

fn run_production_memory_hotplug_malformed_case(
    bundle: &Path,
    artifacts: &SnapshotArtifactSet,
    malformed_input: MemoryHotplugMalformedInput,
    baseline_sessions: &[PathBuf],
) {
    let case = match malformed_input {
        MemoryHotplugMalformedInput::StateChecksum => "malformed-state",
        MemoryHotplugMalformedInput::TruncatedMemory => "truncated-memory",
    };
    let original_state =
        fs::read(&artifacts.state).expect("valid memory-hotplug state should read");
    let original_memory =
        fs::read(&artifacts.memory).expect("valid memory-hotplug memory should read");
    let malformed_root = TestDir::new(&format!("memory-hotplug-snapshot-{case}"));
    let canonical_root = fs::canonicalize(malformed_root.path())
        .expect("malformed memory-hotplug fixture root should canonicalize");
    let malformed = SnapshotArtifactSet {
        state: canonical_root.join("malformed-state.snap"),
        memory: canonical_root.join("malformed-memory.snap"),
        root: canonical_root.join("malformed-root.img"),
        data: canonical_root.join("malformed-data.img"),
        audit: canonical_root.join("malformed-audit.img"),
    };
    fs::copy(&artifacts.state, &malformed.state)
        .expect("malformed memory-hotplug state fixture should copy");
    fs::copy(&artifacts.memory, &malformed.memory)
        .expect("malformed memory-hotplug memory fixture should copy");
    for (source, destination, context) in [
        (
            &artifacts.root,
            &malformed.root,
            "malformed memory-hotplug root",
        ),
        (
            &artifacts.data,
            &malformed.data,
            "malformed memory-hotplug data",
        ),
        (
            &artifacts.audit,
            &malformed.audit,
            "malformed memory-hotplug audit",
        ),
    ] {
        hard_link_or_copy_fixture(source, destination, context);
    }
    match malformed_input {
        MemoryHotplugMalformedInput::StateChecksum => {
            let mut malformed_bytes =
                fs::read(&malformed.state).expect("malformed state fixture should read");
            let last = malformed_bytes
                .len()
                .checked_sub(1)
                .expect("native-v2 memory-hotplug state must be nonempty");
            malformed_bytes[last] ^= 0x80;
            fs::write(&malformed.state, malformed_bytes)
                .expect("malformed memory-hotplug checksum fixture should write");
        }
        MemoryHotplugMalformedInput::TruncatedMemory => {
            let len = fs::metadata(&malformed.memory)
                .expect("malformed memory fixture metadata should read")
                .len();
            let truncated = len
                .checked_sub(4096)
                .expect("native-v2 memory file should exceed one page");
            OpenOptions::new()
                .write(true)
                .open(&malformed.memory)
                .expect("malformed memory fixture should reopen")
                .set_len(truncated)
                .expect("malformed memory fixture should truncate");
        }
    }

    let fixture = SnapshotContinuationInputGrantFixture::new(case, malformed, false);
    let sensitive = fixture.sensitive_strings();
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        sensitive.clone(),
        &format!("memory-hotplug-snapshot-{case}"),
        false,
    );
    fixture.replace_source_pathnames();
    configure_memory_hotplug_snapshot_destination_metrics(&running.socket, case);
    let response = http_put(
        &running.socket,
        "/snapshot/load",
        &snapshot_load_body(false),
    );
    assert_http_status(
        &response,
        400,
        &format!("reject production memory-hotplug {case}"),
    );
    for private in &sensitive {
        assert!(
            !response.contains(private),
            "memory-hotplug {case} restore fault must redact private grant data"
        );
    }
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("malformed memory-hotplug metrics should read")
            .is_empty(),
        "memory-hotplug {case} restore must not publish metrics"
    );
    thread::sleep(Duration::from_millis(100));
    if running
        .child
        .try_wait()
        .expect("malformed memory-hotplug launcher status should read")
        .is_some()
        || !running.socket.exists()
    {
        let status = running.wait(&format!(
            "terminal production memory-hotplug {case} destination"
        ));
        assert!(
            !status.success(),
            "terminal memory-hotplug {case} rejection should fail closed"
        );
        assert!(
            !running.socket.exists(),
            "terminal memory-hotplug {case} rejection should remove its API socket"
        );
    } else {
        assert!(
            http_get(&running.socket, "/").contains(r#""state":"Not started""#),
            "memory-hotplug {case} restore must not publish a VM"
        );
        stop_running_launcher(
            &mut running,
            &format!("production memory-hotplug {case} destination"),
        );
    }
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production memory-hotplug {case} malformed destination"),
    );
    assert_eq!(
        fs::read(&artifacts.state).expect("valid state should survive malformed load"),
        original_state
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("valid memory should survive malformed load"),
        original_memory
    );
    fixture.assert_replacement_pathnames_unused(&format!(
        "production memory-hotplug {case} malformed destination"
    ));
}

fn run_production_memory_hotplug_paused_shutdown_case(
    bundle: &Path,
    artifacts: SnapshotArtifactSet,
    shutdown: SnapshotContinuationShutdown,
    baseline_sessions: &[PathBuf],
) -> SnapshotArtifactSet {
    let name = match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => "memory-hotplug-cancellation",
        SnapshotContinuationShutdown::WorkerFirst => "memory-hotplug-worker-first",
        SnapshotContinuationShutdown::LauncherFirst => "memory-hotplug-launcher-first",
    };
    let fixture = SnapshotContinuationInputGrantFixture::new(name, artifacts, false);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("memory-hotplug-snapshot-{name}"),
        false,
    );
    let opened = fixture.replace_source_pathnames();
    configure_memory_hotplug_snapshot_destination_metrics(&running.socket, name);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("shutdown-case memory-hotplug metrics should read")
            .is_empty(),
        "fresh {name} metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &snapshot_load_body(false),
        ),
        204,
        &format!("load production memory-hotplug snapshot before {name}"),
    );
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Paused""#),
        "memory-hotplug destination should remain Paused before {name}"
    );
    assert_production_memory_hotplug_config(&running.socket, 128, 128, name);
    let state_before =
        fs::read(&opened.state).expect("shutdown-case memory-hotplug state should read");
    let memory_before =
        fs::read(&opened.memory).expect("shutdown-case memory-hotplug memory should read");
    assert_eq!(session_entries().len(), baseline_sessions.len() + 1);

    let status = match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => {
            let launcher =
                i32::try_from(running.child.id()).expect("memory-hotplug launcher PID should fit");
            // SAFETY: The unreaped launcher owns this exact PID.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
            running.wait("Paused memory-hotplug restoration cancellation")
        }
        SnapshotContinuationShutdown::WorkerFirst => {
            let worker = only_worker_pid(&running.child);
            // SAFETY: The worker is the sole live child of the unreaped launcher.
            assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
            running.wait("Paused memory-hotplug worker-first death")
        }
        SnapshotContinuationShutdown::LauncherFirst => {
            let worker = only_worker_pid(&running.child);
            let worker_exit = ProcessExitWatch::new(worker);
            let launcher =
                i32::try_from(running.child.id()).expect("memory-hotplug launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID and its worker
            // independently observes authenticated lifecycle EOF.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
            let result = running.wait("Paused memory-hotplug launcher-first death");
            assert!(
                worker_exit.wait(PROCESS_TIMEOUT),
                "memory-hotplug worker should observe launcher death"
            );
            result
        }
    };
    match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => {
            assert!(
                status.success(),
                "memory-hotplug cancellation should be graceful"
            );
        }
        SnapshotContinuationShutdown::WorkerFirst => {
            assert_eq!(status.code(), Some(128 + libc::SIGKILL));
        }
        SnapshotContinuationShutdown::LauncherFirst => {
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
    }
    assert!(
        !running.socket.exists(),
        "production {name} destination should remove its API socket"
    );
    assert_session_entries_eventually_restored(baseline_sessions, name);
    assert_eq!(
        fs::read(&opened.state).expect("shutdown-case memory-hotplug state should remain"),
        state_before,
        "{name} must preserve immutable state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("shutdown-case memory-hotplug memory should remain"),
        memory_before,
        "{name} must preserve immutable memory"
    );
    fixture.assert_replacement_pathnames_unused(&format!(
        "production {name} memory-hotplug shutdown destination"
    ));
    opened
}

struct ProductionMemoryHotplugDestination<'a> {
    bundle: &'a Path,
    artifacts: SnapshotArtifactSet,
    source_memory_hotplug: &'a SnapshotV2MemoryHotplugState,
    enable_pci: bool,
    resume_vm: bool,
    recapture: bool,
    case: &'a str,
    baseline_sessions: &'a [PathBuf],
}

fn run_production_memory_hotplug_snapshot_destination(
    destination: ProductionMemoryHotplugDestination<'_>,
) -> SnapshotArtifactSet {
    const MIB: u64 = 1024 * 1024;

    let ProductionMemoryHotplugDestination {
        bundle,
        artifacts,
        source_memory_hotplug,
        enable_pci,
        resume_vm,
        recapture,
        case,
        baseline_sessions,
    } = destination;
    let fixture = SnapshotContinuationInputGrantFixture::new(case, artifacts, recapture);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("memory-hotplug-snapshot-{case}"),
        enable_pci,
    );
    let opened = fixture.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("destination memory-hotplug state should read before load");
    let memory_before = fs::read(&opened.memory)
        .expect("destination memory-hotplug memory should read before load");
    reset_zeroed_file(&opened.data, SNAPSHOT_MEMORY_HOTPLUG_SECTORS * 512);
    resize_and_write_file_marker_at(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_SECTORS * 512,
        SNAPSHOT_MEMORY_HOTPLUG_CONTINUE_OFFSET,
        SNAPSHOT_MEMORY_HOTPLUG_CONTINUE_MARKER,
    );
    configure_memory_hotplug_snapshot_destination_metrics(&running.socket, case);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("destination memory-hotplug metrics should read")
            .is_empty(),
        "production {case} destination metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &snapshot_load_body(resume_vm),
        ),
        204,
        &format!("load production {case} memory-hotplug snapshot"),
    );
    assert!(
        http_get(&running.socket, "/").contains(if resume_vm {
            r#""state":"Running""#
        } else {
            r#""state":"Paused""#
        }),
        "production {case} destination should publish the requested resume state"
    );
    assert_production_memory_hotplug_config(&running.socket, 128, 128, case);

    if !resume_vm {
        if recapture {
            assert_http_status(
                &http_put(&running.socket, "/snapshot/create", &snapshot_create_body()),
                204,
                &format!("recapture production {case} memory-hotplug snapshot"),
            );
            let recaptured = fixture.recaptured_artifacts();
            let recaptured_memory_hotplug = assert_production_memory_hotplug_snapshot(
                &recaptured.state,
                enable_pci,
                &format!("{case} recapture"),
            );
            assert_eq!(
                &recaptured_memory_hotplug, source_memory_hotplug,
                "production {case} Paused recapture should retain normalized kind-11 semantics"
            );
            fixture.assert_no_recapture_staging();
        }
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            204,
            &format!("resume production {case} memory-hotplug destination"),
        );
    }

    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_RESTORED_MARKER,
        &format!("production {case} restored plugged-memory sentinels"),
    );
    assert_production_memory_hotplug_config(&running.socket, 128, 128, case);
    resize_and_write_file_marker_at(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_SECTORS * 512,
        SNAPSHOT_MEMORY_HOTPLUG_CONTINUE_OFFSET,
        SNAPSHOT_MEMORY_HOTPLUG_OFFLINE_MARKER,
    );
    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_OFFLINE_READY_MARKER,
        &format!("production {case} guest memory offline preparation"),
    );
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":64}"#,
        ),
        204,
        &format!("shrink production {case} memory-hotplug destination"),
    );
    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_SHRUNK_MARKER,
        &format!("production {case} restored disjoint UNPLUG"),
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":64"#,
        SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("production {case} should report 64 MiB plugged: {error}"));
    assert_production_memory_hotplug_config(&running.socket, 64, 64, case);

    resize_and_write_file_marker_at(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_SECTORS * 512,
        SNAPSHOT_MEMORY_HOTPLUG_REPROBE_OFFSET,
        SNAPSHOT_MEMORY_HOTPLUG_REPROBE_MARKER,
    );
    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_UNPLUG_ALL_MARKER,
        &format!("production {case} restored Linux reprobe UNPLUG_ALL"),
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":0"#,
        SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production {case} should report zero plugged after UNPLUG_ALL: {error}")
    });
    assert_production_memory_hotplug_config(&running.socket, 0, 64, case);
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":64}"#,
        ),
        204,
        &format!("refresh production {case} 64 MiB after UNPLUG_ALL"),
    );
    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_REPROBED_MARKER,
        &format!("production {case} restored Linux reprobe"),
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":64"#,
        SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production {case} should replug 64 MiB after reprobe: {error}")
    });
    assert_production_memory_hotplug_config(&running.socket, 64, 64, case);

    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
        ),
        204,
        &format!("regrow production {case} memory-hotplug destination"),
    );
    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_REGROWN_MARKER,
        &format!("production {case} restored disjoint PLUG"),
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":128"#,
        SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("production {case} should report 128 MiB regrown: {error}"));
    assert_production_memory_hotplug_config(&running.socket, 128, 128, case);
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":0}"#,
        ),
        204,
        &format!("fully unplug production {case} memory-hotplug destination"),
    );
    wait_for_memory_hotplug_snapshot_marker(
        &opened.data,
        SNAPSHOT_MEMORY_HOTPLUG_SUCCESS_MARKER,
        &format!("production {case} restored final UNPLUG"),
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":0"#,
        SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("production {case} should report zero plugged: {error}"));
    assert_production_memory_hotplug_config(&running.socket, 0, 0, case);

    let metrics = flush_production_memory_hotplug_metrics(
        &running.socket,
        &fixture.opened_metrics,
        &format!("{case} restored topology activity"),
    );
    for field in ["queue_event_count", "plug_count", "unplug_count"] {
        assert!(
            metrics[field].as_u64().is_some_and(|count| count > 0),
            "production {case} destination memory_hotplug.{field} should be positive"
        );
    }
    assert!(
        metrics["unplug_all_count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "production {case} destination should process Linux reprobe UNPLUG_ALL"
    );
    assert_eq!(metrics["plug_bytes"].as_u64(), Some(128 * MIB));
    assert_eq!(metrics["unplug_bytes"].as_u64(), Some(192 * MIB));
    for field in [
        "activate_fails",
        "queue_event_fails",
        "plug_fails",
        "unplug_fails",
        "unplug_discard_fails",
        "unplug_all_fails",
        "state_fails",
    ] {
        assert_eq!(
            metrics[field].as_u64(),
            Some(0),
            "production {case} destination memory_hotplug.{field} should remain zero; metrics:\n{}",
            fs::read_to_string(&fixture.opened_metrics).unwrap_or_default()
        );
    }
    for extension in [
        "interrupt_fails",
        "rollback_count",
        "rollback_fails",
        "owner_cleanup_count",
        "owner_cleanup_fails",
        "teardown_count",
        "teardown_fails",
    ] {
        assert!(
            metrics.get(extension).is_none(),
            "production {case} destination must not publish memory_hotplug.{extension}"
        );
    }
    assert_metrics_family_fields(
        &fixture.opened_metrics,
        "memory_hotplug",
        &[
            "activate_fails",
            "plug_agg",
            "plug_bytes",
            "plug_count",
            "plug_fails",
            "queue_event_count",
            "queue_event_fails",
            "state_agg",
            "state_count",
            "state_fails",
            "unplug_agg",
            "unplug_all_agg",
            "unplug_all_count",
            "unplug_all_fails",
            "unplug_bytes",
            "unplug_count",
            "unplug_discard_fails",
            "unplug_fails",
        ],
        &format!("production {case} memory-hotplug destination"),
    );
    assert_production_memory_hotplug_latency_aggregates(
        &metrics,
        &format!("production {case} memory-hotplug destination"),
    );
    stop_running_launcher(
        &mut running,
        &format!("production {case} restored memory-hotplug destination"),
    );
    assert_session_entries_eventually_restored(
        baseline_sessions,
        &format!("production {case} restored memory-hotplug destination"),
    );
    assert_eq!(
        fs::read(&opened.state).expect("destination memory-hotplug state should remain"),
        state_before,
        "production {case} load must not mutate state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("destination memory-hotplug memory should remain"),
        memory_before,
        "production {case} load must not mutate memory"
    );
    fixture.assert_replacement_pathnames_unused(&format!(
        "production {case} restored memory-hotplug destination"
    ));
    opened
}

fn configure_memory_hotplug_snapshot_destination_metrics(socket: &Path, context: &str) {
    assert_http_status(
        &http_put(
            socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        &format!("PUT production {context} memory-hotplug destination metrics"),
    );
}

#[test]
fn normal_bundle_certifies_native_v2_balloon_snapshot_continuation_and_containment() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    for enable_pci in [false, true] {
        run_production_balloon_snapshot_continuation(&bundle, enable_pci, &baseline_sessions);
    }
    assert_eq!(
        session_entries(),
        baseline_sessions,
        "balloon snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_production_balloon_snapshot_continuation(
    bundle: &Path,
    enable_pci: bool,
    baseline_sessions: &[PathBuf],
) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let source_fixture = SnapshotSourceGrantFixture::new(&format!("{transport}-balloon-source"));
    let source_logger = DeviceLoggerGrant::add_to_manifest(
        &source_fixture.manifest,
        &format!("{transport}-balloon-source"),
    );
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        source_fixture
            .sensitive_strings()
            .into_iter()
            .chain(source_logger.sensitive_strings())
            .collect(),
        &format!("balloon-snapshot-{transport}-source"),
        false,
        enable_pci,
    );
    source_fixture.replace_source_file_pathnames();
    source_logger.replace_source_pathname();
    source_logger.configure(
        &source.socket,
        &format!("production {transport} balloon source"),
    );
    configure_and_start_balloon_snapshot_source(&source.socket, transport);
    wait_for_production_balloon_page_counts(
        &source.socket,
        2_048,
        2_048,
        &format!("production {transport} balloon source inflation"),
    );
    assert_http_status(
        &http_request(
            &source.socket,
            "PATCH",
            "/balloon/statistics",
            r#"{"stats_polling_interval_s":2}"#,
        ),
        204,
        &format!("update production {transport} balloon polling interval"),
    );
    wait_for_production_balloon_optional_statistics(
        &source.socket,
        &format!("production {transport} balloon source statistics"),
    );
    assert_http_status(
        &http_request(
            &source.socket,
            "PATCH",
            "/balloon/hinting/start",
            r#"{"acknowledge_on_stop":true}"#,
        ),
        204,
        &format!("start production {transport} balloon hinting"),
    );
    wait_for_production_balloon_hinting_status(
        &source.socket,
        u64::from(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE),
        Some(0),
        &format!("production {transport} balloon source hinting"),
    );
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/balloon/hinting/stop", ""),
        204,
        &format!("stop production {transport} balloon hinting"),
    );
    wait_for_production_balloon_metric(
        &source.socket,
        &source_fixture.opened_metrics,
        "free_page_report_count",
        1,
        &format!("production {transport} balloon source reporting"),
    );
    wait_for_file_prefix(
        &source_fixture.opened_data_backing,
        SNAPSHOT_BALLOON_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("production {transport} balloon guest marker should publish: {error}")
    });
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production {transport} balloon source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production {transport} balloon snapshot"),
    );
    let artifacts = source_fixture.artifacts();
    let source_balloon = assert_production_balloon_snapshot(
        &artifacts.state,
        enable_pci,
        &format!("{transport} source"),
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let state_before =
        fs::read(&artifacts.state).expect("production balloon source state should read");
    let memory_before =
        fs::read(&artifacts.memory).expect("production balloon source memory should read");
    stop_running_launcher(
        &mut source,
        &format!("production {transport} balloon snapshot source"),
    );
    assert_eq!(session_entries(), baseline_sessions);
    source_logger.assert_records(
        &["device-kind=balloon operation=inflate outcome=succeeded"],
        source_fixture
            .sensitive_strings()
            .into_iter()
            .chain([String::from_utf8_lossy(SNAPSHOT_BALLOON_MARKER).into_owned()]),
    );

    let mut current = artifacts;
    if !enable_pci {
        run_production_balloon_malformed_state_case(bundle, &current, baseline_sessions);
        for shutdown in [
            SnapshotContinuationShutdown::GracefulCancellation,
            SnapshotContinuationShutdown::WorkerFirst,
            SnapshotContinuationShutdown::LauncherFirst,
        ] {
            current = run_production_balloon_paused_shutdown_case(
                bundle,
                current,
                shutdown,
                baseline_sessions,
            );
        }
    }

    let explicit_case = format!("{transport}-explicit");
    current = run_production_balloon_snapshot_destination(ProductionBalloonSnapshotDestination {
        bundle,
        artifacts: current,
        source_balloon: &source_balloon,
        enable_pci,
        resume_vm: false,
        recapture: true,
        case: &explicit_case,
        baseline_sessions,
    });
    let automatic_case = format!("{transport}-automatic");
    let final_artifacts =
        run_production_balloon_snapshot_destination(ProductionBalloonSnapshotDestination {
            bundle,
            artifacts: current,
            source_balloon: &source_balloon,
            enable_pci,
            resume_vm: true,
            recapture: false,
            case: &automatic_case,
            baseline_sessions,
        });
    assert_eq!(
        fs::read(&final_artifacts.state).expect("final balloon state should read"),
        state_before,
        "{transport} contained repeated loads must not mutate state"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final balloon memory should read"),
        memory_before,
        "{transport} contained repeated loads must not mutate memory"
    );
}

fn configure_and_start_balloon_snapshot_source(socket: &Path, context: &str) {
    for (path, body, request) in [
        (
            "/machine-config",
            serde_json::json!({"vcpu_count": 1, "mem_size_mib": 256}),
            "machine config",
        ),
        (
            "/balloon",
            serde_json::json!({
                "amount_mib": 8,
                "deflate_on_oom": true,
                "stats_polling_interval_s": 1,
                "free_page_hinting": true,
                "free_page_reporting": true,
            }),
            "balloon config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "metrics",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "boot_args": SNAPSHOT_BALLOON_BOOT_ARGS,
            }),
            "boot source",
        ),
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": false,
            }),
            "root drive",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": SNAPSHOT_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
            }),
            "data drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production balloon snapshot request should serialize"),
            ),
            204,
            &format!("PUT production {context} balloon snapshot {request}"),
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        &format!("start production {context} balloon snapshot source"),
    );
}

#[derive(Debug, Clone, Copy)]
enum SnapshotContinuationShutdown {
    GracefulCancellation,
    WorkerFirst,
    LauncherFirst,
}

fn run_production_balloon_malformed_state_case(
    bundle: &Path,
    artifacts: &SnapshotArtifactSet,
    baseline_sessions: &[PathBuf],
) {
    let original_state =
        fs::read(&artifacts.state).expect("valid balloon state should read before corruption");
    let original_memory =
        fs::read(&artifacts.memory).expect("valid balloon memory should read before corruption");
    let malformed_root = TestDir::new("balloon-snapshot-malformed");
    let canonical_root = fs::canonicalize(malformed_root.path())
        .expect("malformed balloon fixture root should canonicalize");
    let malformed = SnapshotArtifactSet {
        state: canonical_root.join("malformed-state.snap"),
        memory: canonical_root.join("malformed-memory.snap"),
        root: canonical_root.join("malformed-root.img"),
        data: canonical_root.join("malformed-data.img"),
        audit: canonical_root.join("malformed-audit.img"),
    };
    fs::copy(&artifacts.state, &malformed.state)
        .expect("malformed balloon state fixture should copy");
    for (source, destination, context) in [
        (
            &artifacts.memory,
            &malformed.memory,
            "malformed balloon memory",
        ),
        (&artifacts.root, &malformed.root, "malformed balloon root"),
        (&artifacts.data, &malformed.data, "malformed balloon data"),
        (
            &artifacts.audit,
            &malformed.audit,
            "malformed balloon audit",
        ),
    ] {
        hard_link_or_copy_fixture(source, destination, context);
    }
    let mut malformed_bytes =
        fs::read(&malformed.state).expect("malformed balloon state fixture should read");
    let last = malformed_bytes
        .len()
        .checked_sub(1)
        .expect("native-v2 balloon state must be nonempty");
    malformed_bytes[last] ^= 0x80;
    fs::write(&malformed.state, malformed_bytes)
        .expect("malformed balloon checksum fixture should write");

    let fixture = SnapshotContinuationInputGrantFixture::new("malformed", malformed, false);
    let sensitive = fixture.sensitive_strings();
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        sensitive.clone(),
        "balloon-snapshot-malformed",
        false,
    );
    fixture.replace_source_pathnames();
    configure_balloon_snapshot_destination_metrics(&running.socket, "malformed");
    let response = http_put(
        &running.socket,
        "/snapshot/load",
        &snapshot_load_body(false),
    );
    assert_http_status(
        &response,
        400,
        "reject malformed production balloon snapshot",
    );
    for private in &sensitive {
        assert!(
            !response.contains(private),
            "malformed balloon restore fault must redact private grant data"
        );
    }
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Not started""#),
        "malformed balloon restore must not publish a VM"
    );
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("malformed balloon metrics should read")
            .is_empty(),
        "malformed balloon restore must not publish metrics"
    );
    stop_running_launcher(&mut running, "malformed balloon snapshot destination");
    assert_eq!(session_entries(), baseline_sessions);
    assert_eq!(
        fs::read(&artifacts.state).expect("valid balloon state should survive malformed load"),
        original_state
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("valid balloon memory should survive malformed load"),
        original_memory
    );
}

fn run_production_balloon_paused_shutdown_case(
    bundle: &Path,
    artifacts: SnapshotArtifactSet,
    shutdown: SnapshotContinuationShutdown,
    baseline_sessions: &[PathBuf],
) -> SnapshotArtifactSet {
    let name = match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => "cancellation",
        SnapshotContinuationShutdown::WorkerFirst => "worker-first",
        SnapshotContinuationShutdown::LauncherFirst => "launcher-first",
    };
    let fixture = SnapshotContinuationInputGrantFixture::new(name, artifacts, false);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("balloon-snapshot-{name}"),
        false,
    );
    let opened = fixture.replace_source_pathnames();
    configure_balloon_snapshot_destination_metrics(&running.socket, name);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("shutdown-case balloon metrics should read")
            .is_empty(),
        "fresh {name} balloon metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &snapshot_load_body(false),
        ),
        204,
        &format!("load production balloon snapshot before {name}"),
    );
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Paused""#),
        "balloon destination should remain Paused before {name}"
    );
    assert_production_balloon_config(&running.socket, name);
    let state_before = fs::read(&opened.state).expect("shutdown-case balloon state should read");
    let memory_before = fs::read(&opened.memory).expect("shutdown-case balloon memory should read");
    assert_eq!(session_entries().len(), baseline_sessions.len() + 1);

    let status = match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => {
            let launcher =
                i32::try_from(running.child.id()).expect("balloon launcher PID should fit");
            // SAFETY: The unreaped launcher owns this exact PID.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
            running.wait("Paused balloon restoration cancellation")
        }
        SnapshotContinuationShutdown::WorkerFirst => {
            let worker = only_worker_pid(&running.child);
            // SAFETY: The worker is the sole live child of the unreaped launcher.
            assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
            running.wait("Paused balloon worker-first death")
        }
        SnapshotContinuationShutdown::LauncherFirst => {
            let worker = only_worker_pid(&running.child);
            let worker_exit = ProcessExitWatch::new(worker);
            let launcher =
                i32::try_from(running.child.id()).expect("balloon launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID and its worker
            // independently observes authenticated lifecycle EOF.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
            let result = running.wait("Paused balloon launcher-first death");
            assert!(
                worker_exit.wait(PROCESS_TIMEOUT),
                "balloon worker should observe launcher death"
            );
            result
        }
    };
    match shutdown {
        SnapshotContinuationShutdown::GracefulCancellation => {
            assert!(status.success(), "balloon cancellation should be graceful");
        }
        SnapshotContinuationShutdown::WorkerFirst => {
            assert_eq!(status.code(), Some(128 + libc::SIGKILL));
        }
        SnapshotContinuationShutdown::LauncherFirst => {
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
    }
    assert!(
        !running.socket.exists(),
        "production balloon {name} destination should remove its API socket"
    );
    assert_eq!(session_entries(), baseline_sessions);
    assert_eq!(
        fs::read(&opened.state).expect("shutdown-case balloon state should remain"),
        state_before,
        "balloon {name} must preserve immutable state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("shutdown-case balloon memory should remain"),
        memory_before,
        "balloon {name} must preserve immutable memory"
    );
    assert_eq!(
        fs::read(&fixture.metrics).expect("shutdown-case replacement metrics should read"),
        b"replacement metrics must remain unused\n"
    );
    opened
}

struct ProductionBalloonSnapshotDestination<'a> {
    bundle: &'a Path,
    artifacts: SnapshotArtifactSet,
    source_balloon: &'a SnapshotV2BalloonState,
    enable_pci: bool,
    resume_vm: bool,
    recapture: bool,
    case: &'a str,
    baseline_sessions: &'a [PathBuf],
}

fn run_production_balloon_snapshot_destination(
    destination: ProductionBalloonSnapshotDestination<'_>,
) -> SnapshotArtifactSet {
    let ProductionBalloonSnapshotDestination {
        bundle,
        artifacts,
        source_balloon,
        enable_pci,
        resume_vm,
        recapture,
        case,
        baseline_sessions,
    } = destination;
    let fixture = SnapshotContinuationInputGrantFixture::new(case, artifacts, recapture);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("balloon-snapshot-{case}"),
        enable_pci,
    );
    let opened = fixture.replace_source_pathnames();
    let state_before =
        fs::read(&opened.state).expect("destination balloon state should read before load");
    let memory_before =
        fs::read(&opened.memory).expect("destination balloon memory should read before load");
    configure_balloon_snapshot_destination_metrics(&running.socket, case);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("destination balloon metrics should read")
            .is_empty(),
        "production {case} destination metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &snapshot_load_body(resume_vm),
        ),
        204,
        &format!("load production {case} balloon snapshot"),
    );
    assert!(
        http_get(&running.socket, "/").contains(if resume_vm {
            r#""state":"Running""#
        } else {
            r#""state":"Paused""#
        }),
        "production {case} destination should publish the requested resume state"
    );
    assert_production_balloon_config(&running.socket, case);

    if !resume_vm {
        thread::sleep(Duration::from_millis(2_250));
        flush_production_metrics(&running.socket, &format!("{case} while Paused"));
        assert_eq!(
            production_balloon_metric_total(&fixture.opened_metrics, "stats_updates_count"),
            0,
            "production {case} must not schedule retained statistics while Paused"
        );
        if recapture {
            assert_http_status(
                &http_put(&running.socket, "/snapshot/create", &snapshot_create_body()),
                204,
                &format!("recapture production {case} balloon snapshot"),
            );
            let recaptured = fixture.recaptured_artifacts();
            let recaptured_balloon = assert_production_balloon_snapshot(
                &recaptured.state,
                enable_pci,
                &format!("{case} recapture"),
            );
            assert_eq!(
                &recaptured_balloon, source_balloon,
                "production {case} Paused recapture should retain normalized balloon semantics"
            );
            fixture.assert_no_recapture_staging();
        }
        assert_http_status(
            &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            204,
            &format!("resume production {case} balloon destination"),
        );
    }

    let restored_statistics = http_get(&running.socket, "/balloon/statistics");
    assert_http_status(
        &restored_statistics,
        200,
        &format!("read production {case} restored balloon statistics"),
    );
    assert_production_balloon_statistics_match_snapshot(&restored_statistics, source_balloon, case);
    let hinting = http_get(&running.socket, "/balloon/hinting/status");
    assert_http_status(
        &hinting,
        200,
        &format!("read production {case} restored balloon hinting"),
    );
    assert!(
        hinting.contains(r#""host_cmd":1"#),
        "production {case} restored hinting should normalize to DONE; response:\n{hinting}"
    );

    let resumed_at = Instant::now();
    thread::sleep(Duration::from_millis(500));
    flush_production_metrics(
        &running.socket,
        &format!("{case} before full statistics interval"),
    );
    assert_eq!(
        production_balloon_metric_total(&fixture.opened_metrics, "stats_updates_count"),
        0,
        "production {case} retained statistics work must not complete early"
    );
    wait_for_production_balloon_metric(
        &running.socket,
        &fixture.opened_metrics,
        "stats_updates_count",
        1,
        &format!("production {case} retained statistics descriptor"),
    );
    assert!(
        resumed_at.elapsed() >= Duration::from_millis(1_500),
        "production {case} statistics completion should wait for one destination-local interval"
    );

    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/balloon/hinting/start",
            r#"{"acknowledge_on_stop":true}"#,
        ),
        204,
        &format!("start production {case} fresh balloon hinting"),
    );
    wait_for_production_balloon_hinting_status(
        &running.socket,
        u64::from(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE),
        Some(0),
        &format!("production {case} fresh balloon hinting"),
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/balloon/hinting/stop", ""),
        204,
        &format!("stop production {case} fresh balloon hinting"),
    );
    wait_for_production_balloon_metric(
        &running.socket,
        &fixture.opened_metrics,
        "free_page_report_count",
        1,
        &format!("production {case} restored free-page reporting"),
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/balloon", r#"{"amount_mib":0}"#),
        204,
        &format!("deflate production {case} restored balloon"),
    );
    wait_for_production_balloon_page_counts(
        &running.socket,
        0,
        0,
        &format!("production {case} restored balloon deflation"),
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/balloon", r#"{"amount_mib":8}"#),
        204,
        &format!("inflate production {case} restored balloon"),
    );
    wait_for_production_balloon_page_counts(
        &running.socket,
        2_048,
        2_048,
        &format!("production {case} restored balloon reinflation"),
    );
    flush_production_metrics(
        &running.socket,
        &format!("{case} after restored balloon activity"),
    );
    let metrics = fs::read_to_string(&fixture.opened_metrics).unwrap_or_default();
    for (field, minimum) in [
        ("stats_updates_count", 1),
        ("deflate_count", 1),
        ("inflate_count", 1),
        ("free_page_report_count", 1),
    ] {
        assert!(
            production_balloon_metric_total(&fixture.opened_metrics, field) >= minimum,
            "production {case} should publish balloon.{field} >= {minimum}; metrics:\n{metrics}"
        );
    }
    assert_metrics_family_fields(
        &fixture.opened_metrics,
        "balloon",
        &[
            "activate_fails",
            "deflate_count",
            "event_fails",
            "free_page_hint_count",
            "free_page_hint_fails",
            "free_page_hint_freed",
            "free_page_report_count",
            "free_page_report_fails",
            "free_page_report_freed",
            "inflate_count",
            "stats_update_fails",
            "stats_updates_count",
        ],
        &format!("production {case} balloon destination"),
    );
    for field in [
        "activate_fails",
        "event_fails",
        "free_page_hint_fails",
        "free_page_report_fails",
        "stats_update_fails",
    ] {
        assert_eq!(
            production_balloon_metric_total(&fixture.opened_metrics, field),
            0,
            "production {case} destination balloon.{field} should remain zero; metrics:\n{metrics}"
        );
    }
    assert!(
        production_balloon_metric_total(&fixture.opened_metrics, "free_page_report_freed") > 0,
        "production {case} destination should publish successfully freed reporting bytes; metrics:\n{metrics}"
    );
    assert_metrics_family_extensions_absent(
        &fixture.opened_metrics,
        "balloon",
        &[
            "inflate_discard_attempts",
            "inflate_discard_advised_bytes",
            "inflate_discard_skipped_bytes",
            "inflate_discard_fails",
            "hinting_discard_attempts",
            "hinting_discard_advised_bytes",
            "hinting_discard_skipped_bytes",
            "hinting_discard_fails",
            "free_page_report_requested_bytes",
            "free_page_report_advised_bytes",
            "free_page_report_skipped_bytes",
        ],
        &format!("production {case} balloon destination"),
    );
    stop_running_launcher(
        &mut running,
        &format!("production {case} restored balloon destination"),
    );
    assert_eq!(session_entries(), baseline_sessions);
    assert_eq!(
        fs::read(&opened.state).expect("destination balloon state should remain readable"),
        state_before,
        "production {case} load must not mutate state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("destination balloon memory should remain readable"),
        memory_before,
        "production {case} load must not mutate memory"
    );
    assert_eq!(
        fs::read(&fixture.metrics).expect("replacement balloon metrics should read"),
        b"replacement metrics must remain unused\n"
    );
    opened
}

fn configure_balloon_snapshot_destination_metrics(socket: &Path, context: &str) {
    assert_http_status(
        &http_put(
            socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        &format!("PUT production {context} balloon destination metrics"),
    );
}

#[test]
fn normal_bundle_certifies_native_v2_entropy_snapshot_continuation_and_containment() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    for enable_pci in [false, true] {
        for with_storage in [false, true] {
            run_production_entropy_snapshot_continuation(
                &bundle,
                enable_pci,
                with_storage,
                &baseline_sessions,
            );
        }
    }
    assert_eq!(
        session_entries(),
        baseline_sessions,
        "entropy snapshot launcher and worker teardown must restore the session namespace"
    );
}

fn run_production_entropy_snapshot_continuation(
    bundle: &Path,
    enable_pci: bool,
    with_storage: bool,
    baseline_sessions: &[PathBuf],
) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let product = if with_storage {
        "storage-entropy"
    } else {
        "entropy-only"
    };
    let case = format!("{transport}-{product}");
    let source_fixture = SerialSnapshotSourceGrantFixture::new_entropy(&case, with_storage);
    let source_logger = DeviceLoggerGrant::add_to_manifest(
        &source_fixture.manifest,
        &format!("{case}-entropy-source"),
    );
    let source_sensitive = source_fixture
        .sensitive_strings()
        .into_iter()
        .chain(source_logger.sensitive_strings())
        .collect::<Vec<_>>();
    let mut source = spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
        bundle,
        &source_fixture.manifest,
        &source_fixture.api_socket(),
        &format!("entropy-snapshot-{case}-source"),
        enable_pci,
    );
    source_fixture.replace_source_pathnames();
    source_logger.replace_source_pathname();
    source_logger.configure(&source.socket, &format!("production {case} entropy source"));
    configure_and_start_entropy_snapshot_grant_source(&source.socket, with_storage, &case);
    source
        .wait_for_stdout_marker(SNAPSHOT_ENTROPY_READY_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("production {case} entropy source should become ready: {error}")
        });
    wait_for_production_entropy_metric(
        &source.socket,
        &source_fixture.opened_metrics,
        "entropy_rate_limiter_throttled",
        1,
        &format!("production {case} source throttle"),
    );
    assert!(
        production_entropy_metric_total(&source_fixture.opened_metrics, "entropy_bytes")
            >= SNAPSHOT_ENTROPY_READ_BYTES,
        "production {case} source should complete one nonempty entropy request"
    );
    assert_http_status(
        &http_request(&source.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        &format!("pause production {case} entropy source"),
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("create production {case} entropy snapshot"),
    );
    let artifacts = source_fixture.artifacts();
    assert!(artifacts.state.is_file(), "{case} state should publish");
    assert!(artifacts.memory.is_file(), "{case} memory should publish");
    assert_production_pending_entropy_snapshot(&artifacts.state, enable_pci, with_storage, &case);
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    let state_before = fs::read(&artifacts.state).expect("production entropy state should read");
    let memory_before = fs::read(&artifacts.memory).expect("production entropy memory should read");

    let source_pid = i32::try_from(source.child.id()).expect("launcher PID should fit");
    // SAFETY: The unreaped source launcher owns this PID.
    assert_eq!(unsafe { libc::kill(source_pid, libc::SIGTERM) }, 0);
    let (source_status, source_stdout, source_stderr) =
        source.wait(&format!("production {case} entropy snapshot source"));
    assert!(
        source_status.success(),
        "production {case} entropy source should stop cleanly: {source_status:?}\nstdout:\n{source_stdout}\nstderr:\n{source_stderr}"
    );
    assert!(source_stdout.contains(SNAPSHOT_ENTROPY_READY_MARKER));
    assert!(!source_stdout.contains(SNAPSHOT_ENTROPY_SUCCESS_MARKER));
    assert_serial_snapshot_output_redacted(
        &source_stdout,
        &source_stderr,
        &source_sensitive,
        &format!("production {case} entropy source"),
    );
    source_logger.assert_records(
        &["device-kind=entropy operation=fill outcome=succeeded"],
        source_fixture
            .sensitive_strings()
            .into_iter()
            .chain([SNAPSHOT_ENTROPY_READY_MARKER.to_owned()]),
    );
    assert!(!source.socket.exists());
    assert_eq!(session_entries(), baseline_sessions);

    let mut current = artifacts;
    if !enable_pci && !with_storage {
        run_production_entropy_malformed_state_case(bundle, &current, baseline_sessions);
        for shutdown in [
            EntropySnapshotShutdown::GracefulCancellation,
            EntropySnapshotShutdown::WorkerFirst,
            EntropySnapshotShutdown::LauncherFirst,
        ] {
            current = run_production_entropy_paused_shutdown_case(
                bundle,
                current,
                shutdown,
                baseline_sessions,
            );
        }
    }

    let paused_fixture =
        SerialSnapshotInputGrantFixture::new_entropy(&format!("{case}-paused"), current, true);
    let paused_sensitive = paused_fixture.sensitive_strings();
    let mut paused = spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
        bundle,
        &paused_fixture.manifest,
        &paused_fixture.api_socket(),
        &format!("entropy-snapshot-{case}-paused"),
        enable_pci,
    );
    let next = paused_fixture.replace_source_pathnames();
    configure_serial_snapshot_grant_destination_metrics(&paused.socket);
    assert!(
        fs::read(&paused_fixture.opened_metrics)
            .expect("opened paused entropy metrics should read")
            .is_empty(),
        "{case} paused destination metrics should start empty"
    );
    assert_http_status(
        &http_put(&paused.socket, "/snapshot/load", &snapshot_load_body(false)),
        204,
        &format!("load production {case} entropy snapshot paused"),
    );
    assert!(
        http_get(&paused.socket, "/").contains(r#""state":"Paused""#),
        "production {case} entropy destination should remain Paused"
    );
    assert_production_entropy_config(&paused.socket, with_storage, &case);
    assert_http_status(
        &http_put(&paused.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        &format!("recapture production {case} entropy destination"),
    );
    let recaptured = paused_fixture.recaptured_artifacts();
    assert!(
        recaptured.state.is_file(),
        "production {case} recaptured entropy state should publish"
    );
    assert!(
        recaptured.memory.is_file(),
        "production {case} recaptured entropy memory should publish"
    );
    assert_production_pending_entropy_snapshot(
        &recaptured.state,
        enable_pci,
        with_storage,
        &format!("{case} recapture"),
    );
    let recaptured_state =
        fs::read(&recaptured.state).expect("recaptured production entropy state should read");
    let recaptured_memory =
        fs::read(&recaptured.memory).expect("recaptured production entropy memory should read");
    assert_http_status(
        &http_put(&paused.socket, "/snapshot/create", &snapshot_create_body()),
        400,
        &format!("reject production {case} entropy recapture collision"),
    );
    assert_eq!(
        fs::read(&recaptured.state).expect("colliding entropy recapture state should read"),
        recaptured_state,
        "production {case} recapture collision must not clobber state"
    );
    assert_eq!(
        fs::read(&recaptured.memory).expect("colliding entropy recapture memory should read"),
        recaptured_memory,
        "production {case} recapture collision must not clobber memory"
    );
    paused_fixture.assert_no_recapture_staging();
    assert_http_status(
        &http_request(&paused.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        &format!("resume production {case} entropy destination"),
    );
    let (paused_status, paused_stdout, paused_stderr) = paused.wait(&format!(
        "production {case} explicitly resumed entropy guest"
    ));
    assert!(
        paused_status.success(),
        "production {case} explicit entropy destination should exit cleanly: {paused_status:?}\nstdout:\n{paused_stdout}\nstderr:\n{paused_stderr}"
    );
    assert!(paused_stdout.contains(SNAPSHOT_ENTROPY_SUCCESS_MARKER));
    assert!(!paused_stdout.contains(SNAPSHOT_ENTROPY_FAILURE_MARKER));
    assert_serial_snapshot_output_redacted(
        &paused_stdout,
        &paused_stderr,
        &paused_sensitive,
        &format!("production {case} explicit entropy destination"),
    );
    assert_production_destination_entropy_metrics(
        &paused_fixture.opened_metrics,
        &format!("production {case} explicit entropy destination"),
    );
    assert_eq!(
        fs::read(&paused_fixture.metrics).expect("replacement paused entropy metrics should read"),
        b"replacement metrics must remain unused\n"
    );
    assert!(!paused.socket.exists());
    assert_eq!(session_entries(), baseline_sessions);

    let resumed_fixture =
        SerialSnapshotInputGrantFixture::new_entropy(&format!("{case}-automatic"), next, false);
    let resumed_sensitive = resumed_fixture.sensitive_strings();
    let mut resumed = spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
        bundle,
        &resumed_fixture.manifest,
        &resumed_fixture.api_socket(),
        &format!("entropy-snapshot-{case}-automatic"),
        enable_pci,
    );
    let final_artifacts = resumed_fixture.replace_source_pathnames();
    configure_serial_snapshot_grant_destination_metrics(&resumed.socket);
    assert!(
        fs::read(&resumed_fixture.opened_metrics)
            .expect("opened automatic entropy metrics should read")
            .is_empty(),
        "{case} automatic destination metrics should start empty"
    );
    assert_http_status(
        &http_put(&resumed.socket, "/snapshot/load", &snapshot_load_body(true)),
        204,
        &format!("load and resume production {case} entropy snapshot"),
    );
    let (resumed_status, resumed_stdout, resumed_stderr) = resumed.wait(&format!(
        "production {case} automatically resumed entropy guest"
    ));
    assert!(
        resumed_status.success(),
        "production {case} automatic entropy destination should exit cleanly: {resumed_status:?}\nstdout:\n{resumed_stdout}\nstderr:\n{resumed_stderr}"
    );
    assert!(resumed_stdout.contains(SNAPSHOT_ENTROPY_SUCCESS_MARKER));
    assert!(!resumed_stdout.contains(SNAPSHOT_ENTROPY_FAILURE_MARKER));
    assert_serial_snapshot_output_redacted(
        &resumed_stdout,
        &resumed_stderr,
        &resumed_sensitive,
        &format!("production {case} automatic entropy destination"),
    );
    assert_production_destination_entropy_metrics(
        &resumed_fixture.opened_metrics,
        &format!("production {case} automatic entropy destination"),
    );
    assert_eq!(
        fs::read(&resumed_fixture.metrics)
            .expect("replacement automatic entropy metrics should read"),
        b"replacement metrics must remain unused\n"
    );
    assert!(!resumed.socket.exists());
    assert_eq!(session_entries(), baseline_sessions);
    assert_eq!(
        fs::read(&final_artifacts.state).expect("final entropy state should read"),
        state_before,
        "{case} repeated contained loads must not mutate state"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final entropy memory should read"),
        memory_before,
        "{case} repeated contained loads must not mutate memory"
    );
}

#[derive(Debug, Clone, Copy)]
enum EntropySnapshotShutdown {
    GracefulCancellation,
    WorkerFirst,
    LauncherFirst,
}

fn run_production_entropy_malformed_state_case(
    bundle: &Path,
    artifacts: &SerialSnapshotGrantArtifacts,
    baseline_sessions: &[PathBuf],
) {
    assert!(
        artifacts.drive.is_none(),
        "representative malformed entropy case should remain entropy-only"
    );
    let original_state =
        fs::read(&artifacts.state).expect("original entropy state should read before corruption");
    let original_memory =
        fs::read(&artifacts.memory).expect("original entropy memory should read before corruption");
    let malformed_root = TestDir::new("entropy-snapshot-malformed");
    let canonical_malformed_root = fs::canonicalize(malformed_root.path())
        .expect("malformed entropy fixture root should canonicalize");
    let malformed_state = canonical_malformed_root.join("malformed-state.snap");
    let malformed_memory = canonical_malformed_root.join("malformed-memory.snap");
    fs::write(&malformed_state, &original_state)
        .expect("malformed entropy state fixture should write");
    fs::write(&malformed_memory, &original_memory)
        .expect("malformed entropy memory fixture should write");
    let mut malformed_bytes =
        fs::read(&malformed_state).expect("malformed entropy state fixture should read");
    let last = malformed_bytes
        .len()
        .checked_sub(1)
        .expect("native-v2 entropy state must be nonempty");
    malformed_bytes[last] ^= 0x80;
    fs::write(&malformed_state, malformed_bytes)
        .expect("malformed entropy checksum fixture should write");

    let fixture = SerialSnapshotInputGrantFixture::new_entropy(
        "entropy-malformed",
        SerialSnapshotGrantArtifacts {
            state: malformed_state,
            memory: malformed_memory,
            drive: None,
        },
        false,
    );
    let sensitive = fixture.sensitive_strings();
    let mut running = spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        "entropy-snapshot-malformed",
        false,
    );
    let opened = fixture.replace_source_pathnames();
    configure_serial_snapshot_grant_destination_metrics(&running.socket);
    let response = http_put(
        &running.socket,
        "/snapshot/load",
        &snapshot_load_body(false),
    );
    assert_http_status(
        &response,
        400,
        "reject malformed production entropy snapshot",
    );
    for private in &sensitive {
        assert!(
            !response.contains(private),
            "malformed entropy restore fault must redact private grant data"
        );
    }
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Not started""#),
        "malformed entropy restore must not publish a VM"
    );
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("malformed entropy destination metrics should read")
            .is_empty(),
        "malformed entropy restore must not publish destination metrics"
    );
    let launcher = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: The unreaped malformed-case launcher owns this exact PID.
    assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
    let (status, stdout, stderr) = running.wait("malformed entropy snapshot destination");
    assert!(
        status.success(),
        "malformed entropy destination should cancel cleanly: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_serial_snapshot_output_redacted(
        &stdout,
        &stderr,
        &sensitive,
        "malformed entropy snapshot destination",
    );
    assert!(!running.socket.exists());
    assert_eq!(session_entries(), baseline_sessions);
    assert_ne!(
        fs::read(&opened.state).expect("opened malformed entropy state should read"),
        original_state,
        "malformed fixture must differ from the valid entropy state"
    );
    assert_eq!(
        fs::read(&artifacts.state).expect("valid entropy state should survive malformed load"),
        original_state
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("valid entropy memory should survive malformed load"),
        original_memory
    );
}

fn run_production_entropy_paused_shutdown_case(
    bundle: &Path,
    artifacts: SerialSnapshotGrantArtifacts,
    shutdown: EntropySnapshotShutdown,
    baseline_sessions: &[PathBuf],
) -> SerialSnapshotGrantArtifacts {
    assert!(
        artifacts.drive.is_none(),
        "representative entropy shutdown case should remain entropy-only"
    );
    let name = match shutdown {
        EntropySnapshotShutdown::GracefulCancellation => "cancellation",
        EntropySnapshotShutdown::WorkerFirst => "worker-first",
        EntropySnapshotShutdown::LauncherFirst => "launcher-first",
    };
    let fixture =
        SerialSnapshotInputGrantFixture::new_entropy(&format!("entropy-{name}"), artifacts, false);
    let sensitive = fixture.sensitive_strings();
    let mut running = spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        &format!("entropy-snapshot-{name}"),
        false,
    );
    let opened = fixture.replace_source_pathnames();
    configure_serial_snapshot_grant_destination_metrics(&running.socket);
    assert!(
        fs::read(&fixture.opened_metrics)
            .expect("shutdown-case entropy metrics should read")
            .is_empty(),
        "fresh {name} entropy destination metrics should start empty"
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &snapshot_load_body(false),
        ),
        204,
        &format!("load production entropy snapshot before {name}"),
    );
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Paused""#),
        "entropy destination should remain Paused before {name}"
    );
    assert_production_entropy_config(&running.socket, false, name);
    for field in [
        "entropy_bytes",
        "rate_limiter_event_count",
        "host_rng_fails",
    ] {
        assert_eq!(
            production_entropy_metric_total(&fixture.opened_metrics, field),
            0,
            "Paused {name} entropy destination must start with zero entropy.{field}"
        );
    }
    let state_before = fs::read(&opened.state).expect("shutdown-case entropy state should read");
    let memory_before = fs::read(&opened.memory).expect("shutdown-case entropy memory should read");
    assert_eq!(session_entries().len(), baseline_sessions.len() + 1);

    let (status, stdout, stderr) = match shutdown {
        EntropySnapshotShutdown::GracefulCancellation => {
            let launcher =
                i32::try_from(running.child.id()).expect("entropy launcher PID should fit");
            // SAFETY: The unreaped launcher owns this exact PID.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGTERM) }, 0);
            running.wait("Paused entropy restoration cancellation")
        }
        EntropySnapshotShutdown::WorkerFirst => {
            let worker = only_worker_pid(&running.child);
            // SAFETY: The worker is the sole live child of the unreaped launcher.
            assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
            running.wait("Paused entropy worker-first death")
        }
        EntropySnapshotShutdown::LauncherFirst => {
            let worker = only_worker_pid(&running.child);
            let worker_exit = ProcessExitWatch::new(worker);
            let launcher =
                i32::try_from(running.child.id()).expect("entropy launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID and its worker
            // independently observes authenticated lifecycle EOF.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
            let result = running.wait("Paused entropy launcher-first death");
            assert!(
                worker_exit.wait(PROCESS_TIMEOUT),
                "entropy worker should observe launcher death"
            );
            result
        }
    };
    match shutdown {
        EntropySnapshotShutdown::GracefulCancellation => {
            assert!(status.success(), "entropy cancellation should be graceful");
        }
        EntropySnapshotShutdown::WorkerFirst => {
            assert_eq!(status.code(), Some(128 + libc::SIGKILL));
        }
        EntropySnapshotShutdown::LauncherFirst => {
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
    }
    assert_serial_snapshot_output_redacted(
        &stdout,
        &stderr,
        &sensitive,
        &format!("production entropy {name} destination"),
    );
    assert!(
        !running.socket.exists(),
        "production entropy {name} destination should remove its API socket"
    );
    assert_eq!(session_entries(), baseline_sessions);
    assert_eq!(
        fs::read(&opened.state).expect("shutdown-case entropy state should remain"),
        state_before,
        "entropy {name} must preserve immutable state"
    );
    assert_eq!(
        fs::read(&opened.memory).expect("shutdown-case entropy memory should remain"),
        memory_before,
        "entropy {name} must preserve immutable memory"
    );
    assert_eq!(
        fs::read(&fixture.metrics).expect("shutdown-case replacement metrics should read"),
        b"replacement metrics must remain unused\n"
    );
    opened
}

fn run_native_v2_snapshot_grant_case(bundle: &Path, enable_pci: bool) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    initialize_worker_container(bundle);
    let baseline_sessions = session_entries();
    let source_fixture = SnapshotSourceGrantFixture::new(&format!("{transport}-continuity"));
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        source_fixture.sensitive_strings(),
        &format!("snapshot-{transport}-source"),
        false,
        enable_pci,
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

    let peer_fixture = SnapshotSourceGrantFixture::new(&format!("{transport}-concurrent-peer"));
    let mut peer = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &peer_fixture.manifest,
        peer_fixture.sensitive_strings(),
        &format!("snapshot-{transport}-concurrent-peer"),
        false,
        enable_pci,
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

    let describe =
        SnapshotDescribeGrantFixture::new(&format!("{transport}-valid"), &artifacts.state, true);
    let describe_output = run_snapshot_describe(bundle, &describe);
    assert_output_success(&describe_output, "granted snapshot description");
    assert_eq!(
        String::from_utf8_lossy(&describe_output.stdout).trim(),
        "v2.12.0"
    );
    assert_snapshot_output_redacted(&describe_output, &describe.sensitive_strings());

    let mismatch = SnapshotDescribeGrantFixture::new(
        &format!("{transport}-wrong-role"),
        &artifacts.state,
        false,
    );
    let mismatch_output = run_snapshot_describe(bundle, &mismatch);
    assert_eq!(
        mismatch_output.status.code(),
        Some(BAD_CONFIGURATION_EXIT_CODE)
    );
    assert!(String::from_utf8_lossy(&mismatch_output.stderr).contains("snapshot inspection"));
    assert_snapshot_output_redacted(&mismatch_output, &mismatch.sensitive_strings());
    assert_eq!(session_entries(), baseline_sessions);

    let editor =
        certify_signed_snapshot_editor(&artifacts, state_before, memory_before, transport, false);

    let paused_fixture =
        SnapshotInputGrantFixture::new(&format!("{transport}-paused"), editor.artifacts.clone());
    let mut paused = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &paused_fixture.manifest,
        paused_fixture.sensitive_strings(),
        &format!("snapshot-{transport}-paused"),
        false,
        enable_pci,
    );
    let next_artifacts = paused_fixture.replace_source_pathnames();
    let paused_load_body = snapshot_load_body(false);
    let paused_load = http_put(&paused.socket, "/snapshot/load", &paused_load_body);
    assert_http_status(&paused_load, 204, "load granted snapshot paused");
    let paused_state = http_get(&paused.socket, "/");
    assert_http_status(&paused_state, 200, "read granted paused snapshot state");
    assert!(paused_state.contains(r#""state":"Paused""#));
    let paused_config = http_get(&paused.socket, "/vm/config");
    assert_http_status(
        &paused_config,
        200,
        "read granted paused multi-block snapshot config",
    );
    for expected in [
        r#""drive_id":"rootfs""#,
        r#""drive_id":"data""#,
        r#""drive_id":"audit""#,
        r#""is_root_device":true"#,
        r#""is_read_only":false"#,
        r#""is_read_only":true"#,
        r#""cache_type":"Unsafe""#,
        r#""cache_type":"Writeback""#,
        r#""io_engine":"Async""#,
        r#""io_engine":"Sync""#,
    ] {
        assert!(
            paused_config.contains(expected),
            "{transport} contained restore should retain {expected}; response:\n{paused_config}"
        );
    }
    assert_eq!(
        paused_config.matches(r#""drive_id":"#).count(),
        3,
        "{transport} contained restore should commit the complete drive vector"
    );
    assert_http_status(
        &http_request(&paused.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume granted snapshot",
    );
    assert!(
        paused
            .wait(&format!(
                "explicitly resumed {transport} granted snapshot root read"
            ))
            .success(),
        "explicitly resumed {transport} granted snapshot should power off after the root read"
    );
    editor.assert_opened_artifacts_unchanged(
        &next_artifacts,
        &format!("explicitly resumed {transport} edited Full restore"),
    );
    assert_eq!(session_entries(), baseline_sessions);

    let resumed_fixture =
        SnapshotInputGrantFixture::new(&format!("{transport}-automatic"), next_artifacts);
    let mut resumed = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &resumed_fixture.manifest,
        resumed_fixture.sensitive_strings(),
        &format!("snapshot-{transport}-automatic"),
        false,
        enable_pci,
    );
    let final_artifacts = resumed_fixture.replace_source_pathnames();
    let resumed_load = http_put(&resumed.socket, "/snapshot/load", &snapshot_load_body(true));
    assert_http_status(
        &resumed_load,
        204,
        "load and automatically resume granted snapshot",
    );
    assert!(
        resumed
            .wait(&format!(
                "automatically resumed {transport} granted snapshot root read"
            ))
            .success(),
        "automatically resumed {transport} granted snapshot should power off after the root read"
    );
    editor.assert_opened_artifacts_unchanged(
        &final_artifacts,
        &format!("automatically resumed {transport} edited Full restore"),
    );
    assert_eq!(session_entries(), baseline_sessions);
}

fn run_native_v2_diff_snapshot_grant_case(bundle: &Path, enable_pci: bool) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    initialize_worker_container(bundle);
    let baseline_sessions = session_entries();
    let source_fixture =
        SnapshotSourceGrantFixture::new(&format!("{transport}-diff-certification"));
    let mut source = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        source_fixture.sensitive_strings(),
        &format!("snapshot-{transport}-diff-source"),
        false,
        enable_pci,
    );
    source_fixture.replace_source_file_pathnames();
    configure_and_pause_snapshot_source_with_tracking(
        &source,
        &source_fixture.opened_metrics,
        true,
    );

    let create = http_put(
        &source.socket,
        "/snapshot/create",
        &snapshot_diff_create_body(),
    );
    assert_http_status(&create, 204, "create granted exact-2.13 Diff snapshot");
    let artifacts = source_fixture.artifacts();
    assert!(artifacts.state.is_file(), "granted Diff state should exist");
    assert!(
        artifacts.memory.is_file(),
        "granted Diff layer should exist"
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);

    let state_before = fs::read(&artifacts.state).expect("granted Diff state should read");
    let memory_before = fs::read(&artifacts.memory).expect("granted Diff layer should read");
    let structural =
        decode_snapshot_v2_state(&state_before).expect("granted Diff state should decode");
    let decoded = decode_hvf_snapshot_v2_diff_state(&structural)
        .expect("granted Diff state should close as exact native-v2 2.13");
    let graph = decoded
        .device_graph()
        .expect("granted Diff should retain all three block devices");
    assert_eq!(graph.block_records().len(), 3);
    assert!(graph.pmem_records().is_empty());
    assert_eq!(
        graph.transport_kind(),
        if enable_pci {
            SnapshotV2DeviceTransportKind::Pci
        } else {
            SnapshotV2DeviceTransportKind::Mmio
        }
    );
    let layer = decoded.layer();
    assert!(matches!(layer.base(), SnapshotV2DiffBase::Zero));
    let selected_bytes = layer
        .data_extents()
        .iter()
        .map(|extent| extent.range().size())
        .sum::<u64>();
    let result_bytes = layer
        .result()
        .extents()
        .iter()
        .map(|extent| extent.range().size())
        .sum::<u64>();
    assert!(
        selected_bytes > 0 && selected_bytes < result_bytes,
        "tracked {transport} boot Diff should be nonempty and sparse: selected={selected_bytes}, result={result_bytes}"
    );
    let mut layer_file = fs::File::open(&artifacts.memory)
        .expect("granted Diff layer should reopen for detached verification");
    verify_snapshot_v2_diff_layer_output(layer, &mut layer_file)
        .expect("granted Diff layer should match its exact state binding");
    drop(layer_file);
    drop(decoded);

    stop_running_launcher(&mut source, "granted exact-2.13 Diff source");
    source_fixture
        .assert_replacement_pathnames_unused("ordinary production exact-2.13 Diff source");
    assert_eq!(session_entries(), baseline_sessions);

    let describe = SnapshotDescribeGrantFixture::new(
        &format!("{transport}-diff-valid"),
        &artifacts.state,
        true,
    );
    let describe_output = run_snapshot_describe(bundle, &describe);
    assert_output_success(&describe_output, "granted Diff snapshot description");
    assert_eq!(
        String::from_utf8_lossy(&describe_output.stdout).trim(),
        "v2.13.0"
    );
    assert_snapshot_output_redacted(&describe_output, &describe.sensitive_strings());
    assert_eq!(session_entries(), baseline_sessions);

    let editor =
        certify_signed_snapshot_editor(&artifacts, state_before, memory_before, transport, true);

    let destination_fixture = SnapshotInputGrantFixture::new(
        &format!("{transport}-diff-paused"),
        editor.artifacts.clone(),
    );
    let mut destination = spawn_ready_snapshot_grant_api_launcher(
        bundle,
        &destination_fixture.manifest,
        destination_fixture.sensitive_strings(),
        &format!("snapshot-{transport}-diff-destination"),
        false,
        enable_pci,
    );
    let opened = destination_fixture.replace_source_pathnames();
    let load = http_put(
        &destination.socket,
        "/snapshot/load",
        &snapshot_load_body(false),
    );
    assert_http_status(&load, 204, "load granted exact-2.13 Diff paused");
    let state = http_get(&destination.socket, "/");
    assert_http_status(&state, 200, "read granted Diff destination state");
    assert!(state.contains(r#""state":"Paused""#));

    let config = http_get(&destination.socket, "/vm/config");
    assert_http_status(&config, 200, "read granted Diff destination config");
    for expected in [
        r#""drive_id":"rootfs""#,
        r#""drive_id":"data""#,
        r#""drive_id":"audit""#,
        r#""is_root_device":true"#,
        r#""is_read_only":false"#,
        r#""is_read_only":true"#,
        r#""cache_type":"Unsafe""#,
        r#""cache_type":"Writeback""#,
        r#""io_engine":"Async""#,
        r#""io_engine":"Sync""#,
    ] {
        assert!(
            config.contains(expected),
            "{transport} Diff restore should retain {expected}; response:\n{config}"
        );
    }
    assert_eq!(
        config.matches(r#""drive_id":"#).count(),
        3,
        "{transport} Diff restore should commit the complete drive vector"
    );
    assert_http_status(
        &http_request(
            &destination.socket,
            "PATCH",
            "/vm",
            r#"{"state":"Resumed"}"#,
        ),
        204,
        "resume granted exact-2.13 Diff",
    );
    assert!(
        destination
            .wait(&format!(
                "explicitly resumed {transport} granted exact-2.13 Diff root read"
            ))
            .success(),
        "explicitly resumed {transport} Diff should power off after the root read"
    );
    assert_session_entries_eventually_restored(
        &baseline_sessions,
        &format!("ordinary production {transport} Diff destination"),
    );
    editor.assert_opened_artifacts_unchanged(
        &opened,
        &format!("explicitly resumed {transport} edited Diff restore"),
    );
    destination_fixture
        .assert_replacement_pathnames_unused("ordinary production exact-2.13 Diff destination");
}

#[test]
fn normal_bundle_certifies_native_v2_storage_epochs_over_mmio_and_pci() {
    let bundle = production_bundle();
    for enable_pci in [false, true] {
        for rooted in [true, false] {
            run_native_v2_snapshot_epoch_grant_case(&bundle, enable_pci, rooted);
        }
    }
}

fn run_native_v2_snapshot_epoch_grant_case(bundle: &Path, enable_pci: bool, rooted: bool) {
    let transport = if enable_pci { "pci" } else { "mmio" };
    let product = if rooted {
        "pmem-only-rooted"
    } else {
        "mixed-rootless"
    };
    let case = format!("{transport}-{product}");
    initialize_worker_container(bundle);
    let baseline_sessions = session_entries();

    let source_fixture = SnapshotEpochSourceGrantFixture::new(&case, rooted);
    let mut source = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &source_fixture.manifest,
        &source_fixture.api_socket(),
        source_fixture.sensitive_strings(),
        &format!("snapshot-epoch-{case}-source"),
        enable_pci,
    );
    source_fixture.replace_source_file_pathnames();
    configure_and_pause_snapshot_epoch_source(
        &source,
        &source_fixture.opened_metrics,
        source_fixture.opened_blocks.as_ref(),
        &source_fixture.opened_writable_pmem,
        &source_fixture.opened_read_only_pmem,
        rooted,
    );
    assert_snapshot_pmem_epoch(
        &source_fixture.writable_pmem,
        SNAPSHOT_PMEM_WRITABLE_REPLACEMENT_BYTE,
        &format!("{case} source writable replacement pathname"),
    );
    assert_snapshot_pmem_epoch(
        &source_fixture.read_only_pmem,
        SNAPSHOT_PMEM_READ_ONLY_REPLACEMENT_BYTE,
        &format!("{case} source read-only replacement pathname"),
    );

    let create_body = snapshot_create_body();
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &create_body),
        204,
        "create epoch snapshot",
    );
    let artifacts = source_fixture.artifacts();
    assert!(artifacts.state.is_file(), "{case} state should publish");
    assert!(artifacts.memory.is_file(), "{case} memory should publish");
    let state_before = fs::read(&artifacts.state).expect("epoch snapshot state should read");
    let memory_before = fs::read(&artifacts.memory).expect("epoch snapshot memory should read");
    if let Some(blocks) = artifacts.blocks.as_ref() {
        assert_snapshot_block_epoch(
            &blocks.root,
            SNAPSHOT_BLOCK_DRIVE_A_PRE_CAPTURE_BYTE,
            &format!("{case} source primary epoch"),
        );
        assert_snapshot_block_epoch(
            &blocks.data,
            SNAPSHOT_BLOCK_DRIVE_B_PRE_CAPTURE_BYTE,
            &format!("{case} source data epoch"),
        );
        assert_snapshot_block_epoch(
            &blocks.audit,
            SNAPSHOT_BLOCK_AUDIT_BYTE,
            &format!("{case} source audit epoch"),
        );
    }
    assert_snapshot_pmem_epoch(
        &artifacts.writable_pmem,
        SNAPSHOT_PMEM_WRITABLE_PRE_CAPTURE_BYTE,
        &format!("{case} source writable pmem epoch"),
    );
    assert_snapshot_pmem_epoch(
        &artifacts.read_only_pmem,
        SNAPSHOT_PMEM_READ_ONLY_BYTE,
        &format!("{case} source read-only pmem epoch"),
    );

    assert_http_status(
        &http_put(
            &source.socket,
            "/snapshot/create",
            &repeated_snapshot_create_body(),
        ),
        204,
        "create second epoch snapshot through reused output grants",
    );
    let repeated = source_fixture.repeated_artifacts();
    assert!(
        repeated.state.is_file(),
        "{case} repeated state should publish"
    );
    assert!(
        repeated.memory.is_file(),
        "{case} repeated memory should publish"
    );
    assert_http_status(
        &http_put(&source.socket, "/snapshot/create", &create_body),
        400,
        "reject epoch snapshot output collision without clobber",
    );
    assert_eq!(
        fs::read(&artifacts.state).expect("epoch state should survive collision"),
        state_before
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("epoch memory should survive collision"),
        memory_before
    );
    assert_no_snapshot_staging(&source_fixture.state_directory);
    assert_no_snapshot_staging(&source_fixture.memory_directory);
    stop_running_launcher(&mut source, "granted snapshot epoch source");
    assert_eq!(session_entries(), baseline_sessions);

    let mut current = artifacts;
    if !enable_pci && !rooted {
        current = run_snapshot_epoch_paused_death_case(
            bundle,
            current,
            SnapshotEpochDeathOrder::WorkerFirst,
            &baseline_sessions,
        );
        current = run_snapshot_epoch_paused_death_case(
            bundle,
            current,
            SnapshotEpochDeathOrder::LauncherFirst,
            &baseline_sessions,
        );
    }

    let paused_fixture = SnapshotEpochInputGrantFixture::new(&format!("{case}-paused"), current);
    let mut paused = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &paused_fixture.manifest,
        &paused_fixture.api_socket(),
        paused_fixture.sensitive_strings(),
        &format!("snapshot-epoch-{case}-paused"),
        enable_pci,
    );
    let next = paused_fixture.replace_source_pathnames();
    configure_snapshot_epoch_destination_metrics(&paused, &format!("{case} paused destination"));
    assert_http_status(
        &http_put(&paused.socket, "/snapshot/load", &snapshot_load_body(false)),
        204,
        "load epoch snapshot paused",
    );
    let paused_state = http_get(&paused.socket, "/");
    assert_http_status(&paused_state, 200, "read epoch destination state");
    assert!(paused_state.contains(r#""state":"Paused""#));
    assert!(
        paused_state.contains(&format!("snapshot-epoch-{case}-paused")),
        "{case} destination should publish a fresh process identity"
    );
    assert_snapshot_epoch_public_config(&paused.socket, rooted, &case);
    assert_http_status(
        &http_put(&paused.socket, "/snapshot/create", &snapshot_create_body()),
        204,
        "recapture paused epoch destination",
    );
    assert!(
        paused_fixture.recaptured_artifacts().state.is_file(),
        "{case} recaptured state should publish"
    );
    assert!(
        paused_fixture.recaptured_artifacts().memory.is_file(),
        "{case} recaptured memory should publish"
    );
    let recaptured = paused_fixture.recaptured_artifacts();
    assert_production_snapshot_time_identity_transition(&paused_fixture.opened, &recaptured, &case);
    assert_no_snapshot_staging(&paused_fixture.state_directory);
    assert_no_snapshot_staging(&paused_fixture.memory_directory);
    assert_http_status(
        &http_request(&paused.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume epoch snapshot",
    );
    assert!(
        paused
            .wait(&format!("{case} explicitly resumed epoch destination"))
            .success(),
        "{case} explicitly resumed epoch destination should power off"
    );
    if let Some(blocks) = next.blocks.as_ref() {
        assert_snapshot_block_epoch(
            &blocks.root,
            SNAPSHOT_BLOCK_DRIVE_A_DESTINATION_ONE_BYTE,
            &format!("{case} destination-one primary epoch"),
        );
        assert_snapshot_block_epoch(
            &blocks.data,
            SNAPSHOT_BLOCK_DRIVE_B_DESTINATION_ONE_BYTE,
            &format!("{case} destination-one data epoch"),
        );
        assert_snapshot_block_epoch(
            &blocks.audit,
            SNAPSHOT_BLOCK_AUDIT_BYTE,
            &format!("{case} destination-one audit epoch"),
        );
        assert_snapshot_block_metrics(
            &paused_fixture.opened_metrics,
            true,
            &format!("{case} paused destination metrics"),
        );
    }
    assert_snapshot_pmem_epoch(
        &next.writable_pmem,
        SNAPSHOT_PMEM_WRITABLE_DESTINATION_ONE_BYTE,
        &format!("{case} destination-one writable pmem epoch"),
    );
    assert_snapshot_pmem_epoch(
        &next.read_only_pmem,
        SNAPSHOT_PMEM_READ_ONLY_BYTE,
        &format!("{case} destination-one read-only pmem epoch"),
    );
    assert_snapshot_pmem_metrics(
        &paused_fixture.opened_metrics,
        true,
        &format!("{case} paused destination metrics"),
    );
    assert_snapshot_pmem_epoch(
        &paused_fixture.sources.writable_pmem,
        SNAPSHOT_PMEM_WRITABLE_REPLACEMENT_BYTE,
        &format!("{case} paused writable replacement pathname"),
    );
    assert_snapshot_pmem_epoch(
        &paused_fixture.sources.read_only_pmem,
        SNAPSHOT_PMEM_READ_ONLY_REPLACEMENT_BYTE,
        &format!("{case} paused read-only replacement pathname"),
    );
    assert_eq!(session_entries(), baseline_sessions);

    let resumed_fixture = SnapshotEpochInputGrantFixture::new(&format!("{case}-automatic"), next);
    let mut resumed = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &resumed_fixture.manifest,
        &resumed_fixture.api_socket(),
        resumed_fixture.sensitive_strings(),
        &format!("snapshot-epoch-{case}-automatic"),
        enable_pci,
    );
    let final_artifacts = resumed_fixture.replace_source_pathnames();
    configure_snapshot_epoch_destination_metrics(
        &resumed,
        &format!("{case} automatic destination"),
    );
    assert_http_status(
        &http_put(&resumed.socket, "/snapshot/load", &snapshot_load_body(true)),
        204,
        "load and automatically resume epoch snapshot",
    );
    assert!(
        resumed
            .wait(&format!("{case} automatically resumed epoch destination"))
            .success(),
        "{case} automatically resumed epoch destination should power off"
    );
    if let Some(blocks) = final_artifacts.blocks.as_ref() {
        assert_snapshot_block_epoch(
            &blocks.root,
            SNAPSHOT_BLOCK_DRIVE_A_DESTINATION_TWO_BYTE,
            &format!("{case} destination-two primary epoch"),
        );
        assert_snapshot_block_epoch(
            &blocks.data,
            SNAPSHOT_BLOCK_DRIVE_B_DESTINATION_TWO_BYTE,
            &format!("{case} destination-two data epoch"),
        );
        assert_snapshot_block_epoch(
            &blocks.audit,
            SNAPSHOT_BLOCK_AUDIT_BYTE,
            &format!("{case} destination-two audit epoch"),
        );
        assert_snapshot_block_metrics(
            &resumed_fixture.opened_metrics,
            true,
            &format!("{case} automatic destination metrics"),
        );
    }
    assert_snapshot_pmem_epoch(
        &final_artifacts.writable_pmem,
        SNAPSHOT_PMEM_WRITABLE_DESTINATION_TWO_BYTE,
        &format!("{case} destination-two writable pmem epoch"),
    );
    assert_snapshot_pmem_epoch(
        &final_artifacts.read_only_pmem,
        SNAPSHOT_PMEM_READ_ONLY_BYTE,
        &format!("{case} destination-two read-only pmem epoch"),
    );
    assert_snapshot_pmem_metrics(
        &resumed_fixture.opened_metrics,
        true,
        &format!("{case} automatic destination metrics"),
    );
    assert_snapshot_pmem_epoch(
        &resumed_fixture.sources.writable_pmem,
        SNAPSHOT_PMEM_WRITABLE_REPLACEMENT_BYTE,
        &format!("{case} automatic writable replacement pathname"),
    );
    assert_snapshot_pmem_epoch(
        &resumed_fixture.sources.read_only_pmem,
        SNAPSHOT_PMEM_READ_ONLY_REPLACEMENT_BYTE,
        &format!("{case} automatic read-only replacement pathname"),
    );
    assert_eq!(session_entries(), baseline_sessions);

    assert_eq!(
        fs::read(&final_artifacts.state).expect("final epoch state should read"),
        state_before,
        "{case} repeated destinations must not mutate state"
    );
    assert_eq!(
        fs::read(&final_artifacts.memory).expect("final epoch memory should read"),
        memory_before,
        "{case} repeated destinations must not mutate memory"
    );
}

struct ProductionSnapshotTimeIdentityEvidence {
    stable_profile: serde_json::Value,
    time: serde_json::Value,
    vmgenid: [u8; 16],
}

fn json_difference_paths(left: &serde_json::Value, right: &serde_json::Value) -> Vec<String> {
    fn visit(
        left: &serde_json::Value,
        right: &serde_json::Value,
        path: &str,
        differences: &mut Vec<String>,
    ) {
        if differences.len() >= 16 || left == right {
            return;
        }
        match (left, right) {
            (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
                for (key, left) in left {
                    if differences.len() >= 16 {
                        break;
                    }
                    let child = format!("{path}.{key}");
                    if let Some(right) = right.get(key) {
                        visit(left, right, &child, differences);
                    } else {
                        differences.push(child);
                    }
                }
                for key in right.keys().filter(|key| !left.contains_key(*key)) {
                    if differences.len() >= 16 {
                        break;
                    }
                    differences.push(format!("{path}.{key}"));
                }
            }
            (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
                for index in 0..left.len().max(right.len()) {
                    if differences.len() >= 16 {
                        break;
                    }
                    let child = format!("{path}[{index}]");
                    match (left.get(index), right.get(index)) {
                        (Some(left), Some(right)) => visit(left, right, &child, differences),
                        _ => differences.push(child),
                    }
                }
            }
            _ => differences.push(path.to_owned()),
        }
    }

    let mut differences = Vec::new();
    visit(left, right, "$", &mut differences);
    differences
}

fn normalize_snapshot_device_relative_timers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(name.as_str(), "age_nanos" | "remaining_nanos") && value.is_number() {
                    *value = serde_json::Value::from(0_u64);
                } else {
                    normalize_snapshot_device_relative_timers(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_snapshot_device_relative_timers(value);
            }
        }
        _ => {}
    }
}

fn production_snapshot_time_identity_evidence(
    artifacts: &SnapshotEpochArtifactSet,
    context: &str,
) -> ProductionSnapshotTimeIdentityEvidence {
    let state_bytes = fs::read(&artifacts.state)
        .unwrap_or_else(|error| panic!("{context} state should read: {error}"));
    let document = HvfNativeSnapshotDocument::decode(&state_bytes)
        .unwrap_or_else(|error| panic!("{context} state should decode: {error}"));
    let canonical = document
        .encode()
        .unwrap_or_else(|error| panic!("{context} state should encode: {error}"));
    assert!(
        canonical == state_bytes,
        "{context} state should use its canonical exact profile"
    );

    let inspection = document
        .inspect_vm_state()
        .to_pretty_json()
        .unwrap_or_else(|error| panic!("{context} state should inspect: {error}"));
    let inspection: serde_json::Value = serde_json::from_str(&inspection)
        .unwrap_or_else(|error| panic!("{context} inspection should parse: {error}"));
    let stable = |field| {
        inspection
            .get(field)
            .cloned()
            .unwrap_or_else(|| panic!("{context} inspection should contain stable {field} state"))
    };
    let mut stable_profile = serde_json::json!({
        "schema": stable("schema"),
        "view": stable("view"),
        "family": stable("family"),
        "profile": stable("profile"),
        "version": stable("version"),
        "machine": stable("machine"),
        "topology": stable("topology"),
        "devices": stable("devices"),
        "diff": stable("diff"),
    });
    normalize_snapshot_device_relative_timers(&mut stable_profile["devices"]);
    let time = inspection
        .get("time")
        .cloned()
        .unwrap_or_else(|| panic!("{context} inspection should contain time state"));
    let vmgenid = time
        .get("vmgenid")
        .and_then(|value| value.get("range"))
        .unwrap_or_else(|| panic!("{context} inspection should contain VMGenID range"));
    assert!(
        vmgenid.get("size").and_then(serde_json::Value::as_u64) == Some(16),
        "{context} VMGenID range should remain exactly 16 bytes"
    );
    let vmgenid_start = vmgenid
        .get("start")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.strip_prefix("0x"))
        .and_then(|value| u64::from_str_radix(value, 16).ok())
        .unwrap_or_else(|| panic!("{context} VMGenID range should use canonical hexadecimal"));

    let structural = decode_snapshot_v2_state(&state_bytes)
        .unwrap_or_else(|error| panic!("{context} structural state should decode: {error}"));
    let memory = load_snapshot_v2_memory_path(&structural, &artifacts.memory)
        .unwrap_or_else(|error| panic!("{context} memory should match its state: {error}"));
    let mut vmgenid_bytes = [0_u8; 16];
    memory
        .read_slice(&mut vmgenid_bytes, GuestAddress::new(vmgenid_start))
        .unwrap_or_else(|error| panic!("{context} VMGenID memory should read: {error}"));
    assert!(
        vmgenid_bytes.iter().any(|byte| *byte != 0),
        "{context} VMGenID should be nonzero"
    );

    ProductionSnapshotTimeIdentityEvidence {
        stable_profile,
        time,
        vmgenid: vmgenid_bytes,
    }
}

fn assert_production_snapshot_time_identity_transition(
    source: &SnapshotEpochArtifactSet,
    recaptured: &SnapshotEpochArtifactSet,
    context: &str,
) {
    let source = production_snapshot_time_identity_evidence(source, context);
    let recaptured = production_snapshot_time_identity_evidence(recaptured, context);
    assert!(
        source.vmgenid != recaptured.vmgenid,
        "{context} contained restore should publish a fresh VMGenID"
    );
    for field in [
        "schema", "view", "family", "profile", "version", "machine", "topology", "devices", "diff",
    ] {
        let source_field = source
            .stable_profile
            .get(field)
            .expect("stable source profile field is constructed above");
        let recaptured_field = recaptured
            .stable_profile
            .get(field)
            .expect("stable recaptured profile field is constructed above");
        assert!(
            source_field == recaptured_field,
            "{context} recapture should preserve stable {field} state; differing paths: {:?}",
            json_difference_paths(source_field, recaptured_field)
        );
    }

    let mut source_time = source.time;
    let mut recaptured_time = recaptured.time;
    for time in [&source_time, &recaptured_time] {
        assert!(
            time.get("rtc_restore_policy")
                .and_then(serde_json::Value::as_str)
                == Some("destination-system-time-reset"),
            "{context} RTC restore policy should remain destination-time reset"
        );
        assert!(
            time.get("vmgenid_restore_policy")
                .and_then(serde_json::Value::as_str)
                == Some("regenerate-and-notify"),
            "{context} VMGenID restore policy should remain regenerate-and-notify"
        );
        assert!(
            time.get("vmclock_restore_policy")
                .and_then(serde_json::Value::as_str)
                == Some("increment-and-notify"),
            "{context} VMClock restore policy should remain increment-and-notify"
        );
        assert!(
            time.get("pvtime_restore_policy")
                .and_then(serde_json::Value::as_str)
                == Some("preserve-cumulative-exclude-downtime"),
            "{context} PVTime policy should continue to exclude snapshot downtime"
        );
        assert!(
            time.get("vmclock_abi")
                .and_then(|value| value.get("algorithm"))
                .and_then(serde_json::Value::as_str)
                == Some("sha256"),
            "{context} VMClock inspection should use the reviewed fingerprint algorithm"
        );
        assert!(
            time.get("vmclock_abi")
                .and_then(|value| value.get("byte_length"))
                .and_then(serde_json::Value::as_u64)
                == Some(112),
            "{context} VMClock ABI should remain exactly 112 bytes"
        );
    }

    let source_vmclock = source_time
        .get("vmclock_abi")
        .and_then(|value| value.get("digest"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("{context} source VMClock fingerprint should exist"));
    let recaptured_vmclock = recaptured_time
        .get("vmclock_abi")
        .and_then(|value| value.get("digest"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("{context} recaptured VMClock fingerprint should exist"));
    assert!(
        source_vmclock != recaptured_vmclock,
        "{context} contained restore should advance VMClock before Paused publication"
    );
    source_time["vmclock_abi"]["digest"] = serde_json::Value::String("<normalized>".to_owned());
    recaptured_time["vmclock_abi"]["digest"] = serde_json::Value::String("<normalized>".to_owned());
    assert!(
        source_time == recaptured_time,
        "{context} recapture should change only the reviewed VMClock ABI fingerprint in canonical time state"
    );
}

#[derive(Debug, Clone, Copy)]
enum SnapshotEpochDeathOrder {
    WorkerFirst,
    LauncherFirst,
}

fn run_snapshot_epoch_paused_death_case(
    bundle: &Path,
    artifacts: SnapshotEpochArtifactSet,
    order: SnapshotEpochDeathOrder,
    baseline_sessions: &[PathBuf],
) -> SnapshotEpochArtifactSet {
    let order_name = match order {
        SnapshotEpochDeathOrder::WorkerFirst => "worker-first",
        SnapshotEpochDeathOrder::LauncherFirst => "launcher-first",
    };
    let fixture = SnapshotEpochInputGrantFixture::new(order_name, artifacts);
    let mut running = spawn_ready_snapshot_epoch_grant_api_launcher(
        bundle,
        &fixture.manifest,
        &fixture.api_socket(),
        fixture.sensitive_strings(),
        &format!("snapshot-epoch-death-{order_name}"),
        false,
    );
    let opened = fixture.replace_source_pathnames();
    configure_snapshot_epoch_destination_metrics(
        &running,
        &format!("{order_name} death destination"),
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/snapshot/load",
            &snapshot_load_body(false),
        ),
        204,
        "load rootless MMIO epoch snapshot before process death",
    );
    assert!(
        http_get(&running.socket, "/").contains(r#""state":"Paused""#),
        "{order_name} death destination should publish Paused"
    );
    let state_before = fs::read(&opened.state).expect("death-case state should read");
    let memory_before = fs::read(&opened.memory).expect("death-case memory should read");
    let blocks = opened
        .blocks
        .as_ref()
        .expect("rootless MMIO death case should retain mixed block artifacts");
    let root_before = fs::read(&blocks.root).expect("death-case primary should read");
    let data_before = fs::read(&blocks.data).expect("death-case data should read");
    let audit_before = fs::read(&blocks.audit).expect("death-case audit should read");
    let writable_pmem_before =
        fs::read(&opened.writable_pmem).expect("death-case writable pmem should read");
    let read_only_pmem_before =
        fs::read(&opened.read_only_pmem).expect("death-case read-only pmem should read");
    assert_eq!(session_entries().len(), baseline_sessions.len() + 1);

    match order {
        SnapshotEpochDeathOrder::WorkerFirst => {
            let worker = only_worker_pid(&running.child);
            // SAFETY: The worker is the one live child of this unreaped launcher.
            assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
            assert_eq!(
                running.wait("profile-3 worker-first death").code(),
                Some(128 + libc::SIGKILL)
            );
        }
        SnapshotEpochDeathOrder::LauncherFirst => {
            let worker = only_worker_pid(&running.child);
            let worker_exit = ProcessExitWatch::new(worker);
            let launcher = i32::try_from(running.child.id()).expect("launcher PID should fit");
            // SAFETY: The unreaped launcher owns this PID and its worker
            // observes authenticated lifecycle EOF independently.
            assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
            assert_eq!(
                running.wait("profile-3 launcher-first death").signal(),
                Some(libc::SIGKILL)
            );
            assert!(
                worker_exit.wait(PROCESS_TIMEOUT),
                "profile-3 worker should observe launcher death"
            );
        }
    }

    assert_eq!(session_entries(), baseline_sessions);
    assert!(!running.socket.exists());
    assert_eq!(
        fs::read(&opened.state).expect("death-case state should remain"),
        state_before
    );
    assert_eq!(
        fs::read(&opened.memory).expect("death-case memory should remain"),
        memory_before
    );
    assert_eq!(
        fs::read(&blocks.root).expect("death-case primary should remain"),
        root_before
    );
    assert_eq!(
        fs::read(&blocks.data).expect("death-case data should remain"),
        data_before
    );
    assert_eq!(
        fs::read(&blocks.audit).expect("death-case audit should remain"),
        audit_before
    );
    assert_eq!(
        fs::read(&opened.writable_pmem).expect("death-case writable pmem should remain"),
        writable_pmem_before
    );
    assert_eq!(
        fs::read(&opened.read_only_pmem).expect("death-case read-only pmem should remain"),
        read_only_pmem_before
    );
    assert_no_snapshot_staging(&fixture.state_directory);
    assert_no_snapshot_staging(&fixture.memory_directory);
    opened
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
            false,
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
    let logger = fixture.devices.add_logger_grant("guest-vsock");
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
    logger.replace_source_pathname();
    assert_socket_mode(&fixture.api_socket(), 0o600, "granted API socket");
    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "launcher-owned API publication must not leave a worker helper"
    );

    assert_http_status(
        &http_put(
            &running.socket,
            "/logger",
            &serde_json::json!({"log_path": OUTPUT_LOGGER_REF}).to_string(),
        ),
        204,
        "PUT granted-vsock logger grant",
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
        stream
            .set_nonblocking(true)
            .expect("accepted vsock stream should remain nonblocking");
        streams.push((path, Some(listener), stream));
    }

    for ((path, listener, stream), &(_, guest_payload, _)) in
        streams.iter_mut().zip(GRANTED_VSOCK_EXCHANGES)
    {
        let mut received = vec![0_u8; guest_payload.len()];
        read_exact_nonblocking(stream, &mut received, PROCESS_TIMEOUT)
            .expect("guest vsock payload should arrive");
        assert_eq!(received, guest_payload);
        // The exact payload proves the launcher's pathname identity check and
        // SCM_RIGHTS handoff completed before pathname authority is withdrawn.
        drop(listener.take());
        fs::remove_file(path).expect("host-owned vsock port path should clean up");
    }
    for ((_, _, stream), &(_, _, host_payload)) in streams.iter_mut().zip(GRANTED_VSOCK_EXCHANGES) {
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
    let mut forbidden = fixture.sensitive_strings();
    forbidden.push(
        std::str::from_utf8(GRANTED_VSOCK_MARKER)
            .expect("vsock marker should be UTF-8")
            .to_owned(),
    );
    for (_, guest_payload, host_payload) in GRANTED_VSOCK_EXCHANGES {
        forbidden.push(String::from_utf8_lossy(guest_payload).into_owned());
        forbidden.push(String::from_utf8_lossy(host_payload).into_owned());
    }
    logger.assert_records(
        &["device-kind=vsock operation=tx outcome=succeeded"],
        forbidden,
    );
}

#[test]
fn normal_bundle_brokers_multiple_contained_vhost_user_children_without_helpers() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new_with_vhost_user("vhost-user");
    let metrics = fixture.devices.add_metrics_grant("vhost-user");
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
        VhostUserBlockBackendOptions::regular(true).with_metrics_delays(VHOST_USER_METRICS_DELAY),
    )
    .expect("contained vhost root backend should start");
    let scratch_backend = VhostUserBlockBackend::start(
        &scratch_socket,
        &scratch_backing,
        VhostUserBlockBackendOptions::regular(false).with_metrics_delays(VHOST_USER_METRICS_DELAY),
    )
    .expect("contained vhost scratch backend should start");

    let mut running = spawn_ready_socket_grant_api_launcher(&bundle, &fixture, "vhost-user");
    metrics.replace_source_pathname();
    metrics.configure(&running.socket, "contained vhost-user");
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
    assert_http_status(
        &http_put(
            &running.socket,
            "/hotplug/memory",
            r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
        ),
        204,
        "PUT contained-vhost memory hotplug config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.vhost-user-block=ro bangbang.expect-vhost-resize=1",
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
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
        ),
        204,
        "grow contained-vhost guest memory",
    );
    let grown_memory = wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":128"#,
        PROCESS_TIMEOUT,
    )
    .expect("contained-vhost guest should complete memory grow");
    assert_http_status(&grown_memory, 200, "GET grown contained-vhost memory");
    assert_vhost_user_memory_aperture(&root_backend.report(), "contained root");
    assert_vhost_user_memory_aperture(&scratch_backend.report(), "contained scratch");
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
    metrics.assert_vhost_user_metrics(
        &running.socket,
        &[("rootfs", false), ("scratch", true)],
        "contained MMIO vhost-user lifecycle",
        fixture.sensitive_strings(),
    );
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":0}"#,
        ),
        204,
        "shrink contained-vhost guest memory",
    );
    let shrunk_memory = wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":0"#,
        PROCESS_TIMEOUT,
    )
    .expect("contained-vhost guest should complete memory shrink");
    assert_http_status(&shrunk_memory, 200, "GET shrunk contained-vhost memory");
    assert_vhost_user_memory_aperture(&root_backend.report(), "contained root after shrink");
    assert_vhost_user_memory_aperture(&scratch_backend.report(), "contained scratch after shrink");
    assert!(
        child_pids(worker).is_empty(),
        "contained vhost plus dynamic memory must not retain a helper"
    );

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
fn normal_bundle_certifies_aggregate_storage_semantics_through_contained_grants() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new_with_vhost_user("storage-certification");
    let logger = fixture.devices.add_logger_grant("storage-certification");
    let metrics = fixture.devices.add_metrics_grant("storage-certification");
    let vhost_socket = fixture.vhost_user_socket(VHOST_USER_SOCKET_CHILD_ONE);
    let vhost_backing = fixture.vhost_user_backing("storage-certification-vhost.img");

    resize_and_write_file_marker_at(
        &fixture.devices.data,
        16 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        STORAGE_CONTROL_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.replacement,
        8 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        STORAGE_ASYNC_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.hotplug_reuse,
        8 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        STORAGE_ASYNC_REPLACEMENT_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.storage_block_one,
        8 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        STORAGE_RUNTIME_BLOCK_ONE_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.storage_block_two,
        8 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        STORAGE_RUNTIME_BLOCK_TWO_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.pmem,
        PMEM_BACKING_LEN,
        0,
        STORAGE_PMEM_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.pmem_reuse,
        PMEM_BACKING_LEN,
        0,
        STORAGE_RUNTIME_PMEM_ONE_HOST_MARKER,
    );
    resize_and_write_file_marker_at(
        &fixture.devices.storage_pmem,
        PMEM_BACKING_LEN,
        0,
        STORAGE_RUNTIME_PMEM_TWO_HOST_MARKER,
    );
    create_sized_file(&vhost_backing, 8 * VIRTIO_BLOCK_SECTOR_BYTES);
    resize_and_write_file_marker_at(
        &vhost_backing,
        8 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        STORAGE_VHOST_HOST_MARKER,
    );
    let vhost_backend = VhostUserBlockBackend::start(
        &vhost_socket,
        &vhost_backing,
        VhostUserBlockBackendOptions::regular(false).with_metrics_delays(VHOST_USER_METRICS_DELAY),
    )
    .expect("contained aggregate vhost-user backend should start");

    let mut running = spawn_ready_socket_grant_api_launcher_with_extra_args(
        &bundle,
        &fixture,
        "storage-certification",
        &["--enable-pci"],
    );
    fixture.devices.replace_source_pathnames();
    logger.replace_source_pathname();
    metrics.replace_source_pathname();
    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "contained aggregate setup must not retain a broker helper",
    );

    assert_http_status(
        &http_put(
            &running.socket,
            "/logger",
            &serde_json::json!({"log_path": OUTPUT_LOGGER_REF}).to_string(),
        ),
        204,
        "PUT contained aggregate logger grant",
    );
    metrics.configure(&running.socket, "contained aggregate storage");

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained aggregate machine config",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/hotplug/memory",
            r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
        ),
        204,
        "PUT contained aggregate memory hotplug config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": DIRECT_ROOTFS_STORAGE_CERTIFICATION_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source).expect("aggregate boot source should serialize"),
        ),
        204,
        "PUT contained aggregate boot source",
    );
    for (path, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": GUEST_ROOTFS_REF,
                "is_root_device": true,
                "is_read_only": true,
                "io_engine": "Sync",
            }),
            "PUT contained aggregate read-only Sync rootfs",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "path_on_host": GUEST_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Sync",
            }),
            "PUT contained aggregate Sync control",
        ),
        (
            "/drives/asyncdata",
            serde_json::json!({
                "drive_id": "asyncdata",
                "path_on_host": GUEST_REPLACEMENT_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Async",
            }),
            "PUT contained aggregate Async data",
        ),
        (
            "/drives/vhostdata",
            serde_json::json!({
                "drive_id": "vhostdata",
                "socket": VHOST_USER_SOCKET_REF_ONE,
                "is_root_device": false,
                "cache_type": "Writeback",
            }),
            "PUT contained aggregate vhost-user data",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("aggregate drive should serialize"),
            ),
            204,
            context,
        );
    }
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/pmem0",
            &serde_json::json!({
                "id": "pmem0",
                "path_on_host": GUEST_PMEM_REF,
                "read_only": false,
            })
            .to_string(),
        ),
        204,
        "PUT contained aggregate startup pmem",
    );
    let configured = http_get(&running.socket, "/vm/config");
    assert_http_status(&configured, 200, "GET contained aggregate startup config");
    for expected in [
        GUEST_ROOTFS_REF,
        GUEST_DATA_REF,
        GUEST_REPLACEMENT_REF,
        GUEST_PMEM_REF,
        VHOST_USER_SOCKET_REF_ONE,
        r#""io_engine":"Sync""#,
        r#""io_engine":"Async""#,
    ] {
        assert!(
            configured.contains(expected),
            "contained aggregate config should contain {expected:?}: {configured}",
        );
    }
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained aggregate storage guest",
    );
    wait_for_file_marker_at(
        &fixture.devices.opened_data,
        STORAGE_READY_OFFSET,
        STORAGE_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate guest should complete initial storage I/O");
    vhost_backend
        .wait_for_activation(PROCESS_TIMEOUT)
        .expect("contained aggregate vhost-user backend should activate");
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_data,
            STORAGE_CONTROL_GUEST_OFFSET,
            STORAGE_CONTROL_GUEST_MARKER.len(),
        ),
        STORAGE_CONTROL_GUEST_MARKER,
    );
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_replacement,
            STORAGE_CONTROL_GUEST_OFFSET,
            STORAGE_ASYNC_GUEST_MARKER.len(),
        ),
        STORAGE_ASYNC_GUEST_MARKER,
    );
    assert_eq!(
        file_bytes_at(
            &vhost_backing,
            STORAGE_CONTROL_GUEST_OFFSET,
            STORAGE_VHOST_GUEST_MARKER.len(),
        ),
        STORAGE_VHOST_GUEST_MARKER,
    );
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_pmem,
            PMEM_GUEST_FLUSH_OFFSET,
            STORAGE_PMEM_GUEST_MARKER.len(),
        ),
        STORAGE_PMEM_GUEST_MARKER,
    );
    vhost_backend
        .wait_for_flush(PROCESS_TIMEOUT)
        .expect("contained aggregate vhost-user write should flush");

    OpenOptions::new()
        .write(true)
        .open(&vhost_backing)
        .expect("contained aggregate vhost backing should reopen")
        .set_len(16 * VIRTIO_BLOCK_SECTOR_BYTES)
        .expect("contained aggregate vhost backing should resize");
    let (async_patch, pmem_patch, vhost_patch) = thread::scope(|scope| {
        let async_patch = scope.spawn(|| {
            http_request(
                &running.socket,
                "PATCH",
                "/drives/asyncdata",
                r#"{"drive_id":"asyncdata","rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
            )
        });
        let pmem_patch = scope.spawn(|| {
            http_request(
                &running.socket,
                "PATCH",
                "/pmem/pmem0",
                r#"{"id":"pmem0","rate_limiter":{"ops":{"size":3,"one_time_burst":1,"refill_time":100}}}"#,
            )
        });
        let vhost_patch = scope.spawn(|| {
            http_request(
                &running.socket,
                "PATCH",
                "/drives/vhostdata",
                r#"{"drive_id":"vhostdata"}"#,
            )
        });
        (
            async_patch
                .join()
                .expect("contained Async PATCH should join"),
            pmem_patch.join().expect("contained pmem PATCH should join"),
            vhost_patch
                .join()
                .expect("contained vhost PATCH should join"),
        )
    });
    assert_http_status(&async_patch, 204, "concurrent contained Async PATCH");
    assert_http_status(&pmem_patch, 204, "concurrent contained pmem PATCH");
    assert_http_status(&vhost_patch, 204, "concurrent contained vhost PATCH");
    assert_eq!(vhost_backend.report().config_requests, 2);
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
        ),
        204,
        "grow contained aggregate memory",
    );
    let grown = wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":128"#,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate memory should grow");
    assert_http_status(&grown, 200, "GET grown contained aggregate memory");

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained aggregate guest before replacement",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/asyncdata",
            &serde_json::json!({
                "drive_id": "asyncdata",
                "path_on_host": GUEST_HOTPLUG_REUSE_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Async",
                "rate_limiter": {"ops": {"size": 4, "one_time_burst": 1, "refill_time": 100}},
            })
            .to_string(),
        ),
        204,
        "replace contained aggregate Async backing",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/runtime_block",
            &serde_json::json!({
                "drive_id": "runtime_block",
                "path_on_host": GUEST_STORAGE_BLOCK_ONE_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Sync",
            })
            .to_string(),
        ),
        204,
        "PUT first contained aggregate runtime block",
    );
    let replaced = http_get(&running.socket, "/vm/config");
    assert_http_status(&replaced, 200, "GET contained aggregate replaced config");
    assert!(replaced.contains(GUEST_HOTPLUG_REUSE_REF));
    assert!(!replaced.contains(GUEST_REPLACEMENT_REF));
    resize_and_write_file_marker_at(
        &fixture.devices.opened_data,
        16 * VIRTIO_BLOCK_SECTOR_BYTES,
        STORAGE_CONTINUE_ONE_OFFSET,
        STORAGE_CONTINUE_ONE_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained aggregate first block round",
    );
    wait_for_file_marker_at(
        &fixture.devices.opened_data,
        STORAGE_FIRST_REMOVED_OFFSET,
        STORAGE_FIRST_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate first block should leave the guest");
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_hotplug_reuse,
            STORAGE_CONTROL_GUEST_OFFSET,
            STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER.len(),
        ),
        STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER,
    );
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_storage_block_one,
            STORAGE_CONTROL_GUEST_OFFSET,
            STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER.len(),
        ),
        STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER,
    );

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained aggregate before block reuse",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/runtime_block", ""),
        204,
        "DELETE first contained aggregate runtime block",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/runtime_block_two",
            &serde_json::json!({
                "drive_id": "runtime_block_two",
                "path_on_host": GUEST_STORAGE_BLOCK_TWO_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Sync",
            })
            .to_string(),
        ),
        204,
        "PUT reused contained aggregate runtime block",
    );
    resize_and_write_file_marker_at(
        &fixture.devices.opened_data,
        16 * VIRTIO_BLOCK_SECTOR_BYTES,
        STORAGE_CONTINUE_TWO_OFFSET,
        STORAGE_CONTINUE_TWO_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained aggregate reused block round",
    );
    wait_for_file_marker_at(
        &fixture.devices.opened_data,
        STORAGE_SECOND_BLOCK_REMOVED_OFFSET,
        STORAGE_SECOND_BLOCK_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate reused block should preserve its PCI slot");
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_storage_block_two,
            STORAGE_CONTROL_GUEST_OFFSET,
            STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER.len(),
        ),
        STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER,
    );

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained aggregate before first pmem",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/runtime_block_two", ""),
        204,
        "DELETE reused contained aggregate runtime block",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/runtime_pmem",
            &serde_json::json!({
                "id": "runtime_pmem",
                "path_on_host": GUEST_PMEM_REUSE_REF,
                "read_only": false,
            })
            .to_string(),
        ),
        204,
        "PUT first contained aggregate runtime pmem",
    );
    resize_and_write_file_marker_at(
        &fixture.devices.opened_data,
        16 * VIRTIO_BLOCK_SECTOR_BYTES,
        STORAGE_CONTINUE_PMEM_ONE_OFFSET,
        STORAGE_CONTINUE_PMEM_ONE_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained aggregate first pmem round",
    );
    wait_for_file_marker_at(
        &fixture.devices.opened_data,
        STORAGE_FIRST_PMEM_REMOVED_OFFSET,
        STORAGE_FIRST_PMEM_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate first pmem should leave the guest");
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_pmem_reuse,
            PMEM_GUEST_FLUSH_OFFSET,
            STORAGE_RUNTIME_PMEM_ONE_GUEST_MARKER.len(),
        ),
        STORAGE_RUNTIME_PMEM_ONE_GUEST_MARKER,
    );

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained aggregate before pmem reuse",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/pmem/runtime_pmem", ""),
        204,
        "DELETE first contained aggregate runtime pmem",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/pmem/runtime_pmem_two",
            &serde_json::json!({
                "id": "runtime_pmem_two",
                "path_on_host": GUEST_STORAGE_PMEM_REF,
                "read_only": false,
                "rate_limiter": {"ops": {"size": 5, "refill_time": 100}},
            })
            .to_string(),
        ),
        204,
        "PUT reused contained aggregate runtime pmem",
    );
    resize_and_write_file_marker_at(
        &fixture.devices.opened_data,
        16 * VIRTIO_BLOCK_SECTOR_BYTES,
        STORAGE_CONTINUE_PMEM_TWO_OFFSET,
        STORAGE_CONTINUE_PMEM_TWO_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained aggregate reused pmem round",
    );
    wait_for_file_marker_at(
        &fixture.devices.opened_data,
        STORAGE_SUCCESS_OFFSET,
        STORAGE_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate guest should complete storage certification");
    assert_eq!(
        file_bytes_at(
            &fixture.devices.opened_storage_pmem,
            PMEM_GUEST_FLUSH_OFFSET,
            STORAGE_RUNTIME_PMEM_TWO_GUEST_MARKER.len(),
        ),
        STORAGE_RUNTIME_PMEM_TWO_GUEST_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/pmem/runtime_pmem_two", ""),
        204,
        "final DELETE contained aggregate runtime pmem",
    );
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":0}"#,
        ),
        204,
        "shrink contained aggregate memory",
    );
    let shrunk = wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":0"#,
        PROCESS_TIMEOUT,
    )
    .expect("contained aggregate memory should shrink");
    assert_http_status(&shrunk, 200, "GET shrunk contained aggregate memory");
    let final_config = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &final_config,
        200,
        "GET final contained aggregate storage config",
    );
    for expected in [
        GUEST_ROOTFS_REF,
        GUEST_DATA_REF,
        GUEST_HOTPLUG_REUSE_REF,
        GUEST_PMEM_REF,
        VHOST_USER_SOCKET_REF_ONE,
        r#""drive_id":"asyncdata""#,
        r#""id":"pmem0""#,
    ] {
        assert!(
            final_config.contains(expected),
            "final contained aggregate config should contain {expected:?}: {final_config}",
        );
    }
    for removed in [
        GUEST_REPLACEMENT_REF,
        GUEST_STORAGE_BLOCK_ONE_REF,
        GUEST_STORAGE_BLOCK_TWO_REF,
        GUEST_PMEM_REUSE_REF,
        GUEST_STORAGE_PMEM_REF,
        r#""drive_id":"runtime_block""#,
        r#""drive_id":"runtime_block_two""#,
        r#""id":"runtime_pmem""#,
        r#""id":"runtime_pmem_two""#,
    ] {
        assert!(
            !final_config.contains(removed),
            "final contained aggregate config must omit removed or replaced storage {removed:?}: {final_config}",
        );
    }
    assert_aggregate_storage_vhost_user_memory_aperture(&vhost_backend.report());
    assert!(
        child_pids(worker).is_empty(),
        "contained aggregate storage must not retain a helper",
    );
    metrics.assert_vhost_user_metrics(
        &running.socket,
        &[("vhostdata", true)],
        "contained PCI aggregate vhost-user lifecycle",
        fixture
            .sensitive_strings()
            .into_iter()
            .chain([path_text(&vhost_backing).to_owned()]),
    );

    stop_running_launcher(&mut running, "contained aggregate storage shutdown");
    vhost_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("contained aggregate vhost frontend should close orderly");
    let report = vhost_backend
        .finish()
        .expect("contained aggregate vhost backend should finish");
    assert!(report.activated && report.frontend_closed);
    assert!(report.reads > 0 && report.writes > 0 && report.flushes > 0);
    assert!(!vhost_socket.exists());
    assert!(!fixture.api_socket().exists());
    assert!(session_entries().is_empty());
    logger.assert_records(
        &[
            "device-kind=block operation=request outcome=succeeded",
            "device-kind=pmem operation=flush outcome=succeeded",
        ],
        fixture.sensitive_strings().into_iter().chain([
            path_text(&vhost_backing).to_owned(),
            std::str::from_utf8(STORAGE_CONTROL_GUEST_MARKER)
                .expect("storage marker should be UTF-8")
                .to_owned(),
            std::str::from_utf8(STORAGE_PMEM_GUEST_MARKER)
                .expect("pmem marker should be UTF-8")
                .to_owned(),
        ]),
    );
    for (path, marker) in [
        (&fixture.devices.data, STORAGE_CONTROL_GUEST_MARKER),
        (&fixture.devices.replacement, STORAGE_ASYNC_GUEST_MARKER),
        (
            &fixture.devices.hotplug_reuse,
            STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER,
        ),
        (
            &fixture.devices.storage_block_one,
            STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER,
        ),
        (
            &fixture.devices.storage_block_two,
            STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER,
        ),
        (&fixture.devices.pmem, STORAGE_PMEM_GUEST_MARKER),
        (
            &fixture.devices.pmem_reuse,
            STORAGE_RUNTIME_PMEM_ONE_GUEST_MARKER,
        ),
        (
            &fixture.devices.storage_pmem,
            STORAGE_RUNTIME_PMEM_TWO_GUEST_MARKER,
        ),
    ] {
        assert!(
            !fs::read(path)
                .expect("planted replacement backing should read")
                .windows(marker.len())
                .any(|window| window == marker),
            "contained aggregate guest marker must remain on the launcher-opened object",
        );
    }
}

#[test]
fn normal_bundle_retries_hotplugs_deletes_and_reuses_contained_vhost_user_block() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new_with_vhost_user("vhost-user-runtime");
    let metrics = fixture.devices.add_metrics_grant("vhost-user-runtime");
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
    metrics.replace_source_pathname();
    metrics.configure(&running.socket, "contained runtime vhost-user");
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
    assert_http_status(
        &http_put(
            &running.socket,
            "/hotplug/memory",
            r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
        ),
        204,
        "PUT contained runtime-vhost memory hotplug config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "boot_args": DIRECT_ROOTFS_VHOST_BLOCK_HOTPLUG_BOOT_ARGS,
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
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
        ),
        204,
        "grow memory before contained runtime-vhost insertion",
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":128"#,
        PROCESS_TIMEOUT,
    )
    .expect("guest should grow memory before contained runtime-vhost insertion");
    assert_vhost_user_memory_aperture(&control_backend.report(), "contained runtime control");

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
    assert!(
        rejected_response.contains("vhost-user backend lacks configuration protocol support"),
        "response:\n{rejected_response}"
    );
    assert!(!rejected_response.contains(VHOST_USER_SOCKET_REF_TWO));
    assert!(!http_get(&running.socket, "/vm/config").contains(r#""drive_id":"rejected""#));
    let rejected_report = rejected_backend
        .finish()
        .expect("rejecting contained runtime-vhost backend should finish");
    assert!(rejected_report.discovery_rejected);

    let first_backend = VhostUserBlockBackend::start(
        &first_socket,
        &first_backing,
        VhostUserBlockBackendOptions::regular(false).with_metrics_delays(VHOST_USER_METRICS_DELAY),
    )
    .expect("first contained runtime-vhost backend should start");
    let second_backend = VhostUserBlockBackend::start(
        &second_socket,
        &second_backing,
        VhostUserBlockBackendOptions::regular(false).with_metrics_delays(VHOST_USER_METRICS_DELAY),
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
    assert_vhost_user_memory_aperture(&first_backend.report(), "first contained runtime block");
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/drives/hotdata",
            r#"{"drive_id":"hotdata"}"#,
        ),
        204,
        "PATCH first contained runtime-vhost metrics generation",
    );
    assert_eq!(
        first_backend.report().config_requests,
        2,
        "first contained runtime generation should receive discovery and refresh requests"
    );
    metrics.assert_vhost_user_metrics(
        &running.socket,
        &[("hotdata", true)],
        "first contained runtime vhost-user generation",
        fixture.sensitive_strings(),
    );
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
    assert_vhost_user_memory_aperture(&second_backend.report(), "reused contained runtime block");
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
    metrics.assert_vhost_user_metrics(
        &running.socket,
        &[("hotdata", false)],
        "reused contained runtime vhost-user generation",
        fixture.sensitive_strings(),
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "final DELETE contained runtime-vhost block",
    );
    second_backend
        .wait_for_frontend_close(PROCESS_TIMEOUT)
        .expect("reused contained runtime-vhost frontend should close after DELETE");
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":0}"#,
        ),
        204,
        "shrink memory after contained runtime-vhost reuse",
    );
    wait_for_http_response_fragment(
        &running.socket,
        "/hotplug/memory",
        r#""plugged_size_mib":0"#,
        PROCESS_TIMEOUT,
    )
    .expect("guest should shrink memory after contained runtime-vhost reuse");
    assert_vhost_user_memory_aperture(
        &control_backend.report(),
        "contained runtime control after shrink",
    );
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

    let vm_config_before_metrics = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &vm_config_before_metrics,
        200,
        "GET VM config before granted metrics",
    );
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
    let vm_config_after_metrics = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &vm_config_after_metrics,
        200,
        "GET VM config after granted metrics",
    );
    assert_eq!(vm_config_after_metrics, vm_config_before_metrics);
    for private in fixture.sensitive_strings().into_iter().chain([
        r#""metrics""#.to_owned(),
        "private resource grant failed".to_owned(),
        "descriptor".to_owned(),
    ]) {
        assert!(
            !vm_config_after_metrics.contains(&private),
            "VM config leaked contained metrics authority: {private}"
        );
    }
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
fn normal_bundle_certifies_metrics_schema_across_real_periodic_and_terminal_lifecycle() {
    let bundle = production_bundle();
    let fixture = OutputGrantFixture::new("metrics-schema-lifecycle");
    let mut running = spawn_ready_serial_snapshot_grant_api_launcher(
        &bundle,
        &fixture.manifest,
        "metrics-schema-lifecycle",
        false,
    );
    fixture.replace_source_pathnames();

    assert_http_status(
        &http_put(
            &running.socket,
            "/metrics",
            &serde_json::json!({"metrics_path": OUTPUT_METRICS_REF}).to_string(),
        ),
        204,
        "PUT lifecycle metrics grant",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT lifecycle metrics machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "initrd_path": path_text(&resources.join("guest-initrd")),
        "boot_args": GUEST_SERIAL_RX_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(&running.socket, "/boot-source", &boot_source.to_string()),
        204,
        "PUT lifecycle metrics boot source",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start lifecycle metrics guest",
    );
    running
        .wait_for_stdout_marker(GUEST_SERIAL_RX_READY_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| panic!("lifecycle metrics guest should become ready: {error}"));

    let initial = wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        1,
        PROCESS_TIMEOUT,
        "initial",
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause lifecycle metrics guest",
    );
    assert!(http_get(&running.socket, "/").contains(r#""state":"Paused""#));

    let paused_periodic = wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        2,
        REAL_PERIODIC_METRICS_TIMEOUT,
        "real periodic while Paused",
    );
    assert_real_periodic_metrics_spacing(&paused_periodic, 0, 1, "Paused");
    assert!(http_get(&running.socket, "/").contains(r#""state":"Paused""#));

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume lifecycle metrics guest",
    );
    assert!(http_get(&running.socket, "/").contains(r#""state":"Running""#));
    let running_periodic = wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        3,
        REAL_PERIODIC_METRICS_TIMEOUT,
        "real periodic while Running",
    );
    assert_real_periodic_metrics_spacing(&running_periodic, 1, 2, "Running");

    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        ),
        204,
        "explicit lifecycle metrics flush",
    );
    wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        4,
        PROCESS_TIMEOUT,
        "explicit",
    );

    let launcher_pid = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: This targets the live unreaped launcher owned by the test and
    // exercises its ordinary cancellation and terminal-observability path.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGTERM) }, 0);
    let (status, stdout, stderr) = running.wait("lifecycle metrics graceful stop");
    assert!(
        status.success(),
        "lifecycle metrics launcher should stop cleanly: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let terminal = wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        5,
        PROCESS_TIMEOUT,
        "terminal",
    );
    assert_eq!(
        initial[0]["utc_timestamp_ms"],
        terminal[0]["utc_timestamp_ms"]
    );
    assert!(!running.socket.exists());
    assert!(session_entries().is_empty());
    for sensitive in fixture.sensitive_strings() {
        assert!(
            !stdout.contains(&sensitive),
            "stdout leaked metrics authority"
        );
        assert!(
            !stderr.contains(&sensitive),
            "stderr leaked metrics authority"
        );
    }
    assert_eq!(
        fs::read(&fixture.opened_logger).expect("unclaimed logger grant should read"),
        OUTPUT_LOGGER_SEED,
    );
    assert_eq!(
        fs::read(&fixture.opened_serial).expect("unclaimed serial grant should read"),
        OUTPUT_SERIAL_SEED,
    );
    fixture.assert_replacement_outputs_unchanged();
}

#[test]
fn normal_bundle_streams_default_serial_stdio_across_launcher_worker_boundary() {
    let bundle = production_bundle();
    let mut running = spawn_ready_serial_api_launcher(&bundle, "serial-stdio");
    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT bundle serial stdio machine config",
    );
    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "initrd_path": path_text(&resources.join("guest-initrd")),
        "boot_args": GUEST_SERIAL_RX_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source)
                .expect("bundle serial boot source should serialize"),
        ),
        204,
        "PUT bundle serial stdio boot source",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start bundle serial stdio guest",
    );
    running
        .wait_for_stdout_marker(GUEST_SERIAL_RX_READY_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| panic!("bundle serial receiver should become ready: {error}"));

    let mut serial_input = b"BANGBANG_SERIAL_RX_".to_vec();
    serial_input.extend(std::iter::repeat_n(b'A', 80));
    serial_input.extend_from_slice(b"_END\n");
    running.write_stdin(&serial_input);
    running
        .wait_for_stdout_marker(GUEST_SERIAL_RX_SUCCESS_MARKER, PROCESS_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("bundle worker should receive the full launcher stdin stream: {error}")
        });
    assert!(
        !running
            .stdout_snapshot()
            .contains(GUEST_SERIAL_RX_FAILURE_MARKER)
    );
    running.close_stdin();
    thread::sleep(Duration::from_millis(100));
    assert_http_status(
        &http_get(&running.socket, "/"),
        200,
        "bundle API after serial stdin EOF",
    );

    let launcher_pid = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: This targets the live unreaped launcher owned by the test.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGTERM) }, 0);
    let (status, stdout, stderr) = running.wait("bundle serial stdio graceful stop");
    assert!(
        status.success(),
        "bundle serial stdio should stop cleanly: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains(GUEST_SERIAL_RX_READY_MARKER));
    assert!(stdout.contains(GUEST_SERIAL_RX_SUCCESS_MARKER));
    assert!(!stdout.contains(GUEST_SERIAL_RX_FAILURE_MARKER));
    assert!(!running.socket.exists());
    assert!(session_entries().is_empty());
}

#[test]
fn normal_bundle_isolates_concurrent_default_serial_stdio_sessions() {
    let bundle = production_bundle();
    let mut first = spawn_ready_serial_api_launcher(&bundle, "concurrent-serial-a");
    let mut second = spawn_ready_serial_api_launcher(&bundle, "concurrent-serial-b");
    assert_eq!(session_entries().len(), 2);

    let configure_and_start = |running: &RunningSerialApiLauncher, name: &str| {
        assert_http_status(
            &http_put(
                &running.socket,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            204,
            &format!("PUT {name} serial machine config"),
        );
        let resources = worker_bundle(&bundle).join("Contents/Resources");
        let boot_source = serde_json::json!({
            "kernel_image_path": path_text(&resources.join("guest-kernel")),
            "initrd_path": path_text(&resources.join("guest-initrd")),
            "boot_args": GUEST_SERIAL_RX_BOOT_ARGS,
        });
        assert_http_status(
            &http_put(
                &running.socket,
                "/boot-source",
                &serde_json::to_string(&boot_source)
                    .expect("concurrent serial boot source should serialize"),
            ),
            204,
            &format!("PUT {name} serial boot source"),
        );
        assert_http_status(
            &http_put(
                &running.socket,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            204,
            &format!("start {name} serial guest"),
        );
        running
            .wait_for_stdout_marker(GUEST_SERIAL_RX_READY_MARKER, PROCESS_TIMEOUT)
            .unwrap_or_else(|error| panic!("{name} serial receiver should become ready: {error}"));
    };
    configure_and_start(&first, "first concurrent");
    configure_and_start(&second, "second concurrent");

    let first_worker = only_worker_pid(&first.child);
    let second_worker = only_worker_pid(&second.child);
    assert_ne!(first_worker, second_worker);
    assert!(
        child_pids(first_worker).is_empty(),
        "first serial worker must not retain a binder or broker helper"
    );
    assert!(
        child_pids(second_worker).is_empty(),
        "second serial worker must not retain a binder or broker helper"
    );

    assert_http_status(
        &http_request(&first.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause first concurrent production serial guest",
    );
    let mut serial_input = b"BANGBANG_SERIAL_RX_".to_vec();
    serial_input.extend(std::iter::repeat_n(b'A', 80));
    serial_input.extend_from_slice(b"_END\n");

    second.write_stdin(&serial_input);
    second
        .wait_for_stdout_marker(GUEST_SERIAL_RX_SUCCESS_MARKER, PROCESS_TIMEOUT)
        .expect("second concurrent production serial guest should receive its input");
    assert!(
        !first
            .stdout_snapshot()
            .contains(GUEST_SERIAL_RX_SUCCESS_MARKER),
        "the paused first serial session must not observe second-session progress"
    );

    first.write_stdin(&serial_input);
    thread::sleep(Duration::from_millis(200));
    assert!(
        !first
            .stdout_snapshot()
            .contains(GUEST_SERIAL_RX_SUCCESS_MARKER),
        "the paused first serial session must not consume its queued input"
    );
    assert_http_status(
        &http_request(&first.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume first concurrent production serial guest",
    );
    first
        .wait_for_stdout_marker(GUEST_SERIAL_RX_SUCCESS_MARKER, PROCESS_TIMEOUT)
        .expect("first concurrent production serial guest should consume its own input");

    for (running, name) in [(&first, "first"), (&second, "second")] {
        let stdout = running.stdout_snapshot();
        assert_eq!(
            stdout.matches(GUEST_SERIAL_RX_SUCCESS_MARKER).count(),
            1,
            "{name} serial session should publish exactly one success marker"
        );
        assert!(!stdout.contains(GUEST_SERIAL_RX_FAILURE_MARKER));
        for expected in [
            "operation=server outcome=running\n",
            "operation=process-startup outcome=running\n",
            "action=request outcome=no-content\n",
        ] {
            assert!(
                stdout.contains(expected),
                "{name} production stdout should multiplex {expected:?}: {stdout}"
            );
        }
    }
    second.close_stdin();
    thread::sleep(Duration::from_millis(100));
    assert_http_status(
        &http_get(&second.socket, "/"),
        200,
        "second concurrent serial API after stdin EOF",
    );
    first.close_stdin();
    thread::sleep(Duration::from_millis(100));
    assert_http_status(
        &http_get(&first.socket, "/"),
        200,
        "first concurrent serial API after stdin EOF",
    );

    let second_pid = i32::try_from(second.child.id()).expect("second launcher PID should fit");
    // SAFETY: This targets the live unreaped launcher owned by the test.
    assert_eq!(unsafe { libc::kill(second_pid, libc::SIGTERM) }, 0);
    let (second_status, second_stdout, second_stderr) =
        second.wait("second concurrent production serial stop");
    assert!(
        second_status.success(),
        "second concurrent serial launcher should stop cleanly: {second_status:?}\nstdout:\n{second_stdout}\nstderr:\n{second_stderr}"
    );
    assert!(!second.socket.exists());
    assert!(first.socket.exists());
    assert_eq!(session_entries().len(), 1);
    assert_http_status(
        &http_get(&first.socket, "/"),
        200,
        "first concurrent serial API after peer termination",
    );
    assert_eq!(only_worker_pid(&first.child), first_worker);
    assert!(child_pids(first_worker).is_empty());

    let first_pid = i32::try_from(first.child.id()).expect("first launcher PID should fit");
    // SAFETY: This targets the live unreaped launcher owned by the test.
    assert_eq!(unsafe { libc::kill(first_pid, libc::SIGTERM) }, 0);
    let (first_status, first_stdout, first_stderr) =
        first.wait("first concurrent production serial stop");
    assert!(
        first_status.success(),
        "first concurrent serial launcher should stop cleanly: {first_status:?}\nstdout:\n{first_stdout}\nstderr:\n{first_stderr}"
    );
    assert!(!first.socket.exists());
    assert!(session_entries().is_empty());
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
            command.args(["--level", "Debug"]);
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
fn normal_bundle_preserves_worker_fatal_exit_and_granted_terminal_metrics() {
    let bundle = production_bundle();
    let baseline_sessions = session_entries();
    let fixture = OutputGrantFixture::new("fatal-signal");
    let mut running = spawn_ready_output_grant_api_launcher(&bundle, &fixture, "fatal-signal");
    fixture.replace_source_pathnames();
    configure_output_grant_session(&bundle, &running, "bangbang::");

    let resources = worker_bundle(&bundle).join("Contents/Resources");
    let waiting_boot_source = serde_json::json!({
        "kernel_image_path": path_text(&resources.join("guest-kernel")),
        "initrd_path": path_text(&resources.join("guest-initrd")),
        "boot_args": GUEST_SERIAL_RX_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&waiting_boot_source)
                .expect("waiting boot source should serialize"),
        ),
        204,
        "replace fatal-signal boot source",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start fatal-signal production guest",
    );
    let initial = wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        1,
        PROCESS_TIMEOUT,
        "fatal-signal initial",
    );
    assert_eq!(initial[0]["signals"]["sighup"], 0);

    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "fatal-signal worker must not retain a binder helper"
    );
    // SAFETY: `worker` is the sole live child of the retained launcher, and
    // SIGHUP is the production convergence stimulus under test.
    assert_eq!(unsafe { libc::kill(worker, libc::SIGHUP) }, 0);
    let status = running.wait("production worker SIGHUP convergence");
    assert_eq!(
        status.code(),
        Some(156),
        "the outer supervisor must preserve the worker's exact compatible exit"
    );
    assert!(!running.socket.exists());
    assert_eq!(session_entries(), baseline_sessions);

    let metrics = wait_for_canonical_output_metrics_lines(
        &fixture.opened_metrics,
        2,
        PROCESS_TIMEOUT,
        "fatal-signal terminal",
    );
    assert_eq!(metrics[0]["signals"]["sighup"], 0);
    assert_eq!(metrics[1]["signals"]["sighup"], 1);
    assert_eq!(metrics[1]["seccomp"]["num_faults"], 0);
    let logger = fs::read_to_string(&fixture.opened_logger)
        .expect("fatal-signal granted logger should read");
    assert_eq!(
        logger
            .matches("operation=shutdown outcome=abnormal\n")
            .count(),
        1
    );
    assert_eq!(
        logger
            .matches("event=process-exit category=process-failure\n")
            .count(),
        1
    );
    fixture.assert_replacement_outputs_unchanged();
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
fn normal_bundle_replaces_contained_macos_block_special_media_over_mmio() {
    let bundle = production_bundle();
    let outer_display = codesign_display(&bundle);
    let worker_display = codesign_display(&worker_bundle(&bundle));
    assert!(outer_display.contains(&format!("Identifier={LAUNCHER_BUNDLE_IDENTIFIER}")));
    assert!(worker_display.contains(&format!("Identifier={WORKER_BUNDLE_IDENTIFIER}")));
    assert_exact_networkless_bundle_entitlements(&bundle);

    let fixture = BlockSpecialGrantFixture::new("mmio-replacement");
    write_virtual_block_marker_at(&fixture.first_media, 0, BLOCK_LIFECYCLE_HOST_ONE_MARKER);
    write_virtual_block_marker_at(&fixture.second_media, 0, BLOCK_LIFECYCLE_HOST_THREE_MARKER);
    let expected_initial_device_id = expected_block_device_id(fixture.first_path());
    let mut running =
        spawn_ready_block_special_grant_api_launcher(&bundle, &fixture, "block-mmio", &[]);

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained MMIO block-special machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": DIRECT_ROOTFS_BLOCK_LIFECYCLE_TWO_BOOT_ARGS,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source)
                .expect("contained MMIO boot request should serialize"),
        ),
        204,
        "PUT contained MMIO block-special boot source",
    );
    for (route, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": BLOCK_SPECIAL_ROOT_REF,
                "is_root_device": true,
                "is_read_only": true,
            }),
            "PUT contained MMIO block-special rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": BLOCK_SPECIAL_FIRST_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Async",
            }),
            "PUT contained MMIO first block-special data drive",
        ),
        (
            "/drives/auditro",
            serde_json::json!({
                "drive_id": "auditro",
                "path_on_host": BLOCK_SPECIAL_READ_ONLY_REF,
                "is_root_device": false,
                "is_read_only": true,
                "cache_type": "Unsafe",
                "io_engine": "Sync",
            }),
            "PUT contained MMIO read-only block-special audit drive",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "path_on_host": BLOCK_SPECIAL_CONTROL_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
            }),
            "PUT contained MMIO lifecycle control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                route,
                &serde_json::to_string(&body).expect("contained MMIO drive should serialize"),
            ),
            204,
            context,
        );
    }
    let serial = serde_json::json!({"serial_out_path": BLOCK_SPECIAL_SERIAL_REF});
    assert_http_status(
        &http_put(
            &running.socket,
            "/serial",
            &serde_json::to_string(&serial).expect("contained MMIO serial should serialize"),
        ),
        204,
        "PUT contained MMIO block-special serial output",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained MMIO block-special lifecycle guest",
    );
    wait_for_file_contains(
        &fixture.serial,
        BLOCK_LIFECYCLE_LIMITER_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained MMIO guest should become ready for the live limiter patch");
    assert_http_status(
        &http_request(
            &running.socket,
            "PATCH",
            "/drives/data",
            r#"{"drive_id":"data","rate_limiter":{"ops":{"size":1,"refill_time":100}}}"#,
        ),
        204,
        "PATCH contained MMIO block-special limiter after guest probe",
    );
    resize_and_write_file_marker_at(
        &fixture.control,
        2 * VIRTIO_BLOCK_SECTOR_BYTES,
        0,
        BLOCK_LIFECYCLE_LIMITER_CONTINUE_MARKER,
    );
    wait_for_virtual_block_marker(
        &fixture.first_media,
        BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
        BLOCK_LIFECYCLE_GUEST_ONE_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        stop_running_launcher(&mut running, "failed contained MMIO first block phase");
        panic!("contained MMIO first block-special phase failed: {error}")
    });
    wait_for_file_contains(
        &fixture.serial,
        BLOCK_LIFECYCLE_PHASE_ONE_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained MMIO guest should publish phase one");
    assert_phase_block_serial_report(
        &fixture.serial,
        BLOCK_LIFECYCLE_INITIAL_SERIAL_BEGIN_MARKER,
        BLOCK_LIFECYCLE_INITIAL_SERIAL_END_MARKER,
        &expected_initial_device_id,
        "contained MMIO startup block-special drive",
    );

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained MMIO block-special guest",
    );
    let first_capture = http_put(&running.socket, "/snapshot/create", &snapshot_create_body());
    assert_http_status(
        &first_capture,
        400,
        "first contained block-special native-v1 rejection",
    );
    fixture.assert_no_snapshot_artifacts();
    let second_capture = http_put(
        &running.socket,
        "/snapshot/create",
        &repeated_snapshot_create_body(),
    );
    assert_http_status(
        &second_capture,
        400,
        "second contained block-special native-v1 rejection",
    );
    fixture.assert_no_snapshot_artifacts();

    let failed_patch = http_request(
        &running.socket,
        "PATCH",
        "/drives/data",
        &serde_json::json!({
            "drive_id": "data",
            "path_on_host": BLOCK_SPECIAL_READ_ONLY_REF,
        })
        .to_string(),
    );
    assert_http_status(
        &failed_patch,
        400,
        "reject contained MMIO access-mismatched block grant",
    );
    let unchanged = http_get(&running.socket, "/vm/config");
    assert_http_status(&unchanged, 200, "GET config after failed block grant claim");
    assert!(unchanged.contains(BLOCK_SPECIAL_FIRST_REF));
    assert!(!unchanged.contains(BLOCK_SPECIAL_SECOND_REF));

    let replacement = serde_json::json!({
        "drive_id": "data",
        "path_on_host": BLOCK_SPECIAL_SECOND_REF,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
        "io_engine": "Sync",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/data",
            &serde_json::to_string(&replacement)
                .expect("contained block replacement should serialize"),
        ),
        204,
        "replace contained MMIO block-special backing and engine",
    );
    let replaced = http_get(&running.socket, "/vm/config");
    assert_http_status(&replaced, 200, "GET replaced contained MMIO config");
    assert!(replaced.contains(BLOCK_SPECIAL_SECOND_REF));
    assert!(!replaced.contains(BLOCK_SPECIAL_FIRST_REF));
    assert!(replaced.contains(r#""io_engine":"Sync""#));
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained MMIO block-special guest",
    );
    wait_for_virtual_block_marker(
        &fixture.second_media,
        BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
        BLOCK_LIFECYCLE_GUEST_THREE_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        stop_running_launcher(&mut running, "failed contained MMIO final block phase");
        panic!("contained MMIO final block-special phase failed: {error}")
    });
    wait_for_file_contains(
        &fixture.serial,
        BLOCK_LIFECYCLE_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained MMIO guest should complete block-special lifecycle");

    stop_running_launcher(&mut running, "contained MMIO block-special lifecycle");
    fixture.assert_no_snapshot_artifacts();
    drop(running);
    fixture.verify_persistence_and_cleanup(
        BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
        BLOCK_LIFECYCLE_GUEST_ONE_MARKER,
        BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
        BLOCK_LIFECYCLE_GUEST_THREE_MARKER,
    );
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
fn normal_bundle_hotplugs_contained_macos_block_special_media_over_pci() {
    let bundle = production_bundle();
    assert_exact_networkless_bundle_entitlements(&bundle);

    let fixture = BlockSpecialGrantFixture::new("pci-hotplug");
    write_virtual_block_marker_at(&fixture.first_media, 0, BLOCK_HOTPLUG_HOST_ONE_MARKER);
    write_virtual_block_marker_at(&fixture.second_media, 0, BLOCK_HOTPLUG_HOST_TWO_MARKER);
    let first_device_id = expected_block_device_id(fixture.first_path());
    let second_device_id = expected_block_device_id(fixture.second_path());
    let mut running = spawn_ready_block_special_grant_api_launcher(
        &bundle,
        &fixture,
        "block-pci",
        &["--enable-pci"],
    );

    assert_http_status(
        &http_put(
            &running.socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        ),
        204,
        "PUT contained PCI block-special machine config",
    );
    let sealed_kernel = worker_bundle(&bundle).join("Contents/Resources/guest-kernel");
    let boot_args = format!(
        "{DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS} bangbang.expect-block-special-hotplug=1 bangbang.block-hotplug-cache-order=unsafe-writeback"
    );
    let boot_source = serde_json::json!({
        "kernel_image_path": path_text(&sealed_kernel),
        "boot_args": boot_args,
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/boot-source",
            &serde_json::to_string(&boot_source)
                .expect("contained PCI boot request should serialize"),
        ),
        204,
        "PUT contained PCI block-special boot source",
    );
    for (route, body, context) in [
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": BLOCK_SPECIAL_ROOT_REF,
                "is_root_device": true,
                "is_read_only": true,
            }),
            "PUT contained PCI block-special rootfs",
        ),
        (
            "/drives/control",
            serde_json::json!({
                "drive_id": "control",
                "path_on_host": BLOCK_SPECIAL_CONTROL_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
            }),
            "PUT contained PCI block-special control drive",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                route,
                &serde_json::to_string(&body).expect("contained PCI drive should serialize"),
            ),
            204,
            context,
        );
    }
    let serial = serde_json::json!({"serial_out_path": BLOCK_SPECIAL_SERIAL_REF});
    assert_http_status(
        &http_put(
            &running.socket,
            "/serial",
            &serde_json::to_string(&serial).expect("contained PCI serial should serialize"),
        ),
        204,
        "PUT contained PCI block-special serial output",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        ),
        204,
        "start contained PCI block-special guest",
    );
    wait_for_file_prefix(
        &fixture.control,
        BLOCK_HOTPLUG_READY_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("contained PCI block-special guest should become ready");

    let wrong_access = serde_json::json!({
        "drive_id": "hotdata",
        "path_on_host": BLOCK_SPECIAL_FIRST_REF,
        "is_root_device": false,
        "is_read_only": true,
        "cache_type": "Unsafe",
        "io_engine": "Async",
    });
    let denied = http_put(
        &running.socket,
        "/drives/hotdata",
        &serde_json::to_string(&wrong_access).expect("wrong-access drive should serialize"),
    );
    assert_http_status(&denied, 400, "contained PCI block grant access mismatch");
    assert!(!http_get(&running.socket, "/vm/config").contains(r#""drive_id":"hotdata""#));

    let first = serde_json::json!({
        "drive_id": "hotdata",
        "path_on_host": BLOCK_SPECIAL_FIRST_REF,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Unsafe",
        "io_engine": "Async",
        "rate_limiter": {
            "ops": {
                "size": 1,
                "refill_time": 100,
            },
        },
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/hotdata",
            &serde_json::to_string(&first).expect("first contained PCI drive should serialize"),
        ),
        204,
        "runtime PUT first contained block-special PCI drive",
    );
    let first_config = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &first_config,
        200,
        "GET first contained block-special PCI config",
    );
    assert!(first_config.contains(BLOCK_SPECIAL_FIRST_REF));
    assert!(first_config.contains(r#""cache_type":"Unsafe""#));
    assert!(first_config.contains(r#""io_engine":"Async""#));
    assert!(first_config.contains(r#""refill_time":100"#));
    wait_for_virtual_block_marker(
        &fixture.first_media,
        0,
        BLOCK_HOTPLUG_GUEST_ONE_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        stop_running_launcher(&mut running, "failed contained PCI first block round");
        panic!("contained first PCI block-special round failed: {error}")
    });
    wait_for_file_prefix(
        &fixture.control,
        BLOCK_HOTPLUG_FIRST_REMOVED_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should manually remove first contained block-special function");
    wait_for_file_contains(
        &fixture.serial,
        BLOCK_HOTPLUG_FIRST_SERIAL_END_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should report first contained hotplug GET_ID");
    assert_phase_block_serial_report(
        &fixture.serial,
        BLOCK_HOTPLUG_FIRST_SERIAL_BEGIN_MARKER,
        BLOCK_HOTPLUG_FIRST_SERIAL_END_MARKER,
        &first_device_id,
        "first contained PCI block-special drive",
    );

    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause contained block-special guest before DELETE",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "DELETE first contained block-special PCI drive",
    );
    let removed = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &removed,
        200,
        "GET config after contained block-special DELETE",
    );
    assert!(!removed.contains(r#""drive_id":"hotdata""#));

    let second = serde_json::json!({
        "drive_id": "hotdata",
        "path_on_host": BLOCK_SPECIAL_SECOND_REF,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
        "io_engine": "Sync",
    });
    assert_http_status(
        &http_put(
            &running.socket,
            "/drives/hotdata",
            &serde_json::to_string(&second).expect("second contained PCI drive should serialize"),
        ),
        204,
        "paused PUT reused contained block-special PCI drive",
    );
    let reused = http_get(&running.socket, "/vm/config");
    assert_http_status(
        &reused,
        200,
        "GET reused contained block-special PCI config",
    );
    assert!(reused.contains(BLOCK_SPECIAL_SECOND_REF));
    assert!(reused.contains(r#""cache_type":"Writeback""#));
    assert!(reused.contains(r#""io_engine":"Sync""#));
    resize_and_write_file_marker_at(
        &fixture.control,
        2 * VIRTIO_BLOCK_SECTOR_BYTES,
        VIRTIO_BLOCK_SECTOR_BYTES,
        BLOCK_HOTPLUG_CONTINUE_MARKER,
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
        204,
        "resume contained block-special guest after slot reuse",
    );
    wait_for_virtual_block_marker(
        &fixture.second_media,
        0,
        BLOCK_HOTPLUG_GUEST_TWO_MARKER,
        PROCESS_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        stop_running_launcher(&mut running, "failed contained PCI second block round");
        panic!("contained second PCI block-special round failed: {error}")
    });
    wait_for_file_prefix(
        &fixture.control,
        BLOCK_HOTPLUG_SUCCESS_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should manually remove reused contained block-special function");
    wait_for_file_contains(
        &fixture.serial,
        BLOCK_HOTPLUG_SECOND_SERIAL_END_MARKER,
        PROCESS_TIMEOUT,
    )
    .expect("guest should report second contained hotplug GET_ID");
    assert_phase_block_serial_report(
        &fixture.serial,
        BLOCK_HOTPLUG_SECOND_SERIAL_BEGIN_MARKER,
        BLOCK_HOTPLUG_SECOND_SERIAL_END_MARKER,
        &second_device_id,
        "second contained PCI block-special drive",
    );
    assert_http_status(
        &http_request(&running.socket, "DELETE", "/drives/hotdata", ""),
        204,
        "final DELETE contained block-special PCI drive",
    );

    stop_running_launcher(&mut running, "contained block-special PCI lifecycle");
    drop(running);
    fixture.verify_persistence_and_cleanup(
        0,
        BLOCK_HOTPLUG_GUEST_ONE_MARKER,
        0,
        BLOCK_HOTPLUG_GUEST_TWO_MARKER,
    );
}

#[test]
fn normal_bundle_hotplugs_mmds_network_without_vmnet_authority() {
    let bundle = production_bundle();
    let fixture = GuestDeviceGrantFixture::new("runtime-network-hotplug");
    let logger = fixture.add_logger_grant("runtime-network-hotplug");
    resize_and_write_file_marker_at(&fixture.data, 1536, 0, &[]);
    let mut running = spawn_ready_device_grant_api_launcher_with_extra_args(
        &bundle,
        &fixture,
        "runtime-network-hotplug",
        &["--enable-pci"],
    );
    fixture.replace_source_pathnames();
    logger.replace_source_pathname();

    assert_http_status(
        &http_put(
            &running.socket,
            "/logger",
            &serde_json::json!({"log_path": OUTPUT_LOGGER_REF}).to_string(),
        ),
        204,
        "PUT contained network-hotplug logger grant",
    );

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
    logger.assert_records(
        &["device-kind=network operation=mmds-request outcome=detoured"],
        fixture.sensitive_strings().into_iter().chain([
            "06:00:00:00:00:42".to_owned(),
            "BANGBANG_MMDS_GUEST_VALUE".to_owned(),
            std::str::from_utf8(NETWORK_HOTPLUG_SUCCESS_MARKER)
                .expect("network marker should be UTF-8")
                .to_owned(),
        ]),
    );
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

    let restore = RestoreTransactionGrantFixture::new("normal", RestoreGrantVariant::Exact);
    let output = run_restore_probe(&bundle, &restore, "restore-success");
    assert_eq!(output.status.code(), Some(ARGUMENT_PARSING_EXIT_CODE));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(RESTORE_ACTIVE_READY));
    assert_restore_output_redacted(&output, &restore);
    restore.assert_pristine();
}

#[test]
fn normal_production_bundle_statically_and_dynamically_excludes_elevated_probe() {
    let bundle = production_bundle();
    assert!(
        !worker_bundle(&bundle)
            .join("Contents/Resources")
            .join(ELEVATED_PROBE_MARKER)
            .exists(),
        "normal production bundle must not carry the elevated probe marker"
    );
    assert!(
        !worker_bundle(&bundle)
            .join("Contents/Resources")
            .join(ELEVATED_RUNTIME_MARKER)
            .exists(),
        "normal production bundle must not carry the target runtime marker"
    );
    let launcher_bytes = fs::read(launcher(&bundle)).expect("normal launcher should read");
    let worker_bytes = fs::read(worker_executable(&bundle)).expect("normal worker should read");
    for artifact in [&launcher_bytes, &worker_bytes] {
        for marker in [
            ELEVATED_WORKER_OPTION,
            ELEVATED_READY_RECORD,
            ELEVATED_BLOCKED_STATUS,
            ELEVATED_INHERITED_MODE,
            ELEVATED_HVF_STAGE,
            ELEVATED_CREDENTIAL_DROP_MODE,
            ELEVATED_CREDENTIAL_RETAIN_MODE,
            ELEVATED_CREDENTIAL_UNMAPPED_MODE,
            ELEVATED_CREDENTIAL_CONTROL_MODE,
            ELEVATED_CREDENTIAL_STATUS,
            ELEVATED_CREDENTIAL_RECORD,
            ELEVATED_CREDENTIAL_DATAGRAM,
            ELEVATED_CREDENTIAL_STEP,
            ELEVATED_CREDENTIAL_LAUNCHER_ARTIFACT,
            ELEVATED_CREDENTIAL_WORKER_ARTIFACT,
            ELEVATED_RUNTIME_DROP_MODE,
            ELEVATED_RUNTIME_RETAIN_MODE,
            ELEVATED_RUNTIME_UNMAPPED_MODE,
            ELEVATED_RUNTIME_STATUS,
            ELEVATED_CONTINUATION_RECORD,
            ELEVATED_RUNTIME_AUTHORITY_RECORD,
            ELEVATED_RUNTIME_GRANT_CASE,
            ELEVATED_RUNTIME_LAUNCHER_BOUNDARIES,
            ELEVATED_RUNTIME_WORKER_BOUNDARIES,
            ELEVATED_API_LISTENER_LAUNCHER_BOUNDARY,
            ELEVATED_API_LISTENER_WORKER_BOUNDARY,
        ]
        .into_iter()
        .chain(ELEVATED_GUEST_MARKERS.iter().copied())
        {
            assert!(
                !artifact
                    .windows(marker.len())
                    .any(|window| window == marker),
                "normal artifact must statically exclude the elevated probe"
            );
        }
    }

    for mode in ["drop", "credential-drop", "runtime-drop"] {
        let output = run_launcher(
            &bundle,
            &[
                OsStr::new(ELEVATED_PROBE_OPTION),
                OsStr::new("--root"),
                OsStr::new("/private/var/root/bangbang-elevated-probe.Disabled"),
                OsStr::new("--target-uid"),
                OsStr::new("501"),
                OsStr::new("--target-gid"),
                OsStr::new("20"),
                OsStr::new("--mode"),
                OsStr::new(mode),
                OsStr::new("--"),
            ],
        );
        assert_eq!(output.status.code(), Some(ARGUMENT_PARSING_EXIT_CODE));
        let diagnostics = [output.stdout, output.stderr].concat();
        for marker in [
            ELEVATED_READY_RECORD,
            ELEVATED_BLOCKED_STATUS,
            ELEVATED_INHERITED_MODE,
            ELEVATED_HVF_STAGE,
            ELEVATED_CREDENTIAL_STATUS,
            ELEVATED_CREDENTIAL_RECORD,
            ELEVATED_CREDENTIAL_DATAGRAM,
            ELEVATED_CREDENTIAL_STEP,
            ELEVATED_CREDENTIAL_LAUNCHER_ARTIFACT,
            ELEVATED_CREDENTIAL_WORKER_ARTIFACT,
            ELEVATED_RUNTIME_STATUS,
            ELEVATED_CONTINUATION_RECORD,
            ELEVATED_RUNTIME_AUTHORITY_RECORD,
            ELEVATED_RUNTIME_GRANT_CASE,
            ELEVATED_RUNTIME_LAUNCHER_BOUNDARIES,
            ELEVATED_RUNTIME_WORKER_BOUNDARIES,
            ELEVATED_API_LISTENER_LAUNCHER_BOUNDARY,
            ELEVATED_API_LISTENER_WORKER_BOUNDARY,
        ]
        .into_iter()
        .chain(ELEVATED_GUEST_MARKERS.iter().copied())
        {
            assert!(
                !diagnostics
                    .windows(marker.len())
                    .any(|window| window == marker),
                "normal bundle must not activate elevated probe behavior"
            );
        }
    }
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
fn signed_restore_transaction_covers_logical_abort_cancellation_and_commit_phases() {
    let bundle = grant_test_bundle();
    assert_exact_networkless_bundle_entitlements(&bundle);
    recover_session_root(&bundle);

    for (index, case) in [
        "restore-logical-mismatch",
        "restore-reservation-abort",
        "restore-cancellation",
        "restore-success",
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = RestoreTransactionGrantFixture::new(
            &format!("phase-{index}"),
            RestoreGrantVariant::Exact,
        );
        let output = run_restore_probe(&bundle, &fixture, case);
        assert_output_success(&output, &format!("signed {case} probe"));
        assert_restore_output_redacted(&output, &fixture);
        fixture.assert_pristine();
        assert!(session_entries().is_empty());
    }

    let extra =
        RestoreTransactionGrantFixture::new("extra-authority", RestoreGrantVariant::ExtraUnrelated);
    let output = run_restore_probe(&bundle, &extra, "restore-success");
    assert_output_success(&output, "signed restore with unrelated startup authority");
    assert_restore_output_redacted(&output, &extra);
    extra.assert_pristine();
    assert!(session_entries().is_empty());
}

#[test]
fn signed_restore_transaction_authority_mismatches_fail_closed() {
    let bundle = grant_test_bundle();
    recover_session_root(&bundle);
    for (index, variant) in [
        RestoreGrantVariant::MissingRoot,
        RestoreGrantVariant::MissingDirectory,
        RestoreGrantVariant::WrongRootRole,
        RestoreGrantVariant::WrongRootAccess,
        RestoreGrantVariant::WrongRootKind,
        RestoreGrantVariant::WrongDirectoryRole,
        RestoreGrantVariant::WrongDirectoryAccess,
        RestoreGrantVariant::WrongDirectoryKind,
        RestoreGrantVariant::SubstitutedIds,
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = RestoreTransactionGrantFixture::new(&format!("reject-{index}"), variant);
        let output = run_restore_probe(&bundle, &fixture, "restore-success");
        assert_eq!(
            output.status.code(),
            Some(PROCESS_FAILURE_EXIT_CODE),
            "restore authority variant {variant:?} must fail closed"
        );
        assert_restore_output_redacted(&output, &fixture);
        fixture.assert_pristine();
        assert!(session_entries().is_empty());
    }
}

#[test]
fn signed_restore_transaction_uses_launcher_opened_root_after_path_replacement() {
    let bundle = grant_test_bundle();
    let fixture =
        RestoreTransactionGrantFixture::new("root-replacement", RestoreGrantVariant::Exact);
    let mut holding = spawn_holding_restore_probe(
        &bundle,
        &fixture,
        "restore-wait-then-success",
        RESTORE_REPLACE_READY,
        true,
    );
    let retained = fixture.replace_root_source();
    holding.release_stdin();
    assert!(
        holding.wait("restore root replacement").success(),
        "restore root replacement probe should succeed"
    );
    fixture.assert_root_replacement_preserved(&retained);
    assert!(session_entries().is_empty());
}

#[test]
fn signed_restore_transaction_active_boundary_has_no_helper_and_cleans_gracefully() {
    let bundle = grant_test_bundle();
    let fixture =
        RestoreTransactionGrantFixture::new("active-boundary", RestoreGrantVariant::Exact);
    let mut holding = spawn_holding_restore_probe(
        &bundle,
        &fixture,
        "restore-hold-active",
        RESTORE_ACTIVE_READY,
        false,
    );
    assert!(is_socket_path(&fixture.socket()));
    let worker = only_worker_pid(&holding.child);
    assert!(
        child_pids(worker).is_empty(),
        "restore binder must be reaped before active readiness"
    );
    assert_exact_networkless_bundle_entitlements(&bundle);
    holding.stop(libc::SIGTERM, "active restore transaction");
    assert!(!fixture.socket().exists());
    assert!(session_entries().is_empty());
}

#[test]
fn signed_restore_transaction_cleans_both_independent_death_orders() {
    let bundle = grant_test_bundle();
    recover_session_root(&bundle);

    let launcher_fixture =
        RestoreTransactionGrantFixture::new("launcher-death", RestoreGrantVariant::Exact);
    let mut launcher_first = spawn_holding_restore_probe(
        &bundle,
        &launcher_fixture,
        "restore-hold-active",
        RESTORE_ACTIVE_READY,
        false,
    );
    let worker = only_worker_pid(&launcher_first.child);
    let worker_exit = ProcessExitWatch::new(worker);
    let launcher = i32::try_from(launcher_first.child.id()).expect("launcher PID should fit");
    // SAFETY: The unreaped restore launcher owns this exact PID.
    assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
    assert_eq!(
        launcher_first.wait("restore launcher SIGKILL").signal(),
        Some(libc::SIGKILL)
    );
    assert!(
        worker_exit.wait(PROCESS_TIMEOUT),
        "restore worker should observe launcher death"
    );
    assert!(!launcher_fixture.socket().exists());
    assert!(session_entries().is_empty());

    let worker_fixture =
        RestoreTransactionGrantFixture::new("worker-death", RestoreGrantVariant::Exact);
    let mut worker_first = spawn_holding_restore_probe(
        &bundle,
        &worker_fixture,
        "restore-hold-active",
        RESTORE_ACTIVE_READY,
        false,
    );
    let worker = only_worker_pid(&worker_first.child);
    // SAFETY: The worker is the sole live child of this unreaped launcher.
    assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
    assert_eq!(
        worker_first.wait("active restore worker SIGKILL").code(),
        Some(128 + libc::SIGKILL)
    );
    assert!(!worker_fixture.socket().exists());
    assert!(session_entries().is_empty());

    let prepared_fixture =
        RestoreTransactionGrantFixture::new("prepared-death", RestoreGrantVariant::Exact);
    let mut prepared = spawn_holding_restore_probe(
        &bundle,
        &prepared_fixture,
        "restore-hold-prepared",
        RESTORE_PREPARED_READY,
        false,
    );
    let worker = only_worker_pid(&prepared.child);
    // SAFETY: The worker is the sole live child of this unreaped launcher.
    assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
    assert_eq!(
        prepared.wait("prepared restore worker SIGKILL").code(),
        Some(128 + libc::SIGKILL)
    );
    assert!(!prepared_fixture.socket().exists());
    assert!(session_entries().is_empty());
}

#[test]
fn signed_restore_transaction_preserves_a_published_socket_replacement() {
    let bundle = grant_test_bundle();
    let fixture =
        RestoreTransactionGrantFixture::new("socket-replacement", RestoreGrantVariant::Exact);
    let mut holding = spawn_holding_restore_probe(
        &bundle,
        &fixture,
        "restore-hold-active",
        RESTORE_ACTIVE_READY,
        false,
    );
    let socket = fixture.socket();
    let retained = fixture._root.path().join("retained-owned.sock");
    fs::rename(&socket, &retained).expect("owned restore socket should rename");
    let replacement = UnixListener::bind(&socket).expect("replacement socket should bind");
    holding.stop(libc::SIGTERM, "restore socket replacement");
    assert!(
        is_socket_path(&socket),
        "identity cleanup must preserve the replacement socket"
    );
    assert!(is_socket_path(&retained));
    drop(replacement);
    fs::remove_file(&socket).expect("replacement socket should clean");
    assert!(session_entries().is_empty());
}

#[test]
fn concurrent_signed_restore_transactions_keep_same_ids_noninterchangeable() {
    let bundle = grant_test_bundle();
    recover_session_root(&bundle);
    let alpha = RestoreTransactionGrantFixture::new("concurrent-alpha", RestoreGrantVariant::Exact);
    let beta = RestoreTransactionGrantFixture::new("concurrent-beta", RestoreGrantVariant::Exact);
    let mut alpha_probe = spawn_holding_restore_probe(
        &bundle,
        &alpha,
        "restore-hold-active",
        RESTORE_ACTIVE_READY,
        false,
    );
    let mut beta_probe = spawn_holding_restore_probe(
        &bundle,
        &beta,
        "restore-hold-active",
        RESTORE_ACTIVE_READY,
        false,
    );
    assert!(is_socket_path(&alpha.socket()));
    assert!(is_socket_path(&beta.socket()));
    assert_eq!(session_entries().len(), 2);
    alpha_probe.stop(libc::SIGTERM, "alpha restore transaction");
    assert!(!alpha.socket().exists());
    assert!(
        is_socket_path(&beta.socket()),
        "stopping alpha must preserve beta authority"
    );
    beta_probe.stop(libc::SIGTERM, "beta restore transaction");
    assert!(!beta.socket().exists());
    assert!(session_entries().is_empty());
}

#[test]
fn signed_pager_grant_completes_and_repeats_under_unchanged_entitlements() {
    let bundle = grant_test_bundle();
    assert_exact_networkless_bundle_entitlements(&bundle);

    let fixture = PagerGrantFixture::new("complete-repeat");
    for cycle in 0..2 {
        let mut peer = fixture.start_peer("complete");
        let output = run_pager_probe(&bundle, &fixture, "pager-complete");
        assert_output_success(&output, "signed contained pager probe");
        assert_pager_output_redacted(&output, &fixture);
        peer.wait_success(&format!("pager reference cycle {cycle}"));
        fixture.clear_socket();
        assert!(session_entries().is_empty());
    }
}

#[test]
fn signed_pager_consumer_chain_runs_inside_app_sandbox() {
    let bundle = grant_test_bundle();
    let fixture = PagerGrantFixture::new("consumer-chain");
    let mut peer = fixture.start_peer("complete");
    let output = run_pager_probe(&bundle, &fixture, "pager-consumer");

    assert_output_success(&output, "signed contained pager consumer probe");
    assert_pager_output_redacted(&output, &fixture);
    peer.wait_success("pager consumer reference peer");
    fixture.clear_socket();
    assert!(session_entries().is_empty());
}

#[test]
fn signed_pager_grant_covers_cancellation_and_terminal_shutdown() {
    let bundle = grant_test_bundle();
    for (index, (peer_mode, probe_case)) in
        [("cancel", "pager-cancel"), ("terminal", "pager-terminal")]
            .into_iter()
            .enumerate()
    {
        let fixture = PagerGrantFixture::new(&format!("lifecycle-{index}"));
        let mut peer = fixture.start_peer(peer_mode);
        let output = run_pager_probe(&bundle, &fixture, probe_case);
        let peer_status = peer.wait("pager lifecycle reference peer");
        assert_output_success(
            &output,
            &format!("signed contained pager {peer_mode} lifecycle probe"),
        );
        assert_pager_output_redacted(&output, &fixture);
        assert!(
            peer_status.success(),
            "pager {peer_mode} reference peer should succeed: {peer_status:?}"
        );
        fixture.clear_socket();
        assert!(session_entries().is_empty());
    }
}

#[test]
fn signed_pager_grant_rejects_connection_descriptor_and_protocol_failures() {
    let bundle = grant_test_bundle();

    let missing = PagerGrantFixture::new("missing");
    let output = run_pager_probe(&bundle, &missing, "pager-complete");
    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert_pager_output_redacted(&output, &missing);
    assert!(session_entries().is_empty());

    let wrong = PagerGrantFixture::new("wrong-descriptor");
    wrong.install_wrong_descriptor();
    let output = run_pager_probe(&bundle, &wrong, "pager-complete");
    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert_pager_output_redacted(&output, &wrong);
    assert!(session_entries().is_empty());

    for (index, peer_mode) in ["corrupt", "eof", "stall"].into_iter().enumerate() {
        let fixture = PagerGrantFixture::new(&format!("protocol-{index}"));
        let mut peer = fixture.start_peer(peer_mode);
        let started = Instant::now();
        let output = run_pager_probe(&bundle, &fixture, "pager-complete");
        assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "pager {peer_mode} failure must remain bounded"
        );
        assert_pager_output_redacted(&output, &fixture);
        peer.wait_success("failing pager reference peer");
        fixture.clear_socket();
        assert!(session_entries().is_empty());
    }
}

#[test]
fn signed_pager_grant_closes_streams_across_both_process_death_orders() {
    let bundle = grant_test_bundle();
    recover_session_root(&bundle);

    let peer_death = PagerGrantFixture::new("peer-death");
    let mut peer = peer_death.start_peer("hold");
    let mut launcher = HoldingPagerLauncher::start(&bundle, &peer_death);
    let peer_status = peer.kill(libc::SIGKILL, "pager peer SIGKILL");
    assert_eq!(peer_status.signal(), Some(libc::SIGKILL));
    assert_eq!(
        launcher.wait("launcher after pager peer death").code(),
        Some(PROCESS_FAILURE_EXIT_CODE)
    );
    peer_death.clear_socket();
    assert!(session_entries().is_empty());

    let worker_death = PagerGrantFixture::new("worker-death");
    let mut peer = worker_death.start_peer("hold");
    let mut launcher = HoldingPagerLauncher::start(&bundle, &worker_death);
    let worker = only_worker_pid(&launcher.child);
    // SAFETY: The worker is the sole live child of this unreaped launcher.
    assert_eq!(unsafe { libc::kill(worker, libc::SIGKILL) }, 0);
    assert_eq!(
        launcher.wait("pager worker SIGKILL").code(),
        Some(128 + libc::SIGKILL)
    );
    peer.wait_success("pager reference after worker death");
    worker_death.clear_socket();
    assert!(session_entries().is_empty());
}

#[test]
fn signed_contained_block_device_uses_launcher_control_broker() {
    let bundle = grant_test_bundle();
    assert_exact_networkless_bundle_entitlements(&bundle);
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

#[test]
fn normal_granted_api_listener_preserves_5000_fresh_connections() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new("api-transport-stress");
    let mut running =
        spawn_ready_socket_grant_api_launcher(&bundle, &fixture, "api-transport-stress");

    for request in 0..5_000 {
        let response = http_get(&running.socket, "/");
        assert!(
            response.starts_with("HTTP/1.1 200 "),
            "fresh production API request {request} failed:\n{response}"
        );
    }

    let launcher_pid = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: `launcher_pid` is the live unreaped launcher owned by this fixture.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGTERM) }, 0);
    assert!(running.wait("API transport stress graceful stop").success());
    assert!(!running.socket.exists());
    assert!(session_entries().is_empty());
}

#[test]
fn direct_api_remains_independent_of_unused_socket_directory_grants() {
    let bundle = production_bundle();
    let fixture = SocketDirectoryGrantFixture::new("direct-api-unused-socket-grants");
    initialize_worker_container(&bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket = container_tmp_dir().join(format!(
        "bb-direct-extra-{:x}-{test_id:x}.sock",
        std::process::id()
    ));
    let mut child = Command::new(launcher(&bundle))
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.devices.manifest)
        .arg("--")
        .args(["--api-sock", path_text(&socket)])
        .args(["--id", &format!("direct-extra-{test_id}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("direct API launcher with extra grants should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "direct API with unused socket grants should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    let mut running = RunningApiLauncher {
        child,
        socket,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: fixture.sensitive_strings(),
        completed: false,
    };

    assert_http_status(
        &http_get(&running.socket, "/"),
        200,
        "direct API with unused socket grants",
    );
    assert!(
        !fixture.api_socket().exists(),
        "unused API grant must not publish a listener"
    );
    let worker = only_worker_pid(&running.child);
    assert!(
        child_pids(worker).is_empty(),
        "direct API path must not start a socket binder"
    );
    let launcher_pid = i32::try_from(running.child.id()).expect("launcher PID should fit");
    // SAFETY: `launcher_pid` is the live unreaped launcher owned by this fixture.
    assert_eq!(unsafe { libc::kill(launcher_pid, libc::SIGTERM) }, 0);
    assert!(running.wait("direct API with unused grants stop").success());
    assert!(!running.socket.exists());
    assert!(session_entries().is_empty());
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
    data: PathBuf,
    audit: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotEditorFileFacts {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Debug)]
struct SnapshotEditorViews {
    vcpus: serde_json::Value,
    vm: serde_json::Value,
}

#[derive(Debug)]
struct SnapshotEditorCertification {
    artifacts: SnapshotArtifactSet,
    original_state: PathBuf,
    original_state_bytes: Vec<u8>,
    original_state_facts: SnapshotEditorFileFacts,
    edited_state_bytes: Vec<u8>,
    edited_state_facts: SnapshotEditorFileFacts,
    memory_bytes: Vec<u8>,
    memory_facts: SnapshotEditorFileFacts,
}

#[derive(Debug, Clone)]
struct SerialSnapshotGrantArtifacts {
    state: PathBuf,
    memory: PathBuf,
    drive: Option<PathBuf>,
}

#[derive(Debug)]
struct SerialSnapshotSourceGrantFixture {
    _root: TestDir,
    _socket_root: Option<TestDir>,
    manifest: PathBuf,
    kernel: PathBuf,
    initrd: Option<PathBuf>,
    metrics: PathBuf,
    drive: Option<PathBuf>,
    serial: Option<PathBuf>,
    state_directory: PathBuf,
    memory_directory: PathBuf,
    opened_kernel: PathBuf,
    opened_initrd: Option<PathBuf>,
    opened_metrics: PathBuf,
    opened_drive: Option<PathBuf>,
    opened_serial: Option<PathBuf>,
    api_directory: Option<PathBuf>,
}

impl SerialSnapshotSourceGrantFixture {
    fn new(case: &str, with_storage: bool, configured_output: bool) -> Self {
        Self::new_with_guest(case, with_storage, configured_output, false)
    }

    fn new_entropy(case: &str, with_storage: bool) -> Self {
        Self::new_with_guest(case, with_storage, false, true)
    }

    fn new_with_guest(
        case: &str,
        with_storage: bool,
        configured_output: bool,
        entropy_guest: bool,
    ) -> Self {
        let root = TestDir::new(&format!("serial-snapshot-source-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("serial snapshot source root should canonicalize");
        let manifest = canonical_root.join("grant-manifest.json");
        let kernel = canonical_root.join("serial-snapshot.image");
        let initrd = entropy_guest.then(|| canonical_root.join("entropy-snapshot-initrd.cpio"));
        let metrics = canonical_root.join("serial-snapshot.metrics");
        let drive = with_storage.then(|| canonical_root.join("serial-snapshot.drive"));
        let serial = configured_output.then(|| canonical_root.join("serial-snapshot.out"));
        let state_directory = canonical_root.join("state-output");
        let memory_directory = canonical_root.join("memory-output");
        let opened_kernel = canonical_root.join("opened-serial-snapshot.image");
        let opened_initrd = initrd
            .as_ref()
            .map(|_| canonical_root.join("opened-entropy-snapshot-initrd.cpio"));
        let opened_metrics = canonical_root.join("opened-serial-snapshot.metrics");
        let opened_drive = drive
            .as_ref()
            .map(|_| canonical_root.join("opened-serial-snapshot.drive"));
        let opened_serial = serial
            .as_ref()
            .map(|_| canonical_root.join("opened-serial-snapshot.out"));
        let socket_root = entropy_guest.then(|| {
            let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
            let root = TestDir(
                PathBuf::from("/private/tmp")
                    .join(format!("bbes-{}-{socket_id}", std::process::id())),
            );
            fs::create_dir(root.path()).expect("short entropy source socket root should create");
            root
        });
        let api_directory = socket_root.as_ref().map(|root| root.path().join("a"));

        if entropy_guest {
            hard_link_or_copy_fixture(&guest_kernel(), &kernel, "entropy snapshot guest kernel");
            hard_link_or_copy_fixture(
                &guest_initrd(),
                initrd
                    .as_ref()
                    .expect("entropy snapshot guest should have an initrd"),
                "entropy snapshot guest initrd",
            );
        } else {
            fs::write(
                &kernel,
                if configured_output {
                    snapshot_serial::configured_output_guest_image()
                } else {
                    snapshot_serial::default_stdio_guest_image()
                },
            )
            .expect("serial snapshot guest image should write");
        }
        fs::write(&metrics, b"").expect("serial snapshot metrics should write");
        if let Some(drive) = drive.as_ref() {
            create_sized_file(drive, 4096);
        }
        if let Some(serial) = serial.as_ref() {
            fs::write(serial, b"").expect("serial snapshot output should write");
        }
        if let Some(directory) = api_directory.as_ref() {
            fs::create_dir(directory).expect("entropy API socket directory should create");
        }
        fs::create_dir(&state_directory).expect("serial state output directory should create");
        fs::create_dir(&memory_directory).expect("serial memory output directory should create");

        let mut grants = vec![
            serde_json::json!({
                "id": SNAPSHOT_KERNEL_ID,
                "role": "kernel-image",
                "access": "read-only",
                "source": path_text(&kernel),
            }),
            serde_json::json!({
                "id": SNAPSHOT_METRICS_ID,
                "role": "metrics-sink",
                "access": "write-only",
                "source": path_text(&metrics),
            }),
            serde_json::json!({
                "id": SNAPSHOT_STATE_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(&state_directory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(&memory_directory),
            }),
        ];
        if let Some(initrd) = initrd.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_INITRD_ID,
                "role": "initrd-image",
                "access": "read-only",
                "source": path_text(initrd),
            }));
        }
        if let Some(drive) = drive.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_DATA_ID,
                "role": "drive-backing",
                "access": "read-write",
                "source": path_text(drive),
            }));
        }
        if let Some(serial) = serial.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_SERIAL_SINK_ID,
                "role": "serial-sink",
                "access": "write-only",
                "source": path_text(serial),
            }));
        }
        if let Some(directory) = api_directory.as_ref() {
            grants.push(serde_json::json!({
                "id": API_SOCKET_DIRECTORY_ID,
                "role": "api-socket-directory",
                "access": "create-children",
                "source": path_text(directory),
            }));
        }
        let manifest_json = serde_json::json!({"version": 1, "grants": grants});
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json)
                .expect("serial snapshot source manifest should serialize"),
        )
        .expect("serial snapshot source manifest should write");

        Self {
            _root: root,
            _socket_root: socket_root,
            manifest,
            kernel,
            initrd,
            metrics,
            drive,
            serial,
            state_directory,
            memory_directory,
            opened_kernel,
            opened_initrd,
            opened_metrics,
            opened_drive,
            opened_serial,
            api_directory,
        }
    }

    fn replace_source_pathnames(&self) {
        for (source, opened) in [
            (&self.kernel, &self.opened_kernel),
            (&self.metrics, &self.opened_metrics),
        ] {
            fs::rename(source, opened).expect("opened serial snapshot source file should move");
        }
        fs::write(&self.kernel, b"replacement kernel must not boot")
            .expect("replacement serial snapshot kernel should write");
        if let (Some(source), Some(opened)) = (&self.initrd, &self.opened_initrd) {
            fs::rename(source, opened).expect("opened entropy snapshot initrd should move");
            fs::write(source, b"replacement initrd must not boot")
                .expect("replacement entropy snapshot initrd should write");
        }
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement serial snapshot metrics should write");
        if let (Some(source), Some(opened)) = (&self.drive, &self.opened_drive) {
            fs::rename(source, opened).expect("opened serial snapshot drive should move");
            fs::write(source, vec![0xff_u8; 4096])
                .expect("replacement serial snapshot drive should write");
        }
        if let (Some(source), Some(opened)) = (&self.serial, &self.opened_serial) {
            fs::rename(source, opened).expect("opened serial snapshot output should move");
            fs::write(source, b"replacement serial output must remain unused\n")
                .expect("replacement serial snapshot output should write");
        }
    }

    fn artifacts(&self) -> SerialSnapshotGrantArtifacts {
        SerialSnapshotGrantArtifacts {
            state: self.state_directory.join(SNAPSHOT_STATE_CHILD),
            memory: self.memory_directory.join(SNAPSHOT_MEMORY_CHILD),
            drive: self.opened_drive.clone(),
        }
    }

    fn api_socket(&self) -> PathBuf {
        self.api_directory
            .as_ref()
            .expect("entropy source should grant an API socket directory")
            .join(API_SOCKET_CHILD)
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = [
            path_text(&self.manifest),
            path_text(&self.kernel),
            path_text(&self.metrics),
            path_text(&self.state_directory),
            path_text(&self.memory_directory),
            path_text(&self.opened_kernel),
            path_text(&self.opened_metrics),
            SNAPSHOT_KERNEL_ID,
            SNAPSHOT_METRICS_ID,
            SNAPSHOT_STATE_OUTPUT_ID,
            SNAPSHOT_MEMORY_OUTPUT_ID,
            SNAPSHOT_KERNEL_REF,
            SNAPSHOT_METRICS_REF,
            SNAPSHOT_STATE_OUTPUT_REF,
            SNAPSHOT_MEMORY_OUTPUT_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if let (Some(source), Some(opened)) = (&self.drive, &self.opened_drive) {
            sensitive.extend(
                [
                    path_text(source),
                    path_text(opened),
                    SNAPSHOT_DATA_ID,
                    SNAPSHOT_DATA_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if let (Some(source), Some(opened)) = (&self.serial, &self.opened_serial) {
            sensitive.extend(
                [
                    path_text(source),
                    path_text(opened),
                    SNAPSHOT_SERIAL_SINK_ID,
                    SNAPSHOT_SERIAL_SINK_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if let (Some(source), Some(opened)) = (&self.initrd, &self.opened_initrd) {
            sensitive.extend(
                [
                    path_text(source),
                    path_text(opened),
                    SNAPSHOT_INITRD_ID,
                    SNAPSHOT_INITRD_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if let Some(directory) = self.api_directory.as_ref() {
            sensitive.extend(
                [
                    path_text(directory),
                    path_text(&directory.join(API_SOCKET_CHILD)),
                    API_SOCKET_DIRECTORY_ID,
                    API_SOCKET_REF,
                    API_SOCKET_CHILD,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        sensitive
    }
}

#[derive(Debug)]
struct SerialSnapshotInputGrantFixture {
    _root: TestDir,
    _socket_root: Option<TestDir>,
    manifest: PathBuf,
    sources: SerialSnapshotGrantArtifacts,
    opened: SerialSnapshotGrantArtifacts,
    metrics: PathBuf,
    opened_metrics: PathBuf,
    serial: Option<PathBuf>,
    opened_serial: Option<PathBuf>,
    state_directory: Option<PathBuf>,
    memory_directory: Option<PathBuf>,
    api_directory: Option<PathBuf>,
}

impl SerialSnapshotInputGrantFixture {
    fn new(case: &str, sources: SerialSnapshotGrantArtifacts, configured_output: bool) -> Self {
        Self::new_internal(case, sources, configured_output, false, false)
    }

    fn new_entropy(
        case: &str,
        sources: SerialSnapshotGrantArtifacts,
        with_recapture: bool,
    ) -> Self {
        Self::new_internal(case, sources, false, with_recapture, true)
    }

    fn new_internal(
        case: &str,
        sources: SerialSnapshotGrantArtifacts,
        configured_output: bool,
        with_recapture: bool,
        with_api_socket: bool,
    ) -> Self {
        let root = TestDir::new(&format!("serial-snapshot-input-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("serial snapshot input root should canonicalize");
        let manifest = canonical_root.join("grant-manifest.json");
        let metrics = canonical_root.join("serial-snapshot.metrics");
        let opened_metrics = canonical_root.join("opened-serial-snapshot.metrics");
        let serial = configured_output.then(|| canonical_root.join("serial-snapshot.out"));
        let opened_serial = serial
            .as_ref()
            .map(|_| canonical_root.join("opened-serial-snapshot.out"));
        let state_directory =
            with_recapture.then(|| canonical_root.join("recaptured-state-output"));
        let memory_directory =
            with_recapture.then(|| canonical_root.join("recaptured-memory-output"));
        let socket_root = with_api_socket.then(|| {
            let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
            let root = TestDir(
                PathBuf::from("/private/tmp")
                    .join(format!("bbed-{}-{socket_id}", std::process::id())),
            );
            fs::create_dir(root.path())
                .expect("short entropy destination socket root should create");
            root
        });
        let api_directory = socket_root.as_ref().map(|root| root.path().join("a"));
        fs::write(&metrics, b"").expect("serial destination metrics should write");
        if let Some(serial) = serial.as_ref() {
            fs::write(serial, b"").expect("serial destination output should write");
        }
        if let Some(directory) = state_directory.as_ref() {
            fs::create_dir(directory).expect("serial recapture state directory should create");
        }
        if let Some(directory) = memory_directory.as_ref() {
            fs::create_dir(directory).expect("serial recapture memory directory should create");
        }
        if let Some(directory) = api_directory.as_ref() {
            fs::create_dir(directory).expect("serial destination API directory should create");
        }
        let opened = SerialSnapshotGrantArtifacts {
            state: replacement_opened_path(&sources.state, case),
            memory: replacement_opened_path(&sources.memory, case),
            drive: sources
                .drive
                .as_ref()
                .map(|drive| replacement_opened_path(drive, case)),
        };
        let mut grants = vec![
            serde_json::json!({
                "id": SNAPSHOT_STATE_INPUT_ID,
                "role": "snapshot-state-input",
                "access": "read-only",
                "source": path_text(&sources.state),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_INPUT_ID,
                "role": "snapshot-memory-input",
                "access": "read-only",
                "source": path_text(&sources.memory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_METRICS_ID,
                "role": "metrics-sink",
                "access": "write-only",
                "source": path_text(&metrics),
            }),
        ];
        if let Some(drive) = sources.drive.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_DATA_ID,
                "role": "drive-backing",
                "access": "read-write",
                "source": path_text(drive),
            }));
        }
        if let Some(serial) = serial.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_SERIAL_SINK_ID,
                "role": "serial-sink",
                "access": "write-only",
                "source": path_text(serial),
            }));
        }
        if let Some(directory) = state_directory.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_STATE_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(directory),
            }));
        }
        if let Some(directory) = memory_directory.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(directory),
            }));
        }
        if let Some(directory) = api_directory.as_ref() {
            grants.push(serde_json::json!({
                "id": API_SOCKET_DIRECTORY_ID,
                "role": "api-socket-directory",
                "access": "create-children",
                "source": path_text(directory),
            }));
        }
        let manifest_json = serde_json::json!({"version": 1, "grants": grants});
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json)
                .expect("serial snapshot input manifest should serialize"),
        )
        .expect("serial snapshot input manifest should write");
        Self {
            _root: root,
            _socket_root: socket_root,
            manifest,
            sources,
            opened,
            metrics,
            opened_metrics,
            serial,
            opened_serial,
            state_directory,
            memory_directory,
            api_directory,
        }
    }

    fn replace_source_pathnames(&self) -> SerialSnapshotGrantArtifacts {
        for (source, opened) in [
            (&self.sources.state, &self.opened.state),
            (&self.sources.memory, &self.opened.memory),
            (&self.metrics, &self.opened_metrics),
        ] {
            fs::rename(source, opened).expect("opened serial snapshot input should move");
        }
        fs::write(&self.sources.state, b"replacement state must not load")
            .expect("replacement serial snapshot state should write");
        fs::write(&self.sources.memory, b"replacement memory must not load")
            .expect("replacement serial snapshot memory should write");
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement serial destination metrics should write");
        if let (Some(source), Some(opened)) = (&self.sources.drive, &self.opened.drive) {
            fs::rename(source, opened).expect("opened serial snapshot input drive should move");
            fs::write(source, vec![0xee_u8; 4096])
                .expect("replacement serial snapshot input drive should write");
        }
        if let (Some(source), Some(opened)) = (&self.serial, &self.opened_serial) {
            fs::rename(source, opened).expect("opened serial destination output should move");
            fs::write(source, b"replacement serial output must remain unused\n")
                .expect("replacement serial destination output should write");
        }
        self.opened.clone()
    }

    fn recaptured_artifacts(&self) -> SerialSnapshotGrantArtifacts {
        SerialSnapshotGrantArtifacts {
            state: self
                .state_directory
                .as_ref()
                .expect("serial recapture state directory should exist")
                .join(SNAPSHOT_STATE_CHILD),
            memory: self
                .memory_directory
                .as_ref()
                .expect("serial recapture memory directory should exist")
                .join(SNAPSHOT_MEMORY_CHILD),
            drive: self.opened.drive.clone(),
        }
    }

    fn api_socket(&self) -> PathBuf {
        self.api_directory
            .as_ref()
            .expect("entropy destination should grant an API socket directory")
            .join(API_SOCKET_CHILD)
    }

    fn assert_no_recapture_staging(&self) {
        assert_no_snapshot_staging(
            self.state_directory
                .as_ref()
                .expect("serial recapture state directory should exist"),
        );
        assert_no_snapshot_staging(
            self.memory_directory
                .as_ref()
                .expect("serial recapture memory directory should exist"),
        );
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = [
            path_text(&self.manifest),
            path_text(&self.sources.state),
            path_text(&self.sources.memory),
            path_text(&self.opened.state),
            path_text(&self.opened.memory),
            path_text(&self.metrics),
            path_text(&self.opened_metrics),
            SNAPSHOT_STATE_INPUT_ID,
            SNAPSHOT_MEMORY_INPUT_ID,
            SNAPSHOT_METRICS_ID,
            SNAPSHOT_STATE_INPUT_REF,
            SNAPSHOT_MEMORY_INPUT_REF,
            SNAPSHOT_METRICS_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if let (Some(source), Some(opened)) = (&self.sources.drive, &self.opened.drive) {
            sensitive.extend(
                [
                    path_text(source),
                    path_text(opened),
                    SNAPSHOT_DATA_ID,
                    SNAPSHOT_DATA_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if let (Some(source), Some(opened)) = (&self.serial, &self.opened_serial) {
            sensitive.extend(
                [
                    path_text(source),
                    path_text(opened),
                    SNAPSHOT_SERIAL_SINK_ID,
                    SNAPSHOT_SERIAL_SINK_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if let (Some(state), Some(memory)) = (&self.state_directory, &self.memory_directory) {
            sensitive.extend(
                [
                    path_text(state),
                    path_text(memory),
                    SNAPSHOT_STATE_OUTPUT_ID,
                    SNAPSHOT_MEMORY_OUTPUT_ID,
                    SNAPSHOT_STATE_OUTPUT_REF,
                    SNAPSHOT_MEMORY_OUTPUT_REF,
                    SNAPSHOT_STATE_CHILD,
                    SNAPSHOT_MEMORY_CHILD,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if let Some(directory) = self.api_directory.as_ref() {
            sensitive.extend(
                [
                    path_text(directory),
                    path_text(&directory.join(API_SOCKET_CHILD)),
                    API_SOCKET_DIRECTORY_ID,
                    API_SOCKET_REF,
                    API_SOCKET_CHILD,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        sensitive
    }
}

#[derive(Debug, Clone)]
struct SnapshotEpochBlockArtifacts {
    root: PathBuf,
    data: PathBuf,
    audit: PathBuf,
}

#[derive(Debug, Clone)]
struct SnapshotEpochArtifactSet {
    state: PathBuf,
    memory: PathBuf,
    blocks: Option<SnapshotEpochBlockArtifacts>,
    writable_pmem: PathBuf,
    read_only_pmem: PathBuf,
}

#[derive(Debug)]
struct SnapshotSourceGrantFixture {
    _root: TestDir,
    manifest: PathBuf,
    kernel: PathBuf,
    metrics: PathBuf,
    root_backing: PathBuf,
    data_backing: PathBuf,
    audit_backing: PathBuf,
    state_directory: PathBuf,
    memory_directory: PathBuf,
    opened_kernel: PathBuf,
    opened_metrics: PathBuf,
    opened_root_backing: PathBuf,
    opened_data_backing: PathBuf,
    opened_audit_backing: PathBuf,
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
        let metrics = canonical_root.join("snapshot.metrics");
        let root_backing = canonical_root.join("snapshot-root.img");
        let data_backing = canonical_root.join("snapshot-data.img");
        let audit_backing = canonical_root.join("snapshot-audit.img");
        let state_directory = canonical_root.join("state-output");
        let memory_directory = canonical_root.join("memory-output");
        let opened_kernel = canonical_root.join("opened-snapshot-kernel.image");
        let opened_metrics = canonical_root.join("opened-snapshot.metrics");
        let opened_root_backing = canonical_root.join("opened-snapshot-root.img");
        let opened_data_backing = canonical_root.join("opened-snapshot-data.img");
        let opened_audit_backing = canonical_root.join("opened-snapshot-audit.img");
        let opened_state_directory = canonical_root.join("opened-state-output");
        let opened_memory_directory = canonical_root.join("opened-memory-output");

        hard_link_or_copy_fixture(&guest_kernel(), &kernel, "snapshot guest kernel");
        fs::write(&metrics, b"").expect("snapshot metrics fixture should write");
        fs::copy(guest_ext4_rootfs(), &root_backing)
            .expect("writable snapshot root backing should copy");
        create_sized_file(&data_backing, 4096);
        create_sized_file(&audit_backing, 4096);
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
                    "id": SNAPSHOT_METRICS_ID,
                    "role": "metrics-sink",
                    "access": "write-only",
                    "source": path_text(&metrics),
                },
                {
                    "id": SNAPSHOT_ROOT_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&root_backing),
                },
                {
                    "id": SNAPSHOT_DATA_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&data_backing),
                },
                {
                    "id": SNAPSHOT_AUDIT_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&audit_backing),
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
            metrics,
            root_backing,
            data_backing,
            audit_backing,
            state_directory,
            memory_directory,
            opened_kernel,
            opened_metrics,
            opened_root_backing,
            opened_data_backing,
            opened_audit_backing,
            opened_state_directory,
            opened_memory_directory,
        }
    }

    fn replace_source_file_pathnames(&self) {
        for (source, opened) in [
            (&self.kernel, &self.opened_kernel),
            (&self.metrics, &self.opened_metrics),
            (&self.root_backing, &self.opened_root_backing),
            (&self.data_backing, &self.opened_data_backing),
            (&self.audit_backing, &self.opened_audit_backing),
        ] {
            fs::rename(source, opened).expect("launcher-opened snapshot file should move");
        }
        fs::write(&self.kernel, b"replacement kernel must not boot")
            .expect("replacement snapshot kernel should write");
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement metrics should write");
        fs::write(&self.root_backing, vec![0xff_u8; 4096])
            .expect("replacement snapshot root should write");
        fs::write(&self.data_backing, vec![0xee_u8; 4096])
            .expect("replacement snapshot data should write");
        fs::write(&self.audit_backing, vec![0xdd_u8; 4096])
            .expect("replacement snapshot audit should write");
    }

    fn assert_replacement_pathnames_unused(&self, context: &str) {
        assert_eq!(
            fs::read(&self.kernel).expect("replacement snapshot kernel should read"),
            b"replacement kernel must not boot",
            "{context} must not reopen the kernel pathname"
        );
        assert_eq!(
            fs::read(&self.metrics).expect("replacement snapshot metrics should read"),
            b"replacement metrics must remain unused\n",
            "{context} must not reopen the metrics pathname"
        );
        assert_eq!(
            fs::read(&self.root_backing).expect("replacement snapshot root should read"),
            vec![0xff_u8; 4096],
            "{context} must not reopen the root pathname"
        );
        assert_eq!(
            fs::read(&self.data_backing).expect("replacement snapshot data should read"),
            vec![0xee_u8; 4096],
            "{context} must not reopen the data pathname"
        );
        assert_eq!(
            fs::read(&self.audit_backing).expect("replacement snapshot audit should read"),
            vec![0xdd_u8; 4096],
            "{context} must not reopen the audit pathname"
        );
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
            root: self.opened_root_backing.clone(),
            data: self.opened_data_backing.clone(),
            audit: self.opened_audit_backing.clone(),
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.manifest),
            path_text(&self.kernel),
            path_text(&self.metrics),
            path_text(&self.root_backing),
            path_text(&self.data_backing),
            path_text(&self.audit_backing),
            path_text(&self.state_directory),
            path_text(&self.memory_directory),
            path_text(&self.opened_kernel),
            path_text(&self.opened_metrics),
            path_text(&self.opened_root_backing),
            path_text(&self.opened_data_backing),
            path_text(&self.opened_audit_backing),
            path_text(&self.opened_state_directory),
            path_text(&self.opened_memory_directory),
            SNAPSHOT_KERNEL_ID,
            SNAPSHOT_METRICS_ID,
            SNAPSHOT_ROOT_ID,
            SNAPSHOT_DATA_ID,
            SNAPSHOT_AUDIT_ID,
            SNAPSHOT_STATE_OUTPUT_ID,
            SNAPSHOT_MEMORY_OUTPUT_ID,
            SNAPSHOT_KERNEL_REF,
            SNAPSHOT_METRICS_REF,
            SNAPSHOT_ROOT_REF,
            SNAPSHOT_DATA_REF,
            SNAPSHOT_AUDIT_REF,
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
struct SnapshotVsockSourceGrantFixture {
    snapshot: SnapshotSourceGrantFixture,
    _socket_root: TestDir,
    vsock_directory: PathBuf,
}

impl SnapshotVsockSourceGrantFixture {
    fn new(case: &str) -> Self {
        let snapshot = SnapshotSourceGrantFixture::new(case);
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket_root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbvss-{}-{socket_id}", std::process::id())),
        );
        fs::create_dir(socket_root.path()).expect("short snapshot-vsock source root should create");
        let vsock_directory = socket_root.path().join("v");
        fs::create_dir(&vsock_directory).expect("snapshot-vsock source directory should create");
        append_snapshot_vsock_grant(
            &snapshot.manifest,
            SNAPSHOT_VSOCK_SOURCE_DIRECTORY_ID,
            &vsock_directory,
        );
        Self {
            snapshot,
            _socket_root: socket_root,
            vsock_directory,
        }
    }

    fn new_with_read_only_root(case: &str) -> Self {
        let fixture = Self::new(case);
        make_snapshot_root_grant_read_only(&fixture.snapshot.manifest);
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.vsock_directory.join(SNAPSHOT_VSOCK_SOURCE_CHILD)
    }

    fn port_path(&self, port: u32) -> PathBuf {
        snapshot_vsock_port_path(&self.socket(), port)
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = self.snapshot.sensitive_strings();
        sensitive.extend([
            path_text(&self.vsock_directory).to_owned(),
            path_text(&self.socket()).to_owned(),
            SNAPSHOT_VSOCK_SOURCE_DIRECTORY_ID.to_owned(),
            SNAPSHOT_VSOCK_SOURCE_REF.to_owned(),
            SNAPSHOT_VSOCK_SOURCE_CHILD.to_owned(),
        ]);
        sensitive
    }
}

fn append_snapshot_vsock_grant(manifest_path: &Path, id: &str, directory: &Path) {
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(manifest_path).expect("snapshot-vsock manifest should read"),
    )
    .expect("snapshot-vsock manifest should parse");
    manifest
        .get_mut("grants")
        .and_then(serde_json::Value::as_array_mut)
        .expect("snapshot-vsock manifest should contain grants")
        .push(serde_json::json!({
            "id": id,
            "role": "vsock-socket-directory",
            "access": "create-children",
            "source": path_text(directory),
        }));
    fs::write(
        manifest_path,
        serde_json::to_vec(&manifest).expect("snapshot-vsock manifest should serialize"),
    )
    .expect("snapshot-vsock manifest should update");
}

fn make_snapshot_root_grant_read_only(manifest_path: &Path) {
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(manifest_path).expect("snapshot root manifest should read"),
    )
    .expect("snapshot root manifest should parse");
    let grants = manifest
        .get_mut("grants")
        .and_then(serde_json::Value::as_array_mut)
        .expect("snapshot root manifest should contain grants");
    let mut root_grants = grants.iter_mut().filter(|grant| {
        grant.get("id").and_then(serde_json::Value::as_str) == Some(SNAPSHOT_ROOT_ID)
    });
    let root = root_grants
        .next()
        .expect("snapshot root manifest should contain its root grant");
    assert!(
        root_grants.next().is_none(),
        "snapshot root manifest should contain exactly one root grant"
    );
    assert_eq!(
        root.get("role").and_then(serde_json::Value::as_str),
        Some("drive-backing"),
        "snapshot root grant role should remain exact"
    );
    assert_eq!(
        root.get("access").and_then(serde_json::Value::as_str),
        Some("read-write"),
        "snapshot root grant should start read-write"
    );
    root["access"] = serde_json::Value::String("read-only".to_owned());
    fs::write(
        manifest_path,
        serde_json::to_vec(&manifest).expect("snapshot root manifest should serialize"),
    )
    .expect("snapshot root manifest should update");
}

fn snapshot_vsock_port_path(socket: &Path, port: u32) -> PathBuf {
    let mut path = socket.as_os_str().to_os_string();
    path.push(format!("_{port}"));
    PathBuf::from(path)
}

#[derive(Debug)]
struct SnapshotEpochSourceGrantFixture {
    _root: TestDir,
    _socket_root: TestDir,
    manifest: PathBuf,
    kernel: PathBuf,
    initrd: PathBuf,
    metrics: PathBuf,
    blocks: Option<SnapshotEpochBlockArtifacts>,
    writable_pmem: PathBuf,
    read_only_pmem: PathBuf,
    api_directory: PathBuf,
    state_directory: PathBuf,
    memory_directory: PathBuf,
    opened_kernel: PathBuf,
    opened_initrd: PathBuf,
    opened_metrics: PathBuf,
    opened_blocks: Option<SnapshotEpochBlockArtifacts>,
    opened_writable_pmem: PathBuf,
    opened_read_only_pmem: PathBuf,
}

impl SnapshotEpochSourceGrantFixture {
    fn new(case: &str, rooted: bool) -> Self {
        let root = TestDir::new(&format!("snapshot-epoch-source-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("snapshot epoch source root should canonicalize");
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket_root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbse-{}-{socket_id}", std::process::id())),
        );
        fs::create_dir(socket_root.path()).expect("short snapshot epoch socket root should create");
        let manifest = canonical_root.join("grant-manifest.json");
        let kernel = canonical_root.join("snapshot-kernel.image");
        let initrd = canonical_root.join("snapshot-initrd.cpio");
        let metrics = canonical_root.join("snapshot.metrics");
        let blocks = (!rooted).then(|| SnapshotEpochBlockArtifacts {
            root: canonical_root.join("snapshot-primary.img"),
            data: canonical_root.join("snapshot-data.img"),
            audit: canonical_root.join("snapshot-audit.img"),
        });
        let writable_pmem = canonical_root.join("snapshot-pmem-rw.img");
        let read_only_pmem = canonical_root.join("snapshot-pmem-ro.img");
        let api_directory = socket_root.path().join("a");
        let state_directory = canonical_root.join("state-output");
        let memory_directory = canonical_root.join("memory-output");
        let opened_kernel = canonical_root.join("opened-snapshot-kernel.image");
        let opened_initrd = canonical_root.join("opened-snapshot-initrd.cpio");
        let opened_metrics = canonical_root.join("opened-snapshot.metrics");
        let opened_blocks = blocks.as_ref().map(|_| SnapshotEpochBlockArtifacts {
            root: canonical_root.join("opened-snapshot-primary.img"),
            data: canonical_root.join("opened-snapshot-data.img"),
            audit: canonical_root.join("opened-snapshot-audit.img"),
        });
        let opened_writable_pmem = canonical_root.join("opened-snapshot-pmem-rw.img");
        let opened_read_only_pmem = canonical_root.join("opened-snapshot-pmem-ro.img");

        hard_link_or_copy_fixture(&guest_kernel(), &kernel, "snapshot epoch guest kernel");
        hard_link_or_copy_fixture(&guest_initrd(), &initrd, "snapshot epoch guest initrd");
        fs::write(&metrics, b"").expect("snapshot epoch metrics fixture should write");
        if let Some(blocks) = blocks.as_ref() {
            create_snapshot_block_epoch_backing(&blocks.root, SNAPSHOT_BLOCK_DRIVE_A_INITIAL_BYTE);
            create_snapshot_block_epoch_backing(&blocks.data, SNAPSHOT_BLOCK_DRIVE_B_INITIAL_BYTE);
            create_snapshot_block_epoch_backing(&blocks.audit, SNAPSHOT_BLOCK_AUDIT_BYTE);
        }
        create_snapshot_pmem_epoch_backing(&writable_pmem, SNAPSHOT_PMEM_WRITABLE_INITIAL_BYTE);
        create_snapshot_pmem_epoch_backing(&read_only_pmem, SNAPSHOT_PMEM_READ_ONLY_BYTE);
        fs::create_dir(&api_directory).expect("epoch API directory should create");
        fs::create_dir(&state_directory).expect("epoch state output directory should create");
        fs::create_dir(&memory_directory).expect("epoch memory output directory should create");

        let mut grants = vec![
            serde_json::json!({
                "id": SNAPSHOT_KERNEL_ID,
                "role": "kernel-image",
                "access": "read-only",
                "source": path_text(&kernel),
            }),
            serde_json::json!({
                "id": SNAPSHOT_INITRD_ID,
                "role": "initrd-image",
                "access": "read-only",
                "source": path_text(&initrd),
            }),
            serde_json::json!({
                "id": SNAPSHOT_METRICS_ID,
                "role": "metrics-sink",
                "access": "write-only",
                "source": path_text(&metrics),
            }),
        ];
        if let Some(blocks) = blocks.as_ref() {
            grants.extend([
                serde_json::json!({
                    "id": SNAPSHOT_ROOT_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&blocks.root),
                }),
                serde_json::json!({
                    "id": SNAPSHOT_DATA_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&blocks.data),
                }),
                serde_json::json!({
                    "id": SNAPSHOT_AUDIT_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&blocks.audit),
                }),
            ]);
        }
        grants.extend([
            serde_json::json!({
                "id": SNAPSHOT_PMEM_RW_ID,
                "role": "pmem-backing",
                "access": "read-write",
                "source": path_text(&writable_pmem),
            }),
            serde_json::json!({
                "id": SNAPSHOT_PMEM_RO_ID,
                "role": "pmem-backing",
                "access": "read-only",
                "source": path_text(&read_only_pmem),
            }),
            serde_json::json!({
                "id": API_SOCKET_DIRECTORY_ID,
                "role": "api-socket-directory",
                "access": "create-children",
                "source": path_text(&api_directory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_STATE_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(&state_directory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(&memory_directory),
            }),
        ]);
        let manifest_json = serde_json::json!({"version": 1, "grants": grants});
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("snapshot epoch manifest should serialize"),
        )
        .expect("snapshot epoch manifest should write");

        Self {
            _root: root,
            _socket_root: socket_root,
            manifest,
            kernel,
            initrd,
            metrics,
            blocks,
            writable_pmem,
            read_only_pmem,
            api_directory,
            state_directory,
            memory_directory,
            opened_kernel,
            opened_initrd,
            opened_metrics,
            opened_blocks,
            opened_writable_pmem,
            opened_read_only_pmem,
        }
    }

    fn replace_source_file_pathnames(&self) {
        for (source, opened) in [
            (&self.kernel, &self.opened_kernel),
            (&self.initrd, &self.opened_initrd),
            (&self.metrics, &self.opened_metrics),
            (&self.writable_pmem, &self.opened_writable_pmem),
            (&self.read_only_pmem, &self.opened_read_only_pmem),
        ] {
            fs::rename(source, opened).expect("launcher-opened snapshot epoch file should move");
        }
        if let (Some(blocks), Some(opened)) = (&self.blocks, &self.opened_blocks) {
            for (source, opened) in [
                (&blocks.root, &opened.root),
                (&blocks.data, &opened.data),
                (&blocks.audit, &opened.audit),
            ] {
                fs::rename(source, opened)
                    .expect("launcher-opened snapshot epoch block file should move");
            }
        }
        fs::write(&self.kernel, b"replacement kernel must not boot")
            .expect("replacement snapshot epoch kernel should write");
        fs::write(&self.initrd, b"replacement initrd must not boot")
            .expect("replacement snapshot epoch initrd should write");
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement snapshot epoch metrics should write");
        create_snapshot_pmem_epoch_backing(
            &self.writable_pmem,
            SNAPSHOT_PMEM_WRITABLE_REPLACEMENT_BYTE,
        );
        create_snapshot_pmem_epoch_backing(
            &self.read_only_pmem,
            SNAPSHOT_PMEM_READ_ONLY_REPLACEMENT_BYTE,
        );
        if let Some(blocks) = self.blocks.as_ref() {
            fs::write(&blocks.root, vec![0xff_u8; 4096])
                .expect("replacement snapshot epoch primary should write");
            fs::write(&blocks.data, vec![0xee_u8; 4096])
                .expect("replacement snapshot epoch data should write");
            fs::write(&blocks.audit, vec![0xdd_u8; 4096])
                .expect("replacement snapshot epoch audit should write");
        }
    }

    fn artifacts(&self) -> SnapshotEpochArtifactSet {
        self.artifacts_with_children(SNAPSHOT_STATE_CHILD, SNAPSHOT_MEMORY_CHILD)
    }

    fn repeated_artifacts(&self) -> SnapshotEpochArtifactSet {
        self.artifacts_with_children(SNAPSHOT_REPEAT_STATE_CHILD, SNAPSHOT_REPEAT_MEMORY_CHILD)
    }

    fn api_socket(&self) -> PathBuf {
        self.api_directory.join(API_SOCKET_CHILD)
    }

    fn artifacts_with_children(
        &self,
        state_child: &str,
        memory_child: &str,
    ) -> SnapshotEpochArtifactSet {
        SnapshotEpochArtifactSet {
            state: self.state_directory.join(state_child),
            memory: self.memory_directory.join(memory_child),
            blocks: self.opened_blocks.clone(),
            writable_pmem: self.opened_writable_pmem.clone(),
            read_only_pmem: self.opened_read_only_pmem.clone(),
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = [
            path_text(&self.manifest),
            path_text(&self.kernel),
            path_text(&self.initrd),
            path_text(&self.metrics),
            path_text(&self.writable_pmem),
            path_text(&self.read_only_pmem),
            path_text(&self.api_directory),
            path_text(&self.state_directory),
            path_text(&self.memory_directory),
            path_text(&self.opened_kernel),
            path_text(&self.opened_initrd),
            path_text(&self.opened_metrics),
            path_text(&self.opened_writable_pmem),
            path_text(&self.opened_read_only_pmem),
            SNAPSHOT_KERNEL_ID,
            SNAPSHOT_INITRD_ID,
            SNAPSHOT_METRICS_ID,
            SNAPSHOT_PMEM_RW_ID,
            SNAPSHOT_PMEM_RO_ID,
            API_SOCKET_DIRECTORY_ID,
            SNAPSHOT_STATE_OUTPUT_ID,
            SNAPSHOT_MEMORY_OUTPUT_ID,
            SNAPSHOT_KERNEL_REF,
            SNAPSHOT_INITRD_REF,
            SNAPSHOT_METRICS_REF,
            SNAPSHOT_PMEM_RW_REF,
            SNAPSHOT_PMEM_RO_REF,
            API_SOCKET_REF,
            API_SOCKET_CHILD,
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
        .collect::<Vec<_>>();
        if let (Some(blocks), Some(opened)) = (&self.blocks, &self.opened_blocks) {
            sensitive.extend(
                [
                    path_text(&blocks.root),
                    path_text(&blocks.data),
                    path_text(&blocks.audit),
                    path_text(&opened.root),
                    path_text(&opened.data),
                    path_text(&opened.audit),
                    SNAPSHOT_ROOT_ID,
                    SNAPSHOT_DATA_ID,
                    SNAPSHOT_AUDIT_ID,
                    SNAPSHOT_ROOT_REF,
                    SNAPSHOT_DATA_REF,
                    SNAPSHOT_AUDIT_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        sensitive
    }
}

fn hard_link_or_copy_fixture(source: &Path, destination: &Path, context: &str) {
    if let Err(link_error) = fs::hard_link(source, destination) {
        fs::copy(source, destination).unwrap_or_else(|copy_error| {
            panic!(
                "{context} should hard-link or copy from {} to {}: hard-link failed: {link_error}; copy failed: {copy_error}",
                source.display(),
                destination.display()
            )
        });
    }
}

#[derive(Debug)]
struct SnapshotContinuationInputGrantFixture {
    _root: TestDir,
    _socket_root: TestDir,
    manifest: PathBuf,
    sources: SnapshotArtifactSet,
    opened: SnapshotArtifactSet,
    metrics: PathBuf,
    opened_metrics: PathBuf,
    api_directory: PathBuf,
    state_directory: Option<PathBuf>,
    memory_directory: Option<PathBuf>,
}

impl SnapshotContinuationInputGrantFixture {
    fn new(case: &str, sources: SnapshotArtifactSet, with_recapture: bool) -> Self {
        let root = TestDir::new(&format!("balloon-snapshot-input-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("balloon snapshot input root should canonicalize");
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket_root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbbs-{}-{socket_id}", std::process::id())),
        );
        fs::create_dir(socket_root.path())
            .expect("short balloon destination socket root should create");
        let manifest = canonical_root.join("grant-manifest.json");
        let metrics = canonical_root.join("balloon-snapshot.metrics");
        let opened_metrics = canonical_root.join("opened-balloon-snapshot.metrics");
        let api_directory = socket_root.path().join("a");
        let state_directory =
            with_recapture.then(|| canonical_root.join("recaptured-state-output"));
        let memory_directory =
            with_recapture.then(|| canonical_root.join("recaptured-memory-output"));
        let opened = SnapshotArtifactSet {
            state: replacement_opened_path(&sources.state, case),
            memory: replacement_opened_path(&sources.memory, case),
            root: replacement_opened_path(&sources.root, case),
            data: replacement_opened_path(&sources.data, case),
            audit: replacement_opened_path(&sources.audit, case),
        };

        fs::write(&metrics, b"").expect("balloon destination metrics should write");
        fs::create_dir(&api_directory)
            .expect("balloon destination API socket directory should create");
        if let Some(directory) = state_directory.as_ref() {
            fs::create_dir(directory)
                .expect("balloon recapture state output directory should create");
        }
        if let Some(directory) = memory_directory.as_ref() {
            fs::create_dir(directory)
                .expect("balloon recapture memory output directory should create");
        }

        let mut grants = vec![
            serde_json::json!({
                "id": SNAPSHOT_STATE_INPUT_ID,
                "role": "snapshot-state-input",
                "access": "read-only",
                "source": path_text(&sources.state),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_INPUT_ID,
                "role": "snapshot-memory-input",
                "access": "read-only",
                "source": path_text(&sources.memory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_METRICS_ID,
                "role": "metrics-sink",
                "access": "write-only",
                "source": path_text(&metrics),
            }),
            serde_json::json!({
                "id": SNAPSHOT_ROOT_ID,
                "role": "drive-backing",
                "access": "read-write",
                "source": path_text(&sources.root),
            }),
            serde_json::json!({
                "id": SNAPSHOT_DATA_ID,
                "role": "drive-backing",
                "access": "read-write",
                "source": path_text(&sources.data),
            }),
            serde_json::json!({
                "id": SNAPSHOT_AUDIT_ID,
                "role": "drive-backing",
                "access": "read-only",
                "source": path_text(&sources.audit),
            }),
            serde_json::json!({
                "id": API_SOCKET_DIRECTORY_ID,
                "role": "api-socket-directory",
                "access": "create-children",
                "source": path_text(&api_directory),
            }),
        ];
        if let Some(directory) = state_directory.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_STATE_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(directory),
            }));
        }
        if let Some(directory) = memory_directory.as_ref() {
            grants.push(serde_json::json!({
                "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(directory),
            }));
        }
        let manifest_json = serde_json::json!({"version": 1, "grants": grants});
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json)
                .expect("balloon snapshot input manifest should serialize"),
        )
        .expect("balloon snapshot input manifest should write");

        Self {
            _root: root,
            _socket_root: socket_root,
            manifest,
            sources,
            opened,
            metrics,
            opened_metrics,
            api_directory,
            state_directory,
            memory_directory,
        }
    }

    fn replace_source_pathnames(&self) -> SnapshotArtifactSet {
        for (source, opened) in [
            (&self.sources.state, &self.opened.state),
            (&self.sources.memory, &self.opened.memory),
            (&self.sources.root, &self.opened.root),
            (&self.sources.data, &self.opened.data),
            (&self.sources.audit, &self.opened.audit),
            (&self.metrics, &self.opened_metrics),
        ] {
            fs::rename(source, opened).expect("opened balloon snapshot input should move");
        }
        fs::write(&self.sources.state, b"replacement state must not load")
            .expect("replacement balloon snapshot state should write");
        fs::write(&self.sources.memory, b"replacement memory must not load")
            .expect("replacement balloon snapshot memory should write");
        fs::write(&self.sources.root, vec![0xff_u8; 4096])
            .expect("replacement balloon snapshot root should write");
        fs::write(&self.sources.data, vec![0xee_u8; 4096])
            .expect("replacement balloon snapshot data should write");
        fs::write(&self.sources.audit, vec![0xdd_u8; 4096])
            .expect("replacement balloon snapshot audit should write");
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement balloon destination metrics should write");
        self.opened.clone()
    }

    fn assert_replacement_pathnames_unused(&self, context: &str) {
        assert_eq!(
            fs::read(&self.sources.state).expect("replacement snapshot state should read"),
            b"replacement state must not load",
            "{context} must not reopen the state pathname"
        );
        assert_eq!(
            fs::read(&self.sources.memory).expect("replacement snapshot memory should read"),
            b"replacement memory must not load",
            "{context} must not reopen the memory pathname"
        );
        assert_eq!(
            fs::read(&self.sources.root).expect("replacement snapshot root should read"),
            vec![0xff_u8; 4096],
            "{context} must not reopen the root pathname"
        );
        assert_eq!(
            fs::read(&self.sources.data).expect("replacement snapshot data should read"),
            vec![0xee_u8; 4096],
            "{context} must not reopen the data pathname"
        );
        assert_eq!(
            fs::read(&self.sources.audit).expect("replacement snapshot audit should read"),
            vec![0xdd_u8; 4096],
            "{context} must not reopen the audit pathname"
        );
        assert_eq!(
            fs::read(&self.metrics).expect("replacement snapshot metrics should read"),
            b"replacement metrics must remain unused\n",
            "{context} must not reopen the metrics pathname"
        );
    }

    fn recaptured_artifacts(&self) -> SnapshotArtifactSet {
        SnapshotArtifactSet {
            state: self
                .state_directory
                .as_ref()
                .expect("balloon recapture state directory should exist")
                .join(SNAPSHOT_STATE_CHILD),
            memory: self
                .memory_directory
                .as_ref()
                .expect("balloon recapture memory directory should exist")
                .join(SNAPSHOT_MEMORY_CHILD),
            root: self.opened.root.clone(),
            data: self.opened.data.clone(),
            audit: self.opened.audit.clone(),
        }
    }

    fn api_socket(&self) -> PathBuf {
        self.api_directory.join(API_SOCKET_CHILD)
    }

    fn assert_no_recapture_staging(&self) {
        assert_no_snapshot_staging(
            self.state_directory
                .as_ref()
                .expect("balloon recapture state directory should exist"),
        );
        assert_no_snapshot_staging(
            self.memory_directory
                .as_ref()
                .expect("balloon recapture memory directory should exist"),
        );
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = [
            path_text(&self.manifest),
            path_text(&self.sources.state),
            path_text(&self.sources.memory),
            path_text(&self.sources.root),
            path_text(&self.sources.data),
            path_text(&self.sources.audit),
            path_text(&self.opened.state),
            path_text(&self.opened.memory),
            path_text(&self.opened.root),
            path_text(&self.opened.data),
            path_text(&self.opened.audit),
            path_text(&self.metrics),
            path_text(&self.opened_metrics),
            path_text(&self.api_directory),
            path_text(&self.api_socket()),
            SNAPSHOT_STATE_INPUT_ID,
            SNAPSHOT_MEMORY_INPUT_ID,
            SNAPSHOT_METRICS_ID,
            SNAPSHOT_ROOT_ID,
            SNAPSHOT_DATA_ID,
            SNAPSHOT_AUDIT_ID,
            SNAPSHOT_STATE_INPUT_REF,
            SNAPSHOT_MEMORY_INPUT_REF,
            SNAPSHOT_METRICS_REF,
            SNAPSHOT_ROOT_REF,
            SNAPSHOT_DATA_REF,
            SNAPSHOT_AUDIT_REF,
            API_SOCKET_DIRECTORY_ID,
            API_SOCKET_REF,
            API_SOCKET_CHILD,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if let (Some(state), Some(memory)) = (&self.state_directory, &self.memory_directory) {
            sensitive.extend(
                [
                    path_text(state),
                    path_text(memory),
                    SNAPSHOT_STATE_OUTPUT_ID,
                    SNAPSHOT_MEMORY_OUTPUT_ID,
                    SNAPSHOT_STATE_OUTPUT_REF,
                    SNAPSHOT_MEMORY_OUTPUT_REF,
                    SNAPSHOT_STATE_CHILD,
                    SNAPSHOT_MEMORY_CHILD,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        sensitive
    }
}

#[derive(Debug)]
struct SnapshotVsockContinuationInputGrantFixture {
    snapshot: SnapshotContinuationInputGrantFixture,
    _vsock_root: TestDir,
    vsock_directory: PathBuf,
    selector_id: &'static str,
    selector_ref: &'static str,
    selector_child: &'static str,
}

impl SnapshotVsockContinuationInputGrantFixture {
    fn new(
        case: &str,
        sources: SnapshotArtifactSet,
        with_recapture: bool,
        use_override: bool,
    ) -> Self {
        let snapshot = SnapshotContinuationInputGrantFixture::new(case, sources, with_recapture);
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let vsock_root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbvsd-{}-{socket_id}", std::process::id())),
        );
        fs::create_dir(vsock_root.path())
            .expect("short snapshot-vsock destination root should create");
        let vsock_directory = vsock_root.path().join("v");
        fs::create_dir(&vsock_directory)
            .expect("snapshot-vsock destination directory should create");
        let (selector_id, selector_ref, selector_child) = if use_override {
            (
                SNAPSHOT_VSOCK_OVERRIDE_DIRECTORY_ID,
                SNAPSHOT_VSOCK_OVERRIDE_REF,
                SNAPSHOT_VSOCK_OVERRIDE_CHILD,
            )
        } else {
            (
                SNAPSHOT_VSOCK_SOURCE_DIRECTORY_ID,
                SNAPSHOT_VSOCK_SOURCE_REF,
                SNAPSHOT_VSOCK_SOURCE_CHILD,
            )
        };
        append_snapshot_vsock_grant(&snapshot.manifest, selector_id, &vsock_directory);
        Self {
            snapshot,
            _vsock_root: vsock_root,
            vsock_directory,
            selector_id,
            selector_ref,
            selector_child,
        }
    }

    fn new_with_read_only_root(
        case: &str,
        sources: SnapshotArtifactSet,
        with_recapture: bool,
        use_override: bool,
    ) -> Self {
        let fixture = Self::new(case, sources, with_recapture, use_override);
        make_snapshot_root_grant_read_only(&fixture.snapshot.manifest);
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.vsock_directory.join(self.selector_child)
    }

    fn port_path(&self, port: u32) -> PathBuf {
        snapshot_vsock_port_path(&self.socket(), port)
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = self.snapshot.sensitive_strings();
        sensitive.extend([
            path_text(&self.vsock_directory).to_owned(),
            path_text(&self.socket()).to_owned(),
            self.selector_id.to_owned(),
            self.selector_ref.to_owned(),
            self.selector_child.to_owned(),
        ]);
        sensitive
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
            root: replacement_opened_path(&sources.root, case),
            data: replacement_opened_path(&sources.data, case),
            audit: replacement_opened_path(&sources.audit, case),
        };
        let grants = vec![
            serde_json::json!({
                "id": SNAPSHOT_STATE_INPUT_ID,
                "role": "snapshot-state-input",
                "access": "read-only",
                "source": path_text(&sources.state),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_INPUT_ID,
                "role": "snapshot-memory-input",
                "access": "read-only",
                "source": path_text(&sources.memory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_ROOT_ID,
                "role": "drive-backing",
                "access": "read-write",
                "source": path_text(&sources.root),
            }),
            serde_json::json!({
                "id": SNAPSHOT_DATA_ID,
                "role": "drive-backing",
                "access": "read-write",
                "source": path_text(&sources.data),
            }),
            serde_json::json!({
                "id": SNAPSHOT_AUDIT_ID,
                "role": "drive-backing",
                "access": "read-only",
                "source": path_text(&sources.audit),
            }),
        ];
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": grants,
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
            (&self.sources.root, &self.opened.root),
            (&self.sources.data, &self.opened.data),
            (&self.sources.audit, &self.opened.audit),
        ] {
            fs::rename(source, opened).expect("launcher-opened snapshot input should move");
        }
        fs::write(&self.sources.state, b"replacement state must not load")
            .expect("replacement snapshot state should write");
        fs::write(&self.sources.memory, b"replacement memory must not load")
            .expect("replacement snapshot memory should write");
        fs::write(&self.sources.root, vec![0xff_u8; 4096])
            .expect("replacement snapshot root must not load");
        fs::write(&self.sources.data, vec![0xee_u8; 4096])
            .expect("replacement snapshot data must not load");
        fs::write(&self.sources.audit, vec![0xdd_u8; 4096])
            .expect("replacement snapshot audit must not load");
        self.opened.clone()
    }

    fn assert_replacement_pathnames_unused(&self, context: &str) {
        assert_eq!(
            fs::read(&self.sources.state).expect("replacement snapshot state should read"),
            b"replacement state must not load",
            "{context} must not reopen the state pathname"
        );
        assert_eq!(
            fs::read(&self.sources.memory).expect("replacement snapshot memory should read"),
            b"replacement memory must not load",
            "{context} must not reopen the memory pathname"
        );
        assert_eq!(
            fs::read(&self.sources.root).expect("replacement snapshot root should read"),
            vec![0xff_u8; 4096],
            "{context} must not reopen the root pathname"
        );
        assert_eq!(
            fs::read(&self.sources.data).expect("replacement snapshot data should read"),
            vec![0xee_u8; 4096],
            "{context} must not reopen the data pathname"
        );
        assert_eq!(
            fs::read(&self.sources.audit).expect("replacement snapshot audit should read"),
            vec![0xdd_u8; 4096],
            "{context} must not reopen the audit pathname"
        );
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.manifest),
            path_text(&self.sources.state),
            path_text(&self.sources.memory),
            path_text(&self.sources.root),
            path_text(&self.sources.data),
            path_text(&self.sources.audit),
            path_text(&self.opened.state),
            path_text(&self.opened.memory),
            path_text(&self.opened.root),
            path_text(&self.opened.data),
            path_text(&self.opened.audit),
            SNAPSHOT_STATE_INPUT_ID,
            SNAPSHOT_MEMORY_INPUT_ID,
            SNAPSHOT_ROOT_ID,
            SNAPSHOT_DATA_ID,
            SNAPSHOT_AUDIT_ID,
            SNAPSHOT_STATE_INPUT_REF,
            SNAPSHOT_MEMORY_INPUT_REF,
            SNAPSHOT_ROOT_REF,
            SNAPSHOT_DATA_REF,
            SNAPSHOT_AUDIT_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#[derive(Debug)]
struct SnapshotEpochInputGrantFixture {
    _root: TestDir,
    _socket_root: TestDir,
    manifest: PathBuf,
    sources: SnapshotEpochArtifactSet,
    opened: SnapshotEpochArtifactSet,
    metrics: PathBuf,
    opened_metrics: PathBuf,
    api_directory: PathBuf,
    state_directory: PathBuf,
    memory_directory: PathBuf,
}

impl SnapshotEpochInputGrantFixture {
    fn new(case: &str, sources: SnapshotEpochArtifactSet) -> Self {
        let root = TestDir::new(&format!("snapshot-epoch-input-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("snapshot epoch input root should canonicalize");
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let socket_root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbsi-{}-{socket_id}", std::process::id())),
        );
        fs::create_dir(socket_root.path()).expect("short snapshot input socket root should create");
        let manifest = canonical_root.join("grant-manifest.json");
        let metrics = canonical_root.join("snapshot.metrics");
        let opened_metrics = canonical_root.join("opened-snapshot.metrics");
        let api_directory = socket_root.path().join("a");
        let state_directory = canonical_root.join("state-output");
        let memory_directory = canonical_root.join("memory-output");
        fs::write(&metrics, b"").expect("snapshot epoch input metrics should write");
        fs::create_dir(&api_directory).expect("snapshot epoch input API directory should create");
        fs::create_dir(&state_directory).expect("epoch recapture state directory should create");
        fs::create_dir(&memory_directory).expect("epoch recapture memory directory should create");
        let opened = SnapshotEpochArtifactSet {
            state: replacement_opened_path(&sources.state, case),
            memory: replacement_opened_path(&sources.memory, case),
            blocks: sources
                .blocks
                .as_ref()
                .map(|blocks| SnapshotEpochBlockArtifacts {
                    root: replacement_opened_path(&blocks.root, case),
                    data: replacement_opened_path(&blocks.data, case),
                    audit: replacement_opened_path(&blocks.audit, case),
                }),
            writable_pmem: replacement_opened_path(&sources.writable_pmem, case),
            read_only_pmem: replacement_opened_path(&sources.read_only_pmem, case),
        };
        let mut grants = vec![
            serde_json::json!({
                "id": SNAPSHOT_STATE_INPUT_ID,
                "role": "snapshot-state-input",
                "access": "read-only",
                "source": path_text(&sources.state),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_INPUT_ID,
                "role": "snapshot-memory-input",
                "access": "read-only",
                "source": path_text(&sources.memory),
            }),
        ];
        if let Some(blocks) = sources.blocks.as_ref() {
            grants.extend([
                serde_json::json!({
                    "id": SNAPSHOT_ROOT_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&blocks.root),
                }),
                serde_json::json!({
                    "id": SNAPSHOT_DATA_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&blocks.data),
                }),
                serde_json::json!({
                    "id": SNAPSHOT_AUDIT_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&blocks.audit),
                }),
            ]);
        }
        grants.extend([
            serde_json::json!({
                "id": SNAPSHOT_PMEM_RW_ID,
                "role": "pmem-backing",
                "access": "read-write",
                "source": path_text(&sources.writable_pmem),
            }),
            serde_json::json!({
                "id": SNAPSHOT_PMEM_RO_ID,
                "role": "pmem-backing",
                "access": "read-only",
                "source": path_text(&sources.read_only_pmem),
            }),
            serde_json::json!({
                "id": SNAPSHOT_METRICS_ID,
                "role": "metrics-sink",
                "access": "write-only",
                "source": path_text(&metrics),
            }),
            serde_json::json!({
                "id": API_SOCKET_DIRECTORY_ID,
                "role": "api-socket-directory",
                "access": "create-children",
                "source": path_text(&api_directory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_STATE_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(&state_directory),
            }),
            serde_json::json!({
                "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                "role": "snapshot-output-directory",
                "access": "create-children",
                "source": path_text(&memory_directory),
            }),
        ]);
        let manifest_json = serde_json::json!({"version": 1, "grants": grants});
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json)
                .expect("snapshot epoch input manifest should serialize"),
        )
        .expect("snapshot epoch input manifest should write");
        Self {
            _root: root,
            _socket_root: socket_root,
            manifest,
            sources,
            opened,
            metrics,
            opened_metrics,
            api_directory,
            state_directory,
            memory_directory,
        }
    }

    fn replace_source_pathnames(&self) -> SnapshotEpochArtifactSet {
        for (source, opened) in [
            (&self.sources.state, &self.opened.state),
            (&self.sources.memory, &self.opened.memory),
            (&self.sources.writable_pmem, &self.opened.writable_pmem),
            (&self.sources.read_only_pmem, &self.opened.read_only_pmem),
            (&self.metrics, &self.opened_metrics),
        ] {
            fs::rename(source, opened).expect("launcher-opened snapshot epoch input should move");
        }
        if let (Some(sources), Some(opened)) = (&self.sources.blocks, &self.opened.blocks) {
            for (source, opened) in [
                (&sources.root, &opened.root),
                (&sources.data, &opened.data),
                (&sources.audit, &opened.audit),
            ] {
                fs::rename(source, opened)
                    .expect("launcher-opened snapshot epoch block input should move");
            }
        }
        fs::write(&self.sources.state, b"replacement state must not load")
            .expect("replacement snapshot epoch state should write");
        fs::write(&self.sources.memory, b"replacement memory must not load")
            .expect("replacement snapshot epoch memory should write");
        create_snapshot_pmem_epoch_backing(
            &self.sources.writable_pmem,
            SNAPSHOT_PMEM_WRITABLE_REPLACEMENT_BYTE,
        );
        create_snapshot_pmem_epoch_backing(
            &self.sources.read_only_pmem,
            SNAPSHOT_PMEM_READ_ONLY_REPLACEMENT_BYTE,
        );
        if let Some(blocks) = self.sources.blocks.as_ref() {
            fs::write(&blocks.root, vec![0xff_u8; 4096])
                .expect("replacement snapshot epoch primary must not load");
            fs::write(&blocks.data, vec![0xee_u8; 4096])
                .expect("replacement snapshot epoch data must not load");
            fs::write(&blocks.audit, vec![0xdd_u8; 4096])
                .expect("replacement snapshot epoch audit must not load");
        }
        fs::write(&self.metrics, b"replacement metrics must remain unused\n")
            .expect("replacement snapshot epoch metrics should write");
        self.opened.clone()
    }

    fn api_socket(&self) -> PathBuf {
        self.api_directory.join(API_SOCKET_CHILD)
    }

    fn recaptured_artifacts(&self) -> SnapshotEpochArtifactSet {
        SnapshotEpochArtifactSet {
            state: self.state_directory.join(SNAPSHOT_STATE_CHILD),
            memory: self.memory_directory.join(SNAPSHOT_MEMORY_CHILD),
            blocks: self.opened.blocks.clone(),
            writable_pmem: self.opened.writable_pmem.clone(),
            read_only_pmem: self.opened.read_only_pmem.clone(),
        }
    }

    fn sensitive_strings(&self) -> Vec<String> {
        let mut sensitive = [
            path_text(&self.manifest),
            path_text(&self.sources.state),
            path_text(&self.sources.memory),
            path_text(&self.sources.writable_pmem),
            path_text(&self.sources.read_only_pmem),
            path_text(&self.opened.state),
            path_text(&self.opened.memory),
            path_text(&self.opened.writable_pmem),
            path_text(&self.opened.read_only_pmem),
            path_text(&self.metrics),
            path_text(&self.opened_metrics),
            path_text(&self.api_directory),
            path_text(&self.state_directory),
            path_text(&self.memory_directory),
            SNAPSHOT_STATE_INPUT_ID,
            SNAPSHOT_MEMORY_INPUT_ID,
            SNAPSHOT_PMEM_RW_ID,
            SNAPSHOT_PMEM_RO_ID,
            SNAPSHOT_METRICS_ID,
            API_SOCKET_DIRECTORY_ID,
            SNAPSHOT_STATE_OUTPUT_ID,
            SNAPSHOT_MEMORY_OUTPUT_ID,
            SNAPSHOT_STATE_INPUT_REF,
            SNAPSHOT_MEMORY_INPUT_REF,
            SNAPSHOT_PMEM_RW_REF,
            SNAPSHOT_PMEM_RO_REF,
            SNAPSHOT_METRICS_REF,
            API_SOCKET_REF,
            API_SOCKET_CHILD,
            SNAPSHOT_STATE_OUTPUT_REF,
            SNAPSHOT_MEMORY_OUTPUT_REF,
            SNAPSHOT_STATE_CHILD,
            SNAPSHOT_MEMORY_CHILD,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if let (Some(sources), Some(opened)) = (&self.sources.blocks, &self.opened.blocks) {
            sensitive.extend(
                [
                    path_text(&sources.root),
                    path_text(&sources.data),
                    path_text(&sources.audit),
                    path_text(&opened.root),
                    path_text(&opened.data),
                    path_text(&opened.audit),
                    SNAPSHOT_ROOT_ID,
                    SNAPSHOT_DATA_ID,
                    SNAPSHOT_AUDIT_ID,
                    SNAPSHOT_ROOT_REF,
                    SNAPSHOT_DATA_REF,
                    SNAPSHOT_AUDIT_REF,
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        sensitive
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
struct BlockSpecialGrantFixture {
    _root: TestDir,
    rootfs: PathBuf,
    control: PathBuf,
    serial: PathBuf,
    state_directory: PathBuf,
    memory_directory: PathBuf,
    first_media: MacosVirtualBlock,
    second_media: MacosVirtualBlock,
    read_only_media: MacosVirtualBlock,
    manifest: PathBuf,
}

impl BlockSpecialGrantFixture {
    fn new(case: &str) -> Self {
        let root = TestDir::new(&format!("block-special-grant-{case}"));
        let canonical_root =
            fs::canonicalize(root.path()).expect("block-special grant root should canonicalize");
        let rootfs = canonical_root.join("rootfs.ext4");
        let control = canonical_root.join("control.img");
        let serial = canonical_root.join("serial.out");
        let state_directory = canonical_root.join("state-output");
        let memory_directory = canonical_root.join("memory-output");
        let manifest = canonical_root.join("grant-manifest.json");
        fs::copy(guest_ext4_rootfs(), &rootfs).expect("block-special rootfs grant should copy");
        create_sized_file(&control, 2 * VIRTIO_BLOCK_SECTOR_BYTES);
        fs::write(&serial, b"").expect("block-special serial sink should create");
        fs::create_dir(&state_directory).expect("block-special state output should create");
        fs::create_dir(&memory_directory).expect("block-special memory output should create");

        let first_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::FourMib,
        )
        .expect("first contained block-special media should attach");
        let second_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::EightMib,
        )
        .expect("second contained block-special media should attach");
        let mut read_only_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::FourMib,
        )
        .expect("contained read-only block-special media should attach for seeding");
        write_virtual_block_marker_at(&read_only_media, 0, BLOCK_LIFECYCLE_READ_ONLY_MARKER);
        read_only_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("contained audit media should reattach read-only");
        let first_path = first_media
            .device_path()
            .expect("first contained media should expose its exact node")
            .to_path_buf();
        let second_path = second_media
            .device_path()
            .expect("second contained media should expose its exact node")
            .to_path_buf();
        let read_only_path = read_only_media
            .device_path()
            .expect("contained audit media should expose its exact node")
            .to_path_buf();

        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [
                {
                    "id": BLOCK_SPECIAL_ROOT_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&rootfs),
                },
                {
                    "id": BLOCK_SPECIAL_CONTROL_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&control),
                },
                {
                    "id": BLOCK_SPECIAL_FIRST_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&first_path),
                },
                {
                    "id": BLOCK_SPECIAL_SECOND_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&second_path),
                },
                {
                    "id": BLOCK_SPECIAL_READ_ONLY_ID,
                    "role": "drive-backing",
                    "access": "read-only",
                    "source": path_text(&read_only_path),
                },
                {
                    "id": BLOCK_SPECIAL_SERIAL_ID,
                    "role": "serial-sink",
                    "access": "write-only",
                    "source": path_text(&serial),
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
            serde_json::to_vec(&manifest_json)
                .expect("block-special grant manifest should serialize"),
        )
        .expect("block-special grant manifest should write");

        Self {
            _root: root,
            rootfs,
            control,
            serial,
            state_directory,
            memory_directory,
            first_media,
            second_media,
            read_only_media,
            manifest,
        }
    }

    fn first_path(&self) -> &Path {
        self.first_media
            .device_path()
            .expect("first contained block-special media should remain attached")
    }

    fn second_path(&self) -> &Path {
        self.second_media
            .device_path()
            .expect("second contained block-special media should remain attached")
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.rootfs),
            path_text(&self.control),
            path_text(&self.serial),
            path_text(&self.state_directory),
            path_text(&self.memory_directory),
            path_text(self.first_path()),
            path_text(self.second_path()),
            path_text(
                self.read_only_media
                    .device_path()
                    .expect("contained audit media should remain attached"),
            ),
            path_text(&self.manifest),
            BLOCK_SPECIAL_ROOT_ID,
            BLOCK_SPECIAL_CONTROL_ID,
            BLOCK_SPECIAL_FIRST_ID,
            BLOCK_SPECIAL_SECOND_ID,
            BLOCK_SPECIAL_READ_ONLY_ID,
            BLOCK_SPECIAL_SERIAL_ID,
            BLOCK_SPECIAL_ROOT_REF,
            BLOCK_SPECIAL_CONTROL_REF,
            BLOCK_SPECIAL_FIRST_REF,
            BLOCK_SPECIAL_SECOND_REF,
            BLOCK_SPECIAL_READ_ONLY_REF,
            BLOCK_SPECIAL_SERIAL_REF,
            SNAPSHOT_STATE_OUTPUT_ID,
            SNAPSHOT_MEMORY_OUTPUT_ID,
            SNAPSHOT_STATE_OUTPUT_REF,
            SNAPSHOT_MEMORY_OUTPUT_REF,
            SNAPSHOT_REPEAT_STATE_OUTPUT_REF,
            SNAPSHOT_REPEAT_MEMORY_OUTPUT_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn assert_no_snapshot_artifacts(&self) {
        for artifact in [
            self.state_directory.join(SNAPSHOT_STATE_CHILD),
            self.memory_directory.join(SNAPSHOT_MEMORY_CHILD),
            self.state_directory.join(SNAPSHOT_REPEAT_STATE_CHILD),
            self.memory_directory.join(SNAPSHOT_REPEAT_MEMORY_CHILD),
        ] {
            assert!(
                !artifact.exists(),
                "block-special native-v1 rejection must not publish an artifact"
            );
        }
        assert_no_snapshot_staging(&self.state_directory);
        assert_no_snapshot_staging(&self.memory_directory);
    }

    fn verify_persistence_and_cleanup(
        mut self,
        first_offset: u64,
        first_marker: &[u8],
        second_offset: u64,
        second_marker: &[u8],
    ) {
        self.first_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("first contained media should release for read-only inspection");
        self.second_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("second contained media should release for read-only inspection");
        assert_eq!(
            self.first_media
                .read_at(first_offset, first_marker.len())
                .expect("first contained guest marker should persist"),
            first_marker,
        );
        assert_eq!(
            self.second_media
                .read_at(second_offset, second_marker.len())
                .expect("second contained guest marker should persist"),
            second_marker,
        );
        assert_eq!(
            self.read_only_media
                .read_at(0, BLOCK_LIFECYCLE_READ_ONLY_MARKER.len())
                .expect("contained read-only audit marker should remain readable"),
            BLOCK_LIFECYCLE_READ_ONLY_MARKER,
        );
        self.first_media
            .cleanup()
            .expect("first contained media should clean up exactly");
        self.second_media
            .cleanup()
            .expect("second contained media should clean up exactly");
        self.read_only_media
            .cleanup()
            .expect("contained audit media should clean up exactly");
    }
}

#[derive(Debug)]
struct GuestDeviceGrantFixture {
    _root: TestDir,
    rootfs: PathBuf,
    data: PathBuf,
    replacement: PathBuf,
    hotplug_reuse: PathBuf,
    storage_block_one: PathBuf,
    storage_block_two: PathBuf,
    pmem: PathBuf,
    pmem_reuse: PathBuf,
    storage_pmem: PathBuf,
    read_only_data: PathBuf,
    opened_rootfs: PathBuf,
    opened_data: PathBuf,
    opened_replacement: PathBuf,
    opened_hotplug_reuse: PathBuf,
    opened_storage_block_one: PathBuf,
    opened_storage_block_two: PathBuf,
    opened_pmem: PathBuf,
    opened_pmem_reuse: PathBuf,
    opened_storage_pmem: PathBuf,
    opened_read_only_data: PathBuf,
    manifest: PathBuf,
}

#[derive(Debug)]
struct DeviceLoggerGrant {
    source: PathBuf,
    opened: PathBuf,
}

#[derive(Debug)]
struct DeviceMetricsGrant {
    source: PathBuf,
    opened: PathBuf,
}

impl DeviceLoggerGrant {
    fn add_to_manifest(manifest_path: &Path, case: &str) -> Self {
        let canonical_root = manifest_path
            .parent()
            .expect("device grant manifest should have a canonical parent");
        let logger = Self {
            source: canonical_root.join(format!("external-{case}-logger.out")),
            opened: canonical_root.join(format!("opened-{case}-logger.out")),
        };
        fs::write(&logger.source, OUTPUT_LOGGER_SEED).expect("device logger fixture should write");

        let mut manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(manifest_path).expect("device grant manifest should read"),
        )
        .expect("device grant manifest should parse");
        manifest
            .get_mut("grants")
            .and_then(serde_json::Value::as_array_mut)
            .expect("device grant manifest should contain grants")
            .push(serde_json::json!({
                "id": OUTPUT_LOGGER_ID,
                "role": "logger-sink",
                "access": "write-only",
                "source": path_text(&logger.source),
            }));
        fs::write(
            manifest_path,
            serde_json::to_vec(&manifest).expect("device logger grant should serialize"),
        )
        .expect("device logger grant manifest should write");
        logger
    }

    fn replace_source_pathname(&self) {
        fs::rename(&self.source, &self.opened).expect("launcher-opened device logger should move");
        fs::write(&self.source, OUTPUT_REPLACEMENT)
            .expect("replacement device logger should write");
    }

    fn configure(&self, socket: &Path, context: &str) {
        assert_http_status(
            &http_put(
                socket,
                "/logger",
                &serde_json::json!({"log_path": OUTPUT_LOGGER_REF}).to_string(),
            ),
            204,
            &format!("configure {context} device logger"),
        );
    }

    fn sensitive_strings(&self) -> impl Iterator<Item = String> + '_ {
        [
            path_text(&self.source),
            path_text(&self.opened),
            OUTPUT_LOGGER_ID,
            OUTPUT_LOGGER_REF,
        ]
        .into_iter()
        .map(str::to_owned)
    }

    fn assert_records(&self, expected: &[&str], forbidden: impl IntoIterator<Item = String>) {
        let output = fs::read_to_string(&self.opened)
            .expect("launcher-opened device logger output should read");
        assert!(
            output.starts_with(std::str::from_utf8(OUTPUT_LOGGER_SEED).unwrap()),
            "device logger should preserve its seeded output"
        );
        for record in expected {
            assert!(
                output.lines().any(|line| line == *record),
                "production device logger should contain {record:?}; output:\n{output}"
            );
        }
        for value in forbidden.into_iter().chain([
            path_text(&self.source).to_owned(),
            path_text(&self.opened).to_owned(),
            OUTPUT_LOGGER_ID.to_owned(),
            OUTPUT_LOGGER_REF.to_owned(),
        ]) {
            assert!(
                !output.contains(&value),
                "production device logger should redact {value:?}; output:\n{output}"
            );
        }
        assert_eq!(
            fs::read(&self.source).expect("replacement device logger should read"),
            OUTPUT_REPLACEMENT,
            "launcher-opened logger identity must not follow a pathname replacement"
        );
    }
}

impl DeviceMetricsGrant {
    fn add_to_manifest(manifest_path: &Path, case: &str) -> Self {
        let canonical_root = manifest_path
            .parent()
            .expect("device grant manifest should have a canonical parent");
        let metrics = Self {
            source: canonical_root.join(format!("external-{case}-metrics.out")),
            opened: canonical_root.join(format!("opened-{case}-metrics.out")),
        };
        fs::write(&metrics.source, OUTPUT_METRICS_SEED)
            .expect("device metrics fixture should write");

        let mut manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(manifest_path).expect("device grant manifest should read"),
        )
        .expect("device grant manifest should parse");
        manifest
            .get_mut("grants")
            .and_then(serde_json::Value::as_array_mut)
            .expect("device grant manifest should contain grants")
            .push(serde_json::json!({
                "id": OUTPUT_METRICS_ID,
                "role": "metrics-sink",
                "access": "write-only",
                "source": path_text(&metrics.source),
            }));
        fs::write(
            manifest_path,
            serde_json::to_vec(&manifest).expect("device metrics grant should serialize"),
        )
        .expect("device metrics grant manifest should write");
        metrics
    }

    fn replace_source_pathname(&self) {
        fs::rename(&self.source, &self.opened).expect("launcher-opened device metrics should move");
        fs::write(&self.source, OUTPUT_REPLACEMENT)
            .expect("replacement device metrics should write");
    }

    fn configure(&self, socket: &Path, context: &str) {
        assert_http_status(
            &http_put(
                socket,
                "/metrics",
                &serde_json::json!({"metrics_path": OUTPUT_METRICS_REF}).to_string(),
            ),
            204,
            &format!("configure {context} device metrics"),
        );
    }

    fn assert_vhost_user_metrics(
        &self,
        socket: &Path,
        expected_drives: &[(&str, bool)],
        context: &str,
        forbidden: impl IntoIterator<Item = String>,
    ) {
        assert_http_status(
            &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
            204,
            &format!("FlushMetrics for {context}"),
        );
        let output = fs::read_to_string(&self.opened)
            .expect("launcher-opened device metrics output should read");
        let seed =
            std::str::from_utf8(OUTPUT_METRICS_SEED).expect("device metrics seed should be UTF-8");
        let line = output
            .strip_prefix(seed)
            .expect("device metrics output should preserve its seed")
            .lines()
            .next_back()
            .expect("device metrics output should contain a flushed line");
        let value: serde_json::Value =
            serde_json::from_str(line).expect("device metrics line should be valid JSON");
        let exact_fields = [
            "activate_fails",
            "cfg_fails",
            "init_time_us",
            "activate_time_us",
            "config_change_time_us",
        ];
        for &(drive_id, expect_config_change) in expected_drives {
            let key = format!("vhost_user_block_{drive_id}");
            let metrics = value
                .get(&key)
                .and_then(serde_json::Value::as_object)
                .unwrap_or_else(|| panic!("{context} should contain {key}; line:\n{line}"));
            assert_eq!(metrics.len(), exact_fields.len(), "{context} {key}");
            assert!(
                exact_fields
                    .iter()
                    .all(|field| metrics.contains_key(*field)),
                "{context} {key} should contain exactly the pinned fields: {metrics:?}"
            );
            let metric = |field: &str| {
                metrics
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(|| panic!("{context} {key}.{field} should be unsigned"))
            };
            assert_eq!(metric("activate_fails"), 0, "{context} {key}");
            assert_eq!(metric("cfg_fails"), 0, "{context} {key}");
            assert!(metric("init_time_us") > 0, "{context} {key}");
            assert!(metric("activate_time_us") > 0, "{context} {key}");
            assert_eq!(
                metric("config_change_time_us") > 0,
                expect_config_change,
                "{context} {key} config-change activity"
            );
        }
        for sensitive in forbidden.into_iter().chain([
            path_text(&self.source).to_owned(),
            path_text(&self.opened).to_owned(),
            OUTPUT_METRICS_ID.to_owned(),
            OUTPUT_METRICS_REF.to_owned(),
        ]) {
            assert!(
                !line.contains(&sensitive),
                "{context} metrics should redact {sensitive:?}; line:\n{line}"
            );
        }
        assert_eq!(
            fs::read(&self.source).expect("replacement device metrics should read"),
            OUTPUT_REPLACEMENT,
            "launcher-opened metrics identity must not follow a pathname replacement"
        );
    }
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
        let storage_block_one = canonical_root.join("external-storage-block-one.img");
        let storage_block_two = canonical_root.join("external-storage-block-two.img");
        let pmem = canonical_root.join("external-pmem.img");
        let pmem_reuse = canonical_root.join("external-pmem-reuse.img");
        let storage_pmem = canonical_root.join("external-storage-pmem.img");
        let read_only_data = canonical_root.join("external-read-only-data.img");
        let opened_rootfs = canonical_root.join("opened-rootfs.ext4");
        let opened_data = canonical_root.join("opened-data.img");
        let opened_replacement = canonical_root.join("opened-replacement.img");
        let opened_hotplug_reuse = canonical_root.join("opened-hotplug-reuse.img");
        let opened_storage_block_one = canonical_root.join("opened-storage-block-one.img");
        let opened_storage_block_two = canonical_root.join("opened-storage-block-two.img");
        let opened_pmem = canonical_root.join("opened-pmem.img");
        let opened_pmem_reuse = canonical_root.join("opened-pmem-reuse.img");
        let opened_storage_pmem = canonical_root.join("opened-storage-pmem.img");
        let opened_read_only_data = canonical_root.join("opened-read-only-data.img");
        let manifest = canonical_root.join("grant-manifest.json");

        fs::copy(guest_ext4_rootfs(), &rootfs).expect("external rootfs fixture should copy");
        create_sized_file(&data, 512);
        create_sized_file(&replacement, 512);
        create_sized_file(&hotplug_reuse, 512);
        create_sized_file(&storage_block_one, 512);
        create_sized_file(&storage_block_two, 512);
        create_pmem_file(&pmem, PMEM_HOST_MARKER);
        create_pmem_file(&pmem_reuse, PMEM_HOTPLUG_HOST_TWO_MARKER);
        create_pmem_file(&storage_pmem, STORAGE_RUNTIME_PMEM_TWO_HOST_MARKER);
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
                    "id": GUEST_STORAGE_BLOCK_ONE_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&storage_block_one),
                },
                {
                    "id": GUEST_STORAGE_BLOCK_TWO_ID,
                    "role": "drive-backing",
                    "access": "read-write",
                    "source": path_text(&storage_block_two),
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
                    "id": GUEST_STORAGE_PMEM_ID,
                    "role": "pmem-backing",
                    "access": "read-write",
                    "source": path_text(&storage_pmem),
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
            storage_block_one,
            storage_block_two,
            pmem,
            pmem_reuse,
            storage_pmem,
            read_only_data,
            opened_rootfs,
            opened_data,
            opened_replacement,
            opened_hotplug_reuse,
            opened_storage_block_one,
            opened_storage_block_two,
            opened_pmem,
            opened_pmem_reuse,
            opened_storage_pmem,
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
            (&self.storage_block_one, &self.opened_storage_block_one),
            (&self.storage_block_two, &self.opened_storage_block_two),
            (&self.pmem, &self.opened_pmem),
            (&self.pmem_reuse, &self.opened_pmem_reuse),
            (&self.storage_pmem, &self.opened_storage_pmem),
            (&self.read_only_data, &self.opened_read_only_data),
        ] {
            fs::rename(source, opened).expect("launcher-opened source should move");
        }
        create_sized_file(&self.rootfs, 512);
        create_sized_file(&self.data, 512);
        create_sized_file(&self.replacement, 512);
        create_sized_file(&self.hotplug_reuse, 512);
        create_sized_file(&self.storage_block_one, 512);
        create_sized_file(&self.storage_block_two, 512);
        create_sized_file(&self.pmem, PMEM_BACKING_LEN);
        create_sized_file(&self.pmem_reuse, PMEM_BACKING_LEN);
        create_sized_file(&self.storage_pmem, PMEM_BACKING_LEN);
        create_sized_file(&self.read_only_data, 512);
    }

    fn add_logger_grant(&self, case: &str) -> DeviceLoggerGrant {
        DeviceLoggerGrant::add_to_manifest(&self.manifest, case)
    }

    fn add_metrics_grant(&self, case: &str) -> DeviceMetricsGrant {
        DeviceMetricsGrant::add_to_manifest(&self.manifest, case)
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.rootfs),
            path_text(&self.data),
            path_text(&self.replacement),
            path_text(&self.hotplug_reuse),
            path_text(&self.storage_block_one),
            path_text(&self.storage_block_two),
            path_text(&self.pmem),
            path_text(&self.pmem_reuse),
            path_text(&self.storage_pmem),
            path_text(&self.read_only_data),
            path_text(&self.opened_rootfs),
            path_text(&self.opened_data),
            path_text(&self.opened_replacement),
            path_text(&self.opened_hotplug_reuse),
            path_text(&self.opened_storage_block_one),
            path_text(&self.opened_storage_block_two),
            path_text(&self.opened_pmem),
            path_text(&self.opened_pmem_reuse),
            path_text(&self.opened_storage_pmem),
            path_text(&self.opened_read_only_data),
            path_text(&self.manifest),
            GUEST_ROOTFS_ID,
            GUEST_DATA_ID,
            GUEST_REPLACEMENT_ID,
            GUEST_HOTPLUG_REUSE_ID,
            GUEST_STORAGE_BLOCK_ONE_ID,
            GUEST_STORAGE_BLOCK_TWO_ID,
            GUEST_PMEM_ID,
            GUEST_PMEM_REUSE_ID,
            GUEST_STORAGE_PMEM_ID,
            GUEST_PMEM_ROOT_ID,
            GUEST_READ_ONLY_DATA_ID,
            GUEST_ROOTFS_REF,
            GUEST_DATA_REF,
            GUEST_REPLACEMENT_REF,
            GUEST_HOTPLUG_REUSE_REF,
            GUEST_STORAGE_BLOCK_ONE_REF,
            GUEST_STORAGE_BLOCK_TWO_REF,
            GUEST_PMEM_REF,
            GUEST_PMEM_REUSE_REF,
            GUEST_STORAGE_PMEM_REF,
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
        Self::assert_outputs_at(
            &self.opened_logger,
            &self.opened_metrics,
            &self.opened_serial,
            true,
            true,
            true,
        );
    }

    fn assert_original_outputs_with_logger_expectations(&self, api: bool, action: bool) {
        Self::assert_outputs_at(
            &self.opened_logger,
            &self.opened_metrics,
            &self.opened_serial,
            api,
            action,
            false,
        );
    }

    fn assert_current_outputs(&self) {
        Self::assert_outputs_at(&self.logger, &self.metrics, &self.serial, false, true, true);
        let logger = fs::read(&self.logger).expect("startup logger output should read");
        assert!(
            logger
                .windows(b"operation=process-startup outcome=running\n".len())
                .any(|window| window == b"operation=process-startup outcome=running\n"),
            "startup output grant should receive the no-API process startup record"
        );
        for (name, expected) in [
            (
                "transport MMIO registration",
                b"operation=mmio-registration outcome=succeeded\n".as_slice(),
            ),
            (
                "backend guest shutdown",
                b"operation=vcpu-exit outcome=guest-shutdown\n".as_slice(),
            ),
        ] {
            assert!(
                logger
                    .windows(expected.len())
                    .any(|window| window == expected),
                "startup output grant should receive {name} record"
            );
        }
    }

    fn assert_outputs_at(
        logger_path: &Path,
        metrics_path: &Path,
        serial_path: &Path,
        api: bool,
        action: bool,
        terminal: bool,
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
        let has_terminal = logger
            .windows(b"event=process-exit category=success\n".len())
            .any(|window| window == b"event=process-exit category=success\n");
        assert_eq!(
            has_terminal, terminal,
            "terminal logger output should follow the configured module filter"
        );
        if terminal {
            let logger = std::str::from_utf8(&logger)
                .expect("granted logger output should remain valid UTF-8");
            let mut previous = 0usize;
            for expected in [
                "operation=boot-worker outcome=exited\n",
                "operation=guest-power outcome=poweroff\n",
                "operation=vm-stop outcome=succeeded\n",
                "operation=shutdown outcome=orderly\n",
                "event=process-exit category=success\n",
            ] {
                let offset = logger[previous..]
                    .find(expected)
                    .map(|offset| previous + offset)
                    .unwrap_or_else(|| {
                        panic!(
                            "terminal production logger should contain {expected:?} in order: {logger}"
                        )
                    });
                previous = offset + expected.len();
            }
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
        let metrics = lines
            .iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .expect("each appended metrics line should be valid JSON")
            })
            .collect::<Vec<_>>();
        assert!(metrics.iter().all(|line| {
            line["utc_timestamp_ms"].as_u64().is_some()
                && line["vmm"]["panic_count"].as_u64() == Some(0)
                && line["vmm"].get("metrics_flush_count").is_none()
                && line["vmm"].get("boot_run_loop_status").is_none()
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

fn wait_for_canonical_output_metrics_lines(
    path: &Path,
    expected: usize,
    timeout: Duration,
    context: &str,
) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = fs::read_to_string(path).unwrap_or_else(|error| {
            panic!(
                "{context} metrics output {} should be readable: {error}",
                path.display()
            )
        });
        let seed = std::str::from_utf8(OUTPUT_METRICS_SEED)
            .expect("metrics output seed should be valid UTF-8");
        let payload = output
            .strip_prefix(seed)
            .unwrap_or_else(|| panic!("{context} metrics output lost its original seed: {output}"));
        if payload.is_empty() || payload.ends_with('\n') {
            let lines = payload
                .lines()
                .map(|line| {
                    serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                        panic!(
                            "{context} metrics line should be valid JSON: {error}; line:\n{line}"
                        )
                    })
                })
                .collect::<Vec<_>>();
            assert!(
                lines.len() <= expected,
                "{context} emitted more than the expected {expected} metrics lines: {output}"
            );
            if lines.len() == expected {
                for line in &lines {
                    assert_canonical_metrics_tree(line, "metrics");
                    assert_architecture_retained_platform_zero_metrics(line, context);
                    assert!(line["utc_timestamp_ms"].as_u64().is_some());
                    assert_eq!(line["vmm"]["panic_count"].as_u64(), Some(0));
                    assert!(line["vmm"].get("metrics_flush_count").is_none());
                    assert!(line["vmm"].get("boot_run_loop_status").is_none());
                }
                return lines;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {expected} {context} metrics lines; output:\n{output}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn assert_architecture_retained_platform_zero_metrics(value: &serde_json::Value, context: &str) {
    let i8042 = value
        .get("i8042")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("{context} metrics should contain an i8042 object"));
    assert_eq!(
        i8042.len(),
        6,
        "{context} metrics i8042 shape should be exact"
    );
    for field in [
        "error_count",
        "missed_read_count",
        "missed_write_count",
        "read_count",
        "reset_count",
        "write_count",
    ] {
        assert_eq!(
            i8042.get(field).and_then(serde_json::Value::as_u64),
            Some(0),
            "{context} metrics must retain literal zero i8042.{field}"
        );
    }

    let vcpu = value
        .get("vcpu")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("{context} metrics should contain a vcpu object"));
    for field in ["exit_io_in", "exit_io_out", "kvmclock_ctrl_fails"] {
        assert_eq!(
            vcpu.get(field).and_then(serde_json::Value::as_u64),
            Some(0),
            "{context} metrics must retain literal zero vcpu.{field}"
        );
    }
    for aggregate_name in ["exit_io_in_agg", "exit_io_out_agg"] {
        let aggregate = vcpu
            .get(aggregate_name)
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("{context} metrics should contain vcpu.{aggregate_name}"));
        assert_eq!(
            aggregate.len(),
            3,
            "{context} metrics {aggregate_name} shape should be exact"
        );
        for field in ["min_us", "max_us", "sum_us"] {
            assert_eq!(
                aggregate.get(field).and_then(serde_json::Value::as_u64),
                Some(0),
                "{context} metrics must retain literal zero vcpu.{aggregate_name}.{field}"
            );
        }
    }
}

fn assert_canonical_metrics_tree(value: &serde_json::Value, path: &str) {
    match value {
        serde_json::Value::Object(fields) => {
            for (field, value) in fields {
                assert_canonical_metrics_tree(value, &format!("{path}.{field}"));
            }
        }
        serde_json::Value::Number(number) => assert!(
            number.as_u64().is_some(),
            "canonical metrics leaf {path} must be a nonnegative integer: {number}"
        ),
        other => panic!("canonical metrics node {path} has an invalid value: {other}"),
    }
}

fn assert_real_periodic_metrics_spacing(
    lines: &[serde_json::Value],
    previous: usize,
    periodic: usize,
    state: &str,
) {
    let previous = lines[previous]["utc_timestamp_ms"]
        .as_u64()
        .expect("previous metrics timestamp should be an integer");
    let periodic = lines[periodic]["utc_timestamp_ms"]
        .as_u64()
        .expect("periodic metrics timestamp should be an integer");
    let spacing = periodic
        .checked_sub(previous)
        .expect("periodic metrics timestamp should advance");
    assert!(
        (55_000..=95_000).contains(&spacing),
        "real {state} periodic metrics spacing should certify the 60-second scheduler, found {spacing} ms"
    );
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
                serde_json::json!({
                    "log_path": OUTPUT_LOGGER_REF,
                    "level": "Debug",
                }),
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
struct PagerGrantFixture {
    _root: TestDir,
    socket: PathBuf,
    manifest: PathBuf,
}

impl PagerGrantFixture {
    fn new(case: &str) -> Self {
        let socket_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let root = TestDir(
            PathBuf::from("/private/tmp")
                .join(format!("bbp-{}-{socket_id}-{case}", std::process::id())),
        );
        fs::create_dir(root.path()).expect("short pager root should create");
        let canonical_root = fs::canonicalize(root.path()).expect("pager root should canonicalize");
        let socket = canonical_root.join("snapshot-pager.sock");
        let manifest = canonical_root.join("pager-grant-manifest.json");
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": [{
                "id": PAGER_GRANT_ID,
                "role": "snapshot-pager-stream",
                "access": "read-write",
                "source": path_text(&socket),
            }],
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("pager manifest should serialize"),
        )
        .expect("pager manifest should write");
        Self {
            _root: root,
            socket,
            manifest,
        }
    }

    fn start_peer(&self, mode: &str) -> PagerPeerProcess {
        assert!(
            !self.socket.exists(),
            "pager socket path must be absent before peer bind"
        );
        PagerPeerProcess::start(&self.socket, mode, self.sensitive_strings())
    }

    fn clear_socket(&self) {
        if self.socket.exists() {
            fs::remove_file(&self.socket).expect("pager socket path should remove");
        }
    }

    fn install_wrong_descriptor(&self) {
        assert!(!self.socket.exists());
        fs::write(&self.socket, b"not a socket\n").expect("wrong descriptor fixture should write");
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(&self.socket),
            path_text(&self.manifest),
            PAGER_GRANT_ID,
            PAGER_GRANT_REF,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#[derive(Debug)]
struct PagerPeerProcess {
    child: Child,
    stdout_reader: Option<JoinHandle<String>>,
    stderr_reader: Option<JoinHandle<String>>,
    sensitive: Vec<String>,
    completed: bool,
}

impl PagerPeerProcess {
    fn start(path: &Path, mode: &str, sensitive: Vec<String>) -> Self {
        let mut child =
            Command::new(std::env::current_exe().expect("test executable should exist"))
                .args(["--exact", "pager_reference_peer_child", "--nocapture"])
                .env(PAGER_PEER_PATH_ENV, path)
                .env(PAGER_PEER_MODE_ENV, mode)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("pager reference peer should start");
        let (ready, stdout_reader) = read_stdout_until_line(&mut child, PAGER_PEER_LISTENING);
        let stderr_reader = read_stream(child.stderr.take().expect("peer stderr should be piped"));
        if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
            kill_child_group(&mut child);
            let _ = child.wait();
            let stdout = stdout_reader.join().expect("peer stdout should join");
            let stderr = stderr_reader.join().expect("peer stderr should join");
            panic!(
                "pager reference peer should listen: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
        Self {
            child,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            sensitive,
            completed: false,
        }
    }

    fn wait(&mut self, context: &str) -> ExitStatus {
        let status = if wait_for_child_exit(&self.child, PROCESS_TIMEOUT) {
            self.child.wait().expect("pager peer wait should succeed")
        } else {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
            panic!("timed out waiting for {context}");
        };
        self.completed = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("peer stdout reader should exist")
            .join()
            .expect("peer stdout reader should join");
        let stderr = self
            .stderr_reader
            .take()
            .expect("peer stderr reader should exist")
            .join()
            .expect("peer stderr reader should join");
        let combined = format!("{stdout}{stderr}");
        for sensitive in &self.sensitive {
            assert!(
                !combined.contains(sensitive),
                "pager peer diagnostics must be redacted"
            );
        }
        status
    }

    fn wait_success(&mut self, context: &str) {
        let status = self.wait(context);
        assert!(status.success(), "{context} should succeed: {status:?}");
    }

    fn kill(&mut self, signal: i32, context: &str) -> ExitStatus {
        let pid = i32::try_from(self.child.id()).expect("peer PID should fit");
        // SAFETY: The unreaped child owns this live PID and the signal is fixed by the test.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
        self.wait(context)
    }
}

impl Drop for PagerPeerProcess {
    fn drop(&mut self) {
        if !self.completed {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

#[derive(Debug)]
struct HoldingPagerLauncher {
    child: Child,
    stdout_reader: Option<JoinHandle<String>>,
    stderr_reader: Option<JoinHandle<String>>,
    sensitive: Vec<String>,
    completed: bool,
}

impl HoldingPagerLauncher {
    fn start(bundle: &Path, fixture: &PagerGrantFixture) -> Self {
        let mut command = pager_probe_command(bundle, fixture, "pager-wait");
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("holding pager launcher should start");
        let (ready, stdout_reader) = read_stdout_until_line(&mut child, PAGER_PROBE_READY);
        let stderr_reader = read_stream(child.stderr.take().expect("pager stderr should be piped"));
        if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
            kill_child_group(&mut child);
            let _ = child.wait();
            let stdout = stdout_reader.join().expect("pager stdout should join");
            let stderr = stderr_reader.join().expect("pager stderr should join");
            panic!(
                "pager probe should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
        Self {
            child,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            sensitive: fixture.sensitive_strings(),
            completed: false,
        }
    }

    fn wait(&mut self, context: &str) -> ExitStatus {
        let status = if wait_for_child_exit(&self.child, PROCESS_TIMEOUT) {
            self.child
                .wait()
                .expect("pager launcher wait should succeed")
        } else {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
            panic!("timed out waiting for {context}");
        };
        self.completed = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("pager stdout reader should exist")
            .join()
            .expect("pager stdout should join");
        let stderr = self
            .stderr_reader
            .take()
            .expect("pager stderr reader should exist")
            .join()
            .expect("pager stderr should join");
        let combined = format!("{stdout}{stderr}");
        for sensitive in &self.sensitive {
            assert!(
                !combined.contains(sensitive),
                "pager launcher diagnostics must be redacted"
            );
        }
        status
    }
}

impl Drop for HoldingPagerLauncher {
    fn drop(&mut self) {
        if !self.completed {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

fn pager_probe_command(bundle: &Path, fixture: &PagerGrantFixture, case: &str) -> Command {
    let mut command = Command::new(launcher(bundle));
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .arg(GRANT_PROBE_OPTION)
        .arg(case);
    command
}

fn run_pager_probe(bundle: &Path, fixture: &PagerGrantFixture, case: &str) -> Output {
    run_with_timeout(
        &mut pager_probe_command(bundle, fixture, case),
        PROCESS_TIMEOUT,
        "signed pager grant probe",
    )
}

fn assert_pager_output_redacted(output: &Output, fixture: &PagerGrantFixture) {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for sensitive in fixture.sensitive_strings() {
        assert!(
            !combined.contains(&sensitive),
            "pager diagnostics must redact configured authority"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreGrantVariant {
    Exact,
    ExtraUnrelated,
    MissingRoot,
    MissingDirectory,
    WrongRootRole,
    WrongRootAccess,
    WrongRootKind,
    WrongDirectoryRole,
    WrongDirectoryAccess,
    WrongDirectoryKind,
    SubstitutedIds,
}

#[derive(Debug)]
struct RestoreTransactionGrantFixture {
    _root: TestDir,
    root: PathBuf,
    directory: PathBuf,
    manifest: PathBuf,
    extra: PathBuf,
}

impl RestoreTransactionGrantFixture {
    fn new(case: &str, variant: RestoreGrantVariant) -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let root = TestDir(
            PathBuf::from("/private/tmp").join(format!("bbr-{}-{id}-{case}", std::process::id())),
        );
        fs::create_dir(root.path()).expect("short restore root should create");
        let root_file = root.path().join("r.img");
        let directory = root.path().join("d");
        let manifest = root.path().join("m.json");
        let extra = root.path().join("x.img");
        fs::write(&root_file, RESTORE_ROOT_MARKER).expect("restore root marker should write");
        fs::create_dir(&directory).expect("restore socket directory should create");
        fs::write(&extra, b"unrelated-authority\n").expect("extra restore fixture should write");

        let root_id = if variant == RestoreGrantVariant::SubstitutedIds {
            "restore-substituted-root-1601"
        } else {
            RESTORE_ROOT_ID
        };
        let directory_id = if variant == RestoreGrantVariant::SubstitutedIds {
            "restore-substituted-vsock-1601"
        } else {
            RESTORE_VSOCK_ID
        };
        let root_role = if variant == RestoreGrantVariant::WrongRootRole {
            "kernel-image"
        } else {
            "drive-backing"
        };
        let root_access = if variant == RestoreGrantVariant::WrongRootAccess {
            "read-write"
        } else {
            "read-only"
        };
        let root_source = if variant == RestoreGrantVariant::WrongRootKind {
            &directory
        } else {
            &root_file
        };
        let directory_role = if variant == RestoreGrantVariant::WrongDirectoryRole {
            "api-socket-directory"
        } else {
            "vsock-socket-directory"
        };
        let directory_access = if variant == RestoreGrantVariant::WrongDirectoryAccess {
            "connect-children"
        } else {
            "create-children"
        };
        let directory_source = if variant == RestoreGrantVariant::WrongDirectoryKind {
            &root_file
        } else {
            &directory
        };

        let mut grants = Vec::new();
        if variant != RestoreGrantVariant::MissingRoot {
            grants.push(serde_json::json!({
                "id": root_id,
                "role": root_role,
                "access": root_access,
                "source": path_text(root_source),
            }));
        }
        if variant != RestoreGrantVariant::MissingDirectory {
            grants.push(serde_json::json!({
                "id": directory_id,
                "role": directory_role,
                "access": directory_access,
                "source": path_text(directory_source),
            }));
        }
        if variant == RestoreGrantVariant::ExtraUnrelated {
            grants.push(serde_json::json!({
                "id": "restore-unrelated-1601",
                "role": "kernel-image",
                "access": "read-only",
                "source": path_text(&extra),
            }));
        }
        let manifest_json = serde_json::json!({
            "version": 1,
            "grants": grants,
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_json).expect("restore manifest should serialize"),
        )
        .expect("restore manifest should write");

        Self {
            _root: root,
            root: root_file,
            directory,
            manifest,
            extra,
        }
    }

    fn socket(&self) -> PathBuf {
        self.directory.join(RESTORE_SOCKET_CHILD)
    }

    fn replace_root_source(&self) -> PathBuf {
        let retained = self._root.path().join("retained-root.img");
        fs::rename(&self.root, &retained).expect("opened restore root should rename");
        fs::write(&self.root, RESTORE_REPLACEMENT_MARKER)
            .expect("restore root replacement should write");
        retained
    }

    fn assert_pristine(&self) {
        assert_eq!(
            fs::read(&self.root).expect("restore root should remain readable"),
            RESTORE_ROOT_MARKER
        );
        assert_eq!(
            fs::read(&self.extra).expect("unrelated authority should remain readable"),
            b"unrelated-authority\n"
        );
        assert!(!self.socket().exists());
    }

    fn assert_root_replacement_preserved(&self, retained: &Path) {
        assert_eq!(
            fs::read(retained).expect("retained opened root should remain readable"),
            RESTORE_ROOT_MARKER
        );
        assert_eq!(
            fs::read(&self.root).expect("planted root should remain readable"),
            RESTORE_REPLACEMENT_MARKER
        );
        assert!(!self.socket().exists());
    }

    fn sensitive_strings(&self) -> Vec<String> {
        [
            path_text(self._root.path()),
            path_text(&self.root),
            path_text(&self.directory),
            path_text(&self.manifest),
            path_text(&self.extra),
            RESTORE_ROOT_ID,
            RESTORE_VSOCK_ID,
            RESTORE_ROOT_REF,
            RESTORE_VSOCK_REF,
            RESTORE_SOCKET_CHILD,
            "restore-substituted-root-1601",
            "restore-substituted-vsock-1601",
            "restore-unrelated-1601",
            "retained-root.img",
            "retained-owned.sock",
            "unrelated-authority",
            std::str::from_utf8(RESTORE_ROOT_MARKER)
                .expect("restore root marker should be UTF-8")
                .trim_end(),
            std::str::from_utf8(RESTORE_ROOT_MARKER).expect("restore root marker should be UTF-8"),
            std::str::from_utf8(RESTORE_REPLACEMENT_MARKER)
                .expect("restore replacement marker should be UTF-8")
                .trim_end(),
            std::str::from_utf8(RESTORE_REPLACEMENT_MARKER)
                .expect("restore replacement marker should be UTF-8"),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

fn is_socket_path(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

fn restore_probe_command(
    bundle: &Path,
    fixture: &RestoreTransactionGrantFixture,
    case: &str,
) -> Command {
    let mut command = Command::new(launcher(bundle));
    command
        .arg(GRANT_MANIFEST_OPTION)
        .arg(&fixture.manifest)
        .arg("--")
        .arg(GRANT_PROBE_OPTION)
        .arg(case);
    command
}

fn run_restore_probe(
    bundle: &Path,
    fixture: &RestoreTransactionGrantFixture,
    case: &str,
) -> Output {
    run_with_timeout(
        &mut restore_probe_command(bundle, fixture, case),
        PROCESS_TIMEOUT,
        "signed restore transaction probe",
    )
}

fn assert_restore_output_redacted(output: &Output, fixture: &RestoreTransactionGrantFixture) {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for sensitive in fixture.sensitive_strings() {
        assert!(
            !combined.contains(&sensitive),
            "restore transaction diagnostics must redact configured authority"
        );
    }
}

#[derive(Debug)]
struct HoldingRestoreProbe {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_reader: Option<JoinHandle<String>>,
    stderr_reader: Option<JoinHandle<String>>,
    sensitive: Vec<String>,
    completed: bool,
}

impl HoldingRestoreProbe {
    fn release_stdin(&mut self) {
        let mut stdin = self
            .stdin
            .take()
            .expect("replacement restore probe should retain stdin");
        stdin
            .write_all(b"x")
            .expect("replacement restore probe should release");
    }

    fn wait(&mut self, context: &str) -> ExitStatus {
        let status = if wait_for_child_exit(&self.child, PROCESS_TIMEOUT) {
            self.child
                .wait()
                .expect("restore launcher wait should succeed")
        } else {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
            panic!("timed out waiting for {context}");
        };
        self.completed = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("restore stdout reader should exist")
            .join()
            .expect("restore stdout should join");
        let stderr = self
            .stderr_reader
            .take()
            .expect("restore stderr reader should exist")
            .join()
            .expect("restore stderr should join");
        let combined = format!("{stdout}{stderr}");
        for sensitive in &self.sensitive {
            assert!(
                !combined.contains(sensitive),
                "holding restore diagnostics must remain redacted"
            );
        }
        status
    }

    fn stop(&mut self, signal: i32, context: &str) {
        let pid = i32::try_from(self.child.id()).expect("restore launcher PID should fit");
        // SAFETY: The unreaped launcher owns this PID and signal is fixed by the test.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
        let status = self.wait(context);
        assert!(status.success(), "{context} should stop successfully");
    }
}

impl Drop for HoldingRestoreProbe {
    fn drop(&mut self) {
        if !self.completed {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

fn spawn_holding_restore_probe(
    bundle: &Path,
    fixture: &RestoreTransactionGrantFixture,
    case: &str,
    ready_line: &'static str,
    pipe_stdin: bool,
) -> HoldingRestoreProbe {
    let mut command = restore_probe_command(bundle, fixture, case);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if pipe_stdin {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().expect("holding restore probe should start");
    let stdin = child.stdin.take();
    let (ready, stdout_reader) = read_stdout_until_line(&mut child, ready_line);
    let stderr_reader = read_stream(child.stderr.take().expect("restore stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("restore stdout should join");
        let stderr = stderr_reader.join().expect("restore stderr should join");
        panic!("restore probe should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
    HoldingRestoreProbe {
        child,
        stdin,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive: fixture.sensitive_strings(),
        completed: false,
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

#[derive(Debug)]
struct RunningSerialApiLauncher {
    child: Child,
    stdin: Option<ChildStdin>,
    socket: PathBuf,
    stdout_reader: Option<JoinHandle<String>>,
    stdout: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<String>>,
    completed: bool,
}

impl RunningSerialApiLauncher {
    fn write_stdin(&mut self, bytes: &[u8]) {
        self.stdin
            .as_mut()
            .expect("launcher stdin should remain open")
            .write_all(bytes)
            .expect("launcher stdin should accept serial bytes");
    }

    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    fn stdout_snapshot(&self) -> String {
        self.stdout
            .lock()
            .expect("launcher stdout snapshot should lock")
            .clone()
    }

    fn wait_for_stdout_marker(&self, marker: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let output = self.stdout_snapshot();
            if output.contains(marker) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "stdout did not contain {marker:?} before {timeout:?}; stdout:\n{output}"
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait(&mut self, context: &str) -> (ExitStatus, String, String) {
        let status = if wait_for_child_exit(&self.child, PROCESS_TIMEOUT) {
            self.child
                .wait()
                .expect("serial launcher wait should succeed")
        } else {
            kill_child_group(&mut self.child);
            let _ = self.child.wait();
            panic!("timed out waiting for {context}");
        };
        self.completed = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("serial stdout reader should exist")
            .join()
            .expect("serial stdout reader should join");
        let stderr = self
            .stderr_reader
            .take()
            .expect("serial stderr reader should exist")
            .join()
            .expect("serial stderr reader should join");
        (status, stdout, stderr)
    }
}

impl Drop for RunningSerialApiLauncher {
    fn drop(&mut self) {
        if !self.completed {
            let pid = i32::try_from(self.child.id()).expect("serial launcher PID should fit");
            // SAFETY: The unreaped test child owns this PID.
            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
            if !wait_for_child_exit(&self.child, DROP_CLEANUP_TIMEOUT) {
                kill_child_group(&mut self.child);
            }
            let _ = self.child.wait();
            if let Some(reader) = self.stdout_reader.take() {
                let _ = reader.join();
            }
            if let Some(reader) = self.stderr_reader.take() {
                let _ = reader.join();
            }
        }
    }
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

fn spawn_ready_serial_api_launcher(bundle: &Path, name: &str) -> RunningSerialApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket = container_tmp_dir().join(format!("bbs-{:x}-{test_id:x}.sock", std::process::id()));
    let mut child = Command::new(launcher(bundle))
        .args(["--api-sock", path_text(&socket), "--id"])
        .arg(format!("{name}-{}", std::process::id()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("production serial launcher should start");
    let stdin = child.stdin.take().expect("launcher stdin should be piped");
    let (ready, stdout_reader, stdout) =
        read_stdout_until_line_shared(&mut child, "status: API server listening");
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "serial launcher should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningSerialApiLauncher {
        child,
        stdin: Some(stdin),
        socket,
        stdout_reader: Some(stdout_reader),
        stdout,
        stderr_reader: Some(stderr_reader),
        completed: false,
    }
}

fn spawn_ready_serial_snapshot_grant_api_launcher(
    bundle: &Path,
    manifest: &Path,
    name: &str,
    enable_pci: bool,
) -> RunningSerialApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket =
        container_tmp_dir().join(format!("bbss-{:x}-{test_id:x}.sock", std::process::id()));
    let mut command = Command::new(launcher(bundle));
    command.arg(GRANT_MANIFEST_OPTION).arg(manifest).arg("--");
    if enable_pci {
        command.arg("--enable-pci");
    }
    let mut child = command
        .args(["--api-sock", path_text(&socket)])
        .args(["--id", &format!("{name}-{test_id}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("serial snapshot grant launcher should start");
    let stdin = child
        .stdin
        .take()
        .expect("serial snapshot launcher stdin should be piped");
    let (ready, stdout_reader, stdout) =
        read_stdout_until_line_shared(&mut child, "status: API server listening");
    let stderr_reader = read_stream(
        child
            .stderr
            .take()
            .expect("serial snapshot launcher stderr should be piped"),
    );
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "serial snapshot launcher should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningSerialApiLauncher {
        child,
        stdin: Some(stdin),
        socket,
        stdout_reader: Some(stdout_reader),
        stdout,
        stderr_reader: Some(stderr_reader),
        completed: false,
    }
}

fn spawn_ready_serial_snapshot_grant_api_launcher_with_granted_socket(
    bundle: &Path,
    manifest: &Path,
    socket: &Path,
    name: &str,
    enable_pci: bool,
) -> RunningSerialApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let mut command = Command::new(launcher(bundle));
    command.arg(GRANT_MANIFEST_OPTION).arg(manifest).arg("--");
    if enable_pci {
        command.arg("--enable-pci");
    }
    let mut child = command
        .args(["--api-sock", API_SOCKET_REF])
        .args(["--id", &format!("{name}-{test_id}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("granted serial snapshot launcher should start");
    let stdin = child
        .stdin
        .take()
        .expect("granted serial snapshot launcher stdin should be piped");
    let (ready, stdout_reader, stdout) =
        read_stdout_until_line_shared(&mut child, "status: API server listening");
    let stderr_reader = read_stream(
        child
            .stderr
            .take()
            .expect("granted serial snapshot launcher stderr should be piped"),
    );
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "granted serial snapshot launcher should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningSerialApiLauncher {
        child,
        stdin: Some(stdin),
        socket: socket.to_path_buf(),
        stdout_reader: Some(stdout_reader),
        stdout,
        stderr_reader: Some(stderr_reader),
        completed: false,
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

fn spawn_ready_block_special_grant_api_launcher(
    bundle: &Path,
    fixture: &BlockSpecialGrantFixture,
    name: &str,
    worker_args: &[&str],
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket = container_tmp_dir().join(format!("bbb-{:x}-{test_id:x}.sock", std::process::id()));
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
        .expect("block-special grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "block-special grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
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
    enable_pci: bool,
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
    if enable_pci {
        command.arg("--enable-pci");
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

fn spawn_ready_snapshot_epoch_grant_api_launcher(
    bundle: &Path,
    manifest: &Path,
    socket: &Path,
    sensitive: Vec<String>,
    name: &str,
    enable_pci: bool,
) -> RunningApiLauncher {
    initialize_worker_container(bundle);
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let mut command = Command::new(launcher(bundle));
    command.arg(GRANT_MANIFEST_OPTION).arg(manifest).arg("--");
    if enable_pci {
        command.arg("--enable-pci");
    }
    let mut child = command
        .args(["--api-sock", API_SOCKET_REF])
        .args(["--id", &format!("{name}-{test_id}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("snapshot epoch grant launcher should start");
    let (ready, stdout_reader) = read_stdout_until_ready(&mut child);
    let stderr_reader = read_stream(child.stderr.take().expect("stderr should be piped"));
    if let Err(error) = ready.recv_timeout(PROCESS_TIMEOUT) {
        kill_child_group(&mut child);
        let _ = child.wait();
        let stdout = stdout_reader.join().expect("stdout reader should join");
        let stderr = stderr_reader.join().expect("stderr reader should join");
        panic!(
            "snapshot epoch grant API should become ready: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    RunningApiLauncher {
        child,
        socket: socket.to_path_buf(),
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        sensitive,
        completed: false,
    }
}

fn configure_and_pause_snapshot_source(running: &RunningApiLauncher, metrics_path: &Path) {
    configure_and_pause_snapshot_source_with_tracking(running, metrics_path, false);
}

fn configure_and_pause_snapshot_source_with_tracking(
    running: &RunningApiLauncher,
    metrics_path: &Path,
    track_dirty_pages: bool,
) {
    let machine_config = if track_dirty_pages {
        serde_json::json!({
            "vcpu_count": 1,
            "mem_size_mib": 256,
            "track_dirty_pages": true,
        })
    } else {
        serde_json::json!({
            "vcpu_count": 1,
            "mem_size_mib": 256,
        })
    };
    for (path, body, context) in [
        (
            "/machine-config",
            machine_config,
            "PUT snapshot machine config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "PUT snapshot metrics",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "boot_args": SNAPSHOT_ROOT_BOOT_ARGS,
            }),
            "PUT snapshot boot source",
        ),
        (
            "/drives/rootfs",
            serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": SNAPSHOT_ROOT_REF,
                "is_root_device": true,
                "is_read_only": false,
                "cache_type": "Unsafe",
                "io_engine": "Async",
            }),
            "PUT snapshot writable Async Unsafe rootfs",
        ),
        (
            "/drives/data",
            serde_json::json!({
                "drive_id": "data",
                "path_on_host": SNAPSHOT_DATA_REF,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Sync",
            }),
            "PUT snapshot writable Sync Writeback data",
        ),
        (
            "/drives/audit",
            serde_json::json!({
                "drive_id": "audit",
                "path_on_host": SNAPSHOT_AUDIT_REF,
                "is_root_device": false,
                "is_read_only": true,
                "cache_type": "Unsafe",
                "io_engine": "Async",
            }),
            "PUT snapshot read-only Async Unsafe audit",
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
    wait_for_snapshot_root_read(&running.socket, metrics_path, PROCESS_TIMEOUT);
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause snapshot source",
    );
}

fn configure_and_start_serial_snapshot_grant_source(
    socket: &Path,
    with_storage: bool,
    configured_output: bool,
) {
    for (path, body, context) in [
        (
            "/machine-config",
            serde_json::json!({
                "vcpu_count": 1,
                "mem_size_mib": 16,
                "track_dirty_pages": true,
            }),
            "PUT production serial snapshot machine config",
        ),
        (
            "/boot-source",
            serde_json::json!({"kernel_image_path": SNAPSHOT_KERNEL_REF}),
            "PUT production serial snapshot boot source",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "PUT production serial snapshot metrics",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production serial snapshot request should serialize"),
            ),
            204,
            context,
        );
    }
    if with_storage {
        assert_http_status(
            &http_put(
                socket,
                "/drives/serial_data",
                &serde_json::json!({
                    "drive_id": "serial_data",
                    "path_on_host": SNAPSHOT_DATA_REF,
                    "is_root_device": false,
                    "is_read_only": false,
                    "io_engine": "Sync",
                })
                .to_string(),
            ),
            204,
            "PUT production serial snapshot storage",
        );
    }
    if configured_output {
        assert_http_status(
            &http_put(
                socket,
                "/serial",
                &serde_json::json!({
                    "serial_out_path": SNAPSHOT_SERIAL_SINK_REF,
                    "rate_limiter": {
                        "size": snapshot_serial::CONFIGURED_RATE_LIMITER_SIZE,
                        "refill_time":
                            snapshot_serial::CONFIGURED_RATE_LIMITER_REFILL_TIME_MS,
                    },
                })
                .to_string(),
            ),
            204,
            "PUT production serial snapshot output",
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        "start production serial snapshot source",
    );
}

fn configure_serial_snapshot_grant_destination_metrics(socket: &Path) {
    assert_http_status(
        &http_put(
            socket,
            "/metrics",
            &serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}).to_string(),
        ),
        204,
        "PUT production serial snapshot destination metrics",
    );
}

fn configure_and_start_entropy_snapshot_grant_source(
    socket: &Path,
    with_storage: bool,
    context: &str,
) {
    for (path, body, request) in [
        (
            "/machine-config",
            serde_json::json!({
                "vcpu_count": 1,
                "mem_size_mib": 256,
            }),
            "machine config",
        ),
        (
            "/entropy",
            serde_json::json!({
                "rate_limiter": {
                    "bandwidth": {
                        "size": SNAPSHOT_ENTROPY_READ_BYTES,
                        "refill_time": SNAPSHOT_ENTROPY_REFILL_MS,
                    },
                    "ops": {
                        "size": 1,
                        "refill_time": SNAPSHOT_ENTROPY_REFILL_MS,
                    },
                },
            }),
            "entropy config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "metrics",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "initrd_path": SNAPSHOT_INITRD_REF,
                "boot_args": SNAPSHOT_ENTROPY_BOOT_ARGS,
            }),
            "boot source",
        ),
    ] {
        assert_http_status(
            &http_put(
                socket,
                path,
                &serde_json::to_string(&body)
                    .expect("production entropy snapshot request should serialize"),
            ),
            204,
            &format!("PUT production {context} entropy snapshot {request}"),
        );
    }
    if with_storage {
        assert_http_status(
            &http_put(
                socket,
                "/drives/data",
                &serde_json::json!({
                    "drive_id": "data",
                    "path_on_host": SNAPSHOT_DATA_REF,
                    "is_root_device": false,
                    "is_read_only": false,
                    "cache_type": "Writeback",
                    "io_engine": "Sync",
                })
                .to_string(),
            ),
            204,
            &format!("PUT production {context} entropy snapshot storage"),
        );
    }
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"InstanceStart"}"#),
        204,
        &format!("start production {context} entropy snapshot source"),
    );
}

fn production_http_response_json(response: &str) -> serde_json::Value {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("production API response should contain an HTTP body");
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("production API response body should be JSON: {error}; response:\n{response}")
    })
}

fn wait_for_production_balloon_page_counts(
    socket: &Path,
    expected_target_pages: u64,
    expected_actual_pages: u64,
    context: &str,
) {
    let deadline = Instant::now()
        .checked_add(PROCESS_TIMEOUT)
        .expect("production balloon page-count deadline should fit");
    loop {
        let response = http_get(socket, "/balloon/statistics");
        if response.starts_with("HTTP/1.1 200 ") {
            let statistics = production_http_response_json(&response);
            if statistics
                .get("target_pages")
                .and_then(serde_json::Value::as_u64)
                == Some(expected_target_pages)
                && statistics
                    .get("actual_pages")
                    .and_then(serde_json::Value::as_u64)
                    == Some(expected_actual_pages)
            {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{context} should reach target_pages={expected_target_pages}, actual_pages={expected_actual_pages}; last response:\n{response}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_production_balloon_optional_statistics(socket: &Path, context: &str) {
    const OPTIONAL_FIELDS: &[&str] = &[
        "swap_in",
        "swap_out",
        "major_faults",
        "minor_faults",
        "free_memory",
        "total_memory",
        "available_memory",
        "disk_caches",
        "hugetlb_allocations",
        "hugetlb_failures",
        "oom_kill",
        "alloc_stall",
        "async_scan",
        "direct_scan",
        "async_reclaim",
        "direct_reclaim",
    ];

    let deadline = Instant::now()
        .checked_add(PROCESS_TIMEOUT)
        .expect("production balloon optional-statistics deadline should fit");
    loop {
        let response = http_get(socket, "/balloon/statistics");
        if response.starts_with("HTTP/1.1 200 ") {
            let statistics = production_http_response_json(&response);
            if OPTIONAL_FIELDS
                .iter()
                .any(|field| statistics.get(*field).is_some_and(|value| !value.is_null()))
            {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{context} should publish at least one optional guest statistic; last response:\n{response}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_production_balloon_hinting_status(
    socket: &Path,
    expected_host_cmd: u64,
    expected_guest_cmd: Option<u64>,
    context: &str,
) {
    let deadline = Instant::now()
        .checked_add(PROCESS_TIMEOUT)
        .expect("production balloon hinting deadline should fit");
    loop {
        let response = http_get(socket, "/balloon/hinting/status");
        if response.starts_with("HTTP/1.1 200 ") {
            let status = production_http_response_json(&response);
            if status.get("host_cmd").and_then(serde_json::Value::as_u64) == Some(expected_host_cmd)
                && status.get("guest_cmd").and_then(serde_json::Value::as_u64) == expected_guest_cmd
            {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{context} should reach host_cmd={expected_host_cmd}, guest_cmd={expected_guest_cmd:?}; last response:\n{response}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn flush_production_metrics(socket: &Path, context: &str) {
    assert_http_status(
        &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
        204,
        &format!("FlushMetrics for production {context}"),
    );
}

fn wait_for_production_balloon_metric(
    socket: &Path,
    metrics: &Path,
    field: &str,
    expected: u64,
    context: &str,
) {
    let deadline = Instant::now()
        .checked_add(PROCESS_TIMEOUT)
        .expect("production balloon metric deadline should fit");
    loop {
        flush_production_metrics(socket, context);
        let observed = production_balloon_metric_total(metrics, field);
        if observed >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context} should reach balloon.{field} >= {expected}; observed={observed}; metrics:\n{}",
            fs::read_to_string(metrics).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn production_balloon_metric_total(path: &Path, field: &str) -> u64 {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("balloon")?
                .get(field)?
                .as_u64()
        })
        .fold(0, u64::saturating_add)
}

fn assert_production_balloon_snapshot(
    state_path: &Path,
    enable_pci: bool,
    context: &str,
) -> SnapshotV2BalloonState {
    let bytes = fs::read(state_path).unwrap_or_else(|error| {
        panic!(
            "production {context} balloon state {} should read: {error}",
            state_path.display()
        )
    });
    let structural =
        decode_snapshot_v2_state(&bytes).expect("production balloon state should decode");
    let state = decode_hvf_snapshot_v2_vsock_state(&structural)
        .expect("production balloon state should be exact native-v2 2.12");
    let graph = state
        .device_graph()
        .expect("production balloon artifact should retain storage");
    assert_eq!(
        graph.block_records().len(),
        2,
        "production {context} should retain root and data drives"
    );
    assert!(
        state.entropy().is_none(),
        "production {context} should not add entropy state"
    );
    let balloon = state
        .balloon()
        .expect("production certification artifact should contain balloon");
    let transport = if enable_pci {
        SnapshotV2DeviceTransportKind::Pci
    } else {
        SnapshotV2DeviceTransportKind::Mmio
    };
    assert_eq!(graph.transport_kind(), transport);
    assert_eq!(balloon.transport().kind(), transport);
    assert_eq!(balloon.config().amount_mib(), 8);
    assert!(balloon.config().deflate_on_oom());
    assert_eq!(balloon.config().stats_polling_interval_s(), 2);
    assert!(balloon.config().free_page_hinting());
    assert!(balloon.config().free_page_reporting());
    let queues = balloon
        .continuation()
        .active_queues()
        .expect("production balloon queues should be active");
    assert!(queues.statistics().is_some());
    assert!(queues.free_page_hinting().is_some());
    assert!(queues.free_page_reporting().is_some());
    assert!(
        !balloon.continuation().statistics().is_empty(),
        "production {context} should retain latest guest statistics"
    );
    assert!(
        balloon
            .continuation()
            .statistics_pending_descriptor_head()
            .is_some(),
        "production {context} should retain one statistics descriptor"
    );
    assert_eq!(balloon.accounting().inflated_page_count(), 2_048);
    assert!(
        !balloon.accounting().ranges().is_empty(),
        "production {context} should retain canonical nonempty accounting"
    );
    assert_eq!(
        balloon.continuation().hinting().host_cmd(),
        VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
    );
    balloon.clone()
}

fn assert_production_balloon_config(socket: &Path, context: &str) {
    let balloon = http_get(socket, "/balloon");
    assert_http_status(
        &balloon,
        200,
        &format!("read production {context} balloon config"),
    );
    for expected in [
        r#""amount_mib":8"#,
        r#""deflate_on_oom":true"#,
        r#""stats_polling_interval_s":2"#,
        r#""free_page_hinting":true"#,
        r#""free_page_reporting":true"#,
    ] {
        assert!(
            balloon.contains(expected),
            "production {context} balloon config should contain {expected}; response:\n{balloon}"
        );
    }
    let config = http_get(socket, "/vm/config");
    assert_http_status(
        &config,
        200,
        &format!("read production {context} restored VM config"),
    );
    assert_eq!(
        config.matches(r#""drive_id":"#).count(),
        2,
        "production {context} should restore root and data drives"
    );
    assert!(
        config.contains(r#""balloon":"#),
        "production {context} restored VM config should contain balloon"
    );
}

fn assert_production_balloon_statistics_match_snapshot(
    response: &str,
    balloon: &SnapshotV2BalloonState,
    context: &str,
) {
    let actual = production_http_response_json(response);
    for (field, expected) in [
        ("target_pages", 2_048),
        ("target_mib", 8),
        ("actual_pages", balloon.accounting().inflated_page_count()),
        (
            "actual_mib",
            balloon.accounting().inflated_page_count() / 256,
        ),
    ] {
        assert_eq!(
            actual.get(field).and_then(serde_json::Value::as_u64),
            Some(expected),
            "production {context} should restore snapshot-derived {field}"
        );
    }
    for (field, expected) in [
        "swap_in",
        "swap_out",
        "major_faults",
        "minor_faults",
        "free_memory",
        "total_memory",
        "available_memory",
        "disk_caches",
        "hugetlb_allocations",
        "hugetlb_failures",
        "oom_kill",
        "alloc_stall",
        "async_scan",
        "direct_scan",
        "async_reclaim",
        "direct_reclaim",
    ]
    .into_iter()
    .zip(balloon.continuation().statistics().values())
    {
        assert_eq!(
            actual.get(field).and_then(serde_json::Value::as_u64),
            *expected,
            "production {context} should restore latest snapshot statistic {field}"
        );
    }
}

fn wait_for_production_entropy_metric(
    socket: &Path,
    metrics: &Path,
    field: &str,
    expected: u64,
    context: &str,
) {
    let deadline = Instant::now()
        .checked_add(PROCESS_TIMEOUT)
        .expect("production entropy metric deadline should fit");
    loop {
        assert_http_status(
            &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
            204,
            &format!("FlushMetrics while waiting for {context}"),
        );
        let observed = production_entropy_metric_total(metrics, field);
        if observed >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context} should reach entropy.{field} >= {expected}; observed={observed}; metrics:\n{}",
            fs::read_to_string(metrics).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn production_entropy_metric_total(path: &Path, field: &str) -> u64 {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("entropy")?
                .get(field)?
                .as_u64()
        })
        .fold(0, u64::saturating_add)
}

fn assert_production_destination_entropy_metrics(metrics: &Path, context: &str) {
    let output = fs::read_to_string(metrics).unwrap_or_default();
    assert!(
        production_entropy_metric_total(metrics, "entropy_bytes") >= SNAPSHOT_ENTROPY_READ_BYTES,
        "{context} should report restored entropy bytes; metrics:\n{output}"
    );
    assert!(
        production_entropy_metric_total(metrics, "rate_limiter_event_count") >= 1,
        "{context} should report restored retry activity; metrics:\n{output}"
    );
    assert_eq!(
        production_entropy_metric_total(metrics, "host_rng_fails"),
        0,
        "{context} should use a successful fresh OS entropy source"
    );
}

fn assert_production_pending_entropy_snapshot(
    state_path: &Path,
    enable_pci: bool,
    with_storage: bool,
    context: &str,
) {
    let bytes = fs::read(state_path).unwrap_or_else(|error| {
        panic!(
            "production {context} entropy state {} should read: {error}",
            state_path.display()
        )
    });
    let structural =
        decode_snapshot_v2_state(&bytes).expect("production entropy state should decode");
    let state = decode_hvf_snapshot_v2_vsock_state(&structural)
        .expect("production entropy state should be exact native-v2 2.12");
    assert_eq!(
        state.device_graph().is_some(),
        with_storage,
        "production {context} storage presence should remain exact"
    );
    let entropy = state
        .entropy()
        .expect("production certification artifact should contain entropy");
    assert!(
        state.balloon().is_none(),
        "production entropy certification artifact should not add balloon state"
    );
    let transport = if enable_pci {
        SnapshotV2DeviceTransportKind::Pci
    } else {
        SnapshotV2DeviceTransportKind::Mmio
    };
    assert_eq!(entropy.transport().kind(), transport);
    if let Some(graph) = state.device_graph() {
        assert_eq!(graph.transport_kind(), transport);
        assert_eq!(graph.block_records().len(), 1);
    }
    assert_eq!(
        entropy
            .active_queue()
            .expect("production entropy queue should be active")
            .outstanding(),
        1,
        "production {context} should retain one outstanding descriptor"
    );
    assert!(entropy.limiter().bandwidth().is_some());
    assert!(entropy.limiter().ops().is_some());
    assert!(entropy.has_pending_work());
    assert!(entropy.retry().has_retry());
}

fn assert_production_entropy_config(socket: &Path, with_storage: bool, context: &str) {
    let config = http_get(socket, "/vm/config");
    assert_http_status(&config, 200, "read production entropy snapshot config");
    assert!(
        config.contains(r#""entropy":{"rate_limiter":"#),
        "production {context} restored config should contain entropy; response:\n{config}"
    );
    assert_eq!(
        config.matches(r#""drive_id":"#).count(),
        usize::from(with_storage),
        "production {context} restored storage shape should remain exact"
    );
}

fn production_uart_metric_total(path: &Path, field: &str) -> u64 {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("uart")?
                .get(field)?
                .as_u64()
        })
        .fold(0, u64::saturating_add)
}

fn assert_metrics_family_extensions_absent(
    path: &Path,
    family: &str,
    extensions: &[&str],
    context: &str,
) {
    let output = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{context} metrics should be readable: {error}"));
    let mut line_count = 0;
    for line in output.lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("{context} metrics should be JSON: {error}"));
        let object = value[family]
            .as_object()
            .unwrap_or_else(|| panic!("{context} metrics should contain object {family}"));
        for extension in extensions {
            assert!(
                !object.contains_key(*extension),
                "{context} must not publish {family}.{extension}"
            );
        }
        line_count += 1;
    }
    assert!(
        line_count > 0,
        "{context} should publish at least one metrics line"
    );
}

fn assert_metrics_family_fields(
    path: &Path,
    family: &str,
    expected_fields: &[&str],
    context: &str,
) {
    let output = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{context} metrics should be readable: {error}"));
    let mut expected = expected_fields.to_vec();
    expected.sort_unstable();
    let mut line_count = 0;
    for line in output.lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("{context} metrics should be JSON: {error}"));
        let object = value[family]
            .as_object()
            .unwrap_or_else(|| panic!("{context} metrics should contain object {family}"));
        let mut actual = object.keys().map(String::as_str).collect::<Vec<_>>();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "{context} metrics should contain the exact {family} fields"
        );
        line_count += 1;
    }
    assert!(
        line_count > 0,
        "{context} should publish at least one metrics line"
    );
}

fn assert_production_memory_hotplug_latency_aggregates(metrics: &serde_json::Value, context: &str) {
    for operation in ["plug", "unplug", "unplug_all", "state"] {
        let field = format!("{operation}_agg");
        let aggregate = metrics[field.as_str()]
            .as_object()
            .unwrap_or_else(|| panic!("{context} metrics should contain memory_hotplug.{field}"));
        let mut fields = aggregate.keys().map(String::as_str).collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(fields, ["max_us", "min_us", "sum_us"]);
        let min_us = aggregate["min_us"]
            .as_u64()
            .expect("memory-hotplug minimum latency should be u64");
        let max_us = aggregate["max_us"]
            .as_u64()
            .expect("memory-hotplug maximum latency should be u64");
        let sum_us = aggregate["sum_us"]
            .as_u64()
            .expect("memory-hotplug summed latency should be u64");
        assert!(
            min_us <= max_us && max_us <= sum_us,
            "{context} memory_hotplug.{field} should satisfy min <= max <= sum"
        );
    }
}

fn assert_production_uart_extensions_absent(path: &Path, context: &str) {
    assert_metrics_family_extensions_absent(
        path,
        "uart",
        &["input_count", "interrupt_count", "overrun_count"],
        context,
    );
}

fn assert_serial_snapshot_output_redacted(
    stdout: &str,
    stderr: &str,
    sensitive: &[String],
    context: &str,
) {
    assert!(
        !stderr.contains("session-debug"),
        "{context} must not expose private diagnostics\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    for value in sensitive {
        assert!(
            !stdout.contains(value),
            "{context} stdout leaked startup grant data"
        );
        assert!(
            !stderr.contains(value),
            "{context} stderr leaked startup grant data"
        );
    }
}

fn configure_and_pause_snapshot_epoch_source(
    running: &RunningApiLauncher,
    metrics_path: &Path,
    blocks: Option<&SnapshotEpochBlockArtifacts>,
    writable_pmem_path: &Path,
    read_only_pmem_path: &Path,
    rooted: bool,
) {
    assert_eq!(
        blocks.is_none(),
        rooted,
        "rooted epoch products should be pmem-only"
    );
    for (path, body, context) in [
        (
            "/machine-config",
            serde_json::json!({
                "vcpu_count": 1,
                "mem_size_mib": 256,
            }),
            "PUT snapshot epoch machine config",
        ),
        (
            "/metrics",
            serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}),
            "PUT snapshot epoch metrics",
        ),
        (
            "/boot-source",
            serde_json::json!({
                "kernel_image_path": SNAPSHOT_KERNEL_REF,
                "initrd_path": SNAPSHOT_INITRD_REF,
                "boot_args": SNAPSHOT_BLOCK_BOOT_ARGS,
            }),
            "PUT snapshot epoch boot source",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body).expect("snapshot epoch request should serialize"),
            ),
            204,
            context,
        );
    }
    if blocks.is_some() {
        for (path, body, context) in [
            (
                "/drives/primary",
                serde_json::json!({
                    "drive_id": "primary",
                    "path_on_host": SNAPSHOT_ROOT_REF,
                    "is_root_device": false,
                    "is_read_only": false,
                    "cache_type": "Unsafe",
                    "io_engine": "Async",
                    "rate_limiter": {
                        "ops": {
                            "size": 1,
                            "refill_time": 1000,
                        },
                    },
                }),
                "PUT snapshot epoch writable Async Unsafe primary",
            ),
            (
                "/drives/data",
                serde_json::json!({
                    "drive_id": "data",
                    "path_on_host": SNAPSHOT_DATA_REF,
                    "is_root_device": false,
                    "is_read_only": false,
                    "cache_type": "Writeback",
                    "io_engine": "Sync",
                    "partuuid": SNAPSHOT_BLOCK_PARTUUID,
                }),
                "PUT snapshot epoch writable Sync Writeback data",
            ),
            (
                "/drives/audit",
                serde_json::json!({
                    "drive_id": "audit",
                    "path_on_host": SNAPSHOT_AUDIT_REF,
                    "is_root_device": false,
                    "is_read_only": true,
                    "cache_type": "Unsafe",
                    "io_engine": "Async",
                }),
                "PUT snapshot epoch read-only Async Unsafe audit",
            ),
        ] {
            assert_http_status(
                &http_put(
                    &running.socket,
                    path,
                    &serde_json::to_string(&body)
                        .expect("snapshot epoch block request should serialize"),
                ),
                204,
                context,
            );
        }
    }
    for (path, body, context) in [
        (
            "/pmem/epoch_rw",
            serde_json::json!({
                "id": "epoch_rw",
                "path_on_host": SNAPSHOT_PMEM_RW_REF,
                "root_device": rooted,
                "read_only": false,
                "rate_limiter": {
                    "ops": {
                        "size": 1,
                        "refill_time": SNAPSHOT_PMEM_LIMITER_REFILL_MS,
                    },
                },
            }),
            "PUT snapshot epoch writable pmem",
        ),
        (
            "/pmem/epoch_ro",
            serde_json::json!({
                "id": "epoch_ro",
                "path_on_host": SNAPSHOT_PMEM_RO_REF,
                "root_device": false,
                "read_only": true,
            }),
            "PUT snapshot epoch read-only pmem",
        ),
    ] {
        assert_http_status(
            &http_put(
                &running.socket,
                path,
                &serde_json::to_string(&body)
                    .expect("snapshot epoch pmem request should serialize"),
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
        "start snapshot epoch source",
    );
    wait_for_snapshot_pmem_epoch(
        writable_pmem_path,
        SNAPSHOT_PMEM_WRITABLE_PRE_CAPTURE_BYTE,
        PROCESS_TIMEOUT,
        "contained source pmem pre-capture epoch",
    );
    wait_for_snapshot_pmem_throttle(
        &running.socket,
        metrics_path,
        PROCESS_TIMEOUT,
        "contained source pending pmem limiter",
    );
    if let Some(blocks) = blocks {
        assert_snapshot_block_epoch(
            &blocks.root,
            SNAPSHOT_BLOCK_DRIVE_A_PRE_CAPTURE_BYTE,
            "contained source primary pre-capture epoch",
        );
        assert_snapshot_block_epoch(
            &blocks.data,
            SNAPSHOT_BLOCK_DRIVE_B_PRE_CAPTURE_BYTE,
            "contained source data pre-capture epoch",
        );
        assert_snapshot_block_epoch(
            &blocks.audit,
            SNAPSHOT_BLOCK_AUDIT_BYTE,
            "contained source audit epoch",
        );
    }
    assert_snapshot_pmem_epoch(
        read_only_pmem_path,
        SNAPSHOT_PMEM_READ_ONLY_BYTE,
        "contained source read-only pmem epoch",
    );
    assert_http_status(
        &http_request(&running.socket, "PATCH", "/vm", r#"{"state":"Paused"}"#),
        204,
        "pause snapshot epoch source",
    );
    assert_http_status(
        &http_put(
            &running.socket,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        ),
        204,
        "flush snapshot epoch source metrics",
    );
    if blocks.is_some() {
        assert_snapshot_block_metrics(metrics_path, true, "contained source metrics");
    }
    assert_snapshot_pmem_metrics(metrics_path, false, "contained source metrics");
}

fn configure_snapshot_epoch_destination_metrics(running: &RunningApiLauncher, context: &str) {
    assert_http_status(
        &http_put(
            &running.socket,
            "/metrics",
            &serde_json::to_string(&serde_json::json!({"metrics_path": SNAPSHOT_METRICS_REF}))
                .expect("snapshot epoch destination metrics should serialize"),
        ),
        204,
        &format!("PUT {context} metrics"),
    );
}

fn assert_snapshot_epoch_public_config(socket: &Path, rooted: bool, context: &str) {
    let config = http_get(socket, "/vm/config");
    assert_http_status(&config, 200, "read restored snapshot epoch configuration");
    for expected in [
        r#""id":"epoch_rw""#,
        r#""id":"epoch_ro""#,
        r#""read_only":false"#,
        r#""read_only":true"#,
        r#""rate_limiter""#,
    ] {
        assert!(
            config.contains(expected),
            "{context} restored configuration should contain {expected}; response:\n{config}"
        );
    }
    assert_eq!(
        config.contains(r#""root_device":true"#),
        rooted,
        "{context} restored pmem root role should be exact"
    );
    assert_eq!(
        config.matches(r#""drive_id":"#).count(),
        if rooted { 0 } else { 3 },
        "{context} restore should publish the exact block shape"
    );
    if !rooted {
        for expected in [
            r#""drive_id":"primary""#,
            r#""drive_id":"data""#,
            r#""drive_id":"audit""#,
            r#""is_read_only":false"#,
            r#""is_read_only":true"#,
            r#""cache_type":"Unsafe""#,
            r#""cache_type":"Writeback""#,
            r#""io_engine":"Async""#,
            r#""io_engine":"Sync""#,
            SNAPSHOT_BLOCK_PARTUUID,
        ] {
            assert!(
                config.contains(expected),
                "{context} restored mixed configuration should contain {expected}; response:\n{config}"
            );
        }
    }
}

fn create_snapshot_block_epoch_backing(path: &Path, initial: u8) {
    let mut bytes = vec![0_u8; 8 * SNAPSHOT_BLOCK_SECTOR_SIZE];
    bytes[..SNAPSHOT_BLOCK_SECTOR_SIZE].fill(initial);
    fs::write(path, bytes).unwrap_or_else(|error| {
        panic!(
            "snapshot block epoch backing {} should write: {error}",
            path.display()
        )
    });
}

fn create_snapshot_pmem_epoch_backing(path: &Path, initial: u8) {
    let mut bytes = vec![0_u8; SNAPSHOT_PMEM_FILE_BYTES];
    bytes[..SNAPSHOT_PMEM_SECTOR_SIZE].fill(initial);
    fs::write(path, bytes).unwrap_or_else(|error| {
        panic!(
            "snapshot pmem epoch backing {} should write: {error}",
            path.display()
        )
    });
}

fn snapshot_block_epoch(value: u8) -> Vec<u8> {
    vec![value; SNAPSHOT_BLOCK_SECTOR_SIZE]
}

fn snapshot_pmem_epoch(value: u8) -> Vec<u8> {
    vec![value; SNAPSHOT_PMEM_SECTOR_SIZE]
}

fn assert_snapshot_block_epoch(path: &Path, value: u8, context: &str) {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("{context} backing should read: {error}"));
    assert_eq!(
        bytes.get(..SNAPSHOT_BLOCK_SECTOR_SIZE),
        Some(snapshot_block_epoch(value).as_slice()),
        "{context} should retain the exact sector epoch"
    );
}

fn wait_for_snapshot_pmem_epoch(path: &Path, value: u8, timeout: Duration, context: &str) {
    wait_for_file_prefix(path, &snapshot_pmem_epoch(value), timeout)
        .unwrap_or_else(|error| panic!("{context} should become visible: {error}"));
}

fn assert_snapshot_pmem_epoch(path: &Path, value: u8, context: &str) {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("{context} backing should read: {error}"));
    assert_eq!(
        bytes.get(..SNAPSHOT_PMEM_SECTOR_SIZE),
        Some(snapshot_pmem_epoch(value).as_slice()),
        "{context} should retain the exact external-prefix epoch"
    );
    assert_eq!(
        bytes.len(),
        SNAPSHOT_PMEM_FILE_BYTES,
        "{context} should retain the exact unaligned file length"
    );
}

fn wait_for_snapshot_pmem_throttle(
    socket: &Path,
    metrics: &Path,
    timeout: Duration,
    context: &str,
) {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("snapshot pmem throttle deadline should fit");
    loop {
        assert_http_status(
            &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
            204,
            &format!("PUT {context} FlushMetrics"),
        );
        if snapshot_pmem_metric_total_if_readable(metrics, "rate_limiter_throttled_events") > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context} should report a throttled pmem request before timeout; metrics:\n{}",
            fs::read_to_string(metrics).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

const SNAPSHOT_BLOCK_METRIC_FIELDS: [&str; 20] = [
    "activate_fails",
    "cfg_fails",
    "no_avail_buffer",
    "event_fails",
    "execute_fails",
    "invalid_reqs_count",
    "flush_count",
    "queue_event_count",
    "rate_limiter_event_count",
    "update_count",
    "update_fails",
    "read_bytes",
    "write_bytes",
    "read_count",
    "write_count",
    "read_agg",
    "write_agg",
    "rate_limiter_throttled_events",
    "io_engine_throttled_events",
    "remaining_reqs_count",
];
const SNAPSHOT_BLOCK_COUNTER_FIELDS: [&str; 18] = [
    "activate_fails",
    "cfg_fails",
    "no_avail_buffer",
    "event_fails",
    "execute_fails",
    "invalid_reqs_count",
    "flush_count",
    "queue_event_count",
    "rate_limiter_event_count",
    "update_count",
    "update_fails",
    "read_bytes",
    "write_bytes",
    "read_count",
    "write_count",
    "rate_limiter_throttled_events",
    "io_engine_throttled_events",
    "remaining_reqs_count",
];

fn assert_snapshot_block_metric_shape(metric: &serde_json::Value, context: &str) {
    let object = metric
        .as_object()
        .unwrap_or_else(|| panic!("{context} block metric should be an object"));
    assert_eq!(
        object.len(),
        SNAPSHOT_BLOCK_METRIC_FIELDS.len(),
        "{context} should contain exactly the 24 Firecracker block leaves"
    );
    for field in SNAPSHOT_BLOCK_METRIC_FIELDS {
        assert!(
            object.contains_key(field),
            "{context} should contain block field {field}"
        );
    }
    for field in SNAPSHOT_BLOCK_COUNTER_FIELDS {
        assert!(
            object.get(field).is_some_and(serde_json::Value::is_u64),
            "{context}.{field} should be an unsigned counter"
        );
    }
    for aggregate in ["read_agg", "write_agg"] {
        let aggregate = object
            .get(aggregate)
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("{context}.{aggregate} should be an object"));
        assert_eq!(
            aggregate.len(),
            3,
            "{context} latency shape should be exact"
        );
        for field in ["min_us", "max_us", "sum_us"] {
            assert!(
                aggregate.get(field).is_some_and(serde_json::Value::is_u64),
                "{context} latency field {field} should be unsigned"
            );
        }
    }
}

fn assert_snapshot_block_metrics(path: &Path, expect_limiter: bool, context: &str) {
    let output = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("{context} metrics {} should read: {error}", path.display())
    });
    let values = output
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|error| panic!("{context} metrics line should parse: {error}"))
        })
        .collect::<Vec<_>>();
    assert!(!values.is_empty(), "{context} should emit metrics lines");
    for (line_index, value) in values.iter().enumerate() {
        let root = value
            .as_object()
            .unwrap_or_else(|| panic!("{context} metrics line {line_index} should be an object"));
        let static_metric = root
            .get("block")
            .unwrap_or_else(|| panic!("{context} line {line_index} should contain block"));
        assert_snapshot_block_metric_shape(
            static_metric,
            &format!("{context} line {line_index} block"),
        );
        for drive_id in ["primary", "data", "audit"] {
            let key = format!("block_{drive_id}");
            let metric = root
                .get(&key)
                .unwrap_or_else(|| panic!("{context} line {line_index} should contain {key}"));
            assert_snapshot_block_metric_shape(
                metric,
                &format!("{context} line {line_index} {key}"),
            );
        }
        for field in SNAPSHOT_BLOCK_COUNTER_FIELDS {
            let expected = ["primary", "data", "audit"]
                .into_iter()
                .map(|drive_id| {
                    root[&format!("block_{drive_id}")][field]
                        .as_u64()
                        .expect("validated block counter should remain unsigned")
                })
                .fold(0_u64, u64::saturating_add);
            assert_eq!(
                static_metric[field].as_u64(),
                Some(expected),
                "{context} line {line_index} static {field} should derive from configured drives"
            );
        }
        for aggregate in ["read_agg", "write_agg"] {
            let expected_sum = ["primary", "data", "audit"]
                .into_iter()
                .map(|drive_id| {
                    root[&format!("block_{drive_id}")][aggregate]["sum_us"]
                        .as_u64()
                        .expect("validated block latency should remain unsigned")
                })
                .fold(0_u64, u64::saturating_add);
            assert_eq!(static_metric[aggregate]["min_us"], 0);
            assert_eq!(static_metric[aggregate]["max_us"], 0);
            assert_eq!(static_metric[aggregate]["sum_us"], expected_sum);
        }
        for field in [
            "activate_fails",
            "cfg_fails",
            "event_fails",
            "execute_fails",
            "invalid_reqs_count",
            "update_fails",
        ] {
            assert_eq!(
                static_metric[field], 0,
                "{context} successful signed workload should not report {field}"
            );
        }
    }
    for (drive_id, expect_write) in [("primary", true), ("data", true), ("audit", false)] {
        assert!(
            snapshot_block_metric_total(path, drive_id, "queue_event_count") > 0,
            "{context} should report queue events for {drive_id}"
        );
        assert!(
            snapshot_block_metric_total(path, drive_id, "read_count") > 0,
            "{context} should report reads for {drive_id}"
        );
        if expect_write {
            assert!(
                snapshot_block_metric_total(path, drive_id, "write_count") > 0,
                "{context} should report writes for {drive_id}"
            );
        } else {
            assert_eq!(
                snapshot_block_metric_total(path, drive_id, "write_count"),
                0,
                "{context} must not report an audit write"
            );
        }
    }
    if expect_limiter {
        assert!(
            snapshot_block_metric_total(path, "primary", "rate_limiter_throttled_events") > 0,
            "{context} should report a throttled primary request; metrics:\n{output}"
        );
    }
}

fn assert_snapshot_pmem_metrics(path: &Path, expect_retry: bool, context: &str) {
    let output = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("{context} metrics {} should read: {error}", path.display())
    });
    assert!(
        snapshot_pmem_metric_total_if_readable(path, "queue_event_count") > 0,
        "{context} should report writable pmem queue events; metrics:\n{output}"
    );
    assert!(
        snapshot_pmem_metric_total_if_readable(path, "rate_limiter_throttled_events") > 0,
        "{context} should report a throttled writable pmem request; metrics:\n{output}"
    );
    if expect_retry {
        assert!(
            snapshot_pmem_metric_total_if_readable(path, "rate_limiter_event_count") > 0,
            "{context} should report restored pmem limiter progress; metrics:\n{output}"
        );
    }
    assert!(
        output.lines().all(|line| {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                return false;
            };
            value
                .as_object()
                .is_some_and(|root| !root.keys().any(|key| key.starts_with("pmem_")))
        }),
        "{context} must not publish dynamic pmem roots; metrics:\n{output}"
    );
}

fn snapshot_block_metric_total(path: &Path, drive_id: &str, field: &str) -> u64 {
    let section = format!("block_{drive_id}");
    fs::read_to_string(path)
        .unwrap_or_else(|error| {
            panic!(
                "snapshot block metrics {} should read: {error}",
                path.display()
            )
        })
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get(&section)?
                .get(field)?
                .as_u64()
        })
        .fold(0, u64::saturating_add)
}

fn snapshot_pmem_metric_total_if_readable(path: &Path, field: &str) -> u64 {
    let Ok(output) = fs::read_to_string(path) else {
        return 0;
    };
    output
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("pmem")?
                .get(field)?
                .as_u64()
        })
        .fold(0, u64::saturating_add)
}

fn wait_for_snapshot_root_read(socket: &Path, metrics: &Path, timeout: Duration) {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("snapshot metric deadline should fit");
    let mut last_count = 0;
    let mut stable_since = None;
    loop {
        assert_http_status(
            &http_put(socket, "/actions", r#"{"action_type":"FlushMetrics"}"#),
            204,
            "flush snapshot metrics",
        );
        let count = total_snapshot_root_read_count(metrics);
        let now = Instant::now();
        if count >= 1 {
            if count != last_count {
                last_count = count;
                stable_since = Some(now);
            } else if stable_since
                .is_some_and(|started| now.duration_since(started) >= Duration::from_millis(500))
            {
                return;
            }
        }
        assert!(
            now < deadline,
            "snapshot source did not complete root I/O before timeout"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn total_snapshot_root_read_count(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .map(|output| {
            output
                .lines()
                .filter_map(|line| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .ok()?
                        .get("block_rootfs")?
                        .get("read_count")?
                        .as_u64()
                })
                .fold(0, u64::saturating_add)
        })
        .unwrap_or(0)
}

fn snapshot_create_body() -> String {
    snapshot_create_body_for(SNAPSHOT_STATE_OUTPUT_REF, SNAPSHOT_MEMORY_OUTPUT_REF)
}

fn snapshot_diff_create_body() -> String {
    snapshot_create_body_for_type(
        "Diff",
        SNAPSHOT_STATE_OUTPUT_REF,
        SNAPSHOT_MEMORY_OUTPUT_REF,
    )
}

fn repeated_snapshot_create_body() -> String {
    snapshot_create_body_for(
        SNAPSHOT_REPEAT_STATE_OUTPUT_REF,
        SNAPSHOT_REPEAT_MEMORY_OUTPUT_REF,
    )
}

fn snapshot_create_body_for(state: &str, memory: &str) -> String {
    snapshot_create_body_for_type("Full", state, memory)
}

fn snapshot_create_body_for_type(snapshot_type: &str, state: &str, memory: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "snapshot_type": snapshot_type,
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

fn snapshot_editor_file_facts(path: &Path) -> SnapshotEditorFileFacts {
    let metadata =
        fs::symlink_metadata(path).expect("snapshot editor artifact metadata should read");
    assert!(
        metadata.file_type().is_file(),
        "snapshot editor artifact must be a regular file"
    );
    SnapshotEditorFileFacts {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        owner: metadata.uid(),
        group: metadata.gid(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    }
}

fn run_snapshot_editor_info(editor: &Path, view: &str, state: &Path) -> Output {
    let mut command = Command::new(editor);
    command
        .args(["info-vmstate", view, "--vmstate-path"])
        .arg(state);
    run_with_timeout(
        &mut command,
        PROCESS_TIMEOUT,
        &format!("signed snapshot-editor {view}"),
    )
}

fn assert_snapshot_editor_output_redacted(output: &Output, sensitive_paths: &[&Path]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for path in sensitive_paths {
        let value = path_text(path);
        assert!(
            !stdout.contains(value),
            "snapshot editor stdout exposed a path"
        );
        assert!(
            !stderr.contains(value),
            "snapshot editor stderr exposed a path"
        );
    }
    assert!(
        !stdout.contains(SNAPSHOT_EDITOR_DBGBVR0),
        "snapshot editor stdout exposed a register ID"
    );
    assert!(
        !stderr.contains(SNAPSHOT_EDITOR_DBGBVR0),
        "snapshot editor stderr exposed a register ID"
    );
}

fn run_snapshot_editor_info_twice(editor: &Path, view: &str, state: &Path) -> Vec<u8> {
    let first = run_snapshot_editor_info(editor, view, state);
    assert_output_success(&first, &format!("first signed snapshot-editor {view}"));
    assert!(
        first.stderr.is_empty(),
        "successful snapshot-editor {view} must not emit stderr"
    );
    assert_snapshot_editor_output_redacted(&first, &[state]);

    let second = run_snapshot_editor_info(editor, view, state);
    assert_output_success(&second, &format!("second signed snapshot-editor {view}"));
    assert!(
        second.stderr.is_empty(),
        "repeated snapshot-editor {view} must not emit stderr"
    );
    assert_snapshot_editor_output_redacted(&second, &[state]);
    assert_eq!(
        second.stdout, first.stdout,
        "snapshot-editor {view} output must be deterministic"
    );
    first.stdout
}

fn inspect_with_signed_snapshot_editor(
    editor: &Path,
    state: &Path,
    transport: &str,
    is_diff: bool,
) -> SnapshotEditorViews {
    let (version, minor, profile) = if is_diff {
        ("v2.13.0\n", 13_u64, "diff-state-v2.13")
    } else {
        ("v2.12.0\n", 12_u64, "vsock-state-v2.12")
    };
    let version_output = run_snapshot_editor_info_twice(editor, "version", state);
    assert_eq!(version_output, version.as_bytes());

    let vcpus_output = run_snapshot_editor_info_twice(editor, "vcpu-states", state);
    let vm_output = run_snapshot_editor_info_twice(editor, "vm-state", state);
    let vcpus: serde_json::Value = serde_json::from_slice(&vcpus_output)
        .expect("signed snapshot-editor vCPU output should be JSON");
    let vm: serde_json::Value = serde_json::from_slice(&vm_output)
        .expect("signed snapshot-editor VM output should be JSON");

    for (view, value) in [("vcpu-states", &vcpus), ("vm-state", &vm)] {
        assert_eq!(value["schema"], "bangbang.snapshot-editor.info.v1");
        assert_eq!(value["view"], view);
        assert_eq!(value["family"], "native-v2");
        assert_eq!(value["profile"], profile);
        assert_eq!(
            value["version"],
            serde_json::json!({"major": 2, "minor": minor, "patch": 0})
        );
        let states = value["vcpus"]
            .as_array()
            .expect("signed snapshot-editor vCPUs should be an array");
        assert_eq!(states.len(), 1, "production snapshot should have one vCPU");
        assert_eq!(states[0]["index"], 0);
        assert!(
            states[0]["general"]["pc"]
                .as_str()
                .is_some_and(|pc| pc.starts_with("0x")),
            "inspection must retain value-bearing portable registers"
        );
        assert!(
            states[0]["debug"]["reviewed"].is_object(),
            "production snapshot must contain reviewed debug state"
        );
    }
    assert_eq!(vcpus["vcpus"], vm["vcpus"]);
    assert!(
        vm.to_string().contains("<redacted>"),
        "VM inspection must contain literal authority redaction"
    );
    assert!(
        vm["memory"]["file_length"]
            .as_u64()
            .is_some_and(|len| len > 0),
        "VM inspection must retain a concrete memory binding"
    );
    assert_eq!(vm["devices"]["storage"]["transport_kind"], transport);
    assert_eq!(vm["devices"]["storage"]["record_count"], 3);
    assert_eq!(vm["devices"]["storage"]["block_record_count"], 3);
    assert_eq!(vm["devices"]["storage"]["pmem_record_count"], 0);

    if is_diff {
        assert_eq!(vm["diff"]["compatibility"], "v2.13");
        assert_eq!(vm["diff"]["base"]["kind"], "zero");
        assert!(vm["diff"]["base"]["binding"].is_null());
        assert!(
            vm["diff"]["extent_count"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "production Diff inspection must retain a nonempty sparse layer"
        );
        assert_eq!(vm["diff"]["result"], vm["memory"]);
        assert_eq!(
            vm["diff"]["relationship"],
            serde_json::json!({
                "base_is_image": false,
                "base_and_result_are_distinct": true,
                "result_matches_vm_memory": true,
                "omitted_bytes_inherit_base": true,
            })
        );
    } else {
        assert!(vm["diff"].is_null());
    }

    SnapshotEditorViews { vcpus, vm }
}

fn normalize_snapshot_editor_reviewed_fingerprints(
    mut value: serde_json::Value,
) -> (serde_json::Value, Vec<serde_json::Value>) {
    let states = value["vcpus"]
        .as_array_mut()
        .expect("snapshot-editor vCPUs should remain an array");
    let mut fingerprints = Vec::with_capacity(states.len());
    for state in states {
        let reviewed = state["debug"]["reviewed"].clone();
        assert!(
            reviewed.is_object(),
            "reviewed debug fingerprint must be present"
        );
        fingerprints.push(reviewed);
        state["debug"]["reviewed"] = serde_json::Value::String("<normalized>".to_owned());
    }
    (value, fingerprints)
}

fn assert_snapshot_editor_views_change_only_reviewed_debug(
    before: &SnapshotEditorViews,
    after: &SnapshotEditorViews,
) {
    for (view, before, after) in [
        ("vcpu-states", &before.vcpus, &after.vcpus),
        ("vm-state", &before.vm, &after.vm),
    ] {
        let (before_normalized, before_fingerprints) =
            normalize_snapshot_editor_reviewed_fingerprints(before.clone());
        let (after_normalized, after_fingerprints) =
            normalize_snapshot_editor_reviewed_fingerprints(after.clone());
        assert_eq!(
            before_normalized, after_normalized,
            "snapshot-editor {view} changed state outside reviewed debug fingerprints"
        );
        assert_eq!(before_fingerprints.len(), 1);
        assert_eq!(after_fingerprints.len(), 1);
        assert_ne!(
            before_fingerprints, after_fingerprints,
            "snapshot-editor {view} must demonstrate the reviewed debug edit"
        );
    }
}

fn assert_snapshot_editor_views_redacted(views: &SnapshotEditorViews, sensitive_paths: &[&Path]) {
    let output = format!("{}{}", views.vcpus, views.vm);
    for path in sensitive_paths {
        assert!(
            !output.contains(path_text(path)),
            "snapshot-editor JSON exposed product path authority"
        );
    }
}

fn certify_signed_snapshot_editor(
    artifacts: &SnapshotArtifactSet,
    original_state_bytes: Vec<u8>,
    memory_bytes: Vec<u8>,
    transport: &str,
    is_diff: bool,
) -> SnapshotEditorCertification {
    let editor = snapshot_editor();
    let original_state = artifacts.state.clone();
    let original_state_facts = snapshot_editor_file_facts(&original_state);
    let memory_facts = snapshot_editor_file_facts(&artifacts.memory);
    assert_eq!(
        fs::read(&original_state).expect("snapshot-editor source state should read"),
        original_state_bytes
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("snapshot-editor memory should read"),
        memory_bytes
    );

    let before = inspect_with_signed_snapshot_editor(&editor, &original_state, transport, is_diff);
    assert_snapshot_editor_views_redacted(
        &before,
        &[
            &original_state,
            &artifacts.memory,
            &artifacts.root,
            &artifacts.data,
            &artifacts.audit,
        ],
    );
    let edited_state = original_state
        .parent()
        .expect("snapshot state should have a parent")
        .join(SNAPSHOT_EDITOR_OUTPUT_CHILD);
    assert!(
        !edited_state.exists(),
        "snapshot-editor output must begin absent"
    );
    let mut command = Command::new(&editor);
    command
        .args(["edit-vmstate", "remove-regs", SNAPSHOT_EDITOR_DBGBVR0])
        .arg("--vmstate-path")
        .arg(&original_state)
        .arg("--output-path")
        .arg(&edited_state);
    let edit = run_with_timeout(
        &mut command,
        PROCESS_TIMEOUT,
        "signed snapshot-editor reviewed register removal",
    );
    assert_output_success(&edit, "signed snapshot-editor reviewed register removal");
    assert_eq!(
        edit.stdout,
        b"vcpu 0: removed 1, not-present 0\ntotal: requested 1, removed 1, not-present 0\n"
    );
    assert!(
        edit.stderr.is_empty(),
        "successful snapshot-editor edit must not emit stderr"
    );
    assert_snapshot_editor_output_redacted(&edit, &[&original_state, &edited_state]);

    assert_eq!(
        fs::read(&original_state).expect("snapshot-editor source state should remain readable"),
        original_state_bytes,
        "snapshot-editor must not change source bytes"
    );
    assert_eq!(
        snapshot_editor_file_facts(&original_state),
        original_state_facts,
        "snapshot-editor must not change source inode facts"
    );
    assert_eq!(
        fs::read(&artifacts.memory).expect("snapshot-editor memory should remain readable"),
        memory_bytes,
        "state editing must not change memory or Diff bytes"
    );
    assert_eq!(
        snapshot_editor_file_facts(&artifacts.memory),
        memory_facts,
        "state editing must not change memory or Diff inode facts"
    );

    let edited_state_facts = snapshot_editor_file_facts(&edited_state);
    assert_ne!(
        (edited_state_facts.device, edited_state_facts.inode),
        (original_state_facts.device, original_state_facts.inode),
        "snapshot-editor output must use a distinct inode"
    );
    assert_eq!(edited_state_facts.mode & 0o7777, 0o600);
    assert_eq!(edited_state_facts.links, 1);
    let edited_state_bytes =
        fs::read(&edited_state).expect("snapshot-editor output state should read");
    assert_ne!(edited_state_bytes, original_state_bytes);
    let edited_document = HvfNativeSnapshotDocument::decode(&edited_state_bytes)
        .expect("snapshot-editor output must decode canonically");
    assert_eq!(
        edited_document
            .encode()
            .expect("snapshot-editor output must re-encode"),
        edited_state_bytes,
        "snapshot-editor output must be canonical"
    );

    let after = inspect_with_signed_snapshot_editor(&editor, &edited_state, transport, is_diff);
    assert_snapshot_editor_views_redacted(
        &after,
        &[
            &original_state,
            &edited_state,
            &artifacts.memory,
            &artifacts.root,
            &artifacts.data,
            &artifacts.audit,
        ],
    );
    assert_snapshot_editor_views_change_only_reviewed_debug(&before, &after);
    assert_no_snapshot_staging(
        original_state
            .parent()
            .expect("snapshot state should have a parent"),
    );
    assert_no_snapshot_staging(
        artifacts
            .memory
            .parent()
            .expect("snapshot memory should have a parent"),
    );

    let mut edited_artifacts = artifacts.clone();
    edited_artifacts.state = edited_state;
    SnapshotEditorCertification {
        artifacts: edited_artifacts,
        original_state,
        original_state_bytes,
        original_state_facts,
        edited_state_bytes,
        edited_state_facts,
        memory_bytes,
        memory_facts,
    }
}

impl SnapshotEditorCertification {
    fn assert_original_state_unchanged(&self, context: &str) {
        assert_eq!(
            fs::read(&self.original_state).expect("original snapshot state should remain readable"),
            self.original_state_bytes,
            "{context} must preserve original snapshot state bytes"
        );
        assert_eq!(
            snapshot_editor_file_facts(&self.original_state),
            self.original_state_facts,
            "{context} must preserve original snapshot state inode facts"
        );
    }

    fn assert_opened_artifacts_unchanged(&self, opened: &SnapshotArtifactSet, context: &str) {
        self.assert_original_state_unchanged(context);
        assert_eq!(
            fs::read(&opened.state).expect("opened edited snapshot state should remain readable"),
            self.edited_state_bytes,
            "{context} must preserve edited snapshot state bytes"
        );
        assert_eq!(
            snapshot_editor_file_facts(&opened.state),
            self.edited_state_facts,
            "{context} must preserve edited snapshot state inode facts"
        );
        assert_eq!(
            fs::read(&opened.memory).expect("opened snapshot memory should remain readable"),
            self.memory_bytes,
            "{context} must preserve Full memory or Diff layer bytes"
        );
        assert_eq!(
            snapshot_editor_file_facts(&opened.memory),
            self.memory_facts,
            "{context} must preserve Full memory or Diff layer inode facts"
        );
    }
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
                || name.starts_with(".bangbang-snapshot-edit-")
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

fn assert_session_entries_eventually_restored(expected: &[PathBuf], context: &str) {
    let deadline = Instant::now()
        .checked_add(PROCESS_TIMEOUT)
        .expect("session cleanup deadline should fit");
    loop {
        let current = session_entries();
        if current == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context} should restore the session namespace; expected {expected:?}, observed {current:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
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

fn read_stdout_until_line_shared(
    child: &mut Child,
    expected_line: &'static str,
) -> (Receiver<()>, JoinHandle<String>, Arc<Mutex<String>>) {
    let stdout = child.stdout.take().expect("stdout should be piped");
    let (ready_sender, ready_receiver) = mpsc::channel();
    let output = Arc::new(Mutex::new(String::new()));
    let shared_output = Arc::clone(&output);
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
            let mut output = shared_output
                .lock()
                .expect("launcher stdout snapshot should lock");
            output.push_str(&line);
            output.push('\n');
        }
        collected
    });
    (ready_receiver, reader, output)
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

fn reset_zeroed_file(path: &Path, len: u64) {
    assert!(len > 0, "reset backing length must be nonzero");
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("reset backing should reopen");
    file.set_len(0).expect("reset backing should truncate");
    file.set_len(len).expect("reset backing should regrow");
    file.sync_all().expect("reset backing should fsync");
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

fn wait_for_file_marker_at(
    path: &Path,
    offset: u64,
    marker: &[u8],
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if fs::metadata(path).is_ok() && file_bytes_at(path, offset, marker.len()) == marker {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            let observed = file_bytes_at(path, offset, marker.len());
            return Err(format!(
                "timed out after {timeout:?} waiting for marker {:?} at offset {offset}; observed {observed:?}",
                String::from_utf8_lossy(marker),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn write_virtual_block_marker_at(media: &MacosVirtualBlock, offset: u64, marker: &[u8]) {
    let mut sector = vec![0_u8; VIRTIO_BLOCK_SECTOR_BYTES as usize];
    sector[..marker.len()].copy_from_slice(marker);
    media
        .write_at(offset, &sector)
        .expect("contained virtual block marker should persist");
}

fn wait_for_virtual_block_marker(
    media: &MacosVirtualBlock,
    offset: u64,
    marker: &[u8],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match media.read_at(offset, marker.len()) {
            Ok(bytes) if bytes == marker => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(bytes) => {
                return Err(format!(
                    "timed out waiting for contained virtual block marker; observed {bytes:?}"
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
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

fn wait_for_memory_hotplug_snapshot_marker(path: &Path, marker: &[u8], context: &str) {
    let deadline = Instant::now()
        .checked_add(SNAPSHOT_MEMORY_HOTPLUG_TIMEOUT)
        .expect("memory-hotplug marker deadline should fit");
    loop {
        let contents = fs::read(path).unwrap_or_default();
        if contents.starts_with(marker) {
            return;
        }
        assert!(
            !contents.starts_with(SNAPSHOT_MEMORY_HOTPLUG_FAILURE_MARKER),
            "{context} reported guest failure; backing prefix: {:?}",
            String::from_utf8_lossy(&contents[..contents.len().min(128)])
        );
        assert!(
            Instant::now() < deadline,
            "{context} timed out waiting for {:?}; backing prefix: {:?}",
            String::from_utf8_lossy(marker),
            String::from_utf8_lossy(&contents[..contents.len().min(128)])
        );
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

fn wait_for_http_response_fragment(
    socket: &Path,
    path: &str,
    fragment: &str,
    timeout: Duration,
) -> Result<String, String> {
    let started = Instant::now();
    loop {
        let response = http_get(socket, path);
        if response.contains(fragment) {
            return Ok(response);
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "timed out after {timeout:?} waiting for {path} to contain {fragment:?}; last response:\n{response}"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_vhost_user_memory_aperture(report: &VhostUserBlockBackendReport, context: &str) {
    const MIB: u64 = 1024 * 1024;
    const ARM64_DRAM_START: u64 = 0x8000_0000;
    const VIRTIO_MEM_APERTURE_START: u64 = 0x80_0000_0000;
    let geometry = report
        .memory_region_geometry
        .iter()
        .map(|region| {
            (
                region.guest_phys_addr,
                region.memory_size,
                region.mmap_offset,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        geometry,
        vec![
            (ARM64_DRAM_START, 256 * MIB, 0),
            (VIRTIO_MEM_APERTURE_START, 128 * MIB, 0),
        ],
        "{context} backend must receive boot RAM plus one complete stable aperture"
    );
    assert_eq!(report.memory_regions, geometry.len());
    assert_eq!(report.memory_table_requests, 1);
}

fn assert_aggregate_storage_vhost_user_memory_aperture(report: &VhostUserBlockBackendReport) {
    const MIB: u64 = 1024 * 1024;
    const ARM64_DRAM_START: u64 = 0x8000_0000;
    const VIRTIO_MEM_APERTURE_AFTER_PMEM: u64 = 0x80_0800_0000;
    let geometry = report
        .memory_region_geometry
        .iter()
        .map(|region| {
            (
                region.guest_phys_addr,
                region.memory_size,
                region.mmap_offset,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        geometry,
        vec![
            (ARM64_DRAM_START, 256 * MIB, 0),
            (VIRTIO_MEM_APERTURE_AFTER_PMEM, 128 * MIB, 0),
        ],
        "contained aggregate startup pmem must move the complete virtio-mem aperture to the next 128 MiB slot",
    );
    assert_eq!(report.memory_regions, geometry.len());
    assert_eq!(report.memory_table_requests, 1);
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

fn assert_phase_block_serial_report(
    path: &Path,
    begin: &[u8],
    end: &[u8],
    expected: &str,
    context: &str,
) {
    let output = fs::read(path).expect("phase block serial output should be readable");
    let normalized = String::from_utf8_lossy(&output).replace('\r', "");
    let expected_report = format!(
        "{}\n{expected}\n{}",
        String::from_utf8_lossy(begin),
        String::from_utf8_lossy(end),
    );
    assert!(
        normalized.contains(&expected_report),
        "{context} guest block serial must equal the exact current grant descriptor identity"
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

fn wait_for_stream_eof_nonblocking(
    stream: &mut UnixStream,
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("stream EOF deadline should fit Instant");
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(std::io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_socket_event(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return Ok(()),
            Err(error) => return Err(error),
        }
    }
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
    try_http_request(socket, method, path, body)
        .unwrap_or_else(|error| panic!("HTTP request {method} {path} should complete: {error}"))
}

fn try_http_request(
    socket: &Path,
    method: &str,
    path: &str,
    body: &str,
) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(HTTP_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_IO_TIMEOUT))?;
    write_http_request_frame(&mut stream, method, path, body)?;
    if let Err(error) = stream.shutdown(std::net::Shutdown::Write)
        && error.kind() != std::io::ErrorKind::NotConnected
    {
        return Err(error);
    }
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn write_http_request_frame(
    writer: &mut impl Write,
    method: &str,
    path: &str,
    body: &str,
) -> std::io::Result<()> {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    writer.write_all(request.as_bytes())
}

#[test]
fn production_http_request_writer_submits_one_complete_frame() {
    #[derive(Default)]
    struct RecordingWriter {
        writes: Vec<Vec<u8>>,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes.push(bytes.to_vec());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let body = r#"{"state":"Resumed","marker":"雪"}"#;
    let expected = format!(
        "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut writer = RecordingWriter::default();

    write_http_request_frame(&mut writer, "PATCH", "/vm", body)
        .expect("complete production HTTP frame should write");

    assert_eq!(writer.writes, vec![expected.into_bytes()]);
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

fn codesign_entitlement_dictionary(path: &Path) -> plist::Dictionary {
    let xml = codesign_entitlements(path);
    if xml.trim().is_empty() {
        return plist::Dictionary::new();
    }
    let value =
        plist::Value::from_reader_xml(xml.as_bytes()).expect("entitlements should be a plist");
    value
        .as_dictionary()
        .expect("entitlements plist should contain a dictionary")
        .clone()
}

fn assert_exact_networkless_bundle_entitlements(bundle: &Path) {
    let launcher = codesign_entitlement_dictionary(bundle);
    assert!(
        launcher.is_empty(),
        "networkless launcher entitlement dictionary must be empty: {launcher:?}"
    );

    let worker = codesign_entitlement_dictionary(&worker_bundle(bundle));
    assert_eq!(
        worker.len(),
        2,
        "networkless worker must retain exactly two entitlements: {worker:?}"
    );
    for key in [
        "com.apple.security.app-sandbox",
        "com.apple.security.hypervisor",
    ] {
        assert_eq!(
            worker.get(key),
            Some(&plist::Value::Boolean(true)),
            "networkless worker entitlement {key} must be exactly Boolean true: {worker:?}"
        );
    }
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
