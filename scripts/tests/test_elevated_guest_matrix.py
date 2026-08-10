from __future__ import annotations

import importlib.util
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts" / "elevated_guest_matrix.py"
SPEC = importlib.util.spec_from_file_location("elevated_guest_matrix", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
matrix = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = matrix
SPEC.loader.exec_module(matrix)


class ElevatedGuestMatrixTests(unittest.TestCase):
    def test_serial_transcript_requires_one_marker_and_canonical_poweroff(self) -> None:
        success = (
            b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n"
            b"[    0.026251] reboot: Power down\r\n"
        )
        failure = (
            b"BANGBANG_ROOTFS_WORKFLOW_FAIL\r\n"
            b"[   12.345678] reboot: Power down\r\n"
        )
        self.assertEqual(matrix.serial_transcript_outcome(success), "success")
        self.assertEqual(matrix.serial_transcript_outcome(failure), "failure")
        for invalid in (
            b"",
            b"BANGBANG_ROOTFS_WORKFLOW_OK\n",
            b"BANGBANG_ROOTFS_WORKFLOW_OK\r\n",
            success + b"extra",
            success.replace(b"    0.026251", b"00000.026251"),
        ):
            self.assertEqual(matrix.serial_transcript_outcome(invalid), "invalid")

    def test_runtime_root_name_matches_the_closed_launcher_shape(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(matrix, "ROOT_PARENT", Path(directory)):
                with mock.patch.object(matrix.secrets, "token_hex", return_value="a1b2c3d4"):
                    root = matrix._create_runtime_root()
            try:
                self.assertEqual(root.name, "bangbang-elevated-probe.a1b2c3d4")
                self.assertTrue(root.is_dir())
                self.assertEqual(root.stat().st_mode & 0o7777, 0o700)
            finally:
                root.rmdir()

    def test_launcher_death_cleanup_removes_only_the_ledgered_empty_session(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "root"
            root.mkdir(mode=0o700)
            root.chmod(0o700)
            fixture = object.__new__(matrix.Fixture)
            fixture.root = root
            fixture.uid = os.getuid()
            fixture.gid = os.getgid()
            fixture.root_identity = matrix.ObjectIdentity.capture(root)

            session = root / f"{matrix.SESSION_PREFIX}{'a' * 64}"
            session.mkdir(mode=0o700)
            captured_path, identity = fixture.capture_runtime_session()
            self.assertEqual(captured_path, session)
            fixture.cleanup_runtime_session(captured_path, identity)
            self.assertEqual(list(root.iterdir()), [])

            session.mkdir(mode=0o700)
            captured_path, identity = fixture.capture_runtime_session()
            displaced = root / "displaced"
            session.rename(displaced)
            session.mkdir(mode=0o700)
            with self.assertRaises(matrix.MatrixError):
                fixture.cleanup_runtime_session(captured_path, identity)
            self.assertTrue(session.is_dir())
            session.rmdir()
            displaced.rmdir()

    def test_retired_root_validation_never_removes_a_link_or_replacement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "root"
            root.mkdir(mode=0o700)
            root.chmod(0o700)
            fixture = object.__new__(matrix.Fixture)
            fixture.root = root
            fixture.uid = os.getuid()
            fixture.gid = os.getgid()
            fixture.root_identity = matrix.ObjectIdentity.capture(root)

            fixture.validate_retired_runtime_root()
            canonical = root / f"{matrix.SESSION_PREFIX}{'a' * 64}"
            canonical.mkdir(mode=0o700)
            with self.assertRaises(matrix.MatrixError):
                fixture.validate_retired_runtime_root()
            self.assertTrue(canonical.is_dir())
            canonical.rmdir()

            replacement = root / f"{matrix.SESSION_PREFIX}{'b' * 64}"
            replacement.mkdir(mode=0o700)
            identity = matrix.ObjectIdentity.capture(replacement)
            with self.assertRaises(matrix.MatrixError):
                fixture.validate_retired_runtime_root()
            self.assertEqual(matrix.ObjectIdentity.capture(replacement), identity)
            replacement.rmdir()

    def test_six_modes_bind_workload_identity_and_closed_credentials(self) -> None:
        self.assertEqual(len(matrix.MODE_CASES), 6)
        self.assertEqual(
            {(case.workload, case.identity) for case in matrix.MODE_CASES},
            {
                ("api", "mapped"),
                ("api", "retained-root"),
                ("api", "unmapped"),
                ("no-api", "mapped"),
                ("no-api", "retained-root"),
                ("no-api", "unmapped"),
            },
        )
        for workload in ("api", "no-api"):
            self.assertEqual(
                matrix.target_for(matrix.mode_for(workload, "mapped"), 501, 20),
                (501, 20),
            )
            self.assertEqual(
                matrix.target_for(matrix.mode_for(workload, "retained-root"), 501, 20),
                (0, 0),
            )
            self.assertEqual(
                matrix.target_for(matrix.mode_for(workload, "unmapped"), 501, 20),
                (matrix.UNMAPPED_ID, matrix.UNMAPPED_ID),
            )

    def test_manifests_and_worker_argv_are_exact_and_workload_specific(self) -> None:
        resources = Path("/sealed/Bangbang.app/Contents/Resources")
        workspace = Path("/private/tmp/closed-fixture")
        no_api = matrix.manifest_document("no-api", resources, workspace)
        api = matrix.manifest_document("api", resources, workspace)

        self.assertEqual(
            [grant["id"] for grant in no_api["grants"]],
            [
                "evidence-guest-config",
                "evidence-guest-kernel",
                "evidence-guest-initrd",
                "evidence-guest-rootfs",
                "evidence-guest-logger",
                "evidence-guest-metrics",
                "evidence-guest-serial",
            ],
        )
        self.assertEqual(
            [grant["id"] for grant in api["grants"]],
            [
                "evidence-guest-api",
                "evidence-guest-kernel",
                "evidence-guest-initrd",
                "evidence-guest-rootfs",
                "evidence-guest-logger",
                "evidence-guest-metrics",
                "evidence-guest-serial",
            ],
        )
        self.assertEqual(
            matrix.worker_args("no-api"),
            ["--config-file", "bangbang-grant:evidence-guest-config", "--no-api"],
        )
        self.assertEqual(
            matrix.worker_args("api"),
            ["--api-sock", "bangbang-grant:evidence-guest-api/evidence-api.sock"],
        )

    def test_fault_matrix_covers_every_new_closed_stage(self) -> None:
        self.assertEqual(
            matrix.DAEMON_RETIREMENT_FAULTS,
            (
                "namespace-retire-before-unlink",
                "namespace-retire-after-unlink",
                "namespace-retire-observe",
                "namespace-record-write",
            ),
        )
        self.assertEqual(
            [fault.fault for fault in matrix.FAULT_CASES],
            [
                "guest-grant-contract",
                "grant-transfer",
                "guest-grant-accepted",
                "guest-transport-contamination",
                "guest-resource-witness",
                "api-listener-request",
                "api-listener-bind",
                "api-listener-transfer",
                "api-listener-adoption",
                "api-socket-publication",
                "api-logger-configuration",
                "api-metrics-configuration",
                "api-serial-configuration",
                "api-machine-configuration",
                "api-boot-configuration",
                "api-drive-configuration",
                "api-instance-start",
                "no-api-startup",
                "guest-hvf-witness",
                "guest-hvf-create",
                "guest-execution",
                "guest-oracle",
                "guest-poweroff",
                "guest-timeout",
                "guest-terminal-evidence",
                "guest-cleanup",
                "guest-hvf-witness",
                "guest-hvf-create",
                "guest-execution",
                "guest-oracle",
                "guest-poweroff",
                "guest-timeout",
                "guest-terminal-evidence",
                "guest-cleanup",
            ],
        )
        for fault in matrix.FAULT_CASES:
            case = matrix.mode_for(fault.workload)
            line = matrix.expected_fault_line(case, fault)
            self.assertIn(f"stage={fault.stage}", line)
            self.assertIn(f"error={fault.category}", line)
            self.assertIn(f"result={fault.result}", line)
            self.assertNotIn("/private/", line)
            self.assertNotIn("bangbang-grant:", line)
        self.assertEqual(
            {fault.stage for fault in matrix.FAULT_CASES if fault.workload == "api"},
            {
                "api-listener-request",
                "api-listener-bind",
                "api-listener-transfer",
                "api-listener-adoption",
                "api-socket-publication",
                "api-logger-configuration",
                "api-metrics-configuration",
                "api-serial-configuration",
                "api-machine-configuration",
                "api-boot-configuration",
                "api-drive-configuration",
                "api-instance-start",
                "guest-hvf-witness",
                "guest-hvf-create",
                "guest-execution",
                "guest-oracle",
                "guest-poweroff",
                "guest-timeout",
                "guest-terminal-evidence",
                "guest-cleanup",
            },
        )

    def test_success_output_is_closed_over_worker_and_launcher_lines(self) -> None:
        no_api = matrix.expected_success_output(matrix.mode_for("no-api"))
        api = matrix.expected_success_output(matrix.mode_for("api"))
        for output in (no_api, api):
            self.assertTrue(output.startswith("bangbang 0.1.0\nhvf target supported: true\n"))
            self.assertTrue(output.endswith("lifecycle=terminal cleanup=complete"))
        self.assertEqual(len(no_api.splitlines()), 4)
        self.assertEqual(len(api.splitlines()), 7)
        self.assertIn("status: VM running without API", no_api)
        self.assertIn("status: API server listening", api)
        self.assertIn('The API server received a Put request on "/logger".', api)

    def test_api_completion_and_endpoint_death_summary_are_value_free(self) -> None:
        for identity in ("mapped", "retained-root", "unmapped"):
            case = matrix.mode_for("api", identity)
            output = matrix.expected_success_line(case)
            self.assertIn("resources=consumed workload=api api=complete", output)
            self.assertNotIn("/private/", output)
            self.assertNotIn("bangbang-grant:", output)
        self.assertIn("api-mapped=complete", matrix.MATRIX_SUMMARY)
        self.assertIn("no-api-mapped=complete", matrix.MATRIX_SUMMARY)
        self.assertIn(
            "api-pre-post-worker-first-launcher-first",
            matrix.MATRIX_SUMMARY,
        )
        self.assertIn("faults=all-reachable", matrix.MATRIX_SUMMARY)

    def test_post_adoption_barrier_is_internal_and_precedes_worker_envelope(self) -> None:
        fixture = object.__new__(matrix.Fixture)
        fixture.root = Path("/private/var/root/bangbang-elevated-probe.A1b2C3d4")
        fixture.workspace = Path("/private/tmp/bangbang-elevated-guest.A1b2C3d4")
        fixture.case = matrix.mode_for("no-api")
        fixture.uid = 501
        fixture.gid = 20

        command = fixture.command(
            Path("/sealed/Bangbang.app/Contents/MacOS/bangbang"),
            adoption_barrier=True,
        )
        barrier = command.index(matrix.ADOPTION_BARRIER_OPTION)
        manifest = command.index("--bangbang-grant-manifest")
        self.assertEqual(command[barrier + 1], "--")
        self.assertLess(barrier, manifest)
        self.assertEqual(command[-3:], matrix.worker_args("no-api"))
        with self.assertRaises(matrix.MatrixError):
            fixture.command(
                Path("/sealed/Bangbang.app/Contents/MacOS/bangbang"),
                fault="guest-oracle",
                adoption_barrier=True,
            )

    def test_daemon_command_is_exact_and_controls_are_closed(self) -> None:
        fixture = object.__new__(matrix.Fixture)
        fixture.root = Path("/private/var/root/bangbang-elevated-probe.A1b2C3d4")
        fixture.workspace = Path("/private/tmp/bangbang-elevated-guest.A1b2C3d4")
        fixture.case = matrix.mode_for("api")
        fixture.uid = 501
        fixture.gid = 20
        launcher = Path("/sealed/Bangbang.app/Contents/MacOS/bangbang")

        command = fixture.command(
            launcher,
            daemonize=True,
            daemon_barrier="post-ack-watch",
        )
        barrier = command.index(matrix.DAEMON_BARRIER_OPTION)
        jailer = command.index(matrix.JAILER_ACTIVATION)
        manifest = command.index("--bangbang-grant-manifest")
        self.assertEqual(command[barrier + 1 : barrier + 3], ["post-ack-watch", "--"])
        self.assertLess(barrier, jailer)
        self.assertEqual(
            command[jailer : jailer + 11],
            [
                matrix.JAILER_ACTIVATION,
                "--id",
                "evidence-A1b2C3d4",
                "--exec-file",
                "/sealed/Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker",
                "--uid",
                "0",
                "--gid",
                "0",
                "--daemonize",
                "--",
            ],
        )
        self.assertLess(jailer, manifest)
        self.assertEqual(command[-2:], matrix.worker_args("api"))

        watched = fixture.command(
            launcher,
            fault="guest-endpoint-death",
            daemonize=True,
            daemon_barrier="post-ack-watch",
        )
        self.assertLess(watched.index("--fault"), watched.index(matrix.DAEMON_BARRIER_OPTION))
        retirement = fixture.command(
            launcher,
            daemonize=True,
            daemon_barrier=matrix.DAEMON_NAMESPACE_RETIREMENT_BARRIER,
        )
        retirement_barrier = retirement.index(matrix.DAEMON_BARRIER_OPTION)
        self.assertEqual(
            retirement[retirement_barrier + 1 : retirement_barrier + 3],
            ["daemon-namespace-retirement", "--"],
        )
        with self.assertRaises(matrix.MatrixError):
            fixture.command(launcher, daemon_barrier="post-ack-watch")
        with self.assertRaises(matrix.MatrixError):
            fixture.command(
                launcher,
                fault="guest-oracle",
                daemonize=True,
                daemon_barrier="post-ack-watch",
            )

    def test_daemon_pid_reader_accepts_bounded_output_before_pid(self) -> None:
        long_line = "x" * 417
        process = subprocess.Popen(
            [
                sys.executable,
                "-c",
                (
                    "import sys;"
                    f"sys.stdout.write({long_line!r}+'\\n');"
                    "sys.stdout.write('bangbang daemon pid: 42\\n');"
                    "sys.stdout.flush()"
                ),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        try:
            pid_line, preceding = matrix.read_daemon_pid_line(process)
            self.assertEqual(pid_line, b"bangbang daemon pid: 42\n")
            self.assertEqual(preceding, long_line.encode("ascii") + b"\n")
            self.assertEqual(process.wait(timeout=1), 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            if process.stdout is not None:
                process.stdout.close()

    def test_daemon_pid_reader_deadline_is_not_blocked_by_partial_line(self) -> None:
        process = subprocess.Popen(
            [
                sys.executable,
                "-c",
                "import sys,time;sys.stdout.write('x');sys.stdout.flush();time.sleep(2)",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        try:
            with (
                mock.patch.object(matrix, "BARRIER_TIMEOUT_SECONDS", 0.05),
                self.assertRaisesRegex(matrix.MatrixError, "daemon-parent-pid-timeout"),
            ):
                matrix.read_daemon_pid_line(process)
            self.assertIsNone(process.poll())
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            if process.stdout is not None:
                process.stdout.close()

    def test_daemon_death_actions_use_exact_signal_order_and_raw_status(self) -> None:
        self.assertEqual(matrix.WORKER_SIGHUP_EXIT_CODE, 156)
        self.assertEqual(matrix.WORKER_NAMESPACE_REPLACEMENT_EXIT_CODE, 80)
        fixture = mock.Mock()
        daemon = mock.Mock()
        daemon.supervisor.pid = 11
        daemon.worker.pid = 22
        launcher = Path("/sealed/Bangbang.app/Contents/MacOS/bangbang")

        with (
            mock.patch.object(matrix, "start_stopped_daemon", return_value=daemon),
            mock.patch.object(matrix.os, "kill") as kill,
            mock.patch.object(matrix, "wait_for_exact_process_exit"),
        ):
            daemon.wait.return_value = {22: signal.SIGKILL, 11: 3 << 8}
            matrix.assert_daemon_endpoint_death(launcher, fixture, "worker")
            self.assertEqual(
                kill.call_args_list,
                [mock.call(22, signal.SIGKILL), mock.call(11, signal.SIGCONT)],
            )

            kill.reset_mock()
            daemon.wait.return_value = {11: signal.SIGKILL, 22: 1 << 8}
            matrix.assert_daemon_endpoint_death(launcher, fixture, "launcher")
            self.assertEqual(
                kill.call_args_list,
                [mock.call(11, signal.SIGKILL), mock.call(22, signal.SIGCONT)],
            )

    def test_exact_process_exit_treats_pid_reuse_as_exit(self) -> None:
        def identity(start_seconds: int) -> matrix.ProcessIdentity:
            return matrix.ProcessIdentity(
                pid=42,
                parent_pid=1,
                process_group=42,
                session=42,
                uid=501,
                gid=20,
                real_uid=501,
                real_gid=20,
                saved_uid=501,
                saved_gid=20,
                start_seconds=start_seconds,
                start_microseconds=7,
                executable=Path("/sealed/bangbang"),
            )

        original = identity(1)
        replacement = identity(2)
        with (
            mock.patch.object(matrix, "_pid_exists", return_value=True),
            mock.patch.object(matrix, "capture_process", return_value=replacement),
        ):
            matrix.wait_for_exact_process_exit(original)

    def test_sidecar_replacement_restores_the_original_inode_ledger(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sidecar = Path(directory)
            for index, name in enumerate(matrix.RESOURCE_NAMES.values()):
                path = sidecar / name
                path.write_bytes(f"resource-{index}\n".encode("ascii"))
                path.chmod(0o400)
            expected = matrix.capture_resources(sidecar)
            mutation = matrix.SidecarMutation(sidecar, expected, "no-api")

            mutation.apply()
            for key, name in matrix.RESOURCE_NAMES.items():
                self.assertEqual(
                    (sidecar / name).read_bytes(),
                    matrix.REPLACEMENT_BYTES,
                )
                self.assertEqual(
                    matrix.ObjectIdentity.capture(sidecar / f".adopted-{name}"),
                    expected[key].identity,
                )
            mutation.restore()

            matrix.verify_resources(sidecar, expected)
            self.assertEqual(
                {entry.name for entry in sidecar.iterdir()},
                set(matrix.RESOURCE_NAMES.values()),
            )

    def test_api_socket_replacement_preserves_both_exact_inode_ledgers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            api = Path(directory) / "api"
            api.mkdir(mode=0o700)
            socket = api / matrix.API_SOCKET_CHILD
            import socket as socket_module

            listener = socket_module.socket(socket_module.AF_UNIX)
            listener.bind(os.fspath(socket))
            socket.chmod(0o600)
            fixture = object.__new__(matrix.Fixture)
            fixture.case = matrix.mode_for("api")
            fixture.uid = os.getuid()
            fixture.gid = os.getgid()
            fixture.paths = {"api": api}
            try:
                replacement = matrix.ApiSocketReplacement(fixture)
                replacement.validate()
                self.assertTrue(replacement.displaced.is_socket())
                self.assertEqual(
                    replacement.original.read_bytes(),
                    matrix.API_SOCKET_REPLACEMENT_BYTES,
                )
                replacement.cleanup()
                self.assertEqual(list(api.iterdir()), [])
            finally:
                listener.close()

    def test_manual_scripts_never_request_or_load_credentials(self) -> None:
        for relative in (
            "scripts/build-elevated-bootstrap-probe.sh",
            "scripts/run-elevated-bootstrap-probe.sh",
            "scripts/elevated_guest_matrix.py",
        ):
            text = (REPOSITORY_ROOT / relative).read_text(encoding="utf-8")
            for forbidden in (
                "sudo",
                "target/mima",
                "security find-generic-password",
                "osascript",
                "read -s",
            ):
                self.assertNotIn(forbidden, text)

        wrapper = (
            REPOSITORY_ROOT / "scripts" / "run-elevated-bootstrap-probe.sh"
        ).read_text(encoding="utf-8")
        self.assertIn(
            '/usr/bin/python3 "$repo_root/scripts/elevated_guest_matrix.py"', wrapper
        )
        self.assertIn('--sidecar "$guest_sidecar"', wrapper)
        self.assertIn(
            "deaths=no-api-post-worker-first-launcher-first-"
            "api-pre-post-worker-first-launcher-first",
            wrapper,
        )
        self.assertIn("tamper=rejected-both-workloads", wrapper)
        self.assertIn(
            "adoption-replacement=no-api-complete-api-rejected-at-grant",
            wrapper,
        )
        self.assertIn(
            "socket-replacement=both-cleanup-owners-preserve",
            wrapper,
        )
        self.assertIn("daemon=api-no-api-all-identities-retired", wrapper)
        self.assertIn(
            "cleanup=exact-no-product-session-teardown",
            wrapper,
        )
        self.assertIn("simultaneous-uncatchable-death=unmeasured", wrapper)


if __name__ == "__main__":
    unittest.main()
