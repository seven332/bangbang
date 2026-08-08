#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

# The caller may supply an elevation credential on standard input. Sudo starts
# this wrapper only after consuming it, so replace that descriptor before any
# validation tool, launcher, or signed worker can inherit it.
exec </dev/null

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
if [[ ! -f /usr/lib/dyld || -L /usr/lib/dyld \
  || "$(/usr/bin/stat -f '%u:%g:%HT:%Lp:%l' /usr/lib/dyld 2>/dev/null || true)" \
    != "0:0:Regular File:755:1" ]]; then
  echo "bangbang elevated bootstrap proof: loader validation failed" >&2
  exit 1
fi
loader_size="$(/usr/bin/stat -f '%z' /usr/lib/dyld)"
if [[ "$loader_size" -le 0 || "$loader_size" -gt 16777216 ]] \
  || ! /usr/bin/codesign --verify --strict /usr/lib/dyld >/dev/null 2>&1; then
  echo "bangbang elevated bootstrap proof: loader validation failed" >&2
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
inherited_root=""
inherited_root_identity=""
inherited_ledger=""
inherited_ledger_identity=""
inherited_root_a=""
inherited_root_a_identity=""
inherited_ledger_a=""
inherited_ledger_a_identity=""
inherited_root_b=""
inherited_root_b_identity=""
inherited_ledger_b=""
inherited_ledger_b_identity=""
concurrent_root_a=""
concurrent_root_a_identity=""
concurrent_root_b=""
concurrent_root_b_identity=""
workspace=""
workspace_identity=""
symlink_root=""
symlink_target=""
symlink_identity=""
symlink_placeholder_identity=""
replacement_root=""
replacement_root_identity=""
replacement_candidate=""
replacement_candidate_identity=""

bundle_directories=(
  "Contents"
  "Contents/_CodeSignature"
  "Contents/MacOS"
  "Contents/Helpers"
  "Contents/Helpers/BangbangWorker.app"
  "Contents/Helpers/BangbangWorker.app/Contents"
  "Contents/Helpers/BangbangWorker.app/Contents/_CodeSignature"
  "Contents/Helpers/BangbangWorker.app/Contents/MacOS"
  "Contents/Helpers/BangbangWorker.app/Contents/Resources"
)
bundle_files=(
  "Contents/_CodeSignature/CodeResources"
  "Contents/MacOS/bangbang"
  "Contents/Helpers/BangbangWorker.app/Contents/_CodeSignature/CodeResources"
  "Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker"
  "Contents/Helpers/BangbangWorker.app/Contents/Resources/elevated-bootstrap-probe.enabled"
  "Contents/Helpers/BangbangWorker.app/Contents/Info.plist"
  "Contents/Info.plist"
)

is_staged_relative() {
  case "$1" in
    "Bangbang.app" \
      | "Bangbang.app/Contents" \
      | "Bangbang.app/Contents/_CodeSignature" \
      | "Bangbang.app/Contents/_CodeSignature/CodeResources" \
      | "Bangbang.app/Contents/MacOS" \
      | "Bangbang.app/Contents/MacOS/bangbang" \
      | "Bangbang.app/Contents/Helpers" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/_CodeSignature" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/_CodeSignature/CodeResources" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/Resources" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/Resources/elevated-bootstrap-probe.enabled" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/Info.plist" \
      | "Bangbang.app/Contents/Info.plist" \
      | "usr" \
      | "usr/lib" \
      | "usr/lib/dyld")
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

validate_source_bundle_shape() {
  local relative
  for relative in "${bundle_directories[@]}"; do
    if [[ ! -d "$bundle/$relative" || -L "$bundle/$relative" ]]; then
      return 1
    fi
  done
  for relative in "${bundle_files[@]}"; do
    if [[ ! -f "$bundle/$relative" || -L "$bundle/$relative" ]]; then
      return 1
    fi
  done
  local count
  count="$(/usr/bin/find -x "$bundle" -mindepth 1 -print | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
  [[ "$count" == "$((${#bundle_directories[@]} + ${#bundle_files[@]}))" ]]
}

record_staged_entry() {
  local root="$1"
  local ledger="$2"
  local relative="$3"
  local kind="$4"
  local identity
  if ! is_staged_relative "$relative"; then
    return 1
  fi
  identity="$(/usr/bin/stat -f '%d:%i:%u:%g:%Lp' "$root/$relative")"
  /usr/bin/printf '%s\t%s\t%s\n' "$relative" "$identity" "$kind" >> "$ledger"
}

stage_directory() {
  local root="$1"
  local ledger="$2"
  local relative="$3"
  /bin/mkdir "$root/$relative"
  /usr/sbin/chown 0:0 "$root/$relative"
  /bin/chmod 0755 "$root/$relative"
  record_staged_entry "$root" "$ledger" "$relative" directory
}

stage_file() {
  local root="$1"
  local ledger="$2"
  local source="$3"
  local relative="$4"
  local mode="$5"
  /bin/cp -X "$source" "$root/$relative"
  /usr/sbin/chown 0:0 "$root/$relative"
  /bin/chmod "$mode" "$root/$relative"
  record_staged_entry "$root" "$ledger" "$relative" file
}

stage_inherited_root() {
  local root="$1"
  local ledger="$2"
  local ledger_identity_variable="$3"
  if ! validate_source_bundle_shape; then
    return 1
  fi
  /usr/bin/touch "$ledger"
  /usr/sbin/chown 0:0 "$ledger"
  /bin/chmod 0600 "$ledger"
  printf -v "$ledger_identity_variable" '%s' "$(/usr/bin/stat -f '%d:%i' "$ledger")"

  stage_directory "$root" "$ledger" "Bangbang.app"
  local relative
  for relative in "${bundle_directories[@]}"; do
    stage_directory "$root" "$ledger" "Bangbang.app/$relative"
  done
  for relative in "${bundle_files[@]}"; do
    local mode=0644
    case "$relative" in
      "Contents/MacOS/bangbang" \
        | "Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker")
        mode=0755
        ;;
      "Contents/Helpers/BangbangWorker.app/Contents/Resources/elevated-bootstrap-probe.enabled")
        mode=0600
        ;;
    esac
    stage_file "$root" "$ledger" "$bundle/$relative" "Bangbang.app/$relative" "$mode"
  done
  stage_directory "$root" "$ledger" "usr"
  stage_directory "$root" "$ledger" "usr/lib"
  stage_file "$root" "$ledger" "/usr/lib/dyld" "usr/lib/dyld" 0755

  if ! /usr/bin/cmp -s /usr/lib/dyld "$root/usr/lib/dyld" \
    || ! /usr/bin/codesign --verify --strict "$root/usr/lib/dyld" >/dev/null 2>&1 \
    || ! /usr/bin/codesign --verify --deep --strict "$root/Bangbang.app" >/dev/null 2>&1; then
    return 1
  fi
}

validate_staged_root() {
  local root="$1"
  local root_identity="$2"
  local ledger="$3"
  local ledger_identity="$4"
  if [[ -z "$root" || ! -d "$root" || -L "$root" \
    || "$(/usr/bin/stat -f '%d:%i' "$root" 2>/dev/null || true)" != "$root_identity" \
    || "$(/usr/bin/stat -f '%u:%g:%HT:%Lp' "$root" 2>/dev/null || true)" != "0:0:Directory:700" ]]; then
    return 1
  fi
  if [[ ! -f "$ledger" || -L "$ledger" \
    || "$(/usr/bin/stat -f '%d:%i' "$ledger" 2>/dev/null || true)" != "$ledger_identity" \
    || "$(/usr/bin/stat -f '%u:%g:%HT:%Lp' "$ledger" 2>/dev/null || true)" != "0:0:Regular File:600" ]]; then
    return 1
  fi

  local lines=()
  local relative
  local identity
  local kind
  local seen="|"
  while IFS=$'\t' read -r relative identity kind; do
    if ! is_staged_relative "$relative" \
      || [[ "$kind" != "file" && "$kind" != "directory" ]]; then
      return 1
    fi
    case "$seen" in
      *"|$relative|"*) return 1 ;;
    esac
    seen="${seen}${relative}|"
    lines+=("$relative"$'\t'"$identity"$'\t'"$kind")
  done < "$ledger"
  if [[ "${#lines[@]}" -ne 20 ]]; then
    return 1
  fi

  local line
  local path
  for line in "${lines[@]}"; do
    IFS=$'\t' read -r relative identity kind <<< "$line"
    path="$root/$relative"
    if [[ -L "$path" \
      || "$(/usr/bin/stat -f '%d:%i:%u:%g:%Lp' "$path" 2>/dev/null || true)" != "$identity" ]]; then
      return 1
    fi
    if [[ "$kind" == "file" && ! -f "$path" ]] \
      || [[ "$kind" == "directory" && ! -d "$path" ]]; then
      return 1
    fi
    local links
    links="$(/usr/bin/stat -f '%l' "$path" 2>/dev/null || true)"
    if [[ "$kind" == "file" && "$links" != "1" ]] \
      || [[ "$kind" == "directory" && ("$links" == "" || "$links" -lt 2) ]]; then
      return 1
    fi
  done

  local count
  count="$(/usr/bin/find -x "$root" -mindepth 1 -print | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
  if [[ "$count" != "20" ]] \
    || ! validate_source_bundle_shape \
    || ! /usr/bin/cmp -s /usr/lib/dyld "$root/usr/lib/dyld" \
    || ! /usr/bin/codesign --verify --strict "$root/usr/lib/dyld" >/dev/null 2>&1 \
    || ! /usr/bin/codesign --verify --deep --strict "$root/Bangbang.app" >/dev/null 2>&1; then
    return 1
  fi
  return 0
}

cleanup_staged_root() {
  local root="$1"
  local root_identity="$2"
  local ledger="$3"
  local ledger_identity="$4"
  if [[ -z "$root" ]]; then
    return 0
  fi
  if ! validate_staged_root "$root" "$root_identity" "$ledger" "$ledger_identity"; then
    return 1
  fi

  local lines=()
  local relative
  local identity
  local kind
  while IFS=$'\t' read -r relative identity kind; do
    lines+=("$relative"$'\t'"$identity"$'\t'"$kind")
  done < "$ledger"

  local index
  local path
  for ((index = ${#lines[@]} - 1; index >= 0; index--)); do
    IFS=$'\t' read -r relative identity kind <<< "${lines[index]}"
    path="$root/$relative"
    if [[ "$kind" == "file" ]]; then
      /bin/unlink "$path" || return 1
    else
      /bin/rmdir "$path" || return 1
    fi
  done
  /bin/unlink "$ledger" || return 1
  cleanup_directory "$root" "$root_identity"
}

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

cleanup_symlink() {
  local path="$1"
  local target="$2"
  local identity="$3"
  if [[ ! -L "$path" \
    || "$(/usr/bin/readlink "$path")" != "$target" \
    || "$(/usr/bin/stat -f '%d:%i' "$path" 2>/dev/null || true)" != "$identity" \
    || "$(/usr/bin/stat -f '%u:%g' "$path" 2>/dev/null || true)" != "0:0" ]]; then
    return 1
  fi
  /bin/unlink "$path"
}

cleanup() {
  local prior_status=$?
  trap - EXIT
  trap '' INT TERM HUP
  local cleanup_status=0
  if [[ -n "$symlink_root" ]]; then
    if [[ -n "$symlink_identity" ]]; then
      cleanup_symlink "$symlink_root" "$symlink_target" "$symlink_identity" \
        || cleanup_status=1
    elif [[ -n "$symlink_placeholder_identity" ]]; then
      cleanup_directory "$symlink_root" "$symlink_placeholder_identity" \
        || cleanup_status=1
    else
      cleanup_status=1
    fi
  fi
  if [[ -n "$workspace" && -e "$workspace" ]]; then
    local workspace_current
    local workspace_ownership
    local workspace_shape
    workspace_current="$(/usr/bin/stat -f '%d:%i' "$workspace" 2>/dev/null || true)"
    workspace_ownership="$(/usr/bin/stat -f '%u:%g' "$workspace" 2>/dev/null || true)"
    workspace_shape="$(/usr/bin/stat -f '%HT:%Lp' "$workspace" 2>/dev/null || true)"
    if [[ -d "$workspace" && ! -L "$workspace" \
      && "$workspace_current" == "$workspace_identity" \
      && "$workspace_ownership" == "0:0" \
      && "$workspace_shape" == "Directory:700" ]]; then
      for output in \
        "$workspace/case-a" \
        "$workspace/case-b" \
        "$workspace/inherited-case-a" \
        "$workspace/inherited-case-b"; do
        if [[ -f "$output" && ! -L "$output" ]]; then
          /bin/unlink "$output" || cleanup_status=1
        fi
      done
    else
      cleanup_status=1
    fi
  fi
  cleanup_staged_root \
    "$inherited_root" \
    "$inherited_root_identity" \
    "$inherited_ledger" \
    "$inherited_ledger_identity" \
    || cleanup_status=1
  cleanup_staged_root \
    "$inherited_root_a" \
    "$inherited_root_a_identity" \
    "$inherited_ledger_a" \
    "$inherited_ledger_a_identity" \
    || cleanup_status=1
  cleanup_staged_root \
    "$inherited_root_b" \
    "$inherited_root_b_identity" \
    "$inherited_ledger_b" \
    "$inherited_ledger_b_identity" \
    || cleanup_status=1
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
create_private_directory "/private/var/root/bangbang-elevated-work.XXXXXXXX" \
  workspace workspace_identity

invoke() {
  local root="$1"
  local uid="$2"
  local gid="$3"
  local mode="$4"
  /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin \
    "$launcher" \
    --bangbang-internal-elevated-bootstrap-probe-v2 \
    --root "$root" \
    --target-uid "$uid" \
    --target-gid "$gid" \
    --mode "$mode" \
    --
}

invoke_inherited() {
  local root="$1"
  local root_identity="$2"
  local ledger="$3"
  local ledger_identity="$4"
  if ! validate_staged_root "$root" "$root_identity" "$ledger" "$ledger_identity"; then
    echo "status: elevated bootstrap blocked stage=validate-staged-bundle error=invalid-input"
    return 3
  fi
  invoke "$root" 0 0 inherited-root
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
    /usr/bin/printf 'bangbang elevated bootstrap proof: case failed mode=%s status=%s\n' \
      "$mode" "$status" >&2
    exit 1
  fi
}

assert_inherited_case() {
  local root="$1"
  local root_identity="$2"
  local ledger="$3"
  local ledger_identity="$4"
  local expected_status="$5"
  local expected_output="$6"
  local output
  local status
  set +e
  output="$(invoke_inherited "$root" "$root_identity" "$ledger" "$ledger_identity" 2>&1)"
  status=$?
  set -e
  if [[ "$status" -ne "$expected_status" || "$output" != "$expected_output" ]]; then
    /usr/bin/printf 'bangbang elevated bootstrap proof: inherited case failed status=%s\n' \
      "$status" >&2
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
assert_case "$probe_root" 0 0 hvf-control 0 \
  "status: elevated bootstrap hvf-control complete"

create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  inherited_root inherited_root_identity
inherited_ledger="$workspace/inherited-ledger"
stage_inherited_root "$inherited_root" "$inherited_ledger" inherited_ledger_identity

expected_inherited_block="status: elevated bootstrap blocked stage=worker-bootstrap error=other"
for _ in 1 2 3; do
  assert_inherited_case \
    "$inherited_root" \
    "$inherited_root_identity" \
    "$inherited_ledger" \
    "$inherited_ledger_identity" \
    3 \
    "$expected_inherited_block"
done

invalid_staged="status: elevated bootstrap blocked stage=validate-staged-bundle error=invalid-input"
staged_dyld="$inherited_root/usr/lib/dyld"
saved_dyld="$workspace/staged-dyld-original"

/bin/chmod 0775 "$staged_dyld"
assert_inherited_case \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity" \
  3 \
  "$invalid_staged"
/bin/chmod 0755 "$staged_dyld"

/bin/mv "$staged_dyld" "$saved_dyld"
assert_inherited_case \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity" \
  3 \
  "$invalid_staged"
/bin/mv "$saved_dyld" "$staged_dyld"

/bin/mv "$staged_dyld" "$saved_dyld"
/bin/ln -s /usr/lib/dyld "$staged_dyld"
staged_symlink_identity="$(/usr/bin/stat -f '%d:%i' "$staged_dyld")"
assert_inherited_case \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity" \
  3 \
  "$invalid_staged"
cleanup_symlink "$staged_dyld" /usr/lib/dyld "$staged_symlink_identity"
/bin/mv "$saved_dyld" "$staged_dyld"

/bin/mv "$staged_dyld" "$saved_dyld"
/bin/cp -X /usr/lib/dyld "$staged_dyld"
/usr/sbin/chown 0:0 "$staged_dyld"
/bin/chmod 0755 "$staged_dyld"
replacement_dyld_identity="$(/usr/bin/stat -f '%d:%i' "$staged_dyld")"
assert_inherited_case \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity" \
  3 \
  "$invalid_staged"
if [[ ! -f "$staged_dyld" || -L "$staged_dyld" \
  || "$(/usr/bin/stat -f '%d:%i:%u:%g:%Lp:%l' "$staged_dyld")" \
    != "$replacement_dyld_identity:0:0:755:1" ]] \
  || ! /usr/bin/cmp -s /usr/lib/dyld "$staged_dyld"; then
  echo "bangbang elevated bootstrap proof: staged replacement changed" >&2
  exit 1
fi
/bin/unlink "$staged_dyld"
/bin/mv "$saved_dyld" "$staged_dyld"

staged_worker="$inherited_root/Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker"
saved_worker="$workspace/staged-worker-original"
/bin/mv "$staged_worker" "$saved_worker"
assert_inherited_case \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity" \
  3 \
  "$invalid_staged"
/bin/mv "$saved_worker" "$staged_worker"

unexpected_entry="$inherited_root/unexpected-entry"
/usr/bin/touch "$unexpected_entry"
/usr/sbin/chown 0:0 "$unexpected_entry"
/bin/chmod 0600 "$unexpected_entry"
unexpected_identity="$(/usr/bin/stat -f '%d:%i' "$unexpected_entry")"
assert_inherited_case \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity" \
  3 \
  "$invalid_staged"
if [[ ! -f "$unexpected_entry" || -L "$unexpected_entry" \
  || "$(/usr/bin/stat -f '%d:%i:%u:%g:%Lp:%l' "$unexpected_entry")" \
    != "$unexpected_identity:0:0:600:1" ]]; then
  echo "bangbang elevated bootstrap proof: unexpected entry changed" >&2
  exit 1
fi
/bin/unlink "$unexpected_entry"

if ! validate_staged_root \
  "$inherited_root" \
  "$inherited_root_identity" \
  "$inherited_ledger" \
  "$inherited_ledger_identity"; then
  echo "bangbang elevated bootstrap proof: staging restoration failed" >&2
  exit 1
fi

/bin/chmod 0770 "$probe_root"
assert_case "$probe_root" 0 0 control 1 \
  "bangbang launcher: invalid production launch policy"
/bin/chmod 0700 "$probe_root"

create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  symlink_root symlink_placeholder_identity
symlink_target="$probe_root"
trap '' INT TERM HUP
cleanup_directory "$symlink_root" "$symlink_placeholder_identity"
/bin/ln -s "$probe_root" "$symlink_root"
symlink_placeholder_identity=""
symlink_identity="$(/usr/bin/stat -f '%d:%i' "$symlink_root")"
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
set +e
symlink_output="$(invoke "$symlink_root" 0 0 control 2>&1)"
symlink_status=$?
set -e
if [[ "$symlink_status" -ne 1 || "$symlink_output" != "bangbang launcher: invalid production launch policy" ]]; then
  echo "bangbang elevated bootstrap proof: symlink case failed" >&2
  exit 1
fi
cleanup_symlink "$symlink_root" "$symlink_target" "$symlink_identity"
symlink_root=""
symlink_target=""
symlink_identity=""

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

create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  inherited_root_a inherited_root_a_identity
create_private_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  inherited_root_b inherited_root_b_identity
inherited_ledger_a="$workspace/inherited-ledger-a"
inherited_ledger_b="$workspace/inherited-ledger-b"
stage_inherited_root \
  "$inherited_root_a" \
  "$inherited_ledger_a" \
  inherited_ledger_a_identity
stage_inherited_root \
  "$inherited_root_b" \
  "$inherited_ledger_b" \
  inherited_ledger_b_identity

set +e
invoke_inherited \
  "$inherited_root_a" \
  "$inherited_root_a_identity" \
  "$inherited_ledger_a" \
  "$inherited_ledger_a_identity" \
  > "$workspace/inherited-case-a" 2>&1 &
inherited_pid_a=$!
invoke_inherited \
  "$inherited_root_b" \
  "$inherited_root_b_identity" \
  "$inherited_ledger_b" \
  "$inherited_ledger_b_identity" \
  > "$workspace/inherited-case-b" 2>&1 &
inherited_pid_b=$!
wait "$inherited_pid_a"
inherited_status_a=$?
wait "$inherited_pid_b"
inherited_status_b=$?
set -e
inherited_output_a="$(<"$workspace/inherited-case-a")"
inherited_output_b="$(<"$workspace/inherited-case-b")"
if [[ "$inherited_status_a" -ne 3 || "$inherited_status_b" -ne 3 \
  || "$inherited_output_a" != "$expected_inherited_block" \
  || "$inherited_output_b" != "$expected_inherited_block" ]]; then
  echo "bangbang elevated bootstrap proof: inherited concurrency case failed" >&2
  exit 1
fi
/bin/unlink "$workspace/inherited-case-a"
/bin/unlink "$workspace/inherited-case-b"

echo "result: inherited-root-worker=blocked stage=worker-bootstrap error=other controls=success cleanup=exact"
