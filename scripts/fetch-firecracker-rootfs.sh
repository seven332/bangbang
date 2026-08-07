#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/fetch-firecracker-rootfs.sh [--format squashfs|ext4] [--ext4-size SIZE]

Fetch and verify the pinned Firecracker arm64 Ubuntu rootfs artifact.

Options:
  --format FORMAT  Output format to prepare. Supported values: squashfs, ext4.
                   Defaults to squashfs.
  --ext4-size SIZE Size for the generated ext4 image. Defaults to 1G.
                   Only valid with --format ext4.
  --direct-boot-init
                   Add a deterministic bangbang direct-rootfs boot init script
                   to the generated ext4 image. The init emits serial markers,
                   writes an optional /dev/vdb marker when that drive exists,
                   and can fetch MMDS when requested by boot args. Only valid
                   with --format ext4.
  -h, --help       Show this help.

Environment:
  BANGBANG_GUEST_ARTIFACTS_DIR  Override the guest artifact cache root.
  BANGBANG_MKFS_EXT4            Override the mkfs.ext4 executable path.
  BANGBANG_RUSTC                Override the rustc executable used to build the
                                static arm64 ID-register report helper.
EOF
}

format="squashfs"
ext4_size="1G"
ext4_size_set=false
direct_boot_init=false
internal_populate_dir=""

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --format)
      shift
      if [[ "$#" -eq 0 ]]; then
        echo "--format requires a value" >&2
        usage >&2
        exit 2
      fi
      format="$1"
      ;;
    --format=*)
      format="${1#--format=}"
      ;;
    --ext4-size)
      shift
      if [[ "$#" -eq 0 ]]; then
        echo "--ext4-size requires a value" >&2
        usage >&2
        exit 2
      fi
      ext4_size="$1"
      ext4_size_set=true
      ;;
    --ext4-size=*)
      ext4_size="${1#--ext4-size=}"
      ext4_size_set=true
      ;;
    --direct-boot-init)
      direct_boot_init=true
      ;;
    --internal-populate-direct)
      if [[ "${BANGBANG_GUEST_POLICY_INTERNAL:-}" != "1" || "$#" -ne 2 ]]; then
        echo "internal rootfs population is available only to the checked artifact policy" >&2
        exit 2
      fi
      shift
      internal_populate_dir="$1"
      ;;
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
  shift
done

case "$format" in
  squashfs | ext4)
    ;;
  *)
    echo "unsupported rootfs format: $format" >&2
    usage >&2
    exit 2
    ;;
esac

if [[ "$format" != "ext4" && "$ext4_size_set" == true ]]; then
  echo "--ext4-size is only valid with --format ext4" >&2
  usage >&2
  exit 2
fi
if [[ "$format" != "ext4" && "$direct_boot_init" == true ]]; then
  echo "--direct-boot-init is only valid with --format ext4" >&2
  usage >&2
  exit 2
fi

if [[ ! "$ext4_size" =~ ^([0-9]+)[KkMmGgTt]?$ ]]; then
  echo "invalid ext4 size: $ext4_size" >&2
  usage >&2
  exit 2
fi
if [[ "${BASH_REMATCH[1]}" =~ ^0+$ ]]; then
  echo "invalid ext4 size: $ext4_size" >&2
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
direct_boot_variant="direct-boot-v109"
extract_dir=""

install_arm64_id_register_report_helper() {
  local helper_source="${repo_root}/scripts/guest/arm64-id-register-report.rs"
  local helper_path="${extract_dir}/bangbang-arm64-id-register-report"
  local rustc_path="${BANGBANG_RUSTC:-rustc}"
  local host_target
  local rust_lld
  local rust_sysroot
  local target_libdir

  if [[ ! -f "$helper_source" ]]; then
    echo "arm64 ID-register report helper source is missing: $helper_source" >&2
    exit 1
  fi
  if ! command -v "$rustc_path" >/dev/null 2>&1; then
    echo "rustc is required to build the arm64 ID-register report helper" >&2
    exit 1
  fi

  target_libdir="$("$rustc_path" --print target-libdir --target aarch64-unknown-linux-musl 2>/dev/null || true)"
  if [[ -z "$target_libdir" || ! -d "$target_libdir" ]] \
    || ! compgen -G "$target_libdir/libcore-*.rlib" >/dev/null; then
    echo "Rust target aarch64-unknown-linux-musl is required; install it with rustup target add aarch64-unknown-linux-musl" >&2
    exit 1
  fi
  rust_sysroot="$("$rustc_path" --print sysroot)"
  host_target="$("$rustc_path" -vV | sed -n 's/^host: //p')"
  rust_lld="$rust_sysroot/lib/rustlib/$host_target/bin/rust-lld"
  if [[ -z "$host_target" || ! -x "$rust_lld" ]]; then
    echo "the active Rust toolchain does not provide rust-lld for the host" >&2
    exit 1
  fi

  echo "building static arm64 ID-register report helper" >&2
  "$rustc_path" \
    "$helper_source" \
    --edition 2024 \
    --target aarch64-unknown-linux-musl \
    --remap-path-prefix "$repo_root=/bangbang" \
    -C linker="$rust_lld" \
    -C link-self-contained=no \
    -C link-arg=--build-id=none \
    -C link-arg=--entry=_start \
    -C opt-level=s \
    -C panic=abort \
    -C link-arg=-static \
    -C link-arg=--strip-all \
    -C relocation-model=static \
    -o "$helper_path"
  chmod 0755 "$helper_path"
}

install_direct_boot_init() {
  local init_path="${extract_dir}/bangbang-direct-rootfs-init"

  cat > "$init_path" <<'EOF'
#!/bin/sh
emit_text() {
  text=$1
  while [ -n "$text" ]; do
    rest=${text#????????????????}
    if [ "$rest" = "$text" ]; then
      printf '%s' "$text"
      text=
    else
      chunk=${text%"$rest"}
      printf '%s' "$chunk"
      text=$rest
    fi
  done
}

emit_line() {
  emit_text "$1"
  printf '\n'
}

mount_if_directory() {
  fs_type=$1
  source=$2
  target=$3
  if [ -d "$target" ]; then
    mount -t "$fs_type" "$source" "$target" 2>/dev/null || true
  fi
}

cmdline_has() {
  case " $cmdline " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

report_block_serial() {
  device=$1
  serial_path="/sys/block/$device/serial"
  if [ ! -r "$serial_path" ]; then
    emit_line BANGBANG_BLOCK_SERIAL_FAIL_NO_SERIAL
    return
  fi

  block_serial=$(cat "$serial_path" 2>/dev/null || true)
  case "$block_serial" in
    ''|*[!0-9]*)
      emit_line BANGBANG_BLOCK_SERIAL_FAIL_INVALID
      return
      ;;
  esac
  if [ "${#block_serial}" -gt 20 ]; then
    emit_line BANGBANG_BLOCK_SERIAL_FAIL_TOO_LONG
    return
  fi

  emit_line BANGBANG_BLOCK_SERIAL_BEGIN
  emit_line "$block_serial"
  emit_line BANGBANG_BLOCK_SERIAL_END
}

report_block_serial_phase() {
  device=$1
  phase=$2
  serial_path="/sys/block/$device/serial"
  if [ ! -r "$serial_path" ]; then
    emit_line "BANGBANG_${phase}_SERIAL_FAIL_NO_SERIAL"
    return 1
  fi

  block_serial=$(cat "$serial_path" 2>/dev/null || true)
  case "$block_serial" in
    ''|*[!0-9]*)
      emit_line "BANGBANG_${phase}_SERIAL_FAIL_INVALID"
      return 1
      ;;
  esac
  if [ "${#block_serial}" -gt 20 ]; then
    emit_line "BANGBANG_${phase}_SERIAL_FAIL_TOO_LONG"
    return 1
  fi

  emit_line "BANGBANG_${phase}_SERIAL_BEGIN"
  emit_line "$block_serial"
  emit_line "BANGBANG_${phase}_SERIAL_END"
}

write_vdb_marker_at_sector() {
  marker=$1
  sector=$2
  if [ -b /dev/vdb ]; then
    printf '%-512s' "$marker" \
      | dd of=/dev/vdb bs=512 seek="$sector" count=1 conv=notrunc 2>/dev/null \
      || true
  fi
}

write_vdb_marker() {
  write_vdb_marker_at_sector "$1" 0
}

write_vda_marker() {
  marker=$1
  if [ -b /dev/vda ]; then
    printf '%-512s' "$marker" \
      | dd of=/dev/vda bs=512 count=1 conv=notrunc 2>/dev/null \
      || true
  fi
}

pci_function_has_identity() {
  function=$1
  expected_device=$2
  function_path="/sys/bus/pci/devices/$function"
  vendor=$(cat "$function_path/vendor" 2>/dev/null || true)
  device=$(cat "$function_path/device" 2>/dev/null || true)
  [ "$vendor" = 0x1af4 ] && [ "$device" = "$expected_device" ]
}

vdb_starts_with_marker() {
  marker=$1
  [ -b /dev/vdb ] || return 1
  actual=$(dd if=/dev/vdb bs=1 count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

vdb_sector_starts_with_marker() {
  marker=$1
  sector=$2
  [ -b /dev/vdb ] || return 1
  actual=$(dd if=/dev/vdb bs=512 skip="$sector" count=1 2>/dev/null \
    | dd bs=1 count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

write_vdb_sector_marker() {
  marker=$1
  sector=$2
  [ -b /dev/vdb ] || return 1
  printf '%-512s' "$marker" \
    | dd of=/dev/vdb bs=512 seek="$sector" count=1 conv=notrunc,fsync 2>/dev/null
}

vdc_starts_with_marker() {
  marker=$1
  [ -b /dev/vdc ] || return 1
  actual=$(dd if=/dev/vdc bs=1 count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

vdd_starts_with_marker() {
  marker=$1
  [ -b /dev/vdd ] || return 1
  actual=$(dd if=/dev/vdd bs=1 count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

storage_block_starts_with_marker() {
  device=$1
  marker=$2
  [ -b "$device" ] || return 1
  actual=$(timeout 2 dd if="$device" bs=1 count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

find_storage_block() {
  marker=$1
  storage_block_device=
  for device in /dev/vd*; do
    [ -b "$device" ] || continue
    if storage_block_starts_with_marker "$device" "$marker"; then
      storage_block_device=$device
      return 0
    fi
  done
  return 1
}

rescan_storage_pci() {
  timeout -k 1 5 sh -c 'printf "1\n" > /sys/bus/pci/rescan'
}

wait_for_storage_block() {
  marker=$1
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    if find_storage_block "$marker"; then
      return 0
    fi
    rescan_storage_pci 2>/dev/null || return 1
    sleep 1
    attempts=$((attempts + 1))
  done
  return 1
}

storage_block_sector_starts_with_marker() {
  device=$1
  marker=$2
  sector=$3
  [ -b "$device" ] || return 1
  actual=$(timeout 2 dd if="$device" bs=512 skip="$sector" count=1 2>/dev/null \
    | dd bs=1 count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

write_storage_block_sector_marker() {
  device=$1
  marker=$2
  sector=$3
  [ -b "$device" ] || return 1
  printf '%-512s' "$marker" \
    | timeout 5 dd of="$device" bs=512 seek="$sector" count=1 conv=notrunc,fsync 2>/dev/null
}

storage_block_pci_path() {
  device=$1
  block_name=${device##*/}
  path=$(readlink -f "/sys/class/block/$block_name/device" 2>/dev/null || true)
  while [ -n "$path" ] && [ "$path" != / ]; do
    if [ -e "$path/remove" ] && [ -r "$path/vendor" ] && [ -r "$path/device" ]; then
      vendor=$(cat "$path/vendor" 2>/dev/null || true)
      pci_device=$(cat "$path/device" 2>/dev/null || true)
      if [ "$vendor" = 0x1af4 ] && [ "$pci_device" = 0x1042 ]; then
        printf '%s\n' "$path"
        return 0
      fi
    fi
    parent=${path%/*}
    [ "$parent" != "$path" ] || break
    path=$parent
  done
  return 1
}

find_storage_pmem() {
  marker=$1
  storage_pmem_device=
  storage_pmem_pci_path=
  storage_pmem_resource=

  for function_path in /sys/bus/pci/devices/*; do
    [ -d "$function_path" ] || continue
    function=${function_path##*/}
    pci_function_has_identity "$function" 0x105b || continue
    for block_path in "$function_path"/virtio*/ndbus*/region*/namespace*/block/*; do
      [ -e "$block_path" ] || continue
      namespace_path=${block_path%/block/*}
      resource=$(cat "$namespace_path/resource" 2>/dev/null || true)
      [ -n "$resource" ] || continue
      device="/dev/${block_path##*/}"
      [ -b "$device" ] || continue
      actual=$(timeout 2 dd if="$device" bs=1 count="${#marker}" 2>/dev/null || true)
      [ "$actual" = "$marker" ] || continue
      storage_pmem_device=$device
      storage_pmem_pci_path=$function_path
      storage_pmem_resource=$resource
      return 0
    done
  done

  return 1
}

wait_for_storage_pmem() {
  marker=$1
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    rescan_storage_pci 2>/dev/null || return 1
    sleep 1
    if find_storage_pmem "$marker"; then
      return 0
    fi
    attempts=$((attempts + 1))
  done
  return 1
}

storage_pmem_marker_at_offset() {
  device=$1
  marker=$2
  offset=$3
  actual=$(timeout 2 dd if="$device" bs=1 skip="$offset" count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

storage_certification_fail() {
  reason=$1
  marker="BANGBANG_STORAGE_CERTIFICATION_FAIL_$reason"
  emit_line "$marker"
  if [ -n "${storage_control_device:-}" ]; then
    for sector in 1 4 6 7 9 15; do
      write_storage_block_sector_marker "$storage_control_device" "$marker" "$sector" || true
    done
    sync "$storage_control_device" 2>/dev/null || sync
  fi
}

wait_for_storage_control_marker() {
  marker=$1
  sector=$2
  attempts=0
  emit_line "BANGBANG_STORAGE_CONTROL_WAIT_${sector}_${storage_control_device##*/}"
  while ! storage_block_sector_starts_with_marker \
    "$storage_control_device" "$marker" "$sector"; do
    if [ "$attempts" -eq 0 ]; then
      observed=$(timeout 2 dd if="$storage_control_device" bs=512 skip="$sector" count=1 2>/dev/null \
        | dd bs=1 count="${#marker}" 2>/dev/null || true)
      emit_line "BANGBANG_STORAGE_CONTROL_OBSERVED_${sector}_$observed"
    fi
    if [ "$attempts" -ge 60 ]; then
      return 1
    fi
    sleep 1
    attempts=$((attempts + 1))
  done
  emit_line "BANGBANG_STORAGE_CONTROL_SEEN_$sector"
}

run_storage_block_round() {
  expected=$1
  written=$2
  phase=$3
  if ! wait_for_storage_block "$expected"; then
    storage_certification_fail "${phase}_BLOCK_RESCAN"
    return 1
  fi
  emit_line "BANGBANG_STORAGE_${phase}_BLOCK_FOUND"
  storage_round_block_device=$storage_block_device
  storage_round_block_pci_path=$(storage_block_pci_path "$storage_round_block_device") || {
    storage_certification_fail "${phase}_BLOCK_IDENTITY"
    return 1
  }
  if [ "$phase" = FIRST ]; then
    storage_first_block_pci_path=$storage_round_block_pci_path
  elif [ "$storage_round_block_pci_path" != "$storage_first_block_pci_path" ]; then
    storage_certification_fail "${phase}_BLOCK_SLOT_REUSE"
    return 1
  fi
  if ! write_storage_block_sector_marker "$storage_round_block_device" "$written" 2 \
    || ! storage_block_sector_starts_with_marker \
      "$storage_round_block_device" "$written" 2; then
    storage_certification_fail "${phase}_BLOCK_IO"
    return 1
  fi
  emit_line "BANGBANG_STORAGE_${phase}_BLOCK_IO"
  if ! printf '1\n' > "$storage_round_block_pci_path/remove" 2>/dev/null; then
    storage_certification_fail "${phase}_BLOCK_REMOVE"
    return 1
  fi
  attempts=0
  while [ -b "$storage_round_block_device" ] && [ "$attempts" -lt 30 ]; do
    sleep 1
    attempts=$((attempts + 1))
  done
  if [ -b "$storage_round_block_device" ]; then
    storage_certification_fail "${phase}_BLOCK_REMOVE_WAIT"
    return 1
  fi
  emit_line "BANGBANG_STORAGE_${phase}_BLOCK_REMOVED"
}

run_storage_pmem_round() {
  expected=$1
  written=$2
  phase=$3
  if ! wait_for_storage_pmem "$expected"; then
    storage_certification_fail "${phase}_PMEM_RESCAN"
    return 1
  fi
  emit_line "BANGBANG_STORAGE_${phase}_PMEM_FOUND"
  storage_round_pmem_device=$storage_pmem_device
  storage_round_pmem_pci_path=$storage_pmem_pci_path
  storage_round_pmem_resource=$storage_pmem_resource
  if [ "$phase" = FIRST ]; then
    storage_first_pmem_pci_path=$storage_round_pmem_pci_path
    storage_first_pmem_resource=$storage_round_pmem_resource
  elif [ "$storage_round_pmem_pci_path" != "$storage_first_pmem_pci_path" ]; then
    storage_certification_fail "${phase}_PMEM_SLOT_REUSE"
    return 1
  elif [ "$storage_round_pmem_resource" != "$storage_first_pmem_resource" ]; then
    storage_certification_fail "${phase}_PMEM_RANGE_REUSE"
    return 1
  fi
  if ! printf '%s' "$written" \
    | timeout 5 dd of="$storage_round_pmem_device" bs=1 seek=4096 conv=notrunc 2>/dev/null \
    || ! timeout 5 sync "$storage_round_pmem_device" 2>/dev/null \
    || ! storage_pmem_marker_at_offset \
      "$storage_round_pmem_device" "$written" 4096; then
    storage_certification_fail "${phase}_PMEM_IO"
    return 1
  fi
  emit_line "BANGBANG_STORAGE_${phase}_PMEM_IO"
  if ! printf '1\n' > "$storage_round_pmem_pci_path/remove" 2>/dev/null; then
    storage_certification_fail "${phase}_PMEM_REMOVE"
    return 1
  fi
  attempts=0
  while [ -b "$storage_round_pmem_device" ] && [ "$attempts" -lt 30 ]; do
    sleep 1
    attempts=$((attempts + 1))
  done
  if [ -b "$storage_round_pmem_device" ]; then
    storage_certification_fail "${phase}_PMEM_REMOVE_WAIT"
    return 1
  fi
  emit_line "BANGBANG_STORAGE_${phase}_PMEM_REMOVED"
}

check_storage_certification() {
  storage_control_device=
  storage_first_block_pci_path=
  storage_first_pmem_pci_path=
  storage_first_pmem_resource=

  if ! cmdline_has root=/dev/vda || ! cmdline_has ro \
    || [ "$(cat /sys/class/block/vda/ro 2>/dev/null || true)" != 1 ]; then
    storage_certification_fail ROOT_READ_ONLY
    return
  fi
  if ! wait_for_storage_block BANGBANG_STORAGE_CONTROL_HOST; then
    storage_certification_fail CONTROL_DISCOVERY
    return
  fi
  storage_control_device=$storage_block_device
  if ! wait_for_storage_block BANGBANG_STORAGE_ASYNC_HOST; then
    storage_certification_fail ASYNC_DISCOVERY
    return
  fi
  storage_async_device=$storage_block_device
  if ! wait_for_storage_block BANGBANG_STORAGE_VHOST_HOST; then
    storage_certification_fail VHOST_DISCOVERY
    return
  fi
  storage_vhost_device=$storage_block_device
  if ! wait_for_storage_pmem BANGBANG_STORAGE_PMEM_HOST; then
    storage_certification_fail PMEM_DISCOVERY
    return
  fi
  storage_startup_pmem_device=$storage_pmem_device

  if [ "$storage_control_device" = "$storage_async_device" ] \
    || [ "$storage_control_device" = "$storage_vhost_device" ] \
    || [ "$storage_async_device" = "$storage_vhost_device" ]; then
    storage_certification_fail BLOCK_IDENTITIES
    return
  fi
  if ! write_storage_block_sector_marker \
      "$storage_control_device" BANGBANG_STORAGE_CONTROL_GUEST 2 \
    || ! write_storage_block_sector_marker \
      "$storage_async_device" BANGBANG_STORAGE_ASYNC_GUEST 2 \
    || ! write_storage_block_sector_marker \
      "$storage_vhost_device" BANGBANG_STORAGE_VHOST_GUEST 2 \
    || ! printf '%s' BANGBANG_STORAGE_PMEM_GUEST \
      | timeout 5 dd of="$storage_startup_pmem_device" bs=1 seek=4096 conv=notrunc 2>/dev/null \
    || ! timeout 5 sync "$storage_startup_pmem_device" 2>/dev/null \
    || ! storage_pmem_marker_at_offset \
      "$storage_startup_pmem_device" BANGBANG_STORAGE_PMEM_GUEST 4096; then
    storage_certification_fail INITIAL_IO
    return
  fi
  if ! write_storage_block_sector_marker \
    "$storage_control_device" BANGBANG_STORAGE_READY 1; then
    storage_certification_fail READY
    return
  fi
  emit_line BANGBANG_STORAGE_READY

  if ! wait_for_storage_control_marker BANGBANG_STORAGE_CONTINUE_ONE 3; then
    storage_certification_fail FIRST_CONTINUE
    return
  fi
  storage_vhost_name=${storage_vhost_device##*/}
  attempts=0
  while [ "$(cat "/sys/class/block/$storage_vhost_name/size" 2>/dev/null || true)" != 16 ]; do
    if [ "$attempts" -ge 30 ]; then
      storage_certification_fail VHOST_RESIZE
      return
    fi
    sleep 1
    attempts=$((attempts + 1))
  done
  if ! wait_for_storage_block BANGBANG_STORAGE_ASYNC_REPLACEMENT_HOST; then
    storage_certification_fail ASYNC_REPLACEMENT_DISCOVERY
    return
  fi
  storage_replacement_device=$storage_block_device
  if ! write_storage_block_sector_marker \
      "$storage_replacement_device" BANGBANG_STORAGE_ASYNC_REPLACEMENT_GUEST 2 \
    || ! run_storage_block_round \
      BANGBANG_STORAGE_RUNTIME_BLOCK_ONE_HOST \
      BANGBANG_STORAGE_RUNTIME_BLOCK_ONE_GUEST FIRST; then
    return
  fi
  if ! write_storage_block_sector_marker \
    "$storage_control_device" BANGBANG_STORAGE_FIRST_REMOVED 4; then
    storage_certification_fail FIRST_REMOVED
    return
  fi
  emit_line BANGBANG_STORAGE_FIRST_REMOVED

  if ! wait_for_storage_control_marker BANGBANG_STORAGE_CONTINUE_TWO 5; then
    storage_certification_fail SECOND_CONTINUE
    return
  fi
  if ! run_storage_block_round \
      BANGBANG_STORAGE_RUNTIME_BLOCK_TWO_HOST \
      BANGBANG_STORAGE_RUNTIME_BLOCK_TWO_GUEST SECOND; then
    return
  fi
  if ! write_storage_block_sector_marker \
    "$storage_control_device" BANGBANG_STORAGE_SECOND_BLOCK_REMOVED 7; then
    storage_certification_fail SECOND_BLOCK_REMOVED
    return
  fi
  emit_line BANGBANG_STORAGE_SECOND_BLOCK_REMOVED
  if ! wait_for_storage_control_marker BANGBANG_STORAGE_CONTINUE_PMEM_ONE 8; then
    storage_certification_fail FIRST_PMEM_CONTINUE
    return
  fi
  if ! run_storage_pmem_round \
      BANGBANG_STORAGE_RUNTIME_PMEM_ONE_HOST \
      BANGBANG_STORAGE_RUNTIME_PMEM_ONE_GUEST FIRST; then
    return
  fi
  if ! write_storage_block_sector_marker \
    "$storage_control_device" BANGBANG_STORAGE_FIRST_PMEM_REMOVED 9; then
    storage_certification_fail FIRST_PMEM_REMOVED
    return
  fi
  emit_line BANGBANG_STORAGE_FIRST_PMEM_REMOVED
  if ! wait_for_storage_control_marker BANGBANG_STORAGE_CONTINUE_PMEM_TWO 10; then
    storage_certification_fail SECOND_PMEM_CONTINUE
    return
  fi
  if ! run_storage_pmem_round \
      BANGBANG_STORAGE_RUNTIME_PMEM_TWO_HOST \
      BANGBANG_STORAGE_RUNTIME_PMEM_TWO_GUEST SECOND; then
    return
  fi
  if ! write_storage_block_sector_marker \
    "$storage_control_device" BANGBANG_STORAGE_SUCCESS 6; then
    storage_certification_fail SUCCESS_MARKER
    return
  fi
  emit_line BANGBANG_STORAGE_SUCCESS
}

block_hotplug_fail() {
  reason=$1
  marker="BANGBANG_BLOCK_HOTPLUG_FAIL_$reason"
  emit_line "$marker"
  write_vdb_marker "$marker"
  sync /dev/vdb 2>/dev/null || sync
}

block_device_sectors() {
  device=$1
  cat "/sys/class/block/$device/size" 2>/dev/null || true
}

block_device_write_cache() {
  device=$1
  cat "/sys/class/block/$device/queue/write_cache" 2>/dev/null || true
}

wait_for_block_sectors() {
  device=$1
  expected=$2
  attempts=0
  while [ "$(block_device_sectors "$device")" != "$expected" ]; do
    if [ "$attempts" -ge 30 ]; then
      return 1
    fi
    sleep 1
    attempts=$((attempts + 1))
  done
}

block_lifecycle_fail() {
  reason=$1
  emit_line "BANGBANG_BLOCK_LIFECYCLE_FAIL_$reason"
}

run_block_lifecycle_phase() {
  expected=$1
  written=$2
  phase=$3

  if ! vdb_starts_with_marker "$expected"; then
    block_lifecycle_fail "${phase}_READ"
    return 1
  fi
  if ! write_vdb_sector_marker "$written" 2; then
    block_lifecycle_fail "${phase}_WRITE"
    return 1
  fi
  if ! vdb_sector_starts_with_marker "$written" 2; then
    block_lifecycle_fail "${phase}_VERIFY"
    return 1
  fi
}

check_block_backing_lifecycle() {
  mode=$1
  emit_line BANGBANG_BLOCK_LIFECYCLE_ENTER
  if [ ! -b /dev/vdb ] || [ ! -b /dev/vdc ] || [ ! -b /dev/vdd ]; then
    block_lifecycle_fail PREREQUISITES
    return
  fi
  if [ "$(block_device_sectors vdb)" != 8192 ]; then
    block_lifecycle_fail INITIAL_CAPACITY
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_CAPACITY_OK
  if [ "$(block_device_write_cache vdb)" != "write back" ]; then
    block_lifecycle_fail WRITEBACK_FEATURE
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_WRITEBACK_OK
  if [ "$(cat /sys/class/block/vdc/ro 2>/dev/null || true)" != 1 ]; then
    block_lifecycle_fail READ_ONLY_FLAG
    return
  fi
  if [ "$(block_device_write_cache vdc)" != "write through" ]; then
    block_lifecycle_fail UNSAFE_FEATURE
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_READ_ONLY_CONFIG_OK
  if ! vdc_starts_with_marker BANGBANG_BLOCK_LIFECYCLE_READ_ONLY; then
    block_lifecycle_fail READ_ONLY_READ
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_READ_ONLY_READ_OK
  if printf '%-512s' BANGBANG_BLOCK_LIFECYCLE_READ_ONLY_BAD \
    | dd of=/dev/vdc bs=512 count=1 conv=notrunc,fsync 2>/dev/null; then
    block_lifecycle_fail READ_ONLY_WRITE
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_READ_ONLY_WRITE_OK
  if ! report_block_serial_phase vdb BLOCK_LIFECYCLE_INITIAL; then
    block_lifecycle_fail INITIAL_GET_ID
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_GET_ID_OK
  if cmdline_has bangbang.expect-block-limiter-patch=1; then
    emit_line BANGBANG_BLOCK_LIFECYCLE_LIMITER_READY
    attempts=0
    while ! vdd_starts_with_marker BANGBANG_BLOCK_LIFECYCLE_LIMITER_CONTINUE; do
      if [ "$attempts" -ge 60 ]; then
        block_lifecycle_fail LIMITER_PATCH
        return
      fi
      sleep 1
      attempts=$((attempts + 1))
    done
    emit_line BANGBANG_BLOCK_LIFECYCLE_LIMITER_CONTINUE
  fi
  if ! run_block_lifecycle_phase \
    BANGBANG_BLOCK_LIFECYCLE_HOST_ONE \
    BANGBANG_BLOCK_LIFECYCLE_GUEST_ONE \
    FIRST; then
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_PHASE_ONE

  if [ "$mode" = three ]; then
    if ! wait_for_block_sectors vdb 12288; then
      block_lifecycle_fail REGULAR_CAPACITY
      return
    fi
    if ! run_block_lifecycle_phase \
      BANGBANG_BLOCK_LIFECYCLE_HOST_TWO \
      BANGBANG_BLOCK_LIFECYCLE_GUEST_TWO \
      SECOND; then
      return
    fi
    emit_line BANGBANG_BLOCK_LIFECYCLE_PHASE_TWO
  fi

  if ! wait_for_block_sectors vdb 16384; then
    block_lifecycle_fail FINAL_CAPACITY
    return
  fi
  if ! run_block_lifecycle_phase \
    BANGBANG_BLOCK_LIFECYCLE_HOST_THREE \
    BANGBANG_BLOCK_LIFECYCLE_GUEST_THREE \
    THIRD; then
    return
  fi
  emit_line BANGBANG_BLOCK_LIFECYCLE_SUCCESS
}

wait_for_runtime_block() {
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    if [ -b /dev/vdc ]; then
      return 0
    fi
    printf '1\n' > /sys/bus/pci/rescan 2>/dev/null || return 1
    sleep 1
    attempts=$((attempts + 1))
  done
  return 1
}

runtime_block_pci_path() {
  path=$(readlink -f /sys/class/block/vdc/device 2>/dev/null || true)
  while [ -n "$path" ] && [ "$path" != / ]; do
    if [ -e "$path/remove" ] && [ -r "$path/vendor" ] && [ -r "$path/device" ]; then
      vendor=$(cat "$path/vendor" 2>/dev/null || true)
      device=$(cat "$path/device" 2>/dev/null || true)
      if [ "$vendor" = 0x1af4 ] && [ "$device" = 0x1042 ]; then
        printf '%s\n' "$path"
        return 0
      fi
    fi
    parent=${path%/*}
    if [ "$parent" = "$path" ]; then
      break
    fi
    path=$parent
  done
  return 1
}

run_runtime_block_round() {
  expected=$1
  written=$2
  removed=$3
  phase=$4

  if ! wait_for_runtime_block; then
    block_hotplug_fail "${phase}_RESCAN"
    return 1
  fi
  if cmdline_has bangbang.expect-block-special-hotplug=1; then
    case "$phase" in
      FIRST)
        expected_sectors=8192
        serial_phase=BLOCK_HOTPLUG_FIRST
        ;;
      SECOND)
        expected_sectors=16384
        serial_phase=BLOCK_HOTPLUG_SECOND
        ;;
      *)
        block_hotplug_fail "${phase}_PROFILE"
        return 1
        ;;
    esac
    if [ "$(block_device_sectors vdc)" != "$expected_sectors" ]; then
      block_hotplug_fail "${phase}_CAPACITY"
      return 1
    fi
    expected_cache=
    if cmdline_has bangbang.block-hotplug-cache-order=writeback-unsafe; then
      case "$phase" in
        FIRST) expected_cache="write back" ;;
        SECOND) expected_cache="write through" ;;
      esac
    elif cmdline_has bangbang.block-hotplug-cache-order=unsafe-writeback; then
      case "$phase" in
        FIRST) expected_cache="write through" ;;
        SECOND) expected_cache="write back" ;;
      esac
    fi
    if [ -n "$expected_cache" ] \
      && [ "$(block_device_write_cache vdc)" != "$expected_cache" ]; then
      block_hotplug_fail "${phase}_CACHE"
      return 1
    fi
    if ! report_block_serial_phase vdc "$serial_phase"; then
      block_hotplug_fail "${phase}_GET_ID"
      return 1
    fi
  fi
  if ! vdc_starts_with_marker "$expected"; then
    block_hotplug_fail "${phase}_READ"
    return 1
  fi
  if ! printf '%-512s' "$written" \
    | dd of=/dev/vdc bs=512 count=1 conv=notrunc,fsync 2>/dev/null; then
    block_hotplug_fail "${phase}_WRITE"
    return 1
  fi
  if ! vdc_starts_with_marker "$written"; then
    block_hotplug_fail "${phase}_VERIFY"
    return 1
  fi

  pci_path=$(runtime_block_pci_path) || {
    block_hotplug_fail "${phase}_IDENTITY"
    return 1
  }
  if ! printf '1\n' > "$pci_path/remove" 2>/dev/null; then
    block_hotplug_fail "${phase}_REMOVE"
    return 1
  fi
  attempts=0
  while [ -b /dev/vdc ] && [ "$attempts" -lt 30 ]; do
    sleep 1
    attempts=$((attempts + 1))
  done
  if [ -b /dev/vdc ]; then
    block_hotplug_fail "${phase}_REMOVE_WAIT"
    return 1
  fi

  write_vdb_marker "$removed"
  sync /dev/vdb 2>/dev/null || sync
  emit_line "$removed"
}

check_block_hotplug_marker() {
  if [ ! -b /dev/vdb ] || [ ! -e /sys/bus/pci/rescan ]; then
    block_hotplug_fail PREREQUISITES
    return
  fi

  write_vdb_marker BANGBANG_BLOCK_HOTPLUG_READY
  sync /dev/vdb 2>/dev/null || sync
  emit_line BANGBANG_BLOCK_HOTPLUG_READY

  if cmdline_has bangbang.expect-vhost-resize=1; then
    attempts=0
    while [ "$(cat /sys/class/block/vdb/size 2>/dev/null || true)" != 4 ]; do
      if [ "$attempts" -ge 30 ]; then
        block_hotplug_fail VHOST_RESIZE
        return
      fi
      sleep 1
      attempts=$((attempts + 1))
    done
    if ! write_vdb_sector_marker BANGBANG_VHOST_CONFIG_RESIZED 3; then
      block_hotplug_fail VHOST_RESIZE_WRITE
      return
    fi
    emit_line BANGBANG_VHOST_CONFIG_RESIZED
  fi

  if ! run_runtime_block_round \
    BANGBANG_BLOCK_HOTPLUG_HOST_ONE \
    BANGBANG_BLOCK_HOTPLUG_GUEST_ONE \
    BANGBANG_BLOCK_HOTPLUG_FIRST_REMOVED \
    FIRST; then
    return
  fi

  attempts=0
  while ! vdb_sector_starts_with_marker BANGBANG_BLOCK_HOTPLUG_CONTINUE 1; do
    if [ "$attempts" -ge 60 ]; then
      block_hotplug_fail CONTINUE
      return
    fi
    sleep 1
    attempts=$((attempts + 1))
  done

  if ! run_runtime_block_round \
    BANGBANG_BLOCK_HOTPLUG_HOST_TWO \
    BANGBANG_BLOCK_HOTPLUG_GUEST_TWO \
    BANGBANG_BLOCK_HOTPLUG_SUCCESS \
    SECOND; then
    return
  fi
}

pmem_hotplug_fail() {
  reason=$1
  marker="BANGBANG_PMEM_HOTPLUG_FAIL_$reason"
  emit_line "$marker"
  write_vdb_marker "$marker"
  sync /dev/vdb 2>/dev/null || sync
}

find_runtime_pmem() {
  runtime_pmem_device=
  runtime_pmem_pci_path=
  runtime_pmem_resource=

  for function_path in /sys/bus/pci/devices/*; do
    [ -d "$function_path" ] || continue
    function=${function_path##*/}
    pci_function_has_identity "$function" 0x105b || continue
    for block_path in "$function_path"/virtio*/ndbus*/region*/namespace*/block/*; do
      [ -e "$block_path" ] || continue
      namespace_path=${block_path%/block/*}
      resource=$(cat "$namespace_path/resource" 2>/dev/null || true)
      [ -n "$resource" ] || continue
      device="/dev/${block_path##*/}"
      [ -b "$device" ] || continue
      runtime_pmem_device=$device
      runtime_pmem_pci_path=$function_path
      runtime_pmem_resource=$resource
      return 0
    done
  done

  return 1
}

wait_for_runtime_pmem() {
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    printf '1\n' > /sys/bus/pci/rescan 2>/dev/null || return 1
    sleep 1
    if find_runtime_pmem; then
      return 0
    fi
    attempts=$((attempts + 1))
  done
  return 1
}

runtime_pmem_marker_at_offset() {
  marker=$1
  offset=$2
  actual=$(dd if="$runtime_pmem_device" bs=1 skip="$offset" count="${#marker}" 2>/dev/null || true)
  [ "$actual" = "$marker" ]
}

run_runtime_pmem_round() {
  expected=$1
  written=$2
  removed=$3
  phase=$4

  if ! wait_for_runtime_pmem; then
    pmem_hotplug_fail "${phase}_RESCAN"
    return 1
  fi
  if ! runtime_pmem_marker_at_offset "$expected" 0; then
    pmem_hotplug_fail "${phase}_READ"
    return 1
  fi

  if [ "$phase" = FIRST ]; then
    pmem_hotplug_first_pci_path=$runtime_pmem_pci_path
    pmem_hotplug_first_resource=$runtime_pmem_resource
  elif [ "$runtime_pmem_pci_path" != "$pmem_hotplug_first_pci_path" ]; then
    pmem_hotplug_fail "${phase}_SLOT_REUSE"
    return 1
  elif [ "$runtime_pmem_resource" != "$pmem_hotplug_first_resource" ]; then
    pmem_hotplug_fail "${phase}_RANGE_REUSE"
    return 1
  fi

  if ! printf '%s' "$written" \
    | dd of="$runtime_pmem_device" bs=1 seek=4096 conv=notrunc 2>/dev/null; then
    pmem_hotplug_fail "${phase}_WRITE"
    return 1
  fi
  if ! sync "$runtime_pmem_device" 2>/dev/null; then
    pmem_hotplug_fail "${phase}_FLUSH"
    return 1
  fi
  if ! runtime_pmem_marker_at_offset "$written" 4096; then
    pmem_hotplug_fail "${phase}_VERIFY"
    return 1
  fi

  removed_device=$runtime_pmem_device
  if ! printf '1\n' > "$runtime_pmem_pci_path/remove" 2>/dev/null; then
    pmem_hotplug_fail "${phase}_REMOVE"
    return 1
  fi
  attempts=0
  while [ -b "$removed_device" ] && [ "$attempts" -lt 30 ]; do
    sleep 1
    attempts=$((attempts + 1))
  done
  if [ -b "$removed_device" ]; then
    pmem_hotplug_fail "${phase}_REMOVE_WAIT"
    return 1
  fi

  write_vdb_marker "$removed"
  sync /dev/vdb 2>/dev/null || sync
  emit_line "$removed"
}

check_pmem_hotplug_marker() {
  if [ ! -b /dev/vdb ] || [ ! -e /sys/bus/pci/rescan ]; then
    pmem_hotplug_fail PREREQUISITES
    return
  fi

  pmem_hotplug_first_pci_path=
  pmem_hotplug_first_resource=
  write_vdb_marker BANGBANG_PMEM_HOTPLUG_READY
  sync /dev/vdb 2>/dev/null || sync
  emit_line BANGBANG_PMEM_HOTPLUG_READY

  if ! run_runtime_pmem_round \
    BANGBANG_PMEM_HOTPLUG_HOST_ONE \
    BANGBANG_PMEM_HOTPLUG_GUEST_ONE \
    BANGBANG_PMEM_HOTPLUG_FIRST_REMOVED \
    FIRST; then
    return
  fi

  attempts=0
  while ! vdb_sector_starts_with_marker BANGBANG_PMEM_HOTPLUG_CONTINUE 1; do
    if [ "$attempts" -ge 60 ]; then
      pmem_hotplug_fail CONTINUE
      return
    fi
    sleep 1
    attempts=$((attempts + 1))
  done

  if ! run_runtime_pmem_round \
    BANGBANG_PMEM_HOTPLUG_HOST_TWO \
    BANGBANG_PMEM_HOTPLUG_GUEST_TWO \
    BANGBANG_PMEM_HOTPLUG_SUCCESS \
    SECOND; then
    return
  fi
}

network_hotplug_fail() {
  reason=$1
  marker="BANGBANG_NETWORK_HOTPLUG_FAIL_$reason"
  emit_line "$marker"
  write_vdb_marker "$marker"
  sync /dev/vdb 2>/dev/null || sync
}

find_runtime_network() {
  expected_mac=$1
  runtime_network_iface=
  runtime_network_pci_path=

  for iface_path in /sys/class/net/*; do
    [ -d "$iface_path" ] || continue
    iface=${iface_path##*/}
    [ "$iface" != lo ] || continue
    actual_mac=$(cat "$iface_path/address" 2>/dev/null || true)
    [ "$actual_mac" = "$expected_mac" ] || continue

    path=$(readlink -f "$iface_path/device" 2>/dev/null || true)
    while [ -n "$path" ] && [ "$path" != / ]; do
      if [ -e "$path/remove" ] && [ -r "$path/vendor" ] && [ -r "$path/device" ]; then
        vendor=$(cat "$path/vendor" 2>/dev/null || true)
        device=$(cat "$path/device" 2>/dev/null || true)
        if [ "$vendor" = 0x1af4 ] && [ "$device" = 0x1041 ]; then
          runtime_network_iface=$iface
          runtime_network_pci_path=$path
          return 0
        fi
      fi
      parent=${path%/*}
      [ "$parent" != "$path" ] || break
      path=$parent
    done
  done

  return 1
}

wait_for_runtime_network() {
  expected_mac=$1
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    printf '1\n' > /sys/bus/pci/rescan 2>/dev/null || return 1
    sleep 1
    if find_runtime_network "$expected_mac"; then
      return 0
    fi
    attempts=$((attempts + 1))
  done
  return 1
}

remove_runtime_network() {
  removed_iface=$runtime_network_iface
  if ! printf '1\n' > "$runtime_network_pci_path/remove" 2>/dev/null; then
    return 1
  fi

  attempts=0
  while [ -e "/sys/class/net/$removed_iface" ] && [ "$attempts" -lt 30 ]; do
    sleep 1
    attempts=$((attempts + 1))
  done
  [ ! -e "/sys/class/net/$removed_iface" ]
}

fetch_runtime_network_mmds() {
  if ! ip link set dev "$runtime_network_iface" up 2>/dev/null; then
    return 1
  fi
  ip addr add 169.254.100.2/16 dev "$runtime_network_iface" 2>/dev/null || true
  if ! ip route replace 169.254.169.254/32 \
    dev "$runtime_network_iface" src 169.254.100.2 2>/dev/null; then
    return 1
  fi

  mmds_value=$(
    curl \
      --fail \
      --silent \
      --show-error \
      --connect-timeout 2 \
      --max-time 5 \
      --interface "$runtime_network_iface" \
      http://169.254.169.254/meta-data/bangbang-marker \
      2>/dev/null || true
  )
  [ "$mmds_value" = BANGBANG_MMDS_GUEST_VALUE ]
}

run_runtime_network_round() {
  expected_mac=$1
  removed_marker=$2
  phase=$3

  if ! wait_for_runtime_network "$expected_mac"; then
    network_hotplug_fail "${phase}_RESCAN"
    return 1
  fi
  if [ "$runtime_network_pci_path" != "$network_hotplug_first_pci_path" ]; then
    network_hotplug_fail "${phase}_SLOT_REUSE"
    return 1
  fi
  if ! fetch_runtime_network_mmds; then
    network_hotplug_fail "${phase}_MMDS"
    return 1
  fi
  if ! remove_runtime_network; then
    network_hotplug_fail "${phase}_REMOVE"
    return 1
  fi

  write_vdb_marker "$removed_marker"
  sync /dev/vdb 2>/dev/null || sync
  emit_line "$removed_marker"
}

check_network_hotplug_marker() {
  if [ ! -b /dev/vdb ] || [ ! -e /sys/bus/pci/rescan ]; then
    network_hotplug_fail PREREQUISITES
    return
  fi
  if ! command -v ip >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1; then
    network_hotplug_fail TOOLS
    return
  fi
  if ! find_runtime_network 06:00:00:00:00:42; then
    network_hotplug_fail STARTUP_NETWORK
    return
  fi

  network_hotplug_first_pci_path=$runtime_network_pci_path
  if ! remove_runtime_network; then
    network_hotplug_fail STARTUP_REMOVE
    return
  fi
  write_vdb_marker BANGBANG_NETWORK_HOTPLUG_READY
  sync /dev/vdb 2>/dev/null || sync
  emit_line BANGBANG_NETWORK_HOTPLUG_READY

  attempts=0
  while ! vdb_sector_starts_with_marker BANGBANG_NETWORK_HOTPLUG_FIRST_CONTINUE 1; do
    if [ "$attempts" -ge 60 ]; then
      network_hotplug_fail FIRST_CONTINUE
      return
    fi
    sleep 1
    attempts=$((attempts + 1))
  done
  if ! run_runtime_network_round \
    06:00:00:00:00:42 \
    BANGBANG_NETWORK_HOTPLUG_FIRST_REMOVED \
    FIRST; then
    return
  fi

  attempts=0
  while ! vdb_sector_starts_with_marker BANGBANG_NETWORK_HOTPLUG_SECOND_CONTINUE 2; do
    if [ "$attempts" -ge 60 ]; then
      network_hotplug_fail SECOND_CONTINUE
      return
    fi
    sleep 1
    attempts=$((attempts + 1))
  done
  run_runtime_network_round \
    06:00:00:00:00:42 \
    BANGBANG_NETWORK_HOTPLUG_SUCCESS \
    SECOND
}

pci_all_virtio_fail() {
  reason=$1
  marker="BANGBANG_PCI_ALL_VIRTIO_FAIL_$reason"
  emit_line "$marker"
  write_vdb_marker "$marker"
}

check_all_virtio_pci_marker() {
  if cmdline_has pci=off; then
    pci_all_virtio_fail PCI_OFF
    return
  fi

  if ! pci_function_has_identity 0000:00:01.0 0x1045 \
    || ! pci_function_has_identity 0000:00:02.0 0x1042 \
    || ! pci_function_has_identity 0000:00:03.0 0x1042 \
    || ! pci_function_has_identity 0000:00:04.0 0x1041 \
    || ! pci_function_has_identity 0000:00:05.0 0x105b \
    || ! pci_function_has_identity 0000:00:06.0 0x1053 \
    || ! pci_function_has_identity 0000:00:07.0 0x1044 \
    || ! pci_function_has_identity 0000:00:08.0 0x1058; then
    pci_all_virtio_fail IDENTITIES
    return
  fi
  emit_line BANGBANG_PCI_ALL_VIRTIO_IDENTITIES_OK

  if find /sys/firmware/devicetree/base -maxdepth 1 -name 'virtio_mmio@*' 2>/dev/null \
    | grep -q .; then
    pci_all_virtio_fail LEGACY_MMIO
    return
  fi

  read_entropy_marker
  if ! vdb_starts_with_marker BANGBANG_ENTROPY_GUEST_READ_OK; then
    pci_all_virtio_fail ENTROPY
    return
  fi

  check_balloon_marker
  if ! vdb_starts_with_marker BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK; then
    pci_all_virtio_fail BALLOON
    return
  fi

  fetch_mmds_marker
  if ! vdb_starts_with_marker BANGBANG_MMDS_GUEST_FETCH_OK; then
    pci_all_virtio_fail NETWORK
    return
  fi

  read_flush_pmem_marker
  if ! vdb_starts_with_marker BANGBANG_PMEM_READ_FLUSH_OK; then
    pci_all_virtio_fail PMEM
    return
  fi

  fetch_vsock_marker
  if ! vdb_starts_with_marker BANGBANG_VSOCK_GUEST_CONNECT_OK; then
    pci_all_virtio_fail VSOCK
    return
  fi

  check_memory_hotplug_marker
  if ! vdb_starts_with_marker BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK; then
    pci_all_virtio_fail MEMORY_HOTPLUG
    return
  fi

  write_vdb_marker BANGBANG_PCI_ALL_VIRTIO_GUEST_CHECK_OK
  sync /dev/vdb 2>/dev/null || sync
  emit_line BANGBANG_PCI_ALL_VIRTIO_GUEST_CHECK_OK
}

cpu_template_report_failure() {
  emit_line BANGBANG_CPU_TEMPLATE_GUEST_CHECK_FAIL
  write_vdb_marker BANGBANG_CPU_TEMPLATE_GUEST_CHECK_FAIL
}

report_cpu_template_ids() {
  cpu_report=/dev/bangbang-cpu-template-member
  aggregate_report=/dev/bangbang-cpu-template-report

  if [ ! -x /bangbang-arm64-id-register-report ] \
    || ! command -v taskset >/dev/null 2>&1 \
    || [ ! -b /dev/vdb ]; then
    cpu_template_report_failure
    return
  fi

  if ! printf '%s\n' BANGBANG_ARM64_ID_SET_V1 > "$aggregate_report"; then
    cpu_template_report_failure
    return
  fi

  member_count=0
  for cpu_path in /sys/devices/system/cpu/cpu[0-9]*; do
    if [ ! -d "$cpu_path" ]; then
      continue
    fi
    cpu=${cpu_path##*cpu}
    case "$cpu" in
      "" | *[!0123456789]*) continue ;;
    esac

    if [ -e "$cpu_path/online" ]; then
      if ! printf '1\n' > "$cpu_path/online" 2>/dev/null; then
        cpu_template_report_failure
        return
      fi
      if [ "$(cat "$cpu_path/online" 2>/dev/null || true)" != 1 ]; then
        cpu_template_report_failure
        return
      fi
    fi

    if ! taskset -c "$cpu" /bangbang-arm64-id-register-report \
      > "$cpu_report" 2>/dev/null; then
      cpu_template_report_failure
      return
    fi
    report_size=$(wc -c < "$cpu_report" 2>/dev/null || true)
    report_lines=$(wc -l < "$cpu_report" 2>/dev/null || true)
    report_size=${report_size##* }
    report_lines=${report_lines##* }
    case "$report_size:$report_lines" in
      "" | *[!0123456789:]* | *:) cpu_template_report_failure; return ;;
    esac
    if [ "$report_size" -gt 160 ] || [ "$report_lines" -ne 5 ] \
      || [ "$(sed -n '1p' "$cpu_report")" != BANGBANG_ARM64_ID_REPORT_V1 ] \
      || ! grep -Eq '^pfr0=[0-9a-f]{16}$' "$cpu_report" \
      || ! grep -Eq '^isar0=[0-9a-f]{16}$' "$cpu_report" \
      || ! grep -Eq '^isar1=[0-9a-f]{16}$' "$cpu_report" \
      || ! grep -Eq '^mmfr2=[0-9a-f]{16}$' "$cpu_report"; then
      cpu_template_report_failure
      return
    fi

    if ! printf 'cpu=%s\n' "$cpu" >> "$aggregate_report" \
      || ! sed -n '2,5p' "$cpu_report" >> "$aggregate_report"; then
      cpu_template_report_failure
      return
    fi
    member_count=$((member_count + 1))
    if [ "$member_count" -gt 32 ]; then
      cpu_template_report_failure
      return
    fi
  done

  aggregate_size=$(wc -c < "$aggregate_report" 2>/dev/null || true)
  aggregate_size=${aggregate_size##* }
  case "$aggregate_size" in
    "" | *[!0123456789]*) cpu_template_report_failure; return ;;
  esac
  if [ "$member_count" -eq 0 ] || [ "$aggregate_size" -gt 512 ]; then
    cpu_template_report_failure
    return
  fi
  if ! dd if="$aggregate_report" of=/dev/vdb bs=512 count=1 conv=sync,notrunc,fsync \
    2>/dev/null; then
    cpu_template_report_failure
    return
  fi

  emit_line BANGBANG_CPU_TEMPLATE_GUEST_CHECK_OK
}

first_network_iface() {
  for iface_path in /sys/class/net/*; do
    if [ ! -d "$iface_path" ]; then
      continue
    fi

    iface=${iface_path##*/}
    if [ "$iface" != lo ]; then
      printf '%s\n' "$iface"
      return 0
    fi
  done

  return 1
}

network_iface_with_mac() {
  expected_mac=$1
  attempt=1

  while [ "$attempt" -le 5 ]; do
    for iface_path in /sys/class/net/*; do
      if [ ! -d "$iface_path" ] || [ ! -r "$iface_path/address" ]; then
        continue
      fi

      iface=${iface_path##*/}
      if [ "$iface" = lo ]; then
        continue
      fi

      actual_mac=$(cat "$iface_path/address" 2>/dev/null || true)
      if [ "$actual_mac" = "$expected_mac" ]; then
        printf '%s\n' "$iface"
        return 0
      fi
    done

    if [ "$attempt" -lt 5 ]; then
      sleep 1
    fi
    attempt=$((attempt + 1))
  done

  return 1
}

prepare_mmds_network() {
  if ! command -v ip >/dev/null 2>&1; then
    emit_line "${1}_NO_IP"
    write_vdb_marker "$2"
    return 1
  fi

  if ! command -v curl >/dev/null 2>&1; then
    emit_line "${1}_NO_CURL"
    write_vdb_marker "$2"
    return 1
  fi

  mmds_iface=$(first_network_iface || true)
  if [ -z "$mmds_iface" ]; then
    emit_line "${1}_NO_IFACE"
    write_vdb_marker "$2"
    return 1
  fi

  if cmdline_has bangbang.mmds-mtu=1280; then
    mmds_mtu_path="/sys/class/net/$mmds_iface/mtu"
    if [ ! -r "$mmds_mtu_path" ]; then
      emit_line "${1}_MTU_UNREADABLE"
      write_vdb_marker "$2"
      return 1
    fi

    mmds_actual_mtu=$(cat "$mmds_mtu_path" 2>/dev/null || true)
    if [ "$mmds_actual_mtu" != 1280 ]; then
      emit_line "${1}_MTU_MISMATCH"
      write_vdb_marker "$2"
      return 1
    fi
    emit_line BANGBANG_MMDS_MTU_OK
  fi

  if ! ip link set dev "$mmds_iface" up 2>/dev/null; then
    emit_line "${1}_LINK"
    write_vdb_marker "$2"
    return 1
  fi

  ip addr add 169.254.100.2/16 dev "$mmds_iface" 2>/dev/null || true
  ip route replace 169.254.0.0/16 dev "$mmds_iface" src 169.254.100.2 2>/dev/null || true
  return 0
}

fetch_mmds_marker() {
  if ! prepare_mmds_network BANGBANG_MMDS_FETCH_FAIL BANGBANG_MMDS_FETCH_FAIL; then
    return
  fi

  if cmdline_has bangbang.expect-pci-data=1; then
    if ! pci_function_has_identity 0000:00:01.0 0x1042 \
      || ! pci_function_has_identity 0000:00:02.0 0x1041; then
      emit_line BANGBANG_PCI_NETWORK_IDENTITIES_FAIL
      write_vdb_marker BANGBANG_MMDS_FETCH_FAIL
      return
    fi
    emit_line BANGBANG_PCI_NETWORK_IDENTITIES_OK
  fi

  mmds_value=$(
    curl \
      --fail \
      --silent \
      --show-error \
      --connect-timeout 2 \
      --max-time 5 \
      http://169.254.169.254/meta-data/bangbang-marker \
      2>/dev/null || true
  )

  if [ "$mmds_value" = BANGBANG_MMDS_GUEST_VALUE ]; then
    emit_line BANGBANG_MMDS_FETCH_OK
    if cmdline_has bangbang.mmds-mtu=1280; then
      write_vdb_marker BANGBANG_MMDS_MTU_GUEST_FETCH_OK
    else
      write_vdb_marker BANGBANG_MMDS_GUEST_FETCH_OK
    fi
  else
    emit_line BANGBANG_MMDS_FETCH_FAIL_RESPONSE
    write_vdb_marker BANGBANG_MMDS_FETCH_FAIL
  fi
}

prove_virtio_network_semantics() {
  if ! prepare_mmds_network BANGBANG_VIRTIO_NET_SEMANTICS_FAIL BANGBANG_VIRTIO_NET_SEMANTICS_FAIL; then
    return
  fi

  if cmdline_has bangbang.expect-pci-data=1; then
    if ! pci_function_has_identity 0000:00:01.0 0x1042 \
      || ! pci_function_has_identity 0000:00:02.0 0x1041; then
      emit_line BANGBANG_PCI_NETWORK_IDENTITIES_FAIL
      write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
      return
    fi
    emit_line BANGBANG_PCI_NETWORK_IDENTITIES_OK
  fi

  if ! ip link set dev "$mmds_iface" mtu 1500 2>/dev/null; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_MTU_1500
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi
  if ! ip route replace 169.254.0.0/16 dev "$mmds_iface" \
    src 169.254.100.2 advmss 256 2>/dev/null; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_ADVMSS
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi

  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi
  if ! request_mmds_v2_token \
    BANGBANG_VIRTIO_NET_SEMANTICS_FAIL \
    BANGBANG_VIRTIO_NET_SEMANTICS_FAIL \
    60; then
    return
  fi
  first_mmds_token=$mmds_token
  baseline_value=$(
    python3 - "$mmds_token" <<'PY' 2>/dev/null || true
import socket
import sys

body_marker = b"BANGBANG_MMDS_GUEST_VALUE"
token = sys.argv[1].encode("ascii")
request = (
    b"GET /meta-data/bangbang-marker HTTP/1.1\r\n"
    b"Host: 169.254.169.254\r\n"
    b"Accept: */*\r\n"
    b"X-metadata-token: " + token + b"\r\n"
    b"X-Bangbang-Padding: " + (b"a" * 2200) + b"\r\n\r\n"
)

with socket.create_connection(("169.254.169.254", 80), timeout=10) as connection:
    connection.settimeout(10)
    connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_CORK, 1)
    connection.sendall(request)
    connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_CORK, 0)
    response = bytearray()
    header_end = -1
    content_length = None
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            break
        response.extend(chunk)
        if header_end < 0:
            header_end = response.find(b"\r\n\r\n")
            if header_end >= 0:
                headers = bytes(response[:header_end]).split(b"\r\n")
                for header in headers[1:]:
                    name, separator, value = header.partition(b":")
                    if separator and name.lower() == b"content-length":
                        content_length = int(value.strip())
                        break
        if header_end >= 0 and content_length is not None:
            body_start = header_end + 4
            if len(response) >= body_start + content_length:
                break

if header_end < 0 or content_length is None:
    raise RuntimeError("MMDS response was incomplete")
body_start = header_end + 4
body = bytes(response[body_start:body_start + content_length])
if body != body_marker:
    raise RuntimeError("MMDS response body did not match")
print(body.decode("ascii"))
PY
  )
  if [ "$baseline_value" != BANGBANG_MMDS_GUEST_VALUE ]; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_TSO
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi
  emit_line BANGBANG_MMDS_FETCH_OK
  emit_line BANGBANG_VIRTIO_NET_LARGE_TX_OK

  if ! ip route replace 169.254.0.0/16 dev "$mmds_iface" \
    src 169.254.100.2 2>/dev/null; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_ROUTE_RESTORE
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi

  if ! request_mmds_v2_token \
    BANGBANG_VIRTIO_NET_SEMANTICS_FAIL \
    BANGBANG_VIRTIO_NET_SEMANTICS_FAIL \
    60; then
    return
  fi
  if [ "$mmds_token" = "$first_mmds_token" ]; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_TOKEN_RENEW
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi
  emit_line BANGBANG_MMDS_V2_RENEW_OK

  large_result=$(
    python3 - "$mmds_token" <<'PY' 2>/dev/null || true
import socket
import sys
import time

token = sys.argv[1].encode("ascii")
request = (
    b"GET /meta-data/bangbang-large HTTP/1.1\r\n"
    b"Host: 169.254.169.254\r\n"
    b"Accept: */*\r\n"
    b"X-metadata-token: " + token + b"\r\n\r\n"
)

with socket.create_connection(("169.254.169.254", 80), timeout=15) as connection:
    connection.settimeout(15)
    connection.sendall(request)
    response = bytearray()
    header_end = -1
    content_length = None
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            break
        response.extend(chunk)
        if header_end < 0:
            header_end = response.find(b"\r\n\r\n")
            if header_end >= 0:
                headers = bytes(response[:header_end]).split(b"\r\n")
                for header in headers[1:]:
                    name, separator, value = header.partition(b":")
                    if separator and name.lower() == b"content-length":
                        content_length = int(value.strip())
                        break
        if header_end >= 0 and content_length is not None:
            body_start = header_end + 4
            if len(response) >= body_start + content_length:
                break

    time.sleep(5)

if header_end < 0 or content_length != 49152:
    raise RuntimeError("MMDS large response headers did not match")
body_start = header_end + 4
body = bytes(response[body_start:body_start + content_length])
if len(body) != 49152 or body[:1] != b"z" or body[-1:] != b"z" or body.count(b"z") != 49152:
    raise RuntimeError("MMDS large response body did not match")
print("BANGBANG_VIRTIO_NET_LARGE_RX_OK")
PY
  )
  if [ "$large_result" != BANGBANG_VIRTIO_NET_LARGE_RX_OK ]; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_LARGE_CONTENT
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi

  if ! ip link set dev "$mmds_iface" mtu 50000 2>/dev/null; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_MTU_50000
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi
  large_merged_result=$(
    get_mmds_v2_value meta-data/bangbang-large \
      | python3 -c 'import sys; data = sys.stdin.buffer.read(); print("BANGBANG_VIRTIO_NET_LARGE_MERGED_OK" if len(data) == 49152 and data == b"z" * 49152 else "")' \
      2>/dev/null || true
  )
  if [ "$large_merged_result" != BANGBANG_VIRTIO_NET_LARGE_MERGED_OK ]; then
    emit_line BANGBANG_VIRTIO_NET_SEMANTICS_FAIL_LARGE_MERGED
    write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_FAIL
    return
  fi

  emit_line BANGBANG_VIRTIO_NET_LARGE_RX_OK
  emit_line BANGBANG_VIRTIO_NET_SEMANTICS_OK
  write_vdb_marker BANGBANG_VIRTIO_NET_SEMANTICS_OK
}

fetch_mmds_marker_for_interface() {
  mmds_iface=$1
  source_address=$2
  success_marker=$3
  failure_marker=$4
  marker_sector=$5
  failure_prefix=$6

  if [ -z "$mmds_iface" ]; then
    emit_line "${failure_prefix}_NO_IFACE"
    write_vdb_marker_at_sector "$failure_marker" "$marker_sector"
    return
  fi

  if ! ip link set dev "$mmds_iface" up 2>/dev/null; then
    emit_line "${failure_prefix}_LINK"
    write_vdb_marker_at_sector "$failure_marker" "$marker_sector"
    return
  fi

  if ! ip addr add "$source_address/32" dev "$mmds_iface" 2>/dev/null; then
    emit_line "${failure_prefix}_ADDRESS"
    write_vdb_marker_at_sector "$failure_marker" "$marker_sector"
    return
  fi

  if ! ip route replace 169.254.169.254/32 \
    dev "$mmds_iface" src "$source_address" 2>/dev/null; then
    emit_line "${failure_prefix}_ROUTE"
    write_vdb_marker_at_sector "$failure_marker" "$marker_sector"
    return
  fi

  mmds_value=$(
    curl \
      --fail \
      --silent \
      --show-error \
      --connect-timeout 2 \
      --max-time 5 \
      --interface "$mmds_iface" \
      http://169.254.169.254/meta-data/bangbang-marker \
      2>/dev/null || true
  )

  if [ "$mmds_value" = BANGBANG_MMDS_GUEST_VALUE ]; then
    emit_line "${success_marker}"
    write_vdb_marker_at_sector "$success_marker" "$marker_sector"
  else
    emit_line "${failure_prefix}_RESPONSE"
    write_vdb_marker_at_sector "$failure_marker" "$marker_sector"
  fi
}

fetch_multi_interface_mmds_markers() {
  if ! command -v ip >/dev/null 2>&1; then
    emit_line BANGBANG_MMDS_MULTI_FETCH_FAIL_NO_IP
    write_vdb_marker_at_sector BANGBANG_MMDS_ETH0_FETCH_FAIL 0
    write_vdb_marker_at_sector BANGBANG_MMDS_ETH1_FETCH_FAIL 1
    return
  fi

  if ! command -v curl >/dev/null 2>&1; then
    emit_line BANGBANG_MMDS_MULTI_FETCH_FAIL_NO_CURL
    write_vdb_marker_at_sector BANGBANG_MMDS_ETH0_FETCH_FAIL 0
    write_vdb_marker_at_sector BANGBANG_MMDS_ETH1_FETCH_FAIL 1
    return
  fi

  mmds_eth0_iface=$(network_iface_with_mac 06:00:00:00:00:01 || true)
  mmds_eth1_iface=$(network_iface_with_mac 06:00:00:00:00:02 || true)

  fetch_mmds_marker_for_interface \
    "$mmds_eth0_iface" \
    169.254.100.2 \
    BANGBANG_MMDS_ETH0_GUEST_FETCH_OK \
    BANGBANG_MMDS_ETH0_FETCH_FAIL \
    0 \
    BANGBANG_MMDS_ETH0_FETCH_FAIL
  fetch_mmds_marker_for_interface \
    "$mmds_eth1_iface" \
    169.254.101.2 \
    BANGBANG_MMDS_ETH1_GUEST_FETCH_OK \
    BANGBANG_MMDS_ETH1_FETCH_FAIL \
    1 \
    BANGBANG_MMDS_ETH1_FETCH_FAIL
}

read_entropy_marker() {
  if [ ! -c /dev/hwrng ]; then
    emit_line BANGBANG_ENTROPY_GUEST_READ_FAIL_NO_HWRNG
    write_vdb_marker BANGBANG_ENTROPY_GUEST_READ_FAIL
    return
  fi

  if [ ! -r /sys/class/misc/hw_random/rng_current ]; then
    emit_line BANGBANG_ENTROPY_GUEST_READ_FAIL_NO_RNG_CURRENT
    write_vdb_marker BANGBANG_ENTROPY_GUEST_READ_FAIL
    return
  fi

  rng_current=$(cat /sys/class/misc/hw_random/rng_current 2>/dev/null || true)
  case "$rng_current" in
    virtio_rng*) ;;
    *)
      emit_line BANGBANG_ENTROPY_GUEST_READ_FAIL_NOT_VIRTIO_RNG
      write_vdb_marker BANGBANG_ENTROPY_GUEST_READ_FAIL
      return
      ;;
  esac

  entropy_result=$(dd if=/dev/hwrng bs=32 count=1 2>/dev/null | wc -c 2>/dev/null || true)
  entropy_bytes=${entropy_result##* }
  case "$entropy_bytes" in
    "" | *[!0123456789]*)
      emit_line BANGBANG_ENTROPY_GUEST_READ_FAIL_BAD_COUNT
      write_vdb_marker BANGBANG_ENTROPY_GUEST_READ_FAIL
      ;;
    0)
      emit_line BANGBANG_ENTROPY_GUEST_READ_FAIL_EMPTY
      write_vdb_marker BANGBANG_ENTROPY_GUEST_READ_FAIL
      ;;
    *)
      emit_line BANGBANG_ENTROPY_GUEST_READ_OK
      write_vdb_marker BANGBANG_ENTROPY_GUEST_READ_OK
      ;;
  esac
}

entropy_lifecycle_fail() {
  reason=$1
  marker="BANGBANG_ENTROPY_LIFECYCLE_FAIL_$reason"
  emit_line "$marker"
  write_vdb_sector_marker "$marker" 0 2>/dev/null || true
}

read_entropy_lifecycle_marker() {
  if [ ! -c /dev/hwrng ]; then
    entropy_lifecycle_fail NO_HWRNG
    return
  fi
  if [ ! -r /sys/class/misc/hw_random/rng_current ]; then
    entropy_lifecycle_fail NO_RNG_CURRENT
    return
  fi

  rng_current=$(cat /sys/class/misc/hw_random/rng_current 2>/dev/null || true)
  case "$rng_current" in
    virtio_rng*) ;;
    *)
      entropy_lifecycle_fail NOT_VIRTIO_RNG
      return
      ;;
  esac

  entropy_result=$(timeout 30 dd if=/dev/hwrng bs=32 count=1 2>/dev/null \
    | wc -c 2>/dev/null || true)
  entropy_bytes=${entropy_result##* }
  case "$entropy_bytes" in
    "" | *[!0123456789]* | 0)
      entropy_lifecycle_fail "FIRST_READ_$entropy_bytes"
      return
      ;;
  esac
  if ! write_vdb_sector_marker BANGBANG_ENTROPY_LIFECYCLE_READY 0; then
    entropy_lifecycle_fail READY_MARKER
    return
  fi
  emit_line BANGBANG_ENTROPY_LIFECYCLE_READY

  attempts=0
  while ! vdb_sector_starts_with_marker BANGBANG_ENTROPY_HOST_CONTINUE 1; do
    if [ "$attempts" -ge 60 ]; then
      entropy_lifecycle_fail HOST_CONTINUE
      return
    fi
    sleep 1
    attempts=$((attempts + 1))
  done
  emit_line BANGBANG_ENTROPY_HOST_CONTINUE_SEEN

  entropy_reads=0
  while [ "$entropy_reads" -lt 8 ]; do
    entropy_result=$(timeout 30 dd if=/dev/hwrng bs=32 count=1 2>/dev/null \
      | wc -c 2>/dev/null || true)
    entropy_bytes=${entropy_result##* }
    case "$entropy_bytes" in
      "" | *[!0123456789]* | 0)
        entropy_lifecycle_fail REPEATED_READ
        return
        ;;
    esac
    entropy_reads=$((entropy_reads + 1))
  done
  if ! write_vdb_sector_marker BANGBANG_ENTROPY_LIFECYCLE_OK 0; then
    entropy_lifecycle_fail SUCCESS_MARKER
    return
  fi
  emit_line BANGBANG_ENTROPY_LIFECYCLE_OK
}

check_balloon_marker() {
  if [ ! -d /sys/bus/virtio/devices ]; then
    emit_line BANGBANG_BALLOON_GUEST_CHECK_FAIL_NO_VIRTIO_BUS
    write_vdb_marker BANGBANG_BALLOON_GUEST_CHECK_FAIL
    return
  fi

  balloon_device=
  for driver_link in /sys/bus/virtio/devices/*/driver; do
    if [ ! -L "$driver_link" ]; then
      continue
    fi

    driver_target=$(readlink "$driver_link" 2>/dev/null || true)
    if [ "${driver_target##*/}" = virtio_balloon ]; then
      balloon_device=${driver_link%/driver}
      break
    fi
  done

  if [ -z "$balloon_device" ]; then
    emit_line BANGBANG_BALLOON_GUEST_CHECK_FAIL_NO_DEVICE
    write_vdb_marker BANGBANG_BALLOON_GUEST_CHECK_FAIL
    return
  fi

  features_path=$balloon_device/features
  if [ ! -r "$features_path" ]; then
    emit_line BANGBANG_BALLOON_GUEST_CHECK_FAIL_NO_FEATURES
    write_vdb_marker BANGBANG_BALLOON_GUEST_CHECK_FAIL
    return
  fi

  features=$(tr -d '\r\n' < "$features_path" 2>/dev/null || true)
  case "$features" in
    ?????1*)
      ;;
    *)
      emit_line BANGBANG_BALLOON_GUEST_CHECK_FAIL_REPORTING_NOT_NEGOTIATED
      write_vdb_marker BANGBANG_BALLOON_GUEST_CHECK_FAIL
      return
      ;;
  esac

  emit_line BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK
  write_vdb_marker BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK
}

check_memory_hotplug_marker() {
  if [ ! -d /sys/bus/virtio/devices ]; then
    emit_line BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL_NO_VIRTIO_BUS
    write_vdb_marker BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL
    return
  fi

  memory_device=
  for driver_link in /sys/bus/virtio/devices/*/driver; do
    if [ ! -L "$driver_link" ]; then
      continue
    fi

    driver_target=$(readlink "$driver_link" 2>/dev/null || true)
    if [ "${driver_target##*/}" = virtio_mem ]; then
      memory_device=${driver_link%/driver}
      break
    fi
  done

  if [ -z "$memory_device" ]; then
    emit_line BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL_NO_DEVICE
    write_vdb_marker BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL
    return
  fi

  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL
    return
  fi

  memory_hotplug_result=$(
    python3 - <<'PY' 2>/dev/null || true
import select
import subprocess
import sys
import time

DEVICE = "/dev/vdb"
EXPECTED_REQUESTED_SIZE = 128 * 1024 * 1024
READY_MARKER = b"BANGBANG_MEMORY_HOTPLUG_GUEST_READY"
GROWN_MARKER = b"BANGBANG_MEMORY_HOTPLUG_GUEST_GROWN"
SUCCESS_MARKER = b"BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK"
FAIL_MARKER = b"BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL"
TIMEOUT_SECONDS = 20.0


def marker_text(marker):
    return marker.decode("ascii")


def write_marker(marker):
    try:
        with open(DEVICE, "wb", buffering=0) as drive:
            drive.write(marker.ljust(512, b" "))
    except OSError:
        pass


def fail(reason):
    marker = FAIL_MARKER + b"_" + reason.encode("ascii")
    write_marker(marker)
    print(marker_text(marker))
    sys.exit(1)


def requested_size_from_line(line):
    if "virtio_mem" not in line or "requested size" not in line:
        return None

    value = line.rsplit(":", 1)[-1].strip().split()
    if value:
        try:
            return int(value[0], 0)
        except ValueError:
            pass

    normalized = line.lower()
    if "0x8000000" in normalized or "134217728" in normalized:
        return EXPECTED_REQUESTED_SIZE

    return None


try:
    dmesg = subprocess.Popen(
        ["dmesg", "--follow"],
        bufsize=0,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
except OSError:
    fail("DMESG_START")

try:
    if dmesg.stdout is None:
        fail("DMESG_STDOUT")

    deadline = time.monotonic() + TIMEOUT_SECONDS
    ready = False
    grown = False

    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            if not ready:
                fail("INIT_TIMEOUT")
            if not grown:
                fail("GROW_TIMEOUT")
            fail("SHRINK_TIMEOUT")

        readable, _, _ = select.select([dmesg.stdout], [], [], remaining)
        if not readable:
            if not ready:
                fail("INIT_TIMEOUT")
            if not grown:
                fail("GROW_TIMEOUT")
            fail("SHRINK_TIMEOUT")

        line = dmesg.stdout.readline()
        if not line:
            if dmesg.poll() is not None:
                fail("DMESG_EXIT")
            continue

        text = line.decode("utf-8", "replace").strip()
        requested_size = requested_size_from_line(text)
        if requested_size is None:
            continue

        if not ready and requested_size == 0:
            ready = True
            deadline = time.monotonic() + TIMEOUT_SECONDS
            write_marker(READY_MARKER)
            print(marker_text(READY_MARKER), flush=True)
            continue

        if ready and not grown and requested_size == EXPECTED_REQUESTED_SIZE:
            grown = True
            deadline = time.monotonic() + TIMEOUT_SECONDS
            write_marker(GROWN_MARKER)
            print(marker_text(GROWN_MARKER), flush=True)
            continue

        if grown and requested_size == 0:
            write_marker(SUCCESS_MARKER)
            print(marker_text(SUCCESS_MARKER))
            sys.exit(0)
finally:
    dmesg.terminate()
    try:
        dmesg.wait(timeout=1.0)
    except subprocess.TimeoutExpired:
        dmesg.kill()
        dmesg.wait()
PY
  )

  case "$memory_hotplug_result" in
    *BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK*)
      emit_line BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK
      ;;
    *BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL_*)
      emit_line BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL
      ;;
    *)
      emit_line BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL_RESULT
      write_vdb_marker BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_FAIL
      ;;
  esac
}

check_memory_hotplug_snapshot_marker() {
  if [ ! -d /sys/bus/virtio/devices ]; then
    emit_line BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL_NO_VIRTIO_BUS
    write_vdb_marker BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL
    return
  fi

  memory_device=
  for driver_link in /sys/bus/virtio/devices/*/driver; do
    if [ ! -L "$driver_link" ]; then
      continue
    fi

    driver_target=$(readlink "$driver_link" 2>/dev/null || true)
    if [ "${driver_target##*/}" = virtio_mem ]; then
      memory_device=${driver_link%/driver}
      break
    fi
  done

  if [ -z "$memory_device" ]; then
    emit_line BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL_NO_DEVICE
    write_vdb_marker BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL
    return
  fi

  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL
    return
  fi

  memory_hotplug_snapshot_result=$(
    BANGBANG_MEMORY_HOTPLUG_DEVICE="$memory_device" python3 - <<'PY' 2>/dev/null || true
import glob
import mmap
import os
import select
import subprocess
import sys
import time

DEVICE = "/dev/vdb"
MEMORY_DEVICE = os.environ["BANGBANG_MEMORY_HOTPLUG_DEVICE"]
MEMORY_DEVICE_NAME = os.path.basename(MEMORY_DEVICE)
EXPECTED_REQUESTED_SIZE = 128 * 1024 * 1024
INTERMEDIATE_REQUESTED_SIZE = 64 * 1024 * 1024
FORCED_HOTPLUG_BYTES = 32 * 1024 * 1024
MINIMUM_RESERVE_BYTES = 32 * 1024 * 1024
SECTOR_SIZE = 512
GUEST_OUTPUT_OFFSET = 0
HOST_CONTINUE_OFFSET = SECTOR_SIZE
HOST_REPROBE_OFFSET = 2 * SECTOR_SIZE
READY_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_READY"
SNAPSHOT_READY_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_CAPTURE_READY"
RESTORED_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_BYTES_OK"
OFFLINE_READY_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OFFLINE_READY"
SHRUNK_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_SHRUNK"
REGROWN_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_REGROWN"
REPROBED_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_REPROBED"
UNPLUG_ALL_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_UNPLUG_ALL"
SUCCESS_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OK"
FAIL_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL"
HOST_CONTINUE_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_CONTINUE"
HOST_OFFLINE_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OFFLINE"
HOST_REPROBE_MARKER = b"BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_REPROBE"
STAGE_TIMEOUT_SECONDS = 90.0
MEMORY_TOLERANCE_BYTES = 2 * 1024 * 1024
SENTINEL_SEED = 0xB16B_00B5_A55A_10C5


def marker_text(marker):
    return marker.decode("ascii")


def write_marker(marker):
    try:
        with open(DEVICE, "r+b", buffering=0) as drive:
            drive.seek(GUEST_OUTPUT_OFFSET)
            drive.write(marker.ljust(SECTOR_SIZE, b" "))
            try:
                os.fsync(drive.fileno())
            except OSError:
                pass
    except OSError:
        pass


def fail(reason):
    marker = FAIL_MARKER + b"_" + reason.encode("ascii")
    write_marker(marker)
    print(marker_text(marker))
    sys.exit(1)


def read_marker(offset):
    try:
        with open(DEVICE, "rb", buffering=0) as drive:
            drive.seek(offset)
            return drive.read(SECTOR_SIZE).rstrip(b"\0 ")
    except OSError:
        return b""


def wait_for_host_marker(offset, expected, reason):
    deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if read_marker(offset).startswith(expected):
            return
        time.sleep(0.05)
    fail(reason)


def meminfo_bytes(field):
    try:
        with open("/proc/meminfo", "r", encoding="ascii") as meminfo:
            for line in meminfo:
                name, value = line.split(":", 1)
                if name == field:
                    amount, unit = value.split()
                    if unit != "kB":
                        fail("MEMINFO_UNIT")
                    return int(amount) * 1024
    except (OSError, ValueError):
        pass
    fail("MEMINFO_" + field.upper())


def requested_size_from_line(line):
    if "virtio_mem" not in line or "requested size" not in line:
        return None

    value = line.rsplit(":", 1)[-1].strip().split()
    if value:
        try:
            return int(value[0], 0)
        except ValueError:
            return None
    return None


def wait_for_requested_size(dmesg, expected, reason):
    if dmesg.stdout is None:
        fail("DMESG_STDOUT")
    deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        readable, _, _ = select.select([dmesg.stdout], [], [], remaining)
        if not readable:
            break
        line = dmesg.stdout.readline()
        if not line:
            if dmesg.poll() is not None:
                fail("DMESG_EXIT")
            continue
        value = requested_size_from_line(line.decode("utf-8", "replace").strip())
        if value == expected:
            return
    fail(reason)


def wait_for_hotplugged_total(baseline_total, expected, reason):
    target = baseline_total + expected
    deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        observed = meminfo_bytes("MemTotal")
        if target - MEMORY_TOLERANCE_BYTES <= observed <= target + MEMORY_TOLERANCE_BYTES:
            return
        time.sleep(0.05)
    fail(reason)


def parse_aperture():
    try:
        output = subprocess.check_output(
            ["dmesg"],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8", "replace")
    except (OSError, subprocess.CalledProcessError):
        fail("DMESG_APERTURE")

    start = None
    size = None
    for line in output.splitlines():
        if "virtio_mem" not in line:
            continue
        if "start address" in line:
            try:
                start = int(line.rsplit(":", 1)[-1].strip().split()[0], 0)
            except (IndexError, ValueError):
                pass
        elif "region size" in line:
            try:
                size = int(line.rsplit(":", 1)[-1].strip().split()[0], 0)
            except (IndexError, ValueError):
                pass
    if start is None or size is None or size <= 0:
        fail("APERTURE")
    return start, size


def sentinel_bytes(page_index):
    value = (SENTINEL_SEED ^ page_index) & ((1 << 64) - 1)
    if value == 0:
        value = SENTINEL_SEED
    return value.to_bytes(8, "little")


def write_sentinels(mapping, page_size, length):
    for page_index, offset in enumerate(range(0, length, page_size)):
        mapping[offset : offset + 8] = sentinel_bytes(page_index)


def verify_sentinels(mapping, page_size, length, reason):
    for page_index, offset in enumerate(range(0, length, page_size)):
        if mapping[offset : offset + 8] != sentinel_bytes(page_index):
            fail(reason)


def write_sysfs(path, value, reason):
    try:
        with open(path, "w", encoding="ascii") as target:
            target.write(value)
    except OSError:
        fail(reason)


def wait_for_path(path, exists, reason):
    deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if os.path.exists(path) == exists:
            return
        time.sleep(0.05)
    fail(reason)


def virtio_mem_blocks(aperture_start, aperture_size):
    try:
        with open(
            "/sys/devices/system/memory/block_size_bytes",
            "r",
            encoding="ascii",
        ) as source:
            block_size = int(source.read().strip(), 16)
    except (OSError, ValueError):
        fail("MEMORY_BLOCK_SIZE")

    aperture_end = aperture_start + aperture_size
    selected = []
    for path in glob.glob("/sys/devices/system/memory/memory[0-9]*"):
        try:
            with open(os.path.join(path, "phys_index"), "r", encoding="ascii") as source:
                index = int(source.read().strip(), 16)
        except (OSError, ValueError):
            continue
        address = index * block_size
        if aperture_start <= address < aperture_end:
            selected.append((address, path))
    if not selected:
        fail("MEMORY_BLOCKS")
    return [path for _, path in sorted(selected)]


def offline_blocks(blocks):
    for path in blocks:
        state_path = os.path.join(path, "state")
        try:
            with open(state_path, "r", encoding="ascii") as source:
                state = source.read().strip()
        except OSError:
            fail("MEMORY_BLOCK_STATE_READ")
        if state != "offline":
            write_sysfs(state_path, "offline", "MEMORY_BLOCK_OFFLINE")
            deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
            while time.monotonic() < deadline:
                try:
                    with open(state_path, "r", encoding="ascii") as source:
                        if source.read().strip() == "offline":
                            break
                except OSError:
                    pass
                time.sleep(0.05)
            else:
                fail("MEMORY_BLOCK_OFFLINE_WAIT")


def online_blocks(blocks):
    for path in blocks:
        state_path = os.path.join(path, "state")
        try:
            with open(state_path, "r", encoding="ascii") as source:
                state = source.read().strip()
        except OSError:
            fail("MEMORY_BLOCK_ONLINE_STATE_READ")
        if not state.startswith("online"):
            write_sysfs(state_path, "online_movable", "MEMORY_BLOCK_ONLINE")
            deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
            while time.monotonic() < deadline:
                try:
                    with open(state_path, "r", encoding="ascii") as source:
                        if source.read().strip().startswith("online"):
                            break
                except OSError:
                    pass
                time.sleep(0.05)
            else:
                fail("MEMORY_BLOCK_ONLINE_WAIT")


def wait_for_blocks_removed(blocks, reason):
    deadline = time.monotonic() + STAGE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if all(not os.path.exists(path) for path in blocks):
            return
        time.sleep(0.05)
    fail(reason)


def reprobe_driver():
    driver_root = "/sys/bus/virtio/drivers/virtio_mem"
    driver_link = os.path.join(MEMORY_DEVICE, "driver")
    write_sysfs(
        os.path.join(driver_root, "unbind"),
        MEMORY_DEVICE_NAME,
        "DRIVER_UNBIND",
    )
    wait_for_path(driver_link, False, "DRIVER_UNBIND_WAIT")
    write_sysfs(
        os.path.join(driver_root, "bind"),
        MEMORY_DEVICE_NAME,
        "DRIVER_BIND",
    )
    wait_for_path(driver_link, True, "DRIVER_BIND_WAIT")
    try:
        if os.path.basename(os.readlink(driver_link)) != "virtio_mem":
            fail("DRIVER_BIND_TARGET")
    except OSError:
        fail("DRIVER_BIND_TARGET")


baseline_total = meminfo_bytes("MemTotal")
baseline_available = meminfo_bytes("MemAvailable")
aperture_start, aperture_size = parse_aperture()

try:
    dmesg = subprocess.Popen(
        ["dmesg", "--follow"],
        bufsize=0,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
except OSError:
    fail("DMESG_START")

sentinel_mapping = None
try:
    write_marker(READY_MARKER)
    print(marker_text(READY_MARKER), flush=True)

    wait_for_requested_size(dmesg, EXPECTED_REQUESTED_SIZE, "GROW_REQUEST_TIMEOUT")
    wait_for_hotplugged_total(
        baseline_total,
        EXPECTED_REQUESTED_SIZE,
        "GROW_TOTAL_TIMEOUT",
    )

    page_size = os.sysconf("SC_PAGE_SIZE")
    allocation_bytes = baseline_available + FORCED_HOTPLUG_BYTES
    allocation_bytes = (
        (allocation_bytes + page_size - 1) // page_size
    ) * page_size
    post_plug_available = meminfo_bytes("MemAvailable")
    if post_plug_available < allocation_bytes + MINIMUM_RESERVE_BYTES:
        fail("INSUFFICIENT_HOTPLUG_MEMORY")
    try:
        sentinel_mapping = mmap.mmap(-1, allocation_bytes)
    except (OSError, ValueError):
        fail("SENTINEL_ALLOC")
    write_sentinels(
        sentinel_mapping,
        page_size,
        allocation_bytes,
    )
    verify_sentinels(
        sentinel_mapping,
        page_size,
        allocation_bytes,
        "SENTINEL_SOURCE_VERIFY",
    )
    write_marker(SNAPSHOT_READY_MARKER)
    print(marker_text(SNAPSHOT_READY_MARKER), flush=True)

    wait_for_host_marker(
        HOST_CONTINUE_OFFSET,
        HOST_CONTINUE_MARKER,
        "HOST_CONTINUE_TIMEOUT",
    )
    verify_sentinels(
        sentinel_mapping,
        page_size,
        allocation_bytes,
        "SENTINEL_RESTORE_VERIFY",
    )
    write_marker(RESTORED_MARKER)
    print(marker_text(RESTORED_MARKER), flush=True)
    wait_for_host_marker(
        HOST_CONTINUE_OFFSET,
        HOST_OFFLINE_MARKER,
        "HOST_OFFLINE_TIMEOUT",
    )
    sentinel_mapping.close()
    sentinel_mapping = None
    blocks = virtio_mem_blocks(aperture_start, aperture_size)
    offline_blocks(blocks)
    write_marker(OFFLINE_READY_MARKER)
    print(marker_text(OFFLINE_READY_MARKER), flush=True)

    wait_for_requested_size(
        dmesg,
        INTERMEDIATE_REQUESTED_SIZE,
        "SHRINK_REQUEST_TIMEOUT",
    )
    if any(not os.path.exists(path) for path in blocks):
        fail("SHRINK_PARTIAL_BLOCK")
    write_marker(SHRUNK_MARKER)
    print(marker_text(SHRUNK_MARKER), flush=True)

    wait_for_host_marker(
        HOST_REPROBE_OFFSET,
        HOST_REPROBE_MARKER,
        "HOST_REPROBE_TIMEOUT",
    )
    reprobe_driver()
    wait_for_requested_size(
        dmesg,
        INTERMEDIATE_REQUESTED_SIZE,
        "REPROBE_UNPLUG_ALL_TIMEOUT",
    )
    write_marker(UNPLUG_ALL_MARKER)
    print(marker_text(UNPLUG_ALL_MARKER), flush=True)

    wait_for_requested_size(
        dmesg,
        INTERMEDIATE_REQUESTED_SIZE,
        "REPROBE_REPLUG_REQUEST_TIMEOUT",
    )
    for path in blocks:
        wait_for_path(path, True, "REPROBE_MEMORY_BLOCK_WAIT")
    online_blocks(blocks)
    wait_for_hotplugged_total(
        baseline_total,
        INTERMEDIATE_REQUESTED_SIZE,
        "REPROBE_TOTAL_TIMEOUT",
    )
    write_marker(REPROBED_MARKER)
    print(marker_text(REPROBED_MARKER), flush=True)

    wait_for_requested_size(dmesg, EXPECTED_REQUESTED_SIZE, "REGROW_REQUEST_TIMEOUT")
    wait_for_hotplugged_total(
        baseline_total,
        EXPECTED_REQUESTED_SIZE,
        "REGROW_TOTAL_TIMEOUT",
    )
    blocks = virtio_mem_blocks(aperture_start, aperture_size)
    offline_blocks(blocks)
    write_marker(REGROWN_MARKER)
    print(marker_text(REGROWN_MARKER), flush=True)

    wait_for_requested_size(dmesg, 0, "FINAL_SHRINK_REQUEST_TIMEOUT")
    wait_for_blocks_removed(blocks, "FINAL_SHRINK_REMOVE_TIMEOUT")
    wait_for_hotplugged_total(baseline_total, 0, "FINAL_SHRINK_TOTAL_TIMEOUT")
    write_marker(SUCCESS_MARKER)
    print(marker_text(SUCCESS_MARKER))
finally:
    if sentinel_mapping is not None:
        sentinel_mapping.close()
    dmesg.terminate()
    try:
        dmesg.wait(timeout=1.0)
    except subprocess.TimeoutExpired:
        dmesg.kill()
        dmesg.wait()
PY
  )

  case "$memory_hotplug_snapshot_result" in
    *BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OK*)
      emit_line BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_OK
      ;;
    *BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL_*)
      emit_line BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL
      ;;
    *)
      emit_line BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL_RESULT
      write_vdb_marker BANGBANG_MEMORY_HOTPLUG_SNAPSHOT_FAIL
      ;;
  esac
}

check_cache_fdt_marker() {
  cache_report=/dev/bangbang-cache-report
  cache_report_limit=65536

  cache_report_fail() {
    emit_line BANGBANG_CACHE_FDT_GUEST_CHECK_FAIL
    write_vdb_marker BANGBANG_CACHE_FDT_GUEST_CHECK_FAIL
  }

  if [ ! -b /dev/vdb ] || [ ! -d /sys/devices/system/cpu ]; then
    cache_report_fail
    return
  fi

  for cache_cpu_online in /sys/devices/system/cpu/cpu[0-9]*/online; do
    if [ ! -e "$cache_cpu_online" ]; then
      continue
    fi
    cache_cpu_online_value=$(tr -d '\r\n' < "$cache_cpu_online" 2>/dev/null || true)
    if [ "$cache_cpu_online_value" = 0 ] \
      && ! printf '1\n' > "$cache_cpu_online" 2>/dev/null; then
      cache_report_fail
      return
    fi
  done

  if ! printf '%s\n' BANGBANG_CACHE_REPORT_V1 > "$cache_report"; then
    cache_report_fail
    return
  fi

  cache_record_count=0
  for cache_cpu_path in /sys/devices/system/cpu/cpu[0-9]*; do
    if [ ! -d "$cache_cpu_path/cache" ]; then
      continue
    fi
    cache_cpu=${cache_cpu_path##*/cpu}
    case "$cache_cpu" in
      '' | *[!0-9]*)
        cache_report_fail
        return
        ;;
    esac

    for cache_index_path in "$cache_cpu_path"/cache/index[0-9]*; do
      if [ ! -d "$cache_index_path" ]; then
        continue
      fi
      for cache_fact in level type size coherency_line_size number_of_sets ways_of_associativity shared_cpu_list; do
        if [ ! -r "$cache_index_path/$cache_fact" ]; then
          cache_report_fail
          return
        fi
      done

      cache_level=$(tr -d '\r\n' < "$cache_index_path/level" 2>/dev/null || true)
      cache_type=$(tr -d '\r\n' < "$cache_index_path/type" 2>/dev/null || true)
      cache_size=$(tr -d '\r\n' < "$cache_index_path/size" 2>/dev/null || true)
      cache_line=$(tr -d '\r\n' < "$cache_index_path/coherency_line_size" 2>/dev/null || true)
      cache_sets=$(tr -d '\r\n' < "$cache_index_path/number_of_sets" 2>/dev/null || true)
      cache_ways=$(tr -d '\r\n' < "$cache_index_path/ways_of_associativity" 2>/dev/null || true)
      cache_shared=$(tr -d '\r\n' < "$cache_index_path/shared_cpu_list" 2>/dev/null || true)

      case "$cache_level:$cache_line:$cache_sets:$cache_ways" in
        *[!0-9:]* | :* | *::*)
          cache_report_fail
          return
          ;;
      esac
      case "$cache_shared" in
        '' | *[!0-9,-]*)
          cache_report_fail
          return
          ;;
      esac
      case "$cache_type" in
        Data) cache_type=D ;;
        Instruction) cache_type=I ;;
        Unified) cache_type=U ;;
        *)
          cache_report_fail
          return
          ;;
      esac

      case "$cache_size" in
        *K)
          cache_size_number=${cache_size%K}
          cache_size_multiplier=1024
          ;;
        *M)
          cache_size_number=${cache_size%M}
          cache_size_multiplier=1048576
          ;;
        *G)
          cache_size_number=${cache_size%G}
          cache_size_multiplier=1073741824
          ;;
        *)
          cache_size_number=$cache_size
          cache_size_multiplier=1
          ;;
      esac
      case "$cache_size_number" in
        '' | *[!0-9]*)
          cache_report_fail
          return
          ;;
      esac
      cache_size_bytes=$((cache_size_number * cache_size_multiplier))

      if ! printf '%s|%s|%s|%s|%s|%s|%s|%s\n' \
        "$cache_cpu" \
        "$cache_level" \
        "$cache_type" \
        "$cache_size_bytes" \
        "$cache_line" \
        "$cache_sets" \
        "$cache_ways" \
        "$cache_shared" \
        >> "$cache_report"; then
        cache_report_fail
        return
      fi
      cache_record_count=$((cache_record_count + 1))
    done
  done

  if [ "$cache_record_count" -eq 0 ]; then
    cache_report_fail
    return
  fi
  cache_report_size=$(wc -c < "$cache_report" 2>/dev/null || true)
  cache_report_size=${cache_report_size##* }
  case "$cache_report_size" in
    '' | *[!0-9]*)
      cache_report_fail
      return
      ;;
  esac
  if [ "$cache_report_size" -gt "$cache_report_limit" ]; then
    cache_report_fail
    return
  fi

  if ! dd if="$cache_report" of=/dev/vdb bs=4096 conv=notrunc,fsync 2>/dev/null; then
    cache_report_fail
    return
  fi
  emit_line BANGBANG_CACHE_FDT_GUEST_CHECK_OK
}

check_rtc_marker() {
  if [ ! -c /dev/rtc0 ]; then
    emit_line BANGBANG_RTC_GUEST_CHECK_FAIL_NO_RTC0
    write_vdb_marker BANGBANG_RTC_GUEST_CHECK_FAIL
    return
  fi

  rtc_name=$(cat /sys/class/rtc/rtc0/name 2>/dev/null || true)
  rtc_driver=$(readlink /sys/class/rtc/rtc0/device/driver 2>/dev/null || true)
  rtc_proc=$(cat /proc/driver/rtc 2>/dev/null || true)
  rtc_dmesg=$(dmesg 2>/dev/null | grep -m 1 -e 'rtc-pl031' -e 'pl031' || true)

  case "$rtc_name $rtc_driver $rtc_proc $rtc_dmesg" in
    *rtc-pl031*|*pl031*|*PL031*)
      emit_line BANGBANG_RTC_GUEST_CHECK_OK
      write_vdb_marker BANGBANG_RTC_GUEST_CHECK_OK
      ;;
    *)
      emit_line BANGBANG_RTC_GUEST_CHECK_FAIL_NOT_PL031
      write_vdb_marker BANGBANG_RTC_GUEST_CHECK_FAIL
      ;;
  esac
}

check_vmgenid_marker() {
  vmgenid_path=
  for candidate in /proc/device-tree/vmgenid /sys/firmware/devicetree/base/vmgenid; do
    if [ -d "$candidate" ]; then
      vmgenid_path=$candidate
      break
    fi
  done

  if [ -z "$vmgenid_path" ]; then
    emit_line BANGBANG_VMGENID_GUEST_CHECK_FAIL_NO_NODE
    write_vdb_marker BANGBANG_VMGENID_GUEST_CHECK_FAIL
    return
  fi

  if [ ! -r "$vmgenid_path/compatible" ]; then
    emit_line BANGBANG_VMGENID_GUEST_CHECK_FAIL_NO_COMPATIBLE
    write_vdb_marker BANGBANG_VMGENID_GUEST_CHECK_FAIL
    return
  fi

  if ! grep -q 'microsoft,vmgenid' "$vmgenid_path/compatible" 2>/dev/null; then
    emit_line BANGBANG_VMGENID_GUEST_CHECK_FAIL_COMPATIBLE
    write_vdb_marker BANGBANG_VMGENID_GUEST_CHECK_FAIL
    return
  fi

  if [ ! -r "$vmgenid_path/reg" ]; then
    emit_line BANGBANG_VMGENID_GUEST_CHECK_FAIL_NO_REG
    write_vdb_marker BANGBANG_VMGENID_GUEST_CHECK_FAIL
    return
  fi

  set -- $(wc -c < "$vmgenid_path/reg" 2>/dev/null || printf '0')
  if [ "${1:-0}" != 16 ]; then
    emit_line BANGBANG_VMGENID_GUEST_CHECK_FAIL_REG_SIZE
    write_vdb_marker BANGBANG_VMGENID_GUEST_CHECK_FAIL
    return
  fi

  emit_line BANGBANG_VMGENID_GUEST_CHECK_OK
  write_vdb_marker BANGBANG_VMGENID_GUEST_CHECK_OK
}

check_vmclock_marker() {
  vmclock_path=
  for compatible_path in $(find /proc/device-tree /sys/firmware/devicetree/base -type f -name compatible 2>/dev/null); do
    if grep -q 'amazon,vmclock' "$compatible_path" 2>/dev/null; then
      vmclock_path=${compatible_path%/compatible}
      break
    fi
  done

  if [ -z "$vmclock_path" ]; then
    emit_line BANGBANG_VMCLOCK_GUEST_CHECK_FAIL_NO_NODE
    write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_FAIL
    return
  fi

  case "${vmclock_path##*/}" in
    ptp@*) ;;
    *)
      emit_line BANGBANG_VMCLOCK_GUEST_CHECK_FAIL_NODE_NAME
      write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_FAIL
      return
      ;;
  esac

  if [ ! -r "$vmclock_path/reg" ]; then
    emit_line BANGBANG_VMCLOCK_GUEST_CHECK_FAIL_NO_REG
    write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_FAIL
    return
  fi

  set -- $(wc -c < "$vmclock_path/reg" 2>/dev/null || printf '0')
  if [ "${1:-0}" != 16 ]; then
    emit_line BANGBANG_VMCLOCK_GUEST_CHECK_FAIL_REG_SIZE
    write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_FAIL
    return
  fi

  if ! command -v od >/dev/null 2>&1; then
    emit_line BANGBANG_VMCLOCK_GUEST_CHECK_FAIL_NO_OD
    write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_FAIL
    return
  fi

  reg_hex=$(od -An -tx1 -v "$vmclock_path/reg" 2>/dev/null | tr -d ' \n' || true)
  case "$reg_hex" in
    *0000000000001000)
      emit_line BANGBANG_VMCLOCK_GUEST_CHECK_OK
      write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_OK
      ;;
    *)
      emit_line BANGBANG_VMCLOCK_GUEST_CHECK_FAIL_REG_VALUE
      write_vdb_marker BANGBANG_VMCLOCK_GUEST_CHECK_FAIL
      ;;
  esac
}

flush_writeback_block_marker() {
  if [ ! -b /dev/vdb ]; then
    emit_line BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_NO_VDB
    write_vdb_marker BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL
    return
  fi

  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL
    return
  fi

  if cmdline_has bangbang.expect-pci-data=1; then
    if ! pci_function_has_identity 0000:00:01.0 0x1042 \
      || ! pci_function_has_identity 0000:00:02.0 0x1042; then
      emit_line BANGBANG_PCI_BLOCK_IDENTITIES_FAIL
      write_vdb_marker BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL
      return
    fi
    emit_line BANGBANG_PCI_BLOCK_IDENTITIES_OK
  fi

  block_flush_result=$(
    python3 - <<'PY' 2>/dev/null || true
import os
import sys

DEVICE = "/dev/vdb"
SECTOR_SIZE = 512
PRE_FLUSH_MARKER = b"BANGBANG_BLOCK_WRITEBACK_FLUSH_BEFORE"
SUCCESS_MARKER = b"BANGBANG_BLOCK_WRITEBACK_FLUSH_OK"


def fail(reason):
    print(f"BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_{reason}")
    sys.exit(1)


def write_sector(fd, marker):
    payload = marker.ljust(SECTOR_SIZE, b" ")
    view = memoryview(payload)
    while view:
        try:
            written = os.write(fd, view)
        except OSError:
            fail("WRITE")
        if written <= 0:
            fail("WRITE_SHORT")
        view = view[written:]


try:
    fd = os.open(DEVICE, os.O_RDWR)
except OSError:
    fail("OPEN")

try:
    try:
        os.lseek(fd, 0, os.SEEK_SET)
    except OSError:
        fail("SEEK")
    write_sector(fd, PRE_FLUSH_MARKER)
    try:
        os.fsync(fd)
    except OSError:
        fail("FSYNC")
    try:
        os.lseek(fd, 0, os.SEEK_SET)
    except OSError:
        fail("SEEK")
    write_sector(fd, SUCCESS_MARKER)
finally:
    os.close(fd)

print(SUCCESS_MARKER.decode("ascii"))
PY
  )

  if [ "$block_flush_result" = BANGBANG_BLOCK_WRITEBACK_FLUSH_OK ]; then
    emit_line BANGBANG_BLOCK_WRITEBACK_FLUSH_OK
  else
    case "$block_flush_result" in
      BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_*) emit_line "$block_flush_result" ;;
      *) emit_line BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL_RESULT ;;
    esac
    write_vdb_marker BANGBANG_BLOCK_WRITEBACK_FLUSH_FAIL
  fi
}

vhost_user_block_fail() {
  reason=$1
  marker="BANGBANG_VHOST_USER_BLOCK_FAIL_$reason"
  emit_line "$marker"
  write_vdb_marker "$marker"
  sync /dev/vdb 2>/dev/null || sync
}

check_vhost_user_block_marker() {
  expected_mode=$1
  host_marker=BANGBANG_VHOST_USER_BLOCK_HOST
  success_marker="BANGBANG_VHOST_USER_BLOCK_${expected_mode}_OK"

  if [ ! -b /dev/vda ] || [ ! -b /dev/vdb ]; then
    vhost_user_block_fail NO_BLOCK_DEVICE
    return
  fi
  if cmdline_has bangbang.expect-partuuid=0eaa91a0-01; then
    if ! cmdline_has root=PARTUUID=0eaa91a0-01; then
      vhost_user_block_fail ROOT_PARTUUID_CMDLINE
      return
    fi
  elif ! cmdline_has root=/dev/vda; then
    vhost_user_block_fail ROOT_CMDLINE
    return
  fi
  if [ "$(cat /sys/class/block/vdb/size 2>/dev/null || true)" != 8 ]; then
    vhost_user_block_fail SCRATCH_CAPACITY
    return
  fi
  if [ "$expected_mode" = ro ]; then
    if [ "$(cat /sys/class/block/vda/ro 2>/dev/null || true)" != 1 ]; then
      vhost_user_block_fail ROOT_FEATURE_RO
      return
    fi
  elif [ "$(cat /sys/class/block/vda/ro 2>/dev/null || true)" != 0 ]; then
    vhost_user_block_fail ROOT_FEATURE_RW
    return
  fi
  if cmdline_has bangbang.expect-pci-data=1; then
    if ! pci_function_has_identity 0000:00:01.0 0x1042 \
      || ! pci_function_has_identity 0000:00:02.0 0x1042; then
      vhost_user_block_fail PCI_IDENTITY
      return
    fi
    emit_line BANGBANG_PCI_VHOST_USER_BLOCK_IDENTITIES_OK
  fi

  root_options=$(awk '$2 == "/" { print $4; exit }' /proc/mounts 2>/dev/null || true)
  case ",$root_options," in
    *,"$expected_mode",*) ;;
    *)
      vhost_user_block_fail "ROOT_MOUNT_${expected_mode}"
      return
      ;;
  esac
  if [ "$expected_mode" = ro ]; then
    if touch /bangbang-vhost-user-root-ro-probe 2>/dev/null; then
      vhost_user_block_fail ROOT_RO_WRITE
      return
    fi
  else
    if ! printf '%s' BANGBANG_VHOST_USER_ROOT_RW_PROBE \
      > /bangbang-vhost-user-root-rw-probe 2>/dev/null; then
      vhost_user_block_fail ROOT_RW_WRITE
      return
    fi
    sync /bangbang-vhost-user-root-rw-probe 2>/dev/null || sync
    if [ "$(cat /bangbang-vhost-user-root-rw-probe 2>/dev/null || true)" \
      != BANGBANG_VHOST_USER_ROOT_RW_PROBE ]; then
      vhost_user_block_fail ROOT_RW_VERIFY
      return
    fi
  fi

  if ! vdb_starts_with_marker "$host_marker"; then
    vhost_user_block_fail SCRATCH_READ
    return
  fi
  if ! printf '%-512s' "$success_marker" \
    | dd of=/dev/vdb bs=512 count=1 conv=notrunc oflag=direct,sync 2>/dev/null; then
    vhost_user_block_fail SCRATCH_WRITE
    return
  fi
  if ! vdb_starts_with_marker "$success_marker"; then
    vhost_user_block_fail SCRATCH_VERIFY
    return
  fi

  if cmdline_has bangbang.expect-vhost-resize=1; then
    attempts=0
    while [ "$(cat /sys/class/block/vdb/size 2>/dev/null || true)" != 10 ]; do
      if [ "$attempts" -ge 30 ]; then
        vhost_user_block_fail CONFIG_RESIZE
        return
      fi
      sleep 1
      attempts=$((attempts + 1))
    done
    if ! write_vdb_sector_marker BANGBANG_VHOST_CONFIG_RESIZED 9; then
      vhost_user_block_fail CONFIG_RESIZE_WRITE
      return
    fi
    emit_line BANGBANG_VHOST_CONFIG_RESIZED
  fi

  emit_line "$success_marker"
}

first_pmem_device() {
  for pmem_device_path in /dev/pmem*; do
    if [ -b "$pmem_device_path" ]; then
      printf '%s\n' "$pmem_device_path"
      return 0
    fi
  done

  return 1
}

read_flush_pmem_marker() {
  host_marker=BANGBANG_PMEM_HOST_MARKER
  guest_marker=BANGBANG_PMEM_GUEST_FLUSH_OK
  guest_marker_offset=4096
  pmem_device=$(first_pmem_device || true)
  if [ -z "$pmem_device" ]; then
    emit_line BANGBANG_PMEM_READ_FLUSH_FAIL_NO_DEVICE
    write_vdb_marker BANGBANG_PMEM_READ_FLUSH_FAIL
    return
  fi

  if cmdline_has bangbang.expect-pci-data=1; then
    if ! pci_function_has_identity 0000:00:01.0 0x1042 \
      || ! pci_function_has_identity 0000:00:02.0 0x105b; then
      emit_line BANGBANG_PCI_PMEM_IDENTITIES_FAIL
      write_vdb_marker BANGBANG_PMEM_READ_FLUSH_FAIL
      return
    fi
    emit_line BANGBANG_PCI_PMEM_IDENTITIES_OK
  fi

  host_marker_value=$(
    dd if="$pmem_device" bs=1 count="${#host_marker}" 2>/dev/null || true
  )
  if [ "$host_marker_value" != "$host_marker" ]; then
    emit_line BANGBANG_PMEM_READ_FLUSH_FAIL_BAD_MARKER
    write_vdb_marker BANGBANG_PMEM_READ_FLUSH_FAIL
    return
  fi

  if ! printf '%s' "$guest_marker" \
    | dd of="$pmem_device" bs=1 seek="$guest_marker_offset" conv=notrunc 2>/dev/null; then
    emit_line BANGBANG_PMEM_READ_FLUSH_FAIL_WRITE
    write_vdb_marker BANGBANG_PMEM_READ_FLUSH_FAIL
    return
  fi

  sync "$pmem_device" 2>/dev/null || sync
  emit_line BANGBANG_PMEM_READ_FLUSH_OK
  write_vdb_marker BANGBANG_PMEM_READ_FLUSH_OK
}

pmem_root_fail() {
  reason=$1
  marker="BANGBANG_PMEM_ROOT_FAIL_$reason"
  emit_line "$marker"
  write_vda_marker "$marker"
  sync /dev/vda 2>/dev/null || true
}

check_pmem_root_marker() {
  expected_mode=$1
  if [ "$expected_mode" = ro ]; then
    success_marker=BANGBANG_PMEM_ROOT_RO_OK
  else
    success_marker=BANGBANG_PMEM_ROOT_RW_OK
  fi

  if [ ! -b /dev/pmem0 ]; then
    pmem_root_fail NO_PMEM0
    return
  fi
  if ! cmdline_has root=/dev/pmem0; then
    pmem_root_fail CMDLINE
    return
  fi
  if cmdline_has bangbang.expect-pci-data=1; then
    if ! pci_function_has_identity 0000:00:01.0 0x105b; then
      pmem_root_fail PCI_IDENTITY
      return
    fi
    emit_line BANGBANG_PCI_PMEM_IDENTITIES_OK
  fi

  root_options=$(awk '$2 == "/" { print $4; exit }' /proc/mounts 2>/dev/null || true)
  case ",$root_options," in
    *,"$expected_mode",*) ;;
    *)
      pmem_root_fail "MOUNT_${expected_mode}"
      return
      ;;
  esac

  if [ "$expected_mode" = ro ]; then
    if touch /bangbang-pmem-root-ro-probe 2>/dev/null; then
      pmem_root_fail RO_WRITE
      return
    fi
  else
    if ! printf '%s' BANGBANG_PMEM_ROOT_RW_PROBE \
      > /bangbang-pmem-root-rw-probe 2>/dev/null; then
      pmem_root_fail RW_WRITE
      return
    fi
    sync /bangbang-pmem-root-rw-probe 2>/dev/null || sync
    probe=$(cat /bangbang-pmem-root-rw-probe 2>/dev/null || true)
    if [ "$probe" != BANGBANG_PMEM_ROOT_RW_PROBE ]; then
      pmem_root_fail RW_READ
      return
    fi
  fi

  emit_line "$success_marker"
  write_vda_marker "$success_marker"
  sync /dev/vda 2>/dev/null || true
}

request_mmds_v2_token() {
  failure_prefix=$1
  failure_marker=$2
  token_ttl=$3
  mmds_token=$(
    curl \
      --fail \
      --silent \
      --show-error \
      --connect-timeout 2 \
      --max-time 5 \
      -X PUT \
      -H "X-metadata-token-ttl-seconds: $token_ttl" \
      http://169.254.169.254/latest/api/token \
      2>/dev/null || true
  )

  if ! mmds_v2_token_has_expected_shape "$mmds_token"; then
    emit_line "${failure_prefix}_TOKEN"
    write_vdb_marker "$failure_marker"
    return 1
  fi

  return 0
}

mmds_v2_token_has_expected_shape() {
  candidate_token=$1
  [ "${#candidate_token}" -eq 48 ] || return 1
  case "$candidate_token" in
    *[!A-Za-z0-9+/]*) return 1 ;;
  esac
  return 0
}

wait_for_mmds_v2_peer_token() {
  failure_prefix=$1
  failure_marker=$2
  peer_now=$(date +%s 2>/dev/null || true)
  case "$peer_now" in
    ''|*[!0-9]*)
      emit_line "${failure_prefix}_PEER_CLOCK"
      write_vdb_marker "$failure_marker"
      return 1
      ;;
  esac
  peer_deadline=$((peer_now + 300))

  while ! vdb_sector_starts_with_marker BANGBANG_MMDS_PEER_TOKEN_READY 4; do
    peer_now=$(date +%s 2>/dev/null || true)
    case "$peer_now" in
      ''|*[!0-9]*)
        emit_line "${failure_prefix}_PEER_CLOCK"
        write_vdb_marker "$failure_marker"
        return 1
        ;;
    esac
    if [ "$peer_now" -ge "$peer_deadline" ]; then
      emit_line "${failure_prefix}_PEER_TIMEOUT"
      write_vdb_marker "$failure_marker"
      return 1
    fi
  done

  peer_mmds_token=$(dd if=/dev/vdb bs=512 skip=3 count=1 2>/dev/null \
    | dd bs=1 count=48 2>/dev/null || true)
  if ! mmds_v2_token_has_expected_shape "$peer_mmds_token"; then
    emit_line "${failure_prefix}_PEER_TOKEN"
    write_vdb_marker "$failure_marker"
    return 1
  fi
  return 0
}

mmds_v2_peer_token_is_rejected() {
  peer_status=$(
    curl \
      --silent \
      --output /dev/null \
      --write-out '%{http_code}' \
      --connect-timeout 2 \
      --max-time 5 \
      -H "X-metadata-token: $peer_mmds_token" \
      http://169.254.169.254/meta-data/bangbang-marker \
      2>/dev/null || true
  )
  [ "$peer_status" = 401 ]
}

get_mmds_v2_value() {
  mmds_path=$1
  curl \
    --fail \
    --silent \
    --show-error \
    --connect-timeout 2 \
    --max-time 5 \
    -H "X-metadata-token: $mmds_token" \
    "http://169.254.169.254/$mmds_path" \
    2>/dev/null
}

fetch_mmds_v2_marker() {
  if ! prepare_mmds_network BANGBANG_MMDS_V2_FETCH_FAIL BANGBANG_MMDS_V2_FETCH_FAIL; then
    return
  fi

  if ! request_mmds_v2_token BANGBANG_MMDS_V2_FETCH_FAIL BANGBANG_MMDS_V2_FETCH_FAIL 60; then
    return
  fi

  mmds_value=$(get_mmds_v2_value meta-data/bangbang-marker || true)

  if [ "$mmds_value" = BANGBANG_MMDS_GUEST_VALUE ]; then
    emit_line BANGBANG_MMDS_V2_FETCH_OK
    write_vdb_marker BANGBANG_MMDS_V2_GUEST_FETCH_OK
  else
    emit_line BANGBANG_MMDS_V2_FETCH_FAIL_RESPONSE
    write_vdb_marker BANGBANG_MMDS_V2_FETCH_FAIL
  fi
}

certify_mmds_snapshot_restore() {
  failure_marker=BANGBANG_MMDS_SNAPSHOT_FAIL
  if ! prepare_mmds_network "$failure_marker" "$failure_marker"; then
    return
  fi

  if [ ! -b /dev/vdb ]; then
    emit_line BANGBANG_MMDS_SNAPSHOT_FAIL_NO_VDB
    write_vdb_marker BANGBANG_MMDS_SNAPSHOT_FAIL_NO_VDB
    return
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_MMDS_SNAPSHOT_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_MMDS_SNAPSHOT_FAIL_NO_PYTHON
    return
  fi

  expected_mac=06:00:00:00:00:71
  actual_mac=$(cat "/sys/class/net/$mmds_iface/address" 2>/dev/null || true)
  if [ "$actual_mac" != "$expected_mac" ]; then
    emit_line BANGBANG_MMDS_SNAPSHOT_FAIL_MAC
    write_vdb_marker BANGBANG_MMDS_SNAPSHOT_FAIL_MAC
    return
  fi

  if cmdline_has bangbang.expect-pci-data=1; then
    network_device_path=$(readlink -f "/sys/class/net/$mmds_iface/device" 2>/dev/null || true)
    network_driver=$(readlink "$network_device_path/driver" 2>/dev/null || true)
    network_pci_path=${network_device_path%/*}
    network_vendor=$(cat "$network_pci_path/vendor" 2>/dev/null || true)
    network_device=$(cat "$network_pci_path/device" 2>/dev/null || true)
    if [ "${network_driver##*/}" != virtio_net ] \
      || [ "$network_vendor" != 0x1af4 ] \
      || [ "$network_device" != 0x1041 ]; then
      emit_line BANGBANG_MMDS_SNAPSHOT_FAIL_PCI_IDENTITY
      write_vdb_marker BANGBANG_MMDS_SNAPSHOT_FAIL_PCI_IDENTITY
      return
    fi
    emit_line BANGBANG_MMDS_SNAPSHOT_PCI_IDENTITY_OK
  fi

  mmds_snapshot_version=V1
  if cmdline_has bangbang.mmds-snapshot-v2=1; then
    mmds_snapshot_version=V2
  fi

  mmds_snapshot_result=$(
    python3 - "$mmds_snapshot_version" <<'PY' 2>/dev/null || true
import os
import socket
import sys
import time

VERSION = sys.argv[1]
DEVICE = "/dev/vdb"
MMDS_ADDRESS = ("169.254.169.254", 80)
SECTOR_SIZE = 512
STATUS_OFFSET = 0
HOST_CONTINUE_OFFSET = SECTOR_SIZE
CONNECTION_RESULT_OFFSET = 2 * SECTOR_SIZE
TOKEN_RESULT_OFFSET = 3 * SECTOR_SIZE
FRESH_RESULT_OFFSET = 4 * SECTOR_SIZE
SOURCE_VALUE = b"BANGBANG_MMDS_SNAPSHOT_SOURCE"
DESTINATION_VALUE = b"BANGBANG_MMDS_SNAPSHOT_DESTINATION"
READY_MARKER = b"BANGBANG_MMDS_SNAPSHOT_CAPTURE_READY"
HOST_CONTINUE_MARKER = b"BANGBANG_MMDS_SNAPSHOT_CONTINUE"
CONNECTION_LOST_MARKER = b"BANGBANG_MMDS_SNAPSHOT_CONNECTION_LOST"
TOKEN_REJECTED_MARKER = b"BANGBANG_MMDS_SNAPSHOT_TOKEN_REJECTED"
V1_MARKER = b"BANGBANG_MMDS_SNAPSHOT_V1_NO_TOKEN"
FRESH_MARKER = b"BANGBANG_MMDS_SNAPSHOT_FRESH_FETCH_OK"
SUCCESS_MARKER = b"BANGBANG_MMDS_SNAPSHOT_OK"
FAILURE_MARKER = b"BANGBANG_MMDS_SNAPSHOT_FAIL"
TIMEOUT_SECONDS = 90.0


def marker_text(marker):
    return marker.decode("ascii")


def write_sector(offset, marker):
    try:
        with open(DEVICE, "r+b", buffering=0) as drive:
            drive.seek(offset)
            drive.write(marker.ljust(SECTOR_SIZE, b" "))
            os.fsync(drive.fileno())
    except OSError:
        return False
    return True


def read_sector(offset):
    try:
        with open(DEVICE, "rb", buffering=0) as drive:
            drive.seek(offset)
            return drive.read(SECTOR_SIZE)
    except OSError:
        return b""


def fail(reason):
    marker = FAILURE_MARKER + b"_" + reason.encode("ascii")
    write_sector(STATUS_OFFSET, marker)
    print(marker_text(marker), flush=True)
    sys.exit(1)


def parse_response(response):
    header_end = response.find(b"\r\n\r\n")
    if header_end < 0:
        fail("HTTP_HEADERS")
    status_line = response[:header_end].split(b"\r\n", 1)[0]
    fields = status_line.split()
    if len(fields) < 2:
        fail("HTTP_STATUS")
    try:
        status = int(fields[1])
    except ValueError:
        fail("HTTP_STATUS")
    headers = {}
    for line in response[len(status_line) + 2 : header_end].split(b"\r\n"):
        name, separator, value = line.partition(b":")
        if separator:
            headers[name.strip().lower()] = value.strip()
    body = response[header_end + 4 :]
    content_length = headers.get(b"content-length")
    if content_length is not None:
        try:
            expected = int(content_length)
        except ValueError:
            fail("HTTP_LENGTH")
        if len(body) < expected:
            fail("HTTP_BODY")
        body = body[:expected]
    return status, body


def request(method, path, headers=None):
    request_headers = {
        "Host": "169.254.169.254",
        "Accept": "*/*",
        "Connection": "close",
    }
    if headers:
        request_headers.update(headers)
    encoded = [f"{method} {path} HTTP/1.1\r\n".encode("ascii")]
    encoded.extend(
        f"{name}: {value}\r\n".encode("ascii")
        for name, value in request_headers.items()
    )
    encoded.append(b"Content-Length: 0\r\n\r\n")
    with socket.create_connection(MMDS_ADDRESS, timeout=5.0) as connection:
        connection.settimeout(5.0)
        connection.sendall(b"".join(encoded))
        response = bytearray()
        while True:
            chunk = connection.recv(65536)
            if not chunk:
                break
            response.extend(chunk)
            header_end = response.find(b"\r\n\r\n")
            if header_end >= 0:
                for line in response[:header_end].split(b"\r\n")[1:]:
                    name, separator, value = line.partition(b":")
                    if separator and name.strip().lower() == b"content-length":
                        try:
                            expected = int(value.strip())
                        except ValueError:
                            fail("HTTP_LENGTH")
                        if len(response) >= header_end + 4 + expected:
                            return parse_response(bytes(response))
        return parse_response(bytes(response))


def request_token():
    status, token = request(
        "PUT",
        "/latest/api/token",
        {"X-metadata-token-ttl-seconds": "600"},
    )
    if status != 200 or len(token) != 48:
        fail("TOKEN_CREATE")
    try:
        token.decode("ascii")
    except UnicodeDecodeError:
        fail("TOKEN_ENCODING")
    return token


def get_value(token=None):
    headers = {}
    if token is not None:
        headers["X-metadata-token"] = token.decode("ascii")
    return request("GET", "/meta-data/bangbang-marker", headers)


old_token = None
if VERSION == "V2":
    old_token = request_token()
    status, source_value = get_value(old_token)
elif VERSION == "V1":
    status, source_value = get_value()
else:
    fail("VERSION")
if status != 200 or source_value != SOURCE_VALUE:
    fail("SOURCE_FETCH")

try:
    stale_connection = socket.create_connection(MMDS_ADDRESS, timeout=5.0)
    stale_connection.settimeout(5.0)
    stale_connection.sendall(
        b"GET /meta-data/bangbang-marker HTTP/1.1\r\n"
        b"Host: 169.254.169.254\r\n"
    )
except OSError:
    fail("SOURCE_CONNECTION")

if not write_sector(STATUS_OFFSET, READY_MARKER):
    fail("READY_MARKER")
print(marker_text(READY_MARKER), flush=True)

deadline = time.monotonic() + TIMEOUT_SECONDS
while time.monotonic() < deadline:
    if read_sector(HOST_CONTINUE_OFFSET).startswith(HOST_CONTINUE_MARKER):
        break
    time.sleep(0.05)
else:
    fail("HOST_CONTINUE")

suffix = b"Accept: */*\r\n"
if old_token is not None:
    suffix += b"X-metadata-token: " + old_token + b"\r\n"
suffix += b"\r\n"
connection_lost = False
try:
    stale_connection.sendall(suffix)
    stale_response = stale_connection.recv(65536)
    connection_lost = not stale_response
except OSError:
    connection_lost = True
finally:
    stale_connection.close()
if not connection_lost:
    fail("CONNECTION_SURVIVED")
if not write_sector(CONNECTION_RESULT_OFFSET, CONNECTION_LOST_MARKER):
    fail("CONNECTION_MARKER")

if old_token is not None:
    old_status, _ = get_value(old_token)
    if old_status != 401:
        fail("OLD_TOKEN_ACCEPTED")
    if not write_sector(TOKEN_RESULT_OFFSET, TOKEN_REJECTED_MARKER):
        fail("TOKEN_REJECTED_MARKER")
    fresh_token = request_token()
    if fresh_token == old_token:
        fail("TOKEN_REUSED")
    fresh_status, fresh_value = get_value(fresh_token)
else:
    if not write_sector(TOKEN_RESULT_OFFSET, V1_MARKER):
        fail("V1_MARKER")
    fresh_status, fresh_value = get_value()
if fresh_status != 200 or fresh_value != DESTINATION_VALUE:
    fail("FRESH_FETCH")
if not write_sector(FRESH_RESULT_OFFSET, FRESH_MARKER):
    fail("FRESH_MARKER")
if not write_sector(STATUS_OFFSET, SUCCESS_MARKER):
    fail("SUCCESS_MARKER")
print(marker_text(CONNECTION_LOST_MARKER), flush=True)
print(
    marker_text(TOKEN_REJECTED_MARKER if old_token is not None else V1_MARKER),
    flush=True,
)
print(marker_text(FRESH_MARKER), flush=True)
print(marker_text(SUCCESS_MARKER), flush=True)
os.sync()
PY
  )

  case "$mmds_snapshot_result" in
    *BANGBANG_MMDS_SNAPSHOT_OK*)
      emit_line BANGBANG_MMDS_SNAPSHOT_OK
      sync
      python3 - <<'PY'
import ctypes

libc = ctypes.CDLL(None, use_errno=True)
libc.reboot.argtypes = [ctypes.c_int]
libc.reboot.restype = ctypes.c_int
if libc.reboot(0x4321FEDC) != 0:
    raise OSError(ctypes.get_errno(), "reboot(RB_POWER_OFF) failed")
PY
      emit_line BANGBANG_MMDS_SNAPSHOT_FAIL_POWEROFF
      write_vdb_marker BANGBANG_MMDS_SNAPSHOT_FAIL_POWEROFF
      ;;
    *BANGBANG_MMDS_SNAPSHOT_FAIL_*)
      emit_line "$mmds_snapshot_result"
      ;;
    *)
      emit_line BANGBANG_MMDS_SNAPSHOT_FAIL_RESULT
      write_vdb_marker BANGBANG_MMDS_SNAPSHOT_FAIL_RESULT
      ;;
  esac
}

fetch_mmds_process_a_marker() {
  if ! prepare_mmds_network BANGBANG_MMDS_PROCESS_A_FETCH_FAIL BANGBANG_MMDS_PROCESS_A_FETCH_FAIL; then
    return
  fi

  if ! request_mmds_v2_token BANGBANG_MMDS_PROCESS_A_FETCH_FAIL BANGBANG_MMDS_PROCESS_A_FETCH_FAIL 600; then
    return
  fi

  mmds_value=$(get_mmds_v2_value meta-data/bangbang-marker || true)
  if [ "$mmds_value" != BANGBANG_MMDS_PROCESS_A_VALUE ]; then
    emit_line BANGBANG_MMDS_PROCESS_A_FETCH_FAIL_RESPONSE
    write_vdb_marker BANGBANG_MMDS_PROCESS_A_FETCH_FAIL
    return
  fi
  if ! write_vdb_sector_marker "$mmds_token" 2; then
    emit_line BANGBANG_MMDS_PROCESS_A_FETCH_FAIL_TOKEN_WRITE
    write_vdb_marker BANGBANG_MMDS_PROCESS_A_FETCH_FAIL
    return
  fi
  emit_line BANGBANG_MMDS_PROCESS_A_TOKEN_READY
  write_vdb_marker BANGBANG_MMDS_PROCESS_A_TOKEN_READY

  if ! wait_for_mmds_v2_peer_token BANGBANG_MMDS_PROCESS_A_FETCH_FAIL BANGBANG_MMDS_PROCESS_A_FETCH_FAIL; then
    return
  fi
  if ! mmds_v2_peer_token_is_rejected; then
    emit_line BANGBANG_MMDS_PROCESS_A_FETCH_FAIL_PEER_ACCEPTED
    write_vdb_marker BANGBANG_MMDS_PROCESS_A_FETCH_FAIL
    return
  fi
  mmds_value=$(get_mmds_v2_value meta-data/bangbang-marker || true)
  if [ "$mmds_value" != BANGBANG_MMDS_PROCESS_A_VALUE ]; then
    emit_line BANGBANG_MMDS_PROCESS_A_FETCH_FAIL_OWN_TOKEN
    write_vdb_marker BANGBANG_MMDS_PROCESS_A_FETCH_FAIL
    return
  fi

  emit_line BANGBANG_MMDS_PROCESS_A_FETCH_OK
  write_vdb_marker BANGBANG_MMDS_PROCESS_A_FETCH_OK
}

fetch_mmds_process_b_marker() {
  if ! prepare_mmds_network BANGBANG_MMDS_PROCESS_B_READY_FAIL BANGBANG_MMDS_PROCESS_B_READY_FAIL; then
    return
  fi

  if ! request_mmds_v2_token BANGBANG_MMDS_PROCESS_B_READY_FAIL BANGBANG_MMDS_PROCESS_B_READY_FAIL 600; then
    return
  fi

  mmds_value=$(get_mmds_v2_value meta-data/bangbang-marker || true)
  if [ "$mmds_value" != BANGBANG_MMDS_PROCESS_B_VALUE ]; then
    emit_line BANGBANG_MMDS_PROCESS_B_READY_FAIL_RESPONSE
    write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY_FAIL
    return
  fi

  mmds_release=$(get_mmds_v2_value meta-data/bangbang-release || true)
  if [ "$mmds_release" != BANGBANG_MMDS_PROCESS_B_PENDING ]; then
    emit_line BANGBANG_MMDS_PROCESS_B_READY_FAIL_RELEASE
    write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY_FAIL
    return
  fi

  if ! write_vdb_sector_marker "$mmds_token" 2; then
    emit_line BANGBANG_MMDS_PROCESS_B_READY_FAIL_TOKEN_WRITE
    write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY_FAIL
    return
  fi
  emit_line BANGBANG_MMDS_PROCESS_B_TOKEN_READY
  write_vdb_marker BANGBANG_MMDS_PROCESS_B_TOKEN_READY

  if ! wait_for_mmds_v2_peer_token BANGBANG_MMDS_PROCESS_B_READY_FAIL BANGBANG_MMDS_PROCESS_B_READY_FAIL; then
    return
  fi
  if ! mmds_v2_peer_token_is_rejected; then
    emit_line BANGBANG_MMDS_PROCESS_B_READY_FAIL_PEER_ACCEPTED
    write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY_FAIL
    return
  fi
  mmds_value=$(get_mmds_v2_value meta-data/bangbang-marker || true)
  if [ "$mmds_value" != BANGBANG_MMDS_PROCESS_B_VALUE ]; then
    emit_line BANGBANG_MMDS_PROCESS_B_READY_FAIL_OWN_TOKEN
    write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY_FAIL
    return
  fi

  mmds_now=$(date +%s 2>/dev/null || true)
  case "$mmds_now" in
    ''|*[!0-9]*)
      emit_line BANGBANG_MMDS_PROCESS_B_READY_FAIL_CLOCK
      write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY_FAIL
      return
      ;;
  esac
  mmds_release_deadline=$((mmds_now + 300))

  emit_line BANGBANG_MMDS_PROCESS_B_READY
  write_vdb_marker BANGBANG_MMDS_PROCESS_B_READY

  while true; do
    mmds_now=$(date +%s 2>/dev/null || true)
    case "$mmds_now" in
      ''|*[!0-9]*)
        emit_line BANGBANG_MMDS_PROCESS_B_FETCH_FAIL_CLOCK
        write_vdb_marker_at_sector BANGBANG_MMDS_PROCESS_B_FETCH_FAIL 1
        return
        ;;
    esac
    if [ "$mmds_now" -ge "$mmds_release_deadline" ]; then
      emit_line BANGBANG_MMDS_PROCESS_B_FETCH_FAIL_TIMEOUT
      write_vdb_marker_at_sector BANGBANG_MMDS_PROCESS_B_FETCH_FAIL 1
      return
    fi

    mmds_release=$(get_mmds_v2_value meta-data/bangbang-release || true)
    case "$mmds_release" in
      BANGBANG_MMDS_PROCESS_B_PENDING|'')
        ;;
      BANGBANG_MMDS_PROCESS_B_RELEASE)
        mmds_value=$(get_mmds_v2_value meta-data/bangbang-marker || true)
        if [ "$mmds_value" = BANGBANG_MMDS_PROCESS_B_VALUE ]; then
          emit_line BANGBANG_MMDS_PROCESS_B_FETCH_OK
          write_vdb_marker_at_sector BANGBANG_MMDS_PROCESS_B_FETCH_OK 1
        else
          emit_line BANGBANG_MMDS_PROCESS_B_FETCH_FAIL_RESPONSE
          write_vdb_marker_at_sector BANGBANG_MMDS_PROCESS_B_FETCH_FAIL 1
        fi
        return
        ;;
      *)
        emit_line BANGBANG_MMDS_PROCESS_B_FETCH_FAIL_RELEASE
        write_vdb_marker_at_sector BANGBANG_MMDS_PROCESS_B_FETCH_FAIL 1
        return
        ;;
    esac
  done
}

fetch_vsock_marker() {
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VSOCK_GUEST_CONNECT_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_VSOCK_GUEST_CONNECT_FAIL
    return
  fi

  vsock_result=$(
    python3 - <<'PY' 2>/dev/null || true
import socket
import sys

HOST_CID = getattr(socket, "VMADDR_CID_HOST", 2)
PORT = 5005
TRANSFER_BYTES = 1024 * 1024
CHUNK_BYTES = 16 * 1024
GUEST_STREAM_SEED = 0x3D
HOST_STREAM_SEED = 0xA7
SOCKET_TIMEOUT = 10.0


def fail(reason):
    print(f"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_{reason}")
    sys.exit(1)


def recv_exact(stream, size):
    data = bytearray()
    while len(data) < size:
        try:
            chunk = stream.recv(size - len(data))
        except OSError:
            fail("RECV")
        if not chunk:
            fail("EOF")
        data.extend(chunk)
    return bytes(data)


def deterministic_chunk(offset, size, seed):
    return bytes(
        (
            ((position * 131 + seed) ^ (position >> 8) ^ (position >> 16))
            & 0xFF
        )
        for position in range(offset, offset + size)
    )


def send_deterministic_stream(stream, seed):
    sent = 0
    while sent < TRANSFER_BYTES:
        chunk_size = min(CHUNK_BYTES, TRANSFER_BYTES - sent)
        chunk = deterministic_chunk(sent, chunk_size, seed)
        try:
            stream.sendall(chunk)
        except OSError:
            fail("SEND")
        sent += chunk_size
    if sent != TRANSFER_BYTES:
        fail("SEND_COUNT")


def receive_and_verify_deterministic_stream(stream, seed):
    received = 0
    while received < TRANSFER_BYTES:
        chunk_size = min(CHUNK_BYTES, TRANSFER_BYTES - received)
        chunk = recv_exact(stream, chunk_size)
        if chunk != deterministic_chunk(received, chunk_size, seed):
            fail("CONTENT")
        received += chunk_size
    if received != TRANSFER_BYTES:
        fail("RECV_COUNT")


def expect_eof(stream):
    try:
        trailing = stream.recv(1)
    except OSError as error:
        fail(f"EOF_READ_{error.errno}")
    if trailing:
        fail("TRAILING_DATA")


if not hasattr(socket, "AF_VSOCK"):
    fail("NO_AF_VSOCK")

try:
    stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
except OSError:
    fail("SOCKET")

try:
    stream.settimeout(SOCKET_TIMEOUT)
    try:
        stream.connect((HOST_CID, PORT))
    except OSError:
        fail("CONNECT")

    send_deterministic_stream(stream, GUEST_STREAM_SEED)
    receive_and_verify_deterministic_stream(stream, HOST_STREAM_SEED)
    try:
        stream.shutdown(socket.SHUT_WR)
    except OSError:
        fail("SHUTDOWN_WRITE")
    expect_eof(stream)

    print("BANGBANG_VSOCK_GUEST_CONNECT_OK")
finally:
    stream.close()
PY
  )

  if [ "$vsock_result" = BANGBANG_VSOCK_GUEST_CONNECT_OK ]; then
    emit_line BANGBANG_VSOCK_GUEST_CONNECT_OK
    write_vdb_marker BANGBANG_VSOCK_GUEST_CONNECT_OK
  elif [ -n "$vsock_result" ]; then
    emit_line "$vsock_result"
    write_vdb_marker "$vsock_result"
  else
    emit_line BANGBANG_VSOCK_GUEST_CONNECT_FAIL_EMPTY
    write_vdb_marker BANGBANG_VSOCK_GUEST_CONNECT_FAIL
  fi
}

check_native_v2_root_snapshot_marker() {
  sleep 10

  expected=BANGBANG_NATIVE_V2_ROOT_SNAPSHOT_BACKING
  actual=$(dd if=/bangbang-native-v2-root-snapshot-marker bs=1 count="${#expected}" 2>/dev/null || true)
  if [ "$actual" = "$expected" ]; then
    emit_line BANGBANG_NATIVE_V2_ROOT_SNAPSHOT_RESTORED_OK
    if command -v python3 >/dev/null 2>&1; then
      python3 - <<'PY'
import ctypes

libc = ctypes.CDLL(None, use_errno=True)
libc.reboot.argtypes = [ctypes.c_int]
libc.reboot.restype = ctypes.c_int
if libc.reboot(0x4321FEDC) != 0:
    raise OSError(ctypes.get_errno(), "reboot(RB_POWER_OFF) failed")
PY
    fi
    emit_line BANGBANG_NATIVE_V2_ROOT_SNAPSHOT_POWEROFF_FAIL
  else
    emit_line BANGBANG_NATIVE_V2_ROOT_SNAPSHOT_RESTORED_FAIL
  fi
}

certify_vsock_snapshot_restore() {
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_NO_PYTHON
    return
  fi

  if ! python3 - <<'PY'; then
import socket
import sys
import time

HOST_CID = getattr(socket, "VMADDR_CID_HOST", 2)
SOURCE_GUEST_PORTS = (5011,)
FRESH_GUEST_PORTS = (5021, 5022, 5023, 5024)
GUEST_LISTEN_PORTS = tuple(range(6011, 6027))
SOURCE_HOST_STREAMS = 1
FRESH_HOST_STREAMS = 16
SOCKET_TIMEOUT = 30.0
RECONNECT_TIMEOUT = 10.0


def emit(marker):
    for offset in range(0, len(marker), 16):
        sys.stdout.write(marker[offset : offset + 16])
        sys.stdout.flush()
    sys.stdout.write("\n")
    sys.stdout.flush()


def fail(reason):
    emit(f"BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_{reason}")
    sys.exit(1)


def payload(direction, index, size):
    prefix = f"BANGBANG_VSOCK_SNAPSHOT_{direction}_{index}:".encode("ascii")
    if len(prefix) > size:
        fail("PAYLOAD_PREFIX")
    fill = bytes((ord("A") + index % 26,))
    return prefix + fill * (size - len(prefix))


def connect_host(port):
    try:
        stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    except OSError:
        fail(f"SOCKET_{port}")
    stream.settimeout(SOCKET_TIMEOUT)
    try:
        stream.connect((HOST_CID, port))
    except OSError:
        stream.close()
        fail(f"CONNECT_{port}")
    return stream


def connect_host_after_reset(port):
    deadline = time.monotonic() + RECONNECT_TIMEOUT
    while True:
        try:
            stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        except OSError:
            fail(f"SOCKET_{port}")
        stream.settimeout(1.0)
        try:
            stream.connect((HOST_CID, port))
            stream.settimeout(SOCKET_TIMEOUT)
            return stream
        except OSError:
            stream.close()
            if time.monotonic() >= deadline:
                fail(f"RECONNECT_{port}")
            time.sleep(0.05)


def recv_exact(stream, size):
    data = bytearray()
    while len(data) < size:
        try:
            chunk = stream.recv(size - len(data))
        except socket.timeout:
            fail("RECV_TIMEOUT")
        except OSError:
            fail("RECV_ERROR")
        if not chunk:
            fail("RECV_EOF")
        data.extend(chunk)
    return bytes(data)


def recv_to_eof(stream):
    data = bytearray()
    while True:
        try:
            chunk = stream.recv(65536)
        except socket.timeout:
            fail("EOF_TIMEOUT")
        except OSError:
            fail("EOF_RECV")
        if not chunk:
            return bytes(data)
        data.extend(chunk)


def expect_reset(stream, label):
    stream.settimeout(RECONNECT_TIMEOUT)
    try:
        data = stream.recv(1)
    except socket.timeout:
        fail(f"{label}_RESET_TIMEOUT")
    except OSError:
        return
    if data:
        fail(f"{label}_RESET_DATA")


if not hasattr(socket, "AF_VSOCK"):
    fail("NO_AF_VSOCK")

guest_listeners = []
for index, port in enumerate(GUEST_LISTEN_PORTS):
    try:
        listener = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((getattr(socket, "VMADDR_CID_ANY", -1), port))
        listener.listen(8)
        listener.settimeout(SOCKET_TIMEOUT)
        guest_listeners.append(listener)
    except OSError:
        fail(f"GUEST_LISTENER_{index}")

source_guest_streams = []
for index, port in enumerate(SOURCE_GUEST_PORTS):
    stream = connect_host(port)
    stream.sendall(payload("SOURCE_G2H", index, 1024))
    expected = payload("SOURCE_G2H_ACK", index, 128)
    if recv_exact(stream, len(expected)) != expected:
        fail(f"SOURCE_G2H_ACK_{index}")
    source_guest_streams.append(stream)

source_host_streams = []
for index in range(SOURCE_HOST_STREAMS):
    try:
        stream, _ = guest_listeners[index].accept()
    except OSError:
        fail(f"SOURCE_H2G_ACCEPT_{index}")
    stream.settimeout(SOCKET_TIMEOUT)
    expected = payload("SOURCE_H2G", index, 1024)
    if recv_exact(stream, len(expected)) != expected:
        fail(f"SOURCE_H2G_PAYLOAD_{index}")
    stream.sendall(payload("SOURCE_H2G_ACK", index, 128))
    source_host_streams.append(stream)

emit("BANGBANG_VSOCK_SNAPSHOT_SOURCE_READY")

for index, stream in enumerate(source_guest_streams):
    expect_reset(stream, f"SOURCE_G2H_{index}")
    stream.close()
for index, stream in enumerate(source_host_streams):
    expect_reset(stream, f"SOURCE_H2G_{index}")
    stream.close()

emit("BANGBANG_VSOCK_SNAPSHOT_RESET_OBSERVED")

for index, port in enumerate(FRESH_GUEST_PORTS):
    stream = connect_host_after_reset(port)
    stream.sendall(payload("FRESH_G2H", index, 4096))
    expected = payload("FRESH_G2H_REPLY", index, 4096)
    if recv_to_eof(stream) != expected:
        fail(f"FRESH_G2H_REPLY_{index}")
    stream.close()

emit("BANGBANG_VSOCK_SNAPSHOT_FRESH_G2H_OK")

for index in range(FRESH_HOST_STREAMS):
    try:
        stream, _ = guest_listeners[index].accept()
    except OSError:
        fail(f"FRESH_H2G_ACCEPT_{index}")
    stream.settimeout(SOCKET_TIMEOUT)
    try:
        stream.sendall(payload("FRESH_H2G_REPLY", index, 4096))
        stream.shutdown(socket.SHUT_WR)
    except OSError:
        fail(f"FRESH_H2G_REPLY_{index}")
    expected = payload("FRESH_H2G", index, 4096)
    if recv_to_eof(stream) != expected:
        fail(f"FRESH_H2G_PAYLOAD_{index}")
    stream.close()

for listener in guest_listeners:
    listener.close()
emit("BANGBANG_VSOCK_SNAPSHOT_PRESERVED_LISTENER_OK")
emit("BANGBANG_VSOCK_SNAPSHOT_RESET_OK")
PY
    emit_line BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_UNEXPECTED
  fi
}

fetch_vsock_snapshot_reset_marker() {
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_NO_PYTHON
    return
  fi

  vsock_result=$(
    python3 - <<'PY' 2>/dev/null || true
import socket
import sys
import time

HOST_CID = getattr(socket, "VMADDR_CID_HOST", 2)
OLD_PORT = 5011
FRESH_PORT = 5012
OLD_READY = b"BANGBANG_VSOCK_SNAPSHOT_OLD_READY"
FRESH_READY = b"BANGBANG_VSOCK_SNAPSHOT_FRESH_READY"
FRESH_ACK = b"BANGBANG_VSOCK_SNAPSHOT_FRESH_ACK"
SOCKET_TIMEOUT = 30.0
RECONNECT_TIMEOUT = 10.0


def fail(reason):
    print(f"BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_{reason}")
    sys.exit(1)


def connect(port):
    try:
        stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    except OSError:
        fail(f"SOCKET_{port}")
    stream.settimeout(SOCKET_TIMEOUT)
    try:
        stream.connect((HOST_CID, port))
    except OSError:
        stream.close()
        fail(f"CONNECT_{port}")
    return stream


def connect_after_reset(port):
    deadline = time.monotonic() + RECONNECT_TIMEOUT
    while True:
        try:
            stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        except OSError:
            fail(f"SOCKET_{port}")
        stream.settimeout(1.0)
        try:
            stream.connect((HOST_CID, port))
            stream.settimeout(SOCKET_TIMEOUT)
            return stream
        except OSError:
            stream.close()
            if time.monotonic() >= deadline:
                fail(f"RECONNECT_{port}")
            time.sleep(0.05)


def recv_exact(stream, size):
    data = bytearray()
    while len(data) < size:
        try:
            chunk = stream.recv(size - len(data))
        except socket.timeout:
            fail("FRESH_ACK_TIMEOUT")
        except OSError:
            fail("FRESH_ACK_RECV")
        if not chunk:
            fail("FRESH_ACK_EOF")
        data.extend(chunk)
    return bytes(data)


if not hasattr(socket, "AF_VSOCK"):
    fail("NO_AF_VSOCK")

old_stream = connect(OLD_PORT)
try:
    try:
        old_stream.sendall(OLD_READY)
    except OSError:
        fail("OLD_READY_SEND")

    try:
        old_data = old_stream.recv(1)
    except socket.timeout:
        fail("OLD_RESET_TIMEOUT")
    except OSError:
        pass
    else:
        if old_data:
            fail("OLD_UNEXPECTED_DATA")
finally:
    old_stream.close()

fresh_stream = connect_after_reset(FRESH_PORT)
try:
    try:
        fresh_stream.sendall(FRESH_READY)
    except OSError:
        fail("FRESH_READY_SEND")
    if recv_exact(fresh_stream, len(FRESH_ACK)) != FRESH_ACK:
        fail("FRESH_ACK_CONTENT")
finally:
    fresh_stream.close()

print("BANGBANG_VSOCK_SNAPSHOT_RESET_OK")
PY
  )

  case "$vsock_result" in
    BANGBANG_VSOCK_SNAPSHOT_RESET_OK)
      emit_line BANGBANG_VSOCK_SNAPSHOT_RESET_OK
      write_vdb_marker BANGBANG_VSOCK_SNAPSHOT_RESET_OK
      ;;
    BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_*)
      emit_line "$vsock_result"
      write_vdb_marker "$vsock_result"
      ;;
    *)
      emit_line BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_EMPTY
      write_vdb_marker BANGBANG_VSOCK_SNAPSHOT_RESET_FAIL_EMPTY
      ;;
  esac
}

fetch_multi_vsock_marker() {
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VSOCK_GUEST_MULTISTREAM_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_VSOCK_GUEST_MULTISTREAM_FAIL
    return
  fi

  vsock_result=$(
    python3 - <<'PY' 2>/dev/null || true
import socket
import sys

HOST_CID = getattr(socket, "VMADDR_CID_HOST", 2)
STREAMS = (
    (
        5007,
        b"BANGBANG_VSOCK_GUEST_MULTI_ONE",
        b"BANGBANG_VSOCK_HOST_MULTI_ONE",
    ),
    (
        5008,
        b"BANGBANG_VSOCK_GUEST_MULTI_TWO",
        b"BANGBANG_VSOCK_HOST_MULTI_TWO",
    ),
)


def fail(reason):
    print(f"BANGBANG_VSOCK_GUEST_MULTISTREAM_FAIL_{reason}")
    sys.exit(1)


def recv_exact(stream, size):
    data = b""
    while len(data) < size:
        try:
            chunk = stream.recv(size - len(data))
        except OSError:
            fail("RECV")
        if not chunk:
            fail("EOF")
        data += chunk
    return data


if not hasattr(socket, "AF_VSOCK"):
    fail("NO_AF_VSOCK")

streams = []
try:
    for port, guest_payload, host_reply in STREAMS:
        try:
            stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        except OSError:
            fail(f"SOCKET_{port}")

        try:
            stream.settimeout(5.0)
            stream.connect((HOST_CID, port))
        except OSError:
            stream.close()
            fail(f"CONNECT_{port}")

        streams.append((port, stream, guest_payload, host_reply))

    for index, (_port, stream, guest_payload, _host_reply) in enumerate(streams, start=1):
        try:
            stream.sendall(guest_payload)
        except OSError:
            fail(f"SEND_{index}")

    for index, (_port, stream, _guest_payload, host_reply) in enumerate(streams, start=1):
        reply = recv_exact(stream, len(host_reply))
        if reply != host_reply:
            fail(f"REPLY_{index}")

    print("BANGBANG_VSOCK_GUEST_MULTISTREAM_OK")
finally:
    for _port, stream, _guest_payload, _host_reply in streams:
        stream.close()
PY
  )

  if [ "$vsock_result" = BANGBANG_VSOCK_GUEST_MULTISTREAM_OK ]; then
    emit_line BANGBANG_VSOCK_GUEST_MULTISTREAM_OK
    write_vdb_marker BANGBANG_VSOCK_GUEST_MULTISTREAM_OK
  elif [ -n "$vsock_result" ]; then
    emit_line "$vsock_result"
    write_vdb_marker BANGBANG_VSOCK_GUEST_MULTISTREAM_FAIL
  else
    emit_line BANGBANG_VSOCK_GUEST_MULTISTREAM_FAIL_EMPTY
    write_vdb_marker BANGBANG_VSOCK_GUEST_MULTISTREAM_FAIL
  fi
}

fetch_host_vsock_marker() {
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VSOCK_HOST_CONNECT_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_VSOCK_HOST_CONNECT_FAIL
    return
  fi

  python3 - <<'PY' 2>/dev/null || true
import socket
import sys

CID_ANY = getattr(socket, "VMADDR_CID_ANY", -1)
PORT = 5006
TRANSFER_BYTES = 1024 * 1024
CHUNK_BYTES = 16 * 1024
GUEST_STREAM_SEED = 0x3D
HOST_STREAM_SEED = 0xA7
READY_MARKER = b"BANGBANG_VSOCK_HOST_CONNECT_READY"
SUCCESS_MARKER = b"BANGBANG_VSOCK_HOST_CONNECT_OK"
FAIL_MARKER = b"BANGBANG_VSOCK_HOST_CONNECT_FAIL"
SOCKET_TIMEOUT = 10.0


def marker_text(marker):
    return marker.decode("ascii")


def write_marker(marker):
    try:
        with open("/dev/vdb", "wb", buffering=0) as drive:
            drive.write(marker.ljust(512, b" "))
    except OSError:
        pass


def fail(reason):
    marker = FAIL_MARKER + b"_" + reason.encode("ascii")
    write_marker(marker)
    print(marker_text(marker))
    sys.exit(1)


def recv_exact(stream, size):
    data = bytearray()
    while len(data) < size:
        try:
            chunk = stream.recv(size - len(data))
        except OSError:
            fail("RECV")
        if not chunk:
            fail("EOF")
        data.extend(chunk)
    return bytes(data)


def deterministic_chunk(offset, size, seed):
    return bytes(
        (
            ((position * 131 + seed) ^ (position >> 8) ^ (position >> 16))
            & 0xFF
        )
        for position in range(offset, offset + size)
    )


def send_deterministic_stream(stream, seed):
    sent = 0
    while sent < TRANSFER_BYTES:
        chunk_size = min(CHUNK_BYTES, TRANSFER_BYTES - sent)
        chunk = deterministic_chunk(sent, chunk_size, seed)
        try:
            stream.sendall(chunk)
        except OSError:
            fail("SEND")
        sent += chunk_size
    if sent != TRANSFER_BYTES:
        fail("SEND_COUNT")


def receive_and_verify_deterministic_stream(stream, seed):
    received = 0
    while received < TRANSFER_BYTES:
        chunk_size = min(CHUNK_BYTES, TRANSFER_BYTES - received)
        chunk = recv_exact(stream, chunk_size)
        if chunk != deterministic_chunk(received, chunk_size, seed):
            fail("CONTENT")
        received += chunk_size
    if received != TRANSFER_BYTES:
        fail("RECV_COUNT")


def expect_eof(stream):
    try:
        trailing = stream.recv(1)
    except OSError as error:
        fail(f"EOF_READ_{error.errno}")
    if trailing:
        fail("TRAILING_DATA")


if not hasattr(socket, "AF_VSOCK"):
    fail("NO_AF_VSOCK")

try:
    server = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
except OSError:
    fail("SOCKET")

try:
    server.settimeout(SOCKET_TIMEOUT)
    try:
        server.bind((CID_ANY, PORT))
    except OSError:
        fail("BIND")

    try:
        server.listen(1)
    except OSError:
        fail("LISTEN")

    write_marker(READY_MARKER)
    print(marker_text(READY_MARKER))

    try:
        connection, _addr = server.accept()
    except OSError:
        fail("ACCEPT")

    try:
        connection.settimeout(SOCKET_TIMEOUT)
        send_deterministic_stream(connection, GUEST_STREAM_SEED)
        try:
            connection.shutdown(socket.SHUT_WR)
        except OSError:
            fail("SHUTDOWN_WRITE")
        receive_and_verify_deterministic_stream(connection, HOST_STREAM_SEED)
        expect_eof(connection)
    finally:
        connection.close()

    write_marker(SUCCESS_MARKER)
    print(marker_text(SUCCESS_MARKER))
finally:
    server.close()
PY
}

fetch_multi_host_vsock_marker() {
  if ! command -v python3 >/dev/null 2>&1; then
    emit_line BANGBANG_VSOCK_HOST_MULTISTREAM_FAIL_NO_PYTHON
    write_vdb_marker BANGBANG_VSOCK_HOST_MULTISTREAM_FAIL
    return
  fi

  python3 - <<'PY' 2>/dev/null || true
import socket
import sys

CID_ANY = getattr(socket, "VMADDR_CID_ANY", -1)
STREAMS = (
    (
        5009,
        b"BANGBANG_VSOCK_HOST_MULTI_GUEST_ONE",
        b"BANGBANG_VSOCK_HOST_MULTI_HOST_ONE",
    ),
    (
        5010,
        b"BANGBANG_VSOCK_HOST_MULTI_GUEST_TWO",
        b"BANGBANG_VSOCK_HOST_MULTI_HOST_TWO",
    ),
)
READY_MARKER = b"BANGBANG_VSOCK_HOST_MULTISTREAM_READY"
SUCCESS_MARKER = b"BANGBANG_VSOCK_HOST_MULTISTREAM_OK"
FAIL_MARKER = b"BANGBANG_VSOCK_HOST_MULTISTREAM_FAIL"
SOCKET_TIMEOUT = 10.0


def marker_text(marker):
    return marker.decode("ascii")


def write_marker(marker):
    try:
        with open("/dev/vdb", "wb", buffering=0) as drive:
            drive.write(marker.ljust(512, b" "))
    except OSError:
        pass


def fail(reason):
    marker = FAIL_MARKER + b"_" + reason.encode("ascii")
    write_marker(marker)
    print(marker_text(marker))
    sys.exit(1)


def recv_exact(stream, size):
    data = b""
    while len(data) < size:
        try:
            chunk = stream.recv(size - len(data))
        except OSError:
            fail("RECV")
        if not chunk:
            fail("EOF")
        data += chunk
    return data


if not hasattr(socket, "AF_VSOCK"):
    fail("NO_AF_VSOCK")

listeners = []
connections = []
try:
    for port, guest_payload, host_payload in STREAMS:
        try:
            server = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        except OSError:
            fail(f"SOCKET_{port}")

        try:
            server.settimeout(SOCKET_TIMEOUT)
            server.bind((CID_ANY, port))
            server.listen(1)
        except OSError:
            server.close()
            fail(f"LISTEN_{port}")

        listeners.append((port, server, guest_payload, host_payload))

    write_marker(READY_MARKER)
    print(marker_text(READY_MARKER))

    for port, server, guest_payload, host_payload in listeners:
        try:
            connection, _addr = server.accept()
        except OSError:
            fail(f"ACCEPT_{port}")

        try:
            connection.settimeout(SOCKET_TIMEOUT)
        except OSError:
            connection.close()
            fail(f"TIMEOUT_{port}")

        connections.append((port, connection, guest_payload, host_payload))

    for index, (_port, connection, guest_payload, _host_payload) in enumerate(
        connections, start=1
    ):
        try:
            connection.sendall(guest_payload)
        except OSError:
            fail(f"SEND_{index}")

    for index, (_port, connection, _guest_payload, host_payload) in enumerate(
        connections, start=1
    ):
        payload = recv_exact(connection, len(host_payload))
        if payload != host_payload:
            fail(f"PAYLOAD_{index}")

    write_marker(SUCCESS_MARKER)
    print(marker_text(SUCCESS_MARKER))
finally:
    for _port, connection, _guest_payload, _host_payload in connections:
        connection.close()
    for _port, server, _guest_payload, _host_payload in listeners:
        server.close()
PY
}

pvtime_steal_ticks() {
  awk 'NR == 1 && $1 == "cpu" { print $9; found = 1; exit }
       END { if (!found) exit 1 }' /proc/stat 2>/dev/null
}

pvtime_fail() {
  emit_line "BANGBANG_PVTIME_FAIL_$1"
}

check_pvtime_marker() {
  discovery=$(dmesg 2>/dev/null | grep -m 1 'stolen time PV' || true)
  if [ -z "$discovery" ]; then
    pvtime_fail DISCOVERY
    return
  fi
  emit_line "$discovery"
  emit_line BANGBANG_PVTIME_DISCOVERY_OK

  before=$(pvtime_steal_ticks || true)
  case "$before" in
    ''|*[!0-9]*)
      pvtime_fail BEFORE
      return
      ;;
  esac
  emit_line "BANGBANG_PVTIME_BEFORE=$before"

  work=0
  while [ "$work" -lt 200000 ]; do
    work=$((work + 1))
  done
  emit_line BANGBANG_PVTIME_CONTENTION_WORK_0123456789_0123456789_0123456789_0123456789

  after=$(pvtime_steal_ticks || true)
  case "$after" in
    ''|*[!0-9]*)
      pvtime_fail AFTER
      return
      ;;
  esac
  emit_line "BANGBANG_PVTIME_AFTER=$after"
  if [ "$after" -le "$before" ]; then
    pvtime_fail CONTENTION
    return
  fi
  emit_line BANGBANG_PVTIME_CONTENTION_OK

  idle_before=$(pvtime_steal_ticks || true)
  sleep 1
  idle_after=$(pvtime_steal_ticks || true)
  case "$idle_before" in
    ''|*[!0-9]*)
      pvtime_fail IDLE_SAMPLE
      return
      ;;
  esac
  case "$idle_after" in
    ''|*[!0-9]*)
      pvtime_fail IDLE_SAMPLE
      return
      ;;
  esac
  emit_line "BANGBANG_PVTIME_IDLE_BEFORE=$idle_before"
  emit_line "BANGBANG_PVTIME_IDLE_AFTER=$idle_after"
  if [ "$idle_after" -ne "$idle_before" ]; then
    pvtime_fail IDLE_CHANGED
    return
  fi
  emit_line BANGBANG_PVTIME_IDLE_OK
}

remaining_device_fail() {
  reason=$1
  marker="BANGBANG_REMAINING_DEVICE_FAIL_$reason"
  emit_line "$marker"
  write_vdb_sector_marker "$marker" 7 2>/dev/null || true
}

remaining_device_pci_identity_count() {
  expected_device=$1
  count=0
  for function_path in /sys/bus/pci/devices/*; do
    [ -d "$function_path" ] || continue
    function=${function_path##*/}
    if pci_function_has_identity "$function" "$expected_device"; then
      count=$((count + 1))
    fi
  done
  printf '%s\n' "$count"
}

check_remaining_device_transport() {
  if cmdline_has bangbang.expect-remaining-device-transport=mmio; then
    if ! cmdline_has pci=off; then
      remaining_device_fail MMIO_PCI_ENABLED
      return 1
    fi
    mmio_count=$(find /sys/firmware/devicetree/base -maxdepth 1 -name 'virtio_mmio@*' 2>/dev/null \
      | wc -l 2>/dev/null || true)
    mmio_count=${mmio_count##* }
    case "$mmio_count" in
      '' | *[!0-9]*)
        remaining_device_fail MMIO_COUNT
        return 1
        ;;
    esac
    if [ "$mmio_count" -lt 5 ]; then
      remaining_device_fail MMIO_IDENTITIES
      return 1
    fi
    transport=MMIO
  elif cmdline_has bangbang.expect-remaining-device-transport=pci; then
    if cmdline_has pci=off; then
      remaining_device_fail PCI_DISABLED
      return 1
    fi
    if find /sys/firmware/devicetree/base -maxdepth 1 -name 'virtio_mmio@*' 2>/dev/null \
      | grep -q .; then
      remaining_device_fail PCI_LEGACY_MMIO
      return 1
    fi
    if [ "$(remaining_device_pci_identity_count 0x1045)" != 1 ] \
      || [ "$(remaining_device_pci_identity_count 0x1042)" != 2 ] \
      || [ "$(remaining_device_pci_identity_count 0x1044)" != 1 ] \
      || [ "$(remaining_device_pci_identity_count 0x1058)" != 1 ]; then
      remaining_device_fail PCI_IDENTITIES
      return 1
    fi
    transport=PCI
  else
    remaining_device_fail TRANSPORT_ARGUMENT
    return 1
  fi

  marker="BANGBANG_REMAINING_DEVICE_TRANSPORT_${transport}_OK"
  emit_line "$marker"
  write_vdb_sector_marker "$marker" 2 || {
    remaining_device_fail TRANSPORT_MARKER
    return 1
  }
}

check_remaining_device_pvtime() {
  discovery=$(dmesg 2>/dev/null | grep -m 1 'stolen time PV' || true)
  if [ -z "$discovery" ]; then
    remaining_device_fail PVTIME_DISCOVERY
    return 1
  fi
  steal_ticks=$(pvtime_steal_ticks || true)
  case "$steal_ticks" in
    '' | *[!0-9]*)
      remaining_device_fail PVTIME_STEAL
      return 1
      ;;
  esac
  emit_line "$discovery"
  emit_line "BANGBANG_REMAINING_DEVICE_PVTIME_STEAL=$steal_ticks"
  emit_line BANGBANG_REMAINING_DEVICE_PVTIME_OK
}

check_remaining_device_certification() {
  if ! check_remaining_device_transport; then
    return
  fi

  emit_line BANGBANG_REMAINING_DEVICE_MEMORY_BEGIN
  check_memory_hotplug_marker
  if ! vdb_starts_with_marker BANGBANG_MEMORY_HOTPLUG_GUEST_CHECK_OK; then
    remaining_device_fail MEMORY_HOTPLUG
    return
  fi
  emit_line BANGBANG_REMAINING_DEVICE_MEMORY_OK

  check_rtc_marker
  if ! vdb_starts_with_marker BANGBANG_RTC_GUEST_CHECK_OK; then
    remaining_device_fail RTC
    return
  fi
  check_vmgenid_marker
  if ! vdb_starts_with_marker BANGBANG_VMGENID_GUEST_CHECK_OK; then
    remaining_device_fail VMGENID
    return
  fi
  check_vmclock_marker
  if ! vdb_starts_with_marker BANGBANG_VMCLOCK_GUEST_CHECK_OK; then
    remaining_device_fail VMCLOCK
    return
  fi
  if ! check_remaining_device_pvtime; then
    return
  fi
  if ! write_vdb_sector_marker BANGBANG_REMAINING_DEVICE_TIME_IDENTITY_OK 3; then
    remaining_device_fail TIME_IDENTITY_MARKER
    return
  fi
  emit_line BANGBANG_REMAINING_DEVICE_TIME_IDENTITY_OK

  read_entropy_lifecycle_marker
  if ! vdb_starts_with_marker BANGBANG_ENTROPY_LIFECYCLE_OK; then
    remaining_device_fail ENTROPY
    return
  fi
  emit_line BANGBANG_REMAINING_DEVICE_ENTROPY_OK

  check_balloon_marker
  if ! vdb_starts_with_marker BANGBANG_BALLOON_REPORTING_GUEST_CHECK_OK; then
    remaining_device_fail BALLOON
    return
  fi
  emit_line BANGBANG_REMAINING_DEVICE_BALLOON_OK

  expected_serial_input=BANGBANG_REMAINING_DEVICE_SERIAL_ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz_0123456789_END
  if ! stty -F /dev/ttyS0 raw -echo 2>/dev/null; then
    remaining_device_fail SERIAL_MODE
    return
  fi
  emit_line BANGBANG_REMAINING_DEVICE_SERIAL_READY
  if ! write_vdb_sector_marker BANGBANG_REMAINING_DEVICE_SERIAL_READY 4; then
    remaining_device_fail SERIAL_READY_MARKER
    return
  fi
  serial_input=$(timeout 90 dd if=/dev/ttyS0 bs=1 count="${#expected_serial_input}" 2>/dev/null || true)
  if [ -z "$serial_input" ]; then
    remaining_device_fail SERIAL_EOF
    return
  fi
  if [ "$serial_input" != "$expected_serial_input" ]; then
    remaining_device_fail SERIAL_INPUT
    return
  fi
  emit_line BANGBANG_REMAINING_DEVICE_SERIAL_OK
  if ! write_vdb_sector_marker BANGBANG_REMAINING_DEVICE_SERIAL_OK 4; then
    remaining_device_fail SERIAL_SUCCESS_MARKER
    return
  fi

  emit_line BANGBANG_REMAINING_DEVICE_CERTIFICATION_OK
  if ! write_vdb_sector_marker BANGBANG_REMAINING_DEVICE_CERTIFICATION_OK 5; then
    remaining_device_fail FINAL_MARKER
  fi
}

cmdline=
emit_line BANGBANG_DIRECT_ROOTFS_BOOT_BEGIN
if [ -r /etc/os-release ]; then
  id_line=$(grep -m 1 '^ID=' /etc/os-release 2>/dev/null || true)
  codename_line=$(grep -m 1 '^VERSION_CODENAME=' /etc/os-release 2>/dev/null || true)
  if [ -n "$id_line" ]; then
    emit_line "$id_line"
  fi
  if [ -n "$codename_line" ]; then
    emit_line "$codename_line"
  fi
fi
mount_if_directory proc proc /proc
mount_if_directory devtmpfs devtmpfs /dev
mount_if_directory sysfs sysfs /sys
if [ -r /proc/cmdline ]; then
  emit_line BANGBANG_CMDLINE_BEGIN
  cmdline=$(cat /proc/cmdline 2>/dev/null || true)
  emit_line "$cmdline"
  emit_line BANGBANG_CMDLINE_END
fi
if cmdline_has bangbang.block-serial=vda; then
  report_block_serial vda
elif cmdline_has bangbang.block-serial=vdb; then
  report_block_serial vdb
fi
if cmdline_has bangbang.remaining-device-certification=1; then
  check_remaining_device_certification
elif cmdline_has bangbang.pvtime-check=1; then
  check_pvtime_marker
elif cmdline_has bangbang.storage-certification=1; then
  check_storage_certification
elif cmdline_has bangbang.pmem-root=ro; then
  check_pmem_root_marker ro
elif cmdline_has bangbang.pmem-root=rw; then
  check_pmem_root_marker rw
elif cmdline_has bangbang.vhost-user-block=ro; then
  check_vhost_user_block_marker ro
elif cmdline_has bangbang.vhost-user-block=rw; then
  check_vhost_user_block_marker rw
elif cmdline_has bangbang.network-hotplug=1; then
  check_network_hotplug_marker
elif cmdline_has bangbang.pmem-hotplug=1; then
  check_pmem_hotplug_marker
elif cmdline_has bangbang.block-hotplug=1; then
  check_block_hotplug_marker
elif cmdline_has bangbang.block-backing-lifecycle=three; then
  check_block_backing_lifecycle three
elif cmdline_has bangbang.block-backing-lifecycle=two; then
  check_block_backing_lifecycle two
elif cmdline_has bangbang.pci-all-virtio=1; then
  check_all_virtio_pci_marker
elif cmdline_has bangbang.cpu-template-report=1; then
  report_cpu_template_ids
elif cmdline_has bangbang.cache-fdt-check=1; then
  check_cache_fdt_marker
elif cmdline_has bangbang.entropy-read=1; then
  read_entropy_marker
elif cmdline_has bangbang.entropy-lifecycle=1; then
  read_entropy_lifecycle_marker
elif cmdline_has bangbang.balloon-check=1; then
  check_balloon_marker
elif cmdline_has bangbang.memory-hotplug-snapshot=1; then
  check_memory_hotplug_snapshot_marker
elif cmdline_has bangbang.memory-hotplug-check=1; then
  check_memory_hotplug_marker
elif cmdline_has bangbang.rtc-check=1; then
  check_rtc_marker
elif cmdline_has bangbang.vmgenid-check=1; then
  check_vmgenid_marker
elif cmdline_has bangbang.vmclock-check=1; then
  check_vmclock_marker
elif cmdline_has bangbang.block-writeback-flush=1; then
  flush_writeback_block_marker
elif cmdline_has bangbang.pmem-read-flush=1; then
  read_flush_pmem_marker
elif cmdline_has bangbang.mmds-multi-fetch=1; then
  fetch_multi_interface_mmds_markers
elif cmdline_has bangbang.mmds-process-a-fetch=1; then
  fetch_mmds_process_a_marker
elif cmdline_has bangbang.mmds-process-b-fetch=1; then
  fetch_mmds_process_b_marker
elif cmdline_has bangbang.mmds-v2-fetch=1; then
  fetch_mmds_v2_marker
elif cmdline_has bangbang.mmds-snapshot=1; then
  certify_mmds_snapshot_restore
elif cmdline_has bangbang.virtio-net-semantics=1; then
  prove_virtio_network_semantics
elif cmdline_has bangbang.mmds-fetch=1; then
  fetch_mmds_marker
elif cmdline_has bangbang.native-v2-root-snapshot=1; then
  check_native_v2_root_snapshot_marker
elif cmdline_has bangbang.vsock-snapshot-certify=1; then
  certify_vsock_snapshot_restore
elif cmdline_has bangbang.vsock-snapshot-reset=1; then
  fetch_vsock_snapshot_reset_marker
elif cmdline_has bangbang.vsock-guest-connect=1; then
  fetch_vsock_marker
elif cmdline_has bangbang.vsock-guest-multistream=1; then
  fetch_multi_vsock_marker
elif cmdline_has bangbang.vsock-host-connect=1; then
  fetch_host_vsock_marker
elif cmdline_has bangbang.vsock-host-multistream=1; then
  fetch_multi_host_vsock_marker
else
  write_vdb_marker BANGBANG_DIRECT_ROOTFS_BLOCK_OK
fi
emit_line BANGBANG_DIRECT_ROOTFS_BOOT_OK
exec sleep 3600
EOF
  chmod 0755 "$init_path"
  printf '%s' BANGBANG_NATIVE_V2_ROOT_SNAPSHOT_BACKING \
    > "${extract_dir}/bangbang-native-v2-root-snapshot-marker"
  truncate -s 4096 "${extract_dir}/bangbang-native-v2-root-snapshot-marker"
  chmod 0444 "${extract_dir}/bangbang-native-v2-root-snapshot-marker"
}

if [[ -n "$internal_populate_dir" ]]; then
  case "$internal_populate_dir" in
    /*)
      ;;
    *)
      echo "internal rootfs population directory must be absolute" >&2
      exit 2
      ;;
  esac
  if [[ -L "$internal_populate_dir" || ! -d "$internal_populate_dir" ]]; then
    echo "internal rootfs population directory must be a non-symlink directory" >&2
    exit 1
  fi
  extract_dir="$internal_populate_dir"
  install_arm64_id_register_report_helper
  install_direct_boot_init
  extract_dir=""
  exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to prepare guest artifacts" >&2
  exit 1
fi

case "$format" in
  squashfs)
    exec python3 "$repo_root/scripts/guest_artifact_policy.py" fetch rootfs
    ;;
  ext4)
    if [[ "$direct_boot_init" == true ]]; then
      variant="$direct_boot_variant"
    else
      variant="normal"
    fi
    exec python3 "$repo_root/scripts/guest_artifact_policy.py" \
      prepare-ext4 \
      --size "$ext4_size" \
      --variant "$variant"
    ;;
esac
