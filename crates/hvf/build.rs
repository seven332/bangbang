use std::path::{Path, PathBuf};
use std::process::{self, Command};

const SIMD_FP_SHIM_SOURCE: &str = "native/simd_fp.c";
const AVAILABILITY_SHIM_SOURCE: &str = "native/availability.c";
const MACH_LAZY_FAULT_SHIM_SOURCE: &str = "native/mach_lazy_fault.c";
const MACH_LAZY_FAULT_TEST_SUPPORT_SOURCE: &str = "native/mach_lazy_fault_test_support.c";

fn main() {
    println!("cargo::rerun-if-changed={SIMD_FP_SHIM_SOURCE}");
    println!("cargo::rerun-if-changed={AVAILABILITY_SHIM_SOURCE}");
    println!("cargo::rerun-if-changed={MACH_LAZY_FAULT_SHIM_SOURCE}");
    println!("cargo::rerun-if-changed={MACH_LAZY_FAULT_TEST_SUPPORT_SOURCE}");
    println!("cargo::rerun-if-env-changed=DEVELOPER_DIR");
    println!("cargo::rerun-if-env-changed=SDKROOT");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS");
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH");
    if target_os.as_deref() != Ok("macos") || target_arch.as_deref() != Ok("aarch64") {
        return;
    }

    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        fail("OUT_DIR is unavailable for public mach_exc generation");
    };
    let sdk_root = macos_sdk_root();
    let defs = sdk_root.join("usr/include/mach/mach_exc.defs");
    let user_source = out_dir.join("bangbang_mach_exc_user.c");
    let server_source = out_dir.join("bangbang_mach_exc_server.c");
    let user_header = out_dir.join("bangbang_mach_exc_user.h");
    let server_header = out_dir.join("bangbang_mach_exc_server.h");

    let mut mig = Command::new("xcrun");
    mig.args(["--sdk", "macosx", "mig", "-arch", "arm64"])
        .arg("-user")
        .arg(&user_source)
        .arg("-server")
        .arg(&server_source)
        .arg("-header")
        .arg(&user_header)
        .arg("-sheader")
        .arg(&server_header)
        .arg(&defs);
    let Ok(status) = mig.status() else {
        fail("failed to execute public macOS MIG tool");
    };
    if !status.success() {
        fail("public macOS mach_exc generation failed");
    }

    cc::Build::new()
        .file(SIMD_FP_SHIM_SOURCE)
        .file(AVAILABILITY_SHIM_SOURCE)
        .file(MACH_LAZY_FAULT_SHIM_SOURCE)
        .file(MACH_LAZY_FAULT_TEST_SUPPORT_SOURCE)
        .file(user_source)
        .file(server_source)
        .include(out_dir)
        .std("c11")
        .warnings(true)
        .warnings_into_errors(true)
        .compile("bangbang_hvf_simd_fp");
}

fn macos_sdk_root() -> PathBuf {
    let Ok(output) = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
    else {
        fail("failed to execute xcrun for the public macOS SDK");
    };
    if !output.status.success() {
        fail("xcrun could not locate the public macOS SDK");
    }
    let Ok(path) = std::str::from_utf8(&output.stdout) else {
        fail("xcrun returned a non-UTF-8 macOS SDK path");
    };
    let path = Path::new(path.trim());
    if path.as_os_str().is_empty() {
        fail("xcrun returned an empty macOS SDK path");
    }
    path.to_path_buf()
}

fn fail(message: &str) -> ! {
    eprintln!("bangbang-hvf build error: {message}");
    process::exit(1);
}
