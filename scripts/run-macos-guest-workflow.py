#!/usr/bin/env python3
"""Run the checked, signed, networkless macOS guest workflow."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
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

from guest_artifact_policy import (
    GuestWorkflowManifest,
    GuestWorkflowProfile,
    WorkflowTimeouts,
    load_manifest,
)


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
API_READY = b"status: API server listening"
NO_API_READY = b"status: VM running without API"
EXPECTED_NO_CONTENT_RESPONSE = (
    b"HTTP/1.1 204 No Content\r\n"
    b"Content-Length: 0\r\n"
    b"Connection: close\r\n"
    b"\r\n"
)
MAX_COMMAND_CAPTURE_BYTES = 64 * 1024
MAX_VMM_DIAGNOSTIC_BYTES = 64 * 1024
MAX_HTTP_REQUEST_BYTES = 32 * 1024
MAX_HTTP_RESPONSE_BYTES = 4096
POLL_SECONDS = 0.02
SESSION_MODE = 0o700
PRIVATE_FILE_MODE = 0o600
SUCCESS_LINE_PREFIX = "bangbang macOS guest workflow"


class WorkflowError(RuntimeError):
    """A stable public workflow failure."""

    def __init__(self, category: str, message: str) -> None:
        super().__init__(message)
        self.category = category


class WorkflowInterrupted(WorkflowError):
    """A requested workflow interruption."""


@dataclass(frozen=True)
class PreparedArtifacts:
    kernel: Path
    rootfs: Path
    initrd: Path


@dataclass(frozen=True)
class CommandOutcome:
    returncode: int
    stdout: bytes
    stderr: bytes
    stdout_truncated: bool
    stderr_truncated: bool


@dataclass(frozen=True)
class WorkflowDependencies:
    preflight: Callable[[WorkflowTimeouts], None]
    prepare_artifacts: Callable[
        [GuestWorkflowProfile, GuestWorkflowManifest, WorkflowTimeouts],
        PreparedArtifacts,
    ]
    build_signed_binary: Callable[[Path, WorkflowTimeouts], None]
    process_factory: Callable[..., Any]
    session_parent: Optional[Path]
    stdout: BinaryIO
    stderr: BinaryIO


class _BoundedCapture:
    def __init__(self, limit: int) -> None:
        self._limit = limit
        self._bytes = bytearray()
        self._truncated = False
        self._error: Optional[BaseException] = None
        self._lock = threading.Lock()

    def append(self, value: bytes) -> None:
        with self._lock:
            self._bytes.extend(value)
            if len(self._bytes) > self._limit:
                del self._bytes[: len(self._bytes) - self._limit]
                self._truncated = True

    def fail(self, error: BaseException) -> None:
        with self._lock:
            self._error = error

    def result(self) -> tuple[bytes, bool, Optional[BaseException]]:
        with self._lock:
            return bytes(self._bytes), self._truncated, self._error


def _pump_capture(stream: BinaryIO, capture: _BoundedCapture) -> None:
    try:
        while True:
            chunk = os.read(stream.fileno(), 4096)
            if not chunk:
                return
            capture.append(chunk)
    except BaseException as error:  # pragma: no cover - defensive pipe failure
        capture.fail(error)


def _signal_process_group(process: Any, signal_number: int) -> None:
    try:
        os.killpg(process.pid, signal_number)
    except ProcessLookupError:
        return
    except OSError as error:
        raise WorkflowError("process", "failed to signal the owned process group") from error


def _terminate_and_reap(process: Any, grace_seconds: float) -> int:
    returncode = process.poll()
    if returncode is None:
        _signal_process_group(process, signal.SIGTERM)
        try:
            return process.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            _signal_process_group(process, signal.SIGKILL)
            try:
                return process.wait(timeout=grace_seconds)
            except subprocess.TimeoutExpired as error:
                raise WorkflowError(
                    "process", "owned process group did not terminate within its deadline"
                ) from error
    return process.wait(timeout=grace_seconds)


def run_bounded_command(
    arguments: Sequence[str],
    *,
    timeout_seconds: float,
    phase: str,
    check: bool = True,
) -> CommandOutcome:
    """Run one repository tool in an owned process group with bounded capture."""

    try:
        process = subprocess.Popen(
            tuple(arguments),
            cwd=REPOSITORY_ROOT,
            env=canonical_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        raise WorkflowError("tool", f"failed to start {phase}") from error
    if process.stdout is None or process.stderr is None:  # pragma: no cover - Popen contract
        _terminate_and_reap(process, 1)
        raise WorkflowError("tool", f"failed to capture {phase}")

    stdout_capture = _BoundedCapture(MAX_COMMAND_CAPTURE_BYTES)
    stderr_capture = _BoundedCapture(MAX_COMMAND_CAPTURE_BYTES)
    threads = (
        threading.Thread(
            target=_pump_capture,
            args=(process.stdout, stdout_capture),
            name=f"bangbang-{phase}-stdout",
            daemon=True,
        ),
        threading.Thread(
            target=_pump_capture,
            args=(process.stderr, stderr_capture),
            name=f"bangbang-{phase}-stderr",
            daemon=True,
        ),
    )
    for thread in threads:
        thread.start()

    deadline = time.monotonic() + timeout_seconds
    try:
        while process.poll() is None:
            if time.monotonic() >= deadline:
                raise WorkflowError("timeout", f"{phase} exceeded its deadline")
            time.sleep(POLL_SECONDS)
        returncode = process.wait(timeout=1)
    except BaseException:
        _terminate_and_reap(process, min(5.0, timeout_seconds))
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

    stdout, stdout_truncated, stdout_error = stdout_capture.result()
    stderr, stderr_truncated, stderr_error = stderr_capture.result()
    if any(thread.is_alive() for thread in threads) or stdout_error or stderr_error:
        raise WorkflowError("tool", f"failed to drain {phase} output")
    outcome = CommandOutcome(
        returncode=returncode,
        stdout=stdout,
        stderr=stderr,
        stdout_truncated=stdout_truncated,
        stderr_truncated=stderr_truncated,
    )
    if check and returncode != 0:
        raise WorkflowError("tool", f"{phase} failed")
    return outcome


def canonical_environment() -> dict[str, str]:
    environment = dict(os.environ)
    environment.pop("BANGBANG_GUEST_ARTIFACTS_DIR", None)
    environment.pop("BANGBANG_GUEST_POLICY_INTERNAL", None)
    return environment


def _command_exists(name: str) -> bool:
    return shutil.which(name) is not None


def _sysctl_value(name: str, timeout_seconds: float) -> Optional[str]:
    outcome = run_bounded_command(
        ("sysctl", "-n", name),
        timeout_seconds=timeout_seconds,
        phase="HVF preflight",
        check=False,
    )
    if outcome.returncode != 0 or outcome.stdout_truncated:
        return None
    try:
        return outcome.stdout.decode("ascii").strip()
    except UnicodeDecodeError:
        return None


def preflight_platform(timeouts: WorkflowTimeouts) -> None:
    if sys.version_info < (3, 9):
        raise WorkflowError("platform", "Python 3.9 or newer is required")
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise WorkflowError(
            "platform", "the macOS guest workflow requires Apple Silicon"
        )
    for command in ("cargo", "codesign", "sysctl"):
        if not _command_exists(command):
            raise WorkflowError("platform", f"required tool is unavailable: {command}")

    support = _sysctl_value("kern.hv_support", timeouts.request_seconds)
    if support is None:
        support = _sysctl_value("kern.hv.supported", timeouts.request_seconds)
    if support != "1":
        raise WorkflowError("platform", "Hypervisor.framework is not supported")
    if _sysctl_value("kern.hv_disable", timeouts.request_seconds) == "1":
        raise WorkflowError("platform", "Hypervisor.framework is disabled")


def _path_from_command(outcome: CommandOutcome, expected: Path, phase: str) -> Path:
    if outcome.stdout_truncated:
        raise WorkflowError("tool", f"{phase} returned oversized output")
    try:
        lines = outcome.stdout.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise WorkflowError("tool", f"{phase} returned invalid output") from error
    if lines != [os.fspath(expected)]:
        raise WorkflowError("tool", f"{phase} returned an unexpected artifact identity")
    return expected


def _verify_regular_artifact(path: Path, size_bytes: int, sha256: str, label: str) -> None:
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise WorkflowError("artifact", f"{label} is unavailable") from error
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size != size_bytes:
        raise WorkflowError("artifact", f"{label} has the wrong object type or size")

    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise WorkflowError("artifact", f"{label} could not be verified") from error
    if digest.hexdigest() != sha256:
        raise WorkflowError("artifact", f"{label} has the wrong digest")


def prepare_artifacts(
    profile: GuestWorkflowProfile,
    manifest: GuestWorkflowManifest,
    timeouts: WorkflowTimeouts,
) -> PreparedArtifacts:
    cache_root = REPOSITORY_ROOT / ".tmp" / "guest-artifacts"
    kernel_spec = manifest.downloads[profile.kernel_artifact]
    rootfs_spec = manifest.downloads[profile.rootfs_artifact]
    initrd_spec = manifest.generated[profile.initrd_artifact]
    kernel = cache_root / kernel_spec.cache_path
    rootfs = cache_root / rootfs_spec.cache_path
    initrd = cache_root / initrd_spec.cache_path

    _path_from_command(
        run_bounded_command(
            (os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-kernel.sh"),),
            timeout_seconds=timeouts.artifact_seconds,
            phase="guest kernel preparation",
        ),
        kernel,
        "guest kernel preparation",
    )
    _path_from_command(
        run_bounded_command(
            (os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),),
            timeout_seconds=timeouts.artifact_seconds,
            phase="guest rootfs preparation",
        ),
        rootfs,
        "guest rootfs preparation",
    )
    _path_from_command(
        run_bounded_command(
            (
                os.fspath(REPOSITORY_ROOT / "scripts/build-guest-boot-initrd.py"),
                "--check",
            ),
            timeout_seconds=timeouts.artifact_seconds,
            phase="guest initrd preparation",
        ),
        initrd,
        "guest initrd preparation",
    )

    _verify_regular_artifact(
        kernel, kernel_spec.size_bytes, kernel_spec.sha256, "guest kernel"
    )
    _verify_regular_artifact(
        rootfs, rootfs_spec.size_bytes, rootfs_spec.sha256, "guest rootfs"
    )
    _verify_regular_artifact(
        initrd, initrd_spec.size_bytes, initrd_spec.sha256, "guest initrd"
    )
    return PreparedArtifacts(kernel=kernel, rootfs=rootfs, initrd=initrd)


def build_signed_binary(output: Path, timeouts: WorkflowTimeouts) -> None:
    outcome = run_bounded_command(
        (
            os.fspath(REPOSITORY_ROOT / "scripts/build-signed-bangbang.sh"),
            "--output",
            os.fspath(output),
        ),
        timeout_seconds=timeouts.build_seconds,
        phase="signed bangbang build",
    )
    _path_from_command(outcome, output, "signed bangbang build")
    try:
        metadata = os.lstat(output)
    except OSError as error:
        raise WorkflowError("build", "signed bangbang output is unavailable") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or not metadata.st_mode & stat.S_IXUSR
    ):
        raise WorkflowError("build", "signed bangbang output has an invalid identity")
    run_bounded_command(
        ("codesign", "--verify", "--strict", "--verbose=2", os.fspath(output)),
        timeout_seconds=timeouts.request_seconds,
        phase="signed bangbang verification",
    )


@dataclass(frozen=True)
class OwnedSession:
    path: Path
    device: int
    inode: int
    uid: int

    @classmethod
    def create(cls, parent: Optional[Path]) -> "OwnedSession":
        parent_path = parent if parent is not None else Path(tempfile.gettempdir())
        path: Optional[Path] = None
        try:
            path = Path(
                tempfile.mkdtemp(prefix="bbgw.", dir=parent_path)
            )
            os.chmod(path, SESSION_MODE)
            metadata = os.lstat(path)
        except OSError as error:
            if path is not None:
                try:
                    os.rmdir(path)
                except OSError:
                    pass
            raise WorkflowError("session", "failed to create the private session") from error
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != SESSION_MODE
        ):
            try:
                os.rmdir(path)
            except OSError:
                pass
            raise WorkflowError("session", "private session identity is invalid")
        return cls(path, metadata.st_dev, metadata.st_ino, metadata.st_uid)

    def _verified_metadata(self) -> Optional[os.stat_result]:
        try:
            metadata = os.lstat(self.path)
        except FileNotFoundError:
            return None
        except OSError as error:
            raise WorkflowError("cleanup", "failed to inspect the private session") from error
        if (
            metadata.st_dev != self.device
            or metadata.st_ino != self.inode
            or metadata.st_uid != self.uid
            or not stat.S_ISDIR(metadata.st_mode)
            or stat.S_IMODE(metadata.st_mode) != SESSION_MODE
        ):
            raise WorkflowError("cleanup", "private session identity changed")
        return metadata

    def cleanup(self) -> None:
        if self._verified_metadata() is None:
            return
        _remove_directory_contents(self.path)
        if self._verified_metadata() is None:
            raise WorkflowError("cleanup", "private session disappeared during cleanup")
        try:
            os.rmdir(self.path)
        except OSError as error:
            raise WorkflowError("cleanup", "failed to remove the private session") from error


def _remove_directory_contents(directory: Path) -> None:
    try:
        entries = list(os.scandir(directory))
    except OSError as error:
        raise WorkflowError("cleanup", "failed to enumerate the private session") from error
    for entry in entries:
        child = Path(entry.path)
        try:
            metadata = entry.stat(follow_symlinks=False)
            if stat.S_ISDIR(metadata.st_mode):
                _remove_directory_contents(child)
                os.rmdir(child)
            else:
                os.unlink(child)
        except OSError as error:
            raise WorkflowError("cleanup", "failed to clean a private session child") from error


def _path_text(path: Path) -> str:
    value = os.fspath(path)
    try:
        value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise WorkflowError("path", "workflow paths must be valid UTF-8") from error
    if any(ord(character) < 0x20 for character in value):
        raise WorkflowError("path", "workflow paths must not contain control characters")
    return value


def _canonical_json(value: Mapping[str, object]) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=True,
        separators=(",", ":"),
    ).encode("ascii")


def machine_config() -> dict[str, object]:
    return {"vcpu_count": 1, "mem_size_mib": 256}


def boot_source(profile: GuestWorkflowProfile, artifacts: PreparedArtifacts) -> dict[str, object]:
    return {
        "kernel_image_path": _path_text(artifacts.kernel),
        "initrd_path": _path_text(artifacts.initrd),
        "boot_args": profile.boot_args,
    }


def root_drive(profile: GuestWorkflowProfile, artifacts: PreparedArtifacts) -> dict[str, object]:
    return {
        "drive_id": "rootfs",
        "path_on_host": _path_text(artifacts.rootfs),
        "is_root_device": True,
        "is_read_only": profile.rootfs_read_only,
    }


def canonical_config_bytes(
    profile: GuestWorkflowProfile, artifacts: PreparedArtifacts
) -> bytes:
    document = {
        "machine-config": machine_config(),
        "boot-source": boot_source(profile, artifacts),
        "drives": [root_drive(profile, artifacts)],
    }
    return _canonical_json(document) + b"\n"


def _write_private_file(path: Path, contents: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    descriptor: Optional[int] = None
    try:
        descriptor = os.open(path, flags, PRIVATE_FILE_MODE)
        offset = 0
        while offset < len(contents):
            written = os.write(descriptor, contents[offset:])
            if written <= 0:
                raise OSError("short private-file write")
            offset += written
        os.fsync(descriptor)
    except OSError as error:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass
            descriptor = None
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
        except OSError:
            pass
        raise WorkflowError("session", "failed to write the private configuration") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)
    metadata = os.lstat(path)
    if not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != PRIVATE_FILE_MODE:
        raise WorkflowError("session", "private configuration identity is invalid")


def http_put_request(path: str, body: Mapping[str, object]) -> bytes:
    body_bytes = _canonical_json(body)
    path_bytes = path.encode("ascii")
    request = b"".join(
        (
            b"PUT ",
            path_bytes,
            b" HTTP/1.1\r\n",
            b"Host: localhost\r\n",
            b"Connection: close\r\n",
            b"Content-Type: application/json\r\n",
            f"Content-Length: {len(body_bytes)}\r\n".encode("ascii"),
            b"\r\n",
            body_bytes,
        )
    )
    if len(request) > MAX_HTTP_REQUEST_BYTES:
        raise WorkflowError("http", "workflow API request exceeds its fixed bound")
    return request


def api_requests(
    profile: GuestWorkflowProfile, artifacts: PreparedArtifacts
) -> tuple[tuple[str, bytes], ...]:
    return (
        ("machine configuration", http_put_request("/machine-config", machine_config())),
        ("boot source", http_put_request("/boot-source", boot_source(profile, artifacts))),
        ("root drive", http_put_request("/drives/rootfs", root_drive(profile, artifacts))),
        (
            "instance start",
            http_put_request("/actions", {"action_type": "InstanceStart"}),
        ),
    )


def _remaining(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise WorkflowError("timeout", "workflow API request exceeded its deadline")
    return remaining


def exchange_api_request(socket_path: Path, request: bytes, timeout_seconds: float) -> None:
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
            chunk = client.recv(1024)
            if not chunk:
                break
            response.extend(chunk)
            if len(response) > MAX_HTTP_RESPONSE_BYTES:
                raise WorkflowError("http", "workflow API response exceeds its fixed bound")
    except WorkflowError:
        raise
    except (OSError, socket.timeout) as error:
        raise WorkflowError("http", "workflow API exchange failed") from error
    finally:
        client.close()
    if bytes(response) != EXPECTED_NO_CONTENT_RESPONSE:
        raise WorkflowError("http", "workflow API returned an unexpected response")


class OutputObserver:
    def __init__(
        self,
        ready_marker: bytes,
        success_marker: bytes,
        failure_marker: bytes,
        stdout: BinaryIO,
        stderr: BinaryIO,
    ) -> None:
        self._ready_marker = ready_marker
        self._success_marker = success_marker
        self._failure_marker = failure_marker
        self._sinks = {"stdout": stdout, "stderr": stderr}
        self._tails = {"stdout": bytearray(), "stderr": bytearray()}
        self._ready = False
        self._success = False
        self._failure = False
        self._pump_error: Optional[BaseException] = None
        self.condition = threading.Condition()

    def feed(self, name: str, chunk: bytes) -> None:
        with self.condition:
            tail = self._tails[name]
            tail.extend(chunk)
            if self._ready_marker in tail:
                self._ready = True
            if self._success_marker in tail:
                self._success = True
            if self._failure_marker in tail:
                self._failure = True
            if len(tail) > MAX_VMM_DIAGNOSTIC_BYTES:
                del tail[: len(tail) - MAX_VMM_DIAGNOSTIC_BYTES]
            self.condition.notify_all()
        try:
            self._sinks[name].write(chunk)
            self._sinks[name].flush()
        except (AttributeError, OSError, ValueError):
            pass

    def fail_pump(self, error: BaseException) -> None:
        with self.condition:
            self._pump_error = error
            self.condition.notify_all()

    def state(self) -> tuple[bool, bool, bool, Optional[BaseException]]:
        with self.condition:
            return self._ready, self._success, self._failure, self._pump_error


def _pump_vmm(stream: BinaryIO, name: str, observer: OutputObserver) -> None:
    try:
        while True:
            chunk = os.read(stream.fileno(), 4096)
            if not chunk:
                return
            observer.feed(name, chunk)
    except BaseException as error:  # pragma: no cover - defensive pipe failure
        observer.fail_pump(error)


class VmmSupervisor:
    def __init__(
        self,
        arguments: Sequence[str],
        profile: GuestWorkflowProfile,
        dependencies: WorkflowDependencies,
    ) -> None:
        try:
            self.process = dependencies.process_factory(
                tuple(arguments),
                cwd=REPOSITORY_ROOT,
                env=canonical_environment(),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
        except OSError as error:
            raise WorkflowError("process", "failed to start the signed VMM") from error
        if self.process.stdout is None or self.process.stderr is None:
            _terminate_and_reap(self.process, 1)
            raise WorkflowError("process", "failed to capture signed VMM output")
        ready = API_READY if profile.mode == "api" else NO_API_READY
        self.observer = OutputObserver(
            ready,
            profile.success_marker.encode("ascii"),
            profile.failure_marker.encode("ascii"),
            dependencies.stdout,
            dependencies.stderr,
        )
        self._threads = (
            threading.Thread(
                target=_pump_vmm,
                args=(self.process.stdout, "stdout", self.observer),
                name="bangbang-workflow-vmm-stdout",
                daemon=True,
            ),
            threading.Thread(
                target=_pump_vmm,
                args=(self.process.stderr, "stderr", self.observer),
                name="bangbang-workflow-vmm-stderr",
                daemon=True,
            ),
        )
        self._finished = False
        for thread in self._threads:
            thread.start()

    def _check_observer(self) -> tuple[bool, bool]:
        ready, success, failure, pump_error = self.observer.state()
        if pump_error is not None:
            raise WorkflowError("process", "failed to read signed VMM output")
        if failure:
            raise WorkflowError("guest", "guest reported rootfs verification failure")
        return ready, success

    def wait_ready(self, timeout_seconds: float) -> None:
        deadline = time.monotonic() + timeout_seconds
        with self.observer.condition:
            while True:
                ready, _success = self._check_observer()
                if ready:
                    return
                returncode = self.process.poll()
                if returncode is not None:
                    raise WorkflowError("process", "signed VMM exited before readiness")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise WorkflowError("timeout", "signed VMM readiness exceeded its deadline")
                self.observer.condition.wait(timeout=min(POLL_SECONDS, remaining))

    def raise_if_failed(self) -> None:
        self._check_observer()
        returncode = self.process.poll()
        if returncode is not None:
            raise WorkflowError("process", "signed VMM exited during API configuration")

    def wait_successful_exit(self, timeout_seconds: float) -> None:
        deadline = time.monotonic() + timeout_seconds
        with self.observer.condition:
            while True:
                _ready, success = self._check_observer()
                returncode = self.process.poll()
                pumps_running = any(thread.is_alive() for thread in self._threads)
                if returncode is not None and not pumps_running:
                    if not success:
                        raise WorkflowError(
                            "guest", "signed VMM exited without the guest success marker"
                        )
                    if returncode != 0:
                        raise WorkflowError("process", "signed VMM exited unsuccessfully")
                    self.process.wait(timeout=1)
                    return
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    if returncode is not None:
                        raise WorkflowError(
                            "timeout", "signed VMM output did not drain within its deadline"
                        )
                    if success:
                        raise WorkflowError(
                            "timeout", "signed VMM did not exit after the guest success marker"
                        )
                    raise WorkflowError("timeout", "guest workflow exceeded its deadline")
                self.observer.condition.wait(timeout=min(POLL_SECONDS, remaining))

    def finish(self, grace_seconds: float) -> None:
        if self._finished:
            return
        self._finished = True
        _terminate_and_reap(self.process, grace_seconds)
        for thread in self._threads:
            thread.join(timeout=grace_seconds)
        for stream in (self.process.stdout, self.process.stderr):
            try:
                stream.close()
            except (OSError, ValueError):
                pass
        for thread in self._threads:
            thread.join(timeout=1)
        if any(thread.is_alive() for thread in self._threads):
            raise WorkflowError("process", "signed VMM output pumps did not stop")


class SocketAbsenceWatcher:
    def __init__(self, path: Path) -> None:
        self._path = path
        self._stop = threading.Event()
        self._observed = threading.Event()
        self._thread = threading.Thread(
            target=self._watch,
            name="bangbang-workflow-no-api-socket",
            daemon=True,
        )

    def _watch(self) -> None:
        while not self._stop.is_set():
            try:
                os.lstat(self._path)
            except FileNotFoundError:
                pass
            except OSError:
                self._observed.set()
                return
            else:
                self._observed.set()
                return
            self._stop.wait(POLL_SECONDS)

    def start(self) -> None:
        self._thread.start()

    def assert_absent(self) -> None:
        if self._observed.is_set():
            raise WorkflowError("socket", "no-api mode published an API socket")
        try:
            os.lstat(self._path)
        except FileNotFoundError:
            return
        except OSError as error:
            raise WorkflowError("socket", "failed to inspect the no-api socket path") from error
        raise WorkflowError("socket", "no-api mode published an API socket")

    def stop(self, timeout_seconds: float) -> None:
        self._stop.set()
        self._thread.join(timeout=timeout_seconds)
        if self._thread.is_alive():
            raise WorkflowError("socket", "no-api socket watcher did not stop")


def _validate_api_socket(path: Path) -> bool:
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        return False
    except OSError as error:
        raise WorkflowError("socket", "failed to inspect the API socket") from error
    if (
        not stat.S_ISSOCK(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) & 0o077
    ):
        raise WorkflowError("socket", "API socket identity or permissions are invalid")
    return True


def wait_for_api_socket(
    path: Path, supervisor: VmmSupervisor, timeout_seconds: float
) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        if _validate_api_socket(path):
            return
        supervisor.raise_if_failed()
        if time.monotonic() >= deadline:
            raise WorkflowError("timeout", "API socket publication exceeded its deadline")
        time.sleep(POLL_SECONDS)


def wait_for_socket_cleanup(path: Path, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            os.lstat(path)
        except FileNotFoundError:
            return
        except OSError as error:
            raise WorkflowError("socket", "failed to inspect API socket cleanup") from error
        if time.monotonic() >= deadline:
            raise WorkflowError("socket", "owned API socket was not cleaned up")
        time.sleep(POLL_SECONDS)


@contextmanager
def interruption_boundary() -> Iterator[None]:
    if threading.current_thread() is not threading.main_thread():
        yield
        return
    previous: dict[int, Any] = {}
    interrupted = False

    def handle(signal_number: int, _frame: object) -> None:
        nonlocal interrupted
        if interrupted:
            return
        interrupted = True
        name = signal.Signals(signal_number).name
        raise WorkflowInterrupted("interrupted", f"workflow interrupted by {name}")

    try:
        for signal_number in (signal.SIGINT, signal.SIGTERM):
            previous[signal_number] = signal.getsignal(signal_number)
            signal.signal(signal_number, handle)
        yield
    finally:
        for signal_number, handler in previous.items():
            signal.signal(signal_number, handler)


def default_dependencies() -> WorkflowDependencies:
    return WorkflowDependencies(
        preflight=preflight_platform,
        prepare_artifacts=prepare_artifacts,
        build_signed_binary=build_signed_binary,
        process_factory=subprocess.Popen,
        session_parent=None,
        stdout=sys.stdout.buffer,
        stderr=sys.stderr.buffer,
    )


def _require_runtime_contract(
    mode: str, manifest: GuestWorkflowManifest
) -> tuple[GuestWorkflowProfile, WorkflowTimeouts]:
    if mode not in ("api", "no-api"):
        raise WorkflowError("usage", "mode must be exactly api or no-api")
    profile = manifest.profiles.get(mode)
    if profile is None or profile.mode != mode:
        raise WorkflowError("manifest", "checked workflow profile is unavailable")
    if manifest.guest_identity is None or manifest.timeouts is None:
        raise WorkflowError("manifest", "checked workflow runtime contract is incomplete")
    return profile, manifest.timeouts


def _status(dependencies: WorkflowDependencies, message: str) -> None:
    try:
        dependencies.stderr.write(f"{SUCCESS_LINE_PREFIX}: {message}\n".encode("utf-8"))
        dependencies.stderr.flush()
    except (AttributeError, OSError, ValueError):
        pass


def run_api_workflow(
    socket_path: Path,
    profile: GuestWorkflowProfile,
    artifacts: PreparedArtifacts,
    supervisor: VmmSupervisor,
    timeouts: WorkflowTimeouts,
) -> None:
    supervisor.wait_ready(timeouts.startup_seconds)
    wait_for_api_socket(socket_path, supervisor, timeouts.request_seconds)
    for _label, request in api_requests(profile, artifacts):
        supervisor.raise_if_failed()
        exchange_api_request(socket_path, request, timeouts.request_seconds)
    supervisor.wait_successful_exit(timeouts.guest_seconds)
    wait_for_socket_cleanup(socket_path, timeouts.request_seconds)


def run_no_api_workflow(
    watcher: SocketAbsenceWatcher,
    supervisor: VmmSupervisor,
    timeouts: WorkflowTimeouts,
) -> None:
    supervisor.wait_ready(timeouts.startup_seconds)
    watcher.assert_absent()
    supervisor.wait_successful_exit(timeouts.guest_seconds)
    watcher.assert_absent()


def run_workflow(
    mode: str,
    *,
    manifest: Optional[GuestWorkflowManifest] = None,
    dependencies: Optional[WorkflowDependencies] = None,
) -> None:
    """Run one checked mode; optional dependencies are import-only test seams."""

    checked_manifest = manifest if manifest is not None else load_manifest()
    profile, timeouts = _require_runtime_contract(mode, checked_manifest)
    runtime = dependencies if dependencies is not None else default_dependencies()

    session: Optional[OwnedSession] = None
    supervisor: Optional[VmmSupervisor] = None
    watcher: Optional[SocketAbsenceWatcher] = None
    cleanup_error: Optional[WorkflowError] = None
    active_error = False

    with interruption_boundary():
        try:
            _status(runtime, "preflight")
            runtime.preflight(timeouts)
            _status(runtime, "preparing pinned guest artifacts")
            artifacts = runtime.prepare_artifacts(profile, checked_manifest, timeouts)

            session = OwnedSession.create(runtime.session_parent)
            signed_binary = session.path / "vmm"
            socket_path = session.path / "a.sock"
            config_path = session.path / "c.json"
            if len(os.fsencode(socket_path)) >= 104:
                raise WorkflowError("session", "private API socket path is too long")

            _status(runtime, "building and signing bangbang")
            runtime.build_signed_binary(signed_binary, timeouts)
            if mode == "no-api":
                _write_private_file(config_path, canonical_config_bytes(profile, artifacts))
                watcher = SocketAbsenceWatcher(socket_path)
                watcher.start()
                arguments = (
                    os.fspath(signed_binary),
                    "--api-sock",
                    os.fspath(socket_path),
                    "--config-file",
                    os.fspath(config_path),
                    "--no-api",
                )
            else:
                arguments = (
                    os.fspath(signed_binary),
                    "--api-sock",
                    os.fspath(socket_path),
                )

            _status(runtime, f"running {mode} mode")
            supervisor = VmmSupervisor(arguments, profile, runtime)
            if mode == "api":
                run_api_workflow(
                    socket_path,
                    profile,
                    artifacts,
                    supervisor,
                    timeouts,
                )
            else:
                if watcher is None:  # pragma: no cover - mode construction invariant
                    raise WorkflowError("socket", "no-api watcher is unavailable")
                run_no_api_workflow(watcher, supervisor, timeouts)
        except BaseException:
            active_error = True
            raise
        finally:
            if supervisor is not None:
                try:
                    supervisor.finish(timeouts.terminate_seconds)
                except WorkflowError as error:
                    cleanup_error = error
            if watcher is not None:
                try:
                    watcher.stop(timeouts.request_seconds)
                except WorkflowError as error:
                    cleanup_error = cleanup_error or error
            if session is not None:
                try:
                    session.cleanup()
                except WorkflowError as error:
                    cleanup_error = cleanup_error or error
            if cleanup_error is not None:
                if active_error:
                    _status(runtime, f"cleanup warning: {cleanup_error.category}")
                else:
                    raise cleanup_error

    runtime.stdout.write(f"{SUCCESS_LINE_PREFIX} ({mode}): success\n".encode("utf-8"))
    runtime.stdout.flush()


def parse_args(arguments: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the checked signed Bangbang macOS guest workflow.",
    )
    parser.add_argument(
        "mode",
        choices=("api", "no-api"),
        help="Use the API socket or canonical --no-api configuration path.",
    )
    return parser.parse_args(arguments)


def main(arguments: Optional[Sequence[str]] = None) -> int:
    args = parse_args(arguments)
    try:
        run_workflow(args.mode)
    except WorkflowError as error:
        print(
            f"{SUCCESS_LINE_PREFIX}: {error.category}: {error}",
            file=sys.stderr,
        )
        return 1
    except OSError:
        print(f"{SUCCESS_LINE_PREFIX}: system: workflow system operation failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
