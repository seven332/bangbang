#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

usage() {
  /bin/cat <<'EOF'
Usage: scripts/run-elevated-bootstrap-probe.sh --bundle /absolute/path/Bangbang.app
       --target-uid UID --target-gid GID

Run the no-skip elevated bootstrap evidence matrix on a capable Apple Silicon
host. This wrapper must already have exact real/effective uid/gid zero. It does
not invoke sudo or infer authority from SUDO_*, HOME, PATH, or account names.
EOF
}

bundle=""
bundle_set=false
target_uid=""
target_uid_set=false
target_gid=""
target_gid_set=false

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --bundle)
      if [[ "$bundle_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--bundle requires a path" >&2
        usage >&2
        exit 2
      fi
      bundle="$1"
      bundle_set=true
      ;;
    --target-uid)
      if [[ "$target_uid_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 ]]; then
        echo "--target-uid requires a value" >&2
        usage >&2
        exit 2
      fi
      target_uid="$1"
      target_uid_set=true
      ;;
    --target-gid)
      if [[ "$target_gid_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 ]]; then
        echo "--target-gid requires a value" >&2
        usage >&2
        exit 2
      fi
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

if [[ "$bundle_set" != true || "$target_uid_set" != true || "$target_gid_set" != true ]]; then
  echo "bundle, target uid, and target gid are required" >&2
  usage >&2
  exit 2
fi

is_nonzero_u32_decimal() {
  local value="$1"
  case "$value" in
    "" | 0* | *[!0-9]*)
      return 1
      ;;
  esac
  if [[ "${#value}" -gt 10 \
    || ("${#value}" -eq 10 && "$value" > "4294967295") ]]; then
    return 1
  fi
  return 0
}

if ! is_nonzero_u32_decimal "$target_uid" \
  || ! is_nonzero_u32_decimal "$target_gid"; then
    echo "target uid and gid must be explicit nonzero decimal values" >&2
    exit 2
fi

if [[ "$(/usr/bin/id -u)" != "0" \
  || "$(/usr/bin/id -ru)" != "0" \
  || "$(/usr/bin/id -g)" != "0" \
  || "$(/usr/bin/id -rg)" != "0" ]]; then
  echo "bangbang elevated bootstrap proof: explicit root required" >&2
  exit 4
fi

if [[ "$bundle" != /* || "$(/usr/bin/basename "$bundle")" != "Bangbang.app" || ! -d "$bundle" || -L "$bundle" ]]; then
  echo "invalid evidence bundle" >&2
  exit 2
fi
launcher="$bundle/Contents/MacOS/bangbang"
worker_bundle="$bundle/Contents/Helpers/BangbangWorker.app"
worker="$worker_bundle/Contents/MacOS/bangbang-worker"
marker="$worker_bundle/Contents/Resources/elevated-bootstrap-probe.enabled"
for entry in "$launcher" "$worker" "$marker"; do
  if [[ ! -f "$entry" || -L "$entry" ]]; then
    echo "invalid evidence bundle" >&2
    exit 1
  fi
done

if [[ "$(/usr/bin/uname -m)" != "arm64" ]]; then
  echo "bangbang elevated bootstrap proof: Apple Silicon required" >&2
  exit 1
fi
hv_support="$(/usr/sbin/sysctl -n kern.hv_support 2>/dev/null || true)"
hv_disable="$(/usr/sbin/sysctl -n kern.hv_disable 2>/dev/null || true)"
if [[ "$hv_support" != "1" || "$hv_disable" == "1" ]]; then
  echo "bangbang elevated bootstrap proof: Hypervisor.framework unavailable" >&2
  exit 1
fi
if ! /usr/bin/codesign --verify --deep --strict "$bundle" >/dev/null 2>&1; then
  echo "bangbang elevated bootstrap proof: bundle validation failed" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/dscacheutil -q user -a uid 2147483647)" \
  || -n "$(/usr/bin/dscacheutil -q group -a gid 2147483647)" ]]; then
  echo "bangbang elevated bootstrap proof: unmapped numeric fixture unavailable" >&2
  exit 1
fi
if [[ -z "$(/usr/bin/dscacheutil -q user -a uid "$target_uid")" \
  || -z "$(/usr/bin/dscacheutil -q group -a gid "$target_gid")" ]]; then
  echo "bangbang elevated bootstrap proof: explicit target account unavailable" >&2
  exit 1
fi

os_version="$(/usr/bin/sw_vers -productVersion)"
sdk_version="$(/usr/bin/xcrun --sdk macosx --show-sdk-version)"
echo "platform: macos=$os_version sdk=$sdk_version arch=arm64 hvf=supported root=exact"

probe_root=""
probe_root_identity=""
concurrent_root_a=""
concurrent_root_a_identity=""
concurrent_root_b=""
concurrent_root_b_identity=""
workspace=""
workspace_identity=""
symlink_root=""
symlink_target=""
replacement_root=""
replacement_root_identity=""
replacement_candidate=""
replacement_candidate_identity=""

create_private_directory() {
  local pattern="$1"
  local path_variable="$2"
  local identity_variable="$3"
  local path
  trap '' INT TERM HUP
  path="$(/usr/bin/mktemp -d "$pattern")"
  /bin/chmod 0700 "$path"
  /usr/sbin/chown 0:0 "$path"
  printf -v "$path_variable" '%s' "$path"
  printf -v "$identity_variable" '%s' "$(/usr/bin/stat -f '%d:%i' "$path")"
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
}

cleanup_directory() {
  local path="$1"
  local identity="$2"
  if [[ -z "$path" ]]; then
    return 0
  fi
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    return 0
  fi
  if [[ ! -d "$path" || -L "$path" ]]; then
    return 1
  fi
  local current
  current="$(/usr/bin/stat -f '%d:%i' "$path" 2>/dev/null || true)"
  if [[ "$current" != "$identity" ]]; then
    return 1
  fi
  local ownership
  ownership="$(/usr/bin/stat -f '%u:%g' "$path" 2>/dev/null || true)"
  if [[ "$ownership" != "0:0" ]]; then
    return 1
  fi
  local shape
  shape="$(/usr/bin/stat -f '%HT:%Lp' "$path" 2>/dev/null || true)"
  case "$shape" in
    Directory:700 | Directory:770) ;;
    *) return 1 ;;
  esac
  /bin/chmod 0700 "$path" || return 1
  /bin/rmdir "$path"
}

cleanup() {
  local prior_status=$?
  trap - EXIT
  trap '' INT TERM HUP
  local cleanup_status=0
  if [[ -n "$symlink_root" ]]; then
    if [[ -L "$symlink_root" \
      && "$(/usr/bin/readlink "$symlink_root")" == "$symlink_target" ]]; then
      /bin/unlink "$symlink_root" || cleanup_status=1
    else
      cleanup_status=1
    fi
  fi
  if [[ -n "$workspace" && -d "$workspace" ]]; then
    for output in "$workspace"/case-*; do
      if [[ -f "$output" && ! -L "$output" ]]; then
        /bin/unlink "$output" || cleanup_status=1
      fi
    done
  fi
  cleanup_directory "$concurrent_root_a" "$concurrent_root_a_identity" || cleanup_status=1
  cleanup_directory "$concurrent_root_b" "$concurrent_root_b_identity" || cleanup_status=1
  cleanup_directory "$replacement_candidate" "$replacement_candidate_identity" || cleanup_status=1
  cleanup_directory "$replacement_root" "$replacement_root_identity" || cleanup_status=1
  cleanup_directory "$probe_root" "$probe_root_identity" || cleanup_status=1
  cleanup_directory "$workspace" "$workspace_identity" || cleanup_status=1
  if [[ "$cleanup_status" -ne 0 ]]; then
    echo "bangbang elevated bootstrap proof: exact cleanup failed" >&2
    exit 5
  fi
  exit "$prior_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  probe_root probe_root_identity

invoke() {
  local root="$1"
  local uid="$2"
  local gid="$3"
  local mode="$4"
  /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin \
    "$launcher" \
    --bangbang-internal-elevated-bootstrap-probe-v1 \
    --root "$root" \
    --target-uid "$uid" \
    --target-gid "$gid" \
    --mode "$mode" \
    --
}

assert_case() {
  local root="$1"
  local uid="$2"
  local gid="$3"
  local mode="$4"
  local expected_status="$5"
  local expected_output="$6"
  local output
  local status
  set +e
  output="$(invoke "$root" "$uid" "$gid" "$mode" 2>&1)"
  status=$?
  set -e
  if [[ "$status" -ne "$expected_status" || "$output" != "$expected_output" ]]; then
    echo "bangbang elevated bootstrap proof: case failed" >&2
    exit 1
  fi
}

assert_case "$probe_root" 0 0 control 0 \
  "status: elevated bootstrap control complete"
for _ in 1 2 3; do
  assert_case "$probe_root" "$target_uid" "$target_gid" drop 3 \
    "status: elevated bootstrap blocked stage=chroot error=permission-denied"
done
assert_case "$probe_root" 0 0 retain-root 3 \
  "status: elevated bootstrap blocked stage=chroot error=permission-denied"
assert_case "$probe_root" 2147483647 2147483647 unmapped-syscall 3 \
  "status: elevated bootstrap blocked stage=chroot error=permission-denied"

/bin/chmod 0770 "$probe_root"
assert_case "$probe_root" 0 0 control 1 \
  "bangbang launcher: invalid production launch policy"
/bin/chmod 0700 "$probe_root"

symlink_root="/private/var/root/bangbang-elevated-probe.Symlink1"
symlink_target="$probe_root"
if [[ -e "$symlink_root" || -L "$symlink_root" ]]; then
  echo "bangbang elevated bootstrap proof: symlink fixture collision" >&2
  exit 1
fi
/bin/ln -s "$probe_root" "$symlink_root"
set +e
symlink_output="$(invoke "$symlink_root" 0 0 control 2>&1)"
symlink_status=$?
set -e
if [[ "$symlink_status" -ne 1 || "$symlink_output" != "bangbang launcher: invalid production launch policy" ]]; then
  echo "bangbang elevated bootstrap proof: symlink case failed" >&2
  exit 1
fi
/bin/unlink "$symlink_root"
symlink_root=""
symlink_target=""

original_replacement_identity=""
create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  replacement_root original_replacement_identity
replacement_root_identity="$original_replacement_identity"
create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  replacement_candidate replacement_candidate_identity
/bin/rmdir "$replacement_root"
trap '' INT TERM HUP
/bin/mv "$replacement_candidate" "$replacement_root"
replacement_root_identity="$replacement_candidate_identity"
replacement_candidate=""
replacement_candidate_identity=""
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
if [[ "$replacement_root_identity" == "$original_replacement_identity" ]] \
  || cleanup_directory "$replacement_root" "$original_replacement_identity" \
  || [[ ! -d "$replacement_root" ]]; then
  echo "bangbang elevated bootstrap proof: replacement preservation failed" >&2
  exit 1
fi

create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  concurrent_root_a concurrent_root_a_identity
create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  concurrent_root_b concurrent_root_b_identity
create_private_directory "/private/var/root/bangbang-elevated-work.XXXXXXXX" \
  workspace workspace_identity

set +e
invoke "$concurrent_root_a" "$target_uid" "$target_gid" drop > "$workspace/case-a" 2>&1 &
pid_a=$!
invoke "$concurrent_root_b" "$target_uid" "$target_gid" drop > "$workspace/case-b" 2>&1 &
pid_b=$!
wait "$pid_a"
status_a=$?
wait "$pid_b"
status_b=$?
set -e
output_a="$(<"$workspace/case-a")"
output_b="$(<"$workspace/case-b")"
expected_block="status: elevated bootstrap blocked stage=chroot error=permission-denied"
if [[ "$status_a" -ne 3 || "$status_b" -ne 3 \
  || "$output_a" != "$expected_block" || "$output_b" != "$expected_block" ]]; then
  echo "bangbang elevated bootstrap proof: concurrency case failed" >&2
  exit 1
fi
/bin/unlink "$workspace/case-a"
/bin/unlink "$workspace/case-b"

echo "result: app-sandbox-chroot=permission-denied control=success cleanup=exact"
