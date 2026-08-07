#!/usr/bin/env python3
"""Checked guest-artifact cache and no-clobber publication policy."""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence, TextIO
from urllib.parse import urlparse


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
GUEST_WORKFLOW_AUDIT_PATH = Path(
    "compat/firecracker/v1.16.0/guest-workflow-audit.json"
)
MANIFEST_PATH = REPOSITORY_ROOT / GUEST_WORKFLOW_AUDIT_PATH
DEFAULT_CACHE_RELATIVE = Path(".tmp/guest-artifacts")
MAX_MANIFEST_BYTES = 256 * 1024
MAX_STRING_BYTES = 2048
MAX_EXT4_BYTES = 16 * 1024**4
FILE_MODE = 0o644
STAGE_MODE = 0o600
DIRECT_POPULATE_ENV = "BANGBANG_GUEST_POLICY_INTERNAL"


class ArtifactPolicyError(RuntimeError):
    """A stable guest-artifact policy failure."""

    def __init__(self, category: str, message: str) -> None:
        super().__init__(message)
        self.category = category


@dataclass(frozen=True)
class DownloadArtifact:
    artifact_id: str
    kind: str
    filename: str
    url: str
    sha256: str
    size_bytes: int
    cache_path: Path


@dataclass(frozen=True)
class GeneratedArtifact:
    artifact_id: str
    generator_path: Path
    cache_path: Path
    sha256: str
    size_bytes: int


@dataclass(frozen=True)
class Ext4Recipe:
    recipe_id: str
    variant: str
    filename_template: str
    default_size: str
    minimum_size_bytes: int
    tracked_inputs: tuple[Path, ...]
    sidecar_suffix: str
    raw: Mapping[str, Any]


@dataclass(frozen=True)
class ManifestReference:
    path: Path
    anchor: str


@dataclass(frozen=True)
class GuestIdentity:
    path: str
    size_bytes: int
    sha256: str


@dataclass(frozen=True)
class WorkflowTimeouts:
    artifact_seconds: int
    build_seconds: int
    startup_seconds: int
    request_seconds: int
    guest_seconds: int
    terminate_seconds: int


@dataclass(frozen=True)
class GuestWorkflowProfile:
    profile_id: str
    mode: str
    kernel_artifact: str
    rootfs_artifact: str
    initrd_artifact: str
    boot_args: str
    rootfs_read_only: bool
    success_marker: str
    failure_marker: str
    implementation: tuple[ManifestReference, ...]
    validation: tuple[ManifestReference, ...]


@dataclass(frozen=True)
class GuestWorkflowManifest:
    downloads: Mapping[str, DownloadArtifact]
    generated: Mapping[str, GeneratedArtifact]
    recipes: Mapping[str, Ext4Recipe]
    profiles: Mapping[str, GuestWorkflowProfile] = field(default_factory=dict)
    guest_identity: Optional[GuestIdentity] = None
    timeouts: Optional[WorkflowTimeouts] = None


@dataclass(frozen=True)
class ToolSet:
    unsquashfs: Path
    mkfs_ext4: Path
    e2fsck: Path
    versions: Mapping[str, str]


@dataclass(frozen=True)
class OwnedPath:
    path: Path
    device: int
    inode: int

    @classmethod
    def capture(cls, path: Path) -> "OwnedPath":
        metadata = os.lstat(path)
        return cls(path=path, device=metadata.st_dev, inode=metadata.st_ino)

    def still_owned(self) -> bool:
        try:
            metadata = os.lstat(self.path)
        except FileNotFoundError:
            return False
        return metadata.st_dev == self.device and metadata.st_ino == self.inode


def _duplicate_safe_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ArtifactPolicyError("manifest", f"duplicate manifest key: {key}")
        result[key] = value
    return result


def _require_object(value: Any, keys: Sequence[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ArtifactPolicyError("manifest", f"{label} must be an object")
    expected = set(keys)
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        unknown = sorted(actual - expected)
        raise ArtifactPolicyError(
            "manifest",
            f"{label} has missing keys {missing} and unknown keys {unknown}",
        )
    return value


def _require_list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise ArtifactPolicyError("manifest", f"{label} must be an array")
    return value


def _require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ArtifactPolicyError("manifest", f"{label} must be a non-empty string")
    encoded = value.encode("utf-8")
    if len(encoded) > MAX_STRING_BYTES or any(ord(character) < 0x20 for character in value):
        raise ArtifactPolicyError("manifest", f"{label} is not a bounded printable string")
    return value


def _require_integer(value: Any, label: str, *, minimum: int = 1) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ArtifactPolicyError("manifest", f"{label} must be an integer >= {minimum}")
    return value


def _require_bool(value: Any, label: str) -> bool:
    if not isinstance(value, bool):
        raise ArtifactPolicyError("manifest", f"{label} must be a boolean")
    return value


def _require_sha256(value: Any, label: str) -> str:
    digest = _require_string(value, label)
    if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise ArtifactPolicyError("manifest", f"{label} must be a lowercase SHA-256")
    return digest


def _require_relative_path(value: Any, label: str) -> Path:
    text = _require_string(value, label)
    path = Path(text)
    if path.is_absolute() or not path.parts or any(part in ("", ".", "..") for part in path.parts):
        raise ArtifactPolicyError("manifest", f"{label} must be a safe relative path")
    return path


def _require_exact_strings(value: Any, expected: Sequence[str], label: str) -> None:
    items = _require_list(value, label)
    if items != list(expected) or any(not isinstance(item, str) for item in items):
        raise ArtifactPolicyError("manifest", f"{label} does not match the checked contract")


def _require_reference(value: Any, label: str) -> ManifestReference:
    reference = _require_object(value, ("kind", "path", "anchor"), label)
    if reference["kind"] != "local":
        raise ArtifactPolicyError("manifest", f"{label} must be local evidence")
    return ManifestReference(
        path=_require_relative_path(reference["path"], f"{label}.path"),
        anchor=_require_string(reference["anchor"], f"{label}.anchor"),
    )


def load_manifest(path: Path = MANIFEST_PATH) -> GuestWorkflowManifest:
    """Load the repository-owned manifest with duplicate and closed-field checks."""

    try:
        raw_bytes = path.read_bytes()
    except OSError as error:
        raise ArtifactPolicyError("manifest", f"failed to read checked manifest: {error}") from error
    if not raw_bytes or len(raw_bytes) > MAX_MANIFEST_BYTES:
        raise ArtifactPolicyError("manifest", "checked manifest has an invalid byte size")
    try:
        raw = json.loads(raw_bytes, object_pairs_hook=_duplicate_safe_object)
    except ArtifactPolicyError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ArtifactPolicyError("manifest", f"failed to parse checked manifest: {error}") from error

    root = _require_object(
        raw,
        (
            "schema_version",
            "baseline",
            "delivery",
            "source_namespace",
            "artifacts",
            "generated",
            "ext4_recipes",
            "output_classes",
            "guest_identity",
            "timeouts",
            "profiles",
            "evidence",
            "nonclaims",
        ),
        "manifest",
    )
    if _require_integer(root["schema_version"], "schema_version") != 1:
        raise ArtifactPolicyError("manifest", "unsupported guest workflow schema version")

    baseline = _require_object(root["baseline"], ("version", "commit", "target"), "baseline")
    if [baseline[key] for key in ("version", "commit", "target")] != [
        "1.16.0",
        "d83d72b710361a10294480131377b1b00b163af8",
        "aarch64-macos-hvf",
    ]:
        raise ArtifactPolicyError("manifest", "checked manifest baseline drifted")

    delivery = _require_object(
        root["delivery"],
        ("parent_issue", "preparation_issue", "completion_issue", "state"),
        "delivery",
    )
    if [
        delivery["parent_issue"],
        delivery["preparation_issue"],
        delivery["completion_issue"],
        delivery["state"],
    ] != ["#1796", "#1871", "#1872", "complete"]:
        raise ArtifactPolicyError("manifest", "checked manifest delivery boundary drifted")

    namespace = _require_object(
        root["source_namespace"],
        ("release", "architecture", "provider", "provenance_url", "redistribution"),
        "source_namespace",
    )
    if namespace["release"] != "v1.15" or namespace["architecture"] != "aarch64":
        raise ArtifactPolicyError("manifest", "checked artifact namespace drifted")
    for key, value in namespace.items():
        _require_string(value, f"source_namespace.{key}")

    downloads: dict[str, DownloadArtifact] = {}
    artifact_items = _require_list(root["artifacts"], "artifacts")
    if len(artifact_items) != 2:
        raise ArtifactPolicyError("manifest", "manifest requires exactly kernel and rootfs artifacts")
    for index, value in enumerate(artifact_items):
        item = _require_object(
            value,
            (
                "id",
                "kind",
                "filename",
                "url",
                "sha256",
                "size_bytes",
                "cache_path",
                "output_class",
                "provenance",
                "redistribution",
            ),
            f"artifacts[{index}]",
        )
        artifact_id = _require_string(item["id"], f"artifacts[{index}].id")
        expected_id = ("kernel", "rootfs")[index]
        if artifact_id != expected_id or artifact_id in downloads:
            raise ArtifactPolicyError("manifest", "download artifact order or identity drifted")
        kind = _require_string(item["kind"], f"artifacts[{index}].kind")
        if kind != ("linux-kernel", "squashfs-rootfs")[index]:
            raise ArtifactPolicyError("manifest", f"artifact kind drifted: {artifact_id}")
        url = _require_string(item["url"], f"artifacts[{index}].url")
        parsed_url = urlparse(url)
        if parsed_url.scheme != "https" or not parsed_url.netloc or parsed_url.params or parsed_url.query or parsed_url.fragment:
            raise ArtifactPolicyError("manifest", f"artifact URL is not a fixed HTTPS URL: {artifact_id}")
        if item["output_class"] != "verified-repairable-cache":
            raise ArtifactPolicyError("manifest", f"artifact output class drifted: {artifact_id}")
        downloads[artifact_id] = DownloadArtifact(
            artifact_id=artifact_id,
            kind=kind,
            filename=_require_string(item["filename"], f"artifacts[{index}].filename"),
            url=url,
            sha256=_require_sha256(item["sha256"], f"artifacts[{index}].sha256"),
            size_bytes=_require_integer(item["size_bytes"], f"artifacts[{index}].size_bytes"),
            cache_path=_require_relative_path(item["cache_path"], f"artifacts[{index}].cache_path"),
        )
        _require_string(item["provenance"], f"artifacts[{index}].provenance")
        _require_string(item["redistribution"], f"artifacts[{index}].redistribution")

    generated: dict[str, GeneratedArtifact] = {}
    generated_items = _require_list(root["generated"], "generated")
    if len(generated_items) != 1:
        raise ArtifactPolicyError("manifest", "manifest requires exactly one generated artifact")
    generated_item = _require_object(
        generated_items[0],
        (
            "id",
            "generator_path",
            "cache_path",
            "sha256",
            "size_bytes",
            "output_class",
            "determinism",
        ),
        "generated[0]",
    )
    if (
        generated_item["id"] != "guest-boot-initrd"
        or generated_item["output_class"] != "deterministic-generated-cache"
        or generated_item["determinism"] != "byte-identical"
    ):
        raise ArtifactPolicyError("manifest", "generated initrd policy drifted")
    generated["guest-boot-initrd"] = GeneratedArtifact(
        artifact_id="guest-boot-initrd",
        generator_path=_require_relative_path(generated_item["generator_path"], "generated.generator_path"),
        cache_path=_require_relative_path(generated_item["cache_path"], "generated.cache_path"),
        sha256=_require_sha256(generated_item["sha256"], "generated.sha256"),
        size_bytes=_require_integer(generated_item["size_bytes"], "generated.size_bytes"),
    )

    recipes: dict[str, Ext4Recipe] = {}
    recipe_items = _require_list(root["ext4_recipes"], "ext4_recipes")
    expected_recipe_ids = ("rootfs-ext4", "rootfs-ext4-direct-boot-v109")
    for index, value in enumerate(recipe_items):
        item = _require_object(
            value,
            (
                "id",
                "source_artifact",
                "variant",
                "filename_template",
                "default_size",
                "minimum_size_bytes",
                "classification",
                "output_class",
                "tool_roles",
                "tracked_inputs",
                "sidecar",
            ),
            f"ext4_recipes[{index}]",
        )
        if len(recipe_items) != 2 or item["id"] != expected_recipe_ids[index]:
            raise ArtifactPolicyError("manifest", "ext4 recipe order or identity drifted")
        if (
            item["source_artifact"] != "rootfs"
            or item["classification"] != "recipe-deterministic"
            or item["output_class"] != "verified-repairable-cache"
        ):
            raise ArtifactPolicyError("manifest", f"ext4 recipe policy drifted: {item['id']}")
        _require_exact_strings(item["tool_roles"], ("unsquashfs", "mkfs.ext4", "e2fsck"), "tool_roles")
        tracked_inputs = tuple(
            _require_relative_path(entry, f"ext4_recipes[{index}].tracked_inputs")
            for entry in _require_list(item["tracked_inputs"], "tracked_inputs")
        )
        sidecar = _require_object(
            item["sidecar"],
            ("schema_version", "suffix", "fields", "filesystem_check"),
            f"ext4_recipes[{index}].sidecar",
        )
        if _require_integer(sidecar["schema_version"], "sidecar.schema_version") != 1:
            raise ArtifactPolicyError("manifest", "unsupported ext4 sidecar schema")
        _require_exact_strings(
            sidecar["fields"],
            (
                "schema_version",
                "source_sha256",
                "source_size_bytes",
                "requested_size_bytes",
                "variant",
                "recipe_sha256",
                "tool_versions",
                "output_sha256",
                "output_size_bytes",
                "filesystem_check",
            ),
            "sidecar.fields",
        )
        if sidecar["suffix"] != ".bangbang.json" or sidecar["filesystem_check"] != "e2fsck -fn":
            raise ArtifactPolicyError("manifest", "ext4 sidecar policy drifted")
        recipe_id = item["id"]
        recipes[recipe_id] = Ext4Recipe(
            recipe_id=recipe_id,
            variant=_require_string(item["variant"], f"ext4_recipes[{index}].variant"),
            filename_template=_require_string(item["filename_template"], "filename_template"),
            default_size=_require_string(item["default_size"], "default_size"),
            minimum_size_bytes=_require_integer(item["minimum_size_bytes"], "minimum_size_bytes"),
            tracked_inputs=tracked_inputs,
            sidecar_suffix=sidecar["suffix"],
            raw=item,
        )

    output_items = _require_list(root["output_classes"], "output_classes")
    output_ids = (
        "verified-repairable-cache",
        "deterministic-generated-cache",
        "caller-owned-absent-only",
        "unique-ephemeral-session",
    )
    if len(output_items) != len(output_ids):
        raise ArtifactPolicyError("manifest", "output-class set drifted")
    for index, value in enumerate(output_items):
        item = _require_object(
            value,
            ("id", "reuse", "repair", "publication", "collision", "locking"),
            f"output_classes[{index}]",
        )
        if item["id"] != output_ids[index]:
            raise ArtifactPolicyError("manifest", "output-class order or identity drifted")
        for key, item_value in item.items():
            _require_string(item_value, f"output_classes[{index}].{key}")

    identity = _require_object(
        root["guest_identity"],
        ("path", "size_bytes", "sha256"),
        "guest_identity",
    )
    guest_identity = GuestIdentity(
        path=_require_string(identity["path"], "guest_identity.path"),
        size_bytes=_require_integer(identity["size_bytes"], "guest_identity.size_bytes"),
        sha256=_require_sha256(identity["sha256"], "guest_identity.sha256"),
    )
    if guest_identity != GuestIdentity(
        path="/etc/os-release",
        size_bytes=400,
        sha256="3e5851448bae5b36f351becde037a8b13b77307279f484eda808f8177d9a4293",
    ):
        raise ArtifactPolicyError("manifest", "checked guest identity drifted")

    timeout_values = _require_object(
        root["timeouts"],
        (
            "artifact_seconds",
            "build_seconds",
            "startup_seconds",
            "request_seconds",
            "guest_seconds",
            "terminate_seconds",
        ),
        "timeouts",
    )
    timeouts = WorkflowTimeouts(
        artifact_seconds=_require_integer(
            timeout_values["artifact_seconds"], "timeouts.artifact_seconds"
        ),
        build_seconds=_require_integer(
            timeout_values["build_seconds"], "timeouts.build_seconds"
        ),
        startup_seconds=_require_integer(
            timeout_values["startup_seconds"], "timeouts.startup_seconds"
        ),
        request_seconds=_require_integer(
            timeout_values["request_seconds"], "timeouts.request_seconds"
        ),
        guest_seconds=_require_integer(
            timeout_values["guest_seconds"], "timeouts.guest_seconds"
        ),
        terminate_seconds=_require_integer(
            timeout_values["terminate_seconds"], "timeouts.terminate_seconds"
        ),
    )
    if timeouts != WorkflowTimeouts(600, 900, 30, 5, 60, 5):
        raise ArtifactPolicyError("manifest", "checked workflow timeouts drifted")

    profile_values = _require_list(root["profiles"], "profiles")
    expected_profiles = (
        ("macos-api-rootfs-smoke", "api"),
        ("macos-no-api-rootfs-smoke", "no-api"),
    )
    if len(profile_values) != 2:
        raise ArtifactPolicyError("manifest", "workflow profile set drifted")
    profiles: dict[str, GuestWorkflowProfile] = {}
    for index, value in enumerate(profile_values):
        item = _require_object(
            value,
            (
                "id",
                "state",
                "mode",
                "kernel_artifact",
                "rootfs_artifact",
                "initrd_artifact",
                "boot_args",
                "rootfs_read_only",
                "success_marker",
                "failure_marker",
                "shutdown",
                "networking",
                "platform",
                "implementation",
                "validation",
            ),
            f"profiles[{index}]",
        )
        expected_id, expected_mode = expected_profiles[index]
        implementation = tuple(
            _require_reference(reference, f"profiles[{index}].implementation")
            for reference in _require_list(
                item["implementation"], f"profiles[{index}].implementation"
            )
        )
        validation = tuple(
            _require_reference(reference, f"profiles[{index}].validation")
            for reference in _require_list(
                item["validation"], f"profiles[{index}].validation"
            )
        )
        if (
            item["id"] != expected_id
            or item["state"] != "implemented-and-verified"
            or item["mode"] != expected_mode
            or item["kernel_artifact"] != "kernel"
            or item["rootfs_artifact"] != "rootfs"
            or item["initrd_artifact"] != "guest-boot-initrd"
            or item["boot_args"]
            != "console=ttyS0 reboot=k panic=1 quiet loglevel=1 rdinit=/rootfs-poweroff-init"
            or _require_bool(item["rootfs_read_only"], "rootfs_read_only") is not True
            or item["success_marker"] != "BANGBANG_ROOTFS_WORKFLOW_OK"
            or item["failure_marker"] != "BANGBANG_ROOTFS_WORKFLOW_FAIL"
            or item["shutdown"] != "guest-poweroff"
            or item["networking"] != "none"
            or item["platform"] != "aarch64-apple-darwin-hvf"
            or len(implementation) != 1
            or implementation[0]
            != ManifestReference(
                Path("scripts/run-macos-guest-workflow.py"),
                "def run_workflow(",
            )
            or len(validation) != 1
            or validation[0]
            != ManifestReference(
                Path("scripts/run-integration-tests.sh"),
                f"scripts/run-macos-guest-workflow.py {expected_mode}",
            )
        ):
            raise ArtifactPolicyError("manifest", f"workflow profile drifted: {expected_id}")
        profiles[expected_mode] = GuestWorkflowProfile(
            profile_id=expected_id,
            mode=expected_mode,
            kernel_artifact="kernel",
            rootfs_artifact="rootfs",
            initrd_artifact="guest-boot-initrd",
            boot_args=item["boot_args"],
            rootfs_read_only=True,
            success_marker=item["success_marker"],
            failure_marker=item["failure_marker"],
            implementation=implementation,
            validation=validation,
        )

    evidence = _require_object(root["evidence"], ("implementation", "validation", "documentation"), "evidence")
    for key in evidence:
        for reference in _require_list(evidence[key], f"evidence.{key}"):
            _require_reference(reference, f"evidence.{key}")

    _require_exact_strings(
        root["nonclaims"],
        (
            "byte-reproducible-ext4",
            "hostile-parent-traversal-safety",
            "artifact-redistribution-or-authentication",
            "arbitrary-url-or-profile-input",
            "production-workflow",
            "external-guest-networking",
            "arbitrary-distro-or-freebsd-guest-support",
            "crash-atomic-image-sidecar-pair",
        ),
        "nonclaims",
    )
    return GuestWorkflowManifest(
        downloads=downloads,
        generated=generated,
        recipes=recipes,
        profiles=profiles,
        guest_identity=guest_identity,
        timeouts=timeouts,
    )


def cache_root() -> Path:
    configured = os.environ.get("BANGBANG_GUEST_ARTIFACTS_DIR")
    root = Path(configured) if configured else REPOSITORY_ROOT / DEFAULT_CACHE_RELATIVE
    return Path(os.path.abspath(os.fspath(root)))


def _ensure_directory(path: Path) -> None:
    try:
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        metadata = os.lstat(path)
    except OSError as error:
        raise ArtifactPolicyError("filesystem", f"failed to create cache directory {path}: {error}") from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise ArtifactPolicyError("filesystem", f"cache parent is not a directory: {path}")


def _classify(path: Path) -> str:
    try:
        mode = os.lstat(path).st_mode
    except FileNotFoundError:
        return "absent"
    except OSError as error:
        raise ArtifactPolicyError("filesystem", f"failed to inspect output path {path}: {error}") from error
    if stat.S_ISREG(mode):
        return "regular"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISFIFO(mode):
        return "fifo"
    if stat.S_ISSOCK(mode):
        return "socket"
    return "nonregular"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            while True:
                chunk = source.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
    except OSError as error:
        raise ArtifactPolicyError("filesystem", f"failed to hash artifact {path}: {error}") from error
    return digest.hexdigest()


def _matches(path: Path, size_bytes: int, sha256: str) -> bool:
    try:
        if os.lstat(path).st_size != size_bytes:
            return False
    except OSError as error:
        raise ArtifactPolicyError("filesystem", f"failed to inspect artifact {path}: {error}") from error
    return _sha256(path) == sha256


def _same_bytes(left: Path, right: Path) -> bool:
    try:
        if os.lstat(left).st_size != os.lstat(right).st_size:
            return False
    except OSError as error:
        raise ArtifactPolicyError("filesystem", f"failed to compare output bytes: {error}") from error
    return _sha256(left) == _sha256(right)


def _sync_file(path: Path) -> None:
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    except OSError as error:
        raise ArtifactPolicyError("sync", f"failed to sync artifact {path}: {error}") from error


def _sync_directory(path: Path) -> None:
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    except OSError as error:
        raise ArtifactPolicyError("sync", f"failed to sync artifact directory {path}: {error}") from error


def _owned_temp_file(parent: Path, name: str, suffix: str) -> tuple[Path, OwnedPath]:
    try:
        descriptor, raw_path = tempfile.mkstemp(prefix=f".{name}.", suffix=suffix, dir=parent)
        os.fchmod(descriptor, STAGE_MODE)
        os.close(descriptor)
        path = Path(raw_path)
        return path, OwnedPath.capture(path)
    except OSError as error:
        raise ArtifactPolicyError("filesystem", f"failed to create private artifact stage: {error}") from error


def _unlink_owned(owned: Optional[OwnedPath]) -> None:
    if owned is None:
        return
    if not owned.still_owned():
        if _classify(owned.path) == "absent":
            return
        raise ArtifactPolicyError("cleanup-uncertain", f"owned staging path changed identity: {owned.path}")
    try:
        os.unlink(owned.path)
    except FileNotFoundError:
        return
    except OSError as error:
        raise ArtifactPolicyError("cleanup", f"failed to remove owned staging file {owned.path}: {error}") from error


class CacheLock:
    """A persistent filename whose held advisory lock alone denotes ownership."""

    def __init__(self, target: Path) -> None:
        self.path = target.parent / f".{target.name}.lock"
        self.descriptor: Optional[int] = None

    def __enter__(self) -> "CacheLock":
        flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(self.path, flags, STAGE_MODE)
            metadata = os.fstat(descriptor)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid():
                raise ArtifactPolicyError("lock", f"cache lock is not an owned regular file: {self.path}")
            os.fchmod(descriptor, STAGE_MODE)
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                if error.errno in (errno.EACCES, errno.EAGAIN):
                    raise ArtifactPolicyError("busy", f"guest artifact cache is busy: {self.path}") from error
                raise
            self.descriptor = descriptor
            return self
        except ArtifactPolicyError:
            if "descriptor" in locals():
                os.close(descriptor)
            raise
        except OSError as error:
            if "descriptor" in locals():
                os.close(descriptor)
            raise ArtifactPolicyError("lock", f"failed to acquire cache lock {self.path}: {error}") from error

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        descriptor = self.descriptor
        self.descriptor = None
        if descriptor is not None:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            finally:
                os.close(descriptor)


def _run_child(
    arguments: Sequence[str],
    *,
    env: Optional[Mapping[str, str]] = None,
    timeout: Optional[int] = None,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(arguments),
            check=False,
            env=None if env is None else dict(env),
            timeout=timeout,
            text=True,
            stdout=subprocess.PIPE if capture else sys.stderr,
            stderr=subprocess.STDOUT if capture else None,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ArtifactPolicyError("child", f"failed to execute {Path(arguments[0]).name}: {error}") from error


def fetch_artifact(
    artifact_id: str,
    *,
    manifest: Optional[GuestWorkflowManifest] = None,
    root: Optional[Path] = None,
    runner: Callable[..., subprocess.CompletedProcess[str]] = _run_child,
    stderr: TextIO = sys.stderr,
) -> Path:
    """Fetch or reuse one fixed manifest-owned download cache."""

    policy = manifest or load_manifest()
    if artifact_id not in policy.downloads:
        raise ArtifactPolicyError("invocation", f"unknown checked artifact: {artifact_id}")
    artifact = policy.downloads[artifact_id]
    target = (root or cache_root()) / artifact.cache_path
    _ensure_directory(target.parent)

    with CacheLock(target):
        kind = _classify(target)
        if kind not in ("absent", "regular"):
            raise ArtifactPolicyError(
                "collision",
                f"cached {artifact_id} artifact path is {kind}, not a regular file: {target}",
            )
        if kind == "regular" and _matches(target, artifact.size_bytes, artifact.sha256):
            if stat.S_IMODE(os.lstat(target).st_mode) != FILE_MODE:
                print(f"normalizing cached {artifact_id} artifact permissions: {target}", file=stderr)
                os.chmod(target, FILE_MODE, follow_symlinks=False)
                _sync_file(target)
                _sync_directory(target.parent)
            print(f"using cached Firecracker {artifact_id} artifact: {target}", file=stderr)
            return target
        if kind == "regular":
            print(
                f"cached Firecracker {artifact_id} artifact failed size/SHA-256 verification; repairing: {target}",
                file=stderr,
            )

        stage, owned_stage = _owned_temp_file(target.parent, target.name, ".download")
        try:
            print(f"fetching Firecracker {artifact_id} artifact: {artifact.url}", file=stderr)
            result = runner(
                (
                    "curl",
                    "--fail",
                    "--location",
                    "--show-error",
                    "--silent",
                    "--retry",
                    "3",
                    "--connect-timeout",
                    "10",
                    "--max-time",
                    "600",
                    "--retry-max-time",
                    "600",
                    "--output",
                    os.fspath(stage),
                    artifact.url,
                )
            )
            if result.returncode != 0:
                raise ArtifactPolicyError("download", f"curl failed for checked {artifact_id} artifact")
            _sync_file(stage)
            if not _matches(stage, artifact.size_bytes, artifact.sha256):
                raise ArtifactPolicyError(
                    "verification",
                    f"downloaded Firecracker {artifact_id} artifact failed size/SHA-256 verification",
                )
            os.chmod(stage, FILE_MODE, follow_symlinks=False)
            _sync_file(stage)
            os.replace(stage, target)
            owned_stage = None
            _sync_directory(target.parent)
            return target
        finally:
            _unlink_owned(owned_stage)


def _write_stage(parent: Path, name: str, data: bytes, mode: int = FILE_MODE) -> tuple[Path, OwnedPath]:
    stage, owned = _owned_temp_file(parent, name, ".stage")
    try:
        with stage.open("wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(stage, mode, follow_symlinks=False)
        _sync_file(stage)
        return stage, owned
    except BaseException:
        _unlink_owned(owned)
        raise


def publish_staged_absent(
    stage: Path,
    destination: Path,
    *,
    allow_identical: bool,
) -> str:
    """Atomically publish a same-filesystem regular stage without replacement."""

    if _classify(stage) != "regular":
        raise ArtifactPolicyError("stage", f"publication stage is not a regular file: {stage}")
    stage_owned = OwnedPath.capture(stage)
    _sync_file(stage)
    _ensure_directory(destination.parent)
    existing = _classify(destination)
    if existing != "absent":
        if existing == "regular" and allow_identical and _same_bytes(stage, destination):
            _unlink_owned(stage_owned)
            return "reused"
        raise ArtifactPolicyError(
            "collision",
            f"caller-owned output path is occupied by {existing}: {destination}",
        )

    linked = False
    try:
        try:
            os.link(stage, destination, follow_symlinks=False)
            linked = True
        except FileExistsError as error:
            if allow_identical and _classify(destination) == "regular" and _same_bytes(stage, destination):
                _unlink_owned(stage_owned)
                return "reused"
            raise ArtifactPolicyError(
                "collision", f"caller-owned output appeared during publication: {destination}"
            ) from error
        except OSError as error:
            if error.errno in (
                errno.EXDEV,
                errno.EPERM,
                errno.EACCES,
                getattr(errno, "EOPNOTSUPP", -1),
                getattr(errno, "ENOTSUP", -1),
            ):
                raise ArtifactPolicyError(
                    "unsupported-publication",
                    f"atomic hard-link publication is unsupported for {destination}",
                ) from error
            raise ArtifactPolicyError("publication", f"failed to publish {destination}: {error}") from error

        final_metadata = os.lstat(destination)
        if final_metadata.st_dev != stage_owned.device or final_metadata.st_ino != stage_owned.inode:
            raise ArtifactPolicyError("publication", f"published output identity changed: {destination}")
        _sync_directory(destination.parent)
        _unlink_owned(stage_owned)
        return "published"
    except BaseException:
        if linked:
            try:
                final_metadata = os.lstat(destination)
                if final_metadata.st_dev == stage_owned.device and final_metadata.st_ino == stage_owned.inode:
                    os.unlink(destination)
                    _sync_directory(destination.parent)
            except FileNotFoundError:
                pass
        raise


def publish_generated_bytes(
    destination: Path,
    data: bytes,
    *,
    managed_cache: bool,
    artifact_id: str = "guest-boot-initrd",
    manifest: Optional[GuestWorkflowManifest] = None,
    stderr: TextIO = sys.stderr,
) -> str:
    """Publish deterministic generated bytes under cache or caller ownership."""

    policy = manifest or load_manifest()
    if artifact_id not in policy.generated:
        raise ArtifactPolicyError("invocation", f"unknown generated artifact: {artifact_id}")
    expected = policy.generated[artifact_id]
    digest = hashlib.sha256(data).hexdigest()
    if len(data) != expected.size_bytes or digest != expected.sha256:
        raise ArtifactPolicyError(
            "generated-drift",
            f"generated {artifact_id} bytes do not match the checked manifest",
        )
    destination = Path(os.path.abspath(os.fspath(destination)))
    _ensure_directory(destination.parent)

    if not managed_cache:
        stage, owned_stage = _write_stage(destination.parent, destination.name, data)
        try:
            result = publish_staged_absent(stage, destination, allow_identical=True)
            owned_stage = None
            return result
        finally:
            _unlink_owned(owned_stage)

    with CacheLock(destination):
        kind = _classify(destination)
        if kind not in ("absent", "regular"):
            raise ArtifactPolicyError(
                "collision", f"generated cache path is {kind}, not a regular file: {destination}"
            )
        if kind == "regular" and _matches(destination, expected.size_bytes, expected.sha256):
            if stat.S_IMODE(os.lstat(destination).st_mode) != FILE_MODE:
                print(f"normalizing generated cache permissions: {destination}", file=stderr)
                os.chmod(destination, FILE_MODE, follow_symlinks=False)
                _sync_file(destination)
                _sync_directory(destination.parent)
            return "reused"
        if kind == "regular":
            print(f"generated cache bytes changed; refreshing: {destination}", file=stderr)
        stage, owned_stage = _write_stage(destination.parent, destination.name, data)
        try:
            os.replace(stage, destination)
            owned_stage = None
            _sync_directory(destination.parent)
            return "published" if kind == "absent" else "refreshed"
        finally:
            _unlink_owned(owned_stage)


def parse_ext4_size(value: str, minimum: int = 1024) -> tuple[str, int]:
    match = re.fullmatch(r"([0-9]+)([KkMmGgTt]?)", value)
    if match is None:
        raise ArtifactPolicyError("invocation", f"invalid ext4 size: {value}")
    number = int(match.group(1))
    suffix = match.group(2).upper()
    multiplier = {"": 1, "K": 1024, "M": 1024**2, "G": 1024**3, "T": 1024**4}[suffix]
    size_bytes = number * multiplier
    if size_bytes < minimum or size_bytes > MAX_EXT4_BYTES:
        raise ArtifactPolicyError("invocation", f"ext4 size is outside the checked bounds: {value}")
    return value, size_bytes


def _find_executable(name: str) -> Optional[Path]:
    found = shutil.which(name)
    return Path(found) if found else None


def _checked_executable(path: Path, label: str) -> Path:
    path = Path(os.path.abspath(os.fspath(path)))
    try:
        mode = os.stat(path).st_mode
    except OSError as error:
        raise ArtifactPolicyError("tool", f"{label} is unavailable: {path}: {error}") from error
    if not stat.S_ISREG(mode) or not os.access(path, os.X_OK):
        raise ArtifactPolicyError("tool", f"{label} is not a regular executable: {path}")
    return path


def _brew_e2fsprogs_prefix() -> Optional[Path]:
    brew = _find_executable("brew")
    if brew is None:
        return None
    result = _run_child((os.fspath(brew), "--prefix", "e2fsprogs"), timeout=10, capture=True)
    if result.returncode != 0 or result.stdout is None:
        return None
    text = result.stdout.strip()
    return Path(text) if text and "\n" not in text else None


def _tool_version(path: Path, arguments: Sequence[str]) -> str:
    result = _run_child((os.fspath(path), *arguments), timeout=10, capture=True)
    output = result.stdout or ""
    lines = [" ".join(line.split()) for line in output.splitlines() if line.strip()]
    if result.returncode not in (0, 1) or not lines:
        raise ArtifactPolicyError("tool", f"failed to determine bounded {path.name} version")
    version = lines[0]
    if (
        len(version.encode("utf-8")) > 160
        or any(ord(character) < 0x20 for character in version)
        or os.fspath(path) in version
        or os.fspath(path.parent) in version
    ):
        raise ArtifactPolicyError("tool", f"{path.name} returned an invalid version identity")
    return version


def discover_ext4_tools() -> ToolSet:
    unsquashfs = _find_executable("unsquashfs")
    if unsquashfs is None:
        raise ArtifactPolicyError("tool", "unsquashfs is required to prepare an ext4 rootfs; install squashfs")

    override = os.environ.get("BANGBANG_MKFS_EXT4")
    mkfs_ext4 = Path(override) if override else _find_executable("mkfs.ext4")
    prefix: Optional[Path] = None
    if mkfs_ext4 is None:
        prefix = _brew_e2fsprogs_prefix()
        if prefix is not None:
            candidate = prefix / "sbin/mkfs.ext4"
            if candidate.exists():
                mkfs_ext4 = candidate
    if mkfs_ext4 is None:
        raise ArtifactPolicyError("tool", "mkfs.ext4 is required to prepare an ext4 rootfs; install e2fsprogs")
    mkfs_ext4 = _checked_executable(mkfs_ext4, "mkfs.ext4")

    sibling = mkfs_ext4.parent / "e2fsck"
    e2fsck = sibling if sibling.exists() else _find_executable("e2fsck")
    if e2fsck is None:
        prefix = prefix or _brew_e2fsprogs_prefix()
        if prefix is not None:
            candidate = prefix / "sbin/e2fsck"
            if candidate.exists():
                e2fsck = candidate
    if e2fsck is None:
        raise ArtifactPolicyError("tool", "e2fsck is required to validate a prepared ext4 rootfs; install e2fsprogs")
    e2fsck = _checked_executable(e2fsck, "e2fsck")
    unsquashfs = _checked_executable(unsquashfs, "unsquashfs")

    versions = {
        "unsquashfs": _tool_version(unsquashfs, ("-version",)),
        "mkfs.ext4": _tool_version(mkfs_ext4, ("-V",)),
        "e2fsck": _tool_version(e2fsck, ("-V",)),
    }
    return ToolSet(
        unsquashfs=unsquashfs,
        mkfs_ext4=mkfs_ext4,
        e2fsck=e2fsck,
        versions=versions,
    )


def _canonical_json(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True) + "\n").encode("utf-8")


def _recipe_digest(recipe: Ext4Recipe) -> str:
    inputs = []
    for relative in recipe.tracked_inputs:
        path = REPOSITORY_ROOT / relative
        if _classify(path) != "regular":
            raise ArtifactPolicyError("recipe", f"tracked ext4 recipe input is not regular: {relative}")
        inputs.append({"path": relative.as_posix(), "sha256": _sha256(path)})
    envelope = {"recipe": recipe.raw, "tracked_inputs": inputs}
    return hashlib.sha256(_canonical_json(envelope)).hexdigest()


def _read_sidecar(path: Path) -> Optional[dict[str, Any]]:
    try:
        data = path.read_bytes()
        parsed = json.loads(data, object_pairs_hook=_duplicate_safe_object)
    except ArtifactPolicyError:
        return None
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return None
    expected_keys = {
        "schema_version",
        "source_sha256",
        "source_size_bytes",
        "requested_size_bytes",
        "variant",
        "recipe_sha256",
        "tool_versions",
        "output_sha256",
        "output_size_bytes",
        "filesystem_check",
    }
    if not isinstance(parsed, dict) or set(parsed) != expected_keys:
        return None
    if _canonical_json(parsed) != data:
        return None
    return parsed


def ensure_recipe_cache(
    output: Path,
    sidecar: Path,
    expected: Mapping[str, Any],
    *,
    build_image: Callable[[Path], None],
    filesystem_check: Callable[[Path], bool],
    stderr: TextIO = sys.stderr,
) -> Path:
    """Build/reuse a sidecar-last recipe cache; imported tests inject small recipes."""

    _ensure_directory(output.parent)
    with CacheLock(output):
        output_kind = _classify(output)
        sidecar_kind = _classify(sidecar)
        for path, kind, label in ((output, output_kind, "image"), (sidecar, sidecar_kind, "sidecar")):
            if kind not in ("absent", "regular"):
                raise ArtifactPolicyError("collision", f"prepared ext4 {label} path is {kind}: {path}")

        reason: Optional[str]
        if output_kind == "absent" and sidecar_kind == "absent":
            reason = "missing"
        elif output_kind == "absent" or sidecar_kind == "absent":
            reason = "incomplete image/sidecar pair"
        else:
            parsed = _read_sidecar(sidecar)
            if parsed is None:
                reason = "malformed sidecar"
            elif any(parsed.get(key) != value for key, value in expected.items()):
                reason = "stale recipe inputs"
            elif parsed.get("output_size_bytes") != os.lstat(output).st_size:
                reason = "output size mismatch"
            elif not isinstance(parsed.get("output_sha256"), str) or parsed["output_sha256"] != _sha256(output):
                reason = "output digest mismatch"
            elif not filesystem_check(output):
                reason = "filesystem check failed"
            else:
                if stat.S_IMODE(os.lstat(output).st_mode) != FILE_MODE or stat.S_IMODE(os.lstat(sidecar).st_mode) != FILE_MODE:
                    print(f"normalizing prepared ext4 cache permissions: {output}", file=stderr)
                    os.chmod(output, FILE_MODE, follow_symlinks=False)
                    os.chmod(sidecar, FILE_MODE, follow_symlinks=False)
                    _sync_file(output)
                    _sync_file(sidecar)
                    _sync_directory(output.parent)
                print(f"using prepared ext4 rootfs artifact: {output}", file=stderr)
                return output

        if reason != "missing":
            print(f"prepared ext4 cache is {reason}; repairing: {output}", file=stderr)
        image_stage, owned_image = _owned_temp_file(output.parent, output.name, ".build")
        owned_sidecar: Optional[OwnedPath] = None
        try:
            print(f"preparing ext4 rootfs artifact: {output}", file=stderr)
            build_image(image_stage)
            if _classify(image_stage) != "regular":
                raise ArtifactPolicyError("build", "ext4 builder did not leave a regular image stage")
            _sync_file(image_stage)
            expected_size = expected.get("requested_size_bytes")
            if not isinstance(expected_size, int) or os.lstat(image_stage).st_size != expected_size:
                raise ArtifactPolicyError("verification", "prepared ext4 image has the wrong byte size")
            if not filesystem_check(image_stage):
                raise ArtifactPolicyError("verification", "prepared ext4 image failed e2fsck -fn")
            os.chmod(image_stage, FILE_MODE, follow_symlinks=False)
            _sync_file(image_stage)
            completed = dict(expected)
            completed["output_sha256"] = _sha256(image_stage)
            completed["output_size_bytes"] = os.lstat(image_stage).st_size
            sidecar_stage, owned_sidecar = _write_stage(
                sidecar.parent, sidecar.name, _canonical_json(completed)
            )
            os.replace(image_stage, output)
            owned_image = None
            _sync_directory(output.parent)
            os.replace(sidecar_stage, sidecar)
            owned_sidecar = None
            _sync_directory(sidecar.parent)
            return output
        finally:
            cleanup_error: Optional[BaseException] = None
            for owned in (owned_sidecar, owned_image):
                try:
                    _unlink_owned(owned)
                except BaseException as error:
                    cleanup_error = cleanup_error or error
            if cleanup_error is not None and sys.exc_info()[0] is None:
                raise cleanup_error


def _filesystem_checker(tools: ToolSet, runner: Callable[..., subprocess.CompletedProcess[str]]) -> Callable[[Path], bool]:
    def check(path: Path) -> bool:
        result = runner((os.fspath(tools.e2fsck), "-fn", os.fspath(path)))
        return result.returncode == 0

    return check


def _remove_owned_directory(owned: Optional[OwnedPath]) -> None:
    if owned is None:
        return
    if not owned.still_owned():
        if _classify(owned.path) == "absent":
            return
        raise ArtifactPolicyError("cleanup-uncertain", f"owned extraction directory changed identity: {owned.path}")
    try:
        shutil.rmtree(owned.path)
    except OSError as error:
        raise ArtifactPolicyError("cleanup", f"failed to remove owned extraction directory: {error}") from error


def prepare_ext4(
    size: str,
    variant: str,
    *,
    manifest: Optional[GuestWorkflowManifest] = None,
    root: Optional[Path] = None,
    tools: Optional[ToolSet] = None,
    runner: Callable[..., subprocess.CompletedProcess[str]] = _run_child,
    stderr: TextIO = sys.stderr,
) -> Path:
    """Prepare one fixed rootless ext4 recipe and its sidecar validity marker."""

    policy = manifest or load_manifest()
    recipe_id = "rootfs-ext4" if variant == "normal" else "rootfs-ext4-direct-boot-v109"
    if recipe_id not in policy.recipes or variant not in ("normal", "direct-boot-v109"):
        raise ArtifactPolicyError("invocation", f"unknown checked ext4 variant: {variant}")
    recipe = policy.recipes[recipe_id]
    size_token, size_bytes = parse_ext4_size(size, recipe.minimum_size_bytes)
    root_path = root or cache_root()
    source = fetch_artifact("rootfs", manifest=policy, root=root_path, runner=runner, stderr=stderr)
    tools = tools or discover_ext4_tools()
    output_dir = root_path / "bangbang/rootfs"
    filename = recipe.filename_template.format(size=size_token)
    if Path(filename).name != filename:
        raise ArtifactPolicyError("manifest", "ext4 filename template escaped its cache directory")
    output = output_dir / filename
    sidecar = Path(os.fspath(output) + recipe.sidecar_suffix)
    expected = {
        "schema_version": 1,
        "source_sha256": policy.downloads["rootfs"].sha256,
        "source_size_bytes": policy.downloads["rootfs"].size_bytes,
        "requested_size_bytes": size_bytes,
        "variant": recipe.variant,
        "recipe_sha256": _recipe_digest(recipe),
        "tool_versions": dict(tools.versions),
        "filesystem_check": "e2fsck -fn",
    }
    checker = _filesystem_checker(tools, runner)

    def build(stage: Path) -> None:
        _ensure_directory(output_dir)
        raw_extract = tempfile.mkdtemp(prefix=".ubuntu-24.04.extract.", dir=output_dir)
        extract = Path(raw_extract)
        os.chmod(extract, 0o700)
        owned_extract: Optional[OwnedPath] = OwnedPath.capture(extract)
        try:
            print(f"extracting Firecracker rootfs artifact: {source}", file=stderr)
            result = runner(
                (
                    os.fspath(tools.unsquashfs),
                    "-q",
                    "-no-progress",
                    "-no-xattrs",
                    "-d",
                    os.fspath(extract),
                    os.fspath(source),
                )
            )
            if result.returncode != 0:
                raise ArtifactPolicyError("build", "unsquashfs failed while preparing ext4 rootfs")
            if variant == "direct-boot-v109":
                environment = dict(os.environ)
                environment[DIRECT_POPULATE_ENV] = "1"
                result = runner(
                    (
                        os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),
                        "--internal-populate-direct",
                        os.fspath(extract),
                    ),
                    env=environment,
                )
                if result.returncode != 0:
                    raise ArtifactPolicyError("build", "direct-rootfs population callback failed")
            try:
                with stage.open("r+b") as image:
                    image.truncate(size_bytes)
                    image.flush()
                    os.fsync(image.fileno())
            except OSError as error:
                raise ArtifactPolicyError("build", f"failed to size ext4 image stage: {error}") from error
            result = runner(
                (
                    os.fspath(tools.mkfs_ext4),
                    "-q",
                    "-d",
                    os.fspath(extract),
                    "-F",
                    os.fspath(stage),
                )
            )
            if result.returncode != 0:
                raise ArtifactPolicyError("build", "mkfs.ext4 failed while preparing ext4 rootfs")
        finally:
            _remove_owned_directory(owned_extract)

    return ensure_recipe_cache(
        output,
        sidecar,
        expected,
        build_image=build,
        filesystem_check=checker,
        stderr=stderr,
    )


def _parse_args(arguments: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Apply Bangbang's checked guest-artifact policy.")
    subparsers = parser.add_subparsers(dest="operation", required=True)

    fetch_parser = subparsers.add_parser("fetch", help="Fetch one fixed checked artifact.")
    fetch_parser.add_argument("artifact", choices=("kernel", "rootfs"))

    ext4_parser = subparsers.add_parser("prepare-ext4", help="Prepare one checked ext4 recipe.")
    ext4_parser.add_argument("--size", required=True)
    ext4_parser.add_argument("--variant", required=True, choices=("normal", "direct-boot-v109"))

    publish_parser = subparsers.add_parser("publish", help="Publish a fixed caller-owned output class.")
    publish_parser.add_argument("kind", choices=("signed",))
    publish_parser.add_argument("--stage", type=Path, required=True)
    publish_parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args(arguments)


def main(arguments: Optional[Sequence[str]] = None) -> int:
    try:
        args = _parse_args(arguments)
        if args.operation == "fetch":
            result = fetch_artifact(args.artifact)
        elif args.operation == "prepare-ext4":
            result = prepare_ext4(args.size, args.variant)
        elif args.operation == "publish" and args.kind == "signed":
            publish_staged_absent(
                Path(os.path.abspath(os.fspath(args.stage))),
                Path(os.path.abspath(os.fspath(args.output))),
                allow_identical=False,
            )
            result = Path(os.path.abspath(os.fspath(args.output)))
        else:
            raise ArtifactPolicyError("invocation", "unsupported checked operation")
    except ArtifactPolicyError as error:
        print(f"guest artifact policy: {error.category}: {error}", file=sys.stderr)
        return 1
    print(result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
