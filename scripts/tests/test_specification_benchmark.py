from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, os.fspath(REPOSITORY_ROOT / "scripts"))


def load_benchmark_module():
    import importlib.util

    path = REPOSITORY_ROOT / "scripts/specification-benchmark.py"
    spec = importlib.util.spec_from_file_location("specification_benchmark", path)
    if spec is None or spec.loader is None:
        raise AssertionError("specification benchmark module should be importable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


benchmark = load_benchmark_module()


def config_document() -> dict[str, object]:
    return {
        "host_label": "test-lab",
        "iterations": 3,
        "schema_version": 1,
        "timeouts": {
            "artifact_seconds": 60,
            "build_seconds": 60,
            "guest_seconds": 10,
            "network_seconds": 10,
            "request_seconds": 2,
            "startup_seconds": 5,
            "terminate_seconds": 2,
        },
        "tracing": "disabled",
        "warmups": 1,
    }


def sample_config():
    return benchmark.parse_config_document(config_document())


def sample_environment() -> dict[str, object]:
    return {
        "backend": {
            "hypervisor": "Hypervisor.framework",
            "memory_mib": 256,
            "transport": "virtio-mmio",
            "vcpu_count": 1,
        },
        "binary": {
            "sha256": "1" * 64,
            "signing": "ad-hoc-hvf-entitlement-v1",
            "size_bytes": 123,
        },
        "build": {
            "cargo_lock_sha256": "2" * 64,
            "cargo_version": "cargo 1.90.0",
            "commit": "3" * 40,
            "features": [],
            "profile": "release",
            "rustc_version": "rustc 1.90.0",
            "source_state": "clean",
            "target": "aarch64-apple-darwin",
            "tree": "4" * 40,
        },
        "cpu": {
            "architecture": "arm64",
            "brand": "Apple M4",
            "hardware_model": "Mac16,1",
            "logical_count": 10,
        },
        "guest": {
            "boot_args": benchmark.WORKLOAD_BOOT_ARGS,
            "compute_checksum": benchmark.specification_workload.COMPUTE_CHECKSUM,
            "compute_operations": benchmark.specification_workload.COMPUTE_OPERATIONS,
            "kernel_sha256": "5" * 64,
            "kernel_size_bytes": 100,
            "rootfs_recipe_sha256": "6" * 64,
            "rootfs_sha256": "7" * 64,
            "rootfs_size_bytes": 512 * 1024 * 1024,
            "storage_block_bytes": benchmark.specification_workload.STORAGE_BLOCK_BYTES,
            "storage_bytes": benchmark.specification_workload.STORAGE_BYTES,
            "storage_checksum": 123456789,
            "workload_protocol": "bangbang-specification-workload-v1",
            "workload_source_sha256": "8" * 64,
        },
        "host_label": "test-lab",
        "operating_system": {
            "kernel_release": "25.0.0",
            "macos_build": "25A1",
            "macos_version": "26.0",
        },
        "tracing": "disabled",
    }


def sample_observations(offset: int = 0) -> dict[str, list[int]]:
    observations = {
        name: [offset + 3, offset + 1, offset + 2]
        for name, _method, _unit in benchmark.MEASUREMENT_DEFINITIONS
    }
    observations["metrics_missed_count"] = [1, 1, 1]
    return observations


def write_canonical(path: Path, value: object) -> None:
    path.write_bytes(benchmark.canonical_json(value))


def make_fixture(temp: Path, *, cleanup: str = "complete"):
    executable = temp / "fixture.py"
    output = {
        "backend": "vmnet-shared",
        "cleanup": cleanup,
        "method": "fixed-transfer-v1",
        "schema_version": 1,
        "unit": "bytes-per-second",
        "value": 12345,
        "workload": "operator-network-v1",
    }
    executable.write_text(
        f"#!{sys.executable}\n"
        "import json\n"
        f"print(json.dumps({output!r}, sort_keys=True, indent=2))\n",
        encoding="utf-8",
    )
    executable.chmod(0o700)
    fixture_document = {
        "argv": [os.fspath(executable)],
        "backend": "vmnet-shared",
        "credential_mode": "none",
        "method": "fixed-transfer-v1",
        "schema_version": 1,
        "timeout_seconds": 5,
        "unit": "bytes-per-second",
        "workload": "operator-network-v1",
    }
    fixture_path = temp / "fixture.json"
    write_canonical(fixture_path, fixture_document)
    return benchmark.read_network_fixture(fixture_path), fixture_document


class StrictDocumentTests(unittest.TestCase):
    def test_example_config_is_canonical_and_valid(self) -> None:
        path = REPOSITORY_ROOT / "scripts/specification-benchmark-config.example.json"
        config = benchmark.read_config(path)
        self.assertEqual(config.iterations, 3)
        self.assertEqual(config.warmups, 1)
        self.assertEqual(config.tracing, "disabled")

    def test_config_rejects_duplicate_unknown_missing_noncanonical_and_bounds(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            valid = benchmark.canonical_json(config_document())
            cases: dict[str, bytes] = {
                "duplicate": valid.replace(
                    b'  "iterations": 3,',
                    b'  "iterations": 3,\n  "iterations": 3,',
                    1,
                ),
                "noncanonical": json.dumps(config_document()).encode("ascii"),
            }
            unknown = config_document()
            unknown["threshold"] = 1
            cases["unknown"] = benchmark.canonical_json(unknown)
            missing = config_document()
            del missing["tracing"]
            cases["missing"] = benchmark.canonical_json(missing)
            even = config_document()
            even["iterations"] = 4
            cases["even"] = benchmark.canonical_json(even)
            boolean = config_document()
            boolean["iterations"] = True
            cases["boolean"] = benchmark.canonical_json(boolean)
            oversized = config_document()
            oversized["warmups"] = 11
            cases["oversized"] = benchmark.canonical_json(oversized)
            unsafe = config_document()
            unsafe["host_label"] = "../private"
            cases["unsafe"] = benchmark.canonical_json(unsafe)

            for name, data in cases.items():
                with self.subTest(name=name):
                    path = temp / f"{name}.json"
                    path.write_bytes(data)
                    with self.assertRaises(benchmark.BenchmarkError):
                        benchmark.read_config(path)

    def test_cli_is_closed_and_has_no_unsupported_bypass(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            benchmark.parse_args(["collect", "--allow-unsupported"])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            benchmark.parse_args(["validate", "--report", "r", "--extra"])
        args = benchmark.parse_args(
            ["compare", "--previous", "before", "--current", "after"]
        )
        self.assertEqual(args.command, "compare")

    def test_fixture_is_strict_bounded_and_identity_pinned(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            fixture, document = make_fixture(temp)
            self.assertEqual(fixture.credential_mode, "none")
            self.assertEqual(len(fixture.document_sha256), 64)
            self.assertEqual(len(fixture.executable_sha256), 64)

            relative = copy.deepcopy(document)
            relative["argv"] = ["fixture.py"]
            with self.assertRaisesRegex(benchmark.BenchmarkError, "absolute"):
                benchmark.parse_network_fixture_document(
                    relative, benchmark.canonical_json(relative)
                )

            credentialed = copy.deepcopy(document)
            credentialed["credential_mode"] = "environment"
            with self.assertRaisesRegex(benchmark.BenchmarkError, "credential_mode"):
                benchmark.parse_network_fixture_document(
                    credentialed, benchmark.canonical_json(credentialed)
                )

            unknown = copy.deepcopy(document)
            unknown["environment"] = {"TOKEN": "secret"}
            with self.assertRaisesRegex(benchmark.BenchmarkError, "unknown keys"):
                benchmark.parse_network_fixture_document(
                    unknown, benchmark.canonical_json(unknown)
                )


class ReportContractTests(unittest.TestCase):
    def test_report_retains_raw_integers_and_recomputes_summaries_and_key(self) -> None:
        report = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations()
        )
        validated = benchmark.validate_report_document(report)
        self.assertEqual(validated["measurements"][0]["raw"], [3, 1, 2])
        self.assertEqual(
            validated["measurements"][0]["summary"],
            {"count": 3, "max": 3, "median": 2, "min": 1},
        )
        self.assertEqual(report["comparison_key"], benchmark.comparison_key(report))

        changed_raw = copy.deepcopy(report)
        changed_raw["measurements"][0]["raw"] = [9, 7, 8]
        changed_raw["measurements"][0]["summary"] = benchmark.summarize([9, 7, 8])
        benchmark.validate_report_document(changed_raw)
        self.assertEqual(changed_raw["comparison_key"], report["comparison_key"])

        stale_summary = copy.deepcopy(report)
        stale_summary["measurements"][0]["raw"][0] = 99
        with self.assertRaisesRegex(benchmark.BenchmarkError, "does not match raw"):
            benchmark.validate_report_document(stale_summary)

        stale_key = copy.deepcopy(report)
        stale_key["environment"]["binary"]["sha256"] = "9" * 64
        with self.assertRaisesRegex(benchmark.BenchmarkError, "comparison key"):
            benchmark.validate_report_document(stale_key)

    def test_every_identity_category_blocks_comparison(self) -> None:
        base = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations()
        )
        mutations = {
            "host": ("environment", "host_label", "other-lab"),
            "os": ("environment", "operating_system", "macos_build", "25A2"),
            "cpu": ("environment", "cpu", "brand", "Apple M5"),
            "build": ("environment", "build", "commit", "a" * 40),
            "binary": ("environment", "binary", "sha256", "b" * 64),
            "backend": ("environment", "backend", "memory_mib", 257),
            "guest": ("environment", "guest", "rootfs_sha256", "c" * 64),
            "tracing": ("environment", "tracing", "enabled"),
            "policy": ("policy", "warmups", 2),
            "unit": ("measurements", 0, "unit", "ticks"),
            "method": ("measurements", 0, "method", "other-v1"),
        }
        for name, mutation in mutations.items():
            with self.subTest(name=name):
                changed = copy.deepcopy(base)
                cursor: object = changed
                for key in mutation[:-2]:
                    cursor = cursor[key]  # type: ignore[index]
                cursor[mutation[-2]] = mutation[-1]  # type: ignore[index]
                changed["comparison_key"] = benchmark.comparison_key(changed)
                if name in ("backend", "tracing", "unit", "method"):
                    with self.assertRaises(benchmark.BenchmarkError):
                        benchmark.comparison_document(base, changed)
                else:
                    with self.assertRaisesRegex(
                        benchmark.BenchmarkError, "different comparison identities"
                    ):
                        benchmark.comparison_document(base, changed)

    def test_compare_is_descriptive_and_has_no_threshold_verdict(self) -> None:
        previous = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations()
        )
        current = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations(10)
        )
        comparison = benchmark.comparison_document(previous, current)
        output = benchmark.canonical_json(comparison)
        self.assertIn(b'"previous"', output)
        self.assertIn(b'"current"', output)
        for forbidden in (
            b'"verdict"',
            b'"passed"',
            b'"failed"',
            b'"threshold"',
            b'"regression"',
            b'"parity"',
            b'"delta"',
        ):
            self.assertNotIn(forbidden, output.lower())

    def test_network_is_absent_by_default_and_contains_no_argv_or_environment(self) -> None:
        report = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations()
        )
        self.assertNotIn("network", report)
        with tempfile.TemporaryDirectory() as raw_temp:
            fixture, _document = make_fixture(Path(raw_temp))
            network_report = benchmark.assemble_report(
                sample_config(),
                sample_environment(),
                sample_observations(),
                fixture=fixture,
                network_raw=[30, 10, 20],
            )
            encoded = benchmark.canonical_json(network_report)
            self.assertIn(b'"network"', encoded)
            self.assertNotIn(b'"argv"', encoded)
            self.assertNotIn(b'"environment"', encoded.split(b'"network"', 1)[1])
            self.assertNotIn(os.fsencode(fixture.argv[0]), encoded)

    def test_report_file_must_be_canonical_closed_and_duplicate_free(self) -> None:
        report = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations()
        )
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            canonical = temp / "report.json"
            write_canonical(canonical, report)
            self.assertEqual(benchmark.read_report(canonical), report)

            unknown = copy.deepcopy(report)
            unknown["verdict"] = "pass"
            path = temp / "unknown.json"
            write_canonical(path, unknown)
            with self.assertRaisesRegex(benchmark.BenchmarkError, "unknown keys"):
                benchmark.read_report(path)

            duplicate = temp / "duplicate.json"
            duplicate.write_bytes(
                benchmark.canonical_json(report).replace(
                    b'  "schema_version": 1',
                    b'  "schema_version": 1,\n  "schema_version": 1',
                    1,
                )
            )
            with self.assertRaisesRegex(benchmark.BenchmarkError, "duplicate JSON key"):
                benchmark.read_report(duplicate)

    def test_report_requires_positive_fifo_fill_complete_drain_and_exact_replay(self) -> None:
        report = benchmark.assemble_report(
            sample_config(), sample_environment(), sample_observations()
        )
        by_name = {item["name"]: item for item in report["measurements"]}

        zero_fill = copy.deepcopy(report)
        fill = next(
            item
            for item in zero_fill["measurements"]
            if item["name"] == "metrics_fifo_filled_bytes"
        )
        fill["raw"] = [0, 1, 2]
        fill["summary"] = benchmark.summarize(fill["raw"])
        with self.assertRaisesRegex(benchmark.BenchmarkError, "must be positive"):
            benchmark.validate_report_document(zero_fill)

        short_drain = copy.deepcopy(report)
        drained = next(
            item
            for item in short_drain["measurements"]
            if item["name"] == "metrics_fifo_drained_bytes"
        )
        drained["raw"] = [2, 1, 2]
        drained["summary"] = benchmark.summarize(drained["raw"])
        with self.assertRaisesRegex(benchmark.BenchmarkError, "every filler byte"):
            benchmark.validate_report_document(short_drain)

        bad_replay = copy.deepcopy(report)
        missed = next(
            item
            for item in bad_replay["measurements"]
            if item["name"] == "metrics_missed_count"
        )
        missed["raw"] = [1, 2, 1]
        missed["summary"] = benchmark.summarize(missed["raw"])
        with self.assertRaisesRegex(benchmark.BenchmarkError, "exactly one"):
            benchmark.validate_report_document(bad_replay)

        self.assertEqual(by_name["metrics_missed_count"]["raw"], [1, 1, 1])


class RuntimeBoundaryTests(unittest.TestCase):
    def test_network_fixture_runs_without_inherited_environment_and_requires_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            fixture, _document = make_fixture(temp)
            with mock.patch.dict(os.environ, {"SECRET_TOKEN": "must-not-be-used"}):
                self.assertEqual(benchmark.collect_network_sample(fixture, temp), 12345)

            failed_dir = temp / "failed"
            failed_dir.mkdir()
            failed, _document = make_fixture(failed_dir, cleanup="pending")
            with self.assertRaisesRegex(benchmark.BenchmarkError, "cleanup"):
                benchmark.collect_network_sample(failed, failed_dir)

    def test_network_fixture_uses_a_cleaned_per_sample_session_and_policy_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            fixture, document = make_fixture(temp)
            original_entries = set(temp.iterdir())
            self.assertEqual(benchmark.collect_network_sample(fixture, temp), 12345)
            self.assertEqual(set(temp.iterdir()), original_entries)

            too_slow = copy.deepcopy(document)
            too_slow["timeout_seconds"] = sample_config().timeouts.network_seconds + 1
            parsed = benchmark.parse_network_fixture_document(
                too_slow, benchmark.canonical_json(too_slow)
            )
            with self.assertRaisesRegex(benchmark.BenchmarkError, "collection policy"):
                benchmark.collect_report(
                    sample_config(), temp / "report.json", fixture=parsed
                )

    def test_real_portable_fifo_fill_and_drain_reaches_eagain(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            fifo = Path(raw_temp) / "metrics.fifo"
            reader = benchmark._create_fifo(fifo)
            try:
                filled = benchmark._fill_fifo(fifo)
                drained = benchmark._drain_fifo(reader)
            finally:
                os.close(reader)
            self.assertGreater(filled, 0)
            self.assertEqual(len(drained), filled)
            self.assertTrue(drained.startswith(benchmark.FIFO_SENTINEL_CHUNK[:16]))

    def test_typed_fifo_failure_and_exact_replay_counter_are_closed(self) -> None:
        body = benchmark.canonical_json(
            {"fault_message": benchmark.EXPECTED_WOULD_BLOCK_FAULT}
        ).rstrip(b"\n")
        benchmark._require_would_block(benchmark.HttpResponse(400, body))
        with self.assertRaises(benchmark.BenchmarkError):
            benchmark._require_would_block(benchmark.HttpResponse(500, body))
        with self.assertRaises(benchmark.BenchmarkError):
            benchmark._require_would_block(
                benchmark.HttpResponse(400, b'{"fault_message":"other"}')
            )
        self.assertEqual(
            benchmark._missed_metrics(b'{"logger":{"missed_metrics_count":1}}'), 1
        )
        with self.assertRaisesRegex(benchmark.BenchmarkError, "exactly one"):
            benchmark._missed_metrics(b'{"logger":{"missed_metrics_count":2}}')

    def test_telemetry_sample_has_one_typed_failure_then_one_successful_retry(self) -> None:
        events: list[str] = []
        response_body = benchmark.canonical_json(
            {"fault_message": benchmark.EXPECTED_WOULD_BLOCK_FAULT}
        ).rstrip(b"\n")

        class FakeProcess:
            pid = 123

        class FakeVmm:
            def __init__(self, _arguments, _session_path):
                self.process = FakeProcess()
                events.append("start")

            def wait_marker(self, marker, _timeout):
                events.append(
                    "wait-api" if marker == benchmark.API_SOCKET_READY else "wait-guest"
                )

            def assert_marker_absent(self, _marker):
                events.append("assert-timed-absent")

            def write_stdin(self, value):
                if value != benchmark.specification_workload.RELEASE_BYTE:
                    raise AssertionError("telemetry guest received an unexpected release byte")
                events.append("release")

            def wait_exit(self, _timeout):
                events.append("wait-exit")
                return b"", b""

            def finish(self, _timeout):
                events.append("finish")

        fifo_lines = iter(
            [
                b'{"api_server":{}}',
                b'{"logger":{"missed_metrics_count":1}}',
            ]
        )

        def read_line(_descriptor, _timeout):
            events.append("read-fifo")
            return next(fifo_lines)

        def exchange(_socket, _method, _path, _body, _timeout):
            events.append("flush")
            if events.count("flush") == 1:
                return benchmark.HttpResponse(400, response_body)
            return benchmark.HttpResponse(204, b"")

        artifacts = benchmark.PreparedArtifacts(
            Path("/kernel"),
            "5" * 64,
            100,
            Path("/rootfs"),
            "7" * 64,
            512 * 1024 * 1024,
            "6" * 64,
            123456789,
        )
        build = benchmark.SignedBuild(
            Path("/binary"),
            "1" * 64,
            123,
            "3" * 40,
            "4" * 40,
            "cargo 1.90.0",
            "rustc 1.90.0",
        )
        with tempfile.TemporaryDirectory() as raw_temp, mock.patch.object(
            benchmark, "VmmProcess", FakeVmm
        ), mock.patch.object(
            benchmark,
            "_wait_api_socket",
            side_effect=lambda *_args: events.append("wait-socket"),
        ), mock.patch.object(
            benchmark,
            "_configure_guest",
            side_effect=lambda *_args: events.append("configure"),
        ), mock.patch.object(
            benchmark, "_read_fifo_line", side_effect=read_line
        ), mock.patch.object(
            benchmark,
            "_fill_fifo",
            side_effect=lambda *_args: events.append("fill")
            or len(benchmark.FIFO_SENTINEL_CHUNK),
        ), mock.patch.object(
            benchmark,
            "_drain_fifo",
            side_effect=lambda *_args: events.append("drain")
            or benchmark.FIFO_SENTINEL_CHUNK,
        ), mock.patch.object(
            benchmark, "http_json", side_effect=exchange
        ), mock.patch.object(
            benchmark,
            "_wait_socket_absent",
            side_effect=lambda *_args: events.append("socket-absent"),
        ):
            result = benchmark.collect_telemetry_sample(
                Path(raw_temp), artifacts, build, sample_config(), 0
            )

        self.assertEqual(
            result,
            {
                "metrics_fifo_drained_bytes": len(benchmark.FIFO_SENTINEL_CHUNK),
                "metrics_fifo_filled_bytes": len(benchmark.FIFO_SENTINEL_CHUNK),
                "metrics_missed_count": 1,
            },
        )
        self.assertEqual(events.count("flush"), 2)
        self.assertLess(events.index("fill"), events.index("drain"))
        self.assertLess(events.index("drain"), events.index("socket-absent"))
        self.assertEqual(events[-1], "finish")

    def test_owned_command_enforces_timeout_and_output_bounds(self) -> None:
        with self.assertRaisesRegex(benchmark.BenchmarkError, "deadline"):
            benchmark.run_command(
                (sys.executable, "-c", "import time; time.sleep(2)"),
                timeout_seconds=0.05,
                phase="timeout probe",
            )
        with self.assertRaisesRegex(benchmark.BenchmarkError, "output exceeded"):
            benchmark.run_command(
                (
                    sys.executable,
                    "-c",
                    f"import sys; sys.stdout.buffer.write(b'x' * {benchmark.MAX_CAPTURE_BYTES + 1})",
                ),
                timeout_seconds=5,
                phase="output probe",
            )

    def test_publication_is_absent_only_and_preserves_collision(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            destination = temp / "report.json"
            benchmark.publish_absent(destination, b"evidence\n")
            self.assertEqual(destination.read_bytes(), b"evidence\n")
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o600)
            with self.assertRaisesRegex(benchmark.BenchmarkError, "already exists"):
                benchmark.publish_absent(destination, b"replacement\n")
            self.assertEqual(destination.read_bytes(), b"evidence\n")

    def test_collection_discards_warmup_cleans_root_then_publishes(self) -> None:
        config = sample_config()
        root_paths: list[Path] = []
        calls: list[tuple[str, int]] = []
        artifacts = benchmark.PreparedArtifacts(
            Path("/kernel"), "5" * 64, 100, Path("/rootfs"), "7" * 64,
            512 * 1024 * 1024, "6" * 64, 123456789,
        )

        def build(path, _config):
            root_paths.append(path)
            return benchmark.SignedBuild(
                path / "binary", "1" * 64, 123, "3" * 40, "4" * 40,
                "cargo 1.90.0", "rustc 1.90.0",
            )

        def workload(_parent, _artifacts, _build, _config, index):
            calls.append(("workload", index))
            value = index + 1
            return {
                "guest_compute_duration_ns": value,
                "guest_init_cpu_us": value,
                "guest_init_wall_us": value,
                "guest_storage_duration_ns": value,
                "process_startup_cpu_us": value,
                "process_startup_wall_us": value,
                "whole_process_rss_kib": value,
            }

        def telemetry(_parent, _artifacts, _build, _config, index):
            calls.append(("telemetry", index))
            value = index + 1
            return {
                "metrics_fifo_drained_bytes": value,
                "metrics_fifo_filled_bytes": value,
                "metrics_missed_count": 1,
            }

        dependencies = benchmark.CollectionDependencies(
            preflight=lambda _config: None,
            prepare_artifacts=lambda _config: artifacts,
            build_signed_binary=build,
            inspect_environment=lambda _config, _artifacts, _build: sample_environment(),
            collect_workload=workload,
            collect_telemetry=telemetry,
            collect_network=lambda _fixture, _path: 1,
        )
        with tempfile.TemporaryDirectory() as raw_temp:
            output = Path(raw_temp) / "report.json"
            report = benchmark.collect_report(
                config, output, dependencies=dependencies
            )
            self.assertTrue(output.is_file())
            self.assertEqual(report["measurements"][0]["raw"], [2, 3, 4])
            self.assertEqual(len(calls), 8)
            self.assertTrue(root_paths)
            self.assertTrue(all(not path.exists() for path in root_paths))

    def test_collection_failure_cleans_root_and_never_publishes(self) -> None:
        config = sample_config()
        root_paths: list[Path] = []
        artifacts = benchmark.PreparedArtifacts(
            Path("/kernel"), "5" * 64, 100, Path("/rootfs"), "7" * 64,
            512 * 1024 * 1024, "6" * 64, 123456789,
        )

        def build(path, _config):
            root_paths.append(path)
            return benchmark.SignedBuild(
                path / "binary", "1" * 64, 123, "3" * 40, "4" * 40,
                "cargo 1.90.0", "rustc 1.90.0",
            )

        def workload(_parent, _artifacts, _build, _config, _index):
            return {
                "guest_compute_duration_ns": 1,
                "guest_init_cpu_us": 1,
                "guest_init_wall_us": 1,
                "guest_storage_duration_ns": 1,
                "process_startup_cpu_us": 1,
                "process_startup_wall_us": 1,
                "whole_process_rss_kib": 1,
            }

        dependencies = benchmark.CollectionDependencies(
            preflight=lambda _config: None,
            prepare_artifacts=lambda _config: artifacts,
            build_signed_binary=build,
            inspect_environment=lambda _config, _artifacts, _build: sample_environment(),
            collect_workload=workload,
            collect_telemetry=lambda *_args: (_ for _ in ()).throw(
                benchmark.BenchmarkError("telemetry", "injected failure")
            ),
            collect_network=lambda _fixture, _path: 1,
        )
        with tempfile.TemporaryDirectory() as raw_temp:
            output = Path(raw_temp) / "report.json"
            with self.assertRaisesRegex(benchmark.BenchmarkError, "injected failure"):
                benchmark.collect_report(config, output, dependencies=dependencies)
            self.assertFalse(output.exists())
            self.assertTrue(root_paths)
            self.assertTrue(all(not path.exists() for path in root_paths))


if __name__ == "__main__":
    unittest.main()
