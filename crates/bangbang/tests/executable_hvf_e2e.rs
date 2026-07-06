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
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::support::{
        BangbangProcess, TestDir, assert_bad_request_response, assert_clean_shutdown,
        assert_no_content_response, assert_ok_response, assert_response_contains, http_get,
        http_json, http_no_body, http_put_json, json_string, path_text,
    };

    const BANGBANG_GUEST_KERNEL_PATH_ENV: &str = "BANGBANG_GUEST_KERNEL_PATH";
    const BANGBANG_GUEST_INITRD_PATH_ENV: &str = "BANGBANG_GUEST_INITRD_PATH";
    const BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
    const BLOCK_WRITE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITE_OK";
    const DIRECT_ROOTFS_BLOCK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BLOCK_OK";
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
    const DIRECT_ROOTFS_MMDS_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-fetch=1";
    const DIRECT_ROOTFS_MMDS_V2_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.mmds-v2-fetch=1";
    const DIRECT_ROOTFS_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-connect=1";
    const DIRECT_ROOTFS_VSOCK_MULTISTREAM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-guest-multistream=1";
    const DIRECT_ROOTFS_HOST_VSOCK_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-connect=1";
    const DIRECT_ROOTFS_HOST_VSOCK_MULTISTREAM_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.vsock-host-multistream=1";
    const GUEST_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Clone, Copy)]
    struct DirectRootfsMmdsFetchCase<'a> {
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
        let serial_output_path = test_dir.path().join("serial.out");
        let metrics_path = test_dir.path().join("metrics.out");
        let logger_path = test_dir.path().join("logger.out");
        let uds_path = test_dir.path().join("v.sock");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&backing_path);
        create_empty_file(&serial_output_path);

        let mut bangbang = BangbangProcess::start_with_extra_args(
            &socket_path,
            &instance_id,
            &[
                "--start-time-us",
                "1000",
                "--start-time-cpu-us",
                "2000",
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

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(&serial_response, "PUT /serial");

        let metrics_path_json = json_string(path_text(&metrics_path));
        let metrics_body = format!(r#"{{"metrics_path":{metrics_path_json}}}"#);
        let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
        assert_no_content_response(&metrics_response, "PUT /metrics");

        let logger_path_json = json_string(path_text(&logger_path));
        let logger_body = format!(r#"{{"log_path":{logger_path_json}}}"#);
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

        for (requested_state, action_name) in [("Paused", "Pause"), ("Resumed", "Resume")] {
            let context = format!("PATCH /vm {requested_state} after InstanceStart");
            let body = format!(r#"{{"state":"{requested_state}"}}"#);
            let response = http_json(&socket_path, "PATCH", "/vm", &body);
            assert_bad_request_response(&response, &context);
            assert_response_contains(
                &response,
                &format!(
                    r#"{{"fault_message":"The requested operation is not supported: {action_name}"}}"#
                ),
                &context,
            );

            let instance_info_after_patch = http_get(&socket_path, "/");
            assert_ok_response(&instance_info_after_patch, "GET / after rejected PATCH /vm");
            assert_response_contains(
                &instance_info_after_patch,
                r#""state":"Running""#,
                "GET / after rejected PATCH /vm",
            );
        }

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

        let replacement_backing_path = test_dir.path().join("replacement-data.img");
        let replacement_backing_path_json = json_string(path_text(&replacement_backing_path));
        let drive_update_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{replacement_backing_path_json}
            }}"#
        );
        let drive_update_response =
            http_json(&socket_path, "PATCH", "/drives/data", &drive_update_body);
        assert_bad_request_response(
            &drive_update_response,
            "PATCH /drives/data after InstanceStart",
        );
        assert_response_contains(
            &drive_update_response,
            r#"{"fault_message":"The requested operation is not supported: UpdateBlockDevice"}"#,
            "PATCH /drives/data after InstanceStart",
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
        assert_eq!(
            vm_config.matches(r#""drive_id":"#).count(),
            1,
            "rejected drive update must not add another drive; response:\n{vm_config}"
        );
        assert!(
            !vm_config.contains(&format!(
                r#""path_on_host":{replacement_backing_path_json}"#
            )),
            "rejected drive update must not mutate the configured drive path; response:\n{vm_config}"
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

        let flush_metrics_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(&flush_metrics_response, "PUT /actions FlushMetrics");
        assert_metrics_output(
            &metrics_path,
            Some(
                r#"{"balloon_count":3,"hotplug_memory_count":0,"instance_info_count":5,"machine_cfg_count":0,"mmds_count":1,"vmm_version_count":0}"#,
            ),
            r#"{"actions_count":2,"actions_fails":0,"balloon_count":1,"balloon_fails":1,"boot_source_count":2,"boot_source_fails":1,"cpu_cfg_count":1,"cpu_cfg_fails":1,"drive_count":2,"drive_fails":1,"hotplug_memory_count":0,"hotplug_memory_fails":0,"logger_count":2,"logger_fails":1,"machine_cfg_count":1,"machine_cfg_fails":0,"metrics_count":2,"metrics_fails":1,"mmds_count":2,"mmds_fails":1,"network_count":1,"network_fails":1,"pmem_count":0,"pmem_fails":0,"serial_count":2,"serial_fails":1,"vsock_count":2,"vsock_fails":1}"#,
            Some(
                r#"{"balloon_count":4,"balloon_fails":4,"drive_count":1,"drive_fails":1,"hotplug_memory_count":0,"hotplug_memory_fails":0,"machine_cfg_count":0,"machine_cfg_fails":0,"mmds_count":1,"mmds_fails":0,"network_count":0,"network_fails":0,"pmem_count":0,"pmem_fails":0}"#,
            ),
        );
        assert_startup_time_metrics_output(&metrics_path);
        assert!(
            !replacement_metrics_path.exists(),
            "rejected metrics update must not write later metrics output to replacement path {}",
            replacement_metrics_path.display()
        );
        assert_logger_output(&logger_path);
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
        assert_logger_output(&logger_path);

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
    fn signed_executable_boots_direct_rootfs_and_writes_block_marker() {
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
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs direct rootfs");

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
        assert_no_content_response(&data_drive_response, "PUT /drives/data direct rootfs");

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
                "direct rootfs guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang direct rootfs");
    }

    #[test]
    fn signed_executable_serves_mmds_to_direct_rootfs_guest() {
        run_direct_rootfs_mmds_guest_fetch_test(DirectRootfsMmdsFetchCase {
            request_context: "MMDS guest fetch",
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

        let mmds_response = http_put_json(
            &socket_path,
            "/mmds",
            r#"{"meta-data":{"bangbang-marker":"BANGBANG_MMDS_GUEST_VALUE"}}"#,
        );
        let mmds_context = format!("PUT /mmds {}", case.request_context);
        assert_no_content_response(&mmds_response, &mmds_context);

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
        let output = bangbang.wait_for_exit();

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

        let output = bangbang.wait_for_exit();

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
        let output = bangbang.wait_for_exit();

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

        let output = bangbang.wait_for_exit();

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

    fn assert_startup_time_metrics_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!(
                "metrics output {} should be readable: {err}",
                path.display()
            )
        });

        assert!(
            output.contains(r#""start_time_us":1000"#),
            "metrics output should include start_time_us; output:\n{output}"
        );
        assert!(
            output.contains(r#""start_time_cpu_us":2000"#),
            "metrics output should include start_time_cpu_us; output:\n{output}"
        );
        assert!(
            output.contains(r#""parent_cpu_time_us":3000"#),
            "metrics output should include parent_cpu_time_us; output:\n{output}"
        );
    }

    fn assert_logger_output(path: &Path) {
        let output = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!("logger output {} should be readable: {err}", path.display())
        });

        assert_eq!(
            output, "action=InstanceStart\naction=FlushMetrics\n",
            "logger output should include the expected action records"
        );
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
