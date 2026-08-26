#!/usr/bin/env python3
"""Validate and transport production-vmnet certification contracts.

This module deliberately does not build or launch Bangbang.  The production
orchestrator is a later delivery slice; this file owns only the private input,
public result, guest-control, and retained external-fixture protocols.
"""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import re
import secrets
import select
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import time
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence


SCHEMA_VERSION = 1
MAX_DOCUMENT_BYTES = 64 * 1024
MAX_PROFILE_BYTES = 4 * 1024 * 1024
MAX_FIXTURE_BYTES = 64 * 1024 * 1024
MAX_PATH_BYTES = 4096
MAX_IDENTITY_BYTES = 512
MAX_FIXTURE_LINE_BYTES = 16 * 1024
MAX_FIXTURE_CAPTURE_BYTES = 64 * 1024
MAX_TIMEOUT_SECONDS = 3600
MAX_SHORT_TIMEOUT_SECONDS = 60
MAX_TOTAL_TIMEOUT_SECONDS = 10800
PRIVATE_DIRECTORY_MODE = 0o700
PRIVATE_FILE_MODE = 0o600
FIXTURE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"

SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
GIT_OBJECT_RE = re.compile(r"[0-9a-f]{40}\Z")
BRIDGE_RE = re.compile(r"[A-Za-z0-9_.-]{1,15}\Z")
PLATFORM_VERSION_RE = re.compile(
    r"(?:0|[1-9][0-9]{0,2})(?:\.(?:0|[1-9][0-9]{0,2})){1,2}\Z"
)

CASE_NAMES = (
    "entitlement-split",
    "networkless-denial",
    "missing-policy-denial",
    "mismatched-policy-denial",
    "bridge-allowlist-denial",
    "active-interface-count-exhaustion",
    "mmds-only-no-consumption",
    "shared-connectivity",
    "host-connectivity",
    "bridged-connectivity",
    "not-authorized",
    "sharing-service-busy",
    "normal-teardown",
    "partial-start-cleanup",
    "pre-ready-cancellation",
    "post-ready-cancellation",
    "worker-first-death",
    "launcher-first-death",
    "worker-sigkill-reclamation",
    "clean-repeat",
    "concurrent-noninterchangeability",
)
ENVIRONMENT_GATED_CASES = frozenset(
    {
        "host-connectivity",
        "bridged-connectivity",
        "not-authorized",
        "sharing-service-busy",
    }
)
FIXTURE_CASES = frozenset(
    {
        "shared-connectivity",
        "host-connectivity",
        "bridged-connectivity",
        "not-authorized",
        "sharing-service-busy",
    }
)
CONNECTIVITY_CASES = frozenset(
    {"shared-connectivity", "host-connectivity", "bridged-connectivity"}
)
CASE_OUTCOMES = frozenset({"passed", "environment-gated", "blocked", "failed"})

CONTROL_BYTES = 512
CONTROL_PREFIX_BYTES = 64
CONTROL_DIGEST_BYTES = 32
CONTROL_MAGIC = b"BBVMNET1"
CONTROL_MODES = {"shared": 1, "host": 2, "bridged": 3}
CONTROL_MODE_NAMES = {value: key for key, value in CONTROL_MODES.items()}
CONTROL_PREFIX = struct.Struct("!8sHBB4sH32s14s")
TCP_REQUEST_MAGIC = b"BBVREQ1\0"
TCP_RESPONSE_MAGIC = b"BBVRES1\0"
TCP_RECORD_BYTES = 40


class CertificationError(RuntimeError):
    """Stable public certification-contract failure."""

    def __init__(self, category: str) -> None:
        super().__init__(f"production vmnet certification failed: {category}")
        self.category = category


class RedactedArgumentParser(argparse.ArgumentParser):
    """Argparse surface that never reflects untrusted argv in errors."""

    def __init__(self, *args: object, **kwargs: object) -> None:
        kwargs.setdefault("allow_abbrev", False)
        super().__init__(*args, **kwargs)

    def error(self, _message: str) -> None:
        raise CertificationError("invocation")


@dataclass(frozen=True)
class FileIdentity:
    device: int
    inode: int
    size: int
    sha256: Optional[str] = None
    mtime_ns: Optional[int] = None
    ctime_ns: Optional[int] = None


@dataclass(frozen=True)
class FixtureConfig:
    executable: Path
    expected_sha256: str
    identity: FileIdentity


@dataclass(frozen=True)
class OptionalCases:
    host_connectivity: bool
    bridged_interface: Optional[str]
    not_authorized: bool
    sharing_service_busy: bool


@dataclass(frozen=True)
class CertificationTimeouts:
    artifact_seconds: int
    build_seconds: int
    fixture_seconds: int
    guest_seconds: int
    request_seconds: int
    startup_seconds: int
    terminate_seconds: int


@dataclass(frozen=True)
class CertificationConfig:
    signing_identity: str
    provisioning_profile: Path
    provisioning_profile_identity: FileIdentity
    fixture: FixtureConfig
    optional_cases: OptionalCases
    timeouts: CertificationTimeouts


@dataclass(frozen=True)
class GuestControl:
    mode: str
    endpoint_ipv4: str
    endpoint_port: int
    nonce: bytes


@dataclass(frozen=True)
class FixtureEndpoint:
    ipv4: str
    port: int


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(
            value, sort_keys=True, indent=2, ensure_ascii=True, allow_nan=False
        )
        + "\n"
    ).encode("ascii")


def canonical_line(value: object) -> bytes:
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=True,
            allow_nan=False,
        )
        + "\n"
    ).encode("ascii")


def _duplicate_safe_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise CertificationError("document")
        result[key] = value
    return result


def _reject_json_constant(_value: str) -> None:
    raise CertificationError("document")


def _decode_document(data: bytes, *, line: bool = False) -> dict[str, Any]:
    try:
        value = json.loads(
            data,
            object_pairs_hook=_duplicate_safe_object,
            parse_constant=_reject_json_constant,
        )
    except CertificationError:
        raise
    except (RecursionError, UnicodeDecodeError, ValueError) as error:
        raise CertificationError("document") from error
    if not isinstance(value, dict):
        raise CertificationError("document")
    try:
        expected = canonical_line(value) if line else canonical_json(value)
    except (RecursionError, TypeError, ValueError) as error:
        raise CertificationError("document") from error
    if expected != data:
        raise CertificationError("document")
    return value


def _object(
    value: object,
    required: Sequence[str],
    label: str,
    *,
    optional: Sequence[str] = (),
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise CertificationError(label)
    required_keys = set(required)
    allowed = required_keys | set(optional)
    actual = set(value)
    if not required_keys.issubset(actual) or not actual.issubset(allowed):
        raise CertificationError(label)
    return value


def _string(value: object, label: str, *, maximum: int, minimum: int = 1) -> str:
    if not isinstance(value, str):
        raise CertificationError(label)
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise CertificationError(label) from error
    if not minimum <= len(encoded) <= maximum or "\x00" in value:
        raise CertificationError(label)
    return value


def _bool(value: object, label: str) -> bool:
    if not isinstance(value, bool):
        raise CertificationError(label)
    return value


def _integer(value: object, minimum: int, maximum: int, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise CertificationError(label)
    if not minimum <= value <= maximum:
        raise CertificationError(label)
    return value


def _absolute_path(value: object, label: str) -> Path:
    text = _string(value, label, maximum=MAX_PATH_BYTES)
    path = Path(text)
    if not path.is_absolute():
        raise CertificationError(label)
    return path


def _sha256_fd(fd: int) -> str:
    digest = hashlib.sha256()
    os.lseek(fd, 0, os.SEEK_SET)
    while True:
        chunk = os.read(fd, 1024 * 1024)
        if not chunk:
            break
        digest.update(chunk)
    os.lseek(fd, 0, os.SEEK_SET)
    return digest.hexdigest()


def _open_regular(
    path: Path,
    *,
    category: str,
    maximum: int,
    private: bool,
    executable: bool = False,
    digest: bool = False,
) -> tuple[int, FileIdentity]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        before = os.lstat(path)
        fd = os.open(path, flags)
    except OSError as error:
        raise CertificationError(category) from error
    try:
        current = os.fstat(fd)
        mode = stat.S_IMODE(current.st_mode)
        if (
            not stat.S_ISREG(current.st_mode)
            or stat.S_ISLNK(before.st_mode)
            or before.st_dev != current.st_dev
            or before.st_ino != current.st_ino
            or current.st_nlink != 1
            or current.st_uid != os.getuid()
            or current.st_size < 0
            or current.st_size > maximum
        ):
            raise CertificationError(category)
        if private and mode != PRIVATE_FILE_MODE:
            raise CertificationError(category)
        if executable and (
            not mode & stat.S_IXUSR or mode & 0o022 or mode & 0o7000
        ):
            raise CertificationError(category)
        sha256 = _sha256_fd(fd) if digest else None
        after = os.fstat(fd)
        if (
            after.st_dev != current.st_dev
            or after.st_ino != current.st_ino
            or after.st_size != current.st_size
            or after.st_mtime_ns != current.st_mtime_ns
            or after.st_ctime_ns != current.st_ctime_ns
        ):
            raise CertificationError(category)
        return fd, FileIdentity(
            current.st_dev,
            current.st_ino,
            current.st_size,
            sha256,
            current.st_mtime_ns,
            current.st_ctime_ns,
        )
    except BaseException:
        os.close(fd)
        raise


def _read_private_document(path: Path) -> tuple[dict[str, Any], FileIdentity]:
    if not path.is_absolute():
        raise CertificationError("config")
    fd, identity = _open_regular(
        path,
        category="config",
        maximum=MAX_DOCUMENT_BYTES,
        private=True,
    )
    try:
        data = bytearray()
        while True:
            chunk = os.read(fd, min(4096, MAX_DOCUMENT_BYTES + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
            if len(data) > MAX_DOCUMENT_BYTES:
                raise CertificationError("config")
        after = os.fstat(fd)
        if (
            after.st_dev != identity.device
            or after.st_ino != identity.inode
            or after.st_size != identity.size
            or after.st_mtime_ns != identity.mtime_ns
            or after.st_ctime_ns != identity.ctime_ns
        ):
            raise CertificationError("config")
    finally:
        os.close(fd)
    return _decode_document(bytes(data)), identity


def _parse_timeouts(value: object) -> CertificationTimeouts:
    root = _object(
        value,
        (
            "artifact_seconds",
            "build_seconds",
            "fixture_seconds",
            "guest_seconds",
            "request_seconds",
            "startup_seconds",
            "terminate_seconds",
        ),
        "timeouts",
    )
    values = {
        key: _integer(raw, 1, MAX_TIMEOUT_SECONDS, "timeouts")
        for key, raw in root.items()
    }
    if (
        values["request_seconds"] > MAX_SHORT_TIMEOUT_SECONDS
        or values["terminate_seconds"] > MAX_SHORT_TIMEOUT_SECONDS
        or sum(values.values()) > MAX_TOTAL_TIMEOUT_SECONDS
    ):
        raise CertificationError("timeouts")
    return CertificationTimeouts(**values)


def parse_config_document(document: object) -> CertificationConfig:
    root = _object(
        document,
        (
            "fixture",
            "optional_cases",
            "provisioning_profile",
            "schema_version",
            "signing_identity",
            "timeouts",
        ),
        "config",
    )
    if (
        _integer(
            root["schema_version"], SCHEMA_VERSION, SCHEMA_VERSION, "config"
        )
        != SCHEMA_VERSION
    ):
        raise CertificationError("config")
    signing_identity = _string(
        root["signing_identity"], "identity", maximum=MAX_IDENTITY_BYTES
    )
    normalized_identity = re.sub(r"[^a-z0-9]", "", signing_identity.casefold())
    if (
        not signing_identity.strip()
        or signing_identity != signing_identity.strip()
        or signing_identity.startswith("-")
        or unicodedata.normalize("NFC", signing_identity) != signing_identity
        or normalized_identity == "adhoc"
        or any(
            unicodedata.category(character).startswith("C")
            for character in signing_identity
        )
    ):
        raise CertificationError("identity")

    profile = _absolute_path(root["provisioning_profile"], "profile")
    profile_fd, profile_identity = _open_regular(
        profile,
        category="profile",
        maximum=MAX_PROFILE_BYTES,
        private=True,
    )
    os.close(profile_fd)
    if profile_identity.size == 0:
        raise CertificationError("profile")

    fixture_value = _object(root["fixture"], ("executable", "sha256"), "fixture")
    fixture_path = _absolute_path(fixture_value["executable"], "fixture")
    expected_sha256 = _string(fixture_value["sha256"], "fixture", maximum=64)
    if SHA256_RE.fullmatch(expected_sha256) is None:
        raise CertificationError("fixture")
    fixture_fd, fixture_identity = _open_regular(
        fixture_path,
        category="fixture",
        maximum=MAX_FIXTURE_BYTES,
        private=False,
        executable=True,
        digest=True,
    )
    os.close(fixture_fd)
    if fixture_identity.size == 0 or fixture_identity.sha256 != expected_sha256:
        raise CertificationError("fixture")

    optional_value = _object(
        root["optional_cases"],
        (
            "bridged_interface",
            "host_connectivity",
            "not_authorized",
            "sharing_service_busy",
        ),
        "optional-cases",
    )
    bridge_value = optional_value["bridged_interface"]
    if bridge_value is not None:
        bridge_value = _string(bridge_value, "optional-cases", maximum=15)
        if BRIDGE_RE.fullmatch(bridge_value) is None:
            raise CertificationError("optional-cases")
    optional_cases = OptionalCases(
        host_connectivity=_bool(
            optional_value["host_connectivity"], "optional-cases"
        ),
        bridged_interface=bridge_value,
        not_authorized=_bool(optional_value["not_authorized"], "optional-cases"),
        sharing_service_busy=_bool(
            optional_value["sharing_service_busy"], "optional-cases"
        ),
    )
    return CertificationConfig(
        signing_identity=signing_identity,
        provisioning_profile=profile,
        provisioning_profile_identity=profile_identity,
        fixture=FixtureConfig(fixture_path, expected_sha256, fixture_identity),
        optional_cases=optional_cases,
        timeouts=_parse_timeouts(root["timeouts"]),
    )


def read_config(path: Path) -> CertificationConfig:
    document, _identity = _read_private_document(path)
    return parse_config_document(document)


def _closed_version(value: object, label: str) -> str:
    text = _string(value, label, maximum=64)
    if PLATFORM_VERSION_RE.fullmatch(text) is None:
        raise CertificationError(label)
    return text


def validate_result_document(document: object) -> dict[str, Any]:
    root = _object(
        document,
        (
            "cases",
            "cleanup",
            "entitlements",
            "platform",
            "schema_version",
            "source",
            "verdict",
        ),
        "result",
    )
    if (
        _integer(
            root["schema_version"], SCHEMA_VERSION, SCHEMA_VERSION, "result"
        )
        != SCHEMA_VERSION
    ):
        raise CertificationError("result")
    source = _object(root["source"], ("commit", "tree"), "result")
    if any(
        not isinstance(source[key], str) or GIT_OBJECT_RE.fullmatch(source[key]) is None
        for key in ("commit", "tree")
    ):
        raise CertificationError("result")
    platform = _object(
        root["platform"], ("architecture", "hvf", "macos", "sdk"), "result"
    )
    if (
        platform["architecture"] != "arm64"
        or platform["hvf"] != "supported"
    ):
        raise CertificationError("result")
    _closed_version(platform["macos"], "result")
    _closed_version(platform["sdk"], "result")
    entitlements = _object(
        root["entitlements"],
        ("outer_empty", "worker_app_sandbox_hvf", "worker_vmnet"),
        "result",
    )
    if not all(_bool(value, "result") for value in entitlements.values()):
        raise CertificationError("result")
    cases = root["cases"]
    if not isinstance(cases, list) or len(cases) != len(CASE_NAMES):
        raise CertificationError("result")
    outcomes: dict[str, str] = {}
    for index, value in enumerate(cases):
        item = _object(value, ("name", "outcome"), "result")
        name = item["name"]
        outcome = item["outcome"]
        if (
            name != CASE_NAMES[index]
            or not isinstance(outcome, str)
            or outcome not in CASE_OUTCOMES
            or (
                outcome == "environment-gated"
                and name not in ENVIRONMENT_GATED_CASES
            )
        ):
            raise CertificationError("result")
        outcomes[name] = outcome
    cleanup = root["cleanup"]
    verdict = root["verdict"]
    if cleanup not in ("complete", "incomplete") or verdict not in (
        "passed",
        "blocked",
        "failed",
    ):
        raise CertificationError("result")
    mandatory_outcomes = [
        outcome for name, outcome in outcomes.items() if name not in ENVIRONMENT_GATED_CASES
    ]
    optional_outcomes = [outcomes[name] for name in ENVIRONMENT_GATED_CASES]
    expected_verdict = (
        "failed"
        if cleanup == "incomplete" or "failed" in outcomes.values()
        else "blocked"
        if "blocked" in outcomes.values()
        else "passed"
    )
    if (
        verdict != expected_verdict
        or (
            verdict == "passed"
            and (
                any(outcome != "passed" for outcome in mandatory_outcomes)
                or any(
                    outcome not in ("passed", "environment-gated")
                    for outcome in optional_outcomes
                )
            )
        )
    ):
        raise CertificationError("result")
    return root


def read_result(path: Path) -> dict[str, Any]:
    if not path.is_absolute():
        raise CertificationError("result")
    fd, identity = _open_regular(
        path,
        category="result",
        maximum=MAX_DOCUMENT_BYTES,
        private=True,
    )
    try:
        data_buffer = bytearray()
        while True:
            chunk = os.read(
                fd, min(4096, MAX_DOCUMENT_BYTES + 1 - len(data_buffer))
            )
            if not chunk:
                break
            data_buffer.extend(chunk)
            if len(data_buffer) > MAX_DOCUMENT_BYTES:
                raise CertificationError("result")
        after = os.fstat(fd)
        if (
            after.st_dev != identity.device
            or after.st_ino != identity.inode
            or after.st_size != identity.size
            or after.st_mtime_ns != identity.mtime_ns
            or after.st_ctime_ns != identity.ctime_ns
        ):
            raise CertificationError("result")
    finally:
        os.close(fd)
    document = _decode_document(bytes(data_buffer))
    return validate_result_document(document)


def _owned_unlink_at(
    directory_fd: int, name: str, identity: FileIdentity, category: str
) -> None:
    try:
        current = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError as error:
        raise CertificationError(category) from error
    if current.st_dev != identity.device or current.st_ino != identity.inode:
        raise CertificationError(category)
    try:
        os.unlink(name, dir_fd=directory_fd)
    except OSError as error:
        raise CertificationError(category) from error


def publish_result(path: Path, document: Mapping[str, object]) -> None:
    validated = validate_result_document(dict(document))
    data = canonical_json(validated)
    if (
        len(data) > MAX_DOCUMENT_BYTES
        or not path.is_absolute()
        or path.name in ("", ".", "..")
    ):
        raise CertificationError("output")
    parent = path.parent
    directory_fd = -1
    try:
        parent_metadata = os.lstat(parent)
        directory_fd = os.open(
            parent,
            os.O_RDONLY
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_DIRECTORY", 0)
            | getattr(os, "O_NOFOLLOW", 0),
        )
    except OSError as error:
        raise CertificationError("output") from error
    try:
        current_parent = os.fstat(directory_fd)
    except OSError as error:
        os.close(directory_fd)
        raise CertificationError("output") from error
    if (
        not stat.S_ISDIR(parent_metadata.st_mode)
        or stat.S_ISLNK(parent_metadata.st_mode)
        or current_parent.st_dev != parent_metadata.st_dev
        or current_parent.st_ino != parent_metadata.st_ino
    ):
        os.close(directory_fd)
        raise CertificationError("output")

    def recheck_parent() -> None:
        try:
            visible_parent = os.lstat(parent)
        except OSError as error:
            raise CertificationError("output") from error
        if (
            visible_parent.st_dev != current_parent.st_dev
            or visible_parent.st_ino != current_parent.st_ino
            or not stat.S_ISDIR(visible_parent.st_mode)
            or stat.S_ISLNK(visible_parent.st_mode)
        ):
            raise CertificationError("output")

    try:
        os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        pass
    except OSError as error:
        os.close(directory_fd)
        raise CertificationError("output") from error
    else:
        os.close(directory_fd)
        raise CertificationError("output")

    fd = -1
    stage_name: Optional[str] = None
    stage_identity: Optional[FileIdentity] = None
    output_identity: Optional[FileIdentity] = None
    committed = False
    cleanup_changed = False
    cleanup_error: Optional[CertificationError] = None
    try:
        flags = (
            os.O_RDWR
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        for _attempt in range(128):
            candidate = f".{path.name}.{secrets.token_hex(16)}"
            try:
                fd = os.open(
                    candidate,
                    flags,
                    PRIVATE_FILE_MODE,
                    dir_fd=directory_fd,
                )
            except FileExistsError:
                continue
            except OSError as error:
                raise CertificationError("output") from error
            stage_name = candidate
            break
        if fd < 0 or stage_name is None:
            raise CertificationError("output")
        opened = os.fstat(fd)
        stage_identity = FileIdentity(
            opened.st_dev, opened.st_ino, opened.st_size
        )
        os.fchmod(fd, PRIVATE_FILE_MODE)
        metadata = os.fstat(fd)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != PRIVATE_FILE_MODE
        ):
            raise CertificationError("output")
        stage_identity = FileIdentity(
            metadata.st_dev, metadata.st_ino, metadata.st_size
        )
        view = memoryview(data)
        while view:
            written = os.write(fd, view)
            if written <= 0:
                raise CertificationError("output")
            view = view[written:]
        os.fsync(fd)
        current = os.fstat(fd)
        stage_identity = FileIdentity(
            current.st_dev, current.st_ino, current.st_size
        )
        visible = os.stat(
            stage_name, dir_fd=directory_fd, follow_symlinks=False
        )
        if visible.st_dev != current.st_dev or visible.st_ino != current.st_ino:
            raise CertificationError("output")
        recheck_parent()
        try:
            os.link(
                stage_name,
                path.name,
                src_dir_fd=directory_fd,
                dst_dir_fd=directory_fd,
                follow_symlinks=False,
            )
        except OSError as error:
            raise CertificationError("output") from error
        output_identity = stage_identity
        published = os.stat(
            path.name, dir_fd=directory_fd, follow_symlinks=False
        )
        if (
            published.st_dev != output_identity.device
            or published.st_ino != output_identity.inode
            or published.st_size != len(data)
            or published.st_nlink != 2
            or stat.S_IMODE(published.st_mode) != PRIVATE_FILE_MODE
        ):
            raise CertificationError("output")
        closing_fd = fd
        fd = -1
        try:
            os.close(closing_fd)
        except OSError as error:
            raise CertificationError("output") from error
        _owned_unlink_at(directory_fd, stage_name, stage_identity, "output")
        stage_name = None
        published = os.stat(
            path.name, dir_fd=directory_fd, follow_symlinks=False
        )
        if (
            published.st_dev != output_identity.device
            or published.st_ino != output_identity.inode
            or published.st_size != len(data)
            or published.st_nlink != 1
            or stat.S_IMODE(published.st_mode) != PRIVATE_FILE_MODE
        ):
            raise CertificationError("output")
        os.fsync(directory_fd)
        recheck_parent()
        committed = True
    except OSError as error:
        raise CertificationError("output") from error
    finally:
        if fd >= 0:
            try:
                os.close(fd)
            except OSError:
                cleanup_error = CertificationError("output")
        if not committed and output_identity is not None:
            try:
                _owned_unlink_at(
                    directory_fd, path.name, output_identity, "output"
                )
                cleanup_changed = True
            except CertificationError as error:
                cleanup_error = error
        if stage_name is not None and stage_identity is not None:
            try:
                _owned_unlink_at(
                    directory_fd, stage_name, stage_identity, "output"
                )
                cleanup_changed = True
            except CertificationError as error:
                cleanup_error = cleanup_error or error
        if cleanup_changed:
            try:
                os.fsync(directory_fd)
            except OSError:
                cleanup_error = cleanup_error or CertificationError("output")
        if directory_fd >= 0:
            closing_directory_fd = directory_fd
            directory_fd = -1
            try:
                os.close(closing_directory_fd)
            except OSError:
                if not committed:
                    cleanup_error = cleanup_error or CertificationError("output")
        if cleanup_error is not None:
            raise cleanup_error


def _valid_endpoint(address: ipaddress.IPv4Address) -> bool:
    return not (
        address.is_unspecified
        or address.is_multicast
        or address.is_loopback
        or int(address) == 0xFFFF_FFFF
    )


def encode_guest_control(mode: str, endpoint_ipv4: str, endpoint_port: int, nonce: bytes) -> bytes:
    mode_value = CONTROL_MODES.get(mode) if isinstance(mode, str) else None
    if (
        mode_value is None
        or not isinstance(endpoint_ipv4, str)
        or isinstance(endpoint_port, bool)
        or not isinstance(endpoint_port, int)
        or not 1 <= endpoint_port <= 65535
        or not isinstance(nonce, bytes)
    ):
        raise CertificationError("control")
    try:
        address = ipaddress.IPv4Address(endpoint_ipv4)
    except ipaddress.AddressValueError as error:
        raise CertificationError("control") from error
    if not _valid_endpoint(address) or len(nonce) != 32 or not any(nonce):
        raise CertificationError("control")
    prefix = CONTROL_PREFIX.pack(
        CONTROL_MAGIC,
        SCHEMA_VERSION,
        mode_value,
        4,
        address.packed,
        endpoint_port,
        nonce,
        bytes(14),
    )
    digest = hashlib.sha256(prefix).digest()
    return prefix + digest + bytes(CONTROL_BYTES - len(prefix) - len(digest))


def decode_guest_control(data: bytes) -> GuestControl:
    if not isinstance(data, bytes) or len(data) != CONTROL_BYTES:
        raise CertificationError("control")
    prefix = data[:CONTROL_PREFIX_BYTES]
    digest = data[CONTROL_PREFIX_BYTES : CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES]
    tail = data[CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES :]
    try:
        magic, version, mode_value, family, raw_address, port, nonce, reserved = (
            CONTROL_PREFIX.unpack(prefix)
        )
        address = ipaddress.IPv4Address(raw_address)
    except (struct.error, ipaddress.AddressValueError) as error:
        raise CertificationError("control") from error
    mode = CONTROL_MODE_NAMES.get(mode_value)
    if (
        magic != CONTROL_MAGIC
        or version != SCHEMA_VERSION
        or mode is None
        or family != 4
        or not _valid_endpoint(address)
        or port == 0
        or not any(nonce)
        or any(reserved)
        or digest != hashlib.sha256(prefix).digest()
        or any(tail)
    ):
        raise CertificationError("control")
    return GuestControl(mode, str(address), port, nonce)


def tcp_request(nonce: bytes) -> bytes:
    if not isinstance(nonce, bytes) or len(nonce) != 32 or not any(nonce):
        raise CertificationError("control")
    return TCP_REQUEST_MAGIC + nonce


def tcp_response(nonce: bytes) -> bytes:
    if not isinstance(nonce, bytes) or len(nonce) != 32 or not any(nonce):
        raise CertificationError("control")
    return TCP_RESPONSE_MAGIC + nonce


def _fixture_base(kind: str, case: str, nonce_hex: str) -> dict[str, object]:
    if (
        not isinstance(case, str)
        or case not in FIXTURE_CASES
        or not isinstance(nonce_hex, str)
        or SHA256_RE.fullmatch(nonce_hex) is None
    ):
        raise CertificationError("fixture-protocol")
    return {
        "case": case,
        "kind": kind,
        "nonce": nonce_hex,
        "schema_version": SCHEMA_VERSION,
    }


def fixture_prepare(case: str, nonce_hex: str, bridge_interface: Optional[str] = None) -> bytes:
    message = _fixture_base("prepare", case, nonce_hex)
    if case == "bridged-connectivity":
        if (
            not isinstance(bridge_interface, str)
            or BRIDGE_RE.fullmatch(bridge_interface) is None
        ):
            raise CertificationError("fixture-protocol")
        message["bridge_interface"] = bridge_interface
    elif bridge_interface is not None:
        raise CertificationError("fixture-protocol")
    return canonical_line(message)


def fixture_signal(kind: str, case: str, nonce_hex: str) -> bytes:
    if kind not in ("cleanup",):
        raise CertificationError("fixture-protocol")
    return canonical_line(_fixture_base(kind, case, nonce_hex))


def parse_fixture_message(
    data: bytes,
    *,
    expected_kind: str,
    expected_case: str,
    expected_nonce: str,
) -> Optional[FixtureEndpoint]:
    if (
        expected_kind not in ("ready", "observed", "complete")
        or not isinstance(expected_case, str)
        or expected_case not in FIXTURE_CASES
        or not isinstance(expected_nonce, str)
        or SHA256_RE.fullmatch(expected_nonce) is None
    ):
        raise CertificationError("fixture-protocol")
    try:
        document = _decode_document(data, line=True)
    except CertificationError as error:
        raise CertificationError("fixture-protocol") from error
    required = ["case", "kind", "nonce", "schema_version"]
    optional: tuple[str, ...] = ()
    if expected_kind == "ready" and expected_case in CONNECTIVITY_CASES:
        required.extend(("endpoint_ipv4", "endpoint_port"))
    root = _object(document, tuple(required), "fixture-protocol", optional=optional)
    if (
        _integer(
            root["schema_version"],
            SCHEMA_VERSION,
            SCHEMA_VERSION,
            "fixture-protocol",
        )
        != SCHEMA_VERSION
        or root["kind"] != expected_kind
        or root["case"] != expected_case
        or root["nonce"] != expected_nonce
    ):
        raise CertificationError("fixture-protocol")
    if expected_kind == "ready" and expected_case in CONNECTIVITY_CASES:
        address_text = _string(root["endpoint_ipv4"], "fixture-protocol", maximum=15)
        port = _integer(root["endpoint_port"], 1, 65535, "fixture-protocol")
        try:
            address = ipaddress.IPv4Address(address_text)
        except ipaddress.AddressValueError as error:
            raise CertificationError("fixture-protocol") from error
        if not _valid_endpoint(address) or str(address) != address_text:
            raise CertificationError("fixture-protocol")
        return FixtureEndpoint(address_text, port)
    return None


def _minimal_fixture_environment(directory: Path) -> dict[str, str]:
    return {
        "HOME": os.fspath(directory),
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": FIXTURE_PATH,
        "TMPDIR": os.fspath(directory),
    }


class FixtureSession:
    """One exact retained external fixture process."""

    def __init__(
        self,
        fixture: FixtureConfig,
        case: str,
        nonce: bytes,
        timeout_seconds: int,
        *,
        bridge_interface: Optional[str] = None,
        session_parent: Optional[Path] = None,
        terminate_seconds: int = 5,
        clock: Callable[[], float] = time.monotonic,
        popen_factory: Callable[..., subprocess.Popen[bytes]] = subprocess.Popen,
    ) -> None:
        if (
            not isinstance(fixture, FixtureConfig)
            or not isinstance(case, str)
            or case not in FIXTURE_CASES
            or not isinstance(nonce, bytes)
            or len(nonce) != 32
            or not any(nonce)
            or isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= MAX_TIMEOUT_SECONDS
            or isinstance(terminate_seconds, bool)
            or not isinstance(terminate_seconds, int)
            or not 1 <= terminate_seconds <= MAX_SHORT_TIMEOUT_SECONDS
        ):
            raise CertificationError("fixture-protocol")
        if case == "bridged-connectivity":
            if (
                not isinstance(bridge_interface, str)
                or BRIDGE_RE.fullmatch(bridge_interface) is None
            ):
                raise CertificationError("fixture-protocol")
        elif bridge_interface is not None:
            raise CertificationError("fixture-protocol")
        self._fixture = fixture
        self._case = case
        self._nonce = nonce.hex()
        self._bridge_interface = bridge_interface
        self._clock = clock
        self._deadline = clock() + timeout_seconds
        self._terminate_seconds = terminate_seconds
        self._stdout = bytearray()
        self._stderr = bytearray()
        self._stdout_count = 0
        self._stderr_count = 0
        self._stdout_open = True
        self._stderr_open = True
        self._state = "new"
        self._process: Optional[subprocess.Popen[bytes]] = None
        self._directory: Optional[Path] = None
        self._directory_identity: Optional[FileIdentity] = None

        self._recheck_fixture()
        parent = session_parent
        try:
            raw_directory = tempfile.mkdtemp(
                prefix="bangbang-production-vmnet-fixture.",
                dir=None if parent is None else parent,
            )
            directory = Path(raw_directory)
            metadata = os.lstat(directory)
            self._directory = directory
            self._directory_identity = FileIdentity(
                metadata.st_dev, metadata.st_ino, metadata.st_size
            )
            os.chmod(directory, PRIVATE_DIRECTORY_MODE)
            metadata = os.lstat(directory)
            if (
                not stat.S_ISDIR(metadata.st_mode)
                or stat.S_ISLNK(metadata.st_mode)
                or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
            ):
                raise CertificationError("fixture")
            process = popen_factory(
                (os.fspath(fixture.executable),),
                cwd=directory,
                env=_minimal_fixture_environment(directory),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                bufsize=0,
            )
            self._process = process
            if process.stdin is None or process.stdout is None or process.stderr is None:
                raise CertificationError("fixture")
            for stream in (process.stdin, process.stdout, process.stderr):
                os.set_blocking(stream.fileno(), False)
            self._state = "started"
        except BaseException:
            cleanup_error: Optional[CertificationError] = None
            try:
                self._force_finish()
            except CertificationError as error:
                cleanup_error = error
            try:
                self._cleanup_directory()
            except CertificationError as error:
                cleanup_error = cleanup_error or error
            if cleanup_error is not None:
                raise cleanup_error
            raise

    def _remaining(self) -> float:
        remaining = self._deadline - self._clock()
        if remaining <= 0:
            raise CertificationError("fixture-timeout")
        return remaining

    def _recheck_fixture(self) -> None:
        fd, current = _open_regular(
            self._fixture.executable,
            category="fixture",
            maximum=MAX_FIXTURE_BYTES,
            private=False,
            executable=True,
            digest=True,
        )
        os.close(fd)
        expected = self._fixture.identity
        if (
            current.device != expected.device
            or current.inode != expected.inode
            or current.size != expected.size
            or current.sha256 != self._fixture.expected_sha256
            or current.mtime_ns != expected.mtime_ns
            or current.ctime_ns != expected.ctime_ns
        ):
            raise CertificationError("fixture")

    def _write(self, data: bytes) -> None:
        process = self._process
        if process is None or process.stdin is None:
            raise CertificationError("fixture-protocol")
        view = memoryview(data)
        while view:
            if process.poll() is not None:
                raise CertificationError("fixture-protocol")
            _readable, writable, _exceptional = select.select(
                [], [process.stdin.fileno()], [], self._remaining()
            )
            if not writable:
                raise CertificationError("fixture-timeout")
            try:
                count = os.write(process.stdin.fileno(), view)
            except BlockingIOError:
                continue
            except OSError as error:
                raise CertificationError("fixture-protocol") from error
            if count <= 0:
                raise CertificationError("fixture-protocol")
            view = view[count:]

    def _read_line(self) -> bytes:
        process = self._process
        if process is None or process.stdout is None or process.stderr is None:
            raise CertificationError("fixture-protocol")
        stdout_fd = process.stdout.fileno()
        stderr_fd = process.stderr.fileno()
        while True:
            newline = self._stdout.find(b"\n")
            if newline >= 0:
                line = bytes(self._stdout[: newline + 1])
                del self._stdout[: newline + 1]
                if len(line) > MAX_FIXTURE_LINE_BYTES:
                    raise CertificationError("fixture-protocol")
                return line
            if len(self._stdout) >= MAX_FIXTURE_LINE_BYTES:
                raise CertificationError("fixture-protocol")
            descriptors = []
            if self._stdout_open:
                descriptors.append(stdout_fd)
            if self._stderr_open:
                descriptors.append(stderr_fd)
            if not descriptors:
                raise CertificationError("fixture-protocol")
            readable, _writable, _exceptional = select.select(
                descriptors, [], [], self._remaining()
            )
            if not readable:
                raise CertificationError("fixture-timeout")
            for descriptor in readable:
                try:
                    chunk = os.read(descriptor, 4096)
                except BlockingIOError:
                    continue
                except OSError as error:
                    raise CertificationError("fixture-protocol") from error
                if descriptor == stderr_fd:
                    self._stderr.extend(chunk)
                    self._stderr_count += len(chunk)
                    if self._stderr_count > MAX_FIXTURE_CAPTURE_BYTES:
                        raise CertificationError("fixture-protocol")
                    if chunk:
                        raise CertificationError("fixture-protocol")
                    self._stderr_open = False
                elif chunk:
                    self._stdout.extend(chunk)
                    self._stdout_count += len(chunk)
                    if self._stdout_count > MAX_FIXTURE_CAPTURE_BYTES:
                        raise CertificationError("fixture-protocol")
                else:
                    self._stdout_open = False

    def prepare(self) -> Optional[FixtureEndpoint]:
        try:
            if self._state != "started":
                raise CertificationError("fixture-protocol")
            self._write(
                fixture_prepare(
                    self._case,
                    self._nonce,
                    bridge_interface=self._bridge_interface,
                )
            )
            self._state = "prepare-sent"
            endpoint = parse_fixture_message(
                self._read_line(),
                expected_kind="ready",
                expected_case=self._case,
                expected_nonce=self._nonce,
            )
            self._state = "ready"
            return endpoint
        except BaseException:
            try:
                self.abort()
            except CertificationError as error:
                raise error
            raise

    def wait_observed(self) -> None:
        try:
            if self._state != "ready":
                raise CertificationError("fixture-protocol")
            endpoint = parse_fixture_message(
                self._read_line(),
                expected_kind="observed",
                expected_case=self._case,
                expected_nonce=self._nonce,
            )
            if endpoint is not None:
                raise CertificationError("fixture-protocol")
            self._state = "observed"
        except BaseException:
            try:
                self.abort()
            except CertificationError as error:
                raise error
            raise

    def complete(self) -> None:
        try:
            if self._state != "observed":
                raise CertificationError("fixture-protocol")
            self._write(fixture_signal("cleanup", self._case, self._nonce))
            self._state = "cleanup-sent"
            endpoint = parse_fixture_message(
                self._read_line(),
                expected_kind="complete",
                expected_case=self._case,
                expected_nonce=self._nonce,
            )
            if endpoint is not None or self._stdout:
                raise CertificationError("fixture-protocol")
            self._state = "complete"
            self._finish_clean_exit()
        except BaseException:
            try:
                self.abort()
            except CertificationError as error:
                raise error
            raise

    def _close_streams(self) -> None:
        process = self._process
        if process is None:
            return
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError):
                    pass

    def _finish_clean_exit(self) -> None:
        process = self._process
        if process is None or process.stdin is None:
            raise CertificationError("fixture-cleanup")
        try:
            process.stdin.close()
            process.stdin = None
            stdout_tail, stderr_tail = process.communicate(timeout=self._remaining())
        except subprocess.TimeoutExpired as error:
            raise CertificationError("fixture-cleanup") from error
        except (OSError, ValueError) as error:
            raise CertificationError("fixture-cleanup") from error
        stdout_tail = stdout_tail or b""
        stderr_tail = stderr_tail or b""
        if (
            self._stdout_count + len(stdout_tail) > MAX_FIXTURE_CAPTURE_BYTES
            or self._stderr_count + len(stderr_tail) > MAX_FIXTURE_CAPTURE_BYTES
        ):
            raise CertificationError("fixture-protocol")
        self._stdout_count += len(stdout_tail)
        self._stderr_count += len(stderr_tail)
        self._stdout.extend(stdout_tail)
        self._stderr.extend(stderr_tail)
        if self._stdout or self._stderr:
            raise CertificationError("fixture-protocol")
        if process.returncode != 0:
            raise CertificationError("fixture-cleanup")
        self._recheck_fixture()
        self._close_streams()
        self._process = None
        self._cleanup_directory()

    def _signal_group(self, number: int) -> bool:
        process = self._process
        if process is None:
            return False
        try:
            os.killpg(process.pid, number)
            return True
        except ProcessLookupError:
            return False
        except OSError as error:
            raise CertificationError("fixture-cleanup") from error

    def _group_exists(self) -> bool:
        process = self._process
        if process is None:
            return False
        try:
            os.killpg(process.pid, 0)
            return True
        except ProcessLookupError:
            return False
        except OSError as error:
            raise CertificationError("fixture-cleanup") from error

    def _force_finish(self) -> None:
        process = self._process
        if process is None:
            return
        cleanup_error: Optional[CertificationError] = None
        if process.stdin is not None:
            try:
                process.stdin.close()
                process.stdin = None
            except (OSError, ValueError):
                cleanup_error = CertificationError("fixture-cleanup")
        if process.poll() is None:
            try:
                process.wait(timeout=min(0.25, self._terminate_seconds))
            except subprocess.TimeoutExpired:
                pass
            except OSError:
                cleanup_error = cleanup_error or CertificationError(
                    "fixture-cleanup"
                )
        try:
            if self._group_exists():
                self._signal_group(signal.SIGTERM)
        except CertificationError as error:
            cleanup_error = cleanup_error or error
        if process.poll() is None:
            try:
                process.wait(timeout=self._terminate_seconds)
            except subprocess.TimeoutExpired:
                pass
            except OSError:
                cleanup_error = cleanup_error or CertificationError(
                    "fixture-cleanup"
                )
        try:
            if self._group_exists():
                self._signal_group(signal.SIGKILL)
        except CertificationError as error:
            cleanup_error = cleanup_error or error
        if process.poll() is None:
            try:
                process.wait(timeout=self._terminate_seconds)
            except (subprocess.TimeoutExpired, OSError):
                cleanup_error = cleanup_error or CertificationError(
                    "fixture-cleanup"
                )
        self._close_streams()
        if process.poll() is None:
            try:
                process.wait(timeout=min(0.25, self._terminate_seconds))
            except (subprocess.TimeoutExpired, OSError):
                cleanup_error = cleanup_error or CertificationError(
                    "fixture-cleanup"
                )
        group_exists = True
        try:
            deadline = time.monotonic() + self._terminate_seconds
            while self._group_exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            group_exists = self._group_exists()
        except CertificationError as error:
            cleanup_error = cleanup_error or error
        leader_reaped = process.poll() is not None
        if leader_reaped:
            try:
                process.wait(timeout=0)
            except (subprocess.TimeoutExpired, OSError):
                cleanup_error = cleanup_error or CertificationError(
                    "fixture-cleanup"
                )
            self._process = None
        else:
            cleanup_error = cleanup_error or CertificationError("fixture-cleanup")
        if group_exists:
            cleanup_error = cleanup_error or CertificationError("fixture-cleanup")
        if leader_reaped and not group_exists:
            cleanup_error = None
        if cleanup_error is not None:
            raise cleanup_error

    def _cleanup_directory(self) -> None:
        directory = self._directory
        identity = self._directory_identity
        if directory is None or identity is None:
            return
        try:
            current = os.lstat(directory)
            if current.st_dev != identity.device or current.st_ino != identity.inode:
                raise CertificationError("fixture-cleanup")
            if any(directory.iterdir()):
                raise CertificationError("fixture-cleanup")
            os.rmdir(directory)
        except FileNotFoundError:
            raise CertificationError("fixture-cleanup")
        except OSError as error:
            raise CertificationError("fixture-cleanup") from error
        self._directory = None
        self._directory_identity = None

    def abort(self) -> None:
        cleanup_error: Optional[CertificationError] = None
        process = self._process
        if process is not None and process.poll() is None:
            try:
                if self._state not in ("cleanup-sent", "complete"):
                    self._write(fixture_signal("cleanup", self._case, self._nonce))
                    self._state = "cleanup-sent"
                if self._state == "cleanup-sent":
                    completed = False
                    for _message in range(4):
                        try:
                            endpoint = parse_fixture_message(
                                self._read_line(),
                                expected_kind="complete",
                                expected_case=self._case,
                                expected_nonce=self._nonce,
                            )
                        except CertificationError:
                            continue
                        if endpoint is None:
                            completed = True
                            break
                    if not completed:
                        raise CertificationError("fixture-protocol")
                    self._state = "complete"
                if self._state == "complete":
                    self._finish_clean_exit()
                    self._state = "aborted"
                    return
            except BaseException:
                pass
        try:
            self._force_finish()
        except CertificationError as error:
            cleanup_error = error
        try:
            self._cleanup_directory()
        except CertificationError as error:
            cleanup_error = cleanup_error or error
        if cleanup_error is not None:
            raise cleanup_error
        self._state = "aborted"

    def __enter__(self) -> "FixtureSession":
        return self

    def __exit__(self, *_exception: object) -> None:
        if self._process is not None or self._directory is not None:
            self.abort()


def _parser() -> RedactedArgumentParser:
    parser = RedactedArgumentParser(
        description="Validate production-vmnet certification contract files."
    )
    subparsers = parser.add_subparsers(dest="operation", required=True)
    config = subparsers.add_parser("validate-config")
    config.add_argument("--config", type=Path, required=True)
    result = subparsers.add_parser("validate-result")
    result.add_argument("--result", type=Path, required=True)
    return parser


def _has_duplicate_path_option(arguments: Sequence[str], option: str) -> bool:
    count = sum(
        argument == option or argument.startswith(option + "=")
        for argument in arguments
    )
    return count > 1


def main(argv: Optional[Sequence[str]] = None) -> int:
    arguments = tuple(sys.argv[1:] if argv is None else argv)
    failure: Optional[CertificationError] = None
    try:
        if _has_duplicate_path_option(arguments, "--config") or _has_duplicate_path_option(
            arguments, "--result"
        ):
            raise CertificationError("invocation")
        args = _parser().parse_args(arguments)
        if args.operation == "validate-config":
            read_config(args.config)
            print("bangbang production vmnet config: valid")
        elif args.operation == "validate-result":
            read_result(args.result)
            print("bangbang production vmnet result: valid")
        else:  # pragma: no cover - argparse owns the closed operation set.
            raise CertificationError("invocation")
    except CertificationError as error:
        failure = error
    except SystemExit:
        raise
    except BaseException:
        failure = CertificationError("internal")
    if failure is not None:
        operation = arguments[0] if arguments else ""
        label = (
            "config"
            if failure.category != "invocation" and operation == "validate-config"
            else "result"
            if failure.category != "invocation" and operation == "validate-result"
            else "invocation"
        )
        print(
            f"bangbang production vmnet {label}: invalid category={failure.category}",
            file=sys.stderr,
        )
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
