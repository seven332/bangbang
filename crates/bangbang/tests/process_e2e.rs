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
use std::os::unix::fs::{MetadataExt, symlink};

use support::{
    BangbangProcess, TestDir, assert_bad_request_response, assert_clean_shutdown,
    assert_no_content_response, assert_ok_response, assert_response_contains, http_get, http_json,
    http_no_body, http_put_json, http_raw, json_string, path_text,
};

use bangbang_api::HTTP_MAX_PAYLOAD_SIZE;
use bangbang_runtime::machine::MAX_MEM_SIZE_MIB;
use bangbang_runtime::snapshot_format::{
    NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES, NATIVE_V1_SNAPSHOT_VERSION, SNAPSHOT_ENVELOPE_HEADER_BYTES,
    SNAPSHOT_ENVELOPE_INTEGRITY_BYTES, encode_snapshot_envelope,
};
use crc64::crc64;

const BANGBANG_VERSION: &str = env!("CARGO_PKG_VERSION");
const BAD_SYSCALL_EXIT_CODE: i32 = 148;
const SIGBUS_EXIT_CODE: i32 = 149;
const SIGSEGV_EXIT_CODE: i32 = 150;
const SIGXFSZ_EXIT_CODE: i32 = 151;
const BAD_CONFIGURATION_EXIT_CODE: i32 = 152;
const ARGUMENT_PARSING_EXIT_CODE: i32 = 153;
const SIGXCPU_EXIT_CODE: i32 = 154;
const SIGHUP_EXIT_CODE: i32 = 156;
const SIGILL_EXIT_CODE: i32 = 157;
const MULTI_VCPU_STARTUP_ERROR: &str = "HVF arm64 boot session supports exactly 1 vCPU, got 2";

#[test]
fn executable_prints_help_and_exits_before_socket_publication() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("help.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &socket_path,
        &instance_id,
        &["--help"],
    );

    assert_eq!(output.stderr, "", "help should not write stderr");
    assert!(
        output.stdout.contains("Usage:\n  bangbang [OPTIONS]"),
        "help should print usage; stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("--api-sock <PATH>"),
        "help should list API socket option; stdout:\n{}",
        output.stdout
    );
    assert!(
        output
            .stdout
            .contains("Logger level: Off, Trace, Debug, Info, Warn, Warning, or Error"),
        "help should list accepted logger levels; stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("Current scope:"),
        "help should describe current public scope; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "help must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "help must not publish the API socket"
    );
}

#[test]
fn executable_prints_version_and_exits_before_socket_publication() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("version.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &socket_path,
        &instance_id,
        &["--version"],
    );

    assert_eq!(
        output.stdout,
        format!("bangbang {BANGBANG_VERSION}\n"),
        "version should report the package version"
    );
    assert_eq!(output.stderr, "", "version should not write stderr");
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "version must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "version must not publish the API socket"
    );
}

#[test]
fn executable_short_help_alias_exits_before_socket_publication() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("short-help.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &socket_path,
        &instance_id,
        &["-h"],
    );

    assert_eq!(output.stderr, "", "short help should not write stderr");
    assert!(
        output.stdout.contains("Usage:\n  bangbang [OPTIONS]"),
        "short help should print usage; stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("Current scope:"),
        "short help should describe current public scope; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "short help must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "short help must not publish the API socket"
    );
}

#[test]
fn executable_short_version_alias_exits_before_socket_publication() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("short-version.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &socket_path,
        &instance_id,
        &["-V"],
    );

    assert_eq!(
        output.stdout,
        format!("bangbang {BANGBANG_VERSION}\n"),
        "short version should report the package version"
    );
    assert_eq!(output.stderr, "", "short version should not write stderr");
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "short version must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "short version must not publish the API socket"
    );
}

#[test]
fn executable_help_precedence_ignores_later_unknown_args() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("help-precedence.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &socket_path,
        &instance_id,
        &["--help", "--unknown"],
    );

    assert_eq!(output.stderr, "", "help precedence should not write stderr");
    assert!(
        output.stdout.contains("Usage:\n  bangbang [OPTIONS]"),
        "help precedence should print usage; stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("Current scope:"),
        "help precedence should still print help scope; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("--unknown") && !output.stderr.contains("--unknown"),
        "help precedence must not report the later unknown argument; stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "help precedence must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "help precedence must not publish the API socket"
    );
}

#[test]
fn executable_version_precedence_ignores_later_unknown_args() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("version-precedence.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &socket_path,
        &instance_id,
        &["--version", "--unknown"],
    );

    assert_eq!(
        output.stdout,
        format!("bangbang {BANGBANG_VERSION}\n"),
        "version precedence should report the package version"
    );
    assert_eq!(
        output.stderr, "",
        "version precedence should not write stderr"
    );
    assert!(
        !output.stdout.contains("--unknown") && !output.stderr.contains("--unknown"),
        "version precedence must not report the later unknown argument; stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "version precedence must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "version precedence must not publish the API socket"
    );
}

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
    let socket_mode = fs::symlink_metadata(&socket_path)
        .expect("published API socket metadata should be readable")
        .mode()
        & 0o777;
    assert_eq!(
        socket_mode, 0o600,
        "bangbang should restrict the published API socket to owner-only access"
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
            "--start-time-us=1000",
            "--start-time-cpu-us=2000",
            "--parent-cpu-time-us=3000",
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
            && !vm_config.contains("parent_cpu_time_us")
            && !vm_config.contains("process_startup_time_us")
            && !vm_config.contains("process_startup_time_cpu_us"),
        "GET /vm/config should not expose process startup timing; response:\n{vm_config}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_accepts_boot_timer_flag() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang =
        BangbangProcess::start_with_extra_args(&socket_path, &instance_id, &["--boot-timer"]);

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / with boot timer");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / with boot timer",
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config with boot timer");
    assert!(
        !vm_config.contains("boot_timer") && !vm_config.contains("boot-timer"),
        "GET /vm/config should not expose process boot timer state; response:\n{vm_config}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_startup_metrics_path_writes_initial_metrics() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let metrics_path = test_dir.path().join("startup.metrics");
    let instance_id = test_dir.instance_id();
    let metrics_arg = format!("--metrics-path={}", path_text(&metrics_path));
    let future_start_time = u64::MAX.to_string();
    let bangbang = BangbangProcess::start_with_extra_args(
        &socket_path,
        &instance_id,
        &[
            metrics_arg.as_str(),
            "--start-time-us",
            future_start_time.as_str(),
            "--start-time-cpu-us",
            future_start_time.as_str(),
            "--parent-cpu-time-us=3000",
        ],
    );

    assert_eq!(
        fs::read_to_string(&metrics_path).expect("startup metrics should be readable"),
        "{\"api_server\":{\"process_startup_time_cpu_us\":3000,\"process_startup_time_us\":0},\"vmm\":{\"metrics_flush_count\":1}}\n"
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after startup metrics");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after startup metrics",
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
    assert!(
        oversized_response.starts_with("HTTP/1.1 413 Payload Too Large\r\n"),
        "oversized PUT /mmds should return 413 Payload Too Large; response:\n{oversized_response}"
    );
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
fn executable_rejects_malformed_http_request_without_stopping() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let malformed_response = http_raw(
        &socket_path,
        b"GET /version HTTP/1.1\r\nHost: localhost\r\nContent-Length: +0\r\nConnection: close\r\n\r\n",
    );
    assert_bad_request_response(&malformed_response, "malformed GET /version");
    assert_response_contains(
        &malformed_response,
        r#"{"fault_message":"Malformed HTTP request."}"#,
        "malformed GET /version",
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after malformed request");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after malformed request",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_invalid_api_routes_without_stopping() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    for (request_name, response) in [
        ("GET /unknown", http_get(&socket_path, "/unknown")),
        (
            "POST /version",
            http_no_body(&socket_path, "POST", "/version"),
        ),
    ] {
        assert_bad_request_response(&response, request_name);
        assert_response_contains(
            &response,
            r#"{"fault_message":"Invalid request method and/or path."}"#,
            request_name,
        );
    }

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after invalid API routes");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after invalid API routes",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_empty_mutating_api_requests_without_stopping() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    for (request_name, response, fault_message) in [
        (
            "empty PUT /actions",
            http_json(&socket_path, "PUT", "/actions", ""),
            r#"{"fault_message":"Empty PUT request."}"#,
        ),
        (
            "bodyless PUT /unknown",
            http_no_body(&socket_path, "PUT", "/unknown"),
            r#"{"fault_message":"Empty PUT request."}"#,
        ),
        (
            "empty PATCH /vm",
            http_json(&socket_path, "PATCH", "/vm", ""),
            r#"{"fault_message":"Empty PATCH request."}"#,
        ),
        (
            "bodyless PATCH /unknown",
            http_no_body(&socket_path, "PATCH", "/unknown"),
            r#"{"fault_message":"Empty PATCH request."}"#,
        ),
        (
            "body-carrying DELETE /drives/rootfs",
            http_json(&socket_path, "DELETE", "/drives/rootfs", "{}"),
            r#"{"fault_message":"Empty Delete request."}"#,
        ),
    ] {
        assert_bad_request_response(&response, request_name);
        assert_response_contains(&response, fault_message, request_name);
    }

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after empty mutating API requests");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after empty mutating API requests",
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
fn executable_handles_sigpipe_without_shutdown() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    bangbang.send_signal(libc::SIGPIPE, "SIGPIPE");

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after SIGPIPE");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after SIGPIPE",
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang SIGPIPE");
}

#[test]
fn executable_maps_firecracker_fatal_signals_to_exit_codes() {
    for (signal_name, signal, expected_exit_code) in [
        ("SIGSYS", libc::SIGSYS, BAD_SYSCALL_EXIT_CODE),
        ("SIGBUS", libc::SIGBUS, SIGBUS_EXIT_CODE),
        ("SIGSEGV", libc::SIGSEGV, SIGSEGV_EXIT_CODE),
        ("SIGXFSZ", libc::SIGXFSZ, SIGXFSZ_EXIT_CODE),
        ("SIGXCPU", libc::SIGXCPU, SIGXCPU_EXIT_CODE),
        ("SIGHUP", libc::SIGHUP, SIGHUP_EXIT_CODE),
        ("SIGILL", libc::SIGILL, SIGILL_EXIT_CODE),
    ] {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join(format!("{signal_name}.socket"));
        let instance_id = test_dir.instance_id();
        let bangbang = BangbangProcess::start(&socket_path, &instance_id);

        assert!(
            socket_path.exists(),
            "bangbang should publish the configured API socket before {signal_name}"
        );

        let output = bangbang.stop_with_signal(signal, signal_name);
        assert_eq!(
            output.status.code(),
            Some(expected_exit_code),
            "{signal_name} should make bangbang exit with the Firecracker-compatible exit code; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
    }
}

#[test]
fn executable_rejects_unsupported_firecracker_process_flags_before_socket_publication() {
    for (case_name, expected_name, args, private_value) in [
        ("enable-pci", "enable-pci", &["--enable-pci"][..], None),
        (
            "enable-pci-attached",
            "enable-pci",
            &["--enable-pci=secret-enable-pci-value"][..],
            Some("secret-enable-pci-value"),
        ),
        ("no-seccomp", "no-seccomp", &["--no-seccomp"][..], None),
        (
            "no-seccomp-attached",
            "no-seccomp",
            &["--no-seccomp=secret-no-seccomp-value"][..],
            Some("secret-no-seccomp-value"),
        ),
        (
            "seccomp-filter",
            "seccomp-filter",
            &["--seccomp-filter", "secret-seccomp.bpf"][..],
            Some("secret-seccomp.bpf"),
        ),
        (
            "seccomp-filter-attached",
            "seccomp-filter",
            &["--seccomp-filter=secret-seccomp.bpf"][..],
            Some("secret-seccomp.bpf"),
        ),
    ] {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join(format!("{case_name}.socket"));
        let instance_id = test_dir.instance_id();

        let output =
            BangbangProcess::start_with_extra_args_expect_failure(&socket_path, &instance_id, args);

        assert_eq!(
            output.status.code(),
            Some(ARGUMENT_PARSING_EXIT_CODE),
            "unsupported {case_name} should fail with the argument parsing exit code; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
        assert!(
            output.stderr.contains(&format!(
                "bangbang: unsupported Firecracker argument: --{expected_name}"
            )),
            "unsupported {case_name} should report a Firecracker argument rejection; stderr:\n{}",
            output.stderr
        );
        assert!(
            !output.stdout.contains("status: API server listening"),
            "unsupported {case_name} must not report API readiness; stdout:\n{}",
            output.stdout
        );
        if let Some(private_value) = private_value {
            assert!(
                !output.stdout.contains(private_value) && !output.stderr.contains(private_value),
                "unsupported {case_name} failure must not echo private argument value {private_value:?}; stdout:\n{}\nstderr:\n{}",
                output.stdout,
                output.stderr
            );
        }
        assert!(
            !socket_path.exists(),
            "unsupported {case_name} must fail before publishing the API socket"
        );
    }
}

#[test]
fn executable_reports_native_snapshot_versions_before_socket_publication() {
    let test_dir = TestDir::new();
    let expected = format!("v{NATIVE_V1_SNAPSHOT_VERSION}\n");

    let version_socket = test_dir.path().join("snapshot-version.socket");
    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &version_socket,
        &test_dir.instance_id(),
        &["--snapshot-version"],
    );
    assert_eq!(output.stdout, expected);
    assert_eq!(output.stderr, "");
    assert!(!version_socket.exists());

    let snapshot_path = test_dir.path().join("valid.vmstate");
    fs::write(
        &snapshot_path,
        encode_snapshot_envelope(b"opaque-state").expect("snapshot fixture should encode"),
    )
    .expect("snapshot fixture should be written");
    let describe_socket = test_dir.path().join("describe-snapshot.socket");
    let output = BangbangProcess::run_with_extra_args_expect_successful_exit(
        &describe_socket,
        &test_dir.instance_id(),
        &["--describe-snapshot", path_text(&snapshot_path)],
    );
    assert_eq!(output.stdout, expected);
    assert_eq!(output.stderr, "");
    assert!(!describe_socket.exists());
}

#[test]
fn executable_rejects_unreadable_non_regular_and_oversized_snapshot_files() {
    let test_dir = TestDir::new();

    assert_snapshot_describe_failure(
        &test_dir,
        "missing",
        &test_dir.path().join("private-missing.vmstate"),
        "failed to read snapshot state file: NotFound",
    );

    let directory_path = test_dir.path().join("private-directory.vmstate");
    fs::create_dir(&directory_path).expect("snapshot fixture directory should be created");
    assert_snapshot_describe_failure(
        &test_dir,
        "directory",
        &directory_path,
        "snapshot state file must be a regular file",
    );

    let oversized_path = test_dir.path().join("private-oversized.vmstate");
    let oversized_file =
        fs::File::create(&oversized_path).expect("oversized snapshot fixture should be created");
    oversized_file
        .set_len(
            u64::try_from(NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES)
                .expect("snapshot file limit should fit u64")
                + 1,
        )
        .expect("oversized snapshot fixture should be sized");
    assert_snapshot_describe_failure(
        &test_dir,
        "oversized",
        &oversized_path,
        "snapshot state file exceeds",
    );
}

#[test]
fn executable_rejects_malformed_corrupt_and_incompatible_snapshot_files() {
    let test_dir = TestDir::new();
    let valid =
        encode_snapshot_envelope(b"private-guest-state").expect("snapshot fixture should encode");

    let mut invalid_magic = valid.clone();
    invalid_magic[0] ^= 0xff;

    let truncated = valid[..valid.len() - 1].to_vec();

    let mut trailing = valid.clone();
    trailing.push(0);

    let mut inconsistent = valid.clone();
    inconsistent[24..32].copy_from_slice(&0_u64.to_le_bytes());

    let mut corrupt = valid.clone();
    corrupt[SNAPSHOT_ENVELOPE_HEADER_BYTES] ^= 0xff;

    let mut overflow = valid.clone();
    overflow[24..32].copy_from_slice(&u64::MAX.to_le_bytes());

    let unsupported_version = snapshot_fixture_with_u16(10, 1);
    let incompatible_architecture = snapshot_fixture_with_u16(14, 2);
    let incompatible_page_size = snapshot_fixture_with_u32(16, 16_384);

    for (name, bytes, expected) in [
        (
            "invalid-magic",
            invalid_magic,
            "snapshot envelope magic is invalid",
        ),
        ("truncated", truncated, "snapshot envelope is truncated"),
        ("trailing", trailing, "snapshot envelope has trailing data"),
        (
            "inconsistent-length",
            inconsistent,
            "snapshot envelope has trailing data",
        ),
        (
            "corrupt",
            corrupt,
            "snapshot envelope CRC-64/Jones integrity check failed",
        ),
        (
            "overflow",
            overflow,
            "snapshot envelope payload length overflows",
        ),
        (
            "unsupported-version",
            unsupported_version,
            "snapshot format version 1.1.0 is unsupported",
        ),
        (
            "incompatible-architecture",
            incompatible_architecture,
            "snapshot architecture identifier 2 is incompatible",
        ),
        (
            "incompatible-page-size",
            incompatible_page_size,
            "snapshot guest page size 16384 is incompatible",
        ),
    ] {
        let snapshot_path = test_dir
            .path()
            .join(format!("private-{name}-snapshot.vmstate"));
        fs::write(&snapshot_path, bytes).expect("snapshot fixture should be written");
        assert_snapshot_describe_failure(&test_dir, name, &snapshot_path, expected);
    }
}

#[test]
fn executable_rejects_invalid_logger_level_as_bad_configuration() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--level", "verbose"],
    );

    assert_bad_configuration_exit_code(&output, "invalid logger level");
    assert!(
        output
            .stderr
            .contains("bangbang: invalid --level: logger level is invalid"),
        "stderr should describe invalid logger level; stderr:\n{}",
        output.stderr
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "invalid logger level must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "invalid logger level must fail before publishing the API socket"
    );
}

#[test]
fn executable_rejects_snapshot_requests_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let logger_path = test_dir.path().join("snapshot-requests.log");
    let create_state_path = test_dir.path().join("secret-create.vmstate");
    let create_memory_path = test_dir.path().join("secret-create.mem");
    let load_state_path = test_dir.path().join("secret-load.vmstate");
    let load_memory_path = test_dir.path().join("secret-load.mem");
    let load_vsock_path = test_dir.path().join("secret-load.vsock");
    let load_iface_id = "secret-load-iface";
    let load_host_dev_name = "secret-load-host-device";
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let logger_body = format!(
        r#"{{"log_path":{},"level":"Info","module":"bangbang_runtime::api_server"}}"#,
        json_string(path_text(&logger_path))
    );
    let logger_response = http_put_json(&socket_path, "/logger", &logger_body);
    assert_no_content_response(&logger_response, "PUT /logger before snapshot requests");

    let create_body = format!(
        r#"{{"snapshot_path":{},"mem_file_path":{}}}"#,
        json_string(path_text(&create_state_path)),
        json_string(path_text(&create_memory_path))
    );
    let load_body = format!(
        r#"{{"snapshot_path":{},"mem_backend":{{"backend_path":{},"backend_type":"File"}},"network_overrides":[{{"iface_id":{},"host_dev_name":{}}}],"vsock_override":{{"uds_path":{}}}}}"#,
        json_string(path_text(&load_state_path)),
        json_string(path_text(&load_memory_path)),
        json_string(load_iface_id),
        json_string(load_host_dev_name),
        json_string(path_text(&load_vsock_path))
    );

    for (path, body, expected_fault, private_values) in [
        (
            "/snapshot/create",
            create_body,
            r#"{"fault_message":"The requested operation is not supported in Not started state: CreateSnapshot"}"#,
            vec![
                path_text(&create_state_path),
                path_text(&create_memory_path),
            ],
        ),
        (
            "/snapshot/load",
            load_body,
            r#"{"fault_message":"Snapshot and restore are not supported."}"#,
            vec![
                path_text(&load_state_path),
                path_text(&load_memory_path),
                load_iface_id,
                load_host_dev_name,
                path_text(&load_vsock_path),
            ],
        ),
    ] {
        let response = http_put_json(&socket_path, path, &body);

        assert_bad_request_response(&response, path);
        assert_response_contains(&response, expected_fault, path);
        for private_value in private_values {
            assert!(
                !response.contains(private_value),
                "{path} must not echo private snapshot path {private_value:?}; response:\n{response}"
            );
        }
    }

    for requested_path in [
        &create_state_path,
        &create_memory_path,
        &load_state_path,
        &load_memory_path,
        &load_vsock_path,
    ] {
        assert!(
            !requested_path.exists(),
            "rejected snapshot request must not create {}",
            requested_path.display()
        );
    }

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after rejected snapshots");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after rejected snapshots",
    );

    let logger_output =
        fs::read_to_string(&logger_path).expect("snapshot logger should be readable");
    assert!(logger_output.contains("\"/snapshot/create\""));
    assert!(logger_output.contains("\"/snapshot/load\""));
    for private_value in [
        path_text(&create_state_path),
        path_text(&create_memory_path),
        path_text(&load_state_path),
        path_text(&load_memory_path),
        load_iface_id,
        load_host_dev_name,
        path_text(&load_vsock_path),
    ] {
        assert!(
            !logger_output.contains(private_value),
            "snapshot request logging must not include {private_value:?}: {logger_output}"
        );
    }

    let output = bangbang.terminate();
    for private_value in [
        path_text(&create_state_path),
        path_text(&create_memory_path),
        path_text(&load_state_path),
        path_text(&load_memory_path),
        load_iface_id,
        load_host_dev_name,
        path_text(&load_vsock_path),
    ] {
        assert!(
            !output.stdout.contains(private_value) && !output.stderr.contains(private_value),
            "snapshot request output must not include {private_value:?}; stdout:\n{}\nstderr:\n{}",
            output.stdout,
            output.stderr
        );
    }
    assert_clean_shutdown(output, &socket_path, "bangbang");
}

#[test]
fn executable_handles_remaining_device_requests_and_pmem_config() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let pmem_response = http_put_json(
        &socket_path,
        "/pmem/pmem0",
        r#"{"id":"pmem0","path_on_host":"secret-pmem.img","read_only":true}"#,
    );
    assert_no_content_response(&pmem_response, "PUT /pmem/pmem0");
    let pmem_vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&pmem_vm_config, "GET /vm/config after PUT /pmem/pmem0");
    assert_response_contains(
        &pmem_vm_config,
        r#""pmem":[{"#,
        "GET /vm/config after PUT /pmem/pmem0",
    );
    assert_response_contains(
        &pmem_vm_config,
        r#""id":"pmem0""#,
        "GET /vm/config after PUT /pmem/pmem0",
    );
    assert_response_contains(
        &pmem_vm_config,
        r#""path_on_host":"secret-pmem.img""#,
        "GET /vm/config after PUT /pmem/pmem0",
    );
    assert_response_contains(
        &pmem_vm_config,
        r#""read_only":true"#,
        "GET /vm/config after PUT /pmem/pmem0",
    );

    let pmem_patch_response = http_json(
        &socket_path,
        "PATCH",
        "/pmem/pmem0",
        r#"{"id":"pmem0","rate_limiter":{"bandwidth":null,"ops":null}}"#,
    );
    assert_bad_request_response(&pmem_patch_response, "PATCH /pmem/pmem0");
    assert_response_contains(
        &pmem_patch_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: PatchPmem"}"#,
        "PATCH /pmem/pmem0",
    );

    let pmem_patch_rate_limiter_response = http_json(
        &socket_path,
        "PATCH",
        "/pmem/pmem0",
        r#"{"id":"pmem0","rate_limiter":{"ops":{"size":123456789,"one_time_burst":987654321,"refill_time":777}}}"#,
    );
    assert_bad_request_response(
        &pmem_patch_rate_limiter_response,
        "PATCH /pmem/pmem0 rate_limiter",
    );
    assert_response_contains(
        &pmem_patch_rate_limiter_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: PatchPmem"}"#,
        "PATCH /pmem/pmem0 rate_limiter",
    );
    for private_value in ["123456789", "987654321", "777"] {
        assert!(
            !pmem_patch_rate_limiter_response.contains(private_value),
            "PATCH /pmem/pmem0 rate_limiter must not echo private config value {private_value:?}; response:\n{pmem_patch_rate_limiter_response}"
        );
    }

    let pmem_rate_limiter_response = http_put_json(
        &socket_path,
        "/pmem/pmem0",
        r#"{"id":"pmem0","path_on_host":"secret-new-pmem.img","rate_limiter":{"ops":{"size":123456789,"one_time_burst":987654321,"refill_time":777}}}"#,
    );
    assert_bad_request_response(&pmem_rate_limiter_response, "PUT /pmem/pmem0 rate_limiter");
    assert_response_contains(
        &pmem_rate_limiter_response,
        r#"{"fault_message":"pmem rate_limiter is not supported"}"#,
        "PUT /pmem/pmem0 rate_limiter",
    );
    for private_value in ["secret-new-pmem.img", "123456789", "987654321", "777"] {
        assert!(
            !pmem_rate_limiter_response.contains(private_value),
            "PUT /pmem/pmem0 rate_limiter must not echo private config value {private_value:?}; response:\n{pmem_rate_limiter_response}"
        );
    }
    let pmem_vm_config_after_fault = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &pmem_vm_config_after_fault,
        "GET /vm/config after rejected PUT /pmem/pmem0 rate_limiter",
    );
    assert_response_contains(
        &pmem_vm_config_after_fault,
        r#""path_on_host":"secret-pmem.img""#,
        "GET /vm/config after rejected PUT /pmem/pmem0 rate_limiter",
    );
    assert!(
        !pmem_vm_config_after_fault.contains("secret-new-pmem.img"),
        "rejected pmem update must not replace stored path: {pmem_vm_config_after_fault}"
    );

    let pmem_root_device_response = http_put_json(
        &socket_path,
        "/pmem/pmem0",
        r#"{"id":"pmem0","path_on_host":"secret-root-pmem.img","root_device":true}"#,
    );
    assert_bad_request_response(&pmem_root_device_response, "PUT /pmem/pmem0 root_device");
    assert_response_contains(
        &pmem_root_device_response,
        r#"{"fault_message":"pmem root_device is not supported"}"#,
        "PUT /pmem/pmem0 root_device",
    );
    assert!(
        !pmem_root_device_response.contains("secret-root-pmem.img"),
        "PUT /pmem/pmem0 root_device must not echo rejected path; response:\n{pmem_root_device_response}"
    );
    let pmem_vm_config_after_root_device = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &pmem_vm_config_after_root_device,
        "GET /vm/config after rejected PUT /pmem/pmem0 root_device",
    );
    assert_response_contains(
        &pmem_vm_config_after_root_device,
        r#""path_on_host":"secret-pmem.img""#,
        "GET /vm/config after rejected PUT /pmem/pmem0 root_device",
    );
    assert!(
        !pmem_vm_config_after_root_device.contains("secret-root-pmem.img"),
        "rejected root-device pmem update must not replace stored path: {pmem_vm_config_after_root_device}"
    );

    let pmem_empty_path_response = http_put_json(
        &socket_path,
        "/pmem/pmem0",
        r#"{"id":"pmem0","path_on_host":""}"#,
    );
    assert_bad_request_response(&pmem_empty_path_response, "PUT /pmem/pmem0 empty path");
    assert_response_contains(
        &pmem_empty_path_response,
        r#"{"fault_message":"pmem path_on_host must not be empty"}"#,
        "PUT /pmem/pmem0 empty path",
    );
    let pmem_vm_config_after_empty_path = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &pmem_vm_config_after_empty_path,
        "GET /vm/config after rejected PUT /pmem/pmem0 empty path",
    );
    assert_response_contains(
        &pmem_vm_config_after_empty_path,
        r#""path_on_host":"secret-pmem.img""#,
        "GET /vm/config after rejected PUT /pmem/pmem0 empty path",
    );

    let unconfigured_memory_hotplug_get_response = http_get(&socket_path, "/hotplug/memory");
    assert_bad_request_response(
        &unconfigured_memory_hotplug_get_response,
        "GET /hotplug/memory before PUT",
    );
    assert_response_contains(
        &unconfigured_memory_hotplug_get_response,
        r#"{"fault_message":"Memory hotplug is not supported."}"#,
        "GET /hotplug/memory before PUT",
    );

    let memory_hotplug_put_response = http_put_json(
        &socket_path,
        "/hotplug/memory",
        r#"{"total_size_mib":2048}"#,
    );
    assert_no_content_response(&memory_hotplug_put_response, "PUT /hotplug/memory");

    let memory_hotplug_vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &memory_hotplug_vm_config,
        "GET /vm/config after PUT /hotplug/memory",
    );
    assert_response_contains(
        &memory_hotplug_vm_config,
        r#""memory-hotplug":{"block_size_mib":2,"slot_size_mib":128,"total_size_mib":2048}"#,
        "GET /vm/config after PUT /hotplug/memory",
    );

    let entropy_response = http_put_json(&socket_path, "/entropy", "{}");
    assert_no_content_response(&entropy_response, "PUT /entropy");
    let entropy_vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&entropy_vm_config, "GET /vm/config after PUT /entropy");
    assert_response_contains(
        &entropy_vm_config,
        r#""entropy":{}"#,
        "GET /vm/config after PUT /entropy",
    );

    let entropy_rate_limiter_response = http_put_json(
        &socket_path,
        "/entropy",
        r#"{"rate_limiter":{"bandwidth":{"size":123456789,"one_time_burst":987654321,"refill_time":777}}}"#,
    );
    assert_no_content_response(&entropy_rate_limiter_response, "PUT /entropy rate_limiter");
    let entropy_vm_config_after_limiter = http_get(&socket_path, "/vm/config");
    assert_ok_response(
        &entropy_vm_config_after_limiter,
        "GET /vm/config after PUT /entropy rate_limiter",
    );
    assert_response_contains(
        &entropy_vm_config_after_limiter,
        r#""bandwidth":{"one_time_burst":987654321,"refill_time":777,"size":123456789}"#,
        "GET /vm/config after PUT /entropy rate_limiter",
    );

    let memory_hotplug_get_response = http_get(&socket_path, "/hotplug/memory");
    assert_ok_response(&memory_hotplug_get_response, "GET /hotplug/memory");
    assert_response_contains(
        &memory_hotplug_get_response,
        r#"{"block_size_mib":2,"plugged_size_mib":0,"requested_size_mib":0,"slot_size_mib":128,"total_size_mib":2048}"#,
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
        r#"{"fault_message":"The requested operation is not supported in Not started state: PatchMemoryHotplug"}"#,
        "PATCH /hotplug/memory",
    );

    let balloon_get_response = http_get(&socket_path, "/balloon");
    assert_bad_request_response(&balloon_get_response, "GET /balloon");
    assert_response_contains(
        &balloon_get_response,
        r#"{"fault_message":"Balloon device is not supported."}"#,
        "GET /balloon",
    );

    let balloon_put_response = http_put_json(
        &socket_path,
        "/balloon",
        r#"{"amount_mib":64,"deflate_on_oom":true,"stats_polling_interval_s":60,"free_page_hinting":true,"free_page_reporting":false}"#,
    );
    assert_no_content_response(&balloon_put_response, "PUT /balloon");

    let configured_balloon_get_response = http_get(&socket_path, "/balloon");
    assert_ok_response(&configured_balloon_get_response, "GET /balloon configured");
    for expected in [
        r#""amount_mib":64"#,
        r#""deflate_on_oom":true"#,
        r#""stats_polling_interval_s":60"#,
        r#""free_page_hinting":true"#,
        r#""free_page_reporting":false"#,
    ] {
        assert_response_contains(
            &configured_balloon_get_response,
            expected,
            "GET /balloon configured",
        );
    }

    let balloon_vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&balloon_vm_config, "GET /vm/config after PUT /balloon");
    assert_response_contains(
        &balloon_vm_config,
        r#""balloon":"#,
        "GET /vm/config after PUT /balloon",
    );
    assert_response_contains(
        &balloon_vm_config,
        r#""amount_mib":64"#,
        "GET /vm/config after PUT /balloon",
    );
    assert_response_contains(
        &balloon_vm_config,
        r#""free_page_reporting":false"#,
        "GET /vm/config after PUT /balloon",
    );

    let balloon_reporting_response = http_put_json(
        &socket_path,
        "/balloon",
        r#"{"amount_mib":32,"deflate_on_oom":false,"free_page_reporting":true}"#,
    );
    assert_bad_request_response(
        &balloon_reporting_response,
        "PUT /balloon free_page_reporting",
    );
    assert_response_contains(
        &balloon_reporting_response,
        r#"{"fault_message":"balloon free_page_reporting is not supported"}"#,
        "PUT /balloon free_page_reporting",
    );
    let balloon_after_reporting = http_get(&socket_path, "/balloon");
    assert_ok_response(
        &balloon_after_reporting,
        "GET /balloon after rejected free_page_reporting",
    );
    assert_response_contains(
        &balloon_after_reporting,
        r#""amount_mib":64"#,
        "GET /balloon after rejected free_page_reporting",
    );
    assert_response_contains(
        &balloon_after_reporting,
        r#""free_page_reporting":false"#,
        "GET /balloon after rejected free_page_reporting",
    );

    for (path, action) in [
        ("/balloon/statistics", "GetBalloonStats"),
        ("/balloon/hinting/status", "GetBalloonHintingStatus"),
    ] {
        let request_name = format!("GET {path}");
        let response = http_get(&socket_path, path);

        assert_bad_request_response(&response, &request_name);
        assert_response_contains(
            &response,
            &format!(
                r#"{{"fault_message":"The requested operation is not supported in Not started state: {action}"}}"#
            ),
            &request_name,
        );
    }

    for (path, body, action) in [
        ("/balloon", r#"{"amount_mib":32}"#, "PatchBalloon"),
        (
            "/balloon/statistics",
            r#"{"stats_polling_interval_s":1}"#,
            "PatchBalloonStats",
        ),
        (
            "/balloon/hinting/start",
            r#"{"acknowledge_on_stop":false}"#,
            "PatchBalloonHintingStart",
        ),
    ] {
        let request_name = format!("PATCH {path}");
        let response = http_json(&socket_path, "PATCH", path, body);

        assert_bad_request_response(&response, &request_name);
        assert_response_contains(
            &response,
            &format!(
                r#"{{"fault_message":"The requested operation is not supported in Not started state: {action}"}}"#
            ),
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
        r#"{"fault_message":"The requested operation is not supported in Not started state: PatchBalloonHintingStop"}"#,
        "PATCH /balloon/hinting/stop",
    );

    let pmem_delete_response = http_no_body(&socket_path, "DELETE", "/pmem/pmem0");
    assert_bad_request_response(&pmem_delete_response, "DELETE /pmem/pmem0");
    assert_response_contains(
        &pmem_delete_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: HotUnplugDevice"}"#,
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
    assert_bad_configuration_exit_code(&output, "malformed config file");
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
    assert_bad_configuration_exit_code(&output, "malformed no-api config file");
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
fn executable_config_file_malformed_entropy_section_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = test_dir.path().join("vm-config.json");
    let instance_id = test_dir.instance_id();
    fs::write(
        &config_path,
        r#"{
            "boot-source":{"kernel_image_path":"/tmp/vmlinux"},
            "entropy":{"unknown":true}
        }"#,
    )
    .expect("config file should be written");

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert!(
        !output.status.success(),
        "malformed entropy config should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        !socket_path.exists(),
        "malformed entropy config should fail before API socket publication"
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "malformed entropy config must not report API readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        output
            .stderr
            .contains("bangbang: config-file error: invalid config-file section entropy: Malformed HTTP request."),
        "stderr should describe entropy section parse failure; stderr:\n{}",
        output.stderr
    );
}

#[test]
fn executable_config_file_entropy_rate_limiter_reaches_startup_failure_without_publishing_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = write_entropy_rate_limiter_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert_entropy_rate_limiter_startup_failure(
        &output,
        &socket_path,
        "config-file entropy rate limiter startup",
    );
}

#[test]
fn executable_no_api_config_file_entropy_rate_limiter_reaches_startup_failure_without_publishing_socket()
 {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = write_entropy_rate_limiter_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert_entropy_rate_limiter_startup_failure(
        &output,
        &socket_path,
        "no-api config-file entropy rate limiter startup",
    );
}

#[test]
fn executable_config_file_pmem_root_device_fails_before_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, pmem_path) = write_pmem_root_device_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert_pmem_root_device_startup_failure(
        &output,
        &socket_path,
        &pmem_path,
        "config-file pmem root-device startup",
    );
}

#[test]
fn executable_no_api_config_file_pmem_root_device_fails_before_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, pmem_path) = write_pmem_root_device_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert_pmem_root_device_startup_failure(
        &output,
        &socket_path,
        &pmem_path,
        "no-api config-file pmem root-device startup",
    );
}

#[test]
fn executable_config_file_balloon_free_page_reporting_fails_before_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = write_balloon_free_page_reporting_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert_balloon_free_page_reporting_startup_failure(
        &output,
        &socket_path,
        "config-file balloon free-page reporting startup",
    );
}

#[test]
fn executable_no_api_config_file_balloon_free_page_reporting_fails_before_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let config_path = write_balloon_free_page_reporting_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert_balloon_free_page_reporting_startup_failure(
        &output,
        &socket_path,
        "no-api config-file balloon free-page reporting startup",
    );
}

#[test]
fn executable_rejects_multi_vcpu_instance_start_without_stopping() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let kernel_path = test_dir.path().join("private-vmlinux");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let machine_response = http_put_json(
        &socket_path,
        "/machine-config",
        r#"{"vcpu_count":2,"mem_size_mib":256}"#,
    );
    assert_no_content_response(&machine_response, "PUT /machine-config multi-vCPU");

    let kernel_path_text = path_text(&kernel_path);
    let kernel_path_json = json_string(kernel_path_text);
    let boot_body = format!(r#"{{"kernel_image_path":{kernel_path_json}}}"#);
    let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
    assert_no_content_response(&boot_response, "PUT /boot-source multi-vCPU");

    let start_response = http_put_json(
        &socket_path,
        "/actions",
        r#"{"action_type":"InstanceStart"}"#,
    );
    assert_bad_request_response(&start_response, "PUT /actions multi-vCPU start");
    assert_response_contains(
        &start_response,
        MULTI_VCPU_STARTUP_ERROR,
        "PUT /actions multi-vCPU start",
    );
    assert!(
        !start_response.contains(kernel_path_text),
        "multi-vCPU startup rejection should not echo the private kernel path; response:\n{start_response}"
    );
    assert!(
        !kernel_path.exists(),
        "multi-vCPU startup rejection should happen before touching the kernel path"
    );

    let instance_info = http_get(&socket_path, "/");
    assert_ok_response(&instance_info, "GET / after rejected multi-vCPU start");
    assert_response_contains(
        &instance_info,
        r#""state":"Not started""#,
        "GET / after rejected multi-vCPU start",
    );

    let machine_config = http_get(&socket_path, "/machine-config");
    assert_ok_response(
        &machine_config,
        "GET /machine-config after rejected multi-vCPU start",
    );
    assert_response_contains(
        &machine_config,
        r#""vcpu_count":2"#,
        "GET /machine-config after rejected multi-vCPU start",
    );

    let output = bangbang.terminate();
    assert!(
        !output.stdout.contains(kernel_path_text),
        "multi-vCPU startup rejection should not write the private kernel path to stdout; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stderr.contains(kernel_path_text),
        "multi-vCPU startup rejection should not write the private kernel path to stderr; stderr:\n{}",
        output.stderr
    );
    assert_clean_shutdown(output, &socket_path, "bangbang");
}

#[test]
fn executable_config_file_multi_vcpu_startup_failure_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, kernel_path) = write_multi_vcpu_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path)],
    );

    assert_multi_vcpu_startup_failure(
        &output,
        &socket_path,
        &kernel_path,
        "config-file multi-vCPU startup failure",
    );
}

#[test]
fn executable_no_api_config_file_multi_vcpu_startup_failure_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, kernel_path) = write_multi_vcpu_startup_config(&test_dir);
    let instance_id = test_dir.instance_id();

    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &instance_id,
        &["--config-file", path_text(&config_path), "--no-api"],
    );

    assert_multi_vcpu_startup_failure(
        &output,
        &socket_path,
        &kernel_path,
        "no-api config-file multi-vCPU startup failure",
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
fn executable_config_file_malformed_serial_rate_limiter_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, serial_output_path) = write_malformed_serial_rate_limiter_config(&test_dir);
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
        "config-file malformed serial rate limiter",
    );
}

#[test]
fn executable_no_api_config_file_malformed_serial_rate_limiter_does_not_publish_socket() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let (config_path, serial_output_path) = write_malformed_serial_rate_limiter_config(&test_dir);
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
        "no-api config-file malformed serial rate limiter",
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
        r#"{"fault_message":"The requested operation is not supported in Not started state: HotUnplugDevice"}"#,
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
    assert_no_content_response(&cpu_config_response, "PUT /cpu-config empty");

    let empty_array_cpu_config_response = http_put_json(
        &socket_path,
        "/cpu-config",
        r#"{"kvm_capabilities":[],"reg_modifiers":[],"vcpu_features":[]}"#,
    );
    assert_no_content_response(
        &empty_array_cpu_config_response,
        "PUT /cpu-config empty arrays",
    );
    let instance_info_after_empty_array_cpu_config = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_empty_array_cpu_config,
        r#""state":"Not started""#,
        "GET / after empty array PUT /cpu-config",
    );

    let custom_cpu_config_response =
        http_put_json(&socket_path, "/cpu-config", r#"{"kvm_capabilities":["1"]}"#);
    assert_bad_request_response(&custom_cpu_config_response, "PUT /cpu-config custom");
    assert_response_contains(
        &custom_cpu_config_response,
        r#"{"fault_message":"The requested operation is not supported: PutCpuConfig"}"#,
        "PUT /cpu-config custom",
    );
    let instance_info_after_cpu_config = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_cpu_config,
        r#""state":"Not started""#,
        "GET / after custom PUT /cpu-config",
    );

    let malformed_cpu_config_response = http_put_json(&socket_path, "/cpu-config", "not-json");
    assert_bad_request_response(&malformed_cpu_config_response, "PUT /cpu-config malformed");
    assert_response_contains(
        &malformed_cpu_config_response,
        r#"{"fault_message":"Malformed HTTP request."}"#,
        "PUT /cpu-config malformed",
    );
    let instance_info_after_malformed_cpu_config = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_malformed_cpu_config,
        r#""state":"Not started""#,
        "GET / after malformed PUT /cpu-config",
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

    let vm_resume_response = http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Resumed"}"#);
    assert_bad_request_response(&vm_resume_response, "PATCH /vm resumed");
    assert_response_contains(
        &vm_resume_response,
        r#"{"fault_message":"The requested operation is not supported in Not started state: Resume"}"#,
        "PATCH /vm resumed",
    );
    let instance_info_after_vm_resume_patch = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_vm_resume_patch,
        r#""state":"Not started""#,
        "GET / after failed PATCH /vm resumed",
    );

    let malformed_vm_state_response =
        http_json(&socket_path, "PATCH", "/vm", r#"{"state":"Running"}"#);
    assert_bad_request_response(&malformed_vm_state_response, "PATCH /vm running");
    assert_response_contains(
        &malformed_vm_state_response,
        r#"{"fault_message":"Malformed HTTP request."}"#,
        "PATCH /vm running",
    );
    let instance_info_after_malformed_vm_state = http_get(&socket_path, "/");
    assert_response_contains(
        &instance_info_after_malformed_vm_state,
        r#""state":"Not started""#,
        "GET / after malformed PATCH /vm running",
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
fn executable_rejects_invalid_boot_source_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let original_kernel_path = test_dir.path().join("original-vmlinux");
    let rejected_kernel_path = test_dir.path().join("private-vmlinux");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let original_kernel_path_json = json_string(path_text(&original_kernel_path));
    let original_boot_args = "console=hvc0 reboot=k panic=1";
    let original_boot_args_json = json_string(original_boot_args);
    let original_body = format!(
        r#"{{"kernel_image_path":{original_kernel_path_json},"boot_args":{original_boot_args_json}}}"#
    );
    let original_response = http_put_json(&socket_path, "/boot-source", &original_body);
    assert_no_content_response(&original_response, "PUT /boot-source original");

    let rejected_kernel_path_text = path_text(&rejected_kernel_path);
    let rejected_kernel_path_json = json_string(rejected_kernel_path_text);
    let rejected_body = format!(
        r#"{{"kernel_image_path":{rejected_kernel_path_json},"boot_args":"secret\u0000debug"}}"#
    );
    let rejected_response = http_put_json(&socket_path, "/boot-source", &rejected_body);
    assert_bad_request_response(&rejected_response, "PUT /boot-source invalid");
    assert_response_contains(
        &rejected_response,
        r#"{"fault_message":"kernel command line is invalid: contains a NUL byte"}"#,
        "PUT /boot-source invalid",
    );
    assert!(
        !rejected_response.contains("secret")
            && !rejected_response.contains(rejected_kernel_path_text),
        "invalid boot-source response should not echo private request values; response:\n{rejected_response}"
    );

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config after invalid boot-source");
    assert_response_contains(
        &vm_config,
        &format!(r#""kernel_image_path":{original_kernel_path_json}"#),
        "GET /vm/config after invalid boot-source",
    );
    assert_response_contains(
        &vm_config,
        &format!(r#""boot_args":{original_boot_args_json}"#),
        "GET /vm/config after invalid boot-source",
    );
    assert!(
        !vm_config.contains(rejected_kernel_path_text) && !vm_config.contains("secret"),
        "GET /vm/config should retain the original boot-source after invalid update; response:\n{vm_config}"
    );

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
fn executable_logs_api_request_methods_and_paths_without_bodies() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let logger_path = test_dir.path().join("logger.out");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let logger_path_json = json_string(path_text(&logger_path));
    let logger_body = format!(
        r#"{{
            "log_path":{logger_path_json},
            "level":"Info",
            "module":"bangbang_runtime::api_server"
        }}"#
    );
    let logger_response = http_put_json(&socket_path, "/logger", &logger_body);
    assert_no_content_response(&logger_response, "PUT /logger");

    let version_response = http_get(&socket_path, "/version");
    assert_ok_response(&version_response, "GET /version after logger config");

    let private_value = "private-process-logger-secret";
    let mmds_body = format!(r#"{{"latest":{{"meta-data":{{"secret":"{private_value}"}}}}}}"#);
    let mmds_response = http_put_json(&socket_path, "/mmds", &mmds_body);
    assert_no_content_response(&mmds_response, "PUT /mmds after logger config");

    let logger_output = fs::read_to_string(&logger_path).expect("logger output should be readable");
    assert_eq!(
        logger_output,
        "The API server received a Get request on \"/version\".\nThe API server received a Put request on \"/mmds\".\n"
    );
    assert!(
        !logger_output.contains(private_value),
        "logger output must not include API request body values; output:\n{logger_output}"
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
fn executable_replaces_drive_without_reordering() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let rootfs_path_json = json_string(path_text(&test_dir.path().join("rootfs.ext4")));
    let data1_path_json = json_string(path_text(&test_dir.path().join("data1.ext4")));
    let data2_path_json = json_string(path_text(&test_dir.path().join("data2.ext4")));
    let replaced_data1_path_json =
        json_string(path_text(&test_dir.path().join("data1-replaced.ext4")));
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let put_drive = |drive_id: &str, path_json: &str, is_root_device: bool| {
        let body = format!(
            r#"{{
                "drive_id":"{drive_id}",
                "path_on_host":{path_json},
                "is_root_device":{is_root_device}
            }}"#
        );
        let path = format!("/drives/{drive_id}");
        let request_name = format!("PUT {path}");
        let response = http_put_json(&socket_path, &path, &body);
        assert_no_content_response(&response, &request_name);
    };

    put_drive("rootfs", &rootfs_path_json, true);
    put_drive("data1", &data1_path_json, false);
    put_drive("data2", &data2_path_json, false);
    put_drive("data1", &replaced_data1_path_json, false);

    let vm_config = http_get(&socket_path, "/vm/config");
    assert_ok_response(&vm_config, "GET /vm/config after replacing drive");
    assert_eq!(
        vm_config.matches(r#""drive_id":"#).count(),
        3,
        "drive replacement must not add a duplicate drive; response:\n{vm_config}"
    );
    assert_eq!(
        vm_config.matches(r#""drive_id":"data1""#).count(),
        1,
        "drive replacement must keep one data1 drive; response:\n{vm_config}"
    );
    assert_response_contains(
        &vm_config,
        &format!(r#""path_on_host":{replaced_data1_path_json}"#),
        "GET /vm/config after replacing drive",
    );
    assert!(
        !vm_config.contains(&format!(r#""path_on_host":{data1_path_json}"#)),
        "drive replacement must remove original data1 path; response:\n{vm_config}"
    );

    let rootfs_index = vm_config
        .find(r#""drive_id":"rootfs""#)
        .expect("vm config should include rootfs drive");
    let data1_index = vm_config
        .find(r#""drive_id":"data1""#)
        .expect("vm config should include data1 drive");
    let data2_index = vm_config
        .find(r#""drive_id":"data2""#)
        .expect("vm config should include data2 drive");
    assert!(
        rootfs_index < data1_index && data1_index < data2_index,
        "drive replacement must preserve ordering as rootfs, data1, data2; response:\n{vm_config}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_invalid_drive_configs_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let data_drive_path = test_dir.path().join("data.img");
    let root_drive_path = test_dir.path().join("rootfs.img");
    let mismatched_drive_path = test_dir.path().join("mismatched.img");
    let second_root_drive_path = test_dir.path().join("second-root.img");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let data_drive_path_json = json_string(path_text(&data_drive_path));
    let data_drive_body = format!(
        r#"{{
            "drive_id":"data",
            "path_on_host":{data_drive_path_json},
            "is_root_device":false,
            "is_read_only":false
        }}"#
    );
    let data_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
    assert_no_content_response(&data_response, "PUT /drives/data");

    let assert_accepted_drives =
        |context: &str, root_drive_path_json: Option<&str>, rejected: &[(&str, Option<&str>)]| {
            let vm_config = http_get(&socket_path, "/vm/config");
            assert_ok_response(&vm_config, context);
            assert_response_contains(&vm_config, r#""drive_id":"data""#, context);
            assert_response_contains(
                &vm_config,
                &format!(r#""path_on_host":{data_drive_path_json}"#),
                context,
            );

            let expected_drive_count = if let Some(root_drive_path_json) = root_drive_path_json {
                assert_response_contains(&vm_config, r#""drive_id":"rootfs""#, context);
                assert_response_contains(
                    &vm_config,
                    &format!(r#""path_on_host":{root_drive_path_json}"#),
                    context,
                );
                2
            } else {
                1
            };
            assert_eq!(
                vm_config.matches(r#""drive_id":"#).count(),
                expected_drive_count,
                "{context} must keep only accepted drives; response:\n{vm_config}"
            );

            for (drive_id, path_on_host) in rejected {
                assert!(
                    !vm_config.contains(&format!(r#""drive_id":"{drive_id}""#)),
                    "{context} must not store rejected drive {drive_id}; response:\n{vm_config}"
                );
                if let Some(path_on_host) = path_on_host {
                    assert!(
                        !vm_config.contains(&format!(r#""path_on_host":{path_on_host}"#)),
                        "{context} must not store rejected drive path; response:\n{vm_config}"
                    );
                }
            }
        };

    let mismatched_drive_path_text = path_text(&mismatched_drive_path);
    let mismatched_drive_path_json = json_string(mismatched_drive_path_text);
    let mismatched_body = format!(
        r#"{{
            "drive_id":"mismatched_body",
            "path_on_host":{mismatched_drive_path_json},
            "is_root_device":false
        }}"#
    );
    let mismatched_response =
        http_put_json(&socket_path, "/drives/mismatched_path", &mismatched_body);
    assert_bad_request_response(&mismatched_response, "PUT /drives/mismatched_path");
    assert_response_contains(
        &mismatched_response,
        r#"{"fault_message":"path drive_id must match body drive_id."}"#,
        "PUT /drives/mismatched_path",
    );
    assert!(
        !mismatched_response.contains("mismatched_body")
            && !mismatched_response.contains(mismatched_drive_path_text),
        "mismatched drive response must not echo rejected values; response:\n{mismatched_response}"
    );
    assert_accepted_drives(
        "GET /vm/config after mismatched drive_id",
        None,
        &[("mismatched_body", Some(&mismatched_drive_path_json))],
    );

    let empty_path_body = r#"{"drive_id":"empty_path","path_on_host":"","is_root_device":false}"#;
    let empty_path_response = http_put_json(&socket_path, "/drives/empty_path", empty_path_body);
    assert_bad_request_response(&empty_path_response, "PUT /drives/empty_path");
    assert_response_contains(
        &empty_path_response,
        r#"{"fault_message":"drive path_on_host must not be empty"}"#,
        "PUT /drives/empty_path",
    );
    assert_accepted_drives(
        "GET /vm/config after empty drive path",
        None,
        &[("empty_path", None)],
    );

    let root_drive_path_json = json_string(path_text(&root_drive_path));
    let root_drive_body = format!(
        r#"{{
            "drive_id":"rootfs",
            "path_on_host":{root_drive_path_json},
            "is_root_device":true,
            "is_read_only":true
        }}"#
    );
    let root_response = http_put_json(&socket_path, "/drives/rootfs", &root_drive_body);
    assert_no_content_response(&root_response, "PUT /drives/rootfs");

    let second_root_drive_path_json = json_string(path_text(&second_root_drive_path));
    let second_root_body = format!(
        r#"{{
            "drive_id":"second_root",
            "path_on_host":{second_root_drive_path_json},
            "is_root_device":true,
            "is_read_only":true
        }}"#
    );
    let second_root_response =
        http_put_json(&socket_path, "/drives/second_root", &second_root_body);
    assert_bad_request_response(&second_root_response, "PUT /drives/second_root");
    assert_response_contains(
        &second_root_response,
        r#"{"fault_message":"a root drive is already configured"}"#,
        "PUT /drives/second_root",
    );
    assert_accepted_drives(
        "GET /vm/config after rejected second root drive",
        Some(&root_drive_path_json),
        &[("second_root", Some(&second_root_drive_path_json))],
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
    assert_no_content_response(&rate_limiter_response, "PUT /drives/rate_limited");

    let assert_only_accepted_and_rate_limited_drives =
        |request_name: &str, rejected_drive_id: &str, rejected_path_json: Option<&str>| {
            let vm_config = http_get(&socket_path, "/vm/config");
            assert_ok_response(&vm_config, request_name);
            assert_response_contains(&vm_config, r#""drive_id":"accepted""#, request_name);
            assert_response_contains(
                &vm_config,
                &format!(r#""path_on_host":{accepted_drive_path_json}"#),
                request_name,
            );
            assert_response_contains(&vm_config, r#""drive_id":"rate_limited""#, request_name);
            assert_response_contains(
                &vm_config,
                &format!(r#""path_on_host":{rate_limited_drive_path_json}"#),
                request_name,
            );
            assert_response_contains(&vm_config, r#""rate_limiter":{"bandwidth":"#, request_name);
            assert_response_contains(&vm_config, r#""size":1000"#, request_name);
            assert_eq!(
                vm_config.matches(r#""drive_id":"#).count(),
                2,
                "{request_name} must keep only accepted drives; response:\n{vm_config}"
            );
            assert!(
                !vm_config.contains(&format!(r#""drive_id":"{rejected_drive_id}""#)),
                "{request_name} must not store rejected drive {rejected_drive_id}; response:\n{vm_config}"
            );
            if let Some(rejected_path_json) = rejected_path_json {
                assert!(
                    !vm_config.contains(&format!(r#""path_on_host":{rejected_path_json}"#)),
                    "{request_name} must not store rejected drive path; response:\n{vm_config}"
                );
            }
        };

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
    assert_only_accepted_and_rate_limited_drives(
        "GET /vm/config after rejected drive io_engine",
        "async",
        Some(&async_drive_path_json),
    );

    let private_socket_path_text = path_text(&private_socket_path);
    let private_socket_path_json = json_string(private_socket_path_text);
    let socket_body = format!(
        r#"{{
            "drive_id":"socket",
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
    assert_only_accepted_and_rate_limited_drives(
        "GET /vm/config after rejected drive socket",
        "socket",
        None,
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_invalid_network_interfaces_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let network_response = http_put_json(
        &socket_path,
        "/network-interfaces/eth0",
        r#"{"iface_id":"eth0","host_dev_name":"vmnet:shared","guest_mac":"12:34:56:78:9a:bc","mtu":1500}"#,
    );
    assert_no_content_response(&network_response, "PUT /network-interfaces/eth0");

    let assert_only_eth0 = |context: &str, rejected: &[(&str, Option<&str>, Option<&str>)]| {
        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, context);
        assert_response_contains(&vm_config, r#""iface_id":"eth0""#, context);
        assert_response_contains(&vm_config, r#""host_dev_name":"vmnet:shared""#, context);
        assert_response_contains(&vm_config, r#""guest_mac":"12:34:56:78:9a:bc""#, context);
        assert_response_contains(&vm_config, r#""mtu":1500"#, context);
        assert_eq!(
            vm_config.matches(r#""iface_id":"#).count(),
            1,
            "{context} must keep only the accepted network interface; response:\n{vm_config}"
        );

        for (iface_id, host_dev_name, guest_mac) in rejected {
            assert!(
                !vm_config.contains(&format!(r#""iface_id":"{iface_id}""#)),
                "{context} must not store rejected interface {iface_id}; response:\n{vm_config}"
            );
            if let Some(host_dev_name) = host_dev_name {
                assert!(
                    !vm_config.contains(&format!(r#""host_dev_name":"{host_dev_name}""#)),
                    "{context} must not store rejected host device; response:\n{vm_config}"
                );
            }
            if let Some(guest_mac) = guest_mac {
                assert!(
                    !vm_config.contains(&format!(r#""guest_mac":"{guest_mac}""#)),
                    "{context} must not store rejected guest MAC; response:\n{vm_config}"
                );
            }
        }
    };

    let mismatched_response = http_put_json(
        &socket_path,
        "/network-interfaces/mismatched_path",
        r#"{"iface_id":"mismatched_body","host_dev_name":"vmnet:mismatched","guest_mac":"02:00:00:00:00:01"}"#,
    );
    assert_bad_request_response(
        &mismatched_response,
        "PUT /network-interfaces/mismatched_path",
    );
    assert_response_contains(
        &mismatched_response,
        r#"{"fault_message":"path iface_id must match body iface_id."}"#,
        "PUT /network-interfaces/mismatched_path",
    );
    assert_only_eth0(
        "GET /vm/config after mismatched network iface_id",
        &[(
            "mismatched_body",
            Some("vmnet:mismatched"),
            Some("02:00:00:00:00:01"),
        )],
    );

    let empty_host_response = http_put_json(
        &socket_path,
        "/network-interfaces/empty_host",
        r#"{"iface_id":"empty_host","host_dev_name":"","guest_mac":"02:00:00:00:00:02"}"#,
    );
    assert_bad_request_response(&empty_host_response, "PUT /network-interfaces/empty_host");
    assert_response_contains(
        &empty_host_response,
        r#"{"fault_message":"network host_dev_name must not be empty"}"#,
        "PUT /network-interfaces/empty_host",
    );
    assert_only_eth0(
        "GET /vm/config after empty network host_dev_name",
        &[("empty_host", None, Some("02:00:00:00:00:02"))],
    );

    let rx_rate_limiter_response = http_put_json(
        &socket_path,
        "/network-interfaces/rx_limited",
        r#"{"iface_id":"rx_limited","host_dev_name":"vmnet:rx","guest_mac":"02:00:00:00:00:03","rx_rate_limiter":{"bandwidth":{"size":1000,"one_time_burst":1000,"refill_time":100}}}"#,
    );
    assert_bad_request_response(
        &rx_rate_limiter_response,
        "PUT /network-interfaces/rx_limited",
    );
    assert_response_contains(
        &rx_rate_limiter_response,
        r#"{"fault_message":"network rx_rate_limiter is not supported"}"#,
        "PUT /network-interfaces/rx_limited",
    );
    assert_only_eth0(
        "GET /vm/config after rejected rx rate limiter",
        &[("rx_limited", Some("vmnet:rx"), Some("02:00:00:00:00:03"))],
    );

    let tx_rate_limiter_response = http_put_json(
        &socket_path,
        "/network-interfaces/tx_limited",
        r#"{"iface_id":"tx_limited","host_dev_name":"vmnet:tx","guest_mac":"02:00:00:00:00:04","tx_rate_limiter":{"ops":{"size":100,"one_time_burst":100,"refill_time":1000}}}"#,
    );
    assert_bad_request_response(
        &tx_rate_limiter_response,
        "PUT /network-interfaces/tx_limited",
    );
    assert_response_contains(
        &tx_rate_limiter_response,
        r#"{"fault_message":"network tx_rate_limiter is not supported"}"#,
        "PUT /network-interfaces/tx_limited",
    );
    assert_only_eth0(
        "GET /vm/config after rejected tx rate limiter",
        &[("tx_limited", Some("vmnet:tx"), Some("02:00:00:00:00:04"))],
    );

    let duplicate_mac_response = http_put_json(
        &socket_path,
        "/network-interfaces/duplicate_mac",
        r#"{"iface_id":"duplicate_mac","host_dev_name":"vmnet:duplicate","guest_mac":"12:34:56:78:9a:bc"}"#,
    );
    assert_bad_request_response(
        &duplicate_mac_response,
        "PUT /network-interfaces/duplicate_mac",
    );
    assert_response_contains(
        &duplicate_mac_response,
        r#"{"fault_message":"network guest_mac is already in use"}"#,
        "PUT /network-interfaces/duplicate_mac",
    );
    assert_only_eth0(
        "GET /vm/config after duplicate network guest_mac",
        &[("duplicate_mac", Some("vmnet:duplicate"), None)],
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
        r#"{"fault_message":"The requested operation is not supported in Not started state: UpdateNetworkInterface"}"#,
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
        r#"{"fault_message":"The requested operation is not supported in Not started state: HotUnplugDevice"}"#,
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
fn executable_rejects_invalid_mmds_config_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

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

    let assert_original_mmds_config = |context: &str, rejected_interface: Option<&str>| {
        let vm_config = http_get(&socket_path, "/vm/config");
        assert_ok_response(&vm_config, context);
        assert_response_contains(&vm_config, r#""iface_id":"eth0""#, context);
        assert_response_contains(&vm_config, r#""host_dev_name":"vmnet:shared""#, context);
        assert_response_contains(&vm_config, r#""guest_mac":"12:34:56:78:9a:bc""#, context);
        assert_response_contains(&vm_config, r#""mtu":1500"#, context);
        assert_response_contains(&vm_config, r#""mmds-config":"#, context);
        assert_response_contains(&vm_config, r#""network_interfaces":["eth0"]"#, context);
        assert_response_contains(&vm_config, r#""version":"V2""#, context);
        assert_response_contains(&vm_config, r#""ipv4_address":"169.254.169.254""#, context);
        assert_response_contains(&vm_config, r#""imds_compat":true"#, context);
        assert!(
            !vm_config.contains(r#""network_interfaces":[]"#),
            "{context} must not replace MMDS config with an empty interface list; response:\n{vm_config}"
        );

        if let Some(rejected_interface) = rejected_interface {
            assert!(
                !vm_config.contains(&format!(r#""{rejected_interface}""#)),
                "{context} must not store rejected MMDS interface {rejected_interface}; response:\n{vm_config}"
            );
        }
    };

    let unknown_interface_response = http_put_json(
        &socket_path,
        "/mmds/config",
        r#"{"network_interfaces":["eth1"],"version":"V1","ipv4_address":"169.254.169.253","imds_compat":false}"#,
    );
    assert_bad_request_response(&unknown_interface_response, "PUT /mmds/config with eth1");
    assert_response_contains(
        &unknown_interface_response,
        r#"{"fault_message":"MMDS network interface id is not configured: eth1"}"#,
        "PUT /mmds/config with eth1",
    );
    assert_original_mmds_config("GET /vm/config after unknown MMDS interface", Some("eth1"));

    let empty_interface_list_response = http_put_json(
        &socket_path,
        "/mmds/config",
        r#"{"network_interfaces":[],"version":"V1","ipv4_address":"169.254.169.253","imds_compat":false}"#,
    );
    assert_bad_request_response(
        &empty_interface_list_response,
        "PUT /mmds/config with empty interface list",
    );
    assert_response_contains(
        &empty_interface_list_response,
        r#"{"fault_message":"MMDS network_interfaces must not be empty"}"#,
        "PUT /mmds/config with empty interface list",
    );
    assert_original_mmds_config("GET /vm/config after empty MMDS interface list", None);

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
fn executable_rejects_invalid_machine_config_put_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let put_response = http_put_json(
        &socket_path,
        "/machine-config",
        r#"{"vcpu_count":2,"mem_size_mib":256}"#,
    );
    assert_no_content_response(&put_response, "PUT /machine-config original");

    let invalid_put_response = http_put_json(
        &socket_path,
        "/machine-config",
        r#"{"vcpu_count":4,"mem_size_mib":512,"track_dirty_pages":true}"#,
    );
    assert_bad_request_response(&invalid_put_response, "PUT /machine-config invalid");
    assert_response_contains(
        &invalid_put_response,
        r#"{"fault_message":"machine track_dirty_pages is not supported"}"#,
        "PUT /machine-config invalid",
    );

    let machine_config = http_get(&socket_path, "/machine-config");
    assert_ok_response(&machine_config, "GET /machine-config after invalid PUT");
    assert_response_contains(
        &machine_config,
        r#""vcpu_count":2"#,
        "GET /machine-config after invalid PUT",
    );
    assert_response_contains(
        &machine_config,
        r#""mem_size_mib":256"#,
        "GET /machine-config after invalid PUT",
    );
    assert_response_contains(
        &machine_config,
        r#""smt":false"#,
        "GET /machine-config after invalid PUT",
    );
    assert_response_contains(
        &machine_config,
        r#""track_dirty_pages":false"#,
        "GET /machine-config after invalid PUT",
    );
    assert_response_contains(
        &machine_config,
        r#""huge_pages":"None""#,
        "GET /machine-config after invalid PUT",
    );
    assert!(
        !machine_config.contains(r#""vcpu_count":4"#)
            && !machine_config.contains(r#""mem_size_mib":512"#)
            && !machine_config.contains(r#""track_dirty_pages":true"#),
        "invalid PUT /machine-config must not mutate stored machine config; response:\n{machine_config}"
    );

    assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
}

#[test]
fn executable_rejects_unsupported_machine_config_options_without_mutating() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let put_response = http_put_json(
        &socket_path,
        "/machine-config",
        r#"{"vcpu_count":2,"mem_size_mib":256}"#,
    );
    assert_no_content_response(&put_response, "PUT /machine-config original");

    for (request_name, method, body, expected_fault) in [
        (
            "PUT /machine-config cpu_template",
            "PUT",
            r#"{"vcpu_count":4,"mem_size_mib":512,"cpu_template":"V1N1"}"#,
            r#"{"fault_message":"machine cpu_template V1N1 is not supported"}"#,
        ),
        (
            "PATCH /machine-config cpu_template",
            "PATCH",
            r#"{"mem_size_mib":512,"cpu_template":"T2A"}"#,
            r#"{"fault_message":"machine cpu_template T2A is not supported"}"#,
        ),
        (
            "PUT /machine-config smt",
            "PUT",
            r#"{"vcpu_count":4,"mem_size_mib":512,"smt":true}"#,
            r#"{"fault_message":"machine smt is not supported"}"#,
        ),
        (
            "PATCH /machine-config smt",
            "PATCH",
            r#"{"mem_size_mib":512,"smt":true}"#,
            r#"{"fault_message":"machine smt is not supported"}"#,
        ),
        (
            "PUT /machine-config huge_pages",
            "PUT",
            r#"{"vcpu_count":4,"mem_size_mib":512,"huge_pages":"2M"}"#,
            r#"{"fault_message":"machine huge_pages is not supported"}"#,
        ),
        (
            "PATCH /machine-config huge_pages",
            "PATCH",
            r#"{"mem_size_mib":512,"huge_pages":"2M"}"#,
            r#"{"fault_message":"machine huge_pages is not supported"}"#,
        ),
    ] {
        let response = if method == "PUT" {
            http_put_json(&socket_path, "/machine-config", body)
        } else {
            http_json(&socket_path, method, "/machine-config", body)
        };
        assert_bad_request_response(&response, request_name);
        assert_response_contains(&response, expected_fault, request_name);

        let machine_config = http_get(&socket_path, "/machine-config");
        let config_context = format!("GET /machine-config after {request_name}");
        assert_ok_response(&machine_config, &config_context);
        assert_response_contains(&machine_config, r#""vcpu_count":2"#, &config_context);
        assert_response_contains(&machine_config, r#""mem_size_mib":256"#, &config_context);
        assert_response_contains(&machine_config, r#""smt":false"#, &config_context);
        assert_response_contains(
            &machine_config,
            r#""track_dirty_pages":false"#,
            &config_context,
        );
        assert_response_contains(&machine_config, r#""huge_pages":"None""#, &config_context);
        assert!(
            !machine_config.contains(r#""vcpu_count":4"#)
                && !machine_config.contains(r#""mem_size_mib":512"#)
                && !machine_config.contains(r#""smt":true"#)
                && !machine_config.contains(r#""huge_pages":"2M""#),
            "{request_name} must not mutate stored machine config; response:\n{machine_config}"
        );
    }

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
fn executable_fails_when_api_socket_path_is_broken_symlink_without_removing_it() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let missing_target_path = test_dir.path().join("missing-api.socket");
    let instance_id = test_dir.instance_id();
    symlink(&missing_target_path, &socket_path).expect("fixture symlink should be created");

    let output = BangbangProcess::start_expect_failure(&socket_path, &instance_id);

    assert_eq!(
        output.status.code(),
        Some(1),
        "broken symlink API socket path should fail with process failure; stdout:\n{}\nstderr:\n{}",
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
    assert!(
        fs::symlink_metadata(&socket_path)
            .expect("broken symlink should remain")
            .file_type()
            .is_symlink(),
        "failed startup must leave the broken symlink at the API socket path"
    );
    assert_eq!(
        fs::read_link(&socket_path).expect("broken symlink target should remain readable"),
        missing_target_path,
        "failed startup must not retarget the existing API socket symlink"
    );

    fs::remove_file(socket_path).expect("fixture symlink should clean up");
}

#[test]
fn executable_live_api_socket_conflict_does_not_interrupt_owner() {
    let test_dir = TestDir::new();
    let socket_path = test_dir.path().join("api.socket");
    let instance_id = test_dir.instance_id();
    let owner_instance_id = format!("{instance_id}-owner");
    let conflicting_instance_id = format!("{instance_id}-conflict");
    let owner = BangbangProcess::start(&socket_path, &owner_instance_id);

    assert_instance_info_matches(
        &socket_path,
        &owner_instance_id,
        &conflicting_instance_id,
        "owner bangbang before bind conflict",
    );
    let original_metadata =
        fs::symlink_metadata(&socket_path).expect("owner API socket metadata should be readable");

    let output = BangbangProcess::start_expect_failure(&socket_path, &conflicting_instance_id);

    assert_eq!(
        output.status.code(),
        Some(1),
        "conflicting API socket path should fail with process failure; stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        output
            .stderr
            .contains("API server error: API socket path already exists"),
        "stderr should explain the live API socket bind failure; stderr:\n{}",
        output.stderr
    );
    assert!(
        !output.stdout.contains("status: API server listening"),
        "failed conflicting startup must not report API readiness; stdout:\n{}",
        output.stdout
    );
    let current_metadata =
        fs::symlink_metadata(&socket_path).expect("owner API socket should remain readable");
    assert_eq!(
        (current_metadata.dev(), current_metadata.ino()),
        (original_metadata.dev(), original_metadata.ino()),
        "failed conflicting startup must not replace the owner API socket"
    );
    assert_instance_info_matches(
        &socket_path,
        &owner_instance_id,
        &conflicting_instance_id,
        "owner bangbang after bind conflict",
    );

    assert_clean_shutdown(owner.terminate(), &socket_path, "owner bangbang");
}

#[test]
fn concurrent_executables_keep_api_resources_isolated() {
    let test_dir = TestDir::new();
    let first_socket_path = test_dir.path().join("first-api.socket");
    let second_socket_path = test_dir.path().join("second-api.socket");
    let first_metadata_path = test_dir.path().join("first-metadata.json");
    let second_metadata_path = test_dir.path().join("second-metadata.json");
    let first_metrics_path = test_dir.path().join("first-startup.metrics");
    let second_metrics_path = test_dir.path().join("second-startup.metrics");
    let instance_id = test_dir.instance_id();
    let first_instance_id = format!("{instance_id}-first");
    let second_instance_id = format!("{instance_id}-second");
    fs::write(
        &first_metadata_path,
        r#"{"latest":{"meta-data":{"ami-id":"ami-first"},"user-data":"first-user-data"}}"#,
    )
    .expect("first metadata file should be written");
    fs::write(
        &second_metadata_path,
        r#"{"latest":{"meta-data":{"ami-id":"ami-second"},"user-data":"second-user-data"}}"#,
    )
    .expect("second metadata file should be written");

    let future_start_time = u64::MAX.to_string();
    let first_bangbang = BangbangProcess::start_with_extra_args(
        &first_socket_path,
        &first_instance_id,
        &[
            "--metadata",
            path_text(&first_metadata_path),
            "--metrics-path",
            path_text(&first_metrics_path),
            "--start-time-us",
            future_start_time.as_str(),
            "--start-time-cpu-us",
            future_start_time.as_str(),
            "--parent-cpu-time-us",
            "1300",
        ],
    );
    let second_bangbang = BangbangProcess::start_with_extra_args(
        &second_socket_path,
        &second_instance_id,
        &[
            "--metadata",
            path_text(&second_metadata_path),
            "--metrics-path",
            path_text(&second_metrics_path),
            "--start-time-us",
            future_start_time.as_str(),
            "--start-time-cpu-us",
            future_start_time.as_str(),
            "--parent-cpu-time-us",
            "2300",
        ],
    );

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
    assert_mmds_data_matches(
        &first_socket_path,
        "ami-first",
        "first-user-data",
        "ami-second",
        "second-user-data",
        "first bangbang",
    );
    assert_mmds_data_matches(
        &second_socket_path,
        "ami-second",
        "second-user-data",
        "ami-first",
        "first-user-data",
        "second bangbang",
    );
    assert_startup_metrics_match(
        &first_metrics_path,
        &[
            r#""process_startup_time_us":0"#,
            r#""process_startup_time_cpu_us":1300"#,
        ],
        &[r#""process_startup_time_cpu_us":2300"#],
        "first bangbang",
    );
    assert_startup_metrics_match(
        &second_metrics_path,
        &[
            r#""process_startup_time_us":0"#,
            r#""process_startup_time_cpu_us":2300"#,
        ],
        &[r#""process_startup_time_cpu_us":1300"#],
        "second bangbang",
    );

    assert_clean_shutdown(
        first_bangbang.terminate(),
        &first_socket_path,
        "first bangbang",
    );
    assert!(
        second_socket_path.exists(),
        "first bangbang shutdown should not remove the second API socket"
    );
    assert_instance_info_matches(
        &second_socket_path,
        &second_instance_id,
        &first_instance_id,
        "second bangbang after first shutdown",
    );

    assert_clean_shutdown(
        second_bangbang.terminate(),
        &second_socket_path,
        "second bangbang",
    );
}

fn write_rejected_drive_socket_config(
    test_dir: &TestDir,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let config_path = test_dir.path().join("vm-config.json");
    let private_socket_path = test_dir.path().join("private-vhost.sock");
    let private_socket_path_json = json_string(path_text(&private_socket_path));
    let config = format!(
        r#"{{
            "boot-source": {{"kernel_image_path": "/tmp/vmlinux"}},
            "drives": [{{
                "drive_id": "rootfs",
                "is_root_device": true,
                "socket": {private_socket_path_json}
            }}]
        }}"#
    );
    fs::write(&config_path, config).expect("config file should be written");

    (config_path, private_socket_path)
}

fn write_malformed_serial_rate_limiter_config(
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
                "rate_limiter": {{"size": 1}}
            }}
        }}"#
    );
    fs::write(&config_path, config).expect("config file should be written");

    (config_path, serial_output_path)
}

fn write_entropy_rate_limiter_startup_config(test_dir: &TestDir) -> std::path::PathBuf {
    let config_path = test_dir.path().join("vm-config.json");
    fs::write(
        &config_path,
        r#"{
            "boot-source":{"kernel_image_path":"/tmp/vmlinux"},
            "entropy":{"rate_limiter":{"bandwidth":{"size":123456789,"one_time_burst":987654321,"refill_time":777}}}
        }"#,
    )
    .expect("config file should be written");

    config_path
}

fn write_pmem_root_device_startup_config(
    test_dir: &TestDir,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let config_path = test_dir.path().join("vm-config.json");
    let pmem_path = test_dir.path().join("private-root-pmem.img");
    let pmem_path_json = json_string(path_text(&pmem_path));
    let config = format!(
        r#"{{
            "boot-source": {{"kernel_image_path": "/tmp/vmlinux"}},
            "pmem": [
                {{"id": "pmem0", "path_on_host": "/tmp/pmem-old.img"}},
                {{"id": "pmem0", "path_on_host": {pmem_path_json}, "root_device": true}}
            ]
        }}"#
    );
    fs::write(&config_path, config).expect("config file should be written");

    (config_path, pmem_path)
}

fn write_balloon_free_page_reporting_startup_config(test_dir: &TestDir) -> std::path::PathBuf {
    let config_path = test_dir.path().join("vm-config.json");
    fs::write(
        &config_path,
        r#"{
            "boot-source":{"kernel_image_path":"/tmp/vmlinux"},
            "balloon":{"amount_mib":64,"deflate_on_oom":true,"free_page_reporting":true}
        }"#,
    )
    .expect("config file should be written");

    config_path
}

fn write_multi_vcpu_startup_config(test_dir: &TestDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let config_path = test_dir.path().join("vm-config.json");
    let kernel_path = test_dir.path().join("private-vmlinux");
    let kernel_path_json = json_string(path_text(&kernel_path));
    let config = format!(
        r#"{{
            "machine-config": {{"vcpu_count": 2, "mem_size_mib": 256}},
            "boot-source": {{"kernel_image_path": {kernel_path_json}}}
        }}"#
    );
    fs::write(&config_path, config).expect("multi-vCPU config file should be written");

    (config_path, kernel_path)
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
    assert_bad_configuration_exit_code(output, case_name);
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

fn assert_multi_vcpu_startup_failure(
    output: &support::CompletedProcess,
    socket_path: &std::path::Path,
    kernel_path: &std::path::Path,
    case_name: &str,
) {
    assert!(
        !output.status.success(),
        "{case_name} should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert_bad_configuration_exit_code(output, case_name);
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
            .contains("bangbang: config-file error: failed to apply config-file action: failed to start microVM"),
        "{case_name} stderr should describe startup action failure; stderr:\n{}",
        output.stderr
    );
    assert!(
        output.stderr.contains(MULTI_VCPU_STARTUP_ERROR),
        "{case_name} stderr should describe the HVF single-vCPU startup limit; stderr:\n{}",
        output.stderr
    );
    let kernel_path_text = path_text(kernel_path);
    assert!(
        !output.stdout.contains(kernel_path_text),
        "{case_name} stdout must not echo private kernel path; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stderr.contains(kernel_path_text),
        "{case_name} stderr must not echo private kernel path; stderr:\n{}",
        output.stderr
    );
    assert!(
        !kernel_path.exists(),
        "{case_name} should fail before touching the kernel path"
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
    assert_bad_configuration_exit_code(output, case_name);
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
    assert_bad_configuration_exit_code(output, case_name);
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
            "bangbang: config-file error: invalid config-file section serial: Malformed HTTP request."
        ),
        "{case_name} stderr should describe malformed serial config; stderr:\n{}",
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

fn assert_entropy_rate_limiter_startup_failure(
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
    assert_bad_configuration_exit_code(output, case_name);
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
            "bangbang: config-file error: failed to apply config-file action: failed to start microVM"
        ),
        "{case_name} stderr should describe config-file startup failure; stderr:\n{}",
        output.stderr
    );
    for private_value in ["123456789", "987654321", "777"] {
        assert!(
            !output.stdout.contains(private_value) && !output.stderr.contains(private_value),
            "{case_name} must not echo private config value {private_value}; stdout:\n{}\nstderr:\n{}",
            output.stdout,
            output.stderr
        );
    }
}

fn assert_pmem_root_device_startup_failure(
    output: &support::CompletedProcess,
    socket_path: &std::path::Path,
    pmem_path: &std::path::Path,
    case_name: &str,
) {
    assert!(
        !output.status.success(),
        "{case_name} should fail startup; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert_bad_configuration_exit_code(output, case_name);
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
            "bangbang: config-file error: failed to apply config-file action: pmem root_device is not supported"
        ),
        "{case_name} stderr should describe pmem root-device rejection; stderr:\n{}",
        output.stderr
    );
    let pmem_path_text = path_text(pmem_path);
    assert!(
        !output.stdout.contains(pmem_path_text),
        "{case_name} stdout must not echo private pmem path; stdout:\n{}",
        output.stdout
    );
    assert!(
        !output.stderr.contains(pmem_path_text),
        "{case_name} stderr must not echo private pmem path; stderr:\n{}",
        output.stderr
    );
    assert!(
        !pmem_path.exists(),
        "{case_name} must not create rejected pmem backing path"
    );
}

fn assert_balloon_free_page_reporting_startup_failure(
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
    assert_bad_configuration_exit_code(output, case_name);
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
            "bangbang: config-file error: failed to apply config-file action: balloon free_page_reporting is not supported"
        ),
        "{case_name} stderr should describe balloon free-page reporting rejection; stderr:\n{}",
        output.stderr
    );
}

fn assert_bad_configuration_exit_code(output: &support::CompletedProcess, case_name: &str) {
    assert_eq!(
        output.status.code(),
        Some(BAD_CONFIGURATION_EXIT_CODE),
        "{case_name} should fail with the bad-configuration exit code; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
}

fn assert_snapshot_describe_failure(
    test_dir: &TestDir,
    case_name: &str,
    snapshot_path: &std::path::Path,
    expected_error: &str,
) {
    let socket_path = test_dir.path().join(format!("{case_name}.socket"));
    let output = BangbangProcess::start_with_extra_args_expect_failure(
        &socket_path,
        &test_dir.instance_id(),
        &["--describe-snapshot", path_text(snapshot_path)],
    );

    assert_bad_configuration_exit_code(&output, case_name);
    assert!(
        output.stderr.contains(expected_error),
        "{case_name} should report {expected_error:?}; stderr:\n{}",
        output.stderr
    );
    assert!(
        !output.stdout.contains(path_text(snapshot_path))
            && !output.stderr.contains(path_text(snapshot_path)),
        "{case_name} must redact the snapshot path; stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        !output.stdout.contains("private-guest-state")
            && !output.stderr.contains("private-guest-state"),
        "{case_name} must redact snapshot payload bytes; stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        !output.stdout.contains("status: API server listening")
            && !output.stdout.contains("status: VM running without API"),
        "{case_name} must exit before startup readiness; stdout:\n{}",
        output.stdout
    );
    assert!(
        !socket_path.exists(),
        "{case_name} must not publish the API socket"
    );
}

fn snapshot_fixture_with_u16(offset: usize, value: u16) -> Vec<u8> {
    let mut encoded =
        encode_snapshot_envelope(b"private-guest-state").expect("snapshot fixture should encode");
    replace_snapshot_field_and_checksum(&mut encoded, offset, &value.to_le_bytes());
    encoded
}

fn snapshot_fixture_with_u32(offset: usize, value: u32) -> Vec<u8> {
    let mut encoded =
        encode_snapshot_envelope(b"private-guest-state").expect("snapshot fixture should encode");
    replace_snapshot_field_and_checksum(&mut encoded, offset, &value.to_le_bytes());
    encoded
}

fn replace_snapshot_field_and_checksum(encoded: &mut [u8], offset: usize, value: &[u8]) {
    encoded[offset..offset + value.len()].copy_from_slice(value);
    let checksum_offset = encoded.len() - SNAPSHOT_ENVELOPE_INTEGRITY_BYTES;
    let checksum = crc64(0, &encoded[..checksum_offset]);
    encoded[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
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

fn assert_mmds_data_matches(
    socket_path: &std::path::Path,
    expected_ami_id: &str,
    expected_user_data: &str,
    unexpected_ami_id: &str,
    unexpected_user_data: &str,
    process_name: &str,
) {
    let request_name = format!("GET /mmds for {process_name}");
    let response = http_get(socket_path, "/mmds");
    assert_ok_response(&response, &request_name);
    assert_response_contains(
        &response,
        &format!(r#""ami-id":"{expected_ami_id}""#),
        &request_name,
    );
    assert_response_contains(
        &response,
        &format!(r#""user-data":"{expected_user_data}""#),
        &request_name,
    );
    assert!(
        !response.contains(&format!(r#""ami-id":"{unexpected_ami_id}""#))
            && !response.contains(&format!(r#""user-data":"{unexpected_user_data}""#)),
        "{request_name} response should not contain another process MMDS data; response:\n{response}"
    );
}

fn assert_startup_metrics_match(
    metrics_path: &std::path::Path,
    expected_fragments: &[&str],
    unexpected_fragments: &[&str],
    process_name: &str,
) {
    let output = fs::read_to_string(metrics_path).unwrap_or_else(|err| {
        panic!(
            "{process_name} metrics output {} should be readable: {err}",
            metrics_path.display()
        )
    });
    for expected in expected_fragments {
        assert!(
            output.contains(expected),
            "{process_name} metrics output should contain {expected:?}; output:\n{output}"
        );
    }
    for unexpected in unexpected_fragments {
        assert!(
            !output.contains(unexpected),
            "{process_name} metrics output should not contain another process value {unexpected:?}; output:\n{output}"
        );
    }
}
