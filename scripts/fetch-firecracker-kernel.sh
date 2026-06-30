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

firecracker_minor="v1.15"
kernel_arch="aarch64"
kernel_version="6.1.155"
kernel_sha256="e3544b10603acbf3db492cb52e000d22ba202cb4b63b9add027565683e11c591"
kernel_url="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/${firecracker_minor}/${kernel_arch}/vmlinux-${kernel_version}"

cache_root="${BANGBANG_GUEST_ARTIFACTS_DIR:-$repo_root/.tmp/guest-artifacts}"
target_dir="${cache_root}/firecracker-ci/${firecracker_minor}/${kernel_arch}"
target_path="${target_dir}/vmlinux-${kernel_version}"
tmp_file=""

cleanup() {
  if [[ -n "$tmp_file" && -e "$tmp_file" ]]; then
    rm -f "$tmp_file"
  fi
}
trap cleanup EXIT

hash_file() {
  local path="$1"
  local output

  if command -v shasum >/dev/null 2>&1; then
    output="$(shasum -a 256 "$path")"
    printf '%s\n' "${output%% *}"
    return
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    output="$(sha256sum "$path")"
    printf '%s\n' "${output%% *}"
    return
  fi

  echo "shasum or sha256sum is required to verify guest artifacts" >&2
  exit 1
}

verify_sha256() {
  local path="$1"
  local actual

  actual="$(hash_file "$path")"
  [[ "$actual" == "$kernel_sha256" ]]
}

if [[ -e "$target_path" && ! -f "$target_path" ]]; then
  echo "cached kernel artifact path exists but is not a regular file: $target_path" >&2
  exit 1
fi

if [[ -f "$target_path" ]]; then
  if verify_sha256 "$target_path"; then
    echo "using cached Firecracker kernel artifact: $target_path" >&2
    printf '%s\n' "$target_path"
    exit 0
  fi

  echo "cached Firecracker kernel artifact failed SHA-256 verification; redownloading" >&2
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to fetch guest artifacts" >&2
  exit 1
fi

mkdir -p "$target_dir"
tmp_file="$(mktemp "${target_path}.download.XXXXXX")"

echo "fetching Firecracker kernel artifact: $kernel_url" >&2
curl \
  --fail \
  --location \
  --show-error \
  --silent \
  --retry 3 \
  --connect-timeout 10 \
  --output "$tmp_file" \
  "$kernel_url"

if ! verify_sha256 "$tmp_file"; then
  echo "downloaded Firecracker kernel artifact failed SHA-256 verification" >&2
  exit 1
fi

chmod 0644 "$tmp_file"
mv "$tmp_file" "$target_path"
tmp_file=""

printf '%s\n' "$target_path"
