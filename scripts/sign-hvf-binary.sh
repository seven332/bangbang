#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/sign-hvf-binary.sh INPUT OUTPUT

Copy INPUT to OUTPUT and sign OUTPUT with the Hypervisor.framework entitlement.

Arguments:
  INPUT   Existing binary to copy and sign.
  OUTPUT  Destination path for the signed binary.
EOF
}

if [[ "$#" -eq 1 ]]; then
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
  esac
fi

if [[ "$#" -ne 2 ]]; then
  usage >&2
  exit 2
fi

input="$1"
output="$2"

if [[ ! -f "$input" ]]; then
  echo "input binary does not exist or is not a regular file: $input" >&2
  exit 1
fi

if [[ -z "$output" ]]; then
  echo "output path must not be empty" >&2
  exit 2
fi

case "$output" in
  */)
    echo "output path must name a file: $output" >&2
    exit 2
    ;;
esac

if [[ -d "$output" ]]; then
  echo "output path must not be an existing directory: $output" >&2
  exit 2
fi

if ! command -v codesign >/dev/null 2>&1; then
  echo "codesign is required to sign HVF binaries" >&2
  exit 1
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bangbang-hvf-sign.XXXXXX")"
signed_tmp=""

cleanup() {
  if [[ -n "$signed_tmp" ]]; then
    rm -f -- "$signed_tmp"
  fi
  rm -rf -- "$tmp_dir"
}
trap cleanup EXIT

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

output_dir="$(dirname -- "$output")"
output_name="$(basename -- "$output")"

if [[ "$output_name" == "." || "$output_name" == "/" ]]; then
  echo "output path must name a file: $output" >&2
  exit 2
fi

mkdir -p -- "$output_dir"
case "$output_dir" in
  /*)
    output_tmp_dir="$output_dir"
    ;;
  *)
    output_tmp_dir="./$output_dir"
    ;;
esac
signed_tmp="$(mktemp "$output_tmp_dir/.$output_name.signed.XXXXXX")"
cp -p -- "$input" "$signed_tmp"
codesign --force --sign - --entitlements "$entitlements" "$signed_tmp"
mv -f -- "$signed_tmp" "$output"
signed_tmp=""
