// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod support;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_arm64 {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::support::{
        BangbangProcess, TestDir, assert_bad_request_response, assert_clean_shutdown,
        assert_no_content_response, assert_ok_response, assert_response_contains, http_get,
        http_json, http_json_with_io_timeout, http_no_body, http_put_json, json_string, path_text,
    };

    const BANGBANG_GUEST_KERNEL_PATH_ENV: &str = "BANGBANG_GUEST_KERNEL_PATH";
    const BANGBANG_GUEST_INITRD_PATH_ENV: &str = "BANGBANG_GUEST_INITRD_PATH";
    const BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
    const BLOCK_WRITE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITE_OK";
    const DIRECT_ROOTFS_BLOCK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BLOCK_OK";
    const DIRECT_ROOTFS_BOOT_OK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BOOT_OK";
    const BOOT_TIMER_LOG_MARKER: &[u8] = b"Guest-boot-time =";
    const DIRECT_ROOTFS_BALLOON_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.balloon-check=1";
    const DIRECT_ROOTFS_BALLOON_MARKER: &[u8] = b"BANGBANG_BALLOON_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 memhp_default_state=online_movable init=/bangbang-direct-rootfs-init bangbang.memory-hotplug-check=1";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_READY_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_READY";
    const DIRECT_ROOTFS_MEMORY_HOTPLUG_MARKER: &[u8] = b"BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_RTC_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.rtc-check=1";
    const DIRECT_ROOTFS_RTC_MARKER: &[u8] = b"BANGBANG_RTC_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_VMCLOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vmclock-check=1";
    const DIRECT_ROOTFS_VMCLOCK_MARKER: &[u8] = b"BANGBANG_VMCLOCK_GUEST_CHECK_OK";
    const DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.block-writeback-flush=1";
    const DIRECT_ROOTFS_WRITEBACK_FLUSH_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITEBACK_FLUSH_OK";
    const DIRECT_ROOTFS_ENTROPY_MARKER: &[u8] = b"BANGBANG_ENTROPY_GUEST_READ_OK";
    const DIRECT_ROOTFS_PMEM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.pmem-read-flush=1";
    const DIRECT_ROOTFS_PMEM_READ_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_READ_FLUSH_OK";
    const PMEM_HOST_MARKER: &[u8] = b"BANGBANG_PMEM_HOST_MARKER";
    const PMEM_GUEST_FLUSH_MARKER: &[u8] = b"BANGBANG_PMEM_GUEST_FLUSH_OK";
    const PMEM_GUEST_FLUSH_OFFSET: u64 = 4096;
    const DIRECT_ROOTFS_MMDS_MARKER: &[u8] = b"BANGBANG_MMDS_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_MMDS_V2_MARKER: &[u8] = b"BANGBANG_MMDS_V2_GUEST_FETCH_OK";
    const DIRECT_ROOTFS_VSOCK_MARKER: &[u8] = b"BANGBANG_VSOCK_GUEST_CONNECT_OK";
    const DIRECT_ROOTFS_VSOCK_EXCHANGES: &[(&[u8], &[u8])] = &[
        (
            b"BANGBANG_VSOCK_GUEST_STREAM_ONE",
            b"BANGBANG_VSOCK_HOST_STREAM_ONE",
        ),
        (
            b"BANGBANG_VSOCK_GUEST_STREAM_TWO",
            b"BANGBANG_VSOCK_HOST_STREAM_TWO",
        ),
    ];
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
    const DIRECT_ROOTFS_HOST_VSOCK_EXCHANGES: &[(&[u8], &[u8])] = &[
        (
            b"BANGBANG_VSOCK_GUEST_STREAM_ONE",
            b"BANGBANG_VSOCK_HOST_STREAM_ONE",
        ),
        (
            b"BANGBANG_VSOCK_GUEST_STREAM_TWO",
            b"BANGBANG_VSOCK_HOST_STREAM_TWO",
        ),
    ];
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
    const GUEST_POWEROFF_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init";
    const GUEST_RESET_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/reboot-init";
    const DIRECT_ROOTFS_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init";
    const ROOTFS_BOOT_TIMER_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 nomodule swiotlb=noforce init=/usr/local/bin/init";
    const DIRECT_ROOTFS_ENTROPY_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.entropy-read=1";
    const DIRECT_ROOTFS_MMDS_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-fetch=1";
    const DIRECT_ROOTFS_MMDS_V2_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-v2-fetch=1";
    const DIRECT_ROOTFS_MMDS_CONTENT: &str =
        r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_GUEST_VALUE"}}"#;
    const DIRECT_ROOTFS_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-connect=1";
    const DIRECT_ROOTFS_VSOCK_MULTISTREAM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-multistream=1";
    const DIRECT_ROOTFS_HOST_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-connect=1";
    const DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-multistream=1";
    const GUEST_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);

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
        content_source: DirectRootfsMmdsContentSource,
    }

    #[derive(Clone, Copy)]
    struct DirectRootfsNoApiMmdsFetchCase<'a> {
        request_context: &'a str,
        mmds_config_body: &'a str,
        boot_args: &'a str,
        success_marker: &'a [u8],
    }

    #[test]
    fn signed_executable_starts_instance_and_guest_writes_block_marker() {
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
                "is_read_only":false
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
                "is_read_only":false
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
        assert_bad_request_response(&duplicate_pause_response, "PATCH /vm Paused while paused");
        assert_response_contains(
            &duplicate_pause_response,
            r#"{"fault_message":"The requested operation is not supported in Paused state: Pause"}"#,
            "PATCH /vm Paused while paused",
        );
        let paused_after_duplicate_pause = http_get(&socket_path, "/");
        assert_ok_response(
            &paused_after_duplicate_pause,
            "GET / after rejected duplicate PATCH /vm Paused",
        );
        assert_response_contains(
            &paused_after_duplicate_pause,
            r#""state":"Paused""#,
            "GET / after rejected duplicate PATCH /vm Paused",
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
        assert_bad_request_response(
            &duplicate_resume_response,
            "PATCH /vm Resumed while running",
        );
        assert_response_contains(
            &duplicate_resume_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: Resume"}"#,
            "PATCH /vm Resumed while running",
        );
        let running_after_duplicate_resume = http_get(&socket_path, "/");
        assert_ok_response(
            &running_after_duplicate_resume,
            "GET / after rejected duplicate PATCH /vm Resumed",
        );
        assert_response_contains(
            &running_after_duplicate_resume,
            r#""state":"Running""#,
            "GET / after rejected duplicate PATCH /vm Resumed",
        );

        for (request_context, path, expected_fault, private_id) in [
            (
                "DELETE /drives/private_hot_unplug_drive after InstanceStart",
                "/drives/private_hot_unplug_drive",
                r#"{"fault_message":"Drive updates are not supported."}"#,
                "private_hot_unplug_drive",
            ),
            (
                "DELETE /network-interfaces/private_hot_unplug_iface after InstanceStart",
                "/network-interfaces/private_hot_unplug_iface",
                r#"{"fault_message":"Network interface updates are not supported."}"#,
                "private_hot_unplug_iface",
            ),
            (
                "DELETE /pmem/private_hot_unplug_pmem after InstanceStart",
                "/pmem/private_hot_unplug_pmem",
                r#"{"fault_message":"Pmem device is not supported."}"#,
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
            r#"{"fault_message":"The requested operation is not supported in Running state: PutDrive"}"#,
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
            r#"{"fault_message":"The requested operation is not supported in Running state: PutNetworkInterface"}"#,
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

        assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
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
    fn signed_executable_starts_from_config_file() {
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
                    "is_read_only": false
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

        assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang config file");
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
    fn signed_executable_boots_direct_rootfs_when_data_drive_configured_before_root() {
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
                "is_read_only":false
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
                "is_read_only":true
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

        let balloon_response = http_put_json(
            &socket_path,
            "/balloon",
            r#"{"amount_mib":8,"deflate_on_oom":false,"free_page_hinting":true}"#,
        );
        assert_no_content_response(&balloon_response, "PUT /balloon direct rootfs");

        let configured_balloon = http_get(&socket_path, "/balloon");
        assert_ok_response(&configured_balloon, "GET /balloon direct rootfs");
        for expected in [
            r#""amount_mib":8"#,
            r#""deflate_on_oom":false"#,
            r#""free_page_hinting":true"#,
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
            r#""free_page_hinting":true"#,
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

        let balloon_stats = http_get(&socket_path, "/balloon/statistics");
        assert_ok_response(&balloon_stats, "GET /balloon/statistics direct rootfs");
        for expected in [
            r#""target_pages":2048"#,
            r#""target_mib":8"#,
            r#""actual_pages":"#,
            r#""actual_mib":"#,
        ] {
            assert_response_contains(
                &balloon_stats,
                expected,
                "GET /balloon/statistics direct rootfs",
            );
        }

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
            r#"{"acknowledge_on_stop":false}"#,
        );
        assert_no_content_response(&hinting_start, "PATCH /balloon/hinting/start direct rootfs");
        let started_hinting_status = http_get(&socket_path, "/balloon/hinting/status");
        assert_ok_response(
            &started_hinting_status,
            "GET /balloon/hinting/status after start direct rootfs",
        );
        for expected in [r#""host_cmd":2"#, r#""guest_cmd":null"#] {
            assert_response_contains(
                &started_hinting_status,
                expected,
                "GET /balloon/hinting/status after start direct rootfs",
            );
        }

        let hinting_stop = http_no_body(&socket_path, "PATCH", "/balloon/hinting/stop");
        assert_no_content_response(&hinting_stop, "PATCH /balloon/hinting/stop direct rootfs");
        let stopped_hinting_status = http_get(&socket_path, "/balloon/hinting/status");
        assert_ok_response(
            &stopped_hinting_status,
            "GET /balloon/hinting/status after stop direct rootfs",
        );
        for expected in [r#""host_cmd":1"#, r#""guest_cmd":null"#] {
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

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang balloon direct rootfs",
        );
    }

    #[test]
    fn signed_executable_hotplugs_memory_from_direct_rootfs_guest() {
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
            "PUT /machine-config memory hotplug direct rootfs",
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
            DIRECT_ROOTFS_MEMORY_HOTPLUG_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not observe runtime virtio-mem requested-size update through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

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
            "PUT /machine-config writeback block direct rootfs",
        );

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_WRITEBACK_FLUSH_BOOT_ARGS);
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

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang writeback block direct rootfs",
        );
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
                "path_on_host":{pmem_backing_path_json}
            }}"#
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
            r#"{"id":"pmem0","rate_limiter":{"ops":{"size":123456,"one_time_burst":234567,"refill_time":345678}}}"#,
        );
        assert_bad_request_response(
            &pmem_rate_limiter_patch_response,
            "PATCH /pmem/pmem0 configured rate limiter after InstanceStart",
        );
        assert_response_contains(
            &pmem_rate_limiter_patch_response,
            r#"{"fault_message":"pmem rate_limiter is not supported"}"#,
            "PATCH /pmem/pmem0 configured rate limiter after InstanceStart",
        );
        for private_value in ["123456", "234567", "345678"] {
            assert!(
                !pmem_rate_limiter_patch_response.contains(private_value),
                "PATCH /pmem/pmem0 configured rate limiter must not echo {private_value}: {pmem_rate_limiter_patch_response}"
            );
        }

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

        assert_clean_shutdown(
            bangbang.terminate(),
            &socket_path,
            "bangbang pmem direct rootfs",
        );
    }

    #[test]
    fn signed_executable_serves_mmds_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "MMDS guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            boot_args: DIRECT_ROOTFS_MMDS_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_MARKER,
            content_source: DirectRootfsMmdsContentSource::ApiRequest,
        });
    }

    #[test]
    fn signed_executable_serves_metadata_file_mmds_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "metadata-file MMDS guest fetch",
            mmds_config_body: r#"{"network_interfaces":["eth0"],"version":"V1","ipv4_address":"169.254.169.254"}"#,
            boot_args: DIRECT_ROOTFS_MMDS_BOOT_ARGS,
            success_marker: DIRECT_ROOTFS_MMDS_MARKER,
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
                "host vsock port listener {} should bind before guest startup: {err}",
                host_port_path.display()
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
            &host_port_path,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            Ok(stream) => stream,
            Err(err) => {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "guest did not initiate vsock connection to host listener {}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    host_port_path.display(),
                    output.status,
                    output.stdout,
                    output.stderr
                );
            }
        };
        drop(host_listener);
        fs::remove_file(&host_port_path).unwrap_or_else(|err| {
            panic!(
                "host vsock port listener path {} should be removed after accept: {err}",
                host_port_path.display()
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

        for (exchange_index, &(guest_payload, host_payload)) in
            DIRECT_ROOTFS_VSOCK_EXCHANGES.iter().enumerate()
        {
            let exchange_number = exchange_index + 1;
            let mut received_guest_payload = vec![0; guest_payload.len()];
            if let Err(err) = host_stream.read_exact(&mut received_guest_payload) {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not receive guest vsock payload {exchange_number}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
            assert_eq!(
                received_guest_payload, guest_payload,
                "host side should receive deterministic guest vsock payload {exchange_number}"
            );

            if let Err(err) = host_stream.write_all(host_payload) {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write guest vsock reply {exchange_number}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_VSOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete guest-initiated vsock through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = read_unix_stream_eof(&mut host_stream, &host_port_path) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not observe guest vsock EOF after guest close: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
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
                    "host vsock multistream port listener {} should bind before guest startup: {err}",
                    host_port_path.display()
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
                &host_port_path,
                GUEST_EXECUTION_TIMEOUT,
            ) {
                Ok(stream) => stream,
                Err(err) => {
                    let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                    let output = bangbang.force_stop_and_collect();
                    panic!(
                        "guest did not initiate multistream vsock connection for port {port} to host listener {}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                        host_port_path.display(),
                        output.status,
                        output.stdout,
                        output.stderr
                    );
                }
            };
            drop(host_listener);
            fs::remove_file(&host_port_path).unwrap_or_else(|err| {
                panic!(
                    "host vsock multistream port listener path {} should be removed after accept: {err}",
                    host_port_path.display()
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
            host_streams.push((port, host_port_path, host_stream));
        }

        for (
            stream_index,
            ((port, _host_port_path, host_stream), &(expected_port, guest_payload, _)),
        ) in host_streams
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
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not receive guest multistream payload {stream_number} for port {expected_port}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
            assert_eq!(
                received_guest_payload, guest_payload,
                "host side should receive isolated guest multistream payload {stream_number}"
            );
        }

        for (
            stream_index,
            ((port, _host_port_path, host_stream), &(expected_port, _, host_payload)),
        ) in host_streams
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
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write guest multistream reply {stream_number} for port {expected_port}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_VSOCK_MULTISTREAM_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete guest-initiated vsock multistream through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        for (port, host_port_path, host_stream) in &mut host_streams {
            if let Err(err) = read_unix_stream_eof(host_stream, host_port_path) {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not observe guest multistream EOF for port {port} after guest close: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
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

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_READY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not publish host-initiated vsock ready marker: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let mut host_stream = match UnixStream::connect(&uds_path) {
            Ok(stream) => stream,
            Err(err) => {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not connect to main vsock listener {}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    uds_path.display(),
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
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not write vsock CONNECT request: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        if let Err(err) = read_vsock_connect_ok(&mut host_stream) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not receive vsock CONNECT OK response: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }
        for (exchange_index, &(guest_payload, host_payload)) in
            DIRECT_ROOTFS_HOST_VSOCK_EXCHANGES.iter().enumerate()
        {
            let exchange_number = exchange_index + 1;
            let mut received_guest_payload = vec![0; guest_payload.len()];
            if let Err(err) = host_stream.read_exact(&mut received_guest_payload) {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not receive guest payload {exchange_number} over host-initiated vsock: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
            assert_eq!(
                received_guest_payload, guest_payload,
                "host side should receive deterministic guest payload {exchange_number}"
            );

            if let Err(err) = host_stream.write_all(host_payload) {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write host-initiated vsock reply {exchange_number}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete host-initiated vsock through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = read_unix_stream_eof(&mut host_stream, &uds_path) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "host side did not observe host-initiated vsock EOF after guest close: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
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

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_READY_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not publish host multistream vsock ready marker: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        let mut host_streams = Vec::new();
        for &(port, _, _) in DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_EXCHANGES {
            let mut host_stream = match UnixStream::connect(&uds_path) {
                Ok(stream) => stream,
                Err(err) => {
                    let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                    let output = bangbang.force_stop_and_collect();
                    panic!(
                        "host side did not connect multistream port {port} to main vsock listener {}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                        uds_path.display(),
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
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write multistream vsock CONNECT request for port {port}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
            host_streams.push((port, host_stream));
        }

        let mut acknowledged_local_ports = Vec::new();
        for (port, host_stream) in &mut host_streams {
            let local_port = match read_vsock_connect_ok(host_stream) {
                Ok(local_port) => local_port,
                Err(err) => {
                    let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                    let output = bangbang.force_stop_and_collect();
                    panic!(
                        "host side did not receive multistream vsock CONNECT OK response for port {port}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
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
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not receive guest multistream payload {stream_number} for host port {expected_port}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
            assert_eq!(
                received_guest_payload, guest_payload,
                "host side should receive isolated host multistream payload {stream_number}"
            );
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
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not write multistream vsock reply {stream_number} for host port {expected_port}: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status, output.stdout, output.stderr
                );
            }
        }

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not complete host-initiated vsock multistream through signed bangbang executable: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        for (port, host_stream) in &mut host_streams {
            if let Err(err) = read_unix_stream_eof(host_stream, &uds_path) {
                let backing_prefix = file_prefix_lossy(&data_backing_path, 128);
                let output = bangbang.force_stop_and_collect();
                panic!(
                    "host side did not observe host multistream EOF for port {port} after guest close: {err}; backing prefix: {backing_prefix:?}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
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

        let network_response = http_put_json(
            &socket_path,
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"06:00:00:00:00:01"}"#,
        );
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
            "PATCH /network-interfaces/eth0 rx_rate_limiter {}",
            case.request_context
        );
        let rate_limiter_network_patch_response = http_json(
            &socket_path,
            "PATCH",
            "/network-interfaces/eth0",
            r#"{"iface_id":"eth0","rx_rate_limiter":{"bandwidth":{"size":223456,"one_time_burst":334567,"refill_time":445678}}}"#,
        );
        assert_bad_request_response(
            &rate_limiter_network_patch_response,
            &rate_limiter_network_patch_context,
        );
        assert_response_contains(
            &rate_limiter_network_patch_response,
            r#"{"fault_message":"network rx_rate_limiter is not supported"}"#,
            &rate_limiter_network_patch_context,
        );
        for private_value in ["223456", "334567", "445678"] {
            assert!(
                !rate_limiter_network_patch_response.contains(private_value),
                "{rate_limiter_network_patch_context} must not echo {private_value}; response:\n{rate_limiter_network_patch_response}"
            );
        }

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
        assert!(
            !vm_config.contains(r#""iface_id":"eth9""#),
            "{vm_config_context} must not add the rejected interface; response:\n{vm_config}"
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

        let shutdown_context = format!("bangbang direct rootfs {}", case.request_context);
        assert_clean_shutdown(bangbang.terminate(), &socket_path, &shutdown_context);
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
        boot_args: &str,
    ) {
        let kernel_path_json = json_string(path_text(kernel_path));
        let initrd_path_json = json_string(path_text(initrd_path));
        let boot_args_json = json_string(boot_args);
        let config = format!(
            r#"{{
                "machine-config": {{"vcpu_count": 1, "mem_size_mib": 256}},
                "boot-source": {{
                    "kernel_image_path": {kernel_path_json},
                    "initrd_path": {initrd_path_json},
                    "boot_args": {boot_args_json}
                }}
            }}"#
        );
        fs::write(config_path, config).expect("guest stop config file should be written");
    }

    fn create_zeroed_block_backing(path: &Path) {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("guest block backing should create");
        file.set_len(bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("guest block backing should be one sector");
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

        assert!(
            output.contains(r#""metrics_flush_count":1"#),
            "metrics output should include first flush count; output:\n{output}"
        );
        if let Some(expected_get_api_requests) = expected_get_api_requests {
            let expected_get_metrics = format!(r#""get_api_requests":{expected_get_api_requests}"#);
            assert!(
                output.contains(&expected_get_metrics),
                "metrics output should include expected GET API request counters; output:\n{output}"
            );
        }
        let expected_put_metrics = format!(r#""put_api_requests":{expected_put_api_requests}"#);
        assert!(
            output.contains(&expected_put_metrics),
            "metrics output should include expected PUT API request counters; output:\n{output}"
        );
        if let Some(expected_patch_api_requests) = expected_patch_api_requests {
            let expected_patch_metrics =
                format!(r#""patch_api_requests":{expected_patch_api_requests}"#);
            assert!(
                output.contains(&expected_patch_metrics),
                "metrics output should include expected PATCH API request counters; output:\n{output}"
            );
        } else {
            assert!(
                !output.contains(r#""patch_api_requests":"#),
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

    fn wait_for_unix_listener_accept(
        listener: &UnixListener,
        path: &Path,
        timeout: Duration,
    ) -> Result<UnixStream, String> {
        listener.set_nonblocking(true).map_err(|err| {
            format!(
                "failed to set listener {} nonblocking before accept wait: {err}",
                path.display()
            )
        })?;
        if let Some(stream) = try_accept_unix_listener(listener, path)? {
            return Ok(stream);
        }

        let kqueue = Kqueue::new()?;
        kqueue.watch_reads(listener)?;
        let started_at = Instant::now();

        loop {
            if let Some(stream) = try_accept_unix_listener(listener, path)? {
                return Ok(stream);
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {:?} waiting for Unix listener {} to accept",
                    timeout,
                    path.display()
                ));
            };

            kqueue.wait_for_read(remaining)?;
        }
    }

    fn try_accept_unix_listener(
        listener: &UnixListener,
        path: &Path,
    ) -> Result<Option<UnixStream>, String> {
        match listener.accept() {
            Ok((stream, _addr)) => Ok(Some(stream)),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => Ok(None),
            Err(err) => Err(format!(
                "failed to accept Unix listener {}: {err}",
                path.display()
            )),
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
                .map_err(|err| format!("failed to read CONNECT OK response: {err}"))?;
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }

        let response = String::from_utf8(line)
            .map_err(|err| format!("CONNECT OK response is not UTF-8: {err}"))?;
        let Some(port_text) = response
            .strip_prefix("OK ")
            .and_then(|suffix| suffix.strip_suffix('\n'))
        else {
            return Err(format!("unexpected CONNECT OK response {response:?}"));
        };
        port_text
            .parse::<u32>()
            .map_err(|err| format!("CONNECT OK response has invalid local port: {err}"))
    }

    fn read_unix_stream_eof(stream: &mut UnixStream, path: &Path) -> Result<(), String> {
        let mut byte = [0; 1];
        match stream.read(&mut byte) {
            Ok(0) => Ok(()),
            Ok(read) => Err(format!(
                "expected EOF from Unix stream {}, read {read} byte(s)",
                path.display()
            )),
            Err(err) => Err(format!(
                "failed to read EOF from Unix stream {}: {err}",
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

    fn file_prefix_lossy(path: &Path, len: usize) -> String {
        match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes[..bytes.len().min(len)]).into_owned(),
            Err(err) => format!("failed to read {}: {err}", path.display()),
        }
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
