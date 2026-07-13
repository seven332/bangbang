#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled once per test target"
)]
mod support;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};

use support::{
    BangbangProcess, CompletedProcess, assert_clean_shutdown, assert_ok_response,
    assert_response_contains, http_get,
};

const BANGBANG_PROCESS_E2E_BIN_ENV: &str = "BANGBANG_PROCESS_E2E_BIN";
const BUNDLE_IDENTIFIER: &str = "dev.bangbang.sandbox";
const BAD_CONFIGURATION_EXIT_CODE: i32 = 152;
const PROCESS_FAILURE_EXIT_CODE: i32 = 1;
static INITIALIZE_CONTAINER: Once = Once::new();
static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

fn sandboxed_bangbang() -> PathBuf {
    let path = std::env::var_os(BANGBANG_PROCESS_E2E_BIN_ENV)
        .expect("signed runner should provide the sandboxed bangbang executable");
    assert!(
        !path.is_empty(),
        "sandboxed bangbang executable path must not be empty"
    );

    PathBuf::from(path)
}

fn run_help() -> CompletedProcess {
    BangbangProcess::run_with_args_expect_exit(&[OsStr::new("--help")], "sandboxed bangbang help")
}

fn container_tmp_dir() -> PathBuf {
    INITIALIZE_CONTAINER.call_once(|| {
        let output = run_help();
        assert!(
            output.status.success(),
            "sandboxed help should initialize the app container; status: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            output.stdout,
            output.stderr
        );
    });

    let home =
        PathBuf::from(std::env::var_os("HOME").expect("sandbox integration tests require HOME"));
    let path = home
        .join("Library/Containers")
        .join(BUNDLE_IDENTIFIER)
        .join("Data/tmp");
    fs::create_dir_all(&path).expect("app container temporary directory should be available");

    path
}

fn unique_socket_path(name: &str) -> PathBuf {
    let id = NEXT_SOCKET_ID.fetch_add(1, Ordering::SeqCst);
    container_tmp_dir().join(format!("bb-{}-{id}-{name}.sock", std::process::id()))
}

#[test]
fn sandboxed_bundle_prints_help() {
    let output = run_help();

    assert!(
        output.status.success(),
        "sandboxed help should succeed; status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        output.stdout.contains("Usage:\n  bangbang [OPTIONS]"),
        "sandboxed help should print usage"
    );
    assert_eq!(output.stderr, "");
}

#[test]
fn sandbox_denies_default_tmp_api_socket_without_leaking_path() {
    let output = BangbangProcess::run_with_args_expect_exit(&[], "default API socket denial");
    let stdout = output.stdout;
    let stderr = output.stderr;

    assert_eq!(output.status.code(), Some(PROCESS_FAILURE_EXIT_CODE));
    assert!(
        stderr.contains("API server error: failed to bind API socket: PermissionDenied"),
        "sandboxed default socket failure should retain the error class; stderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("/tmp/bangbang.socket") && !stderr.contains("/tmp/bangbang.socket"),
        "sandboxed default socket failure must not echo the denied path"
    );
    assert!(
        !stdout.contains("status: API server listening"),
        "denied default socket must not publish readiness"
    );
}

#[test]
fn sandbox_denies_outside_config_without_leaking_path() {
    let denied_config = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = BangbangProcess::run_with_args_expect_exit(
        &[
            OsStr::new("--config-file"),
            denied_config.as_os_str(),
            OsStr::new("--no-api"),
        ],
        "outside config denial",
    );
    let stdout = output.stdout;
    let stderr = output.stderr;
    let denied_path = denied_config
        .to_str()
        .expect("manifest path should be valid UTF-8");

    assert_eq!(output.status.code(), Some(BAD_CONFIGURATION_EXIT_CODE));
    assert!(
        stderr.contains("config-file error: failed to read config file: PermissionDenied"),
        "sandboxed config denial should retain the error class; stderr:\n{stderr}"
    );
    assert!(
        !stdout.contains(denied_path) && !stderr.contains(denied_path),
        "sandboxed config denial must not echo the denied path"
    );
    assert!(
        !stdout.contains("status: VM running without API"),
        "denied config must not publish readiness"
    );
}

#[test]
fn sandbox_serves_and_cleans_container_api_socket() {
    let socket_path = unique_socket_path("served");
    let instance_id = format!("sandbox-process-{}", std::process::id());
    let bangbang = BangbangProcess::start(&socket_path, &instance_id);

    let response = http_get(&socket_path, "/");
    assert_ok_response(&response, "sandboxed GET /");
    assert_response_contains(&response, r#""state":"Not started""#, "sandboxed GET /");

    assert_clean_shutdown(
        bangbang.interrupt(),
        &socket_path,
        "sandboxed bangbang SIGINT",
    );
}

#[test]
fn sandbox_bundle_identity_matches_test_contract() {
    let executable = sandboxed_bangbang();
    let components = executable
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect::<Vec<OsString>>();

    assert!(
        components
            .windows(3)
            .any(|window| window == ["Contents", "MacOS", "bangbang"]),
        "sandboxed executable must be launched from a real app bundle: {}",
        executable.display()
    );
}
