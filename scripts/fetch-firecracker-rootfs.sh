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
EOF
}

format="squashfs"
ext4_size="1G"
ext4_size_set=false
direct_boot_init=false

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

firecracker_minor="v1.15"
rootfs_arch="aarch64"
rootfs_name="ubuntu-24.04"
rootfs_sha256="0efb6a3ff2982baa6ca7e3d940966516ba7ddd2df5deb3e6c2161d369a15d608"
rootfs_url="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/${firecracker_minor}/${rootfs_arch}/${rootfs_name}.squashfs"
direct_boot_variant="direct-boot-v21"

cache_root="${BANGBANG_GUEST_ARTIFACTS_DIR:-$repo_root/.tmp/guest-artifacts}"
upstream_dir="${cache_root}/firecracker-ci/${firecracker_minor}/${rootfs_arch}"
upstream_path="${upstream_dir}/${rootfs_name}.squashfs"
prepared_dir="${cache_root}/bangbang/rootfs"
if [[ "$direct_boot_init" == true ]]; then
  ext4_path="${prepared_dir}/${rootfs_name}-${ext4_size}-${direct_boot_variant}.ext4"
else
  ext4_path="${prepared_dir}/${rootfs_name}-${ext4_size}.ext4"
fi
tmp_file=""
tmp_ext4=""
extract_dir=""
mkfs_ext4=""

cleanup() {
  if [[ -n "$tmp_file" && -e "$tmp_file" ]]; then
    rm -f "$tmp_file"
  fi
  if [[ -n "$tmp_ext4" && -e "$tmp_ext4" ]]; then
    rm -f "$tmp_ext4"
  fi
  if [[ -n "$extract_dir" && -e "$extract_dir" ]]; then
    rm -rf "$extract_dir"
  fi
}
trap cleanup EXIT

hash_file() {
  local path="$1"
  local output

  if command -v shasum >/dev/null 2>&1; then
    output="$(shasum -a 256 "$path")"
    printf '%s\n' "${output%% *}"
    return
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    output="$(sha256sum "$path")"
    printf '%s\n' "${output%% *}"
    return
  fi

  echo "shasum or sha256sum is required to verify guest artifacts" >&2
  exit 1
}

verify_sha256() {
  local path="$1"
  local actual

  actual="$(hash_file "$path")"
  [[ "$actual" == "$rootfs_sha256" ]]
}

fetch_squashfs() {
  if [[ -L "$upstream_path" ]]; then
    echo "cached Firecracker rootfs artifact path must not be a symlink: $upstream_path" >&2
    exit 1
  fi

  if [[ -e "$upstream_path" && ! -f "$upstream_path" ]]; then
    echo "cached Firecracker rootfs artifact path exists but is not a regular file: $upstream_path" >&2
    exit 1
  fi

  if [[ -f "$upstream_path" ]]; then
    if verify_sha256 "$upstream_path"; then
      echo "using cached Firecracker rootfs artifact: $upstream_path" >&2
      return
    fi

    echo "cached Firecracker rootfs artifact failed SHA-256 verification; redownloading" >&2
  fi

  if ! command -v curl >/dev/null 2>&1; then
    echo "curl is required to fetch guest artifacts" >&2
    exit 1
  fi

  mkdir -p "$upstream_dir"
  tmp_file="$(mktemp "${upstream_path}.download.XXXXXX")"

  echo "fetching Firecracker rootfs artifact: $rootfs_url" >&2
  curl \
    --fail \
    --location \
    --show-error \
    --silent \
    --retry 3 \
    --connect-timeout 10 \
    --output "$tmp_file" \
    "$rootfs_url"

  if ! verify_sha256 "$tmp_file"; then
    echo "downloaded Firecracker rootfs artifact failed SHA-256 verification" >&2
    exit 1
  fi

  chmod 0644 "$tmp_file"
  mv "$tmp_file" "$upstream_path"
  tmp_file=""
}

find_mkfs_ext4() {
  local candidate
  local prefix

  if [[ -n "${BANGBANG_MKFS_EXT4:-}" ]]; then
    if [[ -f "$BANGBANG_MKFS_EXT4" && -x "$BANGBANG_MKFS_EXT4" ]]; then
      printf '%s\n' "$BANGBANG_MKFS_EXT4"
      return
    fi

    echo "BANGBANG_MKFS_EXT4 does not point to a regular executable file: $BANGBANG_MKFS_EXT4" >&2
    exit 1
  fi

  if command -v mkfs.ext4 >/dev/null 2>&1; then
    command -v mkfs.ext4
    return
  fi

  if command -v brew >/dev/null 2>&1; then
    prefix="$(brew --prefix e2fsprogs 2>/dev/null || true)"
    if [[ -n "$prefix" ]]; then
      candidate="${prefix}/sbin/mkfs.ext4"
      if [[ -f "$candidate" && -x "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return
      fi
    fi
  fi

  echo "mkfs.ext4 is required to prepare an ext4 rootfs; install e2fsprogs" >&2
  exit 1
}

check_ext4_output_path() {
  if [[ -L "$ext4_path" ]]; then
    echo "prepared ext4 rootfs path must not be a symlink: $ext4_path" >&2
    exit 1
  fi

  if [[ -e "$ext4_path" && ! -f "$ext4_path" ]]; then
    echo "prepared ext4 rootfs path exists but is not a regular file: $ext4_path" >&2
    exit 1
  fi
}

ensure_ext4_tools() {
  if ! command -v unsquashfs >/dev/null 2>&1; then
    echo "unsquashfs is required to prepare an ext4 rootfs; install squashfs" >&2
    exit 1
  fi

  if [[ -z "$mkfs_ext4" ]]; then
    mkfs_ext4="$(find_mkfs_ext4)"
  fi
}

preflight_ext4_preparation() {
  check_ext4_output_path

  if [[ -f "$ext4_path" ]]; then
    return
  fi

  ensure_ext4_tools
}

prepare_ext4() {
  check_ext4_output_path

  if [[ -f "$ext4_path" ]]; then
    echo "using prepared ext4 rootfs artifact: $ext4_path" >&2
    return
  fi

  ensure_ext4_tools

  mkdir -p "$prepared_dir"
  extract_dir="$(mktemp -d "${prepared_dir}/${rootfs_name}.extract.XXXXXX")"
  tmp_ext4="$(mktemp "${ext4_path}.build.XXXXXX")"

  echo "extracting Firecracker rootfs artifact: $upstream_path" >&2
  unsquashfs -q -no-progress -no-xattrs -d "$extract_dir" "$upstream_path"
  if [[ "$direct_boot_init" == true ]]; then
    install_direct_boot_init
  fi

  echo "preparing ext4 rootfs artifact: $ext4_path" >&2
  truncate -s "$ext4_size" "$tmp_ext4"
  "$mkfs_ext4" -q -d "$extract_dir" -F "$tmp_ext4"
  chmod 0644 "$tmp_ext4"
  mv "$tmp_ext4" "$ext4_path"
  tmp_ext4=""
  rm -rf "$extract_dir"
  extract_dir=""
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

write_vdb_marker() {
  marker=$1
  if [ -b /dev/vdb ]; then
    printf '%-512s' "$marker" >/dev/vdb 2>/dev/null || true
  fi
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
    write_vdb_marker BANGBANG_MMDS_GUEST_FETCH_OK
  else
    emit_line BANGBANG_MMDS_FETCH_FAIL_RESPONSE
    write_vdb_marker BANGBANG_MMDS_FETCH_FAIL
  fi
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

  emit_line BANGBANG_BALLOON_GUEST_CHECK_OK
  write_vdb_marker BANGBANG_BALLOON_GUEST_CHECK_OK
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

fetch_mmds_v2_marker() {
  if ! prepare_mmds_network BANGBANG_MMDS_V2_FETCH_FAIL BANGBANG_MMDS_V2_FETCH_FAIL; then
    return
  fi

  mmds_token=$(
    curl \
      --fail \
      --silent \
      --show-error \
      --connect-timeout 2 \
      --max-time 5 \
      -X PUT \
      -H 'X-metadata-token-ttl-seconds: 60' \
      http://169.254.169.254/latest/api/token \
      2>/dev/null || true
  )

  if [ "${#mmds_token}" -ne 64 ]; then
    emit_line BANGBANG_MMDS_V2_FETCH_FAIL_TOKEN
    write_vdb_marker BANGBANG_MMDS_V2_FETCH_FAIL
    return
  fi
  case "$mmds_token" in
    *[!0123456789abcdef]*)
      emit_line BANGBANG_MMDS_V2_FETCH_FAIL_TOKEN
      write_vdb_marker BANGBANG_MMDS_V2_FETCH_FAIL
      return
      ;;
  esac

  mmds_value=$(
    curl \
      --fail \
      --silent \
      --show-error \
      --connect-timeout 2 \
      --max-time 5 \
      -H "X-metadata-token: $mmds_token" \
      http://169.254.169.254/meta-data/bangbang-marker \
      2>/dev/null || true
  )

  if [ "$mmds_value" = BANGBANG_MMDS_GUEST_VALUE ]; then
    emit_line BANGBANG_MMDS_V2_FETCH_OK
    write_vdb_marker BANGBANG_MMDS_V2_GUEST_FETCH_OK
  else
    emit_line BANGBANG_MMDS_V2_FETCH_FAIL_RESPONSE
    write_vdb_marker BANGBANG_MMDS_V2_FETCH_FAIL
  fi
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
PAYLOAD_PAIRS = (
    (b"BANGBANG_VSOCK_GUEST_STREAM_ONE", b"BANGBANG_VSOCK_HOST_STREAM_ONE"),
    (b"BANGBANG_VSOCK_GUEST_STREAM_TWO", b"BANGBANG_VSOCK_HOST_STREAM_TWO"),
)


def fail(reason):
    print(f"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_{reason}")
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

try:
    stream = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
except OSError:
    fail("SOCKET")

try:
    stream.settimeout(5.0)
    try:
        stream.connect((HOST_CID, PORT))
    except OSError:
        fail("CONNECT")

    for index, (guest_payload, host_reply) in enumerate(PAYLOAD_PAIRS, start=1):
        try:
            stream.sendall(guest_payload)
        except OSError:
            fail(f"SEND_{index}")

        reply = recv_exact(stream, len(host_reply))
        if reply != host_reply:
            fail(f"REPLY_{index}")

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
    write_vdb_marker BANGBANG_VSOCK_GUEST_CONNECT_FAIL
  else
    emit_line BANGBANG_VSOCK_GUEST_CONNECT_FAIL_EMPTY
    write_vdb_marker BANGBANG_VSOCK_GUEST_CONNECT_FAIL
  fi
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
PAYLOAD_PAIRS = (
    (b"BANGBANG_VSOCK_GUEST_STREAM_ONE", b"BANGBANG_VSOCK_HOST_STREAM_ONE"),
    (b"BANGBANG_VSOCK_GUEST_STREAM_TWO", b"BANGBANG_VSOCK_HOST_STREAM_TWO"),
)
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
        for index, (guest_payload, host_payload) in enumerate(PAYLOAD_PAIRS, start=1):
            try:
                connection.sendall(guest_payload)
            except OSError:
                fail(f"SEND_{index}")
            payload = recv_exact(connection, len(host_payload))
            if payload != host_payload:
                fail(f"PAYLOAD_{index}")
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
if cmdline_has bangbang.entropy-read=1; then
  read_entropy_marker
elif cmdline_has bangbang.balloon-check=1; then
  check_balloon_marker
elif cmdline_has bangbang.block-writeback-flush=1; then
  flush_writeback_block_marker
elif cmdline_has bangbang.pmem-read-flush=1; then
  read_flush_pmem_marker
elif cmdline_has bangbang.mmds-v2-fetch=1; then
  fetch_mmds_v2_marker
elif cmdline_has bangbang.mmds-fetch=1; then
  fetch_mmds_marker
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
}

if [[ "$format" == "ext4" ]]; then
  preflight_ext4_preparation
fi

fetch_squashfs

case "$format" in
  squashfs)
    printf '%s\n' "$upstream_path"
    ;;
  ext4)
    prepare_ext4
    printf '%s\n' "$ext4_path"
    ;;
esac
