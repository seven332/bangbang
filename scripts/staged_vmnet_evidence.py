#!/usr/bin/env python3
"""Run one fixed staged direct-vmnet guest scenario under exact root."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import platform
import secrets
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NoReturn, Optional, Sequence


MAX_HTTP_BYTES = 64 * 1024
MAX_SERIAL_BYTES = 256 * 1024
MAX_PATH_BYTES = 4096
STARTUP_SECONDS = 20.0
REQUEST_SECONDS = 15.0
GUEST_SECONDS = 120.0
FIXTURE_SECONDS = 180.0
CLEANUP_SECONDS = 20.0
POLL_SECONDS = 0.02
FIXED_ENVIRONMENT = {"LANG": "C", "LC_ALL": "C"}
BOOT_ARGS = (
    "console=ttyS0 reboot=k panic=1 quiet loglevel=1 "
    "init=/bangbang-direct-rootfs-init bangbang.staged-vmnet-certification=1"
)
KERNEL_NAME = "vmlinux-6.1.155"
ROOTFS_NAME = "ubuntu-24.04-512M-direct-boot-v112.ext4"
SIDECAR_NAME = ROOTFS_NAME + ".bangbang.json"
BANGBANG_NAME = "bangbang"
API_SOCKET_NAME = "api.sock"
SERIAL_NAME = "serial.out"
ONE_SHOT_CONTROL_NAME = "traffic-control.bin"
BARRIER_NAME = "barrier.bin"
SNAPSHOT_STATE_NAME = "snapshot.state"
SNAPSHOT_MEMORY_NAME = "snapshot.memory"
TCP_REQUEST_MAGIC = b"BBVREQ1\0"
TCP_RESPONSE_MAGIC = b"BBVRES1\0"
TCP_RECORD_BYTES = 40
FAILURE_MARKERS = (
    b"BANGBANG_STAGED_VMNET_FAIL_",
    b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_",
)


class EvidenceError(RuntimeError):
    """One value-free staged evidence failure."""

    def __init__(self, category: str) -> None:
        if not isinstance(category, str) or not category.replace("-", "").islower():
            category = "internal"
        super().__init__(category)
        self.category = category


class ClosedArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> NoReturn:
        raise EvidenceError("invocation")


def _load_protocol() -> Any:
    source_root = Path(__file__).resolve().parent
    candidates = (
        source_root / "staged-vmnet-certification.py",
        source_root / "guest/staged_vmnet_certification.py",
    )
    matches = [path for path in candidates if path.is_file() and not path.is_symlink()]
    if len(matches) != 1:
        raise EvidenceError("protocol")
    spec = importlib.util.spec_from_file_location("bangbang_staged_vmnet_protocol", matches[0])
    if spec is None or spec.loader is None:
        raise EvidenceError("protocol")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException as error:
        raise EvidenceError("protocol") from error
    return module


protocol = _load_protocol()


@dataclass(frozen=True)
class Artifacts:
    bangbang: Path
    kernel: Path
    rootfs: Path


def _environment_path(name: str, filename: str, maximum: int) -> Path:
    raw = os.environ.get(name)
    if raw is None:
        raise EvidenceError("artifact")
    path = Path(raw)
    if not path.is_absolute() or path.name != filename or len(os.fsencode(path)) > MAX_PATH_BYTES:
        raise EvidenceError("artifact")
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise EvidenceError("artifact") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_size <= 0
        or metadata.st_size > maximum
        or metadata.st_mode & 0o022
    ):
        raise EvidenceError("artifact")
    return path


def _artifacts() -> Artifacts:
    bangbang = _environment_path(
        "BANGBANG_ELEVATED_VMNET_BANGBANG", BANGBANG_NAME, 512 * 1024 * 1024
    )
    kernel = _environment_path(
        "BANGBANG_ELEVATED_VMNET_KERNEL", KERNEL_NAME, 256 * 1024 * 1024
    )
    rootfs = _environment_path(
        "BANGBANG_STAGED_VMNET_ROOTFS", ROOTFS_NAME, 2 * 1024 * 1024 * 1024
    )
    sidecar = _environment_path(
        "BANGBANG_STAGED_VMNET_ROOTFS_SIDECAR", SIDECAR_NAME, 64 * 1024
    )
    try:
        document = json.loads(sidecar.read_bytes())
    except (OSError, UnicodeDecodeError, ValueError) as error:
        raise EvidenceError("artifact") from error
    if (
        not isinstance(document, dict)
        or document.get("schema_version") != 1
        or document.get("variant") != "direct-boot-v112"
        or document.get("output_size_bytes") != os.lstat(rootfs).st_size
        or not isinstance(document.get("output_sha256"), str)
        or len(document["output_sha256"]) != 64
        or any(byte not in "0123456789abcdef" for byte in document["output_sha256"])
    ):
        raise EvidenceError("artifact")
    try:
        outcome = subprocess.run(
            ("/usr/bin/codesign", "--verify", "--strict", os.fspath(bangbang)),
            check=False,
            cwd="/",
            env=FIXED_ENVIRONMENT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=15,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("artifact") from error
    if outcome.returncode != 0:
        raise EvidenceError("artifact")
    return Artifacts(bangbang, kernel, rootfs)


def _require_platform() -> None:
    if (os.getuid(), os.geteuid(), os.getgid(), os.getegid()) != (0, 0, 0, 0):
        raise EvidenceError("authority")
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise EvidenceError("platform")
    try:
        outcome = subprocess.run(
            ("/usr/sbin/sysctl", "-n", "kern.hv_support"),
            check=False,
            cwd="/",
            env=FIXED_ENVIRONMENT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("platform") from error
    if outcome.returncode != 0 or outcome.stdout.strip() != b"1":
        raise EvidenceError("platform")


class RunRoot:
    def __init__(self) -> None:
        parent = Path(os.environ.get("TMPDIR", ""))
        if not parent.is_absolute() or not parent.is_dir() or parent.is_symlink():
            raise EvidenceError("artifact")
        try:
            self.path = Path(tempfile.mkdtemp(prefix="staged-vmnet.", dir=parent))
            os.chmod(self.path, 0o700)
        except OSError as error:
            raise EvidenceError("artifact") from error

    def child(self, name: str) -> Path:
        return self.path / name

    def cleanup(self) -> None:
        try:
            import shutil

            shutil.rmtree(self.path)
        except FileNotFoundError:
            return
        except OSError as error:
            raise EvidenceError("cleanup") from error
        if os.path.lexists(self.path):
            raise EvidenceError("cleanup")


class Product:
    def __init__(self, binary: Path, socket_path: Path, instance: str) -> None:
        if len(os.fsencode(socket_path)) >= 104:
            raise EvidenceError("artifact")
        try:
            self.process = subprocess.Popen(
                (
                    os.fspath(binary),
                    "--enable-pci",
                    "--api-sock",
                    os.fspath(socket_path),
                    "--id",
                    instance,
                ),
                cwd="/",
                env=FIXED_ENVIRONMENT,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
        except OSError as error:
            raise EvidenceError("process") from error
        self.socket_path = socket_path
        self.closed = False
        deadline = time.monotonic() + STARTUP_SECONDS
        while True:
            if self.process.poll() is not None:
                raise EvidenceError("process")
            client: Optional[socket.socket] = None
            try:
                client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                client.settimeout(0.1)
                client.connect(os.fspath(socket_path))
                client.close()
                break
            except OSError:
                if client is not None:
                    client.close()
            if time.monotonic() >= deadline:
                self.kill()
                raise EvidenceError("process")
            time.sleep(POLL_SECONDS)

    def running(self) -> bool:
        return self.process.poll() is None

    def terminate(self) -> None:
        if self.closed:
            return
        self._stop(signal.SIGTERM)
        if self.process.returncode != 0:
            raise EvidenceError("cleanup")
        self._finish()

    def kill(self) -> None:
        if self.closed:
            return
        self._stop(signal.SIGKILL)
        self._finish(remove_stale_socket=True)

    def _stop(self, number: int) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, number)
            except ProcessLookupError:
                pass
            except OSError as error:
                raise EvidenceError("cleanup") from error
        try:
            self.process.wait(timeout=CLEANUP_SECONDS)
        except subprocess.TimeoutExpired:
            if number != signal.SIGKILL:
                self._stop(signal.SIGKILL)
                return
            raise EvidenceError("cleanup")

    def _finish(self, *, remove_stale_socket: bool = False) -> None:
        if remove_stale_socket and os.path.lexists(self.socket_path):
            try:
                metadata = os.lstat(self.socket_path)
                if not stat.S_ISSOCK(metadata.st_mode) or metadata.st_uid != os.getuid():
                    raise EvidenceError("cleanup")
                os.unlink(self.socket_path)
            except EvidenceError:
                raise
            except OSError as error:
                raise EvidenceError("cleanup") from error
        deadline = time.monotonic() + CLEANUP_SECONDS
        while os.path.lexists(self.socket_path) and time.monotonic() < deadline:
            time.sleep(POLL_SECONDS)
        if os.path.lexists(self.socket_path):
            raise EvidenceError("cleanup")
        self.closed = True


def _http(
    product: Product,
    method: str,
    path: str,
    body: Optional[dict[str, object]] = None,
) -> tuple[int, bytes]:
    if not product.running() or method not in ("PUT", "PATCH", "DELETE"):
        raise EvidenceError("api")
    body_bytes = b"" if body is None else json.dumps(body, separators=(",", ":")).encode("ascii")
    headers = [
        f"{method} {path} HTTP/1.1",
        "Host: localhost",
        "Connection: close",
    ]
    if body is not None:
        headers.extend(("Content-Type: application/json", f"Content-Length: {len(body_bytes)}"))
    request = ("\r\n".join(headers) + "\r\n\r\n").encode("ascii") + body_bytes
    if len(request) > MAX_HTTP_BYTES:
        raise EvidenceError("api")
    response = bytearray()
    client: Optional[socket.socket] = None
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(REQUEST_SECONDS)
        client.connect(os.fspath(product.socket_path))
        client.sendall(request)
        client.shutdown(socket.SHUT_WR)
        while True:
            chunk = client.recv(4096)
            if not chunk:
                break
            response.extend(chunk)
            if len(response) > MAX_HTTP_BYTES:
                raise EvidenceError("api")
    except (OSError, socket.timeout) as error:
        raise EvidenceError("api") from error
    finally:
        if client is not None:
            try:
                client.close()
            except OSError as error:
                raise EvidenceError("api") from error
    return _parse_http_response(bytes(response))


def _parse_http_response(response: bytes) -> tuple[int, bytes]:
    head, separator, payload = response.partition(b"\r\n\r\n")
    lines = head.split(b"\r\n")
    if not separator or not lines:
        raise EvidenceError("api")
    try:
        parts = lines[0].decode("ascii").split(" ", 2)
        status = int(parts[1])
    except (IndexError, UnicodeDecodeError, ValueError) as error:
        raise EvidenceError("api") from error
    lengths = [line[16:] for line in lines[1:] if line.lower().startswith(b"content-length: ")]
    if len(lengths) != 1 or not lengths[0].isdigit() or int(lengths[0]) != len(payload):
        raise EvidenceError("api")
    return status, payload


def _no_content(response: tuple[int, bytes], category: str = "api") -> None:
    if response != (204, b""):
        raise EvidenceError(category)


def _create_file(path: Path, contents: bytes, *, size: Optional[int] = None) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags, 0o600)
        if contents:
            if os.write(descriptor, contents) != len(contents):
                raise EvidenceError("control")
        if size is not None:
            os.ftruncate(descriptor, size)
        os.fsync(descriptor)
        os.close(descriptor)
    except EvidenceError:
        try:
            os.close(descriptor)
        except (OSError, UnboundLocalError):
            pass
        raise
    except OSError as error:
        try:
            os.close(descriptor)
        except (OSError, UnboundLocalError):
            pass
        raise EvidenceError("control") from error


def _traffic_control(port: int, nonce: bytes) -> bytes:
    if not 1 <= port <= 65535 or len(nonce) != 32 or not any(nonce):
        raise EvidenceError("control")
    value = bytearray(512)
    value[:8] = b"BBEVNET2"
    value[8:10] = (2).to_bytes(2, "big")
    value[10] = 1
    value[11] = 1
    value[16:18] = port.to_bytes(2, "big")
    value[18:50] = nonce
    import hashlib

    value[64:96] = hashlib.sha256(value[:64]).digest()
    return bytes(value)


class Barrier:
    def __init__(self, path: Path, scenario: Any, nonce: bytes) -> None:
        self.path = path
        self.scenario = scenario
        self.nonce = nonce
        header = protocol.encode_header(scenario, nonce)
        _create_file(path, header, size=protocol.CONTROL_BYTES)
        self.previous_status: Optional[Any] = None
        self.previous_command_sequence = 0
        self.terminal = False

    def command(self, sequence: int) -> None:
        if (
            self.terminal
            or sequence != self.previous_command_sequence + 1
            or sequence > protocol.COMMAND_COUNTS[self.scenario]
        ):
            raise EvidenceError("control")
        value = protocol.encode_record(
            protocol.ROLE_COMMAND,
            self.scenario,
            protocol.COMMAND_PROCEED,
            sequence,
            self.nonce,
        )
        try:
            descriptor = os.open(self.path, os.O_RDWR | getattr(os, "O_CLOEXEC", 0))
            if os.pwrite(descriptor, value, protocol.COMMAND_OFFSET) != len(value):
                raise EvidenceError("control")
            os.fsync(descriptor)
            os.close(descriptor)
        except EvidenceError:
            try:
                os.close(descriptor)
            except (OSError, UnboundLocalError):
                pass
            raise
        except OSError as error:
            try:
                os.close(descriptor)
            except (OSError, UnboundLocalError):
                pass
            raise EvidenceError("control") from error
        self.previous_command_sequence = sequence

    def wait(self, product: Product, sequence: int, kind: Any) -> None:
        expected = 1 if self.previous_status is None else self.previous_status.sequence + 1
        graph = protocol.STATUS_GRAPHS[self.scenario]
        if (
            self.terminal
            or not isinstance(kind, protocol.Status)
            or sequence != expected
            or sequence > len(graph)
            or kind is not graph[sequence - 1]
        ):
            raise EvidenceError("control")
        deadline = time.monotonic() + GUEST_SECONDS
        while True:
            if not product.running():
                raise EvidenceError("process")
            descriptor = -1
            try:
                descriptor = os.open(
                    self.path,
                    os.O_RDONLY
                    | getattr(os, "O_CLOEXEC", 0)
                    | getattr(os, "O_NOFOLLOW", 0),
                )
                value = os.pread(descriptor, protocol.SECTOR_BYTES, protocol.STATUS_OFFSET)
                record = protocol.decode_record(value, allow_empty=True)
            except protocol.CoordinatorError as error:
                raise EvidenceError("control") from error
            except OSError as error:
                raise EvidenceError("control") from error
            finally:
                if descriptor >= 0:
                    try:
                        os.close(descriptor)
                    except OSError as error:
                        raise EvidenceError("control") from error
            if record is not None:
                if record == self.previous_status:
                    pass
                elif (
                    record.role == protocol.ROLE_STATUS
                    and record.scenario is self.scenario
                    and record.nonce == self.nonce
                    and record.kind in protocol.FAILURE_CATEGORIES
                ):
                    category = protocol.FAILURE_CATEGORIES[record.kind]
                    raise EvidenceError(f"guest-staged-{category}")
                elif (
                    record.role == protocol.ROLE_STATUS
                    and record.scenario is self.scenario
                    and record.kind == int(protocol.Status.FAILED)
                    and record.nonce == self.nonce
                ):
                    raise EvidenceError("guest")
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
                    raise EvidenceError("protocol")
            if time.monotonic() >= deadline:
                label = kind.label if isinstance(kind, protocol.Status) else "status"
                raise EvidenceError(f"guest-{label}-timeout")
            time.sleep(POLL_SECONDS)

    def assert_terminal(self) -> None:
        if (
            not self.terminal
            or self.previous_status is None
            or self.previous_status.kind != int(protocol.Status.COMPLETE)
            or self.previous_status.sequence
            != len(protocol.STATUS_GRAPHS[self.scenario])
            or self.previous_command_sequence != protocol.COMMAND_COUNTS[self.scenario]
        ):
            raise EvidenceError("control")
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
        except OSError as error:
            raise EvidenceError("control") from error
        finally:
            if descriptor >= 0:
                try:
                    os.close(descriptor)
                except OSError as error:
                    raise EvidenceError("control") from error
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_size != protocol.CONTROL_BYTES
            or len(value) != protocol.CONTROL_BYTES
            or any(value[protocol.STATUS_OFFSET + protocol.SECTOR_BYTES :])
        ):
            raise EvidenceError("control")
        try:
            header = protocol.decode_header(value[: protocol.SECTOR_BYTES])
            command = protocol.decode_record(
                value[
                    protocol.COMMAND_OFFSET : protocol.COMMAND_OFFSET
                    + protocol.SECTOR_BYTES
                ]
            )
            status = protocol.decode_record(
                value[
                    protocol.STATUS_OFFSET : protocol.STATUS_OFFSET
                    + protocol.SECTOR_BYTES
                ]
            )
        except protocol.CoordinatorError as error:
            raise EvidenceError("control") from error
        expected_command = protocol.Record(
            protocol.ROLE_COMMAND,
            self.scenario,
            protocol.COMMAND_PROCEED,
            self.previous_command_sequence,
            self.nonce,
        )
        if (
            header
            != protocol.Header(self.scenario, self.scenario.cycles, self.nonce)
            or command != expected_command
            or status != self.previous_status
        ):
            raise EvidenceError("control")


class Fixture:
    def __init__(self, nonce: bytes, expected: int) -> None:
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("0.0.0.0", 0))
        self.listener.listen(16)
        self.listener.settimeout(0.1)
        self.port = self.listener.getsockname()[1]
        self.nonce = nonce
        self.expected = expected
        self.cancelled = threading.Event()
        self.result: Optional[EvidenceError] = None
        self.thread = threading.Thread(target=self._run, name="staged-vmnet-fixture", daemon=True)
        self.thread.start()

    def _run(self) -> None:
        deadline = time.monotonic() + FIXTURE_SECONDS
        valid = 0
        attempts = 0
        try:
            while valid < self.expected:
                if self.cancelled.is_set():
                    raise EvidenceError("fixture")
                try:
                    connection, _address = self.listener.accept()
                except socket.timeout:
                    if time.monotonic() >= deadline:
                        raise EvidenceError("fixture-timeout")
                    continue
                attempts += 1
                if attempts > 16:
                    connection.close()
                    raise EvidenceError("fixture")
                with connection:
                    connection.settimeout(5)
                    request = bytearray()
                    while len(request) < TCP_RECORD_BYTES:
                        chunk = connection.recv(TCP_RECORD_BYTES - len(request))
                        if not chunk:
                            break
                        request.extend(chunk)
                    trailing = connection.recv(1)
                    if (
                        len(request) != TCP_RECORD_BYTES
                        or request[:8] != TCP_REQUEST_MAGIC
                        or request[8:] != self.nonce
                        or trailing
                    ):
                        continue
                    response = TCP_RESPONSE_MAGIC + self.nonce
                    connection.sendall(response)
                    connection.shutdown(socket.SHUT_WR)
                    valid += 1
        except (EvidenceError, OSError) as error:
            self.result = error if isinstance(error, EvidenceError) else EvidenceError("fixture")
        finally:
            self.listener.close()

    def finish(self) -> None:
        self.thread.join(timeout=FIXTURE_SECONDS)
        if self.thread.is_alive():
            raise EvidenceError("fixture-timeout")
        if self.result is not None:
            raise self.result

    def abort(self) -> None:
        self.cancelled.set()
        self.listener.close()
        self.thread.join(timeout=5)


def _configure(
    product: Product,
    artifacts: Artifacts,
    root: RunRoot,
    traffic_control: Path,
    barrier: Path,
    *,
    startup_network: bool,
) -> None:
    for path, body, category in (
        ("/machine-config", {"vcpu_count": 1, "mem_size_mib": 256}, "api-machine"),
        (
            "/boot-source",
            {"kernel_image_path": os.fspath(artifacts.kernel), "boot_args": BOOT_ARGS},
            "api-boot",
        ),
        (
            "/drives/rootfs",
            {
                "drive_id": "rootfs",
                "path_on_host": os.fspath(artifacts.rootfs),
                "is_root_device": True,
                "is_read_only": True,
            },
            "api-rootfs",
        ),
        (
            "/drives/traffic",
            {
                "drive_id": "traffic",
                "path_on_host": os.fspath(traffic_control),
                "is_root_device": False,
                "is_read_only": True,
            },
            "api-traffic",
        ),
        (
            "/drives/barrier",
            {
                "drive_id": "barrier",
                "path_on_host": os.fspath(barrier),
                "is_root_device": False,
                "is_read_only": False,
                "cache_type": "Writeback",
            },
            "api-barrier",
        ),
        (
            "/serial",
            {"serial_out_path": os.fspath(root.child(SERIAL_NAME))},
            "api-serial",
        ),
    ):
        _no_content(_http(product, "PUT", path, body), category)
    if startup_network:
        _network_put(product)
    _no_content(
        _http(product, "PUT", "/actions", {"action_type": "InstanceStart"}),
        "api-start",
    )


def _network_put(product: Product) -> None:
    _no_content(
        _http(
            product,
            "PUT",
            "/network-interfaces/eth0",
            {"iface_id": "eth0", "host_dev_name": "vmnet:shared"},
        ),
        "api-network-put",
    )


def _network_delete(product: Product) -> None:
    _no_content(
        _http(product, "DELETE", "/network-interfaces/eth0"),
        "api-network-delete",
    )


def _check_serial(path: Path) -> None:
    try:
        metadata = os.lstat(path)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_SERIAL_BYTES:
            raise EvidenceError("guest")
        contents = path.read_bytes().replace(b"\r", b"")
    except OSError as error:
        raise EvidenceError("guest") from error
    if any(marker in contents for marker in FAILURE_MARKERS):
        raise EvidenceError("guest")


def _serial_failure(path: Path) -> Optional[EvidenceError]:
    categories = (
        (b"BANGBANG_STAGED_VMNET_FAIL_CONTROL\n", "guest-staged-control"),
        (b"BANGBANG_STAGED_VMNET_FAIL_IO\n", "guest-staged-io"),
        (b"BANGBANG_STAGED_VMNET_FAIL_TOPOLOGY\n", "guest-staged-topology"),
        (b"BANGBANG_STAGED_VMNET_FAIL_TIMEOUT\n", "guest-staged-timeout"),
        (b"BANGBANG_STAGED_VMNET_FAIL_PROCESS\n", "guest-staged-process"),
        (b"BANGBANG_STAGED_VMNET_FAIL_TRAFFIC\n", "guest-staged-traffic"),
        (b"BANGBANG_STAGED_VMNET_FAIL_INTERNAL\n", "guest-staged-internal"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_CONTROL\n", "guest-control"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_INTERFACE\n", "guest-interface"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_DHCP\n", "guest-dhcp"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_CONFIGURE\n", "guest-configure"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_TCP\n", "guest-tcp"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_CLEANUP\n", "guest-cleanup"),
        (b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_INTERNAL\n", "guest-internal"),
    )
    deadline = time.monotonic() + 1.0
    while True:
        try:
            metadata = os.lstat(path)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_SERIAL_BYTES:
                return None
            contents = path.read_bytes().replace(b"\r", b"")
        except OSError:
            return None
        for marker, category in categories:
            if marker in contents:
                return EvidenceError(category)
        if time.monotonic() >= deadline:
            return None
        time.sleep(POLL_SECONDS)


def _run_startup(driver: Product, barrier: Barrier) -> None:
    barrier.wait(driver, 1, protocol.Status.INITIAL_PRESENT)
    barrier.command(1)
    barrier.wait(driver, 2, protocol.Status.TRAFFIC_ONE)
    barrier.command(2)
    barrier.wait(driver, 3, protocol.Status.ABSENT)
    _network_delete(driver)
    _network_put(driver)
    barrier.command(3)
    barrier.wait(driver, 4, protocol.Status.PRESENT)
    barrier.wait(driver, 5, protocol.Status.TRAFFIC_TWO)
    barrier.command(4)
    barrier.wait(driver, 6, protocol.Status.ABSENT)
    _network_delete(driver)
    barrier.command(5)
    barrier.wait(driver, 7, protocol.Status.COMPLETE)


def _run_runtime(driver: Product, barrier: Barrier) -> None:
    barrier.wait(driver, 1, protocol.Status.INITIAL_ABSENT)
    _network_put(driver)
    barrier.command(1)
    barrier.wait(driver, 2, protocol.Status.PRESENT)
    barrier.wait(driver, 3, protocol.Status.TRAFFIC_ONE)
    barrier.command(2)
    barrier.wait(driver, 4, protocol.Status.ABSENT)
    _network_delete(driver)
    barrier.command(3)
    barrier.wait(driver, 5, protocol.Status.COMPLETE)


def _run_restore(
    source: Product,
    barrier: Barrier,
    artifacts: Artifacts,
    root: RunRoot,
) -> Product:
    barrier.wait(source, 1, protocol.Status.INITIAL_PRESENT)
    barrier.command(1)
    barrier.wait(source, 2, protocol.Status.CAPTURE_READY)
    _no_content(_http(source, "PATCH", "/vm", {"state": "Paused"}), "api-pause")
    state = root.child(SNAPSHOT_STATE_NAME)
    memory = root.child(SNAPSHOT_MEMORY_NAME)
    _no_content(
        _http(
            source,
            "PUT",
            "/snapshot/create",
            {
                "snapshot_type": "Full",
                "snapshot_path": os.fspath(state),
                "mem_file_path": os.fspath(memory),
            },
        ),
        "api-snapshot-create",
    )
    source.terminate()
    destination = Product(artifacts.bangbang, root.child("restore-api.sock"), "staged-restore-destination")
    try:
        _no_content(
            _http(
                destination,
                "PUT",
                "/snapshot/load",
                {
                    "snapshot_path": os.fspath(state),
                    "mem_backend": {
                        "backend_path": os.fspath(memory),
                        "backend_type": "File",
                    },
                    "network_overrides": [
                        {"iface_id": "eth0", "host_dev_name": "vmnet:shared"}
                    ],
                    "resume_vm": True,
                },
            ),
            "api-snapshot-load",
        )
        barrier.command(2)
        barrier.wait(destination, 3, protocol.Status.PRESENT)
        barrier.wait(destination, 4, protocol.Status.TRAFFIC_TWO)
        barrier.command(3)
        barrier.wait(destination, 5, protocol.Status.ABSENT)
        _network_delete(destination)
        barrier.command(4)
        barrier.wait(destination, 6, protocol.Status.COMPLETE)
        return destination
    except BaseException:
        destination.kill()
        raise


def run(scenario_name: str) -> None:
    _require_platform()
    artifacts = _artifacts()
    scenarios = {
        "startup": protocol.Scenario.STARTUP,
        "runtime": protocol.Scenario.RUNTIME,
        "restore": protocol.Scenario.RESTORE,
    }
    scenario = scenarios.get(scenario_name)
    if scenario is None:
        raise EvidenceError("invocation")
    root = RunRoot()
    source: Optional[Product] = None
    destination: Optional[Product] = None
    fixture: Optional[Fixture] = None
    first_error: Optional[EvidenceError] = None
    cleanup_error: Optional[EvidenceError] = None
    try:
        _create_file(root.child(SERIAL_NAME), b"")
        nonce = secrets.token_bytes(32)
        if len(nonce) != 32 or not any(nonce):
            raise EvidenceError("control")
        fixture = Fixture(nonce, scenario.cycles)
        traffic_control = root.child(ONE_SHOT_CONTROL_NAME)
        _create_file(traffic_control, _traffic_control(fixture.port, nonce))
        barrier = Barrier(root.child(BARRIER_NAME), scenario, nonce)
        source = Product(artifacts.bangbang, root.child(API_SOCKET_NAME), f"staged-{scenario_name}-source")
        _configure(
            source,
            artifacts,
            root,
            traffic_control,
            barrier.path,
            startup_network=scenario is not protocol.Scenario.RUNTIME,
        )
        if scenario is protocol.Scenario.STARTUP:
            _run_startup(source, barrier)
        elif scenario is protocol.Scenario.RUNTIME:
            _run_runtime(source, barrier)
        else:
            destination = _run_restore(source, barrier, artifacts, root)
            source = None
        fixture.finish()
        fixture = None
        active = destination if destination is not None else source
        if active is None:
            raise EvidenceError("internal")
        active.terminate()
        if active is destination:
            destination = None
        else:
            source = None
        barrier.assert_terminal()
        _check_serial(root.child(SERIAL_NAME))
    except EvidenceError as error:
        first_error = _serial_failure(root.child(SERIAL_NAME)) or error
    except BaseException as error:
        first_error = EvidenceError("internal")
        first_error.__cause__ = error
    finally:
        if fixture is not None:
            fixture.abort()
        for product in (destination, source):
            if product is not None:
                try:
                    product.kill()
                except EvidenceError as error:
                    cleanup_error = cleanup_error or error
        try:
            root.cleanup()
        except EvidenceError as error:
            cleanup_error = cleanup_error or error
    if cleanup_error is not None:
        raise cleanup_error
    if first_error is not None:
        raise first_error


def _parse(arguments: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = ClosedArgumentParser(allow_abbrev=False)
    parser.add_argument("--scenario", choices=("startup", "runtime", "restore"), required=True)
    return parser.parse_args(arguments)


def main(arguments: Optional[Sequence[str]] = None) -> int:
    try:
        options = _parse(arguments)
        run(options.scenario)
    except EvidenceError as error:
        print(f"bangbang staged vmnet proof: failed category={error.category}", file=sys.stderr)
        return 1
    print(f"bangbang staged vmnet proof: scenario={options.scenario} traffic=passed cleanup=passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
