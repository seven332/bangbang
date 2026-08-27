#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

usage() {
  /bin/cat <<'EOF'
Usage: scripts/prepare-production-vmnet-topology.sh --output ABSOLUTE_PATH

Build and ad-hoc sign the exact networkless production topology bundle as an
ordinary user. The absent output must be named Bangbang.app. This preparation
does not elevate, invoke vmnet, or require Apple developer authorization.
EOF
}

output=""
output_set=false
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --output)
      if [[ "$output_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--output requires a path" >&2
        exit 2
      fi
      output="$1"
      output_set=true
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

if [[ "$output_set" != true || "$output" != /* \
  || "$(/usr/bin/basename "$output")" != "Bangbang.app" ]]; then
  echo "an absolute absent Bangbang.app output is required" >&2
  exit 2
fi
if [[ -e "$output" || -L "$output" ]]; then
  echo "output must be absent" >&2
  exit 2
fi
if [[ "$(/usr/bin/uname -s)" != "Darwin" || "$(/usr/bin/uname -m)" != "arm64" ]]; then
  echo "bangbang production vmnet topology prepare: platform unsupported" >&2
  exit 1
fi
if [[ "$(/usr/bin/id -u)" == "0" || "$(/usr/bin/id -ru)" == "0" ]]; then
  echo "bangbang production vmnet topology prepare: ordinary user required" >&2
  exit 1
fi
if ! /usr/bin/command -v cargo >/dev/null 2>&1 \
  || [[ ! -x /usr/bin/codesign ]] \
  || [[ ! -x /usr/bin/python3 ]]; then
  echo "bangbang production vmnet topology prepare: required tool unavailable" >&2
  exit 1
fi

output_parent="$(/usr/bin/dirname "$output")"
if [[ ! -d "$output_parent" || -L "$output_parent" ]]; then
  echo "bangbang production vmnet topology prepare: output parent invalid" >&2
  exit 1
fi

repo_root="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")/.." && pwd)"
resources="$(/usr/bin/mktemp -d "$output_parent/.bangbang-topology-resources.XXXXXX")"
cleanup() {
  if [[ -n "$resources" && -d "$resources" ]]; then
    /usr/bin/python3 - "$resources" <<'PY'
import shutil
import sys

shutil.rmtree(sys.argv[1])
PY
  fi
}
trap cleanup EXIT

/usr/bin/python3 - "$resources/grant-integration-probe.enabled" <<'PY'
import os
import sys

flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
descriptor = os.open(sys.argv[1], flags, 0o400)
try:
    os.write(descriptor, b"test-only\n")
finally:
    os.close(descriptor)
PY

cd "$repo_root"
target_triple="aarch64-apple-darwin"
cargo build \
  -p bangbang-launcher \
  -p bangbang-vmnet-provider \
  --bin bangbang-launcher \
  --bin bangbang-vmnet-provider \
  --release \
  --no-default-features \
  --locked \
  --target "$target_triple"
cargo build \
  -p bangbang \
  --bin bangbang \
  --release \
  --no-default-features \
  --features grant-integration-probe \
  --locked \
  --target "$target_triple"
cargo build \
  -p bangbang-launcher \
  --bin bangbang-bundle \
  --release \
  --locked

"$repo_root/target/release/bangbang-bundle" build \
  --launcher "$repo_root/target/$target_triple/release/bangbang-launcher" \
  --worker "$repo_root/target/$target_triple/release/bangbang" \
  --vmnet-provider "$repo_root/target/$target_triple/release/bangbang-vmnet-provider" \
  --output "$output" \
  --signing-identity - \
  --worker-profile networkless \
  --test-worker-resources "$resources"

/usr/bin/codesign --verify --deep --strict --verbose=4 "$output"
echo "bangbang production vmnet topology prepare: ready"
