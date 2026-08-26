from __future__ import annotations

import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts/production_vmnet_certification.py"
SPEC = importlib.util.spec_from_file_location("production_vmnet_certification", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("production vmnet certification module should be importable")
vmnet = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = vmnet
SPEC.loader.exec_module(vmnet)


def sample_result() -> dict[str, object]:
    return {
        "cases": [
            {"name": name, "outcome": "passed"} for name in vmnet.CASE_NAMES
        ],
        "cleanup": "complete",
        "entitlements": {
            "outer_empty": True,
            "worker_app_sandbox_hvf": True,
            "worker_vmnet": True,
        },
        "platform": {
            "architecture": "arm64",
            "hvf": "supported",
            "macos": "26.5.2",
            "sdk": "26.5",
        },
        "schema_version": 1,
        "source": {"commit": "1" * 40, "tree": "2" * 40},
        "verdict": "passed",
    }


def fixture_source(mode: str = "success") -> str:
    return textwrap.dedent(
        f"""\
        #!/bin/sh
        behavior='{mode}'
        [ "$#" -eq 0 ] || exit 17
        [ "$HOME" = "$TMPDIR" ] || exit 18
        [ -z "${{PRIVATE_SENTINEL+x}}" ] || exit 19
        if [ "$behavior" = early-exit ]; then
          exit 20
        fi
        if [ "$behavior" = timeout ]; then
          IFS= read -r ignored || exit 21
          while IFS= read -r ignored; do :; done
          exit 21
        fi
        if [ "$behavior" = ignore-term ]; then
          trap '' TERM
          IFS= read -r ignored || exit 22
          while :; do
            IFS= read -r ignored || :
          done
          exit 22
        fi
        if [ "$behavior" = blocked-output ]; then
          index=0
          while [ "$index" -lt 70000 ]; do
            printf x
            index=$((index + 1))
          done
          exit 26
        fi

        IFS= read -r prepare || exit 23
        case_value=$(printf '%s' "$prepare" | sed -n 's/^{{"case":"\([^"]*\)".*/\\1/p')
        nonce=$(printf '%s' "$prepare" | sed -n 's/.*"nonce":"\([^"]*\)".*/\\1/p')
        response_nonce=$nonce
        if [ "$behavior" = wrong-nonce ]; then
          response_nonce=0000000000000000000000000000000000000000000000000000000000000000
        fi
        printf '{{"case":"%s","endpoint_ipv4":"192.168.42.1","endpoint_port":32123,"kind":"ready","nonce":"%s","schema_version":1}}\\n' "$case_value" "$response_nonce"
        if [ "$behavior" = stderr ]; then
          printf '%s\\n' PRIVATE-SENTINEL >&2
        fi
        printf '{{"case":"%s","kind":"observed","nonce":"%s","schema_version":1}}\\n' "$case_value" "$nonce"
        IFS= read -r cleanup || exit 24
        case "$cleanup" in *'\"kind\":\"cleanup\"'*) ;; *) exit 25 ;; esac
        if [ "$behavior" = residue ]; then
          : > private-residue
        fi
        printf '{{"case":"%s","kind":"complete","nonce":"%s","schema_version":1}}\\n' "$case_value" "$nonce"
        if [ "$behavior" = extra-line ]; then
          printf '{{"case":"%s","kind":"complete","nonce":"%s","schema_version":1}}\\n' "$case_value" "$nonce"
        fi
        if [ "$behavior" = replace-self ]; then
          printf '#' >> "$0"
        fi
        """
    )


class ProductionVmnetCertificationTests(unittest.TestCase):
    def assert_category(self, category: str, callback) -> None:
        with self.assertRaises(vmnet.CertificationError) as caught:
            callback()
        self.assertEqual(caught.exception.category, category)
        self.assertNotIn("PRIVATE-SENTINEL", str(caught.exception))

    def make_private_file(self, path: Path, data: bytes, mode: int = 0o600) -> None:
        path.write_bytes(data)
        path.chmod(mode)

    def make_config(
        self, temp: Path, *, behavior: str = "success"
    ) -> tuple[Path, dict[str, object], Path, Path]:
        profile = temp / "approved.provisionprofile"
        self.make_private_file(profile, b"placeholder profile")
        fixture = temp / "fixture"
        fixture.write_text(fixture_source(behavior), encoding="utf-8")
        fixture.chmod(0o700)
        document: dict[str, object] = {
            "fixture": {
                "executable": os.fspath(fixture),
                "sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
            },
            "optional_cases": {
                "bridged_interface": None,
                "host_connectivity": False,
                "not_authorized": False,
                "sharing_service_busy": False,
            },
            "provisioning_profile": os.fspath(profile),
            "schema_version": 1,
            "signing_identity": "Apple Development: Fixture",
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
        config = temp / "config.json"
        self.make_private_file(config, vmnet.canonical_json(document))
        return config, document, profile, fixture

    def fixture_config(self, path: Path) -> object:
        descriptor, identity = vmnet._open_regular(
            path,
            category="fixture",
            maximum=vmnet.MAX_FIXTURE_BYTES,
            private=False,
            executable=True,
            digest=True,
        )
        os.close(descriptor)
        assert identity.sha256 is not None
        return vmnet.FixtureConfig(path, identity.sha256, identity)

    def test_example_is_canonical_and_contains_only_placeholders(self) -> None:
        example = (
            REPOSITORY_ROOT
            / "scripts/production-vmnet-certification-config.example.json"
        ).read_bytes()
        document = vmnet._decode_document(example)
        self.assertEqual(vmnet.canonical_json(document), example)
        self.assertTrue(document["provisioning_profile"].startswith("/private/path/"))
        self.assertTrue(document["fixture"]["executable"].startswith("/private/path/"))
        self.assertEqual(document["fixture"]["sha256"], "0" * 64)

    def test_private_config_round_trip_and_closed_document(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config, document, _profile, _fixture = self.make_config(temp)
            parsed = vmnet.read_config(config.resolve())
            self.assertEqual(parsed.signing_identity, document["signing_identity"])
            self.assertEqual(parsed.timeouts.fixture_seconds, 5)

            noncanonical = temp / "noncanonical.json"
            self.make_private_file(
                noncanonical,
                json.dumps(document, separators=(",", ":")).encode("ascii"),
            )
            self.assert_category(
                "document", lambda: vmnet.read_config(noncanonical.resolve())
            )

            nonfinite = temp / "nonfinite.json"
            self.make_private_file(
                nonfinite,
                vmnet.canonical_json(document).replace(
                    b'"request_seconds": 5', b'"request_seconds": NaN'
                ),
            )
            self.assert_category(
                "document", lambda: vmnet.read_config(nonfinite.resolve())
            )

            invalid_utf8 = temp / "invalid-utf8.json"
            self.make_private_file(invalid_utf8, b"\xff")
            self.assert_category(
                "document", lambda: vmnet.read_config(invalid_utf8.resolve())
            )

            duplicate = vmnet.canonical_json(document).replace(
                b'{\n  "fixture":', b'{\n  "schema_version": 1,\n  "fixture":', 1
            )
            duplicate_path = temp / "duplicate.json"
            self.make_private_file(duplicate_path, duplicate)
            self.assert_category(
                "document", lambda: vmnet.read_config(duplicate_path.resolve())
            )

            unknown = copy.deepcopy(document)
            unknown["private_sentinel"] = True
            self.assert_category(
                "config", lambda: vmnet.parse_config_document(unknown)
            )
            wrong_schema_type = copy.deepcopy(document)
            wrong_schema_type["schema_version"] = True
            self.assert_category(
                "config", lambda: vmnet.parse_config_document(wrong_schema_type)
            )

    def test_private_config_rejects_modes_links_digest_and_bounds(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config, document, profile, fixture = self.make_config(temp)

            config.chmod(0o644)
            self.assert_category("config", lambda: vmnet.read_config(config.resolve()))
            config.chmod(0o600)

            symlink = temp / "config-link"
            symlink.symlink_to(config)
            self.assert_category("config", lambda: vmnet.read_config(symlink.absolute()))

            profile.chmod(0o644)
            self.assert_category(
                "profile", lambda: vmnet.parse_config_document(document)
            )
            profile.chmod(0o600)
            profile.write_bytes(b"")
            self.assert_category(
                "profile", lambda: vmnet.parse_config_document(document)
            )
            profile.write_bytes(b"placeholder profile")

            fixture.chmod(0o722)
            self.assert_category(
                "fixture", lambda: vmnet.parse_config_document(document)
            )
            fixture.chmod(0o700)
            fixture.chmod(0o4700)
            self.assert_category(
                "fixture", lambda: vmnet.parse_config_document(document)
            )
            fixture.chmod(0o700)

            hardlink = temp / "fixture-hardlink"
            os.link(fixture, hardlink)
            self.assert_category(
                "fixture", lambda: vmnet.parse_config_document(document)
            )
            hardlink.unlink()

            wrong_digest = copy.deepcopy(document)
            wrong_digest["fixture"]["sha256"] = "f" * 64
            self.assert_category(
                "fixture", lambda: vmnet.parse_config_document(wrong_digest)
            )

            bad_identity = copy.deepcopy(document)
            bad_identity["signing_identity"] = "-"
            self.assert_category(
                "identity", lambda: vmnet.parse_config_document(bad_identity)
            )
            for identity in (
                "private\nidentity",
                " Apple Development: Fixture",
                "Apple Development: Fixture ",
                "--private-option",
                "Cafe\u0301 Development",
                "ad hoc",
                "AD-HOC",
                "adhoc",
            ):
                bad_identity["signing_identity"] = identity
                self.assert_category(
                    "identity",
                    lambda value=bad_identity: vmnet.parse_config_document(value),
                )

            bad_bridge = copy.deepcopy(document)
            bad_bridge["optional_cases"]["bridged_interface"] = "bridge/interface"
            self.assert_category(
                "optional-cases", lambda: vmnet.parse_config_document(bad_bridge)
            )

            bad_timeout = copy.deepcopy(document)
            bad_timeout["timeouts"]["request_seconds"] = 61
            self.assert_category(
                "timeouts", lambda: vmnet.parse_config_document(bad_timeout)
            )

            oversized = temp / "oversized.json"
            self.make_private_file(oversized, b"x" * (vmnet.MAX_DOCUMENT_BYTES + 1))
            self.assert_category(
                "config", lambda: vmnet.read_config(oversized.resolve())
            )

    def test_public_result_round_trip_and_verdict_coherence(self) -> None:
        result = sample_result()
        self.assertEqual(vmnet.validate_result_document(result), result)

        optional = copy.deepcopy(result)
        for item in optional["cases"]:
            if item["name"] in vmnet.ENVIRONMENT_GATED_CASES:
                item["outcome"] = "environment-gated"
        self.assertEqual(vmnet.validate_result_document(optional), optional)

        mutations = []
        wrong_schema_type = copy.deepcopy(result)
        wrong_schema_type["schema_version"] = True
        mutations.append(wrong_schema_type)
        wrong_order = copy.deepcopy(result)
        wrong_order["cases"][0], wrong_order["cases"][1] = (
            wrong_order["cases"][1],
            wrong_order["cases"][0],
        )
        mutations.append(wrong_order)
        gated_mandatory = copy.deepcopy(result)
        gated_mandatory["cases"][0]["outcome"] = "environment-gated"
        mutations.append(gated_mandatory)
        incoherent = copy.deepcopy(result)
        incoherent["cases"][0]["outcome"] = "blocked"
        mutations.append(incoherent)
        failed = copy.deepcopy(result)
        failed["verdict"] = "failed"
        mutations.append(failed)
        bad_entitlement = copy.deepcopy(result)
        bad_entitlement["entitlements"]["worker_vmnet"] = False
        mutations.append(bad_entitlement)
        private_platform = copy.deepcopy(result)
        private_platform["platform"]["macos"] = "PRIVATE-SENTINEL"
        mutations.append(private_platform)
        noncanonical_platform = copy.deepcopy(result)
        noncanonical_platform["platform"]["sdk"] = "026.05"
        mutations.append(noncanonical_platform)
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.assert_category(
                    "result", lambda value=mutation: vmnet.validate_result_document(value)
                )

        blocked = copy.deepcopy(result)
        blocked["cases"][0]["outcome"] = "blocked"
        blocked["verdict"] = "blocked"
        self.assertEqual(vmnet.validate_result_document(blocked), blocked)
        failed = copy.deepcopy(result)
        failed["cleanup"] = "incomplete"
        failed["verdict"] = "failed"
        self.assertEqual(vmnet.validate_result_document(failed), failed)

    def test_result_read_and_no_clobber_publication(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            destination = (temp / "result.json").resolve()
            vmnet.publish_result(destination, sample_result())
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o600)
            self.assertEqual(vmnet.read_result(destination), sample_result())
            expected = destination.read_bytes()
            self.assert_category(
                "output", lambda: vmnet.publish_result(destination, sample_result())
            )
            self.assertEqual(destination.read_bytes(), expected)
            self.assertEqual(list(temp.glob(".result.json.*")), [])

    def test_publication_failure_rolls_back_owned_output_and_stage(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            destination = (temp / "result.json").resolve()
            real_fsync = vmnet.os.fsync
            calls = 0

            def fail_directory_fsync(descriptor: int) -> None:
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("PRIVATE-SENTINEL")
                real_fsync(descriptor)

            with mock.patch.object(vmnet.os, "fsync", side_effect=fail_directory_fsync):
                self.assert_category(
                    "output", lambda: vmnet.publish_result(destination, sample_result())
                )
            self.assertFalse(destination.exists())
            self.assertEqual(list(temp.iterdir()), [])

            real_close = vmnet.os.close
            failed = False

            def close_stage_then_fail(descriptor: int) -> None:
                nonlocal failed
                metadata = os.fstat(descriptor)
                real_close(descriptor)
                if not failed and stat.S_ISREG(metadata.st_mode):
                    failed = True
                    raise OSError("PRIVATE-SENTINEL")

            with mock.patch.object(vmnet.os, "close", side_effect=close_stage_then_fail):
                self.assert_category(
                    "output", lambda: vmnet.publish_result(destination, sample_result())
                )
            self.assertTrue(failed)
            self.assertFalse(destination.exists())
            self.assertEqual(list(temp.iterdir()), [])

    def test_publication_preserves_colliding_stage_and_symlink_destination(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            destination = (temp / "result.json").resolve()
            collision = temp / ".result.json.fixed"
            collision.write_bytes(b"PRIVATE-SENTINEL")
            with mock.patch.object(vmnet.secrets, "token_hex", return_value="fixed"):
                self.assert_category(
                    "output", lambda: vmnet.publish_result(destination, sample_result())
                )
            self.assertEqual(collision.read_bytes(), b"PRIVATE-SENTINEL")
            self.assertFalse(destination.exists())

            target = temp / "occupied"
            target.write_bytes(b"unchanged")
            destination.symlink_to(target)
            self.assert_category(
                "output", lambda: vmnet.publish_result(destination, sample_result())
            )
            self.assertTrue(destination.is_symlink())
            self.assertEqual(target.read_bytes(), b"unchanged")

    def test_publication_rejects_a_racing_external_hardlink(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            destination = (temp / "result.json").resolve()
            alias = temp / "hostile-alias"
            real_link = vmnet.os.link

            def link_then_alias(source, target, **kwargs) -> None:
                real_link(source, target, **kwargs)
                real_link(
                    source,
                    alias.name,
                    src_dir_fd=kwargs["src_dir_fd"],
                    dst_dir_fd=kwargs["dst_dir_fd"],
                    follow_symlinks=False,
                )

            with mock.patch.object(vmnet.os, "link", side_effect=link_then_alias):
                self.assert_category(
                    "output", lambda: vmnet.publish_result(destination, sample_result())
                )
            self.assertFalse(destination.exists())
            self.assertTrue(alias.is_file())
            self.assertEqual(list(temp.glob(".result.json.*")), [])

    def test_control_and_fixture_message_contracts(self) -> None:
        nonce = bytes(range(1, 33))
        sector = vmnet.encode_guest_control("shared", "192.168.64.1", 12345, nonce)
        self.assertEqual(len(sector), 512)
        self.assertEqual(
            vmnet.decode_guest_control(sector),
            vmnet.GuestControl("shared", "192.168.64.1", 12345, nonce),
        )
        self.assertEqual(len(vmnet.tcp_request(nonce)), 40)
        self.assertNotEqual(vmnet.tcp_request(nonce), vmnet.tcp_response(nonce))
        self.assert_category(
            "control",
            lambda: vmnet.encode_guest_control("shared", "192.168.64.1", True, nonce),
        )
        self.assert_category("control", lambda: vmnet.decode_guest_control(bytearray(sector)))
        self.assert_category("control", lambda: vmnet.tcp_request(None))
        for offset in (0, 8, 10, 11, 12, 16, 18, 50, 64, 96, 511):
            hostile = bytearray(sector)
            hostile[offset] ^= 1
            self.assert_category(
                "control", lambda value=bytes(hostile): vmnet.decode_guest_control(value)
            )

        nonce_hex = nonce.hex()
        ready = vmnet.canonical_line(
            {
                "case": "shared-connectivity",
                "endpoint_ipv4": "192.168.64.1",
                "endpoint_port": 12345,
                "kind": "ready",
                "nonce": nonce_hex,
                "schema_version": 1,
            }
        )
        endpoint = vmnet.parse_fixture_message(
            ready,
            expected_kind="ready",
            expected_case="shared-connectivity",
            expected_nonce=nonce_hex,
        )
        self.assertEqual(endpoint, vmnet.FixtureEndpoint("192.168.64.1", 12345))
        self.assert_category(
            "fixture-protocol",
            lambda: vmnet.parse_fixture_message(
                ready.replace(b"\n", b" \n"),
                expected_kind="ready",
                expected_case="shared-connectivity",
                expected_nonce=nonce_hex,
            ),
        )
        self.assert_category(
            "fixture-protocol",
            lambda: vmnet.parse_fixture_message(
                ready,
                expected_kind="replayed",
                expected_case="shared-connectivity",
                expected_nonce=nonce_hex,
            ),
        )
        boolean_version = ready.replace(
            b'"schema_version":1', b'"schema_version":true'
        )
        self.assert_category(
            "fixture-protocol",
            lambda: vmnet.parse_fixture_message(
                boolean_version,
                expected_kind="ready",
                expected_case="shared-connectivity",
                expected_nonce=nonce_hex,
            ),
        )

    def test_real_retained_fixture_success_and_strict_state(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            _config, _document, _profile, fixture = self.make_config(temp)
            with vmnet.FixtureSession(
                self.fixture_config(fixture),
                "shared-connectivity",
                bytes(range(1, 33)),
                5,
                session_parent=temp,
            ) as session:
                endpoint = session.prepare()
                self.assertEqual(endpoint, vmnet.FixtureEndpoint("192.168.42.1", 32123))
                session.wait_observed()
                session.complete()
            self.assertEqual(
                list(temp.glob("bangbang-production-vmnet-fixture.*")), []
            )

            session = vmnet.FixtureSession(
                self.fixture_config(fixture),
                "shared-connectivity",
                bytes(range(1, 33)),
                5,
                session_parent=temp,
            )
            self.assert_category("fixture-protocol", session.wait_observed)
            self.assertEqual(
                list(temp.glob("bangbang-production-vmnet-fixture.*")), []
            )

    def test_fixture_failures_are_redacted_and_cleaned(self) -> None:
        for behavior, expected in (
            ("wrong-nonce", "fixture-protocol"),
            ("stderr", "fixture-protocol"),
            ("extra-line", "fixture-protocol"),
            ("blocked-output", "fixture-protocol"),
            ("early-exit", "fixture-protocol"),
            ("replace-self", "fixture"),
        ):
            with self.subTest(behavior=behavior), tempfile.TemporaryDirectory() as raw_temp:
                temp = Path(raw_temp)
                _config, _document, _profile, fixture = self.make_config(
                    temp, behavior=behavior
                )
                session = vmnet.FixtureSession(
                    self.fixture_config(fixture),
                    "shared-connectivity",
                    bytes(range(1, 33)),
                    5,
                    session_parent=temp,
                )

                def exercise() -> None:
                    session.prepare()
                    session.wait_observed()
                    session.complete()

                self.assert_category(expected, exercise)
                self.assertEqual(
                    list(temp.glob("bangbang-production-vmnet-fixture.*")), []
                )

    def test_fixture_timeout_and_incomplete_cleanup_are_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            _config, _document, _profile, fixture = self.make_config(
                temp, behavior="timeout"
            )
            session = vmnet.FixtureSession(
                self.fixture_config(fixture),
                "shared-connectivity",
                bytes(range(1, 33)),
                1,
                terminate_seconds=1,
                session_parent=temp,
            )
            self.assert_category("fixture-timeout", session.prepare)
            self.assertEqual(
                list(temp.glob("bangbang-production-vmnet-fixture.*")), []
            )

        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            _config, _document, _profile, fixture = self.make_config(
                temp, behavior="residue"
            )
            session = vmnet.FixtureSession(
                self.fixture_config(fixture),
                "shared-connectivity",
                bytes(range(1, 33)),
                5,
                session_parent=temp,
            )
            session.prepare()
            session.wait_observed()
            self.assert_category("fixture-cleanup", session.complete)

    def test_fixture_replacement_before_spawn_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            _config, _document, _profile, fixture = self.make_config(temp)
            configured = self.fixture_config(fixture)
            replacement = temp / "replacement"
            replacement.write_text(fixture_source(), encoding="utf-8")
            replacement.chmod(0o700)
            os.replace(replacement, fixture)
            self.assert_category(
                "fixture",
                lambda: vmnet.FixtureSession(
                    configured,
                    "shared-connectivity",
                    bytes(range(1, 33)),
                    5,
                    session_parent=temp,
                ),
            )

    def test_fixture_interrupt_reaps_process_and_directory(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            _config, _document, _profile, fixture = self.make_config(temp)
            session = vmnet.FixtureSession(
                self.fixture_config(fixture),
                "shared-connectivity",
                bytes(range(1, 33)),
                5,
                session_parent=temp,
            )
            with mock.patch.object(session, "_read_line", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    session.prepare()
            self.assertEqual(
                list(temp.glob("bangbang-production-vmnet-fixture.*")), []
            )

    def test_fixture_abort_kills_a_term_ignoring_process_group(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            _config, _document, _profile, fixture = self.make_config(
                temp, behavior="ignore-term"
            )
            session = vmnet.FixtureSession(
                self.fixture_config(fixture),
                "shared-connectivity",
                bytes(range(1, 33)),
                1,
                terminate_seconds=1,
                session_parent=temp,
            )
            self.assert_category("fixture-timeout", session.prepare)
            self.assertEqual(
                list(temp.glob("bangbang-production-vmnet-fixture.*")), []
            )

    def test_cli_is_closed_and_emits_only_fixed_status(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            config, _document, _profile, _fixture = self.make_config(temp)
            valid = subprocess.run(
                (sys.executable, os.fspath(SCRIPT_PATH), "validate-config", "--config", os.fspath(config.resolve())),
                capture_output=True,
                text=True,
            )
            self.assertEqual(valid.returncode, 0, valid.stderr)
            self.assertEqual(valid.stdout, "bangbang production vmnet config: valid\n")
            self.assertEqual(valid.stderr, "")

            internal_stdout = io.StringIO()
            internal_stderr = io.StringIO()
            with mock.patch.object(
                vmnet, "read_config", side_effect=OSError("PRIVATE-SENTINEL")
            ), contextlib.redirect_stdout(internal_stdout), contextlib.redirect_stderr(
                internal_stderr
            ):
                self.assertEqual(
                    vmnet.main(("validate-config", "--config", os.fspath(config))), 3
                )
            self.assertEqual(internal_stdout.getvalue(), "")
            self.assertEqual(
                internal_stderr.getvalue(),
                "bangbang production vmnet config: invalid category=internal\n",
            )

            invalid = subprocess.run(
                (sys.executable, os.fspath(SCRIPT_PATH), "validate-config", "--config", "PRIVATE-SENTINEL"),
                capture_output=True,
                text=True,
            )
            self.assertEqual(invalid.returncode, 3)
            self.assertNotIn("PRIVATE-SENTINEL", invalid.stderr)
            self.assertIn("category=config", invalid.stderr)

            closed = subprocess.run(
                (sys.executable, os.fspath(SCRIPT_PATH), "run", "PRIVATE-SENTINEL"),
                capture_output=True,
                text=True,
            )
            self.assertEqual(closed.returncode, 3)
            self.assertNotIn("PRIVATE-SENTINEL", closed.stdout)
            self.assertNotIn("PRIVATE-SENTINEL", closed.stderr)
            self.assertEqual(
                closed.stderr,
                "bangbang production vmnet invocation: invalid category=invocation\n",
            )

            for hostile_arguments in (
                (
                    "validate-config",
                    "--config",
                    os.fspath(config.resolve()),
                    "--config=PRIVATE-SENTINEL",
                ),
                ("validate-config", "--PRIVATE-SENTINEL"),
                ("PRIVATE-SENTINEL",),
            ):
                hostile = subprocess.run(
                    (sys.executable, os.fspath(SCRIPT_PATH), *hostile_arguments),
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(hostile.returncode, 3)
                self.assertEqual(hostile.stdout, "")
                self.assertEqual(
                    hostile.stderr,
                    "bangbang production vmnet invocation: invalid category=invocation\n",
                )


if __name__ == "__main__":
    unittest.main()
