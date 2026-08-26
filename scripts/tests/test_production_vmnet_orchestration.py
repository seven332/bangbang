from __future__ import annotations

import contextlib
import hashlib
import io
import os
import stat
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

from scripts.tests.test_production_vmnet_certification import vmnet


FIXED_NONCE = bytes(range(1, 33))


def orchestration_fixture_source() -> str:
    return textwrap.dedent(
        r"""
        #!/bin/sh
        [ "$#" -eq 0 ] || exit 11
        [ "$HOME" = "$TMPDIR" ] || exit 12
        [ -z "${PRIVATE_SENTINEL+x}" ] || exit 13
        IFS= read -r prepare || exit 14
        case_value=$(printf '%s' "$prepare" | sed -n 's/.*"case":"\([^"]*\)".*/\1/p')
        nonce=$(printf '%s' "$prepare" | sed -n 's/.*"nonce":"\([^"]*\)".*/\1/p')
        case "$case_value" in
          shared-connectivity|host-connectivity|bridged-connectivity)
            printf '{"case":"%s","endpoint_ipv4":"192.168.42.1","endpoint_port":32123,"kind":"ready","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
            ;;
          not-authorized|sharing-service-busy)
            printf '{"case":"%s","kind":"ready","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
            ;;
          *) exit 15 ;;
        esac
        printf '{"case":"%s","kind":"observed","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
        IFS= read -r cleanup || exit 16
        case "$cleanup" in *'"kind":"cleanup"'*) ;; *) exit 17 ;; esac
        printf '{"case":"%s","kind":"complete","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
        """
    ).lstrip()


class FakeDriver:
    def __init__(
        self,
        calls: list[tuple[str, object, bytes]],
        *,
        failure: str | None = None,
        unexpected: str | None = None,
        close_failure: bool = False,
    ) -> None:
        self.calls = calls
        self.failure = failure
        self.unexpected = unexpected
        self.close_failure = close_failure
        self.closed = False

    def execute(
        self,
        case: str,
        *,
        endpoint: object,
        nonce: bytes,
    ) -> None:
        if case in vmnet.CONNECTIVITY_CASES:
            if endpoint != vmnet.FixtureEndpoint("192.168.42.1", 32123):
                raise AssertionError("connectivity should receive the fixture endpoint")
        elif endpoint is not None:
            raise AssertionError("non-connectivity should not receive an endpoint")
        if nonce != FIXED_NONCE:
            raise AssertionError("the deterministic test nonce should be retained")
        self.calls.append((case, endpoint, nonce))
        if case == self.failure:
            raise vmnet.CertificationError("case")
        if case == self.unexpected:
            raise RuntimeError("PRIVATE-SENTINEL")

    def close(self) -> None:
        self.closed = True
        if self.close_failure:
            raise vmnet.CertificationError("cleanup")


class ProductionVmnetOrchestrationTests(unittest.TestCase):
    def assert_category(self, category: str, callback) -> None:
        with self.assertRaises(vmnet.CertificationError) as caught:
            callback()
        self.assertEqual(caught.exception.category, category)
        self.assertNotIn("PRIVATE-SENTINEL", str(caught.exception))

    @staticmethod
    def write_private(path: Path, data: bytes, mode: int = 0o600) -> None:
        path.write_bytes(data)
        path.chmod(mode)

    def make_config(
        self,
        temp: Path,
        *,
        optional: dict[str, object] | None = None,
    ) -> Path:
        profile = temp / "approved.provisionprofile"
        self.write_private(profile, b"placeholder approved profile")
        fixture = temp / "fixture"
        fixture.write_text(orchestration_fixture_source(), encoding="utf-8")
        fixture.chmod(0o700)
        document = {
            "fixture": {
                "executable": os.fspath(fixture),
                "sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
            },
            "optional_cases": optional
            or {
                "bridged_interface": None,
                "host_connectivity": False,
                "not_authorized": False,
                "sharing_service_busy": False,
            },
            "provisioning_profile": os.fspath(profile),
            "schema_version": 1,
            "signing_identity": "Apple Development: Fixture",
            "timeouts": {
                "artifact_seconds": 5,
                "build_seconds": 5,
                "fixture_seconds": 5,
                "guest_seconds": 5,
                "request_seconds": 2,
                "startup_seconds": 2,
                "terminate_seconds": 1,
            },
        }
        config = temp / "config.json"
        self.write_private(config, vmnet.canonical_json(document))
        return config.resolve()

    def dependencies(
        self,
        temp: Path,
        driver: FakeDriver,
        *,
        preflight_error: str | None = None,
        prepare_error: str | None = None,
        source_recheck=None,
    ) -> vmnet.CertificationDependencies:
        kernel = temp / "kernel"
        rootfs = temp / "rootfs"
        kernel.write_bytes(b"kernel")
        rootfs.write_bytes(b"rootfs")
        kernel_identity = vmnet._verify_regular_artifact(kernel.resolve(), "kernel")
        rootfs_identity = vmnet._verify_regular_artifact(rootfs.resolve(), "rootfs")

        def preflight():
            if preflight_error:
                raise vmnet.CertificationError(preflight_error)
            return (
                vmnet.SourceIdentity("1" * 40, "2" * 40),
                vmnet.PlatformIdentity("26.5.2", "26.5"),
            )

        def prepare(_config):
            if prepare_error:
                raise vmnet.CertificationError(prepare_error)
            return vmnet.PreparedArtifacts(
                kernel.resolve(),
                rootfs.resolve(),
                kernel_identity,
                rootfs_identity,
            )

        return vmnet.CertificationDependencies(
            preflight=preflight,
            prepare_artifacts=prepare,
            build_bundles=lambda _config, session: vmnet.ProductionBundles(
                session.path / "networkless/Bangbang.app",
                session.path / "vmnet/Bangbang.app",
            ),
            inspect_bundles=lambda _bundles: vmnet.EntitlementAssertions(
                True, True, True
            ),
            driver_factory=lambda _config, _session, _artifacts, _bundles: driver,
            session_parent=temp,
            recheck_source=source_recheck or (lambda _source: None),
            nonce_factory=lambda size: FIXED_NONCE if size == 32 else b"",
        )

    def test_fixed_matrix_runs_mandatory_cases_and_records_optional_gates(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            calls: list[tuple[str, object, bytes]] = []
            driver = FakeDriver(calls)
            result = (temp / "result.json").resolve()

            document = vmnet.run_certification(
                config,
                result,
                dependencies=self.dependencies(temp, driver),
            )

            expected_calls = [
                case
                for case in vmnet.CASE_NAMES
                if case not in vmnet.ENVIRONMENT_GATED_CASES
            ]
            self.assertEqual([case for case, _endpoint, _nonce in calls], expected_calls)
            outcomes = {
                row["name"]: row["outcome"] for row in document["cases"]
            }
            for case in vmnet.ENVIRONMENT_GATED_CASES:
                self.assertEqual(outcomes[case], "environment-gated")
            for case in expected_calls:
                self.assertEqual(outcomes[case], "passed")
            self.assertEqual(document["verdict"], "passed")
            self.assertEqual(document["cleanup"], "complete")
            self.assertTrue(driver.closed)
            self.assertEqual(vmnet.read_result(result), document)
            self.assertEqual(stat.S_IMODE(result.stat().st_mode), 0o600)
            self.assertEqual(list(temp.glob("bbvmnet.*")), [])
            self.assertEqual(
                list(temp.glob("bangbang-production-vmnet-fixture.*")), []
            )

    def test_authorized_optional_rows_execute_through_fixture_protocol(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(
                temp,
                optional={
                    "bridged_interface": "bridge0",
                    "host_connectivity": True,
                    "not_authorized": True,
                    "sharing_service_busy": True,
                },
            )
            calls: list[tuple[str, object, bytes]] = []
            result = (temp / "result.json").resolve()
            document = vmnet.run_certification(
                config,
                result,
                dependencies=self.dependencies(temp, FakeDriver(calls)),
            )
            self.assertEqual(
                [case for case, _endpoint, _nonce in calls], list(vmnet.CASE_NAMES)
            )
            self.assertTrue(
                all(row["outcome"] == "passed" for row in document["cases"])
            )
            self.assertEqual(document["verdict"], "passed")

    def test_first_case_failure_publishes_failed_row_and_blocks_remainder(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            calls: list[tuple[str, object, bytes]] = []
            failure = "partial-start-cleanup"
            result = (temp / "result.json").resolve()

            self.assert_category(
                "case",
                lambda: vmnet.run_certification(
                    config,
                    result,
                    dependencies=self.dependencies(
                        temp, FakeDriver(calls, failure=failure)
                    ),
                ),
            )
            document = vmnet.read_result(result)
            outcomes = [row["outcome"] for row in document["cases"]]
            index = vmnet.CASE_NAMES.index(failure)
            self.assertEqual(outcomes[index], "failed")
            self.assertTrue(all(value == "blocked" for value in outcomes[index + 1 :]))
            self.assertEqual(document["cleanup"], "complete")
            self.assertEqual(document["verdict"], "failed")
            self.assertNotIn("PRIVATE-SENTINEL", result.read_text(encoding="ascii"))

    def test_unexpected_case_error_is_redacted_and_published_as_failure(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            result = (temp / "result.json").resolve()
            self.assert_category(
                "internal",
                lambda: vmnet.run_certification(
                    config,
                    result,
                    dependencies=self.dependencies(
                        temp,
                        FakeDriver([], unexpected="normal-teardown"),
                    ),
                ),
            )
            self.assertEqual(vmnet.read_result(result)["verdict"], "failed")
            self.assertNotIn("PRIVATE-SENTINEL", result.read_text(encoding="ascii"))

    def test_cleanup_failure_has_precedence_in_result_and_preserves_session_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            result = (temp / "result.json").resolve()
            driver = FakeDriver([], close_failure=True)
            self.assert_category(
                "cleanup",
                lambda: vmnet.run_certification(
                    config,
                    result,
                    dependencies=self.dependencies(temp, driver),
                ),
            )
            document = vmnet.read_result(result)
            self.assertEqual(document["cleanup"], "incomplete")
            self.assertEqual(document["verdict"], "failed")
            self.assertEqual(list(temp.glob("bbvmnet.*")), [])

    def test_cleanup_failure_overrides_an_existing_case_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            result = (temp / "result.json").resolve()
            driver = FakeDriver(
                [], failure="normal-teardown", close_failure=True
            )
            self.assert_category(
                "cleanup",
                lambda: vmnet.run_certification(
                    config,
                    result,
                    dependencies=self.dependencies(temp, driver),
                ),
            )
            document = vmnet.read_result(result)
            self.assertEqual(document["cleanup"], "incomplete")
            self.assertEqual(document["verdict"], "failed")
            self.assertEqual(
                document["cases"][vmnet.CASE_NAMES.index("normal-teardown")][
                    "outcome"
                ],
                "failed",
            )

    def test_final_source_drift_cannot_publish_a_passing_result(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            result = (temp / "result.json").resolve()
            driver = FakeDriver([])

            def recheck(_source) -> None:
                if driver.closed:
                    raise vmnet.CertificationError("source")

            self.assert_category(
                "source",
                lambda: vmnet.run_certification(
                    config,
                    result,
                    dependencies=self.dependencies(
                        temp, driver, source_recheck=recheck
                    ),
                ),
            )
            document = vmnet.read_result(result)
            self.assertEqual(document["verdict"], "failed")
            self.assertEqual(document["cases"][-1]["outcome"], "failed")

    def test_setup_failures_create_neither_result_nor_session(self) -> None:
        for phase, keyword in (("platform", "preflight_error"), ("artifact", "prepare_error")):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as raw_temp:
                temp = Path(raw_temp)
                config = self.make_config(temp)
                result = (temp / "result.json").resolve()
                arguments = {keyword: phase}
                self.assert_category(
                    phase,
                    lambda: vmnet.run_certification(
                        config,
                        result,
                        dependencies=self.dependencies(
                            temp, FakeDriver([]), **arguments
                        ),
                    ),
                )
                self.assertFalse(result.exists())
                self.assertEqual(list(temp.glob("bbvmnet.*")), [])

    def test_existing_output_blocks_every_dependency_before_config_execution(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config = self.make_config(temp)
            result = (temp / "result.json").resolve()
            result.write_bytes(b"PRIVATE-SENTINEL")
            dependencies = mock.Mock()
            self.assert_category(
                "output",
                lambda: vmnet.run_certification(
                    config, result, dependencies=dependencies
                ),
            )
            dependencies.preflight.assert_not_called()
            self.assertEqual(result.read_bytes(), b"PRIVATE-SENTINEL")

    def test_case_files_are_canonical_private_and_bind_control(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            kernel = (temp / "kernel").resolve()
            rootfs = (temp / "rootfs").resolve()
            kernel.write_bytes(b"kernel")
            rootfs.write_bytes(b"rootfs")
            session = vmnet.PrivateSession.create(Path("/private/tmp"))
            try:
                files = vmnet._create_case_files(
                    session,
                    vmnet.PreparedArtifacts(
                        kernel,
                        rootfs,
                        vmnet._verify_regular_artifact(kernel, "kernel"),
                        vmnet._verify_regular_artifact(rootfs, "rootfs"),
                    ),
                    "shared-connectivity",
                    vmnet.CASE_NAMES.index("shared-connectivity"),
                    mode="shared",
                    endpoint=vmnet.FixtureEndpoint("192.168.42.1", 32123),
                    nonce=FIXED_NONCE,
                )
                manifest = vmnet._decode_document(files.manifest.read_bytes())
                grants = [
                    (grant["id"], grant["role"], grant["access"])
                    for grant in manifest["grants"]
                ]
                self.assertEqual(
                    grants,
                    [
                        (vmnet.KERNEL_GRANT_ID, "kernel-image", "read-only"),
                        (vmnet.ROOTFS_GRANT_ID, "drive-backing", "read-only"),
                        (vmnet.CONTROL_GRANT_ID, "drive-backing", "read-only"),
                        (vmnet.SERIAL_GRANT_ID, "serial-sink", "write-only"),
                        (
                            vmnet.API_DIRECTORY_GRANT_ID,
                            "api-socket-directory",
                            "create-children",
                        ),
                    ],
                )
                assert files.control is not None
                self.assertEqual(
                    vmnet.decode_guest_control(files.control.read_bytes()),
                    vmnet.GuestControl(
                        "shared", "192.168.42.1", 32123, FIXED_NONCE
                    ),
                )
                for path in (files.manifest, files.serial, files.control):
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                self.assertEqual(stat.S_IMODE(files.root.stat().st_mode), 0o700)
                self.assertEqual(stat.S_IMODE(files.api_directory.stat().st_mode), 0o700)
            finally:
                session.cleanup()
            self.assertEqual(list(temp.glob("bbvmnet.*")), [])

    def test_case_creation_rejects_replaced_artifacts_before_grants(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            kernel = (temp / "kernel").resolve()
            rootfs = (temp / "rootfs").resolve()
            kernel.write_bytes(b"kernel")
            rootfs.write_bytes(b"rootfs")
            artifacts = vmnet.PreparedArtifacts(
                kernel,
                rootfs,
                vmnet._verify_regular_artifact(kernel, "kernel"),
                vmnet._verify_regular_artifact(rootfs, "rootfs"),
            )
            rootfs.write_bytes(b"replacement")
            session = vmnet.PrivateSession.create(Path("/private/tmp"))
            try:
                self.assert_category(
                    "artifact",
                    lambda: vmnet._create_case_files(
                        session,
                        artifacts,
                        "normal-teardown",
                        vmnet.CASE_NAMES.index("normal-teardown"),
                    ),
                )
                self.assertEqual(list(session.path.iterdir()), [])
            finally:
                session.cleanup()

    def test_http_request_and_response_contracts_are_closed(self) -> None:
        request = vmnet._http_request_bytes(
            "PUT", "/actions", {"action_type": "InstanceStart"}
        )
        self.assertIn(b"Content-Length: 31\r\n", request)
        response = vmnet._parse_http_response(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
        )
        vmnet._require_no_content(response)
        policy_body = b'{"fault_message":"system host networking is not authorized"}'
        policy = vmnet._parse_http_response(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: "
            + str(len(policy_body)).encode("ascii")
            + b"\r\n\r\n"
            + policy_body
        )
        vmnet._require_policy_denial(policy)
        service_body = (
            b'{"fault_message":"failed to start microVM: hypervisor error: '
            b'failed to start vmnet packet I/O: '
            b'VMNET_NOT_AUTHORIZED"}'
        )
        service = vmnet._parse_http_response(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: "
            + str(len(service_body)).encode("ascii")
            + b"\r\n\r\n"
            + service_body
        )
        vmnet._require_service_status(service, "VMNET_NOT_AUTHORIZED")

        hostile = (
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n"
            b"Content-Length: 0\r\n\r\n"
        )
        self.assert_category("http", lambda: vmnet._parse_http_response(hostile))
        self.assert_category(
            "http",
            lambda: vmnet._parse_http_response(
                b"HTTP/1.1 204 No Content\r\nContent-Length: 00\r\n\r\n"
            ),
        )
        self.assert_category(
            "http",
            lambda: vmnet._parse_http_response(
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n"
                b"Transfer-Encoding: chunked\r\n\r\n"
            ),
        )
        self.assert_category(
            "api",
            lambda: vmnet._require_service_status(
                service, "VMNET_SHARING_SERVICE_BUSY"
            ),
        )
        extra_service = vmnet.HttpResponse(
            400,
            b'{"fault_message":"prefix VMNET_NOT_AUTHORIZED"}',
        )
        self.assert_category(
            "api",
            lambda: vmnet._require_service_status(
                extra_service, "VMNET_NOT_AUTHORIZED"
            ),
        )
        self.assert_category(
            "http", lambda: vmnet._http_request_bytes("POST", "/", None)
        )
        self.assert_category(
            "http", lambda: vmnet._http_request_bytes("GET", "/bad path", None)
        )

    def test_bounded_command_rejects_timeout_and_output_overflow(self) -> None:
        for iteration in range(20):
            success = vmnet.run_bounded_command(
                (sys.executable, "-c", "print('fixed')"),
                timeout_seconds=5,
                phase=f"unit-success-{iteration}",
                environment=vmnet._production_environment(),
            )
            self.assertEqual(success.stdout, b"fixed\n")
            self.assertEqual(success.stderr, b"")
        self.assert_category(
            "tool-output",
            lambda: vmnet.run_bounded_command(
                (
                    sys.executable,
                    "-c",
                    f"import sys;sys.stdout.write('x'*{vmnet.MAX_COMMAND_CAPTURE_BYTES + 1})",
                ),
                timeout_seconds=5,
                phase="unit-overflow",
                environment=vmnet._production_environment(),
            ),
        )
        self.assert_category(
            "tool-timeout",
            lambda: vmnet.run_bounded_command(
                (sys.executable, "-c", "import time;time.sleep(5)"),
                timeout_seconds=0.1,
                phase="unit-timeout",
                environment=vmnet._production_environment(),
            ),
        )

    def test_policy_and_launcher_arguments_are_exact_and_secret_free(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            bundle = (temp / "Bangbang.app").resolve()
            manifest = (temp / "grants.json").resolve()
            files = vmnet.CaseFiles(
                temp.resolve(),
                manifest,
                temp.resolve(),
                (temp / "api.sock").resolve(),
                (temp / "serial").resolve(),
                vmnet.FileIdentity(1, 2, 0),
                None,
                None,
            )
            arguments = vmnet._launcher_arguments(
                bundle, files, "cert-01", ("shared", "bridged:bridge0"), 2
            )
            self.assertEqual(arguments.count("--"), 2)
            self.assertEqual(arguments.count("--vmnet-allow"), 2)
            self.assertIn(vmnet.API_SOCKET_REF, arguments)
            self.assertIn(os.fspath(manifest), arguments)
            self.assertNotIn("PRIVATE-SENTINEL", "\n".join(arguments))
            self.assertEqual(
                vmnet._policy_arguments(("host",), 1),
                ["--vmnet-allow", "host", "--vmnet-max-interfaces", "1"],
            )
            self.assert_category(
                "internal", lambda: vmnet._policy_arguments(("shared",), None)
            )
            self.assert_category(
                "internal",
                lambda: vmnet._policy_arguments(
                    ("bridged:private/interface",), 1
                ),
            )

    def test_worker_help_and_api_peer_identity_are_exact(self) -> None:
        vmnet._require_worker_help(
            vmnet.CommandOutcome(
                0,
                b"bangbang 0.1.0\n\nUsage:\n  bangbang [OPTIONS]\n",
                b"",
            )
        )
        self.assert_category(
            "bundle",
            lambda: vmnet._require_worker_help(
                vmnet.CommandOutcome(0, b"Usage: bangbang\n", b"")
            ),
        )

        with tempfile.TemporaryDirectory() as raw_temp:
            path = Path(raw_temp) / "api.sock"
            server = vmnet.socket.socket(vmnet.socket.AF_UNIX, vmnet.socket.SOCK_STREAM)
            client = vmnet.socket.socket(vmnet.socket.AF_UNIX, vmnet.socket.SOCK_STREAM)
            try:
                server.bind(os.fspath(path))
                server.listen(1)
                identity = vmnet._api_socket_identity(path)
                client.connect(os.fspath(path))
                vmnet._verify_connected_api_socket(
                    client, path, identity, os.getpid()
                )
                self.assert_category(
                    "socket",
                    lambda: vmnet._verify_connected_api_socket(
                        client, path, identity, os.getpid() + 1
                    ),
                )
            finally:
                client.close()
                server.close()

    def test_bundle_inspection_requires_the_exact_entitlement_split(self) -> None:
        bundles = vmnet.ProductionBundles(
            Path("/private/tmp/networkless/Bangbang.app"),
            Path("/private/tmp/vmnet/Bangbang.app"),
        )
        networkless = {
            vmnet.APP_SANDBOX_ENTITLEMENT: True,
            vmnet.HYPERVISOR_ENTITLEMENT: True,
        }
        approved = {
            vmnet.APP_SANDBOX_ENTITLEMENT: True,
            vmnet.HYPERVISOR_ENTITLEMENT: True,
            vmnet.VMNET_ENTITLEMENT: True,
            vmnet.APPLICATION_IDENTIFIER_ENTITLEMENT: "TEAM.dev.bangbang.worker",
            vmnet.TEAM_IDENTIFIER_ENTITLEMENT: "TEAM",
        }
        with mock.patch.object(vmnet, "_verify_bundle_layout"), mock.patch.object(
            vmnet, "_verify_code"
        ), mock.patch.object(
            vmnet,
            "_entitlements",
            side_effect=({}, {}, networkless, approved),
        ):
            self.assertEqual(
                vmnet._default_inspect_bundles(bundles),
                vmnet.EntitlementAssertions(True, True, True),
            )

        widened = dict(approved)
        widened["PRIVATE-SENTINEL"] = True
        with mock.patch.object(vmnet, "_verify_bundle_layout"), mock.patch.object(
            vmnet, "_verify_code"
        ), mock.patch.object(
            vmnet,
            "_entitlements",
            side_effect=({}, {}, networkless, widened),
        ):
            self.assert_category(
                "bundle", lambda: vmnet._default_inspect_bundles(bundles)
            )

    def test_system_driver_uses_both_worker_and_launcher_death_orders(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            config = vmnet.read_config(self.make_config(Path(raw_temp)))

            for number in (vmnet.signal.SIGTERM, vmnet.signal.SIGKILL):
                with self.subTest(worker_signal=number):
                    driver = object.__new__(vmnet.SystemCertificationDriver)
                    driver.config = config
                    process = mock.Mock()
                    process.worker_pid.return_value = 41001
                    driver._active = [process]
                    driver._start_live_shared = mock.Mock(return_value=process)
                    with mock.patch.object(vmnet.os, "kill") as kill, mock.patch.object(
                        vmnet, "_wait_process_absent"
                    ) as wait_absent:
                        driver._run_worker_death("worker-first-death", number)
                    kill.assert_called_once_with(41001, number)
                    process.wait_after_external_signal.assert_called_once_with()
                    wait_absent.assert_called_once_with(
                        41001, config.timeouts.terminate_seconds
                    )
                    self.assertEqual(driver._active, [])

            driver = object.__new__(vmnet.SystemCertificationDriver)
            driver.config = config
            process = mock.Mock()
            process.process.pid = 41002
            process.worker_pid.return_value = 41003
            driver._active = [process]
            driver._start_live_shared = mock.Mock(return_value=process)
            with mock.patch.object(vmnet.os, "kill") as kill, mock.patch.object(
                vmnet, "_wait_process_absent"
            ) as wait_absent:
                driver._run_launcher_death("launcher-first-death")
            kill.assert_called_once_with(41002, vmnet.signal.SIGKILL)
            process.wait_after_external_signal.assert_called_once_with()
            wait_absent.assert_called_once_with(
                41003, config.timeouts.terminate_seconds
            )
            self.assertEqual(driver._active, [])

    def test_system_driver_concurrent_case_keeps_first_owner_live(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            config = vmnet.read_config(self.make_config(Path(raw_temp)))
            driver = object.__new__(vmnet.SystemCertificationDriver)
            driver.config = config
            first = mock.Mock()
            second = mock.Mock()
            driver._active = [first]
            driver._start_live_shared = mock.Mock(return_value=first)

            def spawn(_case):
                driver._active.append(second)
                return second

            driver._spawn = spawn
            driver._configure = mock.Mock()
            driver._start = mock.Mock(return_value=vmnet.HttpResponse(204, b""))
            policy = vmnet.HttpResponse(
                400,
                b'{"fault_message":"system host networking is not authorized"}',
            )
            running = vmnet.HttpResponse(200, b'{"state":"Running"}')
            with (
                mock.patch.object(vmnet, "_api_put", return_value=policy) as put,
                mock.patch.object(vmnet, "_api_get", return_value=running) as get,
            ):
                driver._run_concurrent("concurrent-noninterchangeability")
            put.assert_called_once()
            self.assertEqual(get.call_count, 2)
            second.terminate.assert_called_once_with()
            first.terminate.assert_called_once_with()
            self.assertEqual(driver._active, [])

    def test_source_recheck_requires_the_same_clean_identity(self) -> None:
        expected = vmnet.SourceIdentity("1" * 40, "2" * 40)
        with mock.patch.object(
            vmnet, "_read_clean_source_identity", return_value=expected
        ):
            vmnet._default_recheck_source(expected)
        with mock.patch.object(
            vmnet,
            "_read_clean_source_identity",
            return_value=vmnet.SourceIdentity("3" * 40, "4" * 40),
        ):
            self.assert_category(
                "source", lambda: vmnet._default_recheck_source(expected)
            )

    def test_source_identity_rejects_untracked_build_inputs(self) -> None:
        commands: list[tuple[str, ...]] = []

        def run(arguments, **_kwargs):
            command = tuple(arguments)
            commands.append(command)
            output = b"?? build.rs\n" if command[1] == "status" else b""
            return vmnet.CommandOutcome(0, output, b"")

        with mock.patch.object(vmnet, "run_bounded_command", side_effect=run):
            self.assert_category("source", vmnet._read_clean_source_identity)
        self.assertIn(
            (
                "/usr/bin/git",
                "status",
                "--porcelain=v1",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ),
            commands,
        )

    def test_environment_and_error_categories_cannot_reflect_sentinels(self) -> None:
        with mock.patch.dict(os.environ, {"PRIVATE_SENTINEL": "secret"}, clear=False):
            environment = vmnet._production_environment()
        self.assertNotIn("PRIVATE_SENTINEL", environment)
        error = vmnet.CertificationError("PRIVATE-SENTINEL")
        self.assertEqual(error.category, "internal")
        self.assertNotIn("PRIVATE-SENTINEL", str(error))

    def test_result_assembly_preserves_exact_order_and_optional_verdict(self) -> None:
        outcomes = [
            "environment-gated" if case in vmnet.ENVIRONMENT_GATED_CASES else "passed"
            for case in vmnet.CASE_NAMES
        ]
        document = vmnet._result_document(
            vmnet.SourceIdentity("1" * 40, "2" * 40),
            vmnet.PlatformIdentity("26.5.2", "26.5"),
            vmnet.EntitlementAssertions(True, True, True),
            outcomes,
            "complete",
        )
        self.assertEqual(
            [row["name"] for row in document["cases"]], list(vmnet.CASE_NAMES)
        )
        self.assertEqual(document["verdict"], "passed")

    def test_default_platform_preflight_rejects_unsupported_host_without_tools(self) -> None:
        with mock.patch.object(vmnet.sys, "platform", "linux"), mock.patch.object(
            vmnet.platform, "machine", return_value="arm64"
        ):
            self.assert_category("platform", vmnet._default_preflight)
        with mock.patch.object(vmnet.sys, "platform", "darwin"), mock.patch.object(
            vmnet.platform, "machine", return_value="x86_64"
        ):
            self.assert_category("platform", vmnet._default_preflight)

    def test_cli_run_status_is_fixed_for_success_and_failure(self) -> None:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with (
            mock.patch.object(vmnet, "run_certification", return_value={}),
            contextlib.redirect_stdout(stdout),
            contextlib.redirect_stderr(stderr),
        ):
            self.assertEqual(
                vmnet.main(
                    (
                        "run",
                        "--config",
                        "/private/config.json",
                        "--result",
                        "/private/result.json",
                    )
                ),
                0,
            )
        self.assertEqual(stdout.getvalue(), "bangbang production vmnet run: passed\n")
        self.assertEqual(stderr.getvalue(), "")

        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.object(
            vmnet,
            "run_certification",
            side_effect=vmnet.CertificationError("guest-timeout"),
        ), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            self.assertEqual(
                vmnet.main(
                    (
                        "run",
                        "--config",
                        "/private/PRIVATE-SENTINEL.json",
                        "--result",
                        "/private/PRIVATE-SENTINEL-result.json",
                    )
                ),
                3,
            )
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual(
            stderr.getvalue(),
            "bangbang production vmnet run: blocked category=guest-timeout\n",
        )
        self.assertNotIn("PRIVATE-SENTINEL", stderr.getvalue())

    def test_optional_case_mapping_is_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            config = vmnet.read_config(self.make_config(Path(raw_temp)))
            for case in vmnet.ENVIRONMENT_GATED_CASES:
                self.assertFalse(vmnet._optional_case_enabled(config, case))
            self.assertTrue(vmnet._optional_case_enabled(config, "normal-teardown"))


if __name__ == "__main__":
    unittest.main()
