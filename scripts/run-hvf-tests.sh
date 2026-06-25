#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/run-hvf-tests.sh [--allow-unsupported] [-- TEST_ARGS...]

Build and sign the bangbang-hvf lifecycle integration test, then run it when
the host supports Hypervisor.framework VM creation.

Options:
  --allow-unsupported  Exit 0 instead of 1 when the host cannot run HVF tests.
  -h, --help           Show this help.

Any arguments after -- are passed to the signed Rust test binary.
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
      test_args+=("$1")
      ;;
  esac
  shift
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bangbang-hvf-tests.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

finish_unsupported() {
  local message="$1"

  if [[ "$allow_unsupported" == true ]]; then
    echo "$message; skipping signed HVF lifecycle test"
    exit 0
  fi

  echo "$message" >&2
  exit 1
}

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  finish_unsupported "bangbang-hvf tests require macOS Apple Silicon; found $(uname -s) $(uname -m)"
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to parse cargo test JSON output" >&2
  exit 1
fi

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
  --test hvf_lifecycle \
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
            and target.get("name") == "hvf_lifecycle"
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
  echo "failed to locate bangbang-hvf lifecycle test executable" >&2
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
    "$test_bin" --test-threads=1
  else
    "$test_bin" --test-threads=1 "${test_args[@]}"
  fi
done
