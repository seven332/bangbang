import copy
import importlib.util
import json
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("run_kani", ROOT / "scripts/run-kani.py")
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load run-kani.py")
RUN_KANI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUN_KANI)


def authority():
    return RUN_KANI.load_authority()


def list_document(records, package):
    package_prefix = {
        "bangbang-pager": "crates/pager/",
        "bangbang-runtime": "crates/runtime/",
    }[package]
    standard = {}
    for record in records:
        if record["package"] != package:
            continue
        source = record["source"].removeprefix(package_prefix)
        standard.setdefault(source, []).append(record["harness"])
    count = sum(len(harnesses) for harnesses in standard.values())
    return {
        "kani-version": "0.67.0",
        "file-version": "0.1",
        "standard-harnesses": standard,
        "contract-harnesses": {},
        "contracts": [],
        "totals": {
            "standard-harnesses": count,
            "contract-harnesses": 0,
            "functions-under-contract": 0,
        },
    }


class FakeInvoker:
    def __init__(self, checked, *, wrong_version=False, fail_harness=None):
        self.checked = checked
        self.wrong_version = wrong_version
        self.fail_harness = fail_harness
        self.calls = []
        self.list_directories = []

    def __call__(self, command, cwd, capture_output):
        command = list(command)
        cwd = Path(cwd)
        self.calls.append((command, cwd, capture_output))
        if command == ["cargo", "kani", "--version"]:
            output = (
                "Kani Rust Verifier 0.66.0 (cargo plugin)"
                if self.wrong_version
                else RUN_KANI.EXPECTED_VERSION_OUTPUT
            )
            return subprocess.CompletedProcess(command, 0, stdout=output, stderr="")
        if command[:3] == ["cargo", "kani", "list"]:
            package = command[command.index("--package") + 1]
            self.list_directories.append(cwd)
            document = list_document(self.checked, package)
            (cwd / "kani-list.json").write_text(
                json.dumps(document), encoding="utf-8"
            )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        if self.fail_harness is not None and self.fail_harness in command:
            raise subprocess.CalledProcessError(1, command)
        return subprocess.CompletedProcess(command, 0, stdout="{}", stderr="")


class RunKaniTests(unittest.TestCase):
    def test_success_lists_each_package_then_runs_manifest_order(self):
        checked = RUN_KANI.validate_authority_for_execution(authority())
        invoke = FakeInvoker(checked)

        RUN_KANI.run_verification(authority(), ROOT, invoke)

        list_packages = [
            command[command.index("--package") + 1]
            for command, _, _ in invoke.calls
            if command[:3] == ["cargo", "kani", "list"]
        ]
        self.assertEqual(list_packages, RUN_KANI.EXPECTED_PACKAGES)
        proof_commands = [
            command
            for command, _, _ in invoke.calls
            if command[:2] == ["cargo", "kani"] and "--harness" in command
        ]
        self.assertEqual(
            proof_commands,
            [RUN_KANI.canonical_command(record) for record in checked],
        )
        self.assertTrue(invoke.list_directories)
        self.assertTrue(all(not path.exists() for path in invoke.list_directories))

    def test_wrong_version_fails_before_list_or_proofs(self):
        checked = RUN_KANI.validate_authority_for_execution(authority())
        invoke = FakeInvoker(checked, wrong_version=True)

        with self.assertRaisesRegex(RUN_KANI.RunnerError, "checked release"):
            RUN_KANI.run_verification(authority(), ROOT, invoke)

        self.assertFalse(
            any(call[0][:3] == ["cargo", "kani", "list"] for call in invoke.calls)
        )

    def test_noncanonical_command_and_duplicate_identity_fail_closed(self):
        modified = copy.deepcopy(authority())
        modified["harnesses"][0]["command"].append("--quiet")
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "not canonical"):
            RUN_KANI.validate_authority_for_execution(modified)

        modified = copy.deepcopy(authority())
        modified["harnesses"].append(copy.deepcopy(modified["harnesses"][0]))
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "duplicated"):
            RUN_KANI.validate_authority_for_execution(modified)

    def test_compiled_list_rejects_missing_extra_duplicate_and_contract_harnesses(self):
        checked = RUN_KANI.validate_authority_for_execution(authority())
        document = list_document(checked, "bangbang-pager")
        identities = RUN_KANI.compiled_identities(document, "bangbang-pager", checked)
        self.assertEqual(
            identities,
            RUN_KANI.expected_identities(checked, "bangbang-pager"),
        )

        missing = copy.deepcopy(document)
        missing["standard-harnesses"]["src/frame.rs"] = []
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "totals|enumerate"):
            RUN_KANI.compiled_identities(missing, "bangbang-pager", checked)

        extra = copy.deepcopy(document)
        extra["standard-harnesses"]["src/frame.rs"].append("extra::proof")
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "totals|enumerate"):
            RUN_KANI.compiled_identities(extra, "bangbang-pager", checked)

        duplicate = copy.deepcopy(document)
        duplicate["standard-harnesses"]["src/frame.rs"].append(
            duplicate["standard-harnesses"]["src/frame.rs"][0]
        )
        duplicate["totals"]["standard-harnesses"] += 1
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "totals|duplicate"):
            RUN_KANI.compiled_identities(duplicate, "bangbang-pager", checked)

        contract = copy.deepcopy(document)
        contract["contract-harnesses"] = {"src/frame.rs": ["contract"]}
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "contract harnesses"):
            RUN_KANI.compiled_identities(contract, "bangbang-pager", checked)

    def test_reported_source_must_map_uniquely_within_package(self):
        checked = RUN_KANI.validate_authority_for_execution(authority())
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "uniquely"):
            RUN_KANI.normalize_reported_source(
                "bangbang-pager", "src/missing.rs", checked
            )

        ambiguous = list(checked) + [
            {
                "package": "bangbang-pager",
                "source": "other/src/frame.rs",
                "harness": "other::proof",
            }
        ]
        with self.assertRaisesRegex(RUN_KANI.RunnerError, "uniquely"):
            RUN_KANI.normalize_reported_source(
                "bangbang-pager", "src/frame.rs", ambiguous
            )

    def test_proof_failure_is_not_hidden(self):
        checked = RUN_KANI.validate_authority_for_execution(authority())
        failing = checked[2]["harness"]
        invoke = FakeInvoker(checked, fail_harness=failing)

        with self.assertRaises(subprocess.CalledProcessError):
            RUN_KANI.run_verification(authority(), ROOT, invoke)

        proof_commands = [
            command
            for command, _, _ in invoke.calls
            if command[:2] == ["cargo", "kani"] and "--harness" in command
        ]
        self.assertEqual(len(proof_commands), 3)


if __name__ == "__main__":
    unittest.main()
