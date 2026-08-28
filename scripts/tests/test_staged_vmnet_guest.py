from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "scripts/guest/staged_vmnet_certification.py"
SPEC = importlib.util.spec_from_file_location("staged_vmnet_certification", MODULE_PATH)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import contract
    raise RuntimeError("failed to load staged vmnet guest module")
staged = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = staged
SPEC.loader.exec_module(staged)


class FakeBarrier:
    def __init__(self) -> None:
        self.statuses: list[tuple[int, staged.Status]] = []
        self.commands: list[int] = []

    def status(self, sequence: int, kind: staged.Status) -> None:
        self.statuses.append((sequence, kind))

    def proceed(self, sequence: int) -> None:
        self.commands.append(sequence)


def changing_counts(values: list[int]):
    pending = list(values)

    def read() -> int:
        if len(pending) > 1:
            return pending.pop(0)
        return pending[0]

    return read


class StagedVmnetProtocolTests(unittest.TestCase):
    NONCE = bytes(range(1, 33))

    def test_headers_are_exact_and_scenario_bound(self) -> None:
        for scenario in staged.Scenario:
            encoded = staged.encode_header(scenario, self.NONCE)
            self.assertEqual(len(encoded), staged.SECTOR_BYTES)
            self.assertEqual(
                staged.decode_header(encoded),
                staged.Header(scenario, scenario.cycles, self.NONCE),
            )

        valid = staged.encode_header(staged.Scenario.STARTUP, self.NONCE)
        for index in (0, 8, 10, 11, 12, 16, 48, 64, 96, 511):
            with self.subTest(index=index):
                changed = bytearray(valid)
                changed[index] ^= 1
                with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                    staged.decode_header(bytes(changed))

        for invalid in (b"", bytes(staged.SECTOR_BYTES), valid[:-1]):
            with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                staged.decode_header(invalid)

        control = valid + bytes(staged.CONTROL_BYTES - staged.SECTOR_BYTES)
        self.assertEqual(
            staged.decode_initial_control(control),
            staged.Header(staged.Scenario.STARTUP, 2, self.NONCE),
        )
        for index in (staged.SECTOR_BYTES, staged.STATUS_OFFSET, staged.CONTROL_BYTES - 1):
            with self.subTest(index=index):
                changed = bytearray(control)
                changed[index] = 1
                with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                    staged.decode_initial_control(bytes(changed))
        for invalid in (control[:-1], control + b"\0"):
            with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                staged.decode_initial_control(invalid)

    def test_records_are_exact_role_sequence_and_nonce_bound(self) -> None:
        valid = staged.encode_record(
            staged.ROLE_COMMAND,
            staged.Scenario.RESTORE,
            staged.COMMAND_PROCEED,
            7,
            self.NONCE,
        )
        self.assertEqual(
            staged.decode_record(valid),
            staged.Record(
                staged.ROLE_COMMAND,
                staged.Scenario.RESTORE,
                staged.COMMAND_PROCEED,
                7,
                self.NONCE,
            ),
        )
        self.assertIsNone(staged.decode_record(bytes(staged.SECTOR_BYTES), allow_empty=True))
        for index in (0, 8, 10, 11, 12, 13, 16, 24, 56, 64, 96, 511):
            with self.subTest(index=index):
                changed = bytearray(valid)
                changed[index] ^= 1
                with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                    staged.decode_record(bytes(changed))

        for role, kind, sequence, nonce in (
            (0, 1, 1, self.NONCE),
            (3, 1, 1, self.NONCE),
            (staged.ROLE_COMMAND, 0, 1, self.NONCE),
            (staged.ROLE_COMMAND, 1, 0, self.NONCE),
            (staged.ROLE_COMMAND, 1, 1, bytes(32)),
        ):
            with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                staged.encode_record(role, staged.Scenario.STARTUP, kind, sequence, nonce)

        self.assertEqual(
            set(staged.FAILURE_CATEGORIES.values()),
            {"control", "io", "topology", "timeout", "process", "traffic", "internal"},
        )
        self.assertEqual(len(staged.FAILURE_KINDS), len(set(staged.FAILURE_KINDS.values())))

    def test_command_reader_rejects_skips_cross_scenario_and_timeout(self) -> None:
        header = staged.Header(staged.Scenario.STARTUP, 2, self.NONCE)
        first = staged.Record(
            staged.ROLE_COMMAND,
            staged.Scenario.STARTUP,
            staged.COMMAND_PROCEED,
            1,
            self.NONCE,
        )
        second = staged.Record(
            staged.ROLE_COMMAND,
            staged.Scenario.STARTUP,
            staged.COMMAND_PROCEED,
            2,
            self.NONCE,
        )
        barrier = staged.BlockBarrier(1, header)
        pending = iter((first, first, second))
        barrier._read_command = lambda: next(pending)
        with mock.patch.object(staged.time, "sleep"):
            barrier.proceed(1)
            barrier.proceed(2)

        for invalid in (
            staged.Record(
                staged.ROLE_COMMAND,
                staged.Scenario.STARTUP,
                staged.COMMAND_PROCEED,
                2,
                self.NONCE,
            ),
            staged.Record(
                staged.ROLE_COMMAND,
                staged.Scenario.RESTORE,
                staged.COMMAND_PROCEED,
                1,
                self.NONCE,
            ),
            staged.Record(
                staged.ROLE_STATUS,
                staged.Scenario.STARTUP,
                staged.COMMAND_PROCEED,
                1,
                self.NONCE,
            ),
        ):
            with self.subTest(invalid=invalid):
                barrier = staged.BlockBarrier(1, header)
                barrier._read_command = lambda invalid=invalid: invalid
                with self.assertRaisesRegex(staged.CoordinatorError, "control"):
                    barrier.proceed(1)

        barrier = staged.BlockBarrier(1, header)
        barrier._read_command = lambda: None
        with mock.patch.object(staged, "COMMAND_TIMEOUT_SECONDS", 0.0):
            with self.assertRaisesRegex(staged.CoordinatorError, "timeout"):
                barrier.proceed(1)

    def test_state_machine_rejects_noncanonical_header(self) -> None:
        with self.assertRaisesRegex(staged.CoordinatorError, "control"):
            staged.run_scenario(
                staged.Header(staged.Scenario.RUNTIME, 2, self.NONCE),
                FakeBarrier(),
            )

    def test_startup_state_machine_requires_two_distinct_traffic_cycles(self) -> None:
        barrier = FakeBarrier()
        traffic: list[str] = []
        staged.run_scenario(
            staged.Header(staged.Scenario.STARTUP, 2, self.NONCE),
            barrier,
            interface_count=changing_counts([1, 0, 1, 0]),
            rescan=lambda: None,
            remove_interface=lambda: None,
            traffic=lambda: traffic.append("traffic"),
        )
        self.assertEqual(traffic, ["traffic", "traffic"])
        self.assertEqual(barrier.commands, [1, 2, 3, 4, 5])
        self.assertEqual(
            [kind for _sequence, kind in barrier.statuses],
            [
                staged.Status.INITIAL_PRESENT,
                staged.Status.TRAFFIC_ONE,
                staged.Status.ABSENT,
                staged.Status.PRESENT,
                staged.Status.TRAFFIC_TWO,
                staged.Status.ABSENT,
                staged.Status.COMPLETE,
            ],
        )
        self.assertEqual(
            [sequence for sequence, _kind in barrier.statuses],
            list(range(1, 8)),
        )

    def test_runtime_state_machine_starts_absent_and_finishes_absent(self) -> None:
        barrier = FakeBarrier()
        traffic: list[str] = []
        staged.run_scenario(
            staged.Header(staged.Scenario.RUNTIME, 1, self.NONCE),
            barrier,
            interface_count=changing_counts([0, 1, 0]),
            rescan=lambda: None,
            remove_interface=lambda: None,
            traffic=lambda: traffic.append("traffic"),
        )
        self.assertEqual(traffic, ["traffic"])
        self.assertEqual(barrier.commands, [1, 2, 3])
        self.assertEqual(
            [kind for _sequence, kind in barrier.statuses],
            [
                staged.Status.INITIAL_ABSENT,
                staged.Status.PRESENT,
                staged.Status.TRAFFIC_ONE,
                staged.Status.ABSENT,
                staged.Status.COMPLETE,
            ],
        )

    def test_restore_barrier_separates_source_and_destination_traffic(self) -> None:
        barrier = FakeBarrier()
        traffic: list[str] = []
        staged.run_scenario(
            staged.Header(staged.Scenario.RESTORE, 2, self.NONCE),
            barrier,
            interface_count=changing_counts([1, 1, 0]),
            rescan=lambda: None,
            remove_interface=lambda: None,
            traffic=lambda: traffic.append(f"cycle-{len(traffic) + 1}"),
        )
        self.assertEqual(traffic, ["cycle-1", "cycle-2"])
        self.assertEqual(barrier.commands, [1, 2, 3, 4])
        self.assertEqual(
            [kind for _sequence, kind in barrier.statuses],
            [
                staged.Status.INITIAL_PRESENT,
                staged.Status.CAPTURE_READY,
                staged.Status.PRESENT,
                staged.Status.TRAFFIC_TWO,
                staged.Status.ABSENT,
                staged.Status.COMPLETE,
            ],
        )

    def test_wrong_interface_cardinality_and_traffic_failure_are_terminal(self) -> None:
        barrier = FakeBarrier()
        with self.assertRaisesRegex(staged.CoordinatorError, "topology"):
            staged.run_scenario(
                staged.Header(staged.Scenario.STARTUP, 2, self.NONCE),
                barrier,
                interface_count=lambda: 2,
                rescan=lambda: None,
                remove_interface=lambda: None,
                traffic=lambda: None,
            )

        class TrafficFailure(staged.CoordinatorError):
            pass

        def fail() -> None:
            raise TrafficFailure("traffic")

        with self.assertRaisesRegex(staged.CoordinatorError, "traffic"):
            staged.run_scenario(
                staged.Header(staged.Scenario.RUNTIME, 1, self.NONCE),
                FakeBarrier(),
                interface_count=changing_counts([0, 1]),
                rescan=lambda: None,
                remove_interface=lambda: None,
                traffic=fail,
            )


if __name__ == "__main__":
    unittest.main()
