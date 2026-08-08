#!/usr/bin/env python3
"""Collect and compare strict, threshold-free Bangbang specification reports."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Callable, Iterator, Mapping, Optional, Sequence

import guest_artifact_policy
import specification_workload


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
SCHEMA_VERSION = 1
MAX_DOCUMENT_BYTES = 1024 * 1024
MAX_CAPTURE_BYTES = 256 * 1024
MAX_HTTP_REQUEST_BYTES = 32 * 1024
MAX_HTTP_RESPONSE_BYTES = 256 * 1024
MAX_FIFO_BYTES = 1024 * 1024
POLL_SECONDS = 0.01
PRIVATE_DIRECTORY_MODE = 0o700
PRIVATE_FILE_MODE = 0o600
TRACE_MARKER = b"trace module="
API_SOCKET_READY = b"status: API server listening"
WORKLOAD_READY = b"BANGBANG_SPEC_INIT_READY release_byte=82"
WORKLOAD_TIMED = b"BANGBANG_SPEC_COMPUTE"
WORKLOAD_SUCCESS = b"BANGBANG_SPEC_WORKLOAD_OK"
WORKLOAD_FAILURE = b"BANGBANG_SPEC_WORKLOAD_FAIL"
WORKLOAD_BOOT_ARGS = (
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 root=/dev/vda ro rootwait "
    "init=/bangbang-specification-benchmark"
)
FIFO_SENTINEL_CHUNK = (
    b"BANGBANG_SPEC_METRICS_FIFO_SENTINEL_V1\n" * 128
)[:4096]
EXPECTED_WOULD_BLOCK_FAULT = "failed to flush metrics: WouldBlock"
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
GIT_OBJECT_RE = re.compile(r"[0-9a-f]{40}\Z")
HOST_LABEL_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}\Z")
LABEL_RE = re.compile(r"[a-z0-9][a-z0-9._/-]{0,63}\Z")
BOOT_TIMER_RE = re.compile(
    r"Guest-boot-time =\s+([0-9]+) us ([0-9]+) ms,\s+"
    r"([0-9]+) CPU us ([0-9]+) CPU ms"
)
U64_MAX = (1 << 64) - 1


MEASUREMENT_DEFINITIONS = (
    ("process_startup_wall_us", "bangbang-initial-metrics-v1", "microseconds"),
    ("process_startup_cpu_us", "bangbang-initial-metrics-v1", "microseconds"),
    ("whole_process_rss_kib", "ps-rss-kib-v1", "kibibytes"),
    ("guest_init_wall_us", "bangbang-boot-timer-v1", "microseconds"),
    ("guest_init_cpu_us", "bangbang-boot-timer-v1", "microseconds"),
    (
        "guest_compute_duration_ns",
        "guest-clock-monotonic-fixed-loop-v1",
        "nanoseconds",
    ),
    (
        "guest_storage_duration_ns",
        "guest-clock-monotonic-sequential-root-read-v1",
        "nanoseconds",
    ),
    (
        "metrics_fifo_filled_bytes",
        "nonblocking-sentinel-until-eagain-v1",
        "bytes",
    ),
    (
        "metrics_fifo_drained_bytes",
        "drain-after-failed-flush-v1",
        "bytes",
    ),
    (
        "metrics_missed_count",
        "failed-flush-replay-counter-v1",
        "count",
    ),
)
WORKLOAD_MEASUREMENT_NAMES = frozenset(
    {
        "guest_compute_duration_ns",
        "guest_init_cpu_us",
        "guest_init_wall_us",
        "guest_storage_duration_ns",
        "process_startup_cpu_us",
        "process_startup_wall_us",
        "whole_process_rss_kib",
    }
)
TELEMETRY_MEASUREMENT_NAMES = frozenset(
    {
        "metrics_fifo_drained_bytes",
        "metrics_fifo_filled_bytes",
        "metrics_missed_count",
    }
)


class BenchmarkError(RuntimeError):
    """A stable public benchmark failure."""

    def __init__(self, category: str, message: str) -> None:
        super().__init__(message)
        self.category = category


@dataclass(frozen=True)
class BenchmarkTimeouts:
    artifact_seconds: int
    build_seconds: int
    startup_seconds: int
    request_seconds: int
    guest_seconds: int
    terminate_seconds: int
    network_seconds: int

    def document(self) -> dict[str, int]:
        return {
            "artifact_seconds": self.artifact_seconds,
            "build_seconds": self.build_seconds,
            "guest_seconds": self.guest_seconds,
            "network_seconds": self.network_seconds,
            "request_seconds": self.request_seconds,
            "startup_seconds": self.startup_seconds,
            "terminate_seconds": self.terminate_seconds,
        }


@dataclass(frozen=True)
class BenchmarkConfig:
    host_label: str
    iterations: int
    warmups: int
    tracing: str
    timeouts: BenchmarkTimeouts


@dataclass(frozen=True)
class NetworkFixture:
    argv: tuple[str, ...]
    backend: str
    credential_mode: str
    method: str
    unit: str
    workload: str
    timeout_seconds: int
    document_sha256: str
    executable_sha256: str
    executable_device: int
    executable_inode: int

    def identity(self) -> dict[str, object]:
        return {
            "backend": self.backend,
            "document_sha256": self.document_sha256,
            "executable_sha256": self.executable_sha256,
            "method": self.method,
            "unit": self.unit,
            "workload": self.workload,
        }


@dataclass(frozen=True)
class PreparedArtifacts:
    kernel: Path
    kernel_sha256: str
    kernel_size_bytes: int
    rootfs: Path
    rootfs_sha256: str
    rootfs_size_bytes: int
    rootfs_recipe_sha256: str
    storage_checksum: int


@dataclass(frozen=True)
class SignedBuild:
    path: Path
    sha256: str
    size_bytes: int
    commit: str
    tree: str
    cargo_version: str
    rustc_version: str


@dataclass(frozen=True)
class CommandOutcome:
    returncode: int
    stdout: bytes
    stderr: bytes


@dataclass(frozen=True)
class HttpResponse:
    status: int
    body: bytes


@dataclass(frozen=True)
class CollectionDependencies:
    preflight: Callable[[BenchmarkConfig], None]
    prepare_artifacts: Callable[[BenchmarkConfig], PreparedArtifacts]
    build_signed_binary: Callable[[Path, BenchmarkConfig], SignedBuild]
    inspect_environment: Callable[
        [BenchmarkConfig, PreparedArtifacts, SignedBuild], dict[str, object]
    ]
    collect_workload: Callable[
        [Path, PreparedArtifacts, SignedBuild, BenchmarkConfig, int], dict[str, int]
    ]
    collect_telemetry: Callable[
        [Path, PreparedArtifacts, SignedBuild, BenchmarkConfig, int], dict[str, int]
    ]
    collect_network: Callable[[NetworkFixture, Path], int]


def _duplicate_safe_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise BenchmarkError("document", f"duplicate JSON key: {key}")
        result[key] = value
    return result


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True) + "\n"
    ).encode("ascii")


def _read_canonical_document(path: Path, label: str) -> tuple[dict[str, Any], bytes]:
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise BenchmarkError("document", f"{label} is unavailable") from error
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_DOCUMENT_BYTES:
        raise BenchmarkError("document", f"{label} must be a bounded regular file")
    try:
        data = path.read_bytes()
        if len(data) > MAX_DOCUMENT_BYTES:
            raise BenchmarkError("document", f"{label} exceeds its size bound")
        value = json.loads(data, object_pairs_hook=_duplicate_safe_object)
    except BenchmarkError:
        raise
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BenchmarkError("document", f"{label} is not valid JSON") from error
    if not isinstance(value, dict):
        raise BenchmarkError("document", f"{label} must be a JSON object")
    if canonical_json(value) != data:
        raise BenchmarkError("document", f"{label} must use canonical JSON bytes")
    return value, data


def _object(
    value: object,
    required: Sequence[str],
    label: str,
    *,
    optional: Sequence[str] = (),
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BenchmarkError("document", f"{label} must be an object")
    required_keys = set(required)
    allowed = required_keys | set(optional)
    actual = set(value)
    missing = sorted(required_keys - actual)
    unknown = sorted(actual - allowed)
    if missing or unknown:
        raise BenchmarkError(
            "document",
            f"{label} has missing keys {missing} and unknown keys {unknown}",
        )
    return value


def _array(value: object, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise BenchmarkError("document", f"{label} must be an array")
    return value


def _bounded_int(value: object, minimum: int, maximum: int, label: str) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise BenchmarkError(
            "document", f"{label} must be an integer in [{minimum}, {maximum}]"
        )
    return value


def _u64(value: object, label: str) -> int:
    return _bounded_int(value, 0, U64_MAX, label)


def _string(value: object, label: str, *, maximum: int = 128) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise BenchmarkError("document", f"{label} must be a nonempty bounded string")
    try:
        value.encode("ascii")
    except UnicodeEncodeError as error:
        raise BenchmarkError("document", f"{label} must be ASCII") from error
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in value):
        raise BenchmarkError("document", f"{label} must not contain control characters")
    return value


def _closed_label(value: object, label: str) -> str:
    text = _string(value, label, maximum=64)
    if LABEL_RE.fullmatch(text) is None:
        raise BenchmarkError("document", f"{label} has an unsafe label shape")
    return text


def _digest(value: object, label: str) -> str:
    text = _string(value, label, maximum=64)
    if SHA256_RE.fullmatch(text) is None:
        raise BenchmarkError("document", f"{label} must be a lowercase SHA-256")
    return text


def _git_object(value: object, label: str) -> str:
    text = _string(value, label, maximum=40)
    if GIT_OBJECT_RE.fullmatch(text) is None:
        raise BenchmarkError("document", f"{label} must be a lowercase Git object")
    return text


def parse_config_document(document: object) -> BenchmarkConfig:
    root = _object(
        document,
        ("host_label", "iterations", "schema_version", "timeouts", "tracing", "warmups"),
        "config",
    )
    if root["schema_version"] != SCHEMA_VERSION:
        raise BenchmarkError("document", "config schema_version must be 1")
    host_label = _string(root["host_label"], "config.host_label", maximum=64)
    if HOST_LABEL_RE.fullmatch(host_label) is None:
        raise BenchmarkError("document", "config.host_label has an unsafe label shape")
    iterations = _bounded_int(root["iterations"], 3, 31, "config.iterations")
    if iterations % 2 == 0:
        raise BenchmarkError("document", "config.iterations must be odd")
    warmups = _bounded_int(root["warmups"], 0, 10, "config.warmups")
    tracing = _string(root["tracing"], "config.tracing", maximum=16)
    if tracing != "disabled":
        raise BenchmarkError("document", "config.tracing must be exactly disabled")
    timeout_document = _object(
        root["timeouts"],
        (
            "artifact_seconds",
            "build_seconds",
            "guest_seconds",
            "network_seconds",
            "request_seconds",
            "startup_seconds",
            "terminate_seconds",
        ),
        "config.timeouts",
    )
    timeouts = BenchmarkTimeouts(
        artifact_seconds=_bounded_int(
            timeout_document["artifact_seconds"], 1, 3600, "artifact timeout"
        ),
        build_seconds=_bounded_int(
            timeout_document["build_seconds"], 1, 3600, "build timeout"
        ),
        startup_seconds=_bounded_int(
            timeout_document["startup_seconds"], 1, 300, "startup timeout"
        ),
        request_seconds=_bounded_int(
            timeout_document["request_seconds"], 1, 60, "request timeout"
        ),
        guest_seconds=_bounded_int(
            timeout_document["guest_seconds"], 1, 600, "guest timeout"
        ),
        terminate_seconds=_bounded_int(
            timeout_document["terminate_seconds"], 1, 60, "terminate timeout"
        ),
        network_seconds=_bounded_int(
            timeout_document["network_seconds"], 1, 600, "network timeout"
        ),
    )
    return BenchmarkConfig(host_label, iterations, warmups, tracing, timeouts)


def read_config(path: Path) -> BenchmarkConfig:
    document, _data = _read_canonical_document(path, "benchmark config")
    return parse_config_document(document)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise BenchmarkError("artifact", "failed to hash a required file") from error
    return digest.hexdigest()


def parse_network_fixture_document(
    document: object,
    canonical_bytes: bytes,
) -> NetworkFixture:
    root = _object(
        document,
        (
            "argv",
            "backend",
            "credential_mode",
            "method",
            "schema_version",
            "timeout_seconds",
            "unit",
            "workload",
        ),
        "network fixture",
    )
    if root["schema_version"] != SCHEMA_VERSION:
        raise BenchmarkError("document", "network fixture schema_version must be 1")
    if root["credential_mode"] != "none":
        raise BenchmarkError("document", "network fixture credential_mode must be none")
    argv_values = _array(root["argv"], "network fixture.argv")
    if not 1 <= len(argv_values) <= 16:
        raise BenchmarkError("document", "network fixture argv count is outside bounds")
    argv = tuple(
        _string(value, f"network fixture.argv[{index}]", maximum=256)
        for index, value in enumerate(argv_values)
    )
    if sum(len(value) for value in argv) > 2048:
        raise BenchmarkError("document", "network fixture argv bytes exceed the bound")
    executable = Path(argv[0])
    if not executable.is_absolute():
        raise BenchmarkError("document", "network fixture executable must be absolute")
    try:
        metadata = os.lstat(executable)
    except OSError as error:
        raise BenchmarkError("fixture", "network fixture executable is unavailable") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or not metadata.st_mode & stat.S_IXUSR
        or stat.S_ISLNK(metadata.st_mode)
    ):
        raise BenchmarkError(
            "fixture", "network fixture executable must be a regular executable"
        )
    timeout_seconds = _bounded_int(
        root["timeout_seconds"], 1, 600, "network fixture.timeout_seconds"
    )
    return NetworkFixture(
        argv=argv,
        backend=_closed_label(root["backend"], "network fixture.backend"),
        credential_mode="none",
        method=_closed_label(root["method"], "network fixture.method"),
        unit=_closed_label(root["unit"], "network fixture.unit"),
        workload=_closed_label(root["workload"], "network fixture.workload"),
        timeout_seconds=timeout_seconds,
        document_sha256=hashlib.sha256(canonical_bytes).hexdigest(),
        executable_sha256=_sha256(executable),
        executable_device=metadata.st_dev,
        executable_inode=metadata.st_ino,
    )


def read_network_fixture(path: Path) -> NetworkFixture:
    document, data = _read_canonical_document(path, "network fixture")
    return parse_network_fixture_document(document, data)


def summarize(raw: Sequence[int]) -> dict[str, int]:
    if not raw or len(raw) % 2 == 0:
        raise BenchmarkError("report", "raw observations must have a nonempty odd count")
    values = [_u64(value, "raw observation") for value in raw]
    ordered = sorted(values)
    return {
        "count": len(values),
        "max": ordered[-1],
        "median": ordered[len(ordered) // 2],
        "min": ordered[0],
    }


def _measurement(name: str, method: str, unit: str, raw: Sequence[int]) -> dict[str, object]:
    values = list(raw)
    return {
        "method": method,
        "name": name,
        "raw": values,
        "summary": summarize(values),
        "unit": unit,
    }


def _comparison_identity(report: Mapping[str, object]) -> dict[str, object]:
    definitions = []
    for measurement in report["measurements"]:  # type: ignore[index]
        definitions.append(
            {
                "method": measurement["method"],
                "name": measurement["name"],
                "unit": measurement["unit"],
            }
        )
    identity: dict[str, object] = {
        "environment": report["environment"],
        "measurement_definitions": definitions,
        "policy": report["policy"],
    }
    if "network" in report:
        identity["network_fixture"] = report["network"]["fixture"]  # type: ignore[index]
    return identity


def comparison_key(report: Mapping[str, object]) -> str:
    return hashlib.sha256(canonical_json(_comparison_identity(report))).hexdigest()


def assemble_report(
    config: BenchmarkConfig,
    environment: dict[str, object],
    observations: Mapping[str, Sequence[int]],
    *,
    fixture: Optional[NetworkFixture] = None,
    network_raw: Optional[Sequence[int]] = None,
) -> dict[str, object]:
    measurements = []
    for name, method, unit in MEASUREMENT_DEFINITIONS:
        if name not in observations:
            raise BenchmarkError("report", f"missing observation series: {name}")
        measurements.append(_measurement(name, method, unit, observations[name]))
    policy: dict[str, object] = {
        "iterations": config.iterations,
        "page_cache": "uncontrolled",
        "publication": "absent-only-after-cleanup",
        "rss_method": "ps-rss-kib-v1",
        "telemetry_method": "fifo-fill-eagain-flush-fail-drain-retry-v1",
        "timeouts": config.timeouts.document(),
        "warmups": config.warmups,
    }
    report: dict[str, object] = {
        "comparison_key": "0" * 64,
        "environment": environment,
        "measurements": measurements,
        "policy": policy,
        "schema_version": SCHEMA_VERSION,
    }
    if fixture is not None:
        if network_raw is None:
            raise BenchmarkError("report", "network observations are missing")
        report["network"] = {
            "fixture": fixture.identity(),
            "raw": list(network_raw),
            "summary": summarize(network_raw),
        }
    elif network_raw is not None:
        raise BenchmarkError("report", "network observations require an explicit fixture")
    report["comparison_key"] = comparison_key(report)
    validate_report_document(report)
    return report


def _validate_environment(value: object) -> dict[str, Any]:
    environment = _object(
        value,
        (
            "backend",
            "binary",
            "build",
            "cpu",
            "guest",
            "host_label",
            "operating_system",
            "tracing",
        ),
        "report.environment",
    )
    host_label = _string(environment["host_label"], "report host label", maximum=64)
    if HOST_LABEL_RE.fullmatch(host_label) is None:
        raise BenchmarkError("report", "report host label has an unsafe shape")
    if environment["tracing"] != "disabled":
        raise BenchmarkError("report", "report tracing must be disabled")

    backend = _object(
        environment["backend"],
        ("hypervisor", "memory_mib", "transport", "vcpu_count"),
        "report.environment.backend",
    )
    if (
        backend["hypervisor"] != "Hypervisor.framework"
        or backend["transport"] != "virtio-mmio"
        or backend["vcpu_count"] != 1
        or backend["memory_mib"] != 256
    ):
        raise BenchmarkError("report", "report backend identity drifted")

    binary = _object(
        environment["binary"],
        ("sha256", "signing", "size_bytes"),
        "report.environment.binary",
    )
    _digest(binary["sha256"], "report binary digest")
    _u64(binary["size_bytes"], "report binary size")
    if binary["signing"] != "ad-hoc-hvf-entitlement-v1":
        raise BenchmarkError("report", "report binary signing identity drifted")

    build = _object(
        environment["build"],
        (
            "cargo_lock_sha256",
            "cargo_version",
            "commit",
            "features",
            "profile",
            "rustc_version",
            "source_state",
            "target",
            "tree",
        ),
        "report.environment.build",
    )
    _digest(build["cargo_lock_sha256"], "Cargo.lock digest")
    _git_object(build["commit"], "build commit")
    _git_object(build["tree"], "build tree")
    _string(build["cargo_version"], "cargo version", maximum=128)
    _string(build["rustc_version"], "rustc version", maximum=256)
    if (
        build["features"] != []
        or build["profile"] != "release"
        or build["source_state"] != "clean"
        or build["target"] != "aarch64-apple-darwin"
    ):
        raise BenchmarkError("report", "report build boundary drifted")

    cpu = _object(
        environment["cpu"],
        ("architecture", "brand", "hardware_model", "logical_count"),
        "report.environment.cpu",
    )
    if cpu["architecture"] != "arm64":
        raise BenchmarkError("report", "report CPU architecture must be arm64")
    _string(cpu["brand"], "CPU brand", maximum=128)
    _string(cpu["hardware_model"], "hardware model", maximum=128)
    _bounded_int(cpu["logical_count"], 1, 1024, "logical CPU count")

    operating_system = _object(
        environment["operating_system"],
        ("kernel_release", "macos_build", "macos_version"),
        "report.environment.operating_system",
    )
    for key in ("kernel_release", "macos_build", "macos_version"):
        _string(operating_system[key], f"operating system {key}", maximum=128)

    guest = _object(
        environment["guest"],
        (
            "boot_args",
            "compute_checksum",
            "compute_operations",
            "kernel_sha256",
            "kernel_size_bytes",
            "rootfs_recipe_sha256",
            "rootfs_sha256",
            "rootfs_size_bytes",
            "storage_block_bytes",
            "storage_bytes",
            "storage_checksum",
            "workload_protocol",
            "workload_source_sha256",
        ),
        "report.environment.guest",
    )
    if (
        guest["boot_args"] != WORKLOAD_BOOT_ARGS
        or guest["compute_operations"] != specification_workload.COMPUTE_OPERATIONS
        or guest["compute_checksum"] != specification_workload.COMPUTE_CHECKSUM
        or guest["storage_bytes"] != specification_workload.STORAGE_BYTES
        or guest["storage_block_bytes"] != specification_workload.STORAGE_BLOCK_BYTES
        or guest["workload_protocol"] != "bangbang-specification-workload-v1"
    ):
        raise BenchmarkError("report", "report guest workload identity drifted")
    for key in (
        "kernel_sha256",
        "rootfs_recipe_sha256",
        "rootfs_sha256",
        "workload_source_sha256",
    ):
        _digest(guest[key], f"guest {key}")
    for key in ("kernel_size_bytes", "rootfs_size_bytes", "storage_checksum"):
        _u64(guest[key], f"guest {key}")
    return environment


def _validate_policy(value: object) -> dict[str, Any]:
    policy = _object(
        value,
        (
            "iterations",
            "page_cache",
            "publication",
            "rss_method",
            "telemetry_method",
            "timeouts",
            "warmups",
        ),
        "report.policy",
    )
    iterations = _bounded_int(policy["iterations"], 3, 31, "report iterations")
    if iterations % 2 == 0:
        raise BenchmarkError("report", "report iterations must be odd")
    _bounded_int(policy["warmups"], 0, 10, "report warmups")
    if (
        policy["page_cache"] != "uncontrolled"
        or policy["publication"] != "absent-only-after-cleanup"
        or policy["rss_method"] != "ps-rss-kib-v1"
        or policy["telemetry_method"]
        != "fifo-fill-eagain-flush-fail-drain-retry-v1"
    ):
        raise BenchmarkError("report", "report policy identity drifted")
    timeout_document = _object(
        policy["timeouts"],
        (
            "artifact_seconds",
            "build_seconds",
            "guest_seconds",
            "network_seconds",
            "request_seconds",
            "startup_seconds",
            "terminate_seconds",
        ),
        "report.policy.timeouts",
    )
    parse_config_document(
        {
            "host_label": "validation",
            "iterations": iterations,
            "schema_version": 1,
            "timeouts": timeout_document,
            "tracing": "disabled",
            "warmups": policy["warmups"],
        }
    )
    return policy


def _validate_summary(value: object, raw: list[int], label: str) -> None:
    summary = _object(value, ("count", "max", "median", "min"), label)
    for key in ("count", "max", "median", "min"):
        _u64(summary[key], f"{label}.{key}")
    if summary != summarize(raw):
        raise BenchmarkError("report", f"{label} does not match raw observations")


def validate_report_document(document: object) -> dict[str, Any]:
    report = _object(
        document,
        ("comparison_key", "environment", "measurements", "policy", "schema_version"),
        "report",
        optional=("network",),
    )
    if report["schema_version"] != SCHEMA_VERSION:
        raise BenchmarkError("report", "report schema_version must be 1")
    _digest(report["comparison_key"], "report comparison key")
    _validate_environment(report["environment"])
    policy = _validate_policy(report["policy"])
    iterations = policy["iterations"]

    measurements = _array(report["measurements"], "report.measurements")
    if len(measurements) != len(MEASUREMENT_DEFINITIONS):
        raise BenchmarkError("report", "report measurement set is not exact")
    raw_by_name: dict[str, list[int]] = {}
    for index, ((name, method, unit), value) in enumerate(
        zip(MEASUREMENT_DEFINITIONS, measurements)
    ):
        measurement = _object(
            value,
            ("method", "name", "raw", "summary", "unit"),
            f"report.measurements[{index}]",
        )
        if (
            measurement["name"] != name
            or measurement["method"] != method
            or measurement["unit"] != unit
        ):
            raise BenchmarkError("report", "report measurement identity or order drifted")
        raw = _array(measurement["raw"], f"measurement {name} raw")
        if len(raw) != iterations:
            raise BenchmarkError("report", f"measurement {name} count drifted")
        values = [_u64(value, f"measurement {name} observation") for value in raw]
        _validate_summary(measurement["summary"], values, f"measurement {name} summary")
        raw_by_name[name] = values

    filled = raw_by_name["metrics_fifo_filled_bytes"]
    drained = raw_by_name["metrics_fifo_drained_bytes"]
    missed = raw_by_name["metrics_missed_count"]
    if any(value == 0 for value in filled):
        raise BenchmarkError("report", "metrics FIFO fill observations must be positive")
    if any(
        drained_value < filled_value
        for filled_value, drained_value in zip(filled, drained)
    ):
        raise BenchmarkError("report", "metrics FIFO drain must contain every filler byte")
    if any(value != 1 for value in missed):
        raise BenchmarkError("report", "metrics replay observations must be exactly one")

    if "network" in report:
        network = _object(
            report["network"],
            ("fixture", "raw", "summary"),
            "report.network",
        )
        fixture = _object(
            network["fixture"],
            (
                "backend",
                "document_sha256",
                "executable_sha256",
                "method",
                "unit",
                "workload",
            ),
            "report.network.fixture",
        )
        for key in ("backend", "method", "unit", "workload"):
            _closed_label(fixture[key], f"network fixture {key}")
        _digest(fixture["document_sha256"], "network fixture document digest")
        _digest(fixture["executable_sha256"], "network fixture executable digest")
        raw = _array(network["raw"], "report.network.raw")
        if len(raw) != iterations:
            raise BenchmarkError("report", "network observation count drifted")
        values = [_u64(value, "network observation") for value in raw]
        _validate_summary(network["summary"], values, "network summary")

    expected_key = comparison_key(report)
    if report["comparison_key"] != expected_key:
        raise BenchmarkError("report", "report comparison key does not match its identity")
    return report


def read_report(path: Path) -> dict[str, Any]:
    document, _data = _read_canonical_document(path, "benchmark report")
    return validate_report_document(document)


def comparison_document(previous: object, current: object) -> dict[str, object]:
    before = validate_report_document(previous)
    after = validate_report_document(current)
    if before["comparison_key"] != after["comparison_key"]:
        raise BenchmarkError("compare", "reports have different comparison identities")
    rows = []
    for before_measurement, after_measurement in zip(
        before["measurements"], after["measurements"]
    ):
        rows.append(
            {
                "current": after_measurement["summary"],
                "method": after_measurement["method"],
                "name": after_measurement["name"],
                "previous": before_measurement["summary"],
                "unit": after_measurement["unit"],
            }
        )
    comparison: dict[str, object] = {
        "comparison_key": before["comparison_key"],
        "measurements": rows,
        "schema_version": SCHEMA_VERSION,
    }
    if "network" in before:
        comparison["network"] = {
            "current": after["network"]["summary"],
            "fixture": after["network"]["fixture"],
            "previous": before["network"]["summary"],
        }
    return comparison


def _clean_environment() -> dict[str, str]:
    environment = dict(os.environ)
    for key in (
        "BANGBANG_GUEST_ARTIFACTS_DIR",
        "BANGBANG_GUEST_POLICY_INTERNAL",
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
        "CARGO_PROFILE_RELEASE_DEBUG",
        "CARGO_PROFILE_RELEASE_LTO",
        "CARGO_PROFILE_RELEASE_OPT_LEVEL",
        "CARGO_TARGET_DIR",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTFLAGS",
    ):
        environment.pop(key, None)
    environment["LANG"] = "C"
    environment["LC_ALL"] = "C"
    return environment


class _Capture:
    def __init__(self, limit: int) -> None:
        self._limit = limit
        self._bytes = bytearray()
        self._truncated = False
        self._error: Optional[BaseException] = None
        self._lock = threading.Lock()

    def append(self, data: bytes) -> None:
        with self._lock:
            if len(self._bytes) + len(data) > self._limit:
                remaining = max(0, self._limit - len(self._bytes))
                self._bytes.extend(data[:remaining])
                self._truncated = True
            else:
                self._bytes.extend(data)

    def fail(self, error: BaseException) -> None:
        with self._lock:
            self._error = error

    def result(self) -> tuple[bytes, bool, Optional[BaseException]]:
        with self._lock:
            return bytes(self._bytes), self._truncated, self._error


def _pump(stream: BinaryIO, capture: _Capture, condition: Optional[threading.Condition]) -> None:
    try:
        while True:
            data = os.read(stream.fileno(), 4096)
            if not data:
                return
            capture.append(data)
            if condition is not None:
                with condition:
                    condition.notify_all()
    except BaseException as error:  # pragma: no cover - defensive pipe failure
        capture.fail(error)
        if condition is not None:
            with condition:
                condition.notify_all()


def _signal_group(process: Any, signal_number: int) -> None:
    try:
        os.killpg(process.pid, signal_number)
    except ProcessLookupError:
        return
    except OSError as error:
        raise BenchmarkError("process", "failed to signal an owned process group") from error


def _group_exists(process: Any) -> bool:
    try:
        os.killpg(process.pid, 0)
    except ProcessLookupError:
        return False
    except OSError as error:
        raise BenchmarkError("process", "failed to inspect an owned process group") from error
    return True


def _wait_group_absent(process: Any, grace_seconds: float) -> bool:
    deadline = time.monotonic() + grace_seconds
    while _group_exists(process):
        if time.monotonic() >= deadline:
            return False
        time.sleep(POLL_SECONDS)
    return True


def _terminate(process: Any, grace_seconds: float) -> int:
    if process.poll() is None:
        _signal_group(process, signal.SIGTERM)
        try:
            returncode = process.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            _signal_group(process, signal.SIGKILL)
            try:
                returncode = process.wait(timeout=grace_seconds)
            except subprocess.TimeoutExpired as error:
                raise BenchmarkError("process", "owned process group did not terminate") from error
    else:
        returncode = process.wait(timeout=grace_seconds)
    if _group_exists(process):
        _signal_group(process, signal.SIGTERM)
        if not _wait_group_absent(process, grace_seconds):
            _signal_group(process, signal.SIGKILL)
            if not _wait_group_absent(process, grace_seconds):
                raise BenchmarkError("process", "owned process group did not disappear")
    return returncode


def run_command(
    arguments: Sequence[str],
    *,
    timeout_seconds: float,
    phase: str,
    cwd: Path = REPOSITORY_ROOT,
    environment: Optional[Mapping[str, str]] = None,
    check: bool = True,
) -> CommandOutcome:
    try:
        process = subprocess.Popen(
            tuple(arguments),
            cwd=cwd,
            env=dict(environment) if environment is not None else _clean_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        raise BenchmarkError("tool", f"failed to start {phase}") from error
    if process.stdout is None or process.stderr is None:  # pragma: no cover
        _terminate(process, 1)
        raise BenchmarkError("tool", f"failed to capture {phase}")
    captures = (_Capture(MAX_CAPTURE_BYTES), _Capture(MAX_CAPTURE_BYTES))
    threads = (
        threading.Thread(target=_pump, args=(process.stdout, captures[0], None), daemon=True),
        threading.Thread(target=_pump, args=(process.stderr, captures[1], None), daemon=True),
    )
    for thread in threads:
        thread.start()
    deadline = time.monotonic() + timeout_seconds
    try:
        while process.poll() is None:
            if time.monotonic() >= deadline:
                raise BenchmarkError("timeout", f"{phase} exceeded its deadline")
            time.sleep(POLL_SECONDS)
        returncode = process.wait(timeout=1)
        _terminate(process, min(5.0, timeout_seconds))
    except BaseException:
        _terminate(process, min(5.0, timeout_seconds))
        raise
    finally:
        for thread in threads:
            thread.join(timeout=2)
        for stream in (process.stdout, process.stderr):
            try:
                stream.close()
            except (OSError, ValueError):
                pass
        for thread in threads:
            thread.join(timeout=1)
    stdout, stdout_truncated, stdout_error = captures[0].result()
    stderr, stderr_truncated, stderr_error = captures[1].result()
    if (
        stdout_truncated
        or stderr_truncated
        or stdout_error is not None
        or stderr_error is not None
        or any(thread.is_alive() for thread in threads)
    ):
        raise BenchmarkError("tool", f"{phase} output exceeded or failed its bound")
    if check and returncode != 0:
        raise BenchmarkError("tool", f"{phase} failed")
    return CommandOutcome(returncode, stdout, stderr)


@dataclass(frozen=True)
class PrivateSession:
    path: Path
    device: int
    inode: int
    uid: int

    @classmethod
    def create(cls, parent: Optional[Path] = None) -> "PrivateSession":
        base = parent if parent is not None else Path(tempfile.gettempdir())
        try:
            path = Path(tempfile.mkdtemp(prefix="bbspec.", dir=base))
            os.chmod(path, PRIVATE_DIRECTORY_MODE)
            metadata = os.lstat(path)
        except OSError as error:
            raise BenchmarkError("session", "failed to create a private session") from error
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
        ):
            raise BenchmarkError("session", "private session identity is invalid")
        return cls(path, metadata.st_dev, metadata.st_ino, metadata.st_uid)

    def _verify(self) -> None:
        try:
            metadata = os.lstat(self.path)
        except OSError as error:
            raise BenchmarkError("cleanup", "private session is unavailable") from error
        if (
            metadata.st_dev != self.device
            or metadata.st_ino != self.inode
            or metadata.st_uid != self.uid
            or not stat.S_ISDIR(metadata.st_mode)
            or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
        ):
            raise BenchmarkError("cleanup", "private session identity changed")

    def cleanup(self) -> None:
        self._verify()
        _clean_directory(self.path)
        self._verify()
        try:
            os.rmdir(self.path)
        except OSError as error:
            raise BenchmarkError("cleanup", "failed to remove a private session") from error


def _clean_directory(path: Path) -> None:
    try:
        entries = list(os.scandir(path))
    except OSError as error:
        raise BenchmarkError("cleanup", "failed to enumerate a private session") from error
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
            raise BenchmarkError("cleanup", "failed to clean a private session child") from error


@contextmanager
def private_session(parent: Optional[Path] = None) -> Iterator[PrivateSession]:
    session = PrivateSession.create(parent)
    active_error: Optional[BaseException] = None
    try:
        yield session
    except BaseException as error:
        active_error = error
        raise
    finally:
        try:
            session.cleanup()
        except BenchmarkError as cleanup_error:
            if active_error is not None:
                raise cleanup_error from active_error
            raise


def _one_line(outcome: CommandOutcome, phase: str, *, maximum: int = 256) -> str:
    try:
        text = outcome.stdout.decode("ascii")
    except UnicodeDecodeError as error:
        raise BenchmarkError("tool", f"{phase} returned non-ASCII output") from error
    lines = text.splitlines()
    if len(lines) != 1:
        raise BenchmarkError("tool", f"{phase} returned an unexpected line count")
    return _string(lines[0], phase, maximum=maximum)


def _artifact_path(outcome: CommandOutcome, phase: str) -> Path:
    path = Path(_one_line(outcome, phase, maximum=1024))
    if not path.is_absolute():
        raise BenchmarkError("artifact", f"{phase} returned a relative path")
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise BenchmarkError("artifact", f"{phase} output is unavailable") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise BenchmarkError("artifact", f"{phase} output is not regular")
    return path


def preflight(config: BenchmarkConfig) -> None:
    if sys.version_info < (3, 9):
        raise BenchmarkError("platform", "Python 3.9 or newer is required")
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise BenchmarkError("platform", "collection requires Apple Silicon macOS")
    for command in ("cargo", "codesign", "rustc", "sysctl"):
        if shutil.which(command) is None:
            raise BenchmarkError("platform", f"required tool is unavailable: {command}")
    support = run_command(
        ("sysctl", "-n", "kern.hv_support"),
        timeout_seconds=config.timeouts.request_seconds,
        phase="HVF support preflight",
        check=False,
    )
    if support.returncode != 0 or support.stdout.strip() != b"1":
        support = run_command(
            ("sysctl", "-n", "kern.hv.supported"),
            timeout_seconds=config.timeouts.request_seconds,
            phase="HVF compatibility preflight",
            check=False,
        )
    if support.returncode != 0 or support.stdout.strip() != b"1":
        raise BenchmarkError("platform", "Hypervisor.framework is not supported")
    disabled = run_command(
        ("sysctl", "-n", "kern.hv_disable"),
        timeout_seconds=config.timeouts.request_seconds,
        phase="HVF disable preflight",
        check=False,
    )
    if disabled.returncode == 0 and disabled.stdout.strip() == b"1":
        raise BenchmarkError("platform", "Hypervisor.framework is disabled")
    status = run_command(
        ("git", "status", "--porcelain", "--untracked-files=normal"),
        timeout_seconds=config.timeouts.request_seconds,
        phase="source cleanliness check",
    )
    if status.stdout:
        raise BenchmarkError("source", "collection requires a clean source tree")


def _storage_checksum(path: Path) -> int:
    checksum = 0xCBF29CE484222325
    remaining = specification_workload.STORAGE_BYTES
    try:
        with path.open("rb") as source:
            while remaining:
                chunk = source.read(min(specification_workload.STORAGE_BLOCK_BYTES, remaining))
                if len(chunk) != min(specification_workload.STORAGE_BLOCK_BYTES, remaining):
                    raise BenchmarkError("artifact", "rootfs is shorter than the workload range")
                for byte in chunk:
                    checksum ^= byte
                    checksum = (checksum * 0x00000100000001B3) & U64_MAX
                remaining -= len(chunk)
    except OSError as error:
        raise BenchmarkError("artifact", "failed to read the workload rootfs") from error
    return checksum


def prepare_artifacts(config: BenchmarkConfig) -> PreparedArtifacts:
    manifest = guest_artifact_policy.load_manifest()
    kernel = _artifact_path(
        run_command(
            (os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-kernel.sh"),),
            timeout_seconds=config.timeouts.artifact_seconds,
            phase="kernel preparation",
        ),
        "kernel preparation",
    )
    rootfs = _artifact_path(
        run_command(
            (
                os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),
                "--format",
                "ext4",
                "--ext4-size",
                "512M",
                "--direct-boot-init",
            ),
            timeout_seconds=config.timeouts.artifact_seconds,
            phase="workload rootfs preparation",
        ),
        "workload rootfs preparation",
    )
    kernel_spec = manifest.downloads["kernel"]
    kernel_metadata = os.lstat(kernel)
    if (
        kernel_metadata.st_size != kernel_spec.size_bytes
        or _sha256(kernel) != kernel_spec.sha256
    ):
        raise BenchmarkError("artifact", "kernel identity differs from the checked authority")
    sidecar_path = Path(f"{rootfs}.bangbang.json")
    sidecar, _bytes = _read_canonical_document(sidecar_path, "rootfs sidecar")
    sidecar = _object(
        sidecar,
        (
            "filesystem_check",
            "output_sha256",
            "output_size_bytes",
            "recipe_sha256",
            "requested_size_bytes",
            "schema_version",
            "source_sha256",
            "source_size_bytes",
            "tool_versions",
            "variant",
        ),
        "rootfs sidecar",
    )
    rootfs_metadata = os.lstat(rootfs)
    rootfs_sha256 = _sha256(rootfs)
    if (
        sidecar["schema_version"] != 1
        or sidecar["variant"] != "direct-boot-v109"
        or sidecar["requested_size_bytes"] != 512 * 1024 * 1024
        or sidecar["filesystem_check"] != "e2fsck -fn"
        or sidecar["output_size_bytes"] != rootfs_metadata.st_size
        or sidecar["output_sha256"] != rootfs_sha256
    ):
        raise BenchmarkError("artifact", "rootfs sidecar differs from the prepared artifact")
    recipe_sha256 = _digest(sidecar["recipe_sha256"], "rootfs recipe digest")
    return PreparedArtifacts(
        kernel,
        kernel_spec.sha256,
        kernel_spec.size_bytes,
        rootfs,
        rootfs_sha256,
        rootfs_metadata.st_size,
        recipe_sha256,
        _storage_checksum(rootfs),
    )


def build_signed_binary(
    session_path: Path, config: BenchmarkConfig
) -> SignedBuild:
    commit = _one_line(
        run_command(
            ("git", "rev-parse", "HEAD"),
            timeout_seconds=config.timeouts.request_seconds,
            phase="Git commit inspection",
        ),
        "Git commit inspection",
    )
    tree = _one_line(
        run_command(
            ("git", "rev-parse", "HEAD^{tree}"),
            timeout_seconds=config.timeouts.request_seconds,
            phase="Git tree inspection",
        ),
        "Git tree inspection",
    )
    _git_object(commit, "Git commit")
    _git_object(tree, "Git tree")
    cargo_version = _one_line(
        run_command(
            ("cargo", "--version"),
            timeout_seconds=config.timeouts.request_seconds,
            phase="Cargo version inspection",
        ),
        "Cargo version inspection",
    )
    rustc_outcome = run_command(
        ("rustc", "--version"),
        timeout_seconds=config.timeouts.request_seconds,
        phase="Rust compiler inspection",
    )
    try:
        rustc_version = rustc_outcome.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise BenchmarkError("build", "rustc version is not ASCII") from error
    _string(rustc_version, "rustc version", maximum=256)
    target_dir = session_path / "target"
    run_command(
        (
            "cargo",
            "build",
            "--package",
            "bangbang",
            "--release",
            "--locked",
            "--no-default-features",
            "--target",
            "aarch64-apple-darwin",
            "--target-dir",
            os.fspath(target_dir),
        ),
        timeout_seconds=config.timeouts.build_seconds,
        phase="locked default release build",
    )
    unsigned = target_dir / "aarch64-apple-darwin/release/bangbang"
    signed = session_path / "bangbang-signed"
    run_command(
        (
            os.fspath(REPOSITORY_ROOT / "scripts/sign-hvf-binary.sh"),
            os.fspath(unsigned),
            os.fspath(signed),
        ),
        timeout_seconds=config.timeouts.build_seconds,
        phase="HVF binary signing",
    )
    run_command(
        ("codesign", "--verify", "--strict", "--verbose=2", os.fspath(signed)),
        timeout_seconds=config.timeouts.request_seconds,
        phase="HVF signature verification",
    )
    try:
        metadata = os.lstat(signed)
        data = signed.read_bytes()
    except OSError as error:
        raise BenchmarkError("build", "signed binary is unavailable") from error
    if not stat.S_ISREG(metadata.st_mode) or not metadata.st_mode & stat.S_IXUSR:
        raise BenchmarkError("build", "signed binary identity is invalid")
    if TRACE_MARKER in data:
        raise BenchmarkError("build", "default release binary contains the tracing marker")
    return SignedBuild(
        signed,
        hashlib.sha256(data).hexdigest(),
        len(data),
        commit,
        tree,
        cargo_version,
        rustc_version,
    )


def _sysctl(name: str, timeout_seconds: int) -> str:
    return _one_line(
        run_command(
            ("sysctl", "-n", name),
            timeout_seconds=timeout_seconds,
            phase=f"sysctl {name}",
        ),
        f"sysctl {name}",
        maximum=128,
    )


def inspect_environment(
    config: BenchmarkConfig,
    artifacts: PreparedArtifacts,
    build: SignedBuild,
) -> dict[str, object]:
    request_timeout = config.timeouts.request_seconds
    macos_version = _one_line(
        run_command(
            ("sw_vers", "-productVersion"),
            timeout_seconds=request_timeout,
            phase="macOS version inspection",
        ),
        "macOS version inspection",
    )
    macos_build = _one_line(
        run_command(
            ("sw_vers", "-buildVersion"),
            timeout_seconds=request_timeout,
            phase="macOS build inspection",
        ),
        "macOS build inspection",
    )
    logical_count_text = _sysctl("hw.logicalcpu", request_timeout)
    if not logical_count_text.isdigit():
        raise BenchmarkError("environment", "logical CPU count is not an integer")
    logical_count = int(logical_count_text)
    environment: dict[str, object] = {
        "backend": {
            "hypervisor": "Hypervisor.framework",
            "memory_mib": 256,
            "transport": "virtio-mmio",
            "vcpu_count": 1,
        },
        "binary": {
            "sha256": build.sha256,
            "signing": "ad-hoc-hvf-entitlement-v1",
            "size_bytes": build.size_bytes,
        },
        "build": {
            "cargo_lock_sha256": _sha256(REPOSITORY_ROOT / "Cargo.lock"),
            "cargo_version": build.cargo_version,
            "commit": build.commit,
            "features": [],
            "profile": "release",
            "rustc_version": build.rustc_version,
            "source_state": "clean",
            "target": "aarch64-apple-darwin",
            "tree": build.tree,
        },
        "cpu": {
            "architecture": "arm64",
            "brand": _sysctl("machdep.cpu.brand_string", request_timeout),
            "hardware_model": _sysctl("hw.model", request_timeout),
            "logical_count": logical_count,
        },
        "guest": {
            "boot_args": WORKLOAD_BOOT_ARGS,
            "compute_checksum": specification_workload.COMPUTE_CHECKSUM,
            "compute_operations": specification_workload.COMPUTE_OPERATIONS,
            "kernel_sha256": artifacts.kernel_sha256,
            "kernel_size_bytes": artifacts.kernel_size_bytes,
            "rootfs_recipe_sha256": artifacts.rootfs_recipe_sha256,
            "rootfs_sha256": artifacts.rootfs_sha256,
            "rootfs_size_bytes": artifacts.rootfs_size_bytes,
            "storage_block_bytes": specification_workload.STORAGE_BLOCK_BYTES,
            "storage_bytes": specification_workload.STORAGE_BYTES,
            "storage_checksum": artifacts.storage_checksum,
            "workload_protocol": "bangbang-specification-workload-v1",
            "workload_source_sha256": _sha256(
                REPOSITORY_ROOT / "scripts/guest/specification-benchmark.rs"
            ),
        },
        "host_label": config.host_label,
        "operating_system": {
            "kernel_release": platform.release(),
            "macos_build": macos_build,
            "macos_version": macos_version,
        },
        "tracing": "disabled",
    }
    _validate_environment(environment)
    return environment


class VmmProcess:
    def __init__(self, arguments: Sequence[str], session_path: Path) -> None:
        environment = {
            "HOME": os.fspath(session_path),
            "LANG": "C",
            "LC_ALL": "C",
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": os.fspath(session_path),
        }
        try:
            self.process = subprocess.Popen(
                tuple(arguments),
                cwd=REPOSITORY_ROOT,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
        except OSError as error:
            raise BenchmarkError("process", "failed to start the signed VMM") from error
        if (
            self.process.stdin is None
            or self.process.stdout is None
            or self.process.stderr is None
        ):  # pragma: no cover
            _terminate(self.process, 1)
            raise BenchmarkError("process", "failed to attach signed VMM pipes")
        self.stdout_capture = _Capture(MAX_CAPTURE_BYTES)
        self.stderr_capture = _Capture(MAX_CAPTURE_BYTES)
        self.condition = threading.Condition()
        self.threads = (
            threading.Thread(
                target=_pump,
                args=(self.process.stdout, self.stdout_capture, self.condition),
                daemon=True,
            ),
            threading.Thread(
                target=_pump,
                args=(self.process.stderr, self.stderr_capture, self.condition),
                daemon=True,
            ),
        )
        self.finished = False
        for thread in self.threads:
            thread.start()

    def _captures(self) -> tuple[bytes, bytes]:
        stdout, stdout_truncated, stdout_error = self.stdout_capture.result()
        stderr, stderr_truncated, stderr_error = self.stderr_capture.result()
        if stdout_truncated or stderr_truncated:
            raise BenchmarkError("process", "signed VMM output exceeded its bound")
        if stdout_error is not None or stderr_error is not None:
            raise BenchmarkError("process", "failed to read signed VMM output")
        if WORKLOAD_FAILURE in stdout or WORKLOAD_FAILURE in stderr:
            raise BenchmarkError("guest", "guest workload reported failure")
        return stdout, stderr

    def wait_marker(self, marker: bytes, timeout_seconds: float) -> None:
        deadline = time.monotonic() + timeout_seconds
        with self.condition:
            while True:
                stdout, stderr = self._captures()
                if marker in stdout or marker in stderr:
                    return
                if self.process.poll() is not None:
                    raise BenchmarkError("process", "signed VMM exited before a required marker")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise BenchmarkError("timeout", "signed VMM marker exceeded its deadline")
                self.condition.wait(timeout=min(POLL_SECONDS, remaining))

    def assert_marker_absent(self, marker: bytes) -> None:
        stdout, stderr = self._captures()
        if marker in stdout or marker in stderr:
            raise BenchmarkError("guest", "timed guest output appeared before release")

    def write_stdin(self, value: bytes) -> None:
        try:
            self.process.stdin.write(value)
            self.process.stdin.flush()
        except (OSError, ValueError) as error:
            raise BenchmarkError("process", "failed to release the guest workload") from error

    def wait_exit(self, timeout_seconds: float) -> tuple[bytes, bytes]:
        try:
            returncode = self.process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired as error:
            raise BenchmarkError("timeout", "guest workload did not exit") from error
        for thread in self.threads:
            thread.join(timeout=2)
        if any(thread.is_alive() for thread in self.threads):
            raise BenchmarkError("process", "signed VMM output did not drain")
        stdout, stderr = self._captures()
        if returncode != 0 or WORKLOAD_SUCCESS not in stdout + stderr:
            raise BenchmarkError("process", "signed VMM did not exit successfully")
        return stdout, stderr

    def finish(self, grace_seconds: float) -> None:
        if self.finished:
            return
        self.finished = True
        _terminate(self.process, grace_seconds)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            try:
                stream.close()
            except (OSError, ValueError):
                pass
        for thread in self.threads:
            thread.join(timeout=grace_seconds)
        if any(thread.is_alive() for thread in self.threads):
            raise BenchmarkError("process", "signed VMM output pumps did not stop")


def _wait_api_socket(path: Path, process: VmmProcess, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            metadata = os.lstat(path)
        except FileNotFoundError:
            metadata = None
        except OSError as error:
            raise BenchmarkError("socket", "failed to inspect the API socket") from error
        if metadata is not None:
            if (
                not stat.S_ISSOCK(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or stat.S_IMODE(metadata.st_mode) & 0o077
            ):
                raise BenchmarkError("socket", "API socket identity is invalid")
            return
        process._captures()
        if process.process.poll() is not None:
            raise BenchmarkError("process", "signed VMM exited before API readiness")
        if time.monotonic() >= deadline:
            raise BenchmarkError("timeout", "API socket publication exceeded its deadline")
        time.sleep(POLL_SECONDS)


def _wait_socket_absent(path: Path, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            os.lstat(path)
        except FileNotFoundError:
            return
        except OSError as error:
            raise BenchmarkError("socket", "failed to inspect API socket cleanup") from error
        if time.monotonic() >= deadline:
            raise BenchmarkError("cleanup", "API socket was not removed")
        time.sleep(POLL_SECONDS)


def _remaining(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise BenchmarkError("timeout", "API request exceeded its deadline")
    return remaining


def http_json(
    socket_path: Path,
    method: str,
    path: str,
    body: Optional[Mapping[str, object]],
    timeout_seconds: float,
) -> HttpResponse:
    body_bytes = b"" if body is None else json.dumps(
        body, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii")
    headers = [
        f"{method} {path} HTTP/1.1",
        "Host: localhost",
        "Connection: close",
    ]
    if body is not None:
        headers.extend(
            ["Content-Type: application/json", f"Content-Length: {len(body_bytes)}"]
        )
    request = ("\r\n".join(headers) + "\r\n\r\n").encode("ascii") + body_bytes
    if len(request) > MAX_HTTP_REQUEST_BYTES:
        raise BenchmarkError("http", "API request exceeded its bound")
    deadline = time.monotonic() + timeout_seconds
    response = bytearray()
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        client.settimeout(_remaining(deadline))
        client.connect(os.fspath(socket_path))
        client.settimeout(_remaining(deadline))
        client.sendall(request)
        client.shutdown(socket.SHUT_WR)
        while True:
            client.settimeout(_remaining(deadline))
            chunk = client.recv(4096)
            if not chunk:
                break
            response.extend(chunk)
            if len(response) > MAX_HTTP_RESPONSE_BYTES:
                raise BenchmarkError("http", "API response exceeded its bound")
    except BenchmarkError:
        raise
    except (OSError, socket.timeout) as error:
        raise BenchmarkError("http", "API exchange failed") from error
    finally:
        client.close()
    head, separator, response_body = bytes(response).partition(b"\r\n\r\n")
    if not separator:
        raise BenchmarkError("http", "API response has no header boundary")
    lines = head.split(b"\r\n")
    try:
        status_parts = lines[0].decode("ascii").split(" ", 2)
    except UnicodeDecodeError as error:
        raise BenchmarkError("http", "API status line is not ASCII") from error
    if len(status_parts) != 3 or status_parts[0] != "HTTP/1.1":
        raise BenchmarkError("http", "API status line is malformed")
    try:
        status = int(status_parts[1])
    except ValueError as error:
        raise BenchmarkError("http", "API status code is malformed") from error
    parsed_headers: dict[bytes, bytes] = {}
    for line in lines[1:]:
        name, split, value = line.partition(b":")
        lowered = name.lower()
        if not split or lowered in parsed_headers:
            raise BenchmarkError("http", "API response headers are malformed")
        parsed_headers[lowered] = value.strip()
    try:
        content_length = int(parsed_headers[b"content-length"])
    except (KeyError, ValueError) as error:
        raise BenchmarkError("http", "API response Content-Length is invalid") from error
    if content_length != len(response_body):
        raise BenchmarkError("http", "API response body length drifted")
    return HttpResponse(status, response_body)


def _require_no_content(response: HttpResponse, phase: str) -> None:
    if response.status != 204 or response.body:
        raise BenchmarkError("http", f"{phase} did not return empty 204")


def _put(
    socket_path: Path,
    path: str,
    body: Mapping[str, object],
    timeout_seconds: float,
    phase: str,
) -> None:
    _require_no_content(
        http_json(socket_path, "PUT", path, body, timeout_seconds), phase
    )


def _configure_guest(
    socket_path: Path,
    artifacts: PreparedArtifacts,
    metrics_path: Path,
    logger_path: Path,
    timeout_seconds: float,
) -> None:
    _put(
        socket_path,
        "/machine-config",
        {"mem_size_mib": 256, "vcpu_count": 1},
        timeout_seconds,
        "machine configuration",
    )
    _put(
        socket_path,
        "/boot-source",
        {
            "boot_args": WORKLOAD_BOOT_ARGS,
            "kernel_image_path": os.fspath(artifacts.kernel),
        },
        timeout_seconds,
        "boot source",
    )
    _put(
        socket_path,
        "/drives/rootfs",
        {
            "drive_id": "rootfs",
            "is_read_only": True,
            "is_root_device": True,
            "path_on_host": os.fspath(artifacts.rootfs),
        },
        timeout_seconds,
        "root drive",
    )
    _put(
        socket_path,
        "/metrics",
        {"metrics_path": os.fspath(metrics_path)},
        timeout_seconds,
        "metrics sink",
    )
    _put(
        socket_path,
        "/logger",
        {"log_path": os.fspath(logger_path)},
        timeout_seconds,
        "logger sink",
    )
    _put(
        socket_path,
        "/actions",
        {"action_type": "InstanceStart"},
        timeout_seconds,
        "instance start",
    )


def _wait_file_line(path: Path, timeout_seconds: float, label: str) -> bytes:
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            with path.open("rb") as source:
                data = source.read(MAX_CAPTURE_BYTES + 1)
        except FileNotFoundError:
            data = b""
        except OSError as error:
            raise BenchmarkError("file", f"failed to read {label}") from error
        if len(data) > MAX_CAPTURE_BYTES:
            raise BenchmarkError("file", f"{label} exceeded its bound")
        lines = data.splitlines()
        if lines:
            return lines[0]
        if time.monotonic() >= deadline:
            raise BenchmarkError("timeout", f"{label} exceeded its deadline")
        time.sleep(POLL_SECONDS)


def _wait_boot_timer(path: Path, timeout_seconds: float) -> tuple[int, int]:
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            with path.open("rb") as source:
                raw = source.read(MAX_CAPTURE_BYTES + 1)
        except FileNotFoundError:
            raw = b""
        except OSError as error:
            raise BenchmarkError("logger", "boot timer log is unreadable") from error
        if len(raw) > MAX_CAPTURE_BYTES:
            raise BenchmarkError("logger", "boot timer log exceeded its bound")
        try:
            data = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise BenchmarkError("logger", "boot timer log is unreadable") from error
        matches = list(BOOT_TIMER_RE.finditer(data))
        if matches:
            if len(matches) != 1:
                raise BenchmarkError("logger", "boot timer record is not unique")
            wall_us, wall_ms, cpu_us, cpu_ms = (
                int(value) for value in matches[0].groups()
            )
            if wall_ms != wall_us // 1000 or cpu_ms != cpu_us // 1000:
                raise BenchmarkError("logger", "boot timer millisecond fields drifted")
            return wall_us, cpu_us
        if time.monotonic() >= deadline:
            raise BenchmarkError("timeout", "boot timer record exceeded its deadline")
        time.sleep(POLL_SECONDS)


def _metrics_object(line: bytes) -> dict[str, Any]:
    if len(line) > MAX_CAPTURE_BYTES:
        raise BenchmarkError("metrics", "metrics line exceeded its bound")
    try:
        value = json.loads(line, object_pairs_hook=_duplicate_safe_object)
    except BenchmarkError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BenchmarkError("metrics", "metrics line is invalid JSON") from error
    if not isinstance(value, dict):
        raise BenchmarkError("metrics", "metrics line must be an object")
    return value


def _startup_metrics(line: bytes) -> tuple[int, int]:
    value = _metrics_object(line)
    api_server = value.get("api_server")
    if not isinstance(api_server, dict):
        raise BenchmarkError("metrics", "startup metrics family is missing")
    return (
        _u64(api_server.get("process_startup_time_us"), "startup wall metric"),
        _u64(api_server.get("process_startup_time_cpu_us"), "startup CPU metric"),
    )


def _sample_rss(pid: int, timeout_seconds: float) -> int:
    outcome = run_command(
        ("/bin/ps", "-o", "rss=", "-p", str(pid)),
        timeout_seconds=timeout_seconds,
        phase="whole-process RSS sample",
    )
    try:
        value = outcome.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise BenchmarkError("rss", "RSS sample is not ASCII") from error
    if not value.isdigit():
        raise BenchmarkError("rss", "RSS sample is not a canonical integer")
    return _u64(int(value), "RSS sample")


def _vmm_arguments(build: SignedBuild, socket_path: Path, instance_id: str) -> tuple[str, ...]:
    return (
        os.fspath(build.path),
        "--api-sock",
        os.fspath(socket_path),
        "--boot-timer",
        "--id",
        instance_id,
    )


def collect_workload_sample(
    parent: Path,
    artifacts: PreparedArtifacts,
    build: SignedBuild,
    config: BenchmarkConfig,
    sample_index: int,
) -> dict[str, int]:
    with private_session(parent) as session:
        socket_path = session.path / "a.sock"
        metrics_path = session.path / "metrics"
        logger_path = session.path / "logger"
        if len(os.fsencode(socket_path)) >= 104:
            raise BenchmarkError("session", "API socket path is too long")
        process = VmmProcess(
            _vmm_arguments(build, socket_path, f"spec-workload-{sample_index}"),
            session.path,
        )
        try:
            process.wait_marker(API_SOCKET_READY, config.timeouts.startup_seconds)
            _wait_api_socket(socket_path, process, config.timeouts.request_seconds)
            _configure_guest(
                socket_path,
                artifacts,
                metrics_path,
                logger_path,
                config.timeouts.request_seconds,
            )
            initial_line = _wait_file_line(
                metrics_path, config.timeouts.guest_seconds, "initial metrics"
            )
            startup_wall_us, startup_cpu_us = _startup_metrics(initial_line)
            process.wait_marker(WORKLOAD_READY, config.timeouts.guest_seconds)
            boot_wall_us, boot_cpu_us = _wait_boot_timer(
                logger_path, config.timeouts.guest_seconds
            )
            process.assert_marker_absent(WORKLOAD_TIMED)
            rss_kib = _sample_rss(process.process.pid, config.timeouts.request_seconds)
            process.write_stdin(specification_workload.RELEASE_BYTE)
            process.wait_marker(WORKLOAD_SUCCESS, config.timeouts.guest_seconds)
            stdout, stderr = process.wait_exit(config.timeouts.guest_seconds)
            result = specification_workload.parse_transcript(
                stdout + stderr,
                expected_storage_checksum=artifacts.storage_checksum,
            )
            _wait_socket_absent(socket_path, config.timeouts.request_seconds)
            return {
                "guest_compute_duration_ns": result.compute_duration_ns,
                "guest_init_cpu_us": boot_cpu_us,
                "guest_init_wall_us": boot_wall_us,
                "guest_storage_duration_ns": result.storage_duration_ns,
                "process_startup_cpu_us": startup_cpu_us,
                "process_startup_wall_us": startup_wall_us,
                "whole_process_rss_kib": rss_kib,
            }
        finally:
            process.finish(config.timeouts.terminate_seconds)


def _create_fifo(path: Path) -> int:
    try:
        os.mkfifo(path, PRIVATE_FILE_MODE)
        descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        metadata = os.lstat(path)
    except OSError as error:
        raise BenchmarkError("fifo", "failed to create the metrics FIFO") from error
    if not stat.S_ISFIFO(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != PRIVATE_FILE_MODE:
        os.close(descriptor)
        raise BenchmarkError("fifo", "metrics FIFO identity is invalid")
    return descriptor


def _read_fifo_line(descriptor: int, timeout_seconds: float) -> bytes:
    deadline = time.monotonic() + timeout_seconds
    output = bytearray()
    while True:
        try:
            chunk = os.read(descriptor, 4096)
        except BlockingIOError:
            chunk = b""
        except OSError as error:
            raise BenchmarkError("fifo", "failed to read the metrics FIFO") from error
        if chunk:
            output.extend(chunk)
            if len(output) > MAX_FIFO_BYTES:
                raise BenchmarkError("fifo", "metrics FIFO line exceeded its bound")
            newline = output.find(b"\n")
            if newline >= 0:
                if output[newline + 1 :]:
                    raise BenchmarkError("fifo", "metrics FIFO published unexpected extra bytes")
                return bytes(output[:newline])
        if time.monotonic() >= deadline:
            raise BenchmarkError("timeout", "metrics FIFO line exceeded its deadline")
        time.sleep(POLL_SECONDS)


def _fill_fifo(path: Path) -> int:
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
    except OSError as error:
        raise BenchmarkError("fifo", "failed to open the metrics FIFO filler") from error
    total = 0
    try:
        while total <= MAX_FIFO_BYTES:
            try:
                written = os.write(descriptor, FIFO_SENTINEL_CHUNK)
            except BlockingIOError:
                return total
            if written <= 0:
                raise BenchmarkError("fifo", "metrics FIFO filler made no progress")
            total += written
        raise BenchmarkError("fifo", "metrics FIFO did not reach EAGAIN within its bound")
    finally:
        os.close(descriptor)


def _drain_fifo(descriptor: int) -> bytes:
    output = bytearray()
    while True:
        try:
            chunk = os.read(descriptor, 4096)
        except BlockingIOError:
            return bytes(output)
        except OSError as error:
            raise BenchmarkError("fifo", "failed to drain the metrics FIFO") from error
        if not chunk:
            return bytes(output)
        output.extend(chunk)
        if len(output) > MAX_FIFO_BYTES:
            raise BenchmarkError("fifo", "metrics FIFO drain exceeded its bound")


def _require_would_block(response: HttpResponse) -> None:
    if response.status != 400:
        raise BenchmarkError("metrics", "full FIFO flush did not return HTTP 400")
    try:
        value = json.loads(response.body, object_pairs_hook=_duplicate_safe_object)
    except BenchmarkError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BenchmarkError("metrics", "FIFO failure response is invalid JSON") from error
    if value != {"fault_message": EXPECTED_WOULD_BLOCK_FAULT}:
        raise BenchmarkError("metrics", "FIFO failure response is not typed WouldBlock")


def _missed_metrics(line: bytes) -> int:
    value = _metrics_object(line)
    logger = value.get("logger")
    if not isinstance(logger, dict):
        raise BenchmarkError("metrics", "replay metrics logger family is missing")
    count = _u64(logger.get("missed_metrics_count"), "missed metrics replay count")
    if count != 1:
        raise BenchmarkError("metrics", "replay must contain exactly one missed metric")
    return count


def collect_telemetry_sample(
    parent: Path,
    artifacts: PreparedArtifacts,
    build: SignedBuild,
    config: BenchmarkConfig,
    sample_index: int,
) -> dict[str, int]:
    with private_session(parent) as session:
        socket_path = session.path / "a.sock"
        metrics_path = session.path / "metrics.fifo"
        logger_path = session.path / "logger"
        reader = _create_fifo(metrics_path)
        process: Optional[VmmProcess] = None
        try:
            process = VmmProcess(
                _vmm_arguments(build, socket_path, f"spec-telemetry-{sample_index}"),
                session.path,
            )
            process.wait_marker(API_SOCKET_READY, config.timeouts.startup_seconds)
            _wait_api_socket(socket_path, process, config.timeouts.request_seconds)
            _configure_guest(
                socket_path,
                artifacts,
                metrics_path,
                logger_path,
                config.timeouts.request_seconds,
            )
            _metrics_object(_read_fifo_line(reader, config.timeouts.guest_seconds))
            process.wait_marker(WORKLOAD_READY, config.timeouts.guest_seconds)
            process.assert_marker_absent(WORKLOAD_TIMED)
            filled = _fill_fifo(metrics_path)
            failed = http_json(
                socket_path,
                "PUT",
                "/actions",
                {"action_type": "FlushMetrics"},
                config.timeouts.request_seconds,
            )
            _require_would_block(failed)
            drained = _drain_fifo(reader)
            if len(drained) < filled or not drained.startswith(FIFO_SENTINEL_CHUNK[:1]):
                raise BenchmarkError("fifo", "failed-flush drain did not contain the filler")
            retried = http_json(
                socket_path,
                "PUT",
                "/actions",
                {"action_type": "FlushMetrics"},
                config.timeouts.request_seconds,
            )
            _require_no_content(retried, "metrics retry")
            missed = _missed_metrics(
                _read_fifo_line(reader, config.timeouts.request_seconds)
            )
            process.write_stdin(specification_workload.RELEASE_BYTE)
            process.wait_marker(WORKLOAD_SUCCESS, config.timeouts.guest_seconds)
            process.wait_exit(config.timeouts.guest_seconds)
            _wait_socket_absent(socket_path, config.timeouts.request_seconds)
            return {
                "metrics_fifo_drained_bytes": len(drained),
                "metrics_fifo_filled_bytes": filled,
                "metrics_missed_count": missed,
            }
        finally:
            if process is not None:
                process.finish(config.timeouts.terminate_seconds)
            os.close(reader)


def collect_network_sample(fixture: NetworkFixture, session_path: Path) -> int:
    executable = Path(fixture.argv[0])
    try:
        before = os.lstat(executable)
    except OSError as error:
        raise BenchmarkError("fixture", "network fixture executable disappeared") from error
    if (
        before.st_dev != fixture.executable_device
        or before.st_ino != fixture.executable_inode
        or _sha256(executable) != fixture.executable_sha256
    ):
        raise BenchmarkError("fixture", "network fixture executable identity changed")
    with private_session(session_path) as session:
        environment = {
            "HOME": os.fspath(session.path),
            "LANG": "C",
            "LC_ALL": "C",
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": os.fspath(session.path),
        }
        outcome = run_command(
            fixture.argv,
            timeout_seconds=fixture.timeout_seconds,
            phase="network fixture",
            cwd=session.path,
            environment=environment,
        )
    if outcome.stderr:
        raise BenchmarkError("fixture", "network fixture wrote diagnostic output")
    try:
        value = json.loads(outcome.stdout, object_pairs_hook=_duplicate_safe_object)
    except BenchmarkError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BenchmarkError("fixture", "network fixture output is invalid JSON") from error
    if canonical_json(value) != outcome.stdout:
        raise BenchmarkError("fixture", "network fixture output must be canonical JSON")
    output = _object(
        value,
        ("backend", "cleanup", "method", "schema_version", "unit", "value", "workload"),
        "network fixture output",
    )
    if (
        output["schema_version"] != 1
        or output["backend"] != fixture.backend
        or output["method"] != fixture.method
        or output["unit"] != fixture.unit
        or output["workload"] != fixture.workload
        or output["cleanup"] != "complete"
    ):
        raise BenchmarkError("fixture", "network fixture output identity or cleanup drifted")
    observed = _u64(output["value"], "network fixture value")
    after = os.lstat(executable)
    if (
        after.st_dev != before.st_dev
        or after.st_ino != before.st_ino
        or _sha256(executable) != fixture.executable_sha256
    ):
        raise BenchmarkError("fixture", "network fixture executable changed during execution")
    return observed


def default_dependencies() -> CollectionDependencies:
    return CollectionDependencies(
        preflight=preflight,
        prepare_artifacts=prepare_artifacts,
        build_signed_binary=build_signed_binary,
        inspect_environment=inspect_environment,
        collect_workload=collect_workload_sample,
        collect_telemetry=collect_telemetry_sample,
        collect_network=collect_network_sample,
    )


def publish_absent(destination: Path, data: bytes) -> None:
    destination = Path(os.path.abspath(os.fspath(destination)))
    try:
        parent_metadata = os.lstat(destination.parent)
    except OSError as error:
        raise BenchmarkError("publication", "report parent directory is unavailable") from error
    if not stat.S_ISDIR(parent_metadata.st_mode):
        raise BenchmarkError("publication", "report parent is not a directory")
    try:
        os.lstat(destination)
    except FileNotFoundError:
        pass
    except OSError as error:
        raise BenchmarkError("publication", "failed to inspect report destination") from error
    else:
        raise BenchmarkError("collision", "report destination already exists")
    descriptor: Optional[int] = None
    stage: Optional[Path] = None
    linked = False
    try:
        descriptor, raw_stage = tempfile.mkstemp(
            prefix=f".{destination.name}.stage.", dir=destination.parent
        )
        stage = Path(raw_stage)
        os.fchmod(descriptor, PRIVATE_FILE_MODE)
        offset = 0
        while offset < len(data):
            written = os.write(descriptor, data[offset:])
            if written <= 0:
                raise BenchmarkError("publication", "report stage write made no progress")
            offset += written
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = None
        try:
            os.link(stage, destination, follow_symlinks=False)
            linked = True
        except FileExistsError as error:
            raise BenchmarkError("collision", "report destination appeared during publication") from error
        published = os.lstat(destination)
        staged = os.lstat(stage)
        if published.st_dev != staged.st_dev or published.st_ino != staged.st_ino:
            raise BenchmarkError("publication", "published report identity changed")
        directory_fd = os.open(destination.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except BenchmarkError:
        if linked and stage is not None:
            try:
                published = os.lstat(destination)
                staged = os.lstat(stage)
                if published.st_dev == staged.st_dev and published.st_ino == staged.st_ino:
                    os.unlink(destination)
            except FileNotFoundError:
                pass
        raise
    except OSError as error:
        if linked and stage is not None:
            try:
                published = os.lstat(destination)
                staged = os.lstat(stage)
                if published.st_dev == staged.st_dev and published.st_ino == staged.st_ino:
                    os.unlink(destination)
            except FileNotFoundError:
                pass
        raise BenchmarkError("publication", "failed to publish the report") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if stage is not None:
            try:
                os.unlink(stage)
            except FileNotFoundError:
                pass


def collect_report(
    config: BenchmarkConfig,
    output: Path,
    *,
    fixture: Optional[NetworkFixture] = None,
    dependencies: Optional[CollectionDependencies] = None,
) -> dict[str, object]:
    runtime = dependencies if dependencies is not None else default_dependencies()
    if fixture is not None and fixture.timeout_seconds > config.timeouts.network_seconds:
        raise BenchmarkError(
            "fixture", "network fixture timeout exceeds the collection policy"
        )
    report: Optional[dict[str, object]] = None
    with private_session() as root:
        runtime.preflight(config)
        artifacts = runtime.prepare_artifacts(config)
        build = runtime.build_signed_binary(root.path, config)
        environment = runtime.inspect_environment(config, artifacts, build)
        observations = {name: [] for name, _method, _unit in MEASUREMENT_DEFINITIONS}
        network_raw: Optional[list[int]] = [] if fixture is not None else None
        total = config.warmups + config.iterations
        for sample_index in range(total):
            workload = runtime.collect_workload(
                root.path, artifacts, build, config, sample_index
            )
            telemetry = runtime.collect_telemetry(
                root.path, artifacts, build, config, sample_index
            )
            network = (
                runtime.collect_network(fixture, root.path) if fixture is not None else None
            )
            if set(workload) != WORKLOAD_MEASUREMENT_NAMES:
                raise BenchmarkError("collection", "workload sample metric set drifted")
            if set(telemetry) != TELEMETRY_MEASUREMENT_NAMES:
                raise BenchmarkError("collection", "telemetry sample metric set drifted")
            values = {**workload, **telemetry}
            if sample_index >= config.warmups:
                for name, value in values.items():
                    observations[name].append(_u64(value, f"sample {name}"))
                if network_raw is not None and network is not None:
                    network_raw.append(_u64(network, "network sample"))
        report = assemble_report(
            config,
            environment,
            observations,
            fixture=fixture,
            network_raw=network_raw,
        )
    if report is None:  # pragma: no cover - loop construction invariant
        raise BenchmarkError("collection", "report assembly did not complete")
    data = canonical_json(report)
    publish_absent(output, data)
    return report


def parse_args(arguments: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Collect and compare signed Bangbang specification observations.",
        allow_abbrev=False,
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    collect = subparsers.add_parser("collect", allow_abbrev=False)
    collect.add_argument("--config", required=True, type=Path)
    collect.add_argument("--output", required=True, type=Path)
    collect.add_argument("--network-fixture", type=Path)
    validate = subparsers.add_parser("validate", allow_abbrev=False)
    validate.add_argument("--report", required=True, type=Path)
    compare = subparsers.add_parser("compare", allow_abbrev=False)
    compare.add_argument("--previous", required=True, type=Path)
    compare.add_argument("--current", required=True, type=Path)
    return parser.parse_args(arguments)


def main(arguments: Optional[Sequence[str]] = None) -> int:
    args = parse_args(arguments)
    try:
        if args.command == "collect":
            config = read_config(args.config)
            fixture = (
                read_network_fixture(args.network_fixture)
                if args.network_fixture is not None
                else None
            )
            report = collect_report(config, args.output, fixture=fixture)
            print(f"specification benchmark report: {report['comparison_key']}")
        elif args.command == "validate":
            report = read_report(args.report)
            print(f"specification benchmark report is valid: {report['comparison_key']}")
        else:
            previous = read_report(args.previous)
            current = read_report(args.current)
            sys.stdout.buffer.write(canonical_json(comparison_document(previous, current)))
    except BenchmarkError as error:
        print(f"specification benchmark: {error.category}: {error}", file=sys.stderr)
        return 1
    except OSError:
        print("specification benchmark: system: system operation failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
