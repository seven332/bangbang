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
    BangbangProcess, TestDir, assert_clean_shutdown, assert_no_content_response,
    assert_ok_response, assert_response_contains, http_get, http_json, http_put_json, json_string,
    path_text,
};

const BANGBANG_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    assert_response_contains(&vm_config, r#""is_root_device":true"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""is_read_only":true"#, "GET /vm/config");
    assert_response_contains(&vm_config, r#""partuuid":"0eaa91a0-01""#, "GET /vm/config");

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

fn assert_bad_request_response(response: &str, request_name: &str) {
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "{request_name} should return 400 Bad Request; response:\n{response}"
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
