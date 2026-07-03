#!/usr/bin/env python3
"""Build the deterministic initrd used by the guest boot integration test."""

from __future__ import annotations

import argparse
import os
import struct
import sys
import tempfile
from pathlib import Path

BOOT_MARKER = b"BANGBANG_BOOT_OK\n"
BLOCK_READ_MARKER = b"BANGBANG_BLOCK_READ_OK"
DEV_TMPFS_NAME = b"devtmpfs\0"
DEV_PATH = b"/dev\0"
VDA_PATH = b"/dev/vda\0"
DEFAULT_RELATIVE_OUTPUT = Path("bangbang/guest-boot/initrd.cpio")
CPIO_NEWC_HEADER_SIZE = 110
CPIO_TRAILER = "TRAILER!!!"

ELF_BASE_VADDR = 0x400000
ELF_CODE_OFFSET = 0x100
ELF_PHDR_OFFSET = 0x40
ELF_HEADER_SIZE = 0x40
ELF_PROGRAM_HEADER_SIZE = 0x38

AT_FDCWD_U64 = (1 << 64) - 100
AARCH64_COND_NE = 1
LINUX_AARCH64_SYSCALL_MOUNT = 40
LINUX_AARCH64_SYSCALL_OPENAT = 56
LINUX_AARCH64_SYSCALL_READ = 63
LINUX_AARCH64_SYSCALL_WRITE = 64
LINUX_AARCH64_SYSCALL_EXIT = 93
# The tiny init has no UART drain loop, so keep serial writes within the FIFO depth.
GUEST_SERIAL_WRITE_CHUNK_SIZE = 16

S_IFDIR = 0o040000
S_IFCHR = 0o020000
S_IFREG = 0o100000
S_IFMT = 0o170000


def movz_64(register: int, immediate: int, shift: int = 0) -> bytes:
    hw = shift // 16
    instruction = 0xD2800000 | (hw << 21) | ((immediate & 0xFFFF) << 5) | register
    return struct.pack("<I", instruction)


def movk_64(register: int, immediate: int, shift: int) -> bytes:
    hw = shift // 16
    instruction = 0xF2800000 | (hw << 21) | ((immediate & 0xFFFF) << 5) | register
    return struct.pack("<I", instruction)


def mov_imm_64(register: int, value: int) -> bytes:
    return b"".join(
        (
            movz_64(register, value & 0xFFFF),
            movk_64(register, (value >> 16) & 0xFFFF, 16),
            movk_64(register, (value >> 32) & 0xFFFF, 32),
            movk_64(register, (value >> 48) & 0xFFFF, 48),
        )
    )


def svc_0() -> bytes:
    return struct.pack("<I", 0xD4000001)


def cmp_imm_64(register: int, immediate: int) -> bytes:
    if not 0 <= immediate <= 0xFFF:
        raise RuntimeError(f"unsupported CMP immediate: {immediate}")
    instruction = 0xF100001F | ((immediate & 0xFFF) << 10) | (register << 5)
    return struct.pack("<I", instruction)


def branch_to_self() -> bytes:
    return struct.pack("<I", 0x14000000)


class Aarch64CodeBuilder:
    def __init__(self) -> None:
        self._data = bytearray()
        self._labels: dict[str, int] = {}
        self._conditional_branches: list[tuple[int, str, int]] = []

    def emit(self, data: bytes) -> None:
        if len(data) % 4 != 0:
            raise RuntimeError("AArch64 code emission must stay instruction-aligned")
        self._data.extend(data)

    def label(self, name: str) -> None:
        if name in self._labels:
            raise RuntimeError(f"duplicate AArch64 label: {name}")
        self._labels[name] = len(self._data)

    def branch_cond(self, label: str, condition: int) -> None:
        if not 0 <= condition <= 0xF:
            raise RuntimeError(f"invalid AArch64 branch condition: {condition}")
        offset = len(self._data)
        self._conditional_branches.append((offset, label, condition))
        self.emit(bytes(4))

    def build(self) -> bytes:
        data = bytearray(self._data)
        for offset, label, condition in self._conditional_branches:
            try:
                target = self._labels[label]
            except KeyError as err:
                raise RuntimeError(f"unknown AArch64 label: {label}") from err
            delta = target - offset
            if delta % 4 != 0:
                raise RuntimeError("AArch64 conditional branch target is not aligned")
            immediate = delta // 4
            if not -(1 << 18) <= immediate < (1 << 18):
                raise RuntimeError("AArch64 conditional branch target is out of range")
            instruction = 0x54000000 | ((immediate & 0x7FFFF) << 5) | condition
            data[offset : offset + 4] = struct.pack("<I", instruction)
        return bytes(data)


def write_syscall(fd: int, buffer_vaddr: int, size: int) -> bytes:
    return b"".join(
        (
            movz_64(0, fd),
            mov_imm_64(1, buffer_vaddr),
            movz_64(2, size),
            movz_64(8, LINUX_AARCH64_SYSCALL_WRITE),
            svc_0(),
        )
    )


def write_syscalls(fd: int, buffer_vaddr: int, size: int) -> bytes:
    chunks = []
    offset = 0
    while offset < size:
        chunk_size = min(GUEST_SERIAL_WRITE_CHUNK_SIZE, size - offset)
        chunks.append(write_syscall(fd, buffer_vaddr + offset, chunk_size))
        offset += chunk_size
    return b"".join(chunks)


def build_guest_init_code(addresses: dict[str, int]) -> bytes:
    code = Aarch64CodeBuilder()
    code.emit(write_syscalls(1, addresses["boot_marker"], len(BOOT_MARKER)))
    code.emit(
        b"".join(
            (
                mov_imm_64(0, addresses["devtmpfs"]),
                mov_imm_64(1, addresses["dev"]),
                mov_imm_64(2, addresses["devtmpfs"]),
                movz_64(3, 0),
                movz_64(4, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_MOUNT),
                svc_0(),
            )
        )
    )
    code.emit(
        b"".join(
            (
                mov_imm_64(0, AT_FDCWD_U64),
                mov_imm_64(1, addresses["vda"]),
                movz_64(2, 0),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_OPENAT),
                svc_0(),
                mov_imm_64(1, addresses["block_read_buffer"]),
                movz_64(2, len(BLOCK_READ_MARKER)),
                movz_64(8, LINUX_AARCH64_SYSCALL_READ),
                svc_0(),
                cmp_imm_64(0, len(BLOCK_READ_MARKER)),
            )
        )
    )
    code.branch_cond("exit", AARCH64_COND_NE)
    code.emit(write_syscalls(1, addresses["block_read_buffer"], len(BLOCK_READ_MARKER)))
    code.label("exit")
    code.emit(
        b"".join(
            (
                movz_64(0, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_EXIT),
                svc_0(),
                branch_to_self(),
            )
        )
    )
    return code.build()


def guest_init_data() -> list[tuple[str, bytes]]:
    return [
        ("boot_marker", BOOT_MARKER),
        ("devtmpfs", DEV_TMPFS_NAME),
        ("dev", DEV_PATH),
        ("vda", VDA_PATH),
        ("block_read_buffer", bytes(len(BLOCK_READ_MARKER))),
    ]


def guest_init_addresses(code_size: int) -> dict[str, int]:
    addresses: dict[str, int] = {}
    data_offset = ELF_CODE_OFFSET + code_size
    for name, data in guest_init_data():
        addresses[name] = ELF_BASE_VADDR + data_offset
        data_offset += len(data)
    return addresses


def build_guest_init_elf() -> bytes:
    placeholder_addresses = {name: ELF_BASE_VADDR for name, _data in guest_init_data()}
    code_size = len(build_guest_init_code(placeholder_addresses))
    addresses = guest_init_addresses(code_size)
    code = build_guest_init_code(addresses)
    if len(code) != code_size:
        raise RuntimeError("guest init code size changed after address assignment")

    data = b"".join(data for _name, data in guest_init_data())
    file_size = ELF_CODE_OFFSET + len(code) + len(data)
    entry_vaddr = ELF_BASE_VADDR + ELF_CODE_OFFSET

    elf_ident = b"\x7fELF" + bytes([2, 1, 1, 0]) + bytes(8)
    elf_header = struct.pack(
        "<16sHHIQQQIHHHHHH",
        elf_ident,
        2,
        183,
        1,
        entry_vaddr,
        ELF_PHDR_OFFSET,
        0,
        0,
        ELF_HEADER_SIZE,
        ELF_PROGRAM_HEADER_SIZE,
        1,
        0,
        0,
        0,
    )
    program_header = struct.pack(
        "<IIQQQQQQ",
        1,
        7,
        0,
        ELF_BASE_VADDR,
        ELF_BASE_VADDR,
        file_size,
        file_size,
        0x1000,
    )

    headers = elf_header + program_header
    if len(headers) > ELF_CODE_OFFSET:
        raise RuntimeError("ELF headers do not fit before code offset")

    return headers + bytes(ELF_CODE_OFFSET - len(headers)) + code + data


def pad4(data: bytes) -> bytes:
    return data + bytes((-len(data)) % 4)


def pad512(data: bytes) -> bytes:
    return data + bytes((-len(data)) % 512)


def cpio_header(
    *,
    ino: int,
    mode: int,
    nlink: int,
    filesize: int,
    namesize: int,
    rdevmajor: int = 0,
    rdevminor: int = 0,
) -> bytes:
    fields = (
        "070701",
        f"{ino:08x}",
        f"{mode:08x}",
        "00000000",
        "00000000",
        f"{nlink:08x}",
        "00000000",
        f"{filesize:08x}",
        "00000000",
        "00000000",
        f"{rdevmajor:08x}",
        f"{rdevminor:08x}",
        f"{namesize:08x}",
        "00000000",
    )
    return "".join(fields).encode("ascii")


def cpio_entry(
    *,
    name: str,
    ino: int,
    mode: int,
    nlink: int = 1,
    data: bytes = b"",
    rdevmajor: int = 0,
    rdevminor: int = 0,
) -> bytes:
    name_bytes = name.encode("utf-8") + b"\0"
    entry = b"".join(
        (
            cpio_header(
                ino=ino,
                mode=mode,
                nlink=nlink,
                filesize=len(data),
                namesize=len(name_bytes),
                rdevmajor=rdevmajor,
                rdevminor=rdevminor,
            ),
            name_bytes,
        )
    )
    return pad4(entry) + pad4(data)


def build_initrd() -> bytes:
    guest_init = build_guest_init_elf()
    archive = b"".join(
        (
            cpio_entry(name="dev", ino=1, mode=S_IFDIR | 0o755, nlink=2),
            cpio_entry(
                name="dev/console",
                ino=2,
                mode=S_IFCHR | 0o600,
                rdevmajor=5,
                rdevminor=1,
            ),
            cpio_entry(name="init", ino=3, mode=S_IFREG | 0o755, data=guest_init),
            cpio_entry(name="TRAILER!!!", ino=4, mode=0, nlink=1),
        )
    )
    return pad512(archive)


def align4_offset(offset: int) -> int:
    return offset + ((-offset) % 4)


def parse_newc_entries(data: bytes) -> list[dict[str, object]]:
    entries: list[dict[str, object]] = []
    offset = 0

    while True:
        if offset + CPIO_NEWC_HEADER_SIZE > len(data):
            raise RuntimeError("guest initrd ended before newc trailer")

        header = data[offset : offset + CPIO_NEWC_HEADER_SIZE]
        if header[:6] != b"070701":
            raise RuntimeError(f"guest initrd newc entry at offset {offset} has invalid magic")

        field_names = (
            "ino",
            "mode",
            "uid",
            "gid",
            "nlink",
            "mtime",
            "filesize",
            "devmajor",
            "devminor",
            "rdevmajor",
            "rdevminor",
            "namesize",
            "check",
        )
        fields = {
            name: int(header[6 + (index * 8) : 14 + (index * 8)], 16)
            for index, name in enumerate(field_names)
        }
        offset += CPIO_NEWC_HEADER_SIZE

        namesize = int(fields["namesize"])
        name_end = offset + namesize
        if namesize == 0 or name_end > len(data):
            raise RuntimeError("guest initrd newc entry has invalid name size")

        raw_name = data[offset:name_end]
        if raw_name[-1:] != b"\0":
            raise RuntimeError("guest initrd newc entry name is not NUL-terminated")
        name = raw_name[:-1].decode("utf-8")
        offset = align4_offset(name_end)

        filesize = int(fields["filesize"])
        data_end = offset + filesize
        if data_end > len(data):
            raise RuntimeError(f"guest initrd newc entry {name} payload is truncated")
        payload = data[offset:data_end]
        offset = align4_offset(data_end)

        entry = {
            "name": name,
            "payload": payload,
            **fields,
        }
        entries.append(entry)

        if name == CPIO_TRAILER:
            if any(data[offset:]):
                raise RuntimeError("guest initrd has non-zero bytes after newc trailer")
            return entries


def required_entry(entries: dict[str, dict[str, object]], name: str) -> dict[str, object]:
    try:
        return entries[name]
    except KeyError as err:
        raise RuntimeError(f"guest initrd is missing {name}") from err


def file_type(mode: object) -> int:
    return int(mode) & S_IFMT


def validate_initrd(data: bytes) -> None:
    if not data:
        raise RuntimeError("guest initrd is empty")
    if len(data) % 512 != 0:
        raise RuntimeError("guest initrd is not padded to a 512-byte boundary")

    parsed = parse_newc_entries(data)
    entries = {str(entry["name"]): entry for entry in parsed}
    names = [str(entry["name"]) for entry in parsed]
    expected_names = ["dev", "dev/console", "init", CPIO_TRAILER]
    if names != expected_names:
        raise RuntimeError(f"guest initrd entries {names!r} do not match {expected_names!r}")

    dev = required_entry(entries, "dev")
    if file_type(dev["mode"]) != S_IFDIR:
        raise RuntimeError("guest initrd dev entry is not a directory")

    console = required_entry(entries, "dev/console")
    if file_type(console["mode"]) != S_IFCHR:
        raise RuntimeError("guest initrd dev/console entry is not a character device")
    if int(console["rdevmajor"]) != 5 or int(console["rdevminor"]) != 1:
        raise RuntimeError("guest initrd dev/console is not character device 5:1")

    init = required_entry(entries, "init")
    if file_type(init["mode"]) != S_IFREG:
        raise RuntimeError("guest initrd init entry is not a regular file")
    payload = bytes(init["payload"])
    if not payload.startswith(b"\x7fELF"):
        raise RuntimeError("guest initrd init payload is not an ELF file")
    if BOOT_MARKER not in payload:
        raise RuntimeError("guest initrd init payload does not contain the boot marker")
    for guest_path in (DEV_TMPFS_NAME, DEV_PATH, VDA_PATH):
        if guest_path not in payload:
            raise RuntimeError(
                f"guest initrd init payload does not contain {guest_path!r}"
            )


def default_output_path() -> Path:
    script_path = Path(__file__).resolve()
    repo_root = script_path.parent.parent
    cache_root = Path(
        os.environ.get(
            "BANGBANG_GUEST_ARTIFACTS_DIR",
            str(repo_root / ".tmp" / "guest-artifacts"),
        )
    )
    return cache_root / DEFAULT_RELATIVE_OUTPUT


def write_output(path: Path, data: bytes) -> None:
    if path.is_symlink():
        raise RuntimeError(f"guest initrd path must not be a symlink: {path}")
    if path.exists():
        if not path.is_file():
            raise RuntimeError(f"guest initrd path exists but is not a regular file: {path}")
        if path.read_bytes() == data:
            return

    parent = path.parent
    if parent.exists() and not parent.is_dir():
        raise RuntimeError(f"guest initrd parent path exists but is not a directory: {parent}")
    parent.mkdir(parents=True, exist_ok=True)

    temp_name = ""
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            prefix=f"{path.name}.",
            suffix=".tmp",
            dir=parent,
            delete=False,
        ) as temp_file:
            temp_name = temp_file.name
            temp_file.write(data)
        os.chmod(temp_name, 0o644)
        os.replace(temp_name, path)
        temp_name = ""
    finally:
        if temp_name:
            try:
                os.unlink(temp_name)
            except FileNotFoundError:
                pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build the deterministic initrd used by bangbang guest boot integration tests.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=default_output_path(),
        help="Output initrd path. Defaults under BANGBANG_GUEST_ARTIFACTS_DIR or .tmp/guest-artifacts.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Validate the generated initrd structure after writing it.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        output_path = args.output
        data = build_initrd()
        if args.check:
            validate_initrd(data)
        write_output(output_path, data)
        if args.check:
            validate_initrd(output_path.read_bytes())
    except OSError as err:
        print(f"failed to build guest boot initrd: {err}", file=sys.stderr)
        return 1
    except RuntimeError as err:
        print(str(err), file=sys.stderr)
        return 1

    print(output_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
