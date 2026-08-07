from __future__ import annotations

import ast
import hashlib
import io
import json
import os
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from dataclasses import replace
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, os.fspath(REPOSITORY_ROOT / "scripts"))

import guest_artifact_policy as policy  # noqa: E402


def load_workflow_module():
    import importlib.util

    path = REPOSITORY_ROOT / "scripts/run-macos-guest-workflow.py"
    spec = importlib.util.spec_from_file_location("macos_guest_workflow", path)
    if spec is None or spec.loader is None:
        raise AssertionError("workflow module should be importable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


workflow = load_workflow_module()


FAKE_VMM_SOURCE = r'''#!/usr/bin/env python3
import json
import os
import signal
import socket
import stat
import struct
import sys
import time

BEHAVIOR = __BEHAVIOR__
RECORD = __RECORD__
SUCCESS = b"BANGBANG_ROOTFS_WORKFLOW_OK\n"
FAILURE = b"BANGBANG_ROOTFS_WORKFLOW_FAIL\n"
RESPONSE = (
    b"HTTP/1.1 204 No Content\r\n"
    b"Content-Length: 0\r\n"
    b"Connection: close\r\n\r\n"
)


def argument(name):
    index = sys.argv.index(name)
    return sys.argv[index + 1]


def record_request(value):
    with open(RECORD, "ab") as target:
        target.write(struct.pack("!I", len(value)))
        target.write(value)
        target.flush()
        os.fsync(target.fileno())


def receive_request(connection):
    value = bytearray()
    expected = None
    while expected is None or len(value) < expected:
        chunk = connection.recv(4096)
        if not chunk:
            break
        value.extend(chunk)
        if len(value) > 32768:
            raise RuntimeError("request too large")
        split = value.find(b"\r\n\r\n")
        if split >= 0:
            headers = bytes(value[:split]).decode("ascii")
            content_length = 0
            for line in headers.split("\r\n")[1:]:
                if line.lower().startswith("content-length:"):
                    content_length = int(line.split(":", 1)[1].strip())
            expected = split + 4 + content_length
    return bytes(value)


def emit_out(value):
    os.write(1, value)


def run_no_api(socket_path):
    config_path = argument("--config-file")
    with open(config_path, "rb") as source:
        config = source.read()
    metadata = os.stat(config_path, follow_symlinks=False)
    with open(RECORD, "w", encoding="utf-8") as target:
        json.dump(
            {"config": config.decode("utf-8"), "mode": stat.S_IMODE(metadata.st_mode)},
            target,
            sort_keys=True,
        )

    if BEHAVIOR == "early-exit":
        return 3
    if BEHAVIOR in ("no-readiness", "hang-ignore-term"):
        if BEHAVIOR == "hang-ignore-term":
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
        while True:
            time.sleep(1)
    if BEHAVIOR == "socket-violation":
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(socket_path)
        os.chmod(socket_path, 0o600)
        server.listen(1)
    emit_out(b"status: VM running without API\n")
    if BEHAVIOR == "split-pressure":
        os.write(2, b"x" * 131072)
        emit_out(SUCCESS[:9])
        emit_out(SUCCESS[9:])
    elif BEHAVIOR == "failure-marker":
        emit_out(FAILURE)
    elif BEHAVIOR != "missing-marker" and BEHAVIOR != "socket-violation":
        emit_out(SUCCESS)
    if BEHAVIOR == "socket-violation":
        time.sleep(0.2)
        server.close()
        os.unlink(socket_path)
    if BEHAVIOR == "nonzero":
        return 7
    return 0


def run_api(socket_path):
    if BEHAVIOR == "early-exit":
        return 3
    if BEHAVIOR in ("no-readiness", "hang-ignore-term"):
        if BEHAVIOR == "hang-ignore-term":
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
        while True:
            time.sleep(1)

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(socket_path)
    os.chmod(socket_path, 0o600)
    server.listen(4)
    emit_out(b"status: API server listening\n")
    try:
        for index in range(4):
            connection, _address = server.accept()
            with connection:
                request = receive_request(connection)
                record_request(request)
                if BEHAVIOR == "wrong-response" and index == 0:
                    connection.sendall(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"
                    )
                elif BEHAVIOR == "oversized-response" and index == 0:
                    connection.sendall(b"x" * 5000)
                else:
                    connection.sendall(RESPONSE)
            if BEHAVIOR in ("wrong-response", "oversized-response"):
                while True:
                    time.sleep(1)
        if BEHAVIOR == "split-pressure":
            os.write(2, b"x" * 131072)
            emit_out(SUCCESS[:11])
            emit_out(SUCCESS[11:])
        elif BEHAVIOR == "failure-marker":
            emit_out(FAILURE)
        elif BEHAVIOR != "missing-marker":
            emit_out(SUCCESS)
    finally:
        server.close()
        try:
            os.unlink(socket_path)
        except FileNotFoundError:
            pass
    if BEHAVIOR == "nonzero":
        return 7
    return 0


def main():
    socket_path = argument("--api-sock")
    if "--no-api" in sys.argv:
        return run_no_api(socket_path)
    return run_api(socket_path)


sys.exit(main())
'''


def recorded_requests(path: Path) -> list[bytes]:
    data = path.read_bytes()
    values = []
    offset = 0
    while offset < len(data):
        if offset + 4 > len(data):
            raise AssertionError("truncated request record")
        length = struct.unpack("!I", data[offset : offset + 4])[0]
        offset += 4
        end = offset + length
        if end > len(data):
            raise AssertionError("truncated recorded request")
        values.append(data[offset:end])
        offset = end
    return values


class FakeBoundary:
    def __init__(self, root: Path, behavior: str = "success") -> None:
        self.root = root
        self.behavior = behavior
        self.sessions = root / "sessions"
        self.sessions.mkdir()
        self.cache = root / "cache"
        self.cache.mkdir()
        self.record = root / "record"
        self.stdout = io.BytesIO()
        self.stderr = io.BytesIO()
        self.processes: list[subprocess.Popen[bytes]] = []
        self.artifacts = workflow.PreparedArtifacts(
            kernel=self._artifact("kernel", b"kernel"),
            rootfs=self._artifact("rootfs", b"rootfs"),
            initrd=self._artifact("initrd", b"initrd"),
        )

    def _artifact(self, name: str, value: bytes) -> Path:
        path = self.cache / name
        path.write_bytes(value)
        return path

    def build(self, output: Path, _timeouts: policy.WorkflowTimeouts) -> None:
        source = FAKE_VMM_SOURCE.replace("__BEHAVIOR__", repr(self.behavior)).replace(
            "__RECORD__", repr(os.fspath(self.record))
        )
        output.write_text(source, encoding="utf-8")
        output.chmod(0o700)

    def spawn(self, *arguments, **keywords):
        process = subprocess.Popen(*arguments, **keywords)
        self.processes.append(process)
        return process

    def dependencies(self) -> workflow.WorkflowDependencies:
        return workflow.WorkflowDependencies(
            preflight=lambda _timeouts: None,
            prepare_artifacts=lambda _profile, _manifest, _timeouts: self.artifacts,
            build_signed_binary=self.build,
            process_factory=self.spawn,
            session_parent=self.sessions,
            stdout=self.stdout,
            stderr=self.stderr,
        )


def fast_manifest(
    *,
    startup: int = 2,
    request: int = 2,
    guest: int = 2,
    terminate: int = 1,
) -> policy.GuestWorkflowManifest:
    manifest = policy.load_manifest()
    return replace(
        manifest,
        timeouts=policy.WorkflowTimeouts(
            artifact_seconds=2,
            build_seconds=2,
            startup_seconds=startup,
            request_seconds=request,
            guest_seconds=guest,
            terminate_seconds=terminate,
        ),
    )


class MacosGuestWorkflowTests(unittest.TestCase):
    def test_public_cli_and_python_39_surface_are_closed(self) -> None:
        self.assertEqual(workflow.parse_args(["api"]).mode, "api")
        self.assertEqual(workflow.parse_args(["no-api"]).mode, "no-api")
        for arguments in (
            [],
            ["custom"],
            ["api", "--manifest", "other.json"],
            ["no-api", "--timeout", "1"],
        ):
            with self.subTest(arguments=arguments):
                with mock.patch("sys.stderr", io.StringIO()):
                    with self.assertRaises(SystemExit) as caught:
                        workflow.parse_args(arguments)
                self.assertEqual(caught.exception.code, 2)

        source = (REPOSITORY_ROOT / "scripts/run-macos-guest-workflow.py").read_text(
            encoding="utf-8"
        )
        ast.parse(source, feature_version=(3, 9))
        self.assertNotIn("BANGBANG_MACOS_GUEST_WORKFLOW", source)

    def test_checked_profiles_and_identity_are_terminal_and_exact(self) -> None:
        manifest = policy.load_manifest()
        self.assertEqual(list(manifest.profiles), ["api", "no-api"])
        self.assertEqual(manifest.guest_identity.path, "/etc/os-release")
        self.assertEqual(manifest.guest_identity.size_bytes, 400)
        self.assertEqual(
            manifest.guest_identity.sha256,
            "3e5851448bae5b36f351becde037a8b13b77307279f484eda808f8177d9a4293",
        )
        for mode, profile in manifest.profiles.items():
            self.assertEqual(profile.mode, mode)
            self.assertEqual(profile.initrd_artifact, "guest-boot-initrd")
            self.assertIn("rdinit=/rootfs-poweroff-init", profile.boot_args)
            self.assertEqual(profile.failure_marker, "BANGBANG_ROOTFS_WORKFLOW_FAIL")

    def test_api_mode_uses_exact_requests_marker_exit_and_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            boundary = FakeBoundary(Path(raw_temp))
            manifest = fast_manifest()
            workflow.run_workflow(
                "api", manifest=manifest, dependencies=boundary.dependencies()
            )

            expected = [
                request
                for _label, request in workflow.api_requests(
                    manifest.profiles["api"], boundary.artifacts
                )
            ]
            self.assertEqual(recorded_requests(boundary.record), expected)
            self.assertIn(b"BANGBANG_ROOTFS_WORKFLOW_OK", boundary.stdout.getvalue())
            self.assertTrue(boundary.stdout.getvalue().endswith(b"(api): success\n"))
            self.assertEqual(list(boundary.sessions.iterdir()), [])
            for artifact in (
                boundary.artifacts.kernel,
                boundary.artifacts.rootfs,
                boundary.artifacts.initrd,
            ):
                self.assertTrue(artifact.is_file(), "shared caches must remain")
            self.assertEqual(boundary.processes[0].returncode, 0)

    def test_no_api_mode_writes_canonical_private_config_and_no_socket(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            boundary = FakeBoundary(Path(raw_temp))
            manifest = fast_manifest()
            workflow.run_workflow(
                "no-api", manifest=manifest, dependencies=boundary.dependencies()
            )

            record = json.loads(boundary.record.read_text(encoding="utf-8"))
            self.assertEqual(record["mode"], 0o600)
            self.assertEqual(
                record["config"].encode("utf-8"),
                workflow.canonical_config_bytes(
                    manifest.profiles["no-api"], boundary.artifacts
                ),
            )
            self.assertTrue(boundary.stdout.getvalue().endswith(b"(no-api): success\n"))
            self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_split_marker_and_pipe_pressure_are_drained(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            boundary = FakeBoundary(Path(raw_temp), "split-pressure")
            workflow.run_workflow(
                "api",
                manifest=fast_manifest(),
                dependencies=boundary.dependencies(),
            )
            self.assertGreaterEqual(len(boundary.stderr.getvalue()), 131072)
            self.assertEqual(boundary.processes[0].returncode, 0)

    def test_process_exit_waits_for_the_final_marker_to_drain(self) -> None:
        original_feed = workflow.OutputObserver.feed

        def delayed_feed(observer, name, chunk):
            if b"BANGBANG_ROOTFS_WORKFLOW_OK" in chunk:
                time.sleep(0.2)
            original_feed(observer, name, chunk)

        with tempfile.TemporaryDirectory() as raw_temp:
            boundary = FakeBoundary(Path(raw_temp))
            with mock.patch.object(workflow.OutputObserver, "feed", delayed_feed):
                workflow.run_workflow(
                    "api",
                    manifest=fast_manifest(),
                    dependencies=boundary.dependencies(),
                )
            self.assertEqual(boundary.processes[0].returncode, 0)
            self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_http_failures_terminate_reap_and_cleanup(self) -> None:
        for behavior, expected in (
            ("wrong-response", "unexpected response"),
            ("oversized-response", "fixed bound"),
        ):
            with self.subTest(behavior=behavior):
                with tempfile.TemporaryDirectory() as raw_temp:
                    boundary = FakeBoundary(Path(raw_temp), behavior)
                    with self.assertRaisesRegex(workflow.WorkflowError, expected):
                        workflow.run_workflow(
                            "api",
                            manifest=fast_manifest(),
                            dependencies=boundary.dependencies(),
                        )
                    self.assertIsNotNone(boundary.processes[0].poll())
                    self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_readiness_and_guest_deadlines_terminate_kill_and_reap(self) -> None:
        for behavior, expected in (
            ("no-readiness", "readiness"),
            ("hang-ignore-term", "readiness"),
        ):
            with self.subTest(behavior=behavior):
                with tempfile.TemporaryDirectory() as raw_temp:
                    boundary = FakeBoundary(Path(raw_temp), behavior)
                    with self.assertRaisesRegex(workflow.WorkflowError, expected):
                        workflow.run_workflow(
                            "no-api",
                            manifest=fast_manifest(startup=1, terminate=1),
                            dependencies=boundary.dependencies(),
                        )
                    process = boundary.processes[0]
                    self.assertIsNotNone(process.poll())
                    if behavior == "hang-ignore-term":
                        self.assertEqual(process.returncode, -signal.SIGKILL)
                    self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_marker_and_exit_failures_are_distinct(self) -> None:
        cases = (
            ("failure-marker", "reported rootfs verification failure"),
            ("missing-marker", "without the guest success marker"),
            ("nonzero", "exited unsuccessfully"),
            ("early-exit", "before readiness"),
        )
        for behavior, expected in cases:
            with self.subTest(behavior=behavior):
                with tempfile.TemporaryDirectory() as raw_temp:
                    boundary = FakeBoundary(Path(raw_temp), behavior)
                    with self.assertRaisesRegex(workflow.WorkflowError, expected):
                        workflow.run_workflow(
                            "no-api",
                            manifest=fast_manifest(),
                            dependencies=boundary.dependencies(),
                        )
                    self.assertIsNotNone(boundary.processes[0].poll())
                    self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_no_api_socket_publication_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            boundary = FakeBoundary(Path(raw_temp), "socket-violation")
            with self.assertRaisesRegex(workflow.WorkflowError, "published an API socket"):
                workflow.run_workflow(
                    "no-api",
                    manifest=fast_manifest(),
                    dependencies=boundary.dependencies(),
                )
            self.assertIsNotNone(boundary.processes[0].poll())
            self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_interrupt_requests_cleanup_and_reaps_the_child(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            boundary = FakeBoundary(Path(raw_temp), "hang-ignore-term")

            def interrupt() -> None:
                deadline = time.monotonic() + 2
                while not boundary.processes and time.monotonic() < deadline:
                    time.sleep(0.01)
                time.sleep(0.1)
                os.kill(os.getpid(), signal.SIGINT)

            sender = threading.Thread(target=interrupt)
            sender.start()
            with self.assertRaisesRegex(workflow.WorkflowInterrupted, "SIGINT"):
                workflow.run_workflow(
                    "no-api",
                    manifest=fast_manifest(startup=5, terminate=1),
                    dependencies=boundary.dependencies(),
                )
            sender.join(timeout=2)
            self.assertFalse(sender.is_alive())
            self.assertIsNotNone(boundary.processes[0].poll())
            self.assertEqual(list(boundary.sessions.iterdir()), [])

    def test_session_cleanup_does_not_follow_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            parent = root / "sessions"
            parent.mkdir()
            outside = root / "outside"
            outside.write_bytes(b"retain")
            session = workflow.OwnedSession.create(parent)
            (session.path / "file").write_bytes(b"private")
            nested = session.path / "nested"
            nested.mkdir()
            (nested / "child").write_bytes(b"private")
            (session.path / "link").symlink_to(outside)
            session.cleanup()
            self.assertFalse(session.path.exists())
            self.assertEqual(outside.read_bytes(), b"retain")

    def test_replaced_session_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            parent = root / "sessions"
            parent.mkdir()
            session = workflow.OwnedSession.create(parent)
            displaced = root / "displaced"
            session.path.rename(displaced)
            session.path.mkdir(mode=0o700)
            with self.assertRaisesRegex(workflow.WorkflowError, "identity changed"):
                session.cleanup()
            self.assertTrue(displaced.is_dir())
            self.assertTrue(session.path.is_dir())

    def test_http_request_builder_and_response_are_bounded(self) -> None:
        request = workflow.http_put_request("/actions", {"action_type": "InstanceStart"})
        self.assertEqual(
            request,
            b"PUT /actions HTTP/1.1\r\n"
            b"Host: localhost\r\n"
            b"Connection: close\r\n"
            b"Content-Type: application/json\r\n"
            b"Content-Length: 31\r\n\r\n"
            b'{"action_type":"InstanceStart"}',
        )
        with mock.patch.object(workflow, "MAX_HTTP_REQUEST_BYTES", 8):
            with self.assertRaisesRegex(workflow.WorkflowError, "fixed bound"):
                workflow.http_put_request("/actions", {"action_type": "InstanceStart"})

    def test_initrd_contains_exact_checked_rootfs_oracle(self) -> None:
        import importlib.util

        path = REPOSITORY_ROOT / "scripts/build-guest-boot-initrd.py"
        spec = importlib.util.spec_from_file_location("guest_boot_initrd_workflow", path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        data = module.build_initrd()
        module.validate_initrd(data)
        self.assertEqual(len(module.ROOTFS_WORKFLOW_OS_RELEASE), 400)
        self.assertEqual(
            hashlib.sha256(module.ROOTFS_WORKFLOW_OS_RELEASE).hexdigest(),
            "3e5851448bae5b36f351becde037a8b13b77307279f484eda808f8177d9a4293",
        )
        generated = policy.load_manifest().generated["guest-boot-initrd"]
        self.assertEqual(len(data), generated.size_bytes)
        self.assertEqual(hashlib.sha256(data).hexdigest(), generated.sha256)

    def test_signed_wrapper_owns_both_public_modes_and_rejects_test_args(self) -> None:
        script = REPOSITORY_ROOT / "scripts/run-integration-tests.sh"
        syntax = subprocess.run(("bash", "-n", os.fspath(script)), capture_output=True)
        self.assertEqual(syntax.returncode, 0, syntax.stderr.decode(errors="replace"))

        help_output = subprocess.run(
            (os.fspath(script), "--help"),
            capture_output=True,
            text=True,
        )
        self.assertEqual(help_output.returncode, 0, help_output.stderr)
        self.assertIn("guest_workflow", help_output.stdout)

        rejected = subprocess.run(
            (os.fspath(script), "--test", "guest_workflow", "--", "--ignored"),
            capture_output=True,
            text=True,
        )
        self.assertEqual(rejected.returncode, 2)
        self.assertEqual(
            rejected.stderr,
            "guest_workflow does not accept trailing Rust test arguments\n",
        )

        source = script.read_text(encoding="utf-8")
        self.assertIn("scripts/run-macos-guest-workflow.py api", source)
        self.assertIn("scripts/run-macos-guest-workflow.py no-api", source)


if __name__ == "__main__":
    unittest.main()
