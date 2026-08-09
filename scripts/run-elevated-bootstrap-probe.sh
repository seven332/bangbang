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
grant_marker="$worker_bundle/Contents/Resources/grant-integration-probe.enabled"
runtime_marker="$worker_bundle/Contents/Resources/target-runtime-grant-probe.enabled"
for entry in "$launcher" "$worker" "$marker" "$grant_marker" "$runtime_marker"; do
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
runtime_root=""
runtime_root_identity=""
runtime_retain_root=""
runtime_retain_root_identity=""
runtime_unmapped_root=""
runtime_unmapped_root_identity=""
runtime_concurrent_root_a=""
runtime_concurrent_root_a_identity=""
runtime_concurrent_root_b=""
runtime_concurrent_root_b_identity=""
runtime_workspace=""
runtime_workspace_identity=""
runtime_ledger=""
runtime_ledger_identity=""
runtime_retain_workspace=""
runtime_retain_workspace_identity=""
runtime_retain_ledger=""
runtime_retain_ledger_identity=""
runtime_unmapped_workspace=""
runtime_unmapped_workspace_identity=""
runtime_unmapped_ledger=""
runtime_unmapped_ledger_identity=""
runtime_concurrent_workspace_a=""
runtime_concurrent_workspace_a_identity=""
runtime_concurrent_ledger_a=""
runtime_concurrent_ledger_a_identity=""
runtime_concurrent_workspace_b=""
runtime_concurrent_workspace_b_identity=""
runtime_concurrent_ledger_b=""
runtime_concurrent_ledger_b_identity=""
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
unmapped_runtime_result="unmeasured"
cleanup_return=false

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
  "Contents/Helpers/BangbangWorker.app/Contents/Resources/grant-integration-probe.enabled"
  "Contents/Helpers/BangbangWorker.app/Contents/Resources/target-runtime-grant-probe.enabled"
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
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/Resources/grant-integration-probe.enabled" \
      | "Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/Resources/target-runtime-grant-probe.enabled" \
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
      "Contents/Helpers/BangbangWorker.app/Contents/Resources/elevated-bootstrap-probe.enabled" \
        | "Contents/Helpers/BangbangWorker.app/Contents/Resources/grant-integration-probe.enabled" \
        | "Contents/Helpers/BangbangWorker.app/Contents/Resources/target-runtime-grant-probe.enabled")
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
  if [[ "${#lines[@]}" -ne 22 ]]; then
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
  if [[ "$count" != "22" ]] \
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

create_owned_directory() {
  local pattern="$1"
  local uid="$2"
  local gid="$3"
  local path_variable="$4"
  local identity_variable="$5"
  local path
  trap '' INT TERM HUP
  path="$(/usr/bin/mktemp -d "$pattern")"
  /bin/chmod 0700 "$path"
  /usr/sbin/chown "$uid:$gid" "$path"
  printf -v "$path_variable" '%s' "$path"
  printf -v "$identity_variable" '%s' "$(/usr/bin/stat -f '%d:%i' "$path")"
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
}

is_runtime_relative() {
  case "$1" in
    read.input | write.output | authorized-directory | bangbang-grant-probe-outside \
      | grant-manifest.json)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

record_runtime_entry() {
  local root="$1"
  local ledger="$2"
  local relative="$3"
  local kind="$4"
  if ! is_runtime_relative "$relative"; then
    return 1
  fi
  local identity
  identity="$(/usr/bin/stat -f '%d:%i:%u:%g:%Lp' "$root/$relative")"
  /usr/bin/printf '%s\t%s\t%s\n' "$relative" "$identity" "$kind" >> "$ledger"
}

create_runtime_workspace() {
  local uid="$1"
  local gid="$2"
  local path_variable="$3"
  local identity_variable="$4"
  local ledger_variable="$5"
  local ledger_identity_variable="$6"
  local path
  local ledger
  create_private_directory \
    "/private/tmp/bangbang-elevated-runtime.XXXXXXXX" \
    "$path_variable" \
    "$identity_variable"
  path="${!path_variable}"
  ledger="$workspace/${path_variable}-ledger"
  /usr/bin/touch "$ledger"
  /usr/sbin/chown 0:0 "$ledger"
  /bin/chmod 0600 "$ledger"
  printf -v "$ledger_variable" '%s' "$ledger"
  printf -v "$ledger_identity_variable" '%s' "$(/usr/bin/stat -f '%d:%i' "$ledger")"

  /usr/bin/printf 'bangbang-grant-read-target-runtime\n' > "$path/read.input"
  /usr/bin/printf '%36s\n' '' | /usr/bin/tr ' ' '?' > "$path/write.output"
  /bin/mkdir "$path/authorized-directory"
  /usr/bin/printf 'outside-authority\n' > "$path/bangbang-grant-probe-outside"
  /usr/bin/printf \
    '{"version":1,"grants":[{"id":"probe-read-target-runtime","role":"kernel-image","access":"read-only","source":"%s"},{"id":"probe-write-target-runtime","role":"logger-sink","access":"write-only","source":"%s"},{"id":"probe-dir-target-runtime","role":"api-socket-directory","access":"create-children","source":"%s"}]}\n' \
    "$path/read.input" \
    "$path/write.output" \
    "$path/authorized-directory" \
    > "$path/grant-manifest.json"
  /usr/sbin/chown "$uid:$gid" \
    "$path/read.input" \
    "$path/write.output" \
    "$path/authorized-directory" \
    "$path/bangbang-grant-probe-outside" \
    "$path/grant-manifest.json"
  /bin/chmod 0600 \
    "$path/read.input" \
    "$path/write.output" \
    "$path/bangbang-grant-probe-outside" \
    "$path/grant-manifest.json"
  /bin/chmod 0700 "$path/authorized-directory"
  record_runtime_entry "$path" "$ledger" read.input file
  record_runtime_entry "$path" "$ledger" write.output file
  record_runtime_entry "$path" "$ledger" authorized-directory directory
  record_runtime_entry "$path" "$ledger" bangbang-grant-probe-outside file
  record_runtime_entry "$path" "$ledger" grant-manifest.json file
  # Publish only traversal through the complete, identity-ledgered fixture.
  # The outer directory remains root-owned, so the target cannot rename or add
  # fixed entries; authority comes solely from the exact target-owned children.
  /bin/chmod 0711 "$path"
}

validate_runtime_workspace() {
  local path="$1"
  local identity="$2"
  local ledger="$3"
  local ledger_identity="$4"
  local uid="$5"
  local gid="$6"
  if [[ -z "$path" || ! -d "$path" || -L "$path" \
    || "$(/usr/bin/stat -f '%d:%i' "$path" 2>/dev/null || true)" != "$identity" \
    || "$(/usr/bin/stat -f '%u:%g:%HT:%Lp' "$path" 2>/dev/null || true)" \
      != "0:0:Directory:711" \
    || ! -f "$ledger" || -L "$ledger" \
    || "$(/usr/bin/stat -f '%d:%i:%u:%g:%HT:%Lp' "$ledger" 2>/dev/null || true)" \
      != "$ledger_identity:0:0:Regular File:600" ]]; then
    return 1
  fi
  local entries=0
  local seen="|"
  local relative
  local entry_identity
  local kind
  local entry_path
  while IFS=$'\t' read -r relative entry_identity kind; do
    if ! is_runtime_relative "$relative" \
      || [[ "$kind" != "file" && "$kind" != "directory" ]]; then
      return 1
    fi
    case "$seen" in
      *"|$relative|"*) return 1 ;;
    esac
    seen="${seen}${relative}|"
    entry_path="$path/$relative"
    if [[ -L "$entry_path" \
      || "$(/usr/bin/stat -f '%d:%i:%u:%g:%Lp' "$entry_path" 2>/dev/null || true)" \
        != "$entry_identity" ]]; then
      return 1
    fi
    if [[ "$kind" == "file" && ! -f "$entry_path" ]] \
      || [[ "$kind" == "directory" && ! -d "$entry_path" ]]; then
      return 1
    fi
    entries="$((entries + 1))"
  done < "$ledger"
  if [[ "$entries" -ne 5 ]]; then
    return 1
  fi
  local child="$path/authorized-directory/bangbang-grant-target-runtime.out"
  local child_count=0
  if [[ -e "$child" || -L "$child" ]]; then
    if [[ ! -f "$child" || -L "$child" \
      || "$(/usr/bin/stat -f '%u:%g:%Lp:%l' "$child" 2>/dev/null || true)" \
        != "$uid:$gid:600:1" ]]; then
      return 1
    fi
    child_count=1
  fi
  local count
  count="$(/usr/bin/find -x "$path" -mindepth 1 -print | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
  [[ "$count" -eq "$((5 + child_count))" ]]
}

cleanup_owned_directory() {
  local path="$1"
  local identity="$2"
  local uid="$3"
  local gid="$4"
  if [[ -z "$path" ]]; then
    return 0
  fi
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    return 0
  fi
  if [[ ! -d "$path" || -L "$path" \
    || "$(/usr/bin/stat -f '%d:%i' "$path" 2>/dev/null || true)" != "$identity" \
    || "$(/usr/bin/stat -f '%u:%g:%HT:%Lp' "$path" 2>/dev/null || true)" \
      != "$uid:$gid:Directory:700" ]]; then
    return 1
  fi
  /bin/rmdir "$path"
}

cleanup_runtime_workspace() {
  local path="$1"
  local identity="$2"
  local ledger="$3"
  local ledger_identity="$4"
  local uid="$5"
  local gid="$6"
  if [[ -z "$path" ]]; then
    return 0
  fi
  if ! validate_runtime_workspace \
    "$path" "$identity" "$ledger" "$ledger_identity" "$uid" "$gid"; then
    return 1
  fi
  local child="$path/authorized-directory/bangbang-grant-target-runtime.out"
  if [[ -e "$child" ]]; then
    /bin/unlink "$child" || return 1
  fi
  /bin/unlink "$path/read.input" || return 1
  /bin/unlink "$path/write.output" || return 1
  /bin/rmdir "$path/authorized-directory" || return 1
  /bin/unlink "$path/bangbang-grant-probe-outside" || return 1
  /bin/unlink "$path/grant-manifest.json" || return 1
  /bin/unlink "$ledger" || return 1
  /bin/chmod 0700 "$path" || return 1
  cleanup_directory "$path" "$identity"
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
        "$workspace/runtime-case-a" \
        "$workspace/runtime-case-b" \
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
  cleanup_runtime_workspace \
    "$runtime_workspace" \
    "$runtime_workspace_identity" \
    "$runtime_ledger" \
    "$runtime_ledger_identity" \
    "$target_uid" \
    "$target_gid" \
    || cleanup_status=1
  cleanup_runtime_workspace \
    "$runtime_retain_workspace" \
    "$runtime_retain_workspace_identity" \
    "$runtime_retain_ledger" \
    "$runtime_retain_ledger_identity" \
    0 \
    0 \
    || cleanup_status=1
  cleanup_runtime_workspace \
    "$runtime_unmapped_workspace" \
    "$runtime_unmapped_workspace_identity" \
    "$runtime_unmapped_ledger" \
    "$runtime_unmapped_ledger_identity" \
    2147483647 \
    2147483647 \
    || cleanup_status=1
  cleanup_runtime_workspace \
    "$runtime_concurrent_workspace_a" \
    "$runtime_concurrent_workspace_a_identity" \
    "$runtime_concurrent_ledger_a" \
    "$runtime_concurrent_ledger_a_identity" \
    "$target_uid" \
    "$target_gid" \
    || cleanup_status=1
  cleanup_runtime_workspace \
    "$runtime_concurrent_workspace_b" \
    "$runtime_concurrent_workspace_b_identity" \
    "$runtime_concurrent_ledger_b" \
    "$runtime_concurrent_ledger_b_identity" \
    "$target_uid" \
    "$target_gid" \
    || cleanup_status=1
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
  cleanup_owned_directory \
    "$runtime_root" "$runtime_root_identity" "$target_uid" "$target_gid" \
    || cleanup_status=1
  cleanup_owned_directory \
    "$runtime_retain_root" "$runtime_retain_root_identity" 0 0 \
    || cleanup_status=1
  cleanup_owned_directory \
    "$runtime_unmapped_root" "$runtime_unmapped_root_identity" 2147483647 2147483647 \
    || cleanup_status=1
  cleanup_owned_directory \
    "$runtime_concurrent_root_a" \
    "$runtime_concurrent_root_a_identity" \
    "$target_uid" \
    "$target_gid" \
    || cleanup_status=1
  cleanup_owned_directory \
    "$runtime_concurrent_root_b" \
    "$runtime_concurrent_root_b_identity" \
    "$target_uid" \
    "$target_gid" \
    || cleanup_status=1
  cleanup_directory "$workspace" "$workspace_identity" || cleanup_status=1
  if [[ "$cleanup_status" -ne 0 ]]; then
    echo "bangbang elevated bootstrap proof: exact cleanup failed" >&2
    exit 5
  fi
  if [[ "$cleanup_return" == true ]]; then
    return "$prior_status"
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

invoke_runtime() {
  local root="$1"
  local uid="$2"
  local gid="$3"
  local mode="$4"
  local runtime_workspace_path="$5"
  local fault="${6:-}"
  local elevated_args=(
    --bangbang-internal-elevated-bootstrap-probe-v2
    --root "$root"
    --target-uid "$uid"
    --target-gid "$gid"
    --mode "$mode"
  )
  if [[ -n "$fault" ]]; then
    elevated_args+=(--fault "$fault")
  fi
  /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin \
    "$launcher" \
    "${elevated_args[@]}" \
    -- \
    --bangbang-grant-manifest "$runtime_workspace_path/grant-manifest.json" \
    -- \
    --bangbang-internal-grant-probe-v1 target-runtime
}

validate_empty_runtime_root() {
  local root="$1"
  local identity="$2"
  local uid="$3"
  local gid="$4"
  if [[ ! -d "$root" || -L "$root" \
    || "$(/usr/bin/stat -f '%d:%i:%u:%g:%HT:%Lp' "$root" 2>/dev/null || true)" \
      != "$identity:$uid:$gid:Directory:700" ]]; then
    return 1
  fi
  local count
  count="$(/usr/bin/find -x "$root" -mindepth 1 -print | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
  [[ "$count" -eq 0 ]]
}

assert_runtime_objects() {
  local root="$1"
  local root_identity="$2"
  local runtime_workspace_path="$3"
  local runtime_workspace_object="$4"
  local ledger="$5"
  local ledger_identity="$6"
  local uid="$7"
  local gid="$8"
  local write_state="$9"
  if ! validate_empty_runtime_root "$root" "$root_identity" "$uid" "$gid" \
    || ! validate_runtime_workspace \
      "$runtime_workspace_path" \
      "$runtime_workspace_object" \
      "$ledger" \
      "$ledger_identity" \
      "$uid" \
      "$gid" \
    || [[ "$(<"$runtime_workspace_path/read.input")" \
      != "bangbang-grant-read-target-runtime" ]] \
    || [[ "$(<"$runtime_workspace_path/bangbang-grant-probe-outside")" \
      != "outside-authority" ]] \
    || [[ -e "$runtime_workspace_path/authorized-directory/bangbang-grant-target-runtime.out" \
      || -L "$runtime_workspace_path/authorized-directory/bangbang-grant-target-runtime.out" ]]; then
    echo "bangbang elevated bootstrap proof: runtime object validation failed" >&2
    exit 1
  fi
  local actual_write
  actual_write="$(<"$runtime_workspace_path/write.output")"
  case "$write_state" in
    initial)
      if [[ "$actual_write" != "????????????????????????????????????" ]]; then
        echo "bangbang elevated bootstrap proof: runtime output changed before grant use" >&2
        exit 1
      fi
      ;;
    complete)
      if [[ "$actual_write" != "bangbang-grant-write-target-runtime" ]]; then
        echo "bangbang elevated bootstrap proof: runtime output was not committed" >&2
        exit 1
      fi
      ;;
    either)
      if [[ "$actual_write" != "????????????????????????????????????" \
        && "$actual_write" != "bangbang-grant-write-target-runtime" ]]; then
        echo "bangbang elevated bootstrap proof: runtime output is invalid" >&2
        exit 1
      fi
      ;;
    *)
      return 1
      ;;
  esac
}

assert_runtime_output_redacted() {
  local output="$1"
  local root="$2"
  local runtime_workspace_path="$3"
  local sensitive
  for sensitive in \
    "$root" \
    "$runtime_workspace_path" \
    probe-read-target-runtime \
    probe-write-target-runtime \
    probe-dir-target-runtime \
    bangbang-grant-read-target-runtime \
    bangbang-grant-write-target-runtime; do
    if [[ "$output" == *"$sensitive"* ]]; then
      echo "bangbang elevated bootstrap proof: runtime diagnostics leaked fixture data" >&2
      exit 1
    fi
  done
}

output_has_exact_line() {
  local output="$1"
  local expected="$2"
  [[ $'\n'"$output"$'\n' == *$'\n'"$expected"$'\n'* ]]
}

assert_runtime_case() {
  local root="$1"
  local root_identity="$2"
  local runtime_workspace_path="$3"
  local runtime_workspace_object="$4"
  local ledger="$5"
  local ledger_identity="$6"
  local uid="$7"
  local gid="$8"
  local mode="$9"
  local expected_status="${10}"
  local expected_line="${11}"
  local write_state="${12}"
  local fault="${13:-}"
  local output
  local status
  set +e
  output="$(invoke_runtime \
    "$root" \
    "$uid" \
    "$gid" \
    "$mode" \
    "$runtime_workspace_path" \
    "$fault" \
    2>&1)"
  status=$?
  set -e
  assert_runtime_output_redacted "$output" "$root" "$runtime_workspace_path"
  if [[ "$status" -ne "$expected_status" ]]; then
    /usr/bin/printf \
      'bangbang elevated bootstrap proof: runtime case failed mode=%s fault=%s status=%s\n' \
      "$mode" "${fault:-none}" "$status" >&2
    /usr/bin/printf '%s\n' "$output" >&2
    local write_observation="invalid"
    if [[ "$(<"$runtime_workspace_path/write.output")" \
      == "????????????????????????????????????" ]]; then
      write_observation="initial"
    elif [[ "$(<"$runtime_workspace_path/write.output")" \
      == "bangbang-grant-write-target-runtime" ]]; then
      write_observation="complete"
    fi
    local child_observation="absent"
    if [[ -e "$runtime_workspace_path/authorized-directory/bangbang-grant-target-runtime.out" \
      || -L "$runtime_workspace_path/authorized-directory/bangbang-grant-target-runtime.out" ]]; then
      child_observation="present"
    fi
    /usr/bin/printf \
      'runtime-objects: write=%s child=%s\n' \
      "$write_observation" "$child_observation" >&2
    exit 1
  fi
  if [[ "$expected_status" -eq 0 ]]; then
    if [[ "$output" != "$expected_line" ]]; then
      /usr/bin/printf \
        'bangbang elevated bootstrap proof: runtime success output mismatch mode=%s\n' \
        "$mode" >&2
      exit 1
    fi
  elif ! output_has_exact_line "$output" "$expected_line"; then
    /usr/bin/printf \
      'bangbang elevated bootstrap proof: runtime failure output mismatch mode=%s fault=%s\n' \
      "$mode" "${fault:-none}" >&2
    exit 1
  fi
  assert_runtime_objects \
    "$root" \
    "$root_identity" \
    "$runtime_workspace_path" \
    "$runtime_workspace_object" \
    "$ledger" \
    "$ledger_identity" \
    "$uid" \
    "$gid" \
    "$write_state"
}

assert_unmapped_runtime_case() {
  local expected_boundary="$1"
  local output
  local status
  set +e
  output="$(invoke_runtime \
    "$runtime_unmapped_root" \
    2147483647 \
    2147483647 \
    runtime-unmapped \
    "$runtime_unmapped_workspace" \
    2>&1)"
  status=$?
  set -e
  assert_runtime_output_redacted \
    "$output" "$runtime_unmapped_root" "$runtime_unmapped_workspace"
  if [[ "$status" -eq 3 ]] \
    && output_has_exact_line "$output" "$expected_boundary"; then
    unmapped_runtime_result="identity-boundary"
    assert_runtime_objects \
      "$runtime_unmapped_root" \
      "$runtime_unmapped_root_identity" \
      "$runtime_unmapped_workspace" \
      "$runtime_unmapped_workspace_identity" \
      "$runtime_unmapped_ledger" \
      "$runtime_unmapped_ledger_identity" \
      2147483647 \
      2147483647 \
      initial
    return 0
  fi
  /usr/bin/printf \
    'bangbang elevated bootstrap proof: unmapped runtime result was not an allowed exact boundary status=%s\n' \
    "$status" >&2
  /usr/bin/printf '%s\n' "$output" >&2
  exit 1
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

expected_credential_control="status: elevated credential credential-control complete prefix=irreversible identity=target groups=effective-only"
expected_credential_retain_control="status: elevated credential credential-control complete prefix=retained-root identity=initial-and-target groups=initial"
expected_credential_drop="status: elevated credential credential-drop complete stream-eid=snapshot stream-cred=snapshot stream-pid=exact datagram-cred=unsupported datagram-token=changed datagram-pid=exact"
expected_credential_retain="status: elevated credential credential-retain-root complete stream-eid=stable-root stream-cred=stable-root stream-pid=exact datagram-cred=unsupported datagram-token=unchanged datagram-pid=exact"
expected_credential_unmapped="status: elevated credential credential-unmapped complete stream-eid=snapshot stream-cred=snapshot stream-pid=exact datagram-cred=unsupported datagram-token=changed datagram-pid=exact"

assert_case "$probe_root" "$target_uid" "$target_gid" credential-control 0 \
  "$expected_credential_control"
assert_case "$probe_root" 0 0 credential-control 0 \
  "$expected_credential_retain_control"
assert_case "$probe_root" 2147483647 2147483647 credential-control 0 \
  "$expected_credential_control"
for _ in 1 2 3; do
  assert_case "$probe_root" "$target_uid" "$target_gid" credential-drop 0 \
    "$expected_credential_drop"
done
for _ in 1 2 3; do
  assert_case "$probe_root" 0 0 credential-retain-root 0 \
    "$expected_credential_retain"
done
assert_case "$probe_root" 2147483647 2147483647 credential-unmapped 0 \
  "$expected_credential_unmapped"

runtime_drop_semantics="stream-eid=snapshot stream-cred=snapshot stream-pid=exact datagram-cred=unsupported datagram-token=changed datagram-pid=exact"
runtime_retain_semantics="stream-eid=stable-root stream-cred=stable-root stream-pid=exact datagram-cred=unsupported datagram-token=unchanged datagram-pid=exact"
expected_runtime_drop_boundary="status: elevated runtime runtime-drop blocked stage=runtime-namespace error=permission-denied result=namespace-boundary $runtime_drop_semantics"
expected_runtime_retain_boundary="status: elevated runtime runtime-retain-root blocked stage=runtime-namespace error=permission-denied result=namespace-boundary $runtime_retain_semantics"
expected_runtime_unmapped_boundary="status: elevated runtime runtime-unmapped blocked stage=live-identity error=other result=identity-boundary $runtime_drop_semantics"

create_owned_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  "$target_uid" "$target_gid" runtime_root runtime_root_identity
create_runtime_workspace \
  "$target_uid" \
  "$target_gid" \
  runtime_workspace \
  runtime_workspace_identity \
  runtime_ledger \
  runtime_ledger_identity
for _ in 1 2 3; do
  assert_runtime_case \
    "$runtime_root" \
    "$runtime_root_identity" \
    "$runtime_workspace" \
    "$runtime_workspace_identity" \
    "$runtime_ledger" \
    "$runtime_ledger_identity" \
    "$target_uid" \
    "$target_gid" \
    runtime-drop \
    3 \
    "$expected_runtime_drop_boundary" \
    initial
done

create_owned_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  0 0 runtime_retain_root runtime_retain_root_identity
create_runtime_workspace \
  0 \
  0 \
  runtime_retain_workspace \
  runtime_retain_workspace_identity \
  runtime_retain_ledger \
  runtime_retain_ledger_identity
for _ in 1 2 3; do
  assert_runtime_case \
    "$runtime_retain_root" \
    "$runtime_retain_root_identity" \
    "$runtime_retain_workspace" \
    "$runtime_retain_workspace_identity" \
    "$runtime_retain_ledger" \
    "$runtime_retain_ledger_identity" \
    0 \
    0 \
    runtime-retain-root \
    3 \
    "$expected_runtime_retain_boundary" \
    initial
done

for fault_case in \
  "pre-ack:continuation-ack:continuation-boundary:initial" \
  "post-ack:lifecycle-hello:lifecycle-boundary:initial" \
  "namespace:runtime-namespace:namespace-boundary:initial"; do
  IFS=: read -r fault stage result_class write_state <<< "$fault_case"
  expected_fault="status: elevated runtime runtime-drop blocked stage=$stage error=other result=$result_class $runtime_drop_semantics"
  assert_runtime_case \
    "$runtime_root" \
    "$runtime_root_identity" \
    "$runtime_workspace" \
    "$runtime_workspace_identity" \
    "$runtime_ledger" \
    "$runtime_ledger_identity" \
    "$target_uid" \
    "$target_gid" \
    runtime-drop \
    3 \
    "$expected_fault" \
    "$write_state" \
    "$fault"
done

for fault in grant-transfer proceed terminal; do
  assert_runtime_case \
    "$runtime_root" \
    "$runtime_root_identity" \
    "$runtime_workspace" \
    "$runtime_workspace_identity" \
    "$runtime_ledger" \
    "$runtime_ledger_identity" \
    "$target_uid" \
    "$target_gid" \
    runtime-drop \
    3 \
    "$expected_runtime_drop_boundary" \
    initial \
    "$fault"
done

create_owned_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  2147483647 2147483647 runtime_unmapped_root runtime_unmapped_root_identity
create_runtime_workspace \
  2147483647 \
  2147483647 \
  runtime_unmapped_workspace \
  runtime_unmapped_workspace_identity \
  runtime_unmapped_ledger \
  runtime_unmapped_ledger_identity
assert_unmapped_runtime_case "$expected_runtime_unmapped_boundary"

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

set +e
invoke "$concurrent_root_a" "$target_uid" "$target_gid" credential-drop \
  > "$workspace/case-a" 2>&1 &
pid_a=$!
invoke "$concurrent_root_b" "$target_uid" "$target_gid" credential-drop \
  > "$workspace/case-b" 2>&1 &
pid_b=$!
wait "$pid_a"
status_a=$?
wait "$pid_b"
status_b=$?
set -e
output_a="$(<"$workspace/case-a")"
output_b="$(<"$workspace/case-b")"
if [[ "$status_a" -ne 0 || "$status_b" -ne 0 \
  || "$output_a" != "$expected_credential_drop" \
  || "$output_b" != "$expected_credential_drop" ]]; then
  echo "bangbang elevated bootstrap proof: credential concurrency case failed" >&2
  exit 1
fi
/bin/unlink "$workspace/case-a"
/bin/unlink "$workspace/case-b"

create_owned_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  "$target_uid" \
  "$target_gid" \
  runtime_concurrent_root_a \
  runtime_concurrent_root_a_identity
create_owned_directory "/private/var/root/bangbang-elevated-probe.XXXXXXXX" \
  "$target_uid" \
  "$target_gid" \
  runtime_concurrent_root_b \
  runtime_concurrent_root_b_identity
create_runtime_workspace \
  "$target_uid" \
  "$target_gid" \
  runtime_concurrent_workspace_a \
  runtime_concurrent_workspace_a_identity \
  runtime_concurrent_ledger_a \
  runtime_concurrent_ledger_a_identity
create_runtime_workspace \
  "$target_uid" \
  "$target_gid" \
  runtime_concurrent_workspace_b \
  runtime_concurrent_workspace_b_identity \
  runtime_concurrent_ledger_b \
  runtime_concurrent_ledger_b_identity
set +e
invoke_runtime \
  "$runtime_concurrent_root_a" \
  "$target_uid" \
  "$target_gid" \
  runtime-drop \
  "$runtime_concurrent_workspace_a" \
  > "$workspace/runtime-case-a" 2>&1 &
runtime_pid_a=$!
invoke_runtime \
  "$runtime_concurrent_root_b" \
  "$target_uid" \
  "$target_gid" \
  runtime-drop \
  "$runtime_concurrent_workspace_b" \
  > "$workspace/runtime-case-b" 2>&1 &
runtime_pid_b=$!
wait "$runtime_pid_a"
runtime_status_a=$?
wait "$runtime_pid_b"
runtime_status_b=$?
set -e
runtime_output_a="$(<"$workspace/runtime-case-a")"
runtime_output_b="$(<"$workspace/runtime-case-b")"
assert_runtime_output_redacted \
  "$runtime_output_a" "$runtime_concurrent_root_a" "$runtime_concurrent_workspace_a"
assert_runtime_output_redacted \
  "$runtime_output_b" "$runtime_concurrent_root_b" "$runtime_concurrent_workspace_b"
if [[ "$runtime_status_a" -ne 3 || "$runtime_status_b" -ne 3 ]] \
  || ! output_has_exact_line "$runtime_output_a" "$expected_runtime_drop_boundary" \
  || ! output_has_exact_line "$runtime_output_b" "$expected_runtime_drop_boundary"; then
  echo "bangbang elevated bootstrap proof: runtime concurrency case failed" >&2
  exit 1
fi
assert_runtime_objects \
  "$runtime_concurrent_root_a" \
  "$runtime_concurrent_root_a_identity" \
  "$runtime_concurrent_workspace_a" \
  "$runtime_concurrent_workspace_a_identity" \
  "$runtime_concurrent_ledger_a" \
  "$runtime_concurrent_ledger_a_identity" \
  "$target_uid" \
  "$target_gid" \
  initial
assert_runtime_objects \
  "$runtime_concurrent_root_b" \
  "$runtime_concurrent_root_b_identity" \
  "$runtime_concurrent_workspace_b" \
  "$runtime_concurrent_workspace_b_identity" \
  "$runtime_concurrent_ledger_b" \
  "$runtime_concurrent_ledger_b_identity" \
  "$target_uid" \
  "$target_gid" \
  initial
/bin/unlink "$workspace/runtime-case-a"
/bin/unlink "$workspace/runtime-case-b"

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

socket_residue=0
for path in \
  "$probe_root" \
  "$inherited_root" \
  "$inherited_root_a" \
  "$inherited_root_b" \
  "$concurrent_root_a" \
  "$concurrent_root_b" \
  "$runtime_root" \
  "$runtime_retain_root" \
  "$runtime_unmapped_root" \
  "$runtime_concurrent_root_a" \
  "$runtime_concurrent_root_b" \
  "$runtime_workspace" \
  "$runtime_retain_workspace" \
  "$runtime_unmapped_workspace" \
  "$runtime_concurrent_workspace_a" \
  "$runtime_concurrent_workspace_b" \
  "$replacement_root" \
  "$workspace"; do
  if [[ -d "$path" && ! -L "$path" ]]; then
    count="$(/usr/bin/find -x "$path" -type s -print | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
    socket_residue="$((socket_residue + count))"
  fi
done
cleanup_return=true
cleanup

root_residue="$(/usr/bin/find /private/var/root -maxdepth 1 \
  \( -name 'bangbang-elevated-probe.*' -o -name 'bangbang-elevated-work.*' \) \
  -print 2>/dev/null | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
runtime_workspace_residue="$(/usr/bin/find /private/tmp -maxdepth 1 \
  -name 'bangbang-elevated-runtime.*' \
  -print 2>/dev/null | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
launcher_residue="$({ /usr/bin/pgrep -x bangbang-launcher 2>/dev/null || true; } \
  | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
worker_residue="$({ /usr/bin/pgrep -x bangbang 2>/dev/null || true; } \
  | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
if [[ "$socket_residue" -ne 0 || "$root_residue" -ne 0 \
  || "$runtime_workspace_residue" -ne 0 \
  || "$launcher_residue" -ne 0 || "$worker_residue" -ne 0 ]]; then
  echo "bangbang elevated bootstrap proof: final residue scan failed" >&2
  exit 1
fi

echo "result: inherited-root-worker=blocked stage=worker-bootstrap error=other credential-ordinary=complete credential-retained-root=complete-no-drop credential-unmapped=complete runtime-mapped=namespace-boundary runtime-retained-root=namespace-boundary runtime-unmapped=$unmapped_runtime_result grants=unreached lifecycle=hello-start controls=complete cleanup=exact"
echo "observations: stream-eid=snapshot stream-cred=snapshot stream-pid=exact datagram-cred=unsupported datagram-token=changed-or-unchanged datagram-pid=exact"
echo "residue: roots=zero workspaces=zero sockets=zero launchers=zero workers=zero"
echo "nonclaims: target-session=unreached grants=unreached proceed-starting-terminal=unreached api-no-api-real-guest=unmeasured daemon-crash=unmeasured post-drop-guest-hvf=unmeasured public-policy=unchanged chroot=unresolved aggregate-jailer=nonterminal"
