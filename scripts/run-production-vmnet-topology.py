#!/usr/bin/env python3
"""Run the exact-root, no-Apple-authorization production vmnet topology gate."""

from __future__ import annotations

import argparse
import os
import platform
import plistlib
import re
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn, Sequence


OUTER_NAME = "Bangbang.app"
LAUNCHER_NAME = "bangbang"
PROVIDER_NAME = "bangbang-vmnet-provider"
WORKER_BUNDLE_NAME = "BangbangWorker.app"
WORKER_NAME = "bangbang-worker"
MARKER_NAME = "grant-integration-probe.enabled"
LAUNCHER_IDENTIFIER = "dev.bangbang"
PROVIDER_IDENTIFIER = "dev.bangbang.vmnet-provider"
WORKER_IDENTIFIER = "dev.bangbang.worker"
PROCESS_TIMEOUT = 90.0
CLEANUP_TIMEOUT = 20.0
POLL_INTERVAL = 0.05
DAEMON_LINE = re.compile(r"bangbang daemon pid: ([1-9][0-9]*)\n?\Z")
FIXED_ENVIRONMENT = {"LANG": "C", "LC_ALL": "C"}


class TopologyError(RuntimeError):
    """One fixed-category topology failure."""

    def __init__(self, category: str) -> None:
        super().__init__(category)
        self.category = category


class ClosedArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> NoReturn:
        raise TopologyError("invocation")


@dataclass(frozen=True)
class ProductLayout:
    bundle: Path
    launcher: Path
    provider: Path
    worker_bundle: Path
    worker: Path
    marker: Path

    @classmethod
    def from_bundle(cls, bundle: Path) -> ProductLayout:
        if not bundle.is_absolute() or bundle.name != OUTER_NAME:
            raise TopologyError("invocation")
        helpers = bundle / "Contents" / "Helpers"
        worker_bundle = helpers / WORKER_BUNDLE_NAME
        return cls(
            bundle=bundle,
            launcher=bundle / "Contents" / "MacOS" / LAUNCHER_NAME,
            provider=helpers / PROVIDER_NAME,
            worker_bundle=worker_bundle,
            worker=worker_bundle / "Contents" / "MacOS" / WORKER_NAME,
            marker=worker_bundle / "Contents" / "Resources" / MARKER_NAME,
        )


@dataclass(frozen=True)
class ProcessRecord:
    pid: int
    ppid: int
    state: str
    command: str

    @property
    def name(self) -> str:
        return Path(self.command).name


def _parse_id(value: str) -> int:
    if (
        not value
        or not value.isascii()
        or not value.isdecimal()
        or (value.startswith("0") and value != "0")
    ):
        raise TopologyError("invocation")
    parsed = int(value)
    if not 0 < parsed <= 0xFFFF_FFFF:
        raise TopologyError("invocation")
    return parsed


def _parse_arguments(arguments: Sequence[str] | None) -> argparse.Namespace:
    parser = ClosedArgumentParser(allow_abbrev=False)
    parser.add_argument("--prepared", type=Path, required=True)
    parser.add_argument("--target-uid", type=_parse_id, required=True)
    parser.add_argument("--target-gid", type=_parse_id, required=True)
    return parser.parse_args(arguments)


def _require_platform() -> None:
    if (
        os.getuid() != 0
        or os.geteuid() != 0
        or os.getgid() != 0
        or os.getegid() != 0
    ):
        raise TopologyError("exact-root-required")
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise TopologyError("platform")
    result = subprocess.run(
        ["/usr/sbin/sysctl", "-n", "kern.hv_support"],
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=5,
    )
    if result.returncode != 0 or result.stdout.strip() != b"1":
        raise TopologyError("platform")


def _iter_tree(root: Path) -> list[Path]:
    result = [root]
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise TopologyError("package") from error
        for entry in entries:
            path = Path(entry.path)
            result.append(path)
            try:
                if entry.is_dir(follow_symlinks=False):
                    pending.append(path)
            except OSError as error:
                raise TopologyError("package") from error
    return result


def _validate_prepared(layout: ProductLayout, uid: int, gid: int) -> None:
    required_files = (layout.launcher, layout.provider, layout.worker, layout.marker)
    try:
        paths = _iter_tree(layout.bundle)
        for path in paths:
            metadata = os.lstat(path)
            if stat.S_ISLNK(metadata.st_mode) or metadata.st_uid != uid or metadata.st_gid != gid:
                raise TopologyError("package")
            if metadata.st_mode & 0o022:
                raise TopologyError("package")
            if stat.S_ISREG(metadata.st_mode) and metadata.st_nlink != 1:
                raise TopologyError("package")
            if not stat.S_ISDIR(metadata.st_mode) and not stat.S_ISREG(metadata.st_mode):
                raise TopologyError("package")
        for path in required_files:
            metadata = os.lstat(path)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise TopologyError("package")
        for executable in (layout.launcher, layout.provider, layout.worker):
            if os.lstat(executable).st_mode & 0o111 == 0:
                raise TopologyError("package")
        if (layout.worker_bundle / "Contents" / "embedded.provisionprofile").exists():
            raise TopologyError("package")
        if layout.marker.read_bytes() != b"test-only\n":
            raise TopologyError("package")
    except TopologyError:
        raise
    except OSError as error:
        raise TopologyError("package") from error


def _stage_bundle(source: ProductLayout) -> tuple[Path, ProductLayout]:
    try:
        stage = Path(tempfile.mkdtemp(prefix="bangbang-production-topology.", dir="/private/var/tmp"))
        destination = stage / OUTER_NAME
        copied = subprocess.run(
            ["/usr/bin/ditto", "--noqtn", str(source.bundle), str(destination)],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=60,
        )
        if copied.returncode != 0:
            raise TopologyError("staging")
        for path in _iter_tree(destination):
            metadata = os.lstat(path)
            os.lchown(path, 0, 0)
            if stat.S_ISDIR(metadata.st_mode):
                mode = 0o555
            elif stat.S_ISREG(metadata.st_mode) and metadata.st_mode & 0o111:
                mode = 0o555
            else:
                mode = 0o444
            os.chmod(path, mode, follow_symlinks=False)
        return stage, ProductLayout.from_bundle(destination)
    except TopologyError:
        raise
    except (OSError, subprocess.SubprocessError) as error:
        raise TopologyError("staging") from error


def _publish_stage(stage: Path) -> None:
    try:
        metadata = os.lstat(stage)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != 0
            or metadata.st_gid != 0
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise TopologyError("staging")
        os.chmod(stage, 0o711)
        if stat.S_IMODE(os.lstat(stage).st_mode) != 0o711:
            raise TopologyError("staging")
    except TopologyError:
        raise
    except OSError as error:
        raise TopologyError("staging") from error


def _run_codesign(arguments: Sequence[str], category: str) -> subprocess.CompletedProcess[bytes]:
    try:
        result = subprocess.run(
            ["/usr/bin/codesign", *arguments],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise TopologyError(category) from error
    if result.returncode != 0:
        raise TopologyError(category)
    return result


def _entitlements(path: Path) -> dict[str, object]:
    raw = _run_codesign(
        ["--display", "--entitlements", "-", "--xml", str(path)], "signature"
    ).stdout
    if not raw.strip():
        return {}
    try:
        value = plistlib.loads(raw)
    except (plistlib.InvalidFileException, ValueError) as error:
        raise TopologyError("signature") from error
    if not isinstance(value, dict):
        raise TopologyError("signature")
    return value


def _signature_details(path: Path) -> str:
    result = _run_codesign(["--display", "--verbose=4", str(path)], "signature")
    return result.stderr.decode("utf-8", errors="strict")


def _validate_staged_signatures(layout: ProductLayout) -> None:
    _run_codesign(
        ["--verify", "--deep", "--strict", "--verbose=4", str(layout.bundle)],
        "signature",
    )
    for path, identifier in (
        (layout.bundle, LAUNCHER_IDENTIFIER),
        (layout.provider, PROVIDER_IDENTIFIER),
        (layout.worker_bundle, WORKER_IDENTIFIER),
    ):
        details = _signature_details(path)
        if f"Identifier={identifier}" not in details or "runtime" not in details:
            raise TopologyError("signature")
    if _entitlements(layout.bundle) or _entitlements(layout.provider):
        raise TopologyError("entitlements")
    if _entitlements(layout.worker_bundle) != {
        "com.apple.security.app-sandbox": True,
        "com.apple.security.hypervisor": True,
    }:
        raise TopologyError("entitlements")


def _launcher_arguments(
    layout: ProductLayout,
    uid: int,
    gid: int,
    instance: str,
    worker_arguments: Sequence[str],
    *,
    daemon: bool,
) -> list[str]:
    arguments = [
        "--bangbang-jailer-v1",
        "--id",
        instance,
        "--exec-file",
        str(layout.worker),
        "--uid",
        str(uid),
        "--gid",
        str(gid),
    ]
    if daemon:
        arguments.append("--daemonize")
    arguments.extend(
        [
            "--vmnet-allow",
            "shared",
            "--vmnet-max-interfaces",
            "1",
            "--",
            *worker_arguments,
        ]
    )
    return arguments


def _provider_command(
    layout: ProductLayout,
    uid: int,
    gid: int,
    instance: str,
    worker_arguments: Sequence[str],
    *,
    daemon: bool,
) -> list[str]:
    command = [
        str(layout.provider),
        "--bootstrap-v1",
        "--target-uid",
        str(uid),
        "--target-gid",
        str(gid),
    ]
    if daemon:
        command.append("--daemonize")
    command.append("--")
    command.extend(
        _launcher_arguments(layout, uid, gid, instance, worker_arguments, daemon=daemon)
    )
    return command


def _spawn(command: Sequence[str]) -> subprocess.Popen[bytes]:
    try:
        return subprocess.Popen(
            command,
            cwd="/",
            env=FIXED_ENVIRONMENT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        raise TopologyError("spawn") from error


def _communicate(
    process: subprocess.Popen[bytes],
    timeout: float = PROCESS_TIMEOUT,
    prefix: bytes = b"",
) -> tuple[int, bytes]:
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        _terminate_group(process)
        raise TopologyError("timeout") from error
    return process.returncode, prefix + stdout + stderr


def _wait_ready_line(process: subprocess.Popen[bytes]) -> bytes:
    if process.stdout is None:
        raise TopologyError("readiness")
    expected = b"status: grant integration probe ready\n"
    captured = bytearray()
    deadline = time.monotonic() + PROCESS_TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            status, _output = _communicate(process, prefix=bytes(captured))
            if status != 0:
                raise _provider_failure(status, "readiness")
            raise TopologyError("readiness")
        remaining = max(0.0, deadline - time.monotonic())
        readable, _writable, _exceptional = select.select(
            [process.stdout.fileno()], [], [], min(POLL_INTERVAL, remaining)
        )
        if not readable:
            continue
        line = process.stdout.readline()
        if not line:
            continue
        captured.extend(line)
        if line == expected:
            return bytes(captured)
    raise TopologyError("readiness-timeout")


def _terminate_group(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=2)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        pass


def _assert_redacted(output: bytes, sensitive: Sequence[Path]) -> None:
    decoded = output.decode("utf-8", errors="replace")
    if any(str(value) in decoded for value in sensitive):
        raise TopologyError("redaction")


def _provider_failure(status: int, prefix: str) -> TopologyError:
    categories = {
        10: "configuration",
        11: "protocol",
        12: "process",
        13: "timeout",
        14: "cleanup",
        15: "io",
        16: "authority",
        17: "descriptor",
        18: "bootstrap-descriptor",
        19: "provider-descriptor",
    }
    return TopologyError(f"{prefix}-{categories.get(status, 'exit')}")


def _parse_process_table(raw: str) -> dict[int, ProcessRecord]:
    records: dict[int, ProcessRecord] = {}
    for line in raw.splitlines():
        fields = line.strip().split(maxsplit=3)
        if len(fields) != 4:
            continue
        try:
            pid, ppid = int(fields[0]), int(fields[1])
        except ValueError:
            continue
        if pid <= 0 or ppid < 0 or pid in records:
            continue
        records[pid] = ProcessRecord(pid, ppid, fields[2], fields[3])
    return records


def _process_table() -> dict[int, ProcessRecord]:
    try:
        result = subprocess.run(
            ["/bin/ps", "-axo", "pid=,ppid=,state=,comm="],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
            text=True,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise TopologyError("process") from error
    if result.returncode != 0:
        raise TopologyError("process")
    return _parse_process_table(result.stdout)


def _wait_until(predicate, timeout: float = CLEANUP_TIMEOUT) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(POLL_INTERVAL)
    return predicate()


def _require_child(records: dict[int, ProcessRecord], parent: int, name: str) -> int:
    matches = [
        record.pid
        for record in records.values()
        if record.ppid == parent and record.name == name and not record.state.startswith("Z")
    ]
    if len(matches) != 1:
        raise TopologyError("process")
    return matches[0]


def _wait_absent(pids: Sequence[int]) -> None:
    expected = {pid for pid in pids if pid > 1}
    if not _wait_until(lambda: expected.isdisjoint(_process_table())):
        raise TopologyError("cleanup")


def _stage_process_ids(
    records: dict[int, ProcessRecord], layout: ProductLayout
) -> set[int]:
    executables = {str(layout.launcher), str(layout.provider), str(layout.worker)}
    return {
        record.pid
        for record in records.values()
        if record.pid > 1
        and record.command in executables
    }


def _require_stage_processes_absent(layout: ProductLayout) -> None:
    if not _wait_until(lambda: not _stage_process_ids(_process_table(), layout)):
        raise TopologyError("cleanup")


def _force_stage_process_cleanup(layout: ProductLayout) -> bool:
    initial = _stage_process_ids(_process_table(), layout)
    for value in (signal.SIGTERM, signal.SIGKILL):
        if not initial:
            break
        for pid in sorted(_stage_process_ids(_process_table(), layout), reverse=True):
            try:
                os.kill(pid, value)
            except ProcessLookupError:
                pass
            except OSError as error:
                raise TopologyError("cleanup") from error
        if _wait_until(
            lambda: not _stage_process_ids(_process_table(), layout),
            timeout=2.0,
        ):
            break
    if _stage_process_ids(_process_table(), layout):
        raise TopologyError("cleanup")
    return bool(initial)


def _remove_stage(stage: Path) -> None:
    try:
        shutil.rmtree(stage)
    except OSError as error:
        raise TopologyError("cleanup") from error
    if os.path.lexists(stage):
        raise TopologyError("cleanup")


def _signal_exact(pid: int, value: int) -> None:
    if pid <= 1:
        raise TopologyError("process")
    try:
        os.kill(pid, value)
    except OSError as error:
        raise TopologyError("process") from error


def _run_complete_repeat(layout: ProductLayout, uid: int, gid: int, stage: Path) -> None:
    for cycle in range(2):
        command = _provider_command(
            layout,
            uid,
            gid,
            f"topology-complete-{cycle}",
            ["--bangbang-internal-grant-probe-v1", "vmnet-provider-live"],
            daemon=False,
        )
        process = _spawn(command)
        status, output = _communicate(process)
        _assert_redacted(output, [stage, layout.bundle])
        if status != 0:
            raise _provider_failure(status, "provider-lifecycle")
        _require_stage_processes_absent(layout)


def _run_outer_signal(layout: ProductLayout, uid: int, gid: int, stage: Path) -> None:
    process = _spawn(
        _provider_command(
            layout,
            uid,
            gid,
            "topology-outer-signal",
            ["--bangbang-internal-grant-probe-v1", "vmnet-provider-hold"],
            daemon=False,
        )
    )
    tracked: list[int] = []
    try:
        prefix = _wait_ready_line(process)
        records = _process_table()
        outer = _require_child(records, process.pid, LAUNCHER_NAME)
        worker = _require_child(records, outer, WORKER_NAME)
        tracked.extend((outer, worker))
        _signal_exact(outer, signal.SIGTERM)
        status, output = _communicate(process, prefix=prefix)
        _assert_redacted(output, [stage, layout.bundle])
        if status != 0:
            raise _provider_failure(status, "outer-signal")
        _wait_absent(tracked)
        _require_stage_processes_absent(layout)
    finally:
        _terminate_group(process)


def _run_provider_signal(layout: ProductLayout, uid: int, gid: int, stage: Path) -> None:
    process = _spawn(
        _provider_command(
            layout,
            uid,
            gid,
            "topology-provider-signal",
            ["--bangbang-internal-grant-probe-v1", "vmnet-provider-hold"],
            daemon=False,
        )
    )
    tracked: list[int] = []
    try:
        prefix = _wait_ready_line(process)
        records = _process_table()
        outer = _require_child(records, process.pid, LAUNCHER_NAME)
        worker = _require_child(records, outer, WORKER_NAME)
        tracked.extend((outer, worker))
        _signal_exact(process.pid, signal.SIGTERM)
        status, output = _communicate(process, prefix=prefix)
        _assert_redacted(output, [stage, layout.bundle])
        if status == 0:
            raise TopologyError("provider-signal")
        _wait_absent(tracked)
        _require_stage_processes_absent(layout)
    finally:
        _terminate_group(process)


def _parse_daemon_pid(output: bytes) -> int:
    try:
        decoded = output.decode("ascii")
    except UnicodeDecodeError as error:
        raise TopologyError("daemon-handoff") from error
    matched = DAEMON_LINE.fullmatch(decoded)
    if matched is None:
        raise TopologyError("daemon-handoff")
    return int(matched.group(1))


def _run_daemon(layout: ProductLayout, uid: int, gid: int, stage: Path) -> None:
    public = _spawn(
        _provider_command(
            layout,
            uid,
            gid,
            "topology-daemon",
            [
                "--bangbang-internal-grant-probe-v1",
                "vmnet-provider-hold-daemon",
            ],
            daemon=True,
        )
    )
    tracked: list[int] = []
    try:
        status, output = _communicate(public)
        if status != 0:
            raise _provider_failure(status, "daemon-handoff")
        outer = _parse_daemon_pid(output)
        tracked.append(outer)
        records = _process_table()
        outer_record = records.get(outer)
        if outer_record is None or outer_record.name != LAUNCHER_NAME:
            raise TopologyError("process")
        broker = outer_record.ppid
        if broker > 1:
            tracked.append(broker)
        broker_record = records.get(broker)
        if (
            broker_record is None
            or broker_record.ppid != 1
            or broker_record.name != PROVIDER_NAME
            or broker_record.state.startswith("Z")
        ):
            raise TopologyError("process")
        worker = _require_child(records, outer, WORKER_NAME)
        tracked.append(worker)
        _signal_exact(outer, signal.SIGTERM)
        _wait_absent(tracked)
        _require_stage_processes_absent(layout)
    finally:
        _terminate_group(public)
        for pid in reversed(tracked):
            records = _process_table()
            if pid in records:
                try:
                    _signal_exact(pid, signal.SIGTERM)
                except TopologyError:
                    pass


def run(prepared: Path, uid: int, gid: int) -> None:
    _require_platform()
    source = ProductLayout.from_bundle(prepared)
    _validate_prepared(source, uid, gid)
    stage: Path | None = None
    layout: ProductLayout | None = None
    try:
        stage, layout = _stage_bundle(source)
        _validate_staged_signatures(layout)
        _publish_stage(stage)
        _run_complete_repeat(layout, uid, gid, stage)
        _run_outer_signal(layout, uid, gid, stage)
        _run_provider_signal(layout, uid, gid, stage)
        _run_daemon(layout, uid, gid, stage)
    finally:
        cleanup_failed = False
        forced_process_cleanup = False
        if layout is not None:
            try:
                forced_process_cleanup = _force_stage_process_cleanup(layout)
            except TopologyError:
                cleanup_failed = True
        if stage is not None:
            try:
                _remove_stage(stage)
            except TopologyError:
                cleanup_failed = True
        if forced_process_cleanup or cleanup_failed:
            raise TopologyError("cleanup")


def main(arguments: Sequence[str] | None = None) -> int:
    try:
        options = _parse_arguments(arguments)
        with open("/dev/null", "rb", buffering=0) as source:
            os.dup2(source.fileno(), 0)
        run(options.prepared, options.target_uid, options.target_gid)
    except TopologyError as error:
        print(f"bangbang production vmnet topology proof: {error.category}", file=sys.stderr)
        return 1
    except Exception:
        print("bangbang production vmnet topology proof: internal", file=sys.stderr)
        return 1
    print(
        "bangbang production vmnet topology proof: "
        "provider=passed repeat=passed outer-signal=passed "
        "provider-signal=passed daemon=passed cleanup=passed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
