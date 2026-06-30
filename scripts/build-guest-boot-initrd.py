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
DEFAULT_RELATIVE_OUTPUT = Path("bangbang/guest-boot/initrd.cpio")
CPIO_NEWC_HEADER_SIZE = 110
CPIO_TRAILER = "TRAILER!!!"

ELF_BASE_VADDR = 0x400000
ELF_CODE_OFFSET = 0x100
ELF_PHDR_OFFSET = 0x40
ELF_HEADER_SIZE = 0x40
ELF_PROGRAM_HEADER_SIZE = 0x38

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


def branch_to_self() -> bytes:
    return struct.pack("<I", 0x14000000)


def build_guest_init_elf() -> bytes:
    marker_vaddr = ELF_BASE_VADDR + ELF_CODE_OFFSET + 48
    code = b"".join(
        (
            movz_64(0, 1),
            mov_imm_64(1, marker_vaddr),
            movz_64(2, len(BOOT_MARKER)),
            movz_64(8, 64),
            svc_0(),
            movz_64(0, 0),
            movz_64(8, 93),
            svc_0(),
            branch_to_self(),
        )
    )

    if len(code) != 48:
        raise RuntimeError(f"unexpected guest init code size: {len(code)}")

    marker_offset = ELF_CODE_OFFSET + len(code)
    file_size = marker_offset + len(BOOT_MARKER)
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
        5,
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

    return headers + bytes(ELF_CODE_OFFSET - len(headers)) + code + BOOT_MARKER


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
