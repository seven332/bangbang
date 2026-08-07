#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/fetch-firecracker-kernel.sh

Fetch and verify the pinned Firecracker arm64 Linux kernel artifact.

Environment:
  BANGBANG_GUEST_ARTIFACTS_DIR  Override the guest artifact cache root.
EOF
}

if [[ "$#" -gt 0 ]]; then
  case "$1" in
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
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to fetch guest artifacts" >&2
  exit 1
fi

exec python3 "$repo_root/scripts/guest_artifact_policy.py" fetch kernel
