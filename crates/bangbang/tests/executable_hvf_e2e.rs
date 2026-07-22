// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[path = "../../../tests/support/macos_virtual_block.rs"]
mod macos_virtual_block;
mod support;
#[path = "../../../tests/support/vhost_user_block.rs"]
mod vhost_user_block;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_arm64 {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::net::Shutdown;
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::macos_virtual_block::{
        MacosVirtualBlock, MacosVirtualBlockAccess, MacosVirtualBlockSize,
    };
    use crate::support::{
        BangbangProcess, CompletedProcess, TestDir, assert_bad_request_response,
        assert_clean_shutdown, assert_no_content_response, assert_ok_response,
        assert_response_contains, http_get, http_json, http_json_with_io_timeout, http_no_body,
        http_put_json, json_string, path_text,
    };
    use crate::vhost_user_block::{
        VhostUserBlockBackend, VhostUserBlockBackendOptions, VhostUserBlockBackendReport,
    };

    const BANGBANG_GUEST_KERNEL_PATH_ENV: &str = "BANGBANG_GUEST_KERNEL_PATH";
    const BANGBANG_GUEST_INITRD_PATH_ENV: &str = "BANGBANG_GUEST_INITRD_PATH";
    const BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
    const BLOCK_WRITE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITE_OK";
    const DIRECT_ROOTFS_BLOCK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BLOCK_OK";
    const DIRECT_ROOTFS_BOOT_OK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BOOT_OK";
    const BOOT_TIMER_LOG_MARKER: &[u8] = b"Guest-boot-time =";
    const DIRECT_ROOTFS_BALLOON_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.balloon-check=1";
    const DIRECT_ROOTFS_BALLOON_MARKER: &[u8] = b"BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.memory-hotplug-check=1";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_READY";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_GROWN_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_GROWN";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_RTC_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.rtc-check=1";
    const DIRECT_ROOTFS_RTC_MARKER: &[u8] = b"BANGBANG_RTC_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_VMCLOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vmclock-check=1";
    const DIRECT_ROOTFS_VMCLOCK_MARKER: &[u8] = b"BANGBANG_VMCLOCK_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-writeback-flush=1";
    const DIRECT_ROOTFS_WRITEBACK_FLUSH_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITEBACK_FLUSH_OK";
    const BLOCK_SERIAL_BEGIN_MARKER: &[u8] = b"BANGBANG_BLOCK_SERIAL_BEGIN";
    const BLOCK_SERIAL_END_MARKER: &[u8] = b"BANGBANG_BLOCK_SERIAL_END";
    const VHOST_USER_BLOCK_HOST_MARKER: &[u8] = b"BANGBANG_VHOST_USER_BLOCK_HOST";
    const VHOST_USER_BLOCK_RO_MARKER: &[u8] = b"BANGBANG_VHOST_USER_BLOCK_ro_OK";
    const VHOST_USER_BLOCK_RW_MARKER: &[u8] = b"BANGBANG_VHOST_USER_BLOCK_rw_OK";
    const VHOST_USER_BLOCK_PARTUUID: &str = "0eaa91a0-01";
    const DIRECT_ROOTFS_ENTROPY_MARKER: &[u8] = b"BANGBANG_ENTROPY_GUEST_READ_OK";
    const DIRECT_ROOTFS_ENTROPY_LIFECYCLE_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.entropy-lifecycle=1";
    const DIRECT_ROOTFS_ENTROPY_LIFECYCLE_READY_MARKER: &[u8] = b"BANGBANG_ENTROPY_LIFECYCLE_READY";
    const DIRECT_ROOTFS_ENTROPY_HOST_CONTINUE_MARKER: &[u8] = b"BANGBANG_ENTROPY_HOST_CONTINUE";
    const DIRECT_ROOTFS_ENTROPY_LIFECYCLE_SUCCESS_MARKER: &[u8] = b"BANGBANG_ENTROPY_LIFECYCLE_OK";
    const ENTROPY_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(60);
    const DIRECT_ROOTFS_REMAINING_DEVICE_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.remaining-device-certification=1";
    const REMAINING_DEVICE_SERIAL_INPUT: &str = "BANGBANG_REMAINING_DEVICE_SERIAL_ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz_0123456789_END";
    const REMAINING_DEVICE_SERIAL_READY_MARKER: &str = "BANGBANG_REMAINING_DEVICE_SERIAL_READY";
    const REMAINING_DEVICE_SERIAL_SUCCESS_MARKER: &str = "BANGBANG_REMAINING_DEVICE_SERIAL_OK";
    const REMAINING_DEVICE_SUCCESS_MARKER: &str = "BANGBANG_REMAINING_DEVICE_CERTIFICATION_OK";
    const REMAINING_DEVICE_FAILURE_MARKER: &str = "BANGBANG_REMAINING_DEVICE_FAIL_";
    const REMAINING_DEVICE_FINAL_MARKER_OFFSET: u64 =
        5 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const DIRECT_ROOTFS_PMEM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-read-flush=1";
    const DIRECT_ROOTFS_PMEM_READ_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_READ_FLUSH_OK";
    const DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER: &[u8] = b"BANGBANG_PMEM_ROOT_RO_OK";
    const DIRECT_ROOTFS_PMEM_ROOT_RW_MARKER: &[u8] = b"BANGBANG_PMEM_ROOT_RW_OK";
    const DIRECT_ROOTFS_PCI_ALL_VIRTIO_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.pci-all-virtio=1";
    const DIRECT_ROOTFS_PCI_ALL_VIRTIO_MARKER: &[u8] = b"BANGBANG_PCI_ALL_VIRTIO_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-hotplug=1";
    const DIRECT_ROOTFS_BLOCK_LIFECYCLE_THREE_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-backing-lifecycle=three bangbang.expect-block-limiter-patch=1";
    const DIRECT_ROOTFS_VHOST_BLOCK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.block-hotplug=1 bangbang.expect-vhost-resize=1";
    const DIRECT_ROOTFS_PMEM_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-hotplug=1";
    const DIRECT_ROOTFS_NETWORK_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.network-hotplug=1";
    const DIRECT_ROOTFS_STORAGE_CERTIFICATION_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 root=/dev/vda ro rootwait memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.storage-certification=1";
    const STORAGE_CONTROL_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTROL_HOST";
    const STORAGE_CONTROL_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_CONTROL_GUEST";
    const STORAGE_ASYNC_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_HOST";
    const STORAGE_ASYNC_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_GUEST";
    const STORAGE_ASYNC_REPLACEMENT_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_ASYNC_REPLACEMENT_HOST";
    const STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER: &[u8] =
        b"BANGBANG_STORAGE_ASYNC_REPLACEMENT_GUEST";
    const STORAGE_VHOST_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_VHOST_HOST";
    const STORAGE_VHOST_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_VHOST_GUEST";
    const STORAGE_PMEM_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_PMEM_HOST";
    const STORAGE_PMEM_GUEST_MARKER: &[u8] = b"BANGBANG_STORAGE_PMEM_GUEST";
    const STORAGE_RUNTIME_BLOCK_ONE_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_BLOCK_ONE_HOST";
    const STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER: &[u8] =
        b"BANGBANG_STORAGE_RUNTIME_BLOCK_ONE_GUEST";
    const STORAGE_RUNTIME_BLOCK_TWO_HOST_MARKER: &[u8] = b"BANGBANG_STORAGE_RUNTIME_BLOCK_TWO_HOST";
    const STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER: &[u8] =
        b"BANGBANG_STORAGE_RUNTIME_BLOCK_TWO_GUEST";
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
    const STORAGE_CONTROL_GUEST_OFFSET: u64 = 2 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_READY_OFFSET: u64 = bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_CONTINUE_ONE_OFFSET: u64 = 3 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_FIRST_REMOVED_OFFSET: u64 = 4 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_CONTINUE_TWO_OFFSET: u64 = 5 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_SUCCESS_OFFSET: u64 = 6 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_SECOND_BLOCK_REMOVED_OFFSET: u64 =
        7 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_CONTINUE_PMEM_ONE_OFFSET: u64 =
        8 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_FIRST_PMEM_REMOVED_OFFSET: u64 =
        9 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const STORAGE_CONTINUE_PMEM_TWO_OFFSET: u64 =
        10 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const BLOCK_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_READY";
    const BLOCK_HOTPLUG_HOST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_HOST_ONE";
    const BLOCK_HOTPLUG_GUEST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_GUEST_ONE";
    const BLOCK_HOTPLUG_FIRST_REMOVED_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_FIRST_REMOVED";
    const BLOCK_HOTPLUG_CONTINUE_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_CONTINUE";
    const BLOCK_HOTPLUG_HOST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_HOST_TWO";
    const BLOCK_HOTPLUG_GUEST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_GUEST_TWO";
    const BLOCK_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_SUCCESS";
    const BLOCK_HOTPLUG_FIRST_SERIAL_BEGIN_MARKER: &[u8] =
        b"BANGBANG_BLOCK_HOTPLUG_FIRST_SERIAL_BEGIN";
    const BLOCK_HOTPLUG_FIRST_SERIAL_END_MARKER: &[u8] = b"BANGBANG_BLOCK_HOTPLUG_FIRST_SERIAL_END";
    const BLOCK_HOTPLUG_SECOND_SERIAL_BEGIN_MARKER: &[u8] =
        b"BANGBANG_BLOCK_HOTPLUG_SECOND_SERIAL_BEGIN";
    const BLOCK_HOTPLUG_SECOND_SERIAL_END_MARKER: &[u8] =
        b"BANGBANG_BLOCK_HOTPLUG_SECOND_SERIAL_END";
    const BLOCK_LIFECYCLE_INITIAL_SERIAL_BEGIN_MARKER: &[u8] =
        b"BANGBANG_BLOCK_LIFECYCLE_INITIAL_SERIAL_BEGIN";
    const BLOCK_LIFECYCLE_INITIAL_SERIAL_END_MARKER: &[u8] =
        b"BANGBANG_BLOCK_LIFECYCLE_INITIAL_SERIAL_END";
    const BLOCK_LIFECYCLE_HOST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_HOST_ONE";
    const BLOCK_LIFECYCLE_GUEST_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_GUEST_ONE";
    const BLOCK_LIFECYCLE_PHASE_ONE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_PHASE_ONE";
    const BLOCK_LIFECYCLE_LIMITER_READY_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_LIMITER_READY";
    const BLOCK_LIFECYCLE_LIMITER_CONTINUE_MARKER: &[u8] =
        b"BANGBANG_BLOCK_LIFECYCLE_LIMITER_CONTINUE";
    const BLOCK_LIFECYCLE_HOST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_HOST_TWO";
    const BLOCK_LIFECYCLE_GUEST_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_GUEST_TWO";
    const BLOCK_LIFECYCLE_PHASE_TWO_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_PHASE_TWO";
    const BLOCK_LIFECYCLE_HOST_THREE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_HOST_THREE";
    const BLOCK_LIFECYCLE_GUEST_THREE_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_GUEST_THREE";
    const BLOCK_LIFECYCLE_READ_ONLY_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_READ_ONLY";
    const BLOCK_LIFECYCLE_SUCCESS_MARKER: &[u8] = b"BANGBANG_BLOCK_LIFECYCLE_SUCCESS";
    const BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET: u64 =
        2 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const VHOST_CONFIG_RESIZED_MARKER: &[u8] = b"BANGBANG_VHOST_CONFIG_RESIZED";
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
    const NETWORK_HOTPLUG_SECOND_CONTINUE_MARKER: &[u8] =
        b"BANGBANG_NETWORK_HOTPLUG_SECOND_CONTINUE";
    const NETWORK_HOTPLUG_SUCCESS_MARKER: &[u8] = b"BANGBANG_NETWORK_HOTPLUG_SUCCESS";
    const PMEM_HOST_MARKER: &[u8] = b"BANGBANG_PMEM_HOST_MARKER";
    const PMEM_GUEST_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_GUEST_FLUSH_OK";
    const PMEM_GUEST_FLUSH_OFFSET: u64 = 4096;
    const DIRECT_ROOTFS_MMDS_MARKER: &[u8] = b"BANGBANG_MMDS_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_MMDS_MTU_MARKER: &[u8] = b"BANGBANG_MMDS_MTU_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_MMDS_V2_MARKER: &[u8] = b"BANGBANG_MMDS_V2_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_MMDS_ETH0_MARKER: &[u8] = b"BANGBANG_MMDS_ETH0_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_MMDS_ETH0_FAILURE_MARKER: &[u8] = b"BANGBANG_MMDS_ETH0_FETCH_FAIL";
    const DIRECT_ROOTFS_MMDS_ETH1_MARKER: &[u8] = b"BANGBANG_MMDS_ETH1_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_MMDS_ETH1_FAILURE_MARKER: &[u8] = b"BANGBANG_MMDS_ETH1_FETCH_FAIL";
    const DIRECT_ROOTFS_MMDS_ETH0_MARKER_OFFSET: u64 = 0;
    const DIRECT_ROOTFS_MMDS_ETH1_MARKER_OFFSET: u64 =
        bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const CONCURRENT_MMDS_PROCESS_A_IFACE_ID: &str = "mmds_a";
    const CONCURRENT_MMDS_PROCESS_B_IFACE_ID: &str = "mmds_b";
    const CONCURRENT_MMDS_PROCESS_A_VALUE: &str = "BANGBANG_MMDS_PROCESS_A_VALUE";
    const CONCURRENT_MMDS_PROCESS_B_VALUE: &str = "BANGBANG_MMDS_PROCESS_B_VALUE";
    const CONCURRENT_MMDS_PROCESS_B_PENDING: &str = "BANGBANG_MMDS_PROCESS_B_PENDING";
    const CONCURRENT_MMDS_PROCESS_B_RELEASE: &str = "BANGBANG_MMDS_PROCESS_B_RELEASE";
    const CONCURRENT_MMDS_PROCESS_A_CONTENT: &str =
        r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_PROCESS_A_VALUE"}}"#;
    const CONCURRENT_MMDS_PROCESS_B_CONTENT: &str = r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_PROCESS_B_VALUE","bangbang-release":"BANGBANG_MMDS_PROCESS_B_PENDING"}}"#;
    const CONCURRENT_MMDS_PROCESS_B_RELEASE_PATCH: &str =
        r#"{"meta-data":{"bangbang-release":"BANGBANG_MMDS_PROCESS_B_RELEASE"}}"#;
    const CONCURRENT_MMDS_PROCESS_A_SUCCESS: &[u8] = b"BANGBANG_MMDS_PROCESS_A_FETCH_OK";
    const CONCURRENT_MMDS_PROCESS_A_FAILURE: &[u8] = b"BANGBANG_MMDS_PROCESS_A_FETCH_FAIL";
    const CONCURRENT_MMDS_PROCESS_B_READY: &[u8] = b"BANGBANG_MMDS_PROCESS_B_READY";
    const CONCURRENT_MMDS_PROCESS_B_READY_FAILURE: &[u8] = b"BANGBANG_MMDS_PROCESS_B_READY_FAIL";
    const CONCURRENT_MMDS_PROCESS_B_SUCCESS: &[u8] = b"BANGBANG_MMDS_PROCESS_B_FETCH_OK";
    const CONCURRENT_MMDS_PROCESS_B_FAILURE: &[u8] = b"BANGBANG_MMDS_PROCESS_B_FETCH_FAIL";
    const CONCURRENT_MMDS_PROCESS_B_TERMINAL_OFFSET: u64 =
        bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
    const DIRECT_ROOTFS_VSOCK_MARKER: &[u8] = b"BANGBANG_VSOCK_GUEST_CONNECT_OK";
    const DIRECT_ROOTFS_VSOCK_STREAM_BYTES: usize = 1024 * 1024;
    const DIRECT_ROOTFS_VSOCK_STREAM_CHUNK_BYTES: usize = 16 * 1024;
    const DIRECT_ROOTFS_VSOCK_GUEST_STREAM_SEED: u8 = 0x3d;
    const DIRECT_ROOTFS_VSOCK_HOST_STREAM_SEED: u8 = 0xa7;
    const DIRECT_ROOTFS_VSOCK_PORT: u32 = 5005;
    const DIRECT_ROOTFS_VSOCK_MULTISTREAM_MARKER: &[u8] = b"BANGBANG_VSOCK_GUEST_MULTISTREAM_OK";
    const DIRECT_ROOTFS_VSOCK_MULTISTREAM_EXCHANGES: &[(u32, &[u8], &[u8])] = &[
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
    const DIRECT_ROOTFS_HOST_VSOCK_READY_MARKER: &[u8] = b"BANGBANG_VSOCK_HOST_CONNECT_READY";
    const DIRECT_ROOTFS_HOST_VSOCK_MARKER: &[u8] = b"BANGBANG_VSOCK_HOST_CONNECT_OK";
    const DIRECT_ROOTFS_HOST_VSOCK_PORT: u32 = 5006;
    const DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_READY_MARKER: &[u8] =
        b"BANGBANG_VSOCK_HOST_MULTISTREAM_READY";
    const DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_MARKER: &[u8] =
        b"BANGBANG_VSOCK_HOST_MULTISTREAM_OK";
    const DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_EXCHANGES: &[(u32, &[u8], &[u8])] = &[
        (
            5009,
            b"BANGBANG_VSOCK_HOST_MULTI_GUEST_ONE",
            b"BANGBANG_VSOCK_HOST_MULTI_HOST_ONE",
        ),
        (
            5010,
            b"BANGBANG_VSOCK_HOST_MULTI_GUEST_TWO",
            b"BANGBANG_VSOCK_HOST_MULTI_HOST_TWO",
        ),
    ];
    const GUEST_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/init";
    const GUEST_SMP_PROGRESS_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 rdinit=/smp-progress-init";
    const GUEST_SMP_HOTPLUG_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 rdinit=/smp-hotplug-init";
    const GUEST_POWEROFF_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init";
    const GUEST_RESET_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/reboot-init";
    const GUEST_SERIAL_RX_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/serial-rx-init";
    const GUEST_SERIAL_RX_READY_MARKER: &str = "BANGBANG_SERIAL_RX_READY";
    const GUEST_SERIAL_RX_SUCCESS_MARKER: &str = "BANGBANG_SERIAL_RX_OK";
    const GUEST_SERIAL_RX_FAILURE_MARKER: &str = "BANGBANG_SERIAL_RX_FAIL";
    const SMP_PROGRESS_CPU0_TOKEN: u8 = 0xa5;
    const SMP_PROGRESS_CPU1_TOKEN: u8 = 0xd3;
    const SMP_HOTPLUG_READY_MARKER: &[u8] = b"BBHOTREADY";
    const SMP_HOTPLUG_OFF_MARKER: &[u8] = b"BBHOTOFF";
    const SMP_HOTPLUG_DONE_MARKER: &[u8] = b"BBHOTDONE";
    const DIRECT_ROOTFS_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init";
    const ROOTFS_BOOT_TIMER_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 nomodule swiotlb=noforce init=/usr/local/bin/init";
    const DIRECT_ROOTFS_ENTROPY_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.entropy-read=1";
    const DIRECT_ROOTFS_MMDS_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-fetch=1";
    const DIRECT_ROOTFS_MMDS_MTU_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-fetch=1 bangbang.mmds-mtu=1280";
    const DIRECT_ROOTFS_MMDS_V2_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-v2-fetch=1";
    const DIRECT_ROOTFS_MMDS_MULTI_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-multi-fetch=1";
    const CONCURRENT_MMDS_PROCESS_A_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-process-a-fetch=1";
    const CONCURRENT_MMDS_PROCESS_B_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-process-b-fetch=1";
    const DIRECT_ROOTFS_MMDS_CONTENT: &str =
        r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_GUEST_VALUE"}}"#;
    const DIRECT_ROOTFS_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-connect=1";
    const DIRECT_ROOTFS_VSOCK_MULTISTREAM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-multistream=1";
    const DIRECT_ROOTFS_HOST_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-connect=1";
    const DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-multistream=1";
    const GUEST_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);
    const PCI_ALL_VIRTIO_GUEST_TIMEOUT: Duration = Duration::from_secs(90);
    const SNAPSHOT_GUEST_IMAGE_HEADER_SIZE: usize = 64;
    const SNAPSHOT_GUEST_IMAGE_MAGIC: u32 = 0x644d_5241;
    const SNAPSHOT_GUEST_RTC_ADDRESS: u64 = 0x4000_1000;
    const SNAPSHOT_GUEST_UART_ADDRESS: u64 = 0x4000_2000;
    const SNAPSHOT_GUEST_VMCLOCK_ADDRESS: u64 = bangbang_runtime::memory::aarch64::SYSTEM_MEM_START
        + bangbang_runtime::memory::aarch64::SYSTEM_MEM_SIZE
        - bangbang_runtime::fdt::ARM64_FDT_VMCLOCK_SIZE;
    const SNAPSHOT_GUEST_VMGENID_ADDRESS: u64 = bangbang_runtime::memory::aarch64::SYSTEM_MEM_START
        + bangbang_runtime::memory::aarch64::SYSTEM_MEM_SIZE
        - bangbang_runtime::fdt::ARM64_FDT_VMCLOCK_SIZE
        - bangbang_runtime::fdt::ARM64_FDT_VMGENID_SIZE;

    #[derive(Clone, Copy)]
    enum DirectRootfsMmdsContentSource {
        ApiRequest,
        MetadataFile,
    }

    #[derive(Clone, Copy)]
    struct DirectRootfsMmdsFetchCase<'a> {
        request_context: &'a str,
        mmds_config_body: &'a str,
        boot_args: &'a str,
        success_marker: &'a [u8],
        network_mtu: Option<u16>,
        initial_rx_rate_limiter: Option<&'a str>,
        wait_for_guest_completion_before_network_patch: bool,
        content_source: DirectRootfsMmdsContentSource,
    }

    #[derive(Clone, Copy)]
    struct DirectRootfsNoApiMmdsFetchCase<'a> {
        request_context: &'a str,
        mmds_config_body: &'a str,
        boot_args: &'a str,
        success_marker: &'a [u8],
    }

    #[derive(Clone, Copy)]
    struct VhostUserBlockGuestCase {
        mode: &'static str,
        root_read_only: bool,
        enable_pci: bool,
        partitioned_root: bool,
        retry_rejected_discovery: bool,
        refresh_scratch_config: bool,
        memory_hotplug: bool,
        success_marker: &'static [u8],
    }

    #[derive(Clone, Copy)]
    struct ConcurrentMmdsGuestConfig<'a> {
        iface_id: &'a str,
        guest_mac: &'a str,
        mmds_content: &'a str,
        boot_args: &'a str,
        scratch_path: &'a Path,
        metrics_path: &'a Path,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SmpProgressCounts {
        cpu0: usize,
        cpu1: usize,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BlockMarkerState {
        Pending,
        Success,
        Failure,
    }

    #[test]
    fn signed_executable_runs_async_block_over_mmio_with_live_patch() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let backing_path = test_dir.path().join("data.img");
        let scratch_backing_path = test_dir.path().join("scratch.img");
        let replacement_scratch_backing_path = test_dir.path().join("scratch-replacement.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let metrics_path = test_dir.path().join("metrics.out");
        let logger_path = test_dir.path().join("logger.out");
        let uds_path = test_dir.path().join("v.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();
        let future_start_time = u64::MAX.to_string();

        create_zeroed_block_backing(&backing_path);
        create_zeroed_block_backing(&scratch_backing_path);
        create_zeroed_block_backing(&replacement_scratch_backing_path);
        create_empty_file(&serial_output_path);

        let mut bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &[
                "--start-time-us",
                future_start_time.as_str(),
                "--start-time-cpu-us",
                future_start_time.as_str(),
                "--parent-cpu-time-us",
                "3000",
            ],
        );

        let instance_info = http_get(&socket_path, "/");
        assert_ok_response(&instance_info, "GET / before InstanceStart");

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let initrd_path_json = json_string(path_text(&initrd_path));
        let boot_args_json = json_string(GUEST_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "initrd_path":{initrd_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source");

        let backing_path_json = json_string(path_text(&backing_path));
        let drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{backing_path_json},
                "is_root_device":false,
                "is_read_only":false,
                "io_engine":"Async"
            }}"#
        );
        let drive_response = http_put_json(&socket_path, "/drives/data", &drive_body);
        assert_no_content_response(&drive_response, "PUT /drives/data");

        let scratch_backing_path_json = json_string(path_text(&scratch_backing_path));
        let scratch_drive_body = format!(
            r#"{{
                "drive_id":"scratch",
                "path_on_host":{scratch_backing_path_json},
                "is_root_device":false,
                "is_read_only":false,
                "io_engine":"Async"
            }}"#
        );
        let scratch_drive_response =
            http_put_json(&socket_path, "/drives/scratch", &scratch_drive_body);
        assert_no_content_response(&scratch_drive_response, "PUT /drives/scratch");

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(&serial_response, "PUT /serial");

        let metrics_path_json = json_string(path_text(&metrics_path));
        let metrics_body = format!(r#"{{"metrics_path":{metrics_path_json}}}"#);
        let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
        assert_no_content_response(&metrics_response, "PUT /metrics");

        let logger_path_json = json_string(path_text(&logger_path));
        let logger_body = format!(
            r#"{{
                "log_path":{logger_path_json},
                "level":"Info",
                "show_level":true,
                "show_log_origin":true
            }}"#
        );
        let logger_response = http_put_json(&socket_path, "/logger", &logger_body);
        assert_no_content_response(&logger_response, "PUT /logger");

        let uds_path_json = json_string(path_text(&uds_path));
        let vsock_body = format!(r#"{{"guest_cid":3,"uds_path":{uds_path_json}}}"#);
        let vsock_response = http_put_json(&socket_path, "/vsock", &vsock_body);
        assert_no_content_response(&vsock_response, "PUT /vsock");
        assert!(
            !uds_path.exists(),
            "PUT /vsock should store config without binding the host socket path"
        );

        let preboot_mmds_data = http_get(&socket_path, "/mmds");
        assert_ok_response(&preboot_mmds_data, "GET /mmds before InstanceStart");
        assert_response_contains(
            &preboot_mmds_data,
            "\r\n\r\nnull",
            "GET /mmds before InstanceStart",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart");

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(&running_instance_info, "GET / after InstanceStart");
        assert_response_contains(
            &running_instance_info,
            &format!(r#""id":"{instance_id}""#),
            "GET / after InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after InstanceStart",
        );

        let pause_response = http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&pause_response, "PATCH /vm Paused after InstanceStart");
        let paused_instance_info = http_get(&socket_path, "/");
        assert_ok_response(&paused_instance_info, "GET / after PATCH /vm Paused");
        assert_response_contains(
            &paused_instance_info,
            r#""state":"Paused""#,
            "GET / after PATCH /vm Paused",
        );

        let duplicate_pause_response =
            http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&duplicate_pause_response, "PATCH /vm Paused while paused");
        let paused_after_duplicate_pause = http_get(&socket_path, "/");
        assert_ok_response(
            &paused_after_duplicate_pause,
            "GET / after idempotent duplicate PATCH /vm Paused",
        );
        assert_response_contains(
            &paused_after_duplicate_pause,
            r#""state":"Paused""#,
            "GET / after idempotent duplicate PATCH /vm Paused",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused MMIO Async storage preflight",
        );

        let resume_response = http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(&resume_response, "PATCH /vm Resumed after pause");
        let resumed_instance_info = http_get(&socket_path, "/");
        assert_ok_response(&resumed_instance_info, "GET / after PATCH /vm Resumed");
        assert_response_contains(
            &resumed_instance_info,
            r#""state":"Running""#,
            "GET / after PATCH /vm Resumed",
        );

        let duplicate_resume_response =
            http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(
            &duplicate_resume_response,
            "PATCH /vm Resumed while running",
        );
        let running_after_duplicate_resume = http_get(&socket_path, "/");
        assert_ok_response(
            &running_after_duplicate_resume,
            "GET / after idempotent duplicate PATCH /vm Resumed",
        );
        assert_response_contains(
            &running_after_duplicate_resume,
            r#""state":"Running""#,
            "GET / after idempotent duplicate PATCH /vm Resumed",
        );

        for (request_context, path, expected_fault, private_id) in [
            (
                "DELETE /drives/private_hot_unplug_drive after InstanceStart",
                "/drives/private_hot_unplug_drive",
                r#"{"fault_message":"runtime drive insertion and removal require PCI transport"}"#,
                "private_hot_unplug_drive",
            ),
            (
                "DELETE /network-interfaces/private_hot_unplug_iface after InstanceStart",
                "/network-interfaces/private_hot_unplug_iface",
                r#"{"fault_message":"runtime network insertion and removal require PCI transport"}"#,
                "private_hot_unplug_iface",
            ),
            (
                "DELETE /pmem/private_hot_unplug_pmem after InstanceStart",
                "/pmem/private_hot_unplug_pmem",
                r#"{"fault_message":"runtime pmem insertion and removal require PCI transport"}"#,
                "private_hot_unplug_pmem",
            ),
        ] {
            let hot_unplug_response = http_no_body(&socket_path, "DELETE", path);
            assert_bad_request_response(&hot_unplug_response, request_context);
            assert_response_contains(&hot_unplug_response, expected_fault, request_context);
            assert!(
                !hot_unplug_response.contains(private_id),
                "{request_context} response must not echo private hot-unplug id {private_id:?}; response:\n{hot_unplug_response}"
            );
        }
        let running_after_hot_unplug = http_get(&socket_path, "/");
        assert_ok_response(
            &running_after_hot_unplug,
            "GET / after rejected hot-unplug requests",
        );
        assert_response_contains(
            &running_after_hot_unplug,
            r#""state":"Running""#,
            "GET / after rejected hot-unplug requests",
        );

        let replacement_kernel_path = test_dir.path().join("replacement-vmlinux");
        let replacement_kernel_path_json = json_string(path_text(&replacement_kernel_path));
        let boot_update_body = format!(r#"{{"kernel_image_path":{replacement_kernel_path_json}}}"#);
        let boot_update_response = http_put_json(&socket_path, "/boot-source", &boot_update_body);
        assert_bad_request_response(
            &boot_update_response,
            "PUT /boot-source after InstanceStart",
        );
        assert_response_contains(
            &boot_update_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutBootSource"}"#,
            "PUT /boot-source after InstanceStart",
        );
        assert!(
            !replacement_kernel_path.exists(),
            "rejected boot-source update must not create or use replacement kernel path {}",
            replacement_kernel_path.display()
        );

        let cpu_config_response = http_put_json(&socket_path, "/cpu-config", "{}");
        assert_bad_request_response(&cpu_config_response, "PUT /cpu-config after InstanceStart");
        assert_response_contains(
            &cpu_config_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutCpuConfig"}"#,
            "PUT /cpu-config after InstanceStart",
        );
        let instance_info_after_cpu_config = http_get(&socket_path, "/");
        assert_ok_response(
            &instance_info_after_cpu_config,
            "GET / after rejected PUT /cpu-config",
        );
        assert_response_contains(
            &instance_info_after_cpu_config,
            r#""state":"Running""#,
            "GET / after rejected PUT /cpu-config",
        );

        let replacement_put_backing_path = test_dir.path().join("replacement-put-data.img");
        let replacement_put_backing_path_json =
            json_string(path_text(&replacement_put_backing_path));
        let drive_put_body = format!(
            r#"{{
                "drive_id":"replacement",
                "path_on_host":{replacement_put_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let drive_put_response =
            http_put_json(&socket_path, "/drives/replacement", &drive_put_body);
        assert_bad_request_response(
            &drive_put_response,
            "PUT /drives/replacement after InstanceStart",
        );
        assert_response_contains(
            &drive_put_response,
            r#"{"fault_message":"runtime drive insertion and removal require PCI transport"}"#,
            "PUT /drives/replacement after InstanceStart",
        );
        assert!(
            !replacement_put_backing_path.exists(),
            "rejected drive PUT must not create or use replacement backing path {}",
            replacement_put_backing_path.display()
        );

        let replacement_scratch_backing_path_json =
            json_string(path_text(&replacement_scratch_backing_path));
        let drive_update_body = format!(
            r#"{{
                "drive_id":"scratch",
                "path_on_host":{replacement_scratch_backing_path_json}
            }}"#
        );
        let drive_update_response =
            http_json(&socket_path, "PATCH", "/drives/scratch", &drive_update_body);
        assert_no_content_response(
            &drive_update_response,
            "PATCH /drives/scratch backing after InstanceStart",
        );

        let drive_rate_limiter_update_response = http_json(
            &socket_path,
            "PATCH",
            "/drives/data",
            r#"{"drive_id":"data","rate_limiter":{"bandwidth":{"size":1000,"one_time_burst":1000,"refill_time":100}}}"#,
        );
        assert_no_content_response(
            &drive_rate_limiter_update_response,
            "PATCH /drives/data rate_limiter after InstanceStart",
        );

        let replacement_serial_output_path = test_dir.path().join("replacement-serial.out");
        let replacement_serial_output_path_json =
            json_string(path_text(&replacement_serial_output_path));
        let serial_update_body =
            format!(r#"{{"serial_out_path":{replacement_serial_output_path_json}}}"#);
        let serial_update_response = http_put_json(&socket_path, "/serial", &serial_update_body);
        assert_bad_request_response(&serial_update_response, "PUT /serial after InstanceStart");
        assert_response_contains(
            &serial_update_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutSerial"}"#,
            "PUT /serial after InstanceStart",
        );
        assert!(
            !replacement_serial_output_path.exists(),
            "rejected serial update must not create or use replacement output path {}",
            replacement_serial_output_path.display()
        );

        let replacement_metrics_path = test_dir.path().join("replacement-metrics.out");
        let replacement_metrics_path_json = json_string(path_text(&replacement_metrics_path));
        let metrics_update_body = format!(r#"{{"metrics_path":{replacement_metrics_path_json}}}"#);
        let metrics_update_response = http_put_json(&socket_path, "/metrics", &metrics_update_body);
        assert_bad_request_response(&metrics_update_response, "PUT /metrics after InstanceStart");
        assert_response_contains(
            &metrics_update_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutMetrics"}"#,
            "PUT /metrics after InstanceStart",
        );
        assert!(
            !replacement_metrics_path.exists(),
            "rejected metrics update must not create or use replacement output path {}",
            replacement_metrics_path.display()
        );

        let replacement_logger_path = test_dir.path().join("replacement-logger.out");
        let replacement_logger_path_json = json_string(path_text(&replacement_logger_path));
        let logger_update_body = format!(r#"{{"log_path":{replacement_logger_path_json}}}"#);
        let logger_update_response = http_put_json(&socket_path, "/logger", &logger_update_body);
        assert_bad_request_response(&logger_update_response, "PUT /logger after InstanceStart");
        assert_response_contains(
            &logger_update_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutLogger"}"#,
            "PUT /logger after InstanceStart",
        );
        assert!(
            !replacement_logger_path.exists(),
            "rejected logger update must not create or use replacement output path {}",
            replacement_logger_path.display()
        );

        let replacement_vsock_path = test_dir.path().join("rv.sock");
        let replacement_vsock_path_json = json_string(path_text(&replacement_vsock_path));
        let vsock_update_body =
            format!(r#"{{"guest_cid":4,"uds_path":{replacement_vsock_path_json}}}"#);
        let vsock_update_response = http_put_json(&socket_path, "/vsock", &vsock_update_body);
        assert_bad_request_response(&vsock_update_response, "PUT /vsock after InstanceStart");
        assert_response_contains(
            &vsock_update_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutVsock"}"#,
            "PUT /vsock after InstanceStart",
        );
        assert!(
            !replacement_vsock_path.exists(),
            "rejected vsock update must not create or bind replacement socket path {}",
            replacement_vsock_path.display()
        );

        let network_update_body = r#"{
            "iface_id":"eth0",
            "host_dev_name":"vmnet:shared",
            "guest_mac":"12:34:56:78:9a:bc",
            "mtu":1500
        }"#;
        let network_update_response = http_put_json(
            &socket_path,
            "/network-interfaces/eth0",
            network_update_body,
        );
        assert_bad_request_response(
            &network_update_response,
            "PUT /network-interfaces/eth0 after InstanceStart",
        );
        assert_response_contains(
            &network_update_response,
            r#"{"fault_message":"runtime network insertion and removal require PCI transport"}"#,
            "PUT /network-interfaces/eth0 after InstanceStart",
        );

        let mmds_config_update_body = r#"{
            "network_interfaces":["eth0"],
            "version":"V2",
            "ipv4_address":"169.254.169.254",
            "imds_compat":true
        }"#;
        let mmds_config_update_response =
            http_put_json(&socket_path, "/mmds/config", mmds_config_update_body);
        assert_bad_request_response(
            &mmds_config_update_response,
            "PUT /mmds/config after InstanceStart",
        );
        assert_response_contains(
            &mmds_config_update_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutMmdsConfig"}"#,
            "PUT /mmds/config after InstanceStart",
        );

        let put_mmds_response = http_put_json(
            &socket_path,
            "/mmds",
            r#"{"latest":{"meta-data":{"ami-id":"ami-bangbang","remove-me":"yes"},"user-data":"before"}}"#,
        );
        assert_no_content_response(&put_mmds_response, "PUT /mmds after InstanceStart");
        let patch_mmds_response = http_json(
            &socket_path,
            "PATCH",
            "/mmds",
            r#"{"latest":{"dynamic":{"instance-identity":"document"},"meta-data":{"ami-id":"ami-updated","remove-me":null}}}"#,
        );
        assert_no_content_response(&patch_mmds_response, "PATCH /mmds after InstanceStart");
        let mmds_data = http_get(&socket_path, "/mmds");
        assert_ok_response(&mmds_data, "GET /mmds after runtime patch");
        assert_response_contains(
            &mmds_data,
            r#""ami-id":"ami-updated""#,
            "GET /mmds after runtime patch",
        );
        assert_response_contains(
            &mmds_data,
            r#""user-data":"before""#,
            "GET /mmds after runtime patch",
        );
        assert_response_contains(
            &mmds_data,
            r#""instance-identity":"document""#,
            "GET /mmds after runtime patch",
        );
        assert!(
            !mmds_data.contains("remove-me"),
            "PATCH /mmds should remove null-valued fields; response:\n{mmds_data}"
        );

        for (request_name, response, fault_message) in [
            (
                "GET /balloon",
                http_get(&socket_path, "/balloon"),
                "Balloon device is not supported.",
            ),
            (
                "GET /balloon/statistics",
                http_get(&socket_path, "/balloon/statistics"),
                "Balloon device is not supported.",
            ),
            (
                "GET /balloon/hinting/status",
                http_get(&socket_path, "/balloon/hinting/status"),
                "Balloon device is not supported.",
            ),
            (
                "PUT /balloon",
                http_put_json(
                    &socket_path,
                    "/balloon",
                    r#"{"amount_mib":64,"deflate_on_oom":true}"#,
                ),
                "The requested operation is not supported in Running state: PutBalloon",
            ),
            (
                "PATCH /balloon",
                http_json(&socket_path, "PATCH", "/balloon", r#"{"amount_mib":32}"#),
                "Balloon device is not supported.",
            ),
            (
                "PATCH /balloon/statistics",
                http_json(
                    &socket_path,
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":1}"#,
                ),
                "Balloon device is not supported.",
            ),
            (
                "PATCH /balloon/hinting/start",
                http_json(
                    &socket_path,
                    "PATCH",
                    "/balloon/hinting/start",
                    r#"{"acknowledge_on_stop":false}"#,
                ),
                "Balloon device is not supported.",
            ),
            (
                "PATCH /balloon/hinting/stop",
                http_no_body(&socket_path, "PATCH", "/balloon/hinting/stop"),
                "Balloon device is not supported.",
            ),
        ] {
            assert_bad_request_response(&response, request_name);
            assert_response_contains(
                &response,
                &format!(r#"{{"fault_message":"{fault_message}"}}"#),
                request_name,
            );
        }

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after InstanceStart");
        assert_response_contains(
            &vm_config,
            &format!(r#""kernel_image_path":{kernel_path_json}"#),
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""initrd_path":{initrd_path_json}"#),
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""boot_args":{boot_args_json}"#),
            "GET /vm/config after InstanceStart",
        );
        assert!(
            !vm_config.contains(&format!(
                r#""kernel_image_path":{replacement_kernel_path_json}"#
            )),
            "rejected boot-source update must not mutate the configured kernel path; response:\n{vm_config}"
        );
        assert_response_contains(
            &vm_config,
            r#""vcpu_count":1"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""mem_size_mib":256"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""drive_id":"data""#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{backing_path_json}"#),
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""rate_limiter":{"bandwidth":"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""size":1000"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""one_time_burst":1000"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""refill_time":100"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            r#""drive_id":"scratch""#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{replacement_scratch_backing_path_json}"#),
            "GET /vm/config after InstanceStart",
        );
        assert_eq!(
            vm_config.matches(r#""drive_id":"#).count(),
            2,
            "drive update must keep only the configured data and scratch drives; response:\n{vm_config}"
        );
        assert_eq!(
            vm_config.matches(r#""io_engine":"Async""#).count(),
            2,
            "both live MMIO drives must retain Async through PATCH; response:\n{vm_config}"
        );
        assert!(
            !vm_config.contains(r#""drive_id":"replacement""#),
            "rejected drive PUT must not add replacement drive; response:\n{vm_config}"
        );
        assert!(
            !vm_config.contains(&format!(
                r#""path_on_host":{replacement_put_backing_path_json}"#
            )),
            "rejected drive PUT must not store replacement backing path; response:\n{vm_config}"
        );
        assert_response_contains(
            &vm_config,
            r#""guest_cid":3"#,
            "GET /vm/config after InstanceStart",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""uds_path":{uds_path_json}"#),
            "GET /vm/config after InstanceStart",
        );
        assert!(
            !vm_config.contains(&format!(r#""uds_path":{replacement_vsock_path_json}"#)),
            "rejected vsock update must not mutate the configured socket path; response:\n{vm_config}"
        );
        assert_response_contains(
            &vm_config,
            r#""network-interfaces":[]"#,
            "GET /vm/config after InstanceStart",
        );
        assert!(
            !vm_config.contains(r#""iface_id":"eth0""#),
            "rejected network update must not add an interface; response:\n{vm_config}"
        );
        assert!(
            !vm_config.contains(r#""mmds-config":"#),
            "rejected MMDS config update must not add MMDS config; response:\n{vm_config}"
        );
        assert!(
            !vm_config.contains(r#""network_interfaces":["eth0"]"#),
            "rejected MMDS config update must not store interface bindings; response:\n{vm_config}"
        );
        UnixStream::connect(&uds_path).unwrap_or_else(|err| {
            panic!(
                "InstanceStart should bind the configured vsock listener at {}: {err}",
                uds_path.display()
            )
        });

        bangbang.send_signal(libc::SIGPIPE, "SIGPIPE");
        let instance_info_after_sigpipe = http_get(&socket_path, "/");
        assert_ok_response(&instance_info_after_sigpipe, "GET / after SIGPIPE");

        let flush_metrics_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(&flush_metrics_response, "PUT /actions FlushMetrics");
        assert_metrics_output(
            &metrics_path,
            Some(
                r#"{"balloon_count":3,"hotplug_memory_count":0,"instance_info_count":9,"machine_cfg_count":0,"mmds_count":2,"vmm_version_count":0}"#,
            ),
            r#"{"actions_count":2,"actions_fails":0,"balloon_count":1,"balloon_fails":1,"boot_source_count":2,"boot_source_fails":1,"cpu_cfg_count":1,"cpu_cfg_fails":1,"drive_count":3,"drive_fails":1,"hotplug_memory_count":0,"hotplug_memory_fails":0,"logger_count":2,"logger_fails":1,"machine_cfg_count":1,"machine_cfg_fails":0,"metrics_count":2,"metrics_fails":1,"mmds_count":2,"mmds_fails":1,"network_count":1,"network_fails":1,"pmem_count":0,"pmem_fails":0,"serial_count":2,"serial_fails":1,"vsock_count":2,"vsock_fails":1}"#,
            Some(
                r#"{"balloon_count":4,"balloon_fails":4,"drive_count":2,"drive_fails":0,"hotplug_memory_count":0,"hotplug_memory_fails":0,"machine_cfg_count":0,"machine_cfg_fails":0,"mmds_count":1,"mmds_fails":0,"network_count":0,"network_fails":0,"pmem_count":0,"pmem_fails":0}"#,
            ),
        );
        assert_vm_state_latency_metrics_output(&metrics_path);
        assert_block_update_metrics_output(&metrics_path);
        assert_startup_time_metrics_output(&metrics_path);
        assert_sigpipe_signal_metrics_output(&metrics_path);
        assert!(
            !replacement_metrics_path.exists(),
            "rejected metrics update must not write later metrics output to replacement path {}",
            replacement_metrics_path.display()
        );
        assert_logger_output(&logger_path, LoggerPrefixExpectation::LevelOrigin);
        assert!(
            !replacement_logger_path.exists(),
            "rejected logger update must not write later action records to replacement output path {}",
            replacement_logger_path.display()
        );

        let second_start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_bad_request_response(&second_start_response, "PUT /actions second InstanceStart");
        assert_response_contains(
            &second_start_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: InstanceStart"}"#,
            "PUT /actions second InstanceStart",
        );

        let post_start_machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_bad_request_response(
            &post_start_machine_response,
            "PUT /machine-config after InstanceStart",
        );
        assert_response_contains(
            &post_start_machine_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutMachineConfig"}"#,
            "PUT /machine-config after InstanceStart",
        );
        let post_start_machine_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/machine-config",
            r#"{"mem_size_mib":512}"#,
        );
        assert_bad_request_response(
            &post_start_machine_patch_response,
            "PATCH /machine-config after InstanceStart",
        );
        assert_response_contains(
            &post_start_machine_patch_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PatchMachineConfig"}"#,
            "PATCH /machine-config after InstanceStart",
        );
        let machine_config_after_patch = http_get(&socket_path, "/machine-config");
        assert_ok_response(
            &machine_config_after_patch,
            "GET /machine-config after rejected PATCH /machine-config",
        );
        assert_response_contains(
            &machine_config_after_patch,
            r#""vcpu_count":1"#,
            "GET /machine-config after rejected PATCH /machine-config",
        );
        assert_response_contains(
            &machine_config_after_patch,
            r#""mem_size_mib":256"#,
            "GET /machine-config after rejected PATCH /machine-config",
        );

        if let Err(err) =
            wait_for_file_prefix_marker(&backing_path, BLOCK_WRITE_MARKER, GUEST_EXECUTION_TIMEOUT)
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_WRITE_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest serial output file did not contain block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let output = bangbang.terminate();
        assert_clean_shutdown(output, &socket_path, "bangbang");
        assert_normal_terminal_metrics_output(&metrics_path);
        assert!(
            !uds_path.exists(),
            "bangbang shutdown should remove its owned vsock listener path"
        );
        assert!(
            !replacement_vsock_path.exists(),
            "rejected vsock update must not leave replacement socket path {}",
            replacement_vsock_path.display()
        );
    }

    #[test]
    fn signed_executable_runs_and_pauses_two_isolated_public_smp_guests() {
        let test_dir = TestDir::new();
        let socket_a = test_dir.path().join("smp-a.socket");
        let socket_b = test_dir.path().join("smp-b.socket");
        let serial_a = test_dir.path().join("smp-a.serial");
        let serial_b = test_dir.path().join("smp-b.serial");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let base_instance_id = test_dir.instance_id();
        let instance_a = format!("{base_instance_id}-smp-a");
        let instance_b = format!("{base_instance_id}-smp-b");

        create_empty_file(&serial_a);
        create_empty_file(&serial_b);
        let mut process_a = BangbangProcess::start(&socket_a, &instance_a);
        let mut process_b = BangbangProcess::start(&socket_b, &instance_b);
        configure_public_smp_progress(
            &socket_a,
            &kernel_path,
            &initrd_path,
            &serial_a,
            "process A",
        );
        configure_public_smp_progress(
            &socket_b,
            &kernel_path,
            &initrd_path,
            &serial_b,
            "process B",
        );

        let initial_target = SmpProgressCounts { cpu0: 2, cpu1: 2 };
        let _initial_a = wait_for_smp_progress_or_collect(
            &serial_a,
            initial_target,
            &mut process_a,
            &mut process_b,
            "initial process A progress",
        );
        let _initial_b = wait_for_smp_progress_or_collect(
            &serial_b,
            initial_target,
            &mut process_a,
            &mut process_b,
            "initial process B progress",
        );

        for (socket, context) in [(&socket_a, "process A"), (&socket_b, "process B")] {
            let state = http_get(socket, "/");
            assert_ok_response(&state, &format!("GET / for {context}"));
            assert_response_contains(
                &state,
                r#""state":"Running""#,
                &format!("GET / for {context}"),
            );
            let machine = http_get(socket, "/machine-config");
            assert_response_contains(
                &machine,
                r#""vcpu_count":2"#,
                &format!("GET /machine-config for {context}"),
            );
            let repeated_start =
                http_put_json(socket, "/actions", r#"{"action_type":"InstanceStart"}"#);
            assert_bad_request_response(
                &repeated_start,
                &format!("repeated InstanceStart for {context}"),
            );
            assert_response_contains(
                &repeated_start,
                r#"{"fault_message":"The requested operation is not supported in Running state: InstanceStart"}"#,
                &format!("repeated InstanceStart for {context}"),
            );
        }

        let pause = http_json(&socket_a, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&pause, "PATCH process A /vm Paused");
        let duplicate_pause = http_json(&socket_a, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&duplicate_pause, "duplicate process A pause");
        let paused_state = http_get(&socket_a, "/");
        assert_response_contains(
            &paused_state,
            r#""state":"Paused""#,
            "GET process A after pause",
        );
        let peer_state = http_get(&socket_b, "/");
        assert_response_contains(
            &peer_state,
            r#""state":"Running""#,
            "GET process B while process A is paused",
        );

        let paused_bytes = fs::read(&serial_a).expect("paused process A serial should read");
        let peer_before = smp_progress_counts(&serial_b)
            .expect("running process B progress should remain readable");
        let peer_target = SmpProgressCounts {
            cpu0: peer_before.cpu0 + 2,
            cpu1: peer_before.cpu1 + 2,
        };
        let _peer_after = wait_for_smp_progress_or_collect(
            &serial_b,
            peer_target,
            &mut process_a,
            &mut process_b,
            "process B progress while process A is paused",
        );
        assert_eq!(
            fs::read(&serial_a).expect("paused process A serial should remain readable"),
            paused_bytes,
            "process A serial bytes must remain unchanged while both process B vCPUs progress"
        );

        let paused_counts = smp_progress_counts(&serial_a)
            .expect("paused process A progress should remain readable");
        let resume = http_json(&socket_a, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(&resume, "PATCH process A /vm Resumed");
        let duplicate_resume = http_json(&socket_a, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(&duplicate_resume, "duplicate process A resume");
        let resumed_target = SmpProgressCounts {
            cpu0: paused_counts.cpu0 + 2,
            cpu1: paused_counts.cpu1 + 2,
        };
        let _resumed_counts = wait_for_smp_progress_or_collect(
            &serial_a,
            resumed_target,
            &mut process_a,
            &mut process_b,
            "resumed process A progress",
        );

        let output_a = process_a.interrupt();
        assert!(
            !output_a.stdout.contains(path_text(&serial_b))
                && !output_a.stderr.contains(path_text(&serial_b)),
            "process A diagnostics must not expose process B serial path; stdout:\n{}\nstderr:\n{}",
            output_a.stdout,
            output_a.stderr
        );
        assert_clean_shutdown(output_a, &socket_a, "public SMP process A after SIGINT");

        let output_b = process_b.terminate();
        assert!(
            !output_b.stdout.contains(path_text(&serial_a))
                && !output_b.stderr.contains(path_text(&serial_a)),
            "process B diagnostics must not expose process A serial path; stdout:\n{}\nstderr:\n{}",
            output_b.stdout,
            output_b.stderr
        );
        assert_clean_shutdown(output_b, &socket_b, "public SMP process B after SIGTERM");
    }

    #[test]
    fn signed_executable_reenters_a_guest_hotplugged_secondary_cpu() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("smp-hotplug.socket");
        let serial_path = test_dir.path().join("smp-hotplug.serial");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = format!("{}-smp-hotplug", test_dir.instance_id());

        create_empty_file(&serial_path);
        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);
        configure_public_smp_hotplug(&socket_path, &kernel_path, &initrd_path, &serial_path);
        if let Err(err) = wait_for_file_contains_marker(
            &serial_path,
            SMP_HOTPLUG_DONE_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "public SMP hotplug guest did not re-enter CPU1: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}\nserial:\n{}",
                output.status,
                output.stdout,
                output.stderr,
                String::from_utf8_lossy(&fs::read(&serial_path).unwrap_or_default())
            );
        }
        let serial = fs::read(&serial_path).expect("SMP hotplug serial should read");
        for marker in [
            SMP_HOTPLUG_READY_MARKER,
            SMP_HOTPLUG_OFF_MARKER,
            SMP_HOTPLUG_DONE_MARKER,
        ] {
            assert!(
                serial.windows(marker.len()).any(|window| window == marker),
                "SMP hotplug serial should contain {:?}: {}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&serial)
            );
        }

        let output = bangbang
            .wait_for_exit_with_timeout(GUEST_EXECUTION_TIMEOUT, "public SMP hotplug shutdown");
        assert!(
            output.status.success(),
            "hotplug guest SYSTEM_OFF should exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            !socket_path.exists(),
            "hotplug guest shutdown should clean up the API socket"
        );
    }

    #[test]
    fn signed_executable_starts_async_block_from_config_file() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let backing_path = test_dir.path().join("data.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let metrics_path = test_dir.path().join("metrics.out");
        let logger_path = test_dir.path().join("logger.out");
        let uds_path = test_dir.path().join("cf-v.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&backing_path);
        create_empty_file(&serial_output_path);

        let kernel_path_json = json_string(path_text(&kernel_path));
        let initrd_path_json = json_string(path_text(&initrd_path));
        let boot_args_json = json_string(GUEST_BOOT_ARGS);
        let backing_path_json = json_string(path_text(&backing_path));
        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let metrics_path_json = json_string(path_text(&metrics_path));
        let logger_path_json = json_string(path_text(&logger_path));
        let uds_path_json = json_string(path_text(&uds_path));
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": 1, "mem_size_mib": 256}},
                "boot-source": {{
                    "kernel_image_path": {kernel_path_json},
                    "initrd_path": {initrd_path_json},
                    "boot_args": {boot_args_json}
                }},
                "drives": [{{
                    "drive_id": "data",
                    "path_on_host": {backing_path_json},
                    "is_root_device": false,
                    "is_read_only": false,
                    "io_engine": "Async"
                }}],
                "vsock": {{"guest_cid": 3, "uds_path": {uds_path_json}}},
                "metrics": {{"metrics_path": {metrics_path_json}}},
                "logger": {{"log_path": {logger_path_json}}},
                "serial": {{"serial_out_path": {serial_output_path_json}}}
            }}"#
        );
        fs::write(&config_path, config).expect("config file should be written");

        let mut bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path)],
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(&running_instance_info, "GET / after config-file startup");
        assert_response_contains(
            &running_instance_info,
            &format!(r#""id":"{instance_id}""#),
            "GET / after config-file startup",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after config-file startup",
        );

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after config-file startup");
        assert_response_contains(
            &vm_config,
            r#""vcpu_count":1"#,
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            r#""mem_size_mib":256"#,
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""kernel_image_path":{kernel_path_json}"#),
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""initrd_path":{initrd_path_json}"#),
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""boot_args":{boot_args_json}"#),
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            r#""drive_id":"data""#,
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{backing_path_json}"#),
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            r#""guest_cid":3"#,
            "GET /vm/config after config-file startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""uds_path":{uds_path_json}"#),
            "GET /vm/config after config-file startup",
        );
        UnixStream::connect(&uds_path).unwrap_or_else(|err| {
            panic!(
                "config-file startup should bind the configured vsock listener at {}: {err}",
                uds_path.display()
            )
        });

        let second_start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_bad_request_response(
            &second_start_response,
            "PUT /actions second InstanceStart after config-file startup",
        );
        assert_response_contains(
            &second_start_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: InstanceStart"}"#,
            "PUT /actions second InstanceStart after config-file startup",
        );

        let post_start_machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_bad_request_response(
            &post_start_machine_response,
            "PUT /machine-config after config-file startup",
        );
        assert_response_contains(
            &post_start_machine_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutMachineConfig"}"#,
            "PUT /machine-config after config-file startup",
        );

        let flush_metrics_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(&flush_metrics_response, "PUT /actions FlushMetrics");
        assert_metrics_output(
            &metrics_path,
            None,
            r#"{"actions_count":2,"actions_fails":1,"balloon_count":0,"balloon_fails":0,"boot_source_count":0,"boot_source_fails":0,"cpu_cfg_count":0,"cpu_cfg_fails":0,"drive_count":0,"drive_fails":0,"hotplug_memory_count":0,"hotplug_memory_fails":0,"logger_count":0,"logger_fails":0,"machine_cfg_count":1,"machine_cfg_fails":1,"metrics_count":0,"metrics_fails":0,"mmds_count":0,"mmds_fails":0,"network_count":0,"network_fails":0,"pmem_count":0,"pmem_fails":0,"serial_count":0,"serial_fails":0,"vsock_count":0,"vsock_fails":0}"#,
            None,
        );
        assert_logger_output(&logger_path, LoggerPrefixExpectation::None);

        if let Err(err) =
            wait_for_file_prefix_marker(&backing_path, BLOCK_WRITE_MARKER, GUEST_EXECUTION_TIMEOUT)
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "config-file guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        if let Err(err) = wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_WRITE_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "config-file guest serial output file did not contain block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let output = bangbang.terminate();
        assert_clean_shutdown(output, &socket_path, "bangbang config file");
        assert_normal_terminal_metrics_output(&metrics_path);
        assert!(
            !uds_path.exists(),
            "bangbang config-file shutdown should remove its owned vsock listener path"
        );
    }

    #[test]
    fn signed_executable_starts_no_api_from_config_file() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let backing_path = test_dir.path().join("data.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let logger_path = test_dir.path().join("logger.out");
        let uds_path = test_dir.path().join("na-v.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&backing_path);
        create_empty_file(&serial_output_path);

        let kernel_path_json = json_string(path_text(&kernel_path));
        let initrd_path_json = json_string(path_text(&initrd_path));
        let boot_args_json = json_string(GUEST_BOOT_ARGS);
        let backing_path_json = json_string(path_text(&backing_path));
        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let logger_path_json = json_string(path_text(&logger_path));
        let uds_path_json = json_string(path_text(&uds_path));
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": 1, "mem_size_mib": 256}},
                "boot-source": {{
                    "kernel_image_path": {kernel_path_json},
                    "initrd_path": {initrd_path_json},
                    "boot_args": {boot_args_json}
                }},
                "drives": [{{
                    "drive_id": "data",
                    "path_on_host": {backing_path_json},
                    "is_root_device": false,
                    "is_read_only": false
                }}],
                "vsock": {{"guest_cid": 3, "uds_path": {uds_path_json}}},
                "logger": {{"log_path": {logger_path_json}}},
                "serial": {{"serial_out_path": {serial_output_path_json}}}
            }}"#
        );
        fs::write(&config_path, config).expect("config file should be written");

        let mut bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path), "--no-api"],
        );

        assert!(
            !socket_path.exists(),
            "no-api config-file startup must not publish an API socket"
        );
        UnixStream::connect(&uds_path).unwrap_or_else(|err| {
            panic!(
                "no-api config-file startup should bind the configured vsock listener at {}: {err}",
                uds_path.display()
            )
        });

        if let Err(err) =
            wait_for_file_prefix_marker(&backing_path, BLOCK_WRITE_MARKER, GUEST_EXECUTION_TIMEOUT)
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "no-api config-file guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        if let Err(err) = wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_WRITE_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "no-api config-file guest serial output file did not contain block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_no_api_logger_output(&logger_path);
        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang no-api config file",
        );
        assert!(
            !uds_path.exists(),
            "bangbang no-api config-file shutdown should remove its owned vsock listener path"
        );
    }

    #[test]
    fn signed_executable_boots_async_block_root_and_data_when_data_configured_first() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config direct rootfs");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false,
                "io_engine":"Async"
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data before rootfs direct rootfs",
        );

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true,
                "io_engine":"Async"
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(
            &rootfs_response,
            "PUT /drives/rootfs after data direct rootfs",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart direct rootfs");

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_BLOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not write block marker after data-first drive configuration: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang direct rootfs");
    }

    #[test]
    fn signed_executable_writes_direct_rootfs_boot_markers_to_serial_output() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let serial_output_path = test_dir.path().join("serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_empty_file(&serial_output_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config direct rootfs serial",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source direct rootfs serial");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs direct rootfs serial");

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(&serial_response, "PUT /serial direct rootfs");

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart direct rootfs serial",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after direct rootfs serial InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after direct rootfs serial InstanceStart",
        );

        if let Err(err) = wait_for_file_contains_marker(
            &serial_output_path,
            DIRECT_ROOTFS_BOOT_OK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let serial_prefix = file_prefix_lossy(&serial_output_path, 256);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not write boot marker to configured serial output through signed bangbang executable: {err}; serial prefix: {serial_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang direct rootfs serial",
        );
    }

    #[test]
    fn signed_executable_streams_default_serial_stdio_across_lifecycle_boundaries() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let metrics_path = test_dir.path().join("metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();
        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config serial stdio");

        let boot_body = format!(
            r#"{{"kernel_image_path":{},"initrd_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(path_text(&initrd_path)),
            json_string(GUEST_SERIAL_RX_BOOT_ARGS),
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source serial stdio");

        create_empty_file(&metrics_path);
        let metrics_body = format!(
            r#"{{"metrics_path":{}}}"#,
            json_string(path_text(&metrics_path))
        );
        let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
        assert_no_content_response(&metrics_response, "PUT /metrics serial stdio");

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart serial stdio");

        bangbang
            .wait_for_stdout_marker(GUEST_SERIAL_RX_READY_MARKER, GUEST_EXECUTION_TIMEOUT)
            .unwrap_or_else(|error| panic!("guest serial receiver should become ready: {error}"));

        let mut serial_input = b"BANGBANG_SERIAL_RX_".to_vec();
        serial_input.extend(std::iter::repeat_n(b'A', 80));
        serial_input.extend_from_slice(b"_END\n");
        assert!(
            serial_input.len() > bangbang_runtime::serial::SERIAL_RECEIVE_FIFO_CAPACITY,
            "test input must cross the bounded host receive chunk"
        );
        let pause_response = http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&pause_response, "PATCH /vm Paused serial stdio");
        let paused = http_get(&socket_path, "/");
        assert_ok_response(&paused, "GET / while serial stdio paused");
        assert_response_contains(
            &paused,
            r#""state":"Paused""#,
            "GET / while serial stdio paused",
        );

        bangbang.write_stdin(&serial_input);
        std::thread::sleep(Duration::from_millis(200));
        let paused_stdout = bangbang.stdout_snapshot();
        assert!(paused_stdout.contains(GUEST_SERIAL_RX_READY_MARKER));
        assert!(!paused_stdout.contains(GUEST_SERIAL_RX_SUCCESS_MARKER));
        assert!(!paused_stdout.contains(GUEST_SERIAL_RX_FAILURE_MARKER));

        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused serial stdio capture preflight",
        );
        let captured_stdout = bangbang.stdout_snapshot();
        assert!(!captured_stdout.contains(GUEST_SERIAL_RX_SUCCESS_MARKER));
        assert!(!captured_stdout.contains(GUEST_SERIAL_RX_FAILURE_MARKER));

        let resume_response = http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(&resume_response, "PATCH /vm Resumed serial stdio");
        bangbang
            .wait_for_stdout_marker(GUEST_SERIAL_RX_SUCCESS_MARKER, GUEST_EXECUTION_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("guest should receive the full bounded serial stream after resume: {error}")
            });
        assert!(
            !bangbang
                .stdout_snapshot()
                .contains(GUEST_SERIAL_RX_FAILURE_MARKER),
            "guest must validate the exact serial input"
        );

        bangbang.close_stdin();
        std::thread::sleep(Duration::from_millis(100));
        let after_eof = http_get(&socket_path, "/");
        assert_ok_response(&after_eof, "GET / after serial stdin EOF");
        assert_response_contains(
            &after_eof,
            r#""state":"Running""#,
            "GET / after serial stdin EOF",
        );

        let flush_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(&flush_response, "FlushMetrics serial stdio");
        let metrics_output = fs::read_to_string(&metrics_path)
            .expect("serial stdio metrics output should be readable");
        let metrics: serde_json::Value = serde_json::from_str(
            metrics_output
                .lines()
                .next_back()
                .expect("serial stdio metrics should contain a generation"),
        )
        .expect("serial stdio metrics generation should be JSON");
        assert_eq!(
            metrics.pointer("/uart/input_count"),
            Some(&serde_json::json!(serial_input.len())),
            "UART metrics must account for the exact bounded input stream"
        );
        assert_eq!(
            metrics.pointer("/uart/error_count"),
            Some(&serde_json::json!(0)),
            "serial stdin EOF must detach without recording an input error"
        );
        assert_eq!(
            metrics.pointer("/uart/overrun_count"),
            Some(&serde_json::json!(0)),
            "bounded host reads must not overrun the UART FIFO"
        );

        let output = bangbang.terminate();
        assert!(output.stdout.contains(GUEST_SERIAL_RX_READY_MARKER));
        assert!(output.stdout.contains(GUEST_SERIAL_RX_SUCCESS_MARKER));
        assert!(!output.stdout.contains(GUEST_SERIAL_RX_FAILURE_MARKER));
        assert_clean_shutdown(output, &socket_path, "bangbang default serial stdio");
    }

    #[test]
    fn signed_executable_configured_serial_output_does_not_consume_process_stdin() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let serial_output_path = test_dir.path().join("serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();
        create_empty_file(&serial_output_path);
        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config configured serial stdin exclusion",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"initrd_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(path_text(&initrd_path)),
            json_string(GUEST_SERIAL_RX_BOOT_ARGS),
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(
            &boot_response,
            "PUT /boot-source configured serial stdin exclusion",
        );
        let serial_body = format!(
            r#"{{"serial_out_path":{}}}"#,
            json_string(path_text(&serial_output_path))
        );
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(
            &serial_response,
            "PUT /serial configured serial stdin exclusion",
        );
        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions configured serial stdin exclusion",
        );
        wait_for_file_contains_marker(
            &serial_output_path,
            GUEST_SERIAL_RX_READY_MARKER.as_bytes(),
            GUEST_EXECUTION_TIMEOUT,
        )
        .unwrap_or_else(|error| panic!("configured serial receiver should become ready: {error}"));

        let mut serial_input = b"BANGBANG_SERIAL_RX_".to_vec();
        serial_input.extend(std::iter::repeat_n(b'A', 80));
        serial_input.extend_from_slice(b"_END\n");
        bangbang.write_stdin(&serial_input);
        std::thread::sleep(Duration::from_millis(300));
        let configured_output = fs::read_to_string(&serial_output_path)
            .expect("configured serial output should remain readable");
        assert!(configured_output.contains(GUEST_SERIAL_RX_READY_MARKER));
        assert!(!configured_output.contains(GUEST_SERIAL_RX_SUCCESS_MARKER));
        assert!(!configured_output.contains(GUEST_SERIAL_RX_FAILURE_MARKER));
        assert!(
            !bangbang
                .stdout_snapshot()
                .contains(GUEST_SERIAL_RX_READY_MARKER)
        );

        bangbang.close_stdin();
        let running = http_get(&socket_path, "/");
        assert_ok_response(&running, "GET / configured serial stdin exclusion");
        assert_response_contains(
            &running,
            r#""state":"Running""#,
            "GET / configured serial stdin exclusion",
        );
        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang configured serial stdin exclusion",
        );
        let configured_output = fs::read_to_string(&serial_output_path)
            .expect("configured serial output should remain readable after shutdown");
        assert!(!configured_output.contains(GUEST_SERIAL_RX_SUCCESS_MARKER));
        assert!(!configured_output.contains(GUEST_SERIAL_RX_FAILURE_MARKER));
    }

    #[test]
    fn signed_executable_rate_limits_default_serial_stdout_and_records_drops() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let metrics_path = test_dir.path().join("metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();
        create_empty_file(&metrics_path);
        let bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config rate-limited serial stdout",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"initrd_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(path_text(&initrd_path)),
            json_string(GUEST_SERIAL_RX_BOOT_ARGS),
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(
            &boot_response,
            "PUT /boot-source rate-limited serial stdout",
        );
        let metrics_body = format!(
            r#"{{"metrics_path":{}}}"#,
            json_string(path_text(&metrics_path))
        );
        let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
        assert_no_content_response(&metrics_response, "PUT /metrics rate-limited serial stdout");
        let serial_response = http_put_json(
            &socket_path,
            "/serial",
            r#"{"rate_limiter":{"size":1,"refill_time":60000}}"#,
        );
        assert_no_content_response(&serial_response, "PUT /serial rate-limited stdout");
        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions rate-limited serial stdout");

        std::thread::sleep(Duration::from_millis(300));
        let flush_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(&flush_response, "FlushMetrics rate-limited serial stdout");
        let metrics_output = fs::read_to_string(&metrics_path)
            .expect("rate-limited serial metrics should be readable");
        let metrics: serde_json::Value = serde_json::from_str(
            metrics_output
                .lines()
                .next_back()
                .expect("rate-limited serial metrics should contain a generation"),
        )
        .expect("rate-limited serial metrics generation should be JSON");
        assert!(
            metrics
                .pointer("/uart/rate_limiter_dropped_bytes")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|dropped| dropped > 0),
            "serial stdout limiter must account for dropped guest bytes: {metrics}"
        );
        assert_eq!(
            metrics.pointer("/uart/error_count"),
            Some(&serde_json::json!(0)),
            "intentional rate limiting must not be reported as an output error"
        );
        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang rate-limited default serial stdout",
        );
    }

    #[test]
    fn signed_executable_isolates_concurrent_default_serial_stdio_streams() {
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let start_guest = |test_dir: &TestDir, process_name: &str| {
            let socket_path = test_dir.path().join("api.socket");
            let process = BangbangProcess::start(&socket_path, &test_dir.instance_id());
            let machine_response = http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            );
            assert_no_content_response(
                &machine_response,
                &format!("PUT /machine-config {process_name}"),
            );
            let boot_body = format!(
                r#"{{"kernel_image_path":{},"initrd_path":{},"boot_args":{}}}"#,
                json_string(path_text(&kernel_path)),
                json_string(path_text(&initrd_path)),
                json_string(GUEST_SERIAL_RX_BOOT_ARGS),
            );
            let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
            assert_no_content_response(&boot_response, &format!("PUT /boot-source {process_name}"));
            let start_response = http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            );
            assert_no_content_response(&start_response, &format!("PUT /actions {process_name}"));
            process
                .wait_for_stdout_marker(GUEST_SERIAL_RX_READY_MARKER, GUEST_EXECUTION_TIMEOUT)
                .unwrap_or_else(|error| {
                    panic!("{process_name} serial receiver should become ready: {error}")
                });
            process
        };

        let first_dir = TestDir::new();
        let second_dir = TestDir::new();
        let first_socket = first_dir.path().join("api.socket");
        let second_socket = second_dir.path().join("api.socket");
        let mut first = start_guest(&first_dir, "concurrent serial process A");
        let mut second = start_guest(&second_dir, "concurrent serial process B");

        let pause_first = http_json(&first_socket, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&pause_first, "pause concurrent serial process A");
        let mut serial_input = b"BANGBANG_SERIAL_RX_".to_vec();
        serial_input.extend(std::iter::repeat_n(b'A', 80));
        serial_input.extend_from_slice(b"_END\n");

        second.write_stdin(&serial_input);
        second
            .wait_for_stdout_marker(GUEST_SERIAL_RX_SUCCESS_MARKER, GUEST_EXECUTION_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("concurrent serial process B should receive only its input: {error}")
            });
        assert!(
            !first
                .stdout_snapshot()
                .contains(GUEST_SERIAL_RX_SUCCESS_MARKER)
        );

        first.write_stdin(&serial_input);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !first
                .stdout_snapshot()
                .contains(GUEST_SERIAL_RX_SUCCESS_MARKER),
            "paused concurrent process A must not consume its own queued input"
        );
        let resume_first = http_json(&first_socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(&resume_first, "resume concurrent serial process A");
        first
            .wait_for_stdout_marker(GUEST_SERIAL_RX_SUCCESS_MARKER, GUEST_EXECUTION_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("concurrent serial process A should consume its input after resume: {error}")
            });

        assert!(
            !first
                .stdout_snapshot()
                .contains(GUEST_SERIAL_RX_FAILURE_MARKER)
        );
        assert!(
            !second
                .stdout_snapshot()
                .contains(GUEST_SERIAL_RX_FAILURE_MARKER)
        );
        first.close_stdin();
        second.close_stdin();
        assert_clean_shutdown(
            first.terminate(),
            &first_socket,
            "concurrent default serial process A",
        );
        assert_clean_shutdown(
            second.terminate(),
            &second_socket,
            "concurrent default serial process B",
        );
    }

    #[test]
    fn signed_executable_runs_vhost_user_block_over_mmio_and_recovers_discovery() {
        run_signed_vhost_user_block_guest(VhostUserBlockGuestCase {
            mode: "ro",
            root_read_only: true,
            enable_pci: false,
            partitioned_root: false,
            retry_rejected_discovery: true,
            refresh_scratch_config: true,
            memory_hotplug: true,
            success_marker: VHOST_USER_BLOCK_RO_MARKER,
        });
    }

    #[test]
    fn signed_executable_runs_vhost_user_block_pci_partuuid_writable_root() {
        run_signed_vhost_user_block_guest(VhostUserBlockGuestCase {
            mode: "rw",
            root_read_only: false,
            enable_pci: true,
            partitioned_root: true,
            retry_rejected_discovery: false,
            refresh_scratch_config: false,
            memory_hotplug: false,
            success_marker: VHOST_USER_BLOCK_RW_MARKER,
        });
    }

    fn run_signed_vhost_user_block_guest(case: VhostUserBlockGuestCase) {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let root_backend_socket = test_dir.path().join("root-vhost.sock");
        let scratch_backend_socket = test_dir.path().join("scratch-vhost.sock");
        let scratch_path = test_dir.path().join("scratch.img");
        let metrics_path = test_dir.path().join("metrics.out");
        let serial_path = test_dir.path().join("serial.out");
        let snapshot_state = test_dir.path().join("rejected-vhost.state");
        let snapshot_memory = test_dir.path().join("rejected-vhost.memory");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let source_rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let rootfs_path = if case.partitioned_root {
            let path = test_dir.path().join("root-partitioned.img");
            create_mbr_partitioned_rootfs(&source_rootfs_path, &path);
            path
        } else if case.root_read_only {
            source_rootfs_path
        } else {
            let path = test_dir.path().join("root-writable.ext4");
            fs::copy(&source_rootfs_path, &path)
                .expect("writable vhost-user root fixture should copy");
            path
        };
        create_block_backing_with_prefix(&scratch_path, 8, VHOST_USER_BLOCK_HOST_MARKER);
        create_empty_file(&serial_path);

        let scratch_backend = VhostUserBlockBackend::start(
            &scratch_backend_socket,
            &scratch_path,
            VhostUserBlockBackendOptions::regular(false),
        )
        .expect("test scratch vhost-user backend should start");
        let mut root_backend = if case.retry_rejected_discovery {
            None
        } else {
            Some(
                VhostUserBlockBackend::start(
                    &root_backend_socket,
                    &rootfs_path,
                    VhostUserBlockBackendOptions::regular(case.root_read_only),
                )
                .expect("test root vhost-user backend should start"),
            )
        };
        let rejected_backend = case.retry_rejected_discovery.then(|| {
            VhostUserBlockBackend::start(
                &root_backend_socket,
                &rootfs_path,
                VhostUserBlockBackendOptions::missing_version_one(case.root_read_only),
            )
            .expect("rejecting root vhost-user backend should start")
        });

        let instance_id = test_dir.instance_id();
        let mut bangbang = if case.enable_pci {
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"])
        } else {
            BangbangProcess::start(&socket_path, &instance_id)
        };
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT /machine-config vhost-user block",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/metrics",
                &format!(
                    r#"{{"metrics_path":{}}}"#,
                    json_string(path_text(&metrics_path))
                ),
            ),
            "PUT /metrics vhost-user block",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/serial",
                &format!(
                    r#"{{"serial_out_path":{}}}"#,
                    json_string(path_text(&serial_path))
                ),
            ),
            "PUT /serial vhost-user block",
        );
        let mut boot_args = format!(
            "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.vhost-user-block={}",
            case.mode
        );
        if case.enable_pci {
            boot_args.push_str(" bangbang.expect-pci-data=1");
        }
        if case.partitioned_root {
            boot_args.push_str(" bangbang.expect-partuuid=");
            boot_args.push_str(VHOST_USER_BLOCK_PARTUUID);
        }
        if case.refresh_scratch_config {
            boot_args.push_str(" bangbang.expect-vhost-resize=1");
        }
        if case.memory_hotplug {
            boot_args.push_str(" memhp_default_state=online_movable");
        }
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/boot-source",
                &format!(
                    r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
                    json_string(path_text(&kernel_path)),
                    json_string(&boot_args)
                ),
            ),
            "PUT /boot-source vhost-user block",
        );

        let partuuid_field = if case.partitioned_root {
            format!(r#", "partuuid":"{VHOST_USER_BLOCK_PARTUUID}""#)
        } else {
            String::new()
        };
        let root_cache_field = if case.root_read_only {
            ""
        } else {
            r#", "cache_type":"Writeback""#
        };
        let root_body = format!(
            r#"{{"drive_id":"rootfs","is_root_device":true,"socket":{}{}{} }}"#,
            json_string(path_text(&root_backend_socket)),
            partuuid_field,
            root_cache_field,
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/rootfs", &root_body),
            "PUT /drives/rootfs vhost-user block",
        );
        let scratch_body = format!(
            r#"{{"drive_id":"scratch","is_root_device":false,"cache_type":"Writeback","socket":{}}}"#,
            json_string(path_text(&scratch_backend_socket)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/scratch", &scratch_body),
            "PUT /drives/scratch vhost-user block",
        );
        if case.memory_hotplug {
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/hotplug/memory",
                    r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
                ),
                "PUT /hotplug/memory after vhost-user drives",
            );
        }

        let before_start = http_get(&socket_path, "/vm/config");
        assert_exact_vhost_user_vm_config(
            &before_start,
            &root_backend_socket,
            &scratch_backend_socket,
            case.partitioned_root.then_some(VHOST_USER_BLOCK_PARTUUID),
            case.root_read_only,
            "GET /vm/config before vhost-user start",
        );

        if let Some(rejected_backend) = rejected_backend {
            let rejected_start = http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            );
            assert_bad_request_response(&rejected_start, "rejected vhost-user InstanceStart");
            assert_response_contains(
                &rejected_start,
                "vhost-user backend lacks required virtio features",
                "rejected vhost-user InstanceStart",
            );
            assert!(
                !rejected_start.contains(path_text(&root_backend_socket)),
                "rejected discovery response must redact the socket path"
            );
            let not_started = http_get(&socket_path, "/");
            assert_response_contains(
                &not_started,
                r#""state":"Not started""#,
                "GET / after rejected vhost-user InstanceStart",
            );
            let rejected_report = rejected_backend
                .finish()
                .expect("rejecting vhost-user backend should finish");
            assert_eq!(rejected_report.owner_requests, 1);
            assert_eq!(rejected_report.config_requests, 0);
            assert!(rejected_report.discovery_rejected);
            assert!(!rejected_report.activated);

            root_backend = Some(
                VhostUserBlockBackend::start(
                    &root_backend_socket,
                    &rootfs_path,
                    VhostUserBlockBackendOptions::regular(case.root_read_only),
                )
                .expect("replacement root vhost-user backend should start"),
            );
        }

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        if !start_response.starts_with("HTTP/1.1 204 No Content\r\n") {
            let root_report = root_backend.as_ref().map(VhostUserBlockBackend::report);
            let scratch_report = scratch_backend.report();
            let output = bangbang.force_stop_and_collect();
            panic!(
                "vhost-user InstanceStart failed; response:\n{start_response}\nroot report: {root_report:?}; scratch report: {scratch_report:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart vhost-user block",
        );
        let root_backend = root_backend.expect("root vhost-user backend should be installed");
        if let Err(error) = root_backend.wait_for_activation(GUEST_EXECUTION_TIMEOUT) {
            let root_report = root_backend.report();
            let scratch_report = scratch_backend.report();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "root vhost-user backend did not activate: {error}; root report: {root_report:?}; scratch report: {scratch_report:?}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        if let Err(error) = scratch_backend.wait_for_activation(GUEST_EXECUTION_TIMEOUT) {
            let root_report = root_backend.report();
            let scratch_report = scratch_backend.report();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "scratch vhost-user backend did not activate: {error}; root report: {root_report:?}; scratch report: {scratch_report:?}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        let running = http_get(&socket_path, "/");
        assert_response_contains(
            &running,
            r#""state":"Running""#,
            "GET / after vhost-user InstanceStart",
        );
        let running_config = http_get(&socket_path, "/vm/config");
        assert_exact_vhost_user_vm_config(
            &running_config,
            &root_backend_socket,
            &scratch_backend_socket,
            case.partitioned_root.then_some(VHOST_USER_BLOCK_PARTUUID),
            case.root_read_only,
            "GET /vm/config after vhost-user start",
        );

        if let Err(error) = wait_for_file_prefix_marker(
            &scratch_path,
            case.success_marker,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let root_report = root_backend.report();
            let scratch_report = scratch_backend.report();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{} vhost-user guest did not complete block I/O: {error}; root report: {root_report:?}; scratch report: {scratch_report:?}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                case.mode, output.status, output.stdout, output.stderr
            );
        }
        if case.memory_hotplug {
            assert_no_content_response(
                &http_json_with_io_timeout(
                    &socket_path,
                    "PATCH",
                    "/hotplug/memory",
                    r#"{"requested_size_mib":128}"#,
                    GUEST_EXECUTION_TIMEOUT,
                ),
                "PATCH /hotplug/memory grow with active vhost-user block",
            );
            let grown = wait_for_http_response_fragment(
                &socket_path,
                "/hotplug/memory",
                r#""plugged_size_mib":128"#,
                GUEST_EXECUTION_TIMEOUT,
            )
            .expect("guest should complete memory grow with active vhost-user block");
            assert_ok_response(&grown, "GET grown memory with active vhost-user block");
            assert_vhost_user_memory_aperture(&root_backend.report(), "root");
            assert_vhost_user_memory_aperture(&scratch_backend.report(), "scratch");
        }
        if case.refresh_scratch_config {
            fs::OpenOptions::new()
                .write(true)
                .open(&scratch_path)
                .expect("scratch backing should reopen for MMIO config refresh")
                .set_len(10 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE)
                .expect("scratch backing should resize for MMIO config refresh");
            assert_no_content_response(
                &http_json(
                    &socket_path,
                    "PATCH",
                    "/drives/scratch",
                    r#"{"drive_id":"scratch"}"#,
                ),
                "PATCH /drives/scratch MMIO vhost-user config refresh",
            );
            wait_for_file_markers_at(
                &scratch_path,
                &[(
                    9 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                    VHOST_CONFIG_RESIZED_MARKER,
                    b"BANGBANG_VHOST_USER_BLOCK_FAIL",
                )],
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            )
            .expect("guest should observe refreshed MMIO vhost-user capacity");
            assert_eq!(
                scratch_backend.report().config_requests,
                2,
                "startup and MMIO PATCH should issue two exact config requests"
            );
        }
        scratch_backend
            .wait_for_flush(GUEST_EXECUTION_TIMEOUT)
            .expect("scratch vhost-user backend should observe the direct synchronous write flush");
        if !case.root_read_only {
            root_backend
                .wait_for_flush(GUEST_EXECUTION_TIMEOUT)
                .expect("writable root vhost-user backend should observe a filesystem flush");
        }
        assert_active_vhost_user_report(&root_backend.report(), case.root_read_only, 1, "root");
        let scratch_report = scratch_backend.report();
        assert_active_vhost_user_report(
            &scratch_report,
            false,
            if case.refresh_scratch_config { 2 } else { 1 },
            "scratch",
        );
        assert!(
            scratch_report.reads > 0,
            "scratch backend should serve reads"
        );
        assert!(
            scratch_report.writes > 0,
            "scratch backend should serve writes"
        );
        assert!(
            scratch_report.flushes > 0,
            "scratch backend should serve an explicit flush: {scratch_report:?}"
        );
        if !case.root_read_only {
            let root_report = root_backend.report();
            assert!(root_report.writes > 0, "writable root should serve writes");
            assert!(
                root_report.flushes > 0,
                "writable root should serve flushes: {root_report:?}"
            );
        }

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "PATCH /vm Paused vhost-user block",
        );
        let snapshot_create = http_json_with_io_timeout(
            &socket_path,
            "PUT",
            "/snapshot/create",
            &format!(
                r#"{{"snapshot_type":"Full","snapshot_path":{},"mem_file_path":{}}}"#,
                json_string(path_text(&snapshot_state)),
                json_string(path_text(&snapshot_memory))
            ),
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_bad_request_response(&snapshot_create, "vhost-user snapshot create");
        assert_response_contains(
            &snapshot_create,
            "native-v1 storage preflight failed: vhost-user block capture is unsupported",
            "vhost-user snapshot create",
        );
        for private_path in [
            &snapshot_state,
            &snapshot_memory,
            &root_backend_socket,
            &scratch_backend_socket,
            &rootfs_path,
            &scratch_path,
        ] {
            assert!(!snapshot_create.contains(path_text(private_path)));
        }
        assert!(!snapshot_state.exists());
        assert!(!snapshot_memory.exists());
        assert_no_snapshot_staging(test_dir.path());
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "PATCH /vm Resumed vhost-user block",
        );
        if case.memory_hotplug {
            assert_no_content_response(
                &http_json_with_io_timeout(
                    &socket_path,
                    "PATCH",
                    "/hotplug/memory",
                    r#"{"requested_size_mib":0}"#,
                    GUEST_EXECUTION_TIMEOUT,
                ),
                "PATCH /hotplug/memory shrink with active vhost-user block",
            );
            let shrunk = wait_for_http_response_fragment(
                &socket_path,
                "/hotplug/memory",
                r#""plugged_size_mib":0"#,
                GUEST_EXECUTION_TIMEOUT,
            )
            .expect("guest should complete memory shrink with active vhost-user block");
            assert_ok_response(&shrunk, "GET shrunk memory with active vhost-user block");
            assert_vhost_user_memory_aperture(&root_backend.report(), "root after shrink");
            assert_vhost_user_memory_aperture(&scratch_backend.report(), "scratch after shrink");
        }

        scratch_backend
            .disconnect()
            .expect("scratch vhost-user backend should disconnect");
        let terminal_scratch_report = scratch_backend
            .finish()
            .expect("scratch vhost-user backend should stop after disconnect");
        assert!(terminal_scratch_report.activated);
        wait_for_block_event_failure(
            &socket_path,
            &metrics_path,
            "scratch",
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("scratch backend closure should be reflected in block metrics");
        let after_disconnect = http_get(&socket_path, "/");
        assert_response_contains(
            &after_disconnect,
            r#""state":"Running""#,
            "GET / after vhost-user backend disconnect",
        );
        let config_after_disconnect = http_get(&socket_path, "/vm/config");
        assert_exact_vhost_user_vm_config(
            &config_after_disconnect,
            &root_backend_socket,
            &scratch_backend_socket,
            case.partitioned_root.then_some(VHOST_USER_BLOCK_PARTUUID),
            case.root_read_only,
            "GET /vm/config after vhost-user backend disconnect",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            &format!("bangbang {} vhost-user block", case.mode),
        );
        root_backend
            .wait_for_frontend_close(GUEST_EXECUTION_TIMEOUT)
            .expect("root vhost-user backend should observe process shutdown");
        let terminal_root_report = root_backend
            .finish()
            .expect("root vhost-user backend should finish after process shutdown");
        assert!(terminal_root_report.frontend_closed);
        assert!(
            !root_backend_socket.exists(),
            "root backend socket should be cleaned up"
        );
        assert!(
            !scratch_backend_socket.exists(),
            "scratch backend socket should be cleaned up"
        );
    }

    fn assert_exact_vhost_user_vm_config(
        response: &str,
        root_socket: &Path,
        scratch_socket: &Path,
        partuuid: Option<&str>,
        root_read_only: bool,
        context: &str,
    ) {
        assert_ok_response(response, context);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("VM config response should contain a body separator");
        let config: serde_json::Value =
            serde_json::from_str(body).expect("VM config response should contain JSON");
        let mut expected_root = serde_json::json!({
            "cache_type": if root_read_only { "Unsafe" } else { "Writeback" },
            "drive_id": "rootfs",
            "is_root_device": true,
            "socket": path_text(root_socket),
        });
        if let Some(partuuid) = partuuid {
            expected_root
                .as_object_mut()
                .expect("expected root config should be an object")
                .insert(
                    "partuuid".to_string(),
                    serde_json::Value::String(partuuid.to_string()),
                );
        }
        assert_eq!(
            config["drives"],
            serde_json::json!([
                expected_root,
                {
                    "cache_type": "Writeback",
                    "drive_id": "scratch",
                    "is_root_device": false,
                    "socket": path_text(scratch_socket),
                }
            ]),
            "{context} should return exact vhost-user-only drive fields; response:\n{response}"
        );
    }

    fn assert_active_vhost_user_report(
        report: &VhostUserBlockBackendReport,
        read_only: bool,
        expected_config_requests: u64,
        context: &str,
    ) {
        const REQUIRED_FEATURES: u64 = (1 << 30) | (1 << 32);
        const READ_ONLY_FEATURE: u64 = 1 << 5;
        let features = report
            .guest_features
            .unwrap_or_else(|| panic!("{context} backend should observe guest features"));
        assert_eq!(features & REQUIRED_FEATURES, REQUIRED_FEATURES);
        assert_eq!(features & READ_ONLY_FEATURE != 0, read_only);
        assert_eq!(report.owner_requests, 1);
        assert_eq!(report.config_requests, expected_config_requests);
        assert!(report.memory_regions > 0);
        assert_eq!(report.memory_table_requests, 1);
        assert_eq!(report.queue_size, Some(256));
        assert!(report.activated);
        assert!(report.kicks > 0, "{context} backend should receive kicks");
        assert!(report.calls > 0, "{context} backend should send calls");
        assert!(
            report.requests > 0,
            "{context} backend should serve requests"
        );
    }

    fn assert_vhost_user_memory_aperture(report: &VhostUserBlockBackendReport, context: &str) {
        const MIB: u64 = 1024 * 1024;
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
                (
                    bangbang_runtime::memory::aarch64::DRAM_MEM_START,
                    256 * MIB,
                    0
                ),
                (
                    bangbang_runtime::memory_hotplug::VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
                    128 * MIB,
                    0,
                ),
            ],
            "{context} backend must receive boot RAM plus one complete stable aperture"
        );
        assert_eq!(report.memory_regions, geometry.len());
        assert_eq!(report.memory_table_requests, 1);
    }

    fn assert_aggregate_storage_vhost_user_memory_aperture(report: &VhostUserBlockBackendReport) {
        const MIB: u64 = 1024 * 1024;
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
                (
                    bangbang_runtime::memory::aarch64::DRAM_MEM_START,
                    256 * MIB,
                    0,
                ),
                (
                    bangbang_runtime::memory_hotplug::VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value()
                        + 128 * MIB,
                    128 * MIB,
                    0,
                ),
            ],
            "aggregate startup pmem must move the complete virtio-mem aperture to the next 128 MiB slot",
        );
        assert_eq!(report.memory_regions, geometry.len());
        assert_eq!(report.memory_table_requests, 1);
    }

    fn wait_for_block_event_failure(
        socket_path: &Path,
        metrics_path: &Path,
        drive_id: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let started = Instant::now();
        let metrics_key = format!("block_{drive_id}");
        loop {
            let response =
                http_put_json(socket_path, "/actions", r#"{"action_type":"FlushMetrics"}"#);
            if !response.starts_with("HTTP/1.1 204 No Content\r\n") {
                return Err(format!(
                    "FlushMetrics failed after backend death:\n{response}"
                ));
            }
            let event_fails = fs::read_to_string(metrics_path).ok().and_then(|output| {
                output.lines().rev().find_map(|line| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .ok()?
                        .get(&metrics_key)?
                        .get("event_fails")?
                        .as_u64()
                })
            });
            if event_fails.is_some_and(|count| count > 0) {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {metrics_key}.event_fails; latest={event_fails:?}"
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn create_mbr_partitioned_rootfs(source: &Path, destination: &Path) {
        const PARTITION_START_SECTORS: u32 = 2048;
        const PARTITION_START_BYTES: u64 =
            PARTITION_START_SECTORS as u64 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE;
        let source_size = fs::metadata(source)
            .expect("source rootfs metadata should be readable")
            .len();
        assert_eq!(
            source_size % bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            0,
            "rootfs fixture should contain complete sectors"
        );
        let partition_sectors =
            u32::try_from(source_size / bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE)
                .expect("rootfs fixture sector count should fit an MBR partition");
        let mut source_file = fs::File::open(source).expect("source rootfs should open");
        let mut destination_file = fs::File::create(destination)
            .expect("partitioned vhost-user root fixture should create");
        destination_file
            .set_len(PARTITION_START_BYTES + source_size)
            .expect("partitioned root fixture should be sized");
        destination_file
            .seek(SeekFrom::Start(PARTITION_START_BYTES))
            .expect("partitioned root fixture should seek to its partition");
        std::io::copy(&mut source_file, &mut destination_file)
            .expect("rootfs should copy into its MBR partition");

        let mut mbr = [0_u8; 512];
        mbr[440..444].copy_from_slice(&0x0eaa_91a0_u32.to_le_bytes());
        let partition = &mut mbr[446..462];
        partition[0] = 0x80;
        partition[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
        partition[4] = 0x83;
        partition[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
        partition[8..12].copy_from_slice(&PARTITION_START_SECTORS.to_le_bytes());
        partition[12..16].copy_from_slice(&partition_sectors.to_le_bytes());
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        destination_file
            .seek(SeekFrom::Start(0))
            .expect("partitioned root fixture should seek to its MBR");
        destination_file
            .write_all(&mbr)
            .expect("partitioned root fixture MBR should write");
        destination_file
            .sync_all()
            .expect("partitioned root fixture should be durable before startup");
    }

    #[test]
    fn signed_executable_clears_serial_output_before_start() {
        run_signed_executable_serial_clear_before_start("empty-object", "{}");
    }

    #[test]
    fn signed_executable_clears_serial_output_with_null_before_start() {
        run_signed_executable_serial_clear_before_start("null-path", r#"{"serial_out_path":null}"#);
    }

    fn run_signed_executable_serial_clear_before_start(case_name: &str, serial_clear_body: &str) {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let serial_output_path = test_dir
            .path()
            .join(format!("cleared-serial-{case_name}.out"));
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        let bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            &format!("PUT /machine-config serial clear before start {case_name}"),
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let initrd_path_json = json_string(path_text(&initrd_path));
        let boot_args_json = json_string(GUEST_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "initrd_path":{initrd_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(
            &boot_response,
            &format!("PUT /boot-source serial clear before start {case_name}"),
        );

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(
            &serial_response,
            &format!("PUT /serial before clear {case_name}"),
        );
        assert!(
            !serial_output_path.exists(),
            "PUT /serial should store the candidate path without creating it before startup"
        );

        let serial_clear_response = http_put_json(&socket_path, "/serial", serial_clear_body);
        assert_no_content_response(
            &serial_clear_response,
            &format!("PUT /serial clear {case_name}"),
        );
        assert!(
            !serial_output_path.exists(),
            "PUT /serial clear must not create the candidate path before startup"
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            &format!("PUT /actions InstanceStart after serial clear {case_name}"),
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            &format!("GET / after InstanceStart with cleared serial output {case_name}"),
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            &format!("GET / after InstanceStart with cleared serial output {case_name}"),
        );
        assert!(
            !serial_output_path.exists(),
            "InstanceStart must not open or create a cleared serial output path at {}",
            serial_output_path.display()
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            &format!("bangbang cleared serial output {case_name}"),
        );
    }

    #[test]
    fn signed_executable_boot_timer_guest_write_logs_boot_time() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let serial_output_path = test_dir.path().join("serial.out");
        let logger_path = test_dir.path().join("logger.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_empty_file(&serial_output_path);

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--boot-timer"]);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config boot timer");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(ROOTFS_BOOT_TIMER_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source boot timer");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs boot timer");

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(&serial_response, "PUT /serial boot timer");

        let logger_path_json = json_string(path_text(&logger_path));
        let logger_body = format!(r#"{{"log_path":{logger_path_json}}}"#);
        let logger_response = http_put_json(&socket_path, "/logger", &logger_body);
        assert_no_content_response(&logger_response, "PUT /logger boot timer");

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart boot timer");

        if let Err(err) = wait_for_file_contains_marker(
            &logger_path,
            BOOT_TIMER_LOG_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let serial_prefix = file_prefix_lossy(&serial_output_path, 256);
            let logger_prefix = file_prefix_lossy(&logger_path, 256);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "boot timer guest write did not produce logger output: {err}; serial prefix: {serial_prefix:?}; logger prefix: {logger_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let logger_output = fs::read_to_string(&logger_path).unwrap_or_else(|err| {
            panic!(
                "boot timer logger output {} should be readable: {err}",
                logger_path.display()
            )
        });
        assert!(
            logger_output.contains("action=InstanceStart\n"),
            "boot timer logger output should include InstanceStart action; output:\n{logger_output}"
        );
        assert!(
            logger_output.contains("Guest-boot-time ="),
            "boot timer logger output should include Firecracker-shaped boot time; output:\n{logger_output}"
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang direct rootfs boot timer",
        );
    }

    #[test]
    fn signed_executable_exposes_virtio_balloon_to_direct_rootfs_guest() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let metrics_path = test_dir.path().join("metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config balloon direct rootfs",
        );

        let metrics_body = format!(
            r#"{{"metrics_path":{}}}"#,
            json_string(path_text(&metrics_path))
        );
        let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
        assert_no_content_response(&metrics_response, "PUT /metrics balloon direct rootfs");

        let balloon_response = http_put_json(
            &socket_path,
            "/balloon",
            r#"{"amount_mib":8,"deflate_on_oom":false,"stats_polling_interval_s":1,"free_page_hinting":true,"free_page_reporting":true}"#,
        );
        assert_no_content_response(&balloon_response, "PUT /balloon direct rootfs");

        let configured_balloon = http_get(&socket_path, "/balloon");
        assert_ok_response(&configured_balloon, "GET /balloon direct rootfs");
        for expected in [
            r#""amount_mib":8"#,
            r#""deflate_on_oom":false"#,
            r#""stats_polling_interval_s":1"#,
            r#""free_page_hinting":true"#,
            r#""free_page_reporting":true"#,
        ] {
            assert_response_contains(&configured_balloon, expected, "GET /balloon direct rootfs");
        }

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after PUT /balloon");
        assert_response_contains(
            &vm_config,
            r#""balloon":"#,
            "GET /vm/config after PUT /balloon",
        );
        assert_response_contains(
            &vm_config,
            r#""amount_mib":8"#,
            "GET /vm/config after PUT /balloon",
        );
        assert_response_contains(
            &vm_config,
            r#""deflate_on_oom":false"#,
            "GET /vm/config after PUT /balloon",
        );
        assert_response_contains(
            &vm_config,
            r#""stats_polling_interval_s":1"#,
            "GET /vm/config after PUT /balloon",
        );
        assert_response_contains(
            &vm_config,
            r#""free_page_hinting":true"#,
            "GET /vm/config after PUT /balloon",
        );
        assert_response_contains(
            &vm_config,
            r#""free_page_reporting":true"#,
            "GET /vm/config after PUT /balloon",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_BALLOON_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source balloon direct rootfs");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs balloon direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data balloon direct rootfs",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart balloon direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after balloon direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after balloon direct rootfs InstanceStart",
        );

        let balloon_stats = match wait_for_nonzero_balloon_actual_pages(
            &socket_path,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            Ok(response) => response,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "direct rootfs guest did not inflate the configured balloon: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        for expected in [
            r#""target_pages":2048"#,
            r#""target_mib":8"#,
            r#""actual_mib":"#,
        ] {
            assert_response_contains(
                &balloon_stats,
                expected,
                "GET /balloon/statistics direct rootfs",
            );
        }

        let optional_statistics = match wait_for_balloon_optional_statistics(
            &socket_path,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            Ok(statistics) => statistics,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "direct rootfs guest did not publish optional balloon statistics: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        let polling_update = http_json(
            &socket_path,
            "PATCH",
            "/balloon/statistics",
            r#"{"stats_polling_interval_s":2}"#,
        );
        assert_no_content_response(&polling_update, "PATCH /balloon/statistics direct rootfs");
        let updated_balloon = http_get(&socket_path, "/balloon");
        assert_ok_response(
            &updated_balloon,
            "GET /balloon after statistics polling update direct rootfs",
        );
        assert_response_contains(
            &updated_balloon,
            r#""stats_polling_interval_s":2"#,
            "GET /balloon after statistics polling update direct rootfs",
        );
        let statistics_after_polling_update = http_get(&socket_path, "/balloon/statistics");
        assert_eq!(
            balloon_optional_statistics(&statistics_after_polling_update)
                .expect("updated balloon statistics response should be valid"),
            optional_statistics,
            "updating the polling interval must preserve exactly the optional fields already reported by the guest"
        );

        let hinting_status = http_get(&socket_path, "/balloon/hinting/status");
        assert_ok_response(&hinting_status, "GET /balloon/hinting/status direct rootfs");
        for expected in [r#""host_cmd":0"#, r#""guest_cmd":null"#] {
            assert_response_contains(
                &hinting_status,
                expected,
                "GET /balloon/hinting/status direct rootfs",
            );
        }

        let hinting_start = http_json(
            &socket_path,
            "PATCH",
            "/balloon/hinting/start",
            r#"{"acknowledge_on_stop":true}"#,
        );
        assert_no_content_response(&hinting_start, "PATCH /balloon/hinting/start direct rootfs");
        let automatically_acknowledged_hinting = wait_for_balloon_hinting_status(
            &socket_path,
            1,
            Some(0),
            GUEST_EXECUTION_TIMEOUT,
        )
        .unwrap_or_else(|err| {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete the acknowledged hinting run: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        });
        assert_ok_response(
            &automatically_acknowledged_hinting,
            "GET /balloon/hinting/status after automatic acknowledgement direct rootfs",
        );

        let hinting_stop = http_no_body(&socket_path, "PATCH", "/balloon/hinting/stop");
        assert_no_content_response(&hinting_stop, "PATCH /balloon/hinting/stop direct rootfs");
        let stopped_hinting_status = http_get(&socket_path, "/balloon/hinting/status");
        assert_ok_response(
            &stopped_hinting_status,
            "GET /balloon/hinting/status after stop direct rootfs",
        );
        for expected in [r#""host_cmd":1"#, r#""guest_cmd":0"#] {
            assert_response_contains(
                &stopped_hinting_status,
                expected,
                "GET /balloon/hinting/status after stop direct rootfs",
            );
        }

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_BALLOON_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not observe virtio-balloon through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = wait_for_nonzero_balloon_free_page_report_count(
            &socket_path,
            &metrics_path,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let metrics = fs::read_to_string(&metrics_path)
                .unwrap_or_else(|read_err| format!("<metrics unavailable: {read_err}>"));
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not execute virtio-balloon free-page reporting: {err}; metrics:\n{metrics}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before balloon capture-ready preflight",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused MMIO balloon capture-ready preflight",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after balloon capture-ready preflight",
        );

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/balloon", r#"{"amount_mib":0}"#),
            "PATCH /balloon target to zero direct rootfs",
        );
        wait_for_balloon_page_counts(&socket_path, 0, 0, GUEST_EXECUTION_TIMEOUT).unwrap_or_else(
            |err| {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "direct rootfs guest did not deflate the balloon to zero: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            },
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang balloon direct rootfs",
        );
    }

    #[test]
    fn signed_executable_hotplugs_memory_from_direct_rootfs_guest() {
        const MIB: u64 = 1024 * 1024;

        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let metrics_path = test_dir.path().join("metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config memory hotplug direct rootfs",
        );

        let metrics_body = format!(
            r#"{{"metrics_path":{}}}"#,
            json_string(path_text(&metrics_path))
        );
        let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
        assert_no_content_response(
            &metrics_response,
            "PUT /metrics memory hotplug direct rootfs",
        );

        let memory_hotplug_response = http_put_json(
            &socket_path,
            "/hotplug/memory",
            r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
        );
        assert_no_content_response(
            &memory_hotplug_response,
            "PUT /hotplug/memory direct rootfs",
        );

        let configured_memory_hotplug = http_get(&socket_path, "/hotplug/memory");
        assert_ok_response(
            &configured_memory_hotplug,
            "GET /hotplug/memory direct rootfs",
        );
        assert_response_contains(
            &configured_memory_hotplug,
            r#"{"block_size_mib":2,"plugged_size_mib":0,"requested_size_mib":0,"slot_size_mib":128,"total_size_mib":128}"#,
            "GET /hotplug/memory direct rootfs",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(
            &boot_response,
            "PUT /boot-source memory hotplug direct rootfs",
        );

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(
            &rootfs_response,
            "PUT /drives/rootfs memory hotplug direct rootfs",
        );

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data memory hotplug direct rootfs",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart memory hotplug direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after memory hotplug direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after memory hotplug direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_READY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not report virtio-mem readiness through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let memory_hotplug_update = http_json_with_io_timeout(
            &socket_path,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":128}"#,
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_no_content_response(
            &memory_hotplug_update,
            "PATCH /hotplug/memory direct rootfs",
        );

        let updated_memory_hotplug = http_get(&socket_path, "/hotplug/memory");
        assert_ok_response(
            &updated_memory_hotplug,
            "GET /hotplug/memory after PATCH direct rootfs",
        );
        assert_response_contains(
            &updated_memory_hotplug,
            r#""requested_size_mib":128"#,
            "GET /hotplug/memory after PATCH direct rootfs",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_GROWN_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not observe runtime virtio-mem grow request through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let plugged_memory_hotplug = match wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":128"#,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            Ok(response) => response,
            Err(err) => {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "public API did not report the guest-completed virtio-mem grow: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        assert_ok_response(
            &plugged_memory_hotplug,
            "GET /hotplug/memory after guest-completed grow",
        );
        assert_response_contains(
            &plugged_memory_hotplug,
            r#""requested_size_mib":128"#,
            "GET /hotplug/memory after guest-completed grow",
        );

        let grow_metrics = flush_memory_hotplug_metrics(
            &socket_path,
            &metrics_path,
            "FlushMetrics after virtio-mem grow",
        );
        assert_eq!(grow_metrics["plug_bytes"].as_u64(), Some(128 * MIB));
        assert!(
            grow_metrics["plug_count"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert_eq!(grow_metrics["plug_fails"].as_u64(), Some(0));
        assert!(
            grow_metrics["queue_event_count"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before memory-hotplug capture-ready preflight",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused MMIO memory-hotplug capture-ready preflight",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after memory-hotplug capture-ready preflight",
        );

        let memory_hotplug_shrink = http_json_with_io_timeout(
            &socket_path,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":0}"#,
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_no_content_response(
            &memory_hotplug_shrink,
            "PATCH /hotplug/memory shrink direct rootfs",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not observe runtime virtio-mem shrink request through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let unplugged_memory_hotplug = match wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":0"#,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            Ok(response) => response,
            Err(err) => {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "public API did not report the guest-completed virtio-mem shrink: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        assert_ok_response(
            &unplugged_memory_hotplug,
            "GET /hotplug/memory after guest-completed shrink",
        );
        assert_response_contains(
            &unplugged_memory_hotplug,
            r#""requested_size_mib":0"#,
            "GET /hotplug/memory after guest-completed shrink",
        );
        let shrink_metrics = flush_memory_hotplug_metrics(
            &socket_path,
            &metrics_path,
            "FlushMetrics after virtio-mem shrink",
        );
        assert_eq!(shrink_metrics["unplug_bytes"].as_u64(), Some(128 * MIB));
        assert!(
            shrink_metrics["unplug_count"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert_eq!(shrink_metrics["unplug_fails"].as_u64(), Some(0));
        assert_eq!(shrink_metrics["plug_bytes"].as_u64(), Some(0));

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang memory hotplug direct rootfs",
        );
    }

    #[test]
    fn signed_executable_exposes_rtc_to_direct_rootfs_guest() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config RTC direct rootfs");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_RTC_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source RTC direct rootfs");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs RTC direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(&data_drive_response, "PUT /drives/data RTC direct rootfs");

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart RTC direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after RTC direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after RTC direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_RTC_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not observe PL031 RTC through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang RTC direct rootfs",
        );
    }

    #[test]
    fn signed_executable_exposes_vmclock_to_direct_rootfs_guest() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config VMClock direct rootfs",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_VMCLOCK_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source VMClock direct rootfs");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs VMClock direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data VMClock direct rootfs",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart VMClock direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after VMClock direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after VMClock direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_VMCLOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not observe VMClock through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang VMClock direct rootfs",
        );
    }

    #[test]
    fn signed_executable_flushes_writeback_block_from_direct_rootfs_guest() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);
        create_empty_file(&serial_output_path);
        let expected_device_id = expected_block_device_id(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config writeback block direct rootfs",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(&format!(
            "{DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS} bangbang.block-serial=vdb"
        ));
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(
            &boot_response,
            "PUT /boot-source writeback block direct rootfs",
        );

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(
            &rootfs_response,
            "PUT /drives/rootfs writeback block direct rootfs",
        );

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false,
                "cache_type":"Writeback"
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data writeback block direct rootfs",
        );

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after writeback block drive");
        for expected in [
            r#""drive_id":"data""#,
            r#""cache_type":"Writeback""#,
            r#""is_read_only":false"#,
        ] {
            assert_response_contains(
                &vm_config,
                expected,
                "GET /vm/config after writeback block drive",
            );
        }
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{data_backing_path_json}"#),
            "GET /vm/config after writeback block drive",
        );

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(
            &serial_response,
            "PUT /serial writeback block device identity",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart writeback block direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after writeback block direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after writeback block direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_WRITEBACK_FLUSH_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not flush writeback block drive through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_SERIAL_END_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not report the MMIO Sync block identity; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_block_serial_report(&serial_output_path, &expected_device_id);

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang writeback block direct rootfs",
        );
    }

    #[test]
    fn signed_executable_replaces_macos_block_special_backings_over_product_mmio() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let regular_backing_path = test_dir.path().join("middle-regular.img");
        let control_backing_path = test_dir.path().join("lifecycle-control.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        let mut first_media = MacosVirtualBlock::create(MacosVirtualBlockAccess::ReadWrite)
            .expect("first direct MMIO block media should attach");
        let mut final_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::EightMib,
        )
        .expect("final direct MMIO block media should attach");
        let mut read_only_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::FourMib,
        )
        .expect("read-only audit media should attach for seeding");
        write_virtual_block_marker_at(&first_media, 0, BLOCK_LIFECYCLE_HOST_ONE_MARKER);
        write_virtual_block_marker_at(&final_media, 0, BLOCK_LIFECYCLE_HOST_THREE_MARKER);
        write_virtual_block_marker_at(&read_only_media, 0, BLOCK_LIFECYCLE_READ_ONLY_MARKER);
        read_only_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("audit media should reattach read-only before launch");
        create_block_backing_with_prefix(
            &regular_backing_path,
            12_288,
            BLOCK_LIFECYCLE_HOST_TWO_MARKER,
        );
        create_zeroed_block_backing(&control_backing_path);
        create_empty_file(&serial_output_path);

        let first_path = first_media
            .device_path()
            .expect("first media should expose its exact node")
            .to_path_buf();
        let final_path = final_media
            .device_path()
            .expect("final media should expose its exact node")
            .to_path_buf();
        let read_only_path = read_only_media
            .device_path()
            .expect("read-only media should expose its exact node")
            .to_path_buf();
        let expected_initial_device_id = expected_block_device_id(&first_path);
        let first_identity = first_media
            .identity()
            .expect("first direct MMIO media should expose exact identity");
        assert_ne!(first_identity.device(), 0);
        assert_ne!(first_identity.inode(), 0);
        assert_ne!(first_identity.target_device(), 0);
        assert_eq!(
            first_media
                .logical_block_size()
                .expect("first direct MMIO media should expose block size") as u64
                * first_media
                    .block_count()
                    .expect("first direct MMIO media should expose block count"),
            first_media
                .len()
                .expect("first direct MMIO media should expose checked capacity"),
        );

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT direct MMIO block-special machine config",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(DIRECT_ROOTFS_BLOCK_LIFECYCLE_THREE_BOOT_ARGS),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT direct MMIO block-special boot source",
        );
        for (route, body, context) in [
            (
                "/drives/rootfs",
                format!(
                    r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
                    json_string(path_text(&rootfs_path))
                ),
                "PUT direct MMIO block-special rootfs",
            ),
            (
                "/drives/data",
                format!(
                    r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Sync"}}"#,
                    json_string(path_text(&first_path))
                ),
                "PUT direct MMIO first block-special data drive",
            ),
            (
                "/drives/auditro",
                format!(
                    r#"{{"drive_id":"auditro","path_on_host":{},"is_root_device":false,"is_read_only":true,"cache_type":"Unsafe","io_engine":"Async"}}"#,
                    json_string(path_text(&read_only_path))
                ),
                "PUT direct MMIO read-only block-special audit drive",
            ),
            (
                "/drives/control",
                format!(
                    r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
                    json_string(path_text(&control_backing_path))
                ),
                "PUT direct MMIO lifecycle control drive",
            ),
        ] {
            assert_no_content_response(&http_put_json(&socket_path, route, &body), context);
        }
        let serial_body = format!(
            r#"{{"serial_out_path":{}}}"#,
            json_string(path_text(&serial_output_path))
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/serial", &serial_body),
            "PUT direct MMIO block-special serial output",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "start direct MMIO block-special lifecycle guest",
        );

        wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_LIFECYCLE_LIMITER_READY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("direct MMIO guest should become ready for the live limiter patch");
        assert_no_content_response(
            &http_json(
                &socket_path,
                "PATCH",
                "/drives/data",
                r#"{"drive_id":"data","rate_limiter":{"ops":{"size":1,"refill_time":100}}}"#,
            ),
            "PATCH direct MMIO block-special limiter after guest probe",
        );
        write_block_marker_at(
            &control_backing_path,
            0,
            BLOCK_LIFECYCLE_LIMITER_CONTINUE_MARKER,
        );

        if let Err(error) = wait_for_virtual_block_marker(
            &first_media,
            BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
            BLOCK_LIFECYCLE_GUEST_ONE_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let serial_tail = file_tail_lossy(&serial_output_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct MMIO guest did not complete the first block-special phase: {error}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_LIFECYCLE_PHASE_ONE_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("direct MMIO guest should publish phase one");
        assert_phase_block_serial_report(
            &serial_output_path,
            BLOCK_LIFECYCLE_INITIAL_SERIAL_BEGIN_MARKER,
            BLOCK_LIFECYCLE_INITIAL_SERIAL_END_MARKER,
            &expected_initial_device_id,
            "direct MMIO startup block-special drive",
        );

        let invalid_regular = format!(
            r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async"}}"#,
            json_string(path_text(test_dir.path()))
        );
        assert_bad_request_response(
            &http_put_json(&socket_path, "/drives/data", &invalid_regular),
            "reject direct MMIO directory replacement",
        );
        let before_regular = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &before_regular,
            "GET config after failed block-to-regular replacement",
        );
        assert_response_contains(
            &before_regular,
            &format!(r#""path_on_host":{}"#, json_string(path_text(&first_path))),
            "failed block-to-regular replacement keeps the first block media",
        );

        let regular_replacement = format!(
            r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async"}}"#,
            json_string(path_text(&regular_backing_path))
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/data", &regular_replacement),
            "replace direct MMIO block backing with regular backing",
        );
        wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_LIFECYCLE_PHASE_TWO_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("guest should observe the regular replacement capacity");
        assert_eq!(
            file_bytes_at(
                &regular_backing_path,
                BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
                BLOCK_LIFECYCLE_GUEST_TWO_MARKER.len(),
            ),
            BLOCK_LIFECYCLE_GUEST_TWO_MARKER,
        );

        let invalid_block = format!(
            r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async"}}"#,
            json_string(path_text(&read_only_path))
        );
        assert_bad_request_response(
            &http_put_json(&socket_path, "/drives/data", &invalid_block),
            "reject direct MMIO access-mismatched block replacement",
        );
        let before_final = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &before_final,
            "GET config after failed regular-to-block replacement",
        );
        assert_response_contains(
            &before_final,
            &format!(
                r#""path_on_host":{}"#,
                json_string(path_text(&regular_backing_path))
            ),
            "failed regular-to-block replacement keeps the regular backing",
        );

        let final_replacement = format!(
            r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async"}}"#,
            json_string(path_text(&final_path))
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/data", &final_replacement),
            "replace direct MMIO regular backing with final block backing",
        );
        if let Err(error) = wait_for_virtual_block_marker(
            &final_media,
            BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
            BLOCK_LIFECYCLE_GUEST_THREE_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let serial_tail = file_tail_lossy(&serial_output_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct MMIO guest did not complete the final block-special phase: {error}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_LIFECYCLE_SUCCESS_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("direct MMIO guest should complete block-special lifecycle");
        let final_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&final_config, "GET final direct MMIO block-special config");
        for expected in [
            r#""drive_id":"data""#,
            r#""cache_type":"Writeback""#,
            r#""io_engine":"Async""#,
        ] {
            assert_response_contains(&final_config, expected, "final direct MMIO data drive");
        }
        assert_response_contains(
            &final_config,
            &format!(r#""path_on_host":{}"#, json_string(path_text(&final_path))),
            "final direct MMIO block path",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "direct MMIO block-special lifecycle",
        );
        first_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("first direct MMIO media should release for persistence inspection");
        final_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("final direct MMIO media should release for persistence inspection");
        assert_eq!(
            first_media
                .read_at(
                    BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
                    BLOCK_LIFECYCLE_GUEST_ONE_MARKER.len(),
                )
                .expect("first guest marker should persist"),
            BLOCK_LIFECYCLE_GUEST_ONE_MARKER,
        );
        assert_eq!(
            final_media
                .read_at(
                    BLOCK_LIFECYCLE_GUEST_MARKER_OFFSET,
                    BLOCK_LIFECYCLE_GUEST_THREE_MARKER.len(),
                )
                .expect("final guest marker should persist"),
            BLOCK_LIFECYCLE_GUEST_THREE_MARKER,
        );
        assert_eq!(
            read_only_media
                .read_at(0, BLOCK_LIFECYCLE_READ_ONLY_MARKER.len())
                .expect("read-only audit marker should remain readable"),
            BLOCK_LIFECYCLE_READ_ONLY_MARKER,
        );
        first_media
            .cleanup()
            .expect("first direct MMIO media should clean up exactly");
        final_media
            .cleanup()
            .expect("final direct MMIO media should clean up exactly");
        read_only_media
            .cleanup()
            .expect("read-only direct MMIO media should clean up exactly");
    }

    #[test]
    fn signed_executable_reads_entropy_from_direct_rootfs_guest() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config entropy direct rootfs",
        );

        let entropy_response = http_put_json(&socket_path, "/entropy", "{}");
        assert_no_content_response(&entropy_response, "PUT /entropy direct rootfs");

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after PUT /entropy");
        assert_response_contains(
            &vm_config,
            r#""entropy":{}"#,
            "GET /vm/config after PUT /entropy",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_ENTROPY_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source entropy direct rootfs");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs entropy direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data entropy direct rootfs",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart entropy direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after entropy direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after entropy direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_ENTROPY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not read entropy through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang entropy direct rootfs",
        );
    }

    #[test]
    fn signed_executable_captures_throttled_entropy_lifecycle_over_mmio() {
        run_signed_entropy_capture_lifecycle(false);
    }

    #[test]
    fn signed_executable_captures_throttled_entropy_lifecycle_over_product_pci() {
        run_signed_entropy_capture_lifecycle(true);
    }

    fn run_signed_entropy_capture_lifecycle(enable_pci: bool) {
        let transport = if enable_pci { "product PCI" } else { "MMIO" };
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("entropy-control.img");
        let metrics_path = test_dir.path().join("entropy-metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing_with_sectors(&data_backing_path, 4);
        let mut bangbang = if enable_pci {
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"])
        } else {
            BangbangProcess::start(&socket_path, &instance_id)
        };

        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            &format!("PUT /machine-config entropy lifecycle {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/entropy",
                r#"{"rate_limiter":{"bandwidth":{"size":64,"one_time_burst":256,"refill_time":2000},"ops":{"size":1,"one_time_burst":4,"refill_time":2000}}}"#,
            ),
            &format!("PUT /entropy lifecycle {transport}"),
        );
        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &vm_config,
            &format!("GET /vm/config entropy lifecycle {transport}"),
        );
        assert_response_contains(
            &vm_config,
            r#""bandwidth":{"one_time_burst":256,"refill_time":2000,"size":64}"#,
            &format!("GET /vm/config entropy bandwidth lifecycle {transport}"),
        );
        assert_response_contains(
            &vm_config,
            r#""ops":{"one_time_burst":4,"refill_time":2000,"size":1}"#,
            &format!("GET /vm/config entropy ops lifecycle {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/metrics",
                &format!(
                    r#"{{"metrics_path":{}}}"#,
                    json_string(path_text(&metrics_path))
                ),
            ),
            &format!("PUT /metrics entropy lifecycle {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/boot-source",
                &format!(
                    r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
                    json_string(path_text(&kernel_path)),
                    json_string(DIRECT_ROOTFS_ENTROPY_LIFECYCLE_BOOT_ARGS)
                ),
            ),
            &format!("PUT /boot-source entropy lifecycle {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/drives/rootfs",
                &format!(
                    r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
                    json_string(path_text(&rootfs_path))
                ),
            ),
            &format!("PUT /drives/rootfs entropy lifecycle {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/drives/data",
                &format!(
                    r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
                    json_string(path_text(&data_backing_path))
                ),
            ),
            &format!("PUT /drives/data entropy lifecycle {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            &format!("PUT /actions InstanceStart entropy lifecycle {transport}"),
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_ENTROPY_LIFECYCLE_READY_MARKER,
            ENTROPY_LIFECYCLE_TIMEOUT,
        ) {
            let prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} entropy guest did not reach lifecycle readiness: {err}; control prefix: {prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"FlushMetrics"}"#,
            ),
            &format!("baseline FlushMetrics entropy lifecycle {transport}"),
        );
        let first_lifecycle_metric_line = metrics_line_count(&metrics_path);
        write_block_marker_at(
            &data_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            DIRECT_ROOTFS_ENTROPY_HOST_CONTINUE_MARKER,
        );

        let throttled = wait_for_entropy_metric_since(
            &socket_path,
            &metrics_path,
            first_lifecycle_metric_line,
            "entropy_rate_limiter_throttled",
            1,
            ENTROPY_LIFECYCLE_TIMEOUT,
        )
        .unwrap_or_else(|err| {
            let prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} entropy lifecycle did not throttle after host continuation: {err}; control prefix: {prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        });
        assert!(throttled >= 1);

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            &format!("pause throttled entropy lifecycle {transport}"),
        );
        let paused = http_get(&socket_path, "/");
        assert_ok_response(
            &paused,
            &format!("GET paused entropy lifecycle {transport}"),
        );
        assert_response_contains(
            &paused,
            r#""state":"Paused""#,
            &format!("GET paused entropy lifecycle {transport}"),
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            &format!("paused {transport} entropy capture-ready preflight"),
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            &format!("resume throttled entropy lifecycle {transport}"),
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_ENTROPY_LIFECYCLE_SUCCESS_MARKER,
            ENTROPY_LIFECYCLE_TIMEOUT,
        ) {
            let prefix = file_prefix_lossy(&data_backing_path, 128);
            let metrics = fs::read_to_string(&metrics_path)
                .unwrap_or_else(|metrics_err| format!("<metrics unavailable: {metrics_err}>"));
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} entropy guest did not complete repeated reads after resume: {err}; control prefix: {prefix:?}; metrics:\n{metrics}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        let retry_events = wait_for_entropy_metric_since(
            &socket_path,
            &metrics_path,
            first_lifecycle_metric_line,
            "rate_limiter_event_count",
            1,
            ENTROPY_LIFECYCLE_TIMEOUT,
        )
        .expect("resumed entropy lifecycle should publish a retry-event metric");
        assert!(retry_events >= 1);

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            &format!("bangbang entropy lifecycle {transport}"),
        );
    }

    #[test]
    fn signed_executable_certifies_remaining_devices_over_mmio() {
        run_signed_remaining_device_certification(false);
    }

    #[test]
    fn signed_executable_certifies_remaining_devices_over_product_pci() {
        run_signed_remaining_device_certification(true);
    }

    fn run_signed_remaining_device_certification(enable_pci: bool) {
        const MIB: u64 = 1024 * 1024;

        let transport = if enable_pci { "product PCI" } else { "MMIO" };
        let transport_arg = if enable_pci { "pci" } else { "mmio" };
        let transport_marker = format!(
            "BANGBANG_REMAINING_DEVICE_TRANSPORT_{}_OK",
            transport_arg.to_ascii_uppercase()
        );
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_path = test_dir.path().join("remaining-device-control.img");
        let metrics_path = test_dir.path().join("remaining-device-metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing_with_sectors(&control_path, 8);

        let configure = |context: &str| {
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/machine-config",
                    r#"{"vcpu_count":1,"mem_size_mib":256}"#,
                ),
                &format!("PUT /machine-config remaining-device {context}"),
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/balloon",
                    r#"{"amount_mib":8,"deflate_on_oom":true,"stats_polling_interval_s":1,"free_page_hinting":true,"free_page_reporting":true}"#,
                ),
                &format!("PUT /balloon remaining-device {context}"),
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/hotplug/memory",
                    r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
                ),
                &format!("PUT /hotplug/memory remaining-device {context}"),
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/entropy",
                    r#"{"rate_limiter":{"bandwidth":{"size":64,"one_time_burst":256,"refill_time":2000},"ops":{"size":1,"one_time_burst":4,"refill_time":2000}}}"#,
                ),
                &format!("PUT /entropy remaining-device {context}"),
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/metrics",
                    &format!(
                        r#"{{"metrics_path":{}}}"#,
                        json_string(path_text(&metrics_path))
                    ),
                ),
                &format!("PUT /metrics remaining-device {context}"),
            );
            let boot_args = format!(
                "{DIRECT_ROOTFS_REMAINING_DEVICE_BOOT_ARGS} bangbang.expect-remaining-device-transport={transport_arg}"
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/boot-source",
                    &format!(
                        r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
                        json_string(path_text(&kernel_path)),
                        json_string(&boot_args)
                    ),
                ),
                &format!("PUT /boot-source remaining-device {context}"),
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/drives/rootfs",
                    &format!(
                        r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
                        json_string(path_text(&rootfs_path))
                    ),
                ),
                &format!("PUT /drives/rootfs remaining-device {context}"),
            );
            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/drives/control",
                    &format!(
                        r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
                        json_string(path_text(&control_path))
                    ),
                ),
                &format!("PUT /drives/control remaining-device {context}"),
            );

            let vm_config = http_get(&socket_path, "/vm/config");
            assert_ok_response(
                &vm_config,
                &format!("GET /vm/config remaining-device {context}"),
            );
            for expected in [
                r#""vcpu_count":1"#,
                r#""amount_mib":8"#,
                r#""deflate_on_oom":true"#,
                r#""free_page_hinting":true"#,
                r#""free_page_reporting":true"#,
                r#""total_size_mib":128"#,
                r#""bandwidth":{"one_time_burst":256,"refill_time":2000,"size":64}"#,
                r#""ops":{"one_time_burst":4,"refill_time":2000,"size":1}"#,
            ] {
                assert_response_contains(
                    &vm_config,
                    expected,
                    &format!("GET /vm/config remaining-device {context}"),
                );
            }
        };

        let mut bangbang = if enable_pci {
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"])
        } else {
            BangbangProcess::start(&socket_path, &instance_id)
        };
        configure(transport);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            &format!("start remaining-device {transport}"),
        );
        bangbang
            .wait_for_stdout_marker(&transport_marker, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("{transport} remaining-device transport should validate: {error}")
            });

        wait_for_file_prefix_marker(
            &control_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .unwrap_or_else(|error| {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} remaining-device virtio-mem should become ready: {error}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            )
        });

        let rejected_memory = http_json(
            &socket_path,
            "PATCH",
            "/hotplug/memory",
            r#"{"requested_size_mib":127}"#,
        );
        assert_bad_request_response(
            &rejected_memory,
            &format!("reject unaligned remaining-device memory size {transport}"),
        );
        let unchanged_memory = std::panic::catch_unwind(|| http_get(&socket_path, "/hotplug/memory"))
            .unwrap_or_else(|_| {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "{transport} remaining-device process exited after rejected memory update; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                )
            });
        assert_ok_response(
            &unchanged_memory,
            &format!("GET unchanged remaining-device memory {transport}"),
        );
        for expected in [r#""requested_size_mib":0"#, r#""plugged_size_mib":0"#] {
            assert_response_contains(
                &unchanged_memory,
                expected,
                &format!("GET unchanged remaining-device memory {transport}"),
            );
        }

        let (memory_grow, statistics_update) = std::thread::scope(|scope| {
            let memory_grow = scope.spawn(|| {
                http_json_with_io_timeout(
                    &socket_path,
                    "PATCH",
                    "/hotplug/memory",
                    r#"{"requested_size_mib":128}"#,
                    PCI_ALL_VIRTIO_GUEST_TIMEOUT,
                )
            });
            let statistics_update = scope.spawn(|| {
                http_json(
                    &socket_path,
                    "PATCH",
                    "/balloon/statistics",
                    r#"{"stats_polling_interval_s":2}"#,
                )
            });
            (
                memory_grow
                    .join()
                    .expect("remaining-device memory grow request should join"),
                statistics_update
                    .join()
                    .expect("remaining-device balloon statistics request should join"),
            )
        });
        assert_no_content_response(
            &memory_grow,
            &format!("concurrent memory grow remaining-device {transport}"),
        );
        assert_no_content_response(
            &statistics_update,
            &format!("concurrent balloon statistics update remaining-device {transport}"),
        );

        wait_for_file_prefix_marker(
            &control_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_GROWN_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .unwrap_or_else(|error| {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} remaining-device virtio-mem should grow: {error}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            )
        });
        std::panic::catch_unwind(|| {
            wait_for_http_response_fragment(
                &socket_path,
                "/hotplug/memory",
                r#""plugged_size_mib":128"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            )
        })
        .unwrap_or_else(|_| {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} remaining-device process exited after memory grow; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            )
        })
        .expect("remaining-device public memory state should report completed grow");
        let grow_metrics = flush_memory_hotplug_metrics(
            &socket_path,
            &metrics_path,
            &format!("FlushMetrics after remaining-device grow {transport}"),
        );
        assert_eq!(grow_metrics["plug_bytes"].as_u64(), Some(128 * MIB));
        assert_eq!(grow_metrics["plug_fails"].as_u64(), Some(0));

        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":0}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            &format!("shrink remaining-device memory {transport}"),
        );
        wait_for_file_prefix_marker(
            &control_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("remaining-device guest should observe the completed memory shrink");
        std::panic::catch_unwind(|| {
            wait_for_http_response_fragment(
                &socket_path,
                "/hotplug/memory",
                r#""plugged_size_mib":0"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            )
        })
        .unwrap_or_else(|_| {
            let metrics = file_tail_lossy(&metrics_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} remaining-device process exited after memory shrink; status: {:?}\nmetrics:\n{metrics}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr,
            )
        })
        .expect("remaining-device public memory state should report completed shrink");
        let shrink_metrics = flush_memory_hotplug_metrics(
            &socket_path,
            &metrics_path,
            &format!("FlushMetrics after remaining-device shrink {transport}"),
        );
        assert_eq!(shrink_metrics["unplug_bytes"].as_u64(), Some(128 * MIB));
        assert_eq!(shrink_metrics["unplug_fails"].as_u64(), Some(0));

        bangbang
            .wait_for_stdout_marker(
                "BANGBANG_REMAINING_DEVICE_TIME_IDENTITY_OK",
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            )
            .expect("remaining-device guest should validate time and identity devices");
        bangbang
            .wait_for_stdout_marker(
                "BANGBANG_ENTROPY_LIFECYCLE_READY",
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            )
            .expect("remaining-device entropy lifecycle should become ready");
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"FlushMetrics"}"#,
            ),
            &format!("baseline remaining-device entropy metrics {transport}"),
        );
        let entropy_metric_line = metrics_line_count(&metrics_path);
        write_block_marker_at(
            &control_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            DIRECT_ROOTFS_ENTROPY_HOST_CONTINUE_MARKER,
        );
        assert!(
            wait_for_entropy_metric_since(
                &socket_path,
                &metrics_path,
                entropy_metric_line,
                "entropy_rate_limiter_throttled",
                1,
                ENTROPY_LIFECYCLE_TIMEOUT,
            )
            .expect("remaining-device entropy reads should reach dual-bucket pressure")
                >= 1
        );
        bangbang
            .wait_for_stdout_marker(
                "BANGBANG_REMAINING_DEVICE_ENTROPY_OK",
                ENTROPY_LIFECYCLE_TIMEOUT,
            )
            .expect("remaining-device entropy reads should complete after retry");
        assert!(
            wait_for_entropy_metric_since(
                &socket_path,
                &metrics_path,
                entropy_metric_line,
                "rate_limiter_event_count",
                1,
                ENTROPY_LIFECYCLE_TIMEOUT,
            )
            .expect("remaining-device entropy retry event should publish")
                >= 1
        );

        if let Err(error) =
            wait_for_nonzero_balloon_actual_pages(&socket_path, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} remaining-device balloon should inflate: {error}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        if enable_pci {
            let core_statistics = http_get(&socket_path, "/balloon/statistics");
            assert_ok_response(
                &core_statistics,
                "GET core remaining-device balloon statistics over product PCI",
            );
            for expected in [r#""actual_pages":2048"#, r#""target_pages":2048"#] {
                assert_response_contains(
                    &core_statistics,
                    expected,
                    "GET core remaining-device balloon statistics over product PCI",
                );
            }
        } else {
            wait_for_balloon_optional_statistics(&socket_path, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
                .expect("MMIO remaining-device balloon should publish optional statistics");
        }
        let hinting_start = http_json(
            &socket_path,
            "PATCH",
            "/balloon/hinting/start",
            r#"{"acknowledge_on_stop":true}"#,
        );
        if !hinting_start.starts_with("HTTP/1.1 204 No Content\r\n") {
            let metrics = file_tail_lossy(&metrics_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "start remaining-device balloon hinting {transport} failed: {hinting_start}\nstatus: {:?}\nmetrics:\n{metrics}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr,
            );
        }
        wait_for_balloon_hinting_status(&socket_path, 1, Some(0), PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("remaining-device balloon hinting should be acknowledged");
        assert_no_content_response(
            &http_no_body(&socket_path, "PATCH", "/balloon/hinting/stop"),
            &format!("stop remaining-device balloon hinting {transport}"),
        );
        wait_for_nonzero_balloon_free_page_report_count(
            &socket_path,
            &metrics_path,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("remaining-device balloon reporting should execute");

        bangbang
            .wait_for_stdout_marker(
                REMAINING_DEVICE_SERIAL_READY_MARKER,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            )
            .expect("remaining-device serial input should become ready");
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            &format!("pause remaining-device guest {transport}"),
        );
        let mut serial_input = REMAINING_DEVICE_SERIAL_INPUT.as_bytes().to_vec();
        serial_input.push(b'\n');
        assert!(
            serial_input.len() > bangbang_runtime::serial::SERIAL_RECEIVE_FIFO_CAPACITY,
            "remaining-device input must cross the bounded UART FIFO"
        );
        bangbang.write_stdin(&serial_input);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !bangbang
                .stdout_snapshot()
                .contains(REMAINING_DEVICE_SERIAL_SUCCESS_MARKER),
            "paused {transport} remaining-device guest must not consume queued serial input"
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            &format!("paused aggregate remaining-device capture-ready preflight {transport}"),
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            &format!("resume remaining-device guest {transport}"),
        );
        if let Err(error) = bangbang
            .wait_for_stdout_marker(REMAINING_DEVICE_SUCCESS_MARKER, GUEST_EXECUTION_TIMEOUT)
        {
            let _ = http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"FlushMetrics"}"#,
            );
            let metrics = file_tail_lossy(&metrics_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "{transport} remaining-device guest should finish its aggregate contract: {error}; status: {:?}\nmetrics:\n{metrics}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr,
            );
        }
        let aggregate_stdout = bangbang.stdout_snapshot();
        assert!(aggregate_stdout.contains(REMAINING_DEVICE_SERIAL_SUCCESS_MARKER));
        assert!(aggregate_stdout.contains("BANGBANG_REMAINING_DEVICE_PVTIME_STEAL="));
        assert!(!aggregate_stdout.contains(REMAINING_DEVICE_FAILURE_MARKER));
        assert_eq!(
            file_bytes_at(
                &control_path,
                REMAINING_DEVICE_FINAL_MARKER_OFFSET,
                REMAINING_DEVICE_SUCCESS_MARKER.len(),
            ),
            REMAINING_DEVICE_SUCCESS_MARKER.as_bytes(),
        );

        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/balloon",
                r#"{"amount_mib":0}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            &format!("deflate remaining-device balloon {transport}"),
        );
        wait_for_balloon_page_counts(&socket_path, 0, 0, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("remaining-device balloon should deflate to zero");
        bangbang.close_stdin();
        std::thread::sleep(Duration::from_millis(100));
        let after_eof = http_get(&socket_path, "/");
        assert_ok_response(
            &after_eof,
            &format!("GET / after remaining-device serial EOF {transport}"),
        );
        assert_response_contains(
            &after_eof,
            r#""state":"Running""#,
            &format!("GET / after remaining-device serial EOF {transport}"),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"FlushMetrics"}"#,
            ),
            &format!("final remaining-device metrics flush {transport}"),
        );
        let metrics_output = fs::read_to_string(&metrics_path)
            .expect("remaining-device metrics output should be readable");
        let final_metrics: serde_json::Value = serde_json::from_str(
            metrics_output
                .lines()
                .next_back()
                .expect("remaining-device metrics should contain a final generation"),
        )
        .expect("remaining-device final metrics generation should be JSON");
        assert_eq!(
            final_metrics.pointer("/uart/input_count"),
            Some(&serde_json::json!(serial_input.len())),
        );
        assert_eq!(
            final_metrics.pointer("/uart/error_count"),
            Some(&serde_json::json!(0)),
        );
        assert_eq!(
            final_metrics.pointer("/uart/overrun_count"),
            Some(&serde_json::json!(0)),
        );
        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            &format!("bangbang remaining-device {transport}"),
        );

        reset_zeroed_block_backing(&control_path, 8);
        let reused = if enable_pci {
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"])
        } else {
            BangbangProcess::start(&socket_path, &instance_id)
        };
        configure(&format!("reused {transport}"));
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            &format!("start reused remaining-device {transport}"),
        );
        reused
            .wait_for_stdout_marker(&transport_marker, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("reused remaining-device transport should publish");
        wait_for_file_prefix_marker(
            &control_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("reused remaining-device owner set should reach guest readiness");
        assert_clean_shutdown(
            reused.terminate(),
            &socket_path,
            &format!("reused bangbang remaining-device {transport}"),
        );
    }

    #[test]
    fn signed_executable_runs_all_startup_virtio_devices_over_product_pci() {
        const MIB: u64 = 1024 * 1024;

        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let pmem_backing_path = test_dir.path().join("pmem.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let metrics_path = test_dir.path().join("metrics.out");
        let uds_path = test_dir.path().join("pci-vsock.sock");
        let host_port_path = vsock_port_path(&uds_path, DIRECT_ROOTFS_VSOCK_PORT);
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);
        create_pmem_backing(&pmem_backing_path, PMEM_HOST_MARKER);
        let host_listener = UnixListener::bind(&host_port_path).unwrap_or_else(|err| {
            panic!(
                "product PCI vsock listener should bind before guest startup: {:?}",
                err.kind()
            )
        });
        host_listener
            .set_nonblocking(true)
            .expect("product PCI vsock listener should be nonblocking");

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);

        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT /machine-config product PCI",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/balloon",
                r#"{"amount_mib":8,"deflate_on_oom":false,"free_page_hinting":true,"free_page_reporting":true}"#,
            ),
            "PUT /balloon product PCI",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/hotplug/memory",
                r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
            ),
            "PUT /hotplug/memory product PCI",
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/entropy", "{}"),
            "PUT /entropy product PCI",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"06:00:00:00:00:01"}"#,
            ),
            "PUT /network-interfaces/eth0 product PCI",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/mmds/config",
                r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            ),
            "PUT /mmds/config product PCI",
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/mmds", DIRECT_ROOTFS_MMDS_CONTENT),
            "PUT /mmds product PCI",
        );
        let metrics_body = format!(
            r#"{{"metrics_path":{}}}"#,
            json_string(path_text(&metrics_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/metrics", &metrics_body),
            "PUT /metrics product PCI",
        );
        let serial_body = format!(
            r#"{{"serial_out_path":{}}}"#,
            json_string(path_text(&serial_output_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/serial", &serial_body),
            "PUT /serial product PCI",
        );

        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(DIRECT_ROOTFS_PCI_ALL_VIRTIO_BOOT_ARGS),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT /boot-source product PCI",
        );

        let rootfs_body = format!(
            r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
            json_string(path_text(&rootfs_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/rootfs", &rootfs_body),
            "PUT /drives/rootfs product PCI",
        );
        let data_body = format!(
            r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&data_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/data", &data_body),
            "PUT /drives/data product PCI",
        );
        let pmem_body = format!(
            r#"{{"id":"pmem0","path_on_host":{},"read_only":false}}"#,
            json_string(path_text(&pmem_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/pmem/pmem0", &pmem_body),
            "PUT /pmem/pmem0 product PCI",
        );
        let vsock_body = format!(
            r#"{{"guest_cid":3,"uds_path":{}}}"#,
            json_string(path_text(&uds_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/vsock", &vsock_body),
            "PUT /vsock product PCI",
        );

        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "PUT /actions InstanceStart product PCI",
        );
        let running = http_get(&socket_path, "/");
        assert_ok_response(&running, "GET / after product PCI InstanceStart");
        assert_response_contains(
            &running,
            r#""state":"Running""#,
            "GET / after product PCI InstanceStart",
        );

        let mut host_stream = match wait_for_unix_listener_accept(
            &host_listener,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            Ok(stream) => stream,
            Err(err) => {
                let metrics_flush = http_put_json(
                    &socket_path,
                    "/actions",
                    r#"{"action_type":"FlushMetrics"}"#,
                );
                let metrics = file_tail_lossy(&metrics_path, 16 * 1024);
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let serial_tail = file_tail_lossy(&serial_output_path, 16 * 1024);
                let output = bangbang.force_stop_and_collect();
                let stdout_tail = text_tail_lossy(&output.stdout, 16 * 1024);
                let stderr_tail = text_tail_lossy(&output.stderr, 16 * 1024);
                panic!(
                    "product PCI guest did not initiate vsock I/O: {err}; metrics flush: {metrics_flush:?}; metrics tail:\n{metrics}\nbacking prefix: {backing_prefix:?}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout tail:\n{stdout_tail}\nstderr tail:\n{stderr_tail}",
                    output.status,
                );
            }
        };
        drop(host_listener);
        host_stream
            .set_nonblocking(false)
            .expect("product PCI vsock stream should be blocking");
        host_stream
            .set_read_timeout(Some(PCI_ALL_VIRTIO_GUEST_TIMEOUT))
            .expect("product PCI vsock read timeout should set");
        host_stream
            .set_write_timeout(Some(PCI_ALL_VIRTIO_GUEST_TIMEOUT))
            .expect("product PCI vsock write timeout should set");
        assert_eq!(
            read_and_verify_deterministic_vsock_stream(
                &mut host_stream,
                DIRECT_ROOTFS_VSOCK_GUEST_STREAM_SEED,
            )
            .expect("product PCI guest-to-host vsock stream should verify"),
            DIRECT_ROOTFS_VSOCK_STREAM_BYTES
        );
        assert_eq!(
            write_deterministic_vsock_stream(
                &mut host_stream,
                DIRECT_ROOTFS_VSOCK_HOST_STREAM_SEED,
            )
            .expect("product PCI host-to-guest vsock stream should write"),
            DIRECT_ROOTFS_VSOCK_STREAM_BYTES
        );
        shutdown_unix_stream_write(&host_stream)
            .expect("product PCI host vsock write half should close");
        read_unix_stream_eof(&mut host_stream)
            .expect("product PCI guest should close its vsock stream");

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "product PCI virtio-mem did not become ready: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":128}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "PATCH /hotplug/memory grow product PCI",
        );
        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_MEMORY_HOTPLUG_GROWN_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "product PCI virtio-mem did not grow: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":128"#,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("product PCI public status should report completed virtio-mem grow");
        let grow_metrics = flush_memory_hotplug_metrics(
            &socket_path,
            &metrics_path,
            "FlushMetrics after product PCI virtio-mem grow",
        );
        assert_eq!(grow_metrics["plug_bytes"].as_u64(), Some(128 * MIB));
        assert!(
            grow_metrics["plug_count"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert_eq!(grow_metrics["plug_fails"].as_u64(), Some(0));

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before product PCI memory-hotplug capture-ready preflight",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused product PCI memory-hotplug capture-ready preflight",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after product PCI memory-hotplug capture-ready preflight",
        );

        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":0}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "PATCH /hotplug/memory shrink product PCI",
        );
        wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":0"#,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("product PCI public status should report completed virtio-mem shrink");
        let shrink_metrics = flush_memory_hotplug_metrics(
            &socket_path,
            &metrics_path,
            "FlushMetrics after product PCI virtio-mem shrink",
        );
        assert_eq!(shrink_metrics["unplug_bytes"].as_u64(), Some(128 * MIB));
        assert!(
            shrink_metrics["unplug_count"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert_eq!(shrink_metrics["unplug_fails"].as_u64(), Some(0));
        assert_eq!(shrink_metrics["plug_bytes"].as_u64(), Some(0));

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_PCI_ALL_VIRTIO_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "product PCI guest did not complete all-class interrupt/I/O checks: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &pmem_backing_path,
                PMEM_GUEST_FLUSH_OFFSET,
                PMEM_GUEST_FLUSH_MARKER.len(),
            ),
            PMEM_GUEST_FLUSH_MARKER,
            "product PCI pmem flush should persist the guest marker"
        );
        wait_for_nonzero_balloon_actual_pages(&socket_path, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("product PCI balloon should inflate through queue interrupts");

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before product PCI balloon capture-ready preflight",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused product PCI balloon capture-ready preflight",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after product PCI balloon capture-ready preflight",
        );
        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/balloon",
                r#"{"amount_mib":0}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "PATCH /balloon target to zero product PCI",
        );
        wait_for_balloon_page_counts(&socket_path, 0, 0, PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("product PCI balloon should deflate to zero through queue interrupts");

        assert_no_content_response(
            &http_json(
                &socket_path,
                "PATCH",
                "/drives/data",
                r#"{"drive_id":"data","rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
            ),
            "PATCH /drives/data product PCI",
        );
        assert_no_content_response(
            &http_json(
                &socket_path,
                "PATCH",
                "/network-interfaces/eth0",
                r#"{"iface_id":"eth0","rx_rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
            ),
            "PATCH /network-interfaces/eth0 product PCI",
        );
        assert_no_content_response(
            &http_json(
                &socket_path,
                "PATCH",
                "/pmem/pmem0",
                r#"{"id":"pmem0","rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
            ),
            "PATCH /pmem/pmem0 product PCI",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang all-virtio product PCI",
        );
        assert!(
            !uds_path.exists(),
            "product PCI shutdown should remove the owned main vsock listener"
        );
    }

    #[test]
    fn signed_executable_certifies_aggregate_storage_semantics_over_product_pci() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_backing_path = test_dir.path().join("storage-control.img");
        let async_backing_path = test_dir.path().join("storage-async.img");
        let async_replacement_path = test_dir.path().join("storage-async-replacement.img");
        let vhost_backing_path = test_dir.path().join("storage-vhost.img");
        let vhost_socket_path = test_dir.path().join("storage-vhost.socket");
        let startup_pmem_path = test_dir.path().join("storage-pmem.img");
        let runtime_block_one_path = test_dir.path().join("storage-runtime-block-one.img");
        let runtime_block_two_path = test_dir.path().join("storage-runtime-block-two.img");
        let runtime_pmem_one_path = test_dir.path().join("storage-runtime-pmem-one.img");
        let runtime_pmem_two_path = test_dir.path().join("storage-runtime-pmem-two.img");
        let metrics_path = test_dir.path().join("storage-metrics.out");
        let serial_path = test_dir.path().join("storage-serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_block_backing_with_prefix(&control_backing_path, 16, STORAGE_CONTROL_HOST_MARKER);
        create_block_backing_with_prefix(&async_backing_path, 8, STORAGE_ASYNC_HOST_MARKER);
        create_block_backing_with_prefix(
            &async_replacement_path,
            8,
            STORAGE_ASYNC_REPLACEMENT_HOST_MARKER,
        );
        create_block_backing_with_prefix(&vhost_backing_path, 8, STORAGE_VHOST_HOST_MARKER);
        create_pmem_backing(&startup_pmem_path, STORAGE_PMEM_HOST_MARKER);
        create_block_backing_with_prefix(
            &runtime_block_one_path,
            8,
            STORAGE_RUNTIME_BLOCK_ONE_HOST_MARKER,
        );
        create_block_backing_with_prefix(
            &runtime_block_two_path,
            8,
            STORAGE_RUNTIME_BLOCK_TWO_HOST_MARKER,
        );
        create_pmem_backing(&runtime_pmem_one_path, STORAGE_RUNTIME_PMEM_ONE_HOST_MARKER);
        create_pmem_backing(&runtime_pmem_two_path, STORAGE_RUNTIME_PMEM_TWO_HOST_MARKER);
        create_empty_file(&metrics_path);
        create_empty_file(&serial_path);
        let vhost_backend = VhostUserBlockBackend::start(
            &vhost_socket_path,
            &vhost_backing_path,
            VhostUserBlockBackendOptions::regular(false),
        )
        .expect("aggregate storage vhost-user backend should start");

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT aggregate storage machine config",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/hotplug/memory",
                r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
            ),
            "PUT aggregate storage memory hotplug config",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/boot-source",
                &format!(
                    r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
                    json_string(path_text(&kernel_path)),
                    json_string(DIRECT_ROOTFS_STORAGE_CERTIFICATION_BOOT_ARGS),
                ),
            ),
            "PUT aggregate storage boot source",
        );
        let startup_storage = [
            (
                "/drives/rootfs",
                format!(
                    r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true,"io_engine":"Sync"}}"#,
                    json_string(path_text(&rootfs_path)),
                ),
                "PUT aggregate read-only Sync rootfs",
            ),
            (
                "/drives/control",
                format!(
                    r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Sync"}}"#,
                    json_string(path_text(&control_backing_path)),
                ),
                "PUT aggregate writable Sync control",
            ),
            (
                "/drives/asyncdata",
                format!(
                    r#"{{"drive_id":"asyncdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async"}}"#,
                    json_string(path_text(&async_backing_path)),
                ),
                "PUT aggregate writable Async data",
            ),
            (
                "/drives/vhostdata",
                format!(
                    r#"{{"drive_id":"vhostdata","socket":{},"is_root_device":false,"cache_type":"Writeback"}}"#,
                    json_string(path_text(&vhost_socket_path)),
                ),
                "PUT aggregate writable vhost-user data",
            ),
        ];
        for (route, body, context) in startup_storage {
            assert_no_content_response(&http_put_json(&socket_path, route, &body), context);
        }
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/pmem/pmem0",
                &format!(
                    r#"{{"id":"pmem0","path_on_host":{},"read_only":false}}"#,
                    json_string(path_text(&startup_pmem_path)),
                ),
            ),
            "PUT aggregate writable pmem",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/metrics",
                &format!(
                    r#"{{"metrics_path":{}}}"#,
                    json_string(path_text(&metrics_path))
                ),
            ),
            "PUT aggregate storage metrics",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/serial",
                &format!(
                    r#"{{"serial_out_path":{}}}"#,
                    json_string(path_text(&serial_path))
                ),
            ),
            "PUT aggregate storage serial output",
        );

        let configured = http_get(&socket_path, "/vm/config");
        assert_ok_response(&configured, "GET aggregate startup storage config");
        for expected in [
            r#""drive_id":"rootfs""#,
            r#""drive_id":"control""#,
            r#""drive_id":"asyncdata""#,
            r#""drive_id":"vhostdata""#,
            r#""io_engine":"Sync""#,
            r#""io_engine":"Async""#,
            r#""id":"pmem0""#,
        ] {
            assert_response_contains(&configured, expected, "aggregate startup storage config");
        }
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "start aggregate storage guest",
        );

        if let Err(error) = wait_for_file_markers_at(
            &control_backing_path,
            &[(
                STORAGE_READY_OFFSET,
                STORAGE_READY_MARKER,
                b"BANGBANG_STORAGE_CERTIFICATION_FAIL",
            )],
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let report = vhost_backend.report();
            let failure = String::from_utf8_lossy(&file_bytes_at(
                &control_backing_path,
                15 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                128,
            ))
            .into_owned();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "aggregate storage guest did not finish initial I/O: {error}; failure marker: {failure:?}; serial tail:\n{serial_tail}\nvhost report: {report:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        vhost_backend
            .wait_for_activation(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("aggregate startup vhost-user backend should activate");
        assert_eq!(
            file_bytes_at(
                &control_backing_path,
                STORAGE_CONTROL_GUEST_OFFSET,
                STORAGE_CONTROL_GUEST_MARKER.len(),
            ),
            STORAGE_CONTROL_GUEST_MARKER,
        );
        assert_eq!(
            file_bytes_at(
                &async_backing_path,
                STORAGE_CONTROL_GUEST_OFFSET,
                STORAGE_ASYNC_GUEST_MARKER.len(),
            ),
            STORAGE_ASYNC_GUEST_MARKER,
        );
        assert_eq!(
            file_bytes_at(
                &vhost_backing_path,
                STORAGE_CONTROL_GUEST_OFFSET,
                STORAGE_VHOST_GUEST_MARKER.len(),
            ),
            STORAGE_VHOST_GUEST_MARKER,
        );
        assert_eq!(
            file_bytes_at(
                &startup_pmem_path,
                PMEM_GUEST_FLUSH_OFFSET,
                STORAGE_PMEM_GUEST_MARKER.len(),
            ),
            STORAGE_PMEM_GUEST_MARKER,
        );
        vhost_backend
            .wait_for_flush(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("aggregate startup vhost-user write should flush");

        fs::OpenOptions::new()
            .write(true)
            .open(&vhost_backing_path)
            .expect("aggregate vhost backing should reopen for resize")
            .set_len(16 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("aggregate vhost backing should resize");
        let (async_patch, pmem_patch, vhost_patch) = std::thread::scope(|scope| {
            let async_patch = scope.spawn(|| {
                http_json(
                    &socket_path,
                    "PATCH",
                    "/drives/asyncdata",
                    r#"{"drive_id":"asyncdata","rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
                )
            });
            let pmem_patch = scope.spawn(|| {
                http_json(
                    &socket_path,
                    "PATCH",
                    "/pmem/pmem0",
                    r#"{"id":"pmem0","rate_limiter":{"ops":{"size":3,"one_time_burst":1,"refill_time":100}}}"#,
                )
            });
            let vhost_patch = scope.spawn(|| {
                http_json(
                    &socket_path,
                    "PATCH",
                    "/drives/vhostdata",
                    r#"{"drive_id":"vhostdata"}"#,
                )
            });
            (
                async_patch
                    .join()
                    .expect("aggregate Async PATCH should join"),
                pmem_patch.join().expect("aggregate pmem PATCH should join"),
                vhost_patch
                    .join()
                    .expect("aggregate vhost PATCH should join"),
            )
        });
        assert_no_content_response(&async_patch, "concurrent PATCH aggregate Async limiter");
        assert_no_content_response(&pmem_patch, "concurrent PATCH aggregate pmem limiter");
        assert_no_content_response(&vhost_patch, "concurrent PATCH aggregate vhost config");
        assert_eq!(
            vhost_backend.report().config_requests,
            2,
            "aggregate vhost startup and ID-only refresh should issue two config requests",
        );
        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":128}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "grow aggregate storage memory",
        );
        wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":128"#,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("aggregate storage guest should complete memory grow");

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause aggregate storage guest before replacement",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/drives/asyncdata",
                &format!(
                    r#"{{"drive_id":"asyncdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async","rate_limiter":{{"ops":{{"size":4,"one_time_burst":1,"refill_time":100}}}}}}"#,
                    json_string(path_text(&async_replacement_path)),
                ),
            ),
            "paused same-ID aggregate Async replacement",
        );
        let runtime_block_one = format!(
            r#"{{"drive_id":"runtime_block","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Sync"}}"#,
            json_string(path_text(&runtime_block_one_path)),
        );
        let runtime_pmem_one = format!(
            r#"{{"id":"runtime_pmem","path_on_host":{},"read_only":false}}"#,
            json_string(path_text(&runtime_pmem_one_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/runtime_block", &runtime_block_one),
            "paused PUT first aggregate runtime block",
        );
        let replaced = http_get(&socket_path, "/vm/config");
        assert_ok_response(&replaced, "GET aggregate storage replaced config");
        assert_response_contains(
            &replaced,
            path_text(&async_replacement_path),
            "aggregate Async replacement projection",
        );
        assert!(!replaced.contains(path_text(&async_backing_path)));
        write_block_marker_at(
            &control_backing_path,
            STORAGE_CONTINUE_ONE_OFFSET,
            STORAGE_CONTINUE_ONE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume aggregate storage guest for first runtime round",
        );

        if let Err(error) = wait_for_file_markers_at(
            &control_backing_path,
            &[(
                STORAGE_FIRST_REMOVED_OFFSET,
                STORAGE_FIRST_REMOVED_MARKER,
                b"BANGBANG_STORAGE_CERTIFICATION_FAIL",
            )],
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let report = vhost_backend.report();
            let failure = String::from_utf8_lossy(&file_bytes_at(
                &control_backing_path,
                15 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                128,
            ))
            .into_owned();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "aggregate first runtime storage round failed: {error}; failure marker: {failure:?}; serial tail:\n{serial_tail}\nvhost report: {report:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &async_replacement_path,
                STORAGE_CONTROL_GUEST_OFFSET,
                STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER.len(),
            ),
            STORAGE_ASYNC_REPLACEMENT_GUEST_MARKER,
        );
        assert_eq!(
            file_bytes_at(
                &runtime_block_one_path,
                STORAGE_CONTROL_GUEST_OFFSET,
                STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER.len(),
            ),
            STORAGE_RUNTIME_BLOCK_ONE_GUEST_MARKER,
        );

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause aggregate storage guest before runtime reuse",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/runtime_block"),
            "DELETE first aggregate runtime block",
        );
        let runtime_block_two = format!(
            r#"{{"drive_id":"runtime_block_two","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Sync"}}"#,
            json_string(path_text(&runtime_block_two_path)),
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/drives/runtime_block_two",
                &runtime_block_two,
            ),
            "paused PUT reused aggregate runtime block",
        );
        write_block_marker_at(
            &control_backing_path,
            STORAGE_CONTINUE_TWO_OFFSET,
            STORAGE_CONTINUE_TWO_MARKER,
        );
        assert_eq!(
            file_bytes_at(
                &control_backing_path,
                STORAGE_CONTINUE_TWO_OFFSET,
                STORAGE_CONTINUE_TWO_MARKER.len(),
            ),
            STORAGE_CONTINUE_TWO_MARKER,
            "host must persist the second aggregate control marker before resume",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume aggregate storage guest for reused block round",
        );
        if let Err(error) = wait_for_file_markers_at(
            &control_backing_path,
            &[(
                STORAGE_SECOND_BLOCK_REMOVED_OFFSET,
                STORAGE_SECOND_BLOCK_REMOVED_MARKER,
                b"BANGBANG_STORAGE_CERTIFICATION_FAIL",
            )],
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let failure = String::from_utf8_lossy(&file_bytes_at(
                &control_backing_path,
                15 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                128,
            ))
            .into_owned();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "aggregate reused block round failed: {error}; failure marker: {failure:?}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &runtime_block_two_path,
                STORAGE_CONTROL_GUEST_OFFSET,
                STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER.len(),
            ),
            STORAGE_RUNTIME_BLOCK_TWO_GUEST_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause aggregate storage guest before first pmem round",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/runtime_block_two"),
            "DELETE reused aggregate runtime block",
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/pmem/runtime_pmem", &runtime_pmem_one),
            "paused PUT first aggregate runtime pmem",
        );
        write_block_marker_at(
            &control_backing_path,
            STORAGE_CONTINUE_PMEM_ONE_OFFSET,
            STORAGE_CONTINUE_PMEM_ONE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume aggregate storage guest for first pmem round",
        );
        if let Err(error) = wait_for_file_markers_at(
            &control_backing_path,
            &[(
                STORAGE_FIRST_PMEM_REMOVED_OFFSET,
                STORAGE_FIRST_PMEM_REMOVED_MARKER,
                b"BANGBANG_STORAGE_CERTIFICATION_FAIL",
            )],
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let failure = String::from_utf8_lossy(&file_bytes_at(
                &control_backing_path,
                15 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                128,
            ))
            .into_owned();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "aggregate first pmem round failed: {error}; failure marker: {failure:?}; serial tail:\n{serial_tail}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &runtime_pmem_one_path,
                PMEM_GUEST_FLUSH_OFFSET,
                STORAGE_RUNTIME_PMEM_ONE_GUEST_MARKER.len(),
            ),
            STORAGE_RUNTIME_PMEM_ONE_GUEST_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause aggregate storage guest before reused pmem round",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/pmem/runtime_pmem"),
            "DELETE first aggregate runtime pmem",
        );
        let runtime_pmem_two = format!(
            r#"{{"id":"runtime_pmem_two","path_on_host":{},"read_only":false,"rate_limiter":{{"ops":{{"size":5,"refill_time":100}}}}}}"#,
            json_string(path_text(&runtime_pmem_two_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/pmem/runtime_pmem_two", &runtime_pmem_two),
            "paused PUT reused aggregate runtime pmem",
        );
        write_block_marker_at(
            &control_backing_path,
            STORAGE_CONTINUE_PMEM_TWO_OFFSET,
            STORAGE_CONTINUE_PMEM_TWO_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume aggregate storage guest for reused pmem round",
        );
        if let Err(error) = wait_for_file_markers_at(
            &control_backing_path,
            &[(
                STORAGE_SUCCESS_OFFSET,
                STORAGE_SUCCESS_MARKER,
                b"BANGBANG_STORAGE_CERTIFICATION_FAIL",
            )],
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let report = vhost_backend.report();
            let failure = String::from_utf8_lossy(&file_bytes_at(
                &control_backing_path,
                15 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                128,
            ))
            .into_owned();
            let serial_tail = file_tail_lossy(&serial_path, 16 * 1024);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "aggregate reused runtime storage round failed: {error}; failure marker: {failure:?}; serial tail:\n{serial_tail}\nvhost report: {report:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &runtime_pmem_two_path,
                PMEM_GUEST_FLUSH_OFFSET,
                STORAGE_RUNTIME_PMEM_TWO_GUEST_MARKER.len(),
            ),
            STORAGE_RUNTIME_PMEM_TWO_GUEST_MARKER,
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/pmem/runtime_pmem_two"),
            "final DELETE aggregate runtime pmem",
        );
        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":0}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "shrink aggregate storage memory",
        );
        wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":0"#,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("aggregate storage guest should complete memory shrink");
        let final_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&final_config, "GET final aggregate storage config");
        for expected in [
            r#""drive_id":"rootfs""#,
            r#""drive_id":"control""#,
            r#""drive_id":"asyncdata""#,
            r#""drive_id":"vhostdata""#,
            r#""id":"pmem0""#,
            path_text(&async_replacement_path),
        ] {
            assert_response_contains(&final_config, expected, "final aggregate storage config");
        }
        for removed in [
            "runtime_block",
            "runtime_block_two",
            "runtime_pmem",
            "runtime_pmem_two",
            path_text(&async_backing_path),
        ] {
            assert!(
                !final_config.contains(removed),
                "final aggregate config must omit removed or replaced storage {removed:?}: {final_config}",
            );
        }
        assert_aggregate_storage_vhost_user_memory_aperture(&vhost_backend.report());

        vhost_backend
            .disconnect()
            .expect("aggregate vhost-user backend should disconnect last");
        let terminal_vhost_report = vhost_backend
            .finish()
            .expect("aggregate vhost-user backend should finish after disconnect");
        assert!(terminal_vhost_report.activated);
        assert!(terminal_vhost_report.reads > 0);
        assert!(terminal_vhost_report.writes > 0);
        assert!(terminal_vhost_report.flushes > 0);
        wait_for_block_event_failure(
            &socket_path,
            &metrics_path,
            "vhostdata",
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("aggregate terminal vhost-user closure should reach block metrics");
        let after_backend_death = http_get(&socket_path, "/");
        assert_response_contains(
            &after_backend_death,
            r#""state":"Running""#,
            "aggregate VM state after terminal backend death",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang aggregate storage certification",
        );
        assert!(
            !vhost_socket_path.exists(),
            "aggregate vhost-user socket should be cleaned up",
        );
    }

    #[test]
    fn signed_executable_hotplugs_replaces_and_reuses_async_block_over_product_pci() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_backing_path = test_dir.path().join("hotplug-control.img");
        let first_backing_path = test_dir.path().join("hotplug-first.img");
        let second_backing_path = test_dir.path().join("hotplug-second.img");
        let replacement_backing_path = test_dir.path().join("hotplug-replacement.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_block_backing_with_prefix(&control_backing_path, 2, &[]);
        create_block_backing_with_prefix(&first_backing_path, 1, BLOCK_HOTPLUG_HOST_ONE_MARKER);
        create_block_backing_with_prefix(&second_backing_path, 1, BLOCK_HOTPLUG_HOST_TWO_MARKER);
        create_block_backing_with_prefix(
            &replacement_backing_path,
            1,
            BLOCK_HOTPLUG_HOST_TWO_MARKER,
        );

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT /machine-config block hotplug",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT /boot-source block hotplug",
        );
        let rootfs_body = format!(
            r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
            json_string(path_text(&rootfs_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/rootfs", &rootfs_body),
            "PUT /drives/rootfs block hotplug",
        );
        let control_body = format!(
            r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&control_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/control", &control_body),
            "PUT /drives/control block hotplug",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "PUT /actions InstanceStart block hotplug",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "block hotplug guest did not become ready: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let rejected_vhost_socket_path = test_dir.path().join("anonymous-vhost.socket");
        let rejected_vhost_backend = VhostUserBlockBackend::start(
            &rejected_vhost_socket_path,
            &first_backing_path,
            VhostUserBlockBackendOptions::regular(false),
        )
        .expect("anonymous-profile rejection backend should start");
        let rejected_vhost_body = format!(
            r#"{{"drive_id":"anonymous_vhost","socket":{},"is_root_device":false}}"#,
            json_string(path_text(&rejected_vhost_socket_path)),
        );
        let rejected_vhost = http_put_json(
            &socket_path,
            "/drives/anonymous_vhost",
            &rejected_vhost_body,
        );
        assert_bad_request_response(
            &rejected_vhost,
            "anonymous-profile runtime vhost-user insertion",
        );
        assert_response_contains(
            &rejected_vhost,
            "vhost-user block requires shared guest memory",
            "anonymous-profile runtime vhost-user insertion",
        );
        assert!(
            !http_get(&socket_path, "/vm/config").contains(r#""drive_id":"anonymous_vhost""#),
            "anonymous-profile rejection must not publish runtime configuration"
        );
        let rejected_vhost_report = rejected_vhost_backend
            .finish()
            .expect("anonymous-profile rejection backend should finish");
        assert_eq!(
            rejected_vhost_report.owner_requests, 0,
            "anonymous-profile rejection must happen before socket connection"
        );

        let first_body = format!(
            r#"{{"drive_id":"hotdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async"}}"#,
            json_string(path_text(&first_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &first_body),
            "runtime PUT /drives/hotdata first block",
        );
        if let Err(err) = wait_for_file_prefix_marker(
            &first_backing_path,
            BLOCK_HOTPLUG_GUEST_ONE_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "first runtime block did not complete guest read/write/fsync: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_FIRST_REMOVED_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest did not remove the first runtime PCI function: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before runtime block reuse",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/hotdata"),
            "paused DELETE /drives/hotdata",
        );
        let removed_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&removed_config, "GET /vm/config after paused block DELETE");
        assert!(
            !removed_config.contains(r#""drive_id":"hotdata""#),
            "successful DELETE must remove the live configuration projection: {removed_config}"
        );

        let second_body = format!(
            r#"{{"drive_id":"hotdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Sync"}}"#,
            json_string(path_text(&second_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &second_body),
            "paused runtime PUT /drives/hotdata reused Sync block",
        );
        let replacement_body = format!(
            r#"{{"drive_id":"hotdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Async","rate_limiter":{{"ops":{{"size":2,"one_time_burst":1,"refill_time":100}}}}}}"#,
            json_string(path_text(&replacement_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &replacement_body),
            "paused same-ID PUT /drives/hotdata Sync to Async replacement",
        );
        let reused_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&reused_config, "GET /vm/config after paused block reuse");
        assert_response_contains(
            &reused_config,
            path_text(&replacement_backing_path),
            "GET /vm/config after paused block reuse",
        );
        assert_response_contains(
            &reused_config,
            r#""io_engine":"Async""#,
            "GET /vm/config after paused Async replacement",
        );
        assert!(
            !reused_config.contains(path_text(&second_backing_path)),
            "same-ID replacement must remove the intermediate backing projection: {reused_config}"
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused dynamic PCI Async storage preflight",
        );
        write_block_marker_at(
            &control_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            BLOCK_HOTPLUG_CONTINUE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after runtime block reuse",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &replacement_backing_path,
            BLOCK_HOTPLUG_GUEST_TWO_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "reused runtime block did not complete guest I/O: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(&second_backing_path, 0, BLOCK_HOTPLUG_HOST_TWO_MARKER.len(),),
            BLOCK_HOTPLUG_HOST_TWO_MARKER,
            "the replaced intermediate Sync backing must not receive resumed guest I/O"
        );
        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_SUCCESS_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest did not remove the reused runtime PCI function: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/hotdata"),
            "final DELETE /drives/hotdata",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang runtime block hotplug product PCI",
        );
    }

    #[test]
    fn signed_executable_hotplugs_macos_block_special_media_over_product_pci() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_backing_path = test_dir.path().join("block-special-control.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();
        let mut first_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::FourMib,
        )
        .expect("first direct PCI block media should attach");
        let mut second_media = MacosVirtualBlock::create_sized(
            MacosVirtualBlockAccess::ReadWrite,
            MacosVirtualBlockSize::EightMib,
        )
        .expect("second direct PCI block media should attach");
        write_virtual_block_marker_at(&first_media, 0, BLOCK_HOTPLUG_HOST_ONE_MARKER);
        write_virtual_block_marker_at(&second_media, 0, BLOCK_HOTPLUG_HOST_TWO_MARKER);
        let first_path = first_media
            .device_path()
            .expect("first direct PCI media should expose its exact node")
            .to_path_buf();
        let second_path = second_media
            .device_path()
            .expect("second direct PCI media should expose its exact node")
            .to_path_buf();
        let first_device_id = expected_block_device_id(&first_path);
        let second_device_id = expected_block_device_id(&second_path);
        create_block_backing_with_prefix(&control_backing_path, 2, &[]);
        create_empty_file(&serial_output_path);

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT direct PCI block-special machine config",
        );
        let boot_args = format!(
            "{DIRECT_ROOTFS_BLOCK_HOTPLUG_BOOT_ARGS} bangbang.expect-block-special-hotplug=1 bangbang.block-hotplug-cache-order=writeback-unsafe"
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(&boot_args),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT direct PCI block-special boot source",
        );
        for (route, body, context) in [
            (
                "/drives/rootfs",
                format!(
                    r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
                    json_string(path_text(&rootfs_path))
                ),
                "PUT direct PCI block-special rootfs",
            ),
            (
                "/drives/control",
                format!(
                    r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
                    json_string(path_text(&control_backing_path))
                ),
                "PUT direct PCI block-special control drive",
            ),
        ] {
            assert_no_content_response(&http_put_json(&socket_path, route, &body), context);
        }
        let serial_body = format!(
            r#"{{"serial_out_path":{}}}"#,
            json_string(path_text(&serial_output_path))
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/serial", &serial_body),
            "PUT direct PCI block-special serial output",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "start direct PCI block-special hotplug guest",
        );
        wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("direct PCI block-special guest should become ready");

        let first_body = format!(
            r#"{{"drive_id":"hotdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback","io_engine":"Sync","rate_limiter":{{"ops":{{"size":1,"refill_time":100}}}}}}"#,
            json_string(path_text(&first_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &first_body),
            "runtime PUT direct first block-special PCI drive",
        );
        let first_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&first_config, "GET first direct block-special PCI config");
        for expected in [
            r#""drive_id":"hotdata""#,
            r#""cache_type":"Writeback""#,
            r#""io_engine":"Sync""#,
            r#""refill_time":100"#,
        ] {
            assert_response_contains(
                &first_config,
                expected,
                "first direct block-special PCI drive",
            );
        }
        if let Err(error) = wait_for_virtual_block_marker(
            &first_media,
            0,
            BLOCK_HOTPLUG_GUEST_ONE_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct first PCI block-special round failed: {error}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_FIRST_REMOVED_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("guest should manually remove first direct block-special function");
        wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_HOTPLUG_FIRST_SERIAL_END_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("guest should report first direct hotplug GET_ID");
        assert_phase_block_serial_report(
            &serial_output_path,
            BLOCK_HOTPLUG_FIRST_SERIAL_BEGIN_MARKER,
            BLOCK_HOTPLUG_FIRST_SERIAL_END_MARKER,
            &first_device_id,
            "first direct PCI block-special drive",
        );

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause direct block-special guest before DELETE",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/hotdata"),
            "DELETE first direct block-special PCI drive",
        );
        let removed = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &removed,
            "GET config after first direct block-special DELETE",
        );
        assert!(!removed.contains(r#""drive_id":"hotdata""#));

        let second_body = format!(
            r#"{{"drive_id":"hotdata","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Unsafe","io_engine":"Async"}}"#,
            json_string(path_text(&second_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &second_body),
            "paused PUT reused direct block-special PCI drive",
        );
        let reused = http_get(&socket_path, "/vm/config");
        assert_ok_response(&reused, "GET reused direct block-special PCI config");
        for expected in [
            r#""drive_id":"hotdata""#,
            r#""cache_type":"Unsafe""#,
            r#""io_engine":"Async""#,
        ] {
            assert_response_contains(&reused, expected, "reused direct block-special PCI drive");
        }
        write_block_marker_at(
            &control_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            BLOCK_HOTPLUG_CONTINUE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume direct block-special guest after slot reuse",
        );
        if let Err(error) = wait_for_virtual_block_marker(
            &second_media,
            0,
            BLOCK_HOTPLUG_GUEST_TWO_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct second PCI block-special round failed: {error}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_SUCCESS_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("guest should manually remove reused direct block-special function");
        wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_HOTPLUG_SECOND_SERIAL_END_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .expect("guest should report second direct hotplug GET_ID");
        assert_phase_block_serial_report(
            &serial_output_path,
            BLOCK_HOTPLUG_SECOND_SERIAL_BEGIN_MARKER,
            BLOCK_HOTPLUG_SECOND_SERIAL_END_MARKER,
            &second_device_id,
            "second direct PCI block-special drive",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/hotdata"),
            "final DELETE reused direct block-special PCI drive",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "direct block-special PCI lifecycle",
        );
        first_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("first direct PCI media should release for read-only inspection");
        second_media
            .reattach(MacosVirtualBlockAccess::ReadOnly)
            .expect("second direct PCI media should release for read-only inspection");
        assert_eq!(
            first_media
                .read_at(0, BLOCK_HOTPLUG_GUEST_ONE_MARKER.len())
                .expect("first direct PCI marker should persist"),
            BLOCK_HOTPLUG_GUEST_ONE_MARKER,
        );
        assert_eq!(
            second_media
                .read_at(0, BLOCK_HOTPLUG_GUEST_TWO_MARKER.len())
                .expect("second direct PCI marker should persist"),
            BLOCK_HOTPLUG_GUEST_TWO_MARKER,
        );
        first_media
            .cleanup()
            .expect("first direct PCI media should clean up exactly");
        second_media
            .cleanup()
            .expect("second direct PCI media should clean up exactly");
    }

    #[test]
    fn signed_executable_refreshes_hotplugs_and_reuses_vhost_user_block_over_product_pci() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_socket_path = test_dir.path().join("control-vhost.socket");
        let first_socket_path = test_dir.path().join("first-vhost.socket");
        let second_socket_path = test_dir.path().join("second-vhost.socket");
        let control_backing_path = test_dir.path().join("vhost-hotplug-control.img");
        let first_backing_path = test_dir.path().join("vhost-hotplug-first.img");
        let second_backing_path = test_dir.path().join("vhost-hotplug-second.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_block_backing_with_prefix(&control_backing_path, 2, &[]);
        create_block_backing_with_prefix(&first_backing_path, 1, BLOCK_HOTPLUG_HOST_ONE_MARKER);
        create_block_backing_with_prefix(&second_backing_path, 1, BLOCK_HOTPLUG_HOST_TWO_MARKER);
        let control_backend = VhostUserBlockBackend::start(
            &control_socket_path,
            &control_backing_path,
            VhostUserBlockBackendOptions::regular(false),
        )
        .expect("control vhost-user backend should start");

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT /machine-config vhost-user block hotplug",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(DIRECT_ROOTFS_VHOST_BLOCK_HOTPLUG_BOOT_ARGS),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT /boot-source vhost-user block hotplug",
        );
        let rootfs_body = format!(
            r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
            json_string(path_text(&rootfs_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/rootfs", &rootfs_body),
            "PUT /drives/rootfs vhost-user block hotplug",
        );
        let control_body = format!(
            r#"{{"drive_id":"control","socket":{},"is_root_device":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&control_socket_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/control", &control_body),
            "PUT /drives/control vhost-user block hotplug",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/hotplug/memory",
                r#"{"total_size_mib":128,"block_size_mib":2,"slot_size_mib":128}"#,
            ),
            "PUT /hotplug/memory with runtime vhost-user lifecycle",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "PUT /actions InstanceStart vhost-user block hotplug",
        );

        if let Err(error) = wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "vhost-user block hotplug guest did not become ready: {error}; control report: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                control_backend.report(),
                output.status,
                output.stdout,
                output.stderr
            );
        }
        control_backend
            .wait_for_activation(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("control vhost-user backend should activate");
        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":128}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "grow memory before runtime vhost-user insertion",
        );
        wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":128"#,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("guest should complete memory grow before runtime vhost-user insertion");
        assert_vhost_user_memory_aperture(&control_backend.report(), "runtime control");

        fs::OpenOptions::new()
            .write(true)
            .open(&control_backing_path)
            .expect("control backing should reopen for resize")
            .set_len(4 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("control backing should resize");
        assert_no_content_response(
            &http_json(
                &socket_path,
                "PATCH",
                "/drives/control",
                r#"{"drive_id":"control"}"#,
            ),
            "PATCH /drives/control vhost-user config refresh",
        );
        if let Err(error) = wait_for_file_markers_at(
            &control_backing_path,
            &[(
                3 * bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
                VHOST_CONFIG_RESIZED_MARKER,
                b"BANGBANG_BLOCK_HOTPLUG_FAIL",
            )],
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest did not observe refreshed vhost-user capacity: {error}; control report: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                control_backend.report(),
                output.status,
                output.stdout,
                output.stderr
            );
        }
        assert_eq!(
            control_backend.report().config_requests,
            2,
            "startup discovery and ID-only PATCH should issue two exact GET_CONFIG requests"
        );

        let rejected_socket_path = test_dir.path().join("rejected-vhost.socket");
        let rejected_backend = VhostUserBlockBackend::start(
            &rejected_socket_path,
            &first_backing_path,
            VhostUserBlockBackendOptions::regular(false).without_config_protocol(),
        )
        .expect("rejecting runtime vhost-user backend should start");
        let rejected_body = format!(
            r#"{{"drive_id":"rejected","socket":{},"is_root_device":false}}"#,
            json_string(path_text(&rejected_socket_path)),
        );
        let rejected = http_put_json(&socket_path, "/drives/rejected", &rejected_body);
        assert_bad_request_response(&rejected, "runtime vhost-user negotiation rejection");
        assert_response_contains(
            &rejected,
            "vhost-user backend lacks configuration protocol support",
            "runtime vhost-user negotiation rejection",
        );
        assert!(
            !http_get(&socket_path, "/vm/config").contains(r#""drive_id":"rejected""#),
            "failed discovery must not publish runtime configuration"
        );
        let rejected_report = rejected_backend
            .finish()
            .expect("rejecting runtime vhost-user backend should finish");
        assert!(rejected_report.discovery_rejected);

        let first_backend = VhostUserBlockBackend::start(
            &first_socket_path,
            &first_backing_path,
            VhostUserBlockBackendOptions::regular(false),
        )
        .expect("first runtime vhost-user backend should start");
        let second_backend = VhostUserBlockBackend::start(
            &second_socket_path,
            &second_backing_path,
            VhostUserBlockBackendOptions::regular(false),
        )
        .expect("second runtime vhost-user backend should start");
        let first_body = format!(
            r#"{{"drive_id":"hotdata","socket":{},"is_root_device":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&first_socket_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &first_body),
            "runtime PUT /drives/hotdata first vhost-user block",
        );
        let duplicate_body = format!(
            r#"{{"drive_id":"hotdata","socket":{},"is_root_device":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&second_socket_path)),
        );
        let duplicate = http_put_json(&socket_path, "/drives/hotdata", &duplicate_body);
        assert_bad_request_response(
            &duplicate,
            "duplicate runtime PUT /drives/hotdata vhost-user",
        );
        assert_response_contains(
            &duplicate,
            r#"{"fault_message":"drive is already configured"}"#,
            "duplicate runtime PUT /drives/hotdata vhost-user",
        );
        assert_eq!(
            second_backend.report().owner_requests,
            0,
            "duplicate same-ID PUT must reject before connecting"
        );

        first_backend
            .wait_for_activation(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("first runtime vhost-user backend should activate");
        assert_vhost_user_memory_aperture(&first_backend.report(), "first runtime block");
        if let Err(error) = wait_for_file_prefix_marker(
            &first_backing_path,
            BLOCK_HOTPLUG_GUEST_ONE_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "first runtime vhost-user block did not complete guest I/O: {error}; first report: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                first_backend.report(),
                output.status,
                output.stdout,
                output.stderr
            );
        }
        wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_FIRST_REMOVED_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("guest should remove the first runtime vhost-user PCI function");

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before vhost-user runtime block reuse",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/hotdata"),
            "paused DELETE /drives/hotdata vhost-user",
        );
        first_backend
            .wait_for_frontend_close(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("first runtime vhost-user frontend should close after DELETE");
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/hotdata", &duplicate_body),
            "paused runtime PUT /drives/hotdata reused vhost-user block",
        );
        assert_eq!(
            second_backend.report().config_requests,
            1,
            "reused backend should complete discovery while the VM is paused"
        );
        write_block_marker_at(
            &control_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            BLOCK_HOTPLUG_CONTINUE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after vhost-user runtime block reuse",
        );

        second_backend
            .wait_for_activation(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("reused runtime vhost-user backend should activate");
        assert_vhost_user_memory_aperture(&second_backend.report(), "reused runtime block");
        if let Err(error) = wait_for_file_prefix_marker(
            &second_backing_path,
            BLOCK_HOTPLUG_GUEST_TWO_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "reused runtime vhost-user block did not complete guest I/O: {error}; second report: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                second_backend.report(),
                output.status,
                output.stdout,
                output.stderr
            );
        }
        wait_for_file_prefix_marker(
            &control_backing_path,
            BLOCK_HOTPLUG_SUCCESS_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("guest should remove the reused runtime vhost-user PCI function");
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/drives/hotdata"),
            "final DELETE /drives/hotdata vhost-user",
        );
        second_backend
            .wait_for_frontend_close(PCI_ALL_VIRTIO_GUEST_TIMEOUT)
            .expect("reused runtime vhost-user frontend should close after DELETE");
        assert_no_content_response(
            &http_json_with_io_timeout(
                &socket_path,
                "PATCH",
                "/hotplug/memory",
                r#"{"requested_size_mib":0}"#,
                PCI_ALL_VIRTIO_GUEST_TIMEOUT,
            ),
            "shrink memory after runtime vhost-user reuse",
        );
        wait_for_http_response_fragment(
            &socket_path,
            "/hotplug/memory",
            r#""plugged_size_mib":0"#,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        )
        .expect("guest should complete memory shrink after runtime vhost-user reuse");
        assert_vhost_user_memory_aperture(
            &control_backend.report(),
            "runtime control after shrink",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang runtime vhost-user block hotplug product PCI",
        );
        let first_report = first_backend
            .finish()
            .expect("first runtime vhost-user backend should finish");
        let second_report = second_backend
            .finish()
            .expect("second runtime vhost-user backend should finish");
        let control_report = control_backend
            .finish()
            .expect("control vhost-user backend should finish");
        assert!(first_report.activated && first_report.frontend_closed);
        assert!(second_report.activated && second_report.frontend_closed);
        assert!(control_report.activated && control_report.frontend_closed);
    }

    #[test]
    fn signed_executable_hotplugs_mmds_network_and_reuses_product_pci_slot() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_backing_path = test_dir.path().join("network-hotplug-control.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();
        let network_body =
            r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"06:00:00:00:00:42"}"#;

        create_block_backing_with_prefix(&control_backing_path, 3, &[]);

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT /machine-config network hotplug",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(DIRECT_ROOTFS_NETWORK_HOTPLUG_BOOT_ARGS),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT /boot-source network hotplug",
        );
        let rootfs_body = format!(
            r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
            json_string(path_text(&rootfs_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/rootfs", &rootfs_body),
            "PUT /drives/rootfs network hotplug",
        );
        let control_body = format!(
            r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&control_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/control", &control_body),
            "PUT /drives/control network hotplug",
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/network-interfaces/eth0", network_body),
            "PUT /network-interfaces/eth0 before network hotplug",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/mmds/config",
                r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            ),
            "PUT /mmds/config network hotplug",
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/mmds", DIRECT_ROOTFS_MMDS_CONTENT),
            "PUT /mmds network hotplug",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "PUT /actions InstanceStart network hotplug",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            NETWORK_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "network hotplug guest did not remove its startup function: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/network-interfaces/eth0"),
            "DELETE startup /network-interfaces/eth0",
        );
        let removed_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &removed_config,
            "GET /vm/config after startup network DELETE",
        );
        assert_response_contains(
            &removed_config,
            r#""network-interfaces":[]"#,
            "GET /vm/config after startup network DELETE",
        );

        assert_no_content_response(
            &http_put_json(&socket_path, "/network-interfaces/eth0", network_body),
            "runtime PUT /network-interfaces/eth0 first MMDS network",
        );
        let duplicate_id = http_put_json(
            &socket_path,
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","host_dev_name":"vmnet:bridged:private_bridge","guest_mac":"06:00:00:00:00:43"}"#,
        );
        assert_bad_request_response(&duplicate_id, "duplicate runtime network ID");
        assert_response_contains(
            &duplicate_id,
            r#"{"fault_message":"network interface is already configured"}"#,
            "duplicate runtime network ID",
        );
        assert!(!duplicate_id.contains("private_bridge"));
        assert!(!duplicate_id.contains("06:00:00:00:00:43"));

        let duplicate_mac = http_put_json(
            &socket_path,
            "/network-interfaces/private_iface",
            r#"{"iface_id":"private_iface","host_dev_name":"vmnet:shared","guest_mac":"06:00:00:00:00:42"}"#,
        );
        assert_bad_request_response(&duplicate_mac, "duplicate runtime network MAC");
        assert_response_contains(
            &duplicate_mac,
            r#"{"fault_message":"network guest_mac is already in use"}"#,
            "duplicate runtime network MAC",
        );
        assert!(!duplicate_mac.contains("private_iface"));
        assert!(!duplicate_mac.contains("06:00:00:00:00:42"));

        let first_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&first_config, "GET /vm/config after runtime network PUT");
        assert_response_contains(
            &first_config,
            r#""iface_id":"eth0""#,
            "GET /vm/config after runtime network PUT",
        );
        assert_response_contains(
            &first_config,
            r#""guest_mac":"06:00:00:00:00:42""#,
            "GET /vm/config after runtime network PUT",
        );
        write_block_marker_at(
            &control_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            NETWORK_HOTPLUG_FIRST_CONTINUE_MARKER,
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            NETWORK_HOTPLUG_FIRST_REMOVED_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "first runtime network did not exchange MMDS traffic and leave the guest: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/network-interfaces/eth0"),
            "DELETE first runtime /network-interfaces/eth0",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before runtime network reuse",
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/network-interfaces/eth0", network_body),
            "paused runtime PUT /network-interfaces/eth0 reused MMDS network",
        );
        let reused_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&reused_config, "GET /vm/config after network reuse");
        assert_response_contains(
            &reused_config,
            r#""iface_id":"eth0""#,
            "GET /vm/config after network reuse",
        );
        write_block_marker_at(
            &control_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE * 2,
            NETWORK_HOTPLUG_SECOND_CONTINUE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after runtime network reuse",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            NETWORK_HOTPLUG_SUCCESS_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "reused runtime network did not preserve its PCI slot and MMDS path: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/network-interfaces/eth0"),
            "final DELETE /network-interfaces/eth0",
        );
        let final_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&final_config, "GET /vm/config after final network DELETE");
        assert_response_contains(
            &final_config,
            r#""network-interfaces":[]"#,
            "GET /vm/config after final network DELETE",
        );
        let missing = http_no_body(
            &socket_path,
            "DELETE",
            "/network-interfaces/private_missing",
        );
        assert_bad_request_response(&missing, "DELETE missing runtime network");
        assert_response_contains(
            &missing,
            r#"{"fault_message":"network interface is not configured"}"#,
            "DELETE missing runtime network",
        );
        assert!(!missing.contains("private_missing"));

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang MMDS network hotplug product PCI",
        );
    }

    #[test]
    fn signed_executable_hotplugs_flushes_and_reuses_runtime_pmem_over_product_pci() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let control_backing_path = test_dir.path().join("pmem-hotplug-control.img");
        let first_backing_path = test_dir.path().join("pmem-hotplug-first.img");
        let second_backing_path = test_dir.path().join("pmem-hotplug-second.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_block_backing_with_prefix(&control_backing_path, 2, &[]);
        create_pmem_backing(&first_backing_path, PMEM_HOTPLUG_HOST_ONE_MARKER);
        create_pmem_backing(&second_backing_path, PMEM_HOTPLUG_HOST_TWO_MARKER);

        let mut bangbang =
            BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--enable-pci"]);
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/machine-config",
                r#"{"vcpu_count":1,"mem_size_mib":256}"#,
            ),
            "PUT /machine-config pmem hotplug",
        );
        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(&kernel_path)),
            json_string(DIRECT_ROOTFS_PMEM_HOTPLUG_BOOT_ARGS),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/boot-source", &boot_body),
            "PUT /boot-source pmem hotplug",
        );
        let rootfs_body = format!(
            r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
            json_string(path_text(&rootfs_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/rootfs", &rootfs_body),
            "PUT /drives/rootfs pmem hotplug",
        );
        let control_body = format!(
            r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
            json_string(path_text(&control_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/drives/control", &control_body),
            "PUT /drives/control pmem hotplug",
        );
        assert_no_content_response(
            &http_put_json(
                &socket_path,
                "/actions",
                r#"{"action_type":"InstanceStart"}"#,
            ),
            "PUT /actions InstanceStart pmem hotplug",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            PMEM_HOTPLUG_READY_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "pmem hotplug guest did not become ready: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let first_body = format!(
            r#"{{"id":"hotpmem","path_on_host":{},"read_only":false}}"#,
            json_string(path_text(&first_backing_path)),
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/pmem/hotpmem", &first_body),
            "runtime PUT /pmem/hotpmem first backing",
        );
        let second_body = format!(
            r#"{{"id":"hotpmem","path_on_host":{},"read_only":false}}"#,
            json_string(path_text(&second_backing_path)),
        );
        let duplicate = http_put_json(&socket_path, "/pmem/hotpmem", &second_body);
        assert_bad_request_response(&duplicate, "duplicate runtime PUT /pmem/hotpmem");
        assert_response_contains(
            &duplicate,
            r#"{"fault_message":"pmem device is already configured"}"#,
            "duplicate runtime PUT /pmem/hotpmem",
        );
        assert!(
            !duplicate.contains(path_text(&second_backing_path)),
            "duplicate runtime pmem response must redact the rejected backing path: {duplicate}"
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            PMEM_HOTPLUG_FIRST_REMOVED_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "first runtime pmem did not flush and leave the guest: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &first_backing_path,
                PMEM_GUEST_FLUSH_OFFSET,
                PMEM_HOTPLUG_GUEST_ONE_MARKER.len(),
            ),
            PMEM_HOTPLUG_GUEST_ONE_MARKER,
            "first runtime pmem flush should persist before removal"
        );

        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before runtime pmem reuse",
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/pmem/hotpmem"),
            "paused DELETE /pmem/hotpmem",
        );
        let removed_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&removed_config, "GET /vm/config after pmem DELETE");
        assert!(
            !removed_config.contains(r#""id":"hotpmem""#),
            "successful pmem DELETE must remove the configuration projection: {removed_config}"
        );
        assert_no_content_response(
            &http_put_json(&socket_path, "/pmem/hotpmem", &second_body),
            "paused runtime PUT /pmem/hotpmem reused backing",
        );
        let reused_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&reused_config, "GET /vm/config after pmem reuse");
        assert_response_contains(
            &reused_config,
            path_text(&second_backing_path),
            "GET /vm/config after pmem reuse",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused dynamic PCI pmem storage preflight",
        );
        write_block_marker_at(
            &control_backing_path,
            bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE,
            PMEM_HOTPLUG_CONTINUE_MARKER,
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after runtime pmem reuse",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &control_backing_path,
            PMEM_HOTPLUG_SUCCESS_MARKER,
            PCI_ALL_VIRTIO_GUEST_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "reused runtime pmem did not preserve its PCI slot and guest range: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        assert_eq!(
            file_bytes_at(
                &second_backing_path,
                PMEM_GUEST_FLUSH_OFFSET,
                PMEM_HOTPLUG_GUEST_TWO_MARKER.len(),
            ),
            PMEM_HOTPLUG_GUEST_TWO_MARKER,
            "reused runtime pmem flush should persist before final removal"
        );
        assert_no_content_response(
            &http_no_body(&socket_path, "DELETE", "/pmem/hotpmem"),
            "final DELETE /pmem/hotpmem",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang runtime pmem hotplug product PCI",
        );
    }

    #[test]
    fn signed_executable_flushes_virtio_pmem_from_direct_rootfs_guest() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let pmem_backing_path = test_dir.path().join("pmem.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);
        create_pmem_backing(&pmem_backing_path, PMEM_HOST_MARKER);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config pmem direct rootfs");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_PMEM_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source pmem direct rootfs");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs pmem direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(&data_drive_response, "PUT /drives/data pmem direct rootfs");

        let pmem_backing_path_json = json_string(path_text(&pmem_backing_path));
        let pmem_body = format!(
            r#"{{
                "id":"pmem0",
                "path_on_host":{pmem_backing_path_json},
                "rate_limiter":{{
                    "bandwidth":{{
                        "size":{},
                        "refill_time":1000
                    }},
                    "ops":{{
                        "size":1,
                        "refill_time":1000
                    }}
                }}
            }}"#,
            bangbang_runtime::pmem::VIRTIO_PMEM_ALIGNMENT,
        );
        let pmem_response = http_put_json(&socket_path, "/pmem/pmem0", &pmem_body);
        assert_no_content_response(&pmem_response, "PUT /pmem/pmem0 direct rootfs");

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after PUT /pmem/pmem0");
        assert_response_contains(
            &vm_config,
            r#""id":"pmem0""#,
            "GET /vm/config after PUT /pmem/pmem0",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{pmem_backing_path_json}"#),
            "GET /vm/config after PUT /pmem/pmem0",
        );
        assert_response_contains(
            &vm_config,
            r#""rate_limiter":{"bandwidth":{"one_time_burst":null,"refill_time":1000,"size":2097152},"ops":{"one_time_burst":null,"refill_time":1000,"size":1}}"#,
            "GET /vm/config after rate-limited PUT /pmem/pmem0",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart pmem direct rootfs",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after pmem direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after pmem direct rootfs InstanceStart",
        );

        let pmem_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/pmem/pmem0",
            r#"{"id":"pmem0","rate_limiter":{"bandwidth":null,"ops":null}}"#,
        );
        assert_no_content_response(
            &pmem_patch_response,
            "PATCH /pmem/pmem0 no-op rate limiter after InstanceStart",
        );

        let pmem_rate_limiter_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/pmem/pmem0",
            r#"{"id":"pmem0","rate_limiter":{"ops":{"size":2,"one_time_burst":1,"refill_time":100}}}"#,
        );
        assert_no_content_response(
            &pmem_rate_limiter_patch_response,
            "PATCH /pmem/pmem0 configured rate limiter after InstanceStart",
        );
        let patched_vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &patched_vm_config,
            "GET /vm/config after live pmem limiter PATCH",
        );
        assert_response_contains(
            &patched_vm_config,
            r#""rate_limiter":{"bandwidth":{"one_time_burst":null,"refill_time":1000,"size":2097152},"ops":{"one_time_burst":1,"refill_time":100,"size":2}}"#,
            "GET /vm/config after live pmem limiter PATCH",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_PMEM_READ_FLUSH_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let pmem_prefix = file_prefix_lossy(&pmem_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not flush pmem through signed bangbang executable: {err}; data backing prefix: {backing_prefix:?}; pmem prefix: {pmem_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_eq!(
            file_bytes_at(
                &pmem_backing_path,
                PMEM_GUEST_FLUSH_OFFSET,
                PMEM_GUEST_FLUSH_MARKER.len(),
            ),
            PMEM_GUEST_FLUSH_MARKER,
            "guest pmem flush should persist the guest marker to the host backing file"
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#),
            "pause before direct pmem storage preflight",
        );
        assert_capture_ready_snapshot_rejected_without_artifacts(
            &socket_path,
            test_dir.path(),
            "paused direct pmem storage preflight",
        );
        assert_no_content_response(
            &http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#),
            "resume after direct pmem storage preflight",
        );

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang pmem direct rootfs",
        );
    }

    #[test]
    fn signed_executable_boots_read_only_and_writable_pmem_roots() {
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let source_rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);

        for (mode, read_only, enable_pci, success_marker) in [
            ("ro", true, false, DIRECT_ROOTFS_PMEM_ROOT_RO_MARKER),
            ("rw", false, true, DIRECT_ROOTFS_PMEM_ROOT_RW_MARKER),
        ] {
            let test_dir = TestDir::new();
            let socket_path = test_dir.path().join("api.socket");
            let control_backing_path = test_dir
                .path()
                .join(format!("pmem-root-{mode}-control.img"));
            let rootfs_path = if read_only {
                source_rootfs_path.clone()
            } else {
                let copy = test_dir.path().join("pmem-root-rw.ext4");
                fs::copy(&source_rootfs_path, &copy)
                    .expect("writable process pmem root fixture should copy");
                copy
            };
            create_zeroed_block_backing(&control_backing_path);
            let instance_id = test_dir.instance_id();
            let mut bangbang = if enable_pci {
                BangbangProcess::start_with_extra_args(
                    &socket_path,
                    &instance_id,
                    &["--enable-pci"],
                )
            } else {
                BangbangProcess::start(&socket_path, &instance_id)
            };

            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/machine-config",
                    r#"{"vcpu_count":1,"mem_size_mib":256}"#,
                ),
                "PUT /machine-config pmem root",
            );
            let boot_args = format!(
                "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rootwait init=/bangbang-direct-rootfs-init bangbang.pmem-root={mode}"
            );
            let boot_body = format!(
                r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
                json_string(path_text(&kernel_path)),
                json_string(&boot_args),
            );
            assert_no_content_response(
                &http_put_json(&socket_path, "/boot-source", &boot_body),
                "PUT /boot-source pmem root",
            );
            let control_body = format!(
                r#"{{"drive_id":"control","path_on_host":{},"is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}}"#,
                json_string(path_text(&control_backing_path)),
            );
            assert_no_content_response(
                &http_put_json(&socket_path, "/drives/control", &control_body),
                "PUT /drives/control pmem root",
            );
            let rootfs_path_json = json_string(path_text(&rootfs_path));
            let pmem_body = format!(
                r#"{{"id":"root_pmem","path_on_host":{rootfs_path_json},"root_device":true,"read_only":{read_only}}}"#,
            );
            assert_no_content_response(
                &http_put_json(&socket_path, "/pmem/root_pmem", &pmem_body),
                "PUT /pmem/root_pmem",
            );

            let conflict = http_put_json(
                &socket_path,
                "/drives/conflict_root",
                r#"{"drive_id":"conflict_root","path_on_host":"/private/conflict-root.ext4","is_root_device":true}"#,
            );
            assert_bad_request_response(&conflict, "PUT conflicting block root");
            assert_response_contains(
                &conflict,
                r#"{"fault_message":"a root drive is already configured"}"#,
                "PUT conflicting block root",
            );
            assert!(!conflict.contains("/private/conflict-root.ext4"));

            let vm_config = http_get(&socket_path, "/vm/config");
            assert_ok_response(&vm_config, "GET /vm/config before pmem root start");
            assert_response_contains(
                &vm_config,
                r#""id":"root_pmem""#,
                "GET /vm/config before pmem root start",
            );
            assert_response_contains(
                &vm_config,
                r#""root_device":true"#,
                "GET /vm/config before pmem root start",
            );
            assert_response_contains(
                &vm_config,
                &format!(r#""read_only":{read_only}"#),
                "GET /vm/config before pmem root start",
            );
            assert!(!vm_config.contains(r#""drive_id":"conflict_root""#));

            assert_no_content_response(
                &http_put_json(
                    &socket_path,
                    "/actions",
                    r#"{"action_type":"InstanceStart"}"#,
                ),
                "PUT /actions InstanceStart pmem root",
            );
            if let Err(err) = wait_for_file_prefix_marker(
                &control_backing_path,
                success_marker,
                GUEST_EXECUTION_TIMEOUT,
            ) {
                let control_prefix = file_prefix_lossy(&control_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "{mode} pmem root guest did not report success: {err}; control prefix: {control_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }

            if enable_pci {
                let rejected_put = http_put_json(&socket_path, "/pmem/root_pmem", &pmem_body);
                assert_bad_request_response(&rejected_put, "runtime PUT root pmem");
                assert_response_contains(
                    &rejected_put,
                    r#"{"fault_message":"a root pmem device cannot be inserted after the microVM starts"}"#,
                    "runtime PUT root pmem",
                );
                assert!(!rejected_put.contains(path_text(&rootfs_path)));

                let rejected_delete = http_no_body(&socket_path, "DELETE", "/pmem/root_pmem");
                assert_bad_request_response(&rejected_delete, "runtime DELETE root pmem");
                assert_response_contains(
                    &rejected_delete,
                    r#"{"fault_message":"root pmem device cannot be removed"}"#,
                    "runtime DELETE root pmem",
                );
            }

            assert_clean_shutdown(
                bangbang.terminate(),
                &socket_path,
                &format!("bangbang {mode} pmem root"),
            );
        }
    }

    #[test]
    fn signed_executable_serves_mmds_with_configured_mtu_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "MMDS guest fetch with configured MTU",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            boot_args: DIRECT_ROOTFS_MMDS_MTU_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_MTU_MARKER,
            network_mtu: Some(1280),
            initial_rx_rate_limiter: None,
            wait_for_guest_completion_before_network_patch: false,
            content_source: DirectRootfsMmdsContentSource::ApiRequest,
        });
    }

    #[test]
    fn signed_executable_retries_rate_limited_mmds_rx_without_second_guest_notification() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "rate-limited MMDS RX guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            boot_args: DIRECT_ROOTFS_MMDS_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_MARKER,
            network_mtu: None,
            initial_rx_rate_limiter: Some(
                r#"{"ops":{"size":1,"one_time_burst":0,"refill_time":1500}}"#,
            ),
            wait_for_guest_completion_before_network_patch: true,
            content_source: DirectRootfsMmdsContentSource::ApiRequest,
        });
    }

    #[test]
    fn signed_executable_serves_mmds_on_two_isolated_guest_interfaces() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let metrics_path = test_dir.path().join("metrics.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing_with_sectors(&data_backing_path, 2);
        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config multi-interface MMDS guest fetch",
        );

        for (iface_id, guest_mac) in [("eth0", "06:00:00:00:00:01"), ("eth1", "06:00:00:00:00:02")]
        {
            let endpoint = format!("/network-interfaces/{iface_id}");
            let body = format!(
                r#"{{"iface_id":"{iface_id}","host_dev_name":"vmnet:shared","guest_mac":"{guest_mac}"}}"#
            );
            let response = http_put_json(&socket_path, &endpoint, &body);
            assert_no_content_response(
                &response,
                &format!("PUT /network-interfaces/{iface_id} multi-interface MMDS guest fetch"),
            );
        }

        let mmds_config_response = http_put_json(
            &socket_path,
            "/mmds/config",
            r#"{"network_interfaces":["eth0","eth1"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
        );
        assert_no_content_response(
            &mmds_config_response,
            "PUT /mmds/config multi-interface MMDS guest fetch",
        );
        let mmds_response = http_put_json(&socket_path, "/mmds", DIRECT_ROOTFS_MMDS_CONTENT);
        assert_no_content_response(&mmds_response, "PUT /mmds multi-interface MMDS guest fetch");

        let metrics_path_json = json_string(path_text(&metrics_path));
        let metrics_response = http_put_json(
            &socket_path,
            "/metrics",
            &format!(r#"{{"metrics_path":{metrics_path_json}}}"#),
        );
        assert_no_content_response(
            &metrics_response,
            "PUT /metrics multi-interface MMDS guest fetch",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_MMDS_MULTI_BOOT_ARGS);
        let boot_response = http_put_json(
            &socket_path,
            "/boot-source",
            &format!(r#"{{"kernel_image_path":{kernel_path_json},"boot_args":{boot_args_json}}}"#),
        );
        assert_no_content_response(
            &boot_response,
            "PUT /boot-source multi-interface MMDS guest fetch",
        );

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_response = http_put_json(
            &socket_path,
            "/drives/rootfs",
            &format!(
                r#"{{"drive_id":"rootfs","path_on_host":{rootfs_path_json},"is_root_device":true,"is_read_only":true}}"#
            ),
        );
        assert_no_content_response(
            &rootfs_response,
            "PUT /drives/rootfs multi-interface MMDS guest fetch",
        );

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_response = http_put_json(
            &socket_path,
            "/drives/data",
            &format!(
                r#"{{"drive_id":"data","path_on_host":{data_backing_path_json},"is_root_device":false,"is_read_only":false}}"#
            ),
        );
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data multi-interface MMDS guest fetch",
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart multi-interface MMDS guest fetch",
        );
        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after multi-interface MMDS InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after multi-interface MMDS InstanceStart",
        );

        let marker_expectations = [
            (
                DIRECT_ROOTFS_MMDS_ETH0_MARKER_OFFSET,
                DIRECT_ROOTFS_MMDS_ETH0_MARKER,
                DIRECT_ROOTFS_MMDS_ETH0_FAILURE_MARKER,
            ),
            (
                DIRECT_ROOTFS_MMDS_ETH1_MARKER_OFFSET,
                DIRECT_ROOTFS_MMDS_ETH1_MARKER,
                DIRECT_ROOTFS_MMDS_ETH1_FAILURE_MARKER,
            ),
        ];
        if let Err(err) = wait_for_file_markers_at(
            &data_backing_path,
            &marker_expectations,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let eth0_prefix = file_bytes_at(
                &data_backing_path,
                DIRECT_ROOTFS_MMDS_ETH0_MARKER_OFFSET,
                96,
            );
            let eth1_prefix = file_bytes_at(
                &data_backing_path,
                DIRECT_ROOTFS_MMDS_ETH1_MARKER_OFFSET,
                96,
            );
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete isolated multi-interface MMDS fetches: {err}; eth0 marker slot: {:?}; eth1 marker slot: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&eth0_prefix),
                String::from_utf8_lossy(&eth1_prefix),
                output.status,
                output.stdout,
                output.stderr
            );
        }

        let flush_metrics_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(
            &flush_metrics_response,
            "PUT /actions FlushMetrics multi-interface MMDS guest fetch",
        );
        assert_multi_interface_network_metrics(&metrics_path, &["eth0", "eth1"]);

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang multi-interface MMDS direct rootfs",
        );
    }

    #[test]
    fn signed_executable_keeps_concurrent_mmds_processes_isolated() {
        let test_dir = TestDir::new();
        let socket_a = test_dir.path().join("mmds-a.socket");
        let socket_b = test_dir.path().join("mmds-b.socket");
        let scratch_a = test_dir.path().join("mmds-a.img");
        let scratch_b = test_dir.path().join("mmds-b.img");
        let metrics_a = test_dir.path().join("mmds-a.metrics");
        let metrics_b = test_dir.path().join("mmds-b.metrics");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_prefix = test_dir.instance_id();
        let instance_a = format!("{instance_prefix}-mmds-a");
        let instance_b = format!("{instance_prefix}-mmds-b");
        let private_fragments = concurrent_mmds_private_fragments(
            test_dir.path(),
            &kernel_path,
            &rootfs_path,
            &instance_a,
            &instance_b,
        );

        create_zeroed_block_backing(&scratch_a);
        create_zeroed_block_backing_with_sectors(&scratch_b, 2);
        let mut process_a = BangbangProcess::start(&socket_a, &instance_a);
        let mut process_b = BangbangProcess::start(&socket_b, &instance_b);

        let configured_a = configure_concurrent_mmds_guest(
            &socket_a,
            &kernel_path,
            &rootfs_path,
            ConcurrentMmdsGuestConfig {
                iface_id: CONCURRENT_MMDS_PROCESS_A_IFACE_ID,
                guest_mac: "06:00:00:00:01:01",
                mmds_content: CONCURRENT_MMDS_PROCESS_A_CONTENT,
                boot_args: CONCURRENT_MMDS_PROCESS_A_BOOT_ARGS,
                scratch_path: &scratch_a,
                metrics_path: &metrics_a,
            },
        );
        let configured_b = configure_concurrent_mmds_guest(
            &socket_b,
            &kernel_path,
            &rootfs_path,
            ConcurrentMmdsGuestConfig {
                iface_id: CONCURRENT_MMDS_PROCESS_B_IFACE_ID,
                guest_mac: "06:00:00:00:02:02",
                mmds_content: CONCURRENT_MMDS_PROCESS_B_CONTENT,
                boot_args: CONCURRENT_MMDS_PROCESS_B_BOOT_ARGS,
                scratch_path: &scratch_b,
                metrics_path: &metrics_b,
            },
        );
        if configured_a.is_err() || configured_b.is_err() {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "guest configuration",
            );
        }

        if start_concurrent_mmds_guest(&socket_a).is_err()
            || start_concurrent_mmds_guest(&socket_b).is_err()
            || concurrent_mmds_state_is(&socket_a, "Running").is_err()
            || concurrent_mmds_state_is(&socket_b, "Running").is_err()
        {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "concurrent guest startup",
            );
        }

        if wait_for_concurrent_mmds_marker(
            &scratch_b,
            0,
            CONCURRENT_MMDS_PROCESS_B_READY,
            CONCURRENT_MMDS_PROCESS_B_READY_FAILURE,
        )
        .is_err()
            || concurrent_mmds_state_is(&socket_a, "Running").is_err()
            || concurrent_mmds_state_is(&socket_b, "Running").is_err()
        {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process B guest readiness",
            );
        }

        let pause_b = concurrent_mmds_http_json(&socket_b, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        let terminal_before_a_exit = concurrent_mmds_marker_state(
            &scratch_b,
            CONCURRENT_MMDS_PROCESS_B_TERMINAL_OFFSET,
            CONCURRENT_MMDS_PROCESS_B_SUCCESS,
            CONCURRENT_MMDS_PROCESS_B_FAILURE,
        );
        if !matches!(pause_b, Ok(ref response) if concurrent_mmds_response_is_no_content(response))
            || concurrent_mmds_state_is(&socket_b, "Paused").is_err()
            || concurrent_mmds_state_is(&socket_a, "Running").is_err()
            || terminal_before_a_exit != Ok(BlockMarkerState::Pending)
        {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process B pause before process A exit",
            );
        }

        if wait_for_concurrent_mmds_marker(
            &scratch_a,
            0,
            CONCURRENT_MMDS_PROCESS_A_SUCCESS,
            CONCURRENT_MMDS_PROCESS_A_FAILURE,
        )
        .is_err()
        {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process A guest completion",
            );
        }

        let metrics_b_before_a_flush = match fs::read(&metrics_b) {
            Ok(bytes) => bytes,
            Err(_) => fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process B paused metrics read",
            ),
        };
        if flush_concurrent_mmds_metrics(&socket_a).is_err()
            || !concurrent_mmds_metrics_are_isolated(
                &metrics_a,
                CONCURRENT_MMDS_PROCESS_A_IFACE_ID,
                CONCURRENT_MMDS_PROCESS_B_IFACE_ID,
            )
        {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process A metrics isolation",
            );
        }
        let metrics_b_after_a_flush = match fs::read(&metrics_b) {
            Ok(bytes) => bytes,
            Err(_) => fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process B metrics after process A flush",
            ),
        };
        if metrics_b_before_a_flush != metrics_b_after_a_flush {
            fail_concurrent_mmds_pair(
                &mut process_a,
                &mut process_b,
                &private_fragments,
                "process B metrics during process A flush",
            );
        }

        let output_a = process_a.terminate();
        if !output_a.status.success()
            || socket_a.exists()
            || !concurrent_mmds_diagnostics_are_redacted(&output_a, &private_fragments)
        {
            fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process A shutdown",
            );
        }
        let metrics_b_after_a_exit = match fs::read(&metrics_b) {
            Ok(bytes) => bytes,
            Err(_) => fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process B metrics after process A exit",
            ),
        };
        let metrics_a_after_exit = match fs::read(&metrics_a) {
            Ok(bytes) => bytes,
            Err(_) => fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process A final metrics read",
            ),
        };
        if metrics_b_before_a_flush != metrics_b_after_a_exit
            || !socket_b.exists()
            || !scratch_b.exists()
            || !metrics_b.exists()
            || concurrent_mmds_state_is(&socket_b, "Paused").is_err()
        {
            fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process B resources after process A exit",
            );
        }

        let release_b = concurrent_mmds_http_json(
            &socket_b,
            "PATCH",
            "/mmds",
            CONCURRENT_MMDS_PROCESS_B_RELEASE_PATCH,
        );
        let resume_b =
            concurrent_mmds_http_json(&socket_b, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        if !matches!(release_b, Ok(ref response) if concurrent_mmds_response_is_no_content(response))
            || !matches!(resume_b, Ok(ref response) if concurrent_mmds_response_is_no_content(response))
            || concurrent_mmds_state_is(&socket_b, "Running").is_err()
        {
            fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process B release and resume",
            );
        }

        if wait_for_concurrent_mmds_marker(
            &scratch_b,
            CONCURRENT_MMDS_PROCESS_B_TERMINAL_OFFSET,
            CONCURRENT_MMDS_PROCESS_B_SUCCESS,
            CONCURRENT_MMDS_PROCESS_B_FAILURE,
        )
        .is_err()
            || flush_concurrent_mmds_metrics(&socket_b).is_err()
            || !concurrent_mmds_metrics_are_isolated(
                &metrics_b,
                CONCURRENT_MMDS_PROCESS_B_IFACE_ID,
                CONCURRENT_MMDS_PROCESS_A_IFACE_ID,
            )
        {
            fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process B post-shutdown guest completion",
            );
        }
        let metrics_a_after_b_completion = match fs::read(&metrics_a) {
            Ok(bytes) => bytes,
            Err(_) => fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process A retained metrics read",
            ),
        };
        if metrics_a_after_exit != metrics_a_after_b_completion {
            fail_concurrent_mmds_survivor(
                &mut process_b,
                &output_a,
                &private_fragments,
                "process A metrics after process B completion",
            );
        }

        let output_b = process_b.terminate();
        assert!(
            concurrent_mmds_diagnostics_are_redacted(&output_a, &private_fragments)
                && concurrent_mmds_diagnostics_are_redacted(&output_b, &private_fragments),
            "concurrent MMDS diagnostics exposed private test data"
        );
        assert!(
            concurrent_mmds_serial_outputs_are_isolated(&output_a, &output_b),
            "concurrent MMDS guest serial streams crossed process boundaries"
        );
        assert!(
            output_a.status.success() && output_b.status.success(),
            "concurrent MMDS processes should exit cleanly; statuses: {:?}, {:?}",
            output_a.status,
            output_b.status
        );
        assert!(
            !socket_a.exists() && !socket_b.exists(),
            "concurrent MMDS processes should remove only their owned API sockets"
        );
    }

    #[test]
    fn signed_executable_serves_metadata_file_mmds_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "metadata-file MMDS guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            boot_args: DIRECT_ROOTFS_MMDS_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_MARKER,
            network_mtu: None,
            initial_rx_rate_limiter: None,
            wait_for_guest_completion_before_network_patch: false,
            content_source: DirectRootfsMmdsContentSource::MetadataFile,
        });
    }

    #[test]
    fn signed_executable_serves_metadata_file_mmds_v2_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "metadata-file MMDS v2 guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V2","ipv4_address":"169.254.169.254","imds_compat":true}"#,
            boot_args: DIRECT_ROOTFS_MMDS_V2_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_V2_MARKER,
            network_mtu: None,
            initial_rx_rate_limiter: None,
            wait_for_guest_completion_before_network_patch: false,
            content_source: DirectRootfsMmdsContentSource::MetadataFile,
        });
    }

    #[test]
    fn signed_executable_serves_metadata_file_mmds_to_no_api_direct_rootfs_guest() {
        run_direct_rootfs_no_api_mmds_guest_fetch_test(DirectRootfsNoApiMmdsFetchCase {
            request_context: "no-api metadata-file MMDS guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            boot_args: DIRECT_ROOTFS_MMDS_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_MARKER,
        });
    }

    #[test]
    fn signed_executable_serves_mmds_v2_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "MMDS v2 guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V2","ipv4_address":"169.254.169.254","imds_compat":true}"#,
            boot_args: DIRECT_ROOTFS_MMDS_V2_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_V2_MARKER,
            network_mtu: None,
            initial_rx_rate_limiter: None,
            wait_for_guest_completion_before_network_patch: false,
            content_source: DirectRootfsMmdsContentSource::ApiRequest,
        });
    }

    #[test]
    fn signed_executable_serves_metadata_file_mmds_v2_to_no_api_direct_rootfs_guest() {
        run_direct_rootfs_no_api_mmds_guest_fetch_test(DirectRootfsNoApiMmdsFetchCase {
            request_context: "no-api metadata-file MMDS v2 guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V2","ipv4_address":"169.254.169.254","imds_compat":true}"#,
            boot_args: DIRECT_ROOTFS_MMDS_V2_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_V2_MARKER,
        });
    }

    #[test]
    fn signed_executable_handles_guest_initiated_vsock_from_direct_rootfs() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let uds_path = test_dir.path().join("guest-vsock.sock");
        let host_port_path = vsock_port_path(&uds_path, DIRECT_ROOTFS_VSOCK_PORT);
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);
        let host_listener = UnixListener::bind(&host_port_path).unwrap_or_else(|err| {
            panic!(
                "host vsock port listener should bind before guest startup: {:?}",
                err.kind()
            )
        });
        host_listener
            .set_nonblocking(true)
            .expect("host vsock port listener should be nonblocking");

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config guest vsock");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_VSOCK_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source guest vsock");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs guest vsock");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(&data_drive_response, "PUT /drives/data guest vsock");

        let uds_path_json = json_string(path_text(&uds_path));
        let vsock_body = format!(r#"{{"guest_cid":3,"uds_path":{uds_path_json}}}"#);
        let vsock_response = http_put_json(&socket_path, "/vsock", &vsock_body);
        assert_no_content_response(&vsock_response, "PUT /vsock guest vsock");
        assert!(
            !uds_path.exists(),
            "PUT /vsock should not bind the main vsock listener before startup"
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart guest vsock");

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(&running_instance_info, "GET / after guest vsock start");
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after guest vsock start",
        );

        let mut host_stream = match wait_for_unix_listener_accept(
            &host_listener,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            Ok(stream) => stream,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "guest did not initiate vsock connection to host listener: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        drop(host_listener);
        fs::remove_file(&host_port_path).unwrap_or_else(|err| {
            panic!(
                "host vsock port listener path should be removed after accept: {:?}",
                err.kind()
            )
        });

        host_stream
            .set_nonblocking(false)
            .expect("host vsock stream should switch back to blocking mode");
        host_stream
            .set_read_timeout(Some(GUEST_EXECUTION_TIMEOUT))
            .expect("host vsock stream read timeout should set");
        host_stream
            .set_write_timeout(Some(GUEST_EXECUTION_TIMEOUT))
            .expect("host vsock stream write timeout should set");

        let received_guest_bytes = match read_and_verify_deterministic_vsock_stream(
            &mut host_stream,
            DIRECT_ROOTFS_VSOCK_GUEST_STREAM_SEED,
        ) {
            Ok(received) => received,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not verify the guest-initiated guest-to-host vsock stream: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        assert_eq!(
            received_guest_bytes, DIRECT_ROOTFS_VSOCK_STREAM_BYTES,
            "host side should verify the complete guest-to-host vsock byte count"
        );

        let written_host_bytes = match write_deterministic_vsock_stream(
            &mut host_stream,
            DIRECT_ROOTFS_VSOCK_HOST_STREAM_SEED,
        ) {
            Ok(written) => written,
            Err(err) => {
                let guest_failure = direct_rootfs_vsock_failure_phase(&data_backing_path);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write the guest-initiated host-to-guest vsock stream: {err}; guest failure phase: {guest_failure}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        assert_eq!(
            written_host_bytes, DIRECT_ROOTFS_VSOCK_STREAM_BYTES,
            "host side should write the complete host-to-guest vsock byte count"
        );
        if let Err(err) = shutdown_unix_stream_write(&host_stream) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not write-half-close the guest-initiated vsock stream: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_VSOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let guest_failure = direct_rootfs_vsock_failure_phase(&data_backing_path);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete guest-initiated sustained vsock through signed bangbang executable; guest failure phase: {guest_failure}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = read_unix_stream_eof(&mut host_stream) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not observe guest vsock EOF after guest half-close and terminal close: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang direct rootfs guest vsock",
        );
        assert!(
            !uds_path.exists(),
            "bangbang shutdown should remove its owned main vsock listener path"
        );
    }

    #[test]
    fn signed_executable_handles_guest_initiated_vsock_multistream_from_direct_rootfs() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let uds_path = test_dir.path().join("gms.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut host_listeners = Vec::new();
        for &(port, _, _) in DIRECT_ROOTFS_VSOCK_MULTISTREAM_EXCHANGES {
            let host_port_path = vsock_port_path(&uds_path, port);
            let host_listener = UnixListener::bind(&host_port_path).unwrap_or_else(|err| {
                panic!(
                    "host vsock multistream port listener should bind before guest startup: {:?}",
                    err.kind()
                )
            });
            host_listener
                .set_nonblocking(true)
                .expect("host vsock multistream port listener should be nonblocking");
            host_listeners.push((port, host_port_path, host_listener));
        }

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config guest multistream vsock",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_VSOCK_MULTISTREAM_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source guest multistream vsock");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(
            &rootfs_response,
            "PUT /drives/rootfs guest multistream vsock",
        );

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data guest multistream vsock",
        );

        let uds_path_json = json_string(path_text(&uds_path));
        let vsock_body = format!(r#"{{"guest_cid":3,"uds_path":{uds_path_json}}}"#);
        let vsock_response = http_put_json(&socket_path, "/vsock", &vsock_body);
        assert_no_content_response(&vsock_response, "PUT /vsock guest multistream vsock");
        assert!(
            !uds_path.exists(),
            "PUT /vsock should not bind the main vsock listener before startup"
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart guest multistream vsock",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after guest multistream vsock start",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after guest multistream vsock start",
        );

        let mut host_streams = Vec::new();
        for (port, host_port_path, host_listener) in host_listeners {
            let host_stream = match wait_for_unix_listener_accept(
                &host_listener,
                GUEST_EXECUTION_TIMEOUT,
            ) {
                Ok(stream) => stream,
                Err(err) => {
                    let output = bangbang.force_stop_and_collect();
                    panic!(
                        "guest did not initiate multistream vsock connection for port {port}: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                        output.status, output.stdout, output.stderr
                    );
                }
            };
            drop(host_listener);
            fs::remove_file(&host_port_path).unwrap_or_else(|err| {
                panic!(
                    "host vsock multistream port listener path should be removed after accept: {:?}",
                    err.kind()
                )
            });

            host_stream
                .set_nonblocking(false)
                .expect("host vsock multistream stream should switch back to blocking mode");
            host_stream
                .set_read_timeout(Some(GUEST_EXECUTION_TIMEOUT))
                .expect("host vsock multistream stream read timeout should set");
            host_stream
                .set_write_timeout(Some(GUEST_EXECUTION_TIMEOUT))
                .expect("host vsock multistream stream write timeout should set");
            host_streams.push((port, host_stream));
        }

        for (stream_index, ((port, host_stream), &(expected_port, guest_payload, _))) in
            host_streams
                .iter_mut()
                .zip(DIRECT_ROOTFS_VSOCK_MULTISTREAM_EXCHANGES.iter())
                .enumerate()
        {
            assert_eq!(
                *port, expected_port,
                "host vsock multistream stream order should match port {expected_port}"
            );
            let stream_number = stream_index + 1;
            let mut received_guest_payload = vec![0; guest_payload.len()];
            if let Err(err) = host_stream.read_exact(&mut received_guest_payload) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not receive guest multistream payload {stream_number} for port {expected_port}: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    err.kind(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
            if received_guest_payload != guest_payload {
                panic!(
                    "host side did not verify isolated guest multistream payload {stream_number}"
                );
            }
        }

        for (stream_index, ((port, host_stream), &(expected_port, _, host_payload))) in host_streams
            .iter_mut()
            .zip(DIRECT_ROOTFS_VSOCK_MULTISTREAM_EXCHANGES.iter())
            .enumerate()
        {
            assert_eq!(
                *port, expected_port,
                "host vsock multistream reply stream order should match port {expected_port}"
            );
            let stream_number = stream_index + 1;
            if let Err(err) = host_stream.write_all(host_payload) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write guest multistream reply {stream_number} for port {expected_port}: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    err.kind(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
        }

        if wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_VSOCK_MULTISTREAM_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete guest-initiated vsock multistream through signed bangbang executable; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        for (port, host_stream) in &mut host_streams {
            if let Err(err) = read_unix_stream_eof(host_stream) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not observe guest multistream EOF for port {port} after guest close: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang direct rootfs guest multistream vsock",
        );
        assert!(
            !uds_path.exists(),
            "bangbang shutdown should remove its owned main vsock listener path"
        );
    }

    #[test]
    fn signed_executable_handles_host_initiated_vsock_to_direct_rootfs() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let uds_path = test_dir.path().join("host-vsock.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config host-initiated vsock",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_HOST_VSOCK_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source host-initiated vsock");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs host-initiated vsock");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data host-initiated vsock",
        );

        let uds_path_json = json_string(path_text(&uds_path));
        let vsock_body = format!(r#"{{"guest_cid":3,"uds_path":{uds_path_json}}}"#);
        let vsock_response = http_put_json(&socket_path, "/vsock", &vsock_body);
        assert_no_content_response(&vsock_response, "PUT /vsock host-initiated vsock");
        assert!(
            !uds_path.exists(),
            "PUT /vsock should not bind the main vsock listener before startup"
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart host-initiated vsock",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after host-initiated vsock start",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after host-initiated vsock start",
        );

        if wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_READY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not publish host-initiated vsock ready marker; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let mut host_stream = match UnixStream::connect(&uds_path) {
            Ok(stream) => stream,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not connect to the main vsock listener: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    err.kind(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
        };
        host_stream
            .set_read_timeout(Some(GUEST_EXECUTION_TIMEOUT))
            .expect("host-initiated vsock stream read timeout should set");
        host_stream
            .set_write_timeout(Some(GUEST_EXECUTION_TIMEOUT))
            .expect("host-initiated vsock stream write timeout should set");

        let connect_request = format!("CONNECT {DIRECT_ROOTFS_HOST_VSOCK_PORT}\n");
        if let Err(err) = host_stream.write_all(connect_request.as_bytes()) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not write the vsock CONNECT request: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                err.kind(),
                output.status,
                output.stdout,
                output.stderr
            );
        }
        if let Err(err) = read_vsock_connect_ok(&mut host_stream) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not receive the vsock CONNECT OK response: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        let received_guest_bytes = match read_and_verify_deterministic_vsock_stream(
            &mut host_stream,
            DIRECT_ROOTFS_VSOCK_GUEST_STREAM_SEED,
        ) {
            Ok(received) => received,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not verify the host-initiated guest-to-host vsock stream: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        assert_eq!(
            received_guest_bytes, DIRECT_ROOTFS_VSOCK_STREAM_BYTES,
            "host side should verify the complete host-initiated guest-to-host byte count"
        );

        let written_host_bytes = match write_deterministic_vsock_stream(
            &mut host_stream,
            DIRECT_ROOTFS_VSOCK_HOST_STREAM_SEED,
        ) {
            Ok(written) => written,
            Err(err) => {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write the host-initiated host-to-guest vsock stream: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        };
        assert_eq!(
            written_host_bytes, DIRECT_ROOTFS_VSOCK_STREAM_BYTES,
            "host side should write the complete host-initiated host-to-guest byte count"
        );
        if let Err(err) = shutdown_unix_stream_write(&host_stream) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not write-half-close the host-initiated vsock stream: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete host-initiated sustained vsock through signed bangbang executable; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = read_unix_stream_eof(&mut host_stream) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not observe host-initiated vsock EOF after guest half-close and terminal close: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        drop(host_stream);
        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang direct rootfs host-initiated vsock",
        );
        assert!(
            !uds_path.exists(),
            "bangbang shutdown should remove its owned main vsock listener path"
        );
    }

    #[test]
    fn signed_executable_handles_host_initiated_vsock_multistream_to_direct_rootfs() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let uds_path = test_dir.path().join("hms.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(
            &machine_response,
            "PUT /machine-config host multistream vsock",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source host multistream vsock");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(
            &rootfs_response,
            "PUT /drives/rootfs host multistream vsock",
        );

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(
            &data_drive_response,
            "PUT /drives/data host multistream vsock",
        );

        let uds_path_json = json_string(path_text(&uds_path));
        let vsock_body = format!(r#"{{"guest_cid":3,"uds_path":{uds_path_json}}}"#);
        let vsock_response = http_put_json(&socket_path, "/vsock", &vsock_body);
        assert_no_content_response(&vsock_response, "PUT /vsock host multistream vsock");
        assert!(
            !uds_path.exists(),
            "PUT /vsock should not bind the main vsock listener before startup"
        );

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(
            &start_response,
            "PUT /actions InstanceStart host multistream vsock",
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after host multistream vsock start",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after host multistream vsock start",
        );

        if wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_READY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not publish host multistream vsock ready marker; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let mut host_streams = Vec::new();
        for &(port, _, _) in DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_EXCHANGES {
            let mut host_stream = match UnixStream::connect(&uds_path) {
                Ok(stream) => stream,
                Err(err) => {
                    let output = bangbang.force_stop_and_collect();
                    panic!(
                        "host side did not connect multistream port {port} to the main vsock listener: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                        err.kind(),
                        output.status,
                        output.stdout,
                        output.stderr
                    );
                }
            };
            host_stream
                .set_read_timeout(Some(GUEST_EXECUTION_TIMEOUT))
                .expect("host multistream vsock stream read timeout should set");
            host_stream
                .set_write_timeout(Some(GUEST_EXECUTION_TIMEOUT))
                .expect("host multistream vsock stream write timeout should set");

            let connect_request = format!("CONNECT {port}\n");
            if let Err(err) = host_stream.write_all(connect_request.as_bytes()) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write multistream vsock CONNECT request for port {port}: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    err.kind(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
            host_streams.push((port, host_stream));
        }

        let mut acknowledged_local_ports = Vec::new();
        for (port, host_stream) in &mut host_streams {
            let local_port = match read_vsock_connect_ok(host_stream) {
                Ok(local_port) => local_port,
                Err(err) => {
                    let output = bangbang.force_stop_and_collect();
                    panic!(
                        "host side did not receive multistream vsock CONNECT OK response for port {port}: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                        output.status, output.stdout, output.stderr
                    );
                }
            };
            assert!(
                !acknowledged_local_ports.contains(&local_port),
                "host multistream vsock local port {local_port} should be unique"
            );
            acknowledged_local_ports.push(local_port);
        }

        for (stream_index, ((port, host_stream), &(expected_port, guest_payload, _host_payload))) in
            host_streams
                .iter_mut()
                .zip(DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_EXCHANGES.iter())
                .enumerate()
        {
            assert_eq!(
                *port, expected_port,
                "host multistream vsock stream order should match port {expected_port}"
            );
            let stream_number = stream_index + 1;
            let mut received_guest_payload = vec![0; guest_payload.len()];
            if let Err(err) = host_stream.read_exact(&mut received_guest_payload) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not receive guest multistream payload {stream_number} for host port {expected_port}: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    err.kind(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
            if received_guest_payload != guest_payload {
                panic!(
                    "host side did not verify isolated host multistream payload {stream_number}"
                );
            }
        }

        for (stream_index, ((port, host_stream), &(expected_port, _guest_payload, host_payload))) in
            host_streams
                .iter_mut()
                .zip(DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_EXCHANGES.iter())
                .enumerate()
        {
            assert_eq!(
                *port, expected_port,
                "host multistream vsock reply order should match port {expected_port}"
            );
            let stream_number = stream_index + 1;
            if let Err(err) = host_stream.write_all(host_payload) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write multistream vsock reply {stream_number} for host port {expected_port}: {:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    err.kind(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
        }

        if wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        )
        .is_err()
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete host-initiated vsock multistream through signed bangbang executable; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        for (port, host_stream) in &mut host_streams {
            if let Err(err) = read_unix_stream_eof(host_stream) {
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not observe host multistream EOF for port {port} after guest close: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }

        drop(host_streams);
        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang direct rootfs host multistream vsock",
        );
        assert!(
            !uds_path.exists(),
            "bangbang shutdown should remove its owned main vsock listener path"
        );
    }

    fn run_direct_rootfs_mmds_guest_fetch_test(case: DirectRootfsMmdsFetchCase<'_>) {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let metadata_path = test_dir.path().join("metadata.json");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = match case.content_source {
            DirectRootfsMmdsContentSource::ApiRequest => {
                BangbangProcess::start(&socket_path, &instance_id)
            }
            DirectRootfsMmdsContentSource::MetadataFile => {
                fs::write(&metadata_path, DIRECT_ROOTFS_MMDS_CONTENT)
                    .expect("metadata file should be written");
                BangbangProcess::start_with_extra_args(
                    &socket_path,
                    &instance_id,
                    &["--metadata", path_text(&metadata_path)],
                )
            }
        };

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        let machine_context = format!("PUT /machine-config {}", case.request_context);
        assert_no_content_response(&machine_response, &machine_context);

        let mtu_field = case
            .network_mtu
            .map(|mtu| format!(r#", "mtu":{mtu}"#))
            .unwrap_or_default();
        let rx_rate_limiter_field = case
            .initial_rx_rate_limiter
            .map(|rate_limiter| format!(r#", "rx_rate_limiter":{rate_limiter}"#))
            .unwrap_or_default();
        let network_body = format!(
            r#"{{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"06:00:00:00:00:01"{mtu_field}{rx_rate_limiter_field}}}"#
        );
        let network_response =
            http_put_json(&socket_path, "/network-interfaces/eth0", &network_body);
        let network_context = format!("PUT /network-interfaces/eth0 {}", case.request_context);
        assert_no_content_response(&network_response, &network_context);

        let mmds_config_response =
            http_put_json(&socket_path, "/mmds/config", case.mmds_config_body);
        let mmds_config_context = format!("PUT /mmds/config {}", case.request_context);
        assert_no_content_response(&mmds_config_response, &mmds_config_context);

        if let DirectRootfsMmdsContentSource::ApiRequest = case.content_source {
            let mmds_response = http_put_json(&socket_path, "/mmds", DIRECT_ROOTFS_MMDS_CONTENT);
            let mmds_context = format!("PUT /mmds {}", case.request_context);
            assert_no_content_response(&mmds_response, &mmds_context);
        }

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(case.boot_args);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        let boot_context = format!("PUT /boot-source {}", case.request_context);
        assert_no_content_response(&boot_response, &boot_context);

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        let rootfs_context = format!("PUT /drives/rootfs {}", case.request_context);
        assert_no_content_response(&rootfs_response, &rootfs_context);

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        let data_drive_context = format!("PUT /drives/data {}", case.request_context);
        assert_no_content_response(&data_drive_response, &data_drive_context);

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        let start_context = format!("PUT /actions InstanceStart {}", case.request_context);
        assert_no_content_response(&start_response, &start_context);

        let running_instance_info = http_get(&socket_path, "/");
        let get_context = format!("GET / after {} InstanceStart", case.request_context);
        assert_ok_response(&running_instance_info, &get_context);
        assert_response_contains(&running_instance_info, r#""state":"Running""#, &get_context);

        if case.wait_for_guest_completion_before_network_patch {
            assert_direct_rootfs_mmds_guest_completion(&mut bangbang, &data_backing_path, case);
        }

        let network_patch_context = format!(
            "PATCH /network-interfaces/eth0 no-op {}",
            case.request_context
        );
        let network_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","rx_rate_limiter":null,"tx_rate_limiter":null}"#,
        );
        assert_no_content_response(&network_patch_response, &network_patch_context);

        let unknown_network_patch_context =
            format!("PATCH /network-interfaces/eth9 {}", case.request_context);
        let unknown_network_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/network-interfaces/eth9",
            r#"{"iface_id":"eth9"}"#,
        );
        assert_bad_request_response(
            &unknown_network_patch_response,
            &unknown_network_patch_context,
        );
        assert_response_contains(
            &unknown_network_patch_response,
            r#"{"fault_message":"network interface is not configured"}"#,
            &unknown_network_patch_context,
        );
        assert!(
            !unknown_network_patch_response.contains("eth9"),
            "{unknown_network_patch_context} must not echo the rejected iface_id; response:\n{unknown_network_patch_response}"
        );

        let rate_limiter_network_patch_context = format!(
            "PATCH /network-interfaces/eth0 configured rate limiters {}",
            case.request_context
        );
        let rate_limiter_network_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":223456,"one_time_burst":334567,"refill_time":445678}},"tx_rate_limiter":{"ops":{"size":556789,"one_time_burst":667890,"refill_time":778901}}}"#,
        );
        assert_no_content_response(
            &rate_limiter_network_patch_response,
            &rate_limiter_network_patch_context,
        );

        let partial_network_patch_context = format!(
            "PATCH /network-interfaces/eth0 partial rx_rate_limiter {}",
            case.request_context
        );
        let partial_network_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","rx_rate_limiter":{"ops":{"size":889012,"one_time_burst":990123,"refill_time":101234}}}"#,
        );
        assert_no_content_response(
            &partial_network_patch_response,
            &partial_network_patch_context,
        );

        let vm_config_context = format!(
            "GET /vm/config after network PATCH {}",
            case.request_context
        );
        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, &vm_config_context);
        assert_response_contains(&vm_config, r#""iface_id":"eth0""#, &vm_config_context);
        assert_response_contains(
            &vm_config,
            r#""host_dev_name":"vmnet:shared""#,
            &vm_config_context,
        );
        assert_response_contains(
            &vm_config,
            r#""rx_rate_limiter":{"bandwidth":{"one_time_burst":334567,"refill_time":445678,"size":223456},"ops":{"one_time_burst":990123,"refill_time":101234,"size":889012}}"#,
            &vm_config_context,
        );
        assert_response_contains(
            &vm_config,
            r#""tx_rate_limiter":{"ops":{"one_time_burst":667890,"refill_time":778901,"size":556789}}"#,
            &vm_config_context,
        );
        if let Some(mtu) = case.network_mtu {
            assert_response_contains(&vm_config, &format!(r#""mtu":{mtu}"#), &vm_config_context);
        }
        assert!(
            !vm_config.contains(r#""iface_id":"eth9""#),
            "{vm_config_context} must not add the rejected interface; response:\n{vm_config}"
        );

        let disable_network_patch_context = format!(
            "PATCH /network-interfaces/eth0 disable RX bandwidth {}",
            case.request_context
        );
        let disable_network_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":0,"one_time_burst":1234567,"refill_time":100}}}"#,
        );
        assert_no_content_response(
            &disable_network_patch_response,
            &disable_network_patch_context,
        );
        let disabled_vm_config_context = format!(
            "GET /vm/config after disabled RX bandwidth {}",
            case.request_context
        );
        let disabled_vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&disabled_vm_config, &disabled_vm_config_context);
        assert_response_contains(
            &disabled_vm_config,
            r#""rx_rate_limiter":{"ops":{"one_time_burst":990123,"refill_time":101234,"size":889012}}"#,
            &disabled_vm_config_context,
        );
        assert_response_contains(
            &disabled_vm_config,
            r#""tx_rate_limiter":{"ops":{"one_time_burst":667890,"refill_time":778901,"size":556789}}"#,
            &disabled_vm_config_context,
        );
        assert!(
            !disabled_vm_config.contains("223456")
                && !disabled_vm_config.contains("334567")
                && !disabled_vm_config.contains("445678")
                && !disabled_vm_config.contains("1234567"),
            "{disabled_vm_config_context} must clear only the disabled bandwidth bucket; response:\n{disabled_vm_config}"
        );
        if !case.wait_for_guest_completion_before_network_patch {
            assert_direct_rootfs_mmds_guest_completion(&mut bangbang, &data_backing_path, case);
        }

        let shutdown_context = format!("bangbang direct rootfs {}", case.request_context);
        assert_clean_shutdown(bangbang.terminate(), &socket_path, &shutdown_context);
    }

    fn assert_direct_rootfs_mmds_guest_completion(
        bangbang: &mut BangbangProcess,
        data_backing_path: &Path,
        case: DirectRootfsMmdsFetchCase<'_>,
    ) {
        if let Err(err) = wait_for_file_prefix_marker(
            data_backing_path,
            case.success_marker,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete {} through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                case.request_context, output.status, output.stdout, output.stderr
            );
        }
    }

    fn configure_concurrent_mmds_guest(
        socket_path: &Path,
        kernel_path: &Path,
        rootfs_path: &Path,
        config: ConcurrentMmdsGuestConfig<'_>,
    ) -> Result<(), ()> {
        concurrent_mmds_json_no_content(
            socket_path,
            "PUT",
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        )?;

        let network_endpoint = format!("/network-interfaces/{}", config.iface_id);
        let network_body = format!(
            r#"{{"iface_id":"{}","host_dev_name":"vmnet:shared","guest_mac":"{}"}}"#,
            config.iface_id, config.guest_mac
        );
        concurrent_mmds_json_no_content(socket_path, "PUT", &network_endpoint, &network_body)?;

        let mmds_config = format!(
            r#"{{"network_interfaces":["{}"],"version":"V2","ipv4_address":"169.254.169.254"}}"#,
            config.iface_id
        );
        concurrent_mmds_json_no_content(socket_path, "PUT", "/mmds/config", &mmds_config)?;
        concurrent_mmds_json_no_content(socket_path, "PUT", "/mmds", config.mmds_content)?;

        let metrics_body = format!(
            r#"{{"metrics_path":{}}}"#,
            json_string(path_text(config.metrics_path))
        );
        concurrent_mmds_json_no_content(socket_path, "PUT", "/metrics", &metrics_body)?;

        let boot_body = format!(
            r#"{{"kernel_image_path":{},"boot_args":{}}}"#,
            json_string(path_text(kernel_path)),
            json_string(config.boot_args)
        );
        concurrent_mmds_json_no_content(socket_path, "PUT", "/boot-source", &boot_body)?;

        let rootfs_body = format!(
            r#"{{"drive_id":"rootfs","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
            json_string(path_text(rootfs_path))
        );
        concurrent_mmds_json_no_content(socket_path, "PUT", "/drives/rootfs", &rootfs_body)?;

        let scratch_body = format!(
            r#"{{"drive_id":"data","path_on_host":{},"is_root_device":false,"is_read_only":false}}"#,
            json_string(path_text(config.scratch_path))
        );
        concurrent_mmds_json_no_content(socket_path, "PUT", "/drives/data", &scratch_body)
    }

    fn start_concurrent_mmds_guest(socket_path: &Path) -> Result<(), ()> {
        concurrent_mmds_json_no_content(
            socket_path,
            "PUT",
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        )
    }

    fn flush_concurrent_mmds_metrics(socket_path: &Path) -> Result<(), ()> {
        concurrent_mmds_json_no_content(
            socket_path,
            "PUT",
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        )
    }

    fn concurrent_mmds_state_is(socket_path: &Path, expected: &str) -> Result<(), ()> {
        let response = concurrent_mmds_http_request(socket_path, "GET", "/", None)?;
        let state = format!(r#""state":"{expected}""#);
        if response.starts_with("HTTP/1.1 200 OK\r\n") && response.contains(&state) {
            Ok(())
        } else {
            Err(())
        }
    }

    fn concurrent_mmds_json_no_content(
        socket_path: &Path,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<(), ()> {
        let response = concurrent_mmds_http_json(socket_path, method, path, body)?;
        if concurrent_mmds_response_is_no_content(&response) {
            Ok(())
        } else {
            Err(())
        }
    }

    fn concurrent_mmds_http_json(
        socket_path: &Path,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, ()> {
        concurrent_mmds_http_request(socket_path, method, path, Some(body))
    }

    fn concurrent_mmds_http_request(
        socket_path: &Path,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, ()> {
        let io_timeout = Duration::from_secs(5);
        let mut stream = UnixStream::connect(socket_path).map_err(|_| ())?;
        stream.set_read_timeout(Some(io_timeout)).map_err(|_| ())?;
        stream.set_write_timeout(Some(io_timeout)).map_err(|_| ())?;

        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if let Some(body) = body {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            request.push_str(body);
        } else {
            request.push_str("\r\n");
        }
        stream.write_all(request.as_bytes()).map_err(|_| ())?;

        let mut response = String::new();
        stream.read_to_string(&mut response).map_err(|_| ())?;
        Ok(response)
    }

    fn concurrent_mmds_response_is_no_content(response: &str) -> bool {
        response.starts_with("HTTP/1.1 204 No Content\r\n")
            && response.contains("Content-Length: 0\r\n")
            && response.ends_with("\r\n\r\n")
    }

    fn wait_for_concurrent_mmds_marker(
        path: &Path,
        offset: u64,
        success: &[u8],
        failure: &[u8],
    ) -> Result<(), ()> {
        wait_for_file_markers_at(path, &[(offset, success, failure)], GUEST_EXECUTION_TIMEOUT)
            .map_err(|_| ())
    }

    fn concurrent_mmds_marker_state(
        path: &Path,
        offset: u64,
        success: &[u8],
        failure: &[u8],
    ) -> Result<BlockMarkerState, ()> {
        file_marker_state_at(path, offset, success, failure).map_err(|_| ())
    }

    fn concurrent_mmds_metrics_are_isolated(
        path: &Path,
        own_iface_id: &str,
        peer_iface_id: &str,
    ) -> bool {
        let Ok(output) = fs::read_to_string(path) else {
            return false;
        };
        let Some(latest_line) = output.lines().rev().find(|line| !line.is_empty()) else {
            return false;
        };
        let Ok(latest) = serde_json::from_str::<serde_json::Value>(latest_line) else {
            return false;
        };
        let own_key = format!("net_{own_iface_id}");
        let peer_key = format!("net_{peer_iface_id}");
        let Some(own_metrics) = latest.get(&own_key) else {
            return false;
        };
        if latest.get(&peer_key).is_some() || latest.get("mmds").is_none() {
            return false;
        }
        if own_metrics
            .get("event_fails")
            .and_then(serde_json::Value::as_u64)
            != Some(0)
        {
            return false;
        }

        [
            "rx_count",
            "rx_packets_count",
            "tx_count",
            "tx_packets_count",
        ]
        .iter()
        .all(|field| {
            own_metrics
                .get(*field)
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|value| value > 0)
        })
    }

    fn concurrent_mmds_private_fragments(
        test_directory: &Path,
        kernel_path: &Path,
        rootfs_path: &Path,
        instance_a: &str,
        instance_b: &str,
    ) -> Vec<String> {
        let mut fragments = vec![
            path_text(test_directory).to_string(),
            path_text(kernel_path).to_string(),
            path_text(rootfs_path).to_string(),
            instance_a.to_string(),
            instance_b.to_string(),
            CONCURRENT_MMDS_PROCESS_A_VALUE.to_string(),
            CONCURRENT_MMDS_PROCESS_B_VALUE.to_string(),
            CONCURRENT_MMDS_PROCESS_B_PENDING.to_string(),
            CONCURRENT_MMDS_PROCESS_B_RELEASE.to_string(),
        ];
        fragments.extend(
            [
                CONCURRENT_MMDS_PROCESS_A_SUCCESS,
                CONCURRENT_MMDS_PROCESS_A_FAILURE,
                CONCURRENT_MMDS_PROCESS_B_READY,
                CONCURRENT_MMDS_PROCESS_B_READY_FAILURE,
                CONCURRENT_MMDS_PROCESS_B_SUCCESS,
                CONCURRENT_MMDS_PROCESS_B_FAILURE,
            ]
            .into_iter()
            .map(|marker| String::from_utf8_lossy(marker).into_owned()),
        );
        fragments
    }

    fn concurrent_mmds_diagnostics_are_redacted(
        output: &CompletedProcess,
        private_fragments: &[String],
    ) -> bool {
        private_fragments
            .iter()
            .all(|fragment| !output.stderr.contains(fragment.as_str()))
    }

    fn concurrent_mmds_serial_outputs_are_isolated(
        process_a: &CompletedProcess,
        process_b: &CompletedProcess,
    ) -> bool {
        let private_values = [
            CONCURRENT_MMDS_PROCESS_A_VALUE,
            CONCURRENT_MMDS_PROCESS_B_VALUE,
            CONCURRENT_MMDS_PROCESS_B_PENDING,
            CONCURRENT_MMDS_PROCESS_B_RELEASE,
        ];
        private_values
            .iter()
            .all(|value| !process_a.stdout.contains(value) && !process_b.stdout.contains(value))
            && process_a
                .stdout
                .contains(String::from_utf8_lossy(CONCURRENT_MMDS_PROCESS_A_SUCCESS).as_ref())
            && !process_a
                .stdout
                .contains(String::from_utf8_lossy(CONCURRENT_MMDS_PROCESS_B_READY).as_ref())
            && !process_a
                .stdout
                .contains(String::from_utf8_lossy(CONCURRENT_MMDS_PROCESS_B_SUCCESS).as_ref())
            && process_b
                .stdout
                .contains(String::from_utf8_lossy(CONCURRENT_MMDS_PROCESS_B_READY).as_ref())
            && process_b
                .stdout
                .contains(String::from_utf8_lossy(CONCURRENT_MMDS_PROCESS_B_SUCCESS).as_ref())
            && !process_b
                .stdout
                .contains(String::from_utf8_lossy(CONCURRENT_MMDS_PROCESS_A_SUCCESS).as_ref())
    }

    fn fail_concurrent_mmds_pair(
        first: &mut BangbangProcess,
        second: &mut BangbangProcess,
        private_fragments: &[String],
        phase: &str,
    ) -> ! {
        let first_output = first.force_stop_and_collect();
        let second_output = second.force_stop_and_collect();
        assert!(
            concurrent_mmds_diagnostics_are_redacted(&first_output, private_fragments)
                && concurrent_mmds_diagnostics_are_redacted(&second_output, private_fragments),
            "concurrent MMDS diagnostics exposed private test data"
        );
        panic!(
            "concurrent MMDS {phase} failed; process statuses: {:?}, {:?}",
            first_output.status, second_output.status
        );
    }

    fn fail_concurrent_mmds_survivor(
        survivor: &mut BangbangProcess,
        exited_output: &CompletedProcess,
        private_fragments: &[String],
        phase: &str,
    ) -> ! {
        let survivor_output = survivor.force_stop_and_collect();
        assert!(
            concurrent_mmds_diagnostics_are_redacted(exited_output, private_fragments)
                && concurrent_mmds_diagnostics_are_redacted(&survivor_output, private_fragments),
            "concurrent MMDS diagnostics exposed private test data"
        );
        panic!(
            "concurrent MMDS {phase} failed; process statuses: {:?}, {:?}",
            exited_output.status, survivor_output.status
        );
    }

    fn run_direct_rootfs_no_api_mmds_guest_fetch_test(case: DirectRootfsNoApiMmdsFetchCase<'_>) {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let metadata_path = test_dir.path().join("metadata.json");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);
        fs::write(&metadata_path, DIRECT_ROOTFS_MMDS_CONTENT)
            .expect("metadata file should be written");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(case.boot_args);
        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let mmds_config_body = case.mmds_config_body;
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": 1, "mem_size_mib": 256}},
                "network-interfaces": [
                    {{
                        "iface_id": "eth0",
                        "host_dev_name": "vmnet:shared",
                        "guest_mac": "06:00:00:00:00:01"
                    }}
                ],
                "mmds-config": {mmds_config_body},
                "boot-source": {{
                    "kernel_image_path": {kernel_path_json},
                    "boot_args": {boot_args_json}
                }},
                "drives": [
                    {{
                        "drive_id": "rootfs",
                        "path_on_host": {rootfs_path_json},
                        "is_root_device": true,
                        "is_read_only": true
                    }},
                    {{
                        "drive_id": "data",
                        "path_on_host": {data_backing_path_json},
                        "is_root_device": false,
                        "is_read_only": false
                    }}
                ]
            }}"#
        );
        fs::write(&config_path, config).expect("config file should be written");

        let mut bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &[
                "--config-file",
                path_text(&config_path),
                "--metadata",
                path_text(&metadata_path),
                "--no-api",
            ],
        );

        assert!(
            !socket_path.exists(),
            "{} must not publish an API socket",
            case.request_context
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            case.success_marker,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete {} through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                case.request_context, output.status, output.stdout, output.stderr
            );
        }

        let output = bangbang.terminate();
        assert!(
            output.status.success(),
            "{} shutdown signal should make bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            case.request_context,
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            !socket_path.exists(),
            "{} path must leave the API socket absent",
            case.request_context
        );
    }

    #[test]
    fn signed_executable_starts_from_config_file_with_direct_rootfs() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_BOOT_ARGS);
        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": 1, "mem_size_mib": 256}},
                "boot-source": {{
                    "kernel_image_path": {kernel_path_json},
                    "boot_args": {boot_args_json}
                }},
                "drives": [
                    {{
                        "drive_id": "rootfs",
                        "path_on_host": {rootfs_path_json},
                        "is_root_device": true,
                        "is_read_only": true
                    }},
                    {{
                        "drive_id": "data",
                        "path_on_host": {data_backing_path_json},
                        "is_root_device": false,
                        "is_read_only": false
                    }}
                ]
            }}"#
        );
        fs::write(&config_path, config).expect("direct rootfs config file should be written");

        let mut bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path)],
        );

        assert!(
            socket_path.exists(),
            "API-enabled config-file startup should publish an API socket"
        );

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after config-file direct rootfs startup",
        );
        assert_response_contains(
            &running_instance_info,
            &format!(r#""id":"{instance_id}""#),
            "GET / after config-file direct rootfs startup",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after config-file direct rootfs startup",
        );

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(
            &vm_config,
            "GET /vm/config after config-file direct rootfs startup",
        );
        assert_response_contains(
            &vm_config,
            r#""drive_id":"rootfs""#,
            "GET /vm/config after config-file direct rootfs startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{rootfs_path_json}"#),
            "GET /vm/config after config-file direct rootfs startup",
        );
        assert_response_contains(
            &vm_config,
            r#""is_root_device":true"#,
            "GET /vm/config after config-file direct rootfs startup",
        );
        assert_response_contains(
            &vm_config,
            r#""is_read_only":true"#,
            "GET /vm/config after config-file direct rootfs startup",
        );
        assert_response_contains(
            &vm_config,
            r#""drive_id":"data""#,
            "GET /vm/config after config-file direct rootfs startup",
        );
        assert_response_contains(
            &vm_config,
            &format!(r#""path_on_host":{data_backing_path_json}"#),
            "GET /vm/config after config-file direct rootfs startup",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_BLOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "config-file direct rootfs guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang config-file direct rootfs",
        );
    }

    #[test]
    fn signed_executable_exits_after_guest_shutdown_from_config_file() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        write_guest_stop_config(
            &config_path,
            &kernel_path,
            &initrd_path,
            2,
            GUEST_POWEROFF_BOOT_ARGS,
        );

        let bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path)],
        );
        let output = bangbang
            .wait_for_exit_with_timeout(GUEST_EXECUTION_TIMEOUT, "API-enabled guest SYSTEM_OFF");

        assert!(
            output.status.success(),
            "guest SYSTEM_OFF should make API-enabled bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            !socket_path.exists(),
            "guest SYSTEM_OFF should clean up the owned API socket"
        );
    }

    #[test]
    fn signed_executable_exits_after_guest_shutdown_from_no_api_config_file() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        write_guest_stop_config(
            &config_path,
            &kernel_path,
            &initrd_path,
            1,
            GUEST_POWEROFF_BOOT_ARGS,
        );

        let bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path), "--no-api"],
        );
        assert!(
            !socket_path.exists(),
            "guest shutdown no-api startup must not publish an API socket"
        );

        let output =
            bangbang.wait_for_exit_with_timeout(GUEST_EXECUTION_TIMEOUT, "no-api guest SYSTEM_OFF");

        assert!(
            output.status.success(),
            "guest SYSTEM_OFF should make no-api bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            !socket_path.exists(),
            "guest shutdown no-api path must leave the API socket absent"
        );
    }

    #[test]
    fn signed_executable_exits_after_guest_reset_from_config_file() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        write_guest_stop_config(
            &config_path,
            &kernel_path,
            &initrd_path,
            2,
            GUEST_RESET_BOOT_ARGS,
        );

        let bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path)],
        );
        let output = bangbang
            .wait_for_exit_with_timeout(GUEST_EXECUTION_TIMEOUT, "API-enabled guest SYSTEM_RESET");

        assert!(
            output.status.success(),
            "guest SYSTEM_RESET should make API-enabled bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            !socket_path.exists(),
            "guest SYSTEM_RESET should clean up the owned API socket"
        );
    }

    #[test]
    fn signed_executable_exits_after_guest_reset_from_no_api_config_file() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let config_path = test_dir.path().join("vm-config.json");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        write_guest_stop_config(
            &config_path,
            &kernel_path,
            &initrd_path,
            1,
            GUEST_RESET_BOOT_ARGS,
        );

        let bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &["--config-file", path_text(&config_path), "--no-api"],
        );
        assert!(
            !socket_path.exists(),
            "guest reset no-api startup must not publish an API socket"
        );

        let output = bangbang
            .wait_for_exit_with_timeout(GUEST_EXECUTION_TIMEOUT, "no-api guest SYSTEM_RESET");

        assert!(
            output.status.success(),
            "guest SYSTEM_RESET should make no-api bangbang exit successfully; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            !socket_path.exists(),
            "guest reset no-api path must leave the API socket absent"
        );
    }

    #[test]
    fn signed_executable_creates_and_restores_native_v1_snapshot_across_processes() {
        let test_dir = TestDir::new();
        let source_socket = test_dir.path().join("source.socket");
        let paused_socket = test_dir.path().join("paused.socket");
        let resumed_socket = test_dir.path().join("resumed.socket");
        let kernel_path = test_dir.path().join("snapshot-guest.image");
        let root_path = test_dir.path().join("snapshot-root.img");
        let state_path = test_dir.path().join("snapshot.state");
        let memory_path = test_dir.path().join("snapshot.memory");
        let source_metrics = test_dir.path().join("source.metrics");
        let paused_metrics = test_dir.path().join("paused.metrics");
        let instance_id = test_dir.instance_id();

        fs::write(&kernel_path, snapshot_continuity_guest_image())
            .expect("snapshot continuity guest image should be written");
        create_zeroed_block_backing(&root_path);

        let source =
            BangbangProcess::start(&source_socket, &format!("{instance_id}-snapshot-source"));
        let machine = http_put_json(
            &source_socket,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":16,"track_dirty_pages":true}"#,
        );
        assert_no_content_response(&machine, "PUT source /machine-config");
        let boot = http_put_json(
            &source_socket,
            "/boot-source",
            &format!(
                r#"{{"kernel_image_path":{}}}"#,
                json_string(path_text(&kernel_path))
            ),
        );
        assert_no_content_response(&boot, "PUT source /boot-source");
        let drive = http_put_json(
            &source_socket,
            "/drives/root",
            &format!(
                r#"{{"drive_id":"root","path_on_host":{},"is_root_device":true,"is_read_only":true}}"#,
                json_string(path_text(&root_path))
            ),
        );
        assert_no_content_response(&drive, "PUT source /drives/root");
        let metrics = http_put_json(
            &source_socket,
            "/metrics",
            &format!(
                r#"{{"metrics_path":{}}}"#,
                json_string(path_text(&source_metrics))
            ),
        );
        assert_no_content_response(&metrics, "PUT source /metrics");
        let start = http_put_json(
            &source_socket,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start, "PUT source InstanceStart");

        wait_for_uart_write_count(
            &source_socket,
            &source_metrics,
            1,
            GUEST_EXECUTION_TIMEOUT,
            "source snapshot guest readiness",
        );
        let pause = http_json(&source_socket, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(&pause, "PATCH source /vm Paused");

        let create_body = format!(
            r#"{{"snapshot_type":"Full","snapshot_path":{},"mem_file_path":{}}}"#,
            json_string(path_text(&state_path)),
            json_string(path_text(&memory_path))
        );
        let create = http_json_with_io_timeout(
            &source_socket,
            "PUT",
            "/snapshot/create",
            &create_body,
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_no_content_response(&create, "PUT source /snapshot/create");
        assert!(state_path.is_file(), "snapshot state marker should exist");
        assert!(memory_path.is_file(), "snapshot memory image should exist");
        assert_no_snapshot_staging(test_dir.path());
        let state_before = fs::read(&state_path).expect("snapshot state should read");
        let memory_before = fs::read(&memory_path).expect("snapshot memory should read");

        let collision = http_json_with_io_timeout(
            &source_socket,
            "PUT",
            "/snapshot/create",
            &create_body,
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_bad_request_response(&collision, "colliding PUT source /snapshot/create");
        assert_response_contains(
            &collision,
            "failed to create snapshot",
            "colliding PUT source /snapshot/create",
        );
        assert!(!collision.contains(path_text(&state_path)));
        assert!(!collision.contains(path_text(&memory_path)));
        assert_eq!(
            fs::read(&state_path).expect("state should remain readable"),
            state_before
        );
        assert_eq!(
            fs::read(&memory_path).expect("memory should remain readable"),
            memory_before
        );
        assert_no_snapshot_staging(test_dir.path());

        let resume_source = http_json(&source_socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(
            &resume_source,
            "PATCH tracked snapshot source /vm Resumed after commit",
        );
        let repause_source = http_json(&source_socket, "PATCH", "/vm", r#"{"state":"Paused"}"#);
        assert_no_content_response(
            &repause_source,
            "PATCH tracked snapshot source /vm Paused after committed epoch reset",
        );

        let source_output = source.terminate();
        assert_clean_shutdown(source_output, &source_socket, "snapshot source");

        let paused = BangbangProcess::start(
            &paused_socket,
            &format!("{instance_id}-snapshot-paused-destination"),
        );
        let metrics = http_put_json(
            &paused_socket,
            "/metrics",
            &format!(
                r#"{{"metrics_path":{}}}"#,
                json_string(path_text(&paused_metrics))
            ),
        );
        assert_no_content_response(&metrics, "PUT paused destination /metrics");

        let unsupported_state = test_dir.path().join("private-uffd-state");
        let unsupported_memory = test_dir.path().join("private-uffd-memory");
        let unsupported_load = http_json_with_io_timeout(
            &paused_socket,
            "PUT",
            "/snapshot/load",
            &format!(
                r#"{{"snapshot_path":{},"mem_backend":{{"backend_path":{},"backend_type":"Uffd"}}}}"#,
                json_string(path_text(&unsupported_state)),
                json_string(path_text(&unsupported_memory))
            ),
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_bad_request_response(&unsupported_load, "unsupported UFFD snapshot load");
        assert_response_contains(
            &unsupported_load,
            "Snapshot and restore are not supported.",
            "unsupported UFFD snapshot load",
        );
        assert!(!unsupported_load.contains(path_text(&unsupported_state)));
        assert!(!unsupported_load.contains(path_text(&unsupported_memory)));

        let missing_state = test_dir.path().join("private-missing-state");
        let missing_memory = test_dir.path().join("private-missing-memory");
        let missing_load = http_json_with_io_timeout(
            &paused_socket,
            "PUT",
            "/snapshot/load",
            &format!(
                r#"{{"snapshot_path":{},"mem_backend":{{"backend_path":{},"backend_type":"File"}}}}"#,
                json_string(path_text(&missing_state)),
                json_string(path_text(&missing_memory))
            ),
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_bad_request_response(&missing_load, "missing public snapshot load");
        assert_response_contains(
            &missing_load,
            "failed to load snapshot",
            "missing public snapshot load",
        );
        assert!(!missing_load.contains(path_text(&missing_state)));
        assert!(!missing_load.contains(path_text(&missing_memory)));

        let load_paused_body = snapshot_load_body(&state_path, &memory_path, false);
        let load_paused = http_json_with_io_timeout(
            &paused_socket,
            "PUT",
            "/snapshot/load",
            &load_paused_body,
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_no_content_response(&load_paused, "PUT paused destination /snapshot/load");
        let paused_info = http_get(&paused_socket, "/");
        assert_ok_response(&paused_info, "GET paused destination state");
        assert_response_contains(
            &paused_info,
            r#""state":"Paused""#,
            "GET paused destination state",
        );
        let paused_machine = http_get(&paused_socket, "/machine-config");
        assert_ok_response(&paused_machine, "GET tracked paused destination machine");
        assert_response_contains(
            &paused_machine,
            r#""track_dirty_pages":true"#,
            "GET tracked paused destination machine",
        );
        let resume = http_json(&paused_socket, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
        assert_no_content_response(&resume, "PATCH paused destination /vm Resumed");
        let paused_output = paused.wait_for_exit_with_timeout(
            GUEST_EXECUTION_TIMEOUT,
            "restored guest VMGenID change after explicit resume",
        );
        assert_clean_shutdown(
            paused_output,
            &paused_socket,
            "explicitly resumed snapshot destination",
        );

        let resumed = BangbangProcess::start(
            &resumed_socket,
            &format!("{instance_id}-snapshot-auto-destination"),
        );
        let load_resumed_body = snapshot_load_body(&state_path, &memory_path, true);
        let load_resumed = http_json_with_io_timeout(
            &resumed_socket,
            "PUT",
            "/snapshot/load",
            &load_resumed_body,
            GUEST_EXECUTION_TIMEOUT,
        );
        assert_no_content_response(
            &load_resumed,
            "PUT automatic destination /snapshot/load resume_vm",
        );
        let resumed_output = resumed.wait_for_exit_with_timeout(
            GUEST_EXECUTION_TIMEOUT,
            "restored guest VMGenID change after automatic resume",
        );
        assert_clean_shutdown(
            resumed_output,
            &resumed_socket,
            "automatically resumed snapshot destination",
        );

        assert_eq!(
            fs::read(&state_path).expect("final snapshot state should read"),
            state_before
        );
        assert_eq!(
            fs::read(&memory_path).expect("final snapshot memory should read"),
            memory_before
        );
        assert_no_snapshot_staging(test_dir.path());
    }

    #[test]
    fn snapshot_continuity_guest_image_has_expected_header_and_control_flow() {
        let image = snapshot_continuity_guest_image();
        assert_eq!(read_test_u32(&image, 0), 0x1400_0010);
        assert_eq!(read_test_u32(&image, 4), 0xd503_201f);
        assert_eq!(read_test_u64(&image, 8), 0);
        assert_eq!(
            read_test_u64(&image, 16),
            u64::try_from(image.len()).expect("guest image length should fit u64")
        );
        assert_eq!(read_test_u32(&image, 56), SNAPSHOT_GUEST_IMAGE_MAGIC);
        assert_eq!(read_test_u32(&image, 64 + (16 * 4)), aarch64_b_cond(-2, 0));
        assert_eq!(
            read_test_u32(&image, 64 + (20 * 4)),
            aarch64_tbnz_w(11, 0, -6)
        );
        assert_eq!(read_test_u32(&image, 64 + (39 * 4)), 0xd400_0002);
        assert_eq!(read_test_u32(&image, 64 + (40 * 4)), 0x1400_0000);
        assert_eq!(SNAPSHOT_GUEST_VMGENID_ADDRESS, 0x801f_eff0);
        assert_eq!(SNAPSHOT_GUEST_VMCLOCK_ADDRESS, 0x801f_f000);
    }

    fn snapshot_load_body(state_path: &Path, memory_path: &Path, resume_vm: bool) -> String {
        format!(
            r#"{{"snapshot_path":{},"mem_backend":{{"backend_path":{},"backend_type":"File"}},"track_dirty_pages":true,"resume_vm":{resume_vm}}}"#,
            json_string(path_text(state_path)),
            json_string(path_text(memory_path))
        )
    }

    fn flush_memory_hotplug_metrics(
        socket_path: &Path,
        metrics_path: &Path,
        context: &str,
    ) -> serde_json::Value {
        let response = http_put_json(socket_path, "/actions", r#"{"action_type":"FlushMetrics"}"#);
        assert_no_content_response(&response, context);
        let output = fs::read_to_string(metrics_path).unwrap_or_else(|err| {
            panic!(
                "memory-hotplug metrics {} should be readable: {err}",
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
                panic!("{context} should emit memory_hotplug metrics; output:\n{output}")
            })
    }

    fn wait_for_uart_write_count(
        socket_path: &Path,
        metrics_path: &Path,
        expected: u64,
        timeout: Duration,
        context: &str,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            let flush = http_put_json(socket_path, "/actions", r#"{"action_type":"FlushMetrics"}"#);
            assert_no_content_response(&flush, context);
            if latest_uart_write_count(metrics_path).is_some_and(|count| count >= expected) {
                return;
            }
            if Instant::now() >= deadline {
                let metrics = fs::read_to_string(metrics_path)
                    .unwrap_or_else(|err| format!("<metrics unavailable: {err}>"));
                panic!(
                    "{context} did not observe uart.write_count >= {expected} within {timeout:?}; metrics:\n{metrics}"
                );
            }
            std::thread::yield_now();
        }
    }

    fn latest_uart_write_count(path: &Path) -> Option<u64> {
        let output = fs::read_to_string(path).ok()?;
        output.lines().rev().find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("uart")?
                .get("write_count")?
                .as_u64()
        })
    }

    fn metrics_line_count(path: &Path) -> usize {
        fs::read_to_string(path)
            .map(|output| output.lines().count())
            .unwrap_or(0)
    }

    fn entropy_metric_total_since(path: &Path, first_line: usize, key: &str) -> u64 {
        fs::read_to_string(path)
            .ok()
            .into_iter()
            .flat_map(|output| {
                output
                    .lines()
                    .skip(first_line)
                    .filter_map(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .ok()?
                            .get("entropy")?
                            .get(key)?
                            .as_u64()
                    })
                    .collect::<Vec<_>>()
            })
            .fold(0, u64::saturating_add)
    }

    fn wait_for_entropy_metric_since(
        socket_path: &Path,
        metrics_path: &Path,
        first_line: usize,
        key: &str,
        expected: u64,
        timeout: Duration,
    ) -> Result<u64, String> {
        let started_at = Instant::now();
        loop {
            let response =
                http_put_json(socket_path, "/actions", r#"{"action_type":"FlushMetrics"}"#);
            if !response.starts_with("HTTP/1.1 204 No Content\r\n") {
                return Err(format!(
                    "FlushMetrics failed while waiting for entropy.{key}:\n{response}"
                ));
            }
            let observed = entropy_metric_total_since(metrics_path, first_line, key);
            if observed >= expected {
                return Ok(observed);
            }
            if started_at.elapsed() >= timeout {
                let output = fs::read_to_string(metrics_path)
                    .unwrap_or_else(|err| format!("<metrics unavailable: {err}>"));
                return Err(format!(
                    "timed out after {timeout:?} waiting for entropy.{key} >= {expected}; observed={observed}; metrics:\n{output}"
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn assert_no_snapshot_staging(directory: &Path) {
        let staging = fs::read_dir(directory)
            .expect("snapshot directory should remain readable")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| {
                let name = name.to_string_lossy();
                name.starts_with(".bangbang-snapshot-memory-")
                    || name.starts_with(".bangbang-snapshot-state-")
            })
            .collect::<Vec<_>>();
        assert!(
            staging.is_empty(),
            "snapshot staging entries remain: {staging:?}"
        );
    }

    fn assert_capture_ready_snapshot_rejected_without_artifacts(
        socket_path: &Path,
        directory: &Path,
        context: &str,
    ) {
        let state_path = directory.join("capture-ready-rejected.state");
        let memory_path = directory.join("capture-ready-rejected.memory");
        let response = http_json_with_io_timeout(
            socket_path,
            "PUT",
            "/snapshot/create",
            &format!(
                r#"{{"snapshot_type":"Full","snapshot_path":{},"mem_file_path":{}}}"#,
                json_string(path_text(&state_path)),
                json_string(path_text(&memory_path))
            ),
            GUEST_EXECUTION_TIMEOUT,
        );

        assert_bad_request_response(&response, context);
        assert_response_contains(
            &response,
            "Snapshot and restore are not supported.",
            context,
        );
        assert!(!response.contains(path_text(&state_path)));
        assert!(!response.contains(path_text(&memory_path)));
        assert!(!state_path.exists());
        assert!(!memory_path.exists());
        assert_no_snapshot_staging(directory);
    }

    fn snapshot_continuity_guest_image() -> Vec<u8> {
        assert_eq!(SNAPSHOT_GUEST_VMGENID_ADDRESS >> 32, 0);
        assert_eq!(SNAPSHOT_GUEST_VMCLOCK_ADDRESS >> 32, 0);
        assert_eq!(SNAPSHOT_GUEST_RTC_ADDRESS >> 32, 0);
        assert_eq!(SNAPSHOT_GUEST_UART_ADDRESS >> 32, 0);
        let instructions = [
            aarch64_movz_x(1, low_u16(SNAPSHOT_GUEST_VMGENID_ADDRESS, 0), 0),
            aarch64_movk_x(1, low_u16(SNAPSHOT_GUEST_VMGENID_ADDRESS, 16), 16),
            aarch64_ldp_x(2, 3, 1),
            aarch64_movz_x(8, low_u16(SNAPSHOT_GUEST_VMCLOCK_ADDRESS, 0), 0),
            aarch64_movk_x(8, low_u16(SNAPSHOT_GUEST_VMCLOCK_ADDRESS, 16), 16),
            aarch64_ldr_x(9, 8, 16),
            aarch64_ldr_x(10, 8, 104),
            aarch64_movz_x(15, low_u16(SNAPSHOT_GUEST_RTC_ADDRESS, 0), 0),
            aarch64_movk_x(15, low_u16(SNAPSHOT_GUEST_RTC_ADDRESS, 16), 16),
            aarch64_ldr_w(16, 15, 0),
            aarch64_movz_x(4, low_u16(SNAPSHOT_GUEST_UART_ADDRESS, 0), 0),
            aarch64_movk_x(4, low_u16(SNAPSHOT_GUEST_UART_ADDRESS, 16), 16),
            aarch64_movz_x(7, u16::from(b'R'), 0),
            aarch64_strb_w(7, 4),
            aarch64_ldp_x(5, 6, 1),
            aarch64_cmp_x(5, 2),
            aarch64_b_cond(-2, 0), // b.eq poll
            aarch64_cmp_x(6, 3),
            aarch64_b_cond(-4, 0), // b.eq poll
            aarch64_ldr_w(11, 8, 12),
            aarch64_tbnz_w(11, 0, -6),
            0xd503_39bf, // dmb ishld
            aarch64_ldr_x(12, 8, 16),
            aarch64_ldr_x(13, 8, 104),
            0xd503_39bf, // dmb ishld
            aarch64_ldr_w(14, 8, 12),
            aarch64_cmp_w(14, 11),
            aarch64_b_cond(-13, 1), // b.ne poll
            aarch64_cmp_x(12, 9),
            aarch64_b_cond(-15, 0), // b.eq poll
            aarch64_cmp_x(13, 10),
            aarch64_b_cond(-17, 0), // b.eq poll
            aarch64_ldr_w(17, 15, 0),
            aarch64_cmp_w(17, 16),
            aarch64_b_cond(-20, 3), // b.lo poll
            aarch64_movz_x(7, u16::from(b'C'), 0),
            aarch64_strb_w(7, 4),
            aarch64_movz_x(0, 0x0008, 0),
            aarch64_movk_x(0, 0x8400, 16),
            0xd400_0002, // hvc #0 (PSCI_SYSTEM_OFF)
            0x1400_0000, // b . if the host unexpectedly returns
        ];

        let mut image = vec![0; SNAPSHOT_GUEST_IMAGE_HEADER_SIZE];
        write_test_u32(&mut image, 0, 0x1400_0010); // branch from header to offset 64
        write_test_u32(&mut image, 4, 0xd503_201f); // nop
        write_test_u64(&mut image, 8, 0); // text_offset
        write_test_u32(&mut image, 56, SNAPSHOT_GUEST_IMAGE_MAGIC);
        image.extend(instructions.into_iter().flat_map(u32::to_le_bytes));
        let image_size = u64::try_from(image.len()).expect("guest image length should fit u64");
        write_test_u64(&mut image, 16, image_size);
        image
    }

    fn aarch64_movz_x(register: u32, immediate: u16, shift: u32) -> u32 {
        assert!(register <= 30);
        assert!(shift <= 48 && shift.is_multiple_of(16));
        0xd280_0000 | ((shift / 16) << 21) | (u32::from(immediate) << 5) | register
    }

    fn aarch64_movk_x(register: u32, immediate: u16, shift: u32) -> u32 {
        assert!(register <= 30);
        assert!(shift <= 48 && shift.is_multiple_of(16));
        0xf280_0000 | ((shift / 16) << 21) | (u32::from(immediate) << 5) | register
    }

    fn aarch64_ldp_x(first: u32, second: u32, base: u32) -> u32 {
        assert!(first <= 30 && second <= 30 && base <= 30);
        0xa940_0000 | (second << 10) | (base << 5) | first
    }

    fn aarch64_ldr_x(destination: u32, base: u32, byte_offset: u32) -> u32 {
        assert!(destination <= 30 && base <= 30);
        assert!(byte_offset.is_multiple_of(8) && byte_offset / 8 <= 0xfff);
        0xf940_0000 | ((byte_offset / 8) << 10) | (base << 5) | destination
    }

    fn aarch64_ldr_w(destination: u32, base: u32, byte_offset: u32) -> u32 {
        assert!(destination <= 30 && base <= 30);
        assert!(byte_offset.is_multiple_of(4) && byte_offset / 4 <= 0xfff);
        0xb940_0000 | ((byte_offset / 4) << 10) | (base << 5) | destination
    }

    fn aarch64_cmp_x(left: u32, right: u32) -> u32 {
        assert!(left <= 30 && right <= 30);
        0xeb00_001f | (right << 16) | (left << 5)
    }

    fn aarch64_cmp_w(left: u32, right: u32) -> u32 {
        assert!(left <= 30 && right <= 30);
        0x6b00_001f | (right << 16) | (left << 5)
    }

    fn aarch64_b_cond(instruction_offset: i32, condition: u32) -> u32 {
        assert!((-262_144..262_144).contains(&instruction_offset));
        assert!(condition <= 0xf);
        let immediate = instruction_offset.cast_unsigned() & 0x7_ffff;
        0x5400_0000 | (immediate << 5) | condition
    }

    fn aarch64_tbnz_w(register: u32, bit: u32, instruction_offset: i32) -> u32 {
        assert!(register <= 30 && bit <= 31);
        assert!((-8192..8192).contains(&instruction_offset));
        let immediate = instruction_offset.cast_unsigned() & 0x3fff;
        0x3700_0000 | (bit << 19) | (immediate << 5) | register
    }

    fn aarch64_strb_w(source: u32, base: u32) -> u32 {
        assert!(source <= 30 && base <= 30);
        0x3900_0000 | (base << 5) | source
    }

    fn low_u16(value: u64, shift: u32) -> u16 {
        u16::try_from((value >> shift) & u64::from(u16::MAX))
            .expect("masked immediate should fit u16")
    }

    fn write_test_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_test_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn read_test_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("u32 test range should fit"),
        )
    }

    fn read_test_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("u64 test range should fit"),
        )
    }

    fn env_path(name: &str) -> PathBuf {
        match std::env::var_os(name) {
            Some(value) if value.is_empty() => panic!("{name} must not be empty"),
            Some(value) => PathBuf::from(value),
            None => panic!("{name} must be set"),
        }
    }

    fn vsock_port_path(uds_path: &Path, port: u32) -> PathBuf {
        let mut path = uds_path.as_os_str().to_os_string();
        path.push(format!("_{port}"));
        PathBuf::from(path)
    }

    fn write_guest_stop_config(
        config_path: &Path,
        kernel_path: &Path,
        initrd_path: &Path,
        vcpu_count: u8,
        boot_args: &str,
    ) {
        let kernel_path_json = json_string(path_text(kernel_path));
        let initrd_path_json = json_string(path_text(initrd_path));
        let boot_args_json = json_string(boot_args);
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": {vcpu_count}, "mem_size_mib": 256}},
                "boot-source": {{
                    "kernel_image_path": {kernel_path_json},
                    "initrd_path": {initrd_path_json},
                    "boot_args": {boot_args_json}
                }}
            }}"#
        );
        fs::write(config_path, config).expect("guest stop config file should be written");
    }

    fn configure_public_smp_progress(
        socket_path: &Path,
        kernel_path: &Path,
        initrd_path: &Path,
        serial_path: &Path,
        context: &str,
    ) {
        let machine = http_put_json(
            socket_path,
            "/machine-config",
            r#"{"vcpu_count":2,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine, &format!("PUT {context} /machine-config"));
        let boot = http_put_json(
            socket_path,
            "/boot-source",
            &format!(
                r#"{{"kernel_image_path":{},"initrd_path":{},"boot_args":{}}}"#,
                json_string(path_text(kernel_path)),
                json_string(path_text(initrd_path)),
                json_string(GUEST_SMP_PROGRESS_BOOT_ARGS),
            ),
        );
        assert_no_content_response(&boot, &format!("PUT {context} /boot-source"));
        let serial = http_put_json(
            socket_path,
            "/serial",
            &format!(
                r#"{{"serial_out_path":{}}}"#,
                json_string(path_text(serial_path))
            ),
        );
        assert_no_content_response(&serial, &format!("PUT {context} /serial"));
        let start = http_put_json(
            socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start, &format!("PUT {context} InstanceStart"));
    }

    fn configure_public_smp_hotplug(
        socket_path: &Path,
        kernel_path: &Path,
        initrd_path: &Path,
        serial_path: &Path,
    ) {
        let machine = http_put_json(
            socket_path,
            "/machine-config",
            r#"{"vcpu_count":2,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine, "PUT hotplug /machine-config");
        let boot = http_put_json(
            socket_path,
            "/boot-source",
            &format!(
                r#"{{"kernel_image_path":{},"initrd_path":{},"boot_args":{}}}"#,
                json_string(path_text(kernel_path)),
                json_string(path_text(initrd_path)),
                json_string(GUEST_SMP_HOTPLUG_BOOT_ARGS),
            ),
        );
        assert_no_content_response(&boot, "PUT hotplug /boot-source");
        let serial = http_put_json(
            socket_path,
            "/serial",
            &format!(
                r#"{{"serial_out_path":{}}}"#,
                json_string(path_text(serial_path))
            ),
        );
        assert_no_content_response(&serial, "PUT hotplug /serial");
        let start = http_put_json(
            socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start, "PUT hotplug InstanceStart");
    }

    fn create_zeroed_block_backing(path: &Path) {
        create_zeroed_block_backing_with_sectors(path, 1);
    }

    fn create_zeroed_block_backing_with_sectors(path: &Path, sectors: u64) {
        let len = bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE
            .checked_mul(sectors)
            .expect("guest block backing sector count should not overflow");
        assert!(len > 0, "guest block backing should not be empty");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("guest block backing should create");
        file.set_len(len)
            .expect("guest block backing should have requested sectors");
    }

    fn reset_zeroed_block_backing(path: &Path, sectors: u64) {
        let len = bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE
            .checked_mul(sectors)
            .expect("reused guest block backing sector count should not overflow");
        assert!(len > 0, "reused guest block backing should not be empty");
        let file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .expect("guest block backing should reopen for deterministic reuse");
        file.set_len(len)
            .expect("reused guest block backing should have requested sectors");
        file.sync_all()
            .expect("reused guest block backing should persist its reset");
    }

    fn create_block_backing_with_prefix(path: &Path, sectors: u64, marker: &[u8]) {
        create_zeroed_block_backing_with_sectors(path, sectors);
        if !marker.is_empty() {
            write_block_marker_at(path, 0, marker);
        }
    }

    fn write_block_marker_at(path: &Path, offset: u64, marker: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("guest block backing should open for marker write");
        file.seek(SeekFrom::Start(offset))
            .expect("guest block marker offset should seek");
        file.write_all(marker)
            .expect("guest block marker should write");
        file.sync_all().expect("guest block marker should fsync");
    }

    fn write_virtual_block_marker_at(media: &MacosVirtualBlock, offset: u64, marker: &[u8]) {
        let mut sector = vec![0_u8; bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE as usize];
        sector[..marker.len()].copy_from_slice(marker);
        media
            .write_at(offset, &sector)
            .expect("virtual block marker should persist");
    }

    fn create_pmem_backing(path: &Path, marker: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .expect("guest pmem backing should create");
        file.set_len(bangbang_runtime::pmem::VIRTIO_PMEM_ALIGNMENT)
            .expect("guest pmem backing should be aligned");
        file.write_all(marker)
            .expect("guest pmem host marker should write");
    }

    fn create_empty_file(path: &Path) {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("empty test output file should create");
    }

    fn assert_metrics_output(
        path: &Path,
        expected_get_api_requests: Option<&str>,
        expected_put_api_requests: &str,
        expected_patch_api_requests: Option<&str>,
    ) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable: {err}",
                path.display()
            )
        });
        let lines = output
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|err| {
                    panic!("metrics output line should be valid JSON: {err}; line:\n{line}")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lines.len(),
            2,
            "session initial and explicit flush should emit two metrics lines; output:\n{output}"
        );
        let sum_section = |section: &str| {
            let mut total = serde_json::Map::new();
            for line in &lines {
                let Some(fields) = line.get(section).and_then(serde_json::Value::as_object) else {
                    continue;
                };
                for (field, value) in fields {
                    let value = value.as_u64().unwrap_or_else(|| {
                        panic!("metrics field {section}.{field} should be an integer: {value}")
                    });
                    let entry = total
                        .entry(field.clone())
                        .or_insert(serde_json::Value::Number(0_u64.into()));
                    let current = entry.as_u64().unwrap_or_else(|| {
                        panic!("summed metrics field {section}.{field} should be an integer")
                    });
                    *entry = serde_json::Value::Number(current.saturating_add(value).into());
                }
            }
            (!total.is_empty()).then_some(serde_json::Value::Object(total))
        };

        assert!(
            output.contains(r#""metrics_flush_count":1"#),
            "metrics output should include first flush count; output:\n{output}"
        );
        if let Some(expected_get_api_requests) = expected_get_api_requests {
            let expected = serde_json::from_str(expected_get_api_requests)
                .expect("expected GET API request metrics should be valid JSON");
            assert_eq!(
                sum_section("get_api_requests"),
                Some(expected),
                "metrics output should sum to expected GET API request counters; output:\n{output}"
            );
        }
        let expected = serde_json::from_str(expected_put_api_requests)
            .expect("expected PUT API request metrics should be valid JSON");
        assert_eq!(
            sum_section("put_api_requests"),
            Some(expected),
            "metrics output should sum to expected PUT API request counters; output:\n{output}"
        );
        if let Some(expected_patch_api_requests) = expected_patch_api_requests {
            let expected = serde_json::from_str(expected_patch_api_requests)
                .expect("expected PATCH API request metrics should be valid JSON");
            assert_eq!(
                sum_section("patch_api_requests"),
                Some(expected),
                "metrics output should sum to expected PATCH API request counters; output:\n{output}"
            );
        } else {
            assert_eq!(
                sum_section("patch_api_requests"),
                None,
                "metrics output should not include PATCH API request counters; output:\n{output}"
            );
        }
        assert!(
            output.contains(r#""boot_run_loop_status":"running""#)
                || output.contains(r#""boot_run_loop_status":"exited""#),
            "metrics output should include a non-failed boot run-loop status; output:\n{output}"
        );
        assert!(
            !output.contains(r#""boot_run_loop_status":"failed""#),
            "metrics output should not report failed boot run-loop status; output:\n{output}"
        );
    }

    fn assert_normal_terminal_metrics_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "terminal metrics output {} should be readable: {err}",
                path.display()
            )
        });
        let lines = output
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|err| {
                    panic!("terminal metrics line should be valid JSON: {err}; line:\n{line}")
                })
            })
            .collect::<Vec<_>>();

        assert_eq!(
            lines.len(),
            3,
            "session initial, explicit, and normal-terminal attempts should emit three metrics lines; output:\n{output}"
        );
        assert_eq!(
            lines
                .last()
                .and_then(|line| line.pointer("/vmm/metrics_flush_count"))
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "normal-terminal metrics line should carry the per-success flush marker; output:\n{output}"
        );
    }

    fn assert_multi_interface_network_metrics(path: &Path, iface_ids: &[&str]) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable for multi-interface network metrics: {err}",
                path.display()
            )
        });
        let latest_line = output
            .lines()
            .rev()
            .find(|line| !line.is_empty())
            .unwrap_or_else(|| panic!("metrics output should contain a JSON line: {output}"));
        let latest: serde_json::Value = serde_json::from_str(latest_line).unwrap_or_else(|err| {
            panic!("latest metrics output line should be valid JSON: {err}; line:\n{latest_line}")
        });

        for iface_id in iface_ids {
            let key = format!("net_{iface_id}");
            let metrics = latest.get(&key).unwrap_or_else(|| {
                panic!("latest metrics output should include {key}; line:\n{latest_line}")
            });
            for field in [
                "rx_count",
                "rx_packets_count",
                "tx_count",
                "tx_packets_count",
            ] {
                let value = metrics
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(|| {
                        panic!(
                            "latest metrics output should include numeric {key}.{field}; line:\n{latest_line}"
                        )
                    });
                assert!(
                    value > 0,
                    "latest metrics output should report nonzero {key}.{field}; line:\n{latest_line}"
                );
            }
            assert_eq!(
                metrics
                    .get("event_fails")
                    .and_then(serde_json::Value::as_u64),
                Some(0),
                "latest metrics output should report no {key} event failures; line:\n{latest_line}"
            );
        }
        assert!(
            latest.get("mmds").is_some(),
            "latest metrics output should include shared MMDS activity; line:\n{latest_line}"
        );
    }

    fn assert_vm_state_latency_metrics_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable for VM state latency metrics: {err}",
                path.display()
            )
        });

        let mut found_vm_state_latencies = false;
        for line in output.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|err| {
                panic!("metrics output line should be valid JSON: {err}; line:\n{line}")
            });
            let Some(latencies) = value.get("latencies_us") else {
                continue;
            };
            if latencies
                .get("pause_vm")
                .and_then(serde_json::Value::as_u64)
                .is_some()
                && latencies
                    .get("resume_vm")
                    .and_then(serde_json::Value::as_u64)
                    .is_some()
            {
                found_vm_state_latencies = true;
            }
        }

        assert!(
            found_vm_state_latencies,
            "metrics output should include numeric pause/resume VM state latencies; output:\n{output}"
        );
    }

    fn assert_block_update_metrics_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable for block update metrics: {err}",
                path.display()
            )
        });

        let mut found_block_update_metrics = false;
        for line in output.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|err| {
                panic!("metrics output line should be valid JSON: {err}; line:\n{line}")
            });
            let Some(block) = value.get("block") else {
                continue;
            };
            let Some(data) = value.get("block_data") else {
                continue;
            };
            let Some(scratch) = value.get("block_scratch") else {
                continue;
            };
            if block
                .get("update_count")
                .and_then(serde_json::Value::as_u64)
                == Some(2)
                && block
                    .get("update_fails")
                    .and_then(serde_json::Value::as_u64)
                    == Some(0)
                && data.get("update_count").and_then(serde_json::Value::as_u64) == Some(1)
                && data.get("update_fails").and_then(serde_json::Value::as_u64) == Some(0)
                && scratch
                    .get("update_count")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
                && scratch
                    .get("update_fails")
                    .and_then(serde_json::Value::as_u64)
                    == Some(0)
            {
                found_block_update_metrics = true;
            }
        }

        assert!(
            found_block_update_metrics,
            "metrics output should include block metrics after drive update; output:\n{output}"
        );
    }

    fn assert_startup_time_metrics_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable: {err}",
                path.display()
            )
        });

        assert!(
            output.contains(r#""process_startup_time_us":0"#),
            "metrics output should include process_startup_time_us; output:\n{output}"
        );
        assert!(
            output.contains(r#""process_startup_time_cpu_us":3000"#),
            "metrics output should include process_startup_time_cpu_us; output:\n{output}"
        );
        assert!(
            output.contains(r#""api_server""#),
            "metrics output should include api_server metrics object; output:\n{output}"
        );
    }

    fn assert_sigpipe_signal_metrics_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable for signal metrics: {err}",
                path.display()
            )
        });
        assert!(
            output.contains(r#""signals":{"sigpipe":1}"#),
            "metrics output should include SIGPIPE signal metrics; output:\n{output}"
        );
    }

    #[derive(Clone, Copy)]
    enum LoggerPrefixExpectation {
        None,
        LevelOrigin,
    }

    fn assert_logger_output(path: &Path, prefix: LoggerPrefixExpectation) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!("logger output {} should be readable: {err}", path.display())
        });

        assert_logger_output_lines(&output, prefix);
    }

    fn assert_logger_output_lines(output: &str, prefix: LoggerPrefixExpectation) {
        let mut action_lines = Vec::new();
        let mut saw_api_request_line = false;
        for line in output.lines() {
            let record = logger_record_without_prefix(line, prefix, output);
            if record.starts_with("action=") {
                action_lines.push(record);
                continue;
            }

            assert!(
                is_api_request_logger_line(record),
                "logger output line should be an action record or API request record; output:\n{output}\nline: {line}"
            );
            saw_api_request_line = true;
        }

        const EXPECTED_ACTION_LINES: &[&str] = &["action=InstanceStart", "action=FlushMetrics"];
        assert_eq!(
            action_lines.as_slice(),
            EXPECTED_ACTION_LINES,
            "logger output should include the expected action records"
        );
        assert!(
            saw_api_request_line,
            "logger output should include at least one API request record; output:\n{output}"
        );
    }

    fn logger_record_without_prefix<'a>(
        line: &'a str,
        prefix: LoggerPrefixExpectation,
        output: &str,
    ) -> &'a str {
        match prefix {
            LoggerPrefixExpectation::None => line,
            LoggerPrefixExpectation::LevelOrigin => {
                let Some(rest) = line.strip_prefix("level=Info origin=") else {
                    panic!(
                        "logger output line should include level and origin prefix; output:\n{output}\nline: {line}"
                    );
                };
                let Some((origin, record)) = rest.split_once(' ') else {
                    panic!(
                        "logger output line should include origin and record body; output:\n{output}\nline: {line}"
                    );
                };
                let Some((file, line_number)) = origin.rsplit_once(':') else {
                    panic!(
                        "logger output origin should include file and line; output:\n{output}\nline: {line}"
                    );
                };
                assert!(
                    !file.is_empty(),
                    "logger output origin file should not be empty; output:\n{output}\nline: {line}"
                );
                assert!(
                    line_number.parse::<u32>().is_ok(),
                    "logger output origin line should be numeric; output:\n{output}\nline: {line}"
                );
                record
            }
        }
    }

    fn is_api_request_logger_line(line: &str) -> bool {
        let Some(rest) = line.strip_prefix("The API server received a ") else {
            return false;
        };
        let Some((method, path)) = rest.split_once(" request on \"") else {
            return false;
        };
        let Some(path) = path.strip_suffix("\".") else {
            return false;
        };

        !method.is_empty() && path.starts_with('/')
    }

    fn assert_no_api_logger_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!("logger output {} should be readable: {err}", path.display())
        });

        assert_eq!(
            output, "action=InstanceStart\n",
            "no-api logger output should include only the startup action record"
        );
    }

    #[test]
    fn logger_output_accepts_action_records_with_api_request_lines() {
        assert_logger_output_lines(
            "The API server received a Get request on \"/\".\n\
             action=InstanceStart\n\
             The API server received a Put request on \"/actions\".\n\
             action=FlushMetrics\n",
            LoggerPrefixExpectation::None,
        );
    }

    #[test]
    fn logger_output_accepts_level_origin_prefixed_records() {
        assert_logger_output_lines(
            "level=Info origin=crates/runtime/src/logger.rs:1 The API server received a Get request on \"/\".\n\
             level=Info origin=crates/runtime/src/logger.rs:2 action=InstanceStart\n\
             level=Info origin=crates/runtime/src/logger.rs:3 The API server received a Put request on \"/actions\".\n\
             level=Info origin=crates/runtime/src/logger.rs:4 action=FlushMetrics\n",
            LoggerPrefixExpectation::LevelOrigin,
        );
    }

    #[test]
    #[should_panic(expected = "logger output line should be an action record")]
    fn logger_output_rejects_unexpected_non_action_line() {
        assert_logger_output_lines(
            "action=InstanceStart\nunexpected\n",
            LoggerPrefixExpectation::None,
        );
    }

    #[test]
    #[should_panic(expected = "logger output should include the expected action records")]
    fn logger_output_rejects_missing_action_record() {
        assert_logger_output_lines(
            "The API server received a Get request on \"/\".\n",
            LoggerPrefixExpectation::None,
        );
    }

    #[test]
    #[should_panic(expected = "logger output should include at least one API request record")]
    fn logger_output_rejects_missing_api_request_record() {
        assert_logger_output_lines(
            "action=InstanceStart\naction=FlushMetrics\n",
            LoggerPrefixExpectation::None,
        );
    }

    #[test]
    #[should_panic(expected = "logger output line should include level and origin prefix")]
    fn logger_output_rejects_missing_required_prefix() {
        assert_logger_output_lines(
            "The API server received a Get request on \"/\".\n\
             action=InstanceStart\n\
             action=FlushMetrics\n",
            LoggerPrefixExpectation::LevelOrigin,
        );
    }

    #[test]
    #[should_panic(expected = "logger output origin line should be numeric")]
    fn logger_output_rejects_malformed_origin_line() {
        assert_logger_output_lines(
            "level=Info origin=crates/runtime/src/logger.rs:not-a-line The API server received a Get request on \"/\".\n\
             level=Info origin=crates/runtime/src/logger.rs:2 action=InstanceStart\n\
             level=Info origin=crates/runtime/src/logger.rs:3 action=FlushMetrics\n",
            LoggerPrefixExpectation::LevelOrigin,
        );
    }

    fn smp_progress_counts(path: &Path) -> Result<SmpProgressCounts, String> {
        read_smp_progress_counts(path)
    }

    fn read_smp_progress_counts(path: &Path) -> Result<SmpProgressCounts, String> {
        let bytes = fs::read(path)
            .map_err(|err| format!("failed to read SMP progress {}: {err}", path.display()))?;
        Ok(SmpProgressCounts {
            cpu0: bytes
                .iter()
                .filter(|byte| **byte == SMP_PROGRESS_CPU0_TOKEN)
                .count(),
            cpu1: bytes
                .iter()
                .filter(|byte| **byte == SMP_PROGRESS_CPU1_TOKEN)
                .count(),
        })
    }

    fn wait_for_smp_progress_counts(
        path: &Path,
        target: SmpProgressCounts,
        timeout: Duration,
    ) -> Result<SmpProgressCounts, String> {
        let file = fs::File::open(path).map_err(|err| {
            format!(
                "failed to open SMP progress serial output {}: {err}",
                path.display()
            )
        })?;
        let kqueue = Kqueue::new()?;
        kqueue.watch_writes(&file)?;
        let started_at = Instant::now();

        loop {
            let counts = read_smp_progress_counts(path)?;
            if counts.cpu0 >= target.cpu0 && counts.cpu1 >= target.cpu1 {
                return Ok(counts);
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {timeout:?} waiting for CPU0/CPU1 SMP progress counts {target:?} in {}",
                    path.display()
                ));
            };
            kqueue.wait_for_write(remaining)?;
        }
    }

    fn wait_for_smp_progress_or_collect(
        path: &Path,
        target: SmpProgressCounts,
        first: &mut BangbangProcess,
        second: &mut BangbangProcess,
        context: &str,
    ) -> SmpProgressCounts {
        match wait_for_smp_progress_counts(path, target, GUEST_EXECUTION_TIMEOUT) {
            Ok(counts) => counts,
            Err(err) => {
                let serial_tail = match fs::read(path) {
                    Ok(bytes) => {
                        let start = bytes.len().saturating_sub(512);
                        format!("{:02x?}", &bytes[start..])
                    }
                    Err(read_err) => format!("<failed to read serial tail: {read_err}>"),
                };
                let first_output = first.force_stop_and_collect();
                let second_output = second.force_stop_and_collect();
                panic!(
                    "{context} failed: {err}; serial tail: {serial_tail}; first status: {:?}\nfirst stdout:\n{}\nfirst stderr:\n{}\nsecond status: {:?}\nsecond stdout:\n{}\nsecond stderr:\n{}",
                    first_output.status,
                    first_output.stdout,
                    first_output.stderr,
                    second_output.status,
                    second_output.stdout,
                    second_output.stderr
                );
            }
        }
    }

    fn wait_for_file_markers_at(
        path: &Path,
        expectations: &[(u64, &[u8], &[u8])],
        timeout: Duration,
    ) -> Result<(), String> {
        let file = fs::File::open(path).map_err(|err| {
            format!(
                "failed to open block backing {} for marker wait: {err}",
                path.display()
            )
        })?;
        let kqueue = Kqueue::new()?;
        kqueue.watch_writes(&file)?;
        let started_at = Instant::now();

        loop {
            let states = expectations
                .iter()
                .map(|(offset, success, failure)| {
                    file_marker_state_at(path, *offset, success, failure)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if states
                .iter()
                .all(|state| *state != BlockMarkerState::Pending)
            {
                if states
                    .iter()
                    .all(|state| *state == BlockMarkerState::Success)
                {
                    return Ok(());
                }

                let offsets: Vec<_> = expectations.iter().map(|(offset, _, _)| *offset).collect();
                return Err(format!(
                    "observed terminal marker states {states:?} at offsets {offsets:?} in {}",
                    path.display()
                ));
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {timeout:?} waiting for terminal marker states in {}; latest states: {states:?}",
                    path.display()
                ));
            };

            kqueue.wait_for_write(remaining)?;
        }
    }

    fn wait_for_nonzero_balloon_actual_pages(
        socket_path: &Path,
        timeout: Duration,
    ) -> Result<String, String> {
        let started_at = Instant::now();

        loop {
            let response =
                concurrent_mmds_http_request(socket_path, "GET", "/balloon/statistics", None)
                    .map_err(|()| {
                        "balloon statistics endpoint became unavailable while waiting for inflation"
                            .to_string()
                    })?;
            let actual_pages = response
                .strip_prefix("HTTP/1.1 200 OK\r\n")
                .and_then(|_| response.split_once("\r\n\r\n"))
                .and_then(|(_, body)| serde_json::from_str::<serde_json::Value>(body).ok())
                .and_then(|body| body.get("actual_pages").cloned())
                .and_then(|actual_pages| actual_pages.as_u64());
            if actual_pages.is_some_and(|pages| pages > 0) {
                return Ok(response);
            }
            if started_at.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for /balloon/statistics actual_pages > 0; latest actual_pages={actual_pages:?}; latest response:\n{response}"
                ));
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_balloon_page_counts(
        socket_path: &Path,
        expected_target_pages: u64,
        expected_actual_pages: u64,
        timeout: Duration,
    ) -> Result<String, String> {
        let started_at = Instant::now();

        loop {
            let response = concurrent_mmds_http_request(
                socket_path,
                "GET",
                "/balloon/statistics",
                None,
            )
            .map_err(|()| {
                "balloon statistics endpoint became unavailable while waiting for page counts"
                    .to_string()
            })?;
            let body = balloon_response_json(&response);
            let target_pages = body
                .as_ref()
                .and_then(|body| body.get("target_pages"))
                .and_then(serde_json::Value::as_u64);
            let actual_pages = body
                .as_ref()
                .and_then(|body| body.get("actual_pages"))
                .and_then(serde_json::Value::as_u64);
            if target_pages == Some(expected_target_pages)
                && actual_pages == Some(expected_actual_pages)
            {
                return Ok(response);
            }
            if started_at.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for /balloon/statistics target_pages={expected_target_pages} and actual_pages={expected_actual_pages}; latest target_pages={target_pages:?}; latest actual_pages={actual_pages:?}; latest response:\n{response}"
                ));
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_balloon_optional_statistics(
        socket_path: &Path,
        timeout: Duration,
    ) -> Result<Vec<&'static str>, String> {
        let started_at = Instant::now();

        loop {
            let response = concurrent_mmds_http_request(
                socket_path,
                "GET",
                "/balloon/statistics",
                None,
            )
            .map_err(|()| {
                "balloon statistics endpoint became unavailable while waiting for optional fields"
                    .to_string()
            })?;
            let statistics = balloon_optional_statistics(&response)?;
            if !statistics.is_empty() {
                return Ok(statistics);
            }
            if started_at.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for guest-provided optional balloon statistics; latest response:\n{response}"
                ));
            }

            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn balloon_optional_statistics(response: &str) -> Result<Vec<&'static str>, String> {
        let body = balloon_response_json(response)
            .ok_or_else(|| format!("invalid balloon statistics response:\n{response}"))?;
        Ok([
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
        .filter(|field| body.get(*field).is_some())
        .collect())
    }

    fn wait_for_balloon_hinting_status(
        socket_path: &Path,
        expected_host_cmd: u64,
        expected_guest_cmd: Option<u64>,
        timeout: Duration,
    ) -> Result<String, String> {
        let started_at = Instant::now();

        loop {
            let response =
                concurrent_mmds_http_request(socket_path, "GET", "/balloon/hinting/status", None)
                    .map_err(|()| {
                    "balloon hinting endpoint became unavailable while waiting for guest status"
                        .to_string()
                })?;
            let body = balloon_response_json(&response);
            let host_cmd = body
                .as_ref()
                .and_then(|body| body.get("host_cmd"))
                .and_then(serde_json::Value::as_u64);
            let guest_cmd = body
                .as_ref()
                .and_then(|body| body.get("guest_cmd"))
                .and_then(serde_json::Value::as_u64);
            if host_cmd == Some(expected_host_cmd) && guest_cmd == expected_guest_cmd {
                return Ok(response);
            }
            if started_at.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for balloon hinting host_cmd={expected_host_cmd} and guest_cmd={expected_guest_cmd:?}; latest host_cmd={host_cmd:?}; latest guest_cmd={guest_cmd:?}; latest response:\n{response}"
                ));
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn balloon_response_json(response: &str) -> Option<serde_json::Value> {
        response
            .strip_prefix("HTTP/1.1 200 OK\r\n")
            .and_then(|_| response.split_once("\r\n\r\n"))
            .and_then(|(_, body)| serde_json::from_str(body).ok())
    }

    fn wait_for_nonzero_balloon_free_page_report_count(
        socket_path: &Path,
        metrics_path: &Path,
        timeout: Duration,
    ) -> Result<u64, String> {
        let started_at = Instant::now();

        loop {
            let response =
                http_put_json(socket_path, "/actions", r#"{"action_type":"FlushMetrics"}"#);
            if !response.starts_with("HTTP/1.1 204 No Content\r\n") {
                return Err(format!(
                    "FlushMetrics failed while waiting for reporting activity:\n{response}"
                ));
            }
            let count = latest_balloon_free_page_report_count(metrics_path);
            if let Some(count) = count.filter(|count| *count > 0) {
                return Ok(count);
            }
            if started_at.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for balloon.free_page_report_count > 0; latest count={count:?}"
                ));
            }

            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn latest_balloon_free_page_report_count(path: &Path) -> Option<u64> {
        let output = fs::read_to_string(path).ok()?;
        output.lines().rev().find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("balloon")?
                .get("free_page_report_count")?
                .as_u64()
        })
    }

    fn wait_for_http_response_fragment(
        socket_path: &Path,
        path: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let started_at = Instant::now();

        loop {
            let response = http_get(socket_path, path);
            if response.starts_with("HTTP/1.1 200 OK\r\n") && response.contains(expected) {
                return Ok(response);
            }
            if started_at.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {path} response to contain {expected:?}; latest response:\n{response}"
                ));
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_file_prefix_marker(
        path: &Path,
        marker: &[u8],
        timeout: Duration,
    ) -> Result<(), String> {
        let file = fs::File::open(path).map_err(|err| {
            format!(
                "failed to open block backing {} for marker wait: {err}",
                path.display()
            )
        })?;
        if file_starts_with_marker(path, marker)? {
            return Ok(());
        }

        let kqueue = Kqueue::new()?;
        kqueue.watch_writes(&file)?;
        let started_at = Instant::now();

        loop {
            if file_starts_with_marker(path, marker)? {
                return Ok(());
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {:?} waiting for marker {:?} in {}",
                    timeout,
                    String::from_utf8_lossy(marker),
                    path.display()
                ));
            };

            kqueue.wait_for_write(remaining)?;
        }
    }

    fn wait_for_file_contains_marker(
        path: &Path,
        marker: &[u8],
        timeout: Duration,
    ) -> Result<(), String> {
        let file = fs::File::open(path).map_err(|err| {
            format!(
                "failed to open serial output {} for marker wait: {err}",
                path.display()
            )
        })?;
        if file_contains_marker(path, marker)? {
            return Ok(());
        }

        let kqueue = Kqueue::new()?;
        kqueue.watch_writes(&file)?;
        let started_at = Instant::now();

        loop {
            if file_contains_marker(path, marker)? {
                return Ok(());
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {:?} waiting for marker {:?} in {}",
                    timeout,
                    String::from_utf8_lossy(marker),
                    path.display()
                ));
            };

            kqueue.wait_for_write(remaining)?;
        }
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
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(bytes) => {
                    return Err(format!(
                        "timed out waiting for virtual block marker; observed {bytes:?}"
                    ));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn fill_deterministic_vsock_stream_chunk(chunk: &mut [u8], stream_offset: usize, seed: u8) {
        for (index, byte) in chunk.iter_mut().enumerate() {
            let absolute_offset = stream_offset
                .checked_add(index)
                .expect("bounded vsock stream offset should fit usize");
            let mixed = absolute_offset
                .wrapping_mul(131)
                .wrapping_add(usize::from(seed))
                ^ (absolute_offset >> 8)
                ^ (absolute_offset >> 16);
            *byte = mixed.to_le_bytes()[0];
        }
    }

    fn write_deterministic_vsock_stream(
        stream: &mut UnixStream,
        seed: u8,
    ) -> Result<usize, String> {
        let mut chunk = [0; DIRECT_ROOTFS_VSOCK_STREAM_CHUNK_BYTES];
        let mut written = 0;

        while written < DIRECT_ROOTFS_VSOCK_STREAM_BYTES {
            let chunk_len = (DIRECT_ROOTFS_VSOCK_STREAM_BYTES - written).min(chunk.len());
            fill_deterministic_vsock_stream_chunk(&mut chunk[..chunk_len], written, seed);
            stream.write_all(&chunk[..chunk_len]).map_err(|err| {
                format!(
                    "deterministic vsock stream write failed after {written} of {} bytes: {:?}",
                    DIRECT_ROOTFS_VSOCK_STREAM_BYTES,
                    err.kind()
                )
            })?;
            written += chunk_len;
        }

        Ok(written)
    }

    fn read_and_verify_deterministic_vsock_stream(
        stream: &mut UnixStream,
        seed: u8,
    ) -> Result<usize, String> {
        let mut received_chunk = [0; DIRECT_ROOTFS_VSOCK_STREAM_CHUNK_BYTES];
        let mut expected_chunk = [0; DIRECT_ROOTFS_VSOCK_STREAM_CHUNK_BYTES];
        let mut received = 0;

        while received < DIRECT_ROOTFS_VSOCK_STREAM_BYTES {
            let chunk_len = (DIRECT_ROOTFS_VSOCK_STREAM_BYTES - received).min(received_chunk.len());
            stream
                .read_exact(&mut received_chunk[..chunk_len])
                .map_err(|err| {
                    format!(
                        "deterministic vsock stream read failed after {received} of {} bytes: {:?}",
                        DIRECT_ROOTFS_VSOCK_STREAM_BYTES,
                        err.kind()
                    )
                })?;
            fill_deterministic_vsock_stream_chunk(&mut expected_chunk[..chunk_len], received, seed);
            if let Some(index) = received_chunk[..chunk_len]
                .iter()
                .zip(&expected_chunk[..chunk_len])
                .position(|(received, expected)| received != expected)
            {
                return Err(format!(
                    "deterministic vsock stream content mismatch at byte {} of {}",
                    received + index,
                    DIRECT_ROOTFS_VSOCK_STREAM_BYTES
                ));
            }
            received += chunk_len;
        }

        Ok(received)
    }

    fn shutdown_unix_stream_write(stream: &UnixStream) -> Result<(), String> {
        stream.shutdown(Shutdown::Write).map_err(|err| {
            format!(
                "failed to write-half-close deterministic vsock stream: {:?}",
                err.kind()
            )
        })
    }

    fn wait_for_unix_listener_accept(
        listener: &UnixListener,
        timeout: Duration,
    ) -> Result<UnixStream, String> {
        listener.set_nonblocking(true).map_err(|err| {
            format!(
                "failed to set vsock listener nonblocking before accept wait: {:?}",
                err.kind()
            )
        })?;
        if let Some(stream) = try_accept_unix_listener(listener)? {
            return Ok(stream);
        }

        let kqueue = Kqueue::new()?;
        kqueue.watch_reads(listener)?;
        let started_at = Instant::now();

        loop {
            if let Some(stream) = try_accept_unix_listener(listener)? {
                return Ok(stream);
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {timeout:?} waiting for vsock listener accept"
                ));
            };

            kqueue.wait_for_read(remaining)?;
        }
    }

    fn try_accept_unix_listener(listener: &UnixListener) -> Result<Option<UnixStream>, String> {
        match listener.accept() {
            Ok((stream, _addr)) => Ok(Some(stream)),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => Ok(None),
            Err(err) => Err(format!("failed to accept vsock listener: {:?}", err.kind())),
        }
    }

    fn read_vsock_connect_ok(stream: &mut UnixStream) -> Result<u32, String> {
        const CONNECT_OK_MAX_LEN: usize = 32;

        let mut line = Vec::new();
        let mut byte = [0; 1];
        loop {
            if line.len() >= CONNECT_OK_MAX_LEN {
                return Err(format!(
                    "CONNECT OK response exceeded {CONNECT_OK_MAX_LEN} bytes"
                ));
            }

            stream
                .read_exact(&mut byte)
                .map_err(|err| format!("failed to read CONNECT OK response: {:?}", err.kind()))?;
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }

        let response =
            String::from_utf8(line).map_err(|_| "CONNECT OK response is not UTF-8".to_owned())?;
        let Some(port_text) = response
            .strip_prefix("OK ")
            .and_then(|suffix| suffix.strip_suffix('\n'))
        else {
            return Err("unexpected CONNECT OK response".to_owned());
        };
        port_text
            .parse::<u32>()
            .map_err(|_| "CONNECT OK response has invalid local port".to_owned())
    }

    fn read_unix_stream_eof(stream: &mut UnixStream) -> Result<(), String> {
        let mut byte = [0; 1];
        match stream.read(&mut byte) {
            Ok(0) => Ok(()),
            Ok(read) => Err(format!(
                "expected EOF from vsock stream, read {read} extra byte(s)"
            )),
            Err(err) => Err(format!(
                "failed to read EOF from vsock stream: {:?}",
                err.kind()
            )),
        }
    }

    fn file_marker_state_at(
        path: &Path,
        offset: u64,
        success: &[u8],
        failure: &[u8],
    ) -> Result<BlockMarkerState, String> {
        if file_matches_marker_at(path, offset, success)? {
            return Ok(BlockMarkerState::Success);
        }
        if file_matches_marker_at(path, offset, failure)? {
            return Ok(BlockMarkerState::Failure);
        }
        Ok(BlockMarkerState::Pending)
    }

    fn file_matches_marker_at(path: &Path, offset: u64, marker: &[u8]) -> Result<bool, String> {
        let mut file = fs::File::open(path)
            .map_err(|err| format!("failed to open block backing {}: {err}", path.display()))?;
        file.seek(SeekFrom::Start(offset)).map_err(|err| {
            format!(
                "failed to seek block backing {} to offset {offset}: {err}",
                path.display()
            )
        })?;
        let mut buffer = vec![0; marker.len()];
        match file.read_exact(&mut buffer) {
            Ok(()) => Ok(buffer == marker),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
            Err(err) => Err(format!(
                "failed to read block backing {} at offset {offset}: {err}",
                path.display()
            )),
        }
    }

    fn file_starts_with_marker(path: &Path, marker: &[u8]) -> Result<bool, String> {
        let mut file = fs::File::open(path)
            .map_err(|err| format!("failed to open block backing {}: {err}", path.display()))?;
        let mut buffer = vec![0; marker.len()];
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|err| format!("failed to read block backing {}: {err}", path.display()))?;

        Ok(bytes_read == marker.len() && buffer == marker)
    }

    fn direct_rootfs_vsock_failure_phase(path: &Path) -> &'static str {
        const FAILURE_MARKERS: &[(&[u8], &str)] = &[
            (b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_CONTENT", "content"),
            (b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_RECV", "receive"),
            (b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_SEND", "send"),
            (
                b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_SHUTDOWN_WRITE",
                "write-half close",
            ),
            (
                b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_EOF_READ",
                "terminal EOF read",
            ),
            (b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_EOF", "early EOF"),
            (
                b"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_TRAILING_DATA",
                "trailing data",
            ),
        ];
        let Ok(mut file) = fs::File::open(path) else {
            return "unavailable";
        };
        let mut prefix = [0; 128];
        let Ok(bytes_read) = file.read(&mut prefix) else {
            return "unavailable";
        };

        FAILURE_MARKERS
            .iter()
            .find_map(|(marker, phase)| prefix[..bytes_read].starts_with(marker).then_some(*phase))
            .unwrap_or("not published")
    }

    fn file_contains_marker(path: &Path, marker: &[u8]) -> Result<bool, String> {
        if marker.is_empty() {
            return Ok(true);
        }

        let bytes = fs::read(path)
            .map_err(|err| format!("failed to read serial output {}: {err}", path.display()))?;

        Ok(bytes.windows(marker.len()).any(|window| window == marker))
    }

    fn file_bytes_at(path: &Path, offset: u64, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        let mut file = fs::File::open(path)
            .unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
        file.seek(SeekFrom::Start(offset)).unwrap_or_else(|err| {
            panic!(
                "failed to seek {} to offset {offset}: {err}",
                path.display()
            )
        });
        file.read_exact(&mut bytes).unwrap_or_else(|err| {
            panic!(
                "failed to read {} at offset {offset}: {err}",
                path.display()
            )
        });
        bytes
    }

    fn expected_block_device_id(path: &Path) -> String {
        let metadata = fs::metadata(path).expect("block backing metadata should be readable");
        format!("{}{}{}", metadata.dev(), metadata.rdev(), metadata.ino())
            .chars()
            .take(20)
            .collect()
    }

    fn assert_block_serial_report(path: &Path, expected: &str) {
        let output = fs::read(path).expect("block serial output should be readable");
        let normalized = String::from_utf8_lossy(&output).replace('\r', "");
        let expected_report = format!(
            "{}\n{expected}\n{}",
            String::from_utf8_lossy(BLOCK_SERIAL_BEGIN_MARKER),
            String::from_utf8_lossy(BLOCK_SERIAL_END_MARKER),
        );
        assert!(
            normalized.contains(&expected_report),
            "guest block serial must equal the exact host backing metadata identity"
        );
    }

    fn assert_phase_block_serial_report(
        path: &Path,
        begin: &[u8],
        end: &[u8],
        expected: &str,
        context: &str,
    ) {
        let output = fs::read(path).expect("block serial output should be readable");
        let normalized = String::from_utf8_lossy(&output).replace('\r', "");
        let expected_report = format!(
            "{}\n{expected}\n{}",
            String::from_utf8_lossy(begin),
            String::from_utf8_lossy(end),
        );
        assert!(
            normalized.contains(&expected_report),
            "{context} guest block serial must equal the exact current backing identity"
        );
    }

    fn file_prefix_lossy(path: &Path, len: usize) -> String {
        match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes[..bytes.len().min(len)]).into_owned(),
            Err(err) => format!("failed to read {}: {err}", path.display()),
        }
    }

    fn file_tail_lossy(path: &Path, len: usize) -> String {
        match fs::read(path) {
            Ok(bytes) => {
                String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(len)..]).into_owned()
            }
            Err(err) => format!("failed to read {}: {err}", path.display()),
        }
    }

    fn text_tail_lossy(text: &str, len: usize) -> String {
        String::from_utf8_lossy(&text.as_bytes()[text.len().saturating_sub(len)..]).into_owned()
    }

    #[derive(Debug)]
    struct Kqueue {
        fd: libc::c_int,
    }

    impl Kqueue {
        fn new() -> Result<Self, String> {
            // SAFETY: `kqueue` has no preconditions and returns either a new file
            // descriptor or -1 with errno set.
            let fd = unsafe { libc::kqueue() };
            if fd >= 0 {
                Ok(Self { fd })
            } else {
                Err(format!(
                    "failed to create kqueue: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }

        fn watch_writes(&self, file: &fs::File) -> Result<(), String> {
            use std::os::fd::AsRawFd;

            self.register_event(
                file.as_raw_fd(),
                libc::EVFILT_VNODE,
                libc::NOTE_WRITE | libc::NOTE_EXTEND,
                "file write",
            )
        }

        fn watch_reads(&self, listener: &UnixListener) -> Result<(), String> {
            use std::os::fd::AsRawFd;

            self.register_event(listener.as_raw_fd(), libc::EVFILT_READ, 0, "listener read")
        }

        fn register_event(
            &self,
            raw_fd: libc::c_int,
            filter: i16,
            fflags: u32,
            context: &str,
        ) -> Result<(), String> {
            let ident = libc::uintptr_t::try_from(raw_fd)
                .map_err(|_| format!("watched {context} descriptor did not fit uintptr_t"))?;
            let change = libc::kevent {
                ident,
                filter,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags,
                data: 0,
                udata: std::ptr::null_mut(),
            };

            // SAFETY: `self.fd` is an open kqueue descriptor, `change` points to
            // one initialized event, and no output events are requested.
            let result = unsafe {
                libc::kevent(
                    self.fd,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(format!(
                    "failed to register {context} kqueue watch: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }

        fn wait_for_write(&self, timeout: Duration) -> Result<(), String> {
            self.wait_for_event(timeout, "file write")
        }

        fn wait_for_read(&self, timeout: Duration) -> Result<(), String> {
            self.wait_for_event(timeout, "listener read")
        }

        fn wait_for_event(&self, timeout: Duration, context: &str) -> Result<(), String> {
            let timeout = duration_to_timespec(timeout)?;
            let mut event = libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };

            loop {
                // SAFETY: `self.fd` is an open kqueue descriptor, `event`
                // points to writable storage for one event, and `timeout`
                // lives for the call.
                let result =
                    unsafe { libc::kevent(self.fd, std::ptr::null(), 0, &mut event, 1, &timeout) };

                if result > 0 {
                    return Ok(());
                }
                if result == 0 {
                    return Err(format!("timed out waiting for {context} event"));
                }

                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(format!("failed while waiting for {context}: {err}"));
                }
            }
        }
    }

    impl Drop for Kqueue {
        fn drop(&mut self) {
            // SAFETY: `self.fd` was returned by `kqueue` and is owned by this
            // guard.
            let _ = unsafe { libc::close(self.fd) };
        }
    }

    fn duration_to_timespec(duration: Duration) -> Result<libc::timespec, String> {
        let tv_sec = libc::time_t::try_from(duration.as_secs())
            .map_err(|_| format!("duration {duration:?} does not fit time_t"))?;
        let tv_nsec = libc::c_long::from(duration.subsec_nanos());

        Ok(libc::timespec { tv_sec, tv_nsec })
    }
}
