// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;

use support::{
    BangbangProcess, TestDir, assert_bad_request_response, assert_clean_shutdown,
    assert_no_content_response, assert_ok_response, assert_response_contains, http_get, http_json,
    http_no_body, http_put_json, http_raw, json_string, path_text,
};

use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;
use bangbang_runtime::machine::MAX_MEM_SIZE_MIB;

const BANGBANG_VERSION: &str = env!("CARGO_PKG_VERSION");
const ARGUMENT_PARSING_EXIT_CODE: i32 = 153;

#[test]
fn executable_serves_api_and_shuts_down_cleanly() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    assert!(
        socket_path.exists(),
        "bangbang should publish the configured API socket"
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET /");
    assert_response_contains(&instance_info, r#""app_name":"bangbang""#, "GET /");
    assert_response_contains(&instance_info, &format!(r#""id":"{instance_id}""#), "GET /");
    assert_response_contains(&instance_info, r#""state":"Not started""#, "GET /");
    assert_response_contains(
        &instance_info,
        &format!(r#""vmm_version":"{BANGBANG_VERSION}""#),
        "GET /",
    );

    let version = http_get(&socket_path, "/version");
    assert_ok_response(&version, "GET /version");
    assert_response_contains(
        &version,
        &format!(r#""firecracker_version":"{BANGBANG_VERSION}""#),
        "GET /version",
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config");
    assert_response_contains(&vm_config, r#""machine-config":"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""drives":[]"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""network-interfaces":[]"#, "GET /vm/config");

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_accepts_firecracker_startup_time_args() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start_with_extra_args(
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
    assert_ok_response(&instance_info, "GET / with startup time args");
    assert_response_contains(
        &instance_info,
        &format!(r#""id":"{instance_id}""#),
        "GET / with startup time args",
    );
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / with startup time args",
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config with startup time args");
    assert!(
        !vm_config.contains("start_time_us")
            && !vm_config.contains("start_time_cpu_us")
            && !vm_config.contains("parent_cpu_time_us"),
        "GET /vm/config should not expose process startup timing; response:\n{vm_config}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_api_payload_over_limit_without_stopping() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let request = format!(
        "PUT /mmds HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        HTTP_MAX_PAYLOAD_SIZE + 1
    );
    assert!(
        request.len() <= HTTP_MAX_PAYLOAD_SIZE,
        "fixture should exercise declared payload length rejection, not a large write"
    );

    let oversized_response = http_raw(&socket_path, request.as_bytes());
    assert_bad_request_response(&oversized_response, "oversized PUT /mmds");
    assert_response_contains(
        &oversized_response,
        r#"{"fault_message":"HTTP request payload exceeds the configured limit."}"#,
        "oversized PUT /mmds",
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after oversized request");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after oversized request",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_handles_sigint_shutdown_cleanly() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    assert!(
        socket_path.exists(),
        "bangbang should publish the configured API socket before SIGINT"
    );

    assert_clean_shutdown(bangbang.interrupt(), &socket_path, "bangbang SIGINT");
}

#[test]
fn executable_rejects_unsupported_firecracker_process_flags_before_socket_publication() {
    for (name, args, private_value) in [
        ("boot-timer", &["--boot-timer"][..], None),
        (
            "describe-snapshot",
            &["--describe-snapshot", "secret-snapshot.vmstate"],
            Some("secret-snapshot.vmstate"),
        ),
        ("enable-pci", &["--enable-pci"], None),
        ("no-seccomp", &["--no-seccomp"], None),
        (
            "seccomp-filter",
            &["--seccomp-filter", "secret-seccomp.bpf"],
            Some("secret-seccomp.bpf"),
        ),
        ("snapshot-version", &["--snapshot-version"], None),
    ] {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join(format!("{name}.socket"));
        let instance_id = test_dir.instance_id();

        let output =
            BangbangProcess::start_with_extra_args_expect_failure(&socket_path, &instance_id, args);

        assert_eq!(
            output.status.code(),
            Some(ARGUMENT_PARSING_EXIT_CODE),
            "unsupported --{name} should fail with the argument parsing exit code; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            output.stderr.contains(&format!(
                "bangbang: unsupported Firecracker argument: --{name}"
            )),
            "unsupported --{name} should report a Firecracker argument rejection; stderr:\n{}",
            output.stderr
        );
        assert!(
            !output.stdout.contains("status: API server listening"),
            "unsupported --{name} must not report API readiness; stdout:\n{}",
            output.stdout
        );
        if let Some(private_value) = private_value {
            assert!(
                !output.stdout.contains(private_value) && !output.stderr.contains(private_value),
                "unsupported --{name} failure must not echo private argument value {private_value:?}; stdout:\n{}\nstderr:\n{}",
                output.stdout,
                output.stderr
            );
        }
        assert!(
            !socket_path.exists(),
            "unsupported --{name} must fail before publishing the API socket"
        );
    }
}

#[test]
fn executable_rejects_snapshot_requests_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    for (path, body, private_values) in [
        (
            "/snapshot/create",
            r#"{"snapshot_path":"secret-create.vmstate","mem_file_path":"secret-create.mem"}"#,
            &["secret-create.vmstate", "secret-create.mem"][..],
        ),
        (
            "/snapshot/load",
            r#"{"snapshot_path":"secret-load.vmstate","mem_backend":{"backend_path":"secret-load.mem","backend_type":"File"}}"#,
            &["secret-load.vmstate", "secret-load.mem"][..],
        ),
    ] {
        let response = http_put_json(&socket_path, path, body);

        assert_bad_request_response(&response, path);
        assert_response_contains(
            &response,
            r#"{"fault_message":"Snapshot and restore are not supported."}"#,
            path,
        );
        for private_value in private_values {
            assert!(
                !response.contains(private_value),
                "{path} must not echo private snapshot path {private_value:?}; response:\n{response}"
            );
        }
    }

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after rejected snapshots");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after rejected snapshots",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_remaining_device_requests_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    for (path, body, fault_message, private_values) in [
        (
            "/balloon",
            r#"{"amount_mib":64,"deflate_on_oom":true}"#,
            "Balloon device is not supported.",
            &[][..],
        ),
        (
            "/pmem/pmem0",
            r#"{"id":"pmem0","path_on_host":"secret-pmem.img"}"#,
            "Pmem device is not supported.",
            &["secret-pmem.img"][..],
        ),
        (
            "/entropy",
            "{}",
            "Entropy device is not supported.",
            &[][..],
        ),
        (
            "/hotplug/memory",
            r#"{"total_size_mib":2048}"#,
            "Memory hotplug is not supported.",
            &[][..],
        ),
    ] {
        let response = http_put_json(&socket_path, path, body);

        assert_bad_request_response(&response, path);
        assert_response_contains(
            &response,
            &format!(r#"{{"fault_message":"{fault_message}"}}"#),
            path,
        );
        for private_value in private_values {
            assert!(
                !response.contains(private_value),
                "{path} must not echo private device path {private_value:?}; response:\n{response}"
            );
        }
    }

    let memory_hotplug_get_response = http_get(&socket_path, "/hotplug/memory");
    assert_bad_request_response(&memory_hotplug_get_response, "GET /hotplug/memory");
    assert_response_contains(
        &memory_hotplug_get_response,
        r#"{"fault_message":"Memory hotplug is not supported."}"#,
        "GET /hotplug/memory",
    );

    let memory_hotplug_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/hotplug/memory",
        r#"{"requested_size_mib":256}"#,
    );
    assert_bad_request_response(&memory_hotplug_patch_response, "PATCH /hotplug/memory");
    assert_response_contains(
        &memory_hotplug_patch_response,
        r#"{"fault_message":"Memory hotplug is not supported."}"#,
        "PATCH /hotplug/memory",
    );

    for path in ["/balloon", "/balloon/statistics", "/balloon/hinting/status"] {
        let request_name = format!("GET {path}");
        let response = http_get(&socket_path, path);

        assert_bad_request_response(&response, &request_name);
        assert_response_contains(
            &response,
            r#"{"fault_message":"Balloon device is not supported."}"#,
            &request_name,
        );
    }

    for (path, body) in [
        ("/balloon", r#"{"amount_mib":32}"#),
        ("/balloon/statistics", r#"{"stats_polling_interval_s":1}"#),
        ("/balloon/hinting/start", r#"{"acknowledge_on_stop":false}"#),
    ] {
        let request_name = format!("PATCH {path}");
        let response = http_json(&socket_path, "PATCH", path, body);

        assert_bad_request_response(&response, &request_name);
        assert_response_contains(
            &response,
            r#"{"fault_message":"Balloon device is not supported."}"#,
            &request_name,
        );
    }

    let balloon_hinting_stop_response =
        http_no_body(&socket_path, "PATCH", "/balloon/hinting/stop");
    assert_bad_request_response(
        &balloon_hinting_stop_response,
        "PATCH /balloon/hinting/stop",
    );
    assert_response_contains(
        &balloon_hinting_stop_response,
        r#"{"fault_message":"Balloon device is not supported."}"#,
        "PATCH /balloon/hinting/stop",
    );

    let pmem_delete_response = http_no_body(&socket_path, "DELETE", "/pmem/pmem0");
    assert_bad_request_response(&pmem_delete_response, "DELETE /pmem/pmem0");
    assert_response_contains(
        &pmem_delete_response,
        r#"{"fault_message":"Pmem device is not supported."}"#,
        "DELETE /pmem/pmem0",
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after rejected remaining devices");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after rejected remaining devices",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_config_file_failure_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = test_dir.path().join("vm-config.json");
    let instance_id = test_dir.instance_id();
    fs::write(&config_path, "{").expect("malformed config file should be written");

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert!(
        !output.status.success(),
        "malformed config file should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "malformed config file should fail before API socket publication"
    );
    assert!(
        output
            .stderr
            .contains("bangbang: config-file error: malformed config file"),
        "stderr should describe config-file parse failure without JSON contents; stderr:\n{}",
        output.stderr
    );
}

#[test]
fn executable_no_api_config_file_failure_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = test_dir.path().join("vm-config.json");
    let instance_id = test_dir.instance_id();
    fs::write(&config_path, "{").expect("malformed config file should be written");

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert!(
        !output.status.success(),
        "malformed no-api config file should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "malformed no-api config file should not publish an API socket"
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "no-api failure must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("status: VM running without API"),
        "malformed no-api config must not report no-api readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        output
            .stderr
            .contains("bangbang: config-file error: malformed config file"),
        "stderr should describe config-file parse failure without JSON contents; stderr:\n{}",
        output.stderr
    );
}

#[test]
fn executable_metadata_startup_initializes_mmds() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let metadata_path = test_dir.path().join("metadata.json");
    let instance_id = test_dir.instance_id();
    fs::write(
        &metadata_path,
        r#"{"latest":{"meta-data":{"ami-id":"ami-bangbang"},"user-data":"from-startup"}}"#,
    )
    .expect("metadata file should be written");
    let bangbang = BangbangProcess::start_with_extra_args(
        &socket_path,
        &instance_id,
        &["--metadata", path_text(&metadata_path)],
    );

    let mmds_data = http_get(&socket_path, "/mmds");
    assert_ok_response(&mmds_data, "GET /mmds after --metadata");
    assert_response_contains(
        &mmds_data,
        r#""ami-id":"ami-bangbang""#,
        "GET /mmds after --metadata",
    );
    assert_response_contains(
        &mmds_data,
        r#""user-data":"from-startup""#,
        "GET /mmds after --metadata",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_metadata_failure_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let metadata_path = write_malformed_metadata_file(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--metadata", path_text(&metadata_path)],
    );

    assert_metadata_failure(&output, &socket_path, "metadata startup failure");
}

#[test]
fn executable_no_api_metadata_failure_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let metadata_path = write_malformed_metadata_file(&test_dir);
    let config_path = test_dir.path().join("vm-config.json");
    let instance_id = test_dir.instance_id();
    fs::write(
        &config_path,
        r#"{"boot-source":{"kernel_image_path":"/tmp/vmlinux"}}"#,
    )
    .expect("config file should be written");

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &[
            "--metadata",
            path_text(&metadata_path),
            "--config-file",
            path_text(&config_path),
            "--no-api",
        ],
    );

    assert_metadata_failure(&output, &socket_path, "no-api metadata startup failure");
}

#[test]
fn executable_config_file_rejected_drive_socket_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, private_socket_path) = write_rejected_drive_socket_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert_rejected_drive_socket_config_failure(
        &output,
        &socket_path,
        &private_socket_path,
        "config-file rejected drive socket",
    );
}

#[test]
fn executable_no_api_config_file_rejected_drive_socket_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, private_socket_path) = write_rejected_drive_socket_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert_rejected_drive_socket_config_failure(
        &output,
        &socket_path,
        &private_socket_path,
        "no-api config-file rejected drive socket",
    );
}

#[test]
fn executable_config_file_rejected_serial_rate_limiter_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, serial_output_path) = write_rejected_serial_rate_limiter_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert_rejected_serial_config_failure(
        &output,
        &socket_path,
        &serial_output_path,
        "config-file rejected serial rate limiter",
    );
}

#[test]
fn executable_no_api_config_file_rejected_serial_rate_limiter_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, serial_output_path) = write_rejected_serial_rate_limiter_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert_rejected_serial_config_failure(
        &output,
        &socket_path,
        &serial_output_path,
        "no-api config-file rejected serial rate limiter",
    );
}

#[test]
fn executable_configures_vm_before_start() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let kernel_path = test_dir.path().join("vmlinux");
    let rootfs_path = test_dir.path().join("rootfs.ext4");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let machine_response = http_put_json(
        &socket_path,
        "/machine-config",
        r#"{"vcpu_count":1,"mem_size_mib":256}"#,
    );
    assert_no_content_response(&machine_response, "PUT /machine-config");

    let kernel_path = path_text(&kernel_path);
    let kernel_path_json = json_string(kernel_path);
    let boot_body = format!(
        r#"{{"kernel_image_path":{kernel_path_json},"boot_args":"console=hvc0 reboot=k panic=1"}}"#
    );
    let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
    assert_no_content_response(&boot_response, "PUT /boot-source");

    let rootfs_path = path_text(&rootfs_path);
    let rootfs_path_json = json_string(rootfs_path);
    let drive_body = format!(
        r#"{{
            "drive_id":"rootfs",
            "path_on_host":{rootfs_path_json},
            "is_root_device":true,
            "is_read_only":true,
            "partuuid":"0eaa91a0-01"
        }}"#
    );
    let drive_response = http_put_json(&socket_path, "/drives/rootfs", &drive_body);
    assert_no_content_response(&drive_response, "PUT /drives/rootfs");

    let replaced_rootfs_path_json = json_string(path_text(&test_dir.path().join("replaced.ext4")));
    let drive_patch_body = format!(
        r#"{{
            "drive_id":"rootfs",
            "path_on_host":{replaced_rootfs_path_json}
        }}"#
    );
    let drive_patch_response =
        http_json(&socket_path, "PATCH", "/drives/rootfs", &drive_patch_body);
    assert_bad_request_response(&drive_patch_response, "PATCH /drives/rootfs");
    assert_response_contains(
        &drive_patch_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: UpdateBlockDevice"}"#,
        "PATCH /drives/rootfs",
    );

    let drive_delete_response = http_no_body(&socket_path, "DELETE", "/drives/rootfs");
    assert_bad_request_response(&drive_delete_response, "DELETE /drives/rootfs");
    assert_response_contains(
        &drive_delete_response,
        r#"{"fault_message":"Drive updates are not supported."}"#,
        "DELETE /drives/rootfs",
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config");
    assert_response_contains(&vm_config, r#""machine-config":"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""vcpu_count":1"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""mem_size_mib":256"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""boot-source":"#, "GET /vm/config");
    assert_response_contains(
        &vm_config,
        &format!(r#""kernel_image_path":{kernel_path_json}"#),
        "GET /vm/config",
    );
    assert_response_contains(
        &vm_config,
        r#""boot_args":"console=hvc0 reboot=k panic=1""#,
        "GET /vm/config",
    );
    assert_response_contains(&vm_config, r#""drives":["#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""drive_id":"rootfs""#, "GET /vm/config");
    assert_response_contains(
        &vm_config,
        &format!(r#""path_on_host":{rootfs_path_json}"#),
        "GET /vm/config",
    );
    assert!(
        !vm_config.contains(&format!(r#""path_on_host":{replaced_rootfs_path_json}"#)),
        "failed PATCH or DELETE /drives/rootfs must not mutate drive path; response:\n{vm_config}"
    );

    let cpu_config_response = http_put_json(&socket_path, "/cpu-config", "{}");
    assert_bad_request_response(&cpu_config_response, "PUT /cpu-config");
    assert_response_contains(
        &cpu_config_response,
        r#"{"fault_message":"The requested operation is not supported: PutCpuConfig"}"#,
        "PUT /cpu-config",
    );
    let instance_info_after_cpu_config = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_cpu_config,
        r#""state":"Not started""#,
        "GET / after failed PUT /cpu-config",
    );

    let vm_state_response = http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Paused"}"#);
    assert_bad_request_response(&vm_state_response, "PATCH /vm");
    assert_response_contains(
        &vm_state_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: Pause"}"#,
        "PATCH /vm",
    );
    let instance_info_after_vm_state_patch = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_vm_state_patch,
        r#""state":"Not started""#,
        "GET / after failed PATCH /vm",
    );

    let flush_metrics_response = http_put_json(
        &socket_path,
        "/actions",
        r#"{"action_type":"FlushMetrics"}"#,
    );
    assert_bad_request_response(&flush_metrics_response, "PUT /actions FlushMetrics");
    assert_response_contains(
        &flush_metrics_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: FlushMetrics"}"#,
        "PUT /actions FlushMetrics",
    );
    let instance_info_after_flush_metrics = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_flush_metrics,
        r#""state":"Not started""#,
        "GET / after failed PUT /actions FlushMetrics",
    );

    let send_ctrl_alt_del_response = http_put_json(
        &socket_path,
        "/actions",
        r#"{"action_type":"SendCtrlAltDel"}"#,
    );
    assert_bad_request_response(&send_ctrl_alt_del_response, "PUT /actions SendCtrlAltDel");
    assert_response_contains(
        &send_ctrl_alt_del_response,
        r#"{"fault_message":"SendCtrlAltDel is not supported on aarch64."}"#,
        "PUT /actions SendCtrlAltDel",
    );
    let instance_info_after_send_ctrl_alt_del = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_send_ctrl_alt_del,
        r#""state":"Not started""#,
        "GET / after failed PUT /actions SendCtrlAltDel",
    );

    assert_response_contains(&vm_config, r#""is_root_device":true"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""is_read_only":true"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""partuuid":"0eaa91a0-01""#, "GET /vm/config");

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_configures_observability_without_vm_config() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let metrics_path = test_dir.path().join("metrics.out");
    let second_metrics_path = test_dir.path().join("metrics-second.out");
    let logger_path = test_dir.path().join("logger.out");
    let serial_output_path = test_dir.path().join("serial.out");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let metrics_path_json = json_string(path_text(&metrics_path));
    let metrics_body = format!(r#"{{"metrics_path":{metrics_path_json}}}"#);
    let metrics_response = http_put_json(&socket_path, "/metrics", &metrics_body);
    assert_no_content_response(&metrics_response, "PUT /metrics");
    assert!(
        metrics_path.exists(),
        "PUT /metrics should create the configured output file"
    );

    let second_metrics_path_json = json_string(path_text(&second_metrics_path));
    let second_metrics_body = format!(r#"{{"metrics_path":{second_metrics_path_json}}}"#);
    let second_metrics_response = http_put_json(&socket_path, "/metrics", &second_metrics_body);
    assert_bad_request_response(&second_metrics_response, "second PUT /metrics");
    assert_response_contains(
        &second_metrics_response,
        r#"{"fault_message":"metrics system is already initialized"}"#,
        "second PUT /metrics",
    );
    assert!(
        !second_metrics_response.contains(path_text(&second_metrics_path)),
        "duplicate PUT /metrics must not echo the rejected output path; response:\n{second_metrics_response}"
    );
    assert!(
        metrics_path.exists(),
        "duplicate PUT /metrics must keep the original output file"
    );
    assert!(
        !second_metrics_path.exists(),
        "duplicate PUT /metrics must not create the second output file"
    );

    let logger_path_json = json_string(path_text(&logger_path));
    let logger_body = format!(
        r#"{{
            "log_path":{logger_path_json},
            "level":"Warning",
            "show_level":true,
            "show_log_origin":true,
            "module":"api_server"
        }}"#
    );
    let logger_response = http_put_json(&socket_path, "/logger", &logger_body);
    assert_no_content_response(&logger_response, "PUT /logger");
    assert!(
        logger_path.exists(),
        "PUT /logger should create the configured output file"
    );

    let serial_output_path_json = json_string(path_text(&serial_output_path));
    let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
    let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
    assert_no_content_response(&serial_response, "PUT /serial");
    assert!(
        !serial_output_path.exists(),
        "PUT /serial should store the output path without creating it before startup"
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config after observability config");
    assert_response_contains(
        &vm_config,
        r#""machine-config":"#,
        "GET /vm/config after observability config",
    );
    assert!(
        !vm_config.contains("metrics"),
        "GET /vm/config must not include metrics observability state; response:\n{vm_config}"
    );
    assert!(
        !vm_config.contains("logger"),
        "GET /vm/config must not include logger observability state; response:\n{vm_config}"
    );
    assert!(
        !vm_config.contains("serial"),
        "GET /vm/config must not include serial observability state; response:\n{vm_config}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_configures_writeback_drive_cache_type() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let drive_path = test_dir.path().join("writeback.img");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let drive_path_json = json_string(path_text(&drive_path));
    let drive_body = format!(
        r#"{{
            "drive_id":"cache",
            "path_on_host":{drive_path_json},
            "is_root_device":false,
            "is_read_only":false,
            "cache_type":"Writeback"
        }}"#
    );
    let drive_response = http_put_json(&socket_path, "/drives/cache", &drive_body);
    assert_no_content_response(&drive_response, "PUT /drives/cache");

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config after writeback drive");
    assert_response_contains(
        &vm_config,
        r#""drive_id":"cache""#,
        "GET /vm/config after writeback drive",
    );
    assert_response_contains(
        &vm_config,
        &format!(r#""path_on_host":{drive_path_json}"#),
        "GET /vm/config after writeback drive",
    );
    assert_response_contains(
        &vm_config,
        r#""cache_type":"Writeback""#,
        "GET /vm/config after writeback drive",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_unsupported_drive_options_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let accepted_drive_path = test_dir.path().join("accepted.img");
    let rate_limited_drive_path = test_dir.path().join("rate-limited.img");
    let async_drive_path = test_dir.path().join("async.img");
    let socket_drive_path = test_dir.path().join("socket.img");
    let private_socket_path = test_dir.path().join("private-vhost.sock");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let accepted_drive_path_json = json_string(path_text(&accepted_drive_path));
    let accepted_drive_body = format!(
        r#"{{
            "drive_id":"accepted",
            "path_on_host":{accepted_drive_path_json},
            "is_root_device":false,
            "is_read_only":false
        }}"#
    );
    let accepted_response = http_put_json(&socket_path, "/drives/accepted", &accepted_drive_body);
    assert_no_content_response(&accepted_response, "PUT /drives/accepted");

    let assert_only_accepted_drive =
        |request_name: &str, rejected_drive_id: &str, rejected_path_json: &str| {
            let vm_config = http_get(&socket_path, "/vm/config");
            assert_ok_response(&vm_config, request_name);
            assert_response_contains(&vm_config, r#""drive_id":"accepted""#, request_name);
            assert_response_contains(
                &vm_config,
                &format!(r#""path_on_host":{accepted_drive_path_json}"#),
                request_name,
            );
            assert_eq!(
                vm_config.matches(r#""drive_id":"#).count(),
                1,
                "{request_name} must keep only the accepted drive; response:\n{vm_config}"
            );
            assert!(
                !vm_config.contains(&format!(r#""drive_id":"{rejected_drive_id}""#)),
                "{request_name} must not store rejected drive {rejected_drive_id}; response:\n{vm_config}"
            );
            assert!(
                !vm_config.contains(&format!(r#""path_on_host":{rejected_path_json}"#)),
                "{request_name} must not store rejected drive path; response:\n{vm_config}"
            );
        };

    let rate_limited_drive_path_json = json_string(path_text(&rate_limited_drive_path));
    let rate_limiter_body = format!(
        r#"{{
            "drive_id":"rate_limited",
            "path_on_host":{rate_limited_drive_path_json},
            "is_root_device":false,
            "rate_limiter":{{
                "bandwidth":{{
                    "size":1000,
                    "one_time_burst":1000,
                    "refill_time":100
                }}
            }}
        }}"#
    );
    let rate_limiter_response =
        http_put_json(&socket_path, "/drives/rate_limited", &rate_limiter_body);
    assert_bad_request_response(&rate_limiter_response, "PUT /drives/rate_limited");
    assert_response_contains(
        &rate_limiter_response,
        r#"{"fault_message":"drive rate_limiter is not supported"}"#,
        "PUT /drives/rate_limited",
    );
    assert_only_accepted_drive(
        "GET /vm/config after rejected drive rate_limiter",
        "rate_limited",
        &rate_limited_drive_path_json,
    );

    let async_drive_path_json = json_string(path_text(&async_drive_path));
    let async_body = format!(
        r#"{{
            "drive_id":"async",
            "path_on_host":{async_drive_path_json},
            "is_root_device":false,
            "io_engine":"Async"
        }}"#
    );
    let async_response = http_put_json(&socket_path, "/drives/async", &async_body);
    assert_bad_request_response(&async_response, "PUT /drives/async");
    assert_response_contains(
        &async_response,
        r#"{"fault_message":"drive io_engine Async is not supported"}"#,
        "PUT /drives/async",
    );
    assert_only_accepted_drive(
        "GET /vm/config after rejected drive io_engine",
        "async",
        &async_drive_path_json,
    );

    let socket_drive_path_json = json_string(path_text(&socket_drive_path));
    let private_socket_path_text = path_text(&private_socket_path);
    let private_socket_path_json = json_string(private_socket_path_text);
    let socket_body = format!(
        r#"{{
            "drive_id":"socket",
            "path_on_host":{socket_drive_path_json},
            "is_root_device":false,
            "socket":{private_socket_path_json}
        }}"#
    );
    let socket_response = http_put_json(&socket_path, "/drives/socket", &socket_body);
    assert_bad_request_response(&socket_response, "PUT /drives/socket");
    assert_response_contains(
        &socket_response,
        r#"{"fault_message":"drive socket is not supported"}"#,
        "PUT /drives/socket",
    );
    assert!(
        !socket_response.contains(private_socket_path_text),
        "rejected drive socket response must not echo private socket path; response:\n{socket_response}"
    );
    assert!(
        !private_socket_path.exists(),
        "rejected drive socket request must not create the private socket path"
    );
    assert_only_accepted_drive(
        "GET /vm/config after rejected drive socket",
        "socket",
        &socket_drive_path_json,
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_configures_network_and_mmds() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start_with_extra_args(
        &socket_path,
        &instance_id,
        &["--mmds-size-limit", "512"],
    );

    let network_response = http_put_json(
        &socket_path,
        "/network-interfaces/eth0",
        r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9A:BC","mtu":1500}"#,
    );
    assert_no_content_response(&network_response, "PUT /network-interfaces/eth0");

    let mmds_config_response = http_put_json(
        &socket_path,
        "/mmds/config",
        r#"{"network_interfaces":["eth0"],"version":"V2","ipv4_address":"169.254.169.254","imds_compat":true}"#,
    );
    assert_no_content_response(&mmds_config_response, "PUT /mmds/config");

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config");
    assert_response_contains(&vm_config, r#""network-interfaces":["#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""iface_id":"eth0""#, "GET /vm/config");
    assert_response_contains(
        &vm_config,
        r#""host_dev_name":"vmnet:shared""#,
        "GET /vm/config",
    );
    assert_response_contains(
        &vm_config,
        r#""guest_mac":"12:34:56:78:9a:bc""#,
        "GET /vm/config",
    );
    assert_response_contains(&vm_config, r#""mtu":1500"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""mmds-config":"#, "GET /vm/config");
    assert_response_contains(
        &vm_config,
        r#""network_interfaces":["eth0"]"#,
        "GET /vm/config",
    );
    assert_response_contains(&vm_config, r#""version":"V2""#, "GET /vm/config");
    assert_response_contains(
        &vm_config,
        r#""ipv4_address":"169.254.169.254""#,
        "GET /vm/config",
    );
    assert_response_contains(&vm_config, r#""imds_compat":true"#, "GET /vm/config");

    let network_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/network-interfaces/eth0",
        r#"{"iface_id":"eth0","rx_rate_limiter":null}"#,
    );
    assert_bad_request_response(&network_patch_response, "PATCH /network-interfaces/eth0");
    assert_response_contains(
        &network_patch_response,
        r#"{"fault_message":"Network interface updates are not supported."}"#,
        "PATCH /network-interfaces/eth0",
    );

    let mismatched_network_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/network-interfaces/eth0",
        r#"{"iface_id":"eth1"}"#,
    );
    assert_bad_request_response(
        &mismatched_network_patch_response,
        "mismatched PATCH /network-interfaces/eth0",
    );
    assert_response_contains(
        &mismatched_network_patch_response,
        r#"{"fault_message":"path iface_id must match body iface_id."}"#,
        "mismatched PATCH /network-interfaces/eth0",
    );

    let malformed_network_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/network-interfaces/eth0",
        "not-json",
    );
    assert_bad_request_response(
        &malformed_network_patch_response,
        "malformed PATCH /network-interfaces/eth0",
    );
    assert_response_contains(
        &malformed_network_patch_response,
        r#"{"fault_message":"Malformed HTTP request."}"#,
        "malformed PATCH /network-interfaces/eth0",
    );

    let network_delete_response = http_no_body(&socket_path, "DELETE", "/network-interfaces/eth0");
    assert_bad_request_response(&network_delete_response, "DELETE /network-interfaces/eth0");
    assert_response_contains(
        &network_delete_response,
        r#"{"fault_message":"Network interface updates are not supported."}"#,
        "DELETE /network-interfaces/eth0",
    );

    let vm_config_after_network_updates = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &vm_config_after_network_updates,
        "GET /vm/config after rejected network updates",
    );
    assert_response_contains(
        &vm_config_after_network_updates,
        r#""iface_id":"eth0""#,
        "GET /vm/config after rejected network updates",
    );
    assert_response_contains(
        &vm_config_after_network_updates,
        r#""host_dev_name":"vmnet:shared""#,
        "GET /vm/config after rejected network updates",
    );
    assert_response_contains(
        &vm_config_after_network_updates,
        r#""guest_mac":"12:34:56:78:9a:bc""#,
        "GET /vm/config after rejected network updates",
    );
    assert_response_contains(
        &vm_config_after_network_updates,
        r#""mtu":1500"#,
        "GET /vm/config after rejected network updates",
    );
    assert!(
        !vm_config_after_network_updates.contains(r#""iface_id":"eth1""#),
        "rejected network updates must not add a new interface; response:\n{vm_config_after_network_updates}"
    );

    let put_mmds_response = http_put_json(
        &socket_path,
        "/mmds",
        r#"{"latest":{"meta-data":{"ami-id":"ami-bangbang","remove-me":"yes"},"user-data":"before"}}"#,
    );
    assert_no_content_response(&put_mmds_response, "PUT /mmds");

    let patch_mmds_response = http_json(
        &socket_path,
        "PATCH",
        "/mmds",
        r#"{"latest":{"dynamic":{"instance-identity":"document"},"meta-data":{"ami-id":"ami-updated","remove-me":null}}}"#,
    );
    assert_no_content_response(&patch_mmds_response, "PATCH /mmds");

    let mmds_data = http_get(&socket_path, "/mmds");
    assert_ok_response(&mmds_data, "GET /mmds");
    assert_response_contains(&mmds_data, r#""ami-id":"ami-updated""#, "GET /mmds");
    assert_response_contains(&mmds_data, r#""user-data":"before""#, "GET /mmds");
    assert_response_contains(&mmds_data, r#""instance-identity":"document""#, "GET /mmds");
    assert!(
        !mmds_data.contains("remove-me"),
        "PATCH /mmds should remove null-valued fields; response:\n{mmds_data}"
    );

    let oversized_value = "x".repeat(600);
    let oversized_value_json = json_string(&oversized_value);
    let oversized_patch_body = format!(r#"{{"latest":{{"user-data":{oversized_value_json}}}}}"#);
    let oversized_response = http_json(&socket_path, "PATCH", "/mmds", &oversized_patch_body);
    assert_bad_request_response(&oversized_response, "oversized PATCH /mmds");
    assert_response_contains(
        &oversized_response,
        "The MMDS data store size limit was exceeded",
        "oversized PATCH /mmds",
    );

    let mmds_after_oversized_patch = http_get(&socket_path, "/mmds");
    assert_ok_response(
        &mmds_after_oversized_patch,
        "GET /mmds after oversized patch",
    );
    assert_response_contains(
        &mmds_after_oversized_patch,
        r#""ami-id":"ami-updated""#,
        "GET /mmds after oversized patch",
    );
    assert_response_contains(
        &mmds_after_oversized_patch,
        r#""user-data":"before""#,
        "GET /mmds after oversized patch",
    );
    assert_response_contains(
        &mmds_after_oversized_patch,
        r#""instance-identity":"document""#,
        "GET /mmds after oversized patch",
    );
    assert!(
        !mmds_after_oversized_patch.contains(&oversized_value),
        "oversized PATCH /mmds must not partially mutate the data store; response:\n{mmds_after_oversized_patch}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_configures_vsock() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let uds_path = test_dir.path().join("v.sock");
    let invalid_uds_path = test_dir.path().join("private-v.sock");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let uds_path_json = json_string(path_text(&uds_path));
    let vsock_body = format!(r#"{{"vsock_id":"vsock0","guest_cid":3,"uds_path":{uds_path_json}}}"#);
    let vsock_response = http_put_json(&socket_path, "/vsock", &vsock_body);
    assert_no_content_response(&vsock_response, "PUT /vsock");
    assert!(
        !uds_path.exists(),
        "PUT /vsock should store config without binding the host socket path"
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config");
    assert_response_contains(&vm_config, r#""vsock":"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""guest_cid":3"#, "GET /vm/config");
    assert_response_contains(
        &vm_config,
        &format!(r#""uds_path":{uds_path_json}"#),
        "GET /vm/config",
    );
    assert!(
        !vm_config.contains("vsock_id"),
        "GET /vm/config should not emit deprecated vsock_id; response:\n{vm_config}"
    );

    let invalid_uds_path_json = json_string(path_text(&invalid_uds_path));
    let invalid_vsock_body = format!(r#"{{"guest_cid":2,"uds_path":{invalid_uds_path_json}}}"#);
    let invalid_vsock_response = http_put_json(&socket_path, "/vsock", &invalid_vsock_body);
    assert_bad_request_response(&invalid_vsock_response, "invalid PUT /vsock");
    assert_response_contains(
        &invalid_vsock_response,
        r#"{"fault_message":"vsock guest_cid 2 is below minimum 3"}"#,
        "invalid PUT /vsock",
    );
    assert!(
        !invalid_vsock_response.contains(path_text(&invalid_uds_path)),
        "invalid PUT /vsock should not echo the rejected private path; response:\n{invalid_vsock_response}"
    );
    assert!(
        !invalid_uds_path.exists(),
        "invalid PUT /vsock should not create the rejected host socket path"
    );

    let vm_config_after_invalid_vsock = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &vm_config_after_invalid_vsock,
        "GET /vm/config after invalid PUT /vsock",
    );
    assert_response_contains(
        &vm_config_after_invalid_vsock,
        r#""guest_cid":3"#,
        "GET /vm/config after invalid PUT /vsock",
    );
    assert_response_contains(
        &vm_config_after_invalid_vsock,
        &format!(r#""uds_path":{uds_path_json}"#),
        "GET /vm/config after invalid PUT /vsock",
    );
    assert!(
        !vm_config_after_invalid_vsock.contains(path_text(&invalid_uds_path)),
        "invalid PUT /vsock must not mutate stored config; response:\n{vm_config_after_invalid_vsock}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_serves_and_patches_machine_config() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let default_config = http_get(&socket_path, "/machine-config");
    assert_ok_response(&default_config, "GET /machine-config default");
    assert_response_contains(
        &default_config,
        r#""vcpu_count":1"#,
        "GET /machine-config default",
    );
    assert_response_contains(
        &default_config,
        r#""mem_size_mib":128"#,
        "GET /machine-config default",
    );
    assert_response_contains(
        &default_config,
        r#""smt":false"#,
        "GET /machine-config default",
    );
    assert_response_contains(
        &default_config,
        r#""track_dirty_pages":false"#,
        "GET /machine-config default",
    );
    assert_response_contains(
        &default_config,
        r#""huge_pages":"None""#,
        "GET /machine-config default",
    );

    let put_response = http_put_json(
        &socket_path,
        "/machine-config",
        r#"{"vcpu_count":2,"mem_size_mib":256}"#,
    );
    assert_no_content_response(&put_response, "PUT /machine-config");

    let patched_response = http_json(
        &socket_path,
        "PATCH",
        "/machine-config",
        r#"{"mem_size_mib":512}"#,
    );
    assert_no_content_response(&patched_response, "PATCH /machine-config");

    let patched_config = http_get(&socket_path, "/machine-config");
    assert_ok_response(&patched_config, "GET /machine-config patched");
    assert_response_contains(
        &patched_config,
        r#""vcpu_count":2"#,
        "GET /machine-config patched",
    );
    assert_response_contains(
        &patched_config,
        r#""mem_size_mib":512"#,
        "GET /machine-config patched",
    );
    assert_response_contains(
        &patched_config,
        r#""track_dirty_pages":false"#,
        "GET /machine-config patched",
    );

    let oversized_mem_size_mib = MAX_MEM_SIZE_MIB + 1;
    let oversized_put_response = http_put_json(
        &socket_path,
        "/machine-config",
        &format!(r#"{{"vcpu_count":4,"mem_size_mib":{oversized_mem_size_mib}}}"#),
    );
    assert_bad_request_response(&oversized_put_response, "PUT /machine-config oversized");
    assert_response_contains(
        &oversized_put_response,
        &format!(r#"{{"fault_message":"machine mem_size_mib must be in 1..={MAX_MEM_SIZE_MIB}"}}"#),
        "PUT /machine-config oversized",
    );

    let oversized_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/machine-config",
        &format!(r#"{{"mem_size_mib":{oversized_mem_size_mib}}}"#),
    );
    assert_bad_request_response(&oversized_patch_response, "PATCH /machine-config oversized");
    assert_response_contains(
        &oversized_patch_response,
        &format!(r#"{{"fault_message":"machine mem_size_mib must be in 1..={MAX_MEM_SIZE_MIB}"}}"#),
        "PATCH /machine-config oversized",
    );

    let invalid_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/machine-config",
        r#"{"track_dirty_pages":true}"#,
    );
    assert_bad_request_response(&invalid_patch_response, "PATCH /machine-config invalid");
    assert_response_contains(
        &invalid_patch_response,
        r#"{"fault_message":"machine track_dirty_pages is not supported"}"#,
        "PATCH /machine-config invalid",
    );

    let after_invalid_patch = http_get(&socket_path, "/machine-config");
    assert_ok_response(
        &after_invalid_patch,
        "GET /machine-config after invalid patch",
    );
    assert_response_contains(
        &after_invalid_patch,
        r#""vcpu_count":2"#,
        "GET /machine-config after invalid patch",
    );
    assert_response_contains(
        &after_invalid_patch,
        r#""mem_size_mib":512"#,
        "GET /machine-config after invalid patch",
    );
    assert_response_contains(
        &after_invalid_patch,
        r#""track_dirty_pages":false"#,
        "GET /machine-config after invalid patch",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_fails_when_api_socket_path_exists_without_removing_it() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    fs::write(&socket_path, "existing file").expect("fixture file should be written");
    let original_metadata =
        fs::symlink_metadata(&socket_path).expect("existing file metadata should be readable");

    let output = BangbangProcess::start_expect_failure(&socket_path, &instance_id);

    assert_eq!(
        output.status.code(),
        Some(1),
        "existing API socket path should fail with process failure; stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        output
            .stderr
            .contains("API server error: API socket path already exists"),
        "stderr should explain the API socket bind failure; stderr:\n{}",
        output.stderr
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "failed startup must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert_eq!(
        fs::read_to_string(&socket_path).expect("existing file should remain readable"),
        "existing file"
    );
    let current_metadata =
        fs::symlink_metadata(&socket_path).expect("existing file metadata should remain readable");
    assert_eq!(
        (current_metadata.dev(), current_metadata.ino()),
        (original_metadata.dev(), original_metadata.ino()),
        "failed startup must not replace the existing API socket path"
    );
}

#[test]
fn concurrent_executables_keep_api_sockets_isolated() {
    let first_dir = TestDir::new();
    let second_dir = TestDir::new();
    let first_socket_path = first_dir.path().join("api.socket");
    let second_socket_path = second_dir.path().join("api.socket");
    let first_instance_id = first_dir.instance_id();
    let second_instance_id = second_dir.instance_id();

    let first_bangbang = BangbangProcess::start(&first_socket_path, &first_instance_id);
    let second_bangbang = BangbangProcess::start(&second_socket_path, &second_instance_id);

    assert_instance_info_matches(
        &first_socket_path,
        &first_instance_id,
        &second_instance_id,
        "first bangbang",
    );
    assert_instance_info_matches(
        &second_socket_path,
        &second_instance_id,
        &first_instance_id,
        "second bangbang",
    );

    let first_output = first_bangbang.terminate();
    let second_output = second_bangbang.terminate();
    assert_clean_shutdown(first_output, &first_socket_path, "first bangbang");
    assert_clean_shutdown(second_output, &second_socket_path, "second bangbang");
}

fn write_rejected_drive_socket_config(
    test_dir: &TestDir,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let config_path = test_dir.path().join("vm-config.json");
    let drive_path = test_dir.path().join("drive.img");
    let private_socket_path = test_dir.path().join("private-vhost.sock");
    let drive_path_json = json_string(path_text(&drive_path));
    let private_socket_path_json = json_string(path_text(&private_socket_path));
    let config = format!(
        r#"{{
            "boot-source": {{"kernel_image_path": "/tmp/vmlinux"}},
            "drives": [{{
                "drive_id": "rootfs",
                "path_on_host": {drive_path_json},
                "is_root_device": true,
                "socket": {private_socket_path_json}
            }}]
        }}"#
    );
    fs::write(&config_path, config).expect("config file should be written");

    (config_path, private_socket_path)
}

fn write_rejected_serial_rate_limiter_config(
    test_dir: &TestDir,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let config_path = test_dir.path().join("vm-config.json");
    let serial_output_path = test_dir.path().join("private-serial.out");
    let serial_output_path_json = json_string(path_text(&serial_output_path));
    let config = format!(
        r#"{{
            "boot-source": {{"kernel_image_path": "/tmp/vmlinux"}},
            "serial": {{
                "serial_out_path": {serial_output_path_json},
                "rate_limiter": {{"bandwidth": {{"size": 1, "refill_time": 1}}}}
            }}
        }}"#
    );
    fs::write(&config_path, config).expect("config file should be written");

    (config_path, serial_output_path)
}

fn write_malformed_metadata_file(test_dir: &TestDir) -> std::path::PathBuf {
    let metadata_path = test_dir.path().join("metadata.json");
    fs::write(&metadata_path, r#"{"secret":"private-metadata-secret""#)
        .expect("malformed metadata file should be written");

    metadata_path
}

fn assert_metadata_failure(
    output: &support::CompletedProcess,
    socket_path: &std::path::Path,
    case_name: &str,
) {
    assert!(
        !output.status.success(),
        "{case_name} should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "{case_name} should fail before API socket publication"
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "{case_name} must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("status: VM running without API"),
        "{case_name} must not report no-api readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        output
            .stderr
            .contains("bangbang: metadata error: malformed metadata file"),
        "{case_name} stderr should describe metadata parse failure; stderr:\n{}",
        output.stderr
    );
    assert!(
        !output.stdout.contains("private-metadata-secret"),
        "{case_name} stdout must not echo metadata contents; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stderr.contains("private-metadata-secret"),
        "{case_name} stderr must not echo metadata contents; stderr:\n{}",
        output.stderr
    );
}

fn assert_rejected_drive_socket_config_failure(
    output: &support::CompletedProcess,
    socket_path: &std::path::Path,
    private_socket_path: &std::path::Path,
    case_name: &str,
) {
    assert!(
        !output.status.success(),
        "{case_name} should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "{case_name} should fail before API socket publication"
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "{case_name} must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("status: VM running without API"),
        "{case_name} must not report no-api readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        output
            .stderr
            .contains("bangbang: config-file error: failed to apply config-file action: drive socket is not supported"),
        "{case_name} stderr should describe config-file drive socket rejection; stderr:\n{}",
        output.stderr
    );
    let private_socket_path_text = path_text(private_socket_path);
    assert!(
        !output.stdout.contains(private_socket_path_text),
        "{case_name} stdout must not echo private drive socket path; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stderr.contains(private_socket_path_text),
        "{case_name} stderr must not echo private drive socket path; stderr:\n{}",
        output.stderr
    );
    assert!(
        !private_socket_path.exists(),
        "{case_name} must not create rejected drive socket path"
    );
}

fn assert_rejected_serial_config_failure(
    output: &support::CompletedProcess,
    socket_path: &std::path::Path,
    serial_output_path: &std::path::Path,
    case_name: &str,
) {
    assert!(
        !output.status.success(),
        "{case_name} should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "{case_name} should fail before API socket publication"
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "{case_name} must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("status: VM running without API"),
        "{case_name} must not report no-api readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stderr.contains(
            "bangbang: config-file error: failed to apply config-file action: serial output rate limiting is not supported"
        ),
        "{case_name} stderr should describe config-file serial rejection; stderr:\n{}",
        output.stderr
    );
    let serial_output_path_text = path_text(serial_output_path);
    assert!(
        !output.stdout.contains(serial_output_path_text),
        "{case_name} stdout must not echo private serial output path; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stderr.contains(serial_output_path_text),
        "{case_name} stderr must not echo private serial output path; stderr:\n{}",
        output.stderr
    );
    assert!(
        !serial_output_path.exists(),
        "{case_name} must not create rejected serial output path"
    );
}

fn assert_instance_info_matches(
    socket_path: &std::path::Path,
    expected_instance_id: &str,
    unexpected_instance_id: &str,
    process_name: &str,
) {
    let response = http_get(socket_path, "/");
    assert_ok_response(&response, process_name);
    assert_response_contains(
        &response,
        &format!(r#""id":"{expected_instance_id}""#),
        process_name,
    );
    assert!(
        !response.contains(&format!(r#""id":"{unexpected_instance_id}""#)),
        "{process_name} response should not contain another process id; response:\n{response}"
    );
}
