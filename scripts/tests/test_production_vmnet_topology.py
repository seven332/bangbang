from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER_PATH = ROOT / "scripts" / "run-production-vmnet-topology.py"
PREPARE_PATH = ROOT / "scripts" / "prepare-production-vmnet-topology.sh"
SPEC = importlib.util.spec_from_file_location("production_vmnet_topology", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
topology = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = topology
SPEC.loader.exec_module(topology)


class ProductionVmnetTopologyTests(unittest.TestCase):
    def test_fixed_product_layout_and_commands_derive_only_from_bundle(self) -> None:
        bundle = Path("/private/var/tmp/root-stage/Bangbang.app")
        layout = topology.ProductLayout.from_bundle(bundle)
        self.assertEqual(
            layout.provider,
            bundle / "Contents/Helpers/bangbang-vmnet-provider",
        )
        self.assertEqual(
            layout.worker,
            bundle
            / "Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker",
        )

        command = topology._provider_command(
            layout,
            501,
            20,
            "case",
            ["--api-sock", "/private/var/tmp/root-stage/api/api.sock"],
            daemon=True,
        )
        self.assertEqual(command[0], str(layout.provider))
        self.assertEqual(command.count("--bootstrap-v1"), 1)
        self.assertEqual(command.count("--daemonize"), 2)
        self.assertIn(str(layout.worker), command)
        self.assertEqual(command[-2:], ["--api-sock", "/private/var/tmp/root-stage/api/api.sock"])

    def test_closed_ids_and_daemon_record_reject_noncanonical_values(self) -> None:
        for value in ("501", "4294967295"):
            self.assertGreater(topology._parse_id(value), 0)
        for value in ("", "0", "0501", "-1", "4294967296", " 501"):
            with self.assertRaises(topology.TopologyError):
                topology._parse_id(value)

        self.assertEqual(topology._parse_daemon_pid(b"bangbang daemon pid: 42\n"), 42)
        for output in (
            b"bangbang daemon pid: 0\n",
            b"prefix bangbang daemon pid: 42\n",
            b"bangbang daemon pid: 42\nextra\n",
        ):
            with self.assertRaises(topology.TopologyError):
                topology._parse_daemon_pid(output)

    def test_process_table_accepts_only_fixed_observation_fields(self) -> None:
        records = topology._parse_process_table(
            " 100 1 Ss /root/Bangbang.app/Contents/Helpers/bangbang-vmnet-provider\n"
            " 101 100 Ss /root/Bangbang.app/Contents/MacOS/bangbang\n"
            " 102 101 S /root/Bangbang.app/Contents/Helpers/BangbangWorker.app/Contents/MacOS/bangbang-worker\n"
            "malformed record\n"
        )
        self.assertEqual(records[100].name, "bangbang-vmnet-provider")
        self.assertEqual(records[101].ppid, 100)
        self.assertEqual(records[102].state, "S")
        self.assertNotIn(0, records)

        layout = topology.ProductLayout.from_bundle(Path("/root/Bangbang.app"))
        self.assertEqual(topology._stage_process_ids(records, layout), {100, 101, 102})
        records[103] = topology.ProcessRecord(103, 1, "S", "/other/bangbang")
        records[104] = topology.ProcessRecord(104, 1, "Z", str(layout.provider))
        self.assertEqual(
            topology._stage_process_ids(records, layout), {100, 101, 102, 104}
        )

    def test_wrappers_never_request_credentials_or_apple_authorization(self) -> None:
        prepare = PREPARE_PATH.read_text(encoding="utf-8")
        runner = RUNNER_PATH.read_text(encoding="utf-8")
        self.assertNotIn("sudo", prepare.lower())
        self.assertNotIn("sudo", runner.lower())
        self.assertNotIn("password", prepare.lower())
        self.assertNotIn("password", runner.lower())
        self.assertIn("--signing-identity -", prepare)
        self.assertIn("--features grant-integration-probe", prepare)
        self.assertIn('["/bin/ps", "-axo", "pid=,ppid=,state=,comm="]', runner)


if __name__ == "__main__":
    unittest.main()
