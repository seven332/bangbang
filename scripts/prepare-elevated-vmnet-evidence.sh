#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

usage() {
  /bin/cat <<'EOF'
Usage: scripts/prepare-elevated-vmnet-evidence.sh --output ABSOLUTE_DIRECTORY

Build and validate the entitlement-free elevated shared-vmnet evidence package
as an ordinary user. The output must be absent. This command does not elevate
itself and never runs the positive root-required cases.
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
        echo "--output requires a directory" >&2
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

if [[ "$output_set" != true || "$output" != /* ]]; then
  echo "an absolute output directory is required" >&2
  exit 2
fi
if [[ -e "$output" || -L "$output" ]]; then
  echo "output directory must be absent" >&2
  exit 2
fi
if [[ "$(/usr/bin/uname -s)" != "Darwin" || "$(/usr/bin/uname -m)" != "arm64" ]]; then
  echo "bangbang elevated vmnet prepare: platform unsupported" >&2
  exit 1
fi
if [[ "$(/usr/bin/id -u)" == "0" || "$(/usr/bin/id -ru)" == "0" ]]; then
  echo "bangbang elevated vmnet prepare: ordinary user required" >&2
  exit 1
fi
if ! /usr/bin/command -v cargo >/dev/null 2>&1 \
  || ! /usr/bin/command -v python3 >/dev/null 2>&1 \
  || [[ ! -x /usr/bin/codesign ]]; then
  echo "bangbang elevated vmnet prepare: required tool unavailable" >&2
  exit 1
fi

repo_root="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_parent="$(/usr/bin/dirname "$output")"
output_name="$(/usr/bin/basename "$output")"
if [[ ! -d "$output_parent" || -L "$output_parent" || "$output_name" == "." || "$output_name" == "/" ]]; then
  echo "bangbang elevated vmnet prepare: unsafe output parent" >&2
  exit 1
fi

stage="$(/usr/bin/mktemp -d "$output_parent/.$output_name.stage.XXXXXX")"
ordinary_runs=""
published=false
cleanup() {
  if [[ -n "$ordinary_runs" && -d "$ordinary_runs" ]]; then
    /bin/rm -rf -- "$ordinary_runs"
  fi
  if [[ "$published" != true && -n "$stage" && -d "$stage" ]]; then
    /bin/rm -rf -- "$stage"
  fi
}
trap cleanup EXIT
log="$stage/prepare.log"
: > "$log"
/bin/chmod 0600 "$log"

cd "$repo_root"
kernel="$(scripts/fetch-firecracker-kernel.sh 2>>"$log")" || {
  echo "bangbang elevated vmnet prepare: kernel failed" >&2
  exit 1
}
rootfs="$(scripts/fetch-firecracker-rootfs.sh \
  --format ext4 \
  --ext4-size 512M \
  --direct-boot-init \
  --direct-boot-variant direct-boot-v111 \
  2>>"$log")" || {
  echo "bangbang elevated vmnet prepare: rootfs failed" >&2
  exit 1
}
sidecar="${rootfs}.bangbang.json"
if [[ ! -f "$kernel" || -L "$kernel" \
  || ! -f "$rootfs" || -L "$rootfs" \
  || ! -f "$sidecar" || -L "$sidecar" \
  || "$(/usr/bin/basename "$rootfs")" != "ubuntu-24.04-512M-direct-boot-v111.ext4" ]]; then
  echo "bangbang elevated vmnet prepare: artifact failed" >&2
  exit 1
fi

scripts/build-signed-bangbang.sh --output "$stage/bangbang" >>"$log" 2>&1 || {
  echo "bangbang elevated vmnet prepare: product build failed" >&2
  exit 1
}
cargo_messages="$stage/cargo-test.json"
cargo test \
  -p bangbang \
  --test elevated_vmnet_e2e \
  --all-features \
  --locked \
  --target aarch64-apple-darwin \
  --no-run \
  --message-format=json \
  >"$cargo_messages" 2>>"$log" || {
  echo "bangbang elevated vmnet prepare: harness build failed" >&2
  exit 1
}

test_source="$(/usr/bin/python3 - "$cargo_messages" <<'PY'
import json
import sys

matches = []
with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        message = json.loads(line)
        target = message.get("target", {})
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and executable is not None
            and target.get("name") == "elevated_vmnet_e2e"
            and "test" in target.get("kind", [])
        ):
            matches.append(executable)
if len(matches) != 1:
    raise SystemExit(1)
sys.stdout.write(matches[0])
PY
)" || {
  echo "bangbang elevated vmnet prepare: harness artifact failed" >&2
  exit 1
}
if [[ ! -f "$test_source" || -L "$test_source" ]]; then
  echo "bangbang elevated vmnet prepare: harness artifact failed" >&2
  exit 1
fi
/bin/cp -p -- "$test_source" "$stage/elevated-vmnet-e2e"
/usr/bin/codesign --force --sign - "$stage/elevated-vmnet-e2e" >>"$log" 2>&1
/usr/bin/codesign --verify --strict "$stage/elevated-vmnet-e2e" >>"$log" 2>&1

cargo build \
  -p bangbang-vmnet-provider \
  --bin bangbang-vmnet-provider \
  --all-features \
  --locked \
  --target aarch64-apple-darwin \
  --message-format=json \
  >"$cargo_messages" 2>>"$log" || {
  echo "bangbang elevated vmnet prepare: provider build failed" >&2
  exit 1
}
provider_source="$(/usr/bin/python3 - "$cargo_messages" <<'PY'
import json
import sys

matches = []
with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        message = json.loads(line)
        target = message.get("target", {})
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and executable is not None
            and target.get("name") == "bangbang-vmnet-provider"
            and "bin" in target.get("kind", [])
        ):
            matches.append(executable)
if len(matches) != 1:
    raise SystemExit(1)
sys.stdout.write(matches[0])
PY
)" || {
  echo "bangbang elevated vmnet prepare: provider artifact failed" >&2
  exit 1
}
if [[ ! -f "$provider_source" || -L "$provider_source" ]]; then
  echo "bangbang elevated vmnet prepare: provider artifact failed" >&2
  exit 1
fi
/bin/cp -p -- "$provider_source" "$stage/bangbang-vmnet-provider"
/usr/bin/codesign --force --sign - "$stage/bangbang-vmnet-provider" >>"$log" 2>&1
/usr/bin/codesign --verify --strict "$stage/bangbang-vmnet-provider" >>"$log" 2>&1

cargo test \
  -p bangbang-vmnet-provider \
  --test elevated_vmnet_provider_e2e \
  --all-features \
  --locked \
  --target aarch64-apple-darwin \
  --no-run \
  --message-format=json \
  >"$cargo_messages" 2>>"$log" || {
  echo "bangbang elevated vmnet prepare: provider harness build failed" >&2
  exit 1
}
provider_test_source="$(/usr/bin/python3 - "$cargo_messages" <<'PY'
import json
import sys

matches = []
with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        message = json.loads(line)
        target = message.get("target", {})
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and executable is not None
            and target.get("name") == "elevated_vmnet_provider_e2e"
            and "test" in target.get("kind", [])
        ):
            matches.append(executable)
if len(matches) != 1:
    raise SystemExit(1)
sys.stdout.write(matches[0])
PY
)" || {
  echo "bangbang elevated vmnet prepare: provider harness artifact failed" >&2
  exit 1
}
if [[ ! -f "$provider_test_source" || -L "$provider_test_source" ]]; then
  echo "bangbang elevated vmnet prepare: provider harness artifact failed" >&2
  exit 1
fi
/bin/cp -p -- "$provider_test_source" "$stage/elevated-vmnet-provider-e2e"
/usr/bin/codesign --force --sign - "$stage/elevated-vmnet-provider-e2e" >>"$log" 2>&1
/usr/bin/codesign --verify --strict "$stage/elevated-vmnet-provider-e2e" >>"$log" 2>&1

/bin/cp -p -- "$kernel" "$stage/vmlinux-6.1.155"
/bin/cp -p -- "$rootfs" "$stage/ubuntu-24.04-512M-direct-boot-v111.ext4"
/bin/cp -p -- "$sidecar" "$stage/ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json"
/bin/cp -p -- scripts/elevated_vmnet_evidence.py "$stage/elevated-vmnet-evidence.py"
/bin/chmod 0555 \
  "$stage/bangbang" \
  "$stage/elevated-vmnet-e2e" \
  "$stage/bangbang-vmnet-provider" \
  "$stage/elevated-vmnet-provider-e2e"
/bin/chmod 0444 \
  "$stage/vmlinux-6.1.155" \
  "$stage/ubuntu-24.04-512M-direct-boot-v111.ext4" \
  "$stage/ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json" \
  "$stage/elevated-vmnet-evidence.py"

entitlements="$stage/entitlements.plist"
/usr/bin/codesign --display --entitlements - --xml "$stage/bangbang" >"$entitlements" 2>>"$log" || {
  echo "bangbang elevated vmnet prepare: product signature failed" >&2
  exit 1
}
if ! /usr/bin/python3 - "$entitlements" <<'PY'
import plistlib
import sys

with open(sys.argv[1], "rb") as source:
    value = plistlib.load(source)
if value != {"com.apple.security.hypervisor": True}:
    raise SystemExit(1)
PY
then
  echo "bangbang elevated vmnet prepare: product entitlement failed" >&2
  exit 1
fi
provider_entitlements="$(/usr/bin/codesign --display --entitlements - --xml \
  "$stage/bangbang-vmnet-provider" 2>>"$log")" || {
  echo "bangbang elevated vmnet prepare: provider signature failed" >&2
  exit 1
}
if [[ -n "$provider_entitlements" ]]; then
  echo "bangbang elevated vmnet prepare: provider entitlement failed" >&2
  exit 1
fi
/bin/rm -f -- "$entitlements" "$cargo_messages"

ordinary_runs="$(/usr/bin/mktemp -d /private/var/tmp/bbe-prep.XXXXXX)"
BANGBANG_ELEVATED_VMNET_BANGBANG="$stage/bangbang" \
BANGBANG_ELEVATED_VMNET_KERNEL="$stage/vmlinux-6.1.155" \
BANGBANG_ELEVATED_VMNET_ROOTFS="$stage/ubuntu-24.04-512M-direct-boot-v111.ext4" \
BANGBANG_ELEVATED_VMNET_ROOTFS_SIDECAR="$stage/ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json" \
TMPDIR="$ordinary_runs" \
"$stage/elevated-vmnet-e2e" \
  --exact macos_arm64::ordinary_user_vmnet_start_is_denied \
  --test-threads=1 >>"$log" 2>&1 || {
  echo "bangbang elevated vmnet prepare: ordinary denial failed" >&2
  exit 1
}
if [[ -n "$(/usr/bin/find -x "$ordinary_runs" -mindepth 1 -print -quit)" ]]; then
  echo "bangbang elevated vmnet prepare: ordinary denial residue" >&2
  exit 1
fi
/bin/rmdir -- "$ordinary_runs"
ordinary_runs=""

BANGBANG_ELEVATED_VMNET_PROVIDER="$stage/bangbang-vmnet-provider" \
"$stage/elevated-vmnet-provider-e2e" \
  --exact macos_arm64::ordinary_user_provider_broker_is_denied \
  --test-threads=1 >>"$log" 2>&1 || {
  echo "bangbang elevated vmnet prepare: ordinary provider denial failed" >&2
  exit 1
}

printf '' > "$log"
/bin/chmod 0600 "$log"
if ! /usr/bin/python3 "$stage/elevated-vmnet-evidence.py" create \
  --directory "$stage" \
  --owner "$(/usr/bin/id -u)" >>"$log" 2>&1; then
  echo "bangbang elevated vmnet prepare: manifest failed" >&2
  exit 1
fi

if [[ -e "$output" || -L "$output" ]]; then
  echo "bangbang elevated vmnet prepare: output collision" >&2
  exit 1
fi
/bin/mv -- "$stage" "$output"
published=true
stage=""
echo "bangbang elevated vmnet prepare: ready"
