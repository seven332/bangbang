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
credential_drop_mode="credential-drop"
credential_retain_mode="credential-retain-root"
credential_unmapped_mode="credential-unmapped"
credential_control_mode="credential-control"
credential_status="status: elevated credential"
credential_record="BBC1"
credential_datagram="BBG1"
credential_step="restore-groups"
credential_launcher_artifact="bangbang-elevated-credential-launcher-v1-credential-drop-BBC1-BBG1-restore-groups"
credential_worker_artifact="bangbang-elevated-credential-worker-v1-credential-drop-BBC1-BBG1-restore-groups"
runtime_drop_mode="runtime-drop"
runtime_retain_mode="runtime-retain-root"
runtime_unmapped_mode="runtime-unmapped"
runtime_status="status: elevated runtime"
continuation_record="BBA1"
runtime_authority_record="BBN1"
grant_activation="--bangbang-internal-grant-probe-v1"
grant_runtime_case="target-runtime"
runtime_launcher_boundary_artifact="bangbang-elevated-runtime-launcher-boundaries-v2-pre-ack-post-ack-session-create-session-open-authority-send-authority-receive-authority-validate-session-lock-session-enter-prepared-namespace-grant-transfer-proceed-terminal-continuation-ack-lifecycle-hello-runtime-session-create-runtime-session-open-runtime-authority-send-runtime-authority-receive-runtime-authority-validate-runtime-session-lock-runtime-session-enter-lifecycle-prepared-runtime-namespace-grant-accepted-lifecycle-proceed-lifecycle-terminal-runtime-cleanup-complete-continuation-boundary-identity-boundary-explicit-root-boundary-namespace-boundary-grant-boundary-lifecycle-boundary"
runtime_worker_boundary_artifact="bangbang-elevated-runtime-worker-boundaries-v2-pre-ack-post-ack-session-create-session-open-authority-send-authority-receive-authority-validate-session-lock-session-enter-prepared-namespace-grant-transfer-proceed-terminal-continuation-ack-lifecycle-hello-runtime-session-create-runtime-session-open-runtime-authority-send-runtime-authority-receive-runtime-authority-validate-runtime-session-lock-runtime-session-enter-lifecycle-prepared-runtime-namespace-grant-accepted-lifecycle-proceed-lifecycle-terminal-runtime-cleanup-complete-continuation-boundary-identity-boundary-explicit-root-boundary-namespace-boundary-grant-boundary-lifecycle-boundary"

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
  "$credential_drop_mode"
  "$credential_retain_mode"
  "$credential_unmapped_mode"
  "$credential_control_mode"
  "$credential_status"
  "$credential_record"
  "$credential_datagram"
  "$credential_step"
  "$credential_launcher_artifact"
  "$credential_worker_artifact"
  "$runtime_drop_mode"
  "$runtime_retain_mode"
  "$runtime_unmapped_mode"
  "$runtime_status"
  "$continuation_record"
  "$runtime_authority_record"
  "$grant_activation"
  "$grant_runtime_case"
  "$runtime_launcher_boundary_artifact"
  "$runtime_worker_boundary_artifact"
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
  --features bangbang/elevated-bootstrap-probe,bangbang/grant-integration-probe,bangbang-launcher/elevated-bootstrap-probe \
  --locked \
  --target "$target_triple"

for marker_value in \
  "$worker_activation" \
  "$status_activation" \
  "$inherited_mode" \
  "$hvf_stage" \
  "$credential_drop_mode" \
  "$credential_retain_mode" \
  "$credential_unmapped_mode" \
  "$credential_control_mode" \
  "$credential_status" \
  "$credential_record" \
  "$credential_datagram" \
  "$credential_step" \
  "$credential_launcher_artifact"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$launcher_bin"; then
    echo "evidence artifact is missing the elevated probe boundary" >&2
    exit 1
  fi
done
for marker_value in \
  "$runtime_drop_mode" \
  "$runtime_retain_mode" \
  "$runtime_unmapped_mode" \
  "$runtime_status" \
  "$continuation_record" \
  "$runtime_authority_record" \
  "$runtime_launcher_boundary_artifact"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$launcher_bin"; then
    echo "evidence launcher is missing the runtime continuation boundary" >&2
    exit 1
  fi
done
for marker_value in \
  "$ready_activation" \
  "$credential_drop_mode" \
  "$credential_record" \
  "$credential_datagram" \
  "$credential_step" \
  "$credential_worker_artifact"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$worker_bin"; then
    echo "evidence artifact is missing the elevated probe boundary" >&2
    exit 1
  fi
done
for marker_value in \
  "$runtime_drop_mode" \
  "$runtime_retain_mode" \
  "$runtime_unmapped_mode" \
  "$continuation_record" \
  "$runtime_authority_record" \
  "$grant_activation" \
  "$grant_runtime_case" \
  "$runtime_worker_boundary_artifact"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$worker_bin"; then
    echo "evidence worker is missing the runtime continuation boundary" >&2
    exit 1
  fi
done

cargo build \
  -p bangbang-launcher \
  --bin bangbang-bundle \
  --release \
  --locked

resources="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/bangbang-elevated-resources.XXXXXXXX")"
marker="$resources/elevated-bootstrap-probe.enabled"
grant_marker="$resources/grant-integration-probe.enabled"
runtime_marker="$resources/target-runtime-grant-probe.enabled"
cleanup_resources() {
  local prior_status=$?
  trap - EXIT
  if [[ -f "$marker" && ! -L "$marker" ]]; then
    /bin/unlink "$marker" || exit 1
  fi
  if [[ -f "$grant_marker" && ! -L "$grant_marker" ]]; then
    /bin/unlink "$grant_marker" || exit 1
  fi
  if [[ -f "$runtime_marker" && ! -L "$runtime_marker" ]]; then
    /bin/unlink "$runtime_marker" || exit 1
  fi
  if [[ -d "$resources" && ! -L "$resources" ]]; then
    /bin/rmdir "$resources" || exit 1
  fi
  exit "$prior_status"
}
trap cleanup_resources EXIT

/usr/bin/printf 'test-only\n' > "$marker"
/bin/chmod 0600 "$marker"
/usr/bin/printf 'test-only\n' > "$grant_marker"
/bin/chmod 0600 "$grant_marker"
/usr/bin/printf 'test-only\n' > "$runtime_marker"
/bin/chmod 0600 "$runtime_marker"

"$repo_root/target/release/bangbang-bundle" build \
  --launcher "$launcher_bin" \
  --worker "$worker_bin" \
  --output "$output" \
  --signing-identity - \
  --worker-profile networkless \
  --test-worker-resources "$resources"
