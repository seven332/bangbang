#!/usr/bin/env python3
"""Prepare and run the least-privileged elevated vmnet handoff foundation."""

from __future__ import annotations

import argparse
import array
import ctypes
import dataclasses
import enum
import errno
import fcntl
import hashlib
import json
import os
import platform
import plistlib
import re
import resource
import secrets
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, NoReturn, Optional, Sequence


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
BUILD_BUNDLE = REPOSITORY_ROOT / "scripts/build-production-bundle.sh"
PREPARE_WRAPPER = REPOSITORY_ROOT / "scripts/prepare-elevated-vmnet-handoff.sh"
RUN_WRAPPER = REPOSITORY_ROOT / "scripts/run-elevated-vmnet-handoff.sh"
IMPLEMENTATION_PATHS = (
    Path("scripts/elevated_vmnet_handoff.py"),
    Path("scripts/prepare-elevated-vmnet-handoff.sh"),
    Path("scripts/run-elevated-vmnet-handoff.sh"),
    Path("scripts/build-production-bundle.sh"),
)

SCHEMA_VERSION = 1
PACKAGE_KIND = "bangbang-elevated-vmnet-handoff"
MANIFEST_NAME = "manifest.json"
BUNDLE_NAME = "Bangbang.app"
LAUNCHER_NAME = "bangbang"
PROVIDER_NAME = "bangbang-vmnet-provider"
WORKER_BUNDLE_NAME = "BangbangWorker.app"
WORKER_NAME = "bangbang-worker"
LAUNCHER_IDENTIFIER = "dev.bangbang"
PROVIDER_IDENTIFIER = "dev.bangbang.vmnet-provider"
WORKER_IDENTIFIER = "dev.bangbang.worker"
MAX_MANIFEST_BYTES = 512 * 1024
MAX_PACKAGE_ENTRIES = 512
MAX_PACKAGE_BYTES = 2 * 1024 * 1024 * 1024
MAX_IMPLEMENTATION_BYTES = 2 * 1024 * 1024
GIT_OBJECT_RE = re.compile(r"[0-9a-f]{40,64}\Z")
FIXED_ENVIRONMENT = {"LANG": "C", "LC_ALL": "C"}
RENAME_EXCL = 0x0000_0004

RECORD_BYTES = 4096
RECORD_MAGIC = b"BBEH0001"
RECORD_VERSION = 1
RECORD_PREFIX = struct.Struct("!8sHHHHQQ32sQqI12s")
DIGEST_OFFSET = RECORD_PREFIX.size
PAYLOAD_OFFSET = DIGEST_OFFSET + 32
MAX_RECORD_PAYLOAD = RECORD_BYTES - PAYLOAD_OFFSET
MAX_ARGUMENTS = 64
MAX_ARGUMENT_BYTES = 1024
MAX_ARGUMENT_PAYLOAD = 3072
MAX_ANCILLARY_DESCRIPTORS = 4
SOCKET_BUFFER_BYTES = 8192
PROTOCOL_TIMEOUT = 5.0
WAIT_SLICE_MILLISECONDS = 250
PROCESS_TIMEOUT = 90.0
SESSION_TIMEOUT = 30.0 * 60.0
CLEANUP_TIMEOUT = 10.0
POLL_SECONDS = 0.05
MAX_CAPTURE_BYTES = 256 * 1024
MAX_CREDENTIAL_GROUPS = 1024
PROVIDER_PARENT_LOSS_GRACE = 0.5
PROVIDER_SIGNAL_GRACE = 2.0

CREDENTIAL_FAILURES = (
    "credentials-initial-observe",
    "credentials-initial-process",
    "credentials-initial-root",
    "credentials-clear-groups",
    "credentials-cleared-groups",
    "credentials-set-gid",
    "credentials-set-uid",
    "credentials-dropped-observe",
    "credentials-dropped-process",
    "credentials-dropped-groups",
    "credentials-dropped-uid",
    "credentials-dropped-gid",
    "credentials-restore-uid",
    "credentials-restore-gid",
    "credentials-restore-groups",
    "credentials-restored-observe",
    "credentials-restored-state",
)
PROVIDER_STATUS_FAILURES = {
    10: "probe-completion-configuration",
    11: "probe-completion-provider-protocol",
    12: "probe-completion-provider-process",
    13: "probe-completion-provider-timeout",
    14: "probe-completion-provider-cleanup",
    15: "probe-completion-provider-io",
    16: "probe-completion-provider-authority",
    17: "probe-completion-provider-descriptor",
    18: "probe-completion-provider-bootstrap-descriptor",
    19: "probe-completion-provider-stream",
}
PROBE_FAILURES = (
    *PROVIDER_STATUS_FAILURES.values(),
    "probe-completion-child-status",
    "probe-completion-signal",
    "probe-completion-stderr",
    "probe-completion-stdout",
    "probe-signal-exited",
    "probe-socket-observe",
    "probe-socket-shape",
    "probe-socket-timeout",
    "probe-path",
    "probe-private-root",
    "probe-private-file",
    "probe-session-root",
    "probe-term-cleanup",
    "probe-kill-cleanup",
)
CONTROLLER_FAILURES = (
    "internal",
    "credentials",
    *CREDENTIAL_FAILURES,
    "protocol",
    "protocol-timeout",
    "descriptor",
    "arguments",
    "output",
    "timeout",
    "cleanup",
    "probe",
    *PROBE_FAILURES,
    "controller",
)
SUPERVISOR_PHASES = (
    "initial",
    "fork",
    "credentials",
    "handshake",
    "lifecycle",
    "controller-exit",
    "cleanup-ack",
)
LIFECYCLE_FAILURES = (
    "guardian",
    "controller",
    "spawn",
    "cleanup",
    "signal",
    "lease",
    "session-timeout",
    "protocol",
    "protocol-timeout",
    "descriptor",
    "arguments",
)
SUPERVISOR_FAILURES = (
    *SUPERVISOR_PHASES,
    "identity-capture",
    "identity-parent",
    "identity-uid",
    "identity-gid",
    *(f"lifecycle-{category}" for category in LIFECYCLE_FAILURES),
    *(f"controller-{category}" for category in CONTROLLER_FAILURES),
)

PROC_PIDTBSDINFO = 3
PROC_PIDPATHINFO_MAXSIZE = 4096


class HandoffError(RuntimeError):
    """One value-free handoff failure."""

    def __init__(self, category: str) -> None:
        super().__init__(category)
        self.category = category


class ClosedArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> NoReturn:
        raise HandoffError("invocation")


def _fail(category: str) -> NoReturn:
    raise HandoffError(category)


def _parse_id(value: str) -> int:
    if (
        not value
        or not value.isascii()
        or not value.isdecimal()
        or value.startswith("0")
        or len(value) > 10
    ):
        _fail("invocation")
    parsed = int(value)
    if not 0 < parsed <= 0xFFFF_FFFF:
        _fail("invocation")
    return parsed


def _canonical(value: object) -> bytes:
    try:
        return (
            json.dumps(
                value,
                allow_nan=False,
                ensure_ascii=True,
                indent=2,
                sort_keys=True,
            )
            + "\n"
        ).encode("ascii")
    except (TypeError, ValueError) as error:
        raise HandoffError("manifest") from error


def _duplicate_safe_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail("manifest")
        result[key] = value
    return result


def _load_canonical_json(path: Path, maximum: int) -> dict[str, Any]:
    descriptor = -1
    try:
        before = os.lstat(path)
        if (
            not stat.S_ISREG(before.st_mode)
            or stat.S_ISLNK(before.st_mode)
            or before.st_nlink != 1
            or before.st_size <= 0
            or before.st_size > maximum
        ):
            _fail("manifest")
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_dev != before.st_dev
            or opened.st_ino != before.st_ino
            or opened.st_size != before.st_size
        ):
            _fail("manifest")
        raw = bytearray()
        while len(raw) <= maximum:
            chunk = os.read(descriptor, min(64 * 1024, maximum + 1 - len(raw)))
            if not chunk:
                break
            raw.extend(chunk)
        after = os.fstat(descriptor)
        visible = os.lstat(path)
        if (
            len(raw) > maximum
            or after.st_dev != opened.st_dev
            or after.st_ino != opened.st_ino
            or after.st_size != opened.st_size
            or visible.st_dev != opened.st_dev
            or visible.st_ino != opened.st_ino
        ):
            _fail("manifest")
        value = json.loads(bytes(raw), object_pairs_hook=_duplicate_safe_object)
    except HandoffError:
        raise
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise HandoffError("manifest") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    if not isinstance(value, dict) or bytes(raw) != _canonical(value):
        _fail("manifest")
    return value


def _sha256_file(path: Path, maximum: int) -> tuple[int, str]:
    descriptor = -1
    digest = hashlib.sha256()
    try:
        before = os.lstat(path)
        if (
            not stat.S_ISREG(before.st_mode)
            or stat.S_ISLNK(before.st_mode)
            or before.st_nlink != 1
            or before.st_size < 0
            or before.st_size > maximum
        ):
            _fail("artifact")
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_dev != before.st_dev
            or opened.st_ino != before.st_ino
            or opened.st_size != before.st_size
        ):
            _fail("artifact")
        while chunk := os.read(descriptor, 1024 * 1024):
            digest.update(chunk)
        after = os.fstat(descriptor)
        visible = os.lstat(path)
        if (
            after.st_dev != opened.st_dev
            or after.st_ino != opened.st_ino
            or after.st_size != opened.st_size
            or visible.st_dev != opened.st_dev
            or visible.st_ino != opened.st_ino
            or visible.st_size != opened.st_size
        ):
            _fail("artifact")
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("artifact") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    return before.st_size, digest.hexdigest()


def _run_tool(
    arguments: Sequence[str],
    category: str,
    *,
    timeout: float = 30.0,
    cwd: Path = REPOSITORY_ROOT,
    maximum: int = MAX_CAPTURE_BYTES,
    environment: Optional[Mapping[str, str]] = None,
) -> subprocess.CompletedProcess[bytes]:
    if not arguments or any(not value or "\x00" in value for value in arguments):
        _fail("internal")
    try:
        result = subprocess.run(
            tuple(arguments),
            cwd=cwd,
            env=dict(environment) if environment is not None else FIXED_ENVIRONMENT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise HandoffError(category) from error
    if len(result.stdout) > maximum or len(result.stderr) > maximum:
        _fail(category)
    return result


@dataclasses.dataclass(frozen=True)
class SourceIdentity:
    commit: str
    tree: str


def read_clean_source_identity() -> SourceIdentity:
    for arguments in (
        ("/usr/bin/git", "diff-index", "--quiet", "HEAD", "--"),
        ("/usr/bin/git", "diff-files", "--quiet", "--"),
        (
            "/usr/bin/git",
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ),
    ):
        outcome = _run_tool(arguments, "source", timeout=10.0)
        if outcome.returncode != 0 or outcome.stdout or outcome.stderr:
            _fail("source")

    values: list[str] = []
    for arguments in (
        ("/usr/bin/git", "rev-parse", "--verify", "HEAD"),
        ("/usr/bin/git", "rev-parse", "--verify", "HEAD^{tree}"),
    ):
        outcome = _run_tool(arguments, "source", timeout=10.0)
        try:
            lines = outcome.stdout.decode("ascii").splitlines()
        except UnicodeDecodeError as error:
            raise HandoffError("source") from error
        if outcome.returncode != 0 or outcome.stderr or len(lines) != 1:
            _fail("source")
        values.append(lines[0])
    if any(GIT_OBJECT_RE.fullmatch(value) is None for value in values):
        _fail("source")
    return SourceIdentity(*values)


@dataclasses.dataclass(frozen=True)
class ProductLayout:
    bundle: Path
    launcher: Path
    provider: Path
    worker_bundle: Path
    worker: Path

    @classmethod
    def from_package(cls, package: Path) -> "ProductLayout":
        if not package.is_absolute():
            _fail("package")
        bundle = package / BUNDLE_NAME
        helpers = bundle / "Contents/Helpers"
        worker_bundle = helpers / WORKER_BUNDLE_NAME
        return cls(
            bundle=bundle,
            launcher=bundle / "Contents/MacOS" / LAUNCHER_NAME,
            provider=helpers / PROVIDER_NAME,
            worker_bundle=worker_bundle,
            worker=worker_bundle / "Contents/MacOS" / WORKER_NAME,
        )

    def executable_paths(self) -> tuple[Path, Path, Path]:
        return (self.provider, self.launcher, self.worker)


def _iter_plain_tree(root: Path) -> list[tuple[Path, os.stat_result]]:
    result: list[tuple[Path, os.stat_result]] = []
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise HandoffError("package") from error
        for entry in entries:
            path = Path(entry.path)
            try:
                metadata = entry.stat(follow_symlinks=False)
            except OSError as error:
                raise HandoffError("package") from error
            result.append((path, metadata))
            if len(result) > MAX_PACKAGE_ENTRIES:
                _fail("package")
            if stat.S_ISDIR(metadata.st_mode):
                pending.append(path)
            elif not stat.S_ISREG(metadata.st_mode):
                _fail("package")
    return sorted(result, key=lambda item: item[0].relative_to(root).as_posix())


def _path_name(root: Path, path: Path) -> str:
    try:
        name = path.relative_to(root).as_posix()
        encoded = name.encode("utf-8")
    except (UnicodeEncodeError, ValueError) as error:
        raise HandoffError("package") from error
    if (
        not name
        or name.startswith("/")
        or "//" in name
        or any(part in ("", ".", "..") for part in name.split("/"))
        or len(encoded) > 4096
        or any(byte < 0x20 or byte == 0x7F for byte in encoded)
    ):
        _fail("package")
    return name


def _implementation_records() -> list[dict[str, object]]:
    records = []
    for relative in IMPLEMENTATION_PATHS:
        size, digest = _sha256_file(REPOSITORY_ROOT / relative, MAX_IMPLEMENTATION_BYTES)
        records.append(
            {"name": relative.as_posix(), "sha256": digest, "size_bytes": size}
        )
    return records


def _entry_records(root: Path, owner: int, group: int) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    total = 0
    for path, metadata in _iter_plain_tree(root):
        name = _path_name(root, path)
        if name == MANIFEST_NAME:
            continue
        mode = stat.S_IMODE(metadata.st_mode)
        if (
            metadata.st_uid != owner
            or metadata.st_gid != group
            or mode & 0o222
            or stat.S_ISREG(metadata.st_mode) and metadata.st_nlink != 1
        ):
            _fail("package")
        if stat.S_ISDIR(metadata.st_mode):
            if mode != 0o555:
                _fail("package")
            records.append({"kind": "directory", "mode": mode, "name": name})
            continue
        if mode not in (0o444, 0o555):
            _fail("package")
        size, digest = _sha256_file(path, MAX_PACKAGE_BYTES)
        total += size
        if total > MAX_PACKAGE_BYTES:
            _fail("package")
        records.append(
            {
                "kind": "file",
                "mode": mode,
                "name": name,
                "sha256": digest,
                "size_bytes": size,
            }
        )
    if not records or records[0].get("name") != BUNDLE_NAME:
        _fail("package")
    return records


def _codesign(arguments: Sequence[str], category: str) -> subprocess.CompletedProcess[bytes]:
    outcome = _run_tool(("/usr/bin/codesign", *arguments), category)
    if outcome.returncode != 0:
        _fail(category)
    return outcome


def _entitlements(path: Path) -> dict[str, object]:
    raw = _codesign(("--display", "--entitlements", "-", "--xml", os.fspath(path)), "signature").stdout
    if not raw.strip():
        return {}
    try:
        value = plistlib.loads(raw)
    except (plistlib.InvalidFileException, ValueError) as error:
        raise HandoffError("signature") from error
    if not isinstance(value, dict):
        _fail("signature")
    return value


def validate_product(layout: ProductLayout) -> None:
    required = (*layout.executable_paths(), layout.worker_bundle)
    try:
        for path in required:
            metadata = os.lstat(path)
            if stat.S_ISLNK(metadata.st_mode):
                _fail("layout")
        for executable in layout.executable_paths():
            metadata = os.lstat(executable)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or not metadata.st_mode & 0o111:
                _fail("layout")
        if not stat.S_ISDIR(os.lstat(layout.worker_bundle).st_mode):
            _fail("layout")
        profile = layout.worker_bundle / "Contents/embedded.provisionprofile"
        if os.path.lexists(profile):
            _fail("entitlements")
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("layout") from error

    _codesign(("--verify", "--deep", "--strict", "--verbose=4", os.fspath(layout.bundle)), "signature")
    for path, identifier in (
        (layout.bundle, LAUNCHER_IDENTIFIER),
        (layout.provider, PROVIDER_IDENTIFIER),
        (layout.worker_bundle, WORKER_IDENTIFIER),
    ):
        details = _codesign(("--display", "--verbose=4", os.fspath(path)), "signature").stderr.decode(
            "utf-8", errors="strict"
        )
        if (
            f"Identifier={identifier}" not in details
            or "runtime" not in details.lower()
            or "Signature=adhoc" not in details
        ):
            _fail("signature")
    if _entitlements(layout.bundle) or _entitlements(layout.provider):
        _fail("entitlements")
    if _entitlements(layout.worker_bundle) != {
        "com.apple.security.app-sandbox": True,
        "com.apple.security.hypervisor": True,
    }:
        _fail("entitlements")


def _write_exclusive(path: Path, data: bytes, mode: int) -> None:
    descriptor = -1
    try:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
        descriptor = os.open(path, flags, mode)
        offset = 0
        while offset < len(data):
            count = os.write(descriptor, data[offset:])
            if count <= 0:
                _fail("publication")
            offset += count
        os.fsync(descriptor)
        os.fchmod(descriptor, mode)
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("publication") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def create_manifest(root: Path, source: SourceIdentity, owner: int, group: int) -> None:
    if not root.is_absolute() or not isinstance(source, SourceIdentity):
        _fail("invocation")
    try:
        metadata = os.lstat(root)
    except OSError as error:
        raise HandoffError("package") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != owner
        or metadata.st_gid != group
        or stat.S_IMODE(metadata.st_mode) != 0o700
        or os.path.lexists(root / MANIFEST_NAME)
    ):
        _fail("package")
    layout = ProductLayout.from_package(root)
    validate_product(layout)
    for path, child in reversed(_iter_plain_tree(root)):
        mode = 0o555 if stat.S_ISDIR(child.st_mode) or child.st_mode & 0o111 else 0o444
        try:
            os.chmod(path, mode, follow_symlinks=False)
        except OSError as error:
            raise HandoffError("package") from error
    entries = _entry_records(root, owner, group)
    document = {
        "bundle_profile": "adhoc-networkless",
        "entries": entries,
        "implementation": _implementation_records(),
        "kind": PACKAGE_KIND,
        "schema_version": SCHEMA_VERSION,
        "source": dataclasses.asdict(source),
    }
    _write_exclusive(root / MANIFEST_NAME, _canonical(document), 0o444)
    try:
        os.chmod(root, 0o500)
    except OSError as error:
        raise HandoffError("package") from error


def _parse_manifest(document: Mapping[str, object]) -> tuple[SourceIdentity, list[dict[str, object]]]:
    if set(document) != {
        "bundle_profile",
        "entries",
        "implementation",
        "kind",
        "schema_version",
        "source",
    }:
        _fail("manifest")
    if (
        document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != PACKAGE_KIND
        or document.get("bundle_profile") != "adhoc-networkless"
    ):
        _fail("manifest")
    source_value = document.get("source")
    if not isinstance(source_value, dict) or set(source_value) != {"commit", "tree"}:
        _fail("manifest")
    commit, tree = source_value.get("commit"), source_value.get("tree")
    if not isinstance(commit, str) or not isinstance(tree, str) or GIT_OBJECT_RE.fullmatch(commit) is None or GIT_OBJECT_RE.fullmatch(tree) is None:
        _fail("manifest")
    entries = document.get("entries")
    implementations = document.get("implementation")
    if not isinstance(entries, list) or not isinstance(implementations, list):
        _fail("manifest")
    if implementations != _implementation_records():
        _fail("implementation")
    return SourceIdentity(commit, tree), entries


def verify_package(root: Path, owner: int, group: int, *, root_mode: int = 0o500) -> SourceIdentity:
    if not root.is_absolute():
        _fail("invocation")
    try:
        metadata = os.lstat(root)
        manifest_metadata = os.lstat(root / MANIFEST_NAME)
    except OSError as error:
        raise HandoffError("package") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != owner
        or metadata.st_gid != group
        or stat.S_IMODE(metadata.st_mode) != root_mode
        or not stat.S_ISREG(manifest_metadata.st_mode)
        or stat.S_ISLNK(manifest_metadata.st_mode)
        or manifest_metadata.st_nlink != 1
        or manifest_metadata.st_uid != owner
        or manifest_metadata.st_gid != group
        or stat.S_IMODE(manifest_metadata.st_mode) != 0o444
    ):
        _fail("package")
    document = _load_canonical_json(root / MANIFEST_NAME, MAX_MANIFEST_BYTES)
    source, expected_entries = _parse_manifest(document)
    actual_entries = _entry_records(root, owner, group)
    if actual_entries != expected_entries:
        _fail("manifest")
    validate_product(ProductLayout.from_package(root))
    return source


def _publish_exclusive(source: Path, destination: Path) -> None:
    if sys.platform != "darwin":
        _fail("platform")
    library = ctypes.CDLL(None, use_errno=True)
    renamex = library.renamex_np
    renamex.argtypes = (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint)
    renamex.restype = ctypes.c_int
    result = renamex(os.fsencode(source), os.fsencode(destination), RENAME_EXCL)
    if result != 0:
        _fail("publication")


def _remove_tree(path: Path) -> None:
    try:
        if os.path.lexists(path):
            metadata = os.lstat(path)
            if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
                _fail("cleanup")
            os.chmod(path, 0o700)
            for child, child_metadata in _iter_plain_tree(path):
                if stat.S_ISDIR(child_metadata.st_mode):
                    os.chmod(child, 0o700, follow_symlinks=False)
            shutil.rmtree(path)
        if os.path.lexists(path):
            _fail("cleanup")
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("cleanup") from error


def prepare_package(output: Path) -> None:
    if (
        not output.is_absolute()
        or output.name != PACKAGE_KIND
        or os.path.lexists(output)
        or sys.platform != "darwin"
        or platform.machine() != "arm64"
        or os.getuid() == 0
        or os.geteuid() == 0
    ):
        _fail("invocation")
    parent = output.parent
    try:
        parent_metadata = os.lstat(parent)
    except OSError as error:
        raise HandoffError("invocation") from error
    if not stat.S_ISDIR(parent_metadata.st_mode) or stat.S_ISLNK(parent_metadata.st_mode):
        _fail("invocation")
    source = read_clean_source_identity()
    stage = Path(tempfile.mkdtemp(prefix=f".{PACKAGE_KIND}.stage.", dir=parent))
    published = False
    try:
        outcome = _run_tool(
            (os.fspath(BUILD_BUNDLE), "--output", os.fspath(stage / BUNDLE_NAME)),
            "build",
            timeout=1800.0,
            maximum=1024 * 1024,
            environment=os.environ,
        )
        if outcome.returncode != 0:
            _fail("build")
        if read_clean_source_identity() != source:
            _fail("source")
        create_manifest(stage, source, os.getuid(), os.getgid())
        verify_package(stage, os.getuid(), os.getgid())
        if read_clean_source_identity() != source or os.path.lexists(output):
            _fail("source")
        _publish_exclusive(stage, output)
        published = True
    finally:
        if not published and os.path.lexists(stage):
            _remove_tree(stage)


class Role(enum.IntEnum):
    SUPERVISOR = 1
    CONTROLLER = 2


class Kind(enum.IntEnum):
    WELCOME = 1
    READY = 2
    SPAWN = 10
    SPAWNED = 11
    POLL = 12
    WAIT = 13
    RUNNING = 14
    EXITED = 15
    TERM = 16
    KILL = 17
    CLOSE = 18
    CLOSED = 19
    FINISH = 20
    FINISHED = 21
    FAILURE = 22


@dataclasses.dataclass(frozen=True)
class Record:
    role: Role
    kind: Kind
    sequence: int
    correlation: int
    session: bytes
    handle: int = 0
    value: int = 0
    payload: bytes = b""
    descriptor_count: int = 0

    def encode(self) -> bytes:
        if (
            not isinstance(self.role, Role)
            or not isinstance(self.kind, Kind)
            or not 1 <= self.sequence <= 0xFFFF_FFFF_FFFF_FFFF
            or not 0 <= self.correlation <= 0xFFFF_FFFF_FFFF_FFFF
            or len(self.session) != 32
            or not 0 <= self.handle <= 0xFFFF_FFFF_FFFF_FFFF
            or not -(1 << 63) <= self.value < (1 << 63)
            or not isinstance(self.payload, bytes)
            or len(self.payload) > MAX_RECORD_PAYLOAD
            or not 0 <= self.descriptor_count <= MAX_ANCILLARY_DESCRIPTORS
        ):
            _fail("protocol")
        prefix = RECORD_PREFIX.pack(
            RECORD_MAGIC,
            RECORD_VERSION,
            int(self.role),
            int(self.kind),
            self.descriptor_count,
            self.sequence,
            self.correlation,
            self.session,
            self.handle,
            self.value,
            len(self.payload),
            bytes(12),
        )
        digest = hashlib.sha256(prefix + self.payload).digest()
        return prefix + digest + self.payload + bytes(MAX_RECORD_PAYLOAD - len(self.payload))

    @classmethod
    def decode(cls, raw: bytes) -> "Record":
        if len(raw) != RECORD_BYTES:
            _fail("protocol")
        try:
            (
                magic,
                version,
                role,
                kind,
                descriptor_count,
                sequence,
                correlation,
                session,
                handle,
                value,
                payload_length,
                reserved,
            ) = RECORD_PREFIX.unpack(raw[:DIGEST_OFFSET])
            decoded_role = Role(role)
            decoded_kind = Kind(kind)
        except (ValueError, struct.error) as error:
            raise HandoffError("protocol") from error
        if (
            magic != RECORD_MAGIC
            or version != RECORD_VERSION
            or reserved != bytes(12)
            or sequence == 0
            or payload_length > MAX_RECORD_PAYLOAD
            or descriptor_count > MAX_ANCILLARY_DESCRIPTORS
        ):
            _fail("protocol")
        payload = raw[PAYLOAD_OFFSET : PAYLOAD_OFFSET + payload_length]
        if raw[PAYLOAD_OFFSET + payload_length :] != bytes(MAX_RECORD_PAYLOAD - payload_length):
            _fail("protocol")
        expected = hashlib.sha256(raw[:DIGEST_OFFSET] + payload).digest()
        if not secrets.compare_digest(raw[DIGEST_OFFSET:PAYLOAD_OFFSET], expected):
            _fail("protocol")
        return cls(
            decoded_role,
            decoded_kind,
            sequence,
            correlation,
            session,
            handle,
            value,
            payload,
            descriptor_count,
        )


def _close_descriptors(descriptors: Iterable[int]) -> None:
    for descriptor in descriptors:
        try:
            os.close(descriptor)
        except OSError:
            pass


class RecordSocket:
    def __init__(self, connection: socket.socket) -> None:
        if connection.family != socket.AF_UNIX or connection.type & 0xF != socket.SOCK_DGRAM:
            _fail("descriptor")
        self.connection = connection
        try:
            self.connection.setsockopt(
                socket.SOL_SOCKET, socket.SO_SNDBUF, SOCKET_BUFFER_BYTES
            )
            self.connection.setsockopt(
                socket.SOL_SOCKET, socket.SO_RCVBUF, SOCKET_BUFFER_BYTES
            )
            if (
                self.connection.getsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF)
                < SOCKET_BUFFER_BYTES
                or self.connection.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
                < SOCKET_BUFFER_BYTES
            ):
                _fail("descriptor")
            self.connection.settimeout(PROTOCOL_TIMEOUT)
        except HandoffError:
            raise
        except OSError as error:
            raise HandoffError("descriptor") from error

    def close(self) -> None:
        try:
            self.connection.close()
        except OSError:
            pass

    def send(self, record: Record, descriptors: Sequence[int] = ()) -> None:
        if len(descriptors) != record.descriptor_count:
            _fail("descriptor")
        ancillary = []
        if descriptors:
            encoded = array.array("i", descriptors)
            ancillary.append((socket.SOL_SOCKET, socket.SCM_RIGHTS, encoded))
        try:
            sent = self.connection.sendmsg((record.encode(),), ancillary)
        except (OSError, socket.timeout) as error:
            raise HandoffError("protocol-timeout" if isinstance(error, socket.timeout) else "protocol") from error
        if sent != RECORD_BYTES:
            _fail("protocol")

    def receive(self) -> tuple[Record, list[int]]:
        descriptors: list[int] = []
        rights_messages = 0
        try:
            raw, ancillary, flags, _address = self.connection.recvmsg(
                RECORD_BYTES + 1,
                socket.CMSG_SPACE(MAX_ANCILLARY_DESCRIPTORS * array.array("i").itemsize),
            )
            for level, kind, data in ancillary:
                if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS:
                    _fail("descriptor")
                values = array.array("i")
                if not data or len(data) % values.itemsize:
                    _fail("descriptor")
                rights_messages += 1
                usable = len(data) - len(data) % values.itemsize
                values.frombytes(data[:usable])
                descriptors.extend(values)
            if flags & (getattr(socket, "MSG_TRUNC", 0) | getattr(socket, "MSG_CTRUNC", 0)):
                _fail("descriptor")
            record = Record.decode(raw)
            if (
                record.descriptor_count != len(descriptors)
                or rights_messages != (1 if descriptors else 0)
            ):
                _fail("descriptor")
            return record, descriptors
        except HandoffError:
            _close_descriptors(descriptors)
            raise
        except socket.timeout as error:
            _close_descriptors(descriptors)
            raise HandoffError("protocol-timeout") from error
        except OSError as error:
            _close_descriptors(descriptors)
            raise HandoffError("protocol") from error


class SessionSocket:
    def __init__(self, transport: RecordSocket, role: Role, session: Optional[bytes] = None) -> None:
        self.transport = transport
        self.role = role
        self.peer = Role.CONTROLLER if role == Role.SUPERVISOR else Role.SUPERVISOR
        self.session = session
        self.send_sequence = 1
        self.receive_sequence = 1
        self.terminal = False

    def send(
        self,
        kind: Kind,
        *,
        correlation: int = 0,
        handle: int = 0,
        value: int = 0,
        payload: bytes = b"",
        descriptors: Sequence[int] = (),
    ) -> int:
        if self.terminal or self.session is None:
            _fail("protocol")
        sequence = self.send_sequence
        record = Record(
            self.role,
            kind,
            sequence,
            correlation,
            self.session,
            handle,
            value,
            payload,
            len(descriptors),
        )
        self.transport.send(record, descriptors)
        if sequence == 0xFFFF_FFFF_FFFF_FFFF:
            self.terminal = True
        else:
            self.send_sequence += 1
        return sequence

    def receive(self, *, allow_unbound_welcome: bool = False) -> tuple[Record, list[int]]:
        if self.terminal:
            _fail("protocol")
        record, descriptors = self.transport.receive()
        if (
            record.role != self.peer
            or record.sequence != self.receive_sequence
            or (
                self.session is not None
                and not secrets.compare_digest(record.session, self.session)
            )
        ):
            _close_descriptors(descriptors)
            _fail("protocol")
        if self.session is None:
            if not allow_unbound_welcome or record.kind != Kind.WELCOME:
                _close_descriptors(descriptors)
                _fail("protocol")
            self.session = record.session
        if record.sequence == 0xFFFF_FFFF_FFFF_FFFF:
            self.terminal = True
        else:
            self.receive_sequence += 1
        return record, descriptors


def encode_arguments(arguments: Sequence[bytes]) -> bytes:
    if not arguments or len(arguments) > MAX_ARGUMENTS:
        _fail("arguments")
    payload = bytearray(struct.pack("!H", len(arguments)))
    for argument in arguments:
        if (
            not isinstance(argument, bytes)
            or not argument
            or len(argument) > MAX_ARGUMENT_BYTES
            or b"\x00" in argument
        ):
            _fail("arguments")
        payload.extend(struct.pack("!H", len(argument)))
        payload.extend(argument)
    if len(payload) > MAX_ARGUMENT_PAYLOAD:
        _fail("arguments")
    return bytes(payload)


def decode_arguments(payload: bytes) -> tuple[bytes, ...]:
    if not 2 <= len(payload) <= MAX_ARGUMENT_PAYLOAD:
        _fail("arguments")
    try:
        count = struct.unpack_from("!H", payload)[0]
    except struct.error as error:
        raise HandoffError("arguments") from error
    if not 1 <= count <= MAX_ARGUMENTS:
        _fail("arguments")
    result = []
    offset = 2
    for _index in range(count):
        if offset + 2 > len(payload):
            _fail("arguments")
        length = struct.unpack_from("!H", payload, offset)[0]
        offset += 2
        if not 1 <= length <= MAX_ARGUMENT_BYTES or offset + length > len(payload):
            _fail("arguments")
        value = payload[offset : offset + length]
        if b"\x00" in value:
            _fail("arguments")
        result.append(value)
        offset += length
    if offset != len(payload):
        _fail("arguments")
    return tuple(result)


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


@dataclasses.dataclass(frozen=True)
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

    def same_process(self, other: "ProcessIdentity") -> bool:
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


def _libproc() -> ctypes.CDLL:
    try:
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    except OSError as error:
        raise HandoffError("identity") from error
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
    if sys.platform != "darwin" or not 1 < pid <= 0x7FFF_FFFF:
        _fail("identity")
    library = _libproc()
    info = ProcBsdInfo()
    size = ctypes.sizeof(info)
    if library.proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, ctypes.byref(info), size) != size:
        _fail("identity")
    path_buffer = ctypes.create_string_buffer(PROC_PIDPATHINFO_MAXSIZE)
    path_length = library.proc_pidpath(pid, path_buffer, PROC_PIDPATHINFO_MAXSIZE)
    if path_length <= 0 or path_length >= PROC_PIDPATHINFO_MAXSIZE:
        _fail("identity")
    try:
        executable = Path(path_buffer.raw[:path_length].decode("utf-8"))
        session_id = os.getsid(pid)
    except (OSError, UnicodeDecodeError) as error:
        raise HandoffError("identity") from error
    values = (
        int(info.pid),
        int(info.ppid),
        int(info.pgid),
        int(session_id),
        int(info.uid),
        int(info.gid),
        int(info.ruid),
        int(info.rgid),
        int(info.svuid),
        int(info.svgid),
        int(info.start_seconds),
        int(info.start_microseconds),
    )
    if values[0] != pid or any(value < 0 for value in values) or not executable.is_absolute():
        _fail("identity")
    return ProcessIdentity(*values, executable)


class CredentialBackend:
    """Injectable credential operations for the post-fork controller transition."""

    def groups(self) -> list[int]:
        return _process_groups()

    def clear_groups(self) -> None:
        os.setgroups([])

    def set_gid(self, value: int) -> None:
        os.setgid(value)

    def set_uid(self, value: int) -> None:
        os.setuid(value)

    def restore_groups(self) -> None:
        os.setgroups([0])

    def identity(self) -> ProcessIdentity:
        return capture_process(os.getpid())


def _read_process_groups(getgroups: Any) -> list[int]:
    count = getgroups(0, None)
    if count < 0:
        raise OSError(ctypes.get_errno(), "getgroups")
    if count > MAX_CREDENTIAL_GROUPS:
        raise OSError(errno.EOVERFLOW, "getgroups")
    if count == 0:
        return []
    groups = (ctypes.c_uint32 * count)()
    actual = getgroups(count, groups)
    if actual < 0:
        raise OSError(ctypes.get_errno(), "getgroups")
    if actual != count:
        raise OSError(errno.EIO, "getgroups")
    return [int(groups[index]) for index in range(actual)]


def _process_groups() -> list[int]:
    # Xcode's system Python is linked to getgroups$DARWIN_EXTSN, which reports
    # the account-directory access list and ignores setgroups(2). Resolving the
    # unversioned ABI explicitly observes the bounded live process group list.
    library = ctypes.CDLL(None, use_errno=True)
    getgroups = library.getgroups
    getgroups.argtypes = (ctypes.c_int, ctypes.POINTER(ctypes.c_uint32))
    getgroups.restype = ctypes.c_int
    return _read_process_groups(getgroups)


def transition_controller_credentials(
    target_uid: int,
    target_gid: int,
    supervisor_pid: int,
    backend: Optional[CredentialBackend] = None,
) -> ProcessIdentity:
    if target_uid == 0 or target_gid == 0 or supervisor_pid <= 1:
        _fail("credentials")
    operations = backend if backend is not None else CredentialBackend()
    try:
        initial = operations.identity()
    except HandoffError as error:
        raise HandoffError("credentials-initial-observe") from error
    if initial.pid != os.getpid() or initial.parent_pid != supervisor_pid:
        _fail("credentials-initial-process")
    if any(
            value != 0
            for value in (
                initial.uid,
                initial.gid,
                initial.real_uid,
                initial.real_gid,
                initial.saved_uid,
                initial.saved_gid,
            )
        ):
        _fail("credentials-initial-root")
    try:
        operations.clear_groups()
    except OSError as error:
        raise HandoffError("credentials-clear-groups") from error
    try:
        cleared_groups = operations.groups()
    except OSError as error:
        raise HandoffError("credentials-cleared-groups") from error
    if cleared_groups != [0]:
        _fail("credentials-cleared-groups")
    try:
        operations.set_gid(target_gid)
    except OSError as error:
        raise HandoffError("credentials-set-gid") from error
    try:
        operations.set_uid(target_uid)
    except OSError as error:
        raise HandoffError("credentials-set-uid") from error
    try:
        dropped = operations.identity()
    except HandoffError as error:
        raise HandoffError("credentials-dropped-observe") from error
    if not initial.same_process(dropped) or dropped.parent_pid != supervisor_pid:
        _fail("credentials-dropped-process")
    try:
        dropped_groups = operations.groups()
    except OSError as error:
        raise HandoffError("credentials-dropped-groups") from error
    if dropped_groups != [target_gid]:
        _fail("credentials-dropped-groups")
    if (
        dropped.uid,
        dropped.real_uid,
        dropped.saved_uid,
    ) != (target_uid, target_uid, target_uid):
        _fail("credentials-dropped-uid")
    if (
        dropped.gid,
        dropped.real_gid,
        dropped.saved_gid,
    ) != (target_gid, target_gid, target_gid):
        _fail("credentials-dropped-gid")
    for label, restore in (
        ("uid", lambda: operations.set_uid(0)),
        ("gid", lambda: operations.set_gid(0)),
        ("groups", operations.restore_groups),
    ):
        try:
            restore()
        except OSError:
            pass
        else:
            _fail(f"credentials-restore-{label}")
        try:
            current = operations.identity()
        except HandoffError as error:
            raise HandoffError("credentials-restored-observe") from error
        try:
            current_groups = operations.groups()
        except OSError as error:
            raise HandoffError("credentials-restored-state") from error
        if current != dropped or current_groups != [target_gid]:
            _fail("credentials-restored-state")
    return dropped


@dataclasses.dataclass(frozen=True)
class ProcessRecord:
    pid: int
    parent_pid: int
    state: str
    command: str


def _parse_process_table(raw: str) -> dict[int, ProcessRecord]:
    records: dict[int, ProcessRecord] = {}
    for line in raw.splitlines():
        fields = line.strip().split(maxsplit=3)
        if len(fields) != 4:
            continue
        try:
            pid, parent = int(fields[0]), int(fields[1])
        except ValueError:
            continue
        if pid <= 0 or parent < 0 or pid in records or not fields[2] or not fields[3]:
            continue
        records[pid] = ProcessRecord(pid, parent, fields[2], fields[3])
    return records


def _process_table() -> dict[int, ProcessRecord]:
    outcome = _run_tool(
        ("/bin/ps", "-axo", "pid=,ppid=,state=,comm="),
        "process",
        timeout=5.0,
    )
    if outcome.returncode != 0 or outcome.stderr:
        _fail("process")
    try:
        return _parse_process_table(outcome.stdout.decode("utf-8", errors="strict"))
    except UnicodeDecodeError as error:
        raise HandoffError("process") from error


@dataclasses.dataclass(frozen=True)
class Stage:
    root: Path
    package: Path
    layout: ProductLayout
    device: int
    inode: int


def _normalize_root_copy(root: Path) -> None:
    try:
        for path, metadata in reversed(_iter_plain_tree(root)):
            os.lchown(path, 0, 0)
            mode = 0o555 if stat.S_ISDIR(metadata.st_mode) or metadata.st_mode & 0o111 else 0o444
            os.chmod(path, mode, follow_symlinks=False)
        os.lchown(root, 0, 0)
        os.chmod(root, 0o555)
    except OSError as error:
        raise HandoffError("staging") from error


def stage_package(prepared: Path, uid: int, gid: int) -> Stage:
    expected_source = verify_package(prepared, uid, gid)
    try:
        root = Path(
            tempfile.mkdtemp(
                prefix="bangbang-elevated-handoff.", dir="/private/var/tmp"
            )
        )
        package = root / "package"
        shutil.copytree(prepared, package, symlinks=True, copy_function=shutil.copy2)
        _normalize_root_copy(package)
        if verify_package(prepared, uid, gid) != expected_source:
            _fail("staging")
        if verify_package(package, 0, 0, root_mode=0o555) != expected_source:
            _fail("staging")
        layout = ProductLayout.from_package(package)
        validate_product(layout)
        root_metadata = os.lstat(root)
        if (
            not stat.S_ISDIR(root_metadata.st_mode)
            or stat.S_ISLNK(root_metadata.st_mode)
            or root_metadata.st_uid != 0
            or root_metadata.st_gid != 0
            or stat.S_IMODE(root_metadata.st_mode) != 0o700
        ):
            _fail("staging")
        os.chmod(root, 0o711)
        return Stage(root, package, layout, root_metadata.st_dev, root_metadata.st_ino)
    except HandoffError:
        if "root" in locals() and os.path.lexists(root):
            try:
                _remove_tree(root)
            except HandoffError:
                pass
        raise
    except OSError as error:
        if "root" in locals() and os.path.lexists(root):
            try:
                _remove_tree(root)
            except HandoffError:
                pass
        raise HandoffError("staging") from error


def _stage_process_ids(stage: Stage, records: Mapping[int, ProcessRecord]) -> set[int]:
    exact = {os.fspath(path) for path in stage.layout.executable_paths()}
    return {
        record.pid
        for record in records.values()
        if record.command in exact
    }


def _wait_until(predicate: Callable[[], bool], timeout: float = CLEANUP_TIMEOUT) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(POLL_SECONDS)
    return predicate()


def _retire_pid(identity: ProcessIdentity) -> bool:
    forced = False
    try:
        current = capture_process(identity.pid)
    except HandoffError:
        if identity.pid not in _process_table():
            return forced
        _fail("cleanup")
    if not current.same_process(identity):
        _fail("cleanup")
    for value in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(identity.pid, value)
            forced = True
        except ProcessLookupError:
            return forced
        except OSError as error:
            raise HandoffError("cleanup") from error
        if _wait_until(
            lambda: _process_identity_absent(identity),
            timeout=2.0,
        ):
            return forced
    _fail("cleanup")


def _process_identity_absent(identity: ProcessIdentity) -> bool:
    try:
        current = capture_process(identity.pid)
    except HandoffError:
        if identity.pid not in _process_table():
            return True
        _fail("cleanup")
    if not current.same_process(identity):
        _fail("cleanup")
    return False


def force_stage_process_cleanup(stage: Stage) -> bool:
    forced = False
    for value in (signal.SIGTERM, signal.SIGKILL):
        identifiers = _stage_process_ids(stage, _process_table())
        if not identifiers:
            return forced
        for pid in sorted(identifiers, reverse=True):
            try:
                os.kill(pid, value)
                forced = True
            except ProcessLookupError:
                pass
            except OSError as error:
                raise HandoffError("cleanup") from error
        if _wait_until(lambda: not _stage_process_ids(stage, _process_table()), timeout=2.0):
            return forced
    _fail("cleanup")


def remove_stage(stage: Stage) -> None:
    try:
        metadata = os.lstat(stage.root)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_dev != stage.device
            or metadata.st_ino != stage.inode
            or metadata.st_uid != 0
            or metadata.st_gid != 0
            or stat.S_IMODE(metadata.st_mode) not in (0o700, 0o711)
            or _stage_process_ids(stage, _process_table())
        ):
            _fail("cleanup")
        os.chmod(stage.root, 0o700)
        shutil.rmtree(stage.root)
        if os.path.lexists(stage.root):
            _fail("cleanup")
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("cleanup") from error


def _validate_output_descriptor(descriptor: int) -> None:
    try:
        metadata = os.fstat(descriptor)
        flags = fcntl.fcntl(descriptor, fcntl.F_GETFL)
        descriptor_flags = fcntl.fcntl(descriptor, fcntl.F_GETFD)
        if not stat.S_ISFIFO(metadata.st_mode) or flags & os.O_ACCMODE != os.O_RDONLY:
            _fail("descriptor")
        if not descriptor_flags & fcntl.FD_CLOEXEC:
            fcntl.fcntl(descriptor, fcntl.F_SETFD, descriptor_flags | fcntl.FD_CLOEXEC)
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("descriptor") from error


@dataclasses.dataclass
class OwnedProvider:
    process: subprocess.Popen[bytes]
    handle: int


def _provider_group_has_live_members(process_group: int) -> bool:
    if process_group <= 1 or os.geteuid() != 0:
        _fail("cleanup")
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # On Darwin an unreaped session leader with no other live members
        # yields EPERM even to root. Keeping that exact child unreaped pins the
        # PGID while this probe distinguishes live descendants from the
        # provider zombie.
        return False
    except OSError as error:
        raise HandoffError("cleanup") from error
    return True


def _wait_provider_group_quiescent(
    process_group: int, timeout: float = PROVIDER_SIGNAL_GRACE
) -> bool:
    return _wait_until(
        lambda: not _provider_group_has_live_members(process_group), timeout
    )


def _provider_group_absent(process_group: int) -> bool:
    if process_group <= 1 or os.geteuid() != 0:
        _fail("cleanup")
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return True
    except OSError as error:
        raise HandoffError("cleanup") from error
    return False


def _send_group_cleanup_signal(process_group: int, value: signal.Signals) -> None:
    if not _provider_group_has_live_members(process_group):
        return
    try:
        os.killpg(process_group, value)
    except (ProcessLookupError, PermissionError):
        if _provider_group_has_live_members(process_group):
            _fail("cleanup")
    except OSError as error:
        raise HandoffError("cleanup") from error


def _signal_provider(provider: OwnedProvider, kind: Kind) -> int:
    process = provider.process
    if process.returncode is not None or kind not in (Kind.TERM, Kind.KILL):
        _fail("signal")
    try:
        if os.getpgid(process.pid) != process.pid:
            _fail("signal")
        if kind == Kind.TERM:
            os.killpg(process.pid, signal.SIGTERM)
        else:
            os.kill(process.pid, signal.SIGKILL)
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("signal") from error

    quiescent = _wait_provider_group_quiescent(
        process.pid,
        PROVIDER_SIGNAL_GRACE
        if kind == Kind.TERM
        else PROVIDER_PARENT_LOSS_GRACE,
    )
    if not quiescent and kind == Kind.KILL:
        _send_group_cleanup_signal(process.pid, signal.SIGTERM)
        quiescent = _wait_provider_group_quiescent(process.pid)
    if not quiescent:
        _send_group_cleanup_signal(process.pid, signal.SIGKILL)
        quiescent = _wait_provider_group_quiescent(process.pid)
    if not quiescent:
        _fail("cleanup")

    try:
        status = process.wait(timeout=PROVIDER_SIGNAL_GRACE)
    except subprocess.TimeoutExpired as error:
        raise HandoffError("cleanup") from error
    if kind == Kind.KILL and status != -signal.SIGKILL:
        _fail("signal")
    if not _wait_until(
        lambda: _provider_group_absent(process.pid), PROVIDER_SIGNAL_GRACE
    ):
        _fail("cleanup")
    return status


def _terminate_provider(provider: OwnedProvider) -> bool:
    process = provider.process
    if process.returncode is not None:
        if not _wait_until(
            lambda: _provider_group_absent(process.pid), PROVIDER_SIGNAL_GRACE
        ):
            _fail("cleanup")
        return False
    try:
        if os.getpgid(process.pid) != process.pid:
            _fail("cleanup")
    except ProcessLookupError:
        status = process.poll()
        if status is None or not _provider_group_absent(process.pid):
            _fail("cleanup")
        return False
    except OSError as error:
        raise HandoffError("cleanup") from error
    forced = False
    if _provider_group_has_live_members(process.pid):
        _send_group_cleanup_signal(process.pid, signal.SIGTERM)
        forced = True
    quiescent = _wait_provider_group_quiescent(process.pid)
    if not quiescent:
        _send_group_cleanup_signal(process.pid, signal.SIGKILL)
        forced = True
        quiescent = _wait_provider_group_quiescent(process.pid)
    if not quiescent:
        _fail("cleanup")
    try:
        process.wait(timeout=PROVIDER_SIGNAL_GRACE)
    except subprocess.TimeoutExpired as error:
        raise HandoffError("cleanup") from error
    if not _wait_until(
        lambda: _provider_group_absent(process.pid), PROVIDER_SIGNAL_GRACE
    ):
        _fail("cleanup")
    return forced


class ProviderSupervisor:
    def __init__(
        self,
        session: SessionSocket,
        layout: ProductLayout,
        uid: int,
        gid: int,
        controller_pid: int,
        controller_identity: ProcessIdentity,
        guardian_lease: int,
    ) -> None:
        self.session = session
        self.layout = layout
        self.uid = uid
        self.gid = gid
        self.controller_pid = controller_pid
        self.controller_identity = controller_identity
        self.guardian_lease = guardian_lease
        self.providers: dict[int, OwnedProvider] = {}
        self.next_handle = 1
        self.controller_status: Optional[int] = None

    def _guardian_alive(self) -> bool:
        try:
            readable, _writable, _exceptional = select.select(
                [self.guardian_lease], [], [], 0
            )
            if not readable:
                return True
            return bool(os.read(self.guardian_lease, 1))
        except OSError as error:
            raise HandoffError("guardian") from error

    def _controller_alive(self) -> bool:
        if self.controller_status is not None:
            return False
        try:
            pid, status = os.waitpid(self.controller_pid, os.WNOHANG)
        except ChildProcessError:
            return False
        except OSError as error:
            raise HandoffError("controller") from error
        if pid == 0:
            try:
                current = capture_process(self.controller_pid)
            except HandoffError:
                return False
            return current.same_process(self.controller_identity)
        self.controller_status = status
        return False

    def _spawn(self, arguments: tuple[bytes, ...]) -> tuple[OwnedProvider, list[int]]:
        if self.next_handle > 0xFFFF_FFFF or len(self.providers) >= 8:
            _fail("spawn")
        stdout_read = stdout_write = stderr_read = stderr_write = -1
        process: Optional[subprocess.Popen[bytes]] = None
        transferred = False
        try:
            stdout_read, stdout_write = os.pipe()
            stderr_read, stderr_write = os.pipe()
            for descriptor in (stdout_read, stdout_write, stderr_read, stderr_write):
                flags = fcntl.fcntl(descriptor, fcntl.F_GETFD)
                fcntl.fcntl(descriptor, fcntl.F_SETFD, flags | fcntl.FD_CLOEXEC)
            command = (
                os.fsencode(self.layout.provider),
                b"--bootstrap-v1",
                b"--target-uid",
                str(self.uid).encode("ascii"),
                b"--target-gid",
                str(self.gid).encode("ascii"),
                b"--",
                *arguments,
            )
            process = subprocess.Popen(
                command,
                cwd=b"/",
                env=FIXED_ENVIRONMENT,
                stdin=subprocess.DEVNULL,
                stdout=stdout_write,
                stderr=stderr_write,
                start_new_session=True,
                close_fds=True,
            )
            os.close(stdout_write)
            stdout_write = -1
            os.close(stderr_write)
            stderr_write = -1
            if process.pid <= 1 or os.getpgid(process.pid) != process.pid:
                _fail("spawn")
            handle = self.next_handle
            self.next_handle += 1
            owned = OwnedProvider(process, handle)
            self.providers[handle] = owned
            transferred = True
            return owned, [stdout_read, stderr_read]
        except HandoffError:
            if process is not None:
                _terminate_provider(OwnedProvider(process, 0))
            raise
        except OSError as error:
            if process is not None:
                try:
                    _terminate_provider(OwnedProvider(process, 0))
                except HandoffError:
                    pass
            raise HandoffError("spawn") from error
        finally:
            _close_descriptors(
                descriptor
                for descriptor in (stdout_write, stderr_write)
                if descriptor >= 0
            )
            if not transferred:
                _close_descriptors(
                    descriptor
                    for descriptor in (stdout_read, stderr_read)
                    if descriptor >= 0
                )

    def _status(self, owned: OwnedProvider) -> Optional[int]:
        status = owned.process.poll()
        if status is not None and not _wait_until(
            lambda: _provider_group_absent(owned.process.pid),
            PROVIDER_SIGNAL_GRACE,
        ):
            _fail("cleanup")
        return status

    def _wait_status(self, owned: OwnedProvider, milliseconds: int) -> Optional[int]:
        if not 1 <= milliseconds <= 1000:
            _fail("protocol")
        deadline = time.monotonic() + milliseconds / 1000.0
        while time.monotonic() < deadline:
            status = owned.process.poll()
            if status is not None:
                return status
            if not self._guardian_alive() or not self._controller_alive():
                _fail("lease")
            time.sleep(min(POLL_SECONDS, max(0.0, deadline - time.monotonic())))
        return owned.process.poll()

    def _reply_status(self, request: Record, owned: OwnedProvider, status: Optional[int]) -> None:
        self.session.send(
            Kind.RUNNING if status is None else Kind.EXITED,
            correlation=request.sequence,
            handle=owned.handle,
            value=0 if status is None else status,
        )

    def _require_handle(self, request: Record) -> OwnedProvider:
        if request.payload or request.descriptor_count or request.handle == 0:
            _fail("protocol")
        try:
            return self.providers[request.handle]
        except KeyError as error:
            raise HandoffError("protocol") from error

    def cleanup(self) -> bool:
        forced = False
        for handle in sorted(tuple(self.providers), reverse=True):
            owned = self.providers.pop(handle)
            forced = _terminate_provider(owned) or forced
        return forced

    def serve(self) -> None:
        deadline = time.monotonic() + SESSION_TIMEOUT
        try:
            while True:
                if not self._guardian_alive() or not self._controller_alive():
                    _fail("lease")
                if time.monotonic() >= deadline:
                    _fail("session-timeout")
                try:
                    request, descriptors = self.session.receive()
                except HandoffError as error:
                    if error.category == "protocol-timeout":
                        continue
                    raise
                if descriptors or request.correlation != 0:
                    _close_descriptors(descriptors)
                    _fail("protocol")
                if request.kind == Kind.SPAWN:
                    if request.handle or request.value:
                        _fail("protocol")
                    arguments = decode_arguments(request.payload)
                    owned, outputs = self._spawn(arguments)
                    try:
                        self.session.send(
                            Kind.SPAWNED,
                            correlation=request.sequence,
                            handle=owned.handle,
                            value=owned.process.pid,
                            descriptors=outputs,
                        )
                    except HandoffError:
                        _terminate_provider(owned)
                        self.providers.pop(owned.handle, None)
                        raise
                    finally:
                        _close_descriptors(outputs)
                elif request.kind == Kind.FAILURE:
                    if (
                        request.handle
                        or request.correlation
                        or request.payload
                        or not 1 <= request.value <= len(CONTROLLER_FAILURES)
                    ):
                        _fail("protocol")
                    _fail(f"controller-{CONTROLLER_FAILURES[request.value - 1]}")
                elif request.kind in (Kind.POLL, Kind.WAIT):
                    owned = self._require_handle(request)
                    if request.kind == Kind.POLL:
                        if request.value != 0:
                            _fail("protocol")
                        status = self._status(owned)
                    else:
                        status = self._wait_status(owned, request.value)
                    self._reply_status(request, owned, status)
                elif request.kind in (Kind.TERM, Kind.KILL):
                    owned = self._require_handle(request)
                    if request.value != 0:
                        _fail("protocol")
                    status = _signal_provider(owned, request.kind)
                    self._reply_status(request, owned, status)
                elif request.kind == Kind.CLOSE:
                    owned = self._require_handle(request)
                    if request.value != 0:
                        _fail("protocol")
                    _terminate_provider(owned)
                    status = owned.process.poll()
                    if status is None:
                        _fail("cleanup")
                    self.providers.pop(owned.handle)
                    self.session.send(
                        Kind.CLOSED,
                        correlation=request.sequence,
                        handle=owned.handle,
                        value=status,
                    )
                elif request.kind == Kind.FINISH:
                    if request.handle or request.value or request.payload or self.providers:
                        _fail("protocol")
                    self.session.send(Kind.FINISHED, correlation=request.sequence)
                    return
                else:
                    _fail("protocol")
        except BaseException:
            try:
                self.cleanup()
            except HandoffError:
                pass
            raise


class _BoundedCapture:
    def __init__(self, maximum: int) -> None:
        self.maximum = maximum
        self.data = bytearray()
        self.overflow = False
        self.failure = False
        self.lock = threading.Lock()

    def append(self, value: bytes) -> None:
        with self.lock:
            remaining = max(0, self.maximum - len(self.data))
            self.data.extend(value[:remaining])
            if len(value) > remaining:
                self.overflow = True

    def fail(self) -> None:
        with self.lock:
            self.failure = True

    def result(self) -> tuple[bytes, bool, bool]:
        with self.lock:
            return bytes(self.data), self.overflow, self.failure


def _pump_descriptor(descriptor: int, capture: _BoundedCapture) -> None:
    try:
        while True:
            chunk = os.read(descriptor, 16 * 1024)
            if not chunk:
                return
            capture.append(chunk)
    except OSError:
        capture.fail()


class ControllerProxy:
    def __init__(self, session: SessionSocket, layout: ProductLayout) -> None:
        self.session = session
        self.layout = layout
        self.processes: set["RemoteProviderProcess"] = set()
        self.finished = False

    def _exchange(
        self,
        kind: Kind,
        *,
        handle: int = 0,
        value: int = 0,
        payload: bytes = b"",
    ) -> tuple[Record, list[int]]:
        if self.finished:
            _fail("protocol")
        sequence = self.session.send(
            kind, handle=handle, value=value, payload=payload
        )
        response, descriptors = self.session.receive()
        if response.correlation != sequence or response.kind == Kind.FAILURE:
            _close_descriptors(descriptors)
            _fail("protocol")
        return response, descriptors

    def spawn(self, arguments: Sequence[str]) -> "RemoteProviderProcess":
        if (
            not arguments
            or any(not isinstance(value, str) or "\x00" in value for value in arguments)
            or os.fsencode(arguments[0]) != os.fsencode(self.layout.launcher)
        ):
            _fail("arguments")
        payload = encode_arguments(tuple(os.fsencode(value) for value in arguments[1:]))
        response, descriptors = self._exchange(Kind.SPAWN, payload=payload)
        try:
            if (
                response.kind != Kind.SPAWNED
                or response.handle == 0
                or not 1 < response.value <= 0x7FFF_FFFF
                or response.payload
                or len(descriptors) != 2
                or descriptors[0] == descriptors[1]
            ):
                _fail("descriptor")
            identities = []
            for descriptor in descriptors:
                _validate_output_descriptor(descriptor)
                metadata = os.fstat(descriptor)
                identities.append((metadata.st_dev, metadata.st_ino))
            if len(set(identities)) != 2:
                _fail("descriptor")
            process = RemoteProviderProcess(
                self,
                response.handle,
                response.value,
                descriptors[0],
                descriptors[1],
            )
            descriptors = []
            self.processes.add(process)
            return process
        finally:
            _close_descriptors(descriptors)

    def _status_exchange(self, kind: Kind, handle: int, value: int = 0) -> Optional[int]:
        response, descriptors = self._exchange(kind, handle=handle, value=value)
        try:
            if descriptors or response.handle != handle or response.payload:
                _fail("protocol")
            if response.kind == Kind.RUNNING and response.value == 0:
                return None
            if response.kind == Kind.EXITED and -255 <= response.value <= 255:
                return response.value
            _fail("protocol")
        finally:
            _close_descriptors(descriptors)

    def _close_process(self, process: "RemoteProviderProcess") -> int:
        response, descriptors = self._exchange(Kind.CLOSE, handle=process.handle)
        try:
            if (
                descriptors
                or response.kind != Kind.CLOSED
                or response.handle != process.handle
                or response.payload
                or not -255 <= response.value <= 255
            ):
                _fail("protocol")
            self.processes.discard(process)
            return response.value
        finally:
            _close_descriptors(descriptors)

    def finish(self) -> None:
        if self.finished or self.processes:
            _fail("cleanup")
        response, descriptors = self._exchange(Kind.FINISH)
        try:
            if (
                descriptors
                or response.kind != Kind.FINISHED
                or response.handle
                or response.value
                or response.payload
            ):
                _fail("protocol")
            self.finished = True
        finally:
            _close_descriptors(descriptors)

    def close(self) -> None:
        failures = False
        for process in tuple(self.processes):
            try:
                process.close()
            except HandoffError:
                failures = True
        if failures:
            _fail("cleanup")


class RemoteProviderProcess:
    def __init__(
        self,
        proxy: ControllerProxy,
        handle: int,
        pid: int,
        stdout_descriptor: int,
        stderr_descriptor: int,
    ) -> None:
        self.proxy = proxy
        self.handle = handle
        self.pid = pid
        self.returncode: Optional[int] = None
        self.stdout_descriptor = stdout_descriptor
        self.stderr_descriptor = stderr_descriptor
        self.stdout_capture = _BoundedCapture(MAX_CAPTURE_BYTES)
        self.stderr_capture = _BoundedCapture(MAX_CAPTURE_BYTES)
        self.closed = False
        self.threads = (
            threading.Thread(
                target=_pump_descriptor,
                args=(stdout_descriptor, self.stdout_capture),
                name="bangbang-handoff-stdout",
                daemon=True,
            ),
            threading.Thread(
                target=_pump_descriptor,
                args=(stderr_descriptor, self.stderr_capture),
                name="bangbang-handoff-stderr",
                daemon=True,
            ),
        )
        for thread in self.threads:
            thread.start()

    def __hash__(self) -> int:
        return id(self)

    def _output_failed(self) -> bool:
        return any(capture.result()[1] or capture.result()[2] for capture in (self.stdout_capture, self.stderr_capture))

    def poll(self) -> Optional[int]:
        if self.closed:
            return self.returncode
        if self.returncode is None:
            if self._output_failed():
                self.kill()
                _fail("output")
            self.returncode = self.proxy._status_exchange(Kind.POLL, self.handle)
        return self.returncode

    def wait(self, timeout: float = PROCESS_TIMEOUT) -> int:
        if timeout <= 0:
            _fail("timeout")
        deadline = time.monotonic() + timeout
        while self.returncode is None:
            if self._output_failed():
                try:
                    self.kill()
                finally:
                    _fail("output")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                _fail("timeout")
            milliseconds = max(
                1,
                min(WAIT_SLICE_MILLISECONDS, int(remaining * 1000)),
            )
            self.returncode = self.proxy._status_exchange(
                Kind.WAIT, self.handle, milliseconds
            )
        return self.returncode

    def terminate(self) -> None:
        if self.returncode is None and not self.closed:
            self.returncode = self.proxy._status_exchange(Kind.TERM, self.handle)

    def kill(self) -> None:
        if self.returncode is None and not self.closed:
            self.returncode = self.proxy._status_exchange(Kind.KILL, self.handle)

    def _finish_readers(self) -> tuple[bytes, bytes]:
        for thread in self.threads:
            thread.join(timeout=2.0)
        if any(thread.is_alive() for thread in self.threads):
            _fail("output")
        stdout, stdout_overflow, stdout_failure = self.stdout_capture.result()
        stderr, stderr_overflow, stderr_failure = self.stderr_capture.result()
        if stdout_overflow or stderr_overflow or stdout_failure or stderr_failure:
            _fail("output")
        return stdout, stderr

    def communicate(self, timeout: float = PROCESS_TIMEOUT) -> tuple[bytes, bytes]:
        self.wait(timeout)
        return self._finish_readers()

    def close(self) -> None:
        if self.closed and self not in self.proxy.processes:
            return
        if self.closed:
            _fail("cleanup")
        failure: Optional[HandoffError] = None
        prior_status = self.returncode
        try:
            if self.returncode is None:
                self.terminate()
                try:
                    self.wait(2.0)
                except HandoffError:
                    self.kill()
                    self.wait(2.0)
            closed_status = self.proxy._close_process(self)
            if prior_status is not None and closed_status != prior_status:
                _fail("protocol")
            self.returncode = closed_status
        except HandoffError as error:
            failure = error
        finally:
            self.closed = True
            _close_descriptors((self.stdout_descriptor, self.stderr_descriptor))
            for thread in self.threads:
                thread.join(timeout=0.25)
        if failure is not None:
            raise failure

    def __enter__(self) -> "RemoteProviderProcess":
        return self

    def __exit__(self, _kind: object, _value: object, _traceback: object) -> None:
        self.close()


def _write_private_file(path: Path, data: bytes) -> None:
    _write_exclusive(path, data, 0o600)
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise HandoffError("probe-private-file") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_uid != os.getuid()
        or metadata.st_gid != os.getgid()
        or stat.S_IMODE(metadata.st_mode) != 0o600
    ):
        _fail("probe-private-file")


def _probe_session_root() -> Path:
    # This lookup runs only in the irreversibly ordinary controller. The root
    # guardian and protocol supervisor never receive or discover the account
    # name or home path.
    try:
        import pwd

        account = pwd.getpwuid(os.getuid())
        home = Path(account.pw_dir)
    except (ImportError, KeyError, OSError) as error:
        raise HandoffError("probe-session-root") from error
    if account.pw_uid != os.getuid() or not home.is_absolute() or "\x00" in os.fspath(home):
        _fail("probe-session-root")
    return home / "Library/Containers" / WORKER_IDENTIFIER / "Data/tmp/bangbang-sessions-v1"


def _probe_session_entries(
    root: Optional[Path] = None,
) -> tuple[
    tuple[str, int, int, tuple[tuple[str, int, int, int, int, int], ...]], ...
]:
    root = _probe_session_root() if root is None else root
    try:
        metadata = os.lstat(root)
    except FileNotFoundError:
        return ()
    except OSError as error:
        raise HandoffError("probe-session-root") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_gid != os.getgid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        _fail("probe-session-root")
    try:
        entries = sorted(os.scandir(root), key=lambda entry: entry.name)
        if len(entries) > 128:
            _fail("probe-session-root")
        result = []
        for entry in entries:
            if re.fullmatch(r"session-[0-9a-f]{64}", entry.name) is None:
                _fail("probe-session-root")
            session = entry.stat(follow_symlinks=False)
            if (
                not stat.S_ISDIR(session.st_mode)
                or stat.S_ISLNK(session.st_mode)
                or session.st_uid != os.getuid()
                or session.st_gid != os.getgid()
                or stat.S_IMODE(session.st_mode) != 0o700
            ):
                _fail("probe-session-root")
            children = sorted(os.scandir(entry.path), key=lambda child: child.name)
            if len(children) > 8:
                _fail("probe-session-root")
            child_rows = []
            for child in children:
                child_metadata = child.stat(follow_symlinks=False)
                if (
                    stat.S_ISLNK(child_metadata.st_mode)
                    or child_metadata.st_uid != os.getuid()
                    or child_metadata.st_gid != os.getgid()
                    or not (
                        stat.S_ISREG(child_metadata.st_mode)
                        or stat.S_ISSOCK(child_metadata.st_mode)
                    )
                ):
                    _fail("probe-session-root")
                child_rows.append(
                    (
                        child.name,
                        stat.S_IFMT(child_metadata.st_mode)
                        | stat.S_IMODE(child_metadata.st_mode),
                        child_metadata.st_dev,
                        child_metadata.st_ino,
                        child_metadata.st_size,
                        child_metadata.st_mtime_ns,
                    )
                )
            result.append(
                (
                    entry.name,
                    session.st_dev,
                    session.st_ino,
                    tuple(child_rows),
                )
            )
        return tuple(result)
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("probe-session-root") from error


def _create_private_probe_root(
    parent: Path = Path("/private/var/tmp"),
) -> Path:
    root: Optional[Path] = None
    try:
        root = Path(tempfile.mkdtemp(prefix="bbhandoff.", dir=parent))
        os.chown(root, os.getuid(), os.getgid())
        os.chmod(root, 0o700)
        metadata = os.lstat(root)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_gid != os.getgid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            _fail("probe-private-root")
        return root
    except BaseException as error:
        if root is not None and os.path.lexists(root):
            try:
                shutil.rmtree(root)
            except OSError as cleanup_error:
                raise HandoffError("cleanup") from cleanup_error
        if isinstance(error, HandoffError):
            raise
        if isinstance(error, OSError):
            raise HandoffError("probe-private-root") from error
        raise


def _launcher_arguments(
    layout: ProductLayout,
    uid: int,
    gid: int,
    instance: str,
    worker_arguments: Sequence[str],
) -> tuple[str, ...]:
    if re.fullmatch(r"[a-z0-9-]{1,64}", instance) is None:
        _fail("probe-path")
    return (
        os.fspath(layout.launcher),
        "--bangbang-jailer-v1",
        "--id",
        instance,
        "--exec-file",
        os.fspath(layout.worker),
        "--uid",
        str(uid),
        "--gid",
        str(gid),
        "--vmnet-allow",
        "shared",
        "--vmnet-max-interfaces",
        "1",
        "--",
        *worker_arguments,
    )


def _fixed_completion_probe(
    proxy: ControllerProxy, layout: ProductLayout, uid: int, gid: int, instance: str
) -> None:
    process = proxy.spawn(
        _launcher_arguments(layout, uid, gid, instance, ("--version",))
    )
    try:
        stdout, stderr = process.communicate()
        if process.returncode != 0:
            category = PROVIDER_STATUS_FAILURES.get(
                process.returncode,
                "probe-completion-signal"
                if process.returncode is not None and process.returncode < 0
                else "probe-completion-child-status",
            )
            _fail(category)
        if stderr:
            _fail("probe-completion-stderr")
        if not stdout.startswith(b"bangbang "):
            _fail("probe-completion-stdout")
    finally:
        process.close()


def _probe_socket_present(path: Path, process: RemoteProviderProcess) -> bool:
    if process.poll() is not None:
        _fail("probe-signal-exited")
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        return False
    except OSError as error:
        raise HandoffError("probe-socket-observe") from error
    if (
        not stat.S_ISSOCK(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_gid != os.getgid()
        or stat.S_IMODE(metadata.st_mode) != 0o600
    ):
        _fail("probe-socket-shape")
    return True


def _remove_killed_probe_socket(path: Path) -> None:
    try:
        metadata = os.lstat(path)
        if (
            not stat.S_ISSOCK(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_gid != os.getgid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
        ):
            _fail("cleanup")
        path.unlink()
        if os.path.lexists(path):
            _fail("cleanup")
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("cleanup") from error


def _cleanup_probe_root(root: Path, *, require_material: bool = True) -> bool:
    forced = False
    try:
        metadata = os.lstat(root)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_gid != os.getgid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            _fail("cleanup")
        expected = {
            "api": "directory",
            "api/api.sock": "socket",
            "grants.json": "file",
        }
        actual: dict[str, str] = {}
        for directory, names, files in os.walk(root):
            base = Path(directory)
            directory_metadata = os.lstat(base)
            if (
                not stat.S_ISDIR(directory_metadata.st_mode)
                or stat.S_ISLNK(directory_metadata.st_mode)
                or directory_metadata.st_uid != os.getuid()
                or directory_metadata.st_gid != os.getgid()
            ):
                _fail("cleanup")
            for name in (*names, *files):
                path = base / name
                child = os.lstat(path)
                relative = path.relative_to(root).as_posix()
                if (
                    stat.S_ISLNK(child.st_mode)
                    or child.st_uid != os.getuid()
                    or child.st_gid != os.getgid()
                ):
                    _fail("cleanup")
                if stat.S_ISDIR(child.st_mode):
                    kind = "directory"
                elif stat.S_ISREG(child.st_mode) and child.st_nlink == 1:
                    kind = "file"
                elif stat.S_ISSOCK(child.st_mode):
                    kind = "socket"
                else:
                    _fail("cleanup")
                expected_mode = {
                    "api": 0o700,
                    "api/api.sock": 0o600,
                    "grants.json": 0o600,
                }.get(relative)
                if expected_mode is None or stat.S_IMODE(child.st_mode) != expected_mode:
                    _fail("cleanup")
                actual[relative] = kind
        if (
            (
                require_material
                and not {"api", "grants.json"}.issubset(actual)
            )
            or set(actual) - set(expected)
            or any(
                expected[name] != kind for name, kind in actual.items()
            )
        ):
            _fail("cleanup")
        socket_path = root / "api/api.sock"
        if actual.get("api/api.sock") == "socket":
            socket_path.unlink()
            forced = True
        shutil.rmtree(root)
        if os.path.lexists(root):
            _fail("cleanup")
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("cleanup") from error
    return forced


def _fixed_signal_probe(
    proxy: ControllerProxy,
    layout: ProductLayout,
    uid: int,
    gid: int,
    instance: str,
    kind: Kind,
) -> None:
    session_root = _probe_session_root()
    session_baseline = _probe_session_entries(session_root)
    root = _create_private_probe_root()
    process: Optional[RemoteProviderProcess] = None
    material_complete = False
    try:
        api = root / "api"
        api.mkdir(mode=0o700)
        manifest = root / "grants.json"
        _write_private_file(
            manifest,
            _canonical(
                {
                    "grants": [
                        {
                            "access": "create-children",
                            "id": "handoff-api",
                            "role": "api-socket-directory",
                            "source": os.fspath(api),
                        }
                    ],
                    "version": 1,
                }
            ),
        )
        material_complete = True
        socket_path = api / "api.sock"
        if len(os.fsencode(socket_path)) >= 104:
            _fail("probe-path")
        arguments = _launcher_arguments(
            layout,
            uid,
            gid,
            instance,
            (
                "--bangbang-grant-manifest",
                os.fspath(manifest),
                "--",
                "--api-sock",
                "bangbang-grant:handoff-api/api.sock",
            ),
        )
        process = proxy.spawn(arguments)
        if not _wait_until(lambda: _probe_socket_present(socket_path, process), 20.0):
            _fail("probe-socket-timeout")
        if kind == Kind.TERM:
            process.terminate()
        elif kind == Kind.KILL:
            process.kill()
        else:
            _fail("internal")
        process.wait(10.0)
        process._finish_readers()
        process.close()
        process = None
        if os.path.lexists(socket_path):
            if kind != Kind.KILL:
                _fail("cleanup")
            _remove_killed_probe_socket(socket_path)
    finally:
        cleanup_failure: Optional[HandoffError] = None
        if process is not None:
            try:
                process.close()
            except HandoffError as error:
                cleanup_failure = error
        try:
            if _cleanup_probe_root(root, require_material=material_complete):
                _fail("cleanup")
        except HandoffError as error:
            if cleanup_failure is None:
                cleanup_failure = error
        try:
            if not _wait_until(
                lambda: _probe_session_entries(session_root) == session_baseline,
                CLEANUP_TIMEOUT,
            ):
                _fail("cleanup")
        except HandoffError as error:
            if cleanup_failure is None:
                cleanup_failure = error
        if cleanup_failure is not None:
            raise cleanup_failure


def run_fixed_probes(proxy: ControllerProxy, layout: ProductLayout, uid: int, gid: int) -> None:
    _fixed_completion_probe(proxy, layout, uid, gid, "handoff-complete-1")
    _fixed_completion_probe(proxy, layout, uid, gid, "handoff-complete-2")
    for instance, kind, category in (
        ("handoff-term", Kind.TERM, "probe-term-cleanup"),
        ("handoff-kill", Kind.KILL, "probe-kill-cleanup"),
    ):
        try:
            _fixed_signal_probe(proxy, layout, uid, gid, instance, kind)
        except HandoffError as error:
            if error.category == "cleanup":
                raise HandoffError(category) from error
            raise


ControllerEntry = Callable[[ControllerProxy, ProductLayout, int, int], None]
ControllerLoader = Callable[[], ControllerEntry]


def default_controller_loader() -> ControllerEntry:
    return run_fixed_probes


def _redirect_stdio() -> None:
    descriptor = -1
    try:
        descriptor = os.open("/dev/null", os.O_RDWR | getattr(os, "O_CLOEXEC", 0))
        for standard in (0, 1, 2):
            os.dup2(descriptor, standard)
    except OSError as error:
        raise HandoffError("descriptor") from error
    finally:
        if descriptor > 2:
            os.close(descriptor)


def _close_unrelated_descriptors(preserved: set[int]) -> None:
    try:
        maximum = resource.getrlimit(resource.RLIMIT_NOFILE)[0]
    except (OSError, ValueError) as error:
        raise HandoffError("descriptor") from error
    if maximum == resource.RLIM_INFINITY:
        maximum = 65536
    maximum = min(int(maximum), 65536)
    for descriptor in range(3, maximum):
        if descriptor in preserved:
            continue
        try:
            os.close(descriptor)
        except OSError:
            pass


def _write_all(descriptor: int, data: bytes) -> None:
    offset = 0
    try:
        while offset < len(data):
            count = os.write(descriptor, data[offset:])
            if count <= 0:
                _fail("protocol")
            offset += count
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("protocol") from error


def _read_exact(descriptor: int, size: int, timeout: float) -> bytes:
    deadline = time.monotonic() + timeout
    data = bytearray()
    try:
        while len(data) < size:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                _fail("protocol-timeout")
            readable, _writable, _exceptional = select.select(
                [descriptor], [], [], remaining
            )
            if not readable:
                _fail("protocol-timeout")
            chunk = os.read(descriptor, size - len(data))
            if not chunk:
                _fail("protocol")
            data.extend(chunk)
        return bytes(data)
    except HandoffError:
        raise
    except OSError as error:
        raise HandoffError("protocol") from error


def _controller_child(
    connection: socket.socket,
    ready_descriptor: int,
    supervisor_pid: int,
    layout: ProductLayout,
    uid: int,
    gid: int,
    loader: ControllerLoader,
) -> NoReturn:
    exit_code = 1
    proxy: Optional[ControllerProxy] = None
    session: Optional[SessionSocket] = None
    failure_category = "internal"
    ready_published = False
    try:
        _redirect_stdio()
        _close_unrelated_descriptors({connection.fileno(), ready_descriptor})
        transition_controller_credentials(uid, gid, supervisor_pid)
        _write_all(ready_descriptor, b"R\x00")
        ready_published = True
        os.close(ready_descriptor)
        ready_descriptor = -1
        session = SessionSocket(RecordSocket(connection), Role.CONTROLLER)
        welcome, descriptors = session.receive(allow_unbound_welcome=True)
        try:
            if (
                descriptors
                or welcome.kind != Kind.WELCOME
                or welcome.correlation
                or welcome.handle
                or welcome.payload
                or welcome.value != supervisor_pid
                or os.getppid() != supervisor_pid
            ):
                _fail("protocol")
        finally:
            _close_descriptors(descriptors)
        session.send(Kind.READY, correlation=welcome.sequence, value=os.getpid())
        proxy = ControllerProxy(session, layout)
        entry = loader()
        if not callable(entry):
            _fail("controller")
        entry(proxy, layout, uid, gid)
        proxy.finish()
        exit_code = 0
    except HandoffError as error:
        failure_category = error.category
        if ready_descriptor >= 0 and not ready_published:
            try:
                category = (
                    failure_category
                    if failure_category in CONTROLLER_FAILURES
                    else "internal"
                )
                _write_all(
                    ready_descriptor,
                    bytes((ord("F"), CONTROLLER_FAILURES.index(category) + 1)),
                )
            except HandoffError:
                pass
        if proxy is not None:
            try:
                proxy.close()
            except HandoffError:
                pass
        if session is not None and not session.terminal:
            try:
                category = (
                    failure_category
                    if failure_category in CONTROLLER_FAILURES
                    else "internal"
                )
                session.send(
                    Kind.FAILURE,
                    value=CONTROLLER_FAILURES.index(category) + 1,
                )
            except HandoffError:
                pass
    except BaseException:
        if ready_descriptor >= 0 and not ready_published:
            try:
                _write_all(ready_descriptor, b"F\x01")
            except HandoffError:
                pass
        if proxy is not None:
            try:
                proxy.close()
            except HandoffError:
                pass
        if session is not None and not session.terminal:
            try:
                session.send(Kind.FAILURE, value=1)
            except HandoffError:
                pass
    finally:
        if ready_descriptor >= 0:
            try:
                os.close(ready_descriptor)
            except OSError:
                pass
        try:
            connection.close()
        except OSError:
            pass
    os._exit(exit_code)


def _wait_child(pid: int, timeout: float) -> int:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            waited, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            _fail("controller")
        except OSError as error:
            raise HandoffError("controller") from error
        if waited == pid:
            return status
        time.sleep(POLL_SECONDS)
    _fail("controller-timeout")


def _guardian_lease_alive(descriptor: int) -> bool:
    try:
        readable, _writable, _exceptional = select.select([descriptor], [], [], 0)
        if not readable:
            return True
        return bool(os.read(descriptor, 1))
    except OSError:
        return False


def _receive_completion(connection: socket.socket) -> bytes:
    message = bytearray()
    while len(message) < 2:
        chunk = connection.recv(2 - len(message))
        if not chunk:
            raise OSError("completion peer closed")
        message.extend(chunk)
    return bytes(message)


def _supervisor_complete(connection: socket.socket) -> None:
    try:
        connection.settimeout(CLEANUP_TIMEOUT)
        if connection.send(b"C\x00") != 2 or _receive_completion(connection) != b"A\x00":
            _fail("guardian")
    except HandoffError:
        raise
    except (OSError, socket.timeout) as error:
        raise HandoffError("guardian") from error


def _wait_supervisor_complete(connection: socket.socket) -> None:
    try:
        connection.settimeout(SESSION_TIMEOUT + CLEANUP_TIMEOUT)
        message = _receive_completion(connection)
        if message == b"C\x00":
            return
        if (
            len(message) == 2
            and message[0] == ord("F")
            and 1 <= message[1] <= len(SUPERVISOR_FAILURES)
        ):
            _fail(f"supervisor-{SUPERVISOR_FAILURES[message[1] - 1]}")
        _fail("supervisor")
    except HandoffError:
        raise
    except (OSError, socket.timeout) as error:
        raise HandoffError("supervisor") from error


def _acknowledge_guardian_cleanup(connection: socket.socket) -> None:
    try:
        connection.settimeout(CLEANUP_TIMEOUT)
        if connection.send(b"A\x00") != 2:
            _fail("supervisor")
    except HandoffError:
        raise
    except (OSError, socket.timeout) as error:
        raise HandoffError("supervisor") from error


def _supervisor_failure_category(error: BaseException, phase: int) -> str:
    fallback = SUPERVISOR_PHASES[phase - 1]
    if not isinstance(error, HandoffError):
        return fallback
    if error.category in SUPERVISOR_FAILURES:
        return error.category
    if phase == 5 and error.category in LIFECYCLE_FAILURES:
        return f"lifecycle-{error.category}"
    return fallback


def _supervisor_entry(
    stage: Stage,
    uid: int,
    gid: int,
    guardian_lease: int,
    identity_descriptor: int,
    completion: socket.socket,
    loader: ControllerLoader,
) -> int:
    controller_pid = -1
    controller_identity: Optional[ProcessIdentity] = None
    server: Optional[ProviderSupervisor] = None
    supervisor_socket: Optional[socket.socket] = None
    controller_socket: Optional[socket.socket] = None
    ready_read = ready_write = -1
    guardian_lost = False
    phase = 1
    try:
        phase = 2
        supervisor_socket, controller_socket = socket.socketpair(
            socket.AF_UNIX, socket.SOCK_DGRAM
        )
        ready_read, ready_write = os.pipe()
        controller_pid = os.fork()
        if controller_pid == 0:
            supervisor_socket.close()
            os.close(ready_read)
            _controller_child(
                controller_socket,
                ready_write,
                os.getppid(),
                stage.layout,
                uid,
                gid,
                loader,
            )
        controller_socket.close()
        controller_socket = None
        os.close(ready_write)
        ready_write = -1
        _write_all(identity_descriptor, struct.pack("!I", controller_pid))
        os.close(identity_descriptor)
        identity_descriptor = -1
        phase = 3
        ready_record = _read_exact(ready_read, 2, PROTOCOL_TIMEOUT)
        if ready_record != b"R\x00":
            if (
                ready_record[0] == ord("F")
                and 1 <= ready_record[1] <= len(CONTROLLER_FAILURES)
            ):
                _fail(f"controller-{CONTROLLER_FAILURES[ready_record[1] - 1]}")
            _fail("credentials")
        os.close(ready_read)
        ready_read = -1
        try:
            controller_identity = capture_process(controller_pid)
        except HandoffError as error:
            raise HandoffError("identity-capture") from error
        if controller_identity.parent_pid != os.getpid():
            _fail("identity-parent")
        if (
            controller_identity.uid,
            controller_identity.real_uid,
            controller_identity.saved_uid,
        ) != (uid, uid, uid):
            _fail("identity-uid")
        if (
            controller_identity.gid,
            controller_identity.real_gid,
            controller_identity.saved_gid,
        ) != (gid, gid, gid):
            _fail("identity-gid")
        session_id = secrets.token_bytes(32)
        phase = 4
        session = SessionSocket(
            RecordSocket(supervisor_socket), Role.SUPERVISOR, session_id
        )
        welcome_sequence = session.send(Kind.WELCOME, value=os.getpid())
        ready, descriptors = session.receive()
        try:
            if (
                descriptors
                or ready.kind != Kind.READY
                or ready.correlation != welcome_sequence
                or ready.handle
                or ready.payload
                or ready.value != controller_pid
            ):
                _fail("protocol")
        finally:
            _close_descriptors(descriptors)
        server = ProviderSupervisor(
            session,
            stage.layout,
            uid,
            gid,
            controller_pid,
            controller_identity,
            guardian_lease,
        )
        phase = 5
        server.serve()
        phase = 6
        status = _wait_child(controller_pid, PROTOCOL_TIMEOUT)
        controller_pid = -1
        if os.waitstatus_to_exitcode(status) != 0 or server.providers:
            _fail("controller")
        phase = 7
        _supervisor_complete(completion)
        if os.path.lexists(stage.root):
            _fail("cleanup")
        return 0
    except BaseException as error:
        try:
            category = _supervisor_failure_category(error, phase)
            completion.send(
                bytes((ord("F"), SUPERVISOR_FAILURES.index(category) + 1))
            )
        except OSError:
            pass
        guardian_lost = not _guardian_lease_alive(guardian_lease)
        if server is not None:
            try:
                server.cleanup()
            except HandoffError:
                pass
        if controller_pid > 1:
            try:
                if controller_identity is None:
                    controller_identity = capture_process(controller_pid)
                _retire_pid(controller_identity)
            except HandoffError:
                pass
            try:
                os.waitpid(controller_pid, 0)
            except (ChildProcessError, OSError):
                pass
        try:
            force_stage_process_cleanup(stage)
        except HandoffError:
            pass
        if guardian_lost:
            try:
                remove_stage(stage)
            except HandoffError:
                pass
        return 1
    finally:
        for descriptor in (
            guardian_lease,
            identity_descriptor,
            ready_read,
            ready_write,
        ):
            if descriptor >= 0:
                try:
                    os.close(descriptor)
                except OSError:
                    pass
        for connection in (supervisor_socket, controller_socket):
            if connection is not None:
                try:
                    connection.close()
                except OSError:
                    pass
        try:
            completion.close()
        except OSError:
            pass


def _require_root_platform(uid: int, gid: int) -> None:
    if (
        sys.platform != "darwin"
        or platform.machine() != "arm64"
        or uid == 0
        or gid == 0
        or any(value != 0 for value in (os.getuid(), os.geteuid(), os.getgid(), os.getegid()))
        or threading.active_count() != 1
    ):
        _fail("platform")
    outcome = _run_tool(
        ("/usr/sbin/sysctl", "-n", "kern.hv_support"),
        "platform",
        timeout=5.0,
    )
    if outcome.returncode != 0 or outcome.stderr or outcome.stdout.strip() != b"1":
        _fail("platform")


def run_root(
    prepared: Path,
    uid: int,
    gid: int,
    loader: ControllerLoader = default_controller_loader,
) -> None:
    _require_root_platform(uid, gid)
    if not prepared.is_absolute() or prepared.name != PACKAGE_KIND:
        _fail("invocation")
    stage = stage_package(prepared, uid, gid)
    lease_read = lease_write = identity_read = identity_write = -1
    guardian_completion: Optional[socket.socket] = None
    supervisor_completion: Optional[socket.socket] = None
    supervisor_pid = -1
    controller_identity: Optional[ProcessIdentity] = None
    forced = False
    completed = False
    try:
        lease_read, lease_write = os.pipe()
        identity_read, identity_write = os.pipe()
        guardian_completion, supervisor_completion = socket.socketpair(
            socket.AF_UNIX, socket.SOCK_STREAM
        )
        supervisor_pid = os.fork()
        if supervisor_pid == 0:
            guardian_completion.close()
            os.close(lease_write)
            os.close(identity_read)
            status = _supervisor_entry(
                stage,
                uid,
                gid,
                lease_read,
                identity_write,
                supervisor_completion,
                loader,
            )
            os._exit(status)
        supervisor_completion.close()
        supervisor_completion = None
        os.close(lease_read)
        lease_read = -1
        os.close(identity_write)
        identity_write = -1
        try:
            raw_pid = _read_exact(identity_read, 4, PROTOCOL_TIMEOUT)
            controller_pid = struct.unpack("!I", raw_pid)[0]
            controller_identity = capture_process(controller_pid)
        finally:
            os.close(identity_read)
            identity_read = -1
        try:
            _wait_supervisor_complete(guardian_completion)
        except HandoffError as error:
            guardian_completion.close()
            guardian_completion = None
            _waited, status = os.waitpid(supervisor_pid, 0)
            supervisor_pid = -1
            if controller_identity is not None:
                forced = _retire_pid(controller_identity) or forced
            forced = force_stage_process_cleanup(stage) or forced
            remove_stage(stage)
            raise error
        if controller_identity is not None:
            forced = _retire_pid(controller_identity) or forced
        forced = force_stage_process_cleanup(stage) or forced
        remove_stage(stage)
        _acknowledge_guardian_cleanup(guardian_completion)
        guardian_completion.close()
        guardian_completion = None
        _waited, status = os.waitpid(supervisor_pid, 0)
        supervisor_pid = -1
        os.close(lease_write)
        lease_write = -1
        if os.waitstatus_to_exitcode(status) != 0 or forced:
            _fail("session")
        completed = True
    finally:
        for descriptor in (lease_read, lease_write, identity_read, identity_write):
            if descriptor >= 0:
                try:
                    os.close(descriptor)
                except OSError:
                    pass
        for connection in (guardian_completion, supervisor_completion):
            if connection is not None:
                try:
                    connection.close()
                except OSError:
                    pass
        if supervisor_pid > 1:
            try:
                os.kill(supervisor_pid, signal.SIGKILL)
            except OSError:
                pass
            try:
                os.waitpid(supervisor_pid, 0)
            except OSError:
                pass
        if not completed and os.path.lexists(stage.root):
            try:
                force_stage_process_cleanup(stage)
                remove_stage(stage)
            except HandoffError:
                pass


def _parse_arguments(arguments: Optional[Sequence[str]]) -> argparse.Namespace:
    parser = ClosedArgumentParser(allow_abbrev=False)
    subparsers = parser.add_subparsers(dest="command", required=True)
    prepare = subparsers.add_parser("prepare", allow_abbrev=False)
    prepare.add_argument("--output", required=True, type=Path)
    root = subparsers.add_parser("run-root", allow_abbrev=False)
    root.add_argument("--prepared", required=True, type=Path)
    root.add_argument("--target-uid", required=True, type=_parse_id)
    root.add_argument("--target-gid", required=True, type=_parse_id)
    return parser.parse_args(arguments)


def main(arguments: Optional[Sequence[str]] = None) -> int:
    try:
        options = _parse_arguments(arguments)
        if options.command == "prepare":
            prepare_package(options.output)
            print("bangbang elevated vmnet handoff prepare: ready")
        elif options.command == "run-root":
            run_root(options.prepared, options.target_uid, options.target_gid)
            print(
                "bangbang elevated vmnet handoff proof: "
                "ordinary=passed complete=passed repeat=passed "
                "term=passed kill=passed cleanup=passed"
            )
        else:  # pragma: no cover - argparse owns the closed command set
            _fail("invocation")
    except HandoffError as error:
        print(f"bangbang elevated vmnet handoff: {error.category}", file=sys.stderr)
        return 1
    except SystemExit as error:
        return int(error.code) if isinstance(error.code, int) else 1
    except BaseException:
        print("bangbang elevated vmnet handoff: internal", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
