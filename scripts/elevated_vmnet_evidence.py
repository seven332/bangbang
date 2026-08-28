#!/usr/bin/env python3
"""Create and verify the closed elevated-vmnet evidence package."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path
from typing import Any, NoReturn, Sequence


SCHEMA_VERSION = 1
KIND = "bangbang-elevated-vmnet-evidence"
MANIFEST_NAME = "manifest.json"
LOG_NAME = "prepare.log"
FILES = (
    ("bangbang", 0o555, 512 * 1024 * 1024),
    ("elevated-vmnet-e2e", 0o555, 512 * 1024 * 1024),
    ("bangbang-vmnet-provider", 0o555, 512 * 1024 * 1024),
    ("elevated-vmnet-provider-e2e", 0o555, 512 * 1024 * 1024),
    ("vmlinux-6.1.155", 0o444, 256 * 1024 * 1024),
    ("ubuntu-24.04-512M-direct-boot-v111.ext4", 0o444, 2 * 1024 * 1024 * 1024),
    (
        "ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json",
        0o444,
        64 * 1024,
    ),
    ("ubuntu-24.04-512M-direct-boot-v112.ext4", 0o444, 2 * 1024 * 1024 * 1024),
    (
        "ubuntu-24.04-512M-direct-boot-v112.ext4.bangbang.json",
        0o444,
        64 * 1024,
    ),
    ("elevated-vmnet-evidence.py", 0o444, 256 * 1024),
    ("staged-vmnet-evidence.py", 0o444, 256 * 1024),
    ("staged-vmnet-certification.py", 0o444, 256 * 1024),
)


class EvidenceError(RuntimeError):
    """A fixed-category evidence package failure."""

    def __init__(self, category: str) -> None:
        super().__init__(category)
        self.category = category


class ClosedArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> NoReturn:
        raise EvidenceError("invocation")


def _fail(category: str) -> NoReturn:
    raise EvidenceError(category)


def _duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail("manifest")
        result[key] = value
    return result


def _canonical(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode(
        "ascii"
    )


def _load_json(path: Path, maximum: int) -> tuple[dict[str, Any], bytes]:
    try:
        metadata = os.lstat(path)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_size <= 0
            or metadata.st_size > maximum
        ):
            _fail("manifest")
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=_duplicates)
    except EvidenceError:
        raise
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceError("manifest") from error
    if not isinstance(value, dict) or raw != _canonical(value):
        _fail("manifest")
    return value, raw


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = -1
    try:
        before = os.lstat(path)
        descriptor = os.open(path, flags)
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_dev != before.st_dev
            or opened.st_ino != before.st_ino
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
        ):
            _fail("artifact")
    except EvidenceError:
        raise
    except OSError as error:
        raise EvidenceError("artifact") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    return digest.hexdigest()


def _directory(path: Path, owner: int) -> None:
    if not path.is_absolute() or isinstance(owner, bool) or not 0 <= owner <= 0xFFFF_FFFF:
        _fail("invocation")
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise EvidenceError("package") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or stat.S_IMODE(metadata.st_mode) != 0o700
        or metadata.st_uid != owner
    ):
        _fail("package")


def _file(path: Path, owner: int, mode: int, maximum: int, *, allow_empty: bool = False) -> os.stat_result:
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise EvidenceError("artifact") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_uid != owner
        or stat.S_IMODE(metadata.st_mode) != mode
        or (metadata.st_size == 0 and not allow_empty)
        or metadata.st_size > maximum
    ):
        _fail("artifact")
    return metadata


def _record(root: Path, owner: int, name: str, mode: int, maximum: int) -> dict[str, object]:
    path = root / name
    metadata = _file(path, owner, mode, maximum)
    return {
        "mode": mode,
        "name": name,
        "sha256": _sha256(path),
        "size_bytes": metadata.st_size,
    }


def _validate_sidecars(root: Path, records: list[dict[str, object]]) -> None:
    for variant in ("direct-boot-v111", "direct-boot-v112"):
        filename = f"ubuntu-24.04-512M-{variant}.ext4"
        sidecar, _raw = _load_json(root / f"{filename}.bangbang.json", 64 * 1024)
        rootfs = next(
            (record for record in records if record.get("name") == filename),
            None,
        )
        if (
            rootfs is None
            or sidecar.get("schema_version") != 1
            or sidecar.get("variant") != variant
            or sidecar.get("output_sha256") != rootfs["sha256"]
            or sidecar.get("output_size_bytes") != rootfs["size_bytes"]
        ):
            _fail("sidecar")


def create_manifest(root: Path, owner: int) -> None:
    _directory(root, owner)
    if (root / MANIFEST_NAME).exists() or (root / MANIFEST_NAME).is_symlink():
        _fail("collision")
    _file(root / LOG_NAME, owner, 0o600, 1024 * 1024, allow_empty=True)
    records = [_record(root, owner, *specification) for specification in FILES]
    _validate_sidecars(root, records)
    document = {
        "files": records,
        "kind": KIND,
        "ordinary_denial": "passed",
        "schema_version": SCHEMA_VERSION,
    }
    manifest = root / MANIFEST_NAME
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(manifest, flags, 0o444)
        try:
            data = _canonical(document)
            written = 0
            while written < len(data):
                count = os.write(descriptor, data[written:])
                if count <= 0:
                    _fail("publication")
                written += count
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        os.chmod(manifest, 0o444, follow_symlinks=False)
    except EvidenceError:
        raise
    except OSError as error:
        raise EvidenceError("publication") from error


def verify_manifest(root: Path, owner: int) -> None:
    _directory(root, owner)
    expected_names = {name for name, _mode, _maximum in FILES}
    expected_names.update((MANIFEST_NAME, LOG_NAME))
    try:
        actual_names = {entry.name for entry in root.iterdir()}
    except OSError as error:
        raise EvidenceError("package") from error
    if actual_names != expected_names:
        _fail("package")
    _file(root / LOG_NAME, owner, 0o600, 0, allow_empty=True)
    _file(root / MANIFEST_NAME, owner, 0o444, 256 * 1024)
    document, _raw = _load_json(root / MANIFEST_NAME, 256 * 1024)
    if set(document) != {"files", "kind", "ordinary_denial", "schema_version"}:
        _fail("manifest")
    if (
        document["schema_version"] != SCHEMA_VERSION
        or document["kind"] != KIND
        or document["ordinary_denial"] != "passed"
    ):
        _fail("manifest")
    records = document["files"]
    if not isinstance(records, list) or len(records) != len(FILES):
        _fail("manifest")
    actual_records = []
    for index, specification in enumerate(FILES):
        record = records[index]
        if not isinstance(record, dict) or set(record) != {
            "mode",
            "name",
            "sha256",
            "size_bytes",
        }:
            _fail("manifest")
        actual = _record(root, owner, *specification)
        if record != actual:
            _fail("artifact")
        actual_records.append(actual)
    _validate_sidecars(root, actual_records)


def _owner(value: str) -> int:
    if not value or (value.startswith("0") and value != "0") or not value.isascii() or not value.isdecimal():
        _fail("invocation")
    parsed = int(value)
    if not 0 <= parsed <= 0xFFFF_FFFF:
        _fail("invocation")
    return parsed


def _parse(arguments: Sequence[str] | None) -> argparse.Namespace:
    parser = ClosedArgumentParser(allow_abbrev=False)
    subparsers = parser.add_subparsers(dest="operation", required=True)
    for operation in ("create", "verify"):
        child = subparsers.add_parser(operation, allow_abbrev=False)
        child.add_argument("--directory", type=Path, required=True)
        child.add_argument("--owner", type=_owner, required=True)
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    try:
        options = _parse(arguments)
        if options.operation == "create":
            create_manifest(options.directory, options.owner)
        else:
            verify_manifest(options.directory, options.owner)
    except EvidenceError as error:
        print(f"bangbang elevated vmnet evidence: {error.category}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
