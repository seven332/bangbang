#!/usr/bin/python3
"""Guest-side DHCP and traffic oracle for production vmnet certification."""

from __future__ import annotations

import hashlib
import ipaddress
import math
import os
import re
import socket
import stat
import struct
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional, Protocol, Sequence


SCHEMA_VERSION = 1
CONTROL_BYTES = 512
CONTROL_PREFIX_BYTES = 64
CONTROL_DIGEST_BYTES = 32
CONTROL_MAGIC = b"BBVMNET1"
CONTROL_MODES = {1: "shared", 2: "host", 3: "bridged"}
CONTROL_PREFIX = struct.Struct("!8sHBB4sH32s14s")

TCP_REQUEST_MAGIC = b"BBVREQ1\0"
TCP_RESPONSE_MAGIC = b"BBVRES1\0"
TCP_RECORD_BYTES = 40

BOOTREQUEST = 1
BOOTREPLY = 2
ETHERNET_HARDWARE = 1
ETHERNET_ADDRESS_BYTES = 6
DHCP_CLIENT_PORT = 68
DHCP_SERVER_PORT = 67
DHCP_MAGIC_COOKIE = b"\x63\x82\x53\x63"
DHCP_MIN_PACKET_BYTES = 300
DHCP_MAX_PACKET_BYTES = 576
DHCP_FIXED_HEADER = struct.Struct("!BBBBIHH4s4s4s4s16s64s128s")
DHCP_DISCOVER = 1
DHCP_OFFER = 2
DHCP_REQUEST = 3
DHCP_ACK = 5
DHCP_NAK = 6
DHCP_OPTION_SUBNET_MASK = 1
DHCP_OPTION_ROUTER = 3
DHCP_OPTION_REQUESTED_ADDRESS = 50
DHCP_OPTION_LEASE_TIME = 51
DHCP_OPTION_OVERLOAD = 52
DHCP_OPTION_MESSAGE_TYPE = 53
DHCP_OPTION_SERVER_ID = 54
DHCP_OPTION_PARAMETER_REQUEST = 55
DHCP_OPTION_CLIENT_ID = 61
DHCP_OPTION_PAD = 0
DHCP_OPTION_END = 255
DHCP_REQUESTED_OPTIONS = bytes(
    (
        DHCP_OPTION_SUBNET_MASK,
        DHCP_OPTION_ROUTER,
        DHCP_OPTION_LEASE_TIME,
        DHCP_OPTION_SERVER_ID,
    )
)
DHCP_SINGLETON_OPTIONS = frozenset(
    {
        DHCP_OPTION_SUBNET_MASK,
        DHCP_OPTION_ROUTER,
        DHCP_OPTION_REQUESTED_ADDRESS,
        DHCP_OPTION_LEASE_TIME,
        DHCP_OPTION_OVERLOAD,
        DHCP_OPTION_MESSAGE_TYPE,
        DHCP_OPTION_SERVER_ID,
        DHCP_OPTION_PARAMETER_REQUEST,
        DHCP_OPTION_CLIENT_ID,
    }
)
DHCP_ATTEMPTS = 3
DHCP_DEADLINE_SECONDS = 30
DHCP_ATTEMPT_SECONDS = 5
DHCP_PACKET_LIMIT = 64
TCP_DEADLINE_SECONDS = 10
COMMAND_TIMEOUT_SECONDS = 5

INTERFACE_RE = re.compile(r"[A-Za-z0-9_.-]{1,15}\Z")
MAC_RE = re.compile(r"[0-9a-f]{2}(?::[0-9a-f]{2}){5}\Z")
IP_EXECUTABLE = "/usr/bin/ip"
MINIMAL_ENVIRONMENT = {
    "LANG": "C",
    "LC_ALL": "C",
    "PATH": "/usr/sbin:/usr/bin:/sbin:/bin",
}

BEGIN_MARKER = "BANGBANG_PRODUCTION_VMNET_CERTIFICATION_BEGIN"
SUCCESS_MARKER = "BANGBANG_PRODUCTION_VMNET_CERTIFICATION_OK"
FAILURE_PREFIX = "BANGBANG_PRODUCTION_VMNET_CERTIFICATION_FAIL_"
FAILURE_PHASES = frozenset(
    {"control", "interface", "dhcp", "configure", "tcp", "cleanup", "internal"}
)


class GuestError(RuntimeError):
    """A closed, non-sensitive guest failure."""

    def __init__(self, phase: str) -> None:
        if phase not in FAILURE_PHASES:
            phase = "internal"
        super().__init__(phase)
        self.phase = phase


@dataclass(frozen=True)
class GuestControl:
    mode: str
    endpoint_ipv4: str
    endpoint_port: int
    nonce: bytes


@dataclass(frozen=True)
class GuestInterface:
    name: str
    mac: bytes


@dataclass(frozen=True)
class ParsedDhcpReply:
    message_type: int
    offered_address: ipaddress.IPv4Address
    options: dict[int, bytes]


@dataclass(frozen=True)
class DhcpLease:
    address: ipaddress.IPv4Address
    prefix_length: int
    subnet_mask: ipaddress.IPv4Address
    router: ipaddress.IPv4Address
    server: ipaddress.IPv4Address
    lease_seconds: int


class DhcpTransport(Protocol):
    def send(self, packet: bytes) -> None: ...

    def receive(self, timeout: float) -> bytes: ...


def _valid_endpoint(address: ipaddress.IPv4Address) -> bool:
    return not (
        address.is_unspecified
        or address.is_multicast
        or address.is_loopback
        or int(address) == 0xFFFF_FFFF
    )


def decode_guest_control(data: bytes) -> GuestControl:
    if not isinstance(data, bytes) or len(data) != CONTROL_BYTES:
        raise GuestError("control")
    prefix = data[:CONTROL_PREFIX_BYTES]
    digest = data[CONTROL_PREFIX_BYTES : CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES]
    tail = data[CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES :]
    try:
        magic, version, mode_value, family, raw_address, port, nonce, reserved = (
            CONTROL_PREFIX.unpack(prefix)
        )
        address = ipaddress.IPv4Address(raw_address)
    except (struct.error, ipaddress.AddressValueError) as error:
        raise GuestError("control") from error
    mode = CONTROL_MODES.get(mode_value)
    if (
        magic != CONTROL_MAGIC
        or version != SCHEMA_VERSION
        or mode is None
        or family != 4
        or not _valid_endpoint(address)
        or port == 0
        or not any(nonce)
        or any(reserved)
        or digest != hashlib.sha256(prefix).digest()
        or any(tail)
    ):
        raise GuestError("control")
    return GuestControl(mode, str(address), port, nonce)


def read_guest_control(path: Path = Path("/dev/vdb")) -> GuestControl:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
        try:
            data = os.pread(descriptor, CONTROL_BYTES, 0)
        finally:
            os.close(descriptor)
    except OSError as error:
        raise GuestError("control") from error
    return decode_guest_control(data)


def tcp_request(nonce: bytes) -> bytes:
    if not isinstance(nonce, bytes) or len(nonce) != 32 or not any(nonce):
        raise GuestError("control")
    return TCP_REQUEST_MAGIC + nonce


def tcp_response(nonce: bytes) -> bytes:
    if not isinstance(nonce, bytes) or len(nonce) != 32 or not any(nonce):
        raise GuestError("control")
    return TCP_RESPONSE_MAGIC + nonce


def _read_bounded_text(path: Path, maximum: int) -> str:
    try:
        metadata = os.lstat(path)
        if not stat.S_ISREG(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            raise GuestError("interface")
        data = path.read_bytes()
    except GuestError:
        raise
    except OSError as error:
        raise GuestError("interface") from error
    if not data or len(data) > maximum:
        raise GuestError("interface")
    try:
        return data.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise GuestError("interface") from error


def _parse_mac(value: str) -> bytes:
    if MAC_RE.fullmatch(value) is None:
        raise GuestError("interface")
    mac = bytes.fromhex(value.replace(":", ""))
    if len(mac) != ETHERNET_ADDRESS_BYTES or not any(mac) or mac[0] & 1:
        raise GuestError("interface")
    return mac


def discover_interface(sysfs_root: Path = Path("/sys/class/net")) -> GuestInterface:
    try:
        entries = sorted(sysfs_root.iterdir(), key=lambda entry: entry.name)
    except OSError as error:
        raise GuestError("interface") from error
    if len(entries) > 64:
        raise GuestError("interface")
    candidates: list[GuestInterface] = []
    for entry in entries:
        name = entry.name
        if name == "lo" or INTERFACE_RE.fullmatch(name) is None:
            continue
        device = entry / "device"
        driver = device / "driver"
        try:
            if not device.exists() or not driver.is_symlink():
                continue
            driver_name = driver.resolve(strict=True).name
        except OSError:
            continue
        if driver_name != "virtio_net":
            continue
        if _read_bounded_text(entry / "type", 16) != "1":
            raise GuestError("interface")
        candidates.append(
            GuestInterface(name, _parse_mac(_read_bounded_text(entry / "address", 32)))
        )
    if len(candidates) != 1:
        raise GuestError("interface")
    return candidates[0]


def _validate_transaction(xid: int, mac: bytes) -> None:
    if (
        isinstance(xid, bool)
        or not isinstance(xid, int)
        or not 1 <= xid <= 0xFFFF_FFFF
        or not isinstance(mac, bytes)
        or len(mac) != ETHERNET_ADDRESS_BYTES
        or not any(mac)
        or mac[0] & 1
    ):
        raise GuestError("dhcp")


def _dhcp_option(code: int, payload: bytes) -> bytes:
    if not 1 <= code <= 254 or not 1 <= len(payload) <= 255:
        raise GuestError("dhcp")
    return bytes((code, len(payload))) + payload


def _boot_request_header(xid: int, mac: bytes) -> bytes:
    _validate_transaction(xid, mac)
    return DHCP_FIXED_HEADER.pack(
        BOOTREQUEST,
        ETHERNET_HARDWARE,
        ETHERNET_ADDRESS_BYTES,
        0,
        xid,
        0,
        0x8000,
        bytes(4),
        bytes(4),
        bytes(4),
        bytes(4),
        mac + bytes(16 - len(mac)),
        bytes(64),
        bytes(128),
    )


def _finalize_dhcp_request(packet: bytes) -> bytes:
    if len(packet) > DHCP_MAX_PACKET_BYTES:
        raise GuestError("dhcp")
    return packet + bytes(max(0, DHCP_MIN_PACKET_BYTES - len(packet)))


def encode_dhcp_discover(xid: int, mac: bytes) -> bytes:
    packet = (
        _boot_request_header(xid, mac)
        + DHCP_MAGIC_COOKIE
        + _dhcp_option(DHCP_OPTION_MESSAGE_TYPE, bytes((DHCP_DISCOVER,)))
        + _dhcp_option(DHCP_OPTION_CLIENT_ID, bytes((ETHERNET_HARDWARE,)) + mac)
        + _dhcp_option(DHCP_OPTION_PARAMETER_REQUEST, DHCP_REQUESTED_OPTIONS)
        + bytes((DHCP_OPTION_END,))
    )
    return _finalize_dhcp_request(packet)


def encode_dhcp_request(xid: int, mac: bytes, lease: DhcpLease) -> bytes:
    _validate_transaction(xid, mac)
    if not isinstance(lease, DhcpLease):
        raise GuestError("dhcp")
    packet = (
        _boot_request_header(xid, mac)
        + DHCP_MAGIC_COOKIE
        + _dhcp_option(DHCP_OPTION_MESSAGE_TYPE, bytes((DHCP_REQUEST,)))
        + _dhcp_option(DHCP_OPTION_REQUESTED_ADDRESS, lease.address.packed)
        + _dhcp_option(DHCP_OPTION_SERVER_ID, lease.server.packed)
        + _dhcp_option(DHCP_OPTION_CLIENT_ID, bytes((ETHERNET_HARDWARE,)) + mac)
        + _dhcp_option(DHCP_OPTION_PARAMETER_REQUEST, DHCP_REQUESTED_OPTIONS)
        + bytes((DHCP_OPTION_END,))
    )
    return _finalize_dhcp_request(packet)


def _parse_dhcp_options(data: bytes, mac: bytes) -> dict[int, bytes]:
    options: dict[int, bytes] = {}
    offset = DHCP_FIXED_HEADER.size + len(DHCP_MAGIC_COOKIE)
    found_end = False
    while offset < len(data):
        code = data[offset]
        offset += 1
        if code == DHCP_OPTION_PAD:
            continue
        if code == DHCP_OPTION_END:
            found_end = True
            if any(data[offset:]):
                raise GuestError("dhcp")
            break
        if offset >= len(data):
            raise GuestError("dhcp")
        length = data[offset]
        offset += 1
        if length == 0 or offset + length > len(data):
            raise GuestError("dhcp")
        payload = data[offset : offset + length]
        offset += length
        if code in DHCP_SINGLETON_OPTIONS and code in options:
            raise GuestError("dhcp")
        if code == DHCP_OPTION_OVERLOAD:
            raise GuestError("dhcp")
        options.setdefault(code, payload)
    if not found_end:
        raise GuestError("dhcp")

    lengths = {
        DHCP_OPTION_SUBNET_MASK: 4,
        DHCP_OPTION_LEASE_TIME: 4,
        DHCP_OPTION_MESSAGE_TYPE: 1,
        DHCP_OPTION_SERVER_ID: 4,
    }
    for code, expected in lengths.items():
        if code in options and len(options[code]) != expected:
            raise GuestError("dhcp")
    if DHCP_OPTION_ROUTER in options and (
        not options[DHCP_OPTION_ROUTER]
        or len(options[DHCP_OPTION_ROUTER]) % 4 != 0
    ):
        raise GuestError("dhcp")
    if DHCP_OPTION_CLIENT_ID in options and options[DHCP_OPTION_CLIENT_ID] != (
        bytes((ETHERNET_HARDWARE,)) + mac
    ):
        raise GuestError("dhcp")
    return options


def parse_dhcp_reply(
    data: bytes, xid: int, mac: bytes
) -> Optional[ParsedDhcpReply]:
    _validate_transaction(xid, mac)
    if not isinstance(data, bytes):
        raise GuestError("dhcp")
    if len(data) < 34:
        return None
    packet_xid = struct.unpack_from("!I", data, 4)[0]
    packet_mac = data[28:34]
    if packet_xid != xid or packet_mac != mac:
        return None
    if not DHCP_FIXED_HEADER.size + 5 <= len(data) <= DHCP_MAX_PACKET_BYTES:
        raise GuestError("dhcp")
    try:
        (
            operation,
            hardware_type,
            hardware_length,
            _hops,
            parsed_xid,
            _seconds,
            _flags,
            _client_address,
            offered_address,
            _server_address,
            _relay_address,
            client_hardware,
            _server_name,
            _boot_file,
        ) = DHCP_FIXED_HEADER.unpack_from(data)
    except struct.error as error:
        raise GuestError("dhcp") from error
    if (
        operation != BOOTREPLY
        or hardware_type != ETHERNET_HARDWARE
        or hardware_length != ETHERNET_ADDRESS_BYTES
        or parsed_xid != xid
        or client_hardware[:ETHERNET_ADDRESS_BYTES] != mac
        or any(client_hardware[ETHERNET_ADDRESS_BYTES:])
        or data[DHCP_FIXED_HEADER.size : DHCP_FIXED_HEADER.size + 4]
        != DHCP_MAGIC_COOKIE
    ):
        raise GuestError("dhcp")
    options = _parse_dhcp_options(data, mac)
    message = options.get(DHCP_OPTION_MESSAGE_TYPE)
    if message is None:
        raise GuestError("dhcp")
    try:
        address = ipaddress.IPv4Address(offered_address)
    except ipaddress.AddressValueError as error:
        raise GuestError("dhcp") from error
    return ParsedDhcpReply(message[0], address, options)


def _option_ipv4(options: dict[int, bytes], code: int) -> ipaddress.IPv4Address:
    value = options.get(code)
    if value is None or len(value) < 4:
        raise GuestError("dhcp")
    try:
        return ipaddress.IPv4Address(value[:4])
    except ipaddress.AddressValueError as error:
        raise GuestError("dhcp") from error


def _prefix_length(mask: ipaddress.IPv4Address) -> int:
    raw = int(mask)
    inverted = raw ^ 0xFFFF_FFFF
    if raw == 0 or inverted & (inverted + 1):
        raise GuestError("dhcp")
    return bin(raw).count("1")


def lease_from_offer(reply: ParsedDhcpReply) -> DhcpLease:
    if (
        not isinstance(reply, ParsedDhcpReply)
        or reply.message_type != DHCP_OFFER
        or not _valid_endpoint(reply.offered_address)
    ):
        raise GuestError("dhcp")
    mask = _option_ipv4(reply.options, DHCP_OPTION_SUBNET_MASK)
    prefix = _prefix_length(mask)
    router = _option_ipv4(reply.options, DHCP_OPTION_ROUTER)
    server = _option_ipv4(reply.options, DHCP_OPTION_SERVER_ID)
    lease_raw = reply.options.get(DHCP_OPTION_LEASE_TIME)
    if lease_raw is None or len(lease_raw) != 4:
        raise GuestError("dhcp")
    lease_seconds = struct.unpack("!I", lease_raw)[0]
    if (
        lease_seconds == 0
        or not _valid_endpoint(router)
        or not _valid_endpoint(server)
        or router == reply.offered_address
    ):
        raise GuestError("dhcp")
    network = ipaddress.IPv4Network((reply.offered_address, prefix), strict=False)
    if router not in network:
        raise GuestError("dhcp")
    if prefix <= 30 and (
        reply.offered_address in (network.network_address, network.broadcast_address)
        or router in (network.network_address, network.broadcast_address)
    ):
        raise GuestError("dhcp")
    return DhcpLease(
        reply.offered_address,
        prefix,
        mask,
        router,
        server,
        lease_seconds,
    )


def validate_ack(reply: ParsedDhcpReply, offer: DhcpLease) -> DhcpLease:
    if (
        not isinstance(reply, ParsedDhcpReply)
        or not isinstance(offer, DhcpLease)
        or reply.message_type == DHCP_NAK
        or reply.message_type != DHCP_ACK
    ):
        raise GuestError("dhcp")
    acknowledged = lease_from_offer(
        ParsedDhcpReply(DHCP_OFFER, reply.offered_address, reply.options)
    )
    if acknowledged != offer:
        raise GuestError("dhcp")
    return acknowledged


def _receive_matching_reply(
    transport: DhcpTransport,
    xid: int,
    mac: bytes,
    deadline: float,
    clock: Callable[[], float],
) -> Optional[ParsedDhcpReply]:
    for _packet in range(DHCP_PACKET_LIMIT):
        remaining = deadline - clock()
        if remaining <= 0:
            return None
        try:
            data = transport.receive(remaining)
        except TimeoutError:
            return None
        except OSError as error:
            raise GuestError("dhcp") from error
        reply = parse_dhcp_reply(data, xid, mac)
        if reply is not None:
            return reply
    raise GuestError("dhcp")


def acquire_lease(
    transport: DhcpTransport,
    mac: bytes,
    xid: int,
    deadline: float,
    *,
    clock: Callable[[], float] = time.monotonic,
) -> DhcpLease:
    _validate_transaction(xid, mac)
    current = clock()
    if (
        isinstance(deadline, bool)
        or not isinstance(deadline, (int, float))
        or not math.isfinite(deadline)
        or isinstance(current, bool)
        or not isinstance(current, (int, float))
        or not math.isfinite(current)
        or deadline <= current
    ):
        raise GuestError("dhcp")
    discover = encode_dhcp_discover(xid, mac)
    for _attempt in range(DHCP_ATTEMPTS):
        if clock() >= deadline:
            break
        try:
            transport.send(discover)
        except OSError as error:
            raise GuestError("dhcp") from error
        offer_deadline = min(deadline, clock() + DHCP_ATTEMPT_SECONDS)
        reply = _receive_matching_reply(
            transport, xid, mac, offer_deadline, clock
        )
        if reply is None:
            continue
        offer = lease_from_offer(reply)
        request = encode_dhcp_request(xid, mac, offer)
        try:
            transport.send(request)
        except OSError as error:
            raise GuestError("dhcp") from error
        ack_deadline = min(deadline, clock() + DHCP_ATTEMPT_SECONDS)
        reply = _receive_matching_reply(transport, xid, mac, ack_deadline, clock)
        if reply is None:
            continue
        return validate_ack(reply, offer)
    raise GuestError("dhcp")


class SocketDhcpTransport:
    """Linux interface-bound DHCPv4 datagram transport."""

    def __init__(
        self,
        interface: GuestInterface,
        *,
        socket_factory: Callable[..., socket.socket] = socket.socket,
    ) -> None:
        if (
            not isinstance(interface, GuestInterface)
            or not isinstance(interface.name, str)
            or INTERFACE_RE.fullmatch(interface.name) is None
            or not isinstance(interface.mac, bytes)
            or len(interface.mac) != ETHERNET_ADDRESS_BYTES
            or not any(interface.mac)
            or interface.mac[0] & 1
        ):
            raise GuestError("dhcp")
        self._socket: Optional[socket.socket] = None
        try:
            connection = socket_factory(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
            self._socket = connection
            connection.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
            connection.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            connection.setsockopt(
                socket.SOL_SOCKET,
                getattr(socket, "SO_BINDTODEVICE", 25),
                interface.name.encode("ascii") + b"\0",
            )
            connection.bind(("0.0.0.0", DHCP_CLIENT_PORT))
        except (AttributeError, OSError, TypeError, UnicodeEncodeError, ValueError) as error:
            self.close()
            raise GuestError("dhcp") from error

    def send(self, packet: bytes) -> None:
        connection = self._socket
        if (
            connection is None
            or not isinstance(packet, bytes)
            or not 1 <= len(packet) <= DHCP_MAX_PACKET_BYTES
        ):
            raise GuestError("dhcp")
        try:
            count = connection.sendto(
                packet, ("255.255.255.255", DHCP_SERVER_PORT)
            )
        except (OSError, TypeError, ValueError) as error:
            raise GuestError("dhcp") from error
        if isinstance(count, bool) or not isinstance(count, int) or count != len(packet):
            raise GuestError("dhcp")

    def receive(self, timeout: float) -> bytes:
        connection = self._socket
        if connection is None:
            raise GuestError("dhcp")
        if isinstance(timeout, bool) or not isinstance(timeout, (int, float)):
            raise GuestError("dhcp")
        if not math.isfinite(timeout):
            raise GuestError("dhcp")
        if timeout <= 0:
            raise TimeoutError
        try:
            connection.settimeout(timeout)
            data, peer = connection.recvfrom(DHCP_MAX_PACKET_BYTES + 1)
        except socket.timeout as error:
            raise TimeoutError from error
        except (OSError, TypeError, ValueError) as error:
            raise GuestError("dhcp") from error
        if (
            not isinstance(data, bytes)
            or len(data) > DHCP_MAX_PACKET_BYTES
            or not isinstance(peer, tuple)
            or len(peer) < 2
            or isinstance(peer[1], bool)
            or not isinstance(peer[1], int)
            or peer[1] != DHCP_SERVER_PORT
        ):
            raise GuestError("dhcp")
        return data

    def close(self) -> None:
        connection = self._socket
        self._socket = None
        if connection is not None:
            try:
                connection.close()
            except (OSError, ValueError) as error:
                raise GuestError("dhcp") from error

    def __enter__(self) -> "SocketDhcpTransport":
        return self

    def __exit__(self, *_exception: object) -> None:
        self.close()


def secure_xid(random_bytes: Callable[[int], bytes] = os.urandom) -> int:
    for _attempt in range(16):
        try:
            value = random_bytes(4)
        except OSError as error:
            raise GuestError("dhcp") from error
        if not isinstance(value, bytes) or len(value) != 4:
            raise GuestError("dhcp")
        xid = int.from_bytes(value, "big")
        if xid != 0:
            return xid
    raise GuestError("dhcp")


def _run_ip(
    arguments: Sequence[str],
    runner: Callable[..., subprocess.CompletedProcess[bytes]],
) -> None:
    command = (IP_EXECUTABLE, *arguments)
    try:
        result = runner(
            command,
            cwd="/",
            env=dict(MINIMAL_ENVIRONMENT),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=COMMAND_TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise GuestError("configure") from error
    if result.returncode != 0:
        raise GuestError("configure")


class NetworkConfigurator:
    def __init__(
        self,
        interface: GuestInterface,
        *,
        runner: Callable[..., subprocess.CompletedProcess[bytes]] = subprocess.run,
    ) -> None:
        if (
            not isinstance(interface, GuestInterface)
            or not isinstance(interface.name, str)
            or INTERFACE_RE.fullmatch(interface.name) is None
            or not isinstance(interface.mac, bytes)
            or len(interface.mac) != ETHERNET_ADDRESS_BYTES
            or not any(interface.mac)
            or interface.mac[0] & 1
        ):
            raise GuestError("interface")
        self._interface = interface
        self._runner = runner
        self._link_touched = False
        self._address_touched = False
        self._route_touched = False
        self._lease: Optional[DhcpLease] = None

    def bring_up(self) -> None:
        self._link_touched = True
        _run_ip(
            ("link", "set", "dev", self._interface.name, "up"), self._runner
        )

    def apply(self, lease: DhcpLease) -> None:
        self._lease = lease
        address = f"{lease.address}/{lease.prefix_length}"
        self._address_touched = True
        _run_ip(
            ("address", "replace", address, "dev", self._interface.name),
            self._runner,
        )
        self._route_touched = True
        _run_ip(
            (
                "route",
                "replace",
                "default",
                "via",
                str(lease.router),
                "dev",
                self._interface.name,
            ),
            self._runner,
        )

    def cleanup(self) -> None:
        failures = False
        lease = self._lease
        commands: list[tuple[str, ...]] = []
        if self._route_touched and lease is not None:
            commands.append(
                (
                    "route",
                    "del",
                    "default",
                    "via",
                    str(lease.router),
                    "dev",
                    self._interface.name,
                )
            )
        if self._address_touched and lease is not None:
            commands.append(
                (
                    "address",
                    "del",
                    f"{lease.address}/{lease.prefix_length}",
                    "dev",
                    self._interface.name,
                )
            )
        if self._link_touched:
            commands.append(("link", "set", "dev", self._interface.name, "down"))
        for command in commands:
            try:
                _run_ip(command, self._runner)
            except GuestError:
                failures = True
        self._route_touched = False
        self._address_touched = False
        self._link_touched = False
        if failures:
            raise GuestError("cleanup")


class TcpSocket(Protocol):
    def settimeout(self, value: float) -> None: ...

    def send(self, data: memoryview) -> int: ...

    def shutdown(self, how: int) -> None: ...

    def recv(self, size: int) -> bytes: ...

    def close(self) -> None: ...


def _tcp_remaining(deadline: float, clock: Callable[[], float]) -> float:
    if (
        isinstance(deadline, bool)
        or not isinstance(deadline, (int, float))
        or not math.isfinite(deadline)
    ):
        raise GuestError("tcp")
    current = clock()
    if (
        isinstance(current, bool)
        or not isinstance(current, (int, float))
        or not math.isfinite(current)
    ):
        raise GuestError("tcp")
    remaining = deadline - current
    if not math.isfinite(remaining) or remaining <= 0:
        raise GuestError("tcp")
    return remaining


def _set_tcp_timeout(
    connection: TcpSocket, deadline: float, clock: Callable[[], float]
) -> None:
    try:
        connection.settimeout(_tcp_remaining(deadline, clock))
    except (OSError, ValueError) as error:
        raise GuestError("tcp") from error


def tcp_exchange(
    control: GuestControl,
    deadline: float,
    *,
    connector: Callable[..., TcpSocket] = socket.create_connection,
    clock: Callable[[], float] = time.monotonic,
) -> None:
    if (
        not isinstance(control, GuestControl)
        or control.mode not in CONTROL_MODES.values()
        or not isinstance(control.endpoint_ipv4, str)
        or isinstance(control.endpoint_port, bool)
        or not isinstance(control.endpoint_port, int)
        or not 1 <= control.endpoint_port <= 65535
        or not isinstance(control.nonce, bytes)
        or len(control.nonce) != 32
        or not any(control.nonce)
    ):
        raise GuestError("tcp")
    try:
        endpoint = ipaddress.IPv4Address(control.endpoint_ipv4)
    except ipaddress.AddressValueError as error:
        raise GuestError("tcp") from error
    if not _valid_endpoint(endpoint) or str(endpoint) != control.endpoint_ipv4:
        raise GuestError("tcp")
    connection: Optional[TcpSocket] = None
    try:
        try:
            connection = connector(
                (control.endpoint_ipv4, control.endpoint_port),
                timeout=_tcp_remaining(deadline, clock),
            )
        except (OSError, ValueError) as error:
            raise GuestError("tcp") from error
        request = tcp_request(control.nonce)
        view = memoryview(request)
        while view:
            _set_tcp_timeout(connection, deadline, clock)
            try:
                count = connection.send(view)
            except (OSError, ValueError) as error:
                raise GuestError("tcp") from error
            if count <= 0 or count > len(view):
                raise GuestError("tcp")
            view = view[count:]
        try:
            connection.shutdown(socket.SHUT_WR)
        except (OSError, ValueError) as error:
            raise GuestError("tcp") from error
        response = bytearray()
        while len(response) < TCP_RECORD_BYTES:
            _set_tcp_timeout(connection, deadline, clock)
            try:
                chunk = connection.recv(TCP_RECORD_BYTES - len(response))
            except (OSError, ValueError) as error:
                raise GuestError("tcp") from error
            if not chunk:
                raise GuestError("tcp")
            response.extend(chunk)
        _set_tcp_timeout(connection, deadline, clock)
        try:
            trailing = connection.recv(1)
        except (OSError, ValueError) as error:
            raise GuestError("tcp") from error
        if trailing or bytes(response) != tcp_response(control.nonce):
            raise GuestError("tcp")
    finally:
        if connection is not None:
            try:
                connection.close()
            except (OSError, ValueError) as error:
                raise GuestError("tcp") from error


def _default_transport_factory(interface: GuestInterface) -> SocketDhcpTransport:
    return SocketDhcpTransport(interface)


def run_certification(
    *,
    control_reader: Callable[[], GuestControl] = read_guest_control,
    interface_discoverer: Callable[[], GuestInterface] = discover_interface,
    transport_factory: Callable[[GuestInterface], DhcpTransport] = (
        _default_transport_factory
    ),
    random_bytes: Callable[[int], bytes] = os.urandom,
    command_runner: Callable[..., subprocess.CompletedProcess[bytes]] = subprocess.run,
    connector: Callable[..., TcpSocket] = socket.create_connection,
    clock: Callable[[], float] = time.monotonic,
) -> None:
    network: Optional[NetworkConfigurator] = None
    primary_error: Optional[GuestError] = None
    try:
        control = control_reader()
        interface = interface_discoverer()
        network = NetworkConfigurator(interface, runner=command_runner)
        network.bring_up()
        transport = transport_factory(interface)
        try:
            xid = secure_xid(random_bytes)
            lease = acquire_lease(
                transport,
                interface.mac,
                xid,
                clock() + DHCP_DEADLINE_SECONDS,
                clock=clock,
            )
        finally:
            close = getattr(transport, "close", None)
            if close is None:
                raise GuestError("dhcp")
            close()
        network.apply(lease)
        tcp_exchange(
            control,
            clock() + TCP_DEADLINE_SECONDS,
            connector=connector,
            clock=clock,
        )
    except GuestError as error:
        primary_error = error
    except BaseException:
        primary_error = GuestError("internal")

    if network is not None:
        try:
            network.cleanup()
        except GuestError as error:
            raise error
    if primary_error is not None:
        raise primary_error


def _emit_marker(marker: str, stream: object) -> None:
    try:
        stream.write(marker + "\n")
        stream.flush()
    except (AttributeError, OSError, TypeError, ValueError) as error:
        raise GuestError("internal") from error


def main(
    argv: Optional[Sequence[str]] = None,
    *,
    certification: Callable[[], None] = run_certification,
    stream: object = sys.stdout,
) -> int:
    arguments = tuple(sys.argv[1:] if argv is None else argv)
    try:
        _emit_marker(BEGIN_MARKER, stream)
        if arguments:
            raise GuestError("internal")
        certification()
    except GuestError as error:
        try:
            _emit_marker(FAILURE_PREFIX + error.phase.upper(), stream)
        except GuestError:
            pass
        return 3
    except BaseException:
        try:
            _emit_marker(FAILURE_PREFIX + "INTERNAL", stream)
        except GuestError:
            pass
        return 3
    try:
        _emit_marker(SUCCESS_MARKER, stream)
    except GuestError:
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
