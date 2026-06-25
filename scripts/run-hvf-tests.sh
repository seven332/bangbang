#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "bangbang-hvf tests require macOS Apple Silicon; found $(uname -s) $(uname -m)" >&2
  exit 1
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bangbang-hvf-tests.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

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
cargo test -p bangbang-hvf --test hvf_lifecycle --all-features --locked --no-run --message-format=json > "$cargo_messages"

test_bins=()
while IFS= read -r test_bin; do
  if [[ -n "$test_bin" ]]; then
    test_bins+=("$test_bin")
  fi
done < <(sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' "$cargo_messages")

if [[ "${#test_bins[@]}" -eq 0 ]]; then
  echo "failed to locate bangbang-hvf lifecycle test executable" >&2
  exit 1
fi

for test_bin in "${test_bins[@]}"; do
  codesign --force --sign - --entitlements "$entitlements" "$test_bin"
  "$test_bin" --test-threads=1 "$@"
done
