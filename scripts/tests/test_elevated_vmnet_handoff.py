from __future__ import annotations

import contextlib
import dataclasses
import hashlib
import importlib.util
import io
import json
import os
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "scripts/elevated_vmnet_handoff.py"
PREPARE_PATH = REPOSITORY_ROOT / "scripts/prepare-elevated-vmnet-handoff.sh"
RUN_PATH = REPOSITORY_ROOT / "scripts/run-elevated-vmnet-handoff.sh"
SPEC = importlib.util.spec_from_file_location("elevated_vmnet_handoff", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
handoff = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = handoff
SPEC.loader.exec_module(handoff)


IMPLEMENTATION = [
    {"name": "scripts/elevated_vmnet_handoff.py", "sha256": "1" * 64, "size_bytes": 1}
]


def make_product(root: Path) -> handoff.ProductLayout:
    root.chmod(0o700)
    layout = handoff.ProductLayout.from_package(root)
    for directory in (
        layout.bundle,
        layout.bundle / "Contents",
        layout.bundle / "Contents/MacOS",
        layout.bundle / "Contents/Helpers",
        layout.worker_bundle,
        layout.worker_bundle / "Contents",
        layout.worker_bundle / "Contents/MacOS",
    ):
        directory.mkdir(mode=0o755, exist_ok=True)
        directory.chmod(0o755)
    for index, executable in enumerate(layout.executable_paths(), 1):
        executable.write_bytes(f"executable-{index}".encode("ascii"))
        executable.chmod(0o755)
    info = layout.worker_bundle / "Contents/Info.plist"
    info.write_bytes(b"plist")
    info.chmod(0o644)
    return layout


def thaw(root: Path) -> None:
    if not os.path.lexists(root):
        return
    for directory, names, files in os.walk(root):
        Path(directory).chmod(0o700)
        for name in names:
            path = Path(directory) / name
            if not path.is_symlink():
                path.chmod(0o700)
        for name in files:
            path = Path(directory) / name
            if not path.is_symlink():
                path.chmod(0o600)


@contextlib.contextmanager
def package_directory():
    raw = tempfile.mkdtemp()
    root = Path(raw)
    try:
        make_product(root)
        yield root
    finally:
        thaw(root)
        import shutil

        shutil.rmtree(root, ignore_errors=True)


def create_package(root: Path) -> None:
    source = handoff.SourceIdentity("a" * 40, "b" * 40)
    with mock.patch.object(handoff, "validate_product"), mock.patch.object(
        handoff, "_implementation_records", return_value=IMPLEMENTATION
    ):
        handoff.create_manifest(root, source, os.getuid(), os.getgid())


def verify_package(root: Path) -> handoff.SourceIdentity:
    with mock.patch.object(handoff, "validate_product"), mock.patch.object(
        handoff, "_implementation_records", return_value=IMPLEMENTATION
    ):
        return handoff.verify_package(root, os.getuid(), os.getgid())


class ManifestTests(unittest.TestCase):
    def test_canonical_manifest_binds_every_plain_entry_and_source(self) -> None:
        with package_directory() as root:
            create_package(root)
            self.assertEqual(stat.S_IMODE(root.stat().st_mode), 0o500)
            self.assertEqual(
                verify_package(root), handoff.SourceIdentity("a" * 40, "b" * 40)
            )
            document = json.loads((root / handoff.MANIFEST_NAME).read_bytes())
            self.assertEqual(document["implementation"], IMPLEMENTATION)
            self.assertEqual(document["bundle_profile"], "adhoc-networkless")
            names = [entry["name"] for entry in document["entries"]]
            self.assertEqual(names, sorted(names))
            self.assertEqual(names[0], handoff.BUNDLE_NAME)
            self.assertNotIn(handoff.MANIFEST_NAME, names)
            for entry in document["entries"]:
                self.assertFalse(entry["mode"] & 0o222)
                if entry["kind"] == "file":
                    self.assertRegex(entry["sha256"], r"^[0-9a-f]{64}$")

    def test_verifier_rejects_extra_missing_content_mode_link_and_hardlink(self) -> None:
        for mutation in ("extra", "missing", "content", "mode", "link", "hardlink"):
            with self.subTest(mutation=mutation), package_directory() as root:
                create_package(root)
                root.chmod(0o700)
                target = root / handoff.BUNDLE_NAME / "Contents/MacOS/bangbang"
                target.parent.chmod(0o755)
                if mutation == "extra":
                    (root / "extra").write_bytes(b"extra")
                    (root / "extra").chmod(0o444)
                elif mutation == "missing":
                    target.unlink()
                elif mutation == "content":
                    target.chmod(0o755)
                    target.write_bytes(b"changed")
                    target.chmod(0o555)
                elif mutation == "mode":
                    target.chmod(0o755)
                elif mutation == "link":
                    target.unlink()
                    target.symlink_to(root / handoff.MANIFEST_NAME)
                else:
                    linked = root / "linked"
                    os.link(target, linked)
                    linked.chmod(0o555)
                with self.assertRaises(handoff.HandoffError):
                    verify_package(root)

    def test_verifier_rejects_noncanonical_duplicate_unknown_and_stale_implementation(self) -> None:
        for mutation in ("noncanonical", "duplicate", "unknown", "implementation"):
            with self.subTest(mutation=mutation), package_directory() as root:
                create_package(root)
                root.chmod(0o700)
                manifest = root / handoff.MANIFEST_NAME
                document = json.loads(manifest.read_bytes())
                manifest.chmod(0o600)
                if mutation == "noncanonical":
                    raw = json.dumps(document).encode("ascii")
                elif mutation == "duplicate":
                    raw = b'{"kind":"one","kind":"two"}\n'
                else:
                    if mutation == "unknown":
                        document["unexpected"] = True
                    else:
                        document["implementation"][0]["sha256"] = "f" * 64
                    raw = handoff._canonical(document)
                manifest.write_bytes(raw)
                manifest.chmod(0o444)
                with self.assertRaises(handoff.HandoffError):
                    verify_package(root)

    def test_manifest_creation_rejects_collision_and_nonuniform_owner_contract(self) -> None:
        with package_directory() as root:
            (root / handoff.MANIFEST_NAME).write_bytes(b"occupied")
            with mock.patch.object(handoff, "validate_product"), mock.patch.object(
                handoff, "_implementation_records", return_value=IMPLEMENTATION
            ), self.assertRaisesRegex(handoff.HandoffError, "package"):
                handoff.create_manifest(
                    root,
                    handoff.SourceIdentity("a" * 40, "b" * 40),
                    os.getuid(),
                    os.getgid(),
                )

    def test_exclusive_publication_never_replaces_destination(self) -> None:
        if sys.platform != "darwin":
            self.skipTest("renamex_np is a Darwin contract")
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            source = parent / "source"
            destination = parent / "destination"
            source.mkdir()
            handoff._publish_exclusive(source, destination)
            self.assertTrue(destination.is_dir())
            replacement = parent / "replacement"
            replacement.mkdir()
            with self.assertRaisesRegex(handoff.HandoffError, "publication"):
                handoff._publish_exclusive(replacement, destination)
            self.assertTrue(replacement.is_dir())


class ProtocolTests(unittest.TestCase):
    SESSION = bytes(range(32))

    def record(self, **changes) -> handoff.Record:
        values = {
            "role": handoff.Role.CONTROLLER,
            "kind": handoff.Kind.SPAWN,
            "sequence": 7,
            "correlation": 0,
            "session": self.SESSION,
            "handle": 0,
            "value": 0,
            "payload": handoff.encode_arguments((b"one", b"two")),
            "descriptor_count": 0,
        }
        values.update(changes)
        return handoff.Record(**values)

    def test_record_round_trip_is_exact_fixed_authenticated_and_zero_tailed(self) -> None:
        record = self.record()
        encoded = record.encode()
        self.assertEqual(len(encoded), handoff.RECORD_BYTES)
        self.assertEqual(handoff.Record.decode(encoded), record)
        payload_end = handoff.PAYLOAD_OFFSET + len(record.payload)
        self.assertEqual(encoded[payload_end:], bytes(handoff.RECORD_BYTES - payload_end))
        self.assertEqual(
            encoded[handoff.DIGEST_OFFSET : handoff.PAYLOAD_OFFSET],
            hashlib.sha256(encoded[: handoff.DIGEST_OFFSET] + record.payload).digest(),
        )

    def test_decoder_rejects_every_structural_region_and_length(self) -> None:
        encoded = self.record().encode()
        for offset in (0, 8, 10, 12, 14, 16, 24, 32, 64, 72, 80, 84, 96, 128, 4095):
            with self.subTest(offset=offset):
                changed = bytearray(encoded)
                changed[offset] ^= 1
                with self.assertRaisesRegex(handoff.HandoffError, "protocol"):
                    handoff.Record.decode(bytes(changed))
        for invalid in (b"", encoded[:-1], encoded + b"x"):
            with self.assertRaisesRegex(handoff.HandoffError, "protocol"):
                handoff.Record.decode(invalid)

    def test_argument_vector_is_closed_bounded_and_byte_preserving(self) -> None:
        arguments = (b"--opaque", b"private-\xff-value")
        self.assertEqual(handoff.decode_arguments(handoff.encode_arguments(arguments)), arguments)
        for invalid in ((), (b"",), (b"a\x00b",), (b"x" * 1025,)):
            with self.assertRaisesRegex(handoff.HandoffError, "arguments"):
                handoff.encode_arguments(invalid)
        valid = handoff.encode_arguments((b"one",))
        for invalid in (valid[:-1], valid + b"x", struct.pack("!H", 0)):
            with self.assertRaisesRegex(handoff.HandoffError, "arguments"):
                handoff.decode_arguments(invalid)

    def test_datagram_transport_preserves_record_and_rights(self) -> None:
        left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        read_descriptor, write_descriptor = os.pipe()
        received = []
        try:
            sender = handoff.RecordSocket(left)
            receiver = handoff.RecordSocket(right)
            self.assertGreaterEqual(
                left.getsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF),
                handoff.SOCKET_BUFFER_BYTES,
            )
            self.assertGreaterEqual(
                right.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF),
                handoff.SOCKET_BUFFER_BYTES,
            )
            record = self.record(
                role=handoff.Role.SUPERVISOR,
                kind=handoff.Kind.SPAWNED,
                descriptor_count=1,
            )
            sender.send(record, (read_descriptor,))
            decoded, received = receiver.receive()
            self.assertEqual(decoded, record)
            self.assertEqual(len(received), 1)
            handoff._validate_output_descriptor(received[0])
        finally:
            handoff._close_descriptors(received)
            os.close(read_descriptor)
            os.close(write_descriptor)
            left.close()
            right.close()

    def test_transport_rejects_descriptor_count_confusion(self) -> None:
        left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        try:
            handoff.RecordSocket(left)
            receiver = handoff.RecordSocket(right)
            record = self.record(descriptor_count=1)
            self.assertEqual(left.send(record.encode()), handoff.RECORD_BYTES)
            with self.assertRaisesRegex(handoff.HandoffError, "descriptor"):
                receiver.receive()
        finally:
            left.close()
            right.close()

    def test_output_descriptor_rejects_writable_pipe_and_non_pipe(self) -> None:
        read_descriptor, write_descriptor = os.pipe()
        with tempfile.TemporaryFile() as regular:
            try:
                handoff._validate_output_descriptor(read_descriptor)
                with self.assertRaisesRegex(handoff.HandoffError, "descriptor"):
                    handoff._validate_output_descriptor(write_descriptor)
                with self.assertRaisesRegex(handoff.HandoffError, "descriptor"):
                    handoff._validate_output_descriptor(regular.fileno())
            finally:
                os.close(read_descriptor)
                os.close(write_descriptor)

    def test_session_rejects_post_terminal_send_and_receive(self) -> None:
        left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        try:
            session = handoff.SessionSocket(
                handoff.RecordSocket(left), handoff.Role.CONTROLLER, self.SESSION
            )
            session.terminal = True
            with self.assertRaisesRegex(handoff.HandoffError, "protocol"):
                session.send(handoff.Kind.FINISH)
            with self.assertRaisesRegex(handoff.HandoffError, "protocol"):
                session.receive()
        finally:
            left.close()
            right.close()

    def test_session_rejects_replay_skips_cross_session_and_wrong_role(self) -> None:
        cases = ("replay", "skip", "cross-session", "wrong-role")
        for case in cases:
            with self.subTest(case=case):
                left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
                try:
                    handoff.RecordSocket(left)
                    receiver = handoff.SessionSocket(
                        handoff.RecordSocket(right), handoff.Role.SUPERVISOR, self.SESSION
                    )
                    first = self.record(sequence=1)
                    left.send(first.encode())
                    receiver.receive()
                    if case == "replay":
                        invalid = first
                    elif case == "skip":
                        invalid = self.record(sequence=3)
                    elif case == "cross-session":
                        invalid = self.record(sequence=2, session=b"z" * 32)
                    else:
                        invalid = self.record(sequence=2, role=handoff.Role.SUPERVISOR)
                    left.send(invalid.encode())
                    with self.assertRaisesRegex(handoff.HandoffError, "protocol"):
                        receiver.receive()
                finally:
                    left.close()
                    right.close()


class FakeCredentialBackend(handoff.CredentialBackend):
    def __init__(self, supervisor_pid: int, uid: int, gid: int) -> None:
        self.supervisor_pid = supervisor_pid
        self.target_uid = uid
        self.target_gid = gid
        self.uid = 0
        self.gid = 0
        self.saved_uid = 0
        self.saved_gid = 0
        self.current_groups = [0, 80]
        self.final_groups = [gid]
        self.calls = []

    def groups(self) -> list[int]:
        return list(self.current_groups)

    def clear_groups(self) -> None:
        self.calls.append("clear-groups")
        self.current_groups = [self.gid]

    def set_gid(self, value: int) -> None:
        self.calls.append(f"set-gid-{value}")
        if value == 0 and self.uid != 0:
            raise PermissionError
        self.gid = value
        self.saved_gid = value
        self.current_groups = list(self.final_groups)

    def set_uid(self, value: int) -> None:
        self.calls.append(f"set-uid-{value}")
        if value == 0 and self.uid != 0:
            raise PermissionError
        self.uid = value
        self.saved_uid = value

    def restore_groups(self) -> None:
        self.calls.append("restore-groups")
        raise PermissionError

    def identity(self) -> handoff.ProcessIdentity:
        return handoff.ProcessIdentity(
            os.getpid(),
            self.supervisor_pid,
            os.getpgrp(),
            os.getsid(0),
            self.uid,
            self.gid,
            self.uid,
            self.gid,
            self.saved_uid,
            self.saved_gid,
            1,
            2,
            Path("/usr/bin/python3"),
        )


class CredentialTests(unittest.TestCase):
    def test_process_group_reader_is_exact_bounded_and_two_phase(self) -> None:
        expected = [20, 80]
        calls = []

        def getgroups(size, groups):
            calls.append(size)
            if size == 0:
                return len(expected)
            for index, value in enumerate(expected):
                groups[index] = value
            return len(expected)

        self.assertEqual(handoff._read_process_groups(getgroups), expected)
        self.assertEqual(calls, [0, len(expected)])

        with self.assertRaises(OSError):
            handoff._read_process_groups(
                lambda size, _groups: handoff.MAX_CREDENTIAL_GROUPS + 1
                if size == 0
                else 0
            )

    def test_transition_orders_drop_and_rejects_all_root_restoration(self) -> None:
        backend = FakeCredentialBackend(42, 501, 20)
        identity = handoff.transition_controller_credentials(501, 20, 42, backend)
        self.assertEqual((identity.uid, identity.gid), (501, 20))
        self.assertEqual(
            backend.calls,
            [
                "clear-groups",
                "set-gid-20",
                "set-uid-501",
                "set-uid-0",
                "set-gid-0",
                "restore-groups",
            ],
        )

    def test_transition_rejects_wrong_order_effect_saved_or_groups(self) -> None:
        backend = FakeCredentialBackend(42, 501, 20)
        backend.saved_uid = 1
        with self.assertRaisesRegex(handoff.HandoffError, "credentials"):
            handoff.transition_controller_credentials(501, 20, 42, backend)
        for uid, gid in ((0, 20), (501, 0)):
            with self.assertRaisesRegex(handoff.HandoffError, "credentials"):
                handoff.transition_controller_credentials(
                    uid, gid, 42, FakeCredentialBackend(42, 501, 20)
                )

    def test_transition_requires_exact_effective_only_group_shape(self) -> None:
        for groups in ([], [20, 80], [80]):
            with self.subTest(groups=groups):
                backend = FakeCredentialBackend(42, 501, 20)
                backend.final_groups = groups
                with self.assertRaisesRegex(
                    handoff.HandoffError, "credentials-dropped-groups"
                ):
                    handoff.transition_controller_credentials(501, 20, 42, backend)


class FakeProxy:
    def __init__(self) -> None:
        self.statuses = [None, 0]
        self.closed = 0
        self.signals = []
        self.processes = set()

    def _status_exchange(self, kind, _handle, _value=0):
        if kind in (handoff.Kind.TERM, handoff.Kind.KILL):
            self.signals.append(kind)
            return -int(kind == handoff.Kind.KILL) - 15
        return self.statuses.pop(0) if self.statuses else 0

    def _close_process(self, _process):
        self.closed += 1
        self.processes.discard(_process)
        return _process.returncode if _process.returncode is not None else 0


class ProcessTests(unittest.TestCase):
    def test_probe_failures_are_closed_controller_categories(self) -> None:
        self.assertTrue(set(handoff.PROBE_FAILURES).issubset(handoff.CONTROLLER_FAILURES))
        self.assertLessEqual(len(handoff.CONTROLLER_FAILURES), 255)
        self.assertLessEqual(len(handoff.SUPERVISOR_FAILURES), 255)
        self.assertEqual(
            set(handoff.PROVIDER_STATUS_FAILURES), set(range(10, 20))
        )
        self.assertTrue(
            set(handoff.PROVIDER_STATUS_FAILURES.values()).issubset(
                handoff.PROBE_FAILURES
            )
        )

    def test_private_probe_root_has_exact_ordinary_identity_and_mode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = handoff._create_private_probe_root(Path(directory))
            try:
                metadata = os.lstat(root)
                self.assertEqual((metadata.st_uid, metadata.st_gid), (os.getuid(), os.getgid()))
                self.assertEqual(stat.S_IMODE(metadata.st_mode), 0o700)
            finally:
                handoff._remove_tree(root)

    def test_probe_session_baseline_binds_exact_owned_namespace_and_children(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "bangbang-sessions-v1"
            root.mkdir(mode=0o700)
            self.assertEqual(handoff._probe_session_entries(root), ())
            session = root / ("session-" + "a" * 64)
            session.mkdir(mode=0o700)
            record = session / ".api-socket-owner"
            record.write_bytes(bytes(96))
            record.chmod(0o600)
            baseline = handoff._probe_session_entries(root)
            self.assertEqual(len(baseline), 1)
            self.assertEqual(baseline[0][0], session.name)
            record.write_bytes(b"x" * 96)
            changed = record.stat().st_mtime_ns + 1_000_000_000
            os.utime(record, ns=(changed, changed))
            self.assertNotEqual(handoff._probe_session_entries(root), baseline)

    def test_probe_session_baseline_rejects_unowned_shape(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "bangbang-sessions-v1"
            root.mkdir(mode=0o700)
            (root / "unexpected").mkdir(mode=0o700)
            with self.assertRaisesRegex(handoff.HandoffError, "probe-session-root"):
                handoff._probe_session_entries(root)

    def test_fixed_probes_reject_preexisting_production_session_residue(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "bangbang-sessions-v1"
            root.mkdir(mode=0o700)
            session = root / ("session-" + "a" * 64)
            session.mkdir(mode=0o700)
            with mock.patch.object(
                handoff, "_probe_session_root", return_value=root
            ), self.assertRaisesRegex(handoff.HandoffError, "probe-session-root"):
                handoff.run_fixed_probes(
                    object(),
                    handoff.ProductLayout.from_package(Path("/absolute/package")),
                    os.getuid(),
                    os.getgid(),
                )

    def test_provider_kill_targets_leader_then_gracefully_converges_group(self) -> None:
        process = mock.Mock(pid=42, returncode=None)
        process.wait.return_value = -int(handoff.signal.SIGKILL)
        provider = handoff.OwnedProvider(process, 1)
        with mock.patch.object(handoff.os, "getpgid", return_value=42), mock.patch.object(
            handoff.os, "kill"
        ) as kill, mock.patch.object(handoff.os, "killpg") as killpg, mock.patch.object(
            handoff, "_provider_group_has_live_members", return_value=True
        ), mock.patch.object(
            handoff,
            "_wait_provider_group_quiescent",
            side_effect=(False, True),
        ), mock.patch.object(
            handoff, "_provider_group_absent", return_value=True
        ):
            self.assertEqual(handoff._signal_provider(provider, handoff.Kind.KILL), -9)
        kill.assert_called_once_with(42, handoff.signal.SIGKILL)
        killpg.assert_called_once_with(42, handoff.signal.SIGTERM)

    def test_provider_kill_escalates_only_after_bounded_parent_and_term_windows(self) -> None:
        process = mock.Mock(pid=42, returncode=None)
        process.wait.return_value = -int(handoff.signal.SIGKILL)
        provider = handoff.OwnedProvider(process, 1)
        with mock.patch.object(handoff.os, "getpgid", return_value=42), mock.patch.object(
            handoff.os, "kill"
        ), mock.patch.object(handoff.os, "killpg") as killpg, mock.patch.object(
            handoff, "_provider_group_has_live_members", return_value=True
        ), mock.patch.object(
            handoff,
            "_wait_provider_group_quiescent",
            side_effect=(False, False, True),
        ), mock.patch.object(
            handoff, "_provider_group_absent", return_value=True
        ):
            self.assertEqual(handoff._signal_provider(provider, handoff.Kind.KILL), -9)
        self.assertEqual(
            killpg.call_args_list,
            [mock.call(42, handoff.signal.SIGTERM), mock.call(42, handoff.signal.SIGKILL)],
        )

    def test_provider_signal_requires_group_absence_after_exact_reap(self) -> None:
        process = mock.Mock(pid=42, returncode=None)
        process.wait.return_value = -int(handoff.signal.SIGTERM)
        provider = handoff.OwnedProvider(process, 1)
        with mock.patch.object(handoff.os, "getpgid", return_value=42), mock.patch.object(
            handoff.os, "killpg"
        ), mock.patch.object(
            handoff, "_wait_provider_group_quiescent", return_value=True
        ), mock.patch.object(
            handoff, "_wait_until", return_value=False
        ), self.assertRaisesRegex(handoff.HandoffError, "cleanup"):
            handoff._signal_provider(provider, handoff.Kind.TERM)

    def test_remote_process_drains_both_descriptors_and_closes_once(self) -> None:
        stdout_read, stdout_write = os.pipe()
        stderr_read, stderr_write = os.pipe()
        proxy = FakeProxy()
        process = handoff.RemoteProviderProcess(
            proxy, 1, 100, stdout_read, stderr_read
        )
        proxy.processes.add(process)
        os.write(stdout_write, b"stdout")
        os.write(stderr_write, b"stderr")
        os.close(stdout_write)
        os.close(stderr_write)
        self.assertEqual(process.communicate(2.0), (b"stdout", b"stderr"))
        process.close()
        process.close()
        self.assertEqual(proxy.closed, 1)

    def test_remote_process_output_overflow_is_terminal_and_signals(self) -> None:
        stdout_read, stdout_write = os.pipe()
        stderr_read, stderr_write = os.pipe()
        proxy = FakeProxy()
        proxy.statuses = [None, 0]
        process = handoff.RemoteProviderProcess(
            proxy, 1, 100, stdout_read, stderr_read
        )
        proxy.processes.add(process)
        process.stdout_capture.maximum = 4
        os.write(stdout_write, b"overflow")
        os.close(stdout_write)
        os.close(stderr_write)
        deadline = __import__("time").monotonic() + 2
        while not process.stdout_capture.result()[1] and __import__("time").monotonic() < deadline:
            __import__("time").sleep(0.01)
        with self.assertRaisesRegex(handoff.HandoffError, "output"):
            process.wait(1.0)
        process.close()
        self.assertIn(handoff.Kind.KILL, proxy.signals)

    def test_remote_process_wait_timeout_keeps_handle_owned_for_explicit_cleanup(self) -> None:
        stdout_read, stdout_write = os.pipe()
        stderr_read, stderr_write = os.pipe()
        os.close(stdout_write)
        os.close(stderr_write)
        proxy = FakeProxy()
        process = handoff.RemoteProviderProcess(
            proxy, 1, 100, stdout_read, stderr_read
        )
        proxy.processes.add(process)
        with mock.patch.object(
            proxy, "_status_exchange", return_value=None
        ), self.assertRaisesRegex(handoff.HandoffError, "timeout"):
            process.wait(0.01)
        self.assertIn(process, proxy.processes)
        process.close()
        self.assertNotIn(process, proxy.processes)

    def test_partial_spawn_closes_every_untransferred_pipe(self) -> None:
        left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        lease_read, lease_write = os.pipe()
        created = []
        real_pipe = os.pipe

        def tracked_pipe():
            pair = real_pipe()
            created.extend(pair)
            return pair

        supervisor = handoff.ProviderSupervisor(
            handoff.SessionSocket(
                handoff.RecordSocket(left), handoff.Role.SUPERVISOR, b"x" * 32
            ),
            handoff.ProductLayout.from_package(Path("/absolute/package")),
            501,
            20,
            999,
            object(),
            lease_read,
        )
        try:
            with mock.patch.object(handoff.os, "pipe", side_effect=tracked_pipe), mock.patch.object(
                handoff.subprocess, "Popen", side_effect=OSError
            ), self.assertRaisesRegex(handoff.HandoffError, "spawn"):
                supervisor._spawn((b"--fixed",))
            for descriptor in created:
                with self.assertRaises(OSError):
                    os.fstat(descriptor)
        finally:
            os.close(lease_read)
            os.close(lease_write)
            left.close()
            right.close()

    def test_guardian_lease_detects_exact_pipe_loss(self) -> None:
        left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        lease_read, lease_write = os.pipe()
        supervisor = handoff.ProviderSupervisor(
            handoff.SessionSocket(
                handoff.RecordSocket(left), handoff.Role.SUPERVISOR, b"x" * 32
            ),
            handoff.ProductLayout.from_package(Path("/absolute/package")),
            501,
            20,
            999,
            object(),
            lease_read,
        )
        try:
            self.assertTrue(supervisor._guardian_alive())
            os.close(lease_write)
            lease_write = -1
            self.assertFalse(supervisor._guardian_alive())
        finally:
            os.close(lease_read)
            if lease_write >= 0:
                os.close(lease_write)
            left.close()
            right.close()

    def test_server_cleans_owned_handles_on_controller_or_guardian_loss(self) -> None:
        for lost in ("controller", "guardian"):
            with self.subTest(lost=lost):
                left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
                lease_read, lease_write = os.pipe()
                supervisor = handoff.ProviderSupervisor(
                    handoff.SessionSocket(
                        handoff.RecordSocket(left),
                        handoff.Role.SUPERVISOR,
                        b"x" * 32,
                    ),
                    handoff.ProductLayout.from_package(Path("/absolute/package")),
                    501,
                    20,
                    999,
                    object(),
                    lease_read,
                )
                try:
                    with mock.patch.object(
                        supervisor,
                        "_guardian_alive",
                        return_value=lost != "guardian",
                    ), mock.patch.object(
                        supervisor,
                        "_controller_alive",
                        return_value=lost != "controller",
                    ), mock.patch.object(
                        supervisor, "cleanup", return_value=True
                    ) as cleanup, self.assertRaisesRegex(
                        handoff.HandoffError, "lease"
                    ):
                        supervisor.serve()
                    cleanup.assert_called_once_with()
                finally:
                    os.close(lease_read)
                    os.close(lease_write)
                    left.close()
                    right.close()

    def test_protocol_pipe_timeout_is_categorical(self) -> None:
        read_descriptor, write_descriptor = os.pipe()
        try:
            with self.assertRaisesRegex(handoff.HandoffError, "protocol-timeout"):
                handoff._read_exact(read_descriptor, 1, 0.01)
        finally:
            os.close(read_descriptor)
            os.close(write_descriptor)

    def test_cleanup_completion_ack_keeps_supervisor_alive_until_stage_owner_finishes(self) -> None:
        guardian, supervisor = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        failures = []

        def complete():
            try:
                handoff._supervisor_complete(supervisor)
            except BaseException as error:
                failures.append(error)

        thread = threading.Thread(target=complete)
        thread.start()
        handoff._wait_supervisor_complete(guardian)
        self.assertTrue(thread.is_alive())
        handoff._acknowledge_guardian_cleanup(guardian)
        thread.join(timeout=2)
        self.assertFalse(thread.is_alive())
        self.assertEqual(failures, [])
        guardian.close()
        supervisor.close()

    def test_cleanup_completion_detects_each_single_peer_loss(self) -> None:
        guardian, supervisor = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        guardian.close()
        with self.assertRaisesRegex(handoff.HandoffError, "guardian"):
            handoff._supervisor_complete(supervisor)
        supervisor.close()

        guardian, supervisor = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        supervisor.close()
        with self.assertRaisesRegex(handoff.HandoffError, "supervisor"):
            handoff._wait_supervisor_complete(guardian)
        guardian.close()

    def test_supervisor_lifecycle_failures_remain_fixed_and_categorical(self) -> None:
        self.assertEqual(
            handoff._supervisor_failure_category(handoff.HandoffError("spawn"), 5),
            "lifecycle-spawn",
        )
        self.assertEqual(
            handoff._supervisor_failure_category(
                handoff.HandoffError("controller-probe"), 5
            ),
            "controller-probe",
        )
        self.assertEqual(
            handoff._supervisor_failure_category(ValueError("private"), 5),
            "lifecycle",
        )

    def test_private_probe_cleanup_removes_exact_socket_and_reports_forcing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "probe"
            root.mkdir(mode=0o700)
            root.chmod(0o700)
            api = root / "api"
            api.mkdir(mode=0o700)
            api.chmod(0o700)
            manifest = root / "grants.json"
            manifest.write_bytes(b"manifest")
            manifest.chmod(0o600)
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(os.fspath(api / "api.sock"))
            (api / "api.sock").chmod(0o600)
            try:
                self.assertTrue(handoff._cleanup_probe_root(root))
                self.assertFalse(os.path.lexists(root))
            finally:
                listener.close()

    def test_private_probe_cleanup_removes_valid_partial_material_after_early_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "probe"
            root.mkdir(mode=0o700)
            root.chmod(0o700)
            api = root / "api"
            api.mkdir(mode=0o700)
            api.chmod(0o700)
            self.assertFalse(
                handoff._cleanup_probe_root(root, require_material=False)
            )
            self.assertFalse(os.path.lexists(root))

    def test_killed_probe_socket_has_exact_validated_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "api.sock"
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(os.fspath(path))
            path.chmod(0o600)
            try:
                handoff._remove_killed_probe_socket(path)
                self.assertFalse(os.path.lexists(path))
            finally:
                listener.close()

    def test_private_probe_cleanup_rejects_unknown_residue_without_deleting_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "probe"
            root.mkdir(mode=0o700)
            root.chmod(0o700)
            api = root / "api"
            api.mkdir(mode=0o700)
            api.chmod(0o700)
            manifest = root / "grants.json"
            manifest.write_bytes(b"manifest")
            manifest.chmod(0o600)
            unexpected = root / "unexpected"
            unexpected.write_bytes(b"private")
            unexpected.chmod(0o600)
            with self.assertRaisesRegex(handoff.HandoffError, "cleanup"):
                handoff._cleanup_probe_root(root)
            self.assertEqual(unexpected.read_bytes(), b"private")


class SurfaceTests(unittest.TestCase):
    def test_fixed_layout_and_ordinary_factory_strip_only_the_launcher(self) -> None:
        layout = handoff.ProductLayout.from_package(
            Path("/private/var/tmp/stage/package")
        )
        arguments = handoff._launcher_arguments(
            layout, 501, 20, "fixed-case", ("--version",)
        )
        self.assertEqual(Path(arguments[0]), layout.launcher)
        self.assertEqual(arguments.count("--bangbang-jailer-v1"), 1)
        self.assertEqual(arguments[-1], "--version")
        self.assertNotIn(os.fspath(layout.provider), arguments)

    def test_controller_rejects_wrong_response_correlation(self) -> None:
        class WrongCorrelationSession:
            def send(self, *_arguments, **_keywords):
                return 9

            def receive(self):
                return (
                    handoff.Record(
                        handoff.Role.SUPERVISOR,
                        handoff.Kind.RUNNING,
                        1,
                        8,
                        b"x" * 32,
                    ),
                    [],
                )

        proxy = handoff.ControllerProxy(
            WrongCorrelationSession(),
            handoff.ProductLayout.from_package(Path("/absolute/package")),
        )
        with self.assertRaisesRegex(handoff.HandoffError, "protocol"):
            proxy._exchange(handoff.Kind.POLL, handle=1)

    def test_controller_rejects_duplicate_pipe_identity_and_closes_both(self) -> None:
        layout = handoff.ProductLayout.from_package(Path("/absolute/package"))
        proxy = handoff.ControllerProxy(object(), layout)
        read_descriptor, write_descriptor = os.pipe()
        duplicate = os.dup(read_descriptor)
        response = handoff.Record(
            handoff.Role.SUPERVISOR,
            handoff.Kind.SPAWNED,
            1,
            1,
            b"x" * 32,
            handle=1,
            value=100,
            descriptor_count=2,
        )
        try:
            with mock.patch.object(
                proxy, "_exchange", return_value=(response, [read_descriptor, duplicate])
            ), self.assertRaisesRegex(handoff.HandoffError, "descriptor"):
                proxy.spawn((os.fspath(layout.launcher), "--fixed"))
            for descriptor in (read_descriptor, duplicate):
                with self.assertRaises(OSError):
                    os.fstat(descriptor)
        finally:
            os.close(write_descriptor)

    def test_process_table_uses_only_pid_ppid_state_and_comm(self) -> None:
        records = handoff._parse_process_table(
            " 10 1 Ss /stage/provider\n"
            " 11 10 S /stage/launcher\n"
            "malformed\n"
        )
        self.assertEqual(records[10], handoff.ProcessRecord(10, 1, "Ss", "/stage/provider"))
        self.assertEqual(records[11].parent_pid, 10)
        self.assertNotIn(0, records)
        source = MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn('(\"/bin/ps\", \"-axo\", \"pid=,ppid=,state=,comm=\")', source)
        self.assertNotIn("pid=,ppid=,state=,command=", source)

    def test_public_wrappers_have_closed_authority_and_no_internal_elevation(self) -> None:
        prepare = PREPARE_PATH.read_text(encoding="utf-8")
        runner = RUN_PATH.read_text(encoding="utf-8")
        combined = (prepare + runner).lower()
        self.assertNotIn("sudo", combined)
        self.assertNotIn("password", combined)
        self.assertNotIn("signing-identity", runner)
        self.assertNotIn("provisioning-profile", runner)
        self.assertNotIn("/bin/rm", combined)
        self.assertIn("</dev/null", prepare)
        self.assertIn("</dev/null", runner)
        self.assertEqual(runner.count("--prepared"), 4)
        self.assertIn("--target-uid", runner)
        self.assertIn("--target-gid", runner)

    def test_cli_rejects_root_zero_leading_zero_unknown_and_duplicate_authority(self) -> None:
        valid = handoff._parse_arguments(
            [
                "run-root",
                "--prepared",
                "/tmp/bangbang-elevated-vmnet-handoff",
                "--target-uid",
                "501",
                "--target-gid",
                "20",
            ]
        )
        self.assertEqual((valid.target_uid, valid.target_gid), (501, 20))
        for value in ("0", "0501", "-1", "4294967296", " 501"):
            with self.assertRaises(handoff.HandoffError):
                handoff._parse_arguments(
                    [
                        "run-root",
                        "--prepared",
                        "/tmp/bangbang-elevated-vmnet-handoff",
                        "--target-uid",
                        value,
                        "--target-gid",
                        "20",
                    ]
                )

    def test_public_failures_are_categorical_and_value_free(self) -> None:
        stderr = io.StringIO()
        with mock.patch.object(
            handoff, "prepare_package", side_effect=handoff.HandoffError("source")
        ), contextlib.redirect_stderr(stderr):
            status = handoff.main(
                ["prepare", "--output", "/private/SECRET/bangbang-elevated-vmnet-handoff"]
            )
        self.assertEqual(status, 1)
        self.assertEqual(stderr.getvalue(), "bangbang elevated vmnet handoff: source\n")
        self.assertNotIn("SECRET", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
