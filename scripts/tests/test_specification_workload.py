from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, os.fspath(REPOSITORY_ROOT / "scripts"))

import specification_workload as workload  # noqa: E402
import guest_artifact_policy as artifact_policy  # noqa: E402


STORAGE_CHECKSUM = 12_345_678_901
VALID_TRANSCRIPT = (
    b"Linux boot noise\r\n"
    b"BANGBANG_SPEC_WORKLOAD_V1\r\n"
    b"BANGBANG_SPEC_INIT_READY release_byte=82\r\n"
    b"BANGBANG_SPEC_COMPUTE duration_ns=0 operations=5000000 "
    b"checksum=8398723902783368615\r\n"
    b"BANGBANG_SPEC_STORAGE duration_ns=987654 bytes=16777216 "
    b"block_bytes=4096 checksum=12345678901\r\n"
    b"BANGBANG_SPEC_WORKLOAD_OK\r\n"
    b"shutdown noise\r\n"
)


class SpecificationWorkloadParserTests(unittest.TestCase):
    def test_complete_transcript_parses_without_performance_thresholds(self) -> None:
        result = workload.parse_transcript(
            VALID_TRANSCRIPT,
            expected_storage_checksum=STORAGE_CHECKSUM,
        )

        self.assertEqual(result.compute_duration_ns, 0)
        self.assertEqual(result.compute_operations, workload.COMPUTE_OPERATIONS)
        self.assertEqual(result.compute_checksum, workload.COMPUTE_CHECKSUM)
        self.assertEqual(result.storage_duration_ns, 987_654)
        self.assertEqual(result.storage_bytes, workload.STORAGE_BYTES)
        self.assertEqual(result.storage_block_bytes, workload.STORAGE_BLOCK_BYTES)
        self.assertEqual(result.storage_checksum, STORAGE_CHECKSUM)
        self.assertEqual(workload.RELEASE_BYTE, b"R")

    def test_missing_duplicate_reordered_and_unknown_records_fail(self) -> None:
        records = [
            line
            for line in VALID_TRANSCRIPT.splitlines()
            if line.startswith(b"BANGBANG_SPEC_")
        ]
        cases = {
            "missing": records[:-1],
            "duplicate": [*records, records[-1]],
            "reordered": [records[0], records[1], records[3], records[2], records[4]],
            "unknown": [*records[:-1], b"BANGBANG_SPEC_FUTURE", records[-1]],
        }
        for name, lines in cases.items():
            with self.subTest(name=name), self.assertRaises(
                workload.SpecificationWorkloadError
            ):
                workload.parse_transcript(b"\n".join(lines))

    def test_malformed_and_constant_drifted_records_fail(self) -> None:
        replacements = {
            "leading zero": (b"duration_ns=0", b"duration_ns=00"),
            "u64 overflow": (
                b"duration_ns=0",
                b"duration_ns=18446744073709551616",
            ),
            "release": (b"release_byte=82", b"release_byte=10"),
            "operations": (b"operations=5000000", b"operations=4999999"),
            "compute checksum": (
                b"checksum=8398723902783368615",
                b"checksum=8398723902783368614",
            ),
            "storage bytes": (b"bytes=16777216", b"bytes=16777215"),
            "block bytes": (b"block_bytes=4096", b"block_bytes=512"),
            "storage checksum": (b"checksum=12345678901", b"checksum=12345678900"),
        }
        for name, (before, after) in replacements.items():
            with self.subTest(name=name), self.assertRaises(
                workload.SpecificationWorkloadError
            ):
                workload.parse_transcript(
                    VALID_TRANSCRIPT.replace(before, after, 1),
                    expected_storage_checksum=STORAGE_CHECKSUM,
                )

    def test_guest_failure_and_non_line_aligned_record_fail(self) -> None:
        with self.assertRaisesRegex(
            workload.SpecificationWorkloadError,
            "failed in phase release-eof",
        ):
            workload.parse_transcript(
                b"BANGBANG_SPEC_WORKLOAD_V1\n"
                b"BANGBANG_SPEC_WORKLOAD_FAIL phase=release-eof\n"
            )
        with self.assertRaisesRegex(
            workload.SpecificationWorkloadError,
            "not line-aligned",
        ):
            workload.parse_transcript(b"noise BANGBANG_SPEC_WORKLOAD_V1\n")
        with self.assertRaisesRegex(
            workload.SpecificationWorkloadError,
            "failed in phase poweroff",
        ):
            workload.parse_transcript(
                VALID_TRANSCRIPT
                + b"BANGBANG_SPEC_WORKLOAD_FAIL phase=poweroff\n"
            )


class SpecificationWorkloadBuildBoundaryTests(unittest.TestCase):
    def test_direct_rootfs_variants_install_helpers_with_identical_closed_flags(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            populate = temp / "rootfs"
            populate.mkdir()
            sysroot = temp / "sysroot"
            target_libdir = sysroot / "lib/rustlib/aarch64-unknown-linux-musl/lib"
            target_libdir.mkdir(parents=True)
            (target_libdir / "libcore-test.rlib").write_bytes(b"core")
            host_bin = sysroot / "lib/rustlib/fake-host/bin"
            host_bin.mkdir(parents=True)
            rust_lld = host_bin / "rust-lld"
            rust_lld.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            rust_lld.chmod(0o755)

            log_path = temp / "rustc.jsonl"
            fake_rustc = temp / "rustc"
            fake_rustc.write_text(
                """#!/usr/bin/env python3
import json
import os
import struct
import sys
from pathlib import Path

args = sys.argv[1:]
sysroot = Path(os.environ["FAKE_RUST_SYSROOT"])
mode = os.environ.get("FAKE_RUST_MODE", "static")
if args[:2] == ["--print", "target-libdir"]:
    target = "missing-target" if mode == "missing-target" else "aarch64-unknown-linux-musl"
    print(sysroot / f"lib/rustlib/{target}/lib")
elif args == ["--print", "sysroot"]:
    print(sysroot)
elif args == ["-vV"]:
    host = "missing-host" if mode == "missing-linker" else "fake-host"
    print(f"host: {host}")
else:
    with Path(os.environ["FAKE_RUST_LOG"]).open("a", encoding="utf-8") as log:
        log.write(json.dumps(args) + "\\n")
    output = Path(args[args.index("-o") + 1])
    if mode == "malformed":
        output.write_bytes(b"not an ELF")
    else:
        identity = b"\\x7fELF\\x02\\x01\\x01" + bytes(9)
        image_size = 64 + 56
        header = struct.pack(
            "<16sHHIQQQIHHHHHH",
            identity,
            2,
            183,
            1,
            0x400000,
            64,
            0,
            0,
            64,
            56,
            1,
            0,
            0,
            0,
        )
        segment_type = 3 if mode == "dynamic" else 1
        program = struct.pack(
            "<IIQQQQQQ",
            segment_type,
            5,
            0,
            0x400000,
            0x400000,
            image_size,
            image_size,
            4096,
        )
        output.write_bytes(header + program)
""",
                encoding="utf-8",
            )
            fake_rustc.chmod(0o755)

            environment = dict(os.environ)
            environment.update(
                {
                    "BANGBANG_GUEST_POLICY_INTERNAL": "1",
                    "BANGBANG_GUEST_POLICY_VARIANT": "direct-boot-v110",
                    "BANGBANG_RUSTC": os.fspath(fake_rustc),
                    "FAKE_RUST_LOG": os.fspath(log_path),
                    "FAKE_RUST_SYSROOT": os.fspath(sysroot),
                }
            )
            result = subprocess.run(
                (
                    os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),
                    "--internal-populate-direct",
                    os.fspath(populate),
                ),
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            report = populate / "bangbang-arm64-id-register-report"
            benchmark = populate / "bangbang-specification-benchmark"
            vmnet = populate / "bangbang-production-vmnet-certification"
            init = populate / "bangbang-direct-rootfs-init"
            self.assertTrue(report.is_file())
            self.assertTrue(benchmark.is_file())
            self.assertTrue(vmnet.is_file())
            self.assertEqual(stat.S_IMODE(report.stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE(benchmark.stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE(vmnet.stat().st_mode), 0o555)
            self.assertEqual(
                vmnet.read_bytes(),
                (
                    REPOSITORY_ROOT
                    / "scripts/guest/production_vmnet_certification.py"
                ).read_bytes(),
            )
            init_text = init.read_text(encoding="utf-8")
            selector = "cmdline_has bangbang.production-vmnet-certification=1"
            self.assertEqual(init_text.count(selector), 1)
            self.assertLess(
                init_text.index(selector),
                init_text.index("write_vdb_marker BANGBANG_DIRECT_ROOTFS_BLOCK_OK"),
            )

            invocations = [
                json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(invocations), 2)
            self.assertEqual(
                [Path(arguments[0]).name for arguments in invocations],
                ["arm64-id-register-report.rs", "specification-benchmark.rs"],
            )
            normalized: list[list[str]] = []
            for arguments in invocations:
                output_index = arguments.index("-o")
                normalized.append(
                    ["<source>", *arguments[1:output_index], "-o", "<output>"]
                )
            self.assertEqual(normalized[0], normalized[1])
            self.assertIn("--target", normalized[0])
            self.assertIn("aarch64-unknown-linux-musl", normalized[0])
            self.assertIn("-C", normalized[0])
            self.assertIn("link-arg=-static", normalized[0])
            self.assertIn("relocation-model=static", normalized[0])

            v111_root = temp / "rootfs-v111"
            v111_root.mkdir()
            v111_log = temp / "rustc-v111.jsonl"
            v111_environment = dict(environment)
            v111_environment["BANGBANG_GUEST_POLICY_VARIANT"] = "direct-boot-v111"
            v111_environment["FAKE_RUST_LOG"] = os.fspath(v111_log)
            v111_result = subprocess.run(
                (
                    os.fspath(REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"),
                    "--internal-populate-direct",
                    os.fspath(v111_root),
                ),
                env=v111_environment,
                capture_output=True,
                text=True,
            )
            self.assertEqual(v111_result.returncode, 0, v111_result.stderr)
            v111_entry = v111_root / "bangbang-elevated-vmnet-certification"
            self.assertTrue(v111_entry.is_file())
            self.assertEqual(stat.S_IMODE(v111_entry.stat().st_mode), 0o755)
            v111_invocations = [
                json.loads(line)
                for line in v111_log.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(v111_invocations), 3)
            self.assertEqual(
                [Path(arguments[0]).name for arguments in v111_invocations],
                [
                    "arm64-id-register-report.rs",
                    "specification-benchmark.rs",
                    "elevated_vmnet_certification.rs",
                ],
            )
            v111_normalized: list[list[str]] = []
            for arguments in v111_invocations:
                output_index = arguments.index("-o")
                v111_normalized.append(
                    ["<source>", *arguments[1:output_index], "-o", "<output>"]
                )
            self.assertEqual(v111_normalized, [normalized[0]] * 3)
            self.assertIn("-D", v111_normalized[2])
            self.assertIn("warnings", v111_normalized[2])

            for mode, expected_error in (
                ("missing-target", "Rust target aarch64-unknown-linux-musl is required"),
                ("missing-linker", "does not provide rust-lld"),
                ("malformed", "shorter than an ELF64 header"),
                ("dynamic", "dynamic or interpreter segment"),
            ):
                with self.subTest(rejected_boundary=mode):
                    rejected_root = temp / f"rejected-{mode}"
                    rejected_root.mkdir()
                    rejected_environment = dict(environment)
                    rejected_environment["FAKE_RUST_MODE"] = mode
                    rejected = subprocess.run(
                        (
                            os.fspath(
                                REPOSITORY_ROOT / "scripts/fetch-firecracker-rootfs.sh"
                            ),
                            "--internal-populate-direct",
                            os.fspath(rejected_root),
                        ),
                        env=rejected_environment,
                        capture_output=True,
                        text=True,
                    )
                    self.assertNotEqual(rejected.returncode, 0)
                    self.assertIn(expected_error, rejected.stderr)

    def test_recipe_digest_tracks_the_workload_source(self) -> None:
        manifest = json.loads(
            (
                REPOSITORY_ROOT
                / "compat/firecracker/v1.16.0/guest-workflow-audit.json"
            ).read_text(encoding="utf-8")
        )
        direct = next(
            recipe
            for recipe in manifest["ext4_recipes"]
            if recipe["id"] == "rootfs-ext4-direct-boot-v110"
        )
        self.assertEqual(
            direct["tracked_inputs"],
            [
                "compat/firecracker/v1.16.0/guest-workflow-audit.json",
                "scripts/fetch-firecracker-rootfs.sh",
                "scripts/guest/arm64-id-register-report.rs",
                "scripts/guest/production_vmnet_certification.py",
                "scripts/guest/specification-benchmark.rs",
                "scripts/guest_artifact_policy.py",
            ],
        )

        parsed = artifact_policy.load_manifest()
        recipe = parsed.recipes["rootfs-ext4-direct-boot-v110"]
        baseline = artifact_policy._recipe_digest(recipe)
        helper = REPOSITORY_ROOT / "scripts/guest/production_vmnet_certification.py"
        real_sha256 = artifact_policy._sha256

        def changed_helper(path: Path) -> str:
            if path == helper:
                return "f" * 64
            return real_sha256(path)

        with mock.patch.object(
            artifact_policy, "_sha256", side_effect=changed_helper
        ):
            self.assertNotEqual(artifact_policy._recipe_digest(recipe), baseline)

        v111 = next(
            recipe
            for recipe in manifest["ext4_recipes"]
            if recipe["id"] == "rootfs-ext4-direct-boot-v111"
        )
        self.assertEqual(
            v111["tracked_inputs"],
            [
                "compat/firecracker/v1.16.0/guest-workflow-audit.json",
                "scripts/fetch-firecracker-rootfs.sh",
                "scripts/guest/arm64-id-register-report.rs",
                "scripts/guest/elevated_vmnet_certification.rs",
                "scripts/guest/specification-benchmark.rs",
                "scripts/guest_artifact_policy.py",
            ],
        )


if __name__ == "__main__":
    unittest.main()
