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
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::support::{
        BangbangProcess, TestDir, assert_bad_request_response, assert_clean_shutdown,
        assert_no_content_response, assert_ok_response, assert_response_contains, http_get,
        http_json, http_put_json, json_string, path_text,
    };

    const BANGBANG_GUEST_KERNEL_PATH_ENV: &str = "BANGBANG_GUEST_KERNEL_PATH";
    const BANGBANG_GUEST_INITRD_PATH_ENV: &str = "BANGBANG_GUEST_INITRD_PATH";
    const BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
    const BLOCK_WRITE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITE_OK";
    const DIRECT_ROOTFS_BLOCK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BLOCK_OK";
    const GUEST_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/init";
    const DIRECT_ROOTFS_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init";
    const GUEST_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn signed_executable_starts_instance_and_guest_writes_block_marker() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let backing_path = test_dir.path().join("data.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let metrics_path = test_dir.path().join("metrics.out");
        let logger_path = test_dir.path().join("logger.out");
        let uds_path = test_dir.path().join("vsock.sock");
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

        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, "GET /vm/config after InstanceStart");
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
        assert_metrics_output(&metrics_path);
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
        let uds_path = test_dir.path().join("config-file-vsock.sock");
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
        assert_metrics_output(&metrics_path);
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
        let uds_path = test_dir.path().join("no-api-config-file-vsock.sock");
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

    fn env_path(name: &str) -> PathBuf {
        match std::env::var_os(name) {
            Some(value) if value.is_empty() => panic!("{name} must not be empty"),
            Some(value) => PathBuf::from(value),
            None => panic!("{name} must be set"),
        }
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

    fn assert_metrics_output(path: &Path) {
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

            let ident = libc::uintptr_t::try_from(file.as_raw_fd())
                .map_err(|_| "watched file descriptor did not fit uintptr_t".to_string())?;
            let change = libc::kevent {
                ident,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND,
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
                    "failed to register file kqueue watch: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }

        fn wait_for_write(&self, timeout: Duration) -> Result<(), String> {
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
                    return Err("timed out waiting for file write event".to_string());
                }

                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(format!("failed while waiting for file write: {err}"));
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
