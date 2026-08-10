from __future__ import annotations

import importlib.util
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts" / "elevated_guest_evidence.py"
SPEC = importlib.util.spec_from_file_location("elevated_guest_evidence", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evidence
SPEC.loader.exec_module(evidence)


class ElevatedGuestEvidenceTests(unittest.TestCase):
    def test_checked_contract_binds_exact_sources_and_canonical_config(self) -> None:
        contract = evidence.load_contract()

        self.assertEqual(
            [resource.resource_id for resource in contract.resources],
            ["kernel", "rootfs", "guest-boot-initrd", "no-api-config"],
        )
        self.assertEqual(
            [resource.bundle_name for resource in contract.resources],
            [
                "evidence-guest-kernel",
                "evidence-guest-rootfs",
                "evidence-guest-initrd",
                "evidence-guest-no-api.json",
            ],
        )
        self.assertTrue(all(resource.mode == 0o400 for resource in contract.resources))
        self.assertEqual(contract.marker_name, "elevated-guest-evidence.enabled")
        self.assertEqual(contract.sidecar_suffix, ".elevated-guest-sidecar")

    def test_prepare_is_rootless_exact_and_independent_from_source_paths(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            sources = root / "sources"
            resources = root / "resources"
            sidecar = root / "sidecar"
            sources.mkdir(mode=0o700)
            resources.mkdir(mode=0o700)
            sidecar.mkdir(mode=0o700)
            specs = []
            for index, name in enumerate(("a", "b", "c", "d")):
                source = sources / name
                contents = bytes([index + 1]) * (index + 3)
                source.write_bytes(contents)
                source.chmod(0o644)
                specs.append(
                    evidence.ResourceSpec(
                        resource_id=name,
                        source=source,
                        bundle_name=f"evidence-guest-{name}",
                        size_bytes=len(contents),
                        sha256=evidence.hashlib.sha256(contents).hexdigest(),
                        mode=0o400,
                    )
                )
            contract = evidence.EvidenceContract(
                tuple(specs),
                "elevated-guest-evidence.enabled",
                b"test-only\n",
                0o600,
                ".elevated-guest-sidecar",
            )

            evidence.prepare(resources, sidecar, contract)
            evidence._verify_directory(resources, contract, include_marker=True)
            evidence._verify_directory(sidecar, contract, include_marker=False)
            for spec in specs:
                self.assertEqual((resources / spec.bundle_name).read_bytes(), spec.source.read_bytes())
                self.assertEqual((sidecar / spec.bundle_name).read_bytes(), spec.source.read_bytes())
                self.assertEqual(
                    stat.S_IMODE((resources / spec.bundle_name).stat().st_mode), 0o400
                )

    def test_prepare_rejects_root_before_destination_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            resources = root / "resources"
            sidecar = root / "sidecar"
            resources.mkdir()
            sidecar.mkdir()
            contract = evidence.EvidenceContract((), "marker", b"x", 0o600, ".sidecar")

            with mock.patch.object(evidence.os, "geteuid", return_value=0):
                with self.assertRaises(evidence.EvidenceError):
                    evidence.prepare(resources, sidecar, contract)

            self.assertEqual(list(resources.iterdir()), [])
            self.assertEqual(list(sidecar.iterdir()), [])

    @unittest.skipIf(os.geteuid() == 0, "exclusive publication requires an ordinary user")
    def test_sidecar_publication_is_exclusive_and_failure_cleanup_is_exact(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            source = root / "source"
            source.write_bytes(b"fixed")
            source.chmod(0o644)
            spec = evidence.ResourceSpec(
                "fixed",
                source,
                "evidence-guest-fixed",
                5,
                evidence.hashlib.sha256(b"fixed").hexdigest(),
                0o400,
            )
            contract = evidence.EvidenceContract(
                (spec,), "marker", b"x", 0o600, ".sidecar"
            )
            stage = root / "stage"
            stage.mkdir()
            evidence._copy_exact(spec, stage / spec.bundle_name)
            destination = root / "published"

            evidence.publish_sidecar(stage, destination, contract)
            self.assertFalse(stage.exists())
            evidence._verify_directory(destination, contract, include_marker=False)

            collision = root / "collision"
            collision.mkdir()
            evidence._copy_exact(spec, collision / spec.bundle_name)
            with self.assertRaises(evidence.EvidenceError):
                evidence.publish_sidecar(collision, destination, contract)
            evidence._verify_directory(collision, contract, include_marker=False)

            evidence.cleanup_sidecar(collision, contract)
            evidence.cleanup_sidecar(destination, contract)
            self.assertFalse(collision.exists())
            self.assertFalse(destination.exists())

    def test_verifier_rejects_tamper_extra_and_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            resource = root / "source"
            resource.write_bytes(b"fixed")
            resource.chmod(0o400)
            spec = evidence.ResourceSpec(
                "fixed",
                resource,
                "evidence-guest-fixed",
                5,
                evidence.hashlib.sha256(b"fixed").hexdigest(),
                0o400,
            )
            contract = evidence.EvidenceContract((spec,), "marker", b"x", 0o600, ".sidecar")
            directory = root / "directory"
            directory.mkdir()
            destination = directory / spec.bundle_name
            destination.write_bytes(b"fixed")
            destination.chmod(0o400)
            evidence._verify_directory(directory, contract, include_marker=False)

            destination.chmod(0o600)
            destination.write_bytes(b"wrong")
            destination.chmod(0o400)
            with self.assertRaises(evidence.EvidenceError):
                evidence._verify_directory(directory, contract, include_marker=False)

            destination.unlink()
            destination.symlink_to(resource)
            with self.assertRaises(evidence.EvidenceError):
                evidence._verify_directory(directory, contract, include_marker=False)

            destination.unlink()
            destination.write_bytes(b"fixed")
            destination.chmod(0o400)
            (directory / "extra").write_bytes(b"extra")
            with self.assertRaises(evidence.EvidenceError):
                evidence._verify_directory(directory, contract, include_marker=False)

    def test_build_and_run_wrappers_never_request_or_load_elevation_credentials(self) -> None:
        for relative in (
            "scripts/build-elevated-bootstrap-probe.sh",
            "scripts/run-elevated-bootstrap-probe.sh",
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

    def test_build_wrapper_keeps_the_published_evidence_pair_on_verify_failure(self) -> None:
        text = (
            REPOSITORY_ROOT / "scripts/build-elevated-bootstrap-probe.sh"
        ).read_text(encoding="utf-8")

        publication = text.index("bundle_published=true")
        bundle_verification = text.index(
            '  --directory "$worker_resources" \\\n  --kind bundle'
        )
        self.assertLess(publication, bundle_verification)
        self.assertIn(
            'if [[ "$bundle_published" != true && "$sidecar_published" == true ]]',
            text,
        )


if __name__ == "__main__":
    unittest.main()
