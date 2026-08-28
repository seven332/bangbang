from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
HELPER_PATH = REPOSITORY_ROOT / "scripts/elevated_vmnet_evidence.py"
PREPARE_PATH = REPOSITORY_ROOT / "scripts/prepare-elevated-vmnet-evidence.sh"
RUN_PATH = REPOSITORY_ROOT / "scripts/run-elevated-vmnet-evidence.sh"
GUEST_PATH = REPOSITORY_ROOT / "scripts/guest/elevated_vmnet_certification.rs"
SPEC = importlib.util.spec_from_file_location("elevated_vmnet_evidence", HELPER_PATH)
assert SPEC is not None and SPEC.loader is not None
evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evidence
SPEC.loader.exec_module(evidence)


def canonical(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode(
        "ascii"
    )


def prepared_package(root: Path) -> None:
    root.chmod(0o700)
    payloads = {
        "bangbang": b"product",
        "elevated-vmnet-e2e": b"harness",
        "bangbang-vmnet-provider": b"provider",
        "elevated-vmnet-provider-e2e": b"provider-harness",
        "vmlinux-6.1.155": b"kernel",
        "ubuntu-24.04-512M-direct-boot-v111.ext4": b"rootfs",
        "ubuntu-24.04-512M-direct-boot-v112.ext4": b"staged-rootfs",
        "elevated-vmnet-evidence.py": b"helper",
        "staged-vmnet-evidence.py": b"staged-helper",
        "staged-vmnet-certification.py": b"staged-protocol",
    }
    for name, payload in payloads.items():
        path = root / name
        path.write_bytes(payload)
        mode = next(mode for candidate, mode, _maximum in evidence.FILES if candidate == name)
        path.chmod(mode)
    for variant in ("direct-boot-v111", "direct-boot-v112"):
        name = f"ubuntu-24.04-512M-{variant}.ext4"
        rootfs = payloads[name]
        sidecar = root / f"{name}.bangbang.json"
        sidecar.write_bytes(
            canonical(
                {
                    "filesystem_check": "e2fsck -fn",
                    "output_sha256": hashlib.sha256(rootfs).hexdigest(),
                    "output_size_bytes": len(rootfs),
                    "recipe_sha256": "1" * 64,
                    "requested_size_bytes": len(rootfs),
                    "schema_version": 1,
                    "source_sha256": "2" * 64,
                    "source_size_bytes": len(rootfs),
                    "tool_versions": {},
                    "variant": variant,
                }
            )
        )
        sidecar.chmod(0o444)
    log = root / evidence.LOG_NAME
    log.write_bytes(b"")
    log.chmod(0o600)


class ElevatedVmnetEvidenceTests(unittest.TestCase):
    def test_create_and_verify_closed_package(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            prepared_package(root)
            evidence.create_manifest(root, os.getuid())
            evidence.verify_manifest(root, os.getuid())
            manifest = root / evidence.MANIFEST_NAME
            self.assertEqual(stat.S_IMODE(manifest.stat().st_mode), 0o444)
            document = json.loads(manifest.read_bytes())
            self.assertEqual(document["ordinary_denial"], "passed")
            self.assertEqual(
                [record["name"] for record in document["files"]],
                [name for name, _mode, _maximum in evidence.FILES],
            )

    def test_verifier_rejects_extra_missing_mode_symlink_and_content_drift(self) -> None:
        mutations = ("extra", "missing", "mode", "symlink", "content")
        for mutation in mutations:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as raw_temp:
                root = Path(raw_temp)
                prepared_package(root)
                evidence.create_manifest(root, os.getuid())
                if mutation == "extra":
                    (root / "extra").write_bytes(b"extra")
                elif mutation == "missing":
                    (root / evidence.LOG_NAME).unlink()
                elif mutation == "mode":
                    (root / "vmlinux-6.1.155").chmod(0o644)
                elif mutation == "symlink":
                    target = root / "vmlinux-6.1.155"
                    target.unlink()
                    target.symlink_to(root / "bangbang")
                else:
                    path = root / "bangbang"
                    path.chmod(0o755)
                    path.write_bytes(b"changed")
                    path.chmod(0o555)
                with self.assertRaises(evidence.EvidenceError):
                    evidence.verify_manifest(root, os.getuid())

    def test_verifier_rejects_stale_sidecars(self) -> None:
        for variant in ("direct-boot-v111", "direct-boot-v112"):
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as raw_temp:
                root = Path(raw_temp)
                prepared_package(root)
                sidecar = root / f"ubuntu-24.04-512M-{variant}.ext4.bangbang.json"
                value = json.loads(sidecar.read_bytes())
                value["variant"] = "direct-boot-v110"
                sidecar.chmod(0o644)
                sidecar.write_bytes(canonical(value))
                sidecar.chmod(0o444)
                with self.assertRaisesRegex(evidence.EvidenceError, "sidecar"):
                    evidence.create_manifest(root, os.getuid())

    def test_verifier_rejects_noncanonical_duplicate_and_unknown_manifest(self) -> None:
        for mutation in ("noncanonical", "duplicate", "unknown"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as raw_temp:
                root = Path(raw_temp)
                prepared_package(root)
                evidence.create_manifest(root, os.getuid())
                manifest = root / evidence.MANIFEST_NAME
                value = json.loads(manifest.read_bytes())
                manifest.chmod(0o644)
                if mutation == "noncanonical":
                    manifest.write_text(json.dumps(value), encoding="ascii")
                elif mutation == "duplicate":
                    manifest.write_text(
                        '{"schema_version":1,"schema_version":1}\n', encoding="ascii"
                    )
                else:
                    value["unknown"] = True
                    manifest.write_bytes(canonical(value))
                manifest.chmod(0o444)
                with self.assertRaises(evidence.EvidenceError):
                    evidence.verify_manifest(root, os.getuid())

    def test_cli_is_fixed_and_redacted(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(evidence.main(["verify", "--directory", "/secret"]), 1)
        self.assertEqual(stderr.getvalue(), "bangbang elevated vmnet evidence: invocation\n")
        self.assertNotIn("/secret", stderr.getvalue())

    def test_wrappers_have_split_authority_and_closed_root_execution(self) -> None:
        prepare = PREPARE_PATH.read_text(encoding="utf-8")
        runner = RUN_PATH.read_text(encoding="utf-8")
        self.assertIn("direct-boot-v111", prepare)
        self.assertIn("direct-boot-v112", prepare)
        self.assertIn("staged_vmnet_evidence.py", prepare)
        self.assertIn("staged_vmnet_certification.py", prepare)
        self.assertIn("ordinary_user_vmnet_start_is_denied", prepare)
        self.assertIn("ordinary denial residue", prepare)
        self.assertIn("cargo test", prepare)
        self.assertNotIn("sudo", prepare.lower())
        self.assertIn("exec </dev/null", runner)
        self.assertIn("/usr/bin/env -i", runner)
        self.assertIn("dropped_owner_retains_bounded_vmnet_io", runner)
        self.assertIn("dropped_provider_serves_data_lifecycle", runner)
        self.assertIn("control_cancellation_reaps_dropped_provider", runner)
        self.assertEqual(runner.count("dropped_provider_serves_data_lifecycle"), 2)
        self.assertEqual(runner.count("elevated_direct_guest_uses_shared_vmnet"), 2)
        self.assertIn('for scenario in startup runtime restore', runner)
        self.assertIn('run_staged_case "$scenario"', runner)
        self.assertIn("PYTHONDONTWRITEBYTECODE=1", runner)
        self.assertIn('final_count="$(/usr/bin/find -x "$stage"', runner)
        self.assertNotIn("sudo", runner.lower())
        self.assertNotIn("SUDO_", runner)
        self.assertNotIn("dscacheutil", runner)
        self.assertNotIn("ps ", runner)
        self.assertEqual(runner.count('find -x "$stage/runs"'), 7)

    def test_guest_oracle_protocol_failures_are_portable(self) -> None:
        compiler = os.environ.get("RUSTC") or shutil.which("rustc")
        self.assertIsNotNone(compiler, "rustc is required by the workspace")
        with tempfile.TemporaryDirectory() as raw_temp:
            test_binary = Path(raw_temp) / "elevated-vmnet-guest-tests"
            built = subprocess.run(
                (
                    os.fspath(compiler),
                    "--edition=2024",
                    "--test",
                    os.fspath(GUEST_PATH),
                    "-o",
                    os.fspath(test_binary),
                ),
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(built.returncode, 0, built.stderr)
            tested = subprocess.run(
                (os.fspath(test_binary), "--test-threads=1"),
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(tested.returncode, 0, tested.stdout + tested.stderr)

    def test_wrapper_help_and_fail_closed_invocation_are_portable(self) -> None:
        for script in (PREPARE_PATH, RUN_PATH):
            result = subprocess.run(
                (os.fspath(script), "--help"),
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn(str(REPOSITORY_ROOT), result.stdout)

        missing = subprocess.run(
            (os.fspath(RUN_PATH),),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(missing.returncode, 2)
        self.assertEqual(missing.stdout, "")
        self.assertNotIn(str(REPOSITORY_ROOT), missing.stderr)


if __name__ == "__main__":
    unittest.main()
