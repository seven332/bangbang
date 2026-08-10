#!/usr/bin/env python3
"""Run foreground and retired-daemon explicit-root guest evidence."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import secrets
import select
import signal
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn


SUCCESS_ORACLE = b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n"
FAILURE_ORACLE = b"BANGBANG_ROOTFS_WORKFLOW_FAIL\r\n"
POWEROFF_SUFFIX = b"] reboot: Power down\r\n"
UNMAPPED_ID = 2_147_483_647
PROCESS_TIMEOUT_SECONDS = 45
BARRIER_TIMEOUT_SECONDS = 10
MAX_OUTPUT_BYTES = 1024 * 1024
MAX_OUTPUT_LINE_BYTES = 4 * 1024
WORKER_SIGHUP_EXIT_CODE = 156
WORKER_NAMESPACE_REPLACEMENT_EXIT_CODE = 80
ADOPTION_BARRIER_OPTION = "--bangbang-internal-post-adoption-stop-v1"
DAEMON_BARRIER_OPTION = "--bangbang-internal-daemon-barrier-v1"
DAEMON_FAULT_OPTION = "--bangbang-internal-daemon-fault-v1"
DAEMON_NAMESPACE_RETIREMENT_BARRIER = "daemon-namespace-retirement"
JAILER_ACTIVATION = "--bangbang-jailer-v1"
REPLACEMENT_BYTES = b"invalid-adoption-replacement\n"
API_SOCKET_CHILD = "evidence-api.sock"
API_DISPLACED_SOCKET_CHILD = ".displaced-evidence-api.sock"
API_SOCKET_REPLACEMENT_BYTES = b"preserved-api-socket-replacement\n"
ROOT_PARENT = Path("/private/var/root")
ROOT_PREFIX = "bangbang-elevated-probe."
SESSION_PREFIX = "session-"
SESSION_ID_HEX_BYTES = 64
PROC_PIDTBSDINFO = 3
PROC_PIDPATHINFO_MAXSIZE = 4096
NOTE_EXITSTATUS = 0x04000000
RESOURCE_NAMES = {
    "config": "evidence-guest-no-api.json",
    "kernel": "evidence-guest-kernel",
    "initrd": "evidence-guest-initrd",
    "rootfs": "evidence-guest-rootfs",
}
GRANT_IDS = {
    "config": "evidence-guest-config",
    "api": "evidence-guest-api",
    "kernel": "evidence-guest-kernel",
    "initrd": "evidence-guest-initrd",
    "rootfs": "evidence-guest-rootfs",
    "logger": "evidence-guest-logger",
    "metrics": "evidence-guest-metrics",
    "serial": "evidence-guest-serial",
}


class MatrixError(RuntimeError):
    """Value-free guest-matrix failure."""


def _fail(message: str) -> NoReturn:
    raise MatrixError(message)


class ProcBsdInfo(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_uint32),
        ("status", ctypes.c_uint32),
        ("xstatus", ctypes.c_uint32),
        ("pid", ctypes.c_uint32),
        ("ppid", ctypes.c_uint32),
        ("uid", ctypes.c_uint32),
        ("gid", ctypes.c_uint32),
        ("ruid", ctypes.c_uint32),
        ("rgid", ctypes.c_uint32),
        ("svuid", ctypes.c_uint32),
        ("svgid", ctypes.c_uint32),
        ("reserved", ctypes.c_uint32),
        ("command", ctypes.c_char * 16),
        ("name", ctypes.c_char * 32),
        ("nfiles", ctypes.c_uint32),
        ("pgid", ctypes.c_uint32),
        ("pjobc", ctypes.c_uint32),
        ("terminal_device", ctypes.c_uint32),
        ("terminal_pgid", ctypes.c_uint32),
        ("nice", ctypes.c_int32),
        ("start_seconds", ctypes.c_uint64),
        ("start_microseconds", ctypes.c_uint64),
    ]


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    parent_pid: int
    process_group: int
    session: int
    uid: int
    gid: int
    real_uid: int
    real_gid: int
    saved_uid: int
    saved_gid: int
    start_seconds: int
    start_microseconds: int
    executable: Path

    def same_start(self, other: "ProcessIdentity") -> bool:
        return (
            self.pid,
            self.start_seconds,
            self.start_microseconds,
            self.executable,
        ) == (
            other.pid,
            other.start_seconds,
            other.start_microseconds,
            other.executable,
        )


@dataclass(frozen=True)
class ModeCase:
    mode: str
    workload: str
    identity: str
    semantics: str


MODE_CASES = (
    ModeCase(
        "guest-no-api-drop",
        "no-api",
        "mapped",
        "stream-eid=snapshot stream-cred=snapshot stream-pid=exact "
        "datagram-cred=unsupported datagram-token=changed datagram-pid=exact",
    ),
    ModeCase(
        "guest-no-api-retain-root",
        "no-api",
        "retained-root",
        "stream-eid=stable-root stream-cred=stable-root stream-pid=exact "
        "datagram-cred=unsupported datagram-token=unchanged datagram-pid=exact",
    ),
    ModeCase(
        "guest-no-api-unmapped",
        "no-api",
        "unmapped",
        "stream-eid=snapshot stream-cred=snapshot stream-pid=exact "
        "datagram-cred=unsupported datagram-token=changed datagram-pid=exact",
    ),
    ModeCase(
        "guest-api-drop",
        "api",
        "mapped",
        "stream-eid=snapshot stream-cred=snapshot stream-pid=exact "
        "datagram-cred=unsupported datagram-token=changed datagram-pid=exact",
    ),
    ModeCase(
        "guest-api-retain-root",
        "api",
        "retained-root",
        "stream-eid=stable-root stream-cred=stable-root stream-pid=exact "
        "datagram-cred=unsupported datagram-token=unchanged datagram-pid=exact",
    ),
    ModeCase(
        "guest-api-unmapped",
        "api",
        "unmapped",
        "stream-eid=snapshot stream-cred=snapshot stream-pid=exact "
        "datagram-cred=unsupported datagram-token=changed datagram-pid=exact",
    ),
)


@dataclass(frozen=True)
class FaultCase:
    fault: str
    stage: str
    result: str
    workload: str
    category: str = "other"


FAULT_CASES = (
    FaultCase("guest-grant-contract", "guest-grant-contract", "grant-boundary", "no-api"),
    FaultCase("grant-transfer", "grant-transfer", "grant-boundary", "no-api"),
    FaultCase(
        "guest-grant-accepted", "grant-accepted", "grant-boundary", "no-api"
    ),
    FaultCase(
        "guest-transport-contamination",
        "guest-resource-witness",
        "grant-boundary",
        "no-api",
        "invalid-input",
    ),
    FaultCase("guest-resource-witness", "guest-resource-witness", "grant-boundary", "no-api"),
    FaultCase("api-listener-request", "api-listener-request", "api-boundary", "api"),
    FaultCase("api-listener-bind", "api-listener-bind", "api-boundary", "api"),
    FaultCase("api-listener-transfer", "api-listener-transfer", "api-boundary", "api"),
    FaultCase("api-listener-adoption", "api-listener-adoption", "api-boundary", "api"),
    FaultCase("api-socket-publication", "api-socket-publication", "api-boundary", "api"),
    FaultCase("api-logger-configuration", "api-logger-configuration", "api-boundary", "api"),
    FaultCase("api-metrics-configuration", "api-metrics-configuration", "api-boundary", "api"),
    FaultCase("api-serial-configuration", "api-serial-configuration", "api-boundary", "api"),
    FaultCase("api-machine-configuration", "api-machine-configuration", "api-boundary", "api"),
    FaultCase("api-boot-configuration", "api-boot-configuration", "api-boundary", "api"),
    FaultCase("api-drive-configuration", "api-drive-configuration", "api-boundary", "api"),
    FaultCase("api-instance-start", "api-instance-start", "api-boundary", "api"),
    FaultCase("no-api-startup", "no-api-startup", "guest-boundary", "no-api"),
    FaultCase("guest-hvf-witness", "guest-hvf-witness", "hvf-boundary", "no-api"),
    FaultCase("guest-hvf-create", "guest-hvf-create", "hvf-boundary", "no-api"),
    FaultCase("guest-execution", "guest-execution", "guest-boundary", "no-api"),
    FaultCase("guest-oracle", "guest-oracle", "guest-boundary", "no-api"),
    FaultCase("guest-poweroff", "guest-poweroff", "guest-boundary", "no-api"),
    FaultCase("guest-timeout", "guest-timeout", "guest-boundary", "no-api"),
    FaultCase(
        "guest-terminal-evidence",
        "guest-terminal-evidence",
        "guest-boundary",
        "no-api",
    ),
    FaultCase("guest-cleanup", "guest-cleanup", "guest-boundary", "no-api"),
    FaultCase("guest-hvf-witness", "guest-hvf-witness", "hvf-boundary", "api"),
    FaultCase("guest-hvf-create", "guest-hvf-create", "hvf-boundary", "api"),
    FaultCase("guest-execution", "guest-execution", "guest-boundary", "api"),
    FaultCase("guest-oracle", "guest-oracle", "guest-boundary", "api"),
    FaultCase("guest-poweroff", "guest-poweroff", "guest-boundary", "api"),
    FaultCase("guest-timeout", "guest-timeout", "guest-boundary", "api"),
    FaultCase(
        "guest-terminal-evidence",
        "guest-terminal-evidence",
        "guest-boundary",
        "api",
    ),
    FaultCase("guest-cleanup", "guest-cleanup", "guest-boundary", "api"),
)

DAEMON_RETIREMENT_FAULTS = (
    "namespace-retire-before-unlink",
    "namespace-retire-after-unlink",
    "namespace-retire-observe",
    "namespace-record-write",
)

MATRIX_SUMMARY = (
    "guest-matrix: api-mapped=complete api-retained-root=complete-no-drop "
    "api-unmapped=complete no-api-mapped=complete "
    "no-api-retained-root=complete-no-drop no-api-unmapped=complete "
    "repeats=three concurrency=api-no-api-complete faults=all-reachable "
    "deaths=no-api-post-worker-first-launcher-first-"
    "api-pre-post-worker-first-launcher-first "
    "tamper=rejected-both-workloads "
    "adoption-replacement=no-api-complete-api-rejected-at-grant "
    "socket-replacement=both-cleanup-owners-preserve "
    "daemon=api-no-api-all-identities-retired "
    "daemon-faults=retirement-all-reachable "
    "daemon-deaths=api-no-api-worker-first-launcher-first "
    "daemon-signals=int-term-hup daemon-replacement=preserved "
    "daemon-concurrency=peer-survives-launcher-kill "
    "cleanup=exact-no-product-session-teardown"
)

API_PREOPENED_REPLACEMENT_BOUNDARY = FaultCase(
    "",
    "grant-accepted",
    "grant-boundary",
    "api",
)


@dataclass(frozen=True)
class ObjectIdentity:
    device: int
    inode: int
    uid: int
    gid: int
    mode: int
    links: int
    size: int

    @classmethod
    def capture(cls, path: Path) -> "ObjectIdentity":
        metadata = path.lstat()
        return cls(
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_uid,
            metadata.st_gid,
            stat.S_IMODE(metadata.st_mode),
            metadata.st_nlink,
            metadata.st_size,
        )


@dataclass(frozen=True)
class ResourceIdentity:
    identity: ObjectIdentity
    sha256: str


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def capture_resources(resources: Path) -> dict[str, ResourceIdentity]:
    result: dict[str, ResourceIdentity] = {}
    for key, name in RESOURCE_NAMES.items():
        path = resources / name
        identity = ObjectIdentity.capture(path)
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or identity.mode != 0o400
            or identity.links != 1
            or identity.size <= 0
        ):
            _fail("immutable-resource-shape")
        result[key] = ResourceIdentity(identity, _sha256(path))
    return result


def verify_resources(resources: Path, expected: dict[str, ResourceIdentity]) -> None:
    if set(expected) != set(RESOURCE_NAMES):
        _fail("immutable-resource-ledger")
    for key, name in RESOURCE_NAMES.items():
        path = resources / name
        if ObjectIdentity.capture(path) != expected[key].identity:
            _fail("immutable-resource-identity")
        if _sha256(path) != expected[key].sha256:
            _fail("immutable-resource-digest")


def _write_all(descriptor: int, contents: bytes) -> None:
    remaining = memoryview(contents)
    while remaining:
        written = os.write(descriptor, remaining)
        if written <= 0:
            _fail("fixture-short-write")
        remaining = remaining[written:]


def _create_file(path: Path, contents: bytes, uid: int, gid: int, mode: int) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, mode)
    try:
        _write_all(descriptor, contents)
        os.fchmod(descriptor, mode)
        os.fchown(descriptor, uid, gid)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _create_runtime_root() -> Path:
    for _ in range(64):
        path = ROOT_PARENT / f"{ROOT_PREFIX}{secrets.token_hex(4)}"
        try:
            path.mkdir(mode=0o700)
        except FileExistsError:
            continue
        return path
    _fail("runtime-root-collision")


def serial_transcript_outcome(contents: bytes) -> str:
    if contents.startswith(SUCCESS_ORACLE):
        outcome = "success"
        tail = contents[len(SUCCESS_ORACLE) :]
    elif contents.startswith(FAILURE_ORACLE):
        outcome = "failure"
        tail = contents[len(FAILURE_ORACLE) :]
    else:
        return "invalid"
    if not tail.startswith(b"[") or not tail.endswith(POWEROFF_SUFFIX):
        return "invalid"
    timestamp = tail[1 : -len(POWEROFF_SUFFIX)]
    if len(timestamp) != 12 or timestamp[5:6] != b".":
        return "invalid"
    whole = timestamp[:5]
    digits = whole.lstrip(b" ")
    if (
        not digits
        or not digits.isdigit()
        or (len(digits) > 1 and digits.startswith(b"0"))
        or not timestamp[6:].isdigit()
    ):
        return "invalid"
    return outcome


def target_for(case: ModeCase, target_uid: int, target_gid: int) -> tuple[int, int]:
    if case.identity == "mapped":
        return target_uid, target_gid
    if case.identity == "retained-root":
        return 0, 0
    if case.identity == "unmapped":
        return UNMAPPED_ID, UNMAPPED_ID
    _fail("unknown-identity-class")


def worker_args(workload: str) -> list[str]:
    if workload == "no-api":
        return ["--config-file", "bangbang-grant:evidence-guest-config", "--no-api"]
    if workload == "api":
        return ["--api-sock", "bangbang-grant:evidence-guest-api/evidence-api.sock"]
    _fail("unknown-workload")


def manifest_document(
    workload: str, resources: Path, workspace: Path
) -> dict[str, object]:
    inputs = {
        "kernel": resources / RESOURCE_NAMES["kernel"],
        "initrd": resources / RESOURCE_NAMES["initrd"],
        "rootfs": resources / RESOURCE_NAMES["rootfs"],
    }
    grants: list[dict[str, str]] = []
    if workload == "no-api":
        grants.append(
            {
                "id": GRANT_IDS["config"],
                "role": "startup-config",
                "access": "read-only",
                "source": os.fspath(resources / RESOURCE_NAMES["config"]),
            }
        )
    elif workload == "api":
        grants.append(
            {
                "id": GRANT_IDS["api"],
                "role": "api-socket-directory",
                "access": "create-children",
                "source": os.fspath(workspace / "api"),
            }
        )
    else:
        _fail("unknown-workload")
    for key, role in (
        ("kernel", "kernel-image"),
        ("initrd", "initrd-image"),
        ("rootfs", "drive-backing"),
    ):
        grants.append(
            {
                "id": GRANT_IDS[key],
                "role": role,
                "access": "read-only",
                "source": os.fspath(inputs[key]),
            }
        )
    for key, role in (
        ("logger", "logger-sink"),
        ("metrics", "metrics-sink"),
        ("serial", "serial-sink"),
    ):
        grants.append(
            {
                "id": GRANT_IDS[key],
                "role": role,
                "access": "write-only",
                "source": os.fspath(workspace / key),
            }
        )
    return {"version": 1, "grants": grants}


class Fixture:
    def __init__(
        self,
        resources: Path,
        case: ModeCase,
        target_uid: int,
        target_gid: int,
    ) -> None:
        self.case = case
        self.uid, self.gid = target_for(case, target_uid, target_gid)
        self.root = _create_runtime_root()
        os.chown(self.root, self.uid, self.gid)
        self.root.chmod(0o700)
        self.root_identity = ObjectIdentity.capture(self.root)
        self.workspace = Path(
            tempfile.mkdtemp(prefix="bangbang-elevated-guest.", dir="/private/tmp")
        )
        self.workspace.chmod(0o700)
        self.entries: dict[str, ObjectIdentity] = {}
        for name in ("logger", "metrics", "serial"):
            _create_file(self.workspace / name, b"", self.uid, self.gid, 0o600)
            self.entries[name] = ObjectIdentity.capture(self.workspace / name)
        if case.workload == "api":
            api = self.workspace / "api"
            api.mkdir(mode=0o700)
            os.chown(api, self.uid, self.gid)
            self.entries["api"] = ObjectIdentity.capture(api)
        manifest = (
            json.dumps(
                manifest_document(case.workload, resources, self.workspace),
                ensure_ascii=True,
                separators=(",", ":"),
            )
            + "\n"
        ).encode("ascii")
        _create_file(self.workspace / "grant-manifest.json", manifest, self.uid, self.gid, 0o600)
        self.entries["grant-manifest.json"] = ObjectIdentity.capture(
            self.workspace / "grant-manifest.json"
        )
        self.paths = {
            name: self.workspace / name
            for name in self.entries
        }
        self.replacement_files: dict[str, ResourceIdentity] = {}
        self.replacement_directories: dict[str, ObjectIdentity] = {}
        self.workspace.chmod(0o711)
        self.workspace_identity = ObjectIdentity.capture(self.workspace)
        self.cleaned = False

    def command(
        self,
        launcher: Path,
        fault: str | None = None,
        adoption_barrier: bool = False,
        daemonize: bool = False,
        daemon_barrier: str | None = None,
        daemon_fault: str | None = None,
    ) -> list[str]:
        if adoption_barrier and (
            fault is not None
            or daemonize
            or daemon_barrier is not None
            or daemon_fault is not None
        ):
            _fail("incompatible-process-options")
        if (daemon_barrier is not None or daemon_fault is not None) and not daemonize:
            _fail("daemon-control-without-daemon")
        if daemon_fault is not None and (fault is not None or daemon_barrier is not None):
            _fail("incompatible-daemon-options")
        if fault is not None and daemon_barrier is not None and not (
            fault == "guest-endpoint-death" and daemon_barrier == "post-ack-watch"
        ):
            _fail("incompatible-daemon-options")
        arguments = [
            os.fspath(launcher),
            "--bangbang-internal-elevated-bootstrap-probe-v2",
            "--root",
            os.fspath(self.root),
            "--target-uid",
            str(self.uid),
            "--target-gid",
            str(self.gid),
            "--mode",
            self.case.mode,
        ]
        if fault is not None:
            arguments.extend(("--fault", fault))
        if adoption_barrier:
            arguments.append(ADOPTION_BARRIER_OPTION)
        if daemon_barrier is not None:
            arguments.extend((DAEMON_BARRIER_OPTION, daemon_barrier))
        if daemon_fault is not None:
            arguments.extend((DAEMON_FAULT_OPTION, daemon_fault))
        arguments.append("--")
        if daemonize:
            worker = (
                launcher.parent.parent
                / "Helpers"
                / "BangbangWorker.app"
                / "Contents"
                / "MacOS"
                / "bangbang-worker"
            )
            arguments.extend(
                (
                    JAILER_ACTIVATION,
                    "--id",
                    f"evidence-{self.root.name[-8:]}",
                    "--exec-file",
                    os.fspath(worker),
                    "--uid",
                    "0",
                    "--gid",
                    "0",
                    "--daemonize",
                    "--",
                )
            )
        arguments.extend(
            (
                "--bangbang-grant-manifest",
                os.fspath(self.workspace / "grant-manifest.json"),
                "--",
                *worker_args(self.case.workload),
            )
        )
        return arguments

    def authority_path(self, name: str) -> Path:
        try:
            return self.paths[name]
        except KeyError as error:
            raise MatrixError("unknown-fixture-authority") from error

    def displace_runtime_authorities(self) -> None:
        names = ["logger", "metrics", "serial"]
        if self.case.workload == "api":
            names.append("api")
        for name in names:
            original = self.workspace / name
            displaced = self.workspace / f".adopted-{name}"
            if self.paths.get(name) != original or original.is_symlink() or displaced.exists():
                _fail("authority-displacement-state")
            original.rename(displaced)
            self.paths[name] = displaced
            expected = self.entries[name]
            if name == "api":
                original.mkdir(mode=0o700)
                os.chown(original, expected.uid, expected.gid)
                self.replacement_directories[name] = ObjectIdentity.capture(original)
            else:
                _create_file(
                    original,
                    REPLACEMENT_BYTES,
                    expected.uid,
                    expected.gid,
                    expected.mode,
                )
                self.replacement_files[name] = ResourceIdentity(
                    ObjectIdentity.capture(original),
                    _sha256(original),
                )

    def validate_runtime_replacements(self) -> None:
        for name, expected in self.replacement_files.items():
            path = self.workspace / name
            metadata = path.lstat()
            if (
                not stat.S_ISREG(metadata.st_mode)
                or ObjectIdentity.capture(path) != expected.identity
                or _sha256(path) != expected.sha256
            ):
                _fail("output-replacement-identity")
        for name, expected in self.replacement_directories.items():
            path = self.workspace / name
            metadata = path.lstat()
            if (
                not stat.S_ISDIR(metadata.st_mode)
                or ObjectIdentity.capture(path) != expected
                or any(path.iterdir())
            ):
                _fail("api-replacement-identity")

    def restore_runtime_authorities(self) -> None:
        self.validate_runtime_replacements()
        for name in ("logger", "metrics", "serial", "api"):
            displaced = self.paths.get(name)
            original = self.workspace / name
            if displaced is None or displaced == original:
                continue
            if name in self.replacement_directories:
                original.rmdir()
                self.replacement_directories.pop(name)
            elif name in self.replacement_files:
                original.unlink()
                self.replacement_files.pop(name)
            elif original.exists() or original.is_symlink():
                _fail("authority-replacement-ledger")
            actual = ObjectIdentity.capture(displaced)
            expected = self.entries[name]
            if (
                actual.device != expected.device
                or actual.inode != expected.inode
                or actual.uid != expected.uid
                or actual.gid != expected.gid
                or actual.mode != expected.mode
                or actual.links != expected.links
            ):
                _fail("authority-adopted-identity")
            displaced.rename(original)
            self.paths[name] = original

    def validate_shape(self, require_outputs: bool) -> None:
        root = ObjectIdentity.capture(self.root)
        if (
            root.device != self.root_identity.device
            or root.inode != self.root_identity.inode
            or root.uid != self.uid
            or root.gid != self.gid
            or root.mode != 0o700
            or any(self.root.iterdir())
        ):
            _fail("runtime-root-residue")
        workspace = ObjectIdentity.capture(self.workspace)
        if (
            workspace.device != self.workspace_identity.device
            or workspace.inode != self.workspace_identity.inode
            or workspace.uid != 0
            or workspace.gid != 0
            or workspace.mode != 0o711
        ):
            _fail("workspace-identity")
        expected_names = {path.name for path in self.paths.values()}
        expected_names.update(self.replacement_files)
        expected_names.update(self.replacement_directories)
        if {entry.name for entry in self.workspace.iterdir()} != expected_names:
            _fail("workspace-shape")
        for name, expected in self.entries.items():
            path = self.paths[name]
            actual = ObjectIdentity.capture(path)
            if name in ("logger", "metrics", "serial") and require_outputs:
                if (
                    actual.device != expected.device
                    or actual.inode != expected.inode
                    or actual.uid != expected.uid
                    or actual.gid != expected.gid
                    or actual.mode != expected.mode
                    or actual.links != expected.links
                    or actual.size > MAX_OUTPUT_BYTES
                ):
                    _fail("output-identity")
            elif name == "api":
                if (
                    actual.device != expected.device
                    or actual.inode != expected.inode
                    or actual.uid != expected.uid
                    or actual.gid != expected.gid
                    or actual.mode != 0o700
                    or actual.links != expected.links
                ):
                    _fail("api-directory-identity")
            elif actual != expected:
                _fail("fixture-identity")
        self.validate_runtime_replacements()
        if self.case.workload == "api" and any(self.paths["api"].iterdir()):
            _fail("api-socket-residue")

    def validate_success_outputs(self) -> None:
        self.validate_shape(require_outputs=True)
        serial = self.paths["serial"].read_bytes()
        if serial_transcript_outcome(serial) != "success":
            _fail("serial-oracle")
        logger_bytes = self.paths["logger"].read_bytes()
        try:
            logger = logger_bytes.decode("utf-8")
        except UnicodeDecodeError as error:
            raise MatrixError("logger-encoding") from error
        for forbidden in (
            "outcome=failed",
            "outcome=abnormal",
            "category=process-failure",
            "category=panic",
        ):
            if forbidden in logger:
                _fail("logger-failure")
        previous = 0
        for index, expected in enumerate((
            "device-kind=time-identity operation=platform-publication outcome=succeeded\n",
            "device-kind=time-identity operation=pvtime-initialization outcome=succeeded\n",
            "operation=backend-startup outcome=succeeded\n",
            "action=InstanceStart\n",
            "operation=boot-worker outcome=running\n",
            "operation=vm-start outcome=succeeded\n",
            "device-kind=block operation=device-reset outcome=succeeded\n",
            "device-kind=block operation=device-activation outcome=succeeded\n",
            "device-kind=block operation=request outcome=succeeded\n",
            "operation=boot-worker outcome=exited\n",
            "operation=guest-power outcome=poweroff\n",
            "operation=vm-stop outcome=succeeded\n",
            "operation=shutdown outcome=orderly\n",
            "event=process-exit category=success\n",
        )):
            offset = logger.find(expected, previous)
            if offset < 0:
                _fail(f"logger-sequence-{index}")
            previous = offset + len(expected)
        if not logger.endswith("event=process-exit category=success\n"):
            _fail("logger-terminal")
        metrics_bytes = self.paths["metrics"].read_bytes()
        if not metrics_bytes or not metrics_bytes.endswith(b"\n"):
            _fail("metrics-framing")
        try:
            lines = [json.loads(line) for line in metrics_bytes.splitlines()]
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise MatrixError("metrics-json") from error
        if len(lines) < 2 or len(lines) > 8:
            _fail("metrics-count")
        for line in lines:
            if (
                not isinstance(line, dict)
                or not isinstance(line.get("utc_timestamp_ms"), int)
                or not isinstance(line.get("vmm"), dict)
                or line["vmm"].get("panic_count") != 0
                or "metrics_flush_count" in line["vmm"]
                or "boot_run_loop_status" in line["vmm"]
            ):
                _fail("metrics-shape")

    def validate_fault_outputs(self) -> None:
        self.validate_shape(require_outputs=True)
        for name in ("logger", "metrics", "serial"):
            if self.paths[name].stat().st_size > MAX_OUTPUT_BYTES:
                _fail("fault-output-bound")

    def capture_runtime_session(self) -> tuple[Path, ObjectIdentity]:
        self.validate_runtime_root()
        entries = list(self.root.iterdir())
        if len(entries) != 1:
            _fail("runtime-session-ledger")
        session = entries[0]
        suffix = session.name.removeprefix(SESSION_PREFIX)
        if (
            not session.name.startswith(SESSION_PREFIX)
            or len(suffix) != SESSION_ID_HEX_BYTES
            or any(character not in "0123456789abcdef" for character in suffix)
        ):
            _fail("runtime-session-name")
        metadata = session.lstat()
        identity = ObjectIdentity.capture(session)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or identity.uid != self.uid
            or identity.gid != self.gid
            or identity.mode != 0o700
            or identity.links != 2
        ):
            _fail("runtime-session-identity")
        return session, identity

    def validate_runtime_root(self) -> None:
        root = ObjectIdentity.capture(self.root)
        if (
            root.device != self.root_identity.device
            or root.inode != self.root_identity.inode
            or root.uid != self.uid
            or root.gid != self.gid
            or root.mode != 0o700
        ):
            _fail("runtime-root-identity")

    def validate_retired_runtime_root(self) -> None:
        self.validate_runtime_root()
        if any(self.root.iterdir()):
            _fail("retired-runtime-session-residue")

    def cleanup_runtime_session(
        self,
        session: Path,
        expected: ObjectIdentity,
    ) -> None:
        if not session.exists() and not session.is_symlink():
            return
        if session.is_symlink() or ObjectIdentity.capture(session) != expected:
            _fail("runtime-session-replacement")
        if any(session.iterdir()):
            _fail("runtime-session-residue")
        session.rmdir()

    def cleanup(self) -> None:
        if self.cleaned:
            return
        if self.replacement_files or self.replacement_directories:
            _fail("fixture-replacement-live")
        if any(path != self.workspace / name for name, path in self.paths.items()):
            _fail("fixture-authority-displaced")
        self.validate_shape(require_outputs=True)
        for name in ("logger", "metrics", "serial", "grant-manifest.json"):
            (self.workspace / name).unlink()
        if self.case.workload == "api":
            (self.workspace / "api").rmdir()
        self.workspace.chmod(0o700)
        self.workspace.rmdir()
        self.root.rmdir()
        self.cleaned = True


class ApiSocketReplacement:
    def __init__(self, fixture: Fixture) -> None:
        if fixture.case.workload != "api":
            _fail("api-socket-replacement-workload")
        directory = fixture.authority_path("api")
        self.original = directory / API_SOCKET_CHILD
        self.displaced = directory / API_DISPLACED_SOCKET_CHILD
        if self.displaced.exists() or self.displaced.is_symlink():
            _fail("api-socket-replacement-collision")
        metadata = self.original.lstat()
        self.owned = ObjectIdentity.capture(self.original)
        if (
            not stat.S_ISSOCK(metadata.st_mode)
            or self.owned.uid != fixture.uid
            or self.owned.gid != fixture.gid
            or self.owned.mode != 0o600
            or self.owned.links != 1
        ):
            _fail("api-socket-replacement-owned-shape")
        self.original.rename(self.displaced)
        _create_file(
            self.original,
            API_SOCKET_REPLACEMENT_BYTES,
            fixture.uid,
            fixture.gid,
            0o600,
        )
        self.replacement = ResourceIdentity(
            ObjectIdentity.capture(self.original),
            _sha256(self.original),
        )
        self.cleaned = False
        self.validate()

    def validate(self) -> None:
        if self.cleaned:
            _fail("api-socket-replacement-already-cleaned")
        replacement_metadata = self.original.lstat()
        displaced_metadata = self.displaced.lstat()
        if (
            not stat.S_ISREG(replacement_metadata.st_mode)
            or ObjectIdentity.capture(self.original) != self.replacement.identity
            or _sha256(self.original) != self.replacement.sha256
            or not stat.S_ISSOCK(displaced_metadata.st_mode)
            or ObjectIdentity.capture(self.displaced) != self.owned
        ):
            _fail("api-socket-replacement-identity")

    def cleanup(self) -> None:
        if self.cleaned:
            return
        self.validate()
        self.original.unlink()
        self.displaced.unlink()
        self.cleaned = True


class SidecarMutation:
    def __init__(
        self,
        sidecar: Path,
        expected: dict[str, ResourceIdentity],
        workload: str,
    ) -> None:
        self.sidecar = sidecar
        self.expected = expected
        self.keys = (
            ("config", "kernel", "initrd", "rootfs")
            if workload == "no-api"
            else ("kernel", "initrd", "rootfs")
        )
        self.displaced: dict[str, Path] = {}
        self.replacements: dict[str, ResourceIdentity] = {}

    def apply(self) -> None:
        if self.displaced or self.replacements:
            _fail("sidecar-mutation-reused")
        for key in self.keys:
            original = self.sidecar / RESOURCE_NAMES[key]
            displaced = self.sidecar / f".adopted-{RESOURCE_NAMES[key]}"
            if original.is_symlink() or displaced.exists():
                _fail("sidecar-displacement-state")
            original.rename(displaced)
            self.displaced[key] = displaced
            expected = self.expected[key].identity
            _create_file(
                original,
                REPLACEMENT_BYTES,
                expected.uid,
                expected.gid,
                expected.mode,
            )
            self.replacements[key] = ResourceIdentity(
                ObjectIdentity.capture(original),
                _sha256(original),
            )
        self.validate()

    def validate(self) -> None:
        if set(self.displaced) != set(self.keys) or set(self.replacements) != set(self.keys):
            _fail("sidecar-mutation-ledger")
        expected_names = set(RESOURCE_NAMES.values())
        expected_names.update(
            f".adopted-{RESOURCE_NAMES[key]}"
            for key in self.keys
        )
        if {entry.name for entry in self.sidecar.iterdir()} != expected_names:
            _fail("sidecar-replacement-shape")
        for key in self.keys:
            displaced = self.displaced[key]
            if (
                ObjectIdentity.capture(displaced) != self.expected[key].identity
                or _sha256(displaced) != self.expected[key].sha256
            ):
                _fail("sidecar-adopted-identity")
            original = self.sidecar / RESOURCE_NAMES[key]
            metadata = original.lstat()
            replacement = self.replacements[key]
            if (
                not stat.S_ISREG(metadata.st_mode)
                or ObjectIdentity.capture(original) != replacement.identity
                or _sha256(original) != replacement.sha256
            ):
                _fail("sidecar-replacement-identity")
        for key in set(RESOURCE_NAMES) - set(self.keys):
            original = self.sidecar / RESOURCE_NAMES[key]
            if (
                ObjectIdentity.capture(original) != self.expected[key].identity
                or _sha256(original) != self.expected[key].sha256
            ):
                _fail("sidecar-unrelated-identity")

    def restore(self) -> None:
        if self.displaced or self.replacements:
            if set(self.displaced) == set(self.keys) and set(self.replacements) == set(self.keys):
                self.validate()
        for key in reversed(self.keys):
            if key not in self.displaced:
                continue
            original = self.sidecar / RESOURCE_NAMES[key]
            replacement = self.replacements.get(key)
            if replacement is not None:
                if (
                    ObjectIdentity.capture(original) != replacement.identity
                    or _sha256(original) != replacement.sha256
                ):
                    _fail("sidecar-replacement-identity")
                original.unlink()
            elif original.exists() or original.is_symlink():
                _fail("sidecar-replacement-ledger")
            displaced = self.displaced[key]
            if (
                ObjectIdentity.capture(displaced) != self.expected[key].identity
                or _sha256(displaced) != self.expected[key].sha256
            ):
                _fail("sidecar-adopted-identity")
            displaced.rename(original)
            self.replacements.pop(key, None)
            self.displaced.pop(key)
        verify_resources(self.sidecar, self.expected)


def expected_success_line(case: ModeCase) -> str:
    workload = (
        "resources=consumed workload=no-api api=absent hvf=created guest=oracle-poweroff"
        if case.workload == "no-api"
        else "resources=consumed workload=api api=complete hvf=created guest=oracle-poweroff"
    )
    return (
        f"status: elevated runtime {case.mode} complete result=complete {case.semantics} "
        "namespace=launcher-created-target-owned authority=consumed lock=independent "
        f"grants=committed {workload} lifecycle=terminal cleanup=complete"
    )


def expected_success_output(case: ModeCase) -> str:
    if case.workload == "no-api":
        prefix = "status: VM running without API\n"
    else:
        prefix = (
            "operation=server outcome=running\n"
            "operation=process-startup outcome=running\n"
            "status: API server listening\n"
            'The API server received a Put request on "/logger".\n'
        )
    return (
        "bangbang 0.1.0\n"
        "hvf target supported: true\n"
        f"{prefix}"
        f"{expected_success_line(case)}"
    )


def expected_fault_line(case: ModeCase, fault: FaultCase) -> str:
    return (
        f"status: elevated runtime {case.mode} blocked stage={fault.stage} "
        f"error={fault.category} "
        f"result={fault.result} {case.semantics}"
    )


def _decode_output(output: bytes) -> str:
    if len(output) > MAX_OUTPUT_BYTES:
        _fail("process-output-bound")
    try:
        return output.decode("utf-8")
    except UnicodeDecodeError as error:
        raise MatrixError("process-output-encoding") from error


def validate_redacted(output: str, fixture: Fixture) -> None:
    sensitive = [
        os.fspath(fixture.root),
        os.fspath(fixture.workspace),
        *GRANT_IDS.values(),
        "bangbang-grant:",
        "BANGBANG_ROOTFS_WORKFLOW_",
    ]
    if any(value in output for value in sensitive):
        _fail("process-output-redaction")


def run_process(command: list[str]) -> tuple[int, bytes]:
    try:
        completed = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
            timeout=PROCESS_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise MatrixError("process-timeout") from error
    return completed.returncode, completed.stdout


def wait_for_adoption_stop(process: subprocess.Popen[bytes]) -> None:
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        try:
            waited, status = os.waitpid(process.pid, os.WUNTRACED | os.WNOHANG)
        except ChildProcessError as error:
            raise MatrixError("adoption-child-lost") from error
        if waited == 0:
            time.sleep(0.01)
            continue
        if waited != process.pid:
            _fail("adoption-child-identity")
        if os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGSTOP:
            return
        if os.WIFEXITED(status) or os.WIFSIGNALED(status):
            process.returncode = os.waitstatus_to_exitcode(status)
            _fail("adoption-stop-missing")
        _fail("adoption-stop-invalid")
    _fail("adoption-stop-timeout")


def _process_table() -> list[tuple[int, int, str]]:
    completed = subprocess.run(
        ["/bin/ps", "-axo", "pid=,ppid=,state="],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
        timeout=BARRIER_TIMEOUT_SECONDS,
        check=False,
    )
    if completed.returncode != 0 or len(completed.stdout) > MAX_OUTPUT_BYTES:
        _fail("process-table")
    rows = []
    for line in completed.stdout.decode("ascii", errors="strict").splitlines():
        fields = line.split()
        if len(fields) != 3 or not fields[0].isdigit() or not fields[1].isdigit():
            _fail("process-table-shape")
        rows.append((int(fields[0]), int(fields[1]), fields[2]))
    return rows


def _libproc() -> ctypes.CDLL:
    try:
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    except OSError as error:
        raise MatrixError("process-info-library") from error
    library.proc_pidinfo.argtypes = (
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_uint64,
        ctypes.c_void_p,
        ctypes.c_int,
    )
    library.proc_pidinfo.restype = ctypes.c_int
    library.proc_pidpath.argtypes = (
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    )
    library.proc_pidpath.restype = ctypes.c_int
    return library


def capture_process(pid: int) -> ProcessIdentity:
    if pid <= 1 or pid > 0x7FFF_FFFF:
        _fail("process-identity-pid")
    library = _libproc()
    info = ProcBsdInfo()
    size = ctypes.sizeof(info)
    result = library.proc_pidinfo(
        pid,
        PROC_PIDTBSDINFO,
        0,
        ctypes.byref(info),
        size,
    )
    if result != size:
        _fail("process-identity-info")
    path_buffer = ctypes.create_string_buffer(PROC_PIDPATHINFO_MAXSIZE)
    path_length = library.proc_pidpath(
        pid,
        path_buffer,
        PROC_PIDPATHINFO_MAXSIZE,
    )
    if path_length <= 0 or path_length >= PROC_PIDPATHINFO_MAXSIZE:
        _fail("process-identity-path")
    try:
        executable = Path(path_buffer.raw[:path_length].decode("utf-8"))
    except UnicodeDecodeError as error:
        raise MatrixError("process-identity-path-encoding") from error
    session = os.getsid(pid)
    values = (
        int(info.pid),
        int(info.ppid),
        int(info.pgid),
        session,
        int(info.uid),
        int(info.gid),
        int(info.ruid),
        int(info.rgid),
        int(info.svuid),
        int(info.svgid),
        int(info.start_seconds),
        int(info.start_microseconds),
    )
    if (
        values[0] != pid
        or values[1] <= 0
        or values[2] <= 0
        or values[3] <= 0
        or values[10] <= 0
        or values[11] < 0
        or values[11] >= 1_000_000
        or not executable.is_absolute()
    ):
        _fail("process-identity-shape")
    return ProcessIdentity(*values, executable)


def expected_worker(launcher: Path) -> Path:
    return (
        launcher.parent.parent
        / "Helpers"
        / "BangbangWorker.app"
        / "Contents"
        / "MacOS"
        / "bangbang-worker"
    )


def validate_daemon_topology(
    supervisor: ProcessIdentity,
    worker: ProcessIdentity,
    launcher: Path,
    fixture: Fixture,
    parent: ProcessIdentity | None = None,
) -> None:
    expected_ids = (fixture.uid, fixture.gid)
    supervisor_ids = (
        supervisor.uid,
        supervisor.gid,
        supervisor.real_uid,
        supervisor.real_gid,
        supervisor.saved_uid,
        supervisor.saved_gid,
    )
    worker_ids = (
        worker.uid,
        worker.gid,
        worker.real_uid,
        worker.real_gid,
        worker.saved_uid,
        worker.saved_gid,
    )
    if (
        supervisor.process_group != supervisor.pid
        or supervisor.session != supervisor.pid
        or supervisor.executable != launcher
        or (parent is not None and supervisor.parent_pid != parent.pid)
        or worker.parent_pid != supervisor.pid
        or worker.process_group != supervisor.pid
        or worker.session != supervisor.pid
        or worker.executable != expected_worker(launcher)
        or supervisor_ids != expected_ids * 3
        or worker_ids != expected_ids * 3
    ):
        _fail("daemon-process-topology")
    if parent is not None:
        parent_ids = (
            parent.uid,
            parent.gid,
            parent.real_uid,
            parent.real_gid,
            parent.saved_uid,
            parent.saved_gid,
        )
        if parent.executable != launcher or parent_ids != expected_ids * 3:
            _fail("daemon-parent-identity")


def wait_for_stopped_daemon(
    supervisor_pid: int,
    launcher: Path,
    fixture: Fixture,
    parent: ProcessIdentity | None = None,
) -> tuple[ProcessIdentity, ProcessIdentity]:
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    observed_worker: int | None = None
    while time.monotonic() < deadline:
        rows = _process_table()
        supervisors = [row for row in rows if row[0] == supervisor_pid]
        children = [row for row in rows if row[1] == supervisor_pid]
        if len(supervisors) > 1 or len(children) > 1:
            _fail("daemon-stop-topology")
        if supervisors and children:
            worker_pid = children[0][0]
            if observed_worker is not None and observed_worker != worker_pid:
                _fail("daemon-stop-worker-swap")
            observed_worker = worker_pid
            if "T" in supervisors[0][2] and "T" in children[0][2]:
                supervisor = capture_process(supervisor_pid)
                worker = capture_process(worker_pid)
                validate_daemon_topology(supervisor, worker, launcher, fixture, parent)
                return supervisor, worker
        time.sleep(0.01)
    _fail("daemon-stop-timeout")


def parse_daemon_pid(output: bytes) -> int:
    decoded = _decode_output(output)
    prefix = "bangbang daemon pid: "
    if not decoded.startswith(prefix) or not decoded.endswith("\n") or decoded.count("\n") != 1:
        _fail("daemon-pid-output")
    value = decoded[len(prefix) : -1]
    if not value.isascii() or not value.isdigit() or value.startswith("0"):
        _fail("daemon-pid-shape")
    pid = int(value)
    if pid <= 1 or pid > 0x7FFF_FFFF:
        _fail("daemon-pid-range")
    return pid


class ProcessExitWatch:
    def __init__(self, processes: tuple[ProcessIdentity, ...]) -> None:
        if len(processes) == 0 or len({process.pid for process in processes}) != len(processes):
            _fail("exit-watch-ledger")
        self.processes = {process.pid: process for process in processes}
        self.queue = select.kqueue()
        changes = [
            select.kevent(
                process.pid,
                filter=select.KQ_FILTER_PROC,
                flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_ONESHOT,
                fflags=select.KQ_NOTE_EXIT | NOTE_EXITSTATUS,
            )
            for process in processes
        ]
        try:
            events = self.queue.control(changes, 0, 0)
        except OSError as error:
            self.queue.close()
            raise MatrixError("exit-watch-register") from error
        if events:
            self.queue.close()
            _fail("exit-watch-register-event")

    def wait(self) -> dict[int, int]:
        if self.queue is None:
            _fail("exit-watch-closed")
        deadline = time.monotonic() + PROCESS_TIMEOUT_SECONDS
        statuses: dict[int, int] = {}
        while len(statuses) != len(self.processes):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                _fail("exit-watch-timeout")
            events = self.queue.control(None, len(self.processes), remaining)
            if not events:
                _fail("exit-watch-timeout")
            for event in events:
                pid = int(event.ident)
                if (
                    event.filter != select.KQ_FILTER_PROC
                    or pid not in self.processes
                    or pid in statuses
                    or event.fflags & select.KQ_NOTE_EXIT == 0
                    or event.fflags & NOTE_EXITSTATUS == 0
                    or event.data < 0
                    or event.data > 0xFFFF
                ):
                    _fail("exit-watch-event")
                statuses[pid] = int(event.data)
        return statuses

    def close(self) -> None:
        if self.queue is not None:
            self.queue.close()
            self.queue = None


@dataclass
class StoppedDaemon:
    parent: subprocess.Popen[bytes]
    parent_identity: ProcessIdentity
    supervisor: ProcessIdentity
    worker: ProcessIdentity
    watch: ProcessExitWatch
    fixture: Fixture
    observed_parent_output: bytes

    def resume(self) -> None:
        try:
            os.kill(self.worker.pid, signal.SIGCONT)
            os.kill(self.supervisor.pid, signal.SIGCONT)
        except ProcessLookupError as error:
            raise MatrixError("daemon-resume-process") from error

    def wait(self) -> dict[int, int]:
        try:
            statuses = self.watch.wait()
        finally:
            self.watch.close()
        wait_for_daemon_parent_detach(
            self.parent,
            self.fixture,
            self.observed_parent_output,
        )
        return statuses

    def cleanup(self) -> None:
        self.watch.close()
        for process in (self.worker, self.supervisor):
            signal_exact_process_if_live(process, signal.SIGKILL)
        if self.parent.poll() is None:
            self.parent.kill()
        try:
            self.parent.wait(timeout=BARRIER_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            self.parent.kill()
            self.parent.wait()
        for process in (self.worker, self.supervisor):
            try:
                wait_for_exact_process_exit(process)
            except MatrixError:
                pass


def validate_daemon_output_prefix(output: bytes, fixture: Fixture) -> None:
    decoded = _decode_output(output)
    validate_redacted(decoded, fixture)
    if not expected_success_output(fixture.case).encode("utf-8").startswith(output):
        _fail("daemon-output-prefix")


def wait_for_daemon_parent_detach(
    parent: subprocess.Popen[bytes],
    fixture: Fixture,
    observed_output: bytes,
) -> None:
    try:
        output, _ = parent.communicate(timeout=BARRIER_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as error:
        raise MatrixError("daemon-parent-detach-timeout") from error
    if parent.returncode != 0:
        _fail("daemon-parent-detach-result")
    validate_daemon_output_prefix(observed_output + output, fixture)


def read_daemon_pid_line(process: subprocess.Popen[bytes]) -> tuple[bytes, bytes]:
    stdout = process.stdout
    if stdout is None:
        _fail("daemon-parent-output")
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    pending = bytearray()
    observed_output = bytearray()
    while time.monotonic() < deadline:
        ready, _, _ = select.select([stdout], [], [], min(0.1, deadline - time.monotonic()))
        if ready:
            try:
                chunk = os.read(stdout.fileno(), MAX_OUTPUT_LINE_BYTES)
            except OSError as error:
                raise MatrixError("daemon-parent-output") from error
            if not chunk:
                _fail("daemon-parent-exited-before-pid")
            pending.extend(chunk)
            if len(observed_output) + len(pending) > MAX_OUTPUT_BYTES:
                _fail("daemon-parent-output-bound")
            while b"\n" in pending:
                end = pending.index(b"\n") + 1
                line = bytes(pending[:end])
                del pending[:end]
                if len(line) > MAX_OUTPUT_LINE_BYTES:
                    _fail("daemon-parent-output-shape")
                if line.startswith(b"bangbang daemon pid: "):
                    observed_output.extend(pending)
                    return line, bytes(observed_output)
                observed_output.extend(line)
            if len(pending) >= MAX_OUTPUT_LINE_BYTES:
                _fail("daemon-parent-output-shape")
        elif process.poll() is not None:
            _fail("daemon-parent-exited-before-pid")
    _fail("daemon-parent-pid-timeout")


def wait_for_namespace_retirement_stop(
    parent: subprocess.Popen[bytes],
    launcher: Path,
    fixture: Fixture,
    expect_linked: bool,
    expected_supervisor: ProcessIdentity | None = None,
) -> tuple[ProcessIdentity, ProcessIdentity, ProcessIdentity, list[Path]]:
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if parent.poll() is not None:
            _fail("namespace-retirement-parent-exit")
        rows = _process_table()
        supervisors = [row for row in rows if row[1] == parent.pid]
        if expected_supervisor is not None:
            supervisors = [
                row for row in supervisors if row[0] == expected_supervisor.pid
            ]
        if len(supervisors) > 1:
            _fail("namespace-retirement-supervisor-ledger")
        if supervisors:
            supervisor_row = supervisors[0]
            workers = [row for row in rows if row[1] == supervisor_row[0]]
            if len(workers) > 1:
                _fail("namespace-retirement-worker-ledger")
            if workers and "T" in supervisor_row[2]:
                entries = list(fixture.root.iterdir())
                correct_ledger = (expect_linked and len(entries) == 1) or (
                    not expect_linked and not entries
                )
                if correct_ledger:
                    parent_identity = capture_process(parent.pid)
                    supervisor = capture_process(supervisor_row[0])
                    worker = capture_process(workers[0][0])
                    validate_daemon_topology(
                        supervisor,
                        worker,
                        launcher,
                        fixture,
                        parent_identity,
                    )
                    if expected_supervisor is not None and not supervisor.same_start(
                        expected_supervisor
                    ):
                        _fail("namespace-retirement-supervisor-reuse")
                    return parent_identity, supervisor, worker, entries
        time.sleep(0.01)
    _fail("namespace-retirement-stop-timeout")


def assert_daemon_namespace_replacement(
    launcher: Path,
    fixture: Fixture,
) -> None:
    parent = subprocess.Popen(
        fixture.command(
            launcher,
            daemonize=True,
            daemon_barrier=DAEMON_NAMESPACE_RETIREMENT_BARRIER,
        ),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
    )
    replacement: Path | None = None
    replacement_identity: ObjectIdentity | None = None
    watch: ProcessExitWatch | None = None
    supervisor: ProcessIdentity | None = None
    worker: ProcessIdentity | None = None
    later_supervisor: ProcessIdentity | None = None
    later_worker: ProcessIdentity | None = None
    try:
        parent_identity, supervisor, worker, entries = wait_for_namespace_retirement_stop(
            parent,
            launcher,
            fixture,
            True,
        )
        canonical = entries[0]
        suffix = canonical.name.removeprefix(SESSION_PREFIX)
        original = ObjectIdentity.capture(canonical)
        if (
            not canonical.name.startswith(SESSION_PREFIX)
            or len(suffix) != SESSION_ID_HEX_BYTES
            or any(character not in "0123456789abcdef" for character in suffix)
            or original.uid != fixture.uid
            or original.gid != fixture.gid
            or original.mode != 0o700
            or original.links != 2
        ):
            _fail("namespace-retirement-linked-shape")
        os.kill(supervisor.pid, signal.SIGCONT)
        later_parent, later_supervisor, later_worker, _ = wait_for_namespace_retirement_stop(
            parent,
            launcher,
            fixture,
            False,
            supervisor,
        )
        if not later_parent.same_start(parent_identity) or not later_worker.same_start(worker):
            _fail("namespace-retirement-process-reuse")
        replacement = fixture.root / canonical.name
        replacement.mkdir(mode=0o700)
        os.chown(replacement, fixture.uid, fixture.gid)
        replacement.chmod(0o700)
        replacement_identity = ObjectIdentity.capture(replacement)
        watch = ProcessExitWatch((later_supervisor, later_worker))
        os.kill(later_supervisor.pid, signal.SIGCONT)
        statuses = watch.wait()
        watch.close()
        watch = None
        try:
            output, _ = parent.communicate(timeout=BARRIER_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired as error:
            raise MatrixError("namespace-retirement-parent-timeout") from error
        decoded = _decode_output(output)
        validate_redacted(decoded, fixture)
        if (
            statuses
            != {
                later_supervisor.pid: 3 << 8,
                later_worker.pid: WORKER_NAMESPACE_REPLACEMENT_EXIT_CODE << 8,
            }
            or parent.returncode != 1
            or decoded
            != "bangbang launcher: elevated daemon handoff failed stage=ready-send\n"
        ):
            _fail("namespace-retirement-replacement-result")
        if (
            list(fixture.root.iterdir()) != [replacement]
            or ObjectIdentity.capture(replacement) != replacement_identity
            or any(replacement.iterdir())
        ):
            _fail("namespace-retirement-replacement-preservation")
        wait_for_exact_process_exit(later_worker)
        wait_for_exact_process_exit(later_supervisor)
        replacement.rmdir()
        replacement = None
        fixture.validate_retired_runtime_root()
        fixture.validate_fault_outputs()
    finally:
        if watch is not None:
            watch.close()
        if parent.poll() is None:
            parent.kill()
            parent.wait()
        for process in (later_worker, later_supervisor, worker, supervisor):
            if process is not None:
                signal_exact_process_if_live(process, signal.SIGKILL)
        if replacement is not None and replacement.exists():
            if ObjectIdentity.capture(replacement) != replacement_identity:
                _fail("namespace-retirement-replacement-cleanup-identity")
            replacement.rmdir()


def start_stopped_daemon(
    launcher: Path,
    fixture: Fixture,
    fault: str | None = None,
) -> StoppedDaemon:
    parent = subprocess.Popen(
        fixture.command(
            launcher,
            fault=fault,
            daemonize=True,
            daemon_barrier="post-ack-watch",
        ),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
    )
    supervisor: ProcessIdentity | None = None
    worker: ProcessIdentity | None = None
    watch: ProcessExitWatch | None = None
    try:
        output, observed_parent_output = read_daemon_pid_line(parent)
        validate_daemon_output_prefix(observed_parent_output, fixture)
        validate_redacted(_decode_output(output), fixture)
        supervisor_pid = parse_daemon_pid(output)
        parent_identity = capture_process(parent.pid)
        supervisor, worker = wait_for_stopped_daemon(
            supervisor_pid,
            launcher,
            fixture,
            parent_identity,
        )
        watch = ProcessExitWatch((supervisor, worker))
        return StoppedDaemon(
            parent,
            parent_identity,
            supervisor,
            worker,
            watch,
            fixture,
            observed_parent_output,
        )
    except BaseException:
        if watch is not None:
            watch.close()
        for process in (worker, supervisor):
            if process is not None:
                signal_exact_process_if_live(process, signal.SIGKILL)
        if parent.poll() is None:
            parent.kill()
            parent.wait()
        raise


def wait_for_exact_process_exit(process: ProcessIdentity) -> None:
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if not _pid_exists(process.pid):
            return
        try:
            current = capture_process(process.pid)
        except MatrixError:
            if not _pid_exists(process.pid):
                return
            raise
        if not current.same_start(process):
            return
        time.sleep(0.01)
    _fail("exact-process-exit-timeout")


def signal_exact_process_if_live(
    process: ProcessIdentity,
    signal_number: int,
) -> None:
    try:
        current = capture_process(process.pid)
    except MatrixError:
        return
    if not current.same_start(process):
        return
    try:
        os.kill(process.pid, signal_number)
    except ProcessLookupError:
        pass


def assert_daemon_success(launcher: Path, fixture: Fixture) -> None:
    daemon = start_stopped_daemon(launcher, fixture)
    try:
        fixture.validate_retired_runtime_root()
        daemon.resume()
        statuses = daemon.wait()
        if statuses != {daemon.supervisor.pid: 0, daemon.worker.pid: 0}:
            _fail("daemon-success-status")
        wait_for_exact_process_exit(daemon.worker)
        wait_for_exact_process_exit(daemon.supervisor)
        fixture.validate_retired_runtime_root()
        fixture.validate_success_outputs()
    finally:
        daemon.cleanup()


def assert_daemon_retirement_fault(
    launcher: Path,
    fixture: Fixture,
    fault: str,
) -> None:
    if fault not in DAEMON_RETIREMENT_FAULTS:
        _fail("daemon-retirement-fault")
    status, raw_output = run_process(
        fixture.command(launcher, fault=fault, daemonize=True)
    )
    output = _decode_output(raw_output)
    validate_redacted(output, fixture)
    if (
        status != 1
        or output
        != "bangbang launcher: elevated daemon handoff failed stage=ready-send\n"
    ):
        _fail("daemon-retirement-fault-result")
    fixture.validate_retired_runtime_root()
    fixture.validate_fault_outputs()


def assert_daemon_endpoint_death(
    launcher: Path,
    fixture: Fixture,
    first: str,
) -> None:
    daemon = start_stopped_daemon(launcher, fixture, "guest-endpoint-death")
    try:
        fixture.validate_retired_runtime_root()
        if first == "worker":
            os.kill(daemon.worker.pid, signal.SIGKILL)
            os.kill(daemon.supervisor.pid, signal.SIGCONT)
            expected = {daemon.worker.pid: signal.SIGKILL, daemon.supervisor.pid: 3 << 8}
        elif first == "launcher":
            os.kill(daemon.supervisor.pid, signal.SIGKILL)
            os.kill(daemon.worker.pid, signal.SIGCONT)
            expected = {daemon.supervisor.pid: signal.SIGKILL, daemon.worker.pid: 1 << 8}
        else:
            _fail("daemon-death-order")
        statuses = daemon.wait()
        if statuses != expected:
            _fail("daemon-death-status")
        wait_for_exact_process_exit(daemon.worker)
        wait_for_exact_process_exit(daemon.supervisor)
        fixture.validate_retired_runtime_root()
        fixture.validate_fault_outputs()
    finally:
        daemon.cleanup()


def assert_daemon_signal(
    launcher: Path,
    fixture: Fixture,
    target: str,
    signal_number: int,
) -> None:
    daemon = start_stopped_daemon(launcher, fixture, "guest-endpoint-death")
    try:
        fixture.validate_retired_runtime_root()
        if target == "supervisor" and signal_number in (signal.SIGINT, signal.SIGTERM):
            os.kill(daemon.supervisor.pid, signal_number)
            expected = {daemon.supervisor.pid: 3 << 8, daemon.worker.pid: 1 << 8}
        elif target == "worker" and signal_number == signal.SIGHUP:
            os.kill(daemon.worker.pid, signal_number)
            expected = {
                daemon.supervisor.pid: 3 << 8,
                daemon.worker.pid: WORKER_SIGHUP_EXIT_CODE << 8,
            }
        else:
            _fail("daemon-signal-case")
        daemon.resume()
        statuses = daemon.wait()
        if statuses != expected:
            _fail("daemon-signal-status")
        wait_for_exact_process_exit(daemon.worker)
        wait_for_exact_process_exit(daemon.supervisor)
        fixture.validate_retired_runtime_root()
        fixture.validate_fault_outputs()
    finally:
        daemon.cleanup()


def run_daemon_concurrent_survival(
    launcher: Path,
    killed_fixture: Fixture,
    surviving_fixture: Fixture,
) -> None:
    killed = start_stopped_daemon(launcher, killed_fixture, "guest-endpoint-death")
    surviving: StoppedDaemon | None = None
    try:
        surviving = start_stopped_daemon(launcher, surviving_fixture)
        killed_fixture.validate_retired_runtime_root()
        surviving_fixture.validate_retired_runtime_root()
        if len(
            {
                killed.supervisor.pid,
                killed.worker.pid,
                surviving.supervisor.pid,
                surviving.worker.pid,
            }
        ) != 4:
            _fail("daemon-concurrency-process-ledger")
        os.kill(killed.supervisor.pid, signal.SIGKILL)
        os.kill(killed.worker.pid, signal.SIGCONT)
        surviving.resume()
        killed_statuses = killed.wait()
        surviving_statuses = surviving.wait()
        if killed_statuses != {
            killed.supervisor.pid: signal.SIGKILL,
            killed.worker.pid: 1 << 8,
        } or surviving_statuses != {
            surviving.supervisor.pid: 0,
            surviving.worker.pid: 0,
        }:
            _fail("daemon-concurrency-status")
        for process in (
            killed.worker,
            killed.supervisor,
            surviving.worker,
            surviving.supervisor,
        ):
            wait_for_exact_process_exit(process)
        killed_fixture.validate_retired_runtime_root()
        surviving_fixture.validate_retired_runtime_root()
        killed_fixture.validate_fault_outputs()
        surviving_fixture.validate_success_outputs()
    finally:
        killed.cleanup()
        if surviving is not None:
            surviving.cleanup()


def wait_for_stopped_worker(process: subprocess.Popen[bytes]) -> int:
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    observed: int | None = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            _fail("death-launcher-exited-early")
        children = [row for row in _process_table() if row[1] == process.pid]
        if len(children) > 1:
            _fail("death-worker-topology")
        if children:
            worker_pid, _, state = children[0]
            if worker_pid <= 1 or (observed is not None and worker_pid != observed):
                _fail("death-worker-identity")
            observed = worker_pid
            if "T" in state:
                return worker_pid
        time.sleep(0.01)
    _fail("death-worker-stop-timeout")


def _pid_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def wait_for_pid_exit(pid: int) -> None:
    deadline = time.monotonic() + BARRIER_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if not _pid_exists(pid):
            return
        time.sleep(0.01)
    _fail("death-worker-exit-timeout")


def assert_success(launcher: Path, fixture: Fixture) -> None:
    status, raw_output = run_process(fixture.command(launcher))
    output = _decode_output(raw_output).rstrip("\n")
    validate_redacted(output, fixture)
    if status != 0:
        prefix = f"status: elevated runtime {fixture.case.mode} blocked stage="
        for line in output.splitlines():
            if line.startswith(prefix):
                fields = line.removeprefix(prefix).split()
                stage = fields[0] if fields else ""
                category = next(
                    (field.removeprefix("error=") for field in fields if field.startswith("error=")),
                    "",
                )
                if (
                    stage
                    and category
                    and all(character.isalnum() or character == "-" for character in stage)
                    and all(character.isalnum() or character == "-" for character in category)
                ):
                    _fail(
                        f"success-result-{fixture.case.mode}-blocked-{stage}-{category}"
                    )
        _fail(f"success-result-{fixture.case.mode}-exit")
    if output != expected_success_output(fixture.case):
        _fail(f"success-result-{fixture.case.mode}-output")
    fixture.validate_success_outputs()


def assert_adoption_replacement(
    launcher: Path,
    fixture: Fixture,
    sidecar_mutation: SidecarMutation,
) -> None:
    process = subprocess.Popen(
        fixture.command(launcher, adoption_barrier=True),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
    )
    try:
        wait_for_adoption_stop(process)
        sidecar_mutation.apply()
        fixture.displace_runtime_authorities()
        os.kill(process.pid, signal.SIGCONT)
        try:
            raw_output, _ = process.communicate(timeout=PROCESS_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired as error:
            process.kill()
            process.wait()
            raise MatrixError("adoption-process-timeout") from error
        output = _decode_output(raw_output).rstrip("\n")
        validate_redacted(output, fixture)
        if fixture.case.workload == "api":
            expected = expected_fault_line(
                fixture.case,
                API_PREOPENED_REPLACEMENT_BOUNDARY,
            )
            if process.returncode != 3 or expected not in output.splitlines():
                prefix = f"status: elevated runtime {fixture.case.mode} blocked stage="
                for line in output.splitlines():
                    if line.startswith(prefix):
                        fields = line.removeprefix(prefix).split()
                        stage = fields[0] if fields else "unknown"
                        category = next(
                            (
                                field.removeprefix("error=")
                                for field in fields
                                if field.startswith("error=")
                            ),
                            "unknown",
                        )
                        _fail(f"adoption-process-result-api-{stage}-{category}")
                _fail("adoption-process-result-api")
            fixture.validate_fault_outputs()
        else:
            if process.returncode != 0 or output != expected_success_output(fixture.case):
                _fail("adoption-process-result-no-api")
            fixture.validate_success_outputs()
        fixture.validate_runtime_replacements()
        sidecar_mutation.validate()
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


def assert_tampered_resource_rejected(
    launcher: Path,
    fixture: Fixture,
    sidecar_mutation: SidecarMutation,
) -> None:
    sidecar_mutation.apply()
    status, raw_output = run_process(fixture.command(launcher))
    output = _decode_output(raw_output)
    validate_redacted(output, fixture)
    if status != 1 or output != "bangbang launcher: invalid production launch policy\n":
        _fail("tampered-resource-result")
    fixture.validate_fault_outputs()


def assert_fault(launcher: Path, fixture: Fixture, fault: FaultCase) -> None:
    status, raw_output = run_process(fixture.command(launcher, fault.fault))
    output = _decode_output(raw_output)
    validate_redacted(output, fixture)
    expected = expected_fault_line(fixture.case, fault)
    if status != 3 or expected not in output.splitlines():
        _fail("fault-result")
    fixture.validate_fault_outputs()


def assert_endpoint_death(
    launcher: Path,
    fixture: Fixture,
    first: str,
    boundary: str,
    replace_api_socket: bool = False,
) -> None:
    if boundary == "pre-readiness":
        if fixture.case.workload != "api":
            _fail("death-pre-readiness-workload")
        fault = FaultCase(
            "api-listener-endpoint-death",
            "api-listener-adoption",
            "api-boundary",
            "api",
        )
    elif boundary == "post-hvf":
        fault = FaultCase(
            "guest-endpoint-death",
            "guest-endpoint-death",
            "guest-boundary",
            fixture.case.workload,
        )
    else:
        _fail("death-boundary")
    if replace_api_socket and boundary != "pre-readiness":
        _fail("death-replacement-boundary")
    process = subprocess.Popen(
        fixture.command(launcher, fault.fault),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
    )
    worker_pid: int | None = None
    runtime_session: tuple[Path, ObjectIdentity] | None = None
    replacement: ApiSocketReplacement | None = None
    try:
        worker_pid = wait_for_stopped_worker(process)
        runtime_session = fixture.capture_runtime_session()
        if replace_api_socket:
            replacement = ApiSocketReplacement(fixture)
        if first == "worker":
            os.kill(worker_pid, signal.SIGKILL)
        elif first == "launcher":
            os.kill(process.pid, signal.SIGKILL)
            os.kill(worker_pid, signal.SIGCONT)
        else:
            _fail("death-order")
        try:
            raw_output, _ = process.communicate(timeout=PROCESS_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired as error:
            raise MatrixError("death-process-timeout") from error
        output = _decode_output(raw_output)
        validate_redacted(output, fixture)
        if first == "worker":
            expected = expected_fault_line(fixture.case, fault)
            if process.returncode != 3 or expected not in output.splitlines():
                _fail("worker-first-death-result")
        elif process.returncode != -signal.SIGKILL or expected_success_line(
            fixture.case
        ) in output.splitlines():
            _fail("launcher-first-death-result")
        wait_for_pid_exit(worker_pid)
        if first == "launcher":
            session, identity = runtime_session
            fixture.cleanup_runtime_session(session, identity)
        if replacement is not None:
            replacement.validate()
            replacement.cleanup()
        fixture.validate_fault_outputs()
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        if worker_pid is not None and _pid_exists(worker_pid):
            try:
                os.kill(worker_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            else:
                try:
                    wait_for_pid_exit(worker_pid)
                except MatrixError:
                    pass
        if replacement is not None and not replacement.cleaned:
            try:
                replacement.cleanup()
            except (MatrixError, OSError):
                pass


def run_concurrent(launcher: Path, fixtures: list[Fixture]) -> None:
    processes = [
        subprocess.Popen(
            fixture.command(launcher),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env={"HOME": "/var/root", "PATH": "/usr/bin:/bin"},
        )
        for fixture in fixtures
    ]
    if len({process.pid for process in processes}) != len(processes):
        _fail("concurrent-process-identity")
    try:
        for fixture, process in zip(fixtures, processes):
            try:
                output, _ = process.communicate(timeout=PROCESS_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired as error:
                for running in processes:
                    if running.poll() is None:
                        running.kill()
                        running.wait()
                raise MatrixError("concurrent-timeout") from error
            decoded = _decode_output(output).rstrip("\n")
            validate_redacted(decoded, fixture)
            if process.returncode != 0 or decoded != expected_success_output(fixture.case):
                _fail("concurrent-result")
            fixture.validate_success_outputs()
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
                process.wait()


def mode_for(workload: str, identity: str = "mapped") -> ModeCase:
    for case in MODE_CASES:
        if case.workload == workload and case.identity == identity:
            return case
    _fail("mode-selection")


def run_matrix(
    launcher: Path,
    resources: Path,
    sidecar: Path,
    target_uid: int,
    target_gid: int,
) -> None:
    resource_ledger = capture_resources(resources)
    sidecar_ledger = capture_resources(sidecar)
    if any(
        resource_ledger[key].identity == sidecar_ledger[key].identity
        for key in RESOURCE_NAMES
    ):
        _fail("sidecar-not-independent")
    live: list[Fixture] = []
    try:
        for case in MODE_CASES:
            for _ in range(3):
                fixture = Fixture(resources, case, target_uid, target_gid)
                live.append(fixture)
                assert_success(launcher, fixture)
                verify_resources(resources, resource_ledger)
                fixture.cleanup()
        for case in MODE_CASES:
            fixture = Fixture(resources, case, target_uid, target_gid)
            live.append(fixture)
            assert_daemon_success(launcher, fixture)
            verify_resources(resources, resource_ledger)
            fixture.cleanup()
        for fault in DAEMON_RETIREMENT_FAULTS:
            fixture = Fixture(
                resources,
                mode_for("no-api"),
                target_uid,
                target_gid,
            )
            live.append(fixture)
            assert_daemon_retirement_fault(launcher, fixture, fault)
            verify_resources(resources, resource_ledger)
            fixture.cleanup()
        for workload in ("no-api", "api"):
            for first in ("worker", "launcher"):
                fixture = Fixture(
                    resources,
                    mode_for(workload),
                    target_uid,
                    target_gid,
                )
                live.append(fixture)
                assert_daemon_endpoint_death(launcher, fixture, first)
                verify_resources(resources, resource_ledger)
                fixture.cleanup()
        for target, signal_number in (
            ("supervisor", signal.SIGINT),
            ("supervisor", signal.SIGTERM),
            ("worker", signal.SIGHUP),
        ):
            fixture = Fixture(
                resources,
                mode_for("no-api"),
                target_uid,
                target_gid,
            )
            live.append(fixture)
            assert_daemon_signal(launcher, fixture, target, signal_number)
            verify_resources(resources, resource_ledger)
            fixture.cleanup()
        replacement_fixture = Fixture(
            resources,
            mode_for("no-api"),
            target_uid,
            target_gid,
        )
        live.append(replacement_fixture)
        assert_daemon_namespace_replacement(launcher, replacement_fixture)
        verify_resources(resources, resource_ledger)
        replacement_fixture.cleanup()
        killed_fixture = Fixture(
            resources,
            mode_for("no-api"),
            target_uid,
            target_gid,
        )
        surviving_fixture = Fixture(
            resources,
            mode_for("api"),
            target_uid,
            target_gid,
        )
        live.extend((killed_fixture, surviving_fixture))
        run_daemon_concurrent_survival(
            launcher,
            killed_fixture,
            surviving_fixture,
        )
        verify_resources(resources, resource_ledger)
        killed_fixture.cleanup()
        surviving_fixture.cleanup()
        for workload in ("no-api", "api"):
            fixtures = [
                Fixture(resources, mode_for(workload), target_uid, target_gid)
                for _ in range(2)
            ]
            live.extend(fixtures)
            run_concurrent(launcher, fixtures)
            verify_resources(resources, resource_ledger)
            for fixture in fixtures:
                fixture.cleanup()
        for fault in FAULT_CASES:
            fixture = Fixture(
                resources,
                mode_for(fault.workload),
                target_uid,
                target_gid,
            )
            live.append(fixture)
            assert_fault(launcher, fixture, fault)
            verify_resources(resources, resource_ledger)
            fixture.cleanup()
        for first in ("worker", "launcher"):
            fixture = Fixture(
                resources,
                mode_for("no-api"),
                target_uid,
                target_gid,
            )
            live.append(fixture)
            assert_endpoint_death(launcher, fixture, first, "post-hvf")
            verify_resources(resources, resource_ledger)
            fixture.cleanup()
        for boundary in ("pre-readiness", "post-hvf"):
            for first in ("worker", "launcher"):
                fixture = Fixture(
                    resources,
                    mode_for("api"),
                    target_uid,
                    target_gid,
                )
                live.append(fixture)
                assert_endpoint_death(
                    launcher,
                    fixture,
                    first,
                    boundary,
                    replace_api_socket=boundary == "pre-readiness",
                )
                verify_resources(resources, resource_ledger)
                fixture.cleanup()
        for workload in ("no-api", "api"):
            tamper_fixture = Fixture(
                sidecar,
                mode_for(workload),
                target_uid,
                target_gid,
            )
            live.append(tamper_fixture)
            tamper_mutation = SidecarMutation(sidecar, sidecar_ledger, workload)
            try:
                assert_tampered_resource_rejected(
                    launcher,
                    tamper_fixture,
                    tamper_mutation,
                )
            finally:
                tamper_mutation.restore()
            verify_resources(sidecar, sidecar_ledger)
            verify_resources(resources, resource_ledger)
            tamper_fixture.cleanup()
        for workload in ("no-api", "api"):
            fixture = Fixture(
                sidecar,
                mode_for(workload),
                target_uid,
                target_gid,
            )
            live.append(fixture)
            mutation = SidecarMutation(sidecar, sidecar_ledger, workload)
            try:
                assert_adoption_replacement(launcher, fixture, mutation)
            finally:
                try:
                    fixture.restore_runtime_authorities()
                finally:
                    mutation.restore()
            if workload == "api":
                fixture.validate_fault_outputs()
            else:
                fixture.validate_success_outputs()
            verify_resources(sidecar, sidecar_ledger)
            verify_resources(resources, resource_ledger)
            fixture.cleanup()
    finally:
        for fixture in reversed(live):
            if not fixture.cleaned:
                try:
                    fixture.cleanup()
                except (MatrixError, OSError):
                    pass
    verify_resources(resources, resource_ledger)
    verify_resources(sidecar, sidecar_ledger)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--launcher", type=Path, required=True)
    parser.add_argument("--resources", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--target-uid", type=int, required=True)
    parser.add_argument("--target-gid", type=int, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if os.getuid() != 0 or os.geteuid() != 0 or os.getgid() != 0 or os.getegid() != 0:
            _fail("explicit-root-required")
        if (
            not args.launcher.is_absolute()
            or not args.resources.is_absolute()
            or not args.sidecar.is_absolute()
            or not args.launcher.is_file()
            or args.launcher.is_symlink()
            or not args.resources.is_dir()
            or args.resources.is_symlink()
            or not args.sidecar.is_dir()
            or args.sidecar.is_symlink()
            or args.target_uid <= 0
            or args.target_uid > 0xFFFF_FFFF
            or args.target_gid <= 0
            or args.target_gid > 0xFFFF_FFFF
        ):
            _fail("invalid-input")
        run_matrix(
            args.launcher,
            args.resources,
            args.sidecar,
            args.target_uid,
            args.target_gid,
        )
    except MatrixError as error:
        print(f"bangbang elevated guest matrix: failed reason={error}", file=sys.stderr)
        return 1
    except OSError:
        print("bangbang elevated guest matrix: failed reason=os-error", file=sys.stderr)
        return 1
    except ValueError:
        print("bangbang elevated guest matrix: failed reason=value-error", file=sys.stderr)
        return 1
    print(MATRIX_SUMMARY)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
