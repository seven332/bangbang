#!/usr/bin/env bash
set -euo pipefail

supported_tests=(hvf_lifecycle guest_boot executable_hvf_e2e app_sandbox production_bundle)

usage() {
  cat <<'EOF'
Usage: scripts/run-integration-tests.sh [--allow-unsupported] [--test NAME]... [-- TEST_ARGS...]

Build and sign bangbang integration test artifacts that require
Hypervisor.framework entitlements, then run them when the host supports HVF
execution.

Options:
  --allow-unsupported  Exit 0 instead of 1 when the host cannot execute HVF.
  --test NAME          Run one integration test target. Can be repeated.
                       Supported values: hvf_lifecycle, guest_boot,
                       executable_hvf_e2e, app_sandbox, production_bundle.
  -h, --help           Show this help.

Arguments after -- are passed to each signed Rust test binary or executable
e2e cargo test invocation, except --test-threads because the wrapper always
runs integration tests with one test thread.
EOF
}

contains() {
  local needle="$1"
  shift

  for item in "$@"; do
    if [[ "$item" == "$needle" ]]; then
      return 0
    fi
  done

  return 1
}

add_selected_test() {
  local test_name="$1"

  if [[ -z "$test_name" ]]; then
    echo "--test requires a non-empty test target name" >&2
    exit 2
  fi

  if ! contains "$test_name" "${supported_tests[@]}"; then
    echo "unsupported integration test target: $test_name" >&2
    usage >&2
    exit 2
  fi

  if [[ "${#selected_tests[@]}" -gt 0 ]] && contains "$test_name" "${selected_tests[@]}"; then
    return
  fi

  selected_tests+=("$test_name")
}

allow_unsupported=false
selected_tests=()
test_args=()

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --allow-unsupported)
      allow_unsupported=true
      ;;
    --test)
      shift
      if [[ "$#" -eq 0 ]]; then
        echo "--test requires a test target name" >&2
        usage >&2
        exit 2
      fi
      add_selected_test "$1"
      ;;
    --test=*)
      add_selected_test "${1#--test=}"
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    --)
      shift
      test_args+=("$@")
      break
      ;;
    *)
      echo "unknown argument before --: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ "${#selected_tests[@]}" -eq 0 ]]; then
  selected_tests=("${supported_tests[@]}")
fi

if [[ "${#test_args[@]}" -gt 0 ]]; then
  for test_arg in "${test_args[@]}"; do
    case "$test_arg" in
      --test-threads | --test-threads=*)
        echo "scripts/run-integration-tests.sh controls --test-threads and always uses 1" >&2
        exit 2
        ;;
    esac
  done
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bangbang-integration-tests.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

finish_unsupported() {
  local message="$1"

  if [[ "$allow_unsupported" == true ]]; then
    echo "$message; skipping signed integration tests"
    exit 0
  fi

  echo "$message" >&2
  exit 1
}

build_test_executables() {
  local test_name="$1"
  local cargo_messages="$tmp_dir/cargo-test-$test_name.json"
  local test_bins_file="$tmp_dir/test-bins-$test_name"

  cargo test \
    -p bangbang-hvf \
    --test "$test_name" \
    --all-features \
    --locked \
    --target "$target_triple" \
    --no-run \
    --message-format=json \
    > "$cargo_messages"

  python3 - "$cargo_messages" "$test_name" > "$test_bins_file" <<'PY'
import json
import sys

target_name = sys.argv[2]

with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        message = json.loads(line)
        target = message.get("target", {})
        executable = message.get("executable")

        if (
            message.get("reason") == "compiler-artifact"
            and executable is not None
            and target.get("name") == target_name
            and "test" in target.get("kind", [])
        ):
            sys.stdout.write(executable)
            sys.stdout.write("\0")
PY

  built_test_bins=()
  local test_bin
  while IFS= read -r -d "" test_bin; do
    if [[ -n "$test_bin" ]]; then
      built_test_bins+=("$test_bin")
    fi
  done < "$test_bins_file"

  if [[ "${#built_test_bins[@]}" -eq 0 ]]; then
    echo "failed to locate bangbang-hvf integration test executable: $test_name" >&2
    exit 1
  fi
}

build_and_sign_test() {
  local test_name="$1"
  build_test_executables "$test_name"

  local test_bin
  local index
  for index in "${!built_test_bins[@]}"; do
    test_bin="${built_test_bins[$index]}"
    signed_test_bin="$tmp_dir/$(basename "$test_bin").$index"
    scripts/sign-hvf-binary.sh "$test_bin" "$signed_test_bin"
    signed_test_names+=("$test_name")
    signed_test_bins+=("$signed_test_bin")
  done
}

build_app_sandbox_tests() {
  build_test_executables hvf_lifecycle
  if [[ "${#built_test_bins[@]}" -ne 1 ]]; then
    echo "App Sandbox validation requires exactly one hvf_lifecycle executable" >&2
    exit 1
  fi

  app_sandbox_hvf_bin="$(scripts/sign-app-sandbox-bundle.sh \
    "${built_test_bins[0]}" \
    "$tmp_dir/BangbangHvfLifecycleSandbox.app" \
    scripts/app-sandbox/hvf-lifecycle-Info.plist)"

  cargo build \
    -p bangbang \
    --all-features \
    --locked \
    --target "$target_triple"

  app_sandbox_bangbang_bin="$(scripts/sign-app-sandbox-bundle.sh \
    "$repo_root/target/$target_triple/debug/bangbang" \
    "$tmp_dir/BangbangProcessSandbox.app" \
    scripts/app-sandbox/bangbang-Info.plist)"

  cargo test \
    -p bangbang \
    --test app_sandbox_process_e2e \
    --all-features \
    --locked \
    --target "$target_triple" \
    --no-run
}

build_production_bundle_tests() {
  if [[ -z "$guest_kernel_path" || -z "$guest_initrd_path" ]]; then
    echo "production bundle validation requires guest kernel and initrd fixtures" >&2
    exit 1
  fi

  cargo build \
    -p bangbang \
    -p bangbang-launcher \
    --bin bangbang \
    --bin bangbang-launcher \
    --release \
    --all-features \
    --locked \
    --target "$target_triple"

  cargo build \
    -p bangbang-launcher \
    --bin bangbang-bundle \
    --release \
    --locked

  production_bundle_path="$tmp_dir/Bangbang.app"
  production_resources="$tmp_dir/production-bundle-resources"
  mkdir -p "$production_resources"
  cp -p -- "$guest_kernel_path" "$production_resources/guest-kernel"
  cp -p -- "$guest_initrd_path" "$production_resources/guest-initrd"

  production_worker_resources="$production_bundle_path/Contents/Helpers/BangbangWorker.app/Contents/Resources"
  python3 - \
    "$production_worker_resources/guest-kernel" \
    "$production_worker_resources/guest-initrd" \
    "$production_resources/vm-config.json" <<'PY'
import json
import sys

kernel_path, initrd_path, output_path = sys.argv[1:]
config = {
    "machine-config": {"vcpu_count": 1, "mem_size_mib": 256},
    "boot-source": {
        "kernel_image_path": kernel_path,
        "initrd_path": initrd_path,
        "boot_args": "console=ttyS0 reboot=k panic=1 rdinit=/poweroff-init",
    },
}
with open(output_path, "w", encoding="utf-8") as output:
    json.dump(config, output, separators=(",", ":"))
PY

  "$repo_root/target/release/bangbang-bundle" \
    --launcher "$repo_root/target/$target_triple/release/bangbang-launcher" \
    --worker "$repo_root/target/$target_triple/release/bangbang" \
    --output "$production_bundle_path" \
    --signing-identity - \
    --test-worker-resources "$production_resources"

  cargo test \
    -p bangbang-launcher \
    --test production_bundle_e2e \
    --all-features \
    --locked \
    --target "$target_triple" \
    --no-run
}

build_executable_hvf_e2e() {
  executable_hvf_e2e_bangbang="$tmp_dir/bangbang"
  scripts/build-signed-bangbang.sh --output "$executable_hvf_e2e_bangbang"

  cargo test \
    -p bangbang \
    --test executable_hvf_e2e \
    --all-features \
    --locked \
    --target "$target_triple" \
    --no-run
}

host_os="$(uname -s)"
host_arch="$(uname -m)"

if [[ "$host_os" != "Darwin" ]]; then
  finish_unsupported "signed integration tests require macOS; found $host_os $host_arch"
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to prepare and run signed integration tests" >&2
  exit 1
fi

if ! command -v codesign >/dev/null 2>&1; then
  echo "codesign is required to sign integration tests" >&2
  exit 1
fi

guest_kernel_path=""
guest_initrd_path=""
guest_rootfs_path=""
guest_ext4_rootfs_path=""
if contains guest_boot "${selected_tests[@]}" \
  || contains executable_hvf_e2e "${selected_tests[@]}" \
  || contains production_bundle "${selected_tests[@]}"; then
  guest_kernel_path="$(scripts/fetch-firecracker-kernel.sh)"
  guest_initrd_path="$(scripts/build-guest-boot-initrd.py --check)"
fi
if contains guest_boot "${selected_tests[@]}"; then
  guest_rootfs_path="$(scripts/fetch-firecracker-rootfs.sh)"
fi

target_triple="aarch64-apple-darwin"

built_test_bins=()
signed_test_names=()
signed_test_bins=()
executable_hvf_e2e_bangbang=""
app_sandbox_hvf_bin=""
app_sandbox_bangbang_bin=""
production_bundle_path=""

for test_name in "${selected_tests[@]}"; do
  case "$test_name" in
    app_sandbox)
      build_app_sandbox_tests
      ;;
    production_bundle)
      build_production_bundle_tests
      ;;
    executable_hvf_e2e)
      build_executable_hvf_e2e
      ;;
    guest_boot | hvf_lifecycle)
      build_and_sign_test "$test_name"
      ;;
    *)
      echo "unsupported integration test target after selection: $test_name" >&2
      exit 1
      ;;
  esac
done

if [[ "$host_arch" != "arm64" ]]; then
  finish_unsupported "signed integration tests require Apple Silicon; found $host_os $host_arch"
fi

hv_support="$(sysctl -n kern.hv_support 2>/dev/null || sysctl -n kern.hv.supported 2>/dev/null || true)"
if [[ "$hv_support" != "1" ]]; then
  finish_unsupported "Hypervisor.framework is not supported by this host"
fi

hv_disable="$(sysctl -n kern.hv_disable 2>/dev/null || true)"
if [[ "$hv_disable" == "1" ]]; then
  finish_unsupported "Hypervisor.framework is disabled on this host"
fi

if contains guest_boot "${selected_tests[@]}" || contains executable_hvf_e2e "${selected_tests[@]}"; then
  guest_ext4_rootfs_path="$(scripts/fetch-firecracker-rootfs.sh \
    --format ext4 \
    --ext4-size 512M \
    --direct-boot-init)"
fi

for index in "${!signed_test_bins[@]}"; do
  test_name="${signed_test_names[$index]}"
  test_bin="${signed_test_bins[$index]}"

  case "$test_name" in
    guest_boot)
      if [[ "${#test_args[@]}" -eq 0 ]]; then
        BANGBANG_GUEST_KERNEL_PATH="$guest_kernel_path" \
          BANGBANG_GUEST_INITRD_PATH="$guest_initrd_path" \
          BANGBANG_GUEST_ROOTFS_PATH="$guest_rootfs_path" \
          BANGBANG_GUEST_EXT4_ROOTFS_PATH="$guest_ext4_rootfs_path" \
          "$test_bin" --test-threads=1
      else
        BANGBANG_GUEST_KERNEL_PATH="$guest_kernel_path" \
          BANGBANG_GUEST_INITRD_PATH="$guest_initrd_path" \
          BANGBANG_GUEST_ROOTFS_PATH="$guest_rootfs_path" \
          BANGBANG_GUEST_EXT4_ROOTFS_PATH="$guest_ext4_rootfs_path" \
          "$test_bin" --test-threads=1 "${test_args[@]}"
      fi
      ;;
    hvf_lifecycle)
      if [[ "${#test_args[@]}" -eq 0 ]]; then
        "$test_bin" --test-threads=1
      else
        "$test_bin" --test-threads=1 "${test_args[@]}"
      fi
      ;;
    *)
      echo "unsupported signed integration test target after build: $test_name" >&2
      exit 1
      ;;
  esac
done

if contains app_sandbox "${selected_tests[@]}"; then
  if [[ "${#test_args[@]}" -eq 0 ]]; then
    "$app_sandbox_hvf_bin" --test-threads=1
  else
    "$app_sandbox_hvf_bin" --test-threads=1 "${test_args[@]}"
  fi

  if [[ "${#test_args[@]}" -eq 0 ]]; then
    BANGBANG_PROCESS_E2E_BIN="$app_sandbox_bangbang_bin" \
      cargo test \
        -p bangbang \
        --test app_sandbox_process_e2e \
        --all-features \
        --locked \
        --target "$target_triple" \
        -- \
        --test-threads=1
  else
    BANGBANG_PROCESS_E2E_BIN="$app_sandbox_bangbang_bin" \
      cargo test \
        -p bangbang \
        --test app_sandbox_process_e2e \
        --all-features \
        --locked \
        --target "$target_triple" \
        -- \
        --test-threads=1 \
        "${test_args[@]}"
  fi
fi

if contains production_bundle "${selected_tests[@]}"; then
  if [[ "${#test_args[@]}" -eq 0 ]]; then
    BANGBANG_PRODUCTION_BUNDLE_PATH="$production_bundle_path" \
      cargo test \
        -p bangbang-launcher \
        --test production_bundle_e2e \
        --all-features \
        --locked \
        --target "$target_triple" \
        -- \
        --test-threads=1
  else
    BANGBANG_PRODUCTION_BUNDLE_PATH="$production_bundle_path" \
      cargo test \
        -p bangbang-launcher \
        --test production_bundle_e2e \
        --all-features \
        --locked \
        --target "$target_triple" \
        -- \
        --test-threads=1 \
        "${test_args[@]}"
  fi
fi

if contains executable_hvf_e2e "${selected_tests[@]}"; then
  if [[ "${#test_args[@]}" -eq 0 ]]; then
    BANGBANG_PROCESS_E2E_BIN="$executable_hvf_e2e_bangbang" \
      BANGBANG_GUEST_KERNEL_PATH="$guest_kernel_path" \
      BANGBANG_GUEST_INITRD_PATH="$guest_initrd_path" \
      BANGBANG_GUEST_EXT4_ROOTFS_PATH="$guest_ext4_rootfs_path" \
      cargo test \
        -p bangbang \
        --test executable_hvf_e2e \
        --all-features \
        --locked \
        --target "$target_triple" \
        -- \
        --test-threads=1
  else
    BANGBANG_PROCESS_E2E_BIN="$executable_hvf_e2e_bangbang" \
      BANGBANG_GUEST_KERNEL_PATH="$guest_kernel_path" \
      BANGBANG_GUEST_INITRD_PATH="$guest_initrd_path" \
      BANGBANG_GUEST_EXT4_ROOTFS_PATH="$guest_ext4_rootfs_path" \
      cargo test \
        -p bangbang \
        --test executable_hvf_e2e \
        --all-features \
        --locked \
        --target "$target_triple" \
        -- \
        --test-threads=1 \
        "${test_args[@]}"
  fi
fi
