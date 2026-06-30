#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/run-guest-boot-smoke.sh [--allow-unsupported] [-- TEST_ARGS...]

Prepare the pinned Firecracker kernel and generated initrd, build and sign the
bangbang-hvf guest boot smoke integration test, then run it when the host
supports Hypervisor.framework VM execution.

Options:
  --allow-unsupported  Exit 0 instead of 1 when the host cannot execute HVF.
  -h, --help           Show this help.

Arguments after -- are passed to the signed Rust test binary, except
--test-threads because the wrapper always runs HVF tests with one test thread.
EOF
}

allow_unsupported=false
test_args=()

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --allow-unsupported)
      allow_unsupported=true
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

if [[ "${#test_args[@]}" -gt 0 ]]; then
  for test_arg in "${test_args[@]}"; do
    case "$test_arg" in
      --test-threads | --test-threads=*)
        echo "scripts/run-guest-boot-smoke.sh controls --test-threads and always uses 1" >&2
        exit 2
        ;;
    esac
  done
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bangbang-guest-boot-smoke.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

finish_unsupported() {
  local message="$1"

  if [[ "$allow_unsupported" == true ]]; then
    echo "$message; skipping signed guest boot smoke test"
    exit 0
  fi

  echo "$message" >&2
  exit 1
}

host_os="$(uname -s)"
host_arch="$(uname -m)"

if [[ "$host_os" != "Darwin" ]]; then
  finish_unsupported "guest boot smoke tests require macOS; found $host_os $host_arch"
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to prepare guest boot smoke artifacts" >&2
  exit 1
fi

if ! command -v codesign >/dev/null 2>&1; then
  echo "codesign is required to sign guest boot smoke tests" >&2
  exit 1
fi

kernel_path="$(scripts/fetch-firecracker-kernel.sh)"
initrd_path="$(scripts/build-guest-boot-initrd.py --check)"

target_triple="aarch64-apple-darwin"

entitlements="$tmp_dir/hvf-entitlements.plist"
cat > "$entitlements" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.hypervisor</key>
  <true/>
</dict>
</plist>
EOF

cargo_messages="$tmp_dir/cargo-test.json"
cargo test \
  -p bangbang-hvf \
  --test guest_boot_smoke \
  --all-features \
  --locked \
  --target "$target_triple" \
  --no-run \
  --message-format=json \
  > "$cargo_messages"

test_bins_file="$tmp_dir/test-bins"
python3 - "$cargo_messages" > "$test_bins_file" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        message = json.loads(line)
        target = message.get("target", {})
        executable = message.get("executable")

        if (
            message.get("reason") == "compiler-artifact"
            and executable is not None
            and target.get("name") == "guest_boot_smoke"
            and "test" in target.get("kind", [])
        ):
            sys.stdout.write(executable)
            sys.stdout.write("\0")
PY

test_bins=()
while IFS= read -r -d "" test_bin; do
  if [[ -n "$test_bin" ]]; then
    test_bins+=("$test_bin")
  fi
done < "$test_bins_file"

if [[ "${#test_bins[@]}" -eq 0 ]]; then
  echo "failed to locate bangbang-hvf guest boot smoke test executable" >&2
  exit 1
fi

signed_test_bins=()
for index in "${!test_bins[@]}"; do
  test_bin="${test_bins[$index]}"
  signed_test_bin="$tmp_dir/$(basename "$test_bin").$index"
  cp "$test_bin" "$signed_test_bin"
  codesign --force --sign - --entitlements "$entitlements" "$signed_test_bin"
  signed_test_bins+=("$signed_test_bin")
done

if [[ "$host_arch" != "arm64" ]]; then
  finish_unsupported "guest boot smoke tests require Apple Silicon; found $host_os $host_arch"
fi

hv_support="$(sysctl -n kern.hv_support 2>/dev/null || sysctl -n kern.hv.supported 2>/dev/null || true)"
if [[ "$hv_support" != "1" ]]; then
  finish_unsupported "Hypervisor.framework is not supported by this host"
fi

hv_disable="$(sysctl -n kern.hv_disable 2>/dev/null || true)"
if [[ "$hv_disable" == "1" ]]; then
  finish_unsupported "Hypervisor.framework is disabled on this host"
fi

for test_bin in "${signed_test_bins[@]}"; do
  if [[ "${#test_args[@]}" -eq 0 ]]; then
    BANGBANG_GUEST_KERNEL_PATH="$kernel_path" \
      BANGBANG_GUEST_INITRD_PATH="$initrd_path" \
      "$test_bin" --test-threads=1
  else
    BANGBANG_GUEST_KERNEL_PATH="$kernel_path" \
      BANGBANG_GUEST_INITRD_PATH="$initrd_path" \
      "$test_bin" --test-threads=1 "${test_args[@]}"
  fi
done
