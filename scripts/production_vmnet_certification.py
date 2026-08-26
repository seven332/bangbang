#!/usr/bin/env python3
"""Validate and run the fail-closed production-vmnet certification matrix.

The public CLI owns strict private input, retained external-fixture and guest
control protocols, real production-bundle orchestration, and a redacted
no-clobber result. Dependency injection is available only to portable tests;
the ``run`` operation always selects the system driver.
"""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import platform
import plistlib
import re
import secrets
import select
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Callable, Iterator, Mapping, Optional, Protocol, Sequence


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
REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
MAX_COMMAND_CAPTURE_BYTES = 64 * 1024
MAX_PROCESS_CAPTURE_BYTES = 64 * 1024
MAX_HTTP_REQUEST_BYTES = 32 * 1024
MAX_HTTP_RESPONSE_BYTES = 64 * 1024
MAX_HTTP_HEADERS = 64
MAX_SERIAL_BYTES = 64 * 1024
POLL_SECONDS = 0.02
PRODUCTION_SESSION_PARENT = Path("/private/tmp")
DARWIN_SOL_LOCAL = 0
DARWIN_LOCAL_PEERPID = 2

OUTER_BUNDLE_NAME = "Bangbang.app"
WORKER_BUNDLE_NAME = "BangbangWorker.app"
LAUNCHER_EXECUTABLE_NAME = "bangbang"
WORKER_EXECUTABLE_NAME = "bangbang-worker"
WORKER_BUNDLE_IDENTIFIER = "dev.bangbang.worker"
JAILER_OPTION = "--bangbang-jailer-v1"
GRANT_MANIFEST_OPTION = "--bangbang-grant-manifest"
API_READY_MARKER = b"status: API server listening\n"
GUEST_BEGIN_MARKER = b"BANGBANG_PRODUCTION_VMNET_CERTIFICATION_BEGIN\n"
GUEST_SUCCESS_MARKER = b"BANGBANG_PRODUCTION_VMNET_CERTIFICATION_OK\n"
GUEST_FAILURE_PREFIX = b"BANGBANG_PRODUCTION_VMNET_CERTIFICATION_FAIL_"
DIRECT_ROOTFS_BOOT_MARKER = b"BANGBANG_DIRECT_ROOTFS_BOOT_OK\n"
DIRECT_ROOTFS_BOOT_ARGS = (
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 "
    "init=/bangbang-direct-rootfs-init"
)
PRODUCTION_VMNET_BOOT_ARGS = (
    f"{DIRECT_ROOTFS_BOOT_ARGS} bangbang.production-vmnet-certification=1"
)

KERNEL_GRANT_ID = "cert-kernel"
ROOTFS_GRANT_ID = "cert-rootfs"
CONTROL_GRANT_ID = "cert-control"
SERIAL_GRANT_ID = "cert-serial"
API_DIRECTORY_GRANT_ID = "cert-api"
KERNEL_GRANT_REF = f"bangbang-grant:{KERNEL_GRANT_ID}"
ROOTFS_GRANT_REF = f"bangbang-grant:{ROOTFS_GRANT_ID}"
CONTROL_GRANT_REF = f"bangbang-grant:{CONTROL_GRANT_ID}"
SERIAL_GRANT_REF = f"bangbang-grant:{SERIAL_GRANT_ID}"
API_SOCKET_CHILD = "api.sock"
API_SOCKET_REF = f"bangbang-grant:{API_DIRECTORY_GRANT_ID}/{API_SOCKET_CHILD}"

APP_SANDBOX_ENTITLEMENT = "com.apple.security.app-sandbox"
HYPERVISOR_ENTITLEMENT = "com.apple.security.hypervisor"
VMNET_ENTITLEMENT = "com.apple.vm.networking"
APPLICATION_IDENTIFIER_ENTITLEMENT = "com.apple.application-identifier"
TEAM_IDENTIFIER_ENTITLEMENT = "com.apple.developer.team-identifier"
WORKER_ENTITLEMENT_KEYS = frozenset(
    {
        APP_SANDBOX_ENTITLEMENT,
        HYPERVISOR_ENTITLEMENT,
        VMNET_ENTITLEMENT,
        APPLICATION_IDENTIFIER_ENTITLEMENT,
        TEAM_IDENTIFIER_ENTITLEMENT,
    }
)

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
        if (
            not isinstance(category, str)
            or re.fullmatch(r"[a-z][a-z0-9-]{0,31}", category) is None
        ):
            category = "internal"
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


@dataclass(frozen=True)
class SourceIdentity:
    commit: str
    tree: str


@dataclass(frozen=True)
class PlatformIdentity:
    macos: str
    sdk: str
    architecture: str = "arm64"
    hvf: str = "supported"


@dataclass(frozen=True)
class PreparedArtifacts:
    kernel: Path
    rootfs: Path
    kernel_identity: FileIdentity
    rootfs_identity: FileIdentity


@dataclass(frozen=True)
class ProductionBundles:
    networkless: Path
    vmnet: Path


@dataclass(frozen=True)
class EntitlementAssertions:
    outer_empty: bool
    worker_app_sandbox_hvf: bool
    worker_vmnet: bool


@dataclass(frozen=True)
class CommandOutcome:
    returncode: int
    stdout: bytes
    stderr: bytes


@dataclass(frozen=True)
class HttpResponse:
    status: int
    body: bytes


class CertificationCaseDriver(Protocol):
    def execute(
        self,
        case: str,
        *,
        endpoint: Optional[FixtureEndpoint],
        nonce: bytes,
    ) -> None: ...

    def close(self) -> None: ...


@dataclass(frozen=True)
class CertificationDependencies:
    preflight: Callable[[], tuple[SourceIdentity, PlatformIdentity]]
    prepare_artifacts: Callable[[CertificationConfig], PreparedArtifacts]
    build_bundles: Callable[
        [CertificationConfig, "PrivateSession"], ProductionBundles
    ]
    inspect_bundles: Callable[[ProductionBundles], EntitlementAssertions]
    driver_factory: Callable[
        [
            CertificationConfig,
            "PrivateSession",
            PreparedArtifacts,
            ProductionBundles,
        ],
        CertificationCaseDriver,
    ]
    session_parent: Optional[Path]
    recheck_source: Callable[[SourceIdentity], None]
    fixture_popen_factory: Callable[..., subprocess.Popen[bytes]] = subprocess.Popen
    clock: Callable[[], float] = time.monotonic
    nonce_factory: Callable[[int], bytes] = secrets.token_bytes


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


class _BoundedCapture:
    def __init__(self, maximum: int) -> None:
        self._maximum = maximum
        self._data = bytearray()
        self._overflow = False
        self._error: Optional[BaseException] = None
        self._lock = threading.Lock()

    def append(self, data: bytes) -> None:
        with self._lock:
            available = max(0, self._maximum - len(self._data))
            self._data.extend(data[:available])
            if len(data) > available:
                self._overflow = True

    def fail(self, error: BaseException) -> None:
        with self._lock:
            self._error = error

    def result(self) -> tuple[bytes, bool, Optional[BaseException]]:
        with self._lock:
            return bytes(self._data), self._overflow, self._error


def _pump_capture(stream: BinaryIO, capture: _BoundedCapture) -> None:
    try:
        while True:
            chunk = os.read(stream.fileno(), 4096)
            if not chunk:
                return
            capture.append(chunk)
    except BaseException as error:  # pragma: no cover - defensive pipe failure
        capture.fail(error)


def _production_environment(*, temporary: Optional[Path] = None) -> dict[str, str]:
    home = os.environ.get("HOME", "")
    path = os.environ.get("PATH", "")
    if (
        not home
        or not Path(home).is_absolute()
        or len(os.fsencode(home)) > MAX_PATH_BYTES
        or not path
        or len(os.fsencode(path)) > MAX_PATH_BYTES
        or "\x00" in home
        or "\x00" in path
    ):
        raise CertificationError("environment")
    environment = {
        "HOME": home,
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": path,
    }
    if temporary is not None:
        environment["TMPDIR"] = os.fspath(temporary)
    return environment


def _signal_process_group(process: subprocess.Popen[bytes], number: int) -> None:
    try:
        os.killpg(process.pid, number)
    except ProcessLookupError:
        return
    except OSError as error:
        raise CertificationError("process-cleanup") from error


def _process_group_exists(process: subprocess.Popen[bytes]) -> bool:
    try:
        os.killpg(process.pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError as error:
        raise CertificationError("process-cleanup") from error
    return True


def _wait_process_group_absent(
    process: subprocess.Popen[bytes], deadline: float
) -> bool:
    while True:
        if not _process_group_exists(process):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(POLL_SECONDS)


def _terminate_process(
    process: subprocess.Popen[bytes], grace_seconds: float
) -> int:
    if process.poll() is None:
        _signal_process_group(process, signal.SIGTERM)
        try:
            process.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            _signal_process_group(process, signal.SIGKILL)
            try:
                process.wait(timeout=grace_seconds)
            except subprocess.TimeoutExpired as error:
                raise CertificationError("process-cleanup") from error
    else:
        try:
            process.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired as error:  # pragma: no cover - poll invariant
            raise CertificationError("process-cleanup") from error
    deadline = time.monotonic() + grace_seconds
    if not _wait_process_group_absent(process, deadline):
        _signal_process_group(process, signal.SIGKILL)
        if not _wait_process_group_absent(process, time.monotonic() + grace_seconds):
            raise CertificationError("process-cleanup")
    return process.returncode if process.returncode is not None else -1


def run_bounded_command(
    arguments: Sequence[str],
    *,
    timeout_seconds: float,
    phase: str,
    check: bool = True,
    environment: Optional[Mapping[str, str]] = None,
    cwd: Path = REPOSITORY_ROOT,
) -> CommandOutcome:
    if (
        not arguments
        or not phase
        or timeout_seconds <= 0
        or any(not isinstance(value, str) or "\x00" in value for value in arguments)
    ):
        raise CertificationError("internal")
    try:
        process = subprocess.Popen(
            tuple(arguments),
            cwd=cwd,
            env=dict(environment) if environment is not None else _production_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            bufsize=0,
        )
    except OSError as error:
        raise CertificationError("tool") from error
    if process.stdout is None or process.stderr is None:  # pragma: no cover - Popen contract
        _terminate_process(process, min(5.0, timeout_seconds))
        raise CertificationError("tool")

    stdout_capture = _BoundedCapture(MAX_COMMAND_CAPTURE_BYTES)
    stderr_capture = _BoundedCapture(MAX_COMMAND_CAPTURE_BYTES)
    threads = (
        threading.Thread(
            target=_pump_capture,
            args=(process.stdout, stdout_capture),
            name=f"bangbang-vmnet-{phase}-stdout",
            daemon=True,
        ),
        threading.Thread(
            target=_pump_capture,
            args=(process.stderr, stderr_capture),
            name=f"bangbang-vmnet-{phase}-stderr",
            daemon=True,
        ),
    )
    for thread in threads:
        thread.start()

    deadline = time.monotonic() + timeout_seconds
    timed_out = False
    reader_stuck = False
    try:
        while process.poll() is None:
            if time.monotonic() >= deadline:
                timed_out = True
                break
            time.sleep(POLL_SECONDS)
        if timed_out:
            _terminate_process(process, min(5.0, timeout_seconds))
        else:
            process.wait(timeout=min(5.0, timeout_seconds))
            if _process_group_exists(process):
                _terminate_process(process, min(5.0, timeout_seconds))
    except BaseException:
        try:
            _terminate_process(process, min(5.0, timeout_seconds))
        except CertificationError:
            pass
        raise
    finally:
        for thread in threads:
            thread.join(timeout=2)
        reader_stuck = any(thread.is_alive() for thread in threads)
        for stream in (process.stdout, process.stderr):
            try:
                stream.close()
            except (OSError, ValueError):
                pass
        if reader_stuck:
            for thread in threads:
                thread.join(timeout=0.25)
            reader_stuck = any(thread.is_alive() for thread in threads)

    stdout, stdout_overflow, stdout_error = stdout_capture.result()
    stderr, stderr_overflow, stderr_error = stderr_capture.result()
    if (
        timed_out
        or reader_stuck
        or stdout_error is not None
        or stderr_error is not None
    ):
        raise CertificationError("tool-timeout" if timed_out else "tool")
    if stdout_overflow or stderr_overflow:
        raise CertificationError("tool-output")
    outcome = CommandOutcome(process.returncode or 0, stdout, stderr)
    if check and outcome.returncode != 0:
        raise CertificationError("tool")
    return outcome


@dataclass(frozen=True)
class PrivateSession:
    path: Path
    device: int
    inode: int
    uid: int

    @classmethod
    def create(cls, parent: Optional[Path]) -> "PrivateSession":
        base = parent if parent is not None else PRODUCTION_SESSION_PARENT
        path: Optional[Path] = None
        try:
            path = Path(tempfile.mkdtemp(prefix="bbvmnet.", dir=base))
            os.chmod(path, PRIVATE_DIRECTORY_MODE)
            metadata = os.lstat(path)
        except OSError as error:
            if path is not None:
                try:
                    os.rmdir(path)
                except OSError:
                    pass
            raise CertificationError("session") from error
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
            or metadata.st_nlink < 2
        ):
            try:
                os.rmdir(path)
            except OSError:
                pass
            raise CertificationError("session")
        return cls(path, metadata.st_dev, metadata.st_ino, metadata.st_uid)

    def verify(self) -> None:
        try:
            metadata = os.lstat(self.path)
        except OSError as error:
            raise CertificationError("cleanup") from error
        if (
            metadata.st_dev != self.device
            or metadata.st_ino != self.inode
            or metadata.st_uid != self.uid
            or not stat.S_ISDIR(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
        ):
            raise CertificationError("cleanup")

    def cleanup(self) -> None:
        self.verify()
        _clean_directory(self.path)
        self.verify()
        try:
            os.rmdir(self.path)
        except OSError as error:
            raise CertificationError("cleanup") from error


def _clean_directory(path: Path) -> None:
    try:
        entries = list(os.scandir(path))
    except OSError as error:
        raise CertificationError("cleanup") from error
    for entry in entries:
        child = Path(entry.path)
        try:
            metadata = entry.stat(follow_symlinks=False)
            if stat.S_ISDIR(metadata.st_mode):
                _clean_directory(child)
                os.rmdir(child)
            else:
                os.unlink(child)
        except OSError as error:
            raise CertificationError("cleanup") from error


def _path_text(path: Path, category: str = "path") -> str:
    value = os.fspath(path)
    try:
        encoded = os.fsencode(value)
    except (TypeError, UnicodeEncodeError) as error:
        raise CertificationError(category) from error
    if (
        not path.is_absolute()
        or len(encoded) == 0
        or len(encoded) > MAX_PATH_BYTES
        or any(byte < 0x20 for byte in encoded)
    ):
        raise CertificationError(category)
    return value


def _write_private_file(path: Path, contents: bytes, *, executable: bool = False) -> FileIdentity:
    if not path.is_absolute() or len(contents) > MAX_DOCUMENT_BYTES:
        raise CertificationError("session")
    mode = 0o700 if executable else PRIVATE_FILE_MODE
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    fd = -1
    try:
        fd = os.open(path, flags, mode)
        os.fchmod(fd, mode)
        view = memoryview(contents)
        while view:
            written = os.write(fd, view)
            if written <= 0:
                raise CertificationError("session")
            view = view[written:]
        os.fsync(fd)
        metadata = os.fstat(fd)
        visible = os.lstat(path)
    except CertificationError:
        raise
    except OSError as error:
        raise CertificationError("session") from error
    finally:
        if fd >= 0:
            try:
                os.close(fd)
            except OSError as error:
                raise CertificationError("session") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != mode
        or metadata.st_size != len(contents)
        or visible.st_dev != metadata.st_dev
        or visible.st_ino != metadata.st_ino
    ):
        raise CertificationError("session")
    return FileIdentity(metadata.st_dev, metadata.st_ino, metadata.st_size)


def _verify_regular_artifact(path: Path, label: str) -> FileIdentity:
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise CertificationError("artifact") from error
    if (
        not path.is_absolute()
        or not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_size <= 0
        or metadata.st_nlink != 1
        or metadata.st_uid != os.getuid()
        or len(label) == 0
    ):
        raise CertificationError("artifact")
    return FileIdentity(
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        mtime_ns=metadata.st_mtime_ns,
        ctime_ns=metadata.st_ctime_ns,
    )


def _recheck_artifact(path: Path, expected: FileIdentity) -> None:
    current = _verify_regular_artifact(path, "retained")
    if (
        current.device != expected.device
        or current.inode != expected.inode
        or current.size != expected.size
        or current.mtime_ns != expected.mtime_ns
        or current.ctime_ns != expected.ctime_ns
    ):
        raise CertificationError("artifact")


def _one_path(outcome: CommandOutcome, label: str) -> Path:
    if outcome.returncode != 0:
        raise CertificationError("artifact")
    try:
        text = outcome.stdout.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CertificationError("artifact") from error
    lines = text.splitlines()
    if len(lines) != 1 or not lines[0] or "\x00" in lines[0] or not label:
        raise CertificationError("artifact")
    path = Path(lines[0])
    _path_text(path, "artifact")
    return path


def _closed_tool_line(outcome: CommandOutcome, category: str) -> str:
    if outcome.returncode != 0 or outcome.stderr:
        raise CertificationError(category)
    try:
        text = outcome.stdout.decode("ascii")
    except UnicodeDecodeError as error:
        raise CertificationError(category) from error
    lines = text.splitlines()
    if len(lines) != 1 or not lines[0] or len(lines[0]) > 128:
        raise CertificationError(category)
    return lines[0]


def _read_clean_source_identity() -> SourceIdentity:
    environment = _production_environment()
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
        outcome = run_bounded_command(
            arguments,
            timeout_seconds=10,
            phase="source-clean",
            check=False,
            environment=environment,
        )
        if outcome.returncode != 0 or outcome.stdout or outcome.stderr:
            raise CertificationError("source")
    commit = _closed_tool_line(
        run_bounded_command(
            ("/usr/bin/git", "rev-parse", "--verify", "HEAD"),
            timeout_seconds=10,
            phase="source-commit",
            environment=environment,
        ),
        "source",
    )
    tree = _closed_tool_line(
        run_bounded_command(
            ("/usr/bin/git", "rev-parse", "--verify", "HEAD^{tree}"),
            timeout_seconds=10,
            phase="source-tree",
            environment=environment,
        ),
        "source",
    )
    if GIT_OBJECT_RE.fullmatch(commit) is None or GIT_OBJECT_RE.fullmatch(tree) is None:
        raise CertificationError("source")
    return SourceIdentity(commit, tree)


def _default_recheck_source(expected: SourceIdentity) -> None:
    if not isinstance(expected, SourceIdentity):
        raise CertificationError("internal")
    if _read_clean_source_identity() != expected:
        raise CertificationError("source")


def _default_preflight() -> tuple[SourceIdentity, PlatformIdentity]:
    if sys.platform != "darwin" or platform.machine() != "arm64":
        raise CertificationError("platform")
    for executable in (
        "/usr/bin/git",
        "/usr/bin/codesign",
        "/usr/bin/xcrun",
        "/usr/bin/sw_vers",
        "/usr/sbin/sysctl",
    ):
        if not Path(executable).is_file() or not os.access(executable, os.X_OK):
            raise CertificationError("platform")
    environment = _production_environment()
    hvf = _closed_tool_line(
        run_bounded_command(
            ("/usr/sbin/sysctl", "-n", "kern.hv_support"),
            timeout_seconds=10,
            phase="platform-hvf",
            environment=environment,
        ),
        "platform",
    )
    if hvf != "1":
        raise CertificationError("platform")
    macos = _closed_tool_line(
        run_bounded_command(
            ("/usr/bin/sw_vers", "-productVersion"),
            timeout_seconds=10,
            phase="platform-macos",
            environment=environment,
        ),
        "platform",
    )
    sdk = _closed_tool_line(
        run_bounded_command(
            ("/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-version"),
            timeout_seconds=10,
            phase="platform-sdk",
            environment=environment,
        ),
        "platform",
    )
    if PLATFORM_VERSION_RE.fullmatch(macos) is None or PLATFORM_VERSION_RE.fullmatch(sdk) is None:
        raise CertificationError("platform")

    return _read_clean_source_identity(), PlatformIdentity(macos, sdk)


def _default_prepare_artifacts(config: CertificationConfig) -> PreparedArtifacts:
    _recheck_config_inputs(config)
    environment = _production_environment()
    kernel = _one_path(
        run_bounded_command(
            (os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-kernel.sh"),),
            timeout_seconds=config.timeouts.artifact_seconds,
            phase="artifact-kernel",
            environment=environment,
        ),
        "kernel",
    )
    rootfs = _one_path(
        run_bounded_command(
            (
                os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),
                "--format",
                "ext4",
                "--ext4-size",
                "512M",
                "--direct-boot-init",
            ),
            timeout_seconds=config.timeouts.artifact_seconds,
            phase="artifact-rootfs",
            environment=environment,
        ),
        "rootfs",
    )
    if rootfs.name != "ubuntu-24.04-512M-direct-boot-v110.ext4":
        raise CertificationError("artifact")
    kernel_identity = _verify_regular_artifact(kernel, "kernel")
    rootfs_identity = _verify_regular_artifact(rootfs, "rootfs")
    sidecar = rootfs.with_name(rootfs.name + ".bangbang.json")
    _verify_regular_artifact(sidecar, "rootfs-sidecar")
    return PreparedArtifacts(kernel, rootfs, kernel_identity, rootfs_identity)


def _recheck_identity(
    path: Path,
    expected: FileIdentity,
    *,
    category: str,
    maximum: int,
    executable: bool = False,
    digest: bool = False,
) -> FileIdentity:
    fd, current = _open_regular(
        path,
        category=category,
        maximum=maximum,
        private=category == "profile",
        executable=executable,
        digest=digest,
    )
    os.close(fd)
    if (
        current.device != expected.device
        or current.inode != expected.inode
        or current.size != expected.size
        or current.mtime_ns != expected.mtime_ns
        or current.ctime_ns != expected.ctime_ns
        or (digest and current.sha256 != expected.sha256)
    ):
        raise CertificationError(category)
    return current


def _recheck_config_inputs(config: CertificationConfig) -> None:
    _recheck_identity(
        config.provisioning_profile,
        config.provisioning_profile_identity,
        category="profile",
        maximum=MAX_PROFILE_BYTES,
    )
    current = _recheck_identity(
        config.fixture.executable,
        config.fixture.identity,
        category="fixture",
        maximum=MAX_FIXTURE_BYTES,
        executable=True,
        digest=True,
    )
    if current.sha256 != config.fixture.expected_sha256:
        raise CertificationError("fixture")


def _verify_bundle_layout(bundle: Path) -> None:
    if not bundle.is_absolute() or bundle.name != OUTER_BUNDLE_NAME:
        raise CertificationError("bundle")
    expected = (
        bundle,
        bundle / "Contents",
        bundle / "Contents/Helpers" / WORKER_BUNDLE_NAME,
    )
    files = (
        bundle / "Contents/MacOS" / LAUNCHER_EXECUTABLE_NAME,
        bundle
        / "Contents/Helpers"
        / WORKER_BUNDLE_NAME
        / "Contents/MacOS"
        / WORKER_EXECUTABLE_NAME,
    )
    try:
        for path in expected:
            metadata = os.lstat(path)
            if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
                raise CertificationError("bundle")
        for path in files:
            metadata = os.lstat(path)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or stat.S_ISLNK(metadata.st_mode)
                or metadata.st_size <= 0
                or not metadata.st_mode & stat.S_IXUSR
            ):
                raise CertificationError("bundle")
    except CertificationError:
        raise
    except OSError as error:
        raise CertificationError("bundle") from error


def _default_build_bundles(
    config: CertificationConfig, session: PrivateSession
) -> ProductionBundles:
    _recheck_config_inputs(config)
    session.verify()
    networkless = session.path / "networkless" / OUTER_BUNDLE_NAME
    vmnet = session.path / "vmnet" / OUTER_BUNDLE_NAME
    try:
        networkless.parent.mkdir(mode=PRIVATE_DIRECTORY_MODE)
        vmnet.parent.mkdir(mode=PRIVATE_DIRECTORY_MODE)
    except OSError as error:
        raise CertificationError("bundle") from error
    environment = _production_environment(temporary=session.path)
    run_bounded_command(
        (
            os.fspath(REPOSITORY_ROOT / "scripts/build-production-bundle.sh"),
            "--output",
            os.fspath(networkless),
            "--worker-profile",
            "networkless",
        ),
        timeout_seconds=config.timeouts.build_seconds,
        phase="bundle-networkless",
        environment=environment,
    )
    _recheck_config_inputs(config)
    run_bounded_command(
        (
            os.fspath(REPOSITORY_ROOT / "scripts/build-production-bundle.sh"),
            "--output",
            os.fspath(vmnet),
            "--signing-identity",
            config.signing_identity,
            "--worker-profile",
            "vmnet",
            "--provisioning-profile",
            os.fspath(config.provisioning_profile),
        ),
        timeout_seconds=config.timeouts.build_seconds,
        phase="bundle-vmnet",
        environment=environment,
    )
    _verify_bundle_layout(networkless)
    _verify_bundle_layout(vmnet)
    return ProductionBundles(networkless, vmnet)


def _bundle_paths(bundle: Path) -> tuple[Path, Path, Path]:
    launcher = bundle / "Contents/MacOS" / LAUNCHER_EXECUTABLE_NAME
    worker_bundle = bundle / "Contents/Helpers" / WORKER_BUNDLE_NAME
    worker = worker_bundle / "Contents/MacOS" / WORKER_EXECUTABLE_NAME
    return launcher, worker_bundle, worker


def _entitlements(path: Path) -> dict[str, object]:
    outcome = run_bounded_command(
        (
            "/usr/bin/codesign",
            "--display",
            "--entitlements",
            "-",
            "--xml",
            os.fspath(path),
        ),
        timeout_seconds=30,
        phase="bundle-entitlements",
        environment=_production_environment(),
    )
    if not outcome.stdout:
        return {}
    try:
        value = plistlib.loads(outcome.stdout)
    except (plistlib.InvalidFileException, ValueError) as error:
        raise CertificationError("bundle") from error
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        raise CertificationError("bundle")
    return value


def _verify_code(path: Path) -> None:
    run_bounded_command(
        (
            "/usr/bin/codesign",
            "--verify",
            "--strict",
            "--verbose=2",
            os.fspath(path),
        ),
        timeout_seconds=30,
        phase="bundle-code",
        environment=_production_environment(),
    )


def _default_inspect_bundles(bundles: ProductionBundles) -> EntitlementAssertions:
    for bundle in (bundles.networkless, bundles.vmnet):
        _verify_bundle_layout(bundle)
        launcher, worker_bundle, worker = _bundle_paths(bundle)
        for path in (launcher, worker_bundle, worker, bundle):
            _verify_code(path)
    networkless_launcher, networkless_worker_bundle, _networkless_worker = _bundle_paths(
        bundles.networkless
    )
    vmnet_launcher, vmnet_worker_bundle, _vmnet_worker = _bundle_paths(bundles.vmnet)
    networkless_outer = _entitlements(networkless_launcher)
    vmnet_outer = _entitlements(vmnet_launcher)
    networkless_worker = _entitlements(networkless_worker_bundle)
    vmnet_worker = _entitlements(vmnet_worker_bundle)
    if networkless_outer or vmnet_outer:
        raise CertificationError("bundle")
    if set(networkless_worker) != {
        APP_SANDBOX_ENTITLEMENT,
        HYPERVISOR_ENTITLEMENT,
    } or any(networkless_worker[key] is not True for key in networkless_worker):
        raise CertificationError("bundle")
    if (
        set(vmnet_worker) != WORKER_ENTITLEMENT_KEYS
        or vmnet_worker.get(APP_SANDBOX_ENTITLEMENT) is not True
        or vmnet_worker.get(HYPERVISOR_ENTITLEMENT) is not True
        or vmnet_worker.get(VMNET_ENTITLEMENT) is not True
        or not isinstance(vmnet_worker.get(APPLICATION_IDENTIFIER_ENTITLEMENT), str)
        or not vmnet_worker.get(APPLICATION_IDENTIFIER_ENTITLEMENT)
        or not isinstance(vmnet_worker.get(TEAM_IDENTIFIER_ENTITLEMENT), str)
        or not vmnet_worker.get(TEAM_IDENTIFIER_ENTITLEMENT)
    ):
        raise CertificationError("bundle")
    return EntitlementAssertions(True, True, True)


@dataclass(frozen=True)
class CaseFiles:
    root: Path
    manifest: Path
    api_directory: Path
    api_socket: Path
    serial: Path
    serial_identity: FileIdentity
    control: Optional[Path]
    control_identity: Optional[FileIdentity]


def _create_private_directory(path: Path) -> None:
    try:
        path.mkdir(mode=PRIVATE_DIRECTORY_MODE)
        metadata = os.lstat(path)
    except OSError as error:
        raise CertificationError("session") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
    ):
        raise CertificationError("session")


def _create_case_files(
    session: PrivateSession,
    artifacts: PreparedArtifacts,
    case: str,
    index: int,
    *,
    mode: Optional[str] = None,
    endpoint: Optional[FixtureEndpoint] = None,
    nonce: bytes = b"",
    attempt: int = 0,
) -> CaseFiles:
    if (
        case not in CASE_NAMES
        or not 0 <= index < len(CASE_NAMES)
        or not 0 <= attempt <= 99
    ):
        raise CertificationError("internal")
    session.verify()
    _recheck_artifact(artifacts.kernel, artifacts.kernel_identity)
    _recheck_artifact(artifacts.rootfs, artifacts.rootfs_identity)
    root = session.path / f"case-{index:02d}-{attempt:02d}-{case}"
    api_directory = root / "api"
    _create_private_directory(root)
    _create_private_directory(api_directory)
    serial = root / "serial.out"
    serial_identity = _write_private_file(serial, b"")
    control: Optional[Path] = None
    control_identity: Optional[FileIdentity] = None
    if mode is not None or endpoint is not None or nonce:
        if mode is None or endpoint is None:
            raise CertificationError("control")
        control = root / "control.bin"
        control_identity = _write_private_file(
            control, encode_guest_control(mode, endpoint.ipv4, endpoint.port, nonce)
        )
    grants: list[dict[str, object]] = [
        {
            "access": "read-only",
            "id": KERNEL_GRANT_ID,
            "role": "kernel-image",
            "source": _path_text(artifacts.kernel, "artifact"),
        },
        {
            "access": "read-only",
            "id": ROOTFS_GRANT_ID,
            "role": "drive-backing",
            "source": _path_text(artifacts.rootfs, "artifact"),
        },
        {
            "access": "write-only",
            "id": SERIAL_GRANT_ID,
            "role": "serial-sink",
            "source": _path_text(serial, "session"),
        },
        {
            "access": "create-children",
            "id": API_DIRECTORY_GRANT_ID,
            "role": "api-socket-directory",
            "source": _path_text(api_directory, "session"),
        },
    ]
    if control is not None:
        grants.insert(
            2,
            {
                "access": "read-only",
                "id": CONTROL_GRANT_ID,
                "role": "drive-backing",
                "source": _path_text(control, "session"),
            },
        )
    manifest = root / "grants.json"
    _write_private_file(manifest, canonical_json({"grants": grants, "version": 1}))
    api_socket = api_directory / API_SOCKET_CHILD
    if len(os.fsencode(api_socket)) >= 104:
        raise CertificationError("socket")
    return CaseFiles(
        root,
        manifest,
        api_directory,
        api_socket,
        serial,
        serial_identity,
        control,
        control_identity,
    )


def _read_identity_bounded(
    path: Path,
    identity: FileIdentity,
    *,
    maximum: int,
    category: str,
) -> bytes:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    fd = -1
    data = bytearray()
    try:
        fd = os.open(path, flags)
        before = os.fstat(fd)
        if (
            before.st_dev != identity.device
            or before.st_ino != identity.inode
            or not stat.S_ISREG(before.st_mode)
        ):
            raise CertificationError(category)
        while True:
            chunk = os.read(fd, min(4096, maximum + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
            if len(data) > maximum:
                raise CertificationError(category)
        after = os.fstat(fd)
        visible = os.lstat(path)
    except CertificationError:
        raise
    except OSError as error:
        raise CertificationError(category) from error
    finally:
        if fd >= 0:
            os.close(fd)
    if (
        after.st_dev != before.st_dev
        or after.st_ino != before.st_ino
        or visible.st_dev != before.st_dev
        or visible.st_ino != before.st_ino
    ):
        raise CertificationError(category)
    return bytes(data)


def _wait_serial(
    files: CaseFiles,
    marker: bytes,
    timeout_seconds: float,
    *,
    require_begin: bool = False,
) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        data = _read_identity_bounded(
            files.serial,
            files.serial_identity,
            maximum=MAX_SERIAL_BYTES,
            category="guest",
        )
        lines = data.splitlines(keepends=True)
        if any(line.startswith(GUEST_FAILURE_PREFIX) for line in lines):
            raise CertificationError("guest")
        if marker in lines and (not require_begin or GUEST_BEGIN_MARKER in lines):
            return
        if time.monotonic() >= deadline:
            raise CertificationError("guest-timeout")
        time.sleep(POLL_SECONDS)


def _http_request_bytes(
    method: str, path: str, body: Optional[Mapping[str, object]]
) -> bytes:
    if method not in ("GET", "PUT", "PATCH", "DELETE"):
        raise CertificationError("http")
    if (
        not path.startswith("/")
        or len(path) > 256
        or any(ord(character) < 0x21 or ord(character) > 0x7E for character in path)
    ):
        raise CertificationError("http")
    body_bytes = (
        b""
        if body is None
        else json.dumps(
            body,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=True,
            allow_nan=False,
        ).encode("ascii")
    )
    headers = [
        f"{method} {path} HTTP/1.1",
        "Host: localhost",
        "Connection: close",
    ]
    if body is not None:
        headers.extend(
            ("Content-Type: application/json", f"Content-Length: {len(body_bytes)}")
        )
    request = ("\r\n".join(headers) + "\r\n\r\n").encode("ascii") + body_bytes
    if len(request) > MAX_HTTP_REQUEST_BYTES:
        raise CertificationError("http")
    return request


def _remaining(deadline: float, category: str) -> float:
    value = deadline - time.monotonic()
    if value <= 0:
        raise CertificationError(category)
    return value


def _parse_http_response(data: bytes) -> HttpResponse:
    if len(data) > MAX_HTTP_RESPONSE_BYTES or b"\x00" in data:
        raise CertificationError("http")
    head, separator, body = data.partition(b"\r\n\r\n")
    if not separator:
        raise CertificationError("http")
    lines = head.split(b"\r\n")
    if not lines or len(lines) > MAX_HTTP_HEADERS + 1:
        raise CertificationError("http")
    try:
        status_line = lines[0].decode("ascii")
    except UnicodeDecodeError as error:
        raise CertificationError("http") from error
    match = re.fullmatch(r"HTTP/1\.1 ([1-5][0-9]{2}) ([\x20-\x7e]+)", status_line)
    if match is None:
        raise CertificationError("http")
    status = int(match.group(1))
    headers: dict[bytes, bytes] = {}
    for line in lines[1:]:
        name, split, value = line.partition(b":")
        lowered = name.lower()
        stripped = value.strip()
        if (
            not split
            or re.fullmatch(rb"[!#$%&'*+.^_`|~0-9A-Za-z-]+", name) is None
            or lowered in headers
            or any(byte < 0x20 or byte > 0x7E for byte in stripped)
        ):
            raise CertificationError("http")
        headers[lowered] = stripped
    if b"transfer-encoding" in headers:
        raise CertificationError("http")
    try:
        raw_length = headers[b"content-length"]
        if re.fullmatch(rb"0|[1-9][0-9]*", raw_length) is None:
            raise ValueError
        content_length = int(raw_length)
    except (KeyError, ValueError) as error:
        raise CertificationError("http") from error
    if content_length != len(body):
        raise CertificationError("http")
    return HttpResponse(status, body)


def http_exchange(
    socket_path: Path,
    method: str,
    path: str,
    body: Optional[Mapping[str, object]],
    timeout_seconds: float,
    *,
    socket_identity: Optional[FileIdentity] = None,
    expected_peer_pid: Optional[int] = None,
) -> HttpResponse:
    if (socket_identity is None) != (expected_peer_pid is None):
        raise CertificationError("internal")
    request = _http_request_bytes(method, path, body)
    deadline = time.monotonic() + timeout_seconds
    response = bytearray()
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        client.settimeout(_remaining(deadline, "http-timeout"))
        client.connect(os.fspath(socket_path))
        if socket_identity is not None and expected_peer_pid is not None:
            _verify_connected_api_socket(
                client,
                socket_path,
                socket_identity,
                expected_peer_pid,
            )
        client.settimeout(_remaining(deadline, "http-timeout"))
        client.sendall(request)
        client.shutdown(socket.SHUT_WR)
        while True:
            client.settimeout(_remaining(deadline, "http-timeout"))
            chunk = client.recv(4096)
            if not chunk:
                break
            response.extend(chunk)
            if len(response) > MAX_HTTP_RESPONSE_BYTES:
                raise CertificationError("http")
    except CertificationError:
        raise
    except (OSError, socket.timeout) as error:
        raise CertificationError("http") from error
    finally:
        client.close()
    return _parse_http_response(bytes(response))


def http_send_without_response(
    socket_path: Path,
    method: str,
    path: str,
    body: Mapping[str, object],
    timeout_seconds: float,
    *,
    socket_identity: Optional[FileIdentity] = None,
    expected_peer_pid: Optional[int] = None,
) -> None:
    if (socket_identity is None) != (expected_peer_pid is None):
        raise CertificationError("internal")
    request = _http_request_bytes(method, path, body)
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        client.settimeout(timeout_seconds)
        client.connect(os.fspath(socket_path))
        if socket_identity is not None and expected_peer_pid is not None:
            _verify_connected_api_socket(
                client,
                socket_path,
                socket_identity,
                expected_peer_pid,
            )
        client.sendall(request)
        client.shutdown(socket.SHUT_WR)
    except (OSError, socket.timeout) as error:
        raise CertificationError("http") from error
    finally:
        client.close()


def _require_no_content(response: HttpResponse) -> None:
    if response.status != 204 or response.body:
        raise CertificationError("api")


def _decode_api_fault(response: HttpResponse) -> str:
    if response.status != 400 or len(response.body) > 4096:
        raise CertificationError("api")
    try:
        value = json.loads(
            response.body,
            object_pairs_hook=_duplicate_safe_object,
            parse_constant=_reject_json_constant,
        )
    except (CertificationError, RecursionError, UnicodeDecodeError, ValueError) as error:
        raise CertificationError("api") from error
    if (
        not isinstance(value, dict)
        or tuple(value) != ("fault_message",)
        or not isinstance(value["fault_message"], str)
        or not 1 <= len(value["fault_message"]) <= 1024
        or any(ord(character) < 0x20 for character in value["fault_message"])
    ):
        raise CertificationError("api")
    return value["fault_message"]


def _require_policy_denial(response: HttpResponse) -> None:
    if _decode_api_fault(response) != "system host networking is not authorized":
        raise CertificationError("api")


def _require_service_status(response: HttpResponse, expected: str) -> None:
    if expected not in ("VMNET_NOT_AUTHORIZED", "VMNET_SHARING_SERVICE_BUSY"):
        raise CertificationError("internal")
    message = _decode_api_fault(response)
    if message != (
        "failed to start microVM: hypervisor error: "
        f"failed to start vmnet packet I/O: {expected}"
    ):
        raise CertificationError("api")


def _api_socket_identity(path: Path) -> FileIdentity:
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise CertificationError("socket") from error
    if (
        not stat.S_ISSOCK(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_uid != os.getuid()
    ):
        raise CertificationError("socket")
    return FileIdentity(metadata.st_dev, metadata.st_ino, metadata.st_size)


def _verify_connected_api_socket(
    client: socket.socket,
    path: Path,
    identity: FileIdentity,
    expected_peer_pid: int,
) -> None:
    if sys.platform != "darwin" or expected_peer_pid <= 0:
        raise CertificationError("socket")
    try:
        peer_pid = client.getsockopt(DARWIN_SOL_LOCAL, DARWIN_LOCAL_PEERPID)
        current = _api_socket_identity(path)
    except (OSError, TypeError) as error:
        raise CertificationError("socket") from error
    if (
        not isinstance(peer_pid, int)
        or peer_pid != expected_peer_pid
        or current.device != identity.device
        or current.inode != identity.inode
    ):
        raise CertificationError("socket")


def _wait_api_socket(
    path: Path, process: "ProductionProcess", timeout_seconds: float
) -> FileIdentity:
    deadline = time.monotonic() + timeout_seconds
    while True:
        process.raise_if_failed()
        try:
            metadata = os.lstat(path)
        except FileNotFoundError:
            metadata = None
        except OSError as error:
            raise CertificationError("socket") from error
        if metadata is not None:
            return _api_socket_identity(path)
        if time.monotonic() >= deadline:
            raise CertificationError("socket-timeout")
        time.sleep(POLL_SECONDS)


def _wait_socket_absent(path: Path, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            os.lstat(path)
        except FileNotFoundError:
            return
        except OSError as error:
            raise CertificationError("cleanup") from error
        if time.monotonic() >= deadline:
            raise CertificationError("cleanup")
        time.sleep(POLL_SECONDS)


def _session_root() -> Path:
    home = Path(_production_environment()["HOME"])
    return (
        home
        / "Library/Containers"
        / WORKER_BUNDLE_IDENTIFIER
        / "Data/tmp/bangbang-sessions-v1"
    )


def _session_entries() -> tuple[tuple[str, int, int], ...]:
    root = _session_root()
    try:
        entries = list(os.scandir(root))
    except FileNotFoundError:
        return ()
    except OSError as error:
        raise CertificationError("process") from error
    result = []
    for entry in entries:
        if not entry.name.startswith("session-"):
            continue
        try:
            metadata = entry.stat(follow_symlinks=False)
        except OSError as error:
            raise CertificationError("process") from error
        result.append((entry.name, metadata.st_dev, metadata.st_ino))
    result.sort()
    return tuple(result)


def _wait_sessions_restored(
    baseline: tuple[tuple[str, int, int], ...], timeout_seconds: float
) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        if _session_entries() == baseline:
            return
        if time.monotonic() >= deadline:
            raise CertificationError("cleanup")
        time.sleep(POLL_SECONDS)


class ProductionProcess:
    def __init__(
        self,
        arguments: Sequence[str],
        files: CaseFiles,
        config: CertificationConfig,
    ) -> None:
        self.files = files
        self.config = config
        self.baseline_sessions = _session_entries()
        self._stdout = _BoundedCapture(MAX_PROCESS_CAPTURE_BYTES)
        self._stderr = _BoundedCapture(MAX_PROCESS_CAPTURE_BYTES)
        self._api_identity: Optional[FileIdentity] = None
        self._worker_pid: Optional[int] = None
        self._closed = False
        try:
            process = subprocess.Popen(
                tuple(arguments),
                cwd=REPOSITORY_ROOT,
                env=_production_environment(temporary=files.root),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                bufsize=0,
            )
        except OSError as error:
            raise CertificationError("process") from error
        self.process = process
        if process.stdout is None or process.stderr is None:  # pragma: no cover
            _terminate_process(process, config.timeouts.terminate_seconds)
            raise CertificationError("process")
        self._threads = (
            threading.Thread(
                target=_pump_capture,
                args=(process.stdout, self._stdout),
                name="bangbang-vmnet-process-stdout",
                daemon=True,
            ),
            threading.Thread(
                target=_pump_capture,
                args=(process.stderr, self._stderr),
                name="bangbang-vmnet-process-stderr",
                daemon=True,
            ),
        )
        for thread in self._threads:
            thread.start()

    def _captures(self) -> tuple[bytes, bytes]:
        stdout, stdout_overflow, stdout_error = self._stdout.result()
        stderr, stderr_overflow, stderr_error = self._stderr.result()
        if stdout_overflow or stderr_overflow or stdout_error or stderr_error:
            raise CertificationError("process-output")
        return stdout, stderr

    def raise_if_failed(self) -> None:
        self._captures()
        if self.process.poll() is not None:
            raise CertificationError("process")

    def wait_ready(self) -> None:
        deadline = time.monotonic() + self.config.timeouts.startup_seconds
        while True:
            stdout, _stderr = self._captures()
            if API_READY_MARKER in stdout:
                self._api_identity = _wait_api_socket(
                    self.files.api_socket,
                    self,
                    self.config.timeouts.request_seconds,
                )
                self._worker_pid = self.worker_pid()
                return
            if self.process.poll() is not None:
                raise CertificationError("process")
            if time.monotonic() >= deadline:
                raise CertificationError("process-timeout")
            time.sleep(POLL_SECONDS)

    def worker_pid(self) -> int:
        if self._worker_pid is not None:
            _require_process_row(self._worker_pid, self.process.pid)
            return self._worker_pid
        deadline = time.monotonic() + self.config.timeouts.startup_seconds
        while True:
            self.raise_if_failed()
            outcome = run_bounded_command(
                ("/usr/bin/pgrep", "-P", str(self.process.pid)),
                timeout_seconds=self.config.timeouts.request_seconds,
                phase="process-child",
                check=False,
                environment=_production_environment(),
            )
            if outcome.returncode == 0 and not outcome.stderr:
                try:
                    values = [int(line) for line in outcome.stdout.splitlines()]
                except ValueError as error:
                    raise CertificationError("process") from error
                if len(values) == 1 and values[0] > 0:
                    _require_process_row(values[0], self.process.pid)
                    self._worker_pid = values[0]
                    return self._worker_pid
                if len(values) > 1:
                    raise CertificationError("process")
            elif outcome.returncode not in (0, 1):
                raise CertificationError("process")
            if time.monotonic() >= deadline:
                raise CertificationError("process-timeout")
            time.sleep(POLL_SECONDS)

    def api_authority(self) -> tuple[FileIdentity, int]:
        self.raise_if_failed()
        if self._api_identity is None or self._worker_pid is None:
            raise CertificationError("socket")
        return self._api_identity, self._worker_pid

    def terminate(self) -> None:
        _terminate_process(self.process, self.config.timeouts.terminate_seconds)
        self._finish()

    def wait_after_external_signal(self) -> None:
        try:
            self.process.wait(timeout=self.config.timeouts.terminate_seconds)
        except subprocess.TimeoutExpired:
            _terminate_process(self.process, self.config.timeouts.terminate_seconds)
            raise CertificationError("process-timeout")
        if _process_group_exists(self.process):
            if not _wait_process_group_absent(
                self.process,
                time.monotonic() + self.config.timeouts.terminate_seconds,
            ):
                _terminate_process(self.process, self.config.timeouts.terminate_seconds)
                raise CertificationError("process-cleanup")
        self._finish()

    def _finish(self) -> None:
        if self._closed:
            return
        for thread in self._threads:
            thread.join(timeout=self.config.timeouts.terminate_seconds)
        reader_stuck = any(thread.is_alive() for thread in self._threads)
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError):
                    pass
        if reader_stuck:
            for thread in self._threads:
                thread.join(timeout=0.25)
        if any(thread.is_alive() for thread in self._threads):
            raise CertificationError("process-cleanup")
        self._captures()
        _wait_socket_absent(
            self.files.api_socket, self.config.timeouts.request_seconds
        )
        _wait_sessions_restored(
            self.baseline_sessions, self.config.timeouts.terminate_seconds
        )
        self._closed = True

    def close(self) -> None:
        if self._closed:
            return
        _terminate_process(self.process, self.config.timeouts.terminate_seconds)
        self._finish()

    def __enter__(self) -> "ProductionProcess":
        return self

    def __exit__(self, *_exception: object) -> None:
        self.close()


def _require_process_row(pid: int, expected_parent: int) -> None:
    outcome = run_bounded_command(
        (
            "/bin/ps",
            "-o",
            "pid=,ppid=,state=,comm=",
            "-p",
            str(pid),
        ),
        timeout_seconds=10,
        phase="process-row",
        check=False,
        environment=_production_environment(),
    )
    if outcome.returncode != 0 or outcome.stderr:
        raise CertificationError("process")
    try:
        rows = outcome.stdout.decode("utf-8").splitlines()
        fields = rows[0].strip().split(None, 3)
        observed_pid = int(fields[0])
        parent = int(fields[1])
        state = fields[2]
        command = fields[3]
    except (IndexError, UnicodeDecodeError, ValueError) as error:
        raise CertificationError("process") from error
    if (
        len(rows) != 1
        or observed_pid != pid
        or parent != expected_parent
        or state.startswith("Z")
        or Path(command).name != WORKER_EXECUTABLE_NAME
    ):
        raise CertificationError("process")


def _process_absent(pid: int) -> bool:
    outcome = run_bounded_command(
        ("/bin/ps", "-o", "pid=,ppid=,state=,comm=", "-p", str(pid)),
        timeout_seconds=10,
        phase="process-absence",
        check=False,
        environment=_production_environment(),
    )
    if outcome.returncode == 1 and not outcome.stdout and not outcome.stderr:
        return True
    if outcome.returncode != 0 or outcome.stderr:
        raise CertificationError("process")
    try:
        rows = outcome.stdout.decode("utf-8").splitlines()
        fields = rows[0].strip().split(None, 3)
        observed = int(fields[0])
        state = fields[2]
    except (IndexError, UnicodeDecodeError, ValueError) as error:
        raise CertificationError("process") from error
    if len(rows) != 1 or observed != pid:
        raise CertificationError("process")
    return state.startswith("Z")


def _wait_process_absent(pid: int, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        if _process_absent(pid):
            return
        if time.monotonic() >= deadline:
            raise CertificationError("process-cleanup")
        time.sleep(POLL_SECONDS)


def _policy_arguments(allowed: Sequence[str], maximum: Optional[int]) -> list[str]:
    if not allowed:
        if maximum is not None:
            raise CertificationError("internal")
        return []
    if (
        maximum is None
        or isinstance(maximum, bool)
        or not isinstance(maximum, int)
        or not 1 <= maximum <= 4
        or len(allowed) > 6
    ):
        raise CertificationError("internal")
    result: list[str] = []
    seen: set[str] = set()
    for value in allowed:
        if (
            value in seen
            or (
                value not in ("host", "shared")
                and (
                    not value.startswith("bridged:")
                    or BRIDGE_RE.fullmatch(value.removeprefix("bridged:")) is None
                )
            )
        ):
            raise CertificationError("internal")
        seen.add(value)
        result.extend(("--vmnet-allow", value))
    result.extend(("--vmnet-max-interfaces", str(maximum)))
    return result


def _launcher_arguments(
    bundle: Path,
    files: CaseFiles,
    instance: str,
    allowed: Sequence[str],
    maximum: Optional[int],
) -> tuple[str, ...]:
    if (
        not re.fullmatch(r"[A-Za-z0-9-]{1,64}", instance)
        or not bundle.is_absolute()
    ):
        raise CertificationError("internal")
    launcher, _worker_bundle, worker = _bundle_paths(bundle)
    arguments = [
        os.fspath(launcher),
        JAILER_OPTION,
        "--id",
        instance,
        "--exec-file",
        os.fspath(worker),
        "--uid",
        str(os.getuid()),
        "--gid",
        str(os.getgid()),
    ]
    arguments.extend(_policy_arguments(allowed, maximum))
    arguments.extend(
        (
            "--",
            GRANT_MANIFEST_OPTION,
            os.fspath(files.manifest),
            "--",
            "--enable-pci",
            "--api-sock",
            API_SOCKET_REF,
            "--id",
            instance,
        )
    )
    return tuple(arguments)


def _networkless_denial_arguments(bundle: Path) -> tuple[str, ...]:
    launcher, _worker_bundle, worker = _bundle_paths(bundle)
    return (
        os.fspath(launcher),
        JAILER_OPTION,
        "--id",
        "cert-networkless-denial",
        "--exec-file",
        os.fspath(worker),
        "--uid",
        str(os.getuid()),
        "--gid",
        str(os.getgid()),
        "--vmnet-allow",
        "shared",
        "--vmnet-max-interfaces",
        "1",
        "--",
        "--version",
    )


def _api_put(
    process: ProductionProcess, path: str, body: Mapping[str, object]
) -> HttpResponse:
    socket_identity, peer_pid = process.api_authority()
    return http_exchange(
        process.files.api_socket,
        "PUT",
        path,
        body,
        process.config.timeouts.request_seconds,
        socket_identity=socket_identity,
        expected_peer_pid=peer_pid,
    )


def _api_get(process: ProductionProcess, path: str) -> HttpResponse:
    socket_identity, peer_pid = process.api_authority()
    return http_exchange(
        process.files.api_socket,
        "GET",
        path,
        None,
        process.config.timeouts.request_seconds,
        socket_identity=socket_identity,
        expected_peer_pid=peer_pid,
    )


def _require_running(response: HttpResponse) -> None:
    if response.status != 200 or len(response.body) > 4096:
        raise CertificationError("api")
    try:
        value = json.loads(
            response.body,
            object_pairs_hook=_duplicate_safe_object,
            parse_constant=_reject_json_constant,
        )
    except (CertificationError, RecursionError, UnicodeDecodeError, ValueError) as error:
        raise CertificationError("api") from error
    if not isinstance(value, dict) or value.get("state") != "Running":
        raise CertificationError("api")


def _require_worker_help(outcome: CommandOutcome) -> None:
    if (
        outcome.returncode != 0
        or outcome.stderr
        or not outcome.stdout.startswith(b"bangbang ")
        or b"\n\nUsage:\n  bangbang [OPTIONS]\n" not in outcome.stdout
    ):
        raise CertificationError("bundle")


class SystemCertificationDriver:
    def __init__(
        self,
        config: CertificationConfig,
        session: PrivateSession,
        artifacts: PreparedArtifacts,
        bundles: ProductionBundles,
    ) -> None:
        self.config = config
        self.session = session
        self.artifacts = artifacts
        self.bundles = bundles
        self._attempts: dict[str, int] = {}
        self._active: list[ProductionProcess] = []
        self._initialize_production_home()

    def _initialize_production_home(self) -> None:
        launcher, _worker_bundle, _worker = _bundle_paths(self.bundles.networkless)
        outcome = run_bounded_command(
            (os.fspath(launcher), "--help"),
            timeout_seconds=self.config.timeouts.request_seconds,
            phase="bundle-initialize",
            environment=_production_environment(temporary=self.session.path),
        )
        _require_worker_help(outcome)
        container_tmp = _session_root().parent
        try:
            container_tmp.mkdir(mode=PRIVATE_DIRECTORY_MODE, parents=True, exist_ok=True)
            metadata = os.lstat(container_tmp)
        except OSError as error:
            raise CertificationError("session") from error
        if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            raise CertificationError("session")

    def _files(
        self,
        case: str,
        *,
        mode: Optional[str] = None,
        endpoint: Optional[FixtureEndpoint] = None,
        nonce: bytes = b"",
    ) -> CaseFiles:
        attempt = self._attempts.get(case, 0)
        self._attempts[case] = attempt + 1
        return _create_case_files(
            self.session,
            self.artifacts,
            case,
            CASE_NAMES.index(case),
            mode=mode,
            endpoint=endpoint,
            nonce=nonce,
            attempt=attempt,
        )

    def _spawn(
        self,
        case: str,
        *,
        bundle: Optional[Path] = None,
        allowed: Sequence[str] = (),
        maximum: Optional[int] = None,
        mode: Optional[str] = None,
        endpoint: Optional[FixtureEndpoint] = None,
        nonce: bytes = b"",
    ) -> ProductionProcess:
        files = self._files(case, mode=mode, endpoint=endpoint, nonce=nonce)
        selected = self.bundles.vmnet if bundle is None else bundle
        instance = f"cert-{CASE_NAMES.index(case):02d}-{self._attempts[case]:02d}"
        process = ProductionProcess(
            _launcher_arguments(selected, files, instance, allowed, maximum),
            files,
            self.config,
        )
        self._active.append(process)
        return process

    def _retire(self, process: ProductionProcess) -> None:
        try:
            self._active.remove(process)
        except ValueError as error:
            raise CertificationError("internal") from error

    def _configure(
        self,
        process: ProductionProcess,
        *,
        networks: Sequence[tuple[str, str]] = (),
        mmds_interfaces: Sequence[str] = (),
        guest_oracle: bool = False,
    ) -> None:
        process.wait_ready()
        for path, body in (
            ("/machine-config", {"mem_size_mib": 256, "vcpu_count": 1}),
            (
                "/boot-source",
                {
                    "boot_args": (
                        PRODUCTION_VMNET_BOOT_ARGS
                        if guest_oracle
                        else DIRECT_ROOTFS_BOOT_ARGS
                    ),
                    "kernel_image_path": KERNEL_GRANT_REF,
                },
            ),
            (
                "/drives/rootfs",
                {
                    "drive_id": "rootfs",
                    "is_read_only": True,
                    "is_root_device": True,
                    "path_on_host": ROOTFS_GRANT_REF,
                },
            ),
        ):
            _require_no_content(_api_put(process, path, body))
        if process.files.control is not None:
            _require_no_content(
                _api_put(
                    process,
                    "/drives/control",
                    {
                        "drive_id": "control",
                        "is_read_only": True,
                        "is_root_device": False,
                        "path_on_host": CONTROL_GRANT_REF,
                    },
                )
            )
        _require_no_content(
            _api_put(process, "/serial", {"serial_out_path": SERIAL_GRANT_REF})
        )
        for iface_id, host_dev_name in networks:
            _require_no_content(
                _api_put(
                    process,
                    f"/network-interfaces/{iface_id}",
                    {"host_dev_name": host_dev_name, "iface_id": iface_id},
                )
            )
        if mmds_interfaces:
            _require_no_content(
                _api_put(
                    process,
                    "/mmds/config",
                    {
                        "ipv4_address": "169.254.169.254",
                        "network_interfaces": list(mmds_interfaces),
                        "version": "V1",
                    },
                )
            )

    def _start(self, process: ProductionProcess) -> HttpResponse:
        return _api_put(process, "/actions", {"action_type": "InstanceStart"})

    def _finish_process(self, process: ProductionProcess) -> None:
        try:
            process.terminate()
        finally:
            self._retire(process)

    def _abort_process(self, process: ProductionProcess) -> None:
        try:
            process.close()
        finally:
            if process in self._active:
                self._retire(process)

    def _run_policy_denial(
        self,
        case: str,
        *,
        allowed: Sequence[str],
        maximum: Optional[int],
        networks: Sequence[tuple[str, str]],
    ) -> None:
        process = self._spawn(case, allowed=allowed, maximum=maximum)
        try:
            self._configure(process, networks=networks)
            _require_policy_denial(self._start(process))
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_mmds_only(self, case: str) -> None:
        process = self._spawn(case, bundle=self.bundles.networkless)
        try:
            self._configure(
                process,
                networks=(("eth0", "vmnet:shared"),),
                mmds_interfaces=("eth0",),
            )
            _require_no_content(self._start(process))
            _wait_serial(
                process.files,
                DIRECT_ROOTFS_BOOT_MARKER,
                self.config.timeouts.guest_seconds,
            )
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_connectivity(
        self,
        case: str,
        mode: str,
        endpoint: Optional[FixtureEndpoint],
        nonce: bytes,
    ) -> None:
        if endpoint is None:
            raise CertificationError("fixture-protocol")
        if mode == "shared":
            authority = "shared"
            host_dev_name = "vmnet:shared"
        elif mode == "host":
            authority = "host"
            host_dev_name = "vmnet:host"
        elif mode == "bridged":
            bridge = self.config.optional_cases.bridged_interface
            if bridge is None:
                raise CertificationError("optional-cases")
            authority = f"bridged:{bridge}"
            host_dev_name = f"vmnet:bridged:{bridge}"
        else:
            raise CertificationError("internal")
        process = self._spawn(
            case,
            allowed=(authority,),
            maximum=1,
            mode=mode,
            endpoint=endpoint,
            nonce=nonce,
        )
        try:
            self._configure(
                process,
                networks=(("eth0", host_dev_name),),
                guest_oracle=True,
            )
            _require_no_content(self._start(process))
            _wait_serial(
                process.files,
                GUEST_SUCCESS_MARKER,
                self.config.timeouts.guest_seconds,
                require_begin=True,
            )
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_service_case(self, case: str, expected: str) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            _require_service_status(self._start(process), expected)
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _start_live_shared(self, case: str) -> ProductionProcess:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            _require_no_content(self._start(process))
            _wait_serial(
                process.files,
                DIRECT_ROOTFS_BOOT_MARKER,
                self.config.timeouts.guest_seconds,
            )
            return process
        except BaseException:
            self._abort_process(process)
            raise

    def _run_normal_teardown(self, case: str) -> None:
        self._finish_process(self._start_live_shared(case))

    def _run_partial_start(self, case: str) -> None:
        process = self._start_live_shared(case)
        try:
            denied = _api_put(
                process,
                "/network-interfaces/eth1",
                {"host_dev_name": "vmnet:shared", "iface_id": "eth1"},
            )
            _require_policy_denial(denied)
            _require_running(_api_get(process, "/"))
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_pre_ready_cancellation(self, case: str) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            socket_identity, peer_pid = process.api_authority()
            http_send_without_response(
                process.files.api_socket,
                "PUT",
                "/actions",
                {"action_type": "InstanceStart"},
                self.config.timeouts.request_seconds,
                socket_identity=socket_identity,
                expected_peer_pid=peer_pid,
            )
            try:
                os.kill(process.process.pid, signal.SIGTERM)
            except OSError as error:
                raise CertificationError("process") from error
            process.wait_after_external_signal()
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_post_ready_cancellation(self, case: str) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            _require_no_content(self._start(process))
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_worker_death(self, case: str, number: int) -> None:
        process = self._start_live_shared(case)
        try:
            worker = process.worker_pid()
            try:
                os.kill(worker, number)
            except OSError as error:
                raise CertificationError("process") from error
            process.wait_after_external_signal()
            _wait_process_absent(worker, self.config.timeouts.terminate_seconds)
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_launcher_death(self, case: str) -> None:
        process = self._start_live_shared(case)
        try:
            worker = process.worker_pid()
            try:
                os.kill(process.process.pid, signal.SIGKILL)
            except OSError as error:
                raise CertificationError("process") from error
            process.wait_after_external_signal()
            _wait_process_absent(worker, self.config.timeouts.terminate_seconds)
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_clean_repeat(self, case: str) -> None:
        for _attempt in range(2):
            self._finish_process(self._start_live_shared(case))

    def _run_concurrent(self, case: str) -> None:
        first = self._start_live_shared(case)
        second: Optional[ProductionProcess] = None
        try:
            second = self._spawn(case)
            self._configure(second)
            _require_no_content(self._start(second))
            _require_policy_denial(
                _api_put(
                    second,
                    "/network-interfaces/eth0",
                    {"host_dev_name": "vmnet:shared", "iface_id": "eth0"},
                )
            )
            _require_running(_api_get(first, "/"))
            self._finish_process(second)
            second = None
            _require_running(_api_get(first, "/"))
            self._finish_process(first)
        except BaseException:
            if second is not None:
                self._abort_process(second)
            self._abort_process(first)
            raise

    def execute(
        self,
        case: str,
        *,
        endpoint: Optional[FixtureEndpoint],
        nonce: bytes,
    ) -> None:
        if case not in CASE_NAMES or len(nonce) != 32 or not any(nonce):
            raise CertificationError("internal")
        if case == "entitlement-split":
            return
        if case == "networkless-denial":
            baseline = _session_entries()
            outcome = run_bounded_command(
                _networkless_denial_arguments(self.bundles.networkless),
                timeout_seconds=self.config.timeouts.startup_seconds,
                phase="networkless-denial",
                check=False,
                environment=_production_environment(temporary=self.session.path),
            )
            if (
                outcome.returncode != 1
                or outcome.stdout
                or outcome.stderr
                != b"bangbang launcher: invalid production launch policy\n"
            ):
                raise CertificationError("case")
            _wait_sessions_restored(baseline, self.config.timeouts.terminate_seconds)
            return
        if case == "missing-policy-denial":
            self._run_policy_denial(
                case,
                allowed=(),
                maximum=None,
                networks=(("eth0", "vmnet:shared"),),
            )
            return
        if case == "mismatched-policy-denial":
            self._run_policy_denial(
                case,
                allowed=("host",),
                maximum=1,
                networks=(("eth0", "vmnet:shared"),),
            )
            return
        if case == "bridge-allowlist-denial":
            self._run_policy_denial(
                case,
                allowed=("bridged:certbridge",),
                maximum=1,
                networks=(("eth0", "vmnet:bridged:certother"),),
            )
            return
        if case == "active-interface-count-exhaustion":
            self._run_policy_denial(
                case,
                allowed=("shared",),
                maximum=1,
                networks=(
                    ("eth0", "vmnet:shared"),
                    ("eth1", "vmnet:shared"),
                ),
            )
            return
        if case == "mmds-only-no-consumption":
            self._run_mmds_only(case)
            return
        if case == "shared-connectivity":
            self._run_connectivity(case, "shared", endpoint, nonce)
            return
        if case == "host-connectivity":
            self._run_connectivity(case, "host", endpoint, nonce)
            return
        if case == "bridged-connectivity":
            self._run_connectivity(case, "bridged", endpoint, nonce)
            return
        if case == "not-authorized":
            self._run_service_case(case, "VMNET_NOT_AUTHORIZED")
            return
        if case == "sharing-service-busy":
            self._run_service_case(case, "VMNET_SHARING_SERVICE_BUSY")
            return
        if case == "normal-teardown":
            self._run_normal_teardown(case)
            return
        if case == "partial-start-cleanup":
            self._run_partial_start(case)
            return
        if case == "pre-ready-cancellation":
            self._run_pre_ready_cancellation(case)
            return
        if case == "post-ready-cancellation":
            self._run_post_ready_cancellation(case)
            return
        if case == "worker-first-death":
            self._run_worker_death(case, signal.SIGTERM)
            return
        if case == "launcher-first-death":
            self._run_launcher_death(case)
            return
        if case == "worker-sigkill-reclamation":
            self._run_worker_death(case, signal.SIGKILL)
            return
        if case == "clean-repeat":
            self._run_clean_repeat(case)
            return
        if case == "concurrent-noninterchangeability":
            self._run_concurrent(case)
            return
        raise CertificationError("internal")

    def close(self) -> None:
        cleanup_error: Optional[CertificationError] = None
        for process in reversed(self._active):
            try:
                process.close()
            except CertificationError as error:
                cleanup_error = cleanup_error or error
        self._active.clear()
        if cleanup_error is not None:
            raise cleanup_error


def _optional_case_enabled(config: CertificationConfig, case: str) -> bool:
    if case == "host-connectivity":
        return config.optional_cases.host_connectivity
    if case == "bridged-connectivity":
        return config.optional_cases.bridged_interface is not None
    if case == "not-authorized":
        return config.optional_cases.not_authorized
    if case == "sharing-service-busy":
        return config.optional_cases.sharing_service_busy
    if case in ENVIRONMENT_GATED_CASES:
        raise CertificationError("internal")
    return True


def _validate_output_target(path: Path) -> None:
    if (
        not path.is_absolute()
        or path.name in ("", ".", "..")
        or len(os.fsencode(path)) > MAX_PATH_BYTES
    ):
        raise CertificationError("output")
    try:
        parent = os.lstat(path.parent)
    except OSError as error:
        raise CertificationError("output") from error
    if not stat.S_ISDIR(parent.st_mode) or stat.S_ISLNK(parent.st_mode):
        raise CertificationError("output")
    try:
        os.lstat(path)
    except FileNotFoundError:
        return
    except OSError as error:
        raise CertificationError("output") from error
    raise CertificationError("output")


def _next_nonce(factory: Callable[[int], bytes]) -> bytes:
    try:
        nonce = factory(32)
    except BaseException as error:
        raise CertificationError("nonce") from error
    if not isinstance(nonce, bytes) or len(nonce) != 32 or not any(nonce):
        raise CertificationError("nonce")
    return nonce


def _result_document(
    source: SourceIdentity,
    host: PlatformIdentity,
    entitlements: EntitlementAssertions,
    outcomes: Sequence[str],
    cleanup: str,
) -> dict[str, object]:
    if len(outcomes) != len(CASE_NAMES):
        raise CertificationError("internal")
    verdict = (
        "failed"
        if cleanup == "incomplete" or "failed" in outcomes
        else "blocked"
        if "blocked" in outcomes
        else "passed"
    )
    document: dict[str, object] = {
        "cases": [
            {"name": name, "outcome": outcome}
            for name, outcome in zip(CASE_NAMES, outcomes)
        ],
        "cleanup": cleanup,
        "entitlements": {
            "outer_empty": entitlements.outer_empty,
            "worker_app_sandbox_hvf": entitlements.worker_app_sandbox_hvf,
            "worker_vmnet": entitlements.worker_vmnet,
        },
        "platform": {
            "architecture": host.architecture,
            "hvf": host.hvf,
            "macos": host.macos,
            "sdk": host.sdk,
        },
        "schema_version": SCHEMA_VERSION,
        "source": {"commit": source.commit, "tree": source.tree},
        "verdict": verdict,
    }
    return validate_result_document(document)


@contextmanager
def _interruption_boundary() -> Iterator[None]:
    previous: dict[int, Any] = {}

    def handle(_number: int, _frame: object) -> None:
        raise CertificationError("interrupted")

    if threading.current_thread() is threading.main_thread():
        for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            previous[number] = signal.getsignal(number)
            signal.signal(number, handle)
    try:
        yield
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)


def default_dependencies() -> CertificationDependencies:
    return CertificationDependencies(
        preflight=_default_preflight,
        prepare_artifacts=_default_prepare_artifacts,
        build_bundles=_default_build_bundles,
        inspect_bundles=_default_inspect_bundles,
        driver_factory=SystemCertificationDriver,
        session_parent=PRODUCTION_SESSION_PARENT,
        recheck_source=_default_recheck_source,
    )


def run_certification(
    config_path: Path,
    result_path: Path,
    *,
    dependencies: Optional[CertificationDependencies] = None,
) -> dict[str, Any]:
    """Run the fixed production matrix; dependency injection is import-only."""

    _validate_output_target(result_path)
    config = read_config(config_path)
    runtime = dependencies if dependencies is not None else default_dependencies()
    source, host = runtime.preflight()
    if not isinstance(source, SourceIdentity) or not isinstance(host, PlatformIdentity):
        raise CertificationError("internal")

    outcomes = ["blocked"] * len(CASE_NAMES)
    session: Optional[PrivateSession] = None
    driver: Optional[CertificationCaseDriver] = None
    entitlements: Optional[EntitlementAssertions] = None
    setup_complete = False
    active_index: Optional[int] = None
    first_error: Optional[CertificationError] = None
    cleanup_error: Optional[CertificationError] = None

    with _interruption_boundary():
        try:
            session = PrivateSession.create(runtime.session_parent)
            artifacts = runtime.prepare_artifacts(config)
            if not isinstance(artifacts, PreparedArtifacts):
                raise CertificationError("internal")
            runtime.recheck_source(source)
            bundles = runtime.build_bundles(config, session)
            if not isinstance(bundles, ProductionBundles):
                raise CertificationError("internal")
            entitlements = runtime.inspect_bundles(bundles)
            if (
                not isinstance(entitlements, EntitlementAssertions)
                or not entitlements.outer_empty
                or not entitlements.worker_app_sandbox_hvf
                or not entitlements.worker_vmnet
            ):
                raise CertificationError("bundle")
            runtime.recheck_source(source)
            driver = runtime.driver_factory(config, session, artifacts, bundles)
            setup_complete = True
            for index, case in enumerate(CASE_NAMES):
                active_index = index
                if case in ENVIRONMENT_GATED_CASES and not _optional_case_enabled(
                    config, case
                ):
                    outcomes[index] = "environment-gated"
                    active_index = None
                    continue
                nonce = _next_nonce(runtime.nonce_factory)
                if case in FIXTURE_CASES:
                    bridge = (
                        config.optional_cases.bridged_interface
                        if case == "bridged-connectivity"
                        else None
                    )
                    with FixtureSession(
                        config.fixture,
                        case,
                        nonce,
                        config.timeouts.fixture_seconds,
                        bridge_interface=bridge,
                        session_parent=session.path,
                        terminate_seconds=config.timeouts.terminate_seconds,
                        clock=runtime.clock,
                        popen_factory=runtime.fixture_popen_factory,
                    ) as fixture:
                        endpoint = fixture.prepare()
                        if case in CONNECTIVITY_CASES and endpoint is None:
                            raise CertificationError("fixture-protocol")
                        if case not in CONNECTIVITY_CASES and endpoint is not None:
                            raise CertificationError("fixture-protocol")
                        driver.execute(
                            case,
                            endpoint=endpoint,
                            nonce=nonce,
                        )
                        fixture.wait_observed()
                        fixture.complete()
                else:
                    driver.execute(case, endpoint=None, nonce=nonce)
                runtime.recheck_source(source)
                outcomes[index] = "passed"
                active_index = None
        except CertificationError as error:
            first_error = error
            if setup_complete and active_index is not None:
                outcomes[active_index] = "failed"
        except KeyboardInterrupt as error:  # pragma: no cover - signal handler owns CLI
            first_error = CertificationError("interrupted")
            if setup_complete and active_index is not None:
                outcomes[active_index] = "failed"
        except BaseException as error:
            first_error = CertificationError("internal")
            first_error.__cause__ = error
            if setup_complete and active_index is not None:
                outcomes[active_index] = "failed"
        finally:
            if driver is not None:
                try:
                    driver.close()
                except CertificationError as error:
                    cleanup_error = error
                except BaseException as error:
                    cleanup_error = CertificationError("cleanup")
                    cleanup_error.__cause__ = error
            if session is not None:
                try:
                    session.cleanup()
                except CertificationError as error:
                    cleanup_error = cleanup_error or error
                except BaseException as error:
                    cleanup_error = cleanup_error or CertificationError("cleanup")
                    cleanup_error.__cause__ = error

    if setup_complete:
        try:
            runtime.recheck_source(source)
        except CertificationError as error:
            if first_error is None:
                first_error = error
                if "failed" not in outcomes:
                    outcomes[-1] = "failed"
        except BaseException as error:
            if first_error is None:
                first_error = CertificationError("internal")
                first_error.__cause__ = error
                if "failed" not in outcomes:
                    outcomes[-1] = "failed"

    if not setup_complete or entitlements is None:
        raise cleanup_error or first_error or CertificationError("internal")
    document = _result_document(
        source,
        host,
        entitlements,
        outcomes,
        "incomplete" if cleanup_error is not None else "complete",
    )
    publish_result(result_path, document)
    if cleanup_error is not None:
        raise cleanup_error
    if first_error is not None:
        raise first_error
    return document


def _parser() -> RedactedArgumentParser:
    parser = RedactedArgumentParser(
        description="Validate or run production-vmnet certification."
    )
    subparsers = parser.add_subparsers(dest="operation", required=True)
    config = subparsers.add_parser("validate-config")
    config.add_argument("--config", type=Path, required=True)
    result = subparsers.add_parser("validate-result")
    result.add_argument("--result", type=Path, required=True)
    run = subparsers.add_parser("run")
    run.add_argument("--config", type=Path, required=True)
    run.add_argument("--result", type=Path, required=True)
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
        elif args.operation == "run":
            run_certification(args.config, args.result)
            print("bangbang production vmnet run: passed")
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
            else "run"
            if failure.category != "invocation" and operation == "run"
            else "invocation"
        )
        if label == "run":
            print(
                f"bangbang production vmnet run: blocked category={failure.category}",
                file=sys.stderr,
            )
        else:
            print(
                f"bangbang production vmnet {label}: invalid category={failure.category}",
                file=sys.stderr,
            )
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
