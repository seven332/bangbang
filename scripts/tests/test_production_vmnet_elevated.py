from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts/production_vmnet_certification.py"
SPEC = importlib.util.spec_from_file_location(
    "production_vmnet_certification_elevated_test", SCRIPT_PATH
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("production vmnet certification module should be importable")
vmnet = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = vmnet
SPEC.loader.exec_module(vmnet)
elevated = vmnet._elevated_module()


def fixture_source() -> str:
    return r"""#!/bin/sh
IFS= read -r prepare || exit 10
case_value=$(printf '%s' "$prepare" | sed -n 's/^{"case":"\([^"]*\)".*/\1/p')
nonce=$(printf '%s' "$prepare" | sed -n 's/.*"nonce":"\([^"]*\)".*/\1/p')
case "$case_value" in
  shared-connectivity|host-connectivity|bridged-connectivity|startup-interface-remove|runtime-hotplug-remove|capture-restore-fresh-ownership)
    printf '{"case":"%s","endpoint_ipv4":"192.168.42.1","endpoint_port":32123,"kind":"ready","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
    ;;
  *)
    printf '{"case":"%s","kind":"ready","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
    ;;
esac
printf '{"case":"%s","kind":"observed","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
IFS= read -r cleanup || exit 11
printf '{"case":"%s","kind":"complete","nonce":"%s","schema_version":1}\n' "$case_value" "$nonce"
"""


class FakeDriver:
    def __init__(self) -> None:
        self.calls: list[tuple[str, object, bytes]] = []
        self.closed = False

    def execute(self, case: str, *, endpoint: object, nonce: bytes) -> None:
        self.calls.append((case, endpoint, nonce))

    def close(self) -> None:
        self.closed = True


class FailingDriver(FakeDriver):
    def execute(self, case: str, *, endpoint: object, nonce: bytes) -> None:
        super().execute(case, endpoint=endpoint, nonce=nonce)
        raise vmnet.CertificationError("case")


class BarrierProcess:
    def __init__(self) -> None:
        self.config = SimpleNamespace(
            timeouts=SimpleNamespace(guest_seconds=1)
        )

    def raise_if_failed(self) -> None:
        pass


def elevated_result() -> dict[str, object]:
    return {
        "authority": {
            "controller": "ordinary",
            "kind": "elevated-provider",
            "outer": "ordinary",
            "owner": "irreversibly-ordinary",
            "provider": "bounded-root",
            "route": "remote-only",
        },
        "cases": [
            {"name": name, "outcome": "passed"} for name in vmnet.V2_CASE_NAMES
        ],
        "cleanup": "complete",
        "entitlements": {
            "outer_empty": True,
            "provider_empty": True,
            "worker_app_sandbox_hvf": True,
            "worker_vmnet": False,
        },
        "platform": {
            "architecture": "arm64",
            "hvf": "supported",
            "macos": "26.5.2",
            "sdk": "26.5",
        },
        "schema_version": 2,
        "source": {"commit": "1" * 40, "tree": "2" * 40},
        "verdict": "passed",
    }


class ElevatedProductionVmnetContractTests(unittest.TestCase):
    def assert_category(self, category: str, callback) -> None:
        with self.assertRaises(vmnet.CertificationError) as caught:
            callback()
        self.assertEqual(caught.exception.category, category)

    def make_config(self, root: Path) -> dict[str, object]:
        fixture = root / "fixture"
        fixture.write_text(fixture_source(), encoding="utf-8")
        fixture.chmod(0o700)
        return {
            "authority": {"kind": "elevated-provider"},
            "fixture": {
                "executable": os.fspath(fixture.resolve()),
                "sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
            },
            "optional_cases": {
                "bridged_interface": None,
                "host_connectivity": False,
                "not_authorized": False,
                "sharing_service_busy": False,
            },
            "schema_version": 2,
            "timeouts": {
                "artifact_seconds": 60,
                "build_seconds": 60,
                "fixture_seconds": 5,
                "guest_seconds": 30,
                "request_seconds": 5,
                "startup_seconds": 10,
                "terminate_seconds": 1,
            },
        }

    def test_v2_config_is_exact_and_disjoint_from_v1(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            document = self.make_config(Path(raw_temp))
            parsed = vmnet.parse_config_document(document)
            self.assertIsInstance(parsed, vmnet.ElevatedCertificationConfig)
            self.assertEqual(parsed.authority_kind, "elevated-provider")

            for key, value in (
                ("signing_identity", "Apple Development: Fixture"),
                ("provisioning_profile", "/private/path/profile"),
                ("private_sentinel", True),
            ):
                hostile = copy.deepcopy(document)
                hostile[key] = value
                self.assert_category(
                    "config", lambda candidate=hostile: vmnet.parse_config_document(candidate)
                )

            wrong_authority = copy.deepcopy(document)
            wrong_authority["authority"] = {"kind": "apple-profile"}
            self.assert_category(
                "authority",
                lambda: vmnet.parse_config_document(wrong_authority),
            )
            partial_authority = copy.deepcopy(document)
            partial_authority["authority"] = {}
            self.assert_category(
                "authority",
                lambda: vmnet.parse_config_document(partial_authority),
            )
            unknown_authority = copy.deepcopy(document)
            unknown_authority["authority"]["private_sentinel"] = True
            self.assert_category(
                "authority",
                lambda: vmnet.parse_config_document(unknown_authority),
            )

            downgraded = copy.deepcopy(document)
            downgraded["schema_version"] = 1
            self.assert_category(
                "config", lambda: vmnet.parse_config_document(downgraded)
            )

    def test_v2_config_duplicate_and_noncanonical_documents_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            document = self.make_config(root)
            config = root / "config.json"
            data = vmnet.canonical_json(document)
            config.write_bytes(data)
            config.chmod(0o600)
            parsed = vmnet.read_config(config.resolve())
            self.assertIsInstance(parsed, vmnet.ElevatedCertificationConfig)

            duplicate = data.replace(
                b'{\n  "authority":',
                b'{\n  "schema_version": 2,\n  "authority":',
                1,
            )
            duplicate_path = root / "duplicate.json"
            duplicate_path.write_bytes(duplicate)
            duplicate_path.chmod(0o600)
            self.assert_category(
                "document", lambda: vmnet.read_config(duplicate_path.resolve())
            )

            compact_path = root / "compact.json"
            compact_path.write_text(json.dumps(document), encoding="ascii")
            compact_path.chmod(0o600)
            self.assert_category(
                "document", lambda: vmnet.read_config(compact_path.resolve())
            )

    def test_v2_result_is_exact_and_only_four_rows_may_be_gated(self) -> None:
        result = elevated_result()
        self.assertEqual(vmnet.validate_result_document(result), result)
        gated = copy.deepcopy(result)
        for row in gated["cases"]:
            if row["name"] in vmnet.V2_ENVIRONMENT_GATED_CASES:
                row["outcome"] = "environment-gated"
        self.assertEqual(vmnet.validate_result_document(gated), gated)

        mandatory_gated = copy.deepcopy(result)
        mandatory_gated["cases"][0]["outcome"] = "environment-gated"
        self.assert_category(
            "result", lambda: vmnet.validate_result_document(mandatory_gated)
        )

        wrong_order = copy.deepcopy(result)
        wrong_order["cases"][0], wrong_order["cases"][1] = (
            wrong_order["cases"][1],
            wrong_order["cases"][0],
        )
        self.assert_category(
            "result", lambda: vmnet.validate_result_document(wrong_order)
        )

    def test_v2_result_rejects_authority_entitlement_and_version_crossing(self) -> None:
        result = elevated_result()
        mutations: list[dict[str, object]] = []
        for key, value in (
            ("provider", "ordinary"),
            ("route", "local-fallback"),
            ("owner", "root"),
        ):
            mutation = copy.deepcopy(result)
            mutation["authority"][key] = value
            mutations.append(mutation)
        worker_entitled = copy.deepcopy(result)
        worker_entitled["entitlements"]["worker_vmnet"] = True
        mutations.append(worker_entitled)
        provider_entitled = copy.deepcopy(result)
        provider_entitled["entitlements"]["provider_empty"] = False
        mutations.append(provider_entitled)
        downgraded = copy.deepcopy(result)
        downgraded["schema_version"] = 1
        mutations.append(downgraded)
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.assert_category(
                    "result", lambda value=mutation: vmnet.validate_result_document(value)
                )

    def test_v1_case_authority_remains_distinct(self) -> None:
        self.assertEqual(vmnet.CASE_NAMES, vmnet.V1_CASE_NAMES)
        self.assertNotEqual(vmnet.V1_CASE_NAMES, vmnet.V2_CASE_NAMES)
        self.assertEqual(len(vmnet.V1_CASE_NAMES), 21)
        self.assertEqual(len(vmnet.V2_CASE_NAMES), 31)

    def test_elevated_matrix_runs_exact_rows_and_publishes_canonical_result(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            config = vmnet.parse_config_document(self.make_config(root))
            self.assertIsInstance(config, vmnet.ElevatedCertificationConfig)
            result = (root / "result.json").resolve()
            driver = FakeDriver()
            rechecks = 0

            def recheck() -> None:
                nonlocal rechecks
                rechecks += 1

            document = vmnet.run_elevated_certification(
                config,
                result,
                vmnet.SourceIdentity("1" * 40, "2" * 40),
                vmnet.PlatformIdentity("26.5.2", "26.5"),
                vmnet.ElevatedEntitlementAssertions(True, True, True, False),
                lambda _config, _session: driver,
                session_parent=root,
                recheck=recheck,
                nonce_factory=lambda _count: bytes(range(1, 33)),
            )
            executed = [case for case, _endpoint, _nonce in driver.calls]
            expected = [
                case
                for case in vmnet.V2_CASE_NAMES
                if case not in vmnet.V2_ENVIRONMENT_GATED_CASES
            ]
            self.assertEqual(executed, expected)
            self.assertTrue(driver.closed)
            self.assertGreaterEqual(rechecks, len(expected) + 2)
            self.assertEqual(document, vmnet.read_result(result))
            self.assertEqual(document["schema_version"], 2)
            self.assertEqual(document["verdict"], "passed")
            self.assertEqual(document["cleanup"], "complete")
            self.assertEqual(
                [row["name"] for row in document["cases"]],
                list(vmnet.V2_CASE_NAMES),
            )
            self.assertEqual(
                [
                    row["name"]
                    for row in document["cases"]
                    if row["outcome"] == "environment-gated"
                ],
                sorted(
                    vmnet.V2_ENVIRONMENT_GATED_CASES,
                    key=vmnet.V2_CASE_NAMES.index,
                ),
            )

    def test_elevated_cli_is_closed_and_root_absent_fails_before_package(self) -> None:
        run_wrapper = (
            REPOSITORY_ROOT / "scripts/run-production-vmnet-certification.sh"
        )
        source = run_wrapper.read_text(encoding="utf-8")
        self.assertNotIn("/usr/bin/sudo", source.lower())
        self.assertNotIn("exec sudo", source.lower())
        self.assertIn("</dev/null", source)
        outcome = subprocess.run(
            (
                os.fspath(run_wrapper),
                "--prepared",
                "/private/tmp/bangbang-elevated-vmnet-handoff",
                "--target-uid",
                str(os.getuid() or 501),
                "--target-gid",
                str(os.getgid() or 20),
            ),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(outcome.returncode, 3)
        self.assertEqual(outcome.stdout, "")
        self.assertEqual(
            outcome.stderr,
            "bangbang production vmnet elevated run: blocked category=platform\n",
        )

        duplicate = subprocess.run(
            (
                sys.executable,
                os.fspath(SCRIPT_PATH),
                "run-elevated",
                "--prepared",
                "/private/tmp/bangbang-elevated-vmnet-handoff",
                "--prepared=/private/PRIVATE-SENTINEL",
                "--target-uid",
                "501",
                "--target-gid",
                "20",
            ),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(duplicate.returncode, 3)
        self.assertEqual(
            duplicate.stderr,
            "bangbang production vmnet invocation: invalid category=invocation\n",
        )
        self.assertNotIn("PRIVATE-SENTINEL", duplicate.stderr)

    def test_elevated_example_and_wrappers_are_canonical_and_credential_free(self) -> None:
        example_path = (
            REPOSITORY_ROOT
            / "scripts/production-vmnet-certification-elevated-config.example.json"
        )
        document = vmnet._decode_document(example_path.read_bytes())
        self.assertEqual(vmnet.canonical_json(document), example_path.read_bytes())
        self.assertEqual(document["schema_version"], 2)
        self.assertEqual(document["authority"], {"kind": "elevated-provider"})
        for name in (
            "prepare-production-vmnet-certification.sh",
            "run-production-vmnet-certification.sh",
        ):
            wrapper = (REPOSITORY_ROOT / "scripts" / name).read_text(
                encoding="utf-8"
            )
            self.assertNotIn("/usr/bin/sudo", wrapper.lower())
            self.assertNotIn("exec sudo", wrapper.lower())
            self.assertNotIn("security find-identity", wrapper)
            self.assertIn("</dev/null", wrapper)

    def test_elevated_dispatch_routes_every_exact_row(self) -> None:
        endpoint = object()
        nonce = bytes(range(1, 33))
        token = object()
        methods = (
            "_run_authority_split",
            "_run_networkless_denial",
            "_run_policy_denial",
            "_run_mmds_only",
            "_run_connectivity",
            "_run_service_case",
            "_run_staged",
            "_start_live_shared",
            "_finish_process",
            "_run_partial_start",
            "_run_pre_ready_cancellation",
            "_run_post_ready_cancellation",
            "_run_startup_provider_death",
            "_run_live_role_death",
            "_run_clean_repeat",
            "_run_concurrent",
        )
        expected: dict[str, list[tuple[str, tuple[object, ...], dict[str, object]]]] = {
            "authority-split": [("_run_authority_split", (), {})],
            "networkless-denial": [("_run_networkless_denial", (), {})],
            "missing-policy-denial": [
                (
                    "_run_policy_denial",
                    ("missing-policy-denial",),
                    {
                        "allowed": (),
                        "maximum": None,
                        "networks": (("eth0", "vmnet:shared"),),
                    },
                )
            ],
            "mismatched-policy-denial": [
                (
                    "_run_policy_denial",
                    ("mismatched-policy-denial",),
                    {
                        "allowed": ("host",),
                        "maximum": 1,
                        "networks": (("eth0", "vmnet:shared"),),
                    },
                )
            ],
            "bridge-allowlist-denial": [
                (
                    "_run_policy_denial",
                    ("bridge-allowlist-denial",),
                    {
                        "allowed": ("bridged:certbridge",),
                        "maximum": 1,
                        "networks": (("eth0", "vmnet:bridged:certother"),),
                    },
                )
            ],
            "active-interface-count-exhaustion": [
                (
                    "_run_policy_denial",
                    ("active-interface-count-exhaustion",),
                    {
                        "allowed": ("shared",),
                        "maximum": 1,
                        "networks": (
                            ("eth0", "vmnet:shared"),
                            ("eth1", "vmnet:shared"),
                        ),
                    },
                )
            ],
            "mmds-only-no-consumption": [
                ("_run_mmds_only", ("mmds-only-no-consumption",), {})
            ],
            "shared-connectivity": [
                (
                    "_run_connectivity",
                    ("shared-connectivity", "shared", endpoint, nonce),
                    {},
                )
            ],
            "host-connectivity": [
                (
                    "_run_connectivity",
                    ("host-connectivity", "host", endpoint, nonce),
                    {},
                )
            ],
            "bridged-connectivity": [
                (
                    "_run_connectivity",
                    ("bridged-connectivity", "bridged", endpoint, nonce),
                    {},
                )
            ],
            "not-authorized": [
                (
                    "_run_service_case",
                    ("not-authorized", "VMNET_NOT_AUTHORIZED"),
                    {},
                )
            ],
            "sharing-service-busy": [
                (
                    "_run_service_case",
                    ("sharing-service-busy", "VMNET_SHARING_SERVICE_BUSY"),
                    {},
                )
            ],
            "startup-interface-remove": [
                (
                    "_run_staged",
                    ("startup-interface-remove", endpoint, nonce),
                    {},
                )
            ],
            "runtime-hotplug-remove": [
                (
                    "_run_staged",
                    ("runtime-hotplug-remove", endpoint, nonce),
                    {},
                )
            ],
            "normal-teardown": [
                ("_start_live_shared", ("normal-teardown",), {}),
                ("_finish_process", (token,), {}),
            ],
            "partial-start-cleanup": [
                ("_run_partial_start", ("partial-start-cleanup",), {})
            ],
            "pre-ready-cancellation": [
                ("_run_pre_ready_cancellation", ("pre-ready-cancellation",), {})
            ],
            "post-ready-cancellation": [
                ("_run_post_ready_cancellation", ("post-ready-cancellation",), {})
            ],
            "provider-startup-death": [
                (
                    "_run_startup_provider_death",
                    ("provider-startup-death", signal.SIGTERM),
                    {},
                )
            ],
            "broker-runtime-death": [
                (
                    "_run_live_role_death",
                    ("broker-runtime-death", "broker", signal.SIGTERM),
                    {},
                )
            ],
            "owner-runtime-death": [
                (
                    "_run_live_role_death",
                    ("owner-runtime-death", "owner", signal.SIGTERM),
                    {},
                )
            ],
            "launcher-first-death": [
                (
                    "_run_live_role_death",
                    ("launcher-first-death", "outer", signal.SIGTERM),
                    {},
                )
            ],
            "worker-first-death": [
                (
                    "_run_live_role_death",
                    ("worker-first-death", "worker", signal.SIGTERM),
                    {},
                )
            ],
            "provider-sigkill-reclamation": [
                (
                    "_run_startup_provider_death",
                    ("provider-sigkill-reclamation", signal.SIGKILL),
                    {},
                )
            ],
            "broker-sigkill-reclamation": [
                (
                    "_run_live_role_death",
                    ("broker-sigkill-reclamation", "broker", signal.SIGKILL),
                    {},
                )
            ],
            "owner-sigkill-reclamation": [
                (
                    "_run_live_role_death",
                    ("owner-sigkill-reclamation", "owner", signal.SIGKILL),
                    {},
                )
            ],
            "launcher-sigkill-reclamation": [
                (
                    "_run_live_role_death",
                    ("launcher-sigkill-reclamation", "outer", signal.SIGKILL),
                    {},
                )
            ],
            "worker-sigkill-reclamation": [
                (
                    "_run_live_role_death",
                    ("worker-sigkill-reclamation", "worker", signal.SIGKILL),
                    {},
                )
            ],
            "clean-repeat": [("_run_clean_repeat", ("clean-repeat",), {})],
            "capture-restore-fresh-ownership": [
                (
                    "_run_staged",
                    ("capture-restore-fresh-ownership", endpoint, nonce),
                    {},
                )
            ],
            "concurrent-noninterchangeability": [
                (
                    "_run_concurrent",
                    ("concurrent-noninterchangeability",),
                    {},
                )
            ],
        }
        self.assertEqual(set(expected), set(vmnet.V2_CASE_NAMES))

        for case in vmnet.V2_CASE_NAMES:
            with self.subTest(case=case):
                driver = object.__new__(elevated.ElevatedSystemCertificationDriver)
                driver.vmnet = vmnet
                events: list[
                    tuple[str, tuple[object, ...], dict[str, object]]
                ] = []

                def recorder(name: str):
                    def call(*args: object, **kwargs: object) -> object:
                        events.append((name, args, kwargs))
                        return token if name == "_start_live_shared" else None

                    return call

                for name in methods:
                    setattr(driver, name, recorder(name))
                driver.execute(case, endpoint=endpoint, nonce=nonce)
                self.assertEqual(events, expected[case])

        driver = object.__new__(elevated.ElevatedSystemCertificationDriver)
        driver.vmnet = vmnet
        self.assert_category(
            "internal",
            lambda: driver.execute("unknown", endpoint=None, nonce=nonce),
        )
        self.assert_category(
            "internal",
            lambda: driver.execute(
                "authority-split", endpoint=None, nonce=bytes(32)
            ),
        )

    def test_elevated_failure_closes_driver_and_publishes_first_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            config = vmnet.parse_config_document(self.make_config(root))
            result = (root / "result.json").resolve()
            driver = FailingDriver()
            self.assert_category(
                "case",
                lambda: vmnet.run_elevated_certification(
                    config,
                    result,
                    vmnet.SourceIdentity("1" * 40, "2" * 40),
                    vmnet.PlatformIdentity("26.5.2", "26.5"),
                    vmnet.ElevatedEntitlementAssertions(True, True, True, False),
                    lambda _config, _session: driver,
                    session_parent=root,
                    nonce_factory=lambda _count: bytes(range(1, 33)),
                ),
            )
            self.assertTrue(driver.closed)
            document = vmnet.read_result(result)
            self.assertEqual(document["verdict"], "failed")
            self.assertEqual(document["cleanup"], "complete")
            self.assertEqual(document["cases"][0]["outcome"], "failed")
            self.assertTrue(
                all(row["outcome"] == "blocked" for row in document["cases"][1:])
            )
            self.assertEqual(
                sorted(path.name for path in root.iterdir()),
                ["fixture", "result.json"],
            )

    def test_elevated_result_is_no_clobber_and_cli_redacts_private_path(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            config = vmnet.parse_config_document(self.make_config(root))
            result = (root / "result.json").resolve()
            result.write_bytes(b"sentinel\n")
            factory = mock.Mock()
            self.assert_category(
                "output",
                lambda: vmnet.run_elevated_certification(
                    config,
                    result,
                    vmnet.SourceIdentity("1" * 40, "2" * 40),
                    vmnet.PlatformIdentity("26.5.2", "26.5"),
                    vmnet.ElevatedEntitlementAssertions(True, True, True, False),
                    factory,
                    session_parent=root,
                ),
            )
            factory.assert_not_called()
            self.assertEqual(result.read_bytes(), b"sentinel\n")

        private = "/private/PRIVATE-ELEVATED-SENTINEL.json"
        outcome = subprocess.run(
            (
                sys.executable,
                os.fspath(SCRIPT_PATH),
                "prepare-elevated",
                "--config",
                private,
                "--result",
                "/private/tmp/result.json",
                "--output",
                "/private/tmp/bangbang-elevated-vmnet-handoff",
            ),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(outcome.returncode, 3)
        self.assertEqual(outcome.stdout, "")
        self.assertEqual(
            outcome.stderr,
            "bangbang production vmnet elevated prepare: blocked category=config\n",
        )
        self.assertNotIn("PRIVATE-ELEVATED-SENTINEL", outcome.stderr)

    def test_staged_barrier_requires_exact_terminal_file(self) -> None:
        protocol = elevated.load_staged_protocol()
        nonce = bytes(range(1, 33))
        with tempfile.TemporaryDirectory() as raw_temp:
            path = Path(raw_temp) / "barrier.bin"
            barrier = elevated.StagedBarrier(
                vmnet,
                path,
                protocol.Scenario.RUNTIME,
                nonce,
                create=True,
            )
            for sequence in range(1, protocol.COMMAND_COUNTS[barrier.scenario] + 1):
                barrier.command(sequence)
            for sequence, status_value in enumerate(
                protocol.STATUS_GRAPHS[barrier.scenario], start=1
            ):
                record = protocol.encode_record(
                    protocol.ROLE_STATUS,
                    barrier.scenario,
                    int(status_value),
                    sequence,
                    nonce,
                )
                with path.open("r+b", buffering=0) as destination:
                    destination.seek(protocol.STATUS_OFFSET)
                    destination.write(record)
                barrier.wait(BarrierProcess(), sequence, status_value)
            barrier.assert_terminal()
            with path.open("r+b", buffering=0) as destination:
                destination.seek(protocol.CONTROL_BYTES - 1)
                destination.write(b"\1")
            self.assert_category("control", barrier.assert_terminal)

    def test_staged_traffic_control_uses_dhcp_router_endpoint(self) -> None:
        nonce = bytes(range(1, 33))
        control = elevated._staged_traffic_control(vmnet, 32123, nonce)
        self.assertEqual(len(control), 512)
        self.assertEqual(control[:8], b"BBEVNET2")
        self.assertEqual(control[8:10], (2).to_bytes(2, "big"))
        self.assertEqual(control[10:12], b"\1\1")
        self.assertFalse(any(control[12:16]))
        self.assertEqual(control[16:18], (32123).to_bytes(2, "big"))
        self.assertEqual(control[18:50], nonce)
        self.assertFalse(any(control[50:64]))
        self.assertEqual(control[64:96], hashlib.sha256(control[:64]).digest())
        self.assertFalse(any(control[96:]))
        for port, bad_nonce in ((0, nonce), (65536, nonce), (32123, bytes(32))):
            with self.subTest(port=port):
                self.assert_category(
                    "control",
                    lambda port=port, bad_nonce=bad_nonce: elevated._staged_traffic_control(
                        vmnet, port, bad_nonce
                    ),
                )

    def test_result_target_seal_rejects_hostile_values_and_creation(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp).resolve()
            metadata = os.lstat(root)
            document: dict[str, object] = {
                "device": metadata.st_dev,
                "group": metadata.st_gid,
                "inode": metadata.st_ino,
                "name": "result.json",
                "parent": os.fspath(root),
                "uid": metadata.st_uid,
            }
            seal = elevated._result_target_seal(
                vmnet, document, metadata.st_uid, metadata.st_gid
            )
            elevated._recheck_result_target(vmnet, seal)
            hostile = dict(document)
            hostile["device"] = True
            self.assert_category(
                "output",
                lambda: elevated._result_target_seal(
                    vmnet, hostile, metadata.st_uid, metadata.st_gid
                ),
            )
            (root / "result.json").write_bytes(b"occupied\n")
            self.assert_category(
                "output", lambda: elevated._recheck_result_target(vmnet, seal)
            )

    def test_loaded_package_recheck_uses_metadata_seal(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp).resolve()
            metadata = os.lstat(root)
            result_seal = elevated.ResultTargetSeal(
                root,
                "result.json",
                metadata.st_dev,
                metadata.st_ino,
                metadata.st_uid,
                metadata.st_gid,
            )
            package_seal = (
                elevated.PackageEntrySeal(
                    ".", 1, 2, 0o555, 0, 0, 64, 3, 4
                ),
            )
            artifacts = SimpleNamespace(
                kernel=Path("/kernel"),
                rootfs=Path("/rootfs"),
                kernel_identity=object(),
                rootfs_identity=object(),
                owner_uid=0,
            )
            loaded = elevated.LoadedPackage(
                object(),
                root / "result.json",
                object(),
                object(),
                artifacts,
                package_seal,
                result_seal,
            )
            with (
                mock.patch.object(
                    elevated, "_package_seal", return_value=package_seal
                ) as seal_check,
                mock.patch.object(
                    vmnet, "_recheck_config_inputs_elevated"
                ) as config_check,
                mock.patch.object(vmnet, "_recheck_artifact") as artifact_check,
            ):
                elevated.recheck_loaded_package(vmnet, Path("/package"), loaded)
            seal_check.assert_called_once()
            config_check.assert_called_once_with(loaded.config)
            self.assertEqual(artifact_check.call_count, 2)

            changed = package_seal[:-1]
            with mock.patch.object(elevated, "_package_seal", return_value=changed):
                self.assert_category(
                    "package",
                    lambda: elevated.recheck_loaded_package(
                        vmnet, Path("/package"), loaded
                    ),
                )

    def test_certification_failures_cross_handoff_as_closed_categories(self) -> None:
        handoff = elevated.load_handoff()
        self.assertLessEqual(len(handoff.SUPERVISOR_FAILURES), 255)
        self.assertEqual(
            len(handoff.CONTROLLER_FAILURES),
            len(set(handoff.CONTROLLER_FAILURES)),
        )
        for category in handoff.CERTIFICATION_FAILURES:
            with self.subTest(category=category):
                wire = f"supervisor-controller-certification-{category}"
                self.assertIn(
                    f"controller-certification-{category}",
                    handoff.SUPERVISOR_FAILURES,
                )
                self.assertEqual(elevated._handoff_category(wire), category)
        self.assertEqual(
            elevated._handoff_category(
                "supervisor-controller-certification-private-sentinel"
            ),
            "handoff",
        )

    def test_remote_process_removes_only_the_pinned_stale_socket(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            path = Path(raw_temp) / "api.sock"
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(os.fspath(path))
            path.chmod(0o600)
            identity = vmnet._api_socket_identity(path)
            process = object.__new__(elevated.RemoteProductionProcess)
            process.vmnet = vmnet
            process.files = SimpleNamespace(api_socket=path)
            process._api_identity = identity
            process._remove_stale_socket()
            self.assertFalse(os.path.lexists(path))
            listener.close()

            original = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            original.bind(os.fspath(path))
            path.chmod(0o600)
            process._api_identity = vmnet._api_socket_identity(path)
            original.close()
            path.unlink()
            replacement = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            replacement.bind(os.fspath(path))
            path.chmod(0o600)
            self.assert_category("process-cleanup", process._remove_stale_socket)
            self.assertTrue(os.path.lexists(path))
            replacement.close()
            path.unlink()

    def test_restore_orchestration_requires_fresh_owner_and_exact_barrier(self) -> None:
        protocol = elevated.load_staged_protocol()
        nonce = bytes(range(1, 33))
        endpoint = SimpleNamespace(ipv4="192.0.2.1", port=1234)
        events: list[tuple[object, ...]] = []

        class FakeBarrier:
            def __init__(self) -> None:
                self.protocol = protocol

            def wait(self, process: object, sequence: int, kind: object) -> None:
                events.append(("wait", process, sequence, kind))

            def command(self, sequence: int) -> None:
                events.append(("command", sequence))

            def assert_terminal(self) -> None:
                events.append(("terminal",))

        class FakeProcess:
            def __init__(self, name: str, owner: int) -> None:
                self.name = name
                self.owner = owner
                self.retained = False
                self.waited = False

            def owner_pid(self) -> int:
                events.append(("owner", self.name, self.owner))
                return self.owner

            def retain_files(self) -> None:
                self.retained = True
                events.append(("retain", self.name))

            def wait_ready(self) -> None:
                self.waited = True
                events.append(("ready", self.name))

        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            source_root = root / "source"
            destination_root = root / "destination"
            state_directory = source_root / "state"
            memory_directory = source_root / "memory"
            state_directory.mkdir(parents=True)
            memory_directory.mkdir(parents=True)
            (state_directory / "state.snap").write_bytes(b"state")
            (memory_directory / "memory.snap").write_bytes(b"memory")
            destination_root.mkdir()
            source_files = SimpleNamespace(
                root=source_root,
                state_directory=state_directory,
                memory_directory=memory_directory,
                control=source_root / "traffic.bin",
            )
            destination_files = SimpleNamespace(root=destination_root)
            barrier = FakeBarrier()
            source = FakeProcess("source", 101)
            destination = FakeProcess("destination", 202)
            driver = object.__new__(elevated.ElevatedSystemCertificationDriver)
            driver.vmnet = vmnet
            driver.config = SimpleNamespace(
                timeouts=SimpleNamespace(terminate_seconds=3)
            )
            driver._active = [source, destination]
            driver._create_staged_files = mock.Mock(
                side_effect=((source_files, barrier), (destination_files, barrier))
            )
            driver._spawn_files = mock.Mock(side_effect=(source, destination))
            driver._configure_staged = mock.Mock()
            driver._api_exchange = mock.Mock(return_value=object())
            driver._network_delete = mock.Mock(
                side_effect=lambda process: events.append(("delete", process.name))
            )
            driver._finish_process = mock.Mock(
                side_effect=lambda process: events.append(("finish", process.name))
            )
            driver._abort_process = mock.Mock()
            driver.cleanup_case_files = mock.Mock(
                side_effect=lambda files: events.append(("cleanup", files.root))
            )
            with (
                mock.patch.object(vmnet, "_require_no_content"),
                mock.patch.object(vmnet, "_api_put", return_value=object()) as api_put,
                mock.patch.object(vmnet, "_verify_regular_artifact"),
                mock.patch.object(vmnet, "_wait_process_absent") as wait_absent,
            ):
                driver._run_staged_restore(
                    "capture-restore-fresh-ownership",
                    endpoint,
                    nonce,
                    protocol.Scenario.RESTORE,
                )
            self.assertTrue(source.retained)
            self.assertTrue(destination.waited)
            self.assertEqual(
                [event for event in events if event[0] in ("wait", "command", "terminal")],
                [
                    ("wait", source, 1, protocol.Status.INITIAL_PRESENT),
                    ("command", 1),
                    ("wait", source, 2, protocol.Status.CAPTURE_READY),
                    ("command", 2),
                    ("wait", destination, 3, protocol.Status.PRESENT),
                    ("wait", destination, 4, protocol.Status.TRAFFIC_TWO),
                    ("command", 3),
                    ("wait", destination, 5, protocol.Status.ABSENT),
                    ("command", 4),
                    ("wait", destination, 6, protocol.Status.COMPLETE),
                    ("terminal",),
                ],
            )
            self.assertEqual(
                wait_absent.call_args_list,
                [mock.call(101, 3), mock.call(202, 3)],
            )
            self.assertEqual(api_put.call_count, 2)
            self.assertEqual(
                [call.args[1] for call in api_put.call_args_list],
                ["/snapshot/create", "/snapshot/load"],
            )
            self.assertEqual(
                driver._finish_process.call_args_list,
                [mock.call(source), mock.call(destination)],
            )
            driver.cleanup_case_files.assert_called_once_with(source_files)
            driver._abort_process.assert_not_called()

    def test_death_and_concurrency_orchestration_are_fail_closed(self) -> None:
        death = mock.Mock()
        death.signal_role.return_value = 31337
        driver = object.__new__(elevated.ElevatedSystemCertificationDriver)
        driver.vmnet = vmnet
        driver.config = SimpleNamespace(
            timeouts=SimpleNamespace(terminate_seconds=2)
        )
        driver._start_live_shared = mock.Mock(return_value=death)
        driver._retire = mock.Mock()
        driver._abort_process = mock.Mock()
        with mock.patch.object(vmnet, "_wait_process_absent") as wait_absent:
            driver._run_live_role_death(
                "owner-sigkill-reclamation", "owner", signal.SIGKILL
            )
        death.signal_role.assert_called_once_with("owner", signal.SIGKILL)
        death.wait_after_external_signal.assert_called_once_with()
        wait_absent.assert_called_once_with(31337, 2)
        driver._retire.assert_called_once_with(death)
        driver._abort_process.assert_not_called()

        first = mock.Mock()
        second = mock.Mock()
        first.roles.return_value = elevated.ProductRoles(11, 12, 13, (14,))
        second.roles.return_value = elevated.ProductRoles(21, 22, 23, ())
        driver._start_live_shared = mock.Mock(return_value=first)
        driver._spawn = mock.Mock(return_value=second)
        driver._configure = mock.Mock()
        driver._start = mock.Mock(return_value=object())
        driver._finish_process = mock.Mock()
        driver._abort_process = mock.Mock()
        with (
            mock.patch.object(vmnet, "_require_no_content"),
            mock.patch.object(vmnet, "_require_policy_denial"),
            mock.patch.object(vmnet, "_require_running") as require_running,
            mock.patch.object(vmnet, "_api_put", return_value=object()),
            mock.patch.object(vmnet, "_api_get", return_value=object()),
        ):
            driver._run_concurrent("concurrent-noninterchangeability")
        first.roles.assert_called_once_with(require_owner=True)
        second.roles.assert_called_once_with(require_owner=False, forbid_owner=True)
        self.assertEqual(require_running.call_count, 2)
        self.assertEqual(
            driver._finish_process.call_args_list,
            [mock.call(second), mock.call(first)],
        )
        driver._abort_process.assert_not_called()

        second.roles.return_value = elevated.ProductRoles(21, 12, 23, ())
        driver._finish_process.reset_mock()
        driver._abort_process.reset_mock()
        with (
            mock.patch.object(vmnet, "_require_no_content"),
            mock.patch.object(vmnet, "_require_policy_denial"),
            mock.patch.object(vmnet, "_api_put", return_value=object()),
        ):
            self.assert_category(
                "case",
                lambda: driver._run_concurrent(
                    "concurrent-noninterchangeability"
                ),
            )
        self.assertEqual(
            driver._abort_process.call_args_list,
            [mock.call(second), mock.call(first)],
        )


if __name__ == "__main__":
    unittest.main()
