#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/run-signed-process-tests.sh

Build and sign the bangbang executable, then run process-level e2e tests
against that signed executable. This script does not start HVF or send
InstanceStart.

Options:
  -h, --help  Show this help.
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

if [[ "$#" -ne 0 ]]; then
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tmp_root="$repo_root/.tmp"
mkdir -p -- "$tmp_root"
signed_dir="$(mktemp -d "$tmp_root/signed-process-e2e.XXXXXX")"
trap 'rm -rf -- "$signed_dir"' EXIT

signed_bangbang="$signed_dir/bangbang"
scripts/build-signed-bangbang.sh --output "$signed_bangbang"

BANGBANG_PROCESS_E2E_BIN="$signed_bangbang" \
  cargo test -p bangbang --test process_e2e --all-features --locked
