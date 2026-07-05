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
direct_boot_variant="direct-boot-v14"

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
GUEST_PAYLOAD = b"BANGBANG_VSOCK_GUEST_PAYLOAD"
HOST_REPLY = b"BANGBANG_VSOCK_HOST_REPLY"


def fail(reason):
    print(f"BANGBANG_VSOCK_GUEST_CONNECT_FAIL_{reason}")
    sys.exit(1)


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

    try:
        stream.sendall(GUEST_PAYLOAD)
    except OSError:
        fail("SEND")

    reply = b""
    while len(reply) < len(HOST_REPLY):
        try:
            chunk = stream.recv(len(HOST_REPLY) - len(reply))
        except OSError:
            fail("RECV")
        if not chunk:
            fail("EOF")
        reply += chunk

    if reply != HOST_REPLY:
        fail("REPLY")

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
if cmdline_has bangbang.mmds-v2-fetch=1; then
  fetch_mmds_v2_marker
elif cmdline_has bangbang.mmds-fetch=1; then
  fetch_mmds_marker
elif cmdline_has bangbang.vsock-guest-connect=1; then
  fetch_vsock_marker
elif cmdline_has bangbang.vsock-host-connect=1; then
  fetch_host_vsock_marker
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
