from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path


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
        fixture.write_bytes(b"#!/bin/sh\nexit 0\n")
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


if __name__ == "__main__":
    unittest.main()
