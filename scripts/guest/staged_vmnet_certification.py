#!/usr/bin/env python3
"""Closed guest-side barriers for staged elevated-vmnet certification.

The coordinator owns no DHCP or traffic semantics.  It observes only the
presence of one non-loopback interface, exchanges authenticated fixed-sector
barriers on /dev/vdc, and invokes the checked one-shot v111 DHCP/TCP oracle for
each generation that must prove live traffic.
"""

from __future__ import annotations

import hashlib
import os
import re
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from enum import IntEnum
from pathlib import Path
from typing import Callable, NoReturn, Optional, Protocol


CONTROL_PATH = Path("/dev/vdc")
ONE_SHOT_ORACLE = Path("/bangbang-elevated-vmnet-certification")
NETWORK_DIRECTORY = Path("/sys/class/net")
PCI_RESCAN_PATH = Path("/sys/bus/pci/rescan")
PCI_DEVICES_ROOT = Path("/sys/devices")
HEADER_MAGIC = b"BBSTGVM1"
RECORD_MAGIC = b"BBSTREC1"
VERSION = 1
SECTOR_BYTES = 512
CONTROL_BYTES = 4096
PREFIX_BYTES = 64
DIGEST_BYTES = 32
HEADER_OFFSET = 0
COMMAND_OFFSET = SECTOR_BYTES
STATUS_OFFSET = SECTOR_BYTES * 2
ROLE_COMMAND = 1
ROLE_STATUS = 2
COMMAND_PROCEED = 1
FAILURE_KINDS = {
    "control": 240,
    "io": 241,
    "topology": 242,
    "timeout": 243,
    "process": 244,
    "traffic": 245,
    "internal": 246,
}
FAILURE_CATEGORIES = {kind: category for category, kind in FAILURE_KINDS.items()}
INTERFACE_TIMEOUT_SECONDS = 60.0
COMMAND_TIMEOUT_SECONDS = 120.0
TRAFFIC_TIMEOUT_SECONDS = 90.0
POLL_SECONDS = 0.02
FIXED_ENVIRONMENT = {"LANG": "C", "LC_ALL": "C", "PATH": "/usr/sbin:/usr/bin:/sbin:/bin"}
INTERFACE_NAME = re.compile(r"[A-Za-z0-9_.-]{1,15}\Z")


class Scenario(IntEnum):
    STARTUP = 1
    RUNTIME = 2
    RESTORE = 3

    @property
    def cycles(self) -> int:
        return 1 if self is Scenario.RUNTIME else 2


class Status(IntEnum):
    INITIAL_PRESENT = 1
    INITIAL_ABSENT = 2
    PRESENT = 3
    TRAFFIC_ONE = 4
    ABSENT = 5
    TRAFFIC_TWO = 6
    CAPTURE_READY = 7
    COMPLETE = 8
    FAILED = 9

    @property
    def label(self) -> str:
        return self.name.casefold().replace("_", "-")


STATUS_GRAPHS = {
    Scenario.STARTUP: (
        Status.INITIAL_PRESENT,
        Status.TRAFFIC_ONE,
        Status.ABSENT,
        Status.PRESENT,
        Status.TRAFFIC_TWO,
        Status.ABSENT,
        Status.COMPLETE,
    ),
    Scenario.RUNTIME: (
        Status.INITIAL_ABSENT,
        Status.PRESENT,
        Status.TRAFFIC_ONE,
        Status.ABSENT,
        Status.COMPLETE,
    ),
    Scenario.RESTORE: (
        Status.INITIAL_PRESENT,
        Status.CAPTURE_READY,
        Status.PRESENT,
        Status.TRAFFIC_TWO,
        Status.ABSENT,
        Status.COMPLETE,
    ),
}
COMMAND_COUNTS = {
    Scenario.STARTUP: 5,
    Scenario.RUNTIME: 3,
    Scenario.RESTORE: 4,
}


class CoordinatorError(RuntimeError):
    """One value-free staged guest failure."""

    def __init__(self, category: str) -> None:
        if not isinstance(category, str) or re.fullmatch(r"[a-z][a-z-]{0,31}", category) is None:
            category = "internal"
        super().__init__(category)
        self.category = category


@dataclass(frozen=True)
class Header:
    scenario: Scenario
    cycles: int
    nonce: bytes


@dataclass(frozen=True)
class Record:
    role: int
    scenario: Scenario
    kind: int
    sequence: int
    nonce: bytes


def _digest(prefix: bytes) -> bytes:
    if len(prefix) != PREFIX_BYTES:
        raise CoordinatorError("control")
    return hashlib.sha256(prefix).digest()


def encode_header(scenario: Scenario, nonce: bytes) -> bytes:
    if not isinstance(scenario, Scenario) or not isinstance(nonce, bytes) or len(nonce) != 32 or not any(nonce):
        raise CoordinatorError("control")
    value = bytearray(SECTOR_BYTES)
    value[:8] = HEADER_MAGIC
    value[8:10] = VERSION.to_bytes(2, "big")
    value[10] = scenario
    value[11] = scenario.cycles
    value[16:48] = nonce
    value[PREFIX_BYTES : PREFIX_BYTES + DIGEST_BYTES] = _digest(bytes(value[:PREFIX_BYTES]))
    return bytes(value)


def decode_header(value: bytes) -> Header:
    if not isinstance(value, bytes) or len(value) != SECTOR_BYTES:
        raise CoordinatorError("control")
    try:
        scenario = Scenario(value[10])
    except (IndexError, ValueError) as error:
        raise CoordinatorError("control") from error
    nonce = value[16:48]
    if (
        value[:8] != HEADER_MAGIC
        or int.from_bytes(value[8:10], "big") != VERSION
        or value[11] != scenario.cycles
        or any(value[12:16])
        or len(nonce) != 32
        or not any(nonce)
        or any(value[48:PREFIX_BYTES])
        or value[PREFIX_BYTES : PREFIX_BYTES + DIGEST_BYTES] != _digest(value[:PREFIX_BYTES])
        or any(value[PREFIX_BYTES + DIGEST_BYTES :])
    ):
        raise CoordinatorError("control")
    return Header(scenario, scenario.cycles, nonce)


def decode_initial_control(value: bytes) -> Header:
    if not isinstance(value, bytes) or len(value) != CONTROL_BYTES or any(value[SECTOR_BYTES:]):
        raise CoordinatorError("control")
    return decode_header(value[:SECTOR_BYTES])


def encode_record(
    role: int,
    scenario: Scenario,
    kind: int,
    sequence: int,
    nonce: bytes,
) -> bytes:
    if (
        role not in (ROLE_COMMAND, ROLE_STATUS)
        or not isinstance(scenario, Scenario)
        or isinstance(kind, bool)
        or not isinstance(kind, int)
        or not 1 <= kind <= 255
        or isinstance(sequence, bool)
        or not isinstance(sequence, int)
        or not 1 <= sequence <= 0xFFFF_FFFF_FFFF_FFFF
        or not isinstance(nonce, bytes)
        or len(nonce) != 32
        or not any(nonce)
    ):
        raise CoordinatorError("control")
    value = bytearray(SECTOR_BYTES)
    value[:8] = RECORD_MAGIC
    value[8:10] = VERSION.to_bytes(2, "big")
    value[10] = role
    value[11] = scenario
    value[12] = kind
    value[16:24] = sequence.to_bytes(8, "big")
    value[24:56] = nonce
    value[PREFIX_BYTES : PREFIX_BYTES + DIGEST_BYTES] = _digest(bytes(value[:PREFIX_BYTES]))
    return bytes(value)


def decode_record(value: bytes, *, allow_empty: bool = False) -> Optional[Record]:
    if not isinstance(value, bytes) or len(value) != SECTOR_BYTES:
        raise CoordinatorError("control")
    if not any(value):
        if allow_empty:
            return None
        raise CoordinatorError("control")
    try:
        scenario = Scenario(value[11])
    except (IndexError, ValueError) as error:
        raise CoordinatorError("control") from error
    role = value[10]
    kind = value[12]
    sequence = int.from_bytes(value[16:24], "big")
    nonce = value[24:56]
    if (
        value[:8] != RECORD_MAGIC
        or int.from_bytes(value[8:10], "big") != VERSION
        or role not in (ROLE_COMMAND, ROLE_STATUS)
        or kind == 0
        or sequence == 0
        or len(nonce) != 32
        or not any(nonce)
        or any(value[13:16])
        or any(value[56:PREFIX_BYTES])
        or value[PREFIX_BYTES : PREFIX_BYTES + DIGEST_BYTES] != _digest(value[:PREFIX_BYTES])
        or any(value[PREFIX_BYTES + DIGEST_BYTES :])
    ):
        raise CoordinatorError("control")
    return Record(role, scenario, kind, sequence, nonce)


class Barrier(Protocol):
    def status(self, sequence: int, kind: Status) -> None: ...

    def proceed(self, sequence: int) -> None: ...


class BlockBarrier:
    def __init__(self, device: int, header: Header) -> None:
        self.device = device
        self.header = header
        self._previous_command: Optional[Record] = None
        self._previous_status_sequence = 0
        self._terminal = False
        self._closed = False

    @classmethod
    def open(cls, path: Path = CONTROL_PATH) -> BlockBarrier:
        flags = os.O_RDWR | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = -1
        try:
            descriptor = os.open(path, flags)
            metadata = os.fstat(descriptor)
            control_bytes = os.pread(descriptor, CONTROL_BYTES + 1, HEADER_OFFSET)
        except OSError as error:
            try:
                os.close(descriptor)
            except (OSError, UnboundLocalError):
                pass
            raise CoordinatorError("io") from error
        if not stat.S_ISBLK(metadata.st_mode):
            os.close(descriptor)
            raise CoordinatorError("control")
        try:
            header = decode_initial_control(control_bytes)
        except BaseException:
            os.close(descriptor)
            raise
        os.close(descriptor)
        return cls(metadata.st_rdev, header)

    def _open(self) -> int:
        if self._closed:
            raise CoordinatorError("io")
        flags = os.O_RDWR | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(CONTROL_PATH, flags)
            metadata = os.fstat(descriptor)
        except OSError as error:
            try:
                os.close(descriptor)
            except (OSError, UnboundLocalError):
                pass
            raise CoordinatorError("io") from error
        if not stat.S_ISBLK(metadata.st_mode) or metadata.st_rdev != self.device:
            os.close(descriptor)
            raise CoordinatorError("control")
        return descriptor

    def close(self) -> None:
        self._closed = True

    def _read_command(self) -> Optional[Record]:
        descriptor = self._open()
        try:
            value = os.pread(descriptor, SECTOR_BYTES, COMMAND_OFFSET)
        except OSError as error:
            raise CoordinatorError("io") from error
        finally:
            os.close(descriptor)
        return decode_record(value, allow_empty=True)

    def status(self, sequence: int, kind: Status) -> None:
        graph = STATUS_GRAPHS[self.header.scenario]
        if (
            self._terminal
            or not isinstance(kind, Status)
            or sequence != self._previous_status_sequence + 1
            or sequence > len(graph)
            or kind is not graph[sequence - 1]
            or (
                kind is Status.COMPLETE
                and (
                    self._previous_command is None
                    or self._previous_command.sequence
                    != COMMAND_COUNTS[self.header.scenario]
                )
            )
        ):
            raise CoordinatorError("control")
        value = encode_record(
            ROLE_STATUS,
            self.header.scenario,
            int(kind),
            sequence,
            self.header.nonce,
        )
        descriptor = self._open()
        try:
            written = os.pwrite(descriptor, value, STATUS_OFFSET)
            os.fsync(descriptor)
        except OSError as error:
            raise CoordinatorError("io") from error
        finally:
            os.close(descriptor)
        if written != len(value):
            raise CoordinatorError("io")
        self._previous_status_sequence = sequence
        self._terminal = kind is Status.COMPLETE
        _emit(f"BANGBANG_STAGED_VMNET_STATUS_{kind.name}")

    def failure(self, category: str) -> None:
        kind = FAILURE_KINDS.get(category, FAILURE_KINDS["internal"])
        value = encode_record(
            ROLE_STATUS,
            self.header.scenario,
            kind,
            0xFFFF_FFFF_FFFF_FFFF,
            self.header.nonce,
        )
        descriptor = self._open()
        try:
            written = os.pwrite(descriptor, value, STATUS_OFFSET)
            os.fsync(descriptor)
        except OSError as error:
            raise CoordinatorError("io") from error
        finally:
            os.close(descriptor)
        if written != len(value):
            raise CoordinatorError("io")

    def proceed(self, sequence: int) -> None:
        expected = 1 if self._previous_command is None else self._previous_command.sequence + 1
        if (
            self._terminal
            or sequence != expected
            or sequence > COMMAND_COUNTS[self.header.scenario]
        ):
            raise CoordinatorError("control")
        deadline = time.monotonic() + COMMAND_TIMEOUT_SECONDS
        while True:
            record = self._read_command()
            if record is not None:
                if record == self._previous_command:
                    pass
                elif (
                    record.role == ROLE_COMMAND
                    and record.scenario is self.header.scenario
                    and record.kind == COMMAND_PROCEED
                    and record.sequence == sequence
                    and record.nonce == self.header.nonce
                ):
                    self._previous_command = record
                    return
                else:
                    raise CoordinatorError("control")
            if time.monotonic() >= deadline:
                raise CoordinatorError("timeout")
            time.sleep(POLL_SECONDS)

    def __enter__(self) -> BlockBarrier:
        return self

    def __exit__(self, *_exception: object) -> None:
        self.close()


def _interface_count(directory: Path = NETWORK_DIRECTORY) -> int:
    try:
        names = sorted(entry.name for entry in os.scandir(directory) if entry.name != "lo")
    except OSError as error:
        raise CoordinatorError("topology") from error
    if any(INTERFACE_NAME.fullmatch(name) is None for name in names) or len(names) > 1:
        raise CoordinatorError("topology")
    return len(names)


def _rescan(path: Path = PCI_RESCAN_PATH) -> None:
    try:
        with path.open("wb", buffering=0) as destination:
            if destination.write(b"1\n") != 2:
                raise CoordinatorError("topology")
    except CoordinatorError:
        raise
    except OSError as error:
        raise CoordinatorError("topology") from error


def _network_pci_remove_path(directory: Path = NETWORK_DIRECTORY) -> Path:
    try:
        interfaces = [entry.name for entry in os.scandir(directory) if entry.name != "lo"]
    except OSError as error:
        raise CoordinatorError("topology") from error
    if len(interfaces) != 1 or INTERFACE_NAME.fullmatch(interfaces[0]) is None:
        raise CoordinatorError("topology")
    try:
        current = (directory / interfaces[0] / "device").resolve(strict=True)
        devices_root = PCI_DEVICES_ROOT.resolve(strict=True)
    except OSError as error:
        raise CoordinatorError("topology") from error
    if current != devices_root and devices_root not in current.parents:
        raise CoordinatorError("topology")
    while current != devices_root:
        remove = current / "remove"
        try:
            vendor = (current / "vendor").read_text(encoding="ascii").strip()
            device = (current / "device").read_text(encoding="ascii").strip()
        except OSError:
            pass
        else:
            if vendor == "0x1af4" and device == "0x1041" and remove.exists():
                return remove
        current = current.parent
    raise CoordinatorError("topology")


def _remove_interface() -> None:
    path = _network_pci_remove_path()
    try:
        with path.open("wb", buffering=0) as destination:
            if destination.write(b"1\n") != 2:
                raise CoordinatorError("topology")
    except CoordinatorError:
        raise
    except OSError as error:
        raise CoordinatorError("topology") from error


def _wait_interface(
    present: bool,
    interface_count: Callable[[], int],
    rescan: Callable[[], None],
    *,
    timeout: float = INTERFACE_TIMEOUT_SECONDS,
) -> None:
    deadline = time.monotonic() + timeout
    expected = 1 if present else 0
    while True:
        observed = interface_count()
        if observed not in (0, 1):
            raise CoordinatorError("topology")
        if observed == expected:
            return
        if present:
            rescan()
        if time.monotonic() >= deadline:
            raise CoordinatorError("timeout")
        time.sleep(POLL_SECONDS)


def _run_traffic(oracle: Path = ONE_SHOT_ORACLE) -> None:
    if not oracle.is_absolute():
        raise CoordinatorError("process")
    try:
        metadata = os.lstat(oracle)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_mode & 0o111 == 0
            or metadata.st_mode & 0o022
        ):
            raise CoordinatorError("process")
        outcome = subprocess.run(
            (os.fspath(oracle),),
            check=False,
            cwd="/",
            env=FIXED_ENVIRONMENT,
            stdin=subprocess.DEVNULL,
            timeout=TRAFFIC_TIMEOUT_SECONDS,
        )
    except CoordinatorError:
        raise
    except (OSError, subprocess.SubprocessError) as error:
        raise CoordinatorError("process") from error
    if outcome.returncode != 0:
        raise CoordinatorError("traffic")


def _emit(value: str) -> None:
    try:
        sys.stdout.write(value + "\n")
        sys.stdout.flush()
    except (OSError, UnicodeError) as error:
        raise CoordinatorError("io") from error


def run_scenario(
    header: Header,
    barrier: Barrier,
    *,
    interface_count: Callable[[], int] = _interface_count,
    rescan: Callable[[], None] = _rescan,
    remove_interface: Callable[[], None] = _remove_interface,
    traffic: Callable[[], None] = _run_traffic,
) -> None:
    if (
        not isinstance(header, Header)
        or not isinstance(header.scenario, Scenario)
        or header.cycles != header.scenario.cycles
        or not isinstance(header.nonce, bytes)
        or len(header.nonce) != 32
        or not any(header.nonce)
    ):
        raise CoordinatorError("control")
    status_sequence = 0

    def publish(kind: Status) -> None:
        nonlocal status_sequence
        status_sequence += 1
        barrier.status(status_sequence, kind)

    scenario = header.scenario
    if scenario is Scenario.STARTUP:
        _wait_interface(True, interface_count, rescan)
        publish(Status.INITIAL_PRESENT)
        barrier.proceed(1)
        traffic()
        publish(Status.TRAFFIC_ONE)
        barrier.proceed(2)
        remove_interface()
        _wait_interface(False, interface_count, rescan)
        publish(Status.ABSENT)
        barrier.proceed(3)
        _wait_interface(True, interface_count, rescan)
        publish(Status.PRESENT)
        traffic()
        publish(Status.TRAFFIC_TWO)
        barrier.proceed(4)
        remove_interface()
        _wait_interface(False, interface_count, rescan)
        publish(Status.ABSENT)
        barrier.proceed(5)
        _wait_interface(False, interface_count, rescan)
        publish(Status.COMPLETE)
        return
    if scenario is Scenario.RUNTIME:
        _wait_interface(False, interface_count, rescan)
        publish(Status.INITIAL_ABSENT)
        barrier.proceed(1)
        _wait_interface(True, interface_count, rescan)
        publish(Status.PRESENT)
        traffic()
        publish(Status.TRAFFIC_ONE)
        barrier.proceed(2)
        remove_interface()
        _wait_interface(False, interface_count, rescan)
        publish(Status.ABSENT)
        barrier.proceed(3)
        _wait_interface(False, interface_count, rescan)
        publish(Status.COMPLETE)
        return
    if scenario is Scenario.RESTORE:
        _wait_interface(True, interface_count, rescan)
        publish(Status.INITIAL_PRESENT)
        barrier.proceed(1)
        traffic()
        publish(Status.CAPTURE_READY)
        barrier.proceed(2)
        _wait_interface(True, interface_count, rescan)
        publish(Status.PRESENT)
        traffic()
        publish(Status.TRAFFIC_TWO)
        barrier.proceed(3)
        remove_interface()
        _wait_interface(False, interface_count, rescan)
        publish(Status.ABSENT)
        barrier.proceed(4)
        _wait_interface(False, interface_count, rescan)
        publish(Status.COMPLETE)
        return
    raise CoordinatorError("control")


def _fail(error: CoordinatorError, barrier: Optional[BlockBarrier]) -> NoReturn:
    if barrier is not None:
        try:
            barrier.failure(error.category)
        except BaseException:
            pass
    try:
        _emit(f"BANGBANG_STAGED_VMNET_FAIL_{error.category.upper().replace('-', '_')}")
    except CoordinatorError:
        pass
    raise SystemExit(1)


def main() -> int:
    barrier: Optional[BlockBarrier] = None
    try:
        _emit("BANGBANG_STAGED_VMNET_BEGIN")
        barrier = BlockBarrier.open()
        run_scenario(barrier.header, barrier)
        _emit("BANGBANG_STAGED_VMNET_OK")
        barrier.close()
        return 0
    except CoordinatorError as error:
        _fail(error, barrier)
    except BaseException as error:
        internal = CoordinatorError("internal")
        internal.__cause__ = error
        _fail(internal, barrier)


if __name__ == "__main__":
    raise SystemExit(main())
