#!/usr/bin/env bash
set -euo pipefail

usage() {
  /bin/cat <<'EOF'
Usage: scripts/build-elevated-bootstrap-probe.sh --output /absolute/path/Bangbang.app

Build and ad-hoc sign the feature-gated elevated-bootstrap evidence bundle.
The destination must be absent. This build step must run as an ordinary user;
the separate run-elevated-bootstrap-probe.sh wrapper owns explicit root use.
EOF
}

output=""
output_set=false

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

if [[ "$output_set" != true || "$output" != /* || "$(basename "$output")" != "Bangbang.app" ]]; then
  echo "--output must be an absolute absent path named Bangbang.app" >&2
  usage >&2
  exit 2
fi
if [[ -e "$output" || -L "$output" ]]; then
  echo "output already exists" >&2
  exit 1
fi
if [[ "$(/usr/bin/id -u)" == "0" ]]; then
  echo "elevated probe build must run as an ordinary user" >&2
  exit 4
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_triple="aarch64-apple-darwin"
launcher_bin="$repo_root/target/$target_triple/release/bangbang-launcher"
worker_bin="$repo_root/target/$target_triple/release/bangbang"
worker_activation="--bangbang-internal-elevated-bootstrap-worker-v2"
status_activation="status: elevated bootstrap blocked"
ready_activation="BBEP-READY-V2"
inherited_mode="inherited-root"
hvf_stage="hvf-create"

cargo build \
  -p bangbang \
  -p bangbang-launcher \
  --bin bangbang \
  --bin bangbang-launcher \
  --release \
  --no-default-features \
  --locked \
  --target "$target_triple"

probe_markers=(
  "$worker_activation"
  "$status_activation"
  "$ready_activation"
  "$inherited_mode"
  "$hvf_stage"
)
for artifact in "$launcher_bin" "$worker_bin"; do
  for marker_value in "${probe_markers[@]}"; do
    if LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$artifact"; then
      echo "normal artifact unexpectedly contains elevated probe code" >&2
      exit 1
    fi
  done
done

cargo build \
  -p bangbang \
  -p bangbang-launcher \
  --bin bangbang \
  --bin bangbang-launcher \
  --release \
  --no-default-features \
  --features elevated-bootstrap-probe \
  --locked \
  --target "$target_triple"

for marker_value in \
  "$worker_activation" \
  "$status_activation" \
  "$inherited_mode" \
  "$hvf_stage"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$launcher_bin"; then
    echo "evidence artifact is missing the elevated probe boundary" >&2
    exit 1
  fi
done
if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$ready_activation" "$worker_bin"; then
  echo "evidence artifact is missing the elevated probe boundary" >&2
  exit 1
fi

cargo build \
  -p bangbang-launcher \
  --bin bangbang-bundle \
  --release \
  --locked

resources="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/bangbang-elevated-resources.XXXXXXXX")"
marker="$resources/elevated-bootstrap-probe.enabled"
cleanup_resources() {
  local prior_status=$?
  trap - EXIT
  if [[ -f "$marker" && ! -L "$marker" ]]; then
    /bin/unlink "$marker" || exit 1
  fi
  if [[ -d "$resources" && ! -L "$resources" ]]; then
    /bin/rmdir "$resources" || exit 1
  fi
  exit "$prior_status"
}
trap cleanup_resources EXIT

/usr/bin/printf 'test-only\n' > "$marker"
/bin/chmod 0600 "$marker"

"$repo_root/target/release/bangbang-bundle" build \
  --launcher "$launcher_bin" \
  --worker "$worker_bin" \
  --output "$output" \
  --signing-identity - \
  --worker-profile networkless \
  --test-worker-resources "$resources"
