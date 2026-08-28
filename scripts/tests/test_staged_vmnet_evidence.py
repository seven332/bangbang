from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "scripts/staged_vmnet_evidence.py"
SPEC = importlib.util.spec_from_file_location("staged_vmnet_evidence", MODULE_PATH)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import contract
    raise RuntimeError("failed to load staged vmnet evidence module")
evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evidence
SPEC.loader.exec_module(evidence)


class FakeProduct:
    def running(self) -> bool:
        return True


class FakeRestoreProduct:
    def __init__(self) -> None:
        self.terminated = False
        self.killed = False

    def terminate(self) -> None:
        self.terminated = True

    def kill(self) -> None:
        self.killed = True


class FakeRoot:
    def child(self, name: str) -> Path:
        return Path("/private/run") / name


class FakeBarrier:
    def __init__(self) -> None:
        self.events: list[tuple[str, int, object | None]] = []

    def wait(self, _product: object, sequence: int, kind: object) -> None:
        self.events.append(("wait", sequence, kind))

    def command(self, sequence: int) -> None:
        self.events.append(("command", sequence, None))


class StagedVmnetEvidenceTests(unittest.TestCase):
    def test_http_response_parser_requires_exact_content_length(self) -> None:
        self.assertEqual(
            evidence._parse_http_response(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"),
            (204, b""),
        )
        for response in (
            b"HTTP/1.1 204 No Content\r\nContent-Length: 1\r\n\r\n",
            b"HTTP/1.1 204 No Content\r\n\r\n",
        ):
            with self.assertRaisesRegex(evidence.EvidenceError, "api"):
                evidence._parse_http_response(response)

    def test_cli_and_public_failure_are_value_redacted(self) -> None:
        parsed = evidence._parse(["--scenario", "restore"])
        self.assertEqual(parsed.scenario, "restore")
        for arguments in ([], ["--scenario", "unknown"], ["--scenario", "startup", "extra"]):
            with self.subTest(arguments=arguments):
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaisesRegex(
                    evidence.EvidenceError, "invocation"
                ):
                    evidence._parse(arguments)
        stderr = io.StringIO()
        with mock.patch.object(
            evidence, "run", side_effect=evidence.EvidenceError("guest-timeout")
        ), contextlib.redirect_stderr(stderr):
            self.assertEqual(evidence.main(["--scenario", "runtime"]), 1)
        self.assertEqual(
            stderr.getvalue(),
            "bangbang staged vmnet proof: failed category=guest-timeout\n",
        )

    def test_one_shot_control_is_exact_nonce_and_digest_bound(self) -> None:
        nonce = bytes(range(1, 33))
        control = evidence._traffic_control(8080, nonce)
        self.assertEqual(len(control), 512)
        self.assertEqual(control[:8], b"BBEVNET2")
        self.assertEqual(control[8:10], (2).to_bytes(2, "big"))
        self.assertEqual(control[16:18], (8080).to_bytes(2, "big"))
        self.assertEqual(control[18:50], nonce)
        import hashlib

        self.assertEqual(control[64:96], hashlib.sha256(control[:64]).digest())
        self.assertFalse(any(control[96:]))
        for port, invalid_nonce in ((0, nonce), (65536, nonce), (8080, bytes(32))):
            with self.assertRaisesRegex(evidence.EvidenceError, "control"):
                evidence._traffic_control(port, invalid_nonce)

    def test_host_barrier_rejects_skipped_and_post_terminal_operations(self) -> None:
        nonce = bytes(range(1, 33))
        with tempfile.TemporaryDirectory() as raw_temp:
            barrier = evidence.Barrier(
                Path(raw_temp) / "barrier.bin",
                evidence.protocol.Scenario.RUNTIME,
                nonce,
            )
            with self.assertRaisesRegex(evidence.EvidenceError, "control"):
                barrier.command(2)
            barrier.command(1)
            with self.assertRaisesRegex(evidence.EvidenceError, "control"):
                barrier.command(1)
            barrier.command(2)
            barrier.command(3)
            for sequence, status in enumerate(
                evidence.protocol.STATUS_GRAPHS[evidence.protocol.Scenario.RUNTIME],
                start=1,
            ):
                record = evidence.protocol.encode_record(
                    evidence.protocol.ROLE_STATUS,
                    evidence.protocol.Scenario.RUNTIME,
                    int(status),
                    sequence,
                    nonce,
                )
                with barrier.path.open("r+b", buffering=0) as destination:
                    destination.seek(evidence.protocol.STATUS_OFFSET)
                    destination.write(record)
                barrier.wait(FakeProduct(), sequence, status)
            barrier.assert_terminal()
            with self.assertRaisesRegex(evidence.EvidenceError, "control"):
                barrier.command(4)
            with self.assertRaisesRegex(evidence.EvidenceError, "control"):
                barrier.wait(FakeProduct(), 6, evidence.protocol.Status.ABSENT)

            with barrier.path.open("r+b", buffering=0) as destination:
                destination.seek(evidence.protocol.CONTROL_BYTES - 1)
                destination.write(b"\1")
            with self.assertRaisesRegex(evidence.EvidenceError, "control"):
                barrier.assert_terminal()

    def test_host_barrier_surfaces_authenticated_guest_failure(self) -> None:
        nonce = bytes(range(1, 33))
        with tempfile.TemporaryDirectory() as raw_temp:
            barrier = evidence.Barrier(
                Path(raw_temp) / "barrier.bin",
                evidence.protocol.Scenario.STARTUP,
                nonce,
            )
            failure = evidence.protocol.encode_record(
                evidence.protocol.ROLE_STATUS,
                evidence.protocol.Scenario.STARTUP,
                evidence.protocol.FAILURE_KINDS["traffic"],
                0xFFFF_FFFF_FFFF_FFFF,
                nonce,
            )
            with barrier.path.open("r+b", buffering=0) as destination:
                destination.seek(evidence.protocol.STATUS_OFFSET)
                destination.write(failure)
            with self.assertRaisesRegex(evidence.EvidenceError, "guest-staged-traffic"):
                barrier.wait(FakeProduct(), 1, evidence.protocol.Status.INITIAL_PRESENT)

    def test_startup_dispatch_orders_two_network_generations(self) -> None:
        barrier = FakeBarrier()
        network: list[str] = []
        with mock.patch.object(evidence, "_network_put", side_effect=lambda _p: network.append("put")), mock.patch.object(
            evidence, "_network_delete", side_effect=lambda _p: network.append("delete")
        ):
            evidence._run_startup(FakeProduct(), barrier)
        self.assertEqual(network, ["delete", "put", "delete"])
        self.assertEqual(
            barrier.events,
            [
                ("wait", 1, evidence.protocol.Status.INITIAL_PRESENT),
                ("command", 1, None),
                ("wait", 2, evidence.protocol.Status.TRAFFIC_ONE),
                ("command", 2, None),
                ("wait", 3, evidence.protocol.Status.ABSENT),
                ("command", 3, None),
                ("wait", 4, evidence.protocol.Status.PRESENT),
                ("wait", 5, evidence.protocol.Status.TRAFFIC_TWO),
                ("command", 4, None),
                ("wait", 6, evidence.protocol.Status.ABSENT),
                ("command", 5, None),
                ("wait", 7, evidence.protocol.Status.COMPLETE),
            ],
        )

    def test_runtime_dispatch_boots_absent_and_removes_after_traffic(self) -> None:
        barrier = FakeBarrier()
        network: list[str] = []
        with mock.patch.object(evidence, "_network_put", side_effect=lambda _p: network.append("put")), mock.patch.object(
            evidence, "_network_delete", side_effect=lambda _p: network.append("delete")
        ):
            evidence._run_runtime(FakeProduct(), barrier)
        self.assertEqual(network, ["put", "delete"])
        self.assertEqual(
            barrier.events,
            [
                ("wait", 1, evidence.protocol.Status.INITIAL_ABSENT),
                ("command", 1, None),
                ("wait", 2, evidence.protocol.Status.PRESENT),
                ("wait", 3, evidence.protocol.Status.TRAFFIC_ONE),
                ("command", 2, None),
                ("wait", 4, evidence.protocol.Status.ABSENT),
                ("command", 3, None),
                ("wait", 5, evidence.protocol.Status.COMPLETE),
            ],
        )

    def test_restore_dispatch_uses_fresh_network_override_and_file_backend(self) -> None:
        source = FakeRestoreProduct()
        destination = FakeRestoreProduct()
        barrier = FakeBarrier()
        exchanges: list[tuple[object, str, str, object]] = []

        def exchange(product: object, method: str, path: str, body: object = None):
            exchanges.append((product, method, path, body))
            return 204, b""

        artifacts = SimpleNamespace(bangbang=Path("/bangbang"))
        with mock.patch.object(evidence, "Product", return_value=destination), mock.patch.object(
            evidence, "_http", side_effect=exchange
        ):
            restored = evidence._run_restore(source, barrier, artifacts, FakeRoot())
        self.assertIs(restored, destination)
        self.assertTrue(source.terminated)
        load = next(body for _product, _method, path, body in exchanges if path == "/snapshot/load")
        self.assertNotIn("mem_file_path", load)
        self.assertEqual(load["mem_backend"]["backend_type"], "File")
        self.assertEqual(
            load["network_overrides"],
            [{"iface_id": "eth0", "host_dev_name": "vmnet:shared"}],
        )
        self.assertEqual(
            barrier.events,
            [
                ("wait", 1, evidence.protocol.Status.INITIAL_PRESENT),
                ("command", 1, None),
                ("wait", 2, evidence.protocol.Status.CAPTURE_READY),
                ("command", 2, None),
                ("wait", 3, evidence.protocol.Status.PRESENT),
                ("wait", 4, evidence.protocol.Status.TRAFFIC_TWO),
                ("command", 3, None),
                ("wait", 5, evidence.protocol.Status.ABSENT),
                ("command", 4, None),
                ("wait", 6, evidence.protocol.Status.COMPLETE),
            ],
        )

    def test_empty_serial_is_allowed_but_failure_marker_is_not(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            path = Path(raw_temp) / "serial.out"
            path.write_bytes(b"")
            evidence._check_serial(path)
            path.write_bytes(b"BANGBANG_STAGED_VMNET_FAIL_TRAFFIC\r\n")
            with self.assertRaisesRegex(evidence.EvidenceError, "guest"):
                evidence._check_serial(path)


if __name__ == "__main__":
    unittest.main()
