#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

# An external authorization mechanism may use standard input. No validator or
# evidence process may inherit that descriptor.
exec </dev/null

usage() {
  /bin/cat <<'EOF'
Usage: scripts/run-elevated-vmnet-evidence.sh --prepared ABSOLUTE_DIRECTORY
       --target-uid UID --target-gid GID

Run the already-prepared entitlement-free shared-vmnet evidence on a capable
Apple Silicon host. This command must already have exact root authority. It
does not build, download, sign, or discover credentials.
EOF
}

prepared=""
target_uid=""
target_gid=""
prepared_set=false
target_uid_set=false
target_gid_set=false
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --prepared)
      if [[ "$prepared_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      [[ "$#" -gt 0 && -n "$1" ]] || { echo "missing prepared directory" >&2; exit 2; }
      prepared="$1"
      prepared_set=true
      ;;
    --target-uid)
      if [[ "$target_uid_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      [[ "$#" -gt 0 && -n "$1" ]] || { echo "missing target uid" >&2; exit 2; }
      target_uid="$1"
      target_uid_set=true
      ;;
    --target-gid)
      if [[ "$target_gid_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      [[ "$#" -gt 0 && -n "$1" ]] || { echo "missing target gid" >&2; exit 2; }
      target_gid="$1"
      target_gid_set=true
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

if [[ "$prepared_set" != true || "$target_uid_set" != true || "$target_gid_set" != true ]]; then
  echo "prepared directory and target ids are required" >&2
  exit 2
fi
case "$target_uid" in
  "" | 0* | *[!0-9]*) echo "target ids must be nonzero decimal" >&2; exit 2 ;;
esac
case "$target_gid" in
  "" | 0* | *[!0-9]*) echo "target ids must be nonzero decimal" >&2; exit 2 ;;
esac
if [[ "${#target_uid}" -gt 10 || ("${#target_uid}" -eq 10 && "$target_uid" > "4294967295") \
  || "${#target_gid}" -gt 10 || ("${#target_gid}" -eq 10 && "$target_gid" > "4294967295") ]]; then
  echo "target ids must fit u32" >&2
  exit 2
fi
if [[ "$(/usr/bin/id -u)" != "0" || "$(/usr/bin/id -ru)" != "0" \
  || "$(/usr/bin/id -g)" != "0" || "$(/usr/bin/id -rg)" != "0" ]]; then
  echo "bangbang elevated vmnet proof: exact root required" >&2
  exit 4
fi
if [[ "$prepared" != /* || ! -d "$prepared" || -L "$prepared" ]]; then
  echo "bangbang elevated vmnet proof: prepared package invalid" >&2
  exit 2
fi
prepared_state="$(/usr/bin/stat -f '%u:%g:%HT:%Lp' "$prepared" 2>/dev/null || true)"
if [[ "$prepared_state" != "$target_uid:$target_gid:Directory:700" ]]; then
  echo "bangbang elevated vmnet proof: prepared package invalid" >&2
  exit 1
fi
if [[ "$(/usr/bin/uname -s)" != "Darwin" || "$(/usr/bin/uname -m)" != "arm64" ]]; then
  echo "bangbang elevated vmnet proof: platform unsupported" >&2
  exit 1
fi
hv_support="$(/usr/sbin/sysctl -n kern.hv_support 2>/dev/null || true)"
hv_disable="$(/usr/sbin/sysctl -n kern.hv_disable 2>/dev/null || true)"
if [[ "$hv_support" != "1" || "$hv_disable" == "1" ]]; then
  echo "bangbang elevated vmnet proof: hypervisor unavailable" >&2
  exit 1
fi

names=(
  bangbang
  elevated-vmnet-e2e
  bangbang-vmnet-provider
  elevated-vmnet-provider-e2e
  vmlinux-6.1.155
  ubuntu-24.04-512M-direct-boot-v111.ext4
  ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json
  ubuntu-24.04-512M-direct-boot-v112.ext4
  ubuntu-24.04-512M-direct-boot-v112.ext4.bangbang.json
  elevated-vmnet-evidence.py
  staged-vmnet-evidence.py
  staged-vmnet-certification.py
  manifest.json
  prepare.log
)
count="$(/usr/bin/find -x "$prepared" -mindepth 1 -maxdepth 1 -print | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
if [[ "$count" != "${#names[@]}" ]]; then
  echo "bangbang elevated vmnet proof: prepared package invalid" >&2
  exit 1
fi
for name in "${names[@]}"; do
  path="$prepared/$name"
  state="$(/usr/bin/stat -f '%u:%g:%HT:%l' "$path" 2>/dev/null || true)"
  case "$name" in
    bangbang | elevated-vmnet-e2e | bangbang-vmnet-provider | elevated-vmnet-provider-e2e)
      expected="$target_uid:$target_gid:Regular File:1"
      mode=555
      ;;
    prepare.log)
      expected="$target_uid:$target_gid:Regular File:1"
      mode=600
      ;;
    *)
      expected="$target_uid:$target_gid:Regular File:1"
      mode=444
      ;;
  esac
  if [[ "$state" != "$expected" || "$(/usr/bin/stat -f '%Lp' "$path" 2>/dev/null || true)" != "$mode" || -L "$path" ]]; then
    echo "bangbang elevated vmnet proof: prepared package invalid" >&2
    exit 1
  fi
done

stage="$(/usr/bin/mktemp -d /private/var/tmp/bangbang-elevated-vmnet.XXXXXX)"
cleanup() {
  if [[ -n "$stage" && -d "$stage" ]]; then
    /bin/rm -rf -- "$stage"
  fi
}
trap cleanup EXIT
for name in "${names[@]}"; do
  /bin/cp -p -- "$prepared/$name" "$stage/$name"
done
/usr/sbin/chown -R 0:0 "$stage"
/bin/chmod 0700 "$stage"
/bin/chmod 0555 \
  "$stage/bangbang" \
  "$stage/elevated-vmnet-e2e" \
  "$stage/bangbang-vmnet-provider" \
  "$stage/elevated-vmnet-provider-e2e"
/bin/chmod 0444 \
  "$stage/vmlinux-6.1.155" \
  "$stage/ubuntu-24.04-512M-direct-boot-v111.ext4" \
  "$stage/ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json" \
  "$stage/ubuntu-24.04-512M-direct-boot-v112.ext4" \
  "$stage/ubuntu-24.04-512M-direct-boot-v112.ext4.bangbang.json" \
  "$stage/elevated-vmnet-evidence.py" \
  "$stage/staged-vmnet-evidence.py" \
  "$stage/staged-vmnet-certification.py" \
  "$stage/manifest.json"
/bin/chmod 0600 "$stage/prepare.log"
if ! /usr/bin/python3 "$stage/elevated-vmnet-evidence.py" verify \
  --directory "$stage" \
  --owner 0 >/dev/null 2>&1; then
  echo "bangbang elevated vmnet proof: prepared package invalid" >&2
  exit 1
fi
/bin/mkdir -m 0700 "$stage/runs"
if ! /usr/bin/codesign --verify --strict "$stage/bangbang" >/dev/null 2>&1 \
  || ! /usr/bin/codesign --verify --strict "$stage/elevated-vmnet-e2e" >/dev/null 2>&1 \
  || ! /usr/bin/codesign --verify --strict "$stage/bangbang-vmnet-provider" >/dev/null 2>&1 \
  || ! /usr/bin/codesign --verify --strict "$stage/elevated-vmnet-provider-e2e" >/dev/null 2>&1; then
  echo "bangbang elevated vmnet proof: code validation failed" >&2
  exit 1
fi
entitlements="$stage/entitlements.plist"
if ! /usr/bin/codesign --display --entitlements - --xml "$stage/bangbang" >"$entitlements" 2>/dev/null \
  || ! /usr/bin/python3 - "$entitlements" <<'PY'
import plistlib
import sys
with open(sys.argv[1], "rb") as source:
    value = plistlib.load(source)
if value != {"com.apple.security.hypervisor": True}:
    raise SystemExit(1)
PY
then
  echo "bangbang elevated vmnet proof: entitlement validation failed" >&2
  exit 1
fi
provider_entitlements="$(/usr/bin/codesign --display --entitlements - --xml \
  "$stage/bangbang-vmnet-provider" 2>/dev/null)" || {
  echo "bangbang elevated vmnet proof: provider signature invalid" >&2
  exit 1
}
if [[ -n "$provider_entitlements" ]]; then
  echo "bangbang elevated vmnet proof: provider entitlement invalid" >&2
  exit 1
fi
/bin/rm -f -- "$entitlements"

run_case() {
  local test_name="$1"
  /usr/bin/env -i \
    LANG=C \
    LC_ALL=C \
    TMPDIR="$stage/runs" \
    BANGBANG_ELEVATED_VMNET_BANGBANG="$stage/bangbang" \
    BANGBANG_ELEVATED_VMNET_KERNEL="$stage/vmlinux-6.1.155" \
    BANGBANG_ELEVATED_VMNET_ROOTFS="$stage/ubuntu-24.04-512M-direct-boot-v111.ext4" \
    BANGBANG_ELEVATED_VMNET_ROOTFS_SIDECAR="$stage/ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json" \
    BANGBANG_ELEVATED_VMNET_TARGET_UID="$target_uid" \
    BANGBANG_ELEVATED_VMNET_TARGET_GID="$target_gid" \
    "$stage/elevated-vmnet-e2e" \
      --exact "$test_name" \
      --test-threads=1 \
      >/dev/null 2>&1
}

run_provider_case() {
  local test_name="$1"
  /usr/bin/env -i \
    LANG=C \
    LC_ALL=C \
    TMPDIR="$stage/runs" \
    BANGBANG_ELEVATED_VMNET_PROVIDER="$stage/bangbang-vmnet-provider" \
    BANGBANG_ELEVATED_VMNET_TARGET_UID="$target_uid" \
    BANGBANG_ELEVATED_VMNET_TARGET_GID="$target_gid" \
    "$stage/elevated-vmnet-provider-e2e" \
      --exact "$test_name" \
      --test-threads=1 \
      >/dev/null 2>&1
}

run_staged_case() {
  local scenario="$1"
  /usr/bin/env -i \
    LANG=C \
    LC_ALL=C \
    TMPDIR="$stage/runs" \
    BANGBANG_ELEVATED_VMNET_BANGBANG="$stage/bangbang" \
    BANGBANG_ELEVATED_VMNET_KERNEL="$stage/vmlinux-6.1.155" \
    BANGBANG_STAGED_VMNET_ROOTFS="$stage/ubuntu-24.04-512M-direct-boot-v112.ext4" \
    BANGBANG_STAGED_VMNET_ROOTFS_SIDECAR="$stage/ubuntu-24.04-512M-direct-boot-v112.ext4.bangbang.json" \
    /usr/bin/python3 "$stage/staged-vmnet-evidence.py" \
      --scenario "$scenario" \
      >/dev/null 2>&1
}

if ! run_provider_case macos_arm64::dropped_provider_serves_data_lifecycle; then
  echo "bangbang elevated vmnet proof: provider data failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet proof: provider data residue" >&2
  exit 1
fi
if ! run_provider_case macos_arm64::control_cancellation_reaps_dropped_provider; then
  echo "bangbang elevated vmnet proof: provider cancellation failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet proof: provider cancellation residue" >&2
  exit 1
fi
if ! run_provider_case macos_arm64::dropped_provider_serves_data_lifecycle; then
  echo "bangbang elevated vmnet proof: provider repeat failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet proof: provider repeat residue" >&2
  exit 1
fi

if ! run_case macos_arm64::dropped_owner_retains_bounded_vmnet_io; then
  echo "bangbang elevated vmnet proof: dropped owner failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet proof: dropped owner residue" >&2
  exit 1
fi
if ! run_case macos_arm64::elevated_direct_guest_uses_shared_vmnet; then
  echo "bangbang elevated vmnet proof: first guest failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet proof: first guest residue" >&2
  exit 1
fi
if ! run_case macos_arm64::elevated_direct_guest_uses_shared_vmnet; then
  echo "bangbang elevated vmnet proof: repeat guest failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet proof: repeat guest residue" >&2
  exit 1
fi

for scenario in startup runtime restore; do
  if ! run_staged_case "$scenario"; then
    echo "bangbang elevated vmnet proof: staged $scenario failed" >&2
    exit 1
  fi
  if [[ -n "$(/usr/bin/find -x "$stage/runs" -mindepth 1 -print -quit)" ]]; then
    echo "bangbang elevated vmnet proof: staged $scenario residue" >&2
    exit 1
  fi
done

os_version="$(/usr/bin/sw_vers -productVersion)"
sdk_version="$(/usr/bin/xcrun --sdk macosx --show-sdk-version)"
echo "platform: macos=$os_version sdk=$sdk_version arch=arm64 hvf=supported root=exact apple-vmnet=absent"
echo "bangbang elevated vmnet proof: denial=passed provider=passed provider-cancel=passed provider-repeat=passed dropped-owner=passed guest=passed repeat=passed startup=passed runtime=passed restore=passed cleanup=passed"
