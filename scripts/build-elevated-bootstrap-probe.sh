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
sidecar="${output}.elevated-guest-sidecar"
if [[ -e "$output" || -L "$output" || -e "$sidecar" || -L "$sidecar" ]]; then
  echo "output or evidence sidecar already exists" >&2
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
api_listener_launcher_boundary_artifact="bangbang-elevated-api-listener-launcher-v1-BBL1-request-bind-transfer-adoption-final-child-one-right"
api_listener_worker_boundary_artifact="bangbang-elevated-api-listener-worker-v1-BBL1-request-ack-adoption-record-readiness"
guest_no_api_drop_mode="guest-no-api-drop"
guest_no_api_retain_mode="guest-no-api-retain-root"
guest_no_api_unmapped_mode="guest-no-api-unmapped"
guest_api_drop_mode="guest-api-drop"
guest_api_retain_mode="guest-api-retain-root"
guest_api_unmapped_mode="guest-api-unmapped"
guest_evidence_record="BBW1"
guest_resource_witness="guest-resource-witness"
guest_grant_accepted="guest-grant-accepted"
guest_transport_contamination="guest-transport-contamination"
guest_hvf_witness="guest-hvf-witness"
guest_terminal_evidence="guest-terminal-evidence"
guest_api_start="api-instance-start"
guest_kernel_reference="bangbang-grant:evidence-guest-kernel"
guest_serial_reference="bangbang-grant:evidence-guest-serial"
guest_adoption_barrier="--bangbang-internal-post-adoption-stop-v1"
guest_isolation_markers=(
  guest-no-api-drop
  guest-no-api-retain-root
  guest-no-api-unmapped
  guest-api-drop
  guest-api-retain-root
  guest-api-unmapped
  BBW1
  guest-grant-contract
  guest-grant-accepted
  guest-transport-contamination
  guest-resource-witness
  api-listener-request
  api-listener-bind
  api-listener-transfer
  api-listener-adoption
  api-socket-publication
  api-logger-configuration
  api-metrics-configuration
  api-serial-configuration
  api-machine-configuration
  api-boot-configuration
  api-drive-configuration
  api-instance-start
  no-api-startup
  guest-hvf-witness
  guest-hvf-create
  guest-execution
  guest-oracle
  guest-poweroff
  guest-timeout
  guest-endpoint-death
  guest-terminal-evidence
  guest-cleanup
  api-boundary
  hvf-boundary
  guest-boundary
  evidence-guest-config
  evidence-guest-kernel
  evidence-guest-initrd
  evidence-guest-rootfs
  evidence-guest-logger
  evidence-guest-metrics
  evidence-guest-serial
  evidence-guest-api
  BANGBANG_ROOTFS_WORKFLOW_OK
  resources=consumed\ workload=no-api
  resources=consumed\ workload=api
)

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
  "$api_listener_launcher_boundary_artifact"
  "$api_listener_worker_boundary_artifact"
  "$guest_no_api_drop_mode"
  "$guest_no_api_retain_mode"
  "$guest_no_api_unmapped_mode"
  "$guest_api_drop_mode"
  "$guest_api_retain_mode"
  "$guest_api_unmapped_mode"
  "$guest_evidence_record"
  "$guest_resource_witness"
  "$guest_grant_accepted"
  "$guest_transport_contamination"
  "$guest_hvf_witness"
  "$guest_terminal_evidence"
  "$guest_api_start"
  "$guest_kernel_reference"
  "$guest_serial_reference"
  "$guest_adoption_barrier"
  "${guest_isolation_markers[@]}"
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
  "$guest_no_api_drop_mode" \
  "$guest_no_api_retain_mode" \
  "$guest_no_api_unmapped_mode" \
  "$guest_api_drop_mode" \
  "$guest_api_retain_mode" \
  "$guest_api_unmapped_mode" \
  "$guest_evidence_record" \
  "$guest_resource_witness" \
  "$guest_grant_accepted" \
  "$guest_transport_contamination" \
  "$guest_hvf_witness" \
  "$guest_terminal_evidence" \
  "$guest_api_start" \
  "$guest_kernel_reference" \
  "$guest_serial_reference" \
  "$api_listener_launcher_boundary_artifact"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$launcher_bin"; then
    echo "evidence launcher is missing the guest continuation boundary" >&2
    exit 1
  fi
done
# The stop marker is launcher-only and intentionally absent from the worker.
if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$guest_adoption_barrier" "$launcher_bin"; then
  echo "evidence launcher is missing the post-adoption barrier" >&2
  exit 1
fi
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
for marker_value in \
  "$guest_no_api_drop_mode" \
  "$guest_no_api_retain_mode" \
  "$guest_no_api_unmapped_mode" \
  "$guest_api_drop_mode" \
  "$guest_api_retain_mode" \
  "$guest_api_unmapped_mode" \
  "$guest_evidence_record" \
  "$guest_resource_witness" \
  "$guest_grant_accepted" \
  "$guest_transport_contamination" \
  "$guest_hvf_witness" \
  "$guest_terminal_evidence" \
  "$guest_kernel_reference" \
  "$guest_serial_reference" \
  "$api_listener_worker_boundary_artifact"; do
  if ! LC_ALL=C /usr/bin/grep -a -F -q -- "$marker_value" "$worker_bin"; then
    echo "evidence worker is missing the guest continuation boundary" >&2
    exit 1
  fi
done

scripts/fetch-firecracker-kernel.sh >/dev/null
scripts/fetch-firecracker-rootfs.sh >/dev/null
scripts/build-guest-boot-initrd.py --check >/dev/null

cargo build \
  -p bangbang-launcher \
  --bin bangbang-bundle \
  --release \
  --locked

resources="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/bangbang-elevated-resources.XXXXXXXX")"
output_parent="$(/usr/bin/dirname "$output")"
if [[ ! -d "$output_parent" || -L "$output_parent" ]]; then
  echo "output parent must be an existing directory" >&2
  exit 1
fi
sidecar_stage="$(/usr/bin/mktemp -d "$output_parent/.bangbang-elevated-sidecar.XXXXXXXX")"
marker="$resources/elevated-bootstrap-probe.enabled"
grant_marker="$resources/grant-integration-probe.enabled"
runtime_marker="$resources/target-runtime-grant-probe.enabled"
guest_marker="$resources/elevated-guest-evidence.enabled"
guest_resource_names=(
  evidence-guest-kernel
  evidence-guest-rootfs
  evidence-guest-initrd
  evidence-guest-no-api.json
)
bundle_published=false
sidecar_published=false
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
  for resource_name in "${guest_resource_names[@]}"; do
    if [[ -f "$resources/$resource_name" && ! -L "$resources/$resource_name" ]]; then
      /bin/unlink "$resources/$resource_name" || exit 1
    fi
  done
  if [[ -f "$guest_marker" && ! -L "$guest_marker" ]]; then
    /bin/unlink "$guest_marker" || exit 1
  fi
  if [[ -d "$resources" && ! -L "$resources" ]]; then
    /bin/rmdir "$resources" || exit 1
  fi
  if [[ "$bundle_published" != true && "$sidecar_published" == true ]]; then
    /usr/bin/python3 scripts/elevated_guest_evidence.py cleanup-sidecar \
      --directory "$sidecar" >/dev/null || exit 1
  elif [[ "$bundle_published" != true ]]; then
    for resource_name in "${guest_resource_names[@]}"; do
      if [[ -f "$sidecar_stage/$resource_name" && ! -L "$sidecar_stage/$resource_name" ]]; then
        /bin/unlink "$sidecar_stage/$resource_name" || exit 1
      fi
    done
    if [[ -d "$sidecar_stage" && ! -L "$sidecar_stage" ]]; then
      /bin/rmdir "$sidecar_stage" || exit 1
    fi
  fi
  exit "$prior_status"
}
trap cleanup_resources EXIT

/usr/bin/python3 scripts/elevated_guest_evidence.py prepare \
  --resources "$resources" \
  --sidecar "$sidecar_stage" >/dev/null

/usr/bin/printf 'test-only\n' > "$marker"
/bin/chmod 0600 "$marker"
/usr/bin/printf 'test-only\n' > "$grant_marker"
/bin/chmod 0600 "$grant_marker"
/usr/bin/printf 'test-only\n' > "$runtime_marker"
/bin/chmod 0600 "$runtime_marker"

/usr/bin/python3 scripts/elevated_guest_evidence.py verify \
  --directory "$sidecar_stage" \
  --kind sidecar >/dev/null
/usr/bin/python3 scripts/elevated_guest_evidence.py publish-sidecar \
  --directory "$sidecar_stage" \
  --destination "$sidecar" >/dev/null
sidecar_published=true

"$repo_root/target/release/bangbang-bundle" build \
  --launcher "$launcher_bin" \
  --worker "$worker_bin" \
  --output "$output" \
  --signing-identity - \
  --worker-profile networkless \
  --test-worker-resources "$resources"
bundle_published=true

worker_resources="$output/Contents/Helpers/BangbangWorker.app/Contents/Resources"
/usr/bin/python3 scripts/elevated_guest_evidence.py verify \
  --directory "$worker_resources" \
  --kind bundle >/dev/null
/usr/bin/python3 scripts/elevated_guest_evidence.py verify \
  --directory "$sidecar" \
  --kind sidecar >/dev/null
