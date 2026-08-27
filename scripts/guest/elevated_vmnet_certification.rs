#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), no_std)]

//! Static aarch64 Linux guest oracle for elevated shared-vmnet evidence.

use core::ptr;

#[cfg(target_arch = "aarch64")]
use core::arch::asm;

#[cfg(not(test))]
use core::panic::PanicInfo;

const STDOUT: usize = 1;
const AT_FDCWD: usize = (-100_isize) as usize;
const O_RDONLY: usize = 0;
const O_RDWR: usize = 2;
const O_DIRECTORY: usize = 0x4000;
const O_CLOEXEC: usize = 0x8_0000;

const SYS_DUP3: usize = 24;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_GETDENTS64: usize = 61;
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_PPOLL: usize = 73;
const SYS_NANOSLEEP: usize = 101;
const SYS_CLOCK_GETTIME: usize = 113;
const SYS_KILL: usize = 129;
const SYS_SOCKET: usize = 198;
const SYS_BIND: usize = 200;
const SYS_CONNECT: usize = 203;
const SYS_SENDTO: usize = 206;
const SYS_RECVFROM: usize = 207;
const SYS_SETSOCKOPT: usize = 208;
const SYS_SHUTDOWN: usize = 210;
const SYS_CLONE: usize = 220;
const SYS_EXECVE: usize = 221;
const SYS_EXIT: usize = 93;
const SYS_WAIT4: usize = 260;

const EINTR: isize = 4;
const EAGAIN: isize = 11;
const SIGCHLD: usize = 17;
const SIGKILL: usize = 9;
const WNOHANG: usize = 1;
const CLOCK_MONOTONIC: usize = 1;

const AF_INET: usize = 2;
const SOCK_STREAM: usize = 1;
const SOCK_DGRAM: usize = 2;
const SOCK_CLOEXEC: usize = 0x8_0000;
const SOL_SOCKET: usize = 1;
const SO_REUSEADDR: usize = 2;
const SO_BROADCAST: usize = 6;
const SO_RCVTIMEO: usize = 20;
const SO_SNDTIMEO: usize = 21;
const SO_BINDTODEVICE: usize = 25;
const SHUT_WR: usize = 1;
const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;
const POLLHUP: i16 = 16;

const BEGIN_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_BEGIN\n";
const SUCCESS_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_OK\n";
const FAILURE_PREFIX: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_";

const CONTROL_PATH: &[u8] = b"/dev/vdb\0";
const NET_DIRECTORY: &[u8] = b"/sys/class/net\0";
const DEV_NULL: &[u8] = b"/dev/null\0";
const IP_PATH: &[u8] = b"/usr/bin/ip\0";

const CONTROL_BYTES: usize = 512;
const CONTROL_PREFIX_BYTES: usize = 64;
const CONTROL_DIGEST_BYTES: usize = 32;
const CONTROL_MAGIC: &[u8; 8] = b"BBEVNET2";
const CONTROL_VERSION: u16 = 2;
const CONTROL_SHARED_MODE: u8 = 1;
const CONTROL_DHCP_ROUTER_ENDPOINT: u8 = 1;

const TCP_REQUEST_MAGIC: &[u8; 8] = b"BBVREQ1\0";
const TCP_RESPONSE_MAGIC: &[u8; 8] = b"BBVRES1\0";
const TCP_RECORD_BYTES: usize = 40;

const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_MAGIC_COOKIE: &[u8; 4] = b"\x63\x82\x53\x63";
const DHCP_FIXED_HEADER_BYTES: usize = 236;
const DHCP_MIN_PACKET_BYTES: usize = 300;
const DHCP_MAX_PACKET_BYTES: usize = 576;
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const DHCP_NAK: u8 = 6;
const DHCP_ATTEMPTS: usize = 3;
const DHCP_PACKET_LIMIT: usize = 64;
const DHCP_WINDOW_NS: i64 = 5_000_000_000;
const INTERFACE_TIMEOUT_NS: i64 = 10_000_000_000;
const INTERFACE_POLL_NS: i64 = 20_000_000;
const TCP_TIMEOUT_NS: i64 = 10_000_000_000;
const COMMAND_TIMEOUT_NS: i64 = 5_000_000_000;
const SERIAL_WRITE_BYTES: usize = 16;

const DHCP_OPTION_SUBNET_MASK: u8 = 1;
const DHCP_OPTION_ROUTER: u8 = 3;
const DHCP_OPTION_REQUESTED_ADDRESS: u8 = 50;
const DHCP_OPTION_LEASE_TIME: u8 = 51;
const DHCP_OPTION_OVERLOAD: u8 = 52;
const DHCP_OPTION_MESSAGE_TYPE: u8 = 53;
const DHCP_OPTION_SERVER_ID: u8 = 54;
const DHCP_OPTION_PARAMETER_REQUEST: u8 = 55;
const DHCP_OPTION_CLIENT_ID: u8 = 61;
const DHCP_OPTION_PAD: u8 = 0;
const DHCP_OPTION_END: u8 = 255;

#[repr(C)]
#[derive(Clone, Copy)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Timeval {
    seconds: i64,
    microseconds: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PollFd {
    descriptor: i32,
    events: i16,
    returned: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrIn {
    family: u16,
    port: u16,
    address: u32,
    zero: [u8; 8],
}

const _: [(); 16] = [(); core::mem::size_of::<SockAddrIn>()];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Control,
    Interface,
    Dhcp,
    Configure,
    Tcp,
    Cleanup,
    Internal,
}

impl Phase {
    const fn marker(self) -> &'static [u8] {
        match self {
            Self::Control => b"CONTROL\n",
            Self::Interface => b"INTERFACE\n",
            Self::Dhcp => b"DHCP\n",
            Self::Configure => b"CONFIGURE\n",
            Self::Tcp => b"TCP\n",
            Self::Cleanup => b"CLEANUP\n",
            Self::Internal => b"INTERNAL\n",
        }
    }
}

type GuestResult<T> = Result<T, Phase>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Control {
    port: u16,
    nonce: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterfaceName {
    bytes: [u8; 16],
    length: u8,
}

impl InterfaceName {
    fn parse(value: &[u8]) -> Option<Self> {
        if value.is_empty()
            || value.len() >= 16
            || value
                .iter()
                .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(*byte, b'_' | b'-' | b'.'))
        {
            return None;
        }
        let mut bytes = [0_u8; 16];
        bytes[..value.len()].copy_from_slice(value);
        Some(Self {
            bytes,
            length: value.len() as u8,
        })
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }

    fn as_c_bytes(&self) -> &[u8] {
        &self.bytes[..=usize::from(self.length)]
    }

    fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interface {
    name: InterfaceName,
    mac: [u8; 6],
}

#[derive(Clone, Copy)]
enum InterfaceProbeError {
    DirectoryOpen,
    DirectoryRead,
    DirectoryRecord,
    DirectoryClose,
    Topology,
    Address,
    Mac,
}

impl InterfaceProbeError {
    const fn marker(self) -> &'static [u8] {
        match self {
            Self::DirectoryOpen => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_DIRECTORY_OPEN\n",
            Self::DirectoryRead => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_DIRECTORY_READ\n",
            Self::DirectoryRecord => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_DIRECTORY_RECORD\n",
            Self::DirectoryClose => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_DIRECTORY_CLOSE\n",
            Self::Topology => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_TOPOLOGY\n",
            Self::Address => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_ADDRESS\n",
            Self::Mac => b"BANGBANG_ELEVATED_VMNET_INTERFACE_PROBE_MAC\n",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lease {
    address: [u8; 4],
    prefix: u8,
    mask: [u8; 4],
    router: [u8; 4],
    server: [u8; 4],
    seconds: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ParsedOptions {
    mask: Option<[u8; 4]>,
    router: Option<[u8; 4]>,
    server: Option<[u8; 4]>,
    lease_seconds: Option<u32>,
    message: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedReply {
    message: u8,
    offered: [u8; 4],
    options: ParsedOptions,
}

struct Packet {
    bytes: [u8; DHCP_MAX_PACKET_BYTES],
    length: usize,
}

impl Packet {
    const fn new() -> Self {
        Self {
            bytes: [0; DHCP_MAX_PACKET_BYTES],
            length: 0,
        }
    }

    fn append(&mut self, value: &[u8]) -> bool {
        let Some(end) = self.length.checked_add(value.len()) else {
            return false;
        };
        let Some(destination) = self.bytes.get_mut(self.length..end) else {
            return false;
        };
        destination.copy_from_slice(value);
        self.length = end;
        true
    }

    fn push(&mut self, value: u8) -> bool {
        self.append(&[value])
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

struct CBuffer {
    bytes: [u8; 64],
    length: usize,
}

impl CBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; 64],
            length: 0,
        }
    }

    fn append(&mut self, value: &[u8]) -> bool {
        value.iter().copied().all(|byte| self.append_byte(byte))
    }

    fn append_byte(&mut self, value: u8) -> bool {
        if value == 0 || self.length + 1 >= self.bytes.len() {
            return false;
        }
        self.bytes[self.length] = value;
        self.length += 1;
        true
    }

    fn append_decimal(&mut self, value: u8) -> bool {
        let hundreds = value / 100;
        let tens = (value % 100) / 10;
        let ones = value % 10;
        (hundreds == 0 || self.append_byte(b'0' + hundreds))
            && (hundreds == 0 && tens == 0 || self.append_byte(b'0' + tens))
            && self.append_byte(b'0' + ones)
    }

    fn append_ipv4(&mut self, value: [u8; 4]) -> bool {
        for (index, octet) in value.into_iter().enumerate() {
            if index != 0 && !self.append_byte(b'.') {
                return false;
            }
            if !self.append_decimal(octet) {
                return false;
            }
        }
        true
    }

    fn terminate(&mut self) -> bool {
        if self.length >= self.bytes.len() {
            return false;
        }
        self.bytes[self.length] = 0;
        true
    }

    fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    fn as_c_bytes(&self) -> &[u8] {
        &self.bytes[..=self.length]
    }
}

struct NetworkState {
    interface: InterfaceName,
    link: bool,
    address: bool,
    route: bool,
    lease: Option<Lease>,
}

impl NetworkState {
    const fn new(interface: InterfaceName) -> Self {
        Self {
            interface,
            link: false,
            address: false,
            route: false,
            lease: None,
        }
    }

    fn bring_up(&mut self) -> GuestResult<()> {
        self.link = true;
        run_ip(&[
            b"link\0".as_ptr(),
            b"set\0".as_ptr(),
            b"dev\0".as_ptr(),
            self.interface.as_ptr(),
            b"up\0".as_ptr(),
        ])
    }

    fn apply(&mut self, lease: Lease) -> GuestResult<()> {
        let address = address_prefix(lease.address, lease.prefix).ok_or(Phase::Configure)?;
        self.lease = Some(lease);
        self.address = true;
        run_ip(&[
            b"address\0".as_ptr(),
            b"replace\0".as_ptr(),
            address.as_ptr(),
            b"dev\0".as_ptr(),
            self.interface.as_ptr(),
        ])?;
        let router = ipv4_c_buffer(lease.router).ok_or(Phase::Configure)?;
        self.route = true;
        run_ip(&[
            b"route\0".as_ptr(),
            b"replace\0".as_ptr(),
            b"default\0".as_ptr(),
            b"via\0".as_ptr(),
            router.as_ptr(),
            b"dev\0".as_ptr(),
            self.interface.as_ptr(),
        ])
    }

    fn cleanup(&mut self) -> GuestResult<()> {
        let mut failed = false;
        if self.route {
            if let Some(lease) = self.lease {
                if let Some(router) = ipv4_c_buffer(lease.router) {
                    failed |= run_ip(&[
                        b"route\0".as_ptr(),
                        b"del\0".as_ptr(),
                        b"default\0".as_ptr(),
                        b"via\0".as_ptr(),
                        router.as_ptr(),
                        b"dev\0".as_ptr(),
                        self.interface.as_ptr(),
                    ])
                    .is_err();
                } else {
                    failed = true;
                }
            } else {
                failed = true;
            }
        }
        if self.address {
            if let Some(lease) = self.lease {
                if let Some(address) = address_prefix(lease.address, lease.prefix) {
                    failed |= run_ip(&[
                        b"address\0".as_ptr(),
                        b"del\0".as_ptr(),
                        address.as_ptr(),
                        b"dev\0".as_ptr(),
                        self.interface.as_ptr(),
                    ])
                    .is_err();
                } else {
                    failed = true;
                }
            } else {
                failed = true;
            }
        }
        if self.link {
            failed |= run_ip(&[
                b"link\0".as_ptr(),
                b"set\0".as_ptr(),
                b"dev\0".as_ptr(),
                self.interface.as_ptr(),
                b"down\0".as_ptr(),
            ])
            .is_err();
        }
        self.route = false;
        self.address = false;
        self.link = false;
        if failed { Err(Phase::Cleanup) } else { Ok(()) }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    let _ = write_all(STDOUT, FAILURE_PREFIX);
    let _ = write_all(STDOUT, Phase::Internal.marker());
    exit(101)
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    if !write_all(STDOUT, BEGIN_MARKER) {
        exit(3);
    }
    match run() {
        Ok(()) => {
            if !write_all(STDOUT, SUCCESS_MARKER) {
                exit(3);
            }
            exit(0)
        }
        Err(phase) => {
            let _ = write_all(STDOUT, FAILURE_PREFIX);
            let _ = write_all(STDOUT, phase.marker());
            exit(3)
        }
    }
}

fn run() -> GuestResult<()> {
    let control = read_control()?;
    let interface = discover_interface()?;
    let mut network = NetworkState::new(interface.name);
    let primary = (|| {
        network.bring_up()?;
        let socket = dhcp_socket(&interface.name)?;
        let result = acquire_lease(socket, transaction_id(&control.nonce), &interface.mac);
        let close_result = close(socket);
        let lease = result?;
        if close_result != 0 {
            return Err(Phase::Dhcp);
        }
        network.apply(lease)?;
        tcp_exchange(lease.router, control.port, &control.nonce)
    })();
    let cleanup = network.cleanup();
    if cleanup.is_err() {
        return Err(Phase::Cleanup);
    }
    primary
}

fn read_control() -> GuestResult<Control> {
    let descriptor = open(CONTROL_PATH, O_RDONLY | O_CLOEXEC).ok_or(Phase::Control)?;
    let mut data = [0_u8; CONTROL_BYTES];
    let result = read_exact(descriptor, &mut data);
    let close_result = close(descriptor);
    if !result || close_result != 0 {
        return Err(Phase::Control);
    }
    decode_control(&data)
}

fn decode_control(data: &[u8]) -> GuestResult<Control> {
    if data.len() != CONTROL_BYTES
        || data.get(..8) != Some(CONTROL_MAGIC)
        || data.get(8..10) != Some(&CONTROL_VERSION.to_be_bytes())
        || data.get(10) != Some(&CONTROL_SHARED_MODE)
        || data.get(11) != Some(&CONTROL_DHCP_ROUTER_ENDPOINT)
        || data
            .get(12..16)
            .is_none_or(|value| value.iter().any(|byte| *byte != 0))
        || data
            .get(50..64)
            .is_none_or(|value| value.iter().any(|byte| *byte != 0))
        || data
            .get(96..)
            .is_none_or(|value| value.iter().any(|byte| *byte != 0))
    {
        return Err(Phase::Control);
    }
    let port = u16::from_be_bytes(
        data.get(16..18)
            .and_then(|value| value.try_into().ok())
            .ok_or(Phase::Control)?,
    );
    let nonce: [u8; 32] = data
        .get(18..50)
        .and_then(|value| value.try_into().ok())
        .ok_or(Phase::Control)?;
    let expected =
        sha256(data.get(..CONTROL_PREFIX_BYTES).ok_or(Phase::Control)?).ok_or(Phase::Control)?;
    if port == 0
        || nonce.iter().all(|byte| *byte == 0)
        || data.get(CONTROL_PREFIX_BYTES..CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES)
            != Some(expected.as_slice())
    {
        return Err(Phase::Control);
    }
    Ok(Control { port, nonce })
}

fn discover_interface() -> GuestResult<Interface> {
    let deadline = monotonic_ns().ok_or(Phase::Interface)? + INTERFACE_TIMEOUT_NS;
    loop {
        let error = match discover_interface_once() {
            Ok(interface) => return Ok(interface),
            Err(error) => error,
        };
        if monotonic_ns().is_none_or(|now| now >= deadline) {
            let _ = write_all(STDOUT, error.marker());
            return Err(Phase::Interface);
        }
        sleep_ns(INTERFACE_POLL_NS);
    }
}

fn discover_interface_once() -> Result<Interface, InterfaceProbeError> {
    let directory = open(NET_DIRECTORY, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        .ok_or(InterfaceProbeError::DirectoryOpen)?;
    let candidate = (|| {
        let mut buffer = [0_u8; 4096];
        let mut candidate = None;
        loop {
            let count = getdents(directory, &mut buffer);
            if count < 0 {
                return Err(InterfaceProbeError::DirectoryRead);
            }
            if count == 0 {
                break;
            }
            let limit = usize::try_from(count).map_err(|_| InterfaceProbeError::DirectoryRead)?;
            let mut offset = 0_usize;
            while offset < limit {
                let record = buffer
                    .get(offset..limit)
                    .ok_or(InterfaceProbeError::DirectoryRecord)?;
                let length_bytes: [u8; 2] = record
                    .get(16..18)
                    .and_then(|value| value.try_into().ok())
                    .ok_or(InterfaceProbeError::DirectoryRecord)?;
                let length = usize::from(u16::from_ne_bytes(length_bytes));
                if length < 20 || offset.checked_add(length).is_none_or(|end| end > limit) {
                    return Err(InterfaceProbeError::DirectoryRecord);
                }
                let name_field = record
                    .get(19..length)
                    .ok_or(InterfaceProbeError::DirectoryRecord)?;
                let name_end = name_field
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or(InterfaceProbeError::DirectoryRecord)?;
                let name = &name_field[..name_end];
                if name != b"." && name != b".." && name != b"lo" {
                    let name = InterfaceName::parse(name).ok_or(InterfaceProbeError::Topology)?;
                    if candidate.replace(name).is_some() {
                        return Err(InterfaceProbeError::Topology);
                    }
                }
                offset += length;
            }
        }
        candidate.ok_or(InterfaceProbeError::Topology)
    })();
    let close_result = close(directory);
    let name = candidate?;
    if close_result != 0 {
        return Err(InterfaceProbeError::DirectoryClose);
    }
    let mut address_path = CBuffer::new();
    if !address_path.append(b"/sys/class/net/")
        || !address_path.append(name.as_bytes())
        || !address_path.append(b"/address")
        || !address_path.terminate()
    {
        return Err(InterfaceProbeError::Address);
    }
    let descriptor = open(address_path.as_c_bytes(), O_RDONLY | O_CLOEXEC)
        .ok_or(InterfaceProbeError::Address)?;
    let mut raw = [0_u8; 32];
    let length = read_some(descriptor, &mut raw);
    let close_result = close(descriptor);
    if length < 17 || close_result != 0 {
        return Err(InterfaceProbeError::Address);
    }
    let length = usize::try_from(length).map_err(|_| InterfaceProbeError::Address)?;
    let mac = parse_mac(raw.get(..length).ok_or(InterfaceProbeError::Address)?)
        .map_err(|_| InterfaceProbeError::Mac)?;
    Ok(Interface { name, mac })
}

fn parse_mac(value: &[u8]) -> GuestResult<[u8; 6]> {
    let value = if value.last() == Some(&b'\n') {
        &value[..value.len() - 1]
    } else {
        value
    };
    if value.len() != 17 {
        return Err(Phase::Interface);
    }
    let mut mac = [0_u8; 6];
    for index in 0..6 {
        let start = index * 3;
        if index != 5 && value.get(start + 2) != Some(&b':') {
            return Err(Phase::Interface);
        }
        let high = hex(*value.get(start).ok_or(Phase::Interface)?).ok_or(Phase::Interface)?;
        let low = hex(*value.get(start + 1).ok_or(Phase::Interface)?).ok_or(Phase::Interface)?;
        mac[index] = (high << 4) | low;
    }
    if mac.iter().all(|byte| *byte == 0) || mac[0] & 1 != 0 {
        return Err(Phase::Interface);
    }
    Ok(mac)
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn transaction_id(nonce: &[u8; 32]) -> u32 {
    let mut input = [0_u8; 43];
    input[..32].copy_from_slice(nonce);
    input[32..].copy_from_slice(b"dhcp-xid-v1");
    let digest = sha256(&input).unwrap_or([0; 32]);
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    if value == 0 { 1 } else { value }
}

fn dhcp_socket(interface: &InterfaceName) -> GuestResult<usize> {
    let descriptor = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC);
    if descriptor < 0 {
        return Err(Phase::Dhcp);
    }
    let descriptor = usize::try_from(descriptor).map_err(|_| Phase::Dhcp)?;
    let enabled = 1_i32;
    if !set_socket_option(descriptor, SO_BROADCAST, &enabled)
        || !set_socket_option(descriptor, SO_REUSEADDR, &enabled)
        || !set_socket_bytes(descriptor, SO_BINDTODEVICE, interface.as_c_bytes())
    {
        let _ = close(descriptor);
        return Err(Phase::Dhcp);
    }
    let local = socket_address([0, 0, 0, 0], DHCP_CLIENT_PORT);
    if bind(descriptor, &local) != 0 {
        let _ = close(descriptor);
        return Err(Phase::Dhcp);
    }
    Ok(descriptor)
}

fn acquire_lease(descriptor: usize, xid: u32, mac: &[u8; 6]) -> GuestResult<Lease> {
    let discover = encode_dhcp(xid, mac, DHCP_DISCOVER, None)?;
    let server = socket_address([255, 255, 255, 255], DHCP_SERVER_PORT);
    for _ in 0..DHCP_ATTEMPTS {
        if send_to(descriptor, discover.as_bytes(), &server) != discover.length as isize {
            return Err(Phase::Dhcp);
        }
        let Some(offer_reply) = receive_reply(descriptor, xid, mac, DHCP_WINDOW_NS)? else {
            continue;
        };
        let offer = lease_from_reply(offer_reply, DHCP_OFFER)?;
        let request = encode_dhcp(xid, mac, DHCP_REQUEST, Some(offer))?;
        if send_to(descriptor, request.as_bytes(), &server) != request.length as isize {
            return Err(Phase::Dhcp);
        }
        let Some(ack_reply) = receive_reply(descriptor, xid, mac, DHCP_WINDOW_NS)? else {
            continue;
        };
        if ack_reply.message == DHCP_NAK {
            return Err(Phase::Dhcp);
        }
        let acknowledged = lease_from_reply(ack_reply, DHCP_ACK)?;
        if acknowledged != offer {
            return Err(Phase::Dhcp);
        }
        return Ok(acknowledged);
    }
    Err(Phase::Dhcp)
}

fn encode_dhcp(xid: u32, mac: &[u8; 6], message: u8, lease: Option<Lease>) -> GuestResult<Packet> {
    if xid == 0 || mac.iter().all(|byte| *byte == 0) || mac[0] & 1 != 0 {
        return Err(Phase::Dhcp);
    }
    let mut header = [0_u8; DHCP_FIXED_HEADER_BYTES];
    header[0] = 1;
    header[1] = 1;
    header[2] = 6;
    header[4..8].copy_from_slice(&xid.to_be_bytes());
    header[10..12].copy_from_slice(&0x8000_u16.to_be_bytes());
    header[28..34].copy_from_slice(mac);
    let mut packet = Packet::new();
    if !packet.append(&header)
        || !packet.append(DHCP_MAGIC_COOKIE)
        || !append_option(&mut packet, DHCP_OPTION_MESSAGE_TYPE, &[message])
    {
        return Err(Phase::Dhcp);
    }
    if message == DHCP_REQUEST {
        let lease = lease.ok_or(Phase::Dhcp)?;
        if !append_option(&mut packet, DHCP_OPTION_REQUESTED_ADDRESS, &lease.address)
            || !append_option(&mut packet, DHCP_OPTION_SERVER_ID, &lease.server)
        {
            return Err(Phase::Dhcp);
        }
    } else if message != DHCP_DISCOVER || lease.is_some() {
        return Err(Phase::Dhcp);
    }
    let mut client = [0_u8; 7];
    client[0] = 1;
    client[1..].copy_from_slice(mac);
    if !append_option(&mut packet, DHCP_OPTION_CLIENT_ID, &client)
        || !append_option(
            &mut packet,
            DHCP_OPTION_PARAMETER_REQUEST,
            &[
                DHCP_OPTION_SUBNET_MASK,
                DHCP_OPTION_ROUTER,
                DHCP_OPTION_LEASE_TIME,
                DHCP_OPTION_SERVER_ID,
            ],
        )
        || !packet.push(DHCP_OPTION_END)
    {
        return Err(Phase::Dhcp);
    }
    while packet.length < DHCP_MIN_PACKET_BYTES {
        if !packet.push(0) {
            return Err(Phase::Dhcp);
        }
    }
    Ok(packet)
}

fn append_option(packet: &mut Packet, code: u8, payload: &[u8]) -> bool {
    code != 0
        && code != DHCP_OPTION_END
        && !payload.is_empty()
        && payload.len() <= 255
        && packet.push(code)
        && packet.push(payload.len() as u8)
        && packet.append(payload)
}

fn receive_reply(
    descriptor: usize,
    xid: u32,
    mac: &[u8; 6],
    window_ns: i64,
) -> GuestResult<Option<ParsedReply>> {
    let deadline = monotonic_ns().ok_or(Phase::Dhcp)? + window_ns;
    let mut data = [0_u8; DHCP_MAX_PACKET_BYTES + 1];
    for _ in 0..DHCP_PACKET_LIMIT {
        let Some(remaining) = remaining_timeout(deadline, monotonic_ns().ok_or(Phase::Dhcp)?)
        else {
            return Ok(None);
        };
        if !wait_ready(descriptor, POLLIN, remaining) {
            return Ok(None);
        }
        let mut peer = socket_address([0, 0, 0, 0], 0);
        let mut peer_length = core::mem::size_of::<SockAddrIn>() as u32;
        let length = recv_from(descriptor, &mut data, &mut peer, &mut peer_length);
        if length == -EINTR {
            continue;
        }
        if length == -EAGAIN {
            return Ok(None);
        }
        if length < 0
            || peer_length as usize != core::mem::size_of::<SockAddrIn>()
            || u16::from_be(peer.port) != DHCP_SERVER_PORT
        {
            return Err(Phase::Dhcp);
        }
        let length = usize::try_from(length).map_err(|_| Phase::Dhcp)?;
        if length > DHCP_MAX_PACKET_BYTES {
            return Err(Phase::Dhcp);
        }
        if let Some(reply) = parse_reply(&data[..length], xid, mac)? {
            return Ok(Some(reply));
        }
    }
    Err(Phase::Dhcp)
}

fn parse_reply(data: &[u8], xid: u32, mac: &[u8; 6]) -> GuestResult<Option<ParsedReply>> {
    if data.len() < 34 {
        return Ok(None);
    }
    if data.get(4..8) != Some(xid.to_be_bytes().as_slice()) || data.get(28..34) != Some(mac) {
        return Ok(None);
    }
    if !(DHCP_FIXED_HEADER_BYTES + 5..=DHCP_MAX_PACKET_BYTES).contains(&data.len())
        || data[0] != 2
        || data[1] != 1
        || data[2] != 6
        || data
            .get(34..44)
            .is_none_or(|value| value.iter().any(|byte| *byte != 0))
        || data.get(DHCP_FIXED_HEADER_BYTES..DHCP_FIXED_HEADER_BYTES + 4) != Some(DHCP_MAGIC_COOKIE)
    {
        return Err(Phase::Dhcp);
    }
    let offered: [u8; 4] = data
        .get(16..20)
        .and_then(|value| value.try_into().ok())
        .ok_or(Phase::Dhcp)?;
    let options = parse_options(&data[DHCP_FIXED_HEADER_BYTES + 4..], mac)?;
    let message = options.message.ok_or(Phase::Dhcp)?;
    Ok(Some(ParsedReply {
        message,
        offered,
        options,
    }))
}

fn parse_options(data: &[u8], mac: &[u8; 6]) -> GuestResult<ParsedOptions> {
    let mut options = ParsedOptions::default();
    let mut singleton = [false; 256];
    let mut offset = 0_usize;
    let mut ended = false;
    while offset < data.len() {
        let code = data[offset];
        offset += 1;
        if code == DHCP_OPTION_PAD {
            continue;
        }
        if code == DHCP_OPTION_END {
            ended = true;
            if data[offset..].iter().any(|byte| *byte != 0) {
                return Err(Phase::Dhcp);
            }
            break;
        }
        let length = usize::from(*data.get(offset).ok_or(Phase::Dhcp)?);
        offset += 1;
        let Some(end) = offset.checked_add(length) else {
            return Err(Phase::Dhcp);
        };
        if length == 0 || end > data.len() {
            return Err(Phase::Dhcp);
        }
        let payload = &data[offset..end];
        offset = end;
        if code == DHCP_OPTION_OVERLOAD {
            return Err(Phase::Dhcp);
        }
        let unique = matches!(
            code,
            DHCP_OPTION_SUBNET_MASK
                | DHCP_OPTION_ROUTER
                | DHCP_OPTION_REQUESTED_ADDRESS
                | DHCP_OPTION_LEASE_TIME
                | DHCP_OPTION_MESSAGE_TYPE
                | DHCP_OPTION_SERVER_ID
                | DHCP_OPTION_PARAMETER_REQUEST
                | DHCP_OPTION_CLIENT_ID
        );
        if unique && singleton[usize::from(code)] {
            return Err(Phase::Dhcp);
        }
        if unique {
            singleton[usize::from(code)] = true;
        }
        match code {
            DHCP_OPTION_SUBNET_MASK if payload.len() == 4 => {
                options.mask = payload.try_into().ok();
            }
            DHCP_OPTION_ROUTER if !payload.is_empty() && payload.len() % 4 == 0 => {
                options.router = payload.get(..4).and_then(|value| value.try_into().ok());
            }
            DHCP_OPTION_SERVER_ID if payload.len() == 4 => {
                options.server = payload.try_into().ok();
            }
            DHCP_OPTION_LEASE_TIME if payload.len() == 4 => {
                options.lease_seconds = payload.try_into().ok().map(u32::from_be_bytes);
            }
            DHCP_OPTION_MESSAGE_TYPE if payload.len() == 1 => {
                options.message = payload.first().copied();
            }
            DHCP_OPTION_CLIENT_ID if payload.len() == 7 => {
                if payload.first() != Some(&1) || payload.get(1..) != Some(mac) {
                    return Err(Phase::Dhcp);
                }
            }
            DHCP_OPTION_CLIENT_ID
            | DHCP_OPTION_SUBNET_MASK
            | DHCP_OPTION_ROUTER
            | DHCP_OPTION_SERVER_ID
            | DHCP_OPTION_LEASE_TIME
            | DHCP_OPTION_MESSAGE_TYPE => return Err(Phase::Dhcp),
            _ => {}
        }
    }
    if !ended {
        return Err(Phase::Dhcp);
    }
    Ok(options)
}

fn lease_from_reply(reply: ParsedReply, expected_message: u8) -> GuestResult<Lease> {
    if reply.message != expected_message || !valid_endpoint(reply.offered) {
        return Err(Phase::Dhcp);
    }
    let mask = reply.options.mask.ok_or(Phase::Dhcp)?;
    let prefix = prefix_length(mask)?;
    let router = reply.options.router.ok_or(Phase::Dhcp)?;
    let server = reply.options.server.ok_or(Phase::Dhcp)?;
    let seconds = reply.options.lease_seconds.ok_or(Phase::Dhcp)?;
    if seconds == 0 || !valid_endpoint(router) || !valid_endpoint(server) || router == reply.offered
    {
        return Err(Phase::Dhcp);
    }
    let mask_raw = ipv4_u32(mask);
    let address_raw = ipv4_u32(reply.offered);
    let router_raw = ipv4_u32(router);
    let network = address_raw & mask_raw;
    if router_raw & mask_raw != network {
        return Err(Phase::Dhcp);
    }
    if prefix <= 30 {
        let broadcast = network | !mask_raw;
        if address_raw == network
            || address_raw == broadcast
            || router_raw == network
            || router_raw == broadcast
        {
            return Err(Phase::Dhcp);
        }
    }
    Ok(Lease {
        address: reply.offered,
        prefix,
        mask,
        router,
        server,
        seconds,
    })
}

fn prefix_length(mask: [u8; 4]) -> GuestResult<u8> {
    let raw = ipv4_u32(mask);
    let inverted = !raw;
    if raw == 0 || inverted & inverted.wrapping_add(1) != 0 {
        return Err(Phase::Dhcp);
    }
    u8::try_from(raw.count_ones()).map_err(|_| Phase::Dhcp)
}

const fn ipv4_u32(value: [u8; 4]) -> u32 {
    u32::from_be_bytes(value)
}

fn valid_endpoint(value: [u8; 4]) -> bool {
    value != [0, 0, 0, 0]
        && value[0] != 127
        && !(value[0] >= 224 && value[0] <= 239)
        && value != [255, 255, 255, 255]
}

fn ipv4_c_buffer(value: [u8; 4]) -> Option<CBuffer> {
    let mut output = CBuffer::new();
    if output.append_ipv4(value) && output.terminate() {
        Some(output)
    } else {
        None
    }
}

fn address_prefix(value: [u8; 4], prefix: u8) -> Option<CBuffer> {
    let mut output = CBuffer::new();
    if prefix <= 32
        && output.append_ipv4(value)
        && output.append_byte(b'/')
        && output.append_decimal(prefix)
        && output.terminate()
    {
        Some(output)
    } else {
        None
    }
}

fn tcp_exchange(router: [u8; 4], port: u16, nonce: &[u8; 32]) -> GuestResult<()> {
    if !valid_endpoint(router) || port == 0 || nonce.iter().all(|byte| *byte == 0) {
        return Err(Phase::Tcp);
    }
    let descriptor = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC);
    if descriptor < 0 {
        return Err(Phase::Tcp);
    }
    let descriptor = usize::try_from(descriptor).map_err(|_| Phase::Tcp)?;
    let endpoint = socket_address(router, port);
    let result = (|| {
        let timeout = Timeval {
            seconds: TCP_TIMEOUT_NS / 1_000_000_000,
            microseconds: (TCP_TIMEOUT_NS % 1_000_000_000) / 1_000,
        };
        if !set_socket_option(descriptor, SO_RCVTIMEO, &timeout)
            || !set_socket_option(descriptor, SO_SNDTIMEO, &timeout)
        {
            return Err(Phase::Tcp);
        }
        if connect(descriptor, &endpoint) != 0 {
            return Err(Phase::Tcp);
        }
        let mut request = [0_u8; TCP_RECORD_BYTES];
        request[..8].copy_from_slice(TCP_REQUEST_MAGIC);
        request[8..].copy_from_slice(nonce);
        if !write_socket_all(descriptor, &request, TCP_TIMEOUT_NS)
            || shutdown(descriptor, SHUT_WR) != 0
        {
            return Err(Phase::Tcp);
        }
        let mut response = [0_u8; TCP_RECORD_BYTES];
        if !read_socket_exact(descriptor, &mut response, TCP_TIMEOUT_NS) {
            return Err(Phase::Tcp);
        }
        let mut trailing = [0_u8; 1];
        let trailing_count = read_socket(descriptor, &mut trailing, TCP_TIMEOUT_NS);
        if !tcp_response_is_exact(&response, trailing_count, nonce) {
            return Err(Phase::Tcp);
        }
        Ok(())
    })();
    let close_result = close(descriptor);
    if close_result != 0 {
        return Err(Phase::Tcp);
    }
    result
}

fn tcp_response_is_exact(response: &[u8], trailing_count: isize, nonce: &[u8; 32]) -> bool {
    trailing_count == 0
        && response.len() == TCP_RECORD_BYTES
        && response.get(..8) == Some(TCP_RESPONSE_MAGIC)
        && response.get(8..) == Some(nonce)
}

fn run_ip(arguments: &[*const u8]) -> GuestResult<()> {
    if arguments.len() + 2 > 16 {
        return Err(Phase::Configure);
    }
    let mut argv = [ptr::null::<u8>(); 16];
    argv[0] = IP_PATH.as_ptr();
    for (index, argument) in arguments.iter().copied().enumerate() {
        argv[index + 1] = argument;
    }
    let pid = clone_process();
    if pid < 0 {
        return Err(Phase::Configure);
    }
    if pid == 0 {
        child_exec(&argv)
    }
    let deadline = monotonic_ns().ok_or(Phase::Configure)? + COMMAND_TIMEOUT_NS;
    loop {
        let mut status = 0_i32;
        let waited = wait4(pid, &mut status, WNOHANG);
        if waited == pid {
            return if status & 0x7f == 0 && (status >> 8) & 0xff == 0 {
                Ok(())
            } else {
                Err(Phase::Configure)
            };
        }
        if waited < 0 && waited != -EINTR {
            return Err(Phase::Configure);
        }
        if monotonic_ns().is_none_or(|now| now >= deadline) {
            let _ = kill(pid, SIGKILL);
            let mut status = 0_i32;
            let _ = wait4(pid, &mut status, 0);
            return Err(Phase::Configure);
        }
        sleep_ns(10_000_000);
    }
}

fn child_exec(argv: &[*const u8; 16]) -> ! {
    let null = open(DEV_NULL, O_RDWR).unwrap_or(usize::MAX);
    if null == usize::MAX
        || (null != 0 && dup3(null, 0) != 0)
        || (null != 1 && dup3(null, 1) != 1)
        || (null != 2 && dup3(null, 2) != 2)
    {
        exit(126);
    }
    if null > 2 {
        let _ = close(null);
    }
    let environment = [
        b"LANG=C\0".as_ptr(),
        b"LC_ALL=C\0".as_ptr(),
        b"PATH=/usr/sbin:/usr/bin:/sbin:/bin\0".as_ptr(),
        ptr::null(),
    ];
    execve(IP_PATH, argv.as_ptr(), environment.as_ptr());
    exit(127)
}

fn sha256(input: &[u8]) -> Option<[u8; 32]> {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    if input.len() > 119 {
        return None;
    }
    let block_count = if input.len() + 9 <= 64 { 1 } else { 2 };
    let mut padded = [0_u8; 128];
    padded[..input.len()].copy_from_slice(input);
    padded[input.len()] = 0x80;
    let bit_length = (input.len() as u64).wrapping_mul(8).to_be_bytes();
    let padded_length = block_count * 64;
    padded[padded_length - 8..padded_length].copy_from_slice(&bit_length);
    let mut state = INITIAL;
    for block in padded[..padded_length].chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(chunk.try_into().ok()?);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let first = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let second = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut output = [0_u8; 32];
    for (chunk, value) in output.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    Some(output)
}

fn socket_address(address: [u8; 4], port: u16) -> SockAddrIn {
    SockAddrIn {
        family: AF_INET as u16,
        port: port.to_be(),
        address: u32::from_ne_bytes(address),
        zero: [0; 8],
    }
}

fn wait_ready(descriptor: usize, events: i16, timeout_ns: i64) -> bool {
    if timeout_ns <= 0 {
        return false;
    }
    let mut poll = PollFd {
        descriptor: descriptor as i32,
        events,
        returned: 0,
    };
    let timeout = Timespec {
        seconds: timeout_ns / 1_000_000_000,
        nanoseconds: timeout_ns % 1_000_000_000,
    };
    let result = ppoll(&mut poll, &timeout);
    let accepted = if events == POLLIN {
        POLLIN | POLLHUP
    } else {
        events
    };
    result == 1 && poll.returned & accepted != 0
}

fn write_socket_all(descriptor: usize, bytes: &[u8], timeout_ns: i64) -> bool {
    let deadline = match monotonic_ns() {
        Some(now) => now + timeout_ns,
        None => return false,
    };
    let mut written = 0_usize;
    while written < bytes.len() {
        let remaining = match monotonic_ns().and_then(|now| remaining_timeout(deadline, now)) {
            Some(remaining) => remaining,
            _ => return false,
        };
        if !wait_ready(descriptor, POLLOUT, remaining) {
            return false;
        }
        let count = write(descriptor, &bytes[written..]);
        if count == -EINTR || count == -EAGAIN {
            continue;
        }
        if count <= 0 {
            return false;
        }
        written += count as usize;
    }
    true
}

fn read_socket_exact(descriptor: usize, bytes: &mut [u8], timeout_ns: i64) -> bool {
    let deadline = match monotonic_ns() {
        Some(now) => now + timeout_ns,
        None => return false,
    };
    let mut read_bytes = 0_usize;
    while read_bytes < bytes.len() {
        let remaining = match monotonic_ns().and_then(|now| remaining_timeout(deadline, now)) {
            Some(remaining) => remaining,
            _ => return false,
        };
        if !wait_ready(descriptor, POLLIN, remaining) {
            return false;
        }
        let count = read(descriptor, &mut bytes[read_bytes..]);
        if count == -EINTR || count == -EAGAIN {
            continue;
        }
        if count <= 0 {
            return false;
        }
        read_bytes += count as usize;
    }
    true
}

fn read_socket(descriptor: usize, bytes: &mut [u8], timeout_ns: i64) -> isize {
    if !wait_ready(descriptor, POLLIN, timeout_ns) {
        return -1;
    }
    read(descriptor, bytes)
}

fn monotonic_ns() -> Option<i64> {
    let mut value = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    if clock_gettime(&mut value) != 0
        || value.seconds < 0
        || !(0..1_000_000_000).contains(&value.nanoseconds)
    {
        return None;
    }
    value
        .seconds
        .checked_mul(1_000_000_000)?
        .checked_add(value.nanoseconds)
}

fn remaining_timeout(deadline: i64, now: i64) -> Option<i64> {
    deadline.checked_sub(now).filter(|remaining| *remaining > 0)
}

fn sleep_ns(value: i64) {
    let request = Timespec {
        seconds: value / 1_000_000_000,
        nanoseconds: value % 1_000_000_000,
    };
    let _ = nanosleep(&request);
}

fn open(path: &[u8], flags: usize) -> Option<usize> {
    if path.last() != Some(&0) {
        return None;
    }
    let result = syscall6(SYS_OPENAT, AT_FDCWD, path.as_ptr() as usize, flags, 0, 0, 0);
    usize::try_from(result).ok()
}

fn close(descriptor: usize) -> isize {
    syscall6(SYS_CLOSE, descriptor, 0, 0, 0, 0, 0)
}

fn read_exact(descriptor: usize, bytes: &mut [u8]) -> bool {
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let count = read(descriptor, &mut bytes[offset..]);
        if count == -EINTR {
            continue;
        }
        if count <= 0 {
            return false;
        }
        offset += count as usize;
    }
    true
}

fn read_some(descriptor: usize, bytes: &mut [u8]) -> isize {
    loop {
        let result = read(descriptor, bytes);
        if result != -EINTR {
            return result;
        }
    }
}

fn read(descriptor: usize, bytes: &mut [u8]) -> isize {
    syscall6(
        SYS_READ,
        descriptor,
        bytes.as_mut_ptr() as usize,
        bytes.len(),
        0,
        0,
        0,
    )
}

fn write(descriptor: usize, bytes: &[u8]) -> isize {
    syscall6(
        SYS_WRITE,
        descriptor,
        bytes.as_ptr() as usize,
        bytes.len(),
        0,
        0,
        0,
    )
}

fn write_all(descriptor: usize, bytes: &[u8]) -> bool {
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let end = bytes.len().min(offset + SERIAL_WRITE_BYTES);
        let count = write(descriptor, &bytes[offset..end]);
        if count == -EINTR {
            continue;
        }
        if count <= 0 {
            return false;
        }
        offset += count as usize;
    }
    true
}

fn getdents(descriptor: usize, bytes: &mut [u8]) -> isize {
    syscall6(
        SYS_GETDENTS64,
        descriptor,
        bytes.as_mut_ptr() as usize,
        bytes.len(),
        0,
        0,
        0,
    )
}

fn socket(domain: usize, kind: usize) -> isize {
    syscall6(SYS_SOCKET, domain, kind, 0, 0, 0, 0)
}

fn bind(descriptor: usize, address: &SockAddrIn) -> isize {
    syscall6(
        SYS_BIND,
        descriptor,
        address as *const SockAddrIn as usize,
        core::mem::size_of::<SockAddrIn>(),
        0,
        0,
        0,
    )
}

fn connect(descriptor: usize, address: &SockAddrIn) -> isize {
    syscall6(
        SYS_CONNECT,
        descriptor,
        address as *const SockAddrIn as usize,
        core::mem::size_of::<SockAddrIn>(),
        0,
        0,
        0,
    )
}

fn send_to(descriptor: usize, bytes: &[u8], address: &SockAddrIn) -> isize {
    syscall6(
        SYS_SENDTO,
        descriptor,
        bytes.as_ptr() as usize,
        bytes.len(),
        0,
        address as *const SockAddrIn as usize,
        core::mem::size_of::<SockAddrIn>(),
    )
}

fn recv_from(
    descriptor: usize,
    bytes: &mut [u8],
    address: &mut SockAddrIn,
    address_length: &mut u32,
) -> isize {
    syscall6(
        SYS_RECVFROM,
        descriptor,
        bytes.as_mut_ptr() as usize,
        bytes.len(),
        0,
        address as *mut SockAddrIn as usize,
        address_length as *mut u32 as usize,
    )
}

fn set_socket_option<T>(descriptor: usize, option: usize, value: &T) -> bool {
    syscall6(
        SYS_SETSOCKOPT,
        descriptor,
        SOL_SOCKET,
        option,
        value as *const T as usize,
        core::mem::size_of::<T>(),
        0,
    ) == 0
}

fn set_socket_bytes(descriptor: usize, option: usize, value: &[u8]) -> bool {
    syscall6(
        SYS_SETSOCKOPT,
        descriptor,
        SOL_SOCKET,
        option,
        value.as_ptr() as usize,
        value.len(),
        0,
    ) == 0
}

fn shutdown(descriptor: usize, how: usize) -> isize {
    syscall6(SYS_SHUTDOWN, descriptor, how, 0, 0, 0, 0)
}

fn ppoll(descriptor: &mut PollFd, timeout: &Timespec) -> isize {
    syscall6(
        SYS_PPOLL,
        descriptor as *mut PollFd as usize,
        1,
        timeout as *const Timespec as usize,
        0,
        0,
        0,
    )
}

fn nanosleep(timeout: &Timespec) -> isize {
    syscall6(
        SYS_NANOSLEEP,
        timeout as *const Timespec as usize,
        0,
        0,
        0,
        0,
        0,
    )
}

fn clock_gettime(value: &mut Timespec) -> isize {
    syscall6(
        SYS_CLOCK_GETTIME,
        CLOCK_MONOTONIC,
        value as *mut Timespec as usize,
        0,
        0,
        0,
        0,
    )
}

fn clone_process() -> isize {
    syscall6(SYS_CLONE, SIGCHLD, 0, 0, 0, 0, 0)
}

fn dup3(source: usize, destination: usize) -> isize {
    syscall6(SYS_DUP3, source, destination, 0, 0, 0, 0)
}

fn execve(path: &[u8], arguments: *const *const u8, environment: *const *const u8) -> isize {
    syscall6(
        SYS_EXECVE,
        path.as_ptr() as usize,
        arguments as usize,
        environment as usize,
        0,
        0,
        0,
    )
}

fn wait4(pid: isize, status: &mut i32, options: usize) -> isize {
    syscall6(
        SYS_WAIT4,
        pid as usize,
        status as *mut i32 as usize,
        options,
        0,
        0,
        0,
    )
}

fn kill(pid: isize, signal: usize) -> isize {
    syscall6(SYS_KILL, pid as usize, signal, 0, 0, 0, 0)
}

#[cfg(target_arch = "aarch64")]
fn exit(status: usize) -> ! {
    // SAFETY: the Linux aarch64 exit syscall takes one scalar status, does not
    // return, and uses only the declared registers without touching the stack.
    unsafe {
        asm!(
            "svc 0",
            in("x8") SYS_EXIT,
            in("x0") status,
            options(noreturn, nostack),
        );
    }
}

#[cfg(all(test, not(target_arch = "aarch64")))]
fn exit(_status: usize) -> ! {
    panic!("guest exit is not executed by portable unit tests")
}

#[cfg(target_arch = "aarch64")]
fn syscall6(
    number: usize,
    argument0: usize,
    argument1: usize,
    argument2: usize,
    argument3: usize,
    argument4: usize,
    argument5: usize,
) -> isize {
    let result;
    // SAFETY: Linux validates every userspace address passed through these
    // opaque scalar registers. Each typed wrapper above supplies a live slice,
    // structure, or C-vector for the full synchronous syscall duration, and
    // the assembly declares every clobbered register and preserves the stack.
    unsafe {
        asm!(
            "svc 0",
            in("x8") number,
            inlateout("x0") argument0 => result,
            in("x1") argument1,
            in("x2") argument2,
            in("x3") argument3,
            in("x4") argument4,
            in("x5") argument5,
            lateout("x6") _,
            lateout("x7") _,
            options(nostack),
        );
    }
    result
}

#[cfg(all(test, not(target_arch = "aarch64")))]
fn syscall6(
    _number: usize,
    _argument0: usize,
    _argument1: usize,
    _argument2: usize,
    _argument3: usize,
    _argument4: usize,
    _argument5: usize,
) -> isize {
    -38
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn memcpy(destination: *mut u8, source: *const u8, length: usize) -> *mut u8 {
    for index in 0..length {
        // SAFETY: the compiler calls this symbol with non-overlapping readable
        // and writable ranges of at least `length` bytes.
        unsafe {
            destination
                .add(index)
                .write_volatile(source.add(index).read_volatile());
        }
    }
    destination
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn memset(destination: *mut u8, value: i32, length: usize) -> *mut u8 {
    for index in 0..length {
        // SAFETY: the compiler calls this symbol with a writable range of at
        // least `length` bytes.
        unsafe {
            destination.add(index).write_volatile(value as u8);
        }
    }
    destination
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn bcmp(left: *const u8, right: *const u8, length: usize) -> i32 {
    for index in 0..length {
        // SAFETY: the compiler calls this symbol with two readable ranges of
        // at least `length` bytes.
        let (left_byte, right_byte) = unsafe {
            (
                left.add(index).read_volatile(),
                right.add(index).read_volatile(),
            )
        };
        if left_byte != right_byte {
            return 1;
        }
    }
    0
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_entry_graph_is_linked_without_executing_linux_syscalls() {
        let _run: fn() -> GuestResult<()> = run;
        let _write: fn(usize, &[u8]) -> bool = write_all;
        let _exit: fn(usize) -> ! = exit;
        assert_eq!(STDOUT, 1);
        assert_eq!(BEGIN_MARKER.last(), Some(&b'\n'));
        assert_eq!(SUCCESS_MARKER.last(), Some(&b'\n'));
        assert_eq!(FAILURE_PREFIX.last(), Some(&b'_'));
        assert_eq!(Phase::Internal.marker(), b"INTERNAL\n");
    }

    fn control() -> [u8; CONTROL_BYTES] {
        let mut data = [0_u8; CONTROL_BYTES];
        data[..8].copy_from_slice(CONTROL_MAGIC);
        data[8..10].copy_from_slice(&CONTROL_VERSION.to_be_bytes());
        data[10] = CONTROL_SHARED_MODE;
        data[11] = CONTROL_DHCP_ROUTER_ENDPOINT;
        data[16..18].copy_from_slice(&8080_u16.to_be_bytes());
        data[18..50].copy_from_slice(&[0x5a; 32]);
        let digest = sha256(&data[..CONTROL_PREFIX_BYTES]).expect("digest should build");
        data[64..96].copy_from_slice(&digest);
        data
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256(b""),
            Some([
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ])
        );
        assert_eq!(
            sha256(b"abc"),
            Some([
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ])
        );
    }

    #[test]
    fn control_decoder_is_closed() {
        let valid = control();
        assert_eq!(
            decode_control(&valid),
            Ok(Control {
                port: 8080,
                nonce: [0x5a; 32],
            })
        );
        for index in [0, 8, 10, 11, 12, 16, 18, 50, 64, 96] {
            let mut changed = valid;
            changed[index] ^= 1;
            assert_eq!(decode_control(&changed), Err(Phase::Control));
        }

        for index in [10, 11] {
            let mut changed = valid;
            changed[index] ^= 1;
            let digest = sha256(&changed[..CONTROL_PREFIX_BYTES]).expect("digest should build");
            changed[CONTROL_PREFIX_BYTES..CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES]
                .copy_from_slice(&digest);
            assert_eq!(decode_control(&changed), Err(Phase::Control));
        }
    }

    #[test]
    fn packet_and_lease_validation_are_exact() {
        let mac = [0x02, 0, 0, 0, 0, 1];
        let discover =
            encode_dhcp(0x1234_5678, &mac, DHCP_DISCOVER, None).expect("discover should build");
        assert_eq!(discover.length, DHCP_MIN_PACKET_BYTES);
        assert_eq!(&discover.bytes[..4], &[1, 1, 6, 0]);
        assert_eq!(&discover.bytes[28..34], &mac);
        assert_eq!(prefix_length([255, 255, 255, 0]), Ok(24));
        assert_eq!(prefix_length([255, 0, 255, 0]), Err(Phase::Dhcp));
        assert!(!valid_endpoint([0, 0, 0, 0]));
        assert!(!valid_endpoint([127, 0, 0, 1]));
        assert!(!valid_endpoint([255, 255, 255, 255]));
        assert!(valid_endpoint([192, 168, 64, 1]));
        assert_eq!(transaction_id(&[0x5a; 32]), 0xf4f3_e8ba);
    }

    fn reply_with_router(router: Option<[u8; 4]>) -> ParsedReply {
        ParsedReply {
            message: DHCP_OFFER,
            offered: [192, 168, 64, 2],
            options: ParsedOptions {
                mask: Some([255, 255, 255, 0]),
                router,
                server: Some([192, 168, 64, 1]),
                lease_seconds: Some(3600),
                message: Some(DHCP_OFFER),
            },
        }
    }

    #[test]
    fn lease_rejects_missing_invalid_and_unrelated_routers() {
        assert_eq!(
            lease_from_reply(reply_with_router(Some([192, 168, 64, 1])), DHCP_OFFER),
            Ok(Lease {
                address: [192, 168, 64, 2],
                prefix: 24,
                mask: [255, 255, 255, 0],
                router: [192, 168, 64, 1],
                server: [192, 168, 64, 1],
                seconds: 3600,
            })
        );
        assert_eq!(
            lease_from_reply(reply_with_router(None), DHCP_OFFER),
            Err(Phase::Dhcp)
        );
        assert_eq!(
            lease_from_reply(reply_with_router(Some([224, 0, 0, 1])), DHCP_OFFER),
            Err(Phase::Dhcp)
        );
        assert_eq!(
            lease_from_reply(reply_with_router(Some([10, 0, 0, 1])), DHCP_OFFER),
            Err(Phase::Dhcp)
        );
    }

    #[test]
    fn tcp_response_timeout_and_short_io_boundaries_are_closed() {
        let nonce = [0x5a; 32];
        let mut response = [0_u8; TCP_RECORD_BYTES];
        response[..8].copy_from_slice(TCP_RESPONSE_MAGIC);
        response[8..].copy_from_slice(&nonce);
        assert!(tcp_response_is_exact(&response, 0, &nonce));
        assert!(!tcp_response_is_exact(&response[..39], 0, &nonce));
        assert!(!tcp_response_is_exact(&response, 1, &nonce));
        let mut wrong_nonce = nonce;
        wrong_nonce[0] ^= 1;
        assert!(!tcp_response_is_exact(&response, 0, &wrong_nonce));

        assert_eq!(remaining_timeout(10, 9), Some(1));
        assert_eq!(remaining_timeout(10, 10), None);
        assert_eq!(remaining_timeout(10, 11), None);
        assert_eq!(remaining_timeout(i64::MIN, i64::MAX), None);
    }

    #[test]
    fn serial_diagnostics_are_fixed_and_value_free() {
        for marker in [
            Phase::Control.marker(),
            Phase::Interface.marker(),
            Phase::Dhcp.marker(),
            Phase::Configure.marker(),
            Phase::Tcp.marker(),
            Phase::Cleanup.marker(),
            Phase::Internal.marker(),
        ] {
            assert!(marker.ends_with(b"\n"));
            assert!(
                marker
                    .iter()
                    .all(|byte| byte.is_ascii_uppercase() || matches!(*byte, b'_' | b'\n'))
            );
        }
    }

    #[test]
    fn c_buffers_are_canonical() {
        let address = address_prefix([192, 168, 64, 2], 24).expect("address should format");
        assert_eq!(&address.bytes[..16], b"192.168.64.2/24\0");
        let router = ipv4_c_buffer([192, 168, 64, 1]).expect("router should format");
        assert_eq!(&router.bytes[..13], b"192.168.64.1\0");
    }

    #[test]
    fn mac_parser_rejects_multicast_and_malformed_values() {
        assert_eq!(parse_mac(b"02:00:00:00:00:01\n"), Ok([0x02, 0, 0, 0, 0, 1]));
        assert_eq!(parse_mac(b"03:00:00:00:00:01\n"), Err(Phase::Interface));
        assert_eq!(parse_mac(b"bad"), Err(Phase::Interface));
        let name = InterfaceName::parse(b"enp0s1").expect("interface name should parse");
        assert_eq!(name.as_bytes(), b"enp0s1");
        assert_eq!(name.as_c_bytes(), b"enp0s1\0");
        assert!(InterfaceName::parse(b"name/with/slash").is_none());
        assert!(InterfaceName::parse(b"0123456789abcdef").is_none());
    }
}
