from __future__ import annotations

import contextlib
import hashlib
import io
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

import guest_artifact_policy as policy  # noqa: E402


def completed(arguments: object, returncode: int = 0) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(arguments, returncode)


def small_manifest(download_bytes: bytes = b"download", generated_bytes: bytes = b"generated") -> policy.GuestWorkflowManifest:
    download = policy.DownloadArtifact(
        artifact_id="kernel",
        kind="linux-kernel",
        filename="kernel",
        url="https://example.invalid/kernel",
        sha256=hashlib.sha256(download_bytes).hexdigest(),
        size_bytes=len(download_bytes),
        cache_path=Path("downloads/kernel"),
    )
    generated = policy.GeneratedArtifact(
        artifact_id="guest-boot-initrd",
        generator_path=Path("scripts/build-guest-boot-initrd.py"),
        cache_path=Path("generated/initrd"),
        sha256=hashlib.sha256(generated_bytes).hexdigest(),
        size_bytes=len(generated_bytes),
    )
    return policy.GuestWorkflowManifest(
        downloads={"kernel": download},
        generated={"guest-boot-initrd": generated},
        recipes={},
    )


def recipe_expected(size: int = 4, variant: str = "normal") -> dict[str, object]:
    return {
        "schema_version": 1,
        "source_sha256": "1" * 64,
        "source_size_bytes": 8,
        "requested_size_bytes": size,
        "variant": variant,
        "recipe_sha256": "2" * 64,
        "tool_versions": {
            "unsquashfs": "unsquashfs 1",
            "mkfs.ext4": "mke2fs 1",
            "e2fsck": "e2fsck 1",
        },
        "filesystem_check": "e2fsck -fn",
    }


class GuestArtifactPolicyTests(unittest.TestCase):
    def test_checked_manifest_is_strict_and_public_cli_is_closed(self) -> None:
        manifest = policy.load_manifest()
        self.assertEqual(list(manifest.downloads), ["kernel", "rootfs"])
        self.assertEqual(
            list(manifest.recipes),
            [
                "rootfs-ext4",
                "rootfs-ext4-direct-boot-v110",
                "rootfs-ext4-direct-boot-v111",
                "rootfs-ext4-direct-boot-v112",
            ],
        )

        source = policy.MANIFEST_PATH.read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            duplicate = temp / "duplicate.json"
            duplicate.write_text(
                source.replace(
                    '"schema_version": 1,',
                    '"schema_version": 1,\n  "schema_version": 1,',
                    1,
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "duplicate manifest key"):
                policy.load_manifest(duplicate)

            unknown_data = json.loads(source)
            unknown_data["final"] = True
            unknown = temp / "unknown.json"
            unknown.write_text(json.dumps(unknown_data), encoding="utf-8")
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "unknown keys"):
                policy.load_manifest(unknown)

            escaping_data = json.loads(source)
            escaping_data["artifacts"][0]["cache_path"] = "../kernel"
            escaping = temp / "escaping.json"
            escaping.write_text(json.dumps(escaping_data), encoding="utf-8")
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "safe relative path"):
                policy.load_manifest(escaping)

        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            policy._parse_args(["fetch", "kernel", "--url", "https://example.invalid"])
        self.assertEqual(caught.exception.code, 2)
        parsed = policy._parse_args(
            ["prepare-ext4", "--variant", "direct-boot-v111", "--size", "512M"]
        )
        self.assertEqual(parsed.variant, "direct-boot-v111")
        parsed = policy._parse_args(
            ["prepare-ext4", "--variant", "direct-boot-v112", "--size", "512M"]
        )
        self.assertEqual(parsed.variant, "direct-boot-v112")
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            policy._parse_args(["prepare-ext4", "--variant", "custom", "--size", "1G"])
        self.assertEqual(caught.exception.code, 2)
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            policy._parse_args(
                ["prepare-ext4", "--variant", "direct-boot-v109", "--size", "512M"]
            )
        self.assertEqual(caught.exception.code, 2)

    def test_fixed_cli_keeps_result_on_stdout_and_diagnostics_on_stderr(self) -> None:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.object(policy, "fetch_artifact", return_value=Path("/cache/kernel")):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                self.assertEqual(policy.main(["fetch", "kernel"]), 0)
        self.assertEqual(stdout.getvalue(), "/cache/kernel\n")
        self.assertEqual(stderr.getvalue(), "")

        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.object(
            policy,
            "fetch_artifact",
            side_effect=policy.ArtifactPolicyError("busy", "cache busy"),
        ):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                self.assertEqual(policy.main(["fetch", "kernel"]), 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual(stderr.getvalue(), "guest artifact policy: busy: cache busy\n")

    def test_child_tool_stdout_is_routed_to_diagnostics(self) -> None:
        with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as diagnostics:
            with mock.patch.object(policy.sys, "stderr", diagnostics):
                result = policy._run_child(
                    (sys.executable, "-c", "print('tool progress')")
                )
            diagnostics.seek(0)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(diagnostics.read(), "tool progress\n")

    def test_download_cache_reuses_exact_size_hash_and_normalizes_mode(self) -> None:
        content = b"download"
        manifest = small_manifest(content)
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            target = root / "downloads/kernel"
            target.parent.mkdir(parents=True)
            target.write_bytes(content)
            target.chmod(0o600)

            def unexpected_runner(*args: object, **kwargs: object) -> subprocess.CompletedProcess[str]:
                self.fail("valid cache must not invoke curl")

            diagnostics = io.StringIO()
            result = policy.fetch_artifact(
                "kernel",
                manifest=manifest,
                root=root,
                runner=unexpected_runner,
                stderr=diagnostics,
            )
            self.assertEqual(result, target)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o644)
            self.assertIn("using cached", diagnostics.getvalue())
            self.assertIn("normalizing", diagnostics.getvalue())

    def test_download_repairs_invalid_regular_cache_and_cleans_stages(self) -> None:
        content = b"download"
        manifest = small_manifest(content)
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            target = root / "downloads/kernel"
            target.parent.mkdir(parents=True)
            target.write_bytes(b"wrong")

            def runner(arguments: object, **kwargs: object) -> subprocess.CompletedProcess[str]:
                command = list(arguments)  # type: ignore[arg-type]
                output = Path(command[command.index("--output") + 1])
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
                output.write_bytes(content)
                return completed(arguments)

            diagnostics = io.StringIO()
            policy.fetch_artifact(
                "kernel", manifest=manifest, root=root, runner=runner, stderr=diagnostics
            )
            self.assertEqual(target.read_bytes(), content)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o644)
            self.assertIn("repairing", diagnostics.getvalue())
            self.assertEqual(list(target.parent.glob(".kernel.*.download")), [])

            def failing_runner(arguments: object, **kwargs: object) -> subprocess.CompletedProcess[str]:
                return completed(arguments, 22)

            target.write_bytes(b"bad")
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "curl failed"):
                policy.fetch_artifact(
                    "kernel", manifest=manifest, root=root, runner=failing_runner
                )
            self.assertEqual(target.read_bytes(), b"bad")
            self.assertEqual(list(target.parent.glob(".kernel.*.download")), [])

    def test_download_rejects_every_nonregular_final_object(self) -> None:
        manifest = small_manifest()
        constructors = {
            "symlink": lambda path: path.symlink_to("missing"),
            "directory": lambda path: path.mkdir(),
            "fifo": lambda path: os.mkfifo(path),
        }
        for expected_kind, constructor in constructors.items():
            with self.subTest(expected_kind=expected_kind):
                with tempfile.TemporaryDirectory() as raw_temp:
                    root = Path(raw_temp)
                    target = root / "downloads/kernel"
                    target.parent.mkdir(parents=True)
                    constructor(target)
                    with self.assertRaisesRegex(policy.ArtifactPolicyError, expected_kind):
                        policy.fetch_artifact("kernel", manifest=manifest, root=root)

    def test_download_lock_contention_is_fail_fast(self) -> None:
        manifest = small_manifest()
        with tempfile.TemporaryDirectory() as raw_temp:
            root = Path(raw_temp)
            target = root / "downloads/kernel"
            target.parent.mkdir(parents=True)
            with policy.CacheLock(target):
                with self.assertRaisesRegex(policy.ArtifactPolicyError, "cache is busy") as caught:
                    policy.fetch_artifact("kernel", manifest=manifest, root=root)
            self.assertEqual(caught.exception.category, "busy")
            self.assertTrue((target.parent / ".kernel.lock").is_file())
            self.assertEqual(
                stat.S_IMODE((target.parent / ".kernel.lock").stat().st_mode),
                0o600,
            )

    def test_absent_only_publication_is_atomic_and_preserves_stage_mode(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            stage = temp / "stage"
            destination = temp / "signed"
            stage.write_bytes(b"binary")
            stage.chmod(0o755)
            self.assertEqual(
                policy.publish_staged_absent(stage, destination, allow_identical=False),
                "published",
            )
            self.assertFalse(stage.exists())
            self.assertEqual(destination.read_bytes(), b"binary")
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o755)

    def test_absent_only_collisions_leave_destination_and_stage_unchanged(self) -> None:
        constructors = {
            "regular": lambda path: path.write_bytes(b"occupied"),
            "symlink": lambda path: path.symlink_to("missing"),
            "directory": lambda path: path.mkdir(),
            "fifo": lambda path: os.mkfifo(path),
        }
        for expected_kind, constructor in constructors.items():
            with self.subTest(expected_kind=expected_kind):
                with tempfile.TemporaryDirectory() as raw_temp:
                    temp = Path(raw_temp)
                    stage = temp / "stage"
                    destination = temp / "output"
                    stage.write_bytes(b"new")
                    constructor(destination)
                    before = os.lstat(destination)
                    with self.assertRaisesRegex(policy.ArtifactPolicyError, expected_kind):
                        policy.publish_staged_absent(
                            stage, destination, allow_identical=False
                        )
                    after = os.lstat(destination)
                    self.assertEqual((before.st_dev, before.st_ino), (after.st_dev, after.st_ino))
                    self.assertEqual(stage.read_bytes(), b"new")

    def test_identical_publication_reuses_but_different_bytes_collide(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            destination = temp / "output"
            destination.write_bytes(b"same")
            stage = temp / "same-stage"
            stage.write_bytes(b"same")
            self.assertEqual(
                policy.publish_staged_absent(stage, destination, allow_identical=True),
                "reused",
            )
            self.assertFalse(stage.exists())

            different = temp / "different-stage"
            different.write_bytes(b"different")
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "occupied"):
                policy.publish_staged_absent(
                    different, destination, allow_identical=True
                )
            self.assertEqual(destination.read_bytes(), b"same")
            self.assertEqual(different.read_bytes(), b"different")

    def test_unsupported_hard_links_and_sync_failure_leave_final_absent(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            stage = temp / "stage"
            destination = temp / "output"
            stage.write_bytes(b"data")
            with mock.patch.object(policy.os, "link", side_effect=OSError(errno_value("EXDEV"), "cross-device")):
                with self.assertRaisesRegex(policy.ArtifactPolicyError, "unsupported"):
                    policy.publish_staged_absent(stage, destination, allow_identical=False)
            self.assertFalse(destination.exists())
            self.assertTrue(stage.exists())

            with mock.patch.object(
                policy,
                "_sync_directory",
                side_effect=policy.ArtifactPolicyError("sync", "injected sync failure"),
            ):
                with self.assertRaisesRegex(policy.ArtifactPolicyError, "sync failure"):
                    policy.publish_staged_absent(stage, destination, allow_identical=False)
            self.assertFalse(destination.exists())
            self.assertTrue(stage.exists())

    def test_generated_cache_refresh_and_explicit_no_clobber(self) -> None:
        data = b"generated"
        manifest = small_manifest(generated_bytes=data)
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            cache = temp / "cache/initrd"
            self.assertEqual(
                policy.publish_generated_bytes(
                    cache, data, managed_cache=True, manifest=manifest
                ),
                "published",
            )
            self.assertEqual(
                policy.publish_generated_bytes(
                    cache, data, managed_cache=True, manifest=manifest
                ),
                "reused",
            )
            cache.write_bytes(b"old")
            diagnostics = io.StringIO()
            self.assertEqual(
                policy.publish_generated_bytes(
                    cache,
                    data,
                    managed_cache=True,
                    manifest=manifest,
                    stderr=diagnostics,
                ),
                "refreshed",
            )
            self.assertIn("refreshing", diagnostics.getvalue())

            explicit = temp / "caller/initrd"
            explicit.parent.mkdir()
            explicit.write_bytes(b"occupied")
            before = explicit.read_bytes()
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "occupied"):
                policy.publish_generated_bytes(
                    explicit, data, managed_cache=False, manifest=manifest
                )
            self.assertEqual(explicit.read_bytes(), before)
            explicit.write_bytes(data)
            self.assertEqual(
                policy.publish_generated_bytes(
                    explicit, data, managed_cache=False, manifest=manifest
                ),
                "reused",
            )

    def test_generated_bytes_must_match_checked_identity_before_mutation(self) -> None:
        manifest = small_manifest(generated_bytes=b"expected")
        with tempfile.TemporaryDirectory() as raw_temp:
            output = Path(raw_temp) / "output"
            with self.assertRaisesRegex(policy.ArtifactPolicyError, "do not match"):
                policy.publish_generated_bytes(
                    output, b"different", managed_cache=True, manifest=manifest
                )
            self.assertFalse(output.exists())

    def test_initrd_wrapper_reuses_identical_explicit_output_and_rejects_difference(self) -> None:
        script = REPOSITORY_ROOT / "scripts/build-guest-boot-initrd.py"
        with tempfile.TemporaryDirectory() as raw_temp:
            output = Path(raw_temp) / "initrd.cpio"
            first = subprocess.run(
                (os.fspath(script), "--check", "--output", os.fspath(output)),
                capture_output=True,
                text=True,
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            first_metadata = os.lstat(output)
            second = subprocess.run(
                (os.fspath(script), "--check", "--output", os.fspath(output)),
                capture_output=True,
                text=True,
            )
            self.assertEqual(second.returncode, 0, second.stderr)
            second_metadata = os.lstat(output)
            self.assertEqual(
                (first_metadata.st_dev, first_metadata.st_ino),
                (second_metadata.st_dev, second_metadata.st_ino),
            )

            output.write_bytes(b"occupied")
            occupied_metadata = os.lstat(output)
            failed = subprocess.run(
                (os.fspath(script), "--output", os.fspath(output)),
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("collision", failed.stderr)
            after_metadata = os.lstat(output)
            self.assertEqual(
                (occupied_metadata.st_dev, occupied_metadata.st_ino),
                (after_metadata.st_dev, after_metadata.st_ino),
            )
            self.assertEqual(output.read_bytes(), b"occupied")

    def test_recipe_cache_build_reuse_and_sidecar_matrix(self) -> None:
        expected = recipe_expected()
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            output = temp / "rootfs.ext4"
            sidecar = temp / "rootfs.ext4.bangbang.json"
            builds = []

            def build(path: Path) -> None:
                builds.append(path)
                path.write_bytes(b"data")

            self.assertEqual(
                policy.ensure_recipe_cache(
                    output,
                    sidecar,
                    expected,
                    build_image=build,
                    filesystem_check=lambda path: path.read_bytes() == b"data",
                ),
                output,
            )
            self.assertEqual(len(builds), 1)
            parsed = json.loads(sidecar.read_text(encoding="utf-8"))
            self.assertEqual(parsed["output_sha256"], hashlib.sha256(b"data").hexdigest())
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o644)
            self.assertEqual(stat.S_IMODE(sidecar.stat().st_mode), 0o644)

            policy.ensure_recipe_cache(
                output,
                sidecar,
                expected,
                build_image=lambda path: self.fail("valid pair must be reused"),
                filesystem_check=lambda path: True,
            )

            mutations = (
                ("missing-sidecar", lambda: sidecar.unlink()),
                ("malformed-sidecar", lambda: sidecar.write_text("{", encoding="utf-8")),
                (
                    "source-drift",
                    lambda: write_sidecar_field(sidecar, "source_sha256", "3" * 64),
                ),
                (
                    "requested-size-drift",
                    lambda: write_sidecar_field(sidecar, "requested_size_bytes", 8),
                ),
                (
                    "historical-v109-sidecar",
                    lambda: write_sidecar_field(
                        sidecar, "variant", "direct-boot-v109"
                    ),
                ),
                (
                    "recipe-drift",
                    lambda: write_sidecar_field(sidecar, "recipe_sha256", "4" * 64),
                ),
                (
                    "tool-drift",
                    lambda: write_sidecar_field(
                        sidecar,
                        "tool_versions",
                        {**expected["tool_versions"], "e2fsck": "e2fsck 2"},
                    ),
                ),
                ("wrong-image-size", lambda: output.write_bytes(b"x")),
                ("wrong-image-hash", lambda: output.write_bytes(b"xxxx")),
            )
            for name, mutate in mutations:
                with self.subTest(name=name):
                    mutate()
                    diagnostics = io.StringIO()
                    before_builds = len(builds)
                    policy.ensure_recipe_cache(
                        output,
                        sidecar,
                        expected,
                        build_image=build,
                        filesystem_check=lambda path: path.read_bytes() == b"data",
                        stderr=diagnostics,
                    )
                    self.assertEqual(len(builds), before_builds + 1)
                    self.assertIn("repairing", diagnostics.getvalue())

    def test_recipe_cache_rejects_nonregular_pair_objects(self) -> None:
        expected = recipe_expected()
        for which in ("image", "sidecar"):
            with self.subTest(which=which):
                with tempfile.TemporaryDirectory() as raw_temp:
                    temp = Path(raw_temp)
                    output = temp / "rootfs.ext4"
                    sidecar = temp / "rootfs.ext4.bangbang.json"
                    target = output if which == "image" else sidecar
                    target.symlink_to("missing")
                    with self.assertRaisesRegex(policy.ArtifactPolicyError, "symlink"):
                        policy.ensure_recipe_cache(
                            output,
                            sidecar,
                            expected,
                            build_image=lambda path: path.write_bytes(b"data"),
                            filesystem_check=lambda path: True,
                        )

    def test_recipe_cache_repairs_failed_filesystem_check(self) -> None:
        expected = recipe_expected()
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            output = temp / "rootfs.ext4"
            sidecar = temp / "rootfs.ext4.bangbang.json"
            build = lambda path: path.write_bytes(b"data")
            policy.ensure_recipe_cache(
                output,
                sidecar,
                expected,
                build_image=build,
                filesystem_check=lambda path: True,
            )
            checks = iter((False, True))
            diagnostics = io.StringIO()
            policy.ensure_recipe_cache(
                output,
                sidecar,
                expected,
                build_image=build,
                filesystem_check=lambda path: next(checks),
                stderr=diagnostics,
            )
            self.assertIn("filesystem check failed", diagnostics.getvalue())

    def test_recipe_cache_build_failure_and_lock_contention_clean_stages(self) -> None:
        expected = recipe_expected()
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            output = temp / "rootfs.ext4"
            sidecar = temp / "rootfs.ext4.bangbang.json"

            def fail_build(path: Path) -> None:
                path.write_bytes(b"part")
                raise policy.ArtifactPolicyError("build", "injected failure")

            with self.assertRaisesRegex(policy.ArtifactPolicyError, "injected failure"):
                policy.ensure_recipe_cache(
                    output,
                    sidecar,
                    expected,
                    build_image=fail_build,
                    filesystem_check=lambda path: True,
                )
            self.assertFalse(output.exists())
            self.assertFalse(sidecar.exists())
            self.assertEqual(list(temp.glob("*.build")), [])

            with policy.CacheLock(output):
                with self.assertRaisesRegex(policy.ArtifactPolicyError, "cache is busy"):
                    policy.ensure_recipe_cache(
                        output,
                        sidecar,
                        expected,
                        build_image=lambda path: path.write_bytes(b"data"),
                        filesystem_check=lambda path: True,
                    )

    def test_image_before_sidecar_interruption_is_detected_and_repaired(self) -> None:
        expected = recipe_expected()
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            output = temp / "rootfs.ext4"
            sidecar = temp / "rootfs.ext4.bangbang.json"
            original_replace = policy.os.replace

            def interrupt(source: object, destination: object) -> None:
                if Path(destination) == sidecar:
                    raise OSError("injected sidecar commit interruption")
                original_replace(source, destination)

            with mock.patch.object(policy.os, "replace", side_effect=interrupt):
                with self.assertRaises(OSError):
                    policy.ensure_recipe_cache(
                        output,
                        sidecar,
                        expected,
                        build_image=lambda path: path.write_bytes(b"data"),
                        filesystem_check=lambda path: True,
                    )
            self.assertTrue(output.is_file())
            self.assertFalse(sidecar.exists())

            diagnostics = io.StringIO()
            policy.ensure_recipe_cache(
                output,
                sidecar,
                expected,
                build_image=lambda path: path.write_bytes(b"data"),
                filesystem_check=lambda path: True,
                stderr=diagnostics,
            )
            self.assertIn("incomplete image/sidecar pair", diagnostics.getvalue())

    def test_ext4_size_parser_preserves_accepted_token_and_is_bounded(self) -> None:
        self.assertEqual(policy.parse_ext4_size("512m"), ("512m", 512 * 1024**2))
        self.assertEqual(policy.parse_ext4_size("01G"), ("01G", 1024**3))
        for invalid in ("0", "-1G", "1P", "1.5G", "999999999T"):
            with self.subTest(invalid=invalid):
                with self.assertRaises(policy.ArtifactPolicyError):
                    policy.parse_ext4_size(invalid)

    def test_modified_bash_wrappers_keep_bash_32_syntax_and_help_contracts(self) -> None:
        scripts = (
            "fetch-firecracker-kernel.sh",
            "fetch-firecracker-rootfs.sh",
            "sign-hvf-binary.sh",
            "build-signed-bangbang.sh",
        )
        for script in scripts:
            with self.subTest(script=script):
                path = REPOSITORY_ROOT / "scripts" / script
                syntax = subprocess.run(
                    ("/bin/bash", "-n", os.fspath(path)), capture_output=True, text=True
                )
                self.assertEqual(syntax.returncode, 0, syntax.stderr)
                help_result = subprocess.run(
                    (os.fspath(path), "--help"), capture_output=True, text=True
                )
                self.assertEqual(help_result.returncode, 0, help_result.stderr)
                self.assertIn("Usage:", help_result.stdout)

    def test_signing_wrapper_publishes_unique_output_and_never_clobbers(self) -> None:
        script = REPOSITORY_ROOT / "scripts/sign-hvf-binary.sh"
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            fake_bin = temp / "bin"
            fake_bin.mkdir()
            fake_codesign = fake_bin / "codesign"
            fake_codesign.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            fake_codesign.chmod(0o755)
            source = temp / "input"
            source.write_bytes(b"binary")
            source.chmod(0o755)
            environment = dict(os.environ)
            environment["PATH"] = f"{fake_bin}:{environment['PATH']}"

            unique = temp / "unique-output"
            result = subprocess.run(
                (os.fspath(script), os.fspath(source), os.fspath(unique)),
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(unique.read_bytes(), b"binary")
            self.assertEqual(stat.S_IMODE(unique.stat().st_mode), 0o755)

            occupied = temp / "occupied"
            occupied.write_bytes(b"old")
            before = os.lstat(occupied)
            result = subprocess.run(
                (os.fspath(script), os.fspath(source), os.fspath(occupied)),
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("collision", result.stderr)
            after = os.lstat(occupied)
            self.assertEqual((before.st_dev, before.st_ino), (after.st_dev, after.st_ino))
            self.assertEqual(occupied.read_bytes(), b"old")
            self.assertEqual(list(temp.glob(".occupied.signed.*")), [])

            broken = temp / "broken"
            broken.symlink_to("missing")
            result = subprocess.run(
                (os.fspath(script), os.fspath(source), os.fspath(broken)),
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(broken.is_symlink())


def errno_value(name: str) -> int:
    import errno

    return int(getattr(errno, name))


def write_sidecar_field(path: Path, key: str, value: object) -> None:
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload[key] = value
    path.write_bytes(policy._canonical_json(payload))


if __name__ == "__main__":
    unittest.main()
