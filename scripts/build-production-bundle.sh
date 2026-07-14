#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-production-bundle.sh --output PATH [--signing-identity IDENTITY]

Build the Apple Silicon production launcher and sandbox worker, then publish the
fixed Bangbang.app bundle without replacing an existing destination.

Options:
  --output PATH                 Absent destination named Bangbang.app.
  --signing-identity IDENTITY   One identity for both code objects (default: -).
  -h, --help                    Show this help.
EOF
}

output=""
output_set=false
signing_identity="-"
signing_identity_set=false

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --output)
      if [[ "$output_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--output requires a path" >&2
        usage >&2
        exit 2
      fi
      output="$1"
      output_set=true
      ;;
    --output=*)
      if [[ "$output_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      output="${1#--output=}"
      output_set=true
      ;;
    --signing-identity)
      if [[ "$signing_identity_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--signing-identity requires a non-empty value" >&2
        usage >&2
        exit 2
      fi
      signing_identity="$1"
      signing_identity_set=true
      ;;
    --signing-identity=*)
      if [[ "$signing_identity_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      signing_identity="${1#--signing-identity=}"
      if [[ -z "$signing_identity" ]]; then
        echo "--signing-identity requires a non-empty value" >&2
        usage >&2
        exit 2
      fi
      signing_identity_set=true
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ -z "$output" ]]; then
  echo "--output is required" >&2
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_triple="aarch64-apple-darwin"
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

"$repo_root/target/release/bangbang-bundle" \
  --launcher "$repo_root/target/$target_triple/release/bangbang-launcher" \
  --worker "$repo_root/target/$target_triple/release/bangbang" \
  --output "$output" \
  --signing-identity "$signing_identity"
