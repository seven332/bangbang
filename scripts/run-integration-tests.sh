#!/usr/bin/env bash
set -euo pipefail

supported_tests=(hvf_lifecycle guest_boot)

usage() {
  cat <<'EOF'
Usage: scripts/run-integration-tests.sh [--allow-unsupported] [--test NAME]... [-- TEST_ARGS...]

Build and sign bangbang integration tests that require Hypervisor.framework
entitlements, then run them when the host supports HVF execution.

Options:
  --allow-unsupported  Exit 0 instead of 1 when the host cannot execute HVF.
  --test NAME          Run one integration test target. Can be repeated.
                       Supported values: hvf_lifecycle, guest_boot.
  -h, --help           Show this help.

Arguments after -- are passed to each signed Rust test binary, except
--test-threads because the wrapper always runs integration tests with one test
thread.
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

build_and_sign_test() {
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

  local test_bins=()
  local test_bin
  while IFS= read -r -d "" test_bin; do
    if [[ -n "$test_bin" ]]; then
      test_bins+=("$test_bin")
    fi
  done < "$test_bins_file"

  if [[ "${#test_bins[@]}" -eq 0 ]]; then
    echo "failed to locate bangbang-hvf integration test executable: $test_name" >&2
    exit 1
  fi

  local index
  for index in "${!test_bins[@]}"; do
    test_bin="${test_bins[$index]}"
    signed_test_bin="$tmp_dir/$(basename "$test_bin").$index"
    scripts/sign-hvf-binary.sh "$test_bin" "$signed_test_bin"
    signed_test_names+=("$test_name")
    signed_test_bins+=("$signed_test_bin")
  done
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
if contains guest_boot "${selected_tests[@]}"; then
  guest_kernel_path="$(scripts/fetch-firecracker-kernel.sh)"
  guest_initrd_path="$(scripts/build-guest-boot-initrd.py --check)"
  guest_rootfs_path="$(scripts/fetch-firecracker-rootfs.sh)"
fi

target_triple="aarch64-apple-darwin"

signed_test_names=()
signed_test_bins=()

for test_name in "${selected_tests[@]}"; do
  build_and_sign_test "$test_name"
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

if contains guest_boot "${selected_tests[@]}"; then
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
