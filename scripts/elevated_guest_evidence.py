#!/usr/bin/env python3
"""Prepare and verify the closed elevated-guest evidence resource set."""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import os
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NoReturn


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
CONTRACT_PATH = (
    REPOSITORY_ROOT
    / "compat"
    / "firecracker"
    / "v1.16.0"
    / "elevated-guest-evidence.json"
)
EXPECTED_BOOT_ARGS = (
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 "
    "rdinit=/rootfs-poweroff-init"
)
EXPECTED_GRANT_REFERENCES = {
    "kernel_image_path": "bangbang-grant:evidence-guest-kernel",
    "initrd_path": "bangbang-grant:evidence-guest-initrd",
    "rootfs": "bangbang-grant:evidence-guest-rootfs",
    "logger": "bangbang-grant:evidence-guest-logger",
    "metrics": "bangbang-grant:evidence-guest-metrics",
    "serial": "bangbang-grant:evidence-guest-serial",
}


class EvidenceError(RuntimeError):
    """Value-free evidence preparation or verification failure."""


@dataclass(frozen=True)
class ResourceSpec:
    resource_id: str
    source: Path
    bundle_name: str
    size_bytes: int
    sha256: str
    mode: int


@dataclass(frozen=True)
class EvidenceContract:
    resources: tuple[ResourceSpec, ...]
    marker_name: str
    marker_contents: bytes
    marker_mode: int
    sidecar_suffix: str


def _fail(message: str) -> NoReturn:
    raise EvidenceError(message)


def _object(value: Any, keys: tuple[str, ...], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or tuple(value) != keys:
        _fail(f"{label} has an invalid closed shape")
    return value


def _array(value: Any, length: int, label: str) -> list[Any]:
    if not isinstance(value, list) or len(value) != length:
        _fail(f"{label} has an invalid closed shape")
    return value


def _text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or "\0" in value:
        _fail(f"{label} is invalid")
    return value


def _integer(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        _fail(f"{label} is invalid")
    return value


def _digest(value: Any, label: str) -> str:
    value = _text(value, label)
    if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
        _fail(f"{label} is invalid")
    return value


def _relative(value: Any, label: str) -> Path:
    text = _text(value, label)
    path = Path(text)
    if path.is_absolute() or text != path.as_posix() or any(part in ("", ".", "..") for part in path.parts):
        _fail(f"{label} is invalid")
    return path


def _mode(value: Any, label: str) -> int:
    text = _text(value, label)
    if len(text) != 4 or text[0] != "0" or any(character not in "01234567" for character in text):
        _fail(f"{label} is invalid")
    return int(text, 8)


def _load_json(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                _fail("checked JSON contains a duplicate key")
            result[key] = value
        return result

    try:
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=reject_duplicates)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceError("checked JSON could not be read") from error
    if not isinstance(value, dict):
        _fail("checked JSON root is invalid")
    canonical = (json.dumps(value, ensure_ascii=True, indent=2) + "\n").encode("ascii")
    if raw != canonical:
        _fail("checked JSON is not canonical")
    return value


def _audit_resources(audit_path: Path) -> dict[tuple[str, str], tuple[Path, int, str]]:
    audit = _load_json(audit_path)
    artifacts = _array(audit.get("artifacts"), 2, "guest artifacts")
    generated = _array(audit.get("generated"), 1, "generated guest artifacts")
    cache_root = Path(
        os.environ.get(
            "BANGBANG_GUEST_ARTIFACTS_DIR",
            os.fspath(REPOSITORY_ROOT / ".tmp" / "guest-artifacts"),
        )
    )
    result: dict[tuple[str, str], tuple[Path, int, str]] = {}
    for section, values in (("artifacts", artifacts), ("generated", generated)):
        for index, raw in enumerate(values):
            if not isinstance(raw, dict):
                _fail(f"{section}[{index}] is invalid")
            resource_id = _text(raw.get("id"), f"{section}[{index}].id")
            cache_path = _relative(raw.get("cache_path"), f"{section}[{index}].cache_path")
            size_bytes = _integer(raw.get("size_bytes"), f"{section}[{index}].size_bytes")
            sha256 = _digest(raw.get("sha256"), f"{section}[{index}].sha256")
            result[(section, resource_id)] = (cache_root / cache_path, size_bytes, sha256)
    return result


def load_contract() -> EvidenceContract:
    raw = _object(
        _load_json(CONTRACT_PATH),
        (
            "schema_version",
            "guest_workflow_audit",
            "resources",
            "marker",
            "replacement_sidecar_suffix",
        ),
        "elevated guest contract",
    )
    if raw["schema_version"] != 1:
        _fail("elevated guest contract version is invalid")
    audit_relative = _relative(raw["guest_workflow_audit"], "guest_workflow_audit")
    audit = _audit_resources(REPOSITORY_ROOT / audit_relative)
    resources = []
    expected_ids = ("kernel", "rootfs", "guest-boot-initrd", "no-api-config")
    expected_names = (
        "evidence-guest-kernel",
        "evidence-guest-rootfs",
        "evidence-guest-initrd",
        "evidence-guest-no-api.json",
    )
    for index, item_raw in enumerate(_array(raw["resources"], 4, "resources")):
        if not isinstance(item_raw, dict):
            _fail(f"resources[{index}] is invalid")
        resource_id = _text(item_raw.get("id"), f"resources[{index}].id")
        bundle_name = _text(item_raw.get("bundle_name"), f"resources[{index}].bundle_name")
        if resource_id != expected_ids[index] or bundle_name != expected_names[index]:
            _fail("elevated guest resource order or identity drifted")
        mode = _mode(item_raw.get("mode"), f"resources[{index}].mode")
        if mode != 0o400:
            _fail("elevated guest input mode drifted")
        if resource_id == "no-api-config":
            item = _object(
                item_raw,
                ("id", "source_path", "bundle_name", "size_bytes", "sha256", "mode"),
                f"resources[{index}]",
            )
            source = REPOSITORY_ROOT / _relative(item["source_path"], "config source_path")
            size_bytes = _integer(item["size_bytes"], "config size_bytes")
            sha256 = _digest(item["sha256"], "config sha256")
        else:
            item = _object(
                item_raw,
                ("id", "audit_section", "bundle_name", "mode"),
                f"resources[{index}]",
            )
            section = _text(item["audit_section"], f"resources[{index}].audit_section")
            try:
                source, size_bytes, sha256 = audit[(section, resource_id)]
            except KeyError as error:
                raise EvidenceError("elevated guest audit reference is invalid") from error
        resources.append(
            ResourceSpec(resource_id, source, bundle_name, size_bytes, sha256, mode)
        )
    marker = _object(raw["marker"], ("bundle_name", "contents", "mode"), "marker")
    marker_name = _text(marker["bundle_name"], "marker.bundle_name")
    marker_contents = _text(marker["contents"], "marker.contents").encode("utf-8")
    marker_mode = _mode(marker["mode"], "marker.mode")
    sidecar_suffix = _text(raw["replacement_sidecar_suffix"], "sidecar suffix")
    if (
        marker_name != "elevated-guest-evidence.enabled"
        or marker_contents != b"test-only\n"
        or marker_mode != 0o600
        or sidecar_suffix != ".elevated-guest-sidecar"
    ):
        _fail("elevated guest marker or sidecar policy drifted")
    contract = EvidenceContract(
        tuple(resources), marker_name, marker_contents, marker_mode, sidecar_suffix
    )
    _validate_canonical_config(contract.resources[-1])
    return contract


def _sha256_descriptor(descriptor: int) -> str:
    digest = hashlib.sha256()
    try:
        os.lseek(descriptor, 0, os.SEEK_SET)
        while chunk := os.read(descriptor, 1024 * 1024):
            digest.update(chunk)
    except OSError as error:
        raise EvidenceError("elevated guest resource could not be read") from error
    return digest.hexdigest()


def _verify_file(path: Path, size_bytes: int, sha256: str, mode: int) -> None:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise EvidenceError("elevated guest resource is unavailable") from error
    try:
        before = os.fstat(descriptor)
        digest = _sha256_descriptor(descriptor)
        after = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_nlink != 1
            or before.st_size != size_bytes
            or stat.S_IMODE(before.st_mode) != mode
            or digest != sha256
            or (
                before.st_dev,
                before.st_ino,
                before.st_uid,
                before.st_gid,
                before.st_mode,
                before.st_nlink,
                before.st_size,
            )
            != (
                after.st_dev,
                after.st_ino,
                after.st_uid,
                after.st_gid,
                after.st_mode,
                after.st_nlink,
                after.st_size,
            )
        ):
            _fail("elevated guest resource identity is invalid")
    finally:
        os.close(descriptor)


def _validate_canonical_config(spec: ResourceSpec) -> None:
    _verify_file(spec.source, spec.size_bytes, spec.sha256, 0o644)
    try:
        document = json.loads(spec.source.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise EvidenceError("canonical no-API configuration is invalid") from error
    expected = {
        "machine-config": {"vcpu_count": 1, "mem_size_mib": 128},
        "boot-source": {
            "kernel_image_path": EXPECTED_GRANT_REFERENCES["kernel_image_path"],
            "initrd_path": EXPECTED_GRANT_REFERENCES["initrd_path"],
            "boot_args": EXPECTED_BOOT_ARGS,
        },
        "drives": [
            {
                "drive_id": "rootfs",
                "path_on_host": EXPECTED_GRANT_REFERENCES["rootfs"],
                "is_root_device": True,
                "is_read_only": True,
            }
        ],
        "metrics": {"metrics_path": EXPECTED_GRANT_REFERENCES["metrics"]},
        "logger": {
            "log_path": EXPECTED_GRANT_REFERENCES["logger"],
            "level": "Info",
            "show_level": True,
            "show_log_origin": True,
        },
        "serial": {"serial_out_path": EXPECTED_GRANT_REFERENCES["serial"]},
    }
    if document != expected:
        _fail("canonical no-API configuration drifted")


def verify_sources(contract: EvidenceContract) -> None:
    for spec in contract.resources:
        source_mode = 0o644 if spec.resource_id == "no-api-config" else stat.S_IMODE(spec.source.lstat().st_mode)
        _verify_file(spec.source, spec.size_bytes, spec.sha256, source_mode)


def _open_destination(path: Path, mode: int) -> int:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    return os.open(path, flags, mode)


def _copy_exact(spec: ResourceSpec, destination: Path) -> None:
    descriptor = _open_destination(destination, spec.mode)
    try:
        with spec.source.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                view = memoryview(chunk)
                while view:
                    written = os.write(descriptor, view)
                    if written <= 0:
                        _fail("elevated guest resource copy was short")
                    view = view[written:]
        os.fchmod(descriptor, spec.mode)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    _verify_file(destination, spec.size_bytes, spec.sha256, spec.mode)


def _verify_directory(directory: Path, contract: EvidenceContract, include_marker: bool) -> None:
    expected = {spec.bundle_name for spec in contract.resources}
    if include_marker:
        expected.add(contract.marker_name)
    try:
        present = {entry.name for entry in directory.iterdir()}
    except OSError as error:
        raise EvidenceError("elevated guest resource directory is unavailable") from error
    if include_marker:
        unexpected_evidence = {
            name
            for name in present
            if (name.startswith("evidence-guest-") or name == contract.marker_name)
            and name not in expected
        }
        if unexpected_evidence or not expected.issubset(present):
            _fail("elevated guest bundle resource set is invalid")
    elif present != expected:
        _fail("elevated guest sidecar resource set is invalid")
    for spec in contract.resources:
        _verify_file(
            directory / spec.bundle_name, spec.size_bytes, spec.sha256, spec.mode
        )
    if include_marker:
        marker = directory / contract.marker_name
        _verify_file(
            marker,
            len(contract.marker_contents),
            hashlib.sha256(contract.marker_contents).hexdigest(),
            contract.marker_mode,
        )


def prepare(resources: Path, sidecar: Path, contract: EvidenceContract) -> None:
    if os.geteuid() == 0:
        _fail("elevated guest resources must be prepared by an ordinary user")
    for directory in (resources, sidecar):
        metadata = directory.lstat()
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or any(directory.iterdir())
        ):
            _fail("elevated guest destination must be an empty owned directory")
    verify_sources(contract)
    for spec in contract.resources:
        _copy_exact(spec, resources / spec.bundle_name)
        _copy_exact(spec, sidecar / spec.bundle_name)
    marker_path = resources / contract.marker_name
    descriptor = _open_destination(marker_path, contract.marker_mode)
    try:
        remaining = memoryview(contract.marker_contents)
        while remaining:
            written = os.write(descriptor, remaining)
            if written <= 0:
                _fail("elevated guest marker copy was short")
            remaining = remaining[written:]
        os.fchmod(descriptor, contract.marker_mode)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    _verify_directory(resources, contract, include_marker=True)
    _verify_directory(sidecar, contract, include_marker=False)


def _exclusive_rename(source: Path, destination: Path) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    source_bytes = os.fsencode(source)
    destination_bytes = os.fsencode(destination)
    if sys.platform == "darwin":
        rename = libc.renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        result = rename(source_bytes, destination_bytes, 0x0000_0004)
    elif sys.platform.startswith("linux"):
        rename = libc.renameat2
        rename.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        rename.restype = ctypes.c_int
        result = rename(-100, source_bytes, -100, destination_bytes, 0x0000_0001)
    else:
        _fail("exclusive evidence publication is unsupported")
    if result != 0:
        error = ctypes.get_errno()
        if error == errno.EEXIST:
            _fail("elevated guest sidecar publication collided")
        _fail("elevated guest sidecar could not be published")


def publish_sidecar(
    directory: Path, destination: Path, contract: EvidenceContract
) -> None:
    if os.geteuid() == 0:
        _fail("elevated guest sidecar must be published by an ordinary user")
    if not directory.is_absolute() or not destination.is_absolute() or directory == destination:
        _fail("elevated guest sidecar publication path is invalid")
    metadata = directory.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid():
        _fail("elevated guest sidecar staging identity is invalid")
    _verify_directory(directory, contract, include_marker=False)
    _exclusive_rename(directory, destination)
    _verify_directory(destination, contract, include_marker=False)


def cleanup_sidecar(directory: Path, contract: EvidenceContract) -> None:
    if os.geteuid() == 0:
        _fail("elevated guest sidecar must be cleaned by an ordinary user")
    _verify_directory(directory, contract, include_marker=False)
    initial = directory.lstat()
    if not stat.S_ISDIR(initial.st_mode) or initial.st_uid != os.geteuid():
        _fail("elevated guest sidecar cleanup identity is invalid")
    for spec in contract.resources:
        path = directory / spec.bundle_name
        _verify_file(path, spec.size_bytes, spec.sha256, spec.mode)
        path.unlink()
    final = directory.lstat()
    if (
        final.st_dev != initial.st_dev
        or final.st_ino != initial.st_ino
        or final.st_uid != initial.st_uid
        or any(directory.iterdir())
    ):
        _fail("elevated guest sidecar cleanup identity changed")
    directory.rmdir()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("verify-contract")
    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--resources", type=Path, required=True)
    prepare_parser.add_argument("--sidecar", type=Path, required=True)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--directory", type=Path, required=True)
    verify_parser.add_argument("--kind", choices=("bundle", "sidecar"), required=True)
    publish_parser = subparsers.add_parser("publish-sidecar")
    publish_parser.add_argument("--directory", type=Path, required=True)
    publish_parser.add_argument("--destination", type=Path, required=True)
    cleanup_parser = subparsers.add_parser("cleanup-sidecar")
    cleanup_parser.add_argument("--directory", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        contract = load_contract()
        if args.command == "verify-contract":
            pass
        elif args.command == "prepare":
            prepare(args.resources, args.sidecar, contract)
        elif args.command == "verify":
            _verify_directory(args.directory, contract, args.kind == "bundle")
        elif args.command == "publish-sidecar":
            publish_sidecar(args.directory, args.destination, contract)
        elif args.command == "cleanup-sidecar":
            cleanup_sidecar(args.directory, contract)
        else:
            _fail("unknown evidence command")
    except (EvidenceError, OSError) as error:
        print(f"elevated guest evidence: {error}", file=sys.stderr)
        return 1
    print("elevated guest evidence: verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
