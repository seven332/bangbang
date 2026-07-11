const SIMD_FP_SHIM_SOURCE: &str = "native/simd_fp.c";

fn main() {
    println!("cargo::rerun-if-changed={SIMD_FP_SHIM_SOURCE}");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS");
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH");
    if target_os.as_deref() != Ok("macos") || target_arch.as_deref() != Ok("aarch64") {
        return;
    }

    cc::Build::new()
        .file(SIMD_FP_SHIM_SOURCE)
        .std("c11")
        .warnings(true)
        .warnings_into_errors(true)
        .compile("bangbang_hvf_simd_fp");
}
