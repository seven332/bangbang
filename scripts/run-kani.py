#!/usr/bin/env python3
"""Validate and execute the checked targeted Kani proof manifest."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Callable, Dict, List, Mapping, Sequence, Set, Tuple


ROOT = Path(__file__).resolve().parent.parent
AUTHORITY_PATH = ROOT / "compat/firecracker/v1.16.0/formal-verification-audit.json"
VERSION_COMMAND_LABEL = "cargo kani --version"
LIST_COMMAND_LABEL = "cargo kani list --format json"
EXPECTED_KANI_VERSION = "0.67.0"
EXPECTED_VERSION_OUTPUT = "cargo-kani 0.67.0"
EXPECTED_LIST_FORMAT = "0.1"
EXPECTED_PACKAGES = ["bangbang-pager", "bangbang-runtime"]

HarnessIdentity = Tuple[str, str, str]
Invoke = Callable[[Sequence[str], Path, bool], subprocess.CompletedProcess[str]]


class RunnerError(RuntimeError):
    """A stable targeted-verification runner failure."""


def load_authority(path: Path = AUTHORITY_PATH) -> Dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RunnerError("formal verification authority is unreadable") from error
    if not isinstance(value, dict):
        raise RunnerError("formal verification authority must be a JSON object")
    return value


def canonical_command(record: Mapping[str, Any]) -> List[str]:
    package = record.get("package")
    harness = record.get("harness")
    if not isinstance(package, str) or not isinstance(harness, str):
        raise RunnerError("formal verification record has invalid package or harness")
    return [
        "cargo",
        "kani",
        "--package",
        package,
        "--lib",
        "--harness",
        harness,
        "--exact",
    ]


def validate_authority_for_execution(authority: Mapping[str, Any]) -> List[Mapping[str, Any]]:
    toolchain = authority.get("toolchain")
    execution = authority.get("execution")
    records = authority.get("harnesses")
    if not isinstance(toolchain, dict) or toolchain.get("version") != EXPECTED_KANI_VERSION:
        raise RunnerError("formal verification authority has the wrong Kani version")
    if toolchain.get("list_format_version") != EXPECTED_LIST_FORMAT:
        raise RunnerError("formal verification authority has the wrong Kani list format")
    if not isinstance(execution, dict) or execution.get("packages") != EXPECTED_PACKAGES:
        raise RunnerError("formal verification authority has the wrong package order")
    if execution.get("sequential") is not True:
        raise RunnerError("formal verification authority must execute sequentially")
    if not isinstance(records, list) or not records:
        raise RunnerError("formal verification authority has no harness records")

    identities: Set[HarnessIdentity] = set()
    checked: List[Mapping[str, Any]] = []
    for record in records:
        if not isinstance(record, dict):
            raise RunnerError("formal verification harness record must be an object")
        source = record.get("source")
        package = record.get("package")
        harness = record.get("harness")
        command = record.get("command")
        if not all(isinstance(value, str) for value in [source, package, harness]):
            raise RunnerError("formal verification harness identity is invalid")
        identity = (package, source, harness)
        if identity in identities:
            raise RunnerError("formal verification harness identity is duplicated")
        identities.add(identity)
        if command != canonical_command(record):
            raise RunnerError("formal verification harness command is not canonical")
        checked.append(record)
    return checked


def default_invoke(
    command: Sequence[str], cwd: Path, capture_output: bool
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(command),
        cwd=cwd,
        check=True,
        text=True,
        capture_output=capture_output,
    )


def expected_identities(
    records: Sequence[Mapping[str, Any]], package: str
) -> Set[HarnessIdentity]:
    return {
        (package, str(record["source"]), str(record["harness"]))
        for record in records
        if record["package"] == package
    }


def normalize_reported_source(
    package: str,
    reported: str,
    records: Sequence[Mapping[str, Any]],
) -> str:
    normalized = reported.replace("\\", "/")
    components = [component for component in normalized.split("/") if component not in ["", "."]]
    if ".." in components or not components:
        raise RunnerError("Kani reported an unsafe source path")
    suffix = "/".join(components)
    candidates = {
        str(record["source"])
        for record in records
        if record["package"] == package
        and (
            str(record["source"]) == suffix
            or str(record["source"]).endswith("/" + suffix)
            or suffix.endswith("/" + str(record["source"]))
        )
    }
    if len(candidates) != 1:
        raise RunnerError("Kani source path does not map uniquely within its package")
    return next(iter(candidates))


def compiled_identities(
    document: Mapping[str, Any],
    package: str,
    records: Sequence[Mapping[str, Any]],
) -> Set[HarnessIdentity]:
    expected_count = len(expected_identities(records, package))
    if set(document) != {
        "kani-version",
        "file-version",
        "standard-harnesses",
        "contract-harnesses",
        "contracts",
        "totals",
    }:
        raise RunnerError("Kani list JSON has an unexpected schema")
    if document.get("kani-version") != EXPECTED_KANI_VERSION:
        raise RunnerError("Kani list JSON has the wrong verifier version")
    if document.get("file-version") != EXPECTED_LIST_FORMAT:
        raise RunnerError("Kani list JSON has the wrong file version")
    if document.get("contract-harnesses") != {} or document.get("contracts") != []:
        raise RunnerError("targeted verification does not admit contract harnesses")
    totals = document.get("totals")
    if totals != {
        "standard-harnesses": expected_count,
        "contract-harnesses": 0,
        "functions-under-contract": 0,
    }:
        raise RunnerError("Kani list JSON has stale harness totals")
    standard = document.get("standard-harnesses")
    if not isinstance(standard, dict):
        raise RunnerError("Kani list JSON standard harness map is invalid")

    identities: Set[HarnessIdentity] = set()
    reported_count = 0
    for reported_source, harnesses in standard.items():
        if not isinstance(reported_source, str) or not isinstance(harnesses, list):
            raise RunnerError("Kani list JSON contains an invalid harness entry")
        source = normalize_reported_source(package, reported_source, records)
        for harness in harnesses:
            reported_count += 1
            if not isinstance(harness, str):
                raise RunnerError("Kani list JSON contains a non-string harness")
            identity = (package, source, harness)
            if identity in identities:
                raise RunnerError("Kani list JSON contains a duplicate harness")
            identities.add(identity)
    if reported_count != expected_count:
        raise RunnerError("Kani list JSON did not enumerate every expected harness")
    return identities


def read_list_document(path: Path) -> Mapping[str, Any]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RunnerError("Kani list JSON is unreadable") from error
    if not isinstance(document, dict):
        raise RunnerError("Kani list JSON must be an object")
    return document


def run_verification(
    authority: Mapping[str, Any],
    root: Path = ROOT,
    invoke: Invoke = default_invoke,
) -> None:
    records = validate_authority_for_execution(authority)
    invoke(
        [
            "cargo",
            "run",
            "--package",
            "bangbang-firecracker-capability-audit",
            "--locked",
            "--",
            "validate",
            "--formal-verification-final",
        ],
        root,
        False,
    )
    version = invoke(["cargo", "kani", "--version"], root, True)
    if version.stdout.strip() != EXPECTED_VERSION_OUTPUT:
        raise RunnerError("installed cargo-kani version is not the checked release")
    invoke(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
        root,
        True,
    )

    compiled: Set[HarnessIdentity] = set()
    for package in EXPECTED_PACKAGES:
        with tempfile.TemporaryDirectory(prefix="bangbang-kani-list-") as temporary:
            output_dir = Path(temporary)
            invoke(
                [
                    "cargo",
                    "kani",
                    "--manifest-path",
                    str(root / "Cargo.toml"),
                    "--package",
                    package,
                    "--lib",
                    "list",
                    "--format",
                    "json",
                    "--quiet",
                ],
                output_dir,
                False,
            )
            package_compiled = compiled_identities(
                read_list_document(output_dir / "kani-list.json"), package, records
            )
            if compiled.intersection(package_compiled):
                raise RunnerError("Kani package lists contain a duplicate identity")
            compiled.update(package_compiled)

    expected = {
        (str(record["package"]), str(record["source"]), str(record["harness"]))
        for record in records
    }
    if compiled != expected:
        raise RunnerError("compiled Kani harnesses differ from the checked authority")

    for record in records:
        print(f"verifying {record['id']}", flush=True)
        invoke(canonical_command(record), root, False)


def main() -> int:
    try:
        run_verification(load_authority())
    except (RunnerError, subprocess.CalledProcessError) as error:
        print(f"targeted Kani verification failed: {error}", file=sys.stderr)
        return 1
    print("targeted Kani verification passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
