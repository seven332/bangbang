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
BLOCK_WRITE_MARKER = b"BANGBANG_BLOCK_WRITE_OK"
BLOCK_WRITE_SECTOR_SIZE = 512
ROOTFS_READ_MARKER = b"BANGBANG_ROOTFS_READ_OK"
SMP_SECONDARY_MARKER = b"BANGBANG_SECONDARY_CPU_OK\n"
SMP_PROGRESS_READY_MARKER = b"BBSMPREADY\n"
SMP_PROGRESS_CPU0_TOKEN = b"\xa5"
SMP_PROGRESS_CPU1_TOKEN = b"\xd3"
SMP_PROGRESS_CHILD_STACK_SIZE = 4096
SMP_HOTPLUG_READY_MARKER = b"BBHOTREADY\n"
SMP_HOTPLUG_OFF_MARKER = b"BBHOTOFF\n"
SMP_HOTPLUG_DONE_MARKER = b"BBHOTDONE\n"
SMP_HOTPLUG_CHILD_STACK_SIZE = 4096
SMP_HOTPLUG_QUIESCENCE_ITERATIONS = 4095
ROOTFS_OS_RELEASE_READ_SIZE = 256
CMDLINE_BEGIN_MARKER = b"BANGBANG_CMDLINE_BEGIN\n"
CMDLINE_END_MARKER = b"BANGBANG_CMDLINE_END\n"
VIRTIO_PCI_RNG_BOUND_MARKER = b"BANGBANG_VIRTIO_PCI_RNG_BOUND\n"
VIRTIO_PCI_RNG_IO_MARKER = b"BANGBANG_VIRTIO_PCI_RNG_IO_OK\n"
VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN\n"
VIRTIO_PCI_RNG_IRQ_BEFORE_END = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_BEFORE_END\n"
VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN\n"
VIRTIO_PCI_RNG_IRQ_AFTER_END = b"BANGBANG_VIRTIO_PCI_RNG_IRQ_AFTER_END\n"
VIRTIO_PCI_RNG_SUCCESS_MARKER = b"BANGBANG_VIRTIO_PCI_RNG_OK\n"
VIRTIO_PCI_RNG_FAILURE_MARKER = b"BANGBANG_VIRTIO_PCI_RNG_FAIL\n"
VIRTIO_PCI_RNG_EXPECTED_DRIVER = b"virtio_rng"
VIRTIO_PCI_RNG_ENTROPY_BYTE = 0xA5
VIRTIO_PCI_RNG_READ_SIZE = 32
VIRTIO_PCI_RNG_PROC_BUFFER_SIZE = 2048
VIRTIO_PCI_RNG_YIELD_COUNT = 64
DEV_TMPFS_NAME = b"devtmpfs\0"
DEV_PATH = b"/dev\0"
MNT_PATH = b"/mnt\0"
PROC_FS_NAME = b"proc\0"
PROC_PATH = b"/proc\0"
PROC_CMDLINE_PATH = b"/proc/cmdline\0"
PROC_INTERRUPTS_PATH = b"/proc/interrupts\0"
SYS_FS_NAME = b"sysfs\0"
SYS_PATH = b"/sys\0"
VIRTIO_PCI_RNG_CURRENT_PATH = b"/sys/class/misc/hw_random/rng_current\0"
VIRTIO_PCI_RNG_DEVICE_PATH = b"/dev/hwrng\0"
CPU1_ONLINE_PATH = b"/sys/devices/system/cpu/cpu1/online\0"
SQUASHFS_NAME = b"squashfs\0"
VDA_PATH = b"/dev/vda\0"
ROOTFS_OS_RELEASE_PATH = b"/mnt/etc/os-release\0"
DEFAULT_RELATIVE_OUTPUT = Path("bangbang/guest-boot/initrd.cpio")
CPIO_NEWC_HEADER_SIZE = 110
CPIO_TRAILER = "TRAILER!!!"

ELF_BASE_VADDR = 0x400000
ELF_CODE_OFFSET = 0x100
ELF_PHDR_OFFSET = 0x40
ELF_HEADER_SIZE = 0x40
ELF_PROGRAM_HEADER_SIZE = 0x38

AT_FDCWD_U64 = (1 << 64) - 100
AARCH64_COND_EQ = 0
AARCH64_COND_NE = 1
AARCH64_COND_MI = 4
LINUX_MOUNT_FLAG_RDONLY = 1
LINUX_OPEN_FLAG_RDWR = 2
LINUX_OPEN_FLAG_WRONLY = 1
LINUX_AARCH64_SYSCALL_MOUNT = 40
LINUX_AARCH64_SYSCALL_OPENAT = 56
LINUX_AARCH64_SYSCALL_CLOSE = 57
LINUX_AARCH64_SYSCALL_READ = 63
LINUX_AARCH64_SYSCALL_WRITE = 64
LINUX_AARCH64_SYSCALL_FSYNC = 82
LINUX_AARCH64_SYSCALL_EXIT = 93
LINUX_AARCH64_SYSCALL_REBOOT = 142
LINUX_AARCH64_SYSCALL_SCHED_SETAFFINITY = 122
LINUX_AARCH64_SYSCALL_SCHED_YIELD = 124
LINUX_AARCH64_SYSCALL_GETCPU = 168
LINUX_AARCH64_SYSCALL_CLONE = 220
LINUX_CLONE_VM = 0x100
LINUX_SIGCHLD = 17
LINUX_REBOOT_MAGIC1 = 0xFEE1DEAD
LINUX_REBOOT_MAGIC2 = 0x28121969
LINUX_REBOOT_CMD_RESTART = 0x01234567
LINUX_REBOOT_CMD_POWER_OFF = 0x4321FEDC
# The tiny init has no UART drain loop, so keep serial writes within the FIFO depth.
GUEST_SERIAL_WRITE_CHUNK_SIZE = 16
# Match bangbang's arm64 command-line capacity so the serial capture can include
# any valid guest command line plus the zero-filled tail after a shorter read.
GUEST_CMDLINE_BUFFER_SIZE = 2048

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


def mov_reg_64(destination: int, source: int) -> bytes:
    instruction = 0xAA0003E0 | (source << 16) | destination
    return struct.pack("<I", instruction)


def svc_0() -> bytes:
    return struct.pack("<I", 0xD4000001)


def cmp_imm_64(register: int, immediate: int) -> bytes:
    if not 0 <= immediate <= 0xFFF:
        raise RuntimeError(f"unsupported CMP immediate: {immediate}")
    instruction = 0xF100001F | ((immediate & 0xFFF) << 10) | (register << 5)
    return struct.pack("<I", instruction)


def add_imm_32(destination: int, source: int, immediate: int) -> bytes:
    if not 0 <= immediate <= 0xFFF:
        raise RuntimeError(f"unsupported ADD immediate: {immediate}")
    instruction = (
        0x11000000
        | ((immediate & 0xFFF) << 10)
        | (source << 5)
        | destination
    )
    return struct.pack("<I", instruction)


def add_imm_64(destination: int, source: int, immediate: int) -> bytes:
    if not 0 <= immediate <= 0xFFF:
        raise RuntimeError(f"unsupported ADD immediate: {immediate}")
    instruction = (
        0x91000000
        | ((immediate & 0xFFF) << 10)
        | (source << 5)
        | destination
    )
    return struct.pack("<I", instruction)


def sub_imm_64(destination: int, source: int, immediate: int) -> bytes:
    if not 0 <= immediate <= 0xFFF:
        raise RuntimeError(f"unsupported SUB immediate: {immediate}")
    instruction = (
        0xD1000000
        | ((immediate & 0xFFF) << 10)
        | (source << 5)
        | destination
    )
    return struct.pack("<I", instruction)


def cmp_reg_32(left: int, right: int) -> bytes:
    instruction = 0x6B00001F | (right << 16) | (left << 5)
    return struct.pack("<I", instruction)


def ldr_u32(destination: int, base: int) -> bytes:
    instruction = 0xB9400000 | (base << 5) | destination
    return struct.pack("<I", instruction)


def ldrb_u32(destination: int, base: int) -> bytes:
    instruction = 0x39400000 | (base << 5) | destination
    return struct.pack("<I", instruction)


def ldar_u32(destination: int, base: int) -> bytes:
    instruction = 0x88DFFC00 | (base << 5) | destination
    return struct.pack("<I", instruction)


def stlr_u32(source: int, base: int) -> bytes:
    instruction = 0x889FFC00 | (base << 5) | source
    return struct.pack("<I", instruction)


def branch_to_self() -> bytes:
    return struct.pack("<I", 0x14000000)


class Aarch64CodeBuilder:
    def __init__(self) -> None:
        self._data = bytearray()
        self._labels: dict[str, int] = {}
        self._conditional_branches: list[tuple[int, str, int]] = []
        self._unconditional_branches: list[tuple[int, str]] = []

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

    def branch(self, label: str) -> None:
        offset = len(self._data)
        self._unconditional_branches.append((offset, label))
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
        for offset, label in self._unconditional_branches:
            try:
                target = self._labels[label]
            except KeyError as err:
                raise RuntimeError(f"unknown AArch64 label: {label}") from err
            delta = target - offset
            if delta % 4 != 0:
                raise RuntimeError("AArch64 branch target is not aligned")
            immediate = delta // 4
            if not -(1 << 25) <= immediate < (1 << 25):
                raise RuntimeError("AArch64 branch target is out of range")
            instruction = 0x14000000 | (immediate & 0x3FFFFFF)
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


def write_from_open_fd(buffer_vaddr: int, size: int) -> bytes:
    return b"".join(
        (
            mov_imm_64(1, buffer_vaddr),
            movz_64(2, size),
            movz_64(8, LINUX_AARCH64_SYSCALL_WRITE),
            svc_0(),
        )
    )


def block_write_sector() -> bytes:
    if len(BLOCK_WRITE_MARKER) > BLOCK_WRITE_SECTOR_SIZE:
        raise RuntimeError("guest block write marker does not fit in one sector")
    return BLOCK_WRITE_MARKER + bytes(BLOCK_WRITE_SECTOR_SIZE - len(BLOCK_WRITE_MARKER))


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
                mov_imm_64(0, addresses["procfs"]),
                mov_imm_64(1, addresses["proc"]),
                mov_imm_64(2, addresses["procfs"]),
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
                mov_imm_64(1, addresses["proc_cmdline"]),
                movz_64(2, 0),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_OPENAT),
                svc_0(),
                mov_imm_64(1, addresses["cmdline_buffer"]),
                movz_64(2, GUEST_CMDLINE_BUFFER_SIZE),
                movz_64(8, LINUX_AARCH64_SYSCALL_READ),
                svc_0(),
            )
        )
    )
    code.emit(
        write_syscalls(1, addresses["cmdline_begin_marker"], len(CMDLINE_BEGIN_MARKER))
    )
    code.emit(write_syscalls(1, addresses["cmdline_buffer"], GUEST_CMDLINE_BUFFER_SIZE))
    code.emit(write_syscalls(1, addresses["cmdline_end_marker"], len(CMDLINE_END_MARKER)))
    code.emit(
        b"".join(
            (
                mov_imm_64(0, addresses["vda"]),
                mov_imm_64(1, addresses["mnt"]),
                mov_imm_64(2, addresses["squashfs"]),
                movz_64(3, LINUX_MOUNT_FLAG_RDONLY),
                movz_64(4, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_MOUNT),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("after_rootfs_attempt", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_imm_64(0, AT_FDCWD_U64),
                mov_imm_64(1, addresses["rootfs_os_release"]),
                movz_64(2, 0),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_OPENAT),
                svc_0(),
                mov_imm_64(1, addresses["rootfs_os_release_buffer"]),
                movz_64(2, ROOTFS_OS_RELEASE_READ_SIZE),
                movz_64(8, LINUX_AARCH64_SYSCALL_READ),
                svc_0(),
                cmp_imm_64(0, ROOTFS_OS_RELEASE_READ_SIZE),
            )
        )
    )
    code.branch_cond("after_rootfs_attempt", AARCH64_COND_NE)
    code.emit(
        write_syscalls(
            1,
            addresses["rootfs_os_release_buffer"],
            ROOTFS_OS_RELEASE_READ_SIZE,
        )
    )
    code.emit(write_syscalls(1, addresses["rootfs_read_marker"], len(ROOTFS_READ_MARKER)))
    code.label("after_rootfs_attempt")
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
    code.emit(
        b"".join(
            (
                mov_imm_64(0, AT_FDCWD_U64),
                mov_imm_64(1, addresses["vda"]),
                movz_64(2, LINUX_OPEN_FLAG_RDWR),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_OPENAT),
                svc_0(),
                mov_reg_64(19, 0),
                write_from_open_fd(
                    addresses["block_write_sector"], BLOCK_WRITE_SECTOR_SIZE
                ),
                cmp_imm_64(0, BLOCK_WRITE_SECTOR_SIZE),
            )
        )
    )
    code.branch_cond("exit", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_reg_64(0, 19),
                movz_64(8, LINUX_AARCH64_SYSCALL_FSYNC),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("exit", AARCH64_COND_NE)
    code.emit(write_syscalls(1, addresses["block_write_marker"], len(BLOCK_WRITE_MARKER)))
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
        ("cmdline_begin_marker", CMDLINE_BEGIN_MARKER),
        ("cmdline_end_marker", CMDLINE_END_MARKER),
        ("devtmpfs", DEV_TMPFS_NAME),
        ("dev", DEV_PATH),
        ("mnt", MNT_PATH),
        ("procfs", PROC_FS_NAME),
        ("proc", PROC_PATH),
        ("proc_cmdline", PROC_CMDLINE_PATH),
        ("squashfs", SQUASHFS_NAME),
        ("vda", VDA_PATH),
        ("rootfs_os_release", ROOTFS_OS_RELEASE_PATH),
        ("cmdline_buffer", bytes(GUEST_CMDLINE_BUFFER_SIZE)),
        ("rootfs_os_release_buffer", bytes(ROOTFS_OS_RELEASE_READ_SIZE)),
        ("block_read_buffer", bytes(len(BLOCK_READ_MARKER))),
        ("block_write_marker", BLOCK_WRITE_MARKER),
        ("block_write_sector", block_write_sector()),
        ("rootfs_read_marker", ROOTFS_READ_MARKER),
    ]


def guest_init_addresses(code_size: int) -> dict[str, int]:
    addresses: dict[str, int] = {}
    data_offset = ELF_CODE_OFFSET + code_size
    for name, data in guest_init_data():
        addresses[name] = ELF_BASE_VADDR + data_offset
        data_offset += len(data)
    return addresses


def build_guest_elf(code: bytes, data: bytes) -> bytes:
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


def build_guest_init_elf() -> bytes:
    placeholder_addresses = {name: ELF_BASE_VADDR for name, _data in guest_init_data()}
    code_size = len(build_guest_init_code(placeholder_addresses))
    addresses = guest_init_addresses(code_size)
    code = build_guest_init_code(addresses)
    if len(code) != code_size:
        raise RuntimeError("guest init code size changed after address assignment")

    data = b"".join(data for _name, data in guest_init_data())
    return build_guest_elf(code, data)


def emit_mount(
    code: Aarch64CodeBuilder,
    source_vaddr: int,
    target_vaddr: int,
    filesystem_vaddr: int,
    failure_label: str,
) -> None:
    code.emit(
        b"".join(
            (
                mov_imm_64(0, source_vaddr),
                mov_imm_64(1, target_vaddr),
                mov_imm_64(2, filesystem_vaddr),
                movz_64(3, 0),
                movz_64(4, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_MOUNT),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond(failure_label, AARCH64_COND_NE)


def emit_open_and_read(
    code: Aarch64CodeBuilder,
    path_vaddr: int,
    buffer_vaddr: int,
    size: int,
    failure_label: str,
) -> None:
    code.emit(
        b"".join(
            (
                mov_imm_64(0, AT_FDCWD_U64),
                mov_imm_64(1, path_vaddr),
                movz_64(2, 0),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_OPENAT),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond(failure_label, AARCH64_COND_MI)
    code.emit(
        b"".join(
            (
                mov_imm_64(1, buffer_vaddr),
                movz_64(2, size),
                movz_64(8, LINUX_AARCH64_SYSCALL_READ),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond(failure_label, AARCH64_COND_EQ)
    code.branch_cond(failure_label, AARCH64_COND_MI)


def build_pci_rng_init_code(addresses: dict[str, int]) -> bytes:
    code = Aarch64CodeBuilder()
    emit_mount(
        code,
        addresses["devtmpfs"],
        addresses["dev"],
        addresses["devtmpfs"],
        "failure",
    )
    emit_mount(
        code,
        addresses["procfs"],
        addresses["proc"],
        addresses["procfs"],
        "failure",
    )
    emit_mount(
        code,
        addresses["sysfs"],
        addresses["sys"],
        addresses["sysfs"],
        "failure",
    )

    emit_open_and_read(
        code,
        addresses["rng_current_path"],
        addresses["rng_current_buffer"],
        32,
        "failure",
    )
    code.emit(mov_imm_64(20, addresses["rng_current_buffer"]))
    for expected in VIRTIO_PCI_RNG_EXPECTED_DRIVER:
        code.emit(
            b"".join(
                (
                    ldrb_u32(21, 20),
                    movz_64(22, expected),
                    cmp_reg_32(21, 22),
                )
            )
        )
        code.branch_cond("failure", AARCH64_COND_NE)
        code.emit(add_imm_64(20, 20, 1))
    code.emit(
        write_syscalls(
            1,
            addresses["bound_marker"],
            len(VIRTIO_PCI_RNG_BOUND_MARKER),
        )
    )

    code.emit(
        write_syscalls(
            1,
            addresses["irq_before_begin"],
            len(VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN),
        )
    )
    emit_open_and_read(
        code,
        addresses["proc_interrupts_path"],
        addresses["irq_before_buffer"],
        VIRTIO_PCI_RNG_PROC_BUFFER_SIZE,
        "failure",
    )
    code.emit(
        write_syscalls(
            1,
            addresses["irq_before_buffer"],
            VIRTIO_PCI_RNG_PROC_BUFFER_SIZE,
        )
    )
    code.emit(
        write_syscalls(
            1,
            addresses["irq_before_end"],
            len(VIRTIO_PCI_RNG_IRQ_BEFORE_END),
        )
    )

    emit_open_and_read(
        code,
        addresses["hwrng_path"],
        addresses["entropy_buffer"],
        VIRTIO_PCI_RNG_READ_SIZE,
        "failure",
    )
    code.emit(cmp_imm_64(0, VIRTIO_PCI_RNG_READ_SIZE))
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_imm_64(20, addresses["entropy_buffer"]),
                movz_64(21, VIRTIO_PCI_RNG_READ_SIZE),
            )
        )
    )
    code.label("check_entropy")
    code.emit(
        b"".join(
            (
                ldrb_u32(22, 20),
                movz_64(23, VIRTIO_PCI_RNG_ENTROPY_BYTE),
                cmp_reg_32(22, 23),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                add_imm_64(20, 20, 1),
                sub_imm_64(21, 21, 1),
                cmp_imm_64(21, 0),
            )
        )
    )
    code.branch_cond("check_entropy", AARCH64_COND_NE)
    code.emit(
        write_syscalls(
            1,
            addresses["io_marker"],
            len(VIRTIO_PCI_RNG_IO_MARKER),
        )
    )

    code.emit(movz_64(20, VIRTIO_PCI_RNG_YIELD_COUNT))
    code.label("yield_for_config_irq")
    code.emit(
        b"".join(
            (
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_YIELD),
                svc_0(),
                sub_imm_64(20, 20, 1),
                cmp_imm_64(20, 0),
            )
        )
    )
    code.branch_cond("yield_for_config_irq", AARCH64_COND_NE)

    code.emit(
        write_syscalls(
            1,
            addresses["irq_after_begin"],
            len(VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN),
        )
    )
    emit_open_and_read(
        code,
        addresses["proc_interrupts_path"],
        addresses["irq_after_buffer"],
        VIRTIO_PCI_RNG_PROC_BUFFER_SIZE,
        "failure",
    )
    code.emit(
        write_syscalls(
            1,
            addresses["irq_after_buffer"],
            VIRTIO_PCI_RNG_PROC_BUFFER_SIZE,
        )
    )
    code.emit(
        write_syscalls(
            1,
            addresses["irq_after_end"],
            len(VIRTIO_PCI_RNG_IRQ_AFTER_END),
        )
    )
    code.emit(
        write_syscalls(
            1,
            addresses["success_marker"],
            len(VIRTIO_PCI_RNG_SUCCESS_MARKER),
        )
    )
    code.branch("exit")

    code.label("failure")
    code.emit(
        write_syscalls(
            1,
            addresses["failure_marker"],
            len(VIRTIO_PCI_RNG_FAILURE_MARKER),
        )
    )
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


def pci_rng_init_data() -> list[tuple[str, bytes]]:
    return [
        ("devtmpfs", DEV_TMPFS_NAME),
        ("dev", DEV_PATH),
        ("procfs", PROC_FS_NAME),
        ("proc", PROC_PATH),
        ("sysfs", SYS_FS_NAME),
        ("sys", SYS_PATH),
        ("rng_current_path", VIRTIO_PCI_RNG_CURRENT_PATH),
        ("hwrng_path", VIRTIO_PCI_RNG_DEVICE_PATH),
        ("proc_interrupts_path", PROC_INTERRUPTS_PATH),
        ("bound_marker", VIRTIO_PCI_RNG_BOUND_MARKER),
        ("io_marker", VIRTIO_PCI_RNG_IO_MARKER),
        ("irq_before_begin", VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN),
        ("irq_before_end", VIRTIO_PCI_RNG_IRQ_BEFORE_END),
        ("irq_after_begin", VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN),
        ("irq_after_end", VIRTIO_PCI_RNG_IRQ_AFTER_END),
        ("success_marker", VIRTIO_PCI_RNG_SUCCESS_MARKER),
        ("failure_marker", VIRTIO_PCI_RNG_FAILURE_MARKER),
        ("rng_current_buffer", bytes(32)),
        ("entropy_buffer", bytes(VIRTIO_PCI_RNG_READ_SIZE)),
        ("irq_before_buffer", bytes(VIRTIO_PCI_RNG_PROC_BUFFER_SIZE)),
        ("irq_after_buffer", bytes(VIRTIO_PCI_RNG_PROC_BUFFER_SIZE)),
    ]


def pci_rng_init_addresses(code_size: int) -> dict[str, int]:
    addresses: dict[str, int] = {}
    data_offset = ELF_CODE_OFFSET + code_size
    for name, data in pci_rng_init_data():
        addresses[name] = ELF_BASE_VADDR + data_offset
        data_offset += len(data)
    return addresses


def build_pci_rng_init_elf() -> bytes:
    placeholder_addresses = {
        name: ELF_BASE_VADDR for name, _data in pci_rng_init_data()
    }
    code_size = len(build_pci_rng_init_code(placeholder_addresses))
    addresses = pci_rng_init_addresses(code_size)
    code = build_pci_rng_init_code(addresses)
    if len(code) != code_size:
        raise RuntimeError("PCI rng init code size changed after address assignment")
    data = b"".join(data for _name, data in pci_rng_init_data())
    return build_guest_elf(code, data)


def smp_init_data() -> list[tuple[str, bytes]]:
    return [
        ("affinity_mask", struct.pack("<Q", 1 << 1)),
        ("observed_cpu", bytes(4)),
        ("secondary_marker", SMP_SECONDARY_MARKER),
    ]


def smp_init_addresses(code_size: int) -> dict[str, int]:
    addresses: dict[str, int] = {}
    data_offset = ELF_CODE_OFFSET + code_size
    for name, data in smp_init_data():
        addresses[name] = ELF_BASE_VADDR + data_offset
        data_offset += len(data)
    return addresses


def build_smp_init_code(addresses: dict[str, int]) -> bytes:
    code = Aarch64CodeBuilder()
    code.emit(
        b"".join(
            (
                movz_64(0, 0),
                movz_64(1, 8),
                mov_imm_64(2, addresses["affinity_mask"]),
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_SETAFFINITY),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_imm_64(0, addresses["observed_cpu"]),
                movz_64(1, 0),
                movz_64(2, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_GETCPU),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(mov_imm_64(1, addresses["observed_cpu"]))
    code.emit(ldr_u32(0, 1))
    code.emit(cmp_imm_64(0, 1))
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        write_syscalls(
            1,
            addresses["secondary_marker"],
            len(SMP_SECONDARY_MARKER),
        )
    )
    code.emit(
        b"".join(
            (
                movz_64(0, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_EXIT),
                svc_0(),
            )
        )
    )
    code.label("failure")
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                movz_64(8, LINUX_AARCH64_SYSCALL_EXIT),
                svc_0(),
                branch_to_self(),
            )
        )
    )
    return code.build()


def build_smp_init_elf() -> bytes:
    placeholder_addresses = {name: ELF_BASE_VADDR for name, _data in smp_init_data()}
    code_size = len(build_smp_init_code(placeholder_addresses))
    addresses = smp_init_addresses(code_size)
    code = build_smp_init_code(addresses)
    if len(code) != code_size:
        raise RuntimeError("guest SMP init code size changed after address assignment")

    data = b"".join(data for _name, data in smp_init_data())
    return build_guest_elf(code, data)


def smp_progress_init_data(code_size: int) -> list[tuple[str, bytes]]:
    data = [
        ("cpu0_affinity_mask", struct.pack("<Q", 1 << 0)),
        ("cpu1_affinity_mask", struct.pack("<Q", 1 << 1)),
        ("cpu0_observed", bytes(4)),
        ("cpu1_observed", bytes(4)),
        ("cpu1_ready", bytes(4)),
        ("start", bytes(4)),
        ("ready_marker", SMP_PROGRESS_READY_MARKER),
        ("cpu0_token", SMP_PROGRESS_CPU0_TOKEN),
        ("cpu1_token", SMP_PROGRESS_CPU1_TOKEN),
    ]
    stack_offset = ELF_CODE_OFFSET + code_size + sum(len(value) for _name, value in data)
    data.append(("stack_padding", bytes((-stack_offset) % 16)))
    data.append(("child_stack", bytes(SMP_PROGRESS_CHILD_STACK_SIZE)))
    return data


def smp_progress_init_addresses(code_size: int) -> dict[str, int]:
    addresses: dict[str, int] = {}
    data_offset = ELF_CODE_OFFSET + code_size
    for name, data in smp_progress_init_data(code_size):
        addresses[name] = ELF_BASE_VADDR + data_offset
        data_offset += len(data)
    return addresses


def emit_smp_progress_affinity_check(
    code: Aarch64CodeBuilder,
    addresses: dict[str, int],
    *,
    affinity_mask: str,
    observed_cpu: str,
    expected_cpu: int,
) -> None:
    code.emit(
        b"".join(
            (
                movz_64(0, 0),
                movz_64(1, 8),
                mov_imm_64(2, addresses[affinity_mask]),
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_SETAFFINITY),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_imm_64(0, addresses[observed_cpu]),
                movz_64(1, 0),
                movz_64(2, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_GETCPU),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(mov_imm_64(1, addresses[observed_cpu]))
    code.emit(ldr_u32(0, 1))
    code.emit(cmp_imm_64(0, expected_cpu))
    code.branch_cond("failure", AARCH64_COND_NE)


def emit_smp_progress_loop(
    code: Aarch64CodeBuilder,
    *,
    label: str,
    token_address: int,
) -> None:
    code.label(label)
    code.emit(write_syscall(1, token_address, 1))
    code.emit(
        b"".join(
            (
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_YIELD),
                svc_0(),
            )
        )
    )
    code.branch(label)


def build_smp_progress_init_code(addresses: dict[str, int]) -> bytes:
    code = Aarch64CodeBuilder()
    emit_smp_progress_affinity_check(
        code,
        addresses,
        affinity_mask="cpu0_affinity_mask",
        observed_cpu="cpu0_observed",
        expected_cpu=0,
    )
    code.emit(
        b"".join(
            (
                mov_imm_64(0, LINUX_CLONE_VM | LINUX_SIGCHLD),
                mov_imm_64(
                    1,
                    addresses["child_stack"] + SMP_PROGRESS_CHILD_STACK_SIZE,
                ),
                movz_64(2, 0),
                movz_64(3, 0),
                movz_64(4, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_CLONE),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_MI)
    code.branch_cond("child", AARCH64_COND_EQ)

    code.label("parent_wait_ready")
    code.emit(mov_imm_64(1, addresses["cpu1_ready"]))
    code.emit(ldar_u32(0, 1))
    code.emit(cmp_imm_64(0, 1))
    code.branch_cond("parent_wait_ready", AARCH64_COND_NE)
    code.emit(
        write_syscalls(
            1,
            addresses["ready_marker"],
            len(SMP_PROGRESS_READY_MARKER),
        )
    )
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                mov_imm_64(1, addresses["start"]),
                stlr_u32(0, 1),
            )
        )
    )
    emit_smp_progress_loop(
        code,
        label="cpu0_progress",
        token_address=addresses["cpu0_token"],
    )

    code.label("child")
    emit_smp_progress_affinity_check(
        code,
        addresses,
        affinity_mask="cpu1_affinity_mask",
        observed_cpu="cpu1_observed",
        expected_cpu=1,
    )
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                mov_imm_64(1, addresses["cpu1_ready"]),
                stlr_u32(0, 1),
            )
        )
    )
    code.label("child_wait_start")
    code.emit(mov_imm_64(1, addresses["start"]))
    code.emit(ldar_u32(0, 1))
    code.emit(cmp_imm_64(0, 1))
    code.branch_cond("child_wait_start", AARCH64_COND_NE)
    emit_smp_progress_loop(
        code,
        label="cpu1_progress",
        token_address=addresses["cpu1_token"],
    )

    code.label("failure")
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                movz_64(8, LINUX_AARCH64_SYSCALL_EXIT),
                svc_0(),
                branch_to_self(),
            )
        )
    )
    return code.build()


def build_smp_progress_init_elf() -> bytes:
    placeholder_addresses = {
        name: ELF_BASE_VADDR for name, _data in smp_progress_init_data(0)
    }
    code_size = len(build_smp_progress_init_code(placeholder_addresses))
    addresses = smp_progress_init_addresses(code_size)
    code = build_smp_progress_init_code(addresses)
    if len(code) != code_size:
        raise RuntimeError("guest SMP progress init code size changed after address assignment")
    if addresses["child_stack"] % 16 != 0:
        raise RuntimeError("guest SMP progress child stack is not 16-byte aligned")

    data = b"".join(data for _name, data in smp_progress_init_data(code_size))
    return build_guest_elf(code, data)


def smp_hotplug_init_data(code_size: int) -> list[tuple[str, bytes]]:
    data = [
        ("cpu0_affinity_mask", struct.pack("<Q", 1 << 0)),
        ("cpu1_affinity_mask", struct.pack("<Q", 1 << 1)),
        ("cpu0_observed", bytes(4)),
        ("cpu1_observed", bytes(4)),
        ("child_ready", bytes(4)),
        ("child_quiesced", bytes(4)),
        ("start", bytes(4)),
        ("cpu1_progress", bytes(4)),
        ("offline_baseline", bytes(4)),
        ("sysfs", SYS_FS_NAME),
        ("sys", SYS_PATH),
        ("cpu1_online", CPU1_ONLINE_PATH),
        ("offline_value", b"0"),
        ("online_value", b"1"),
        ("ready_marker", SMP_HOTPLUG_READY_MARKER),
        ("off_marker", SMP_HOTPLUG_OFF_MARKER),
        ("done_marker", SMP_HOTPLUG_DONE_MARKER),
    ]
    stack_offset = ELF_CODE_OFFSET + code_size + sum(len(value) for _name, value in data)
    data.append(("stack_padding", bytes((-stack_offset) % 16)))
    data.append(("child_stack", bytes(SMP_HOTPLUG_CHILD_STACK_SIZE)))
    return data


def smp_hotplug_init_addresses(code_size: int) -> dict[str, int]:
    addresses: dict[str, int] = {}
    data_offset = ELF_CODE_OFFSET + code_size
    for name, data in smp_hotplug_init_data(code_size):
        addresses[name] = ELF_BASE_VADDR + data_offset
        data_offset += len(data)
    return addresses


def emit_cpu1_online_write(
    code: Aarch64CodeBuilder,
    addresses: dict[str, int],
    value_name: str,
) -> None:
    code.emit(
        b"".join(
            (
                mov_imm_64(0, AT_FDCWD_U64),
                mov_imm_64(1, addresses["cpu1_online"]),
                movz_64(2, LINUX_OPEN_FLAG_WRONLY),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_OPENAT),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_MI)
    code.emit(
        b"".join(
            (
                mov_reg_64(6, 0),
                mov_imm_64(1, addresses[value_name]),
                movz_64(2, 1),
                movz_64(8, LINUX_AARCH64_SYSCALL_WRITE),
                svc_0(),
                cmp_imm_64(0, 1),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_reg_64(0, 6),
                movz_64(8, LINUX_AARCH64_SYSCALL_CLOSE),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)


def build_smp_hotplug_init_code(addresses: dict[str, int]) -> bytes:
    code = Aarch64CodeBuilder()
    emit_smp_progress_affinity_check(
        code,
        addresses,
        affinity_mask="cpu0_affinity_mask",
        observed_cpu="cpu0_observed",
        expected_cpu=0,
    )
    code.emit(
        b"".join(
            (
                mov_imm_64(0, addresses["sysfs"]),
                mov_imm_64(1, addresses["sys"]),
                mov_imm_64(2, addresses["sysfs"]),
                movz_64(3, 0),
                movz_64(4, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_MOUNT),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                mov_imm_64(0, LINUX_CLONE_VM | LINUX_SIGCHLD),
                mov_imm_64(
                    1,
                    addresses["child_stack"] + SMP_HOTPLUG_CHILD_STACK_SIZE,
                ),
                movz_64(2, 0),
                movz_64(3, 0),
                movz_64(4, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_CLONE),
                svc_0(),
                cmp_imm_64(0, 0),
            )
        )
    )
    code.branch_cond("failure", AARCH64_COND_MI)
    code.branch_cond("child", AARCH64_COND_EQ)

    code.label("parent_wait_ready")
    code.emit(mov_imm_64(1, addresses["child_ready"]))
    code.emit(ldar_u32(0, 1))
    code.emit(cmp_imm_64(0, 1))
    code.branch_cond("parent_wait_ready", AARCH64_COND_NE)
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                mov_imm_64(1, addresses["start"]),
                stlr_u32(0, 1),
            )
        )
    )
    code.label("parent_wait_baseline_progress")
    code.emit(mov_imm_64(1, addresses["cpu1_progress"]))
    code.emit(ldar_u32(0, 1))
    code.emit(cmp_imm_64(0, 0))
    code.branch_cond("parent_wait_baseline_progress", AARCH64_COND_EQ)
    code.emit(
        write_syscalls(
            1,
            addresses["ready_marker"],
            len(SMP_HOTPLUG_READY_MARKER),
        )
    )

    emit_cpu1_online_write(code, addresses, "offline_value")
    code.emit(
        b"".join(
            (
                movz_64(0, 2),
                mov_imm_64(1, addresses["start"]),
                stlr_u32(0, 1),
            )
        )
    )
    code.label("parent_wait_child_quiesced")
    code.emit(mov_imm_64(1, addresses["child_quiesced"]))
    code.emit(ldar_u32(0, 1))
    code.emit(cmp_imm_64(0, 1))
    code.branch_cond("parent_wait_child_quiesced", AARCH64_COND_NE)
    code.emit(mov_imm_64(1, addresses["cpu1_progress"]))
    code.emit(ldar_u32(0, 1))
    code.emit(mov_imm_64(1, addresses["offline_baseline"]))
    code.emit(stlr_u32(0, 1))
    code.emit(
        write_syscalls(
            1,
            addresses["off_marker"],
            len(SMP_HOTPLUG_OFF_MARKER),
        )
    )

    code.emit(movz_64(5, SMP_HOTPLUG_QUIESCENCE_ITERATIONS))
    code.label("offline_quiescence_work")
    code.emit(
        b"".join(
            (
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_YIELD),
                svc_0(),
                sub_imm_64(5, 5, 1),
                cmp_imm_64(5, 0),
            )
        )
    )
    code.branch_cond("offline_quiescence_work", AARCH64_COND_NE)
    code.emit(mov_imm_64(1, addresses["cpu1_progress"]))
    code.emit(ldar_u32(0, 1))
    code.emit(mov_imm_64(1, addresses["offline_baseline"]))
    code.emit(ldar_u32(2, 1))
    code.emit(cmp_reg_32(0, 2))
    code.branch_cond("failure", AARCH64_COND_NE)

    emit_cpu1_online_write(code, addresses, "online_value")
    code.emit(
        b"".join(
            (
                movz_64(0, 3),
                mov_imm_64(1, addresses["start"]),
                stlr_u32(0, 1),
            )
        )
    )
    code.label("parent_wait_reentry")
    code.emit(mov_imm_64(1, addresses["cpu1_progress"]))
    code.emit(ldar_u32(0, 1))
    code.emit(mov_imm_64(1, addresses["offline_baseline"]))
    code.emit(ldar_u32(2, 1))
    code.emit(cmp_reg_32(0, 2))
    code.branch_cond("parent_wait_reentry", AARCH64_COND_EQ)
    code.emit(
        write_syscalls(
            1,
            addresses["done_marker"],
            len(SMP_HOTPLUG_DONE_MARKER),
        )
    )
    code.emit(
        b"".join(
            (
                mov_imm_64(0, LINUX_REBOOT_MAGIC1),
                mov_imm_64(1, LINUX_REBOOT_MAGIC2),
                mov_imm_64(2, LINUX_REBOOT_CMD_POWER_OFF),
                movz_64(3, 0),
                movz_64(8, LINUX_AARCH64_SYSCALL_REBOOT),
                svc_0(),
                branch_to_self(),
            )
        )
    )

    code.label("child")
    emit_smp_progress_affinity_check(
        code,
        addresses,
        affinity_mask="cpu1_affinity_mask",
        observed_cpu="cpu1_observed",
        expected_cpu=1,
    )
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                mov_imm_64(1, addresses["child_ready"]),
                stlr_u32(0, 1),
            )
        )
    )
    code.label("child_dispatch")
    code.emit(mov_imm_64(1, addresses["start"]))
    code.emit(ldar_u32(0, 1))
    code.emit(cmp_imm_64(0, 1))
    code.branch_cond("child_progress", AARCH64_COND_EQ)
    code.emit(cmp_imm_64(0, 2))
    code.branch_cond("child_quiesce", AARCH64_COND_EQ)
    code.emit(cmp_imm_64(0, 3))
    code.branch_cond("child_reenter", AARCH64_COND_EQ)
    code.emit(
        b"".join(
            (
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_YIELD),
                svc_0(),
            )
        )
    )
    code.branch("child_dispatch")

    code.label("child_progress")
    code.emit(mov_imm_64(1, addresses["cpu1_progress"]))
    code.emit(ldar_u32(0, 1))
    code.emit(add_imm_32(0, 0, 1))
    code.emit(stlr_u32(0, 1))
    code.emit(
        b"".join(
            (
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_YIELD),
                svc_0(),
            )
        )
    )
    code.branch("child_dispatch")

    code.label("child_quiesce")
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                mov_imm_64(1, addresses["child_quiesced"]),
                stlr_u32(0, 1),
                movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_YIELD),
                svc_0(),
            )
        )
    )
    code.branch("child_dispatch")

    code.label("child_reenter")
    emit_smp_progress_affinity_check(
        code,
        addresses,
        affinity_mask="cpu1_affinity_mask",
        observed_cpu="cpu1_observed",
        expected_cpu=1,
    )
    code.emit(
        b"".join(
            (
                movz_64(0, 4),
                mov_imm_64(1, addresses["start"]),
                stlr_u32(0, 1),
            )
        )
    )
    code.branch("child_progress")

    code.label("failure")
    code.emit(
        b"".join(
            (
                movz_64(0, 1),
                movz_64(8, LINUX_AARCH64_SYSCALL_EXIT),
                svc_0(),
                branch_to_self(),
            )
        )
    )
    return code.build()


def build_smp_hotplug_init_elf() -> bytes:
    placeholder_addresses = {
        name: ELF_BASE_VADDR for name, _data in smp_hotplug_init_data(0)
    }
    code_size = len(build_smp_hotplug_init_code(placeholder_addresses))
    addresses = smp_hotplug_init_addresses(code_size)
    code = build_smp_hotplug_init_code(addresses)
    if len(code) != code_size:
        raise RuntimeError("guest SMP hotplug init code size changed after address assignment")
    if addresses["child_stack"] % 16 != 0:
        raise RuntimeError("guest SMP hotplug child stack is not 16-byte aligned")

    data = b"".join(data for _name, data in smp_hotplug_init_data(code_size))
    return build_guest_elf(code, data)


def build_reboot_syscall_init_elf(command: int) -> bytes:
    code = b"".join(
        (
            mov_imm_64(0, LINUX_REBOOT_MAGIC1),
            mov_imm_64(1, LINUX_REBOOT_MAGIC2),
            mov_imm_64(2, command),
            movz_64(3, 0),
            movz_64(8, LINUX_AARCH64_SYSCALL_REBOOT),
            svc_0(),
            branch_to_self(),
        )
    )
    return build_guest_elf(code, b"")


def build_poweroff_init_elf() -> bytes:
    return build_reboot_syscall_init_elf(LINUX_REBOOT_CMD_POWER_OFF)


def build_reboot_init_elf() -> bytes:
    return build_reboot_syscall_init_elf(LINUX_REBOOT_CMD_RESTART)


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
    pci_rng_init = build_pci_rng_init_elf()
    smp_init = build_smp_init_elf()
    smp_progress_init = build_smp_progress_init_elf()
    smp_hotplug_init = build_smp_hotplug_init_elf()
    poweroff_init = build_poweroff_init_elf()
    reboot_init = build_reboot_init_elf()
    archive = b"".join(
        (
            cpio_entry(name="dev", ino=1, mode=S_IFDIR | 0o755, nlink=2),
            cpio_entry(name="proc", ino=2, mode=S_IFDIR | 0o755, nlink=2),
            cpio_entry(name="mnt", ino=3, mode=S_IFDIR | 0o755, nlink=2),
            cpio_entry(name="sys", ino=4, mode=S_IFDIR | 0o755, nlink=2),
            cpio_entry(
                name="dev/console",
                ino=5,
                mode=S_IFCHR | 0o600,
                rdevmajor=5,
                rdevminor=1,
            ),
            cpio_entry(name="init", ino=6, mode=S_IFREG | 0o755, data=guest_init),
            cpio_entry(
                name="poweroff-init",
                ino=7,
                mode=S_IFREG | 0o755,
                data=poweroff_init,
            ),
            cpio_entry(
                name="reboot-init",
                ino=8,
                mode=S_IFREG | 0o755,
                data=reboot_init,
            ),
            cpio_entry(
                name="smp-init",
                ino=9,
                mode=S_IFREG | 0o755,
                data=smp_init,
            ),
            cpio_entry(
                name="smp-progress-init",
                ino=10,
                mode=S_IFREG | 0o755,
                data=smp_progress_init,
            ),
            cpio_entry(
                name="smp-hotplug-init",
                ino=11,
                mode=S_IFREG | 0o755,
                data=smp_hotplug_init,
            ),
            cpio_entry(
                name="pci-rng-init",
                ino=12,
                mode=S_IFREG | 0o755,
                data=pci_rng_init,
            ),
            cpio_entry(name="TRAILER!!!", ino=13, mode=0, nlink=1),
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


def validate_reboot_init_entry(
    entries: dict[str, dict[str, object]],
    *,
    name: str,
    command: int,
    command_description: str,
) -> None:
    entry = required_entry(entries, name)
    if file_type(entry["mode"]) != S_IFREG:
        raise RuntimeError(f"guest initrd {name} entry is not a regular file")
    payload = bytes(entry["payload"])
    if not payload.startswith(b"\x7fELF"):
        raise RuntimeError(f"guest initrd {name} payload is not an ELF file")
    if mov_imm_64(0, LINUX_REBOOT_MAGIC1) not in payload:
        raise RuntimeError(f"guest initrd {name} payload does not load reboot magic1")
    if mov_imm_64(1, LINUX_REBOOT_MAGIC2) not in payload:
        raise RuntimeError(f"guest initrd {name} payload does not load reboot magic2")
    if mov_imm_64(2, command) not in payload:
        raise RuntimeError(
            f"guest initrd {name} payload does not load {command_description} command"
        )
    if movz_64(8, LINUX_AARCH64_SYSCALL_REBOOT) not in payload:
        raise RuntimeError(f"guest initrd {name} payload does not load reboot syscall")
    if svc_0() not in payload:
        raise RuntimeError(f"guest initrd {name} payload does not contain SVC #0")


def validate_smp_init_entry(entries: dict[str, dict[str, object]]) -> None:
    entry = required_entry(entries, "smp-init")
    if file_type(entry["mode"]) != S_IFREG:
        raise RuntimeError("guest initrd smp-init entry is not a regular file")
    payload = bytes(entry["payload"])
    if not payload.startswith(b"\x7fELF"):
        raise RuntimeError("guest initrd smp-init payload is not an ELF file")
    if SMP_SECONDARY_MARKER not in payload:
        raise RuntimeError("guest initrd smp-init payload does not contain the CPU1 marker")
    if struct.pack("<Q", 1 << 1) not in payload:
        raise RuntimeError("guest initrd smp-init payload does not contain the CPU1 affinity mask")
    if movz_64(8, LINUX_AARCH64_SYSCALL_SCHED_SETAFFINITY) not in payload:
        raise RuntimeError("guest initrd smp-init payload does not load sched_setaffinity")
    if movz_64(8, LINUX_AARCH64_SYSCALL_GETCPU) not in payload:
        raise RuntimeError("guest initrd smp-init payload does not load getcpu")
    if ldr_u32(0, 1) not in payload or cmp_imm_64(0, 1) not in payload:
        raise RuntimeError("guest initrd smp-init payload does not verify observed CPU1")
    if svc_0() not in payload:
        raise RuntimeError("guest initrd smp-init payload does not contain SVC #0")


def validate_smp_progress_init_entry(entries: dict[str, dict[str, object]]) -> None:
    entry = required_entry(entries, "smp-progress-init")
    if file_type(entry["mode"]) != S_IFREG:
        raise RuntimeError("guest initrd smp-progress-init entry is not a regular file")
    payload = bytes(entry["payload"])
    if not payload.startswith(b"\x7fELF"):
        raise RuntimeError("guest initrd smp-progress-init payload is not an ELF file")
    if len(payload) < SMP_PROGRESS_CHILD_STACK_SIZE:
        raise RuntimeError("guest initrd smp-progress-init payload omits the child stack")
    if (
        SMP_PROGRESS_READY_MARKER
        + SMP_PROGRESS_CPU0_TOKEN
        + SMP_PROGRESS_CPU1_TOKEN
        not in payload
    ):
        raise RuntimeError(
            "guest initrd smp-progress-init payload omits the ready marker or progress tokens"
        )
    affinity_masks = struct.pack("<Q", 1 << 0) + struct.pack("<Q", 1 << 1)
    if affinity_masks not in payload:
        raise RuntimeError(
            "guest initrd smp-progress-init payload omits ordered CPU0/CPU1 affinity masks"
        )
    for syscall, description in (
        (LINUX_AARCH64_SYSCALL_CLONE, "clone"),
        (LINUX_AARCH64_SYSCALL_SCHED_SETAFFINITY, "sched_setaffinity"),
        (LINUX_AARCH64_SYSCALL_SCHED_YIELD, "sched_yield"),
        (LINUX_AARCH64_SYSCALL_GETCPU, "getcpu"),
    ):
        if movz_64(8, syscall) not in payload:
            raise RuntimeError(
                f"guest initrd smp-progress-init payload does not load {description}"
            )
    if mov_imm_64(0, LINUX_CLONE_VM | LINUX_SIGCHLD) not in payload:
        raise RuntimeError("guest initrd smp-progress-init payload omits clone flags")
    if ldar_u32(0, 1) not in payload or stlr_u32(0, 1) not in payload:
        raise RuntimeError(
            "guest initrd smp-progress-init payload omits acquire/release coordination"
        )
    if cmp_imm_64(0, 0) not in payload or cmp_imm_64(0, 1) not in payload:
        raise RuntimeError(
            "guest initrd smp-progress-init payload does not verify CPU0 and CPU1"
        )
    if svc_0() not in payload:
        raise RuntimeError("guest initrd smp-progress-init payload does not contain SVC #0")


def validate_smp_hotplug_init_entry(entries: dict[str, dict[str, object]]) -> None:
    entry = required_entry(entries, "smp-hotplug-init")
    if file_type(entry["mode"]) != S_IFREG:
        raise RuntimeError("guest initrd smp-hotplug-init entry is not a regular file")
    payload = bytes(entry["payload"])
    if not payload.startswith(b"\x7fELF"):
        raise RuntimeError("guest initrd smp-hotplug-init payload is not an ELF file")
    if len(payload) < SMP_HOTPLUG_CHILD_STACK_SIZE:
        raise RuntimeError("guest initrd smp-hotplug-init payload omits the child stack")
    if (
        SMP_HOTPLUG_READY_MARKER
        + SMP_HOTPLUG_OFF_MARKER
        + SMP_HOTPLUG_DONE_MARKER
        not in payload
    ):
        raise RuntimeError("guest initrd smp-hotplug-init payload omits phase markers")
    for guest_path in (SYS_FS_NAME, SYS_PATH, CPU1_ONLINE_PATH):
        if guest_path not in payload:
            raise RuntimeError(
                f"guest initrd smp-hotplug-init payload omits {guest_path!r}"
            )
    if b"01" not in payload:
        raise RuntimeError("guest initrd smp-hotplug-init payload omits online values")
    affinity_masks = struct.pack("<Q", 1 << 0) + struct.pack("<Q", 1 << 1)
    if affinity_masks not in payload:
        raise RuntimeError(
            "guest initrd smp-hotplug-init payload omits ordered CPU affinity masks"
        )
    for syscall, description in (
        (LINUX_AARCH64_SYSCALL_MOUNT, "mount"),
        (LINUX_AARCH64_SYSCALL_OPENAT, "openat"),
        (LINUX_AARCH64_SYSCALL_WRITE, "write"),
        (LINUX_AARCH64_SYSCALL_CLOSE, "close"),
        (LINUX_AARCH64_SYSCALL_CLONE, "clone"),
        (LINUX_AARCH64_SYSCALL_SCHED_SETAFFINITY, "sched_setaffinity"),
        (LINUX_AARCH64_SYSCALL_SCHED_YIELD, "sched_yield"),
        (LINUX_AARCH64_SYSCALL_GETCPU, "getcpu"),
        (LINUX_AARCH64_SYSCALL_REBOOT, "reboot"),
    ):
        if movz_64(8, syscall) not in payload:
            raise RuntimeError(
                f"guest initrd smp-hotplug-init payload does not load {description}"
            )
    for instruction, description in (
        (ldar_u32(0, 1), "acquire load"),
        (stlr_u32(0, 1), "release store"),
        (add_imm_32(0, 0, 1), "progress increment"),
        (sub_imm_64(5, 5, 1), "quiescence decrement"),
        (cmp_reg_32(0, 2), "shared progress comparison"),
    ):
        if instruction not in payload:
            raise RuntimeError(
                f"guest initrd smp-hotplug-init payload omits {description}"
            )
    if svc_0() not in payload:
        raise RuntimeError("guest initrd smp-hotplug-init payload does not contain SVC #0")


def validate_initrd(data: bytes) -> None:
    if not data:
        raise RuntimeError("guest initrd is empty")
    if len(data) % 512 != 0:
        raise RuntimeError("guest initrd is not padded to a 512-byte boundary")

    parsed = parse_newc_entries(data)
    entries = {str(entry["name"]): entry for entry in parsed}
    names = [str(entry["name"]) for entry in parsed]
    expected_names = [
        "dev",
        "proc",
        "mnt",
        "sys",
        "dev/console",
        "init",
        "poweroff-init",
        "reboot-init",
        "smp-init",
        "smp-progress-init",
        "smp-hotplug-init",
        "pci-rng-init",
        CPIO_TRAILER,
    ]
    if names != expected_names:
        raise RuntimeError(f"guest initrd entries {names!r} do not match {expected_names!r}")

    dev = required_entry(entries, "dev")
    if file_type(dev["mode"]) != S_IFDIR:
        raise RuntimeError("guest initrd dev entry is not a directory")

    proc = required_entry(entries, "proc")
    if file_type(proc["mode"]) != S_IFDIR:
        raise RuntimeError("guest initrd proc entry is not a directory")

    mnt = required_entry(entries, "mnt")
    if file_type(mnt["mode"]) != S_IFDIR:
        raise RuntimeError("guest initrd mnt entry is not a directory")

    sysfs = required_entry(entries, "sys")
    if file_type(sysfs["mode"]) != S_IFDIR:
        raise RuntimeError("guest initrd sys entry is not a directory")

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
    for marker in (
        CMDLINE_BEGIN_MARKER,
        CMDLINE_END_MARKER,
        BLOCK_WRITE_MARKER,
        ROOTFS_READ_MARKER,
    ):
        if marker not in payload:
            raise RuntimeError(
                f"guest initrd init payload does not contain {marker!r}"
            )
    if block_write_sector() not in payload:
        raise RuntimeError("guest initrd init payload does not contain the write sector")
    for guest_path in (
        DEV_TMPFS_NAME,
        DEV_PATH,
        MNT_PATH,
        PROC_FS_NAME,
        PROC_PATH,
        PROC_CMDLINE_PATH,
        SQUASHFS_NAME,
        VDA_PATH,
        ROOTFS_OS_RELEASE_PATH,
    ):
        if guest_path not in payload:
            raise RuntimeError(
                f"guest initrd init payload does not contain {guest_path!r}"
            )

    pci_rng_init = required_entry(entries, "pci-rng-init")
    if file_type(pci_rng_init["mode"]) != S_IFREG:
        raise RuntimeError("guest initrd pci-rng-init entry is not a regular file")
    pci_rng_payload = bytes(pci_rng_init["payload"])
    if not pci_rng_payload.startswith(b"\x7fELF"):
        raise RuntimeError("guest initrd pci-rng-init payload is not an ELF file")
    for marker in (
        VIRTIO_PCI_RNG_BOUND_MARKER,
        VIRTIO_PCI_RNG_IO_MARKER,
        VIRTIO_PCI_RNG_IRQ_BEFORE_BEGIN,
        VIRTIO_PCI_RNG_IRQ_BEFORE_END,
        VIRTIO_PCI_RNG_IRQ_AFTER_BEGIN,
        VIRTIO_PCI_RNG_IRQ_AFTER_END,
        VIRTIO_PCI_RNG_SUCCESS_MARKER,
        VIRTIO_PCI_RNG_FAILURE_MARKER,
    ):
        if marker not in pci_rng_payload:
            raise RuntimeError(
                f"guest initrd pci-rng-init payload does not contain {marker!r}"
            )
    for guest_path in (
        DEV_TMPFS_NAME,
        DEV_PATH,
        PROC_FS_NAME,
        PROC_PATH,
        SYS_FS_NAME,
        SYS_PATH,
        VIRTIO_PCI_RNG_CURRENT_PATH,
        VIRTIO_PCI_RNG_DEVICE_PATH,
        PROC_INTERRUPTS_PATH,
    ):
        if guest_path not in pci_rng_payload:
            raise RuntimeError(
                f"guest initrd pci-rng-init payload does not contain {guest_path!r}"
            )
    if bytes([VIRTIO_PCI_RNG_ENTROPY_BYTE]) * VIRTIO_PCI_RNG_READ_SIZE in pci_rng_payload:
        raise RuntimeError(
            "guest initrd pci-rng-init must validate host-provided bytes instead of embedding them"
        )

    validate_reboot_init_entry(
        entries,
        name="poweroff-init",
        command=LINUX_REBOOT_CMD_POWER_OFF,
        command_description="poweroff",
    )
    validate_reboot_init_entry(
        entries,
        name="reboot-init",
        command=LINUX_REBOOT_CMD_RESTART,
        command_description="restart",
    )
    validate_smp_init_entry(entries)
    validate_smp_progress_init_entry(entries)
    validate_smp_hotplug_init_entry(entries)


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
