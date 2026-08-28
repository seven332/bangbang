#!/usr/bin/env python3
"""Import-only no-Apple adapter for the canonical production certification."""

from __future__ import annotations

import dataclasses
import hashlib
import importlib.util
import os
import platform
import re
import signal
import stat
import subprocess
import sys
import time
from pathlib import Path
from types import ModuleType
from typing import Any, Callable, Mapping, NoReturn, Optional, Sequence


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
HANDOFF_PATH = REPOSITORY_ROOT / "scripts/elevated_vmnet_handoff.py"
STAGED_PROTOCOL_PATH = (
    REPOSITORY_ROOT / "scripts/guest/staged_vmnet_certification.py"
)

PACKAGE_NAME = "bangbang-elevated-vmnet-handoff"
CERTIFICATION_DIRECTORY = "certification"
PRIVATE_PLAN_NAME = "run-plan.json"
PAYLOAD_MANIFEST_NAME = "payload-manifest.json"
KERNEL_NAME = "vmlinux-6.1.155"
ROOTFS_NAME = "ubuntu-24.04-512M-direct-boot-v112.ext4"
SIDECAR_NAME = ROOTFS_NAME + ".bangbang.json"
FIXTURE_NAME = "fixture"
PAYLOAD_SCHEMA_VERSION = 1
PLAN_SCHEMA_VERSION = 1
ELEVATED_SESSION_TIMEOUT = 3.0 * 60.0 * 60.0
MAX_PAYLOAD_MANIFEST_BYTES = 64 * 1024
MAX_PRIVATE_PLAN_BYTES = 64 * 1024
MAX_KERNEL_BYTES = 512 * 1024 * 1024
MAX_ROOTFS_BYTES = 1024 * 1024 * 1024
MAX_SIDECAR_BYTES = 1024 * 1024
FIXED_ENVIRONMENT = {"LANG": "C", "LC_ALL": "C"}
CASE_FILE_CLEANUP_CATEGORIES = frozenset(
    {"case-remove-cleanup", "case-root-cleanup", "case-tree-cleanup"}
)
PAYLOAD_ROLES = {
    FIXTURE_NAME: ("fixture", 0o555),
    KERNEL_NAME: ("kernel", 0o444),
    PRIVATE_PLAN_NAME: ("private-plan", 0o444),
    ROOTFS_NAME: ("rootfs", 0o444),
    SIDECAR_NAME: ("rootfs-sidecar", 0o444),
}

_HANDOFF: Optional[ModuleType] = None
_STAGED_PROTOCOL: Optional[ModuleType] = None


def _load_module(name: str, path: Path) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError("module unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def load_handoff() -> ModuleType:
    global _HANDOFF
    if _HANDOFF is None:
        _HANDOFF = _load_module(
            "bangbang_production_vmnet_elevated_handoff", HANDOFF_PATH
        )
    return _HANDOFF


def load_staged_protocol() -> ModuleType:
    global _STAGED_PROTOCOL
    if _STAGED_PROTOCOL is None:
        _STAGED_PROTOCOL = _load_module(
            "bangbang_production_vmnet_staged_protocol", STAGED_PROTOCOL_PATH
        )
    return _STAGED_PROTOCOL


def _fail(vmnet: ModuleType, category: str) -> NoReturn:
    raise vmnet.CertificationError(category)


def _elevated_launcher_arguments(
    vmnet: ModuleType,
    bundle: Path,
    files: Any,
    instance: str,
    allowed: Sequence[str],
    maximum: Optional[int],
) -> tuple[str, ...]:
    arguments = vmnet._launcher_arguments(
        bundle,
        files,
        instance,
        allowed,
        maximum,
    )
    if arguments[-2:] != ("--id", instance):
        _fail(vmnet, "internal")
    return arguments[:-2]


def _sha256(path: Path, maximum: int) -> tuple[int, str]:
    descriptor = -1
    digest = hashlib.sha256()
    size = 0
    try:
        before = os.lstat(path)
        descriptor = os.open(
            path,
            os.O_RDONLY
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0),
        )
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or stat.S_ISLNK(before.st_mode)
            or before.st_dev != opened.st_dev
            or before.st_ino != opened.st_ino
            or opened.st_nlink != 1
            or opened.st_size <= 0
            or opened.st_size > maximum
        ):
            raise OSError("invalid payload")
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > maximum:
                raise OSError("oversized payload")
            digest.update(chunk)
        after = os.fstat(descriptor)
        visible = os.lstat(path)
        if (
            after.st_dev != opened.st_dev
            or after.st_ino != opened.st_ino
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
            or visible.st_dev != opened.st_dev
            or visible.st_ino != opened.st_ino
            or size != opened.st_size
        ):
            raise OSError("payload changed")
        return size, digest.hexdigest()
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _copy_payload(
    source: Path,
    destination: Path,
    *,
    maximum: int,
    executable: bool = False,
) -> tuple[int, str]:
    source_descriptor = destination_descriptor = -1
    source_size = 0
    digest = hashlib.sha256()
    try:
        before = os.lstat(source)
        source_descriptor = os.open(
            source,
            os.O_RDONLY
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0),
        )
        opened = os.fstat(source_descriptor)
        source_mode = stat.S_IMODE(opened.st_mode)
        if (
            not stat.S_ISREG(opened.st_mode)
            or stat.S_ISLNK(before.st_mode)
            or before.st_dev != opened.st_dev
            or before.st_ino != opened.st_ino
            or opened.st_nlink != 1
            or opened.st_uid != os.getuid()
            or opened.st_size <= 0
            or opened.st_size > maximum
            or source_mode & 0o022
            or executable
            and (not source_mode & stat.S_IXUSR or source_mode & 0o7000)
        ):
            raise OSError("invalid source payload")
        destination_descriptor = os.open(
            destination,
            os.O_WRONLY
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0),
            0o700 if executable else 0o600,
        )
        os.fchmod(destination_descriptor, 0o700 if executable else 0o600)
        while True:
            chunk = os.read(source_descriptor, 1024 * 1024)
            if not chunk:
                break
            source_size += len(chunk)
            if source_size > maximum:
                raise OSError("oversized source payload")
            digest.update(chunk)
            view = memoryview(chunk)
            while view:
                written = os.write(destination_descriptor, view)
                if written <= 0:
                    raise OSError("short payload write")
                view = view[written:]
        os.fsync(destination_descriptor)
        after = os.fstat(source_descriptor)
        copied = os.fstat(destination_descriptor)
        if (
            after.st_dev != opened.st_dev
            or after.st_ino != opened.st_ino
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
            or copied.st_size != source_size
            or source_size != opened.st_size
        ):
            raise OSError("payload changed")
        return source_size, digest.hexdigest()
    finally:
        for descriptor in (destination_descriptor, source_descriptor):
            if descriptor >= 0:
                os.close(descriptor)


def _write_payload(path: Path, data: bytes, *, executable: bool = False) -> None:
    descriptor = -1
    try:
        descriptor = os.open(
            path,
            os.O_WRONLY
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0),
            0o700 if executable else 0o600,
        )
        os.fchmod(descriptor, 0o700 if executable else 0o600)
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise OSError("short payload write")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _artifact_path(vmnet: ModuleType, outcome: Any, expected_name: str) -> Path:
    path = vmnet._one_path(outcome, "artifact")
    if path.name != expected_name:
        _fail(vmnet, "artifact")
    return path


@dataclasses.dataclass(frozen=True)
class PreparedInputs:
    kernel: Path
    rootfs: Path
    sidecar: Path
    kernel_identity: Any
    rootfs_identity: Any
    sidecar_identity: Any


def _prepare_inputs(
    vmnet: ModuleType, config: Any
) -> PreparedInputs:
    environment = vmnet._production_environment()
    kernel = _artifact_path(
        vmnet,
        vmnet.run_bounded_command(
            (os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-kernel.sh"),),
            timeout_seconds=config.timeouts.artifact_seconds,
            phase="elevated-artifact-kernel",
            environment=environment,
        ),
        KERNEL_NAME,
    )
    rootfs = _artifact_path(
        vmnet,
        vmnet.run_bounded_command(
            (
                os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),
                "--format",
                "ext4",
                "--ext4-size",
                "512M",
                "--direct-boot-init",
                "--direct-boot-variant",
                "direct-boot-v112",
            ),
            timeout_seconds=config.timeouts.artifact_seconds,
            phase="elevated-artifact-rootfs",
            environment=environment,
        ),
        ROOTFS_NAME,
    )
    sidecar = rootfs.with_name(SIDECAR_NAME)
    kernel_identity = vmnet._verify_regular_artifact(kernel, "kernel")
    rootfs_identity = vmnet._verify_regular_artifact(rootfs, "rootfs")
    sidecar_identity = vmnet._verify_regular_artifact(sidecar, "rootfs-sidecar")
    _validate_sidecar(vmnet, sidecar, rootfs)
    return PreparedInputs(
        kernel,
        rootfs,
        sidecar,
        kernel_identity,
        rootfs_identity,
        sidecar_identity,
    )


def _validate_sidecar(vmnet: ModuleType, sidecar: Path, rootfs: Path) -> None:
    try:
        data = sidecar.read_bytes()
        document = vmnet._decode_document(data)
        metadata = os.lstat(rootfs)
    except BaseException as error:
        if isinstance(error, vmnet.CertificationError):
            raise
        raise vmnet.CertificationError("artifact") from error
    if (
        document.get("variant") != "direct-boot-v112"
        or document.get("output_size_bytes") != metadata.st_size
    ):
        _fail(vmnet, "artifact")


def _result_target(vmnet: ModuleType, path: Path) -> dict[str, object]:
    vmnet._validate_output_target(path)
    try:
        parent = os.lstat(path.parent)
    except OSError as error:
        raise vmnet.CertificationError("output") from error
    name = path.name
    if (
        not stat.S_ISDIR(parent.st_mode)
        or stat.S_ISLNK(parent.st_mode)
        or parent.st_uid != os.getuid()
        or not name
        or name in (".", "..")
        or "/" in name
        or len(os.fsencode(name)) > 255
    ):
        _fail(vmnet, "output")
    return {
        "device": parent.st_dev,
        "group": parent.st_gid,
        "inode": parent.st_ino,
        "name": name,
        "parent": os.fspath(path.parent),
        "uid": parent.st_uid,
    }


def _optional_document(config: Any) -> dict[str, object]:
    return {
        "bridged_interface": config.optional_cases.bridged_interface,
        "host_connectivity": config.optional_cases.host_connectivity,
        "not_authorized": config.optional_cases.not_authorized,
        "sharing_service_busy": config.optional_cases.sharing_service_busy,
    }


def _timeout_document(config: Any) -> dict[str, int]:
    return dataclasses.asdict(config.timeouts)


def _platform_document(host: Any) -> dict[str, str]:
    return {
        "architecture": host.architecture,
        "hvf": host.hvf,
        "macos": host.macos,
        "sdk": host.sdk,
    }


def _payload_record(
    path: Path,
    name: str,
    role: str,
    current_mode: int,
    sealed_mode: int,
    maximum: int,
) -> dict[str, object]:
    size, digest = _sha256(path, maximum)
    metadata = os.lstat(path)
    if (
        metadata.st_uid != os.getuid()
        or metadata.st_gid != os.getgid()
        or stat.S_IMODE(metadata.st_mode) != current_mode
    ):
        raise OSError("payload ownership")
    return {
        "mode": sealed_mode,
        "name": name,
        "role": role,
        "sha256": digest,
        "size_bytes": size,
    }


def prepare_elevated(
    vmnet: ModuleType,
    config_path: Path,
    result_path: Path,
    output: Path,
) -> None:
    config = vmnet.read_config(config_path)
    if not isinstance(config, vmnet.ElevatedCertificationConfig):
        _fail(vmnet, "config")
    if output.name != PACKAGE_NAME:
        _fail(vmnet, "output")
    result = _result_target(vmnet, result_path)
    source, host = vmnet._default_preflight()
    inputs = _prepare_inputs(vmnet, config)
    handoff = load_handoff()

    def populate(stage: Path, package_source: Any) -> None:
        if (
            package_source.commit != source.commit
            or package_source.tree != source.tree
        ):
            _fail(vmnet, "source")
        vmnet._recheck_config_inputs_elevated(config)
        vmnet._recheck_artifact(inputs.kernel, inputs.kernel_identity)
        vmnet._recheck_artifact(inputs.rootfs, inputs.rootfs_identity)
        vmnet._recheck_artifact(inputs.sidecar, inputs.sidecar_identity)
        _result_target(vmnet, result_path)
        directory = stage / CERTIFICATION_DIRECTORY
        directory.mkdir(mode=0o700)
        copied: list[tuple[str, int]] = []
        for source_path, name, maximum, executable in (
            (config.fixture.executable, FIXTURE_NAME, vmnet.MAX_FIXTURE_BYTES, True),
            (inputs.kernel, KERNEL_NAME, MAX_KERNEL_BYTES, False),
            (inputs.rootfs, ROOTFS_NAME, MAX_ROOTFS_BYTES, False),
            (inputs.sidecar, SIDECAR_NAME, MAX_SIDECAR_BYTES, False),
        ):
            _copy_payload(
                source_path,
                directory / name,
                maximum=maximum,
                executable=executable,
            )
            copied.append((name, maximum))
        plan = {
            "authority": {"kind": "elevated-provider"},
            "fixture": {
                "name": FIXTURE_NAME,
                "sha256": config.fixture.expected_sha256,
            },
            "optional_cases": _optional_document(config),
            "platform": _platform_document(host),
            "result": result,
            "schema_version": PLAN_SCHEMA_VERSION,
            "source": {"commit": source.commit, "tree": source.tree},
            "timeouts": _timeout_document(config),
        }
        plan_data = vmnet.canonical_json(plan)
        if len(plan_data) > MAX_PRIVATE_PLAN_BYTES:
            _fail(vmnet, "package")
        _write_payload(directory / PRIVATE_PLAN_NAME, plan_data)
        copied.append((PRIVATE_PLAN_NAME, MAX_PRIVATE_PLAN_BYTES))
        payloads = []
        for name, maximum in sorted(copied):
            role, sealed_mode = PAYLOAD_ROLES[name]
            current_mode = 0o700 if sealed_mode == 0o555 else 0o600
            payloads.append(
                _payload_record(
                    directory / name,
                    name,
                    role,
                    current_mode,
                    sealed_mode,
                    maximum,
                )
            )
        manifest = {
            "kind": "bangbang-production-vmnet-elevated-payload",
            "payloads": payloads,
            "schema_version": PAYLOAD_SCHEMA_VERSION,
            "source": {"commit": source.commit, "tree": source.tree},
        }
        manifest_data = vmnet.canonical_json(manifest)
        if len(manifest_data) > MAX_PAYLOAD_MANIFEST_BYTES:
            _fail(vmnet, "package")
        _write_payload(directory / PAYLOAD_MANIFEST_NAME, manifest_data)
        vmnet._recheck_config_inputs_elevated(config)
        vmnet._recheck_artifact(inputs.kernel, inputs.kernel_identity)
        vmnet._recheck_artifact(inputs.rootfs, inputs.rootfs_identity)
        vmnet._recheck_artifact(inputs.sidecar, inputs.sidecar_identity)
        _result_target(vmnet, result_path)

    try:
        handoff.prepare_package(output, populate=populate)
    except handoff.HandoffError as error:
        raise vmnet.CertificationError(_handoff_category(error.category)) from error
    _result_target(vmnet, result_path)


def _handoff_category(category: str) -> str:
    direct = {
        "build": "bundle",
        "implementation": "source",
        "invocation": "invocation",
        "manifest": "package",
        "package": "package",
        "platform": "platform",
        "publication": "output",
        "source": "source",
    }.get(category)
    if direct is not None:
        return direct
    prefix = "supervisor-controller-certification-"
    if category.startswith(prefix):
        certification = category[len(prefix) :]
        if certification in load_handoff().CERTIFICATION_FAILURES:
            return certification
    return "handoff"


def _platform_identity(vmnet: ModuleType) -> Any:
    if (
        sys.platform != "darwin"
        or platform.machine() != "arm64"
        or os.getuid() == 0
        or os.geteuid() == 0
    ):
        _fail(vmnet, "platform")
    macos = vmnet._closed_tool_line(
        vmnet.run_bounded_command(
            ("/usr/bin/sw_vers", "-productVersion"),
            timeout_seconds=10,
            phase="elevated-platform-macos",
            environment=FIXED_ENVIRONMENT,
        ),
        "platform",
    )
    sdk = vmnet._closed_tool_line(
        vmnet.run_bounded_command(
            ("/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-version"),
            timeout_seconds=10,
            phase="elevated-platform-sdk",
            environment=FIXED_ENVIRONMENT,
        ),
        "platform",
    )
    hvf = vmnet.run_bounded_command(
        ("/usr/sbin/sysctl", "-n", "kern.hv_support"),
        timeout_seconds=10,
        phase="elevated-platform-hvf",
        environment=FIXED_ENVIRONMENT,
    )
    if hvf.returncode != 0 or hvf.stderr or hvf.stdout.strip() != b"1":
        _fail(vmnet, "platform")
    return vmnet.PlatformIdentity(macos, sdk)


def _root_file_identity(vmnet: ModuleType, path: Path, maximum: int) -> Any:
    descriptor, identity = vmnet._open_regular(
        path,
        category="package",
        maximum=maximum,
        private=False,
        digest=True,
        owner_uid=0,
    )
    os.close(descriptor)
    return identity


def _read_root_document(
    vmnet: ModuleType, path: Path, maximum: int
) -> dict[str, Any]:
    identity = _root_file_identity(vmnet, path, maximum)
    try:
        data = path.read_bytes()
    except OSError as error:
        raise vmnet.CertificationError("package") from error
    if len(data) != identity.size:
        _fail(vmnet, "package")
    return vmnet._decode_document(data)


@dataclasses.dataclass(frozen=True)
class LoadedPackage:
    config: Any
    result: Path
    source: Any
    host: Any
    artifacts: Any
    package_seal: tuple["PackageEntrySeal", ...]
    result_seal: "ResultTargetSeal"


@dataclasses.dataclass(frozen=True)
class PackageEntrySeal:
    name: str
    device: int
    inode: int
    mode: int
    uid: int
    gid: int
    size: int
    mtime_ns: int
    ctime_ns: int


@dataclasses.dataclass(frozen=True)
class ResultTargetSeal:
    parent: Path
    name: str
    device: int
    inode: int
    uid: int
    gid: int


def _package_seal(
    vmnet: ModuleType, handoff: ModuleType, package: Path
) -> tuple[PackageEntrySeal, ...]:
    try:
        root_metadata = os.lstat(package)
        entries = [(package, root_metadata), *handoff._iter_plain_tree(package)]
    except BaseException as error:
        if isinstance(error, handoff.HandoffError):
            raise vmnet.CertificationError(_handoff_category(error.category)) from error
        raise vmnet.CertificationError("package") from error
    sealed: list[PackageEntrySeal] = []
    for path, metadata in entries:
        try:
            name = "." if path == package else path.relative_to(package).as_posix()
        except ValueError as error:
            raise vmnet.CertificationError("package") from error
        mode = stat.S_IMODE(metadata.st_mode)
        if (
            metadata.st_uid != 0
            or metadata.st_gid != 0
            or stat.S_ISDIR(metadata.st_mode)
            and mode != 0o555
            or stat.S_ISREG(metadata.st_mode)
            and (mode not in (0o444, 0o555) or metadata.st_nlink != 1)
            or not stat.S_ISDIR(metadata.st_mode)
            and not stat.S_ISREG(metadata.st_mode)
        ):
            _fail(vmnet, "package")
        sealed.append(
            PackageEntrySeal(
                name,
                metadata.st_dev,
                metadata.st_ino,
                mode,
                metadata.st_uid,
                metadata.st_gid,
                metadata.st_size,
                metadata.st_mtime_ns,
                metadata.st_ctime_ns,
            )
        )
    return tuple(sealed)


def _result_target_seal(
    vmnet: ModuleType,
    value: Mapping[str, object],
    uid: int,
    gid: int,
) -> ResultTargetSeal:
    numeric = ("device", "group", "inode", "uid")
    if any(type(value.get(key)) is not int or int(value[key]) < 0 for key in numeric):
        _fail(vmnet, "output")
    parent_value = value.get("parent")
    name_value = value.get("name")
    if (
        not isinstance(parent_value, str)
        or not isinstance(name_value, str)
        or value["uid"] != uid
        or value["group"] != gid
    ):
        _fail(vmnet, "output")
    parent = Path(parent_value)
    result = parent / name_value
    try:
        metadata = os.lstat(parent)
    except OSError as error:
        raise vmnet.CertificationError("output") from error
    if (
        not parent.is_absolute()
        or not result.is_absolute()
        or result.parent != parent
        or result.name != name_value
        or not name_value
        or name_value in (".", "..")
        or "/" in name_value
        or not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_dev != value["device"]
        or metadata.st_ino != value["inode"]
        or metadata.st_uid != uid
        or metadata.st_gid != gid
        or os.path.lexists(result)
    ):
        _fail(vmnet, "output")
    return ResultTargetSeal(
        parent,
        name_value,
        metadata.st_dev,
        metadata.st_ino,
        uid,
        gid,
    )


def _recheck_result_target(vmnet: ModuleType, seal: ResultTargetSeal) -> None:
    result = seal.parent / seal.name
    try:
        metadata = os.lstat(seal.parent)
    except OSError as error:
        raise vmnet.CertificationError("output") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_dev != seal.device
        or metadata.st_ino != seal.inode
        or metadata.st_uid != seal.uid
        or metadata.st_gid != seal.gid
        or os.path.lexists(result)
    ):
        _fail(vmnet, "output")


def recheck_loaded_package(
    vmnet: ModuleType,
    package: Path,
    loaded: LoadedPackage,
) -> None:
    """Recheck an immutable root-owned package without rehashing its large images."""

    handoff = load_handoff()
    if not isinstance(loaded, LoadedPackage):
        _fail(vmnet, "internal")
    if _package_seal(vmnet, handoff, package) != loaded.package_seal:
        _fail(vmnet, "package")
    vmnet._recheck_config_inputs_elevated(loaded.config)
    vmnet._recheck_artifact(
        loaded.artifacts.kernel,
        loaded.artifacts.kernel_identity,
        owner_uid=loaded.artifacts.owner_uid,
    )
    vmnet._recheck_artifact(
        loaded.artifacts.rootfs,
        loaded.artifacts.rootfs_identity,
        owner_uid=loaded.artifacts.owner_uid,
    )
    _recheck_result_target(vmnet, loaded.result_seal)


def _closed_object(
    vmnet: ModuleType,
    value: object,
    keys: Sequence[str],
    category: str,
) -> dict[str, Any]:
    return vmnet._object(value, keys, category)


def load_package(
    vmnet: ModuleType, package: Path, uid: int, gid: int
) -> LoadedPackage:
    handoff = load_handoff()
    try:
        package_source = handoff.verify_package(
            package, 0, 0, root_mode=0o555
        )
    except handoff.HandoffError as error:
        raise vmnet.CertificationError(_handoff_category(error.category)) from error
    directory = package / CERTIFICATION_DIRECTORY
    try:
        directory_metadata = os.lstat(directory)
    except OSError as error:
        raise vmnet.CertificationError("package") from error
    if (
        not stat.S_ISDIR(directory_metadata.st_mode)
        or stat.S_ISLNK(directory_metadata.st_mode)
        or directory_metadata.st_uid != 0
        or directory_metadata.st_gid != 0
        or stat.S_IMODE(directory_metadata.st_mode) != 0o555
    ):
        _fail(vmnet, "package")
    payload_manifest = _read_root_document(
        vmnet, directory / PAYLOAD_MANIFEST_NAME, MAX_PAYLOAD_MANIFEST_BYTES
    )
    manifest_root = _closed_object(
        vmnet,
        payload_manifest,
        ("kind", "payloads", "schema_version", "source"),
        "package",
    )
    if (
        manifest_root["kind"] != "bangbang-production-vmnet-elevated-payload"
        or manifest_root["schema_version"] != PAYLOAD_SCHEMA_VERSION
    ):
        _fail(vmnet, "package")
    source_value = _closed_object(
        vmnet, manifest_root["source"], ("commit", "tree"), "package"
    )
    if source_value != {
        "commit": package_source.commit,
        "tree": package_source.tree,
    }:
        _fail(vmnet, "source")
    payload_values = manifest_root["payloads"]
    if not isinstance(payload_values, list) or len(payload_values) != len(PAYLOAD_ROLES):
        _fail(vmnet, "package")
    observed: dict[str, dict[str, Any]] = {}
    for value in payload_values:
        record = _closed_object(
            vmnet,
            value,
            ("mode", "name", "role", "sha256", "size_bytes"),
            "package",
        )
        name = record["name"]
        if not isinstance(name, str) or name in observed or name not in PAYLOAD_ROLES:
            _fail(vmnet, "package")
        role, expected_mode = PAYLOAD_ROLES[name]
        path = directory / name
        maximum = {
            FIXTURE_NAME: vmnet.MAX_FIXTURE_BYTES,
            KERNEL_NAME: MAX_KERNEL_BYTES,
            PRIVATE_PLAN_NAME: MAX_PRIVATE_PLAN_BYTES,
            ROOTFS_NAME: MAX_ROOTFS_BYTES,
            SIDECAR_NAME: MAX_SIDECAR_BYTES,
        }[name]
        identity = _root_file_identity(vmnet, path, maximum)
        metadata = os.lstat(path)
        if (
            record["role"] != role
            or record["mode"] != expected_mode
            or stat.S_IMODE(metadata.st_mode) != expected_mode
            or record["size_bytes"] != identity.size
            or record["sha256"] != identity.sha256
        ):
            _fail(vmnet, "package")
        observed[name] = record
    if set(observed) != set(PAYLOAD_ROLES):
        _fail(vmnet, "package")

    plan = _read_root_document(
        vmnet, directory / PRIVATE_PLAN_NAME, MAX_PRIVATE_PLAN_BYTES
    )
    plan_root = _closed_object(
        vmnet,
        plan,
        (
            "authority",
            "fixture",
            "optional_cases",
            "platform",
            "result",
            "schema_version",
            "source",
            "timeouts",
        ),
        "package",
    )
    if plan_root["schema_version"] != PLAN_SCHEMA_VERSION:
        _fail(vmnet, "package")
    authority = _closed_object(
        vmnet, plan_root["authority"], ("kind",), "package"
    )
    if authority["kind"] != "elevated-provider":
        _fail(vmnet, "authority")
    plan_source = _closed_object(
        vmnet, plan_root["source"], ("commit", "tree"), "package"
    )
    if plan_source != source_value:
        _fail(vmnet, "source")
    host = _platform_identity(vmnet)
    if plan_root["platform"] != _platform_document(host):
        _fail(vmnet, "platform")
    fixture_value = _closed_object(
        vmnet, plan_root["fixture"], ("name", "sha256"), "package"
    )
    if (
        fixture_value["name"] != FIXTURE_NAME
        or fixture_value["sha256"] != observed[FIXTURE_NAME]["sha256"]
    ):
        _fail(vmnet, "fixture")
    fixture_path = directory / FIXTURE_NAME
    fixture_identity = _root_file_identity(
        vmnet, fixture_path, vmnet.MAX_FIXTURE_BYTES
    )
    config_document = {
        "authority": {"kind": "elevated-provider"},
        "fixture": {
            "executable": os.fspath(fixture_path),
            "sha256": fixture_value["sha256"],
        },
        "optional_cases": plan_root["optional_cases"],
        "schema_version": vmnet.ELEVATED_SCHEMA_VERSION,
        "timeouts": plan_root["timeouts"],
    }
    config = vmnet._parse_elevated_config_document_staged(
        config_document, fixture_identity
    )
    result_value = _closed_object(
        vmnet,
        plan_root["result"],
        ("device", "group", "inode", "name", "parent", "uid"),
        "output",
    )
    result_seal = _result_target_seal(vmnet, result_value, uid, gid)
    result = result_seal.parent / result_seal.name
    kernel_path = directory / KERNEL_NAME
    rootfs_path = directory / ROOTFS_NAME
    artifacts = vmnet.PreparedArtifacts(
        kernel_path,
        rootfs_path,
        _root_file_identity(vmnet, kernel_path, MAX_KERNEL_BYTES),
        _root_file_identity(vmnet, rootfs_path, MAX_ROOTFS_BYTES),
        owner_uid=0,
    )
    _validate_sidecar(vmnet, directory / SIDECAR_NAME, rootfs_path)
    loaded = LoadedPackage(
        config,
        result,
        vmnet.SourceIdentity(package_source.commit, package_source.tree),
        host,
        artifacts,
        _package_seal(vmnet, handoff, package),
        result_seal,
    )
    recheck_loaded_package(vmnet, package, loaded)
    return loaded


@dataclasses.dataclass(frozen=True)
class ProductRoles:
    provider: int
    outer: int
    worker: int
    owners: tuple[int, ...]


STAGED_TRAFFIC_ID = "cert-staged-traffic"
STAGED_BARRIER_ID = "cert-staged-barrier"
SNAPSHOT_STATE_OUTPUT_ID = "cert-snapshot-state-output"
SNAPSHOT_MEMORY_OUTPUT_ID = "cert-snapshot-memory-output"
SNAPSHOT_STATE_INPUT_ID = "cert-snapshot-state-input"
SNAPSHOT_MEMORY_INPUT_ID = "cert-snapshot-memory-input"
STAGED_TRAFFIC_REF = f"bangbang-grant:{STAGED_TRAFFIC_ID}"
STAGED_BARRIER_REF = f"bangbang-grant:{STAGED_BARRIER_ID}"
SNAPSHOT_STATE_OUTPUT_REF = (
    f"bangbang-grant:{SNAPSHOT_STATE_OUTPUT_ID}/state.snap"
)
SNAPSHOT_MEMORY_OUTPUT_REF = (
    f"bangbang-grant:{SNAPSHOT_MEMORY_OUTPUT_ID}/memory.snap"
)
SNAPSHOT_STATE_INPUT_REF = f"bangbang-grant:{SNAPSHOT_STATE_INPUT_ID}"
SNAPSHOT_MEMORY_INPUT_REF = f"bangbang-grant:{SNAPSHOT_MEMORY_INPUT_ID}"
STAGED_BOOT_ARGS = (
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 "
    "init=/bangbang-direct-rootfs-init bangbang.staged-vmnet-certification=1"
)
ELEVATED_VMNET_BOOT_ARGS = (
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 "
    "init=/bangbang-direct-rootfs-init bangbang.elevated-vmnet-certification=1"
)
ELEVATED_GUEST_BEGIN_MARKER = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_BEGIN\n"
ELEVATED_GUEST_SUCCESS_MARKER = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_OK\n"
ELEVATED_GUEST_FAILURE_PREFIX = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_"
STAGED_TRAFFIC_MAGIC = b"BBEVNET2"
STAGED_TRAFFIC_VERSION = 2
STAGED_TRAFFIC_MODE_SHARED = 1
STAGED_TRAFFIC_DHCP_ROUTER_ENDPOINT = 1


def _staged_traffic_control(vmnet: ModuleType, port: int, nonce: bytes) -> bytes:
    if (
        isinstance(port, bool)
        or not isinstance(port, int)
        or not 1 <= port <= 65535
        or not isinstance(nonce, bytes)
        or len(nonce) != 32
        or not any(nonce)
    ):
        _fail(vmnet, "control")
    value = bytearray(512)
    value[:8] = STAGED_TRAFFIC_MAGIC
    value[8:10] = STAGED_TRAFFIC_VERSION.to_bytes(2, "big")
    value[10] = STAGED_TRAFFIC_MODE_SHARED
    value[11] = STAGED_TRAFFIC_DHCP_ROUTER_ENDPOINT
    value[16:18] = port.to_bytes(2, "big")
    value[18:50] = nonce
    value[64:96] = hashlib.sha256(value[:64]).digest()
    return bytes(value)


def _wait_router_oracle(
    vmnet: ModuleType,
    files: Any,
    timeout_seconds: float,
) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        data = vmnet._read_identity_bounded(
            files.serial,
            files.serial_identity,
            maximum=vmnet.MAX_SERIAL_BYTES,
            category="guest",
        )
        lines = data.splitlines(keepends=True)
        if any(line.startswith(ELEVATED_GUEST_FAILURE_PREFIX) for line in lines):
            _fail(vmnet, "guest")
        if (
            ELEVATED_GUEST_BEGIN_MARKER in lines
            and ELEVATED_GUEST_SUCCESS_MARKER in lines
        ):
            return
        if time.monotonic() >= deadline:
            _fail(vmnet, "guest-timeout")
        time.sleep(vmnet.POLL_SECONDS)


@dataclasses.dataclass(frozen=True)
class StagedCaseFiles:
    root: Path
    manifest: Path
    api_directory: Path
    api_socket: Path
    serial: Path
    serial_identity: Any
    control: Path
    control_identity: Any
    barrier: Path
    state_directory: Optional[Path]
    memory_directory: Optional[Path]


def _create_sized_private_file(
    vmnet: ModuleType, path: Path, data: bytes, size: int
) -> Any:
    if not data or len(data) > size or size > MAX_PRIVATE_PLAN_BYTES:
        _fail(vmnet, "control")
    descriptor = -1
    try:
        descriptor = os.open(
            path,
            os.O_RDWR
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        os.fchmod(descriptor, 0o600)
        if os.write(descriptor, data) != len(data):
            _fail(vmnet, "control")
        os.ftruncate(descriptor, size)
        os.fsync(descriptor)
        metadata = os.fstat(descriptor)
        visible = os.lstat(path)
    except vmnet.CertificationError:
        raise
    except OSError as error:
        raise vmnet.CertificationError("control") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size != size
        or visible.st_dev != metadata.st_dev
        or visible.st_ino != metadata.st_ino
    ):
        _fail(vmnet, "control")
    return vmnet.FileIdentity(
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        mtime_ns=metadata.st_mtime_ns,
        ctime_ns=metadata.st_ctime_ns,
    )


class StagedBarrier:
    def __init__(
        self,
        vmnet: ModuleType,
        path: Path,
        scenario: Any,
        nonce: bytes,
        *,
        create: bool,
    ) -> None:
        self.vmnet = vmnet
        self.protocol = load_staged_protocol()
        self.path = path
        self.scenario = scenario
        self.nonce = nonce
        self.previous_status: Optional[Any] = None
        self.previous_command_sequence = 0
        self.terminal = False
        if create:
            _create_sized_private_file(
                vmnet,
                path,
                self.protocol.encode_header(scenario, nonce),
                self.protocol.CONTROL_BYTES,
            )

    def command(self, sequence: int) -> None:
        protocol = self.protocol
        if (
            self.terminal
            or sequence != self.previous_command_sequence + 1
            or sequence > protocol.COMMAND_COUNTS[self.scenario]
        ):
            _fail(self.vmnet, "control")
        value = protocol.encode_record(
            protocol.ROLE_COMMAND,
            self.scenario,
            protocol.COMMAND_PROCEED,
            sequence,
            self.nonce,
        )
        descriptor = -1
        try:
            descriptor = os.open(
                self.path,
                os.O_RDWR
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0),
            )
            metadata = os.fstat(descriptor)
            current = protocol.decode_record(
                os.pread(
                    descriptor,
                    protocol.SECTOR_BYTES,
                    protocol.COMMAND_OFFSET,
                ),
                allow_empty=True,
            )
            expected = None
            if self.previous_command_sequence:
                expected = protocol.Record(
                    protocol.ROLE_COMMAND,
                    self.scenario,
                    protocol.COMMAND_PROCEED,
                    self.previous_command_sequence,
                    self.nonce,
                )
            if (
                not stat.S_ISREG(metadata.st_mode)
                or metadata.st_nlink != 1
                or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != 0o600
                or metadata.st_size != protocol.CONTROL_BYTES
                or current != expected
            ):
                _fail(self.vmnet, "control")
            if os.pwrite(descriptor, value, protocol.COMMAND_OFFSET) != len(value):
                _fail(self.vmnet, "control")
            os.fsync(descriptor)
        except self.vmnet.CertificationError:
            raise
        except BaseException as error:
            raise self.vmnet.CertificationError("control") from error
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        self.previous_command_sequence = sequence

    def wait(self, process: Any, sequence: int, kind: Any) -> None:
        protocol = self.protocol
        expected_sequence = (
            1 if self.previous_status is None else self.previous_status.sequence + 1
        )
        graph = protocol.STATUS_GRAPHS[self.scenario]
        if (
            self.terminal
            or not isinstance(kind, protocol.Status)
            or sequence != expected_sequence
            or sequence > len(graph)
            or kind is not graph[sequence - 1]
        ):
            _fail(self.vmnet, "control")
        deadline = time.monotonic() + process.config.timeouts.guest_seconds
        while True:
            process.raise_if_failed()
            descriptor = -1
            try:
                descriptor = os.open(
                    self.path,
                    os.O_RDONLY
                    | getattr(os, "O_CLOEXEC", 0)
                    | getattr(os, "O_NOFOLLOW", 0),
                )
                record = protocol.decode_record(
                    os.pread(
                        descriptor,
                        protocol.SECTOR_BYTES,
                        protocol.STATUS_OFFSET,
                    ),
                    allow_empty=True,
                )
            except BaseException as error:
                if isinstance(error, self.vmnet.CertificationError):
                    raise
                raise self.vmnet.CertificationError("control") from error
            finally:
                if descriptor >= 0:
                    os.close(descriptor)
            if record is not None:
                if record == self.previous_status:
                    pass
                elif (
                    record.role == protocol.ROLE_STATUS
                    and record.scenario is self.scenario
                    and record.nonce == self.nonce
                    and record.kind in protocol.FAILURE_CATEGORIES
                    and record.sequence == 0xFFFF_FFFF_FFFF_FFFF
                ):
                    _fail(self.vmnet, "guest")
                elif (
                    record.role == protocol.ROLE_STATUS
                    and record.scenario is self.scenario
                    and record.kind == int(kind)
                    and record.sequence == sequence
                    and record.nonce == self.nonce
                ):
                    self.previous_status = record
                    self.terminal = kind is protocol.Status.COMPLETE
                    return
                else:
                    _fail(self.vmnet, "control")
            if time.monotonic() >= deadline:
                _fail(self.vmnet, "guest-timeout")
            time.sleep(self.vmnet.POLL_SECONDS)

    def assert_terminal(self) -> None:
        protocol = self.protocol
        if (
            not self.terminal
            or self.previous_status is None
            or self.previous_status.kind != int(protocol.Status.COMPLETE)
            or self.previous_status.sequence
            != len(protocol.STATUS_GRAPHS[self.scenario])
            or self.previous_command_sequence
            != protocol.COMMAND_COUNTS[self.scenario]
        ):
            _fail(self.vmnet, "control")
        descriptor = -1
        try:
            descriptor = os.open(
                self.path,
                os.O_RDONLY
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0),
            )
            metadata = os.fstat(descriptor)
            value = os.pread(descriptor, protocol.CONTROL_BYTES + 1, 0)
        except BaseException as error:
            if isinstance(error, self.vmnet.CertificationError):
                raise
            raise self.vmnet.CertificationError("control") from error
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_size != protocol.CONTROL_BYTES
            or len(value) != protocol.CONTROL_BYTES
            or any(value[protocol.STATUS_OFFSET + protocol.SECTOR_BYTES :])
        ):
            _fail(self.vmnet, "control")
        try:
            header = protocol.decode_header(value[: protocol.SECTOR_BYTES])
            command = protocol.decode_record(
                value[
                    protocol.COMMAND_OFFSET : protocol.COMMAND_OFFSET
                    + protocol.SECTOR_BYTES
                ]
            )
            status_record = protocol.decode_record(
                value[
                    protocol.STATUS_OFFSET : protocol.STATUS_OFFSET
                    + protocol.SECTOR_BYTES
                ]
            )
        except BaseException as error:
            raise self.vmnet.CertificationError("control") from error
        expected_command = protocol.Record(
            protocol.ROLE_COMMAND,
            self.scenario,
            protocol.COMMAND_PROCEED,
            self.previous_command_sequence,
            self.nonce,
        )
        if (
            header != protocol.Header(self.scenario, self.scenario.cycles, self.nonce)
            or command != expected_command
            or status_record != self.previous_status
        ):
            _fail(self.vmnet, "control")


def _map_handoff_error(vmnet: ModuleType, error: BaseException) -> Any:
    handoff = load_handoff()
    if not isinstance(error, handoff.HandoffError):
        return vmnet.CertificationError("internal")
    category = {
        "arguments": "process",
        "cleanup": "process-cleanup",
        "descriptor": "process",
        "output": "process-output",
        "protocol": "process",
        "signal": "process",
        "timeout": "process-timeout",
    }.get(error.category, "process")
    return vmnet.CertificationError(category)


def _signalable(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return False
    except ProcessLookupError:
        return False
    except OSError:
        return False


class RemoteProductionProcess:
    def __init__(
        self,
        driver: "ElevatedSystemCertificationDriver",
        arguments: Sequence[str],
        files: Any,
    ) -> None:
        self.driver = driver
        self.vmnet = driver.vmnet
        self.handoff = driver.handoff
        self.files = files
        self.config = driver.config
        self.baseline_sessions = driver.session_entries()
        self._api_identity: Optional[Any] = None
        self._roles: Optional[ProductRoles] = None
        self._closed = False
        self._retain_files = False
        try:
            self.process = driver.proxy.spawn(arguments)
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error

    @property
    def pid(self) -> int:
        return self.process.pid

    def _captures(self) -> tuple[bytes, bytes]:
        try:
            return self.process.snapshot_output()
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error

    def raise_if_failed(self) -> None:
        self._captures()
        try:
            status = self.process.poll()
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error
        if status is not None:
            raise self.vmnet.CertificationError("process")

    def _observe_roles(
        self, *, require_owner: bool, forbid_owner: bool
    ) -> Optional[ProductRoles]:
        try:
            records = self.handoff._process_table()
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error
        provider = records.get(self.pid)
        if (
            provider is None
            or provider.state.startswith("Z")
            or provider.command != os.fspath(self.driver.layout.provider)
            or _signalable(self.pid)
        ):
            return None
        outers = [
            record.pid
            for record in records.values()
            if record.parent_pid == self.pid
            and record.command == os.fspath(self.driver.layout.launcher)
            and not record.state.startswith("Z")
        ]
        if len(outers) != 1 or not _signalable(outers[0]):
            return None
        workers = [
            record.pid
            for record in records.values()
            if record.parent_pid == outers[0]
            and record.command == os.fspath(self.driver.layout.worker)
            and not record.state.startswith("Z")
        ]
        if len(workers) != 1 or not _signalable(workers[0]):
            return None
        owners = tuple(
            sorted(
                record.pid
                for record in records.values()
                if record.parent_pid == self.pid
                and record.command == os.fspath(self.driver.layout.provider)
                and not record.state.startswith("Z")
                and _signalable(record.pid)
            )
        )
        if require_owner and len(owners) != 1:
            return None
        if forbid_owner and owners:
            return None
        return ProductRoles(self.pid, outers[0], workers[0], owners)

    def roles(
        self, *, require_owner: bool = False, forbid_owner: bool = False
    ) -> ProductRoles:
        deadline = time.monotonic() + self.config.timeouts.startup_seconds
        while True:
            self.raise_if_failed()
            roles = self._observe_roles(
                require_owner=require_owner, forbid_owner=forbid_owner
            )
            if roles is not None:
                self._roles = roles
                return roles
            if time.monotonic() >= deadline:
                raise self.vmnet.CertificationError("process-timeout")
            time.sleep(self.vmnet.POLL_SECONDS)

    def wait_ready(self) -> None:
        deadline = time.monotonic() + self.config.timeouts.startup_seconds
        while True:
            stdout, _stderr = self._captures()
            if self.vmnet.API_READY_MARKER in stdout:
                roles = self.roles(require_owner=False, forbid_owner=True)
                self._api_identity = self.vmnet._wait_api_socket(
                    self.files.api_socket,
                    self,
                    self.config.timeouts.request_seconds,
                )
                self._roles = roles
                return
            try:
                status = self.process.poll()
            except BaseException as error:
                raise _map_handoff_error(self.vmnet, error) from error
            if status is not None:
                raise self.vmnet.CertificationError("process")
            if time.monotonic() >= deadline:
                raise self.vmnet.CertificationError("process-timeout")
            time.sleep(self.vmnet.POLL_SECONDS)

    def worker_pid(self) -> int:
        return self.roles(require_owner=False).worker

    def owner_pid(self) -> int:
        return self.roles(require_owner=True).owners[0]

    def api_authority(self) -> tuple[Any, int]:
        self.raise_if_failed()
        if self._api_identity is None:
            raise self.vmnet.CertificationError("socket")
        return self._api_identity, self.worker_pid()

    def _signal_ordinary(self, pid: int, number: int) -> None:
        if pid <= 1 or not _signalable(pid):
            raise self.vmnet.CertificationError("process")
        try:
            os.kill(pid, number)
        except OSError as error:
            raise self.vmnet.CertificationError("process") from error

    def signal_role(self, role: str, number: int) -> int:
        if number not in (signal.SIGTERM, signal.SIGKILL):
            raise self.vmnet.CertificationError("internal")
        if role in ("provider", "broker"):
            try:
                if number == signal.SIGTERM:
                    self.process.terminate()
                else:
                    self.process.kill()
            except BaseException as error:
                raise _map_handoff_error(self.vmnet, error) from error
            return self.pid
        roles = self.roles(require_owner=role == "owner")
        pid = {
            "owner": roles.owners[0] if roles.owners else 0,
            "outer": roles.outer,
            "worker": roles.worker,
        }.get(role, 0)
        if pid <= 1:
            raise self.vmnet.CertificationError("internal")
        self._signal_ordinary(pid, number)
        return pid

    def _wait_remote(self) -> int:
        status, _stdout, _stderr = self.wait_output()
        return status

    def wait_output(self) -> tuple[int, bytes, bytes]:
        try:
            status = self.process.wait(self.config.timeouts.terminate_seconds)
            stdout, stderr = self.process.finish_output()
            return status, stdout, stderr
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error

    def finish_exited(self) -> None:
        try:
            if self.process.poll() is None:
                raise self.vmnet.CertificationError("process")
        except BaseException as error:
            if isinstance(error, self.vmnet.CertificationError):
                raise
            raise _map_handoff_error(self.vmnet, error) from error
        self._finish()

    def _remove_stale_socket(self) -> None:
        path = self.files.api_socket
        try:
            metadata = os.lstat(path)
        except FileNotFoundError:
            return
        except OSError as error:
            raise self.vmnet.CertificationError("process-cleanup") from error
        identity = self._api_identity
        if (
            identity is None
            or not stat.S_ISSOCK(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_dev != identity.device
            or metadata.st_ino != identity.inode
        ):
            raise self.vmnet.CertificationError("process-cleanup")
        try:
            path.unlink()
        except OSError as error:
            raise self.vmnet.CertificationError("process-cleanup") from error

    def _finish(self) -> None:
        if self._closed:
            return
        try:
            self.process.close()
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error
        try:
            self._remove_stale_socket()
            self.vmnet._wait_socket_absent(
                self.files.api_socket, self.config.timeouts.request_seconds
            )
        except self.vmnet.CertificationError as error:
            raise self.vmnet.CertificationError("socket-cleanup") from error
        try:
            self.driver.wait_sessions(self.baseline_sessions)
        except self.vmnet.CertificationError as error:
            raise self.vmnet.CertificationError("session-cleanup") from error
        if self._roles is not None:
            for pid in (
                self._roles.provider,
                self._roles.outer,
                self._roles.worker,
                *self._roles.owners,
            ):
                self.vmnet._wait_process_absent(
                    pid, self.config.timeouts.terminate_seconds
                )
        if not self._retain_files:
            try:
                self.driver.cleanup_case_files(self.files)
            except self.vmnet.CertificationError as error:
                if error.category in CASE_FILE_CLEANUP_CATEGORIES:
                    raise
                raise self.vmnet.CertificationError("file-cleanup") from error
        self._closed = True

    def terminate(self) -> None:
        roles = self.roles(require_owner=False)
        self._signal_ordinary(roles.outer, signal.SIGTERM)
        self._wait_remote()
        self._finish()

    def wait_after_external_signal(self) -> None:
        self._wait_remote()
        self._finish()

    def close(self) -> None:
        if self._closed:
            return
        try:
            self.process.close()
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error
        try:
            self._remove_stale_socket()
            self.vmnet._wait_socket_absent(
                self.files.api_socket, self.config.timeouts.request_seconds
            )
        except self.vmnet.CertificationError as error:
            raise self.vmnet.CertificationError("socket-cleanup") from error
        try:
            self.driver.wait_sessions(self.baseline_sessions)
        except self.vmnet.CertificationError as error:
            raise self.vmnet.CertificationError("session-cleanup") from error
        if self._roles is not None:
            for pid in (
                self._roles.provider,
                self._roles.outer,
                self._roles.worker,
                *self._roles.owners,
            ):
                self.vmnet._wait_process_absent(
                    pid, self.config.timeouts.terminate_seconds
                )
        if not self._retain_files:
            try:
                self.driver.cleanup_case_files(self.files)
            except self.vmnet.CertificationError as error:
                if error.category in CASE_FILE_CLEANUP_CATEGORIES:
                    raise
                raise self.vmnet.CertificationError("file-cleanup") from error
        self._closed = True

    def retain_files(self) -> None:
        if self._closed:
            raise self.vmnet.CertificationError("internal")
        self._retain_files = True


class ElevatedSystemCertificationDriver:
    """Exact elevated product driver; case mechanics are defined below."""

    def __init__(
        self,
        vmnet: ModuleType,
        handoff: ModuleType,
        proxy: Any,
        layout: Any,
        uid: int,
        gid: int,
        config: Any,
        session: Any,
        artifacts: Any,
    ) -> None:
        self.vmnet = vmnet
        self.handoff = handoff
        self.proxy = proxy
        self.layout = layout
        self.uid = uid
        self.gid = gid
        self.config = config
        self.session = session
        self.artifacts = artifacts
        self._attempts: dict[str, int] = {}
        self._active: list[RemoteProductionProcess] = []
        self._closed = False
        self._session_root = handoff._probe_session_root()
        if self.session_entries():
            _fail(vmnet, "cleanup")
        self._initialize_production_home()

    def _initialize_production_home(self) -> None:
        outcome = self.vmnet.run_bounded_command(
            (os.fspath(self.layout.launcher), "--help"),
            timeout_seconds=self.config.timeouts.request_seconds,
            phase="elevated-bundle-initialize",
            environment=self.vmnet._production_environment(
                temporary=self.session.path
            ),
        )
        self.vmnet._require_worker_help(outcome)

    def session_entries(self) -> tuple[Any, ...]:
        try:
            return self.handoff._probe_session_entries(self._session_root)
        except BaseException as error:
            raise _map_handoff_error(self.vmnet, error) from error

    def wait_sessions(self, baseline: tuple[Any, ...]) -> None:
        deadline = time.monotonic() + self.config.timeouts.terminate_seconds
        while True:
            if self.session_entries() == baseline:
                return
            if time.monotonic() >= deadline:
                raise self.vmnet.CertificationError("process-cleanup")
            time.sleep(self.vmnet.POLL_SECONDS)

    def cleanup_case_files(self, files: Any) -> None:
        root = files.root
        try:
            root.relative_to(self.session.path)
            metadata = os.lstat(root)
            if (
                not stat.S_ISDIR(metadata.st_mode)
                or stat.S_ISLNK(metadata.st_mode)
                or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != 0o700
            ):
                _fail(self.vmnet, "case-root-cleanup")
        except self.vmnet.CertificationError:
            raise
        except (OSError, ValueError) as error:
            raise self.vmnet.CertificationError("case-root-cleanup") from error
        try:
            self.vmnet._clean_directory(root)
        except self.vmnet.CertificationError as error:
            raise self.vmnet.CertificationError("case-tree-cleanup") from error
        try:
            os.rmdir(root)
            if os.path.lexists(root):
                _fail(self.vmnet, "case-remove-cleanup")
        except self.vmnet.CertificationError:
            raise
        except OSError as error:
            raise self.vmnet.CertificationError("case-remove-cleanup") from error
        try:
            self.session.verify()
        except self.vmnet.CertificationError as error:
            raise self.vmnet.CertificationError("session-cleanup") from error

    def _files(
        self,
        case: str,
        *,
        mode: Optional[str] = None,
        endpoint: Optional[Any] = None,
        nonce: bytes = b"",
        control_data: Optional[bytes] = None,
    ) -> Any:
        attempt = self._next_attempt(case)
        return self.vmnet._create_case_files(
            self.session,
            self.artifacts,
            case,
            self.vmnet.V2_CASE_NAMES.index(case),
            mode=mode,
            endpoint=endpoint,
            nonce=nonce,
            attempt=attempt,
            case_names=self.vmnet.V2_CASE_NAMES,
            control_data=control_data,
        )

    def _next_attempt(self, case: str) -> int:
        attempt = self._attempts.get(case, 0)
        if not 0 <= attempt <= 99:
            _fail(self.vmnet, "internal")
        self._attempts[case] = attempt + 1
        return attempt

    def _create_staged_files(
        self,
        case: str,
        endpoint: Any,
        nonce: bytes,
        scenario: Any,
        *,
        traffic: Optional[Path] = None,
        barrier: Optional[StagedBarrier] = None,
        snapshot_outputs: bool = False,
        snapshot_inputs: Optional[tuple[Path, Path]] = None,
    ) -> tuple[StagedCaseFiles, StagedBarrier]:
        attempt = self._next_attempt(case)
        index = self.vmnet.V2_CASE_NAMES.index(case)
        root = self.session.path / f"case-{index:02d}-{attempt:02d}-{case}"
        api_directory = root / "api"
        created = False
        try:
            self.vmnet._recheck_artifact(
                self.artifacts.kernel,
                self.artifacts.kernel_identity,
                owner_uid=self.artifacts.owner_uid,
            )
            self.vmnet._recheck_artifact(
                self.artifacts.rootfs,
                self.artifacts.rootfs_identity,
                owner_uid=self.artifacts.owner_uid,
            )
            self.vmnet._create_private_directory(root)
            created = True
            self.vmnet._create_private_directory(api_directory)
            serial = root / "serial.out"
            serial_identity = self.vmnet._write_private_file(serial, b"")
            if traffic is None:
                traffic = root / "traffic.bin"
                traffic_identity = self.vmnet._write_private_file(
                    traffic,
                    _staged_traffic_control(self.vmnet, endpoint.port, nonce),
                )
            else:
                descriptor, traffic_identity = self.vmnet._open_regular(
                    traffic,
                    category="control",
                    maximum=self.vmnet.CONTROL_BYTES,
                    private=True,
                )
                os.close(descriptor)
            if barrier is None:
                barrier_path = root / "barrier.bin"
                barrier = StagedBarrier(
                    self.vmnet,
                    barrier_path,
                    scenario,
                    nonce,
                    create=True,
                )
            else:
                barrier_path = barrier.path
            state_directory: Optional[Path] = None
            memory_directory: Optional[Path] = None
            grants: list[dict[str, object]] = [
                {
                    "access": "read-only",
                    "id": self.vmnet.KERNEL_GRANT_ID,
                    "role": "kernel-image",
                    "source": self.vmnet._path_text(
                        self.artifacts.kernel, "artifact"
                    ),
                },
                {
                    "access": "read-only",
                    "id": self.vmnet.ROOTFS_GRANT_ID,
                    "role": "drive-backing",
                    "source": self.vmnet._path_text(
                        self.artifacts.rootfs, "artifact"
                    ),
                },
                {
                    "access": "read-only",
                    "id": STAGED_TRAFFIC_ID,
                    "role": "drive-backing",
                    "source": self.vmnet._path_text(traffic, "control"),
                },
                {
                    "access": "read-write",
                    "id": STAGED_BARRIER_ID,
                    "role": "drive-backing",
                    "source": self.vmnet._path_text(barrier_path, "control"),
                },
                {
                    "access": "write-only",
                    "id": self.vmnet.SERIAL_GRANT_ID,
                    "role": "serial-sink",
                    "source": self.vmnet._path_text(serial, "session"),
                },
                {
                    "access": "create-children",
                    "id": self.vmnet.API_DIRECTORY_GRANT_ID,
                    "role": "api-socket-directory",
                    "source": self.vmnet._path_text(api_directory, "session"),
                },
            ]
            if snapshot_outputs:
                state_directory = root / "snapshot-state"
                memory_directory = root / "snapshot-memory"
                self.vmnet._create_private_directory(state_directory)
                self.vmnet._create_private_directory(memory_directory)
                grants.extend(
                    (
                        {
                            "access": "create-children",
                            "id": SNAPSHOT_STATE_OUTPUT_ID,
                            "role": "snapshot-output-directory",
                            "source": self.vmnet._path_text(
                                state_directory, "session"
                            ),
                        },
                        {
                            "access": "create-children",
                            "id": SNAPSHOT_MEMORY_OUTPUT_ID,
                            "role": "snapshot-output-directory",
                            "source": self.vmnet._path_text(
                                memory_directory, "session"
                            ),
                        },
                    )
                )
            if snapshot_inputs is not None:
                state, memory = snapshot_inputs
                self.vmnet._verify_regular_artifact(state, "snapshot-state")
                self.vmnet._verify_regular_artifact(memory, "snapshot-memory")
                grants.extend(
                    (
                        {
                            "access": "read-only",
                            "id": SNAPSHOT_STATE_INPUT_ID,
                            "role": "snapshot-state-input",
                            "source": self.vmnet._path_text(state, "session"),
                        },
                        {
                            "access": "read-only",
                            "id": SNAPSHOT_MEMORY_INPUT_ID,
                            "role": "snapshot-memory-input",
                            "source": self.vmnet._path_text(memory, "session"),
                        },
                    )
                )
            manifest = root / "grants.json"
            self.vmnet._write_private_file(
                manifest,
                self.vmnet.canonical_json({"grants": grants, "version": 1}),
            )
            api_socket = api_directory / self.vmnet.API_SOCKET_CHILD
            if len(os.fsencode(api_socket)) >= 104:
                _fail(self.vmnet, "socket")
            files = StagedCaseFiles(
                root,
                manifest,
                api_directory,
                api_socket,
                serial,
                serial_identity,
                traffic,
                traffic_identity,
                barrier_path,
                state_directory,
                memory_directory,
            )
            return files, barrier
        except BaseException:
            if created and os.path.lexists(root):
                try:
                    self.vmnet._clean_directory(root)
                    os.rmdir(root)
                except OSError:
                    pass
            raise

    def _spawn(
        self,
        case: str,
        *,
        allowed: Sequence[str] = (),
        maximum: Optional[int] = None,
        mode: Optional[str] = None,
        endpoint: Optional[Any] = None,
        nonce: bytes = b"",
        control_data: Optional[bytes] = None,
    ) -> RemoteProductionProcess:
        files = self._files(
            case,
            mode=mode,
            endpoint=endpoint,
            nonce=nonce,
            control_data=control_data,
        )
        return self._spawn_files(case, files, allowed=allowed, maximum=maximum)

    def _spawn_files(
        self,
        case: str,
        files: Any,
        *,
        allowed: Sequence[str],
        maximum: Optional[int],
    ) -> RemoteProductionProcess:
        instance = (
            f"elevated-{self.vmnet.V2_CASE_NAMES.index(case):02d}-"
            f"{self._attempts[case]:02d}"
        )
        try:
            process = RemoteProductionProcess(
                self,
                _elevated_launcher_arguments(
                    self.vmnet,
                    self.layout.bundle,
                    files,
                    instance,
                    allowed,
                    maximum,
                ),
                files,
            )
        except BaseException:
            self.cleanup_case_files(files)
            raise
        self._active.append(process)
        return process

    def _retire(self, process: RemoteProductionProcess) -> None:
        try:
            self._active.remove(process)
        except ValueError as error:
            raise self.vmnet.CertificationError("internal") from error

    def _finish_process(self, process: RemoteProductionProcess) -> None:
        try:
            process.terminate()
        finally:
            self._retire(process)

    def _abort_process(self, process: RemoteProductionProcess) -> None:
        if process not in self._active:
            return
        try:
            process.close()
        finally:
            if process in self._active:
                self._retire(process)

    def _configure(
        self,
        process: Any,
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
                        ELEVATED_VMNET_BOOT_ARGS
                        if guest_oracle
                        else self.vmnet.DIRECT_ROOTFS_BOOT_ARGS
                    ),
                    "kernel_image_path": self.vmnet.KERNEL_GRANT_REF,
                },
            ),
            (
                "/drives/rootfs",
                {
                    "drive_id": "rootfs",
                    "is_read_only": True,
                    "is_root_device": True,
                    "path_on_host": self.vmnet.ROOTFS_GRANT_REF,
                },
            ),
        ):
            self.vmnet._require_no_content(self.vmnet._api_put(process, path, body))
        if process.files.control is not None:
            self.vmnet._require_no_content(
                self.vmnet._api_put(
                    process,
                    "/drives/control",
                    {
                        "drive_id": "control",
                        "is_read_only": True,
                        "is_root_device": False,
                        "path_on_host": self.vmnet.CONTROL_GRANT_REF,
                    },
                )
            )
        self.vmnet._require_no_content(
            self.vmnet._api_put(
                process,
                "/serial",
                {"serial_out_path": self.vmnet.SERIAL_GRANT_REF},
            )
        )
        for iface_id, host_dev_name in networks:
            self.vmnet._require_no_content(
                self.vmnet._api_put(
                    process,
                    f"/network-interfaces/{iface_id}",
                    {"host_dev_name": host_dev_name, "iface_id": iface_id},
                )
            )
        if mmds_interfaces:
            self.vmnet._require_no_content(
                self.vmnet._api_put(
                    process,
                    "/mmds/config",
                    {
                        "ipv4_address": "169.254.169.254",
                        "network_interfaces": list(mmds_interfaces),
                        "version": "V1",
                    },
                )
            )

    def _start(self, process: Any) -> Any:
        return self.vmnet._api_put(
            process, "/actions", {"action_type": "InstanceStart"}
        )

    def _run_authority_split(self) -> None:
        baseline = self.session_entries()
        try:
            process = self.proxy.spawn(
                self.handoff._launcher_arguments(
                    self.layout,
                    self.uid,
                    self.gid,
                    "elevated-authority-split",
                    ("--version",),
                )
            )
            try:
                stdout, stderr = process.communicate(
                    self.config.timeouts.startup_seconds
                )
                if process.returncode != 0 or stderr or not stdout.startswith(b"bangbang "):
                    _fail(self.vmnet, "case")
            finally:
                process.close()
        except BaseException as error:
            if isinstance(error, self.vmnet.CertificationError):
                raise
            raise _map_handoff_error(self.vmnet, error) from error
        self.wait_sessions(baseline)

    def _run_networkless_denial(self) -> None:
        baseline = self.session_entries()
        outcome = self.vmnet.run_bounded_command(
            self.vmnet._networkless_denial_arguments(self.layout.bundle),
            timeout_seconds=self.config.timeouts.startup_seconds,
            phase="elevated-networkless-denial",
            check=False,
            environment=self.vmnet._production_environment(
                temporary=self.session.path
            ),
        )
        if (
            outcome.returncode != 1
            or outcome.stdout
            or outcome.stderr
            != b"bangbang launcher: invalid production launch policy\n"
        ):
            _fail(self.vmnet, "case")
        self.wait_sessions(baseline)

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
            try:
                process.wait_ready()
            except self.vmnet.CertificationError as error:
                status, stdout, stderr = process.wait_output()
                if stdout:
                    raise self.vmnet.CertificationError("case-stdout") from error
                if stderr:
                    raise self.vmnet.CertificationError("case-stderr") from error
                if 10 <= status <= 19:
                    raise self.vmnet.CertificationError(
                        f"provider-status-{status}"
                    ) from error
                raise self.vmnet.CertificationError("policy-configure") from error
            for index, (iface_id, host_dev_name) in enumerate(networks):
                try:
                    response = self.vmnet._api_put(
                        process,
                        f"/network-interfaces/{iface_id}",
                        {
                            "host_dev_name": host_dev_name,
                            "iface_id": iface_id,
                        },
                    )
                except self.vmnet.CertificationError as error:
                    raise self.vmnet.CertificationError("policy-request") from error
                try:
                    if index + 1 == len(networks):
                        self.vmnet._require_policy_denial(response)
                    else:
                        self.vmnet._require_no_content(response)
                except self.vmnet.CertificationError as error:
                    raise self.vmnet.CertificationError("policy-response") from error
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_missing_policy_denial(self, case: str) -> None:
        process = self._spawn(case)
        try:
            status, stdout, stderr = process.wait_output()
            if status != 11:
                category = (
                    f"provider-status-{status}"
                    if 10 <= status <= 19
                    else "case-status"
                )
                _fail(self.vmnet, category)
            if stdout:
                _fail(self.vmnet, "case-stdout")
            if stderr != b"bangbang launcher: invalid production launch policy\n":
                diagnostic = {
                    b"": 10,
                    b"bangbang launcher: private vmnet topology failed\n": 12,
                    b"bangbang launcher: invalid production bundle layout\n": 13,
                    (
                        b"bangbang: private launcher session failed\n"
                        b"bangbang launcher: private vmnet topology failed\n"
                    ): 14,
                }.get(stderr)
                if diagnostic is not None:
                    _fail(self.vmnet, f"provider-status-{diagnostic}")
                _fail(self.vmnet, "case-stderr")
            process.finish_exited()
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_mmds_only(self, case: str) -> None:
        process = self._spawn(case, allowed=(), maximum=None)
        try:
            self._configure(
                process,
                networks=(("eth0", "vmnet:shared"),),
                mmds_interfaces=("eth0",),
            )
            self.vmnet._require_no_content(self._start(process))
            self.vmnet._wait_serial(
                files,
                self.vmnet.DIRECT_ROOTFS_BOOT_MARKER,
                self.config.timeouts.guest_seconds,
            )
            process.roles(require_owner=False, forbid_owner=True)
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_connectivity(
        self,
        case: str,
        mode: str,
        endpoint: Optional[Any],
        nonce: bytes,
    ) -> None:
        if endpoint is None:
            _fail(self.vmnet, "fixture-protocol")
        if mode == "shared":
            authority = "shared"
            host_dev_name = "vmnet:shared"
        elif mode == "host":
            authority = "host"
            host_dev_name = "vmnet:host"
        elif mode == "bridged":
            bridge = self.config.optional_cases.bridged_interface
            if bridge is None:
                _fail(self.vmnet, "optional-cases")
            authority = f"bridged:{bridge}"
            host_dev_name = f"vmnet:bridged:{bridge}"
        else:
            _fail(self.vmnet, "internal")
        process = self._spawn(
            case,
            allowed=(authority,),
            maximum=1,
            control_data=_staged_traffic_control(
                self.vmnet, endpoint.port, nonce
            ),
        )
        try:
            self._configure(
                process,
                networks=(("eth0", host_dev_name),),
                guest_oracle=True,
            )
            self.vmnet._require_no_content(self._start(process))
            process.owner_pid()
            _wait_router_oracle(
                self.vmnet,
                process.files,
                self.config.timeouts.guest_seconds,
            )
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_service_case(self, case: str, expected: str) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            self.vmnet._require_service_status(self._start(process), expected)
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _start_live_shared(self, case: str) -> RemoteProductionProcess:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            self.vmnet._require_no_content(self._start(process))
            process.owner_pid()
            self.vmnet._wait_serial(
                process.files,
                self.vmnet.DIRECT_ROOTFS_BOOT_MARKER,
                self.config.timeouts.guest_seconds,
            )
            return process
        except BaseException:
            self._abort_process(process)
            raise

    def _run_partial_start(self, case: str) -> None:
        process = self._start_live_shared(case)
        try:
            self.vmnet._require_policy_denial(
                self.vmnet._api_put(
                    process,
                    "/network-interfaces/eth1",
                    {"host_dev_name": "vmnet:shared", "iface_id": "eth1"},
                )
            )
            self.vmnet._require_running(self.vmnet._api_get(process, "/"))
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_pre_ready_cancellation(self, case: str) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            socket_identity, peer_pid = process.api_authority()
            self.vmnet.http_send_without_response(
                process.files.api_socket,
                "PUT",
                "/actions",
                {"action_type": "InstanceStart"},
                self.config.timeouts.request_seconds,
                socket_identity=socket_identity,
                expected_peer_pid=peer_pid,
            )
            process.signal_role("outer", signal.SIGTERM)
            process.wait_after_external_signal()
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_post_ready_cancellation(self, case: str) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            self._configure(process, networks=(("eth0", "vmnet:shared"),))
            self.vmnet._require_no_content(self._start(process))
            process.owner_pid()
            process.signal_role("outer", signal.SIGTERM)
            process.wait_after_external_signal()
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_startup_provider_death(self, case: str, number: int) -> None:
        process = self._spawn(case, allowed=("shared",), maximum=1)
        try:
            process.signal_role("provider", number)
            process.wait_after_external_signal()
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_live_role_death(self, case: str, role: str, number: int) -> None:
        process = self._start_live_shared(case)
        try:
            pid = process.signal_role(role, number)
            process.wait_after_external_signal()
            self.vmnet._wait_process_absent(
                pid, self.config.timeouts.terminate_seconds
            )
            self._retire(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_clean_repeat(self, case: str) -> None:
        for _attempt in range(2):
            self._finish_process(self._start_live_shared(case))

    def _run_concurrent(self, case: str) -> None:
        first = self._start_live_shared(case)
        second: Optional[RemoteProductionProcess] = None
        try:
            second = self._spawn(case)
            self._configure(second)
            self.vmnet._require_no_content(self._start(second))
            self.vmnet._require_policy_denial(
                self.vmnet._api_put(
                    second,
                    "/network-interfaces/eth0",
                    {"host_dev_name": "vmnet:shared", "iface_id": "eth0"},
                )
            )
            first_roles = first.roles(require_owner=True)
            second_roles = second.roles(require_owner=False, forbid_owner=True)
            if {
                first_roles.provider,
                first_roles.outer,
                first_roles.worker,
                *first_roles.owners,
            } & {
                second_roles.provider,
                second_roles.outer,
                second_roles.worker,
                *second_roles.owners,
            }:
                _fail(self.vmnet, "case")
            self.vmnet._require_running(self.vmnet._api_get(first, "/"))
            self._finish_process(second)
            second = None
            self.vmnet._require_running(self.vmnet._api_get(first, "/"))
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
        endpoint: Optional[Any],
        nonce: bytes,
    ) -> None:
        if case not in self.vmnet.V2_CASE_NAMES or len(nonce) != 32 or not any(nonce):
            _fail(self.vmnet, "internal")
        if case == "authority-split":
            self._run_authority_split()
            return
        if case == "networkless-denial":
            self._run_networkless_denial()
            return
        if case == "missing-policy-denial":
            self._run_missing_policy_denial(case)
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
        if case in (
            "startup-interface-remove",
            "runtime-hotplug-remove",
            "capture-restore-fresh-ownership",
        ):
            self._run_staged(case, endpoint, nonce)
            return
        if case == "normal-teardown":
            self._finish_process(self._start_live_shared(case))
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
        if case == "provider-startup-death":
            self._run_startup_provider_death(case, signal.SIGTERM)
            return
        if case == "broker-runtime-death":
            self._run_live_role_death(case, "broker", signal.SIGTERM)
            return
        if case == "owner-runtime-death":
            self._run_live_role_death(case, "owner", signal.SIGTERM)
            return
        if case == "launcher-first-death":
            self._run_live_role_death(case, "outer", signal.SIGTERM)
            return
        if case == "worker-first-death":
            self._run_live_role_death(case, "worker", signal.SIGTERM)
            return
        if case == "provider-sigkill-reclamation":
            self._run_startup_provider_death(case, signal.SIGKILL)
            return
        if case == "broker-sigkill-reclamation":
            self._run_live_role_death(case, "broker", signal.SIGKILL)
            return
        if case == "owner-sigkill-reclamation":
            self._run_live_role_death(case, "owner", signal.SIGKILL)
            return
        if case == "launcher-sigkill-reclamation":
            self._run_live_role_death(case, "outer", signal.SIGKILL)
            return
        if case == "worker-sigkill-reclamation":
            self._run_live_role_death(case, "worker", signal.SIGKILL)
            return
        if case == "clean-repeat":
            self._run_clean_repeat(case)
            return
        if case == "concurrent-noninterchangeability":
            self._run_concurrent(case)
            return
        _fail(self.vmnet, "internal")

    def _run_staged(
        self, case: str, endpoint: Optional[Any], nonce: bytes
    ) -> None:
        if endpoint is None:
            _fail(self.vmnet, "fixture-protocol")
        protocol = load_staged_protocol()
        if case == "startup-interface-remove":
            self._run_staged_startup(
                case, endpoint, nonce, protocol.Scenario.STARTUP
            )
            return
        if case == "runtime-hotplug-remove":
            self._run_staged_runtime(
                case, endpoint, nonce, protocol.Scenario.RUNTIME
            )
            return
        if case == "capture-restore-fresh-ownership":
            self._run_staged_restore(
                case, endpoint, nonce, protocol.Scenario.RESTORE
            )
            return
        _fail(self.vmnet, "internal")

    def _api_exchange(
        self, process: Any, method: str, path: str, body: Mapping[str, object]
    ) -> Any:
        socket_identity, peer_pid = process.api_authority()
        return self.vmnet.http_exchange(
            process.files.api_socket,
            method,
            path,
            body,
            self.config.timeouts.request_seconds,
            socket_identity=socket_identity,
            expected_peer_pid=peer_pid,
        )

    def _network_put(self, process: Any) -> None:
        self.vmnet._require_no_content(
            self.vmnet._api_put(
                process,
                "/network-interfaces/eth0",
                {"host_dev_name": "vmnet:shared", "iface_id": "eth0"},
            )
        )

    def _network_delete(self, process: Any) -> None:
        self.vmnet._require_no_content(
            self._api_exchange(process, "DELETE", "/network-interfaces/eth0", {})
        )

    def _configure_staged(
        self, process: RemoteProductionProcess, *, startup_network: bool
    ) -> None:
        process.wait_ready()
        for path, body in (
            ("/machine-config", {"mem_size_mib": 256, "vcpu_count": 1}),
            (
                "/boot-source",
                {
                    "boot_args": STAGED_BOOT_ARGS,
                    "kernel_image_path": self.vmnet.KERNEL_GRANT_REF,
                },
            ),
            (
                "/drives/rootfs",
                {
                    "drive_id": "rootfs",
                    "is_read_only": True,
                    "is_root_device": True,
                    "path_on_host": self.vmnet.ROOTFS_GRANT_REF,
                },
            ),
            (
                "/drives/traffic",
                {
                    "drive_id": "traffic",
                    "is_read_only": True,
                    "is_root_device": False,
                    "path_on_host": STAGED_TRAFFIC_REF,
                },
            ),
            (
                "/drives/barrier",
                {
                    "cache_type": "Writeback",
                    "drive_id": "barrier",
                    "is_read_only": False,
                    "is_root_device": False,
                    "path_on_host": STAGED_BARRIER_REF,
                },
            ),
            (
                "/serial",
                {"serial_out_path": self.vmnet.SERIAL_GRANT_REF},
            ),
        ):
            self.vmnet._require_no_content(self.vmnet._api_put(process, path, body))
        if startup_network:
            self._network_put(process)
        self.vmnet._require_no_content(self._start(process))

    def _run_staged_startup(
        self,
        case: str,
        endpoint: Any,
        nonce: bytes,
        scenario: Any,
    ) -> None:
        files, barrier = self._create_staged_files(
            case, endpoint, nonce, scenario
        )
        process = self._spawn_files(
            case, files, allowed=("shared",), maximum=1
        )
        protocol = barrier.protocol
        try:
            self._configure_staged(process, startup_network=True)
            first_owner = process.owner_pid()
            barrier.wait(process, 1, protocol.Status.INITIAL_PRESENT)
            barrier.command(1)
            barrier.wait(process, 2, protocol.Status.TRAFFIC_ONE)
            barrier.command(2)
            barrier.wait(process, 3, protocol.Status.ABSENT)
            self._network_delete(process)
            self.vmnet._wait_process_absent(
                first_owner, self.config.timeouts.terminate_seconds
            )
            self._network_put(process)
            second_owner = process.owner_pid()
            if second_owner == first_owner:
                _fail(self.vmnet, "case")
            barrier.command(3)
            barrier.wait(process, 4, protocol.Status.PRESENT)
            barrier.wait(process, 5, protocol.Status.TRAFFIC_TWO)
            barrier.command(4)
            barrier.wait(process, 6, protocol.Status.ABSENT)
            self._network_delete(process)
            self.vmnet._wait_process_absent(
                second_owner, self.config.timeouts.terminate_seconds
            )
            barrier.command(5)
            barrier.wait(process, 7, protocol.Status.COMPLETE)
            barrier.assert_terminal()
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_staged_runtime(
        self,
        case: str,
        endpoint: Any,
        nonce: bytes,
        scenario: Any,
    ) -> None:
        files, barrier = self._create_staged_files(
            case, endpoint, nonce, scenario
        )
        process = self._spawn_files(
            case, files, allowed=("shared",), maximum=1
        )
        protocol = barrier.protocol
        try:
            self._configure_staged(process, startup_network=False)
            barrier.wait(process, 1, protocol.Status.INITIAL_ABSENT)
            process.roles(require_owner=False, forbid_owner=True)
            self._network_put(process)
            barrier.command(1)
            owner = process.owner_pid()
            barrier.wait(process, 2, protocol.Status.PRESENT)
            barrier.wait(process, 3, protocol.Status.TRAFFIC_ONE)
            barrier.command(2)
            barrier.wait(process, 4, protocol.Status.ABSENT)
            self._network_delete(process)
            self.vmnet._wait_process_absent(
                owner, self.config.timeouts.terminate_seconds
            )
            barrier.command(3)
            barrier.wait(process, 5, protocol.Status.COMPLETE)
            barrier.assert_terminal()
            self._finish_process(process)
        except BaseException:
            self._abort_process(process)
            raise

    def _run_staged_restore(
        self,
        case: str,
        endpoint: Any,
        nonce: bytes,
        scenario: Any,
    ) -> None:
        source_files, barrier = self._create_staged_files(
            case,
            endpoint,
            nonce,
            scenario,
            snapshot_outputs=True,
        )
        source = self._spawn_files(
            case, source_files, allowed=("shared",), maximum=1
        )
        destination: Optional[RemoteProductionProcess] = None
        source_retired = False
        source_files_cleaned = False
        protocol = barrier.protocol
        try:
            self._configure_staged(source, startup_network=True)
            first_owner = source.owner_pid()
            barrier.wait(source, 1, protocol.Status.INITIAL_PRESENT)
            barrier.command(1)
            barrier.wait(source, 2, protocol.Status.CAPTURE_READY)
            self.vmnet._require_no_content(
                self._api_exchange(source, "PATCH", "/vm", {"state": "Paused"})
            )
            self.vmnet._require_no_content(
                self.vmnet._api_put(
                    source,
                    "/snapshot/create",
                    {
                        "mem_file_path": SNAPSHOT_MEMORY_OUTPUT_REF,
                        "snapshot_path": SNAPSHOT_STATE_OUTPUT_REF,
                        "snapshot_type": "Full",
                    },
                )
            )
            if (
                source_files.state_directory is None
                or source_files.memory_directory is None
            ):
                _fail(self.vmnet, "internal")
            state = source_files.state_directory / "state.snap"
            memory = source_files.memory_directory / "memory.snap"
            self.vmnet._verify_regular_artifact(state, "snapshot-state")
            self.vmnet._verify_regular_artifact(memory, "snapshot-memory")
            source.retain_files()
            self._finish_process(source)
            source_retired = True
            self.vmnet._wait_process_absent(
                first_owner, self.config.timeouts.terminate_seconds
            )
            destination_files, destination_barrier = self._create_staged_files(
                case,
                endpoint,
                nonce,
                scenario,
                traffic=source_files.control,
                barrier=barrier,
                snapshot_inputs=(state, memory),
            )
            if destination_barrier is not barrier:
                _fail(self.vmnet, "internal")
            destination = self._spawn_files(
                case,
                destination_files,
                allowed=("shared",),
                maximum=1,
            )
            destination.wait_ready()
            self.vmnet._require_no_content(
                self.vmnet._api_put(
                    destination,
                    "/snapshot/load",
                    {
                        "mem_backend": {
                            "backend_path": SNAPSHOT_MEMORY_INPUT_REF,
                            "backend_type": "File",
                        },
                        "network_overrides": [
                            {
                                "host_dev_name": "vmnet:shared",
                                "iface_id": "eth0",
                            }
                        ],
                        "resume_vm": True,
                        "snapshot_path": SNAPSHOT_STATE_INPUT_REF,
                    },
                )
            )
            second_owner = destination.owner_pid()
            if second_owner == first_owner:
                _fail(self.vmnet, "case")
            barrier.command(2)
            barrier.wait(destination, 3, protocol.Status.PRESENT)
            barrier.wait(destination, 4, protocol.Status.TRAFFIC_TWO)
            barrier.command(3)
            barrier.wait(destination, 5, protocol.Status.ABSENT)
            self._network_delete(destination)
            self.vmnet._wait_process_absent(
                second_owner, self.config.timeouts.terminate_seconds
            )
            barrier.command(4)
            barrier.wait(destination, 6, protocol.Status.COMPLETE)
            barrier.assert_terminal()
            self._finish_process(destination)
            destination = None
            self.cleanup_case_files(source_files)
            source_files_cleaned = True
        except BaseException:
            if destination is not None:
                self._abort_process(destination)
            if not source_retired and source in self._active:
                source.retain_files()
                self._abort_process(source)
            raise
        finally:
            if not source_files_cleaned and os.path.lexists(source_files.root):
                self.cleanup_case_files(source_files)

    def close(self) -> None:
        cleanup_error: Optional[Any] = None
        for process in reversed(self._active):
            try:
                process.close()
            except self.vmnet.CertificationError as error:
                cleanup_error = cleanup_error or error
        self._active.clear()
        if self.session_entries():
            cleanup_error = cleanup_error or self.vmnet.CertificationError("cleanup")
        self._closed = True
        if cleanup_error is not None:
            raise cleanup_error


def _controller_entry(
    vmnet: ModuleType,
    proxy: Any,
    layout: Any,
    uid: int,
    gid: int,
) -> None:
    handoff = load_handoff()
    try:
        package = layout.bundle.parent
        loaded = load_package(vmnet, package, uid, gid)

        def recheck() -> None:
            recheck_loaded_package(vmnet, package, loaded)

        assertions = vmnet.ElevatedEntitlementAssertions(True, True, True, False)
        vmnet.run_elevated_certification(
            loaded.config,
            loaded.result,
            loaded.source,
            loaded.host,
            assertions,
            lambda config, session: ElevatedSystemCertificationDriver(
                vmnet,
                handoff,
                proxy,
                layout,
                uid,
                gid,
                config,
                session,
                loaded.artifacts,
            ),
            recheck=recheck,
        )
    except vmnet.CertificationError as error:
        category = (
            error.category
            if error.category in handoff.CERTIFICATION_FAILURES
            else "internal"
        )
        raise handoff.HandoffError(f"certification-{category}") from error


def run_elevated(
    vmnet: ModuleType,
    prepared: Path,
    uid: int,
    gid: int,
) -> None:
    handoff = load_handoff()

    def loader() -> Callable[[Any, Any, int, int], None]:
        return lambda proxy, layout, target_uid, target_gid: _controller_entry(
            vmnet, proxy, layout, target_uid, target_gid
        )

    try:
        handoff.run_root(
            prepared,
            uid,
            gid,
            loader,
            session_timeout=ELEVATED_SESSION_TIMEOUT,
        )
    except handoff.HandoffError as error:
        raise vmnet.CertificationError(_handoff_category(error.category)) from error
