#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-signed-bangbang.sh --output PATH

Build the bangbang executable for aarch64-apple-darwin, copy it to PATH, and
sign PATH with the Hypervisor.framework entitlement. This script does not run
the signed executable.

Options:
  --output PATH  Destination path for the signed bangbang executable.
  -h, --help     Show this help.
EOF
}

output_path=""

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --output)
      shift
      if [[ "$#" -eq 0 ]]; then
        echo "--output requires a path" >&2
        usage >&2
        exit 2
      fi
      output_path="$1"
      ;;
    --output=*)
      output_path="${1#--output=}"
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ -z "$output_path" ]]; then
  echo "--output is required" >&2
  usage >&2
  exit 2
fi

if ! command -v codesign >/dev/null 2>&1; then
  echo "codesign is required to sign the bangbang executable" >&2
  exit 1
fi

initial_dir="$(pwd)"
case "$output_path" in
  /*)
    signed_output="$output_path"
    ;;
  *)
    signed_output="$initial_dir/$output_path"
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_triple="aarch64-apple-darwin"
cargo build \
  -p bangbang \
  --all-features \
  --locked \
  --target "$target_triple"

unsigned_bangbang="$repo_root/target/$target_triple/debug/bangbang"
scripts/sign-hvf-binary.sh "$unsigned_bangbang" "$signed_output"

printf '%s\n' "$signed_output"
