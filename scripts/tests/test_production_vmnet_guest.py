from __future__ import annotations

import importlib.util
import io
import ipaddress
import socket
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Optional


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"{name} should be importable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


host = load_module(
    "production_vmnet_certification_guest_test_host",
    REPOSITORY_ROOT / "scripts/production_vmnet_certification.py",
)
guest = load_module(
    "production_vmnet_certification_guest",
    REPOSITORY_ROOT / "scripts/guest/production_vmnet_certification.py",
)

MAC = bytes.fromhex("020000000001")
XID = 0x12345678
ADDRESS = ipaddress.IPv4Address("192.168.64.10")
MASK = ipaddress.IPv4Address("255.255.255.0")
ROUTER = ipaddress.IPv4Address("192.168.64.1")
SERVER = ipaddress.IPv4Address("192.168.64.2")
NONCE = bytes(range(1, 33))


def dhcp_option(code: int, payload: bytes) -> bytes:
    return bytes((code, len(payload))) + payload


def reply_packet(
    message_type: int,
    *,
    xid: int = XID,
    mac: bytes = MAC,
    address: ipaddress.IPv4Address = ADDRESS,
    mask: ipaddress.IPv4Address = MASK,
    router: ipaddress.IPv4Address = ROUTER,
    server: ipaddress.IPv4Address = SERVER,
    lease_seconds: int = 3600,
    options_override: Optional[bytes] = None,
) -> bytes:
    header = guest.DHCP_FIXED_HEADER.pack(
        guest.BOOTREPLY,
        guest.ETHERNET_HARDWARE,
        guest.ETHERNET_ADDRESS_BYTES,
        0,
        xid,
        0,
        0,
        bytes(4),
        address.packed,
        bytes(4),
        bytes(4),
        mac + bytes(10),
        bytes(64),
        bytes(128),
    )
    if options_override is None:
        options = (
            dhcp_option(guest.DHCP_OPTION_MESSAGE_TYPE, bytes((message_type,)))
            + dhcp_option(guest.DHCP_OPTION_SUBNET_MASK, mask.packed)
            + dhcp_option(guest.DHCP_OPTION_ROUTER, router.packed)
            + dhcp_option(guest.DHCP_OPTION_LEASE_TIME, struct.pack("!I", lease_seconds))
            + dhcp_option(guest.DHCP_OPTION_SERVER_ID, server.packed)
            + bytes((guest.DHCP_OPTION_END,))
        )
    else:
        options = options_override
    return header + guest.DHCP_MAGIC_COOKIE + options


def sample_lease() -> object:
    reply = guest.parse_dhcp_reply(reply_packet(guest.DHCP_OFFER), XID, MAC)
    assert reply is not None
    return guest.lease_from_offer(reply)


class FakeClock:
    def __init__(self, value: float = 100.0) -> None:
        self.value = value

    def __call__(self) -> float:
        return self.value

    def advance(self, seconds: float) -> None:
        self.value += seconds


class FakeTransport:
    def __init__(
        self, responses: list[Optional[bytes]], clock: Optional[FakeClock] = None
    ) -> None:
        self.responses = list(responses)
        self.sent: list[bytes] = []
        self.clock = clock
        self.closed = False
        self.timeouts: list[float] = []

    def send(self, packet: bytes) -> None:
        self.sent.append(packet)

    def receive(self, timeout: float) -> bytes:
        self.timeouts.append(timeout)
        if not self.responses:
            if self.clock is not None:
                self.clock.advance(timeout)
            raise TimeoutError
        response = self.responses.pop(0)
        if response is None:
            if self.clock is not None:
                self.clock.advance(timeout)
            raise TimeoutError
        return response

    def close(self) -> None:
        self.closed = True


class FakeDhcpSocket:
    def __init__(self) -> None:
        self.options: list[tuple[int, int, object]] = []
        self.bound: Optional[tuple[str, int]] = None
        self.sent: list[tuple[bytes, tuple[str, int]]] = []
        self.send_count: Optional[int] = None
        self.timeout: Optional[float] = None
        self.response = (reply_packet(guest.DHCP_OFFER), ("192.168.64.2", 67))
        self.closed = False

    def setsockopt(self, level: int, option: int, value: object) -> None:
        self.options.append((level, option, value))

    def bind(self, address: tuple[str, int]) -> None:
        self.bound = address

    def sendto(self, packet: bytes, peer: tuple[str, int]) -> int:
        self.sent.append((packet, peer))
        return len(packet) if self.send_count is None else self.send_count

    def settimeout(self, value: float) -> None:
        self.timeout = value

    def recvfrom(self, _size: int):
        return self.response

    def close(self) -> None:
        self.closed = True


class FakeTcpSocket:
    def __init__(self, responses: list[bytes], *, send_limit: int = 40) -> None:
        self.responses = list(responses)
        self.send_limit = send_limit
        self.sent = bytearray()
        self.timeouts: list[float] = []
        self.shutdown_how: Optional[int] = None
        self.closed = False

    def settimeout(self, value: float) -> None:
        self.timeouts.append(value)

    def send(self, data: memoryview) -> int:
        count = min(len(data), self.send_limit)
        self.sent.extend(bytes(data[:count]))
        return count

    def shutdown(self, how: int) -> None:
        self.shutdown_how = how

    def recv(self, size: int) -> bytes:
        if not self.responses:
            return b""
        value = self.responses.pop(0)
        if len(value) <= size:
            return value
        self.responses.insert(0, value[size:])
        return value[:size]

    def close(self) -> None:
        self.closed = True


class ProductionVmnetGuestTests(unittest.TestCase):
    def assert_phase(self, phase: str, callback) -> None:
        with self.assertRaises(guest.GuestError) as caught:
            callback()
        self.assertEqual(caught.exception.phase, phase)
        self.assertNotIn("PRIVATE-SENTINEL", str(caught.exception))

    def test_control_and_tcp_contract_match_independent_host_codec(self) -> None:
        for mode in ("shared", "host", "bridged"):
            sector = host.encode_guest_control(mode, "192.168.64.1", 23456, NONCE)
            self.assertEqual(
                guest.decode_guest_control(sector),
                guest.GuestControl(mode, "192.168.64.1", 23456, NONCE),
            )
        self.assertEqual(guest.tcp_request(NONCE), host.tcp_request(NONCE))
        self.assertEqual(guest.tcp_response(NONCE), host.tcp_response(NONCE))

        sector = host.encode_guest_control("shared", "192.168.64.1", 23456, NONCE)
        for offset in (0, 8, 10, 11, 12, 16, 18, 50, 64, 96, 511):
            hostile = bytearray(sector)
            hostile[offset] ^= 1
            self.assert_phase(
                "control", lambda value=bytes(hostile): guest.decode_guest_control(value)
            )

    def test_dhcp_discover_and_request_are_exact(self) -> None:
        discover = guest.encode_dhcp_discover(XID, MAC)
        self.assertEqual(len(discover), 300)
        header = guest.DHCP_FIXED_HEADER.unpack_from(discover)
        self.assertEqual(header[:7], (1, 1, 6, 0, XID, 0, 0x8000))
        self.assertEqual(header[11], MAC + bytes(10))
        self.assertEqual(discover[236:240], guest.DHCP_MAGIC_COOKIE)
        expected_options = (
            b"\x35\x01\x01"
            + b"\x3d\x07\x01"
            + MAC
            + b"\x37\x04\x01\x03\x33\x36\xff"
        )
        self.assertEqual(discover[240 : 240 + len(expected_options)], expected_options)
        self.assertFalse(any(discover[240 + len(expected_options) :]))

        lease = sample_lease()
        request = guest.encode_dhcp_request(XID, MAC, lease)
        self.assertIn(b"\x32\x04" + ADDRESS.packed, request)
        self.assertIn(b"\x36\x04" + SERVER.packed, request)
        self.assertEqual(request[270], guest.DHCP_OPTION_END)
        self.assertFalse(any(request[271:]))
        self.assert_phase("dhcp", lambda: guest.encode_dhcp_request(XID, MAC, None))

    def test_dhcp_reply_filters_unrelated_identity_then_rejects_matching_malformed(self) -> None:
        self.assertIsNone(
            guest.parse_dhcp_reply(
                reply_packet(guest.DHCP_OFFER, xid=XID + 1), XID, MAC
            )
        )
        other_mac = bytes.fromhex("020000000002")
        self.assertIsNone(
            guest.parse_dhcp_reply(
                reply_packet(guest.DHCP_OFFER, mac=other_mac), XID, MAC
            )
        )
        matching = reply_packet(guest.DHCP_OFFER)
        self.assert_phase(
            "dhcp", lambda: guest.parse_dhcp_reply(bytearray(matching), XID, MAC)
        )
        hostile_packets = [
            matching[:34],
            matching[:239],
            matching[:-1],
            matching[:236] + b"BAD!" + matching[240:],
            matching + b"\x01",
            matching + bytes(600),
        ]
        operation = bytearray(matching)
        operation[0] = guest.BOOTREQUEST
        hostile_packets.append(bytes(operation))
        hardware = bytearray(matching)
        hardware[1] = 2
        hostile_packets.append(bytes(hardware))
        hardware_length = bytearray(matching)
        hardware_length[2] = 5
        hostile_packets.append(bytes(hardware_length))
        padded_mac = bytearray(matching)
        padded_mac[34] = 1
        hostile_packets.append(bytes(padded_mac))
        for packet in hostile_packets:
            with self.subTest(length=len(packet), prefix=packet[:4]):
                self.assert_phase(
                    "dhcp", lambda value=packet: guest.parse_dhcp_reply(value, XID, MAC)
                )

    def test_dhcp_options_are_strict_and_duplicate_safe(self) -> None:
        base = (
            dhcp_option(guest.DHCP_OPTION_MESSAGE_TYPE, b"\x02")
            + dhcp_option(guest.DHCP_OPTION_SUBNET_MASK, MASK.packed)
            + dhcp_option(guest.DHCP_OPTION_ROUTER, ROUTER.packed)
            + dhcp_option(guest.DHCP_OPTION_LEASE_TIME, struct.pack("!I", 3600))
            + dhcp_option(guest.DHCP_OPTION_SERVER_ID, SERVER.packed)
        )
        cases = [
            base,
            base + b"\x35\x01\x02\xff",
            base + b"\x34\x01\x00\xff",
            base + b"\x7f\x04\x01\x02",
            base + b"\xff\x01",
            base.replace(b"\x01\x04" + MASK.packed, b"\x01\x03abc") + b"\xff",
            base.replace(b"\x03\x04" + ROUTER.packed, b"\x03\x03abc") + b"\xff",
        ]
        for options in cases:
            self.assert_phase(
                "dhcp",
                lambda value=options: guest.parse_dhcp_reply(
                    reply_packet(guest.DHCP_OFFER, options_override=value), XID, MAC
                ),
            )

    def test_offer_and_ack_require_coherent_network_values(self) -> None:
        offer_reply = guest.parse_dhcp_reply(reply_packet(guest.DHCP_OFFER), XID, MAC)
        assert offer_reply is not None
        lease = guest.lease_from_offer(offer_reply)
        self.assertEqual(lease.address, ADDRESS)
        self.assertEqual(lease.prefix_length, 24)
        self.assertEqual(lease.router, ROUTER)
        ack = guest.parse_dhcp_reply(reply_packet(guest.DHCP_ACK), XID, MAC)
        assert ack is not None
        self.assertEqual(guest.validate_ack(ack, lease), lease)

        bad_values = [
            reply_packet(guest.DHCP_OFFER, address=ipaddress.IPv4Address("0.0.0.0")),
            reply_packet(guest.DHCP_OFFER, mask=ipaddress.IPv4Address("255.0.255.0")),
            reply_packet(guest.DHCP_OFFER, router=ipaddress.IPv4Address("10.0.0.1")),
            reply_packet(guest.DHCP_OFFER, router=ADDRESS),
            reply_packet(guest.DHCP_OFFER, lease_seconds=0),
        ]
        for packet in bad_values:
            reply = guest.parse_dhcp_reply(packet, XID, MAC)
            assert reply is not None
            self.assert_phase("dhcp", lambda value=reply: guest.lease_from_offer(value))

        nak = guest.parse_dhcp_reply(reply_packet(guest.DHCP_NAK), XID, MAC)
        assert nak is not None
        self.assert_phase("dhcp", lambda: guest.validate_ack(nak, lease))
        drift = guest.parse_dhcp_reply(
            reply_packet(guest.DHCP_ACK, lease_seconds=3601), XID, MAC
        )
        assert drift is not None
        self.assert_phase("dhcp", lambda: guest.validate_ack(drift, lease))

    def test_acquisition_retries_without_resetting_absolute_deadline(self) -> None:
        clock = FakeClock()
        offer = reply_packet(guest.DHCP_OFFER)
        ack = reply_packet(guest.DHCP_ACK)
        transport = FakeTransport([None, offer, ack], clock)
        lease = guest.acquire_lease(
            transport, MAC, XID, clock() + 30, clock=clock
        )
        self.assertEqual(lease.address, ADDRESS)
        self.assertEqual(len(transport.sent), 3)
        self.assertEqual(transport.sent[0], transport.sent[1])
        self.assertNotEqual(transport.sent[1], transport.sent[2])

        deadline_clock = FakeClock()
        no_reply = FakeTransport([None, None, None], deadline_clock)
        self.assert_phase(
            "dhcp",
            lambda: guest.acquire_lease(
                no_reply, MAC, XID, deadline_clock() + 6, clock=deadline_clock
            ),
        )
        self.assertEqual(deadline_clock(), 106.0)
        self.assertEqual(len(no_reply.sent), 2)
        self.assert_phase(
            "dhcp",
            lambda: guest.acquire_lease(
                FakeTransport([]), MAC, XID, float("nan"), clock=lambda: 100
            ),
        )

        unrelated = reply_packet(guest.DHCP_OFFER, xid=XID + 1)
        transport = FakeTransport([unrelated, offer, ack])
        self.assertEqual(
            guest.acquire_lease(transport, MAC, XID, 200, clock=lambda: 100).address,
            ADDRESS,
        )

    def test_socket_transport_is_interface_bound_and_fail_closed(self) -> None:
        interface = guest.GuestInterface("eth0", MAC)
        created: list[tuple[int, int, int]] = []
        connection = FakeDhcpSocket()

        def factory(family: int, kind: int, protocol: int):
            created.append((family, kind, protocol))
            return connection

        transport = guest.SocketDhcpTransport(interface, socket_factory=factory)
        self.assertEqual(
            created, [(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)]
        )
        self.assertEqual(
            connection.options,
            [
                (socket.SOL_SOCKET, socket.SO_BROADCAST, 1),
                (socket.SOL_SOCKET, socket.SO_REUSEADDR, 1),
                (
                    socket.SOL_SOCKET,
                    getattr(socket, "SO_BINDTODEVICE", 25),
                    b"eth0\0",
                ),
            ],
        )
        self.assertEqual(connection.bound, ("0.0.0.0", guest.DHCP_CLIENT_PORT))
        packet = guest.encode_dhcp_discover(XID, MAC)
        transport.send(packet)
        self.assertEqual(
            connection.sent,
            [(packet, ("255.255.255.255", guest.DHCP_SERVER_PORT))],
        )
        self.assertEqual(transport.receive(1.25), reply_packet(guest.DHCP_OFFER))
        self.assertEqual(connection.timeout, 1.25)

        connection.send_count = len(packet) - 1
        self.assert_phase("dhcp", lambda: transport.send(packet))
        connection.response = (reply_packet(guest.DHCP_OFFER), ("192.168.64.2", 68))
        self.assert_phase("dhcp", lambda: transport.receive(1))
        connection.response = (b"x" * (guest.DHCP_MAX_PACKET_BYTES + 1), ("0.0.0.0", 67))
        self.assert_phase("dhcp", lambda: transport.receive(1))
        self.assert_phase("dhcp", lambda: transport.receive(float("nan")))
        transport.close()
        self.assertTrue(connection.closed)

    def test_conflicting_offer_and_matching_malformed_are_not_ignored(self) -> None:
        offer = reply_packet(guest.DHCP_OFFER)
        conflicting = reply_packet(
            guest.DHCP_OFFER, address=ipaddress.IPv4Address("192.168.64.11")
        )
        transport = FakeTransport([offer, conflicting])
        self.assert_phase(
            "dhcp",
            lambda: guest.acquire_lease(transport, MAC, XID, 200, clock=lambda: 100),
        )

        malformed = reply_packet(guest.DHCP_OFFER)[:-1]
        transport = FakeTransport([malformed])
        self.assert_phase(
            "dhcp",
            lambda: guest.acquire_lease(transport, MAC, XID, 200, clock=lambda: 100),
        )

    def make_interface(self, root: Path, name: str, address: str = "02:00:00:00:00:01") -> None:
        drivers = root.parent / "drivers"
        driver = drivers / "virtio_net"
        driver.mkdir(parents=True, exist_ok=True)
        entry = root / name
        (entry / "device").mkdir(parents=True)
        (entry / "device/driver").symlink_to(driver, target_is_directory=True)
        (entry / "type").write_text("1\n", encoding="ascii")
        (entry / "address").write_text(address + "\n", encoding="ascii")

    def test_interface_discovery_is_exact(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            root = temp / "net"
            root.mkdir()
            self.make_interface(root, "eth0")
            self.assertEqual(
                guest.discover_interface(root), guest.GuestInterface("eth0", MAC)
            )
            self.make_interface(root, "eth1", "02:00:00:00:00:02")
            self.assert_phase("interface", lambda: guest.discover_interface(root))

        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            root = temp / "net"
            root.mkdir()
            self.make_interface(root, "eth0", "01:00:00:00:00:01")
            self.assert_phase("interface", lambda: guest.discover_interface(root))

    def test_network_configuration_argv_and_reverse_cleanup_are_exact(self) -> None:
        calls: list[tuple[tuple[str, ...], dict[str, object]]] = []

        def runner(command, **kwargs):
            calls.append((tuple(command), kwargs))
            return SimpleNamespace(returncode=0)

        interface = guest.GuestInterface("eth0", MAC)
        self.assert_phase(
            "interface",
            lambda: guest.NetworkConfigurator(
                guest.GuestInterface("bad/interface", MAC), runner=runner
            ),
        )
        network = guest.NetworkConfigurator(interface, runner=runner)
        network.bring_up()
        network.apply(sample_lease())
        network.cleanup()
        self.assertEqual(
            [call[0] for call in calls],
            [
                (guest.IP_EXECUTABLE, "link", "set", "dev", "eth0", "up"),
                (guest.IP_EXECUTABLE, "address", "replace", "192.168.64.10/24", "dev", "eth0"),
                (guest.IP_EXECUTABLE, "route", "replace", "default", "via", "192.168.64.1", "dev", "eth0"),
                (guest.IP_EXECUTABLE, "route", "del", "default", "via", "192.168.64.1", "dev", "eth0"),
                (guest.IP_EXECUTABLE, "address", "del", "192.168.64.10/24", "dev", "eth0"),
                (guest.IP_EXECUTABLE, "link", "set", "dev", "eth0", "down"),
            ],
        )
        for _command, kwargs in calls:
            self.assertEqual(kwargs["cwd"], "/")
            self.assertEqual(kwargs["env"], guest.MINIMAL_ENVIRONMENT)
            self.assertFalse(kwargs["check"])

    def test_tcp_exchange_requires_exact_response_and_eof(self) -> None:
        control = guest.GuestControl("shared", "192.168.64.1", 23456, NONCE)
        self.assert_phase(
            "tcp",
            lambda: guest.tcp_exchange(
                guest.GuestControl("shared", "192.168.064.1", 23456, NONCE),
                110,
                connector=lambda *_args, **_kwargs: FakeTcpSocket([]),
                clock=lambda: 100,
            ),
        )
        connection = FakeTcpSocket(
            [guest.tcp_response(NONCE)[:7], guest.tcp_response(NONCE)[7:], b""],
            send_limit=3,
        )
        connector_calls: list[tuple[object, float]] = []

        def connector(endpoint, *, timeout):
            connector_calls.append((endpoint, timeout))
            return connection

        guest.tcp_exchange(control, 110, connector=connector, clock=lambda: 100)
        self.assertEqual(bytes(connection.sent), guest.tcp_request(NONCE))
        self.assertEqual(connection.shutdown_how, socket.SHUT_WR)
        self.assertTrue(connection.closed)
        self.assertEqual(connector_calls[0][0], ("192.168.64.1", 23456))

        hostile = [
            [guest.tcp_response(NONCE)[:-1], b""],
            [guest.tcp_response(bytes(reversed(NONCE))), b""],
            [guest.tcp_response(NONCE), b"x"],
        ]
        for responses in hostile:
            self.assert_phase(
                "tcp",
                lambda value=responses: guest.tcp_exchange(
                    control,
                    110,
                    connector=lambda *_args, **_kwargs: FakeTcpSocket(list(value)),
                    clock=lambda: 100,
                ),
            )

        timeout_error = FakeTcpSocket([guest.tcp_response(NONCE), b""])

        def reject_timeout(_value: float) -> None:
            raise ValueError("PRIVATE-SENTINEL")

        timeout_error.settimeout = reject_timeout
        self.assert_phase(
            "tcp",
            lambda: guest.tcp_exchange(
                control,
                110,
                connector=lambda *_args, **_kwargs: timeout_error,
                clock=lambda: 100,
            ),
        )
        self.assert_phase(
            "tcp",
            lambda: guest.tcp_exchange(
                control,
                float("inf"),
                connector=lambda *_args, **_kwargs: FakeTcpSocket([]),
                clock=lambda: 100,
            ),
        )

    def test_full_injected_run_and_cleanup_precedence(self) -> None:
        control = guest.GuestControl("shared", "192.168.64.1", 23456, NONCE)
        interface = guest.GuestInterface("eth0", MAC)
        transport = FakeTransport(
            [reply_packet(guest.DHCP_OFFER), reply_packet(guest.DHCP_ACK)]
        )
        commands: list[tuple[str, ...]] = []

        def runner(command, **_kwargs):
            commands.append(tuple(command))
            return SimpleNamespace(returncode=0)

        tcp = FakeTcpSocket([guest.tcp_response(NONCE), b""])
        guest.run_certification(
            control_reader=lambda: control,
            interface_discoverer=lambda: interface,
            transport_factory=lambda _interface: transport,
            random_bytes=lambda _count: XID.to_bytes(4, "big"),
            command_runner=runner,
            connector=lambda *_args, **_kwargs: tcp,
            clock=lambda: 100,
        )
        self.assertTrue(transport.closed)
        self.assertTrue(tcp.closed)
        self.assertEqual(commands[-1], (guest.IP_EXECUTABLE, "link", "set", "dev", "eth0", "down"))

        cleanup_calls = 0

        def cleanup_fails(command, **_kwargs):
            nonlocal cleanup_calls
            cleanup_calls += 1
            if command[-1] == "down":
                return SimpleNamespace(returncode=1)
            return SimpleNamespace(returncode=0)

        transport = FakeTransport(
            [reply_packet(guest.DHCP_OFFER), reply_packet(guest.DHCP_ACK)]
        )
        self.assert_phase(
            "cleanup",
            lambda: guest.run_certification(
                control_reader=lambda: control,
                interface_discoverer=lambda: interface,
                transport_factory=lambda _interface: transport,
                random_bytes=lambda _count: XID.to_bytes(4, "big"),
                command_runner=cleanup_fails,
                connector=lambda *_args, **_kwargs: FakeTcpSocket(
                    [b"PRIVATE-SENTINEL", b""]
                ),
                clock=lambda: 100,
            ),
        )
        self.assertGreater(cleanup_calls, 0)

    def test_main_uses_only_fixed_markers(self) -> None:
        success = io.StringIO()
        self.assertEqual(
            guest.main([], certification=lambda: None, stream=success), 0
        )
        self.assertEqual(
            success.getvalue(), guest.BEGIN_MARKER + "\n" + guest.SUCCESS_MARKER + "\n"
        )

        for phase in sorted(guest.FAILURE_PHASES):
            output = io.StringIO()

            def fail(value=phase):
                raise guest.GuestError(value)

            self.assertEqual(guest.main([], certification=fail, stream=output), 3)
            self.assertEqual(
                output.getvalue(),
                guest.BEGIN_MARKER
                + "\n"
                + guest.FAILURE_PREFIX
                + phase.upper()
                + "\n",
            )
            self.assertNotIn("PRIVATE-SENTINEL", output.getvalue())

        output = io.StringIO()
        self.assertEqual(
            guest.main(["PRIVATE-SENTINEL"], certification=lambda: None, stream=output),
            3,
        )
        self.assertNotIn("PRIVATE-SENTINEL", output.getvalue())


if __name__ == "__main__":
    unittest.main()
